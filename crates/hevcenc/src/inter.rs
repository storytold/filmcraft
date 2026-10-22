//! Inter prediction: motion compensation, merge list and motion search (phase 1, 8-bit 4:2:0, P-frames only).
//!
//! Luma uses the 8-tap filter of §8.5.3.3.3.2 and chroma the 4-tap filter of §8.5.3.3.3.3;
//! the taps live in `tables::LUMA_FILTER` / `CHROMA_FILTER`. Bit depth is 8, so shift1 = 0, shift2 = 6,
//! shift3 = 6 and the final offset is 32; this matches `hevc::inter` sample-exactly (same tap rows by fractional
//! part, same shifts, same clip). Merge follows §8.5.3.2.3 pruning + §8.5.3.2.5 zero fill (no temporal: our SPS
//! disables TMVP; no combined bi-predictive: no B slices in phase 1).

use crate::Error;
use crate::dsp::{sad, satd};
use crate::picture::Plane;
use crate::tables::{CHROMA_FILTER, LUMA_FILTER};

/// Motion vector in quarter-pel luma units; phase 1 keeps it inside ±252 (within the VUI-declared ±256).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mv {
    pub x: i32,
    pub y: i32,
}

/// One merge candidate: the neighbour motion vector (quarter-pel) plus its reference index (phase 1: 0).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MergeCand {
    pub mv: Mv,
    pub ref_idx: i32,
}

/// Padded stride helper: `pad = (stride - w) / 2`, mirroring `Plane::new` (saturating: hostile planes give 0).
fn pad_of(plane: &Plane) -> usize {
    plane.stride.saturating_sub(plane.w) / 2
}

/// Final 8-bit rounding shared by every path: `Clip((v + 32) >> 6)`, i.e. §8.5.3.3.4.2 with shift 6.
#[inline]
fn clip_round(v: i32) -> u8 {
    ((v + 32) >> 6).clamp(0, 255) as u8
}

/// Separable luma prediction into dense `out` (row stride `w`); caller guarantees the full 8-tap window
/// `[xi-3, xi+w+3] x [yi-3, yi+h+3]` lies inside the padded buffer and `out.len() >= w*h`, so plain indexing
/// below cannot go out of bounds (bounds are validated once in `mc_luma` / `search`, keeping the hot loop fast).
fn luma_pred_into(data: &[u8], stride: usize, pad: usize, xi: i64, yi: i64, fx: usize, fy: usize, w: usize, h: usize, out: &mut [u8]) {
    let pad_i = pad as i64;
    if fx == 0 && fy == 0 {
        // Integer position: direct copy, no filtering and no rounding.
        let sx = (xi + pad_i) as usize;
        let sy = (yi + pad_i) as usize;
        for r in 0..h {
            let src = (sy + r) * stride + sx;
            let dst = r * w;
            out[dst..dst + w].copy_from_slice(&data[src..src + w]);
        }
        return;
    }
    if fy == 0 {
        // Horizontal only: one 8-tap pass (shift1 = 0) then the final round (shift3 = 6).
        let f = &LUMA_FILTER[fx];
        let yb = (yi + pad_i) as usize;
        let xb = (xi - 3 + pad_i) as usize;
        for r in 0..h {
            let row = (yb + r) * stride + xb;
            for c in 0..w {
                let mut s = 0i32;
                for k in 0..8 {
                    s += f[k] as i32 * data[row + c + k] as i32;
                }
                out[r * w + c] = clip_round(s);
            }
        }
        return;
    }
    if fx == 0 {
        // Vertical only: symmetric to the horizontal path.
        let f = &LUMA_FILTER[fy];
        let yb = (yi - 3 + pad_i) as usize;
        let xb = (xi + pad_i) as usize;
        for r in 0..h {
            for c in 0..w {
                let mut s = 0i32;
                for k in 0..8 {
                    s += f[k] as i32 * data[(yb + r + k) * stride + xb + c] as i32;
                }
                out[r * w + c] = clip_round(s);
            }
        }
        return;
    }
    // Both axes: horizontal pass into `tmp` (shift1 = 0), vertical pass with shift2 = 6, final round with shift3 = 6.
    // Caps in the callers keep `w,h <= 64`, so `(h+7)*w <= 4544` and this one allocation stays tiny.
    let mut tmp = vec![0i32; (h + 7) * w];
    let fh = &LUMA_FILTER[fx];
    let fv = &LUMA_FILTER[fy];
    let y0 = (yi - 3 + pad_i) as usize;
    let x0 = (xi - 3 + pad_i) as usize;
    for tr in 0..h + 7 {
        let row = (y0 + tr) * stride + x0;
        for c in 0..w {
            let mut s = 0i32;
            for k in 0..8 {
                s += fh[k] as i32 * data[row + c + k] as i32;
            }
            tmp[tr * w + c] = s;
        }
    }
    for r in 0..h {
        for c in 0..w {
            let mut s = 0i32;
            for k in 0..8 {
                s += fv[k] as i32 * tmp[(r + k) * w + c];
            }
            out[r * w + c] = clip_round(s >> 6);
        }
    }
}

