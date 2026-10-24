//! Hardware acceleration front end (stateless decoders such as VA-API).
//!
//! [`AccelDecoder`] does the part of decoding a stateless hardware decoder leaves to the
//! application, with the software decoder's own code: superframes, the uncompressed header
//! ([`crate::header`], including the loop filter and segmentation state carried from frame to
//! frame), the eight reference slots, `show_existing_frame`, and the per-segment dequantizers and
//! loop filter levels. Each coded frame goes to the [`Accelerator`] as an [`AccelFrame`]; the
//! hardware parses the compressed header and keeps the probability contexts and segmentation map
//! itself. Output is [`AccelOutput`]s naming frames by id; the accelerator owns the pixels.
//!
//! Streams the hardware cannot take as they are (a reference slot that was never filled, a
//! reference of another bit depth or chroma format) make it return an error, so that a caller can
//! switch to the software decoder.

use crate::decoder::segment_quantizers;
use crate::error::{Error, Result, ensure};
use crate::header::{HeaderState, KEY_FRAME, RefInfo, parse_uncompressed, split_superframe};
use crate::loopfilter::segment_filter_levels;
use crate::tables::{SEG_LVL_REF_FRAME, SEG_LVL_SKIP};

pub use crate::header::{ColorConfig, FrameHeader, LoopFilterParams, Segmentation};

/// What a frame's segment needs (VA-API `VASegmentParameterVP9`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SegmentParams {
    /// SEG_LVL_REF_FRAME: the reference frame every block of the segment uses.
    pub reference: Option<u8>,
    /// SEG_LVL_SKIP.
    pub skip: bool,
    /// Loop filter level per reference frame (intra, last, golden, altref) and mode (ZEROMV,
    /// other).
    pub filter_level: [[u8; 2]; 4],
    /// Dequantizers: luma DC, luma AC, chroma DC, chroma AC.
    pub luma_dc: i32,
    pub luma_ac: i32,
    pub chroma_dc: i32,
    pub chroma_ac: i32,
}

/// One coded frame.
pub struct AccelFrame<'a> {
    /// Id of the frame being decoded.
    pub id: u32,
    pub header: &'a FrameHeader,
    /// Loop filter and segmentation as in effect for this frame (carried from earlier frames).
    pub lf: &'a LoopFilterParams,
    pub seg: &'a Segmentation,
    pub segments: [SegmentParams; 8],
    /// The frame ids in the eight reference slots before this frame (`None`: empty slot).
    pub slots: [Option<u32>; 8],
    /// The whole frame, uncompressed header first.
    pub data: &'a [u8],
}

/// A frame leaving the decoder, in output order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccelOutput {
    pub id: u32,
    pub pts: i64,
    pub key: bool,
    pub width: u32,
    pub height: u32,
    pub bit_depth: u8,
    pub subsampling: (bool, bool),
    pub color_space: u8,
    pub full_range: bool,
}

/// A stateless hardware decoder.
pub trait Accelerator: Send {
    /// Decode `frame`. An error stops decoding with that error.
    fn decode_frame(&mut self, frame: &AccelFrame<'_>) -> std::result::Result<(), String>;
    /// After a frame, the ids still needed (the reference slots and the frame just output); any
    /// other frame's storage may be reused.
    fn retain(&mut self, live: &[u32]);
}

#[derive(Clone, Copy, Debug)]
struct Slot {
    id: u32,
    width: u32,
    height: u32,
    bit_depth: u8,
    ss: (bool, bool),
    color_space: u8,
    full_range: bool,
    key: bool,
}

impl Slot {
    fn output(&self, pts: i64) -> AccelOutput {
        AccelOutput {
            id: self.id,
            pts,
            key: self.key,
            width: self.width,
            height: self.height,
            bit_depth: self.bit_depth,
            subsampling: self.ss,
            color_space: self.color_space,
            full_range: self.full_range,
        }
    }
}

/// The VP9 decoding front end for an [`Accelerator`] (see the module documentation).
pub struct AccelDecoder {
    st: HeaderState,
    slots: [Option<Slot>; 8],
    next_id: u32,
    accel: Box<dyn Accelerator>,
}

impl AccelDecoder {
    pub fn new(accel: Box<dyn Accelerator>) -> Self {
        AccelDecoder { st: HeaderState::default(), slots: [None; 8], next_id: 0, accel }
    }

