//! The processing of PR #1068 (normalise, demosaic, crop, matrix), kept unchanged as the reference
//! the faster pipeline is compared against (tests and benchmarks only).

#[cfg(feature = "threads")]
use rayon::prelude::*;

use super::demosaic::demosaic_with;
use crate::color::{M3, V3};
use crate::options::ProcessOptions;
use crate::process::{Developed, Params, active_rect, crop_rect};

type Rect = (usize, usize, usize, usize);
use crate::raw::RawImage;
use crate::{Error, Result};

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

/// Develop `raw` (steps 1–7). `neutral` and `matrix` come from [`crate::color::Calibration::solve`].
pub fn develop(raw: &RawImage, p: &Params, neutral: &V3, matrix: &M3) -> Result<Developed> {
    develop_with(raw, p, neutral, matrix, &ProcessOptions::default())
}

/// [`develop`] with raw settings. White balance and exposure are already in `neutral` and
/// `matrix` ([`crate::Dng::develop_with`] solves them); this applies the demosaic quality.
pub fn develop_with(raw: &RawImage, p: &Params, neutral: &V3, matrix: &M3, o: &ProcessOptions) -> Result<Developed> {
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
        (Some(pat), _) => demosaic_with(&norm, aw, ah, pat, o.demosaic),
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
