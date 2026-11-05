//! External oracle: LibRaw's `unprocessed_raw` sample program (an independent DNG reader) must
//! read the raw samples of our synthetic DNGs exactly as written and as we decode them: packed
//! uncompressed data at every depth in both byte orders, and lossless JPEG with every predictor.
//! Skipped when `unprocessed_raw` is not installed (or `FILMCRAFT_UNPROCESSED_RAW` names it).
//! LibRaw is only run, never linked or read.

use std::path::PathBuf;
use std::process::Command;

use filmcraft_dng::Dng;
use filmcraft_dng::writer::{Compression, Layout, Options, write};

fn tool() -> Option<String> {
    let t = std::env::var("FILMCRAFT_UNPROCESSED_RAW").unwrap_or_else(|_| "unprocessed_raw".into());
    Command::new(&t).arg("-q").output().ok().map(|_| t)
}

fn picture(w: usize, h: usize, bits: u32, seed: u32) -> Vec<u16> {
    let mut s = seed.wrapping_mul(2_654_435_761).wrapping_add(7);
    let max = (1u32 << bits) - 1;
    (0..w * h)
        .map(|i| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            let (x, y) = (i % w, i / w);
            (((x * 97 + y * 41) as u32 % (max + 1)) ^ (s & 15)).min(max) as u16
        })
        .collect()
}

/// The 16-bit samples of a binary PGM.
fn pgm(b: &[u8]) -> Option<(usize, usize, Vec<u16>)> {
    let mut fields = Vec::new();
    let mut i = 0;
    while fields.len() < 4 {
        while b.get(i)?.is_ascii_whitespace() {
            i += 1;
        }
        let start = i;
        while !b.get(i)?.is_ascii_whitespace() {
            i += 1;
        }
        fields.push(std::str::from_utf8(&b[start..i]).ok()?.to_string());
    }
    let (w, h): (usize, usize) = (fields[1].parse().ok()?, fields[2].parse().ok()?);
    let data = b.get(i + 1..)?;
    Some((w, h, data.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes(*c)).collect()))
}

#[test]
fn libraw_reads_our_raw_samples() {
    let Some(tool) = tool() else {
        eprintln!("SKIPPED: unprocessed_raw (LibRaw samples) not installed");
        return;
    };
    let dir: PathBuf = std::env::temp_dir().join(format!("filmcraft-dng-oracle-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (w, h) = (40, 26);
    let mut cases: Vec<(String, Options, u32)> = Vec::new();
    for bits in [8, 10, 12, 14, 16] {
        for be in [false, true] {
            cases.push((
                format!("u{bits}{}", if be { "be" } else { "le" }),
                Options { width: w, height: h, bits, big_endian: be, layout: Layout::Strips { rows: 7 }, ..Default::default() },
                bits,
            ));
        }
    }
    for predictor in 1..=7 {
        for components in [1, 2] {
            let o = Options {
                width: w,
                height: h,
                bits: 14,
                layout: Layout::Tiles { width: 16, height: 16 },
                compression: Compression::LosslessJpeg { predictor, components, restart_lines: 0, pt: 0 },
                ..Default::default()
            };
            cases.push((format!("lj{predictor}c{components}"), o, 14));
        }
    }
    for (i, (name, o, bits)) in cases.iter().enumerate() {
        let px = picture(w, h, *bits, i as u32);
        let f = write(&px, o);
        assert_eq!(Dng::parse(&f).unwrap().raw().unwrap().data, px, "{name}: ours");
        let path = dir.join(format!("{name}.dng"));
        std::fs::write(&path, &f).unwrap();
        let out = Command::new(&tool).arg("-q").arg(&path).output().unwrap();
        assert!(out.status.success(), "{name}: {}", String::from_utf8_lossy(&out.stderr));
        let (pw, ph, theirs) = pgm(&std::fs::read(dir.join(format!("{name}.dng.pgm"))).unwrap()).unwrap();
        assert_eq!((pw, ph), (w, h), "{name}");
        assert_eq!(theirs, px, "{name}: LibRaw");
        eprintln!("{name}: LibRaw agrees");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
