//! A minimal ZIP reader for `.drp` archives (PKWARE *APPNOTE.TXT* 6.3.10: end of central
//! directory, central directory headers, local file headers; methods 0 *stored* and 8 *deflated*).
//! ZIP64, encryption and multi-disk archives are not used by DaVinci Resolve and are rejected.
//!
//! Every size and offset comes from the file, so all of them are checked; an entry inflates to at
//! most its declared size, capped at [`MAX_ENTRY`].

/// Largest entry we inflate (the XML of a large project is a few tens of MB).
pub(crate) const MAX_ENTRY: usize = 512 << 20;
/// Most entries we read from one archive.
const MAX_ENTRIES: usize = 100_000;

const EOCD_SIG: u32 = 0x0605_4b50;
const CDIR_SIG: u32 = 0x0201_4b50;
const LOCAL_SIG: u32 = 0x0403_4b50;

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    let s = b.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes([s[0], s[1]]))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// Whether `bytes` start like a ZIP archive.
pub(crate) fn sniff(bytes: &[u8]) -> bool {
    bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06")
}

/// One file of an archive: its name and contents.
pub(crate) struct Entry {
    pub name: String,
    pub data: Vec<u8>,
}

/// The names of the files in an archive (without inflating anything).
pub(crate) fn names(bytes: &[u8]) -> Result<Vec<String>, String> {
    Ok(directory(bytes)?.into_iter().map(|e| e.name).collect())
}

struct DirEntry {
    name: String,
    method: u16,
    compressed: usize,
    size: usize,
    local: usize,
}

fn directory(bytes: &[u8]) -> Result<Vec<DirEntry>, String> {
    // The end of central directory record is 22 bytes plus a comment of up to 65535 bytes.
    let min_start = bytes.len().saturating_sub(22 + 0xffff);
    let eocd = (min_start..=bytes.len().saturating_sub(22))
        .rev()
        .find(|&i| u32_at(bytes, i) == Some(EOCD_SIG))
        .ok_or("not a ZIP archive (no end of central directory)")?;
    let count = u16_at(bytes, eocd + 10).ok_or("truncated ZIP directory")? as usize;
    let dir_offset = u32_at(bytes, eocd + 16).ok_or("truncated ZIP directory")? as usize;
    if count == 0xffff || dir_offset == 0xffff_ffff {
        return Err("ZIP64 archives are not supported".into());
    }
    if count > MAX_ENTRIES {
        return Err(format!("the archive lists {count} files"));
    }
    let mut out = Vec::with_capacity(count);
    let mut p = dir_offset;
    for _ in 0..count {
        if u32_at(bytes, p) != Some(CDIR_SIG) {
            return Err("damaged ZIP central directory".into());
        }
        let flags = u16_at(bytes, p + 8).ok_or("truncated ZIP directory")?;
        let method = u16_at(bytes, p + 10).ok_or("truncated ZIP directory")?;
        let compressed = u32_at(bytes, p + 20).ok_or("truncated ZIP directory")? as usize;
        let size = u32_at(bytes, p + 24).ok_or("truncated ZIP directory")? as usize;
        let name_len = u16_at(bytes, p + 28).ok_or("truncated ZIP directory")? as usize;
        let extra_len = u16_at(bytes, p + 30).ok_or("truncated ZIP directory")? as usize;
        let comment_len = u16_at(bytes, p + 32).ok_or("truncated ZIP directory")? as usize;
        let local = u32_at(bytes, p + 42).ok_or("truncated ZIP directory")? as usize;
        let name_at = p + 46;
        let name = bytes.get(name_at..name_at + name_len).ok_or("truncated ZIP directory")?;
        let name = String::from_utf8_lossy(name).replace('\\', "/");
        if flags & 1 != 0 {
            return Err(format!("{name}: encrypted ZIP entries are not supported"));
        }
        out.push(DirEntry { name, method, compressed, size, local });
        p = name_at + name_len + extra_len + comment_len;
    }
    Ok(out)
}

