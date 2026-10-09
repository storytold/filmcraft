//! H.264 on VA-API: `filmcraft_h264`'s decoder with an [`Accelerator`] that sends each picture
//! to the GPU instead of reconstructing it. Parameter sets, slice headers, picture order counts,
//! reference marking, reference lists and output order are the software decoder's own; this
//! module only translates them into VA-API buffers and reads the decoded surfaces back.
//!
//! 8-bit 4:2:0 progressive streams (Constrained Baseline, Main, High); everything else declines
//! at session creation or fails over to software in [`crate::HybridDecoder`] (field pictures,
//! frame_num gaps, a driver error).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use filmcraft_codecs::{CodecError, DecodedFrame, Result, VideoDecoder};
use filmcraft_h264::accel::{AccelOutput, AccelPicture, AccelRef, Accelerator};

use super::device::{Display, Session};
use super::ffi::*;
use crate::biplanar::Geometry;

/// Surfaces beyond the DPB: the picture being decoded and one waiting for output.
const EXTRA_SURFACES: usize = 3;

/// The GPU side: the session and which surface holds which picture.
struct State {
    display: Option<Display>,
    session: Option<Session>,
    slots: HashMap<u32, usize>,
}

impl State {
    fn session_for(&mut self, w: u32, h: u32, dpb: usize) -> std::result::Result<&Session, String> {
        let want = (dpb + EXTRA_SURFACES).clamp(4, 32);
        let fits = self.session.as_ref().is_some_and(|s| s.width == w && s.height == h && s.surface_count() >= want);
        if !fits {
            self.slots.clear();
            // a new session needs the display back: take it from the old session or the spare
            let display = match (self.session.take(), self.display.take()) {
                (_, Some(d)) => d,
                (Some(_old), None) => Display::open_for(VAProfileH264High, VA_RT_FORMAT_YUV420)?,
                (None, None) => Display::open_for(VAProfileH264High, VA_RT_FORMAT_YUV420)?,
            };
            self.session = Some(Session::new(display, VAProfileH264High, 8, w, h, want)?);
        }
        self.session.as_ref().ok_or_else(|| "no session".to_string())
    }
}

struct VaAccel {
    state: Arc<Mutex<State>>,
}

fn va_ref(r: &AccelRef, slots: &HashMap<u32, usize>, s: &Session) -> VAPictureH264 {
    let Some(surface) = slots.get(&r.id).and_then(|i| s.surface_id(*i)) else { return VAPictureH264::INVALID };
    VAPictureH264 {
        picture_id: surface,
        frame_idx: if r.long_term { r.long_term_frame_idx } else { r.frame_num },
        flags: if r.long_term { VA_PICTURE_H264_LONG_TERM_REFERENCE } else { VA_PICTURE_H264_SHORT_TERM_REFERENCE },
        TopFieldOrderCnt: r.poc,
        BottomFieldOrderCnt: r.poc,
        va_reserved: [0; 4],
    }
}

