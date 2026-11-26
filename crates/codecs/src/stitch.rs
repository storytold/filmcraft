//! The dual-fisheye → equirectangular stitch: the calibrated seam-free projection of
//! Insta360 INSV lens circles (the job of the Insta360 Reframe plugin).
//!
//! Each fisheye is an MEI (unified) projection: a ray from the sphere centre is rotated
//! into the lens frame, intersects the unit sphere shifted by the mirror parameter `xi`,
//! and the normalised point is distorted with the Brown coefficients before the
//! intrinsics map it to the circle. The factory calibration in the INSV trailer gives
//! `xi`, the focal lengths, the principal point, the distortion and the lens pose, so the
//! projection is exact — unlike ffmpeg's `dfisheye`, which assumes a tangent 180°
//! hemisphere and misprojects the overlap band where both lenses see the scene.
//!
//! The two circles are projected onto the sphere and crossfaded across that band
//! (`FEATHER_DEG` of longitude around each seam), with a per-lens luma gain measured over
//! the overlap so a brightness step between the lens edges cannot survive the blend — the
//! same `colorAdjustMap` trick Insta360 Studio's stitch uses. The longitude convention
//! matches `filmcraft-render`'s Immersive Video effects (x is longitude −180°…180°, lon 0
//! in front of the camera, wrapping at the frame edges): the front lens (the left circle,
//! facing +Z) owns |lon| < 90°, the rear lens (the right circle, facing −Z) the far side.
//! Verified against Insta360 Studio's own stitch on X4 footage.
//!
//! Every map is baked once per output size (source coordinates + blend weight per output
//! pixel, per plane at its own subsampling); [`Stitch::apply`] only samples and blends.

use filmcraft_frame::{Chroma, PixelData, VideoFrame};
use rayon::prelude::*;
use std::borrow::Cow;
use std::sync::{Arc, Mutex};

/// Longitude band (deg) around each seam where both lenses see the scene and the stitch
/// crossfades them.
const FEATHER_DEG: f64 = 30.0;
/// The crossfade's steepness, ported from Insta360 Studio's `alphaNormalized`
/// (strings from studio_worker.dll, clean-room): a symmetric power S-curve, not a
/// linear ramp. Each half is `0.5 * (2t)^LAMBDA`, so the blend lingers near 0 and 1
/// (one lens owns most of the band) and crosses 50/50 sharply at the middle — the
/// seam reads as a single ownership boundary instead of a wide double-image.
const LAMBDA: f64 = 5.2;
/// The nadir taper, also Studio's: below the horizon the blend angle shrinks as
/// `1 - (-y)^4` (down to `BLEND_MIN_DEG`), because the nadir seam is where a
/// tripod/selfie-stick appears and a wide crossfade there ghosts it. Studio's
/// `blendAngle = clamp((1 - pow(-v_pos.y, 4.0)) * FSParam[0].z, 5.0, FSParam[0].z)`.
const BLEND_MIN_DEG: f64 = 5.0;
/// How far the per-lens luma gain may move a lens before it is trusted: the two lenses of
/// a fixed-mount camera differ by vignetting and a fraction of an EV, never by more.
const GAIN_CLAMP: f64 = 1.25;
/// The block size (px) of the local colour-gain map (Studio's `calcColorGainMapOnPano`):
/// the two lenses' brightness ratio varies smoothly across the band (fisheye vignetting
/// and exposure falloff), so a coarse grid captures it; a global scalar cannot.
const GAIN_BLOCK: usize = 48;
/// A pixel contributes to a block's gain only if the lenses agree on it to within this
/// luma. The statistic is a block *average*, so a genuinely aligned object (the girl, the
/// LED) reads ratio ~1 and is left alone however much its individual pixels disagree; the
/// gate's job is to exclude a *half-covered* pixel (one lens black at a band edge, or an
/// occluding object's silhouette), whose difference is not an exposure ratio. It must be
/// wide enough to keep far background that disagrees structurally: the littlebird truss
/// disagrees by ~66 from 3-D parallax on a lattice lit from one side, yet its exposure
/// ratio is real — a gate at 48 dropped those pixels and left a hard seam across the truss.
const GAIN_AGREE: f64 = 120.0;
/// How far a block's local gain may move a lens. Wider than [`GAIN_CLAMP`] because the
/// *local* ratio legitimately exceeds the global one — that is the whole point of the map.
const MAP_GAIN_CLAMP: f64 = 5.0;
/// The lens circle's usable radius, as a fraction of half its width: the extreme edge
/// samples the encoder's chroma sit on and the calibration's own uncertainty peaks.
const CIRCLE_EDGE: f64 = 0.985;
/// Most output pixels one bake may hold (a hostile size is refused, not allocated).
const MAX_PIXELS: usize = 1 << 26;

/// One lens's baked remap of one plane: per output pixel, the source pixel to sample and
/// the blend weight (0 where the lens sees nothing there). The sample is stored as a
/// top-left index plus 8.8 fractional weights and the neighbour steps, so the per-frame
/// remap is integer bilinear with no floor, clamp or float work.
struct PlaneMap {
    /// Top-left source index (`y0 * sw + x0`).
    idx: Vec<u32>,
    /// Packed: bits 0..8 `fx`, 9..17 `fy` (0..=256), 18 `dx`, 19 `dy` (0/1 steps).
    wgt: Vec<u32>,
    /// The feather weight, 0..=255 (0 also means "invalid").
    w: Vec<u8>,
    pw: usize,
    ph: usize,
    /// The source plane's width (the row stride `dy` scales by) and height (the clamp).
    sw: usize,
    sh: usize,
}

/// Pack a remap sample: top-left index and the 8.8 weights / neighbour steps.
fn pack(idx: usize, fx: u32, fy: u32, dx: u32, dy: u32) -> (u32, u32) {
    (idx as u32, fx | (fy << 9) | (dx << 18) | (dy << 19))
}

/// Integer bilinear of a plane at a packed sample. Returns 0..=255.
#[inline]
fn sample(p: &[u8], idx: u32, wgt: u32, sw: usize) -> i32 {
    let (fx, fy) = (wgt & 0x1FF, (wgt >> 9) & 0x1FF);
    let (ox, oy) = (((wgt >> 18) & 1) as usize, ((wgt >> 19) & 1) as usize * sw);
    let i = idx as usize;
    let (a, b, c, d) = (p[i] as i32, p[i + ox] as i32, p[i + oy] as i32, p[i + ox + oy] as i32);
    let top = a * (256 - fx as i32) + b * fx as i32;
    let bot = c * (256 - fx as i32) + d * fx as i32;
    (top * (256 - fy as i32) + bot * fy as i32 + (1 << 15)) >> 16
}

/// A baked dual-fisheye → equirect remap at one output size.
pub struct Stitch {
    w: usize,
    h: usize,
    /// Per plane (Y, U, V), per lens (0 = left = front, 1 = right = rear).
    maps: [[PlaneMap; 2]; 3],
    /// 1 / (total coverage) per luma pixel: the blend's denominator.
    inv: Vec<f32>,
    /// Luma pixels both lenses see: the band the gain is measured over.
    overlap: Vec<bool>,
    /// The planes' chroma format, taken from the source and kept by the output.
    chroma: Chroma,
    /// The two overlap bands (lon ±90), in luma columns.
    bands: [Band; 2],
}

/// One overlap band's geometry (luma columns).
#[derive(Clone, Copy)]
struct Band {
    x0: usize,
    x1: usize,
    /// True at lon -90: the front lens's weight rises across the band; false at
    /// lon +90, where it falls.
    front_rises: bool,
}

/// One band's temporally-smoothed per-frame alignment state (luma grid): the
/// along-seam shift (px per row), the hint for the next frame's search (the SDK's
/// DIS hint flow). The luma pass decides it and the chroma planes reuse the
/// decision at their own scale, so luma and chroma edges cannot disagree. The seam
/// itself has no temporal state: it is placed fresh from the aligned renders each
/// frame, as the SDK's seam finder does.
#[derive(Clone)]
struct BandSmooth {
    dx: Vec<f64>,
    /// The 2-D block flow, for the hint of the next frame (None when 1-D).
    flow: Option<Flow2>,
}

/// One source's seam/flow history (see [`Stitch::apply`]): the last finished
/// frame's temporally-smoothed per-band alignment. A bake can be shared by
/// several sources (every part of one spanned recording, two clips of one
/// camera), but their frames interleave - each source keeps its own history so
/// one clip's flow hint never blends into another's seam. Playback keeps a
/// window of a source's frames in flight, so the slots are swapped under the
/// lock and carried through the alignment outside it: a frame still in flight
/// is simply a not-yet-current hint, and the smoothing self-corrects within a
/// few frames, as it does after a seek.
#[derive(Default)]
pub struct SeamState {
    smooth: Mutex<[Option<BandSmooth>; 2]>,
}

impl SeamState {
    /// An empty history: the first frame aligns from the picture alone.
    pub fn new() -> Self {
        Self::default()
    }

    /// A copy of the last finished alignment of both bands (`apply` carries it
    /// through the work and hands the result to [`Self::set`]).
    fn grab(&self) -> [Option<BandSmooth>; 2] {
        self.smooth.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    /// Record what one `apply` finished with, from whenever its frames were
    /// taken: newer windows simply overrule older ones.
    fn set(&self, state: [Option<BandSmooth>; 2]) {
        *self.smooth.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = state;
    }
}

/// The equirectangular size for a `circle`-pixel fisheye: 2:1, a third wider than the
/// circle (the choice verified against Studio's output on X4), rounded down to 16 so
/// 4:2:0 chroma and every encoder divide evenly.
pub fn equirect_size(circle: u32) -> (u32, u32) {
    let w = (circle.saturating_mul(4) / 3).clamp(64, 8192) & !15;
    (w, w / 2)
}

impl Stitch {
    /// Bake the remap of two same-size circles to `w`×`h` (even, 2:1). `lenses` are the
    /// calibration scaled to the circle, in stream order (lens 0 = left = front). `chroma`
    /// is the source's format, which the output keeps.
    pub fn bake(lenses: &[filmcraft_insta360::Lens; 2], circle: u32, w: usize, h: usize, chroma: Chroma) -> Option<Self> {
        if w < 16 || h < 8 || !w.is_multiple_of(2) || !h.is_multiple_of(2) || circle < 8 || w.checked_mul(h)?.checked_mul(2)? > MAX_PIXELS {
            return None;
        }
        let cw = circle as usize;
        let (sw, sh) = chroma.shifts();
        let empty = || PlaneMap { idx: Vec::new(), wgt: Vec::new(), w: Vec::new(), pw: 0, ph: 0, sw: 0, sh: 0 };
        let mut maps = [[empty(), empty()], [empty(), empty()], [empty(), empty()]];
        let mut tot = vec![0f64; w * h];
        // The reference depth the two lens views are reconciled at (inverse depth
        // k = 1/Z, metres^-1); 0 (infinity) treats the lenses as co-located, the
        // historical behaviour. The baseline is centred on the two lenses' midpoint
        // so a point at Z projects to the same equirect column from both, which is
        // where the vendor SDK places a near object.
        let k = stitch_inv_depth();
        let mid = [(lenses[0].tx + lenses[1].tx) / 2.0, (lenses[0].ty + lenses[1].ty) / 2.0, (lenses[0].tz + lenses[1].tz) / 2.0];
        for (i, lens) in lenses.iter().enumerate() {
            let r = rotation_for(*lens, i);
            let base = [(lens.tx - mid[0]) * k, (lens.ty - mid[1]) * k, (lens.tz - mid[2]) * k];
            for p in 0..3 {
                let (ssx, ssy) = if p == 0 { (0, 0) } else { (sw, sh) };
                let (pw, ph) = (w >> ssx, h >> ssy);
                let m = bake_plane(lens, &r, base, PlaneGeom { circle: cw, pw, ph, ssx, ssy })?;
                if p == 0 {
                    for (k, &wt) in m.w.iter().enumerate() {
                        if wt > 0 {
                            tot[k] += f64::from(wt) / 255.0;
                        }
                    }
                }
                maps[p][i] = m;
            }
        }
        let inv: Vec<f32> = tot.iter().map(|t| if *t > 1e-6 { (1.0 / t) as f32 } else { 0.0 }).collect();
        let overlap = (0..w * h).map(|k| maps[0][0].w[k] > 0 && maps[0][1].w[k] > 0).collect();
        // The overlap bands, matching the static feather's ramp in `bake_plane`:
        // lon ±90 ± FEATHER_DEG/2, i.e. w/24 columns either side of the quarter widths.
        let half = ((FEATHER_DEG / 360.0) * w as f64 / 2.0).round().clamp(0.0, w as f64 / 4.0) as usize;
        let bands = [Band { x0: w / 4 - half, x1: w / 4 + half, front_rises: true }, Band { x0: 3 * w / 4 - half, x1: 3 * w / 4 + half, front_rises: false }];
        Some(Self { w, h, maps, inv, overlap, chroma, bands })
    }

    /// The equirect from two decoded circles (lens 0 = left, lens 1 = right).
    pub fn apply(&self, seam: &SeamState, left: &VideoFrame, right: &VideoFrame) -> Option<Arc<VideoFrame>> {
        let (l0, l1) = (Planes::of(left)?, Planes::of(right)?);
        // Every plane must carry the layout the bake maps into. The frames' own
        // geometry must be the baked circles exactly: sampling runs through the
        // baked plane strides, so a frame of equal area but different dimensions
        // (a track header lying, a size changed mid-stream) would pass any
        // length check while reading the wrong rows. Their chroma must be the
        // layout the bake was made for, and every plane at least as long as that
        // map's plane - a bad frame is reported via None like any other decode
        // failure, never a panic.
        let (b0, b1) = (&self.maps[0][0], &self.maps[0][1]);
        if [left, right].iter().zip([b0, b1]).any(|(f, m)| f.width as usize != m.sw || f.height as usize != m.sh) {
            return None;
        }
        let fmt_ok = |f: &VideoFrame| match &f.data {
            PixelData::Yuv8 { .. } | PixelData::Yuv16 { .. } => f.chroma() == self.chroma,
            _ => self.chroma == Chroma::C420, // Planes::of flattens RGBA to 4:2:0
        };
        if !fmt_ok(left) || !fmt_ok(right) {
            return None;
        }
        for (pl, i) in [(&l0, 0usize), (&l1, 1)] {
            for p in 0..3 {
                let m = &self.maps[p][i];
                if m.sw.checked_mul(m.sh).is_none_or(|need| need > pl.p[p].len()) {
                    return None;
                }
            }
        }
        let (w, h) = (self.w, self.h);
        let m = &self.maps[0];
        // per-lens luma means over the overlap band (F39): the colorAdjustMap gain.
        // Folded over rows, which is exact: the terms are integer samples widened
        // to f64, so any summation order (or none) gives the same sums.
        let (s0, s1, n) = (0..h)
            .into_par_iter()
            .map(|row| {
                let (k0, k1) = (row * w, (row + 1) * w);
                let mut acc = (0f64, 0f64, 0usize);
                for (k, o) in (k0..k1).zip(&self.overlap[k0..k1]) {
                    if *o {
                        acc.0 += f64::from(sample(&l0.p[0], m[0].idx[k], m[0].wgt[k], m[0].sw));
                        acc.1 += f64::from(sample(&l1.p[0], m[1].idx[k], m[1].wgt[k], m[1].sw));
                        acc.2 += 1;
                    }
                }
                acc
            })
            .reduce(|| (0f64, 0f64, 0usize), |a, b| (a.0 + b.0, a.1 + b.1, a.2 + b.2));
        let (g0, g1) = if n > 0 { lens_gains(s0 / n as f64, s1 / n as f64) } else { (1.0, 1.0) };
        // Per-frame seam alignment (F40): flow + DP seam per band, temporally
        // smoothed. Each source owns its history (`seam`), so a seek self-corrects
        // within a few frames and sharing a bake between sources cannot mix their
        // histories; the lock covers only the slot swaps, so the frames a source
        // has in flight at once (playback's prefetch window) align concurrently
        // instead of queueing behind each other's work. The two bands are
        // independent, so they align concurrently too.
        let plans = {
            let [mut s0, mut s1] = seam.grab();
            let (a, b) =
                rayon::join(|| self.band_plan(&self.bands[0], &l0, &l1, g0, g1, &mut s0), || self.band_plan(&self.bands[1], &l0, &l1, g0, g1, &mut s1));
            seam.set([s0, s1]);
            [a, b]
        };
        // luma: remap + feather + gain, then the aligned bands overwrite their columns
        let mut out = vec![0u8; w * h];
        out.par_chunks_mut(w).enumerate().for_each(|(row, dst)| {
            for x in 0..w {
                let k = row * w + x;
                let (a0, a1) = (f64::from(m[0].w[k]) / 255.0 * g0, f64::from(m[1].w[k]) / 255.0 * g1);
                let mut acc = 0f64;
                if a0 > 0.0 {
                    acc += f64::from(sample(&l0.p[0], m[0].idx[k], m[0].wgt[k], m[0].sw)) * a0;
                }
                if a1 > 0.0 {
                    acc += f64::from(sample(&l1.p[0], m[1].idx[k], m[1].wgt[k], m[1].sw)) * a1;
                }
                dst[x] = (acc * f64::from(self.inv[k])).clamp(0.0, 255.0).round() as u8;
            }
        });
        for p in plans.iter().flatten() {
            out.par_chunks_mut(w * REMAP_ROWS).enumerate().for_each(|(ci, block)| {
                let y0 = ci * REMAP_ROWS;
                for j in 0..block.len() / w {
                    let y = y0 + j;
                    for i in 0..p.bw {
                        let at = y * p.bw + i;
                        let acc = f64::from(p.f[at]) * f64::from(p.a0[at]) * (g0 * f64::from(p.gm0[at]))
                            + f64::from(p.r[at]) * f64::from(p.a1[at]) * (g1 * f64::from(p.gm1[at]));
                        block[j * w + p.x0 + i] = (acc * f64::from(p.inv[at])).clamp(0.0, 255.0).round() as u8;
                    }
                }
            });
        }
        // chroma: remap + feather, no gain (a centred difference signal), bands overwritten
        let mut planes = [out, Vec::new(), Vec::new()];
        for q in 1..3 {
            let m = &self.maps[q];
            let (pw, ph) = (m[0].pw, m[0].ph);
            if l0.p[q].len() < m[0].sw * m[0].sh || l1.p[q].len() < m[1].sw * m[1].sh {
                return None;
            }
            let mut c = vec![128u8; pw * ph];
            c.par_chunks_mut(pw).enumerate().for_each(|(row, dst)| {
                for x in 0..pw {
                    let k = row * pw + x;
                    let (a0, a1) = (f64::from(m[0].w[k]) / 255.0, f64::from(m[1].w[k]) / 255.0);
                    let mut acc = 0f64;
                    let mut tot = 0f64;
                    if a0 > 0.0 {
                        acc += f64::from(sample(&l0.p[q], m[0].idx[k], m[0].wgt[k], m[0].sw)) * a0;
                        tot += a0;
                    }
                    if a1 > 0.0 {
                        acc += f64::from(sample(&l1.p[q], m[1].idx[k], m[1].wgt[k], m[1].sw)) * a1;
                        tot += a1;
                    }
                    dst[x] = if tot > 1e-6 { (acc / tot).clamp(0.0, 255.0).round() as u8 } else { 128 };
                }
            });
            for p in plans.iter().flatten() {
                for y in 0..p.ch {
                    for i in 0..p.cbw {
                        let at = y * p.cbw + i;
                        let acc = f64::from(p.cf[q - 1][at]) * f64::from(p.ca0[at]) + f64::from(p.cr[q - 1][at]) * f64::from(p.ca1[at]);
                        c[y * pw + p.cx0 + i] = (acc * f64::from(p.cinv[at])).clamp(0.0, 255.0).round() as u8;
                    }
                }
            }
            planes[q] = c;
        }
        let f = VideoFrame {
            width: self.w as u32,
            height: self.h as u32,
            data: PixelData::Yuv8 { planes: planes.map(Arc::new), chroma: self.chroma, alpha: None },
            color: left.color,
            par: (1, 1),
            pts: left.pts,
        };
        Some(Arc::new(f))
    }

