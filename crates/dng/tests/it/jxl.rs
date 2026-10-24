//! JPEG XL DNG (DNG 1.7, Compression 52546), feature `jxl`. The JPEG XL strips are made by the
//! reference encoder `cjxl` at test time (skipped when it is not installed; nothing is
//! committed): lossless strips must decode to exactly the samples written.

#[cfg(feature = "jxl")]
use filmcraft_dng::writer::Layout;
use filmcraft_dng::writer::{Compression, Options, write};
#[cfg(feature = "jxl")]
use filmcraft_dng::{Dng, probe};
use filmcraft_dng::{Error, decode};

fn picture(w: usize, h: usize, spp: usize, bits: u32, seed: u32) -> Vec<u16> {
    let mut s = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    let max = (1u32 << bits) - 1;
    (0..w * h * spp)
        .map(|i| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            ((i as u32 * 97 + (s & 255)) % (max + 1)) as u16
        })
        .collect()
}

/// `cjxl` the `w` × `h` × `spp` samples (PGM / PPM at `bits` bits) with `args`.
#[cfg(feature = "jxl")]
fn cjxl(px: &[u16], w: usize, h: usize, spp: usize, bits: u32, args: &[&str]) -> Option<Vec<u8>> {
    use std::process::Command;
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("dng-jxl-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    let n = px.as_ptr() as usize ^ px.len() ^ bits as usize;
    let (src, dst) = (dir.join(format!("{n}.{}", if spp == 3 { "ppm" } else { "pgm" })), dir.join(format!("{n}.jxl")));
    let mut pnm = format!("{}\n{w} {h}\n{}\n", if spp == 3 { "P6" } else { "P5" }, (1u32 << bits) - 1).into_bytes();
    // PNM: one byte per sample up to maxval 255, two (big-endian) above
    if bits <= 8 {
        pnm.extend(px.iter().map(|&v| v as u8));
    } else {
        pnm.extend(px.iter().flat_map(|v| v.to_be_bytes()));
    }
    std::fs::write(&src, pnm).ok()?;
    let ok = Command::new("cjxl").arg(&src).arg(&dst).args(args).output().ok()?.status.success();
    ok.then(|| std::fs::read(&dst).ok()).flatten()
}

#[cfg(feature = "jxl")]
fn jxl_dng(px: &[u16], w: usize, h: usize, spp: usize, bits: u32, rows: usize, args: &[&str]) -> Option<Vec<u8>> {
    let mut external = Vec::new();
    for y in (0..h).step_by(rows) {
        let r = rows.min(h - y);
        external.push(cjxl(&px[y * w * spp..(y + r) * w * spp], w, r, spp, bits, args)?);
    }
    let o = Options {
        width: w,
        height: h,
        samples_per_pixel: spp,
        bits,
        cfa: None,
        layout: Layout::Strips { rows },
        compression: Compression::External { code: 52546 },
        external,
        ..Default::default()
    };
    Some(write(px, &o))
}

#[cfg(feature = "jxl")]
#[test]
fn lossless_jpeg_xl_is_bit_exact() {
    if std::process::Command::new("cjxl").arg("--version").output().is_err() {
        eprintln!("cjxl not installed: skipped");
        return;
    }
    let (w, h) = (37, 21);
    for (spp, bits) in [(1, 16), (1, 8), (3, 12), (3, 16), (1, 10)] {
        for rows in [h, 8] {
            let px = picture(w, h, spp, bits, bits + spp as u32);
            let f = jxl_dng(&px, w, h, spp, bits, rows, &["-d", "0", "-e", "2"]).expect("cjxl");
            let info = probe(&f).unwrap();
            assert_eq!((info.compression, info.bits, info.samples_per_pixel), ("JPEG XL", bits, spp));
            assert_eq!(Dng::parse(&f).unwrap().raw().unwrap().data, px, "{spp} × {bits} bits, {rows} rows");
            assert!(decode(&f).is_ok());
        }
    }
}

#[cfg(feature = "jxl")]
#[test]
fn lossy_jpeg_xl_is_close_and_hostile_files_do_not_panic() {
    if std::process::Command::new("cjxl").arg("--version").output().is_err() {
        return;
    }
    let (w, h) = (32, 24);
    // a smooth 16-bit LinearRaw picture at distance 0.5
    let px: Vec<u16> = (0..w * h * 3).map(|i| (8000 + (i / 3 % w) * 900 + (i / 3 / w) * 500 + (i % 3) * 3000) as u16).collect();
    let f = jxl_dng(&px, w, h, 3, 16, h, &["-d", "0.5"]).expect("cjxl");
    let raw = Dng::parse(&f).unwrap().raw().unwrap();
    let worst = raw.data.iter().zip(&px).map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs()).max().unwrap();
    assert!(worst < 1500, "off by {worst}");
    for n in 0..f.len() {
        assert!(std::panic::catch_unwind(|| decode(&f[..n]).map(|_| ())).is_ok(), "truncated to {n}");
    }
    let mut s = 99u32;
    for i in 0..1500 {
        let mut g = f.clone();
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        let at = s as usize % g.len();
        g[at] ^= 1 << (s >> 29);
        assert!(std::panic::catch_unwind(|| decode(&g).map(|_| ())).is_ok(), "mutation {i}");
    }
}

#[test]
fn jpeg_xl_without_the_feature_or_on_cfa_data_is_named() {
    let px = picture(8, 8, 1, 12, 1);
    // a CFA file claiming JPEG XL
    let o = Options { bits: 12, compression: Compression::External { code: 52546 }, external: vec![vec![0xFF, 0x0A, 0, 0]], ..Default::default() };
    let e = decode(&write(&px, &o)).err();
    assert!(matches!(e, Some(Error::Unsupported(ref m)) if m.contains("JPEG XL")), "{e:?}");
    #[cfg(not(feature = "jxl"))]
    {
        let o = Options { bits: 12, cfa: None, compression: Compression::External { code: 52546 }, external: vec![vec![0xFF, 0x0A]], ..Default::default() };
        assert!(matches!(decode(&write(&px, &o)), Err(Error::Unsupported(m)) if m.contains("feature `jxl`")));
    }
}
