//! Raw samples to a linear Rec. 709 picture, in the order of the DNG specification's chapter 5
//! ("Mapping Raw Values to Linear Reference Values") and chapter 6 (colour):
//!
//! 1. ActiveArea: only the active (unmasked) sensor area is used; black levels, the CFA pattern
//!    and the default crop are relative to its top left corner.
//! 2. LinearizationTable maps each sample.
//! 3. The black level (BlackLevel over its BlackLevelRepeatDim pattern, plus BlackLevelDeltaH for
//!    the column and BlackLevelDeltaV for the row) is subtracted and the range up to WhiteLevel
//!    scaled to 0–1 (clipped there).
//! 4. White balance: each sample is divided by the camera neutral of its colour and clipped at
//!    1, so highlights the sensor clipped stay neutral.
//! 5. A CFA is demosaiced ([`crate::demosaic`]).
//! 6. DefaultCropOrigin / DefaultCropSize cut the picture.
//! 7. The colour matrix of [`crate::color`] takes the white-balanced camera values to linear
//!    Rec. 709 (including BaselineExposure); negative results are clipped to 0.

#[cfg(feature = "threads")]
use rayon::prelude::*;

use crate::color::{M3, V3};
use crate::demosaic::Pattern;
use crate::opcode::{self, Opcode};
use crate::options::ProcessOptions;
use crate::raw::RawImage;
use crate::{Error, Result};

/// The processing tags of a raw image.
#[derive(Clone, Debug, Default)]
pub struct Params {
    /// top, left, bottom, right
    pub active_area: Option<[usize; 4]>,
    pub linearization: Option<Vec<u16>>,
    /// rows, columns of the BlackLevel pattern
    pub black_repeat: (usize, usize),
    /// rows × columns × samples per pixel values
    pub black: Vec<f64>,
    pub black_delta_h: Vec<f64>,
    pub black_delta_v: Vec<f64>,
    /// per sample
    pub white: Vec<f64>,
    pub crop_origin: Option<(f64, f64)>,
    pub crop_size: Option<(f64, f64)>,
    /// CFA pattern in camera planes (`None`: LinearRaw).
    pub cfa: Option<Pattern>,
    /// OpcodeList1–3.
    pub opcodes: crate::opcode::Lists,
}

/// A rectangle: x, y, width, height.
type Rect = (usize, usize, usize, usize);

/// The active area of a `w` × `h` raw image.
pub fn active_rect(p: &Params, w: usize, h: usize) -> Result<Rect> {
    match p.active_area {
        None => Ok((0, 0, w, h)),
        Some([top, left, bottom, right]) => {
            if top >= bottom || left >= right || bottom > h || right > w {
                return Err(Error::Malformed("ActiveArea outside the image"));
            }
            Ok((left, top, right - left, bottom - top))
        }
    }
}

/// The default crop inside an active area of `aw` × `ah`.
pub fn crop_rect(p: &Params, aw: usize, ah: usize) -> Rect {
    let (ox, oy) = p.crop_origin.unwrap_or((0.0, 0.0));
    let (sw, sh) = p.crop_size.unwrap_or((aw as f64, ah as f64));
    let to = |v: f64, max: usize| if v.is_finite() && v > 0.0 { (v.min(max as f64)) as usize } else { 0 };
    let x = to(ox.floor(), aw.saturating_sub(1));
    let y = to(oy.floor(), ah.saturating_sub(1));
    let w = to(sw.round(), aw - x).max(1);
    let h = to(sh.round(), ah - y).max(1);
    (x, y, w, h)
}

/// Rows-parallel map over `out` in rows of `row_len` values (sequential without `threads`).
fn for_rows(out: &mut [f32], row_len: usize, f: impl Fn(usize, &mut [f32]) + Sync + Send) {
    let row_len = row_len.max(1);
    #[cfg(feature = "threads")]
    out.par_chunks_mut(row_len).enumerate().for_each(|(y, o)| f(y, o));
    #[cfg(not(feature = "threads"))]
    out.chunks_mut(row_len).enumerate().for_each(|(y, o)| f(y, o));
}

fn gcd(a: usize, b: usize) -> usize {
    if b == 0 { a } else { gcd(b, a % b) }
}

fn lcm(a: usize, b: usize) -> usize {
    a / gcd(a, b).max(1) * b
}

