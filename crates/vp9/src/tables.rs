//! Constants, small lookup tables and decode trees of the VP9 specification (sections 3, 6, 9.3
//! and 10.2). The large tables are extracted from the specification text into `spec_tables`.

pub use crate::spec_tables::*;

// Block sizes (7.4.3).
pub const BLOCK_8X8: u8 = 3;
pub const BLOCK_16X16: u8 = 6;
pub const BLOCK_64X64: u8 = 12;
pub const BLOCK_INVALID: u8 = 14;

// Transform sizes / modes.
pub const TX_4X4: u8 = 0;
pub const TX_8X8: u8 = 1;
pub const TX_16X16: u8 = 2;
pub const TX_32X32: u8 = 3;
pub const TX_MODE_SELECT: u8 = 4;

// Prediction modes (7.4.5, 7.4.11).
pub const DC_PRED: u8 = 0;
pub const V_PRED: u8 = 1;
pub const H_PRED: u8 = 2;
pub const D45_PRED: u8 = 3;
pub const D135_PRED: u8 = 4;
pub const D117_PRED: u8 = 5;
pub const D153_PRED: u8 = 6;
pub const D207_PRED: u8 = 7;
pub const D63_PRED: u8 = 8;
pub const TM_PRED: u8 = 9;
pub const NEARESTMV: u8 = 10;
pub const NEARMV: u8 = 11;
pub const ZEROMV: u8 = 12;
pub const NEWMV: u8 = 13;

// Reference frames.
pub const NONE: i8 = -1;
pub const INTRA_FRAME: i8 = 0;
pub const LAST_FRAME: i8 = 1;
pub const GOLDEN_FRAME: i8 = 2;
pub const ALTREF_FRAME: i8 = 3;

// Transform types.
pub const DCT_DCT: u8 = 0;
pub const ADST_DCT: u8 = 1;
pub const DCT_ADST: u8 = 2;
pub const ADST_ADST: u8 = 3;

// Interpolation filters.
pub const SWITCHABLE: u8 = 4;

// Segmentation features.
pub const SEG_LVL_ALT_Q: usize = 0;
pub const SEG_LVL_ALT_L: usize = 1;
pub const SEG_LVL_REF_FRAME: usize = 2;
pub const SEG_LVL_SKIP: usize = 3;
pub const MAX_SEGMENTS: usize = 8;

pub const MAX_LOOP_FILTER: i32 = 63;
pub const MIN_TILE_WIDTH_B64: u32 = 4;
pub const MAX_TILE_WIDTH_B64: u32 = 64;
pub const COMPANDED_MVREF_THRESH: i32 = 8;
pub const MV_BORDER: i32 = 128;
pub const INTERP_EXTEND: i32 = 4;
pub const BORDERINPIXELS: i32 = 160;
pub const COUNT_SAT: u32 = 20;
pub const MAX_UPDATE_FACTOR: u32 = 128;

pub const B_WIDTH_LOG2: [u8; 13] = [0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 3, 4, 4];
pub const B_HEIGHT_LOG2: [u8; 13] = [0, 1, 0, 1, 2, 1, 2, 3, 2, 3, 4, 3, 4];
pub const NUM_4X4_WIDE: [u8; 13] = [1, 1, 2, 2, 2, 4, 4, 4, 8, 8, 8, 16, 16];
pub const NUM_4X4_HIGH: [u8; 13] = [1, 2, 1, 2, 4, 2, 4, 8, 4, 8, 16, 8, 16];
pub const MI_WIDTH_LOG2: [u8; 13] = [0, 0, 0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 3];
pub const NUM_8X8_WIDE: [u8; 13] = [1, 1, 1, 1, 1, 2, 2, 2, 4, 4, 4, 8, 8];
pub const NUM_8X8_HIGH: [u8; 13] = [1, 1, 1, 1, 2, 1, 2, 4, 2, 4, 8, 4, 8];
pub const SIZE_GROUP: [u8; 13] = [0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 3, 3];
pub const MAX_TXSIZE: [u8; 13] = [0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 3, 3];
pub const TX_MODE_TO_BIGGEST_TX_SIZE: [u8; 5] = [0, 1, 2, 3, 3];

/// subsize_lookup[partition][bsize] (10.2).
pub const SUBSIZE_LOOKUP: [[u8; 13]; 4] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
    [14, 14, 14, 2, 14, 14, 5, 14, 14, 8, 14, 14, 11],
    [14, 14, 14, 1, 14, 14, 4, 14, 14, 7, 14, 14, 10],
    [14, 14, 14, 0, 14, 14, 3, 14, 14, 6, 14, 14, 9],
];

