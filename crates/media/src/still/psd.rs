//! Photoshop documents (`.psd`) as their flattened composite, read from Adobe's public "Adobe
//! Photoshop File Formats Specification": a 26-byte header, the colour mode data, image resources
//! and layer and mask sections (skipped by their lengths), then the image data section, which holds
//! the document merged the way Photoshop last saved it. That is what Premiere's "Merge All Layers"
//! import shows; the layers themselves are not read. The composite is planar, one channel after the
//! other, uncompressed or PackBits run-length coded per row. Grayscale and RGB at 8 or 16 bits per
//! channel are decoded (16-bit kept at 8, as for DPX); the first extra channel is the composite's
//! transparency only when the layer count is negative, which is how the format says so (otherwise
//! extra channels are saved selections). Photoshop stores a transparent composite blended over
//! white, so its colour is unblended back to straight alpha; the specification doesn't say so, it is
//! what psd-tools and ImageMagick do, and Photoshop-written files bear it out (their transparent
//! pixels are white, and a semi-transparent one is its layer's colour blended over white). Documents
//! saved without "Maximize Compatibility" (version info resource 1057 says the image data is not the
//! real composite), PSB, bitmap, indexed, CMYK, multichannel, duotone, Lab, 32-bit and ZIP-compressed
//! documents are refused.

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
    b.starts_with(b"8BPS")
}

fn err(name: &str, what: &str) -> MediaError {
    MediaError::Decode(format!("{name}: PSD: {what}"))
}

fn unsupported(name: &str, what: &str) -> MediaError {
    MediaError::Unsupported(format!("{name}: PSD: {what} documents are not supported"))
}

fn be16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(*b.get(at..at.checked_add(2)?)?.first_chunk()?))
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(*b.get(at..at.checked_add(4)?)?.first_chunk()?))
}

/// The section that starts at `pos` (a 4-byte length, then the data) and the position after it.
fn section<'a>(name: &str, b: &'a [u8], pos: usize) -> Result<(&'a [u8], usize)> {
    let len = be32(b, pos).ok_or_else(|| err(name, "truncated file"))? as usize;
    let start = pos + 4;
    let end = start.checked_add(len).ok_or_else(|| err(name, "truncated file"))?;
    let data = b.get(start..end).ok_or_else(|| err(name, "truncated file"))?;
    Ok((data, end))
}

/// What the version info resource (1057) says about the image data: `Some(false)` when it is not
/// the real composite. Resources are `8BIM`, an id, an even-padded Pascal name, then a length and
/// the even-padded data.
fn real_merged_data(s: &[u8]) -> Option<bool> {
    let mut p = 0usize;
    // each resource takes at least 12 bytes, so this walks the section once
    while let Some(head) = s.get(p..p.checked_add(7)?) {
        if !head.starts_with(b"8BIM") {
            return None;
        }
        let id = be16(head, 4)?;
        let name = (usize::from(*head.get(6)?) + 2) & !1;
        let len_at = p.checked_add(6)?.checked_add(name)?;
        let len = be32(s, len_at)? as usize;
        let data = len_at + 4;
        if id == 1057 {
            return s.get(data + 4).map(|&v| v != 0);
        }
        p = data.checked_add(len)?.checked_add(len & 1)?;
    }
    None
}

/// Whether the layer and mask section has a negative layer count, which marks the first extra
/// channel of the composite as its transparency. 16-bit documents leave the layer info empty and
/// keep the layers in an `Lr16` (or `Lr32`) tagged block after the global layer mask.
fn merged_alpha(s: &[u8]) -> bool {
    let negative = |at: usize| be16(s, at).is_some_and(|n| (n as i16) < 0);
    let Some(info) = be32(s, 0) else { return false };
    if info >= 2 {
        return negative(4);
    }
    let Some(mut p) = (info as usize).checked_add(4) else { return false };
    let Some(mask) = be32(s, p) else { return false };
    let Some(next) = p.checked_add(4).and_then(|q| q.checked_add(mask as usize)) else { return false };
    p = next;
    // each block takes at least 12 bytes, so this walks the section once
    while let Some(head) = p.checked_add(12).and_then(|end| s.get(p..end)) {
        let (sig, key) = head.split_at(4);
        if sig != b"8BIM" && sig != b"8B64" {
            return false;
        }
        if key.starts_with(b"Lr16") || key.starts_with(b"Lr32") {
            return negative(p + 12);
        }
        // global tagged blocks pad their data to a multiple of 4 bytes (the specification says even;
        // 4 is how psd-tools reads them)
        let Some(next) = be32(head, 8).and_then(|len| (p + 12).checked_add((len as usize).checked_add(3)? & !3)) else { return false };
        p = next;
    }
    false
}

/// Expand one PackBits-coded row into `out`, which it must fill exactly.
fn unpack(name: &str, src: &[u8], out: &mut [u8]) -> Result<()> {
    let (mut i, mut o) = (0, 0);
    while o < out.len() {
        let n = *src.get(i).ok_or_else(|| err(name, "truncated run-length row"))? as i8;
        i += 1;
        if n >= 0 {
            let k = n as usize + 1;
            let lit = src.get(i..i + k).ok_or_else(|| err(name, "truncated run-length row"))?;
            out.get_mut(o..o + k).ok_or_else(|| err(name, "run-length row longer than the picture"))?.copy_from_slice(lit);
            i += k;
            o += k;
        } else if n != -128 {
            let k = usize::from(n.unsigned_abs()) + 1;
            let v = *src.get(i).ok_or_else(|| err(name, "truncated run-length row"))?;
            out.get_mut(o..o + k).ok_or_else(|| err(name, "run-length row longer than the picture"))?.fill(v);
            i += 1;
            o += k;
        }
    }
    Ok(())
}

