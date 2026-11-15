//! DPX (`.dpx`) stills, decoded clean-room from SMPTE ST 268 (Digital Picture Exchange).
//!
//! The first image element is decoded: luma, RGB, RGBA, ABGR, or Y'CbCr (CbYCrY 4:2:2, CbYACrYA
//! 4:2:2:4, CbYCr 4:4:4, CbYCrA 4:4:4:4), 8, 10, 12 or 16 bits per component, big-endian (`SDPX`) or
//! little-endian (`XPDS`) files. 10-bit components are filled into 32-bit words (packing 1, method
//! A: two padding bits at the bottom; packing 2, method B: at the top), 12-bit components fill
//! 16-bit words the same two ways; packed 10/12-bit components (packing 0) follow each other with no
//! padding, the first in the low bits of a 32-bit word, a component crossing into the next word
//! continuing in its low bits. Scan lines start on a 32-bit boundary and may carry end-of-line
//! padding. Orientations 0–3 (either scan direction, either line order) are honoured. RGB and luma
//! code values map linearly onto 8 bits (full range); a logarithmic (printing density) transfer is
//! shown as stored, without a film-log conversion. Y'CbCr is converted with the matrix the
//! colorimetric field names (BT.709, or BT.601 for the 601 and composite video codes; by picture
//! size when unspecified), in video range unless the reference low / high data codes span the
//! whole code range; 4:2:2 chroma is shared by the two pixels of a pair.
//!
//! Not decoded (an "unsupported" error): run-length encoded elements, 1-bit and floating-point
//! components, transposed orientations.

use filmcraft_color::{Matrix, Range, normalize_c, normalize_y, ycbcr_to_rgb};

use crate::{MediaError, Result, still::Raster};

/// Generic file + image information header: everything we read lies below this.
const HEADER: usize = 1408;
/// First image element of the image information header (72 bytes each).
const ELEMENT: usize = 780;
/// Largest picture we decode (16384 × 16384): bounds the allocation a header can ask for.
const MAX_PIXELS: usize = 1 << 28;
/// "Undefined" value of numeric header fields.
const UNDEFINED: u32 = u32::MAX;

fn err(name: &str, what: &str) -> MediaError {
    MediaError::Decode(format!("{name}: DPX: {what}"))
}

fn unsupported(name: &str, what: String) -> MediaError {
    MediaError::Unsupported(format!("{name}: DPX {what}"))
}

/// Whether `bytes` starts with the DPX magic number in either byte order.
pub fn sniff(bytes: &[u8]) -> bool {
    bytes.starts_with(b"SDPX") || bytes.starts_with(b"XPDS")
}

/// Header and sample reads in the file's byte order.
#[derive(Clone, Copy)]
struct Order {
    big: bool,
}

impl Order {
    fn u16(self, b: &[u8], at: usize) -> u16 {
        let v = [b.get(at).copied().unwrap_or(0), b.get(at + 1).copied().unwrap_or(0)];
        if self.big { u16::from_be_bytes(v) } else { u16::from_le_bytes(v) }
    }

    fn u32(self, b: &[u8], at: usize) -> u32 {
        let v = core::array::from_fn(|i| b.get(at + i).copied().unwrap_or(0));
        if self.big { u32::from_be_bytes(v) } else { u32::from_le_bytes(v) }
    }
}

/// How components are stored in a scan line.
#[derive(Clone, Copy)]
enum Packing {
    /// One byte per component.
    Bytes,
    /// One 16-bit word per component; the value is `word >> shift` (12-bit method A: 4).
    Words { shift: u32 },
    /// Three 10-bit components per 32-bit word, the first in the high bits, above `pad` bits.
    Ten { pad: u32 },
    /// `bits`-bit components back to back in 32-bit words, low bits first (packing 0).
    Packed { bits: u32 },
}

impl Packing {
    /// Bytes of a scan line holding `comps` components, before end-of-line padding (lines start on
    /// a 32-bit boundary).
    fn line_bytes(self, comps: usize) -> Option<usize> {
        let raw = match self {
            Packing::Bytes => comps,
            Packing::Words { .. } => comps.checked_mul(2)?,
            Packing::Ten { .. } => comps.div_ceil(3).checked_mul(4)?,
            Packing::Packed { bits } => comps.checked_mul(bits as usize)?.div_ceil(32).checked_mul(4)?,
        };
        raw.checked_next_multiple_of(4)
    }

    /// The `comps` component values of one scan line.
    fn unpack(self, line: &[u8], comps: usize, order: Order, out: &mut Vec<u16>) {
        out.clear();
        match self {
            Packing::Bytes => out.extend(line.iter().take(comps).map(|&b| u16::from(b))),
            Packing::Words { shift } => out.extend((0..comps).map(|i| order.u16(line, i * 2) >> shift)),
            Packing::Ten { pad } => {
                for w in 0..comps.div_ceil(3) {
                    let word = order.u32(line, w * 4);
                    for s in [20 + pad, 10 + pad, pad] {
                        if out.len() < comps {
                            out.push(((word >> s) & 0x3FF) as u16);
                        }
                    }
                }
            }
            Packing::Packed { bits } => {
                let mask = (1u64 << bits) - 1;
                for i in 0..comps {
                    let bit = i * bits as usize;
                    let at = bit / 32 * 4;
                    let pair = u64::from(order.u32(line, at)) | (u64::from(order.u32(line, at + 4)) << 32);
                    out.push(((pair >> (bit % 32)) & mask) as u16);
                }
            }
        }
    }
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
    fn per_pixel(self) -> usize {
        match self {
            Components::Rgb { n, .. } | Components::YCbCr { n, .. } => n,
        }
    }

