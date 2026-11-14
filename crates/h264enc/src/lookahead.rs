//! Cheap half-resolution analysis used for rate control, scene-cut detection and adaptive quantisation.

use crate::dsp::{block_var, sad, satd};
use crate::picture::Frame;
use crate::pixel::Pixel;

/// Half-resolution 8-bit luma (2x2 box filtered) of the macroblock-aligned picture. Higher bit depths are
/// scaled down to 8 bits, so rate control and scene-cut decisions see the same numbers at every depth.
pub struct LowRes {
    pub w: usize,
    pub h: usize,
    pub data: Vec<u8>,
}

impl LowRes {
    pub fn new<P: Pixel>(f: &Frame<P>, shift: u32) -> Self {
        let (w, h) = (f.y.w / 2, f.y.h / 2);
        let mut data = vec![0u8; w * h];
        let row = |y: usize, out: &mut [u8]| {
            let r0 = f.y.idx(0, (2 * y) as isize);
            let r1 = f.y.idx(0, (2 * y + 1) as isize);
            for x in 0..w {
                let s = f.y.data[r0 + 2 * x].get() + f.y.data[r0 + 2 * x + 1].get() + f.y.data[r1 + 2 * x].get() + f.y.data[r1 + 2 * x + 1].get();
                out[x] = ((s + (2 << shift)) >> (2 + shift)).min(255) as u8;
            }
        };
        #[cfg(feature = "threads")]
        {
            use rayon::prelude::*;
            data.par_chunks_mut(w).enumerate().for_each(|(y, r)| row(y, r));
        }
        #[cfg(not(feature = "threads"))]
        for (y, r) in data.chunks_mut(w).enumerate() {
            row(y, r);
        }
        LowRes { w, h, data }
    }
}

/// Per-frame complexity estimates (sum over 8x8 low-res blocks, i.e. per macroblock).
#[derive(Clone, Copy, Debug, Default)]
pub struct Complexity {
    pub intra: u64,
    /// Inter cost against the previous frame (min with intra per block); equals `intra` when there is no previous frame.
    pub inter: u64,
}

fn block_rows<F: Fn(usize) -> (u64, u64) + Sync + Send>(rows: usize, f: F) -> (u64, u64) {
    #[cfg(feature = "threads")]
    {
        use rayon::prelude::*;
        (0..rows).into_par_iter().map(f).reduce(|| (0, 0), |a, b| (a.0 + b.0, a.1 + b.1))
    }
    #[cfg(not(feature = "threads"))]
    {
        (0..rows).map(f).fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1))
    }
}

pub fn complexity(cur: &LowRes, prev: Option<&LowRes>) -> Complexity {
    let bw = cur.w / 8;
    let bh = cur.h / 8;
    let (intra, inter) = block_rows(bh, |by| {
        let mut ci = 0u64;
        let mut cp = 0u64;
        let mut blk = [0u8; 64];
        for bx in 0..bw {
            let (x0, y0) = (bx * 8, by * 8);
            let base = y0 * cur.w + x0;
            // intra: best of DC / H / V predictions from source neighbours
            let mut best = u32::MAX;
            for mode in 0..3 {
                if (mode == 1 && x0 == 0) || (mode == 2 && y0 == 0) {
                    continue;
                }
                for y in 0..8 {
                    for x in 0..8 {
                        blk[y * 8 + x] = match mode {
                            0 => {
                                if x0 > 0 && y0 > 0 {
                                    ((cur.data[(y0 - 1) * cur.w + x0 + x] as u32 + cur.data[(y0 + y) * cur.w + x0 - 1] as u32) / 2) as u8
                                } else {
                                    128
                                }
                            }
                            1 => cur.data[(y0 + y) * cur.w + x0 - 1],
                            _ => cur.data[(y0 - 1) * cur.w + x0 + x],
                        };
                    }
                }
                best = best.min(satd(&cur.data[base..], cur.w, &blk, 8, 8, 8));
            }
            let intra_c = best as u64 + 16;
            ci += intra_c;
            if let Some(p) = prev {
                // small diamond search on SAD, then SATD of the best
                let mut bmv = (0i32, 0i32);
                let cost_at = |mx: i32, my: i32| -> u32 {
                    let x = x0 as i32 + mx;
                    let y = y0 as i32 + my;
                    if x < 0 || y < 0 || x + 8 > p.w as i32 || y + 8 > p.h as i32 {
                        return u32::MAX;
                    }
                    sad(&cur.data[base..], cur.w, &p.data[y as usize * p.w + x as usize..], p.w, 8, 8)
                };
                let mut bc = cost_at(0, 0);
                for _ in 0..8 {
                    let c = bmv;
                    for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1), (-2, 0), (2, 0), (0, -2), (0, 2)] {
                        let k = cost_at(c.0 + dx, c.1 + dy);
                        if k < bc {
                            bc = k;
                            bmv = (c.0 + dx, c.1 + dy);
                        }
                    }
                    if bmv == c {
                        break;
                    }
                }
                let x = (x0 as i32 + bmv.0) as usize;
                let y = (y0 as i32 + bmv.1) as usize;
                let inter_c = satd(&cur.data[base..], cur.w, &p.data[y * p.w + x..], p.w, 8, 8) as u64 + 8;
                cp += inter_c.min(intra_c);
            } else {
                cp += intra_c;
            }
        }
        (ci, cp)
    });
    Complexity { intra, inter }
}

/// Variance-based adaptive quantisation offsets per macroblock (QP units, mean ~0). Variances are measured
/// in 8-bit units (divided by 4^`shift`).
pub fn aq_offsets<P: Pixel>(f: &Frame<P>, mbw: usize, mbh: usize, strength: f32, shift: u32) -> Vec<f32> {
    let mut e = vec![0f32; mbw * mbh];
    let norm = (1u32 << (2 * shift)) as f32;
    let row = |my: usize, e: &mut [f32]| {
        for mx in 0..mbw {
            let i = f.y.idx((mx * 16) as isize, (my * 16) as isize);
            let vy = block_var(&f.y.data[i..], f.y.stride, 16, 16) as f32 / 256.0;
            let iu = f.u.idx((mx * 8) as isize, (my * 8) as isize);
            let vu = block_var(&f.u.data[iu..], f.u.stride, 8, 8) as f32 / 64.0;
            let iv = f.v.idx((mx * 8) as isize, (my * 8) as isize);
            let vv = block_var(&f.v.data[iv..], f.v.stride, 8, 8) as f32 / 64.0;
            e[mx] = ((vy + 0.5 * (vu + vv)) / norm + 1.0).log2();
        }
    };
    #[cfg(feature = "threads")]
    {
        use rayon::prelude::*;
        e.par_chunks_mut(mbw).enumerate().for_each(|(my, r)| row(my, r));
    }
    #[cfg(not(feature = "threads"))]
    for (my, r) in e.chunks_mut(mbw).enumerate() {
        row(my, r);
    }
    let mean = e.iter().sum::<f32>() / e.len() as f32;
    e.iter().map(|&v| (strength * (v - mean)).clamp(-8.0, 8.0)).collect()
}
