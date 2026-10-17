//! H.264 export encoding through VA-API (Linux: AMD, Intel and other Mesa / vendor drivers).
//! [`factory`] is registered behind NVENC and in front of FilmCraft's own encoder
//! (`filmcraft_export::register_encoder`). It takes an export only when Export ▸ Hardware
//! encoding is Auto and the GPU can do what the settings ask; otherwise the next encoder runs,
//! exactly as before.
//!
//! The stream is IDR + P pictures (no B-frames), one slice per picture, picture order count type
//! 0, CABAC for Main / High and no 8×8 transform (AMD's encoders never use it, so a PPS that
//! allows it makes High-profile streams undecodable). The SPS and PPS are written by
//! `filmcraft_h264enc`'s own writers from the same values the driver is given and go into the
//! `avcC`; parameter sets, delimiters and SEI the driver writes itself are left out of the samples. A
//! hardware failure after the first picture ends the export with an error: the software encoder
//! cannot continue a stream the hardware started.
//!
//! Ported from the VA-API encoder of storytold/filmcraft#408 (Manuel Camacho) onto the shared
//! display and libva table of [`super::va`].

use std::sync::OnceLock;

use filmcraft_export::{
    BitrateMode, EncodedPacket, EncoderFrame, ExportError, ExportSettings, FieldOrder, Format, H264Pass, H264Profile, HardwareEncoding, Result, VideoEncoder,
};
use filmcraft_isobmff::{AvcConfig, SampleEntry};
use filmcraft_time::FrameRate;

use super::ffi::{self, VAPictureH264, VASurfaceID};
use super::va::{self, EncodeCaps, EncodeSession, H264Picture, Misc};

/// log2(MaxFrameNum) and log2(MaxPicOrderCntLsb).
const LOG2_MAX_FRAME_NUM: u32 = 8;
const LOG2_MAX_POC_LSB: u32 = 8;
/// Reconstructed-picture surfaces: the picture being encoded and its one reference.
const RECON: usize = 2;
/// The largest picture taken when the driver does not say.
const DEFAULT_MAX_SIZE: (u32, u32) = (4096, 4096);

/// What the encoder was asked to do.
#[derive(Clone, Debug)]
pub(crate) struct Config {
    pub(crate) entrypoint: ffi::VAEntrypoint,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) fps: (u32, u32),
    pub(crate) kbps: u32,
    pub(crate) max_kbps: u32,
    pub(crate) cbr: bool,
    pub(crate) keyint: u32,
    pub(crate) profile: H264Profile,
    pub(crate) level: u8,
    pub(crate) sar: (u16, u16),
}

fn va_profile(p: H264Profile) -> ffi::VAProfile {
    match p {
        H264Profile::Baseline => ffi::VAProfileH264ConstrainedBaseline,
        H264Profile::Main => ffi::VAProfileH264Main,
        H264Profile::High => ffi::VAProfileH264High,
    }
}

