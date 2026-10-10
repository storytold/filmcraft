//! Uncompressed still formats written directly from RGBA8 frames: Targa and DPX.
//!
//! Targa follows the Truevision TGA File Format Specification 2.0 (image type 2, uncompressed
//! true-colour, bottom-left origin, the layout every reader accepts). DPX follows SMPTE ST 268
//! (version 2.0 header): one RGB image element, 10 bits per component packed into 32-bit
//! big-endian words (method A, two padding bits at the bottom), BT.709 transfer and colorimetry.
use crate::{ExportError, Result};

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
    fn hostile_sizes_are_errors() {
        assert!(dpx(&[0; 7], 2, 1).is_err());
        assert!(dpx(&[], 0, 0).is_err());
        assert!(tga(&[0; 4], 1, 2, false).is_err());
        assert!(tga(&[0; 4], u32::MAX, u32::MAX, false).is_err());
        assert!(rgb(&[0; 3], 1, 1).is_err());
        assert!(dpx(&[0; 4], u32::MAX, u32::MAX).is_err());
    }
}
