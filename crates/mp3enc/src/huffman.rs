//! Huffman coding of a granule's quantized lines (ISO/IEC 11172-3 §2.4.2.7 `huffmancodebits`,
//! §2.4.3.4.6): the big-values region in pairs, split into three regions at scalefactor band
//! boundaries with a table each, then the count1 region in quadruples of magnitude ≤ 1, then the
//! zeros. The encoder picks the region split and tables that need the fewest bits.

use filmcraft_bitstream::BitWriter;

use crate::filterbank::GRANULE;
use crate::huffman_tables::{LINBITS, QUAD_A_CODES, QUAD_A_LENS, QUAD_B_CODES, pair_table};

/// The tables without `linbits`, smallest first.
const SMALL: [usize; 13] = [1, 2, 3, 5, 6, 7, 8, 9, 10, 11, 12, 13, 15];
/// Escape tables: 16-23 share table 16's codes, 24-31 table 24's.
const ESC16: usize = 16;
const ESC24: usize = 24;

/// How a granule's lines are coded, as signalled in the side information.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Coding {
    /// Pairs in the big-values region.
    pub big_values: usize,
    /// Quadruples in the count1 region.
    pub count1: usize,
    pub table_select: [usize; 3],
    pub region0_count: usize,
    pub region1_count: usize,
    /// 0: table A, 1: table B.
    pub count1_table: usize,
    /// Huffman-coded bits (part 3 of `part2_3_length`).
    pub bits: usize,
}

/// Cumulative bits per table over the scalefactor bands of the big-values region: entry `b` sums
/// bands `0..b`.
struct Prefix {
    small: [[u32; 23]; SMALL.len()],
    esc16: [u32; 23],
    esc24: [u32; 23],
    escapes: [u32; 23],
    max: [u16; 22],
}

/// Bits needed for `v` (0 for 0).
fn bit_width(v: u32) -> u32 {
    32 - v.leading_zeros()
}

impl Prefix {
    fn new(ix: &[u16], bands: &[u16; 23], end: usize) -> (Prefix, usize) {
        let mut p = Prefix { small: [[0; 23]; SMALL.len()], esc16: [0; 23], esc24: [0; 23], escapes: [0; 23], max: [0; 22] };
        let tables: Vec<_> = SMALL.iter().map(|&t| pair_table(t)).collect();
        let (t16, t24) = (pair_table(ESC16), pair_table(ESC24));
        let mut nb = 0;
        for (b, w) in bands.windows(2).enumerate() {
            let (lo, hi) = (usize::from(w[0]), usize::from(w[1]).min(end));
            if lo >= end || b >= 22 {
                break;
            }
            let lines = ix.get(lo..hi).unwrap_or(&[]);
            let max = lines.iter().copied().max().unwrap_or(0);
            p.max[b] = max;
            for (n, t) in tables.iter().enumerate() {
                let bits = match t.as_ref().filter(|t| usize::from(max) < t.size) {
                    Some(t) => lines
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|q| {
                            let (x, y) = (usize::from(q[0]), usize::from(q[1]));
                            u32::from(t.lens.get(x * t.size + y).copied().unwrap_or(32)) + u32::from(x != 0) + u32::from(y != 0)
                        })
                        .sum(),
                    None => u32::MAX / 32,
                };
                p.small[n][b + 1] = p.small[n][b].saturating_add(bits);
            }
            let (mut e16, mut e24, mut esc) = (0u32, 0u32, 0u32);
            for q in lines.as_chunks::<2>().0 {
                let (x, y) = (usize::from(q[0]), usize::from(q[1]));
                let k = x.min(15) * 16 + y.min(15);
                let signs = u32::from(x != 0) + u32::from(y != 0);
                e16 += u32::from(t16.as_ref().and_then(|t| t.lens.get(k)).copied().unwrap_or(32)) + signs;
                e24 += u32::from(t24.as_ref().and_then(|t| t.lens.get(k)).copied().unwrap_or(32)) + signs;
                esc += u32::from(x >= 15) + u32::from(y >= 15);
            }
            p.esc16[b + 1] = p.esc16[b] + e16;
            p.esc24[b + 1] = p.esc24[b] + e24;
            p.escapes[b + 1] = p.escapes[b] + esc;
            nb = b + 1;
        }
        (p, nb)
    }

    /// The cheapest table for bands `a..b`: (table_select, bits).
    fn best(&self, a: usize, b: usize) -> (usize, u32) {
        let max = self.max.get(a..b).and_then(|m| m.iter().copied().max()).unwrap_or(0);
        if max == 0 {
            return (0, 0);
        }
        let mut best = (0usize, u32::MAX);
        for (n, &t) in SMALL.iter().enumerate() {
            let sum = self.small[n][b].saturating_sub(self.small[n][a]);
            if usize::from(max) < pair_table(t).map_or(0, |p| p.size) && sum < best.1 {
                best = (t, sum);
            }
        }
        // the escape tables: the smallest linbits that hold the largest value
        let need = if max >= 15 { bit_width(u32::from(max) - 15) } else { 0 };
        let escapes = self.escapes[b] - self.escapes[a];
        for (family, base) in [(ESC16, self.esc16[b] - self.esc16[a]), (ESC24, self.esc24[b] - self.esc24[a])] {
            if let Some(t) = (family..family + 8).find(|&t| LINBITS.get(t).is_some_and(|&l| l >= need)) {
                let bits = base.saturating_add(escapes.saturating_mul(LINBITS.get(t).copied().unwrap_or(0)));
                if bits < best.1 {
                    best = (t, bits);
                }
            }
        }
        best
    }
}

