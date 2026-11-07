//! Clean-room MPEG-4 Part 2 video (ISO/IEC 14496-2) decoder: DivX and Xvid.
//!
//! - Simple Profile: I and P VOPs, 4MV, H.263 and MPEG quantisation with custom matrices, AC/DC
//!   prediction, video packets (resync markers) and data partitioning, not-coded VOPs, half-sample
//!   motion compensation with unrestricted vectors and rounding control.
//! - Advanced Simple Profile: B-VOPs with direct mode, quarter-sample motion compensation, and the
//!   DivX "packed bitstream" (two VOPs in one AVI chunk).
//! - Output in display order as planar 8-bit 4:2:0 frames.
//!
//! Interlaced coding, GMC, sprites, reversible VLCs and the rest are errors that name the feature.
//!
//! ```no_run
//! let mut dec = filmcraft_mpeg4v::Decoder::new();
//! # let chunks: Vec<(Vec<u8>, i64)> = vec![];
//! for (chunk, pts) in chunks {
//!     for pic in dec.decode(&chunk, pts)? {
//!         println!("{}x{} {:?} pts {}", pic.width, pic.height, pic.vop_type, pic.pts);
//!     }
//! }
//! let rest = dec.flush();
//! # Ok::<(), filmcraft_mpeg4v::Error>(())
//! ```

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

mod bits;
mod headers;
mod idct;
mod mc;
mod vlc;
mod vop;

use std::sync::Arc;

pub use headers::VopType;

use headers::{
    GOV_START, ObjectHeader, VO_START, VOL_MAX, VOL_MIN, VOP_START, VOS_START, Vol, VopHeader, parse_gov, parse_vo, parse_vol, parse_vop, parse_vos,
};
use mc::Frame;
use vop::{Side, Vop};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid MPEG-4 video: {0}")]
    Invalid(String),
    #[error("unsupported MPEG-4 video: {0}")]
    Unsupported(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Stream-level properties (visual object and video object layer headers).
#[derive(Clone, Debug, PartialEq)]
pub struct StreamInfo {
    pub width: u32,
    pub height: u32,
    /// Sample aspect ratio.
    pub sar: (u32, u32),
    pub profile_and_level: Option<u8>,
    /// Visual object type of the layer (1 Simple, 17 Advanced Simple...).
    pub object_type: u8,
    /// No B-VOPs, when the layer says.
    pub low_delay: Option<bool>,
    /// Frames per second from vop_time_increment_resolution and a fixed VOP rate.
    pub frame_rate: Option<(u32, u32)>,
    /// (colour_primaries, transfer_characteristics, matrix_coefficients).
    pub colour: Option<(u8, u8, u8)>,
    pub full_range: bool,
    pub quarter_sample: bool,
    pub mpeg_quant: bool,
    /// The feature that keeps this layer from decoding.
    pub unsupported: Option<String>,
}

/// A decoded frame, cropped to the layer's size. Chroma is 4:2:0.
#[derive(Clone, Debug)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub y: Vec<u8>,
    pub cb: Vec<u8>,
    pub cr: Vec<u8>,
    /// The timestamp of the chunk the picture is shown for.
    pub pts: i64,
    pub vop_type: VopType,
    pub info: Arc<StreamInfo>,
}

impl Picture {
    pub fn chroma_width(&self) -> u32 {
        self.width.div_ceil(2)
    }
    pub fn chroma_height(&self) -> u32 {
        self.height.div_ceil(2)
    }
}

