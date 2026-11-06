//! CABAC arithmetic encoder (H.265 §9.3.5, informative) plus a bit-cost estimator for mode decision.
//!
//! The engine evolves exactly like our decoder (`filmcraft_hevc::cabac`, §9.3.4.3): the same
//! `rangeTabLps` (Table 9-52), MPS/LPS transitions (Table 9-53) and renormalisation, so every
//! byte string produced here decodes there. Contexts initialise per §9.3.2.2 (`initType` 0 for
//! I slices, 1 for P slices with `cabac_init_flag = 0`).
//!
//! Bits pack MSB-first exactly like `filmcraft_bitstream::BitWriter`, but *without* emulation
//! prevention: the caller assembles the NAL unit and applies `escape_rbsp` itself.

use crate::tables::{CABAC_INIT, NUM_CTX};

/// Table 9-52 `rangeTabLps[pStateIdx][qRangeIdx]`, shared verbatim with the decoder.
#[rustfmt::skip]
const RANGE_TAB_LPS: [[u8; 4]; 64] = [
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

/// Table 9-53 `transIdxLps`, shared verbatim with the decoder.
#[rustfmt::skip]
const TRANS_LPS: [u8; 64] = [
    0, 0, 1, 2, 2, 4, 4, 5, 6, 7, 8, 9, 9, 11, 11, 12,
    13, 13, 15, 15, 16, 16, 18, 18, 19, 19, 21, 21, 22, 22, 23, 24,
    24, 25, 26, 26, 27, 27, 28, 29, 29, 30, 30, 30, 31, 32, 32, 33,
    33, 33, 34, 34, 35, 35, 35, 36, 36, 36, 37, 37, 37, 38, 38, 63,
];

/// Table 9-53 `transIdxMps`, shared verbatim with the decoder.
#[rustfmt::skip]
const TRANS_MPS: [u8; 64] = [
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
    17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32,
    33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48,
    49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 62, 63,
];

/// One CABAC context: probability state plus the more probable bin value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CtxState {
    /// Probability state `pStateIdx` (0 = LPS probability 1/2, 63 = nearly certain MPS).
    pub pstate: u8,
    /// More probable symbol `valMps` (0 or 1).
    pub mps: u8,
}

/// All slice contexts in spec Table 9-4 order; index via `crate::tables` bases (e.g. `SIG_COEFF`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CtxStates(pub [CtxState; NUM_CTX]);

impl CtxStates {
    /// Zeroed contexts; call [`CtxStates::init`] per slice before encoding.
    pub fn new() -> Self {
        CtxStates([CtxState { pstate: 0, mps: 0 }; NUM_CTX])
    }

    /// Initialise per §9.3.2.2 (Eq. 9-4..9-6); an unknown `init_type` leaves states untouched.
    pub fn init(&mut self, init_type: usize, slice_qp: u8) {
        let Some(row) = CABAC_INIT.get(init_type) else { return };
        let qp = i32::from(slice_qp.min(51));
        for (st, &init_value) in self.0.iter_mut().zip(row.iter()) {
            let slope = i32::from(init_value >> 4); // slopeIdx (Eq. 9-4)
            let offset = i32::from(init_value & 15); // offsetIdx (Eq. 9-4)
            let m = slope * 5 - 45; // Eq. 9-5, first half
            let n = (offset << 3) - 16; // Eq. 9-5, second half
            let pre = (((m * qp) >> 4) + n).clamp(1, 126); // Eq. 9-6 (`>>` is arithmetic, as in the decoder)
            if pre <= 63 {
                st.pstate = (63 - pre) as u8;
                st.mps = 0;
            } else {
                st.pstate = (pre - 64) as u8;
                st.mps = 1;
            }
        }
    }

    /// Context index for a base plus increment, saturated so hostile inputs cannot panic.
    pub fn idx(&self, base: usize, inc: usize) -> usize {
        base.saturating_add(inc).min(NUM_CTX - 1)
    }
}

impl Default for CtxStates {
    fn default() -> Self {
        Self::new()
    }
}

/// Table 9-53 state step (§9.3.4.3.2.2), shared by the encoder and the estimator.
fn transit(st: &mut CtxState, is_lps: bool) {
    let p = st.pstate.min(63); // saturate: a hand-filled state must still index the tables
    let mps = st.mps != 0;
    if is_lps {
        st.mps = u8::from(if p == 0 { !mps } else { mps }); // state 0 flips valMps on an LPS
        st.pstate = TRANS_LPS[p as usize];
    } else {
        st.mps = u8::from(mps); // normalise any nonzero fill back to 1
        st.pstate = TRANS_MPS[p as usize];
    }
}

