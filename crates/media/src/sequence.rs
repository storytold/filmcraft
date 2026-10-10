//! Image sequences: numbered stills (`shot_0001.png`, `shot_0002.png`, …) as one movie clip.
//!
//! - [`Numbered`] splits a file name around its last run of digits; [`sequence_frames`] lists the
//!   frames from the chosen file's number up to the highest number present (gaps are `None`).
//! - [`ImageSequenceSource`] decodes frames on demand through a [`FrameLoader`] (the host's file
//!   access) with the still decoders (PNG, JPEG, TIFF, BMP, WebP, GIF), keeps recently used frames
//!   in a byte-budgeted [`FrameCache`], and shows the nearest earlier frame for a missing or
//!   undecodable one (the nearest later frame before the first).
//! - The frame rate is the caller's (Settings ▸ Media ▸ Indeterminate Media Timebase).

use std::sync::Arc;

use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_time::FrameRate;

use crate::{FrameCache, FrameRequest, MediaError, MediaInfo, MediaKind, MediaSource, Result, VideoStreamInfo};

/// Reads a file's bytes (the host's file access).
pub type FrameLoader = Arc<dyn Fn(&str) -> std::io::Result<Vec<u8>> + Send + Sync>;

/// A file name with a frame number: `<dir><prefix><digits><suffix>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Numbered {
    /// Directory part including the trailing separator ("" when none).
    pub dir: String,
    pub prefix: String,
    pub number: u64,
    /// Width of the digit run as written (zero padding).
    pub digits: usize,
    /// Everything after the digits, including the extension.
    pub suffix: String,
}

impl Numbered {
    /// Split `path` at the last run of digits in its file stem. `None` when the stem has no digits
    /// or the file is not a still image.
    pub fn parse(path: &str) -> Option<Numbered> {
        let cut = path.rfind(['/', '\\']).map_or(0, |i| i + 1);
        let (dir, name) = path.split_at(cut);
        let ext_at = name.rfind('.').filter(|&i| i > 0)?;
        let ext = name[ext_at + 1..].to_ascii_lowercase();
        if !crate::STILL_EXTENSIONS.contains(&ext.as_str()) {
            return None;
        }
        let stem = &name[..ext_at];
        let end = stem.rfind(|c: char| c.is_ascii_digit())? + 1;
        let start = stem[..end].rfind(|c: char| !c.is_ascii_digit()).map_or(0, |i| i + 1);
        let digits = &stem[start..end];
        if digits.len() > 18 {
            return None;
        }
        Some(Numbered {
            dir: dir.to_string(),
            prefix: stem[..start].to_string(),
            number: digits.parse().ok()?,
            digits: digits.len(),
            suffix: name[end..].to_string(),
        })
    }

    /// Whether the numbers are zero-padded to a fixed width.
    pub fn padded(&self) -> bool {
        self.digits > 1 && self.number.to_string().len() < self.digits
    }

    /// The file name (no directory) of frame number `n`.
    pub fn file_name(&self, n: u64) -> String {
        format!("{}{:0w$}{}", self.prefix, n, self.suffix, w = self.digits)
    }

    /// The path of frame number `n`.
    pub fn path(&self, n: u64) -> String {
        format!("{}{}", self.dir, self.file_name(n))
    }

    /// The frame number of a sibling file name of this sequence (same prefix, suffix and padding).
    pub fn number_of(&self, file_name: &str) -> Option<u64> {
        let mid = file_name.strip_prefix(self.prefix.as_str())?.strip_suffix(self.suffix.as_str())?;
        if mid.is_empty() || !mid.bytes().all(|b| b.is_ascii_digit()) || mid.len() > 18 {
            return None;
        }
        // every number has the first one's width, or is wider without leading zeros (overflowing
        // the padding, or an unpadded sequence counting past 9, 99…)
        if mid.len() != self.digits && (mid.len() < self.digits || mid.starts_with('0')) {
            return None;
        }
        mid.parse().ok()
    }
}

/// The frames of the sequence `first` belongs to, from `first.number` to the highest number among
/// `siblings` (file names in its directory): each frame's path, or `None` for a missing number.
pub fn sequence_frames(first: &Numbered, siblings: &[String]) -> Vec<Option<String>> {
    let mut nums: Vec<(u64, &String)> = siblings.iter().filter_map(|n| first.number_of(n).map(|k| (k, n))).filter(|(k, _)| *k >= first.number).collect();
    nums.sort();
    nums.dedup_by_key(|x| x.0);
    let last = nums.last().map_or(first.number, |x| x.0);
    // a sequence longer than this is a numbering accident (e.g. a date in the name)
    let span = (last - first.number).min(1_000_000) as usize + 1;
    let mut frames = vec![None; span];
    for (k, name) in nums {
        let i = (k - first.number) as usize;
        if i < span {
            frames[i] = Some(format!("{}{}", first.dir, name));
        }
    }
    if frames[0].is_none() {
        frames[0] = Some(first.path(first.number));
    }
    frames
}