/// Separable chroma prediction; same contract as `luma_pred_into` but with the 4-tap `CHROMA_FILTER` rows and
/// the smaller `(-1..+2)` window `[xi-1, xi+w+1] x [yi-1, yi+h+1]`.
fn chroma_pred_into(data: &[u8], stride: usize, pad: usize, xi: i64, yi: i64, fx: usize, fy: usize, w: usize, h: usize, out: &mut [u8]) {
    let pad_i = pad as i64;
    if fx == 0 && fy == 0 {
        let sx = (xi + pad_i) as usize;
        let sy = (yi + pad_i) as usize;
        for r in 0..h {
            let src = (sy + r) * stride + sx;
            let dst = r * w;
            out[dst..dst + w].copy_from_slice(&data[src..src + w]);
        }
        return;
    }
    if fy == 0 {
        let f = &CHROMA_FILTER[fx];
        let yb = (yi + pad_i) as usize;
        let xb = (xi - 1 + pad_i) as usize;
        for r in 0..h {
            let row = (yb + r) * stride + xb;
            for c in 0..w {
                let mut s = 0i32;
                for k in 0..4 {
                    s += f[k] as i32 * data[row + c + k] as i32;
                }
                out[r * w + c] = clip_round(s);
            }
        }
        return;
    }
    if fx == 0 {
        let f = &CHROMA_FILTER[fy];
        let yb = (yi - 1 + pad_i) as usize;
        let xb = (xi + pad_i) as usize;
        for r in 0..h {
            for c in 0..w {
                let mut s = 0i32;
                for k in 0..4 {
                    s += f[k] as i32 * data[(yb + r + k) * stride + xb + c] as i32;
                }
                out[r * w + c] = clip_round(s);
            }
        }
        return;
    }
    // Caps keep `(h+3)*w <= 35*64`, so this stays a small single allocation.
    let mut tmp = vec![0i32; (h + 3) * w];
    let fh = &CHROMA_FILTER[fx];
    let fv = &CHROMA_FILTER[fy];
    let y0 = (yi - 1 + pad_i) as usize;
    let x0 = (xi - 1 + pad_i) as usize;
    for tr in 0..h + 3 {
        let row = (y0 + tr) * stride + x0;
        for c in 0..w {
            let mut s = 0i32;
            for k in 0..4 {
                s += fh[k] as i32 * data[row + c + k] as i32;
            }
            tmp[tr * w + c] = s;
        }
    }
    for r in 0..h {
        for c in 0..w {
            let mut s = 0i32;
            for k in 0..4 {
                s += fv[k] as i32 * tmp[(r + k) * w + c];
            }
            out[r * w + c] = clip_round(s >> 6);
        }
    }
}