    /// One band's per-frame alignment (F40): warp both lens renders, estimate the
    /// along-seam shift per row (block matching), warp the rear onto the front, place
    /// the seam by DP through the (aligned) disagreement, smooth both across frames,
    /// and derive the blend weights — for luma and, at the source's subsampling with
    /// the luma decision reused, for both chroma planes. `None` when the band is too
    /// narrow to align (the static feather then stands).
    fn band_plan(&self, b: &Band, l0: &Planes, l1: &Planes, g0: f64, g1: f64, state: &mut Option<BandSmooth>) -> Option<BandPlan> {
        let (w, h) = (self.w, self.h);
        let bw = b.x1.checked_sub(b.x0)?;
        if bw < 8 {
            return None;
        }
        let sc = decision_scale(bw);
        let (dw, dh) = (bw * sc, h * sc);
        let margin = FLOW_MAX_PX as usize + 2;
        let (rx0, rx1) = (b.x0.saturating_sub(margin), (b.x1 + margin).min(w));
        let f = warp_band(&self.maps[0][0], &l0.p[0], b.x0, b.x1);
        let rwide = warp_band(&self.maps[0][1], &l1.p[0], rx0, rx1);
        let off = b.x0 - rx0;
        let r0: Vec<u8> = {
            let mut v = vec![0u8; bw * h];
            for y in 0..h {
                v[y * bw..(y + 1) * bw].copy_from_slice(&rwide[y * (rx1 - rx0) + off..y * (rx1 - rx0) + off + bw]);
            }
            v
        };
        // The seam/flow decisions run on gain-normalised renders (Studio's
        // calcColorGainMapOnPano): the two lenses differ in exposure, and the raw
        // |f - r| cost would be dominated by that offset rather than by content,
        // so the DP seam wanders frame to frame and slices through a subject.
        // Equalising the means first makes the disagreement mean the scene, so the
        // seam routes around objects consistently. The blend still applies the
        // gains to the raw renders, so the output is unchanged except where the
        // seam is placed.
        let fg_u = up_u8(&normalise(&f, g0), bw, h, sc);
        let r0g_u = up_u8(&normalise(&r0, g1), bw, h, sc);
        // 1. the along-seam shift, seeded by the previous frame's (the hint flow),
        // temporally smoothed, then applied to the rear render — at the decision
        // lattice, so a proxy band's flow votes carry the same angle per pixel as
        // the full stitch's did when the limits were tuned.
        let prev = state.clone();
        let rw_u = up_u8(&rwide, rx1 - rx0, h, sc);
        let mut r_u = vec![0u8; dw * dh];
        // The shift tapers to zero at the band edges (Hanning) so the warped rear
        // meets the unwarped render the static feather uses outside the band.
        let (dx_u, flow2_u) = warp_rear(&fg_u, &r0g_u, &RearSrc { data: &rw_u, stride: (rx1 - rx0) * sc, off: off * sc }, dw, dh, prev.as_ref(), &mut r_u);
        let r = box_down_u8(&r_u, dw, dh, bw, h);
        // The spatially-varying colour gain (Studio's calcColorGainMapOnPano): the
        // lenses' brightness ratio is a vignetting gradient across the band, not a
        // constant, so the single global gain leaves a step the hard seam shows as a
        // light line (and half-lights a subject sitting on it). Measured on the
        // globally-normalised aligned renders as the *residual* local ratio, it is
        // tapered to unity at the band edges (so it cannot fight the static feather
        // there) and applied on top of the global gain to both the seam decision and
        // the blend.
        let rg_u = normalise(&r_u, g1);
        let (gm0_u, gm1_u) = local_gain_map(&fg_u, &rg_u, dw, dh);
        let (fg, rg) = (apply_map(&fg_u, &gm0_u), apply_map(&rg_u, &gm1_u));
        // 2. the seam: a 2-D cut over the aligned disagreement (shortest-path dual, routes around
        // a compact parallax blob in both axes), falling back to the 1-D DP when the
        // band is too small or too large for a bounded graph. The cut places the
        // seam fresh from the aligned renders each frame (as the SDK's seam finder
        // does — no temporal seam state); its label grid drives the blend below.
        let mdec = band_coverage_maps(&self.maps[0], b, dw, dh, sc);
        let bdec = Band { x0: 0, x1: dw, front_rises: b.front_rises };
        let grid = seam_grid_with_floor(&fg, &rg, &mdec, &bdec, dw, dh, sc > 1);
        let (mut path, labels) = match seam_cut(&grid, &bdec, dw, dh, sc == 1) {
            Some((labels, path)) => (path, labels),
            None => (seam_path(&grid, dw, dh), Vec::new()),
        };
        let base = (dw as f64 * BLEND_RAMP).max(4.0);
        *state = Some(BandSmooth { dx: dx_u.clone(), flow: flow2_u.clone() });
        // The crossfade ramp: a fixed width (BLEND_RAMP of the band, narrower than
        // the placement window now that the gain map removes the brightness step the
        // wide ramp used to hide), narrowed below the horizon by Studio's nadir
        // taper. The seam stays where the cut placed it; only the ramp is drawn.
        let mut ramp = vec![base; dh];
        for y in 0..dh {
            // Studio's nadir taper: below the horizon the blend narrows (to
            // BLEND_MIN_DEG at the pole), because the nadir seam is where a
            // tripod/selfie-stick appears and a wide crossfade there ghosts it.
            // lat = pi/2 - (y+.5)/h*pi, so sin(lat) < 0 below the equator.
            let sl = std::f64::consts::FRAC_PI_2 - (y as f64 + 0.5) / dh as f64 * std::f64::consts::PI;
            let (sl, _) = sl.sin_cos();
            if sl < 0.0 {
                ramp[y] *= (1.0 - sl.powi(4)).max(BLEND_MIN_DEG / FEATHER_DEG);
            }
            path[y] = path[y].clamp(ramp[y] / 2.0, dw as f64 - ramp[y] / 2.0);
        }
        // TEMP DIAGNOSTIC: FILMCRAFT_STITCH_DUMP=<dir> writes the band renders and
        // the placed seam geometry per frame for offline analysis.
        if let Ok(dir) = std::env::var("FILMCRAFT_STITCH_DUMP") {
            static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let p = std::path::Path::new(&dir);
            let _ = std::fs::create_dir_all(p);
            let tag = if b.x0 > self.w / 2 { "b1" } else { "b0" };
            let wpgm = |name: &str, d: &[u8]| {
                let mut v = format!("P5\n{dw} {dh}\n255\n").into_bytes();
                v.extend_from_slice(d);
                let _ = std::fs::write(p.join(format!("{tag}_{name}_{n:04}.pgm")), v);
            };
            wpgm("f", &fg);
            wpgm("r", &rg);
            if let Some(f2) = &flow2_u {
                let (gx, gy) = (f2.gx, f2.gy);
                let mut fx = vec![0u8; gx * gy];
                let mut fy = vec![0u8; gx * gy];
                for i in 0..(gx * gy) {
                    fx[i] = (128.0 + f2.dxs[i] * 4.0).clamp(0.0, 255.0) as u8;
                    fy[i] = (128.0 + f2.dys[i] * 4.0).clamp(0.0, 255.0) as u8;
                }
                let _ = std::fs::write(
                    p.join(format!("{tag}_flowx_{n:04}.pgm")),
                    format!("P5\n{gx} {gy}\n255\n").into_bytes().into_iter().chain(fx).collect::<Vec<_>>(),
                );
                let _ = std::fs::write(
                    p.join(format!("{tag}_flowy_{n:04}.pgm")),
                    format!("P5\n{gx} {gy}\n255\n").into_bytes().into_iter().chain(fy).collect::<Vec<_>>(),
                );
            }
            if !labels.is_empty() {
                wpgm("labels", &labels_map(&grid, &labels, dw, dh));
            }
            let mut g = Vec::new();
            for y in (0..dh).step_by(8) {
                g.push(format!("{y} {} {}\n", path[y], ramp[y]));
            }
            let _ = std::fs::write(p.join(format!("{tag}_geom_{n:04}.txt")), g.concat());
        }
        let (mut a0, mut a1, mut inv) = (vec![0u8; bw * h], vec![0u8; bw * h], vec![0f32; bw * h]);
        let d = if b.front_rises { 1.0 } else { -1.0 };
        let alpha_u = ownership_field(&grid, &labels, &ramp, &path, d, dw, dh);
        // Map the decisions back onto output pixels: ownership and gains are
        // field quantities (box-average), shifts divide by the lattice ratio.
        let alpha_field = box_down_f64(&alpha_u, dw, dh, bw, h);
        let gm0 = box_down_f32(&gm0_u, dw, dh, bw, h);
        let gm1 = box_down_f32(&gm1_u, dw, dh, bw, h);
        let dx: Vec<f64> = (0..h).map(|y| (0..sc).map(|j| dx_u[y * sc + j]).sum::<f64>() / (sc * sc) as f64).collect();
        let (m0w, m1w) = (&self.maps[0][0].w, &self.maps[0][1].w);
        a0.par_chunks_mut(bw * REMAP_ROWS).zip(a1.par_chunks_mut(bw * REMAP_ROWS)).zip(inv.par_chunks_mut(bw * REMAP_ROWS)).enumerate().for_each(
            |(ci, ((p0, p1), pi))| {
                let y0 = ci * REMAP_ROWS;
                for j in 0..p0.len() / bw {
                    let y = y0 + j;
                    for i in 0..bw {
                        let at = j * bw + i;
                        let alpha = alpha_field[y * bw + i];
                        let k = y * w + b.x0 + i;
                        let (v0, v1) = (m0w[k] > 0, m1w[k] > 0);
                        p0[at] = if v0 { (alpha * 255.0).round() as u8 } else { 0 };
                        p1[at] = if v1 { ((1.0 - alpha) * 255.0).round() as u8 } else { 0 };
                        let tot = u32::from(p0[at]) + u32::from(p1[at]);
                        pi[at] = if tot > 0 { 1.0 / tot as f32 } else { 0.0 };
                    }
                }
            },
        );
        // chroma: the same decision at the plane's own scale (luma and chroma edges
        // must not disagree), so the shift and seam divide by the subsampling.
        let (ssx, ssy) = self.chroma.shifts();
        let (hs, vs) = ((1usize << ssx) as f64, (1usize << ssy) as f64);
        let (cbw, ch, cx0) = (bw >> ssx, h >> ssy, b.x0 >> ssx);
        let cmargin = (margin >> ssx).max(1);
        let (crx0, crx1) = (cx0.saturating_sub(cmargin), (cx0 + cbw + cmargin).min(w >> ssx));
        let mut cf = [Vec::new(), Vec::new()];
        let mut cr = [Vec::new(), Vec::new()];
        let dx_c: Vec<f64> = (0..ch).map(|y| (0..(1 << ssy)).map(|j| dx[(y << ssy) + j]).sum::<f64>() / vs / hs).collect();
        // The luma ownership field at chroma scale: the box average straight from
        // the decision field (so luma and chroma edges cannot disagree).
        let ca_alpha = box_down_f64(&alpha_u, dw, dh, cbw, ch);
        // The luma 2-D flow at chroma scale: same grid, spacing and magnitudes
        // divided by the subsampling, so luma and chroma warp by the same physical
        // shift (None when 1-D, which uses dx_c below).
        // The 2-D flow at chroma scale: the decision-lattice grid's spacing and
        // magnitudes divided by the lattice ratio and the subsampling, so luma and
        // chroma warp by the same physical shift (None when 1-D, which uses dx_c).
        let flow2_c = flow2_u.as_ref().map(|f| Flow2 {
            dxs: f.dxs.clone(),
            dys: f.dys.clone(),
            gx: f.gx,
            gy: f.gy,
            bsx: f.bsx / (hs * sc as f64),
            bsy: f.bsy / (vs * sc as f64),
            sx: f.sx / (hs * sc as f64),
            sy: f.sy / (vs * sc as f64),
        });
        let (mut ca0, mut ca1, mut cinv) = (vec![0u8; cbw * ch], vec![0u8; cbw * ch], vec![0f32; cbw * ch]);
        for q in 1..3 {
            let (mm0, mm1) = (&self.maps[q][0], &self.maps[q][1]);
            let cw_full = w >> ssx;
            let f_c = warp_band(mm0, &l0.p[q], cx0, cx0 + cbw);
            let rw = warp_band(mm1, &l1.p[q], crx0, crx1);
            let mut r_c = vec![0u8; cbw * ch];
            let win_c: Vec<f64> =
                (0..cbw).map(|i| if cbw == 1 { 1.0 } else { 0.5 - 0.5 * (std::f64::consts::TAU * (i as f64 + 0.5) / cbw as f64).cos() }).collect();
            match &flow2_c {
                Some(f) => resample_2d(&rw, crx1 - crx0, cbw, cx0 - crx0, f, &win_c, &mut r_c),
                None => resample_x(&rw, crx1 - crx0, cbw, cx0 - crx0, &dx_c, &win_c, &mut r_c),
            }
            cf[q - 1] = f_c;
            cr[q - 1] = r_c;
            if q == 1 {
                for y in 0..ch {
                    for i in 0..cbw {
                        let at = y * cbw + i;
                        let alpha = ca_alpha[at];
                        let k = y * cw_full + cx0 + i;
                        let (v0, v1) = (mm0.w[k] > 0, mm1.w[k] > 0);
                        ca0[at] = if v0 { (alpha * 255.0).round() as u8 } else { 0 };
                        ca1[at] = if v1 { ((1.0 - alpha) * 255.0).round() as u8 } else { 0 };
                        let tot = u32::from(ca0[at]) + u32::from(ca1[at]);
                        cinv[at] = if tot > 0 { 1.0 / tot as f32 } else { 0.0 };
                    }
                }
            }
        }
        Some(BandPlan { x0: b.x0, bw, f, r, a0, a1, inv, gm0, gm1, cx0, cbw, ch, cf, cr, ca0, ca1, cinv })
    }
}

/// One plane's geometry for [`bake_plane`]: the decoded lens circle, the plane's size, and its
/// chroma subsampling shifts.
#[derive(Clone, Copy)]
struct PlaneGeom {
    circle: usize,
    pw: usize,
    ph: usize,
    ssx: u32,
    ssy: u32,
}

/// Bake one lens's remap of one plane at its own subsampling. The feather side comes
/// from the rotation `r`: each lens owns the scene near its own axis (see `bake_plane`).
fn bake_plane(lens: &filmcraft_insta360::Lens, r: &[[f64; 3]; 3], base: [f64; 3], g: PlaneGeom) -> Option<PlaneMap> {
    let PlaneGeom { circle, pw, ph, ssx, ssy } = g;
    let (fx, fy, cx, cy, xi) = (lens.fx, lens.fy, lens.cx, lens.cy, lens.xi);
    if !xi.is_finite() || xi <= 1.0 || fx <= 0.0 || fy <= 0.0 || pw == 0 || ph == 0 {
        return None;
    }
    // The lens's baseline offset in its own frame (rotated once): the sampling ray
    // is shifted by this so a point at the reference depth projects to the same
    // longitude from both lenses. Zero baseline (or infinity depth) leaves it put.
    let (bx, by, bz) = (
        r[0][0] * base[0] + r[0][1] * base[1] + r[0][2] * base[2],
        r[1][0] * base[0] + r[1][1] * base[1] + r[1][2] * base[2],
        r[2][0] * base[0] + r[2][1] * base[1] + r[2][2] * base[2],
    );
    let (q, sq) = ((1u32 << ssx) as f64, (1u32 << ssy) as f64);
    let (sw, sh) = ((circle as f64 / q) as usize, (circle as f64 / sq) as usize);
    if sw < 2 || sh < 2 {
        return None;
    }
    let lim = circle as f64 - 1.0;
    let edge2 = (circle as f64 / 2.0 * CIRCLE_EDGE) * (circle as f64 / 2.0 * CIRCLE_EDGE);
    let mut idx = vec![0u32; pw * ph];
    let mut wgt = vec![0u32; pw * ph];
    let mut w = vec![0u8; pw * ph];
    for y in 0..ph {
        let lat = std::f64::consts::FRAC_PI_2 - (y as f64 * sq + sq * 0.5) / (ph as f64 * sq) * std::f64::consts::PI;
        let (cl, sl) = (lat.cos(), lat.sin());
        for x in 0..pw {
            let lon = (x as f64 * q + q * 0.5) / (pw as f64 * q) * std::f64::consts::TAU - std::f64::consts::PI;
            let (rx, ry, rz) = (cl * lon.sin(), -sl, cl * lon.cos());
            let (lx, ly, lz) =
                (r[0][0] * rx + r[0][1] * ry + r[0][2] * rz, r[1][0] * rx + r[1][1] * ry + r[1][2] * rz, r[2][0] * rx + r[2][1] * ry + r[2][2] * rz);
            let nrm = (lx * lx + ly * ly + lz * lz).max(1e-10).sqrt();
            let (xs, ys, zs) = (lx / nrm, ly / nrm, lz / nrm);
            let at = y * pw + x;
            // The crossfade, ported from Insta360 Studio's `calAlpha` +
            // `alphaNormalized`: a nadir-tapered steepstep, not a linear ramp.
            // Below the horizon the blend narrows (a tripod/stick there would
            // otherwise ghost); the steepstep makes each lens own most of the
            // band and cross 50/50 sharply, so the seam reads as one boundary.
            let taper = if sl < 0.0 { (1.0 - sl.powi(4)).max(BLEND_MIN_DEG / FEATHER_DEG) } else { 1.0 };
            let eff = FEATHER_DEG * taper;
            // The crossfade is the ray's ANGLE from the lens's own axis (Studio's
            // `calAlpha`: `atan(sqrt(x^2+y^2), -z)` in the lens frame), not its
            // longitude. The two agree along the equator (where the seam is a
            // meridian, so angle-from-axis == |lon|) but differ toward the poles:
            // a point straight up is 90 deg from BOTH horizontal lenses whatever
            // its longitude, so the zenith/nadir must be shared 50/50, not owned
            // by |lon| < 90. `zs` is the ray's lens-frame cosine, so `acos(zs)`
            // is that angle; each lens owns the scene near its own axis.
            let theta = zs.clamp(-1.0, 1.0).acos().to_degrees();
            let ramp = (theta - (90.0 - eff / 2.0)) / eff;
            let owns = steepstep(1.0 - ramp.clamp(0.0, 1.0));
            // The sampling ray, shifted by the baseline (the ownership ray above is
            // the nominal one, so the seam stays where the feather puts it). At the
            // reference depth the two lenses' sampling rays agree on the point.
            let (px, py, pz) = (xs - bx, ys - by, zs - bz);
            let pnrm = (px * px + py * py + pz * pz).max(1e-10).sqrt();
            let (px, py, pz) = (px / pnrm, py / pnrm, pz / pnrm);
            let den = pz + xi;
            if den > 1e-6 && pz > -1.0 / xi && owns > 0.0 {
                let (u, v) = (px / den, py / den);
                let r2 = u * u + v * v;
                let rad = 1.0 + lens.k1 * r2 + lens.k2 * r2 * r2 + lens.k3 * r2 * r2 * r2;
                let xd = u * rad + 2.0 * lens.p1 * u * v + lens.p2 * (r2 + 2.0 * u * u);
                let yd = v * rad + lens.p1 * (r2 + 2.0 * v * v) + 2.0 * lens.p2 * u * v;
                let (sx, sy) = (fx * xd + cx, fy * yd + cy);
                if sx >= 0.0 && sx < lim && sy >= 0.0 && sy < lim && (sx - cx) * (sx - cx) + (sy - cy) * (sy - cy) < edge2 {
                    // circle luma coordinates → this plane's coordinates, then a packed
                    // bilinear sample (top-left index + 8.8 weights + neighbour steps)
                    let (fxp, fyp) = ((sx - q * 0.5 + 0.5) / q, (sy - sq * 0.5 + 0.5) / sq);
                    let (x0, y0) = (fxp.floor().max(0.0) as usize, fyp.floor().max(0.0) as usize);
                    let (x0, y0) = (x0.min(sw - 1), y0.min(sh - 1));
                    let (tx, ty) = ((fxp - x0 as f64).clamp(0.0, 1.0), (fyp - y0 as f64).clamp(0.0, 1.0));
                    let (dx, dy) = (if x0 < sw - 1 { 1 } else { 0 }, if y0 < sh - 1 { 1 } else { 0 });
                    let (i, g) = pack(y0 * sw + x0, (tx * 256.0).round() as u32, (ty * 256.0).round() as u32, dx, dy);
                    idx[at] = i;
                    wgt[at] = g;
                    w[at] = (owns * 255.0).round() as u8;
                }
            }
        }
    }
    Some(PlaneMap { idx, wgt, w, pw, ph, sw, sh })
}

/// Rotate the sphere ray into the lens frame: yaw/pitch from the calibration (the
/// firmware de-rolls the image, so `roll` describes the sensor mount, not the picture),
/// plus a half turn for the rear lens (index 1, the right circle, facing −Z).
fn rotation_for(lens: filmcraft_insta360::Lens, index: usize) -> [[f64; 3]; 3] {
    let r = rot_yx(lens.yaw.to_radians(), lens.pitch.to_radians());
    if index == 1 { mat_mul(&rot_yx(std::f64::consts::PI, 0.0), &r) } else { r }
}

/// Ry(yaw) @ Rx(pitch) — the convention the reference stitchers use.
fn rot_yx(yaw: f64, pitch: f64) -> [[f64; 3]; 3] {
    let (sy, cy) = yaw.sin_cos();
    let (sx, cx) = pitch.sin_cos();
    [[cy, sy * sx, sy * cx], [0.0, cx, -sx], [-sy, cy * sx, cy * cx]]
}

fn mat_mul(a: &[[f64; 3]; 3], b: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut o = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            o[i][j] = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
        }
    }
    o
}

/// The three 8-bit planes of a decoded frame: borrowed when the frame already holds them
/// (YUV8, the common case: no copy of ~25 MB per stitched frame), converted otherwise.
struct Planes<'a> {
    p: [Cow<'a, [u8]>; 3],
}

impl<'a> Planes<'a> {
    fn of(f: &'a VideoFrame) -> Option<Self> {
        match &f.data {
            PixelData::Yuv8 { planes, .. } => Some(Self { p: [Cow::Borrowed(&planes[0][..]), Cow::Borrowed(&planes[1][..]), Cow::Borrowed(&planes[2][..])] }),
            PixelData::Yuv16 { planes, bits, .. } => {
                let s = bits.saturating_sub(8).clamp(0, 8);
                let c = |p: &[u16]| Cow::Owned(p.iter().map(|&v| (v >> s) as u8).collect::<Vec<u8>>());
                Some(Self { p: [c(&planes[0]), c(&planes[1]), c(&planes[2])] })
            }
            _ => {
                let rgba = f.to_rgba8();
                let (w, h) = (f.width as usize, f.height as usize);
                let (mut y, mut u, mut v) = (vec![0u8; w * h], vec![128u8; w * h / 4], vec![128u8; w * h / 4]);
                for r in 0..h {
                    for x in 0..w {
                        let p = &rgba[(r * w + x) * 4..];
                        let yy = 0.2126 * p[0] as f64 + 0.7152 * p[1] as f64 + 0.0722 * p[2] as f64;
                        y[r * w + x] = yy.round() as u8;
                    }
                }
                for r in 0..h / 2 {
                    for x in 0..w / 2 {
                        let p = &rgba[(r * 2 * w + x * 2) * 4..];
                        let yy = 0.2126 * p[0] as f64 + 0.7152 * p[1] as f64 + 0.0722 * p[2] as f64;
                        u[r * (w / 2) + x] = (128.0 + (p[1] as f64 - yy) / 1.772).clamp(0.0, 255.0) as u8;
                        v[r * (w / 2) + x] = (128.0 + (p[0] as f64 - yy) / 1.402).clamp(0.0, 255.0) as u8;
                    }
                }
                Some(Self { p: [Cow::Owned(y), Cow::Owned(u), Cow::Owned(v)] })
            }
        }
    }
}

/// Per-lens gains equalising two overlap means to their geometric mean, clamped.
fn lens_gains(m0: f64, m1: f64) -> (f64, f64) {
    if m0 <= 0.0 || m1 <= 0.0 || !m0.is_finite() || !m1.is_finite() {
        return (1.0, 1.0);
    }
    let t = (m0 * m1).max(0.0).sqrt();
    let (lo, hi) = (1.0 / GAIN_CLAMP, GAIN_CLAMP);
    ((t / m0).clamp(lo, hi), (t / m1).clamp(lo, hi))
}

/// The spatially-varying per-lens gain map (Studio's `calcColorGainMapOnPano`): the
/// two lenses' brightness ratio is not a constant but a smooth gradient across the
/// band (fisheye vignetting and exposure falloff), which a single global gain cannot
/// follow — so a hard seam shows it as a light line and cuts a subject that crosses
/// it. Measured on a coarse [`GAIN_BLOCK`] grid over the *globally-normalised, aligned*
/// renders, using only pixels where both lenses cover and agree to within
/// [`GAIN_AGREE`] luma (background, where the ratio is the vignetting we want, not an
/// object's parallax), each block's ratio is split geometrically between the lenses so
/// the local mean is preserved, clamped to [`MAP_GAIN_CLAMP`], and bilinearly
/// upsampled to per-pixel. Returns the front and rear maps (`bw * h`, ~1.0).
fn local_gain_map(f: &[u8], r: &[u8], bw: usize, h: usize) -> (Vec<f32>, Vec<f32>) {
    let (gw, gh) = (bw.div_ceil(GAIN_BLOCK).max(1), h.div_ceil(GAIN_BLOCK).max(1));
    let (lo, hi) = (1.0 / MAP_GAIN_CLAMP, MAP_GAIN_CLAMP);
    // Per block: the rear/front ratio over the block's centre column, using only
    // pixels both lenses cover and agree on (background, where the ratio is the
    // vignetting we want to measure, not an object's parallax).
    let mut front = vec![1f32; gw * gh];
    let mut rear = vec![1f32; gw * gh];
    front.par_chunks_mut(gw).zip(rear.par_chunks_mut(gw)).enumerate().for_each(|(by, (frow, rrow))| {
        for bx in 0..gw {
            let x = (bx * GAIN_BLOCK + GAIN_BLOCK / 2).min(bw - 1);
            let (mut s0, mut s1, mut n) = (0f64, 0f64, 0u64);
            for y in (by * GAIN_BLOCK)..((by + 1) * GAIN_BLOCK).min(h) {
                let (fv, rv) = (f64::from(f[y * bw + x]), f64::from(r[y * bw + x]));
                if fv > 2.0 && rv > 2.0 && (fv - rv).abs() < GAIN_AGREE {
                    s0 += fv;
                    s1 += rv;
                    n += 1;
                }
            }
            let (fcell, rcell) = (&mut frow[bx], &mut rrow[bx]);
            if n >= 8 && s0 > 0.0 {
                // Split the ratio geometrically so the local mean is preserved: the
                // brighter lens is cut by sqrt(ratio), the dimmer boosted by its
                // reciprocal, so both land on the geometric mean.
                let rho = (s1 / s0).clamp(lo * lo, hi * hi).sqrt();
                *fcell = rho.clamp(lo, hi) as f32;
                *rcell = (1.0 / rho).clamp(lo, hi) as f32;
            }
        }
    });
    (bilinear_up(&front, gw, gh, bw, h), bilinear_up(&rear, gw, gh, bw, h))
}

/// Bilinearly upsample a `gw * gh` grid to `bw * bh`, clamping at the edges.
fn bilinear_up(g: &[f32], gw: usize, gh: usize, bw: usize, bh: usize) -> Vec<f32> {
    let mut o = vec![1f32; bw * bh];
    if gw == 0 || gh == 0 {
        return o;
    }
    let sx = if gw > 1 { (gw - 1) as f64 / (bw - 1).max(1) as f64 } else { 0.0 };
    let sy = if gh > 1 { (gh - 1) as f64 / (bh - 1).max(1) as f64 } else { 0.0 };
    o.par_chunks_mut(bw).enumerate().for_each(|(y, orow)| {
        let gy = (y as f64 * sy).clamp(0.0, (gh - 1) as f64);
        let (y0, fy) = (gy.floor() as usize, gy - gy.floor());
        let y1 = (y0 + 1).min(gh - 1);
        for (x, cell) in orow.iter_mut().enumerate() {
            let gx = (x as f64 * sx).clamp(0.0, (gw - 1) as f64);
            let (x0, fx) = (gx.floor() as usize, gx - gx.floor());
            let x1 = (x0 + 1).min(gw - 1);
            let (fx, fy) = (fx as f32, fy as f32);
            let (a, b, c, d) = (g[y0 * gw + x0], g[y0 * gw + x1], g[y1 * gw + x0], g[y1 * gw + x1]);
            *cell = a * (1.0 - fx) * (1.0 - fy) + b * fx * (1.0 - fy) + c * (1.0 - fx) * fy + d * fx * fy;
        }
    });
    o
}

/// Apply a per-pixel gain map to a render (a float multiply). The map is already
/// ~1.0 at the band's single-coverage edges (a block where one lens is black has no
/// agreeing pixels and defaults to unity), so it needs no extra taper to meet the
/// static feather there.
fn apply_map(p: &[u8], m: &[f32]) -> Vec<u8> {
    let mut o = vec![0u8; p.len()];
    o.par_chunks_mut(4096).zip(p.par_chunks(4096)).zip(m.par_chunks(4096)).for_each(|((d, s), mm)| {
        for (x, (&v, &mg)) in s.iter().zip(mm).enumerate() {
            d[x] = (f64::from(v) * f64::from(mg)).clamp(0.0, 255.0).round() as u8;
        }
    });
    o
}

/// Scale a render by a lens gain for the seam decision, borrowing the original
/// when the gain is unity (the common case: matched lenses) so the normalisation
/// costs nothing then. A 256-entry table keeps the per-pixel work a byte lookup
/// rather than a float multiply.
fn normalise<'a>(p: &'a [u8], g: f64) -> Cow<'a, [u8]> {
    if (g - 1.0).abs() <= 1e-3 {
        return Cow::Borrowed(p);
    }
    let lut: [u8; 256] = std::array::from_fn(|i| (f64::from(i as u32) * g).clamp(0.0, 255.0).round() as u8);
    let mut v = vec![0u8; p.len()];
    v.par_chunks_mut(4096).zip(p.par_chunks(4096)).for_each(|(d, s)| {
        for (o, &x) in d.iter_mut().zip(s) {
            *o = lut[usize::from(x)];
        }
    });
    Cow::Owned(v)
}

/// Studio's `alphaNormalized`: a symmetric power S-curve on the crossfade weight./// Each half is `0.5 * (2t)^LAMBDA`, so the blend lingers near 0 and 1 (one lens
/// owns most of the band) and crosses 50/50 sharply at the middle. `x` is in
/// [0, 1]; the result is the steepened weight in [0, 1].
fn steepstep(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    if x <= 0.5 { 0.5 * (2.0 * x).powf(LAMBDA) } else { 1.0 - 0.5 * (2.0 * (1.0 - x)).powf(LAMBDA) }
}

// ---------------------------------------------------------------------------
// Per-frame seam alignment (the ghost fix)
// ---------------------------------------------------------------------------

/// Rows per parallel task of the band remap: one rayon task per row of a narrow
/// band is dominated by scheduling cost, so rows are grouped into blocks.
const REMAP_ROWS: usize = 32;
/// Largest along-seam shift the band alignment trusts (luma px): the parallax of
/// the closest subject the lenses share in the overlap, beyond which a match is
/// texture noise, not geometry.
const FLOW_MAX_PX: f64 = 6.0;
/// The 2:1 output width whose 30-degree overlap band ([`FEATHER_DEG`] of longitude
/// at [`equirect_size`]'s geometry: `w / 12`) matches the reference lattice. The
/// search blocks, blend windows, graph grids, gain cells and occlusion gate of the
/// seam decision were all tuned there, where the band is this many pixels.
const DECISION_BAND: usize = 426;

