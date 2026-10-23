//! EBML primitives (RFC 8794): variable-length integers, element headers, typed values, CRC-32,
//! and Matroska block lacing (RFC 9559 §10.3).

use crate::error::{Result, invalid};

/// Length in bytes of a VINT whose first byte is `b` (1..=8), or `None` for a zero first byte.
#[inline]
pub fn vint_len(b: u8) -> Option<usize> {
    if b == 0 { None } else { Some(b.leading_zeros() as usize + 1) }
}

/// Element data size: known, or "unknown" (all value bits set).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    Known(u64),
    Unknown,
}

impl Size {
    pub fn known(self) -> Option<u64> {
        match self {
            Size::Known(n) => Some(n),
            Size::Unknown => None,
        }
    }
}

/// Decode an unsigned VINT (marker bit stripped). Returns `(value, length, all_ones)`.
pub fn read_vint(data: &[u8]) -> Option<(u64, usize, bool)> {
    let first = *data.first()?;
    let len = vint_len(first)?;
    if data.len() < len {
        return None;
    }
    let mask = if len == 8 { 0 } else { 0xFFu8 >> len };
    let mut v = (first & mask) as u64;
    for &b in &data[1..len] {
        v = (v << 8) | b as u64;
    }
    let all_ones = v == (1u64 << (7 * len)) - 1;
    Some((v, len, all_ones))
}

/// Decode an element data size. All-ones is the reserved "unknown size".
pub fn read_size(data: &[u8]) -> Option<(Size, usize)> {
    let (v, len, ones) = read_vint(data)?;
    Some((if ones { Size::Unknown } else { Size::Known(v) }, len))
}

/// Decode an element ID (marker bits kept, 1..=4 bytes). Rejects all-ones value bits (reserved).
/// All-zero value bits are accepted, because Matroska's `ChapterDisplay` uses ID `0x80`.
pub fn read_id(data: &[u8]) -> Option<(u32, usize)> {
    let first = *data.first()?;
    let len = vint_len(first)?;
    if len > 4 || data.len() < len {
        return None;
    }
    let (v, _, ones) = read_vint(&data[..len])?;
    if ones || (v == 0 && len > 1) {
        return None;
    }
    let mut id = 0u32;
    for &b in &data[..len] {
        id = (id << 8) | b as u32;
    }
    Some((id, len))
}

/// Decode a signed VINT as used by EBML lacing (value − (2^(7·len−1) − 1)).
pub fn read_svint(data: &[u8]) -> Option<(i64, usize)> {
    let (v, len, _) = read_vint(data)?;
    let bias = (1i64 << (7 * len - 1)) - 1;
    Some((v as i64 - bias, len))
}

/// Encode an element ID (already carrying its marker bits).
pub fn write_id(out: &mut Vec<u8>, id: u32) {
    let n = if id >= 0x100_0000 {
        4
    } else if id >= 0x1_0000 {
        3
    } else if id >= 0x100 {
        2
    } else {
        1
    };
    out.extend_from_slice(&id.to_be_bytes()[4 - n..]);
}

/// Encode a data size as a VINT using the shortest length (or `min_len` if larger).
pub fn write_size(out: &mut Vec<u8>, size: u64, min_len: usize) {
    let mut len = 1;
    while len < 8 && size >= (1u64 << (7 * len)) - 1 {
        len += 1;
    }
    let len = len.max(min_len).min(8);
    let v = size | (1u64 << (7 * len));
    out.extend_from_slice(&v.to_be_bytes()[8 - len..]);
}

/// Encode the "unknown size" marker of the given length.
pub fn write_unknown_size(out: &mut Vec<u8>, len: usize) {
    out.push(0xFFu8 >> (len - 1));
    out.extend(std::iter::repeat_n(0xFF, len - 1));
}

/// Big-endian unsigned integer of 0..=8 bytes.
pub fn uint(data: &[u8]) -> u64 {
    data.iter().take(8).fold(0u64, |a, &b| (a << 8) | b as u64)
}

/// Big-endian two's-complement signed integer of 0..=8 bytes.
pub fn int(data: &[u8]) -> i64 {
    let n = data.len().min(8);
    if n == 0 {
        return 0;
    }
    let v = uint(&data[..n]);
    let shift = 64 - 8 * n as u32;
    ((v << shift) as i64) >> shift
}

/// IEEE float of 0, 4 or 8 bytes.
pub fn float(data: &[u8]) -> f64 {
    match data.len() {
        4 => f32::from_be_bytes(data.try_into().unwrap_or([0; 4])) as f64,
        8 => f64::from_be_bytes(data.try_into().unwrap_or([0; 8])),
        _ => 0.0,
    }
}

