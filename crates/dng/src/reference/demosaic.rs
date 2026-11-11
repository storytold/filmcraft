//! The demosaic of PR #1068, kept unchanged as the reference (tests and benchmarks only).

#[cfg(feature = "threads")]
use rayon::prelude::*;

use crate::demosaic::Pattern;
use crate::options::DemosaicQuality;

const PAD: usize = 2;

/// Mirror index `i` (which may be up to PAD outside) into 0..n.
fn mirror(i: isize, n: usize) -> usize {
    let n = n as isize;
    let mut i = i;
    if i < 0 {
        i = -i;
    }
    if i >= n {
        i = 2 * (n - 1) - i;
    }
    i.clamp(0, n - 1) as usize
}

/// The plane extended by PAD mirrored pixels on each side.
fn padded(plane: &[f32], w: usize, h: usize) -> (Vec<f32>, usize) {
    let pw = w + 2 * PAD;
    let ph = h + 2 * PAD;
    let mut out = vec![0f32; pw * ph];
    for y in 0..ph {
        let sy = mirror(y as isize - PAD as isize, h);
        for x in 0..pw {
            let sx = mirror(x as isize - PAD as isize, w);
            out[y * pw + x] = plane.get(sy * w + sx).copied().unwrap_or(0.0);
        }
    }
    (out, pw)
}

/// Demosaic `plane` (`w` × `h`, one sample per pixel) into interleaved RGB.
pub fn demosaic(plane: &[f32], w: usize, h: usize, pattern: &Pattern) -> Vec<f32> {
    demosaic_with(plane, w, h, pattern, DemosaicQuality::High)
}

/// [`demosaic`] at a chosen quality: [`DemosaicQuality::Fast`] is bilinear for every pattern.
pub fn demosaic_with(plane: &[f32], w: usize, h: usize, pattern: &Pattern, quality: DemosaicQuality) -> Vec<f32> {
    let mut out = vec![0f32; w * h * 3];
    if w == 0 || h == 0 || plane.len() < w * h {
        return out;
    }
    let (p, pw) = padded(plane, w, h);
    let bayer = quality == DemosaicQuality::High && pattern.is_bayer() && w >= 3 && h >= 3;
    let row = |y: usize, o: &mut [f32]| {
        if bayer {
            malvar_row(&p, pw, y, w, pattern, o);
        } else {
            bilinear_row(&p, pw, y, w, h, pattern, o);
        }
    };
    #[cfg(feature = "threads")]
    out.par_chunks_mut(w * 3).enumerate().for_each(|(y, o)| row(y, o));
    #[cfg(not(feature = "threads"))]
    out.chunks_mut(w * 3).enumerate().for_each(|(y, o)| row(y, o));
    out
}

fn malvar_row(p: &[f32], pw: usize, y: usize, w: usize, pat: &Pattern, out: &mut [f32]) {
    let py = y + PAD;
    for x in 0..w {
        let px = x + PAD;
        let at = |dx: isize, dy: isize| p[((py as isize + dy) as usize) * pw + (px as isize + dx) as usize];
        let c = at(0, 0);
        let cross1 = at(-1, 0) + at(1, 0) + at(0, -1) + at(0, 1);
        let cross2 = at(-2, 0) + at(2, 0) + at(0, -2) + at(0, 2);
        let diag = at(-1, -1) + at(1, -1) + at(-1, 1) + at(1, 1);
        let me = pat.color(x, y);
        let rgb = match me {
            1 => {
                let h1 = at(-1, 0) + at(1, 0);
                let h2 = at(-2, 0) + at(2, 0);
                let v1 = at(0, -1) + at(0, 1);
                let v2 = at(0, -2) + at(0, 2);
                // colour along the row and along the column
                let along_row = (5.0 * c + 4.0 * h1 - h2 - diag + 0.5 * v2) / 8.0;
                let along_col = (5.0 * c + 4.0 * v1 - v2 - diag + 0.5 * h2) / 8.0;
                if pat.color(x + 1, y) == 0 { [along_row, c, along_col] } else { [along_col, c, along_row] }
            }
            _ => {
                let g = (4.0 * c + 2.0 * cross1 - cross2) / 8.0;
                let other = (6.0 * c + 2.0 * diag - 1.5 * cross2) / 8.0;
                if me == 0 { [c, g, other] } else { [other, g, c] }
            }
        };
        if let Some(o) = out.get_mut(x * 3..x * 3 + 3) {
            o.copy_from_slice(&rgb);
        }
    }
}

fn bilinear_row(p: &[f32], pw: usize, y: usize, w: usize, h: usize, pat: &Pattern, out: &mut [f32]) {
    for x in 0..w {
        let me = pat.color(x, y);
        let mut rgb = [0f32; 3];
        for (k, v) in rgb.iter_mut().enumerate() {
            if usize::from(me) == k {
                *v = p[(y + PAD) * pw + x + PAD];
                continue;
            }
            for r in 1..=PAD as isize {
                let (mut sum, mut n) = (0f32, 0u32);
                for dy in -r..=r {
                    for dx in -r..=r {
                        // the colour of a mirrored neighbour is the colour where it was mirrored from
                        let sx = mirror(x as isize + dx, w);
                        let sy = mirror(y as isize + dy, h);
                        if usize::from(pat.color(sx, sy)) == k {
                            sum += p[(sy + PAD) * pw + sx + PAD];
                            n += 1;
                        }
                    }
                }
                if n > 0 {
                    *v = sum / n as f32;
                    break;
                }
            }
        }
        if let Some(o) = out.get_mut(x * 3..x * 3 + 3) {
            o.copy_from_slice(&rgb);
        }
    }
}
