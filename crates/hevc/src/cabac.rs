//! CABAC arithmetic decoding engine (9.3.4.3) and context initialisation (9.3.2.2).

use crate::error::{Result, ensure};
use crate::spec_tables::{CABAC_INIT, NUM_CTX};

/// Table 9-52 (same values as H.264) rangeTabLPS[pStateIdx][qCodIRangeIdx].
#[rustfmt::skip]
pub(crate) static RANGE_TAB_LPS: [[u8; 4]; 64] = [
    [128, 176, 208, 240], [128, 167, 197, 227], [128, 158, 187, 216], [123, 150, 178, 205],
    [116, 142, 169, 195], [111, 135, 160, 185], [105, 128, 152, 175], [100, 122, 144, 166],
    [95, 116, 137, 158], [90, 110, 130, 150], [85, 104, 123, 142], [81, 99, 117, 135],
    [77, 94, 111, 128], [73, 89, 105, 122], [69, 85, 100, 116], [66, 80, 95, 110],
    [62, 76, 90, 104], [59, 72, 86, 99], [56, 69, 81, 94], [53, 65, 77, 89],
    [51, 62, 73, 85], [48, 59, 69, 80], [46, 56, 66, 76], [43, 53, 63, 72],
    [41, 50, 59, 69], [39, 48, 56, 65], [37, 45, 54, 62], [35, 43, 51, 59],
    [33, 41, 48, 56], [32, 39, 46, 53], [30, 37, 43, 50], [29, 35, 41, 48],
    [27, 33, 39, 45], [26, 31, 37, 43], [24, 30, 35, 41], [23, 28, 33, 39],
    [22, 27, 32, 37], [21, 26, 30, 35], [20, 24, 29, 33], [19, 23, 27, 31],
    [18, 22, 26, 30], [17, 21, 25, 28], [16, 20, 23, 27], [15, 19, 22, 25],
    [14, 18, 21, 24], [14, 17, 20, 23], [13, 16, 19, 22], [12, 15, 18, 21],
    [12, 14, 17, 20], [11, 14, 16, 19], [11, 13, 15, 18], [10, 12, 15, 17],
    [10, 12, 14, 16], [9, 11, 13, 15], [9, 11, 12, 14], [8, 10, 12, 14],
    [8, 9, 11, 13], [7, 9, 11, 12], [7, 9, 10, 12], [7, 8, 10, 11],
    [6, 8, 9, 11], [6, 7, 9, 10], [6, 7, 8, 9], [2, 2, 2, 2],
];

/// Table 9-53 transIdxLPS.
#[rustfmt::skip]
static TRANS_IDX_LPS: [u8; 64] = [
    0, 0, 1, 2, 2, 4, 4, 5, 6, 7, 8, 9, 9, 11, 11, 12,
    13, 13, 15, 15, 16, 16, 18, 18, 19, 19, 21, 21, 22, 22, 23, 24,
    24, 25, 26, 26, 27, 27, 28, 29, 29, 30, 30, 30, 31, 32, 32, 33,
    33, 33, 34, 34, 35, 35, 35, 36, 36, 36, 37, 37, 37, 38, 38, 63,
];

/// Table 9-53 transIdxMPS.
#[rustfmt::skip]
static TRANS_IDX_MPS: [u8; 64] = [
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
    17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32,
    33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48,
    49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 62, 63,
];

/// Combined state transition: index = (pStateIdx << 1 | valMPS) * 2 + bin_is_lps.
/// Each context is stored as `pStateIdx << 1 | valMPS`.
pub(crate) static NEXT_STATE: [[u8; 2]; 128] = {
    let mut t = [[0u8; 2]; 128];
    let mut s = 0;
    while s < 128 {
        let p = s >> 1;
        let mps = s & 1;
        // MPS path
        t[s][0] = (TRANS_IDX_MPS[p] << 1) | mps as u8;
        // LPS path
        let new_mps = if p == 0 { 1 - mps } else { mps };
        t[s][1] = (TRANS_IDX_LPS[p] << 1) | new_mps as u8;
        s += 1;
    }
    t
};

