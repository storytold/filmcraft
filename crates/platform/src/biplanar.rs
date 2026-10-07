//! Biplanar 4:2:0 pictures (NV12, and P010 with its 10-bit samples in the high bits of 16-bit
//! words) as read back from a decoder surface, into the planar [`PixelData::Yuv8`] /
//! [`PixelData::Yuv16`] frames the rest of FilmCraft uses. Safe code, tested on every target.
//!
//! This is the one place the Windows backend copies pixels (surface to staging texture to these
//! planes). A zero-copy path would replace it, not the decoder around it.

use std::sync::Arc;

use filmcraft_frame::{Chroma, PixelData, VideoFrame, pool};

/// A mapped biplanar surface: a luma plane followed by interleaved chroma, `stride` bytes per row.
pub struct Biplanar<'a> {
    pub luma: &'a [u8],
    pub chroma: &'a [u8],
    pub stride: usize,
    /// Rows in the luma plane (the surface height, which may exceed the picture's).
    pub rows: usize,
}

/// Where the picture is in the surface and how its samples are stored.
#[derive(Clone, Copy, Debug)]
pub struct Geometry {
    /// Output rectangle in luma samples: x, y, width, height.
    pub crop: (u32, u32, u32, u32),
    /// Significant bits per sample: 8 (NV12) or 10 (P010).
    pub bits: u32,
    pub color: filmcraft_color::ColorInfo,
    pub par: (u32, u32),
}

fn row(plane: &[u8], stride: usize, y: usize, x: usize, len: usize) -> Result<&[u8], String> {
    let start = y.checked_mul(stride).and_then(|s| s.checked_add(x)).ok_or("plane offset overflows")?;
    plane.get(start..start.checked_add(len).ok_or("plane offset overflows")?).ok_or_else(|| "plane row out of range".to_string())
}

