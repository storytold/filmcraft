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

const GRAIN_W: usize = 82;
const GRAIN_H: usize = 73;
/// Height of one luma noise stripe in the stripe buffers (32 rows plus 2 overlap rows).
const STRIPE_H: usize = 34;

/// A grain template: 73 rows of 82 samples.
type GrainTemplate = Vec<[i32; GRAIN_W]>;

fn new_template() -> GrainTemplate {
    vec![[0i32; GRAIN_W]; GRAIN_H]
}

/// The allowed range of a grain sample at the picture's bit depth.
#[derive(Clone, Copy)]
struct GrainRange {
    min: i32,
    max: i32,
}

impl GrainRange {
    fn new(bit_depth: u32) -> Self {
        let center = 128i32 << (bit_depth - 8);
        Self { min: -center, max: (256i32 << (bit_depth - 8)) - 1 - center }
    }

    fn clamp(self, v: i32) -> i32 {
        v.clamp(self.min, self.max)
    }

    /// Overlap blend of a full-resolution grain sample with the one it overlaps (weights 27/17 then 17/27).
    fn blend_full(self, old: i32, new: i32, first: bool) -> i32 {
        let mixed = if first { old * 27 + new * 17 } else { old * 17 + new * 27 };
        self.clamp(round2(mixed, 5))
    }

    /// Overlap blend of a subsampled grain sample with the one it overlaps.
    fn blend_subsampled(self, old: i32, new: i32) -> i32 {
        self.clamp(round2(old * 23 + new * 22, 5))
    }
}

/// Chroma subsampling shifts of the picture.
#[derive(Clone, Copy)]
struct Subsampling {
    x: usize,
    y: usize,
}

/// Fills `template` (limited to `cols` x `rows`) with scaled Gaussian noise, or zeros when `enabled` is false.
fn fill_template(template: &mut GrainTemplate, rng: &mut Rng, enabled: bool, (cols, rows): (usize, usize), shift: u32) {
    for row in template.iter_mut().take(rows) {
        for v in row.iter_mut().take(cols) {
            let gv = if enabled { GAUSSIAN_SEQUENCE[rng.next(11) as usize] as i32 } else { 0 };
            *v = round2(gv, shift);
        }
    }
}

/// Weighted sum of the already-generated luma neighbours of `(y, x)` (auto-regressive filter, 7.18.3.3).
fn luma_ar_sum(g: &FilmGrainParams, luma: &GrainTemplate, y: usize, x: usize, lag: i32) -> i32 {
    let mut s = 0;
    let mut pos = 0;
    for dr in -lag..=0 {
        for dc in -lag..=lag {
            if dr == 0 && dc == 0 {
                return s;
            }
            let c = g.ar_coeffs_y_plus_128[pos] as i32 - 128;
            s += luma[(y as i32 + dr) as usize][(x as i32 + dc) as usize] * c;
            pos += 1;
        }
    }
    s
}

/// Luma grain template with the auto-regressive filter applied.
fn generate_luma_grain(g: &FilmGrainParams, rng: &mut Rng, shift: u32, range: GrainRange) -> GrainTemplate {
    let mut luma = new_template();
    fill_template(&mut luma, rng, g.num_y_points > 0, (GRAIN_W, GRAIN_H), shift);
    let ar_shift = g.ar_coeff_shift_minus_6 + 6;
    let lag = g.ar_coeff_lag as i32;
    for y in 3..GRAIN_H {
        for x in 3..GRAIN_W - 3 {
            let s = luma_ar_sum(g, &luma, y, x, lag);
            luma[y][x] = range.clamp(luma[y][x] + round2(s, ar_shift));
        }
    }
    luma
}

/// Chroma grain templates and the geometry they were generated with.
struct ChromaGrain {
    cb: GrainTemplate,
    cr: GrainTemplate,
    cols: usize,
    rows: usize,
}

/// Rounded average of the luma grain co-located with chroma sample `(y, x)`.
fn collocated_luma_grain(luma: &GrainTemplate, y: usize, x: usize, sub: Subsampling) -> i32 {
    let lx = ((x - 3) << sub.x) + 3;
    let ly = ((y - 3) << sub.y) + 3;
    let mut l = 0;
    for i in 0..=sub.y {
        for j in 0..=sub.x {
            l += luma[ly + i][lx + j];
        }
    }
    round2(l, (sub.x + sub.y) as u32)
}

