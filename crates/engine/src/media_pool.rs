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

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};

use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{FrameRequest, MediaError, MediaInfo, MediaSource, Opener, SharedSource};
use filmcraft_project::{ItemId, MediaClip, MediaRef, Project};
use filmcraft_render::offline::{MAX_OFFLINE_SLATE_PIXELS, MAX_OFFLINE_SLATE_SIDE, OfflineReason};

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
    slate_cache: RwLock<Arc<SharedSlateCache>>,
    use_proxies: AtomicBool,
    /// Per item: video frames that failed in a row in the monitors (see [`Self::monitor_source`]).
    failures: Arc<Mutex<HashMap<ItemId, u32>>>,
    /// Openers tried before the built-in ones (MP4/MOV + codecs register here).
    pub openers: RwLock<Vec<Opener>>,
}

impl Default for MediaPool {
    /// A pool with the built-in container/codec openers registered.
    fn default() -> Self {
        Self {
            sources: RwLock::new(HashMap::new()),
            proxies: RwLock::new(HashMap::new()),
            offline: Arc::new(RwLock::new(HashMap::new())),
            slate_cache: RwLock::new(Arc::default()),
            use_proxies: AtomicBool::new(false),
            failures: Arc::default(),
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
        self.offline.write().unwrap_or_else(|e| e.into_inner()).remove(&item);
        let src = self.unsupported_as_slate(item, path, src);
        self.sources.write().unwrap_or_else(|e| e.into_inner()).insert(item, (format!("file:{path}"), src));
    }

    fn slate_source(&self, info: MediaInfo, name: &str, reason: OfflineReason) -> SlateSource {
        let cache = self.slate_cache.read().unwrap_or_else(|e| e.into_inner()).clone();
        SlateSource::with_cache(info, name, reason, cache)
    }

    /// `src` (opened from `path` for `item`) showing the unreadable slate for the frames it
    /// cannot decode.
    fn unsupported_as_slate(&self, item: ItemId, path: &str, src: SharedSource) -> SharedSource {
        let slate = self.slate_source(src.info().clone(), &file_name(path), OfflineReason::Unreadable);
        Arc::new(UnsupportedAsSlate { inner: src, slate, item, path: path.to_string(), offline: self.offline.clone() })
    }

    pub fn remove(&self, item: ItemId) {
        self.sources.write().unwrap_or_else(|e| e.into_inner()).remove(&item);
        self.proxies.write().unwrap_or_else(|e| e.into_inner()).remove(&item);
        self.offline.write().unwrap_or_else(|e| e.into_inner()).remove(&item);
    }

    /// Start another project without sharing source or slate caches with in-flight readers.
    /// Keep host-registered openers and the editor's proxy preference.
    pub(crate) fn fresh_for_project(&self) -> Self {
        let next = Self::default();
        next.set_use_proxies(self.use_proxies());
        let openers = self.openers.read().unwrap_or_else(|e| e.into_inner()).clone();
        *next.openers.write().unwrap_or_else(|e| e.into_inner()) = openers;
        next
    }

    /// Forget every opened source (files are re-read on next use).
    pub fn clear(&self) {
        self.sources.write().unwrap_or_else(|e| e.into_inner()).clear();
        self.proxies.write().unwrap_or_else(|e| e.into_inner()).clear();
        self.offline.write().unwrap_or_else(|e| e.into_inner()).clear();
        *self.slate_cache.write().unwrap_or_else(|e| e.into_inner()) = Arc::default();
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

    /// `src` (item `item`) as the monitors show it: once [`DAMAGED_AFTER`] of its video frames in a
    /// row fail to decode or read (a damaged file), its failing frames show the unreadable slate
    /// instead of nothing, until a frame decodes again (#626, follow-up to #762). Only for preview:
    /// exports and other renders use the pool's sources directly, so they never contain the slate.
    pub fn monitor_source(&self, item: ItemId, src: SharedSource) -> SharedSource {
        Arc::new(DamagedAsSlate {
            slate: self.slate_source(src.info().clone(), &src.info().name, OfflineReason::Unreadable),
            inner: src,
            item,
            failures: self.failures.clone(),
        })
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
                Arc::new(self.slate_source(m.info.clone(), &file_name(path), OfflineReason::MadeOffline))
            }
            MediaRef::File { path } => match if m.info.kind == filmcraft_media::MediaKind::ImageSequence {
                open_image_sequence(path, services, m.info.frame_rate(), &it.name)
            } else {
                self.open_file(path, services)
            } {
                Ok(s) => {
                    self.offline.write().unwrap_or_else(|e| e.into_inner()).remove(&item);
                    self.unsupported_as_slate(item, path, s)
                }
                Err(_) if filmcraft_media::pending::is_set() => {
                    // the host is still fetching the file's bytes: show the slate for now, retry later
                    return Some(Arc::new(self.slate_source(m.info.clone(), &file_name(path), OfflineReason::Missing)));
                }
                Err(e) => {
                    log::warn!("media offline: {path}: {e}");
                    let reason = if matches!(e, MediaError::Offline(_)) { OfflineReason::Missing } else { OfflineReason::Unreadable };
                    self.set_offline(item, reason, path, &e.to_string());
                    Arc::new(self.slate_source(m.info.clone(), &file_name(path), reason))
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
        let used = used.lock().unwrap_or_else(|e| e.into_inner()).clone();
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
            used.lock().unwrap_or_else(|e| e.into_inner()).insert(item);
        }
        if self.full_res { self.pool.full_res_source(&self.project, item, &*self.services) } else { self.pool.source_for(&self.project, item, &*self.services) }
    }
}

/// The offline slate as a media source: video frames have bounded preview dimensions; audio is
/// silence. Source metadata retains its original dimensions.
pub struct SlateSource {
    info: MediaInfo,
    name: Arc<str>,
    reason: OfflineReason,
    cache: Arc<SharedSlateCache>,
}

impl SlateSource {
    pub fn new(info: MediaInfo, name: &str, reason: OfflineReason) -> Self {
        Self::with_cache(info, name, reason, Arc::default())
    }

    fn with_cache(info: MediaInfo, name: &str, reason: OfflineReason, cache: Arc<SharedSlateCache>) -> Self {
        Self { info, name: filmcraft_render::offline::slate_display_name(name).into(), reason, cache }
    }
    pub fn reason(&self) -> OfflineReason {
        self.reason
    }
}

/// A 720p slate needs at most about 18.4 MB for the renderer's float RGBA image and
/// its RGBA8 copy; valid 16K source metadata must not force a larger placeholder.
const MAX_OFFLINE_SLATE_CACHE_BYTES: usize = 64 << 20;
const MAX_OFFLINE_SLATE_CACHE_ENTRIES: usize = 16;
// Cold admission uses three cache accesses per key; leave room for a full hot-set sweep.
const MAX_OFFLINE_SLATE_CACHE_IDLE_ACCESSES: u64 = (MAX_OFFLINE_SLATE_CACHE_ENTRIES as u64) * 8;
type SlateKey = (Arc<str>, u32, u32, &'static str);

#[derive(Default)]
struct SlateFrameCache {
    entries: HashMap<SlateKey, (u64, VideoFrame)>,
    bytes: usize,
    clock: u64,
}

impl SlateFrameCache {
    fn frame_bytes(frame: &VideoFrame) -> usize {
        frame.byte_size()
    }

    fn next_tick(&mut self) -> u64 {
        self.clock = self.clock.wrapping_add(1);
        self.clock
    }

    fn get(&mut self, key: &SlateKey) -> Option<VideoFrame> {
        let tick = self.next_tick();
        self.entries.get_mut(key).map(|(last, frame)| {
            *last = tick;
            frame.clone()
        })
    }

    fn insert(&mut self, key: SlateKey, frame: VideoFrame) {
        let cost = Self::frame_bytes(&frame);
        if cost > MAX_OFFLINE_SLATE_CACHE_BYTES || key.0.len() > 4096 {
            return;
        }
        if let Some((_, old)) = self.entries.remove(&key) {
            self.bytes = self.bytes.saturating_sub(Self::frame_bytes(&old));
        }
        let tick = self.next_tick();
        if self.entries.len() >= MAX_OFFLINE_SLATE_CACHE_ENTRIES || self.bytes.saturating_add(cost) > MAX_OFFLINE_SLATE_CACHE_BYTES {
            // Keep recently used slates so a one-entry overflow cannot evict the whole hot set.
            let stale_before = tick.saturating_sub(MAX_OFFLINE_SLATE_CACHE_IDLE_ACCESSES);
            let mut released = 0usize;
            self.entries.retain(|_, (last, old)| {
                if *last > stale_before {
                    true
                } else {
                    released = released.saturating_add(Self::frame_bytes(old));
                    false
                }
            });
            self.bytes = self.bytes.saturating_sub(released);
        }
        if self.entries.len() >= MAX_OFFLINE_SLATE_CACHE_ENTRIES || self.bytes.saturating_add(cost) > MAX_OFFLINE_SLATE_CACHE_BYTES {
            return;
        }
        self.entries.insert(key, (tick, frame));
        self.bytes = self.bytes.saturating_add(cost);
    }
}

type SlateFlight = OnceLock<filmcraft_media::Result<VideoFrame>>;

#[derive(Default)]
struct SharedSlateCache {
    frames: Mutex<SlateFrameCache>,
    render: Mutex<()>,
    flights: Mutex<HashMap<SlateKey, Weak<SlateFlight>>>,
}

fn cached_slate_frame(
    cache: &SharedSlateCache,
    key: SlateKey,
    make: impl FnOnce() -> filmcraft_media::Result<VideoFrame>,
) -> filmcraft_media::Result<VideoFrame> {
    cached_slate_frame_with_admission_hook(cache, key, make, || {})
}

fn cached_slate_frame_with_admission_hook(
    cache: &SharedSlateCache,
    key: SlateKey,
    make: impl FnOnce() -> filmcraft_media::Result<VideoFrame>,
    after_miss: impl FnOnce(),
) -> filmcraft_media::Result<VideoFrame> {
    // Keep cache lookup and flight enrollment under one lock. Otherwise a caller can miss
    // the cache, pause, and start a second render after the first uncached flight ends.
    let flight = {
        let mut flights = cache.flights.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(frame) = cache.frames.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
            return Ok(frame);
        }
        after_miss();
        if let Some(existing) = flights.get(&key).and_then(Weak::upgrade) {
            existing
        } else {
            flights.retain(|_, weak| weak.strong_count() > 0);
            let fresh = Arc::new(SlateFlight::new());
            flights.insert(key.clone(), Arc::downgrade(&fresh));
            fresh
        }
    };
    flight
        .get_or_init(|| {
            // Generate one slate at a time without holding the cache lock needed by hot frames.
            let _render = cache.render.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(frame) = cache.frames.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
                return Ok(frame);
            }
            let frame = std::panic::catch_unwind(std::panic::AssertUnwindSafe(make))
                .map_err(|_| MediaError::Decode("offline slate render failed; see the crash log".into()))??;
            cache.frames.lock().unwrap_or_else(|e| e.into_inner()).insert(key, frame.clone());
            Ok(frame)
        })
        .clone()
}

fn slate_dimensions(width: u32, height: u32, scale: f32) -> (u32, u32) {
    if width == 0 || height == 0 {
        return (2, 2);
    }
    // A placeholder can remain visible even while corrupt source metadata awaits repair.
    let scale = if scale.is_finite() { scale.clamp(1.0 / 64.0, 1.0) } else { 1.0 };
    let (mut w, mut h) = (((width as f64 * f64::from(scale)).round() as u32).max(2), ((height as f64 * f64::from(scale)).round() as u32).max(2));
    let side = w.max(h);
    if side > MAX_OFFLINE_SLATE_SIDE {
        w = (u64::from(w) * u64::from(MAX_OFFLINE_SLATE_SIDE) / u64::from(side)) as u32;
        h = (u64::from(h) * u64::from(MAX_OFFLINE_SLATE_SIDE) / u64::from(side)) as u32;
        w = w.max(2);
        h = h.max(2);
    }
    let pixels = u64::from(w) * u64::from(h);
    if pixels > MAX_OFFLINE_SLATE_PIXELS {
        let shrink = (MAX_OFFLINE_SLATE_PIXELS as f64 / pixels as f64).sqrt();
        w = ((f64::from(w) * shrink).floor() as u32).max(2);
        h = ((f64::from(h) * shrink).floor() as u32).max(2);
        // A final integer bound also handles any floating-point rounding at the budget edge.
        w = w.min((MAX_OFFLINE_SLATE_PIXELS / u64::from(h)) as u32);
    }
    (w, h)
}

impl MediaSource for SlateSource {
    fn info(&self) -> &MediaInfo {
        &self.info
    }
    fn video_frame(&self, req: FrameRequest) -> filmcraft_media::Result<Arc<VideoFrame>> {
        let v = self.info.video.as_ref().ok_or(MediaError::NoStream("video"))?;
        let (w, h) = slate_dimensions(v.width, v.height, req.scale);
        let key = (self.name.clone(), w, h, self.reason.headline());
        let frame = cached_slate_frame(&self.cache, key, || {
            let pixels =
                filmcraft_render::offline::slate_rgba8(w as usize, h as usize, &self.name, self.reason).map_err(|e| MediaError::Unsupported(e.to_string()))?;
            Ok(VideoFrame::rgba8(w, h, pixels))
        })?;
        Ok(Arc::new(frame.with_pts(req.time)))
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
                self.offline.write().unwrap_or_else(|e| e.into_inner()).entry(self.item).or_insert_with(|| {
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
        let mut failures = self.failures.lock().unwrap_or_else(|e| e.into_inner());
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
    use filmcraft_frame::PixelData;
    use filmcraft_project::ItemKind;

    #[test]
    fn offline_slate_bounds_hostile_and_large_valid_dimensions() {
        let mut info = filmcraft_media::generators::GeneratorSource::new(
            filmcraft_media::Generator::BlackVideo,
            16,
            16,
            filmcraft_time::FrameRate::FPS_24,
            filmcraft_time::Tick(1),
        )
        .info()
        .clone();
        let video = info.video.as_mut().unwrap();
        video.width = 1_000_000;
        video.height = 1_000_000;
        let source = SlateSource::new(info, "offline.mov", OfflineReason::MadeOffline);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| source.video_frame(FrameRequest::full(filmcraft_time::Tick::ZERO))));
        assert!(matches!(result, Ok(Ok(frame)) if (frame.width, frame.height) == (960, 960)), "corrupt source metadata must still display a bounded warning");

        for (width, height) in [(15_360, 8_640), (32_768, 8_192), (8_192, 32_768)] {
            let (w, h) = slate_dimensions(width, height, 1.0);
            assert!(u64::from(w) * u64::from(h) <= MAX_OFFLINE_SLATE_PIXELS);
            assert!((f64::from(w) / f64::from(h) - f64::from(width) / f64::from(height)).abs() < 0.01);
        }
        assert_eq!(slate_dimensions(1920, 1080, 1.0), (1280, 720));
        assert_eq!(slate_dimensions(1280, 1280, 1.0), (960, 960));
        assert_eq!(slate_dimensions(1440, 1080, 1.0), (1108, 831));
        assert_eq!(slate_dimensions(32_768, 1, 1.0), (1280, 2));
        assert_eq!(slate_dimensions(32, 32, 1.0 / 64.0), (2, 2));
        assert_eq!(slate_dimensions(0, 0, 1.0), (2, 2));
        assert_eq!(slate_dimensions(0, 0, f32::NAN), (2, 2));
        assert_eq!(slate_dimensions(1920, 1080, f32::NAN), (1280, 720));

        let mut info = source.info().clone();
        let video = info.video.as_mut().unwrap();
        video.width = 15_360;
        video.height = 8_640;
        let source = SlateSource::new(info, "valid-16k.mov", OfflineReason::MadeOffline);
        assert_eq!(source.video_frame(FrameRequest { time: filmcraft_time::Tick::ZERO, scale: f32::NAN }).unwrap().width, 1280);
        assert_eq!(source.video_frame(FrameRequest { time: filmcraft_time::Tick::ZERO, scale: f32::INFINITY }).unwrap().width, 1280);
        let frame = source.video_frame(FrameRequest::full(filmcraft_time::Tick::ZERO)).unwrap();
        assert_eq!((frame.width, frame.height), (1280, 720));
        assert_eq!(u64::from(frame.width) * u64::from(frame.height), MAX_OFFLINE_SLATE_PIXELS);
        assert_eq!(source.info().video.as_ref().map(|v| (v.width, v.height)), Some((15_360, 8_640)));

        for (width, height) in [(32_768, 1), (1, 32_768)] {
            let mut info = source.info().clone();
            let video = info.video.as_mut().unwrap();
            video.width = width;
            video.height = height;
            let slate = SlateSource::new(info, "extreme.mov", OfflineReason::MadeOffline);
            let frame = slate.video_frame(FrameRequest::full(filmcraft_time::Tick::ZERO)).unwrap();
            assert!(frame.width <= MAX_OFFLINE_SLATE_SIDE && frame.height <= MAX_OFFLINE_SLATE_SIDE);
            assert_eq!(frame.to_rgba8().len(), frame.width as usize * frame.height as usize * 4);
        }
    }

    #[test]
    fn hostile_slate_names_share_the_bounded_notice_and_cached_frame() {
        let info = filmcraft_media::generators::GeneratorSource::new(
            filmcraft_media::Generator::BlackVideo,
            64,
            36,
            filmcraft_time::FrameRate::FPS_24,
            filmcraft_time::Tick(1),
        )
        .info()
        .clone();
        let cache = Arc::<SharedSlateCache>::default();
        let first = SlateSource::with_cache(info.clone(), &"x".repeat(10_000), OfflineReason::Missing, cache.clone());
        let second = SlateSource::with_cache(info, "multi\nline.mov", OfflineReason::Missing, cache);
        assert_eq!(first.name, second.name);
        assert!(first.name.len() < 512);
        let a = first.video_frame(FrameRequest::full(filmcraft_time::Tick::ZERO)).unwrap();
        let b = second.video_frame(FrameRequest::full(filmcraft_time::Tick(1))).unwrap();
        assert!(matches!((&a.data, &b.data), (PixelData::Rgba8(x), PixelData::Rgba8(y)) if Arc::ptr_eq(x, y)), "equal notices should reuse the cached pixels");
    }

    #[test]
    fn clearing_a_project_starts_a_fresh_slate_cache() {
        let pool = MediaPool::default();
        let info = filmcraft_media::generators::GeneratorSource::new(
            filmcraft_media::Generator::BlackVideo,
            16,
            8,
            filmcraft_time::FrameRate::FPS_24,
            filmcraft_time::Tick(1),
        )
        .info()
        .clone();
        let previous = pool.slate_cache.read().unwrap_or_else(|e| e.into_inner()).clone();
        for i in 0..MAX_OFFLINE_SLATE_CACHE_ENTRIES {
            pool.slate_source(info.clone(), &format!("old-{i}.mov"), OfflineReason::Missing)
                .video_frame(FrameRequest::full(filmcraft_time::Tick::ZERO))
                .unwrap();
        }
        assert_eq!(previous.frames.lock().unwrap_or_else(|e| e.into_inner()).entries.len(), MAX_OFFLINE_SLATE_CACHE_ENTRIES);

        pool.clear();
        let next = pool.slate_cache.read().unwrap_or_else(|e| e.into_inner()).clone();
        assert!(!Arc::ptr_eq(&previous, &next), "the next project must not inherit a full cache");
        assert!(next.frames.lock().unwrap_or_else(|e| e.into_inner()).entries.is_empty());
        let source = pool.slate_source(info, "new-project.mov", OfflineReason::Missing);
        assert!(Arc::ptr_eq(&source.cache, &next));
        let a = source.video_frame(FrameRequest::full(filmcraft_time::Tick::ZERO)).unwrap();
        let b = source.video_frame(FrameRequest::full(filmcraft_time::Tick(1))).unwrap();
        assert!(
            matches!((&a.data, &b.data), (PixelData::Rgba8(x), PixelData::Rgba8(y)) if Arc::ptr_eq(x, y)),
            "the new project's first slate should be cached"
        );
        assert_eq!(next.frames.lock().unwrap_or_else(|e| e.into_inner()).entries.len(), 1);
    }

    #[test]
    fn offline_slate_scales_and_uses_a_bounded_shared_cache() {
        let info = filmcraft_media::generators::GeneratorSource::new(
            filmcraft_media::Generator::BlackVideo,
            15_360,
            8_640,
            filmcraft_time::FrameRate::FPS_24,
            filmcraft_time::Tick(1),
        )
        .info()
        .clone();
        let source = SlateSource::new(info, "missing.mov", OfflineReason::Missing);
        let frame = source.video_frame(FrameRequest { time: filmcraft_time::Tick::ZERO, scale: 0.075 }).unwrap();
        assert_eq!((frame.width, frame.height), (1152, 648), "valid source metadata must scale the bounded slate");
        let second = source.video_frame(FrameRequest { time: filmcraft_time::Tick(1), scale: 0.075 }).unwrap();
        assert_eq!(second.pts, filmcraft_time::Tick(1));
        assert_eq!((second.width, second.height), (frame.width, frame.height));
        let cache = source.cache.frames.lock().unwrap_or_else(|e| e.into_inner());
        assert!(cache.bytes <= MAX_OFFLINE_SLATE_CACHE_BYTES);
        assert!(cache.entries.len() <= MAX_OFFLINE_SLATE_CACHE_ENTRIES);
    }

    #[test]
    fn slate_cache_evicts_old_frames_by_bytes_and_entry_count() {
        let mut cache = SlateFrameCache::default();
        let make = || VideoFrame::rgba8(1280, 720, vec![7; 1280 * 720 * 4]);
        let first = (Arc::<str>::from("first.mov"), 1280, 720, OfflineReason::Missing.headline());
        let frame = make();
        cache.insert(first.clone(), frame.clone());
        let cached = cache.get(&first).unwrap();
        assert!(matches!((&frame.data, &cached.data), (PixelData::Rgba8(a), PixelData::Rgba8(b)) if Arc::ptr_eq(a, b)));
        for i in 0..15 {
            cache.insert((Arc::<str>::from(format!("{i}.mov")), 1280, 720, OfflineReason::Missing.headline()), make());
        }
        cache.insert((Arc::<str>::from("overflow.mov"), 1280, 720, OfflineReason::Missing.headline()), make());
        assert!(cache.bytes <= MAX_OFFLINE_SLATE_CACHE_BYTES);
        assert!(cache.entries.len() <= MAX_OFFLINE_SLATE_CACHE_ENTRIES);
        assert!(cache.get(&first).is_some(), "a one-entry overflow must not evict the hot set");
        let absent = (Arc::<str>::from("absent.mov"), 1280, 720, OfflineReason::Missing.headline());
        for _ in 0..(MAX_OFFLINE_SLATE_CACHE_IDLE_ACCESSES * 2) {
            assert!(cache.get(&absent).is_none());
        }
        cache.insert((Arc::<str>::from("replacement.mov"), 1280, 720, OfflineReason::Missing.headline()), make());
        assert!(cache.get(&first).is_none(), "inactive slates must eventually be evicted");
    }

    #[test]
    fn slate_cache_counts_float_frames_toward_its_byte_budget() {
        let mut cache = SlateFrameCache::default();
        cache.insert((Arc::<str>::from("float.mov"), 2, 2, OfflineReason::Missing.headline()), VideoFrame::rgba_f32(2, 2, vec![0.0; 16]));
        assert_eq!(cache.bytes, 16 * std::mem::size_of::<f32>());
        cache.insert((Arc::<str>::from("rgba.mov"), 2, 2, OfflineReason::Missing.headline()), VideoFrame::rgba8(2, 2, vec![0; 16]));
        assert_eq!(cache.bytes, 16 * std::mem::size_of::<f32>() + 16, "RGBA8 cache cost is bytes, not pixels");
    }

    #[test]
    fn nine_full_sized_slates_remain_hot_within_the_bounded_cache() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let cache = SharedSlateCache::default();
        let builds = AtomicUsize::new(0);
        for _ in 0..3 {
            for i in 0..9 {
                let key = (Arc::<str>::from(format!("{i}.mov")), 1280, 720, OfflineReason::Missing.headline());
                cached_slate_frame(&cache, key, || {
                    builds.fetch_add(1, Ordering::SeqCst);
                    Ok(VideoFrame::rgba8(1280, 720, vec![7; 1280 * 720 * 4]))
                })
                .unwrap();
            }
        }
        assert_eq!(builds.load(Ordering::SeqCst), 9, "nine active 720p slates should render once each");
        let cache = cache.frames.lock().unwrap();
        assert!(cache.bytes <= MAX_OFFLINE_SLATE_CACHE_BYTES);
        assert_eq!(cache.entries.len(), 9);
    }

