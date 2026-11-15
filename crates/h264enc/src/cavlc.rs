//! CAVLC residual coding (§9.2) and macroblock-layer syntax for Baseline streams.

use filmcraft_bitstream::BitWriter;

use crate::mbinfo::{MbKind, Part};
use crate::nal::SliceType;
use crate::syntax::{MbCode, NbCtx};
use crate::tables::{
    BLK4_XY, CBP_INTER_TO_CODE, CBP_INTRA_TO_CODE, COEFF_TOKEN_CDC_CODE, COEFF_TOKEN_CDC_LEN, COEFF_TOKEN_CODE, COEFF_TOKEN_LEN, RUN_BEFORE, TOTAL_ZEROS,
    TOTAL_ZEROS_CDC,
};

/// Largest level magnitude representable with level_prefix <= 15 in every suffixLength state.
pub const MAX_LEVEL: i32 = 2047;

/// Write one CAVLC residual block. `nc` = -1 for chroma DC. Returns TotalCoeff.
pub fn write_residual(w: &mut BitWriter, coeffs: &[i16], nc: i32) -> u8 {
    let max = coeffs.len();
    // levels from highest frequency to lowest
    let mut levels = [0i32; 16];
    let mut pos = [0usize; 16];
    let mut total = 0usize;
    for i in (0..max).rev() {
        if coeffs[i] != 0 {
            levels[total] = coeffs[i] as i32;
            pos[total] = i;
            total += 1;
        }
    }
    let mut t1 = 0usize;
    while t1 < total && t1 < 3 && levels[t1].abs() == 1 {
        t1 += 1;
    }
    // coeff_token
    if nc == -1 {
        w.write_bits(COEFF_TOKEN_CDC_CODE[total][t1] as u32, COEFF_TOKEN_CDC_LEN[total][t1] as u32);
    } else if nc >= 8 {
        if total == 0 {
            w.write_bits(3, 6);
        } else {
            w.write_bits((((total - 1) << 2) | t1) as u32, 6);
        }
    } else {
        let t = if nc < 2 {
            0
        } else if nc < 4 {
            1
        } else {
            2
        };
        w.write_bits(COEFF_TOKEN_CODE[t][total][t1] as u32, COEFF_TOKEN_LEN[t][total][t1] as u32);
    }
    if total == 0 {
        return 0;
    }
    let mut suffix_len: u32 = if total > 10 && t1 < 3 { 1 } else { 0 };
    for i in 0..total {
        let l = levels[i];
        if i < t1 {
            w.write_bit(l < 0);
            continue;
        }
        let mut code = if l > 0 { 2 * l - 2 } else { -2 * l - 1 };
        if i == t1 && t1 < 3 {
            code -= 2;
        }
        let code = code as u32;
        if suffix_len == 0 {
            if code < 14 {
                w.write_bits(1, code + 1);
            } else if code < 30 {
                w.write_bits(1, 15);
                w.write_bits(code - 14, 4);
            } else {
                write_escape(w, code - 30);
            }
        } else if code < (15 << suffix_len) {
            w.write_bits(1, (code >> suffix_len) + 1);
            w.write_bits(code & ((1 << suffix_len) - 1), suffix_len);
        } else {
            write_escape(w, code - (15 << suffix_len));
        }
        if suffix_len == 0 {
            suffix_len = 1;
        }
        if l.unsigned_abs() > (3 << (suffix_len - 1)) && suffix_len < 6 {
            suffix_len += 1;
        }
    }
    let total_zeros = pos[0] + 1 - total;
    if total < max {
        let (len, code) = if nc == -1 { TOTAL_ZEROS_CDC[total - 1][total_zeros] } else { TOTAL_ZEROS[total - 1][total_zeros] };
        w.write_bits(code as u32, len as u32);
    }
    let mut zeros_left = total_zeros;
    for i in 0..total - 1 {
        if zeros_left == 0 {
            break;
        }
        let run = pos[i] - pos[i + 1] - 1;
        let (len, code) = RUN_BEFORE[zeros_left.min(7) - 1][run];
        w.write_bits(code as u32, len as u32);
        zeros_left -= run;
    }
    total as u8
}

/// level_prefix >= 15 escape; `rem` = levelCode minus the prefix-15 base.
fn write_escape(w: &mut BitWriter, rem: u32) {
    if rem < 4096 {
        w.write_bits(1, 16);
        w.write_bits(rem, 12);
    } else {
        // level_prefix p >= 16 (High profiles): suffix of p-3 bits, value rem + 4096 - 2^(p-3)
        let mut p = 16;
        while rem + 4096 >= (1 << (p - 3)) * 2 {
            p += 1;
        }
        w.write_bits(0, p);
        w.write_bit(true);
        w.write_bits(rem + 4096 - (1 << (p - 3)), p - 3);
    }
}

fn nc_of(a: Option<u8>, b: Option<u8>) -> i32 {
    match (a, b) {
        (Some(a), Some(b)) => (a as i32 + b as i32 + 1) >> 1,
        (Some(a), None) => a as i32,
        (None, Some(b)) => b as i32,
        _ => 0,
    }
}

