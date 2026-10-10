//! The media pool: one shared [`MediaSource`] per project item, created lazily from its
//! [`MediaRef`], plus the registry of container/codec openers.
//!
//! - Sources are cached per item **and per reference** (the file path, the offline flag), so a
//!   relink, Make Offline or their undo picks up the right file on the next frame.
//! - Media that can't be opened (missing, unreadable) or was made offline renders the offline
//!   slate ([`filmcraft_render::offline`]) instead of failing; the reason is kept for the UI.
//! - With proxies enabled ([`MediaPool::set_use_proxies`]) items with an attached proxy are read
//!   from it. Proxy frames are smaller; the source still reports the full-resolution size, and the
//!   compositor derives its pixel scale from the frame it gets, so effects and Motion render the
//!   same picture at lower resolution. Export asks for [`MediaPool::full_res_provider`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{FrameRequest, MediaError, MediaInfo, MediaSource, Opener, SharedSource};
use filmcraft_project::{ItemId, MediaClip, MediaRef, Project};
use filmcraft_render::offline::OfflineReason;

use crate::Services;

/// Why an item renders the offline slate, as last seen by the pool.
#[derive(Clone, Debug, PartialEq)]
pub struct OfflineStatus {
    pub reason: OfflineReason,
    pub path: String,
    pub error: String,
}

pub struct MediaPool {
    /// item → (reference key, source)
    sources: RwLock<HashMap<ItemId, (String, SharedSource)>>,
    proxies: RwLock<HashMap<ItemId, (String, SharedSource)>>,
    offline: RwLock<HashMap<ItemId, OfflineStatus>>,
    use_proxies: AtomicBool,
    /// Openers tried before the built-in ones (MP4/MOV + codecs register here).
    pub openers: RwLock<Vec<Opener>>,
}

impl Default for MediaPool {
    /// A pool with the built-in container/codec openers registered.
    fn default() -> Self {
        Self {
            sources: RwLock::new(HashMap::new()),
            proxies: RwLock::new(HashMap::new()),
            offline: RwLock::new(HashMap::new()),
            use_proxies: AtomicBool::new(false),
            openers: RwLock::new(filmcraft_codecs::openers()),
        }
    }
}

/// Largest file [`MediaPool::probe_file`] reads whole when no streaming reader takes it (WAV,
/// MP3 and other byte-opened formats, or an unsupported container).
pub const PROBE_WHOLE_FILE_MAX: u64 = 64 * 1024 * 1024;

/// Cache key of a media clip's full-resolution reference ("" matches anything: sources inserted
/// without a key). A generator's key holds its parameters, so a Color Matte whose color changes
/// (or comes back with undo) is generated again.
pub fn media_key(m: &MediaClip) -> String {
    match &m.media {
        MediaRef::File { path } if m.offline => format!("offline:{path}"),
        MediaRef::File { path } => format!("file:{path}"),
        MediaRef::Generator(g) => format!("generator:{g:?}"),
    }
}

/// Open the image sequence that `first` (a numbered still) starts: its frames from that number to
/// the highest present one, played at `rate`. Frames are listed with [`Services::list_dir`], or
/// found by probing consecutive numbers (stopping after 100 missing in a row) when the host cannot
/// list directories.
pub fn open_image_sequence(first: &str, services: &dyn Services, rate: filmcraft_time::FrameRate, name: &str) -> Result<SharedSource, MediaError> {
    let frames = image_sequence_frames(first, services)?;
    let loader = match services.file_loader() {
        Some(l) => l,
        None => {
            // no lasting file access: read the (encoded) frames now
            let mut files = HashMap::new();
            for p in frames.iter().flatten() {
                if let Ok(b) = services.read_file(p) {
                    files.insert(p.clone(), Arc::<[u8]>::from(b));
                }
            }
            Arc::new(move |p: &str| files.get(p).map(|b| b.to_vec()).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, p.to_string())))
        }
    };
    Ok(Arc::new(filmcraft_media::sequence::ImageSequenceSource::new(name, frames, rate, loader)?))
}

