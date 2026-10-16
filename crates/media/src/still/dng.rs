//! DNG camera raw stills and CinemaDNG frames (decoded by `filmcraft-dng`).
//!
//! The raw image is developed the way the DNG specification describes (linearisation, black and
//! white levels, the camera's as-shot white balance, demosaic, default crop, colour matrices and
//! baseline exposure) into scene-linear Rec. 709, kept as 32-bit floats: the compositor's working
//! format, so highlights above 1.0 survive into effects and colour work, exactly as with OpenEXR.
//! A white the sensor clipped is 1.0 × 2^BaselineExposure.
//!
//! A numbered folder of `.dng` frames imports as an image sequence (CinemaDNG); the first frame's
//! CinemaDNG FrameRate and TimeCodes tags give the sequence its frame rate and start time code.

use std::sync::Arc;

use filmcraft_color::{ColorInfo, Transfer};
use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_time::FrameRate;

use super::DEFAULT_STILL_DURATION;
use crate::raw::{self, RawInfo, RawSettings};
use crate::{FrameCache, FrameRequest, MediaError, MediaInfo, MediaKind, MediaSource, Result, SharedSource, VideoStreamInfo};

/// The codec name of DNG stills in the media info.
pub const CODEC: &str = "DNG";
/// The codec name of numbered DNG folders (CinemaDNG sequences).
pub const SEQUENCE_CODEC: &str = "DNG sequence";
/// The codec name of CinemaDNG in MXF.
pub const MXF_CODEC: &str = "CinemaDNG";

/// Whether media with this info is camera raw (developed by this module): a DNG still, a DNG
/// sequence or CinemaDNG in MXF.
pub fn is_camera_raw(info: &MediaInfo) -> bool {
    info.video.as_ref().is_some_and(|v| matches!(v.codec.as_str(), CODEC | SEQUENCE_CODEC | MXF_CODEC))
}

/// Whether `b` is a DNG file.
pub fn sniff(b: &[u8]) -> bool {
    filmcraft_dng::sniff(b)
}

/// The colour of developed frames: scene-linear light, Rec. 709 primaries.
pub const LINEAR_709: ColorInfo = ColorInfo { transfer: Transfer::Linear, ..ColorInfo::SRGB_FULL };

/// A developed DNG frame.
pub struct Decoded {
    /// Linear Rec. 709 RGBA floats (opaque), default-crop sized.
    pub frame: VideoFrame,
    /// For the media info, e.g. "Bayer 12-bit raw (lossless JPEG)".
    pub pixel_format: String,
    pub info: filmcraft_dng::Info,
}

fn error(name: &str, e: filmcraft_dng::Error) -> MediaError {
    match e {
        filmcraft_dng::Error::Unsupported(m) => MediaError::Unsupported(format!("{name}: {m}")),
        e => MediaError::Decode(format!("{name}: {e}")),
    }
}

/// The pixel format shown in the media info, e.g. "Bayer 12-bit raw (lossless JPEG)".
pub fn pixel_format(i: &filmcraft_dng::Info) -> String {
    let kind = if i.cfa { "Bayer" } else { "linear" };
    format!("{kind} {}-bit raw ({})", i.bits, i.compression)
}

/// Read a DNG's metadata without developing it: the developed size, pixel format and tags (for
/// CinemaDNG in MXF, whose descriptor may not describe the picture).
pub fn probe(name: &str, bytes: &[u8]) -> Result<(u32, u32, String, filmcraft_dng::Info)> {
    let i = filmcraft_dng::probe(bytes).map_err(|e| error(name, e))?;
    let (Ok(w), Ok(h)) = (u32::try_from(i.width), u32::try_from(i.height)) else { return Err(MediaError::Decode(format!("{name}: picture too large"))) };
    if w == 0 || h == 0 {
        return Err(MediaError::Decode(format!("{name}: empty picture")));
    }
    Ok((w, h, pixel_format(&i), i))
}

/// Develop a DNG file as the file says, at full resolution.
pub fn decode(name: &str, bytes: &[u8]) -> Result<Decoded> {
    decode_with(name, bytes, &RawSettings::default(), 0)
}

