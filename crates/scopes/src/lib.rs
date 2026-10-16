//! Video scopes maths (layer L3, no UI): the waveform, parade, histogram and the YUV and HLS
//! vectorscopes of a frame, as count grids ("how many samples land on this cell"), plus numeric
//! summaries for agents ([`summary`]) and the density-to-pixels mapping the UI draws ([`paint`]).
//!
//! The input is a [`Signal`]: the frame's R'G'B' code values (display-encoded, 0..1 nominal;
//! values outside it are super-whites / sub-blacks of float or HDR frames), decimated to at most
//! [`MAX_W`] × [`MAX_H`] samples by nearest-sample picking (no averaging, so flat colours and
//! test patterns land on exact cells). Y'CbCr comes from the scope's colour space matrix
//! (BT.601, BT.709 or BT.2020 NCL; `filmcraft_color::rgb_to_ycbcr`).
//!
//! | Scope | Grid | Columns × rows |
//! |---|---|---|
//! | Waveform RGB / Luma / YC / YC no Chroma | per trace: R, G, B / Y / Y and C (Y ± \|CbCr\|) / Y | signal width × `rows` |
//! | Parade RGB / YUV / RGB-White | per trace: R, G, B / Y, Cb + ½, Cr + ½ / R, G, B, Y | signal width × `rows` (drawn side by side) |
//! | Histogram | 256 bins per channel R, G, B, Y | — |
//! | Vectorscope YUV | Cb horizontal, Cr up, ±[`VECTOR_EXTENT`] | `vector_size`² |
//! | Vectorscope HLS | hue as the angle (red at the YUV red angle), HLS saturation as the radius (1 = 0.5) | `vector_size`² |
//!
//! Waveform rows cover `[lo, hi]` = `[0, 1]` with Clamp Signal on (values clamped first) and
//! `[-0.1, 1.1]` without (values outside are dropped). With 256 rows over `[0, 1]`, an 8-bit code
//! value `c` lands exactly on row `c`.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod paint;
pub mod summary;
#[cfg(test)]
mod tests;

pub use filmcraft_color::Matrix;
use filmcraft_color::rgb_to_ycbcr;
use serde::{Deserialize, Serialize};

/// Largest decimated signal (a 1080p frame is sampled every 4th pixel both ways).
pub const MAX_W: usize = 480;
pub const MAX_H: usize = 270;

/// Half-width of the vectorscope plane in Cb/Cr units (the 100 % targets of every matrix fit).
pub const VECTOR_EXTENT: f32 = 0.6;

/// The skin-tone line (the "I" axis of NTSC): 123° counter-clockwise from +Cb.
pub const SKIN_TONE_DEG: f32 = 123.0;

// ------------------------------------------------------------------------------------ settings

/// A scope of the Lumetri Scopes panel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScopeKind {
    VectorscopeYuv,
    VectorscopeHls,
    Histogram,
    Parade,
    #[default]
    Waveform,
}

impl ScopeKind {
    pub const ALL: [ScopeKind; 5] = [ScopeKind::VectorscopeYuv, ScopeKind::VectorscopeHls, ScopeKind::Histogram, ScopeKind::Parade, ScopeKind::Waveform];
    /// Stable id (`"vectorscopeYuv"`…), as in JSON.
    pub fn name(self) -> &'static str {
        match self {
            ScopeKind::VectorscopeYuv => "vectorscopeYuv",
            ScopeKind::VectorscopeHls => "vectorscopeHls",
            ScopeKind::Histogram => "histogram",
            ScopeKind::Parade => "parade",
            ScopeKind::Waveform => "waveform",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            ScopeKind::VectorscopeYuv => "Vectorscope YUV",
            ScopeKind::VectorscopeHls => "Vectorscope HLS",
            ScopeKind::Histogram => "Histogram",
            ScopeKind::Parade => "Parade",
            ScopeKind::Waveform => "Waveform",
        }
    }
    /// By id or label, ignoring case, spaces and dashes (`"vectorscope"` = the YUV one).
    pub fn from_name(s: &str) -> Option<ScopeKind> {
        let n = norm(s);
        if n == "vectorscope" || n == "yuv" {
            return Some(ScopeKind::VectorscopeYuv);
        }
        if n == "hls" {
            return Some(ScopeKind::VectorscopeHls);
        }
        Self::ALL.into_iter().find(|k| norm(k.name()) == n || norm(k.label()) == n)
    }
}

fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_ascii_lowercase()
}

/// Waveform Type.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WaveformType {
    #[default]
    Rgb,
    Luma,
    Yc,
    YcNoChroma,
}

impl WaveformType {
    pub const ALL: [WaveformType; 4] = [WaveformType::Rgb, WaveformType::Luma, WaveformType::Yc, WaveformType::YcNoChroma];
    pub fn label(self) -> &'static str {
        match self {
            WaveformType::Rgb => "RGB",
            WaveformType::Luma => "Luma",
            WaveformType::Yc => "YC",
            WaveformType::YcNoChroma => "YC no Chroma",
        }
    }
    pub fn from_name(s: &str) -> Option<Self> {
        let n = norm(s);
        Self::ALL.into_iter().find(|k| norm(k.label()) == n || norm(&format!("{k:?}")) == n)
    }
}

/// Parade Type.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ParadeType {
    #[default]
    Rgb,
    Yuv,
    RgbWhite,
}

impl ParadeType {
    pub const ALL: [ParadeType; 3] = [ParadeType::Rgb, ParadeType::Yuv, ParadeType::RgbWhite];
    pub fn label(self) -> &'static str {
        match self {
            ParadeType::Rgb => "RGB",
            ParadeType::Yuv => "YUV",
            ParadeType::RgbWhite => "RGB-White",
        }
    }
    pub fn from_name(s: &str) -> Option<Self> {
        let n = norm(s);
        Self::ALL.into_iter().find(|k| norm(k.label()) == n || norm(&format!("{k:?}")) == n)
    }
}

/// The scopes' colour space (the Y'CbCr matrix; Rec. 2100 also means an HDR signal).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ColorSpace {
    /// Follow the sequence: Rec. 2100 for an HDR working space, else Rec. 709.
    #[default]
    Auto,
    Rec601,
    Rec709,
    Rec2100,
}

impl ColorSpace {
    pub const ALL: [ColorSpace; 4] = [ColorSpace::Auto, ColorSpace::Rec601, ColorSpace::Rec709, ColorSpace::Rec2100];
    pub fn label(self) -> &'static str {
        match self {
            ColorSpace::Auto => "Automatic",
            ColorSpace::Rec601 => "Rec. 601",
            ColorSpace::Rec709 => "Rec. 709",
            ColorSpace::Rec2100 => "Rec. 2100",
        }
    }
    /// `Auto` resolved for a sequence (`hdr`: its working space is PQ/HLG).
    pub fn resolve(self, hdr: bool) -> ColorSpace {
        match self {
            ColorSpace::Auto if hdr => ColorSpace::Rec2100,
            ColorSpace::Auto => ColorSpace::Rec709,
            c => c,
        }
    }
    pub fn matrix(self) -> Matrix {
        match self {
            ColorSpace::Rec601 => Matrix::Bt601,
            ColorSpace::Rec2100 => Matrix::Bt2020Ncl,
            _ => Matrix::Bt709,
        }
    }
    pub fn from_name(s: &str) -> Option<Self> {
        let n = norm(s);
        Self::ALL.into_iter().find(|k| norm(k.label()) == n || norm(&format!("{k:?}")) == n || norm(k.label()).trim_start_matches("rec") == n)
    }
}

/// Signal scale (the bit-depth dropdown): the labels of the levels axis.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Scale {
    /// 0–255 code values.
    #[default]
    Bits8,
    /// 0.0–1.0.
    Float,
    /// cd/m² on a PQ axis (0–10 000).
    Hdr,
}

impl Scale {
    pub const ALL: [Scale; 3] = [Scale::Bits8, Scale::Float, Scale::Hdr];
    pub fn label(self) -> &'static str {
        match self {
            Scale::Bits8 => "8 Bit",
            Scale::Float => "Float",
            Scale::Hdr => "HDR",
        }
    }
    pub fn from_name(s: &str) -> Option<Self> {
        let n = norm(s);
        Self::ALL.into_iter().find(|k| norm(k.label()) == n || norm(&format!("{k:?}")) == n)
    }
}

