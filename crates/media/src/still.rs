//! Still images (PNG, JPEG, GIF, WebP, TIFF, BMP, Targa, DPX, OpenEXR, Radiance HDR, DNG camera raw) as media
//! sources. Animated PNG and GIF files open as movies instead ([`crate::animated`]); here they would
//! show their first frame.
//!
//! A still is decoded when it is opened (its size, alpha and pixel format are needed then) and
//! again whenever its picture is asked for after the stills cache gave it up. The cache is one
//! [`FrameCache`] for every still in the process, bounded by [`STILL_BUDGET`]: a source holds
//! only the means to read its file again (a reader, or the file's bytes when that is all the host
//! gave us), never the decoded picture, so a project or a Media Browser folder with hundreds of
//! photos keeps a bounded number of them decoded, the way the GOP caches of video sources share a
//! frame budget. A source's entry leaves the cache with the source.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};

use filmcraft_frame::VideoFrame;
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick};

use crate::reader::{SharedReader, read_range};
use crate::{FrameCache, FrameRequest, MediaError, MediaInfo, MediaKind, MediaSource, Result, VideoStreamInfo};

pub mod dng;
pub mod exr;
pub mod hdr;
mod ycbcr_tiff;

/// Default still duration (Premiere's preference default is 5 seconds).
pub const DEFAULT_STILL_DURATION: Tick = Tick(5 * TICKS_PER_SECOND);

/// Whether `b` (the contents of the file `name`) is a still image we decode.
pub fn sniff(name: &str, b: &[u8]) -> bool {
    crate::dpx::sniff(b)
        || hdr::sniff(b)
        || crate::tga::sniff(name, b)
        || b.starts_with(b"\x89PNG")
        || b.starts_with(&[0xFF, 0xD8, 0xFF])
        || b.starts_with(b"GIF8")
        || (b.len() > 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP")
        || b.starts_with(b"II*\0")
        || b.starts_with(b"MM\0*")
        || b.starts_with(b"BM")
}

/// A decoded picture: straight-alpha RGBA8, top row first.
pub struct Raster {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// Whether any pixel is not fully opaque.
    pub has_alpha: bool,
}

/// Decode a still image file (`name` picks Targa, which has no magic number; the content picks
/// every other format).
pub fn decode_raster(name: &str, bytes: &[u8]) -> Result<Raster> {
    if crate::dpx::sniff(bytes) {
        return crate::dpx::decode(name, bytes);
    }
    if crate::tga::sniff(name, bytes) {
        return crate::tga::decode(name, bytes);
    }
    if ycbcr_tiff::sniff(bytes) {
        return ycbcr_tiff::decode(name, bytes);
    }
    let img = image::load_from_memory(bytes).map_err(|e| MediaError::Decode(format!("{name}: {e}")))?;
    let has_alpha = img.color().has_alpha();
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(Raster { width, height, rgba: rgba.into_raw(), has_alpha })
}

/// Decoded pictures of every still in the process may hold together; past it the least recently
/// shown are decoded again when next asked for.
///
/// Same as the video frame budget (`codecs::gop::FRAME_BUDGET`). Float stills (EXR, HDR, DNG) are
/// 16 bytes per pixel, so a two-layer frame of camera raws needs this much: a logo over a 61 MP raw,
/// or a dissolve between two 24 MP raws, play from the cache; two 45 MP raws on one frame do not.
pub const STILL_BUDGET: usize = 1 << 30;

static CACHE: LazyLock<Arc<FrameCache<u64>>> = LazyLock::new(|| Arc::new(FrameCache::new(STILL_BUDGET)));
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Bytes of decoded stills held right now (`perf.stats`).
pub fn cached_bytes() -> usize {
    CACHE.bytes()
}

/// How a still reads its file again once its picture has left the cache.
type Reload = Arc<dyn Fn() -> Result<Arc<[u8]>> + Send + Sync>;

/// A still image's decoded picture and the stream info describing it.
pub(crate) struct Decoded {
    pub info: MediaInfo,
    pub frame: VideoFrame,
}

/// Decode a still of any supported format (the content picks the format; `name` picks Targa).
pub(crate) fn decode_still(name: &str, bytes: &[u8]) -> Result<Decoded> {
    if exr::sniff(bytes) {
        return exr::still(name, bytes);
    }
    if hdr::sniff(bytes) {
        return hdr::still(name, bytes);
    }
    // a DNG is a TIFF too: develop its raw image instead of reading its preview
    if dng::sniff(bytes) {
        return dng::still(name, bytes);
    }
    let r = decode_raster(name, bytes)?;
    Ok(Decoded::from_rgba(name, r.width, r.height, r.rgba, r.has_alpha, bytes.len() as u64))
}

pub struct StillSource {
    info: MediaInfo,
    name: String,
    /// Key in `cache`.
    id: u64,
    reload: Reload,
    cache: Arc<FrameCache<u64>>,
}

impl StillSource {
    /// Open a still from the whole file's bytes, which are kept for decoding it again.
    pub fn from_bytes(name: &str, bytes: Arc<[u8]>) -> Result<Self> {
        let d = decode_still(name, &bytes)?;
        let reload: Reload = Arc::new(move || Ok(bytes.clone()));
        Ok(Self::new(name, d, reload, CACHE.clone()))
    }

    /// Open a still from `bytes`, the whole file as read through `reader`. The reader, not the
    /// bytes, is kept: it is read again (whole) to decode the picture again.
    pub fn open(name: &str, reader: SharedReader, bytes: &[u8]) -> Result<Self> {
        let d = decode_still(name, bytes)?;
        let n = name.to_string();
        let read = move || {
            read_range(&*reader, 0, usize::try_from(reader.len()).unwrap_or(usize::MAX)).map(Arc::from).map_err(|e| MediaError::Io(format!("{n}: {e}")))
        };
        Ok(Self::new(name, d, Arc::new(read), CACHE.clone()))
    }

    fn new(name: &str, d: Decoded, reload: Reload, cache: Arc<FrameCache<u64>>) -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        cache.insert(id, Arc::new(d.frame));
        Self { info: d.info, name: name.to_string(), id, reload, cache }
    }

    /// Whether this source's picture is in the cache right now.
    pub fn is_cached(&self) -> bool {
        self.cache.get(&self.id).is_some()
    }
}

