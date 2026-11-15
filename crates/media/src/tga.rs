//! Targa (`.tga`) stills, decoded clean-room from the Truevision TGA File Format Specification 2.0.
//!
//! Image types 1/2/3 (colour-mapped, true-colour, grayscale) and their run-length encoded forms
//! 9/10/11; 15/16-bit (5-5-5, the top bit as alpha when the descriptor says one attribute bit),
//! 24-bit BGR and 32-bit BGRA true-colour, 8-bit grayscale and 16-bit grayscale + alpha, colour maps
//! of 15/16/24/32-bit entries indexed by 8 or 16 bits; any origin corner.

use crate::{MediaError, Result, still::Raster};

/// Length of the fixed header.
const HEADER: usize = 18;
/// TGA 2.0 files end with this signature (after two 32-bit area offsets).
const FOOTER: &[u8] = b"TRUEVISION-XFILE.\0";
/// Largest picture we decode (16384 × 16384): bounds the allocation a header can ask for.
const MAX_PIXELS: usize = 1 << 28;

fn err(name: &str, what: &str) -> MediaError {
    MediaError::Decode(format!("{name}: Targa: {what}"))
}

/// Whether `bytes` looks like a Targa file: a TGA 2.0 footer, or (for the original format, which
/// has no signature) a `.tga` name and a plausible header.
pub fn sniff(name: &str, bytes: &[u8]) -> bool {
    let header_ok = || {
        let Some(h) = bytes.first_chunk::<HEADER>() else { return false };
        let (map_type, kind, depth) = (h[1], h[2], h[16]);
        let (w, ht) = (u16::from_le_bytes([h[12], h[13]]), u16::from_le_bytes([h[14], h[15]]));
        map_type <= 1 && matches!(kind, 1 | 2 | 3 | 9 | 10 | 11) && w > 0 && ht > 0 && matches!(depth, 8 | 15 | 16 | 24 | 32)
    };
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    let named = matches!(ext.as_deref(), Some("tga" | "targa" | "icb" | "vda" | "vst"));
    header_ok() && (named || bytes.ends_with(FOOTER))
}

/// Pixel layout of the stored values (after the colour-map lookup, for mapped images).
#[derive(Clone, Copy)]
enum Layout {
    /// 15/16-bit `ARRRRRGG GGGBBBBB`, little-endian; `alpha`: the top bit is an alpha bit.
    Rgb555 {
        alpha: bool,
    },
    Bgr24,
    Bgra32,
    Gray8,
    GrayAlpha16,
}

impl Layout {
    fn true_colour(depth: u8, attr_bits: u8) -> Option<Self> {
        Some(match depth {
            15 => Layout::Rgb555 { alpha: false },
            16 => Layout::Rgb555 { alpha: attr_bits == 1 },
            24 => Layout::Bgr24,
            32 => Layout::Bgra32,
            _ => return None,
        })
    }

    fn bytes(self) -> usize {
        match self {
            Layout::Rgb555 { .. } | Layout::GrayAlpha16 => 2,
            Layout::Bgr24 => 3,
            Layout::Bgra32 => 4,
            Layout::Gray8 => 1,
        }
    }

    /// RGBA of one stored value (`p` holds exactly [`bytes`](Self::bytes) bytes).
    fn rgba(self, p: &[u8]) -> [u8; 4] {
        let b = |i: usize| p.get(i).copied().unwrap_or(0);
        match self {
            Layout::Rgb555 { alpha } => {
                let v = u16::from_le_bytes([b(0), b(1)]);
                let five = |s: u16| {
                    let c = ((v >> s) & 31) as u8;
                    (c << 3) | (c >> 2)
                };
                let a = if !alpha || v & 0x8000 != 0 { 255 } else { 0 };
                [five(10), five(5), five(0), a]
            }
            Layout::Bgr24 => [b(2), b(1), b(0), 255],
            Layout::Bgra32 => [b(2), b(1), b(0), b(3)],
            Layout::Gray8 => [b(0), b(0), b(0), 255],
            Layout::GrayAlpha16 => [b(0), b(0), b(0), b(1)],
        }
    }
}

