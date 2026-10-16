//! Intra sample prediction (H.265 §8.4.4.2, Tables 8-5/8-6): reference substitution, the conditional [1 2 1] tap and the
//! 32x32 strong-smoothing gate, planar / DC / angular prediction and the §8.4.2 MPM list. Formulae mirror the decoder in
//! `filmcraft-hevc` so encoder prediction matches reconstruction sample-exactly. 8-bit only: luma TBs 4/8/16/32, 4:2:0 chroma TBs 2/4/8/16.

use crate::Error;
use crate::tables::{INTRA_PRED_ANGLE, INV_ANGLE};

/// Intra reference samples of one square TB, laid out like the decoder's `Refs`: `left[0] == top[0]` is `p[-1][-1]`,
/// `left[1 + y]` is `p[-1][y]` and `top[1 + x]` is `p[x][-1]`; only `0..=2*size` is used, the rest stays zero.
#[derive(Clone, Copy, Debug)]
pub struct PreparedRefs {
    pub size: usize,
    pub left: [u8; 65],
    pub top: [u8; 65],
}

/// Build substituted (and, for luma, filtered) refs from reconstructed neighbours (§8.4.4.2.2/2.3). `top`/`left` hold
/// the `2*size` samples above/left of the TB with per-sample availability; `corner`/`avail_corner` is `p[-1][-1]`.
/// `mode` gates the filter, `strong_smoothing` is the SPS flag, `is_luma` selects the luma-only filtering path.
pub fn prepare_refs(
    size: usize,
    top: &[u8],
    left: &[u8],
    corner: u8,
    avail_top: &[bool],
    avail_left: &[bool],
    avail_corner: bool,
    mode: u8,
    strong_smoothing: bool,
    is_luma: bool,
) -> Result<PreparedRefs, Error> {
    if !matches!(size, 2 | 4 | 8 | 16 | 32) {
        return Err(Error::InvalidConfig(format!("intra TB size {size} not in {{2, 4, 8, 16, 32}}")));
    }
    if mode > 34 {
        return Err(Error::InvalidConfig(format!("intra mode {mode} out of range 0..=34")));
    }
    let n2 = 2 * size;
    if top.len() < n2 || left.len() < n2 || avail_top.len() < n2 || avail_left.len() < n2 {
        return Err(Error::InvalidConfig("intra neighbour buffers shorter than 2*size".to_string()));
    }
    let mut l = [0u8; 65];
    let mut t = [0u8; 65];
    let mut al = [false; 65];
    let mut at = [false; 65];
    l[0] = corner;
    al[0] = avail_corner;
    l[1..1 + n2].copy_from_slice(&left[..n2]);
    t[1..1 + n2].copy_from_slice(&top[..n2]);
    al[1..1 + n2].copy_from_slice(&avail_left[..n2]);
    at[1..1 + n2].copy_from_slice(&avail_top[..n2]);
    if !al[..=n2].iter().any(|&a| a) && !at[1..=n2].iter().any(|&a| a) {
        l[..=n2].fill(128); // 1 << (8 - 1): mid-grey when nothing is available (§8.4.4.2.2).
        t[..=n2].fill(128);
    } else {
        if !al[n2] {
            // Bottom of the column takes the nearest available sample below it, else the first along the row (step 1).
            let mut found = None;
            for y in (0..n2).rev() {
                if al[y] {
                    found = Some(l[y]);
                    break;
                }
            }
            if found.is_none() {
                for x in 1..=n2 {
                    if at[x] {
                        found = Some(t[x]);
                        break;
                    }
                }
            }
            if let Some(v) = found {
                l[n2] = v;
                al[n2] = true;
            }
        }
        for y in (0..n2).rev() {
            // Each unavailable column sample pads from the sample below it (step 2).
            if !al[y] {
                l[y] = l[y + 1];
                al[y] = true;
            }
        }
        t[0] = l[0]; // Corner resolves through the column, then the row pads leftwards (step 3).
        at[0] = true;
        for x in 1..=n2 {
            if !at[x] {
                t[x] = t[x - 1];
                at[x] = true;
            }
        }
    }
    if is_luma {
        // 4:2:0 chroma skips §8.4.4.2.3 entirely (filtering needs cIdx 0 or 4:4:4), matching the decoder call sites.
        filter_refs(&mut l, &mut t, mode, size, strong_smoothing);
    }
    Ok(PreparedRefs { size, left: l, top: t })
}

