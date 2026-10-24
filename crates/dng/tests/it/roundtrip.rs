//! Round trips through the test writer: container parsing, bit-exact raw decoding (uncompressed at
//! every depth, lossless JPEG with every predictor), the processing maths against hand-computed
//! values, CinemaDNG metadata, and hostile files.

use filmcraft_dng::color::{D50, D65, bradford, inverse, mul, rec709_to_xyz, xy_to_xyz};
use filmcraft_dng::tiff::tags;
use filmcraft_dng::writer::{Compression, Layout, Options, Value, lj92, write};
use filmcraft_dng::{Dng, Error, Timecode, decode, ljpeg, probe, sniff};

/// A deterministic pseudo-random sequence.
fn noise(n: usize, bits: u32, seed: u32) -> Vec<u16> {
    let mut s = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            (s & ((1u32 << bits) - 1)) as u16
        })
        .collect()
}

/// A smooth picture with some noise (like a real sensor), `bits` deep.
fn picture(w: usize, h: usize, spp: usize, bits: u32, seed: u32) -> Vec<u16> {
    let max = (1u32 << bits) - 1;
    let n = noise(w * h * spp, 4.min(bits), seed);
    (0..w * h * spp)
        .map(|i| {
            let (x, y) = ((i / spp) % w, (i / spp) / w);
            let v = (x * 37 + y * 23 + (i % spp) * 101) as u32 % (max + 1);
            (v ^ u32::from(n[i])).min(max) as u16
        })
        .collect()
}

#[test]
fn sniff_and_container() {
    let px = picture(16, 8, 1, 12, 1);
    for be in [false, true] {
        for preview in [false, true] {
            let o = Options { width: 16, height: 8, bits: 12, big_endian: be, preview, ..Default::default() };
            let f = write(&px, &o);
            assert!(sniff(&f), "be {be} preview {preview}");
            let d = Dng::parse(&f).unwrap();
            let info = d.info().unwrap();
            assert_eq!((info.raw_width, info.raw_height, info.width, info.height, info.bits), (16, 8, 16, 8, 12));
            assert!(info.cfa);
            assert_eq!(info.dng_version, [1, 4, 0, 0]);
            assert_eq!(info.camera.as_deref(), Some("FilmCraft Synthetic"));
            assert_eq!(d.raw().unwrap().data, px, "be {be} preview {preview}");
        }
    }
    // a plain TIFF is not a DNG
    let mut plain = write(&px, &Options::default());
    // remove the DNGVersion tag by renaming it to an unknown tag number
    let pos = find_tag(&plain, tags::DNG_VERSION).unwrap();
    plain[pos..pos + 2].copy_from_slice(&40000u16.to_le_bytes());
    assert!(!sniff(&plain));
    assert_eq!(Dng::parse(&plain).err(), Some(Error::NotDng));
    assert!(!sniff(b"\x89PNG\r\n\x1a\n"));
    assert!(!sniff(b""));
}

/// Offset of the directory entry for `tag` in a little-endian file's first IFD.
fn find_tag(f: &[u8], tag: u16) -> Option<usize> {
    let ifd = u32::from_le_bytes(f[4..8].try_into().unwrap()) as usize;
    let n = u16::from_le_bytes(f[ifd..ifd + 2].try_into().unwrap()) as usize;
    (0..n).map(|i| ifd + 2 + 12 * i).find(|&e| u16::from_le_bytes(f[e..e + 2].try_into().unwrap()) == tag)
}

#[test]
fn uncompressed_every_depth_strips_and_tiles() {
    for bits in [8, 9, 10, 12, 14, 16] {
        for (spp, cfa) in [(1, true), (3, false)] {
            for layout in [Layout::Strips { rows: 3 }, Layout::Strips { rows: 100 }, Layout::Tiles { width: 16, height: 8 }] {
                for be in [false, true] {
                    let (w, h) = (21, 13);
                    let px = picture(w, h, spp, bits, bits + spp as u32);
                    let o = Options {
                        width: w,
                        height: h,
                        samples_per_pixel: spp,
                        bits,
                        big_endian: be,
                        layout,
                        cfa: cfa.then(|| (2, 2, vec![0, 1, 1, 2])),
                        ..Default::default()
                    };
                    let raw = Dng::parse(&write(&px, &o)).unwrap().raw().unwrap();
                    assert_eq!(raw.data, px, "bits {bits} spp {spp} {layout:?} be {be}");
                    assert_eq!(raw.bits, bits);
                }
            }
        }
    }
}