/// CABAC encoding engine (§9.3.5.2..9.3.5.6); holds no contexts, they live in [`CtxStates`].
///
/// `encode_term` writes `end_of_slice_segment_flag = 0` after a non-final CTU, while `finish`
/// writes the final flag `1` and flushes, so a slice is `encode_*` calls plus one `finish`.
#[derive(Clone, Debug)]
pub struct CabacEncoder {
    low: u32,         // ivlLow: interval base (10-bit precision suffices per §9.3.5.2)
    range: u32,       // ivlCurrRange: interval width (9-bit precision suffices)
    outstanding: u32, // bitsOutstanding: deferred opposite-polarity bits pending a carry
    first: bool,      // firstBitFlag: the first PutBit output is absorbed, never written
    finished: bool,   // set once the terminating 1 plus flush went out; later bins are ignored
    cur: u8,          // partially filled output byte, MSB-first like BitWriter
    filled: u8,       // valid bits already packed into `cur`
    out: Vec<u8>,     // finished output bytes (still raw: the caller applies `escape_rbsp`)
}

impl CabacEncoder {
    /// Fresh engine per slice (§9.3.5.2: `ivlLow = 0`, `ivlCurrRange = 510`).
    pub fn new() -> Self {
        CabacEncoder { low: 0, range: 510, outstanding: 0, first: true, finished: false, cur: 0, filled: 0, out: Vec::new() }
    }

    /// Append one raw output bit, MSB-first (the `WriteBits(B, 1)` of Fig. 9-12).
    fn write_bit(&mut self, b: u32) {
        self.cur = (self.cur << 1) | (b & 1) as u8;
        self.filled += 1;
        if self.filled == 8 {
            self.out.push(self.cur);
            self.cur = 0;
            self.filled = 0;
        }
    }

    /// Carry-safe bit emission (Fig. 9-12: outstanding bits take the opposite polarity).
    fn put_bit(&mut self, b: u32) {
        if self.first {
            self.first = false;
        } else {
            self.write_bit(b);
        }
        while self.outstanding > 0 {
            self.outstanding -= 1;
            self.write_bit(1 - b);
        }
    }

    /// Encoder renormalisation (Fig. 9-11: shift out settled top bits until range >= 256).
    fn renorm(&mut self) {
        while self.range < 256 {
            if self.low < 256 {
                self.put_bit(0);
            } else if self.low >= 512 {
                self.low -= 512;
                self.put_bit(1);
            } else {
                self.low -= 256;
                self.outstanding += 1;
            }
            self.range <<= 1;
            self.low <<= 1;
        }
    }

    /// Encode one context-coded bin (§9.3.5.3); an out-of-range context is ignored, never panics.
    pub fn encode_bin(&mut self, states: &mut CtxStates, ctx: usize, bin: bool) {
        if self.finished {
            return;
        }
        let Some(st) = states.0.get_mut(ctx) else { return };
        let p = st.pstate.min(63) as usize;
        let is_lps = bin != (st.mps != 0);
        let lps = u32::from(RANGE_TAB_LPS[p][((self.range >> 6) & 3) as usize]); // Eq. 9-64..9-65
        self.range -= lps;
        if is_lps {
            self.low += self.range;
            self.range = lps;
        }
        transit(st, is_lps); // same Table 9-53 step the decoder applies after DecodeDecision
        self.renorm();
    }

    /// Encode one uniform-probability bin (Fig. 9-13: no renormalisation loop, single step).
    pub fn encode_bypass(&mut self, bin: bool) {
        if self.finished {
            return;
        }
        self.low <<= 1;
        if bin {
            self.low += self.range;
        }
        if self.low >= 1024 {
            self.put_bit(1);
            self.low -= 1024;
        } else if self.low < 512 {
            self.put_bit(0);
        } else {
            self.low -= 512;
            self.outstanding += 1;
        }
    }

    /// Encode `end_of_slice_segment_flag = 0` (Fig. 9-14 upper branch: shrink range, renormalise).
    pub fn encode_term(&mut self) {
        if self.finished {
            return;
        }
        self.range -= 2;
        self.renorm();
    }

