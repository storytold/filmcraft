//! Lossy JPEG DNG (Compression 34892, DNG 1.4): 8-bit LinearRaw tiles or strips of one or three
//! components, usually with a LinearizationTable taking the 8-bit codes back to linear values.

use filmcraft_dng::tiff::tags;
use filmcraft_dng::writer::{Compression, Layout, Options, Value, lossy_jpeg, write};
use filmcraft_dng::{Dng, Error, decode, probe};

/// A smooth 8-bit picture (what JPEG is good at).
fn smooth(w: usize, h: usize, spp: usize) -> Vec<u16> {
    (0..w * h * spp)
        .map(|i| {
            let (x, y, s) = ((i / spp) % w, (i / spp) / w, i % spp);
            (40.0 + 80.0 * (1.0 + ((x as f32 + 3.0 * s as f32) / 9.0).sin() * ((y as f32) / 7.0).cos())) as u16
        })
        .collect()
}

fn lossy(px: &[u16], w: usize, h: usize, spp: usize, layout: Layout, quality: u8, raw_tags: Vec<(u16, Value)>) -> Vec<u8> {
    let o = Options {
        width: w,
        height: h,
        samples_per_pixel: spp,
        bits: 8,
        cfa: None,
        layout,
        compression: Compression::LossyJpeg { quality },
        raw_tags,
        ..Default::default()
    };
    write(px, &o)
}

#[test]
fn lossy_jpeg_tiles_and_strips_one_and_three_components() {
    let (w, h) = (40, 28);
    for spp in [1, 3] {
        let px = smooth(w, h, spp);
        for layout in [Layout::Strips { rows: h }, Layout::Strips { rows: 9 }, Layout::Tiles { width: 16, height: 16 }] {
            let f = lossy(&px, w, h, spp, layout, 100, vec![]);
            let info = probe(&f).unwrap();
            assert_eq!((info.compression, info.bits, info.cfa, info.samples_per_pixel), ("lossy JPEG", 8, false, spp));
            let raw = Dng::parse(&f).unwrap().raw().unwrap();
            assert_eq!(raw.data.len(), px.len());
            // quality 100 is close to the original (colour goes through YCbCr)
            let worst = raw.data.iter().zip(&px).map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs()).max().unwrap();
            assert!(worst <= 4, "spp {spp} {layout:?}: off by {worst}");
            // a picture comes out
            let img = decode(&f).unwrap();
            assert_eq!((img.width, img.height), (w, h));
        }
    }
}

#[test]
fn samples_are_the_jpeg_decoders_exactly() {
    // one strip = one JPEG: our samples are the decoded JPEG's, in raster order
    let (w, h) = (24, 16);
    for spp in [1, 3] {
        let px = smooth(w, h, spp);
        let f = lossy(&px, w, h, spp, Layout::Strips { rows: h }, 75, vec![]);
        let jpeg = lossy_jpeg(&px, w, h, spp, 75);
        let want: Vec<u16> = match image::load_from_memory(&jpeg).unwrap() {
            image::DynamicImage::ImageLuma8(g) => g.into_raw().into_iter().map(u16::from).collect(),
            image::DynamicImage::ImageRgb8(c) => c.into_raw().into_iter().map(u16::from).collect(),
            other => panic!("{:?}", other.color()),
        };
        assert_eq!(Dng::parse(&f).unwrap().raw().unwrap().data, want, "spp {spp}");
    }
}

#[test]
fn linearisation_table_expands_the_codes() {
    // grey 8-bit codes through a square-law table to 16 bits, as lossy DNG writers do
    let (w, h) = (16, 16);
    let px = vec![128u16; w * h];
    let table: Vec<u16> = (0..256u32).map(|v| (v * v * 65_535 / (255 * 255)) as u16).collect();
    let f = lossy(
        &px,
        w,
        h,
        1,
        Layout::Strips { rows: h },
        90,
        vec![(tags::LINEARIZATION_TABLE, Value::Short(table.clone())), (tags::WHITE_LEVEL, Value::Long(vec![65_535]))],
    );
    let img = decode(&f).unwrap();
    let code = Dng::parse(&f).unwrap().raw().unwrap().data[0];
    let want = f32::from(table[usize::from(code)]) / 65_535.0;
    assert!((127..=129).contains(&code), "{code}");
    assert!((img.rgba[0] - want).abs() < 1e-6 && (img.rgba[0] - 0.252).abs() < 0.01, "{}", img.rgba[0]);
}

#[test]
fn only_8_bit_linear_raw_and_hostile_lossy_files() {
    let (w, h) = (16, 16);
    // CFA or 12-bit lossy JPEG is outside the specification
    let o = Options { width: w, height: h, bits: 12, raw_tags: vec![(tags::COMPRESSION, Value::Short(vec![34892]))], ..Default::default() };
    let e = decode(&write(&vec![100; w * h], &o)).err();
    assert!(matches!(e, Some(Error::Unsupported(ref m)) if m.contains("lossy JPEG") && m.contains("CFA")), "{e:?}");
    let o = Options { width: w, height: h, bits: 12, cfa: None, raw_tags: vec![(tags::COMPRESSION, Value::Short(vec![34892]))], ..Default::default() };
    assert!(matches!(decode(&write(&vec![100; w * h], &o)), Err(Error::Unsupported(m)) if m.contains("12-bit")));
    // a tiny file claiming a huge picture is refused before allocating it
    let mut big = lossy(&smooth(w, h, 3), w, h, 3, Layout::Strips { rows: h }, 80, vec![]);
    let ifd = u32::from_le_bytes(big[4..8].try_into().unwrap()) as usize;
    let n = u16::from_le_bytes(big[ifd..ifd + 2].try_into().unwrap()) as usize;
    for e in (0..n).map(|i| ifd + 2 + 12 * i) {
        let tag = u16::from_le_bytes(big[e..e + 2].try_into().unwrap());
        if tag == tags::IMAGE_WIDTH || tag == tags::IMAGE_LENGTH || tag == tags::ROWS_PER_STRIP {
            big[e + 8..e + 12].copy_from_slice(&60_000u32.to_le_bytes());
        }
    }
    assert!(decode(&big).is_err());
    // truncations and mutations
    let f = lossy(&smooth(w, h, 3), w, h, 3, Layout::Tiles { width: 16, height: 8 }, 85, vec![]);
    for n in 0..f.len() {
        assert!(std::panic::catch_unwind(|| decode(&f[..n]).map(|_| ())).is_ok(), "truncated to {n}");
    }
    let mut s = 4321u32;
    for i in 0..3000 {
        let mut g = f.clone();
        for _ in 0..1 + i % 4 {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            let at = s as usize % g.len();
            g[at] = if i % 3 == 0 { 0xFF } else { g[at] ^ (1 << (s >> 29)) };
        }
        assert!(std::panic::catch_unwind(|| decode(&g).map(|_| ())).is_ok(), "mutation {i}");
    }
}