    #[test]
    fn slate_cache_does_not_thrash_when_one_more_slate_than_fits_is_rendered_per_frame() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let cache = SharedSlateCache::default();
        let builds = AtomicUsize::new(0);
        for _ in 0..3 {
            for i in 0..17 {
                let key = (Arc::<str>::from(format!("{i}.mov")), 2, 2, OfflineReason::Missing.headline());
                cached_slate_frame(&cache, key, || {
                    builds.fetch_add(1, Ordering::SeqCst);
                    Ok(VideoFrame::rgba8(2, 2, vec![7; 16]))
                })
                .unwrap();
            }
        }
        assert_eq!(builds.load(Ordering::SeqCst), 19, "sixteen hot slates are reused across frames");
        let cache = cache.frames.lock().unwrap();
        assert!(cache.bytes <= MAX_OFFLINE_SLATE_CACHE_BYTES);
        assert!(cache.entries.len() <= MAX_OFFLINE_SLATE_CACHE_ENTRIES);
    }

    #[test]
    fn a_cold_slate_does_not_block_an_unrelated_cached_frame() {
        use std::sync::mpsc::channel;
        use std::time::Duration;

        let cache = Arc::new(SharedSlateCache::default());
        let hot = (Arc::<str>::from("hot.mov"), 2, 2, OfflineReason::Missing.headline());
        cached_slate_frame(&cache, hot.clone(), || Ok(VideoFrame::rgba8(2, 2, vec![7; 16]))).unwrap();
        let (started_tx, started_rx) = channel();
        let (release_tx, release_rx) = channel();
        let cold_cache = cache.clone();
        let cold = std::thread::spawn(move || {
            let key = (Arc::<str>::from("cold.mov"), 2, 2, OfflineReason::Missing.headline());
            cached_slate_frame(&cold_cache, key, || {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(VideoFrame::rgba8(2, 2, vec![8; 16]))
            })
        });
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let (hit_tx, hit_rx) = channel();
        let hot_cache = cache.clone();
        let hit = std::thread::spawn(move || {
            hit_tx.send(cached_slate_frame(&hot_cache, hot, || panic!("cached slate was regenerated")).is_ok()).unwrap();
        });
        let arrived = hit_rx.recv_timeout(Duration::from_secs(2));
        release_tx.send(()).unwrap();
        cold.join().unwrap().unwrap();
        hit.join().unwrap();
        assert!(arrived.unwrap(), "a cache hit waited for an unrelated slate render");
    }