    /// Encode the final `end_of_slice_segment_flag = 1` and flush (Figs 9-14..9-15).
    ///
    /// The trailing `1` doubles as `rbsp_stop_one_bit`; zeros pad to the byte. Bytes stay raw
    /// for the caller to pass through `escape_rbsp` when assembling the NAL unit.
    pub fn finish(&mut self) -> Vec<u8> {
        if !self.finished {
            self.finished = true;
            self.range -= 2;
            self.low += self.range;
            self.range = 2;
            self.renorm();
            self.put_bit((self.low >> 9) & 1);
            self.write_bit((self.low >> 8) & 1);
            self.write_bit(1);
            while self.filled != 0 {
                self.write_bit(0);
            }
        }
        self.out.clone()
    }
}

impl Default for CabacEncoder {
    fn default() -> Self {
        Self::new()
    }
}

/// A sink for bins, implemented by the encoder and the estimator so one syntax-element writer
/// drives both bitstream writing and RD cost estimation (mirrors `h264enc::cabac::BinSink`,
/// with the HEVC context table passed in because the engine holds no contexts itself).
pub trait BinSink {
    /// Context-coded bin against `states[ctx]`, adapting that context.
    fn decision(&mut self, states: &mut CtxStates, ctx: usize, bin: bool);
    /// Uniform-probability bin.
    fn bypass(&mut self, bin: bool);
    /// `end_of_slice_segment_flag`-style terminating bin (`true` ends the stream like `finish`).
    fn terminate(&mut self, bin: bool);
}

impl BinSink for CabacEncoder {
    fn decision(&mut self, states: &mut CtxStates, ctx: usize, bin: bool) {
        self.encode_bin(states, ctx, bin);
    }

    fn bypass(&mut self, bin: bool) {
        self.encode_bypass(bin);
    }

    fn terminate(&mut self, bin: bool) {
        if bin {
            let _ = self.finish();
        } else {
            self.encode_term();
        }
    }
}

/// Fractional-bit price table: `cost[pStateIdx][is_mps as usize]` in 1/64-bit units.
#[derive(Clone, Debug)]
pub struct CostTable {
    cost: [[u32; 2]; 64],
}

impl CostTable {
    /// Build from the mean `rangeTabLps` row over a mid-range interval (index 0 = LPS price).
    pub fn new() -> Self {
        let mut cost = [[0u32; 2]; 64];
        for (row, tab) in cost.iter_mut().zip(RANGE_TAB_LPS.iter()) {
            let mean = tab.iter().map(|&v| f64::from(v)).sum::<f64>() / 4.0;
            let p_lps = (mean / 384.0).clamp(1e-4, 0.5); // LPS share of the interval, kept binary
            row[0] = (-p_lps.log2() * 64.0).round() as u32;
            row[1] = (-(1.0 - p_lps).log2() * 64.0).round() as u32;
        }
        CostTable { cost }
    }

    /// Price of one bin under `pstate` in 1/64-bit units (saturates a hand-filled state).
    fn price(&self, pstate: u8, is_mps: bool) -> u32 {
        self.cost[pstate.min(63) as usize][usize::from(is_mps)]
    }
}

impl Default for CostTable {
    fn default() -> Self {
        Self::new()
    }
}

/// Rate estimator: a [`BinSink`] that prices bins and adapts `states` exactly like the encoder.
#[derive(Clone, Debug)]
pub struct CabacEstimator<'a> {
    /// Accumulated price in 1/64-bit units; read via [`CabacEstimator::bits`].
    pub cost: u64,
    table: &'a CostTable,
}

impl<'a> CabacEstimator<'a> {
    /// Start estimating against `table` (contexts live with the caller, as for the encoder).
    pub fn new(table: &'a CostTable) -> Self {
        CabacEstimator { cost: 0, table }
    }

    /// Accumulated price in whole bits (rounded down).
    pub fn bits(&self) -> u64 {
        self.cost / 64
    }
}

impl BinSink for CabacEstimator<'_> {
    fn decision(&mut self, states: &mut CtxStates, ctx: usize, bin: bool) {
        let Some(st) = states.0.get_mut(ctx) else { return };
        let is_mps = bin == (st.mps != 0);
        self.cost += u64::from(self.table.price(st.pstate, is_mps));
        transit(st, !is_mps); // identical adaptation keeps estimator states beside encoder states
    }

    fn bypass(&mut self, _bin: bool) {
        self.cost += 64; // a bypass bin always costs exactly one bit
    }

    fn terminate(&mut self, bin: bool) {
        self.cost += if bin { 640 } else { 1 }; // mean flush-plus-pad vs a renormalise-only 0
    }
}