#[test]
fn lossless_jpeg_every_predictor_bit_exact() {
    for predictor in 1..=7u8 {
        for bits in [8u32, 12, 14, 16] {
            for (components, layout) in
                [(1, Layout::Strips { rows: 5 }), (2, Layout::Tiles { width: 16, height: 16 }), (4, Layout::Tiles { width: 8, height: 4 })]
            {
                let (w, h) = (36, 22);
                let px = picture(w, h, 1, bits, u32::from(predictor) * 7 + bits);
                let o = Options {
                    width: w,
                    height: h,
                    bits,
                    layout,
                    compression: Compression::LosslessJpeg { predictor, components, restart_lines: 0, pt: 0 },
                    ..Default::default()
                };
                let f = write(&px, &o);
                assert_eq!(probe(&f).unwrap().compression, "lossless JPEG");
                let raw = Dng::parse(&f).unwrap().raw().unwrap();
                assert_eq!(raw.data, px, "predictor {predictor} bits {bits} components {components}");
            }
        }
    }
}

#[test]
fn lossless_jpeg_extremes_restarts_and_point_transform() {
    // full-swing 16-bit noise hits every difference category including SSSS = 16
    let (w, h) = (17, 9);
    let mut px = noise(w * h * 2, 16, 3);
    px[0] = 0;
    px[2] = 32_768; // 0 after the 2^15 initial prediction is a difference of 32768 (SSSS 16)
    for predictor in 1..=7 {
        let j = lj92(&px, w, h, 2, 16, predictor, 0, 0);
        let d = ljpeg::decode(&j, 1 << 20).unwrap();
        assert_eq!((d.width, d.height, d.components, d.precision, d.predictor), (w, h, 2, 16, predictor));
        assert_eq!(d.samples, px, "predictor {predictor}");
        // restart markers every 2 and every 3 lines
        for r in [2, 3] {
            let j = lj92(&px, w, h, 2, 16, predictor, r, 0);
            assert!(j.windows(2).any(|b| b == [0xFF, 0xD0]), "has RST0");
            assert_eq!(ljpeg::decode(&j, 1 << 20).unwrap().samples, px, "predictor {predictor} restart {r}");
        }
    }
    // point transform: the low bits are dropped and the rest shifted back
    let px = picture(12, 6, 1, 14, 9);
    let j = lj92(&px, 12, 6, 1, 14, 4, 0, 2);
    let want: Vec<u16> = px.iter().map(|v| v >> 2 << 2).collect();
    assert_eq!(ljpeg::decode(&j, 1 << 20).unwrap().samples, want);
    // the sample cap
    assert_eq!(ljpeg::decode(&lj92(&px, 12, 6, 1, 14, 1, 0, 0), 71).err(), Some(Error::TooLarge));
}

/// The colour tags of a camera whose white-balanced values are Rec. 709 already.
fn rec709_camera_tags(neutral: [f64; 3]) -> Vec<(u16, Value)> {
    let cm = inverse(&rec709_to_xyz().unwrap()).unwrap();
    let fm = mul(&bradford(&xy_to_xyz(D65), &xy_to_xyz(D50)).unwrap(), &rec709_to_xyz().unwrap());
    let flat = |m: [[f64; 3]; 3]| m.iter().flatten().copied().collect::<Vec<f64>>();
    vec![
        (tags::COLOR_MATRIX_1, Value::srationals(&flat(cm))),
        (tags::FORWARD_MATRIX_1, Value::srationals(&flat(fm))),
        (tags::CALIBRATION_ILLUMINANT_1, Value::Short(vec![21])),
        (tags::AS_SHOT_NEUTRAL, Value::rationals(&neutral)),
    ]
}

