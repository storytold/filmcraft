//! DPX (`.dpx`) stills: craft-dpx (SMPTE ST 268) reads the header and the code values; FilmCraft
//! turns them into RGBA8 for the still pipeline.
//!
//! The first image element is decoded: luma, RGB, RGBA, ABGR, or Y'CbCr (CbYCrY 4:2:2, CbYACrYA
//! 4:2:2:4, CbYCr 4:4:4, CbYCrA 4:4:4:4), 8, 10, 12 or 16 bits per component, big-endian (`SDPX`) or
//! little-endian (`XPDS`) files, every packing craft-dpx reads. Orientations 0–3 (either scan
//! direction, either line order) are honoured. RGB and luma code values map linearly onto 8 bits
//! (full range); a logarithmic (printing density) transfer is shown as stored, without a film-log
//! conversion. Y'CbCr is converted with the matrix the colorimetric field names (BT.709, or BT.601
//! for the 601 and composite video codes; by picture size when unspecified), in video range unless
//! the reference low / high data codes span the whole code range; 4:2:2 chroma is shared by the two
//! pixels of a pair.
//!
//! Not decoded (an "unsupported" error): run-length encoded elements, signed, 1-bit and
//! floating-point components, transposed orientations. Cineon files are not routed here.

use craft_dpx::{Error, Layout, Limits};
use filmcraft_color::{Matrix, Range, normalize_c, normalize_y, ycbcr_to_rgb};

use crate::{MediaError, Result, still::Raster};

fn err(name: &str, what: &str) -> MediaError {
    MediaError::Decode(format!("{name}: DPX: {what}"))
}

fn unsupported(name: &str, what: String) -> MediaError {
    MediaError::Unsupported(format!("{name}: DPX {what}"))
}

fn map_error(name: &str, e: Error) -> MediaError {
    match e {
        Error::Unsupported(what) => unsupported(name, what),
        Error::NotDpx => err(name, "not a DPX file"),
        Error::Malformed(what) => err(name, &what),
        Error::LimitExceeded(_) => err(name, "picture too large"),
        Error::Encode(what) => err(name, &what),
    }
}

/// Whether `bytes` starts with the DPX magic number in either byte order.
pub fn sniff(bytes: &[u8]) -> bool {
    craft_dpx::sniff(bytes) == Some(craft_dpx::Kind::Dpx)
}

/// What the stored components of a pixel are.
#[derive(Clone, Copy)]
enum Components {
    /// `n` components per pixel; the indices of R, G, B and A among them (alpha `None`: opaque).
    Rgb { n: usize, map: [Option<usize>; 4] },
    /// `n` components per pixel: Cb (4:2:2: Cb on even pixels, Cr on odd ones), Y', then Cr (4:4:4
    /// only), then alpha when `alpha`.
    YCbCr { n: usize, sub: bool, alpha: bool, matrix: Matrix, range: Range },
}

impl Components {
    fn has_alpha(self) -> bool {
        match self {
            Components::Rgb { map, .. } => map[3].is_some(),
            Components::YCbCr { alpha, .. } => alpha,
        }
    }

    /// RGBA8 of pixel `x` from the component values of its scan line (`bits` per value).
    fn pixel(self, line: &[u16], x: usize, bits: u32) -> [u8; 4] {
        let max = (1u32 << bits) - 1;
        let to8 = |v: u32| ((v.min(max) * 255 + max / 2) / max) as u8;
        let get = |i: usize| line.get(i).map(|&v| u32::from(v));
        match self {
            Components::Rgb { n, map } => map.map(|slot| match slot {
                Some(i) => get(x * n + i).map_or(0, to8),
                None => 255,
            }),
            Components::YCbCr { n, sub, alpha, matrix, range } => {
                let base = x * n;
                let (cb, cr) = if sub {
                    let pair = (x & !1) * n;
                    (get(pair), get(pair + n))
                } else {
                    (get(base), get(base + 2))
                };
                // a missing Cr (odd width, 4:2:2) is neutral
                let mid = 1u32 << (bits - 1);
                let y = normalize_y(get(base + 1).unwrap_or(0), bits, range);
                let rgb = ycbcr_to_rgb(y, normalize_c(cb.unwrap_or(mid), bits, range), normalize_c(cr.unwrap_or(mid), bits, range), matrix);
                let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                let a = if alpha { get(base + n - 1).map_or(0, to8) } else { 255 };
                [c(rgb[0]), c(rgb[1]), c(rgb[2]), a]
            }
        }
    }
}

