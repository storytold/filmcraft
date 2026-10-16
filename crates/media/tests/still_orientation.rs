//! EXIF Orientation on stills (#1583): a phone photo stored sideways with Orientation 6 must
//! decode upright, its size swapped. The files are built here: EXIF from the EXIF 2.32 / TIFF 6.0
//! IFD layout (tag 0x0112, SHORT, count 1), carried in a JPEG APP1, a PNG eXIf chunk, a WebP EXIF
//! chunk, or as tag 274 of a TIFF.
//!
//! The picture is 3 × 2 blocks of distinct colours, so each of the eight orientations puts them
//! somewhere different. Stored block (x, y) of a W × H picture ends up, for Orientation
//! 1: (x, y) 2: (W-1-x, y) 3: (W-1-x, H-1-y) 4: (x, H-1-y) 5: (y, x) 6: (H-1-y, x)
//! 7: (H-1-y, W-1-x) 8: (y, W-1-x), the picture H × W for 5–8.

use std::sync::Arc;

use filmcraft_media::sequence::{FrameLoader, ImageSequenceSource};
use filmcraft_media::still::{Raster, decode_raster};
use filmcraft_media::{FrameRequest, MediaSource, open_bytes};
use filmcraft_time::{FrameRate, Tick};
use image::ImageEncoder;

const W: u32 = 3;
const H: u32 = 2;
const COLOURS: [[u8; 3]; 6] = [[230, 20, 20], [20, 200, 20], [20, 20, 230], [230, 230, 20], [230, 20, 230], [20, 230, 230]];

/// The stored picture: `W` × `H` blocks of `block` × `block` pixels, RGB8.
fn picture(block: u32) -> Vec<u8> {
    let (w, h) = (W * block, H * block);
    (0..w * h).flat_map(|i| COLOURS[((i / w / block) * W + (i % w) / block) as usize]).collect()
}

/// Where stored block (x, y) lands for EXIF Orientation `o`.
fn oriented(o: u16, x: u32, y: u32) -> (u32, u32) {
    match o {
        2 => (W - 1 - x, y),
        3 => (W - 1 - x, H - 1 - y),
        4 => (x, H - 1 - y),
        5 => (y, x),
        6 => (H - 1 - y, x),
        7 => (H - 1 - y, W - 1 - x),
        8 => (y, W - 1 - x),
        _ => (x, y),
    }
}

/// `r` is the stored picture under Orientation `o` (1 for "unchanged"), each block's centre pixel
/// within `tol` of its colour.
fn assert_oriented(r: &Raster, o: u16, block: u32, tol: u8, what: &str) {
    let (w, h) = if (5..=8).contains(&o) { (H, W) } else { (W, H) };
    assert_eq!((r.width, r.height, r.rgba.len()), (w * block, h * block, (w * h * block * block * 4) as usize), "{what}: size");
    for y in 0..H {
        for x in 0..W {
            let (ox, oy) = oriented(o, x, y);
            let (px, py) = (ox * block + block / 2, oy * block + block / 2);
            let i = ((py * r.width + px) * 4) as usize;
            let got = &r.rgba[i..i + 4];
            let want = COLOURS[(y * W + x) as usize];
            let close = got[..3].iter().zip(want).all(|(a, b)| a.abs_diff(b) <= tol) && got[3] == 255;
            assert!(close, "{what}: stored block ({x}, {y}) should be at ({ox}, {oy}), found {got:?} there, want {want:?}");
        }
    }
}

/// A little-endian EXIF block (a TIFF header and one IFD) with `entries` (tag, type, count,
/// value field).
fn exif(entries: &[(u16, u16, u32, [u8; 4])]) -> Vec<u8> {
    let mut out = b"II*\0".to_vec();
    out.extend(8u32.to_le_bytes());
    out.extend((entries.len() as u16).to_le_bytes());
    for (tag, ty, count, value) in entries {
        out.extend(tag.to_le_bytes());
        out.extend(ty.to_le_bytes());
        out.extend(count.to_le_bytes());
        out.extend(value);
    }
    out.extend(0u32.to_le_bytes());
    out
}

/// EXIF with only an Orientation tag of value `o`.
fn orientation_exif(o: u16) -> Vec<u8> {
    let [a, b] = o.to_le_bytes();
    exif(&[(0x0112, 3, 1, [a, b, 0, 0])])
}

