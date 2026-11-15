//! Pixel parity with the decoder this crate had before its TIFF reader, lossless JPEG decoder, raw
//! strip / tile decoder and opcode envelopes moved to craft-libs (craft-libs #778): every fixture
//! below is developed five ways and hashed, and the hashes were recorded from the local readers on
//! `main` (1ec744c2). A different hash means a developed f32 differs in at least one bit, or the
//! picture size, the raw image's size, the as-shot temperature / tint or the `Info` changed. Run
//! with `FILMCRAFT_DNG_PARITY_PRINT=1` to print the current hashes.

use filmcraft_dng::opcode::encode;
use filmcraft_dng::tiff::tags;
use filmcraft_dng::writer::{Compression, Layout, Options, Value, write};
use filmcraft_dng::{DemosaicQuality, Dng, Highlights, ProcessOptions, WhiteBalance};

fn noise(n: usize, max: u32, seed: u32) -> Vec<u16> {
    let mut s = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    (0..n)
        .map(|i| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            let v = (i as u32 * 37) % (max + 1);
            (match s % 23 {
                0 => 0,
                1 => max,
                _ => (v ^ (s & 63)).min(max),
            }) as u16
        })
        .collect()
}

/// FNV-1a over 64 bits.
fn fnv(h: &mut u64, bytes: &[u8]) {
    for &b in bytes {
        *h ^= u64::from(b);
        *h = h.wrapping_mul(0x0100_0000_01b3);
    }
}

fn colour() -> Vec<(u16, Value)> {
    let cm = [0.9f64, -0.3, -0.05, -0.4, 1.2, 0.2, -0.05, 0.15, 0.6];
    let cm2 = [1.1f64, -0.4, -0.1, -0.3, 1.3, 0.1, -0.02, 0.2, 0.7];
    vec![
        (tags::COLOR_MATRIX_1, Value::srationals(&cm)),
        (tags::COLOR_MATRIX_2, Value::srationals(&cm2)),
        (tags::CALIBRATION_ILLUMINANT_1, Value::Short(vec![17])),
        (tags::CALIBRATION_ILLUMINANT_2, Value::Short(vec![21])),
        (tags::AS_SHOT_NEUTRAL, Value::rationals(&[0.55, 1.0, 0.7])),
        (tags::BASELINE_EXPOSURE, Value::srationals(&[0.35])),
        (tags::UNIQUE_CAMERA_MODEL, Value::Ascii("Parity Cam".into())),
    ]
}

fn u(v: &[u32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_be_bytes()).collect()
}

fn d(v: &[f64]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_be_bytes()).collect()
}