/// String / UTF-8 value: bytes up to the first NUL, lossily decoded.
pub fn string(data: &[u8]) -> String {
    let end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
    String::from_utf8_lossy(&data[..end]).into_owned()
}

/// One element inside an in-memory master element.
#[derive(Clone, Copy, Debug)]
pub struct Elem<'a> {
    pub id: u32,
    /// Offset of the element header, relative to the start of the parent's data.
    pub offset: usize,
    pub header_len: usize,
    pub data: &'a [u8],
    /// The size field was "unknown" (data runs to the end of the parent).
    pub unknown_size: bool,
    /// The declared size overran the parent and was truncated.
    pub truncated: bool,
}

/// Iterator over the children of an in-memory master element.
///
/// Damaged data ends the iteration (see [`Children::damaged`]); oversize elements are truncated to
/// the parent's end.
pub struct Children<'a> {
    data: &'a [u8],
    pos: usize,
    damaged: bool,
}

impl<'a> Children<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0, damaged: false }
    }
    /// True if iteration stopped on an undecodable header.
    pub fn damaged(&self) -> bool {
        self.damaged
    }
}

impl<'a> Iterator for Children<'a> {
    type Item = Elem<'a>;
    fn next(&mut self) -> Option<Elem<'a>> {
        let rest = self.data.get(self.pos..)?;
        if rest.is_empty() {
            return None;
        }
        let Some((id, il)) = read_id(rest) else {
            self.damaged = true;
            self.pos = self.data.len();
            return None;
        };
        let Some((size, sl)) = read_size(&rest[il..]) else {
            self.damaged = true;
            self.pos = self.data.len();
            return None;
        };
        let hl = il + sl;
        let avail = (rest.len() - hl) as u64;
        let (len, unknown, truncated) = match size {
            Size::Known(n) if n <= avail => (n as usize, false, false),
            Size::Known(_) => (avail as usize, false, true),
            Size::Unknown => (avail as usize, true, false),
        };
        let e = Elem { id, offset: self.pos, header_len: hl, data: &rest[hl..hl + len], unknown_size: unknown, truncated };
        self.pos += hl + len;
        Some(e)
    }
}

/// CRC-32 (ISO 3309 / IEEE 802.3, reflected polynomial 0xEDB88320), as used by EBML CRC-32 elements.
pub fn crc32(data: &[u8]) -> u32 {
    crc32_update(0xFFFF_FFFF, data) ^ 0xFFFF_FFFF
}

/// Incremental form: start with `0xFFFF_FFFF`, finish with `^ 0xFFFF_FFFF`.
pub fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let t = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, e) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *e = c;
        }
        t
    });
    for &b in data {
        crc = t[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc
}

/// Check a master element's leading CRC-32 child, if present. `Some(true/false)` if there is one.
pub fn verify_crc(master_data: &[u8]) -> Option<bool> {
    let mut it = Children::new(master_data);
    let first = it.next()?;
    if first.id != crate::ids::CRC32 || first.data.len() != 4 {
        return None;
    }
    let stored = u32::from_le_bytes(first.data.try_into().ok()?);
    let rest = &master_data[first.offset + first.header_len + 4..];
    Some(crc32(rest) == stored)
}

/// Block lacing mode (flags bits 1–2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lacing {
    None,
    Xiph,
    FixedSize,
    Ebml,
}

impl Lacing {
    pub fn from_flags(flags: u8) -> Lacing {
        match (flags >> 1) & 3 {
            0 => Lacing::None,
            1 => Lacing::Xiph,
            2 => Lacing::FixedSize,
            _ => Lacing::Ebml,
        }
    }
}

