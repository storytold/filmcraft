//! Uncompressed and PackBits YCbCr TIFFs (TIFF 6.0 §21), as ffmpeg writes from YUV sources. The
//! image crate reads YCbCr only inside JPEG compression, so these are decoded here: 8-bit chunky
//! data units of any subsampling (1, 2 or 4 each way), converted to RGB with the file's
//! YCbCrCoefficients and ReferenceBlackWhite (§20), chroma repeated across each data unit.
//!
//! Every number comes from the file: sizes, strip offsets and counts are checked before use, the
//! picture is capped at [`MAX_PIXELS`], and PackBits output stops at the bytes the image needs.

use filmcraft_dng::tiff::{Tiff, tags};

use super::Raster;
use crate::{MediaError, Result};

/// Largest picture decoded (1 GiB of RGBA), as for animated images.
const MAX_PIXELS: u64 = 1 << 28;

const PHOTOMETRIC_YCBCR: u64 = 6;
const COMPRESSION_NONE: u64 = 1;
const COMPRESSION_PACKBITS: u64 = 32773;
const YCBCR_COEFFICIENTS: u16 = 529;
const YCBCR_SUBSAMPLING: u16 = 530;
const REFERENCE_BLACK_WHITE: u16 = 532;

/// Whether `bytes` is a TIFF whose first image is uncompressed or PackBits YCbCr (the ones this
/// module decodes instead of the image crate).
pub fn sniff(bytes: &[u8]) -> bool {
    let Ok((t, first)) = Tiff::new(bytes) else { return false };
    let Ok((ifd, _)) = t.read_ifd(first, 0) else { return false };
    let num = |tag| ifd.get(tag).and_then(|e| t.uint(e).ok());
    num(tags::PHOTOMETRIC) == Some(PHOTOMETRIC_YCBCR) && matches!(num(tags::COMPRESSION).unwrap_or(COMPRESSION_NONE), COMPRESSION_NONE | COMPRESSION_PACKBITS)
}

