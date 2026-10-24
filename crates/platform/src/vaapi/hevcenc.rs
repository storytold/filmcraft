//! HEVC Main 8-bit export on the shared VA display. No software codec fallback.
//! Packed headers follow H.265 (09/2023). Driver-emitted parameter sets are validated for hvc1.

use super::va::{EncodeSession, HevcEncodeCaps, HevcPicture, Misc};
use super::{ffi, va};
use crate::nvenc::{self, Codec};
use filmcraft_export::{
    BitrateMode, EncodedPacket, EncoderFrame, ExportError, ExportSettings, FieldOrder, Format, H264Pass, HardwareEncoding, Result, VideoEncoder,
};
use filmcraft_isobmff::{HevcConfig, SampleEntry};
use filmcraft_time::FrameRate;
use std::sync::OnceLock;

#[derive(Clone, Debug)]
struct Config {
    width: u32,
    height: u32,
    fps: (u32, u32),
    kbps: u32,
    peak: u32,
    keyint: u32,
    cbr: bool,
    level: u8,
}

fn config(w: u32, h: u32, rate: FrameRate, s: &ExportSettings, caps: &HevcEncodeCaps) -> std::result::Result<Config, String> {
    if s.format != Format::Hevc || s.signal.is_hdr() || s.format.is_mxf() {
        return Err("HEVC Main accepts SDR MP4/MOV only".into());
    }
    if s.field_order != FieldOrder::Progressive || s.pixel_aspect.is_some_and(|(n, d)| n != d) {
        return Err("HEVC Main requires progressive square pixels".into());
    }
    if matches!(s.bitrate_mode, BitrateMode::Crf | BitrateMode::Vbr2Pass) || !matches!(s.h264_pass, H264Pass::Single) {
        return Err("HEVC Main requires single-pass CBR or VBR".into());
    }
    let (mw, mh) = caps.limits.max_size.unwrap_or((4096, 4096));
    if w == 0 || h == 0 || w > mw.min(16384) || h > mh.min(16384) || !w.is_multiple_of(2) || !h.is_multiple_of(2) {
        return Err(format!("HEVC Main needs even dimensions, at most {mw}x{mh}: {w}x{h}"));
    }
    let coded = (w.div_ceil(64) * 64, h.div_ceil(64) * 64);
    if coded.0 > mw.min(16384) || coded.1 > mh.min(16384) {
        return Err(format!("HEVC coded dimensions {}x{} exceed {mw}x{mh}", coded.0, coded.1));
    }
    let (Ok(num), Ok(den)) = (u32::try_from(rate.num), u32::try_from(rate.den)) else { return Err("invalid HEVC frame rate".into()) };
    if num == 0 || den == 0 || num > 65535 || den > 65535 {
        return Err("HEVC frame rate must fit the VA rational frame-rate field".into());
    }
    let cbr = s.bitrate_mode == BitrateMode::Cbr;
    let rc = if cbr { ffi::VA_RC_CBR } else { ffi::VA_RC_VBR };
    if caps.limits.rate_control & rc == 0 {
        return Err("HEVC driver lacks requested rate control".into());
    }
    if s.bitrate_kbps == 0 || s.bitrate_kbps > 240000 {
        return Err("HEVC Main tier bitrate outside 1..240000 kbps".into());
    }
    let kbps = s.bitrate_kbps;
    let peak = if cbr { kbps } else { s.max_bitrate_kbps.unwrap_or(kbps.saturating_mul(3) / 2).max(kbps) };
    let level =
        nvenc::hevc::main_tier_level(coded.0, coded.1, (num, den), peak).and_then(nvenc::hevc::level_code).ok_or("no HEVC Main tier level fits this export")?;
    let keyint = s.keyframe_distance.unwrap_or_else(|| num.saturating_mul(2).div_ceil(den)).clamp(1, 10000);
    Ok(Config { width: w, height: h, fps: (num, den), kbps, peak, keyint, cbr, level })
}

/// Whether a frame is what the session takes: SDR RGBA at the configured size. HDR input never
/// reaches a session (`config` refuses an HDR signal; this refuses HDR frames).
fn frame_admissible(c: &Config, f: &EncoderFrame) -> bool {
    f.width == c.width && f.height == c.height && f.hdr.is_none() && f.rgba.len() == c.width as usize * c.height as usize * 4
}

