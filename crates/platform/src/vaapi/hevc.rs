//! HEVC on VA-API: `filmcraft_hevc`'s decoder with an [`Accelerator`] that sends each picture to
//! the GPU instead of reconstructing it. Parameter sets, slice segment headers, picture order
//! counts, reference picture sets, reference lists, RASL handling and output order are the
//! software decoder's own; this module translates them into VA-API buffers and reads the decoded
//! surfaces back.
//!
//! Main and Main 10, 4:2:0. Streams with missing reference pictures or a driver error fail over to
//! software in [`crate::HybridDecoder`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use filmcraft_codecs::{CodecError, DecodedFrame, Result, VideoDecoder};
use filmcraft_hevc::accel::{AccelOutput, AccelPicture, AccelRef, Accelerator};

use super::device::{Display, Session};
use super::ffi::*;
use crate::biplanar::Geometry;

const EXTRA_SURFACES: usize = 3;

fn profile_for(bits: u32) -> (VAProfile, u32) {
    if bits > 8 { (VAProfileHEVCMain10, VA_RT_FORMAT_YUV420_10) } else { (VAProfileHEVCMain, VA_RT_FORMAT_YUV420) }
}

struct State {
    display: Option<Display>,
    session: Option<Session>,
    slots: HashMap<u32, usize>,
}

impl State {
    fn session_for(&mut self, bits: u32, w: u32, h: u32, dpb: usize) -> std::result::Result<(), String> {
        let want = (dpb + EXTRA_SURFACES).clamp(4, 32);
        let fits = self.session.as_ref().is_some_and(|s| s.width == w && s.height == h && s.bits == bits && s.surface_count() >= want);
        if !fits {
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
            self.session = Some(Session::new(display, profile, bits, w, h, want)?);
        }
        Ok(())
    }
}

struct VaAccel {
    state: Arc<Mutex<State>>,
}

fn clamp_i8(v: i32) -> i8 {
    v.clamp(i8::MIN as i32, i8::MAX as i32) as i8
}

fn u8_of(v: u32) -> u8 {
    v.min(u8::MAX as u32) as u8
}