/// The encoder's configuration for an export, or why the GPU does not take it. `caps` is what
/// the driver offers for the profile the settings ask for.
pub(crate) fn config(format: Format, w: u32, h: u32, rate: FrameRate, s: &ExportSettings, caps: &EncodeCaps) -> std::result::Result<Config, String> {
    if format != Format::H264 || s.format.is_mxf() {
        return Err("only MP4 / MOV H.264 exports (MXF carries an Annex B stream)".into());
    }
    if s.signal.is_hdr() {
        return Err("HDR".into());
    }
    if s.bitrate_mode == BitrateMode::Vbr2Pass || !matches!(s.h264_pass, H264Pass::Single) {
        return Err("two-pass VBR".into());
    }
    if s.bitrate_mode == BitrateMode::Crf {
        return Err("constant-quality (CRF) rate control".into());
    }
    if s.field_order != FieldOrder::Progressive {
        return Err("interlaced output".into());
    }
    let cbr = s.bitrate_mode == BitrateMode::Cbr;
    let rc = if cbr { ffi::VA_RC_CBR } else { ffi::VA_RC_VBR };
    if caps.rate_control & rc == 0 {
        return Err(format!("the GPU's encoder has no {} rate control", if cbr { "CBR" } else { "VBR" }));
    }
    let (mw, mh) = caps.max_size.unwrap_or(DEFAULT_MAX_SIZE);
    if w == 0 || h == 0 || w > mw || h > mh {
        return Err(format!("{w}x{h} (the GPU encodes up to {mw}x{mh})"));
    }
    let (Ok(num), Ok(den)) = (u32::try_from(rate.num), u32::try_from(rate.den)) else { return Err("frame rate".into()) };
    if num == 0 || den == 0 || num > 0xffff || den > 0xffff {
        return Err(format!("frame rate {num}/{den}"));
    }
    let fps = f64::from(num) / f64::from(den);
    let kbps = s.bitrate_kbps.clamp(100, 4_000_000);
    let keyint = s.keyframe_distance.filter(|k| *k > 0).unwrap_or_else(|| (fps * 2.0).round().max(1.0) as u32).min(10_000);
    let max_kbps = if cbr { kbps } else { s.max_bitrate_kbps.filter(|m| *m >= kbps).unwrap_or(kbps.saturating_mul(3) / 2) };
    let high = s.h264_profile == H264Profile::High;
    let auto = filmcraft_h264enc::nal::pick_level(w.div_ceil(16), h.div_ceil(16), fps, 1, Some(max_kbps), high);
    let level = s.h264_level.map_or(auto, |l| filmcraft_h264enc::nal::level_at_least(l, auto));
    let sar = s.pixel_aspect.map_or((1, 1), |(n, d)| (n.clamp(1, 65535) as u16, d.clamp(1, 65535) as u16));
    Ok(Config { entrypoint: caps.entrypoint, width: w, height: h, fps: (num, den), kbps, max_kbps, cbr, keyint, profile: s.h264_profile, level, sar })
}

/// `VAEncSequenceParameterBufferH264::seq_fields`: 4:2:0, frame pictures only, direct 8×8
/// inference, POC type 0 (bit layout in ffi.rs; positions checked against the header).
fn seq_fields(log2_max_frame_num: u32, log2_max_poc_lsb: u32) -> u32 {
    1 | 1 << 2 | 1 << 5 | (log2_max_frame_num.saturating_sub(4) & 0xf) << 6 | (log2_max_poc_lsb.saturating_sub(4) & 0xf) << 12
}

/// `vui_fields`: aspect ratio, timing and bitstream restriction present, 2^11 motion-vector
/// range, fixed frame rate, motion vectors may cross picture boundaries.
fn vui_fields() -> u32 {
    1 | 1 << 1 | 1 << 2 | 11 << 3 | 11 << 8 | 1 << 13 | 1 << 15
}

/// `pic_fields`: IDR flag, reference picture, CABAC, deblocking filter control present (as our
/// PPS says); no 8×8 transform.
fn pic_fields(idr: bool, cabac: bool) -> u32 {
    u32::from(idr) | 1 << 1 | u32::from(cabac) << 3 | 1 << 9
}

fn va_picture(surface: VASurfaceID, frame_num: u32, poc: i32, flags: u32) -> VAPictureH264 {
    VAPictureH264 { picture_id: surface, frame_idx: frame_num, flags, TopFieldOrderCnt: poc, BottomFieldOrderCnt: poc, va_reserved: [0; 4] }
}

/// The NAL units of an Annex B stream (start codes and trailing zero bytes removed).
fn annexb_nals(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    let mut i = 0;
    while i + 2 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            if let Some(s) = start {
                let mut end = i;
                while end > s && data[end - 1] == 0 {
                    end -= 1;
                }
                out.push(&data[s..end]);
            }
            i += 3;
            start = Some(i);
            continue;
        }
        i += 1;
    }
    if let Some(s) = start.filter(|s| *s < data.len()) {
        out.push(&data[s..]);
    }
    out
}

/// A length-prefixed sample from an Annex B access unit, without the SPS / PPS / AUD (the `avcC`
/// carries ours) or SEI the driver wrote. Mesa's radeonsi writes each slice with a zero NAL header byte when the
/// application supplies no packed headers; such a byte (nal_unit_type 0 is never a slice) is set
/// to what the picture is: an IDR slice (`0x65`) or a reference non-IDR slice (`0x41`; every
/// picture here is a reference). Slices whose header byte the driver writes are left alone.
fn sample(annexb: &[u8], idr: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(annexb.len() + 16);
    for nal in annexb_nals(annexb) {
        let Some(&h) = nal.first() else { continue };
        // SEI (6): written against the driver's own SPS (HRD, picture timing) that ours does not signal
        if matches!(h & 0x1f, 6..=9) {
            continue;
        }
        let Ok(len) = u32::try_from(nal.len()) else { continue };
        out.extend_from_slice(&len.to_be_bytes());
        out.push(if h == 0 { if idr { 0x65 } else { 0x41 } } else { h });
        out.extend_from_slice(nal.get(1..).unwrap_or(&[]));
    }
    out
}