/// Decode the first image of a YCbCr TIFF (see [`sniff`]) to RGBA.
pub fn decode(name: &str, bytes: &[u8]) -> Result<Raster> {
    let bad = |what: &str| MediaError::Decode(format!("{name}: YCbCr TIFF: {what}"));
    let unsupported = |what: &str| MediaError::Unsupported(format!("{name}: YCbCr TIFF with {what}"));
    let (t, first) = Tiff::new(bytes).map_err(|_| bad("not a TIFF"))?;
    let (ifd, _) = t.read_ifd(first, 0).map_err(|_| bad("unreadable image directory"))?;
    let uints = |tag: u16| -> Result<Option<Vec<u64>>> { ifd.get(tag).map(|e| t.uints(e).map_err(|_| bad("unreadable field"))).transpose() };
    let one = |tag: u16| -> Result<Option<u64>> { Ok(uints(tag)?.and_then(|v| v.first().copied())) };

    let width = one(tags::IMAGE_WIDTH)?.ok_or_else(|| bad("no ImageWidth"))?;
    let height = one(tags::IMAGE_LENGTH)?.ok_or_else(|| bad("no ImageLength"))?;
    if width == 0 || height == 0 {
        return Err(bad("empty image"));
    }
    if width.saturating_mul(height) > MAX_PIXELS {
        return Err(unsupported("more than 268 million pixels"));
    }
    if one(tags::SAMPLES_PER_PIXEL)?.unwrap_or(1) != 3 {
        return Err(unsupported("other than three samples per pixel"));
    }
    if uints(tags::BITS_PER_SAMPLE)?.unwrap_or_else(|| vec![1]).iter().any(|&b| b != 8) {
        return Err(unsupported("other than 8 bits per sample"));
    }
    if one(tags::PLANAR_CONFIGURATION)?.unwrap_or(1) != 1 {
        return Err(unsupported("separate planes"));
    }
    if ifd.get(tags::TILE_OFFSETS).is_some() {
        return Err(unsupported("tiles"));
    }
    let compression = one(tags::COMPRESSION)?.unwrap_or(COMPRESSION_NONE);
    let sub = uints(YCBCR_SUBSAMPLING)?.unwrap_or_else(|| vec![2, 2]);
    let (sh, sv) = match sub.as_slice() {
        [h, v, ..] if [1, 2, 4].contains(h) && [1, 2, 4].contains(v) => (*h, *v),
        _ => return Err(bad("invalid YCbCrSubSampling")),
    };
    // §21: the data units cover the image rounded up to whole units.
    let (units_x, units_y) = (width.div_ceil(sh), height.div_ceil(sv));
    let unit_len = sh * sv + 2;
    let need = usize::try_from(units_x.saturating_mul(units_y).saturating_mul(unit_len)).map_err(|_| bad("too large"))?;

    let offsets = uints(tags::STRIP_OFFSETS)?.ok_or_else(|| bad("no StripOffsets"))?;
    let counts = uints(tags::STRIP_BYTE_COUNTS)?.ok_or_else(|| bad("no StripByteCounts"))?;
    let mut units: Vec<u8> = Vec::with_capacity(need);
    for (&off, &len) in offsets.iter().zip(&counts) {
        if units.len() >= need {
            break;
        }
        let strip = usize::try_from(off)
            .ok()
            .zip(usize::try_from(len).ok())
            .and_then(|(o, l)| bytes.get(o..o.checked_add(l)?))
            .ok_or_else(|| bad("strip outside the file"))?;
        match compression {
            COMPRESSION_NONE => units.extend_from_slice(strip.get(..need - units.len()).unwrap_or(strip)),
            _ => unpackbits(strip, &mut units, need),
        }
    }
    if units.len() < need {
        return Err(bad("image data shorter than the image"));
    }

    let coeffs = match ifd.get(YCBCR_COEFFICIENTS) {
        Some(e) => t.floats(e).map_err(|_| bad("unreadable YCbCrCoefficients"))?,
        None => vec![0.299, 0.587, 0.114],
    };
    let [lr, lg, lb] = coeffs.get(..3).and_then(|c| <[f64; 3]>::try_from(c).ok()).ok_or_else(|| bad("invalid YCbCrCoefficients"))?;
    if lg.abs() < 1e-6 {
        return Err(bad("invalid YCbCrCoefficients"));
    }
    let rbw = match ifd.get(REFERENCE_BLACK_WHITE) {
        Some(e) => t.floats(e).map_err(|_| bad("unreadable ReferenceBlackWhite"))?,
        None => vec![0.0, 255.0, 128.0, 255.0, 128.0, 255.0],
    };
    let [yb, yw, cbb, cbw, crb, crw] = rbw.get(..6).and_then(|c| <[f64; 6]>::try_from(c).ok()).ok_or_else(|| bad("invalid ReferenceBlackWhite"))?;
    if (yw - yb).abs() < 1e-6 || (cbw - cbb).abs() < 1e-6 || (crw - crb).abs() < 1e-6 {
        return Err(bad("invalid ReferenceBlackWhite"));
    }
    // §20: luma to 0–255, chroma to ±127 about its reference black; §21: back to R'G'B'.
    let to_rgb = |y: u8, cb: u8, cr: u8| -> [u8; 3] {
        let y = (f64::from(y) - yb) * 255.0 / (yw - yb);
        let cb = (f64::from(cb) - cbb) * 127.0 / (cbw - cbb);
        let cr = (f64::from(cr) - crb) * 127.0 / (crw - crb);
        let r = cr * (2.0 - 2.0 * lr) + y;
        let b = cb * (2.0 - 2.0 * lb) + y;
        let g = (y - lb * b - lr * r) / lg;
        [r, g, b].map(|v| v.round().clamp(0.0, 255.0) as u8)
    };

    let (w, h) = (width as usize, height as usize);
    let (sh, sv, units_x) = (sh as usize, sv as usize, units_x as usize);
    let mut rgba = vec![255u8; w * h * 4];
    for (i, unit) in units.chunks_exact(sh * sv + 2).take(need / (sh * sv + 2)).enumerate() {
        let (ux, uy) = (i % units_x, i / units_x);
        let (cb, cr) = (unit[sh * sv], unit[sh * sv + 1]);
        for (k, &y) in unit[..sh * sv].iter().enumerate() {
            let (x, row) = (ux * sh + k % sh, uy * sv + k / sh);
            if x < w && row < h {
                let at = (row * w + x) * 4;
                rgba[at..at + 3].copy_from_slice(&to_rgb(y, cb, cr));
            }
        }
    }
    Ok(Raster { width: width as u32, height: height as u32, rgba, has_alpha: false })
}

/// PackBits (TIFF 6.0 §9) from `src` appended to `out`, stopping at `limit` bytes of output or at
/// a run that the data ends inside.
fn unpackbits(src: &[u8], out: &mut Vec<u8>, limit: usize) {
    let mut p = 0;
    while out.len() < limit {
        let Some(&n) = src.get(p) else { return };
        let n = n as i8;
        p += 1;
        if n >= 0 {
            let len = n as usize + 1;
            let Some(lit) = src.get(p..p + len) else { return };
            out.extend_from_slice(&lit[..len.min(limit - out.len())]);
            p += len;
        } else if n != -128 {
            let Some(&b) = src.get(p) else { return };
            let len = (1 - n as isize) as usize;
            out.extend(std::iter::repeat_n(b, len.min(limit - out.len())));
            p += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packbits_runs_literals_and_noops() {
        let mut out = Vec::new();
        // literal 2, a no-op, a run of 3, then a literal cut short by the end of the data
        unpackbits(&[1, 10, 11, 0x80, 0xFE, 7, 4, 1], &mut out, 100);
        assert_eq!(out, [10, 11, 7, 7, 7]);
        let mut capped = Vec::new();
        unpackbits(&[0xFE, 7, 0xFE, 8], &mut capped, 4);
        assert_eq!(capped, [7, 7, 7, 8]);
    }
}
