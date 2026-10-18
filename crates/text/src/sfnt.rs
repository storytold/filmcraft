//! Minimal SFNT (TrueType / OpenType / collection) header reader for font discovery: reads only
//! the table directory, `name` and `OS/2` of each face, so scanning a system font folder does not
//! load whole files.

use std::io::{Read, Seek, SeekFrom};

/// Names and style attributes of one face in a font file.
#[derive(Clone, Debug, PartialEq)]
pub struct FaceNames {
    pub index: u32,
    pub family: String,
    pub style: String,
    pub full_name: String,
    pub weight: u16,
    pub italic: bool,
}

fn be16(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*b.get(o)?, *b.get(o + 1)?]))
}
fn be32(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes([*b.get(o)?, *b.get(o + 1)?, *b.get(o + 2)?, *b.get(o + 3)?]))
}

fn read_at<R: Read + Seek>(r: &mut R, off: u64, len: usize) -> Option<Vec<u8>> {
    if len > 4 << 20 {
        return None;
    }
    r.seek(SeekFrom::Start(off)).ok()?;
    let mut v = vec![0u8; len];
    r.read_exact(&mut v).ok()?;
    Some(v)
}

/// Read the names of every face in a font file (a single font or a collection).
pub fn read_faces<R: Read + Seek>(r: &mut R) -> Vec<FaceNames> {
    let Some(head) = read_at(r, 0, 12) else { return Vec::new() };
    let offsets: Vec<u32> = if &head[0..4] == b"ttcf" {
        let n = be32(&head, 8).unwrap_or(0).min(256) as usize;
        let Some(t) = read_at(r, 12, n * 4) else { return Vec::new() };
        (0..n).filter_map(|i| be32(&t, i * 4)).collect()
    } else {
        vec![0]
    };
    offsets.iter().enumerate().filter_map(|(i, &o)| read_face(r, o as u64, i as u32)).collect()
}

/// Names from in-memory font data.
pub fn read_faces_bytes(data: &[u8]) -> Vec<FaceNames> {
    read_faces(&mut std::io::Cursor::new(data))
}

fn read_face<R: Read + Seek>(r: &mut R, off: u64, index: u32) -> Option<FaceNames> {
    let h = read_at(r, off, 12)?;
    let tag = &h[0..4];
    if tag != [0, 1, 0, 0] && tag != b"OTTO" && tag != b"true" {
        return None;
    }
    let n = be16(&h, 4)? as usize;
    let dir = read_at(r, off + 12, n * 16)?;
    let mut name = None;
    let mut os2 = None;
    for i in 0..n {
        let rec = &dir[i * 16..i * 16 + 16];
        let (o, l) = (be32(rec, 8)? as u64, be32(rec, 12)? as usize);
        match &rec[0..4] {
            b"name" => name = Some((o, l)),
            b"OS/2" => os2 = Some((o, l)),
            _ => {}
        }
    }
    let (no, nl) = name?;
    let nb = read_at(r, no, nl)?;
    let get = |id: u16| name_string(&nb, id);
    let family = get(16).or_else(|| get(1))?;
    let style = get(17).or_else(|| get(2)).unwrap_or_else(|| "Regular".into());
    let full_name = get(4).unwrap_or_else(|| format!("{family} {style}"));
    let (mut weight, mut italic) = (400u16, false);
    if let Some((oo, ol)) = os2
        && let Some(ob) = read_at(r, oo, ol.min(78))
    {
        weight = be16(&ob, 4).unwrap_or(400);
        italic = be16(&ob, 62).is_some_and(|fs| fs & 1 != 0);
    }
    let sl = style.to_ascii_lowercase();
    italic |= sl.contains("italic") || sl.contains("oblique");
    Some(FaceNames { index, family, style, full_name, weight, italic })
}

/// A name-table string by id, preferring Windows Unicode English, then any Unicode, then Mac Roman.
fn name_string(b: &[u8], id: u16) -> Option<String> {
    let count = be16(b, 2)? as usize;
    let storage = be16(b, 4)? as usize;
    let mut best: Option<(u8, String)> = None;
    for i in 0..count {
        let r = 6 + i * 12;
        let (plat, enc, lang, nid, len, off) =
            (be16(b, r)?, be16(b, r + 2)?, be16(b, r + 4)?, be16(b, r + 6)?, be16(b, r + 8)? as usize, be16(b, r + 10)? as usize);
        if nid != id {
            continue;
        }
        let raw = b.get(storage + off..storage + off + len)?;
        let (rank, s) = match plat {
            3 if enc == 1 || enc == 10 => {
                let u: Vec<u16> = raw.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
                (if lang == 0x409 { 0 } else { 1 }, String::from_utf16_lossy(&u))
            }
            0 => {
                let u: Vec<u16> = raw.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
                (2, String::from_utf16_lossy(&u))
            }
            1 if enc == 0 => (3, raw.iter().map(|&c| if c < 128 { c as char } else { '?' }).collect()),
            _ => continue,
        };
        if best.as_ref().is_none_or(|(r0, _)| rank < *r0) {
            best = Some((rank, s));
        }
    }
    best.map(|(_, s)| s).filter(|s| !s.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_bundled_names() {
        let f = read_faces_bytes(crate::fonts::INTER_BOLD);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].family, "Inter");
        assert_eq!(f[0].style, "Bold");
        assert_eq!(f[0].weight, 700);
        assert!(!f[0].italic);
        let i = read_faces_bytes(crate::fonts::INTER_ITALIC);
        assert!(i[0].italic, "{:?}", i[0]);
        let n = read_faces_bytes(crate::fonts::NOTO_SERIF_REGULAR);
        assert_eq!(n[0].family, "Noto Serif");
        assert!(read_faces_bytes(b"not a font").is_empty());
    }
}
