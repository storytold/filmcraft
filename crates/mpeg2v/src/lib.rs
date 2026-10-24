//! Clean-room MPEG-2 video (ITU-T H.262 | ISO/IEC 13818-2) and MPEG-1 video (ISO/IEC 11172-2)
//! decoder.
//!
//! - Main Profile at Low/Main/High-1440/High Level, 4:2:2 Profile (XDCAM HD422, IMX / D-10),
//!   Simple Profile, and MPEG-1 (including D pictures and constrained-parameters streams).
//! - Progressive and interlaced coding: frame pictures (frame / field / dual-prime motion
//!   compensation, frame and field DCT) and field pictures (field / 16x8 / dual-prime motion
//!   compensation, the second field predicted from the first), alternate scan, both intra VLC
//!   tables, linear and non-linear quantiser scale, quantiser matrices (sequence header and quant
//!   matrix extension), intra DC precision 8-11 bits, concealment motion vectors.
//! - Output in display order as planar 8-bit frames (two fields woven into one frame), with
//!   field order, repeat-first-field, picture type and colour description.
//!
//! ```no_run
//! let mut dec = filmcraft_mpeg2v::Decoder::new();
//! # let access_units: Vec<(Vec<u8>, i64)> = vec![];
//! for (au, pts) in access_units {
//!     for pic in dec.decode(&au, pts)? {
//!         println!("{}x{} {:?} pts {}", pic.width, pic.height, pic.picture_type, pic.pts);
//!     }
//! }
//! let rest = dec.flush();
//! # Ok::<(), filmcraft_mpeg2v::Error>(())
//! ```

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

mod bits;
pub mod headers;
mod idct;
mod mc;
mod slice;
mod vlc;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

pub use headers::{
    DisplayExtension, Extension, GopHeader, PictureCodingExtension, PictureHeader, PictureType, QuantMatrixExtension, SequenceExtension, SequenceHeader,
    profile_level_name, start_codes,
};

use headers::{EXTENSION, GROUP_START, PICTURE_START, SEQUENCE_END, SEQUENCE_HEADER, SLICE_MAX, SLICE_MIN};
use mc::Frame;
use slice::{PicParams, Target, decode_slice, slice_row};

/// One access unit must not retain an unbounded number of cropped decoded pictures.
const MAX_OUTPUT_PICTURES_PER_ACCESS_UNIT: usize = 8;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid MPEG video: {0}")]
    Invalid(String),
    #[error("unsupported MPEG video: {0}")]
    Unsupported(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChromaFormat {
    Yuv420,
    Yuv422,
    Yuv444,
}

/// Stream-level properties (sequence header and its extensions).
#[derive(Clone, Debug, PartialEq)]
pub struct SequenceInfo {
    /// ISO/IEC 13818-2 (has a sequence extension) or ISO/IEC 11172-2.
    pub mpeg2: bool,
    pub width: u32,
    pub height: u32,
    pub chroma: ChromaFormat,
    pub profile_and_level: Option<u8>,
    pub progressive_sequence: bool,
    pub low_delay: bool,
    /// Frames per second (num, den).
    pub frame_rate: Option<(u32, u32)>,
    /// Sample aspect ratio.
    pub sar: (u32, u32),
    /// Display aspect ratio when signalled (MPEG-2 aspect_ratio_information 2-4).
    pub dar: Option<(u32, u32)>,
    /// (colour_primaries, transfer_characteristics, matrix_coefficients) from the sequence
    /// display extension.
    pub colour: Option<(u8, u8, u8)>,
    pub video_format: Option<u8>,
    /// bits/s (0: unknown / variable).
    pub bit_rate: u64,
}

impl SequenceInfo {
    /// "MPEG-2 Video (Main@High)", "MPEG-1 Video".
    pub fn codec_name(&self) -> String {
        match (self.mpeg2, self.profile_and_level) {
            (false, _) => "MPEG-1 Video".into(),
            (true, Some(p)) => format!("MPEG-2 Video ({})", profile_level_name(p)),
            (true, None) => "MPEG-2 Video".into(),
        }
    }
}

/// A decoded frame (both fields of field-coded pictures), cropped to the coded size.
#[derive(Clone, Debug)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub chroma: ChromaFormat,
    pub y: Vec<u8>,
    pub cb: Vec<u8>,
    pub cr: Vec<u8>,
    /// The timestamp passed with the access unit the picture (its first field) came in.
    pub pts: i64,
    pub picture_type: PictureType,
    pub temporal_reference: u16,
    pub progressive_frame: bool,
    pub top_field_first: bool,
    pub repeat_first_field: bool,
    /// Coded as two field pictures.
    pub field_pictures: bool,
    pub info: Arc<SequenceInfo>,
}