fn jpeg(exif: Option<Vec<u8>>) -> Vec<u8> {
    // quality 100 (all quantisers 1) and 4:4:4, flat 8 × 8 blocks: within a few levels
    let mut out = Vec::new();
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 100);
    if let Some(e) = exif {
        enc.set_exif_metadata(e).unwrap();
    }
    enc.write_image(&picture(8), W * 8, H * 8, image::ExtendedColorType::Rgb8).unwrap();
    out
}

fn png(o: u16) -> Vec<u8> {
    let mut out = Vec::new();
    let mut enc = image::codecs::png::PngEncoder::new(&mut out);
    enc.set_exif_metadata(orientation_exif(o)).unwrap();
    enc.write_image(&picture(1), W, H, image::ExtendedColorType::Rgb8).unwrap();
    out
}

fn webp(o: u16) -> Vec<u8> {
    let mut out = Vec::new();
    let mut enc = image::codecs::webp::WebPEncoder::new_lossless(&mut out);
    enc.set_exif_metadata(orientation_exif(o)).unwrap();
    enc.write_image(&picture(1), W, H, image::ExtendedColorType::Rgb8).unwrap();
    out
}

/// An uncompressed, single-strip, little-endian TIFF of `width` × `height`, 8 bits × 3 samples,
/// PhotometricInterpretation `photometric` (2 RGB, 6 YCbCr), plus `extra` entries (tag, SHORT
/// values).
fn tiff(width: u32, height: u32, photometric: u16, data: &[u8], extra: &[(u16, Vec<u16>)]) -> Vec<u8> {
    let mut entries: Vec<(u16, u16, Vec<u32>)> = vec![
        (256, 4, vec![width]),
        (257, 4, vec![height]),
        (258, 3, vec![8, 8, 8]),
        (259, 3, vec![1]),
        (262, 3, vec![u32::from(photometric)]),
        (273, 4, vec![0]), // patched below
        (277, 3, vec![3]),
        (278, 4, vec![height]),
        (279, 4, vec![data.len() as u32]),
    ];
    entries.extend(extra.iter().map(|(tag, v)| (*tag, 3, v.iter().map(|&v| u32::from(v)).collect())));
    entries.sort_by_key(|e| e.0);
    let ifd_at = 8u32;
    let data_at = ifd_at + 2 + 12 * entries.len() as u32 + 4;
    let mut values_at = data_at + data.len() as u32;
    let mut ifd: Vec<u8> = (entries.len() as u16).to_le_bytes().to_vec();
    let mut extra_data: Vec<u8> = Vec::new();
    for (tag, ty, vals) in &entries {
        let vals = if *tag == 273 { vec![data_at] } else { vals.clone() };
        let mut bytes: Vec<u8> = Vec::new();
        for v in &vals {
            if *ty == 3 { bytes.extend((*v as u16).to_le_bytes()) } else { bytes.extend(v.to_le_bytes()) }
        }
        ifd.extend(tag.to_le_bytes());
        ifd.extend(ty.to_le_bytes());
        ifd.extend((vals.len() as u32).to_le_bytes());
        if bytes.len() <= 4 {
            bytes.resize(4, 0);
            ifd.extend(&bytes);
        } else {
            ifd.extend(values_at.to_le_bytes());
            values_at += bytes.len() as u32;
            extra_data.extend(bytes);
        }
    }
    ifd.extend(0u32.to_le_bytes());
    let mut out = b"II*\0".to_vec();
    out.extend(ifd_at.to_le_bytes());
    out.extend(ifd);
    out.extend(data);
    out.extend(extra_data);
    out
}

fn rgb_tiff(o: u16) -> Vec<u8> {
    tiff(W, H, 2, &picture(1), &[(274, vec![o])])
}

#[test]
fn jpeg_exif_orientation_is_applied() {
    for o in 1..=8 {
        let r = decode_raster("photo.jpg", &jpeg(Some(orientation_exif(o)))).unwrap();
        assert_oriented(&r, o, 8, 8, &format!("JPEG orientation {o}"));
        assert!(!r.has_alpha);
    }
}

