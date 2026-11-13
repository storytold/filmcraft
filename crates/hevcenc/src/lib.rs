//! Clean-room, pure-Rust H.265/HEVC software encoder, written from the public
//! ITU-T Rec. H.265 specification (v8, 08/2021; ISO/IEC 23008-2).
//!
//! Phase 1 (this crate today):
//! - Main profile, 4:2:0 8-bit, frame coding; CTU 64 with CU splits down to 8x8.
//! - I-frames (35 intra modes) and P-frames (zero-motion skip plus integer motion search).
//! - 4x4–32x32 integer transforms with deadzone quantisation, full CABAC, normative deblocking.
//! - Rate control: constant QP, constant quality (CRF-like), 1-pass ABR with VBV.
//! - `PacketFormat::AnnexB` (default, parameter sets in-band on every IDR) and
//!   `PacketFormat::LengthPrefixed` (MP4 samples; sets out-of-band via [`Encoder::hvcc`]).
//! - Slice threading is a follow-up: one slice per picture for now.
//!
//! ```no_run
//! use filmcraft_hevcenc::{Encoder, EncoderConfig, RateControl, YuvFrame};
//! let mut cfg = EncoderConfig::new(1280, 720, 30, 1);
//! cfg.rate = RateControl::Crf(22.0);
//! let mut enc = Encoder::new(cfg).unwrap();
//! let (y, u, v) = (vec![128u8; 1280 * 720], vec![128u8; 640 * 360], vec![128u8; 640 * 360]);
//! let frame = YuvFrame { y: &y, u: &u, v: &v, y_stride: 1280, uv_stride: 640 };
//! let mut packets = enc.encode(&frame, 0).unwrap();
//! packets.extend(enc.flush());
//! ```

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

mod cabac;
mod deblock;
mod dsp;
mod inter;
mod intra;
mod lookahead;
mod params;
mod picture;
mod ratecontrol;
mod slice;
// Normative tables with subsets reserved for follow-up tools (SAO, tiles, WPP, range extensions).
#[allow(dead_code)]
mod tables;
mod transform;

use lookahead::{LowRes, aq_offsets, complexity, is_scene_cut};
use picture::{Frame, RefPic, import_yuv};
use ratecontrol::Rc;

pub use ratecontrol::{FrameStat, PassStats};

/// Encoder errors.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid dimensions {0}x{1} (must be even and non-zero)")]
    InvalidDimensions(u32, u32),
    #[error("invalid frame rate {0}/{1}")]
    InvalidFrameRate(u32, u32),
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    #[error("input frame planes too small for the configured size")]
    BadFrame,
    #[error("unsupported: {0}")]
    Unsupported(String),
}

/// Coded profile. Phase 1 writes Main only; the VPS/SPS/PTL already carry
/// `general_profile_idc = 1` so Main 10 arrives without a header change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Profile {
    /// Main: CABAC, I/P frames, 4:2:0 8-bit.
    #[default]
    Main,
}

/// Speed / quality trade-off.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Preset {
    Speed,
    #[default]
    Balanced,
    Quality,
}

/// Frame-level rate control.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RateControl {
    /// Constant quality; roughly the QP of P frames (0–51, lower is better).
    Crf(f32),
    /// Constant bitrate target (1-pass ABR with a 1-second VBV at `kbps`).
    Cbr { kbps: u32 },
    /// Variable bitrate: average `target_kbps`, VBV-limited to `max_kbps`.
    Vbr { target_kbps: u32, max_kbps: u32 },
    /// Constant QP (I frames use QP − 2, clamped).
    Qp(u8),
}

/// Multi-pass mode for bitrate-targeted rate control.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Pass {
    #[default]
    Single,
    /// First pass: encode and collect [`PassStats`] via [`Encoder::pass_stats`].
    First,
    /// Second pass using the statistics of the first (follow-up: rejected for now).
    Second(PassStats),
}