/// Choose the regions and tables for `ix` (magnitudes ≤ [`crate::tables::MAX_QUANT`]) with
/// scalefactor band boundaries `bands`.
pub(crate) fn choose(ix: &[u16; GRANULE], bands: &[u16; 23]) -> Coding {
    let mut coding = Coding::default();
    let Some(last) = ix.iter().rposition(|&v| v != 0) else {
        return coding;
    };
    let n = (last + 2) & !1; // round up to a whole pair
    let mut i = n;
    while i >= 4 && ix[i - 4..i].iter().all(|&v| v <= 1) {
        i -= 4;
    }
    // count1 region [i, n)
    let (mut bits_a, mut bits_b, mut signs) = (0u32, 0u32, 0u32);
    for q in ix[i..n].as_chunks::<4>().0 {
        let idx = usize::from(q[0] * 8 + q[1] * 4 + q[2] * 2 + q[3]);
        bits_a += u32::from(QUAD_A_LENS.get(idx).copied().unwrap_or(6));
        bits_b += 4;
        signs += q.iter().map(|&v| u32::from(v)).sum::<u32>();
    }
    coding.count1 = (n - i) / 4;
    coding.count1_table = usize::from(bits_b < bits_a);
    let count1_bits = bits_a.min(bits_b) + signs;
    coding.big_values = i / 2;
    // big-values region [0, i): costs per scalefactor band, clipped at i
    let (prefix, nb) = Prefix::new(ix, bands, i);
    let mut best: Option<(u32, usize, usize, [usize; 3])> = None;
    if nb > 0 {
        for r0 in 0..16usize {
            for r1 in 0..8usize {
                if r0 + r1 + 2 > 22 {
                    continue;
                }
                let s1 = (r0 + 1).min(nb);
                let s2 = (r0 + r1 + 2).min(nb);
                let (t0, b0) = prefix.best(0, s1);
                let (t1, b1) = prefix.best(s1, s2);
                let (t2, b2) = prefix.best(s2, nb);
                let total = b0.saturating_add(b1).saturating_add(b2);
                if best.is_none_or(|b| total < b.0) {
                    best = Some((total, r0, r1, [t0, t1, t2]));
                }
                if s2 >= nb && r1 > 0 {
                    break; // longer region 1 changes nothing
                }
            }
            if r0 + 1 >= nb {
                break;
            }
        }
    }
    let (big_bits, r0, r1, tables) = best.unwrap_or((0, 0, 0, [0; 3]));
    coding.region0_count = r0;
    coding.region1_count = r1;
    coding.table_select = tables;
    coding.bits = big_bits as usize + count1_bits as usize;
    coding
}

/// Write the Huffman-coded lines; `negative[i]` is the sign of line `i`.
pub(crate) fn write(w: &mut BitWriter, ix: &[u16; GRANULE], negative: &[bool; GRANULE], c: &Coding, bands: &[u16; 23]) {
    let r1 = bands.get(c.region0_count + 1).map_or(GRANULE, |&b| usize::from(b));
    let r2 = bands.get(c.region0_count + c.region1_count + 2).map_or(GRANULE, |&b| usize::from(b));
    let end = (2 * c.big_values).min(GRANULE);
    for i in (0..end).step_by(2) {
        let select = c.table_select[if i < r1 {
            0
        } else if i < r2 {
            1
        } else {
            2
        }];
        let Some(t) = pair_table(select) else { continue };
        let linbits = LINBITS.get(select).copied().unwrap_or(0);
        let (x, y) = (usize::from(ix[i]), usize::from(ix[i + 1]));
        let (xe, ye) = if linbits > 0 { (x.min(15), y.min(15)) } else { (x, y) };
        let k = xe * t.size + ye;
        w.write_bits(u32::from(t.codes.get(k).copied().unwrap_or(0)), u32::from(t.lens.get(k).copied().unwrap_or(0)));
        if linbits > 0 && xe == 15 {
            w.write_bits((x - 15) as u32, linbits);
        }
        if x != 0 {
            w.write_bit(negative[i]);
        }
        if linbits > 0 && ye == 15 {
            w.write_bits((y - 15) as u32, linbits);
        }
        if y != 0 {
            w.write_bit(negative[i + 1]);
        }
    }
    let c1_end = (end + 4 * c.count1).min(GRANULE);
    for i in (end..c1_end).step_by(4) {
        let Some(q) = ix.get(i..i + 4) else { break };
        let idx = usize::from(q[0] * 8 + q[1] * 4 + q[2] * 2 + q[3]) & 15;
        if c.count1_table == 0 {
            w.write_bits(u32::from(QUAD_A_CODES[idx]), u32::from(QUAD_A_LENS[idx]));
        } else {
            w.write_bits(u32::from(QUAD_B_CODES[idx]), 4);
        }
        for (k, &v) in q.iter().enumerate() {
            if v != 0 {
                w.write_bit(negative[i + k]);
            }
        }
    }
}
