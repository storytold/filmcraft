//! Reading a render back: Premiere's output from `plan/premiere/fidelity/<case>/`, and ours from
//! `<out>/ours/<case>/`, through the same decoders, so both sides are treated alike.
//!
//! A case directory holds the picture as numbered stills (PNG, TIFF, BMP, JPEG, Targa, DPX,
//! OpenEXR) or as one movie (QuickTime / MP4 / MXF with a codec we decode, e.g. ProRes), and the
//! sound as a WAV / AIFF beside them (or inside the movie). Anything else in the directory is
//! ignored. Frames come back as 8-bit sRGB over black.

use std::path::{Path, PathBuf};

use filmcraft_media::{FrameRequest, MediaSource, SharedSource};
use filmcraft_time::{TICKS_PER_SECOND, Tick};
use serde::{Deserialize, Serialize};

use crate::Error;
use crate::score::Frame;

/// Most frames read from one render (a hostile or runaway directory stops here).
pub const MAX_FRAMES: usize = 20_000;
/// Most audio samples per channel read from one render (one hour at 48 kHz).
pub const MAX_SAMPLES: usize = 48_000 * 3600;

const STILL_EXT: &[&str] = &["png", "tif", "tiff", "bmp", "jpg", "jpeg", "tga", "dpx", "exr"];
const MOVIE_EXT: &[&str] = &["mov", "mp4", "m4v", "mxf", "mkv", "avi"];
const AUDIO_EXT: &[&str] = &["wav", "wave", "bwf", "aif", "aiff"];

/// The picture files of a render.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoFiles {
    None,
    /// Numbered stills in frame order.
    Stills(Vec<PathBuf>),
    Movie(PathBuf),
}

impl VideoFiles {
    pub fn is_none(&self) -> bool {
        matches!(self, VideoFiles::None)
    }

    /// A one-line description for reports.
    pub fn describe(&self) -> String {
        match self {
            VideoFiles::None => "no picture".into(),
            VideoFiles::Stills(v) => format!("{} stills", v.len()),
            VideoFiles::Movie(p) => p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
        }
    }
}

/// What a render directory holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Found {
    pub dir: PathBuf,
    pub video: VideoFiles,
    /// A separate sound file, when there is one (else the movie's own sound, if any).
    pub audio: Option<PathBuf>,
}

fn ext_of(p: &Path) -> String {
    p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default()
}

/// The trailing number of a file stem (`frame0007.png` → 7), for frame order.
fn trailing_number(p: &Path) -> Option<u64> {
    let stem = p.file_stem()?.to_string_lossy().to_string();
    let digits: String = stem.chars().rev().take_while(|c| c.is_ascii_digit()).collect::<Vec<_>>().into_iter().rev().collect();
    digits.parse().ok()
}

/// Look at `dir`: `Ok(None)` when it does not exist or holds no render.
pub fn locate(dir: &Path) -> Result<Option<Found>, Error> {
    if !dir.is_dir() {
        return Ok(None);
    }
    let mut stills = Vec::new();
    let mut movies = Vec::new();
    let mut audio = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| Error::Io(dir.to_path_buf(), e))? {
        let p = entry.map_err(|e| Error::Io(dir.to_path_buf(), e))?.path();
        if !p.is_file() || p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')) {
            continue;
        }
        let ext = ext_of(&p);
        if STILL_EXT.contains(&ext.as_str()) {
            stills.push(p);
        } else if MOVIE_EXT.contains(&ext.as_str()) {
            movies.push(p);
        } else if AUDIO_EXT.contains(&ext.as_str()) {
            audio.push(p);
        }
    }
    if stills.len() > MAX_FRAMES {
        return Err(Error::Reference(dir.to_path_buf(), format!("more than {MAX_FRAMES} stills")));
    }
    // frame order: by trailing number, then by name
    stills.sort_by(|a, b| trailing_number(a).cmp(&trailing_number(b)).then_with(|| a.cmp(b)));
    movies.sort();
    audio.sort();
    let video = if !stills.is_empty() {
        VideoFiles::Stills(stills)
    } else if let Some(m) = movies.into_iter().next() {
        VideoFiles::Movie(m)
    } else {
        VideoFiles::None
    };
    let audio = audio.into_iter().next();
    if video.is_none() && audio.is_none() {
        return Ok(None);
    }
    Ok(Some(Found { dir: dir.to_path_buf(), video, audio }))
}

fn open(path: &Path) -> Result<SharedSource, Error> {
    open_media(path).map_err(|e| Error::Reference(path.to_path_buf(), e))
}

/// Open a media file through every container / codec opener the app has (the same as
/// `filmcraft_media::open_path`, which native-only code paths gate off the web build).
pub fn open_media(path: &Path) -> Result<SharedSource, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    filmcraft_media::open_bytes(&name, bytes.into(), &filmcraft_codecs::openers()).map_err(|e| e.to_string())
}

/// Straight-alpha RGBA8 flattened over black (opaque frames are unchanged).
pub fn over_black(w: u32, h: u32, mut rgba: Vec<u8>) -> Result<Frame, Error> {
    for px in rgba.as_chunks_mut::<4>().0 {
        let a = u16::from(px[3]);
        if a != 255 {
            for c in &mut px[..3] {
                *c = ((u16::from(*c) * a + 127) / 255).min(255) as u8;
            }
            px[3] = 255;
        }
    }
    Frame::new(w, h, rgba).map_err(Error::Score)
}

