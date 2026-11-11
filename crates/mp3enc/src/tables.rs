//! Tables from ISO/IEC 11172-3 (MPEG-1) and ISO/IEC 13818-3 (MPEG-2 lower sampling frequencies).

use std::sync::OnceLock;

/// Layer III bit rates in kbit/s by `bitrate_index` 1-14 (§2.4.2.3); index 0 (free format) and
/// 15 (forbidden) are never written.
pub(crate) const BITRATES_MPEG1: [u32; 14] = [32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320];
/// Layer III bit rates of the lower sampling frequencies (ISO/IEC 13818-3 §2.4.2.3).
pub(crate) const BITRATES_LSF: [u32; 14] = [8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];

/// Sampling frequencies by `sampling_frequency` (0-2) of MPEG-1 and of MPEG-2 LSF.
pub(crate) const RATES_MPEG1: [u32; 3] = [44_100, 48_000, 32_000];
pub(crate) const RATES_LSF: [u32; 3] = [22_050, 24_000, 16_000];

/// Long-block scalefactor band boundaries (Table 3-B.8 of 11172-3; Table B.2 of 13818-3), by
/// `sampling_frequency`: 22 bands over the 576 frequency lines of a granule.
pub(crate) const SFB_MPEG1: [[u16; 23]; 3] = [
    [0, 4, 8, 12, 16, 20, 24, 30, 36, 44, 52, 62, 74, 90, 110, 134, 162, 196, 238, 288, 342, 418, 576],
    [0, 4, 8, 12, 16, 20, 24, 30, 36, 42, 50, 60, 72, 88, 106, 128, 156, 190, 230, 276, 330, 384, 576],
    [0, 4, 8, 12, 16, 20, 24, 30, 36, 44, 54, 66, 82, 102, 126, 156, 194, 240, 296, 364, 448, 550, 576],
];
pub(crate) const SFB_LSF: [[u16; 23]; 3] = [
    [0, 6, 12, 18, 24, 30, 36, 44, 54, 66, 80, 96, 116, 140, 168, 200, 238, 284, 336, 396, 464, 522, 576],
    [0, 6, 12, 18, 24, 30, 36, 44, 54, 66, 80, 96, 114, 136, 162, 194, 232, 278, 332, 394, 464, 540, 576],
    [0, 6, 12, 18, 24, 30, 36, 44, 54, 66, 80, 96, 116, 140, 168, 200, 238, 284, 336, 396, 464, 522, 576],
];

/// `slen1` (bands 0-10) and `slen2` (bands 11-20) by MPEG-1 `scalefac_compress` (§2.4.2.7).
pub(crate) const SLEN: [(u32, u32); 16] =
    [(0, 0), (0, 1), (0, 2), (0, 3), (3, 0), (1, 1), (1, 2), (1, 3), (2, 1), (2, 2), (2, 3), (3, 1), (3, 2), (3, 3), (4, 2), (4, 3)];

/// Scalefactor bands per partition for long blocks without intensity stereo when MPEG-2
/// `scalefac_compress` < 400 (13818-3 §2.4.3.2, `nr_of_sfb_block[0][0]`).
pub(crate) const LSF_PARTITIONS: [usize; 4] = [6, 5, 5, 5];

/// Coefficients `D[0..=256]` of the synthesis window (Table 3-B.3), in units of 2⁻¹⁶: the table's
/// nine-decimal values are exact multiples of 1/65536. The analysis window of Table 3-C.1 is the
/// same prototype divided by 32: `C[i] = D[i] / 32`.
const WINDOW_HALF: [i32; 257] = [
    0, -1, -1, -1, -1, -1, -1, -2, -2, -2, -2, -3, -3, -4, -4, -5, //
    -5, -6, -7, -7, -8, -9, -10, -11, -13, -14, -16, -17, -19, -21, -24, -26, //
    -29, -31, -35, -38, -41, -45, -49, -53, -58, -63, -68, -73, -79, -85, -91, -97, //
    -104, -111, -117, -125, -132, -139, -147, -154, -161, -169, -176, -183, -190, -196, -202, -208, //
    213, 218, 222, 225, 227, 228, 228, 227, 224, 221, 215, 208, 200, 189, 177, 163, //
    146, 127, 106, 83, 57, 29, -2, -36, -72, -111, -153, -197, -244, -294, -347, -401, //
    -459, -519, -581, -645, -711, -779, -848, -919, -991, -1064, -1137, -1210, -1283, -1356, -1428, -1498, //
    -1567, -1634, -1698, -1759, -1817, -1870, -1919, -1962, -2001, -2032, -2057, -2075, -2085, -2087, -2080, -2063, //
    2037, 2000, 1952, 1893, 1822, 1739, 1644, 1535, 1414, 1280, 1131, 970, 794, 605, 402, 185, //
    -45, -288, -545, -814, -1095, -1388, -1692, -2006, -2330, -2663, -3004, -3351, -3705, -4063, -4425, -4788, //
    -5153, -5517, -5879, -6237, -6589, -6935, -7271, -7597, -7910, -8209, -8491, -8755, -8998, -9219, -9416, -9585, //
    -9727, -9838, -9916, -9959, -9966, -9935, -9863, -9750, -9592, -9389, -9139, -8840, -8492, -8092, -7640, -7134, //
    6574, 5959, 5288, 4561, 3776, 2935, 2037, 1082, 70, -998, -2122, -3300, -4533, -5818, -7154, -8540, //
    -9975, -11455, -12980, -14548, -16155, -17799, -19478, -21189, -22929, -24694, -26482, -28289, -30112, -31947, -33791, -35640, //
    -37489, -39336, -41176, -43006, -44821, -46617, -48390, -50137, -51853, -53534, -55178, -56778, -58333, -59838, -61289, -62684, //
    -64019, -65290, -66494, -67629, -68692, -69679, -70590, -71420, -72169, -72835, -73415, -73908, -74313, -74630, -74856, -74992, //
    75038,
];

