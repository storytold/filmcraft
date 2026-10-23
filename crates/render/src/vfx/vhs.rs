//! VHS Look (#2516): a clip-level analog tape look, run scanline by scanline over Y'CbCr.
//!
//! Every line goes through the stages a VHS recording puts a picture through:
//!
//! 1. R'G'B' (display-encoded) to Y'CbCr with the ITU-R BT.601 coefficients, the matrix of the
//!    525/625-line systems VHS belongs to.
//! 2. Luma and the two colour differences are low-passed *separately*. VHS keeps about 3 MHz of
//!    brightness (about 240 TV lines), recorded as FM, and colour in a narrow "colour-under" band of
//!    about 0.3 to 0.5 MHz, so edge colour is far softer than edge brightness (IEC 60774-1:1994;
//!    Poynton, *Digital Video and HDTV Algorithms and Interfaces*, 2003).
//! 3. Chroma Shift delays the colour against the brightness, so colour bleeds to one side of an edge.
//! 4. Line Jitter displaces whole lines sideways: the timebase error of a deck without a time-base
//!    corrector, part slow wobble of the tape path and part random per line.
//! 5. Tape noise, a head-switching band at the bottom of the picture (the drum's two heads take turns
//!    a few lines before vertical sync, about 6.5 on 525/60 decks, per US patent 6,650,825: the last
//!    lines are displaced and noisy, and overscan usually hides them) and dropouts (short streaks
//!    where the FM carrier is lost) are added last.
//!
//! Bandwidth is a number of cycles per *picture width*, not per pixel: a signal of B MHz crossing the
//! active part of a 525/60 line (about 52.6 µs) holds `B × 52.6` cycles, so 3 MHz is about 158
//! cycles and the look does not depend on the clip's resolution. Vertically the unit is a line of the
//! 480-line raster VHS records: jitter, noise, dropouts and the head-switching band are keyed to that
//! raster, and a 1080-line clip shows each of those lines as two or three of its own.
//!
//! A low-pass with its -3 dB point at `f` cycles per pixel is a Gaussian of sigma `sqrt(ln 2) / (2π f)`
//! pixels (the Fourier transform of a Gaussian is a Gaussian). It is run as three box passes of equal
//! variance, O(1) per pixel for any width (Wells, "Efficient synthesis of Gaussian filters by cascaded
//! uniform filters", IEEE PAMI 8(2), 1986), with fractional weights on the box ends so that the
//! variance is exact even below one pixel (Gwosdek et al., "Theoretical foundations of Gaussian
//! convolution by extended box filtering", SSVM 2011, LNCS 6667). Alpha weights the filter, so a
//! transparent pixel never lends or takes colour.
//!
//! Everything time-varying is hashed from the frame time, the clip and the Seed (never a random
//! generator), so preview and export agree bit for bit.

use filmcraft_color::{Matrix, rgb_to_ycbcr, ycbcr_to_rgb};
use filmcraft_project::EffectInstance;
use rayon::prelude::*;

use super::{fv, hash_u, hash01, smoothstep};
use crate::effects::{FxCtx, dec, enc, noise_frame_seed};
use crate::image::Image;

/// Active picture time of a line of 525/60 video, in µs.
const ACTIVE_LINE_US: f32 = 52.6;
/// Lines of the raster VHS records (525/60 active picture).
const LINES: f32 = 480.0;
/// Sigma, in pixels, of a Gaussian low-pass whose -3 dB point is 1 cycle per pixel: `sqrt(ln 2) / 2π`.
const SIGMA_AT_ONE: f32 = 0.1325;
/// Slow wobble of the tape path: lines between two random knots.
const WOBBLE_LINES: f32 = 24.0;
/// Head-switching skew at the bottom line of the band, per line of band, as a fraction of the width.
const HEAD_SKEW: f32 = 1.5 / 720.0;
/// Weight below which a filtered pixel has no colour of its own to give (all its neighbours are transparent).
const MIN_WEIGHT: f32 = 1e-4;
const SQRT_3: f32 = 1.732_050_8;

/// Hash streams, one per random quantity.
const WOBBLE: i64 = 1;
const JITTER: i64 = 2;
const HEAD_JITTER: i64 = 3;
const DROP_HIT: i64 = 4;
const DROP_START: i64 = 5;
const DROP_LENGTH: i64 = 6;
const NOISE_Y: i64 = 7;
const NOISE_CB: i64 = 8;
const NOISE_CR: i64 = 9;
const HEAD_NOISE: i64 = 10;
const DROP_SNOW: i64 = 11;

