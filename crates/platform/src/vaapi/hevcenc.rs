//! H.265 (HEVC) export encoding on VA-API. [`factory`] is registered with
//! `filmcraft_export::register_encoder`, and [`available`] as the format probe: H.265 has no
//! software encoder, so choosing the format is the opt-in (as with VideoToolbox), and the format is
//! offered only where a GPU can encode it.
//!
//! Main profile (8-bit 4:2:0 SDR), IDR + P pictures (no B-frames), one slice per picture, 64×64
//! coding tree blocks, picture order count `gop_index`. The VPS, SPS and PPS are written here
//! from the same values the driver is given and go into the `hvcC`; headers a driver may emit
//! itself are left out of the samples. A hardware failure in the middle of an export ends it with
//! an error.

use filmcraft_bitstream::{BitWriter, escape_rbsp, unescape_rbsp};
use filmcraft_export::{BitrateMode, EncodedPacket, EncoderFrame, ExportError, ExportSettings, FieldOrder, Format, Result, VideoEncoder};
use filmcraft_isobmff::{HevcConfig, HevcNalArray, SampleEntry};
use filmcraft_time::FrameRate;

use super::device::{Display, EncodeSession};
use super::ffi::*;

/// log2(MaxPicOrderCntLsb).
const LOG2_MAX_POC_LSB: u32 = 8;
const NAL_TRAIL_R: u8 = 1;
const NAL_IDR_W_RADL: u8 = 19;
const NAL_VPS: u8 = 32;
const NAL_SPS: u8 = 33;
const NAL_PPS: u8 = 34;

/// What the encoder was asked to do.
#[derive(Clone, Debug)]
struct Config {
    width: u32,
    height: u32,
    fps: (u32, u32),
    kbps: u32,
    max_kbps: u32,
    cbr: bool,
    keyint: u32,
    /// general_level_idc (30 × the level).
    level: u8,
    sar: (u16, u16),
}

/// The lowest Main tier level for a picture size, sample rate and bitrate (Annex A, tables A.8 /
/// A.9): (general_level_idc, MaxLumaPs, MaxLumaSr, MaxBR in kbit/s).
fn pick_level(w: u32, h: u32, fps: f64, kbps: u32) -> u8 {
    const LEVELS: [(u8, u64, u64, u32); 13] = [
        (30, 36_864, 552_960, 128),
        (60, 122_880, 3_686_400, 1_500),
        (63, 245_760, 7_372_800, 3_000),
        (90, 552_960, 16_588_800, 6_000),
        (93, 983_040, 33_177_600, 10_000),
        (120, 2_228_224, 66_846_720, 12_000),
        (123, 2_228_224, 133_693_440, 20_000),
        (150, 8_912_896, 267_386_880, 25_000),
        (153, 8_912_896, 534_773_760, 40_000),
        (156, 8_912_896, 1_069_547_520, 60_000),
        (180, 35_651_584, 1_069_547_520, 60_000),
        (183, 35_651_584, 2_139_095_040, 120_000),
        (186, 35_651_584, 4_278_190_080, 240_000),
    ];
    let ps = u64::from(w) * u64::from(h);
    let sr = (ps as f64 * fps).ceil() as u64;
    LEVELS.iter().find(|&&(_, mps, msr, br)| ps <= mps && sr <= msr && kbps <= br).map_or(186, |l| l.0)
}

