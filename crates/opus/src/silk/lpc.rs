//! Fixed-point NLSF decoding, NLSF→LPC conversion and filter stabilisation
//! (RFC 6716 §4.2.7.5, with the RFC 8251 §6/§7 overflow fixes).

use super::tables::{
    COS_Q12, LSF_ORDER_NBMB, LSF_ORDER_WB, NLSF_CB_NBMB, NLSF_CB_WB, NLSF_MIN_SPACING_NBMB, NLSF_MIN_SPACING_WB, NLSF_PRED_NBMB, NLSF_PRED_WB, NLSF_PSEL_NBMB,
    NLSF_PSEL_WB,
};

pub const MAX_LPC_ORDER: usize = 16;

#[inline]
pub fn smulwb(a: i32, b: i32) -> i32 {
    ((a as i64 * (b as i16) as i64) >> 16) as i32
}
#[inline]
pub fn smlawb(a: i32, b: i32, c: i32) -> i32 {
    a.wrapping_add(smulwb(b, c))
}
#[inline]
pub fn smulww(a: i32, b: i32) -> i32 {
    ((a as i64 * b as i64) >> 16) as i32
}
#[inline]
pub fn smmul(a: i32, b: i32) -> i32 {
    ((a as i64 * b as i64) >> 32) as i32
}
#[inline]
pub fn rshift_round(a: i32, s: u32) -> i32 {
    if s == 1 { (a >> 1) + (a & 1) } else { ((a >> (s - 1)) + 1) >> 1 }
}
#[inline]
pub fn rshift_round64(a: i64, s: u32) -> i64 {
    if s == 1 { (a >> 1) + (a & 1) } else { ((a >> (s - 1)) + 1) >> 1 }
}
#[inline]
pub fn sat16(a: i32) -> i32 {
    a.clamp(-32768, 32767)
}

/// `silk_INVERSE32_varQ`: approximation of `(1 << q_res) / b32`.
pub fn inverse32_varq(b32: i32, q_res: i32) -> i32 {
    let b_headrm = b32.unsigned_abs().leading_zeros() as i32 - 1;
    let b32_nrm = b32.wrapping_shl(b_headrm as u32);
    let b32_inv = (i32::MAX >> 2) / (b32_nrm >> 16);
    let mut result = b32_inv << 16;
    let err_q32 = ((1i32 << 29).wrapping_sub(smulwb(b32_nrm, b32_inv))).wrapping_shl(3);
    result = result.wrapping_add(((err_q32 as i64 * b32_inv as i64) >> 16) as i32);
    let lshift = 61 - b_headrm - q_res;
    if lshift <= 0 {
        let s = (-lshift) as u32;
        let lim_hi = i32::MAX >> s;
        let lim_lo = i32::MIN >> s;
        result.clamp(lim_lo, lim_hi) << s
    } else if lshift < 32 {
        result >> lshift
    } else {
        0
    }
}

/// `silk_DIV32_varQ`: approximation of `(a32 << q_res) / b32`.
pub fn div32_varq(a32: i32, b32: i32, q_res: i32) -> i32 {
    let a_headrm = a32.unsigned_abs().leading_zeros() as i32 - 1;
    let a32_nrm = a32.wrapping_shl(a_headrm as u32);
    let b_headrm = b32.unsigned_abs().leading_zeros() as i32 - 1;
    let b32_nrm = b32.wrapping_shl(b_headrm as u32);
    let b32_inv = (i32::MAX >> 2) / (b32_nrm >> 16);
    let mut result = smulwb(a32_nrm, b32_inv);
    let a32_nrm = a32_nrm.wrapping_sub(((smmul(b32_nrm, result) as i64) << 3) as i32);
    result = smlawb(result, a32_nrm, b32_inv);
    let lshift = 29 + a_headrm - b_headrm - q_res;
    if lshift < 0 {
        let s = (-lshift) as u32;
        let lim_hi = i32::MAX >> s;
        let lim_lo = i32::MIN >> s;
        result.clamp(lim_lo, lim_hi) << s
    } else if lshift < 32 {
        result >> lshift
    } else {
        0
    }
}