#[test]
fn big_endian_exif_is_read_too() {
    let mut e = b"MM\0*".to_vec();
    e.extend(8u32.to_be_bytes());
    e.extend(1u16.to_be_bytes());
    e.extend(0x0112u16.to_be_bytes());
    e.extend(3u16.to_be_bytes());
    e.extend(1u32.to_be_bytes());
    e.extend([0, 6, 0, 0]);
    e.extend(0u32.to_be_bytes());
    let r = decode_raster("photo.jpg", &jpeg(Some(e))).unwrap();
    assert_oriented(&r, 6, 8, 8, "big-endian JPEG orientation 6");
}

#[test]
fn png_exif_chunk_orientation_is_applied() {
    for o in 1..=8 {
        assert_oriented(&decode_raster("shot.png", &png(o)).unwrap(), o, 1, 0, &format!("PNG orientation {o}"));
    }
}

#[test]
fn webp_exif_chunk_orientation_is_applied() {
    for o in 1..=8 {
        assert_oriented(&decode_raster("shot.webp", &webp(o)).unwrap(), o, 1, 0, &format!("WebP orientation {o}"));
    }
}

#[test]
fn rgb_tiff_orientation_tag_is_applied() {
    for o in 1..=8 {
        assert_oriented(&decode_raster("scan.tiff", &rgb_tiff(o)).unwrap(), o, 1, 0, &format!("TIFF orientation {o}"));
    }
}

#[test]
fn opened_still_reports_the_upright_size() {
    let s = open_bytes("portrait.jpg", Arc::from(jpeg(Some(orientation_exif(6)))), &[]).unwrap();
    let v = s.info().video.as_ref().unwrap();
    assert_eq!((v.width, v.height), (H * 8, W * 8));
    let f = s.video_frame(FrameRequest::full(Tick::ZERO)).unwrap();
    assert_eq!((f.width, f.height), (v.width, v.height));
    let filmcraft_frame::PixelData::Rgba8(p) = &f.data else { panic!("not RGBA8") };
    assert_oriented(&Raster { width: f.width, height: f.height, rgba: p.to_vec(), has_alpha: false }, 6, 8, 8, "opened JPEG");
}

#[test]
fn image_sequence_frames_are_upright() {
    let files: std::collections::HashMap<String, Vec<u8>> = [("s1.png".to_string(), png(8)), ("s2.png".to_string(), png(8))].into_iter().collect();
    let loader: FrameLoader = Arc::new(move |p: &str| files.get(p).cloned().ok_or_else(|| std::io::Error::other("missing")));
    let s = ImageSequenceSource::new("s1.png", vec![Some("s1.png".into()), Some("s2.png".into())], FrameRate::FPS_24, loader).unwrap();
    let v = s.info().video.as_ref().unwrap();
    assert_eq!((v.width, v.height), (H, W));
    for i in 0..2 {
        let f = s.video_frame(FrameRequest::full(FrameRate::FPS_24.tick_of(i))).unwrap();
        let filmcraft_frame::PixelData::Rgba8(p) = &f.data else { panic!("not RGBA8") };
        assert_oriented(&Raster { width: f.width, height: f.height, rgba: p.to_vec(), has_alpha: false }, 8, 1, 0, &format!("sequence frame {i}"));
    }
}

/// Frames of one sequence may carry different orientations: each decodes upright at its own size,
/// and the sequence keeps the first frame's size (frames of another size are scaled to fit, as for
/// any mixed-size sequence).
#[test]
fn sequence_frames_with_different_orientations_each_decode_upright() {
    let files: std::collections::HashMap<String, Vec<u8>> = [("m1.png".to_string(), png(1)), ("m2.png".to_string(), png(6))].into_iter().collect();
    let loader: FrameLoader = Arc::new(move |p: &str| files.get(p).cloned().ok_or_else(|| std::io::Error::other("missing")));
    let s = ImageSequenceSource::new("m1.png", vec![Some("m1.png".into()), Some("m2.png".into())], FrameRate::FPS_24, loader).unwrap();
    let v = s.info().video.as_ref().unwrap();
    assert_eq!((v.width, v.height), (W, H));
    for (i, o) in [(0, 1), (1, 6)] {
        let f = s.video_frame(FrameRequest::full(FrameRate::FPS_24.tick_of(i))).unwrap();
        let filmcraft_frame::PixelData::Rgba8(p) = &f.data else { panic!("not RGBA8") };
        assert_oriented(&Raster { width: f.width, height: f.height, rgba: p.to_vec(), has_alpha: false }, o, 1, 0, &format!("sequence frame {i}"));
    }
}