/// Split laced block payload. `data` starts right after the block's flags byte and runs to the end
/// of the block. Returns `(offset, len)` of each frame relative to `data`.
pub fn parse_lacing(lacing: Lacing, data: &[u8]) -> Result<Vec<(usize, usize)>> {
    if lacing == Lacing::None {
        return Ok(vec![(0, data.len())]);
    }
    let Some(&n1) = data.first() else { return invalid("laced block without frame count") };
    let count = n1 as usize + 1;
    let mut pos = 1usize;
    let mut sizes = Vec::with_capacity(count);
    match lacing {
        Lacing::None => return Ok(vec![(0, data.len())]),
        Lacing::Xiph => {
            for _ in 0..count - 1 {
                let mut s = 0usize;
                loop {
                    let Some(&b) = data.get(pos) else { return invalid("truncated Xiph lace header") };
                    pos += 1;
                    s += b as usize;
                    if b != 255 {
                        break;
                    }
                }
                sizes.push(s);
            }
        }
        Lacing::Ebml => {
            if count > 1 {
                let Some((first, l, _)) = read_vint(&data[pos..]) else { return invalid("bad EBML lace size") };
                pos += l;
                let mut prev = first as i64;
                sizes.push(first as usize);
                for _ in 1..count - 1 {
                    let Some((d, l)) = read_svint(&data[pos..]) else { return invalid("bad EBML lace delta") };
                    pos += l;
                    prev += d;
                    if prev < 0 {
                        return invalid("negative EBML lace size");
                    }
                    sizes.push(prev as usize);
                }
            }
        }
        Lacing::FixedSize => {
            let rest = data.len() - pos;
            if !rest.is_multiple_of(count) {
                return invalid("fixed-size lacing does not divide block");
            }
            sizes.extend(std::iter::repeat_n(rest / count, count - 1));
        }
    }
    let used: usize = sizes.iter().try_fold(0usize, |a, &s| a.checked_add(s)).unwrap_or(usize::MAX);
    if pos > data.len() || used > data.len() - pos {
        return invalid("lace sizes exceed block");
    }
    sizes.push(data.len() - pos - used);
    if sizes.iter().any(|&size| size == 0) {
        return invalid("empty laced frame");
    }
    let mut out = Vec::with_capacity(count);
    for s in sizes {
        out.push((pos, s));
        pos += s;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vint_roundtrip() {
        for &v in &[0u64, 1, 126, 127, 128, 16382, 16383, 16384, 1 << 20, (1 << 56) - 2] {
            let mut b = Vec::new();
            write_size(&mut b, v, 1);
            let (s, l) = read_size(&b).unwrap();
            assert_eq!(s, Size::Known(v), "value {v}");
            assert_eq!(l, b.len());
        }
        // 127 needs 2 bytes because 0xFF is "unknown"
        let mut b = Vec::new();
        write_size(&mut b, 127, 1);
        assert_eq!(b, [0x40, 0x7F]);
        // padded length
        let mut b = Vec::new();
        write_size(&mut b, 5, 8);
        assert_eq!(b, [1, 0, 0, 0, 0, 0, 0, 5]);
        assert_eq!(read_size(&b).unwrap(), (Size::Known(5), 8));
    }

    #[test]
    fn vint_unknown_and_invalid() {
        assert_eq!(read_size(&[0xFF]).unwrap(), (Size::Unknown, 1));
        assert_eq!(read_size(&[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]).unwrap(), (Size::Unknown, 8));
        for l in 1..=8 {
            let mut b = Vec::new();
            write_unknown_size(&mut b, l);
            assert_eq!(read_size(&b).unwrap(), (Size::Unknown, l));
        }
        assert!(read_size(&[0x00, 1, 2]).is_none());
        assert!(read_size(&[0x40]).is_none()); // truncated
        assert_eq!(read_vint(&[0x81]).unwrap(), (1, 1, false));
        assert_eq!(read_vint(&[0x10, 0x00, 0x00, 0x01]).unwrap(), (1, 4, false));
    }

    #[test]
    fn ids() {
        assert_eq!(read_id(&[0x1A, 0x45, 0xDF, 0xA3, 0x99]).unwrap(), (crate::ids::EBML, 4));
        assert_eq!(read_id(&[0xA3]).unwrap(), (0xA3, 1));
        assert_eq!(read_id(&[0x42, 0x86]).unwrap(), (0x4286, 2));
        assert!(read_id(&[0xFF]).is_none()); // reserved
        assert_eq!(read_id(&[0x80]).unwrap(), (0x80, 1)); // ChapterDisplay
        assert!(read_id(&[0x40, 0x00]).is_none()); // zero value
        assert!(read_id(&[0x08, 0, 0, 0, 1]).is_none()); // 5-byte ID
        for id in [0xA3u32, 0x4286, 0x2AD7B1, 0x1F43B675] {
            let mut b = Vec::new();
            write_id(&mut b, id);
            assert_eq!(read_id(&b).unwrap(), (id, b.len()));
        }
    }

    #[test]
    fn signed_vint() {
        assert_eq!(read_svint(&[0xBF]).unwrap(), (0, 1)); // 63 - 63
        assert_eq!(read_svint(&[0x80]).unwrap(), (-63, 1));
        assert_eq!(read_svint(&[0xC0]).unwrap(), (1, 1));
        assert_eq!(read_svint(&[0x5F, 0xFF]).unwrap(), (0, 2));
        assert_eq!(read_svint(&[0x60, 0x00]).unwrap(), (1, 2));
    }

    #[test]
    fn values() {
        assert_eq!(uint(&[]), 0);
        assert_eq!(uint(&[1, 0]), 256);
        assert_eq!(int(&[0xFF]), -1);
        assert_eq!(int(&[0xFF, 0xFE]), -2);
        assert_eq!(int(&[0x7F]), 127);
        assert_eq!(float(&1.5f32.to_be_bytes()), 1.5);
        assert_eq!(float(&2.25f64.to_be_bytes()), 2.25);
        assert_eq!(string(b"abc\0\0"), "abc");
    }

    #[test]
    fn crc() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        let payload = [0x42u8, 0x86, 0x81, 0x01];
        let mut m = vec![0xBF, 0x84];
        m.extend_from_slice(&crc32(&payload).to_le_bytes());
        m.extend_from_slice(&payload);
        assert_eq!(verify_crc(&m), Some(true));
        m[7] ^= 1;
        assert_eq!(verify_crc(&m), Some(false));
        assert_eq!(verify_crc(&payload), None);
    }

    #[test]
    fn children_iter() {
        // two elements, then one whose size overruns
        let d = [0x42, 0x86, 0x81, 0x01, 0xEC, 0x82, 0, 0, 0xA3, 0x85, 1, 2];
        let v: Vec<_> = Children::new(&d).collect();
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].id, 0x4286);
        assert_eq!(v[0].data, &[1]);
        assert_eq!(v[1].id, 0xEC);
        assert_eq!(v[2].data, &[1, 2]);
        assert!(v[2].truncated);
        let mut it = Children::new(&[0x00, 0x01]);
        assert!(it.next().is_none());
        assert!(it.damaged());
    }

    fn frames<'a>(d: &'a [u8], f: &[(usize, usize)]) -> Vec<&'a [u8]> {
        f.iter().map(|&(o, l)| &d[o..o + l]).collect()
    }

    #[test]
    fn lacing_xiph() {
        // 3 frames: 300, 2, rest(3)
        let mut d = vec![2u8, 255, 45, 2];
        d.extend(std::iter::repeat_n(7u8, 300));
        d.extend_from_slice(&[1, 2]);
        d.extend_from_slice(&[3, 4, 5]);
        let f = parse_lacing(Lacing::Xiph, &d).unwrap();
        assert_eq!(f.iter().map(|x| x.1).collect::<Vec<_>>(), [300, 2, 3]);
        assert_eq!(frames(&d, &f)[2], &[3, 4, 5]);
        // exact multiple of 255 needs a terminating 0
        let mut d = vec![1u8, 255, 0];
        d.extend(std::iter::repeat_n(1u8, 255));
        d.push(9);
        let f = parse_lacing(Lacing::Xiph, &d).unwrap();
        assert_eq!(f.iter().map(|x| x.1).collect::<Vec<_>>(), [255, 1]);
        assert!(parse_lacing(Lacing::Xiph, &[1, 200, 1]).is_err());
    }

    #[test]
    fn lacing_ebml() {
        // 4 frames: 5, 3 (delta -2), 4 (delta +1), rest 2
        let mut d = vec![3u8, 0x85, 0xBF - 2, 0xBF + 1];
        d.extend_from_slice(&[1; 5]);
        d.extend_from_slice(&[2; 3]);
        d.extend_from_slice(&[3; 4]);
        d.extend_from_slice(&[4; 2]);
        let f = parse_lacing(Lacing::Ebml, &d).unwrap();
        assert_eq!(f.iter().map(|x| x.1).collect::<Vec<_>>(), [5, 3, 4, 2]);
        assert_eq!(frames(&d, &f), [&[1u8; 5][..], &[2; 3], &[3; 4], &[4; 2]]);
        assert!(parse_lacing(Lacing::Ebml, &[1, 0x90]).is_err());
    }

    #[test]
    fn lacing_fixed_and_none() {
        let d = [2u8, 1, 1, 2, 2, 3, 3];
        let f = parse_lacing(Lacing::FixedSize, &d).unwrap();
        assert_eq!(frames(&d, &f), [&[1u8, 1][..], &[2, 2], &[3, 3]]);
        assert!(parse_lacing(Lacing::FixedSize, &[2, 1, 1]).is_err());
        assert!(parse_lacing(Lacing::FixedSize, &[1]).is_err());
        assert_eq!(parse_lacing(Lacing::None, &[9, 9]).unwrap(), [(0, 2)]);
        assert_eq!(Lacing::from_flags(0x80 | 0x06), Lacing::Ebml);
        assert_eq!(Lacing::from_flags(0x02), Lacing::Xiph);
        assert_eq!(Lacing::from_flags(0x04), Lacing::FixedSize);
    }
}
