//! HEIF / HEIC stills: iPhone and Mac photos (and cameras that shoot HEIF), through `craft-heif`
//! (storytold/craft-libs): a pure-Rust container and HEVC still-picture decoder whose every call
//! runs under `catch_unwind`. Single and grid-tiled pictures, 8 to 16 bits, alpha, with the
//! container's rotation, mirror and crop applied so the photo comes in upright. The picture is
//! kept at 8 bits, like the other stills, in sRGB: a photo in Display P3 (iPhone, Mac) or BT.2020
//! is converted from its primaries, read from the `nclx` code points or the ICC profile's
//! colorants, before it is quantised.

use filmcraft_color::Gamut;
use filmcraft_color::spaces::{D65, Mat3, apply3, gamut_matrix, mul, mul_vec, npm, to_f32};

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
    let major = brands.get(..4).unwrap_or(&[]);
    // AVIF (major brand avif / avis) and image sequences (msf1, hevc / avcs) are not ours, even
    // when they list mif1 or heic as compatible brands
    if matches!(major, b"avif" | b"avis" | b"msf1" | b"hevc" | b"hevx" | b"avcs") {
        return false;
    }
    let mut found = BRANDS.iter().any(|x| &x[..] == major);
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
    let gamut = source_gamut(&d);
    let convert = (gamut != Gamut::Bt709).then(|| GamutToSrgb::new(gamut));
    let mut rgba = Vec::with_capacity(pixels.saturating_mul(4));
    let mut has_alpha = false;
    for px in d.data.chunks_exact(channels).take(pixels) {
        let a = if d.has_alpha { px.get(3).copied().map_or(255, to8) } else { 255 };
        has_alpha |= a < 255;
        let rgb = match &convert {
            Some(c) => c.apply([px[0], px[1], px[2]]),
            None => [to8(px[0]), to8(px[1]), to8(px[2])],
        };
        rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], a]);
    }
    Ok(Raster { width: d.width, height: d.height, rgba, has_alpha })
}

/// The primaries the decoded samples are in. An ICC profile names them by its colorants (iPhone
/// and Mac photos carry a Display P3 profile); otherwise the `nclx` / VUI code points of ITU-T
/// H.273 do (1 BT.709, 9 BT.2020, 12 P3 D65). Anything else is read as sRGB / BT.709. The
/// transfer is taken as the sRGB curve, as the rest of the still pipeline does.
fn source_gamut(d: &craft_heif::Decoded) -> Gamut {
    if let Some(g) = d.icc.as_deref().and_then(icc_gamut) {
        return g;
    }
    match d.colour.primaries {
        9 => Gamut::Bt2020,
        12 => Gamut::P3D65,
        _ => Gamut::Bt709,
    }
}

/// Bradford adaptation from D65 to D50, the ICC profile connection space's white.
const D65_TO_D50: Mat3 = [[1.047_811_2, 0.022_886_6, -0.050_127], [0.029_542_4, 0.990_484_4, -0.017_049_1], [-0.009_234_5, 0.015_043_6, 0.752_131_6]];

/// The gamut whose D50-adapted colorants match the profile's `rXYZ` / `gXYZ` / `bXYZ` tags, if
/// one of BT.709, P3 D65 or BT.2020 does (within 0.01). Malformed profiles give `None`.
pub(crate) fn icc_gamut(icc: &[u8]) -> Option<Gamut> {
    let u32_at = |o: usize| icc.get(o..o.checked_add(4)?).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    let count = usize::try_from(u32_at(128)?).ok()?.min(256);
    let tag = |sig: &[u8; 4]| -> Option<[f64; 3]> {
        for i in 0..count {
            let e = 132 + i * 12;
            if icc.get(e..e + 4)? == sig {
                let off = usize::try_from(u32_at(e + 4)?).ok()?;
                // XYZType: 'XYZ ', 4 reserved bytes, then three s15Fixed16Number
                if icc.get(off..off.checked_add(4)?)? != b"XYZ " {
                    return None;
                }
                let fixed = |k: usize| u32_at(off + 8 + 4 * k).map(|v| f64::from(v as i32) / 65536.0);
                return Some([fixed(0)?, fixed(1)?, fixed(2)?]);
            }
        }
        None
    };
    let found = [tag(b"rXYZ")?, tag(b"gXYZ")?, tag(b"bXYZ")?];
    [Gamut::Bt709, Gamut::P3D65, Gamut::Bt2020].into_iter().find(|g| {
        let m = mul(&D65_TO_D50, &npm(g.primaries(), D65));
        (0..3).all(|c| {
            let want = mul_vec(&m, [f64::from(u8::from(c == 0)), f64::from(u8::from(c == 1)), f64::from(u8::from(c == 2))]);
            (0..3).all(|k| (want[k] - found[c][k]).abs() < 0.01)
        })
    })
}