/// Numbered stills played as a movie.
pub struct ImageSequenceSource {
    info: MediaInfo,
    frames: Vec<Option<String>>,
    loader: FrameLoader,
    cache: FrameCache<usize>,
}

impl ImageSequenceSource {
    /// `frames`: path per frame (`None`: missing). The first present frame is decoded now (size,
    /// alpha); the rest on demand.
    pub fn new(name: &str, frames: Vec<Option<String>>, rate: FrameRate, loader: FrameLoader) -> Result<Self> {
        let first = frames.iter().flatten().next().ok_or_else(|| MediaError::Offline(format!("{name}: no frames")))?.clone();
        let bytes = loader(&first).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound { MediaError::Offline(format!("{first}: {e}")) } else { MediaError::Io(format!("{first}: {e}")) }
        })?;
        let img = image::load_from_memory(&bytes).map_err(|e| MediaError::Decode(format!("{first}: {e}")))?;
        let has_alpha = img.color().has_alpha();
        let (w, h) = (img.width(), img.height());
        let ext = first.rsplit('.').next().unwrap_or("image").to_ascii_uppercase();
        let n = frames.len() as i64;
        let info = MediaInfo {
            name: name.to_string(),
            kind: MediaKind::ImageSequence,
            duration: rate.tick_of(n),
            video: Some(VideoStreamInfo {
                width: w,
                height: h,
                frame_rate: rate,
                par: (1, 1),
                codec: format!("{ext} sequence"),
                pixel_format: if has_alpha { "RGBA 8-bit".into() } else { "RGB 8-bit".into() },
                color: filmcraft_color::ColorInfo::SRGB_FULL,
                has_alpha,
                bitrate: None,
                hdr: None,
            }),
            audio_streams: Vec::new(),
            container: "Image Sequence".into(),
            start_timecode: None,
            file_size: None,
        };
        let cache = FrameCache::new(256 << 20);
        cache.insert(frames.iter().position(Option::is_some).unwrap_or(0), Arc::new(VideoFrame::rgba8(w, h, img.to_rgba8().into_raw())));
        Ok(Self { info, frames, loader, cache })
    }

    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Indices of missing frames.
    pub fn missing_frames(&self) -> Vec<usize> {
        self.frames.iter().enumerate().filter(|(_, f)| f.is_none()).map(|(i, _)| i).collect()
    }

    /// Path of frame `i` (`None` when missing).
    pub fn frame_path(&self, i: usize) -> Option<&str> {
        self.frames.get(i).and_then(|f| f.as_deref())
    }

    fn decode(&self, i: usize) -> Result<Arc<VideoFrame>> {
        if let Some(f) = self.cache.get(&i) {
            return Ok(f);
        }
        let path = self.frames[i].as_deref().ok_or(MediaError::NoStream("video"))?;
        let bytes = (self.loader)(path).map_err(|e| MediaError::Io(format!("{path}: {e}")))?;
        let img = image::load_from_memory(&bytes).map_err(|e| MediaError::Decode(format!("{path}: {e}")))?;
        let f = Arc::new(VideoFrame::rgba8(img.width(), img.height(), img.to_rgba8().into_raw()));
        self.cache.insert(i, f.clone());
        Ok(f)
    }
}

impl MediaSource for ImageSequenceSource {
    fn info(&self) -> &MediaInfo {
        &self.info
    }