fn sqrt_approx(x: i32) -> i32 {
    if x <= 0 {
        return 0;
    }
    let lz = x.leading_zeros() as i32;
    let frac = (x.rotate_right((24 - lz).rem_euclid(32) as u32)) & 0x7f;
    let mut y = if lz & 1 != 0 { 32768 } else { 46214 };
    y >>= lz >> 1;
    smlawb(y, y, 213 * frac)
}

/// Decodes the stage-1/stage-2 indices into stabilised NLSFs (Q15).
pub fn nlsf_decode(indices: &[i32], wb: bool) -> [i32; MAX_LPC_ORDER] {
    let order = if wb { 16 } else { 10 };
    let i1 = indices[0] as usize;
    let cb1: &[i32] = if wb { &NLSF_CB_WB[i1] } else { &NLSF_CB_NBMB[i1] };
    let qstep = if wb { 9830 } else { 11796 };
    // Residual dequantisation with backwards prediction.
    let mut res_q10 = [0i32; MAX_LPC_ORDER];
    let mut out_q10 = 0i32;
    for i in (0..order).rev() {
        let pred = if i + 1 < order {
            let sel = if wb { NLSF_PSEL_WB[i1][i] } else { NLSF_PSEL_NBMB[i1][i] } as usize;
            let w = if wb { NLSF_PRED_WB[sel][i] } else { NLSF_PRED_NBMB[sel][i] };
            (out_q10 * w) >> 8
        } else {
            0
        };
        let mut v = indices[i + 1] << 10;
        if v > 0 {
            v -= 102;
        } else if v < 0 {
            v += 102;
        }
        out_q10 = smlawb(pred, v, qstep);
        res_q10[i] = out_q10;
    }
    // Laroia weights.
    let mut nlsf = [0i32; MAX_LPC_ORDER];
    for k in 0..order {
        let prev = if k == 0 { 0 } else { cb1[k - 1] };
        let next = if k + 1 == order { 256 } else { cb1[k + 1] };
        let w = (1024 / (cb1[k] - prev).max(1) + 1024 / (next - cb1[k]).max(1)).min(32767);
        let w_q9 = sqrt_approx(w << 16);
        let v = (cb1[k] << 7) + (res_q10[k] << 14) / w_q9.max(1);
        nlsf[k] = v.clamp(0, 32767);
    }
    let dmin: &[i32] = if wb { &NLSF_MIN_SPACING_WB } else { &NLSF_MIN_SPACING_NBMB };
    nlsf_stabilize(&mut nlsf[..order], dmin);
    nlsf
}

pub fn nlsf_stabilize(nlsf: &mut [i32], dmin: &[i32]) {
    let l = nlsf.len();
    for _ in 0..20 {
        let mut min_diff = nlsf[0] - dmin[0];
        let mut idx = 0;
        for i in 1..l {
            let d = nlsf[i] - (nlsf[i - 1] + dmin[i]);
            if d < min_diff {
                min_diff = d;
                idx = i;
            }
        }
        let d = 32768 - (nlsf[l - 1] + dmin[l]);
        if d < min_diff {
            min_diff = d;
            idx = l;
        }
        if min_diff >= 0 {
            return;
        }
        if idx == 0 {
            nlsf[0] = dmin[0];
        } else if idx == l {
            nlsf[l - 1] = 32768 - dmin[l];
        } else {
            let mut min_center = dmin[..idx].iter().sum::<i32>();
            min_center += dmin[idx] >> 1;
            let mut max_center = 32768 - dmin[idx + 1..=l].iter().sum::<i32>();
            max_center -= dmin[idx] >> 1;
            let c = rshift_round(nlsf[idx - 1] + nlsf[idx], 1);
            let c = if min_center > max_center { c.clamp(max_center, min_center) } else { c.clamp(min_center, max_center) };
            nlsf[idx - 1] = c - (dmin[idx] >> 1);
            nlsf[idx] = nlsf[idx - 1] + dmin[idx];
        }
    }
    // Fallback.
    nlsf.sort_unstable();
    nlsf[0] = nlsf[0].max(dmin[0]);
    for i in 1..l {
        nlsf[i] = nlsf[i].max(sat16(nlsf[i - 1] + dmin[i]));
    }
    nlsf[l - 1] = nlsf[l - 1].min(32768 - dmin[l]);
    for i in (0..l - 1).rev() {
        nlsf[i] = nlsf[i].min(nlsf[i + 1] - dmin[i + 1]);
    }
}