/// The 512 synthesis window coefficients `D[i]` (Table 3-B.3). The table stores the symmetric
/// prototype `h[i] = h[512 - i]` with every other block of 64 negated, so for `i > 256`
/// `D[i] = -D[512 - i]`, except at multiples of 64 where both halves share the block parity.
pub(crate) fn synthesis_window() -> &'static [f32; 512] {
    static W: OnceLock<[f32; 512]> = OnceLock::new();
    W.get_or_init(|| {
        let mut w = [0f32; 512];
        for (i, d) in w.iter_mut().enumerate() {
            let (j, sign) = if i <= 256 { (i, 1.0) } else { (512 - i, if i % 64 == 0 { 1.0 } else { -1.0 }) };
            *d = sign * WINDOW_HALF.get(j).copied().unwrap_or(0) as f32 / 65_536.0;
        }
        w
    })
}

/// The analysis window `C[i]` (Table 3-C.1): `D[i] / 32`.
pub(crate) fn analysis_window() -> &'static [f32; 512] {
    static C: OnceLock<[f32; 512]> = OnceLock::new();
    C.get_or_init(|| {
        let mut c = [0f32; 512];
        c.iter_mut().zip(synthesis_window()).for_each(|(c, d)| *c = d / 32.0);
        c
    })
}

/// The analysis matrixing coefficients `M[i][k] = cos((2i + 1)(k - 16)π / 64)` (Annex C.1.3),
/// `i` in `0..32`, `k` in `0..64`.
pub(crate) fn analysis_matrix() -> &'static [[f32; 64]; 32] {
    static M: OnceLock<[[f32; 64]; 32]> = OnceLock::new();
    M.get_or_init(|| {
        let mut m = [[0f32; 64]; 32];
        for (i, row) in m.iter_mut().enumerate() {
            for (k, c) in row.iter_mut().enumerate() {
                *c = ((2 * i + 1) as f64 * (k as f64 - 16.0) * std::f64::consts::PI / 64.0).cos() as f32;
            }
        }
        m
    })
}

/// Alias-reduction butterfly coefficients `(cs[i], ca[i])` from `c[i]` (Table 3-B.9).
pub(crate) fn alias_coefficients() -> &'static [(f32, f32); 8] {
    static A: OnceLock<[(f32, f32); 8]> = OnceLock::new();
    A.get_or_init(|| {
        const C: [f64; 8] = [-0.6, -0.535, -0.33, -0.185, -0.095, -0.041, -0.0142, -0.0037];
        let mut a = [(0f32, 0f32); 8];
        for (o, c) in a.iter_mut().zip(C) {
            let n = (1.0 + c * c).sqrt();
            *o = ((1.0 / n) as f32, (c / n) as f32);
        }
        a
    })
}

/// `|i|^(4/3)` for every quantized magnitude a big-values escape can carry (`15 + 2^13 - 1`).
pub(crate) const MAX_QUANT: usize = 8206;
pub(crate) fn pow43() -> &'static [f32] {
    static P: OnceLock<Vec<f32>> = OnceLock::new();
    P.get_or_init(|| (0..=MAX_QUANT).map(|i| (i as f64).powf(4.0 / 3.0) as f32).collect())
}

/// The absolute threshold of hearing in dB SPL at `f` Hz (Terhardt's approximation, as in the
/// psychoacoustic models of 11172-3 Annex D).
pub(crate) fn ath_db(f: f64) -> f64 {
    let k = (f.max(20.0)) / 1000.0;
    3.64 * k.powf(-0.8) - 6.5 * (-0.6 * (k - 3.3) * (k - 3.3)).exp() + 0.001 * k.powi(4)
}
