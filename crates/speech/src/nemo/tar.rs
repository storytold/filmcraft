//! A member index of an uncompressed tar archive (POSIX ustar with PAX `path`/`size` records and
//! GNU long names), read with seeks: member data is never copied.
//!
//! Hostile input: header checksums are verified, sizes must fit in the file, PAX records and long
//! names are capped, and the member count is capped.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use crate::SpeechError;

const BLOCK: u64 = 512;
const MAX_MEMBERS: usize = 100_000;
const MAX_META: u64 = 1 << 20;

/// A regular file in the archive.
#[derive(Clone, Debug, PartialEq)]
pub struct Member {
    /// Path without a leading `./`.
    pub name: String,
    /// Absolute offset of the data.
    pub offset: u64,
    pub size: u64,
}

fn bad(msg: &str) -> SpeechError {
    SpeechError::Model(format!("not a valid .nemo archive: {msg}"))
}

/// Parse an octal (or GNU base-256) numeric field.
fn number(field: &[u8]) -> Option<u64> {
    if field.first().is_some_and(|b| b & 0x80 != 0) {
        // base-256: big-endian, first byte's top bit is the marker
        let mut v: u64 = u64::from(field.first()? & 0x7f);
        for &b in field.get(1..)? {
            v = v.checked_mul(256)?.checked_add(u64::from(b))?;
        }
        return Some(v);
    }
    let s: Vec<u8> = field.iter().copied().skip_while(|b| *b == b' ').take_while(|b| (b'0'..=b'7').contains(b)).collect();
    if s.is_empty() {
        return Some(0);
    }
    let mut v: u64 = 0;
    for b in s {
        v = v.checked_mul(8)?.checked_add(u64::from(b - b'0'))?;
    }
    Some(v)
}

fn cstr(field: &[u8]) -> String {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(field.get(..end).unwrap_or_default()).into_owned()
}

fn clean(name: &str) -> String {
    let mut n = name.trim_end_matches('/');
    while let Some(r) = n.strip_prefix("./") {
        n = r;
    }
    n.to_string()
}

/// PAX extended header records: `"<len> <key>=<value>\n"`.
fn pax(data: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = data;
    while !rest.is_empty() {
        let Some(sp) = rest.iter().position(|&b| b == b' ') else { break };
        let Some(len) = std::str::from_utf8(rest.get(..sp).unwrap_or_default()).ok().and_then(|s| s.parse::<usize>().ok()) else { break };
        if len <= sp + 1 || len > rest.len() {
            break;
        }
        let rec = rest.get(sp + 1..len).unwrap_or_default();
        let rec = rec.strip_suffix(b"\n").unwrap_or(rec);
        if let Some(eq) = rec.iter().position(|&b| b == b'=') {
            let k = String::from_utf8_lossy(rec.get(..eq).unwrap_or_default()).into_owned();
            let v = String::from_utf8_lossy(rec.get(eq + 1..).unwrap_or_default()).into_owned();
            out.push((k, v));
        }
        rest = rest.get(len..).unwrap_or_default();
    }
    out
}