impl Accelerator for VaAccel {
    fn decode_picture(&mut self, pic: &AccelPicture<'_>) -> std::result::Result<(), String> {
        let sps = pic.sps;
        let pps = pic.pps;
        if sps.chroma_format_idc != 1 || sps.bit_depth_luma != 8 || sps.bit_depth_chroma != 8 || !sps.frame_mbs_only {
            return Err("only 8-bit 4:2:0 progressive H.264 is decoded in hardware".into());
        }
        let (w, h) = (sps.pic_width_in_mbs.saturating_mul(16), sps.frame_height_in_mbs().saturating_mul(16));
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.session_for(w, h, sps.max_dpb_frames())?;
        // a surface for this picture: the first one no live picture holds
        let used: Vec<usize> = st.slots.values().copied().collect();
        let st = &mut *st;
        let session = st.session.as_ref().ok_or("no session")?;
        let slot = (0..session.surface_count()).find(|i| !used.contains(i)).ok_or("no free surface")?;
        st.slots.insert(pic.id, slot);
        let cur = session.surface_id(slot).ok_or("no surface")?;

        let mut refs = [VAPictureH264::INVALID; 16];
        for (dst, r) in refs.iter_mut().zip(&pic.refs) {
            *dst = va_ref(r, &st.slots, session);
        }
        let pp = VAPictureParameterBufferH264 {
            CurrPic: VAPictureH264 {
                picture_id: cur,
                frame_idx: pic.frame_num,
                flags: 0,
                TopFieldOrderCnt: pic.poc.0,
                BottomFieldOrderCnt: pic.poc.1,
                va_reserved: [0; 4],
            },
            ReferenceFrames: refs,
            picture_width_in_mbs_minus1: sps.pic_width_in_mbs.saturating_sub(1).min(u16::MAX as u32) as u16,
            picture_height_in_mbs_minus1: sps.frame_height_in_mbs().saturating_sub(1).min(u16::MAX as u32) as u16,
            bit_depth_luma_minus8: 0,
            bit_depth_chroma_minus8: 0,
            num_ref_frames: sps.max_num_ref_frames.min(16) as u8,
            seq_fields: h264_seq_fields(
                sps.chroma_format_idc,
                sps.separate_colour_plane,
                sps.gaps_in_frame_num_allowed,
                sps.frame_mbs_only,
                sps.mb_adaptive_frame_field,
                sps.direct_8x8_inference,
                sps.level_idc >= 31,
                sps.log2_max_frame_num.saturating_sub(4),
                sps.pic_order_cnt_type,
                sps.log2_max_poc_lsb.saturating_sub(4),
                sps.delta_pic_order_always_zero,
            ),
            num_slice_groups_minus1: 0,
            slice_group_map_type: 0,
            slice_group_change_rate_minus1: 0,
            pic_init_qp_minus26: (pps.pic_init_qp - 26).clamp(-128, 127) as i8,
            pic_init_qs_minus26: (pps.pic_init_qs - 26).clamp(-128, 127) as i8,
            chroma_qp_index_offset: pps.chroma_qp_index_offset.clamp(-128, 127) as i8,
            second_chroma_qp_index_offset: pps.second_chroma_qp_index_offset.clamp(-128, 127) as i8,
            pic_fields: h264_pic_fields(
                pps.entropy_coding_mode,
                pps.weighted_pred,
                pps.weighted_bipred_idc,
                pps.transform_8x8_mode,
                false,
                pps.constrained_intra_pred,
                pps.bottom_field_pic_order_in_frame_present,
                pps.deblocking_filter_control_present,
                pps.redundant_pic_cnt_present,
                pic.reference,
            ),
            frame_num: pic.frame_num.min(u16::MAX as u32) as u16,
            va_reserved: [0; 8],
        };
        let iq = VAIQMatrixBufferH264 { ScalingList4x4: pps.scaling.m4, ScalingList8x8: [pps.scaling.m8[0], pps.scaling.m8[1]], va_reserved: [0; 4] };

        let pp_buf = session.params(VAPictureParameterBufferType, &[pp])?;
        let iq_buf = session.params(VAIQMatrixBufferType, &[iq])?;
        let mut bufs = vec![pp_buf, iq_buf];
        for s in &pic.slices {
            let sh = s.header;
            let mut sp = VASliceParameterBufferH264 {
                slice_data_size: u32::try_from(s.nal.len()).map_err(|_| "slice too large")?,
                slice_data_offset: 0,
                slice_data_flag: VA_SLICE_DATA_FLAG_ALL,
                slice_data_bit_offset: u16::try_from(s.data_bit_offset).map_err(|_| "slice header too long")?,
                first_mb_in_slice: u16::try_from(sh.first_mb_in_slice).map_err(|_| "first_mb_in_slice out of range")?,
                slice_type: (sh.slice_type_raw % 5) as u8,
                direct_spatial_mv_pred_flag: u8::from(sh.direct_spatial_mv_pred),
                num_ref_idx_l0_active_minus1: sh.num_ref_idx_active[0].saturating_sub(1).min(31) as u8,
                num_ref_idx_l1_active_minus1: sh.num_ref_idx_active[1].saturating_sub(1).min(31) as u8,
                cabac_init_idc: sh.cabac_init_idc.min(2) as u8,
                slice_qp_delta: sh.slice_qp_delta.clamp(-128, 127) as i8,
                disable_deblocking_filter_idc: sh.disable_deblocking_filter_idc.min(2) as u8,
                slice_alpha_c0_offset_div2: sh.slice_alpha_c0_offset_div2.clamp(-6, 6) as i8,
                slice_beta_offset_div2: sh.slice_beta_offset_div2.clamp(-6, 6) as i8,
                RefPicList0: [VAPictureH264::INVALID; 32],
                RefPicList1: [VAPictureH264::INVALID; 32],
                luma_log2_weight_denom: 0,
                chroma_log2_weight_denom: 0,
                luma_weight_l0_flag: 0,
                luma_weight_l0: [0; 32],
                luma_offset_l0: [0; 32],
                chroma_weight_l0_flag: 0,
                chroma_weight_l0: [[0; 2]; 32],
                chroma_offset_l0: [[0; 2]; 32],
                luma_weight_l1_flag: 0,
                luma_weight_l1: [0; 32],
                luma_offset_l1: [0; 32],
                chroma_weight_l1_flag: 0,
                chroma_weight_l1: [[0; 2]; 32],
                chroma_offset_l1: [[0; 2]; 32],
                va_reserved: [0; 4],
            };
            for (dst, r) in sp.RefPicList0.iter_mut().zip(&s.lists[0]) {
                *dst = va_ref(r, &st.slots, session);
            }
            for (dst, r) in sp.RefPicList1.iter_mut().zip(&s.lists[1]) {
                *dst = va_ref(r, &st.slots, session);
            }
            if let Some(w) = &sh.pred_weight_table {
                sp.luma_log2_weight_denom = w.luma_log2_denom.min(7) as u8;
                sp.chroma_log2_weight_denom = w.chroma_log2_denom.min(7) as u8;
                let (dl, dc) = (1i32 << sp.luma_log2_weight_denom, 1i32 << sp.chroma_log2_weight_denom);
                let fill = |e: &[filmcraft_h264::slice::WeightEntry],
                            lw: &mut [i16; 32],
                            lo: &mut [i16; 32],
                            cw: &mut [[i16; 2]; 32],
                            co: &mut [[i16; 2]; 32]|
                 -> (u8, u8) {
                    let (mut lf, mut cf) = (0u8, 0u8);
                    for (i, x) in e.iter().take(32).enumerate() {
                        lf |= u8::from(x.luma_flag);
                        cf |= u8::from(x.chroma_flag);
                        lw[i] = if x.luma_flag { x.luma_weight.clamp(-128, 127) as i16 } else { dl as i16 };
                        lo[i] = if x.luma_flag { x.luma_offset.clamp(-128, 127) as i16 } else { 0 };
                        for c in 0..2 {
                            cw[i][c] = if x.chroma_flag { x.chroma_weight[c].clamp(-128, 127) as i16 } else { dc as i16 };
                            co[i][c] = if x.chroma_flag { x.chroma_offset[c].clamp(-128, 127) as i16 } else { 0 };
                        }
                    }
                    (lf, cf)
                };
                let (lf, cf) = fill(&w.l0, &mut sp.luma_weight_l0, &mut sp.luma_offset_l0, &mut sp.chroma_weight_l0, &mut sp.chroma_offset_l0);
                sp.luma_weight_l0_flag = lf;
                sp.chroma_weight_l0_flag = cf;
                let (lf, cf) = fill(&w.l1, &mut sp.luma_weight_l1, &mut sp.luma_offset_l1, &mut sp.chroma_weight_l1, &mut sp.chroma_offset_l1);
                sp.luma_weight_l1_flag = lf;
                sp.chroma_weight_l1_flag = cf;
            }
            bufs.push(session.params(VASliceParameterBufferType, &[sp])?);
            bufs.push(session.data(s.nal)?);
        }
        session.decode(slot, &bufs)
    }