/// Colour description written to the VUI (ITU-T H.273 code points).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColorConfig {
    pub primaries: u8,
    pub transfer: u8,
    pub matrix: u8,
    pub full_range: bool,
}

impl Default for ColorConfig {
    /// BT.709, limited range.
    fn default() -> Self {
        ColorConfig { primaries: 1, transfer: 1, matrix: 1, full_range: false }
    }
}

/// How coded pictures leave the encoder.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PacketFormat {
    /// `00 00 00 01`-prefixed NAL units; VPS/SPS/PPS in-band on every IDR.
    #[default]
    AnnexB,
    /// 4-byte big-endian lengths; parameter sets out-of-band via [`Encoder::hvcc`].
    LengthPrefixed,
}

/// Encoder configuration. Start from [`EncoderConfig::new`] and override fields.
#[derive(Clone, Debug)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    pub profile: Profile,
    pub preset: Preset,
    pub rate: RateControl,
    /// IDR interval in pictures (a scene cut also inserts one when `scenecut`).
    pub keyint: u32,
    /// Phase 1 stores 0 only; B-frames are a follow-up.
    pub bframes: u32,
    pub color: ColorConfig,
    /// Rayon threads for analysis (0 = automatic). Single-slice coding stays serial.
    pub threads: u32,
    pub pass: Pass,
    pub format: PacketFormat,
    /// Variance-adaptive quantisation strength (0 = off).
    pub aq_strength: f32,
    /// Sample aspect ratio.
    pub sar: (u16, u16),
    /// Insert an IDR on a scene cut.
    pub scenecut: bool,
    /// Emit an access-unit delimiter per picture (Annex-B only).
    pub aud: bool,
    /// `general_level_idc` override (30 = level 1, 120 = level 4.0, …); auto-picked when `None`.
    pub level: Option<u8>,
}

impl EncoderConfig {
    pub fn new(width: u32, height: u32, fps_num: u32, fps_den: u32) -> Self {
        EncoderConfig {
            width,
            height,
            fps_num,
            fps_den,
            profile: Profile::Main,
            preset: Preset::Balanced,
            rate: RateControl::Crf(23.0),
            keyint: 250,
            bframes: 0,
            color: ColorConfig::default(),
            threads: 0,
            pass: Pass::Single,
            format: PacketFormat::AnnexB,
            aq_strength: 0.6,
            sar: (1, 1),
            scenecut: true,
            aud: true,
            level: None,
        }
    }
}

/// One input picture: planar 4:2:0 8-bit.
#[derive(Clone, Copy, Debug)]
pub struct YuvFrame<'a> {
    pub y: &'a [u8],
    pub u: &'a [u8],
    pub v: &'a [u8],
    pub y_stride: usize,
    pub uv_stride: usize,
}

/// Coded picture type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameType {
    Idr,
    I,
    P,
}

/// One coded picture in coding order.
#[derive(Clone, Debug)]
pub struct Packet {
    pub data: Vec<u8>,
    pub pts: i64,
    pub dts: i64,
    pub keyframe: bool,
    pub frame_type: FrameType,
    pub qp: u8,
}

/// A reconstructed picture, for tests proving encoder/decoder agreement.
#[derive(Clone, Debug)]
pub struct ReconFrame {
    pub pts: i64,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
}

/// Maximum coded size; the SPS frame cropping signals the visible window.
pub const MAX_DIMENSION: u32 = 16384;

/// The H.265 software encoder.
pub struct Encoder {
    cfg: EncoderConfig,
    level_idc: u8,
    vps: Vec<u8>,
    sps: Vec<u8>,
    pps: Vec<u8>,
    init_qp: i32,
    log2_max_poc_lsb: u32,
    coded_w: usize,
    coded_h: usize,
    ctbs_w: usize,
    ctbs_h: usize,
    cur: Frame,
    ref_pic: Option<RefPic>,
    poc: u32,
    rc: Rc,
    lowres_prev: Option<LowRes>,
    lowres_prev_cplx: Option<lookahead::Complexity>,
    first_picture: bool,
    capture_recon: bool,
    recon: Vec<ReconFrame>,
}

