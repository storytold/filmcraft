//! Support for hardware (OS) video decoders registered by `filmcraft-platform`: the
//! Settings ▸ Playback ▸ Hardware decoding switch, the hardware counters `perf.stats` reports, and
//! what such a decoder needs from an `avcC` / `hvcC` sample entry to behave exactly like our
//! software decoder for the same stream (parameter sets, cropping, colour, pixel aspect, reorder
//! depth, random-access and disposable samples).
//!
//! The software decoders stay the reference: a hardware factory declines streams it cannot
//! decode, and a hardware decoder that fails mid-stream switches to
//! [`crate::software_video_decoder`], so registering one never makes a file undecodable.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use filmcraft_isobmff::{CodecConfig, SampleEntry};

pub use crate::hw_frame::{FrameCodec, FrameStreamInfo, PictureParams};
use crate::video::{avcc_length_size, h264_disposable, hevc_disposable, hvcc_length_size_and_tid, sar_par, vui_color};
use crate::{CodecError, Result};

static ENABLED: AtomicBool = AtomicBool::new(true);

/// Settings ▸ Playback ▸ Hardware decoding: `Auto` (true, the default) lets registered hardware
/// decoders take streams they support; `Off` (false) makes every new decoder a software one.
/// Decoders already created keep running: a source picks the change up when it is reopened.
pub fn set_hardware_decoding(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// Whether hardware decoders may be used for new decoders ([`set_hardware_decoding`]).
pub fn hardware_decoding() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

static BACKEND: std::sync::Mutex<Option<&'static str>> = std::sync::Mutex::new(None);

/// Record the OS decoder backend `filmcraft_platform::register` put in front of our decoders
/// ("VideoToolbox", "Media Foundation"), for `perf.stats`.
pub fn set_hw_backend(name: &'static str) {
    *BACKEND.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(name);
}

/// The registered hardware decoder backend, if any ([`set_hw_backend`]).
pub fn hw_backend() -> Option<&'static str> {
    *BACKEND.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

static GPU_FRAMES: AtomicBool = AtomicBool::new(false);

/// Whether hardware decoders may hand out pictures as GPU surfaces ([`filmcraft_frame::GpuPixels`])
/// instead of CPU planes: set by `filmcraft_platform` once the renderer's device can open them,
/// cleared when the GPU compositor is dropped (a GPU error) or by the platform on a device loss.
pub fn set_gpu_frames(on: bool) {
    GPU_FRAMES.store(on, Ordering::Relaxed);
}

/// See [`set_gpu_frames`].
pub fn gpu_frames() -> bool {
    GPU_FRAMES.load(Ordering::Relaxed)
}

/// Process-wide hardware decoding counters (they only grow; subtract two snapshots to measure an
/// interval). Frames decoded in software are `GopStats::frames - frames`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HwStats {
    /// Pictures output by hardware decoders.
    pub frames: u64,
    /// Hardware decoders created (one per decoder instance; a seek reuses it).
    pub sessions: u64,
    /// Streams a hardware factory handed to the software decoder (unsupported profile, size,
    /// chroma format or bit depth, no hardware, or the switch is Off).
    pub declined: u64,
    /// Hardware decoders that failed mid-stream and continued in software.
    pub fallbacks: u64,
    /// Of `frames`, the pictures handed out as GPU surfaces (zero-copy: no readback, the compositor
    /// samples the decoder's memory).
    pub zero_copy_frames: u64,
}

static ZERO_COPY: AtomicU64 = AtomicU64::new(0);
static FRAMES: AtomicU64 = AtomicU64::new(0);
static SESSIONS: AtomicU64 = AtomicU64::new(0);
static DECLINED: AtomicU64 = AtomicU64::new(0);
static FALLBACKS: AtomicU64 = AtomicU64::new(0);

