//! Huffman coding of spectral values and scalefactors (ISO/IEC 14496-3 §4.6.3, Annex 4.A).

use std::sync::OnceLock;

use filmcraft_bitstream::{BitReader, BitWriter};

use crate::huffman_tables::{
    SCALEFACTOR, SPECTRUM_1, SPECTRUM_2, SPECTRUM_3, SPECTRUM_4, SPECTRUM_5, SPECTRUM_6, SPECTRUM_7, SPECTRUM_8, SPECTRUM_9, SPECTRUM_10, SPECTRUM_11,
};
use crate::{Error, Result};

/// `ZERO_HCB`: band quantised to zero, nothing transmitted.
pub const ZERO_HCB: u8 = 0;
/// `ESC_HCB`: codebook 11 with escape sequences.
pub const ESC_HCB: u8 = 11;
/// `NOISE_HCB`: perceptual noise substitution.
pub const NOISE_HCB: u8 = 13;
/// `INTENSITY_HCB2`: intensity stereo, out of phase.
pub const INTENSITY_HCB2: u8 = 14;
/// `INTENSITY_HCB`: intensity stereo, in phase.
pub const INTENSITY_HCB: u8 = 15;

/// Parameters of one spectrum codebook.
pub struct Book {
    /// Values per codeword (4 = quad, 2 = pair).
    pub dim: usize,
    /// Signed values in the codeword (no separate sign bits).
    pub signed: bool,
    /// Largest absolute value (16 = escape for codebook 11).
    pub lav: u32,
    /// Radix of the index (`2*lav+1` signed, `lav+1` unsigned).
    pub modulo: u32,
    pub codes: &'static [(u32, u8)],
}

static BOOKS: [Book; 11] = [
    Book { dim: 4, signed: true, lav: 1, modulo: 3, codes: &SPECTRUM_1 },
    Book { dim: 4, signed: true, lav: 1, modulo: 3, codes: &SPECTRUM_2 },
    Book { dim: 4, signed: false, lav: 2, modulo: 3, codes: &SPECTRUM_3 },
    Book { dim: 4, signed: false, lav: 2, modulo: 3, codes: &SPECTRUM_4 },
    Book { dim: 2, signed: true, lav: 4, modulo: 9, codes: &SPECTRUM_5 },
    Book { dim: 2, signed: true, lav: 4, modulo: 9, codes: &SPECTRUM_6 },
    Book { dim: 2, signed: false, lav: 7, modulo: 8, codes: &SPECTRUM_7 },
    Book { dim: 2, signed: false, lav: 7, modulo: 8, codes: &SPECTRUM_8 },
    Book { dim: 2, signed: false, lav: 12, modulo: 13, codes: &SPECTRUM_9 },
    Book { dim: 2, signed: false, lav: 12, modulo: 13, codes: &SPECTRUM_10 },
    Book { dim: 2, signed: false, lav: 16, modulo: 17, codes: &SPECTRUM_11 },
];

/// Spectrum codebook 1..=11.
#[inline]
pub fn book(cb: u8) -> &'static Book {
    &BOOKS[(cb as usize).clamp(1, 11) - 1]
}

/// Bits resolved by the first table level.
const L1: u32 = 9;
/// Flag of a first-level entry that points to a sub-table.
const SUB: u32 = 1 << 31;

/// Two-level decoding table for one prefix code. A leaf entry is `len << 24 | symbol`; a first-level
/// entry with `SUB` set is `SUB | offset << 5 | width` and selects a sub-table of `1 << width`
/// leaves indexed by the next `width` bits. An entry of 0 is a bit pattern that no codeword starts
/// with (the codebooks are complete, so this does not occur).
struct Lut {
    t: Vec<u32>,
}