/// SPS and PPS NAL units matching what the driver is told.
fn parameter_sets(cfg: &Config, coded_w: u32, coded_h: u32) -> (Vec<u8>, Vec<u8>) {
    use filmcraft_h264enc::nal;
    let (profile_idc, constraint) = match cfg.profile {
        H264Profile::Baseline => (66u8, 0xC0u8),
        H264Profile::Main => (77, 0x40),
        H264Profile::High => (100, 0x00),
    };
    let color = filmcraft_h264enc::ColorConfig::default();
    let sps = nal::Sps {
        profile_idc,
        constraint_flags: constraint,
        level_idc: cfg.level,
        width_mbs: coded_w / 16,
        height_mbs: coded_h / 16,
        crop_right: coded_w.saturating_sub(cfg.width),
        crop_bottom: coded_h.saturating_sub(cfg.height),
        log2_max_frame_num: LOG2_MAX_FRAME_NUM,
        log2_max_poc_lsb: LOG2_MAX_POC_LSB,
        max_num_ref_frames: 1,
        vui: nal::Vui {
            sar: cfg.sar,
            video_full_range: color.full_range,
            colour_primaries: color.primaries,
            transfer: color.transfer,
            matrix: color.matrix,
            num_units_in_tick: cfg.fps.1,
            time_scale: cfg.fps.0.saturating_mul(2),
            max_num_reorder_frames: 0,
            max_dec_frame_buffering: 1,
        },
    };
    let pps = nal::Pps {
        cabac: cfg.profile != H264Profile::Baseline,
        pic_init_qp: 26,
        chroma_qp_offset: 0,
        transform_8x8: false,
        high: cfg.profile == H264Profile::High,
    };
    (nal::nal(3, nal::NAL_SPS, &sps.rbsp()), nal::nal(3, nal::NAL_PPS, &pps.rbsp()))
}

/// An H.264 export encoder on a VA-API encode session.
pub struct VaH264Encoder {
    session: EncodeSession,
    cfg: Config,
    sps: Vec<u8>,
    pps: Vec<u8>,
    frame_num: u32,
    idr_pic_id: u16,
    gop_index: u32,
    /// The previous picture: (recon slot, frame_num, POC), the next picture's reference.
    prev: Option<(usize, u32, i32)>,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

impl VaH264Encoder {
    fn new(cfg: Config) -> std::result::Result<Self, String> {
        let rc = if cfg.cbr { ffi::VA_RC_CBR } else { ffi::VA_RC_VBR };
        let session = EncodeSession::new(va_profile(cfg.profile), cfg.entrypoint, rc, cfg.width, cfg.height, RECON)?;
        let (cw, ch) = session.coded_size();
        let (sps, pps) = parameter_sets(&cfg, cw, ch);
        Ok(Self { session, cfg, sps, pps, frame_num: 0, idr_pic_id: 0, gop_index: 0, prev: None, y: Vec::new(), u: Vec::new(), v: Vec::new() })
    }

    fn cabac(&self) -> bool {
        self.cfg.profile != H264Profile::Baseline
    }

