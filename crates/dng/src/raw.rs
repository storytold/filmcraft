//! The raw image data of a DNG: finding the main raw IFD, and reading its strips or tiles
//! (uncompressed or lossless JPEG) into one array of 16-bit samples.

#[cfg(feature = "threads")]
use rayon::prelude::*;

use crate::tiff::{Ifd, Tiff, tags};
use crate::{Error, Result, ljpeg};

/// Largest raw image decoded, in samples (width × height × samples per pixel).
pub const MAX_SAMPLES: usize = 1 << 27;

/// Photometric interpretations of DNG raw data.
pub const PHOTOMETRIC_CFA: u64 = 32803;
pub const PHOTOMETRIC_LINEAR_RAW: u64 = 34892;

/// The layout of a raw IFD's image data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawLayout {
    pub width: usize,
    pub height: usize,
    pub samples_per_pixel: usize,
    pub bits: u32,
    /// 1 uncompressed, 7 lossless JPEG, 34892 lossy JPEG.
    pub compression: u64,
    pub photometric: u64,
}

/// Raw samples: `width` × `height` pixels of `samples_per_pixel` interleaved 16-bit samples.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawImage {
    pub width: usize,
    pub height: usize,
    pub samples_per_pixel: usize,
    pub bits: u32,
    pub data: Vec<u16>,
}

fn uint_or(t: &Tiff, ifd: &Ifd, tag: u16, default: u64) -> Result<u64> {
    match ifd.get(tag) {
        Some(e) => t.uint(e),
        None => Ok(default),
    }
}

/// The main raw image IFD: the largest full-resolution (NewSubFileType 0) IFD whose photometric
/// interpretation is CFA or LinearRaw.
pub fn find_raw_ifd<'i>(t: &Tiff, ifds: &'i [Ifd]) -> Result<&'i Ifd> {
    let mut best: Option<(&Ifd, u64)> = None;
    for ifd in ifds {
        let Ok(kind) = uint_or(t, ifd, tags::NEW_SUBFILE_TYPE, 0) else { continue };
        let Ok(photometric) = uint_or(t, ifd, tags::PHOTOMETRIC, 0) else { continue };
        if kind != 0 || !matches!(photometric, PHOTOMETRIC_CFA | PHOTOMETRIC_LINEAR_RAW) {
            continue;
        }
        let w = uint_or(t, ifd, tags::IMAGE_WIDTH, 0).unwrap_or(0);
        let h = uint_or(t, ifd, tags::IMAGE_LENGTH, 0).unwrap_or(0);
        let area = w.saturating_mul(h);
        if best.is_none_or(|(_, a)| area > a) {
            best = Some((ifd, area));
        }
    }
    best.map(|(i, _)| i).ok_or(Error::Unsupported("no raw image in this DNG".into()))
}

