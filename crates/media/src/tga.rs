//! Targa decoding through shared craft-tga; Film retains the media error/raster adapter.
use crate::{MediaError, Result, still::Raster};

pub fn sniff(name: &str, bytes: &[u8]) -> bool {
    craft_tga::sniff(name, bytes)
}

pub fn decode(name: &str, bytes: &[u8]) -> Result<Raster> {
    let image = craft_tga::decode(bytes, &limits()).map_err(|e| match e {
        craft_tga::Error::Unsupported(kind) => MediaError::Unsupported(format!("{name}: Targa image type {kind}")),
        other => MediaError::Decode(format!("{name}: {other}")),
    })?;
    Ok(Raster { width: image.width, height: image.height, rgba: image.rgba, has_alpha: image.has_alpha })
}

/// Film's Targa ceiling is the 2^28-pixel limit alone, as with its previous decoder: the shared
/// default 256 MiB byte budget would refuse pictures from about 38 million 24-bit pixels. The
/// budget covers four stored bytes and four RGBA bytes per pixel plus the largest colour map.
fn limits() -> craft_tga::Limits {
    let mut limits = craft_tga::Limits::default();
    let pixels = usize::try_from(limits.max_pixels).unwrap_or(usize::MAX);
    limits.max_decoded_bytes = pixels.saturating_mul(8).saturating_add(usize::from(u16::MAX) * 4);
    limits
}

#[cfg(test)]
const FOOTER: &[u8] = b"TRUEVISION-XFILE.\0";

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
    fn pixel_count_not_the_byte_budget_bounds_a_picture() {
        let limits = limits();
        assert_eq!(limits.max_pixels, craft_tga::Limits::default().max_pixels);
        // An 8000×6000 24-bit picture stores 3 bytes and decodes to 4 bytes per pixel (336 MB).
        assert!(limits.max_decoded_bytes >= 8000 * 6000 * (3 + 4));
        assert!(craft_tga::Limits::default().max_decoded_bytes < 8000 * 6000 * (3 + 4));
    }

    #[test]
    fn unsupported_image_type_preserves_media_error() {
        assert!(matches!(decode("kind.tga", &header(3 + 1, 1, 1, 24, 0)), Err(MediaError::Unsupported(message)) if message == "kind.tga: Targa image type 4"));
    }

    #[test]
    fn media_route_rejects_footer_as_missing_pixel() {
        let mut f = header(2, 2, 1, 24, 0x20);
        f.extend_from_slice(&[0, 0, 255]);
        f.extend_from_slice(&[0; 8]);
        f.extend_from_slice(FOOTER);
        assert!(matches!(crate::open_bytes("truncated.tga", f.into(), &[]), Err(MediaError::Decode(_))));
    }

    #[test]
    fn media_route_accepts_complete_pixels_before_footer() {
        let mut f = header(2, 1, 1, 24, 0x20);
        f.extend_from_slice(&[0, 0, 255]);
        f.extend_from_slice(&[0; 8]);
        f.extend_from_slice(FOOTER);
        assert!(crate::open_bytes("complete.tga", f.clone().into(), &[]).is_ok());
        assert_eq!(decode("complete.tga", &f).unwrap().rgba, [255, 0, 0, 255]);
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