/// Most values in a normalisation lookup table (sites × codes).
const MAX_LUT: usize = 1 << 21;

/// Steps 2–4 for one sample: linearisation, black and white levels, and (with `wb`) white
/// balance with the clip at 1.
struct Normaliser<'a> {
    spp: usize,
    br: usize,
    bc: usize,
    black: Vec<f64>,
    white: Vec<f64>,
    lin: Option<&'a [u16]>,
    inv_neutral: Vec<f32>,
    cfa: Option<&'a Pattern>,
    wb: bool,
}

impl Normaliser<'_> {
    #[inline]
    fn value(&self, code: u16, x: usize, y: usize, s: usize, dh: f64, dv: f64) -> f32 {
        let v = match self.lin {
            Some(t) => f64::from(t.get(usize::from(code)).or(t.last()).copied().unwrap_or(0)),
            None => f64::from(code),
        };
        let b = self.black.get(((y % self.br) * self.bc + x % self.bc) * self.spp + s).copied().unwrap_or(0.0) + dh + dv;
        let range = self.white.get(s).copied().unwrap_or(1.0) - b;
        let n = if range > 0.0 { ((v - b) / range).clamp(0.0, 1.0) } else { 0.0 };
        if !self.wb {
            return n as f32;
        }
        let plane = match self.cfa {
            Some(c) => usize::from(c.color(x, y)),
            None => s,
        };
        let wb = self.inv_neutral.get(plane).copied().unwrap_or(1.0);
        (n as f32 * wb).min(1.0)
    }
}

/// Normalised samples of the active area (steps 1–4), one plane per sample; white balanced and
/// clipped at 1 when `wb` (else only clipped to 0–1, for OpcodeList2).
///
/// Without black-level deltas every sample is a function of its code and its position in the
/// black-level and CFA repeat, so a lookup table per site replaces the arithmetic (same values:
/// each entry is computed by the same expression).
fn normalise_with(raw: &RawImage, p: &Params, (ax, ay, aw, ah): Rect, neutral: &V3, planes: usize, wb: bool) -> Result<Vec<f32>> {
    let spp = raw.samples_per_pixel;
    let (br, bc) = (p.black_repeat.0.max(1), p.black_repeat.1.max(1));
    if br > 64 || bc > 64 {
        return Err(Error::Unsupported("a BlackLevelRepeatDim larger than 64".into()));
    }
    let need = br * bc * spp;
    let black: Vec<f64> = match p.black.len() {
        0 => vec![0.0; need],
        1 => vec![p.black[0]; need],
        n if n >= need => p.black[..need].to_vec(),
        _ => return Err(Error::Malformed("BlackLevel has too few values")),
    };
    let max_code = ((1u32 << raw.bits.min(16)) - 1) as f64;
    let white: Vec<f64> = (0..spp).map(|s| p.white.get(s).or(p.white.first()).copied().unwrap_or(max_code)).collect();
    let lin = p.linearization.as_deref().filter(|t| !t.is_empty());
    let inv_neutral: Vec<f32> = (0..planes).map(|c| (1.0 / neutral.get(c).copied().unwrap_or(1.0)) as f32).collect();
    let cfa = p.cfa.as_ref();
    let n = Normaliser { spp, br, bc, black, white, lin, inv_neutral, cfa, wb };
    let row_len = aw * spp;
    let src = |y: usize| raw.data.get((ay + y) * raw.width * spp + ax * spp..).unwrap_or(&[]);
    let mut out = vec![0f32; aw * ah * spp];
    // the repeat of the black levels and the CFA together
    let (rr, rc) = match cfa {
        Some(c) => (lcm(br, c.rows.max(1)), lcm(bc, c.cols.max(1))),
        None => (br, bc),
    };
    let deltas = p.black_delta_h.iter().chain(&p.black_delta_v).any(|&d| d != 0.0);
    let lut_len = 1usize << raw.bits.clamp(1, 16);
    let sites = rr * rc * spp;
    let use_lut = !deltas && sites.saturating_mul(lut_len) <= MAX_LUT && sites * lut_len <= out.len();
    if use_lut {
        // lut[((y % rr) * rc + x % rc) * spp + s][code]
        let mut lut = vec![0f32; sites * lut_len];
        let fill = |site: usize, t: &mut [f32]| {
            let (s, xy) = (site % spp, site / spp);
            let (y, x) = (xy / rc, xy % rc);
            for (code, v) in t.iter_mut().enumerate() {
                *v = n.value(code as u16, x, y, s, 0.0, 0.0);
            }
        };
        for_rows(&mut lut, lut_len, fill);
        let lut = &lut;
        for_rows(&mut out, row_len, |y, o| {
            let src = src(y);
            let row_sites = (y % rr) * rc * spp;
            let mut site = row_sites;
            let row_end = row_sites + rc * spp;
            for (x, (o, &code)) in o.iter_mut().zip(src).enumerate() {
                let table = lut.get(site * lut_len..(site + 1) * lut_len).unwrap_or(&[]);
                *o = match table.get(usize::from(code)) {
                    Some(&v) => v,
                    None => n.value(code, x / spp, y, x % spp, 0.0, 0.0),
                };
                site += 1;
                if site == row_end {
                    site = row_sites;
                }
            }
        });
    } else {
        for_rows(&mut out, row_len, |y, o| {
            let src = src(y);
            let dv = p.black_delta_v.get(y).copied().unwrap_or(0.0);
            for (x, (px, codes)) in o.chunks_exact_mut(spp).zip(src.chunks_exact(spp)).enumerate() {
                let dh = p.black_delta_h.get(x).copied().unwrap_or(0.0);
                for (s, (o, &code)) in px.iter_mut().zip(codes).enumerate() {
                    *o = n.value(code, x, y, s, dh, dv);
                }
            }
        });
    }
    Ok(out)
}

