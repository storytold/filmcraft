//! Colour-managed conversions between sources, the working space and outputs.
//!
//! **Working units.** The compositor works in linear light where 1.0 is *reference white*: SDR
//! white for SDR media, and 203 cd/m² for HDR (ITU-R BT.2408 "HDR Reference White"). SDR media
//! therefore keeps its values when it is placed in an HDR sequence, and HDR highlights are
//! simply values above 1.0. Log media decodes to scene-linear reflectance (18 % grey = 0.18).
//!
//! **Input** ([`InputTransform`]): signal → per-channel decode table (curve) → HLG OOTF (BT.2100,
//! 1000 cd/m² nominal peak, γ = 1.2) → 3×3 gamut matrix into the working primaries → tone mapping
//! (only when an HDR or log source enters an SDR working space and Auto Tone Map is on) → gamut
//! compression (only when the source gamut is wider than the working gamut).
//!
//! **Output** ([`OutputTransform`]): working linear → display-referred SDR BT.709 for monitors
//! (tone mapping from HDR working spaces, gamut mapping from wide gamut), or → encoded PQ/HLG
//! signal for HDR exports. Out-of-gamut colours there are desaturated towards their luminance
//! just enough to fit ([`desaturate_into_gamut`]), so in-gamut colours (e.g. BT.709 content in a
//! BT.2020 working space) come back exactly.
//!
//! **Tone mapping** is the ITU-R BT.2390 EETF (Hermite knee in the PQ domain) applied to
//! max(R,G,B) and used as a ratio on RGB, so hue and saturation are preserved. **Gamut
//! compression** pulls out-of-gamut colours (negative components after the matrix) towards the
//! achromatic axis with a per-channel distance compression after the ACES Reference Gamut
//! Compression (threshold 0.8, limit 1.25, power 1.2); colours inside 80 % of the gamut boundary
//! are unchanged.

use crate::log::{Domain, normalized_from_signal, signal_from_normalized};
use crate::spaces::{ColorPipeline, ColorSpace, Curve, Gamut, gamut_matrix, to_f32};
use crate::{Range, hlg_inverse_oetf, hlg_oetf, linear_to_srgb, pq_eotf, pq_inverse_eotf, srgb_to_linear};

/// BT.2408 HDR reference white.
pub const REFERENCE_WHITE_NITS: f64 = 203.0;
/// Nominal peak of an HLG display (BT.2100 Note 5c) and the default mastering peak of PQ media
/// without metadata.
pub const HDR_PEAK_NITS: f64 = 1000.0;
const HLG_GAMMA: f64 = 1.2;
/// Source peak assumed when tone mapping log media: the curve's own peak, capped here so the
/// BT.2390 knee starts above mid grey (≈ 45 cd/m², i.e. 0.22 in working units).
pub const LOG_PEAK_NITS: f64 = 4000.0;

/// Decode one channel: decoder-normalised signal → working-linear (no OOTF, no matrix).
pub fn decode_channel(v: f64, curve: Curve, range: Range) -> f64 {
    match curve {
        Curve::Sdr | Curve::Srgb => {
            if v < 0.0 {
                -(srgb_to_linear((-v) as f32) as f64)
            } else {
                srgb_to_linear(v as f32) as f64
            }
        }
        Curve::Linear => v,
        Curve::Pq => pq_eotf(v as f32) as f64 * 10_000.0 / REFERENCE_WHITE_NITS,
        // scene light 0..1; the OOTF (which needs all three channels) runs in the pixel stage
        Curve::Hlg => hlg_inverse_oetf(v.clamp(0.0, 1.0) as f32) as f64,
        Curve::Log(c) => c.decode(signal_from_normalized(v, range, c.domain())),
    }
}

/// Encode one channel: working-linear → decoder-normalised signal (inverse of [`decode_channel`]).
pub fn encode_channel(l: f64, curve: Curve, range: Range) -> f64 {
    match curve {
        Curve::Sdr | Curve::Srgb => {
            if l < 0.0 {
                -(linear_to_srgb((-l) as f32) as f64)
            } else {
                linear_to_srgb(l as f32) as f64
            }
        }
        Curve::Linear => l,
        Curve::Pq => pq_inverse_eotf((l.max(0.0) * REFERENCE_WHITE_NITS / 10_000.0) as f32) as f64,
        Curve::Hlg => hlg_oetf(l.clamp(0.0, 1.0) as f32) as f64,
        Curve::Log(c) => normalized_from_signal(c.encode(l), range, c.domain()),
    }
}

