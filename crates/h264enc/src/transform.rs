//! Integer transforms, quantisation and the normative inverse transforms/scaling (§8.5.12).

use crate::tables::{DEQUANT4, DEQUANT8, QUANT4, pos_class4, pos_class8};

/// Forward 4x4 core transform of a residual block (raster order).
#[inline]
pub fn fdct4(d: &[i32; 16]) -> [i32; 16] {
    let mut t = [0i32; 16];
    for i in 0..4 {
        let (a, b, c, e) = (d[i * 4], d[i * 4 + 1], d[i * 4 + 2], d[i * 4 + 3]);
        let s03 = a + e;
        let d03 = a - e;
        let s12 = b + c;
        let d12 = b - c;
        t[i * 4] = s03 + s12;
        t[i * 4 + 1] = 2 * d03 + d12;
        t[i * 4 + 2] = s03 - s12;
        t[i * 4 + 3] = d03 - 2 * d12;
    }
    let mut o = [0i32; 16];
    for i in 0..4 {
        let (a, b, c, e) = (t[i], t[4 + i], t[8 + i], t[12 + i]);
        let s03 = a + e;
        let d03 = a - e;
        let s12 = b + c;
        let d12 = b - c;
        o[i] = s03 + s12;
        o[4 + i] = 2 * d03 + d12;
        o[8 + i] = s03 - s12;
        o[12 + i] = d03 - 2 * d12;
    }
    o
}

/// Normative inverse 4x4 transform of scaled coefficients `d` (raster), returns residual r (after the >>6).
#[inline]
pub fn idct4(d: &[i32; 16]) -> [i32; 16] {
    let mut f = [0i32; 16];
    for i in 0..4 {
        let (d0, d1, d2, d3) = (d[i * 4], d[i * 4 + 1], d[i * 4 + 2], d[i * 4 + 3]);
        let e = d0 + d2;
        let ff = d0 - d2;
        let g = (d1 >> 1) - d3;
        let h = d1 + (d3 >> 1);
        f[i * 4] = e + h;
        f[i * 4 + 1] = ff + g;
        f[i * 4 + 2] = ff - g;
        f[i * 4 + 3] = e - h;
    }
    let mut r = [0i32; 16];
    for j in 0..4 {
        let (f0, f1, f2, f3) = (f[j], f[4 + j], f[8 + j], f[12 + j]);
        let e = f0 + f2;
        let ff = f0 - f2;
        let g = (f1 >> 1) - f3;
        let h = f1 + (f3 >> 1);
        r[j] = (e + h + 32) >> 6;
        r[4 + j] = (ff + g + 32) >> 6;
        r[8 + j] = (ff - g + 32) >> 6;
        r[12 + j] = (e - h + 32) >> 6;
    }
    r
}

#[inline(always)]
fn fdct8_1d(s: [i32; 8]) -> [i32; 8] {
    let a0 = s[0] + s[7];
    let a1 = s[1] + s[6];
    let a2 = s[2] + s[5];
    let a3 = s[3] + s[4];
    let b0 = s[0] - s[7];
    let b1 = s[1] - s[6];
    let b2 = s[2] - s[5];
    let b3 = s[3] - s[4];
    let e0 = a0 + a3;
    let e1 = a1 + a2;
    let e2 = a0 - a3;
    let e3 = a1 - a2;
    [
        8 * (e0 + e1),
        12 * b0 + 10 * b1 + 6 * b2 + 3 * b3,
        8 * e2 + 4 * e3,
        10 * b0 - 3 * b1 - 12 * b2 - 6 * b3,
        8 * (e0 - e1),
        6 * b0 - 12 * b1 + 3 * b2 + 10 * b3,
        4 * e2 - 8 * e3,
        3 * b0 - 6 * b1 + 10 * b2 - 12 * b3,
    ]
}