/// Luma entry point: `size` 4/8/16/32; `mode` selects the §8.4.4.2.3 gate and `strong_smoothing` is our SPS flag (1).
pub fn prepare_luma_refs(
    size: usize,
    top: &[u8],
    left: &[u8],
    corner: u8,
    avail_top: &[bool],
    avail_left: &[bool],
    avail_corner: bool,
    mode: u8,
    strong_smoothing: bool,
) -> Result<PreparedRefs, Error> {
    if !matches!(size, 4 | 8 | 16 | 32) {
        return Err(Error::InvalidConfig(format!("luma intra TB size {size} not in {{4, 8, 16, 32}}")));
    }
    prepare_refs(size, top, left, corner, avail_top, avail_left, avail_corner, mode, strong_smoothing, true)
}

/// Chroma entry point (4:2:0, `size` 2/4/8/16): substitution only, never filtered.
pub fn prepare_chroma_refs(
    size: usize,
    top: &[u8],
    left: &[u8],
    corner: u8,
    avail_top: &[bool],
    avail_left: &[bool],
    avail_corner: bool,
) -> Result<PreparedRefs, Error> {
    if !matches!(size, 2 | 4 | 8 | 16) {
        return Err(Error::InvalidConfig(format!("chroma intra TB size {size} not in {{2, 4, 8, 16}}")));
    }
    prepare_refs(size, top, left, corner, avail_top, avail_left, avail_corner, 1, false, false)
}

/// Conditional neighbour filtering (§8.4.4.2.3): DC and 4x4 never filter, near-horizontal/vertical modes skip per Table
/// 8-4, and flat 32x32 luma takes the bilinear strong-smoothing path when the SPS flag is set. All indices are provably
/// in range (`n` in {4, 8, 16, 32}, taps reach at most `2*n <= 64`), so no bounds checks are needed.
fn filter_refs(l: &mut [u8; 65], t: &mut [u8; 65], mode: u8, n: usize, strong: bool) {
    if mode == 1 || n == 4 {
        return;
    }
    let dist = (i16::from(mode) - 26).abs().min((i16::from(mode) - 10).abs());
    let thres: i16 = match n {
        8 => 7,
        16 => 1,
        _ => 0,
    };
    if dist <= thres {
        return;
    }
    let n2 = 2 * n;
    let c = i32::from(l[0]);
    if strong && n == 32 {
        // Bilinear gate: corner-to-mid-to-end must be nearly flat on both sides (8-bit threshold 1 << (8-5) = 8).
        let (bl, tr) = (i32::from(l[n2]), i32::from(t[n2]));
        if (c + tr - 2 * i32::from(t[n])).abs() < 8 && (c + bl - 2 * i32::from(l[n])).abs() < 8 {
            for i in 0..63 {
                l[1 + i] = (((63 - i as i32) * c + (i as i32 + 1) * bl + 32) >> 6) as u8;
                t[1 + i] = (((63 - i as i32) * c + (i as i32 + 1) * tr + 32) >> 6) as u8;
            }
            return;
        }
    }
    let (lc, tc) = (*l, *t); // Separable [1 2 1] tap reads the unfiltered copies; corner and far ends keep (8-41/43/45).
    let corner = ((u32::from(lc[1]) + 2 * u32::from(lc[0]) + u32::from(tc[1]) + 2) >> 2) as u8;
    for i in 1..n2 {
        l[i] = ((u32::from(lc[i - 1]) + 2 * u32::from(lc[i]) + u32::from(lc[i + 1]) + 2) >> 2) as u8;
        t[i] = ((u32::from(tc[i - 1]) + 2 * u32::from(tc[i]) + u32::from(tc[i + 1]) + 2) >> 2) as u8;
    }
    l[0] = corner;
    t[0] = corner;
}