/// Luma motion compensation of a `w x h` block at visible origin `(x, y)` displaced by `mv` (quarter-pel).
/// `out` holds the dense `w*h` prediction (row stride `w`). Reads outside the visible picture rely on the
/// caller's `extend_edges` padding (8-tap needs 3 samples left/top and 4/3 right/bottom). Only `out.len()`
/// is validated here (`Error::InvalidConfig`); the picture-side contract is documented above and checked once.
pub fn mc_luma(ref_plane: &Plane, x: usize, y: usize, mv: Mv, w: usize, h: usize, out: &mut [u8]) -> Result<(), Error> {
    if w == 0 || h == 0 || w > 64 || h > 64 {
        return Err(Error::InvalidConfig(format!("inter luma block {w}x{h} outside 1..=64")));
    }
    let need = match w.checked_mul(h) {
        Some(n) => n,
        None => return Err(Error::InvalidConfig("luma block size overflows".to_string())),
    };
    if out.len() < need {
        return Err(Error::InvalidConfig(format!("luma out holds {} samples, need {need}", out.len())));
    }
    if mv.x < -252 || mv.x > 252 || mv.y < -252 || mv.y > 252 {
        return Err(Error::InvalidConfig(format!("luma MV ({}, {}) outside ±252", mv.x, mv.y)));
    }
    let x_end = match x.checked_add(w) {
        Some(e) => e,
        None => return Err(Error::InvalidConfig("luma x origin overflows".to_string())),
    };
    let y_end = match y.checked_add(h) {
        Some(e) => e,
        None => return Err(Error::InvalidConfig("luma y origin overflows".to_string())),
    };
    if x_end > ref_plane.w || y_end > ref_plane.h {
        return Err(Error::InvalidConfig("luma block extends past the visible picture".to_string()));
    }
    if ref_plane.stride == 0 {
        return Err(Error::InvalidConfig("reference plane has zero stride".to_string()));
    }
    // Quarter-pel split per §8.5.3.3.3.1 (8-214..8-217): arithmetic shift floors negatives, `& 3` is the sub-part.
    let fx = (mv.x & 3) as usize;
    let fy = (mv.y & 3) as usize;
    let xi = x as i64 + (mv.x as i64 >> 2);
    let yi = y as i64 + (mv.y as i64 >> 2);
    let pad = pad_of(ref_plane);
    let rows = ref_plane.data.len() / ref_plane.stride;
    let (pad_i, stride_i, rows_i) = (pad as i64, ref_plane.stride as i64, rows as i64);
    let (x0, y0) = (xi - 3, yi - 3);
    if x0 + pad_i < 0 || x0 + w as i64 + 7 + pad_i > stride_i || y0 + pad_i < 0 || y0 + h as i64 + 7 + pad_i > rows_i {
        return Err(Error::InvalidConfig("luma reference window leaves the padded picture".to_string()));
    }
    luma_pred_into(&ref_plane.data, ref_plane.stride, pad, xi, yi, fx, fy, w, h, out);
    Ok(())
}