fn bwexpander_32(ar: &mut [i32], mut chirp_q16: i32) {
    let d = ar.len();
    let chirp_minus_one = chirp_q16 - 65536;
    for v in ar.iter_mut().take(d - 1) {
        *v = smulww(chirp_q16, *v);
        chirp_q16 += rshift_round(chirp_q16.wrapping_mul(chirp_minus_one), 16);
    }
    ar[d - 1] = smulww(chirp_q16, ar[d - 1]);
}

/// `silk_bwexpander` on Q12 coefficients (16-bit).
pub fn bwexpander(ar: &mut [i32], mut chirp_q16: i32) {
    let d = ar.len();
    let chirp_minus_one = chirp_q16 - 65536;
    for v in ar.iter_mut().take(d - 1) {
        *v = rshift_round(chirp_q16.wrapping_mul(*v), 16);
        chirp_q16 += rshift_round(chirp_q16.wrapping_mul(chirp_minus_one), 16);
    }
    ar[d - 1] = rshift_round(chirp_q16.wrapping_mul(ar[d - 1]), 16);
}

fn lpc_inverse_pred_gain(a_q12: &[i32]) -> i32 {
    const QA: u32 = 24;
    const A_LIMIT: i32 = 16_773_022;
    let order = a_q12.len();
    let mut a = [0i32; MAX_LPC_ORDER];
    let mut dc = 0;
    for k in 0..order {
        dc += a_q12[k];
        a[k] = a_q12[k] << (QA - 12);
    }
    if dc >= 4096 {
        return 0;
    }
    let mut inv_gain: i32 = 1 << 30;
    let mul32_frac_q31 = |x: i32, y: i32| rshift_round64(x as i64 * y as i64, 31) as i32;
    for k in (1..order).rev() {
        if a[k] > A_LIMIT || a[k] < -A_LIMIT {
            return 0;
        }
        let rc_q31 = -(a[k] << (31 - QA));
        let rc_mult1 = (1i32 << 30) - smmul(rc_q31, rc_q31);
        inv_gain = smmul(inv_gain, rc_mult1) << 2;
        if inv_gain < 107_374 {
            return 0;
        }
        let mult2q = 32 - rc_mult1.unsigned_abs().leading_zeros() as i32;
        let rc_mult2 = inverse32_varq(rc_mult1, mult2q + 30);
        for n in 0..(k + 1) >> 1 {
            let t1 = a[n];
            let t2 = a[k - n - 1];
            let v1 = rshift_round64(t1.saturating_sub(mul32_frac_q31(t2, rc_q31)) as i64 * rc_mult2 as i64, mult2q as u32);
            if v1 > i32::MAX as i64 || v1 < i32::MIN as i64 {
                return 0;
            }
            let v2 = rshift_round64(t2.saturating_sub(mul32_frac_q31(t1, rc_q31)) as i64 * rc_mult2 as i64, mult2q as u32);
            if v2 > i32::MAX as i64 || v2 < i32::MIN as i64 {
                return 0;
            }
            a[n] = v1 as i32;
            a[k - n - 1] = v2 as i32;
        }
    }
    if a[0] > A_LIMIT || a[0] < -A_LIMIT {
        return 0;
    }
    let rc_q31 = -(a[0] << (31 - QA));
    let rc_mult1 = (1i32 << 30) - smmul(rc_q31, rc_q31);
    inv_gain = smmul(inv_gain, rc_mult1) << 2;
    if inv_gain < 107_374 {
        return 0;
    }
    inv_gain
}

