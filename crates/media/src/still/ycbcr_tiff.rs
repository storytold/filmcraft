//! YCbCr TIFF still adapter. Codec/colour conversion lives in craft-tiff; FilmCraft
//! retains its classic-TIFF compatibility and upright-image policy.
use super::Raster;
use crate::{MediaError, Result};
use craft_tiff::classic::{Tiff, Type, tags};
use craft_tiff::{Entry, Ifd, Value};
use craft_tiff_ycbcr as craft_tiff;
use image::metadata::Orientation;

const MAX_PIXELS: u64 = 1 << 28;

/// Recognize the classic uncompressed/PackBits YCbCr route.
pub fn sniff(bytes: &[u8]) -> bool {
    let Ok((t, first)) = Tiff::new(bytes) else { return false };
    let Ok((ifd, _)) = t.read_ifd(first, 0) else { return false };
    let num = |tag| ifd.get(tag).and_then(|e| t.uint(e).ok());
    num(tags::PHOTOMETRIC) == Some(6) && matches!(num(tags::COMPRESSION).unwrap_or(1), 1 | 32773)
}

/// Decode with the shared codec and retain the app's colour and orientation policy.
pub fn decode(name: &str, bytes: &[u8]) -> Result<Raster> {
    let bad = |what: &str| MediaError::Decode(format!("{name}: YCbCr TIFF: {what}"));
    let (t, first) = Tiff::new(bytes).map_err(|_| bad("not a TIFF"))?;
    let (ifd, _) = t.read_ifd(first, 0).map_err(|_| bad("unreadable image directory"))?;
    // Keep the existing lazy first-directory policy: unrelated or damaged metadata is
    // not traversed, and only the unsigned/colour fields the codec needs are decoded.
    let mut image = Ifd::default();
    for tag in [256, 257, 258, 259, 262, 273, 277, 279, 284, 530] {
        if let Some(e) = ifd.get(tag) {
            image.entries.push(Entry { tag, value: Value::Long8(t.uints(e).map_err(|_| bad("unreadable field"))?), offset: 0 });
        }
    }
    if ifd.get(tags::TILE_OFFSETS).is_some() {
        image.entries.push(Entry { tag: tags::TILE_OFFSETS, value: Value::Long(vec![]), offset: 0 });
    }
    for tag in [529, 532] {
        if let Some(e) = ifd.get(tag) {
            image.entries.push(Entry { tag, value: Value::Double(t.floats(e).map_err(|_| bad("unreadable colour field"))?), offset: 0 });
        }
    }
    image.entries.sort_by_key(|e| e.tag);
    let width = image.u32(tags::IMAGE_WIDTH).ok_or_else(|| bad("no ImageWidth"))?;
    let height = image.u32(tags::IMAGE_LENGTH).ok_or_else(|| bad("no ImageLength"))?;
    let rgba = craft_tiff::ycbcr::decode_rgba8(bytes, &image, MAX_PIXELS, craft_tiff::ycbcr::ColorPolicy::TiffEquation).map_err(|e| match e {
        craft_tiff::TiffError::Limit("pixel count") => MediaError::Unsupported(format!("{name}: YCbCr TIFF with more than 268 million pixels")),
        craft_tiff::TiffError::Invalid(ref why) if why.starts_with("unsupported YCbCr image with ") => {
            let detail = why.strip_prefix("unsupported YCbCr image with ").unwrap_or(why);
            MediaError::Unsupported(format!("{name}: YCbCr TIFF with {detail}"))
        }
        other => bad(&other.to_string()),
    })?;
    // Preserve #4608: only a single SHORT Orientation is meaningful. Damaged values
    // remain unread, rather than allocating according to their declared count.
    let orientation = ifd
        .get(274)
        .filter(|e| e.ty == Type::Short && e.count == 1)
        .and_then(|e| t.uint(e).ok())
        .and_then(|v| u8::try_from(v).ok())
        .and_then(Orientation::from_exif)
        .unwrap_or(Orientation::NoTransforms);
    let img = image::RgbaImage::from_raw(width, height, rgba).ok_or_else(|| bad("pixel buffer does not match the image size"))?;
    let img = orient(img, orientation).map_err(bad)?;
    Ok(Raster { width: img.width(), height: img.height(), rgba: img.into_raw(), has_alpha: false })
}

// Unlike DynamicImage::apply_orientation, the copying rotations reserve fallibly.
fn orient(mut img: image::RgbaImage, orientation: Orientation) -> std::result::Result<image::RgbaImage, &'static str> {
    use image::imageops;
    match orientation {
        Orientation::NoTransforms => {}
        Orientation::Rotate180 => imageops::rotate180_in_place(&mut img),
        Orientation::FlipHorizontal => imageops::flip_horizontal_in_place(&mut img),
        Orientation::FlipVertical => imageops::flip_vertical_in_place(&mut img),
        Orientation::Rotate90 | Orientation::Rotate270 | Orientation::Rotate90FlipH | Orientation::Rotate270FlipH => {
            let mut storage = Vec::new();
            storage.try_reserve_exact(img.as_raw().len()).map_err(|_| "image too large to orient")?;
            storage.resize(img.as_raw().len(), 0);
            let mut dst = image::RgbaImage::from_raw(img.height(), img.width(), storage).ok_or("invalid rotated image size")?;
            if matches!(orientation, Orientation::Rotate90 | Orientation::Rotate90FlipH) {
                imageops::rotate90_in(&img, &mut dst).map_err(|_| "invalid rotated image size")?;
            } else {
                imageops::rotate270_in(&img, &mut dst).map_err(|_| "invalid rotated image size")?;
            }
            if matches!(orientation, Orientation::Rotate90FlipH | Orientation::Rotate270FlipH) {
                imageops::flip_horizontal_in_place(&mut dst);
            }
            img = dst;
        }
    }
    Ok(img)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallible_orientation_matches_image_for_all_eight_values() {
        let pixels: Vec<u8> = (0..24).collect();
        let source = image::RgbaImage::from_raw(3, 2, pixels).unwrap();
        for value in 1..=8 {
            let orientation = Orientation::from_exif(value).unwrap();
            let mut expected = image::DynamicImage::ImageRgba8(source.clone());
            expected.apply_orientation(orientation);
            let got = orient(source.clone(), orientation).unwrap();
            assert_eq!(got, expected.into_rgba8(), "orientation {value}");
        }
    }
}