/// The settings of one frame, in working-image pixels.
struct Look {
    w: usize,
    h: usize,
    /// Low-pass sigma of luma and chroma in pixels; 0 when the cutoff is above what the image can hold.
    sigma_luma: f32,
    sigma_chroma: f32,
    /// Correlation length of luma and chroma noise along a line, in pixels.
    cell_luma: f32,
    cell_chroma: f32,
    shift: f32,
    jitter: f32,
    noise: f32,
    head_lines: f32,
    dropouts: f32,
    seed: u64,
}

/// Per-thread buffers for one row.
struct Scratch {
    y: Vec<f32>,
    cb: Vec<f32>,
    cr: Vec<f32>,
    /// Alpha weights, one copy per low-pass (they are filtered with their planes).
    wl: Vec<f32>,
    wc: Vec<f32>,
    live: Vec<bool>,
    tmp: Vec<f32>,
}

impl Scratch {
    fn new(w: usize) -> Self {
        Scratch { y: vec![0.0; w], cb: vec![0.0; w], cr: vec![0.0; w], wl: vec![0.0; w], wc: vec![0.0; w], live: vec![false; w], tmp: vec![0.0; w] }
    }
}

/// A control read as a finite number in `lo..=hi` (NaN gives `default`).
fn control(e: &EffectInstance, id: &str, cx: &FxCtx, lo: f32, hi: f32, default: f32) -> f32 {
    let v = fv(e, id, cx);
    if v.is_nan() { default } else { v.clamp(lo, hi) }
}

/// Sigma in pixels of the low-pass for `mhz` on a `w`-pixel line; 0 when its cutoff is at or above one
/// cycle per pixel, where it can change nothing a sampled image holds.
fn sigma_for(mhz: f32, w: usize) -> f32 {
    let cycles = mhz * ACTIVE_LINE_US;
    let w = w as f32;
    if cycles >= w { 0.0 } else { (SIGMA_AT_ONE * w / cycles).min(4.0 * w) }
}

pub fn vhs_look(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let (w, h) = (img.w, img.h);
    let Some(row_len) = w.checked_mul(4) else { return };
    if w == 0 || h == 0 || row_len.checked_mul(h) != Some(img.px.len()) {
        return;
    }
    let scale = if cx.px_scale.is_finite() && cx.px_scale > 0.0 { cx.px_scale.min(64.0) } else { 1.0 };
    let (luma_mhz, chroma_mhz) = (control(e, "luma_bw", cx, 0.5, 5.0, 3.0), control(e, "chroma_bw", cx, 0.05, 1.5, 0.3));
    let user_seed = control(e, "seed", cx, 0.0, 10_000.0, 0.0) as i64;
    let clip = cx.env.map_or(0, |env| env.clip_key()) as i64;
    let cell = |mhz: f32| (w as f32 / (2.0 * mhz * ACTIVE_LINE_US)).max(1.0);
    let look = Look {
        w,
        h,
        sigma_luma: sigma_for(luma_mhz, w),
        sigma_chroma: sigma_for(chroma_mhz, w),
        cell_luma: cell(luma_mhz),
        cell_chroma: cell(chroma_mhz),
        shift: control(e, "chroma_shift", cx, -20.0, 20.0, 0.0) * scale,
        jitter: control(e, "jitter", cx, 0.0, 10.0, 0.0) * scale,
        noise: control(e, "noise", cx, 0.0, 100.0, 0.0) / 100.0,
        head_lines: control(e, "head_switch", cx, 0.0, 30.0, 0.0),
        dropouts: control(e, "dropouts", cx, 0.0, 100.0, 0.0) / 100.0,
        seed: hash_u(clip, user_seed, 0x5648_5321, noise_frame_seed(cx)),
    };
    img.px.par_chunks_mut(row_len).enumerate().for_each_init(|| Scratch::new(w), |s, (y, row)| look.row(y, row, s));
}

/// Straight display-encoded Y'CbCr of a premultiplied linear pixel and its weight, or `None` for a
/// pixel that is transparent or not a number.
fn read(p: &[f32]) -> Option<([f32; 3], f32)> {
    let a = *p.get(3)?;
    if a <= 1e-6 || !p.iter().all(|v| v.is_finite()) {
        return None;
    }
    let straight = [p[0] / a, p[1] / a, p[2] / a];
    if !straight.iter().all(|v| v.is_finite()) {
        return None;
    }
    let c = enc(straight);
    Some((rgb_to_ycbcr(c[0], c[1], c[2], Matrix::Bt601), a.min(1.0)))
}

/// `plane` at `pos` pixels, linearly interpolated, the first and last pixel repeated beyond the ends.
fn lerp_at(plane: &[f32], pos: f32) -> f32 {
    let last = plane.len().saturating_sub(1);
    let p = pos.clamp(0.0, last as f32);
    let i = p as usize;
    let t = p - i as f32;
    let a = plane.get(i).copied().unwrap_or(0.0);
    let b = plane.get((i + 1).min(last)).copied().unwrap_or(a);
    a + (b - a) * t
}

