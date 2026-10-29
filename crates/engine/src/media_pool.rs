//! The media pool: one shared [`MediaSource`] per project item, created lazily from its
//! [`MediaRef`], plus the registry of container/codec openers.
//!
//! - Sources are cached per item **and per reference** (the file path, the offline flag), so a
//!   relink, Make Offline or their undo picks up the right file on the next frame.
//! - Media that can't be opened (missing, unreadable) or was made offline renders the offline
//!   slate ([`filmcraft_render::offline`]) instead of failing; the reason is kept for the UI. So
//!   do the frames of a file that opens but whose picture FilmCraft cannot decode (a codec or
//!   profile without a decoder).
//! - With proxies enabled ([`MediaPool::set_use_proxies`]) items with an attached proxy are read
//!   from it. Proxy frames are smaller; the source still reports the full-resolution size, and the
//!   compositor derives its pixel scale from the frame it gets, so effects and Motion render the
//!   same picture at lower resolution. Export asks for [`MediaPool::full_res_provider`].
//! - Paths that came from a document (the project file, a template, an interchange file) are
//!   opened through [`crate::doc_path`] ([`MediaPool::follow`]); a refused one renders as missing.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

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

/// An offline item an export or a rendered frame drew the slate for (#1054). `status` is the
/// word `media.status` uses: `missing`, `offline` (made offline) or `unreadable`.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct OfflineMedia {
    pub item: u64,
    pub name: String,
    pub path: String,
    pub status: &'static str,
    pub error: String,
}

pub struct MediaPool {
    /// item → (reference key, source)
    sources: RwLock<HashMap<ItemId, (String, SharedSource)>>,
    proxies: RwLock<HashMap<ItemId, (String, SharedSource)>>,
    offline: Arc<RwLock<HashMap<ItemId, OfflineStatus>>>,
    use_proxies: AtomicBool,
    /// Per item: video frames that failed in a row in the monitors (see [`Self::monitor_source`]).
    failures: Arc<Mutex<HashMap<ItemId, u32>>>,
    /// Openers tried before the built-in ones (MP4/MOV + codecs register here).
    pub openers: RwLock<Vec<Opener>>,
    /// Media and proxy paths that came from a document, with that document's path (see
    /// [`Self::follow`]).
    stored: RwLock<HashMap<String, Option<PathBuf>>>,
}