    fn video_frame(&self, req: FrameRequest) -> Result<Arc<VideoFrame>> {
        let n = self.frames.len();
        let want = self.info.frame_rate().frame_at(req.time.max(filmcraft_time::Tick::ZERO)).clamp(0, n as i64 - 1) as usize;
        // the wanted frame, else the nearest earlier present frame, else the nearest later one
        let candidates = (0..=want).rev().chain(want + 1..n);
        let mut last_err = None;
        for i in candidates.filter(|&i| self.frames[i].is_some()).take(8) {
            match self.decode(i) {
                Ok(f) => return Ok(f),
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap_or(MediaError::NoStream("video")))
    }

    fn audio(&self, _start: i64, _frames: usize, _sample_rate: u32) -> Result<AudioBuffer> {
        Err(MediaError::NoStream("audio"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[test]
    fn numbered_names() {
        let n = Numbered::parse("/shots/A001_C002_0099.png").unwrap();
        assert_eq!((n.dir.as_str(), n.prefix.as_str(), n.number, n.digits, n.suffix.as_str()), ("/shots/", "A001_C002_", 99, 4, ".png"));
        assert!(n.padded());
        assert_eq!(n.path(100), "/shots/A001_C002_0100.png");
        assert_eq!(n.number_of("A001_C002_0100.png"), Some(100));
        assert_eq!(n.number_of("A001_C002_100.png"), None, "padding differs");
        assert_eq!(n.number_of("A001_C002_12345.png"), Some(12345), "overflowing the padding");
        assert_eq!(n.number_of("A001_C002_0100.jpg"), None);
        assert_eq!(n.number_of("B001_C002_0100.png"), None);
        let u = Numbered::parse("frame7.TIF").unwrap();
        assert_eq!((u.prefix.as_str(), u.number, u.suffix.as_str()), ("frame", 7, ".TIF"));
        assert!(!u.padded());
        assert_eq!(u.file_name(10), "frame10.TIF");
        assert_eq!(u.number_of("frame10.TIF"), Some(10));
        assert_eq!(Numbered::parse("clip.mov"), None, "not a still");
        assert_eq!(Numbered::parse("nodigits.png"), None);
        let w = Numbered::parse("C:\\renders\\out.0010.exr.png").unwrap();
        assert_eq!((w.dir.as_str(), w.prefix.as_str(), w.number), ("C:\\renders\\", "out.", 10));
    }

    #[test]
    fn frames_with_gaps() {
        let n = Numbered::parse("/d/s_003.png").unwrap();
        let names: Vec<String> = ["s_001.png", "s_003.png", "s_004.png", "s_007.png", "other.png", "s_005.jpg"].iter().map(|s| s.to_string()).collect();
        let f = sequence_frames(&n, &names);
        assert_eq!(f.len(), 5);
        assert_eq!(f[0].as_deref(), Some("/d/s_003.png"));
        assert_eq!(f[1].as_deref(), Some("/d/s_004.png"));
        assert_eq!((f[2].as_deref(), f[3].as_deref()), (None, None));
        assert_eq!(f[4].as_deref(), Some("/d/s_007.png"));
    }

    fn png(w: u32, h: u32, v: u8) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([v, 255 - v, 7, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn webp_frames_lossless() {
        let enc = |v: u8| {
            let img = image::RgbaImage::from_fn(6, 4, |x, y| image::Rgba([v, (x * 40) as u8, (y * 60) as u8, 255]));
            let mut out = Vec::new();
            image::codecs::webp::WebPEncoder::new_lossless(&mut out).encode(img.as_raw(), 6, 4, image::ExtendedColorType::Rgba8).unwrap();
            (img.into_raw(), out)
        };
        let (a, wa) = enc(1);
        let (b, wb) = enc(2);
        let files: HashMap<String, Vec<u8>> = [("w1.webp".to_string(), wa), ("w2.webp".to_string(), wb)].into_iter().collect();
        let loader: FrameLoader = Arc::new(move |p: &str| files.get(p).cloned().ok_or_else(|| std::io::Error::other("missing")));
        let s = ImageSequenceSource::new("w1.webp", vec![Some("w1.webp".into()), Some("w2.webp".into())], FrameRate::FPS_24, loader).unwrap();
        for (i, want) in [a, b].iter().enumerate() {
            match &s.video_frame(FrameRequest::full(FrameRate::FPS_24.tick_of(i as i64))).unwrap().data {
                filmcraft_frame::PixelData::Rgba8(p) => assert_eq!(&p[..], &want[..]),
                _ => panic!(),
            }
        }
    }

    #[test]
    fn plays_frames_holds_missing_and_caches() {
        let files: HashMap<String, Vec<u8>> = [(0, 10u8), (1, 20), (3, 40)].iter().map(|(i, v)| (format!("f{i}.png"), png(8, 4, *v))).collect();
        let reads = Arc::new(Mutex::new(0usize));
        let r = reads.clone();
        let loader: FrameLoader = Arc::new(move |p: &str| {
            *r.lock().unwrap() += 1;
            files.get(p).cloned().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, p.to_string()))
        });
        let frames = vec![Some("f0.png".to_string()), Some("f1.png".into()), None, Some("f3.png".into())];
        let s = ImageSequenceSource::new("f0.png", frames, FrameRate::FPS_25, loader).unwrap();
        assert_eq!(s.info().kind, MediaKind::ImageSequence);
        assert_eq!(s.info().duration, FrameRate::FPS_25.tick_of(4));
        assert_eq!(s.missing_frames(), vec![2]);
        let px = |i: i64| match &s.video_frame(FrameRequest::full(FrameRate::FPS_25.tick_of(i))).unwrap().data {
            filmcraft_frame::PixelData::Rgba8(p) => p[0],
            _ => panic!(),
        };
        assert_eq!(px(0), 10);
        assert_eq!(px(1), 20);
        assert_eq!(px(2), 20, "missing frame holds the previous one");
        assert_eq!(px(3), 40);
        assert_eq!(px(99), 40, "past the end: the last frame");
        let n = *reads.lock().unwrap();
        for i in 0..4 {
            px(i);
        }
        assert_eq!(*reads.lock().unwrap(), n, "frames come from the cache");
    }
}