/// Why the GPU does not take this export, or its configuration.
fn config(format: Format, w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> std::result::Result<Config, String> {
    if format != Format::Hevc || s.format.is_mxf() {
        return Err("only MP4 / MOV H.265 exports".into());
    }
    if s.signal.is_hdr() {
        return Err("HDR (Main 10)".into());
    }
    if s.bitrate_mode == BitrateMode::Vbr2Pass {
        return Err("two-pass VBR".into());
    }
    if s.field_order != FieldOrder::Progressive {
        return Err("interlaced output".into());
    }
    if w < 64 || h < 64 || w > 16384 || h > 16384 {
        return Err(format!("{w}x{h}"));
    }
    let (Ok(num), Ok(den)) = (u32::try_from(rate.num), u32::try_from(rate.den)) else { return Err("frame rate".into()) };
    if num == 0 || den == 0 || num > 0xffff || den > 0xffff {
        return Err("frame rate".into());
    }
    let kbps = s.bitrate_kbps.clamp(100, 4_000_000);
    let keyint = s.keyframe_distance.filter(|k| *k > 0).unwrap_or_else(|| (f64::from(num) / f64::from(den) * 2.0).round().max(1.0) as u32).min(10_000);
    let max_kbps = s.max_bitrate_kbps.filter(|m| *m >= kbps).unwrap_or(kbps.saturating_mul(3) / 2);
    Ok(Config {
        width: w,
        height: h,
        fps: (num, den),
        kbps,
        max_kbps,
        cbr: s.bitrate_mode == BitrateMode::Cbr,
        keyint,
        level: pick_level(w, h, f64::from(num) / f64::from(den), max_kbps),
        sar: s.pixel_aspect.map_or((1, 1), |(n, d)| (n.clamp(1, 65535) as u16, d.clamp(1, 65535) as u16)),
    })
}

struct VaHevcEncoder {
    session: EncodeSession,
    cfg: Config,
    vps: Vec<u8>,
    sps: Vec<u8>,
    pps: Vec<u8>,
    gop_index: u32,
    /// Reconstructed slot holding the previous picture, and its picture order count.
    prev: Option<(usize, i32)>,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

/// The coded picture size as the GPU encodes it, found by decoding its output: the width a whole
/// number of 64-wide coding tree blocks, the height of minimum coding blocks (8). The SPS crops it
/// to the exported size.
fn coded(w: u32, h: u32) -> (u32, u32) {
    (w.div_ceil(64) * 64, h.div_ceil(8) * 8)
}

fn va_picture(surface: VASurfaceID, poc: i32) -> VAPictureHEVC {
    VAPictureHEVC { picture_id: surface, pic_order_cnt: poc, flags: 0, va_reserved: [0; 4] }
}

impl VaHevcEncoder {
    fn new(cfg: Config) -> std::result::Result<Self, String> {
        let rc = if cfg.cbr { VA_RC_CBR } else { VA_RC_VBR };
        let display = Display::open_for_encode(VAProfileHEVCMain, rc)?;
        if let Ok(Some((mw, mh))) = display.max_encode_size(VAProfileHEVCMain)
            && (cfg.width > mw || cfg.height > mh)
        {
            return Err(format!("{}x{} (this GPU encodes up to {mw}x{mh})", cfg.width, cfg.height));
        }
        let (cw, ch) = coded(cfg.width, cfg.height);
        // input surfaces as wide as the coded picture, so its padding holds replicated edges
        let session = EncodeSession::new(display, VAProfileHEVCMain, rc, cw, ch, 2)?;
        let (vps, sps, pps) = parameter_sets(&cfg, cw, ch);
        Ok(VaHevcEncoder { session, cfg, vps, sps, pps, gop_index: 0, prev: None, y: Vec::new(), u: Vec::new(), v: Vec::new() })
    }

    fn encode_picture(&mut self) -> std::result::Result<(Vec<u8>, bool), String> {
        let idr = self.gop_index == 0 || self.prev.is_none();
        if idr {
            self.gop_index = 0;
            self.prev = None;
        }
        let poc = (self.gop_index & ((1 << LOG2_MAX_POC_LSB) - 1)) as i32;
        let slot = match self.prev {
            Some((p, _)) => (p + 1) % self.session.recon_count().max(1),
            None => 0,
        };
        let s = &self.session;
        let cur = s.recon(slot).ok_or("no reconstructed surface")?;
        let cfg = &self.cfg;
        let (num, den) = cfg.fps;
        let (cw, ch) = coded(cfg.width, cfg.height);
        let ctus = cw.div_ceil(64) * ch.div_ceil(64);
        let seq = VAEncSequenceParameterBufferHEVC {
            general_profile_idc: 1,
            general_level_idc: cfg.level,
            general_tier_flag: 0,
            intra_period: cfg.keyint,
            intra_idr_period: cfg.keyint,
            ip_period: 1,
            bits_per_second: cfg.kbps.saturating_mul(1000),
            pic_width_in_luma_samples: u16::try_from(cw).map_err(|_| "too wide")?,
            pic_height_in_luma_samples: u16::try_from(ch).map_err(|_| "too tall")?,
            seq_fields: pack_bits(&[(1, 2), (0, 1), (0, 3), (0, 3), (0, 1), (1, 1), (1, 1), (1, 1), (0, 1), (0, 1), (0, 1), (1, 1), (0, 1)]),
            log2_min_luma_coding_block_size_minus3: 0,
            log2_diff_max_min_luma_coding_block_size: 3,
            log2_min_transform_block_size_minus2: 0,
            log2_diff_max_min_transform_block_size: 3,
            max_transform_hierarchy_depth_inter: 3,
            max_transform_hierarchy_depth_intra: 3,
            pcm_sample_bit_depth_luma_minus1: 0,
            pcm_sample_bit_depth_chroma_minus1: 0,
            log2_min_pcm_luma_coding_block_size_minus3: 0,
            log2_max_pcm_luma_coding_block_size_minus3: 0,
            vui_parameters_present_flag: 1,
            vui_fields: pack_bits(&[(1, 1), (0, 1), (0, 1), (1, 1), (0, 1), (0, 1), (1, 1), (1, 1), (15, 5), (15, 5)]),
            aspect_ratio_idc: if cfg.sar == (1, 1) { 1 } else { 255 },
            sar_width: u32::from(cfg.sar.0),
            sar_height: u32::from(cfg.sar.1),
            vui_num_units_in_tick: den,
            vui_time_scale: num,
            min_spatial_segmentation_idc: 0,
            max_bytes_per_pic_denom: 0,
            max_bits_per_min_cu_denom: 0,
            scc_fields: 0,
            va_reserved: [0; 7],
        };
        let mut refs = [VAPictureHEVC::INVALID; 15];
        let mut list0 = [VAPictureHEVC::INVALID; 15];
        if let Some((p, ppoc)) = self.prev {
            let surface = s.recon(p).ok_or("no reference surface")?;
            refs[0] = va_picture(surface, ppoc);
            list0[0] = refs[0];
        }
        let pic = VAEncPictureParameterBufferHEVC {
            decoded_curr_pic: va_picture(cur, poc),
            reference_frames: refs,
            coded_buf: s.coded_buffer(),
            collocated_ref_pic_index: 0xff,
            last_picture: 0,
            pic_init_qp: 26,
            diff_cu_qp_delta_depth: 0,
            pps_cb_qp_offset: 0,
            pps_cr_qp_offset: 0,
            num_tile_columns_minus1: 0,
            num_tile_rows_minus1: 0,
            column_width_minus1: [0; 19],
            row_height_minus1: [0; 21],
            log2_parallel_merge_level_minus2: 0,
            ctu_max_bitsize_allowed: 0,
            num_ref_idx_l0_default_active_minus1: 0,
            num_ref_idx_l1_default_active_minus1: 0,
            slice_pic_parameter_set_id: 0,
            nal_unit_type: if idr { NAL_IDR_W_RADL } else { NAL_TRAIL_R },
            // idr, coding_type (1 I, 2 P), reference, …, cu_qp_delta_enabled,
            // …, pps_loop_filter_across_slices_enabled
            pic_fields: pack_bits(&[
                (u32::from(idr), 1),
                (if idr { 1 } else { 2 }, 3),
                (1, 1),
                (0, 1),
                (0, 1),
                (0, 1),
                (0, 1),
                (1, 1),
                (0, 1),
                (0, 1),
                (0, 1),
                (0, 1),
                (0, 1),
                (0, 1),
                (1, 1),
            ]),
            hierarchical_level_plus1: 0,
            va_byte_reserved: 0,
            scc_fields: 0,
            va_reserved: [0; 15],
        };
        let slice = VAEncSliceParameterBufferHEVC {
            slice_segment_address: 0,
            num_ctu_in_slice: ctus,
            slice_type: if idr { 2 } else { 1 },
            slice_pic_parameter_set_id: 0,
            num_ref_idx_l0_active_minus1: 0,
            num_ref_idx_l1_active_minus1: 0,
            ref_pic_list0: list0,
            ref_pic_list1: [VAPictureHEVC::INVALID; 15],
            luma_log2_weight_denom: 0,
            delta_chroma_log2_weight_denom: 0,
            delta_luma_weight_l0: [0; 15],
            luma_offset_l0: [0; 15],
            delta_chroma_weight_l0: [[0; 2]; 15],
            chroma_offset_l0: [[0; 2]; 15],
            delta_luma_weight_l1: [0; 15],
            luma_offset_l1: [0; 15],
            delta_chroma_weight_l1: [[0; 2]; 15],
            chroma_offset_l1: [[0; 2]; 15],
            max_num_merge_cand: 5,
            slice_qp_delta: 0,
            slice_cb_qp_offset: 0,
            slice_cr_qp_offset: 0,
            slice_beta_offset_div2: 0,
            slice_tc_offset_div2: 0,
            // last_slice_of_pic, …, slice_sao_luma, slice_sao_chroma, …,
            // slice_loop_filter_across_slices_enabled, collocated_from_l0
            slice_fields: pack_bits(&[(1, 1), (0, 1), (0, 2), (0, 1), (1, 1), (1, 1), (0, 1), (0, 1), (0, 1), (0, 2), (1, 1), (1, 1)]),
            pred_weight_table_bit_offset: 0,
            pred_weight_table_bit_length: 0,
            va_reserved: [0; 6],
        };
        let bps = cfg.kbps.saturating_mul(1000);
        let rc = VAEncMisc {
            type_: VAEncMiscParameterTypeRateControl,
            data: VAEncMiscParameterRateControl {
                bits_per_second: if cfg.cbr { bps } else { cfg.max_kbps.saturating_mul(1000) },
                target_percentage: if cfg.cbr { 100 } else { (u64::from(cfg.kbps) * 100 / u64::from(cfg.max_kbps.max(1))).clamp(1, 100) as u32 },
                window_size: 1500,
                initial_qp: 0,
                min_qp: 0,
                max_qp: 51,
                ..Default::default()
            },
        };
        let fr = VAEncMisc { type_: VAEncMiscParameterTypeFrameRate, data: VAEncMiscParameterFrameRate { framerate: (den << 16) | num, ..Default::default() } };
        let hrd = VAEncMisc {
            type_: VAEncMiscParameterTypeHRD,
            data: VAEncMiscParameterHRD {
                buffer_size: cfg.max_kbps.saturating_mul(1000).saturating_mul(2),
                initial_buffer_fullness: cfg.max_kbps.saturating_mul(1000),
                ..Default::default()
            },
        };
        let mut bufs = Vec::with_capacity(6);
        if idr {
            bufs.push(s.params(VAEncSequenceParameterBufferType, &seq)?);
            bufs.push(s.params(VAEncMiscParameterBufferType, &rc)?);
            bufs.push(s.params(VAEncMiscParameterBufferType, &fr)?);
            bufs.push(s.params(VAEncMiscParameterBufferType, &hrd)?);
        }
        bufs.push(s.params(VAEncPictureParameterBufferType, &pic)?);
        bufs.push(s.params(VAEncSliceParameterBufferType, &slice)?);
        s.encode(&bufs)?;
        drop(bufs);
        let coded = s.coded_bytes()?;
        let prev_poc = self.prev.map_or(0, |p| p.1);
        let mut bytes = Vec::with_capacity(coded.len() + 16);
        for nal in filmcraft_bitstream::annexb_nals(&coded) {
            let t = nal.first().map_or(0, |h| (h >> 1) & 0x3f);
            let nal = if matches!(t, NAL_TRAIL_R | NAL_IDR_W_RADL) { rewrite_slice(nal, idr, poc, prev_poc)? } else { nal.to_vec() };
            bytes.extend_from_slice(&[0, 0, 0, 1]);
            bytes.extend_from_slice(&nal);
        }
        self.prev = Some((slot, poc));
        self.gop_index += 1;
        if self.gop_index >= self.cfg.keyint {
            self.gop_index = 0;
        }
        Ok((bytes, idr))
    }

    /// Length-prefixed sample from an Annex-B access unit; VPS / SPS / PPS / AUD / SEI are taken
    /// out (the `hvcC` carries ours).
    fn sample(annexb: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(annexb.len() + 16);
        for nal in filmcraft_bitstream::annexb_nals(annexb) {
            let Some(&h) = nal.first() else { continue };
            if !matches!((h >> 1) & 0x3f, 32..=35 | 39 | 40) {
                out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
                out.extend_from_slice(nal);
            }
        }
        out
    }
}

impl VideoEncoder for VaHevcEncoder {
    fn sample_entry(&self) -> SampleEntry {
        let (w, h) = (u16::try_from(self.cfg.width).unwrap_or(u16::MAX), u16::try_from(self.cfg.height).unwrap_or(u16::MAX));
        let array = |nal_type: u8, nal: &[u8]| HevcNalArray { completeness: true, nal_type, nalus: vec![nal.to_vec()] };
        let cfg = HevcConfig {
            general_profile_space: 0,
            general_tier_flag: false,
            general_profile_idc: 1,
            general_profile_compatibility_flags: PROFILE_COMPATIBILITY,
            general_constraint_indicator_flags: CONSTRAINT_FLAGS,
            general_level_idc: self.cfg.level,
            min_spatial_segmentation_idc: 0,
            parallelism_type: 0,
            chroma_format_idc: 1,
            bit_depth_luma: 8,
            bit_depth_chroma: 8,
            avg_frame_rate: 0,
            constant_frame_rate: 0,
            num_temporal_layers: 1,
            temporal_id_nested: true,
            length_size: 4,
            arrays: vec![array(NAL_VPS, &self.vps), array(NAL_SPS, &self.sps), array(NAL_PPS, &self.pps)],
        };
        SampleEntry::hevc(cfg, w, h)
    }

    fn timescale(&self) -> u32 {
        self.cfg.fps.0
    }

    fn encode(&mut self, f: &EncoderFrame) -> Result<Vec<EncodedPacket>> {
        if f.width != self.cfg.width || f.height != self.cfg.height {
            return Err(ExportError::Encode(format!("VA-API: a {}x{} picture for a {}x{} encoder", f.width, f.height, self.cfg.width, self.cfg.height)));
        }
        let (w, h) = (self.cfg.width as usize, self.cfg.height as usize);
        filmcraft_export::rgba_to_yuv420_8(f.rgba, w, h, &mut self.y, &mut self.u, &mut self.v);
        let fail = |e: String| ExportError::Encode(format!("VA-API H.265: {e}"));
        self.session.upload(&self.y, &self.u, &self.v, w, h).map_err(fail)?;
        let (bytes, key) = self.encode_picture().map_err(fail)?;
        let data = Self::sample(&bytes);
        if data.is_empty() {
            return Err(ExportError::Encode("VA-API H.265: the driver returned no picture data".into()));
        }
        filmcraft_export::note_hw_encode_frame();
        Ok(vec![EncodedPacket { data, key, duration: self.cfg.fps.1, composition_offset: 0 }])
    }

    fn flush(&mut self) -> Result<Vec<EncodedPacket>> {
        Ok(Vec::new())
    }
}

/// general_profile_compatibility_flag[1] and [2] (Main is decodable by Main 10 decoders), as a
/// 32-bit field with flag 0 in the top bit.
const PROFILE_COMPATIBILITY: u32 = 0x6000_0000;
/// general_progressive_source_flag and general_frame_only_constraint_flag (the top 48 bits:
/// progressive, interlaced, non_packed, frame_only, then 44 zero bits).
const CONSTRAINT_FLAGS: u64 = 0x9000_0000_0000;

/// profile_tier_level( 1, 0 ) (7.3.3).
fn profile_tier_level(w: &mut BitWriter, level: u8) {
    w.write_bits(0, 2); // general_profile_space
    w.write_bit(false); // general_tier_flag
    w.write_bits(1, 5); // general_profile_idc: Main
    w.write_bits(PROFILE_COMPATIBILITY, 32);
    w.write_bits((CONSTRAINT_FLAGS >> 32) as u32, 16);
    w.write_bits(CONSTRAINT_FLAGS as u32, 32);
    w.write_bits(u32::from(level), 8);
}

fn nal(nal_type: u8, rbsp: &[u8]) -> Vec<u8> {
    let mut out = vec![nal_type << 1, 1]; // forbidden 0, layer 0, temporal_id_plus1 1
    out.extend_from_slice(&escape_rbsp(rbsp));
    out
}

/// VPS, SPS and PPS NAL units matching what the driver is told (see the module documentation).
fn parameter_sets(cfg: &Config, coded_w: u32, coded_h: u32) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    // video_parameter_set_rbsp (7.3.2.1)
    let mut v = BitWriter::new();
    v.write_bits(0, 4); // vps_video_parameter_set_id
    v.write_bit(true); // vps_base_layer_internal_flag
    v.write_bit(true); // vps_base_layer_available_flag
    v.write_bits(0, 6); // vps_max_layers_minus1
    v.write_bits(0, 3); // vps_max_sub_layers_minus1
    v.write_bit(true); // vps_temporal_id_nesting_flag
    v.write_bits(0xffff, 16); // vps_reserved_0xffff_16bits
    profile_tier_level(&mut v, cfg.level);
    v.write_bit(true); // vps_sub_layer_ordering_info_present_flag
    v.write_ue(1); // vps_max_dec_pic_buffering_minus1
    v.write_ue(0); // vps_max_num_reorder_pics
    v.write_ue(0); // vps_max_latency_increase_plus1
    v.write_bits(0, 6); // vps_max_layer_id
    v.write_ue(0); // vps_num_layer_sets_minus1
    v.write_bit(true); // vps_timing_info_present_flag
    v.write_bits(cfg.fps.1, 32); // vps_num_units_in_tick
    v.write_bits(cfg.fps.0, 32); // vps_time_scale
    v.write_bit(false); // vps_poc_proportional_to_timing_flag
    v.write_ue(0); // vps_num_hrd_parameters
    v.write_bit(false); // vps_extension_flag
    v.rbsp_trailing();

    // seq_parameter_set_rbsp (7.3.2.2)
    let mut s = BitWriter::new();
    s.write_bits(0, 4); // sps_video_parameter_set_id
    s.write_bits(0, 3); // sps_max_sub_layers_minus1
    s.write_bit(true); // sps_temporal_id_nesting_flag
    profile_tier_level(&mut s, cfg.level);
    s.write_ue(0); // sps_seq_parameter_set_id
    s.write_ue(1); // chroma_format_idc: 4:2:0
    s.write_ue(coded_w);
    s.write_ue(coded_h);
    let (crop_r, crop_b) = (coded_w - cfg.width, coded_h - cfg.height);
    s.write_bit(crop_r > 0 || crop_b > 0); // conformance_window_flag
    if crop_r > 0 || crop_b > 0 {
        // in chroma sample units (SubWidthC = SubHeightC = 2)
        s.write_ue(0);
        s.write_ue(crop_r / 2);
        s.write_ue(0);
        s.write_ue(crop_b / 2);
    }
    s.write_ue(0); // bit_depth_luma_minus8
    s.write_ue(0); // bit_depth_chroma_minus8
    s.write_ue(LOG2_MAX_POC_LSB - 4);
    s.write_bit(true); // sps_sub_layer_ordering_info_present_flag
    s.write_ue(1); // sps_max_dec_pic_buffering_minus1
    s.write_ue(0); // sps_max_num_reorder_pics
    s.write_ue(0); // sps_max_latency_increase_plus1
    s.write_ue(0); // log2_min_luma_coding_block_size_minus3: 8x8
    s.write_ue(3); // log2_diff_max_min_luma_coding_block_size: 64x64 CTBs
    s.write_ue(0); // log2_min_luma_transform_block_size_minus2: 4x4
    s.write_ue(3); // log2_diff_max_min_luma_transform_block_size: 32x32
    // The GPU's coding tools, found by decoding its output (the driver reports none of them):
    // transform trees up to depth 3 for inter CUs (it splits that deep), intra CUs never split
    // below the CU (depths 1-3 decode the same; 0 does not), AMP, SAO, strong intra smoothing,
    // CU QP deltas, no sign data hiding or transform skip.
    s.write_ue(3); // max_transform_hierarchy_depth_inter
    s.write_ue(3); // max_transform_hierarchy_depth_intra
    s.write_bit(false); // scaling_list_enabled_flag
    s.write_bit(true); // amp_enabled_flag
    s.write_bit(true); // sample_adaptive_offset_enabled_flag
    s.write_bit(false); // pcm_enabled_flag
    s.write_ue(0); // num_short_term_ref_pic_sets (each slice carries its own)
    s.write_bit(false); // long_term_ref_pics_present_flag
    s.write_bit(false); // sps_temporal_mvp_enabled_flag (this GPU has none)
    s.write_bit(true); // strong_intra_smoothing_enabled_flag
    s.write_bit(true); // vui_parameters_present_flag
    // vui_parameters (E.2.1): BT.709 limited range, as the software encoders signal it
    let color = filmcraft_h264enc::ColorConfig::default();
    let sar = cfg.sar != (1, 1);
    s.write_bit(true); // aspect_ratio_info_present_flag
    if sar {
        s.write_bits(255, 8);
        s.write_bits(u32::from(cfg.sar.0), 16);
        s.write_bits(u32::from(cfg.sar.1), 16);
    } else {
        s.write_bits(1, 8);
    }
    s.write_bit(false); // overscan_info_present_flag
    s.write_bit(true); // video_signal_type_present_flag
    s.write_bits(5, 3); // video_format: unspecified
    s.write_bit(color.full_range);
    s.write_bit(true); // colour_description_present_flag
    s.write_bits(u32::from(color.primaries), 8);
    s.write_bits(u32::from(color.transfer), 8);
    s.write_bits(u32::from(color.matrix), 8);
    s.write_bit(false); // chroma_loc_info_present_flag
    s.write_bit(false); // neutral_chroma_indication_flag
    s.write_bit(false); // field_seq_flag
    s.write_bit(false); // frame_field_info_present_flag
    s.write_bit(false); // default_display_window_flag
    s.write_bit(true); // vui_timing_info_present_flag
    s.write_bits(cfg.fps.1, 32);
    s.write_bits(cfg.fps.0, 32);
    s.write_bit(false); // vui_poc_proportional_to_timing_flag
    s.write_bit(false); // vui_hrd_parameters_present_flag
    s.write_bit(false); // bitstream_restriction_flag
    s.write_bit(false); // sps_extension_present_flag
    s.rbsp_trailing();

    // pic_parameter_set_rbsp (7.3.2.3)
    let mut p = BitWriter::new();
    p.write_ue(0); // pps_pic_parameter_set_id
    p.write_ue(0); // pps_seq_parameter_set_id
    p.write_bit(false); // dependent_slice_segments_enabled_flag
    p.write_bit(false); // output_flag_present_flag
    p.write_bits(0, 3); // num_extra_slice_header_bits
    p.write_bit(false); // sign_data_hiding_enabled_flag
    p.write_bit(false); // cabac_init_present_flag
    p.write_ue(0); // num_ref_idx_l0_default_active_minus1
    p.write_ue(0); // num_ref_idx_l1_default_active_minus1
    p.write_se(0); // init_qp_minus26
    p.write_bit(false); // constrained_intra_pred_flag
    p.write_bit(false); // transform_skip_enabled_flag
    p.write_bit(true); // cu_qp_delta_enabled_flag
    p.write_ue(0); // diff_cu_qp_delta_depth
    p.write_se(0); // pps_cb_qp_offset
    p.write_se(0); // pps_cr_qp_offset
    p.write_bit(false); // pps_slice_chroma_qp_offsets_present_flag
    p.write_bit(false); // weighted_pred_flag
    p.write_bit(false); // weighted_bipred_flag
    p.write_bit(false); // transquant_bypass_enabled_flag
    p.write_bit(false); // tiles_enabled_flag
    p.write_bit(false); // entropy_coding_sync_enabled_flag
    p.write_bit(true); // pps_loop_filter_across_slices_enabled_flag
    p.write_bit(false); // deblocking_filter_control_present_flag
    p.write_bit(false); // pps_scaling_list_data_present_flag
    p.write_bit(false); // lists_modification_present_flag
    p.write_ue(0); // log2_parallel_merge_level_minus2
    p.write_bit(false); // slice_segment_header_extension_present_flag
    p.write_bit(false); // pps_extension_present_flag
    p.rbsp_trailing();
    (nal(NAL_VPS, &v.finish()), nal(NAL_SPS, &s.finish()), nal(NAL_PPS, &p.finish()))
}

/// What the driver's own slice header says that the slice data depends on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DriverSlice {
    sao: (bool, bool),
    /// num_ref_idx_l0_active_minus1 when the header overrides the PPS default.
    l0_override: Option<u32>,
    five_minus_max_num_merge_cand: u32,
    qp_delta: i32,
    /// Bytes of the RBSP the header takes (it ends byte-aligned; slice data follows).
    len: usize,
}

