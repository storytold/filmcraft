//! Bit-level reading and writing shared by FilmCraft's codec crates.
//!
//! - [`BitReader`]: MSB-first reader with Exp-Golomb (`ue(v)`, `se(v)`), as used by H.264/HEVC/AV1 headers.
//! - [`BitWriter`]: the matching writer (for encoders and header rewriting).
//! - [`unescape_rbsp`] / [`escape_rbsp`]: remove/insert H.264/HEVC emulation-prevention bytes.
//! - [`annexb_nals`] / [`length_prefixed_nals`]: split Annex-B byte streams and AVCC/HVCC samples into NAL units.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

use std::{cell::Cell, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitError {
    /// Tried to read past the end of the buffer.
    Eof,
    /// An Exp-Golomb code longer than 32 leading zeros.
    InvalidExpGolomb,
    /// An AVCC/HVCC NAL length prefix is not one of the supported widths.
    InvalidNalLengthSize,
}

impl fmt::Display for BitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BitError::Eof => f.write_str("unexpected end of bitstream"),
            BitError::InvalidExpGolomb => f.write_str("invalid Exp-Golomb code"),
            BitError::InvalidNalLengthSize => f.write_str("NAL length prefix must be 1 to 4 bytes"),
        }
    }
}
impl std::error::Error for BitError {}

pub type Result<T> = std::result::Result<T, BitError>;

/// MSB-first bit reader over a byte slice.
#[derive(Clone)]
pub struct BitReader<'a> {
    data: &'a [u8],
    /// Position in bits.
    pos: usize,
    /// Cached position of the RBSP stop bit, computed only by [`Self::more_rbsp_data`].
    rbsp_stop: Cell<Option<Option<usize>>>,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0, rbsp_stop: Cell::new(None) }
    }
    pub fn data(&self) -> &'a [u8] {
        self.data
    }
    /// Bit position from the start.
    pub fn position(&self) -> usize {
        self.pos
    }
    pub fn bits_left(&self) -> usize {
        (self.data.len() * 8).saturating_sub(self.pos)
    }
    pub fn is_byte_aligned(&self) -> bool {
        self.pos.is_multiple_of(8)
    }
    pub fn byte_align(&mut self) {
        self.pos = self.pos.div_ceil(8) * 8;
    }
    /// Byte offset of the current position (rounded down).
    pub fn byte_pos(&self) -> usize {
        self.pos / 8
    }
    pub fn seek_bits(&mut self, pos: usize) {
        self.pos = pos;
    }

    /// Peek up to 32 bits without consuming (zero-padded past the end).
    #[inline]
    pub fn peek(&self, n: u32) -> u32 {
        debug_assert!(n <= 32);
        if n == 0 {
            return 0;
        }
        let byte = self.pos / 8;
        let shift = self.pos % 8;
        let mut v: u64 = 0;
        for i in 0..5 {
            v = (v << 8) | *self.data.get(byte + i).unwrap_or(&0) as u64;
        }
        // v holds 40 bits starting at `byte`
        ((v << (24 + shift)) >> (64 - n)) as u32
    }

    #[inline]
    pub fn read_bits(&mut self, n: u32) -> Result<u32> {
        if n as usize > self.bits_left() {
            return Err(BitError::Eof);
        }
        let v = self.peek(n);
        self.pos += n as usize;
        Ok(v)
    }

    pub fn read_bits_u64(&mut self, n: u32) -> Result<u64> {
        if n <= 32 {
            return self.read_bits(n).map(u64::from);
        }
        let hi = self.read_bits(n - 32)? as u64;
        let lo = self.read_bits(32)? as u64;
        Ok((hi << 32) | lo)
    }

    #[inline]
    pub fn read_bit(&mut self) -> Result<bool> {
        self.read_bits(1).map(|b| b == 1)
    }
    pub fn read_flag(&mut self) -> Result<bool> {
        self.read_bit()
    }

    pub fn skip(&mut self, n: usize) -> Result<()> {
        if n > self.bits_left() {
            return Err(BitError::Eof);
        }
        self.pos += n;
        Ok(())
    }

    /// Raw Exp-Golomb codeNum, up to 32 leading zeros (so below 2^33).
    fn read_code_num(&mut self) -> Result<u64> {
        let mut zeros = 0u32;
        while !self.read_bit()? {
            zeros += 1;
            if zeros > 32 {
                return Err(BitError::InvalidExpGolomb);
            }
        }
        if zeros == 0 {
            return Ok(0);
        }
        let v = self.read_bits_u64(zeros)?;
        Ok((1u64 << zeros) - 1 + v)
    }

    /// Unsigned Exp-Golomb `ue(v)`.
    pub fn read_ue(&mut self) -> Result<u32> {
        u32::try_from(self.read_code_num()?).map_err(|_| BitError::InvalidExpGolomb)
    }

    /// Signed Exp-Golomb `se(v)`.
    pub fn read_se(&mut self) -> Result<i32> {
        // codeNum 2^32 is -2^31, so the mapping is done before narrowing to 32 bits.
        let k = self.read_code_num()? as i64;
        let v = if k & 1 == 1 { (k + 1) / 2 } else { -(k / 2) };
        i32::try_from(v).map_err(|_| BitError::InvalidExpGolomb)
    }

    /// Truncated Exp-Golomb `te(v)` with range `max`.
    pub fn read_te(&mut self, max: u32) -> Result<u32> {
        if max > 1 { self.read_ue() } else { Ok(!self.read_bit()? as u32) }
    }

    /// H.264/HEVC `more_rbsp_data()`: true if there is data before the rbsp trailing bits.
    pub fn more_rbsp_data(&self) -> bool {
        let total = self.data.len() * 8;
        if self.pos >= total {
            return false;
        }
        let last = self.rbsp_stop.get().unwrap_or_else(|| {
            // The trailing stop bit is immutable for this reader.  Caching it avoids searching a
            // long zero suffix after every macroblock.
            let stop = self.data.iter().rposition(|&b| b != 0).map(|i| i * 8 + 7 - self.data[i].trailing_zeros() as usize);
            self.rbsp_stop.set(Some(stop));
            stop
        });
        match last {
            Some(stop) => self.pos < stop,
            None => false,
        }
    }
}

