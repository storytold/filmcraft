//! The ALAC adaptive predictor, encoder side.
//!
//! The decoder rebuilds each sample from the residual as
//! `out[j] = signext(res[j] + top + ((Σ c[k]·(out[j−1−k] − top) + 2^(s−1)) >> s))`, with
//! `top = out[j − n − 1]`, `n` active coefficients and denominator shift `s`. After each sample it
//! nudges the coefficients by ±1 towards a smaller residual (a sign-sign LMS, oldest tap first,
//! stopping once the residual's share is used up). The first `n + 1` samples are a first-order
//! difference. [`residuals`] runs the same recurrence on the input, so the coefficients after a
//! block are the ones the decoder has too, and the encoder can carry them into the next block's
//! header. All arithmetic is 32-bit and wraps, as in the decoders.

/// `-1`, `0` or `1`.
#[inline]
fn sign(v: i32) -> i32 {
    (v > 0) as i32 - (v < 0) as i32
}

/// `v` sign-extended from its low `bits` bits (1..=32).
#[inline]
pub fn sign_extend(v: i32, bits: u32) -> i32 {
    let sh = 32 - bits.clamp(1, 32);
    v.wrapping_shl(sh).wrapping_shr(sh)
}

/// Residuals of `x` (samples within `bits`-bit signed range) for `coefs.len()` active coefficients
/// (0..=30) and denominator shift `den_shift` (1..=15). `coefs` adapts in place exactly as the
/// decoder's do.
pub fn residuals(x: &[i32], coefs: &mut [i16], bits: u32, den_shift: u32, out: &mut Vec<i32>) {
    out.clear();
    let n = x.len();
    let Some(&first) = x.first() else { return };
    out.reserve(n);
    out.push(first);
    let order = coefs.len();
    if order == 0 {
        out.extend_from_slice(x.get(1..).unwrap_or_default());
        return;
    }
    let lim = order + 1;
    for j in 1..lim.min(n) {
        out.push(sign_extend(x[j].wrapping_sub(x[j - 1]), bits));
    }
    let den_half = 1i32 << (den_shift.clamp(1, 15) - 1);
    let den_shift = den_shift.clamp(1, 15);
    for j in lim..n {
        let top = x[j - lim];
        // most recent sample first: coefs[k] weighs x[j - 1 - k]
        let mut sum = 0i32;
        for (k, &c) in coefs.iter().enumerate() {
            sum = sum.wrapping_add(i32::from(c).wrapping_mul(x[j - 1 - k].wrapping_sub(top)));
        }
        let pred = top.wrapping_add(sum.wrapping_add(den_half) >> den_shift);
        let del = sign_extend(x[j].wrapping_sub(pred), bits);
        out.push(del);
        let sg = sign(del);
        let mut del0 = del;
        if sg > 0 {
            for k in (0..order).rev() {
                let dd = top.wrapping_sub(x[j - 1 - k]);
                let s = sign(dd);
                coefs[k] = coefs[k].wrapping_sub(s as i16);
                del0 = del0.wrapping_sub(((order - k) as i32).wrapping_mul(s.wrapping_mul(dd) >> den_shift));
                if del0 <= 0 {
                    break;
                }
            }
        } else if sg < 0 {
            for k in (0..order).rev() {
                let dd = top.wrapping_sub(x[j - 1 - k]);
                let s = sign(dd);
                coefs[k] = coefs[k].wrapping_add(s as i16);
                del0 = del0.wrapping_sub(((order - k) as i32).wrapping_mul((-s).wrapping_mul(dd) >> den_shift));
                if del0 >= 0 {
                    break;
                }
            }
        }
    }
}