/// Validate a prediction call without touching samples: mode range, TB-size set, matching refs and output coverage.
/// Checked math keeps a hostile stride from overflowing; every later index then sits below the validated length.
fn check_predict_args(mode: u8, size: usize, refs: &PreparedRefs, out_len: usize, stride: usize, luma: bool) -> Result<(), Error> {
    if mode > 34 {
        return Err(Error::InvalidConfig(format!("intra mode {mode} out of range 0..=34")));
    }
    let ok = if luma { matches!(size, 4 | 8 | 16 | 32) } else { matches!(size, 2 | 4 | 8 | 16) };
    if !ok {
        return Err(Error::InvalidConfig(format!("intra TB size {size} not supported for {}", if luma { "luma" } else { "chroma" })));
    }
    if refs.size != size {
        return Err(Error::InvalidConfig(format!("refs prepared for {} but predicting {}", refs.size, size)));
    }
    if stride < size {
        return Err(Error::InvalidConfig(format!("stride {stride} smaller than TB size {size}")));
    }
    let need = (size - 1).checked_mul(stride).and_then(|v| v.checked_add(size)).ok_or_else(|| Error::InvalidConfig("stride overflows output".to_string()))?;
    if out_len < need {
        return Err(Error::InvalidConfig("prediction output too small".to_string()));
    }
    Ok(())
}

/// Luma prediction of one TB: planar (0), DC (1) with boundary smoothing, angular (2..=34) via Tables 8-5/8-6. `refs`
/// must come from [`prepare_luma_refs`] for the same `size`; `out` needs `(size - 1) * stride + size` samples.
pub fn predict_luma(mode: u8, size: usize, refs: &PreparedRefs, out: &mut [u8], stride: usize) -> Result<(), Error> {
    check_predict_args(mode, size, refs, out.len(), stride, true)?;
    match mode {
        0 => predict_planar(size, refs, out, stride),
        1 => predict_dc(size, refs, out, stride, true),
        _ => predict_angular(mode, size, refs, out, stride, true)?,
    }
    Ok(())
}

/// Map `intra_chroma_pred_mode` 0..=4 plus the luma mode to the effective `IntraPredModeC` (Table 8-2): modes 0..3 name
/// planar / vertical (26) / horizontal (10) / DC unless the luma mode already is that direction (then 34 avoids the
/// duplicate); mode 4 is derived (DM) and follows the luma mode.
pub fn resolve_chroma_mode(chroma_mode: u8, luma_mode: u8) -> Result<u8, Error> {
    if chroma_mode > 4 {
        return Err(Error::InvalidConfig(format!("intra_chroma_pred_mode {chroma_mode} out of range 0..=4")));
    }
    if luma_mode > 34 {
        return Err(Error::InvalidConfig(format!("luma intra mode {luma_mode} out of range 0..=34")));
    }
    Ok(match chroma_mode {
        0 => {
            if luma_mode == 0 {
                34
            } else {
                0
            }
        }
        1 => {
            if luma_mode == 26 {
                34
            } else {
                26
            }
        }
        2 => {
            if luma_mode == 10 {
                34
            } else {
                10
            }
        }
        3 => {
            if luma_mode == 1 {
                34
            } else {
                1
            }
        }
        _ => luma_mode,
    })
}

/// Chroma prediction of a 4:2:0 TB (`size` 2/4/8/16): `chroma_mode` 0..=4 per [`resolve_chroma_mode`], with the caller
/// passing the PB's luma mode for the DM case. Never filtered or boundary-smoothed (cIdx > 0 skips both in §8.4.4.2).
pub fn predict_chroma(chroma_mode: u8, luma_mode: u8, size: usize, refs: &PreparedRefs, out: &mut [u8], stride: usize) -> Result<(), Error> {
    let mode = resolve_chroma_mode(chroma_mode, luma_mode)?;
    check_predict_args(mode, size, refs, out.len(), stride, false)?;
    match mode {
        0 => predict_planar(size, refs, out, stride),
        1 => predict_dc(size, refs, out, stride, false),
        _ => predict_angular(mode, size, refs, out, stride, false)?,
    }
    Ok(())
}