/// A sampled signal → linear lookup with linear interpolation over `[lo, hi]` (values outside
/// clamp), so log footage keeps its super-whites.
#[derive(Clone, Debug)]
pub struct DecodeTable {
    pub lo: f32,
    pub hi: f32,
    pub values: Vec<f32>,
}

impl DecodeTable {
    pub fn new(lo: f32, hi: f32, n: usize, f: impl Fn(f64) -> f64) -> Self {
        let values = (0..n).map(|i| f(lo as f64 + (hi - lo) as f64 * i as f64 / (n - 1) as f64) as f32).collect();
        DecodeTable { lo, hi, values }
    }
    /// The decode table for a colour space's curve.
    pub fn for_curve(curve: Curve, range: Range) -> Self {
        match curve {
            Curve::Log(_) => DecodeTable::new(-0.1, 1.1, 8192, |v| decode_channel(v, curve, range)),
            _ => DecodeTable::new(0.0, 1.0, 4096, |v| decode_channel(v, curve, range)),
        }
    }
    #[inline]
    pub fn lookup(&self, v: f32) -> f32 {
        let n = self.values.len() - 1;
        let p = ((v - self.lo) / (self.hi - self.lo)).clamp(0.0, 1.0) * n as f32;
        let i = (p as usize).min(n - 1);
        let f = p - i as f32;
        self.values[i] + (self.values[i + 1] - self.values[i]) * f
    }
}

/// ITU-R BT.2390 EETF: maps `nits` from a display with peak `src_peak` to one with peak
/// `dst_peak` (black level 0). Identity when `dst_peak ≥ src_peak`.
pub fn bt2390_eetf(nits: f64, src_peak: f64, dst_peak: f64) -> f64 {
    if dst_peak >= src_peak || nits <= 0.0 {
        return nits.max(0.0).min(src_peak.max(dst_peak));
    }
    let pq = |n: f64| pq_inverse_eotf((n / 10_000.0).clamp(0.0, 1.0) as f32) as f64;
    let smax = pq(src_peak);
    let e1 = (pq(nits) / smax).min(1.0);
    let max_lum = pq(dst_peak) / smax;
    let ks = 1.5 * max_lum - 0.5;
    let e2 = if e1 < ks {
        e1
    } else {
        let t = (e1 - ks) / (1.0 - ks);
        let (t2, t3) = (t * t, t * t * t);
        (2.0 * t3 - 3.0 * t2 + 1.0) * ks + (t3 - 2.0 * t2 + t) * (1.0 - ks) + (-2.0 * t3 + 3.0 * t2) * max_lum
    };
    pq_eotf((e2 * smax) as f32) as f64 * 10_000.0
}

/// Tone-mapping operator in working units: BT.2390 on max(R,G,B), applied as a ratio.
#[derive(Clone, Debug)]
pub struct ToneMap {
    pub src_peak_nits: f64,
    pub dst_peak_nits: f64,
    /// max(R,G,B) (working units) → scale factor, sampled in the PQ domain.
    table: DecodeTable,
}

impl ToneMap {
    pub fn new(src_peak_nits: f64, dst_peak_nits: f64) -> Self {
        let (s, d) = (src_peak_nits, dst_peak_nits);
        // sample on a PQ-encoded axis so the shadows get enough resolution
        let table = DecodeTable::new(0.0, 1.0, 4096, |e| {
            let nits = pq_eotf(e as f32) as f64 * 10_000.0;
            if nits <= 1e-6 { 1.0 } else { bt2390_eetf(nits, s, d) / nits }
        });
        ToneMap { src_peak_nits, dst_peak_nits, table }
    }
    #[inline]
    pub fn apply(&self, c: [f32; 3]) -> [f32; 3] {
        let m = c[0].max(c[1]).max(c[2]);
        if m <= 0.0 {
            return c;
        }
        let nits = m * REFERENCE_WHITE_NITS as f32;
        let k = if nits >= self.src_peak_nits as f32 {
            // above the source peak: clip to the target peak
            (self.dst_peak_nits / REFERENCE_WHITE_NITS) as f32 / m
        } else {
            self.table.lookup(pq_inverse_eotf(nits / 10_000.0))
        };
        [c[0] * k, c[1] * k, c[2] * k]
    }
}

