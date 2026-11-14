//! Intra prediction for 4x4, 8x8, 16x16, 32x32, and 64x64 blocks (spec 7.11.2).
//!
//! Provides DC, Vertical, Horizontal, Smooth, and Paeth prediction modes with distortion optimization.

pub const DC_PRED: u8 = 0;
pub const V_PRED: u8 = 1;
pub const H_PRED: u8 = 2;
pub const SMOOTH_PRED: u8 = 9;
pub const PAETH_PRED: u8 = 12;

/// Compute intra prediction for a block of size (w, h).
///
/// `above`: pixel row above the block (length >= w).
/// `left`: pixel column left of the block (length >= h).
/// `mode`: DC_PRED, V_PRED, H_PRED, SMOOTH_PRED, or PAETH_PRED.
pub fn predict_intra_block(mode: u8, w: usize, h: usize, above: Option<&[u8]>, left: Option<&[u8]>, dst: &mut [u8], dst_stride: usize) {
    let valid_above = above.and_then(|a| a.get(..w));
    let valid_left = left.and_then(|l| l.get(..h));
    match mode {
        V_PRED => {
            if let Some(ab) = valid_above {
                for y in 0..h {
                    let Some(start) = y.checked_mul(dst_stride) else { continue };
                    let Some(end) = start.checked_add(w) else { continue };
                    if let Some(row) = dst.get_mut(start..end) {
                        row.copy_from_slice(ab);
                    }
                }
            } else {
                fill_dc(128, w, h, dst, dst_stride);
            }
        }
        H_PRED => {
            if let Some(lf) = valid_left {
                for y in 0..h {
                    if let Some(&val) = lf.get(y) {
                        let Some(start) = y.checked_mul(dst_stride) else { continue };
                        let Some(end) = start.checked_add(w) else { continue };
                        if let Some(row) = dst.get_mut(start..end) {
                            row.fill(val);
                        }
                    }
                }
            } else {
                fill_dc(128, w, h, dst, dst_stride);
            }
        }
        SMOOTH_PRED => {
            if let (Some(ab), Some(lf)) = (valid_above, valid_left) {
                predict_smooth(ab, lf, w, h, dst, dst_stride);
            } else {
                let dc_val = get_dc_val(valid_above, valid_left, w, h);
                fill_dc(dc_val, w, h, dst, dst_stride);
            }
        }
        PAETH_PRED => {
            if let (Some(ab), Some(lf)) = (valid_above, valid_left) {
                let top_left = ab.first().copied().unwrap_or(128);
                predict_paeth(ab, lf, top_left, w, h, dst, dst_stride);
            } else {
                let dc_val = get_dc_val(valid_above, valid_left, w, h);
                fill_dc(dc_val, w, h, dst, dst_stride);
            }
        }
        _ => {
            let dc_val = get_dc_val(valid_above, valid_left, w, h);
            fill_dc(dc_val, w, h, dst, dst_stride);
        }
    }
}

pub fn get_dc_val(above: Option<&[u8]>, left: Option<&[u8]>, w: usize, h: usize) -> u8 {
    let valid_above = above.and_then(|a| a.get(..w));
    let valid_left = left.and_then(|l| l.get(..h));
    match (valid_above, valid_left) {
        (Some(ab), Some(lf)) => {
            let sum: u32 = ab.iter().map(|&x| x as u32).sum::<u32>() + lf.iter().map(|&x| x as u32).sum::<u32>();
            let count = (w.saturating_add(h)) as u32;
            (sum + (count >> 1)).checked_div(count).map(|v| v as u8).unwrap_or(128)
        }
        (Some(ab), None) => {
            let sum: u32 = ab.iter().map(|&x| x as u32).sum();
            let count = w as u32;
            (sum + (count >> 1)).checked_div(count).map(|v| v as u8).unwrap_or(128)
        }
        (None, Some(lf)) => {
            let sum: u32 = lf.iter().map(|&x| x as u32).sum();
            let count = h as u32;
            (sum + (count >> 1)).checked_div(count).map(|v| v as u8).unwrap_or(128)
        }
        (None, None) => 128,
    }
}