/// ss_size_lookup[bsize][subsampling_x][subsampling_y] (6.4.23).
pub const SS_SIZE_LOOKUP: [[[u8; 2]; 2]; 13] = [
    [[0, 14], [14, 14]],
    [[1, 0], [14, 14]],
    [[2, 14], [0, 14]],
    [[3, 2], [1, 0]],
    [[4, 3], [14, 1]],
    [[5, 14], [3, 2]],
    [[6, 5], [4, 3]],
    [[7, 6], [14, 4]],
    [[8, 14], [6, 5]],
    [[9, 8], [7, 6]],
    [[10, 9], [14, 7]],
    [[11, 14], [9, 8]],
    [[12, 11], [10, 9]],
];

/// mode2txfm_map (10.2).
pub const MODE2TXFM: [u8; 14] =
    [DCT_DCT, ADST_DCT, DCT_ADST, DCT_DCT, ADST_ADST, ADST_DCT, DCT_ADST, DCT_ADST, ADST_DCT, ADST_ADST, DCT_DCT, DCT_DCT, DCT_DCT, DCT_DCT];

/// counter_to_context (6.5.1); INVALID_CASE entries are 9.
pub const COUNTER_TO_CONTEXT: [u8; 19] = [2, 3, 4, 1, 3, 9, 0, 9, 9, 5, 5, 9, 5, 9, 9, 9, 9, 9, 6];

/// idx_n_column_to_subblock (6.5.11).
pub const IDX_N_COLUMN_TO_SUBBLOCK: [[u8; 2]; 4] = [[1, 2], [1, 3], [3, 2], [3, 3]];

/// literal_to_type (6.2.7).
pub const LITERAL_TO_TYPE: [u8; 4] = [1, 0, 2, 3];

pub const SEGMENTATION_FEATURE_BITS: [u32; 4] = [8, 6, 2, 0];
pub const SEGMENTATION_FEATURE_SIGNED: [bool; 4] = [true, true, false, false];

/// extra_bits[token] = (cat, numExtra, base) (6.4.26).
pub const EXTRA_BITS: [(u8, u8, u16); 11] =
    [(0, 0, 0), (0, 0, 1), (0, 0, 2), (0, 0, 3), (0, 0, 4), (1, 1, 5), (2, 2, 7), (3, 3, 11), (4, 4, 19), (5, 5, 35), (6, 14, 67)];

/// cat_probs (6.4.26).
pub const CAT_PROBS: [&[u8]; 7] = [
    &[],
    &[159],
    &[165, 145],
    &[173, 148, 140],
    &[176, 155, 140, 135],
    &[180, 157, 141, 134, 130],
    &[254, 254, 254, 252, 249, 243, 230, 196, 177, 153, 140, 133, 130, 129],
];

// Decode trees (9.3.1); leaves are stored negated.
pub const PARTITION_TREE: [i8; 6] = [0, 2, -1, 4, -2, -3];
pub const INTRA_MODE_TREE: [i8; 18] = [
    -(DC_PRED as i8),
    2,
    -(TM_PRED as i8),
    4,
    -(V_PRED as i8),
    6,
    8,
    12,
    -(H_PRED as i8),
    10,
    -(D135_PRED as i8),
    -(D117_PRED as i8),
    -(D45_PRED as i8),
    14,
    -(D63_PRED as i8),
    16,
    -(D153_PRED as i8),
    -(D207_PRED as i8),
];
pub const SEGMENT_TREE: [i8; 14] = [2, 4, 6, 8, 10, 12, 0, -1, -2, -3, -4, -5, -6, -7];
pub const TX_SIZE_32_TREE: [i8; 6] = [0, 2, -1, 4, -2, -3];
pub const TX_SIZE_16_TREE: [i8; 4] = [0, 2, -1, -2];
pub const TX_SIZE_8_TREE: [i8; 2] = [0, -1];
/// inter_mode_tree: values are inter_mode = y_mode - NEARESTMV.
pub const INTER_MODE_TREE: [i8; 6] = [-2, 2, 0, 4, -1, -3];
pub const INTERP_FILTER_TREE: [i8; 4] = [0, 2, -1, -2];
pub const MV_JOINT_TREE: [i8; 6] = [0, 2, -1, 4, -2, -3];
pub const MV_CLASS_TREE: [i8; 20] = [0, 2, -1, 4, 6, 8, -2, -3, 10, 12, -4, -5, -6, 14, 16, 18, -7, -8, -9, -10];
pub const MV_FR_TREE: [i8; 6] = [0, 2, -1, 4, -2, -3];
/// small_token_tree used by coefficient probability adaptation (8.4.3).
pub const SMALL_TOKEN_TREE: [i8; 6] = [0, 0, 0, 4, -1, -2];
pub const BINARY_TREE: [i8; 2] = [0, -1];