/// Copy the picture at `g.crop` out of `src` into a planar 4:2:0 frame (deinterleaving chroma,
/// shifting 10-bit samples down from the high bits). Errors when the surface is smaller than the
/// rectangle needs.
pub fn to_frame(src: &Biplanar, g: &Geometry) -> Result<VideoFrame, String> {
    let (cx, cy, w, h) = g.crop;
    let (ox, oy, w, h) = (cx as usize, cy as usize, w as usize, h as usize);
    if w == 0 || h == 0 || !matches!(g.bits, 8 | 10) {
        return Err(format!("cannot read a {w}x{h} picture of {} bits", g.bits));
    }
    let bps = if g.bits > 8 { 2 } else { 1 };
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let (cox, coy) = (ox / 2, oy / 2);
    let needs_rows = oy.checked_add(h).ok_or("picture size overflows")?;
    let needs_cols = ox.checked_add(w).and_then(|c| c.checked_mul(bps)).ok_or("picture size overflows")?;
    if src.rows < needs_rows || src.stride < needs_cols || src.stride < (cox + cw) * 2 * bps {
        return Err(format!("decoded surface ({} rows, {} byte rows) is smaller than the {w}x{h} picture at {ox},{oy}", src.rows, src.stride));
    }
    let data = if bps == 1 {
        let mut yp = pool::take_u8(w * h);
        for y in 0..h {
            yp.extend_from_slice(row(src.luma, src.stride, oy + y, ox, w)?);
        }
        let (mut u, mut v) = (pool::take_u8(cw * ch), pool::take_u8(cw * ch));
        for y in 0..ch {
            // two passes over the row: each is a simple strided copy the compiler vectorises
            let pairs = row(src.chroma, src.stride, coy + y, cox * 2, cw * 2)?.as_chunks::<2>().0;
            u.extend(pairs.iter().map(|p| p[0]));
            v.extend(pairs.iter().map(|p| p[1]));
        }
        PixelData::Yuv8 { planes: [Arc::new(yp), Arc::new(u), Arc::new(v)], chroma: Chroma::C420, alpha: None }
    } else {
        let shift = 16 - g.bits;
        let mut yp = pool::take_u16(w * h);
        for y in 0..h {
            yp.extend(row(src.luma, src.stride, oy + y, ox * 2, w * 2)?.as_chunks::<2>().0.iter().map(|b| u16::from_le_bytes([b[0], b[1]]) >> shift));
        }
        let (mut u, mut v) = (pool::take_u16(cw * ch), pool::take_u16(cw * ch));
        for y in 0..ch {
            let quads = row(src.chroma, src.stride, coy + y, cox * 4, cw * 4)?.as_chunks::<4>().0;
            u.extend(quads.iter().map(|q| u16::from_le_bytes([q[0], q[1]]) >> shift));
            v.extend(quads.iter().map(|q| u16::from_le_bytes([q[2], q[3]]) >> shift));
        }
        PixelData::Yuv16 { planes: [Arc::new(yp), Arc::new(u), Arc::new(v)], chroma: Chroma::C420, bits: g.bits, alpha: None }
    };
    Ok(VideoFrame { width: w as u32, height: h as u32, data, color: g.color, par: g.par, pts: filmcraft_time::Tick::ZERO })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geometry(crop: (u32, u32, u32, u32), bits: u32) -> Geometry {
        Geometry { crop, bits, color: filmcraft_color::ColorInfo::REC709, par: (1, 1) }
    }

    #[test]
    fn nv12_is_deinterleaved_and_cropped() {
        // 8x6 surface, stride 10; the picture is the 4x4 at (2, 2)
        let (stride, rows) = (10usize, 6usize);
        let luma: Vec<u8> = (0..stride * rows).map(|i| i as u8).collect();
        let chroma: Vec<u8> = (0..stride * rows / 2).map(|i| 100 + i as u8).collect();
        let f = to_frame(&Biplanar { luma: &luma, chroma: &chroma, stride, rows }, &geometry((2, 2, 4, 4), 8)).unwrap();
        let PixelData::Yuv8 { planes, chroma: c, .. } = &f.data else { panic!("8-bit") };
        assert_eq!(*c, Chroma::C420);
        assert_eq!(&planes[0][..], &[22, 23, 24, 25, 32, 33, 34, 35, 42, 43, 44, 45, 52, 53, 54, 55]);
        // chroma rows 1..3, pairs from byte 2: row 1 starts at 10, row 2 at 20
        assert_eq!(&planes[1][..], &[112, 114, 122, 124]);
        assert_eq!(&planes[2][..], &[113, 115, 123, 125]);
    }

    #[test]
    fn p010_shifts_down_from_the_high_bits() {
        let (stride, rows) = (8usize, 2usize);
        let mut luma = vec![0u8; stride * rows];
        for (i, s) in luma.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            *s = (((i as u16) + 1) << 6).to_le_bytes();
        }
        let mut chroma = vec![0u8; stride];
        for (i, s) in chroma.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            *s = ((500 + i as u16) << 6).to_le_bytes();
        }
        let f = to_frame(&Biplanar { luma: &luma, chroma: &chroma, stride, rows }, &geometry((0, 0, 4, 2), 10)).unwrap();
        let PixelData::Yuv16 { planes, bits, .. } = &f.data else { panic!("16-bit") };
        assert_eq!(*bits, 10);
        assert_eq!(&planes[0][..], &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!((&planes[1][..], &planes[2][..]), (&[500, 502][..], &[501, 503][..]));
    }

    #[test]
    fn surfaces_smaller_than_the_picture_are_errors() {
        let luma = vec![0u8; 64];
        let chroma = vec![0u8; 32];
        let src = Biplanar { luma: &luma, chroma: &chroma, stride: 8, rows: 8 };
        for crop in [(0, 0, 9, 8), (0, 0, 8, 9), (4, 0, 8, 8), (0, 0, 0, 8), (u32::MAX, 0, 8, 8), (0, 0, 8, 8)] {
            let r = to_frame(&src, &geometry(crop, 8));
            assert_eq!(r.is_ok(), crop == (0, 0, 8, 8), "{crop:?}");
        }
        assert!(to_frame(&src, &geometry((0, 0, 4, 4), 12)).is_err());
        // a short chroma plane is an error, not a panic
        let short = Biplanar { luma: &luma, chroma: &chroma[..8], stride: 8, rows: 8 };
        assert!(to_frame(&short, &geometry((0, 0, 8, 8), 8)).is_err());
    }
}
