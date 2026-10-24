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
use filmcraft_frame::VideoFrame;
use filmcraft_time::FrameRate;

use super::{DEFAULT_STILL_DURATION, StillSource};
use crate::{MediaError, MediaInfo, MediaKind, Result, VideoStreamInfo};

/// Whether `b` is a DNG file.
pub fn sniff(b: &[u8]) -> bool {
    filmcraft_dng::sniff(b)
}

/// The colour of developed frames: scene-linear light, Rec. 709 primaries.
const LINEAR_709: ColorInfo = ColorInfo { transfer: Transfer::Linear, ..ColorInfo::SRGB_FULL };

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

/// Develop a DNG file.
pub fn decode(name: &str, bytes: &[u8]) -> Result<Decoded> {
    let img = filmcraft_dng::decode(bytes).map_err(|e| error(name, e))?;
    let (w, h) = (u32::try_from(img.width), u32::try_from(img.height));
    let (Ok(w), Ok(h)) = (w, h) else { return Err(MediaError::Decode(format!("{name}: picture too large"))) };
    let i = &img.info;
    let kind = if i.cfa { "Bayer" } else { "linear" };
    let pixel_format = format!("{kind} {}-bit raw ({})", i.bits, i.compression);
    let mut frame = VideoFrame::rgba_f32(w, h, img.rgba);
    frame.color = LINEAR_709;
    Ok(Decoded { frame, pixel_format, info: img.info })
}

/// Open a DNG file as a still.
pub fn still(name: &str, bytes: &[u8]) -> Result<StillSource> {
    let d = decode(name, bytes)?;
    let info = MediaInfo {
        name: name.to_string(),
        kind: MediaKind::Still,
        duration: DEFAULT_STILL_DURATION,
        video: Some(VideoStreamInfo {
            width: d.frame.width,
            height: d.frame.height,
            frame_rate: FrameRate::FPS_29_97,
            par: (1, 1),
            codec: "DNG".into(),
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
    Ok(StillSource { info, frame: Arc::new(d.frame) })
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
