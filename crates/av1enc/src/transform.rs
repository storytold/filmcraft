//! Discrete Cosine Transform (2D DCT) and quantisation (spec 7.13 / 5.11).

/// Forward 4x4 2D DCT.
pub fn fdct4x4(src: &[i16; 16], dst: &mut [i32; 16]) {
    // 1D DCT on rows
    let mut intermediate = [0i32; 16];
    for y in 0..4 {
        let row = &src[y * 4..y * 4 + 4];
        let (s0, s1, s2, s3) = (row[0] as i32, row[1] as i32, row[2] as i32, row[3] as i32);
        let a0 = s0 + s3;
        let a1 = s1 + s2;
        let a2 = s1 - s2;
        let a3 = s0 - s3;

        intermediate[y * 4] = (a0 + a1) * 64;
        intermediate[y * 4 + 2] = (a0 - a1) * 64;
        intermediate[y * 4 + 1] = a3 * 83 + a2 * 36;
        intermediate[y * 4 + 3] = a3 * 36 - a2 * 83;
    }

    // 1D DCT on columns
    for x in 0..4 {
        let s0 = intermediate[x];
        let s1 = intermediate[x + 4];
        let s2 = intermediate[x + 8];
        let s3 = intermediate[x + 12];

        let a0 = s0 + s3;
        let a1 = s1 + s2;
        let a2 = s1 - s2;
        let a3 = s0 - s3;

        dst[x] = (a0 + a1 + 4) >> 3;
        dst[x + 8] = (a0 - a1 + 4) >> 3;
        dst[x + 4] = ((a3 * 83 + a2 * 36) + 256) >> 9;
        dst[x + 12] = ((a3 * 36 - a2 * 83) + 256) >> 9;
    }
}

/// Quantise DCT coefficients with deadzone / round.
pub fn quantise4x4(coeffs: &[i32; 16], qstep: i32, out: &mut [i16; 16]) -> usize {
    let mut eob = 0;
    for i in 0..16 {
        let c = coeffs[i];
        let sign = c < 0;
        let abs = c.abs();
        let q = (abs + (qstep / 2)) / qstep;
        let val = if sign { -(q as i16) } else { q as i16 };
        out[i] = val;
        if val != 0 {
            eob = i + 1;
        }
    }
    eob
}

/// Inverse 4x4 2D DCT for reconstruction.
pub fn idct4x4(src: &[i16; 16], qstep: i32, dst: &mut [i16; 16]) {
    let mut intermediate = [0i32; 16];
    // Dequantise and 1D IDCT on columns
    for x in 0..4 {
        let s0 = src[x] as i32 * qstep;
        let s1 = src[x + 4] as i32 * qstep;
        let s2 = src[x + 8] as i32 * qstep;
        let s3 = src[x + 12] as i32 * qstep;

        let a0 = s0 + s2;
        let a1 = s0 - s2;
        let a2 = (s1 * 36 - s3 * 83) >> 6;
        let a3 = (s1 * 83 + s3 * 36) >> 6;

        intermediate[x] = a0 + a3;
        intermediate[x + 4] = a1 + a2;
        intermediate[x + 8] = a1 - a2;
        intermediate[x + 12] = a0 - a3;
    }

    // 1D IDCT on rows
    for y in 0..4 {
        let s0 = intermediate[y * 4];
        let s1 = intermediate[y * 4 + 1];
        let s2 = intermediate[y * 4 + 2];
        let s3 = intermediate[y * 4 + 3];

        let a0 = s0 + s2;
        let a1 = s0 - s2;
        let a2 = (s1 * 36 - s3 * 83) >> 6;
        let a3 = (s1 * 83 + s3 * 36) >> 6;

        dst[y * 4] = ((a0 + a3 + 4) >> 3) as i16;
        dst[y * 4 + 1] = ((a1 + a2 + 4) >> 3) as i16;
        dst[y * 4 + 2] = ((a1 - a2 + 4) >> 3) as i16;
        dst[y * 4 + 3] = ((a0 - a3 + 4) >> 3) as i16;
    }
}