pub fn sad_dc(src: &[u8], src_stride: usize, w: usize, h: usize, dc: u8) -> u64 {
    let mut sad = 0u64;
    for y in 0..h {
        let Some(row_start) = y.checked_mul(src_stride) else { continue };
        let Some(row_end) = row_start.checked_add(w) else { continue };
        let Some(row) = src.get(row_start..row_end) else {
            continue;
        };
        let (chunks, remainder) = row.as_chunks::<16>();
        for chunk in chunks {
            for &px in chunk {
                sad += px.abs_diff(dc) as u64;
            }
        }
        for &px in remainder {
            sad += px.abs_diff(dc) as u64;
        }
    }
    sad
}

pub fn sad_v(src: &[u8], src_stride: usize, w: usize, h: usize, above: &[u8]) -> u64 {
    let mut sad = 0u64;
    let Some(ab) = above.get(..w) else {
        return u64::MAX;
    };
    let (ab_chunks, ab_remainder) = ab.as_chunks::<16>();
    for y in 0..h {
        let Some(row_start) = y.checked_mul(src_stride) else { continue };
        let Some(row_end) = row_start.checked_add(w) else { continue };
        let Some(row) = src.get(row_start..row_end) else {
            continue;
        };
        let (chunks, remainder) = row.as_chunks::<16>();
        for (chunk, ab_chunk) in chunks.iter().zip(ab_chunks.iter()) {
            for (&px, &ref_px) in chunk.iter().zip(ab_chunk.iter()) {
                sad += px.abs_diff(ref_px) as u64;
            }
        }
        for (&px, &ref_px) in remainder.iter().zip(ab_remainder.iter()) {
            sad += px.abs_diff(ref_px) as u64;
        }
    }
    sad
}

pub fn sad_h(src: &[u8], src_stride: usize, w: usize, h: usize, left: &[u8]) -> u64 {
    let mut sad = 0u64;
    for y in 0..h {
        let Some(&p) = left.get(y) else {
            continue;
        };
        let Some(row_start) = y.checked_mul(src_stride) else { continue };
        let Some(row_end) = row_start.checked_add(w) else { continue };
        let Some(row) = src.get(row_start..row_end) else {
            continue;
        };
        let (chunks, remainder) = row.as_chunks::<16>();
        for chunk in chunks {
            for &px in chunk {
                sad += px.abs_diff(p) as u64;
            }
        }
        for &px in remainder {
            sad += px.abs_diff(p) as u64;
        }
    }
    sad
}

pub fn sad_smooth(src: &[u8], src_stride: usize, w: usize, h: usize, above: &[u8], left: &[u8]) -> u64 {
    let mut sad = 0u64;
    let Some(ab) = above.get(..w) else {
        return u64::MAX;
    };
    let Some(lf) = left.get(..h) else {
        return u64::MAX;
    };
    let bot_left = lf.last().copied().unwrap_or(128) as u32;
    let top_right = ab.last().copied().unwrap_or(128) as u32;

    // Precalculate horizontal wx weights and top-right terms once for up to 64x64 blocks.
    let mut wx_terms = [(0u32, 0u32); 64];
    for (x, slot) in wx_terms.iter_mut().take(w).enumerate() {
        let wx = ((w - 1 - x) as u32 * 256) / (w as u32).max(1);
        *slot = (wx, (256 - wx) * top_right);
    }

    for y in 0..h {
        let wy = ((h - 1 - y) as u32 * 256) / (h as u32).max(1);
        let term_bot_left = (256 - wy) * bot_left;
        let p_left = lf[y] as u32;
        let Some(row_start) = y.checked_mul(src_stride) else { continue };
        let Some(row_end) = row_start.checked_add(w) else { continue };
        let Some(row) = src.get(row_start..row_end) else {
            continue;
        };

        for (x, &px) in row.iter().enumerate() {
            let (wx, term_top_right) = wx_terms[x];
            let p_above = ab[x] as u32;
            let val = ((wy * p_above + term_bot_left + wx * p_left + term_top_right + 256) >> 9) as u8;
            sad += px.abs_diff(val) as u64;
        }
    }
    sad
}

