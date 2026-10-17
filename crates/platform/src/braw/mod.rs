//! Blackmagic RAW (`.braw`) through Blackmagic's own decoder (Linux, Windows and macOS).
//!
//! Blackmagic RAW has no public specification: the only decoder is Blackmagic's, in its free
//! Blackmagic RAW SDK. FilmCraft does not include it. When its runtime library
//! (`libBlackmagicRawAPI.so`, `BlackmagicRawAPI.dll` or Blackmagic's `BlackmagicRawAPI.framework`,
//! from Blackmagic RAW 6.0, or DaVinci Resolve 21's copy on Linux) is installed where FilmCraft
//! looks for it ([`RUNTIME_ENV`], next to FilmCraft,
//! where Blackmagic's installers put it, or the system's library search), [`open_path`]
//! opens `.braw` files through it as movies: frames decoded and
//! processed by Blackmagic's decoder with the clip's own settings (as recorded, or as its sidecar
//! file sets them) to 8-bit RGB, the clip's PCM audio, frame rate and start time code. Without the
//! runtime a `.braw` file fails to open with a message saying what to install. A runtime with other
//! interfaces (an older Blackmagic RAW) is refused by their ids rather than called at the wrong slots.
//!
//! Decoding is on the CPU, at the smallest of Blackmagic RAW's resolution scales (full, ½, ¼, ⅛)
//! that covers the frame request, so previews and thumbnails decode a fraction of the sensor.
//!
//! The FFI is in `ffi` (declarations) and `sdk` (calls); the rest of this module is safe code.

#[cfg(test)]
mod abi_tests;
mod ffi;
#[allow(unsafe_code)]
mod sdk;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use filmcraft_color::{ColorInfo, Matrix, Primaries, Range, Transfer};
use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_media::{AudioStreamInfo, FrameRequest, MediaError, MediaInfo, MediaKind, MediaSource, Result, SharedSource, VideoStreamInfo};
use filmcraft_time::FrameRate;

/// Environment variable naming the folder that holds `libBlackmagicRawAPI.so`, searched first.
pub const RUNTIME_ENV: &str = "FILMCRAFT_BRAW_RUNTIME";

/// Where Blackmagic RAW for Linux installs the runtime (its SDK and its Player), then DaVinci
/// Resolve's copies.
#[cfg(target_os = "linux")]
fn install_dirs() -> Vec<PathBuf> {
    [
        "/usr/lib64/blackmagic/BlackmagicRAWSDK/Linux/Libraries",
        "/usr/lib64/blackmagic/BlackmagicRAWPlayer/BlackmagicRawAPI",
        "/opt/resolve/libs",
        "/opt/resolve/BlackmagicRAWPlayer/BlackmagicRawAPI",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect()
}

/// Where Blackmagic RAW's Windows installer puts the runtime (its MSI's directory table): the SDK's
/// libraries (the ARM64 build in a subfolder), then the Player's and Speed Test's copies, under the
/// Program Files folders.
#[cfg(target_os = "windows")]
fn install_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for var in ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"] {
        let Some(base) = std::env::var_os(var).filter(|b| !b.is_empty()) else { continue };
        let root = PathBuf::from(base).join("Blackmagic Design").join("Blackmagic RAW");
        let libraries = root.join("Blackmagic RAW SDK").join("Win").join("Libraries");
        let sdk = if cfg!(target_arch = "aarch64") { libraries.join("ARM64") } else { libraries };
        for dir in [sdk, root.join("Blackmagic RAW Player").join("BlackmagicRawAPI"), root.join("Blackmagic RAW Speed Test").join("BlackmagicRawAPI")] {
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
    }
    dirs
}

