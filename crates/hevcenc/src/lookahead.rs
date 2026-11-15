//! Half-resolution lookahead analysis for rate control, scene-cut detection and adaptive quantisation.
//! Mirrors `h264enc::lookahead` structure but at CTU granularity instead of 16x16 macroblocks.

use crate::dsp::block_var;
use crate::picture::Frame;

/// Half-resolution luma from 2x2 box decimation; dims are ceil halves so odd sizes keep the edge pixel.
pub struct LowRes {
    pub y: Vec<u8>,
    pub w: usize,
    pub h: usize,
}

impl LowRes {
    /// Box-decimate luma by 2x2, reading only `w`x`h` visible samples via `stride`.
    pub fn new(frame: &Frame) -> Self {
        let (fw, fh, stride) = (frame.y.w, frame.y.h, frame.y.stride);
        let (w, h) = (fw.div_ceil(2), fh.div_ceil(2));
        // Empty picture has no rows; avoids `chunks_mut(0)` which would panic.
        if w == 0 || h == 0 {
            return Self { y: Vec::new(), w, h };
        }
        let mut y = vec![0u8; w * h];
        let row = |ly: usize, out: &mut [u8]| {
            let sy = ly * 2;
            for lx in 0..w {
                let sx = lx * 2;
                // Odd edges cover fewer than 4 source samples; average what exists.
                let nx = if sx + 1 < fw { 2 } else { 1 };
                let ny = if sy + 1 < fh { 2 } else { 1 };
                let r0 = sy * stride + sx;
                let mut sum = u32::from(frame.y.data[r0]);
                if nx == 2 {
                    sum += u32::from(frame.y.data[r0 + 1]);
                }
                if ny == 2 {
                    let r1 = r0 + stride;
                    sum += u32::from(frame.y.data[r1]);
                    if nx == 2 {
                        sum += u32::from(frame.y.data[r1 + 1]);
                    }
                }
                let n = (nx * ny) as u32;
                out[lx] = ((sum + n / 2) / n) as u8;
            }
        };
        #[cfg(feature = "threads")]
        {
            use rayon::prelude::*;
            y.par_chunks_mut(w).enumerate().for_each(|(ly, r)| row(ly, r));
        }
        #[cfg(not(feature = "threads"))]
        for (ly, r) in y.chunks_mut(w).enumerate() {
            row(ly, r);
        }
        Self { y, w, h }
    }
}

/// Per-frame complexity estimates summed over 8x8 low-res blocks (one block per 16x16 luma area).
#[derive(Clone, Copy, Debug, Default)]
pub struct Complexity {
    pub intra: u64,
    pub inter: u64,
}

