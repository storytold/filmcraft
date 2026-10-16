//! Forward + inverse integer transforms and deadzone quantisation (HEVC §8.6.3–8.6.4).
//!
//! 8-bit 4:2:0 only, flat scaling lists (`m = 16`, no `scaling_list_data`), no transform skip.
//! The inverse path is bit-exact with the decoder (`crates/hevc/src/transform.rs`, spec 8-314/8-315/
//! 8-317/8-299): same shifts, offsets and clipping. The forward path is the encoder-side adjoint —
//! it dots the same `TRANS_MATRIX` / DST rows the decoder uses, with shifts chosen so the chain
//! `fwd → quant → dequant → inv` has unity gain (see `QUANT_MUL`). Forward shifts are encoder choice
//! (the spec normatively defines only the inverse); the values below keep every intermediate in
//! `i32` for full-scale 8-bit residuals while preserving precision for the quantiser to threshold.

use crate::tables::{TRANS_MATRIX, qpc};

/// `levelScale[k]` (spec 8.6.3, used by 8-309). Local copy: `hevcenc` is L0 and cannot depend on `hevc`.
const LEVEL_SCALE: [i32; 6] = [40, 45, 51, 57, 64, 72];

/// DST-VII core for 4x4 luma intra (spec 8-316, `trType == 1`). Rows are coefficient basis vectors,
/// stored exactly like `TRANS_MATRIX` so forward and inverse dot the same rows (see module docs).
const DST_MATRIX: [[i8; 4]; 4] = [[29, 55, 74, 84], [74, 74, 0, -74], [84, -29, -74, 55], [55, -84, 74, -29]];

/// Forward-quant multipliers `QUANT_MUL[r] = round(2^15 / (2 * levelScale[r]))`, one per `qp % 6`.
/// The forward DCT divides by `2^(2*log2N + 5)` (shifts `s1 + s2` below); combined with the dequant
/// gain `16 * levelScale * 2^(qp/6) / 2^(3 + log2N)` (8-309, 8-bit) the residual-domain step is
/// `Qstep = 2 * levelScale[qp%6] * 2^(qp/6) / N`, and `level = (|C| * QUANT_MUL[qp%6] + f) >> qbits`
/// with `qbits = 15 + qp/6 - log2N` divides by exactly that step (worst-case table error 0.2%).
const QUANT_MUL: [i64; 6] = [410, 364, 321, 287, 256, 228];

/// Deadzone rounding offset as a fraction of one quantisation step, in 1/256 units (mirrors `h264enc`).
#[derive(Clone, Copy, Debug)]
pub struct Deadzone {
    pub f256: i64,
}

impl Deadzone {
    /// Intra deadzone ≈ 1/3 step: keeps small detail that inter would kill.
    pub const INTRA: Deadzone = Deadzone { f256: 85 };
    /// Inter deadzone ≈ 1/6 step: residuals are already motion-compensated, so threshold harder.
    pub const INTER: Deadzone = Deadzone { f256: 43 };
}

/// Luma + chroma QPs for one block. 8-bit implies `QpBdOffset = 0`, so chroma follows Table 8-10 directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuantParams {
    pub qp_y: u8,
    pub qp_cb: u8,
    pub qp_cr: u8,
}

impl QuantParams {
    /// Derive chroma QPs from the luma QP plus the combined (PPS + slice + CU) chroma offsets (spec 8.6.1).
    pub fn new(qp_y: u8, cb_offset: i32, cr_offset: i32) -> Self {
        let qp_y = qp_y.min(51);
        let map = |off: i32| qpc(i32::from(qp_y).saturating_add(off).clamp(0, 51) as u8);
        QuantParams { qp_y, qp_cb: map(cb_offset), qp_cr: map(cr_offset) }
    }
}

/// One 1-D forward projection: dot `x` with a matrix row, then round-shift (encoder-side choice, see module docs).
#[inline]
fn fwd_dot(row: &[i8], x: &[i32], shift: u32) -> i32 {
    let mut s = 0i32;
    for k in 0..x.len() {
        s += i32::from(row[k]) * x[k];
    }
    (s + (1 << (shift - 1))) >> shift
}