/// Probabilities of the full token tree for each model probability (9.3.2 pareto): row
/// `p - 1` holds the probabilities of token-tree nodes 2..9 when `coef_probs[..][2] == p`.
pub fn pareto_full() -> &'static [[u8; 8]; 255] {
    use std::sync::OnceLock;
    static T: OnceLock<[[u8; 8]; 255]> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = [[0u8; 8]; 255];
        for p in 1..=255usize {
            let x = (p - 1) / 2;
            for n in 0..8 {
                t[p - 1][n] =
                    if p & 1 == 1 { PARETO_TABLE[x * 8 + n] } else { ((PARETO_TABLE[x * 8 + n] as u32 + PARETO_TABLE[(x + 1) * 8 + n] as u32) >> 1) as u8 };
            }
        }
        t
    })
}

/// Scan order for a transform size and type (6.4.25).
pub fn scan(tx_size: u8, tx_type: u8) -> &'static [u16] {
    match (tx_size, tx_type) {
        (0, ADST_DCT) => &ROW_SCAN_4X4,
        (0, DCT_ADST) => &COL_SCAN_4X4,
        (0, _) => &DEFAULT_SCAN_4X4,
        (1, ADST_DCT) => &ROW_SCAN_8X8,
        (1, DCT_ADST) => &COL_SCAN_8X8,
        (1, _) => &DEFAULT_SCAN_8X8,
        (2, ADST_DCT) => &ROW_SCAN_16X16,
        (2, DCT_ADST) => &COL_SCAN_16X16,
        (2, _) => &DEFAULT_SCAN_16X16,
        _ => &DEFAULT_SCAN_32X32,
    }
}

/// One scan position of a transform block: the coefficient it reads, the two neighbours whose
/// token energy gives the context of the next ones (9.3.2, `nb`) and its band (6.4.24).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanPos {
    pub pos: u16,
    pub nb: [u16; 2],
    pub band: u8,
}

/// The scan of `sc` for a `4 << tx` square transform of type `ty`, with neighbours and bands.
const fn scan_table<const N: usize>(sc: &[u16; N], tx: usize, ty: u8) -> [ScanPos; N] {
    let n = 4usize << tx;
    let mut t = [ScanPos { pos: 0, nb: [0, 0], band: 0 }; N];
    let mut c = 0;
    while c < N {
        let pos = sc[c] as usize;
        let (i, j) = (pos / n, pos % n);
        let nb = if c == 0 {
            [0, 0]
        } else if i > 0 && j > 0 {
            let a = ((i - 1) * n + j) as u16;
            let a2 = (i * n + j - 1) as u16;
            match ty {
                DCT_ADST => [a, a],
                ADST_DCT => [a2, a2],
                _ => [a, a2],
            }
        } else if i > 0 {
            let a = ((i - 1) * n + j) as u16;
            [a, a]
        } else {
            let a = (i * n + j - 1) as u16;
            [a, a]
        };
        let band = if tx == 0 { COEFBAND_4X4[c] } else { COEFBAND_8X8PLUS[c] };
        t[c] = ScanPos { pos: pos as u16, nb, band };
        c += 1;
    }
    t
}

static DEFAULT_TABLE_4X4: [ScanPos; 16] = scan_table(&DEFAULT_SCAN_4X4, 0, DCT_DCT);
static COL_TABLE_4X4: [ScanPos; 16] = scan_table(&COL_SCAN_4X4, 0, DCT_ADST);
static ROW_TABLE_4X4: [ScanPos; 16] = scan_table(&ROW_SCAN_4X4, 0, ADST_DCT);
static DEFAULT_TABLE_8X8: [ScanPos; 64] = scan_table(&DEFAULT_SCAN_8X8, 1, DCT_DCT);
static COL_TABLE_8X8: [ScanPos; 64] = scan_table(&COL_SCAN_8X8, 1, DCT_ADST);
static ROW_TABLE_8X8: [ScanPos; 64] = scan_table(&ROW_SCAN_8X8, 1, ADST_DCT);
static DEFAULT_TABLE_16X16: [ScanPos; 256] = scan_table(&DEFAULT_SCAN_16X16, 2, DCT_DCT);
static COL_TABLE_16X16: [ScanPos; 256] = scan_table(&COL_SCAN_16X16, 2, DCT_ADST);
static ROW_TABLE_16X16: [ScanPos; 256] = scan_table(&ROW_SCAN_16X16, 2, ADST_DCT);
// 32x32 transforms are always DCT_DCT.
static DEFAULT_TABLE_32X32: [ScanPos; 1024] = scan_table(&DEFAULT_SCAN_32X32, 3, DCT_DCT);