/// H.265 (09/2023) §§7.3.2.1–3 and E.2.1. These values also populate the VA buffers.
fn parameter_sets(c: &Config, coded: (u32, u32)) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    use filmcraft_bitstream::{BitWriter, escape_rbsp};
    fn ptl(w: &mut BitWriter, level: u8) {
        w.write_bits(1, 8); // profile space 0, Main tier, Main profile
        w.write_bits(1 << 30, 32); // Main compatibility
        w.write_bits(0xb, 4); // progressive, non-packed, frame-only
        w.write_bits(0, 32);
        w.write_bits(0, 12);
        w.write_bits(u32::from(level), 8);
    }
    fn nal(kind: u8, mut w: BitWriter) -> Vec<u8> {
        w.rbsp_trailing();
        let mut out = vec![kind << 1, 1];
        out.extend_from_slice(&escape_rbsp(&w.finish()));
        out
    }
    let mut v = BitWriter::new();
    v.write_bits(0, 4);
    v.write_bits(3, 2);
    v.write_bits(0, 6);
    v.write_bits(0, 3);
    v.write_bit(true);
    v.write_bits(0xffff, 16);
    ptl(&mut v, c.level);
    v.write_bit(false);
    v.write_ue(1);
    v.write_ue(0);
    v.write_ue(0); // two DPB slots, no reordering
    v.write_bits(0, 6);
    v.write_ue(0);
    v.write_bit(false);
    v.write_bit(false);

    let mut s = BitWriter::new();
    s.write_bits(0, 4);
    s.write_bits(0, 3);
    s.write_bit(true);
    ptl(&mut s, c.level);
    s.write_ue(0);
    s.write_ue(1);
    s.write_ue(coded.0);
    s.write_ue(coded.1);
    let crop = coded != (c.width, c.height);
    s.write_bit(crop);
    if crop {
        s.write_ue(0);
        s.write_ue((coded.0 - c.width) / 2);
        s.write_ue(0);
        s.write_ue((coded.1 - c.height) / 2);
    }
    s.write_ue(0);
    s.write_ue(0);
    s.write_ue(4); // 8-bit, eight POC LSB bits
    s.write_bit(false);
    s.write_ue(1);
    s.write_ue(0);
    s.write_ue(0);
    s.write_ue(0);
    s.write_ue(3); // coding blocks 8..64
    s.write_ue(0);
    s.write_ue(3); // transform blocks 4..32
    s.write_ue(2);
    s.write_ue(2);
    s.write_bit(false);
    s.write_bit(true);
    s.write_bit(false);
    s.write_bit(false); // no scaling/SAO/PCM, AMP
    s.write_ue(0);
    s.write_bit(false);
    s.write_bit(false);
    s.write_bit(true); // explicit slice RPS, no LT/MVP, smoothing
    s.write_bit(true); // VUI: square pixels, BT.709 limited, exact frame rate
    s.write_bit(true);
    s.write_bits(1, 8);
    s.write_bit(false);
    s.write_bit(true);
    s.write_bits(5, 3);
    s.write_bit(false);
    s.write_bit(true);
    s.write_bits(1, 8);
    s.write_bits(1, 8);
    s.write_bits(1, 8);
    s.write_bits(0, 5); // chroma location, neutral chroma, field sequence, field info, display window
    s.write_bit(true);
    s.write_bits(c.fps.1, 32);
    s.write_bits(c.fps.0, 32);
    s.write_bit(false);
    s.write_bit(false);
    s.write_bit(false); // no POC timing, HRD or restrictions
    s.write_bit(false); // no SPS extensions

    let mut p = BitWriter::new();
    p.write_ue(0);
    p.write_ue(0);
    p.write_bits(0, 7); // ids, no dependent/output/extra/sign/CABAC flags
    p.write_ue(0);
    p.write_ue(0);
    p.write_se(0);
    p.write_bit(false);
    p.write_bit(false);
    p.write_bit(true);
    p.write_ue(0); // CU QP deltas
    p.write_se(0);
    p.write_se(0);
    p.write_bits(0, 8); // chroma offsets, weighting, bypass, tiles/WPP, cross-slice loop filter, deblock control
    p.write_bit(false);
    p.write_bit(false);
    p.write_ue(0);
    p.write_bit(false);
    p.write_bit(false);
    (nal(32, v), nal(33, s), nal(34, p))
}

