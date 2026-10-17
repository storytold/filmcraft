//! zlib / DEFLATE decompression (RFC 1950 / RFC 1951), written from the RFCs for OpenEXR's ZIP
//! compression. Every input is hostile: truncated streams, invalid codes, distances before the start
//! and output beyond the caller's limit are errors, never panics.

/// Why a stream did not decompress.
pub type Error = &'static str;

/// Inflate a zlib stream (RFC 1950: two-byte header, DEFLATE data, Adler-32 trailer) into exactly
/// `expected` bytes. The trailer is not checked: the caller verifies the size, and EXR's own
/// structure catches corruption.
pub fn zlib(src: &[u8], expected: usize) -> Result<Vec<u8>, Error> {
    let (&cmf, &flg) = match src {
        [a, b, ..] => (a, b),
        _ => return Err("truncated zlib header"),
    };
    if cmf & 0x0F != 8 || cmf >> 4 > 7 || (u16::from(cmf) << 8 | u16::from(flg)) % 31 != 0 {
        return Err("not a zlib stream");
    }
    if flg & 0x20 != 0 {
        return Err("zlib preset dictionary");
    }
    let out = inflate(src.get(2..).unwrap_or_default(), expected)?;
    if out.len() != expected {
        return Err("zlib stream ends early");
    }
    Ok(out)
}

pub fn gzip(src: &[u8], limit: usize) -> Result<Vec<u8>, Error> {
    const FHCRC: u8 = 0x02;
    const FEXTRA: u8 = 0x04;
    const FNAME: u8 = 0x08;
    const FCOMMENT: u8 = 0x10;
    let [0x1f, 0x8b, 8, flags, _, _, _, _, _, _, rest @ ..] = src else {
        return Err("not a gzip stream");
    };
    let mut body = rest;
    if flags & FEXTRA != 0 {
        let [lo, hi, tail @ ..] = body else { return Err("truncated gzip header") };
        body = tail.get(usize::from(u16::from_le_bytes([*lo, *hi]))..).ok_or("truncated gzip header")?;
    }
    for flag in [FNAME, FCOMMENT] {
        if flags & flag != 0 {
            let end = body.iter().position(|&b| b == 0).ok_or("truncated gzip header")?;
            body = body.get(end + 1..).unwrap_or_default();
        }
    }
    if flags & FHCRC != 0 {
        body = body.get(2..).ok_or("truncated gzip header")?;
    }
    let [.., a, b, c, d] = body else { return Err("truncated gzip stream") };
    let size = usize::try_from(u32::from_le_bytes([*a, *b, *c, *d])).unwrap_or(usize::MAX);
    if size > limit {
        return Err("gzip stream is larger than allowed");
    }
    let out = inflate(body, size)?;
    if out.len() != size {
        return Err("gzip stream ends early");
    }
    Ok(out)
}

/// Inflate raw DEFLATE data, producing at most `limit` bytes (more is an error).
pub fn inflate(src: &[u8], limit: usize) -> Result<Vec<u8>, Error> {
    let mut bits = Bits { src, pos: 0, buf: 0, cnt: 0 };
    let mut out = Vec::new();
    out.try_reserve_exact(limit).map_err(|_| "output too large to allocate")?;
    let mut fixed: Option<(Huffman, Huffman)> = None;
    loop {
        let last = bits.take(1)? == 1;
        match bits.take(2)? {
            0 => stored(&mut bits, &mut out, limit)?,
            1 => {
                let (lit, dist) = match &fixed {
                    Some(t) => t,
                    None => fixed.insert(fixed_tables()?),
                };
                codes(&mut bits, &mut out, limit, lit, dist)?;
            }
            2 => {
                let (lit, dist) = dynamic_tables(&mut bits)?;
                codes(&mut bits, &mut out, limit, &lit, &dist)?;
            }
            _ => return Err("invalid deflate block type"),
        }
        if last {
            return Ok(out);
        }
    }
}

/// LSB-first bit reader.
struct Bits<'a> {
    src: &'a [u8],
    pos: usize,
    buf: u64,
    cnt: u32,
}