/// The counters so far.
pub fn hw_stats() -> HwStats {
    HwStats {
        frames: FRAMES.load(Ordering::Relaxed),
        sessions: SESSIONS.load(Ordering::Relaxed),
        declined: DECLINED.load(Ordering::Relaxed),
        fallbacks: FALLBACKS.load(Ordering::Relaxed),
        zero_copy_frames: ZERO_COPY.load(Ordering::Relaxed),
    }
}

/// A hardware decoder output `n` pictures.
pub fn note_hw_frames(n: usize) {
    FRAMES.fetch_add(n as u64, Ordering::Relaxed);
}

/// A hardware decoder output `n` pictures as GPU surfaces (also counted by [`note_hw_frames`]).
pub fn note_hw_zero_copy(n: usize) {
    ZERO_COPY.fetch_add(n as u64, Ordering::Relaxed);
}

/// A hardware decoder was created.
pub fn note_hw_session() {
    SESSIONS.fetch_add(1, Ordering::Relaxed);
}

/// A hardware factory declined a stream it is responsible for.
pub fn note_hw_declined() {
    DECLINED.fetch_add(1, Ordering::Relaxed);
}

/// A hardware decoder switched to software mid-stream.
pub fn note_hw_fallback() {
    FALLBACKS.fetch_add(1, Ordering::Relaxed);
}

/// The two codecs whose `avcC` / `hvcC` streams hardware decoders take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NalCodec {
    H264,
    Hevc,
}

/// What a decoder needs to know about a length-prefixed H.264 / HEVC stream, from its sample
/// entry's configuration record and active sequence parameter set (parsed with the software
/// decoders' own parsers, so both decoders derive the same values).
#[derive(Clone, Debug)]
pub struct NalStreamInfo {
    pub codec: NalCodec,
    /// NAL unit length-prefix size of the samples (1, 2 or 4).
    pub length_size: usize,
    /// HEVC: highest TemporalId (numTemporalLayers - 1), when signalled.
    pub highest_tid: Option<u8>,
    /// The parameter-set NAL units of the record (header included, emulation prevention kept),
    /// in record order: H.264 SPS then PPS; HEVC VPS, SPS, PPS. SEI arrays are left out.
    pub parameter_sets: Vec<Vec<u8>>,
    /// Coded (decoded) picture size in luma samples.
    pub coded: (u32, u32),
    /// Output (cropped) rectangle in luma samples: x, y, width, height.
    pub crop: (u32, u32, u32, u32),
    /// 0 monochrome, 1 4:2:0, 2 4:2:2, 3 4:4:4.
    pub chroma_format_idc: u32,
    pub bit_depth_luma: u32,
    pub bit_depth_chroma: u32,
    /// Field (PAFF / MBAFF) coding (H.264 `frame_mbs_only_flag` 0).
    pub interlaced: bool,
    /// H.264 `profile_idc` / HEVC `general_profile_idc`.
    pub profile_idc: u8,
    /// Colour of every output picture, as the software decoder reports it.
    pub color: filmcraft_color::ColorInfo,
    /// Pixel aspect ratio, as the software decoder reports it.
    pub par: (u32, u32),
    /// Pictures that may precede a picture in decoding order and follow it in output order
    /// (H.264 `max_num_reorder_frames`, HEVC `sps_max_num_reorder_pics`).
    pub reorder: usize,
}

/// Any stream a hardware decoder can take: what [`crate::hw`]'s hybrid decoder needs to know about
/// it to find restart points and in-band parameter changes.
#[derive(Clone, Debug)]
pub enum StreamInfo {
    Nal(NalStreamInfo),
    Frame(FrameStreamInfo),
}

impl From<NalStreamInfo> for StreamInfo {
    fn from(i: NalStreamInfo) -> Self {
        Self::Nal(i)
    }
}

impl From<FrameStreamInfo> for StreamInfo {
    fn from(i: FrameStreamInfo) -> Self {
        Self::Frame(i)
    }
}

impl StreamInfo {
    /// Whether decoding can restart at `sample` with no earlier state (IDR / IRAP, a key frame).
    pub fn is_irap(&self, sample: &[u8]) -> bool {
        match self {
            Self::Nal(i) => i.is_irap(sample),
            Self::Frame(i) => i.is_random_access(sample),
        }
    }