/// Develop a DNG file with raw `settings` at reduced-resolution `level` ([`raw::scale_level`]: the
/// picture may be smaller than the file's, never smaller than 1 / 2^level of it).
pub fn decode_with(name: &str, bytes: &[u8], settings: &RawSettings, level: u8) -> Result<Decoded> {
    let dng = filmcraft_dng::Dng::parse(bytes).map_err(|e| error(name, e))?;
    let target = if level == 0 {
        None
    } else {
        let i = dng.info().map_err(|e| error(name, e))?;
        Some((u32::try_from(i.width).unwrap_or(u32::MAX), u32::try_from(i.height).unwrap_or(u32::MAX)))
    };
    let o = match target {
        Some((w, h)) => settings.options(w, h, level),
        None => settings.options(0, 0, 0),
    };
    let img = dng.develop_with(&o).map_err(|e| error(name, e))?;
    let (w, h) = (u32::try_from(img.width), u32::try_from(img.height));
    let (Ok(w), Ok(h)) = (w, h) else { return Err(MediaError::Decode(format!("{name}: picture too large"))) };
    let pixel_format = pixel_format(&img.info);
    let mut frame = VideoFrame::rgba_f32(w, h, img.rgba);
    frame.color = LINEAR_709;
    Ok(Decoded { frame, pixel_format, info: img.info })
}

/// The as-shot white balance of a DNG as (temperature in kelvin, tint), when it has the colour
/// calibration to say.
pub fn as_shot(bytes: &[u8]) -> Option<(f64, f64)> {
    filmcraft_dng::Dng::parse(bytes).ok()?.as_shot_temperature_tint().ok().flatten()
}

/// A DNG still: developed with its raw settings, once per reduced-resolution level asked for.
pub struct DngStill {
    info: MediaInfo,
    bytes: Arc<[u8]>,
    settings: RawSettings,
    as_shot: Option<(f64, f64)>,
    /// Developed pictures by level ([`raw::scale_level`]).
    cache: FrameCache<u8>,
}

impl DngStill {
    /// Develop `bytes` at full resolution with `settings` (an error if it cannot be developed).
    pub fn open(name: &str, bytes: Arc<[u8]>, settings: RawSettings) -> Result<Self> {
        let d = decode_with(name, &bytes, &settings, 0)?;
        let info = MediaInfo {
            name: name.to_string(),
            kind: MediaKind::Still,
            duration: DEFAULT_STILL_DURATION,
            video: Some(VideoStreamInfo {
                width: d.frame.width,
                height: d.frame.height,
                frame_rate: FrameRate::FPS_29_97,
                par: (1, 1),
                codec: CODEC.into(),
                pixel_format: d.pixel_format,
                color: LINEAR_709,
                has_alpha: false,
                bitrate: None,
                hdr: None,
            }),
            audio_streams: Vec::new(),
            container: "Image".into(),
            start_timecode: None,
            file_size: Some(bytes.len() as u64),
        };
        let cache = FrameCache::new(512 << 20);
        cache.insert(0, Arc::new(d.frame));
        Ok(Self { info, as_shot: as_shot(&bytes), bytes, settings, cache })
    }
}

impl MediaSource for DngStill {
    fn info(&self) -> &MediaInfo {
        &self.info
    }
    fn video_frame(&self, req: FrameRequest) -> Result<Arc<VideoFrame>> {
        let level = raw::scale_level(req.scale);
        if let Some(f) = self.cache.get(&level) {
            return Ok(f);
        }
        let f = Arc::new(decode_with(&self.info.name, &self.bytes, &self.settings, level)?.frame);
        self.cache.insert(level, f.clone());
        Ok(f)
    }
    fn audio(&self, _start: i64, _frames: usize, _sample_rate: u32) -> Result<AudioBuffer> {
        Err(MediaError::NoStream("audio"))
    }
    fn raw_info(&self) -> Option<RawInfo> {
        Some(RawInfo { as_shot: self.as_shot, settings: self.settings })
    }
    fn with_raw_settings(&self, settings: &RawSettings) -> Option<SharedSource> {
        // a file that developed once develops again; if not, the caller keeps this source
        DngStill::open(&self.info.name, self.bytes.clone(), *settings).ok().map(|s| Arc::new(s) as SharedSource)
    }
}

