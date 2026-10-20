//! Where every macroblock scan line of a frame is: for decoders that run elsewhere (the GPU
//! compositor decodes scan lines in compute shaders, `filmcraft-gpu`). The headers, the coding
//! units of a field-coded frame and the scan indices are parsed and checked here exactly as
//! [`crate::decode_frame`] does, so a frame with a layout fails CPU decoding only inside a scan
//! line's entropy-coded data.

use crate::decode::{coding_units, row_ranges};
use crate::header::{FieldCode, FrameHeader};
use crate::tables::cid_info;
use crate::{Error, Result};

/// One macroblock scan line: its coded data and where its 16 lines go in the frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowLayout {
    /// Byte range (offset into the frame data, length) of its macroblocks.
    pub data: (u32, u32),
    /// Scan line index within its coding unit.
    pub mb_row: u32,
    /// Frame line of the coding unit's first line (1 for the bottom field), and the distance of
    /// its lines in the frame (2 for a field).
    pub first_line: u32,
    pub line_step: u32,
    /// Its coding unit (0, or 1 for the second of a field-coded frame), in coded order.
    pub unit: u32,
}

/// A frame's geometry and scan lines (both coding units of a field-coded frame, in coded order).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameLayout {
    /// The first coding unit's header.
    pub header: FrameHeader,
    /// Frame size (both fields of a field-coded frame).
    pub width: u32,
    pub height: u32,
    pub rows: Vec<RowLayout>,
}

impl FrameLayout {
    /// Macroblocks per scan line.
    pub fn mb_width(&self) -> u32 {
        self.header.mb_width()
    }
    /// Whether every colour macroblock is coded the way a scan line decoder of [`tables`]
    /// handles: no macroblock-adaptive field coding (CID 1260).
    pub fn scan_line_decodable(&self) -> bool {
        let h = &self.header;
        !h.macf && h.cid != 1260
    }
}

/// Parse a frame's coding-unit headers and scan indices.
pub fn frame_layout(data: &[u8]) -> Result<FrameLayout> {
    if u32::try_from(data.len()).is_err() {
        return Err(Error::Invalid("frame too large"));
    }
    let units = coding_units(data)?;
    let first = units.first().map(|(_, h)| h.clone()).ok_or(Error::Truncated)?;
    let fields = units.len() > 1;
    let mut rows = Vec::with_capacity(units.iter().map(|(_, h)| h.mb_rows as usize).sum());
    for (ui, (range, h)) in units.iter().enumerate() {
        let unit = data.get(range.clone()).ok_or(Error::Truncated)?;
        let (first_line, line_step) = match (fields, h.field) {
            (false, _) => (0, 1),
            (true, FieldCode::Field2) => (1, 2),
            (true, _) => (0, 2),
        };
        for (i, (st, end)) in row_ranges(h, unit)?.into_iter().enumerate() {
            let off = range.start + st;
            rows.push(RowLayout { data: (off as u32, (end - st) as u32), mb_row: i as u32, first_line, line_step, unit: ui as u32 });
        }
    }
    let height = if fields { first.lines * 2 } else { first.lines };
    Ok(FrameLayout { width: first.width, height, header: first, rows })
}

/// The constant tables a scan line decoder needs (SMPTE ST 2019-1 Annexes C–E), for decoders
/// elsewhere.
pub mod tables {
    use crate::ChromaFormat;
    pub use crate::tables::{AMP_EOB, AMP_FINDEX, AMP_FRUN, LINK};

    /// A two-level VLC lookup table: `primary` is indexed by the next `bits` bits of the stream;
    /// an entry is a symbol (`length << 16 | value`) or, with [`LINK`] set, a link
    /// (`LINK | sub_bits << 24 | offset`) into `secondary`, indexed by the `sub_bits` bits after
    /// those. The longest code is `bits` + the largest `sub_bits`.
    #[derive(Debug, Clone, Copy)]
    pub struct Lut {
        pub primary: &'static [u32],
        pub secondary: &'static [u32],
        pub bits: u32,
    }

    impl Lut {
        /// Length of the longest code.
        pub fn max_len(&self) -> u32 {
            self.bits + self.primary.iter().filter(|&&e| e & LINK != 0).map(|e| (e >> 24) & 0x7f).max().unwrap_or(0)
        }
    }

