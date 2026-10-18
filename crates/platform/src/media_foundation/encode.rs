//! Media Foundation hardware H.264 / HEVC encoding (Windows): [`factory`] is registered in front
//! of the software H.264 encoder (`filmcraft_export::register_encoder`), behind NVENC, so an NVIDIA
//! machine keeps NVENC and every other machine with a hardware encoder (Intel, AMD, Qualcomm) gets
//! one it did not have.
//!
//! - H.264: the factory takes an export only when Export ▸ Hardware encoding is Auto and the
//!   machine's encoder can do what the settings ask; otherwise it returns `None` and the software
//!   encoder runs, exactly as before.
//! - H.265 (HEVC Main 8-bit 4:2:0 SDR): there is no software encoder, so the Hardware encoding
//!   toggle does not apply and choosing the format is the opt-in. What no encoder here can do is
//!   counted as declined and ends the export with an error that says why; when the machine has no
//!   HEVC encoder at all, [`hevc_available`] says so and the factory returns `None`.
//!
//! A hardware encoder that fails in the middle of an export ends the export with an error naming
//! it: like NVENC's, it cannot be replayed into the software encoder, because the two write
//! different streams.
//!
//! # What the machine's encoder has to be like
//!
//! Every vendor's encoder MFT is an **asynchronous** MFT, and an asynchronous MFT answers
//! `MF_E_TRANSFORM_ASYNC_LOCKED` to every call until the caller sets `MF_TRANSFORM_ASYNC_UNLOCK`;
//! they also do not declare their media types up front, so `MFTEnumEx` filtered on the input
//! subtype finds none of them and the candidates come from an unfiltered enumeration. Both are
//! handled in [`MfEncoder::new`]; see `docs/adr/0001-platform-ffi.md`.

use std::sync::{Once, OnceLock};

use filmcraft_codecs::hw::NalCodec;
use filmcraft_export::{
    BitrateMode, EncodedPacket, EncoderFrame, ExportError, ExportSettings, FieldOrder, Format, H264Pass, H264Profile, HardwareEncoding, Result, VideoEncoder,
};
use filmcraft_isobmff::{AvcConfig, SampleEntry};
use filmcraft_time::FrameRate;

use super::mft::{EncSpec, Input, MfApi, MfEncoder};
use crate::annexb;

/// Media Foundation timestamps are 100 ns units.
const TIME_100NS: i64 = 10_000_000;
/// How long the drain at the end of an export waits for the encoder's last pictures. Generous:
/// this is the only place where nothing else will ask the MFT again.
const DRAIN_WAIT: std::time::Duration = std::time::Duration::from_secs(10);
/// Mid-stream, the encoder is not waited for. An asynchronous MFT is about a picture period
/// behind, so asking for output immediately after each picture always costs a wait; the picture is
/// not lost, because the MFT's input queue fills and `try_input` then blocks on its output until it
/// is taken. Waiting only where nothing else would pull keeps an export at render speed instead of
/// encoder latency.
const FRAME_WAIT: std::time::Duration = std::time::Duration::ZERO;
/// How long a full input queue waits for the encoder to catch up before the export is called off.
/// Generous: this is the encoder setting the pace, and a real encode never gets near it.
const INPUT_WAIT: std::time::Duration = std::time::Duration::from_secs(30);
/// One wait on a full queue: the encoder's own quiet period, so the loop above sees its output.
const IDLE_WAIT: std::time::Duration = std::time::Duration::from_millis(30);

/// What the encoder is asked for, or why it is not used for that export.
struct Config {
    size: (u32, u32),
    rate: (u32, u32),
    bitrate_bps: u32,
    /// Constant bit rate (Export ▸ Bitrate Encoding ▸ CBR); otherwise VBR.
    cbr: bool,
    codec: NalCodec,
    /// The output media subtype (`MFVideoFormat_H264` / `MFVideoFormat_HEVC`).
    subtype: windows::core::GUID,
    /// `MF_MT_MPEG2_PROFILE` to ask for, where the codec has a defined mapping.
    profile: Option<u32>,
}