/// Normalised, white-balanced samples of the active area (steps 1–4), one plane per sample.
fn normalise(raw: &RawImage, p: &Params, rect: Rect, neutral: &V3, planes: usize) -> Result<Vec<f32>> {
    normalise_with(raw, p, rect, neutral, planes, true)
}

/// A developed picture: premultiplied linear Rec. 709 RGBA floats (alpha 1).
pub struct Developed {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<f32>,
}

/// Develop `raw` (steps 1–7). `neutral` and `matrix` come from [`crate::color::Calibration::solve`].
pub fn develop(raw: &RawImage, p: &Params, neutral: &V3, matrix: &M3) -> Result<Developed> {
    develop_with(raw, p, neutral, matrix, &ProcessOptions::default())
}

/// The output pixel of white-balanced camera values `c` (step 7).
#[inline(always)]
fn to_output(m: &[[f32; 3]; 3], mono: bool, c: [f32; 3], d: &mut [f32; 4]) {
    let px = if mono {
        // grey: the matrix's luminance gain only
        let g = c[0] * (m[1][0] + m[1][1] + m[1][2]);
        [g, g, g]
    } else {
        [0, 1, 2].map(|i| m[i][0] * c[0] + m[i][1] * c[1] + m[i][2] * c[2])
    };
    *d = [px[0].max(0.0), px[1].max(0.0), px[2].max(0.0), 1.0];
}

/// The reduction factor (1, 2, 4 or 8) for a `cw` × `ch` picture when the caller needs at least
/// `target`: the largest that still covers it.
pub fn reduction(target: Option<(usize, usize)>, cw: usize, ch: usize) -> usize {
    let Some((tw, th)) = target else { return 1 };
    let mut f = 1;
    while f < 8 && cw / (f * 2) >= tw.max(1) && ch / (f * 2) >= th.max(1) {
        f *= 2;
    }
    f
}

/// Whether `list` has an opcode to apply (`preview`: those marked as needed only for full
/// quality are left out).
fn active(list: &[Opcode], preview: bool) -> bool {
    list.iter().any(|o| !(preview && o.preview_skippable()))
}

/// Whether this development is a reduced-resolution preview: a target size smaller than the
/// crop, and no OpcodeList3 work that has to be done at full size.
pub fn preview(p: &Params, o: &ProcessOptions, info: &crate::Info) -> bool {
    let reduced = reduction(o.target_size, info.width, info.height) > 1 && p.cfa.as_ref().is_none_or(Pattern::is_bayer);
    reduced && !active(&p.opcodes.list3, true)
}

