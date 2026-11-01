//! CABAC arithmetic encoder (ITU-T H.264 §9.3.4.2) and a matching bit-cost estimator.

use crate::tables::{NUM_CTX, RANGE_LPS, TRANS_LPS};

/// A sink for CABAC bins. Implemented by the real arithmetic coder and by the rate estimator, so the same
/// syntax-element code drives both bitstream writing and RD cost estimation.
pub trait BinSink {
    fn decision(&mut self, ctx: usize, bin: u32);
    fn bypass(&mut self, bin: u32);
    fn terminate(&mut self, bin: u32);
}

/// Context states: `pStateIdx << 1 | valMPS`.
#[derive(Clone)]
pub struct Contexts(pub [u8; NUM_CTX]);

impl Contexts {
    pub fn init(table: &[(i8, i8); NUM_CTX], slice_qp: i32) -> Self {
        let qp = slice_qp.clamp(0, 51);
        let mut s = [0u8; NUM_CTX];
        for (i, &(m, n)) in table.iter().enumerate() {
            let pre = (((m as i32) * qp) >> 4) + n as i32;
            let pre = pre.clamp(1, 126);
            s[i] = if pre <= 63 { ((63 - pre) as u8) << 1 } else { (((pre - 64) as u8) << 1) | 1 };
        }
        Contexts(s)
    }
}

/// The arithmetic encoder writing into a byte buffer.
///
/// `low` holds the 10-bit coding window plus `queue + 8` pending bits above it; whole bytes are emitted
/// as soon as they are complete. A pending byte of 0xff may still receive a carry, so runs of them are
/// counted in `outstanding` until a byte that is not 0xff resolves the carry.
#[derive(Clone)]
pub struct CabacEncoder {
    pub ctx: Contexts,
    low: u32,
    range: u32,
    queue: i32,
    outstanding: usize,
    buf: Vec<u8>,
}

impl CabacEncoder {
    /// Start encoding after a slice header; `header` must be byte aligned (cabac_alignment_one_bits written).
    pub fn new(ctx: Contexts, header: Vec<u8>) -> Self {
        // queue = -9: the first bit out of the window is the spec's discarded first bit, which is always 0.
        CabacEncoder { ctx, low: 0, range: 510, queue: -9, outstanding: 0, buf: header }
    }

    #[inline]
    fn put_byte(&mut self) {
        if self.queue >= 0 {
            let out = self.low >> (self.queue + 10);
            self.low &= (0x400 << self.queue) - 1;
            self.queue -= 8;
            if out & 0xff == 0xff {
                self.outstanding += 1;
            } else {
                let carry = out >> 8;
                if let Some(last) = self.buf.last_mut() {
                    *last = last.wrapping_add(carry as u8);
                }
                let fill = (carry as u8).wrapping_sub(1);
                self.buf.extend(std::iter::repeat_n(fill, self.outstanding));
                self.buf.push(out as u8);
                self.outstanding = 0;
            }
        }
    }

    #[inline]
    fn renorm(&mut self) {
        let shift = self.range.leading_zeros() - 23;
        self.range <<= shift;
        self.low <<= shift;
        self.queue += shift as i32;
        self.put_byte();
    }

    /// Terminate with end_of_slice_flag = 1 and flush; returns the RBSP bytes (with rbsp trailing bits).
    pub fn finish(mut self) -> Vec<u8> {
        self.terminate(1);
        // EncodeFlush: the window's top two bits, then the stop bit, then zeros to byte alignment.
        let bits = self.queue + 11;
        let pad = (8 - bits % 8) % 8;
        self.low = ((self.low >> 7) | 1) << (pad + 10);
        self.queue = bits + pad - 8;
        while self.queue >= 0 {
            self.put_byte();
        }
        self.buf.extend(std::iter::repeat_n(0xff, self.outstanding));
        self.buf
    }
}