/// Read the slice_segment_header() a driver wrote for one of our pictures. Mesa's radeonsi writes
/// a 4-bit slice_pic_order_cnt_lsb and an empty inline reference picture set, neither of which a
/// decoder can use with our SPS; everything else follows our SPS / PPS. Anything unexpected is an
/// error (never a broken file).
fn read_driver_slice(rbsp: &[u8], idr: bool) -> std::result::Result<DriverSlice, String> {
    use filmcraft_bitstream::BitReader;
    let bad = |what: &str| format!("unexpected slice header from the driver ({what})");
    let mut r = BitReader::new(rbsp);
    let e = |_| bad("truncated");
    if !r.read_flag().map_err(e)? {
        return Err(bad("not the first slice"));
    }
    if idr {
        r.read_flag().map_err(e)?; // no_output_of_prior_pics_flag
    }
    if r.read_ue().map_err(e)? != 0 {
        return Err(bad("PPS id"));
    }
    if r.read_ue().map_err(e)? != if idr { 2 } else { 1 } {
        return Err(bad("slice type"));
    }
    if !idr {
        r.read_bits(4).map_err(e)?; // slice_pic_order_cnt_lsb (the driver's)
        if r.read_flag().map_err(e)? {
            return Err(bad("RPS from the SPS"));
        }
        let (neg, pos) = (r.read_ue().map_err(e)?, r.read_ue().map_err(e)?);
        if neg > 16 || pos > 16 {
            return Err(bad("RPS size"));
        }
        for _ in 0..neg + pos {
            r.read_ue().map_err(e)?;
            r.read_flag().map_err(e)?;
        }
    }
    let sao = (r.read_flag().map_err(e)?, r.read_flag().map_err(e)?);
    let mut l0_override = None;
    let mut five_minus_max_num_merge_cand = 0;
    if !idr {
        if r.read_flag().map_err(e)? {
            l0_override = Some(r.read_ue().map_err(e)?);
        }
        five_minus_max_num_merge_cand = r.read_ue().map_err(e)?;
        if five_minus_max_num_merge_cand > 4 || l0_override.is_some_and(|n| n > 14) {
            return Err(bad("merge candidates"));
        }
    }
    let qp_delta = r.read_se().map_err(e)?;
    if !(-26..=25).contains(&qp_delta) {
        return Err(bad("QP"));
    }
    r.read_flag().map_err(e)?; // slice_loop_filter_across_slices_enabled_flag
    // byte_alignment(): a one bit, then zeros to the byte boundary
    if !r.read_flag().map_err(e)? {
        return Err(bad("alignment"));
    }
    while !r.is_byte_aligned() {
        if r.read_flag().map_err(e)? {
            return Err(bad("alignment"));
        }
    }
    Ok(DriverSlice { sao, l0_override, five_minus_max_num_merge_cand, qp_delta, len: r.byte_pos() })
}