/// Decode the composite image of a Photoshop document to straight-alpha RGBA8.
pub fn decode(name: &str, b: &[u8]) -> Result<Raster> {
    if !sniff(b) {
        return Err(err(name, "not a Photoshop document"));
    }
    let h = b.get(..26).ok_or_else(|| err(name, "truncated header"))?;
    let field16 = |at| be16(h, at).unwrap_or(0);
    let field32 = |at| be32(h, at).unwrap_or(0);
    match field16(4) {
        1 => {}
        2 => return Err(unsupported(name, "large document format (PSB)")),
        v => return Err(err(name, &format!("unknown version {v}"))),
    }
    let (channels, height, width, depth, mode) = (usize::from(field16(12)), field32(14), field32(18), field16(22), field16(24));
    let colours = match mode {
        1 => 1,
        3 => 3,
        0 => return Err(unsupported(name, "bitmap")),
        2 => return Err(unsupported(name, "indexed colour")),
        4 => return Err(unsupported(name, "CMYK")),
        7 => return Err(unsupported(name, "multichannel")),
        8 => return Err(unsupported(name, "duotone")),
        9 => return Err(unsupported(name, "Lab")),
        m => return Err(err(name, &format!("unknown colour mode {m}"))),
    };
    let bytes_per_sample = match depth {
        8 => 1,
        16 => 2,
        1 | 32 => return Err(unsupported(name, &format!("{depth}-bit"))),
        d => return Err(err(name, &format!("unknown bit depth {d}"))),
    };
    if channels < colours || channels > MAX_CHANNELS {
        return Err(err(name, &format!("{channels} channels")));
    }
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return Err(err(name, "picture size out of range"));
    }
    let (w, ht) = (width as usize, height as usize);
    let pixels = w.checked_mul(ht).filter(|&n| n <= MAX_PIXELS).ok_or_else(|| err(name, "picture too large"))?;

    let (_, pos) = section(name, b, 26)?;
    let (resources, pos) = section(name, b, pos)?;
    if real_merged_data(resources) == Some(false) {
        return Err(MediaError::Unsupported(format!(
            "{name}: PSD: saved without Maximize Compatibility, so the file holds no composite image (resave it with that option on)"
        )));
    }
    let (layers, pos) = section(name, b, pos)?;
    let alpha = channels > colours && merged_alpha(layers);
    let used = colours + usize::from(alpha);
    let compression = be16(b, pos).ok_or_else(|| err(name, "truncated file"))?;
    let data = b.get(pos + 2..).unwrap_or_default();
    let row_bytes = w * bytes_per_sample;
    let needed = (row_bytes as u64) * (ht as u64) * (used as u64);
    // a PackBits pair expands to at most 128 bytes: no shorter file can hold the picture
    let ratio = match compression {
        0 => 1,
        1 => 64,
        2 | 3 => return Err(unsupported(name, "ZIP-compressed")),
        c => return Err(err(name, &format!("unknown compression {c}"))),
    };
    if (data.len() as u64).saturating_mul(ratio) < needed {
        return Err(err(name, "truncated image data"));
    }

    // where each row of the used channels starts, and how long it is
    let mut rows = Vec::with_capacity(ht * used);
    if compression == 0 {
        rows.extend((0..ht * used).map(|i| (i * row_bytes, row_bytes)));
    } else {
        let mut at = ht * channels * 2;
        for i in 0..ht * used {
            let len = usize::from(be16(data, i * 2).ok_or_else(|| err(name, "truncated row lengths"))?);
            rows.push((at, len));
            at += len;
        }
    }

    let max: u32 = if bytes_per_sample == 1 { 255 } else { 65_535 };
    let sample = |s: &[u8]| match *s {
        [v] => u16::from(v),
        [hi, lo] => u16::from_be_bytes([hi, lo]),
        _ => 0,
    };
    let to8 = |v: u32| ((u64::from(v) * 255 + u64::from(max / 2)) / u64::from(max)) as u8;
    let mut row = vec![0u8; row_bytes];
    let read = |i: usize, row: &mut [u8]| -> Result<()> {
        let &(at, len) = rows.get(i).ok_or_else(|| err(name, "truncated image data"))?;
        let src = data.get(at..at + len).ok_or_else(|| err(name, "truncated image data"))?;
        if compression == 0 {
            row.copy_from_slice(src);
            Ok(())
        } else {
            unpack(name, src, row)
        }
    };

    // the transparency first, at the file's depth (16 bits at most): the colour channels are unblended
    // from white with it
    let mut opacity = Vec::new();
    if alpha {
        opacity.reserve(pixels);
        for y in 0..ht {
            read(colours * ht + y, &mut row)?;
            opacity.extend(row.chunks_exact(bytes_per_sample).map(sample));
        }
    }
    let mut rgba = vec![255u8; pixels * 4];
    for c in 0..colours {
        for y in 0..ht {
            read(c * ht + y, &mut row)?;
            let out = rgba.get_mut(y * w * 4..(y + 1) * w * 4).ok_or_else(|| err(name, "picture too large"))?;
            for (x, (px, s)) in out.as_chunks_mut::<4>().0.iter_mut().zip(row.chunks_exact(bytes_per_sample)).enumerate() {
                let mut v = u32::from(sample(s));
                if let Some(&a) = opacity.get(y * w + x) {
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
    }
    let has_alpha = alpha && rgba.as_chunks::<4>().0.iter().any(|p| p[3] < 255);
    Ok(Raster { width, height, rgba, has_alpha })
}

#[cfg(test)]
mod tests;