/// The scan of a transform size and type with each position's context neighbours and band, in
/// the order of `scan`.
pub fn scan_positions(tx_size: u8, tx_type: u8) -> &'static [ScanPos] {
    match (tx_size, tx_type) {
        (0, ADST_DCT) => &ROW_TABLE_4X4,
        (0, DCT_ADST) => &COL_TABLE_4X4,
        (0, _) => &DEFAULT_TABLE_4X4,
        (1, ADST_DCT) => &ROW_TABLE_8X8,
        (1, DCT_ADST) => &COL_TABLE_8X8,
        (1, _) => &DEFAULT_TABLE_8X8,
        (2, ADST_DCT) => &ROW_TABLE_16X16,
        (2, DCT_ADST) => &COL_TABLE_16X16,
        (2, _) => &DEFAULT_TABLE_16X16,
        _ => &DEFAULT_TABLE_32X32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trees_are_consistent() {
        // Every leaf value appears exactly once.
        fn leaves(t: &[i8]) -> Vec<i8> {
            let mut v: Vec<i8> =
                t.iter().enumerate().filter(|(i, x)| **x <= 0 && !(*i == 0 && t.len() > 2 && t[0] == 0 && t[1] == 0)).map(|(_, x)| -x).collect();
            v.sort();
            v
        }
        assert_eq!(leaves(&INTRA_MODE_TREE), (0..10).collect::<Vec<i8>>());
        assert_eq!(leaves(&MV_CLASS_TREE), (0..11).collect::<Vec<i8>>());
        assert_eq!(leaves(&PARTITION_TREE), (0..4).collect::<Vec<i8>>());
        assert_eq!(leaves(&INTER_MODE_TREE), (0..4).collect::<Vec<i8>>());
    }

    /// Coefficient context neighbours (9.3.2, `nb`) for every scan position of a transform size and
    /// type: `[c] = (nb0, nb1)` positions in the block.
    fn neighbors(tx_size: u8, tx_type: u8) -> &'static [(u16, u16)] {
        use std::sync::OnceLock;
        static T: OnceLock<Vec<Vec<(u16, u16)>>> = OnceLock::new();
        let all = T.get_or_init(|| {
            let mut v = Vec::new();
            for tx in 0..4u8 {
                for ty in 0..4u8 {
                    let sc = scan(tx, ty);
                    let n = 4usize << tx;
                    let mut nb = vec![(0u16, 0u16); sc.len()];
                    for (c, &pos) in sc.iter().enumerate().skip(1) {
                        let pos = pos as usize;
                        let (i, j) = (pos / n, pos % n);
                        nb[c] = if i > 0 && j > 0 {
                            let a = ((i - 1) * n + j) as u16;
                            let a2 = (i * n + j - 1) as u16;
                            // Only the 4x4..16x16 scans depend on the type; 32x32 is always DCT_DCT.
                            let ty = if tx == 3 { DCT_DCT } else { ty };
                            match ty {
                                DCT_ADST => (a, a),
                                ADST_DCT => (a2, a2),
                                _ => (a, a2),
                            }
                        } else if i > 0 {
                            let a = ((i - 1) * n + j) as u16;
                            (a, a)
                        } else {
                            let a = (i * n + j - 1) as u16;
                            (a, a)
                        };
                    }
                    v.push(nb);
                }
            }
            v
        });
        &all[(tx_size * 4 + tx_type) as usize]
    }

    #[test]
    fn scan_positions_match_the_neighbour_derivation() {
        for tx in 0..4u8 {
            for ty in 0..4u8 {
                let t = scan_positions(tx, ty);
                let sc = scan(tx, ty);
                let nb = neighbors(tx, ty);
                assert_eq!(t.len(), sc.len());
                for (c, e) in t.iter().enumerate() {
                    let band = if tx == 0 { COEFBAND_4X4[c] } else { COEFBAND_8X8PLUS[c] };
                    assert_eq!(*e, ScanPos { pos: sc[c], nb: [nb[c].0, nb[c].1], band }, "tx {tx} type {ty} c {c}");
                }
            }
        }
    }

    #[test]
    fn pareto_rows() {
        let p = pareto_full();
        assert_eq!(p[0], [3, 86, 128, 6, 86, 23, 88, 29]);
        assert_eq!(p[254][..], PARETO_TABLE[127 * 8..128 * 8]);
    }

    #[test]
    fn default_coef_probs_unused_are_zero() {
        for (k, chunk) in DEFAULT_COEF_PROBS.chunks(3 * 6).enumerate() {
            let band = k % 6;
            for (ctx, p) in chunk.chunks(3).enumerate() {
                if band == 0 && ctx >= 3 {
                    assert_eq!(p, [0, 0, 0]);
                } else {
                    assert!(p.iter().all(|&x| x > 0), "band {band} ctx {ctx}");
                }
            }
        }
    }
}
