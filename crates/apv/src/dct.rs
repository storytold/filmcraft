//! RFC 9924 §6.3 scaling and 8x8 integer inverse transform, plus the matching forward transform.

use crate::tables::LEVEL_SCALE;

/// Gathers column `x` of an 8x8 block stored row-major.
#[inline(always)]
fn column(block: &[i32; 64], x: usize) -> [i32; 8] {
    std::array::from_fn(|i| block[i * 8 + x])
}

/// 1D 8-point inverse transform using the RFC 9924 §6.3.2.3 even/odd butterfly decomposition,
/// shifting each output by `(sum + round) >> shift`.
#[inline(always)]
fn idct_1d(x: [i32; 8], round: i32, shift: u32) -> [i32; 8] {
    let [x0, x1, x2, x3, x4, x5, x6, x7] = x;
    if (x1 | x2 | x3 | x4 | x5 | x6 | x7) == 0 {
        let v = (x0 * 64 + round) >> shift;
        return [v; 8];
    }
    let e0 = 64 * (x0 + x4);
    let e1 = 64 * (x0 - x4);
    let e2 = 35 * x2 - 84 * x6;
    let e3 = 84 * x2 + 35 * x6;

    let even0 = e0 + e3;
    let even1 = e1 + e2;
    let even2 = e1 - e2;
    let even3 = e0 - e3;

    let odd0 = 89 * x1 + 75 * x3 + 50 * x5 + 18 * x7;
    let odd1 = 75 * x1 - 18 * x3 - 89 * x5 - 50 * x7;
    let odd2 = 50 * x1 - 89 * x3 + 18 * x5 + 75 * x7;
    let odd3 = 18 * x1 - 50 * x3 + 75 * x5 - 89 * x7;

    [
        (even0 + odd0 + round) >> shift,
        (even1 + odd1 + round) >> shift,
        (even2 + odd2 + round) >> shift,
        (even3 + odd3 + round) >> shift,
        (even3 - odd3 + round) >> shift,
        (even2 - odd2 + round) >> shift,
        (even1 - odd1 + round) >> shift,
        (even0 - odd0 + round) >> shift,
    ]
}

/// Fast path when an 8x8 block has only a DC coefficient (`coeff[1..64]` are all zero).
#[inline]
pub fn scale_and_idct8x8_dc_only(dc_coeff: i16, q0: u8, qp: u8, bit_depth: u8, out: &mut [u16; 64]) {
    let bd_shift_scale = (bit_depth as u32).saturating_sub(2);
    let scale_round = if bd_shift_scale > 0 { 1i64 << (bd_shift_scale - 1) } else { 0 };
    let level_scale = LEVEL_SCALE[(qp % 6) as usize] as i64;
    let qp_shift = (qp / 6) as u32;

    let scaled = ((dc_coeff as i64 * q0 as i64 * level_scale) << qp_shift) + scale_round;
    let d0 = (scaled >> bd_shift_scale).clamp(-32768, 32767) as i32;
    let g0 = (d0 * 64 + 64) >> 7;

    let bd_shift = 20u32.saturating_sub(bit_depth as u32);
    let round = 1i32 << (bd_shift - 1);
    let mid = 1i32 << (bit_depth - 1);
    let max_val = (1i32 << bit_depth) - 1;
    let sample = (((g0 * 64 + round) >> bd_shift) + mid).clamp(0, max_val) as u16;
    out.fill(sample);
}

/// Scale and inverse-transform one 8x8 block of transform coefficients (indexed by `y * 8 + x`)
/// into reconstructed samples at `bit_depth` (`10..=16`), per RFC 9924 §6.3.
#[inline]
pub fn scale_and_idct8x8(coeff: &[i16; 64], q_matrix: &[u8; 64], qp: u8, bit_depth: u8, out: &mut [u16; 64]) {
    let bd_shift_scale = (bit_depth as u32).saturating_sub(2);
    let scale_round = if bd_shift_scale > 0 { 1i64 << (bd_shift_scale - 1) } else { 0 };
    let level_scale = LEVEL_SCALE[(qp % 6) as usize] as i64;
    let qp_shift = (qp / 6) as u32;

    let mut d = [0i32; 64];
    for idx in 0..64 {
        let c = coeff[idx] as i64;
        if c != 0 {
            let qm = q_matrix[idx] as i64;
            let scaled = ((c * qm * level_scale) << qp_shift) + scale_round;
            d[idx] = (scaled >> bd_shift_scale).clamp(-32768, 32767) as i32;
        }
    }

    // 1D vertical transform on each column x = 0..8, followed by (e + 64) >> 7
    let mut g = [0i32; 64];
    for x in 0..8 {
        let col = idct_1d(column(&d, x), 64, 7);
        for i in 0..8 {
            g[i * 8 + x] = col[i];
        }
    }

    // 1D horizontal transform on each row y = 0..8, followed by final bit-depth shift and offset
    let bd_shift = 20u32.saturating_sub(bit_depth as u32);
    let round = 1i32 << (bd_shift - 1);
    let mid = 1i32 << (bit_depth - 1);
    let max_val = (1i32 << bit_depth) - 1;

    for y in 0..8 {
        let r = &g[y * 8..y * 8 + 8];
        let row = idct_1d([r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7]], round, bd_shift);
        for i in 0..8 {
            out[y * 8 + i] = (row[i] + mid).clamp(0, max_val) as u16;
        }
    }
}