/// OpcodeList1 on the stored samples (integer range 0–65535).
pub fn apply_list1(raw: &mut RawImage, p: &Params, preview: bool) -> Result<()> {
    if !active(&p.opcodes.list1, preview) {
        return Ok(());
    }
    let mut data: Vec<f32> = raw.data.iter().map(|&v| f32::from(v)).collect();
    let mut img = opcode::Image {
        data: &mut data,
        width: raw.width,
        height: raw.height,
        planes: raw.samples_per_pixel,
        max: 65_535.0,
        mosaic: p.cfa.is_some(),
        allow_trim: false,
        origin: (0, 0),
    };
    opcode::apply(&p.opcodes.list1, &mut img, preview)?;
    raw.data = data.iter().map(|&v| v.clamp(0.0, 65_535.0) as u16).collect();
    Ok(())
}

/// [`develop`] with raw settings. White balance and exposure are already in `neutral` and
/// `matrix` ([`crate::Dng::develop_with`] solves them); this applies the demosaic quality and the
/// reduced-resolution path.
pub fn develop_with(raw: &RawImage, p: &Params, neutral: &V3, matrix: &M3, o: &ProcessOptions) -> Result<Developed> {
    let spp = raw.samples_per_pixel;
    if p.cfa.is_some() && spp != 1 {
        return Err(Error::Malformed("a CFA image with more than one sample per pixel"));
    }
    if p.cfa.is_none() && !matches!(spp, 1 | 3) {
        return Err(Error::Unsupported(format!("LinearRaw with {spp} samples per pixel")));
    }
    if raw.data.len() < raw.width.saturating_mul(raw.height).saturating_mul(spp) {
        return Err(Error::Truncated);
    }
    let active_r = active_rect(p, raw.width, raw.height)?;
    let (_, _, aw, ah) = active_r;
    let planes = if p.cfa.is_some() { 3 } else { spp };
    let crop = crop_rect(p, aw, ah);
    let m: [[f32; 3]; 3] = matrix.map(|r| r.map(|v| v as f32));
    let f = reduction(o.target_size, crop.2, crop.3);
    let preview = f > 1 && p.cfa.as_ref().is_none_or(Pattern::is_bayer) && !active(&p.opcodes.list3, true);
    let norm = if active(&p.opcodes.list2, preview) {
        // OpcodeList2 works on the linear reference values, before the white balance
        let mut norm = normalise_with(raw, p, active_r, neutral, planes, false)?;
        let mut img =
            opcode::Image { data: &mut norm, width: aw, height: ah, planes: spp, max: 1.0, mosaic: p.cfa.is_some(), allow_trim: false, origin: (0, 0) };
        opcode::apply(&p.opcodes.list2, &mut img, preview)?;
        let inv: Vec<f32> = (0..planes).map(|c| (1.0 / neutral.get(c).copied().unwrap_or(1.0)) as f32).collect();
        for_rows(&mut norm, aw * spp, |y, row| {
            for (i, v) in row.iter_mut().enumerate() {
                let plane = match &p.cfa {
                    Some(c) => usize::from(c.color(i, y)),
                    None => i % spp,
                };
                *v = (*v * inv.get(plane).copied().unwrap_or(1.0)).min(1.0);
            }
        });
        norm
    } else {
        normalise(raw, p, active_r, neutral, planes)?
    };
    if preview {
        return Ok(binned(&norm, aw, spp, p.cfa.as_ref(), crop, f, &m));
    }
    if active(&p.opcodes.list3, false) {
        return with_list3(norm, aw, ah, spp, p, crop, o, &m, neutral);
    }
    Ok(full(norm, aw, ah, spp, p.cfa.as_ref(), crop, o, &m))
}