/// H.265 §7.3.6: one IDR/P slice, explicit one-picture short-term RPS, eight POC LSB bits.
fn slice_header(poc: u32, idr: bool) -> Vec<u8> {
    use filmcraft_bitstream::{BitWriter, escape_rbsp};
    let mut w = BitWriter::new();
    w.write_bit(true);
    if idr {
        w.write_bit(false);
    }
    w.write_ue(0);
    w.write_ue(if idr { 2 } else { 1 });
    if !idr {
        w.write_bits(poc & 255, 8);
        w.write_bit(false);
        w.write_ue(1);
        w.write_ue(0);
        w.write_ue(0);
        w.write_bit(true);
        w.write_bit(false);
        w.write_ue(0); // one active L0 reference, five merge candidates
    }
    w.write_se(0);
    w.rbsp_trailing(); // slice QP delta; byte_alignment()
    let mut out = vec![0, 0, 0, 1, if idr { 38 } else { 2 }, 1];
    out.extend_from_slice(&escape_rbsp(&w.finish()));
    out
}

/// Refuse driver headers that do not describe the submitted IDR/P picture.
fn validate_picture(bytes: &[u8], idr: bool) -> std::result::Result<(), String> {
    let mut slices = 0;
    for nal in nvenc::annex_b_nals(bytes) {
        let [first, second, ..] = nal else {
            return Err("truncated HEVC NAL header".into());
        };
        if first & 0x81 != 0 || second & 0xf8 != 0 || second & 7 != 1 {
            return Err("unexpected HEVC layer, temporal id or forbidden bit".into());
        }
        let kind = (first >> 1) & 63;
        if kind < 32 {
            if kind != if idr { 19 } else { 1 } {
                return Err(format!("HEVC NAL type {kind} does not match submitted picture"));
            }
            slices += 1;
        }
    }
    if slices != 1 {
        return Err(format!("HEVC driver returned {slices} slices, expected one"));
    }
    Ok(())
}

pub struct VaHevcEncoder {
    session: EncodeSession,
    config: Config,
    hvcc: HevcConfig,
    headers: [Vec<u8>; 3],
    first_picture: bool,
    gop: u32,
    previous: Option<(usize, i32)>,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

impl VaHevcEncoder {
    fn new(config: Config, caps: &HevcEncodeCaps) -> std::result::Result<Self, String> {
        let rc = if config.cbr { ffi::VA_RC_CBR } else { ffi::VA_RC_VBR };
        let session = EncodeSession::new_hevc(caps, rc, config.width, config.height)?;
        Self::with_session(config, session)
    }

    fn new_resident_linear(config: Config, caps: &HevcEncodeCaps, node: super::DrmNodeId) -> std::result::Result<Self, String> {
        let rc = if config.cbr { ffi::VA_RC_CBR } else { ffi::VA_RC_VBR };
        let session = EncodeSession::new_hevc_resident_linear(node, caps, rc, config.width, config.height)?;
        Self::with_session(config, session)
    }

    fn with_session(config: Config, session: EncodeSession) -> std::result::Result<Self, String> {
        let (vps, sps, pps) = parameter_sets(&config, session.coded_size());
        let hvcc = nvenc::hevc::hevc_config(&vps, &sps, &pps, (config.width, config.height), 8)?;
        let headers = [vps, sps, pps].map(|nal| {
            let mut out = vec![0, 0, 0, 1];
            out.extend_from_slice(&nal);
            out
        });
        Ok(Self { session, config, hvcc, headers, first_picture: true, gop: 0, previous: None, y: Vec::new(), u: Vec::new(), v: Vec::new() })
    }