/// Context states, padded to a power of two so that the per-bin index needs no bounds check.
pub type Contexts = [u8; CTX_SLOTS];
const CTX_SLOTS: usize = NUM_CTX.next_power_of_two();

/// Initialise all context variables for `slice_qp` and `init_type` (9.3.2.2). Each context is stored as
/// `pStateIdx << 1 | valMps`.
pub fn init_contexts(ctx: &mut Contexts, slice_qp: i32, init_type: usize) {
    let qp = slice_qp.clamp(0, 51);
    for (c, &v) in ctx.iter_mut().zip(CABAC_INIT[init_type].iter()) {
        let slope = (v >> 4) as i32;
        let offset = (v & 15) as i32;
        let m = slope * 5 - 45;
        let n = (offset << 3) - 16;
        let pre = (((m * qp) >> 4) + n).clamp(1, 126);
        *c = if pre <= 63 { ((63 - pre) << 1) as u8 } else { (((pre - 64) << 1) | 1) as u8 };
    }
}

/// Engine state and contexts of a [`Cabac`] decoder, to continue decoding the same data later
/// ([`Cabac::resume`]).
#[derive(Clone)]
pub struct CabacState {
    next_byte: usize,
    range: u32,
    value: u64,
    bits: u32,
    ctx: Contexts,
}

pub struct Cabac<'a> {
    data: &'a [u8],
    /// Byte position of the next byte to load into `value`.
    next_byte: usize,
    range: u32,
    /// ivlOffset scaled by 2^bits: value = (ivlOffset << bits) | (bits already loaded but not yet
    /// consumed by the arithmetic decoder).
    value: u64,
    bits: u32,
    pub ctx: Contexts,
}

impl<'a> Cabac<'a> {
    /// Create an engine over `data` starting at byte `pos`, with contexts initialised for `slice_qp`
    /// and `init_type`.
    pub fn new(data: &'a [u8], pos: usize, slice_qp: i32, init_type: usize) -> Result<Self> {
        let mut c = Cabac { data, next_byte: pos, range: 0, value: 0, bits: 0, ctx: [0; CTX_SLOTS] };
        init_contexts(&mut c.ctx, slice_qp, init_type);
        c.init_engine()?;
        Ok(c)
    }

    /// The engine state and contexts, for [`Cabac::resume`].
    pub fn save(&self) -> CabacState {
        CabacState { next_byte: self.next_byte, range: self.range, value: self.value, bits: self.bits, ctx: self.ctx }
    }

    /// Continue decoding `data` where [`Cabac::save`] left off.
    pub fn resume(data: &'a [u8], s: &CabacState) -> Self {
        Cabac { data, next_byte: s.next_byte, range: s.range, value: s.value, bits: s.bits, ctx: s.ctx }
    }

    /// 9.3.2.6: ivlCurrRange = 510, ivlOffset = read_bits(9) (from the current byte-aligned position).
    pub fn init_engine(&mut self) -> Result<()> {
        self.range = 510;
        self.value = 0;
        self.bits = 0;
        self.refill();
        self.bits -= 9;
        ensure!((self.value >> self.bits) < 510, "invalid CABAC offset at init");
        Ok(())
    }

    /// Load 32 more bits (zeros past the end of the data).
    #[inline(always)]
    fn refill(&mut self) {
        let b = self.next_byte;
        let mut w = [0u8; 4];
        if let Some(src) = self.data.get(b..b + 4) {
            w.copy_from_slice(src);
        } else if b < self.data.len() {
            let n = self.data.len() - b;
            w[..n].copy_from_slice(&self.data[b..]);
        }
        self.next_byte += 4;
        self.value = (self.value << 32) | u32::from_be_bytes(w) as u64;
        self.bits += 32;
    }

