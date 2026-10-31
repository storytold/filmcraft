//! YCbCr TIFF stills (#1293): uncompressed and PackBits files with PhotometricInterpretation 6, as
//! ffmpeg writes from YUV sources. The files are built here from TIFF 6.0 (§21 YCbCr images, §9
//! PackBits), little-endian, one strip.

use filmcraft_media::still::decode_raster;

/// A YCbCr TIFF: `width` × `height`, subsampling `sub` (horizontal, vertical), `data` the strip
/// as stored (data units, already compressed when `compression` is 32773), plus `extra` entries
/// (tag, type, values).
fn ycbcr_tiff(width: u32, height: u32, sub: (u16, u16), compression: u16, data: &[u8], extra: &[(u16, u16, Vec<u32>)]) -> Vec<u8> {
    let mut entries: Vec<(u16, u16, Vec<u32>)> = vec![
        (256, 4, vec![width]),
        (257, 4, vec![height]),
        (258, 3, vec![8, 8, 8]),
        (259, 3, vec![u32::from(compression)]),
        (262, 3, vec![6]),
        (273, 4, vec![0]), // patched below
        (277, 3, vec![3]),
        (278, 4, vec![height]),
        (279, 4, vec![data.len() as u32]),
        (530, 3, vec![u32::from(sub.0), u32::from(sub.1)]),
    ];
    entries.extend(extra.iter().cloned());
    entries.sort_by_key(|e| e.0);
    let ifd_at = 8u32;
    let ifd_len = 2 + 12 * entries.len() as u32 + 4;
    let mut extra_data: Vec<u8> = Vec::new();
    let mut ifd: Vec<u8> = (entries.len() as u16).to_le_bytes().to_vec();
    let data_at = ifd_at + ifd_len;
    let mut values_at = data_at + data.len() as u32;
    for (tag, ty, vals) in &entries {
        let vals = if *tag == 273 { vec![data_at] } else { vals.clone() };
        let mut bytes: Vec<u8> = Vec::new();
        for v in &vals {
            match ty {
                3 => bytes.extend((*v as u16).to_le_bytes()),
                5 => bytes.extend(v.to_le_bytes()), // rationals are passed as num, den pairs
                _ => bytes.extend(v.to_le_bytes()),
            }
        }
        let count = if *ty == 5 { vals.len() / 2 } else { vals.len() };
        ifd.extend(tag.to_le_bytes());
        ifd.extend(ty.to_le_bytes());
        ifd.extend((count as u32).to_le_bytes());
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

fn px(r: &filmcraft_media::still::Raster, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * r.width + x) * 4) as usize;
    [r.rgba[i], r.rgba[i + 1], r.rgba[i + 2], r.rgba[i + 3]]
}

fn near(a: [u8; 4], b: [u8; 4]) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 2)
}

#[test]
fn subsampled_ycbcr_places_each_data_unit() {
    // 4 × 2 at 2 × 2: two data units (four Y, then Cb, Cr). Left grey, right red (BT.601, the
    // default YCbCrCoefficients, full range: the default ReferenceBlackWhite).
    let data = [128, 128, 128, 128, 128, 128, 76, 76, 76, 76, 85, 255];
    let r = decode_raster("frame.tiff", &ycbcr_tiff(4, 2, (2, 2), 1, &data, &[])).unwrap();
    assert_eq!((r.width, r.height), (4, 2));
    for y in 0..2 {
        assert!(near(px(&r, 0, y), [128, 128, 128, 255]) && near(px(&r, 1, y), [128, 128, 128, 255]), "{:?}", px(&r, 0, y));
        assert!(near(px(&r, 2, y), [254, 0, 0, 255]) && near(px(&r, 3, y), [254, 0, 0, 255]), "{:?}", px(&r, 2, y));
    }
}

#[test]
fn packbits_ycbcr_with_video_range_reference_levels() {
    // 2 × 1 at 1 × 1 (4:4:4): white then black in video range (16–235, chroma 128 ± 112), the
    // ReferenceBlackWhite ffmpeg writes. PackBits: a literal run of 6 bytes.
    let units = [235, 128, 128, 16, 128, 128];
    let mut data = vec![5u8];
    data.extend(units);
    let rbw = vec![16, 1, 235, 1, 128, 1, 240, 1, 128, 1, 240, 1];
    let r = decode_raster("frame.tif", &ycbcr_tiff(2, 1, (1, 1), 32773, &data, &[(532, 5, rbw)])).unwrap();
    assert!(near(px(&r, 0, 0), [255, 255, 255, 255]), "{:?}", px(&r, 0, 0));
    assert!(near(px(&r, 1, 0), [0, 0, 0, 255]), "{:?}", px(&r, 1, 0));
}

