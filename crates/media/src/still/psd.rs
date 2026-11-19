//! Photoshop documents (`.psd`) as their flattened composite, decoded via `craft_psd::merged::Merged`.
//! Grayscale and RGB at 8 or 16 bits per channel are decoded (16-bit kept at 8, as for DPX); the
//! first extra channel is the composite's transparency only when the layer section marks it
//! (negative layer count, `Lr16`/`Lr32`, or `Mt*` block; otherwise extra channels are saved
//! selections). Photoshop stores a transparent composite blended over white, so its colour is
//! unblended back to straight alpha. Documents saved without "Maximize Compatibility" (version info
//! resource 1057 says the image data is not the real composite), PSB, bitmap, indexed, CMYK,
//! multichannel, duotone, Lab, 32-bit and ZIP-compressed documents are refused.

use craft_psd::merged::Merged;
use craft_psd::resources::ids;
use craft_psd::{ColorMode, Header, Version};

use super::Raster;
use crate::{MediaError, Result};

/// Largest side the format allows.
const MAX_SIDE: u32 = 30_000;
/// Largest picture we decode (each pixel takes 4 bytes as RGBA8, plus 2 for the transparency).
const MAX_PIXELS: usize = 1 << 28;
/// Most channels the format allows.
const MAX_CHANNELS: usize = 56;

/// Whether `b` starts like a Photoshop document.
pub fn sniff(b: &[u8]) -> bool {
    craft_psd::head::sniff(b)
}

#[cfg(test)]
fn unpack(name: &str, src: &[u8], out: &mut [u8]) -> Result<()> {
    craft_psd::compression::packbits::decode_to_slice(src, out).map_err(|e| err(name, &e.to_string()))
}

fn err(name: &str, what: &str) -> MediaError {
    MediaError::Decode(format!("{name}: PSD: {what}"))
}

fn unsupported(name: &str, what: &str) -> MediaError {
    MediaError::Unsupported(format!("{name}: PSD: {what} documents are not supported"))
}

fn validate_header(name: &str, h: &Header) -> Result<(usize, usize)> {
    match h.version {
        Version::Psd => {}
        Version::Psb => return Err(unsupported(name, "large document format (PSB)")),
    }
    let colours = match h.color_mode {
        ColorMode::Grayscale => 1,
        ColorMode::Rgb => 3,
        ColorMode::Bitmap => return Err(unsupported(name, "bitmap")),
        ColorMode::Indexed => return Err(unsupported(name, "indexed colour")),
        ColorMode::Cmyk => return Err(unsupported(name, "CMYK")),
        ColorMode::Multichannel => return Err(unsupported(name, "multichannel")),
        ColorMode::Duotone => return Err(unsupported(name, "duotone")),
        ColorMode::Lab => return Err(unsupported(name, "Lab")),
        ColorMode::Unknown(m) => return Err(err(name, &format!("unknown colour mode {m}"))),
    };
    let bytes_per_sample = match h.depth {
        8 => 1,
        16 => 2,
        1 | 32 => return Err(unsupported(name, &format!("{}-bit", h.depth))),
        d => return Err(err(name, &format!("unknown bit depth {d}"))),
    };
    let channels = usize::from(h.channels);
    if channels < colours || channels > MAX_CHANNELS {
        return Err(err(name, &format!("{channels} channels")));
    }
    if h.width == 0 || h.height == 0 || h.width > MAX_SIDE || h.height > MAX_SIDE {
        return Err(err(name, "picture size out of range"));
    }
    Ok((colours, bytes_per_sample))
}

fn planes_to_rgba8(planes: &[Vec<u8>], colours: usize, bytes_per_sample: usize, alpha: bool, pixels: usize) -> (Vec<u8>, bool) {
    let max: u32 = if bytes_per_sample == 1 { 255 } else { 65_535 };
    let sample = |s: &[u8]| match *s {
        [v] => u16::from(v),
        [hi, lo] => u16::from_be_bytes([hi, lo]),
        _ => 0,
    };
    let to8 = |v: u32| ((u64::from(v) * 255 + u64::from(max / 2)) / u64::from(max)) as u8;
    let opacity: Vec<u16> = if alpha { planes[colours].chunks_exact(bytes_per_sample).map(sample).collect() } else { Vec::new() };
    let mut rgba = vec![255u8; pixels * 4];
    for (c, plane) in planes.iter().take(colours).enumerate() {
        for (i, (px, s)) in rgba.as_chunks_mut::<4>().0.iter_mut().zip(plane.chunks_exact(bytes_per_sample)).enumerate() {
            let mut v = u32::from(sample(s));
            if let Some(&a) = opacity.get(i) {
                let a = u32::from(a);
                // stored = colour * a + white * (1 - a), rounded
                if a > 0 && a < max {
                    let (v64, a64, m64) = (u64::from(v), u64::from(a), u64::from(max));
                    v = ((v64 + a64).saturating_sub(m64) * m64 + a64 / 2).checked_div(a64).unwrap_or(0).min(m64) as u32;
                }
                px[3] = to8(a);
            }
            if colours == 1 {
                px[..3].fill(to8(v));
            } else if let Some(d) = px.get_mut(c) {
                *d = to8(v);
            }
        }
    }
    let has_alpha = alpha && rgba.as_chunks::<4>().0.iter().any(|p| p[3] < 255);
    (rgba, has_alpha)
}

/// Decode the composite image of a Photoshop document to straight-alpha RGBA8.
pub fn decode(name: &str, b: &[u8]) -> Result<Raster> {
    if !sniff(b) {
        return Err(err(name, "not a Photoshop document"));
    }
    let header = Header::from_bytes(b).map_err(|e| err(name, &e.to_string()))?;
    let (colours, bytes_per_sample) = validate_header(name, &header)?;
    let merged = Merged::open(b, MAX_PIXELS as u64).map_err(|e| err(name, &e.to_string()))?;
    if merged.head.resource(ids::VERSION_INFO).and_then(|r| r.data.get(4)).is_some_and(|&v| v == 0) {
        return Err(MediaError::Unsupported(format!(
            "{name}: PSD: saved without Maximize Compatibility, so the file holds no composite image (resave it with that option on)"
        )));
    }
    match merged.compression {
        0 | 1 => {}
        2 | 3 => return Err(unsupported(name, "ZIP-compressed")),
        c => return Err(err(name, &format!("unknown compression {c}"))),
    }
    let h = &merged.head.header;
    let alpha = usize::from(h.channels) > colours && merged.transparency;
    let used = colours + usize::from(alpha);
    let planes = merged.read_planes(used).map_err(|e| err(name, &e.to_string()))?;
    let pixels = (h.width as usize) * (h.height as usize);
    let (rgba, has_alpha) = planes_to_rgba8(&planes, colours, bytes_per_sample, alpha, pixels);
    Ok(Raster { width: h.width, height: h.height, rgba, has_alpha })
}

#[cfg(test)]
mod tests;