/// The fixtures: (name, file).
fn fixtures() -> Vec<(&'static str, Vec<u8>)> {
    let mut out = Vec::new();
    let (w, h) = (26, 18);
    let base = || Options { width: w, height: h, main_tags: colour(), ..Default::default() };
    out.push(("bayer16 strips", write(&noise(w * h, 65535, 1), &Options { layout: Layout::Strips { rows: 5 }, ..base() })));
    out.push((
        "bayer12 big-endian preview subifd bggr",
        write(
            &noise(w * h, 4095, 2),
            &Options {
                bits: 12,
                big_endian: true,
                preview: true,
                cfa: Some((2, 2, vec![2, 1, 1, 0])),
                raw_tags: vec![(tags::BLACK_LEVEL, Value::Short(vec![64])), (tags::WHITE_LEVEL, Value::Short(vec![4000]))],
                ..base()
            },
        ),
    ));
    out.push((
        "bayer14 ljpeg p1 2 comps tiles",
        write(
            &noise(32 * 32, 16383, 3),
            &Options {
                width: 32,
                height: 32,
                bits: 14,
                compression: Compression::LosslessJpeg { predictor: 1, components: 2, restart_lines: 0, pt: 0 },
                layout: Layout::Tiles { width: 16, height: 16 },
                cfa: Some((2, 2, vec![1, 0, 2, 1])),
                ..base()
            },
        ),
    ));
    out.push((
        "bayer12 ljpeg p6 restarts strips",
        write(
            &noise(w * h, 4095, 4),
            &Options {
                bits: 12,
                compression: Compression::LosslessJpeg { predictor: 6, components: 1, restart_lines: 2, pt: 0 },
                layout: Layout::Strips { rows: 6 },
                ..base()
            },
        ),
    ));
    out.push((
        "levels pattern deltas active area odd crop",
        write(
            &noise(w * h, 16383, 5),
            &Options {
                bits: 14,
                cfa: Some((2, 2, vec![1, 2, 0, 1])),
                raw_tags: vec![
                    (tags::BLACK_LEVEL_REPEAT_DIM, Value::Short(vec![2, 2])),
                    (tags::BLACK_LEVEL, Value::Rational(vec![(512, 1), (520, 1), (508, 1), (515, 2)])),
                    (tags::BLACK_LEVEL_DELTA_H, Value::srationals(&(0..22).map(|i| f64::from(i % 5) - 2.0).collect::<Vec<_>>())),
                    (tags::BLACK_LEVEL_DELTA_V, Value::srationals(&(0..15).map(|i| f64::from(i % 3)).collect::<Vec<_>>())),
                    (tags::WHITE_LEVEL, Value::Short(vec![15000])),
                    (tags::ACTIVE_AREA, Value::Long(vec![1, 3, 16, 25])),
                    (tags::DEFAULT_CROP_ORIGIN, Value::Rational(vec![(3, 2), (1, 1)])),
                    (tags::DEFAULT_CROP_SIZE, Value::Rational(vec![(17, 1), (11, 1)])),
                ],
                ..base()
            },
        ),
    ));
    let table: Vec<u16> = (0..1024u32).map(|v| ((v * v) / 16) as u16).collect();
    out.push((
        "linearization table",
        write(
            &noise(w * h, 1023, 6),
            &Options {
                bits: 10,
                raw_tags: vec![(tags::LINEARIZATION_TABLE, Value::Short(table.clone())), (tags::WHITE_LEVEL, Value::Long(vec![65000]))],
                ..base()
            },
        ),
    ));
    out.push(("linear raw rgb16", write(&noise(w * h * 3, 65535, 7), &Options { samples_per_pixel: 3, cfa: None, ..base() })));
    out.push((
        "linear raw rgb lossy jpeg",
        write(
            &noise(32 * 24 * 3, 255, 8),
            &Options {
                width: 32,
                height: 24,
                samples_per_pixel: 3,
                bits: 8,
                cfa: None,
                compression: Compression::LossyJpeg { quality: 90 },
                layout: Layout::Tiles { width: 16, height: 16 },
                ..base()
            },
        ),
    ));
    out.push((
        "monochrome lossy jpeg",
        write(
            &noise(24 * 16, 255, 9),
            &Options {
                width: 24,
                height: 16,
                bits: 8,
                cfa: None,
                compression: Compression::LossyJpeg { quality: 75 },
                layout: Layout::Strips { rows: 16 },
                main_tags: vec![],
                ..base()
            },
        ),
    ));
    out.push((
        "monochrome 16",
        write(&noise(w * h, 65535, 10), &Options { cfa: None, main_tags: vec![(tags::BASELINE_EXPOSURE, Value::srationals(&[-0.5]))], ..base() }),
    ));
    out.push((
        "6x6 cfa",
        write(
            &noise(36 * 24, 4095, 11),
            &Options {
                width: 36,
                height: 24,
                bits: 12,
                cfa: Some((6, 6, b"112112110110001001112112110110001001".iter().map(|c| c - b'0').map(|c| [1, 0, 2][usize::from(c)]).collect())),
                ..base()
            },
        ),
    ));
    out.push((
        "cinemadng tags tiles big-endian",
        write(
            &noise(w * h, 65535, 12),
            &Options {
                big_endian: true,
                layout: Layout::Tiles { width: 16, height: 8 },
                main_tags: [
                    colour(),
                    vec![
                        (51044, Value::SRational(vec![(24000, 1001)])),
                        (51043, Value::Byte(vec![0x42, 0x13, 0x59, 0x23, 0, 0, 0, 0])),
                        (51081, Value::Ascii("A001".into())),
                    ],
                ]
                .concat(),
                ..base()
            },
        ),
    ));
    // opcode lists at each stage: MapPolynomial on list 2, a gain map on list 3, bad pixels on list 1
    let poly = encode(&[(8, 0, [u(&[0, 0, h as u32, w as u32, 0, 1, 1, 1, 2]), d(&[0.01, 0.9, 0.05])].concat())]);
    let gain = encode(&[(
        9,
        0,
        [u(&[0, 0, h as u32 - 2, w as u32 - 2, 0, 3, 1, 1, 2, 2]), d(&[0.0, 0.0, 1.0, 1.0]), u(&[0]), d(&[0.0, 0.0]), f32s(&[1.0, 1.2, 0.9, 1.1])].concat(),
    )]);
    let bad = encode(&[(4, 0, u(&[0, 0]))]);
    out.push((
        "opcode lists",
        write(
            &noise(w * h, 4095, 13),
            &Options {
                bits: 12,
                raw_tags: vec![
                    (tags::WHITE_LEVEL, Value::Short(vec![4095])),
                    (tags::OPCODE_LIST_1, Value::Undefined(bad)),
                    (tags::OPCODE_LIST_2, Value::Undefined(poly)),
                    (tags::OPCODE_LIST_3, Value::Undefined(gain)),
                ],
                ..base()
            },
        ),
    ));
    out
}