/// Auto-regressive sums `(cb, cr)` for chroma sample `(y, x)`.
fn chroma_ar_sums(g: &FilmGrainParams, luma: &GrainTemplate, chroma: &ChromaGrain, pos: (usize, usize), sub: Subsampling) -> (i32, i32) {
    let (y, x) = pos;
    let lag = g.ar_coeff_lag as i32;
    let mut s0 = 0;
    let mut s1 = 0;
    let mut idx = 0;
    for dr in -lag..=0 {
        for dc in -lag..=lag {
            let c0 = g.ar_coeffs_cb_plus_128[idx] as i32 - 128;
            let c1 = g.ar_coeffs_cr_plus_128[idx] as i32 - 128;
            if dr == 0 && dc == 0 {
                if g.num_y_points > 0 {
                    let l = collocated_luma_grain(luma, y, x, sub);
                    s0 += l * c0;
                    s1 += l * c1;
                }
                return (s0, s1);
            }
            let (yy, xx) = ((y as i32 + dr) as usize, (x as i32 + dc) as usize);
            s0 += chroma.cb[yy][xx] * c0;
            s1 += chroma.cr[yy][xx] * c1;
            idx += 1;
        }
    }
    (s0, s1)
}

/// Cb/Cr grain templates (all zero for monochrome pictures).
fn generate_chroma_grain(g: &FilmGrainParams, luma: &GrainTemplate, rng: &mut Rng, sub: Subsampling, shift: u32, range: GrainRange) -> ChromaGrain {
    let mut chroma =
        ChromaGrain { cb: new_template(), cr: new_template(), cols: if sub.x != 0 { 44 } else { GRAIN_W }, rows: if sub.y != 0 { 38 } else { GRAIN_H } };
    let dims = (chroma.cols, chroma.rows);
    rng.0 = g.grain_seed ^ 0xb524;
    fill_template(&mut chroma.cb, rng, g.num_cb_points > 0 || g.chroma_scaling_from_luma, dims, shift);
    rng.0 = g.grain_seed ^ 0x49d8;
    fill_template(&mut chroma.cr, rng, g.num_cr_points > 0 || g.chroma_scaling_from_luma, dims, shift);
    let ar_shift = g.ar_coeff_shift_minus_6 + 6;
    for y in 3..chroma.rows {
        for x in 3..chroma.cols - 3 {
            let (s0, s1) = chroma_ar_sums(g, luma, &chroma, (y, x), sub);
            chroma.cb[y][x] = range.clamp(chroma.cb[y][x] + round2(s0, ar_shift));
            chroma.cr[y][x] = range.clamp(chroma.cr[y][x] + round2(s1, ar_shift));
        }
    }
    chroma
}