/// Index the regular files of the tar archive `f` (`file_len` bytes).
pub fn index(f: &mut File, file_len: u64) -> Result<Vec<Member>, SpeechError> {
    let mut out = Vec::new();
    let mut pos = 0u64;
    let mut next_name: Option<String> = None;
    let mut next_size: Option<u64> = None;
    let mut zero_blocks = 0;
    let mut header = [0u8; 512];
    while pos.checked_add(BLOCK).is_some_and(|e| e <= file_len) {
        f.seek(SeekFrom::Start(pos))?;
        f.read_exact(&mut header)?;
        if pos == 0 && header.starts_with(&[0x1f, 0x8b]) {
            return Err(bad("the archive is gzip-compressed (only uncompressed .nemo archives are supported)"));
        }
        pos += BLOCK;
        if header.iter().all(|&b| b == 0) {
            zero_blocks += 1;
            if zero_blocks == 2 {
                break;
            }
            continue;
        }
        zero_blocks = 0;
        let stored = number(&header[148..156]).ok_or_else(|| bad("header checksum"))?;
        let sum: u64 = header.iter().enumerate().map(|(i, &b)| if (148..156).contains(&i) { 32 } else { u64::from(b) }).sum();
        if stored != sum {
            return Err(bad("header checksum mismatch"));
        }
        let mut size = number(&header[124..136]).ok_or_else(|| bad("member size"))?;
        let typeflag = header[156];
        if let Some(s) = next_size.take()
            && matches!(typeflag, b'0' | 0 | b'7')
        {
            size = s;
        }
        let data = pos;
        let padded = size.checked_add(BLOCK - 1).map(|v| v / BLOCK * BLOCK).ok_or_else(|| bad("member size"))?;
        let end = data.checked_add(size).ok_or_else(|| bad("member size"))?;
        if end > file_len {
            return Err(bad("a member extends past the end of the file"));
        }
        match typeflag {
            b'x' | b'L' => {
                if size > MAX_META {
                    return Err(bad("extended header too large"));
                }
                let mut meta = vec![0u8; size as usize];
                f.read_exact(&mut meta)?;
                if typeflag == b'L' {
                    next_name = Some(cstr(&meta));
                } else {
                    for (k, v) in pax(&meta) {
                        match k.as_str() {
                            "path" => next_name = Some(v),
                            "size" => next_size = Some(v.trim().parse().map_err(|_| bad("PAX size"))?),
                            _ => {}
                        }
                    }
                }
            }
            b'0' | 0 | b'7' => {
                let name = match next_name.take() {
                    Some(n) => n,
                    None => {
                        let prefix = cstr(&header[345..500]);
                        let n = cstr(&header[..100]);
                        if prefix.is_empty() || &header[257..262] != b"ustar" { n } else { format!("{prefix}/{n}") }
                    }
                };
                if out.len() >= MAX_MEMBERS {
                    return Err(bad("too many members"));
                }
                out.push(Member { name: clean(&name), offset: data, size });
            }
            // directories, links, global PAX headers and anything else: skipped
            _ => next_name = None,
        }
        pos = pos.checked_add(padded).ok_or_else(|| bad("member size"))?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nemo::testutil;

    fn index_bytes(b: &[u8]) -> Result<Vec<Member>, SpeechError> {
        let p = std::env::temp_dir().join(format!("filmcraft-tar-{}-{}", std::process::id(), b.len()));
        std::fs::write(&p, b).unwrap();
        let mut f = File::open(&p).unwrap();
        let r = index(&mut f, b.len() as u64);
        drop(f);
        let _ = std::fs::remove_file(&p);
        r
    }

    #[test]
    fn indexes_members_and_pax_names() {
        let mut b = Vec::new();
        let long = format!("./{}/model_config.yaml", "d".repeat(150));
        let rec = format!("path={long}\n");
        let rec = format!("{} {rec}", rec.len() + 3 + 1);
        b.extend_from_slice(&testutil::tar_header("./PaxHeader", rec.len() as u64, b'x'));
        b.extend_from_slice(rec.as_bytes());
        b.resize(b.len().div_ceil(512) * 512, 0);
        b.extend_from_slice(&testutil::tar_header("short", 3, b'0'));
        b.extend_from_slice(b"abc");
        b.resize(b.len().div_ceil(512) * 512, 0);
        b.extend_from_slice(&testutil::tar(&[("./x.txt", b"hello")]));
        let m = index_bytes(&b).unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].name, long.trim_start_matches("./"));
        assert_eq!((m[0].offset, m[0].size), (1024 + 512, 3));
        assert_eq!(m[1].name, "x.txt");
        assert_eq!(m[1].size, 5);
    }

    #[test]
    fn rejects_bad_checksums_and_sizes() {
        let mut b = testutil::tar(&[("a", b"0123456789")]);
        b[0] = b'b';
        assert!(index_bytes(&b).is_err());
        let mut b = testutil::tar(&[("a", b"0123456789")]);
        // size field claims far more data than the file has (checksum recomputed)
        let mut h = testutil::tar_header("a", 1 << 32, b'0');
        b[..512].copy_from_slice(&h);
        assert!(index_bytes(&b).is_err());
        h[124] = 0x80; // base-256 marker with garbage
        assert!(index_bytes(&h).is_err());
        let mut gz = vec![0x1f, 0x8b, 8, 0];
        gz.resize(2048, 0);
        assert!(index_bytes(&gz).unwrap_err().to_string().contains("gzip"));
        assert_eq!(number(b"0000012\0"), Some(10));
        assert_eq!(number(&[0x80, 0, 0, 1, 0]), Some(256));
        assert_eq!(number(&[0xff; 12]), None);
    }
}