/// Trace brightness.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Brightness {
    Dimmed,
    #[default]
    Normal,
    Bright,
}

impl Brightness {
    pub const ALL: [Brightness; 3] = [Brightness::Dimmed, Brightness::Normal, Brightness::Bright];
    pub fn label(self) -> &'static str {
        match self {
            Brightness::Dimmed => "Dimmed",
            Brightness::Normal => "Normal",
            Brightness::Bright => "Bright",
        }
    }
    /// Multiplier of the trace intensity.
    pub fn gain(self) -> f32 {
        match self {
            Brightness::Dimmed => 0.55,
            Brightness::Normal => 1.0,
            Brightness::Bright => 1.6,
        }
    }
}

/// Which colour-bar targets the YUV vectorscope draws.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Targets {
    #[default]
    #[serde(rename = "75")]
    Percent75,
    #[serde(rename = "100")]
    Percent100,
}

impl Targets {
    pub fn amplitude(self) -> f32 {
        match self {
            Targets::Percent75 => 0.75,
            Targets::Percent100 => 1.0,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Targets::Percent75 => "75%",
            Targets::Percent100 => "100%",
        }
    }
}

/// How scopes are computed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    pub matrix: Matrix,
    /// Clamp Signal: values are clamped to 0..1 before they are plotted.
    pub clamp: bool,
    /// Waveform / parade levels.
    pub rows: usize,
    /// Vectorscope grid size (square; odd, so the centre is a cell).
    pub vector_size: usize,
}

impl Default for Params {
    fn default() -> Self {
        Params { matrix: Matrix::Bt709, clamp: true, rows: 256, vector_size: 255 }
    }
}

impl Params {
    /// The plotted value range of waveform rows.
    pub fn range(&self) -> (f32, f32) {
        if self.clamp { (0.0, 1.0) } else { (-0.1, 1.1) }
    }
}

// ------------------------------------------------------------------------------------ signal

/// A frame's R'G'B' samples (decimated), row-major.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Signal {
    pub w: usize,
    pub h: usize,
    pub rgb: Vec<[f32; 3]>,
}

fn steps(w: usize, h: usize, max_w: usize, max_h: usize) -> (usize, usize) {
    (w.div_ceil(max_w.max(1)).max(1), h.div_ceil(max_h.max(1)).max(1))
}

impl Signal {
    /// From straight RGBA8 (code / 255), decimated to [`MAX_W`] × [`MAX_H`].
    pub fn from_rgba8(w: usize, h: usize, px: &[u8]) -> Signal {
        Self::from_rgba8_max(w, h, px, MAX_W, MAX_H)
    }

    pub fn from_rgba8_max(w: usize, h: usize, px: &[u8], max_w: usize, max_h: usize) -> Signal {
        if w == 0 || h == 0 || px.len() < w * h * 4 {
            return Signal::default();
        }
        let lut: [f32; 256] = std::array::from_fn(|i| i as f32 / 255.0);
        let (sx, sy) = steps(w, h, max_w, max_h);
        let (ow, oh) = (w.div_ceil(sx), h.div_ceil(sy));
        let mut rgb = Vec::with_capacity(ow * oh);
        for y in (0..h).step_by(sy) {
            let row = &px[y * w * 4..(y + 1) * w * 4];
            for x in (0..w).step_by(sx) {
                let p = &row[x * 4..x * 4 + 3];
                rgb.push([lut[p[0] as usize], lut[p[1] as usize], lut[p[2] as usize]]);
            }
        }
        Signal { w: ow, h: oh, rgb }
    }

