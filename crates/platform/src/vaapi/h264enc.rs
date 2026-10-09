//! H.264 export encoding on VA-API. [`factory`] is registered in front of the software encoder
//! (`filmcraft_export::register_encoder`) and takes an export only when Export ▸ Hardware encoding
//! is Auto and the GPU can do what the settings ask; otherwise the software encoder runs, exactly
//! as before.
//!
//! The stream is IDR + P pictures (no B-frames), one slice per picture, picture order count type
//! 0, CABAC for Main / High (no 8×8 transform: AMD encoders do not use it). The SPS and PPS are written by
//! `filmcraft_h264enc`'s own writers from the same values the driver is given (profile, level,
//! colour, timing and VUI exactly as the software encoder signals them) and go into the `avcC`;
//! headers a driver may emit itself are left out of the samples.
//! A hardware failure in the middle of an export ends it with an error (the software encoder
//! cannot continue a stream the hardware started).

use filmcraft_export::{
    BitrateMode, EncodedPacket, EncoderFrame, ExportError, ExportSettings, FieldOrder, Format, H264Pass, H264Profile, HardwareEncoding, Result, VideoEncoder,
};
use filmcraft_isobmff::{AvcConfig, SampleEntry};
use filmcraft_time::FrameRate;

use super::device::{Display, EncodeSession};
use super::ffi::*;

/// log2(MaxFrameNum) and log2(MaxPicOrderCntLsb).
const LOG2_MAX_FRAME_NUM: u32 = 8;
const LOG2_MAX_POC_LSB: u32 = 8;

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
    profile: H264Profile,
    level: u8,
    sar: (u16, u16),
}