/// Decode a Targa file to straight-alpha RGBA8, top row first.
pub fn decode(name: &str, bytes: &[u8]) -> Result<Raster> {
    let h = bytes.first_chunk::<HEADER>().ok_or_else(|| err(name, "truncated header"))?;
    let (id_len, map_type, kind) = (usize::from(h[0]), h[1], h[2]);
    let map_first = usize::from(u16::from_le_bytes([h[3], h[4]]));
    let map_len = usize::from(u16::from_le_bytes([h[5], h[6]]));
    let map_depth = h[7];
    let (w, ht) = (usize::from(u16::from_le_bytes([h[12], h[13]])), usize::from(u16::from_le_bytes([h[14], h[15]])));
    let (depth, descriptor) = (h[16], h[17]);
    let attr_bits = descriptor & 0x0F;
    let (right_to_left, top_to_bottom) = (descriptor & 0x10 != 0, descriptor & 0x20 != 0);
    if w == 0 || ht == 0 {
        return Err(err(name, "empty picture"));
    }
    let pixels = w.checked_mul(ht).filter(|&n| n <= MAX_PIXELS).ok_or_else(|| err(name, "picture too large"))?;
    let rle = kind >= 9;
    // stored value layout and, for colour-mapped images, the palette
    let mut palette: Vec<[u8; 4]> = Vec::new();
    let map_entry_layout = if map_type == 1 {
        Some(Layout::true_colour(map_depth, if map_depth == 16 { attr_bits.min(1) } else { 0 }).ok_or_else(|| err(name, "colour map entry size"))?)
    } else {
        None
    };
    let mut at = HEADER.checked_add(id_len).ok_or_else(|| err(name, "header"))?;
    if let Some(l) = map_entry_layout {
        let size = map_len.checked_mul(l.bytes()).ok_or_else(|| err(name, "colour map"))?;
        let end = at.checked_add(size).ok_or_else(|| err(name, "colour map"))?;
        let map = bytes.get(at..end).ok_or_else(|| err(name, "truncated colour map"))?;
        palette = map.chunks_exact(l.bytes()).map(|e| l.rgba(e)).collect();
        at = end;
    }
    let (layout, mapped) = match kind & 7 {
        1 => {
            if map_type != 1 || !matches!(depth, 8 | 16) {
                return Err(err(name, "colour-mapped picture without an 8/16-bit index and a colour map"));
            }
            (if depth == 8 { Layout::Gray8 } else { Layout::GrayAlpha16 }, true)
        }
        2 => (Layout::true_colour(depth, attr_bits).ok_or_else(|| err(name, "true-colour pixel depth"))?, false),
        3 => match depth {
            8 => (Layout::Gray8, false),
            16 => (Layout::GrayAlpha16, false),
            _ => return Err(err(name, "grayscale pixel depth")),
        },
        _ => return Err(MediaError::Unsupported(format!("{name}: Targa image type {kind}"))),
    };
    let data = bytes.get(at..).unwrap_or_default();
    let bpp = layout.bytes();
    // the stored values, in file order
    let stored: Vec<u8> = if rle {
        unpack_rle(name, data, pixels, bpp)?
    } else {
        let n = pixels.checked_mul(bpp).ok_or_else(|| err(name, "picture too large"))?;
        data.get(..n).ok_or_else(|| err(name, "truncated pixel data"))?.to_vec()
    };
    let mut rgba = vec![0u8; pixels * 4];
    for (i, v) in stored.chunks_exact(bpp).enumerate().take(pixels) {
        let (x, y) = (i % w, i / w);
        let x = if right_to_left { w - 1 - x } else { x };
        let y = if top_to_bottom { y } else { ht - 1 - y };
        let px = if mapped {
            let byte = |i: usize| v.get(i).copied().unwrap_or(0);
            let index = if bpp == 1 { usize::from(byte(0)) } else { usize::from(u16::from_le_bytes([byte(0), byte(1)])) };
            *index.checked_sub(map_first).and_then(|k| palette.get(k)).ok_or_else(|| err(name, "colour index outside the colour map"))?
        } else {
            layout.rgba(v)
        };
        let o = (y * w + x) * 4;
        if let Some(d) = rgba.get_mut(o..o + 4) {
            d.copy_from_slice(&px);
        }
    }
    // 32-bit files from writers that leave the attribute bits unset often store zero alpha
    // everywhere: such a picture is opaque, not invisible
    let alpha_used = match (layout, mapped) {
        (Layout::Bgra32, false) => attr_bits != 0 || rgba.as_chunks::<4>().0.iter().any(|p| p[3] != 0),
        _ => true,
    };
    if !alpha_used {
        rgba.as_chunks_mut::<4>().0.iter_mut().for_each(|p| p[3] = 255);
    }
    let has_alpha = rgba.as_chunks::<4>().0.iter().any(|p| p[3] != 255);
    Ok(Raster { width: w as u32, height: ht as u32, rgba, has_alpha })
}

