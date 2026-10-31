//! Uncompressed still formats written directly from RGBA8 frames: Targa, DPX and OpenEXR.
//!
//! Targa follows the Truevision TGA File Format Specification 2.0 (image type 2, uncompressed
//! true-colour, bottom-left origin, the layout every reader accepts). DPX follows SMPTE ST 268
//! (version 2.0 header): one RGB image element, 10 bits per component packed into 32-bit
//! big-endian words (method A, two padding bits at the bottom), BT.709 transfer and colorimetry.
//! OpenEXR follows the public OpenEXR file layout (version 2, single-part scan-line file): half-float
//! B, G, R (and A) channels, scene-linear Rec.709 values with premultiplied alpha, as EXR readers
//! expect; uncompressed, RLE, ZIPS (zlib per scan line) or ZIP (zlib per 16 scan lines). The
//! compressed modes first split each chunk's bytes into even and odd halves and delta-code them
//! (the format's predictor); a chunk that would not shrink is stored raw, as the format allows.
use crate::{ExportError, ExrCompression, Result};

/// `w * h * 4` when `rgba` holds exactly that many bytes.
fn checked_len(rgba: &[u8], w: u32, h: u32) -> Result<usize> {
    let n = (w as usize).checked_mul(h as usize).and_then(|n| n.checked_mul(4)).ok_or_else(|| ExportError::Encode("frame size".into()))?;
    if w == 0 || h == 0 || rgba.len() != n {
        return Err(ExportError::Encode("frame size".into()));
    }
    Ok(n)
}

/// The RGB bytes of an RGBA8 frame (alpha dropped).
pub(crate) fn rgb(rgba: &[u8], w: u32, h: u32) -> Result<Vec<u8>> {
    checked_len(rgba, w, h)?;
    Ok(rgba.as_chunks::<4>().0.iter().flat_map(|&[r, g, b, _]| [r, g, b]).collect())
}

/// An uncompressed Targa file: 24-bit BGR, or 32-bit BGRA with `alpha` (straight alpha, as the
/// frame carries it).
pub(crate) fn tga(rgba: &[u8], w: u32, h: u32, alpha: bool) -> Result<Vec<u8>> {
    checked_len(rgba, w, h)?;
    let (tw, th) = match (u16::try_from(w), u16::try_from(h)) {
        (Ok(tw), Ok(th)) => (tw, th),
        _ => return Err(ExportError::Unsupported(format!("Targa frames are at most 65535×65535 (got {w}×{h})"))),
    };
    let bpp: usize = if alpha { 4 } else { 3 };
    let row = w as usize * 4;
    let mut out = Vec::with_capacity(18 + w as usize * h as usize * bpp + 26);
    out.extend_from_slice(&[0, 0, 2]); // no ID, no colour map, uncompressed true-colour
    out.extend_from_slice(&[0; 5]); // colour map specification (unused)
    out.extend_from_slice(&[0, 0, 0, 0]); // x / y origin
    out.extend_from_slice(&tw.to_le_bytes());
    out.extend_from_slice(&th.to_le_bytes());
    out.push(if alpha { 32 } else { 24 });
    out.push(if alpha { 8 } else { 0 }); // attribute bits; bottom-left origin
    // bottom-left origin: the last frame row comes first
    for line in rgba.chunks_exact(row).rev() {
        for &[r, g, b, a] in line.as_chunks::<4>().0 {
            out.extend_from_slice(&[b, g, r]);
            if alpha {
                out.push(a);
            }
        }
    }
    // TGA 2.0 footer without extension or developer areas
    out.extend_from_slice(&[0; 8]);
    out.extend_from_slice(b"TRUEVISION-XFILE.\0");
    Ok(out)
}

/// Header size of the DPX files we write (file + image + orientation + film + TV headers).
const DPX_HEADER: usize = 2048;

