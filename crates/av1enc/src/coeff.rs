//! Clean-room transform coefficient entropy encoding for AV1 tiles (spec 5.11.37 / 7.13.3).

/// 4x4 zigzag scan order (spec Table 7.13.1).
pub const SCAN_4X4: [usize; 16] = [0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15];

/// Inverse scan mapping: gives the scan index (0..15) for a raster position (0..15).
pub const INV_SCAN_4X4: [usize; 16] = [0, 1, 5, 6, 2, 4, 7, 12, 3, 8, 11, 13, 9, 10, 14, 15];

/// End-of-block position mapping for 4x4 blocks (multisize 0, eob in 1..=16).
/// Returns (eob_pt_sym, eob_extra_opt, literal_bits).
#[inline(always)]
pub fn map_eob_to_syntax(eob: usize) -> (usize, Option<usize>, u32, usize) {
    debug_assert!((1..=16).contains(&eob));
    match eob {
        1 => (0, None, 0, 0),
        2 => (1, None, 0, 0),
        3 => (2, Some(0), 0, 0),
        4 => (2, Some(1), 0, 0),
        5 => (3, Some(0), 0, 1),
        6 => (3, Some(0), 1, 1),
        7 => (3, Some(1), 0, 1),
        8 => (3, Some(1), 1, 1),
        9 => (4, Some(0), 0, 2),
        10 => (4, Some(0), 1, 2),
        11 => (4, Some(0), 2, 2),
        12 => (4, Some(0), 3, 2),
        13 => (4, Some(1), 0, 2),
        14 => (4, Some(1), 1, 2),
        15 => (4, Some(1), 2, 2),
        _ => (4, Some(1), 3, 2), // 16
    }
}

/// Find the End-of-Block (EOB) index in scan order for a 4x4 quantized block.
/// Returns 0 if all coefficients are zero.
#[inline(always)]
pub fn find_eob_4x4(quant: &[i16; 16]) -> usize {
    for i in (0..16).rev() {
        if quant[SCAN_4X4[i]] != 0 {
            return i + 1;
        }
    }
    0
}

use crate::symbol::SymbolEncoder;
use crate::tables;

/// Convert qindex (0..=255) to AV1 spec CDF table tier (0..=3).
#[inline(always)]
pub fn qindex_to_tier(qindex: u8) -> usize {
    match qindex {
        0..=20 => 0,
        21..=60 => 1,
        61..=120 => 2,
        _ => 3,
    }
}

/// Default CDF for transform block skip (`txb_skip`, Section 9.3).
pub static DEFAULT_TXB_SKIP_CDF: [u16; 3] = [31849, 32768, 0];

/// Default CDF for end-of-block pointer for 4x4 blocks (multisize 0).
pub static DEFAULT_EOB_PT_16_CDF: [u16; 6] = [840, 1039, 1980, 4895, 32768, 0];

/// Default CDF for eob extra bits.
pub static DEFAULT_EOB_EXTRA_CDF: [u16; 3] = [16384, 32768, 0];

/// Write a transform block skip flag (`txb_skip`) using qindex tier, transform size context, and skip context.
pub fn write_txb_skip(sym: &mut SymbolEncoder, qindex: u8, tx_sz_ctx: usize, ctx: usize, all_zero: bool) {
    let tier = qindex_to_tier(qindex).min(3);
    let s_ctx = tx_sz_ctx.min(4);
    let c_ctx = ctx.min(12);
    if let Some(cdf) = tables::DEFAULT_TXB_SKIP_CDF.get(tier).and_then(|t| t.get(s_ctx)).and_then(|s| s.get(c_ctx)) {
        sym.write_symbol_no_update(if all_zero { 1 } else { 0 }, cdf);
    }
}

/// Write EOB syntax for a 4x4 transform block into the symbol stream.
pub fn write_eob_4x4(sym: &mut SymbolEncoder, eob: usize) {
    debug_assert!((1..=16).contains(&eob));
    let (pt_sym, extra_opt, lit_val, lit_bits) = map_eob_to_syntax(eob);
    sym.write_symbol_no_update(pt_sym, &DEFAULT_EOB_PT_16_CDF);
    if let Some(extra_sym) = extra_opt {
        sym.write_symbol_no_update(extra_sym, &DEFAULT_EOB_EXTRA_CDF);
    }
    for i in (0..lit_bits).rev() {
        let bit = ((lit_val >> i) & 1) == 1;
        sym.write_bool(bit);
    }
}