/// Expand run-length packets into `pixels` stored values of `bpp` bytes each. Packets may span
/// scan lines (the TGA 2.0 spec discourages it; older writers do it).
fn unpack_rle(name: &str, mut data: &[u8], pixels: usize, bpp: usize) -> Result<Vec<u8>> {
    // a two-byte packet expands to at most 128 values: a header asking for more is lying
    let bound = data.len().saturating_mul(64).saturating_add(128);
    if pixels > bound {
        return Err(err(name, "truncated run-length data"));
    }
    let total = pixels.checked_mul(bpp).ok_or_else(|| err(name, "picture too large"))?;
    let mut out = Vec::with_capacity(total);
    while out.len() < total {
        let (&head, rest) = data.split_first().ok_or_else(|| err(name, "truncated run-length data"))?;
        let count = usize::from(head & 0x7F) + 1;
        let want = (total - out.len()).min(count * bpp);
        if head & 0x80 != 0 {
            let v = rest.get(..bpp).ok_or_else(|| err(name, "truncated run-length data"))?;
            for _ in 0..want / bpp {
                out.extend_from_slice(v);
            }
            data = rest.get(bpp..).unwrap_or_default();
        } else {
            let n = count * bpp;
            let v = rest.get(..n).ok_or_else(|| err(name, "truncated run-length data"))?;
            out.extend_from_slice(v.get(..want).unwrap_or(v));
            data = rest.get(n..).unwrap_or_default();
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(kind: u8, w: u16, h: u16, depth: u8, descriptor: u8) -> Vec<u8> {
        let mut b = vec![0, 0, kind, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        b.extend_from_slice(&w.to_le_bytes());
        b.extend_from_slice(&h.to_le_bytes());
        b.extend_from_slice(&[depth, descriptor]);
        b
    }

    #[test]
    fn bottom_up_bgr_is_flipped() {
        let mut f = header(2, 2, 2, 24, 0);
        // bottom row first: blue, green; then top row: red, white
        f.extend_from_slice(&[255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255]);
        let r = decode("a.tga", &f).unwrap();
        assert_eq!((r.width, r.height, r.has_alpha), (2, 2, false));
        assert_eq!(r.rgba, [255, 0, 0, 255, 255, 255, 255, 255, 0, 0, 255, 255, 0, 255, 0, 255]);
    }

    #[test]
    fn rle_runs_and_raw_packets_cross_lines() {
        let mut f = header(10, 3, 2, 32, 0x28);
        // a run of 4 translucent red, then 2 raw pixels
        f.extend_from_slice(&[0x83, 0, 0, 255, 128, 0x01, 1, 2, 3, 255, 4, 5, 6, 255]);
        let r = decode("a.tga", &f).unwrap();
        assert!(r.has_alpha);
        assert_eq!(&r.rgba[..16], &[255, 0, 0, 128].repeat(4)[..]);
        assert_eq!(&r.rgba[16..], &[3, 2, 1, 255, 6, 5, 4, 255]);
    }

    #[test]
    fn colour_map_and_16_bit_alpha() {
        let mut f = header(1, 2, 1, 8, 0x20);
        f[1] = 1; // colour map present
        f[3..5].copy_from_slice(&1u16.to_le_bytes()); // first entry index 1
        f[5..7].copy_from_slice(&2u16.to_le_bytes());
        f[7] = 24;
        f.extend_from_slice(&[10, 20, 30, 40, 50, 60, 2, 1]);
        let r = decode("a.tga", &f).unwrap();
        assert_eq!(r.rgba, [60, 50, 40, 255, 30, 20, 10, 255]);
        f[18 + 6] = 0; // index 0 is below the map's first entry
        assert!(decode("a.tga", &f).is_err());

        let mut f = header(2, 2, 1, 16, 0x21);
        f.extend_from_slice(&0xFC00u16.to_le_bytes()); // opaque full red
        f.extend_from_slice(&0x03E0u16.to_le_bytes()); // transparent full green
        let r = decode("a.tga", &f).unwrap();
        assert_eq!(r.rgba, [255, 0, 0, 255, 0, 255, 0, 0]);
    }

    #[test]
    fn zero_alpha_32_bit_without_attribute_bits_is_opaque() {
        let mut f = header(2, 1, 1, 32, 0);
        f.extend_from_slice(&[1, 2, 3, 0]);
        let r = decode("a.tga", &f).unwrap();
        assert_eq!((r.rgba.as_slice(), r.has_alpha), (&[3, 2, 1, 255][..], false));
    }

    #[test]
    fn hostile_headers_fail_without_allocating() {
        // 65535 × 65535 RLE from a few bytes, truncated raw data, bad types
        let mut f = header(10, 65535, 65535, 32, 0);
        f.extend_from_slice(&[0xFF, 1, 2, 3, 4]);
        assert!(decode("a.tga", &f).is_err());
        let mut f = header(2, 4, 4, 24, 0);
        f.extend_from_slice(&[0; 10]);
        assert!(decode("a.tga", &f).is_err());
        assert!(decode("a.tga", &header(2, 1, 1, 7, 0)).is_err());
        assert!(decode("a.tga", &header(1, 1, 1, 8, 0)).is_err());
        assert!(decode("a.tga", &[0; 5]).is_err());
        for n in 0..40 {
            let mut f = header(9, 3, 3, 8, 0);
            f[1] = 1;
            f[5] = 2;
            f[7] = 16;
            f.extend_from_slice(&[0x82, 1, 0, 1, 0x01, 0, 0]);
            f.truncate(n);
            let _ = decode("a.tga", &f);
        }
    }

    #[test]
    fn sniffs_by_footer_or_name() {
        let mut f = header(2, 1, 1, 24, 0);
        f.extend_from_slice(&[0, 0, 0]);
        assert!(sniff("x.TGA", &f));
        assert!(!sniff("x.bin", &f));
        f.extend_from_slice(&[0; 8]);
        f.extend_from_slice(FOOTER);
        assert!(sniff("x.bin", &f));
        assert!(!sniff("x.tga", b"\x89PNG\r\n\x1a\n0000000000"));
    }
}