#[test]
fn sizes_that_are_not_a_whole_number_of_data_units_are_padded() {
    // 3 × 3 at 2 × 2: the data units cover 4 × 4 (TIFF 6.0 §21), the extra column and row dropped.
    let unit = |y: u8| [y, y, y, y, 128, 128];
    let data: Vec<u8> = [unit(50), unit(100), unit(150), unit(200)].concat();
    let r = decode_raster("frame.tiff", &ycbcr_tiff(3, 3, (2, 2), 1, &data, &[])).unwrap();
    assert_eq!((r.width, r.height, r.rgba.len()), (3, 3, 36));
    assert!(near(px(&r, 0, 0), [50, 50, 50, 255]) && near(px(&r, 2, 0), [100, 100, 100, 255]));
    assert!(near(px(&r, 0, 2), [150, 150, 150, 255]) && near(px(&r, 2, 2), [200, 200, 200, 255]));
}

#[test]
fn damaged_ycbcr_tiffs_fail_without_panicking() {
    let data: Vec<u8> = (0..4 * 4 / 4 * 6).map(|i| (i * 37 % 256) as u8).collect();
    let good = ycbcr_tiff(4, 4, (2, 2), 1, &data, &[]);
    let mut cases: Vec<Vec<u8>> = (0..good.len()).map(|n| good[..n].to_vec()).collect();
    for i in 0..good.len() {
        for bit in [0, 3, 7] {
            let mut b = good.clone();
            b[i] ^= 1 << bit;
            cases.push(b);
        }
    }
    // A huge size, a zero subsampling factor, and a PackBits run past the strip.
    cases.push(ycbcr_tiff(65_535, 65_535, (2, 2), 1, &data, &[]));
    cases.push(ycbcr_tiff(4, 4, (0, 2), 1, &data, &[]));
    cases.push(ycbcr_tiff(4, 4, (2, 2), 32773, &[0x81, 7], &[]));
    for (i, b) in cases.into_iter().enumerate() {
        assert!(std::panic::catch_unwind(|| decode_raster("damaged.tiff", &b).map(|r| r.rgba.len())).is_ok(), "case {i} panicked");
    }
}

/// ffmpeg (an external oracle) writes YCbCr TIFFs from YUV frames; our decode must match ffmpeg's
/// own RGB decode of the same file. A smooth gradient, so the chroma upsampling (we repeat chroma
/// across a data unit) does not dominate; 95 × 63 so the data units do not line up.
#[test]
fn ffmpeg_ycbcr_tiffs_match_ffmpeg_decode() {
    let Some(ff) = filmcraft_testkit::oracle::ffmpeg_or_skip("YCbCr TIFF oracle") else { return };
    let dir = filmcraft_testkit::fixtures::fixtures_dir("media/tiff_ycbcr");
    for pix_fmt in ["yuv420p", "yuv422p", "yuv444p"] {
        let tiff = dir.join(format!("gradient_fixed_{pix_fmt}.tiff"));
        let made = filmcraft_testkit::fixtures::generate(&tiff, |tmp| {
            std::process::Command::new(&ff)
                // Default gradient colors stay random even with a fixed seed.
                .args(["-y", "-v", "error", "-f", "lavfi", "-i", "gradients=size=95x63:seed=7:c0=0x204060:c1=0xd0a080", "-frames:v", "1", "-pix_fmt", pix_fmt])
                .arg(tmp)
                .status()
                .is_ok_and(|s| s.success())
        });
        let Some(tiff) = made else { continue };
        let out = std::process::Command::new(&ff).args(["-v", "error", "-i"]).arg(&tiff).args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-"]).output().unwrap();
        let ours = decode_raster("gradient.tiff", &std::fs::read(&tiff).unwrap()).unwrap();
        assert_eq!((ours.width, ours.height), (95, 63), "{pix_fmt}");
        let theirs = out.stdout;
        assert_eq!(theirs.len(), 95 * 63 * 3, "{pix_fmt}");
        let worst =
            ours.rgba.as_chunks::<4>().0.iter().zip(theirs.as_chunks::<3>().0).flat_map(|(a, b)| (0..3).map(move |c| a[c].abs_diff(b[c]))).max().unwrap();
        assert!(worst <= 6, "{pix_fmt}: worst channel difference {worst}");
    }
}