/// Converts NLSFs (Q15) to stable Q12 LPC coefficients (`silk_NLSF2A`).
pub fn nlsf2a(nlsf: &[i32]) -> [i32; MAX_LPC_ORDER] {
    let d = nlsf.len();
    let ordering: &[usize] = if d == 16 { &LSF_ORDER_WB } else { &LSF_ORDER_NBMB };
    let mut c_q17 = [0i32; MAX_LPC_ORDER];
    for k in 0..d {
        let i = (nlsf[k] >> 8) as usize;
        let f = nlsf[k] & 255;
        let c0 = COS_Q12[i];
        let c1 = COS_Q12[(i + 1).min(128)];
        c_q17[ordering[k]] = (c0 * 256 + (c1 - c0) * f + 4) >> 3;
    }
    let d2 = d / 2;
    let find_poly = |off: usize| -> [i64; 10] {
        let mut out = [0i64; 10];
        out[0] = 1 << 16;
        out[1] = -(c_q17[off] as i64);
        for k in 1..d2 {
            let ftmp = c_q17[2 * k + off] as i64;
            out[k + 1] = 2 * out[k - 1] - ((ftmp * out[k] + 32768) >> 16);
            for n in (2..=k).rev() {
                out[n] += out[n - 2] - ((ftmp * out[n - 1] + 32768) >> 16);
            }
            out[1] -= ftmp;
        }
        out
    };
    let p = find_poly(0);
    let q = find_poly(1);
    let mut a32 = [0i32; MAX_LPC_ORDER];
    for k in 0..d2 {
        let ptmp = p[k + 1] + p[k];
        let qtmp = q[k + 1] - q[k];
        a32[k] = (-qtmp - ptmp) as i32;
        a32[d - k - 1] = (qtmp - ptmp) as i32;
    }
    let a32 = &mut a32[..d];
    // Limit the dynamic range (silk_LPC_fit with QIN = 17, QOUT = 12).
    let mut a_q12 = [0i32; MAX_LPC_ORDER];
    let mut i = 0;
    while i < 10 {
        let mut maxabs = 0i32;
        let mut idx = 0;
        for (k, &v) in a32.iter().enumerate() {
            let av = v.saturating_abs();
            if av > maxabs {
                maxabs = av;
                idx = k;
            }
        }
        let maxabs = rshift_round(maxabs, 5);
        if maxabs > 32767 {
            let maxabs = maxabs.min(163_838);
            let chirp = 65470 - ((maxabs - 32767) << 14) / ((maxabs * (idx as i32 + 1)) >> 2);
            bwexpander_32(a32, chirp);
        } else {
            break;
        }
        i += 1;
    }
    if i == 10 {
        for k in 0..d {
            a_q12[k] = sat16(rshift_round(a32[k], 5));
            a32[k] = a_q12[k] << 5;
        }
    } else {
        for k in 0..d {
            a_q12[k] = rshift_round(a32[k], 5);
        }
    }
    for i in 0..16 {
        if lpc_inverse_pred_gain(&a_q12[..d]) == 0 {
            bwexpander_32(a32, 65536 - (2 << i));
            for k in 0..d {
                a_q12[k] = rshift_round(a32[k], 5);
            }
        } else {
            break;
        }
    }
    a_q12
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_nlsf_gives_small_lpc() {
        // Uniformly spaced NLSFs correspond to an (almost) flat spectrum.
        let nlsf: Vec<i32> = (1..=16).map(|k| k * 32768 / 17).collect();
        let a = nlsf2a(&nlsf);
        assert!(a.iter().all(|v| v.abs() < 600), "{a:?}");
        let nlsf: Vec<i32> = (1..=10).map(|k| k * 32768 / 11).collect();
        let a = nlsf2a(&nlsf);
        assert!(a[..10].iter().all(|v| v.abs() < 600), "{a:?}");
    }

    #[test]
    fn stabilize_enforces_spacing() {
        let mut v = [100, 90, 80, 30000, 30001, 30002, 31000, 32000, 32700, 32760];
        nlsf_stabilize(&mut v, &NLSF_MIN_SPACING_NBMB);
        assert!(v[0] >= NLSF_MIN_SPACING_NBMB[0]);
        for i in 1..10 {
            assert!(v[i] - v[i - 1] >= NLSF_MIN_SPACING_NBMB[i], "{v:?}");
        }
        assert!(32768 - v[9] >= NLSF_MIN_SPACING_NBMB[10]);
    }

    #[test]
    fn inverse_approximations() {
        for &b in &[65536, 100_000, 1 << 20, 81920, 1_686_110_208] {
            let inv = inverse32_varq(b, 47) as f64;
            let exact = 2f64.powi(47) / b as f64;
            assert!((inv - exact).abs() / exact < 1e-3, "{b}");
        }
        let q = div32_varq(65536, 131072, 16) as f64;
        assert!((q - 32768.0).abs() < 2.0);
    }
}