impl Drop for StillSource {
    fn drop(&mut self) {
        self.cache.remove(&self.id);
    }
}

impl Decoded {
    pub(crate) fn from_rgba(name: &str, w: u32, h: u32, rgba: Vec<u8>, has_alpha: bool, file_size: u64) -> Self {
        let codec = name.rsplit('.').next().unwrap_or("image").to_ascii_uppercase();
        let info = MediaInfo {
            name: name.to_string(),
            kind: MediaKind::Still,
            duration: DEFAULT_STILL_DURATION,
            video: Some(VideoStreamInfo {
                width: w,
                height: h,
                frame_rate: FrameRate::FPS_29_97,
                par: (1, 1),
                codec,
                pixel_format: if has_alpha { "RGBA 8-bit".into() } else { "RGB 8-bit".into() },
                color: filmcraft_color::ColorInfo::SRGB_FULL,
                has_alpha,
                bitrate: None,
                hdr: None,
            }),
            audio_streams: Vec::new(),
            container: "Image".into(),
            start_timecode: None,
            file_size: Some(file_size),
        };
        Self { info, frame: VideoFrame::rgba8(w, h, rgba) }
    }
}

impl MediaSource for StillSource {
    fn info(&self) -> &MediaInfo {
        &self.info
    }
    fn video_frame(&self, _req: FrameRequest) -> Result<Arc<VideoFrame>> {
        self.cache.get_or_insert_with(self.id, || {
            let bytes = (self.reload)()?;
            Ok(Arc::new(decode_still(&self.name, &bytes)?.frame))
        })
    }
    fn audio(&self, _start: i64, _frames: usize, _sample_rate: u32) -> Result<filmcraft_frame::AudioBuffer> {
        Err(MediaError::NoStream("audio"))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn png(w: u32, h: u32, v: u8) -> Arc<[u8]> {
        let mut out = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(w, h, image::Rgba([v, v, v, 255])).write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner().into()
    }

    /// A still on `cache` whose file is read through `reads` (counted).
    fn still(name: &str, bytes: Arc<[u8]>, reads: &Arc<AtomicUsize>, cache: &Arc<FrameCache<u64>>) -> StillSource {
        let d = decode_still(name, &bytes).unwrap();
        let reads = reads.clone();
        let reload: Reload = Arc::new(move || {
            reads.fetch_add(1, Ordering::SeqCst);
            Ok(bytes.clone())
        });
        StillSource::new(name, d, reload, cache.clone())
    }

    /// #2464: stills share one bounded cache; a picture past the budget is decoded again from the
    /// file when next shown, and a dropped source frees its picture at once.
    #[test]
    fn stills_share_a_bounded_cache_and_decode_again_after_eviction() {
        let frame = 4 * 4 * 4;
        let cache = Arc::new(FrameCache::new(frame * 2 + frame / 2));
        let reads = Arc::new(AtomicUsize::new(0));
        let a = still("a.png", png(4, 4, 10), &reads, &cache);
        let b = still("b.png", png(4, 4, 20), &reads, &cache);
        let c = still("c.png", png(4, 4, 30), &reads, &cache);
        assert_eq!(reads.load(Ordering::SeqCst), 0, "opening decodes the bytes it was given");
        // the third picture pushed the first (least recently used) out
        assert!(!a.is_cached() && b.is_cached() && c.is_cached());
        assert!(cache.bytes() <= cache.budget());
        // showing it again reads and decodes the file once more, and evicts the next oldest
        let f = a.video_frame(FrameRequest::full(Tick(0))).unwrap();
        assert_eq!((f.width, f.height), (4, 4));
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        assert!(a.is_cached() && !b.is_cached());
        // a cached picture costs no read
        a.video_frame(FrameRequest::full(Tick(0))).unwrap();
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        // a dropped source leaves the cache with its picture
        let before = cache.bytes();
        drop(c);
        assert_eq!(cache.bytes(), before - frame);
        assert_eq!(b.info().video.as_ref().map(|v| (v.width, v.height)), Some((4, 4)));
    }

    /// A picture larger than the whole budget still plays from the cache: it is kept on its own
    /// until the next still pushes it out, not decoded again for every frame.
    #[test]
    fn a_still_larger_than_the_budget_stays_cached_until_the_next_one() {
        let frame = 4 * 4 * 4;
        let cache = Arc::new(FrameCache::new(frame / 2));
        let reads = Arc::new(AtomicUsize::new(0));
        let a = still("a.png", png(4, 4, 10), &reads, &cache);
        assert!(a.is_cached() && cache.bytes() == frame);
        a.video_frame(FrameRequest::full(Tick(0))).unwrap();
        a.video_frame(FrameRequest::full(Tick(0))).unwrap();
        assert_eq!(reads.load(Ordering::SeqCst), 0, "shown twice from the cache");
        let b = still("b.png", png(4, 4, 20), &reads, &cache);
        assert!(!a.is_cached() && b.is_cached());
        a.video_frame(FrameRequest::full(Tick(0))).unwrap();
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        assert!(a.is_cached() && !b.is_cached());
    }

    /// The budget holds a two-layer frame of camera raws: a dissolve between two 24 MP float stills
    /// (384 MB each) and a logo over a 61 MP one (9504 x 6336, 963 MB) both stay cached, where
    /// 512 MiB thrashed.
    #[test]
    fn budget_holds_a_two_layer_frame_of_camera_raws() {
        let f32_px = 16;
        let target = STILL_BUDGET / 10 * 9; // what `insert` evicts down to once over budget
        assert!(2 * 6000 * 4000 * f32_px <= target);
        assert!(9504 * 6336 * f32_px + 400 * 200 * 4 <= STILL_BUDGET);
    }

    /// `from_bytes` puts the picture in the process-wide cache and drop takes it out again. Other
    /// tests use that cache at the same time, so only this source's own entry is looked at.
    #[test]
    fn from_bytes_uses_the_shared_cache() {
        let s = StillSource::from_bytes("g.png", png(2, 3, 1)).unwrap();
        let id = s.id;
        assert!(Arc::ptr_eq(&s.cache, &CACHE));
        assert_eq!(CACHE.get(&id).map(|f| f.byte_size()), Some(2 * 3 * 4));
        drop(s);
        assert!(CACHE.get(&id).is_none());
    }
}