impl Look {
    /// Line of the 480-line raster that row `y` of `h` shows.
    fn line_of(&self, y: usize) -> i64 {
        (((y as f32 + 0.5) * LINES / self.h as f32) as i64).min(LINES as i64 - 1)
    }

    /// Uniform random number in -1..1 for (a, b) in `stream`.
    fn rnd(&self, a: i64, b: i64, stream: i64) -> f32 {
        hash01(a, b, stream, self.seed) * 2.0 - 1.0
    }
    fn unit(&self, a: i64, stream: i64) -> f32 {
        hash01(a, 0, stream, self.seed)
    }

    /// Noise of unit variance along `line`, correlated over `cell` pixels: random knots, linear between.
    fn grain(&self, x: usize, line: i64, stream: i64, cell: f32) -> f32 {
        let u = x as f32 / cell;
        let knot = u.floor();
        let t = u - knot;
        let i = knot as i64;
        let (a, b) = (self.rnd(i, line, stream), self.rnd(i + 1, line, stream));
        (a + (b - a) * t) * SQRT_3
    }

    /// How far `line` is displaced to the right, in pixels: jitter, and the skew of the head-switching band.
    fn line_shift(&self, line: i64) -> f32 {
        let mut dx = 0.0;
        if self.jitter > 0.0 {
            let u = line as f32 / WOBBLE_LINES;
            let knot = u.floor();
            let i = knot as i64;
            let t = smoothstep(0.0, 1.0, u - knot);
            let (a, b) = (self.rnd(i, 0, WOBBLE), self.rnd(i + 1, 0, WOBBLE));
            dx += self.jitter * (0.6 * (a + (b - a) * t) + 0.4 * self.rnd(line, 0, JITTER));
        }
        let depth = self.band_depth(line);
        if depth > 0.0 {
            let skew = self.head_lines * HEAD_SKEW * self.w as f32;
            dx += skew * depth * (depth + 0.25 * self.rnd(line, 0, HEAD_JITTER));
        }
        dx
    }

    /// 0 above the head-switching band, rising to 1 at the last line of the picture.
    fn band_depth(&self, line: i64) -> f32 {
        if self.head_lines <= 0.0 {
            return 0.0;
        }
        ((line as f32 + 0.5 - (LINES - self.head_lines)) / self.head_lines).clamp(0.0, 1.0)
    }

    /// The pixels of `line` a dropout covers, if it has one.
    fn dropout(&self, line: i64) -> Option<(f32, f32)> {
        if self.dropouts <= 0.0 || self.unit(line, DROP_HIT) >= 0.06 * self.dropouts {
            return None;
        }
        let len = (0.06 + 0.16 * self.unit(line, DROP_LENGTH)) * self.w as f32;
        let start = self.unit(line, DROP_START) * (self.w as f32 - len).max(0.0);
        Some((start, start + len))
    }

