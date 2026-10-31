//! Tables from ISO/IEC 11172-3 Annex B and ISO/IEC 13818-3 Annex B.

use std::sync::OnceLock;

/// Bit rates in kbit/s by `bitrate_index` (ISO/IEC 11172-3 §2.4.2.3; ISO/IEC 13818-3 §2.4.2.3 for
/// the lower sampling frequencies). Index 0 is the free format, index 15 is forbidden.
pub(crate) const BITRATES_MPEG1_L1: [u32; 15] = [0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448];
pub(crate) const BITRATES_MPEG1_L2: [u32; 15] = [0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384];
pub(crate) const BITRATES_MPEG1_L3: [u32; 15] = [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320];
pub(crate) const BITRATES_LSF_L1: [u32; 15] = [0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256];
pub(crate) const BITRATES_LSF_L23: [u32; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];

/// Sampling frequencies of MPEG-1 by `sampling_frequency` (3 is reserved); MPEG-2 halves them,
/// the unofficial MPEG-2.5 quarters them.
pub(crate) const SAMPLE_RATES: [u32; 3] = [44_100, 48_000, 32_000];

/// Coefficients `D[0..=256]` of the synthesis window (Table 3-B.3), in units of 2⁻¹⁶: the table's
/// nine-decimal values are exact multiples of 1/65536. The rest of the window follows from the
/// symmetry of the prototype filter, see [`window`].
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

/// `D[i]` for `i` in `0..=256`, in units of 2⁻¹⁶.
pub(crate) fn window_half(i: usize) -> Option<i32> {
    WINDOW_HALF.get(i).copied()
}

/// The 512 synthesis window coefficients `D[i]` (Table 3-B.3). The table stores the symmetric
/// prototype `h[i] = h[512 - i]` with every other block of 64 negated, so for `i > 256`
/// `D[i] = -D[512 - i]`, except at multiples of 64 where both halves share the block parity.
pub(crate) fn window() -> &'static [f32; 512] {
    static W: OnceLock<[f32; 512]> = OnceLock::new();
    W.get_or_init(|| {
        let mut w = [0f32; 512];
        for (i, d) in w.iter_mut().enumerate() {
            let (j, sign) = if i <= 256 { (i, 1.0) } else { (512 - i, if i % 64 == 0 { 1.0 } else { -1.0 }) };
            *d = sign * window_half(j).unwrap_or(0) as f32 / 65_536.0;
        }
        w
    })
}

/// The matrixing coefficients `N[i][k] = cos((16 + i)(2k + 1)π / 64)` of the synthesis subband
/// filter (§2.4.3.2, Figure A.2), `i` in `0..64`, `k` in `0..32`.
pub(crate) fn matrix() -> &'static [[f32; 32]; 64] {
    static N: OnceLock<[[f32; 32]; 64]> = OnceLock::new();
    N.get_or_init(|| {
        let mut n = [[0f32; 32]; 64];
        for (i, row) in n.iter_mut().enumerate() {
            for (k, c) in row.iter_mut().enumerate() {
                *c = (((16 + i) * (2 * k + 1)) as f64 * std::f64::consts::PI / 64.0).cos() as f32;
            }
        }
        n
    })
}

/// Scalefactors (Table 3-B.1): `2^(1 - index / 3)`. Index 63 is not used by encoders; it continues
/// the progression rather than failing the frame.
pub(crate) fn scalefactor(index: u32) -> f32 {
    static S: OnceLock<[f32; 64]> = OnceLock::new();
    let s = S.get_or_init(|| {
        let mut s = [0f32; 64];
        for (i, v) in s.iter_mut().enumerate() {
            *v = 2f64.powf(1.0 - i as f64 / 3.0) as f32;
        }
        s
    });
    s.get(index as usize).copied().unwrap_or(0.0)
}

// Layer II possible quantizations per subband (Tables 3-B.2a-d; ISO/IEC 13818-3 Table B.1): the
// number of quantization steps for each value of the subband's allocation field, 0 meaning
// nothing is transmitted. A row has 2^nbal entries.