/// Forward 8x8 transform X = A x A^T (unnormalised; rows of A are the H.264 8x8 basis scaled by 8).
pub fn fdct8(d: &[i32; 64]) -> [i32; 64] {
    let mut t = [0i32; 64];
    for (y, row) in d.as_chunks::<8>().0.iter().enumerate() {
        let o = fdct8_1d(*row);
        t[y * 8..y * 8 + 8].copy_from_slice(&o);
    }
    let mut out = [0i32; 64];
    for x in 0..8 {
        let col = [t[x], t[8 + x], t[16 + x], t[24 + x], t[32 + x], t[40 + x], t[48 + x], t[56 + x]];
        let o = fdct8_1d(col);
        for k in 0..8 {
            out[k * 8 + x] = o[k];
        }
    }
    out
}

#[cfg(test)]
const A8: [[i32; 8]; 8] = [
    [8, 8, 8, 8, 8, 8, 8, 8],
    [12, 10, 6, 3, -3, -6, -10, -12],
    [8, 4, -4, -8, -8, -4, 4, 8],
    [10, -3, -12, -6, 6, 12, 3, -10],
    [8, -8, -8, 8, 8, -8, -8, 8],
    [6, -12, 3, 10, -10, -3, 12, -6],
    [4, -8, 8, -4, -4, 8, -8, 4],
    [3, -6, 10, -12, 12, -10, 6, -3],
];

/// Normative inverse 8x8 transform (§8.5.13), returns residual after (+32)>>6.
pub fn idct8(d: &[i32; 64]) -> [i32; 64] {
    fn one(s: [i32; 8]) -> [i32; 8] {
        let a0 = s[0] + s[4];
        let a4 = s[0] - s[4];
        let a2 = (s[2] >> 1) - s[6];
        let a6 = s[2] + (s[6] >> 1);
        let b0 = a0 + a6;
        let b2 = a4 + a2;
        let b4 = a4 - a2;
        let b6 = a0 - a6;
        let a1 = -s[3] + s[5] - s[7] - (s[7] >> 1);
        let a3 = s[1] + s[7] - s[3] - (s[3] >> 1);
        let a5 = -s[1] + s[7] + s[5] + (s[5] >> 1);
        let a7 = s[3] + s[5] + s[1] + (s[1] >> 1);
        let b1 = a1 + (a7 >> 2);
        let b7 = a7 - (a1 >> 2);
        let b3 = a3 + (a5 >> 2);
        let b5 = (a3 >> 2) - a5;
        [b0 + b7, b2 + b5, b4 + b3, b6 + b1, b6 - b1, b4 - b3, b2 - b5, b0 - b7]
    }
    let mut g = [0i32; 64];
    for (i, row) in d.as_chunks::<8>().0.iter().enumerate() {
        let o = one(*row);
        g[i * 8..i * 8 + 8].copy_from_slice(&o);
    }
    let mut r = [0i32; 64];
    for j in 0..8 {
        let col = [g[j], g[8 + j], g[16 + j], g[24 + j], g[32 + j], g[40 + j], g[48 + j], g[56 + j]];
        let o = one(col);
        for i in 0..8 {
            r[i * 8 + j] = (o[i] + 32) >> 6;
        }
    }
    r
}

/// Per-QP quantiser tables.
pub struct QuantTables {
    /// mf4[qp%6][pos]
    pub mf4: [[u32; 16]; 6],
    pub v4: [[i32; 16]; 6],
    pub mf8: [[u32; 64]; 6],
    pub v8: [[i32; 64]; 6],
}

impl QuantTables {
    pub fn new() -> Self {
        let mut mf4 = [[0; 16]; 6];
        let mut v4 = [[0; 16]; 6];
        let mut mf8 = [[0; 64]; 6];
        let mut v8 = [[0; 64]; 6];
        let norm8 = [512.0, 578.0, 320.0, 578.0, 512.0, 578.0, 320.0, 578.0f64];
        for q in 0..6 {
            for i in 0..16 {
                mf4[q][i] = QUANT4[q][pos_class4(i)] as u32;
                v4[q][i] = DEQUANT4[q][pos_class4(i)];
            }
            for i in 0..64 {
                let v = DEQUANT8[q][pos_class8(i)];
                v8[q][i] = v;
                let (x, y) = (i & 7, i >> 3);
                mf8[q][i] = ((1u64 << 36) as f64 / (norm8[x] * norm8[y] * v as f64)).round() as u32;
            }
        }
        QuantTables { mf4, v4, mf8, v8 }
    }
}