pub fn sad_paeth(src: &[u8], src_stride: usize, w: usize, h: usize, above: &[u8], left: &[u8], top_left: u8) -> u64 {
    let mut sad = 0u64;
    let Some(ab) = above.get(..w) else {
        return u64::MAX;
    };
    let Some(lf) = left.get(..h) else {
        return u64::MAX;
    };
    let tl = top_left as i32;

    for y in 0..h {
        let l = lf[y] as i32;
        let Some(row_start) = y.checked_mul(src_stride) else { continue };
        let Some(row_end) = row_start.checked_add(w) else { continue };
        let Some(row) = src.get(row_start..row_end) else {
            continue;
        };
        for (x, &px) in row.iter().enumerate() {
            let a = ab[x] as i32;
            let base = a + l - tl;
            let pa = (base - a).abs();
            let pl = (base - l).abs();
            let pc = (base - tl).abs();
            let p = if pa <= pl && pa <= pc {
                a
            } else if pl <= pc {
                l
            } else {
                tl
            } as u8;
            sad += px.abs_diff(p) as u64;
        }
    }
    sad
}

/// Compute spatial variance of a block for Adaptive Quantization (AQ).
pub fn block_variance(src: &[u8], stride: usize, w: usize, h: usize) -> u64 {
    if w == 0 || h == 0 {
        return 0;
    }
    let count = (w * h) as u64;
    let mut sum = 0u64;
    let mut sum_sq = 0u64;
    for y in 0..h {
        let Some(row_start) = y.checked_mul(stride) else { continue };
        let Some(row_end) = row_start.checked_add(w) else { continue };
        let Some(row) = src.get(row_start..row_end) else {
            continue;
        };
        let (chunks, remainder) = row.as_chunks::<16>();
        for chunk in chunks {
            for &px in chunk {
                let v = px as u64;
                sum += v;
                sum_sq += v * v;
            }
        }
        for &px in remainder {
            let v = px as u64;
            sum += v;
            sum_sq += v * v;
        }
    }
    let mean = sum / count;
    (sum_sq / count).saturating_sub(mean * mean)
}

/// Pick the intra prediction mode (DC, Vertical, Horizontal, Smooth, Paeth) minimizing SAD.
///
/// Features early mode termination: if DC prediction error is already negligible (e.g. flat block),
/// skips evaluation of directional and smooth modes to drastically reduce encode latency.
pub fn choose_best_intra_mode(src: &[u8], src_stride: usize, w: usize, h: usize, above: Option<&[u8]>, left: Option<&[u8]>) -> u8 {
    let dc = get_dc_val(above, left, w, h);
    let mut best_sad = sad_dc(src, src_stride, w, h, dc);
    let mut best_mode = DC_PRED;

    // Early Mode Termination: if block is nearly flat (e.g. average SAD per pixel < 1.0 or <= 128 total),
    // skip evaluating the other 4 directional and complex modes.
    let early_threshold = ((w * h) as u64).max(128);
    if best_sad <= early_threshold {
        return best_mode;
    }

    if let Some(ab) = above.filter(|a| a.len() >= w) {
        let sad = sad_v(src, src_stride, w, h, ab);
        if sad.saturating_add(sad / 16) < best_sad {
            best_sad = sad;
            best_mode = V_PRED;
        }
    }

    if let Some(lf) = left.filter(|l| l.len() >= h) {
        let sad = sad_h(src, src_stride, w, h, lf);
        if sad.saturating_add(sad / 16) < best_sad {
            best_sad = sad;
            best_mode = H_PRED;
        }
    }

    if let (Some(ab), Some(lf)) = (above.filter(|a| a.len() >= w), left.filter(|l| l.len() >= h)) {
        let s_sad = sad_smooth(src, src_stride, w, h, ab, lf);
        if s_sad.saturating_add(s_sad / 16) < best_sad {
            best_sad = s_sad;
            best_mode = SMOOTH_PRED;
        }

        let top_left = ab.first().copied().unwrap_or(128);
        let p_sad = sad_paeth(src, src_stride, w, h, ab, lf, top_left);
        if p_sad.saturating_add(p_sad / 16) < best_sad {
            best_mode = PAETH_PRED;
        }
    }

    best_mode
}

