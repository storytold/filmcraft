//! The VideoToolbox H.264 and HEVC encoders behind `filmcraft_export::VideoEncoder` (macOS; safe code
//! around [`crate::videotoolbox_encode`]).
//!
//! H.264 hardware encoding is opt-in per export (`ExportSettings::hardware_encoding`, `Auto`): its output
//! depends on the machine, while the built-in encoder's is byte-identical everywhere. The factory
//! declines, so the built-in encoder is used, for everything the hardware path does not take: other
//! formats, MXF (Annex B), two-pass VBR, HDR, odd picture sizes, non-square pixels, and any
//! configuration VideoToolbox cannot create a hardware session for. A hardware encoder that fails in
//! the middle of an export reports an error (the export stops); it cannot hand the stream over to
//! another encoder.
//!
//! HEVC (`Format::Hevc`) has no built-in encoder, so choosing the format is the opt-in, the same
//! configurations are declined, and a declined export fails with "encoder not available" instead of
//! falling back; [`hevc_available`] tells the format list whether this machine can do it at all.

use std::sync::{Once, OnceLock};

use filmcraft_export::{BitrateMode, EncodedPacket, EncoderFrame, ExportError, ExportSettings, Format, H264Pass, H264Profile, HardwareEncoding, VideoEncoder};
use filmcraft_isobmff::{AvcConfig, HevcConfig, SampleEntry};
use filmcraft_time::FrameRate;

use crate::videotoolbox_encode::{ParamSets, VtCodec, VtConfig, VtEncoder, VtInput, VtPacket, VtProfile, VtRate};