/// 3-B.2a/b subbands 0-2.
const A_LOW: [u16; 16] = [0, 3, 7, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 16383, 32767, 65535];
/// 3-B.2a/b subbands 3-10.
const A_MID: [u16; 16] = [0, 3, 5, 7, 9, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 65535];
/// 3-B.2a/b subbands 11-22.
const A_HIGH: [u16; 8] = [0, 3, 5, 7, 9, 15, 31, 65535];
/// 3-B.2a/b subbands 23-29.
const A_TOP: [u16; 4] = [0, 3, 5, 65535];
/// 3-B.2c/d subbands 0-1.
const C_LOW: [u16; 16] = [0, 3, 5, 9, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 16383, 32767];
/// 3-B.2c/d subbands 2-11; 13818-3 B.1 subbands 4-10.
const C_HIGH: [u16; 8] = [0, 3, 5, 9, 15, 31, 63, 127];
/// 13818-3 B.1 subbands 0-3.
const LSF_LOW: [u16; 16] = [0, 3, 5, 7, 9, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 16383];
/// 13818-3 B.1 subbands 11-29.
const LSF_TOP: [u16; 4] = [0, 3, 5, 9];

/// Subbands of the polyphase filter bank: the size of the decoder's per-subband arrays.
pub(crate) const SUBBANDS: usize = 32;

// Every table's `sblimit` fits the per-subband arrays, so `sb < sblimit` indexes them in bounds.
const _: () = {
    let tables = [AllocTable::A, AllocTable::B, AllocTable::C, AllocTable::D, AllocTable::Lsf];
    let mut i = 0;
    while i < tables.len() {
        assert!(tables[i].sblimit() <= SUBBANDS);
        i += 1;
    }
};

/// A Layer II bit allocation table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AllocTable {
    /// 3-B.2a: 27 subbands.
    A,
    /// 3-B.2b: 30 subbands.
    B,
    /// 3-B.2c: 8 subbands.
    C,
    /// 3-B.2d: 12 subbands.
    D,
    /// ISO/IEC 13818-3 Table B.1 (lower sampling frequencies): 30 subbands.
    Lsf,
}

impl AllocTable {
    /// The table for a Layer II stream (§2.4.3.3.1 and Table 3-B.2): by sampling frequency and bit
    /// rate per channel. ISO/IEC 11172-3 lists only the allowed bit rate / mode combinations;
    /// encoders also write the others (ffmpeg's default for mono is 384 kbit/s), so the bit rate
    /// ranges are taken as open-ended: at least 56 kbit/s per channel at 48 kHz, or 56-80 kbit/s,
    /// is table a; 96 kbit/s and up at 44.1 / 32 kHz is table b; 48 kbit/s and below is table c
    /// (d at 32 kHz). The free format (`kbps_per_channel` 0) uses a or b like the high rates.
    pub(crate) fn select(lsf: bool, sample_rate: u32, kbps_per_channel: u32) -> Self {
        if lsf {
            return Self::Lsf;
        }
        let free = kbps_per_channel == 0;
        if (sample_rate == 48_000 && (free || kbps_per_channel >= 56)) || (56..=80).contains(&kbps_per_channel) {
            Self::A
        } else if free || kbps_per_channel >= 96 {
            Self::B
        } else if sample_rate == 32_000 {
            Self::D
        } else {
            Self::C
        }
    }

    /// Subbands with an allocation (`sblimit`): at most [`SUBBANDS`] (checked at compile time).
    pub(crate) const fn sblimit(self) -> usize {
        match self {
            Self::A => 27,
            Self::B | Self::Lsf => 30,
            Self::C => 8,
            Self::D => 12,
        }
    }

    /// The quantization steps per allocation value for subband `sb`; its length is 2^nbal. A match,
    /// not an index: any `sb` gets a row (subbands past `sblimit` get the table's last one).
    pub(crate) fn row(self, sb: usize) -> &'static [u16] {
        match self {
            Self::A | Self::B => match sb {
                0..=2 => &A_LOW,
                3..=10 => &A_MID,
                11..=22 => &A_HIGH,
                _ => &A_TOP,
            },
            Self::C | Self::D => match sb {
                0..=1 => &C_LOW,
                _ => &C_HIGH,
            },
            Self::Lsf => match sb {
                0..=3 => &LSF_LOW,
                4..=10 => &C_HIGH,
                _ => &LSF_TOP,
            },
        }
    }
}

/// How the samples of a quantization class are coded (Table 3-B.4): `Some(bits)` for the grouped
/// classes (three samples in one codeword), `None` for one `log2(steps + 1)`-bit code per sample.
pub(crate) fn grouping(steps: u16) -> Option<u32> {
    match steps {
        3 => Some(5),
        5 => Some(7),
        9 => Some(10),
        _ => None,
    }
}