    /// From RGBA f32 pixels through `map` (e.g. linear light → PQ code values), decimated.
    pub fn from_rgba_f32_with(w: usize, h: usize, px: &[f32], max_w: usize, max_h: usize, map: impl Fn([f32; 4]) -> [f32; 3]) -> Signal {
        if w == 0 || h == 0 || px.len() < w * h * 4 {
            return Signal::default();
        }
        let (sx, sy) = steps(w, h, max_w, max_h);
        let (ow, oh) = (w.div_ceil(sx), h.div_ceil(sy));
        let mut rgb = Vec::with_capacity(ow * oh);
        for y in (0..h).step_by(sy) {
            for x in (0..w).step_by(sx) {
                let i = (y * w + x) * 4;
                rgb.push(map([px[i], px[i + 1], px[i + 2], px[i + 3]]));
            }
        }
        Signal { w: ow, h: oh, rgb }
    }

    /// A synthetic signal (tests, generated patterns).
    pub fn from_fn(w: usize, h: usize, f: impl Fn(usize, usize) -> [f32; 3]) -> Signal {
        let mut rgb = Vec::with_capacity(w * h);
        for y in 0..h {
            for x in 0..w {
                rgb.push(f(x, y));
            }
        }
        Signal { w, h, rgb }
    }

    /// Every sample through `f` (e.g. SDR code values → PQ for the HDR scale).
    pub fn map(&self, f: impl Fn([f32; 3]) -> [f32; 3]) -> Signal {
        Signal { w: self.w, h: self.h, rgb: self.rgb.iter().map(|c| f(*c)).collect() }
    }

    pub fn len(&self) -> usize {
        self.rgb.len()
    }
    pub fn is_empty(&self) -> bool {
        self.rgb.is_empty()
    }
}

/// Linear light (1.0 = reference white = 203 cd/m², premultiplied RGBA) → PQ code values, for
/// HDR scopes: 0..1 on the axis is 0..10 000 cd/m².
pub fn linear_to_pq([r, g, b, a]: [f32; 4]) -> [f32; 3] {
    let a = if a > 1e-6 { a } else { 1.0 };
    let f = |v: f32| filmcraft_color::pq_inverse_eotf(((v / a).max(0.0) * filmcraft_color::REFERENCE_WHITE_NITS as f32 / 10_000.0).clamp(0.0, 1.0));
    [f(r), f(g), f(b)]
}

/// An SDR code value (BT.1886, 100 cd/m² white) on the PQ axis (the HDR scale of an SDR frame).
pub fn sdr_to_pq(v: f32) -> f32 {
    filmcraft_color::pq_inverse_eotf((v.clamp(0.0, 1.0).powf(2.4) * 100.0 / 10_000.0).clamp(0.0, 1.0))
}

/// cd/m² → height on the PQ axis (0..1).
pub fn nits_to_pq(nits: f32) -> f32 {
    filmcraft_color::pq_inverse_eotf((nits / 10_000.0).clamp(0.0, 1.0))
}

// ------------------------------------------------------------------------------------ grids

/// A count grid: `data[row * cols + col]`, row 0 = the lowest level.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Grid {
    pub name: &'static str,
    pub cols: usize,
    pub rows: usize,
    pub data: Vec<u32>,
}

impl Grid {
    pub fn new(name: &'static str, cols: usize, rows: usize) -> Grid {
        Grid { name, cols, rows, data: vec![0; cols * rows] }
    }
    pub fn at(&self, col: usize, row: usize) -> u32 {
        self.data[row * self.cols + col]
    }
    pub fn total(&self) -> u64 {
        self.data.iter().map(|&c| c as u64).sum()
    }
    pub fn max(&self) -> u32 {
        self.data.iter().copied().max().unwrap_or(0)
    }
    /// The rows hit in a column, lowest first.
    pub fn rows_in(&self, col: usize) -> Vec<usize> {
        (0..self.rows).filter(|&r| self.at(col, r) > 0).collect()
    }
}

/// Waveform or parade: one grid per trace, columns = the signal's width.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Waveform {
    pub lo: f32,
    pub hi: f32,
    /// Samples per column (the signal's height).
    pub per_column: usize,
    pub traces: Vec<Grid>,
}

impl Waveform {
    pub fn trace(&self, name: &str) -> Option<&Grid> {
        self.traces.iter().find(|t| t.name == name)
    }
    /// The value at the centre of a row.
    pub fn level(&self, row: usize, rows: usize) -> f32 {
        self.lo + (self.hi - self.lo) * row as f32 / (rows.max(2) - 1) as f32
    }
}