fn predict_smooth(above: &[u8], left: &[u8], w: usize, h: usize, dst: &mut [u8], dst_stride: usize) {
    let Some(ab) = above.get(..w) else {
        return;
    };
    let Some(lf) = left.get(..h) else {
        return;
    };
    let bot_left = lf.last().copied().unwrap_or(128) as u32;
    let top_right = ab.last().copied().unwrap_or(128) as u32;

    let mut wx_terms = [(0u32, 0u32); 64];
    for (x, slot) in wx_terms.iter_mut().take(w).enumerate() {
        let wx = ((w - 1 - x) as u32 * 256) / (w as u32).max(1);
        *slot = (wx, (256 - wx) * top_right);
    }

    for y in 0..h {
        let wy = ((h - 1 - y) as u32 * 256) / (h as u32).max(1);
        let term_bot_left = (256 - wy) * bot_left;
        let p_left = lf[y] as u32;
        let Some(row_start) = y.checked_mul(dst_stride) else { continue };
        let Some(row_end) = row_start.checked_add(w) else { continue };
        let Some(row) = dst.get_mut(row_start..row_end) else {
            continue;
        };
        for (x, slot) in row.iter_mut().enumerate() {
            let (wx, term_top_right) = wx_terms[x];
            let p_above = ab[x] as u32;
            *slot = ((wy * p_above + term_bot_left + wx * p_left + term_top_right + 256) >> 9) as u8;
        }
    }
}

fn predict_paeth(above: &[u8], left: &[u8], top_left: u8, w: usize, h: usize, dst: &mut [u8], dst_stride: usize) {
    let Some(ab) = above.get(..w) else {
        return;
    };
    let Some(lf) = left.get(..h) else {
        return;
    };
    let tl = top_left as i32;

    for y in 0..h {
        let l = lf[y] as i32;
        let Some(row_start) = y.checked_mul(dst_stride) else { continue };
        let Some(row_end) = row_start.checked_add(w) else { continue };
        let Some(row) = dst.get_mut(row_start..row_end) else {
            continue;
        };
        for (x, slot) in row.iter_mut().enumerate() {
            let a = ab[x] as i32;
            let base = a + l - tl;
            let pa = (base - a).abs();
            let pl = (base - l).abs();
            let pc = (base - tl).abs();
            *slot = if pa <= pl && pa <= pc {
                a
            } else if pl <= pc {
                l
            } else {
                tl
            } as u8;
        }
    }
}

fn fill_dc(val: u8, w: usize, h: usize, dst: &mut [u8], dst_stride: usize) {
    for y in 0..h {
        let Some(row_start) = y.checked_mul(dst_stride) else { continue };
        let Some(row_end) = row_start.checked_add(w) else { continue };
        if let Some(row) = dst.get_mut(row_start..row_end) {
            row.fill(val);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_predict_intra_block_hostile_inputs_do_not_panic() {
        let mut dst = [0u8; 64];

        // Empty above slice with V_PRED
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            predict_intra_block(V_PRED, 4, 4, Some(&[]), None, &mut dst, 4);
        }));
        assert!(res.is_ok());

        // Empty left slice with H_PRED
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            predict_intra_block(H_PRED, 4, 4, None, Some(&[]), &mut dst, 4);
        }));
        assert!(res.is_ok());

        // Undersized slices for Smooth and Paeth
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            predict_intra_block(SMOOTH_PRED, 8, 8, Some(&[1, 2]), Some(&[3]), &mut dst, 8);
            predict_intra_block(PAETH_PRED, 8, 8, Some(&[1]), Some(&[2, 3]), &mut dst, 8);
        }));
        assert!(res.is_ok());

        // Hostile stride usize::MAX
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            predict_intra_block(DC_PRED, 4, 4, Some(&[1, 2, 3, 4]), Some(&[1, 2, 3, 4]), &mut dst, usize::MAX);
            predict_intra_block(V_PRED, 4, 4, Some(&[1, 2, 3, 4]), Some(&[1, 2, 3, 4]), &mut dst, usize::MAX);
        }));
        assert!(res.is_ok());
    }

    #[test]
    fn test_intra_sad_and_dc_hostile_inputs_do_not_panic() {
        let src = [42u8; 64];
        let above = [10u8; 8];
        let left = [20u8; 8];

        assert_eq!(get_dc_val(Some(&[]), None, 4, 4), 128);
        assert_eq!(get_dc_val(None, Some(&[]), 4, 4), 128);
        assert_eq!(get_dc_val(Some(&[]), Some(&[]), 0, 0), 128);

        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = sad_dc(&src, usize::MAX, 4, 4, 128);
            let _ = sad_v(&src, usize::MAX, 4, 4, &above);
            let _ = sad_h(&src, usize::MAX, 4, 4, &left);
            let _ = sad_smooth(&src, usize::MAX, 4, 4, &above, &left);
            let _ = sad_paeth(&src, usize::MAX, 4, 4, &above, &left, 128);
            let _ = block_variance(&src, usize::MAX, 4, 4);
        }));
        assert!(res.is_ok());
    }
}
