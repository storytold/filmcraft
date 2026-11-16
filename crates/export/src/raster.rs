//! Uncompressed still formats written directly from RGBA8 frames: Targa, DPX and OpenEXR.
//!
//! Targa follows the Truevision TGA File Format Specification 2.0 (image type 2, uncompressed
//! true-colour, bottom-left origin, the layout every reader accepts). DPX follows SMPTE ST 268
//! (version 2.0 header): one RGB image element, 10 bits per component packed into 32-bit
//! big-endian words (method A, two padding bits at the bottom), BT.709 transfer and colorimetry.
//! OpenEXR follows the public OpenEXR file layout (version 2, single-part scan-line file): half-float
//! B, G, R (and A) channels, scene-linear Rec.709 values with premultiplied alpha, as EXR readers
//! expect; uncompressed, RLE, ZIPS (zlib per scan line) or ZIP (zlib per 16 scan lines). The
//! compressed modes first split each chunk's bytes into even and odd halves and delta-code them
//! (the format's predictor); a chunk that would not shrink is stored raw, as the format allows.
use crate::{ExportError, ExrCompression, Result};

/// `w * h * 4` when `rgba` holds exactly that many bytes.
fn checked_len(rgba: &[u8], w: u32, h: u32) -> Result<usize> {
    let n = (w as usize).checked_mul(h as usize).and_then(|n| n.checked_mul(4)).ok_or_else(|| ExportError::Encode("frame size".into()))?;
    if w == 0 || h == 0 || rgba.len() != n {
        return Err(ExportError::Encode("frame size".into()));
    }
    Ok(n)
}

/// The RGB bytes of an RGBA8 frame (alpha dropped).
pub(crate) fn rgb(rgba: &[u8], w: u32, h: u32) -> Result<Vec<u8>> {
    checked_len(rgba, w, h)?;
    Ok(rgba.as_chunks::<4>().0.iter().flat_map(|&[r, g, b, _]| [r, g, b]).collect())
}

/// An uncompressed Targa file in bottom-up order.
pub(crate) fn tga(rgba: &[u8], w: u32, h: u32, alpha: bool) -> Result<Vec<u8>> {
    craft_tga::rgba8_raw::encode_rgba8_raw(rgba, w, h, alpha).map_err(|e| match e {
        craft_tga::rgba8_raw::Rgba8EncodeError::FrameSize => ExportError::Encode("frame size".into()),
        craft_tga::rgba8_raw::Rgba8EncodeError::Dimensions => ExportError::Unsupported(format!("Targa frames are at most 65535×65535 (got {w}×{h})")),
        craft_tga::rgba8_raw::Rgba8EncodeError::OutOfMemory => ExportError::Encode(format!("out of memory for a {w}×{h} Targa frame")),
    })
}

/// A 10-bit RGB DPX file.
pub(crate) fn dpx(rgba: &[u8], w: u32, h: u32) -> Result<Vec<u8>> {
    craft_dpx::rgba8_dpx10::encode_rgba8_dpx10(rgba, w, h, "FilmCraft").map_err(|e| match e {
        craft_dpx::rgba8_dpx10::Rgba8EncodeError::FrameSize => ExportError::Encode("frame size".into()),
        craft_dpx::rgba8_dpx10::Rgba8EncodeError::TooLarge => ExportError::Unsupported(format!("a {w}×{h} DPX frame exceeds 4 GB")),
        craft_dpx::rgba8_dpx10::Rgba8EncodeError::OutOfMemory => ExportError::Encode(format!("out of memory for a {w}×{h} DPX frame")),
    })
}

#[cfg(test)]
use craft_exr::rgba8::{half, rle};