/// Why the GPU does not take this export, or its configuration.
fn config(format: Format, w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> std::result::Result<Config, String> {
    if format != Format::H264 || s.format.is_mxf() {
        return Err("only MP4 / MOV H.264 exports".into());
    }
    if s.signal.is_hdr() {
        return Err("HDR".into());
    }
    if !matches!(s.h264_pass, H264Pass::Single) {
        return Err("two-pass VBR".into());
    }
    if s.field_order != FieldOrder::Progressive {
        return Err("interlaced output".into());
    }
    if w == 0 || h == 0 || w > 4096 || h > 4096 {
        return Err(format!("{w}x{h} (the hardware encoder takes up to 4096x4096)"));
    }
    let (Ok(num), Ok(den)) = (u32::try_from(rate.num), u32::try_from(rate.den)) else { return Err("frame rate".into()) };
    if num == 0 || den == 0 || num > 0xffff || den > 0xffff {
        return Err("frame rate".into());
    }
    let kbps = s.bitrate_kbps.clamp(100, 4_000_000);
    let keyint = s.keyframe_distance.filter(|k| *k > 0).unwrap_or_else(|| (f64::from(num) / f64::from(den) * 2.0).round().max(1.0) as u32).min(10_000);
    let high = s.h264_profile == H264Profile::High;
    let max_kbps = s.max_bitrate_kbps.filter(|m| *m >= kbps).unwrap_or(kbps.saturating_mul(3) / 2);
    let auto = filmcraft_h264enc::nal::pick_level(w.div_ceil(16), h.div_ceil(16), f64::from(num) / f64::from(den), 1, Some(max_kbps), high);
    let level = s.h264_level.map_or(auto, |l| filmcraft_h264enc::nal::level_at_least(l, auto));
    Ok(Config {
        width: w,
        height: h,
        fps: (num, den),
        kbps,
        max_kbps,
        cbr: s.bitrate_mode == BitrateMode::Cbr,
        keyint,
        profile: s.h264_profile,
        level,
        sar: s.pixel_aspect.map_or((1, 1), |(n, d)| (n.clamp(1, 65535) as u16, d.clamp(1, 65535) as u16)),
    })
}

struct VaH264Encoder {
    session: EncodeSession,
    cfg: Config,
    profile_idc: VAProfile,
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
    frame_num: u32,
    idr_pic_id: u16,
    gop_index: u32,
    /// Reconstructed slot holding the previous reference picture, and its frame_num / POC.
    prev: Option<(usize, u32, i32)>,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

fn va_picture(surface: VASurfaceID, frame_num: u32, poc: i32, flags: u32) -> VAPictureH264 {
    VAPictureH264 { picture_id: surface, frame_idx: frame_num, flags, TopFieldOrderCnt: poc, BottomFieldOrderCnt: poc, va_reserved: [0; 4] }
}

/// Split an Annex-B stream into NAL units (start codes removed).
pub(crate) fn annexb_nals(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut start: Option<usize> = None;
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

impl VaH264Encoder {
    fn new(cfg: Config) -> std::result::Result<Self, String> {
        let rc = if cfg.cbr { VA_RC_CBR } else { VA_RC_VBR };
        let profile = match cfg.profile {
            H264Profile::Baseline => VAProfileH264ConstrainedBaseline,
            H264Profile::Main => VAProfileH264Main,
            H264Profile::High => VAProfileH264High,
        };
        let display = Display::open_for_encode(profile, rc)?;
        let session = EncodeSession::new(display, profile, rc, 0, cfg.width, cfg.height, 2)?;
        let (sps, pps) = parameter_sets(&cfg, session.width, session.height);
        Ok(VaH264Encoder {
            session,
            cfg,
            profile_idc: profile,
            sps: Some(sps),
            pps: Some(pps),
            frame_num: 0,
            idr_pic_id: 0,
            gop_index: 0,
            prev: None,
            y: Vec::new(),
            u: Vec::new(),
            v: Vec::new(),
        })
    }

    fn cabac(&self) -> bool {
        self.profile_idc != VAProfileH264ConstrainedBaseline
    }

    fn encode_picture(&mut self) -> std::result::Result<(Vec<u8>, bool), String> {
        let idr = self.gop_index == 0;
        if idr {
            self.frame_num = 0;
            self.prev = None;
        }
        let poc = (2 * self.gop_index as i32) & ((1 << LOG2_MAX_POC_LSB) - 1);
        let slot = match self.prev {
            Some((p, _, _)) => (p + 1) % self.session.recon_count().max(1),
            None => 0,
        };
        let s = &self.session;
        let cur = s.recon(slot).ok_or("no reconstructed surface")?;
        let (mbw, mbh) = (s.width / 16, s.height / 16);
        let cfg = &self.cfg;

        let crop = s.width != cfg.width || s.height != cfg.height;
        let (num, den) = cfg.fps;
        let mut seq = VAEncSequenceParameterBufferH264 {
            seq_parameter_set_id: 0,
            level_idc: cfg.level,
            intra_period: cfg.keyint,
            intra_idr_period: cfg.keyint,
            ip_period: 1,
            bits_per_second: cfg.kbps.saturating_mul(1000),
            max_num_ref_frames: 1,
            picture_width_in_mbs: mbw as u16,
            picture_height_in_mbs: mbh as u16,
            seq_fields: pack_bits(&[(1, 2), (1, 1), (0, 1), (0, 1), (1, 1), (LOG2_MAX_FRAME_NUM - 4, 4), (0, 2), (LOG2_MAX_POC_LSB - 4, 4), (0, 1)]),
            bit_depth_luma_minus8: 0,
            bit_depth_chroma_minus8: 0,
            num_ref_frames_in_pic_order_cnt_cycle: 0,
            offset_for_non_ref_pic: 0,
            offset_for_top_to_bottom_field: 0,
            offset_for_ref_frame: [0; 256],
            frame_cropping_flag: u8::from(crop),
            frame_crop_left_offset: 0,
            frame_crop_right_offset: (s.width - cfg.width) / 2,
            frame_crop_top_offset: 0,
            frame_crop_bottom_offset: (s.height - cfg.height) / 2,
            vui_parameters_present_flag: 1,
            vui_fields: pack_bits(&[(1, 1), (1, 1), (1, 1), (11, 5), (11, 5), (1, 1), (0, 1), (1, 1)]),
            aspect_ratio_idc: if cfg.sar == (1, 1) { 1 } else { 255 },
            sar_width: u32::from(cfg.sar.0),
            sar_height: u32::from(cfg.sar.1),
            num_units_in_tick: den,
            time_scale: num.saturating_mul(2),
            va_reserved: [0; 4],
        };
        if !crop {
            seq.frame_crop_right_offset = 0;
            seq.frame_crop_bottom_offset = 0;
        }
        let mut refs = [VAPictureH264::INVALID; 16];
        let mut list0 = [VAPictureH264::INVALID; 32];
        if let Some((p, pfn, ppoc)) = self.prev {
            let surface = s.recon(p).ok_or("no reference surface")?;
            refs[0] = va_picture(surface, pfn, ppoc, VA_PICTURE_H264_SHORT_TERM_REFERENCE);
            list0[0] = refs[0];
        }
        let pic = VAEncPictureParameterBufferH264 {
            CurrPic: va_picture(cur, self.frame_num, poc, 0),
            ReferenceFrames: refs,
            coded_buf: s.coded_buffer(),
            pic_parameter_set_id: 0,
            seq_parameter_set_id: 0,
            last_picture: 0,
            frame_num: self.frame_num as u16,
            pic_init_qp: 26,
            num_ref_idx_l0_active_minus1: 0,
            num_ref_idx_l1_active_minus1: 0,
            chroma_qp_index_offset: 0,
            second_chroma_qp_index_offset: 0,
            pic_fields: pack_bits(&[
                (u32::from(idr), 1),
                (1, 2),
                (u32::from(self.cabac()), 1),
                (0, 1),
                (0, 2),
                (0, 1),
                // transform_8x8_mode_flag off: AMD's encoder ignores it (its slices never use the 8x8
                // transform), so a PPS saying otherwise makes High profile streams undecodable
                (0, 1),
                (1, 1),
                (0, 1),
                (0, 1),
                (0, 1),
            ]),
            va_reserved: [0; 4],
        };
        let slice = VAEncSliceParameterBufferH264 {
            macroblock_address: 0,
            num_macroblocks: mbw * mbh,
            macroblock_info: VA_INVALID_ID,
            slice_type: if idr { 2 } else { 0 },
            pic_parameter_set_id: 0,
            idr_pic_id: self.idr_pic_id,
            pic_order_cnt_lsb: poc as u16,
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
        let bytes = s.coded_bytes()?;
        // this picture becomes the reference for the next
        self.prev = Some((slot, self.frame_num, poc));
        self.frame_num = (self.frame_num + 1) % (1 << LOG2_MAX_FRAME_NUM);
        self.gop_index += 1;
        if self.gop_index >= self.cfg.keyint {
            self.gop_index = 0;
            self.idr_pic_id = self.idr_pic_id.wrapping_add(1);
        }
        Ok((bytes, idr))
    }

    /// Length-prefixed sample from an Annex-B access unit; SPS / PPS / AUD are taken out (the SPS
    /// and PPS go into the `avcC`).
    ///
    /// Mesa's radeonsi driver (which advertises application-supplied packed headers) writes each
    /// slice with a zero NAL header byte when none is supplied; the slice header after it is
    /// complete. Such a byte (nal_unit_type 0 is never a valid slice) is set to what the picture
    /// is: an IDR slice (`0x65`) or a reference non-IDR slice (`0x41`, every picture here is a
    /// reference). Slices from a driver that writes the byte itself are left alone.
    fn sample(&mut self, annexb: &[u8], idr: bool) -> Vec<u8> {
        let mut out = Vec::with_capacity(annexb.len() + 16);
        for nal in annexb_nals(annexb) {
            let Some(&h) = nal.first() else { continue };
            match h & 0x1f {
                // parameter sets and delimiters: the avcC carries ours
                7..=9 => {}
                _ => {
                    out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
                    out.push(if h == 0 { if idr { 0x65 } else { 0x41 } } else { h });
                    out.extend_from_slice(nal.get(1..).unwrap_or(&[]));
                }
            }
        }
        out
    }
}

impl VideoEncoder for VaH264Encoder {
    fn sample_entry(&self) -> SampleEntry {
        let (w, h) = (u16::try_from(self.cfg.width).unwrap_or(u16::MAX), u16::try_from(self.cfg.height).unwrap_or(u16::MAX));
        SampleEntry::avc(AvcConfig::new(self.sps.iter().cloned().collect(), self.pps.iter().cloned().collect(), 4), w, h)
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
        let fail = |e: String| ExportError::Encode(format!("VA-API: {e} (turn Export ▸ Hardware encoding off to use the software encoder)"));
        self.session.upload(&self.y, &self.u, &self.v, w, h).map_err(fail)?;
        let (bytes, key) = self.encode_picture().map_err(fail)?;
        let data = self.sample(&bytes, key);
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

/// SPS and PPS NAL units matching what the driver is told (see the module documentation).
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
        crop_right: coded_w - cfg.width,
        crop_bottom: coded_h - cfg.height,
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
    let high = cfg.profile == H264Profile::High;
    let pps = nal::Pps { cabac: cfg.profile != H264Profile::Baseline, pic_init_qp: 26, chroma_qp_offset: 0, transform_8x8: false, high }; // see encode_picture
    (nal::nal(3, nal::NAL_SPS, &sps.rbsp()), nal::nal(3, nal::NAL_PPS, &pps.rbsp()))
}

/// The Export encoder factory (see the module documentation).
pub fn factory(format: Format, w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>> {
    if s.hardware_encoding != HardwareEncoding::Auto || format != Format::H264 {
        return None;
    }
    let declined = |why: &str| {
        log::info!("hardware encoding declined: {why}");
        filmcraft_export::note_hw_encode_declined();
        None
    };
    let cfg = match config(format, w, h, rate, s) {
        Ok(c) => c,
        Err(why) => return declined(&why),
    };
    match VaH264Encoder::new(cfg) {
        Ok(enc) => {
            filmcraft_export::note_hw_encode_session();
            Some(Ok(Box::new(enc)))
        }
        Err(why) => declined(&why),
    }
}

#[cfg(test)]
mod tests {
    use super::annexb_nals;

    #[test]
    fn splits_annexb() {
        let s = [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 4, 5];
        let n = annexb_nals(&s);
        assert_eq!(n, vec![&[0x67, 1, 2][..], &[0x68, 3][..], &[0x65, 4, 5][..]]);
        assert!(annexb_nals(&[]).is_empty());
        assert!(annexb_nals(&[0, 0]).is_empty());
    }
}