/// The frame rate and start time code (in frames at that rate) of a CinemaDNG sequence whose first
/// frame is `info`: the FrameRate tag when present (else `default`), the TimeCodes tag when present.
pub fn sequence_timing(info: &filmcraft_dng::Info, default: FrameRate) -> (FrameRate, Option<i64>) {
    let rate = info.frame_rate.map(|(n, d)| FrameRate::new(n, d).sane()).unwrap_or(default);
    let tc = info.timecode.map(|t| {
        let drop = t.drop_frame && rate.supports_drop_frame();
        filmcraft_time::fields_to_frames(i64::from(t.hours), i64::from(t.minutes), i64::from(t.seconds), i64::from(t.frames), rate, drop)
    });
    (rate, tc)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use filmcraft_dng::tiff::tags;
    use filmcraft_dng::writer::{Compression, Options, Value, write};
    use filmcraft_frame::PixelData;

    use super::*;
    use crate::sequence::{FrameLoader, ImageSequenceSource};
    use crate::{FrameRequest, MediaSource};

    /// A 16 × 12 RGGB DNG of a flat grey at `level` (0–1 of the 12-bit range above black 0), white
    /// balanced by neutral (0.5, 1, 0.5); no colour matrix, so the camera values are taken as Rec. 709.
    fn grey_dng(level: f64, extra: Vec<(u16, Value)>) -> Vec<u8> {
        let (w, h) = (16, 12);
        let neutral = [0.5, 1.0, 0.5];
        let pat = [0usize, 1, 1, 2];
        let px: Vec<u16> = (0..w * h).map(|i| (level * neutral[pat[(i / w % 2) * 2 + i % w % 2]] * 4095.0).round() as u16).collect();
        let mut main = vec![(tags::AS_SHOT_NEUTRAL, Value::rationals(&neutral))];
        main.extend(extra);
        let o = Options {
            width: w,
            height: h,
            bits: 12,
            compression: Compression::LosslessJpeg { predictor: 1, components: 2, restart_lines: 0, pt: 0 },
            raw_tags: vec![(tags::WHITE_LEVEL, Value::Short(vec![4095]))],
            main_tags: main,
            preview: true,
            ..Default::default()
        };
        write(&px, &o)
    }

    fn red_at(f: &VideoFrame, x: usize, y: usize) -> f32 {
        match &f.data {
            PixelData::RgbaF32(p) => p[(y * f.width as usize + x) * 4],
            _ => panic!("float frames"),
        }
    }

    fn red(f: &VideoFrame) -> f32 {
        match &f.data {
            PixelData::RgbaF32(p) => p[(5 * 16 + 5) * 4],
            _ => panic!("float frames"),
        }
    }

    #[test]
    fn dng_still_opens_as_linear_float() {
        let f = grey_dng(0.5, vec![]);
        // a DNG is a TIFF, but the TIFF decoder must not get it
        let src = crate::open_bytes("A001_C001_0001.dng", f.clone().into(), &[]).unwrap();
        let info = src.info();
        assert_eq!(info.kind, MediaKind::Still);
        let v = info.video.as_ref().unwrap();
        assert_eq!((v.width, v.height, v.codec.as_str(), v.has_alpha), (16, 12, "DNG", false));
        assert_eq!(v.pixel_format, "Bayer 12-bit raw (lossless JPEG)");
        assert_eq!(v.color.transfer, Transfer::Linear);
        let frame = src.video_frame(FrameRequest::full(filmcraft_time::Tick(0))).unwrap();
        assert_eq!(frame.color.transfer, Transfer::Linear);
        assert!((red(&frame) - 0.5).abs() < 1e-3, "{}", red(&frame));
        assert!(crate::STILL_EXTENSIONS.contains(&"dng"));
        assert!(crate::is_importable(std::path::Path::new("/cards/A001/frame_000001.DNG")));
    }

    #[test]
    fn broken_dngs_are_errors() {
        let f = grey_dng(0.5, vec![]);
        for n in [8, 64, f.len() / 2, f.len() - 3] {
            assert!(crate::open_bytes("x.dng", f[..n].to_vec().into(), &[]).is_err(), "{n}");
        }
        // lossy DNG is named as unsupported
        let mut lossy = grey_dng(0.5, vec![]);
        let at = lossy.windows(4).position(|w| w == [0x03, 0x01, 0x03, 0x00]).unwrap(); // tag 259 SHORT in the raw IFD
        lossy[at + 8..at + 10].copy_from_slice(&34892u16.to_le_bytes());
        let e = crate::open_bytes("x.dng", lossy.into(), &[]).err().unwrap();
        assert!(matches!(e, MediaError::Unsupported(ref m) if m.contains("lossy JPEG")), "{e}");
    }

    #[test]
    fn cinemadng_sequence_takes_rate_and_time_code_from_the_frames() {
        let tags = || {
            vec![
                (tags::FRAME_RATE, Value::SRational(vec![(24, 1)])),
                // 10:00:00:12
                (tags::TIME_CODES, Value::Byte(vec![0x12, 0x00, 0x00, 0x10, 0, 0, 0, 0])),
            ]
        };
        let files: HashMap<String, Vec<u8>> = (0..3).map(|i| (format!("/c/A_{i:04}.dng"), grey_dng(0.2 + 0.2 * f64::from(i), tags()))).collect();
        let loader: FrameLoader =
            Arc::new(move |p: &str| files.get(p).cloned().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, p.to_string())));
        let frames = (0..3).map(|i| Some(format!("/c/A_{i:04}.dng"))).collect();
        // the caller's (Indeterminate Media Timebase) rate is 25; the frames say 24
        let s = ImageSequenceSource::new("A_0000.dng", frames, FrameRate::FPS_25, loader).unwrap();
        let info = s.info();
        assert_eq!(info.kind, MediaKind::ImageSequence);
        assert_eq!(info.frame_rate(), FrameRate::FPS_24);
        assert_eq!(info.duration, FrameRate::FPS_24.tick_of(3));
        assert_eq!(info.start_timecode, Some(10 * 3600 * 24 + 12));
        let v = info.video.as_ref().unwrap();
        assert_eq!((v.codec.as_str(), v.color.transfer), ("DNG sequence", Transfer::Linear));
        for i in 0..3 {
            let f = s.video_frame(FrameRequest::full(FrameRate::FPS_24.tick_of(i))).unwrap();
            let want = 0.2 + 0.2 * i as f32;
            assert!((red(&f) - want).abs() < 1e-3, "frame {i}: {}", red(&f));
        }
    }

    #[test]
    fn raw_settings_change_the_developed_still() {
        let f: Arc<[u8]> = grey_dng(0.25, vec![]).into();
        let src = crate::open_bytes("A001_C001_0001.dng", f.clone(), &[]).unwrap();
        // no ColorMatrix: no as-shot temperature to offer, default settings in use
        assert_eq!(src.raw_info(), Some(RawInfo { as_shot: None, settings: RawSettings::default() }));
        let base = red(&src.video_frame(FrameRequest::full(filmcraft_time::Tick(0))).unwrap());
        assert!((base - 0.25).abs() < 1e-3, "{base}");
        // +1 stop doubles the scene-linear values, -2 quarters them
        for (stops, factor) in [(1.0, 2.0), (-2.0, 0.25)] {
            let s = src.with_raw_settings(&RawSettings { exposure: stops, ..Default::default() }).unwrap();
            assert_eq!(s.raw_info().unwrap().settings.exposure, stops);
            assert_eq!(s.info(), src.info());
            let v = red(&s.video_frame(FrameRequest::full(filmcraft_time::Tick(0))).unwrap());
            assert!((v - base * factor).abs() < 1e-3, "{stops}: {v}");
        }
        // the original source is unchanged
        assert!((red(&src.video_frame(FrameRequest::full(filmcraft_time::Tick(0))).unwrap()) - base).abs() < 1e-6);
        // the fast demosaic gives the same flat field
        let fast = src.with_raw_settings(&RawSettings { demosaic: raw::DemosaicQuality::Fast, ..Default::default() }).unwrap();
        assert!((red(&fast.video_frame(FrameRequest::full(filmcraft_time::Tick(0))).unwrap()) - base).abs() < 1e-3);
        // other sources are not camera raw
        let png = crate::still::StillSource::from_rgba("x.png", 2, 2, vec![0; 16], false, 16);
        assert!(png.raw_info().is_none());
        assert!(png.with_raw_settings(&RawSettings::default()).is_none());
        assert!(is_camera_raw(src.info()));
        assert!(!is_camera_raw(png.info()));
    }

    #[test]
    fn reduced_resolution_requests_develop_smaller_pictures() {
        let f: Arc<[u8]> = grey_dng(0.5, vec![]).into();
        let src = crate::open_bytes("x.dng", f, &[]).unwrap();
        let full = src.video_frame(FrameRequest::full(filmcraft_time::Tick(0))).unwrap();
        assert_eq!((full.width, full.height), (16, 12));
        // the decoder may develop a smaller picture, never one smaller than the request
        let half = src.video_frame(FrameRequest { time: filmcraft_time::Tick(0), scale: 0.5 }).unwrap();
        assert!((8..16).contains(&half.width) && (6..12).contains(&half.height), "{}x{}", half.width, half.height);
        assert!((red_at(&half, 2, 2) - 0.5).abs() < 1e-3);
        // the media info keeps the full size
        assert_eq!(src.info().video.as_ref().unwrap().width, 16);
    }

    #[test]
    fn raw_settings_reach_every_sequence_frame() {
        let files: HashMap<String, Vec<u8>> = (0..2).map(|i| (format!("/c/B_{i:04}.dng"), grey_dng(0.1 + 0.2 * f64::from(i), vec![]))).collect();
        let loader: FrameLoader =
            Arc::new(move |p: &str| files.get(p).cloned().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, p.to_string())));
        let frames = (0..2).map(|i| Some(format!("/c/B_{i:04}.dng"))).collect();
        let s = ImageSequenceSource::new("B_0000.dng", frames, FrameRate::FPS_25, loader).unwrap();
        assert_eq!(s.raw_info().unwrap().settings, RawSettings::default());
        let bright = s.with_raw_settings(&RawSettings { exposure: 1.0, ..Default::default() }).unwrap();
        assert_eq!(bright.info(), s.info());
        for i in 0..2 {
            let t = FrameRate::FPS_25.tick_of(i);
            let (a, b) = (red(&s.video_frame(FrameRequest::full(t)).unwrap()), red(&bright.video_frame(FrameRequest::full(t)).unwrap()));
            assert!((b - 2.0 * a).abs() < 1e-3, "frame {i}: {a} {b}");
        }
        let quarter = bright.video_frame(FrameRequest { time: FrameRate::FPS_25.tick_of(1), scale: 0.25 }).unwrap();
        assert!((4..16).contains(&quarter.width) && quarter.height >= 3, "{}x{}", quarter.width, quarter.height);
        assert!((red_at(&quarter, 1, 1) - 2.0 * 0.3).abs() < 1e-3);
    }

    #[test]
    fn timing_without_cinemadng_tags_is_the_callers() {
        let info = filmcraft_dng::probe(&grey_dng(0.5, vec![])).unwrap();
        assert_eq!(sequence_timing(&info, FrameRate::FPS_25), (FrameRate::FPS_25, None));
        // drop frame only where the rate has it
        let mut i = info.clone();
        i.frame_rate = Some((30_000, 1001));
        i.timecode = Some(filmcraft_dng::Timecode { hours: 0, minutes: 1, seconds: 0, frames: 2, drop_frame: true });
        assert_eq!(sequence_timing(&i, FrameRate::FPS_25), (FrameRate::FPS_29_97, Some(1800)));
        i.frame_rate = Some((25, 1));
        assert_eq!(sequence_timing(&i, FrameRate::FPS_24), (FrameRate::FPS_25, Some(1502)));
    }
}