/// The decoder's reconstruction (for tests): the inverse of [`residuals`].
#[cfg(test)]
pub fn reconstruct(res: &[i32], coefs: &mut [i16], bits: u32, den_shift: u32) -> Vec<i32> {
    let n = res.len();
    let mut out = Vec::with_capacity(n);
    let Some(&first) = res.first() else { return out };
    out.push(first);
    let order = coefs.len();
    if order == 0 {
        out.extend_from_slice(&res[1..]);
        return out;
    }
    let lim = order + 1;
    for j in 1..lim.min(n) {
        out.push(sign_extend(res[j].wrapping_add(out[j - 1]), bits));
    }
    let den_half = 1i32 << (den_shift - 1);
    for j in lim..n {
        let top = out[j - lim];
        let mut sum = 0i32;
        for (k, &c) in coefs.iter().enumerate() {
            sum = sum.wrapping_add(i32::from(c).wrapping_mul(out[j - 1 - k].wrapping_sub(top)));
        }
        let del = res[j];
        out.push(sign_extend(del.wrapping_add(top).wrapping_add(sum.wrapping_add(den_half) >> den_shift), bits));
        let sg = sign(del);
        let mut del0 = del;
        if sg > 0 {
            for k in (0..order).rev() {
                let dd = top.wrapping_sub(out[j - 1 - k]);
                let s = sign(dd);
                coefs[k] = coefs[k].wrapping_sub(s as i16);
                del0 = del0.wrapping_sub(((order - k) as i32).wrapping_mul(s.wrapping_mul(dd) >> den_shift));
                if del0 <= 0 {
                    break;
                }
            }
        } else if sg < 0 {
            for k in (0..order).rev() {
                let dd = top.wrapping_sub(out[j - 1 - k]);
                let s = sign(dd);
                coefs[k] = coefs[k].wrapping_add(s as i16);
                del0 = del0.wrapping_sub(((order - k) as i32).wrapping_mul((-s).wrapping_mul(dd) >> den_shift));
                if del0 >= 0 {
                    break;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signal(n: usize, bits: u32, seed: u32) -> Vec<i32> {
        let max = ((1i64 << (bits - 1)) - 1) as f64;
        let mut s = seed | 1;
        (0..n)
            .map(|i| {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                let noise = (s >> 8) as f64 / (1u32 << 24) as f64 - 0.5;
                let v = 0.6 * (i as f64 * 0.031).sin() + 0.25 * (i as f64 * 0.17).cos() + 0.05 * noise;
                (v * max).round() as i32
            })
            .collect()
    }

    #[test]
    fn reconstruction_inverts_residuals_and_adapts_alike() {
        for (bits, order) in [(16u32, 8usize), (17, 4), (17, 8), (20, 8), (21, 16), (16, 0), (16, 1)] {
            let x = signal(4096, bits, bits * 7 + order as u32);
            let init: Vec<i16> = (0..order).map(|k| [1216i16, -928, -64][k.min(2)] * i16::from(k < 3)).collect();
            let (mut ce, mut cd) = (init.clone(), init.clone());
            let mut r = Vec::new();
            residuals(&x, &mut ce, bits, 9, &mut r);
            assert_eq!(reconstruct(&r, &mut cd, bits, 9), x, "bits {bits} order {order}");
            assert_eq!(ce, cd, "the coefficients adapt the same way");
            // a predictable signal leaves small residuals
            if order >= 4 {
                let mean = r.iter().skip(64).map(|v| v.unsigned_abs() as f64).sum::<f64>() / (r.len() - 64) as f64;
                assert!(mean < (1u64 << (bits - 4)) as f64, "bits {bits} order {order}: mean residual {mean}");
            }
        }
    }

    #[test]
    fn extremes_wrap_like_the_decoder() {
        // full-scale square waves and noise overflow the 32-bit sums: both sides wrap the same way
        for bits in [16u32, 17, 21, 32] {
            let hi = ((1i64 << (bits - 1)) - 1) as i32;
            let lo = (-(1i64 << (bits - 1))) as i32;
            let x: Vec<i32> = (0..3000).map(|i| if (i / 3) % 2 == 0 { hi } else { lo }).collect();
            let init = vec![32767i16, -32768, 32767, -32768, 1, -1, 0, 5];
            let (mut ce, mut cd) = (init.clone(), init);
            let mut r = Vec::new();
            residuals(&x, &mut ce, bits, 9, &mut r);
            assert!(r.iter().all(|&v| sign_extend(v, bits) == v), "residuals fit {bits} bits");
            assert_eq!(reconstruct(&r, &mut cd, bits, 9), x, "bits {bits}");
            // and both end on the same coefficients, which start the next frame
            assert_eq!(ce, cd, "coefficients, bits {bits}");
        }
        let mut c: Vec<i16> = Vec::new();
        let mut r = Vec::new();
        residuals(&[], &mut c, 16, 9, &mut r);
        assert!(r.is_empty());
        residuals(&[5], &mut [1, 2, 3], 16, 9, &mut r);
        assert_eq!(r, [5]);
    }
}
