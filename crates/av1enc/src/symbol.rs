//! Clean-room pure-Rust boolean / multi-symbol arithmetic encoder (Daala / AV1 spec 8.2).
//!
//! Encodes symbols into an inverted, byte-aligned bitstream compatible with AV1 SymbolDecoder.

const EC_PROB_SHIFT: u32 = 6;
const EC_MIN_PROB: u32 = 4;

/// CDF adaptation (spec 8.2.6).
#[inline(always)]
pub fn update_cdf(cdf: &mut [u16], n: usize, symbol: usize) {
    let count = cdf[n];
    let rate = 3 + (count > 15) as u32 + (count > 31) as u32 + (31 - (n as u32).leading_zeros()).min(2);
    for (i, e) in cdf[..n - 1].iter_mut().enumerate() {
        let c = *e as u32;
        *e = if i < symbol { c - (c >> rate) } else { c + (((1 << 15) - c) >> rate) } as u16;
    }
    cdf[n] += (count < 32) as u16;
}

/// Arithmetic / Symbol encoder for AV1 tiles.
pub struct SymbolEncoder {
    // Current range (15-bit value, normalized so MSB is bit 15, i.e., in [1<<15, 1<<16))
    range: u32,
    // Current bit position in the stream
    bit_pos: usize,
    // Stream of inverted bits (accumulated as bytes)
    stream: Vec<u8>,
}

impl Default for SymbolEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl SymbolEncoder {
    pub fn new() -> Self {
        Self::with_capacity(64)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        let cap = capacity.max(8);
        let mut stream = Vec::with_capacity(cap);
        stream.resize(8, 0);
        Self { range: 1 << 15, bit_pos: 0, stream }
    }

    /// Write a boolean with probability P(0) = 0.5 (equal probability).
    #[inline(always)]
    pub fn write_bool(&mut self, val: bool) {
        let mut cdf = [1u16 << 14, 1 << 15, 0];
        self.write_symbol(val as usize, &mut cdf);
    }

    /// Write literal n bits.
    #[inline]
    pub fn write_literal(&mut self, val: u32, n: u32) {
        for i in (0..n).rev() {
            let bit = (val >> i) & 1 != 0;
            self.write_bool(bit);
        }
    }

    /// Write a symbol with a CDF (N+1 entries). Adapts CDF if update is true.
    #[inline]
    pub fn write_symbol(&mut self, symbol: usize, cdf: &mut [u16]) {
        let n = cdf.len().saturating_sub(1);
        self.encode_symbol(symbol, cdf, n);
        crate::symbol::update_cdf(cdf, n, symbol);
    }

    /// Write a symbol with a static / non-adapting CDF.
    #[inline(always)]
    pub fn write_symbol_no_update(&mut self, symbol: usize, cdf: &[u16]) {
        let n = cdf.len().saturating_sub(1);
        self.encode_symbol(symbol, cdf, n);
    }

    #[inline(always)]
    fn encode_symbol(&mut self, symbol: usize, cdf: &[u16], n: usize) {
        if n == 0 {
            return;
        }
        let symbol = symbol.min(n - 1);
        let cur_range = self.range;
        let fl = if symbol == 0 { 1u32 << 15 } else { (1u32 << 15) - cdf[symbol - 1] as u32 };
        let fh = (1u32 << 15) - cdf[symbol] as u32;

        let u = if symbol == 0 { cur_range } else { (((cur_range >> 8) * (fl >> EC_PROB_SHIFT)) >> (7 - EC_PROB_SHIFT)) + EC_MIN_PROB * (n - symbol) as u32 };
        let v = (((cur_range >> 8) * (fh >> EC_PROB_SHIFT)) >> (7 - EC_PROB_SHIFT)) + EC_MIN_PROB * (n - symbol - 1) as u32;

        let cur = v;
        let prev = u;
        let sym_range = prev - cur;

        self.add_value(cur as u64);

        self.range = sym_range;
        let bits = 15 - (31 - self.range.leading_zeros());
        self.range <<= bits;
        self.bit_pos += bits as usize;
    }