    /// An HEVC CRA restarts nothing by itself (its RASL pictures need the previous restart point).
    pub fn keeps_previous_restart(&self, sample: &[u8]) -> bool {
        match self {
            Self::Nal(i) => i.codec == NalCodec::Hevc && i.nal_types(sample).contains(&21),
            Self::Frame(_) => false,
        }
    }

    /// Whether `sample` carries parameters different from the ones the hardware decoder was set up
    /// with (parameter sets, a sequence header, a key frame of another format).
    pub fn parameters_changed(&self, sample: &[u8]) -> bool {
        match self {
            Self::Nal(i) => {
                i.nals(sample).into_iter().any(|n| n.first().is_some_and(|&h| i.is_parameter_set(i.nal_type(h))) && !i.parameter_sets.iter().any(|p| p == n))
            }
            Self::Frame(i) => i.parameters_changed(sample),
        }
    }

    /// As the software decoder's [`crate::VideoDecoder::is_random_access`].
    pub fn is_random_access(&self, sample: &[u8]) -> Option<bool> {
        match self {
            Self::Nal(i) => i.is_random_access(sample),
            Self::Frame(i) => Some(i.is_random_access(sample)),
        }
    }

    /// As the software decoder's [`crate::VideoDecoder::is_disposable`].
    pub fn is_disposable(&self, sample: &[u8]) -> bool {
        match self {
            Self::Nal(i) => i.is_disposable(sample),
            Self::Frame(_) => false,
        }
    }
}

/// Length-prefixed parameter-set entries (`u16` length + bytes) starting at `pos`.
fn read_sets(rec: &[u8], pos: &mut usize, count: usize, out: &mut Vec<Vec<u8>>) -> Result<()> {
    for _ in 0..count {
        let len = rec
            .get(*pos..*pos + 2)
            .map(|b| u16::from_be_bytes([b[0], b[1]]) as usize)
            .ok_or_else(|| CodecError::Decode("configuration record truncated".into()))?;
        *pos += 2;
        let nal = rec.get(*pos..*pos + len).ok_or_else(|| CodecError::Decode("configuration record truncated".into()))?;
        *pos += len;
        if !nal.is_empty() {
            out.push(nal.to_vec());
        }
    }
    Ok(())
}

impl NalStreamInfo {
    /// The stream info of an `avcC` / `hvcC` sample entry (`None` for other codecs).
    pub fn from_entry(e: &SampleEntry) -> Option<Result<Self>> {
        match &e.codec {
            CodecConfig::Avc(a) => Some(Self::from_avcc(&a.to_bytes())),
            CodecConfig::Hevc(c) => Some(Self::from_hvcc(&c.to_bytes())),
            _ => None,
        }
    }

    /// From an `avcC` (AVCDecoderConfigurationRecord) payload.
    pub fn from_avcc(avcc: &[u8]) -> Result<Self> {
        if avcc.len() < 7 || avcc[0] != 1 {
            return Err(CodecError::Decode("bad avcC record".into()));
        }
        let mut sets = Vec::new();
        let mut pos = 6;
        read_sets(avcc, &mut pos, (avcc[5] & 0x1f) as usize, &mut sets)?;
        let nsps = sets.len();
        let npps = *avcc.get(pos).ok_or_else(|| CodecError::Decode("avcC truncated".into()))? as usize;
        pos += 1;
        read_sets(avcc, &mut pos, npps, &mut sets)?;
        let sps_nal = sets.iter().take(nsps).find(|n| n.first().is_some_and(|h| h & 0x1f == 7)).ok_or_else(|| CodecError::Decode("avcC has no SPS".into()))?;
        if sets.len() == nsps {
            return Err(CodecError::Decode("avcC has no PPS".into()));
        }
        let sps = filmcraft_h264::params::Sps::parse(&filmcraft_bitstream::unescape_rbsp(sps_nal.get(1..).unwrap_or_default()))
            .map_err(|e| CodecError::Decode(e.to_string()))?;
        let crop = sps.crop_rect();
        let vui = sps.vui.clone().unwrap_or_default();
        Ok(Self {
            codec: NalCodec::H264,
            length_size: avcc_length_size(avcc),
            highest_tid: None,
            parameter_sets: sets,
            coded: (sps.width(), sps.height()),
            crop,
            chroma_format_idc: sps.chroma_format_idc,
            bit_depth_luma: sps.bit_depth_luma,
            bit_depth_chroma: sps.bit_depth_chroma,
            interlaced: !sps.frame_mbs_only,
            profile_idc: sps.profile_idc,
            color: vui_color(crop.2, crop.3, vui.matrix_coefficients, vui.transfer_characteristics, vui.full_range),
            par: sar_par(vui.sar),
            // as the software decoder's DPB (`max_reorder`)
            reorder: sps.max_num_reorder_frames().min(sps.max_dpb_frames()),
        })
    }