/// MSB-first bit writer.
#[derive(Default, Clone)]
pub struct BitWriter {
    buf: Vec<u8>,
    acc: u64,
    nbits: u32,
}

impl BitWriter {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn write_bits(&mut self, value: u32, n: u32) {
        debug_assert!(n <= 32);
        if n == 0 {
            return;
        }
        let v = (value as u64) & ((1u64 << n) - 1);
        self.acc = (self.acc << n) | v;
        self.nbits += n;
        while self.nbits >= 8 {
            self.nbits -= 8;
            self.buf.push((self.acc >> self.nbits) as u8);
        }
        self.acc &= (1u64 << self.nbits) - 1;
    }
    pub fn write_bit(&mut self, b: bool) {
        self.write_bits(b as u32, 1);
    }
    pub fn write_ue(&mut self, v: u32) {
        self.write_code_num(u64::from(v));
    }
    /// Exp-Golomb codeNum up to 2^32 (the code of `se(v)` -2^31).
    fn write_code_num(&mut self, v: u64) {
        let x = v + 1;
        let len = 64 - x.leading_zeros();
        let zeros = len - 1;
        for _ in 0..zeros {
            self.write_bit(false);
        }
        if len > 32 {
            self.write_bits((x >> 32) as u32, len - 32);
            self.write_bits(x as u32, 32);
        } else {
            self.write_bits(x as u32, len);
        }
    }
    pub fn write_se(&mut self, v: i32) {
        let m = u64::from(v.unsigned_abs()) * 2;
        self.write_code_num(if v > 0 { m - 1 } else { m });
    }
    pub fn bit_len(&self) -> usize {
        self.buf.len() * 8 + self.nbits as usize
    }
    pub fn is_byte_aligned(&self) -> bool {
        self.nbits == 0
    }
    /// Pad with zero bits to a byte boundary.
    pub fn align_zero(&mut self) {
        if self.nbits > 0 {
            self.write_bits(0, 8 - self.nbits);
        }
    }
    /// `rbsp_trailing_bits()`: a one bit then zeros to alignment.
    pub fn rbsp_trailing(&mut self) {
        self.write_bit(true);
        self.align_zero();
    }
    pub fn write_bytes(&mut self, bytes: &[u8]) {
        if self.nbits == 0 {
            self.buf.extend_from_slice(bytes);
        } else {
            for &b in bytes {
                self.write_bits(b as u32, 8);
            }
        }
    }
    pub fn finish(mut self) -> Vec<u8> {
        self.align_zero();
        self.buf
    }
    pub fn bytes(&self) -> &[u8] {
        &self.buf
    }
}

