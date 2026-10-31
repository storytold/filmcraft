//! Boolean entropy decoder (RFC 6386 section 7).
//!
//! The specification keeps two bytes of the input in `value` and shifts in one byte every eight
//! renormalisation steps. This implementation keeps the active bits at the top of a 64-bit window
//! whose lower bits hold the next bytes of the input, which is equivalent and refills only every
//! few bytes. Reading past the end of the data yields zero bits.

pub struct BoolDecoder<'a> {
    data: &'a [u8],
    pos: usize,
    /// The bits being decoded, most significant first: the top 8 bits correspond to the
    /// specification's `value >> 8`, followed by `count` further bits of input.
    value: u64,
    count: i32,
    range: u32,
}

impl<'a> BoolDecoder<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        let mut d = BoolDecoder { data, pos: 0, value: 0, count: -8, range: 255 };
        d.fill();
        d
    }

    fn fill(&mut self) {
        let mut shift = 48 - self.count;
        while shift >= 0 {
            match self.data.get(self.pos) {
                Some(&b) => {
                    self.pos += 1;
                    self.value |= u64::from(b) << shift;
                    self.count += 8;
                    shift -= 8;
                }
                None => {
                    // past the end: zero bits for as long as anyone reads
                    self.count += 0x4000;
                    return;
                }
            }
        }
    }

    /// `read_bool` (7.3): a bool whose probability of being zero is `p / 256`.
    #[inline(always)]
    pub fn read_bool(&mut self, p: u8) -> bool {
        let split = 1 + (((self.range - 1) * u32::from(p)) >> 8);
        if self.count < 0 {
            self.fill();
        }
        let big_split = u64::from(split) << 56;
        let bit = if self.value >= big_split {
            self.range -= split;
            self.value -= big_split;
            true
        } else {
            self.range = split;
            false
        };
        // renormalise: shift until range >= 128
        let shift = self.range.leading_zeros().saturating_sub(24);
        self.range <<= shift;
        self.value <<= shift;
        self.count -= shift as i32;
        bit
    }

    /// `read_literal` (7.3): an `n`-bit unsigned value, most significant bit first, each bit at
    /// probability 1/2.
    #[inline]
    pub fn read_literal(&mut self, n: u32) -> u32 {
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | u32::from(self.read_bool(128));
        }
        v
    }

    /// An `L(1)` flag.
    #[inline]
    pub fn read_flag(&mut self) -> bool {
        self.read_bool(128)
    }

    /// A magnitude `L(n)` followed by a sign `L(1)` (1 = negative), as the frame header codes its
    /// signed fields.
    pub fn read_signed(&mut self, n: u32) -> i32 {
        let v = self.read_literal(n) as i32;
        if self.read_flag() { -v } else { v }
    }

    /// An optional signed field: a flag, then [`Self::read_signed`] when it is set, else 0.
    pub fn read_optional_signed(&mut self, n: u32) -> i32 {
        if self.read_flag() { self.read_signed(n) } else { 0 }
    }

    /// `treed_read` (8.1): a tree-coded value. `tree` uses the specification's layout: positive
    /// entries index deeper nodes, non-positive entries `-v` are leaves of value `v`; `probs[i]`
    /// is the probability of node `2 * i`.
    #[inline]
    pub fn read_tree(&mut self, tree: &[i8], probs: &[u8]) -> u8 {
        self.read_tree_from(tree, probs, 0)
    }

    /// [`Self::read_tree`] starting at node `start` instead of the root.
    #[inline]
    pub fn read_tree_from(&mut self, tree: &[i8], probs: &[u8], start: usize) -> u8 {
        let mut i = start;
        loop {
            let p = probs.get(i >> 1).copied().unwrap_or(128);
            let next = tree.get(i + usize::from(self.read_bool(p))).copied().unwrap_or(0);
            if next <= 0 {
                return (-next) as u8;
            }
            i = next as usize;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The encoder of section 7.3, to round-trip the decoder.
    struct BoolEncoder {
        out: Vec<u8>,
        range: u32,
        bottom: u32,
        bit_count: i32,
    }

    impl BoolEncoder {
        fn new() -> Self {
            BoolEncoder { out: Vec::new(), range: 255, bottom: 0, bit_count: 24 }
        }
        fn add_one(&mut self) {
            let mut i = self.out.len();
            while i > 0 {
                i -= 1;
                if self.out[i] == 255 {
                    self.out[i] = 0;
                } else {
                    self.out[i] += 1;
                    break;
                }
            }
        }
        fn write(&mut self, p: u8, bit: bool) {
            let split = 1 + (((self.range - 1) * u32::from(p)) >> 8);
            if bit {
                self.bottom = self.bottom.wrapping_add(split);
                self.range -= split;
            } else {
                self.range = split;
            }
            while self.range < 128 {
                self.range <<= 1;
                if self.bottom & (1 << 31) != 0 {
                    self.add_one();
                }
                self.bottom <<= 1;
                self.bit_count -= 1;
                if self.bit_count == 0 {
                    self.out.push((self.bottom >> 24) as u8);
                    self.bottom &= (1 << 24) - 1;
                    self.bit_count = 8;
                }
            }
        }
        fn finish(mut self) -> Vec<u8> {
            let mut c = self.bit_count;
            let mut v = self.bottom;
            if v & (1u32.wrapping_shl((32 - c) as u32)) != 0 {
                self.add_one();
            }
            v <<= c & 7;
            c >>= 3;
            while c > 0 {
                c -= 1;
                v <<= 8;
            }
            for _ in 0..4 {
                self.out.push((v >> 24) as u8);
                v <<= 8;
            }
            self.out
        }
    }

    #[test]
    fn round_trips_bools_at_every_probability() {
        let mut seed = 0x1234_5678u32;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let bits: Vec<(u8, bool)> = (0..20_000).map(|_| (((rnd() % 255) + 1) as u8, rnd() % 3 == 0)).collect();
        let mut e = BoolEncoder::new();
        for &(p, b) in &bits {
            e.write(p, b);
        }
        let data = e.finish();
        let mut d = BoolDecoder::new(&data);
        for (i, &(p, b)) in bits.iter().enumerate() {
            assert_eq!(d.read_bool(p), b, "bool {i}");
        }
    }

    #[test]
    fn reads_zeros_past_the_end() {
        let mut d = BoolDecoder::new(&[]);
        for _ in 0..1000 {
            assert!(!d.read_bool(128));
        }
    }
}