/// Separable 2-D forward DCT: rows then columns. `s1 = log2N - 1`, `s2 = log2N + 6` (8-bit).
/// `TRANS_MATRIX` rows hold the coefficient basis, subsampled by `step = 32/N` per 8-317/8-318.
fn fwd_dct(block: &[i16], stride: usize, n: usize, out: &mut [i32]) {
    let l = n.trailing_zeros();
    let (s1, s2, step) = (l - 1, l + 6, 32 / n);
    let mut tmp = [0i32; 1024];
    for y in 0..n {
        let mut row = [0i32; 32];
        for k in 0..n {
            row[k] = i32::from(block[y * stride + k]);
        }
        for i in 0..n {
            tmp[y * n + i] = fwd_dot(&TRANS_MATRIX[i * step][..n], &row[..n], s1);
        }
    }
    for x in 0..n {
        let mut col = [0i32; 32];
        for k in 0..n {
            col[k] = tmp[k * n + x];
        }
        for i in 0..n {
            out[i * n + x] = fwd_dot(&TRANS_MATRIX[i * step][..n], &col[..n], s2);
        }
    }
}

/// Forward 4x4 DST-VII for luma intra (spec 8-315/8-316). Same shifts as the 4x4 DCT: DST row norms match DCT-4.
pub fn fwd_dst4(block: &[i16], stride: usize, out: &mut [i32; 16]) {
    let mut tmp = [0i32; 16];
    for y in 0..4 {
        let mut row = [0i32; 4];
        for k in 0..4 {
            row[k] = i32::from(block[y * stride + k]);
        }
        for i in 0..4 {
            tmp[y * 4 + i] = fwd_dot(&DST_MATRIX[i], &row, 1);
        }
    }
    for x in 0..4 {
        let col = [tmp[x], tmp[4 + x], tmp[8 + x], tmp[12 + x]];
        for i in 0..4 {
            out[i * 4 + x] = fwd_dot(&DST_MATRIX[i], &col, 8);
        }
    }
}

/// Forward transform dispatch for `n` in {4, 8, 16, 32}; `dst` selects the DST path (honoured only for `n == 4`).
/// Callers pass full buffers (the slice module always does); short buffers leave `out` untouched instead of panicking.
pub fn fwd_block(block: &[i16], stride: usize, n: usize, dst: bool, out: &mut [i32]) {
    match n {
        4 | 8 | 16 | 32 => {}
        _ => return,
    }
    let need_in = (n - 1).checked_mul(stride).and_then(|v| v.checked_add(n));
    match need_in {
        Some(need) if block.len() >= need && out.len() >= n * n => {}
        _ => return,
    }
    match n {
        4 if dst => {
            let mut o = [0i32; 16];
            fwd_dst4(block, stride, &mut o);
            out[..16].copy_from_slice(&o);
        }
        4 | 8 | 16 | 32 => fwd_dct(block, stride, n, out),
        // Unreachable after the size check above; kept for exhaustiveness.
        _ => {}
    }
}

/// First (vertical) inverse stage: `e = (M^T * d + 64) >> 7`, clipped (spec 8-314), like the decoder.
fn inv_first_stage(coeffs: &[i32], n: usize, step: usize, dst: bool, col: &mut [[i32; 32]; 32]) {
    for x in 0..n {
        let acc = &mut col[x];
        for j in 0..n {
            let d = coeffs[j * n + x];
            if d != 0 {
                // Zero coefficients contribute nothing, so skipping them matches the decoder bit-exactly.
                let basis: &[i8] = if dst { &DST_MATRIX[j][..n] } else { &TRANS_MATRIX[j * step][..n] };
                for (a, &m) in acc.iter_mut().zip(basis) {
                    *a += i32::from(m) * d;
                }
            }
        }
        for a in acc[..n].iter_mut() {
            *a = ((*a + 64) >> 7).clamp(-32768, 32767);
        }
    }
}

/// Second (horizontal) inverse stage: `r = (M^T * g + 2^11) >> 12` (spec 8-299, 8-295 with 8-bit depth).
fn inv_second_stage(n: usize, step: usize, dst: bool, col: &[[i32; 32]; 32], out: &mut [i32]) {
    for y in 0..n {
        for x in 0..n {
            let mut s = 1 << 11;
            for j in 0..n {
                let g = col[j][y];
                if g != 0 {
                    let m = if dst { DST_MATRIX[j][x] } else { TRANS_MATRIX[j * step][x] };
                    s += i32::from(m) * g;
                }
            }
            out[y * n + x] = s >> 12;
        }
    }
}