/// The §8.4.2 candidate list: missing (or out-of-range, treated as missing) neighbours contribute DC; equal candidates
/// extend to planar/DC/vertical or the ±1 angular neighbours (8-24..26); unequal ones add planar, then DC, then 26.
pub fn mpm_candidates(left_mode: Option<u8>, above_mode: Option<u8>) -> [u8; 3] {
    let a = match left_mode {
        Some(m) if m <= 34 => m,
        _ => 1,
    };
    let b = match above_mode {
        Some(m) if m <= 34 => m,
        _ => 1,
    };
    if a == b {
        if a < 2 {
            [0, 1, 26]
        } else {
            [a, 2 + (a + 29) % 32, 2 + (a - 1) % 32] // u8-safe: a <= 34 keeps a + 29 <= 63, a >= 2 keeps a - 1 >= 1.
        }
    } else if a != 0 && b != 0 {
        [a, b, 0]
    } else if a != 1 && b != 1 {
        [a, b, 1]
    } else {
        [a, b, 26]
    }
}

/// Planar (8-46): mean of a horizontal and a vertical ramp; takes no boundary pass. Weights sum to `2*n`, so the
/// result cannot exceed 255 and needs no clipping.
fn predict_planar(n: usize, refs: &PreparedRefs, out: &mut [u8], stride: usize) {
    let shift = n.trailing_zeros() + 1;
    let tr = i32::from(refs.top[1 + n]);
    let bl = i32::from(refs.left[1 + n]);
    for y in 0..n {
        let l = i32::from(refs.left[1 + y]);
        for x in 0..n {
            let t = i32::from(refs.top[1 + x]);
            out[y * stride + x] =
                (((n as i32 - 1 - x as i32) * l + (x as i32 + 1) * tr + (n as i32 - 1 - y as i32) * t + (y as i32 + 1) * bl + n as i32) >> shift) as u8;
        }
    }
}

/// DC (8-47..8-52): mean of the first `n` top/left samples; luma TBs below 32x32 then smooth the top row and left column
/// from the true neighbours (our headers leave the boundary filter enabled, so the decoder does the same).
fn predict_dc(n: usize, refs: &PreparedRefs, out: &mut [u8], stride: usize, edge: bool) {
    let shift = n.trailing_zeros() + 1;
    let sum: i32 = (1..=n).map(|i| i32::from(refs.top[i]) + i32::from(refs.left[i])).sum();
    let dc = (sum + n as i32) >> shift;
    for y in 0..n {
        for x in 0..n {
            out[y * stride + x] = dc as u8;
        }
    }
    if edge && n < 32 {
        out[0] = ((i32::from(refs.left[1]) + 2 * dc + i32::from(refs.top[1]) + 2) >> 2) as u8;
        for x in 1..n {
            out[x] = ((i32::from(refs.top[1 + x]) + 3 * dc + 2) >> 2) as u8;
        }
        for y in 1..n {
            out[y * stride] = ((i32::from(refs.left[1 + y]) + 3 * dc + 2) >> 2) as u8;
        }
    }
}