/// Steps 5–7 at full resolution: demosaic, crop and colour, row by row without intermediate
/// pictures for 2 × 2 Bayer patterns and LinearRaw.
fn full(norm: Vec<f32>, aw: usize, ah: usize, spp: usize, cfa: Option<&Pattern>, (cx, cy, cw, ch): Rect, o: &ProcessOptions, m: &[[f32; 3]; 3]) -> Developed {
    let mono = cfa.is_none() && spp == 1;
    let mut rgba = vec![0f32; cw * ch * 4];
    match cfa {
        Some(pat) if pat.is_bayer() && aw >= 3 && ah >= 3 => {
            let norm = &norm;
            for_rows(&mut rgba, cw * 4, |y, out| {
                let mut out = out.as_chunks_mut::<4>().0.iter_mut();
                crate::demosaic::BayerRow::new(norm, aw, ah, pat, o.demosaic, cy + y).span(cx, cx + cw, |rgb| {
                    if let Some(d) = out.next() {
                        to_output(m, false, rgb, d);
                    }
                });
            });
        }
        Some(pat) => {
            let rgb = crate::demosaic::demosaic_with(&norm, aw, ah, pat, o.demosaic);
            for_rows(&mut rgba, cw * 4, |y, out| {
                let src = (cy + y) * aw + cx;
                for (x, d) in out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                    if let Some(c) = rgb.get((src + x) * 3..(src + x) * 3 + 3) {
                        to_output(m, false, [c[0], c[1], c[2]], d);
                    }
                }
            });
        }
        None => {
            let norm = &norm;
            for_rows(&mut rgba, cw * 4, |y, out| {
                let src = (cy + y) * aw + cx;
                for (x, d) in out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                    let c = if spp == 3 {
                        let Some(c) = norm.get((src + x) * 3..(src + x) * 3 + 3) else { continue };
                        [c[0], c[1], c[2]]
                    } else {
                        let Some(&v) = norm.get(src + x) else { continue };
                        [v, v, v]
                    };
                    to_output(m, mono, c, d);
                }
            });
        }
    }
    Developed { width: cw, height: ch, rgba }
}

/// Steps 5–7 with OpcodeList3: demosaic the whole active area, apply the list to the camera
/// values (white balance taken out again, range 0–1), then crop (within what TrimBounds left)
/// and colour.
#[allow(clippy::too_many_arguments)]
fn with_list3(
    norm: Vec<f32>,
    aw: usize,
    ah: usize,
    spp: usize,
    p: &Params,
    crop: Rect,
    o: &ProcessOptions,
    m: &[[f32; 3]; 3],
    neutral: &V3,
) -> Result<Developed> {
    let mono = p.cfa.is_none() && spp == 1;
    let planes = if mono { 1 } else { 3 };
    let mut rgb = match &p.cfa {
        Some(pat) if pat.is_bayer() && aw >= 3 && ah >= 3 => {
            let mut rgb = vec![0f32; aw * ah * 3];
            let norm = &norm;
            for_rows(&mut rgb, aw * 3, |y, out| {
                let mut out = out.as_chunks_mut::<3>().0.iter_mut();
                crate::demosaic::BayerRow::new(norm, aw, ah, pat, o.demosaic, y).span(0, aw, |c| {
                    if let Some(d) = out.next() {
                        *d = c;
                    }
                });
            });
            rgb
        }
        Some(pat) => crate::demosaic::demosaic_with(&norm, aw, ah, pat, o.demosaic),
        None => norm,
    };
    let n: Vec<f32> = (0..planes).map(|c| if mono { 1.0 } else { neutral.get(c).copied().unwrap_or(1.0) as f32 }).collect();
    for px in rgb.chunks_exact_mut(planes) {
        for (v, k) in px.iter_mut().zip(&n) {
            *v *= k;
        }
    }
    let mut img = opcode::Image { data: &mut rgb, width: aw, height: ah, planes, max: 1.0, mosaic: false, allow_trim: true, origin: (0, 0) };
    opcode::apply(&p.opcodes.list3, &mut img, false)?;
    let (tw, th, (ox, oy)) = (img.width, img.height, img.origin);
    for px in rgb.chunks_exact_mut(planes) {
        for (v, k) in px.iter_mut().zip(&n) {
            *v /= k;
        }
    }
    // the default crop, inside the trimmed picture
    let (cx, cy, cw, ch) = crop;
    let (x0, y0) = (cx.max(ox), cy.max(oy));
    let (x1, y1) = ((cx + cw).min(ox + tw), (cy + ch).min(oy + th));
    if x0 >= x1 || y0 >= y1 {
        return Err(Error::Malformed("TrimBounds outside the default crop"));
    }
    let (w, h) = (x1 - x0, y1 - y0);
    let mut rgba = vec![0f32; w * h * 4];
    let rgb = &rgb;
    for_rows(&mut rgba, w * 4, |y, out| {
        let src = (y0 - oy + y) * tw + (x0 - ox);
        for (x, d) in out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let i = (src + x) * planes;
            let c = if mono {
                let v = rgb.get(i).copied().unwrap_or(0.0);
                [v, v, v]
            } else {
                let Some(c) = rgb.get(i..i + 3) else { continue };
                [c[0], c[1], c[2]]
            };
            to_output(m, mono, c, d);
        }
    });
    Ok(Developed { width: w, height: h, rgba })
}