/// Read and check the layout tags of a raw IFD.
pub fn layout(t: &Tiff, ifd: &Ifd) -> Result<RawLayout> {
    let width = uint_or(t, ifd, tags::IMAGE_WIDTH, 0)?;
    let height = uint_or(t, ifd, tags::IMAGE_LENGTH, 0)?;
    let spp = uint_or(t, ifd, tags::SAMPLES_PER_PIXEL, 1)?;
    let photometric = uint_or(t, ifd, tags::PHOTOMETRIC, 0)?;
    let compression = uint_or(t, ifd, tags::COMPRESSION, 1)?;
    let bits = match ifd.get(tags::BITS_PER_SAMPLE) {
        Some(e) => {
            let b = t.uints(e)?;
            let first = *b.first().ok_or(Error::Malformed("empty BitsPerSample"))?;
            if b.iter().any(|&x| x != first) {
                return Err(Error::Unsupported("different bit depths per sample".into()));
            }
            first
        }
        None => 1,
    };
    if width == 0 || height == 0 || width > 65_535 || height > 65_535 {
        return Err(Error::Malformed("raw image size"));
    }
    if !(1..=4).contains(&spp) {
        return Err(Error::Unsupported(format!("{spp} samples per pixel")));
    }
    if photometric == PHOTOMETRIC_CFA && spp != 1 {
        return Err(Error::Malformed("a CFA image with more than one sample per pixel"));
    }
    if !(1..=16).contains(&bits) {
        return Err(Error::Unsupported(format!("{bits}-bit raw samples")));
    }
    if uint_or(t, ifd, tags::SAMPLE_FORMAT, 1)? != 1 {
        return Err(Error::Unsupported("floating-point raw data".into()));
    }
    if spp > 1 && uint_or(t, ifd, tags::PLANAR_CONFIGURATION, 1)? != 1 {
        return Err(Error::Unsupported("planar raw data".into()));
    }
    match compression {
        1 => {
            if uint_or(t, ifd, tags::PREDICTOR, 1)? != 1 {
                return Err(Error::Unsupported("a TIFF predictor on uncompressed raw data".into()));
            }
        }
        7 => {}
        8 => return Err(Error::Unsupported("deflate-compressed DNG".into())),
        // DNG 1.4: lossy JPEG only for 8-bit LinearRaw (or mask) data
        34892 if photometric != PHOTOMETRIC_LINEAR_RAW => {
            return Err(Error::Unsupported("lossy JPEG DNG of CFA data (DNG allows it only for 8-bit LinearRaw)".into()));
        }
        34892 if bits != 8 => return Err(Error::Unsupported(format!("lossy JPEG DNG with {bits}-bit samples (DNG allows only 8)"))),
        34892 => {}
        // DNG 1.7: JPEG XL for 8–16-bit integer data of one or three planes (not CFA mosaics)
        52546 if !cfg!(feature = "jxl") => return Err(Error::Unsupported("JPEG XL DNG (this build has no JPEG XL decoder, feature `jxl`)".into())),
        52546 if photometric != PHOTOMETRIC_LINEAR_RAW => return Err(Error::Unsupported("JPEG XL DNG of CFA data (DNG allows it only for LinearRaw)".into())),
        52546 if !(8..=16).contains(&bits) || !matches!(spp, 1 | 3) => {
            return Err(Error::Unsupported(format!("JPEG XL DNG with {spp} samples of {bits} bits")));
        }
        52546 => {}
        c => return Err(Error::Unsupported(format!("DNG compression {c}"))),
    }
    Ok(RawLayout { width: width as usize, height: height as usize, samples_per_pixel: spp as usize, bits: bits as u32, compression, photometric })
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
        34892 => lossy_jpeg(c.bytes, need),
        #[cfg(feature = "jxl")]
        52546 => jpeg_xl(c.bytes, need),
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

/// Decode a baseline (lossy) JPEG tile or strip (Compression 34892) into `need` 8-bit samples:
/// one component (greyscale) or three (colour, converted from YCbCr by the JPEG decoder as the
/// JPEG says), in raster order.
fn lossy_jpeg(bytes: &[u8], need: usize) -> Result<Vec<u16>> {
    use image::ImageDecoder;
    let bad = |_| Error::Malformed("lossy JPEG tile");
    let decode = || -> Result<Vec<u16>> {
        let mut d = image::codecs::jpeg::JpegDecoder::new(std::io::Cursor::new(bytes)).map_err(bad)?;
        let channels = match d.color_type() {
            image::ColorType::L8 => 1usize,
            image::ColorType::Rgb8 => 3,
            c => return Err(Error::Unsupported(format!("lossy JPEG DNG tile of colour type {c:?}"))),
        };
        let (w, h) = d.dimensions();
        let total = (w as usize).checked_mul(h as usize).and_then(|v| v.checked_mul(channels)).ok_or(Error::TooLarge)?;
        if total < need {
            return Err(Error::Malformed("lossy JPEG smaller than its tile or strip"));
        }
        if total > need.saturating_mul(4).saturating_add(65_536) || total > MAX_SAMPLES {
            return Err(Error::TooLarge);
        }
        let mut limits = image::Limits::default();
        limits.max_alloc = Some(total as u64 * 4 + (1 << 20));
        d.set_limits(limits).map_err(bad)?;
        let mut buf = vec![0u8; total];
        d.read_image(&mut buf).map_err(bad)?;
        Ok(buf.iter().take(need).map(|&v| u16::from(v)).collect())
    };
    // a third-party decoder: a panic in it is an error for this file, not a crash
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(decode)).unwrap_or(Err(Error::Malformed("lossy JPEG decoder failed")))
}

