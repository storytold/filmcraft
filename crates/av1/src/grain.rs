//! Film grain synthesis (7.18.3), applied to output pictures only (references stay clean).

use crate::Picture;
use crate::header::{FilmGrainParams, SequenceHeader};
use crate::spec_tables::{GAUSSIAN_SEQUENCE, MC_IDENTITY};

#[inline]
fn round2(x: i32, n: u32) -> i32 {
    if n == 0 { x } else { (x + (1 << (n - 1))) >> n }
}

struct Rng(u32);

impl Rng {
    /// get_random_number( bits ) (7.18.3.2)
    fn next(&mut self, bits: u32) -> i32 {
        let r = self.0;
        let bit = (r ^ (r >> 1) ^ (r >> 3) ^ (r >> 12)) & 1;
        let r = (r >> 1) | (bit << 15);
        self.0 = r;
        ((r >> (16 - bits)) & ((1 << bits) - 1)) as i32
    }
}

pub(crate) fn apply(pic: &mut Picture, g: &FilmGrainParams, seq: &SequenceHeader) {
    let _ = seq;
    let bd = pic.bit_depth as u32;
    let mono = pic.mono_chrome;
    let num_planes = if mono { 1 } else { 3 };
    let (sub_x, sub_y) = (pic.subsampling_x as usize, pic.subsampling_y as usize);
    let w = pic.width as usize;
    let h = pic.height as usize;
    let grain_center = 128i32 << (bd - 8);
    let grain_min = -grain_center;
    let grain_max = (256i32 << (bd - 8)) - 1 - grain_center;

    // Generate grain (7.18.3.3).
    let mut rng = Rng(g.grain_seed);
    let shift = 12 - bd + g.grain_scale_shift;
    let mut luma = vec![[0i32; 82]; 73];
    for row in luma.iter_mut() {
        for v in row.iter_mut() {
            let gv = if g.num_y_points > 0 { GAUSSIAN_SEQUENCE[rng.next(11) as usize] as i32 } else { 0 };
            *v = round2(gv, shift);
        }
    }
    let ar_shift = g.ar_coeff_shift_minus_6 + 6;
    let lag = g.ar_coeff_lag as i32;
    for y in 3..73 {
        for x in 3..82 - 3 {
            let mut s = 0;
            let mut pos = 0;
            'outer: for dr in -lag..=0 {
                for dc in -lag..=lag {
                    if dr == 0 && dc == 0 {
                        break 'outer;
                    }
                    let c = g.ar_coeffs_y_plus_128[pos] as i32 - 128;
                    s += luma[(y as i32 + dr) as usize][(x as i32 + dc) as usize] * c;
                    pos += 1;
                }
            }
            luma[y][x] = (luma[y][x] + round2(s, ar_shift)).clamp(grain_min, grain_max);
        }
    }
    let chroma_w = if sub_x != 0 { 44 } else { 82 };
    let chroma_h = if sub_y != 0 { 38 } else { 73 };
    let mut cb = vec![[0i32; 82]; 73];
    let mut cr = vec![[0i32; 82]; 73];
    if !mono {
        for (arr, seed, on) in [(&mut cb, 0xb524u32, g.num_cb_points > 0), (&mut cr, 0x49d8, g.num_cr_points > 0)] {
            rng.0 = g.grain_seed ^ seed;
            for row in arr.iter_mut().take(chroma_h) {
                for v in row.iter_mut().take(chroma_w) {
                    let gv = if on || g.chroma_scaling_from_luma { GAUSSIAN_SEQUENCE[rng.next(11) as usize] as i32 } else { 0 };
                    *v = round2(gv, shift);
                }
            }
        }
        for y in 3..chroma_h {
            for x in 3..chroma_w - 3 {
                let mut s0 = 0;
                let mut s1 = 0;
                let mut pos = 0;
                'outer: for dr in -lag..=0 {
                    for dc in -lag..=lag {
                        let c0 = g.ar_coeffs_cb_plus_128[pos] as i32 - 128;
                        let c1 = g.ar_coeffs_cr_plus_128[pos] as i32 - 128;
                        if dr == 0 && dc == 0 {
                            if g.num_y_points > 0 {
                                let mut l = 0;
                                let lx = ((x - 3) << sub_x) + 3;
                                let ly = ((y - 3) << sub_y) + 3;
                                for i in 0..=sub_y {
                                    for j in 0..=sub_x {
                                        l += luma[ly + i][lx + j];
                                    }
                                }
                                let l = round2(l, (sub_x + sub_y) as u32);
                                s0 += l * c0;
                                s1 += l * c1;
                            }
                            break 'outer;
                        }
                        let (yy, xx) = ((y as i32 + dr) as usize, (x as i32 + dc) as usize);
                        s0 += cb[yy][xx] * c0;
                        s1 += cr[yy][xx] * c1;
                        pos += 1;
                    }
                }
                cb[y][x] = (cb[y][x] + round2(s0, ar_shift)).clamp(grain_min, grain_max);
                cr[y][x] = (cr[y][x] + round2(s1, ar_shift)).clamp(grain_min, grain_max);
            }
        }
    }

    // Scaling lookup (7.18.3.4).
    let mut lut = [[0i32; 256]; 3];
    for (plane, l) in lut.iter_mut().enumerate().take(num_planes) {
        let (n, xs, ys) = if plane == 0 || g.chroma_scaling_from_luma {
            (g.num_y_points, &g.point_y_value, &g.point_y_scaling)
        } else if plane == 1 {
            (g.num_cb_points, &g.point_cb_value, &g.point_cb_scaling)
        } else {
            (g.num_cr_points, &g.point_cr_value, &g.point_cr_scaling)
        };
        if n == 0 {
            continue;
        }
        for v in l.iter_mut().take(xs[0] as usize) {
            *v = ys[0] as i32;
        }
        for i in 0..n - 1 {
            let dy = ys[i + 1] as i32 - ys[i] as i32;
            let dx = xs[i + 1] as i32 - xs[i] as i32;
            // The header parser rejects non-increasing points; never divide by zero regardless.
            if dx <= 0 {
                continue;
            }
            let delta = dy * ((65536 + (dx >> 1)) / dx);
            for x in 0..dx {
                l[xs[i] as usize + x as usize] = ys[i] as i32 + ((x * delta + 32768) >> 16);
            }
        }
        for v in l.iter_mut().skip(xs[n - 1] as usize) {
            *v = ys[n - 1] as i32;
        }
    }
    let scale_lut = |plane: usize, index: i32| -> i32 {
        let sh = bd - 8;
        let x = index >> sh;
        let rem = index - (x << sh);
        if bd == 8 || x == 255 {
            lut[plane][x as usize]
        } else {
            let start = lut[plane][x as usize];
            let end = lut[plane][x as usize + 1];
            start + round2((end - start) * rem, sh)
        }
    };

    // Noise stripes (7.18.3.5).
    let stripe_w = w + 64;
    let n_stripes = (h.div_ceil(2)).div_ceil(16).max(1);
    let mut stripes: Vec<[Vec<i32>; 3]> = (0..n_stripes).map(|_| [vec![0; 34 * stripe_w], vec![0; 34 * stripe_w], vec![0; 34 * stripe_w]]).collect();
    let overlap = g.overlap_flag;
    for (luma_num, stripe) in stripes.iter_mut().enumerate() {
        let mut r = g.grain_seed;
        r ^= (((luma_num * 37 + 178) & 255) << 8) as u32;
        r ^= ((luma_num * 173 + 105) & 255) as u32;
        rng.0 = r;
        let mut x = 0;
        while x < w.div_ceil(2) {
            let rand = rng.next(8) as usize;
            let off_x = rand >> 4;
            let off_y = rand & 15;
            for (plane, s) in stripe.iter_mut().enumerate().take(num_planes) {
                let psx = if plane > 0 { sub_x } else { 0 };
                let psy = if plane > 0 { sub_y } else { 0 };
                let pox = if psx != 0 { 6 + off_x } else { 9 + off_x * 2 };
                let poy = if psy != 0 { 6 + off_y } else { 9 + off_y * 2 };
                let src = match plane {
                    0 => &luma,
                    1 => &cb,
                    _ => &cr,
                };
                for i in 0..(34 >> psy) {
                    for j in 0..(34 >> psx) {
                        let mut gv = src[poy + i][pox + j];
                        if psx == 0 {
                            let idx = i * stripe_w + x * 2 + j;
                            if j < 2 && overlap && x > 0 {
                                let old = s[idx];
                                gv = if j == 0 { old * 27 + gv * 17 } else { old * 17 + gv * 27 };
                                gv = round2(gv, 5).clamp(grain_min, grain_max);
                            }
                            s[idx] = gv;
                        } else {
                            let idx = i * stripe_w + x + j;
                            if j == 0 && overlap && x > 0 {
                                let old = s[idx];
                                gv = round2(old * 23 + gv * 22, 5).clamp(grain_min, grain_max);
                            }
                            s[idx] = gv;
                        }
                    }
                }
            }
            x += 16;
        }
    }
    let noise_at = |plane: usize, y: usize, x: usize| -> i32 {
        let psy = if plane > 0 { sub_y } else { 0 };
        let luma_num = y >> (5 - psy);
        let i = y - (luma_num << (5 - psy));
        let mut gv = stripes[luma_num][plane][i * stripe_w + x];
        if psy == 0 {
            if i < 2 && luma_num > 0 && overlap {
                let old = stripes[luma_num - 1][plane][(i + 32) * stripe_w + x];
                gv = if i == 0 { old * 27 + gv * 17 } else { old * 17 + gv * 27 };
                gv = round2(gv, 5).clamp(grain_min, grain_max);
            }
        } else if i < 1 && luma_num > 0 && overlap {
            let old = stripes[luma_num - 1][plane][(i + 16) * stripe_w + x];
            gv = round2(old * 23 + gv * 22, 5).clamp(grain_min, grain_max);
        }
        gv
    };

    // Blend (7.18.3.5).
    let (min_value, max_luma, max_chroma) = if g.clip_to_restricted_range {
        let ml = 235 << (bd - 8);
        (16 << (bd - 8), ml, if pic.matrix_coefficients as usize == MC_IDENTITY { ml } else { 240 << (bd - 8) })
    } else {
        let m = (256 << (bd - 8)) - 1;
        (0, m, m)
    };
    let pix_max = (1i32 << bd) - 1;
    let scaling_shift = g.grain_scaling_minus_8 + 8;
    if !mono {
        let cw = (w + sub_x) >> sub_x;
        let ch = (h + sub_y) >> sub_y;
        let [py, pu, pv] = &mut pic.planes;
        for y in 0..ch {
            for x in 0..cw {
                let lx = x << sub_x;
                let ly = y << sub_y;
                let lnx = (lx + 1).min(w - 1);
                let avg = if sub_x != 0 { round2(py[ly * w + lx] as i32 + py[ly * w + lnx] as i32, 1) } else { py[ly * w + lx] as i32 };
                for (plane, out, on, luma_mult, mult, offset) in [
                    (1usize, &mut *pu, g.num_cb_points > 0, g.cb_luma_mult, g.cb_mult, g.cb_offset),
                    (2, &mut *pv, g.num_cr_points > 0, g.cr_luma_mult, g.cr_mult, g.cr_offset),
                ] {
                    if !(on || g.chroma_scaling_from_luma) {
                        continue;
                    }
                    let orig = out[y * cw + x] as i32;
                    let merged = if g.chroma_scaling_from_luma {
                        avg
                    } else {
                        let combined = avg * (luma_mult as i32 - 128) + orig * (mult as i32 - 128);
                        ((combined >> 6) + ((offset as i32 - 256) << (bd - 8))).clamp(0, pix_max)
                    };
                    let noise = round2(scale_lut(plane, merged) * noise_at(plane, y, x), scaling_shift);
                    out[y * cw + x] = (orig + noise).clamp(min_value, max_chroma) as u16;
                }
            }
        }
    }
    if g.num_y_points > 0 {
        let py = &mut pic.planes[0];
        for y in 0..h {
            for x in 0..w {
                let orig = py[y * w + x] as i32;
                let noise = round2(scale_lut(0, orig) * noise_at(0, y, x), scaling_shift);
                py[y * w + x] = (orig + noise).clamp(min_value, max_luma) as u16;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Regression: two equal scaling points divided by zero building the scaling lookup.
    #[test]
    fn equal_scaling_points_do_not_panic() {
        let mut g = FilmGrainParams { apply_grain: true, num_y_points: 2, ..Default::default() };
        g.point_y_value[..2].copy_from_slice(&[64, 64]);
        g.point_y_scaling[..2].copy_from_slice(&[20, 40]);
        let (w, h) = (16usize, 16usize);
        let mut pic = Picture {
            width: w as u32,
            height: h as u32,
            bit_depth: 10,
            subsampling_x: 1,
            subsampling_y: 1,
            mono_chrome: false,
            planes: [vec![512; w * h], vec![512; w * h / 4], vec![512; w * h / 4]],
            color_primaries: 1,
            transfer_characteristics: 1,
            matrix_coefficients: 1,
            full_range: false,
            pts: 0,
            draft: false,
        };
        apply(&mut pic, &g, &SequenceHeader::default());
        assert_eq!(pic.planes[0].len(), w * h);
    }
}