impl Default for QuantTables {
    fn default() -> Self {
        Self::new()
    }
}

/// Deadzone rounding fractions (of 2^qbits), as numerator over 6*... we express as shift-based fractions.
#[derive(Clone, Copy)]
pub struct Deadzone {
    /// rounding offset as a fraction of one quantisation step, in 1/256 units.
    pub f256: i64,
}

impl Deadzone {
    pub const INTRA: Deadzone = Deadzone { f256: 85 }; // ~1/3
    pub const INTER: Deadzone = Deadzone { f256: 43 }; // ~1/6
}

/// Quantise a 4x4 block of transform coefficients (raster) in place into levels; returns number of non-zeros.
/// `skip_dc` leaves coefficient 0 untouched (for Intra16x16/chroma AC where DC is coded separately).
#[inline]
pub fn quant4(c: &mut [i32; 16], qt: &QuantTables, qp: u8, dz: Deadzone, skip_dc: bool) -> u32 {
    let qbits = 15 + (qp / 6) as u32;
    let f = (((1i64 << qbits) * dz.f256) >> 8) as u32;
    let mf = &qt.mf4[(qp % 6) as usize];
    let dc = c[0];
    let mut nz = 0;
    for (v, &m) in c.iter_mut().zip(mf) {
        // Residual coefficients stay below 2^14; the clamp keeps the u32 product from overflowing on any input.
        let a = ((v.unsigned_abs().min(0xffff) * m + f) >> qbits) as i32;
        nz += (a != 0) as u32;
        *v = if *v < 0 { -a } else { a };
    }
    if skip_dc {
        nz -= (c[0] != 0) as u32;
        c[0] = dc;
    }
    nz
}

/// Dequantise 4x4 levels (raster) for flat scaling matrices (§8.5.12.1). `skip_dc` leaves c[0] unchanged.
#[inline]
pub fn dequant4(c: &mut [i32; 16], qt: &QuantTables, qp: u8, skip_dc: bool) {
    let v = &qt.v4[(qp % 6) as usize];
    let sh = (qp / 6) as u32;
    let start = if skip_dc { 1 } else { 0 };
    for i in start..16 {
        // LevelScale = 16*v; qP>=24: (c*LS) << (qP/6-4); else (c*LS + 2^(3-qP/6)) >> (4-qP/6) == c*v << qP/6.
        c[i] = (c[i] * v[i]) << sh;
    }
}

/// Quantise an 8x8 block (raster), returns non-zero count.
pub fn quant8(c: &mut [i32; 64], qt: &QuantTables, qp: u8, dz: Deadzone) -> u32 {
    let qbits = 22 + (qp / 6) as u32;
    let f = (((1i64 << qbits) * dz.f256) >> 8) as u32;
    let mf = &qt.mf8[(qp % 6) as usize];
    let mut nz = 0;
    for (v, &m) in c.iter_mut().zip(mf) {
        // |v| * m needs up to 36 bits. Splitting |v| at bit 8 keeps both partial products in u32, and
        // (256 * hi + lo) >> qbits == (hi + (lo >> 8)) >> (qbits - 8) keeps the result exact.
        // Residual coefficients stay below 2^21; the clamp keeps the sums from overflowing on any input.
        let x = v.unsigned_abs().min(0xff_ffff);
        let a = (((x >> 8) * m + (((x & 0xff) * m + f) >> 8)) >> (qbits - 8)) as i32;
        nz += (a != 0) as u32;
        *v = if *v < 0 { -a } else { a };
    }
    nz
}