/// slice_segment_header() (7.3.6.1) for the SPS / PPS of [`parameter_sets`], including
/// byte_alignment(): the picture order count and the reference picture set (the previous picture)
/// are ours, the coding choices the driver's.
fn slice_header(idr: bool, poc: i32, prev_poc: i32, d: &DriverSlice) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.write_bit(true); // first_slice_segment_in_pic_flag
    if idr {
        w.write_bit(false); // no_output_of_prior_pics_flag
    }
    w.write_ue(0); // slice_pic_parameter_set_id
    w.write_ue(if idr { 2 } else { 1 }); // slice_type: I / P
    if !idr {
        w.write_bits(poc as u32 & ((1 << LOG2_MAX_POC_LSB) - 1), LOG2_MAX_POC_LSB);
        w.write_bit(false); // short_term_ref_pic_set_sps_flag
        // st_ref_pic_set( 0 ): the previous picture, used by this one
        w.write_ue(1); // num_negative_pics
        w.write_ue(0); // num_positive_pics
        w.write_ue((poc - prev_poc - 1).max(0) as u32); // delta_poc_s0_minus1
        w.write_bit(true); // used_by_curr_pic_s0_flag
    }
    w.write_bit(d.sao.0);
    w.write_bit(d.sao.1);
    if !idr {
        w.write_bit(d.l0_override.is_some()); // num_ref_idx_active_override_flag
        if let Some(n) = d.l0_override {
            w.write_ue(n);
        }
        w.write_ue(d.five_minus_max_num_merge_cand);
    }
    w.write_se(d.qp_delta);
    w.write_bit(true); // slice_loop_filter_across_slices_enabled_flag
    w.rbsp_trailing(); // byte_alignment()
    w.finish()
}