fn f32s(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_be_bytes()).collect()
}

fn options() -> Vec<(&'static str, ProcessOptions)> {
    vec![
        ("default", ProcessOptions::default()),
        ("fast", ProcessOptions { demosaic: DemosaicQuality::Fast, ..Default::default() }),
        ("opcodes", ProcessOptions { apply_opcodes: true, ..Default::default() }),
        (
            "custom",
            ProcessOptions {
                white_balance: WhiteBalance::Custom { temperature: 4300.0, tint: 7.0 },
                exposure: 0.7,
                highlights: Highlights::Clip,
                ..Default::default()
            },
        ),
        ("half", ProcessOptions { target_size: Some((8, 6)), ..Default::default() }),
    ]
}

fn hash(f: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let d = Dng::parse(f).unwrap();
    fnv(&mut h, format!("{:?}", d.info().unwrap()).as_bytes());
    fnv(&mut h, format!("{:?}", d.as_shot_temperature_tint().unwrap()).as_bytes());
    let raw = d.raw().unwrap();
    fnv(&mut h, format!("{} {} {} {}", raw.width, raw.height, raw.samples_per_pixel, raw.bits).as_bytes());
    for (_, o) in options() {
        match d.develop_with(&o) {
            Ok(img) => {
                fnv(&mut h, format!("{}x{}", img.width, img.height).as_bytes());
                img.rgba.iter().for_each(|v| fnv(&mut h, &v.to_bits().to_le_bytes()));
            }
            Err(e) => fnv(&mut h, format!("{e:?}").as_bytes()),
        }
    }
    h
}

/// Recorded from the local reader on `main` 1ec744c2.
const EXPECTED: &[(&str, u64)] = &[
    ("bayer16 strips", 0x5f89dc4f74daf2a2),
    ("bayer12 big-endian preview subifd bggr", 0xa95e6716ea514df7),
    ("bayer14 ljpeg p1 2 comps tiles", 0x4068dfc881f6685e),
    ("bayer12 ljpeg p6 restarts strips", 0xf438f4cd3a722ba2),
    ("levels pattern deltas active area odd crop", 0xa0e12d7e11b41381),
    ("linearization table", 0xd336eb35864a9ddf),
    ("linear raw rgb16", 0xa0cd05e7e7d6f791),
    ("linear raw rgb lossy jpeg", 0xa8f6a8ab5202cdb2),
    ("monochrome lossy jpeg", 0x7a8bb3ff8d32ae49),
    ("monochrome 16", 0x284d958a1f4c71ff),
    ("6x6 cfa", 0xbee3e94bf8e25f34),
    ("cinemadng tags tiles big-endian", 0xedf973d5ac4e41b7),
    ("opcode lists", 0xb15e6d64c0bad19b),
];

#[test]
fn developed_pixels_match_the_local_decoder_bit_for_bit() {
    let got: Vec<(&str, u64)> = fixtures().into_iter().map(|(name, f)| (name, hash(&f))).collect();
    if std::env::var_os("FILMCRAFT_DNG_PARITY_PRINT").is_some() {
        for (name, h) in &got {
            println!("    (\"{name}\", 0x{h:016x}),");
        }
    }
    assert_eq!(got.len(), EXPECTED.len(), "fixture count");
    let differ: Vec<&str> = got.iter().zip(EXPECTED).filter(|(g, e)| g != e).map(|(g, _)| g.0).collect();
    assert!(differ.is_empty(), "developed output differs from the local decoder for {differ:?}");
}