/// Write a scanline OpenEXR using FilmCraft's color conversion table.
pub(crate) fn exr(rgba: &[u8], w: u32, h: u32, alpha: bool, compression: ExrCompression) -> Result<Vec<u8>> {
    let compression = match compression {
        ExrCompression::None => craft_exr::rgba8::Compression::None,
        ExrCompression::Rle => craft_exr::rgba8::Compression::Rle,
        ExrCompression::Zips => craft_exr::rgba8::Compression::Zips,
        ExrCompression::Zip => craft_exr::rgba8::Compression::Zip,
    };
    craft_exr::rgba8::write_rgba8(rgba, w, h, alpha, compression, filmcraft_color::srgb_u8_to_linear_table()).map_err(|e| match e {
        craft_exr::rgba8::Error::FrameSize => ExportError::Encode("frame size".into()),
        craft_exr::rgba8::Error::TooLarge => ExportError::Unsupported(format!("an OpenEXR frame of {w}×{h} is too large")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn be32(b: &[u8], at: usize) -> u32 {
        u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
    }

    #[test]
    fn dpx_header_and_packing() {
        // 2×1: pure red, then (1, 128, 255)
        let px = [255, 0, 0, 255, 1, 128, 255, 7];
        let f = dpx(&px, 2, 1).unwrap();
        assert_eq!(&f[0..4], b"SDPX");
        assert_eq!(f.len(), 2048 + 8);
        assert_eq!(be32(&f, 4), 2048);
        assert_eq!(be32(&f, 16), f.len() as u32);
        assert_eq!((be32(&f, 772), be32(&f, 776)), (2, 1));
        assert_eq!((f[800], f[803]), (50, 10));
        assert_eq!(be32(&f, 2048), 1023 << 22);
        let p = be32(&f, 2052);
        assert_eq!(((p >> 22) & 1023, (p >> 12) & 1023, (p >> 2) & 1023, p & 3), (4, 514, 1023, 0));
    }

    #[test]
    fn tga_rows_are_bottom_up_bgr() {
        // 1×2: top red, bottom blue (half transparent)
        let px = [255, 0, 0, 255, 0, 0, 255, 128];
        let f = tga(&px, 1, 2, false).unwrap();
        assert_eq!(&f[..3], &[0, 0, 2]);
        assert_eq!((f[16], f[17]), (24, 0));
        assert_eq!(&f[18..24], &[255, 0, 0, 0, 0, 255]); // blue row first, as BGR
        assert!(f.ends_with(b"TRUEVISION-XFILE.\0"));
        let f = tga(&px, 1, 2, true).unwrap();
        assert_eq!((f[16], f[17]), (32, 8));
        assert_eq!(&f[18..26], &[255, 0, 0, 128, 0, 0, 255, 255]);
    }

    #[test]
    fn half_floats_round_to_nearest_even() {
        for (v, h) in [
            (0.0, 0x0000),
            (-0.0, 0x8000),
            (1.0, 0x3C00),
            (0.5, 0x3800),
            (-2.0, 0xC000),
            (65504.0, 0x7BFF),
            (65520.0, 0x7C00),        // halfway past the largest half: ties to even = infinity
            (1.0e9, 0x7C00),          // overflow
            (6.103_515_6e-5, 0x0400), // smallest normal
            (5.960_464_5e-8, 0x0001), // smallest subnormal, 2^-24
            (2.980_232_2e-8, 0x0000), // 2^-25: halfway to zero, ties to even
            (1.0e-10, 0x0000),
            (1.000_488_3, 0x3C00), // 1 + 2^-11: halfway, ties to even (down)
            (1.001_464_8, 0x3C02), // 1 + 3·2^-11: halfway, ties to even (up)
            (f32::INFINITY, 0x7C00),
        ] {
            assert_eq!(half(v), h, "{v}");
        }
        assert_eq!(half(f32::NAN) & 0x7E00, 0x7E00);
    }

    fn le32(b: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
    }

    /// Where `needle` (an attribute name, NUL-terminated) starts in the header.
    fn find(b: &[u8], needle: &[u8]) -> usize {
        b.windows(needle.len()).position(|w| w == needle).unwrap()
    }

    #[test]
    fn exr_layout() {
        // 2×1: opaque white, then (255, 0, 0) at alpha 0.5
        let px = [255, 255, 255, 255, 255, 0, 0, 128];
        for alpha in [false, true] {
            let f = exr(&px, 2, 1, alpha, ExrCompression::None).unwrap();
            let nch = if alpha { 4 } else { 3 };
            assert_eq!(&f[..8], &[0x76, 0x2F, 0x31, 0x01, 2, 0, 0, 0]);
            let ch = find(&f, b"channels\0chlist\0") + 16;
            assert_eq!(le32(&f, ch) as usize, nch * 18 + 1);
            let names: Vec<u8> = (0..nch).map(|i| f[ch + 4 + i * 18]).collect();
            assert_eq!(names, if alpha { b"ABGR".to_vec() } else { b"BGR".to_vec() });
            let dw = find(&f, b"dataWindow\0box2i\0") + 17;
            assert_eq!((le32(&f, dw), le32(&f, dw + 4), le32(&f, dw + 8), le32(&f, dw + 12)), (16, 0, 0, 1));
            let cmp = find(&f, b"compression\0compression\0") + 24;
            assert_eq!((le32(&f, cmp), f[cmp + 4]), (1, 0));
            // header ends with a NUL; one offset (scan line 0) points at the only chunk
            let end = f.len() - (8 + 2 * nch * 2) - 8;
            assert_eq!(f[end - 1], 0);
            let off = u64::from_le_bytes(f[end..end + 8].try_into().unwrap()) as usize;
            assert_eq!(off, end + 8);
            assert_eq!((le32(&f, off), le32(&f, off + 4) as usize), (0, 2 * nch * 2));
            let hv = |c: usize, x: usize| u16::from_le_bytes(f[off + 8 + (c * 2 + x) * 2..][..2].try_into().unwrap());
            let (b, g, r) = if alpha { (1, 2, 3) } else { (0, 1, 2) };
            // white is 1.0 everywhere; red is linear 1.0 · alpha (premultiplied) or 1.0 flat
            assert_eq!((hv(b, 0), hv(g, 0), hv(r, 0)), (0x3C00, 0x3C00, 0x3C00));
            assert_eq!((hv(b, 1), hv(g, 1)), (0, 0));
            if alpha {
                assert_eq!((hv(0, 0), hv(0, 1)), (0x3C00, half(128.0 / 255.0)));
                assert_eq!(hv(r, 1), half(128.0 / 255.0));
            } else {
                assert_eq!(hv(r, 1), 0x3C00);
            }
        }
    }

    /// A 40×35 test picture: gradients, a flat band and a noisy corner, with varying alpha (35
    /// rows: the last 16-line ZIP chunk is short).
    fn picture() -> (Vec<u8>, u32, u32) {
        let (w, h) = (40_u32, 35_u32);
        let px = (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .flat_map(|(x, y)| {
                let noise = (x * 7919 + y * 104_729) % 251;
                let r = if y < 12 {
                    (x * 6) as u8
                } else if y < 20 {
                    200
                } else {
                    noise as u8
                };
                [r, (y * 7) as u8, ((x + y) * 3) as u8, [255, 128, 0, 64][(x / 10) as usize % 4]]
            })
            .collect();
        (px, w, h)
    }

    /// The chunks of an OpenEXR file we wrote: (first y, data).
    fn chunks(f: &[u8], count: usize) -> Vec<(u32, &[u8])> {
        let end = find(f, b"screenWindowWidth\0float\0") + 24 + 4 + 4 + 1;
        (0..count)
            .map(|k| {
                let off = u64::from_le_bytes(f[end + k * 8..end + k * 8 + 8].try_into().unwrap()) as usize;
                let n = le32(f, off + 4) as usize;
                (le32(f, off), &f[off + 8..off + 8 + n])
            })
            .collect()
    }

    /// Undo [`predict`].
    fn unpredict(t: &[u8]) -> Vec<u8> {
        let mut t = t.to_vec();
        for i in 1..t.len() {
            t[i] = t[i - 1].wrapping_add(t[i]).wrapping_sub(128);
        }
        let half = t.len().div_ceil(2);
        (0..t.len()).map(|i| t[if i % 2 == 0 { i / 2 } else { half + i / 2 }]).collect()
    }

    fn unrle(mut s: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        while let [c, rest @ ..] = s {
            let c = *c as i8;
            if c < 0 {
                out.extend_from_slice(&rest[..c.unsigned_abs() as usize]);
                s = &rest[c.unsigned_abs() as usize..];
            } else {
                out.extend(std::iter::repeat_n(rest[0], c as usize + 1));
                s = &rest[1..];
            }
        }
        out
    }

    #[test]
    fn rle_codes_runs_and_literals() {
        for src in [vec![], vec![5], vec![1, 1], vec![9; 300], (0..=255).collect::<Vec<u8>>(), [vec![1, 2], vec![3; 3], vec![4, 4, 5]].concat()] {
            assert_eq!(unrle(&rle(&src)), src);
        }
        assert_eq!(rle(&[7; 130]), vec![127, 7, 0xFE, 7, 7]);
        assert_eq!(rle(&[1, 2, 2, 2]), vec![0xFF, 1, 2, 2]);
    }

    /// Every compression stores the same pixels as the uncompressed file, in chunks of the right
    /// height, and the compressed files are smaller.
    #[test]
    fn compressed_exr_holds_the_same_pixels() {
        let (px, w, h) = picture();
        for alpha in [false, true] {
            let plain = exr(&px, w, h, alpha, ExrCompression::None).unwrap();
            let lines: Vec<&[u8]> = chunks(&plain, h as usize).into_iter().map(|(_, d)| d).collect();
            for c in [ExrCompression::Rle, ExrCompression::Zips, ExrCompression::Zip] {
                let f = exr(&px, w, h, alpha, c).unwrap();
                let cmp = find(&f, b"compression\0compression\0") + 24;
                assert_eq!(f[cmp + 4], c.code());
                assert!(f.len() < plain.len() * 3 / 4, "{c:?}: {} vs {}", f.len(), plain.len());
                let n = (h as usize).div_ceil(c.lines());
                for (k, (y, data)) in chunks(&f, n).into_iter().enumerate() {
                    assert_eq!(y as usize, k * c.lines());
                    let want: Vec<u8> = lines[k * c.lines()..((k + 1) * c.lines()).min(h as usize)].concat();
                    let got = if data.len() == want.len() {
                        data.to_vec()
                    } else if c == ExrCompression::Rle {
                        unpredict(&unrle(data))
                    } else {
                        {
                            use std::io::Read as _;
                            let mut decoded = Vec::new();
                            flate2::read::ZlibDecoder::new(data).read_to_end(&mut decoded).unwrap();
                            unpredict(&decoded)
                        }
                    };
                    assert_eq!(got, want, "{c:?} chunk {k}");
                }
            }
        }
    }

    /// ffmpeg (external test oracle) decodes our OpenEXR files to the values we meant: linear
    /// light, premultiplied alpha, rows top to bottom; in every compression.
    #[test]
    fn exr_decodes_in_ffmpeg() {
        let Some(ffmpeg) = filmcraft_testkit::ffmpeg_or_skip("OpenEXR stills") else { return };
        let (px, w, h) = picture();
        let dir = filmcraft_testkit::fixtures::workspace_root().join("target").join("export-tests").join(format!("exr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lin = filmcraft_color::srgb_u8_to_linear_table();
        for c in ExrCompression::ALL {
            for alpha in [false, true] {
                let path = dir.join(format!("f{alpha}-{c:?}.exr"));
                std::fs::write(&path, exr(&px, w, h, alpha, c).unwrap()).unwrap();
                let out = std::process::Command::new(&ffmpeg)
                    .args(["-v", "error", "-i"])
                    .arg(&path)
                    .args(["-f", "rawvideo", "-pix_fmt", "gbrapf32le", "-"])
                    .output()
                    .unwrap();
                assert!(out.status.success(), "{c:?}: {}", String::from_utf8_lossy(&out.stderr));
                let n = (w * h) as usize;
                assert_eq!(out.stdout.len(), n * 4 * 4, "{c:?}");
                let plane = |p: usize, i: usize| f32::from_le_bytes(out.stdout[(p * n + i) * 4..][..4].try_into().unwrap());
                for (i, &[r, g, b, a]) in px.as_chunks::<4>().0.iter().enumerate() {
                    let k = if alpha { f32::from(a) / 255.0 } else { 1.0 };
                    // gbrap planes: G, B, R, A
                    let want = [lin[g as usize] * k, lin[b as usize] * k, lin[r as usize] * k, k];
                    for (p, want) in want.into_iter().enumerate() {
                        let got = plane(p, i);
                        assert!((got - want).abs() <= 1e-3 + want * 1e-3, "{c:?}, alpha {alpha}, pixel {i}, plane {p}: {got} vs {want}");
                    }
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hostile_sizes_are_errors() {
        assert!(dpx(&[0; 7], 2, 1).is_err());
        assert!(dpx(&[], 0, 0).is_err());
        assert!(tga(&[0; 4], 1, 2, false).is_err());
        assert!(tga(&[0; 4], u32::MAX, u32::MAX, false).is_err());
        assert!(rgb(&[0; 3], 1, 1).is_err());
        assert!(dpx(&[0; 4], u32::MAX, u32::MAX).is_err());
        for c in ExrCompression::ALL {
            assert!(exr(&[0; 4], u32::MAX, u32::MAX, true, c).is_err());
            assert!(exr(&[0; 7], 2, 1, false, c).is_err());
            assert!(exr(&[], 0, 1, false, c).is_err());
        }
    }

    #[test]
    fn tga_and_dpx_errors_keep_their_export_messages() {
        assert!(matches!(tga(&[0; 7], 2, 1, true), Err(ExportError::Encode(m)) if m == "frame size"));
        let wide = vec![0; 65536 * 4];
        assert!(matches!(tga(&wide, 65536, 1, false), Err(ExportError::Unsupported(m)) if m.contains("65535×65535 (got 65536×1)")));
        assert!(matches!(dpx(&[0; 7], 2, 1), Err(ExportError::Encode(m)) if m == "frame size"));
    }
}
