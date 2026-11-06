//! Constant tables defined by the ProRes bitstream format (SMPTE RDD 36).

/// A ProRes variable-length codebook: a Golomb-Rice code for small values that switches to an
/// Exp-Golomb code for large ones.
///
/// A codeword starts with `q` zero bits and a one bit.
/// - If `q <= rice_q_max` it is a Rice codeword: `value = (q << rice) | next(rice bits)`.
/// - Otherwise it is an Exp-Golomb codeword of order `exp` whose prefix is `q - rice_q_max - 1`
///   zeros, offset by the number of values the Rice part covers: `(rice_q_max + 1) << rice`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Codebook {
    pub rice: u8,
    pub exp: u8,
    pub rice_q_max: u8,
}

const fn cb(rice: u8, exp: u8, rice_q_max: u8) -> Codebook {
    Codebook { rice, exp, rice_q_max }
}

/// Codebook of the first DC coefficient in a slice component (equivalent to Exp-Golomb order 5).
pub(crate) const FIRST_DC_CB: Codebook = cb(5, 6, 0);

/// DC difference codebooks, selected by the previous DC codeword value (clamped to 6).
pub(crate) const DC_CB: [Codebook; 7] = [cb(0, 1, 0), cb(1, 2, 0), cb(1, 2, 0), cb(2, 3, 1), cb(2, 3, 1), cb(3, 4, 0), cb(3, 4, 0)];

/// AC run codebooks, selected by the previous run (clamped to 15).
pub(crate) const RUN_CB: [Codebook; 16] = [
    cb(0, 1, 2),
    cb(0, 1, 2),
    cb(0, 1, 1),
    cb(0, 1, 1),
    cb(0, 1, 0),
    cb(1, 2, 1),
    cb(1, 2, 1),
    cb(1, 2, 1),
    cb(1, 2, 1),
    cb(1, 2, 0),
    cb(1, 2, 0),
    cb(1, 2, 0),
    cb(1, 2, 0),
    cb(1, 2, 0),
    cb(1, 2, 0),
    cb(2, 3, 0),
];

/// AC level codebooks (level magnitude minus one), selected by the previous level magnitude (clamped to 9).
pub(crate) const LEVEL_CB: [Codebook; 10] =
    [cb(0, 1, 0), cb(0, 2, 2), cb(0, 1, 1), cb(0, 1, 2), cb(0, 1, 0), cb(1, 2, 0), cb(1, 2, 0), cb(1, 2, 0), cb(1, 2, 0), cb(2, 3, 0)];

/// Initial DC codebook context for the second DC of a component.
pub(crate) const DC_CTX_INIT: u32 = 5;
/// Initial AC run / level codebook contexts.
pub(crate) const RUN_CTX_INIT: u32 = 4;
pub(crate) const LEVEL_CTX_INIT: u32 = 2;

/// Progressive-frame coefficient scan: scan position → raster index within the 8×8 block.
pub(crate) const SCAN_PROGRESSIVE: [u8; 64] = [
    0, 1, 8, 9, 2, 3, 10, 11, //
    16, 17, 24, 25, 18, 19, 26, 27, //
    4, 5, 12, 20, 13, 6, 7, 14, //
    21, 28, 29, 22, 15, 23, 30, 31, //
    32, 33, 40, 48, 41, 34, 35, 42, //
    49, 56, 57, 50, 43, 36, 37, 44, //
    51, 58, 59, 52, 45, 38, 39, 46, //
    53, 60, 61, 54, 47, 55, 62, 63,
];

/// Interlaced-field coefficient scan: scan position → raster index within the 8×8 block.
pub(crate) const SCAN_INTERLACED: [u8; 64] = [
    0, 8, 1, 9, 16, 24, 17, 25, //
    2, 10, 3, 11, 18, 26, 19, 27, //
    32, 40, 33, 34, 41, 48, 56, 49, //
    42, 35, 43, 50, 57, 58, 51, 59, //
    4, 12, 5, 6, 13, 20, 28, 21, //
    14, 7, 15, 22, 29, 36, 44, 37, //
    30, 23, 31, 38, 45, 52, 60, 53, //
    46, 39, 47, 54, 61, 62, 55, 63,
];

/// Quantisation scale for a slice `quantization_index` (1..=224).
#[inline]
pub(crate) fn qscale(qidx: u8) -> u32 {
    let q = qidx.max(1) as u32;
    if q <= 128 { q } else { 128 + ((q - 128) << 2) }
}

/// Orthonormal 1-D DCT-II basis: `BASIS[u][x] = C(u)/2 · cos((2x+1)uπ/16)`, `C(0) = 1/√2`.
pub(crate) const BASIS: [[f32; 8]; 8] = [
    [0.353_553_39, 0.353_553_39, 0.353_553_39, 0.353_553_39, 0.353_553_39, 0.353_553_39, 0.353_553_39, 0.353_553_39],
    [0.49039264, 0.415_734_8, 0.277_785_12, 0.097_545_16, -0.097_545_16, -0.277_785_12, -0.415_734_8, -0.49039264],
    [0.461_939_77, 0.191_341_72, -0.191_341_72, -0.461_939_77, -0.461_939_77, -0.191_341_72, 0.191_341_72, 0.461_939_77],
    [0.415_734_8, -0.097_545_16, -0.49039264, -0.277_785_12, 0.277_785_12, 0.49039264, 0.097_545_16, -0.415_734_8],
    [0.353_553_39, -0.353_553_39, -0.353_553_39, 0.353_553_39, 0.353_553_39, -0.353_553_39, -0.353_553_39, 0.353_553_39],
    [0.277_785_12, -0.49039264, 0.097_545_16, 0.415_734_8, -0.415_734_8, -0.097_545_16, 0.49039264, -0.277_785_12],
    [0.191_341_72, -0.461_939_77, 0.461_939_77, -0.191_341_72, -0.191_341_72, 0.461_939_77, -0.461_939_77, 0.191_341_72],
    [0.097_545_16, -0.277_785_12, 0.415_734_8, -0.49039264, 0.49039264, -0.415_734_8, 0.277_785_12, -0.097_545_16],
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_are_permutations() {
        for scan in [&SCAN_PROGRESSIVE, &SCAN_INTERLACED] {
            let mut seen = [false; 64];
            for &i in scan.iter() {
                assert!(!seen[i as usize]);
                seen[i as usize] = true;
            }
        }
    }

    #[test]
    fn basis_matches_cosines() {
        for u in 0..8 {
            for x in 0..8 {
                let c = if u == 0 { (0.125f64).sqrt() } else { 0.5 };
                let v = c * (((2 * x + 1) * u) as f64 * std::f64::consts::PI / 16.0).cos();
                assert!((v - BASIS[u][x] as f64).abs() < 1e-7);
            }
        }
    }

    #[test]
    fn qscale_mapping() {
        assert_eq!(qscale(1), 1);
        assert_eq!(qscale(128), 128);
        assert_eq!(qscale(129), 132);
        assert_eq!(qscale(224), 512);
    }
}