use crate::tables::{AC_QLOOKUP_8BIT, DC_QLOOKUP_8BIT};

/// Look up spec-compliant DC quantization step for 8-bit AV1.
pub fn qstep_dc(qindex: u8) -> i32 {
    DC_QLOOKUP_8BIT.get(qindex as usize).copied().unwrap_or(1) as i32
}

/// Look up spec-compliant AC quantization step for 8-bit AV1.
pub fn qstep_ac(qindex: u8) -> i32 {
    AC_QLOOKUP_8BIT.get(qindex as usize).copied().unwrap_or(1) as i32
}

/// Quantise 4x4 DCT coefficients with separate DC and AC quantization steps according to AV1 spec.
pub fn quantise_block_4x4(coeffs: &[i32; 16], qindex: u8, out: &mut [i16; 16]) -> usize {
    let dc_step = qstep_dc(qindex);
    let ac_step = qstep_ac(qindex);
    let mut eob = 0;
    for i in 0..16 {
        let step = if i == 0 { dc_step } else { ac_step };
        let c = coeffs[i];
        let sign = c < 0;
        let abs = c.abs();
        let q = (abs + (step / 2)) / step;
        let val = if sign { -(q as i16) } else { q as i16 };
        out[i] = val;
        if val != 0 {
            eob = i + 1;
        }
    }
    eob
}

/// Dequantise and inverse 4x4 2D DCT using spec-compliant DC and AC steps.
pub fn dequantise_and_idct4x4(src: &[i16; 16], qindex: u8, dst: &mut [i16; 16]) {
    let dc_step = qstep_dc(qindex);
    let ac_step = qstep_ac(qindex);
    let mut intermediate = [0i32; 16];
    // Dequantise and 1D IDCT on columns
    for x in 0..4 {
        let s0 = src[x] as i32 * if x == 0 { dc_step } else { ac_step };
        let s1 = src[x + 4] as i32 * ac_step;
        let s2 = src[x + 8] as i32 * ac_step;
        let s3 = src[x + 12] as i32 * ac_step;

        let a0 = s0 + s2;
        let a1 = s0 - s2;
        let a2 = (s1 * 36 - s3 * 83) >> 6;
        let a3 = (s1 * 83 + s3 * 36) >> 6;

        intermediate[x] = a0 + a3;
        intermediate[x + 4] = a1 + a2;
        intermediate[x + 8] = a1 - a2;
        intermediate[x + 12] = a0 - a3;
    }

    // 1D IDCT on rows
    for y in 0..4 {
        let s0 = intermediate[y * 4];
        let s1 = intermediate[y * 4 + 1];
        let s2 = intermediate[y * 4 + 2];
        let s3 = intermediate[y * 4 + 3];

        let a0 = s0 + s2;
        let a1 = s0 - s2;
        let a2 = (s1 * 36 - s3 * 83) >> 6;
        let a3 = (s1 * 83 + s3 * 36) >> 6;

        dst[y * 4] = ((a0 + a3 + 64) >> 7) as i16;
        dst[y * 4 + 1] = ((a1 + a2 + 64) >> 7) as i16;
        dst[y * 4 + 2] = ((a1 - a2 + 64) >> 7) as i16;
        dst[y * 4 + 3] = ((a0 - a3 + 64) >> 7) as i16;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fdct_idct_roundtrip() {
        let mut residual = [0i16; 16];
        for i in 0..16 {
            residual[i] = (i as i16 * 7) - 30;
        }
        let mut dct = [0i32; 16];
        fdct4x4(&residual, &mut dct);

        let mut quant = [0i16; 16];
        let eob = quantise_block_4x4(&dct, 20, &mut quant);
        assert!(eob > 0);

        let mut reconstructed = [0i16; 16];
        dequantise_and_idct4x4(&quant, 20, &mut reconstructed);

        // Near-lossless at low qindex
        for (orig, rec) in residual.iter().zip(reconstructed.iter()) {
            assert!((orig - rec).abs() <= 5, "orig {orig} vs rec {rec}");
        }
    }
}