/// Chroma motion compensation of both planes for a `w x h` chroma block at chroma origin `(x, y)`.
/// The luma `mv` (quarter-pel) converts per §8.5.3.2.10 (8-210..8-211): `mvC = mv*2/Sub`, and with 4:2:0
/// `SubWidthC = SubHeightC = 2`, so `mvC == mv` numerically but in eighth-chroma-sample units; the integer
/// part is `mvC >> 3` chroma samples and the fraction `mvC & 7` selects the `CHROMA_FILTER` row. Callers pass
/// the already-halved chroma origin (luma/2); padding needs 1 sample left/top and 2/1 right/bottom.
pub fn mc_chroma(ref_plane: &Plane, x: usize, y: usize, mv: Mv, w: usize, h: usize, out_u: &mut [u8], out_v: &mut [u8]) -> Result<(), Error> {
    if w == 0 || h == 0 || w > 64 || h > 64 {
        return Err(Error::InvalidConfig(format!("inter chroma block {w}x{h} outside 1..=64")));
    }
    let need = match w.checked_mul(h) {
        Some(n) => n,
        None => return Err(Error::InvalidConfig("chroma block size overflows".to_string())),
    };
    if out_u.len() < need || out_v.len() < need {
        return Err(Error::InvalidConfig(format!("chroma out holds {}/{} samples, need {need}", out_u.len(), out_v.len())));
    }
    if mv.x < -252 || mv.x > 252 || mv.y < -252 || mv.y > 252 {
        return Err(Error::InvalidConfig(format!("chroma MV ({}, {}) outside ±252", mv.x, mv.y)));
    }
    let x_end = match x.checked_add(w) {
        Some(e) => e,
        None => return Err(Error::InvalidConfig("chroma x origin overflows".to_string())),
    };
    let y_end = match y.checked_add(h) {
        Some(e) => e,
        None => return Err(Error::InvalidConfig("chroma y origin overflows".to_string())),
    };
    if x_end > ref_plane.w || y_end > ref_plane.h {
        return Err(Error::InvalidConfig("chroma block extends past the visible picture".to_string()));
    }
    if ref_plane.stride == 0 {
        return Err(Error::InvalidConfig("reference plane has zero stride".to_string()));
    }
    // Eighth-pel split per §8.5.3.3.3.1 (8-218..8-221) on the derived mvC.
    let fx = (mv.x & 7) as usize;
    let fy = (mv.y & 7) as usize;
    let xi = x as i64 + (mv.x as i64 >> 3);
    let yi = y as i64 + (mv.y as i64 >> 3);
    let pad = pad_of(ref_plane);
    let rows = ref_plane.data.len() / ref_plane.stride;
    let (pad_i, stride_i, rows_i) = (pad as i64, ref_plane.stride as i64, rows as i64);
    let (x0, y0) = (xi - 1, yi - 1);
    if x0 + pad_i < 0 || x0 + w as i64 + 3 + pad_i > stride_i || y0 + pad_i < 0 || y0 + h as i64 + 3 + pad_i > rows_i {
        return Err(Error::InvalidConfig("chroma reference window leaves the padded picture".to_string()));
    }
    // Both chroma planes share one validation: the slice module passes its `u` / `v` planes in one call.
    chroma_pred_into(&ref_plane.data, ref_plane.stride, pad, xi, yi, fx, fy, w, h, out_u);
    chroma_pred_into(&ref_plane.data, ref_plane.stride, pad, xi, yi, fx, fy, w, h, out_v);
    Ok(())
}

/// Merge list from the five spatial neighbours in §8.5.3.2.3 order `neigh = [A1, B1, B0, A0, B2]`
/// (`None` = unavailable). Pruning compares full motion (MV + ref_idx): B1 skips on A1-duplicate, B0 on
/// B1-duplicate, A0 on A1-duplicate, B2 on A1/B1-duplicate or when four candidates already exist; the tail
/// is zero-vector `{(0,0), ref 0}` fill per §8.5.3.2.5 (phase 1 has one reference). `max_cand` saturates
/// into `1..=5` (never an error, never a panic); the result truncates / fills to exactly that length.
pub fn merge_list(neigh: [Option<(Mv, i32)>; 5], max_cand: usize) -> Vec<MergeCand> {
    let max = max_cand.clamp(1, 5);
    let same = |a: (Mv, i32), b: (Mv, i32)| a.0 == b.0 && a.1 == b.1;
    let dup_a1 = |m: (Mv, i32)| match neigh[0] {
        Some(a) => same(a, m),
        None => false,
    };
    let dup_b1 = |m: (Mv, i32)| match neigh[1] {
        Some(b) => same(b, m),
        None => false,
    };
    let mut list: Vec<MergeCand> = Vec::with_capacity(max);
    if let Some((mv, r)) = neigh[0] {
        list.push(MergeCand { mv, ref_idx: r });
    }
    if let Some(m) = neigh[1]
        && !dup_a1(m)
    {
        list.push(MergeCand { mv: m.0, ref_idx: m.1 });
    }
    if let Some(m) = neigh[2]
        && !dup_b1(m)
    {
        list.push(MergeCand { mv: m.0, ref_idx: m.1 });
    }
    if let Some(m) = neigh[3]
        && !dup_a1(m)
    {
        list.push(MergeCand { mv: m.0, ref_idx: m.1 });
    }
    if let Some(m) = neigh[4]
        && !dup_a1(m)
        && !dup_b1(m)
        && list.len() != 4
    {
        list.push(MergeCand { mv: m.0, ref_idx: m.1 });
    }
    list.truncate(max);
    while list.len() < max {
        list.push(MergeCand { mv: Mv { x: 0, y: 0 }, ref_idx: 0 });
    }
    list
}