impl Lut {
    fn build(codes: &[(u32, u8)]) -> Lut {
        let mut t = vec![0u32; 1 << L1];
        // sub-table width per first-level prefix
        let mut width = vec![0u32; 1 << L1];
        for &(code, len) in codes {
            let len = len as u32;
            if len > L1 {
                let p = (code >> (len - L1)) as usize;
                width[p] = width[p].max(len - L1);
            }
        }
        for (p, &w) in width.iter().enumerate() {
            if w > 0 {
                t[p] = SUB | (t.len() as u32) << 5 | w;
                t.resize(t.len() + (1 << w), 0);
            }
        }
        for (sym, &(code, len)) in codes.iter().enumerate() {
            let len = len as u32;
            let leaf = len << 24 | sym as u32;
            if len <= L1 {
                let first = (code << (L1 - len)) as usize;
                t[first..first + (1 << (L1 - len))].fill(leaf);
            } else {
                let e = t[(code >> (len - L1)) as usize];
                let (off, w) = (((e & !SUB) >> 5) as usize, e & 31);
                let rest = code & ((1 << (len - L1)) - 1);
                let first = off + (rest << (w - (len - L1))) as usize;
                t[first..first + (1 << (w - (len - L1)))].fill(leaf);
            }
        }
        Lut { t }
    }

    /// The symbol of the codeword at the reader position; `Eof` if the codeword is cut off.
    #[inline]
    fn decode(&self, br: &mut BitReader) -> Result<u32> {
        let bits = peek32(br);
        let mut e = self.t[(bits >> (32 - L1)) as usize];
        if e & SUB != 0 {
            let w = e & 31;
            e = self.t[((e & !SUB) >> 5) as usize + ((bits << L1) >> (32 - w)) as usize];
        }
        let len = e >> 24;
        if len == 0 {
            return Err(Error::Bitstream("invalid Huffman codeword"));
        }
        br.skip(len as usize).map_err(|_| Error::Eof)?;
        Ok(e & 0xFF_FFFF)
    }
}

/// The next 32 bits (zero padded past the end), from one 8-byte load when possible.
#[inline]
fn peek32(br: &BitReader) -> u32 {
    let pos = br.position();
    let byte = pos >> 3;
    match br.data().get(byte..byte.saturating_add(8)).and_then(|b| <[u8; 8]>::try_from(b).ok()) {
        Some(b) => ((u64::from_be_bytes(b) << (pos & 7)) >> 32) as u32,
        None => br.peek(32),
    }
}

fn luts() -> &'static [Lut; 12] {
    static LUTS: OnceLock<[Lut; 12]> = OnceLock::new();
    LUTS.get_or_init(|| std::array::from_fn(|i| if i == 0 { Lut::build(&SCALEFACTOR) } else { Lut::build(BOOKS[i - 1].codes) }))
}

/// Decode one scalefactor difference (`-60..=60`).
#[inline]
pub fn decode_sf(br: &mut BitReader) -> Result<i32> {
    Ok(luts()[0].decode(br)? as i32 - 60)
}

/// Write one scalefactor difference (`-60..=60`).
#[inline]
pub fn write_sf(bw: &mut BitWriter, diff: i32) {
    let (code, len) = SCALEFACTOR[(diff + 60).clamp(0, 120) as usize];
    bw.write_bits(code, len as u32);
}

/// Bits of one scalefactor difference.
#[inline]
pub fn sf_bits(diff: i32) -> u32 {
    SCALEFACTOR[(diff + 60).clamp(0, 120) as usize].1 as u32
}

/// Decode one codeword of codebook `cb` (1..=11) into `out[..dim]`, including sign bits and escapes.
pub fn decode_spectral(br: &mut BitReader, cb: u8, out: &mut [i32]) -> Result<()> {
    let b = book(cb);
    let mut idx = luts()[cb as usize].decode(br)?;
    for i in (0..b.dim).rev() {
        out[i] = (idx % b.modulo) as i32;
        idx /= b.modulo;
    }
    if b.signed {
        for v in out[..b.dim].iter_mut() {
            *v -= b.lav as i32;
        }
        return Ok(());
    }
    for v in out[..b.dim].iter_mut() {
        if *v != 0 && br.read_bits(1)? == 1 {
            *v = -*v;
        }
    }
    if cb == ESC_HCB {
        for v in out[..b.dim].iter_mut() {
            if v.abs() == 16 {
                let mut n = 0u32;
                while br.read_bits(1)? == 1 {
                    n += 1;
                    if n > 8 {
                        return Err(Error::Bitstream("escape sequence too long"));
                    }
                }
                let mag = (1i32 << (n + 4)) + br.read_bits(n + 4)? as i32;
                *v = if *v < 0 { -mag } else { mag };
            }
        }
    }
    Ok(())
}

