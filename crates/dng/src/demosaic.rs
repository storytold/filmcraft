//! Demosaicing: one colour sample per pixel (a colour filter array) to three.
//!
//! - A 2 × 2 Bayer pattern (one red, two green, one blue) uses the gradient-corrected linear
//!   interpolation of H. S. Malvar, L.-W. He and R. Cutler, "High-quality linear interpolation for
//!   demosaicing of Bayer-patterned color images", IEEE ICASSP 2004: bilinear interpolation plus a
//!   Laplacian correction from the pixel's own colour, as 5 × 5 filters.
//! - Any other pattern (up to 8 × 8) uses bilinear interpolation: the mean of the nearest samples
//!   of each missing colour in the 3 × 3 neighbourhood, else the 5 × 5 one.
//!
//! The picture is extended by two pixels on every side by mirroring (without repeating the edge),
//! which keeps a 2 × 2 pattern's phase.

#[cfg(feature = "threads")]
use rayon::prelude::*;

use crate::options::DemosaicQuality;

/// A CFA pattern: `rows` × `cols` colour indices (0 red, 1 green, 2 blue), origin at the top left
/// of the image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pattern {
    pub rows: usize,
    pub cols: usize,
    pub colors: Vec<u8>,
}

impl Pattern {
    pub fn color(&self, x: usize, y: usize) -> u8 {
        self.colors.get((y % self.rows) * self.cols + x % self.cols).copied().unwrap_or(1)
    }

    /// Whether this is a 2 × 2 Bayer pattern.
    pub fn is_bayer(&self) -> bool {
        if (self.rows, self.cols) != (2, 2) {
            return false;
        }
        let [a, b, c, d] = self.colors[..] else { return false };
        let count = |k: u8| [a, b, c, d].iter().filter(|&&v| v == k).count();
        // the two greens sit on a diagonal
        count(0) == 1 && count(1) == 2 && count(2) == 1 && ((a == 1 && d == 1) || (b == 1 && c == 1))
    }
}

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

/// One output row of a 2 × 2 Bayer demosaic read straight from the plane (no padded copy): the
/// five rows a 5 × 5 filter needs, mirrored at the top and bottom, and mirrored columns only near
/// the left and right edges. Gives exactly the values of [`demosaic_with`] (same filters, same
/// order of operations).
pub(crate) struct BayerRow<'a> {
    rows: [&'a [f32]; 5],
    w: usize,
    /// Colour of even and odd columns of this row.
    colors: [u8; 2],
    high: bool,
}

