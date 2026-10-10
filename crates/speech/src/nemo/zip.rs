//! The central directory of a zip archive stored inside a byte range of a file (a PyTorch
//! checkpoint inside a `.nemo` tar). Only *stored* (uncompressed) entries are supported, which is
//! what `torch.save` writes; zip64 end records and extra fields are understood.
//!
//! Hostile input: every offset and size is checked against the range, the directory size and
//! entry count are capped, and compressed entries are refused.

use std::fs::File;

use crate::SpeechError;

const MAX_ENTRIES: usize = 1_000_000;
const MAX_DIRECTORY: u64 = 256 << 20;
/// End-of-central-directory record plus the largest comment.
const MAX_TAIL: u64 = 22 + 0xffff;

/// One stored entry.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub name: String,
    /// Absolute file offset of the entry's data.
    pub offset: u64,
    pub size: u64,
}

fn bad(msg: &str) -> SpeechError {
    SpeechError::Model(format!("not a valid checkpoint: {msg}"))
}

fn u16_at(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes(*b.get(i..i + 2)?.first_chunk::<2>()?))
}
fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(*b.get(i..i + 4)?.first_chunk::<4>()?))
}
fn u64_at(b: &[u8], i: usize) -> Option<u64> {
    Some(u64::from_le_bytes(*b.get(i..i + 8)?.first_chunk::<8>()?))
}

fn read(f: &mut File, at: u64, len: u64) -> Result<Vec<u8>, SpeechError> {
    super::read_at(f, at, len)
}