/// Decode the first image element of a DPX file to straight-alpha RGBA8, top row first.
pub fn decode(name: &str, bytes: &[u8]) -> Result<Raster> {
    if !sniff(bytes) {
        return Err(err(name, "not a DPX file"));
    }
    let frame = craft_dpx::decode(bytes, &Limits::default()).map_err(|e| map_error(name, e))?;
    let info = &frame.info;
    let (w, h) = (info.width as usize, info.height as usize);
    let (flip_x, flip_y) = info.flips().ok_or_else(|| unsupported(name, format!("orientation {}", info.orientation)))?;
    let max = u32::from(info.max_code());
    // video range unless the reference codes span every code value
    let range = if info.reference_low_code == 0 && info.reference_high_code == max { Range::Full } else { Range::Limited };
    let matrix = match info.colorimetric {
        Some(5 | 6) => Matrix::Bt709,
        Some(7..=10) => Matrix::Bt601,
        _ => filmcraft_frame::default_matrix(info.width, info.height),
    };
    let ycbcr = |n, sub, alpha| Components::YCbCr { n, sub, alpha, matrix, range };
    let components = match info.layout {
        Layout::Luma => Components::Rgb { n: 1, map: [Some(0), Some(0), Some(0), None] },
        Layout::Rgb => Components::Rgb { n: 3, map: [Some(0), Some(1), Some(2), None] },
        // craft-dpx returns ABGR (descriptor 52) reordered to RGBA
        Layout::Rgba => Components::Rgb { n: 4, map: [Some(0), Some(1), Some(2), Some(3)] },
        Layout::CbYCrY422 => ycbcr(2, true, false),
        Layout::CbYACrYA4224 => ycbcr(3, true, true),
        Layout::CbYCr444 => ycbcr(3, false, false),
        Layout::CbYCrA4444 => ycbcr(4, false, true),
    };
    let per_line = w * info.layout.components();
    let bits = u32::from(info.bits);
    let pixels = w.checked_mul(h).ok_or_else(|| err(name, "picture too large"))?;
    let mut rgba = vec![0u8; pixels * 4];
    for (y, line) in frame.codes.chunks_exact(per_line.max(1)).take(h).enumerate() {
        let dy = if flip_y { h - 1 - y } else { y };
        for x in 0..w {
            let dx = if flip_x { w - 1 - x } else { x };
            let o = (dy * w + dx) * 4;
            if let Some(px) = rgba.get_mut(o..o + 4) {
                px.copy_from_slice(&components.pixel(line, x, bits));
            }
        }
    }
    let has_alpha = components.has_alpha() && rgba.as_chunks::<4>().0.iter().any(|p| p[3] != 255);
    Ok(Raster { width: info.width, height: info.height, rgba, has_alpha })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// First image element of the image information header.
    const ELEMENT: usize = 780;
    /// "Undefined" value of numeric header fields.
    const UNDEFINED: u32 = u32::MAX;

    /// A minimal DPX file (header + `data`) in the given byte order.
    /// Image element fields of a test DPX header.
    struct Element {
        descriptor: u8,
        bits: u8,
        packing: u16,
    }

    fn file(big: bool, w: u32, h: u32, element: Element, orientation: u16, data: &[u8]) -> Vec<u8> {
        let Element { descriptor, bits, packing } = element;
        let mut f = vec![0u8; 2048];
        let p32 = |f: &mut Vec<u8>, at: usize, v: u32| f[at..at + 4].copy_from_slice(&if big { v.to_be_bytes() } else { v.to_le_bytes() });
        let p16 = |f: &mut Vec<u8>, at: usize, v: u16| f[at..at + 2].copy_from_slice(&if big { v.to_be_bytes() } else { v.to_le_bytes() });
        f[0..4].copy_from_slice(if big { b"SDPX" } else { b"XPDS" });
        p32(&mut f, 4, 2048);
        p16(&mut f, 768, orientation);
        p16(&mut f, 770, 1);
        p32(&mut f, 772, w);
        p32(&mut f, 776, h);
        f[ELEMENT + 20] = descriptor;
        f[ELEMENT + 23] = bits;
        p16(&mut f, ELEMENT + 24, packing);
        p32(&mut f, ELEMENT + 28, UNDEFINED);
        p32(&mut f, ELEMENT + 32, UNDEFINED);
        f.extend_from_slice(data);
        f
    }

    #[test]
    fn ten_bit_method_a_and_b() {
        let word = |r: u32, g: u32, b: u32, pad: u32| ((r << (20 + pad)) | (g << (10 + pad)) | (b << pad)).to_be_bytes();
        let mut data = Vec::new();
        data.extend_from_slice(&word(1023, 0, 512, 2));
        data.extend_from_slice(&word(4, 1019, 0, 2));
        let r = decode("a.dpx", &file(true, 2, 1, Element { descriptor: 50, bits: 10, packing: 1 }, 0, &data)).unwrap();
        assert_eq!(r.rgba, [255, 0, 128, 255, 1, 254, 0, 255]);
        assert!(!r.has_alpha);
        let mut data = Vec::new();
        data.extend_from_slice(&word(1023, 0, 512, 0));
        data.extend_from_slice(&word(4, 1019, 0, 0));
        let r = decode("a.dpx", &file(true, 2, 1, Element { descriptor: 50, bits: 10, packing: 2 }, 0, &data)).unwrap();
        assert_eq!(r.rgba, [255, 0, 128, 255, 1, 254, 0, 255]);
    }

    #[test]
    fn rgba_components_roll_across_ten_bit_words() {
        // 2 RGBA pixels = 8 components = 3 words; the last word is half empty
        let comps = [1023u32, 0, 0, 1023, 0, 1023, 0, 0];
        let mut data = Vec::new();
        for c in comps.chunks(3) {
            let get = |i: usize| c.get(i).copied().unwrap_or(0);
            data.extend_from_slice(&((get(0) << 22) | (get(1) << 12) | (get(2) << 2)).to_le_bytes());
        }
        let r = decode("a.dpx", &file(false, 2, 1, Element { descriptor: 51, bits: 10, packing: 1 }, 0, &data)).unwrap();
        assert_eq!(r.rgba, [255, 0, 0, 255, 0, 255, 0, 0]);
        assert!(r.has_alpha);
    }

    #[test]
    fn eight_bit_lines_pad_to_words_and_orientation_flips() {
        // 1 × 2 RGB: each 3-byte line padded to 4; bottom-to-top, right-to-left orientation 3
        let r = decode("a.dpx", &file(false, 1, 2, Element { descriptor: 50, bits: 8, packing: 0 }, 3, &[1, 2, 3, 0, 4, 5, 6, 0])).unwrap();
        assert_eq!(r.rgba, [4, 5, 6, 255, 1, 2, 3, 255]);
        // 2 × 1 luma, 16-bit big-endian; ABGR, 12-bit method A
        let r = decode("a.dpx", &file(true, 2, 1, Element { descriptor: 6, bits: 16, packing: 0 }, 0, &[0xFF, 0xFF, 0x80, 0x00])).unwrap();
        assert_eq!(r.rgba, [255, 255, 255, 255, 128, 128, 128, 255]);
        let r =
            decode("a.dpx", &file(true, 1, 1, Element { descriptor: 52, bits: 12, packing: 1 }, 0, &[0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xF0])).unwrap();
        assert_eq!(r.rgba, [255, 0, 0, 128]);
    }

    /// Packing 0: components back to back, low bits first, each line on a 32-bit boundary.
    fn packed(lines: &[&[u32]], bits: u32, big: bool) -> Vec<u8> {
        let mut out = Vec::new();
        for line in lines {
            let (mut acc, mut n) = (0u64, 0u32);
            for &v in *line {
                acc |= u64::from(v) << n;
                n += bits;
                while n >= 32 {
                    let w = acc as u32;
                    out.extend_from_slice(&if big { w.to_be_bytes() } else { w.to_le_bytes() });
                    acc >>= 32;
                    n -= 32;
                }
            }
            if n > 0 {
                let w = acc as u32;
                out.extend_from_slice(&if big { w.to_be_bytes() } else { w.to_le_bytes() });
            }
        }
        out
    }

    #[test]
    fn packed_ten_and_twelve_bit_components_cross_words() {
        // 2 × 2 RGB 10-bit: 6 components (60 bits) per line, the fourth straddles two words
        let lines: [&[u32]; 2] = [&[1023, 0, 512, 4, 1019, 1023], &[0, 1023, 0, 1023, 1023, 1023]];
        for big in [true, false] {
            let r = decode("a.dpx", &file(big, 2, 2, Element { descriptor: 50, bits: 10, packing: 0 }, 0, &packed(&lines, 10, big))).unwrap();
            assert_eq!(r.rgba, [255, 0, 128, 255, 1, 254, 255, 255, 0, 255, 0, 255, 255, 255, 255, 255]);
        }
        // 3 × 1 RGB 12-bit: 9 components, 108 bits in 4 words
        let line: [&[u32]; 1] = [&[4095, 0, 2048, 16, 4079, 0, 0, 0, 4095]];
        let r = decode("a.dpx", &file(true, 3, 1, Element { descriptor: 50, bits: 12, packing: 0 }, 0, &packed(&line, 12, true))).unwrap();
        assert_eq!(r.rgba, [255, 0, 128, 255, 1, 254, 0, 255, 0, 0, 255, 255]);
    }

    #[test]
    fn ycbcr_descriptors_convert_to_rgb() {
        // 8-bit CbYCrY, video range, BT.709 (colorimetric 6): black, white, then a red pixel pair
        // (the 709 code triple of red is rounded, hence green 1)
        let mut f = file(true, 4, 1, Element { descriptor: 100, bits: 8, packing: 0 }, 0, &[128, 16, 128, 235, 102, 63, 240, 63]);
        f[ELEMENT + 22] = 6;
        let r = decode("a.dpx", &f).unwrap();
        assert_eq!(r.rgba, [0, 0, 0, 255, 255, 255, 255, 255, 255, 1, 0, 255, 255, 1, 0, 255]);
        assert!(!r.has_alpha);
        // red in BT.601 (colorimetric 7): the 601 code triple, rounded
        let mut f = file(true, 2, 1, Element { descriptor: 100, bits: 8, packing: 0 }, 0, &[90, 81, 240, 81]);
        f[ELEMENT + 22] = 7;
        assert_eq!(decode("a.dpx", &f).unwrap().rgba, [254, 0, 0, 255, 254, 0, 0, 255]);
        // 10-bit CbYCr filled (method A), full range from the reference codes: mid grey
        let word = |a: u32, b: u32, c: u32| ((a << 22) | (b << 12) | (c << 2)).to_be_bytes();
        let mut f = file(true, 1, 1, Element { descriptor: 102, bits: 10, packing: 1 }, 0, &word(512, 512, 512));
        f[ELEMENT + 4..ELEMENT + 8].copy_from_slice(&0u32.to_be_bytes());
        f[ELEMENT + 12..ELEMENT + 16].copy_from_slice(&1023u32.to_be_bytes());
        assert_eq!(decode("a.dpx", &f).unwrap().rgba, [128, 128, 128, 255]);
        // 16-bit CbYCrA: white at half opacity
        let v = |x: u16| x.to_be_bytes();
        let data: Vec<u8> = [v(32768), v(60160), v(32768), v(32768)].concat();
        let r = decode("a.dpx", &file(true, 1, 1, Element { descriptor: 103, bits: 16, packing: 0 }, 0, &data)).unwrap();
        assert_eq!(r.rgba, [255, 255, 255, 128]);
        assert!(r.has_alpha);
        // 8-bit CbYACrYA, 3 pixels: the last pixel has no Cr and is neutral
        let r = decode("a.dpx", &file(true, 3, 1, Element { descriptor: 101, bits: 8, packing: 0 }, 0, &[128, 235, 255, 128, 16, 0, 128, 235, 255, 0, 0, 0]))
            .unwrap();
        assert_eq!(r.rgba, [255, 255, 255, 255, 0, 0, 0, 0, 255, 255, 255, 255]);
        assert!(r.has_alpha);
    }

    #[test]
    fn hostile_headers_fail_cleanly() {
        // huge dimensions with no data, truncated data, unsupported layouts
        assert!(decode("a.dpx", &file(true, u32::MAX, u32::MAX, Element { descriptor: 50, bits: 10, packing: 1 }, 0, &[])).is_err());
        assert!(decode("a.dpx", &file(true, 4096, 4096, Element { descriptor: 50, bits: 10, packing: 1 }, 0, &[0; 64])).is_err());
        assert!(decode("a.dpx", &file(true, 2, 2, Element { descriptor: 50, bits: 10, packing: 1 }, 0, &[0; 12])).is_err());
        assert!(decode("a.dpx", &file(true, 1, 1, Element { descriptor: 150, bits: 8, packing: 0 }, 0, &[0; 4])).is_err());
        assert!(decode("a.dpx", &file(true, 1, 1, Element { descriptor: 50, bits: 10, packing: 3 }, 0, &[0; 4])).is_err());
        assert!(decode("a.dpx", &file(true, 1, 1, Element { descriptor: 50, bits: 16, packing: 0 }, 0, &[0; 4])).is_err());
        assert!(decode("a.dpx", &file(true, 3, 3, Element { descriptor: 100, bits: 12, packing: 0 }, 0, &[0; 20])).is_err());
        assert!(decode("a.dpx", &file(true, 1, 1, Element { descriptor: 50, bits: 32, packing: 0 }, 0, &[0; 16])).is_err());
        assert!(decode("a.dpx", &file(true, 1, 1, Element { descriptor: 50, bits: 8, packing: 0 }, 5, &[0; 4])).is_err());
        assert!(decode("a.dpx", &file(true, 0, 1, Element { descriptor: 50, bits: 8, packing: 0 }, 0, &[0; 4])).is_err());
        assert!(decode("a.dpx", b"SDPX").is_err());
        let mut f = file(true, 1, 1, Element { descriptor: 50, bits: 8, packing: 0 }, 0, &[0; 4]);
        f[4..8].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(decode("a.dpx", &f).is_err());
        let mut f = file(true, 1, 1, Element { descriptor: 50, bits: 8, packing: 0 }, 0, &[0; 4]);
        f[ELEMENT + 32..ELEMENT + 36].copy_from_slice(&(u32::MAX - 1).to_be_bytes());
        assert!(decode("a.dpx", &file(true, 1, 2, Element { descriptor: 50, bits: 8, packing: 0 }, 0, &[0; 4])).is_err());
        assert!(decode("a.dpx", &f).is_ok());
    }
}
