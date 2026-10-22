//! VideoToolbox H.264, HEVC, ProRes and Motion JPEG behind the export trait.
//!
//! H.264/ProRes/JPEG are opt-in and decline to the built-in encoders when a hardware session
//! cannot preserve the configuration. Choosing HEVC is itself the opt-in; unsupported requests
//! report the reason. Main 10 / Main 4:2:2 10 preserve SDR/PQ/HLG, ProRes 4444/XQ preserve alpha,
//! and HEVC Main supports straight SDR alpha. A mid-export error stops rather than splicing
//! incompatible streams. See docs/macos-videotoolbox.md for tested limits and precision.

use std::sync::{Once, OnceLock};

use filmcraft_export::{BitrateMode, EncodedPacket, EncoderFrame, ExportError, ExportSettings, Format, H264Pass, H264Profile, HardwareEncoding, VideoEncoder};
use filmcraft_isobmff::{AvcConfig, HevcConfig, SampleEntry};
use filmcraft_time::FrameRate;

use crate::videotoolbox_encode::{ParamSets, VtCodec, VtConfig, VtEncoder, VtOptions, VtPacket, VtProfile, VtRate};

/// The hardware encoder's configuration for an export, or why it is not used for it.
pub fn config_for(w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Result<VtConfig, String> {
    let format = s.video_format();
    let hevc = format == Format::Hevc;
    if !hevc && s.hardware_encoding != HardwareEncoding::Auto {
        return Err("hardware encoding is off".into());
    }
    if s.format.is_mxf() && format != Format::ProRes {
        return Err("MXF carries an Annex B stream".into());
    }
    if matches!(format, Format::H264 | Format::Hevc) && (s.bitrate_mode == BitrateMode::Vbr2Pass || !matches!(s.h264_pass, H264Pass::Single)) {
        return Err("two-pass encoding".into());
    }
    if matches!(format, Format::H264 | Format::Hevc) && s.bitrate_mode == BitrateMode::Crf {
        return Err("CRF (constant quality)".into());
    }
    if s.signal.is_hdr() && !matches!(format, Format::Hevc | Format::ProRes) {
        return Err("HDR".into());
    }
    if !w.is_multiple_of(2) || !h.is_multiple_of(2) {
        return Err("odd picture size".into());
    }
    if s.pixel_aspect.is_some_and(|(n, d)| n != d) {
        return Err("non-square pixels".into());
    }
    let timescale = u32::try_from(rate.num).ok().filter(|n| *n > 0).ok_or("invalid frame rate")?;
    let frame_duration = u32::try_from(rate.den).ok().filter(|d| *d > 0).ok_or("invalid frame rate")?;
    if hevc && s.keeps_alpha() && (s.signal.is_hdr() || !matches!(s.hevc_profile.as_str(), "" | "auto" | "main")) {
        return Err("HEVC alpha requires Main 8-bit SDR; choose ProRes 4444 for HDR with alpha".into());
    }
    let kbps = s.bitrate_kbps.max(100);
    let max_kbps = s.max_bitrate_kbps.filter(|m| *m >= kbps).unwrap_or(kbps.saturating_mul(3) / 2);
    let fps = f64::from(timescale) / f64::from(frame_duration);
    Ok(VtConfig {
        width: w,
        height: h,
        timescale,
        frame_duration,
        keyframe_interval: s.keyframe_distance.filter(|k| *k > 0).unwrap_or_else(|| (fps * 2.0).round().max(1.0) as u32),
        profile: match (hevc, s.h264_profile) {
            (true, _) => match s.hevc_profile.to_ascii_lowercase().as_str() {
                "" | "auto" if s.signal.is_hdr() => VtProfile::HevcMain10,
                "" | "auto" | "main" if !s.signal.is_hdr() => VtProfile::HevcMain,
                "main10" => VtProfile::HevcMain10,
                "main42210" => VtProfile::HevcMain42210,
                _ => return Err("invalid HEVC profile or an 8-bit profile requested for HDR".into()),
            },
            (false, _) if format == Format::ProRes => VtProfile::ProRes(filmcraft_export::prores_profile(&s.prores_profile).fourcc()),
            (false, _) if format == Format::Mjpeg => VtProfile::Jpeg,
            (false, H264Profile::Baseline) => VtProfile::Baseline,
            (false, H264Profile::Main) => VtProfile::Main,
            (false, H264Profile::High) => VtProfile::High,
        },
        rate: match s.bitrate_mode {
            BitrateMode::Cbr => VtRate::Cbr { kbps },
            _ => VtRate::Vbr { target_kbps: kbps, max_kbps },
        },
    })
}

static HEVC_PROBE: OnceLock<bool> = OnceLock::new();

/// Whether this machine has a hardware HEVC encoder (asked once, by creating a small session, which
/// takes about 0.1 s): what makes `Format::Hevc` available. [`warm_hevc_probe`] asks it ahead of the
/// first caller.
pub fn hevc_available() -> bool {
    *HEVC_PROBE.get_or_init(|| {
        let probe = VtConfig {
            width: 640,
            height: 360,
            timescale: 30,
            frame_duration: 1,
            keyframe_interval: 30,
            profile: VtProfile::HevcMain,
            rate: VtRate::Vbr { target_kbps: 2000, max_kbps: 3000 },
        };
        let result = VtEncoder::new(probe);
        if let Err(why) = &result {
            log::info!("no hardware HEVC encoder: {why}");
        }
        result.is_ok()
    })
}

/// Whether [`hevc_available`] has its answer yet (diagnostics and tests).
pub fn hevc_probed() -> bool {
    HEVC_PROBE.get().is_some()
}

/// Ask [`hevc_available`] now, on a thread of its own, so that the first draw of the format list does
/// not spend its 0.1 s creating a hardware session on the UI thread. Whoever asks while the thread
/// runs waits for the same answer (no longer than asking first); if the thread cannot start, or the
/// probe panics, the first caller asks, as before. Only the first call starts a thread.
pub fn warm_hevc_probe() {
    static STARTED: Once = Once::new();
    STARTED.call_once(|| {
        let spawned = std::thread::Builder::new().name("filmcraft-hevc-probe".into()).spawn(|| {
            let _ = std::panic::catch_unwind(|| {
                hevc_available();
                hevc_hdr_available();
            });
        });
        if let Err(why) = spawned {
            log::info!("HEVC probe not started ahead of time: {why}");
        }
    });
}

/// The encoder factory `register` puts in front of the built-in ones: a hardware encoder when the
/// export asks for one (H.264 with `hardware_encoding`; HEVC always) and the OS takes the
/// configuration, `None` (the built-in encoder, if there is one) otherwise.
pub fn videotoolbox_encoder_factory(
    format: Format,
    w: u32,
    h: u32,
    rate: FrameRate,
    s: &ExportSettings,
) -> Option<filmcraft_export::Result<Box<dyn VideoEncoder>>> {
    if format != s.video_format() {
        return None;
    }
    let name = match format {
        Format::H264 if s.hardware_encoding == HardwareEncoding::Auto => "H.264",
        Format::Hevc => "HEVC",
        Format::ProRes if s.hardware_encoding == HardwareEncoding::Auto => "ProRes",
        Format::Mjpeg if s.hardware_encoding == HardwareEncoding::Auto => "Motion JPEG",
        _ => return None,
    };
    let config = match config_for(w, h, rate, s) {
        Ok(c) => c,
        Err(why) => {
            log::info!("hardware {name} encoding declined: {why}");
            filmcraft_export::note_hw_encode_declined();
            return if format == Format::Hevc { Some(Err(ExportError::Unsupported(format!("VideoToolbox {name}: {why}")))) } else { None };
        }
    };
    let options = VtOptions { signal: s.signal, alpha: s.keeps_alpha(), quality: s.quality };
    match VtEncoder::with_options(config, options) {
        Ok(vt) => {
            filmcraft_export::note_hw_encode_session();
            Some(Ok(Box::new(HardwareEncoder { vt, signal: s.signal, alpha: s.keeps_alpha(), decoded: 0 })))
        }
        Err(why) => {
            log::info!("hardware {name} encoding declined: {why}");
            filmcraft_export::note_hw_encode_declined();
            if format == Format::Hevc { Some(Err(ExportError::Unsupported(format!("VideoToolbox {name}: {why}")))) } else { None }
        }
    }
}

/// The `hvcC` configuration of an HEVC stream: VideoToolbox's own record when it wrote one.
fn hevc_config(p: &ParamSets) -> Result<HevcConfig, String> {
    let record = p.hvcc.as_deref().ok_or("VideoToolbox wrote no hvcC record for the stream")?;
    let cfg = HevcConfig::parse(record).map_err(|e| format!("unreadable hvcC record: {e}"))?;
    if cfg.vps().is_empty() || cfg.sps().is_empty() || cfg.pps().is_empty() || cfg.length_size != 4 {
        return Err("the hvcC record lacks a parameter set or does not use 4-byte NAL lengths".into());
    }
    Ok(cfg)
}

/// [`VtEncoder`] as a `VideoEncoder`: RGBA pictures in, MP4 samples out.
pub struct HardwareEncoder {
    vt: VtEncoder,
    /// Compressed frames returned so far.
    decoded: i64,
    signal: filmcraft_export::ColorSignal,
    alpha: bool,
}

impl HardwareEncoder {
    /// MP4 samples from compressed frames. Without frame reordering every frame is presented at
    /// its decode time, so composition offsets are 0; a frame whose time is not `n × duration`
    /// after the first (a dropped or reordered frame) is an error, never a file with wrong times.
    fn samples(&mut self, packets: Vec<VtPacket>) -> filmcraft_export::Result<Vec<EncodedPacket>> {
        let duration = self.vt.config().frame_duration;
        let mut out = Vec::with_capacity(packets.len());
        for p in packets {
            let n = self.decoded;
            self.decoded += 1;
            if n.checked_mul(i64::from(duration)) != Some(p.pts) || p.pts != p.dts {
                return Err(ExportError::Encode(format!(
                    "VideoToolbox returned frame {n} at time {} (decode time {}), expected {}",
                    p.pts,
                    p.dts,
                    n.saturating_mul(i64::from(duration))
                )));
            }
            out.push(EncodedPacket { data: p.data, key: p.key, duration, composition_offset: 0 });
        }
        Ok(out)
    }
}

impl VideoEncoder for HardwareEncoder {
    fn sample_entry(&self) -> SampleEntry {
        let c = self.vt.config();
        let (w, h) = (c.width.min(u32::from(u16::MAX)) as u16, c.height.min(u32::from(u16::MAX)) as u16);
        match (c.profile.codec(), self.vt.parameter_sets()) {
            (VtCodec::H264, p) => {
                let (sps, pps) = p.map(|p| (p.sps, p.pps)).unwrap_or_default();
                SampleEntry::avc(AvcConfig::new(sps, pps, 4), w, h)
            }
            // `encode` refuses to go on without a usable record, so a stream that got this far has one
            (VtCodec::Hevc, p) => {
                let mut e = SampleEntry::hevc(p.as_ref().and_then(|p| hevc_config(p).ok()).unwrap_or_default(), w, h);
                self.signal.apply_to(&mut e, false);
                e
            }
            (VtCodec::ProRes, _) => {
                let fourcc = match c.profile {
                    VtProfile::ProRes(c) => c,
                    _ => *b"apch",
                };
                let mut e = SampleEntry::prores(filmcraft_isobmff::FourCc(fourcc), w, h);
                self.signal.apply_to(&mut e, true);
                e
            }
            (VtCodec::Jpeg, _) => SampleEntry::jpeg(w, h),
        }
    }

    fn timescale(&self) -> u32 {
        self.vt.config().timescale
    }

    fn encode(&mut self, f: &EncoderFrame) -> filmcraft_export::Result<Vec<EncodedPacket>> {
        let c = self.vt.config().clone();
        let codec = c.profile.codec();
        if f.hdr.is_some() && !c.profile.ten_bit() {
            return Err(ExportError::Unsupported("hardware encoding does not take HDR pictures".into()));
        }
        if (f.width, f.height) != (c.width, c.height) {
            return Err(ExportError::Encode(format!("picture is {}x{}, the encoder was created for {}x{}", f.width, f.height, c.width, c.height)));
        }
        let (w, h) = (f.width as usize, f.height as usize);
        if (f.hdr.is_none() || self.alpha) && f.rgba.len() != w.saturating_mul(h).saturating_mul(4) {
            return Err(ExportError::Encode("the RGBA picture is smaller than its size".into()));
        }
        let packets = if c.profile.ten_bit() {
            let (kr, kb) = self.signal.kr_kb();
            self.vt.encode_frame10(f.index, f.rgba, f.hdr, kr, kb).map_err(ExportError::Encode)?
        } else if self.alpha {
            self.vt.encode_rgba(f.index, f.rgba).map_err(ExportError::Encode)?
        } else {
            self.vt.encode_rgba_nv12(f.index, f.rgba).map_err(ExportError::Encode)?
        };
        filmcraft_export::note_hw_encode_frame();
        // the muxer needs the parameter sets after the first group of pictures
        if f.index == 0 && matches!(codec, VtCodec::H264 | VtCodec::Hevc) {
            let params = self.vt.parameter_sets().ok_or_else(|| ExportError::Encode("VideoToolbox returned no parameter sets for the first frame".into()))?;
            if codec == VtCodec::Hevc {
                let cfg = hevc_config(&params).map_err(ExportError::Encode)?;
                let wanted_bits = if c.profile.ten_bit() { 10 } else { 8 };
                let wanted_chroma = if c.profile == VtProfile::HevcMain42210 { 2 } else { 1 };
                if u32::from(cfg.bit_depth_luma) != wanted_bits
                    || cfg.chroma_format_idc != wanted_chroma
                    || filmcraft_codecs::hw::hevc_has_alpha(&cfg) != self.alpha
                {
                    return Err(ExportError::Encode("VideoToolbox changed the requested HEVC bit depth, chroma or alpha".into()));
                }
            }
        }
        self.samples(packets)
    }

    fn flush(&mut self) -> filmcraft_export::Result<Vec<EncodedPacket>> {
        let packets = self.vt.flush().map_err(ExportError::Encode)?;
        self.samples(packets)
    }
}

/// Whether the actual device accepts Main 10 HDR (never inferred from the chip name).
pub fn hevc_hdr_available() -> bool {
    static PROBE: OnceLock<bool> = OnceLock::new();
    *PROBE.get_or_init(|| {
        let settings = ExportSettings { format: Format::Hevc, signal: filmcraft_export::ColorSignal::PQ, ..Default::default() };
        config_for(640, 360, FrameRate::FPS_30, &settings)
            .ok()
            .is_some_and(|c| VtEncoder::with_options(c, VtOptions { signal: settings.signal, ..Default::default() }).is_ok())
    })
}
