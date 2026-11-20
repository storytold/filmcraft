//! Photoshop documents (`.psd`) as their flattened composite, decoded via `craft_psd::merged::Merged`.
//! Grayscale and RGB at 8 or 16 bits per channel are decoded (16-bit kept at 8, as for DPX); the
//! first extra channel is the composite's transparency only when the layer section marks it
//! (negative layer count, `Lr16`/`Lr32`, or `Mt*` block; otherwise extra channels are saved
//! selections). Photoshop stores a transparent composite blended over white, so its colour is
//! unblended back to straight alpha. Documents saved without "Maximize Compatibility" (version info
//! resource 1057 says the image data is not the real composite), PSB, bitmap, indexed, CMYK,
//! multichannel, duotone, Lab, 32-bit and ZIP-compressed documents are refused.

use craft_psd::merged::{EncodedRows, Merged};
use craft_psd::resources::ids;
use craft_psd::{ColorMode, Header, Version};

use image::metadata::Orientation;

use super::Raster;
use crate::{MediaError, Result};

/// Largest side the format allows.
const MAX_SIDE: u32 = 30_000;
/// Largest picture we decode (4 bytes per RGBA8 pixel, plus bounded row buffers).
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

fn rows_to_rgba8(name: &str, rows: &EncodedRows<'_>, h: &Header, colours: usize, alpha: bool) -> Result<(Vec<u8>, bool)> {
    let bytes_per_sample = usize::from(h.depth / 8);
    let width = h.width as usize;
    let pixels = width.checked_mul(h.height as usize).ok_or_else(|| err(name, "picture size overflow"))?;
    let len = pixels.checked_mul(4).ok_or_else(|| err(name, "RGBA size overflow"))?;
    let used = colours + usize::from(alpha);
    // At most four cursors and four decoded rows. Do not keep full decoded planes or a
    // full-resolution alpha copy beside the RGBA result.
    let mut cursors = Vec::new();
    let mut buffers = Vec::new();
    for c in 0..used {
        cursors.push(rows.channel(c).map_err(|e| err(name, &e.to_string()))?);
        let mut row = Vec::new();
        row.try_reserve_exact(rows.row_bytes()).map_err(|_| err(name, "cannot allocate decoded row"))?;
        row.resize(rows.row_bytes(), 0);
        buffers.push(row);
    }
    let mut rgba = Vec::new();
    rgba.try_reserve_exact(len).map_err(|_| err(name, "cannot allocate RGBA image"))?;
    rgba.resize(len, 255);
    let max: u32 = if bytes_per_sample == 1 { 255 } else { 65_535 };
    let sample = |s: &[u8]| match *s {
        [v] => u16::from(v),
        [hi, lo] => u16::from_be_bytes([hi, lo]),
        _ => 0,
    };
    let to8 = |v: u32| ((u64::from(v) * 255 + u64::from(max / 2)) / u64::from(max)) as u8;
    let mut has_alpha = false;
    for output in rgba.chunks_exact_mut(width * 4) {
        for (cursor, row) in cursors.iter_mut().zip(&mut buffers) {
            cursor
                .next_row()
                .map_err(|e| err(name, &e.to_string()))?
                .ok_or_else(|| err(name, "missing composite row"))?
                .decode_to_slice(row)
                .map_err(|e| err(name, &e.to_string()))?;
        }
        for (c, row) in buffers.iter().take(colours).enumerate() {
            for (i, (px, s)) in output.as_chunks_mut::<4>().0.iter_mut().zip(row.chunks_exact(bytes_per_sample)).enumerate() {
                let mut v = u32::from(sample(s));
                if alpha {
                    let at = i * bytes_per_sample;
                    // The selected alpha buffer has the same validated width and depth as
                    // each colour row. Keep native 16-bit alpha until unblending is finished.
                    let a = u32::from(sample(&buffers[colours][at..at + bytes_per_sample]));
                    // stored = colour * a + white * (1 - a), rounded
                    if a > 0 && a < max {
                        let (v64, a64, m64) = (u64::from(v), u64::from(a), u64::from(max));
                        v = ((v64 + a64).saturating_sub(m64) * m64 + a64 / 2).checked_div(a64).unwrap_or(0).min(m64) as u32;
                    }
                    px[3] = to8(a);
                    has_alpha |= px[3] < 255;
                }
                if colours == 1 {
                    px[..3].fill(to8(v));
                } else if let Some(d) = px.get_mut(c) {
                    *d = to8(v);
                }
            }
        }
    }
    Ok((rgba, has_alpha))
}

/// Decode the composite image of a Photoshop document to straight-alpha RGBA8.
/// The size [`decode`] reports for this file, from its header alone. `None` when the
/// header does not parse or the picture is past the pixel ceiling [`decode`] enforces.
pub fn geometry(b: &[u8]) -> Option<(u32, u32, Orientation)> {
    let header = Header::from_bytes(b).ok()?;
    if u64::from(header.width) * u64::from(header.height) > MAX_PIXELS as u64 {
        return None;
    }
    Some((header.width, header.height, Orientation::NoTransforms))
}

pub fn decode(name: &str, b: &[u8]) -> Result<Raster> {
    if !sniff(b) {
        return Err(err(name, "not a Photoshop document"));
    }
    let header = Header::from_bytes(b).map_err(|e| err(name, &e.to_string()))?;
    let (colours, _) = validate_header(name, &header)?;
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
    // Validate selected encoded payload extents before allocating the final image. The app
    // supports at most MAX_SIDE 16-bit samples per row; the shared view allocates no planes.
    let rows = merged.encoded_rows(used, MAX_SIDE as usize * 2).map_err(|e| err(name, &e.to_string()))?;
    let (rgba, has_alpha) = rows_to_rgba8(name, &rows, h, colours, alpha)?;
    Ok(Raster { width: h.width, height: h.height, rgba, has_alpha, orientation: Orientation::NoTransforms })
}

#[cfg(test)]
mod tests;