/// Why Media Foundation does not take this export (the software encoder does, for H.264), or the
/// encoder's configuration.
fn config(format: Format, w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> std::result::Result<Config, String> {
    let hevc = format == Format::Hevc;
    if !(format == Format::H264 || hevc) || s.format.is_mxf() {
        return Err("only MP4 / MOV H.264 and H.265 exports".into());
    }
    // the NV12 pictures this path encodes are 8-bit 4:2:0: HDR would need P010 and Main 10
    if s.signal.is_hdr() {
        return Err("HDR (this path encodes 8-bit 4:2:0)".into());
    }
    if !matches!(s.h264_pass, H264Pass::Single) || s.bitrate_mode == BitrateMode::Vbr2Pass {
        return Err("two-pass encoding".into());
    }
    if s.bitrate_mode == BitrateMode::Crf {
        return Err("CRF (constant quality)".into());
    }
    if hevc && s.pixel_aspect.is_some_and(|(n, d)| n != d) {
        return Err("non-square pixels".into());
    }
    if s.field_order != FieldOrder::Progressive {
        return Err("interlaced output".into());
    }
    if w == 0 || h == 0 || !w.is_multiple_of(2) || !h.is_multiple_of(2) {
        return Err("the encoder needs nonzero even dimensions".into());
    }
    if w > u32::from(u16::MAX) || h > u32::from(u16::MAX) {
        return Err(format!("{w}x{h} does not fit an MP4 sample entry"));
    }
    let (Ok(num), Ok(den)) = (u32::try_from(rate.num), u32::try_from(rate.den)) else {
        return Err("frame rate".into());
    };
    if num == 0 || den == 0 {
        return Err("frame rate".into());
    }
    use windows::Win32::Media::MediaFoundation::{
        MFVideoFormat_H264, MFVideoFormat_HEVC, eAVEncH264VProfile_Base, eAVEncH264VProfile_High, eAVEncH264VProfile_Main,
    };
    Ok(Config {
        size: (w, h),
        rate: (num, den),
        // bits per second; encoders refuse a type without a bit rate
        bitrate_bps: s.bitrate_kbps.max(100).saturating_mul(1000),
        cbr: s.bitrate_mode == BitrateMode::Cbr,
        codec: if hevc { NalCodec::Hevc } else { NalCodec::H264 },
        subtype: if hevc { MFVideoFormat_HEVC } else { MFVideoFormat_H264 },
        // H.264 only. HEVC's `MF_MT_MPEG2_PROFILE` values are `eAVEncH265VProfile_*`, and every
        // encoder this path has met already writes Main without being asked, so there is nothing to
        // correct there and nothing to check it against.
        // The eAVEncH264VProfile values are the H.264 `profile_idc` (66 / 77 / 100) and
        // `MF_MT_MPEG2_PROFILE` is a UINT32, so they cross over as positive constants.
        profile: if hevc {
            None
        } else {
            Some(match s.h264_profile {
                H264Profile::Baseline => eAVEncH264VProfile_Base.0 as u32,
                H264Profile::Main => eAVEncH264VProfile_Main.0 as u32,
                H264Profile::High => eAVEncH264VProfile_High.0 as u32,
            })
        },
    })
}

/// The Export encoder factory (see the module documentation).
pub fn factory(format: Format, w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>> {
    // H.264 is opt-in per export: its output depends on the machine, while the built-in encoder's
    // is byte-identical everywhere. H.265 has no software encoder, so choosing the format is the
    // opt-in and the Hardware encoding toggle does not apply (as with NVENC and VideoToolbox).
    if format == Format::H264 && s.hardware_encoding != HardwareEncoding::Auto {
        return None;
    }
    let hard = format == Format::Hevc;
    let cfg = match config(format, w, h, rate, s) {
        Ok(c) => c,
        Err(why) => return declined(&why, hard),
    };
    let api = match super::mft::api() {
        Ok(a) => a,
        Err(why) => return declined(&why, hard),
    };
    let spec = EncSpec {
        subtype: cfg.subtype,
        profile: cfg.profile,
        label: if hard { "H.265" } else { "H.264" },
        size: cfg.size,
        rate: cfg.rate,
        bitrate: cfg.bitrate_bps,
        cbr: cfg.cbr,
    };
    let mft = match super::open(api, &spec) {
        Ok(m) => m,
        Err(why) => {
            // no HEVC encoder here at all: not a hardware attempt, the export's own "encoder not
            // available" error
            return if hard && !hevc_available() {
                log::info!("no hardware H.265 encoder: {why}");
                None
            } else {
                declined(&why, hard)
            };
        }
    };
    log::info!("hardware {} encoding on {}", if hard { "H.265" } else { "H.264" }, mft.name());
    filmcraft_export::note_hw_encode_session();
    Some(Ok(Box::new(MfVideoEncoder { mft, api, cfg, rate, planes: Planes::default(), entry: None, written: 0, timeline: Timeline::default() })))
}

/// What the encoder's picture times say about the stream it is producing.
///
/// A hardware encoder hands its pictures back in decode order, each stamped with the presentation
/// time of the picture it came from. Many stamp nothing useful (time zero, or no check possible),
/// but some run a pipeline a picture or two deep and stamp everything late: Qualcomm's AV1 encoder
/// by exactly one picture, as does any B-frame encoder.
///
/// A **constant** offset is ordinary. The pictures are in order and complete, and the file wants an
/// edit list that starts the video that far in — which is what
/// [`VideoEncoder::media_start`](filmcraft_export::VideoEncoder::media_start) is for. A **changing**
/// offset is not something this path can describe, so it is an error naming the encoder rather than a
/// file with wrong timestamps.
#[derive(Clone, Copy, Debug, Default)]
struct Timeline {
    /// How many pictures late every one of them is. `None` until the first picture with a time.
    delay: Option<u64>,
}

/// The most pictures an encoder may run behind and still be a stream rather than a reorder. A DPB
/// is at most 16 frames, so more than that is not an encoder we can describe.
const MAX_DELAY_PICTURES: u64 = 16;

impl Timeline {
    /// Check the picture numbered `written` that came back stamped `time` (100 ns units).
    fn accept(&mut self, rate: FrameRate, written: u64, time: i64) -> std::result::Result<(), ExportError> {
        // Picture 0 is stamped 0 whether the encoder is telling us the time or not, so its stamp
        // says nothing; from the second picture on, 0 is not a time any of ours can have.
        if time == 0 && written == 0 {
            return Ok(());
        }
        // The delay is measured in pictures, not in 100 ns units: a picture's length is
        // `TIME_100NS / rate` nanoseconds, which does not divide evenly, and converting the
        // difference back would round a one-picture delay away to nothing.
        let matches = |k: u64| time_100ns(rate, written.saturating_add(k)) == time;
        match self.delay {
            None => {
                let found = (0..=MAX_DELAY_PICTURES).find(|k| matches(*k));
                self.delay = Some(found.ok_or_else(|| reorder(&rate, written, time))?);
            }
            Some(k) if !matches(k) => return Err(reorder(&rate, written, time)),
            Some(_) => {}
        }
        Ok(())
    }

    /// How far into the file the first picture starts, in the encoder's timescale.
    fn media_start(&self, rate: FrameRate) -> Option<i64> {
        let delay = self.delay.filter(|d| *d != 0)?;
        i64::try_from(delay.saturating_mul(u64::try_from(rate.den.max(1)).ok()?)).ok().filter(|v| *v != 0)
    }
}

/// The error for a picture that is not the one due, or the one after the encoder's delay.
fn reorder(rate: &FrameRate, written: u64, time: i64) -> ExportError {
    ExportError::Encode(format!(
        "Media Foundation: the encoder returned the picture after number {written} at {time} ns, which is not its turn at {} fps (it reorders frames)",
        rate.num as f64 / rate.den as f64
    ))
}

/// The Media Foundation presentation time of picture `index` at `rate`, in 100 ns units. A frame
/// rate that does not fit, or a picture count that would not, saturate: they are declined long
/// before here, so this only ever guards against a value that slipped through.
fn time_100ns(rate: FrameRate, index: u64) -> i64 {
    let (Ok(num), Ok(den)) = (u32::try_from(rate.num), u32::try_from(rate.den)) else {
        return i64::MAX;
    };
    let (num, den) = (i128::from(num.max(1)), i128::from(den.max(1)));
    i64::try_from(i128::from(index) * den * i128::from(TIME_100NS) / num).unwrap_or(i64::MAX)
}

/// What a decline means: H.265 has no software encoder to fall back to, so it is an error; H.264
/// falls through to FilmCraft's own encoder, exactly as when there was no hardware encoder at all.
fn declined(why: &str, hard: bool) -> Option<Result<Box<dyn VideoEncoder>>> {
    if hard {
        log::info!("hardware H.265 encoding declined: {why}");
        filmcraft_export::note_hw_encode_declined();
        Some(Err(ExportError::Unsupported(format!("H.265 export with Media Foundation: {why}"))))
    } else {
        log::info!("hardware encoding declined: {why}");
        filmcraft_export::note_hw_encode_declined();
        None
    }
}

/// The scratch planes one picture is converted through, kept between calls.
#[derive(Default)]
struct Planes {
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    nv12: Vec<u8>,
}

/// The Media Foundation encoder as an Export encoder: RGBA pictures in, MP4 samples out.
struct MfVideoEncoder {
    mft: MfEncoder,
    api: &'static MfApi,
    cfg: Config,
    rate: FrameRate,
    planes: Planes,
    /// The sample entry, which may only be complete once the first access unit has come out.
    entry: Option<SampleEntry>,
    /// Access units written so far, which is what their times are checked against.
    written: u64,
    /// What the encoder's picture times say (see [`Timeline`]).
    timeline: Timeline,
}

impl MfVideoEncoder {
    /// One finished access unit as an MP4 sample, with the parameter sets taken out.
    fn packet(&mut self, annex_b: &[u8], time: i64) -> Result<EncodedPacket> {
        let codec = self.cfg.codec;
        // Check the picture's time before anything else: a wrong clock means a wrong file, and the
        // rest of the work would be wasted.
        self.timeline.accept(self.rate, self.written, time)?;
        let key = annexb::is_key(annex_b, codec);
        let data = annexb::to_length_prefixed(codec, annex_b).map_err(|e| ExportError::Encode(format!("Media Foundation: {e}")))?;
        if self.entry.is_none() {
            self.entry = Some(self.sample_entry_from(annex_b)?);
        }
        self.written += 1;
        filmcraft_export::note_hw_encode_frame();
        Ok(EncodedPacket { data, key, duration: u32::try_from(self.rate.den).unwrap_or(u32::MAX), composition_offset: 0 })
    }

    /// The Media Foundation presentation time (100 ns units) of picture `index` of this export.
    fn time_100ns(&self, index: u64) -> i64 {
        time_100ns(self.rate, index)
    }

    /// The MP4 / MOV sample entry from the parameter sets the encoder wrote in front of the first
    /// picture.
    fn sample_entry_from(&self, annex_b: &[u8]) -> Result<SampleEntry> {
        let (w, h) = (u16::try_from(self.cfg.size.0).unwrap_or(u16::MAX), u16::try_from(self.cfg.size.1).unwrap_or(u16::MAX));
        let entry = match self.cfg.codec {
            NalCodec::H264 => {
                let (sps, pps) = annexb::split_h264_parameter_sets(annex_b).map_err(|e| ExportError::Encode(format!("Media Foundation: {e}")))?;
                SampleEntry::avc(AvcConfig::new(vec![sps], vec![pps], 4), w, h)
            }
            NalCodec::Hevc => {
                let (vps, sps, pps) = annexb::split_hevc_parameter_sets(annex_b).map_err(|e| ExportError::Encode(format!("Media Foundation: {e}")))?;
                let record =
                    crate::nvenc::hevc::hevc_config(&vps, &sps, &pps, self.cfg.size, 8).map_err(|e| ExportError::Encode(format!("Media Foundation: {e}")))?;
                SampleEntry::hevc(record, w, h)
            }
        };
        Ok(entry)
    }

    /// One RGBA picture converted into `self.planes.nv12`, returning how many bytes it holds.
    fn nv12(&mut self, f: &EncoderFrame) -> Result<usize> {
        if f.hdr.is_some() {
            return Err(ExportError::Unsupported("Media Foundation encoding does not take HDR pictures".into()));
        }
        if (f.width, f.height) != self.cfg.size {
            return Err(ExportError::Encode(format!(
                "Media Foundation: a {}x{} picture for a {}x{} encoder",
                f.width, f.height, self.cfg.size.0, self.cfg.size.1
            )));
        }
        let (w, h) = (self.cfg.size.0 as usize, self.cfg.size.1 as usize);
        if f.rgba.len() < w.saturating_mul(h).saturating_mul(4) {
            return Err(ExportError::Encode("the RGBA picture is smaller than its size".into()));
        }
        filmcraft_export::timed(filmcraft_export::Stage::Convert, || {
            filmcraft_export::rgba_to_yuv420_8(f.rgba, w, h, &mut self.planes.y, &mut self.planes.u, &mut self.planes.v)
        });
        let p = &mut self.planes;
        let luma = w * h;
        let needed = luma + luma / 2;
        if p.nv12.len() < needed {
            p.nv12.resize(needed, 0);
        }
        p.nv12[..luma].copy_from_slice(&p.y[..luma]);
        // 4:2:0 chroma, interleaved and subsampled: the U and V of two pixels share two bytes
        for row in 0..h / 2 {
            let (src, dst) = (row * (w / 2), luma + row * w);
            for col in 0..w / 2 {
                p.nv12[dst + col * 2] = p.u[src + col];
                p.nv12[dst + col * 2 + 1] = p.v[src + col];
            }
        }
        Ok(needed)
    }

    /// Everything the MFT has finished, as MP4 samples, waiting up to `budget` for an asynchronous one.
    fn take_ready(&mut self, budget: std::time::Duration) -> Result<Vec<EncodedPacket>> {
        let frames = self.mft.wait(self.api, budget).map_err(|e| ExportError::Encode(format!("Media Foundation: {e}")))?;
        frames.iter().map(|(bytes, time)| self.packet(bytes, *time)).collect()
    }
}

impl VideoEncoder for MfVideoEncoder {
    fn sample_entry(&self) -> SampleEntry {
        // Only complete once the encoder has written its parameter sets; the export asks again
        // after the first picture, which is when they exist.
        self.entry.clone().unwrap_or_else(|| SampleEntry::avc(AvcConfig::default(), self.cfg.size.0 as u16, self.cfg.size.1 as u16))
    }

    fn timescale(&self) -> u32 {
        u32::try_from(self.rate.num).unwrap_or(u32::MAX)
    }

    fn encode(&mut self, f: &EncoderFrame) -> Result<Vec<EncodedPacket>> {
        let needed = self.nv12(f)?;
        let time = self.time_100ns(f.index);
        let mut ready = Vec::new();
        // Feed the picture, taking the encoder's finished pictures whenever its input queue is
        // full. Waiting there is the encoder setting the pace, not us being slow: an asynchronous
        // MFT has no other way to say "not yet".
        let deadline = std::time::Instant::now() + INPUT_WAIT;
        loop {
            match self.mft.try_input(self.api, &self.planes.nv12[..needed], time) {
                Ok(Input::Taken) => break,
                Ok(Input::Full) if std::time::Instant::now() < deadline => ready.extend(self.take_ready(IDLE_WAIT)?),
                Ok(Input::Full) => return Err(ExportError::Encode("Media Foundation: the encoder stopped accepting pictures".into())),
                Err(e) => return Err(ExportError::Encode(format!("Media Foundation: {e}"))),
            }
        }
        ready.extend(self.take_ready(FRAME_WAIT)?);
        Ok(ready)
    }

    fn media_start(&self) -> Option<i64> {
        self.timeline.media_start(self.rate)
    }

    fn flush(&mut self) -> Result<Vec<EncodedPacket>> {
        self.mft.drain().map_err(|e| ExportError::Encode(format!("Media Foundation: {e}")))?;
        // everything the encoder still holds, which is a pipeline delay's worth of pictures
        self.take_ready(DRAIN_WAIT)
    }
}

static HEVC_PROBE: OnceLock<bool> = OnceLock::new();
static H264_PROBE: OnceLock<bool> = OnceLock::new();

/// Whether this machine has a hardware H.264 encoder, by opening a small session (asked once).
/// A machine has an H.264 encoder or an HEVC one or both; a caller counting which factory takes
/// an export needs to know whether anything will before the software encoder does.
pub fn h264_available() -> bool {
    *H264_PROBE.get_or_init(|| {
        let Ok(api) = super::mft::api() else { return false };
        let spec = EncSpec {
            subtype: windows::Win32::Media::MediaFoundation::MFVideoFormat_H264,
            profile: Some(windows::Win32::Media::MediaFoundation::eAVEncH264VProfile_High.0 as u32),
            label: "H.264",
            size: (640, 360),
            rate: (30, 1),
            bitrate: 2_000_000,
            cbr: false,
        };
        match super::open(api, &spec) {
            Ok(_) => true,
            Err(why) => {
                log::info!("no Media Foundation hardware H.264 encoder: {why}");
                false
            }
        }
    })
}

/// Whether this machine has a hardware HEVC encoder, which is what makes `Format::Hevc` available
/// (asked once, by creating a small session). Registered alongside NVENC's probe, so a machine with
/// either encoder offers the format.
pub fn hevc_available() -> bool {
    *HEVC_PROBE.get_or_init(|| {
        let Ok(api) = super::mft::api() else { return false };
        use windows::Win32::Media::MediaFoundation::MFVideoFormat_HEVC;
        let spec = EncSpec { subtype: MFVideoFormat_HEVC, profile: None, label: "H.265", size: (640, 360), rate: (30, 1), bitrate: 2_000_000, cbr: false };
        match super::open(api, &spec) {
            Ok(_) => true,
            Err(why) => {
                log::info!("no Media Foundation hardware HEVC encoder: {why}");
                false
            }
        }
    })
}

/// Whether [`hevc_available`] has its answer yet (diagnostics and tests).
pub fn hevc_probed() -> bool {
    HEVC_PROBE.get().is_some()
}

/// Ask [`hevc_available`] now, on a thread of its own, so the first draw of the export format list
/// does not spend the time of opening a hardware session on the UI thread. Whoever asks while the
/// thread runs waits for the same answer.
pub fn warm_hevc_probe() {
    static STARTED: Once = Once::new();
    STARTED.call_once(|| {
        let spawned = std::thread::Builder::new().name("mf-hevc-probe".into()).spawn(|| {
            let _ = std::panic::catch_unwind(hevc_available);
        });
        if let Err(why) = spawned {
            log::info!("Media Foundation HEVC probe not started ahead of time: {why}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_export::{BitrateMode, ColorSignal, FieldOrder, Format, H264Pass, HardwareEncoding};

    fn settings(format: Format) -> ExportSettings {
        ExportSettings { format, hardware_encoding: HardwareEncoding::Auto, bitrate_kbps: 4000, ..Default::default() }
    }

    /// The export's bitrate mode reaches the encoder as its rate control mode (#936).
    #[test]
    fn rate_control_follows_the_export_settings() {
        assert!(!config(Format::H264, 1280, 720, FrameRate::FPS_24, &settings(Format::H264)).unwrap().cbr);
        let cbr = ExportSettings { bitrate_mode: BitrateMode::Cbr, ..settings(Format::H264) };
        assert!(config(Format::H264, 1280, 720, FrameRate::FPS_24, &cbr).unwrap().cbr);
    }

    #[test]
    fn h264_is_opt_in_and_other_formats_are_not_ours() {
        // Hardware encoding off: the built-in encoder runs, and the MFT is never opened
        let off = ExportSettings { hardware_encoding: HardwareEncoding::Off, ..settings(Format::H264) };
        assert!(factory(Format::H264, 640, 360, FrameRate::FPS_24, &off).is_none());
        for format in [Format::ProRes, Format::MxfOp1a, Format::Mjpeg, Format::DnxHr] {
            assert!(config(format, 640, 360, FrameRate::FPS_24, &settings(format)).is_err(), "{format:?}");
        }
    }

    #[test]
    fn what_this_path_takes_and_what_it_declines() {
        let s = settings(Format::H264);
        let c = config(Format::H264, 1280, 720, FrameRate::FPS_24, &s).unwrap();
        assert_eq!((c.size, c.rate, c.bitrate_bps, c.codec), ((1280, 720), (24, 1), 4_000_000, NalCodec::H264));
        // the export's H.264 profile reaches the encoder as `MF_MT_MPEG2_PROFILE` (profile_idc)
        assert_eq!(c.profile, Some(100), "high by default");
        for (p, idc) in [(H264Profile::Baseline, 66), (H264Profile::Main, 77), (H264Profile::High, 100)] {
            let s = ExportSettings { h264_profile: p, ..s.clone() };
            assert_eq!(config(Format::H264, 1280, 720, FrameRate::FPS_24, &s).unwrap().profile, Some(idc), "{p:?}");
        }
        assert_eq!(config(Format::Hevc, 1280, 720, FrameRate::FPS_24, &ExportSettings { format: Format::Hevc, ..s.clone() }).unwrap().codec, NalCodec::Hevc);

        for (what, s) in [
            ("CRF", ExportSettings { bitrate_mode: BitrateMode::Crf, ..s.clone() }),
            ("two-pass", ExportSettings { bitrate_mode: BitrateMode::Vbr2Pass, ..s.clone() }),
            ("analysis pass", ExportSettings { h264_pass: H264Pass::First, ..s.clone() }),
            ("interlaced", ExportSettings { field_order: FieldOrder::UpperFirst, ..s.clone() }),
            ("HDR", ExportSettings { signal: ColorSignal::PQ, ..s.clone() }),
            ("MXF", ExportSettings { format: Format::MxfOp1a, ..s.clone() }),
        ] {
            assert!(config(Format::H264, 1280, 720, FrameRate::FPS_24, &s).is_err(), "{what}");
        }
        // non-square pixels: the H.264 path writes the aspect ratio, H.265 does not
        let par = ExportSettings { pixel_aspect: Some((4, 3)), ..s.clone() };
        assert!(config(Format::H264, 1280, 720, FrameRate::FPS_24, &par).is_ok());
        assert!(config(Format::Hevc, 1280, 720, FrameRate::FPS_24, &par).is_err());

        // hostile sizes and frame rates
        assert!(config(Format::H264, 641, 360, FrameRate::FPS_24, &s).is_err());
        assert!(config(Format::H264, 0, 360, FrameRate::FPS_24, &s).is_err());
        assert!(config(Format::H264, 70_000, 64, FrameRate::FPS_24, &s).is_err());
        assert!(config(Format::H264, 64, 70_000, FrameRate::FPS_24, &s).is_err());
        for rate in [FrameRate { num: 0, den: 1 }, FrameRate { num: 24, den: 0 }, FrameRate { num: -24, den: 1 }, FrameRate { num: i64::MAX, den: 1 }] {
            assert!(config(Format::H264, 1280, 720, rate, &s).is_err(), "{rate:?}");
        }
        // the bit rate an encoder is given never wraps to zero
        let huge = ExportSettings { bitrate_kbps: u32::MAX, ..s };
        assert_eq!(config(Format::H264, 640, 360, FrameRate::FPS_24, &huge).unwrap().bitrate_bps, u32::MAX);
    }

    #[test]
    fn a_constant_encoder_delay_is_an_edit_list_not_a_failure() {
        // Most encoders report the picture's own time: nothing to correct.
        let mut t = Timeline::default();
        for n in 0..5u64 {
            t.accept(FrameRate::FPS_24, n, time_100ns(FrameRate::FPS_24, n)).unwrap();
        }
        assert_eq!(t.media_start(FrameRate::FPS_24), None);

        // An encoder a picture deep (Qualcomm's AV1 one does exactly this) is fine too: the same
        // offset on every picture, and the edit list carries it.
        let mut t = Timeline::default();
        for n in 0..5u64 {
            t.accept(FrameRate::FPS_24, n, time_100ns(FrameRate::FPS_24, n + 1)).unwrap();
        }
        assert_eq!(t.media_start(FrameRate::FPS_24), Some(1), "one picture late at 24 fps is one tick of the timescale");

        // An encoder that reports no times at all cannot be checked on its first picture, which is
        // stamped 0 either way; from the second on, a 0 is not a time any of ours can have.
        let mut t = Timeline::default();
        t.accept(FrameRate::FPS_24, 0, 0).unwrap();
        assert_eq!(t.media_start(FrameRate::FPS_24), None);
        assert!(t.accept(FrameRate::FPS_24, 1, 0).is_err(), "only picture 0 can legitimately be stamped 0");

        // A picture at a different offset than the rest is reordering, and is an error that names it.
        let mut t = Timeline::default();
        t.accept(FrameRate::FPS_24, 0, time_100ns(FrameRate::FPS_24, 1)).unwrap();
        let e = t.accept(FrameRate::FPS_24, 1, time_100ns(FrameRate::FPS_24, 5)).unwrap_err().to_string();
        assert!(e.contains("reorders frames"), "{e}");
        // a picture that arrives *early* is reordering too
        let mut t = Timeline::default();
        t.accept(FrameRate::FPS_24, 0, time_100ns(FrameRate::FPS_24, 2)).unwrap();
        assert!(t.accept(FrameRate::FPS_24, 1, time_100ns(FrameRate::FPS_24, 2)).is_err(), "a picture that arrives early is reordering too");
    }

    #[test]
    fn picture_times_are_100_nanosecond_units() {
        // 24 fps: one picture every 10_000_000 / 24, and exactly one second after 24 of them
        assert_eq!(time_100ns(FrameRate::FPS_24, 0), 0);
        assert_eq!(time_100ns(FrameRate::FPS_24, 24), TIME_100NS);
        assert_eq!(time_100ns(FrameRate::FPS_24, 25), TIME_100NS * 25 / 24);
        // 29.97 fps and 60 fps
        assert!((time_100ns(FrameRate { num: 30000, den: 1001 }, 1) - 333_666).abs() <= 2);
        assert_eq!(time_100ns(FrameRate { num: 60, den: 1 }, 1), 166_666);
        assert_eq!(time_100ns(FrameRate { num: 1, den: 1 }, 3), 30_000_000);
        // a frame rate that does not fit, or a picture count that would not, saturates
        assert_eq!(time_100ns(FrameRate { num: i64::MAX, den: 1 }, 1), i64::MAX);
        assert_eq!(time_100ns(FrameRate { num: 24, den: i64::MAX }, u64::MAX), i64::MAX);
    }
}