/// Codebook index of the (sign-stripped for unsigned books) values.
#[inline]
fn index_of(b: &Book, vals: &[i32]) -> usize {
    let mut idx = 0u32;
    for &v in &vals[..b.dim] {
        let d = if b.signed { (v + b.lav as i32) as u32 } else { v.unsigned_abs().min(b.lav) };
        idx = idx * b.modulo + d;
    }
    idx as usize
}

#[inline]
fn escape_bits(mag: u32) -> u32 {
    // mag >= 16: N ones, a zero, N+4 bits where 2^(N+4) <= mag < 2^(N+5).
    let n = 31 - mag.leading_zeros() - 4;
    2 * n + 5
}

/// Bits needed to code `vals` (a multiple of the codebook dimension) with codebook `cb` (1..=11).
/// The values must fit the codebook (`|v| <= lav`, or `<= 8191` for codebook 11).
pub fn count_bits(cb: u8, vals: &[i32]) -> u32 {
    let b = book(cb);
    let mut bits = 0u32;
    for chunk in vals.chunks_exact(b.dim) {
        bits += b.codes[index_of(b, chunk)].1 as u32;
        if !b.signed {
            for &v in chunk {
                if v != 0 {
                    bits += 1;
                    if cb == ESC_HCB && v.unsigned_abs() >= 16 {
                        bits += escape_bits(v.unsigned_abs());
                    }
                }
            }
        }
    }
    bits
}

/// Bits for codebooks `cb` and `cb + 1` in one pass. `cb` must be odd and below 11: each such pair
/// shares dimension, modulus and signedness, so the index is computed once and only the length table
/// differs.
pub fn count_bits_pair(cb: u8, vals: &[i32]) -> (u32, u32) {
    debug_assert!(cb % 2 == 1 && cb < ESC_HCB);
    let (b, b2) = (book(cb), book(cb + 1));
    let (mut x, mut y) = (0u32, 0u32);
    for chunk in vals.chunks_exact(b.dim) {
        let i = index_of(b, chunk);
        x += b.codes[i].1 as u32;
        y += b2.codes[i].1 as u32;
        if !b.signed {
            // sign bits (no escapes below codebook 11)
            let nz = chunk.iter().filter(|&&v| v != 0).count() as u32;
            x += nz;
            y += nz;
        }
    }
    (x, y)
}