    fn add_value(&mut self, val: u64) {
        if val == 0 {
            return;
        }
        let end_bit = self.bit_pos + 14;
        let end_byte = end_bit / 8;
        let needed_bytes = end_byte + 1;
        if self.stream.len() < needed_bytes {
            self.stream.resize(needed_bytes, 0);
        }

        let shift = 7 - (end_bit % 8);
        let mut addend = (val as u128) << shift;

        let mut p = end_byte;
        while addend > 0 {
            let byte_add = (addend & 0xFF) as u8;
            addend >>= 8;
            let (new_val, carry) = self.stream[p].overflowing_add(byte_add);
            self.stream[p] = new_val;
            if carry {
                addend += 1;
            }
            if p == 0 {
                break;
            }
            p -= 1;
        }
    }

    /// Finalize the tile stream and return the encoded bytes.
    pub fn finish(mut self) -> Vec<u8> {
        let total_bits = self.bit_pos + 15;
        let num_bytes = total_bits.div_ceil(8);
        self.stream.truncate(num_bytes);
        // Invert bytes for AV1 SymbolDecoder
        for b in &mut self.stream {
            *b = !*b;
        }
        self.stream
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDecoder<'a> {
        data: &'a [u8],
        next: usize,
        win: u64,
        k: u32,
        range: u32,
    }

    impl<'a> TestDecoder<'a> {
        fn new(data: &'a [u8]) -> Self {
            let mut d = TestDecoder { data, next: 0, win: 0, k: 0, range: 1 << 15 };
            let mut filled = 0;
            while filled < 15 + 32 {
                d.win = (d.win << 8) | d.inverted_byte() as u64;
                filled += 8;
            }
            d.k = filled - 15;
            d
        }

        fn inverted_byte(&mut self) -> u8 {
            let b = self.data.get(self.next).copied().unwrap_or(0);
            self.next += 1;
            !b
        }

        fn read_symbol(&mut self, cdf: &[u16]) -> usize {
            let n = cdf.len() - 1;
            let value = (self.win >> self.k) as u32;
            let mut cur = self.range;
            let mut symbol = 0usize;
            let mut prev;
            loop {
                prev = cur;
                let f = (1u32 << 15) - cdf[symbol] as u32;
                cur = ((self.range >> 8) * (f >> EC_PROB_SHIFT)) >> (7 - EC_PROB_SHIFT);
                cur += EC_MIN_PROB * (n - symbol - 1) as u32;
                println!("  sym {}: cur = {}, value = {}, range = {}", symbol, cur, value, self.range);
                if value >= cur {
                    break;
                }
                symbol += 1;
            }
            self.range = prev - cur;
            self.win -= (cur as u64) << self.k;
            let bits = 15 - (31 - self.range.leading_zeros());
            self.range <<= bits;
            self.k -= bits;
            if self.k < 16 {
                while self.k <= 40 {
                    self.win = (self.win << 8) | self.inverted_byte() as u64;
                    self.k += 8;
                }
            }
            symbol
        }
    }

    #[test]
    fn test_symbol_roundtrip() {
        let mut enc = SymbolEncoder::new();
        let cdf = [16384u16, 32768, 0];
        enc.write_symbol_no_update(0, &cdf);
        enc.write_symbol_no_update(1, &cdf);
        enc.write_symbol_no_update(0, &cdf);
        let bytes = enc.finish();
        println!("Encoded {} bytes: {:?}", bytes.len(), bytes);

        let mut dec = TestDecoder::new(&bytes);
        let s0 = dec.read_symbol(&cdf);
        println!("Decoded s0: {}", s0);
        let s1 = dec.read_symbol(&cdf);
        println!("Decoded s1: {}", s1);
        let s2 = dec.read_symbol(&cdf);
        println!("Decoded s2: {}", s2);
        assert_eq!(s0, 0);
        assert_eq!(s1, 1);
        assert_eq!(s2, 0);
    }
}