// Fan row costs out to threads when enabled, else fold serially; mirrors h264enc's helper.
fn reduce_blocks<F: Fn(usize) -> (u64, u64) + Sync + Send>(rows: usize, f: F) -> (u64, u64) {
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

/// Intra is mean-normalized variance per 8x8 block; inter is co-located SAD vs `prev` (0 when `None`).
pub fn complexity(cur: &LowRes, prev: Option<&LowRes>) -> Complexity {
    // Incomparable sizes cannot SAD; drop the reference so inter equals intra and reads as a cut.
    let prev = match prev {
        Some(p) if p.w == cur.w && p.h == cur.h => Some(p),
        _ => None,
    };
    let (bw, bh) = (cur.w / 8, cur.h / 8);
    let (intra, inter) = reduce_blocks(bh, |by| {
        let (mut ci, mut cp) = (0u64, 0u64);
        for bx in 0..bw {
            let (x0, y0) = (bx * 8, by * 8);
            // Spatial energy proxy: variance (normalized by 64) keeps intra on the SAD scale.
            let off = y0.saturating_mul(cur.w).saturating_add(x0);
            let intra_c = u64::from(block_var(cur.y.get(off..).unwrap_or(&[]), cur.w, 8, 8)) / 64;
            ci += intra_c;
            if let Some(p) = prev {
                // Temporal cost without motion search; identical inputs give exactly 0.
                let mut sad = 0u64;
                for dy in 0..8 {
                    let rc = (y0 + dy) * cur.w + x0;
                    let rp = (y0 + dy) * p.w + x0;
                    for dx in 0..8 {
                        sad += u64::from(cur.y[rc + dx].abs_diff(p.y[rp + dx]));
                    }
                }
                cp += sad;
            }
        }
        (ci, cp)
    });
    Complexity { intra, inter }
}

/// Per-CTU QP offsets from luma variance: `e = log2(1 + var)`, `offset = clamp(strength * (e - mean), -8, 8)`.
/// Each CTU covers the evenly tiled visible rectangle so edge CTUs shrink; mean subtraction keeps average ~0.
pub fn aq_offsets(frame: &Frame, ctu_w: usize, ctu_h: usize, strength: f32) -> Vec<f32> {
    let n = match ctu_w.checked_mul(ctu_h) {
        Some(n) => n,
        None => return Vec::new(),
    };
    if n == 0 {
        return Vec::new();
    }
    // Strength 0 disables AQ exactly, avoiding any `-0.0` from scaling.
    if strength == 0.0 {
        return vec![0.0; n];
    }
    let (fw, fh, stride) = (frame.y.w, frame.y.h, frame.y.stride);
    let mut e = vec![0f32; n];
    let row = |cy: usize, out: &mut [f32]| {
        let y0 = cy.saturating_mul(fh) / ctu_h;
        let y1 = (cy + 1).saturating_mul(fh) / ctu_h;
        for cx in 0..ctu_w {
            let x0 = cx.saturating_mul(fw) / ctu_w;
            let x1 = (cx + 1).saturating_mul(fw) / ctu_w;
            let (mut s, mut ss, mut count) = (0u64, 0u64, 0u64);
            for y in y0..y1 {
                let r = y * stride + x0;
                for x in x0..x1 {
                    let v = u64::from(frame.y.data[r + (x - x0)]);
                    s += v;
                    ss += v * v;
                    count += 1;
                }
            }
            // Empty regions (zero-area frames) carry zero energy instead of dividing by zero.
            let var = if count > 0 {
                let n = count as f64;
                ((ss as f64 - (s as f64) * (s as f64) / n) / n).max(0.0) as f32
            } else {
                0.0
            };
            // Log compresses the wide variance range so offsets stay modest.
            out[cx] = (1.0 + var).log2();
        }
    };
    #[cfg(feature = "threads")]
    {
        use rayon::prelude::*;
        e.par_chunks_mut(ctu_w).enumerate().for_each(|(cy, r)| row(cy, r));
    }
    #[cfg(not(feature = "threads"))]
    for (cy, r) in e.chunks_mut(ctu_w).enumerate() {
        row(cy, r);
    }
    let mean = e.iter().sum::<f32>() / n as f32;
    e.iter().map(|&v| (strength * (v - mean)).clamp(-8.0, 8.0)).collect()
}

/// True when temporal prediction fails: `cur.inter > 0.75 * cur.intra`, mirroring h264enc's scene-cut rule.
/// `prev_c` is kept so call sites thread (previous, current) pairs; the decision uses the current picture.
pub fn is_scene_cut(prev_c: &Complexity, cur_c: &Complexity) -> bool {
    let _ = prev_c;
    (cur_c.inter as f64) > 0.75 * (cur_c.intra as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::picture::{Frame, Plane};

    fn test_frame(w: usize, h: usize, f: impl Fn(usize, usize) -> u8) -> Frame {
        let stride = w;
        let mut yd = vec![0u8; w.saturating_mul(h)];
        for y in 0..h {
            for x in 0..w {
                yd[y * stride + x] = f(x, y);
            }
        }
        let (cw, ch) = (w / 2, h / 2);
        let cd = vec![128u8; cw.saturating_mul(ch)];
        Frame { y: Plane { data: yd, stride, w, h }, u: Plane { data: cd.clone(), stride: cw, w: cw, h: ch }, v: Plane { data: cd, stride: cw, w: cw, h: ch } }
    }

    #[test]
    fn lowres_ceils_odd_dimensions() {
        let f = test_frame(5, 7, |x, y| (y * 5 + x) as u8);
        let l = LowRes::new(&f);
        assert_eq!(l.w, 3);
        assert_eq!(l.h, 4);
        assert_eq!(l.y.len(), 12);
        let tiny = test_frame(1, 1, |_, _| 42);
        let lt = LowRes::new(&tiny);
        assert_eq!((lt.w, lt.h), (1, 1));
        assert_eq!(lt.y, vec![42u8]);
    }

    #[test]
    fn lowres_averages_2x2_with_edge_clamping() {
        // 3x3 0..8 halves to 2x2: full 2x2 rounds to 2, edges average 2 and 1 samples.
        let f = test_frame(3, 3, |x, y| (y * 3 + x) as u8);
        let l = LowRes::new(&f);
        assert_eq!((l.w, l.h), (2, 2));
        assert_eq!(l.y, vec![2, 4, 7, 8]);
    }

    #[test]
    fn lowres_respects_stride_padding() {
        // Padding bytes after each visible row must not leak into the decimation.
        let (w, h, stride) = (4usize, 4usize, 6usize);
        let mut data = vec![0xFFu8; stride * h];
        for y in 0..h {
            for x in 0..w {
                data[y * stride + x] = (y * 4 + x) as u8;
            }
        }
        let frame = Frame {
            y: Plane { data, stride, w, h },
            u: Plane { data: vec![128u8; 4], stride: 2, w: 2, h: 2 },
            v: Plane { data: vec![128u8; 4], stride: 2, w: 2, h: 2 },
        };
        let l = LowRes::new(&frame);
        assert_eq!((l.w, l.h), (2, 2));
        assert_eq!(l.y, LowRes::new(&test_frame(4, 4, |x, y| (y * 4 + x) as u8)).y);
    }

    #[test]
    fn identical_frames_have_zero_inter() {
        let f = test_frame(64, 64, |x, y| ((x + y) % 256) as u8);
        let a = LowRes::new(&f);
        let b = LowRes::new(&f);
        let c = complexity(&b, Some(&a));
        assert_eq!(c.inter, 0);
        assert!(c.intra > 0);
        assert_eq!(complexity(&a, None).inter, 0);
    }

    #[test]
    fn shifted_frame_has_positive_inter() {
        let f = test_frame(64, 64, |x, y| ((x + y) % 256) as u8);
        let g = test_frame(64, 64, |x, y| ((x + 3 + y) % 256) as u8);
        let a = LowRes::new(&f);
        let b = LowRes::new(&g);
        assert!(complexity(&b, Some(&a)).inter > 0);
    }

    #[test]
    fn scene_cut_fires_on_unrelated_but_not_on_pan() {
        // Grey field with a white square: a 2px pan perturbs only square edges, black is alien.
        let base = test_frame(64, 64, |x, y| if (16..24).contains(&x) && (16..24).contains(&y) { 255 } else { 128 });
        let pan = test_frame(64, 64, |x, y| {
            let xs = x.saturating_sub(2);
            if (16..24).contains(&xs) && (16..24).contains(&y) { 255 } else { 128 }
        });
        let other = test_frame(64, 64, |_, _| 0);
        let base_low = LowRes::new(&base);
        let base_c = complexity(&base_low, None);
        let pan_c = complexity(&LowRes::new(&pan), Some(&base_low));
        let other_c = complexity(&LowRes::new(&other), Some(&base_low));
        assert!(!is_scene_cut(&base_c, &pan_c));
        assert!(is_scene_cut(&base_c, &other_c));
    }

    #[test]
    fn aq_offsets_length_mean_and_strength_zero() {
        let f = test_frame(64, 64, |x, y| ((x * 3 + y * 7) % 256) as u8);
        let aq = aq_offsets(&f, 2, 2, 0.6);
        assert_eq!(aq.len(), 4);
        let mean = aq.iter().sum::<f32>() / aq.len() as f32;
        assert!(mean.abs() < 1e-4, "mean {mean} should be ~0");
        let off = aq_offsets(&f, 2, 2, 0.0);
        assert_eq!(off.len(), 4);
        assert!(off.iter().all(|&v| v == 0.0));
    }
}
