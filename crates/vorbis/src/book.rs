//! Bit packing (Vorbis I §2) and codebooks (§3): Huffman lengths, the codeword assignment the
//! decoder derives from them, VQ lattices (lookup type 1) and the header encoding.

use crate::{Error, Result};

/// LSB-first bit packer (Vorbis I §2.1.4): bits fill each byte from its least significant end.
#[derive(Clone, Debug, Default)]
pub struct BitWriter {
    buf: Vec<u8>,
    bits: u64,
}

impl BitWriter {
    pub fn new() -> BitWriter {
        BitWriter::default()
    }

    /// Writes the low `n` bits of `v` (`n` ≤ 32).
    pub fn write(&mut self, v: u32, n: u32) {
        let n = n.min(32);
        for i in 0..n {
            let bit = (v >> i) & 1;
            let at = (self.bits / 8) as usize;
            if at == self.buf.len() {
                self.buf.push(0);
            }
            if bit != 0
                && let Some(b) = self.buf.get_mut(at)
            {
                *b |= 1 << (self.bits % 8);
            }
            self.bits += 1;
        }
    }

    pub fn flag(&mut self, b: bool) {
        self.write(b as u32, 1);
    }

    /// Writes a codeword MSB first: the decoder walks the tree from its first bit.
    fn codeword(&mut self, word: u32, len: u32) {
        for i in (0..len).rev() {
            self.write((word >> i) & 1, 1);
        }
    }

    pub fn bytes(&mut self, data: &[u8]) {
        for &b in data {
            self.write(u32::from(b), 8);
        }
    }

    pub fn finish(self) -> Vec<u8> {
        self.buf
    }
}

/// `ilog` of the specification (§9.2.1): the position of the highest set bit, 0 for 0.
pub fn ilog(v: u32) -> u32 {
    32 - v.leading_zeros()
}

/// The 32-bit float format of codebook headers (§9.2.2), for values it represents exactly.
pub fn float32_pack(v: f64) -> u32 {
    if v == 0.0 || !v.is_finite() {
        return 0;
    }
    let sign = if v < 0.0 { 0x8000_0000u32 } else { 0 };
    let mut m = v.abs();
    let mut exp: i32 = 0;
    // a 21-bit mantissa: scale into [2^20, 2^21)
    while m >= (1 << 21) as f64 {
        m /= 2.0;
        exp += 1;
    }
    while m < (1 << 20) as f64 {
        m *= 2.0;
        exp -= 1;
    }
    let mant = (m.round() as u32).min((1 << 21) - 1);
    let biased = (exp + 788).clamp(0, 1023) as u32;
    sign | (biased << 21) | mant
}

/// Huffman code lengths (1..=`max_len`) for `weights`, every entry coded.
pub fn huffman_lengths(weights: &[f64], max_len: u32) -> Vec<u32> {
    let n = weights.len();
    if n <= 1 {
        return vec![1; n];
    }
    let total: f64 = weights.iter().map(|w| if w.is_finite() { w.max(0.0) } else { 0.0 }).sum::<f64>().max(1e-300);
    let mut floor = 1e-9;
    loop {
        let w: Vec<f64> = weights.iter().map(|&x| (if x.is_finite() { x.max(0.0) } else { 0.0 }) / total + floor).collect();
        let lens = huffman(&w);
        if lens.iter().all(|&l| l <= max_len) || floor > 1.0 {
            return lens.into_iter().map(|l| l.clamp(1, max_len)).collect();
        }
        floor *= 4.0;
    }
}

fn huffman(w: &[f64]) -> Vec<u32> {
    // nodes: (weight, children) with leaves first
    let n = w.len();
    let mut parent = vec![usize::MAX; 2 * n];
    let mut weight: Vec<f64> = w.to_vec();
    let mut alive: Vec<usize> = (0..n).collect();
    while alive.len() > 1 {
        alive.sort_by(|&a, &b| weight[b].partial_cmp(&weight[a]).unwrap_or(std::cmp::Ordering::Equal).then(b.cmp(&a)));
        let (Some(a), Some(b)) = (alive.pop(), alive.pop()) else { break };
        let id = weight.len();
        weight.push(weight[a] + weight[b]);
        if parent.len() <= id {
            parent.resize(id + 1, usize::MAX);
        }
        parent[a] = id;
        parent[b] = id;
        alive.push(id);
    }
    (0..n)
        .map(|i| {
            let mut d = 0;
            let mut x = i;
            while let Some(&p) = parent.get(x) {
                if p == usize::MAX {
                    break;
                }
                d += 1;
                x = p;
            }
            d
        })
        .collect()
}