/// The frames of the image sequence starting at `first` (see [`open_image_sequence`]).
pub fn image_sequence_frames(first: &str, services: &dyn Services) -> Result<Vec<Option<String>>, MediaError> {
    use filmcraft_media::sequence::{Numbered, sequence_frames};
    let n = Numbered::parse(first).ok_or_else(|| MediaError::Unsupported(format!("{first}: not a numbered still image")))?;
    let io = |e: std::io::Error| {
        if e.kind() == std::io::ErrorKind::NotFound { MediaError::Offline(format!("{first}: {e}")) } else { MediaError::Io(format!("{first}: {e}")) }
    };
    services.file_size(first).map_err(io)?;
    match services.list_dir(n.dir.trim_end_matches(['/', '\\'])) {
        Some(list) => Ok(sequence_frames(&n, &list.map_err(io)?)),
        None => {
            let mut names = vec![n.file_name(n.number)];
            let mut misses = 0;
            let mut k = n.number + 1;
            while misses < 100 && names.len() < 1_000_000 {
                if services.file_size(&n.path(k)).is_ok() {
                    names.push(n.file_name(k));
                    misses = 0;
                } else {
                    misses += 1;
                }
                k += 1;
            }
            Ok(sequence_frames(&n, &names))
        }
    }
}