impl Bits<'_> {
    fn refill(&mut self) {
        while self.cnt <= 56 {
            let Some(&b) = self.src.get(self.pos) else { return };
            self.buf |= u64::from(b) << self.cnt;
            self.pos += 1;
            self.cnt += 8;
        }
    }
    /// The next `n` (≤ 32) bits without consuming them; zeros past the end of the input.
    fn peek(&mut self, n: u32) -> u32 {
        self.refill();
        (self.buf & ((1u64 << n) - 1)) as u32
    }
    fn consume(&mut self, n: u32) -> Result<(), Error> {
        if n > self.cnt {
            return Err("truncated deflate stream");
        }
        self.buf >>= n;
        self.cnt -= n;
        Ok(())
    }
    fn take(&mut self, n: u32) -> Result<u32, Error> {
        let v = self.peek(n);
        self.consume(n)?;
        Ok(v)
    }
}

/// A stored (uncompressed) block.
fn stored(bits: &mut Bits, out: &mut Vec<u8>, limit: usize) -> Result<(), Error> {
    bits.consume(bits.cnt % 8)?;
    let len = bits.take(16)?;
    if len != !bits.take(16)? & 0xFFFF {
        return Err("stored block length mismatch");
    }
    if out.len().saturating_add(len as usize) > limit {
        return Err("deflate output exceeds the expected size");
    }
    for _ in 0..len {
        out.push(bits.take(8)? as u8);
    }
    Ok(())
}

/// Codes of up to this many bits decode with one table lookup; longer ones walk the canonical code.
const FAST_BITS: u32 = 10;
const MAX_BITS: usize = 15;

/// A canonical Huffman code (RFC 1951 §3.2.2).
struct Huffman {
    /// Indexed by the next `FAST_BITS` input bits: `symbol << 4 | length`, 0 when the code is longer
    /// (or unassigned).
    fast: Vec<u16>,
    /// Number of codes of each length.
    count: [u16; MAX_BITS + 1],
    /// Symbols ordered by code (length, then symbol value).
    symbols: Vec<u16>,
}

impl Huffman {
    /// Build from per-symbol code lengths (0 = unused). Over-subscribed lengths are an error;
    /// incomplete codes are allowed (an unassigned code is an error when it is met).
    fn new(lengths: &[u8]) -> Result<Self, Error> {
        let mut count = [0u16; MAX_BITS + 1];
        for &l in lengths {
            let c = count.get_mut(usize::from(l)).ok_or("code length over 15")?;
            *c += 1;
        }
        count[0] = 0;
        let mut left: i32 = 1;
        for &c in &count[1..] {
            left = (left << 1) - i32::from(c);
            if left < 0 {
                return Err("over-subscribed Huffman code");
            }
        }
        // first index into `symbols` and first code of each length
        let mut offs = [0usize; MAX_BITS + 2];
        let mut next = [0u32; MAX_BITS + 2];
        let mut code = 0u32;
        for len in 1..=MAX_BITS {
            offs[len + 1] = offs[len] + usize::from(count[len]);
            code = (code + u32::from(count[len - 1])) << 1;
            next[len] = code;
        }
        let mut symbols = vec![0u16; offs[MAX_BITS + 1]];
        let mut fast = vec![0u16; 1 << FAST_BITS];
        for (sym, &l) in lengths.iter().enumerate() {
            let len = usize::from(l);
            if len == 0 {
                continue;
            }
            if let Some(s) = symbols.get_mut(offs[len]) {
                *s = sym as u16;
            }
            offs[len] += 1;
            let c = next[len];
            next[len] += 1;
            if len as u32 <= FAST_BITS {
                // codes are sent MSB first; the reader sees them bit-reversed
                let rev = c.reverse_bits() >> (32 - len as u32);
                let entry = (sym as u16) << 4 | len as u16;
                let mut i = rev as usize;
                while let Some(e) = fast.get_mut(i) {
                    *e = entry;
                    i += 1 << len;
                }
            }
        }
        Ok(Self { fast, count, symbols })
    }