/// Read every file of the archive for which `want(name)` is true.
pub(crate) fn read(bytes: &[u8], want: impl Fn(&str) -> bool) -> Result<Vec<Entry>, String> {
    let mut out = Vec::new();
    let mut total = 0usize;
    for e in directory(bytes)? {
        if e.name.ends_with('/') || !want(&e.name) {
            continue;
        }
        if u32_at(bytes, e.local) != Some(LOCAL_SIG) {
            return Err(format!("{}: damaged ZIP entry", e.name));
        }
        let name_len = u16_at(bytes, e.local + 26).ok_or("truncated ZIP entry")? as usize;
        let extra_len = u16_at(bytes, e.local + 28).ok_or("truncated ZIP entry")? as usize;
        let start = e.local + 30 + name_len + extra_len;
        let raw = start.checked_add(e.compressed).and_then(|end| bytes.get(start..end)).ok_or_else(|| format!("{}: truncated ZIP entry", e.name))?;
        if e.size > MAX_ENTRY {
            return Err(format!("{}: entry too large ({} bytes)", e.name, e.size));
        }
        let data = match e.method {
            0 => raw.to_vec(),
            8 => {
                miniz_oxide::inflate::decompress_to_vec_with_limit(raw, e.size).map_err(|err| format!("{}: damaged deflate data ({:?})", e.name, err.status))?
            }
            m => return Err(format!("{}: unsupported ZIP compression method {m}", e.name)),
        };
        total = total.saturating_add(data.len());
        if total > MAX_ENTRY {
            return Err("the archive is too large".into());
        }
        out.push(Entry { name: e.name, data });
    }
    Ok(out)
}

/// Write a ZIP archive (tests only: stored entries, or deflated when `deflate`).
#[cfg(test)]
pub(crate) fn write(files: &[(&str, &[u8])], deflate: bool) -> Vec<u8> {
    fn crc32(b: &[u8]) -> u32 {
        let mut c = 0xffff_ffffu32;
        for &x in b {
            c ^= x as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { (c >> 1) ^ 0xedb8_8320 } else { c >> 1 };
            }
        }
        !c
    }
    let mut out = Vec::new();
    let mut dir = Vec::new();
    for (name, data) in files {
        let packed = if deflate { miniz_oxide::deflate::compress_to_vec(data, 6) } else { data.to_vec() };
        let method: u16 = if deflate { 8 } else { 0 };
        let crc = crc32(data);
        let local = out.len() as u32;
        for (o, head) in [(&mut out, false), (&mut dir, true)] {
            o.extend_from_slice(&(if head { CDIR_SIG } else { LOCAL_SIG }).to_le_bytes());
            if head {
                o.extend_from_slice(&20u16.to_le_bytes());
            }
            o.extend_from_slice(&20u16.to_le_bytes());
            o.extend_from_slice(&0u16.to_le_bytes());
            o.extend_from_slice(&method.to_le_bytes());
            o.extend_from_slice(&[0, 0, 0, 0]);
            o.extend_from_slice(&crc.to_le_bytes());
            o.extend_from_slice(&(packed.len() as u32).to_le_bytes());
            o.extend_from_slice(&(data.len() as u32).to_le_bytes());
            o.extend_from_slice(&(name.len() as u16).to_le_bytes());
            o.extend_from_slice(&0u16.to_le_bytes());
            if head {
                o.extend_from_slice(&[0; 6]);
                o.extend_from_slice(&[0; 4]);
                o.extend_from_slice(&local.to_le_bytes());
            }
            o.extend_from_slice(name.as_bytes());
        }
        out.extend_from_slice(&packed);
    }
    let dir_at = out.len() as u32;
    let n = files.len() as u16;
    let dir_len = dir.len() as u32;
    out.extend_from_slice(&dir);
    out.extend_from_slice(&EOCD_SIG.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&n.to_le_bytes());
    out.extend_from_slice(&n.to_le_bytes());
    out.extend_from_slice(&dir_len.to_le_bytes());
    out.extend_from_slice(&dir_at.to_le_bytes());
    out.extend_from_slice(&[0; 2]);
    out
}
