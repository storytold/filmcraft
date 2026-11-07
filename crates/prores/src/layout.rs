//! Where every slice of a frame is: for decoders that run elsewhere (the GPU compositor decodes
//! slices in compute shaders, `filmcraft-gpu`). The headers, the slice tables and every slice
//! header are parsed and checked here exactly as [`crate::decode_frame`] does, so a frame with a
//! layout fails CPU decoding only inside a slice's entropy-coded data.

use crate::decode::{pictures, slice_parts};
use crate::header::FrameHeader;
use crate::tables::qscale;
use crate::{AlphaType, ChromaFormat, Error, Interlace, Result};

/// One slice: its place in the frame and its coded data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SliceLayout {
    /// Macroblock row (of its picture; a field picture's rows are 32 frame lines apart).
    pub mb_row: u32,
    /// First macroblock column.
    pub mb_x: u32,
    /// `log2` of its width in macroblocks (0..=3).
    pub log2_mbs: u8,
    /// Line parity within the frame of its picture (0 = even lines; always 0 progressive).
    pub parity: u8,
    /// Quantisation scale (`qscale(quantization_index)`).
    pub qscale: u32,
    /// Byte ranges (offset into the frame data, length) of the Y, Cb, Cr and alpha data; alpha is
    /// empty without an alpha channel.
    pub comps: [(u32, u32); 4],
}

/// A frame's geometry and slices (both pictures of an interlaced frame, in coded order).
#[derive(Debug, Clone, PartialEq)]
pub struct FrameLayout {
    pub header: FrameHeader,
    /// Bytes of the frame (the sample may hold trailing bytes after it).
    pub frame_size: usize,
    /// The precision the frame is coded at: 10 bits for 4:2:2, 12 for 4:4:4.
    pub coded_depth: u8,
    pub mb_width: u32,
    /// Macroblock rows per picture.
    pub mb_height: u32,
    pub slices: Vec<SliceLayout>,
}

impl FrameLayout {
    pub fn interlaced(&self) -> bool {
        self.header.interlace != Interlace::Progressive
    }
    pub fn has_alpha(&self) -> bool {
        self.header.alpha != AlphaType::None
    }
    /// Widest slice, in macroblocks.
    pub fn max_slice_mbs(&self) -> u32 {
        self.slices.iter().map(|s| 1u32 << s.log2_mbs).max().unwrap_or(1)
    }
}

/// Parse a frame's headers, slice tables and slice headers.
pub fn frame_layout(data: &[u8]) -> Result<FrameLayout> {
    let (hdr, off, frame_size) = FrameHeader::parse(data)?;
    let data = data.get(..frame_size).ok_or(Error::Truncated)?;
    let width = hdr.width as usize;
    let height = hdr.height as usize;
    let interlaced = hdr.interlace != Interlace::Progressive;
    let mb_width = width.div_ceil(16);
    let mb_height = if interlaced { height.div_ceil(32) } else { height.div_ceil(16) };
    let pics = pictures(data, &hdr, off, mb_width, mb_height)?;
    let has_alpha = hdr.alpha != AlphaType::None;
    let mut slices = Vec::with_capacity(pics.iter().map(|(p, _)| p.slices.len()).sum());
    for (pi, (pic, widths)) in pics.iter().enumerate() {
        for row in 0..mb_height {
            let mut mb_x = 0usize;
            for (k, &w) in widths.iter().enumerate() {
                let si = row * widths.len() + k;
                let &(start, len) = pic.slices.get(si).ok_or(Error::Truncated)?;
                let d = data.get(start..start + len).ok_or(Error::Truncated)?;
                let sp = slice_parts(d, has_alpha).map_err(|_| Error::Slice { picture: pi, slice: si })?;
                let at = |(o, n): (usize, usize)| ((start + o) as u32, n as u32);
                slices.push(SliceLayout {
                    mb_row: row as u32,
                    mb_x: mb_x as u32,
                    log2_mbs: w.trailing_zeros() as u8,
                    parity: pic.parity as u8,
                    qscale: qscale(sp.qidx),
                    comps: sp.comps.map(at),
                });
                mb_x += w;
            }
        }
    }
    let coded_depth = match hdr.chroma {
        ChromaFormat::Yuv422 => 10,
        ChromaFormat::Yuv444 => 12,
    };
    Ok(FrameLayout { header: hdr, frame_size, coded_depth, mb_width: mb_width as u32, mb_height: mb_height as u32, slices })
}

