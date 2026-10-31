//! Symbol (arithmetic) decoder, spec 8.2.
//!
//! The state follows the specification literally: `SymbolValue` / `SymbolRange` are 15/16-bit
//! quantities renormalised by reading `bits` new bits at a time, and `SymbolMaxBits` counts the
//! bits still available (going negative while zero padding is consumed).

const EC_PROB_SHIFT: u32 = 6;
const EC_MIN_PROB: u32 = 4;

pub(crate) struct SymbolDecoder<'a> {
    data: &'a [u8],
    /// Next byte of `data` to load into the window.
    next: usize,
    /// SymbolValue followed by `k` look-ahead bits: the bitstream with every bit inverted
    /// (the spec's renormalisation XORs the new bits with ones), zero bits past the end of
    /// the data reading as ones. SymbolValue = win >> k.
    win: u64,
    k: u32,
    range: u32,
    pub disable_update: bool,
}

impl<'a> SymbolDecoder<'a> {
    /// `init_symbol( sz )` over `data` (the tile's bytes).
    pub fn new(data: &'a [u8], disable_update: bool) -> Self {
        let mut d = SymbolDecoder { data, next: 0, win: 0, k: 0, range: 1 << 15, disable_update };
        // SymbolValue = ((1 << 15) - 1) ^ (first 15 bits): 15 inverted bits.
        let mut filled = 0;
        while filled < 15 + 32 {
            d.win = (d.win << 8) | d.inverted_byte() as u64;
            filled += 8;
        }
        d.k = filled - 15;
        d
    }

    #[inline(always)]
    fn inverted_byte(&mut self) -> u8 {
        let b = self.data.get(self.next).copied().unwrap_or(0);
        self.next += 1;
        !b
    }

    /// `read_symbol( cdf )`: `cdf` has N + 1 entries (the last is the adaptation counter).
    #[inline]
    pub fn read_symbol(&mut self, cdf: &mut [u16]) -> usize {
        let n = cdf.len() - 1;
        let symbol = match cdf.first_chunk::<3>() {
            Some(c) if n == 4 => self.decode4(c),
            _ => self.decode(cdf, n),
        };
        if !self.disable_update {
            match cdf.first_chunk_mut::<5>() {
                Some(c) if n == 4 => update_cdf4(c, symbol),
                _ => update_cdf(cdf, n, symbol),
            }
        }
        symbol
    }

    #[inline(always)]
    fn decode(&mut self, cdf: &[u16], n: usize) -> usize {
        let value = (self.win >> self.k) as u32;
        let mut cur = self.range;
        let mut symbol = 0usize;
        let mut prev;
        loop {
            prev = cur;
            let f = (1u32 << 15) - cdf[symbol] as u32;
            cur = ((self.range >> 8) * (f >> EC_PROB_SHIFT)) >> (7 - EC_PROB_SHIFT);
            cur += EC_MIN_PROB * (n - symbol - 1) as u32;
            if value >= cur {
                break;
            }
            symbol += 1;
        }
        self.range = prev - cur;
        self.win -= (cur as u64) << self.k;
        self.renormalize();
        symbol
    }

    /// `decode` of a 4 symbol CDF (its first three entries) without a branch: the bounds never
    /// increase, so the symbol is the number of bounds above the value.
    #[inline(always)]
    fn decode4(&mut self, cdf: &[u16; 3]) -> usize {
        let value = (self.win >> self.k) as u32;
        let r8 = self.range >> 8;
        let bound = |e: u16, bias: u32| ((r8 * (((1u32 << 15) - e as u32) >> EC_PROB_SHIFT)) >> (7 - EC_PROB_SHIFT)) + bias;
        let (c0, c1, c2) = (bound(cdf[0], 3 * EC_MIN_PROB), bound(cdf[1], 2 * EC_MIN_PROB), bound(cdf[2], EC_MIN_PROB));
        let (a0, a1, a2) = (value < c0, value < c1, value < c2);
        let symbol = a0 as usize + a1 as usize + a2 as usize;
        let cur = if !a0 {
            c0
        } else if !a1 {
            c1
        } else if !a2 {
            c2
        } else {
            0
        };
        let prev = if !a0 {
            self.range
        } else if !a1 {
            c0
        } else if !a2 {
            c1
        } else {
            c2
        };
        self.range = prev - cur;
        self.win -= (cur as u64) << self.k;
        self.renormalize();
        symbol
    }

    /// Renormalisation (8.2.6): SymbolValue takes `bits` more (inverted) bits, which are
    /// already in the window.
    #[inline(always)]
    fn renormalize(&mut self) {
        let bits = 15 - (31 - self.range.leading_zeros());
        self.range <<= bits;
        self.k -= bits;
        if self.k < 16 {
            // SymbolValue < 2^16, so 16 + k <= 64 bits stay in the window.
            while self.k <= 40 {
                self.win = (self.win << 8) | self.inverted_byte() as u64;
                self.k += 8;
            }
        }
    }