/// Upscale for the seam-decision lattice: a band already at or near the reference
/// width decides at its own scale (identity, byte-identical to how the full stitch
/// has always behaved); a narrower one (a proxy, or a downscaled export) is brought
/// up to roughly the reference width, so every length constant in the decision
/// chain means the same ANGLE at every output resolution. Without this, a 92-px
/// proxy band runs 16-px flow blocks across 6 degrees and votes of a few pixels
/// carry ~5x the angle they do in the main file — the preview seam wanders while
/// the exported seam (same source, full resolution) sits elsewhere.
fn decision_scale(bw: usize) -> usize {
    if bw * 2 >= DECISION_BAND {
        return 1;
    }
    DECISION_BAND.div_ceil(bw.max(1)).min(32)
}
/// How much of the previous frame's seam / shift carries over: without this a
/// flickering light inside the band drags the seam back and forth (a crawling
/// edge). 0.75 gives a ~4-frame time constant.
const SMOOTH: f64 = 0.75;
/// The seam-placement window of the graph cut, as a fraction of the band: how far
/// either side of a candidate boundary the cut charges for disagreement (a boundary
/// at `p` crossfades everything near `p`, so `p` pays the worst disagreement in
/// `p`'s window). Wide enough to bridge the agreeing gap between a doubled object's
/// two parallax copies - a person's body is ~90 px at this band width, so the
/// window must reach past half of that for the gap to cost what its edges do.
/// The *blend* ramp the boundary is drawn with is narrower ([`BLEND_RAMP`]): the
/// placement window is a cost model, the blend is what the viewer sees, and a
/// 100-px crossfade smears a subject's sharp edge into a halo even when the seam
/// sits exactly on it (the 16-second artifact: a hard light line along the
/// standing person's flank).
const SEAM_RAMP: f64 = 0.25;
/// The crossfade ramp around the placed seam, as a fraction of the band: sharp
/// enough that a subject's edge stays an edge (the local gain map removes the
/// brightness step the wide ramp used to hide), soft enough that the seam itself
/// never reads as a line.
const BLEND_RAMP: f64 = 0.12;
/// Rows per DP seam node (the seam is smooth vertically) and columns the search
/// grid is resampled to.
const DP_ROW_STRIDE: usize = 8;
const DP_COLS: usize = 160;
/// How strongly the seam is pushed to the side of the lens that does *not* see an
/// occluding object (see [`seam_path`]'s occlusion cue). Tuned on X4 footage: 12.0
/// routes the seam clear of a person the rig occludes in one lens (the seam moves
/// from inside them to their near edge, whole in 90% of frames) without moving a
/// seam that has one clear route (baddie's stays put).
const OCCLUSION_K: f64 = 12.0;
/// The disagreement magnitude (luma) below which the signed lens difference is
/// treated as a lighting asymmetry, not an occlusion, so it does not bias the seam.
/// Above it the cue ramps in over [`OCCLUSION_RAMP`]. Sensor noise in a dark scene
/// sits around 15, so the gate starts there.
const OCCLUSION_GATE: f64 = 15.0;
/// How far above the gate the occlusion cue reaches full strength.
const OCCLUSION_RAMP: f64 = 20.0;
/// Fixed-point scale for seam-cut costs: the float cell costs are rounded to
/// integers times this, so the cut's shortest path (and the reference max-flow of
/// the tests) runs on exact `i64` arithmetic with no comparison drift.
const GC_SCALE: f64 = 64.0;
/// The largest graph the seam cut will build before falling back to the 1-D DP: a
/// hostile (huge) band is refused rather than allocated (see AGENTS §0.4).
const GC_MAX_NODES: usize = 1 << 19;

/// One overlap band's per-frame blend plan: the aligned samples and weights, in
/// luma columns for the luma pass and at the chroma planes' scale for those.
struct BandPlan {
    /// Band start (luma col) and width; `f`/`r` are the front/rear renders,
    /// `a0`/`a1` the 8-bit weights, `inv` 1/(a0+a1) — all `bw` × `h`.
    x0: usize,
    bw: usize,
    f: Vec<u8>,
    r: Vec<u8>,
    a0: Vec<u8>,
    a1: Vec<u8>,
    inv: Vec<f32>,
    /// The per-pixel local colour-gain maps (Studio's calcColorGainMapOnPano),
    /// applied on top of the global `g0`/`g1` in the blend so the lenses' vignetting
    /// gradient cannot show as a light line at the seam. `bw` × `h`, ~1.0.
    gm0: Vec<f32>,
    gm1: Vec<f32>,
    /// The chroma grid: band start/width and height, the aligned renders per
    /// plane, and the weights (shared by U and V).
    cx0: usize,
    cbw: usize,
    ch: usize,
    cf: [Vec<u8>; 2],
    cr: [Vec<u8>; 2],
    ca0: Vec<u8>,
    ca1: Vec<u8>,
    cinv: Vec<f32>,
}

/// Remap one lens's plane over the column range `x0..x1` of the equirect. Rows are
/// grouped into chunks of [`REMAP_ROWS`] so the parallel work has few enough tasks.
fn warp_band(m: &PlaneMap, p: &[u8], x0: usize, x1: usize) -> Vec<u8> {
    let (pw, ph) = (m.pw, m.ph);
    let (x0, x1) = (x0.min(pw), x1.min(pw).max(x0.min(pw)));
    let bw = x1 - x0;
    let mut out = vec![0u8; bw * ph];
    out.par_chunks_mut(bw * REMAP_ROWS).enumerate().for_each(|(ci, block)| {
        let y0 = ci * REMAP_ROWS;
        for (j, row) in block.chunks_mut(bw).enumerate() {
            let y = y0 + j;
            for i in 0..bw {
                let k = y * pw + x0 + i;
                row[i] = if m.w[k] > 0 { sample(p, m.idx[k], m.wgt[k], m.sw).clamp(0, 255) as u8 } else { 0 };
            }
        }
    });
    out
}

/// Shift each row of `src` (stride `sstride`, `swide` wide) by `dx` px tapered by
/// `win` (bilinear, replicate at the edges) into `dst` (`dw` wide), taking `dst`'s
/// columns from `src`'s columns `off..off + dw`.
fn resample_x(src: &[u8], sstride: usize, dw: usize, off: usize, dx: &[f64], win: &[f64], dst: &mut [u8]) {
    dst.par_chunks_mut(dw).enumerate().for_each(|(y, drow)| {
        let dxf = dx.get(y).copied().unwrap_or(0.0).clamp(-(FLOW_MAX_PX * 2.0), FLOW_MAX_PX * 2.0);
        let row = &src[y * sstride..(y + 1) * sstride];
        for (i, d) in drow.iter_mut().enumerate() {
            let sx = (off + i) as f64 + dxf * win.get(i).copied().unwrap_or(1.0);
            let (x0, fx) = (sx.floor(), sx - sx.floor());
            let (x0, x1) = (x0.clamp(0.0, (sstride - 1) as f64) as usize, x0.clamp(0.0, (sstride - 1) as f64) as usize + 1);
            *d = (f64::from(row[x0]) * (1.0 - fx) + f64::from(row[x1.min(sstride - 1)]) * fx).round().clamp(0.0, 255.0) as u8;
        }
    });
}

/// The along-seam shift per row (px) that lands the rear render on the front:
/// a row-wise block match (the shift is a low-frequency horizontal signal; the
/// seam is vertical and the calibrated geometry already aligns rows). Positive
/// means the rear must be sampled further right. Estimated on a 4x-downsampled
/// band (the shift is low-frequency, and averaging kills per-pixel noise), as
/// the median of the block votes per small row, then interpolated back — raw
/// per-pixel matching shreds the band into horizontal slices.
///
/// `hint` is the previous frame's shift per row (Studio's hint flow, §4 of the
/// reverse-engineered flow spec): the block search centres on it, and a row with
/// no locatable texture keeps the hint instead of voting zero (their
/// `UpdateRowsLackOfTexture`). Without this, a dark or featureless band — where
/// most blocks are flat — has its flow dragged toward zero by the flat votes and
/// the alignment decays exactly where it is needed most.
fn flow_dx(f: &[u8], r: &[u8], bw: usize, h: usize, hint: Option<&[f64]>) -> Vec<f64> {
    let s = 4usize;
    let (sw, sh) = (bw / s, h / s);
    if sw < 4 || sh < 1 {
        return vec![0.0; h];
    }
    let mut fs = vec![0u8; sw * sh];
    let mut rs = vec![0u8; sw * sh];
    fs.par_chunks_mut(sw).zip(rs.par_chunks_mut(sw)).enumerate().for_each(|(sy, (fr, rr))| {
        for sx in 0..sw {
            let (mut af, mut ar) = (0u32, 0u32);
            for j in 0..s {
                for i in 0..s {
                    let k = (sy * s + j) * bw + sx * s + i;
                    (af, ar) = (af + u32::from(f[k]), ar + u32::from(r[k]));
                }
            }
            (fr[sx], rr[sx]) = ((af / (s * s) as u32) as u8, (ar / (s * s) as u32) as u8);
        }
    });
    let rad = (FLOW_MAX_PX as usize).div_ceil(s).max(2) as i32;
    let nb = (sw / 8).max(1);
    let bs = sw / nb;
    // The previous frame's shift for a small row, in full px (zero without a hint).
    let hrow = |sy: usize| -> f64 { hint.and_then(|hp| hp.get((sy * s + s / 2).min(h.saturating_sub(1)))).copied().unwrap_or(0.0) };
    let mut small = vec![0f64; sh];
    small.par_iter_mut().enumerate().for_each(|(sy, row)| {
        let hv = hrow(sy);
        // Votes stay within the same absolute bound the resampler clamps to, so a
        // hint-centred search cannot drift the flow without limit across frames.
        let cap = FLOW_MAX_PX * 2.0;
        // The search centre: the hint, in small px. With no hint this is zero and
        // the search is the plain +-rad window it always was.
        let hb = (hv / s as f64).round() as i32;
        let mut votes: Vec<f64> = Vec::with_capacity(nb);
        for bi in 0..nb {
            let x0 = bi * bs;
            // A flat block cannot locate a shift: it keeps the hint (Studio's
            // UpdateRowsLackOfTexture), which is zero on the first frame — so a
            // featureless door still votes no motion, but a dark band that was
            // aligned last frame stays aligned instead of decaying to zero.
            let mean: f64 = (x0..(x0 + bs).min(sw)).map(|x| f64::from(fs[sy * sw + x])).sum::<f64>() / bs as f64;
            let var: f64 = (x0..(x0 + bs).min(sw))
                .map(|x| {
                    let d = f64::from(fs[sy * sw + x]) - mean;
                    d * d
                })
                .sum::<f64>()
                / bs as f64;
            if var < 16.0 {
                votes.push(hv);
                continue;
            }
            let (mut errs, mut best, mut be) = (vec![i64::MAX; (2 * rad + 1) as usize], 0i32, i64::MAX);
            for dd in -rad..=rad {
                let d = hb + dd;
                let mut err = 0i64;
                for x in x0..(x0 + bs).min(sw) {
                    let sx = (x as i32 + d).clamp(0, sw as i32 - 1) as usize;
                    let e = i64::from(fs[sy * sw + x]) - i64::from(rs[sy * sw + sx]);
                    err += e * e;
                }
                errs[(dd + rad) as usize] = err;
                if err < be {
                    (best, be) = (d, err);
                }
            }
            // Parabolic sub-pixel refinement on the SAD curve at the winner's
            // +-1 neighbours (in small px).
            let ei = (best - hb + rad) as usize;
            let mut sub = best as f64;
            if ei > 0 && ei + 1 < errs.len() {
                let den = errs[ei - 1] as f64 - 2.0 * errs[ei] as f64 + errs[ei + 1] as f64;
                if den.abs() > 1e-6 {
                    sub = best as f64 + 0.5 * (errs[ei - 1] as f64 - errs[ei + 1] as f64) / den;
                }
            }
            votes.push((sub * s as f64).clamp(-cap, cap));
        }
        votes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        *row = votes[votes.len() / 2];
    });
    // Interpolate the per-small-row shifts back to full rows, then smooth
    // vertically: the parallax varies slowly with latitude, and a rough shift
    // profile is what makes the seam waver.
    let mut out = vec![0f64; h];
    for y in 0..h {
        let g = (y as f64 + 0.5) / s as f64 - 0.5;
        let g0 = g.clamp(0.0, (sh - 1) as f64);
        let (a, b) = (g0.floor() as usize, g0.floor() as usize + 1);
        let t = g0 - g0.floor();
        out[y] = small[a.min(sh - 1)] * (1.0 - t) + small[b.min(sh - 1)] * t;
    }
    let k = 31usize;
    let mut sm = vec![0f64; h];
    for y in 0..h {
        let (lo, hi) = (y.saturating_sub(k / 2), (y + k / 2 + 1).min(h));
        sm[y] = out[lo..hi].iter().sum::<f64>() / (hi - lo) as f64;
    }
    sm
}

/// Block size (full px) of the 2-D flow grid, and the band downsample the match
/// runs on. The shift is low-frequency and averaging kills per-pixel noise; the
/// coarse level also shrinks the far truss's lattice period below the search
/// radius so the 2-D match cannot alias onto a neighbouring strut.
const FLOW_BS: usize = 16;
const FLOW_S: usize = 4;

/// The 2-D flow search limits (full px): the seam's displacement is mostly vertical
/// (the epipolar lines there are vertical), so vertical gets a wider radius than
/// horizontal, and the rig's baseline projects to a larger offset at high latitude.
fn flow_limits() -> (f64, f64) {
    (FLOW_MAX_PX * 2.0, FLOW_MAX_PX * 4.0)
}

/// Whether the 2-D block flow runs (default on; `FILMCRAFT_STITCH_2D=0` falls back to
/// the 1-D along-seam flow). The 2-D path captures the vertical epipolar shift the
/// 1-D flow misses; it costs no measurable time (decode dominates) and does not
/// regress the doubled-object case, so it is the default.
fn stitch_2d_enabled() -> bool {
    std::env::var("FILMCRAFT_STITCH_2D").map_or(true, |v| !matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "no" | "off"))
}

/// The reference depth the baseline-aware reprojection reconciles the two lens
/// views at, in metres (the inverse feeds [`bake_plane`]). The vendor SDK stitches
/// at a finite depth, so a near object lands at the midpoint of the two lenses'
/// views; treating the rig as co-located (infinity) instead shows one lens's view
/// and leaves near objects ~half their parallax off. Tuned on four X4 scenes against
/// the SDK oracle: 0.3 m puts the womeninthemix chandelier on the SDK's column
/// (165 -> 201 vs 204) and littlebird's standing person's flank on its brightness
/// (22 -> 10.7 vs 11), improves baddie's and the birds' band error, and preserves
/// far-edge sharpness and frame-to-frame stability. `FILMCRAFT_STITCH_DEPTH`
/// overrides it (0 = infinity, the co-located model).
const STITCH_DEPTH_M: f64 = 0.3;
fn stitch_inv_depth() -> f64 {
    let d = std::env::var("FILMCRAFT_STITCH_DEPTH").ok().and_then(|v| v.trim().parse::<f64>().ok()).filter(|d| d.is_finite() && *d >= 0.0);
    match d {
        Some(d) if d > 0.0 => 1.0 / d,
        Some(_) => 0.0,
        None => 1.0 / STITCH_DEPTH_M,
    }
}

/// The rear band render to align onto the front: `data` (stride `stride`), whose
/// columns `off..off + bw` land on the band's columns `0..bw`.
struct RearSrc<'a> {
    data: &'a [u8],
    stride: usize,
    off: usize,
}

/// Align the rear band render onto the front: estimate the shift (1-D along-seam, or
/// 2-D block flow when `FILMCRAFT_STITCH_2D`), seed it from the previous frame's
/// (`prev`), smooth it temporally, and write the warped rear into `r`. Returns the
/// 1-D per-row shift and, in 2-D mode, the flow field (for the next frame's hint and
/// the chroma warp).
fn warp_rear(fg: &[u8], r0g: &[u8], rear: &RearSrc, bw: usize, h: usize, prev: Option<&BandSmooth>, r: &mut [u8]) -> (Vec<f64>, Option<Flow2>) {
    let (rwide, sstride, off) = (rear.data, rear.stride, rear.off);
    // The shift tapers to zero at the band edges (Hanning) so the warped rear meets
    // the unwarped render the static feather uses outside the band.
    let win: Vec<f64> = (0..bw).map(|i| if bw == 1 { 1.0 } else { 0.5 - 0.5 * (std::f64::consts::TAU * (i as f64 + 0.5) / bw as f64).cos() }).collect();
    let mut dx = flow_dx(fg, r0g, bw, h, prev.map(|p| p.dx.as_slice()));
    if let Some(p) = prev
        && p.dx.len() == dx.len()
    {
        for (d, &pd) in dx.iter_mut().zip(&p.dx) {
            *d = SMOOTH * pd + (1.0 - SMOOTH) * *d;
        }
    }
    if !stitch_2d_enabled() {
        resample_x(rwide, sstride, bw, off, &dx, &win, r);
        return (dx, None);
    }
    let mut f2 = flow_2d(fg, r0g, bw, h, prev.and_then(|p| p.flow.as_ref()));
    if let Some(p) = prev.and_then(|p| p.flow.as_ref())
        && p.dxs.len() == f2.dxs.len()
    {
        for ((d, &pd), (e, &pe)) in f2.dxs.iter_mut().zip(&p.dxs).zip(f2.dys.iter_mut().zip(&p.dys)) {
            *d = SMOOTH * pd + (1.0 - SMOOTH) * *d;
            *e = SMOOTH * pe + (1.0 - SMOOTH) * *e;
        }
    }
    resample_2d(rwide, sstride, bw, off, &f2, &win, r);
    (dx, Some(f2))
}

/// A 2-D block flow field: `dxs`/`dys` (luma px) on a `gx`×`gy` grid whose blocks
/// are `bsx`×`bsy` destination px apart, with `sx`/`sy` scaling the magnitudes to
/// destination px. The luma field has unit scale and `flow_bs()` spacing; the
/// chroma field reuses the same grid with the spacing and magnitudes divided by
/// the subsampling, so luma and chroma warp by the same physical shift.
#[derive(Clone)]
struct Flow2 {
    dxs: Vec<f64>,
    dys: Vec<f64>,
    gx: usize,
    gy: usize,
    bsx: f64,
    bsy: f64,
    sx: f64,
    sy: f64,
}

/// The 2-D shift per block (px) that lands the rear render on the front, on a
/// `gx`×`gy` grid. At the seam (lon ±90) the epipolar lines are vertical (great
/// circles through the front/rear epipoles), so the inter-lens displacement there
/// is vertical: a 1-D along-seam shift misses it and leaves the far background (a
/// truss) misaligned, which pins the seam to a near subject's near edge instead of
/// routing around it. Block-matching in 2-D captures the vertical component.
/// Estimated on a [`FLOW_S`]-downsampled band as the per-block SAD minimum over a
/// search window centred on the previous frame's flow (the hint), median-filtered
/// 3×3 (a lone bad block must not shred the field), then returned in full px. A
/// flat block cannot locate a shift and keeps the hint (Studio's
/// `UpdateRowsLackOfTexture`).
fn flow_2d(f: &[u8], r: &[u8], bw: usize, h: usize, hint: Option<&Flow2>) -> Flow2 {
    let s = FLOW_S;
    let bs_full = FLOW_BS;
    let (sw, sh) = (bw / s, h / s);
    let gx = (bw / bs_full).max(1);
    let gy = (h / bs_full).max(1);
    let zero = |gx, gy| Flow2 { dxs: vec![0.0; gx * gy], dys: vec![0.0; gx * gy], gx, gy, bsx: bs_full as f64, bsy: bs_full as f64, sx: 1.0, sy: 1.0 };
    if sw < 4 || sh < 4 {
        return zero(gx, gy);
    }
    let mut fs = vec![0u8; sw * sh];
    let mut rs = vec![0u8; sw * sh];
    fs.par_chunks_mut(sw).zip(rs.par_chunks_mut(sw)).enumerate().for_each(|(sy, (fr, rr))| {
        for sx in 0..sw {
            let (mut af, mut ar) = (0u32, 0u32);
            for j in 0..s {
                for i in 0..s {
                    let k = (sy * s + j) * bw + sx * s + i;
                    (af, ar) = (af + u32::from(f[k]), ar + u32::from(r[k]));
                }
            }
            (fr[sx], rr[sx]) = ((af / (s * s) as u32) as u8, (ar / (s * s) as u32) as u8);
        }
    });
    // The block is FLOW_BS/s small px; the search is +-rad small px (clamped to the
    // absolute bound the resampler trusts). Vertical gets a wider radius than
    // horizontal: the seam's displacement is mostly vertical and the rig's baseline
    // projects to a larger vertical offset at high latitude.
    let bs = (bs_full / s).max(2);
    let (lim_x, lim_y) = flow_limits();
    let rad_x = ((lim_x as usize) / s).clamp(2, bs * 3) as i32;
    let rad_y = ((lim_y as usize) / s).clamp(2, bs * 3) as i32;
    let mut dxs = vec![0f64; gx * gy];
    let mut dys = vec![0f64; gx * gy];
    dxs.par_chunks_mut(gx).zip(dys.par_chunks_mut(gx)).enumerate().for_each(|(by, (dxr, dyr))| {
        for bx in 0..gx {
            let (sy, sx) = (by * bs, bx * bs);
            if sy + bs > sh || sx + bs > sw {
                continue;
            }
            // The hint for this block, in small px (zero without one, or when the
            // grid changed size between frames).
            let (hx, hy) = hint.filter(|p| p.gx == gx && p.gy == gy).map_or((0.0, 0.0), |p| {
                let i = (by * gx + bx).min(p.dxs.len().saturating_sub(1));
                (p.dxs.get(i).copied().unwrap_or(0.0) / s as f64, p.dys.get(i).copied().unwrap_or(0.0) / s as f64)
            });
            let (mut mean, mut var) = (0f64, 0f64);
            for j in 0..bs {
                for i in 0..bs {
                    mean += f64::from(fs[(sy + j) * sw + sx + i]);
                }
            }
            mean /= (bs * bs) as f64;
            for j in 0..bs {
                for i in 0..bs {
                    let d = f64::from(fs[(sy + j) * sw + sx + i]) - mean;
                    var += d * d;
                }
            }
            var /= (bs * bs) as f64;
            if var < 16.0 {
                (dxr[bx], dyr[bx]) = (hx * s as f64, hy * s as f64);
                continue;
            }
            let (hb_x, hb_y) = (hx.round() as i32, hy.round() as i32);
            let (mut best, mut be) = ((0i32, 0i32), i64::MAX);
            for dy in hb_y - rad_y..=hb_y + rad_y {
                for dx in hb_x - rad_x..=hb_x + rad_x {
                    let (py, px) = (sy as i32 + dy, sx as i32 + dx);
                    if py < 0 || px < 0 || py + bs as i32 > sh as i32 || px + bs as i32 > sw as i32 {
                        continue;
                    }
                    let (py, px) = (py as usize, px as usize);
                    let mut err = 0i64;
                    for j in 0..bs {
                        for i in 0..bs {
                            err += (i64::from(fs[(sy + j) * sw + sx + i]) - i64::from(rs[(py + j) * sw + px + i])).abs();
                        }
                    }
                    if err < be {
                        (best, be) = ((dx, dy), err);
                    }
                }
            }
            (dxr[bx], dyr[bx]) = (f64::from(best.0) * s as f64, f64::from(best.1) * s as f64);
        }
    });
    // Median-filter the field 3x3 so a lone mismatched block cannot tear the warp.
    let (mut mdx, mut mdy) = (vec![0f64; gx * gy], vec![0f64; gx * gy]);
    for by in 0..gy {
        for bx in 0..gx {
            let (mut vx, mut vy) = (Vec::with_capacity(9), Vec::with_capacity(9));
            for j in by.saturating_sub(1)..=(by + 1).min(gy - 1) {
                for i in bx.saturating_sub(1)..=(bx + 1).min(gx - 1) {
                    vx.push(dxs[j * gx + i]);
                    vy.push(dys[j * gx + i]);
                }
            }
            vx.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            vy.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            (mdx[by * gx + bx], mdy[by * gx + bx]) = (vx[vx.len() / 2], vy[vy.len() / 2]);
        }
    }
    Flow2 { dxs: mdx, dys: mdy, gx, gy, bsx: bs_full as f64, bsy: bs_full as f64, sx: 1.0, sy: 1.0 }
}