/// Piecewise-linear scaling function of one plane, expanded to 256 entries (7.18.3.4).
fn fill_plane_lut(l: &mut [i32; 256], n: usize, xs: &[u8; 16], ys: &[u8; 16]) {
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

/// Scaling lookup tables for all planes, interpolated for high bit depths.
struct ScalingLut {
    lut: [[i32; 256]; 3],
    bit_depth: u32,
}

impl ScalingLut {
    fn new(g: &FilmGrainParams, num_planes: usize, bit_depth: u32) -> Self {
        let mut lut = [[0i32; 256]; 3];
        for (plane, l) in lut.iter_mut().enumerate().take(num_planes) {
            let (n, xs, ys) = if plane == 0 || g.chroma_scaling_from_luma {
                (g.num_y_points, &g.point_y_value, &g.point_y_scaling)
            } else if plane == 1 {
                (g.num_cb_points, &g.point_cb_value, &g.point_cb_scaling)
            } else {
                (g.num_cr_points, &g.point_cr_value, &g.point_cr_scaling)
            };
            if n != 0 {
                fill_plane_lut(l, n, xs, ys);
            }
        }
        Self { lut, bit_depth }
    }

    fn scale(&self, plane: usize, index: i32) -> i32 {
        let sh = self.bit_depth - 8;
        let x = index >> sh;
        let rem = index - (x << sh);
        if self.bit_depth == 8 || x == 255 {
            self.lut[plane][x as usize]
        } else {
            let start = self.lut[plane][x as usize];
            let end = self.lut[plane][x as usize + 1];
            start + round2((end - start) * rem, sh)
        }
    }
}

/// Everything the noise stripe synthesis needs besides the templates.
struct StripeParams {
    width: usize,
    height: usize,
    num_planes: usize,
    sub: Subsampling,
    range: GrainRange,
}

/// Noise stripes of the whole picture (7.18.3.5).
struct NoiseStripes {
    stripes: Vec<[Vec<i32>; 3]>,
    stripe_w: usize,
    sub_y: usize,
    overlap: bool,
    range: GrainRange,
}

/// Whether neighbouring noise blocks are blended, and the sample range of the result.
#[derive(Clone, Copy)]
struct OverlapBlend {
    overlap: bool,
    range: GrainRange,
}

/// Copies one random 34x34 block of grain into plane `s` of a stripe at horizontal position `x`.
fn copy_stripe_block(s: &mut [i32], src: &GrainTemplate, origin: (usize, usize), (psx, psy): (usize, usize), x: usize, stripe_w: usize, blend: OverlapBlend) {
    let (pox, poy) = origin;
    let OverlapBlend { overlap, range } = blend;
    for i in 0..(STRIPE_H >> psy) {
        for j in 0..(STRIPE_H >> psx) {
            let mut gv = src[poy + i][pox + j];
            if psx == 0 {
                let idx = i * stripe_w + x * 2 + j;
                if j < 2 && overlap && x > 0 {
                    gv = range.blend_full(s[idx], gv, j == 0);
                }
                s[idx] = gv;
            } else {
                let idx = i * stripe_w + x + j;
                if j == 0 && overlap && x > 0 {
                    gv = range.blend_subsampled(s[idx], gv);
                }
                s[idx] = gv;
            }
        }
    }
}

impl NoiseStripes {
    fn generate(g: &FilmGrainParams, rng: &mut Rng, luma: &GrainTemplate, chroma: &ChromaGrain, p: &StripeParams) -> Self {
        let stripe_w = p.width + 64;
        let n_stripes = (p.height.div_ceil(2)).div_ceil(16).max(1);
        let mut stripes: Vec<[Vec<i32>; 3]> =
            (0..n_stripes).map(|_| [vec![0; STRIPE_H * stripe_w], vec![0; STRIPE_H * stripe_w], vec![0; STRIPE_H * stripe_w]]).collect();
        for (luma_num, stripe) in stripes.iter_mut().enumerate() {
            let mut r = g.grain_seed;
            r ^= (((luma_num * 37 + 178) & 255) << 8) as u32;
            r ^= ((luma_num * 173 + 105) & 255) as u32;
            rng.0 = r;
            for x in (0..p.width.div_ceil(2)).step_by(16) {
                let rand = rng.next(8) as usize;
                let off_x = rand >> 4;
                let off_y = rand & 15;
                for (plane, s) in stripe.iter_mut().enumerate().take(p.num_planes) {
                    let psx = if plane > 0 { p.sub.x } else { 0 };
                    let psy = if plane > 0 { p.sub.y } else { 0 };
                    let pox = if psx != 0 { 6 + off_x } else { 9 + off_x * 2 };
                    let poy = if psy != 0 { 6 + off_y } else { 9 + off_y * 2 };
                    let src = match plane {
                        0 => luma,
                        1 => &chroma.cb,
                        _ => &chroma.cr,
                    };
                    copy_stripe_block(s, src, (pox, poy), (psx, psy), x, stripe_w, OverlapBlend { overlap: g.overlap_flag, range: p.range });
                }
            }
        }
        Self { stripes, stripe_w, sub_y: p.sub.y, overlap: g.overlap_flag, range: p.range }
    }

    /// Grain sample for `(y, x)` of `plane`, blended with the previous stripe where they overlap.
    fn noise_at(&self, plane: usize, y: usize, x: usize) -> i32 {
        let psy = if plane > 0 { self.sub_y } else { 0 };
        let luma_num = y >> (5 - psy);
        let i = y - (luma_num << (5 - psy));
        let mut gv = self.stripes[luma_num][plane][i * self.stripe_w + x];
        if psy == 0 {
            if i < 2 && luma_num > 0 && self.overlap {
                let old = self.stripes[luma_num - 1][plane][(i + 32) * self.stripe_w + x];
                gv = self.range.blend_full(old, gv, i == 0);
            }
        } else if i < 1 && luma_num > 0 && self.overlap {
            let old = self.stripes[luma_num - 1][plane][(i + 16) * self.stripe_w + x];
            gv = self.range.blend_subsampled(old, gv);
        }
        gv
    }
}

/// Sample limits and scaling shift used when adding grain to the picture.
struct BlendParams {
    bit_depth: u32,
    min_value: i32,
    max_luma: i32,
    max_chroma: i32,
    scaling_shift: u32,
}

impl BlendParams {
    fn new(g: &FilmGrainParams, pic: &Picture) -> Self {
        let bd = pic.bit_depth as u32;
        let (min_value, max_luma, max_chroma) = if g.clip_to_restricted_range {
            let ml = 235 << (bd - 8);
            (16 << (bd - 8), ml, if pic.matrix_coefficients as usize == MC_IDENTITY { ml } else { 240 << (bd - 8) })
        } else {
            let m = (256 << (bd - 8)) - 1;
            (0, m, m)
        };
        Self { bit_depth: bd, min_value, max_luma, max_chroma, scaling_shift: g.grain_scaling_minus_8 + 8 }
    }
}

/// Per-chroma-plane film grain parameters used by the blend.
struct ChromaBlendPlane {
    plane: usize,
    enabled: bool,
    luma_mult: u32,
    mult: u32,
    offset: u32,
}

/// Adds scaled grain to the chroma planes (7.18.3.5).
fn blend_chroma(pic: &mut Picture, g: &FilmGrainParams, lut: &ScalingLut, noise: &NoiseStripes, b: &BlendParams) {
    let (w, h) = (pic.width as usize, pic.height as usize);
    let (sub_x, sub_y) = (pic.subsampling_x as usize, pic.subsampling_y as usize);
    let pix_max = (1i32 << b.bit_depth) - 1;
    let cw = (w + sub_x) >> sub_x;
    let ch = (h + sub_y) >> sub_y;
    let [py, pu, pv] = &mut pic.planes;
    let planes = [
        ChromaBlendPlane { plane: 1, enabled: g.num_cb_points > 0, luma_mult: g.cb_luma_mult, mult: g.cb_mult, offset: g.cb_offset },
        ChromaBlendPlane { plane: 2, enabled: g.num_cr_points > 0, luma_mult: g.cr_luma_mult, mult: g.cr_mult, offset: g.cr_offset },
    ];
    for y in 0..ch {
        for x in 0..cw {
            let lx = x << sub_x;
            let ly = y << sub_y;
            let lnx = (lx + 1).min(w - 1);
            let avg = if sub_x != 0 { round2(py[ly * w + lx] as i32 + py[ly * w + lnx] as i32, 1) } else { py[ly * w + lx] as i32 };
            for (cp, out) in planes.iter().zip([&mut *pu, &mut *pv]) {
                if !(cp.enabled || g.chroma_scaling_from_luma) {
                    continue;
                }
                let orig = out[y * cw + x] as i32;
                let merged = if g.chroma_scaling_from_luma {
                    avg
                } else {
                    let combined = avg * (cp.luma_mult as i32 - 128) + orig * (cp.mult as i32 - 128);
                    ((combined >> 6) + ((cp.offset as i32 - 256) << (b.bit_depth - 8))).clamp(0, pix_max)
                };
                let n = round2(lut.scale(cp.plane, merged) * noise.noise_at(cp.plane, y, x), b.scaling_shift);
                out[y * cw + x] = (orig + n).clamp(b.min_value, b.max_chroma) as u16;
            }
        }
    }
}

/// Adds scaled grain to the luma plane (7.18.3.5).
fn blend_luma(pic: &mut Picture, lut: &ScalingLut, noise: &NoiseStripes, b: &BlendParams) {
    let (w, h) = (pic.width as usize, pic.height as usize);
    let py = &mut pic.planes[0];
    for y in 0..h {
        for x in 0..w {
            let orig = py[y * w + x] as i32;
            let n = round2(lut.scale(0, orig) * noise.noise_at(0, y, x), b.scaling_shift);
            py[y * w + x] = (orig + n).clamp(b.min_value, b.max_luma) as u16;
        }
    }
}

pub(crate) fn apply(pic: &mut Picture, g: &FilmGrainParams, seq: &SequenceHeader) {
    let _ = seq;
    let bd = pic.bit_depth as u32;
    let mono = pic.mono_chrome;
    let num_planes = if mono { 1 } else { 3 };
    let sub = Subsampling { x: pic.subsampling_x as usize, y: pic.subsampling_y as usize };
    let range = GrainRange::new(bd);

    // Generate grain (7.18.3.3).
    let mut rng = Rng(g.grain_seed);
    let shift = 12 - bd + g.grain_scale_shift;
    let luma = generate_luma_grain(g, &mut rng, shift, range);
    let chroma = if mono {
        ChromaGrain { cb: new_template(), cr: new_template(), cols: 0, rows: 0 }
    } else {
        generate_chroma_grain(g, &luma, &mut rng, sub, shift, range)
    };

    let lut = ScalingLut::new(g, num_planes, bd);
    let stripe_params = StripeParams { width: pic.width as usize, height: pic.height as usize, num_planes, sub, range };
    let noise = NoiseStripes::generate(g, &mut rng, &luma, &chroma, &stripe_params);

    let blend = BlendParams::new(g, pic);
    if !mono {
        blend_chroma(pic, g, &lut, &noise, &blend);
    }
    if g.num_y_points > 0 {
        blend_luma(pic, &lut, &noise, &blend);
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