/// Inverse DCT/DST shared core: full block (equals the decoder's bounded version when outer coeffs are zero).
fn inv_core(coeffs: &[i32], n: usize, dst: bool, out: &mut [i32]) {
    let step = 32 / n;
    let mut col = [[0i32; 32]; 32];
    inv_first_stage(coeffs, n, step, dst, &mut col);
    inv_second_stage(n, step, dst, &col, out);
}

/// Inverse 4x4 transform; `dst` selects DST-VII (luma intra) else DCT. Output raster residuals (`i32`, unclipped).
pub fn inv4(coeffs: &[i32; 16], dst: bool, out: &mut [i32; 16]) {
    if dst {
        let mut col = [[0i32; 32]; 32];
        inv_first_stage(&coeffs[..], 4, 8, true, &mut col);
        inv_second_stage(4, 8, true, &col, &mut out[..]);
    } else {
        inv_core(&coeffs[..], 4, false, &mut out[..]);
    }
}

/// Inverse transform dispatch for `n` in {4, 8, 16, 32}; `dst` selects DST (honoured only for `n == 4`).
/// Short buffers leave `out` untouched instead of panicking, like [`fwd_block`].
pub fn inv_block(coeffs: &[i32], n: usize, dst: bool, out: &mut [i32]) {
    match n {
        4 | 8 | 16 | 32 => {}
        _ => return,
    }
    if coeffs.len() < n * n || out.len() < n * n {
        return;
    }
    match n {
        4 => {
            let mut c = [0i32; 16];
            c.copy_from_slice(&coeffs[..16]);
            let mut o = [0i32; 16];
            inv4(&c, dst, &mut o);
            out[..16].copy_from_slice(&o);
        }
        8 => inv_core(&coeffs[..64], 8, false, &mut out[..64]),
        16 => inv_core(&coeffs[..256], 16, false, &mut out[..256]),
        32 => inv_core(&coeffs[..1024], 32, false, &mut out[..1024]),
        // Unreachable after the size check above; kept for exhaustiveness.
        _ => {}
    }
}

/// Quantise `log2N` block in place into levels; returns the non-zero count. `unsigned_abs` avoids any `i32::MIN` hazard.
fn quant_run(c: &mut [i32], log2: u32, qp: u8, dz: Deadzone) -> u32 {
    let qp = qp.min(51);
    let qbits = 15 + u32::from(qp / 6) - log2;
    let f = ((1i64 << qbits) * dz.f256) >> 8;
    let mf = QUANT_MUL[(qp % 6) as usize];
    let mut nz = 0;
    for v in c.iter_mut() {
        let mag = ((*v as i64).unsigned_abs() * mf as u64 + f as u64) >> qbits;
        let a = mag as i32;
        *v = if *v < 0 { -a } else { a };
        nz += u32::from(a != 0);
    }
    nz
}

/// Dequantise levels in place, mirroring the decoder scaling exactly (spec 8-309, flat `m = 16`, 8-bit `bdShift`).
fn dequant_run(c: &mut [i32], log2: u32, qp: u8) {
    let qp = qp.min(51);
    let bd_shift = 3 + log2;
    let round = 1i64 << (bd_shift - 1);
    let scale = i64::from(LEVEL_SCALE[(qp % 6) as usize]) * (1i64 << (qp / 6));
    for v in c.iter_mut() {
        *v = (((i64::from(*v) * 16 * scale + round) >> bd_shift).clamp(-32768, 32767)) as i32;
    }
}

/// Quantise an `n`x`n` block (`n` in {4, 8, 16, 32}); short buffers or other sizes are a no-op returning 0.
pub fn quant_block(c: &mut [i32], n: usize, qp: u8, dz: Deadzone) -> u32 {
    let log2 = match n {
        4 => 2,
        8 => 3,
        16 => 4,
        32 => 5,
        _ => return 0,
    };
    if c.len() < n * n {
        return 0;
    }
    quant_run(&mut c[..n * n], log2, qp, dz)
}