impl Encoder {
    /// Validate the configuration and write the VPS/SPS/PPS. [`Error::Unsupported`]
    /// is returned only for `Pass::Second` (two-pass is a follow-up).
    pub fn new(cfg: EncoderConfig) -> Result<Self, Error> {
        if cfg.width == 0
            || cfg.height == 0
            || !cfg.width.is_multiple_of(2)
            || !cfg.height.is_multiple_of(2)
            || cfg.width > MAX_DIMENSION
            || cfg.height > MAX_DIMENSION
        {
            return Err(Error::InvalidDimensions(cfg.width, cfg.height));
        }
        if cfg.fps_num == 0 || cfg.fps_den == 0 {
            return Err(Error::InvalidFrameRate(cfg.fps_num, cfg.fps_den));
        }
        if cfg.bframes != 0 {
            return Err(Error::InvalidConfig(format!("B-frames are a follow-up (got bframes={}), phase 1 codes I/P only", cfg.bframes)));
        }
        if cfg.keyint < 1 {
            return Err(Error::InvalidConfig("keyint must be at least 1".to_string()));
        }
        match cfg.rate {
            RateControl::Qp(qp) if qp > 51 => {
                return Err(Error::InvalidConfig(format!("QP {qp} out of range 0..=51")));
            }
            RateControl::Crf(crf) if !(0.0..=51.0).contains(&crf) => {
                return Err(Error::InvalidConfig(format!("CRF {crf} out of range 0..=51")));
            }
            RateControl::Cbr { kbps } | RateControl::Vbr { target_kbps: kbps, .. } if kbps == 0 => {
                return Err(Error::InvalidConfig("bitrate target must be non-zero".to_string()));
            }
            _ => {}
        }
        if matches!(cfg.pass, Pass::Second(_)) {
            return Err(Error::Unsupported("two-pass second pass arrives in a follow-up".to_string()));
        }
        if let Some(level) = cfg.level
            && !params::is_legal_level_idc(level)
        {
            return Err(Error::InvalidConfig(format!("illegal general_level_idc {level}")));
        }
        let level_idc = cfg.level.unwrap_or_else(|| params::pick_level(cfg.width, cfg.height, cfg.fps_num, cfg.fps_den, params::target_kbps(cfg.rate)));
        let init_qp = match cfg.rate {
            RateControl::Qp(qp) => i32::from(qp),
            RateControl::Crf(crf) => crf.round() as i32,
            RateControl::Cbr { .. } | RateControl::Vbr { .. } => 26,
        };
        let poc_lsb_minus4 = params::poc_lsb_minus4(cfg.keyint);
        let sps = params::Sps {
            width: cfg.width,
            height: cfg.height,
            sar: cfg.sar,
            color: cfg.color,
            fps_num: cfg.fps_num,
            fps_den: cfg.fps_den,
            level_idc,
            poc_lsb_minus4,
        };
        let vps = params::assemble(params::NAL_VPS, &params::Vps { level_idc }.rbsp());
        let sps = params::assemble(params::NAL_SPS, &sps.rbsp());
        let pps = params::assemble(params::NAL_PPS, &params::Pps { init_qp }.rbsp());
        // Coded (CTU-aligned) geometry and the reconstruction working buffer.
        let coded_w = (cfg.width as usize).div_ceil(64) * 64;
        let coded_h = (cfg.height as usize).div_ceil(64) * 64;
        let cur = Frame::new(coded_w, coded_h, crate::slice::REF_PAD);
        // Frame-level rate control: Single shares Rc::new, First collects pass-1 stats.
        let fps = cfg.fps_num as f64 / cfg.fps_den.max(1) as f64;
        let rc = if matches!(cfg.pass, Pass::First) { Rc::new_first_pass(cfg.rate, fps, None) } else { Rc::new(cfg.rate, fps, None) };
        Ok(Encoder {
            cfg,
            level_idc,
            vps,
            sps,
            pps,
            init_qp,
            log2_max_poc_lsb: poc_lsb_minus4 + 4,
            coded_w,
            coded_h,
            ctbs_w: coded_w.div_ceil(64),
            ctbs_h: coded_h.div_ceil(64),
            cur,
            ref_pic: None,
            poc: 0,
            rc,
            lowres_prev: None,
            lowres_prev_cplx: None,
            first_picture: true,
            capture_recon: false,
            recon: Vec::new(),
        })
    }