/// Where Blackmagic RAW's macOS installer puts the runtime: the framework inside its applications
/// (the Player and Speed Test), Adobe's plug-ins, the SDK's own folder, and DaVinci Resolve's app.
#[cfg(target_os = "macos")]
fn install_dirs() -> Vec<PathBuf> {
    [
        "/Library/Frameworks",
        "/Applications/Blackmagic RAW/Blackmagic RAW SDK/Mac/Libraries",
        "/Applications/Blackmagic RAW/Blackmagic RAW Player.app/Contents/Frameworks",
        "/Applications/Blackmagic RAW/Blackmagic RAW Speed Test.app/Contents/Frameworks",
        "/Library/Application Support/Adobe/Common/Plug-ins/7.0/MediaCore/BlackmagicRawPremiereImporter.bundle/Contents/Frameworks",
        "/Applications/DaVinci Resolve/DaVinci Resolve.app/Contents/Frameworks",
        "/Applications/DaVinci Resolve/DaVinci Resolve.app/Contents/Libraries",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect()
}

/// Largest picture side a clip may report.
const MAX_SIDE: u32 = 32_768;
/// Most audio samples (all channels) read for one request.
const MAX_AUDIO_SAMPLES: usize = 1 << 24;
/// Most sample frames asked of the runtime at a time.
const AUDIO_CHUNK: u64 = 1 << 18;

/// Frames come out Rec.709-encoded full-range RGB: FilmCraft shows them as Rec.709 whatever gamma
/// the clip is set to (a log gamma looks flat, as in other editors before grading).
const OUTPUT_COLOR: ColorInfo = ColorInfo { matrix: Matrix::Bt709, transfer: Transfer::Bt709, primaries: Primaries::Bt709, range: Range::Full };

/// Where the runtime is looked for, in order: [`RUNTIME_ENV`], a `BlackmagicRawAPI` folder next to
/// FilmCraft's executable (on macOS then the `Frameworks` folder beside it) and the executable's own
/// folder (the SDK's convention for applications that ship it), then where Blackmagic's
/// installers (and on Linux DaVinci Resolve; on macOS also DaVinci Resolve's app) put it; after
/// them the system's library search (not on macOS, where the framework is only found by path).
fn runtime_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = std::env::var_os(RUNTIME_ENV).filter(|d| !d.is_empty()) {
        dirs.push(PathBuf::from(dir));
    }
    if let Some(exe_dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        dirs.push(exe_dir.join("BlackmagicRawAPI"));
        // A macOS .app keeps private frameworks in Contents/Frameworks, as the SDK's own loader
        // looks for them.
        #[cfg(target_os = "macos")]
        dirs.push(exe_dir.join("..").join("Frameworks"));
        dirs.push(exe_dir);
    }
    dirs.extend(install_dirs());
    dirs
}

/// The [`filmcraft_media::PathOpener`] for `.braw` files: every other file is declined.
pub fn open_path(path: &str) -> Option<Result<SharedSource>> {
    let path = Path::new(path);
    if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("braw")) {
        return None;
    }
    Some(BrawSource::open(path).map(|s| Arc::new(s) as SharedSource))
}

/// The resolution scale (a `blackmagicRawResolutionScale*` value) whose frames cover a request at
/// `scale` of full size.
fn resolution_scale(scale: f32) -> u32 {
    if scale <= 0.125 {
        ffi::blackmagicRawResolutionScaleEighth
    } else if scale <= 0.25 {
        ffi::blackmagicRawResolutionScaleQuarter
    } else if scale <= 0.5 {
        ffi::blackmagicRawResolutionScaleHalf
    } else {
        ffi::blackmagicRawResolutionScaleFull
    }
}

/// An open `.braw` file.
pub struct BrawSource {
    info: MediaInfo,
    clip: sdk::Clip,
    rate: FrameRate,
    frames: u64,
    audio: Option<sdk::AudioInfo>,
}