impl Accelerator for VaAccel {
    fn decode_picture(&mut self, pic: &AccelPicture<'_>) -> std::result::Result<(), String> {
        let (sps, pps) = (pic.sps, pic.pps);
        if sps.chroma_format_idc != 1 || sps.bit_depth_luma != sps.bit_depth_chroma || !(8..=10).contains(&sps.bit_depth_luma) {
            return Err("only 8- and 10-bit 4:2:0 HEVC is decoded in hardware".into());
        }
        let bits = sps.bit_depth_luma;
        let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        guard.session_for(bits, sps.width, sps.height, sps.max_dec_pic_buffering as usize)?;
        let st = &mut *guard;
        let used: Vec<usize> = st.slots.values().copied().collect();
        let session = st.session.as_ref().ok_or("no session")?;
        let slot = (0..session.surface_count()).find(|i| !used.contains(i)).ok_or("no free surface")?;
        st.slots.insert(pic.id, slot);
        let cur = session.surface_id(slot).ok_or("no surface")?;

        // ReferenceFrames: every DPB reference, flagged with its RPS subset
        let in_set = |set: &[AccelRef], id: u32| set.iter().any(|r| r.id == id);
        let mut frames = [VAPictureHEVC::INVALID; 15];
        let mut index: HashMap<u32, u8> = HashMap::new();
        for (i, r) in pic.refs.iter().take(15).enumerate() {
            let Some(surface) = st.slots.get(&r.id).and_then(|s| session.surface_id(*s)) else { continue };
            let mut flags = 0;
            if r.long_term {
                flags |= VA_PICTURE_HEVC_LONG_TERM_REFERENCE;
            }
            if in_set(&pic.st_curr_before, r.id) {
                flags |= VA_PICTURE_HEVC_RPS_ST_CURR_BEFORE;
            } else if in_set(&pic.st_curr_after, r.id) {
                flags |= VA_PICTURE_HEVC_RPS_ST_CURR_AFTER;
            } else if in_set(&pic.lt_curr, r.id) {
                flags |= VA_PICTURE_HEVC_RPS_LT_CURR;
            }
            frames[i] = VAPictureHEVC { picture_id: surface, pic_order_cnt: r.poc, flags, va_reserved: [0; 4] };
            index.insert(r.id, i as u8);
        }
        let first = pic.slices.first().ok_or("picture without slices")?.header;
        let mut cols = [0u16; 19];
        let mut rows = [0u16; 21];
        if pps.tiles_enabled {
            for (d, w) in cols.iter_mut().zip(&pps.column_widths) {
                *d = w.saturating_sub(1).min(u16::MAX as u32) as u16;
            }
            for (d, h) in rows.iter_mut().zip(&pps.row_heights) {
                *d = h.saturating_sub(1).min(u16::MAX as u32) as u16;
            }
        }
        let b = |x: bool| (u32::from(x), 1);
        let pp = VAPictureParameterBufferHEVC {
            CurrPic: VAPictureHEVC { picture_id: cur, pic_order_cnt: pic.poc, flags: 0, va_reserved: [0; 4] },
            ReferenceFrames: frames,
            pic_width_in_luma_samples: sps.width.min(u16::MAX as u32) as u16,
            pic_height_in_luma_samples: sps.height.min(u16::MAX as u32) as u16,
            pic_fields: pack_bits(&[
                (sps.chroma_format_idc, 2),
                b(sps.separate_colour_plane),
                b(sps.pcm),
                b(sps.scaling_list_enabled),
                b(pps.transform_skip),
                b(sps.amp),
                b(sps.strong_intra_smoothing),
                b(pps.sign_data_hiding),
                b(pps.constrained_intra_pred),
                b(pps.cu_qp_delta_enabled),
                b(pps.weighted_pred),
                b(pps.weighted_bipred),
                b(pps.transquant_bypass),
                b(pps.tiles_enabled),
                b(pps.entropy_coding_sync),
                b(pps.loop_filter_across_slices),
                b(pps.loop_filter_across_tiles),
                b(sps.pcm_loop_filter_disabled),
                b(sps.max_num_reorder == 0),
                b(false),
            ]),
            sps_max_dec_pic_buffering_minus1: u8_of(sps.max_dec_pic_buffering.saturating_sub(1)),
            bit_depth_luma_minus8: u8_of(sps.bit_depth_luma.saturating_sub(8)),
            bit_depth_chroma_minus8: u8_of(sps.bit_depth_chroma.saturating_sub(8)),
            pcm_sample_bit_depth_luma_minus1: u8_of(sps.pcm_bit_depth_luma.saturating_sub(1)),
            pcm_sample_bit_depth_chroma_minus1: u8_of(sps.pcm_bit_depth_chroma.saturating_sub(1)),
            log2_min_luma_coding_block_size_minus3: u8_of(sps.log2_min_cb.saturating_sub(3)),
            log2_diff_max_min_luma_coding_block_size: u8_of(sps.log2_ctb.saturating_sub(sps.log2_min_cb)),
            log2_min_transform_block_size_minus2: u8_of(sps.log2_min_tb.saturating_sub(2)),
            log2_diff_max_min_transform_block_size: u8_of(sps.log2_max_tb.saturating_sub(sps.log2_min_tb)),
            log2_min_pcm_luma_coding_block_size_minus3: u8_of(sps.log2_min_pcm.saturating_sub(3)),
            log2_diff_max_min_pcm_luma_coding_block_size: u8_of(sps.log2_max_pcm.saturating_sub(sps.log2_min_pcm)),
            max_transform_hierarchy_depth_intra: u8_of(sps.max_th_depth_intra),
            max_transform_hierarchy_depth_inter: u8_of(sps.max_th_depth_inter),
            init_qp_minus26: clamp_i8(pps.init_qp - 26),
            diff_cu_qp_delta_depth: u8_of(pps.diff_cu_qp_delta_depth),
            pps_cb_qp_offset: clamp_i8(pps.cb_qp_offset),
            pps_cr_qp_offset: clamp_i8(pps.cr_qp_offset),
            log2_parallel_merge_level_minus2: u8_of(pps.log2_parallel_merge_level.saturating_sub(2)),
            num_tile_columns_minus1: u8_of(pps.num_tile_columns.saturating_sub(1)),
            num_tile_rows_minus1: u8_of(pps.num_tile_rows.saturating_sub(1)),
            column_width_minus1: cols,
            row_height_minus1: rows,
            slice_parsing_fields: pack_bits(&[
                b(pps.lists_modification_present),
                b(sps.long_term_refs_present),
                b(sps.temporal_mvp),
                b(pps.cabac_init_present),
                b(pps.output_flag_present),
                b(pps.dependent_slice_segments_enabled),
                b(pps.slice_chroma_qp_offsets_present),
                b(sps.sao),
                b(pps.deblocking_override_enabled),
                b(pps.deblocking_disabled),
                b(pps.slice_header_extension_present),
                b(pic.irap),
                b(pic.idr),
                b(pic.intra),
            ]),
            log2_max_pic_order_cnt_lsb_minus4: u8_of(sps.log2_max_poc_lsb.saturating_sub(4)),
            num_short_term_ref_pic_sets: u8_of(sps.st_rps.len() as u32),
            num_long_term_ref_pic_sps: u8_of(sps.lt_ref_pics.len() as u32),
            num_ref_idx_l0_default_active_minus1: u8_of(pps.num_ref_idx_l0_default.saturating_sub(1)),
            num_ref_idx_l1_default_active_minus1: u8_of(pps.num_ref_idx_l1_default.saturating_sub(1)),
            pps_beta_offset_div2: clamp_i8(pps.beta_offset_div2),
            pps_tc_offset_div2: clamp_i8(pps.tc_offset_div2),
            num_extra_slice_header_bits: u8_of(pps.num_extra_slice_header_bits),
            st_rps_bits: first.st_rps_bits.min(u32::MAX as usize) as u32,
            va_reserved: [0; 8],
        };
        let mut bufs = vec![session.params(VAPictureParameterBufferType, &[pp])?];
        if sps.scaling_list_enabled {
            let sl = pps.scaling_list.as_ref().or(sps.scaling_list.as_ref()).cloned().unwrap_or_else(filmcraft_hevc::params::ScalingList::default_lists);
            let mut iq = VAIQMatrixBufferHEVC {
                ScalingList4x4: [[0; 16]; 6],
                ScalingList8x8: sl.lists[1],
                ScalingList16x16: sl.lists[2],
                ScalingList32x32: [sl.lists[3][0], sl.lists[3][3]],
                ScalingListDC16x16: sl.dc[0],
                ScalingListDC32x32: [sl.dc[1][0], sl.dc[1][3]],
                va_reserved: [0; 4],
            };
            for (d, s) in iq.ScalingList4x4.iter_mut().zip(&sl.lists[0]) {
                d.copy_from_slice(s.get(..16).ok_or("scaling list")?);
            }
            bufs.push(session.params(VAIQMatrixBufferType, &[iq])?);
        }
        let n = pic.slices.len();
        for (k, s) in pic.slices.iter().enumerate() {
            let sh = s.header;
            let mut lists = [[0xFFu8; 15]; 2];
            for (l, list) in s.lists.iter().enumerate() {
                for (i, r) in list.iter().take(15).enumerate() {
                    lists[l][i] = index.get(&r.id).copied().ok_or("a reference is not in the DPB")?;
                }
            }
            let mut sp = VASliceParameterBufferHEVC {
                slice_data_size: u32::try_from(s.nal.len()).map_err(|_| "slice too large")?,
                slice_data_offset: 0,
                slice_data_flag: VA_SLICE_DATA_FLAG_ALL,
                slice_data_byte_offset: u32::try_from(s.data_byte_offset).map_err(|_| "slice header too long")?,
                slice_segment_address: sh.segment_address,
                RefPicList: lists,
                LongSliceFlags: pack_bits(&[
                    b(k + 1 == n),
                    b(sh.dependent),
                    (sh.slice_type as u32, 2),
                    (0, 2),
                    b(sh.sao_luma),
                    b(sh.sao_chroma),
                    b(sh.mvd_l1_zero),
                    b(sh.cabac_init_flag),
                    b(sh.temporal_mvp),
                    b(sh.deblocking_disabled),
                    b(sh.collocated_from_l0),
                    b(sh.loop_filter_across_slices),
                ]),
                collocated_ref_idx: if sh.temporal_mvp { u8_of(sh.collocated_ref_idx) } else { 0xFF },
                num_ref_idx_l0_active_minus1: u8_of(sh.num_ref_idx[0].saturating_sub(1)),
                num_ref_idx_l1_active_minus1: u8_of(sh.num_ref_idx[1].saturating_sub(1)),
                slice_qp_delta: clamp_i8(sh.qp_delta),
                slice_cb_qp_offset: clamp_i8(sh.cb_qp_offset),
                slice_cr_qp_offset: clamp_i8(sh.cr_qp_offset),
                slice_beta_offset_div2: clamp_i8(sh.beta_offset_div2),
                slice_tc_offset_div2: clamp_i8(sh.tc_offset_div2),
                luma_log2_weight_denom: 0,
                delta_chroma_log2_weight_denom: 0,
                delta_luma_weight_l0: [0; 15],
                luma_offset_l0: [0; 15],
                delta_chroma_weight_l0: [[0; 2]; 15],
                ChromaOffsetL0: [[0; 2]; 15],
                delta_luma_weight_l1: [0; 15],
                luma_offset_l1: [0; 15],
                delta_chroma_weight_l1: [[0; 2]; 15],
                ChromaOffsetL1: [[0; 2]; 15],
                five_minus_max_num_merge_cand: u8_of(5u32.saturating_sub(sh.max_num_merge_cand)),
                num_entry_point_offsets: sh.entry_points.len().min(u16::MAX as usize) as u16,
                entry_offset_to_subset_array: 0,
                slice_data_num_emu_prevn_bytes: s.header_emulation_bytes.min(u16::MAX as usize) as u16,
                va_reserved: [0; 2],
            };
            if let Some(w) = &sh.pwt {
                sp.luma_log2_weight_denom = u8_of(w.luma_log2_denom);
                sp.delta_chroma_log2_weight_denom = clamp_i8(w.chroma_log2_denom as i32 - w.luma_log2_denom as i32);
                let (dl, dc) = (1i32 << w.luma_log2_denom.min(7), 1i32 << w.chroma_log2_denom.min(7));
                for l in 0..2 {
                    let (dw, lo, dcw, co) = if l == 0 {
                        (&mut sp.delta_luma_weight_l0, &mut sp.luma_offset_l0, &mut sp.delta_chroma_weight_l0, &mut sp.ChromaOffsetL0)
                    } else {
                        (&mut sp.delta_luma_weight_l1, &mut sp.luma_offset_l1, &mut sp.delta_chroma_weight_l1, &mut sp.ChromaOffsetL1)
                    };
                    for (i, e) in w.l.get(l).into_iter().flatten().take(15).enumerate() {
                        if e.luma_flag {
                            dw[i] = clamp_i8(e.luma.0 - dl);
                            lo[i] = clamp_i8(e.luma.1);
                        }
                        if e.chroma_flag {
                            for c in 0..2 {
                                dcw[i][c] = clamp_i8(e.chroma[c].0 - dc);
                                co[i][c] = clamp_i8(e.chroma[c].1);
                            }
                        }
                    }
                }
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

/// A VA-API HEVC decoder behind FilmCraft's [`VideoDecoder`].
pub struct VaHevcDecoder {
    dec: filmcraft_hevc::Decoder,
    state: Arc<Mutex<State>>,
    hvcc: Vec<u8>,
}

impl VaHevcDecoder {
    /// A decoder for an `hvcC` stream of `bits` (8 or 10); fails when this system cannot decode it
    /// in hardware.
    pub fn new(hvcc: &[u8], bits: u32) -> std::result::Result<Self, String> {
        let (profile, rt) = profile_for(bits);
        let display = Display::open_for(profile, rt)?;
        let state = Arc::new(Mutex::new(State { display: Some(display), session: None, slots: HashMap::new() }));
        let mut dec = filmcraft_hevc::Decoder::with_accelerator(Box::new(VaAccel { state: state.clone() }));
        dec.configure_hvcc(hvcc).map_err(|e| e.to_string())?;
        Ok(VaHevcDecoder { dec, state, hvcc: hvcc.to_vec() })
    }

    fn frames(&mut self, outs: Vec<AccelOutput>) -> Result<Vec<DecodedFrame>> {
        let st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let mut v = Vec::with_capacity(outs.len());
        for o in outs {
            let session = st.session.as_ref().ok_or_else(|| CodecError::Decode("no hardware session".into()))?;
            let slot = st.slots.get(&o.id).copied().ok_or_else(|| CodecError::Decode(format!("picture {} has no surface", o.id)))?;
            let g = Geometry {
                crop: o.crop,
                bits: o.bit_depth,
                color: filmcraft_codecs::video::vui_color(o.crop.2, o.crop.3, o.matrix_coefficients, o.transfer_characteristics, o.full_range),
                par: filmcraft_codecs::video::sar_par(o.sar),
            };
            let frame = session.read(slot, &g).map_err(CodecError::Decode)?;
            v.push(DecodedFrame { pts: o.pts, frame, draft: false });
        }
        Ok(v)
    }
}

impl VideoDecoder for VaHevcDecoder {
    fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
        let outs = self.dec.decode_accel(sample, pts).map_err(|e| CodecError::Decode(e.to_string()))?;
        self.frames(outs)
    }

    fn flush(&mut self) -> Vec<DecodedFrame> {
        let outs = self.dec.flush_accel();
        self.frames(outs).unwrap_or_default()
    }

    fn reset(&mut self) {
        let mut dec = filmcraft_hevc::Decoder::with_accelerator(Box::new(VaAccel { state: self.state.clone() }));
        if dec.configure_hvcc(&self.hvcc).is_ok() {
            self.state.lock().unwrap_or_else(PoisonError::into_inner).slots.clear();
            self.dec = dec;
        }
    }

    fn name(&self) -> &str {
        "VA-API HEVC"
    }
}