/// Encode a single DC residual for a transform block (e.g. 64x64, 32x32, 16x16, 8x8, or 4x4).
/// When `plane == 0`, `tx_sz_ctx <= 2`, and `qindex > 0`, `y_mode` is used to signal `DCT_DCT`
/// from `DEFAULT_INTRA_TX_TYPE_SET2_CDF[tx_sz_ctx][y_mode]`.
#[allow(clippy::too_many_arguments)]
pub fn encode_dc_residual(
    sym: &mut SymbolEncoder,
    qindex: u8,
    plane: usize,
    tx_sz_ctx: usize,
    ctx: usize,
    dc_sign_ctx: usize,
    level: u32,
    is_negative: bool,
    y_mode: u8,
) {
    if level == 0 {
        write_txb_skip(sym, qindex, tx_sz_ctx, ctx, true);
        return;
    }

    let tier = qindex_to_tier(qindex).min(3);
    let s_ctx = tx_sz_ctx.min(4);

    // 1. txb_skip = 0 (block is not skipped)
    write_txb_skip(sym, qindex, tx_sz_ctx, ctx, false);

    // 1b. transform_type (spec 5.11.39): for intra luma blocks with tx_sz <= 16x16 (s_ctx <= 2)
    // and reduced_tx_set == 1, get_tx_set returns TX_SET_INTRA_2 (2).
    // In TX_TYPE_INTRA_INV_SET2 = [9, 0, 3, 1, 2], DCT_DCT (0) is symbol 1.
    if plane == 0
        && s_ctx <= 2
        && qindex > 0
        && let Some(tx_cdf) = tables::DEFAULT_INTRA_TX_TYPE_SET2_CDF.get(s_ctx).and_then(|s| s.get(y_mode as usize))
    {
        sym.write_symbol_no_update(1, tx_cdf);
    }

    // 2. eob = 1: eob_pt symbol 0 (no extra bits)
    let eob_cdf: Option<&[u16]> = if plane == 0 {
        match s_ctx {
            4 | 3 => tables::DEFAULT_EOB_PT_1024_CDF_BY_TIER.get(tier).map(|a| a.as_slice()),
            2 => tables::DEFAULT_EOB_PT_256_CDF_BY_TIER.get(tier).map(|a| a.as_slice()),
            1 => tables::DEFAULT_EOB_PT_64_CDF_BY_TIER.get(tier).map(|a| a.as_slice()),
            _ => tables::DEFAULT_EOB_PT_16_CDF_BY_TIER.get(tier).map(|a| a.as_slice()),
        }
    } else {
        match s_ctx {
            4 | 3 => tables::DEFAULT_EOB_PT_1024_CHROMA_CDF_BY_TIER.get(tier).map(|a| a.as_slice()),
            2 => tables::DEFAULT_EOB_PT_256_CHROMA_CDF_BY_TIER.get(tier).map(|a| a.as_slice()),
            1 => tables::DEFAULT_EOB_PT_64_CHROMA_CDF_BY_TIER.get(tier).map(|a| a.as_slice()),
            _ => tables::DEFAULT_EOB_PT_16_CHROMA_CDF_BY_TIER.get(tier).map(|a| a.as_slice()),
        }
    };
    if let Some(cdf) = eob_cdf {
        sym.write_symbol_no_update(0, cdf);
    }

    // 3. coeff_base_eob: symbol = (level - 1).min(2) (NUM_BASE_LEVELS = 2)
    let base_sym = ((level - 1).min(2)) as usize;
    let base_cdf: Option<&[u16]> = if plane == 0 {
        tables::DEFAULT_COEFF_BASE_EOB_CDF_BY_TIER.get(tier).and_then(|t| t.get(s_ctx)).map(|a| a.as_slice())
    } else {
        tables::DEFAULT_COEFF_BASE_EOB_CHROMA_CDF_BY_TIER.get(tier).and_then(|t| t.get(s_ctx)).map(|a| a.as_slice())
    };
    if let Some(base_cdf) = base_cdf {
        sym.write_symbol_no_update(base_sym, base_cdf);
    }

    // 4. If level > 2 (i.e. > NUM_BASE_LEVELS), write extra br-levels
    if level > 2 {
        let mut rem = level - 3;
        let br_cdf = if plane == 0 {
            tables::DEFAULT_COEFF_BR_DC_CDF_BY_TIER.get(tier).and_then(|t| t.get(s_ctx.min(3)))
        } else {
            tables::DEFAULT_COEFF_BR_DC_CHROMA_CDF_BY_TIER.get(tier).and_then(|t| t.get(s_ctx.min(3)))
        };
        if let Some(br_cdf) = br_cdf {
            for _ in 0..(tables::COEFF_BASE_RANGE / (tables::BR_CDF_SIZE - 1)) {
                let br = rem.min(3);
                sym.write_symbol_no_update(br as usize, br_cdf);
                rem = rem.saturating_sub(br);
                if br < 3 {
                    break;
                }
            }
        }
    }

    // 5. dc_sign: symbol 1 if negative, 0 if positive
    let ptype = (plane > 0) as usize;
    let sign_cdf = &tables::DEFAULT_DC_SIGN_CDF_ALL[ptype][dc_sign_ctx.min(2)];
    sym.write_symbol_no_update(if is_negative { 1 } else { 0 }, sign_cdf);

    // 6. Exp-Golomb escape if level > NUM_BASE_LEVELS + COEFF_BASE_RANGE (14)
    if level > tables::COEFF_BASE_RANGE + 2 {
        let x = level - (tables::COEFF_BASE_RANGE + 2);
        let length = 32 - x.leading_zeros();
        for _ in 0..(length - 1) {
            sym.write_literal(0, 1);
        }
        sym.write_literal(1, 1);
        for i in (0..length - 1).rev() {
            let bit = (x >> i) & 1;
            sym.write_literal(bit, 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_eob_and_scan_symmetry() {
        let mut block = [0i16; 16];
        assert_eq!(find_eob_4x4(&block), 0);

        block[0] = 5; // DC coeff is scan pos 0
        assert_eq!(find_eob_4x4(&block), 1);

        block[15] = -2; // Last coeff in scan order is raster 15
        assert_eq!(find_eob_4x4(&block), 16);

        // Verify scan inversion
        for i in 0..16 {
            assert_eq!(INV_SCAN_4X4[SCAN_4X4[i]], i);
            assert_eq!(SCAN_4X4[INV_SCAN_4X4[i]], i);
        }
    }

    #[test]
    fn test_eob_syntax_mapping() {
        for eob in 1..=16 {
            let (pt, extra, lit_val, lit_bits) = map_eob_to_syntax(eob);
            let eob_pt = pt + 1;
            let mut decoded_eob = if eob_pt < 2 { eob_pt } else { (1 << (eob_pt - 2)) + 1 };
            let eob_shift = eob_pt as i32 - 3;
            if eob_shift >= 0 {
                if extra == Some(1) {
                    decoded_eob += 1 << eob_shift;
                }
                if lit_bits > 0 {
                    decoded_eob += (lit_val as usize) & ((1 << lit_bits) - 1);
                }
            }
            assert_eq!(decoded_eob, eob, "failed for eob {eob}");
        }
    }

    #[test]
    fn test_write_txb_skip_and_eob() {
        let mut sym = SymbolEncoder::new();
        write_txb_skip(&mut sym, 20, 0, 0, false);
        write_eob_4x4(&mut sym, 4);
        let bytes = sym.finish();
        assert!(!bytes.is_empty());
    }

    #[test]
    fn test_encode_dc_residual() {
        let mut sym = SymbolEncoder::new();
        encode_dc_residual(&mut sym, 100, 0, 4, 0, 0, 3, false, 0);
        encode_dc_residual(&mut sym, 100, 0, 4, 0, 1, 10, true, 0);
        encode_dc_residual(&mut sym, 100, 1, 3, 7, 0, 2, false, 0); // Chroma test
        let bytes = sym.finish();
        assert!(!bytes.is_empty());
    }
}