    /// Encode the uploaded picture; its Annex B bytes and whether it is an IDR picture.
    fn encode_picture(&mut self) -> std::result::Result<(Vec<u8>, bool), String> {
        let idr = self.gop_index == 0;
        if idr {
            self.frame_num = 0;
            self.prev = None;
        }
        let poc = (self.gop_index.wrapping_mul(2) & ((1 << LOG2_MAX_POC_LSB) - 1)) as i32;
        let recon = self.session.recon_count().max(1);
        let slot = self.prev.map_or(0, |(p, _, _)| (p + 1) % recon);
        let cur = self.session.recon(slot).ok_or("no reconstructed-picture surface")?;
        let (cw, ch) = self.session.coded_size();
        let (mbw, mbh) = (cw / 16, ch / 16);
        let cfg = &self.cfg;
        let (num, den) = cfg.fps;
        let crop = cw != cfg.width || ch != cfg.height;
        let seq = ffi::VAEncSequenceParameterBufferH264 {
            seq_parameter_set_id: 0,
            level_idc: cfg.level,
            intra_period: cfg.keyint,
            intra_idr_period: cfg.keyint,
            ip_period: 1,
            bits_per_second: cfg.kbps.saturating_mul(1000),
            max_num_ref_frames: 1,
            picture_width_in_mbs: u16::try_from(mbw).map_err(|_| "picture too wide")?,
            picture_height_in_mbs: u16::try_from(mbh).map_err(|_| "picture too tall")?,
            seq_fields: seq_fields(LOG2_MAX_FRAME_NUM, LOG2_MAX_POC_LSB),
            bit_depth_luma_minus8: 0,
            bit_depth_chroma_minus8: 0,
            num_ref_frames_in_pic_order_cnt_cycle: 0,
            offset_for_non_ref_pic: 0,
            offset_for_top_to_bottom_field: 0,
            offset_for_ref_frame: [0; 256],
            frame_cropping_flag: u8::from(crop),
            frame_crop_left_offset: 0,
            frame_crop_right_offset: if crop { (cw - cfg.width) / 2 } else { 0 },
            frame_crop_top_offset: 0,
            frame_crop_bottom_offset: if crop { (ch - cfg.height) / 2 } else { 0 },
            vui_parameters_present_flag: 1,
            vui_fields: vui_fields(),
            aspect_ratio_idc: if cfg.sar == (1, 1) { 1 } else { 255 },
            sar_width: u32::from(cfg.sar.0),
            sar_height: u32::from(cfg.sar.1),
            num_units_in_tick: den,
            time_scale: num.saturating_mul(2),
            va_reserved: [0; 4],
        };
        let mut refs = [VAPictureH264::INVALID; 16];
        let mut list0 = [VAPictureH264::INVALID; 32];
        if let Some((p, pfn, ppoc)) = self.prev {
            let surface = self.session.recon(p).ok_or("no reference surface")?;
            refs[0] = va_picture(surface, pfn, ppoc, ffi::VA_PICTURE_H264_SHORT_TERM_REFERENCE);
            list0[0] = refs[0];
        }
        let picture = ffi::VAEncPictureParameterBufferH264 {
            CurrPic: va_picture(cur, self.frame_num, poc, 0),
            ReferenceFrames: refs,
            coded_buf: self.session.coded_buffer(),
            pic_parameter_set_id: 0,
            seq_parameter_set_id: 0,
            last_picture: 0,
            frame_num: u16::try_from(self.frame_num).unwrap_or(0),
            pic_init_qp: 26,
            num_ref_idx_l0_active_minus1: 0,
            num_ref_idx_l1_active_minus1: 0,
            chroma_qp_index_offset: 0,
            second_chroma_qp_index_offset: 0,
            pic_fields: pic_fields(idr, self.cabac()),
            va_reserved: [0; 4],
        };
        let slice = ffi::VAEncSliceParameterBufferH264 {
            macroblock_address: 0,
            num_macroblocks: mbw.saturating_mul(mbh),
            macroblock_info: ffi::VA_INVALID_ID,
            slice_type: if idr { 2 } else { 0 },
            pic_parameter_set_id: 0,
            idr_pic_id: self.idr_pic_id,
            pic_order_cnt_lsb: u16::try_from(poc).unwrap_or(0),
            delta_pic_order_cnt_bottom: 0,
            delta_pic_order_cnt: [0; 2],
            direct_spatial_mv_pred_flag: 1,
            num_ref_idx_active_override_flag: 0,
            num_ref_idx_l0_active_minus1: 0,
            num_ref_idx_l1_active_minus1: 0,
            RefPicList0: list0,
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
            cabac_init_idc: 0,
            slice_qp_delta: 0,
            disable_deblocking_filter_idc: 0,
            slice_alpha_c0_offset_div2: 0,
            slice_beta_offset_div2: 0,
            va_reserved: [0; 4],
        };
        let bps = cfg.kbps.saturating_mul(1000);
        let peak = cfg.max_kbps.saturating_mul(1000);
        let misc = [
            Misc::RateControl(ffi::VAEncMisc {
                type_: ffi::VAEncMiscParameterTypeRateControl,
                data: ffi::VAEncMiscParameterRateControl {
                    bits_per_second: if cfg.cbr { bps } else { peak },
                    target_percentage: if cfg.cbr { 100 } else { (u64::from(cfg.kbps) * 100 / u64::from(cfg.max_kbps.max(1))).clamp(1, 100) as u32 },
                    window_size: 1500,
                    max_qp: 51,
                    ..Default::default()
                },
            }),
            Misc::FrameRate(ffi::VAEncMisc {
                type_: ffi::VAEncMiscParameterTypeFrameRate,
                data: ffi::VAEncMiscParameterFrameRate { framerate: (den << 16) | num, ..Default::default() },
            }),
            Misc::Hrd(ffi::VAEncMisc {
                type_: ffi::VAEncMiscParameterTypeHRD,
                data: ffi::VAEncMiscParameterHRD { buffer_size: peak.saturating_mul(2), initial_buffer_fullness: peak, ..Default::default() },
            }),
        ];
        let bytes = self.session.encode_h264(&H264Picture { sequence: idr.then_some((&seq, &misc[..])), picture: &picture, slice: &slice })?;
        // this picture is the next one's reference
        self.prev = Some((slot, self.frame_num, poc));
        self.frame_num = (self.frame_num + 1) % (1 << LOG2_MAX_FRAME_NUM);
        self.gop_index += 1;
        if self.gop_index >= self.cfg.keyint {
            self.gop_index = 0;
            self.idr_pic_id = self.idr_pic_id.wrapping_add(1);
        }
        Ok((bytes, idr))
    }
}

impl VideoEncoder for VaH264Encoder {
    fn sample_entry(&self) -> SampleEntry {
        let (w, h) = (u16::try_from(self.cfg.width).unwrap_or(u16::MAX), u16::try_from(self.cfg.height).unwrap_or(u16::MAX));
        SampleEntry::avc(AvcConfig::new(vec![self.sps.clone()], vec![self.pps.clone()], 4), w, h)
    }