/// Row of value `v` in `[lo, hi]` over `rows` levels (None outside, or NaN).
#[inline]
pub fn row_of(v: f32, lo: f32, hi: f32, rows: usize) -> Option<usize> {
    let t = (v - lo) / (hi - lo);
    if !(0.0..=1.0).contains(&t) {
        return None;
    }
    Some(((t * (rows - 1) as f32).round() as usize).min(rows - 1))
}

/// Up to 4 (trace, value) pairs per sample.
type Hits = ([(u8, f32); 4], usize);

fn build(s: &Signal, p: &Params, names: &[&'static str], sample: impl Fn([f32; 3]) -> Hits) -> Waveform {
    let (lo, hi) = p.range();
    let rows = p.rows.max(2);
    let mut traces: Vec<Grid> = names.iter().map(|n| Grid::new(n, s.w, rows)).collect();
    for y in 0..s.h {
        for x in 0..s.w {
            let mut c = s.rgb[y * s.w + x];
            if p.clamp {
                c = c.map(|v| v.clamp(0.0, 1.0));
            }
            let (hits, n) = sample(c);
            for &(t, v) in &hits[..n] {
                let v = if p.clamp { v.clamp(0.0, 1.0) } else { v };
                if let Some(r) = row_of(v, lo, hi, rows) {
                    let g = &mut traces[t as usize];
                    g.data[r * g.cols + x] += 1;
                }
            }
        }
    }
    Waveform { lo, hi, per_column: s.h, traces }
}

/// The waveform of a signal.
pub fn waveform(s: &Signal, kind: WaveformType, p: &Params) -> Waveform {
    let m = p.matrix;
    let z = (0u8, 0.0f32);
    match kind {
        WaveformType::Rgb => build(s, p, &["R", "G", "B"], |[r, g, b]| ([(0, r), (1, g), (2, b), z], 3)),
        WaveformType::Luma => build(s, p, &["Y"], |[r, g, b]| ([(0, rgb_to_ycbcr(r, g, b, m)[0]), z, z, z], 1)),
        WaveformType::YcNoChroma => build(s, p, &["Y"], |[r, g, b]| ([(0, rgb_to_ycbcr(r, g, b, m)[0]), z, z, z], 1)),
        WaveformType::Yc => build(s, p, &["Y", "C"], |[r, g, b]| {
            let [y, cb, cr] = rgb_to_ycbcr(r, g, b, m);
            let c = (cb * cb + cr * cr).sqrt();
            ([(0, y), (1, y + c), (1, y - c), z], if c > 1e-6 { 3 } else { 2 })
        }),
    }
}

/// The parade of a signal (the traces are drawn side by side).
pub fn parade(s: &Signal, kind: ParadeType, p: &Params) -> Waveform {
    let m = p.matrix;
    let z = (0u8, 0.0f32);
    match kind {
        ParadeType::Rgb => build(s, p, &["R", "G", "B"], |[r, g, b]| ([(0, r), (1, g), (2, b), z], 3)),
        ParadeType::RgbWhite => build(s, p, &["R", "G", "B", "Y"], |[r, g, b]| ([(0, r), (1, g), (2, b), (3, rgb_to_ycbcr(r, g, b, m)[0])], 4)),
        ParadeType::Yuv => build(s, p, &["Y", "Cb", "Cr"], |[r, g, b]| {
            let [y, cb, cr] = rgb_to_ycbcr(r, g, b, m);
            ([(0, y), (1, cb + 0.5), (2, cr + 0.5), z], 3)
        }),
    }
}

// ------------------------------------------------------------------------------------ histogram

/// 256-bin histograms of R', G', B' and Y'.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Histogram {
    pub r: Vec<u32>,
    pub g: Vec<u32>,
    pub b: Vec<u32>,
    pub y: Vec<u32>,
    pub samples: u32,
    /// Samples below 0 / above 1 per channel (R, G, B, Y); with Clamp Signal they are also
    /// counted in the first / last bin.
    pub below: [u32; 4],
    pub above: [u32; 4],
}

pub const BINS: usize = 256;