/// Clamp an MV into the valid ±252 quarter-pel window (saturating: hostile inputs stay usable, never panic).
fn clamp_mv(mv: Mv) -> Mv {
    Mv { x: mv.x.clamp(-252, 252), y: mv.y.clamp(-252, 252) }
}

/// Whether the full 8-tap luma window for integer position `(xi, yi)` fits the padded buffer (pure i64 math).
fn luma_window_fits(stride: usize, rows: usize, pad: usize, xi: i64, yi: i64, w: usize, h: usize) -> bool {
    let (pad_i, stride_i, rows_i) = (pad as i64, stride as i64, rows as i64);
    let (x0, y0) = (xi - 3, yi - 3);
    x0 + pad_i >= 0 && x0 + w as i64 + 7 + pad_i <= stride_i && y0 + pad_i >= 0 && y0 + h as i64 + 7 + pad_i <= rows_i
}

/// Tiny tie-break bias toward the predictor: `(|dx| + |dy|) >> 3` quarter-pel units, so an exact duplicate
/// far from the predictor still wins on distortion while equal-distortion ties prefer the smaller MVD.
fn mv_bias(mv: Mv, pred: Mv) -> u32 {
    let dx = (mv.x as i64 - pred.x as i64).unsigned_abs();
    let dy = (mv.y as i64 - pred.y as i64).unsigned_abs();
    ((dx + dy) >> 3) as u32
}

