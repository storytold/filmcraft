//! Boolean (arithmetic) decoder of section 9.2.
//!
//! The specification keeps an 8-bit `BoolValue` and shifts in one bit per renormalisation step.
//! This implementation keeps `BoolValue` in the top 8 bits of a 64-bit window whose lower bits
//! hold the next bits of the input, which is equivalent and needs a refill only every few bytes.
//! Reading past the end of the data yields zero bits, as the specification does once
//! `BoolMaxBits` is exhausted.

pub struct BoolDecoder<'a> {
    data: &'a [u8],
    pos: usize,
    /// BoolValue in bits 56..63, followed by `count` valid input bits.
    value: u64,
    count: i32,
    range: u32,
}

impl<'a> BoolDecoder<'a> {
    /// init_bool (9.2.1): the marker bit is read and returned in `Err` position when non-zero is
    /// not treated as fatal (libvpx ignores it too), so this only returns the decoder.
    pub fn new(data: &'a [u8]) -> Self {
        let mut d = BoolDecoder { data, pos: 0, value: 0, count: -8, range: 255 };
        d.fill();
        d.read_bool(128);
        d
    }

    #[inline]
    fn fill(&mut self) {
        // Room for whole bytes below the top 8 bits and the `count` valid bits.
        let mut shift = 48 - self.count;
        if let Some(&bytes) = self.data.get(self.pos..).and_then(|d| d.first_chunk::<8>()) {
            let v = u64::from_be_bytes(bytes);
            let n = ((shift + 8) / 8) as usize; // bytes that fit
            let n = n.min(7);
            // Take the top n bytes of v.
            let take = v >> (64 - 8 * n);
            self.value |= take << (shift + 8 - 8 * n as i32);
            self.pos += n;
            self.count += 8 * n as i32;
            return;
        }
        while shift >= 0 {
            let b = if self.pos < self.data.len() {
                let b = self.data[self.pos];
                self.pos += 1;
                b
            } else {
                0
            };
            self.value |= (b as u64) << shift;
            self.count += 8;
            shift -= 8;
        }
    }

    /// read_bool (9.2.2).
    #[inline(always)]
    pub fn read_bool(&mut self, p: u8) -> bool {
        if self.count < 8 {
            self.fill();
        }
        let split = 1 + (((self.range - 1) * p as u32) >> 8);
        let bigsplit = (split as u64) << 56;
        // The outcome is close to random: select instead of branching on it.
        let bit = self.value >= bigsplit;
        let m = (bit as u64).wrapping_neg();
        self.value -= bigsplit & m;
        let m32 = m as u32;
        self.range = ((self.range - split) & m32) | (split & !m32);
        let shift = self.range.leading_zeros() - 24;
        self.range <<= shift;
        self.value <<= shift;
        self.count -= shift as i32;
        bit
    }

    /// `read_bool(128)`: the split is `1 + ((range - 1) >> 1) == (range + 1) >> 1`.
    #[inline(always)]
    pub fn read_bool_half(&mut self) -> bool {
        if self.count < 8 {
            self.fill();
        }
        let split = (self.range + 1) >> 1;
        let bigsplit = (split as u64) << 56;
        let bit = self.value >= bigsplit;
        let m = (bit as u64).wrapping_neg();
        self.value -= bigsplit & m;
        let m32 = m as u32;
        self.range = ((self.range - split) & m32) | (split & !m32);
        let shift = self.range.leading_zeros() - 24;
        self.range <<= shift;
        self.value <<= shift;
        self.count -= shift as i32;
        bit
    }

    /// read_literal (9.2.4).
    pub fn read_literal(&mut self, n: u32) -> u32 {
        let mut x = 0;
        for _ in 0..n {
            x = (x << 1) | self.read_bool(128) as u32;
        }
        x
    }

    /// Tree decoding process (9.3.3) with probabilities `probs[node]`.
    #[inline]
    pub fn read_tree(&mut self, tree: &[i8], probs: &[u8]) -> u8 {
        let mut n = 0i8;
        loop {
            n = tree[(n + self.read_bool(probs[(n >> 1) as usize]) as i8) as usize];
            if n <= 0 {
                return (-n) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Straightforward implementation of 9.2.2 for comparison.
    struct SpecDecoder<'a> {
        data: &'a [u8],
        bitpos: usize,
        value: u32,
        range: u32,
        max_bits: i64,
    }

    impl<'a> SpecDecoder<'a> {
        fn new(data: &'a [u8]) -> Self {
            let mut d = SpecDecoder { data, bitpos: 0, value: 0, range: 255, max_bits: 8 * data.len() as i64 - 8 };
            for _ in 0..8 {
                d.value = (d.value << 1) | d.bit();
            }
            d.read(128);
            d
        }
        fn bit(&mut self) -> u32 {
            let b = self.data.get(self.bitpos / 8).map(|b| (b >> (7 - self.bitpos % 8)) & 1).unwrap_or(0);
            self.bitpos += 1;
            b as u32
        }
        fn read(&mut self, p: u8) -> bool {
            let split = 1 + (((self.range - 1) * p as u32) >> 8);
            let bit = if self.value < split {
                self.range = split;
                false
            } else {
                self.range -= split;
                self.value -= split;
                true
            };
            while self.range < 128 {
                let nb = if self.max_bits > 0 {
                    self.max_bits -= 1;
                    self.bit()
                } else {
                    0
                };
                self.range *= 2;
                self.value = (self.value << 1) + nb;
            }
            bit
        }
    }

    #[test]
    fn matches_spec_model() {
        let mut seed = 0x1234_5678u32;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for len in [1usize, 2, 3, 7, 8, 9, 15, 16, 17, 100, 1000] {
            let data: Vec<u8> = (0..len).map(|_| rnd() as u8).collect();
            let mut a = BoolDecoder::new(&data);
            let mut b = SpecDecoder::new(&data);
            for i in 0..len * 12 {
                let p = (rnd() % 255 + 1) as u8;
                assert_eq!(a.read_bool(p), b.read(p), "len {len} bit {i}");
            }
        }
    }
}