/// 16-bit samples in another gamut → sRGB 8-bit: decode the sRGB curve, convert the primaries in
/// linear light, clip to the sRGB gamut and encode again. Tables keep it cheap for large photos.
struct GamutToSrgb {
    m: [[f32; 3]; 3],
    /// 16-bit code → linear (every 16th code; the rest interpolate).
    decode: Vec<f32>,
    /// linear in 1/4095 steps → 8-bit sRGB code.
    encode: Vec<u8>,
}

impl GamutToSrgb {
    fn new(src: Gamut) -> Self {
        let decode = (0..=4096u32).map(|i| filmcraft_color::srgb_to_linear((i * 16).min(65535) as f32 / 65535.0)).collect();
        let encode = (0..=4095u32).map(|i| (filmcraft_color::linear_to_srgb(i as f32 / 4095.0) * 255.0).round().clamp(0.0, 255.0) as u8).collect();
        GamutToSrgb { m: to_f32(&gamut_matrix(src, Gamut::Bt709)), decode, encode }
    }

    fn lin(&self, v: u16) -> f32 {
        let i = usize::from(v >> 4);
        let f = f32::from(v & 15) / 16.0;
        let a = self.decode.get(i).copied().unwrap_or(1.0);
        let b = self.decode.get(i + 1).copied().unwrap_or(a);
        a + (b - a) * f
    }