/// Steps 5–7 at 1/`f` resolution: each output pixel averages every colour over an `f` × `f`
/// block of the crop (a 2 × 2 Bayer block has all three; no demosaic needed).
fn binned(norm: &[f32], aw: usize, spp: usize, cfa: Option<&Pattern>, (cx, cy, cw, ch): Rect, f: usize, m: &[[f32; 3]; 3]) -> Developed {
    let mono = cfa.is_none() && spp == 1;
    let (w, h) = ((cw / f).max(1), (ch / f).max(1));
    let mut rgba = vec![0f32; w * h * 4];
    // the colours of a 2 × 2 Bayer pattern (the only patterns binned), and how many samples of
    // each an f × f block holds (f is even, so every block starts on the same phase)
    let quad: [u8; 4] = cfa.map(|p| [p.color(0, 0), p.color(1, 0), p.color(0, 1), p.color(1, 1)]).unwrap_or([0; 4]);
    let mut n = [0u32; 3];
    for by in 0..f {
        for bx in 0..f {
            match cfa {
                Some(_) => n[usize::from(quad[((cy + by) & 1) * 2 + ((cx + bx) & 1)]).min(2)] += 1,
                None => n = n.map(|v| v + 1),
            }
        }
    }
    let inv = n.map(|v| if v > 0 { 1.0 / v as f32 } else { 0.0 });
    for_rows(&mut rgba, w * 4, |y, out| {
        let mut sums = vec![[0f32; 3]; w];
        for by in 0..f {
            let ay = cy + y * f + by;
            let start = (ay * aw + cx) * spp;
            let Some(row) = norm.get(start..start + w * f * spp) else { continue };
            match cfa {
                Some(_) => {
                    let even = usize::from(quad[(ay & 1) * 2 + (cx & 1)]).min(2);
                    let odd = usize::from(quad[(ay & 1) * 2 + ((cx + 1) & 1)]).min(2);
                    for (s, block) in sums.iter_mut().zip(row.chunks_exact(f)) {
                        for pair in block.as_chunks::<2>().0 {
                            s[even] += pair[0];
                            s[odd] += pair[1];
                        }
                    }
                }
                None if spp == 3 => {
                    for (s, block) in sums.iter_mut().zip(row.chunks_exact(f * 3)) {
                        for px in block.as_chunks::<3>().0 {
                            s[0] += px[0];
                            s[1] += px[1];
                            s[2] += px[2];
                        }
                    }
                }
                None => {
                    for (s, block) in sums.iter_mut().zip(row.chunks_exact(f)) {
                        s[0] += block.iter().sum::<f32>();
                    }
                }
            }
        }
        for (s, d) in sums.iter().zip(out.as_chunks_mut::<4>().0.iter_mut()) {
            let c = if mono { [s[0] * inv[0]; 3] } else { [s[0] * inv[0], s[1] * inv[1], s[2] * inv[2]] };
            to_output(m, mono, c, d);
        }
    });
    Developed { width: w, height: h, rgba }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::IDENTITY;

    fn raw(w: usize, h: usize, spp: usize, data: Vec<u16>) -> RawImage {
        RawImage { width: w, height: h, samples_per_pixel: spp, bits: 12, data }
    }

    #[test]
    fn black_white_linearisation_and_white_balance() {
        // LinearRaw 2 × 1: black 100, white 1100; neutral (0.5, 1, 0.25)
        let r = raw(2, 1, 3, vec![600, 600, 600, 100, 1100, 2000]);
        let p = Params { black: vec![100.0], white: vec![1100.0], ..Default::default() };
        let d = develop(&r, &p, &[0.5, 1.0, 0.25], &IDENTITY).unwrap();
        assert_eq!((d.width, d.height), (2, 1));
        // 0.5 / 0.5 = 1, 0.5 / 1, 0.5 / 0.25 = 2 → clipped to 1
        assert_eq!(&d.rgba[..4], &[1.0, 0.5, 1.0, 1.0]);
        // below black → 0, at white → 1, above white → 1
        assert_eq!(&d.rgba[4..], &[0.0, 1.0, 1.0, 1.0]);
        // a linearisation table doubling the codes
        let t: Vec<u16> = (0..4096u32).map(|v| (v * 2).min(65_535) as u16).collect();
        let p = Params { linearization: Some(t), black: vec![200.0], white: vec![2200.0], ..p };
        let d = develop(&r, &p, &[1.0; 3], &IDENTITY).unwrap();
        assert_eq!(&d.rgba[..3], &[0.5, 0.5, 0.5]);
    }

    #[test]
    fn per_cfa_black_levels_and_deltas() {
        // 2 × 2 CFA, black repeat 2 × 2 with a different black per site, plus row/column deltas
        let pat = Pattern { rows: 2, cols: 2, colors: vec![0, 1, 1, 2] };
        let p = Params {
            black_repeat: (2, 2),
            black: vec![10.0, 20.0, 30.0, 40.0],
            black_delta_h: vec![0.0, 5.0],
            black_delta_v: vec![0.0, 1.0],
            white: vec![1010.0],
            cfa: Some(pat),
            ..Default::default()
        };
        let r = raw(2, 2, 1, vec![510, 525, 531, 546]);
        let n = normalise(&r, &p, (0, 0, 2, 2), &[1.0; 3], 3).unwrap();
        // (v - black) / (white - black) for black 10, 25, 31, 46
        let want = [500.0 / 1000.0, 500.0 / 985.0, 500.0 / 979.0, 500.0 / 964.0];
        for (a, b) in n.iter().zip(want) {
            assert!((a - b).abs() < 1e-6, "{a} vs {b}");
        }
    }

    #[test]
    fn active_area_and_default_crop() {
        // 6 × 4 LinearRaw grey ramp; active area cuts a 1-pixel border, crop takes 2 × 1 at (1, 1)
        let data: Vec<u16> = (0..24u16).flat_map(|i| [i * 10; 3]).collect();
        let r = raw(6, 4, 3, data);
        let p =
            Params { active_area: Some([1, 1, 3, 5]), white: vec![4095.0], crop_origin: Some((1.0, 1.0)), crop_size: Some((2.0, 1.0)), ..Default::default() };
        let d = develop(&r, &p, &[1.0; 3], &IDENTITY).unwrap();
        assert_eq!((d.width, d.height), (2, 1));
        // active (1,1)–(5,3); crop origin (1,1) inside it → raw (2, 2) and (3, 2): samples 14, 15
        assert!((d.rgba[0] - 140.0 / 4095.0).abs() < 1e-6);
        assert!((d.rgba[4] - 150.0 / 4095.0).abs() < 1e-6);
        // hostile active areas and crops
        let bad = Params { active_area: Some([3, 0, 1, 6]), ..Default::default() };
        assert!(develop(&r, &bad, &[1.0; 3], &IDENTITY).is_err());
        let bad = Params { active_area: Some([0, 0, 4, 7]), ..Default::default() };
        assert!(develop(&r, &bad, &[1.0; 3], &IDENTITY).is_err());
        let huge = Params { crop_origin: Some((1e12, -5.0)), crop_size: Some((f64::NAN, 1e9)), white: vec![4095.0], ..Default::default() };
        let d = develop(&r, &huge, &[1.0; 3], &IDENTITY).unwrap();
        assert!(d.width >= 1 && d.width <= 6 && d.height >= 1 && d.height <= 4);
    }

    #[test]
    fn matrix_and_clipping_of_negatives() {
        let r = raw(1, 1, 3, vec![4095, 0, 0]);
        let p = Params { white: vec![4095.0], ..Default::default() };
        let m = [[1.0, 0.0, 0.0], [-0.5, 1.0, 0.0], [0.0, 0.0, 2.0]];
        let d = develop(&r, &p, &[1.0; 3], &m).unwrap();
        assert_eq!(&d.rgba, &[1.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn short_black_level_is_an_error() {
        let r = raw(2, 2, 1, vec![0; 4]);
        let p = Params { black_repeat: (2, 2), black: vec![1.0, 2.0], cfa: Some(Pattern { rows: 2, cols: 2, colors: vec![0, 1, 1, 2] }), ..Default::default() };
        assert!(develop(&r, &p, &[1.0; 3], &IDENTITY).is_err());
    }
}