/// Warp `src` (stride `sstride`) onto the front by the 2-D block flow `f`,
/// bilinearly interpolating the flow to each pixel and tapering it to zero at the
/// band's left/right edges (Hanning) so the warped rear meets the unwarped render
/// the static feather uses outside the band. Takes `dst`'s columns from `src`'s
/// columns `off..off + dw`.
fn resample_2d(src: &[u8], sstride: usize, dw: usize, off: usize, f: &Flow2, win: &[f64], dst: &mut [u8]) {
    let (gx, gy) = (f.gx, f.gy);
    if gx == 0 || gy == 0 {
        return;
    }
    let (dxs, dys) = (&f.dxs[..], &f.dys[..]);
    let (bw, h) = (dw, dst.len() / dw.max(1));
    let (bsx, bsy) = (f.bsx, f.bsy);
    dst.par_chunks_mut(bw).enumerate().for_each(|(y, drow)| {
        let by = (y as f64 / bsy).clamp(0.0, (gy - 1) as f64);
        let (b0, b1) = (by.floor() as usize, (by.floor() as usize + 1).min(gy - 1));
        let fy = by - by.floor();
        for (i, d) in drow.iter_mut().enumerate() {
            let w = win.get(i).copied().unwrap_or(1.0);
            let bx = (i as f64 / bsx).clamp(0.0, (gx - 1) as f64);
            let (x0, x1) = (bx.floor() as usize, (bx.floor() as usize + 1).min(gx - 1));
            let fx = bx - bx.floor();
            let g = |a: usize, b: usize, fld: &[f64]| fld[a * gx + b];
            // Interpolate in luma px, clamp there, then scale to this plane's px.
            let (lim_x, lim_y) = flow_limits();
            let dx = (((g(b0, x0, dxs) * (1.0 - fx) + g(b0, x1, dxs) * fx) * (1.0 - fy) + (g(b1, x0, dxs) * (1.0 - fx) + g(b1, x1, dxs) * fx) * fy)
                .clamp(-lim_x, lim_x))
                * f.sx;
            let dy = (((g(b0, x0, dys) * (1.0 - fx) + g(b0, x1, dys) * fx) * (1.0 - fy) + (g(b1, x0, dys) * (1.0 - fx) + g(b1, x1, dys) * fx) * fy)
                .clamp(-lim_y, lim_y))
                * f.sy;
            let (dx, dy) = (dx * w, dy * w);
            let sx = (off + i) as f64 + dx;
            let sy = y as f64 + dy;
            let (sx0, sfx) = (sx.floor(), sx - sx.floor());
            let (sy0, sfy) = (sy.floor(), sy - sy.floor());
            let (sx0, sx1) = (sx0.clamp(0.0, (sstride - 1) as f64) as usize, sx0.clamp(0.0, (sstride - 1) as f64) as usize + 1);
            let (sy0, sy1) = (sy0.clamp(0.0, (h - 1) as f64) as usize, sy0.clamp(0.0, (h - 1) as f64) as usize + 1);
            let (sy0, sy1) = (sy0.min(h - 1), sy1.min(h - 1));
            let p = |yy: usize, xx: usize| f64::from(src[yy * sstride + xx.min(sstride - 1)]);
            *d = (p(sy0, sx0) * (1.0 - sfx) * (1.0 - sfy) + p(sy0, sx1) * sfx * (1.0 - sfy) + p(sy1, sx0) * (1.0 - sfx) * sfy + p(sy1, sx1) * sfx * sfy)
                .round()
                .clamp(0.0, 255.0) as u8;
        }
    });
}

/// The seam decision grid: the per-node cost surface (mean disagreement plus the
/// centre prior, occlusion cue and coverage penalty) that both the
/// 1-D DP ([`seam_path`]) and the 2-D graph cut ([`seam_cut`]) minimise. Shared so
/// the two agree on cost and only differ in how they route the boundary.
///
/// The occlusion cue decides WHICH side of a subject the seam routes to: for a
/// subject the rig occludes in one lens both sides cost about the same by
/// disagreement alone, so it can settle on the wrong one and hold a cut through
/// them. Where
/// one lens is brighter than the other over a wide run (the object is in front of
/// the darker lens's view of the background, i.e. the darker lens is *occluded*
/// there), the seam should pass on the occluded lens's side of the object, so the
/// object comes wholly from the lens that actually sees it. The cue is the running
/// sum of the (mean-subtracted, disagreement-gated) signed lens difference, so it
/// is flat over agreement and ramps across an occluding object, and it is
/// antisymmetric between the two bands (each band's front lens is on the opposite
/// side), which `front_rises` accounts for.
struct SeamGrid {
    /// `nodes * cols` float costs (row-major, node row `ni`, column `ci`).
    grid: Vec<f64>,
    /// `nodes * cols` raw mean disagreement (no priors), the part [`seam_cut`]
    /// spreads over a blend window so a doubled object's gap is not free.
    dis: Vec<f64>,
    /// Per node (`nodes * cols`) which lenses see it: bit 0 = the front
    /// lens, bit 1 = the rear. A node only one lens covers is forced to it
    /// by [`seam_cut`].
    cover: Vec<u8>,
    /// Per node (`nodes * cols`) which lenses see it: bit 0 = the front lens, bit 1 =
    cols: usize,
    nodes: usize,
    stride: usize,
}

/// `floor` subtracts each row block's disagreement noise floor (see below): the
/// proxy decisions run it, where the floor dominates; the reference-scale path
/// must reproduce the tuning it was verified at, noise included.
fn seam_grid_with_floor(f: &[u8], r: &[u8], m: &[PlaneMap; 2], b: &Band, bw: usize, h: usize, floor_on: bool) -> SeamGrid {
    let stride = (bw / DP_COLS).max(1);
    let cols = bw.div_ceil(stride).max(2);
    let covered: Vec<bool> = (0..cols)
        .map(|ci| (0..h).step_by(DP_ROW_STRIDE.max(1)).any(|y| m[0].w[y * m[0].pw + b.x0 + ci * stride] > 0 && m[1].w[y * m[1].pw + b.x0 + ci * stride] > 0))
        .collect();
    // Per-node cost: mean abs disagreement over the row block, plus priors.
    let nodes = (h / DP_ROW_STRIDE).max(1);
    let mut grid = vec![0f64; nodes * cols];
    let mut dis = vec![0f64; nodes * cols];
    // Which lens sees each node, for the cut's coverage forcing.
    let mut cover = vec![0u8; nodes * cols];
    grid.par_chunks_mut(cols).zip(dis.par_chunks_mut(cols)).zip(cover.par_chunks_mut(cols)).enumerate().for_each(|(ni, ((grow, drow), crow))| {
        let y0 = ni * DP_ROW_STRIDE;
        // Per-column mean disagreement and signed (right-lens minus left-lens)
        // difference over the row block, for the occlusion cue. The signed
        // difference is mean-subtracted over the covered columns (so a global
        // exposure offset between the lenses, already handled by the gain, does
        // not bias it) and gated by the disagreement (so a lighting asymmetry on
        // agreeing content, not an occlusion, does not either).
        let mut dm = vec![0f64; cols];
        let mut sgn = vec![0f64; cols];
        let mut cov = vec![false; cols];
        for ci in 0..cols {
            let x = (ci * stride + stride / 2).min(bw - 1);
            let (mut e, mut s, mut n) = (0i64, 0i64, 0u32);
            let (mut c0, mut c1, mut cb) = (false, false, false);
            for y in y0..(y0 + DP_ROW_STRIDE).min(h) {
                let (fv, rv) = (i64::from(f[y * bw + x]), i64::from(r[y * bw + x]));
                e += (fv - rv).abs();
                s += rv - fv;
                n += 1;
                let (v0, v1) = (m[0].w[y * m[0].pw + b.x0 + x] > 0, m[1].w[y * m[1].pw + b.x0 + x] > 0);
                c0 |= v0;
                c1 |= v1;
                cb |= v0 && v1;
            }
            dm[ci] = e as f64 / n.max(1) as f64;
            sgn[ci] = s as f64 / n.max(1) as f64;
            cov[ci] = cb;
            crow[ci] = u8::from(c0) | (u8::from(c1) << 1);
        }
        let mean = {
            let (mut ss, mut nn) = (0f64, 0f64);
            for ci in 0..cols {
                if cov[ci] {
                    ss += sgn[ci];
                    nn += 1.0;
                }
            }
            if nn > 0.0 { ss / nn } else { 0.0 }
        };
        // The disagreement noise floor of this row block: the 10th percentile of
        // the column disagreements where both lenses cover. Two 8-bit encodes of
        // the same scene differ by sensor/codec noise even where they see the same
        // thing, and the floor rises as the picture loses resolution. Subtracting
        // it makes "the lenses disagree" mean CONTENT again at any scale, the way
        // `OCCLUSION_GATE`'s fixed threshold does at reference; the percentile (not
        // the median) leaves a doubled subject covering half the band alone, its
        // two edges still showing above the floor. A uniform shift within the row
        // is neutral to the cut's route; the zero clamp after it is what erases
        // agreement-with-noise into agreement.
        let floor = {
            let mut v: Vec<f64> = cov.iter().zip(&dm).filter(|(c, _)| **c).map(|(_, d)| *d).collect();
            if !floor_on || v.is_empty() {
                0.0
            } else {
                v.sort_by(|a, b| a.total_cmp(b));
                v[(v.len() - 1) / 10]
            }
        };
        // The running sum of the gated signed difference: flat where the lenses
        // agree, ramping across an occluding object. `f` is always the front lens
        // and `r` the rear. The front lens owns the high columns in band 0
        // (`front_rises`) and the low columns in band 1, so an object the front
        // lens is occluded by (rear brighter, `r - f > 0`) wants the seam on the
        // rear's side of it - a low column in band 1, a high column in band 0.
        // Minimising the running sum of `r - f` gives the low column; of `f - r`,
        // the high column. `front_rises` picks the sign so the seam always lands
        // on the side where the object comes wholly from the lens that sees it.
        //
        // The sum only accumulates where BOTH lenses cover: at a band's outer edge
        // one lens is black, so `r - f` is a huge false "difference" there (pure
        // lens boundary, not occlusion), and letting it into the running sum drags
        // the seam to the wrong side of a subject near the middle.
        let mut cum = 0f64;
        let mut occ = vec![0f64; cols];
        for ci in 0..cols {
            if cov[ci] {
                let gate = ((dm[ci] - floor - OCCLUSION_GATE) / OCCLUSION_RAMP).clamp(0.0, 1.0);
                let signed = if b.front_rises { mean - sgn[ci] } else { sgn[ci] - mean };
                cum += gate * signed;
            }
            occ[ci] = cum / cols.max(1) as f64;
        }
        for ci in 0..cols {
            // Mean abs disagreement over the row block AND a +-2-node column
            // window: the raw per-pixel surface is noisy, and a rough surface
            // is what makes the placed seam waver.
            let mut e = 0u64;
            let mut n = 0u64;
            for dc in -2i32..=2 {
                let cj = ci as i32 + dc;
                if cj < 0 || cj as usize >= cols {
                    continue;
                }
                let x = (cj as usize * stride + stride / 2).min(bw - 1);
                for y in y0..(y0 + DP_ROW_STRIDE).min(h) {
                    e += (i64::from(f[y * bw + x]) - i64::from(r[y * bw + x])).unsigned_abs();
                    n += 1;
                }
            }
            let mut c = (e as f64 / n.max(1) as f64 - floor).max(0.0);
            let dis = c;
            let mid = (cols - 1) as f64 / 2.0;
            c += 4.0 * ((ci as f64 - mid) / mid.max(1.0)).powi(2);
            c += OCCLUSION_K * occ[ci];
            if !covered[ci] {
                c += 32.0;
            }
            grow[ci] = c;
            drow[ci] = dis;
        }
    });
    SeamGrid { grid, dis, cover, cols, nodes, stride }
}

/// The seam column per row (band-local): the cheapest top-to-bottom path through
/// the two renders' disagreement (the 1-D case of [`seam_cut`]'s graph), via a
/// Viterbi with a ±1-column continuity limit over [`seam_grid`]'s cost surface.
fn seam_path(g: &SeamGrid, bw: usize, h: usize) -> Vec<f64> {
    let (cols, stride) = (g.cols, g.stride);
    let nodes = h / DP_ROW_STRIDE;
    if nodes == 0 || cols < 3 {
        return vec![bw as f64 / 2.0; h];
    }
    let grid = &g.grid;
    let mut dp = grid[..cols].to_vec();
    let mut back = vec![0i8; grid.len()];
    for ni in 1..nodes {
        let (mut ndp, mut nback) = (vec![f64::MAX; cols], vec![0i8; cols]);
        for ci in 0..cols {
            for d in [-1i32, 0, 1] {
                let pj = ci as i32 + d;
                if pj < 0 || pj as usize >= cols {
                    continue;
                }
                let v = dp[pj as usize] + (if d == 0 { 0.0 } else { 0.5 });
                if v < ndp[ci] {
                    ndp[ci] = v;
                    nback[ci] = d as i8;
                }
            }
        }
        let row = ni * cols;
        for ci in 0..cols {
            dp[ci] = ndp[ci] + grid.get(row + ci).copied().unwrap_or(0.0);
            back[row + ci] = nback[ci];
        }
    }
    let (lo, hi) = (1usize, cols - 2);
    let mut path = vec![0f64; h];
    let mut ci =
        lo + dp[lo..=hi].iter().enumerate().min_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal)).map(|(i, _)| i).unwrap_or(cols / 2);
    for ni in (0..nodes).rev() {
        let col = (ci * stride + stride / 2) as f64;
        for y in (ni * DP_ROW_STRIDE).min(h)..((ni + 1) * DP_ROW_STRIDE).min(h) {
            path[y] = col;
        }
        ci = (ci as i32 + i32::from(back.get(ni * cols + ci).copied().unwrap_or(0))).clamp(0, cols as i32 - 1) as usize;
    }
    path
}

/// A small Dinic max-flow / min-cut on `i64` capacities.
///
/// Seam cuts themselves are solved by [`seam_cut`] as a dual shortest path, so
/// this engine only serves the `flow_tests` module: it is the independent
/// reference (brute force, and the seam-shaped ladder equivalence test) that
/// keeps the dual formulation honest.
/// Nodes are `0..n`; the source is `0` and the sink is `1`. Edges are stored as a
/// flat adjacency of `(to, rev, cap)` so the residual graph needs no `unsafe` and no
/// pointer aliasing. Every capacity is a non-negative integer, so the max-flow is
/// exact and the min-cut it induces is a genuine partition (AGENTS §0.4: no float
/// arithmetic in the residual graph).
#[cfg(test)]
struct MaxFlow {
    n: usize,
    /// `adj[v]` is the list of edge indices leaving `v`.
    adj: Vec<Vec<usize>>,
    /// Edge endpoints and residual capacity; the reverse of edge `i` is `i ^ 1`.
    to: Vec<usize>,
    cap: Vec<i64>,
}

#[cfg(test)]
impl MaxFlow {
    fn new(n: usize) -> Self {
        Self { n, adj: vec![Vec::new(); n], to: Vec::new(), cap: Vec::new() }
    }

    /// Add a directed edge `v -> w` with residual capacity `c` (and its zero twin).
    fn add_edge(&mut self, v: usize, w: usize, c: i64) {
        let (a, b) = (self.to.len(), self.to.len() + 1);
        self.to.extend_from_slice(&[w, v]);
        self.cap.extend_from_slice(&[c, 0]);
        if let Some(l) = self.adj.get_mut(v) {
            l.push(a);
        }
        if let Some(l) = self.adj.get_mut(w) {
            l.push(b);
        }
    }

    /// Max flow / min cut by highest-label push–relabel with gap detection and
    /// periodic (dual-BFS) global relabels: Goldberg–Tarjan with the
    /// Cherkassky–Goldberg engineering. Kept as a second reference solver
    /// beside [`dinic_flow`](Self::dinic_flow): random graphs are checked
    /// against enumeration, and against each other, on the seam-shaped ladder.
    /// Until this revision it was the production seam solver, but on a seam
    /// graph it pays Θ(pushes), and pushes scale with the snake length of the
    /// residual paths (the terminals are pinned to opposite band edges): on the
    /// ~79k-cell proxy lattice that measured seconds per frame, where the dual
    /// shortest path in [`seam_cut`] gets the same cut in milliseconds.
    fn max_flow(&mut self, s: usize, t: usize) -> i64 {
        let n = self.n;
        let mut h = vec![0u32; n];
        let mut ex = vec![0i64; n];
        let mut cur = vec![0usize; n];
        // Buckets of nodes with excess by height; stale entries (height moved
        // under them) are re-homed at pop.
        let mut bl: Vec<Vec<usize>> = vec![Vec::new(); 2 * n + 3];
        let mut act = vec![false; n];
        let mut maxh = 0usize;
        let mut cnt = vec![0u32; 2 * n + 3]; // nodes per height (for gaps)

        // Preflow: saturate the source's arcs.
        h[s] = n as u32;
        for k in 0..self.adj[s].len() {
            let e = self.adj[s][k];
            let c = self.cap[e];
            if c > 0 {
                let w = self.to[e];
                self.cap[e] = 0;
                self.cap[e ^ 1] += c;
                ex[w] += c;
                ex[s] -= c;
            }
        }

        // Global relabel (Cherkassky–Goldberg): heights = exact residual distance
        // to the sink where one exists (reverse BFS; the reverse of positive edge
        // `v -> w` is its twin `e ^ 1`), and for the rest the distance BACK to the
        // source, offset by n — so quarantined excess is one admissible push per
        // hop from home, not a relabel grind (without this the gap-lifted swarm
        // sat at a flat n+1 and crexed for minutes on big bands).
        let gr = |mf: &mut Self, h: &mut [u32], cnt: &mut [u32]| {
            cnt.iter_mut().for_each(|c| *c = 0);
            h.fill(u32::MAX);
            h[t] = 0;
            cnt[0] += 1;
            let mut q = std::collections::VecDeque::from([t]);
            while let Some(w) = q.pop_front() {
                for k in 0..mf.adj[w].len() {
                    let e = mf.adj[w][k];
                    let v = mf.to[e];
                    if mf.cap[e ^ 1] > 0 && h[v] == u32::MAX {
                        h[v] = h[w] + 1;
                        q.push_back(v);
                    }
                }
            }
            // Anything the sink cannot see returns to the source: second reverse
            // BFS from it, depths offset by n, capped at the label bound.
            let mut q = std::collections::VecDeque::from([(s, 0u32)]);
            h[s] = n as u32;
            while let Some((w, d)) = q.pop_front() {
                for k in 0..mf.adj[w].len() {
                    let e = mf.adj[w][k];
                    let v = mf.to[e];
                    if mf.cap[e ^ 1] > 0 && h[v] == u32::MAX {
                        h[v] = (n as u32 + 1 + d).min(2 * n as u32 + 1);
                        q.push_back((v, d + 1));
                    }
                }
            }
            let dead = 2 * n as u32 + 1; // no excess lives here; discharge never sees them
            for hv in h.iter_mut() {
                if *hv == u32::MAX {
                    *hv = dead;
                }
                cnt[*hv as usize] += 1;
            }
        };
        gr(self, &mut h, &mut cnt);

        let enqueue = |v: usize, h: &[u32], act: &mut [bool], bl: &mut [Vec<usize>], maxh: &mut usize| {
            if !act[v] {
                act[v] = true;
                let hv = h[v] as usize;
                bl[hv].push(v);
                *maxh = (*maxh).max(hv);
            }
        };
        for v in 0..n {
            if v != s && v != t && ex[v] > 0 {
                enqueue(v, &h, &mut act, &mut bl, &mut maxh);
            }
        }

        // Periodic global relabel once this many edge inspections accrue.
        let (mut work, refresh) = (0usize, (self.to.len() / 2).max(256));

        while maxh > 0 {
            while bl[maxh].is_empty() {
                maxh -= 1;
                if maxh == 0 {
                    break;
                }
            }
            if maxh == 0 {
                break;
            }
            let u = bl[maxh].pop().expect("bucket non-empty");
            if !act[u] || h[u] as usize != maxh || ex[u] <= 0 {
                if h[u] as usize != maxh && ex[u] > 0 && u != s && u != t {
                    act[u] = false;
                    enqueue(u, &h, &mut act, &mut bl, &mut maxh);
                } else {
                    act[u] = false;
                }
                continue;
            }
            act[u] = false;
            // Discharge u completely: push along admissible arcs, relabel (with
            // gap) when its arc list runs out.
            while ex[u] > 0 {
                if cur[u] >= self.adj[u].len() {
                    let old = h[u];
                    let mut mh = u32::MAX;
                    for &e in &self.adj[u] {
                        if self.cap[e] > 0 {
                            mh = mh.min(h[self.to[e]]);
                        }
                    }
                    cnt[old as usize] -= 1;
                    let nh = if cnt[old as usize] == 0 && (old as usize) < n + 1 {
                        // Gap: this level is empty, so every level strictly
                        // between it and the source end is cut off from the
                        // sink. Lift all of them (their excess runs back at
                        // the source, which sits at height n).
                        let dead = n as u32 + 1;
                        for v in 0..n {
                            let k = h[v];
                            if v != s && v != t && k > old && (k as usize) < n + 1 {
                                cnt[k as usize] -= 1;
                                cnt[dead as usize] += 1;
                                h[v] = dead;
                                cur[v] = 0;
                            }
                        }
                        dead
                    } else {
                        mh.saturating_add(1).min(2 * n as u32 + 1)
                    };
                    h[u] = nh;
                    cnt[nh as usize] += 1;
                    cur[u] = 0;
                    work += self.adj[u].len().max(1);
                } else {
                    let e = self.adj[u][cur[u]];
                    let v = self.to[e];
                    if self.cap[e] > 0 && h[u] == h[v] + 1 {
                        let d = ex[u].min(self.cap[e]);
                        self.cap[e] -= d;
                        self.cap[e ^ 1] += d;
                        ex[u] -= d;
                        ex[v] += d;
                        if v != s && v != t {
                            enqueue(v, &h, &mut act, &mut bl, &mut maxh);
                        }
                    } else {
                        cur[u] += 1;
                        work += 1;
                    }
                }
            }
            if work > refresh {
                gr(self, &mut h, &mut cnt);
                cur.iter_mut().for_each(|c| *c = 0);
                for b in bl.iter_mut() {
                    b.clear();
                }
                act.iter_mut().for_each(|a| *a = false);
                maxh = 0;
                for v in 0..n {
                    if v != s && v != t && ex[v] > 0 {
                        enqueue(v, &h, &mut act, &mut bl, &mut maxh);
                    }
                }
                work = 0;
            }
        }
        ex[t]
    }

    /// The reference solver the push–relabel flow above is tested against.
    fn dinic_flow(&mut self, s: usize, t: usize) -> i64 {
        let (mut level, mut it) = (vec![-1i32; self.n], vec![0usize; self.n]);
        let mut total = 0i64;
        while self.dinic_bfs(s, t, &mut level) {
            it.iter_mut().for_each(|i| *i = 0);
            loop {
                let pushed = self.dinic_dfs(s, t, i64::MAX, &level, &mut it);
                if pushed == 0 {
                    break;
                }
                total = total.saturating_add(pushed);
            }
        }
        total
    }

    fn dinic_bfs(&self, s: usize, t: usize, level: &mut [i32]) -> bool {
        level.iter_mut().for_each(|l| *l = -1);
        level[s] = 0;
        let mut q = std::collections::VecDeque::from([s]);
        while let Some(v) = q.pop_front() {
            for &e in &self.adj[v] {
                if self.cap[e] > 0 {
                    let w = self.to[e];
                    if level[w] < 0 {
                        level[w] = level[v] + 1;
                        q.push_back(w);
                    }
                }
            }
        }
        level[t] >= 0
    }

    fn dinic_dfs(&mut self, v: usize, t: usize, f: i64, level: &[i32], it: &mut Vec<usize>) -> i64 {
        if v == t || f == 0 {
            return f;
        }
        while it[v] < self.adj[v].len() {
            let e = self.adj[v][it[v]];
            let w = self.to[e];
            if self.cap[e] > 0 && level[w] == level[v] + 1 {
                let pushed = self.dinic_dfs(w, t, f.min(self.cap[e]), level, it);
                if pushed > 0 {
                    self.cap[e] -= pushed;
                    self.cap[e ^ 1] += pushed;
                    return pushed;
                }
            }
            it[v] += 1;
        }
        0
    }

    /// The set reachable from `s` in the residual graph after `max_flow`: the
    /// source side of the min-cut.
    fn min_cut_side(&self, s: usize) -> Vec<bool> {
        let mut seen = vec![false; self.n];
        seen[s] = true;
        let mut q = std::collections::VecDeque::from([s]);
        while let Some(v) = q.pop_front() {
            for &e in &self.adj[v] {
                let w = self.to[e];
                if self.cap[e] > 0 && !seen[w] {
                    seen[w] = true;
                    q.push_back(w);
                }
            }
        }
        seen
    }
}