/// Compress out-of-gamut colours towards the achromatic axis (see module docs). Used on input,
/// where camera gamuts extend well beyond the working gamut.
#[inline]
pub fn gamut_compress(c: [f32; 3]) -> [f32; 3] {
    const TH: f32 = 0.8;
    const LIM: f32 = 1.25;
    const P: f32 = 1.2;
    let ach = c[0].max(c[1]).max(c[2]);
    if ach <= 1e-9 {
        return c.map(|v| v.max(0.0));
    }
    // scale so that LIM maps to exactly 1.0
    let s = (LIM - TH) / (((LIM - TH) / (1.0 - TH)).powf(P) - 1.0).powf(1.0 / P);
    c.map(|v| {
        let d = (ach - v) / ach;
        if d < TH {
            return v;
        }
        let x = (d - TH) / s;
        let dc = TH + s * x / (1.0 + x.powf(P)).powf(1.0 / P);
        ach - dc * ach
    })
}

/// Source → working-space conversion for one colour space.
#[derive(Clone, Debug)]
pub struct InputTransform {
    pub source: ColorSpace,
    pub table: DecodeTable,
    matrix: [[f32; 3]; 3],
    identity_matrix: bool,
    /// HLG OOTF luma weights (BT.2020) and the scale to working units.
    hlg: Option<([f32; 3], f32)>,
    tone: Option<ToneMap>,
    compress: bool,
    /// Display-referred wide-gamut media (BT.2020 / P3 / HDR): desaturate into the working gamut
    /// with these luma weights instead of the camera-gamut compression.
    desaturate: Option<[f32; 3]>,
}

impl InputTransform {
    /// `src_peak_nits` is the mastering/content peak of PQ media when the file signals it.
    pub fn new(src: ColorSpace, range: Range, pipe: &ColorPipeline, src_peak_nits: Option<f64>) -> Self {
        let curve = src.curve();
        let wg = pipe.working_gamut();
        let m = gamut_matrix(src.gamut(), wg);
        let identity_matrix = src.gamut() == wg;
        let hlg = (curve == Curve::Hlg).then(|| {
            let l = Gamut::Bt2020.luma();
            ([l[0] as f32, l[1] as f32, l[2] as f32], (HDR_PEAK_NITS / REFERENCE_WHITE_NITS) as f32)
        });
        let peak = match curve {
            Curve::Pq => src_peak_nits.unwrap_or(HDR_PEAK_NITS).clamp(REFERENCE_WHITE_NITS, 10_000.0),
            Curve::Hlg => HDR_PEAK_NITS,
            Curve::Log(c) => (c.peak_linear() * REFERENCE_WHITE_NITS).clamp(REFERENCE_WHITE_NITS, LOG_PEAK_NITS),
            _ => REFERENCE_WHITE_NITS,
        };
        let tone = (src.is_hdr() && !pipe.working.is_hdr() && pipe.auto_tone_map).then(|| ToneMap::new(peak, REFERENCE_WHITE_NITS));
        let wider = !identity_matrix && !gamut_within(src.gamut(), wg);
        // camera gamuts (log media) get the soft compression; display gamuts keep their in-gamut
        // colours exactly (BT.709 content in a BT.2020 file comes back unchanged)
        let compress = wider && src.is_log();
        let l = wg.luma();
        let desaturate = (wider && !src.is_log()).then(|| [l[0] as f32, l[1] as f32, l[2] as f32]);
        InputTransform { source: src, table: DecodeTable::for_curve(curve, range), matrix: to_f32(&m), identity_matrix, hlg, tone, compress, desaturate }
    }

    /// Whether anything happens after the per-channel table.
    pub fn has_pixel_stage(&self) -> bool {
        !self.identity_matrix || self.hlg.is_some() || self.tone.is_some() || self.compress || self.desaturate.is_some()
    }