    fn timescale(&self) -> u32 {
        self.cfg.fps.0
    }

    fn encode(&mut self, f: &EncoderFrame) -> Result<Vec<EncodedPacket>> {
        if f.width != self.cfg.width || f.height != self.cfg.height {
            return Err(ExportError::Encode(format!("VA-API: a {}x{} picture for a {}x{} encoder", f.width, f.height, self.cfg.width, self.cfg.height)));
        }
        let (w, h) = (self.cfg.width as usize, self.cfg.height as usize);
        if f.rgba.len() < w.saturating_mul(h).saturating_mul(4) {
            return Err(ExportError::Encode(format!("VA-API: a {}-byte RGBA picture for {w}x{h}", f.rgba.len())));
        }
        filmcraft_export::rgba_to_yuv420_8(f.rgba, w, h, &mut self.y, &mut self.u, &mut self.v);
        let fail = |e: String| ExportError::Encode(format!("VA-API: {e} (turn Export ▸ Hardware encoding off to use the software encoder)"));
        self.session.upload(&self.y, &self.u, &self.v, w, h).map_err(fail)?;
        let (bytes, key) = self.encode_picture().map_err(fail)?;
        let data = sample(&bytes, key);
        if data.is_empty() {
            return Err(ExportError::Encode("VA-API: the driver returned no picture data".into()));
        }
        filmcraft_export::note_hw_encode_frame();
        Ok(vec![EncodedPacket { data, key, duration: self.cfg.fps.1, composition_offset: 0 }])
    }

    fn flush(&mut self) -> Result<Vec<EncodedPacket>> {
        Ok(Vec::new())
    }
}

/// Whether this machine's VA-API driver encodes H.264 (Main or High, CBR or VBR); checked once.
pub fn available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| [ffi::VAProfileH264High, ffi::VAProfileH264Main].iter().any(|p| va::encode_caps(*p).is_ok()))
}

