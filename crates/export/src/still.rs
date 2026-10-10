//! Single-frame formats and true per-channel depth, separate from sequence export settings.
use crate::{ExportError, Format, Result};
use filmcraft_render::Image;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StillFormat {
    Png,
    Tiff,
    Bmp,
    Jpeg,
}
impl StillFormat {
    pub const ALL: [Self; 4] = [Self::Png, Self::Jpeg, Self::Tiff, Self::Bmp];
    pub fn parse(name: &str) -> Result<Self> {
        match name.to_ascii_lowercase().as_str() {
            "png" => Ok(Self::Png),
            "tif" | "tiff" => Ok(Self::Tiff),
            "bmp" => Ok(Self::Bmp),
            "jpg" | "jpeg" => Ok(Self::Jpeg),
            _ => Err(ExportError::Unsupported("still format must be png, jpeg, tiff or bmp".into())),
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Png => "PNG",
            Self::Tiff => "TIFF",
            Self::Bmp => "BMP",
            Self::Jpeg => "JPEG",
        }
    }
    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Tiff => "tiff",
            Self::Bmp => "bmp",
            Self::Jpeg => "jpg",
        }
    }
    pub fn supports_16(self) -> bool {
        matches!(self, Self::Png | Self::Tiff)
    }
}

/// Quantize the original float image directly, never pad an 8-bit image to 16 bits.
pub fn encode(image: &Image, format: StillFormat, depth: u8) -> Result<Vec<u8>> {
    if !matches!(depth, 8 | 16) || (depth == 16 && !format.supports_16()) {
        return Err(ExportError::Unsupported("16-bit stills require PNG or TIFF; JPEG and BMP support 8 bits per channel".into()));
    }
    let bad = || ExportError::Encode("invalid still image dimensions or pixel buffer".into());
    let w = u32::try_from(image.w).map_err(|_| bad())?;
    let h = u32::try_from(image.h).map_err(|_| bad())?;
    filmcraft_project::validate_frame_size(w, h).map_err(|_| bad())?;
    let samples = image.w.checked_mul(image.h).and_then(|n| n.checked_mul(4)).ok_or_else(bad)?;
    if image.px.len() != samples {
        return Err(bad());
    }
    let err = |e: image::ImageError| ExportError::Encode(e.to_string());
    if depth == 8 {
        let rgba = image.to_rgba8();
        if format == StillFormat::Jpeg {
            let rgb: Vec<u8> = rgba
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| {
                    let [r, g, b, _] = *p;
                    [r, g, b]
                })
                .collect();
            let mut out = Vec::new();
            image::ImageEncoder::write_image(image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 95), &rgb, w, h, image::ExtendedColorType::Rgb8)
                .map_err(err)?;
            return Ok(out);
        }
        let f = match format {
            StillFormat::Png => Format::PngSequence,
            StillFormat::Tiff => Format::TiffSequence,
            _ => Format::BmpSequence,
        };
        // 8-bit TIFF stays RGB, as Export Frame wrote it before; 16-bit output keeps alpha.
        return crate::encode_still(f, rgba, w, h, false);
    }
    let capacity = samples.checked_mul(2).ok_or_else(bad)?;
    let mut bytes = Vec::with_capacity(capacity);
    for pixel in image.px.as_chunks::<4>().0 {
        let [r, g, b, a] = *pixel;
        let a = if a.is_finite() { a.clamp(0.0, 1.0) } else { 0.0 };
        let encode_channel = |v: f32| {
            let v = if a > 0.0 && v.is_finite() { filmcraft_color::linear_to_srgb(v / a).clamp(0.0, 1.0) } else { 0.0 };
            (v * 65535.0 + 0.5) as u16
        };
        for v in [encode_channel(r), encode_channel(g), encode_channel(b), (a * 65535.0 + 0.5) as u16] {
            bytes.extend_from_slice(&v.to_ne_bytes());
        }
    }
    let mut out = std::io::Cursor::new(Vec::new());
    match format {
        StillFormat::Png => {
            image::ImageEncoder::write_image(image::codecs::png::PngEncoder::new(&mut out), &bytes, w, h, image::ExtendedColorType::Rgba16).map_err(err)?
        }
        StillFormat::Tiff => {
            image::ImageEncoder::write_image(image::codecs::tiff::TiffEncoder::new(&mut out), &bytes, w, h, image::ExtendedColorType::Rgba16).map_err(err)?
        }
        _ => return Err(ExportError::Unsupported("16-bit still format".into())),
    }
    Ok(out.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sixteen_bit_stills_preserve_sub_eight_bit_precision_and_alpha() {
        let img = Image { w: 2, h: 1, px: vec![0.101, 0.203, 0.307, 1.0, 0.101 / 2.0, 0.203 / 2.0, 0.307 / 2.0, 0.5] };
        for format in [StillFormat::Png, StillFormat::Tiff] {
            let bytes = encode(&img, format, 16).unwrap();
            let decoded = image::load_from_memory(&bytes).unwrap();
            assert_eq!(decoded.color(), image::ColorType::Rgba16);
            let px = decoded.to_rgba16();
            let a = px.get_pixel(0, 0).0;
            let b = px.get_pixel(1, 0).0;
            assert_ne!(a[0] % 257, 0, "must not be padded 8-bit values");
            assert_eq!(&a[..3], &b[..3]);
            assert_eq!(b[3], 32768);
        }
    }
    #[test]
    fn jpeg_and_bmp_are_eight_bit_and_invalid_buffers_are_errors() {
        let image = Image::filled(2, 2, [0.2, 0.3, 0.4, 1.0]);
        for format in [StillFormat::Jpeg, StillFormat::Bmp] {
            let bytes = encode(&image, format, 8).unwrap();
            assert_eq!(image::load_from_memory(&bytes).unwrap().color(), image::ColorType::Rgb8);
            assert!(encode(&image, format, 16).is_err());
        }
        assert!(encode(&Image { w: 2, h: 2, px: vec![] }, StillFormat::Png, 16).is_err());
        assert!(encode(&image, StillFormat::Png, 12).is_err());
    }
}
