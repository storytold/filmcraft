//! The faster decoder and pipeline must give exactly what the reference implementation of PR
//! #1068 (`filmcraft_dng::reference`) gives: the same raw samples, and the same f32 bit patterns
//! after development, over every Bayer phase, both demosaic qualities, crops with odd origins and
//! at the borders, active-area offsets, linearisation tables, black-level deltas, lossless-JPEG
//! codes above the stated bit depth, LinearRaw, other CFA patterns and tiny pictures.

use filmcraft_dng::tiff::tags;
use filmcraft_dng::writer::{Compression, Layout, Options, Value, write};
use filmcraft_dng::{DemosaicQuality, Dng, ProcessOptions};

fn noise(n: usize, max: u32, seed: u32) -> Vec<u16> {
    let mut s = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    (0..n)
        .map(|i| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            // a ramp with noise, some samples clipped high and low
            let v = (i as u32 * 37) % (max + 1);
            (match s % 23 {
                0 => 0,
                1 => max,
                _ => (v ^ (s & 63)).min(max),
            }) as u16
        })
        .collect()
}

fn same_bits(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
}

fn colour_tags() -> Vec<(u16, Value)> {
    let cm = [0.9f64, -0.3, -0.05, -0.4, 1.2, 0.2, -0.05, 0.15, 0.6];
    vec![
        (tags::COLOR_MATRIX_1, Value::srationals(&cm)),
        (tags::CALIBRATION_ILLUMINANT_1, Value::Short(vec![21])),
        (tags::AS_SHOT_NEUTRAL, Value::rationals(&[0.55, 1.0, 0.7])),
    ]
}

/// Raw samples and every development option of `f` agree with the reference.
fn check(f: &[u8], what: &str) {
    let d = Dng::parse(f).unwrap();
    assert_eq!(d.raw().unwrap(), d.reference_raw().unwrap(), "{what}: raw");
    for demosaic in [DemosaicQuality::High, DemosaicQuality::Fast] {
        let o = ProcessOptions { demosaic, ..Default::default() };
        let (a, b) = (d.develop_with(&o).unwrap(), d.reference_develop(&o).unwrap());
        assert_eq!((a.width, a.height), (b.width, b.height), "{what} {demosaic:?}");
        assert!(same_bits(&a.rgba, &b.rgba), "{what} {demosaic:?}: developed values differ");
    }
}

#[test]
fn bayer_phases_crops_active_areas_linearisation_and_deltas() {
    let phases = [[0u8, 1, 1, 2], [1, 0, 2, 1], [1, 2, 0, 1], [2, 1, 1, 0]];
    let sizes = [(1, 1), (2, 2), (3, 3), (4, 5), (5, 3), (7, 6), (26, 18)];
    for (pi, phase) in phases.iter().enumerate() {
        for &(w, h) in &sizes {
            for variant in 0..6 {
                let px = noise(w * h, 4095, (pi * 100 + w * 7 + h + variant) as u32);
                let mut raw_tags = vec![(tags::BLACK_LEVEL, Value::Short(vec![60])), (tags::WHITE_LEVEL, Value::Short(vec![4000]))];
                match variant {
                    // an active area with an odd offset, a crop with an odd origin inside it
                    1 if w > 4 && h > 4 => {
                        raw_tags.push((tags::ACTIVE_AREA, Value::Long(vec![1, 1, h as u32 - 1, w as u32])));
                        raw_tags.push((tags::DEFAULT_CROP_ORIGIN, Value::Long(vec![1, 1])));
                        raw_tags.push((tags::DEFAULT_CROP_SIZE, Value::Long(vec![w as u32 - 3, h as u32 - 3])));
                    }
                    // a crop touching the right and bottom borders
                    2 if w > 2 && h > 2 => {
                        raw_tags.push((tags::DEFAULT_CROP_ORIGIN, Value::Long(vec![1, 2.min(h as u32 - 1)])));
                        raw_tags.push((tags::DEFAULT_CROP_SIZE, Value::Long(vec![w as u32, h as u32])));
                    }
                    // a linearisation table
                    3 => raw_tags.push((tags::LINEARIZATION_TABLE, Value::Short((0..4096u32).map(|v| (v * v / 4096) as u16).collect()))),
                    // per-site black levels with row and column deltas (the direct path)
                    4 => {
                        raw_tags.push((tags::BLACK_LEVEL_REPEAT_DIM, Value::Short(vec![2, 2])));
                        raw_tags.push((tags::BLACK_LEVEL, Value::Short(vec![50, 61, 72, 83])));
                        raw_tags.push((tags::BLACK_LEVEL_DELTA_H, Value::srationals(&(0..w).map(|x| x as f64 * 0.5).collect::<Vec<_>>())));
                        raw_tags.push((tags::BLACK_LEVEL_DELTA_V, Value::srationals(&(0..h).map(|y| -(y as f64) * 0.25).collect::<Vec<_>>())));
                    }
                    // per-site black levels without deltas (the lookup table path)
                    5 => {
                        raw_tags.push((tags::BLACK_LEVEL_REPEAT_DIM, Value::Short(vec![2, 2])));
                        raw_tags.push((tags::BLACK_LEVEL, Value::Short(vec![50, 61, 72, 83])));
                    }
                    _ => {}
                }
                let o = Options {
                    width: w,
                    height: h,
                    bits: 12,
                    cfa: Some((2, 2, phase.to_vec())),
                    compression: Compression::LosslessJpeg { predictor: 1 + (variant % 7) as u8, components: 1, restart_lines: 0, pt: 0 },
                    main_tags: colour_tags(),
                    raw_tags,
                    ..Default::default()
                };
                check(&write(&px, &o), &format!("phase {phase:?} {w}x{h} variant {variant}"));
            }
        }
    }
}