    fn row(&self, y: usize, row: &mut [f32], s: &mut Scratch) {
        let w = self.w;
        if row.len() != w * 4 || s.y.len() != w {
            return;
        }
        let line = self.line_of(y);

        // 1. planes of the row, weighted by alpha
        let mut opaque = true;
        let mut any = false;
        for x in 0..w {
            match read(&row[x * 4..x * 4 + 4]) {
                Some((ycc, a)) => {
                    (s.y[x], s.cb[x], s.cr[x]) = (ycc[0] * a, ycc[1] * a, ycc[2] * a);
                    (s.wl[x], s.wc[x], s.live[x]) = (a, a, true);
                    opaque &= a >= 1.0;
                    any = true;
                }
                None => {
                    (s.y[x], s.cb[x], s.cr[x]) = (0.0, 0.0, 0.0);
                    (s.wl[x], s.wc[x], s.live[x]) = (0.0, 0.0, false);
                    opaque = false;
                }
            }
        }
        if !any {
            return;
        }

        // 2. luma and colour in their own bands
        if self.sigma_luma > 0.0 {
            smooth(&mut s.y, &mut s.tmp, self.sigma_luma);
            if !opaque {
                smooth(&mut s.wl, &mut s.tmp, self.sigma_luma);
            }
        }
        if self.sigma_chroma > 0.0 {
            smooth(&mut s.cb, &mut s.tmp, self.sigma_chroma);
            smooth(&mut s.cr, &mut s.tmp, self.sigma_chroma);
            if !opaque {
                smooth(&mut s.wc, &mut s.tmp, self.sigma_chroma);
            }
        }

        // 3. timebase: the whole line moves, the colour a little behind
        let line_dx = self.line_shift(line);
        let chroma_dx = line_dx + self.shift;
        let depth = self.band_depth(line);
        let dropout = self.dropout(line);
        let (sd_y, sd_c) = (0.20 * self.noise, 0.12 * self.noise);
        let edge = (2.0 * self.cell_luma).max(1.0);

        for x in 0..w {
            if !s.live[x] {
                continue;
            }
            let (pl, pc) = (x as f32 - line_dx, x as f32 - chroma_dx);
            let (mut yv, wl) = (lerp_at(&s.y, pl), if opaque { 1.0 } else { lerp_at(&s.wl, pl) });
            let (mut cb, mut cr, wc) = (lerp_at(&s.cb, pc), lerp_at(&s.cr, pc), if opaque { 1.0 } else { lerp_at(&s.wc, pc) });
            if wl > MIN_WEIGHT && wc > MIN_WEIGHT {
                (yv, cb, cr) = (yv / wl, cb / wc, cr / wc);
            } else {
                // nothing but transparency within reach: the pixel keeps its own colour
                let Some((own, _)) = read(&row[x * 4..x * 4 + 4]) else { continue };
                if wl > MIN_WEIGHT {
                    yv /= wl;
                } else {
                    yv = own[0];
                }
                if wc > MIN_WEIGHT {
                    (cb, cr) = (cb / wc, cr / wc);
                } else {
                    (cb, cr) = (own[1], own[2]);
                }
            }

            // 4. noise, head-switching burst, dropout
            if self.noise > 0.0 {
                yv += sd_y * self.grain(x, line, NOISE_Y, self.cell_luma);
                cb += sd_c * self.grain(x, line, NOISE_CB, self.cell_chroma);
                cr += sd_c * self.grain(x, line, NOISE_CR, self.cell_chroma);
            }
            if depth > 0.0 {
                yv += 0.35 * depth * self.grain(x, line, HEAD_NOISE, self.cell_luma);
            }
            if let Some((start, end)) = dropout {
                let xf = x as f32;
                let k = smoothstep(start, start + edge, xf) * (1.0 - smoothstep(end - edge, end, xf));
                let snow = 0.88 + 0.12 * self.grain(x, line, DROP_SNOW, self.cell_luma);
                yv += (snow - yv) * k;
                (cb, cr) = (cb * (1.0 - k), cr * (1.0 - k));
            }

            // 5. back to linear, premultiplied; alpha stays as it was
            let c = ycbcr_to_rgb(yv, cb, cr, Matrix::Bt601).map(|v| v.clamp(0.0, 1.0));
            let lin = dec(c);
            let a = row[x * 4 + 3];
            let out = [lin[0] * a, lin[1] * a, lin[2] * a];
            if out.iter().all(|v| v.is_finite()) {
                row[x * 4..x * 4 + 3].copy_from_slice(&out);
            }
        }
    }
}

/// Low-pass `plane` with a Gaussian of `sigma` pixels (three box passes, ends repeated).
fn smooth(plane: &mut [f32], tmp: &mut [f32], sigma: f32) {
    if tmp.len() != plane.len() {
        return;
    }
    let (r, alpha) = box_shape(f64::from(sigma) * f64::from(sigma) / 3.0);
    box_pass(plane, tmp, r, alpha);
    box_pass(tmp, plane, r, alpha);
    box_pass(plane, tmp, r, alpha);
    plane.copy_from_slice(tmp);
}

/// Half-width `r` and end weight `alpha` of the box `[alpha, 1 × (2r + 1), alpha]` (normalised) whose
/// variance is `v`: `r` is the largest with `r (r + 1) / 3 <= v`, and `alpha` makes up the rest.
fn box_shape(v: f64) -> (usize, f64) {
    let r = (((12.0 * v + 1.0).sqrt() - 1.0) / 2.0).floor().max(0.0);
    let inner = r * (r + 1.0) * (2.0 * r + 1.0) / 3.0;
    let alpha = (((2.0 * r + 1.0) * v - inner) / (2.0 * ((r + 1.0) * (r + 1.0) - v))).clamp(0.0, 1.0);
    (r as usize, alpha)
}

/// One pass of the box from [`box_shape`] over `src` into `dst`, with a running sum.
fn box_pass(src: &[f32], dst: &mut [f32], r: usize, alpha: f64) {
    let n = src.len();
    if n == 0 || dst.len() != n {
        return;
    }
    let r = r as isize;
    let at = |i: isize| -> f64 { src.get(i.clamp(0, n as isize - 1) as usize).copied().map_or(0.0, f64::from) };
    let norm = 1.0 / ((2 * r + 1) as f64 + 2.0 * alpha);
    let mut sum = (-r..=r).map(at).sum::<f64>() + alpha * (at(-r - 1) + at(r + 1));
    for (x, out) in dst.iter_mut().enumerate() {
        let x = x as isize;
        *out = (sum * norm) as f32;
        sum += at(x + r + 1) - at(x - r) + alpha * (at(x + r + 2) + at(x - r) - at(x - r - 1) - at(x + r + 1));
    }
}