    /// From an `hvcC` (HEVCDecoderConfigurationRecord) payload.
    pub fn from_hvcc(hvcc: &[u8]) -> Result<Self> {
        if hvcc.len() < 23 || hvcc[0] != 1 {
            return Err(CodecError::Decode("bad hvcC record".into()));
        }
        let (length_size, highest_tid) = hvcc_length_size_and_tid(hvcc);
        let mut sets = Vec::new();
        let mut pos = 23;
        for _ in 0..hvcc[22] {
            let hdr = hvcc.get(pos..pos + 3).ok_or_else(|| CodecError::Decode("hvcC truncated".into()))?;
            let (kind, n) = (hdr[0] & 0x3f, u16::from_be_bytes([hdr[1], hdr[2]]) as usize);
            pos += 3;
            let mut arr = Vec::new();
            read_sets(hvcc, &mut pos, n, &mut arr)?;
            if (32..=34).contains(&kind) {
                sets.extend(arr);
            }
        }
        let nal_type = |n: &[u8]| n.first().map(|h| (h >> 1) & 0x3f);
        let sps_nal = sets.iter().find(|n| nal_type(n) == Some(33)).ok_or_else(|| CodecError::Decode("hvcC has no SPS".into()))?;
        if !sets.iter().any(|n| nal_type(n) == Some(32)) || !sets.iter().any(|n| nal_type(n) == Some(34)) {
            return Err(CodecError::Decode("hvcC lacks a VPS or PPS".into()));
        }
        let sps = filmcraft_hevc::params::Sps::parse(&filmcraft_bitstream::unescape_rbsp(sps_nal.get(2..).unwrap_or_default()))
            .map_err(|e| CodecError::Decode(e.to_string()))?;
        let crop = sps.crop_rect();
        let vui = sps.vui.clone().unwrap_or_default();
        Ok(Self {
            codec: NalCodec::Hevc,
            length_size,
            highest_tid,
            parameter_sets: sets,
            coded: (sps.width, sps.height),
            crop,
            chroma_format_idc: sps.chroma_format_idc,
            bit_depth_luma: sps.bit_depth_luma,
            bit_depth_chroma: sps.bit_depth_chroma,
            interlaced: false,
            profile_idc: sps.ptl.profile_idc,
            color: vui_color(crop.2, crop.3, vui.matrix_coefficients, vui.transfer_characteristics, vui.full_range),
            par: sar_par(vui.sar),
            reorder: sps.max_num_reorder as usize,
        })
    }

    /// NAL unit types of a sample (stops at the first malformed length prefix).
    pub fn nal_types(&self, sample: &[u8]) -> Vec<u8> {
        self.nals(sample).into_iter().filter_map(|n| n.first().map(|&h| self.nal_type(h))).collect()
    }