/// Start codes of `data`: (offset of the `00 00 01`, code).
pub fn start_codes(data: &[u8]) -> Vec<(usize, u8)> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while let Some(w) = data.get(i..i + 3) {
        if w[2] > 1 {
            i += 3;
        } else if w[0] == 0 && w[1] == 0 && w[2] == 1 {
            if let Some(&c) = data.get(i + 3) {
                out.push((i, c));
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    out
}

/// The prediction types of the VOPs in a chunk, in order. A truncated head of a chunk is enough
/// for the first.
pub fn vop_types(chunk: &[u8]) -> Vec<VopType> {
    start_codes(chunk)
        .iter()
        .filter(|c| c.1 == VOP_START)
        .filter_map(|c| {
            chunk.get(c.0 + 4).map(|b| match b >> 6 {
                0 => VopType::I,
                1 => VopType::P,
                2 => VopType::B,
                _ => VopType::S,
            })
        })
        .collect()
}

/// The first video object layer's properties in `data` (a chunk or the stream's extradata).
pub fn probe(data: &[u8]) -> Option<StreamInfo> {
    let codes = start_codes(data);
    let mut object = ObjectHeader::default();
    for (k, &(pos, code)) in codes.iter().enumerate() {
        let end = codes.get(k + 1).map_or(data.len(), |c| c.0);
        let body = data.get(pos + 4..end)?;
        match code {
            VOS_START => parse_vos(body, &mut object),
            VO_START => parse_vo(body, &mut object),
            VOL_MIN..=VOL_MAX => return parse_vol(body, &object).ok().map(|v| stream_info(&v, &object)),
            VOP_START => return None,
            _ => {}
        }
    }
    None
}

fn stream_info(v: &Vol, o: &ObjectHeader) -> StreamInfo {
    StreamInfo {
        width: v.width,
        height: v.height,
        sar: v.sar,
        profile_and_level: o.profile_and_level,
        object_type: v.object_type,
        low_delay: v.low_delay,
        frame_rate: v.fixed_increment.filter(|i| *i > 0).map(|i| (v.time_resolution, i)),
        colour: o.colour,
        full_range: o.full_range,
        quarter_sample: v.quarter_sample,
        mpeg_quant: v.mpeg_quant,
        unsupported: v.unsupported.map(str::to_string),
    }
}

/// Largest picture, in luma samples: 8K, far above the highest level of the visual profiles.
const MAX_SAMPLES: u64 = 8192 * 4320;

/// The active layer and what is sized by it.
struct Layer {
    vol: Vol,
    info: Arc<StreamInfo>,
    mb_w: usize,
    mb_h: usize,
}

/// A video decoder. Chunks go in in stream order; pictures come out in display order.
#[derive(Default)]
pub struct Decoder {
    object: ObjectHeader,
    layer: Option<Layer>,
    /// The newest anchors and the frame free for the next VOP.
    past: Frame,
    future: Frame,
    spare: Frame,
    /// Anchors decoded since the last start: B-VOPs need two.
    anchors: u32,
    side: Side,
    /// The anchor not shown yet: it follows the B-VOPs after it in the stream.
    held: Option<Picture>,
    /// B-VOPs since the last anchor, decoded or left out.
    b_since_anchor: u32,
    /// A chunk with two coded VOPs was seen: not-coded VOPs after B-VOPs show the held anchor.
    packed: bool,
    /// Times in seconds and ticks (§7.3): the time base, the one before, and the anchors' times.
    time_base: i64,
    last_time_base: i64,
    time_past: i64,
    time_future: i64,
    errors: u64,
    last_error: Option<String>,
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// The layer's properties once a header has been read.
    pub fn info(&self) -> Option<&Arc<StreamInfo>> {
        self.layer.as_ref().map(|l| &l.info)
    }

    /// Read headers from out-of-band data (AVI `strf` extradata, MP4 `esds`).
    pub fn configure(&mut self, extradata: &[u8]) -> Result<()> {
        let mut out = Vec::new();
        self.feed(extradata, 0, &mut out)
    }

    /// Forget pictures and references (a seek); the headers stay, since a key frame need not
    /// repeat them.
    pub fn reset(&mut self) {
        self.anchors = 0;
        self.held = None;
        self.b_since_anchor = 0;
        self.packed = false;
        self.time_base = 0;
        self.last_time_base = 0;
    }

    /// Decode one chunk. Damage within a VOP is concealed and counted ([`Self::errors`]), and so
    /// is a VOP header that makes no sense once there are pictures to repeat. An error here means
    /// nothing could be shown: no layer header, or a feature the decoder lacks.
    pub fn decode(&mut self, chunk: &[u8], pts: i64) -> Result<Vec<Picture>> {
        let mut out = Vec::new();
        match self.feed(chunk, pts, &mut out) {
            Err(e) if out.is_empty() => Err(e),
            _ => Ok(out),
        }
    }

    /// VOPs that came with damage, since the decoder was made.
    pub fn errors(&self) -> u64 {
        self.errors
    }

    /// What was wrong with the last of them.
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    fn count_error(&mut self, what: String) {
        self.errors += 1;
        self.last_error = Some(what);
    }

    /// The picture still held for reordering (end of stream).
    pub fn flush(&mut self) -> Vec<Picture> {
        self.held.take().into_iter().collect()
    }

    fn feed(&mut self, data: &[u8], pts: i64, out: &mut Vec<Picture>) -> Result<()> {
        let codes = start_codes(data);
        if codes.is_empty() && !data.is_empty() && self.layer.is_none() {
            return Err(Error::Invalid("no start codes: not MPEG-4 Part 2 (MS-MPEG4 or H.263 under a DivX tag?)".into()));
        }
        let mut coded = 0;
        for (k, &(pos, code)) in codes.iter().enumerate() {
            let end = codes.get(k + 1).map_or(data.len(), |c| c.0);
            let Some(body) = data.get(pos + 4..end) else { continue };
            match code {
                VOS_START => parse_vos(body, &mut self.object),
                VO_START => parse_vo(body, &mut self.object),
                VOL_MIN..=VOL_MAX => match parse_vol(body, &self.object) {
                    Ok(v) => self.set_layer(v, out)?,
                    // a corrupt repeat of the header: the one in use stands
                    Err(_) if self.layer.is_some() => {}
                    Err(e) => return Err(e),
                },
                GOV_START => self.time_base = i64::from(parse_gov(body)),
                VOP_START => {
                    coded += 1;
                    self.packed |= coded > 1;
                    self.vop(body, pts, out)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn set_layer(&mut self, vol: Vol, out: &mut Vec<Picture>) -> Result<()> {
        if self.layer.as_ref().is_some_and(|l| l.vol == vol) {
            return Ok(());
        }
        if u64::from(vol.width) * u64::from(vol.height) > MAX_SAMPLES {
            return Err(Error::Unsupported(format!("{}x{} video", vol.width, vol.height)));
        }
        let (mb_w, mb_h) = ((vol.width as usize).div_ceil(16), (vol.height as usize).div_ceil(16));
        let same_size = self.layer.as_ref().is_some_and(|l| (l.mb_w, l.mb_h) == (mb_w, mb_h));
        let info = Arc::new(stream_info(&vol, &self.object));
        self.layer = Some(Layer { vol, info, mb_w, mb_h });
        if !same_size {
            out.extend(self.held.take());
            self.anchors = 0;
            self.past = Frame::new(mb_w, mb_h);
            self.future = Frame::new(mb_w, mb_h);
            self.spare = Frame::new(mb_w, mb_h);
            self.side.resize(mb_w * mb_h);
        }
        Ok(())
    }

    fn vop(&mut self, body: &[u8], pts: i64, out: &mut Vec<Picture>) -> Result<()> {
        let Some(layer) = self.layer.take() else {
            return Err(Error::Invalid("VOP without a video object layer header".into()));
        };
        let r = self.vop_in(&layer, body, pts, out);
        self.layer = Some(layer);
        r
    }

    fn vop_in(&mut self, layer: &Layer, body: &[u8], pts: i64, out: &mut Vec<Picture>) -> Result<()> {
        if let Some(u) = layer.vol.unsupported {
            return Err(Error::Unsupported(u.into()));
        }
        let hdr = match parse_vop(body, &layer.vol) {
            Err(Error::Invalid(what)) if self.anchors > 0 => {
                self.count_error(what);
                self.not_coded(layer, pts, out);
                return Ok(());
            }
            r => r?,
        };
        let res = i64::from(layer.vol.time_resolution);
        let modulo = i64::from(hdr.modulo);
        if !hdr.coded {
            self.time_base = self.time_base.saturating_add(modulo);
            self.not_coded(layer, pts, out);
            return Ok(());
        }
        if hdr.kind == VopType::B {
            self.b_since_anchor += 1;
            if self.anchors < 2 {
                return Ok(());
            }
            let time = self.last_time_base.saturating_add(modulo).saturating_mul(res).saturating_add(i64::from(hdr.increment));
            let (trb, trd) = (time.saturating_sub(self.time_past), self.time_future.saturating_sub(self.time_past));
            let (trb, trd) = if trd > 0 && trb > 0 && trb < trd && trd <= 1 << 24 { (trb as i32, trd as i32) } else { (1, 2) };
            let mut cur = std::mem::take(&mut self.spare);
            self.decode_vop(layer, body, &hdr, &mut cur, trb, trd);
            out.push(crop(&cur, layer, pts, VopType::B));
            self.spare = cur;
            return Ok(());
        }
        // I or P
        self.last_time_base = self.time_base;
        self.time_base = self.time_base.saturating_add(modulo);
        if hdr.kind == VopType::P && self.anchors == 0 {
            return Ok(());
        }
        let time = self.time_base.saturating_mul(res).saturating_add(i64::from(hdr.increment));
        let mut cur = std::mem::take(&mut self.spare);
        self.decode_vop(layer, body, &hdr, &mut cur, 0, 1);
        std::mem::swap(&mut self.side.mvs, &mut self.side.col_mvs);
        std::mem::swap(&mut self.side.mode, &mut self.side.col_mode);
        out.extend(self.held.take());
        self.held = Some(crop(&cur, layer, pts, hdr.kind));
        self.b_since_anchor = 0;
        self.time_past = self.time_future;
        self.time_future = time;
        self.spare = std::mem::replace(&mut self.past, std::mem::replace(&mut self.future, cur));
        self.anchors = (self.anchors + 1).min(2);
        Ok(())
    }

    /// A not-coded VOP: the slot repeats the picture before it; in a packed stream it is the place
    /// of the anchor whose chunk showed a B-VOP.
    fn not_coded(&mut self, layer: &Layer, pts: i64, out: &mut Vec<Picture>) {
        if self.packed
            && self.b_since_anchor > 0
            && let Some(mut p) = self.held.take()
        {
            p.pts = pts;
            out.push(p);
            return;
        }
        out.extend(self.held.take());
        if self.anchors > 0 {
            out.push(crop(&self.future, layer, pts, VopType::P));
        }
    }

    fn decode_vop(&mut self, layer: &Layer, body: &[u8], hdr: &VopHeader, cur: &mut Frame, trb: i32, trd: i32) {
        let mut v = Vop::new(body, hdr, &layer.vol, (cur, &self.past, &self.future), &mut self.side, (layer.mb_w, layer.mb_h), (trb, trd));
        let r = v.run();
        let damaged = r.is_err() || v.damaged;
        if damaged && self.anchors > 0 {
            v.conceal();
        }
        if damaged {
            self.count_error(format!("damaged {:?}-VOP", hdr.kind));
        }
    }
}

/// Crop a frame to the layer's size.
fn crop(f: &Frame, layer: &Layer, pts: i64, kind: VopType) -> Picture {
    let (w, h) = (layer.vol.width as usize, layer.vol.height as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let tight = |s: &[u8], stride: usize, w: usize, h: usize| -> Vec<u8> {
        let mut o = Vec::with_capacity(w * h);
        for r in s.chunks_exact(stride.max(1)).take(h) {
            o.extend_from_slice(r.get(..w).unwrap_or(r));
        }
        o
    };
    Picture {
        width: layer.vol.width,
        height: layer.vol.height,
        y: tight(&f.y, f.w, w, h),
        cb: tight(&f.u, f.w / 2, cw, ch),
        cr: tight(&f.v, f.w / 2, cw, ch),
        pts,
        vop_type: kind,
        info: layer.info.clone(),
    }
}

#[cfg(test)]
mod synth_tests;
