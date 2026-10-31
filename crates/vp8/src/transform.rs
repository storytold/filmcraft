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

/// Inverse DCT of `coefs` added to the 4x4 block at `pos` of a plane (14.4, 14.5): in one
/// step by the vector kernel when it runs.
#[inline]
pub fn idct_add(plane: &mut [u8], stride: usize, pos: usize, coefs: &[i16; 16]) {
    if !filmcraft_simd::vp8_idct_add(coefs, plane, pos, stride) {
        add_residue(plane, stride, pos, &inverse_dct(coefs));
    }
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

    /// The vector kernel (transform and addition in one) gives what the scalar transform and
    /// addition give: sparse and dense blocks, extreme coefficients (whose intermediate values
    /// wrap to 16 bits), DC-only blocks, and predictions near 0 and 255, on every level.
    #[test]
    fn vector_idct_add_matches_the_scalar_transform() {
        let mut seed = 0x51ab_93c4_77e0_2d1fu64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut ran = 0;
        for case in 0..20000 {
            let mut c = [0i16; 16];
            let density = [0u64, 1, 4, 16][case % 4];
            c[0] = (next() % 4001) as i16 - 2000;
            for v in c.iter_mut().skip(1) {
                if next() % 16 < density {
                    *v = match next() % 8 {
                        0 => i16::MAX,
                        1 => i16::MIN,
                        _ => (next() % 2001) as i16 - 1000,
                    };
                }
            }
            if case % 7 == 0 {
                c[0] = [i16::MAX, i16::MIN, 0][case % 3];
            }
            let stride = 7;
            let plane: Vec<u8> = (0..stride * 5)
                .map(|_| match next() % 6 {
                    0 => 0,
                    1 => 255,
                    _ => next() as u8,
                })
                .collect();
            let mut want = plane.clone();
            add_residue(&mut want, stride, 2, &inverse_dct(&c));
            for &level in filmcraft_simd::levels() {
                let mut got = plane.clone();
                if filmcraft_simd::vp8_idct_add_at(level, &c, &mut got, 2, stride) {
                    ran += 1;
                    assert_eq!(got, want, "{level:?} case {case} {c:?}");
                } else {
                    assert_eq!(got, plane, "nothing written when declined");
                }
            }
        }
        // (only aarch64 has a vector version so far)
        assert!(ran > 0 || !cfg!(target_arch = "aarch64"));
    }

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