/// A DPX file: 10-bit RGB (8-bit values widened by bit replication, so 0 → 0 and 255 → 1023).
pub(crate) fn dpx(rgba: &[u8], w: u32, h: u32) -> Result<Vec<u8>> {
    let n = checked_len(rgba, w, h)?;
    // one 32-bit word per pixel; `n` is already `w * h * 4`
    let size = n.checked_add(DPX_HEADER).ok_or_else(|| ExportError::Encode("frame size".into()))?;
    let file_size = u32::try_from(size).map_err(|_| ExportError::Unsupported(format!("a {w}×{h} DPX frame exceeds 4 GB")))?;
    // SMPTE ST 268: undefined numeric fields are all ones, undefined text fields are NUL
    let mut hd = vec![0xFF_u8; DPX_HEADER];
    for (a, b) in [
        (36, 660),    // file name, creation time, creator, project, copyright
        (664, 768),   // reserved
        (1356, 1408), // reserved
        (1432, 1620), // source file name, time, input device, serial number
        (1644, 1664), // reserved
        (1664, 1712), // film manufacturer, type, offset, prefix, count, format
        (1732, 1920), // frame id, slate, reserved
        (1972, 2048), // reserved
    ] {
        hd[a..b].fill(0);
    }
    for e in 0..8 {
        let d = 780 + e * 72 + 40;
        hd[d..d + 32].fill(0); // element descriptions
    }
    let put32 = |hd: &mut Vec<u8>, at: usize, v: u32| hd[at..at + 4].copy_from_slice(&v.to_be_bytes());
    let put16 = |hd: &mut Vec<u8>, at: usize, v: u16| hd[at..at + 2].copy_from_slice(&v.to_be_bytes());
    // file information header
    hd[0..4].copy_from_slice(b"SDPX");
    put32(&mut hd, 4, DPX_HEADER as u32); // offset to the image data
    hd[8..16].copy_from_slice(b"V2.0\0\0\0\0");
    put32(&mut hd, 16, file_size);
    put32(&mut hd, 20, 1); // ditto key: new frame
    put32(&mut hd, 24, 1664); // generic header length
    put32(&mut hd, 28, 384); // industry header length
    put32(&mut hd, 32, 0); // user data length
    hd[160..169].copy_from_slice(b"FilmCraft");
    // image information header
    put16(&mut hd, 768, 0); // orientation: left to right, top to bottom
    put16(&mut hd, 770, 1); // one image element
    put32(&mut hd, 772, w);
    put32(&mut hd, 776, h);
    let el = 780;
    put32(&mut hd, el, 0); // unsigned
    put32(&mut hd, el + 4, 0); // reference low data code
    put32(&mut hd, el + 12, 1023); // reference high data code
    hd[el + 20] = 50; // descriptor: RGB
    hd[el + 21] = 6; // transfer: ITU-R BT.709
    hd[el + 22] = 6; // colorimetric: ITU-R BT.709
    hd[el + 23] = 10; // bit depth
    put16(&mut hd, el + 24, 1); // packing: filled to 32-bit words, method A
    put16(&mut hd, el + 26, 0); // no encoding (uncompressed)
    put32(&mut hd, el + 28, DPX_HEADER as u32); // offset to the element's data
    put32(&mut hd, el + 32, 0); // end-of-line padding
    put32(&mut hd, el + 36, 0); // end-of-image padding
    // image orientation header: no offset, square pixels
    put32(&mut hd, 1408, 0);
    put32(&mut hd, 1412, 0);
    put32(&mut hd, 1424, w);
    put32(&mut hd, 1428, h);
    put32(&mut hd, 1628, 1);
    put32(&mut hd, 1632, 1);
    // television header: progressive
    hd[1928] = 0;
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(&hd);
    let ten = |v: u8| (u32::from(v) << 2) | (u32::from(v) >> 6);
    for &[r, g, b, _] in rgba.as_chunks::<4>().0 {
        out.extend_from_slice(&((ten(r) << 22) | (ten(g) << 12) | (ten(b) << 2)).to_be_bytes());
    }
    Ok(out)
}

