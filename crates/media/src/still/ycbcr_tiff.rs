//! YCbCr TIFF still adapter. Codec/colour conversion lives in craft-tiff; FilmCraft
//! retains its classic-TIFF compatibility and upright-image policy.
use super::Raster;
use crate::{MediaError, Result};
use craft_tiff::classic::{Tiff, Type, tags};
use craft_tiff::{Entry, Ifd, Value};
use craft_tiff_ycbcr as craft_tiff;
use image::metadata::Orientation;

const MAX_PIXELS: u64 = 1 << 28;
// Typed metadata expands every integer/float to eight bytes, independently of the
// pixel limit. Bound the cumulative fields before any of those arrays are allocated.
const MAX_METADATA_VALUES: u64 = (64 << 20) / 8;
const UINT_TAGS: [u16; 10] = [256, 257, 258, 259, 262, 273, 277, 279, 284, 530];
const FLOAT_TAGS: [u16; 2] = [529, 532];

/// Read only the first unsigned value, while retaining validation of the full field.
/// The parsed entry already records its inline/out-of-line address; reducing its
/// count does not reinterpret an out-of-line value as inline storage.
fn first_uint(t: &Tiff<'_>, e: &craft_tiff::classic::Entry) -> Option<u64> {
    t.value_bytes(e).ok()?;
    let mut first = *e;
    first.count = first.count.min(1);
    t.uint(&first).ok()
}

/// Recognize the classic uncompressed/PackBits YCbCr route.
pub fn sniff(bytes: &[u8]) -> bool {
    let Ok((t, first)) = Tiff::new(bytes) else { return false };
    let Ok((ifd, _)) = t.read_ifd(first, 0) else { return false };
    let num = |tag| ifd.get(tag).and_then(|e| first_uint(&t, e));
    num(tags::PHOTOMETRIC) == Some(6) && matches!(num(tags::COMPRESSION).unwrap_or(1), 1 | 32773)
}

/// The size [`decode`] reports for this file and the orientation it applies, from the
/// image directory alone: the stored ImageWidth and ImageLength, turned by the same
/// single-SHORT Orientation policy [`decode`] keeps, without decoding a pixel. `None`
/// when the directory or either size cannot be read, or the picture is past the pixel
/// ceiling [`decode`] enforces.
pub fn geometry(bytes: &[u8]) -> Option<(u32, u32, Orientation)> {
    let (t, first) = Tiff::new(bytes).ok()?;
    let (ifd, _) = t.read_ifd(first, 0).ok()?;
    let width = u32::try_from(first_uint(&t, ifd.get(tags::IMAGE_WIDTH)?)?).ok()?;
    let height = u32::try_from(first_uint(&t, ifd.get(tags::IMAGE_LENGTH)?)?).ok()?;
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return None;
    }
    let orientation = ifd
        .get(274)
        .filter(|e| e.ty == Type::Short && e.count == 1)
        .and_then(|e| t.uint(e).ok())
        .and_then(|v| u8::try_from(v).ok())
        .and_then(Orientation::from_exif)
        .unwrap_or(Orientation::NoTransforms);
    let (width, height) = match orientation {
        Orientation::Rotate90 | Orientation::Rotate90FlipH | Orientation::Rotate270 | Orientation::Rotate270FlipH => (height, width),
        _ => (width, height),
    };
    Some((width, height, orientation))
}

/// Decode with the shared codec and retain the app's colour and orientation policy.
pub fn decode(name: &str, bytes: &[u8]) -> Result<Raster> {
    let bad = |what: &str| MediaError::Decode(format!("{name}: YCbCr TIFF: {what}"));
    let (t, first) = Tiff::new(bytes).map_err(|_| bad("not a TIFF"))?;
    let (ifd, _) = t.read_ifd(first, 0).map_err(|_| bad("unreadable image directory"))?;
    // Keep the existing lazy first-directory policy: unrelated or damaged metadata is
    // not traversed, and only the unsigned/colour fields the codec needs are decoded.
    let metadata_values = UINT_TAGS.into_iter().chain(FLOAT_TAGS).filter_map(|tag| ifd.get(tag)).map(|e| u64::from(e.count)).sum::<u64>();
    if metadata_values > MAX_METADATA_VALUES {
        return Err(MediaError::Unsupported(format!("{name}: YCbCr TIFF metadata exceeds the 64 MiB decoded-value budget")));
    }
    let mut image = Ifd::default();
    for tag in UINT_TAGS {
        if let Some(e) = ifd.get(tag) {
            image.entries.push(Entry { tag, value: Value::Long8(t.uints(e).map_err(|_| bad("unreadable field"))?), offset: 0 });
        }
    }
    if ifd.get(tags::TILE_OFFSETS).is_some() {
        image.entries.push(Entry { tag: tags::TILE_OFFSETS, value: Value::Long(vec![]), offset: 0 });
    }
    for tag in FLOAT_TAGS {
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
    Ok(Raster { width: img.width(), height: img.height(), rgba: img.into_raw(), has_alpha: false, orientation })
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

    fn directory(entries: &[(u16, u16, u32, u32)]) -> Vec<u8> {
        let mut bytes = b"II*\0\x08\0\0\0".to_vec();
        bytes.extend((entries.len() as u16).to_le_bytes());
        for &(tag, ty, count, value) in entries {
            bytes.extend(tag.to_le_bytes());
            bytes.extend(ty.to_le_bytes());
            bytes.extend(count.to_le_bytes());
            bytes.extend(value.to_le_bytes());
        }
        bytes.extend(0u32.to_le_bytes());
        bytes
    }

    #[test]
    fn metadata_budget_precedes_array_materialization_and_counts_all_used_fields() {
        for entries in [vec![(273, 4, u32::MAX, 0)], vec![(273, 4, (MAX_METADATA_VALUES / 2) as u32, 0), (532, 5, (MAX_METADATA_VALUES / 2 + 1) as u32, 0)]] {
            let error = decode("hostile.tiff", &directory(&entries)).err().unwrap();
            assert!(matches!(error, MediaError::Unsupported(ref why) if why.contains("64 MiB decoded-value budget")), "{error}");
        }
        // The inclusive budget boundary reaches ordinary field validation without
        // allocating: this tiny fixture deliberately lacks the declared value bytes.
        let error = decode("boundary.tiff", &directory(&[(273, 4, MAX_METADATA_VALUES as u32, 0)])).err().unwrap();
        assert!(matches!(error, MediaError::Decode(ref why) if why.contains("unreadable field")), "{error}");
        // An unrelated entry is not charged or traversed by the lazy adapter.
        let error = decode("unrelated.tiff", &directory(&[(65000, 4, u32::MAX, 0)])).err().unwrap();
        assert!(matches!(error, MediaError::Decode(ref why) if why.contains("no ImageWidth")), "{error}");
    }

    #[test]
    fn sniff_reads_first_value_without_changing_storage_or_truncation_rules() {
        // Three SHORTs live out of line, not in the IFD's value slot.
        let offset = 8 + 2 + 12 + 4;
        let mut bytes = directory(&[(262, 3, 3, offset)]);
        bytes.extend([6, 0, 99, 0, 99, 0]);
        assert!(sniff(&bytes));
        bytes.pop();
        assert!(!sniff(&bytes), "the original complete field must still be in bounds");
        assert!(!sniff(&directory(&[(262, 3, 0, 6)])));
    }

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
