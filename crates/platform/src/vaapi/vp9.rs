//! VP9 on VA-API: `filmcraft_vp9`'s accelerated front end ([`filmcraft_vp9::accel`]) with an
//! [`Accelerator`] that sends each frame to the GPU. Superframes, the uncompressed header, the
//! reference slots, `show_existing_frame` and the per-segment dequantizers and loop filter levels
//! are the software decoder's own; the GPU parses the compressed header and keeps the probability
//! contexts and the segmentation map.
//!
//! Profiles 0 (8-bit) and 2 (10-bit), 4:2:0. A frame whose size differs from the session's (VP9
//! can change size at an inter frame), a missing reference or a driver error fail over to software
//! in [`crate::HybridDecoder`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use filmcraft_codecs::hw::FrameStreamInfo;
use filmcraft_codecs::{CodecError, DecodedFrame, Result, VideoDecoder};
use filmcraft_vp9::accel::{AccelDecoder, AccelFrame, AccelOutput, Accelerator};

use super::device::{Display, Session};
use super::ffi::*;
use crate::biplanar::Geometry;

/// The eight reference slots, the frame being decoded and room for the one being read back.
const SURFACES: usize = 12;

fn profile_for(bits: u32) -> (VAProfile, u32) {
    if bits > 8 { (VAProfileVP9Profile2, VA_RT_FORMAT_YUV420_10) } else { (VAProfileVP9Profile0, VA_RT_FORMAT_YUV420) }
}

struct State {
    display: Option<Display>,
    session: Option<Session>,
    slots: HashMap<u32, usize>,
}

impl State {
    fn session_for(&mut self, bits: u32, w: u32, h: u32, key: bool) -> std::result::Result<(), String> {
        let fits = self.session.as_ref().is_some_and(|s| s.width == w && s.height == h && s.bits == bits);
        if fits {
            return Ok(());
        }
        // A new size or bit depth starts over, which only a key frame can do here (an inter
        // frame of another size would need its references scaled from the old surfaces).
        if !key && self.session.is_some() {
            return Err(format!("VP9 frame size changed to {w}x{h} at an inter frame"));
        }
        self.slots.clear();
        let (profile, rt) = profile_for(bits);
        let display = match self.display.take() {
            Some(d) => d,
            None => {
                self.session = None;
                Display::open_for(profile, rt)?
            }
        };
        self.session = None;
        self.session = Some(Session::new(display, profile, bits, w, h, SURFACES)?);
        Ok(())
    }
}

struct VaAccel {
    state: Arc<Mutex<State>>,
}

fn i16_of(v: i32) -> i16 {
    v.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

impl Accelerator for VaAccel {
    fn decode_frame(&mut self, f: &AccelFrame<'_>) -> std::result::Result<(), String> {
        let h = f.header;
        let c = &h.color;
        if !c.subsampling_x || !c.subsampling_y || !matches!(c.bit_depth, 8 | 10) || !matches!(h.profile, 0 | 2) {
            return Err("only 8- and 10-bit 4:2:0 VP9 (profiles 0 and 2) is decoded in hardware".into());
        }
        let (w, ht) = (u16::try_from(h.width).map_err(|_| "frame too wide")?, u16::try_from(h.height).map_err(|_| "frame too tall")?);
        let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        guard.session_for(u32::from(c.bit_depth), h.width, h.height, h.frame_is_intra)?;
        let st = &mut *guard;
        let session = st.session.as_ref().ok_or("no session")?;
        let mut reference_frames = [VA_INVALID_SURFACE; 8];
        for (r, id) in reference_frames.iter_mut().zip(&f.slots) {
            if let Some(s) = id.and_then(|id| st.slots.get(&id)).and_then(|s| session.surface_id(*s)) {
                *r = s;
            }
        }
        let used: Vec<usize> = st.slots.values().copied().collect();
        let slot = (0..session.surface_count()).find(|i| !used.contains(i)).ok_or("no free surface")?;
        st.slots.insert(f.id, slot);
        let seg = f.seg;
        let [last, golden, alt] = h.ref_frame_idx;
        let pic = VADecPictureParameterBufferVP9 {
            frame_width: w,
            frame_height: ht,
            reference_frames,
            pic_fields: pack_bits(&[
                (u32::from(c.subsampling_x), 1),
                (u32::from(c.subsampling_y), 1),
                (u32::from(h.frame_type != 0), 1),
                (u32::from(h.show_frame), 1),
                (u32::from(h.error_resilient_mode), 1),
                (u32::from(h.intra_only), 1),
                (u32::from(h.allow_high_precision_mv), 1),
                (u32::from(h.interp_filter), 3),
                (u32::from(h.frame_parallel_decoding_mode), 1),
                (u32::from(h.reset_frame_context), 2),
                (u32::from(h.refresh_frame_context), 1),
                (u32::from(h.frame_context_idx), 2),
                (u32::from(seg.enabled), 1),
                (u32::from(seg.temporal_update), 1),
                (u32::from(seg.update_map), 1),
                (u32::from(last), 3),
                (u32::from(h.ref_frame_sign_bias[1]), 1),
                (u32::from(golden), 3),
                (u32::from(h.ref_frame_sign_bias[2]), 1),
                (u32::from(alt), 3),
                (u32::from(h.ref_frame_sign_bias[3]), 1),
                (u32::from(h.lossless), 1),
            ]),
            filter_level: f.lf.level,
            sharpness_level: f.lf.sharpness,
            log2_tile_rows: u8::try_from(h.tile_rows_log2).unwrap_or(u8::MAX),
            log2_tile_columns: u8::try_from(h.tile_cols_log2).unwrap_or(u8::MAX),
            frame_header_length_in_bytes: u8::try_from(h.uncompressed_size).map_err(|_| "uncompressed header too long")?,
            first_partition_size: u16::try_from(h.header_size_in_bytes).map_err(|_| "compressed header too long")?,
            mb_segment_tree_probs: seg.tree_probs,
            segment_pred_probs: seg.pred_probs,
            profile: h.profile,
            bit_depth: c.bit_depth,
            va_reserved: [0; 8],
        };
        let seg_param = f.segments.map(|s| VASegmentParameterVP9 {
            segment_flags: pack_bits(&[(u32::from(s.reference.is_some()), 1), (u32::from(s.reference.unwrap_or(0)), 2), (u32::from(s.skip), 1)]) as u16,
            filter_level: s.filter_level,
            luma_ac_quant_scale: i16_of(s.luma_ac),
            luma_dc_quant_scale: i16_of(s.luma_dc),
            chroma_ac_quant_scale: i16_of(s.chroma_ac),
            chroma_dc_quant_scale: i16_of(s.chroma_dc),
            va_reserved: [0; 4],
        });
        let size = u32::try_from(f.data.len()).map_err(|_| "frame too large")?;
        let sp =
            VASliceParameterBufferVP9 { slice_data_size: size, slice_data_offset: 0, slice_data_flag: VA_SLICE_DATA_FLAG_ALL, seg_param, va_reserved: [0; 4] };
        let bufs = vec![session.params(VAPictureParameterBufferType, &[pic])?, session.params(VASliceParameterBufferType, &[sp])?, session.data(f.data)?];
        session.decode(slot, &bufs)
    }

    fn retain(&mut self, live: &[u32]) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.slots.retain(|id, _| live.contains(id));
    }
}