    #[test]
    fn cache_miss_and_flight_enrollment_are_atomic() {
        use std::sync::mpsc::channel;
        use std::time::Duration;

        let cache = Arc::new(SharedSlateCache::default());
        let (entered_tx, entered_rx) = channel();
        let (release_tx, release_rx) = channel();
        let worker_cache = cache.clone();
        let worker = std::thread::spawn(move || {
            let key = (Arc::<str>::from("uncached.mov"), 2, 2, OfflineReason::Missing.headline());
            cached_slate_frame_with_admission_hook(
                &worker_cache,
                key,
                || Ok(VideoFrame::rgba8(2, 2, vec![7; 16])),
                || {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                },
            )
        });
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let admission_is_locked = cache.flights.try_lock().is_err();
        release_tx.send(()).unwrap();
        worker.join().unwrap().unwrap();
        assert!(admission_is_locked, "no caller may observe a cache miss before joining a flight");
    }

    #[test]
    fn concurrent_callers_share_an_overflow_slate_render() {
        use std::sync::Barrier;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::time::Duration;
        use web_time::Instant;

        let cache = Arc::new(SharedSlateCache::default());
        for i in 0..MAX_OFFLINE_SLATE_CACHE_ENTRIES {
            let key = (Arc::<str>::from(format!("hot-{i}.mov")), 2, 2, OfflineReason::Missing.headline());
            cached_slate_frame(&cache, key, || Ok(VideoFrame::rgba8(2, 2, vec![7; 16]))).unwrap();
        }
        let builds = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(AtomicBool::new(false));
        let barrier = Arc::new(Barrier::new(4));
        let key = (Arc::<str>::from("overflow.mov"), 2, 2, OfflineReason::Missing.headline());
        let workers: Vec<_> = (0..4)
            .map(|_| {
                let (cache, builds, release, barrier, key) = (cache.clone(), builds.clone(), release.clone(), barrier.clone(), key.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    cached_slate_frame(&cache, key, || {
                        builds.fetch_add(1, Ordering::SeqCst);
                        while !release.load(Ordering::Acquire) {
                            std::thread::yield_now();
                        }
                        Ok(VideoFrame::rgba8(2, 2, vec![8; 16]))
                    })
                })
            })
            .collect();
        let deadline = Instant::now() + Duration::from_secs(2);
        let all_waiting = loop {
            let count = cache.flights.lock().unwrap().get(&key).map_or(0, Weak::strong_count);
            if count == 4 {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            std::thread::yield_now();
        };
        release.store(true, Ordering::Release);
        let frames: Vec<_> = workers.into_iter().map(|worker| worker.join().unwrap().unwrap()).collect();
        assert!(all_waiting, "all four callers must overlap on the uncached slate");
        assert_eq!(builds.load(Ordering::SeqCst), 1, "overlapping callers must share one rejected render");
        assert!(frames.iter().skip(1).all(|frame| matches!((&frames[0].data, &frame.data), (PixelData::Rgba8(a), PixelData::Rgba8(b)) if Arc::ptr_eq(a, b))));
        assert_eq!(cache.frames.lock().unwrap().entries.len(), MAX_OFFLINE_SLATE_CACHE_ENTRIES);
    }