/// Bin of a value (None when outside 0..1).
#[inline]
pub fn bin_of(v: f32) -> Option<usize> {
    if (0.0..=1.0 + 1e-6).contains(&v) { Some(((v * 255.0).round() as usize).min(BINS - 1)) } else { None }
}

pub fn histogram(s: &Signal, p: &Params) -> Histogram {
    let mut h = Histogram { r: vec![0; BINS], g: vec![0; BINS], b: vec![0; BINS], y: vec![0; BINS], samples: s.len() as u32, below: [0; 4], above: [0; 4] };
    for &[r, g, b] in &s.rgb {
        let y = rgb_to_ycbcr(r, g, b, p.matrix)[0];
        for (i, v) in [r, g, b, y].into_iter().enumerate() {
            let bins = match i {
                0 => &mut h.r,
                1 => &mut h.g,
                2 => &mut h.b,
                _ => &mut h.y,
            };
            match bin_of(v) {
                Some(k) => bins[k] += 1,
                None if v.is_nan() => {}
                None => {
                    let low = v < 0.0;
                    if low {
                        h.below[i] += 1;
                    } else {
                        h.above[i] += 1;
                    }
                    if p.clamp {
                        bins[if low { 0 } else { BINS - 1 }] += 1;
                    }
                }
            }
        }
    }
    h
}

// ------------------------------------------------------------------------------------ vectorscopes

/// A vectorscope: `data[y * size + x]`, x = Cb (right = +), y = Cr (row 0 = top = +Cr).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Vectorscope {
    pub size: usize,
    pub extent: f32,
    pub data: Vec<u32>,
    pub samples: u32,
    /// Mean (Cb, Cr) of the plotted points.
    pub mean: [f32; 2],
}

/// Cell of a (Cb, Cr) point on a `size`² plane spanning ±`extent`.
#[inline]
pub fn cell_of(cb: f32, cr: f32, size: usize, extent: f32) -> Option<(usize, usize)> {
    let n = (size - 1) as f32;
    let fx = (cb / extent * 0.5 + 0.5) * n;
    let fy = (0.5 - cr / extent * 0.5) * n;
    if !(0.0..=n).contains(&fx) || !(0.0..=n).contains(&fy) {
        return None;
    }
    Some((fx.round() as usize, fy.round() as usize))
}

impl Vectorscope {
    fn new(size: usize) -> Self {
        Vectorscope { size, extent: VECTOR_EXTENT, data: vec![0; size * size], samples: 0, mean: [0.0; 2] }
    }
    pub fn at(&self, x: usize, y: usize) -> u32 {
        self.data[y * self.size + x]
    }
    pub fn cell(&self, cb: f32, cr: f32) -> Option<(usize, usize)> {
        cell_of(cb, cr, self.size, self.extent)
    }
    /// (Cb, Cr) at the centre of a cell.
    pub fn point(&self, x: usize, y: usize) -> [f32; 2] {
        let n = (self.size - 1) as f32;
        [(x as f32 / n - 0.5) * 2.0 * self.extent, (0.5 - y as f32 / n) * 2.0 * self.extent]
    }
    fn plot(&mut self, cb: f32, cr: f32, sum: &mut [f64; 2]) {
        if let Some((x, y)) = self.cell(cb, cr) {
            self.data[y * self.size + x] += 1;
            self.samples += 1;
            sum[0] += cb as f64;
            sum[1] += cr as f64;
        }
    }
    fn finish(mut self, sum: [f64; 2]) -> Self {
        if self.samples > 0 {
            self.mean = [(sum[0] / self.samples as f64) as f32, (sum[1] / self.samples as f64) as f32];
        }
        self
    }
}

/// The YUV vectorscope (Cb, Cr of each sample).
pub fn vectorscope_yuv(s: &Signal, p: &Params) -> Vectorscope {
    let mut v = Vectorscope::new(p.vector_size.max(2));
    let mut sum = [0f64; 2];
    for &c in &s.rgb {
        let [r, g, b] = if p.clamp { c.map(|x| x.clamp(0.0, 1.0)) } else { c };
        let [_, cb, cr] = rgb_to_ycbcr(r, g, b, p.matrix);
        v.plot(cb, cr, &mut sum);
    }
    v.finish(sum)
}