/// Dequantise 8x8 levels (raster), flat scaling.
pub fn dequant8(c: &mut [i32; 64], qt: &QuantTables, qp: u8) {
    let v = &qt.v8[(qp % 6) as usize];
    let q6 = (qp / 6) as i32;
    for i in 0..64 {
        let ls = 16 * v[i];
        c[i] = if q6 >= 6 { (c[i] * ls) << (q6 - 6) } else { (c[i] * ls + (1 << (5 - q6))) >> (6 - q6) };
    }
}

/// 4x4 Hadamard used for the Intra16x16 luma DC (unnormalised).
#[inline]
pub fn hadamard4(d: &[i32; 16]) -> [i32; 16] {
    let mut t = [0i32; 16];
    for i in 0..4 {
        let (a, b, c, e) = (d[i * 4], d[i * 4 + 1], d[i * 4 + 2], d[i * 4 + 3]);
        t[i * 4] = a + b + c + e;
        t[i * 4 + 1] = a + b - c - e;
        t[i * 4 + 2] = a - b - c + e;
        t[i * 4 + 3] = a - b + c - e;
    }
    let mut o = [0i32; 16];
    for i in 0..4 {
        let (a, b, c, e) = (t[i], t[4 + i], t[8 + i], t[12 + i]);
        o[i] = a + b + c + e;
        o[4 + i] = a + b - c - e;
        o[8 + i] = a - b - c + e;
        o[12 + i] = a - b + c - e;
    }
    o
}

/// Forward quantisation of the Intra16x16 DC (input: raster 4x4 of block DC coefficients).
pub fn quant_luma_dc(dc: &[i32; 16], qt: &QuantTables, qp: u8, dz: Deadzone) -> ([i32; 16], u32) {
    let h = hadamard4(dc);
    let qbits = 16 + (qp / 6) as u32;
    let f = ((1i64 << qbits) * dz.f256) >> 8;
    let mf = qt.mf4[(qp % 6) as usize][0] as i64;
    let mut out = [0i32; 16];
    let mut nz = 0;
    for i in 0..16 {
        let v = h[i] >> 1;
        let a = ((v.unsigned_abs() as i64 * mf + f) >> qbits) as i32;
        out[i] = if v < 0 { -a } else { a };
        nz += (a != 0) as u32;
    }
    (out, nz)
}

/// Normative Intra16x16 DC inverse (§8.5.10): levels (raster 4x4) -> dcY (raster 4x4).
pub fn dequant_luma_dc(c: &[i32; 16], qt: &QuantTables, qp: u8) -> [i32; 16] {
    let f = hadamard4(c);
    let ls = 16 * qt.v4[(qp % 6) as usize][0];
    let q6 = (qp / 6) as i32;
    let mut o = [0i32; 16];
    for i in 0..16 {
        o[i] = if q6 >= 6 { (f[i] * ls) << (q6 - 6) } else { (f[i] * ls + (1 << (5 - q6))) >> (6 - q6) };
    }
    o
}

/// Forward quantisation of chroma DC (2x2 raster of block DC coefficients).
pub fn quant_chroma_dc(dc: &[i32; 4], qt: &QuantTables, qp: u8, dz: Deadzone) -> ([i32; 4], u32) {
    let h = [dc[0] + dc[1] + dc[2] + dc[3], dc[0] - dc[1] + dc[2] - dc[3], dc[0] + dc[1] - dc[2] - dc[3], dc[0] - dc[1] - dc[2] + dc[3]];
    let qbits = 16 + (qp / 6) as u32;
    let f = ((1i64 << qbits) * dz.f256) >> 8;
    let mf = qt.mf4[(qp % 6) as usize][0] as i64;
    let mut out = [0i32; 4];
    let mut nz = 0;
    for i in 0..4 {
        let v = h[i];
        let a = ((v.unsigned_abs() as i64 * mf + f) >> qbits) as i32;
        out[i] = if v < 0 { -a } else { a };
        nz += (a != 0) as u32;
    }
    (out, nz)
}