    fn apply(&self, px: [u16; 3]) -> [u8; 3] {
        let out = apply3(&self.m, [self.lin(px[0]), self.lin(px[1]), self.lin(px[2])]);
        out.map(|v| self.encode.get((v.clamp(0.0, 1.0) * 4095.0).round() as usize).copied().unwrap_or(255))
    }
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
        // an MP4 and an AVIF are not HEIF stills (also an AVIF whose major brand alone says so)
        assert!(!sniff(b"\0\0\0\x18ftypavif\0\0\0\0mif1miaf"));
        // an image sequence that lists the still brands is not a still either
        assert!(!sniff(b"\0\0\0\x1cftypmsf1\0\0\0\0mif1heicmsf1"));
        assert!(!sniff(b"\0\0\0\x18ftypisom\0\0\0\0isommp41"));
        assert!(!sniff(b"\0\0\0\x1cftypavif\0\0\0\0mif1avifmiaf"));
        assert!(!sniff(b"\0\0\0\x18ftypmif1\0\0\0\0mif1avif"));
        assert!(!sniff(b"short"));
    }

    /// A reddish full-range BT.709 YCbCr sample.
    fn red(_: u32, _: u32) -> [u16; 3] {
        [110, 100, 190]
    }

    /// The same samples tagged BT.709 and Display P3: the P3 photo is converted, so it comes out
    /// more saturated than the BT.709 one, by the P3 → BT.709 matrix in linear light.
    fn p3_and_709(p3_props: Vec<Prop>) -> ([u8; 4], [u8; 4]) {
        let mut spec = Spec::new(16, 16, &red);
        spec.props.push(Prop::Nclx(1, 13, 1, true));
        let srgb = decode("709.heic", &build(&spec)).unwrap().rgba;
        let mut spec = Spec::new(16, 16, &red);
        spec.props = p3_props;
        let p3 = decode("p3.heic", &build(&spec)).unwrap().rgba;
        ([srgb[0], srgb[1], srgb[2], srgb[3]], [p3[0], p3[1], p3[2], p3[3]])
    }

    /// The expected sRGB of an sRGB-coded value read in P3: decode, convert, clip, encode.
    fn p3_expected(px: [u8; 4]) -> [u8; 3] {
        let m = to_f32(&gamut_matrix(Gamut::P3D65, Gamut::Bt709));
        let lin = [px[0], px[1], px[2]].map(|v| filmcraft_color::srgb_to_linear(f32::from(v) / 255.0));
        apply3(&m, lin).map(|v| (filmcraft_color::linear_to_srgb(v.clamp(0.0, 1.0)) * 255.0).round() as u8)
    }

    #[test]
    fn display_p3_photos_are_converted_to_srgb() {
        let (srgb, p3) = p3_and_709(vec![Prop::Nclx(12, 13, 1, true)]);
        let want = p3_expected(srgb);
        for k in 0..3 {
            assert!((i32::from(p3[k]) - i32::from(want[k])).abs() <= 1, "channel {k}: {p3:?} vs {want:?} (from {srgb:?})");
        }
        assert!(p3[0] > srgb[0] && p3[1] < srgb[1], "P3 red is redder than the same numbers in sRGB: {p3:?} vs {srgb:?}");
        assert_eq!(p3[3], 255);
    }

    /// A Display P3 ICC profile (an iPhone photo's) is recognised by its colorants; a profile of
    /// sRGB colorants leaves the samples as they are; a damaged profile changes nothing.
    #[test]
    fn icc_profiles_name_the_primaries() {
        let profile = |g: Gamut| -> Vec<u8> {
            let m = mul(&D65_TO_D50, &npm(g.primaries(), D65));
            let mut icc = vec![0u8; 128];
            icc.extend_from_slice(&3u32.to_be_bytes());
            let base = 132 + 3 * 12;
            for (i, sig) in [b"rXYZ", b"gXYZ", b"bXYZ"].into_iter().enumerate() {
                icc.extend_from_slice(sig);
                icc.extend_from_slice(&((base + i * 20) as u32).to_be_bytes());
                icc.extend_from_slice(&20u32.to_be_bytes());
            }
            for c in 0..3 {
                icc.extend_from_slice(b"XYZ \0\0\0\0");
                for k in 0..3 {
                    icc.extend_from_slice(&((m[k][c] * 65536.0).round() as i32).to_be_bytes());
                }
            }
            icc
        };
        assert_eq!(icc_gamut(&profile(Gamut::P3D65)), Some(Gamut::P3D65));
        assert_eq!(icc_gamut(&profile(Gamut::Bt709)), Some(Gamut::Bt709));
        assert_eq!(icc_gamut(&profile(Gamut::Bt2020)), Some(Gamut::Bt2020));
        // the profile names the primaries; the nclx (primaries unspecified) keeps the same YCbCr
        // matrix and range as the BT.709 reference
        let (srgb, p3) = p3_and_709(vec![Prop::Icc(profile(Gamut::P3D65)), Prop::Nclx(2, 13, 1, true)]);
        let want = p3_expected(srgb);
        assert!((0..3).all(|k| (i32::from(p3[k]) - i32::from(want[k])).abs() <= 1), "{p3:?} vs {want:?}");
        // damaged profiles: no panic, no gamut
        let good = profile(Gamut::P3D65);
        for n in [0, 10, 128, 140, good.len() - 1] {
            assert_eq!(icc_gamut(&good[..n]), None, "{n} bytes");
        }
        let mut huge = good.clone();
        huge[128..132].copy_from_slice(&u32::MAX.to_be_bytes());
        let _ = icc_gamut(&huge);
    }
}
