//! The raw sample reading of PR #1068, kept unchanged as the reference the faster reader is
//! compared against (tests and benchmarks only).

#[cfg(feature = "threads")]
use rayon::prelude::*;

use super::ljpeg;
use crate::raw::{MAX_SAMPLES, RawImage, RawLayout, layout};
use crate::tiff::{Ifd, Tiff, tags};
use crate::{Error, Result};

fn uint_or(t: &Tiff, ifd: &Ifd, tag: u16, default: u64) -> Result<u64> {
    match ifd.get(tag) {
        Some(e) => t.uint(e),
        None => Ok(default),
    }
}

/// One strip or tile: where it sits in the image and its bytes.
struct Chunk<'a> {
    x: usize,
    y: usize,
    /// Pixel size of the chunk as coded (tiles are whole even at the image edge).
    w: usize,
    h: usize,
    bytes: &'a [u8],
}

fn chunks<'a>(t: &Tiff<'a>, ifd: &Ifd, l: &RawLayout) -> Result<Vec<Chunk<'a>>> {
    let (offs, counts, tw, th) = if let (Some(o), Some(c)) = (ifd.get(tags::TILE_OFFSETS), ifd.get(tags::TILE_BYTE_COUNTS)) {
        let tw = uint_or(t, ifd, tags::TILE_WIDTH, 0)? as usize;
        let th = uint_or(t, ifd, tags::TILE_LENGTH, 0)? as usize;
        if tw == 0 || th == 0 || tw > 65_535 || th > 65_535 {
            return Err(Error::Malformed("tile size"));
        }
        (t.uints(o)?, t.uints(c)?, tw, th)
    } else if let (Some(o), Some(c)) = (ifd.get(tags::STRIP_OFFSETS), ifd.get(tags::STRIP_BYTE_COUNTS)) {
        let rows = (uint_or(t, ifd, tags::ROWS_PER_STRIP, l.height as u64)? as usize).clamp(1, l.height);
        (t.uints(o)?, t.uints(c)?, l.width, rows)
    } else {
        return Err(Error::Malformed("no strips or tiles"));
    };
    let across = l.width.div_ceil(tw);
    let down = l.height.div_ceil(th);
    let n = across * down;
    if offs.len() < n || counts.len() < n {
        return Err(Error::Malformed("fewer strips or tiles than the image needs"));
    }
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let (x, y) = ((i % across) * tw, (i / across) * th);
        let start = usize::try_from(offs[i]).map_err(|_| Error::Truncated)?;
        let len = usize::try_from(counts[i]).map_err(|_| Error::Truncated)?;
        let end = start.checked_add(len).ok_or(Error::Truncated)?;
        let bytes = t.data.get(start..end).ok_or(Error::Truncated)?;
        // strips end at the image; tiles are coded whole
        let h = if ifd.get(tags::TILE_OFFSETS).is_some() { th } else { th.min(l.height - y) };
        out.push(Chunk { x, y, w: tw, h, bytes });
    }
    Ok(out)
}

/// Unpack `w` × `h` × `spp` uncompressed samples of `bits` bits: 8 and 16-bit samples as bytes /
/// words in the file's byte order, other depths packed most significant bit first with each row
/// starting on a byte boundary (TIFF 6.0 §7, DNG BitsPerSample).
pub fn unpack(bytes: &[u8], w: usize, h: usize, spp: usize, bits: u32, big_endian: bool) -> Result<Vec<u16>> {
    let per_row = w * spp;
    let row_bytes = (per_row * bits as usize).div_ceil(8);
    let need = row_bytes.checked_mul(h).ok_or(Error::TooLarge)?;
    if bytes.len() < need {
        return Err(Error::Truncated);
    }
    let mut out = Vec::with_capacity(per_row * h);
    for row in bytes.chunks_exact(row_bytes.max(1)).take(h) {
        match bits {
            8 => out.extend(row.iter().map(|&b| u16::from(b))),
            16 => out.extend(row.as_chunks::<2>().0.iter().map(|&b| if big_endian { u16::from_be_bytes(b) } else { u16::from_le_bytes(b) })),
            _ => {
                let mut acc: u32 = 0;
                let mut n: u32 = 0;
                let mut it = row.iter();
                for _ in 0..per_row {
                    while n < bits {
                        acc = (acc << 8) | u32::from(*it.next().ok_or(Error::Truncated)?);
                        n += 8;
                    }
                    n -= bits;
                    out.push(((acc >> n) & ((1 << bits) - 1)) as u16);
                    acc &= (1 << n) - 1;
                }
            }
        }
    }
    Ok(out)
}

/// Decode one chunk into `w` × `h` × `spp` samples.
fn decode_chunk(c: &Chunk, l: &RawLayout, big_endian: bool) -> Result<Vec<u16>> {
    let need = c.w * c.h * l.samples_per_pixel;
    match l.compression {
        1 => unpack(c.bytes, c.w, c.h, l.samples_per_pixel, l.bits, big_endian),
        _ => {
            // the JPEG's samples fill the chunk in raster order (its own width × components may
            // differ from the chunk's, e.g. two components of half the width)
            let d = ljpeg::decode(c.bytes, need.saturating_add(65_536 * 4))?;
            if d.samples.len() < need {
                return Err(Error::Malformed("lossless JPEG smaller than its tile or strip"));
            }
            let mut s = d.samples;
            s.truncate(need);
            Ok(s)
        }
    }
}

/// Read the raw image of `ifd`.
pub fn read(t: &Tiff, ifd: &Ifd) -> Result<RawImage> {
    let l = layout(t, ifd)?;
    let spp = l.samples_per_pixel;
    let total = l.width.checked_mul(l.height).and_then(|v| v.checked_mul(spp)).ok_or(Error::TooLarge)?;
    if total > MAX_SAMPLES {
        return Err(Error::TooLarge);
    }
    let chunks = chunks(t, ifd, &l)?;
    // every sample takes at least one bit of file (a Huffman code is at least one bit long)
    let coded: usize = chunks.iter().map(|c| c.bytes.len()).fold(0usize, usize::saturating_add);
    if total / 8 > coded {
        return Err(Error::Truncated);
    }
    for c in &chunks {
        let n = c.w.checked_mul(c.h).and_then(|v| v.checked_mul(spp)).ok_or(Error::TooLarge)?;
        if n > MAX_SAMPLES {
            return Err(Error::TooLarge);
        }
        if n / 8 > c.bytes.len() {
            return Err(Error::Truncated);
        }
    }
    #[cfg(feature = "threads")]
    let decoded: Vec<Result<Vec<u16>>> = chunks.par_iter().map(|c| decode_chunk(c, &l, t.big_endian)).collect();
    #[cfg(not(feature = "threads"))]
    let decoded: Vec<Result<Vec<u16>>> = chunks.iter().map(|c| decode_chunk(c, &l, t.big_endian)).collect();
    let mut data = vec![0u16; total];
    let row_len = l.width * spp;
    for (c, d) in chunks.iter().zip(decoded) {
        let d = d?;
        let cw = c.w.min(l.width - c.x);
        let ch = c.h.min(l.height - c.y);
        for r in 0..ch {
            let src = d.get(r * c.w * spp..(r * c.w + cw) * spp).ok_or(Error::Truncated)?;
            let at = (c.y + r) * row_len + c.x * spp;
            data.get_mut(at..at + cw * spp).ok_or(Error::Truncated)?.copy_from_slice(src);
        }
    }
    Ok(RawImage { width: l.width, height: l.height, samples_per_pixel: spp, bits: l.bits, data })
}