#[test]
fn develop_matches_hand_computed_values() {
    // 12-bit RGGB, black 64, white 4064 (range 4000); four colour quadrants seen through a camera
    // with neutral (0.5, 1, 0.8). White-balanced values come out as Rec. 709 (the ForwardMatrix
    // is Rec. 709 → XYZ D50), so a quadrant of colour c reads back c.
    let (w, h) = (32, 24);
    let neutral: [f64; 3] = [0.5, 1.0, 0.8];
    let quad = |x: usize, y: usize| -> [f64; 3] {
        match (x < w / 2, y < h / 2) {
            (true, true) => [0.5, 0.5, 0.5],
            (false, true) => [0.8, 0.2, 0.1],
            (true, false) => [0.05, 0.6, 0.3],
            (false, false) => [0.25, 0.25, 0.9],
        }
    };
    let pat = [0u8, 1, 1, 2];
    let px: Vec<u16> = (0..w * h)
        .map(|i| {
            let (x, y) = (i % w, i / w);
            let c = usize::from(pat[(y % 2) * 2 + x % 2]);
            let camera = quad(x, y)[c] * neutral[c];
            (64.0 + camera * 4000.0).round() as u16
        })
        .collect();
    let mut main = rec709_camera_tags(neutral);
    main.push((tags::BASELINE_EXPOSURE, Value::SRational(vec![(0, 1)])));
    let raw_tags = vec![(tags::BLACK_LEVEL, Value::Short(vec![64])), (tags::WHITE_LEVEL, Value::Short(vec![4064]))];
    for compression in [Compression::None, Compression::LosslessJpeg { predictor: 6, components: 2, restart_lines: 0, pt: 0 }] {
        let o = Options { width: w, height: h, bits: 12, compression, main_tags: main.clone(), raw_tags: raw_tags.clone(), ..Default::default() };
        let img = decode(&write(&px, &o)).unwrap();
        assert_eq!((img.width, img.height), (w, h));
        for y in 0..h {
            for x in 0..w {
                // away from the quadrant edges (the demosaic reaches two pixels)
                if (x as isize - w as isize / 2).abs() < 3 || (y as isize - h as isize / 2).abs() < 3 {
                    continue;
                }
                let got = &img.rgba[(y * w + x) * 4..][..4];
                let want = quad(x, y);
                for k in 0..3 {
                    assert!((got[k] - want[k] as f32).abs() < 2e-3, "({x},{y}) ch {k}: {} vs {}", got[k], want[k]);
                }
                assert_eq!(got[3], 1.0);
            }
        }
    }
    // BaselineExposure +1: twice as bright
    let mut main1 = rec709_camera_tags(neutral);
    main1.push((tags::BASELINE_EXPOSURE, Value::SRational(vec![(1, 1)])));
    let o = Options { width: w, height: h, bits: 12, main_tags: main1, raw_tags, ..Default::default() };
    let img = decode(&write(&px, &o)).unwrap();
    assert!((img.rgba[(4 * w + 4) * 4] - 1.0).abs() < 4e-3, "{}", img.rgba[(4 * w + 4) * 4]);
}

#[test]
fn active_area_default_crop_and_linear_raw() {
    // LinearRaw 3 samples, 10 × 6, active area cuts one pixel off each side, crop 4 × 2 at (2, 1)
    let (w, h) = (10, 6);
    let px: Vec<u16> = (0..w * h).flat_map(|i| [i as u16 * 100, 50, 7]).collect();
    let o = Options {
        width: w,
        height: h,
        samples_per_pixel: 3,
        bits: 16,
        cfa: None,
        raw_tags: vec![
            (tags::ACTIVE_AREA, Value::Long(vec![1, 1, 5, 9])),
            (tags::DEFAULT_CROP_ORIGIN, Value::Long(vec![2, 1])),
            (tags::DEFAULT_CROP_SIZE, Value::Rational(vec![(8, 2), (2, 1)])),
            (tags::WHITE_LEVEL, Value::Long(vec![65_535])),
        ],
        ..Default::default()
    };
    let f = write(&px, &o);
    let info = probe(&f).unwrap();
    assert_eq!((info.width, info.height, info.cfa, info.samples_per_pixel), (4, 2, false, 3));
    let img = decode(&f).unwrap();
    assert_eq!((img.width, img.height), (4, 2));
    // output (0, 0) is raw (1 + 2, 1 + 1) = sample 23
    assert!((img.rgba[0] - 2300.0 / 65_535.0).abs() < 1e-6);
    assert!((img.rgba[1] - 50.0 / 65_535.0).abs() < 1e-6);
    // output (3, 1) is raw (6, 3) = sample 36
    assert!((img.rgba[(4 + 3) * 4] - 3600.0 / 65_535.0).abs() < 1e-6);
}

#[test]
fn cinemadng_frame_rate_time_code_and_reel() {
    let px = picture(8, 8, 1, 12, 2);
    let o = Options {
        bits: 12,
        main_tags: vec![
            (tags::FRAME_RATE, Value::SRational(vec![(24_000, 1001)])),
            // 01:02:03:04, drop frame
            (tags::TIME_CODES, Value::Byte(vec![0x44, 0x03, 0x02, 0x01, 0, 0, 0, 0])),
            (tags::REEL_NAME, Value::Ascii("A001".into())),
        ],
        ..Default::default()
    };
    let info = probe(&write(&px, &o)).unwrap();
    assert_eq!(info.frame_rate, Some((24_000, 1001)));
    assert_eq!(info.timecode, Some(Timecode { hours: 1, minutes: 2, seconds: 3, frames: 4, drop_frame: true }));
    assert_eq!(info.reel.as_deref(), Some("A001"));
    // nonsense rates and time codes are ignored
    let o = Options {
        bits: 12,
        main_tags: vec![(tags::FRAME_RATE, Value::SRational(vec![(-25, 1)])), (tags::TIME_CODES, Value::Byte(vec![0x0A, 0x99, 0, 0, 0, 0, 0, 0]))],
        ..Default::default()
    };
    let info = probe(&write(&px, &o)).unwrap();
    assert_eq!((info.frame_rate, info.timecode), (None, None));
}