impl<'a> BayerRow<'a> {
    /// Row `y` of the `w` × `h` plane (`w`, `h` ≥ 3 and a 2 × 2 Bayer `pat`).
    pub(crate) fn new(plane: &'a [f32], w: usize, h: usize, pat: &Pattern, quality: DemosaicQuality, y: usize) -> BayerRow<'a> {
        let row = |dy: isize| {
            let sy = mirror(y as isize + dy, h);
            plane.get(sy * w..sy * w + w).unwrap_or(&[])
        };
        BayerRow { rows: [row(-2), row(-1), row(0), row(1), row(2)], w, colors: [pat.color(0, y), pat.color(1, y)], high: quality == DemosaicQuality::High }
    }

    /// The RGB of columns `x0..x1`, in order, passed to `f`: the interior through 5 × 5 windows
    /// sliding along the five rows, the two columns at either edge through [`BayerRow::pixel`].
    #[inline]
    pub(crate) fn span(&mut self, x0: usize, x1: usize, mut f: impl FnMut([f32; 3])) {
        let (lo, hi) = (2.min(self.w), self.w.saturating_sub(2).max(2));
        let mut rgb = [0f32; 3];
        let mut x = x0;
        while x < x1.min(lo) {
            self.pixel(x, &mut rgb);
            f(rgb);
            x += 1;
        }
        let end = x1.min(hi);
        if x < end {
            let [r0, r1, r2, r3, r4] = self.rows;
            let win = |r: &'a [f32]| r.get(x - 2..end + 2).unwrap_or(&[]).windows(5);
            let rows = win(r0).zip(win(r1)).zip(win(r2)).zip(win(r3)).zip(win(r4));
            let (high, colors) = (self.high, self.colors);
            for ((((a, b), c), d), e) in rows {
                let at = |dx: isize, dy: isize| {
                    let r = match dy {
                        -2 => a,
                        -1 => b,
                        0 => c,
                        1 => d,
                        _ => e,
                    };
                    r.get((dx + 2) as usize).copied().unwrap_or(0.0)
                };
                let me = colors[x & 1];
                let right_is_red = colors[(x + 1) & 1] == 0;
                f(if high { malvar_px(at, me, right_is_red) } else { bilinear_px(at, me, right_is_red) });
                x += 1;
            }
        }
        while x < x1 {
            self.pixel(x, &mut rgb);
            f(rgb);
            x += 1;
        }
    }

    /// The RGB of column `x`.
    #[inline]
    pub(crate) fn pixel(&mut self, x: usize, out: &mut [f32; 3]) {
        let me = self.colors[x & 1];
        let right_is_red = self.colors[(x + 1) & 1] == 0;
        let rows = &self.rows;
        if x >= 2 && x + 2 < self.w {
            // the 5 × 5 window, bounds checked once per row
            let win = rows.map(|r| r.get(x - 2..x + 3).and_then(|s| <[f32; 5]>::try_from(s).ok()).unwrap_or([0.0; 5]));
            let at = |dx: isize, dy: isize| win[(dy + 2) as usize][(dx + 2) as usize];
            *out = if self.high { malvar_px(at, me, right_is_red) } else { bilinear_px(at, me, right_is_red) };
        } else {
            let w = self.w;
            let at = |dx: isize, dy: isize| rows[(dy + 2) as usize].get(mirror(x as isize + dx, w)).copied().unwrap_or(0.0);
            *out = if self.high { malvar_px(at, me, right_is_red) } else { bilinear_px(at, me, right_is_red) };
        }
    }
}

/// Malvar–He–Cutler at one pixel of colour `me` (the expressions of [`malvar_row`]).
#[inline(always)]
fn malvar_px(at: impl Fn(isize, isize) -> f32, me: u8, right_is_red: bool) -> [f32; 3] {
    let c = at(0, 0);
    let diag = at(-1, -1) + at(1, -1) + at(-1, 1) + at(1, 1);
    if me == 1 {
        let h1 = at(-1, 0) + at(1, 0);
        let h2 = at(-2, 0) + at(2, 0);
        let v1 = at(0, -1) + at(0, 1);
        let v2 = at(0, -2) + at(0, 2);
        let along_row = (5.0 * c + 4.0 * h1 - h2 - diag + 0.5 * v2) / 8.0;
        let along_col = (5.0 * c + 4.0 * v1 - v2 - diag + 0.5 * h2) / 8.0;
        if right_is_red { [along_row, c, along_col] } else { [along_col, c, along_row] }
    } else {
        let cross1 = at(-1, 0) + at(1, 0) + at(0, -1) + at(0, 1);
        let cross2 = at(-2, 0) + at(2, 0) + at(0, -2) + at(0, 2);
        let g = (4.0 * c + 2.0 * cross1 - cross2) / 8.0;
        let other = (6.0 * c + 2.0 * diag - 1.5 * cross2) / 8.0;
        if me == 0 { [c, g, other] } else { [other, g, c] }
    }
}

/// Bilinear at one Bayer pixel: the sums of [`bilinear_row`]'s 3 × 3 window in its order.
#[inline(always)]
fn bilinear_px(at: impl Fn(isize, isize) -> f32, me: u8, right_is_red: bool) -> [f32; 3] {
    let c = at(0, 0);
    if me == 1 {
        let along_row = (0.0 + at(-1, 0) + at(1, 0)) / 2.0;
        let along_col = (0.0 + at(0, -1) + at(0, 1)) / 2.0;
        if right_is_red { [along_row, c, along_col] } else { [along_col, c, along_row] }
    } else {
        let g = (0.0 + at(0, -1) + at(-1, 0) + at(1, 0) + at(0, 1)) / 4.0;
        let other = (0.0 + at(-1, -1) + at(1, -1) + at(-1, 1) + at(1, 1)) / 4.0;
        if me == 0 { [c, g, other] } else { [other, g, c] }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn rggb() -> Pattern {
        Pattern { rows: 2, cols: 2, colors: vec![0, 1, 1, 2] }
    }

    /// A mosaic of an RGB picture given per pixel.
    fn mosaic(w: usize, h: usize, pat: &Pattern, rgb: impl Fn(usize, usize) -> [f32; 3]) -> Vec<f32> {
        (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).map(|(x, y)| rgb(x, y)[usize::from(pat.color(x, y))]).collect()
    }

    #[test]
    fn bayer_detection() {
        assert!(rggb().is_bayer());
        for c in [[1, 0, 2, 1], [1, 2, 0, 1], [2, 1, 1, 0]] {
            assert!(Pattern { rows: 2, cols: 2, colors: c.to_vec() }.is_bayer(), "{c:?}");
        }
        assert!(!Pattern { rows: 2, cols: 2, colors: vec![0, 1, 2, 1] }.is_bayer(), "greens side by side");
        assert!(!Pattern { rows: 2, cols: 2, colors: vec![0, 0, 1, 2] }.is_bayer());
        assert!(!Pattern { rows: 1, cols: 3, colors: vec![0, 1, 2] }.is_bayer());
    }

    #[test]
    fn flat_fields_stay_flat_for_every_bayer_phase_and_other_patterns() {
        let pats = [[0, 1, 1, 2], [1, 0, 2, 1], [1, 2, 0, 1], [2, 1, 1, 0]].map(|c| Pattern { rows: 2, cols: 2, colors: c.to_vec() });
        let stripes = Pattern { rows: 1, cols: 3, colors: vec![0, 1, 2] };
        for pat in pats.iter().chain([&stripes]) {
            let (w, h) = (9, 7);
            let m = mosaic(w, h, pat, |_, _| [0.25, 0.5, 0.75]);
            let out = demosaic(&m, w, h, pat);
            for (i, px) in out.chunks(3).enumerate() {
                for (k, want) in [0.25, 0.5, 0.75].iter().enumerate() {
                    assert!((px[k] - want).abs() < 1e-6, "{pat:?} pixel {i} channel {k}: {}", px[k]);
                }
            }
        }
    }

    #[test]
    fn known_samples_are_kept_and_linear_ramps_are_exact() {
        // a horizontal ramp is reconstructed exactly away from the border (all filters are
        // symmetric and sum to one, the correction terms sum to zero)
        let pat = rggb();
        let (w, h) = (16, 12);
        let f = |x: usize, _y: usize| {
            let v = x as f32 / 16.0;
            [v, 0.5 * v + 0.1, 0.2 + 0.25 * v]
        };
        let m = mosaic(w, h, &pat, f);
        let out = demosaic(&m, w, h, &pat);
        for y in 0..h {
            for x in 0..w {
                let px = &out[(y * w + x) * 3..][..3];
                let own = usize::from(pat.color(x, y));
                assert_eq!(px[own], m[y * w + x], "own sample kept");
                if (2..w - 2).contains(&x) {
                    let want = f(x, y);
                    for k in 0..3 {
                        assert!((px[k] - want[k]).abs() < 1e-5, "({x},{y}) ch {k}: {} vs {}", px[k], want[k]);
                    }
                }
            }
        }
    }

    #[test]
    fn a_vertical_edge_does_not_smear_far() {
        // white left half, black right half: pixels two columns from the edge are untouched
        let pat = rggb();
        let (w, h) = (12, 8);
        let m = mosaic(w, h, &pat, |x, _| if x < 6 { [1.0; 3] } else { [0.0; 3] });
        let out = demosaic(&m, w, h, &pat);
        for y in 0..h {
            for x in (0..3).chain(9..12) {
                let want = if x < 6 { 1.0 } else { 0.0 };
                for k in 0..3 {
                    assert!((out[(y * w + x) * 3 + k] - want).abs() < 1e-6, "({x},{y}) ch {k}");
                }
            }
        }
    }

    #[test]
    fn tiny_and_empty_pictures_do_not_panic() {
        let pat = rggb();
        for (w, h) in [(0, 0), (1, 1), (2, 1), (1, 5), (2, 2), (3, 2)] {
            let m = vec![0.5; w * h];
            let out = demosaic(&m, w, h, &pat);
            assert_eq!(out.len(), w * h * 3);
        }
        assert_eq!(demosaic(&[0.5; 3], 4, 4, &pat).len(), 48, "short plane");
    }
}