/// The 2-D seam: a min-cut over [`seam_grid`]'s cost surface that assigns each grid
/// node wholly to the front or the rear lens, generalising [`seam_path`]'s per-row
/// 1-D path to a boundary that can bulge *around* a compact disagreement blob in both
/// axes (Studio's `GraphCutSeamFinder`, clean-room).
///
/// The graph is the node-capacity ("Orloff-Stern") form of seam finding: every node
/// `p` splits into `p_in -> p_out` with capacity equal to `p`'s cost from
/// [`seam_grid`] (its disagreement plus the centre prior, occlusion cue and
/// coverage penalty), so a boundary that passes *through* `p` pays that
/// cost; neighbours are joined by high-capacity edges so the boundary prefers to
/// cross node costs (routing around a blob) rather than fray between them; and the
/// two single-coverage edges of the band are pinned to their lens (an infinite
/// terminal edge), which forces a connected top-to-bottom seam between them. The
/// source side is the rear lens's region and the sink side the front's, so the
/// pinned columns decide which end is which (`front_rises`).
///
/// Returns the label grid (`nodes * cols`, 1 = the front lens owns the node) and the
/// per-row seam column (band-local) read off it, or `None` when the band is too
/// small or too large for a bounded graph (the caller then keeps the 1-D DP result).
///
/// `exact` solves the cut's plateau structure faithfully - the interval dual
/// below, matching the reference graph's min-cut side cell for cell; it costs
/// O(nodes * cols^2). The upscaled decision lattice passes `false`: there the
/// chain dual (one boundary cell per row, the interval family's diagonal) is
/// ~10x cheaper, and its difference from the exact side lives inside
/// exactly-equal costs that the proxy's wider blend.
fn seam_cut(g: &SeamGrid, b: &Band, bw: usize, h: usize, exact: bool) -> Option<(Vec<u8>, Vec<f64>)> {
    let (cols, nodes, stride) = (g.cols, g.nodes, g.stride);
    if nodes == 0 || cols < 3 {
        return None;
    }
    let cells = nodes.checked_mul(cols)?;
    if cells.checked_mul(2)?.checked_add(2)? > GC_MAX_NODES {
        return None;
    }
    // The boundary cost. A seam through a node crossfades everything within half a
    // blend window of it, so the *disagreement* a boundary at `p` must pay is the
    // worst in `p`'s window, not just `p`'s own: without this the cut routes through
    // the low-disagreement GAP between a doubled object's two parallax copies (both
    // copies then show, one per lens) exactly as the 1-D DP does; with it the gap is
    // expensive (each copy is inside the other's window) and the boundary snaps to
    // the edge of the disagreement, giving the object wholly to one lens. The
    // position-specific priors (centre, occlusion cue, coverage) are NOT
    // windowed - they say which side and where, so they stay on the node the
    // boundary actually sits on.
    let base = (bw as f64 * SEAM_RAMP).max(4.0);
    let win = ((base / 2.0 / stride as f64) as usize).max(1);
    let mut cap = vec![0f64; cells];
    for ni in 0..nodes {
        let (drow, grow) = (&g.dis[ni * cols..(ni + 1) * cols], &g.grid[ni * cols..(ni + 1) * cols]);
        // Sliding window maximum of the disagreement over +-win columns, read at
        // the column *centred* in the window. The deque holds candidates for the
        // window [ci - 2 * shift, ci]; its centre is `ci - shift`, and the loop
        // runs `shift` phantom columns past the right edge to give the last real
        // columns the same truncated window the first columns get on their side:
        // the same disagreement costs the same whether it lies left or right of
        // the candidate seam. (A narrow band narrows the shift so every column
        // is still evaluated once with its widest possible window.)
        let shift = win.min(cols - 1);
        let mut dq: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
        for ci in 0..cols + shift {
            while dq.front().is_some_and(|&front| front + 2 * shift < ci) {
                dq.pop_front();
            }
            if ci < cols {
                while dq.back().is_some_and(|&back| drow[back] <= drow[ci]) {
                    dq.pop_back();
                }
                dq.push_back(ci);
            }
            if ci < shift {
                continue;
            }
            let c = ci - shift;
            let winmax = drow[dq.front().copied().unwrap_or(c)];
            // The priors (grid - dis) may be negative — the occlusion cue rewards
            // routing along the edge of an occluding object — so let them pull, but
            // never below the node's *own* disagreement: the pixels a boundary sits
            // on are crossfaded at half weight whatever the priors say, so a
            // high-disagreement node can never be bribed cheaper than an agreeing
            // one (an unbounded floor parks the seam inside the subject it should
            // clear). Clamping the priors alone instead (as this did) silently
            // deletes the cue's pull and pins the seam wherever the priors left it.
            cap[ni * cols + c] = (winmax + grow[c] - drow[c]).max(drow[c]);
        }
    }
    // The min-cut as an interval shortest path — the exact dual of this
    // node-capacity grid. `big` exceeds any chain of cell costs, so the
    // optimum crosses no adjacency edge; a cut is, per node row, a prefix of
    // columns whose `p_out` lie on the source side (last one `a`, possibly
    // "none at all" = -1) and a longer prefix whose `p_in` do (last `b >= a
    // + 1`: a cell's own `in` reachable through out(a) forces in(a + 1) by
    // the row's horizontal edge): the row crosses the cells in (a, b], at the
    // sum of their caps. Avoiding the vertical edges reads `a' <= b && b' >=
    // a` over the previous row's pair. Single-cell chains are the b = a + 1
    // family — but on equal-cost plateaus the true min cut may cross two
    // cells in a row, and a chain-only dual then labels cells the graph calls
    // boundary as fully owned: the blend midpoint shifts, and where plateaus
    // abut real edges the seam moved a whole grid column (the full-res
    // artifacts this fixes). DP over the pairs instead, `sa = a + 1` as the
    // row's first owned-out column: each transition is a 2-D cumulative
    // minimum (prefix in sa', suffix in b') over the neighbouring row's
    // table, so a pass is O(nodes * cols^2) plain integer ops —
    // milliseconds — where the max-flow's augmenting snakes cost seconds per
    // frame on this graph (~180-230 Dinic phases over the lattice's 79k
    // cells; [`MaxFlow`] stays the cfg(test) reference).
    // Canonicalise the mirror: one code path for both longitudes, pretending
    // the REAR lens owns the LOW columns (`ci -> cols-1-ci` is its own
    // inverse, so `can` also maps canonical columns back to band columns).
    let can = |ci: usize| if b.front_rises { ci } else { cols - 1 - ci };
    // Coverage forcing bounds each row: a node only the front lens sees
    // cannot have its `out` on the source side (`out in S` iff c <= a), so
    // sa <= (least front-only c); a node only the rear sees needs its `in` in
    // S (`in in S` iff c <= b), so b >= (largest rear-only c). The band's
    // pins are that statement at the last column (out(cols-1) on source pays
    // a terminal edge: sa <= cols - 1 leaves every column ownable... a <=
    // cols - 2 = sa <= cols - 1, already the state space's bound; the rear
    // pin at column 0 is subsumed by b >= 0).
    let (mut sa_max, mut b_min) = (vec![cols.saturating_sub(1); nodes], vec![0usize; nodes]);
    for (ni, (sm, bl)) in sa_max.iter_mut().zip(b_min.iter_mut()).enumerate() {
        for ci in 0..cols {
            let cov = g.cover[ni * cols + ci];
            let c = can(ci);
            let (front_only, rear_only) = (cov & 1 != 0 && cov & 2 == 0, cov & 2 != 0 && cov & 1 == 0);
            if front_only {
                *sm = (*sm).min(c);
            } else if rear_only {
                *bl = (*bl).max(c);
            }
        }
    }
    let capi: Vec<i64> = (0..cells).map(|p| (cap[p] * GC_SCALE).round().max(0.0).min(i64::MAX as f64) as i64).collect();
    let labels = if exact { seam_cut_intervals(g, b, &capi, &sa_max, &b_min)? } else { seam_cut_chain(g, b, &capi, &sa_max, &b_min)? };
    // Read a per-row seam column off the label grid: the boundary between the
    // rear and front regions in the node row covering each pixel row. The front
    // owns the high columns at lon -90 (the seam is the leftmost non-rear node)
    // and the low ones at lon +90 (the rightmost).
    let mut path = vec![bw as f64 / 2.0; h];
    for (ni, yrow) in path.chunks_mut(DP_ROW_STRIDE.max(1)).enumerate().take(nodes) {
        let row = &labels[ni * cols..(ni + 1) * cols];
        let seam =
            if b.front_rises { row.iter().position(|&l| l != 0).unwrap_or(cols / 2) } else { row.iter().rposition(|&l| l != 0).map_or(cols / 2, |i| i + 1) };
        yrow.fill((seam * stride + stride / 2) as f64);
    }

    Some((labels, path))
}

/// The upscaled-lattice chain dual (one boundary cell per row: the interval
/// family's diagonal), O(cells) in two linear sweeps. See [`seam_cut`]'s
/// `exact` parameter for when it stands in for the interval solver.
fn seam_cut_chain(g: &SeamGrid, b: &Band, capi: &[i64], sa_max: &[usize], b_min: &[usize]) -> Option<Vec<u8>> {
    let (cols, nodes) = (g.cols, g.nodes);
    let cells = nodes * cols;

    const INF: i64 = i64::MAX / 4;
    let can = |ci: usize| if b.front_rises { ci } else { cols - 1 - ci };
    let w = |ni: usize, cl: usize| -> i64 { capi[ni * cols + can(cl)] };
    // Chain dual: one boundary cell per node row (state = the column x of
    // the crossed cell; the interval family's diagonal sa = b = x),
    // transitions |x - x'| <= 1 where coverage bounds allow. F x B sweeps
    // over `cols` per row - O(cells), no square tables - and a cell lies
    // on an optimal chain when F(x) + B(x) (x counted once) reaches the
    // optimum; the least such x per row is that family's smallest
    // source side. This is the boundary a graph whose optimum never needs
    // a multi-cell row labels identically; where plateaus do want one,
    // the answer agrees in cost and differs inside the agreeing
    // background by less than a grid column - invisible on the proxy.
    let (mut frows, mut prev): (Vec<Vec<i64>>, Vec<i64>) = (Vec::with_capacity(nodes), Vec::new());
    for ni in 0..nodes {
        let mut cur = vec![INF; cols];
        for x in 0..cols {
            if x > sa_max[ni] || x < b_min[ni] {
                continue;
            }
            let past = if ni == 0 {
                0
            } else {
                let mut m = prev[x];
                if x > 0 {
                    m = m.min(prev[x - 1]);
                }
                if x + 1 < cols {
                    m = m.min(prev[x + 1]);
                }
                m
            };
            if past < INF {
                cur[x] = past.saturating_add(w(ni, x)).min(INF);
            }
        }
        prev = cur.clone();
        frows.push(cur);
    }
    let copt = frows[nodes - 1].iter().copied().fold(INF, i64::min);
    if copt >= INF {
        return None;
    }
    let mut minxs = vec![usize::MAX; nodes];
    let mut bcol = vec![0i64; cols];
    for ni in (0..=nodes - 1).rev() {
        for x in 0..cols {
            let fv = frows[ni][x];
            if fv < INF && bcol[x] < INF && fv.saturating_add(bcol[x]) == copt {
                minxs[ni] = minxs[ni].min(x);
            }
        }
        if minxs[ni] == usize::MAX {
            return None;
        }
        if ni > 0 {
            let mut nb = vec![INF; cols];
            for x in 0..cols {
                if bcol[x] >= INF {
                    continue;
                }
                let v = w(ni, x).saturating_add(bcol[x]).min(INF);
                for xp in x.saturating_sub(1)..=(x + 1).min(cols - 1) {
                    if v < nb[xp] {
                        nb[xp] = v;
                    }
                }
            }
            bcol = nb;
        }
    }
    let mut labels = vec![1u8; cells];
    for ni in 0..nodes {
        let x = minxs[ni];
        for ci in 0..cols {
            let c = can(ci);
            labels[ni * cols + ci] = if c < x {
                0
            } else if c == x {
                2
            } else {
                1
            };
        }
    }
    Some(labels)
}

/// The exact interval dual of the seam graph (see [`seam_cut`]): per-row
/// boundary pairs (sa, b), O(nodes * cols^2) integer ops through 2-D
/// cumulative minima ([`cummin`], whose row-min stage vectorizes). Table
/// widths are chosen per solve by [`seam_cut_intervals`]: `u32` whenever the
/// costs provably stay clear of the sentinel, halving the bytes and letting
/// the min and add stages vectorize 4-wide, with `u64` as the fall-back
/// width. The backward sweep fills its tables in place, bottom-up (the
/// store-then-reverse the first version needed copied 200 MB per frame),
/// and the second forward sweep scans for optimal states inside the fill
/// it already runs rather than re-walking each table.
fn seam_cut_intervals(g: &SeamGrid, b: &Band, capi: &[i64], sa_max: &[usize], b_min: &[usize]) -> Option<Vec<u8>> {
    let (cols, nodes) = (g.cols, g.nodes);
    // A cell cost is the windowed disagreement plus the position priors, each
    // a mean of 8-bit differences, and the DP adds at most one cell per node
    // row along a path, so costs stay far below 2^28 whenever the cap does;
    // anything weirder (a pathological bake) runs the u64 table instead.
    let (cols_n, nodes_n) = (cols as u64, nodes as u64);
    let max_cap = capi.iter().copied().max().unwrap_or(0) as u64;
    let narrow = max_cap.saturating_mul(cols_n) < u32::MAX as u64 && max_cap.saturating_mul(nodes_n) < 2 * ((1u64 << 31) - 1);
    if narrow { seam_cut_intervals_t::<u32>(g, b, capi, sa_max, b_min) } else { seam_cut_intervals_t::<u64>(g, b, capi, sa_max, b_min) }
}

/// Table value types for the interval DP: saturating costs with the
/// unreachable sentinel, as unsigned so min and add vectorize.
trait DpVal: Copy + Ord + Send + Sync + std::ops::Sub<Output = Self> {
    /// TEMP debug projection.
    const INF: Self;
    fn z() -> Self;
    fn cap64(i: i64) -> Self;
    fn add_sat(self, rhs: Self) -> Self;
}

impl DpVal for u32 {
    const INF: Self = (1 << 31) - 1;
    fn z() -> Self {
        0
    }
    fn cap64(i: i64) -> Self {
        u32::try_from(i).unwrap_or(u32::MAX)
    }
    fn add_sat(self, rhs: Self) -> Self {
        self.saturating_add(rhs)
    }
}

impl DpVal for u64 {
    const INF: Self = (1 << 62) - 1;
    fn z() -> Self {
        0
    }
    fn cap64(i: i64) -> Self {
        i.max(0) as u64
    }
    fn add_sat(self, rhs: Self) -> Self {
        self.saturating_add(rhs)
    }
}

/// The exact interval dual of the seam graph (see [`seam_cut`]): per-row
/// boundary pairs (sa, b), O(nodes * cols^2) integer ops, run in transposed
/// coordinates so the whole solve is a handful of contiguous-row min/scan
/// passes. A node row's table is stored as `t[b][sa]` — state (sa, b) at
/// cell b * cols + sa — which turns the transition's crossed bounds ("every
/// `sa'` up to b+1, every `b'` from sa-1 up", [`seam_cut_intervals_ref`]
/// reads those cell by cell) into one within-row prefix pass and one
/// row-wise suffix pass: exactly the 2-D cumulative minimum, stated in the
/// frame where a bound lives on the axis it bounds. A single blocked
/// transpose returns the table to natural order, shifted by one cell on
/// each axis, and the transition's value at (sa, b) is then the contiguous
/// read `x[min(b + 1, cols - 1)][sa.saturating_sub(1)]` — cost added and
/// the row's coverage bounds applied in the same sweep. The forward sweeps
/// replay the step from a zero seed (above the top row nothing constrains
/// anything); the second one recognises each row's optimal states as it
/// fills them, summing with the stored backward tables to the optimum.
fn seam_cut_intervals_t<T: DpVal>(g: &SeamGrid, b: &Band, capi: &[i64], sa_max: &[usize], b_min: &[usize]) -> Option<Vec<u8>> {
    let (cols, nodes) = (g.cols, g.nodes);
    let cells = nodes * cols;
    let square = cols * cols;

    let can = |ci: usize| if b.front_rises { ci } else { cols - 1 - ci };
    let w = |ni: usize, cl: usize| -> T { T::cap64(capi[ni * cols + can(cl)]) };
    // Interval cost over canonical columns [sa, b]: prefix sums of w.
    let pref: Vec<Vec<T>> = (0..nodes)
        .map(|ni| {
            let mut p = vec![T::z(); cols + 1];
            for cl in 0..cols {
                p[cl + 1] = p[cl].add_sat(w(ni, cl));
            }
            p
        })
        .collect();
    // Both chained sweeps run on their own thread and store their per-row
    // step tables: the backward's tables are what the labelling pass sums
    // the forward's against, cell by cell, so no third chain is ever run.
    let square_nodes = square * nodes;
    let swept = std::thread::scope(|sc| {
        let bwd = sc.spawn(|| {
            let mut btab = vec![T::z(); square_nodes];
            let mut raw = vec![T::z(); square];
            let mut aux = vec![T::z(); square];
            for ni in (0..nodes).rev() {
                let (head, _) = btab.split_at_mut((ni + 1) * square);
                let gtab = &mut head[ni * square..];
                dp_step(&mut raw, &mut aux, gtab, cols, &DpRow { ni, sa_max, b_min, pcost: &pref[ni], bwd_g: None, opt: T::INF });
            }
            btab
        });
        let fwd = sc.spawn(|| {
            let mut ftab = vec![T::z(); square_nodes];
            let mut raw = vec![T::z(); square];
            let mut aux = vec![T::z(); square];
            for ni in 0..nodes {
                let (head, _) = ftab.split_at_mut((ni + 1) * square);
                let gtab = &mut head[ni * square..];
                dp_step(&mut raw, &mut aux, gtab, cols, &DpRow { ni, sa_max, b_min, pcost: &pref[ni], bwd_g: None, opt: T::INF });
            }
            ftab
        });
        match (fwd.join(), bwd.join()) {
            (Ok(ftab), Ok(btab)) => Some((ftab, btab)),
            // A sweep thread can only fail by panicking inside its own
            // arithmetic; degrade to the 1-D cut like a band with no plan.
            _ => None,
        }
    });
    let (ftab, btab) = swept?;
    // The optimum: the least complete forward value over the sweep's last
    // row, reconstructed from the stored table exactly as the fill step
    // would (cell (sa, b) takes row (b+1) of the stored transition table,
    // shifted by one, plus that interval's prefix cost).
    let nr = nodes - 1;
    let pcr = &pref[nr];
    let smaxr = sa_max[nr].min(cols - 1);
    let mut opt = T::INF;
    {
        let ft = &ftab[nr * square..][..square];
        for bv in 0..cols {
            let lim = if bv < b_min[nr] { 0 } else { bv.min(smaxr) + 1 };
            if lim == 0 {
                continue;
            }
            let src = &ft[(bv + 1).min(cols - 1) * cols..][..cols];
            opt = opt.min(src[0].add_sat(pcr[bv + 1] - pcr[0]).min(T::INF));
            for sa in 1..lim {
                opt = opt.min(src[sa - 1].add_sat(pcr[bv + 1] - pcr[sa]).min(T::INF));
            }
        }
    }
    if opt >= T::INF {
        return None; // no finite cut at all: keep the 1-D DP
    }
    // The labelling sweep, cell by cell and band row by band row, with no
    // chain left to run: a state (sa, b) of row ni lies on an optimal cut
    // when the forward value the fill step would form at it - the stored
    // row-table entry at [srow + sa - 1] plus the interval's prefix cost -
    // sums with the backward table's same-indexed entry to the optimum. The
    // cut's smallest source side owns a column's `out` under every optimal
    // cut iff sa <= it in every one - so out in S iff c < min sa over opt
    // states and, dually, in in S iff c <= min b - labelling the rows
    // exactly as the reference max-flow's residual reachable set does.
    let mut labels = vec![1u8; cells];
    let none = std::sync::atomic::AtomicBool::new(false);
    labels.par_chunks_mut(cols).enumerate().for_each(|(ni, lrow)| {
        let (ft, bt, pc) = (&ftab[ni * square..][..square], &btab[ni * square..][..square], &pref[ni]);
        let smax = sa_max[ni].min(cols - 1);
        let (mut msa, mut mb) = (usize::MAX, usize::MAX);
        for bv in 0..cols {
            let lim = if bv < b_min[ni] { 0 } else { bv.min(smax) + 1 };
            let (src, bg) = (&ft[(bv + 1).min(cols - 1) * cols..][..cols], &bt[(bv + 1).min(cols - 1) * cols..][..cols]);
            if lim == 0 {
                continue;
            }
            let v0 = src[0].add_sat(pc[bv + 1] - pc[0]).min(T::INF);
            if v0 < T::INF && v0.add_sat(bg[0]) == opt {
                (msa, mb) = (0, mb.min(bv));
            }
            for sa in 1..lim {
                let v = src[sa - 1].add_sat(pc[bv + 1] - pc[sa]).min(T::INF);
                if v < T::INF && v.add_sat(bg[sa - 1]) == opt {
                    msa = msa.min(sa);
                    mb = mb.min(bv);
                }
            }
        }
        if msa == usize::MAX {
            none.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        for (ci, cell) in lrow.iter_mut().enumerate() {
            let c = can(ci);
            *cell = if c < msa {
                0
            } else if c <= mb {
                2
            } else {
                1
            };
        }
    });
    if none.load(std::sync::atomic::Ordering::Relaxed) {
        return None; // opt finite implies a state; belt & braces
    }
    Some(labels)
}

/// One node row of the interval DP in transposed coordinates (see
/// [`seam_cut_intervals_t`]): `raw` is the previous row of the sweep's
/// costs, cell `[b][sa]`; the sweep's cummin'd, transposed table of `raw`
/// is written to `gtab`, and `raw` in place becomes this row's costs.
/// `bwd_g`, when given, is the backward row's table: states whose values
/// sum with it to `opt` are this row's optimal states, and the least sa
/// and least b among them come back as (msa, mb) — the labelling bounds.
/// The sweep's least cell cost comes back too (the last forward row's is
/// the optimum).
/// One node row of the transposed-coordinate interval DP: an index (for the
/// row's coverage bounds), the row's interval-cost prefix sums, and, for the
/// labelling sweep, the row's stored backward transition table and the
/// optimum the states are tested against.
struct DpRow<'a, T> {
    ni: usize,
    sa_max: &'a [usize],
    b_min: &'a [usize],
    pcost: &'a [T],
    bwd_g: Option<&'a [T]>,
    opt: T,
}

fn dp_step<T: DpVal>(raw: &mut [T], aux: &mut [T], gtab: &mut [T], cols: usize, row: &DpRow<'_, T>) -> (T, usize, usize) {
    // The cumulative minimum's two stages on the states' lower triangle
    // ([`seam_cut_intervals_ref`] does the same math scalar and in place,
    // without bounds). Cells past a row's diagonal (sa > b) hold plateau and
    // INF: the prefix is sixteen independent register-chained scans streaming
    // from `raw` (reading a cell one store ago stalls on store-to-forwarding;
    // accumulators do not, and plateau writes past the chain's frontier are
    // free stores of one value), the suffix merge min-streaks only the strict
    // upper triangle - exactly the cells the transpose carries to a row's
    // transition reads (dst row `b` + 1, columns up to `b`).
    {
        let n = cols.min(16);
        for rg in (0..cols).step_by(n) {
            let re = (rg + n).min(cols);
            let blk = &mut aux[rg * cols..re * cols];
            let mut acc = [T::z(); 16];
            for r in 0..re - rg {
                acc[r] = raw[(rg + r) * cols];
            }
            for c in 1..cols {
                for r in 0..re - rg {
                    let v = raw[(rg + r) * cols + c];
                    if v < acc[r] {
                        acc[r] = v;
                    }
                    blk[r * cols + c] = acc[r];
                }
            }
        }
    }
    {
        for q in (0..cols - 1).rev() {
            let (_, tail) = aux.split_at_mut(q * cols);
            let (rowq, below) = tail.split_at_mut(cols);
            let below = &*below;
            // whole rows: a column needs its full chain to the bottom row, and
            // the transpose reads plateau cells (row < column) whose value
            // carries that chain; only the transpose's own bound is triangular.
            for (o, &v) in rowq.iter_mut().zip(below) {
                if v < *o {
                    *o = v;
                }
            }
        }
    }
    {
        transpose_sq_lower(aux, gtab, cols);
    }
    let samax = row.sa_max[row.ni].min(cols - 1);
    let (mut msa, mut mb, mut low) = (usize::MAX, usize::MAX, T::INF);
    // The fill, per b row: the transition row shifted by one (its column 0
    // duplicated for the saturating sa - 1 bound), plus the row's interval
    // cost and coverage bounds. With a backward table the optimal-state test
    // rides along in the same straight-line sweep (min/select, no branches),
    // so the common sweep stays vectorized; without one the loop is the bare
    // cost add. Rows wholly below the row bound and cells past sa <= min(b,
    // sa_max) are the vacuous dead states, INF.
    if let Some(gtbl) = row.bwd_g {
        for b in 0..cols {
            let srow = (b + 1).min(cols - 1) * cols;
            let (src, bg) = (&gtab[srow..srow + cols], &gtbl[srow..srow + cols]);
            let add = row.pcost[b + 1];
            let dst = &mut raw[b * cols..b * cols + cols];
            let lim = if b < row.b_min[row.ni] { 0 } else { b.min(samax) + 1 };
            let v0 = if lim == 0 { T::INF } else { src[0].add_sat(add - row.pcost[0]).min(T::INF) };
            dst[0] = v0;
            let hit = v0 < T::INF && v0.add_sat(bg[0]) == row.opt;
            msa = msa.min(if hit { 0 } else { usize::MAX });
            mb = mb.min(if hit { b } else { usize::MAX });
            low = low.min(v0);
            for sa in 1..lim {
                let v = src[sa - 1].add_sat(add - row.pcost[sa]).min(T::INF);
                dst[sa] = v;
                low = low.min(v);
                let hit = v < T::INF && v.add_sat(bg[sa - 1]) == row.opt;
                msa = msa.min(if hit { sa } else { usize::MAX });
                mb = mb.min(if hit { b } else { usize::MAX });
            }
            dst[lim..].fill(T::INF);
        }
    } else {
        for b in 0..cols {
            let srow = (b + 1).min(cols - 1) * cols;
            let src = &gtab[srow..srow + cols];
            let add = row.pcost[b + 1];
            let dst = &mut raw[b * cols..b * cols + cols];
            let lim = if b < row.b_min[row.ni] { 0 } else { b.min(samax) + 1 };
            let v0 = if lim == 0 { T::INF } else { src[0].add_sat(add - row.pcost[0]).min(T::INF) };
            dst[0] = v0;
            low = low.min(v0);
            for sa in 1..lim {
                let v = src[sa - 1].add_sat(add - row.pcost[sa]).min(T::INF);
                dst[sa] = v;
                low = low.min(v);
            }
            dst[lim..].fill(T::INF);
        }
    }
    (low, msa, mb)
}

/// The transpose of a triangular table's half that matters: only
/// `dst[i][j] = src[j][i]` for j < i, the cells a row's transition reads
/// (the rest of the square holds the triangle's INF plateau). Full blocks
/// below the diagonal transpose as whole 32x32 tiles; the diagonal band
/// keeps its per-cell bound.
fn transpose_sq_lower<T: DpVal>(src: &[T], dst: &mut [T], cols: usize) {
    const BL: usize = 32;
    let mut blk = [[T::z(); BL]; BL];
    for br in (0..cols).step_by(BL) {
        let re = (br + BL).min(cols);
        // whole tiles: dst rows [br, re) (the larger index) read src rows [bc, ce)
        for bc in (0..br).step_by(BL) {
            let ce = (bc + BL).min(cols);
            for j in 0..ce - bc {
                blk[j][..re - br].copy_from_slice(&src[(bc + j) * cols + br..(bc + j) * cols + re]);
            }
            for k in 0..re - br {
                let d = &mut dst[(br + k) * cols + bc..(br + k) * cols + ce];
                for j in 0..ce - bc {
                    d[j] = blk[j][k];
                }
            }
        }
        // the diagonal band: cells dst[I][J] = src[J][I] with br <= J < I < re
        for j in br..re {
            for i in j + 1..re {
                dst[i * cols + j] = src[j * cols + i];
            }
        }
    }
}