impl BinSink for CabacEncoder {
    #[inline]
    fn decision(&mut self, ctx: usize, bin: u32) {
        let s = self.ctx.0[ctx];
        let state = (s >> 1) as usize;
        let mps = (s & 1) as u32;
        let lps = RANGE_LPS[state][((self.range >> 6) & 3) as usize] as u32;
        self.range -= lps;
        if bin != mps {
            self.low += self.range;
            self.range = lps;
            let new_mps = if state == 0 { 1 - mps } else { mps };
            self.ctx.0[ctx] = (TRANS_LPS[state] << 1) | new_mps as u8;
        } else if state < 62 {
            self.ctx.0[ctx] = (((state + 1) as u8) << 1) | mps as u8;
        }
        self.renorm();
    }

    #[inline]
    fn bypass(&mut self, bin: u32) {
        self.low <<= 1;
        if bin != 0 {
            self.low += self.range;
        }
        self.queue += 1;
        self.put_byte();
    }

    fn terminate(&mut self, bin: u32) {
        self.range -= 2;
        if bin != 0 {
            self.low += self.range;
            self.range = 2;
        }
        self.renorm();
    }
}

/// Fractional-bit cost table: `COST[state][is_mps]` in 1/256 bit units.
pub struct CostTable {
    pub cost: [[u32; 2]; 64],
}

impl CostTable {
    pub fn new() -> Self {
        let mut cost = [[0u32; 2]; 64];
        // Probability of the LPS for each state, estimated from rangeTabLPS relative to a mid-range interval.
        for (s, c) in cost.iter_mut().enumerate() {
            let avg: f64 = RANGE_LPS[s].iter().map(|&v| v as f64).sum::<f64>() / 4.0;
            let p_lps = (avg / 384.0).clamp(1e-4, 0.5);
            c[0] = (-(p_lps.log2()) * 256.0).round() as u32;
            c[1] = (-((1.0 - p_lps).log2()) * 256.0).round() as u32;
        }
        CostTable { cost }
    }
}

impl Default for CostTable {
    fn default() -> Self {
        Self::new()
    }
}

/// Rate estimator: tracks context states like the encoder and accumulates cost in 1/256 bits.
#[derive(Clone)]
pub struct CabacEstimator<'a> {
    pub ctx: Contexts,
    pub cost: u64,
    table: &'a CostTable,
}

impl<'a> CabacEstimator<'a> {
    pub fn new(ctx: Contexts, table: &'a CostTable) -> Self {
        CabacEstimator { ctx, cost: 0, table }
    }
}