/// Every hostile variant must give an error or a picture, never a panic.
fn survives(f: &[u8]) -> bool {
    std::panic::catch_unwind(|| {
        let _ = probe(f);
        let _ = decode(f);
    })
    .is_ok()
}

#[test]
fn hostile_files_are_errors_not_panics() {
    let px = picture(24, 16, 1, 12, 5);
    let files: Vec<Vec<u8>> = [
        Options { width: 24, height: 16, bits: 12, ..Default::default() },
        Options { width: 24, height: 16, bits: 12, preview: true, layout: Layout::Tiles { width: 16, height: 16 }, ..Default::default() },
        Options {
            width: 24,
            height: 16,
            bits: 12,
            compression: Compression::LosslessJpeg { predictor: 4, components: 2, restart_lines: 2, pt: 0 },
            main_tags: rec709_camera_tags([0.5, 1.0, 0.7]),
            ..Default::default()
        },
    ]
    .iter()
    .map(|o| write(&px, o))
    .collect();
    for f in &files {
        assert!(decode(f).is_ok());
        // every truncation
        for n in 0..f.len() {
            assert!(survives(&f[..n]), "truncated to {n}");
            if n < f.len() / 2 {
                assert!(decode(&f[..n]).is_err(), "truncated to {n}");
            }
        }
        // bit flips and corrupt bytes everywhere
        let mut s = 12345u32;
        for i in 0..4000 {
            let mut g = f.clone();
            for _ in 0..1 + i % 4 {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                let at = s as usize % g.len();
                g[at] = if i % 3 == 0 { 0xFF } else { g[at] ^ (1 << (s >> 29)) };
            }
            assert!(survives(&g), "mutation {i}");
        }
    }
}

#[test]
fn cyclic_ifds_huge_sizes_and_bad_offsets() {
    let px = picture(8, 8, 1, 16, 4);
    let f = write(&px, &Options { preview: true, ..Default::default() });
    let ifd0 = u32::from_le_bytes(f[4..8].try_into().unwrap()) as usize;
    let n = u16::from_le_bytes(f[ifd0..ifd0 + 2].try_into().unwrap()) as usize;
    // IFD 0's next pointer to itself
    let mut g = f.clone();
    g[ifd0 + 2 + 12 * n..ifd0 + 6 + 12 * n].copy_from_slice(&(ifd0 as u32).to_le_bytes());
    assert!(decode(&g).is_ok(), "a self-loop ends the chain");
    // the SubIFD pointing back at IFD 0
    let mut g = f.clone();
    let sub = find_tag(&g, tags::SUB_IFDS).unwrap();
    g[sub + 8..sub + 12].copy_from_slice(&(ifd0 as u32).to_le_bytes());
    assert!(matches!(decode(&g), Err(Error::Unsupported(_))), "no raw image left");
    // huge dimensions with a tiny file
    for (wtag, v) in [(tags::IMAGE_WIDTH, 65_535u32), (tags::IMAGE_LENGTH, 65_535), (tags::IMAGE_WIDTH, 0), (tags::IMAGE_WIDTH, u32::MAX)] {
        let mut g = write(&px, &Options::default());
        let e = find_tag(&g, wtag).unwrap();
        g[e + 8..e + 12].copy_from_slice(&v.to_le_bytes());
        assert!(decode(&g).is_err(), "{wtag} = {v}");
    }
    // a strip offset past the end of the file
    let mut g = write(&px, &Options::default());
    let e = find_tag(&g, tags::STRIP_OFFSETS).unwrap();
    g[e + 8..e + 12].copy_from_slice(&0xFFFF_FF00u32.to_le_bytes());
    assert_eq!(decode(&g).err(), Some(Error::Truncated));
    // an IFD offset past the end
    let mut g = write(&px, &Options::default());
    g[4..8].copy_from_slice(&0x7FFF_FFFFu32.to_le_bytes());
    assert!(decode(&g).is_err());
    // a rational with a zero denominator in a raw tag
    let o = Options { raw_tags: vec![(tags::DEFAULT_CROP_SIZE, Value::Rational(vec![(8, 0), (8, 1)]))], ..Default::default() };
    assert!(matches!(decode(&write(&px, &o)), Err(Error::Malformed(_))));
    // unsupported compressions and depths are named
    let o = Options { raw_tags: vec![(tags::COMPRESSION, Value::Short(vec![34892]))], ..Default::default() };
    assert_eq!(decode(&write(&px, &o)).err(), Some(Error::Unsupported("lossy JPEG DNG".into())));
}