/// A square table's transpose, in 32-cell blocks: a block's rows are
/// consecutive cells (one L1-resident scratch block), its writes span two
/// cache lines per destination row, and each destination line is touched
/// exactly once per transpose.
#[allow(dead_code)]
fn transpose_sq<T: DpVal>(src: &[T], dst: &mut [T], cols: usize) {
    const BL: usize = 32;
    let mut blk = [[T::z(); BL]; BL];
    for br in (0..cols).step_by(BL) {
        for bc in (0..cols).step_by(BL) {
            let re = (br + BL).min(cols);
            let ce = (bc + BL).min(cols);
            for r in br..re {
                blk[r - br][..ce - bc].copy_from_slice(&src[r * cols + bc..r * cols + ce]);
            }
            for k in 0..ce - bc {
                let d = &mut dst[(bc + k) * cols + br..(bc + k) * cols + re];
                for j in 0..re - br {
                    d[j] = blk[j][k];
                }
            }
        }
    }
}

/// Row-major reference form of the interval DP (superseded by
/// [`seam_cut_intervals_t`]'s transposed-coordinate step, kept as the
/// independent implementation the differential fuzz compares against).
#[cfg(test)]
fn seam_cut_intervals_ref<T: DpVal>(g: &SeamGrid, b: &Band, capi: &[i64], sa_max: &[usize], b_min: &[usize]) -> Option<Vec<u8>> {
    let (cols, nodes) = (g.cols, g.nodes);
    let cells = nodes * cols;

    let can = |ci: usize| if b.front_rises { ci } else { cols - 1 - ci };
    let w = |ni: usize, cl: usize| -> T { T::cap64(capi[ni * cols + can(cl)]) };
    // Interval cost over canonical columns [sa, b]: prefix sums of w.
    let pref: Vec<Vec<T>> = (0..nodes)
        .map(|ni| {
            let mut p = vec![T::z(); cols + 1];
            for cl in 0..cols {
                p[cl + 1] = p[cl].add_sat(w(ni, cl));
            }
            p
        })
        .collect();
    let square = cols * cols;
    let state_ok = |ni: usize, sa: usize, bnd: usize| sa <= sa_max[ni] && bnd >= b_min[ni].max(sa);
    // Backward: btab[ni][state] = cheapest cost of the rows below ni given
    // row ni's state (the 2-D cumulative minimum of that row's costs) —
    // (forward + btab == opt) marks the states on optimal cuts. Filled IN
    // PLACE, bottom-up — the bottom row is all zeros and stays, since past
    // the last node row any pair can still be crossed at no cost.
    let mut btab = vec![T::z(); square * nodes];
    {
        // `tab` carries row ni+1's costs to row ni; cummin-ing it makes the
        // transition's transposed read exact in one lookup (the cumulative
        // minimum absorbs it), and the bottom row starts as its zero table.
        let mut tab = vec![T::z(); square];
        for ni in (0..nodes).rev() {
            let row = &mut btab[ni * square..(ni + 1) * square];
            if ni + 1 < nodes {
                cummin(&mut tab, cols);
                row.fill(T::INF);
                for sa in 0..cols {
                    for bnd in sa..cols {
                        if state_ok(ni, sa, bnd) {
                            row[sa * cols + bnd] = tab[(bnd + 1).min(cols - 1) * cols + sa.saturating_sub(1)];
                        }
                    }
                }
            }
            tab.fill(T::INF);
            let pcost = &pref[ni];
            for sa in 0..cols {
                for bnd in sa..cols {
                    if state_ok(ni, sa, bnd) {
                        tab[sa * cols + bnd] = row[sa * cols + bnd].add_sat(pcost[bnd + 1] - pcost[sa]).min(T::INF);
                    }
                }
            }
        }
    }
    // Forward twice: the optimum, then (on the second sweep) each row's
    // optimal states, spotted during the same fill. The cut's smallest
    // source side owns a column's `out` under every optimal cut iff sa <=
    // it in every one — so out in S iff c < min sa over opt states and,
    // dually, in in S iff c <= min b — labelling the rows exactly as the
    // reference max-flow's residual reachable set does, cell for cell.
    let mut labels = vec![1u8; cells];
    // `seed` is the rows above, cummin'd and read as the transition is;
    // before row 0 it is the empty prefix, whose every read costs 0.
    let mut seed = vec![T::z(); square];
    let mut f = vec![T::INF; square];
    let mut opt = T::INF;
    for pass in 0..2u8 {
        seed.fill(T::z());
        for ni in 0..nodes {
            if ni > 0 {
                cummin(&mut seed, cols);
            }
            f.fill(T::INF);
            let pcost = &pref[ni];
            let bt = &btab[ni * square..(ni + 1) * square];
            let (mut msa, mut mb) = (usize::MAX, usize::MAX);
            let flag = pass == 1;
            for sa in 0..cols {
                for bnd in sa..cols {
                    if !state_ok(ni, sa, bnd) {
                        continue;
                    }
                    let p = sa * cols + bnd;
                    let past = seed[(bnd + 1).min(cols - 1) * cols + sa.saturating_sub(1)];
                    let v = past.add_sat(pcost[bnd + 1] - pcost[sa]).min(T::INF);
                    f[p] = v;
                    if flag && v < T::INF && v.add_sat(bt[p]) == opt {
                        msa = msa.min(sa);
                        mb = mb.min(bnd);
                    }
                }
            }
            if flag {
                if msa == usize::MAX {
                    return None; // opt finite implies a state; belt & braces
                }
                for ci in 0..cols {
                    let c = can(ci);
                    labels[ni * cols + ci] = if c < msa {
                        0
                    } else if c <= mb {
                        2
                    } else {
                        1
                    };
                }
            }
            std::mem::swap(&mut seed, &mut f);
            if ni + 1 == nodes {
                opt = seed.iter().fold(T::INF, |a, x| a.min(*x));
            }
        }
    }
    if opt >= T::INF {
        return None; // no finite cut at all: keep the 1-D DP
    }
    Some(labels)
}

/// 2-D cumulative minimum in place: prefix over the first table index, suffix
/// over the second (min is commutative, so sweep order does not matter). The
/// prefix stage is pairwise row minima, which vectorize; the suffix stage is
/// a scan. Used by the row-major reference form of the solve.
#[cfg(test)]
fn cummin<T: Copy + Ord>(t: &mut [T], cols: usize) {
    for a in 1..cols {
        let (ra, rp) = (a * cols, (a - 1) * cols);
        for cl in 0..cols {
            let m = t[rp + cl];
            let x = &mut t[ra + cl];
            if m < *x {
                *x = m;
            }
        }
    }
    for a in 0..cols {
        let ro = a * cols;
        for cl in (0..cols - 1).rev() {
            let m = t[ro + cl + 1];
            let x = &mut t[ro + cl];
            if m < *x {
                *x = m;
            }
        }
    }
}

/// The same cut solved the previous way, as an independent reference for the
/// dual: build the node-capacity graph and read the min-cut from the residual
/// reachable (smallest source) side.
#[cfg(test)]
fn seam_cut_by_graph(g: &SeamGrid, b: &Band, bw: usize, h: usize) -> Option<(Vec<u8>, Vec<f64>)> {
    let (cols, nodes, stride) = (g.cols, g.nodes, g.stride);
    if nodes == 0 || cols < 3 {
        return None;
    }
    let cells = nodes.checked_mul(cols)?;
    if cells.checked_mul(2)?.checked_add(2)? > GC_MAX_NODES {
        return None;
    }
    // Node ids: source 0, sink 1, then p_in = 2 + 2*p, p_out = p_in + 1.
    let (s, t) = (0usize, 1usize);
    let mut mf = MaxFlow::new(2 + cells * 2);
    let pin = |p: usize| 2 + 2 * p;
    // A capacity larger than any node cost, so the cut crosses node costs rather than
    // these edges. Bounded well below i64::MAX so a whole boundary's worth of them
    // cannot overflow the flow.
    let dmax = g.grid.iter().cloned().fold(0f64, f64::max).max(1.0);
    const GC_NEIGH: f64 = 64.0;
    let big = (dmax * GC_NEIGH * GC_SCALE).round().clamp(1.0, 1e15) as i64;
    // The boundary cost. A seam through a node crossfades everything within half a
    // blend window of it, so the *disagreement* a boundary at `p` must pay is the
    // worst in `p`'s window, not just `p`'s own: without this the cut routes through
    // the low-disagreement GAP between a doubled object's two parallax copies (both
    // copies then show, one per lens) exactly as the 1-D DP does; with it the gap is
    // expensive (each copy is inside the other's window) and the boundary snaps to
    // the edge of the disagreement, giving the object wholly to one lens. The
    // position-specific priors (centre, occlusion cue, coverage) are NOT
    // windowed - they say which side and where, so they stay on the node the
    // boundary actually sits on.
    let base = (bw as f64 * SEAM_RAMP).max(4.0);
    let win = ((base / 2.0 / stride as f64) as usize).max(1);
    let mut cap = vec![0f64; cells];
    for ni in 0..nodes {
        let (drow, grow) = (&g.dis[ni * cols..(ni + 1) * cols], &g.grid[ni * cols..(ni + 1) * cols]);
        // Sliding window maximum of the disagreement over +-win columns, read at
        // the column *centred* in the window. The deque holds candidates for the
        // window [ci - 2 * shift, ci]; its centre is `ci - shift`, and the loop
        // runs `shift` phantom columns past the right edge to give the last real
        // columns the same truncated window the first columns get on their side:
        // the same disagreement costs the same whether it lies left or right of
        // the candidate seam. (A narrow band narrows the shift so every column
        // is still evaluated once with its widest possible window.)
        let shift = win.min(cols - 1);
        let mut dq: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
        for ci in 0..cols + shift {
            while dq.front().is_some_and(|&front| front + 2 * shift < ci) {
                dq.pop_front();
            }
            if ci < cols {
                while dq.back().is_some_and(|&back| drow[back] <= drow[ci]) {
                    dq.pop_back();
                }
                dq.push_back(ci);
            }
            if ci < shift {
                continue;
            }
            let c = ci - shift;
            let winmax = drow[dq.front().copied().unwrap_or(c)];
            // The priors (grid - dis) may be negative — the occlusion cue rewards
            // routing along the edge of an occluding object — so let them pull, but
            // never below the node's *own* disagreement: the pixels a boundary sits
            // on are crossfaded at half weight whatever the priors say, so a
            // high-disagreement node can never be bribed cheaper than an agreeing
            // one (an unbounded floor parks the seam inside the subject it should
            // clear). Clamping the priors alone instead (as this did) silently
            // deletes the cue's pull and pins the seam wherever the priors left it.
            cap[ni * cols + c] = (winmax + grow[c] - drow[c]).max(drow[c]);
        }
    }
    for ni in 0..nodes {
        for ci in 0..cols {
            let p = ni * cols + ci;
            let (p_in, p_out) = (pin(p), pin(p) + 1);
            let d = (cap[p] * GC_SCALE).round().clamp(0.0, 1e15) as i64;
            mf.add_edge(p_in, p_out, d);
            // Neighbour edges: an adjacency between p and q is `p_out -> q_in` and
            // `q_out -> p_in`, both at `big`, so a node whose `p_out` is reachable
            // forces its neighbour's `q_in` reachable too (the neighbour is then on
            // the same side or is itself a boundary node). Crossing one costs `big`,
            // so the boundary prefers to step through a node's own cost.
            if ci + 1 < cols {
                let q = pin(p + 1);
                mf.add_edge(p_out, q, big);
                mf.add_edge(q + 1, p_in, big);
            }
            if ni + 1 < nodes {
                let q = pin(p + cols);
                mf.add_edge(p_out, q, big);
                mf.add_edge(q + 1, p_in, big);
            }
            // Coverage: a node only one lens sees is pinned to that lens's side
            // (front = sink, rear = source), so the seam cannot strand it.
            let cov = g.cover[p];
            let (front_sees, rear_sees) = (cov & 1 != 0, cov & 2 != 0);
            if front_sees && !rear_sees {
                mf.add_edge(p_out, t, big);
            } else if rear_sees && !front_sees {
                mf.add_edge(s, p_in, big);
            }
        }
    }
    // Pin the band's two ends to opposite lenses so a connected seam must separate
    // them: at lon -90 (`front_rises`) the front owns the high columns, so the last
    // column is pinned to the front (sink) and the first to the rear (source); at
    // lon +90 it is the mirror image.
    let (first, last) = (0usize, cols - 1);
    let (front_col, rear_col) = if b.front_rises { (last, first) } else { (first, last) };
    for ni in 0..nodes {
        mf.add_edge(pin(ni * cols + front_col) + 1, t, big);
        mf.add_edge(s, pin(ni * cols + rear_col), big);
    }
    mf.dinic_flow(s, t);
    let side = mf.min_cut_side(s);
    // A node is rear-owned (both halves on the source side), front-owned (both on
    // the sink side), or the boundary the cut passed through (in on the source side,
    // out on the sink side, so its `p_in -> p_out` edge was cut). The boundary is
    // worth half in the blend, so a straight seam reproduces the linear crossfade.
    let mut labels = vec![0u8; cells];
    for p in 0..cells {
        let (in_s, out_s) = (side[pin(p)], side[pin(p) + 1]);
        labels[p] = match (in_s, out_s) {
            (true, true) => 0,
            (false, false) => 1,
            (true, false) => 2,
            (false, true) => 1,
        };
    }
    // Read a per-row seam column off the label grid: the boundary between the rear
    // and front regions in the node row covering each pixel row. The front owns the
    // high columns at lon -90 (so the seam is the leftmost non-rear node) and the low
    // ones at lon +90 (the rightmost).
    let mut path = vec![bw as f64 / 2.0; h];
    for ni in 0..nodes {
        let row = &labels[ni * cols..(ni + 1) * cols];
        let seam =
            if b.front_rises { row.iter().position(|&l| l != 0).unwrap_or(cols / 2) } else { row.iter().rposition(|&l| l != 0).map_or(cols / 2, |i| i + 1) };
        let col = (seam * stride + stride / 2) as f64;
        for y in (ni * DP_ROW_STRIDE).min(h)..((ni + 1) * DP_ROW_STRIDE).min(h) {
            path[y] = col;
        }
    }
    Some((labels, path))
}

/// The front lens's ownership weight per band pixel: from the 2-D cut's label field
/// when there is one (a window average that reproduces the linear crossfade on a
/// straight seam and hands a routed-around object wholly to one lens), else the ramp
/// around the placed 1-D column. `d` is +1 at lon -90 (the front owns the high
/// columns) and -1 at lon +90.
fn ownership_field(g: &SeamGrid, labels: &[u8], ramp: &[f64], path: &[f64], d: f64, bw: usize, h: usize) -> Vec<f64> {
    if !labels.is_empty() {
        return label_alpha(g, labels, ramp, bw, h);
    }
    let mut v = vec![0.5; bw * h];
    v.par_chunks_mut(bw).enumerate().for_each(|(y, row)| {
        let rm = ramp[y].max(1.0);
        for (i, cell) in row.iter_mut().enumerate() {
            *cell = (0.5 + d * (i as f64 - path[y]) / rm).clamp(0.0, 1.0);
        }
    });
    v
}

/// Expand a node label grid to a per-pixel ownership map (0 rear, 255 front,
/// 128 boundary) for the stitch dump.
fn labels_map(g: &SeamGrid, labels: &[u8], bw: usize, h: usize) -> Vec<u8> {
    let mut lm = vec![0u8; bw * h];
    for (p, &l) in labels.iter().enumerate() {
        let (ni, ci) = (p / g.cols, p % g.cols);
        let v = match l {
            0 => 0u8,
            1 => 255,
            _ => 128,
        };
        for y in (ni * DP_ROW_STRIDE).min(h)..((ni + 1) * DP_ROW_STRIDE).min(h) {
            for x in (ci * g.stride).min(bw)..((ci + 1) * g.stride).min(bw) {
                lm[y * bw + x] = v;
            }
        }
    }
    lm
}

/// The front lens's ownership weight per band pixel, from [`seam_cut`]'s label grid:
/// the mean label over a horizontal window of `ramp[y]` px, the label field taken as
/// constant over each node cell. For a straight vertical seam this is exactly the
/// linear crossfade the 1-D path produces (so the 2-D cut changes nothing where the
/// seam is already clean); where the boundary bulges around a doubled object it hands
/// the whole object to one lens with a hard edge in the agreeing background, instead
/// of crossfading both parallax copies at half weight (the band-0 doubling).
///
/// `labels` holds 0 = rear, 1 = front, 2 = the boundary node (worth half). Returns
/// `bw * h` weights in 0..=1.
fn label_alpha(g: &SeamGrid, labels: &[u8], ramp: &[f64], bw: usize, h: usize) -> Vec<f64> {
    let (cols, nodes, stride) = (g.cols, g.nodes, g.stride);
    let mut alpha = vec![0.5; bw * h];
    if labels.len() < cols * nodes || cols == 0 || nodes == 0 || stride == 0 {
        return alpha;
    }
    // Per node row, the running integral of the label value over pixels (each node
    // cell is `stride` px wide and constant), so a window average is two lookups.
    let val = |l: u8| -> f64 {
        match l {
            0 => 0.0,
            1 => 1.0,
            _ => 0.5,
        }
    };
    // Chunked per node row (exactly its DP_ROW_STRIDE output rows), so the
    // writes are disjoint: the row prefix and its two-lookup integrals are
    // row-local computation.
    alpha.par_chunks_mut(bw * DP_ROW_STRIDE).enumerate().for_each(|(ni, blocks)| {
        // chunks past the last node row repeat its labels (h need not divide
        // by the stride); the y guard below already drops their extra rows
        let row = &labels[ni.min(nodes - 1) * cols..(ni.min(nodes - 1) + 1) * cols];
        let mut pref = vec![0f64; cols + 1];
        for ci in 0..cols {
            pref[ci + 1] = pref[ci] + val(row[ci]) * stride as f64;
        }
        let integral = |x: f64| -> f64 {
            let xc = x.clamp(0.0, (cols * stride) as f64);
            let ci = ((xc as usize) / stride).min(cols - 1);
            pref[ci] + val(row[ci]) * (xc - ci as f64 * stride as f64)
        };
        for (j, blk) in blocks.chunks_mut(bw).enumerate() {
            let y = ni * DP_ROW_STRIDE + j;
            if y >= h {
                break;
            }
            let rm = ramp[y].max(1.0);
            for (i, cell) in blk.iter_mut().enumerate() {
                let xx = i as f64;
                // The window, clipped to the band: outside it the label field is
                // unknown, and averaging only the in-band part keeps the edge
                // pixels fully owned by the lens their edge cell says (the 1-D
                // ramp's clamp), rather than diluted toward half.
                let (a, b) = ((xx - rm / 2.0).clamp(0.0, bw as f64), (xx + rm / 2.0).clamp(0.0, bw as f64));
                *cell = if b > a { ((integral(b) - integral(a)) / (b - a)).clamp(0.0, 1.0) } else { 0.5 };
            }
        }
    });
    alpha
}

/// Box-downsample a `pw`×`ph` field by the integer factors `(pw/ow, ph/oh)` to
/// `ow`×`oh`. Identity when the factors are 1, so the reference-lattice decision
/// path costs nothing extra at the resolution the constants were tuned on.
fn box_down_u8(p: &[u8], pw: usize, ph: usize, ow: usize, oh: usize) -> Vec<u8> {
    let (fx, fy) = (pw / ow.max(1).max(1), ph / oh.max(1));
    if fx == 1 && fy == 1 {
        return p.to_vec();
    }
    let mut d = vec![0u8; ow * oh];
    d.par_chunks_mut(ow).enumerate().for_each(|(oy, row)| {
        for (ox, v) in row.iter_mut().enumerate() {
            let (mut a, mut n) = (0u32, 0u32);
            for j in 0..fy {
                for i in 0..fx {
                    a += u32::from(p[(oy * fy + j) * pw + ox * fx + i]);
                    n += 1;
                }
            }
            *v = (a / n.max(1)) as u8;
        }
    });
    d
}

fn box_down_f32(p: &[f32], pw: usize, ph: usize, ow: usize, oh: usize) -> Vec<f32> {
    let (fx, fy) = (pw / ow.max(1), ph / oh.max(1));
    if fx == 1 && fy == 1 {
        return p.to_vec();
    }
    let mut d = vec![0f32; ow * oh];
    d.par_chunks_mut(ow).enumerate().for_each(|(oy, row)| {
        for (ox, v) in row.iter_mut().enumerate() {
            let mut a = 0f64;
            for j in 0..fy {
                for i in 0..fx {
                    a += f64::from(p[(oy * fy + j) * pw + ox * fx + i]);
                }
            }
            *v = (a / (fx * fy).max(1) as f64) as f32;
        }
    });
    d
}

fn box_down_f64(p: &[f64], pw: usize, ph: usize, ow: usize, oh: usize) -> Vec<f64> {
    let (fx, fy) = (pw / ow.max(1), ph / oh.max(1));
    if fx == 1 && fy == 1 {
        return p.to_vec();
    }
    let mut d = vec![0f64; ow * oh];
    d.par_chunks_mut(ow).enumerate().for_each(|(oy, row)| {
        for (ox, v) in row.iter_mut().enumerate() {
            let mut a = 0f64;
            for j in 0..fy {
                for i in 0..fx {
                    a += p[(oy * fy + j) * pw + ox * fx + i];
                }
            }
            *v = a / (fx * fy).max(1) as f64;
        }
    });
    d
}

/// Bilinear-upsample `p` (`w`×`h`) by an integer factor to (`w*s`)×(`h*s`),
/// pixel-centre aligned, edge clamped: the same resampling [`resample_2d`] does
/// for the rear render, in the other direction. Identity for `s == 1`.
fn up_u8(p: &[u8], w: usize, h: usize, s: usize) -> Vec<u8> {
    if s == 1 || w == 0 || h == 0 {
        return p.to_vec();
    }
    let (dw, dh) = (w * s, h * s);
    let mut d = vec![0u8; dw * dh];
    d.par_chunks_mut(dw).enumerate().for_each(|(y, row)| {
        let fyy = (y as f64 + 0.5) / s as f64 - 0.5;
        let y0 = fyy.clamp(0.0, (h - 1) as f64).floor() as usize;
        let y1 = y0.min(h - 2);
        let ty = fyy - y0 as f64;
        let (r0, r1) = (&p[y0 * w..y0 * w + w], &p[y1 * w..y1 * w + w]);
        for (x, v) in row.iter_mut().enumerate() {
            let fxx = (x as f64 + 0.5) / s as f64 - 0.5;
            let x0 = fxx.clamp(0.0, (w - 1) as f64).floor() as usize;
            let x1 = x0.min(w - 2);
            let tx = fxx - x0 as f64;
            let a = f64::from(r0[x0]) * (1.0 - tx) + f64::from(r0[x1]) * tx;
            let b = f64::from(r1[x0]) * (1.0 - tx) + f64::from(r1[x1]) * tx;
            *v = (a * (1.0 - ty) + b * ty).round().clamp(0.0, 255.0) as u8;
        }
    });
    d
}