    /// Bit position of the next bit not consumed by the arithmetic decoder (for PCM samples and
    /// substream ends after decode_terminate returned 1).
    pub fn bit_pos(&self) -> usize {
        self.next_byte * 8 - self.bits as usize
    }
    /// Continue reading at byte `pos` (call `init_engine` afterwards).
    pub fn set_byte_pos(&mut self, pos: usize) {
        self.next_byte = pos;
        self.value = 0;
        self.bits = 0;
    }
    pub fn data(&self) -> &'a [u8] {
        self.data
    }
    /// True when the reader has run past the end of the data (corrupt stream).
    pub fn overrun(&self) -> bool {
        self.next_byte > self.data.len() + 16
    }

    #[inline(always)]
    fn renorm(&mut self) {
        // ivlCurrRange < 512 always, so the shift is 0 when no renormalisation is needed: no
        // branch on the (unpredictable) range, only on the rare refill.
        let shift = self.range.leading_zeros() - 23;
        self.range <<= shift;
        if self.bits < shift {
            self.refill();
        }
        self.bits -= shift;
    }

    /// 9.3.4.3.2, branch-free: the MPS / LPS outcome of a context-coded bin is close to random
    /// at high bit rates, so values are selected instead of jumping.
    #[inline(always)]
    pub fn decode_decision(&mut self, ctx_idx: usize) -> u32 {
        let ctx_idx = ctx_idx & (CTX_SLOTS - 1);
        let s = (self.ctx[ctx_idx] & 127) as usize;
        let q = ((self.range >> 6) & 3) as usize;
        let lps = RANGE_TAB_LPS[s >> 1][q] as u32;
        let mps_range = self.range - lps;
        let scaled = (mps_range as u64) << self.bits;
        let is_lps = self.value >= scaled;
        self.value -= scaled & (is_lps as u64).wrapping_neg();
        self.range = if is_lps { lps } else { mps_range };
        self.ctx[ctx_idx] = NEXT_STATE[s][is_lps as usize];
        self.renorm();
        (s & 1) as u32 ^ is_lps as u32
    }

    #[inline(always)]
    pub fn decode_bypass(&mut self) -> u32 {
        if self.bits == 0 {
            self.refill();
        }
        self.bits -= 1;
        let scaled = (self.range as u64) << self.bits;
        let one = self.value >= scaled;
        self.value -= scaled & (one as u64).wrapping_neg();
        one as u32
    }

    /// `n` (<= 16) bypass bins at once, most significant first. The bins of a run of bypass
    /// decodes are the binary digits of floor(ivlOffset / ivlCurrRange) in the bit-scaled
    /// representation (no renormalisation happens between them), so one division replaces `n`
    /// compare-subtract steps.
    #[inline(always)]
    pub fn decode_bypass_bits(&mut self, n: u32) -> u32 {
        debug_assert!(n <= 16);
        if self.bits < n {
            self.refill();
        }
        let s = self.bits - n;
        let r = ((self.value >> s) / self.range as u64) as u32;
        self.value -= ((r as u64) * self.range as u64) << s;
        self.bits = s;
        r
    }

    pub fn decode_terminate(&mut self) -> u32 {
        self.range -= 2;
        let scaled = (self.range as u64) << self.bits;
        if self.value >= scaled {
            1
        } else {
            self.renorm();
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal CABAC encoder (9.3.4, informative) used to round-trip test the decoder.
    struct Enc {
        low: u32,
        range: u32,
        outstanding: u32,
        first: bool,
        bits: Vec<bool>,
        ctx: Vec<u8>,
    }
    impl Enc {
        fn new(ctx: &[u8]) -> Self {
            Enc { low: 0, range: 510, outstanding: 0, first: true, bits: vec![], ctx: ctx.to_vec() }
        }
        fn put(&mut self, b: bool) {
            if self.first {
                self.first = false;
            } else {
                self.bits.push(b);
            }
            while self.outstanding > 0 {
                self.bits.push(!b);
                self.outstanding -= 1;
            }
        }
        fn renorm(&mut self) {
            while self.range < 256 {
                if self.low < 256 {
                    self.put(false);
                } else if self.low >= 512 {
                    self.low -= 512;
                    self.put(true);
                } else {
                    self.low -= 256;
                    self.outstanding += 1;
                }
                self.range <<= 1;
                self.low <<= 1;
            }
        }
        fn encode(&mut self, ctx_idx: usize, bin: u32) {
            let s = self.ctx[ctx_idx] as usize;
            let p = s >> 1;
            let mps = (s & 1) as u32;
            let q = ((self.range >> 6) & 3) as usize;
            let lps = RANGE_TAB_LPS[p][q] as u32;
            self.range -= lps;
            if bin != mps {
                self.low += self.range;
                self.range = lps;
                self.ctx[ctx_idx] = NEXT_STATE[s][1];
            } else {
                self.ctx[ctx_idx] = NEXT_STATE[s][0];
            }
            self.renorm();
        }
        fn bypass(&mut self, bin: u32) {
            self.low <<= 1;
            if bin != 0 {
                self.low += self.range;
            }
            if self.low >= 1024 {
                self.put(true);
                self.low -= 1024;
            } else if self.low < 512 {
                self.put(false);
            } else {
                self.low -= 512;
                self.outstanding += 1;
            }
        }
        fn terminate_flush(&mut self) -> Vec<u8> {
            // encode terminate bin = 1 then flush (9.3.4.5)
            self.range -= 2;
            self.low += self.range;
            self.range = 2;
            self.renorm();
            self.put((self.low >> 9) & 1 != 0);
            self.bits.push((self.low >> 8) & 1 != 0);
            self.bits.push(true); // rbsp_stop_one_bit
            while !self.bits.len().is_multiple_of(8) {
                self.bits.push(false);
            }
            self.bits.chunks(8).map(|c| c.iter().fold(0u8, |a, &b| (a << 1) | b as u8)).collect()
        }
    }

    #[test]
    fn context_init_formula() {
        let mut ctx: Contexts = [0u8; CTX_SLOTS];
        // initValue 154 is the "equiprobable" value: m = 0, n = 64 -> pStateIdx 0, valMps 1
        init_contexts(&mut ctx, 30, 0);
        assert_eq!(ctx[crate::spec_tables::CU_TRANSQUANT_BYPASS], 1);
        // split_cu_flag ctx 0 (initValue 139): slope 8, offset 11 -> m = -5, n = 72; QP 30: (-150 >> 4) + 72 = 62
        assert_eq!(ctx[crate::spec_tables::SPLIT_CU], (63 - 62) << 1);
    }

    #[test]
    fn round_trip_random_bins() {
        let mut seed = 12345u32;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let dummy = Cabac::new(&[0, 0, 0, 0], 0, 30, 1).unwrap();
        let mut enc = Enc::new(&dummy.ctx);
        let mut ops = Vec::new();
        for _ in 0..5000 {
            let kind = rnd() % 4;
            let ctx = (rnd() % 16) as usize;
            // skewed bins so contexts adapt
            let bin = if rnd() % 10 < 8 { (ctx & 1) as u32 } else { 1 - (ctx & 1) as u32 };
            if kind == 0 {
                enc.bypass(bin);
            } else {
                enc.encode(ctx, bin);
            }
            ops.push((kind, ctx, bin));
        }
        let bytes = enc.terminate_flush();
        let mut dec = Cabac::new(&bytes, 0, 30, 1).unwrap();
        for (i, &(kind, ctx, bin)) in ops.iter().enumerate() {
            let got = if kind == 0 { dec.decode_bypass() } else { dec.decode_decision(ctx) };
            assert_eq!(got, bin, "bin {i}");
        }
        assert_eq!(dec.decode_terminate(), 1);
    }
}