/// mb_type (and the P_8x8 sub_mb_types).
fn write_mb_type(w: &mut BitWriter, nb: &NbCtx, c: &MbCode) {
    let intra_base = match nb.slice_type {
        SliceType::I => 0,
        SliceType::P => 5,
        SliceType::B => 23,
    };
    match c.kind {
        MbKind::I4x4 | MbKind::I8x8 => w.write_ue(intra_base),
        MbKind::I16x16 => {
            let t = 1 + c.i16_mode as u32 + 4 * (c.cbp >> 4) as u32 + if c.cbp & 15 != 0 { 12 } else { 0 };
            w.write_ue(intra_base + t);
        }
        MbKind::PInter => w.write_ue(match c.part {
            Part::P16x16 => 0,
            Part::P16x8 => 1,
            Part::P8x16 => 2,
            Part::P8x8 => 3,
        }),
        MbKind::BDirect | MbKind::BInter => w.write_ue(c.b_type_number()),
        // Skip / none macroblocks are not written with write_mb.
        _ => debug_assert!(false, "write_mb on a skipped macroblock"),
    }
    if c.kind == MbKind::PInter && c.part == Part::P8x8 {
        for _ in 0..4 {
            w.write_ue(0);
        }
    }
}

/// prev_intra_NxN_pred_mode flags / rem_intra_NxN_pred_mode values.
fn write_intra_pred_modes(w: &mut BitWriter, c: &MbCode) {
    let n = if c.t8x8 { 4 } else { 16 };
    for i in 0..n {
        let p = c.ipred[i];
        if p < 0 {
            w.write_bit(true);
        } else {
            w.write_bit(false);
            w.write_bits(p as u32, 3);
        }
    }
}

/// coded_block_pattern.
fn write_cbp(w: &mut BitWriter, c: &MbCode) {
    let code = if c.kind.is_intra() { CBP_INTRA_TO_CODE[c.cbp as usize] } else { CBP_INTER_TO_CODE[c.cbp as usize] };
    let cbp_idx = (c.cbp & 15) | ((c.cbp >> 4) << 4);
    debug_assert_eq!(cbp_idx, c.cbp);
    w.write_ue(code as u32);
}

/// Luma residual blocks of the macroblock.
fn write_luma_residual(w: &mut BitWriter, nb: &NbCtx, c: &MbCode) {
    let luma_nc = |r: usize| nc_of(nb.luma_nnz(r, true), nb.luma_nnz(r, false));
    if c.kind == MbKind::I16x16 {
        write_residual(w, &c.luma_dc, luma_nc(0));
        if c.cbp & 15 != 0 {
            for blk in 0..16 {
                let (x, y) = BLK4_XY[blk];
                write_residual(w, &c.luma4[blk][1..16], luma_nc(y * 4 + x));
            }
        }
        return;
    }
    for b8 in 0..4 {
        if (c.cbp >> b8) & 1 == 0 {
            continue;
        }
        for i in 0..4 {
            let blk = b8 * 4 + i;
            let (x, y) = BLK4_XY[blk];
            if c.t8x8 {
                let mut sub = [0i16; 16];
                for k in 0..16 {
                    sub[k] = c.luma8[b8][4 * k + i];
                }
                write_residual(w, &sub, luma_nc(y * 4 + x));
            } else {
                write_residual(w, &c.luma4[blk], luma_nc(y * 4 + x));
            }
        }
    }
}

/// Chroma DC / AC residual blocks of the macroblock.
fn write_chroma_residual(w: &mut BitWriter, nb: &NbCtx, c: &MbCode) {
    let chroma = c.cbp >> 4;
    if chroma != 0 {
        for comp in 0..2 {
            write_residual(w, &c.cdc[comp], -1);
        }
    }
    if chroma == 2 {
        for comp in 0..2 {
            for b in 0..4 {
                let ncv = nc_of(nb.chroma_nnz(comp, b, true), nb.chroma_nnz(comp, b, false));
                write_residual(w, &c.cac[comp * 4 + b][1..16], ncv);
            }
        }
    }
}

/// Write the macroblock layer in CAVLC. For P/B slices the caller writes mb_skip_run first.
pub fn write_mb(w: &mut BitWriter, nb: &NbCtx, c: &MbCode) {
    write_mb_type(w, nb, c);
    if c.kind.is_inxn() {
        if nb.t8x8_mode {
            w.write_bit(c.t8x8);
        }
        write_intra_pred_modes(w, c);
    }
    if c.kind.is_intra() {
        w.write_ue(c.chroma_mode as u32);
    }
    if matches!(c.kind, MbKind::PInter | MbKind::BInter) {
        for list in 0..2 {
            for p in 0..c.part.count() {
                if c.uses_list(p, list) {
                    w.write_se(c.mvd[list][p].x as i32);
                    w.write_se(c.mvd[list][p].y as i32);
                }
            }
        }
    }
    if c.kind != MbKind::I16x16 {
        write_cbp(w, c);
        if c.cbp & 15 != 0 && nb.t8x8_mode && !c.kind.is_inxn() {
            w.write_bit(c.t8x8);
        }
    }
    if c.cbp == 0 && c.kind != MbKind::I16x16 {
        return;
    }
    w.write_se(c.qp_delta);
    write_luma_residual(w, nb, c);
    write_chroma_residual(w, nb, c);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_block_matches_spec_example() {
        // scan: 0,3,-1,0,0,-1,1,0,1 -> TotalCoeff 5, T1s 3.
        let coeffs: [i16; 16] = [0, 3, -1, 0, 0, -1, 1, 0, 1, 0, 0, 0, 0, 0, 0, 0];
        let mut w = BitWriter::new();
        let t = write_residual(&mut w, &coeffs, 0);
        assert_eq!(t, 5);
        // coeff_token 0000100 | signs 001 | level -1: 01 | level 3: 001 0 | total_zeros(4) 110 | runs 10 11 01 1
        let bits: String = w.finish().iter().map(|b| format!("{b:08b}")).collect();
        assert!(bits.starts_with("00001000010100101101011011"), "{bits}");
    }
}
