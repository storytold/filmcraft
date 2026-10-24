//! Inverse transforms (14.3, 14.4). Inputs, the intermediate signal and the outputs are 16-bit
//! signed integers, as the specification requires; the casts to `i16` reproduce that.

const COSPI8SQRT2MINUS1: i32 = 20091;
const SINPI8SQRT2: i32 = 35468;

/// Inverse WHT of the Y2 block (14.3): `input` in raster order; `out[i]` becomes the DC of
/// Y subblock `i`.
pub fn inverse_wht(input: &[i16; 16]) -> [i16; 16] {
    let mut tmp = [0i16; 16];
    for i in 0..4 {
        let ip = |k: usize| i32::from(input[i + 4 * k]);
        let a1 = ip(0) + ip(3);
        let b1 = ip(1) + ip(2);
        let c1 = ip(1) - ip(2);
        let d1 = ip(0) - ip(3);
        tmp[i] = (a1 + b1) as i16;
        tmp[i + 4] = (c1 + d1) as i16;
        tmp[i + 8] = (a1 - b1) as i16;
        tmp[i + 12] = (d1 - c1) as i16;
    }
    let mut out = [0i16; 16];
    for i in 0..4 {
        let ip = |k: usize| i32::from(tmp[4 * i + k]);
        let a1 = ip(0) + ip(3);
        let b1 = ip(1) + ip(2);
        let c1 = ip(1) - ip(2);
        let d1 = ip(0) - ip(3);
        out[4 * i] = ((a1 + b1 + 3) >> 3) as i16;
        out[4 * i + 1] = ((c1 + d1 + 3) >> 3) as i16;
        out[4 * i + 2] = ((a1 - b1 + 3) >> 3) as i16;
        out[4 * i + 3] = ((d1 - c1 + 3) >> 3) as i16;
    }
    out
}

/// Inverse DCT (14.4) of a 4x4 block in raster order, returning the residue in raster order.
pub fn inverse_dct(input: &[i16; 16]) -> [i16; 16] {
    let mul_s = |x: i32| (x * SINPI8SQRT2) >> 16;
    let mul_c = |x: i32| x + ((x * COSPI8SQRT2MINUS1) >> 16);
    let mut tmp = [0i16; 16];
    for i in 0..4 {
        let ip = |k: usize| i32::from(input[i + 4 * k]);
        let a1 = ip(0) + ip(2);
        let b1 = ip(0) - ip(2);
        let c1 = mul_s(ip(1)) - mul_c(ip(3));
        let d1 = mul_c(ip(1)) + mul_s(ip(3));
        tmp[i] = (a1 + d1) as i16;
        tmp[i + 12] = (a1 - d1) as i16;
        tmp[i + 4] = (b1 + c1) as i16;
        tmp[i + 8] = (b1 - c1) as i16;
    }
    let mut out = [0i16; 16];
    for i in 0..4 {
        let ip = |k: usize| i32::from(tmp[4 * i + k]);
        let a1 = ip(0) + ip(2);
        let b1 = ip(0) - ip(2);
        let c1 = mul_s(ip(1)) - mul_c(ip(3));
        let d1 = mul_c(ip(1)) + mul_s(ip(3));
        out[4 * i] = ((a1 + d1 + 4) >> 3) as i16;
        out[4 * i + 3] = ((a1 - d1 + 4) >> 3) as i16;
        out[4 * i + 1] = ((b1 + c1 + 4) >> 3) as i16;
        out[4 * i + 2] = ((b1 - c1 + 4) >> 3) as i16;
    }
    out
}

/// Add `residue` to the 4x4 block at `pos` of a plane (14.5), saturating to 8 bits.
#[inline]
pub fn add_residue(plane: &mut [u8], stride: usize, pos: usize, residue: &[i16; 16]) {
    for r in 0..4 {
        let Some(row) = plane.get_mut(pos + r * stride..pos + r * stride + 4) else { return };
        for (c, px) in row.iter_mut().enumerate() {
            *px = (i32::from(*px) + i32::from(residue[4 * r + c])).clamp(0, 255) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dc_only_blocks_are_flat() {
        for dc in [-2048i16, -100, -5, -4, -3, 0, 3, 4, 5, 100, 2047] {
            let mut b = [0i16; 16];
            b[0] = dc;
            let out = inverse_dct(&b);
            assert!(out.iter().all(|&v| i32::from(v) == (i32::from(dc) + 4) >> 3), "dct dc {dc}");
            let out = inverse_wht(&b);
            assert!(out.iter().all(|&v| i32::from(v) == (i32::from(dc) + 3) >> 3), "wht dc {dc}");
        }
    }
}