    fn picture(&mut self) -> std::result::Result<(Vec<u8>, bool), String> {
        let c = &self.config;
        let idr = self.gop == 0;
        if idr {
            self.previous = None;
        }
        let slot = self.previous.map_or(0, |(s, _)| (s + 1) % 2);
        let current = self.session.recon(slot).ok_or("missing HEVC reconstruction surface")?;
        let poc = i32::try_from(self.gop).map_err(|_| "HEVC POC overflow")?;
        let coded = self.session.coded_size();
        let seq = ffi::VAEncSequenceParameterBufferHEVC {
            general_profile_idc: 1,
            general_level_idc: c.level,
            intra_period: c.keyint,
            intra_idr_period: c.keyint,
            ip_period: 1,
            bits_per_second: c.kbps * 1000,
            pic_width_in_luma_samples: coded.0 as u16,
            pic_height_in_luma_samples: coded.1 as u16,
            seq_fields: 1 | 1 << 10 | 1 << 11 | 1 << 16,
            log2_diff_max_min_luma_coding_block_size: 3,
            log2_diff_max_min_transform_block_size: 3,
            max_transform_hierarchy_depth_inter: 2,
            max_transform_hierarchy_depth_intra: 2,
            vui_parameters_present_flag: 1,
            vui_fields: 1 | 1 << 3,
            aspect_ratio_idc: 1,
            sar_width: 1,
            sar_height: 1,
            vui_num_units_in_tick: c.fps.1,
            vui_time_scale: c.fps.0,
            ..Default::default()
        };
        let cur = ffi::VAPictureHEVC { picture_id: current, pic_order_cnt: poc, flags: 0, va_reserved: [0; 4] };
        let mut refs = [ffi::VAPictureHEVC::INVALID; 15];
        if let Some((s, poc)) = self.previous {
            refs[0] = ffi::VAPictureHEVC {
                picture_id: self.session.recon(s).ok_or("missing HEVC reference surface")?,
                pic_order_cnt: poc,
                flags: 0,
                va_reserved: [0; 4],
            };
        }
        let pic = ffi::VAEncPictureParameterBufferHEVC {
            decoded_curr_pic: cur,
            reference_frames: refs,
            coded_buf: self.session.coded_buffer(),
            collocated_ref_pic_index: 255,
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
            nal_unit_type: if idr { 19 } else { 1 },
            pic_fields: u32::from(idr) | (if idr { 1 } else { 2 }) << 1 | 1 << 4 | 1 << 9,
            hierarchical_level_plus1: 0,
            va_byte_reserved: 0,
            scc_fields: 0,
            va_reserved: [0; 15],
        };
        let slice = ffi::VAEncSliceParameterBufferHEVC {
            slice_segment_address: 0,
            num_ctu_in_slice: coded.0.div_ceil(64) * coded.1.div_ceil(64),
            slice_type: if idr { 2 } else { 1 },
            slice_pic_parameter_set_id: 0,
            num_ref_idx_l0_active_minus1: 0,
            num_ref_idx_l1_active_minus1: 0,
            ref_pic_list0: refs,
            ref_pic_list1: [ffi::VAPictureHEVC::INVALID; 15],
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
            slice_fields: 1,
            pred_weight_table_bit_offset: 0,
            pred_weight_table_bit_length: 0,
            va_reserved: [0; 6],
        };
        let misc = [
            Misc::RateControl(ffi::VAEncMisc {
                type_: ffi::VAEncMiscParameterTypeRateControl,
                data: ffi::VAEncMiscParameterRateControl {
                    bits_per_second: c.peak * 1000,
                    target_percentage: ((u64::from(c.kbps) * 100) / u64::from(c.peak)).clamp(1, 100) as u32,
                    window_size: 1000,
                    max_qp: 51,
                    ..Default::default()
                },
            }),
            Misc::FrameRate(ffi::VAEncMisc {
                type_: ffi::VAEncMiscParameterTypeFrameRate,
                data: ffi::VAEncMiscParameterFrameRate { framerate: c.fps.1 << 16 | c.fps.0, ..Default::default() },
            }),
        ];
        let packed_slice = slice_header(self.gop, idr);
        let headers = [(1, self.headers[0].as_slice()), (1, self.headers[1].as_slice()), (2, self.headers[2].as_slice()), (3, packed_slice.as_slice())];
        let bytes = self.session.encode_hevc(&HevcPicture {
            headers: if idr { &headers[..] } else { &headers[3..] },
            sequence: idr.then_some((&seq, &misc[..])),
            picture: &pic,
            slice: &slice,
        })?;
        validate_picture(&bytes, idr)?;
        if let Ok((vps, sps, pps)) = nvenc::split_hevc_parameter_sets(&bytes) {
            let actual = nvenc::hevc::hevc_config(&vps, &sps, &pps, (c.width, c.height), 8)?;
            if !self.first_picture && actual != self.hvcc {
                return Err("HEVC driver changed parameter sets mid-stream".into());
            }
            self.hvcc = actual;
        }
        self.first_picture = false;
        let sample = nvenc::annex_b_to_length_prefixed_for(Codec::Hevc, &bytes)?;
        self.previous = Some((slot, poc));
        self.gop = (self.gop + 1) % c.keyint;
        Ok((sample, idr))
    }
}

impl VaHevcEncoder {
    /// Build the hardware encoder for the AMD-resident path, where the GPU writes the NV12 picture
    /// itself: `None` when these settings are not a VA-API HEVC job, an error when the driver
    /// declines them. No pixels pass through CPU memory.
    pub fn resident(w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Option<std::result::Result<Self, String>> {
        if s.video_format() != Format::Hevc {
            return None;
        }
        let caps = match va::hevc_encode_caps() {
            Ok(caps) => caps,
            Err(why) => {
                log::info!("VA-API HEVC declined: {why}");
                return None;
            }
        };
        Some(config(w, h, rate, s, &caps).and_then(|c| Self::new(c, &caps)))
    }

    /// Strict resident encoder on the exact DRM render node of the Vulkan device: `None` when these
    /// settings are not an AMD-required or auto HEVC job, an error when the node's driver declines
    /// them or cannot allocate a single-object LINEAR NV12 input. No pixels pass through CPU memory.
    pub fn resident_on_node(node: super::DrmNodeId, w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Option<std::result::Result<Self, String>> {
        if !matches!(s.hardware_encoding, HardwareEncoding::Auto | HardwareEncoding::GpuRequired | HardwareEncoding::AmdRequired)
            || s.video_format() != Format::Hevc
        {
            return None;
        }
        let built = va::hevc_encode_caps_on_node(node).and_then(|caps| config(w, h, rate, s, &caps).and_then(|c| Self::new_resident_linear(c, &caps, node)));
        Some(built)
    }

    /// The driver's coded picture size: the requested picture rounded up to whole CTUs.
    pub fn coded_size(&self) -> (u32, u32) {
        self.session.coded_size()
    }

    /// Encode GPU-produced packed NV12 without a pixel readback. `source` must hold the coded-size
    /// picture ([`Self::coded_size`], edges replicated by the producer), `queue` must also contain
    /// the producer submission, and this awaits the exact VA-surface copy submission before the
    /// encode begins. A failed or timed-out copy quarantines the session.
    pub fn encode_gpu_nv12(
        &mut self,
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        source: &wgpu::Buffer,
        y_stride: u32,
        uv_stride: u32,
        uv_offset: u64,
        buffer_size: u64,
        timeout: std::time::Duration,
    ) -> Result<Vec<EncodedPacket>> {
        let (coded_width, coded_height) = self.session.coded_size();
        let mut transfer = crate::vulkan_import::write_coded_nv12_buffer_to_va_input(
            adapter,
            device,
            queue,
            &mut self.session,
            source,
            coded_width,
            coded_height,
            y_stride,
            uv_stride,
            uv_offset,
            buffer_size,
            timeout,
        )
        .map_err(|e| ExportError::Encode(format!("VA-API HEVC GPU input: {e}")))?;
        let (data, key) = self.picture().map_err(|e| ExportError::Encode(format!("VA-API HEVC GPU input: {e}")))?;
        transfer.retired(timeout).map_err(|e| ExportError::Encode(format!("VA-API HEVC GPU retirement: {e}")))?;
        filmcraft_export::note_hw_encode_frame();
        Ok(vec![EncodedPacket { data, key, duration: self.config.fps.1, composition_offset: 0 }])
    }
}

impl VideoEncoder for VaHevcEncoder {
    fn sample_entry(&self) -> SampleEntry {
        SampleEntry::hevc(self.hvcc.clone(), self.config.width as u16, self.config.height as u16)
    }
    fn timescale(&self) -> u32 {
        self.config.fps.0
    }
    fn encode(&mut self, f: &EncoderFrame) -> Result<Vec<EncodedPacket>> {
        let c = &self.config;
        if !frame_admissible(c, f) {
            return Err(ExportError::Encode("VA-API HEVC: invalid RGBA dimensions, length or HDR input".into()));
        }
        filmcraft_export::timed(filmcraft_export::Stage::Convert, || {
            filmcraft_export::rgba_to_yuv420_8(f.rgba, c.width as usize, c.height as usize, &mut self.y, &mut self.u, &mut self.v);
        });
        self.session.upload(&self.y, &self.u, &self.v, c.width as usize, c.height as usize).map_err(|e| ExportError::Encode(format!("VA-API HEVC: {e}")))?;
        let (data, key) = self.picture().map_err(|e| ExportError::Encode(format!("VA-API HEVC: {e}")))?;
        filmcraft_export::note_hw_encode_frame();
        Ok(vec![EncodedPacket { data, key, duration: self.config.fps.1, composition_offset: 0 }])
    }
    fn flush(&mut self) -> Result<Vec<EncodedPacket>> {
        Ok(Vec::new())
    }
}

/// Choosing HEVC is the hardware opt-in, including with the H.264 hardware toggle off.
pub fn factory(format: Format, w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>> {
    if format != Format::Hevc {
        return None;
    }
    let built = VaHevcEncoder::resident(w, h, rate, s)?;
    Some(match built {
        Ok(e) => {
            filmcraft_export::note_hw_encode_session();
            Ok(Box::new(e) as Box<dyn VideoEncoder>)
        }
        Err(why) => {
            filmcraft_export::note_hw_encode_declined();
            Err(ExportError::Encode(format!("VA-API HEVC: {why}")))
        }
    })
}

static AVAILABLE: OnceLock<bool> = OnceLock::new();

/// One functional probe: a small session encodes one picture. On success, the coded size and the
/// sample entry the file would carry; on failure, why this machine cannot encode HEVC.
fn probe_attempt() -> std::result::Result<(usize, SampleEntry), String> {
    let caps = va::hevc_encode_caps()?;
    let s = ExportSettings { format: Format::Hevc, bitrate_mode: BitrateMode::Cbr, bitrate_kbps: 500, ..Default::default() };
    let c = config(256, 256, FrameRate::FPS_30, &s, &caps)?;
    let mut e = VaHevcEncoder::new(c, &caps)?;
    let y = vec![16; 256 * 256];
    let uv = vec![128; 128 * 128];
    e.session.upload(&y, &uv, &uv, 256, 256)?;
    let (data, _) = e.picture()?;
    Ok((data.len(), e.sample_entry()))
}

fn probe() -> bool {
    match probe_attempt() {
        Ok(_) => true,
        Err(why) => {
            log::info!("VA-API HEVC Main unavailable: {why}");
            false
        }
    }
}
/// UI reads only the cached answer; the startup worker owns the real session probe.
pub fn available() -> bool {
    AVAILABLE.get().copied().unwrap_or(false)
}
pub fn warm_probe() {
    static START: std::sync::Once = std::sync::Once::new();
    START.call_once(|| {
        if let Err(why) = std::thread::Builder::new().name("vaapi-hevc-probe".into()).spawn(|| {
            AVAILABLE.get_or_init(|| {
                std::panic::catch_unwind(probe).unwrap_or_else(|_| {
                    log::warn!("VA-API HEVC probe panicked");
                    false
                })
            });
        }) {
            log::warn!("VA-API HEVC probe worker could not start: {why}");
            let _ = AVAILABLE.set(false);
        }
    });
}

/// Headless callers may wait for startup probes without blocking any UI redraw.
pub fn wait_probe(timeout: std::time::Duration) {
    let Some(deadline) = std::time::Instant::now().checked_add(timeout) else {
        return;
    };
    while AVAILABLE.get().is_none() {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return;
        }
        std::thread::sleep(left.min(std::time::Duration::from_millis(10)));
    }
}

/// Validate effective settings without opening an encoder session.
pub(crate) fn validate_export_settings(s: &ExportSettings, r: &filmcraft_export::settings::Resolved, caps: &HevcEncodeCaps) -> std::result::Result<(), String> {
    if !matches!(caps.entrypoint, ffi::VAEntrypointEncSlice | ffi::VAEntrypointEncSliceLP) {
        return Err("HEVC admission encode entrypoint mismatch".into());
    }
    va::validate_packed_headers(Format::Hevc, caps.packed_headers)?;
    config(r.width, r.height, r.rate, s, caps).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_driver_headers_are_rejected() {
        assert!(validate_picture(&[0, 0, 1, 38, 1, 0x80], true).is_ok());
        assert!(validate_picture(&[0, 0, 1, 2, 1, 0x80], false).is_ok());
        for bad in [&[0, 0, 1, 0, 0, 0x80][..], &[0, 0, 1, 38][..], &[0, 0, 1, 38, 0, 0x80][..], &[0, 0, 1, 38, 9, 0x80][..], &[0, 0, 1, 64, 1, 0x80][..]] {
            assert!(validate_picture(bad, true).is_err());
        }
        assert!(validate_picture(&[0, 0, 1, 2, 1, 0x80], true).is_err());
    }

    #[test]
    fn level_and_caps_cover_coded_ctus_instead_of_only_visible_pixels() {
        let mut caps = HevcEncodeCaps {
            limits: va::EncodeCaps { rate_control: ffi::VA_RC_CBR | ffi::VA_RC_VBR, max_size: Some((4096, 4096)) },
            entrypoint: ffi::VAEntrypointEncSlice,
            packed_headers: 7,
        };
        let s = ExportSettings { format: Format::Hevc, bitrate_mode: BitrateMode::Cbr, bitrate_kbps: 500, ..Default::default() };
        let c = config(642, 362, FrameRate::FPS_30, &s, &caps).unwrap();
        // Coded 704x384 is 270336 luma samples: above Level 2.1's 245760, within Level 3.0.
        assert_eq!(c.level, 90);
        caps.limits.max_size = Some((650, 370));
        assert!(config(642, 362, FrameRate::FPS_30, &s, &caps).is_err(), "coded allocation exceeds advertised dimensions");
        caps.limits.max_size = Some((704, 384));
        assert!(config(642, 362, FrameRate::FPS_30, &s, &caps).is_ok());
    }

    #[test]
    fn hdr_signal_and_hdr_frames_are_rejected_before_session() {
        let caps = HevcEncodeCaps {
            limits: va::EncodeCaps { rate_control: ffi::VA_RC_CBR | ffi::VA_RC_VBR, max_size: Some((4096, 4096)) },
            entrypoint: ffi::VAEntrypointEncSlice,
            packed_headers: 0,
        };
        let hdr = ExportSettings {
            format: Format::Hevc,
            bitrate_kbps: 500,
            signal: filmcraft_export::ColorSignal { transfer: 16, ..Default::default() },
            ..Default::default()
        };
        assert!(config(1920, 1080, FrameRate::FPS_30, &hdr, &caps).is_err(), "HDR signal must not reach a session");
        let sdr = ExportSettings { format: Format::Hevc, bitrate_kbps: 500, ..Default::default() };
        let c = config(64, 64, FrameRate::FPS_30, &sdr, &caps).expect("SDR config");
        let rgba = vec![0u8; 64 * 64 * 4];
        let hdr_samples = [0.5f32; 12];
        let mut frame = EncoderFrame { width: 64, height: 64, rgba: &rgba, hdr: Some(&hdr_samples[..]), index: 0 };
        assert!(!frame_admissible(&c, &frame), "HDR frame must not reach a session");
        frame.hdr = None;
        assert!(frame_admissible(&c, &frame), "SDR frame of the configured size is admitted");
    }

    #[test]
    fn invalid_settings_are_rejected_before_session_creation() {
        let caps = HevcEncodeCaps {
            limits: va::EncodeCaps { rate_control: ffi::VA_RC_CBR | ffi::VA_RC_VBR, max_size: Some((4096, 4096)) },
            entrypoint: ffi::VAEntrypointEncSlice,
            packed_headers: 0,
        };
        let s = ExportSettings { format: Format::Hevc, bitrate_kbps: 500, ..Default::default() };
        assert!(config(1920, 1080, FrameRate::FPS_30, &s, &caps).is_ok());
        for (w, h) in [(0, 1080), (7680, 4320), (1919, 1080), (1920, 1079)] {
            assert!(config(w, h, FrameRate::FPS_30, &s, &caps).is_err());
        }
        assert!(config(128, 128, FrameRate { num: 30, den: 0 }, &s, &caps).is_err());
    }
    #[test]
    fn probe_validates_real_hardware_or_reports_unavailable() {
        let outcome = probe_attempt();
        AVAILABLE.get_or_init(|| outcome.is_ok());
        match outcome {
            Ok((bytes, entry)) => {
                assert!(bytes > 0, "functional probe encoded an empty picture");
                assert_ne!(entry.format.0, [0; 4], "functional probe produced an empty sample entry");
                eprintln!("VA-API HEVC functional probe: ok, {bytes} bytes");
            }
            Err(why) => {
                assert!(!why.trim().is_empty(), "functional probe failed without a reason");
                eprintln!("VA-API HEVC functional probe unavailable: {why}");
            }
        }
    }
}