/// The seam-grid lens-coverage maps at the decision lattice: plane weights
/// resampled by nearest (a decision pixel stands for its whole block of output
/// pixels). [`seam_grid`] reads them exactly like the baked maps (full-width
/// rows, band starting at x0=0), so the cut's coverage forcing transfers
/// between resolutions unchanged.
fn band_coverage_maps(m: &[PlaneMap; 2], b: &Band, dw: usize, dh: usize, s: usize) -> [PlaneMap; 2] {
    m.each_ref().map(|p| PlaneMap {
        idx: Vec::new(),
        wgt: Vec::new(),
        w: (0..dh)
            .flat_map(|ys| {
                let y = ys / s;
                (0..dw).map(move |xs| u8::from(p.w[y * p.pw + b.x0 + xs / s] > 0) * 255)
            })
            .collect(),
        pw: dw,
        ph: dh,
        sw: dw,
        sh: dh,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_time::Tick;

    /// The seam column per row exactly as [`band_plan`] places it: the 2-D seam cut
    /// when the band is big enough for a bounded graph, else the 1-D DP. Tests that
    /// assert on seam *placement* (occlusion cue, disagreement routing) go
    /// through this so they exercise the production decision path, not just the DP.
    fn seam(f: &[u8], r: &[u8], m: &[PlaneMap; 2], b: &Band, bw: usize, h: usize) -> Vec<f64> {
        let grid = seam_grid_with_floor(f, r, m, b, bw, h, false);
        seam_cut(&grid, b, bw, h, true).map(|(_, path)| path).unwrap_or_else(|| seam_path(&grid, bw, h))
    }

    fn lens(xi: f64, fx: f64, half: f64, yaw: f64) -> filmcraft_insta360::Lens {
        filmcraft_insta360::Lens {
            xi,
            fx,
            fy: fx,
            cx: half,
            cy: half,
            yaw,
            pitch: 0.0,
            roll: 0.0,
            k1: 0.0,
            k2: 0.0,
            k3: 0.0,
            p1: 0.0,
            p2: 0.0,
            tx: 0.0,
            ty: 0.0,
            tz: 0.0,
            sensor_w: 2 * half as u32,
            sensor_h: half as u32,
        }
    }

    /// The steepstep (Studio's `alphaNormalized`) is a symmetric S-curve: fixed
    /// at 0, 0.5 and 1, steeper than linear in between (each lens owns more of
    /// the band, the 50/50 crossing is sharp).
    #[test]
    fn steepstep_is_a_symmetric_sharp_s() {
        assert_eq!(steepstep(0.0), 0.0);
        assert!((steepstep(0.5) - 0.5).abs() < 1e-9, "the midpoint stays 50/50");
        assert_eq!(steepstep(1.0), 1.0);
        // Symmetric about (0.5, 0.5).
        for t in [0.1, 0.25, 0.4] {
            assert!((steepstep(t) + steepstep(1.0 - t) - 1.0).abs() < 1e-9, "symmetric at {t}");
        }
        // Steeper than linear: below the diagonal on the near half.
        assert!(steepstep(0.25) < 0.25 && steepstep(0.75) > 0.75, "the blend lingers near 0 and 1");
        // Hostile inputs clamp rather than NaN.
        assert!((steepstep(-1.0) - 0.0).abs() < 1e-9 && (steepstep(2.0) - 1.0).abs() < 1e-9);
    }

    /// The nadir taper (Studio's `1 - (-y)^4`, floored at BLEND_MIN_DEG) narrows
    /// the crossfade below the horizon and leaves it alone above it.
    #[test]
    fn nadir_taper_narrows_below_the_horizon() {
        let taper = |lat: f64| {
            let sl = lat.sin();
            if sl < 0.0 { (1.0 - sl.powi(4)).max(BLEND_MIN_DEG / FEATHER_DEG) } else { 1.0 }
        };
        assert_eq!(taper(0.0), 1.0, "the equator is untouched");
        assert_eq!(taper(45f64.to_radians()), 1.0, "the zenith is untouched");
        assert!(taper(-45f64.to_radians()) < 1.0, "below the horizon narrows");
        assert!((taper(-90f64.to_radians()) - BLEND_MIN_DEG / FEATHER_DEG).abs() < 1e-9, "the pole hits the floor");
        assert!(taper(-90f64.to_radians()) > 0.0, "never zero (no black gap)");
    }

    #[test]
    fn gains_equalise_and_clamp() {
        assert_eq!(lens_gains(100.0, 100.0), (1.0, 1.0));
        let (g0, g1) = lens_gains(100.0, 120.0);
        assert!((g0 * 100.0 - (100.0f64 * 120.0).sqrt()).abs() < 1e-6);
        assert!((g1 * 120.0 - (100.0f64 * 120.0).sqrt()).abs() < 1e-6);
        let (g0, g1) = lens_gains(10.0, 200.0);
        assert!(g0 <= GAIN_CLAMP + 1e-9 && g1 >= 1.0 / GAIN_CLAMP - 1e-9);
        assert_eq!(lens_gains(0.0, 100.0), (1.0, 1.0));
    }

    /// The local gain map must equalise a lens exposure ratio on background that
    /// disagrees by MORE than the old [`GAIN_AGREE`] gate. The far truss of the
    /// littlebird footage disagrees by ~66 luma (one lens lit, the other in shadow)
    /// yet is genuine background, so a gate that only measures where the lenses
    /// agree within 48 never sees its ratio and leaves a hard seam across it. With
    /// the gate widened past that, a block whose rear is a constant multiple of its
    /// front must be split to their geometric mean. This is the case the old gate
    /// silently dropped.
    #[test]
    fn gain_map_equalises_a_ratio_beyond_the_old_agreement_gate() {
        let (bw, h) = (96usize, 96usize);
        // Front mid-grey, rear a constant 2x brighter: a pure exposure ratio. The
        // per-pixel disagreement (80) is past the old 48 gate but within the
        // widened one, so the block is measured where it used to be dropped.
        let f = vec![80u8; bw * h];
        let r = vec![160u8; bw * h];
        let (gm0, gm1) = local_gain_map(&f, &r, bw, h);
        let (f2, r2) = (apply_map(&f, &gm0), apply_map(&r, &gm1));
        // Both land near the geometric mean sqrt(80*160) = 113, so the seam step
        // (was 80) collapses and the local mean is preserved.
        let (mf, mr) = (f2[h / 2 * bw + bw / 2] as f64, r2[h / 2 * bw + bw / 2] as f64);
        let gm = (80.0f64 * 160.0).sqrt();
        assert!((mf - gm).abs() < 8.0 && (mr - gm).abs() < 8.0, "equalised to the geometric mean: front {mf} rear {mr} vs {gm}");
        assert!((mf - mr).abs() < 8.0, "the seam step is gone: {mf} vs {mr}");
    }

    /// A bright square at a circle's centre is what that lens sees straight ahead. The
    /// front lens (lens 0, facing +Z = lon 0) must place it at the equirect's centre; the
    /// rear lens (lens 1, facing −Z = lon ±180) at the frame edges.
    #[test]
    fn front_lens_projects_to_the_equator_centre() {
        let c = 64usize;
        let circle = |bright: bool| -> VideoFrame {
            let mut y = vec![20u8; c * c];
            if bright {
                for r in c / 2 - 4..c / 2 + 4 {
                    for col in c / 2 - 4..c / 2 + 4 {
                        y[r * c + col] = 240;
                    }
                }
            }
            VideoFrame {
                width: c as u32,
                height: c as u32,
                data: PixelData::Yuv8 {
                    planes: [Arc::new(y), Arc::new(vec![128; c * c / 4]), Arc::new(vec![128; c * c / 4])],
                    chroma: Chroma::C420,
                    alpha: None,
                },
                color: filmcraft_color::ColorInfo::REC709,
                par: (1, 1),
                pts: Tick::ZERO,
            }
        };
        let l0 = lens(1.948170, c as f64 * 0.77, c as f64 * 0.5, 0.36);
        let l1 = lens(1.948170, c as f64 * 0.77, c as f64 * 0.5, -0.49);
        let (w, h) = (128usize, 64usize);
        let s = Stitch::bake(&[l0, l1], c as u32, w, h, Chroma::C420).expect("bake");
        let f = s.apply(&SeamState::new(), &circle(true), &circle(false)).expect("front object");
        let PixelData::Yuv8 { planes, .. } = &f.data else { panic!("yuv") };
        // the front lens's object lands at lon 0 (centre column), equator row
        let centre = planes[0][h / 2 * w + w / 2];
        let edge = planes[0][h / 2 * w];
        assert!(centre > edge + 100, "the front lens's object lands at lon 0: {centre} vs {edge}");
        // the rear lens's object wraps to the frame edges (lon ±180)
        let f2 = s.apply(&SeamState::new(), &circle(false), &circle(true)).expect("rear object");
        let PixelData::Yuv8 { planes: p2, .. } = &f2.data else { panic!("yuv") };
        let (e0, e1, c2) = (p2[0][h / 2 * w], p2[0][h / 2 * w + w - 1], p2[0][h / 2 * w + w / 2]);
        assert!(e0 > c2 + 100 && e1 > c2 + 100, "rear object wraps lon ±180: {e0} {e1} vs {c2}");
    }

    /// The feather is the ray's angle from the lens axis, not the longitude: the
    /// zenith (the whole top row, one physical point) is 90 deg from BOTH horizontal
    /// lenses, so both must own it equally. A longitude ramp instead hands the
    /// pole's left half to the front lens and its right half to the rear, splitting
    /// one point two ways. Studio's `calAlpha` shares it; so must we.
    #[test]
    fn the_pole_is_shared_by_both_lenses() {
        let c = 64usize;
        let l0 = lens(1.948170, c as f64 * 0.77, c as f64 * 0.5, 0.36);
        let l1 = lens(1.948170, c as f64 * 0.77, c as f64 * 0.5, -0.49);
        let (w, h) = (128usize, 64usize);
        let s = Stitch::bake(&[l0, l1], c as u32, w, h, Chroma::C420).expect("bake");
        // Top row (near the zenith): the front lens's weight must stay near 50/50
        // across the whole row. The old longitude model gave it 255 at lon 0 and 0
        // at lon ±180 (a hard split of one point); the ray-angle model keeps every
        // column shared, because a near-zenith ray is ~90 deg from both axes.
        let (f_left, f_centre, f_right) = (s.maps[0][0].w[0], s.maps[0][0].w[w / 2], s.maps[0][0].w[w - 1]);
        for (x, &v) in [("lon -180", &f_left), ("lon 0", &f_centre), ("lon +180", &f_right)] {
            assert!(v > 40 && v < 215, "near-zenith {x} is shared, not owned: {v}");
        }
        assert!((i32::from(f_left) - i32::from(f_right)).abs() < 128, "no longitude split at the pole: {f_left} vs {f_right}");
        // Equator row: the front lens still owns lon 0 (its axis, yawed a little).
        let eq = h / 2;
        assert!(s.maps[0][0].w[eq * w + w / 2] > 200, "front owns lon 0 at the equator");
    }

    #[test]
    fn equirect_size_is_2_to_1_and_even() {
        assert_eq!(equirect_size(2880), (3840, 1920));
        assert_eq!(equirect_size(64), (80, 40));
        assert_eq!(equirect_size(0), (64, 32));
    }

    /// The ghost fix's core: `flow_dx` must recover a known along-seam shift. The
    /// rear render is the front shifted right by `d` px (nearby objects sit that
    /// much differently in the two lenses); the returned shift is what to ADD to the
    /// rear's sampling x to land it on the front, so it must be `-d`... which the
    /// caller applies as `rear(x + dx)`, i.e. `dx` reads the rear `d` to the left.
    #[test]
    fn flow_recovers_a_known_shift() {
        let (bw, h, d) = (64usize, 40usize, 5i32);
        // A textured front render (a vertical-stripe pattern carries horizontal
        // gradient everywhere, so every block can locate the shift).
        let f: Vec<u8> = (0..h).flat_map(|y| (0..bw).map(|x| (128 + 60 * ((x / 4) % 2) as i32 + (y % 3) as i32) as u8).collect::<Vec<u8>>()).collect();
        // The rear is the front sampled `d` to the right: rear(x) = front(x + d).
        let r: Vec<u8> = (0..h).flat_map(|y| (0..bw).map(|x| f[y * bw + (x + d as usize).min(bw - 1)]).collect::<Vec<u8>>()).collect();
        let dx = flow_dx(&f, &r, bw, h, None);
        // To make rear(x + dx) == front(x) we need x + dx == x + d, so dx == +d.
        assert!(dx.iter().all(|&v| (v - d as f64).abs() < 1.5), "the shift is recovered: {dx:?}");
    }

    /// A flat band has no locatable shift: with no hint the flow must be zero
    /// everywhere (no motion), not noise that shreds the seam into slices.
    #[test]
    fn flow_is_zero_on_a_flat_band() {
        let (bw, h) = (64usize, 40usize);
        let f = vec![120u8; bw * h];
        let r = vec![120u8; bw * h];
        assert!(flow_dx(&f, &r, bw, h, None).iter().all(|&v| v.abs() < 0.6));
    }

    /// Studio's hint flow (the reverse-engineered spec's §4/§7.3): a row whose
    /// blocks carry no texture keeps the previous frame's shift instead of voting
    /// zero. A dark band that was aligned last frame stays aligned — the failure
    /// mode this fixes is exactly "flow decays to zero on low-ISO truss footage,
    /// so the seam re-doubles".
    #[test]
    fn flow_keeps_the_hint_on_untextured_rows() {
        let (bw, h) = (64usize, 40usize);
        // Both renders flat: nothing can be located this frame.
        let f = vec![120u8; bw * h];
        let r = vec![120u8; bw * h];
        let hint = vec![4.0; h];
        let dx = flow_dx(&f, &r, bw, h, Some(&hint));
        assert!(dx.iter().all(|&v| (v - 4.0).abs() < 0.6), "the prior flow survives a textureless frame: {dx:?}");
    }

    /// The DP seam routes to the column where the two renders agree, away from the
    /// one where a parallax object disagrees (the ghost the crossfade would double).
    #[test]
    fn seam_avoids_the_disagreement() {
        let (bw, h) = (80usize, 48usize);
        // Identical everywhere except a bright bar in the front render's middle
        // columns: the seam should leave the centre and run through the agreeing
        // sides rather than across the doubled bar.
        let f: Vec<u8> = (0..h).flat_map(|_| (0..bw).map(|x| if (28..44).contains(&x) { 240 } else { 40 }).collect::<Vec<u8>>()).collect();
        let r: Vec<u8> = vec![40u8; bw * h];
        let m = [
            PlaneMap { idx: vec![0; bw * h], wgt: vec![0; bw * h], w: vec![255; bw * h], pw: bw, ph: h, sw: bw, sh: h },
            PlaneMap { idx: vec![0; bw * h], wgt: vec![0; bw * h], w: vec![255; bw * h], pw: bw, ph: h, sw: bw, sh: h },
        ];
        let b = Band { x0: 0, x1: bw, front_rises: true };
        let path = seam(&f, &r, &m, &b, bw, h);
        // The path must sit outside the disagreeing bar (28..44) for most rows.
        let outside = path.iter().filter(|&&p| p < 28.0 || p > 44.0).count();
        assert!(outside > h * 3 / 4, "the seam routes around the object: {path:?}");
    }

    /// The occlusion cue routes the seam to the side of the lens that actually
    /// sees a subject the rig occludes in the other (no lens guards), so the
    /// subject comes wholly from one lens instead of being cut down the middle.
    /// A subject bright in the rear lens but hidden by the truss in the front
    /// (front dark, rear bright over a wide run) has a cheap seam on either side
    /// by disagreement alone; the signed difference says which side is right.
    #[test]
    fn occlusion_cue_routes_the_seam_to_the_visible_lens() {
        let (bw, h) = (80usize, 48usize);
        // The rear lens sees a bright object the front lens does not (occluded).
        let f: Vec<u8> = vec![40u8; bw * h];
        let r: Vec<u8> = (0..h).flat_map(|_| (0..bw).map(|x| if (30..50).contains(&x) { 240 } else { 40 }).collect::<Vec<u8>>()).collect();
        let m = [
            PlaneMap { idx: vec![0; bw * h], wgt: vec![0; bw * h], w: vec![255; bw * h], pw: bw, ph: h, sw: bw, sh: h },
            PlaneMap { idx: vec![0; bw * h], wgt: vec![0; bw * h], w: vec![255; bw * h], pw: bw, ph: h, sw: bw, sh: h },
        ];
        // Band 1 (front_rises false): the front lens owns the low columns, so an
        // object the front is occluded by must come from the rear - the seam goes
        // to its LEFT (a low column).
        let b1 = Band { x0: 0, x1: bw, front_rises: false };
        let p1 = seam(&f, &r, &m, &b1, bw, h);
        assert!(p1.iter().all(|&p| p < 30.0), "band 1 routes a rear-visible object left: {p1:?}");
        // Band 0 (front_rises true) is the mirror image: the seam goes to its RIGHT.
        let b0 = Band { x0: 0, x1: bw, front_rises: true };
        let p0 = seam(&f, &r, &m, &b0, bw, h);
        assert!(p0.iter().all(|&p| p > 50.0), "band 0 routes a rear-visible object right: {p0:?}");
    }

    /// The occlusion cue's reward must be able to *pull* the seam onto a subject's
    /// visible edge, not merely push it away from the far side. A wide rear-visible
    /// object (front dark, rear bright over a long run): disagreement alone makes
    /// every column whose blend window touches the object equally expensive, so
    /// only the cue can choose where the boundary sits - and it should sit at the
    /// object's edge, giving the agreeing background just beyond it back to the
    /// front lens. Clamping the cue's negative contribution to zero (rather than
    /// flooring the total at the node's own disagreement) deletes that pull and
    /// strands the seam a blend window past the edge, holding real background
    /// hostage to the rear lens.
    #[test]
    fn occlusion_cue_pulls_the_seam_onto_the_subject_edge() {
        let (bw, h) = (160usize, 48usize);
        // The rear lens sees a bright object the front lens does not, over a wide
        // run (30..110) - wider than the blend window, so the window-max cost is
        // flat across it and cannot pick an edge on its own.
        let f: Vec<u8> = vec![40u8; bw * h];
        let r: Vec<u8> = (0..h).flat_map(|_| (0..bw).map(|x| if (30..110).contains(&x) { 240 } else { 40 }).collect::<Vec<u8>>()).collect();
        let m = [
            PlaneMap { idx: vec![0; bw * h], wgt: vec![0; bw * h], w: vec![255; bw * h], pw: bw, ph: h, sw: bw, sh: h },
            PlaneMap { idx: vec![0; bw * h], wgt: vec![0; bw * h], w: vec![255; bw * h], pw: bw, ph: h, sw: bw, sh: h },
        ];
        // Band 0: the cue must pull the seam to the object's high edge (~110), not
        // leave it stranded a window past.
        let b0 = Band { x0: 0, x1: bw, front_rises: true };
        let p0 = seam(&f, &r, &m, &b0, bw, h);
        assert!(p0.iter().all(|&p| (104.0..=116.0).contains(&p)), "the seam sits on the subject's edge: {p0:?}");
    }

    /// The 2-D graph cut encloses a doubled object in one lens's region — the
    /// property the 1-D DP cannot express. A parallax-doubled object (the same
    /// subject at two columns, disagreeing in each lens, agreeing in the gap
    /// between them) is cheap for the DP exactly in that gap: zero disagreement,
    /// and the seam then crossfades both copies at half weight (the doubling). The
    /// cut charges a boundary for the disagreement within its whole blend window,
    /// so the gap is expensive and the boundary snaps outside the cluster, giving
    /// the object wholly to one lens. The copies alternate which lens is brighter
    /// every few rows so the (signed) occlusion cue cancels and the disagreement
    /// alone decides — the state of a doubled, not occluded, subject.
    #[test]
    fn graph_cut_encloses_a_doubled_object_the_dp_splits() {
        let (bw, h) = (80usize, 48usize);
        let cluster = |x: usize| (28..44).contains(&x) || (52..68).contains(&x);
        let f: Vec<u8> = (0..h)
            .flat_map(|y| {
                let front_bright = (y / 4) % 2 == 0;
                (0..bw).map(move |x| if cluster(x) && front_bright { 240 } else { 40 }).collect::<Vec<u8>>()
            })
            .collect();
        let r: Vec<u8> = (0..h)
            .flat_map(|y| {
                let front_bright = (y / 4) % 2 == 0;
                (0..bw).map(move |x| if cluster(x) && !front_bright { 240 } else { 40 }).collect::<Vec<u8>>()
            })
            .collect();
        let m = [
            PlaneMap { idx: vec![0; bw * h], wgt: vec![0; bw * h], w: vec![255; bw * h], pw: bw, ph: h, sw: bw, sh: h },
            PlaneMap { idx: vec![0; bw * h], wgt: vec![0; bw * h], w: vec![255; bw * h], pw: bw, ph: h, sw: bw, sh: h },
        ];
        let b = Band { x0: 0, x1: bw, front_rises: true };
        let grid = seam_grid_with_floor(&f, &r, &m, &b, bw, h, false);
        // The DP sits in the agreeing gap between the copies (splitting the object).
        let dp = seam_path(&grid, bw, h);
        assert!(dp.iter().all(|&p| (44.0..=52.0).contains(&p)), "the DP splits the object: {dp:?}");
        // The cut puts the boundary outside the whole cluster: no row's seam sits
        // between the copies, so one lens owns both.
        let (labels, path) = seam_cut(&grid, &b, bw, h, true).expect("a cuttable band");
        assert!(path.iter().all(|&p| p < 28.0), "the cut encloses the object: {path:?}");
        // Every cluster column is front-owned (the high side), not split.
        assert!((28..68).all(|ci| (0..grid.nodes).all(|ni| labels[ni * grid.cols + ci] == 1)), "both copies belong to one lens");
    }

    /// On a straight vertical boundary the label field reproduces the linear
    /// crossfade the 1-D path produces (to within half a node cell), so the 2-D cut
    /// changes nothing where the seam is already clean.
    #[test]
    fn label_alpha_matches_the_linear_ramp_on_a_straight_seam() {
        let (bw, h, cols, nodes, stride) = (80usize, 48usize, 80usize, 6usize, 1usize);
        let mut labels = vec![0u8; nodes * cols];
        for ni in 0..nodes {
            for ci in 0..cols {
                labels[ni * cols + ci] = if ci < 40 {
                    0
                } else if ci == 40 {
                    2
                } else {
                    1
                };
            }
        }
        let ramp = vec![20.0; h];
        let g = SeamGrid { grid: vec![0.0; nodes * cols], dis: vec![0.0; nodes * cols], cover: vec![3; nodes * cols], cols, nodes, stride };
        let a = label_alpha(&g, &labels, &ramp, bw, h);
        for y in 0..h {
            for i in 0..bw {
                let want = (0.5 + (i as f64 - 40.5) / 20.0).clamp(0.0, 1.0);
                assert!((a[y * bw + i] - want).abs() <= 0.03, "straight seam at ({y},{i}): {} vs {want}", a[y * bw + i]);
            }
        }
    }

    /// Hostile bands: too few columns or too many nodes for a bounded graph make
    /// [`seam_cut`] decline, and [`band_plan`]'s caller keeps the 1-D DP result
    /// (AGENTS §0.4: refuse, never allocate wildly or panic).
    #[test]
    fn graph_cut_declines_on_hostile_bands() {
        // Narrow: cols < 3.
        let (bw, h) = (2usize, 48usize);
        let f = vec![40u8; bw * h];
        let r = vec![40u8; bw * h];
        let m = [
            PlaneMap { idx: vec![0; bw * h], wgt: vec![0; bw * h], w: vec![255; bw * h], pw: bw, ph: h, sw: bw, sh: h },
            PlaneMap { idx: vec![0; bw * h], wgt: vec![0; bw * h], w: vec![255; bw * h], pw: bw, ph: h, sw: bw, sh: h },
        ];
        let b = Band { x0: 0, x1: bw, front_rises: true };
        let grid = seam_grid_with_floor(&f, &r, &m, &b, bw, h, false);
        assert!(seam_cut(&grid, &b, bw, h, true).is_none(), "a two-column band has no 2-D seam to cut");
        // Tall: nodes * cols * 2 exceeds the graph budget.
        let h2 = 30_000usize;
        let f2 = vec![40u8; bw.max(160) * h2];
        let r2 = vec![40u8; bw.max(160) * h2];
        let m2 = [
            PlaneMap { idx: vec![0; 160 * h2], wgt: vec![0; 160 * h2], w: vec![255; 160 * h2], pw: 160, ph: h2, sw: 160, sh: h2 },
            PlaneMap { idx: vec![0; 160 * h2], wgt: vec![0; 160 * h2], w: vec![255; 160 * h2], pw: 160, ph: h2, sw: 160, sh: h2 },
        ];
        let b2 = Band { x0: 0, x1: 160, front_rises: true };
        let grid2 = seam_grid_with_floor(&f2, &r2, &m2, &b2, 160, h2, false);
        assert!(seam_cut(&grid2, &b2, 160, h2, true).is_none(), "an oversized band is refused, not allocated");
        // The DP fallback still answers with an in-band column for every row.
        let dp = seam_path(&grid2, 160, h2);
        assert!(dp.iter().all(|&p| (0.0..=160.0).contains(&p)), "the fallback stays in the band");
    }

    /// End to end: a parallax object in the overlap band must stitch to ONE image,
    /// not the ghost of the static crossfade. The same feature sits at two circle
    /// positions (the lenses' baseline); the aligned blend collapses them.
    #[test]
    fn aligned_blend_does_not_ghost_a_parallax_object() {
        let c = 256usize;
        let (w, h) = (336usize, 168usize); // ~circle*4/3, 2:1
        // A circle that is mid-grey with one bright vertical bar near its right edge
        // (the region that maps into the lon +90 overlap band).
        let circle_bar = |col: usize| -> VideoFrame {
            let mut y = vec![120u8; c * c];
            for r in c / 2 - 40..c / 2 + 40 {
                for x in col..col + 6 {
                    y[r * c + x] = 240;
                }
            }
            VideoFrame {
                width: c as u32,
                height: c as u32,
                data: PixelData::Yuv8 {
                    planes: [Arc::new(y), Arc::new(vec![128; c * c / 4]), Arc::new(vec![128; c * c / 4])],
                    chroma: Chroma::C420,
                    alpha: None,
                },
                color: filmcraft_color::ColorInfo::REC709,
                par: (1, 1),
                pts: Tick::ZERO,
            }
        };
        let l0 = lens(1.948170, c as f64 * 0.77, c as f64 * 0.5, 0.36);
        let l1 = lens(1.948170, c as f64 * 0.77, c as f64 * 0.5, -0.49);
        let s = Stitch::bake(&[l0, l1], c as u32, w, h, Chroma::C420).expect("bake");
        // The bar appears in both circles at the same pixel column: the two renders
        // disagree in the band (parallax). Count bright runs along the equator in the
        // lon +90 band: a ghost is two runs, an aligned blend is one.
        let f = s.apply(&SeamState::new(), &circle_bar(c * 3 / 4), &circle_bar(c * 3 / 4)).expect("apply");
        let PixelData::Yuv8 { planes, .. } = &f.data else { panic!("yuv") };
        let row = h / 2;
        let band = &planes[0][row * w + 3 * w / 4 - 12..row * w + 3 * w / 4 + 12];
        let bright = band.iter().filter(|&&v| v > 200).count();
        // The band must not be half-bright (a full ghost across the crossfade); the
        // aligned seam keeps the object in one lens's ownership for most of it.
        assert!(bright < band.len() * 3 / 4, "the parallax object is not smeared across the whole band: {bright}/{}", band.len());
    }

    /// A hostile bake size is refused, not allocated.
    #[test]
    fn bake_refuses_damaged_input() {
        let l = lens(1.948170, 40.0, 32.0, 0.0);
        assert!(Stitch::bake(&[l, l], 64, 128, 64, Chroma::C420).is_some());
        // xi <= 1 cannot drive the unified projection
        let bad = lens(0.5, 40.0, 32.0, 0.0);
        assert!(Stitch::bake(&[bad, bad], 64, 128, 64, Chroma::C420).is_none());
        // a size past the pixel cap
        assert!(Stitch::bake(&[l, l], 64, 1 << 20, 1 << 20, Chroma::C420).is_none());
    }

    /// The decision lattice: the full stitch's band decides at its own scale
    /// (byte-identical to what the constants were tuned against), and a band
    /// narrower than half that is brought up to ~the reference width so every
    /// block, window and gate of the seam decision spans the same angle the
    /// main file's does. A proxy seam that "wandered while the export sat
    /// still" was the same 30° of scene judged on a 4.6x coarser ruler.
    #[test]
    fn proxy_bands_decide_on_the_reference_lattice() {
        assert_eq!(decision_scale(426), 1, "the reference band is the ruler");
        assert_eq!(decision_scale(1 << 20), 1, "wide bands never upscale");
        let s = decision_scale(92);
        assert!((92 * s).abs_diff(DECISION_BAND) <= 92, "a proxy band lands at the reference width: {}px", 92 * s);
        assert!(decision_scale(8) * 8 >= DECISION_BAND / 2, "even a tiny band gets a usable lattice");
    }

    /// An upscaled decision field mapped back to output pixels must reproduce a
    /// straight seam exactly (the round trip only resamples constants/edges, never
    /// content): if the box-average ever drops a weight or shifts a boundary, the
    /// proxy seam stops tracking the main one in a way no footage review explains.
    #[test]
    fn up_and_back_preserve_constant_fields() {
        let (w, h, s) = (23usize, 9usize, 5usize);
        let flat = vec![77u8; w * h];
        assert_eq!(box_down_u8(&up_u8(&flat, w, h, s), w * s, h * s, w, h), flat, "a constant u8 field round-trips unchanged");
        let one = vec![0.5f64; (w * s) * (h * s)];
        assert!(box_down_f64(&one, w * s, h * s, w, h).iter().all(|&v| v == 0.5), "a constant f64 field round-trips unchanged");
        let g = vec![1.25f32; w * h];
        assert_eq!(box_down_f32(&g, w, h, w, h), g, "identity factors copy");
    }

    /// The 2-D block flow recovers a *vertical* shift that the 1-D along-seam flow
    /// cannot see: at the seam the epipolar lines are vertical, so a rear render
    /// displaced downward needs a vertical flow to align it. A textured band shifted
    /// down by a few px must yield a positive dy (rear must move up onto front) and
    /// near-zero dx; a flat band keeps the hint (zero).
    #[test]
    fn flow_2d_recovers_a_vertical_shift() {
        let (bw, h) = (128usize, 128usize);
        // A textured (checker) front render; the rear is the same shifted down 8px.
        let f: Vec<u8> = (0..h).flat_map(|y| (0..bw).map(|x| if (x / 8 + y / 8).is_multiple_of(2) { 220 } else { 30 }).collect::<Vec<u8>>()).collect();
        let shift = 8usize;
        let r: Vec<u8> =
            (0..h).flat_map(|y| (0..bw).map(|x| if (x / 8 + (y + shift) / 8).is_multiple_of(2) { 220 } else { 30 }).collect::<Vec<u8>>()).collect();
        let fl = flow_2d(&f, &r, bw, h, None);
        assert!(fl.gx > 1 && fl.gy > 1, "a grid is built: {}x{}", fl.gx, fl.gy);
        // The rear sits `shift` px below the front, so landing it on the front needs
        // a negative dy (move rear up). Check the interior blocks (edges taper).
        let mid = fl.gy / 2;
        let dy = fl.dys[mid * fl.gx + fl.gx / 2];
        assert!(dy <= -4.0, "the flow finds the vertical shift: dy {dy}");
        let dx = fl.dxs[mid * fl.gx + fl.gx / 2];
        assert!(dx.abs() <= 4.0, "no spurious horizontal shift: dx {dx}");
        // A flat band cannot locate a shift and keeps the hint (zero).
        let g = vec![90u8; bw * h];
        let ff = flow_2d(&g, &g, bw, h, None);
        assert!(ff.dxs.iter().all(|&d| d.abs() < 1e-9) && ff.dys.iter().all(|&d| d.abs() < 1e-9), "flat keeps zero");
    }

    /// The baseline-aware reprojection: a lens offset along camera Z by `b`
    /// samples the seam ray (world +X at lon +90, where the baseline is lateral)
    /// at the shifted direction (1, 0, −b), moving its normalised u from 1/xi to
    /// 1/(xi − b) — further right on the sensor, toward the reconciled point. A
    /// zero baseline must sample exactly where the co-located model does, and the
    /// two must differ, proving the term is live and signed correctly.
    #[test]
    fn baseline_shifts_the_seam_sample_to_the_reconciled_point() {
        let g = PlaneGeom { circle: 64, pw: 64, ph: 32, ssx: 0, ssy: 0 };
        let l = lens(1.948170, 32.0, 32.0, 0.0);
        let r = rotation_for(l, 0);
        let m0 = bake_plane(&l, &r, [0.0, 0.0, 0.0], g).expect("bake");
        let m1 = bake_plane(&l, &r, [0.0, 0.0, 0.1], g).expect("bake");
        // lon +90 is x = 3/4 pw; the equator is y = ph/2. The ray there is world +X.
        let at = (16) * 64 + 48;
        assert!(m0.w[at] > 0 && m1.w[at] > 0, "both cover the seam ray");
        let (i0, i1) = (i64::from(m0.idx[at]), i64::from(m1.idx[at]));
        assert_ne!(i0, i1, "a baseline moves the sample");
        // u grows (1/(xi-b) > 1/xi), so the sensor x grows: the packed top-left
        // index (row-major over the circle) must increase.
        assert!(i1 > i0, "the sample moves right with a +Z baseline: {i0} -> {i1}");
        // A zero baseline is byte-identical to the co-located model everywhere.
        let m2 = bake_plane(&l, &r, [0.0, 0.0, 0.0], g).expect("bake");
        assert_eq!(m0.idx, m2.idx);
        assert_eq!(m0.w, m2.w);
    }

    /// `resample_2d` with a constant flow warps the rear by exactly that shift, and
    /// the taper drives it to zero at the band edges so the warped rear meets the
    /// unwarped render the static feather uses outside the band.
    #[test]
    fn resample_2d_applies_and_tapers_the_flow() {
        let (bw, h) = (64usize, 64usize);
        // A single bright row at y=24 across the full width; sample it back with a
        // +4 dy flow (dst[y] reads src[y+4]).
        let src: Vec<u8> = (0..h).flat_map(|y| (0..bw).map(|_| if y == 24 { 240 } else { 10 }).collect::<Vec<u8>>()).collect();
        let f = Flow2 { dxs: vec![0.0; 4], dys: vec![4.0; 4], gx: 2, gy: 2, bsx: (bw / 2) as f64, bsy: (h / 2) as f64, sx: 1.0, sy: 1.0 };
        let win: Vec<f64> = (0..bw).map(|i| 0.5 - 0.5 * (std::f64::consts::TAU * (i as f64 + 0.5) / bw as f64).cos()).collect();
        let mut dst = vec![0u8; bw * h];
        resample_2d(&src, bw, bw, 0, &f, &win, &mut dst);
        // dst[y] samples src[y + dy], so the bright source row 24 lands on row 20
        // where the taper weight is (near) full, i.e. in the band's middle.
        let mid = bw / 2;
        assert!(dst[20 * bw + mid] > 200, "the flow moves the feature up 4px: {}", dst[20 * bw + mid]);
        assert!(dst[24 * bw + mid] < 60, "the source row is vacated at full weight");
        // At the band's left edge the taper zeroes the flow, so the feature stays put.
        assert!(dst[24 * bw + 1] > 200, "the edge is untapered-to-zero (unwarped): {}", dst[24 * bw + 1]);
    }

    /// A degenerate band (smaller than the flow grid) yields a zero field rather than
    /// panicking or indexing out of bounds.
    #[test]
    fn flow_2d_survives_a_tiny_band() {
        let (bw, h) = (8usize, 8usize);
        let g = vec![90u8; bw * h];
        let fl = flow_2d(&g, &g, bw, h, None);
        assert!(fl.dxs.iter().all(|&d| d == 0.0), "tiny band gives zero flow");
        let mut dst = vec![0u8; bw * h];
        resample_2d(&g, bw, bw, 0, &fl, &vec![1.0; bw], &mut dst);
        assert_eq!(dst, g, "zero flow is the identity");
    }

    /// A frame whose planes do not match the baked layout must fail to stitch,
    /// not panic mid-remap: a short plane indexed past its end, or chroma at a
    /// different subsampling (which the row stride would silently mis-sample),
    /// both come from damaged files.
    #[test]
    fn apply_rejects_frames_that_do_not_match_the_bake() {
        let c = 64usize;
        let (w, h) = (128usize, 64usize);
        let s = Stitch::bake(
            &[lens(1.948170, c as f64 * 0.77, c as f64 * 0.5, 0.36), lens(1.948170, c as f64 * 0.77, c as f64 * 0.5, 0.36 + std::f64::consts::PI)],
            c as u32,
            w,
            h,
            Chroma::C420,
        )
        .unwrap();
        let circle = |planes: [Vec<u8>; 3], chroma: Chroma| VideoFrame {
            width: c as u32,
            height: c as u32,
            data: PixelData::Yuv8 { planes: planes.map(Arc::new), chroma, alpha: None },
            color: filmcraft_color::ColorInfo::REC709,
            par: (1, 1),
            pts: Tick::ZERO,
        };
        let good = |v: u8| circle([vec![v; c * c], vec![128; c * c / 4], vec![128; c * c / 4]], Chroma::C420);
        let (gl, gr) = (good(40), good(200));
        assert!(s.apply(&SeamState::new(), &gl, &gr).is_some(), "the matching frames must still stitch");
        // A luma plane that is too short for the map's source geometry.
        let short = circle([Arc::new(vec![40; 16]).to_vec(), vec![128; c * c / 4], vec![128; c * c / 4]], Chroma::C420);
        assert!(s.apply(&SeamState::new(), &short, &gr).is_none(), "a short plane must decode to an error, not panic");
        // Chroma at the wrong subsampling: full-sized planes, so only the layout
        // check catches it - sampling through the wrong stride would not panic.
        let wrong = circle([vec![40; c * c], vec![128; c * c], vec![128; c * c]], Chroma::C444);
        assert!(s.apply(&SeamState::new(), &wrong, &gr).is_none(), "mismatched chroma must be refused, not mis-sampled");
        // Equal area, different dimensions (128x32 for a 64x64 bake): every
        // plane is long enough, so only the geometry check catches that the
        // sample would walk 128-wide rows at the baked 64-wide stride.
        let (cw, ch) = (128usize, 32usize);
        let wide = VideoFrame {
            width: cw as u32,
            height: ch as u32,
            data: PixelData::Yuv8 {
                planes: [Arc::new(vec![40; cw * ch]), Arc::new(vec![128; cw * ch / 4]), Arc::new(vec![128; cw * ch / 4])],
                chroma: Chroma::C420,
                alpha: None,
            },
            color: filmcraft_color::ColorInfo::REC709,
            par: (1, 1),
            pts: Tick::ZERO,
        };
        assert!(s.apply(&SeamState::new(), &wide, &gr).is_none(), "same area, wrong dimensions must be refused");
        let tall = VideoFrame { width: ch as u32, height: cw as u32, ..wide };
        assert!(s.apply(&SeamState::new(), &tall, &gr).is_none(), "the mirror of that too");
    }

    /// The blend-window disagreement charge must be symmetric about the seam:
    /// moving a doubled object to the mirrored side of a symmetric cost surface
    /// has to mirror where the seam is placed. The old window only looked left
    /// of the candidate column, so a blob sitting to a seam's right was free and
    /// the seam parked on the blob's left edge whatever the mirrored case said.
    #[test]
    fn seam_cost_window_is_symmetric_about_the_seam() {
        let cols = 32usize;
        let place = |spike: usize| -> usize {
            let dis: Vec<f64> = (0..cols).map(|c| if c == spike { 100.0 } else { 0.0 }).collect();
            let grid: Vec<f64> = dis.iter().enumerate().map(|(c, d)| d + (c as f64 - 15.5).powi(2) * 1e-3).collect();
            let g = SeamGrid { grid, dis, cover: vec![3; cols], cols, nodes: 1, stride: 1 };
            let b = Band { x0: 0, x1: cols, front_rises: true };
            let (_, path) = seam_cut(&g, &b, cols, 1, true).expect("a 32-column band cuts");
            path[0] as usize
        };
        assert_eq!(place(15) + place(16), cols - 1, "a mirrored disagreement blob must mirror the seam's placement");
    }
}

#[cfg(test)]
mod flow_tests {
    use super::MaxFlow;

    /// Push-relabel max flow against brute-force min cut on small random
    /// directed graphs: the seam solver depends on it, and a discharged node,
    /// a gap quarantine or a global relabel that loses one unit of flow is
    /// invisible in footage and fatal in the cut.
    #[test]
    fn push_relabel_matches_min_cut_by_enumeration() {
        let mut st = 0x12345678u32;
        let mut rng = move || {
            st ^= st << 13;
            st ^= st >> 17;
            st ^= st << 5;
            st
        };
        for round in 0..240 {
            let n = 4 + (rng() as usize % 3);
            let (s, t) = (0usize, n - 1);
            let mut mf = MaxFlow::new(n);
            let mut edges = Vec::new();
            let m = 2 + (rng() as usize % (n * 2));
            for _ in 0..m {
                let (u, v) = ((rng() as usize) % n, (rng() as usize) % n);
                let c = 1 + (rng() as i64 % 7);
                if u != v {
                    mf.add_edge(u, v, c);
                    edges.push((u, v, c));
                }
            }
            let got = mf.max_flow(s, t);
            let want = (0..(1usize << (n - 2)))
                .map(|mask| {
                    (0..edges.len())
                        .filter(|&i| {
                            let (u, v, _) = edges[i];
                            let su = u == s || (u != t && mask & (1 << (u - 1)) != 0);
                            let sv = v == s || (v != t && mask & (1 << (v - 1)) != 0);
                            su && !sv
                        })
                        .map(|i| edges[i].2)
                        .sum::<i64>()
                })
                .min()
                .unwrap();
            assert_eq!(got, want, "round {round}");
            // The cut the labels would come from must be a real s–t cut of that
            // value: enumerate the crossing edges of a fresh copy.
            let side = mf.min_cut_side(s);
            assert!(side[s] && !side[t]);
            let sum = edges.iter().filter(|(u, v, _)| side[*u] && !side[*v]).map(|&(_, _, c)| c).sum::<i64>();
            assert_eq!(sum, want, "round {round}: the reachable side is a genuine min-cut");
            let mut mf2 = MaxFlow::new(n);
            for &(u, v, c) in &edges {
                mf2.add_edge(u, v, c);
            }
            assert_eq!(got, mf2.dinic_flow(s, t), "round {round}: the reference solver agrees");
        }
    }

    /// The dual chain must reproduce the reference max-flow's LABELS, not
    /// merely its value: the residual source side is the smallest side among
    /// all min cuts, and a plain leftmost-parent shortest path lands
    /// elsewhere on equal-cost runs (which showed as full-res artifacts).
    /// Ties, forced detours and single-coverage pins, in both band mirrors.
    #[test]
    fn the_dual_reproduces_the_reference_cut_labels() {
        use super::{Band, GC_SCALE, MaxFlow, SeamGrid};
        struct Case {
            front_rises: bool,
            /// Rear-only coverage pins: (row, canonical column).
            pins: Vec<(usize, usize)>,
        }
        for case in [
            Case { front_rises: true, pins: vec![] },
            Case { front_rises: false, pins: vec![] },
            Case { front_rises: true, pins: vec![(4, 5), (6, 5)] },
            Case { front_rises: false, pins: vec![(5, 4)] },
        ] {
            let (nodes, cols) = (10usize, 16usize);
            let cost = |ni: usize, c: usize| -> f64 {
                let wide = (4..=12).contains(&c);
                let narrow = (6..=8).contains(&c);
                if ni < 5 {
                    if wide { 5.0 } else { 40.0 }
                } else if narrow {
                    5.0
                } else {
                    40.0
                }
            };
            // Canonical = front_rises; the other mirror flips columns.
            let can = |c: usize| if case.front_rises { c } else { cols - 1 - c };
            let mut grid = vec![0f64; nodes * cols];
            let dis = vec![2f64; nodes * cols];
            let mut cover = vec![3u8; nodes * cols];
            for (ni, c) in &case.pins {
                cover[ni * cols + can(*c)] &= !1; // rear lens only
            }
            for ni in 0..nodes {
                for ci in 0..cols {
                    grid[ni * cols + ci] = if ci == 0 || ci + 1 == cols {
                        40.0 // single-coverage band edges cost something too
                    } else {
                        cost(ni, ci)
                    };
                }
            }
            let g = SeamGrid { grid, dis, cover, cols, nodes, stride: 1 };
            let b = Band { x0: 0, x1: cols, front_rises: case.front_rises };
            let (labels, path) = super::seam_cut(&g, &b, cols, nodes * 8, true).expect("a cut");
            // Independent reference: the old graph, solved by the retained
            // engine, read out the old way.
            let cap: Vec<i64> = (0..nodes * cols).map(|p| (g.grid[p] * GC_SCALE).round() as i64).collect();
            let big = cap.iter().sum::<i64>() + 1;
            let cell = |p: usize| 2 + 2 * p;
            let (front_c, rear_c) = if case.front_rises { (cols - 1, 0) } else { (0, cols - 1) };
            let mut mf = MaxFlow::new(2 + 2 * nodes * cols);
            for ni in 0..nodes {
                for ci in 0..cols {
                    let p = ni * cols + ci;
                    mf.add_edge(cell(p), cell(p) + 1, cap[p]);
                    if ci + 1 < cols {
                        mf.add_edge(cell(p) + 1, cell(p + 1), big);
                        mf.add_edge(cell(p + 1) + 1, cell(p), big);
                    }
                    if ni + 1 < nodes {
                        mf.add_edge(cell(p) + 1, cell(p + cols), big);
                        mf.add_edge(cell(p + cols) + 1, cell(p), big);
                    }
                    if g.cover[p] == 2 {
                        mf.add_edge(0, cell(p), big); // rear only: to source
                    } else if g.cover[p] == 1 {
                        mf.add_edge(cell(p) + 1, 1, big); // front only: to sink
                    }
                }
                mf.add_edge(cell(ni * cols + front_c) + 1, 1, big);
                mf.add_edge(0, cell(ni * cols + rear_c), big);
            }
            let want = mf.max_flow(0, 1);
            let side = mf.min_cut_side(0);
            let mut wl = vec![0u8; nodes * cols];
            for p in 0..nodes * cols {
                wl[p] = match (side[cell(p)], side[cell(p) + 1]) {
                    (true, true) => 0,
                    (false, false) => 1,
                    (true, false) => 2,
                    (false, true) => 1,
                };
            }
            assert_eq!(labels, wl, "labels must equal the reference ({:?})", case.front_rises);
            // The dual's own value must match the reference flow.
            let dual_cost: i64 = (0..nodes)
                .map(|ni| {
                    let row = ni * cols;
                    let bnd = labels[row..row + cols].iter().position(|&l| l == 2).unwrap();
                    cap[row + bnd]
                })
                .sum();
            assert_eq!(dual_cost, want, "and the same cost, mirror {:?}", case.front_rises);
            // Path readout agrees with the labels.
            for ni in 0..nodes {
                let seam = if case.front_rises {
                    labels[ni * cols..(ni + 1) * cols].iter().position(|&l| l != 0).unwrap()
                } else {
                    labels[ni * cols..(ni + 1) * cols].iter().rposition(|&l| l != 0).map_or(cols - 1, |i| i + 1)
                };
                let got = labels.iter().enumerate().find(|(p, l)| **l == 2 && *p / cols == ni);
                assert!(got.is_some(), "boundary in every row {ni}");
                assert_eq!(path[ni * 8], seam as f64);
            }
        }
    }

    /// Differential fuzz: on random cost surfaces and coverage patterns
    /// (including single-lens zones at the band edges, exactly what two
    /// overlapping circle footprints produce), the dual chain must give the
    /// same labels and seam columns as the graph cut it dualises — value
    /// equality between flow engines alone would not catch a wrong tie rule,
    /// which is what drew the artifacts. Zones run [rear-only][both][front-
    /// only] in canonical order, mirrored per band; a crossed pair of pins
    /// (front-only left of rear-only in a row) admits no finite-boundary cut
    /// and is out of scope — the real geometry cannot present one.
    #[test]
    fn dual_and_graph_cuts_agree_on_random_grids() {
        use super::{Band, SeamGrid, seam_cut, seam_cut_by_graph};
        let mut st = 0x9e3779b9u32;
        let mut rng = move || {
            st ^= st << 13;
            st ^= st >> 17;
            st ^= st << 5;
            st
        };
        for round in 0..400 {
            let nodes = 2 + (rng() as usize % 5);
            let cols = 5 + (rng() as usize % 4);
            let mut grid = vec![0f64; nodes * cols];
            let mut dis = vec![0f64; nodes * cols];
            for p in 0..nodes * cols {
                dis[p] = (rng() % 4) as f64;
                grid[p] = dis[p] + (rng() % 2) as f64;
            }
            for front_rises in [true, false] {
                let mut cover = vec![3u8; nodes * cols];
                for ni in 0..nodes {
                    let (a, z) = (rng() as usize % 3, rng() as usize % 3);
                    for c in 0..a.min(cols) {
                        cover[ni * cols + if front_rises { c } else { cols - 1 - c }] = 2;
                    }
                    for c in (cols - z.min(cols))..cols {
                        cover[ni * cols + if front_rises { c } else { cols - 1 - c }] &= !2;
                    }
                }
                let g = SeamGrid { grid: grid.clone(), dis: dis.clone(), cover: cover.clone(), cols, nodes, stride: 1 };
                let b = Band { x0: 0, x1: cols, front_rises };
                let dual = seam_cut(&g, &b, cols, nodes * 8, true);
                let graph = seam_cut_by_graph(&g, &b, cols, nodes * 8);
                assert_eq!(dual.is_some(), graph.is_some(), "round {round} mirror {front_rises}");
                if let (Some((dl, dp)), Some((gl, gp))) = (dual, graph) {
                    assert_eq!(dl, gl, "round {round} mirror {front_rises}: labels");
                    assert_eq!(dp, gp, "round {round} mirror {front_rises}: path");
                }
            }
        }
    }

    /// Differential fuzz: fed the same prepared costs and bounds, the
    /// transposed-table step kernel must produce exactly the label grid the
    /// row-major reference does, round for round and mirror for mirror, at
    /// both table widths — the two solvers differ only in the layout their
    /// scans run in, so any disagreement is a coordinate bug.
    #[test]
    fn the_transposed_dp_matches_the_row_major_dp() {
        use super::{Band, SeamGrid, seam_cut_intervals_ref, seam_cut_intervals_t};
        let mut st = 0xdead_beeff32a_u64;
        let mut rng = move || {
            st ^= st << 13;
            st ^= st >> 7;
            st ^= st << 17;
            st
        };
        for round in 0..300 {
            let nodes = 1 + (rng() as usize % 9);
            let cols = 3 + (rng() as usize % 10);
            let mut grid = vec![0f64; nodes * cols];
            for p in 0..nodes * cols {
                grid[p] = (rng() % 97) as f64;
            }
            let dis = grid.clone();
            let mut cover = vec![3u8; nodes * cols];
            for ni in 0..nodes {
                if rng() % 3 == 0 {
                    let c = (rng() as usize) % cols;
                    cover[ni * cols + c] = if rng() % 2 == 0 { 1 } else { 2 };
                }
            }
            for front_rises in [true, false] {
                let (mut sa_max, mut b_min) = (vec![cols.saturating_sub(1); nodes], vec![0usize; nodes]);
                for (ni, (sm, bl)) in sa_max.iter_mut().zip(b_min.iter_mut()).enumerate() {
                    for ci in 0..cols {
                        let cov = cover[ni * cols + ci];
                        let c = if front_rises { ci } else { cols - 1 - ci };
                        if cov & 1 != 0 && cov & 2 == 0 {
                            *sm = (*sm).min(c);
                        } else if cov & 2 != 0 && cov & 1 == 0 {
                            *bl = (*bl).max(c);
                        }
                    }
                }
                let capi: Vec<i64> = grid.iter().map(|&x| x.round().max(0.0) as i64 * 64).collect();
                if round == 1 {
                    eprintln!("CASE rise {front_rises} cols {cols} nodes {nodes} grid {grid:?} cover {cover:?} sa {sa_max:?} bm {b_min:?} cap {capi:?}");
                }
                let g = SeamGrid { grid: grid.clone(), dis: dis.clone(), cover: cover.clone(), cols, nodes, stride: 1 };
                let b = Band { x0: 0, x1: cols, front_rises };
                for big in [false, true] {
                    // big forces the u32 tables past their dispatch threshold when true
                    let capv: Vec<i64> = if big { capi.iter().map(|&x| x + (1i64 << 40)).collect() } else { capi.clone() };
                    let t = seam_cut_intervals_t::<u64>(&g, &b, &capv, &sa_max, &b_min);
                    let r = seam_cut_intervals_ref::<u64>(&g, &b, &capv, &sa_max, &b_min);
                    assert_eq!(t, r, "round {round} mirror {front_rises} wide {big}: labels");
                }
                let t32 = seam_cut_intervals_t::<u32>(&g, &b, &capi, &sa_max, &b_min);
                let r64 = seam_cut_intervals_ref::<u64>(&g, &b, &capi, &sa_max, &b_min);
                assert_eq!(t32, r64, "round {round}: the u32 tables agree too");
            }
        }
    }

    /// The shape a seam graph has — a ladder whose every s→t path snakes the
    /// whole length — solved by both engines: same flow, and where the cut
    /// crosses must agree too (tie-free costs: distinct prime column weights).
    #[test]
    fn expanding_trees_match_dinic_on_a_seam_ladder() {
        let (rows, cols) = (240usize, 120usize);
        let cell = |r: usize, c: usize| 2 + 2 * (r * cols + c);
        let mut mf = MaxFlow::new(2 + 2 * rows * cols);
        let mut md = MaxFlow::new(2 + 2 * rows * cols);
        let big = 10i64.pow(9);
        for r in 0..rows {
            for c in 0..cols {
                let w = 1000 + (r * cols + c) as i64 % (cols as i64 - 1) + 1;
                for m in [&mut mf, &mut md] {
                    m.add_edge(cell(r, c), cell(r, c) + 1, w * 8);
                    if c + 1 < cols {
                        m.add_edge(cell(r, c) + 1, cell(r, c + 1), big);
                        m.add_edge(cell(r, c + 1) + 1, cell(r, c), big);
                    }
                    if r + 1 < rows {
                        m.add_edge(cell(r, c) + 1, cell(r + 1, c), big);
                        m.add_edge(cell(r + 1, c) + 1, cell(r, c), big);
                    }
                    if c == 0 {
                        m.add_edge(0, cell(r, c), big);
                    }
                    if c + 1 == cols {
                        m.add_edge(cell(r, c) + 1, 1, big);
                    }
                }
            }
        }
        let a = mf.max_flow(0, 1);
        let b = md.dinic_flow(0, 1);
        assert_eq!(a, b, "flow values agree");
        assert_eq!(mf.min_cut_side(0), md.min_cut_side(0), "the snake lands in the same place");
    }
}
