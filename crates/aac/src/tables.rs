//! Sampling-rate dependent tables from ISO/IEC 14496-3 §4.5.4 (scalefactor bands) and §4.6.9 (TNS).

/// Sampling frequencies indexed by `samplingFrequencyIndex`.
pub const SAMPLE_RATES: [u32; 13] = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350];

/// Map an arbitrary sample rate to its `samplingFrequencyIndex`, if it is one of the standard rates.
pub fn sample_rate_index(rate: u32) -> Option<u8> {
    SAMPLE_RATES.iter().position(|&r| r == rate).map(|i| i as u8)
}

/// The table index used for scalefactor-band tables when a non-standard rate is signalled explicitly
/// (§4.5.4: rates are mapped to the nearest table by frequency ranges).
pub fn table_index_for_rate(rate: u32) -> u8 {
    match rate {
        92017.. => 0,
        75132.. => 1,
        55426.. => 2,
        46009.. => 3,
        37566.. => 4,
        27713.. => 5,
        23004.. => 6,
        18783.. => 7,
        13856.. => 8,
        11502.. => 9,
        9391.. => 10,
        _ => 11,
    }
}

const SWB_LONG_96: &[u16] = &[
    0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 40, 44, 48, 52, 56, 64, 72, 80, 88, 96, 108, 120, 132, 144, 156, 172, 188, 212, 240, 276, 320, 384, 448, 512, 576,
    640, 704, 768, 832, 896, 960, 1024,
];
const SWB_LONG_64: &[u16] = &[
    0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 40, 44, 48, 52, 56, 64, 72, 80, 88, 100, 112, 124, 140, 156, 172, 192, 216, 240, 268, 304, 344, 384, 424, 464, 504,
    544, 584, 624, 664, 704, 744, 784, 824, 864, 904, 944, 984, 1024,
];
const SWB_LONG_48: &[u16] = &[
    0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 40, 48, 56, 64, 72, 80, 88, 96, 108, 120, 132, 144, 160, 176, 196, 216, 240, 264, 292, 320, 352, 384, 416, 448, 480,
    512, 544, 576, 608, 640, 672, 704, 736, 768, 800, 832, 864, 896, 928, 1024,
];
const SWB_LONG_32: &[u16] = &[
    0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 40, 48, 56, 64, 72, 80, 88, 96, 108, 120, 132, 144, 160, 176, 196, 216, 240, 264, 292, 320, 352, 384, 416, 448, 480,
    512, 544, 576, 608, 640, 672, 704, 736, 768, 800, 832, 864, 896, 928, 960, 992, 1024,
];
const SWB_LONG_24: &[u16] = &[
    0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 40, 44, 52, 60, 68, 76, 84, 92, 100, 108, 116, 124, 136, 148, 160, 172, 188, 204, 220, 240, 260, 284, 308, 336, 364,
    396, 432, 468, 508, 552, 600, 652, 704, 768, 832, 896, 960, 1024,
];
const SWB_LONG_16: &[u16] = &[
    0, 8, 16, 24, 32, 40, 48, 56, 64, 72, 80, 88, 100, 112, 124, 136, 148, 160, 172, 184, 196, 212, 228, 244, 260, 280, 300, 320, 344, 368, 396, 424, 456, 492,
    532, 572, 616, 664, 716, 772, 832, 896, 960, 1024,
];
const SWB_LONG_8: &[u16] = &[
    0, 12, 24, 36, 48, 60, 72, 84, 96, 108, 120, 132, 144, 156, 172, 188, 204, 220, 236, 252, 268, 288, 308, 328, 348, 372, 396, 420, 448, 476, 508, 544, 580,
    620, 664, 712, 764, 820, 880, 944, 1024,
];

const SWB_SHORT_96: &[u16] = &[0, 4, 8, 12, 16, 20, 24, 32, 40, 48, 64, 92, 128];
const SWB_SHORT_48: &[u16] = &[0, 4, 8, 12, 16, 20, 28, 36, 44, 56, 68, 80, 96, 112, 128];
const SWB_SHORT_24: &[u16] = &[0, 4, 8, 12, 16, 20, 24, 28, 36, 44, 52, 64, 76, 92, 108, 128];
const SWB_SHORT_16: &[u16] = &[0, 4, 8, 12, 16, 20, 24, 28, 32, 40, 48, 60, 72, 88, 108, 128];
const SWB_SHORT_8: &[u16] = &[0, 4, 8, 12, 16, 20, 24, 28, 36, 44, 52, 60, 72, 88, 108, 128];