    /// Forget every reference and the carried header state (before decoding from another key
    /// frame after a seek).
    pub fn reset(&mut self) {
        self.st = HeaderState::default();
        self.slots = [None; 8];
        self.accel.retain(&[]);
    }

    /// Decode one chunk (a frame or a superframe); returns the frames it shows, in order, each
    /// with `pts`.
    pub fn decode(&mut self, data: &[u8], pts: i64) -> Result<Vec<AccelOutput>> {
        let mut out = Vec::new();
        for f in split_superframe(data) {
            if !f.is_empty() {
                out.extend(self.decode_frame(f, pts)?);
            }
        }
        Ok(out)
    }

    fn decode_frame(&mut self, data: &[u8], pts: i64) -> Result<Option<AccelOutput>> {
        let refs: [Option<RefInfo>; 8] = std::array::from_fn(|i| self.slots[i].map(|s| RefInfo { width: s.width, height: s.height }));
        let h = parse_uncompressed(data, &mut self.st, &refs)?;
        if h.show_existing_frame {
            let slot = self.slots[h.frame_to_show_map_idx as usize & 7]
                .ok_or_else(|| Error::MissingReference(format!("show_existing_frame of empty slot {}", h.frame_to_show_map_idx)))?;
            return Ok(Some(slot.output(pts)));
        }
        ensure!(h.width <= 16384 && h.height <= 16384, "frame size {}x{} too large", h.width, h.height);
        ensure!(h.uncompressed_size + h.header_size_in_bytes as usize <= data.len(), "compressed header exceeds frame data");
        if !h.frame_is_intra {
            for &i in &h.ref_frame_idx {
                let Some(r) = self.slots[i as usize & 7] else {
                    return Err(Error::MissingReference(format!("reference slot {i} is empty")));
                };
                ensure!(
                    r.bit_depth == h.color.bit_depth && r.ss == (h.color.subsampling_x, h.color.subsampling_y),
                    "reference frame format differs from the current frame"
                );
            }
        }
        let seg = &self.st.seg;
        let q = segment_quantizers(&h, seg);
        let lvl = segment_filter_levels(&self.st.lf, seg);
        let segments: [SegmentParams; 8] = std::array::from_fn(|s| {
            let active = |f: usize| seg.enabled && seg.feature_active(s as u8, f);
            SegmentParams {
                reference: active(SEG_LVL_REF_FRAME).then(|| seg.feature_data[s][SEG_LVL_REF_FRAME].clamp(0, 3) as u8),
                skip: active(SEG_LVL_SKIP),
                filter_level: lvl[s],
                luma_dc: q[s][0][0],
                luma_ac: q[s][0][1],
                chroma_dc: q[s][1][0],
                chroma_ac: q[s][1][1],
            }
        });
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let frame = AccelFrame { id, header: &h, lf: &self.st.lf, seg, segments, slots: std::array::from_fn(|i| self.slots[i].map(|s| s.id)), data };
        self.accel.decode_frame(&frame).map_err(Error::Invalid)?;
        let slot = Slot {
            id,
            width: h.width,
            height: h.height,
            bit_depth: h.color.bit_depth,
            ss: (h.color.subsampling_x, h.color.subsampling_y),
            color_space: h.color.color_space,
            full_range: h.color.color_range,
            key: h.frame_type == KEY_FRAME,
        };
        for (i, s) in self.slots.iter_mut().enumerate() {
            if (h.refresh_frame_flags >> i) & 1 == 1 {
                *s = Some(slot);
            }
        }
        let mut live: Vec<u32> = self.slots.iter().flatten().map(|s| s.id).collect();
        if h.show_frame {
            live.push(id);
        }
        self.accel.retain(&live);
        Ok(h.show_frame.then(|| slot.output(pts)))
    }
}
