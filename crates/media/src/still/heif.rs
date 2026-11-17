//! HEIF / HEIC stills: iPhone and Mac photos (and cameras that shoot HEIF), through `craft-heif`
//! (storytold/craft-libs): a pure-Rust container and HEVC still-picture decoder whose every call
//! runs under `catch_unwind`. Single and grid-tiled pictures, 8 to 16 bits, alpha, with the
//! container's rotation, mirror and crop applied so the photo comes in upright. The picture is
//! kept at 8 bits, like the other stills.

use super::Raster;
use crate::{MediaError, Result};

/// Brands of the `ftyp` box that name an HEVC-coded HEIF still. `msf1` (an image sequence) and
/// AVIF are not stills this decoder reads.
const BRANDS: [&[u8; 4]; 6] = [b"heic", b"heix", b"heim", b"heis", b"mif1", b"mif2"];

/// Whether `b` starts like a HEIF still: an `ftyp` box whose major brand (or, for the generic
/// `mif1` major brand, a compatible brand) names one.
pub fn sniff(b: &[u8]) -> bool {
    let Some(head) = b.get(..12) else { return false };
    if head.get(4..8) != Some(&b"ftyp"[..]) {
        return false;
    }
    let size = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
    let end = size.clamp(16, 256).min(b.len());
    let brands = b.get(8..end).unwrap_or(&[]);
    // major brand, then (after the minor version) the compatible brands, four bytes each
    let mut found = brands.get(..4).is_some_and(|m| BRANDS.iter().any(|x| &x[..] == m));
    for c in brands.get(8..).unwrap_or(&[]).as_chunks::<4>().0 {
        found |= BRANDS.contains(&c);
        // an AVIF that also lists mif1 is not ours
        if c == b"avif" || c == b"avis" {
            return false;
        }
    }
    found
}

/// Decode a HEIF still into straight-alpha RGBA8.
pub fn decode(name: &str, bytes: &[u8]) -> Result<Raster> {
    let d = craft_heif::decode(bytes, &craft_heif::Options::default()).map_err(|e| match e {
        craft_heif::Error::Unsupported(m) => MediaError::Unsupported(format!("{name}: HEIF {m}")),
        craft_heif::Error::Limit(m) => MediaError::Unsupported(format!("{name}: {m}")),
        craft_heif::Error::Malformed(m) => MediaError::Decode(format!("{name}: {m}")),
    })?;
    let channels = if d.has_alpha { 4 } else { 3 };
    let pixels = (d.width as usize).checked_mul(d.height as usize).ok_or_else(|| MediaError::Decode(format!("{name}: picture size overflows")))?;
    if d.data.len() < pixels.saturating_mul(channels) {
        return Err(MediaError::Decode(format!("{name}: decoded picture is shorter than its size")));
    }
    // 16-bit samples (0 … 65535 whatever the coded depth) to 8 bits, rounded
    let to8 = |v: u16| ((u32::from(v) * 255 + 32767) / 65535) as u8;
    let mut rgba = Vec::with_capacity(pixels.saturating_mul(4));
    let mut has_alpha = false;
    for px in d.data.chunks_exact(channels).take(pixels) {
        let a = if d.has_alpha { px.get(3).copied().map_or(255, to8) } else { 255 };
        has_alpha |= a < 255;
        rgba.extend_from_slice(&[to8(px[0]), to8(px[1]), to8(px[2]), a]);
    }
    Ok(Raster { width: d.width, height: d.height, rgba, has_alpha })
}

#[cfg(test)]
mod tests {
    use super::*;
    use craft_heif::testdata::{Prop, Spec, build};

    /// A neutral grey ramp, full-range BT.709, so the expected RGB is the luma itself.
    fn ramp(x: u32, y: u32) -> [u16; 3] {
        [(16 + (x + y) * 3) as u16, 128, 128]
    }

    /// iPhone / Mac photos (HEIF / HEIC) import: a lossless synthetic file decodes to the
    /// samples it was written with, grid-tiled sizes and the container's rotation included.
    #[test]
    fn heic_stills_decode_upright() {
        let mut spec = Spec::new(64, 48, &ramp);
        spec.props.push(Prop::Nclx(1, 13, 1, true));
        let bytes = build(&spec);
        assert!(sniff(&bytes));
        assert!(crate::still::sniff("photo.heic", &bytes));
        let r = crate::still::decode_raster("photo.heic", &bytes).unwrap();
        assert_eq!((r.width, r.height), (64, 48));
        assert!(!r.has_alpha);
        let px = |x: usize, y: usize| r.rgba[(y * 64 + x) * 4];
        assert_eq!(px(0, 0), 16);
        assert_eq!(px(10, 5), 16 + 45);
        // a quarter turn swaps the size (the container says how the photo was held)
        spec.props.push(Prop::Irot(1));
        let r = decode("turned.heic", &build(&spec)).unwrap();
        assert_eq!((r.width, r.height), (48, 64));
        // the extension is listed for the import dialogs
        assert!(crate::STILL_EXTENSIONS.contains(&"heic") && crate::STILL_EXTENSIONS.contains(&"heif"));
    }

    /// Hostile input never panics: truncations, bit flips and other ISO-BMFF files are refused
    /// (or sniffed out) with an error.
    #[test]
    fn damaged_heif_files_are_refused_without_a_panic() {
        let bytes = build(&Spec::new(32, 32, &ramp));
        for n in [0, 8, 12, 40, bytes.len() / 2, bytes.len() - 1] {
            let cut = &bytes[..n];
            if sniff(cut) {
                assert!(decode("cut.heic", cut).is_err(), "{n} bytes");
            }
        }
        for i in (0..bytes.len()).step_by(7) {
            let mut b = bytes.clone();
            b[i] ^= 0xA5;
            let _ = std::panic::catch_unwind(|| decode("flip.heic", &b)).expect("no panic");
        }
        // an MP4 and an AVIF are not HEIF stills
        assert!(!sniff(b"\0\0\0\x18ftypisom\0\0\0\0isommp41"));
        assert!(!sniff(b"\0\0\0\x1cftypavif\0\0\0\0mif1avifmiaf"));
        assert!(!sniff(b"\0\0\0\x18ftypmif1\0\0\0\0mif1avif"));
        assert!(!sniff(b"short"));
    }
}