    fn decode(&self, bits: &mut Bits) -> Result<u16, Error> {
        let peek = bits.peek(MAX_BITS as u32);
        let e = self.fast.get((peek & ((1 << FAST_BITS) - 1)) as usize).copied().unwrap_or(0);
        if e != 0 {
            bits.consume(u32::from(e & 15))?;
            return Ok(e >> 4);
        }
        // canonical walk: codes of each length are consecutive, starting at `first`
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..=MAX_BITS {
            code |= ((peek >> (len - 1)) & 1) as i32;
            let count = i32::from(self.count[len]);
            if code - first < count {
                bits.consume(len as u32)?;
                return self.symbols.get((index + code - first) as usize).copied().ok_or("invalid Huffman code");
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err("invalid Huffman code")
    }
}

fn fixed_tables() -> Result<(Huffman, Huffman), Error> {
    let mut lit = [8u8; 288];
    lit[144..256].fill(9);
    lit[256..280].fill(7);
    Ok((Huffman::new(&lit)?, Huffman::new(&[5; 30])?))
}

/// Order in which code-length code lengths are sent.
const CL_ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];

fn dynamic_tables(bits: &mut Bits) -> Result<(Huffman, Huffman), Error> {
    let nlit = bits.take(5)? as usize + 257;
    let ndist = bits.take(5)? as usize + 1;
    let ncl = bits.take(4)? as usize + 4;
    if nlit > 286 || ndist > 30 {
        return Err("too many deflate codes");
    }
    let mut cl = [0u8; 19];
    for &i in CL_ORDER.iter().take(ncl) {
        cl[i] = bits.take(3)? as u8;
    }
    let clh = Huffman::new(&cl)?;
    let mut lengths = vec![0u8; nlit + ndist];
    let mut i = 0;
    while i < lengths.len() {
        let sym = clh.decode(bits)?;
        let (value, repeat) = match sym {
            0..=15 => (sym as u8, 1),
            16 => {
                let prev = *lengths.get(i.wrapping_sub(1)).ok_or("repeat with no previous length")?;
                (prev, 3 + bits.take(2)? as usize)
            }
            17 => (0, 3 + bits.take(3)? as usize),
            18 => (0, 11 + bits.take(7)? as usize),
            _ => return Err("invalid code length symbol"),
        };
        let run = lengths.get_mut(i..i + repeat).ok_or("code lengths overflow")?;
        run.fill(value);
        i += repeat;
    }
    let (lit, dist) = lengths.split_at(nlit);
    if lit.get(256).copied().unwrap_or(0) == 0 {
        return Err("no end-of-block code");
    }
    Ok((Huffman::new(lit)?, Huffman::new(dist)?))
}

const LEN_BASE: [u16; 29] = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LEN_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DIST_BASE: [u16; 30] =
    [1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577];
const DIST_EXTRA: [u8; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

/// The literal / length and distance codes of one compressed block.
fn codes(bits: &mut Bits, out: &mut Vec<u8>, limit: usize, lit: &Huffman, dist: &Huffman) -> Result<(), Error> {
    loop {
        let sym = lit.decode(bits)?;
        if sym < 256 {
            if out.len() >= limit {
                return Err("deflate output exceeds the expected size");
            }
            out.push(sym as u8);
            continue;
        }
        if sym == 256 {
            return Ok(());
        }
        let k = usize::from(sym - 257);
        let (&base, &extra) = LEN_BASE.get(k).zip(LEN_EXTRA.get(k)).ok_or("invalid length code")?;
        let len = usize::from(base) + bits.take(u32::from(extra))? as usize;
        let d = usize::from(dist.decode(bits)?);
        let (&base, &extra) = DIST_BASE.get(d).zip(DIST_EXTRA.get(d)).ok_or("invalid distance code")?;
        let distance = usize::from(base) + bits.take(u32::from(extra))? as usize;
        if distance > out.len() {
            return Err("distance before the start of the output");
        }
        if out.len().saturating_add(len) > limit {
            return Err("deflate output exceeds the expected size");
        }
        let from = out.len() - distance;
        if distance >= len {
            out.extend_from_within(from..from + len);
        } else {
            // overlapping copy: repeats the last `distance` bytes
            for j in 0..len {
                let b = out.get(from + j).copied().unwrap_or(0);
                out.push(b);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Vectors made with Python's zlib (an external tool; the bytes are data).
    /// `zlib.compress(b"hello hello hello hello", 9)`: one fixed-Huffman block with a back reference.
    const FIXED: &[u8] = &[0x78, 0xda, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x57, 0xc8, 0x40, 0x27, 0x01, 0x68, 0x03, 0x08, 0xb1];

    #[test]
    fn fixed_block() {
        assert_eq!(zlib(FIXED, 23).unwrap(), b"hello hello hello hello");
    }

    #[test]
    fn stored_block() {
        // zlib header, one final stored block of 3 bytes, Adler-32
        let s = [0x78, 0x01, 0x01, 0x03, 0x00, 0xfc, 0xff, b'a', b'b', b'c', 0x02, 0x4d, 0x01, 0x27];
        assert_eq!(zlib(&s, 3).unwrap(), b"abc");
    }

    /// `zlib.compressobj(9)` of the 300 bytes `(i*i*7 + i/3) % 23 + 65`: one dynamic-Huffman block.
    const DYNAMIC: &[u8] = &[
        0x78, 0xda, 0xe5, 0xcc, 0x01, 0x11, 0xc0, 0x20, 0x08, 0x00, 0xc0, 0x6c, 0x6c, 0x22, 0x70, 0x0a, 0xe2, 0xa1, 0xd0, 0xbf, 0xc9, 0x82, 0xec, 0x03, 0x3c,
        0x70, 0x8f, 0xf4, 0x07, 0x2c, 0xac, 0x60, 0x9d, 0xdd, 0xf0, 0x36, 0xd0, 0xc5, 0x6e, 0x2f, 0x81, 0x48, 0xbe, 0xc9, 0x5c, 0x08, 0x53, 0x71, 0xca, 0x29,
        0xaf, 0x1c, 0x26, 0xdb, 0x69, 0x74, 0x77, 0x52, 0x89, 0xab, 0xb1, 0x91, 0xe0, 0x1f, 0xc9, 0x07, 0x4a, 0xb8, 0x58, 0xfe,
    ];

    #[test]
    fn dynamic_block() {
        let want: Vec<u8> = (0..300usize).map(|i| ((i * i * 7 + i / 3) % 23 + 65) as u8).collect();
        assert_eq!(zlib(DYNAMIC, 300).unwrap(), want);
    }

    #[test]
    fn garbage_never_panics() {
        for seed in 1u32..3000 {
            let mut x = seed.wrapping_mul(2_654_435_761);
            let v: Vec<u8> = (0..64)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 17;
                    x ^= x << 5;
                    x as u8
                })
                .collect();
            let _ = inflate(&v, 4096);
        }
        // every truncation and single-byte corruption of a real stream
        for n in 0..DYNAMIC.len() {
            let _ = zlib(&DYNAMIC[..n], 300);
            let mut v = DYNAMIC.to_vec();
            v[n] ^= 0x5A;
            let _ = zlib(&v, 300);
        }
    }

    #[test]
    fn hostile_streams_are_errors() {
        assert!(zlib(&[], 1).is_err());
        assert!(zlib(&[0x78, 0x9c], 1).is_err()); // header only
        assert!(zlib(&FIXED[..8], 23).is_err()); // truncated
        assert!(zlib(FIXED, 5).is_err()); // more output than allowed
        assert!(zlib(FIXED, 30).is_err()); // less output than expected
        assert!(zlib(&[0x78, 0x01, 0x07], 1).is_err()); // reserved block type
        // distance 1 with no output yet: fixed block, length code 257, distance code 0
        assert!(inflate(&[0x03, 0x02], 16).is_err());
    }
}