/// The constant tables a slice decoder needs (SMPTE RDD 36), for decoders elsewhere.
pub mod tables {
    use crate::Profile;
    pub use crate::tables::{AC_ROUND, DC_CTX_INIT, LEVEL_CTX_INIT, RUN_CTX_INIT, SCAN_INTERLACED, SCAN_PROGRESSIVE};

    /// A codebook as `(rice, exp, rice_q_max)`.
    pub type Codebook = (u8, u8, u8);

    fn cb(c: crate::tables::Codebook) -> Codebook {
        (c.rice, c.exp, c.rice_q_max)
    }
    pub fn first_dc() -> Codebook {
        cb(crate::tables::FIRST_DC_CB)
    }
    pub fn dc() -> [Codebook; 7] {
        crate::tables::DC_CB.map(cb)
    }
    pub fn run() -> [Codebook; 16] {
        crate::tables::RUN_CB.map(cb)
    }
    pub fn level() -> [Codebook; 10] {
        crate::tables::LEVEL_CB.map(cb)
    }
    /// The quantisation scale of every slice quantisation index `0..=224` (`[0]` is as `[1]`).
    pub fn qscale_table() -> [u32; 225] {
        std::array::from_fn(|q| crate::tables::qscale(q as u8))
    }
    /// The quantisation scale of slice quantisation index `q`.
    pub fn qscale(q: u8) -> u32 {
        crate::tables::qscale(q)
    }
    /// The luma and chroma quantisation matrices (raster order) this crate's encoder writes for
    /// `profile`: flat 4 for every profile.
    pub fn profile_qmats(profile: Profile) -> ([u8; 64], [u8; 64]) {
        let _ = profile;
        ([4; 64], [4; 64])
    }
    /// The orthonormal 1-D DCT basis the IDCT uses (`BASIS[u][x]`).
    pub fn basis() -> [[f32; 8]; 8] {
        crate::tables::BASIS
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Encoder, EncoderConfig, Frame, Profile};

    fn sample(profile: Profile, w: u32, h: u32, log2_slice_mbs: u8) -> Vec<u8> {
        let mut f = Frame::new(w, h, profile.chroma(), 10, false);
        for (i, v) in f.y.iter_mut().enumerate() {
            *v = 64 + (i % 700) as u16;
        }
        let mut enc = Encoder::with_config(EncoderConfig { log2_slice_mbs, ..EncoderConfig::new(profile, w, h) });
        enc.encode(&f).unwrap_or_default()
    }

    #[test]
    fn layout_covers_every_macroblock_once() {
        for (w, h, log2) in [(1920, 1080, 3), (333, 201, 3), (70, 38, 1), (1280, 720, 2)] {
            let s = sample(Profile::Hq, w, h, log2);
            let l = frame_layout(&s).unwrap_or_else(|e| panic!("{w}x{h}: {e}"));
            assert_eq!(l.mb_width, w.div_ceil(16));
            let covered: u32 = l.slices.iter().map(|s| 1u32 << s.log2_mbs).sum();
            assert_eq!(covered, l.mb_width * l.mb_height, "{w}x{h}");
            for sl in &l.slices {
                for (o, n) in sl.comps {
                    assert!((o + n) as usize <= l.frame_size);
                }
            }
            assert!(crate::decode_frame(&s).is_ok());
        }
    }

    #[test]
    fn layout_rejects_what_decoding_rejects_structurally() {
        let s = sample(Profile::Standard, 640, 360, 3);
        for cut in [0, 8, 100, s.len() / 2, s.len() - 1] {
            let t = &s[..cut];
            assert!(frame_layout(t).is_err(), "truncated at {cut}");
            assert!(crate::decode_frame(t).is_err());
        }
    }
}