fn frame_of(src: &dyn MediaSource, t: Tick, path: &Path) -> Result<Frame, Error> {
    let f = src.video_frame(FrameRequest::full(t)).map_err(|e| Error::Reference(path.to_path_buf(), e.to_string()))?;
    over_black(f.width, f.height, f.to_rgba8())
}

/// Every frame of the render's picture, at most `max_frames`.
pub fn read_video(found: &Found, max_frames: usize) -> Result<Vec<Frame>, Error> {
    let max_frames = max_frames.min(MAX_FRAMES);
    match &found.video {
        VideoFiles::None => Ok(Vec::new()),
        VideoFiles::Stills(paths) => paths.iter().take(max_frames).map(|p| frame_of(open(p)?.as_ref(), Tick::ZERO, p)).collect(),
        VideoFiles::Movie(path) => {
            let src = open(path)?;
            let info = src.info();
            let Some(v) = &info.video else {
                return Err(Error::Reference(path.clone(), "movie has no picture".into()));
            };
            let fd = v.frame_rate.frame_duration().0.max(1);
            let n = usize::try_from(info.duration.0.max(0).saturating_add(fd / 2) / fd).unwrap_or(0).min(max_frames);
            (0..n).map(|i| frame_of(src.as_ref(), v.frame_rate.tick_of(i as i64), path)).collect()
        }
    }
}

/// The render's sound at `sample_rate`, planar, at most `max_samples` per channel: the separate
/// sound file when there is one, else the movie's own sound; empty when it has neither.
pub fn read_audio(found: &Found, sample_rate: u32, max_samples: usize) -> Result<Vec<Vec<f32>>, Error> {
    let path = match (&found.audio, &found.video) {
        (Some(a), _) => a.clone(),
        (None, VideoFiles::Movie(m)) => m.clone(),
        (None, _) => return Ok(Vec::new()),
    };
    let src = open(&path)?;
    let info = src.info();
    if !info.has_audio() {
        return Ok(Vec::new());
    }
    let sr = i64::from(sample_rate.max(1));
    let total = usize::try_from(info.duration.0.max(0).saturating_mul(sr) / TICKS_PER_SECOND).unwrap_or(0).min(max_samples.min(MAX_SAMPLES));
    let mut out: Vec<Vec<f32>> = Vec::new();
    let mut read = 0usize;
    while read < total {
        let want = (total - read).min(sample_rate.max(1) as usize);
        let buf = src.audio(read as i64, want, sample_rate).map_err(|e| Error::Reference(path.clone(), e.to_string()))?;
        let got = buf.frames();
        if got == 0 {
            break;
        }
        if out.is_empty() {
            out = vec![Vec::with_capacity(total); buf.channels.len()];
        }
        for (o, ch) in out.iter_mut().zip(buf.channels.iter()) {
            o.extend(ch.iter().take(got));
        }
        read += got;
        if got < want {
            break;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stills_sort_by_their_trailing_number() {
        let dir = std::env::temp_dir().join(format!("fidelity-locate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for n in ["shot10.png", "shot2.png", "shot1.png", ".hidden.png", "notes.txt", "mix.wav", "extra.aiff"] {
            std::fs::write(dir.join(n), b"x").unwrap();
        }
        let f = locate(&dir).unwrap().unwrap();
        let names: Vec<String> = match &f.video {
            VideoFiles::Stills(v) => v.iter().map(|p| p.file_name().unwrap().to_string_lossy().to_string()).collect(),
            other => panic!("{other:?}"),
        };
        assert_eq!(names, ["shot1.png", "shot2.png", "shot10.png"]);
        assert_eq!(f.audio.unwrap().file_name().unwrap(), "extra.aiff", "first by name");
        assert_eq!(f.video.describe(), "3 stills");
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(locate(&dir).unwrap(), None, "missing directory is no reference");
    }

    #[test]
    fn a_directory_without_media_is_no_reference() {
        let dir = std::env::temp_dir().join(format!("fidelity-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("README.md"), b"nothing here").unwrap();
        assert_eq!(locate(&dir).unwrap(), None);
        std::fs::write(dir.join("render.mov"), b"not really").unwrap();
        let f = locate(&dir).unwrap().unwrap();
        assert!(matches!(f.video, VideoFiles::Movie(_)));
        // a corrupt movie is an error, not a panic
        assert!(read_video(&f, 10).is_err());
        assert!(read_audio(&f, 48_000, 10).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn alpha_is_flattened_over_black() {
        let f = over_black(2, 1, vec![200, 100, 50, 255, 200, 100, 50, 0]).unwrap();
        assert_eq!(f.rgba, vec![200, 100, 50, 255, 0, 0, 0, 255]);
        let half = over_black(1, 1, vec![200, 100, 50, 128]).unwrap();
        assert_eq!(&half.rgba[..3], &[100, 50, 25]);
        assert!(over_black(2, 2, vec![0; 3]).is_err());
    }
}