/// Angular modes (8-53..8-68): project each row/column onto the main reference with 1/32-sample interpolation; modes
/// 10/26 additionally smooth the first row/column for luma below 32x32. The 193-entry table with base 64 keeps every
/// hot-loop `k` in 33..=129 (|angle| <= 32, i/j < 32), so only the cold inverse-angle extension needs checked accesses.
fn predict_angular(mode: u8, n: usize, refs: &PreparedRefs, out: &mut [u8], stride: usize, edge: bool) -> Result<(), Error> {
    let angle = i32::from(INTRA_PRED_ANGLE[mode as usize]);
    let (main, side): (&[u8; 65], &[u8; 65]) = if mode >= 18 { (&refs.top, &refs.left) } else { (&refs.left, &refs.top) };
    let mut rf = [0i32; 193];
    let base = 64i32;
    for x in 0..=n {
        rf[(base + x as i32) as usize] = i32::from(main[x]);
    }
    if angle < 0 {
        let lim = (n as i32 * angle) >> 5;
        if lim < -1 {
            // invAngle covers modes 11..=25; the checked index also rejects any table/mode mismatch as an error.
            let inv = i32::from(
                (mode as usize)
                    .checked_sub(11)
                    .and_then(|i| INV_ANGLE.get(i))
                    .copied()
                    .ok_or_else(|| Error::InvalidConfig(format!("no invAngle for mode {mode}")))?,
            );
            for x in lim..=-1 {
                // Side position (8-54/8-62): -1 + ((x * invAngle + 128) >> 8) + 1; checked since its range proof is subtle.
                let s = -1 + ((x * inv + 128) >> 8) + 1;
                let v = i32::from(*side.get(s as usize).ok_or_else(|| Error::InvalidConfig(format!("extended ref {x} out of range")))?);
                *rf.get_mut((base + x) as usize).ok_or_else(|| Error::InvalidConfig(format!("extended ref {x} out of range")))? = v;
            }
        }
    } else {
        for x in n + 1..=2 * n {
            rf[(base + x as i32) as usize] = i32::from(main[x]);
        }
    }
    let horizontal = mode < 18;
    for j in 0..n {
        let pos = (j as i32 + 1) * angle;
        let idx = pos >> 5;
        let fact = pos & 31;
        for i in 0..n {
            let k = (base + i as i32 + idx + 1) as usize;
            let v = if fact != 0 { ((32 - fact) * rf[k] + fact * rf[k + 1] + 16) >> 5 } else { rf[k] };
            if horizontal {
                out[i * stride + j] = v as u8;
            } else {
                out[j * stride + i] = v as u8;
            }
        }
    }
    if edge && n < 32 {
        if mode == 26 {
            for y in 0..n {
                out[y * stride] = (i32::from(refs.top[1]) + ((i32::from(refs.left[1 + y]) - i32::from(refs.left[0])) >> 1)).clamp(0, 255) as u8;
            }
        } else if mode == 10 {
            for x in 0..n {
                out[x] = (i32::from(refs.left[1]) + ((i32::from(refs.top[1 + x]) - i32::from(refs.top[0])) >> 1)).clamp(0, 255) as u8;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dc_of_constant_refs_is_constant() {
        // dcVal = (4*200 + 4*200 + 4) >> 3 = 1604 >> 3 = 200 (8-47); the boundary pass keeps 200 since
        // (200 + 3*200 + 2) >> 2 = 802 >> 2 = 200.
        let refs = prepare_luma_refs(4, &[200; 8], &[200; 8], 200, &[true; 8], &[true; 8], true, 1, true).unwrap();
        let mut out = [0u8; 16];
        predict_luma(1, 4, &refs, &mut out, 4).unwrap();
        assert!(out.iter().all(|&v| v == 200));
    }

    #[test]
    fn planar_corners_match_hand_derivation() {
        // n = 4 skips neighbour filtering and planar takes no boundary pass, so raw refs apply to (8-46):
        // pred[x][y] = ((3-x)*L[y] + (x+1)*TR + (3-y)*T[x] + (y+1)*BL + 4) >> 3, TR = top[4] = 50, BL = left[4] = 140.
        let refs = prepare_luma_refs(4, &[10, 20, 30, 40, 50, 60, 70, 80], &[100, 110, 120, 130, 140, 150, 160, 170], 5, &[true; 8], &[true; 8], true, 0, true)
            .unwrap();
        let mut out = [0u8; 16];
        predict_luma(0, 4, &refs, &mut out, 4).unwrap();
        assert_eq!(out[0], 65); // (3*100 + 50 + 3*10 + 140 + 4) >> 3 = 524 >> 3 = 65.
        assert_eq!(out[3], 58); // (4*50 + 3*40 + 140 + 4) >> 3 = 464 >> 3 = 58.
        assert_eq!(out[12], 125); // (3*130 + 50 + 4*140 + 4) >> 3 = 1004 >> 3 = 125.
        assert_eq!(out[15], 95); // (4*50 + 4*140 + 4) >> 3 = 764 >> 3 = 95.
    }

    #[test]
    fn vertical_copies_top_row_and_horizontal_copies_left_column() {
        // Left samples equal the corner so the mode-26 boundary term ((L - corner) >> 1) vanishes; n = 4 skips filtering.
        let refs = prepare_luma_refs(4, &[11, 22, 33, 44, 55, 66, 77, 88], &[7; 8], 7, &[true; 8], &[true; 8], true, 26, true).unwrap();
        let mut out = [0u8; 16];
        predict_luma(26, 4, &refs, &mut out, 4).unwrap();
        for y in 0..4 {
            assert_eq!(&out[y * 4..y * 4 + 4], &[11, 22, 33, 44]);
        }
        // Dually the mode-10 row term vanishes when the top row equals the corner.
        let refs = prepare_luma_refs(4, &[9; 8], &[5, 15, 25, 35, 45, 55, 65, 75], 9, &[true; 8], &[true; 8], true, 10, true).unwrap();
        predict_luma(10, 4, &refs, &mut out, 4).unwrap();
        for y in 0..4 {
            assert!(out[y * 4..y * 4 + 4].iter().all(|&v| v == [5, 15, 25, 35][y]));
        }
    }

    #[test]
    fn diagonal_18_matches_decoder_projection() {
        // Mode 18 has angle -32, so each row shifts one sample further left (8-56: iIdx = -(y+1), iFact = 0) and negative
        // indices extend into the column via invAngle -256 (8-54): ref[-1..-4] = left[1..4]. Corner 100, top +10 right,
        // left -10 down; n = 4 skips filtering and mode 18 takes no boundary pass, so pred[x][y] = ref[x-y].
        let refs =
            prepare_luma_refs(4, &[110, 120, 130, 140, 150, 160, 170, 180], &[90, 80, 70, 60, 50, 40, 30, 20], 100, &[true; 8], &[true; 8], true, 18, true)
                .unwrap();
        let mut out = [0u8; 16];
        predict_luma(18, 4, &refs, &mut out, 4).unwrap();
        assert_eq!(out[0], 100); // ref[0]: the corner.
        assert_eq!(out[1], 110); // ref[1] = p[0][-1].
        assert_eq!(out[4], 90); // ref[-1] = p[-1][0]: ((-1 * -256 + 128) >> 8) = 1.
        assert_eq!(out[12], 70); // ref[-3] = p[-1][2]: ((-3 * -256 + 128) >> 8) = 3.
        assert_eq!(out[3], 130); // ref[3] = p[2][-1].
        assert_eq!(out[15], 100); // ref[0] again on the diagonal.
    }

    #[test]
    fn substitution_fills_unavailable_neighbours() {
        // Nothing available: every sample becomes 1 << (8-1) = 128 (§8.4.4.2.2), so DC predicts flat 128.
        let refs = prepare_luma_refs(4, &[0; 8], &[0; 8], 0, &[false; 8], &[false; 8], false, 1, true).unwrap();
        assert!(refs.left[..=8].iter().all(|&v| v == 128) && refs.top[..=8].iter().all(|&v| v == 128));
        let mut out = [0u8; 16];
        predict_luma(1, 4, &refs, &mut out, 4).unwrap();
        assert!(out.iter().all(|&v| v == 128));
        // A lone p[-1][0] = 77 propagates: step 1 finds it scanning y = 7..0 for p[-1][7], step 2 pads up the column
        // through the corner, step 3 pads right along the row, so the whole edge reads 77.
        let mut avail_left = [false; 8];
        avail_left[0] = true;
        let refs = prepare_luma_refs(4, &[0; 8], &[77, 0, 0, 0, 0, 0, 0, 0], 0, &[false; 8], &avail_left, false, 1, true).unwrap();
        assert!(refs.left[..=8].iter().all(|&v| v == 77) && refs.top[..=8].iter().all(|&v| v == 77));
    }

    #[test]
    fn strong_smoothing_gate_erases_interior_spikes() {
        // Mode 2 sits 8 from the nearest axis, so 32x32 luma filters; with flat ends (|100 + 100 - 2*100| = 0 < 8) the
        // bilinear path applies and the spike at top[10] disappears from the prepared refs.
        let mut top = [100u8; 64];
        top[10] = 200;
        let refs = prepare_luma_refs(32, &top, &[100; 64], 100, &[true; 64], &[true; 64], true, 2, true).unwrap();
        assert_eq!(refs.top[11], 100);
        // Without the SPS flag the separable [1 2 1] tap applies instead at t[11] (= top[10]):
        // (100 + 2*200 + 100 + 2) >> 2 = 150.
        let refs = prepare_luma_refs(32, &top, &[100; 64], 100, &[true; 64], &[true; 64], true, 2, false).unwrap();
        assert_eq!(refs.top[11], 150);
    }

    #[test]
    fn mpm_lists_follow_8_4_2() {
        assert_eq!(mpm_candidates(None, None), [0, 1, 26]); // Both missing -> DC, DC -> planar/DC/vertical.
        assert_eq!(mpm_candidates(Some(0), Some(1)), [0, 1, 26]); // Planar + DC present -> vertical completes the list.
        assert_eq!(mpm_candidates(Some(26), Some(10)), [26, 10, 0]); // No planar -> planar completes the list.
        assert_eq!(mpm_candidates(Some(5), Some(5)), [5, 4, 6]); // Equal angular: (5+29)%32 = 2 -> 4, (5-1)%32 = 4 -> 6.
        assert_eq!(mpm_candidates(Some(2), Some(2)), [2, 33, 3]); // Wrap-around: (2+29)%32 = 31 -> 33, 1%32 = 1 -> 3.
        assert_eq!(mpm_candidates(Some(99), Some(26)), [1, 26, 0]); // Out-of-range counts as unavailable, i.e. DC.
    }

    #[test]
    fn chroma_dm_follows_luma_without_boundary_smoothing() {
        // DM (4) with vertical luma copies the top row, and the mode-26 first-column smooth does NOT apply to chroma.
        let refs = prepare_chroma_refs(4, &[11, 22, 33, 44, 55, 66, 77, 88], &[7; 8], 7, &[true; 8], &[true; 8], true).unwrap();
        let mut out = [0u8; 16];
        predict_chroma(4, 26, 4, &refs, &mut out, 4).unwrap();
        for y in 0..4 {
            assert_eq!(&out[y * 4..y * 4 + 4], &[11, 22, 33, 44]);
        }
        // A chroma planar signal colliding with planar luma really predicts angular 34 (Table 8-2, first row).
        assert_eq!(resolve_chroma_mode(0, 0).unwrap(), 34);
        assert_eq!(resolve_chroma_mode(0, 5).unwrap(), 0);
        assert_eq!(resolve_chroma_mode(1, 26).unwrap(), 34);
        assert_eq!(resolve_chroma_mode(3, 1).unwrap(), 34);
    }

    #[test]
    fn bad_arguments_return_errors() {
        let refs = prepare_luma_refs(4, &[0; 8], &[0; 8], 0, &[true; 8], &[true; 8], true, 0, true).unwrap();
        assert!(predict_luma(35, 4, &refs, &mut [0u8; 16], 4).is_err());
        assert!(predict_luma(0, 8, &refs, &mut [0u8; 64], 8).is_err()); // Refs/size mismatch.
        assert!(predict_luma(0, 4, &refs, &mut [0u8; 15], 4).is_err()); // Short output.
        assert!(prepare_luma_refs(2, &[0; 8], &[0; 8], 0, &[true; 8], &[true; 8], true, 0, true).is_err());
        assert!(resolve_chroma_mode(5, 0).is_err());
    }
}
