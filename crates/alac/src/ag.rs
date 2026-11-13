//! ALAC's adaptive Golomb coding of prediction residuals.
//!
//! Each residual is folded to an unsigned value (0, −1, 1, −2, 2 … → 0, 1, 2, 3, 4 …) and coded with
//! a Rice-like code whose parameter `k` follows a running mean `mb` (`k = min(⌊log2(mb/512 + 3)⌋,
//! kb)`): `q = n / (2^k − 1)` one bits and a zero, then the remainder `r` in `k − 1` zero bits when
//! `r = 0` and as `r + 1` in `k` bits otherwise. A quotient of 9 or more is an escape: nine one
//! bits and the value in full. When the mean falls low (`mb < 128`) a count of zero residuals
//! follows (a 16-bit-escaped code), and the next value is coded minus one, since a run ends at a
//! non-zero residual. After each value `mb += pb·n − pb·mb/512`.

use filmcraft_bitstream::BitWriter;

/// Where coded bits go: a [`BitWriter`], or a [`Counter`] that only measures.
pub trait Sink {
    fn put_bits(&mut self, value: u32, n: u32);
}

impl Sink for BitWriter {
    #[inline]
    fn put_bits(&mut self, value: u32, n: u32) {
        self.write_bits(value, n);
    }
}

/// Counts bits without storing them (for choosing between codings).
#[derive(Default)]
pub struct Counter(pub usize);

impl Sink for Counter {
    #[inline]
    fn put_bits(&mut self, _: u32, n: u32) {
        self.0 += n as usize;
    }
}

/// Quotient at which a value escapes to its raw bits.
const MAX_PREFIX: u32 = 9;
/// `mb` above this resets it (and the decoders disagree on the boundary; see [`encode`]).
const MEAN_CLAMP: u32 = 0xffff;
/// Longest zero run a run code may carry.
pub const MAX_RUN: u32 = 0xffff;

/// The adaptation parameters of one channel's residuals (from the magic cookie and the
/// channel's `pbFactor`).
#[derive(Clone, Copy, Debug)]
pub struct AgParams {
    /// Initial mean (`mb`, 10 in practice).
    pub mb: u32,
    /// Mean adaptation rate (`pb · pbFactor / 4`, 40 in practice).
    pub pb: u32,
    /// Largest Rice parameter (`kb`, 14 in practice).
    pub kb: u32,
}

/// One value `n` with parameter `k` (`m = 2^k − 1`); a quotient of [`MAX_PREFIX`] or more writes
/// nine ones and `n` in `escape_bits` bits.
fn put<S: Sink>(w: &mut S, n: u32, k: u32, m: u32, escape_bits: u32) {
    let q = n / m.max(1);
    if q >= MAX_PREFIX {
        w.put_bits((1 << MAX_PREFIX) - 1, MAX_PREFIX);
        // escape_bits <= 32
        if escape_bits > 16 {
            w.put_bits(n >> 16, escape_bits - 16);
            w.put_bits(n & 0xffff, 16);
        } else {
            w.put_bits(n, escape_bits);
        }
        return;
    }
    // q ones, a zero
    w.put_bits(((1u32 << q) - 1) << 1, q + 1);
    if k > 1 {
        let r = n % m;
        if r == 0 {
            w.put_bits(0, k - 1);
        } else {
            w.put_bits(r + 1, k);
        }
    }
}

/// The folded value of a residual.
#[inline]
fn fold(v: i32) -> u32 {
    if v < 0 { v.unsigned_abs() * 2 - 1 } else { v as u32 * 2 }
}

/// Floor of log2, 0 for 0.
#[inline]
fn log2(v: u32) -> u32 {
    31u32.saturating_sub(v.leading_zeros())
}