#[test]
fn lossless_jpeg_codes_above_the_bit_depth_and_layouts() {
    // 16-bit lossless JPEG data in a file that says 12 bits: codes above 4095 reach the decoder
    let (w, h) = (40, 22);
    let px = noise(w * h, 65_535, 9);
    for (layout, components, restart_lines, predictor) in [
        (Layout::Strips { rows: h }, 2, 0, 1),
        (Layout::Strips { rows: 5 }, 1, 2, 6),
        (Layout::Tiles { width: 16, height: 16 }, 2, 0, 4),
        (Layout::Tiles { width: 32, height: 8 }, 4, 1, 7),
    ] {
        for be in [false, true] {
            let o = Options {
                width: w,
                height: h,
                bits: 16,
                big_endian: be,
                layout,
                compression: Compression::LosslessJpeg { predictor, components, restart_lines, pt: 0 },
                main_tags: colour_tags(),
                raw_tags: vec![(tags::BITS_PER_SAMPLE, Value::Short(vec![12]))],
                ..Default::default()
            };
            check(&write(&px, &o), &format!("{layout:?} {components} components restart {restart_lines} be {be}"));
        }
    }
}

#[test]
fn uncompressed_linear_raw_mono_and_other_patterns() {
    let (w, h) = (19, 11);
    for bits in [8, 10, 14, 16] {
        let max = (1u32 << bits) - 1;
        // three-sample LinearRaw
        let px = noise(w * h * 3, max, bits);
        let o = Options {
            width: w,
            height: h,
            samples_per_pixel: 3,
            bits,
            cfa: None,
            main_tags: colour_tags(),
            layout: Layout::Strips { rows: 4 },
            ..Default::default()
        };
        check(&write(&px, &o), &format!("LinearRaw {bits}-bit"));
        // monochrome
        let px = noise(w * h, max, bits + 1);
        let o = Options { width: w, height: h, bits, cfa: None, layout: Layout::Tiles { width: 16, height: 16 }, ..Default::default() };
        check(&write(&px, &o), &format!("mono {bits}-bit"));
    }
    // non-Bayer colour filter arrays (bilinear for both qualities)
    for (r, c, p) in [(1, 3, vec![0u8, 1, 2]), (2, 4, vec![0, 1, 2, 1, 1, 2, 1, 0]), (2, 2, vec![0, 1, 2, 1])] {
        let px = noise(w * h, 4095, 3);
        let o = Options { width: w, height: h, bits: 12, cfa: Some((r, c, p.clone())), main_tags: colour_tags(), ..Default::default() };
        check(&write(&px, &o), &format!("CFA {r}x{c} {p:?}"));
    }
}

#[test]
fn reduced_resolution_covers_the_target_and_keeps_flat_colours() {
    let (w, h) = (64, 40);
    // a flat colour: every 2 × 2 site has its own constant level
    let pat = [0usize, 1, 1, 2];
    let level = [1000u16, 2000, 2000, 3000];
    let px: Vec<u16> = (0..w * h).map(|i| level[pat[(i / w % 2) * 2 + i % 2]]).collect();
    let o = Options { width: w, height: h, bits: 12, main_tags: colour_tags(), ..Default::default() };
    let f = write(&px, &o);
    let d = Dng::parse(&f).unwrap();
    let full = d.develop().unwrap();
    for (target, want) in [((64, 40), (64, 40)), ((32, 20), (32, 20)), ((30, 10), (32, 20)), ((16, 10), (16, 10)), ((1, 1), (8, 5)), ((40, 40), (64, 40))] {
        let img = d.develop_with(&ProcessOptions { target_size: Some(target), ..Default::default() }).unwrap();
        assert_eq!((img.width, img.height), want, "target {target:?}");
        assert_eq!(img.rgba.len(), want.0 * want.1 * 4);
        // a flat field develops to the same colour at every size
        for (a, b) in img.rgba.iter().zip(&full.rgba) {
            assert!((a - b).abs() < 1e-5, "target {target:?}: {a} vs {b}");
        }
    }
    // LinearRaw and monochrome reduce too
    let lin: Vec<u16> = (0..w * h).flat_map(|_| [500u16, 1500, 2500]).collect();
    let o = Options { width: w, height: h, samples_per_pixel: 3, bits: 12, cfa: None, ..Default::default() };
    let img = filmcraft_dng::decode_with(&write(&lin, &o), &ProcessOptions { target_size: Some((10, 10)), ..Default::default() }).unwrap();
    assert_eq!((img.width, img.height), (16, 10));
    assert!((img.rgba[0] - 500.0 / 4095.0).abs() < 1e-6);
}