    /// The per-pixel stage (input: the table's output).
    #[inline]
    pub fn apply(&self, mut c: [f32; 3]) -> [f32; 3] {
        if let Some((l, k)) = self.hlg {
            let ys = (l[0] * c[0] + l[1] * c[1] + l[2] * c[2]).max(0.0);
            let g = k * ys.powf((HLG_GAMMA - 1.0) as f32);
            c = c.map(|v| v * g);
        }
        if !self.identity_matrix {
            c = crate::spaces::apply3(&self.matrix, c);
        }
        if let Some(t) = &self.tone {
            c = t.apply(c);
        }
        if self.compress {
            c = gamut_compress(c);
        }
        if let Some(l) = self.desaturate {
            c = desaturate_into_gamut(c, l);
        }
        c
    }

    /// Signal → working linear for one RGB triple (table + pixel stage).
    pub fn convert(&self, rgb: [f32; 3]) -> [f32; 3] {
        self.apply(rgb.map(|v| self.table.lookup(v)))
    }

    /// The transform as plain numbers for a shader, which evaluates the curve and the BT.2390
    /// EETF directly instead of sampling the tables. `None` for what a shader does not do: log
    /// curves and the camera-gamut compression.
    pub fn gpu_params(&self) -> Option<GpuInputParams> {
        let curve = match self.source.curve() {
            Curve::Sdr | Curve::Srgb => GpuCurve::Sdr,
            Curve::Linear => GpuCurve::Linear,
            Curve::Pq => GpuCurve::Pq,
            Curve::Hlg => GpuCurve::Hlg,
            Curve::Log(_) => return None,
        };
        if self.compress {
            return None;
        }
        Some(GpuInputParams {
            curve,
            hlg: self.hlg,
            matrix: (!self.identity_matrix).then_some(self.matrix),
            tone: self.tone.as_ref().map(|t| (t.src_peak_nits as f32, t.dst_peak_nits as f32)),
            desaturate: self.desaturate,
        })
    }
}

/// How a shader decodes a channel ([`decode_channel`] without the table): SDR with the sRGB curve,
/// linear, PQ in working units (cd/m² over [`REFERENCE_WHITE_NITS`]), HLG to scene light.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuCurve {
    Sdr,
    Linear,
    Pq,
    Hlg,
}

/// [`InputTransform`] for a shader ([`InputTransform::gpu_params`]), stage by stage as
/// [`InputTransform::apply`] runs them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GpuInputParams {
    pub curve: GpuCurve,
    /// HLG OOTF: BT.2020 luma weights and the scale to working units.
    pub hlg: Option<([f32; 3], f32)>,
    /// Into the working primaries (row-major), when they differ.
    pub matrix: Option<[[f32; 3]; 3]>,
    /// BT.2390 tone mapping (source peak, target peak) in cd/m², on max(R,G,B) as a ratio.
    pub tone: Option<(f32, f32)>,
    /// Luma weights to desaturate into the working gamut with ([`desaturate_into_gamut`]).
    pub desaturate: Option<[f32; 3]>,
}

/// Whether gamut `a` lies inside gamut `b` (so no compression is needed going a → b).
fn gamut_within(a: Gamut, b: Gamut) -> bool {
    a == b || matches!((a, b), (Gamut::Bt709, _) | (Gamut::P3D65, Gamut::Bt2020))
}

/// Bring a colour with negative components into gamut by desaturating towards its luminance
/// (`luma` = the target gamut's Y weights) just enough that the smallest component is 0. In-gamut
/// colours are unchanged; hue and luminance are preserved.
#[inline]
pub fn desaturate_into_gamut(c: [f32; 3], luma: [f32; 3]) -> [f32; 3] {
    let mn = c[0].min(c[1]).min(c[2]);
    if mn >= 0.0 {
        return c;
    }
    let y = luma[0] * c[0] + luma[1] * c[1] + luma[2] * c[2];
    if y <= 0.0 {
        return [0.0; 3];
    }
    let t = y / (y - mn);
    c.map(|v| (y + (v - y) * t).max(0.0))
}