/// 1D 8-point forward butterfly transform matching `TRANS_MATRIX`.
#[inline(always)]
fn fdct_1d(x: [i32; 8], round: i32, shift: u32) -> [i32; 8] {
    let [x0, x1, x2, x3, x4, x5, x6, x7] = x;
    let s0 = x0 + x7;
    let d0 = x0 - x7;
    let s1 = x1 + x6;
    let d1 = x1 - x6;
    let s2 = x2 + x5;
    let d2 = x2 - x5;
    let s3 = x3 + x4;
    let d3 = x3 - x4;

    let es0 = s0 + s3;
    let ed0 = s0 - s3;
    let es1 = s1 + s2;
    let ed1 = s1 - s2;

    [
        (64 * (es0 + es1) + round) >> shift,
        (89 * d0 + 75 * d1 + 50 * d2 + 18 * d3 + round) >> shift,
        (84 * ed0 + 35 * ed1 + round) >> shift,
        (75 * d0 - 18 * d1 - 89 * d2 - 50 * d3 + round) >> shift,
        (64 * (es0 - es1) + round) >> shift,
        (50 * d0 - 89 * d1 + 18 * d2 + 75 * d3 + round) >> shift,
        (35 * ed0 - 84 * ed1 + round) >> shift,
        (18 * d0 - 50 * d1 + 75 * d2 - 89 * d3 + round) >> shift,
    ]
}

/// Forward 8x8 transform of level-shifted samples (`sample - (1 << (bit_depth - 1))`) into the
/// scaled coefficient domain `d[y * 8 + x]` matching [`scale_and_idct8x8`].
#[inline]
pub fn fdct8x8(samples: &[u16; 64], bit_depth: u8) -> [i32; 64] {
    let mid = 1i32 << (bit_depth - 1);
    let shift1 = (bit_depth as u32).saturating_sub(5);
    let add1 = if shift1 > 0 { 1i32 << (shift1 - 1) } else { 0 };

    // Horizontal forward pass
    let mut t = [0i32; 64];
    for y in 0..8 {
        let s = &samples[y * 8..y * 8 + 8];
        let row = fdct_1d(std::array::from_fn(|i| s[i] as i32 - mid), add1, shift1);
        t[y * 8..y * 8 + 8].copy_from_slice(&row);
    }

    // Vertical forward pass
    let mut f = [0i32; 64];
    for u in 0..8 {
        let col = fdct_1d(column(&t, u), 128, 8);
        for v in 0..8 {
            f[v * 8 + u] = col[v];
        }
    }
    f
}

/// Quantize forward-transformed coefficients `f` (in the `d` domain) at `qp` with `q_matrix`.
#[inline]
pub fn quantize8x8(f: &[i32; 64], q_matrix: &[u8; 64], qp: u8, bit_depth: u8) -> [i16; 64] {
    let bd_shift_scale = (bit_depth as u32).saturating_sub(2);
    let level_scale = LEVEL_SCALE[(qp % 6) as usize] as i64;
    let qp_shift = (qp / 6) as u32;

    let mut out = [0i16; 64];
    for idx in 0..64 {
        let val = f[idx] as i64;
        if val == 0 {
            continue;
        }
        let sign = if val < 0 { -1i64 } else { 1i64 };
        let abs_val = val.unsigned_abs() as i64;
        let qm = q_matrix[idx].max(1) as i64;
        let den = (qm * level_scale) << qp_shift;
        let num = abs_val << bd_shift_scale;
        // Deadzone/rounding offset: 1/2 for DC, 3/8 for AC
        let offset = if idx == 0 { den / 2 } else { (den * 3) / 8 };
        let level = ((num + offset) / den).min(32767);
        out[idx] = (level * sign) as i16;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fdct_idct_roundtrip_10bit() {
        let mut src = [0u16; 64];
        for (i, s) in src.iter_mut().enumerate() {
            *s = (64 + (i * 13) % 850) as u16;
        }
        let qm = [16u8; 64];
        // At qp = 0 (very fine quantization), roundtrip is within 1 LSB
        let f = fdct8x8(&src, 10);
        let q = quantize8x8(&f, &qm, 0, 10);
        let mut rec = [0u16; 64];
        scale_and_idct8x8(&q, &qm, 0, 10, &mut rec);
        for i in 0..64 {
            let err = (src[i] as i32 - rec[i] as i32).abs();
            assert!(err <= 1, "idx {i}: src={} rec={} err={err}", src[i], rec[i]);
        }
    }
}