/// A VA-API VP9 decoder behind FilmCraft's [`VideoDecoder`].
pub struct VaVp9Decoder {
    dec: AccelDecoder,
    state: Arc<Mutex<State>>,
    info: FrameStreamInfo,
    /// Colour of the pictures since the last key frame, as the software decoder reports it.
    color: filmcraft_color::ColorInfo,
}

impl VaVp9Decoder {
    /// A decoder for the stream `info` describes; fails when this system cannot decode it in
    /// hardware.
    pub fn new(info: FrameStreamInfo) -> std::result::Result<Self, String> {
        let (profile, rt) = profile_for(info.bit_depth);
        let display = Display::open_for(profile, rt)?;
        let state = Arc::new(Mutex::new(State { display: Some(display), session: None, slots: HashMap::new() }));
        let dec = AccelDecoder::new(Box::new(VaAccel { state: state.clone() }));
        let color = info.color.unwrap_or(filmcraft_color::ColorInfo::REC709);
        Ok(VaVp9Decoder { dec, state, info, color })
    }

    fn frames(&mut self, outs: Vec<AccelOutput>) -> Result<Vec<DecodedFrame>> {
        let st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let mut v = Vec::with_capacity(outs.len());
        for o in outs {
            let session = st.session.as_ref().ok_or_else(|| CodecError::Decode("no hardware session".into()))?;
            let slot = st.slots.get(&o.id).copied().ok_or_else(|| CodecError::Decode(format!("frame {} has no surface", o.id)))?;
            let g = Geometry { crop: (0, 0, o.width, o.height), bits: u32::from(o.bit_depth), color: self.color, par: (1, 1) };
            let frame = session.read(slot, &g).map_err(CodecError::Decode)?;
            v.push(DecodedFrame { pts: o.pts, frame, draft: false });
        }
        Ok(v)
    }
}

impl VideoDecoder for VaVp9Decoder {
    fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
        if let Some(p) = self.info.picture_params(sample) {
            self.color = p.color;
        }
        let outs = self.dec.decode(sample, pts).map_err(|e| CodecError::Decode(e.to_string()))?;
        self.frames(outs)
    }

    fn flush(&mut self) -> Vec<DecodedFrame> {
        Vec::new()
    }

    fn reset(&mut self) {
        self.dec.reset();
        self.state.lock().unwrap_or_else(PoisonError::into_inner).slots.clear();
    }

    fn name(&self) -> &str {
        "VA-API VP9"
    }

    fn is_random_access(&self, sample: &[u8]) -> Option<bool> {
        Some(filmcraft_vp9::is_keyframe(sample))
    }
}