/// Decode a JPEG XL tile or strip (Compression 52546, bare codestream or container) into `need`
/// samples: one or three channels, integer samples back at the codestream's bit depth (exact
/// for lossless JPEG XL), in raster order.
#[cfg(feature = "jxl")]
fn jpeg_xl(bytes: &[u8], need: usize) -> Result<Vec<u16>> {
    let bad = |_| Error::Malformed("JPEG XL tile");
    let decode = || -> Result<Vec<u16>> {
        let img = jxl_oxide::JxlImage::builder().read(bytes).map_err(bad)?;
        let (w, h) = (img.width() as usize, img.height() as usize);
        let px = w.checked_mul(h).ok_or(Error::TooLarge)?;
        // one or three channels must cover the chunk without being far larger
        if px.saturating_mul(3) < need || px > need.saturating_add(65_536) || px > MAX_SAMPLES {
            return Err(Error::Malformed("JPEG XL picture does not match its tile or strip"));
        }
        let depth = img.image_header().metadata.bit_depth;
        let jxl_oxide::image::BitDepth::IntegerSample { bits_per_sample } = depth else {
            return Err(Error::Unsupported("floating-point JPEG XL DNG".into()));
        };
        let bits = bits_per_sample.clamp(1, 16);
        let render = img.render_frame(0).map_err(bad)?;
        let fb = render.image_all_channels();
        let channels = fb.channels();
        if !matches!(channels, 1 | 3) || px.saturating_mul(channels) < need {
            return Err(Error::Unsupported(format!("JPEG XL DNG tile with {channels} channels")));
        }
        let max = ((1u32 << bits) - 1) as f32;
        Ok(fb.buf().iter().take(need).map(|&v| if v.is_finite() { (v * max).round().clamp(0.0, max) as u16 } else { 0 }).collect())
    };
    // a third-party decoder: a panic in it is an error for this file, not a crash
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(decode)).unwrap_or(Err(Error::Malformed("JPEG XL decoder failed")))
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
    // every sample takes at least one bit of file (a Huffman code is at least one bit long);
    // lossy JPEG at least two bits per 8 × 8 block and component (a DC code and an end of
    // block), so a tiny file cannot claim a huge picture
    let per_byte = match l.compression {
        34892 => 256,
        // JPEG XL codes flat areas in almost nothing; this still bounds the allocation
        52546 => 4096,
        _ => 8,
    };
    let coded: usize = chunks.iter().map(|c| c.bytes.len()).fold(0usize, usize::saturating_add);
    if total / per_byte > coded {
        return Err(Error::Truncated);
    }
    for c in &chunks {
        let n = c.w.checked_mul(c.h).and_then(|v| v.checked_mul(spp)).ok_or(Error::TooLarge)?;
        if n > MAX_SAMPLES {
            return Err(Error::TooLarge);
        }
        if n / per_byte > c.bytes.len() {
            return Err(Error::Truncated);
        }
    }
    let raw = |data| RawImage { width: l.width, height: l.height, samples_per_pixel: spp, bits: l.bits, data };
    // one chunk covering the image (a single strip): its samples are the image, no copy
    if let [c] = chunks.as_slice()
        && (c.x, c.y, c.w) == (0, 0, l.width)
        && c.h >= l.height
    {
        let mut d = decode_chunk(c, &l, t.big_endian)?;
        d.truncate(total);
        if d.len() == total {
            return Ok(raw(d));
        }
        return Err(Error::Truncated);
    }
    // bands of one row of tiles (or one strip): decoded in parallel, each band copying its own
    // chunks into place
    let row_len = l.width * spp;
    let (Some(first), true) = (chunks.first(), row_len > 0) else { return Err(Error::Malformed("no strips or tiles")) };
    let (band_rows, across) = (first.h.max(1), l.width.div_ceil(first.w.max(1)));
    let mut data = vec![0u16; total];
    let band = |k: usize, out: &mut [u16]| -> Result<()> {
        let mine = chunks.get(k * across..((k + 1) * across).min(chunks.len())).unwrap_or(&[]);
        #[cfg(feature = "threads")]
        let decoded: Vec<Result<Vec<u16>>> = mine.par_iter().map(|c| decode_chunk(c, &l, t.big_endian)).collect();
        #[cfg(not(feature = "threads"))]
        let decoded: Vec<Result<Vec<u16>>> = mine.iter().map(|c| decode_chunk(c, &l, t.big_endian)).collect();
        for (c, d) in mine.iter().zip(decoded) {
            let d = d?;
            let cw = c.w.min(l.width.saturating_sub(c.x));
            let ch = c.h.min(l.height.saturating_sub(c.y));
            for r in 0..ch {
                let src = d.get(r * c.w * spp..(r * c.w + cw) * spp).ok_or(Error::Truncated)?;
                let at = r * row_len + c.x * spp;
                out.get_mut(at..at + cw * spp).ok_or(Error::Truncated)?.copy_from_slice(src);
            }
        }
        Ok(())
    };
    #[cfg(feature = "threads")]
    data.par_chunks_mut(band_rows * row_len).enumerate().try_for_each(|(k, out)| band(k, out))?;
    #[cfg(not(feature = "threads"))]
    data.chunks_mut(band_rows * row_len).enumerate().try_for_each(|(k, out)| band(k, out))?;
    Ok(raw(data))
}