/// The driver's slice NAL unit with our slice header in place of its own (see
/// [`read_driver_slice`]); the slice data is untouched.
fn rewrite_slice(nal: &[u8], idr: bool, poc: i32, prev_poc: i32) -> std::result::Result<Vec<u8>, String> {
    let (Some(head), Some(payload)) = (nal.get(..2), nal.get(2..)) else { return Err("empty slice from the driver".into()) };
    let rbsp = unescape_rbsp(payload);
    let d = read_driver_slice(&rbsp, idr)?;
    let mut out_rbsp = slice_header(idr, poc, prev_poc, &d);
    out_rbsp.extend_from_slice(rbsp.get(d.len..).unwrap_or_default());
    let mut out = head.to_vec();
    out.extend_from_slice(&escape_rbsp(&out_rbsp));
    Ok(out)
}

/// Whether this system can export H.265 (the format probe; asked once, the Export panel asks
/// often).
pub fn available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| Display::open_for_encode(VAProfileHEVCMain, VA_RC_VBR).is_ok())
}

/// The Export encoder factory (see the module documentation).
pub fn factory(format: Format, w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>> {
    if format != Format::Hevc {
        return None;
    }
    let cfg = match config(format, w, h, rate, s) {
        Ok(c) => c,
        Err(why) => return Some(Err(ExportError::Encode(format!("H.265 on this GPU: {why}")))),
    };
    match VaHevcEncoder::new(cfg) {
        Ok(enc) => {
            filmcraft_export::note_hw_encode_session();
            Some(Ok(Box::new(enc)))
        }
        Err(why) => Some(Err(ExportError::Encode(format!("H.265 on this GPU: {why}")))),
    }
}

#[cfg(test)]
mod tests {
    use super::pick_level;

    #[test]
    fn levels() {
        assert_eq!(pick_level(1920, 1080, 30.0, 8_000), 120);
        assert_eq!(pick_level(1920, 1080, 60.0, 8_000), 123);
        assert_eq!(pick_level(3840, 2160, 30.0, 20_000), 150);
        assert_eq!(pick_level(3840, 2160, 60.0, 30_000), 153);
        assert_eq!(pick_level(640, 360, 24.0, 2_000), 63);
    }
}