impl Picture {
    pub fn chroma_width(&self) -> u32 {
        match self.chroma {
            ChromaFormat::Yuv444 => self.width,
            _ => self.width.div_ceil(2),
        }
    }
    pub fn chroma_height(&self) -> u32 {
        match self.chroma {
            ChromaFormat::Yuv420 => self.height.div_ceil(2),
            _ => self.height,
        }
    }
    /// Interlaced content and which field comes first: `None` for progressive frames.
    pub fn field_order(&self) -> Option<FieldOrder> {
        if self.progressive_frame {
            None
        } else if self.top_field_first {
            Some(FieldOrder::TopFirst)
        } else {
            Some(FieldOrder::BottomFirst)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldOrder {
    TopFirst,
    BottomFirst,
}

/// The active sequence.
#[derive(Clone)]
struct Seq {
    hdr: SequenceHeader,
    ext: Option<SequenceExtension>,
    display: Option<DisplayExtension>,
    info: Arc<SequenceInfo>,
    mb_width: usize,
    mb_height: usize,
}

/// MPEG-1 pel_aspect_ratio (height/width of a sample) ×10000, Table 2-D.2.
const MPEG1_PEL_ASPECT: [u32; 15] = [0, 10000, 6735, 7031, 7615, 8055, 8437, 8935, 9157, 9815, 10255, 10695, 10950, 11575, 12015];

/// Largest accepted luma area (8192 x 4096; 8K UHD is 7680 x 4320).
const MAX_PICTURE_AREA: u64 = 1 << 25;

impl Seq {
    fn new(hdr: SequenceHeader, ext: Option<SequenceExtension>, display: Option<DisplayExtension>) -> Result<Seq> {
        let (w, h) = match &ext {
            Some(e) => (hdr.horizontal_size | (e.horizontal_size_extension as u32) << 12, hdr.vertical_size | (e.vertical_size_extension as u32) << 12),
            None => (hdr.horizontal_size, hdr.vertical_size),
        };
        let chroma = match ext.map_or(1, |e| e.chroma_format) {
            1 => ChromaFormat::Yuv420,
            2 => ChromaFormat::Yuv422,
            3 => ChromaFormat::Yuv444,
            _ => return Err(Error::Invalid("reserved chroma_format".into())),
        };
        // The 14-bit sizes allow 16383 x 16383 (gigabytes per frame, several frames and a gray reference
        // held); no real stream exceeds 8K, so cap the area there.
        if w > 16383 || h > 16383 || u64::from(w) * u64::from(h) > MAX_PICTURE_AREA {
            return Err(Error::Unsupported(format!("{w}x{h}")));
        }
        let progressive_sequence = ext.is_none_or(|e| e.progressive_sequence);
        let mb_width = (w as usize).div_ceil(16);
        let mb_height = if progressive_sequence { (h as usize).div_ceil(16) } else { 2 * (h as usize).div_ceil(32) };
        let frame_rate = headers::frame_rate(hdr.frame_rate_code, ext.map_or(0, |e| e.frame_rate_extension_n), ext.map_or(0, |e| e.frame_rate_extension_d));
        let (sar, dar) = if ext.is_some() {
            let (dw, dh) = display
                .filter(|d| d.display_horizontal_size > 0 && d.display_vertical_size > 0)
                .map_or((w, h), |d| (d.display_horizontal_size as u32, d.display_vertical_size as u32));
            let dar = match hdr.aspect_ratio_information {
                2 => Some((4, 3)),
                3 => Some((16, 9)),
                4 => Some((221, 100)),
                _ => None,
            };
            let sar = match dar {
                Some((n, d)) => {
                    // the display rectangle (dw × dh samples) has this aspect: SAR = DAR · dh / dw
                    let (n, d) = (n as u64 * dh as u64, d as u64 * dw as u64);
                    let g = gcd64(n, d);
                    ((n / g) as u32, (d / g) as u32)
                }
                None => (1, 1),
            };
            (reduce_sar(sar), dar)
        } else {
            let v = MPEG1_PEL_ASPECT.get(hdr.aspect_ratio_information as usize).copied().filter(|&v| v > 0).unwrap_or(10000);
            let g = headers::gcd(10000, v);
            ((10000 / g, v / g), None)
        };
        let bit_rate = (hdr.bit_rate_value as u64 | (ext.map_or(0, |e| e.bit_rate_extension as u64) << 18)) * 400;
        let info = SequenceInfo {
            mpeg2: ext.is_some(),
            width: w,
            height: h,
            chroma,
            profile_and_level: ext.map(|e| e.profile_and_level_indication),
            progressive_sequence,
            low_delay: ext.is_some_and(|e| e.low_delay),
            frame_rate,
            sar,
            dar,
            colour: display.and_then(|d| d.colour),
            video_format: display.map(|d| d.video_format),
            bit_rate: if hdr.bit_rate_value == 0x3FFFF && ext.is_none() { 0 } else { bit_rate },
        };
        Ok(Seq { hdr, ext, display, info: Arc::new(info), mb_width, mb_height })
    }

    fn shifts(&self) -> (u32, u32) {
        match self.info.chroma {
            ChromaFormat::Yuv420 => (1, 1),
            ChromaFormat::Yuv422 => (1, 0),
            ChromaFormat::Yuv444 => (0, 0),
        }
    }
}

fn gcd64(a: u64, b: u64) -> u64 {
    if b == 0 { a.max(1) } else { gcd64(b, a % b) }
}

fn reduce_sar((n, d): (u32, u32)) -> (u32, u32) {
    if n == 0 || d == 0 {
        return (1, 1);
    }
    let g = headers::gcd(n, d);
    (n / g, d / g)
}

/// Parse the first sequence header (and its extensions) in `data`.
pub fn probe(data: &[u8]) -> Option<SequenceInfo> {
    let codes = start_codes(data);
    let mut seq: Option<(SequenceHeader, Option<SequenceExtension>, Option<DisplayExtension>)> = None;
    for (k, &(pos, code)) in codes.iter().enumerate() {
        let end = codes.get(k + 1).map_or(data.len(), |c| c.0);
        let payload = &data[(pos + 4).min(end)..end];
        match code {
            SEQUENCE_HEADER if seq.is_none() => seq = Some((SequenceHeader::parse(payload).ok()?, None, None)),
            EXTENSION => {
                if let Some(s) = seq.as_mut() {
                    match Extension::parse(payload) {
                        Ok(Extension::Sequence(e)) => s.1 = Some(e),
                        Ok(Extension::Display(d)) => s.2 = Some(d),
                        _ => {}
                    }
                }
            }
            _ if seq.is_some() => break,
            _ => {}
        }
    }
    let (h, e, d) = seq?;
    Seq::new(h, e, d).ok().map(|s| (*s.info).clone())
}

/// What an access unit holds, without decoding it.
#[derive(Clone, Debug, Default)]
pub struct AccessUnitInfo {
    pub sequence_header: bool,
    pub gop: Option<GopHeader>,
    pub pictures: Vec<(PictureHeader, Option<PictureCodingExtension>)>,
}

impl AccessUnitInfo {
    /// Starts with an I picture (decoding can begin here when a sequence header has been seen).
    pub fn is_intra(&self) -> bool {
        self.pictures.first().is_some_and(|p| matches!(p.0.coding_type, PictureType::I | PictureType::D))
    }
    /// Only B pictures (never used for prediction).
    pub fn is_disposable(&self) -> bool {
        !self.pictures.is_empty() && self.pictures.iter().all(|p| p.0.coding_type == PictureType::B)
    }
}

pub fn scan_access_unit(data: &[u8]) -> AccessUnitInfo {
    let codes = start_codes(data);
    let mut info = AccessUnitInfo::default();
    for (k, &(pos, code)) in codes.iter().enumerate() {
        let end = codes.get(k + 1).map_or(data.len(), |c| c.0);
        let payload = &data[(pos + 4).min(end)..end];
        match code {
            SEQUENCE_HEADER => info.sequence_header = true,
            GROUP_START => info.gop = GopHeader::parse(payload).ok(),
            PICTURE_START => {
                if let Ok(ph) = PictureHeader::parse(payload) {
                    info.pictures.push((ph, None));
                }
            }
            EXTENSION => {
                if let (Ok(Extension::PictureCoding(p)), Some(last)) = (Extension::parse(payload), info.pictures.last_mut()) {
                    last.1 = Some(p);
                }
            }
            _ => {}
        }
    }
    info
}

/// Split an elementary stream into access units: one coded frame each (a frame picture or a
/// pair of field pictures) with the sequence / GOP headers before it. Bytes before the first
/// header are skipped.
pub fn access_units(es: &[u8]) -> Vec<std::ops::Range<usize>> {
    let codes = start_codes(es);
    let mut out = Vec::new();
    let mut cur: Option<usize> = None;
    let mut header: Option<usize> = None;
    let mut open_field: Option<u8> = None;
    for (k, &(pos, code)) in codes.iter().enumerate() {
        match code {
            SEQUENCE_HEADER | GROUP_START => {
                header.get_or_insert(pos);
            }
            PICTURE_START => {
                // picture_structure from the picture coding extension that follows
                let structure = codes
                    .iter()
                    .enumerate()
                    .skip(k + 1)
                    .take_while(|(_, c)| c.1 == EXTENSION || c.1 == headers::USER_DATA)
                    .find_map(|(next, &(p, c))| {
                        // `next` is this unit's position in `codes`, so its successor gives the
                        // payload end directly. Searching `codes` from the beginning here made a
                        // stream of short picture/extension pairs quadratic to index.
                        let end = codes.get(next + 1).map_or(es.len(), |x| x.0);
                        (c == EXTENSION).then(|| Extension::parse(&es[(p + 4).min(end)..end]).ok()).flatten()
                    })
                    .and_then(|e| match e {
                        Extension::PictureCoding(p) => Some(p.picture_structure),
                        _ => None,
                    })
                    .unwrap_or(3);
                let start = header.take().unwrap_or(pos);
                let second = matches!(open_field, Some(s) if structure != 3 && structure != s);
                if second {
                    open_field = None;
                } else {
                    if let Some(c) = cur {
                        out.push(c..start);
                    }
                    cur = Some(start);
                    open_field = (structure != 3).then_some(structure);
                }
            }
            _ => {}
        }
    }
    if let Some(c) = cur {
        out.push(c..es.len());
    }
    out
}

/// A picture whose header has been read and whose slices are being collected.
struct Pending {
    ph: PictureHeader,
    pce: Option<PictureCodingExtension>,
    /// (start code, payload range).
    slices: Vec<(u8, std::ops::Range<usize>)>,
}

#[derive(Clone)]
struct Meta {
    pts: i64,
    ptype: PictureType,
    temporal_reference: u16,
    pce: PictureCodingExtension,
    field_pictures: bool,
    info: Arc<SequenceInfo>,
}

/// A frame whose first field has been decoded.
struct Building {
    frame: Frame,
    parity: usize,
    meta: Meta,
}

struct Anchor {
    frame: Arc<Frame>,
    meta: Meta,
    shown: bool,
}

/// A stateful decoder. Feed access units (or any chunks holding whole pictures) in decode order;
/// pictures come out in display order.
pub struct Decoder {
    seq: Option<Seq>,
    /// A sequence header is waiting for its extensions (applied at the next non-extension unit).
    seq_pending: Option<(SequenceHeader, Option<SequenceExtension>, Option<DisplayExtension>)>,
    matrices: [[u8; 64]; 4],
    closed_gop: bool,
    /// Older and newer anchor (reference) frames.
    fwd: Option<Anchor>,
    bwd: Option<Anchor>,
    building: Option<Building>,
    /// Mid-gray stand-in for missing references.
    gray: Option<Arc<Frame>>,
    pending: Option<Pending>,
    out: Vec<Picture>,
    output_overflow: bool,
    errors: usize,
    last_error: Option<&'static str>,
    threads: bool,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder {
    pub fn new() -> Decoder {
        Decoder {
            seq: None,
            seq_pending: None,
            matrices: [headers::DEFAULT_INTRA, [16; 64], headers::DEFAULT_INTRA, [16; 64]],
            closed_gop: false,
            fwd: None,
            bwd: None,
            building: None,
            gray: None,
            pending: None,
            out: Vec::new(),
            output_overflow: false,
            errors: 0,
            last_error: None,
            threads: cfg!(feature = "threads"),
        }
    }

    /// Decode slices on the rayon pool (default with the `threads` feature).
    pub fn set_threads(&mut self, on: bool) {
        self.threads = on && cfg!(feature = "threads");
    }

    /// Slices that failed to decode so far (corrupt data; the picture is still output).
    pub fn errors(&self) -> usize {
        self.errors
    }

    /// What the most recent slice error was.
    pub fn last_error(&self) -> Option<&'static str> {
        self.last_error
    }

    /// The active sequence.
    pub fn sequence(&self) -> Option<&SequenceInfo> {
        self.seq.as_ref().map(|s| &*s.info)
    }

    /// Decode a chunk of the elementary stream holding whole pictures (an access unit) tagged
    /// with `pts`. Returns the pictures that became displayable, in display order.
    pub fn decode(&mut self, data: &[u8], pts: i64) -> Result<Vec<Picture>> {
        self.output_overflow = false;
        let codes = start_codes(data);
        let mut err = None;
        for (k, &(pos, code)) in codes.iter().enumerate() {
            let end = codes.get(k + 1).map_or(data.len(), |c| c.0);
            let range = (pos + 4).min(end)..end;
            let payload = &data[range.clone()];
            if code != EXTENSION {
                self.apply_sequence();
            }
            match code {
                SEQUENCE_HEADER => {
                    self.finish_picture(data, pts);
                    match SequenceHeader::parse(payload) {
                        Ok(h) => self.seq_pending = Some((h, None, None)),
                        Err(e) => err = Some(e),
                    }
                }
                EXTENSION => match Extension::parse(payload) {
                    Ok(Extension::Sequence(e)) => {
                        if let Some(s) = self.seq_pending.as_mut() {
                            s.1 = Some(e);
                        } else if let Some(s) = &self.seq {
                            self.seq_pending = Some((s.hdr.clone(), Some(e), s.display));
                        }
                    }
                    Ok(Extension::Display(d)) => {
                        if let Some(s) = self.seq_pending.as_mut() {
                            s.2 = Some(d);
                        }
                    }
                    Ok(Extension::QuantMatrix(q)) => {
                        if let Some(m) = q.intra {
                            self.matrices[0] = m;
                            self.matrices[2] = m;
                        }
                        if let Some(m) = q.non_intra {
                            self.matrices[1] = m;
                            self.matrices[3] = m;
                        }
                        if let Some(m) = q.chroma_intra {
                            self.matrices[2] = m;
                        }
                        if let Some(m) = q.chroma_non_intra {
                            self.matrices[3] = m;
                        }
                    }
                    Ok(Extension::PictureCoding(p)) => {
                        if let Some(pd) = self.pending.as_mut() {
                            pd.pce = Some(p);
                        }
                    }
                    Ok(Extension::Other(_)) => {}
                    Err(e) => err = Some(e),
                },
                GROUP_START => {
                    self.finish_picture(data, pts);
                    if let Ok(g) = GopHeader::parse(payload) {
                        self.closed_gop = g.closed_gop || g.broken_link;
                    }
                }
                PICTURE_START => {
                    self.finish_picture(data, pts);
                    match PictureHeader::parse(payload) {
                        Ok(ph) => self.pending = Some(Pending { ph, pce: None, slices: Vec::new() }),
                        Err(e) => err = Some(e),
                    }
                }
                SLICE_MIN..=SLICE_MAX => {
                    if let Some(pd) = self.pending.as_mut() {
                        pd.slices.push((code, range));
                    }
                }
                SEQUENCE_END => self.finish_picture(data, pts),
                _ => {}
            }
        }
        self.apply_sequence();
        self.finish_picture(data, pts);
        if self.output_overflow {
            self.out.clear();
            return Err(Error::Invalid("access unit produces too many decoded pictures".into()));
        }
        let out = std::mem::take(&mut self.out);
        match err {
            Some(e) if out.is_empty() && self.seq.is_none() => Err(e),
            _ => Ok(out),
        }
    }

    /// Output the frames held back for reordering (end of stream).
    pub fn flush(&mut self) -> Vec<Picture> {
        if let Some(b) = self.building.take() {
            self.complete(b.frame, b.meta);
        }
        if let Some(a) = self.bwd.as_mut()
            && !a.shown
        {
            a.shown = true;
            let p = to_picture(&a.frame, &a.meta);
            self.push_output(p);
        }
        std::mem::take(&mut self.out)
    }

    /// Forget all pictures (before decoding from a new random-access point). The sequence
    /// parameters are kept: a random-access point without a sequence header still decodes.
    pub fn reset(&mut self) {
        self.fwd = None;
        self.bwd = None;
        self.building = None;
        self.pending = None;
        self.out.clear();
        self.output_overflow = false;
        self.closed_gop = false;
    }

    fn apply_sequence(&mut self) {
        let Some((h, e, d)) = self.seq_pending.take() else { return };
        // a sequence header resets the quantiser matrices (§6.3.11)
        let intra = h.intra_matrix.unwrap_or(headers::DEFAULT_INTRA);
        let non_intra = h.non_intra_matrix.unwrap_or([16; 64]);
        self.matrices = [intra, non_intra, intra, non_intra];
        match Seq::new(h, e, d) {
            Ok(s) => {
                let changed = self.seq.as_ref().is_none_or(|o| o.mb_width != s.mb_width || o.mb_height != s.mb_height || o.info.chroma != s.info.chroma);
                if changed {
                    self.reset();
                }
                self.seq = Some(s);
            }
            Err(_) => self.errors += 1,
        }
    }

    fn finish_picture(&mut self, data: &[u8], pts: i64) {
        let Some(pd) = self.pending.take() else { return };
        if pd.slices.is_empty() {
            self.errors += 1;
            self.last_error = Some("picture has no slices");
            return;
        }
        let Some(seq) = self.seq.clone() else {
            self.errors += 1;
            return;
        };
        let mpeg2 = seq.ext.is_some();
        let pce = match (mpeg2, pd.pce) {
            (true, Some(p)) => p,
            (true, None) => {
                self.errors += 1;
                return;
            }
            (false, _) => PictureCodingExtension::mpeg1(&pd.ph),
        };
        if seq.info.chroma == ChromaFormat::Yuv444 {
            self.errors += 1;
            return;
        }
        let ptype = pd.ph.coding_type;
        let (shx, shy) = seq.shifts();
        let (w, h) = (seq.mb_width * 16, seq.mb_height * 16);
        let (cw, ch) = (w >> shx, h >> shy);
        let structure = pce.picture_structure;
        let parity = if structure == 2 { 1 } else { 0 };
        // field pairing
        let mut second = None;
        if structure == 3 {
            if let Some(b) = self.building.take() {
                self.complete(b.frame, b.meta);
            }
        } else if let Some(b) = self.building.take() {
            if b.parity != parity && b.frame.w == w && b.frame.h == h {
                second = Some(b);
            } else {
                self.complete(b.frame, b.meta);
            }
        }
        // B pictures need their forward reference unless the GOP is closed
        if ptype == PictureType::B && second.is_none() && (self.bwd.is_none() || (self.fwd.is_none() && !self.closed_gop)) {
            return;
        }
        let is_second = second.is_some();
        let (mut frame, meta) = match second {
            Some(b) => (b.frame, b.meta),
            None => (
                Frame::new(w, h, cw, ch),
                Meta { pts, ptype, temporal_reference: pd.ph.temporal_reference, pce, field_pictures: structure != 3, info: seq.info.clone() },
            ),
        };
        // a P second field may predict from the first field of its own frame
        let first_field = (is_second && ptype == PictureType::P).then(|| frame.clone());
        // A missing reference (decoding started at a P picture, or a broken link) predicts from
        // mid-gray.
        if self.gray.as_ref().is_none_or(|g| g.w != w || g.h != h || g.cw != cw) {
            let mut g = Frame::new(w, h, cw, ch);
            g.planes[0].fill(128);
            self.gray = Some(Arc::new(g));
        }
        let gray = self.gray.as_deref();
        let latest = self.bwd.as_ref().map(|a| &*a.frame);
        let older = self.fwd.as_ref().map(|a| &*a.frame);
        let (fwd, bwd) = match ptype {
            PictureType::P => (latest.or(gray), None),
            PictureType::B => (older.or(latest).or(gray), latest.or(gray)),
            _ => (None, None),
        };
        let p = PicParams {
            mpeg2,
            ptype,
            pce,
            full_pel: [pd.ph.full_pel_forward_vector, pd.ph.full_pel_backward_vector],
            shx,
            shy,
            mb_width: seq.mb_width,
            mb_rows: if structure == 3 { seq.mb_height } else { seq.mb_height / 2 },
            matrices: self.matrices,
            fwd,
            bwd,
            first_field: first_field.as_ref(),
            parity,
            vertical_size_extension: seq.info.height > 2800,
        };
        let errors = SliceErrors::default();
        decode_picture(&p, data, &pd.slices, &mut frame, structure, self.threads, &errors);
        self.errors += errors.count.load(Ordering::Relaxed);
        if let Some(e) = errors.first.into_inner().unwrap_or(None) {
            self.last_error = Some(e);
        }
        if structure != 3 && !is_second {
            self.building = Some(Building { frame, parity, meta });
        } else {
            self.complete(frame, meta);
        }
    }

    fn complete(&mut self, frame: Frame, meta: Meta) {
        if meta.ptype.is_anchor() {
            if let Some(mut prev) = self.bwd.take() {
                if !prev.shown {
                    prev.shown = true;
                    self.push_output(to_picture(&prev.frame, &prev.meta));
                }
                self.fwd = Some(prev);
            }
            self.bwd = Some(Anchor { frame: Arc::new(frame), meta, shown: false });
        } else {
            self.push_output(to_picture(&frame, &meta));
        }
    }

    fn push_output(&mut self, picture: Picture) {
        if self.out.len() >= MAX_OUTPUT_PICTURES_PER_ACCESS_UNIT {
            self.output_overflow = true;
        } else {
            self.out.push(picture);
        }
    }
}

/// Slice errors of one picture (slices may decode on several threads).
#[derive(Default)]
struct SliceErrors {
    count: AtomicUsize,
    first: std::sync::Mutex<Option<&'static str>>,
}

impl SliceErrors {
    fn add(&self, what: &'static str) {
        self.count.fetch_add(1, Ordering::Relaxed);
        let mut f = self.first.lock().unwrap_or_else(|e| e.into_inner());
        f.get_or_insert(what);
    }
}

/// Decode the slices of one picture into `frame`.
fn decode_picture(p: &PicParams, data: &[u8], slices: &[(u8, std::ops::Range<usize>)], frame: &mut Frame, structure: u8, threads: bool, errors: &SliceErrors) {
    let (w, cw) = (frame.w, frame.cw);
    let field = (structure != 3).then_some(p.parity);
    let lines = if structure == 3 { 16 } else { 32 };
    let clines = lines >> p.shy;
    let [y, cb, cr] = &mut frame.planes;
    if !p.mpeg2 {
        // MPEG-1 slices may span rows: decode in order into the whole picture
        let mut t = Target { planes: [&mut y[..], &mut cb[..], &mut cr[..]], strides: [w, cw], line0: [0, 0], field };
        for (code, r) in slices {
            let payload = &data[r.clone()];
            if let Err(e) = decode_slice(p, payload, *code as usize - 1, false, &mut t) {
                errors.add(e.0);
            }
        }
        return;
    }
    let mut rows: Vec<Vec<&[u8]>> = vec![Vec::new(); p.mb_rows];
    for (code, r) in slices {
        let payload = &data[r.clone()];
        let row = slice_row(p.vertical_size_extension, *code, payload);
        match rows.get_mut(row) {
            Some(v) => v.push(payload),
            None => {
                errors.add("slice row out of range");
            }
        }
    }
    let run = |r: usize, y: &mut [u8], cb: &mut [u8], cr: &mut [u8]| {
        let Some(list) = rows.get(r) else { return };
        let mut t = Target { planes: [y, cb, cr], strides: [w, cw], line0: [r * lines, r * clines], field };
        for payload in list {
            if let Err(e) = decode_slice(p, payload, r, true, &mut t) {
                errors.add(e.0);
            }
        }
    };
    #[cfg(feature = "threads")]
    if threads && p.mb_rows >= 4 {
        use rayon::prelude::*;
        y.par_chunks_mut(lines * w)
            .zip(cb.par_chunks_mut(clines * cw))
            .zip(cr.par_chunks_mut(clines * cw))
            .enumerate()
            .for_each(|(r, ((y, cb), cr))| run(r, y, cb, cr));
        return;
    }
    let _ = threads;
    for (r, ((y, cb), cr)) in y.chunks_mut(lines * w).zip(cb.chunks_mut(clines * cw)).zip(cr.chunks_mut(clines * cw)).enumerate() {
        run(r, y, cb, cr);
    }
}

fn to_picture(f: &Frame, meta: &Meta) -> Picture {
    let info = &meta.info;
    let (w, h) = (info.width as usize, info.height as usize);
    let (cw, ch) = match info.chroma {
        ChromaFormat::Yuv420 => (w.div_ceil(2), h.div_ceil(2)),
        ChromaFormat::Yuv422 => (w.div_ceil(2), h),
        ChromaFormat::Yuv444 => (w, h),
    };
    let crop = |src: &[u8], stride: usize, w: usize, h: usize| {
        let mut v = Vec::with_capacity(w * h);
        for r in 0..h {
            v.extend_from_slice(&src[r * stride..r * stride + w]);
        }
        v
    };
    let progressive_frame = meta.pce.progressive_frame;
    Picture {
        width: w as u32,
        height: h as u32,
        chroma: info.chroma,
        y: crop(&f.planes[0], f.w, w, h),
        cb: crop(&f.planes[1], f.cw, cw, ch),
        cr: crop(&f.planes[2], f.cw, cw, ch),
        pts: meta.pts,
        picture_type: meta.ptype,
        temporal_reference: meta.temporal_reference,
        progressive_frame,
        // field pictures: the first coded field is displayed first
        top_field_first: if meta.field_pictures { meta.pce.picture_structure == 1 } else { meta.pce.top_field_first },
        repeat_first_field: meta.pce.repeat_first_field,
        field_pictures: meta.field_pictures,
        info: meta.info.clone(),
    }
}

#[cfg(test)]
mod synth_tests;
#[cfg(test)]
mod tests;