fn file_name(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

impl MediaPool {
    pub fn register_opener(&self, o: Opener) {
        self.openers.write().unwrap_or_else(|e| e.into_inner()).push(o);
    }

    /// Cache a source for an item, valid whatever the item's reference (generators).
    pub fn insert(&self, item: ItemId, src: SharedSource) {
        self.sources.write().unwrap_or_else(|e| e.into_inner()).insert(item, (String::new(), src));
    }

    /// Cache a source for an item under its [`media_key`].
    pub fn insert_keyed(&self, item: ItemId, key: String, src: SharedSource) {
        self.sources.write().unwrap_or_else(|e| e.into_inner()).insert(item, (key, src));
    }

    /// Cache a source opened from `path` for an item.
    pub fn insert_file(&self, item: ItemId, path: &str, src: SharedSource) {
        self.sources.write().unwrap_or_else(|e| e.into_inner()).insert(item, (format!("file:{path}"), src));
        self.offline.write().unwrap_or_else(|e| e.into_inner()).remove(&item);
    }

    pub fn remove(&self, item: ItemId) {
        self.sources.write().unwrap_or_else(|e| e.into_inner()).remove(&item);
        self.proxies.write().unwrap_or_else(|e| e.into_inner()).remove(&item);
        self.offline.write().unwrap_or_else(|e| e.into_inner()).remove(&item);
    }

    /// Forget every opened source (files are re-read on next use).
    pub fn clear(&self) {
        self.sources.write().unwrap_or_else(|e| e.into_inner()).clear();
        self.proxies.write().unwrap_or_else(|e| e.into_inner()).clear();
        self.offline.write().unwrap_or_else(|e| e.into_inner()).clear();
    }

    pub fn cached(&self, item: ItemId) -> Option<SharedSource> {
        self.sources.read().unwrap_or_else(|e| e.into_inner()).get(&item).map(|(_, s)| s.clone())
    }

    /// Sources opened so far (originals and proxies; `perf.stats`).
    pub fn open_sources(&self) -> usize {
        self.sources.read().unwrap_or_else(|e| e.into_inner()).len() + self.proxies.read().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn use_proxies(&self) -> bool {
        self.use_proxies.load(Ordering::Relaxed)
    }

    pub fn set_use_proxies(&self, on: bool) {
        self.use_proxies.store(on, Ordering::Relaxed);
    }

    /// Why an item last rendered offline (None = online or not opened yet).
    pub fn offline_status(&self, item: ItemId) -> Option<OfflineStatus> {
        self.offline.read().unwrap_or_else(|e| e.into_inner()).get(&item).cloned()
    }

    /// Open a file through the registered openers.
    pub fn open_bytes(&self, name: &str, bytes: Arc<[u8]>) -> Result<SharedSource, MediaError> {
        let openers = self.openers.read().unwrap_or_else(|e| e.into_inner()).clone();
        filmcraft_media::open_bytes(name, bytes, &openers)
    }

    /// Read and open a file through `services` (through its random-access reader when the host
    /// has one: containers then read only their index now and samples on demand).
    pub fn open_file(&self, path: &str, services: &dyn Services) -> Result<SharedSource, MediaError> {
        let io = |e: std::io::Error| if e.kind() == std::io::ErrorKind::NotFound { MediaError::Offline(e.to_string()) } else { MediaError::Io(e.to_string()) };
        if let Some(r) = services.reader(path) {
            let openers = self.openers.read().unwrap_or_else(|e| e.into_inner()).clone();
            return filmcraft_media::reader::open_reader(&file_name(path), r.map_err(io)?, &filmcraft_codecs::reader_openers(), &openers);
        }
        let bytes = services.read_file(path).map_err(io)?;
        self.open_bytes(&file_name(path), bytes.into())
    }

    /// Open a file to look at it (Media Browser properties and thumbnails), not to import it: a
    /// format without a streaming reader is read whole only up to [`PROBE_WHOLE_FILE_MAX`] bytes
    /// (#157). Hosts without random-access readers open the file as [`Self::open_file`] does.
    pub fn probe_file(&self, path: &str, services: &dyn Services) -> Result<SharedSource, MediaError> {
        let Some(r) = services.reader(path) else { return self.open_file(path, services) };
        let r = r.map_err(|e| if e.kind() == std::io::ErrorKind::NotFound { MediaError::Offline(e.to_string()) } else { MediaError::Io(e.to_string()) })?;
        let openers = self.openers.read().unwrap_or_else(|e| e.into_inner()).clone();
        filmcraft_media::reader::open_reader_within(&file_name(path), r, &filmcraft_codecs::reader_openers(), &openers, PROBE_WHOLE_FILE_MAX)
    }

    /// Resolve (and cache) the source for a project item, using proxies when they are enabled.
    pub fn source_for(&self, p: &Project, item: ItemId, services: &dyn Services) -> Option<SharedSource> {
        self.resolve(p, item, services, self.use_proxies())
    }

    /// Resolve the full-resolution source for an item (never a proxy).
    pub fn full_res_source(&self, p: &Project, item: ItemId, services: &dyn Services) -> Option<SharedSource> {
        self.resolve(p, item, services, false)
    }

    fn resolve(&self, p: &Project, item: ItemId, services: &dyn Services, proxies: bool) -> Option<SharedSource> {
        let (item, m, _) = p.resolve_media(item)?;
        let it = p.item(item)?;
        if proxies
            && !m.offline
            && let Some(MediaRef::File { path }) = &m.proxy
            && let Some(s) = self.proxy_source(item, path, m, services)
        {
            return Some(s);
        }
        let key = media_key(m);
        if let Some((k, s)) = self.sources.read().unwrap_or_else(|e| e.into_inner()).get(&item)
            && (k.is_empty() || *k == key)
        {
            return Some(s.clone());
        }
        let src: SharedSource = match &m.media {
            MediaRef::Generator(g) => {
                let v = m.info.video.as_ref();
                let (w, h, r) = v.map(|v| (v.width, v.height, v.frame_rate)).unwrap_or((1920, 1080, Default::default()));
                Arc::new(GeneratorSource::new(g.clone(), w, h, r, m.info.duration).with_name(&it.name))
            }
            MediaRef::File { path } if m.offline => {
                self.set_offline(item, OfflineReason::MadeOffline, path, "made offline");
                Arc::new(SlateSource::new(m.info.clone(), &file_name(path), OfflineReason::MadeOffline))
            }
            MediaRef::File { path } => match if m.info.kind == filmcraft_media::MediaKind::ImageSequence {
                open_image_sequence(path, services, m.info.frame_rate(), &it.name)
            } else {
                self.open_file(path, services)
            } {
                Ok(s) => {
                    self.offline.write().unwrap_or_else(|e| e.into_inner()).remove(&item);
                    s
                }
                Err(_) if filmcraft_media::pending::is_set() => {
                    // the host is still fetching the file's bytes: show the slate for now, retry later
                    return Some(Arc::new(SlateSource::new(m.info.clone(), &file_name(path), OfflineReason::Missing)));
                }
                Err(e) => {
                    log::warn!("media offline: {path}: {e}");
                    let reason = if matches!(e, MediaError::Offline(_)) { OfflineReason::Missing } else { OfflineReason::Unreadable };
                    self.set_offline(item, reason, path, &e.to_string());
                    Arc::new(SlateSource::new(m.info.clone(), &file_name(path), reason))
                }
            },
        };
        self.sources.write().unwrap_or_else(|e| e.into_inner()).insert(item, (key, src.clone()));
        Some(src)
    }

    fn set_offline(&self, item: ItemId, reason: OfflineReason, path: &str, error: &str) {
        self.offline.write().unwrap_or_else(|e| e.into_inner()).insert(item, OfflineStatus { reason, path: path.to_string(), error: error.to_string() });
    }

    fn proxy_source(&self, item: ItemId, path: &str, m: &MediaClip, services: &dyn Services) -> Option<SharedSource> {
        let key = format!("proxy:{path}");
        if let Some((k, s)) = self.proxies.read().unwrap_or_else(|e| e.into_inner()).get(&item)
            && *k == key
        {
            return Some(s.clone());
        }
        match self.open_file(path, services) {
            Ok(s) => {
                // a proxy with fewer audio streams than the original (say one track of an OBS
                // recording's seven) still stands in for the picture; the audio comes from the
                // original, so every clip keeps the stream it plays
                let audio_from = if s.info().audio_streams.len() < m.info.audio_streams.len() {
                    log::warn!("proxy has fewer audio streams than the original: {path}; audio from full-resolution media");
                    match &m.media {
                        MediaRef::File { path: original } if !m.offline => self.open_file(original, services).ok(),
                        _ => None,
                    }
                } else {
                    None
                };
                let src: SharedSource = Arc::new(ProxySource { proxy: s, info: m.info.clone(), audio_from });
                self.proxies.write().unwrap_or_else(|e| e.into_inner()).insert(item, (key, src.clone()));
                Some(src)
            }
            Err(e) => {
                // a missing proxy falls back to the full-resolution media
                log::warn!("proxy offline: {path}: {e}");
                None
            }
        }
    }

    /// A render-side provider bound to a project snapshot (proxies when enabled).
    pub fn provider(self: &Arc<Self>, project: Arc<Project>, services: Arc<dyn Services>) -> PoolProvider {
        PoolProvider { pool: self.clone(), project, services, full_res: false }
    }

    /// A provider that always reads full-resolution media (export).
    pub fn full_res_provider(self: &Arc<Self>, project: Arc<Project>, services: Arc<dyn Services>) -> PoolProvider {
        PoolProvider { pool: self.clone(), project, services, full_res: true }
    }
}

/// Implements [`filmcraft_render::SourceProvider`] over the pool for one project snapshot.
pub struct PoolProvider {
    pub pool: Arc<MediaPool>,
    pub project: Arc<Project>,
    pub services: Arc<dyn Services>,
    /// Ignore proxies (export always renders full resolution).
    pub full_res: bool,
}

impl filmcraft_render::SourceProvider for PoolProvider {
    fn source(&self, item: ItemId) -> Option<SharedSource> {
        if self.full_res { self.pool.full_res_source(&self.project, item, &*self.services) } else { self.pool.source_for(&self.project, item, &*self.services) }
    }
}

/// The offline slate as a media source: video frames are the slate at the requested scale of the
/// item's frame size; audio is silence.
pub struct SlateSource {
    info: MediaInfo,
    name: String,
    reason: OfflineReason,
    last: Mutex<Option<Arc<VideoFrame>>>,
}

impl SlateSource {
    pub fn new(info: MediaInfo, name: &str, reason: OfflineReason) -> Self {
        Self { info, name: name.to_string(), reason, last: Mutex::new(None) }
    }
    pub fn reason(&self) -> OfflineReason {
        self.reason
    }
}

impl MediaSource for SlateSource {
    fn info(&self) -> &MediaInfo {
        &self.info
    }
    fn video_frame(&self, req: FrameRequest) -> filmcraft_media::Result<Arc<VideoFrame>> {
        let v = self.info.video.as_ref().ok_or(MediaError::NoStream("video"))?;
        let s = req.scale.clamp(1.0 / 64.0, 1.0);
        let (w, h) = (((v.width as f32 * s).round() as u32).max(2), ((v.height as f32 * s).round() as u32).max(2));
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(f) = last.as_ref()
            && f.width == w
            && f.height == h
        {
            return Ok(Arc::new((**f).clone().with_pts(req.time)));
        }
        let px = filmcraft_render::offline::slate_rgba8(w as usize, h as usize, &self.name, self.reason);
        let f = Arc::new(VideoFrame::rgba8(w, h, px));
        *last = Some(f.clone());
        Ok(Arc::new((*f).clone().with_pts(req.time)))
    }
    fn audio(&self, _start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<AudioBuffer> {
        Ok(AudioBuffer::silence(sample_rate, self.info.audio().map_or(2, |a| a.channels as usize), frames))
    }
}

/// A proxy standing in for its full-resolution media: reports the full-resolution info and hands
/// out the proxy's (smaller) frames. A proxy whose aspect ratio differs from the original is
/// resampled to the original's aspect so it lines up exactly.
pub struct ProxySource {
    pub proxy: SharedSource,
    pub info: MediaInfo,
    /// Where audio comes from when the proxy has fewer audio streams than the original.
    pub audio_from: Option<SharedSource>,
}

impl MediaSource for ProxySource {
    fn info(&self) -> &MediaInfo {
        &self.info
    }
    fn video_frame(&self, req: FrameRequest) -> filmcraft_media::Result<Arc<VideoFrame>> {
        let full = self.info.video.as_ref().ok_or(MediaError::NoStream("video"))?;
        let pv = self.proxy.info().video.as_ref().map(|v| (v.width, v.height)).unwrap_or((full.width, full.height));
        // ask the proxy for the scale relative to its own size
        let k = (full.width as f32 / pv.0.max(1) as f32).max(1.0);
        let f = self.proxy.video_frame(FrameRequest { time: req.time, scale: (req.scale * k).min(1.0) })?;
        let want_h = (f.width as f64 * full.height as f64 / full.width.max(1) as f64).round() as u32;
        if want_h.abs_diff(f.height) <= 1 {
            return Ok(f);
        }
        // different aspect: resample (nearest) to the original's aspect at the proxy's width
        let src = f.to_rgba8();
        let (w, h) = (f.width as usize, want_h.max(1) as usize);
        let mut out = vec![0u8; w * h * 4];
        for y in 0..h {
            let sy = ((y as f64 + 0.5) * f.height as f64 / h as f64) as usize;
            let row = &src[sy.min(f.height as usize - 1) * w * 4..][..w * 4];
            out[y * w * 4..(y + 1) * w * 4].copy_from_slice(row);
        }
        Ok(Arc::new(VideoFrame::rgba8(w as u32, h as u32, out).with_pts(f.pts)))
    }
    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<AudioBuffer> {
        self.audio_from.as_ref().unwrap_or(&self.proxy).audio(start, frames, sample_rate)
    }
    fn audio_stream(&self, stream: usize, start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<AudioBuffer> {
        self.audio_from.as_ref().unwrap_or(&self.proxy).audio_stream(stream, start, frames, sample_rate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_project::ItemKind;

    /// A host whose whole-file read fails: opening must go through the reader.
    struct ReaderOnly(Arc<[u8]>);
    impl Services for ReaderOnly {
        fn read_file(&self, path: &str) -> std::io::Result<Vec<u8>> {
            Err(std::io::Error::other(format!("{path}: read whole")))
        }
        fn write_file(&self, _path: &str, _data: &[u8]) -> std::io::Result<()> {
            Ok(())
        }
        fn reader(&self, _path: &str) -> Option<std::io::Result<filmcraft_media::SharedReader>> {
            Some(Ok(Arc::new(filmcraft_media::reader::MemReader(self.0.clone()))))
        }
    }

    #[test]
    fn cyclic_subclips_do_not_recurse_in_media_or_rendering() {
        let mut project = Project::new("damaged references");
        let a = project.add_item(
            "A",
            filmcraft_project::Label::Iris,
            ItemKind::Subclip {
                parent: ItemId(999),
                range: filmcraft_time::TimeRange::new(filmcraft_time::Tick::ZERO, filmcraft_time::Tick(1000)),
                restrict_trims: false,
            },
            None,
        );
        let b = project.add_item(
            "B",
            filmcraft_project::Label::Iris,
            ItemKind::Subclip {
                parent: a,
                range: filmcraft_time::TimeRange::new(filmcraft_time::Tick::ZERO, filmcraft_time::Tick(1000)),
                restrict_trims: false,
            },
            None,
        );
        let pool = MediaPool::default();
        for parent in [a, b] {
            let ItemKind::Subclip { parent: reference, .. } = &mut project.item_mut(a).unwrap().kind else { panic!() };
            *reference = parent;
            assert!(pool.source_for(&project, a, &crate::FsServices).is_none());
            assert!(pool.full_res_source(&project, a, &crate::FsServices).is_none());
            assert!(crate::media_duration(&project, &pool, a).is_none());
            assert!(filmcraft_render::source_size(&project, a).is_none());
            assert!(project.resolve_media(a).is_none());
            assert!(filmcraft_render::colorman::override_of(&project, a).is_none());
            assert!(filmcraft_render::colorman::source_peak_nits(&project, a).is_none());
            assert!(crate::sync::project_clip(&project, a).is_none());
            let mut session = crate::Session { project: Arc::new(project.clone()), ..Default::default() };
            session.state.project_selection = vec![a];
            assert!(!session.is_enabled("file.mediaProperties"));
            assert!(session.execute("transcript.generate", serde_json::json!({"items":[a.0]})).is_err());
        }
    }

    #[test]
    fn open_file_prefers_the_hosts_reader() {
        let wav = filmcraft_media::wav::write_wav16(&[0.0, 0.5, -0.5, 0.25], 2, 48_000);
        let s = MediaPool::default().open_file("a.wav", &ReaderOnly(wav.into())).unwrap();
        assert!(s.info().has_audio());
    }

    /// The desktop host reads media in place instead of loading every clip into memory.
    #[cfg(any(unix, windows))]
    #[test]
    fn fs_services_open_media_through_a_file_reader() {
        let dir = std::env::temp_dir().join(format!("filmcraft-pool-reader-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.wav");
        let wav = filmcraft_media::wav::write_wav16(&[0.0, 0.5, -0.5, 0.25], 2, 48_000);
        std::fs::write(&path, &wav).unwrap();
        let p = path.to_string_lossy().to_string();
        let r = crate::FsServices.reader(&p).expect("a reader").unwrap();
        assert_eq!(r.len(), wav.len() as u64);
        assert!(MediaPool::default().open_file(&p, &crate::FsServices).unwrap().info().has_audio());
        let missing = dir.join("missing.wav").to_string_lossy().to_string();
        assert!(matches!(MediaPool::default().open_file(&missing, &crate::FsServices), Err(MediaError::Offline(_))));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