    pub fn config(&self) -> &EncoderConfig {
        &self.cfg
    }

    /// Signalled `general_level_idc` (picked or overridden).
    pub fn level_idc(&self) -> u8 {
        self.level_idc
    }

    /// Complete VPS/SPS/PPS NAL units (headers included, framing excluded).
    pub fn parameter_sets(&self) -> (&[u8], &[u8], &[u8]) {
        (&self.vps, &self.sps, &self.pps)
    }

    /// `HEVCDecoderConfigurationRecord` for the MP4 `hvcC` box.
    pub fn hvcc(&self) -> Vec<u8> {
        params::hvcc(&self.vps, &self.sps, &self.pps, self.level_idc)
    }

    /// Reorder delay in pictures (phase 1 has no B-frames, always 0).
    pub fn delay(&self) -> usize {
        0
    }

    pub fn set_recon_capture(&mut self, on: bool) {
        self.capture_recon = on;
        if !on {
            self.recon.clear();
        }
    }

    pub fn take_recon(&mut self) -> Vec<ReconFrame> {
        std::mem::take(&mut self.recon)
    }

    /// First-pass statistics (`Some` only for [`Pass::First`], collected by [`Rc`]).
    pub fn pass_stats(&self) -> Option<PassStats> {
        if matches!(self.cfg.pass, Pass::First) { self.rc.first_pass_stats() } else { None }
    }

    pub fn encode(&mut self, frame: &YuvFrame<'_>, pts: i64) -> Result<Vec<Packet>, Error> {
        self.try_encode(frame, pts)
    }