/// Scalefactor band offsets (`swb_offset_long_window`, 1024-line frames); `len() == num_swb + 1`.
pub fn swb_offsets_long(sf_index: u8) -> &'static [u16] {
    match sf_index {
        0 | 1 => SWB_LONG_96,
        2 => SWB_LONG_64,
        3 | 4 => SWB_LONG_48,
        5 => SWB_LONG_32,
        6 | 7 => SWB_LONG_24,
        8..=10 => SWB_LONG_16,
        _ => SWB_LONG_8,
    }
}

/// Scalefactor band offsets for one short window (128 lines).
pub fn swb_offsets_short(sf_index: u8) -> &'static [u16] {
    match sf_index {
        0..=2 => SWB_SHORT_96,
        3..=5 => SWB_SHORT_48,
        6 | 7 => SWB_SHORT_24,
        8..=10 => SWB_SHORT_16,
        _ => SWB_SHORT_8,
    }
}

/// `TNS_MAX_BANDS` for AAC-LC (long, short).
pub fn tns_max_bands(sf_index: u8, short: bool) -> usize {
    const LONG: [u8; 13] = [31, 31, 34, 40, 42, 51, 46, 46, 42, 42, 42, 39, 39];
    const SHORT: [u8; 13] = [9, 9, 10, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14];
    let i = (sf_index as usize).min(12);
    if short { SHORT[i] as usize } else { LONG[i] as usize }
}

/// `TNS_MAX_ORDER` for AAC-LC.
pub fn tns_max_order(short: bool) -> usize {
    if short { 7 } else { 12 }
}

/// The `x^(4/3)` table for the integer quantised magnitudes 0..=8191.
pub fn pow43_table() -> &'static [f32; 8192] {
    use std::sync::OnceLock;
    static TABLE: OnceLock<Box<[f32; 8192]>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t = Box::new([0f32; 8192]);
        for (i, v) in t.iter_mut().enumerate() {
            *v = (i as f64).powf(4.0 / 3.0) as f32;
        }
        t
    })
}

/// `x^(4/3)` for an integer quantised magnitude (table lookup up to 8191).
pub fn pow43(q: u32) -> f32 {
    match pow43_table().get(q as usize) {
        Some(v) => *v,
        None => (q as f64).powf(4.0 / 3.0) as f32,
    }
}

/// `2^(0.25 * (sf - 100))`, the dequantisation gain of scalefactor `sf`.
#[inline]
pub fn sf_gain(sf: i32) -> f32 {
    use std::sync::OnceLock;
    static TABLE: OnceLock<[f32; 256]> = OnceLock::new();
    let t = TABLE.get_or_init(|| std::array::from_fn(|i| sf_gain_exact(i as i32)));
    match usize::try_from(sf).ok().and_then(|i| t.get(i)) {
        Some(g) => *g,
        None => sf_gain_exact(sf),
    }
}

fn sf_gain_exact(sf: i32) -> f32 {
    (0.25 * (sf as f64 - 100.0)).exp2() as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn band_tables_are_monotonic_and_complete() {
        for i in 0..13u8 {
            let l = swb_offsets_long(i);
            assert_eq!(*l.last().unwrap(), 1024);
            assert!(l.windows(2).all(|w| w[0] < w[1] && (w[1] - w[0]) % 4 == 0));
            let s = swb_offsets_short(i);
            assert_eq!(*s.last().unwrap(), 128);
            assert!(s.windows(2).all(|w| w[0] < w[1] && (w[1] - w[0]) % 4 == 0));
            assert!(tns_max_bands(i, false) < l.len());
            assert!(tns_max_bands(i, true) < s.len());
        }
        assert_eq!(swb_offsets_long(3).len() - 1, 49);
        assert_eq!(swb_offsets_long(5).len() - 1, 51);
        assert_eq!(swb_offsets_long(0).len() - 1, 41);
        assert_eq!(swb_offsets_long(2).len() - 1, 47);
        assert_eq!(swb_offsets_long(6).len() - 1, 47);
        assert_eq!(swb_offsets_long(8).len() - 1, 43);
        assert_eq!(swb_offsets_long(11).len() - 1, 40);
    }
}