/// Remove emulation-prevention bytes (`00 00 03` → `00 00`) from a NAL payload.
pub fn unescape_rbsp(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut zeros = 0;
    for &b in data {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        out.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
    out
}

/// Insert emulation-prevention bytes so no `00 00 0x` (x ≤ 3) sequence appears.
pub fn escape_rbsp(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 64);
    let mut zeros = 0;
    for &b in data {
        if zeros >= 2 && b <= 3 {
            out.push(3);
            zeros = 0;
        }
        out.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
    out
}

/// Split an Annex-B byte stream (start codes `00 00 01` / `00 00 00 01`) into NAL units (escaped payloads).
pub fn annexb_nals(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::with_capacity(starts.len());
    for (k, &s) in starts.iter().enumerate() {
        let mut e = if k + 1 < starts.len() { starts[k + 1] - 3 } else { data.len() };
        // trim trailing zero bytes (part of a 4-byte start code or trailing_zero_8bits)
        while e > s && data[e - 1] == 0 {
            e -= 1;
        }
        if e > s {
            out.push(&data[s..e]);
        }
    }
    out
}

/// Split a length-prefixed sample (AVCC/HVCC, `length_size` 1 to 4 bytes) into NAL units.
pub fn length_prefixed_nals(data: &[u8], length_size: usize) -> Result<Vec<&[u8]>> {
    if !matches!(length_size, 1..=4) {
        return Err(BitError::InvalidNalLengthSize);
    }
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < data.len() {
        let prefix_end = i.checked_add(length_size).ok_or(BitError::Eof)?;
        let prefix = data.get(i..prefix_end).ok_or(BitError::Eof)?;
        let mut len = 0usize;
        for &byte in prefix {
            len = (len << 8) | usize::from(byte);
        }
        let end = prefix_end.checked_add(len).ok_or(BitError::Eof)?;
        out.push(data.get(prefix_end..end).ok_or(BitError::Eof)?);
        i = end;
    }
    Ok(out)
}

/// CRC-32 (IEEE, reflected) — used by containers (e.g. Matroska CRC-32 elements, PNG).
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn reads_bits() {
        let d = [0b1010_1100, 0b0101_0011];
        let mut r = BitReader::new(&d);
        assert_eq!(r.read_bits(1).unwrap(), 1);
        assert_eq!(r.read_bits(3).unwrap(), 0b010);
        assert_eq!(r.read_bits(8).unwrap(), 0b1100_0101);
        assert_eq!(r.bits_left(), 4);
        assert_eq!(r.read_bits(5), Err(BitError::Eof));
    }

    #[test]
    fn exp_golomb_known() {
        // 1 | 010 | 011 | 00100 | 00101 -> 0,1,2,3,4
        let mut w = BitWriter::new();
        for v in 0..5 {
            w.write_ue(v);
        }
        let b = w.finish();
        let mut r = BitReader::new(&b);
        for v in 0..5 {
            assert_eq!(r.read_ue().unwrap(), v);
        }
        let mut w = BitWriter::new();
        for v in [0, 1, -1, 2, -2, 1000, -1000] {
            w.write_se(v);
        }
        let b = w.finish();
        let mut r = BitReader::new(&b);
        for v in [0, 1, -1, 2, -2, 1000, -1000] {
            assert_eq!(r.read_se().unwrap(), v);
        }
    }

    #[test]
    fn signed_exp_golomb_extremes_roundtrip() {
        let vals = [0, 1, -1, i32::MAX, i32::MIN];
        let mut w = BitWriter::new();
        for v in vals {
            w.write_se(v);
        }
        let b = w.finish();
        let mut r = BitReader::new(&b);
        for v in vals {
            assert_eq!(r.read_se(), Ok(v));
        }
    }

    #[test]
    fn signed_exp_golomb_reads_minimum_from_spec_bits() {
        // 32 zero bits, 1, then the 32-bit suffix 1: codeNum = 2^32 -> -2^31.
        let bytes = [0, 0, 0, 0, 0x80, 0, 0, 0, 0x80];
        assert_eq!(BitReader::new(&bytes).read_se(), Ok(i32::MIN));
    }

    #[test]
    fn signed_exp_golomb_rejects_positive_overflow() {
        // 32 zero bits, 1, then 32 zero bits: codeNum = 2^32 - 1 -> +2^31, not an i32.
        let bytes = [0, 0, 0, 0, 0x80, 0, 0, 0, 0];
        assert_eq!(BitReader::new(&bytes).read_se(), Err(BitError::InvalidExpGolomb));
    }

    #[test]
    fn unsigned_exp_golomb_keeps_u32_range() {
        // codeNum = 2^32 is a valid se(v) code but does not fit ue(v)'s u32.
        let bytes = [0, 0, 0, 0, 0x80, 0, 0, 0, 0x80];
        assert_eq!(BitReader::new(&bytes).read_ue(), Err(BitError::InvalidExpGolomb));
    }

    #[test]
    fn emulation_prevention() {
        let raw = [0, 0, 1, 0, 0, 0, 5, 0, 0, 3];
        let esc = escape_rbsp(&raw);
        assert_eq!(esc, vec![0, 0, 3, 1, 0, 0, 3, 0, 5, 0, 0, 3, 3]);
        assert_eq!(unescape_rbsp(&esc), raw);
    }

    #[test]
    fn annexb_split() {
        let s = [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 9, 9];
        let n = annexb_nals(&s);
        assert_eq!(n, vec![&[0x67, 1, 2][..], &[0x68, 3][..], &[0x65, 9, 9][..]]);
        let l = [0, 0, 0, 2, 0x67, 1, 0, 0, 0, 1, 0x68];
        assert_eq!(length_prefixed_nals(&l, 4).unwrap(), vec![&[0x67, 1][..], &[0x68][..]]);
    }

    #[test]
    fn length_prefixed_nals_rejects_invalid_prefix_sizes() {
        for size in [0, 5, usize::MAX] {
            assert!(length_prefixed_nals(&[0], size).is_err(), "size {size}");
        }
        // `(lengthSizeMinusOne & 3) + 1` can be 3; such samples still split
        assert_eq!(length_prefixed_nals(&[0, 0, 1, 0x65], 3).unwrap(), vec![&[0x65][..]]);
    }

    #[test]
    fn hostile_lengths_nal_size_does_not_wrap() {
        // #2801: adding the four-byte prefix to u32::MAX overflowed usize on wasm32.
        for data in [&[0xff; 4][..], &[0, 0, 0, 1, 0x67, 0xff, 0xff, 0xff, 0xff][..]] {
            let result = std::panic::catch_unwind(|| length_prefixed_nals(data, 4));
            assert_eq!(result.expect("NAL splitting must not panic"), Err(BitError::Eof));
        }
    }

    #[test]
    fn hostile_lengths_nal_prefix_width_is_bounded() {
        for width in [0, 5, 8, usize::MAX] {
            for data in [&[][..], &[1, 0x67][..]] {
                let result = std::panic::catch_unwind(|| length_prefixed_nals(data, width));
                assert_eq!(result.expect("invalid prefix widths must not panic or loop"), Err(BitError::InvalidNalLengthSize));
            }
        }
    }

    #[test]
    fn hostile_lengths_nal_mutations_and_truncations() {
        for width in 1..=4 {
            let mut data = vec![0; width]; // retain an empty NAL
            data.extend_from_slice(&1u32.to_be_bytes()[4 - width..]);
            data.push(0x67);
            assert_eq!(length_prefixed_nals(&data, width).unwrap(), vec![&[][..], &[0x67][..]]);
            assert_eq!(length_prefixed_nals(&[], width).unwrap(), Vec::<&[u8]>::new());
            for end in 0..=data.len() {
                std::panic::catch_unwind(|| length_prefixed_nals(&data[..end], width)).expect("truncated NAL sample must not panic").ok();
            }
            for index in 0..data.len() {
                for bit in 0..8 {
                    let mut mutated = data.clone();
                    mutated[index] ^= 1 << bit;
                    std::panic::catch_unwind(|| length_prefixed_nals(&mutated, width)).expect("mutated NAL sample must not panic").ok();
                }
            }
        }
    }

    #[test]
    fn more_rbsp() {
        let d = [0b1100_0000];
        let mut r = BitReader::new(&d);
        assert_eq!(r.rbsp_stop.get(), None);
        assert!(r.more_rbsp_data());
        assert_eq!(r.rbsp_stop.get(), Some(Some(1)));
        r.read_bit().unwrap();
        assert!(!r.more_rbsp_data());
    }

    #[test]
    fn crc() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    proptest! {
        #[test]
        fn roundtrip(vals in proptest::collection::vec((0u32..u32::MAX, 1u32..=32), 0..200), ues in proptest::collection::vec(0u32..u32::MAX - 1, 0..50)) {
            let mut w = BitWriter::new();
            for &(v, n) in &vals { w.write_bits(v, n); }
            for &u in &ues { w.write_ue(u); }
            let b = w.finish();
            let mut r = BitReader::new(&b);
            for &(v, n) in &vals {
                let mask = if n == 32 { u32::MAX } else { (1 << n) - 1 };
                prop_assert_eq!(r.read_bits(n).unwrap(), v & mask);
            }
            for &u in &ues { prop_assert_eq!(r.read_ue().unwrap(), u); }
        }

        #[test]
        fn escape_roundtrip(data in proptest::collection::vec(0u8..4, 0..300)) {
            let e = escape_rbsp(&data);
            prop_assert_eq!(unescape_rbsp(&e), data);
            for w in e.windows(3) { prop_assert!(!(w[0] == 0 && w[1] == 0 && w[2] < 3)); }
        }
    }
}