    pub fn try_encode(&mut self, frame: &YuvFrame<'_>, pts: i64) -> Result<Vec<Packet>, Error> {
        // Import the visible picture into a coded-size source frame (edges replicate over the overhang).
        let mut src = Frame::new(self.coded_w, self.coded_h, crate::slice::REF_PAD);
        import_yuv(frame.y, frame.u, frame.v, frame.y_stride, frame.uv_stride, self.cfg.width as usize, self.cfg.height as usize, &mut src)?;
        // Half-res lookahead + complexity against the previous picture.
        let lowres = LowRes::new(&src);
        let cplx = complexity(&lowres, self.lowres_prev.as_ref());
        let scenecut =
            self.cfg.scenecut && !self.first_picture && self.lowres_prev_cplx.is_some() && is_scene_cut(self.lowres_prev_cplx.as_ref().unwrap_or(&cplx), &cplx);
        // IDR decision mirrors h264enc: first picture, keyint interval, or a scene cut.
        let since_idr = self.poc;
        let is_idr = self.first_picture || since_idr >= self.cfg.keyint.max(1) || scenecut;
        let poc = if is_idr { 0 } else { self.poc };
        // Frame QP from rate control (I/IDR uses QP-2 inside Rc).
        let slice_qp = self.rc.frame_qp(is_idr, &cplx);
        // Per-CTU AQ offsets (exactly 0 when strength is 0).
        let aq = aq_offsets(&src, self.ctbs_w, self.ctbs_h, self.cfg.aq_strength);
        // Code the single slice (recon is written post-deblock into self.cur).
        let out = crate::slice::code_picture(
            &src,
            &mut self.cur,
            self.ref_pic.as_ref(),
            is_idr,
            poc,
            self.log2_max_poc_lsb,
            self.init_qp,
            slice_qp,
            self.cfg.preset,
            &aq,
        )?;
        // NAL assembly: AnnexB = [AUD] + [VPS,SPS,PPS on IDR] + [prefix SEI on the first picture only] + slice.
        let aud_primary = if is_idr { 0 } else { 1 };
        let slice_nal = out.slice_nal;
        let (packet_data, bits) = if self.cfg.format == PacketFormat::AnnexB {
            let mut nals: Vec<Vec<u8>> = Vec::new();
            if self.cfg.aud {
                nals.push(params::assemble(params::NAL_AUD, &crate::slice::aud_rbsp(aud_primary)));
            }
            if is_idr {
                nals.push(self.vps.clone());
                nals.push(self.sps.clone());
                nals.push(self.pps.clone());
            }
            if self.first_picture {
                nals.push(params::assemble(params::NAL_SEI_PREFIX, &crate::slice::sei_prefix_rbsp()));
            }
            nals.push(slice_nal);
            let packet_data = params::frame_annexb(&nals.iter().map(|n| n.as_slice()).collect::<Vec<_>>());
            let bits = (packet_data.len() as u64).saturating_mul(8);
            (packet_data, bits)
        } else {
            let packet_data = params::frame_length_prefixed(&[slice_nal.as_slice()]);
            let bits = (packet_data.len() as u64).saturating_mul(8);
            (packet_data, bits)
        };
        self.rc.update(bits, is_idr);
        self.rc.record_first_pass(FrameStat { slice_type: if is_idr { 0 } else { 1 }, qscale: slice_qp as f32, bits });
        // DPB: this picture becomes the single reference (edges extended for sub-pel MC).
        self.cur.y.extend_edges();
        self.cur.u.extend_edges();
        self.cur.v.extend_edges();
        if self.capture_recon {
            let (w, h) = (self.cfg.width as usize, self.cfg.height as usize);
            // `copy_out` yields coded-size rows; crop to the visible window here.
            let mut full_y = Vec::new();
            self.cur.y.copy_out(&mut full_y);
            let mut full_u = Vec::new();
            self.cur.u.copy_out(&mut full_u);
            let mut full_v = Vec::new();
            self.cur.v.copy_out(&mut full_v);
            let (cw, ch) = (self.coded_w, self.coded_h);
            let (cw2, _ch2) = (cw / 2, ch / 2);
            let mut y = Vec::with_capacity(w.saturating_mul(h));
            for yy in 0..h {
                let s = yy.saturating_mul(cw);
                if let Some(row) = full_y.get(s..s.saturating_add(w)) {
                    y.extend_from_slice(row);
                } else {
                    y.extend(std::iter::repeat_n(128, w));
                }
            }
            let mut u = Vec::with_capacity(w.saturating_mul(h) / 4);
            let mut v = Vec::with_capacity(w.saturating_mul(h) / 4);
            for yy in 0..h / 2 {
                let s = yy.saturating_mul(cw2);
                if let Some(row) = full_u.get(s..s.saturating_add(w / 2)) {
                    u.extend_from_slice(row);
                } else {
                    u.extend(std::iter::repeat_n(128, w / 2));
                }
                if let Some(row) = full_v.get(s..s.saturating_add(w / 2)) {
                    v.extend_from_slice(row);
                } else {
                    v.extend(std::iter::repeat_n(128, w / 2));
                }
            }
            self.recon.push(ReconFrame { pts, y, u, v });
        }
        self.ref_pic = Some(RefPic { frame: self.cur.clone() });
        self.lowres_prev = Some(lowres);
        self.lowres_prev_cplx = Some(cplx);
        self.poc = if is_idr { 1 } else { self.poc.saturating_add(1) };
        self.first_picture = false;
        Ok(vec![Packet { data: packet_data, pts, dts: pts, keyframe: is_idr, frame_type: if is_idr { FrameType::Idr } else { FrameType::P }, qp: slice_qp }])
    }

    /// Reorder delay is 0 (no B-frames); nothing is buffered, so flushing yields no packets.
    pub fn flush(&mut self) -> Vec<Packet> {
        Vec::new()
    }
}