/// The Export encoder factory (see the module documentation).
pub fn factory(format: Format, w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>> {
    if s.hardware_encoding != HardwareEncoding::Auto || format != Format::H264 {
        return None;
    }
    let declined = |why: &str| {
        log::info!("VA-API hardware encoding declined: {why}");
        filmcraft_export::note_hw_encode_declined();
        None
    };
    let caps = match va::encode_caps(va_profile(s.h264_profile)) {
        Ok(c) => c,
        Err(why) => return declined(&why),
    };
    let cfg = match config(format, w, h, rate, s, &caps) {
        Ok(c) => c,
        Err(why) => return declined(&why),
    };
    match VaH264Encoder::new(cfg) {
        Ok(enc) => {
            log::info!("hardware H.264 encoding through VA-API ({w}x{h})");
            filmcraft_export::note_hw_encode_session();
            Some(Ok(Box::new(enc)))
        }
        Err(why) => declined(&why),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_export::{BitrateMode, ExportSettings, FieldOrder, Format, H264Pass, H264Profile, HardwareEncoding};
    use filmcraft_time::FrameRate;

    const CAPS: EncodeCaps = EncodeCaps { entrypoint: ffi::VAEntrypointEncSlice, rate_control: ffi::VA_RC_CBR | ffi::VA_RC_VBR, max_size: Some((4096, 4096)) };

    fn auto() -> ExportSettings {
        ExportSettings { format: Format::H264, hardware_encoding: HardwareEncoding::Auto, bitrate_kbps: 20_000, ..Default::default() }
    }

    fn cfg(s: &ExportSettings, w: u32, h: u32) -> std::result::Result<Config, String> {
        config(Format::H264, w, h, FrameRate::FPS_30, s, &CAPS)
    }

    #[test]
    fn plain_h264_export_is_taken() {
        let c = cfg(&auto(), 3840, 2160).unwrap();
        assert_eq!((c.width, c.height, c.fps, c.kbps), (3840, 2160, (30, 1), 20_000));
        assert_eq!(c.keyint, 60, "two seconds by default");
        assert!(!c.cbr);
        assert!(c.max_kbps >= c.kbps);
    }

    #[test]
    fn crf_is_declined() {
        let s = ExportSettings { bitrate_mode: BitrateMode::Crf, ..auto() };
        assert!(cfg(&s, 1920, 1080).unwrap_err().contains("CRF"));
    }

    #[test]
    fn too_large_for_the_gpu_is_declined() {
        assert!(cfg(&auto(), 7680, 4320).unwrap_err().contains("4096x4096"));
        let small = EncodeCaps { max_size: Some((1920, 1088)), ..CAPS };
        assert!(config(Format::H264, 3840, 2160, FrameRate::FPS_30, &auto(), &small).is_err());
        let unknown = EncodeCaps { max_size: None, ..CAPS };
        assert!(config(Format::H264, 3840, 2160, FrameRate::FPS_30, &auto(), &unknown).is_ok(), "default limit 4096");
        assert!(cfg(&auto(), 0, 1080).is_err());
    }

    #[test]
    fn what_the_hardware_path_does_not_do_is_declined() {
        let cases: [(&str, ExportSettings); 4] = [
            ("two-pass", ExportSettings { bitrate_mode: BitrateMode::Vbr2Pass, ..auto() }),
            ("two-pass", ExportSettings { h264_pass: H264Pass::First, ..auto() }),
            ("interlaced", ExportSettings { field_order: FieldOrder::UpperFirst, ..auto() }),
            ("MXF", ExportSettings { format: Format::MxfOp1a, ..auto() }),
        ];
        for (why, s) in cases {
            let e = cfg(&s, 1920, 1080).unwrap_err();
            assert!(e.contains(why), "{why}: {e}");
        }
        assert!(config(Format::Hevc, 1920, 1080, FrameRate::FPS_30, &auto(), &CAPS).is_err(), "H.264 only");
        let cbr_only = EncodeCaps { rate_control: ffi::VA_RC_CBR, ..CAPS };
        assert!(config(Format::H264, 1920, 1080, FrameRate::FPS_30, &auto(), &cbr_only).unwrap_err().contains("VBR"));
        let bad_rate = FrameRate { num: 1 << 20, den: 1 };
        assert!(config(Format::H264, 1920, 1080, bad_rate, &auto(), &CAPS).is_err());
    }

    #[test]
    fn settings_reach_the_config() {
        let s = ExportSettings {
            bitrate_mode: BitrateMode::Cbr,
            keyframe_distance: Some(12),
            h264_profile: H264Profile::Main,
            pixel_aspect: Some((4, 3)),
            ..auto()
        };
        let c = cfg(&s, 1440, 1080).unwrap();
        assert!(c.cbr);
        assert_eq!((c.keyint, c.profile, c.sar), (12, H264Profile::Main, (4, 3)));
        assert_eq!(c.max_kbps, c.kbps, "CBR peak = target");
        let low = ExportSettings { h264_level: Some(31), ..auto() };
        assert!(cfg(&low, 3840, 2160).unwrap().level >= 51, "a requested level too low for 4K is raised");
    }

    /// Bit positions from abi_enc.c (Task 1, `//` lines of abi_enc.out).
    #[test]
    fn bit_fields_match_the_header() {
        assert_eq!(seq_fields(4, 4) & 0x3, 0x1, "chroma_format_idc = 1 at bit 0");
        assert_eq!(seq_fields(4, 4) & (1 << 2), 1 << 2, "frame_mbs_only at bit 2");
        assert_eq!(seq_fields(4, 4) & (1 << 5), 1 << 5, "direct_8x8_inference at bit 5");
        assert_eq!(seq_fields(5, 4) ^ seq_fields(4, 4), 1 << 6, "log2_max_frame_num_minus4 at bit 6");
        assert_eq!(seq_fields(4, 5) ^ seq_fields(4, 4), 1 << 12, "log2_max_pic_order_cnt_lsb_minus4 at bit 12");
        assert_eq!(seq_fields(4, 4) & (0x3 << 10), 0, "pic_order_cnt_type 0 (bits 10-11)");
        assert_eq!(vui_fields(), 1 | 1 << 1 | 1 << 2 | 11 << 3 | 11 << 8 | 1 << 13 | 1 << 15);
        assert_eq!(pic_fields(true, true), 1 | 1 << 1 | 1 << 3 | 1 << 9, "idr, reference=1, CABAC, deblocking control");
        assert_eq!(pic_fields(false, false), 1 << 1 | 1 << 9);
        assert_eq!(pic_fields(true, true) & (1 << 8), 0, "no 8x8 transform");
    }

    #[test]
    fn splits_annexb() {
        let s = [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 4, 5];
        assert_eq!(annexb_nals(&s), vec![&[0x67, 1, 2][..], &[0x68, 3][..], &[0x65, 4, 5][..]]);
        assert!(annexb_nals(&[]).is_empty());
        assert!(annexb_nals(&[0, 0]).is_empty());
        assert!(annexb_nals(&[0, 0, 1]).is_empty(), "a start code with nothing after it");
    }

    #[test]
    fn sample_rewrites_zero_header_and_drops_parameter_sets() {
        // driver output: AUD, SPS, PPS, a slice with a zero header byte
        let au = [0, 0, 0, 1, 0x09, 0xf0, 0, 0, 0, 1, 0x67, 7, 0, 0, 1, 0x68, 8, 0, 0, 1, 0x00, 0xaa, 0xbb];
        assert_eq!(sample(&au, true), vec![0, 0, 0, 3, 0x65, 0xaa, 0xbb], "IDR slice");
        assert_eq!(sample(&au, false), vec![0, 0, 0, 3, 0x41, 0xaa, 0xbb], "reference non-IDR slice");
        // a driver that writes the byte itself is left alone; its SEI (written against its own SPS,
        // whose HRD / picture timing ours does not signal) is dropped
        let au = [0, 0, 1, 0x06, 1, 0, 0, 1, 0x25, 0xcc];
        assert_eq!(sample(&au, true), vec![0, 0, 0, 2, 0x25, 0xcc]);
        assert!(sample(&[0, 0, 1, 0x67, 1], true).is_empty(), "parameter sets only");
    }

    #[test]
    fn parameter_sets_describe_the_stream() {
        let c = cfg(&auto(), 1920, 1080).unwrap();
        let (sps, pps) = parameter_sets(&c, 1920, 1088);
        assert_eq!(sps[0] & 0x1f, 7);
        assert_eq!(sps[1], 100, "High profile_idc");
        assert_eq!(pps[0] & 0x1f, 8);
        // our own decoder's parser reads them: 1920x1080 after cropping
        let parsed = filmcraft_h264::params::Sps::parse(&filmcraft_bitstream::unescape_rbsp(&sps[1..])).unwrap();
        assert_eq!((parsed.width(), parsed.height()), (1920, 1088), "coded size");
        assert_eq!(parsed.crop_rect(), (0, 0, 1920, 1080), "cropped to the picture");
        assert_eq!(parsed.max_frame_num(), 1 << LOG2_MAX_FRAME_NUM);
    }
}