impl BrawSource {
    /// Open `path` through Blackmagic's runtime.
    pub fn open(path: &Path) -> Result<Self> {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
        let meta = std::fs::metadata(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound { MediaError::Offline(format!("{name}: {e}")) } else { MediaError::Io(format!("{name}: {e}")) }
        })?;
        if !meta.is_file() {
            return Err(MediaError::Offline(format!("{name}: not a file")));
        }
        let sdk = sdk::sdk(runtime_dirs).map_err(|e| {
            MediaError::Unsupported(format!(
                "{name}: Blackmagic RAW is decoded by Blackmagic's free Blackmagic RAW runtime, which FilmCraft does not include, and {e}. \
                 Install Blackmagic RAW {} or later from Blackmagic Design's website (or set {RUNTIME_ENV} to the folder that holds its {}), then restart FilmCraft.",
                sdk::SDK_VERSION,
                sdk::LIBRARY
            ))
        })?;
        let (clip, info) = sdk.open(path).map_err(|e| MediaError::Decode(format!("{name}: {e}")))?;
        Self::new(name, clip, info, meta.len())
    }

    fn new(name: String, clip: sdk::Clip, c: sdk::ClipInfo, file_size: u64) -> Result<Self> {
        let bad = |what: String| MediaError::Decode(format!("{name}: {what}"));
        if c.width == 0 || c.height == 0 || c.width > MAX_SIDE || c.height > MAX_SIDE {
            return Err(bad(format!("unsupported picture size {}x{}", c.width, c.height)));
        }
        if !(c.frame_rate.is_finite() && c.frame_rate > 0.0 && c.frame_rate <= 1000.0) {
            return Err(bad(format!("unsupported frame rate {}", c.frame_rate)));
        }
        let frame_count = i64::try_from(c.frame_count).ok().filter(|&n| n > 0).ok_or_else(|| bad("the clip has no frames".into()))?;
        let rate = FrameRate::from_f64(f64::from(c.frame_rate));
        let duration = rate.tick_of(frame_count);
        let start_timecode = c.start_timecode.as_deref().and_then(|tc| filmcraft_time::parse_timecode(tc, rate, tc.contains(';'), 0).ok());
        let seconds = frame_count as f64 / rate.as_f64();
        let bitrate = (seconds > 0.0).then(|| (file_size as f64 * 8.0 / seconds) as u64);
        let look: Vec<&str> = [c.gamma.as_deref(), c.gamut.as_deref()].into_iter().flatten().collect();
        let pixel_format = if look.is_empty() { "RGB 8-bit".to_string() } else { format!("RGB 8-bit ({})", look.join(", ")) };
        let audio_streams = c
            .audio
            .map(|a| AudioStreamInfo { sample_rate: a.sample_rate, channels: a.channels, codec: "PCM".into(), bits_per_sample: Some(a.bits) })
            .into_iter()
            .collect();
        let info = MediaInfo {
            name,
            kind: MediaKind::Movie,
            duration,
            video: Some(VideoStreamInfo {
                width: c.width,
                height: c.height,
                frame_rate: rate,
                par: (1, 1),
                codec: "Blackmagic RAW".into(),
                pixel_format,
                color: OUTPUT_COLOR,
                has_alpha: false,
                bitrate,
                hdr: None,
            }),
            audio_streams,
            container: "Blackmagic RAW".into(),
            start_timecode,
            file_size: Some(file_size),
        };
        Ok(Self { info, clip, rate, frames: c.frame_count, audio: c.audio })
    }

    fn picture(&self, index: u64, scale: u32) -> std::result::Result<sdk::Picture, String> {
        match self.clip.read_frame(index, scale) {
            // a clip that refuses a reduced scale still decodes at full size
            Err(_) if scale != ffi::blackmagicRawResolutionScaleFull => self.clip.read_frame(index, ffi::blackmagicRawResolutionScaleFull),
            r => r,
        }
    }
}

impl MediaSource for BrawSource {
    fn info(&self) -> &MediaInfo {
        &self.info
    }