    fn has_alpha(self) -> bool {
        match self {
            Components::Rgb { map, .. } => map[3].is_some(),
            Components::YCbCr { alpha, .. } => alpha,
        }
    }

    /// RGBA8 of pixel `x` from the unpacked component values of its scan line (`bits` per value).
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
    let order = match bytes.get(..4) {
        Some(b"SDPX") => Order { big: true },
        Some(b"XPDS") => Order { big: false },
        _ => return Err(err(name, "not a DPX file")),
    };
    if bytes.len() < HEADER {
        return Err(err(name, "truncated header"));
    }
    let orientation = order.u16(bytes, 768);
    let (w, h) = (order.u32(bytes, 772) as usize, order.u32(bytes, 776) as usize);
    let el = ELEMENT;
    let low_code = order.u32(bytes, el + 4);
    let high_code = order.u32(bytes, el + 12);
    let descriptor = bytes.get(el + 20).copied().unwrap_or(0);
    let colorimetric = bytes.get(el + 22).copied().unwrap_or(0);
    let bits = bytes.get(el + 23).copied().unwrap_or(0);
    let packing = order.u16(bytes, el + 24);
    let encoding = order.u16(bytes, el + 26);
    let element_offset = order.u32(bytes, el + 28);
    let eol_padding = order.u32(bytes, el + 32);
    if w == 0 || h == 0 {
        return Err(err(name, "empty picture"));
    }
    let pixels = w.checked_mul(h).filter(|&n| n <= MAX_PIXELS).ok_or_else(|| err(name, "picture too large"))?;
    if encoding != 0 {
        return Err(unsupported(name, "run-length encoding".into()));
    }
    let layout = match (bits, packing) {
        (8, _) => Packing::Bytes,
        (16, _) => Packing::Words { shift: 0 },
        (10, 1) => Packing::Ten { pad: 2 },
        (10, 2) => Packing::Ten { pad: 0 },
        (12, 1) => Packing::Words { shift: 4 },
        (12, 2) => Packing::Words { shift: 0 },
        (10 | 12, 0) => Packing::Packed { bits: u32::from(bits) },
        (b, p) => return Err(unsupported(name, format!("{b}-bit components with packing {p}"))),
    };
    let (flip_x, flip_y) = match orientation {
        0 => (false, false),
        1 => (true, false),
        2 => (false, true),
        3 => (true, true),
        o => return Err(unsupported(name, format!("orientation {o}"))),
    };
    let max = (1u32 << bits) - 1;
    // video range unless the reference codes span every code value
    let range = if low_code == 0 && high_code == max { Range::Full } else { Range::Limited };
    let matrix = match colorimetric {
        5 | 6 => Matrix::Bt709,
        7..=10 => Matrix::Bt601,
        _ => filmcraft_frame::default_matrix(w as u32, h as u32),
    };
    let ycbcr = |n, sub, alpha| Components::YCbCr { n, sub, alpha, matrix, range };
    let components = match descriptor {
        6 => Components::Rgb { n: 1, map: [Some(0), Some(0), Some(0), None] },
        50 => Components::Rgb { n: 3, map: [Some(0), Some(1), Some(2), None] },
        51 => Components::Rgb { n: 4, map: [Some(0), Some(1), Some(2), Some(3)] },
        52 => Components::Rgb { n: 4, map: [Some(3), Some(2), Some(1), Some(0)] },
        100 => ycbcr(2, true, false),
        101 => ycbcr(3, true, true),
        102 => ycbcr(3, false, false),
        103 => ycbcr(4, false, true),
        d => return Err(unsupported(name, format!("descriptor {d}"))),
    };
    let n = components.per_pixel();
    let comps = w.checked_mul(n).ok_or_else(|| err(name, "picture too large"))?;
    let pad = if eol_padding == UNDEFINED { 0 } else { eol_padding as usize };
    let stride = layout.line_bytes(comps).and_then(|l| l.checked_add(pad)).ok_or_else(|| err(name, "picture too large"))?;
    let offset = match element_offset {
        0 | UNDEFINED => order.u32(bytes, 4) as usize,
        o => o as usize,
    };
    // the last line needs no end-of-line padding
    let size = stride.checked_mul(h - 1).and_then(|s| s.checked_add(stride - pad)).ok_or_else(|| err(name, "picture too large"))?;
    let data = offset.checked_add(size).and_then(|end| bytes.get(offset..end)).ok_or_else(|| err(name, "truncated image data"))?;
    let bits = u32::from(bits);
    let mut rgba = vec![0u8; pixels * 4];
    let mut line = Vec::with_capacity(comps);
    for y in 0..h {
        let src = data.get(y * stride..).unwrap_or_default();
        layout.unpack(src, comps, order, &mut line);
        let dy = if flip_y { h - 1 - y } else { y };
        for x in 0..w {
            let dx = if flip_x { w - 1 - x } else { x };
            let o = (dy * w + dx) * 4;
            if let Some(px) = rgba.get_mut(o..o + 4) {
                px.copy_from_slice(&components.pixel(&line, x, bits));
            }
        }
    }
    let has_alpha = components.has_alpha() && rgba.as_chunks::<4>().0.iter().any(|p| p[3] != 255);
    Ok(Raster { width: w as u32, height: h as u32, rgba, has_alpha })
}

#[cfg(test)]
mod tests {
    use super::*;

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