    #[test]
    fn failed_slate_flight_returns_an_error_and_can_retry() {
        let cache = SharedSlateCache::default();
        let key = (Arc::<str>::from("retry.mov"), 2, 2, OfflineReason::Missing.headline());
        let failed =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cached_slate_frame(&cache, key.clone(), || panic!("synthetic slate generation failure"))));
        assert!(matches!(failed, Ok(Err(MediaError::Decode(message))) if message.contains("offline slate render failed")));
        let recovered = cached_slate_frame(&cache, key, || Ok(VideoFrame::rgba8(2, 2, vec![7; 16]))).unwrap();
        assert_eq!(recovered.to_rgba8(), vec![7; 16]);
    }

    #[test]
    fn offline_slate_serializes_concurrent_misses() {
        use std::sync::Barrier;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let info = filmcraft_media::generators::GeneratorSource::new(
            filmcraft_media::Generator::BlackVideo,
            1920,
            1080,
            filmcraft_time::FrameRate::FPS_24,
            filmcraft_time::Tick(1),
        )
        .info()
        .clone();
        let source = SlateSource::new(info, "distinct.mov", OfflineReason::Missing);
        let cache = Arc::new(SharedSlateCache::default());
        let builds = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(4));
        let workers: Vec<_> = (0..4)
            .map(|_| {
                let (cache, builds, barrier) = (cache.clone(), builds.clone(), barrier.clone());
                let key = (source.name.clone(), 2, 2, OfflineReason::Missing.headline());
                std::thread::spawn(move || {
                    barrier.wait();
                    cached_slate_frame(&cache, key, || {
                        builds.fetch_add(1, Ordering::SeqCst);
                        Ok(VideoFrame::rgba8(2, 2, vec![7; 16]))
                    })
                })
            })
            .collect();
        let mut frames = Vec::new();
        for worker in workers {
            frames.push(worker.join().unwrap().unwrap());
        }
        assert_eq!(builds.load(Ordering::SeqCst), 1, "only one worker generates the slate");
        let first = &frames[0].data;
        assert!(frames.iter().skip(1).all(|frame| matches!((first, &frame.data), (PixelData::Rgba8(a), PixelData::Rgba8(b)) if Arc::ptr_eq(a, b))));
    }

    #[test]
    fn render_item_decodes_an_unknown_size_slate() {
        use filmcraft_project::{Label, MediaClip, MediaRef};

        let mut info = filmcraft_media::generators::GeneratorSource::new(
            filmcraft_media::Generator::BlackVideo,
            2,
            2,
            filmcraft_time::FrameRate::FPS_24,
            filmcraft_time::Tick(1),
        )
        .info()
        .clone();
        let video = info.video.as_mut().unwrap();
        video.width = 0;
        video.height = 0;
        let mut project = Project::new("Unknown size");
        let item = project.add_item(
            "missing.mov",
            Label::Iris,
            ItemKind::Media(MediaClip {
                media: MediaRef::File { path: "missing.mov".into() },
                info: info.clone(),
                interpret: Default::default(),
                mark_in: None,
                mark_out: None,
                markers: vec![],
                offline: true,
                proxy: None,
                identity: None,
            }),
            None,
        );
        let mut sources = filmcraft_render::SourceMap::default();
        sources.0.insert(item, Arc::new(SlateSource::new(info, "missing.mov", OfflineReason::Missing)));
        let frame = filmcraft_render::render_item(&project, item, filmcraft_time::Tick::ZERO, 1.0, &sources).unwrap();
        assert_eq!(project.source_size(item), Some((0, 0)));
        assert_eq!((frame.w, frame.h), (1, 1));
        assert!(frame.get(0, 0)[3] > 0.9, "the renderer retains a visible offline pixel");
    }

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
        assert_eq!(f.to_rgba8(), filmcraft_render::offline::slate_rgba8(16, 8, "a.mov", OfflineReason::Unreadable).unwrap());
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
        r.as_ref().is_ok_and(|f| f.to_rgba8() == filmcraft_render::offline::slate_rgba8(16, 8, name, OfflineReason::Unreadable).unwrap())
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
        assert_eq!(f.to_rgba8(), filmcraft_render::offline::slate_rgba8(64, 32, "h264_444_64x32.mp4", OfflineReason::Unreadable).unwrap());
        let st = s.media.offline_status(item).unwrap();
        assert_eq!(st.reason, OfflineReason::Unreadable);
        assert!(st.error.contains("H.264") && st.error.contains("chroma_format_idc 3"), "{}", st.error);
    }
}