/// Index the zip archive occupying `[base, base + len)` of `f`.
pub fn index(f: &mut File, base: u64, len: u64) -> Result<Vec<Entry>, SpeechError> {
    let end = base.checked_add(len).ok_or_else(|| bad("size"))?;
    let tail_len = len.min(MAX_TAIL);
    let tail_at = end - tail_len;
    let tail = read(f, tail_at, tail_len)?;
    // the end record is the last occurrence of its signature
    let eocd = (0..tail.len().saturating_sub(21)).rev().find(|&i| u32_at(&tail, i) == Some(0x0605_4b50)).ok_or_else(|| bad("no end of central directory"))?;
    let field = |i: usize| u32_at(&tail, eocd + i).ok_or_else(|| bad("end record"));
    let mut count = u64::from(u16_at(&tail, eocd + 10).ok_or_else(|| bad("end record"))?);
    let mut cd_size = u64::from(field(12)?);
    let mut cd_off = u64::from(field(16)?);
    if count == 0xffff || cd_size == 0xffff_ffff || cd_off == 0xffff_ffff {
        // zip64: the locator sits just before the end record
        let loc = eocd.checked_sub(20).ok_or_else(|| bad("zip64 locator"))?;
        if u32_at(&tail, loc) != Some(0x0706_4b50) {
            return Err(bad("zip64 locator"));
        }
        let rec_off = u64_at(&tail, loc + 8).ok_or_else(|| bad("zip64 locator"))?;
        let rec = read(f, base.checked_add(rec_off).filter(|&a| a < end).ok_or_else(|| bad("zip64 record offset"))?, 56.min(len))?;
        if u32_at(&rec, 0) != Some(0x0606_4b50) {
            return Err(bad("zip64 end record"));
        }
        count = u64_at(&rec, 32).ok_or_else(|| bad("zip64 end record"))?;
        cd_size = u64_at(&rec, 40).ok_or_else(|| bad("zip64 end record"))?;
        cd_off = u64_at(&rec, 48).ok_or_else(|| bad("zip64 end record"))?;
    }
    if cd_size > MAX_DIRECTORY || count > MAX_ENTRIES as u64 {
        return Err(bad("central directory too large"));
    }
    let cd_at = base.checked_add(cd_off).ok_or_else(|| bad("directory offset"))?;
    if cd_at.checked_add(cd_size).is_none_or(|e| e > end) {
        return Err(bad("central directory outside the archive"));
    }
    let cd = read(f, cd_at, cd_size)?;
    let mut out = Vec::with_capacity(count as usize);
    let mut p = 0usize;
    for _ in 0..count {
        if u32_at(&cd, p) != Some(0x0201_4b50) {
            return Err(bad("central directory entry"));
        }
        let h = |i: usize| u16_at(&cd, p + i).map(usize::from).ok_or_else(|| bad("directory entry"));
        let w = |i: usize| u32_at(&cd, p + i).map(u64::from).ok_or_else(|| bad("directory entry"));
        let method = h(10)?;
        let mut csize = w(20)?;
        let mut usize_ = w(24)?;
        let (nlen, xlen, clen) = (h(28)?, h(30)?, h(32)?);
        let mut local = w(42)?;
        let name_at = p + 46;
        let name = cd.get(name_at..name_at + nlen).ok_or_else(|| bad("entry name"))?;
        let name = String::from_utf8_lossy(name).into_owned();
        let extra = cd.get(name_at + nlen..name_at + nlen + xlen).ok_or_else(|| bad("entry extra field"))?;
        // zip64 extended information: present values replace the 0xffffffff placeholders, in order
        let mut x = 0usize;
        while x + 4 <= extra.len() {
            let (id, sz) = (u16_at(extra, x).unwrap_or(0), usize::from(u16_at(extra, x + 2).unwrap_or(0)));
            let body = extra.get(x + 4..x + 4 + sz).ok_or_else(|| bad("extra field"))?;
            if id == 1 {
                let mut q = 0usize;
                for v in [&mut usize_, &mut csize, &mut local] {
                    if *v == 0xffff_ffff {
                        *v = u64_at(body, q).ok_or_else(|| bad("zip64 extra field"))?;
                        q += 8;
                    }
                }
            }
            x += 4 + sz;
        }
        if method != 0 || csize != usize_ {
            return Err(bad(&format!("{name} is compressed (only stored entries are supported)")));
        }
        // the data follows the local header, whose name/extra lengths may differ from the directory's
        let lh_at = base.checked_add(local).filter(|&a| a.checked_add(30).is_some_and(|e| e <= end)).ok_or_else(|| bad("local header offset"))?;
        let lh = read(f, lh_at, 30)?;
        if u32_at(&lh, 0) != Some(0x0403_4b50) {
            return Err(bad("local header"));
        }
        let skip = 30 + u64::from(u16_at(&lh, 26).unwrap_or(0)) + u64::from(u16_at(&lh, 28).unwrap_or(0));
        let data = lh_at + skip;
        if data.checked_add(csize).is_none_or(|e| e > end) {
            return Err(bad(&format!("{name} extends past the archive")));
        }
        out.push(Entry { name, offset: data, size: csize });
        p = name_at + nlen + xlen + clen;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nemo::testutil;

    fn index_bytes(prefix: usize, b: &[u8]) -> Result<Vec<Entry>, SpeechError> {
        let mut all = vec![7u8; prefix];
        all.extend_from_slice(b);
        all.extend_from_slice(&[9u8; 33]);
        let p = std::env::temp_dir().join(format!("filmcraft-zip-{}-{}-{}", std::process::id(), prefix, b.len()));
        std::fs::write(&p, &all).unwrap();
        let mut f = File::open(&p).unwrap();
        let r = index(&mut f, prefix as u64, b.len() as u64);
        drop(f);
        let _ = std::fs::remove_file(&p);
        r
    }

    #[test]
    fn indexes_stored_entries_at_an_offset() {
        let z = testutil::zip(&[("arch/data.pkl", b"pickle"), ("arch/data/0", &[1, 2, 3, 4])]);
        let e = index_bytes(1000, &z).unwrap();
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].name, "arch/data.pkl");
        assert_eq!((e[0].offset, e[0].size), (1000 + 30 + 13, 6));
        assert_eq!(e[1].size, 4);
    }

    #[test]
    fn refuses_compressed_and_out_of_range_entries() {
        let mut z = testutil::zip(&[("a", b"xyz")]);
        let cd = z.len() - 22 - 47;
        z[cd + 10] = 8; // deflate
        assert!(index_bytes(0, &z).is_err());
        let mut z = testutil::zip(&[("a", b"xyz")]);
        let cd = z.len() - 22 - 47;
        z[cd + 20..cd + 24].copy_from_slice(&1000u32.to_le_bytes());
        z[cd + 24..cd + 28].copy_from_slice(&1000u32.to_le_bytes());
        assert!(index_bytes(0, &z).is_err());
        assert!(index_bytes(0, b"PK").is_err());
    }
}