    /// The NAL units of a length-prefixed sample (stops at the first malformed length prefix).
    pub fn nals<'a>(&self, sample: &'a [u8]) -> Vec<&'a [u8]> {
        let ls = self.length_size;
        let mut out = Vec::new();
        let mut pos = 0usize;
        while let Some(pre) = sample.get(pos..pos.saturating_add(ls)) {
            if !(1..=4).contains(&ls) {
                break;
            }
            let len = pre.iter().fold(0usize, |a, &b| (a << 8) | b as usize);
            pos += ls;
            let Some(n) = sample.get(pos..pos.saturating_add(len)) else { break };
            pos += len;
            out.push(n);
        }
        out
    }

    /// The NAL unit type in a NAL header byte.
    pub fn nal_type(&self, header: u8) -> u8 {
        match self.codec {
            NalCodec::H264 => header & 0x1f,
            NalCodec::Hevc => (header >> 1) & 0x3f,
        }
    }

    /// Whether `t` is a parameter-set NAL unit type (H.264 SPS / PPS / SPS extension, HEVC VPS /
    /// SPS / PPS).
    pub fn is_parameter_set(&self, t: u8) -> bool {
        match self.codec {
            NalCodec::H264 => matches!(t, 7 | 8 | 13 | 15),
            NalCodec::Hevc => (32..=34).contains(&t),
        }
    }

    /// Whether decoding can restart at `sample` with no earlier state: an IDR access unit (H.264)
    /// or an IRAP one (HEVC IDR / CRA / BLA).
    pub fn is_irap(&self, sample: &[u8]) -> bool {
        self.nal_types(sample).iter().any(|&t| match self.codec {
            NalCodec::H264 => t == 5,
            NalCodec::Hevc => (16..=23).contains(&t),
        })
    }

    /// As the software decoder's [`crate::VideoDecoder::is_random_access`] for this stream
    /// (length-prefixed samples: unknown, the container's sync flags are trusted).
    pub fn is_random_access(&self, _sample: &[u8]) -> Option<bool> {
        None
    }

    /// As the software decoder's [`crate::VideoDecoder::is_disposable`] for this stream.
    pub fn is_disposable(&self, sample: &[u8]) -> bool {
        match self.codec {
            NalCodec::H264 => h264_disposable(sample, self.length_size),
            NalCodec::Hevc => hevc_disposable(sample, self.length_size, self.highest_tid),
        }
    }

    /// Output picture size (cropped).
    pub fn size(&self) -> (u32, u32) {
        (self.crop.2, self.crop.3)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switch_defaults_to_auto() {
        // other tests in this process may flip it; only check the round trip
        let was = hardware_decoding();
        set_hardware_decoding(false);
        assert!(!hardware_decoding());
        set_hardware_decoding(true);
        assert!(hardware_decoding());
        set_hardware_decoding(was);
    }

    #[test]
    fn hostile_records_are_errors() {
        for rec in [&[][..], &[1, 2, 3], &[1, 0x64, 0, 0x1f, 0xff, 0xe1, 0, 9, 0x67], &[0; 30]] {
            assert!(NalStreamInfo::from_avcc(rec).is_err());
            assert!(NalStreamInfo::from_hvcc(rec).is_err());
        }
    }

    #[test]
    fn nal_splitting_is_bounded() {
        let info = NalStreamInfo {
            codec: NalCodec::Hevc,
            length_size: 4,
            highest_tid: None,
            parameter_sets: Vec::new(),
            coded: (16, 16),
            crop: (0, 0, 16, 16),
            chroma_format_idc: 1,
            bit_depth_luma: 8,
            bit_depth_chroma: 8,
            interlaced: false,
            profile_idc: 1,
            color: filmcraft_color::ColorInfo::REC709,
            par: (1, 1),
            reorder: 0,
        };
        // CRA (21) then a length that runs past the end
        let s = [0, 0, 0, 2, 21 << 1, 1, 0xff, 0xff, 0xff, 0xff, 1];
        assert_eq!(info.nal_types(&s), vec![21]);
        assert!(info.is_irap(&s));
        assert!(!info.is_irap(&[0, 0, 0, 2, 1 << 1, 1]));
    }
}
