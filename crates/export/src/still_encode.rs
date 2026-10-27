use super::format::{ExportSettings, Format};
use super::*;

/// Encode straight sRGB RGBA8 pixels as a PNG file (thumbnails, previews).
pub fn encode_png(rgba: Vec<u8>, w: u32, h: u32) -> Result<Vec<u8>> {
    encode_still(Format::PngSequence, rgba, w, h, true)
}

/// Encode one still of an image sequence (also Export Frame). `alpha` keeps the alpha channel in
/// PNG, TIFF, Targa and OpenEXR, which are otherwise written as RGB.
/// JPEG uses the default export quality ([`encode_still_with_quality`] picks another).
pub fn encode_still(format: Format, rgba: Vec<u8>, w: u32, h: u32, alpha: bool) -> Result<Vec<u8>> {
    encode_still_with_quality(format, rgba, w, h, alpha, ExportSettings::default().quality)
}

/// [`encode_still`] with the JPEG quality (1–100; ignored by the lossless formats).
pub fn encode_still_with_quality(format: Format, rgba: Vec<u8>, w: u32, h: u32, alpha: bool, quality: u8) -> Result<Vec<u8>> {
    encode_still_with(format, rgba, w, h, alpha, quality, ExrCompression::None)
}

/// [`encode_still_with_quality`] with the OpenEXR compression (ignored by the other formats).
pub fn encode_still_with(format: Format, rgba: Vec<u8>, w: u32, h: u32, alpha: bool, quality: u8, exr: ExrCompression) -> Result<Vec<u8>> {
    let enc = |e: image::ImageError| ExportError::Encode(e.to_string());
    let mut out = std::io::Cursor::new(Vec::new());
    match format {
        Format::JpegSequence => {
            let rgb = raster::rgb(&rgba, w, h)?;
            image::ImageEncoder::write_image(
                image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality.clamp(1, 100)),
                &rgb,
                w,
                h,
                image::ExtendedColorType::Rgb8,
            )
            .map_err(enc)?
        }
        Format::TgaSequence => return raster::tga(&rgba, w, h, alpha),
        Format::DpxSequence => return raster::dpx(&rgba, w, h),
        Format::ExrSequence => return raster::exr(&rgba, w, h, alpha, exr),
        Format::PngSequence if alpha => {
            image::ImageEncoder::write_image(image::codecs::png::PngEncoder::new(&mut out), &rgba, w, h, image::ExtendedColorType::Rgba8).map_err(enc)?
        }
        Format::PngSequence => {
            let rgb = raster::rgb(&rgba, w, h)?;
            image::ImageEncoder::write_image(image::codecs::png::PngEncoder::new(&mut out), &rgb, w, h, image::ExtendedColorType::Rgb8).map_err(enc)?
        }
        Format::TiffSequence | Format::BmpSequence => {
            let img = image::RgbaImage::from_raw(w, h, rgba).ok_or_else(|| ExportError::Encode("frame size".into()))?;
            let f = if format == Format::TiffSequence { image::ImageFormat::Tiff } else { image::ImageFormat::Bmp };
            if alpha && format == Format::TiffSequence {
                img.write_to(&mut out, f).map_err(enc)?
            } else {
                image::DynamicImage::ImageRgba8(img).to_rgb8().write_to(&mut out, f).map_err(enc)?
            }
        }
        _ => return Err(ExportError::Unsupported(format!("{} is not an image sequence", format.label()))),
    }
    Ok(out.into_inner())
}