/// IEEE 754 binary16 bits of `v`, rounded to nearest even (overflow → infinity, NaN → quiet NaN).
fn half(v: f32) -> u16 {
    let b = v.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    let exp = ((b >> 23) & 0xFF) as i32;
    let man = b & 0x7F_FFFF;
    if exp == 0xFF {
        return sign | 0x7C00 | if man != 0 { 0x200 } else { 0 };
    }
    // rounds `m >> shift` to nearest, ties to even (`shift` is 1..=24)
    let round = |m: u32, shift: u32| {
        let (q, rem, mid) = (m >> shift, m & ((1 << shift) - 1), 1 << (shift - 1));
        if rem > mid || (rem == mid && q & 1 == 1) { q + 1 } else { q }
    };
    let e = exp - 127 + 15;
    if e >= 0x1F {
        sign | 0x7C00
    } else if e <= 0 {
        if e < -10 {
            return sign;
        }
        // subnormal half: the 24-bit significand scaled by 2^(e - 14); rounding up may carry
        // into the smallest normal, which is the right encoding
        sign | round(man | 0x80_0000, (14 - e) as u32) as u16
    } else {
        // a carry out of the mantissa bumps the exponent (up to infinity), as it should
        sign | round(((e as u32) << 23) | man, 13) as u16
    }
}

/// An OpenEXR file: half-float scene-linear Rec.709 RGB, or RGBA with `alpha` (premultiplied, as
/// EXR defines alpha; the frame's straight alpha is multiplied in after linearisation).
pub(crate) fn exr(rgba: &[u8], w: u32, h: u32, alpha: bool, compression: ExrCompression) -> Result<Vec<u8>> {
    let n = checked_len(rgba, w, h)?;
    let too_large = || ExportError::Unsupported(format!("an OpenEXR frame of {w}×{h} is too large"));
    let (Ok(x_max), Ok(y_max)) = (i32::try_from(w - 1), i32::try_from(h - 1)) else {
        return Err(too_large());
    };
    // channels in the alphabetical order the file layout requires
    let names: &[u8] = if alpha { b"ABGR" } else { b"BGR" };
    let line = (w as usize).checked_mul(names.len() * 2).ok_or_else(|| ExportError::Encode("frame size".into()))?;
    // a chunk's byte count is a signed 32-bit field
    let lines = compression.lines();
    if line.checked_mul(lines).and_then(|c| i32::try_from(c).ok()).is_none() {
        return Err(too_large());
    }

    let mut hd = Vec::with_capacity(512);
    hd.extend_from_slice(&[0x76, 0x2F, 0x31, 0x01]); // magic number 20000630
    hd.extend_from_slice(&[2, 0, 0, 0]); // version 2, single-part scan-line file, short names
    let attr = |hd: &mut Vec<u8>, name: &str, ty: &str, value: &[u8]| {
        for s in [name, ty] {
            hd.extend_from_slice(s.as_bytes());
            hd.push(0);
        }
        hd.extend_from_slice(&(value.len() as i32).to_le_bytes());
        hd.extend_from_slice(value);
    };
    let le = |v: &[f32]| v.iter().flat_map(|f| f.to_le_bytes()).collect::<Vec<u8>>();
    let mut channels = Vec::new();
    for &c in names {
        channels.extend_from_slice(&[c, 0]);
        channels.extend_from_slice(&1_i32.to_le_bytes()); // pixel type HALF
        channels.extend_from_slice(&[0, 0, 0, 0]); // pLinear 0, reserved
        channels.extend_from_slice(&1_i32.to_le_bytes()); // x sampling
        channels.extend_from_slice(&1_i32.to_le_bytes()); // y sampling
    }
    channels.push(0);
    let window: Vec<u8> = [0, 0, x_max, y_max].iter().flat_map(|v| v.to_le_bytes()).collect();
    attr(&mut hd, "channels", "chlist", &channels);
    attr(&mut hd, "chromaticities", "chromaticities", &le(&[0.64, 0.33, 0.30, 0.60, 0.15, 0.06, 0.3127, 0.3290]));
    attr(&mut hd, "compression", "compression", &[compression.code()]);
    attr(&mut hd, "dataWindow", "box2i", &window);
    attr(&mut hd, "displayWindow", "box2i", &window);
    attr(&mut hd, "lineOrder", "lineOrder", &[0]); // INCREASING_Y
    attr(&mut hd, "pixelAspectRatio", "float", &le(&[1.0]));
    attr(&mut hd, "screenWindowCenter", "v2f", &le(&[0.0, 0.0]));
    attr(&mut hd, "screenWindowWidth", "float", &le(&[1.0]));
    hd.push(0); // end of header

    // offset table (one entry per chunk), then the chunks: first y, data size, data
    let chunks = (h as usize).div_ceil(lines);
    let table = chunks.checked_mul(8);
    let body = (h as usize).checked_mul(line + 8);
    let size = table.zip(body).and_then(|(t, b)| hd.len().checked_add(t)?.checked_add(b)).ok_or_else(|| ExportError::Encode("frame size".into()))?;
    // the uncompressed size: compressed chunks only shrink it
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(&hd);
    let table_at = out.len();
    out.resize(table_at + chunks * 8, 0);
    let lin = filmcraft_color::srgb_u8_to_linear_table();
    let opaque: [u16; 256] = std::array::from_fn(|i| half(lin[i]));
    let row = w as usize * 4;
    let wn = w as usize;
    let mut px = vec![0_u16; wn * names.len()];
    let mut raw = Vec::with_capacity(line * lines);
    for (k, block) in rgba[..n].chunks(row * lines).enumerate() {
        raw.clear();
        for src in block.chunks_exact(row) {
            // planar per line: every pixel of the first channel, then the next channel …
            for (x, &[r, g, b, a]) in src.as_chunks::<4>().0.iter().enumerate() {
                let rgb = [b, g, r];
                if alpha {
                    let k = f32::from(a) / 255.0;
                    px[x] = half(k);
                    for (c, &v) in rgb.iter().enumerate() {
                        px[(c + 1) * wn + x] = if a == 255 { opaque[v as usize] } else { half(lin[v as usize] * k) };
                    }
                } else {
                    for (c, &v) in rgb.iter().enumerate() {
                        px[c * wn + x] = opaque[v as usize];
                    }
                }
            }
            raw.extend(px.iter().flat_map(|v| v.to_le_bytes()));
        }
        let packed = match compression {
            ExrCompression::None => None,
            ExrCompression::Rle => Some(rle(&predict(&raw))),
            ExrCompression::Zips | ExrCompression::Zip => Some(crate::deflate::zlib(&predict(&raw))),
        };
        // a chunk that does not shrink is stored as it is (readers compare the sizes)
        let data = packed.as_deref().filter(|p| p.len() < raw.len()).unwrap_or(&raw);
        let offset = (out.len() as u64).to_le_bytes();
        out[table_at + k * 8..table_at + k * 8 + 8].copy_from_slice(&offset);
        out.extend_from_slice(&((k * lines) as i32).to_le_bytes());
        out.extend_from_slice(&(data.len() as i32).to_le_bytes());
        out.extend_from_slice(data);
    }
    Ok(out)
}