/// Motion search for the `w x h` luma block co-located at visible `(x, y)`: `orig` is the current block
/// (origin at `orig[0]`, row stride `orig_stride`), `ref_plane` the padded reference. Integer hexagon search
/// (SAD) starts from `predictor` rounded to the integer pel within ±`range` luma pels (clamped to ±63 and to
/// positions whose padding window fits), then half-pel and quarter-pel 8-point diamond refinement use SATD.
/// Every cost adds `mv_bias`; anything invalid (empty/oversize block, short `orig`, bad geometry) falls back
/// to the clamped predictor. Deterministic: no threads inside (the slice module parallelises across CTUs).
pub fn search(ref_plane: &Plane, orig: &[u8], orig_stride: usize, x: usize, y: usize, w: usize, h: usize, predictor: Mv, range: u32) -> Mv {
    let pred_c = clamp_mv(predictor);
    if w == 0 || h == 0 || w > 64 || h > 64 {
        return pred_c;
    }
    if orig_stride < w {
        return pred_c;
    }
    let need_orig = match (h - 1).checked_mul(orig_stride).and_then(|v| v.checked_add(w)) {
        Some(n) => n,
        None => return pred_c,
    };
    if orig.len() < need_orig {
        return pred_c;
    }
    if ref_plane.stride == 0 || ref_plane.w == 0 || ref_plane.h == 0 {
        return pred_c;
    }
    let x_end = match x.checked_add(w) {
        Some(e) => e,
        None => return pred_c,
    };
    let y_end = match y.checked_add(h) {
        Some(e) => e,
        None => return pred_c,
    };
    if x_end > ref_plane.w || y_end > ref_plane.h {
        return pred_c;
    }
    let stride = ref_plane.stride;
    let pad = pad_of(ref_plane);
    let rows = ref_plane.data.len() / stride;
    if rows == 0 {
        return pred_c;
    }
    let range_pel = range.min(63) as i32;
    let bound_q = range_pel * 4;
    // Integer start: predictor rounded to the pel (`+2 & !3` rounds ties up, matching h264enc), kept valid.
    let centre = clamp_mv(Mv { x: (pred_c.x + 2) & !3, y: (pred_c.y + 2) & !3 });
    let fits_int = |mv: Mv| -> Option<usize> {
        if mv.x < -252 || mv.x > 252 || mv.y < -252 || mv.y > 252 {
            return None;
        }
        if (mv.x - centre.x).abs() > bound_q || (mv.y - centre.y).abs() > bound_q {
            return None;
        }
        let xi = x as i64 + (mv.x as i64 >> 2);
        let yi = y as i64 + (mv.y as i64 >> 2);
        let pad_i = pad as i64;
        if xi + pad_i < 0 || xi + w as i64 + pad_i > stride as i64 || yi + pad_i < 0 || yi + h as i64 + pad_i > rows as i64 {
            return None;
        }
        let idx = (yi + pad_i) as usize * stride + (xi + pad_i) as usize;
        ref_plane.data.get(idx..).map(|_| idx)
    };
    let int_cost = |mv: Mv| -> Option<u32> {
        let idx = fits_int(mv)?;
        let rb = ref_plane.data.get(idx..)?;
        Some(sad(orig, orig_stride, rb, stride, w, h) + mv_bias(mv, pred_c))
    };
    // Capped once (`w*h <= 4096`), reused for every sub-pel SATD evaluation.
    let need = w * h;
    let mut pred = vec![0u8; need];
    let sub_cost = |mv: Mv, pred: &mut [u8]| -> Option<u32> {
        if mv.x < -252 || mv.x > 252 || mv.y < -252 || mv.y > 252 {
            return None;
        }
        let fx = (mv.x & 3) as usize;
        let fy = (mv.y & 3) as usize;
        let xi = x as i64 + (mv.x as i64 >> 2);
        let yi = y as i64 + (mv.y as i64 >> 2);
        if !luma_window_fits(stride, rows, pad, xi, yi, w, h) {
            return None;
        }
        luma_pred_into(&ref_plane.data, stride, pad, xi, yi, fx, fy, w, h, pred);
        Some(satd(orig, orig_stride, pred, w, w, h) + mv_bias(mv, pred_c))
    };
    let mut best = centre;
    if fits_int(best).is_none() {
        let zero = Mv { x: 0, y: 0 };
        if fits_int(zero).is_some() {
            best = zero;
        } else {
            return pred_c;
        }
    }
    let mut bcost = match int_cost(best) {
        Some(c) => c,
        None => return pred_c,
    };
    // Large hexagon in full-pel steps (quarter units x4), bounded around the start, SAD costs.
    const HEX: [(i32, i32); 6] = [(-2, 0), (2, 0), (-1, -2), (1, -2), (-1, 2), (1, 2)];
    if range_pel > 0 {
        for _ in 0..range_pel {
            let c = best;
            let mut moved = false;
            for (dx, dy) in HEX {
                let m = Mv { x: c.x + dx * 4, y: c.y + dy * 4 };
                let k = match int_cost(m) {
                    Some(k) => k,
                    None => continue,
                };
                if k < bcost {
                    bcost = k;
                    best = m;
                    moved = true;
                }
            }
            if !moved {
                break;
            }
        }
        // Small square at integer pels to settle between hexagon points (at most two passes).
        const SQ8: [(i32, i32); 8] = [(-1, 0), (1, 0), (0, -1), (0, 1), (-1, -1), (1, -1), (-1, 1), (1, 1)];
        for _ in 0..2 {
            let c = best;
            for (dx, dy) in SQ8 {
                let m = Mv { x: c.x + dx * 4, y: c.y + dy * 4 };
                let k = match int_cost(m) {
                    Some(k) => k,
                    None => continue,
                };
                if k < bcost {
                    bcost = k;
                    best = m;
                }
            }
            if best == c {
                break;
            }
        }
    }
    // Sub-pel diamond refinement on SATD: rebase the cost at the integer best, then half- then quarter-pel.
    let mut bsat = match sub_cost(best, &mut pred) {
        Some(c) => c,
        None => return best,
    };
    const D8: [(i32, i32); 8] = [(-1, 0), (1, 0), (0, -1), (0, 1), (-1, -1), (1, -1), (-1, 1), (1, 1)];
    for step in [2, 1] {
        for _ in 0..2 {
            let c = best;
            for (dx, dy) in D8 {
                let m = Mv { x: c.x + dx * step, y: c.y + dy * step };
                let k = match sub_cost(m, &mut pred) {
                    Some(k) => k,
                    None => continue,
                };
                if k < bsat {
                    bsat = k;
                    best = m;
                }
            }
            if best == c {
                break;
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::picture::Plane;

    fn textured_plane(w: usize, h: usize, pad: usize, seed: u32) -> Plane {
        let mut p = Plane::new(w, h, pad);
        let mut s = seed;
        for yy in 0..h {
            for xx in 0..w {
                s = s.wrapping_mul(1103515245).wrapping_add(12345);
                let o = p.idx(xx, yy);
                p.data[o] = (s >> 16) as u8;
            }
        }
        p.extend_edges();
        p
    }

    fn visible_block(p: &Plane, x: usize, y: usize, w: usize, h: usize) -> Vec<u8> {
        let mut v = vec![0u8; w * h];
        for r in 0..h {
            for c in 0..w {
                v[r * w + c] = p.data[p.idx(x + c, y + r)];
            }
        }
        v
    }

    #[test]
    fn integer_mv_matches_the_shifted_position() {
        let p = textured_plane(24, 24, 8, 7);
        let (mut a, mut b) = (vec![0u8; 64], vec![0u8; 64]);
        mc_luma(&p, 4, 4, Mv { x: 8, y: 4 }, 8, 8, &mut a).unwrap();
        mc_luma(&p, 6, 5, Mv { x: 0, y: 0 }, 8, 8, &mut b).unwrap();
        assert_eq!(a, b);
        assert_eq!(a, visible_block(&p, 6, 5, 8, 8));
    }

    #[test]
    fn half_pel_matches_an_independent_hand_computation() {
        // Row y=8 holds 10..160 in steps of 10; half-pel (frac 2) taps are [-1,4,-11,40,40,-11,4,-1].
        let mut p = Plane::new(16, 16, 8);
        for yy in 0..16 {
            for xx in 0..16 {
                let o = p.idx(xx, yy);
                p.data[o] = 128;
            }
        }
        for xx in 0..16 {
            let o = p.idx(xx, 8);
            p.data[o] = (xx as u8 + 1) * 10;
        }
        p.extend_edges();
        let mut out = vec![0u8; 2];
        mc_luma(&p, 4, 8, Mv { x: 2, y: 0 }, 2, 1, &mut out).unwrap();
        // Window [20,30,40,50,60,70,80,90]: -20+120-440+2000+2400-770+320-90 = 3520 -> (3520+32)>>6 = 55.
        // Window [30,40,50,60,70,80,90,100]: -30+160-550+2400+2800-880+360-100 = 4160 -> (4160+32)>>6 = 65.
        assert_eq!(out, vec![55, 65]);
    }

    #[test]
    fn chroma_mv_halving_selects_the_eighth_pel_filter() {
        let mut p = Plane::new(8, 8, 4);
        for yy in 0..8 {
            for xx in 0..8 {
                let o = p.idx(xx, yy);
                p.data[o] = 128;
            }
        }
        for xx in 0..8 {
            let o = p.idx(xx, 4);
            p.data[o] = (xx as u8 + 1) * 10;
        }
        p.extend_edges();
        // Luma (4,0) quarter-pel is half a luma pel = half a chroma pel: mvC = 4 eighth-pel, taps [-4,36,36,-4].
        let (mut u, mut v) = (vec![0u8; 1], vec![0u8; 1]);
        mc_chroma(&p, 2, 4, Mv { x: 4, y: 0 }, 1, 1, &mut u, &mut v).unwrap();
        // Window [20,30,40,50]: -80+1080+1440-200 = 2240 -> (2240+32)>>6 = 35.
        assert_eq!((u[0], v[0]), (35, 35));
        // Luma (8,0) is two luma pels = one chroma pel: integer copy of the sample at x = 3.
        let (mut u2, mut v2) = (vec![0u8; 1], vec![0u8; 1]);
        mc_chroma(&p, 2, 4, Mv { x: 8, y: 0 }, 1, 1, &mut u2, &mut v2).unwrap();
        assert_eq!((u2[0], v2[0]), (40, 40));
    }

    #[test]
    fn merge_pruning_collapses_duplicates_and_fills_zeros() {
        let a = (Mv { x: 4, y: -8 }, 0);
        // Duplicate A1/B1 collapses to one spatial candidate, then zero fill to 5.
        let list = merge_list([Some(a), Some(a), None, None, None], 5);
        assert_eq!(list.len(), 5);
        assert_eq!((list[0].mv, list[0].ref_idx), (a.0, 0));
        assert_eq!(list[1], MergeCand { mv: Mv { x: 0, y: 0 }, ref_idx: 0 });
        // All unavailable: five zero vectors.
        let zeros = merge_list([None, None, None, None, None], 5);
        assert!(zeros.iter().all(|c| *c == MergeCand { mv: Mv { x: 0, y: 0 }, ref_idx: 0 }));
        // Truncation to max_cand keeps order A1, B1.
        let b = (Mv { x: -4, y: 12 }, 0);
        let two = merge_list([Some(a), Some(b), Some(b), Some(a), Some(b)], 2);
        assert_eq!(two.len(), 2);
        assert_eq!((two[0].mv, two[1].mv), (a.0, b.0));
        // max_cand saturates into 1..=5 instead of panicking.
        assert_eq!(merge_list([Some(a), Some(b), None, None, None], 0).len(), 1);
        assert_eq!(merge_list([Some(a), Some(b), None, None, None], 99).len(), 5);
    }

    #[test]
    fn search_finds_zero_on_identical_content_and_a_small_shift() {
        let p = textured_plane(32, 32, 8, 99);
        let same = visible_block(&p, 8, 8, 8, 8);
        assert_eq!(search(&p, &same, 8, 8, 8, 8, 8, Mv { x: 0, y: 0 }, 8), Mv { x: 0, y: 0 });
        let shifted = visible_block(&p, 10, 9, 8, 8);
        assert_eq!(search(&p, &shifted, 8, 8, 8, 8, 8, Mv { x: 0, y: 0 }, 8), Mv { x: 8, y: 4 });
    }

    #[test]
    fn short_outputs_report_invalid_config() {
        let p = textured_plane(16, 16, 8, 3);
        assert!(mc_luma(&p, 0, 0, Mv { x: 0, y: 0 }, 8, 8, &mut [0u8; 10]).is_err());
        let (mut u, mut v) = (vec![0u8; 64], vec![0u8; 3]);
        assert!(mc_chroma(&p, 0, 0, Mv { x: 0, y: 0 }, 8, 8, &mut u, &mut v).is_err());
    }
}