impl BinSink for CabacEstimator<'_> {
    #[inline]
    fn decision(&mut self, ctx: usize, bin: u32) {
        let s = self.ctx.0[ctx];
        let state = (s >> 1) as usize;
        let mps = (s & 1) as u32;
        if bin != mps {
            self.cost += self.table.cost[state][0] as u64;
            let new_mps = if state == 0 { 1 - mps } else { mps };
            self.ctx.0[ctx] = (TRANS_LPS[state] << 1) | new_mps as u8;
        } else {
            self.cost += self.table.cost[state][1] as u64;
            if state < 62 {
                self.ctx.0[ctx] = (((state + 1) as u8) << 1) | mps as u8;
            }
        }
    }
    #[inline]
    fn bypass(&mut self, _bin: u32) {
        self.cost += 256;
    }
    fn terminate(&mut self, bin: u32) {
        self.cost += if bin != 0 { 7 * 256 } else { 2 };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reference CABAC decoder (§9.3.1.2 / 9.3.3.2) used only to round-trip-test the encoder.
    struct Dec<'a> {
        data: &'a [u8],
        pos: usize,
        range: u32,
        offset: u32,
        ctx: Contexts,
    }
    impl<'a> Dec<'a> {
        fn bit(&mut self) -> u32 {
            let b = if self.pos / 8 < self.data.len() { (self.data[self.pos / 8] >> (7 - self.pos % 8)) & 1 } else { 0 };
            self.pos += 1;
            b as u32
        }
        fn new(data: &'a [u8], ctx: Contexts) -> Self {
            let mut d = Dec { data, pos: 0, range: 510, offset: 0, ctx };
            for _ in 0..9 {
                d.offset = (d.offset << 1) | d.bit();
            }
            d
        }
        fn decision(&mut self, c: usize) -> u32 {
            let s = self.ctx.0[c];
            let state = (s >> 1) as usize;
            let mps = (s & 1) as u32;
            let lps = RANGE_LPS[state][((self.range >> 6) & 3) as usize] as u32;
            self.range -= lps;
            let bin;
            if self.offset >= self.range {
                bin = 1 - mps;
                self.offset -= self.range;
                self.range = lps;
                let new_mps = if state == 0 { 1 - mps } else { mps };
                self.ctx.0[c] = (TRANS_LPS[state] << 1) | new_mps as u8;
            } else {
                bin = mps;
                if state < 62 {
                    self.ctx.0[c] = (((state + 1) as u8) << 1) | mps as u8;
                }
            }
            while self.range < 256 {
                self.range <<= 1;
                self.offset = (self.offset << 1) | self.bit();
            }
            bin
        }
        fn bypass(&mut self) -> u32 {
            self.offset = (self.offset << 1) | self.bit();
            if self.offset >= self.range {
                self.offset -= self.range;
                1
            } else {
                0
            }
        }
        fn terminate(&mut self) -> u32 {
            self.range -= 2;
            if self.offset >= self.range {
                1
            } else {
                while self.range < 256 {
                    self.range <<= 1;
                    self.offset = (self.offset << 1) | self.bit();
                }
                0
            }
        }
    }

    fn roundtrip(n: usize, seed: u32) {
        let table = crate::tables::cabac_init_pb0();
        let ctx = Contexts::init(&table, 26);
        let mut enc = CabacEncoder::new(ctx.clone(), vec![0xa5]);
        let mut seed = seed;
        let mut rnd = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            seed >> 8
        };
        let mut ops = Vec::new();
        for _ in 0..n {
            let kind = rnd() % 10;
            let c = (rnd() % 400) as usize;
            // skewed bins so contexts adapt
            let bin = if rnd() % 100 < 80 { (c % 2) as u32 } else { 1 - (c % 2) as u32 };
            if kind < 7 {
                enc.decision(c, bin);
                ops.push((0, c, bin));
            } else if kind < 9 {
                enc.bypass(bin);
                ops.push((1, 0, bin));
            } else {
                enc.terminate(0);
                ops.push((2, 0, 0));
            }
        }
        let data = enc.finish();
        assert_eq!(data[0], 0xa5, "header byte changed");
        let mut dec = Dec::new(&data[1..], ctx);
        for (i, &(k, c, bin)) in ops.iter().enumerate() {
            let got = match k {
                0 => dec.decision(c),
                1 => dec.bypass(),
                _ => dec.terminate(),
            };
            assert_eq!(got, bin, "n {n} op {i}");
        }
        assert_eq!(dec.terminate(), 1);
        assert!(dec.pos.div_ceil(8) < data.len(), "n {n}: decoder read past the end");
    }

    #[test]
    fn roundtrip_random_bins() {
        roundtrip(20000, 12345);
    }

    #[test]
    fn roundtrip_short_streams() {
        for n in 0..300 {
            roundtrip(n, n as u32 * 7919 + 1);
        }
    }

    #[test]
    fn long_bypass_runs_and_carries() {
        let ctx = Contexts::init(&crate::tables::cabac_init_pb0(), 26);
        for bin in 0..2 {
            let mut enc = CabacEncoder::new(ctx.clone(), Vec::new());
            for _ in 0..5000 {
                enc.bypass(bin);
            }
            let data = enc.finish();
            let mut dec = Dec::new(&data, ctx.clone());
            for i in 0..5000 {
                assert_eq!(dec.bypass(), bin, "bin {bin} op {i}");
            }
            assert_eq!(dec.terminate(), 1);
        }
    }
}