/// Working space → monitor (display-referred SDR BT.709) or → encoded export signal.
#[derive(Clone, Debug)]
pub struct OutputTransform {
    pub target: ColorSpace,
    matrix: [[f32; 3]; 3],
    identity_matrix: bool,
    tone: Option<ToneMap>,
    /// Target luma weights when the working gamut is wider than the target's.
    compress: Option<[f32; 3]>,
}

impl OutputTransform {
    /// For the monitors: SDR BT.709 linear (encode with the sRGB curve as usual).
    pub fn display(pipe: &ColorPipeline) -> Self {
        Self::new(pipe, ColorSpace::Rec709)
    }
    /// For export/encode into `target` (Rec709, Rec2100Pq or Rec2100Hlg).
    pub fn new(pipe: &ColorPipeline, target: ColorSpace) -> Self {
        let wg = pipe.working_gamut();
        let tg = target.gamut();
        let tone = (pipe.working.is_hdr() && !target.is_hdr()).then(|| ToneMap::new(HDR_PEAK_NITS, REFERENCE_WHITE_NITS));
        let l = tg.luma();
        let compress = (!gamut_within(wg, tg)).then(|| [l[0] as f32, l[1] as f32, l[2] as f32]);
        OutputTransform { target, matrix: to_f32(&gamut_matrix(wg, tg)), identity_matrix: wg == tg, tone, compress }
    }
    pub fn is_identity(&self) -> bool {
        self.identity_matrix && self.tone.is_none()
    }
    /// Working linear → target linear (SDR: 1.0 = white; HDR: working units).
    #[inline]
    pub fn apply(&self, mut c: [f32; 3]) -> [f32; 3] {
        if let Some(t) = &self.tone {
            c = t.apply(c);
        }
        if !self.identity_matrix {
            c = crate::spaces::apply3(&self.matrix, c);
        }
        if let Some(l) = self.compress {
            c = desaturate_into_gamut(c, l);
        }
        c
    }
    /// Working linear → encoded signal (0..1, full scale) of the target colour space.
    pub fn encode(&self, c: [f32; 3]) -> [f32; 3] {
        let l = self.apply(c);
        match self.target.curve() {
            Curve::Hlg => {
                // inverse OOTF (BT.2100): display light (normalised to the 1000 cd/m² peak) → scene
                let k = (REFERENCE_WHITE_NITS / HDR_PEAK_NITS) as f32;
                let fd = l.map(|v| (v * k).clamp(0.0, 1.0));
                let lm = Gamut::Bt2020.luma();
                let yd = (lm[0] as f32 * fd[0] + lm[1] as f32 * fd[1] + lm[2] as f32 * fd[2]).max(1e-9);
                let g = yd.powf(((1.0 - HLG_GAMMA) / HLG_GAMMA) as f32);
                fd.map(|v| hlg_oetf((v * g).clamp(0.0, 1.0)))
            }
            curve => l.map(|v| encode_channel(v as f64, curve, Range::Full).clamp(0.0, 1.0) as f32),
        }
    }
}

/// Convenience: a log colour space → display Rec. 709 (tone mapped), used for the built-in
/// camera conversion LUTs.
pub fn log_to_rec709(src: ColorSpace, rgb_signal: [f32; 3], range: Range) -> [f32; 3] {
    let t = InputTransform::new(src, range, &ColorPipeline::REC709, None);
    let l = t.convert(rgb_signal);
    l.map(|v| linear_to_srgb(v.clamp(0.0, 1.0)))
}