impl Default for MediaPool {
    /// A pool with the built-in container/codec openers registered.
    fn default() -> Self {
        Self {
            sources: RwLock::new(HashMap::new()),
            proxies: RwLock::new(HashMap::new()),
            offline: Arc::new(RwLock::new(HashMap::new())),
            use_proxies: AtomicBool::new(false),
            failures: Arc::default(),
            openers: RwLock::new(filmcraft_codecs::openers()),
            stored: RwLock::default(),
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
        self.openers.write().unwrap_or_else(PoisonError::into_inner).push(o);
    }

    /// Cache a source for an item, valid whatever the item's reference (generators).
    pub fn insert(&self, item: ItemId, src: SharedSource) {
        self.sources.write().unwrap_or_else(PoisonError::into_inner).insert(item, (String::new(), src));
    }

    /// Cache a source for an item under its [`media_key`].
    pub fn insert_keyed(&self, item: ItemId, key: String, src: SharedSource) {
        self.sources.write().unwrap_or_else(PoisonError::into_inner).insert(item, (key, src));
    }

    /// Cache a source opened from `path` for an item.
    pub fn insert_file(&self, item: ItemId, path: &str, src: SharedSource) {
        self.offline.write().unwrap_or_else(PoisonError::into_inner).remove(&item);
        let src = self.unsupported_as_slate(item, path, src);
        self.sources.write().unwrap_or_else(PoisonError::into_inner).insert(item, (format!("file:{path}"), src));
    }

    /// `src` (opened from `path` for `item`) showing the unreadable slate for the frames it
    /// cannot decode.
    fn unsupported_as_slate(&self, item: ItemId, path: &str, src: SharedSource) -> SharedSource {
        let slate = SlateSource::new(src.info().clone(), &file_name(path), OfflineReason::Unreadable);
        Arc::new(UnsupportedAsSlate { inner: src, slate, item, path: path.to_string(), offline: self.offline.clone() })
    }

    pub fn remove(&self, item: ItemId) {
        self.sources.write().unwrap_or_else(PoisonError::into_inner).remove(&item);
        self.proxies.write().unwrap_or_else(PoisonError::into_inner).remove(&item);
        self.offline.write().unwrap_or_else(PoisonError::into_inner).remove(&item);
    }

    /// Record the media and proxy paths of `p`, loaded from the document at `document` (None: one
    /// without a file), as stored paths: they are opened only as [`crate::doc_path::resolve`]
    /// allows. A path already recorded keeps the document it came from first.
    pub fn add_stored(&self, p: &Project, document: Option<&str>) {
        let paths = p.items.values().filter_map(|i| i.as_media()).flat_map(|m| [Some(&m.media), m.proxy.as_ref()]).filter_map(|r| match r {
            Some(MediaRef::File { path }) => Some(path.clone()),
            _ => None,
        });
        self.add_stored_paths(paths, document);
    }

    /// [`Self::add_stored`] for single paths.
    pub fn add_stored_paths(&self, paths: impl IntoIterator<Item = String>, document: Option<&str>) {
        let document = document.filter(|d| !d.is_empty()).map(PathBuf::from);
        let mut stored = self.stored.write().unwrap_or_else(PoisonError::into_inner);
        for p in paths {
            stored.entry(p).or_insert_with(|| document.clone());
        }
    }

    /// Forget the stored paths (a new project).
    pub fn clear_stored(&self) {
        self.stored.write().unwrap_or_else(PoisonError::into_inner).clear();
    }

    /// The user picked `path` in this session (import, Link Media): open it as given.
    pub fn allow(&self, path: &str) {
        self.stored.write().unwrap_or_else(PoisonError::into_inner).remove(path);
    }

    /// The file to open for `path`, a media or proxy path of the project: a stored path as
    /// [`crate::doc_path::resolve`] resolves it, any other path as given.
    pub fn follow(&self, path: &str) -> Result<String, crate::doc_path::NotFollowed> {
        // The web host reads only the files the user gave it, under its own virtual paths.
        if cfg!(target_arch = "wasm32") {
            return Ok(path.to_string());
        }
        match self.stored.read().unwrap_or_else(PoisonError::into_inner).get(path) {
            Some(document) => crate::doc_path::resolve(path, document.as_deref()),
            None => Ok(path.to_string()),
        }
    }

    /// [`Self::follow`] as a media error (a refused path is a missing file).
    fn follow_media(&self, path: &str) -> Result<String, MediaError> {
        self.follow(path).map_err(|why| MediaError::Offline(format!("{path} is not opened: {why}")))
    }

    /// Forget every opened source (files are re-read on next use).
    pub fn clear(&self) {
        self.sources.write().unwrap_or_else(PoisonError::into_inner).clear();
        self.proxies.write().unwrap_or_else(PoisonError::into_inner).clear();
        self.offline.write().unwrap_or_else(PoisonError::into_inner).clear();
    }

    pub fn cached(&self, item: ItemId) -> Option<SharedSource> {
        self.sources.read().unwrap_or_else(PoisonError::into_inner).get(&item).map(|(_, s)| s.clone())
    }

    /// Sources opened so far (originals and proxies; `perf.stats`).
    pub fn open_sources(&self) -> usize {
        self.sources.read().unwrap_or_else(PoisonError::into_inner).len() + self.proxies.read().unwrap_or_else(PoisonError::into_inner).len()
    }

    pub fn use_proxies(&self) -> bool {
        self.use_proxies.load(Ordering::Relaxed)
    }

    pub fn set_use_proxies(&self, on: bool) {
        self.use_proxies.store(on, Ordering::Relaxed);
    }

    /// `src` (item `item`) as the monitors show it: once [`DAMAGED_AFTER`] of its video frames in a
    /// row fail to decode or read (a damaged file), its failing frames show the unreadable slate
    /// instead of nothing, until a frame decodes again (#626, follow-up to #762). Only for preview:
    /// exports and other renders use the pool's sources directly, so they never contain the slate.
    pub fn monitor_source(&self, item: ItemId, src: SharedSource) -> SharedSource {
        Arc::new(DamagedAsSlate {
            slate: SlateSource::new(src.info().clone(), &src.info().name, OfflineReason::Unreadable),
            inner: src,
            item,
            failures: self.failures.clone(),
        })
    }

    /// Why an item last rendered offline (None = online or not opened yet).
    pub fn offline_status(&self, item: ItemId) -> Option<OfflineStatus> {
        self.offline.read().unwrap_or_else(PoisonError::into_inner).get(&item).cloned()
    }

    /// Open a file through the registered openers.
    pub fn open_bytes(&self, name: &str, bytes: Arc<[u8]>) -> Result<SharedSource, MediaError> {
        let openers = self.openers.read().unwrap_or_else(PoisonError::into_inner).clone();
        filmcraft_media::open_bytes(name, bytes, &openers)
    }

    /// Read and open a file through `services` (through its random-access reader when the host
    /// has one: containers then read only their index now and samples on demand).
    pub fn open_file(&self, path: &str, services: &dyn Services) -> Result<SharedSource, MediaError> {
        let io = |e: std::io::Error| if e.kind() == std::io::ErrorKind::NotFound { MediaError::Offline(e.to_string()) } else { MediaError::Io(e.to_string()) };
        if let Some(r) = services.reader(path) {
            let openers = self.openers.read().unwrap_or_else(PoisonError::into_inner).clone();
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
        let openers = self.openers.read().unwrap_or_else(PoisonError::into_inner).clone();
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
        if let Some((k, s)) = self.sources.read().unwrap_or_else(PoisonError::into_inner).get(&item)
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
            MediaRef::File { path } => match self.follow_media(path).and_then(|file| {
                if m.info.kind == filmcraft_media::MediaKind::ImageSequence {
                    open_image_sequence(&file, services, m.info.frame_rate(), &it.name)
                } else {
                    self.open_file(&file, services)
                }
            }) {
                Ok(s) => {
                    self.offline.write().unwrap_or_else(PoisonError::into_inner).remove(&item);
                    self.unsupported_as_slate(item, path, s)
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
        self.sources.write().unwrap_or_else(PoisonError::into_inner).insert(item, (key, src.clone()));
        Some(src)
    }

    fn set_offline(&self, item: ItemId, reason: OfflineReason, path: &str, error: &str) {
        self.offline.write().unwrap_or_else(PoisonError::into_inner).insert(item, OfflineStatus { reason, path: path.to_string(), error: error.to_string() });
    }

    fn proxy_source(&self, item: ItemId, path: &str, m: &MediaClip, services: &dyn Services) -> Option<SharedSource> {
        let key = format!("proxy:{path}");
        if let Some((k, s)) = self.proxies.read().unwrap_or_else(PoisonError::into_inner).get(&item)
            && *k == key
        {
            return Some(s.clone());
        }
        match self.follow_media(path).and_then(|file| self.open_file(&file, services)) {
            Ok(s) => {
                // a proxy with fewer audio streams than the original (say one track of an OBS
                // recording's seven) still stands in for the picture; the audio comes from the
                // original, so every clip keeps the stream it plays
                let audio_from = if s.info().audio_streams.len() < m.info.audio_streams.len() {
                    log::warn!("proxy has fewer audio streams than the original: {path}; audio from full-resolution media");
                    match &m.media {
                        MediaRef::File { path: original } if !m.offline => self.follow_media(original).and_then(|file| self.open_file(&file, services)).ok(),
                        _ => None,
                    }
                } else {
                    None
                };
                let src: SharedSource = Arc::new(ProxySource { proxy: s, info: m.info.clone(), audio_from });
                self.proxies.write().unwrap_or_else(PoisonError::into_inner).insert(item, (key, src.clone()));
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
        PoolProvider { pool: self.clone(), project, services, full_res: false, used: None }
    }

    /// A provider that always reads full-resolution media (export).
    pub fn full_res_provider(self: &Arc<Self>, project: Arc<Project>, services: Arc<dyn Services>) -> PoolProvider {
        PoolProvider { pool: self.clone(), project, services, full_res: true, used: None }
    }
}

/// Implements [`filmcraft_render::SourceProvider`] over the pool for one project snapshot.
pub struct PoolProvider {
    pub pool: Arc<MediaPool>,
    pub project: Arc<Project>,
    pub services: Arc<dyn Services>,
    /// Ignore proxies (export always renders full resolution).
    pub full_res: bool,
    /// Items asked for so far, when recording ([`PoolProvider::recording`]).
    used: Option<Mutex<BTreeSet<ItemId>>>,
}

impl PoolProvider {
    /// This provider, remembering the items it is asked for so [`PoolProvider::offline_used`] can
    /// tell which of them rendered offline.
    pub fn recording(mut self) -> Self {
        self.used = Some(Mutex::new(BTreeSet::new()));
        self
    }

    /// The offline items among those asked for since [`PoolProvider::recording`], once each, in
    /// item order (subclips count as their media). Empty when not recording.
    pub fn offline_used(&self) -> Vec<OfflineMedia> {
        let Some(used) = &self.used else { return Vec::new() };
        let used = used.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let media: BTreeSet<ItemId> = used.into_iter().filter_map(|i| self.project.resolve_media(i).map(|(id, _, _)| id)).collect();
        media
            .into_iter()
            .filter_map(|id| {
                let st = self.pool.offline_status(id)?;
                let status = match st.reason {
                    OfflineReason::Missing => "missing",
                    OfflineReason::MadeOffline => "offline",
                    OfflineReason::Unreadable => "unreadable",
                };
                let name = self.project.item(id).map(|i| i.name.clone()).unwrap_or_default();
                Some(OfflineMedia { item: id.0, name, path: st.path, status, error: st.error })
            })
            .collect()
    }
}

impl filmcraft_render::SourceProvider for PoolProvider {
    fn source(&self, item: ItemId) -> Option<SharedSource> {
        if let Some(used) = &self.used {
            used.lock().unwrap_or_else(PoisonError::into_inner).insert(item);
        }
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
        let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
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

/// A file's source whose picture shows the unreadable slate for each frame its decoder reports as
/// unsupported (H.264 High 4:2:2, a codec without a decoder...), instead of no picture at all,
/// and marks the item unreadable with the reason, so the monitors and the Project panel say why
/// (#626). Its sound plays as before.
struct UnsupportedAsSlate {
    inner: SharedSource,
    slate: SlateSource,
    item: ItemId,
    path: String,
    offline: Arc<RwLock<HashMap<ItemId, OfflineStatus>>>,
}

impl MediaSource for UnsupportedAsSlate {
    fn info(&self) -> &MediaInfo {
        self.inner.info()
    }
    fn video_frame(&self, req: FrameRequest) -> filmcraft_media::Result<Arc<VideoFrame>> {
        match self.inner.video_frame(req) {
            Err(MediaError::Unsupported(why)) => {
                self.offline.write().unwrap_or_else(PoisonError::into_inner).entry(self.item).or_insert_with(|| {
                    log::warn!("media unreadable: {}: {why}", self.path);
                    OfflineStatus { reason: OfflineReason::Unreadable, path: self.path.clone(), error: why }
                });
                self.slate.video_frame(req)
            }
            r => r,
        }
    }
    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<AudioBuffer> {
        self.inner.audio(start, frames, sample_rate)
    }
    fn audio_stream(&self, stream: usize, start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<AudioBuffer> {
        self.inner.audio_stream(stream, start, frames, sample_rate)
    }
}

/// Consecutive failed video frames after which a monitor treats a source as damaged. A single
/// failure (one bad slice, a read hiccup) and the decoder's retry of it stay as they were; three
/// in a row (an eighth of a second at 24 fps) mean the file itself is damaged.
pub const DAMAGED_AFTER: u32 = 3;

/// See [`MediaPool::monitor_source`]. Counts `Decode` / `Io` failures per item; a successful frame
/// resets the count. Data still on its way (the web build's pending reads, see
/// [`filmcraft_media::pending`]) neither counts nor resets, and cancellation, absent video and
/// unsupported frames (already the slate, #762) pass through.
struct DamagedAsSlate {
    inner: SharedSource,
    slate: SlateSource,
    item: ItemId,
    failures: Arc<Mutex<HashMap<ItemId, u32>>>,
}

impl MediaSource for DamagedAsSlate {
    fn info(&self) -> &MediaInfo {
        self.inner.info()
    }
    fn video_frame(&self, req: FrameRequest) -> filmcraft_media::Result<Arc<VideoFrame>> {
        let r = self.inner.video_frame(req);
        let mut failures = self.failures.lock().unwrap_or_else(PoisonError::into_inner);
        match r {
            Ok(f) => {
                failures.remove(&self.item);
                Ok(f)
            }
            Err(MediaError::Decode(_) | MediaError::Io(_)) if !filmcraft_media::pending::is_set() => {
                let n = failures.entry(self.item).or_insert(0);
                *n = n.saturating_add(1);
                if *n >= DAMAGED_AFTER {
                    drop(failures);
                    return self.slate.video_frame(req);
                }
                r
            }
            r => r,
        }
    }
    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<AudioBuffer> {
        self.inner.audio(start, frames, sample_rate)
    }
    fn audio_stream(&self, stream: usize, start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<AudioBuffer> {
        self.inner.audio_stream(stream, start, frames, sample_rate)
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

    /// A file whose first frame decodes and whose later frames need a decoder FilmCraft lacks.
    struct PartlyDecodable(MediaInfo);
    impl MediaSource for PartlyDecodable {
        fn info(&self) -> &MediaInfo {
            &self.0
        }
        fn video_frame(&self, req: FrameRequest) -> filmcraft_media::Result<Arc<VideoFrame>> {
            match req.time.0 {
                0 => Ok(Arc::new(VideoFrame::rgba8(2, 2, vec![7; 16]))),
                1 => Err(MediaError::Decode("damaged slice".into())),
                _ => Err(MediaError::Unsupported("H.264: field pictures".into())),
            }
        }
        fn audio(&self, _start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<AudioBuffer> {
            Ok(AudioBuffer::silence(sample_rate, 3, frames))
        }
    }

    /// #626: frames the decoder does not support show the unreadable slate (not nothing) and the
    /// item says why; decodable frames, damaged ones and the sound are left alone.
    #[test]
    fn undecodable_frames_show_the_unreadable_slate() {
        let info = GeneratorSource::new(filmcraft_media::Generator::ColorMatte { color: [0.0; 4] }, 16, 8, Default::default(), filmcraft_time::Tick(100))
            .info()
            .clone();
        let pool = MediaPool::default();
        let item = ItemId(1);
        pool.insert_file(item, "/media/cam/a.mov", Arc::new(PartlyDecodable(info)));
        let src = pool.cached(item).unwrap();
        let at = |t| src.video_frame(FrameRequest { time: filmcraft_time::Tick(t), scale: 1.0 });
        assert_eq!(at(0).unwrap().to_rgba8(), vec![7; 16]);
        assert!(matches!(at(1), Err(MediaError::Decode(_))));
        assert!(pool.offline_status(item).is_none(), "nothing unsupported yet");
        let f = at(2).unwrap();
        assert_eq!((f.width, f.height), (16, 8));
        assert_eq!(f.to_rgba8(), filmcraft_render::offline::slate_rgba8(16, 8, "a.mov", OfflineReason::Unreadable));
        let st = pool.offline_status(item).unwrap();
        assert_eq!((st.reason, st.path.as_str(), st.error.as_str()), (OfflineReason::Unreadable, "/media/cam/a.mov", "H.264: field pictures"));
        assert_eq!(src.audio(0, 4, 48_000).unwrap().channels.len(), 3);
        // linking the item to another file forgets the reason
        pool.insert_file(item, "/media/cam/b.mov", src.clone());
        assert!(pool.offline_status(item).is_none());
    }

    /// A source whose video decodes (`OK`) or fails as its mode says, switchable at run time.
    struct Flaky(MediaInfo, std::sync::atomic::AtomicU8);
    const OK: u8 = 0;
    const DAMAGED: u8 = 1;
    const IO: u8 = 2;
    const LOADING: u8 = 3;
    const CANCELLED: u8 = 4;
    const NO_VIDEO: u8 = 5;
    impl Flaky {
        fn set(&self, mode: u8) {
            self.1.store(mode, Ordering::Relaxed);
        }
    }
    impl MediaSource for Flaky {
        fn info(&self) -> &MediaInfo {
            &self.0
        }
        fn video_frame(&self, _: FrameRequest) -> filmcraft_media::Result<Arc<VideoFrame>> {
            Err(match self.1.load(Ordering::Relaxed) {
                OK => return Ok(Arc::new(VideoFrame::rgba8(2, 2, vec![7; 16]))),
                DAMAGED => MediaError::Decode("damaged slice".into()),
                IO => MediaError::Io("short read".into()),
                LOADING => {
                    // as the web build's WebCodecs / Blob sources do
                    filmcraft_media::pending::mark();
                    MediaError::Decode("still decoding".into())
                }
                CANCELLED => MediaError::Cancelled,
                _ => MediaError::NoStream("video"),
            })
        }
        fn audio(&self, _start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<AudioBuffer> {
            Ok(AudioBuffer::silence(sample_rate, 1, frames))
        }
    }

    fn flaky() -> (MediaPool, ItemId, Arc<Flaky>) {
        let info = GeneratorSource::new(filmcraft_media::Generator::ColorMatte { color: [0.0; 4] }, 16, 8, Default::default(), filmcraft_time::Tick(100))
            .info()
            .clone();
        (MediaPool::default(), ItemId(1), Arc::new(Flaky(info, Default::default())))
    }

    fn frame(src: &SharedSource) -> filmcraft_media::Result<Arc<VideoFrame>> {
        src.video_frame(FrameRequest { time: filmcraft_time::Tick(0), scale: 1.0 })
    }

    fn is_slate(r: &filmcraft_media::Result<Arc<VideoFrame>>, name: &str) -> bool {
        r.as_ref().is_ok_and(|f| f.to_rgba8() == filmcraft_render::offline::slate_rgba8(16, 8, name, OfflineReason::Unreadable))
    }

    /// #626 follow-up to #762: a damaged file (frames failing again and again) shows the
    /// unreadable slate in the monitors; one failure does not, and a decoded frame clears it.
    #[test]
    fn monitors_show_the_slate_for_persistently_failing_frames_only() {
        let (pool, item, src) = flaky();
        let name = src.info().name.clone();
        let mon = pool.monitor_source(item, src.clone());
        for mode in [DAMAGED, IO] {
            src.set(mode);
            for _ in 1..DAMAGED_AFTER {
                assert!(frame(&mon).is_err(), "a transient failure stays as it was");
            }
            assert!(is_slate(&frame(&mon), &name), "persistent failure shows the slate");
            assert!(is_slate(&frame(&mon), &name));
            // recovery clears it, and one failure after that is transient again
            src.set(OK);
            assert_eq!(frame(&mon).unwrap().to_rgba8(), vec![7; 16]);
            src.set(DAMAGED);
            assert!(frame(&mon).is_err());
            src.set(OK);
            assert!(frame(&mon).is_ok());
        }
        // cancellation and absent video never become the slate
        for mode in [CANCELLED, NO_VIDEO] {
            src.set(mode);
            for _ in 0..DAMAGED_AFTER * 2 {
                assert!(frame(&mon).is_err());
            }
        }
    }

    /// Pictures still on their way (the web build's pending reads) never count as damage.
    #[test]
    fn still_decoding_frames_never_show_the_slate() {
        let (pool, item, src) = flaky();
        let mon = pool.monitor_source(item, src.clone());
        src.set(LOADING);
        for _ in 0..DAMAGED_AFTER * 4 {
            assert!(frame(&mon).is_err());
            assert!(filmcraft_media::pending::take());
        }
        // and they do not interrupt a run of real failures either way: a following damaged frame
        // is still only the first
        src.set(DAMAGED);
        assert!(frame(&mon).is_err());
    }

    /// Exports read the pool's own sources: a damaged file never burns the slate into them,
    /// however often the monitors have shown it.
    #[test]
    fn export_sources_never_show_the_damaged_slate() {
        let (pool, item, src) = flaky();
        let name = src.info().name.clone();
        pool.insert(item, src.clone());
        let mon = pool.monitor_source(item, pool.cached(item).unwrap());
        src.set(DAMAGED);
        for _ in 0..DAMAGED_AFTER {
            let _ = frame(&mon);
        }
        assert!(is_slate(&frame(&mon), &name));
        let export = pool.cached(item).unwrap();
        for _ in 0..DAMAGED_AFTER * 2 {
            assert!(matches!(frame(&export), Err(MediaError::Decode(_))));
        }
    }

    /// #626 end to end: an H.264 profile FilmCraft does not decode yet (4:4:4) imports and shows
    /// the unreadable slate with the decoder's reason, instead of a black picture.
    #[test]
    fn ffmpeg_unsupported_h264_profile_shows_the_unreadable_slate() {
        let Some(ffmpeg) = filmcraft_testkit::oracle::ffmpeg_or_skip("media_pool") else { return };
        let path = filmcraft_testkit::fixtures_dir("engine/media_pool").join("h264_444_64x32.mp4");
        let Some(path) = filmcraft_testkit::fixtures::generate(&path, |tmp| {
            std::process::Command::new(&ffmpeg)
                .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc2=size=64x32:rate=24:duration=0.5"])
                .args(["-c:v", "libx264", "-preset", "fast", "-pix_fmt", "yuv444p"])
                .arg(tmp)
                .status()
                .is_ok_and(|s| s.success())
        }) else {
            return;
        };
        let mut s = crate::Session::default();
        let r = s.execute("file.import", serde_json::json!({"paths": [path.to_string_lossy()]})).unwrap();
        let item = ItemId(r["items"][0].as_u64().unwrap());
        let src = s.media.source_for(&s.project, item, &*s.services).unwrap();
        let f = src.video_frame(FrameRequest { time: filmcraft_time::Tick::ZERO, scale: 1.0 }).unwrap();
        assert_eq!(f.to_rgba8(), filmcraft_render::offline::slate_rgba8(64, 32, "h264_444_64x32.mp4", OfflineReason::Unreadable));
        let st = s.media.offline_status(item).unwrap();
        assert_eq!(st.reason, OfflineReason::Unreadable);
        assert!(st.error.contains("H.264") && st.error.contains("chroma_format_idc 3"), "{}", st.error);
    }
}
