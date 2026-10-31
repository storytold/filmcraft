//! Trees, enumerations and small tables of RFC 6386 that are not printed as `= { ... }` arrays
//! (the large tables are in `spec_tables.rs`).

// Macroblock luma / chroma modes (8.2), in the specification's enumeration order.
pub const DC_PRED: u8 = 0;
pub const V_PRED: u8 = 1;
pub const H_PRED: u8 = 2;
pub const TM_PRED: u8 = 3;
pub const B_PRED: u8 = 4;

// Subblock modes (11.2).
pub const B_DC_PRED: u8 = 0;
pub const B_TM_PRED: u8 = 1;
pub const B_VE_PRED: u8 = 2;
pub const B_HE_PRED: u8 = 3;
pub const B_LD_PRED: u8 = 4;
pub const B_RD_PRED: u8 = 5;
pub const B_VR_PRED: u8 = 6;
pub const B_VL_PRED: u8 = 7;
pub const B_HD_PRED: u8 = 8;
pub const B_HU_PRED: u8 = 9;

// Inter modes (16.2), numbered after the intra modes as in the specification.
pub const MV_NEAREST: u8 = 5;
pub const MV_NEAR: u8 = 6;
pub const MV_ZERO: u8 = 7;
pub const MV_NEW: u8 = 8;
pub const MV_SPLIT: u8 = 9;

// Split MV subblock modes (16.4), relative numbering.
pub const LEFT_4X4: u8 = 0;
pub const ABOVE_4X4: u8 = 1;
pub const ZERO_4X4: u8 = 2;
pub const NEW_4X4: u8 = 3;

/// `ymode_tree` (8.2).
pub const YMODE_TREE: [i8; 8] = [-(DC_PRED as i8), 2, 4, 6, -(V_PRED as i8), -(H_PRED as i8), -(TM_PRED as i8), -(B_PRED as i8)];
/// `kf_ymode_tree` (8.2).
pub const KF_YMODE_TREE: [i8; 8] = [-(B_PRED as i8), 2, 4, 6, -(DC_PRED as i8), -(V_PRED as i8), -(H_PRED as i8), -(TM_PRED as i8)];
/// `uv_mode_tree` (8.2).
pub const UV_MODE_TREE: [i8; 6] = [-(DC_PRED as i8), 2, -(V_PRED as i8), 4, -(H_PRED as i8), -(TM_PRED as i8)];
/// `bmode_tree` (11.2).
pub const BMODE_TREE: [i8; 18] = [
    -(B_DC_PRED as i8),
    2,
    -(B_TM_PRED as i8),
    4,
    -(B_VE_PRED as i8),
    6,
    8,
    12,
    -(B_HE_PRED as i8),
    10,
    -(B_RD_PRED as i8),
    -(B_VR_PRED as i8),
    -(B_LD_PRED as i8),
    14,
    -(B_VL_PRED as i8),
    16,
    -(B_HD_PRED as i8),
    -(B_HU_PRED as i8),
];
/// `mb_segment_tree` (10).
pub const SEGMENT_TREE: [i8; 6] = [2, 4, 0, -1, -2, -3];
/// `mv_ref_tree` (16.2), values relative to `MV_NEAREST`.
pub const MV_REF_TREE: [i8; 8] =
    [-((MV_ZERO - MV_NEAREST) as i8), 2, 0, 4, -((MV_NEAR - MV_NEAREST) as i8), 6, -((MV_NEW - MV_NEAREST) as i8), -((MV_SPLIT - MV_NEAREST) as i8)];

// Split partitionings (16.4).
pub const SPLIT_TOP_BOTTOM: u8 = 0;
pub const SPLIT_LEFT_RIGHT: u8 = 1;
pub const SPLIT_QUARTERS: u8 = 2;
pub const SPLIT_16: u8 = 3;
/// `mvpartition_tree` (16.4).
pub const MVPARTITION_TREE: [i8; 6] = [-(SPLIT_16 as i8), 2, -(SPLIT_QUARTERS as i8), 4, -(SPLIT_TOP_BOTTOM as i8), -(SPLIT_LEFT_RIGHT as i8)];
/// `sub_mv_ref_tree` (16.4).
pub const SUB_MV_REF_TREE: [i8; 6] = [-(LEFT_4X4 as i8), 2, -(ABOVE_4X4 as i8), 4, -(ZERO_4X4 as i8), -(NEW_4X4 as i8)];
/// The partition of each Y subblock, per partitioning (the layouts listed with the MVpartition
/// enumeration, 16.4).
pub const SPLIT_MAPS: [[u8; 16]; 4] = [
    [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1],
    [0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1],
    [0, 0, 1, 1, 0, 0, 1, 1, 2, 2, 3, 3, 2, 2, 3, 3],
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
];
/// Number of partitions per partitioning.
pub const SPLIT_COUNTS: [usize; 4] = [2, 2, 4, 16];

/// `small_mvtree` (17.1).
pub const SMALL_MV_TREE: [i8; 14] = [2, 8, 4, 6, 0, -1, -2, -3, 10, 12, -4, -5, -6, -7];
// Offsets in an MV_CONTEXT (17.1).
pub const MVP_IS_SHORT: usize = 0;
pub const MVP_SIGN: usize = 1;
pub const MVP_SHORT: usize = 2;
pub const MVP_BITS: usize = 9;

// DCT tokens (13.2).
pub const DCT_0: u8 = 0;
pub const DCT_4: u8 = 4;
pub const DCT_CAT1: u8 = 5;
pub const DCT_EOB: u8 = 11;
/// `coeff_tree` (13.2).
pub const COEFF_TREE: [i8; 22] = [-(DCT_EOB as i8), 2, -(DCT_0 as i8), 4, -1, 6, 8, 12, -2, 10, -3, -4, 14, 16, -5, -6, 18, 20, -7, -8, -9, -10];
/// `Pcat1` ... `Pcat6` (13.2), without the terminating zeros.
pub const PCAT: [&[u8]; 6] =
    [&[159], &[165, 145], &[173, 148, 140], &[176, 155, 140, 135], &[180, 157, 141, 134, 130], &[254, 254, 243, 230, 196, 177, 153, 140, 133, 130, 129]];
/// `categoryBase` (13.3).
pub const CAT_BASE: [i32; 6] = [5, 7, 11, 19, 35, 67];

/// Coefficient scan order (13): position `i` of the token stream is coefficient
/// `ZIGZAG[i]` of the 4x4 block in raster order — the usual 4x4 zig-zag.
pub const ZIGZAG: [usize; 16] = [0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15];

/// Probabilities of the segment tree when not updated (9.3).
pub const DEFAULT_SEGMENT_PROB: u8 = 255;