/// Normative chroma DC inverse (§8.5.11.2) for 4:2:0: levels (raster 2x2) -> dcC.
pub fn dequant_chroma_dc(c: &[i32; 4], qt: &QuantTables, qpc: u8) -> [i32; 4] {
    let f = [c[0] + c[1] + c[2] + c[3], c[0] - c[1] + c[2] - c[3], c[0] + c[1] - c[2] - c[3], c[0] - c[1] - c[2] + c[3]];
    let ls = 16 * qt.v4[(qpc % 6) as usize][0];
    let q6 = (qpc / 6) as u32;
    let mut o = [0i32; 4];
    for i in 0..4 {
        o[i] = ((f[i] * ls) << q6) >> 5;
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quant4_i64(c: &mut [i32; 16], qp: u8, dz: Deadzone, skip_dc: bool) -> u32 {
        let qbits = 15 + (qp / 6) as u32;
        let f = ((1i64 << qbits) * dz.f256) >> 8;
        let mut nz = 0;
        for i in (skip_dc as usize)..16 {
            let v = c[i];
            let a = ((v.unsigned_abs() as i64 * QUANT4[(qp % 6) as usize][pos_class4(i)] as i64 + f) >> qbits) as i32;
            c[i] = if v < 0 { -a } else { a };
            nz += (a != 0) as u32;
        }
        nz
    }

    fn mf8_i64() -> [[i64; 64]; 6] {
        let norm8 = [512.0, 578.0, 320.0, 578.0, 512.0, 578.0, 320.0, 578.0f64];
        std::array::from_fn(|q| {
            std::array::from_fn(|i| {
                let v = DEQUANT8[q][pos_class8(i)] as f64;
                ((1u64 << 36) as f64 / (norm8[i & 7] * norm8[i >> 3] * v)).round() as i64
            })
        })
    }

    fn quant8_i64(c: &mut [i32; 64], mf8: &[[i64; 64]; 6], qp: u8, dz: Deadzone) -> u32 {
        let qbits = 22 + (qp / 6) as u32;
        let f = ((1i64 << qbits) * dz.f256) >> 8;
        let mut nz = 0;
        for i in 0..64 {
            let v = c[i];
            let a = ((v.unsigned_abs() as i64 * mf8[(qp % 6) as usize][i] + f) >> qbits) as i32;
            c[i] = if v < 0 { -a } else { a };
            nz += (a != 0) as u32;
        }
        nz
    }

    const DEADZONES: [Deadzone; 3] = [Deadzone::INTRA, Deadzone::INTER, Deadzone { f256: 128 }];

    #[test]
    fn quant4_matches_i64() {
        let qt = QuantTables::new();
        for qp in 0..52u8 {
            for dz in DEADZONES {
                // Every magnitude below 2^16 (4x4 residual coefficients stay below 2^14) at every position.
                for base in (0..1 << 16).step_by(16) {
                    for rot in [0, 5, 10] {
                        let mut c: [i32; 16] = std::array::from_fn(|i| {
                            let v = base + (i + rot) as i32 % 16;
                            if (i ^ (base as usize / 16)) & 1 == 0 { v } else { -v }
                        });
                        let mut r = c;
                        let skip_dc = base & 16 != 0;
                        let nz = quant4(&mut c, &qt, qp, dz, skip_dc);
                        assert_eq!(nz, quant4_i64(&mut r, qp, dz, skip_dc), "qp {qp} base {base}");
                        assert_eq!(c, r, "qp {qp} base {base}");
                    }
                }
            }
        }
    }

    #[test]
    fn quant8_matches_i64() {
        let qt = QuantTables::new();
        let mf8 = mf8_i64();
        assert!(qt.mf8.iter().flatten().all(|&m| m <= 0xffff));
        let mut seed = 1u32;
        let mut rnd = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            seed
        };
        for qp in 0..52u8 {
            for dz in DEADZONES {
                // Every magnitude below 2^21 (8x8 residual coefficients stay below 2^20), rotated over positions.
                for (n, base) in (0..1 << 21).step_by(64).enumerate() {
                    let mut c: [i32; 64] = std::array::from_fn(|i| {
                        let v = base + ((i + n * 7) % 64) as i32;
                        if (i + n) & 1 == 0 { v } else { -v }
                    });
                    let mut r = c;
                    assert_eq!(quant8(&mut c, &qt, qp, dz), quant8_i64(&mut r, &mf8, qp, dz), "qp {qp} base {base}");
                    assert_eq!(c, r, "qp {qp} base {base}");
                }
                for _ in 0..2000 {
                    let mut c: [i32; 64] = std::array::from_fn(|_| (rnd() >> 10) as i32 - (1 << 21));
                    let mut r = c;
                    assert_eq!(quant8(&mut c, &qt, qp, dz), quant8_i64(&mut r, &mf8, qp, dz), "qp {qp}");
                    assert_eq!(c, r, "qp {qp}");
                }
            }
        }
    }

    #[test]
    fn roundtrip_4x4_low_qp() {
        let qt = QuantTables::new();
        let mut d = [0i32; 16];
        for (i, v) in d.iter_mut().enumerate() {
            *v = ((i * 37) % 51) as i32 - 25;
        }
        let mut c = fdct4(&d);
        quant4(&mut c, &qt, 4, Deadzone { f256: 128 }, false);
        dequant4(&mut c, &qt, 4, false);
        let r = idct4(&c);
        for i in 0..16 {
            assert!((r[i] - d[i]).abs() <= 1, "{i}: {} vs {}", r[i], d[i]);
        }
    }

    #[test]
    fn roundtrip_8x8_low_qp() {
        let qt = QuantTables::new();
        let mut d = [0i32; 64];
        for (i, v) in d.iter_mut().enumerate() {
            *v = ((i * 53) % 71) as i32 - 35;
        }
        let mut c = fdct8(&d);
        quant8(&mut c, &qt, 4, Deadzone { f256: 128 });
        dequant8(&mut c, &qt, 4);
        let r = idct8(&c);
        for i in 0..64 {
            assert!((r[i] - d[i]).abs() <= 1, "{i}: {} vs {}", r[i], d[i]);
        }
    }

    #[test]
    fn fdct8_matches_matrix() {
        let mut d = [0i32; 64];
        for (i, v) in d.iter_mut().enumerate() {
            *v = ((i * 97) % 211) as i32 - 105;
        }
        let f = fdct8(&d);
        for k in 0..8 {
            for l in 0..8 {
                let mut s = 0;
                for y in 0..8 {
                    for x in 0..8 {
                        s += A8[k][y] * d[y * 8 + x] * A8[l][x];
                    }
                }
                assert_eq!(f[k * 8 + l], s);
            }
        }
    }

    #[test]
    fn luma_dc_roundtrip() {
        let qt = QuantTables::new();
        // DC of a flat block of value 10 is 160 per 4x4 block.
        let dc = [160i32; 16];
        let (lv, _) = quant_luma_dc(&dc, &qt, 10, Deadzone { f256: 128 });
        let o = dequant_luma_dc(&lv, &qt, 10);
        let mut blk = [0i32; 16];
        blk[0] = o[0];
        let r = idct4(&blk);
        assert!(r.iter().all(|&v| (v - 10).abs() <= 1), "{r:?}");
    }

    #[test]
    fn chroma_dc_roundtrip() {
        let qt = QuantTables::new();
        let dc = [16 * 7, 16 * 7, 16 * 7, 16 * 7];
        let (lv, _) = quant_chroma_dc(&dc, &qt, 10, Deadzone { f256: 128 });
        let o = dequant_chroma_dc(&lv, &qt, 10);
        let mut blk = [0i32; 16];
        blk[0] = o[0];
        let r = idct4(&blk);
        assert!(r.iter().all(|&v| (v - 7).abs() <= 1), "{r:?}");
    }
}