    fn retain(&mut self, live: &[u32]) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.slots.retain(|id, _| live.contains(id));
    }
}

/// A VA-API H.264 decoder behind FilmCraft's [`VideoDecoder`].
pub struct VaH264Decoder {
    dec: filmcraft_h264::Decoder,
    state: Arc<Mutex<State>>,
    avcc: Vec<u8>,
}

impl VaH264Decoder {
    /// A decoder for an `avcC` stream; fails when this system cannot decode H.264 in hardware.
    pub fn new(avcc: &[u8]) -> std::result::Result<Self, String> {
        let display = Display::open_for(VAProfileH264High, VA_RT_FORMAT_YUV420)?;
        let state = Arc::new(Mutex::new(State { display: Some(display), session: None, slots: HashMap::new() }));
        let mut dec = filmcraft_h264::Decoder::with_accelerator(Box::new(VaAccel { state: state.clone() }));
        dec.configure_avcc(avcc).map_err(|e| e.to_string())?;
        Ok(VaH264Decoder { dec, state, avcc: avcc.to_vec() })
    }

    fn frames(&mut self, outs: Vec<AccelOutput>) -> Result<Vec<DecodedFrame>> {
        let st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let mut v = Vec::with_capacity(outs.len());
        for o in outs {
            let session = st.session.as_ref().ok_or_else(|| CodecError::Decode("no hardware session".into()))?;
            let slot = st.slots.get(&o.id).copied().ok_or_else(|| CodecError::Decode(format!("picture {} has no surface", o.id)))?;
            let g = Geometry {
                crop: o.crop,
                bits: 8,
                color: filmcraft_codecs::video::vui_color(o.crop.2, o.crop.3, o.matrix_coefficients, o.transfer_characteristics, o.full_range),
                par: filmcraft_codecs::video::sar_par(o.sar),
            };
            let frame = session.read(slot, &g).map_err(CodecError::Decode)?;
            v.push(DecodedFrame { pts: o.pts, frame, draft: false });
        }
        Ok(v)
    }
}

impl VideoDecoder for VaH264Decoder {
    fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
        let outs = self.dec.decode_accel(sample, pts).map_err(|e| CodecError::Decode(e.to_string()))?;
        self.frames(outs)
    }

    fn flush(&mut self) -> Vec<DecodedFrame> {
        let outs = self.dec.flush_accel();
        self.frames(outs).unwrap_or_default()
    }

    fn reset(&mut self) {
        // keep the GPU session; restart the bitstream state
        let mut dec = filmcraft_h264::Decoder::with_accelerator(Box::new(VaAccel { state: self.state.clone() }));
        if dec.configure_avcc(&self.avcc).is_ok() {
            self.state.lock().unwrap_or_else(PoisonError::into_inner).slots.clear();
            self.dec = dec;
        }
    }

    fn name(&self) -> &str {
        "VA-API H.264"
    }
}