/// Write `vals` with codebook `cb` (1..=11).
pub fn write_spectral(bw: &mut BitWriter, cb: u8, vals: &[i32]) {
    let b = book(cb);
    for chunk in vals.chunks_exact(b.dim) {
        let (code, len) = b.codes[index_of(b, chunk)];
        bw.write_bits(code, len as u32);
        if b.signed {
            continue;
        }
        for &v in chunk {
            if v != 0 {
                bw.write_bits((v < 0) as u32, 1);
            }
        }
        if cb == ESC_HCB {
            for &v in chunk {
                let mag = v.unsigned_abs();
                if mag >= 16 {
                    let n = 31 - mag.leading_zeros() - 4;
                    for _ in 0..n {
                        bw.write_bits(1, 1);
                    }
                    bw.write_bits(0, 1);
                    bw.write_bits(mag - (1 << (n + 4)), n + 4);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check_complete(codes: &[(u32, u8)]) {
        // Kraft sum exactly one and prefix-free.
        let max = codes.iter().map(|c| c.1).max().unwrap() as u32;
        let mut kraft = 0u64;
        for &(_, l) in codes {
            kraft += 1u64 << (max - l as u32);
        }
        assert_eq!(kraft, 1u64 << max);
        for (i, &(ci, li)) in codes.iter().enumerate() {
            for (j, &(cj, lj)) in codes.iter().enumerate() {
                if i != j && li <= lj {
                    assert_ne!(cj >> (lj - li), ci, "codeword {i} is a prefix of {j}");
                }
            }
        }
    }

    #[test]
    fn codebooks_are_complete_prefix_codes() {
        check_complete(&SCALEFACTOR);
        assert_eq!(SCALEFACTOR[60], (0, 1));
        for b in &BOOKS {
            assert_eq!(b.codes.len() as u32, b.modulo.pow(b.dim as u32));
            check_complete(b.codes);
        }
    }

    /// Every codeword, followed by arbitrary bits, decodes to its own symbol and length.
    #[test]
    fn lut_decodes_every_codeword() {
        let mut seed = 7u32;
        for (i, lut) in luts().iter().enumerate() {
            let codes: &[(u32, u8)] = if i == 0 { &SCALEFACTOR } else { BOOKS[i - 1].codes };
            for (sym, &(code, len)) in codes.iter().enumerate() {
                for _ in 0..3 {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    let mut bw = BitWriter::new();
                    bw.write_bits(code, len as u32);
                    bw.write_bits(seed >> 8, 24);
                    let data = bw.finish();
                    let mut br = BitReader::new(&data);
                    assert_eq!(lut.decode(&mut br).unwrap(), sym as u32, "book {i}");
                    assert_eq!(br.position(), len as usize);
                }
            }
        }
    }

    /// A codeword cut off by the end of the data is `Eof`, also at an unaligned position.
    #[test]
    fn truncated_codeword_is_eof() {
        for (i, lut) in luts().iter().enumerate() {
            let codes: &[(u32, u8)] = if i == 0 { &SCALEFACTOR } else { BOOKS[i - 1].codes };
            let &(code, len) = codes.iter().max_by_key(|c| c.1).unwrap();
            for lead in 0..8u32 {
                let mut bw = BitWriter::new();
                bw.write_bits(0, lead);
                bw.write_bits(code, len as u32);
                let total = lead as usize + len as usize;
                let data = bw.finish();
                // drop the last byte if that cuts the codeword, else the codeword is whole
                let short = &data[..data.len() - 1];
                let mut br = BitReader::new(short);
                br.skip(lead as usize).unwrap();
                let r = lut.decode(&mut br);
                if short.len() * 8 < total {
                    assert!(matches!(r, Err(Error::Eof)), "book {i} lead {lead}");
                } else {
                    assert!(r.is_ok());
                }
            }
        }
    }

    #[test]
    fn spectral_roundtrip_every_book() {
        for cb in 1..=11u8 {
            let b = book(cb);
            let lim = if cb == 11 { 8191 } else { b.lav as i32 };
            let mut vals = Vec::new();
            let mut seed = 12345u32;
            for _ in 0..400 {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let r = (seed >> 8) as i32;
                let v = if cb == 11 && r % 3 == 0 { r % (lim + 1) } else { r % (b.lav.min(16) as i32 + 1) };
                let v = v.min(lim);
                vals.push(if b.signed || r & 1 == 0 { v } else { -v });
            }
            if b.signed {
                for v in vals.iter_mut() {
                    *v = (*v).clamp(-(b.lav as i32), b.lav as i32);
                }
            }
            let mut bw = BitWriter::new();
            write_spectral(&mut bw, cb, &vals);
            assert_eq!(bw.bit_len() as u32, count_bits(cb, &vals));
            let data = bw.finish();
            let mut br = BitReader::new(&data);
            let mut out = vec![0i32; vals.len()];
            for c in out.chunks_mut(b.dim) {
                decode_spectral(&mut br, cb, c).unwrap();
            }
            assert_eq!(out, vals, "codebook {cb}");
        }
    }

    #[test]
    fn pair_count_matches_single_count() {
        let mut seed = 99u32;
        for cb in (1..11u8).step_by(2) {
            let b = book(cb);
            for round in 0..40 {
                let mut vals = Vec::new();
                for _ in 0..(8 * b.dim) {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    let m = ((seed >> 8) % (b.lav + 1)) as i32;
                    let m = if round % 4 == 0 { 0 } else { m };
                    let neg = if b.signed { seed & 0x8000_0000 != 0 } else { seed & 1 == 1 };
                    vals.push(if neg { -m } else { m });
                }
                assert_eq!(count_bits_pair(cb, &vals), (count_bits(cb, &vals), count_bits(cb + 1, &vals)), "codebook {cb}");
            }
        }
    }

    #[test]
    fn scalefactor_roundtrip() {
        let mut bw = BitWriter::new();
        for d in -60..=60 {
            write_sf(&mut bw, d);
        }
        let data = bw.finish();
        let mut br = BitReader::new(&data);
        for d in -60..=60 {
            assert_eq!(decode_sf(&mut br).unwrap(), d);
        }
    }
}