/// The codewords the decoder assigns to `lengths` (§3.2.1): in entry order, each entry gets the
/// lowest available codeword of its length (no prefix of an assigned codeword, and no assigned
/// codeword a prefix of it). An error when the lengths do not fit a prefix code.
pub fn assign_codewords(lengths: &[u32]) -> Result<Vec<u32>> {
    // A trie of the assigned codewords: `children[node]` (0 = none), `leaf[node]`.
    let mut children: Vec<[usize; 2]> = vec![[0, 0]];
    let mut leaf = vec![false];
    let mut words = Vec::with_capacity(lengths.len());
    for &len in lengths {
        if len == 0 || len > 32 {
            return Err(Error::Internal("codeword length"));
        }
        let bits = lowest_free(&children, &leaf, 0, len).ok_or(Error::Internal("over-subscribed codebook"))?;
        let mut node = 0usize;
        let mut word = 0u32;
        for &bit in &bits {
            word = (word << 1) | bit as u32;
            node = match children[node][bit] {
                0 => {
                    children.push([0, 0]);
                    leaf.push(false);
                    let id = children.len() - 1;
                    children[node][bit] = id;
                    id
                }
                c => c,
            };
        }
        leaf[node] = true;
        words.push(word);
    }
    Ok(words)
}

/// The leftmost free path of `depth` bits below `node` (depth-first, 0 before 1).
fn lowest_free(children: &[[usize; 2]], leaf: &[bool], node: usize, depth: u32) -> Option<Vec<usize>> {
    if leaf[node] || depth == 0 {
        return None;
    }
    for bit in 0..2 {
        let c = children[node][bit];
        if c == 0 {
            let mut p = vec![bit];
            p.resize(depth as usize, 0);
            return Some(p);
        }
        if let Some(mut rest) = lowest_free(children, leaf, c, depth - 1) {
            rest.insert(0, bit);
            return Some(rest);
        }
    }
    None
}

/// A codebook: entry lengths and codewords, and for VQ books a lattice (lookup type 1).
#[derive(Clone, Debug)]
pub struct Codebook {
    pub dimensions: u32,
    pub lengths: Vec<u32>,
    pub words: Vec<u32>,
    /// Lattice values per dimension (lookup type 1: `values^dimensions == entries`).
    pub lattice: Option<Lattice>,
}

#[derive(Clone, Copy, Debug)]
pub struct Lattice {
    pub minimum: f64,
    pub delta: f64,
    /// Values per dimension.
    pub values: u32,
}

impl Codebook {
    /// A scalar book (no lookup) of `weights.len()` entries.
    pub fn scalar(dimensions: u32, weights: &[f64]) -> Result<Codebook> {
        let lengths = huffman_lengths(weights, 20);
        let words = assign_codewords(&lengths)?;
        Ok(Codebook { dimensions, lengths, words, lattice: None })
    }

    /// A lattice VQ book: `values` integers per dimension from `minimum` in steps of `delta`;
    /// `weight(v)` is the relative probability of one component `v`.
    pub fn lattice(dimensions: u32, values: u32, minimum: i32, delta: i32, weight: impl Fn(i32) -> f64) -> Result<Codebook> {
        let entries = values.checked_pow(dimensions).ok_or(Error::Internal("codebook size"))?;
        let w: Vec<f64> = (0..entries)
            .map(|e| {
                let mut p = 1.0;
                let mut rest = e;
                for _ in 0..dimensions {
                    let t = (rest % values) as i32;
                    rest /= values;
                    p *= weight(minimum + t * delta);
                }
                p
            })
            .collect();
        let lengths = huffman_lengths(&w, 20);
        let words = assign_codewords(&lengths)?;
        Ok(Codebook { dimensions, lengths, words, lattice: Some(Lattice { minimum: f64::from(minimum), delta: f64::from(delta), values }) })
    }

    pub fn entries(&self) -> usize {
        self.lengths.len()
    }

    /// Writes entry `e`.
    pub fn put(&self, w: &mut BitWriter, e: usize) {
        if let (Some(&word), Some(&len)) = (self.words.get(e), self.lengths.get(e)) {
            w.codeword(word, len);
        }
    }

    /// The entry of the lattice vector `v` (component 0 is the least significant digit, §3.2.1).
    pub fn entry_of(&self, v: &[i32]) -> usize {
        let Some(l) = self.lattice else { return 0 };
        let mut e = 0usize;
        let mut scale = 1usize;
        for &x in v.iter().take(self.dimensions as usize) {
            let t = ((f64::from(x) - l.minimum) / l.delta).round().clamp(0.0, f64::from(l.values - 1)) as usize;
            e += t * scale;
            scale *= l.values as usize;
        }
        e
    }

    /// The header encoding (§3.2.1).
    pub fn write_header(&self, w: &mut BitWriter) {
        w.write(0x56_4342, 24);
        w.write(self.dimensions, 16);
        w.write(self.entries() as u32, 24);
        w.flag(false); // not ordered
        w.flag(false); // not sparse
        for &l in &self.lengths {
            w.write(l - 1, 5);
        }
        match self.lattice {
            None => w.write(0, 4),
            Some(l) => {
                w.write(1, 4);
                w.write(float32_pack(l.minimum), 32);
                w.write(float32_pack(l.delta), 32);
                let bits = ilog(l.values - 1).max(1);
                w.write(bits - 1, 4);
                w.flag(false); // sequence_p
                for m in 0..l.values {
                    w.write(m, bits);
                }
            }
        }
    }
}