/// Dequantise an `n`x`n` block (`n` in {4, 8, 16, 32}); short buffers or other sizes are a no-op.
pub fn dequant_block(c: &mut [i32], n: usize, qp: u8) {
    let log2 = match n {
        4 => 2,
        8 => 3,
        16 => 4,
        32 => 5,
        _ => return,
    };
    if c.len() < n * n {
        return;
    }
    dequant_run(&mut c[..n * n], log2, qp);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic xorshift64 (no `rand` crate); mirrors the style of the decoder's own tests.
    fn rng_next(st: &mut u64) -> u64 {
        let mut x = *st;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *st = x;
        x
    }

    /// Direct transcription of spec 8.6.4.1 steps 1–3 with 8.6.4.2 (8-317) for 4x4 DCT, out-of-place.
    fn ref_inv4_dct(c: &[i32; 16]) -> [i32; 16] {
        let mut e = [0i32; 16];
        for x in 0..4 {
            for y in 0..4 {
                let mut s = 0i32;
                for j in 0..4 {
                    s += i32::from(TRANS_MATRIX[j * 8][y]) * c[j * 4 + x];
                }
                e[y * 4 + x] = ((s + 64) >> 7).clamp(-32768, 32767);
            }
        }
        let mut r = [0i32; 16];
        for y in 0..4 {
            for x in 0..4 {
                let mut s = 0i32;
                for j in 0..4 {
                    s += i32::from(TRANS_MATRIX[j * 8][x]) * e[y * 4 + j];
                }
                r[y * 4 + x] = (s + 2048) >> 12;
            }
        }
        r
    }

    /// Direct transcription of spec 8.6.4.1 steps 1–3 with 8.6.4.2 (8-317) for 8x8 DCT, out-of-place.
    fn ref_inv8_dct(c: &[i32; 64]) -> [i32; 64] {
        let mut e = [0i32; 64];
        for x in 0..8 {
            for y in 0..8 {
                let mut s = 0i32;
                for j in 0..8 {
                    s += i32::from(TRANS_MATRIX[j * 4][y]) * c[j * 8 + x];
                }
                e[y * 8 + x] = ((s + 64) >> 7).clamp(-32768, 32767);
            }
        }
        let mut r = [0i32; 64];
        for y in 0..8 {
            for x in 0..8 {
                let mut s = 0i32;
                for j in 0..8 {
                    s += i32::from(TRANS_MATRIX[j * 4][x]) * e[y * 8 + j];
                }
                r[y * 8 + x] = (s + 2048) >> 12;
            }
        }
        r
    }

    #[test]
    fn fwd_quant_inv_roundtrip_small_error() {
        // Full chain at QP 4 (intra deadzone) on seeded ±32 residuals. Only quant rounding survives,
        // bounded by half a residual-domain step (~1 LSB); 3 LSB is generous headroom for every size.
        let mut st = 0x2545_f491_4f6c_dd1du64;
        for n in [4usize, 8, 16, 32] {
            let mut res = vec![0i16; n * n];
            for v in res.iter_mut() {
                *v = (rng_next(&mut st) % 65) as i16 - 32;
            }
            let mut c = vec![0i32; n * n];
            fwd_block(&res, n, n, false, &mut c);
            assert!(quant_block(&mut c, n, 4, Deadzone::INTRA) > 0, "n={n}: chain must keep coeffs");
            dequant_block(&mut c, n, 4);
            let mut rec = vec![0i32; n * n];
            inv_block(&c, n, false, &mut rec);
            let mut max = 0i32;
            for i in 0..n * n {
                max = max.max((rec[i] - i32::from(res[i])).abs());
            }
            assert!(max <= 3, "n={n}: max err {max}");
        }
    }

    #[test]
    fn fwd_honours_stride() {
        // Strided rows must read exactly the dense block (padding carries a sentinel that must not leak).
        let dense: [i16; 16] = [10, -20, 30, -40, 51, -61, 71, -81, 92, -102, 112, -122, 33, -44, 55, -66];
        let mut strided = [999i16; 24];
        for y in 0..4 {
            strided[y * 6..y * 6 + 4].copy_from_slice(&dense[y * 4..y * 4 + 4]);
        }
        let mut a = [0i32; 16];
        let mut b = [0i32; 16];
        fwd_block(&dense, 4, 4, false, &mut a);
        fwd_block(&strided, 6, 4, false, &mut b);
        assert_eq!(a, b);
    }

    #[test]
    fn dc_only_reconstructs_exactly() {
        // Flat-10 blocks at QP 0 (intra): every shift/rounding lands exactly, so DC-only is bit-exact.
        for n in [4usize, 8] {
            let res = vec![10i16; n * n];
            let mut c = vec![0i32; n * n];
            fwd_block(&res, n, n, false, &mut c);
            quant_block(&mut c, n, 0, Deadzone::INTRA);
            for v in c.iter_mut().skip(1) {
                *v = 0;
            }
            dequant_block(&mut c, n, 0);
            let mut rec = vec![0i32; n * n];
            inv_block(&c, n, false, &mut rec);
            assert!(rec.iter().all(|&v| v == 10), "n={n}: {rec:?}");
        }
    }

    #[test]
    fn quant_zero_sign_and_tolerance() {
        // Zero in gives zero out at every size and deadzone (the offset `f` is always below one step).
        for n in [4usize, 8, 16, 32] {
            for dz in [Deadzone::INTRA, Deadzone::INTER] {
                let mut c = vec![0i32; n * n];
                assert_eq!(quant_block(&mut c, n, 27, dz), 0);
                assert!(c.iter().all(|&v| v == 0));
            }
        }
        // Sign symmetry: negatives quantise to the negation (no `abs` hazards by construction).
        let mut pos = [5000i32; 16];
        let mut neg = [-5000i32; 16];
        assert_eq!(quant_block(&mut pos, 4, 22, Deadzone::INTER), quant_block(&mut neg, 4, 22, Deadzone::INTER));
        assert_eq!(pos, neg.map(|v| -v));
        // Dequant inverts quant within one residual-domain step `Qstep = 2*levelScale*2^(qp/6)/N`.
        let mut c = [5000i32; 16];
        quant_block(&mut c, 4, 22, Deadzone::INTER);
        dequant_block(&mut c, 4, 22);
        let qstep = 2 * LEVEL_SCALE[(22 % 6) as usize] * (1 << (22 / 6)) / 4;
        assert!((c[0] - 5000).abs() <= qstep, "{}", c[0]);
    }

    #[test]
    fn dst4_differs_from_dct_and_roundtrips() {
        let blk: [i16; 16] = [12, -7, 30, -22, 5, 18, -3, 9, -14, 26, -31, 4, 8, -11, 17, -6];
        let mut dct = [0i32; 16];
        let mut dst = [0i32; 16];
        fwd_block(&blk, 4, 4, false, &mut dct);
        fwd_block(&blk, 4, 4, true, &mut dst);
        assert_ne!(dct, dst);
        let mut c = dst;
        assert!(quant_block(&mut c, 4, 4, Deadzone::INTRA) > 0);
        dequant_block(&mut c, 4, 4);
        let mut rec = [0i32; 16];
        inv_block(&c, 4, true, &mut rec);
        let mut max = 0i32;
        for i in 0..16 {
            max = max.max((rec[i] - i32::from(blk[i])).abs());
        }
        assert!(max <= 3, "max err {max}");
    }

    #[test]
    fn inverse_matches_spec_reference_4x4_and_8x8() {
        // Fixed coefficients including clipping extremes; both paths implement 8-314/8-317 independently.
        let c4: [i32; 16] = [1000, -700, 250, -40, 600, -30000, 80, -9, 0, 12345, 0, -32768, 0, 0, 32767, -1];
        let mut r4 = [0i32; 16];
        inv_block(&c4, 4, false, &mut r4);
        assert_eq!(r4, ref_inv4_dct(&c4));
        let mut c8 = [0i32; 64];
        for i in 0..64 {
            c8[i] = ((i * 7919 + 13) % 60001) as i32 - 30000;
        }
        let mut r8 = [0i32; 64];
        inv_block(&c8, 8, false, &mut r8);
        assert_eq!(r8, ref_inv8_dct(&c8));
    }

    #[test]
    fn quant_params_derive_chroma_via_qpc() {
        let p = QuantParams::new(30, 0, 0);
        assert_eq!((p.qp_y, p.qp_cb, p.qp_cr), (30, 29, 29));
        let p = QuantParams::new(51, 0, 0);
        assert_eq!((p.qp_y, p.qp_cb, p.qp_cr), (51, 45, 45));
        // Combined offsets shift before the Table 8-10 mapping, then clamp to 0..=51.
        let p = QuantParams::new(28, 5, -60);
        assert_eq!((p.qp_cb, p.qp_cr), (qpc(33), qpc(0)));
        assert_eq!(QuantParams::new(60, 0, 0).qp_y, 51);
    }
}