/// Code `res` (residuals within `bits`-bit signed range, `bits` <= 32). Returns `false`, having
/// written part of the block, when the block meets the one case in which decoders disagree: a
/// value of exactly 2^16 coded right after a zero run (ffmpeg resets the mean, Apple's
/// decoder does not); the caller then stores the frame uncompressed.
pub fn encode<S: Sink>(w: &mut S, res: &[i32], p: AgParams, bits: u32) -> bool {
    let mut mb = p.mb;
    let wb = (1u32 << p.kb.min(31)) - 1;
    let mut zmode = 0u32;
    let mut c = 0usize;
    let n_total = res.len();
    while c < n_total {
        let k = log2((mb >> 9) + 3).min(p.kb);
        let m = (1u32 << k) - 1;
        let folded = fold(res[c]);
        let Some(n) = folded.checked_sub(zmode) else { return false };
        if zmode == 1 && n == MEAN_CLAMP {
            return false;
        }
        put(w, n, k, m, bits);
        c += 1;
        mb = p.pb.wrapping_mul(n + zmode).wrapping_add(mb).wrapping_sub(p.pb.wrapping_mul(mb) >> 9);
        if n > MEAN_CLAMP {
            mb = MEAN_CLAMP;
        }
        zmode = 0;
        if (mb << 2) < 512 && c < n_total {
            let mut run = 0u32;
            while c + (run as usize) < n_total && res[c + run as usize] == 0 && run < MAX_RUN {
                run += 1;
            }
            // k = 7 − ⌊log2 mb⌋ + (mb + 16) / 64; mb is never 0 here (see below)
            let kz = (7 + ((mb + 16) >> 6)).saturating_sub(log2(mb)).min(31);
            let mz = ((1u32 << kz) - 1) & wb;
            put(w, run, kz, mz, 16);
            c += run as usize;
            // a run of 65535 may be followed by a zero, coded with zmode 0. Blocks hold far
            // fewer samples, so every run ends at a non-zero residual or at the block's end and
            // `mb` restarts from a non-zero value
            if run < MAX_RUN {
                zmode = 1;
            }
            mb = 0;
        }
    }
    true
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A reference reader of what [`encode`] writes (Apple's decoder rules).
    pub struct Reader<'a> {
        data: &'a [u8],
        pos: usize,
    }

    impl<'a> Reader<'a> {
        pub fn new(data: &'a [u8], pos: usize) -> Self {
            Reader { data, pos }
        }
        pub fn bit(&mut self) -> u32 {
            let b = self.data.get(self.pos / 8).map_or(0, |&v| (v >> (7 - self.pos % 8)) & 1);
            self.pos += 1;
            b as u32
        }
        pub fn bits(&mut self, n: u32) -> u32 {
            (0..n).fold(0, |a, _| (a << 1) | self.bit())
        }
        fn peek(&self, n: u32) -> u32 {
            let mut r = Reader { data: self.data, pos: self.pos };
            r.bits(n)
        }
        fn get(&mut self, k: u32, m: u32, escape: u32) -> u32 {
            let mut q = 0;
            while q < MAX_PREFIX && self.peek(1) == 1 {
                self.bit();
                q += 1;
            }
            if q >= MAX_PREFIX {
                return self.bits(escape);
            }
            self.bit();
            if k <= 1 {
                return q * m;
            }
            let v = self.peek(k);
            if v < 2 {
                self.pos += (k - 1) as usize;
                q * m
            } else {
                self.pos += k as usize;
                q * m + v - 1
            }
        }
        pub fn decode(&mut self, n_total: usize, p: AgParams, bits: u32) -> Vec<i32> {
            let mut out = Vec::new();
            let mut mb = p.mb;
            let wb = (1u32 << p.kb) - 1;
            let mut zmode = 0;
            while out.len() < n_total {
                let k = log2((mb >> 9) + 3).min(p.kb);
                let m = (1u32 << k) - 1;
                let n = self.get(k, m, bits);
                let nd = n.wrapping_add(zmode);
                out.push(if nd & 1 == 1 { -(((nd + 1) >> 1) as i32) } else { (nd >> 1) as i32 });
                mb = p.pb.wrapping_mul(nd).wrapping_add(mb).wrapping_sub(p.pb.wrapping_mul(mb) >> 9);
                if n > MEAN_CLAMP {
                    mb = MEAN_CLAMP;
                }
                zmode = 0;
                if (mb << 2) < 512 && out.len() < n_total {
                    let kz = 7 + ((mb + 16) >> 6) - log2(mb);
                    let mz = ((1 << kz) - 1) & wb;
                    let run = self.get(kz, mz, 16);
                    out.extend(std::iter::repeat_n(0, run as usize));
                    zmode = u32::from(run < MAX_RUN);
                    mb = 0;
                }
            }
            out
        }
    }

    const P: AgParams = AgParams { mb: 10, pb: 40, kb: 14 };

    #[test]
    fn codes_round_trip_with_runs_and_escapes() {
        let mut seed = 9u32;
        let mut noise = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let cases: Vec<(Vec<i32>, u32)> = vec![
            (vec![0; 100], 16),
            ((0..4096i32).map(|i| if i % 50 < 40 { 0 } else { i % 7 - 3 }).collect(), 16),
            ((0..4096).map(|_| (noise() >> 16) as i16 as i32).collect(), 17),
            ((0..4096).map(|i| if i == 2000 { -65536 } else { (noise() % 5) as i32 - 2 }).collect(), 17),
            ((0..500).map(|_| (noise() >> 8) as i32 - (1 << 23)).collect(), 24),
            ((0..64).map(|i| [i32::MIN + 1, i32::MAX, 0, -1][i % 4]).collect(), 32),
            (vec![], 16),
            (vec![7], 16),
        ];
        for (res, bits) in cases {
            let mut w = BitWriter::new();
            assert!(encode(&mut w, &res, P, bits));
            let data = w.finish();
            assert_eq!(Reader::new(&data, 0).decode(res.len(), P, bits), res, "bits {bits}");
        }
    }

    #[test]
    fn the_value_decoders_disagree_on_is_refused() {
        // a zero run, then a residual of 32768 (folded 65536, coded 65535 after the run)
        let res = [0, 0, 0, 0, 0, 0, 32768];
        let mut w = BitWriter::new();
        assert!(!encode(&mut w, &res, P, 17));
    }
}