/// The hardware encoder's configuration for an export, or why it is not used for it.
pub fn config_for(w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Result<VtConfig, String> {
    let hevc = s.format == Format::Hevc;
    if !hevc && s.hardware_encoding != HardwareEncoding::Auto {
        return Err("hardware encoding is off".into());
    }
    if s.format.is_mxf() {
        return Err("MXF carries an Annex B stream".into());
    }
    if s.bitrate_mode == BitrateMode::Vbr2Pass || !matches!(s.h264_pass, H264Pass::Single) {
        return Err("two-pass encoding".into());
    }
    if s.bitrate_mode == BitrateMode::Crf {
        return Err("CRF (constant quality)".into());
    }
    if s.signal.is_hdr() {
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
            (true, _) => VtProfile::HevcMain,
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
            let _ = std::panic::catch_unwind(hevc_available);
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
    let name = match format {
        Format::H264 if s.hardware_encoding == HardwareEncoding::Auto => "H.264",
        Format::Hevc => "HEVC",
        _ => return None,
    };
    let config = match config_for(w, h, rate, s) {
        Ok(c) => c,
        Err(why) => {
            log::info!("hardware {name} encoding declined: {why}");
            filmcraft_export::note_hw_encode_declined();
            return None;
        }
    };
    match HardwareEncoder::open(config) {
        Ok(enc) => {
            filmcraft_export::note_hw_encode_session();
            Some(Ok(Box::new(enc)))
        }
        Err(why) => {
            log::info!("hardware {name} encoding declined: {why}");
            filmcraft_export::note_hw_encode_declined();
            None
        }
    }
}

/// The session for `config`: one that takes the export's RGBA pictures as they are, so that
/// VideoToolbox converts them to 4:2:0 and the encode thread only copies (the CPU conversion ran
/// there before, where it competed with the render of the next frames for the thread pool and took
/// longer than the hardware encode itself); when that cannot be created, the planar session with
/// the CPU conversion. `rgba` stands for [`VtEncoder::with_rgba_input`] so tests can refuse it.
fn open_with(config: VtConfig, rgba: impl FnOnce(VtConfig) -> Result<VtEncoder, String>) -> Result<VtEncoder, String> {
    rgba(config.clone()).or_else(|why| {
        log::info!("VideoToolbox RGBA input declined ({why}): converting to 4:2:0 on the CPU");
        VtEncoder::new(config)
    })
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
    /// The planar path's conversion buffers (empty while VideoToolbox converts the RGBA pictures).
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    /// Compressed frames returned so far.
    decoded: i64,
}

impl HardwareEncoder {
    /// The encoder for `config` (see [`open_with`]), or why there is none.
    pub fn open(config: VtConfig) -> Result<Self, String> {
        Ok(Self::around(open_with(config, VtEncoder::with_rgba_input)?))
    }

    fn around(vt: VtEncoder) -> Self {
        HardwareEncoder { vt, y: Vec::new(), u: Vec::new(), v: Vec::new(), decoded: 0 }
    }

    /// Whether VideoToolbox converts the RGBA pictures itself (else the CPU converts them to 4:2:0 first).
    pub fn takes_rgba(&self) -> bool {
        self.vt.input() == VtInput::Bgra
    }

    /// Replace the RGB session by a planar one for the rest of the export. Only for a session that
    /// has produced nothing yet (the first picture), so the stream starts over from its first frame.
    fn fall_back_to_planar(&mut self, why: &str) -> filmcraft_export::Result<()> {
        log::info!("VideoToolbox did not take an RGBA picture ({why}): converting to 4:2:0 on the CPU");
        self.vt = VtEncoder::new(self.vt.config().clone()).map_err(ExportError::Encode)?;
        Ok(())
    }

    /// Convert on the CPU and submit to the planar session.
    fn encode_planar(&mut self, f: &EncoderFrame) -> filmcraft_export::Result<Vec<VtPacket>> {
        let (w, h) = (f.width as usize, f.height as usize);
        filmcraft_export::timed(filmcraft_export::Stage::Convert, || filmcraft_export::rgba_to_yuv420_8(f.rgba, w, h, &mut self.y, &mut self.u, &mut self.v));
        self.vt.encode(f.index, &self.y, &self.u, &self.v).map_err(ExportError::Encode)
    }

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
            (VtCodec::Hevc, p) => SampleEntry::hevc(p.as_ref().and_then(|p| hevc_config(p).ok()).unwrap_or_default(), w, h),
        }
    }

    fn timescale(&self) -> u32 {
        self.vt.config().timescale
    }

    fn encode(&mut self, f: &EncoderFrame) -> filmcraft_export::Result<Vec<EncodedPacket>> {
        let c = self.vt.config();
        let codec = c.profile.codec();
        if f.hdr.is_some() {
            return Err(ExportError::Unsupported("hardware encoding does not take HDR pictures".into()));
        }
        if (f.width, f.height) != (c.width, c.height) {
            return Err(ExportError::Encode(format!("picture is {}x{}, the encoder was created for {}x{}", f.width, f.height, c.width, c.height)));
        }
        let (w, h) = (f.width as usize, f.height as usize);
        if f.rgba.len() < w.saturating_mul(h).saturating_mul(4) {
            return Err(ExportError::Encode("the RGBA picture is smaller than its size".into()));
        }
        let packets = if self.takes_rgba() {
            match self.vt.encode_rgba(f.index, f.rgba) {
                Ok((packets, copied)) => {
                    // the CPU's share of the conversion: the copy into the input buffer
                    filmcraft_export::note_stage(filmcraft_export::Stage::Convert, copied);
                    packets
                }
                // Whether this VideoToolbox converts RGB itself is only known from the first picture
                // (the session is created without complaint either way). Nothing has come out of it
                // yet, so the export goes on from this frame with the planar path, the CPU converting.
                Err(why) if f.index == 0 => {
                    self.fall_back_to_planar(&why)?;
                    self.encode_planar(f)?
                }
                Err(why) => return Err(ExportError::Encode(why)),
            }
        } else {
            self.encode_planar(f)?
        };
        filmcraft_export::note_hw_encode_frame();
        // the muxer needs the parameter sets after the first group of pictures
        if f.index == 0 {
            let params = self.vt.parameter_sets().ok_or_else(|| ExportError::Encode("VideoToolbox returned no parameter sets for the first frame".into()))?;
            if codec == VtCodec::Hevc {
                hevc_config(&params).map_err(ExportError::Encode)?;
            }
        }
        self.samples(packets)
    }

    fn flush(&mut self) -> filmcraft_export::Result<Vec<EncodedPacket>> {
        let packets = self.vt.flush().map_err(ExportError::Encode)?;
        self.samples(packets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 320;
    const H: u32 = 180;

    fn config() -> VtConfig {
        VtConfig {
            width: W,
            height: H,
            timescale: 30,
            frame_duration: 1,
            keyframe_interval: 30,
            profile: VtProfile::High,
            rate: VtRate::Vbr { target_kbps: 2000, max_kbps: 3000 },
        }
    }

    /// A gradient picture with a bar at `i`.
    fn picture(i: u64) -> Vec<u8> {
        let (w, h) = (W as usize, H as usize);
        let mut px = vec![255u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let o = (y * w + x) * 4;
                px[o] = (x * 255 / w) as u8;
                px[o + 1] = (y * 255 / h) as u8;
                px[o + 2] = if x / 10 == i as usize { 255 } else { 40 };
            }
        }
        px
    }

    /// Three pictures through the `VideoEncoder` adapter: one compressed frame each.
    fn encodes_three(enc: &mut HardwareEncoder) {
        let mut packets = Vec::new();
        for i in 0..3u64 {
            let px = picture(i);
            packets.extend(enc.encode(&EncoderFrame { width: W, height: H, rgba: &px, hdr: None, index: i }).expect("encode"));
        }
        packets.extend(enc.flush().expect("flush"));
        assert_eq!(packets.len(), 3);
        assert!(packets[0].key);
    }

    #[test]
    fn rgba_input_is_tried_first() {
        let mut attempted = false;
        let opened = open_with(config(), |c| {
            attempted = true;
            assert_eq!((c.width, c.height), (W, H));
            VtEncoder::with_rgba_input(c)
        });
        assert!(attempted);
        match opened {
            Ok(vt) => assert_eq!(vt.input(), VtInput::Bgra),
            Err(why) => eprintln!("SKIPPED: no VideoToolbox hardware encoder ({why})"),
        }
    }

    /// Where the RGB session cannot be created, the planar one takes the export (the CPU converts).
    #[test]
    fn refused_rgba_input_falls_back_to_the_planar_session() {
        let opened = open_with(config(), |_| Err("no RGB input (test)".into()));
        let Ok(vt) = opened else {
            eprintln!("SKIPPED: no VideoToolbox hardware encoder");
            return;
        };
        assert_eq!(vt.input(), VtInput::Nv12);
        let mut enc = HardwareEncoder::around(vt);
        assert!(!enc.takes_rgba());
        encodes_three(&mut enc);
    }

    /// Where the RGB session refuses the first picture, the encoder starts over with a planar session
    /// and the export goes on from its first frame.
    #[test]
    fn a_refused_first_picture_restarts_with_the_planar_session() {
        let Ok(vt) = VtEncoder::with_rgba_input(config()) else {
            eprintln!("SKIPPED: no VideoToolbox hardware encoder");
            return;
        };
        let mut enc = HardwareEncoder::around(vt);
        assert!(enc.takes_rgba());
        enc.fall_back_to_planar("test").expect("the planar session opens");
        assert!(!enc.takes_rgba());
        encodes_three(&mut enc);
    }

    /// A picture the RGB session fails after the first frame is an error, not a silent restart (the
    /// stream already has frames the planar session could not continue).
    #[test]
    fn a_later_failure_is_an_error() {
        let Ok(vt) = VtEncoder::with_rgba_input(config()) else {
            eprintln!("SKIPPED: no VideoToolbox hardware encoder");
            return;
        };
        let mut enc = HardwareEncoder::around(vt);
        let px = picture(0);
        enc.encode(&EncoderFrame { width: W, height: H, rgba: &px, hdr: None, index: 0 }).expect("first frame");
        // a short picture is refused before it reaches VideoToolbox; so is one of another size
        assert!(enc.encode(&EncoderFrame { width: W, height: H, rgba: &px[..100], hdr: None, index: 1 }).is_err());
        assert!(enc.encode(&EncoderFrame { width: W + 2, height: H, rgba: &px, hdr: None, index: 1 }).is_err());
        assert!(enc.takes_rgba(), "no restart after the first frame");
    }
}