/// One-shot price of a bin under its current state, in 1/64-bit units (unknown ctx costs 1 bit).
pub fn bin_cost(states: &CtxStates, ctx: usize, bin: bool) -> u32 {
    static TABLE: std::sync::OnceLock<CostTable> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(CostTable::new);
    match states.0.get(ctx) {
        Some(st) => table.price(st.pstate, bin == (st.mps != 0)),
        None => 64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tables::{SAO_MERGE, SAO_TYPE, SIG_COEFF, SPLIT_CU};

    #[test]
    fn init_matches_hand_computed_spec_examples() {
        // SliceQpY is exactly 26 below, so each expectation is hand arithmetic from Eq. 9-4..9-6.
        let mut s = CtxStates::new();
        s.init(0, 26);
        // SAO_MERGE (ctx 0), initValue 153 = 0x99: slopeIdx 9, offsetIdx 9, m = 9*5-45 = 0,
        // n = (9<<3)-16 = 56, pre = ((0*26)>>4)+56 = 56 <= 63, so valMps 0, pStateIdx 63-56 = 7.
        assert_eq!(s.0[SAO_MERGE], CtxState { pstate: 7, mps: 0 });
        // SPLIT_CU (ctx 2), initValue 139 = 0x8B: slopeIdx 8, offsetIdx 11, m = -5, n = 72,
        // pre = ((-5*26)>>4)+72 = (-130>>4)+72 = -9+72 = 63, so valMps 0, pStateIdx 63-63 = 0.
        assert_eq!(s.0[SPLIT_CU], CtxState { pstate: 0, mps: 0 });
        s.init(1, 26);
        // SAO_TYPE (ctx 1), initValue 185 = 0xB9: slopeIdx 11, offsetIdx 9, m = 10, n = 56,
        // pre = ((10*26)>>4)+56 = (260>>4)+56 = 16+56 = 72 > 63, so valMps 1, pStateIdx 72-64 = 8.
        assert_eq!(s.0[SAO_TYPE], CtxState { pstate: 8, mps: 1 });
    }

    #[test]
    fn end_of_slice_flag_uses_fixed_non_adapting_state() {
        // NOTE 2 under Table 9-4 pins its decode to pStateIdx 63, valMps 0 with no adaptation.
        // The terminate path carries no context, so encoding the flag adapts nothing itself.
        let mut s = CtxStates::new();
        s.init(0, 26);
        let before = s.clone();
        let mut enc = CabacEncoder::new();
        enc.encode_bin(&mut s, SPLIT_CU, true);
        enc.encode_term();
        assert_ne!(s.0[SPLIT_CU], before.0[SPLIT_CU], "sanity: a decision bin adapts");
        for (i, (got, want)) in s.0.iter().zip(before.0.iter()).enumerate() {
            if i == SPLIT_CU {
                continue;
            }
            assert_eq!(got, want, "ctx {i} adapted by encode_term");
        }
    }

    /// Minimal §9.3.4.3 decoder (decision / bypass / terminate plus renormalisation).
    struct TestDecoder<'a> {
        data: &'a [u8],
        pos: usize,
        range: u32,
        offset: u32,
        states: CtxStates,
    }

    impl<'a> TestDecoder<'a> {
        fn bit(&mut self) -> u32 {
            let b = self.data.get(self.pos / 8).map_or(0, |v| u32::from((v >> (7 - self.pos % 8)) & 1));
            self.pos += 1;
            b
        }

        fn new(data: &'a [u8], states: &CtxStates) -> Self {
            let mut d = TestDecoder { data, pos: 0, range: 510, offset: 0, states: states.clone() };
            for _ in 0..9 {
                d.offset = (d.offset << 1) | d.bit(); // §9.3.2.6: ivlOffset reads 9 bits
            }
            d
        }

        fn decision(&mut self, ctx: usize) -> bool {
            // §9.3.4.3.2: split off the LPS sub-interval, then keep whichever side holds ivlOffset.
            let st = &mut self.states.0[ctx];
            let p = st.pstate as usize;
            let mps = st.mps != 0;
            let lps = u32::from(RANGE_TAB_LPS[p][((self.range >> 6) & 3) as usize]);
            self.range -= lps;
            let bin = if self.offset >= self.range {
                self.offset -= self.range;
                self.range = lps;
                transit(st, true);
                !mps
            } else {
                transit(st, false);
                mps
            };
            while self.range < 256 {
                self.range <<= 1; // §9.3.4.3.3: shift one fresh bit into ivlOffset per doubling
                self.offset = (self.offset << 1) | self.bit();
            }
            bin
        }

        fn bypass(&mut self) -> bool {
            // §9.3.4.3.4: no renormalisation, just test ivlOffset against the unchanged range.
            self.offset = (self.offset << 1) | self.bit();
            if self.offset >= self.range {
                self.offset -= self.range;
                true
            } else {
                false
            }
        }

        fn terminate(&mut self) -> bool {
            // §9.3.4.3.5: range shrinks by 2; a set bit ends the slice with no renormalisation.
            self.range -= 2;
            if self.offset >= self.range {
                true
            } else {
                while self.range < 256 {
                    self.range <<= 1;
                    self.offset = (self.offset << 1) | self.bit();
                }
                false
            }
        }
    }

    /// Deterministic op stream: (kind, ctx, bin) with skewed bins so contexts adapt.
    fn sample_ops(seed: u32, n: usize, nctx: usize) -> Vec<(u32, usize, bool)> {
        let mut s = seed;
        let mut rnd = || {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223); // LCG, identical on every build
            s >> 8
        };
        let mut ops = Vec::with_capacity(n);
        for _ in 0..n {
            let kind = rnd() % 10;
            let ctx = (rnd() % nctx as u32) as usize;
            let bin = if rnd() % 100 < 80 { (ctx & 1) == 1 } else { (ctx & 1) == 0 };
            ops.push((kind, ctx, bin));
        }
        ops
    }

    /// One writer drives any sink, so RD search prices exactly what the encoder writes.
    fn drive<S: BinSink>(sink: &mut S, states: &mut CtxStates, ops: &[(u32, usize, bool)]) {
        for &(kind, ctx, bin) in ops {
            if kind < 7 {
                sink.decision(states, ctx, bin);
            } else if kind < 9 {
                sink.bypass(bin);
            } else {
                sink.terminate(false);
            }
        }
    }

    #[test]
    fn encode_decode_round_trip_is_a_conforming_stream() {
        let ops = sample_ops(0x1234_5678, 6000, 48);
        let mut init = CtxStates::new();
        init.init(1, 26);
        let mut enc_states = init.clone();
        let mut enc = CabacEncoder::new();
        drive(&mut enc, &mut enc_states, &ops);
        let bytes = enc.finish();
        let mut dec = TestDecoder::new(&bytes, &init);
        for (i, &(kind, ctx, bin)) in ops.iter().enumerate() {
            let got = if kind < 7 {
                dec.decision(ctx)
            } else if kind < 9 {
                dec.bypass()
            } else {
                dec.terminate()
            };
            // `drive` always writes terminate(false) mid-stream (a terminating 1 would flush and end it).
            let want = if kind == 9 { false } else { bin };
            assert_eq!(got, want, "op {i}");
        }
        assert!(dec.terminate(), "final end_of_slice_segment_flag decodes as 1");
        assert_eq!(dec.states, enc_states, "encoder and decoder adapted identically");
    }

    #[test]
    fn estimator_tracks_actual_length_within_ten_percent() {
        let table = CostTable::new();
        let ops = sample_ops(0x9E37_79B9, 6000, 40);
        let mut init = CtxStates::new();
        init.init(1, 26);
        let mut enc_states = init.clone();
        let mut enc = CabacEncoder::new();
        drive(&mut enc, &mut enc_states, &ops);
        let mut est_states = init.clone();
        let mut est = CabacEstimator::new(&table);
        drive(&mut est, &mut est_states, &ops);
        assert_eq!(est_states, enc_states, "estimator adapts beside the encoder");
        let actual = enc.finish().len() as u64 * 8; // coded bits including flush and byte pad
        let predicted = est.bits();
        let diff = actual.abs_diff(predicted);
        // Measured well under 1% on this shape; 10% + 16 bits leaves slack for short sequences.
        assert!(diff <= actual / 10 + 16, "predicted {predicted} bits vs actual {actual}");
        // The one-shot price agrees with the running estimator on the same bin.
        let mut probe = init.clone();
        let before = est.cost;
        est.decision(&mut probe, SIG_COEFF, true);
        assert_eq!(est.cost - before, u64::from(bin_cost(&init, SIG_COEFF, true)));
    }
}