/// Decoder-normalised value of a curve-domain value — re-exported for callers that build their
/// own tables.
pub fn domain_value(v: f64, range: Range, d: Domain) -> f64 {
    signal_from_normalized(v, range, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spaces::WorkingSpace;

    fn hdr(w: WorkingSpace) -> ColorPipeline {
        ColorPipeline { working: w, wide_gamut: false, auto_tone_map: true }
    }

    #[test]
    fn channel_roundtrips_every_curve() {
        for cs in ColorSpace::ALL {
            for range in [Range::Limited, Range::Full] {
                for i in 1..100 {
                    let v = i as f64 / 100.0;
                    if cs == ColorSpace::AppleLog && domain_value(v, range, Domain::Ire) < 0.0 {
                        continue; // Apple Log has no code values below its black floor
                    }
                    let l = decode_channel(v, cs.curve(), range);
                    let back = encode_channel(l, cs.curve(), range);
                    assert!((back - v).abs() < 2e-4, "{cs:?} {range:?}: {v} → {l} → {back}");
                }
            }
        }
    }

    #[test]
    fn pq_and_hlg_reference_white() {
        // PQ 58 % ≈ 203 cd/m² (BT.2408 table 1) → 1.0
        let pq203 = pq_inverse_eotf(203.0 / 10_000.0) as f64;
        assert!((pq203 - 0.58).abs() < 0.002, "{pq203}");
        assert!((decode_channel(pq203, Curve::Pq, Range::Full) - 1.0).abs() < 1e-3);
        // HLG 75 % on a 1000 cd/m² display ≈ 203 cd/m² (BT.2408) → ≈ 1.0 working
        let t = InputTransform::new(ColorSpace::Rec2100Hlg, Range::Full, &hdr(WorkingSpace::Rec2100Hlg), None);
        let w = t.convert([0.75, 0.75, 0.75]);
        assert!((w[0] - 1.0).abs() < 0.01, "{w:?}");
        // and back through the HLG output
        let o = OutputTransform::new(&hdr(WorkingSpace::Rec2100Hlg), ColorSpace::Rec2100Hlg);
        let e = o.encode(w);
        assert!((e[0] - 0.75).abs() < 1e-3, "{e:?}");
        let o = OutputTransform::new(&hdr(WorkingSpace::Rec2100Pq), ColorSpace::Rec2100Pq);
        assert!((o.encode([1.0; 3])[1] as f64 - pq203).abs() < 1e-4);
    }

    #[test]
    fn eetf_properties() {
        // identity below the knee, continuous, monotone, hits the target peak
        let (s, d) = (1000.0, 203.0);
        assert!((bt2390_eetf(1.0, s, d) - 1.0).abs() < 1e-3);
        assert!((bt2390_eetf(s, s, d) - d).abs() < 0.5);
        let mut prev = 0.0;
        for i in 1..=1000 {
            let y = bt2390_eetf(i as f64, s, d);
            assert!(y >= prev - 1e-3 && y <= d + 0.05, "{i}: {y}");
            prev = y;
        }
        // reference white ends up at ≈ 0.8 of SDR white (≈ 90 % on the sRGB curve)
        let rw = bt2390_eetf(203.0, s, d) / 203.0;
        assert!((0.7..0.9).contains(&rw), "{rw}");
        assert_eq!(bt2390_eetf(500.0, 203.0, 1000.0), 500.0f64.min(1000.0));
    }

    #[test]
    fn tone_map_preserves_hue_and_maps_peak() {
        let tm = ToneMap::new(1000.0, 203.0);
        let c = [4.0f32, 2.0, 1.0];
        let o = tm.apply(c);
        assert!((o[0] / o[1] - 2.0).abs() < 1e-4 && (o[1] / o[2] - 2.0).abs() < 1e-4);
        let peak = tm.apply([1000.0 / 203.0; 3]);
        assert!((peak[0] - 1.0).abs() < 0.01, "{peak:?}");
        let dark = tm.apply([0.05; 3]);
        assert!((dark[0] - 0.05).abs() < 1e-3);
    }

    #[test]
    fn gamut_compression() {
        // in-gamut colours are untouched
        for c in [[0.5f32, 0.4, 0.3], [1.0, 0.25, 0.25], [0.2, 0.2, 0.2]] {
            assert_eq!(gamut_compress(c), c);
        }
        // anything up to the limit comes back inside, hue order kept
        let o = gamut_compress([1.0, -0.2, 0.3]);
        assert!(o.iter().all(|v| *v >= 0.0), "{o:?}");
        assert!(o[0] > o[2] && o[2] > o[1]);
        // continuity at the threshold
        let a = gamut_compress([1.0, 0.2001, 0.5]);
        assert!((a[1] - 0.2001).abs() < 1e-3);
    }

    #[test]
    fn log_sources_land_sensibly_in_rec709() {
        // mid grey stays mid grey; the S-Gamut3.Cine primaries map out of 709 and get compressed
        for cs in ColorSpace::ALL.into_iter().filter(|c| c.is_log()) {
            let Curve::Log(curve) = cs.curve() else { unreachable!() };
            let grey = normalized_from_signal(curve.encode(0.18), Range::Limited, curve.domain()) as f32;
            let t = InputTransform::new(cs, Range::Limited, &ColorPipeline::REC709, None);
            let g = t.convert([grey; 3]);
            assert!(g.iter().all(|v| (v - 0.18).abs() < 0.01), "{cs:?}: {g:?}");
            let red = t.convert([0.9, grey, grey]);
            assert!(red.iter().all(|v| *v >= -1e-6) && red.iter().all(|v| *v <= 1.0 + 1e-4), "{cs:?}: {red:?}");
            let disp = log_to_rec709(cs, [grey; 3], Range::Limited);
            assert!((disp[0] - linear_to_srgb(0.18)).abs() < 0.02);
        }
    }

    #[test]
    fn gpu_params_cover_display_referred_media_only() {
        // HLG / PQ / BT.2020 into Rec. 709: every stage the shader has (#965)
        let p = |cs| InputTransform::new(cs, Range::Limited, &ColorPipeline::REC709, None).gpu_params();
        let hlg = p(ColorSpace::Rec2100Hlg).unwrap();
        assert_eq!(hlg.curve, GpuCurve::Hlg);
        assert!(hlg.hlg.is_some() && hlg.matrix.is_some() && hlg.desaturate.is_some());
        assert_eq!(hlg.tone, Some((HDR_PEAK_NITS as f32, REFERENCE_WHITE_NITS as f32)));
        let pq = InputTransform::new(ColorSpace::Rec2100Pq, Range::Limited, &ColorPipeline::REC709, Some(4000.0)).gpu_params().unwrap();
        assert_eq!((pq.curve, pq.hlg, pq.tone), (GpuCurve::Pq, None, Some((4000.0, REFERENCE_WHITE_NITS as f32))));
        let sdr2020 = p(ColorSpace::Rec2020).unwrap();
        assert_eq!((sdr2020.curve, sdr2020.tone), (GpuCurve::Sdr, None));
        assert!(sdr2020.matrix.is_some() && sdr2020.desaturate.is_some());
        // log curves and camera-gamut compression stay on the CPU
        for cs in ColorSpace::ALL.into_iter().filter(|c| c.is_log()) {
            assert_eq!(p(cs), None, "{cs:?}");
        }
    }

    #[test]
    fn sdr_in_hdr_keeps_values_and_display_tone_maps() {
        let pipe = hdr(WorkingSpace::Rec2100Pq);
        let t = InputTransform::new(ColorSpace::Rec709, Range::Limited, &pipe, None);
        let w = t.convert([1.0, 1.0, 1.0]);
        assert!(w.iter().all(|v| (v - 1.0).abs() < 1e-4), "709 white = reference white: {w:?}");
        let red = t.convert([1.0, 0.0, 0.0]);
        // 709 red in 2020 (BT.2087)
        assert!((red[0] - 0.6274).abs() < 1e-3 && (red[1] - 0.0691).abs() < 1e-3 && (red[2] - 0.0164).abs() < 1e-3);
        let d = OutputTransform::display(&pipe);
        let back = d.apply(red);
        assert!((back[0] - 0.8).abs() < 0.2 && back[1].abs() < 1e-4 && back[2].abs() < 1e-4, "{back:?}");
        // a BT.2020-only green is desaturated into 709 keeping its luminance
        let g = d.apply([0.0, 0.5, 0.0]);
        assert!(g.iter().all(|v| *v >= 0.0) && g[1] > g[0] && g[1] > g[2], "{g:?}");
        // a 1000-nit highlight comes back at SDR peak
        let hi = d.apply([1000.0 / 203.0; 3]);
        assert!((hi[0] - 1.0).abs() < 0.01);
        // plain pipeline is an identity
        assert!(OutputTransform::display(&ColorPipeline::REC709).is_identity());
    }
}