    /// `read_bool()`
    #[inline]
    pub fn read_bool(&mut self) -> u32 {
        // `decode` of the CDF {1 << 14, 1 << 15} without a branch: the one bound is
        // ((range >> 8) << 7) + 4; the bit is 1 below it.
        let value = (self.win >> self.k) as u32;
        let c0 = ((self.range >> 8) << 7) + EC_MIN_PROB;
        let bit = value < c0;
        let cur = if bit { 0 } else { c0 };
        self.range = if bit { c0 } else { self.range - c0 };
        self.win -= (cur as u64) << self.k;
        self.renormalize();
        bit as u32
    }

    /// `read_literal( n )` (L(n))
    pub fn read_literal(&mut self, n: u32) -> u32 {
        let mut x = 0;
        for _ in 0..n {
            x = (x << 1) | self.read_bool();
        }
        x
    }

    /// NS(n)
    pub fn read_ns(&mut self, n: u32) -> u32 {
        if n <= 1 {
            return 0;
        }
        let w = 32 - n.leading_zeros();
        let m = (1u32 << w) - n;
        let v = self.read_literal(w - 1);
        if v < m {
            return v;
        }
        let extra = self.read_literal(1);
        (v << 1) - m + extra
    }
}

/// CDF adaptation (8.2.6).
#[inline(always)]
pub(crate) fn update_cdf(cdf: &mut [u16], n: usize, symbol: usize) {
    let count = cdf[n];
    let rate = 3 + (count > 15) as u32 + (count > 31) as u32 + (31 - (n as u32).leading_zeros()).min(2);
    // Entries below `symbol` move towards 0, the others towards 1 << 15 (the specification's
    // comparison with tmp, resolved per entry: c >= 0 and c <= 1 << 15), without branching on
    // the decoded symbol.
    for (i, e) in cdf[..n - 1].iter_mut().enumerate() {
        let c = *e as u32;
        *e = if i < symbol { c - (c >> rate) } else { c + (((1 << 15) - c) >> rate) } as u16;
    }
    cdf[n] += (count < 32) as u16;
}

/// [`update_cdf`] of a 4 symbol CDF, its three entries written out (rate 5 + (count > 15) +
/// (count > 31)).
#[inline(always)]
fn update_cdf4(cdf: &mut [u16; 5], symbol: usize) {
    let count = cdf[4];
    let rate = 5 + (count > 15) as u32 + (count > 31) as u32;
    for i in 0..3 {
        let c = cdf[i] as u32;
        let down = c - (c >> rate);
        let up = c + (((1 << 15) - c) >> rate);
        cdf[i] = if i < symbol { down } else { up } as u16;
    }
    cdf[4] = count + (count < 32) as u16;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rng(mut seed: u32) -> impl FnMut() -> u32 {
        move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        }
    }

    /// A random valid 4 symbol CDF: entries at 0, next to 32768 and in runs as well.
    fn cdf4(next: &mut impl FnMut() -> u32) -> [u16; 5] {
        let mut e = [0u16; 3].map(|_| match next() % 6 {
            0 => 0,
            1 => 32768,
            2 => 32767 - (next() % 64) as u16,
            3 => (next() % 64) as u16,
            _ => (next() % 32769) as u16,
        });
        if next().is_multiple_of(4) {
            e[1] = e[0];
        }
        e.sort_unstable();
        [e[0], e[1], e[2], 32768, (next() % 33) as u16]
    }

    #[test]
    fn branchless_decodes_match_the_search() {
        // Two decoders over the same random bytes: one through `read_symbol` / `read_bool`
        // (the branchless 4 symbol and bool paths), one through the linear search.
        for seed in 1..=40 {
            let mut next = rng(0x9e37_79b9 ^ seed);
            let data: Vec<u8> = (0..4000).map(|_| next() as u8).collect();
            let disable = seed.is_multiple_of(5);
            let (mut a, mut b) = (SymbolDecoder::new(&data, disable), SymbolDecoder::new(&data, disable));
            let mut cdfs: Vec<[u16; 5]> = (0..8).map(|_| cdf4(&mut next)).collect();
            let mut refs = cdfs.clone();
            for i in 0..20000 {
                let k = next() as usize % cdfs.len();
                let (got, want) = if next().is_multiple_of(3) {
                    (a.read_bool() as usize, b.decode(&[1 << 14, 1 << 15, 0], 2))
                } else {
                    let got = a.read_symbol(&mut cdfs[k]);
                    let want = b.decode(&refs[k], 4);
                    if !disable {
                        spec_update(&mut refs[k], want);
                    }
                    (got, want)
                };
                assert_eq!(got, want, "seed {seed} symbol {i}");
                assert_eq!((a.win, a.k, a.range, a.next), (b.win, b.k, b.range, b.next), "seed {seed} symbol {i}");
                assert_eq!(cdfs[k], refs[k], "seed {seed} symbol {i}");
            }
        }
    }

    /// 8.2.6's adaptation, written out.
    fn spec_update(cdf: &mut [u16; 5], symbol: usize) {
        let rate = 3 + (cdf[4] > 15) as u32 + (cdf[4] > 31) as u32 + 2;
        let mut tmp = 0u32;
        for i in 0..3 {
            if i == symbol {
                tmp = 1 << 15;
            }
            let c = cdf[i] as u32;
            cdf[i] = if tmp < c { c - ((c - tmp) >> rate) } else { c + ((tmp - c) >> rate) } as u16;
        }
        cdf[4] += (cdf[4] < 32) as u16;
    }
}