#[test]
fn missing_invalid_or_damaged_orientation_leaves_the_picture_alone() {
    let six = orientation_exif(6);
    let cases: Vec<(&str, Option<Vec<u8>>)> = vec![
        ("no EXIF", None),
        ("orientation 1", Some(orientation_exif(1))),
        ("orientation 0", Some(orientation_exif(0))),
        ("orientation 9", Some(orientation_exif(9))),
        ("orientation 255", Some(orientation_exif(255))),
        ("orientation 65535", Some(orientation_exif(65_535))),
        ("orientation as a LONG", Some(exif(&[(0x0112, 4, 1, [6, 0, 0, 0])]))),
        ("two orientation values", Some(exif(&[(0x0112, 3, 2, [6, 0, 6, 0])]))),
        ("EXIF cut inside the entry", Some(six[..14].to_vec())),
        ("EXIF cut before the value", Some(six[..18].to_vec())),
        ("IFD offset past the end", Some([&b"II*\0"[..], &u32::MAX.to_le_bytes()].concat())),
        ("not a TIFF header", Some(b"garbage".to_vec())),
    ];
    for (what, e) in cases {
        let r = decode_raster("photo.jpg", &jpeg(e)).unwrap_or_else(|err| panic!("{what}: {err}"));
        assert_oriented(&r, 1, 8, 8, what);
    }
    for (what, o) in [("PNG orientation 0", 0), ("PNG orientation 9", 9)] {
        assert_oriented(&decode_raster("shot.png", &png(o)).unwrap(), 1, 1, 0, what);
    }
    for (what, o) in [("TIFF orientation 0", 0), ("TIFF orientation 9", 9), ("TIFF orientation 255", 255)] {
        assert_oriented(&decode_raster("scan.tiff", &rgb_tiff(o)).unwrap(), 1, 1, 0, what);
    }
}

/// YCbCr TIFFs go through our own decoder, which does not read Orientation yet: the tag is
/// ignored, as before.
#[test]
fn ycbcr_tiff_orientation_is_not_applied_yet() {
    // 4:4:4 grey levels (Cb = Cr = 128), one per pixel, full range
    let levels = [10u8, 50, 90, 130, 170, 210];
    let data: Vec<u8> = levels.iter().flat_map(|&y| [y, 128, 128]).collect();
    let r = decode_raster("frame.tiff", &tiff(W, H, 6, &data, &[(274, vec![6]), (530, vec![1, 1])])).unwrap();
    assert_eq!((r.width, r.height), (W, H));
    for (i, &y) in levels.iter().enumerate() {
        let got = &r.rgba[i * 4..i * 4 + 3];
        assert!(got.iter().all(|c| c.abs_diff(y) <= 2), "pixel {i}: {got:?}, want grey {y}");
    }
}

#[test]
fn damaged_oriented_files_fail_without_panicking() {
    let good_jpeg = jpeg(Some(orientation_exif(6)));
    let mut cases: Vec<Vec<u8>> = Vec::new();
    for good in [&good_jpeg, &png(6), &webp(6), &rgb_tiff(6)] {
        cases.extend((0..good.len()).map(|n| good[..n].to_vec()));
        for i in 0..good.len() {
            for bit in [0, 3, 7] {
                let mut b = good.clone();
                b[i] ^= 1 << bit;
                cases.push(b);
            }
        }
    }
    // EXIF with a huge entry count and an IFD offset pointing at itself
    cases.push(jpeg(Some([&b"II*\0"[..], &8u32.to_le_bytes(), &u16::MAX.to_le_bytes()].concat())));
    cases.push(jpeg(Some([&b"II*\0"[..], &0u32.to_le_bytes()].concat())));
    for (i, b) in cases.into_iter().enumerate() {
        assert!(std::panic::catch_unwind(|| decode_raster("damaged.jpg", &b).map(|r| r.rgba.len())).is_ok(), "case {i} panicked");
    }

    // a 65535 × 65535 SOF behind the orientation: an error, not a 12 GB allocation
    let mut huge = good_jpeg.clone();
    let sof = huge.windows(2).position(|w| w == [0xFF, 0xC0]).unwrap();
    huge[sof + 5..sof + 9].copy_from_slice(&[0xFF; 4]);
    assert!(decode_raster("huge.jpg", &huge).is_err());
}