    /// What a compression ID codes its macroblocks with.
    #[derive(Debug, Clone, Copy)]
    pub struct CidTables {
        /// Quantisation weights `[luma, chroma]` in raster order without the DC entry
        /// (`weights[c][z - 1]` for raster position `z`).
        pub weights: &'static [[u8; 63]; 2],
        /// Inverse quantisation parameter `p` (8 or 32).
        pub p: u32,
        /// Amplitude symbols (`amp | AMP_FRUN | AMP_FINDEX`, or `AMP_EOB`), runs and DC sizes.
        pub amp: Lut,
        pub run: Lut,
        pub dc: Lut,
    }

    /// The tables of compression ID `cid`.
    pub fn cid(cid: u32) -> Option<CidTables> {
        let info = super::cid_info(cid)?;
        let v = crate::tables::vlc(info.vlc);
        let lut = |t: &'static crate::tables::Lut| Lut { primary: &t.primary, secondary: &t.secondary, bits: t.bits };
        Some(CidTables { weights: info.weights, p: info.p, amp: lut(&v.amp), run: lut(&v.run), dc: lut(&v.dc) })
    }

    /// The blocks of a colour macroblock in coded order: (plane, x, y) inside the macroblock.
    pub fn blocks(chroma: ChromaFormat) -> &'static [(u8, u8, u8)] {
        match chroma {
            ChromaFormat::Yuv422 => &crate::decode::LAYOUT_422,
            ChromaFormat::Yuv420 => &crate::decode::LAYOUT_420,
            ChromaFormat::Yuv444 => &crate::decode::LAYOUT_444,
        }
    }

    /// The blocks of a DCT-coded alpha macroblock (plane 3).
    pub fn alpha_blocks() -> &'static [(u8, u8, u8)] {
        &crate::decode::LAYOUT_ALPHA
    }

    /// The luma coefficients (Kr, Kb) `ConvertMacroblockColor` (8.1.1) uses for a colour volume:
    /// what Y'CbCr macroblocks of a mixed R'G'B' picture are converted with.
    pub fn mb_color_kr_kb(cv: crate::ColorVolume) -> (f32, f32) {
        crate::decode::mb_color_kr_kb(cv)
    }

    /// Bitstream coefficient index → raster position `v*8 + u`.
    pub fn zigzag() -> [u8; 64] {
        crate::tables::ZIGZAG
    }

    /// The orthonormal 1-D DCT basis the IDCT uses (`BASIS[u][x]`).
    pub fn basis() -> [[f32; 8]; 8] {
        crate::tables::BASIS
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChromaFormat, Encoder, EncoderConfig, Frame, Profile};

    fn sample(profile: Profile, w: u32, h: u32, bits: u8) -> Vec<u8> {
        let mut f = Frame::new(w, h, ChromaFormat::Yuv422, bits, false);
        for (i, v) in f.y.iter_mut().enumerate() {
            *v = (16 + (i % 200)) as u16;
        }
        let cfg = EncoderConfig { bit_depth: bits, ..EncoderConfig::new(profile, w, h) };
        Encoder::with_config(cfg).and_then(|mut e| e.encode(&f)).unwrap_or_default()
    }

    #[test]
    fn layout_covers_every_scan_line_once() {
        for (p, w, h, bits) in [(Profile::Hq, 1920, 1080, 8), (Profile::Hqx, 334, 201, 10), (Profile::Sq, 64, 16, 8)] {
            let s = sample(p, w, h, bits);
            let l = frame_layout(&s).unwrap_or_else(|e| panic!("{w}x{h}: {e}"));
            assert_eq!((l.width, l.height), (w, h));
            assert_eq!(l.rows.len() as u32, h.div_ceil(16));
            assert!(l.scan_line_decodable());
            for (i, r) in l.rows.iter().enumerate() {
                assert_eq!((r.mb_row, r.first_line, r.line_step, r.unit), (i as u32, 0, 1, 0));
                assert!((r.data.0 + r.data.1) as usize <= s.len());
            }
            assert!(crate::decode_frame(&s).is_ok());
            assert!(tables::cid(l.header.cid).is_some_and(|t| t.amp.max_len() <= 32 && t.run.max_len() <= 32 && t.dc.max_len() <= 32));
        }
    }

    #[test]
    fn layout_rejects_what_decoding_rejects_structurally() {
        let s = sample(Profile::Hq, 640, 360, 8);
        for cut in [0, 8, 0x200, 0x27f] {
            let t = &s[..cut];
            assert!(frame_layout(t).is_err(), "truncated at {cut}");
            assert!(crate::decode_frame(t).is_err());
        }
    }
}