/// The OpenEXR RLE / ZIP predictor: bytes split into even and odd halves, then each byte replaced
/// by its difference from the previous one, plus 128.
fn predict(raw: &[u8]) -> Vec<u8> {
    let half = raw.len().div_ceil(2);
    let mut t = vec![0_u8; raw.len()];
    for (i, &b) in raw.iter().enumerate() {
        t[if i % 2 == 0 { i / 2 } else { half + i / 2 }] = b;
    }
    let mut p = t.first().copied().unwrap_or(0);
    for v in t.iter_mut().skip(1) {
        let d = v.wrapping_sub(p).wrapping_add(128);
        p = *v;
        *v = d;
    }
    t
}

/// OpenEXR run-length coding: a signed count byte, then either `count + 1` copies of one byte
/// (count ≥ 0, runs of 3 to 128) or `-count` literal bytes (1 to 127).
fn rle(src: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len() + src.len() / 127 + 1);
    let mut i = 0;
    while i < src.len() {
        let b = src[i];
        let run = src[i..].iter().take(128).take_while(|&&x| x == b).count();
        if run >= 3 {
            out.extend_from_slice(&[(run - 1) as u8, b]);
            i += run;
            continue;
        }
        // literals up to the next run of three (or 127 bytes)
        let s = i;
        while i < src.len() && i - s < 127 {
            if src.get(i..i + 3).is_some_and(|t| t[0] == t[1] && t[1] == t[2]) {
                break;
            }
            i += 1;
        }
        out.push(((i - s) as u8).wrapping_neg());
        out.extend_from_slice(&src[s..i]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn be32(b: &[u8], at: usize) -> u32 {
        u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
    }

    #[test]
    fn dpx_header_and_packing() {
        // 2×1: pure red, then (1, 128, 255)
        let px = [255, 0, 0, 255, 1, 128, 255, 7];
        let f = dpx(&px, 2, 1).unwrap();
        assert_eq!(&f[0..4], b"SDPX");
        assert_eq!(f.len(), 2048 + 8);
        assert_eq!(be32(&f, 4), 2048);
        assert_eq!(be32(&f, 16), f.len() as u32);
        assert_eq!((be32(&f, 772), be32(&f, 776)), (2, 1));
        assert_eq!((f[800], f[803]), (50, 10));
        assert_eq!(be32(&f, 2048), 1023 << 22);
        let p = be32(&f, 2052);
        assert_eq!(((p >> 22) & 1023, (p >> 12) & 1023, (p >> 2) & 1023, p & 3), (4, 514, 1023, 0));
    }

    #[test]
    fn tga_rows_are_bottom_up_bgr() {
        // 1×2: top red, bottom blue (half transparent)
        let px = [255, 0, 0, 255, 0, 0, 255, 128];
        let f = tga(&px, 1, 2, false).unwrap();
        assert_eq!(&f[..3], &[0, 0, 2]);
        assert_eq!((f[16], f[17]), (24, 0));
        assert_eq!(&f[18..24], &[255, 0, 0, 0, 0, 255]); // blue row first, as BGR
        assert!(f.ends_with(b"TRUEVISION-XFILE.\0"));
        let f = tga(&px, 1, 2, true).unwrap();
        assert_eq!((f[16], f[17]), (32, 8));
        assert_eq!(&f[18..26], &[255, 0, 0, 128, 0, 0, 255, 255]);
    }

    #[test]
    fn half_floats_round_to_nearest_even() {
        for (v, h) in [
            (0.0, 0x0000),
            (-0.0, 0x8000),
            (1.0, 0x3C00),
            (0.5, 0x3800),
            (-2.0, 0xC000),
            (65504.0, 0x7BFF),
            (65520.0, 0x7C00),        // halfway past the largest half: ties to even = infinity
            (1.0e9, 0x7C00),          // overflow
            (6.103_515_6e-5, 0x0400), // smallest normal
            (5.960_464_5e-8, 0x0001), // smallest subnormal, 2^-24
            (2.980_232_2e-8, 0x0000), // 2^-25: halfway to zero, ties to even
            (1.0e-10, 0x0000),
            (1.000_488_3, 0x3C00), // 1 + 2^-11: halfway, ties to even (down)
            (1.001_464_8, 0x3C02), // 1 + 3·2^-11: halfway, ties to even (up)
            (f32::INFINITY, 0x7C00),
        ] {
            assert_eq!(half(v), h, "{v}");
        }
        assert_eq!(half(f32::NAN) & 0x7E00, 0x7E00);
    }

    fn le32(b: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
    }

    /// Where `needle` (an attribute name, NUL-terminated) starts in the header.
    fn find(b: &[u8], needle: &[u8]) -> usize {
        b.windows(needle.len()).position(|w| w == needle).unwrap()
    }

    #[test]
    fn exr_layout() {
        // 2×1: opaque white, then (255, 0, 0) at alpha 0.5
        let px = [255, 255, 255, 255, 255, 0, 0, 128];
        for alpha in [false, true] {
            let f = exr(&px, 2, 1, alpha, ExrCompression::None).unwrap();
            let nch = if alpha { 4 } else { 3 };
            assert_eq!(&f[..8], &[0x76, 0x2F, 0x31, 0x01, 2, 0, 0, 0]);
            let ch = find(&f, b"channels\0chlist\0") + 16;
            assert_eq!(le32(&f, ch) as usize, nch * 18 + 1);
            let names: Vec<u8> = (0..nch).map(|i| f[ch + 4 + i * 18]).collect();
            assert_eq!(names, if alpha { b"ABGR".to_vec() } else { b"BGR".to_vec() });
            let dw = find(&f, b"dataWindow\0box2i\0") + 17;
            assert_eq!((le32(&f, dw), le32(&f, dw + 4), le32(&f, dw + 8), le32(&f, dw + 12)), (16, 0, 0, 1));
            let cmp = find(&f, b"compression\0compression\0") + 24;
            assert_eq!((le32(&f, cmp), f[cmp + 4]), (1, 0));
            // header ends with a NUL; one offset (scan line 0) points at the only chunk
            let end = f.len() - (8 + 2 * nch * 2) - 8;
            assert_eq!(f[end - 1], 0);
            let off = u64::from_le_bytes(f[end..end + 8].try_into().unwrap()) as usize;
            assert_eq!(off, end + 8);
            assert_eq!((le32(&f, off), le32(&f, off + 4) as usize), (0, 2 * nch * 2));
            let hv = |c: usize, x: usize| u16::from_le_bytes(f[off + 8 + (c * 2 + x) * 2..][..2].try_into().unwrap());
            let (b, g, r) = if alpha { (1, 2, 3) } else { (0, 1, 2) };
            // white is 1.0 everywhere; red is linear 1.0 · alpha (premultiplied) or 1.0 flat
            assert_eq!((hv(b, 0), hv(g, 0), hv(r, 0)), (0x3C00, 0x3C00, 0x3C00));
            assert_eq!((hv(b, 1), hv(g, 1)), (0, 0));
            if alpha {
                assert_eq!((hv(0, 0), hv(0, 1)), (0x3C00, half(128.0 / 255.0)));
                assert_eq!(hv(r, 1), half(128.0 / 255.0));
            } else {
                assert_eq!(hv(r, 1), 0x3C00);
            }
        }
    }

    /// A 40×35 test picture: gradients, a flat band and a noisy corner, with varying alpha (35
    /// rows: the last 16-line ZIP chunk is short).
    fn picture() -> (Vec<u8>, u32, u32) {
        let (w, h) = (40_u32, 35_u32);
        let px = (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .flat_map(|(x, y)| {
                let noise = (x * 7919 + y * 104_729) % 251;
                let r = if y < 12 {
                    (x * 6) as u8
                } else if y < 20 {
                    200
                } else {
                    noise as u8
                };
                [r, (y * 7) as u8, ((x + y) * 3) as u8, [255, 128, 0, 64][(x / 10) as usize % 4]]
            })
            .collect();
        (px, w, h)
    }

    /// The chunks of an OpenEXR file we wrote: (first y, data).
    fn chunks(f: &[u8], count: usize) -> Vec<(u32, &[u8])> {
        let end = find(f, b"screenWindowWidth\0float\0") + 24 + 4 + 4 + 1;
        (0..count)
            .map(|k| {
                let off = u64::from_le_bytes(f[end + k * 8..end + k * 8 + 8].try_into().unwrap()) as usize;
                let n = le32(f, off + 4) as usize;
                (le32(f, off), &f[off + 8..off + 8 + n])
            })
            .collect()
    }

    /// Undo [`predict`].
    fn unpredict(t: &[u8]) -> Vec<u8> {
        let mut t = t.to_vec();
        for i in 1..t.len() {
            t[i] = t[i - 1].wrapping_add(t[i]).wrapping_sub(128);
        }
        let half = t.len().div_ceil(2);
        (0..t.len()).map(|i| t[if i % 2 == 0 { i / 2 } else { half + i / 2 }]).collect()
    }

    fn unrle(mut s: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        while let [c, rest @ ..] = s {
            let c = *c as i8;
            if c < 0 {
                out.extend_from_slice(&rest[..c.unsigned_abs() as usize]);
                s = &rest[c.unsigned_abs() as usize..];
            } else {
                out.extend(std::iter::repeat_n(rest[0], c as usize + 1));
                s = &rest[1..];
            }
        }
        out
    }

    #[test]
    fn rle_codes_runs_and_literals() {
        for src in [vec![], vec![5], vec![1, 1], vec![9; 300], (0..=255).collect::<Vec<u8>>(), [vec![1, 2], vec![3; 3], vec![4, 4, 5]].concat()] {
            assert_eq!(unrle(&rle(&src)), src);
        }
        assert_eq!(rle(&[7; 130]), vec![127, 7, 0xFE, 7, 7]);
        assert_eq!(rle(&[1, 2, 2, 2]), vec![0xFF, 1, 2, 2]);
    }

    /// Every compression stores the same pixels as the uncompressed file, in chunks of the right
    /// height, and the compressed files are smaller.
    #[test]
    fn compressed_exr_holds_the_same_pixels() {
        let (px, w, h) = picture();
        for alpha in [false, true] {
            let plain = exr(&px, w, h, alpha, ExrCompression::None).unwrap();
            let lines: Vec<&[u8]> = chunks(&plain, h as usize).into_iter().map(|(_, d)| d).collect();
            for c in [ExrCompression::Rle, ExrCompression::Zips, ExrCompression::Zip] {
                let f = exr(&px, w, h, alpha, c).unwrap();
                let cmp = find(&f, b"compression\0compression\0") + 24;
                assert_eq!(f[cmp + 4], c.code());
                assert!(f.len() < plain.len() * 3 / 4, "{c:?}: {} vs {}", f.len(), plain.len());
                let n = (h as usize).div_ceil(c.lines());
                for (k, (y, data)) in chunks(&f, n).into_iter().enumerate() {
                    assert_eq!(y as usize, k * c.lines());
                    let want: Vec<u8> = lines[k * c.lines()..((k + 1) * c.lines()).min(h as usize)].concat();
                    let got = if data.len() == want.len() {
                        data.to_vec()
                    } else if c == ExrCompression::Rle {
                        unpredict(&unrle(data))
                    } else {
                        unpredict(&crate::deflate::tests::inflate(data).unwrap())
                    };
                    assert_eq!(got, want, "{c:?} chunk {k}");
                }
            }
        }
    }

    /// ffmpeg (external test oracle) decodes our OpenEXR files to the values we meant: linear
    /// light, premultiplied alpha, rows top to bottom; in every compression.
    #[test]
    fn exr_decodes_in_ffmpeg() {
        let Some(ffmpeg) = filmcraft_testkit::ffmpeg_or_skip("OpenEXR stills") else { return };
        let (px, w, h) = picture();
        let dir = filmcraft_testkit::fixtures::workspace_root().join("target").join("export-tests").join(format!("exr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lin = filmcraft_color::srgb_u8_to_linear_table();
        for c in ExrCompression::ALL {
            for alpha in [false, true] {
                let path = dir.join(format!("f{alpha}-{c:?}.exr"));
                std::fs::write(&path, exr(&px, w, h, alpha, c).unwrap()).unwrap();
                let out = std::process::Command::new(&ffmpeg)
                    .args(["-v", "error", "-i"])
                    .arg(&path)
                    .args(["-f", "rawvideo", "-pix_fmt", "gbrapf32le", "-"])
                    .output()
                    .unwrap();
                assert!(out.status.success(), "{c:?}: {}", String::from_utf8_lossy(&out.stderr));
                let n = (w * h) as usize;
                assert_eq!(out.stdout.len(), n * 4 * 4, "{c:?}");
                let plane = |p: usize, i: usize| f32::from_le_bytes(out.stdout[(p * n + i) * 4..][..4].try_into().unwrap());
                for (i, &[r, g, b, a]) in px.as_chunks::<4>().0.iter().enumerate() {
                    let k = if alpha { f32::from(a) / 255.0 } else { 1.0 };
                    // gbrap planes: G, B, R, A
                    let want = [lin[g as usize] * k, lin[b as usize] * k, lin[r as usize] * k, k];
                    for (p, want) in want.into_iter().enumerate() {
                        let got = plane(p, i);
                        assert!((got - want).abs() <= 1e-3 + want * 1e-3, "{c:?}, alpha {alpha}, pixel {i}, plane {p}: {got} vs {want}");
                    }
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hostile_sizes_are_errors() {
        assert!(dpx(&[0; 7], 2, 1).is_err());
        assert!(dpx(&[], 0, 0).is_err());
        assert!(tga(&[0; 4], 1, 2, false).is_err());
        assert!(tga(&[0; 4], u32::MAX, u32::MAX, false).is_err());
        assert!(rgb(&[0; 3], 1, 1).is_err());
        assert!(dpx(&[0; 4], u32::MAX, u32::MAX).is_err());
        for c in ExrCompression::ALL {
            assert!(exr(&[0; 4], u32::MAX, u32::MAX, true, c).is_err());
            assert!(exr(&[0; 7], 2, 1, false, c).is_err());
            assert!(exr(&[], 0, 1, false, c).is_err());
        }
    }
}