/// Angle (degrees, counter-clockwise from +Cb, 0..360) of a chroma point.
pub fn angle_deg(cb: f32, cr: f32) -> f32 {
    cr.atan2(cb).to_degrees().rem_euclid(360.0)
}

/// The angle of pure red on a matrix's vectorscope (BT.709 ≈ 103°): the HLS scope puts hue 0 there.
pub fn red_angle(m: Matrix) -> f32 {
    let [_, cb, cr] = rgb_to_ycbcr(1.0, 0.0, 0.0, m);
    angle_deg(cb, cr)
}

/// The HLS vectorscope.
pub fn vectorscope_hls(s: &Signal, p: &Params) -> Vectorscope {
    let mut v = Vectorscope::new(p.vector_size.max(2));
    let mut sum = [0f64; 2];
    // hue → unit direction, tabulated at 0.1° (trigonometry per sample is the slow part)
    let red = red_angle(p.matrix);
    let dirs: Vec<[f32; 2]> = (0..=3600).map(|i| (red + i as f32 / 10.0).to_radians()).map(|a| [a.cos(), a.sin()]).collect();
    for &c in &s.rgb {
        let [r, g, b] = c.map(|x| x.clamp(0.0, 1.0));
        let [h, sat, _] = filmcraft_color::rgb_to_hsl(r, g, b);
        let [dx, dy] = dirs[((h * 3600.0).round() as usize).min(3600)];
        let rad = sat.clamp(0.0, 1.0) * 0.5;
        v.plot(rad * dx, rad * dy, &mut sum);
    }
    v.finish(sum)
}

/// A colour-bar target of the YUV vectorscope.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Target {
    pub name: &'static str,
    pub rgb: [f32; 3],
    pub cb: f32,
    pub cr: f32,
}

/// The six colour-bar targets (R, Mg, B, Cy, G, Yl) at `amplitude` (0.75 or 1.0).
pub fn targets(m: Matrix, amplitude: f32) -> [Target; 6] {
    let a = amplitude;
    let t = |name, rgb: [f32; 3]| {
        let [_, cb, cr] = rgb_to_ycbcr(rgb[0], rgb[1], rgb[2], m);
        Target { name, rgb, cb, cr }
    };
    [t("R", [a, 0.0, 0.0]), t("Mg", [a, 0.0, a]), t("B", [0.0, 0.0, a]), t("Cy", [0.0, a, a]), t("G", [0.0, a, 0.0]), t("Yl", [a, a, 0.0])]
}

/// Hue labels of the HLS vectorscope at radius 0.5: (name, Cb, Cr).
pub fn hls_targets(m: Matrix) -> [(&'static str, f32, f32); 6] {
    let red = red_angle(m);
    let at = |name, h: f32| {
        let a = (red + h).to_radians();
        (name, 0.5 * a.cos(), 0.5 * a.sin())
    };
    [at("R", 0.0), at("Yl", 60.0), at("G", 120.0), at("Cy", 180.0), at("B", 240.0), at("Mg", 300.0)]
}

// ------------------------------------------------------------------------------------ all scopes

/// Every scope the panel can show, computed for a set of settings.
#[derive(Clone, Debug, PartialEq)]
pub enum Computed {
    Waveform(Waveform),
    Parade(Waveform),
    Histogram(Histogram),
    Vectorscope(Vectorscope),
}

/// Compute one scope.
pub fn compute(kind: ScopeKind, s: &Signal, p: &Params, waveform_type: WaveformType, parade_type: ParadeType) -> Computed {
    match kind {
        ScopeKind::Waveform => Computed::Waveform(waveform(s, waveform_type, p)),
        ScopeKind::Parade => Computed::Parade(parade(s, parade_type, p)),
        ScopeKind::Histogram => Computed::Histogram(histogram(s, p)),
        ScopeKind::VectorscopeYuv => Computed::Vectorscope(vectorscope_yuv(s, p)),
        ScopeKind::VectorscopeHls => Computed::Vectorscope(vectorscope_hls(s, p)),
    }
}