    fn video_frame(&self, req: FrameRequest) -> Result<Arc<VideoFrame>> {
        let index = u64::try_from(self.rate.frame_at(req.time)).unwrap_or(0).min(self.frames.saturating_sub(1));
        let picture = self.picture(index, resolution_scale(req.scale)).map_err(|e| MediaError::Decode(format!("{}: frame {index}: {e}", self.info.name)))?;
        let mut frame = VideoFrame::rgba8(picture.width, picture.height, picture.rgba).with_pts(self.rate.tick_of(index as i64));
        frame.color = OUTPUT_COLOR;
        Ok(Arc::new(frame))
    }

    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> Result<AudioBuffer> {
        let Some(a) = self.audio else { return Err(MediaError::NoStream("audio")) };
        let channels = a.channels as usize;
        let mut out = AudioBuffer::silence(sample_rate, channels, frames);
        if frames == 0 || sample_rate == 0 || channels == 0 {
            return Ok(out);
        }
        // Linear-interpolating resample when rates differ, as WAV sources do.
        let ratio = f64::from(a.sample_rate) / f64::from(sample_rate);
        let src_of = |i: usize| (start as f64 + i as f64) * ratio;
        let (lo, hi) = (src_of(0), src_of(frames - 1));
        if hi < 0.0 || lo >= a.sample_count as f64 {
            return Ok(out);
        }
        let first = lo.max(0.0).floor() as u64;
        let last = (hi.floor() as u64).saturating_add(1).min(a.sample_count - 1);
        let mut pcm: Vec<f32> = Vec::new();
        let mut pos = first;
        while pos <= last && pcm.len() < MAX_AUDIO_SAMPLES {
            let want = (last - pos + 1).min(AUDIO_CHUNK) as u32;
            let chunk = self.clip.read_audio(pos, want, &a).map_err(|e| MediaError::Decode(format!("{}: audio: {e}", self.info.name)))?;
            let got = chunk.len() / channels;
            if got == 0 {
                break;
            }
            pcm.extend_from_slice(chunk.get(..got * channels).unwrap_or_default());
            pos += got as u64;
        }
        let read = pcm.len() / channels;
        if read == 0 {
            return Ok(out);
        }
        for i in 0..frames {
            let s = src_of(i);
            if s < first as f64 {
                continue;
            }
            let i0 = s.floor() as u64;
            let Ok(k0) = usize::try_from(i0 - first) else { break };
            if k0 >= read {
                break;
            }
            let k1 = (k0 + 1).min(read - 1);
            let fr = (s - i0 as f64) as f32;
            for (c, channel) in out.channels.iter_mut().enumerate() {
                let (Some(&x0), Some(&x1)) = (pcm.get(k0 * channels + c), pcm.get(k1 * channels + c)) else { continue };
                if let Some(o) = channel.get_mut(i) {
                    *o = x0 + (x1 - x0) * fr;
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_decode_at_the_smallest_scale_that_covers_them() {
        assert_eq!(resolution_scale(1.0), ffi::blackmagicRawResolutionScaleFull);
        assert_eq!(resolution_scale(0.51), ffi::blackmagicRawResolutionScaleFull);
        assert_eq!(resolution_scale(0.5), ffi::blackmagicRawResolutionScaleHalf);
        assert_eq!(resolution_scale(0.3), ffi::blackmagicRawResolutionScaleHalf);
        assert_eq!(resolution_scale(0.25), ffi::blackmagicRawResolutionScaleQuarter);
        assert_eq!(resolution_scale(0.1), ffi::blackmagicRawResolutionScaleEighth);
        assert_eq!(resolution_scale(f32::NAN), ffi::blackmagicRawResolutionScaleFull, "a damaged request decodes in full");
    }

    #[test]
    fn only_braw_files_are_taken() {
        assert!(open_path("/clips/a.mov").is_none());
        assert!(open_path("/clips/braw").is_none());
        let missing = std::env::temp_dir().join(format!("filmcraft-braw-missing-{}.BRAW", std::process::id()));
        let r = open_path(&missing.to_string_lossy()).expect("a .braw file (any case) is taken");
        assert!(matches!(r, Err(MediaError::Offline(_))), "a missing clip is offline, not unsupported");
    }
}
