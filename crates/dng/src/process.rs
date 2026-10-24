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
use crate::demosaic::{Pattern, demosaic};
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

/// Normalised, white-balanced samples of the active area (steps 1–4), one plane per sample.
fn normalise(raw: &RawImage, p: &Params, (ax, ay, aw, ah): Rect, neutral: &V3, planes: usize) -> Result<Vec<f32>> {
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
    let max_code = ((1u32 << raw.bits) - 1) as f64;
    let white: Vec<f64> = (0..spp).map(|s| p.white.get(s).or(p.white.first()).copied().unwrap_or(max_code)).collect();
    let lin = p.linearization.as_deref().filter(|t| !t.is_empty());
    let inv_neutral: Vec<f32> = (0..planes).map(|c| (1.0 / neutral.get(c).copied().unwrap_or(1.0)) as f32).collect();
    let cfa = p.cfa.as_ref();
    let mut out = vec![0f32; aw * ah * spp];
    let row = |y: usize, o: &mut [f32]| {
        let dv = p.black_delta_v.get(y).copied().unwrap_or(0.0);
        let src_row = (ay + y) * raw.width * spp;
        for x in 0..aw {
            let dh = p.black_delta_h.get(x).copied().unwrap_or(0.0);
            for s in 0..spp {
                let code = raw.data.get(src_row + (ax + x) * spp + s).copied().unwrap_or(0);
                let v = match lin {
                    Some(t) => f64::from(t.get(usize::from(code)).or(t.last()).copied().unwrap_or(0)),
                    None => f64::from(code),
                };
                let b = black[((y % br) * bc + x % bc) * spp + s] + dh + dv;
                let range = white[s] - b;
                let n = if range > 0.0 { ((v - b) / range).clamp(0.0, 1.0) } else { 0.0 };
                let plane = match cfa {
                    Some(c) => usize::from(c.color(x, y)),
                    None => s,
                };
                let wb = inv_neutral.get(plane).copied().unwrap_or(1.0);
                if let Some(o) = o.get_mut(x * spp + s) {
                    *o = (n as f32 * wb).min(1.0);
                }
            }
        }
    };
    #[cfg(feature = "threads")]
    out.par_chunks_mut((aw * spp).max(1)).enumerate().for_each(|(y, o)| row(y, o));
    #[cfg(not(feature = "threads"))]
    out.chunks_mut((aw * spp).max(1)).enumerate().for_each(|(y, o)| row(y, o));
    Ok(out)
}

/// A developed picture: premultiplied linear Rec. 709 RGBA floats (alpha 1).
pub struct Developed {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<f32>,
}

/// Develop `raw` (steps 1–7). `neutral` and `matrix` come from [`crate::color::Calibration::solve`].
pub fn develop(raw: &RawImage, p: &Params, neutral: &V3, matrix: &M3) -> Result<Developed> {
    let spp = raw.samples_per_pixel;
    if p.cfa.is_some() && spp != 1 {
        return Err(Error::Malformed("a CFA image with more than one sample per pixel"));
    }
    if p.cfa.is_none() && !matches!(spp, 1 | 3) {
        return Err(Error::Unsupported(format!("LinearRaw with {spp} samples per pixel")));
    }
    let active = active_rect(p, raw.width, raw.height)?;
    let (_, _, aw, ah) = active;
    let planes = if p.cfa.is_some() { 3 } else { spp };
    let norm = normalise(raw, p, active, neutral, planes)?;
    // three samples per pixel
    let rgb = match (&p.cfa, spp) {
        (Some(pat), _) => demosaic(&norm, aw, ah, pat),
        (None, 3) => norm,
        _ => norm.iter().flat_map(|&v| [v, v, v]).collect(),
    };
    let (cx, cy, cw, ch) = crop_rect(p, aw, ah);
    let m: [[f32; 3]; 3] = matrix.map(|r| r.map(|v| v as f32));
    let mono = p.cfa.is_none() && spp == 1;
    let mut rgba = vec![0f32; cw * ch * 4];
    let row = |y: usize, o: &mut [f32]| {
        let src = (cy + y) * aw + cx;
        for x in 0..cw {
            let Some(c) = rgb.get((src + x) * 3..(src + x) * 3 + 3) else { continue };
            let px = if mono {
                // grey: the matrix's luminance gain only
                let g = c[0] * (m[1][0] + m[1][1] + m[1][2]);
                [g, g, g]
            } else {
                [0, 1, 2].map(|i| m[i][0] * c[0] + m[i][1] * c[1] + m[i][2] * c[2])
            };
            if let Some(d) = o.get_mut(x * 4..x * 4 + 4) {
                d.copy_from_slice(&[px[0].max(0.0), px[1].max(0.0), px[2].max(0.0), 1.0]);
            }
        }
    };
    #[cfg(feature = "threads")]
    rgba.par_chunks_mut(cw * 4).enumerate().for_each(|(y, o)| row(y, o));
    #[cfg(not(feature = "threads"))]
    rgba.chunks_mut(cw * 4).enumerate().for_each(|(y, o)| row(y, o));
    Ok(Developed { width: cw, height: ch, rgba })
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
