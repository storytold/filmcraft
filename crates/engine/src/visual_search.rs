//! Local visual search: cancellable jobs, bounded source sampling and revisioned disk caches.
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use crate::commands::{CommandSpec, always, bad};
use crate::{EngineError, Result, Session};
use filmcraft_project::{ItemId, ItemKind, MediaClip, MediaRef, Project};
use filmcraft_time::{TICKS_PER_SECOND, Tick};
use filmcraft_vision::{Embedder, MODEL_ID};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const MAX_FRAMES: usize = 5000;
const MAX_PER_SOURCE: usize = 1000;
const CACHE_BYTES: u64 = 16 * 1024 * 1024;
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
fn err(e: impl std::fmt::Display) -> EngineError {
    EngineError::Other(e.to_string())
}

#[derive(Clone, Serialize, Deserialize)]
struct Frame {
    start: Tick,
    end: Tick,
    vector: Vec<f32>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Indexed {
    key: String,
    interval: u64,
    complete: bool,
    frames: Vec<Frame>,
}
#[derive(Clone, Serialize)]
pub struct Hit {
    pub item: ItemId,
    pub name: String,
    pub start: Tick,
    pub end: Tick,
    pub score: f32,
    #[serde(skip)]
    pub thumbnail: Option<filmcraft_frame::VideoFrame>,
}
#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    pub query: String,
    pub matches: Vec<Hit>,
    pub indexed_sources: usize,
    pub stale_sources: usize,
    pub incomplete: bool,
}
#[derive(Default)]
struct Store {
    result_project: std::sync::Weak<Project>,
    sources: std::collections::BTreeMap<ItemId, Indexed>,
    result: Option<Arc<SearchResult>>,
    warnings: Vec<String>,
    incomplete: bool,
}

pub struct Service {
    owner: bool,
    active: Arc<Mutex<Option<std::sync::Weak<filmcraft_export::Progress>>>>,
    store: Arc<Mutex<Store>>,
    busy: Arc<AtomicBool>,
    playback_busy: Arc<AtomicBool>,
    model: Arc<Mutex<Option<Arc<dyn Embedder>>>>,
    /// Optional override for embedding providers/tests; no downloaded model needed for synthetic tests.
    pub embedder: Option<Arc<dyn Embedder>>,
    pub cache_dir: Option<PathBuf>,
    pub model_dir: Option<PathBuf>,
}
impl Default for Service {
    fn default() -> Self {
        Self {
            owner: true,
            active: Default::default(),
            store: Default::default(),
            busy: Default::default(),
            playback_busy: Default::default(),
            model: Default::default(),
            embedder: None,
            cache_dir: None,
            model_dir: None,
        }
    }
}
impl Clone for Service {
    fn clone(&self) -> Self {
        Self {
            owner: false,
            active: self.active.clone(),
            store: self.store.clone(),
            busy: self.busy.clone(),
            playback_busy: self.playback_busy.clone(),
            model: self.model.clone(),
            embedder: self.embedder.clone(),
            cache_dir: self.cache_dir.clone(),
            model_dir: self.model_dir.clone(),
        }
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        if self.owner {
            self.cancel();
        }
    }
}
impl Service {
    pub fn cancel(&self) {
        if let Some(progress) = lock(&self.active).as_ref().and_then(std::sync::Weak::upgrade) {
            progress.cancel.store(true, Ordering::Relaxed);
        }
    }
    pub fn reset(&self) {
        self.cancel();
        *lock(&self.store) = Store::default();
    }

    pub fn set_playback_busy(&self, busy: bool) {
        self.playback_busy.store(busy, Ordering::Relaxed);
    }
    pub fn result_for(&self, project: &Arc<Project>) -> Option<Arc<SearchResult>> {
        let store = lock(&self.store);
        if store.result_project.ptr_eq(&Arc::downgrade(project)) { store.result.clone() } else { None }
    }
    pub fn result(&self) -> Option<Arc<SearchResult>> {
        lock(&self.store).result.clone()
    }
    fn root(&self) -> std::result::Result<PathBuf, String> {
        self.model_dir.clone().or_else(|| crate::transcript::models_dir().map(|p| p.join(MODEL_ID))).ok_or_else(|| "no model directory".into())
    }
    fn cache(&self) -> Option<PathBuf> {
        self.cache_dir.clone().or_else(|| crate::autosave::default_data_dir().map(|p| p.join("cache/visual-search/v1")))
    }
    fn model(&self, progress: &filmcraft_export::Progress) -> std::result::Result<Arc<dyn Embedder>, String> {
        if let Some(model) = &self.embedder {
            return Ok(model.clone());
        }
        let mut loaded = lock(&self.model);
        if let Some(model) = loaded.as_ref() {
            return Ok(model.clone());
        }
        if !filmcraft_vision::available() {
            return Err("this build has no visual-search feature".into());
        }
        let root = self.root()?;
        for f in crate::visual_model::FILES {
            if progress.cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            let path = root.join(f.name);
            if !std::fs::metadata(&path).is_ok_and(|m| m.len() == f.size) {
                return Err("Download the CLIP model before indexing visuals".into());
            }
            let mut reader = std::fs::File::open(path).map_err(|e| e.to_string())?;
            let mut h = Sha256::new();
            let mut buf = [0_u8; 65536];
            loop {
                let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
                if n == 0 {
                    break;
                }
                h.update(&buf[..n]);
                if progress.cancel.load(Ordering::Relaxed) {
                    return Err("cancelled".into());
                }
            }
            if format!("{:x}", h.finalize()) != f.sha256 {
                return Err(format!("{} failed checksum verification; remove it and download again", f.name));
            }
        }
        let model = filmcraft_vision::load(&root)?;
        *loaded = Some(model.clone());
        Ok(model)
    }
}

fn key(media: &MediaClip, interval: u64) -> std::result::Result<String, String> {
    let mut h = Sha256::new();
    h.update(crate::visual_model::MODEL_REVISION);
    h.update(b"clip-center-crop-rgb-v1");
    h.update(interval.to_le_bytes());
    h.update(serde_json::to_vec(&(&media.media, &media.identity, &media.info.video, media.duration())).map_err(|e| e.to_string())?);
    if let MediaRef::File { path } = &media.media {
        let m = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?;
        h.update(m.len().to_le_bytes());
        let modified = m.modified().map_err(|e| e.to_string())?.duration_since(std::time::UNIX_EPOCH).map_err(|e| e.to_string())?;
        h.update(modified.as_secs().to_le_bytes());
        h.update(modified.subsec_nanos().to_le_bytes());
    }
    Ok(format!("{:x}", h.finalize()))
}
fn read_cache(path: &Path, key: &str, duration: Tick, interval: u64) -> Option<Indexed> {
    if std::fs::metadata(path).ok()?.len() > CACHE_BYTES {
        return None;
    }
    let mut data = Vec::new();
    std::fs::File::open(path).ok()?.take(CACHE_BYTES + 1).read_to_end(&mut data).ok()?;
    if data.len() as u64 > CACHE_BYTES {
        return None;
    }
    let mut index: Indexed = serde_json::from_slice(&data).ok()?;
    if index.key != key || index.interval != interval || index.frames.len() > MAX_PER_SOURCE {
        return None;
    }
    let mut end = Tick::ZERO;
    for frame in &mut index.frames {
        if frame.start != end || frame.end <= frame.start || frame.end > duration {
            return None;
        }
        frame.vector = filmcraft_vision::normalize(std::mem::take(&mut frame.vector)).ok()?;
        end = frame.end;
    }
    if index.complete && end != duration {
        return None;
    }
    Some(index)
}
// Cache directory metadata is untrusted, including oversized/sparse file lengths.
fn cache_bytes(lengths: impl Iterator<Item = u64>) -> u128 {
    lengths.fold(0_u128, |total, bytes| total.saturating_add(u128::from(bytes)))
}
fn write_cache(root: &Path, index: &Indexed) -> std::result::Result<(), String> {
    static SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let path = root.join(format!("{}.json", index.key));
    let part = root.join(format!("{}.{}.{}.part", index.key, std::process::id(), SERIAL.fetch_add(1, Ordering::Relaxed)));
    let data = serde_json::to_vec(index).map_err(|e| e.to_string())?;
    if data.len() as u64 > CACHE_BYTES {
        return Err("visual cache is too large".into());
    }
    let result = std::fs::write(&part, data).and_then(|_| std::fs::rename(&part, path));
    if result.is_err() {
        let _ = std::fs::remove_file(part);
    }
    result.map_err(|e| e.to_string())?;
    // Only this feature's hexadecimal cache files are eligible for eviction.
    let mut files = Vec::new();
    for entry in std::fs::read_dir(root).map_err(|e| e.to_string())?.take(10_000).flatten() {
        let path = entry.path();
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
        if path.extension().is_some_and(|s| s == "json")
            && stem.len() == 64
            && stem.bytes().all(|b| b.is_ascii_hexdigit())
            && let Ok(metadata) = entry.metadata()
        {
            files.push((metadata.modified().ok(), metadata.len(), path));
        }
    }
    files.sort_by_key(|f| f.0);
    let mut size = cache_bytes(files.iter().map(|f| f.1));
    for (_, bytes, path) in files {
        if size <= 256 * 1024 * 1024 {
            break;
        }
        if std::fs::remove_file(path).is_ok() {
            size = size.saturating_sub(u128::from(bytes));
        }
    }
    Ok(())
}

struct Busy(Arc<AtomicBool>);
impl Drop for Busy {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
fn job(
    s: &mut Session,
    p: &Value,
    label: &str,
    run: impl FnOnce(Arc<filmcraft_export::Progress>) -> std::result::Result<(), String> + Send + 'static,
) -> Result<Value> {
    let wait = match p.get("wait") {
        None => false,
        Some(Value::Bool(v)) => *v,
        _ => return Err(bad("visualSearch", "`wait` must be boolean")),
    };
    if s.visual_search.busy.swap(true, Ordering::AcqRel) {
        return Err(bad("visualSearch", "a visual-search job is already running"));
    }
    let busy = Busy(s.visual_search.busy.clone());
    let id = match s.jobs.iter().map(|j| j.id).max().unwrap_or(0).checked_add(1) {
        Some(id) => id,
        None => return Err(bad("visualSearch", "job identifiers exhausted")),
    };
    let job = crate::Job { id, label: label.into(), progress: Default::default(), result: Default::default() };
    *lock(&s.visual_search.active) = Some(Arc::downgrade(&job.progress));
    let (progress, result) = (job.progress.clone(), job.result.clone());
    let task = crate::export_tools::guard_job(progress.clone(), result.clone(), move || {
        let _busy = busy;
        let start = web_time::Instant::now();
        let answer = run(progress.clone());
        if let Err(e) = &answer {
            *lock(&progress.error) = Some(e.clone());
            *lock(&progress.status) = e.clone();
        }
        progress.finished.store(true, Ordering::Relaxed);
        *lock(&result) = Some(answer.map(|_| filmcraft_export::Report {
            path: String::new(),
            frames: 0,
            seconds: start.elapsed().as_secs_f64(),
            bytes: 0,
            render_fps: 0.0,
            extra_files: Vec::new(),
        }));
    });
    s.jobs.push(job.clone());
    if wait || cfg!(target_arch = "wasm32") {
        task();
        if let Some(Err(e)) = &*lock(&job.result) {
            return Err(err(e));
        }
    } else if let Err(e) = std::thread::Builder::new().name("visual-search".into()).spawn(task) {
        *lock(&job.result) = Some(Err(e.to_string()));
        job.progress.finished.store(true, Ordering::Relaxed);
        return Err(err(e));
    }
    Ok(json!({"job":id}))
}
fn integer(p: &Value, k: &str, default: u64, lo: u64, hi: u64) -> Result<u64> {
    match p.get(k) {
        None => Ok(default),
        Some(v) => v.as_u64().filter(|n| *n >= lo && *n <= hi).ok_or_else(|| bad("visualSearch", format!("`{k}` must be an integer between {lo} and {hi}"))),
    }
}
fn status(progress: &filmcraft_export::Progress, text: &str) {
    *lock(&progress.status) = text.into();
}
fn cancelled(progress: &filmcraft_export::Progress) -> std::result::Result<(), String> {
    if progress.cancel.load(Ordering::Relaxed) { Err("cancelled".into()) } else { Ok(()) }
}

// Analysis owns decoder caches, so it neither thrashes playback caches nor retains stale file
// handles after a file is replaced at the same path. Preserve the host's opener registry.
fn analysis_pool(s: &Session) -> Arc<crate::media_pool::MediaPool> {
    let pool = Arc::new(crate::media_pool::MediaPool::default());
    *pool.openers.write().unwrap_or_else(std::sync::PoisonError::into_inner) =
        s.media.openers.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    pool
}

fn index(s: &mut Session, p: &Value) -> Result<Value> {
    let interval = integer(p, "intervalSeconds", 2, 2, 60)?;
    let max = integer(p, "maxFrames", 5000, 1, MAX_FRAMES as u64)? as usize;
    let items = match p.get("items") {
        None => None,
        Some(Value::Array(a)) if a.len() <= 1000 => {
            Some(a.iter().map(|v| v.as_u64().map(ItemId).ok_or_else(|| bad("visualSearch.index", "items must be source ids"))).collect::<Result<Vec<_>>>()?)
        }
        _ => return Err(bad("visualSearch.index", "items must be an array of at most 1000 source ids")),
    };
    if let Some(ids) = &items {
        for id in ids {
            if !matches!(s.project.item(*id).map(|i|&i.kind),Some(ItemKind::Media(m)) if m.info.video.is_some() && !m.offline) {
                return Err(bad("visualSearch.index", "each item must be an online video source"));
            }
        }
    }
    let targets: Vec<ItemId> = match items {
        Some(ids) => ids.into_iter().collect::<std::collections::BTreeSet<_>>().into_iter().collect(),
        None => s.project.items.keys().take(1000).copied().collect(),
    };
    let (project, pool, services, service) = (s.project.clone(), analysis_pool(s), s.services.clone(), s.visual_search.clone());
    job(s, p, "Indexing visuals", move |progress| {
        status(&progress, "Loading local visual model");
        let model = service.model(&progress)?;
        let mut sources = std::collections::BTreeMap::new();
        let mut warnings = Vec::new();
        let mut count = 0usize;
        let step = Tick(TICKS_PER_SECOND.saturating_mul(interval as i64));
        let planned = targets
            .iter()
            .filter_map(|id| project.item(*id))
            .filter_map(|i| i.as_media())
            .filter(|m| !m.offline && m.info.video.is_some() && m.duration().0 > 0)
            .map(|m| (m.duration().0 as u64).div_ceil(step.0 as u64).min(MAX_PER_SOURCE as u64))
            .sum::<u64>()
            .min(max as u64);
        progress.total.store(planned.max(1), Ordering::Relaxed);
        for item in targets.iter().filter_map(|id| project.item(*id)) {
            cancelled(&progress)?;
            let ItemKind::Media(media) = &item.kind else { continue };
            if media.offline || media.info.video.is_none() || media.duration().0 <= 0 {
                continue;
            }
            let source_key = match key(media, interval) {
                Ok(k) => k,
                Err(e) => {
                    warnings.push(e);
                    continue;
                }
            };
            let cached = service.cache().and_then(|root| read_cache(&root.join(format!("{source_key}.json")), &source_key, media.duration(), interval));
            let mut frames = Vec::new();
            let mut time = Tick::ZERO;
            if let Some(mut cached) = cached {
                if cached.complete || cached.frames.len() >= max.saturating_sub(count) || cached.frames.len() >= MAX_PER_SOURCE {
                    if cached.frames.len() > max.saturating_sub(count) {
                        cached.frames.truncate(max.saturating_sub(count));
                        cached.complete = false;
                    }
                    count += cached.frames.len();
                    sources.insert(item.id, cached);
                    progress.done.store(count as u64, Ordering::Relaxed);
                    if count >= max {
                        break;
                    }
                    continue;
                }
                frames = cached.frames;
                time = frames.last().map(|f| f.end).unwrap_or_default();
                count += frames.len();
            }
            let Some(source) = pool.full_res_source(&project, item.id, &*services) else {
                warnings.push(format!("{}: source unavailable", item.name));
                continue;
            };
            while time < media.duration() && frames.len() < MAX_PER_SOURCE && count < max {
                cancelled(&progress)?;
                while service.playback_busy.load(Ordering::Relaxed) {
                    status(&progress, "Visual indexing paused during playback");
                    cancelled(&progress)?;
                    std::thread::sleep(std::time::Duration::from_millis(30));
                }
                status(&progress, &format!("Indexing {}", item.name));
                let scale = media.info.video.as_ref().map(|v| 256.0 / v.width.min(v.height).max(1) as f32).unwrap_or(1.0).min(1.0);
                let frame = match source.video_frame(filmcraft_media::FrameRequest { time, scale }) {
                    Ok(f) => f,
                    Err(e) => {
                        warnings.push(format!("{}: {e}", item.name));
                        break;
                    }
                };
                if u64::from(frame.width) * u64::from(frame.height) > 16_777_216 {
                    warnings.push(format!("{}: frame exceeds indexing size limit", item.name));
                    break;
                }
                let vector = filmcraft_vision::normalize(model.image(frame.width, frame.height, &frame.to_rgba8())?)?;
                let end = Tick(time.0.saturating_add(step.0)).min(media.duration());
                frames.push(Frame { start: time, end, vector });
                time = end;
                count += 1;
                progress.done.store(count as u64, Ordering::Relaxed);
                // Yield between samples; indexing never shares the UI or audio thread.
                std::thread::yield_now();
            }
            let indexed = Indexed { key: source_key, interval, complete: time >= media.duration(), frames };
            if let Some(root) = service.cache()
                && let Err(e) = write_cache(&root, &indexed)
            {
                warnings.push(format!("Cache: {e}"));
            }
            sources.insert(item.id, indexed);
            pool.clear();
            if count >= max {
                break;
            }
        }
        cancelled(&progress)?;
        let mut store = lock(&service.store);
        cancelled(&progress)?;
        store.incomplete = count >= max || project.items.len() > 1000 || sources.values().any(|s| !s.complete);
        store.sources = sources;
        store.result = None;
        store.warnings = warnings;
        progress.total.store(count.max(1) as u64, Ordering::Relaxed);
        progress.done.store(count.max(1) as u64, Ordering::Relaxed);
        status(&progress, &format!("Indexed {count} visual samples"));
        Ok(())
    })
}

fn search(s: &mut Session, p: &Value) -> Result<Value> {
    let query = p
        .get("query")
        .and_then(Value::as_str)
        .filter(|q| !q.trim().is_empty() && q.len() <= 1024)
        .ok_or_else(|| bad("visualSearch.search", "query must contain 1–1024 bytes"))?
        .to_owned();
    let limit = integer(p, "limit", 20, 1, 100)? as usize;
    let (project, pool, services, service) = (s.project.clone(), analysis_pool(s), s.services.clone(), s.visual_search.clone());
    job(s, p, "Searching visuals", move |progress| {
        cancelled(&progress)?;
        status(&progress, "Searching local visual index");
        let model = service.model(&progress)?;
        let query_vector = filmcraft_vision::normalize(model.text(&query)?)?;
        let sources = lock(&service.store).sources.clone();
        if sources.is_empty() {
            return Err("Index project visuals first".into());
        }
        let mut result = SearchResult { query, incomplete: lock(&service.store).incomplete, ..Default::default() };
        for (id, source) in sources {
            cancelled(&progress)?;
            let Some(item) = project.item(id) else {
                result.stale_sources += 1;
                continue;
            };
            let ItemKind::Media(media) = &item.kind else {
                result.stale_sources += 1;
                continue;
            };
            if media.offline || key(media, source.interval).ok().as_deref() != Some(&source.key) {
                result.stale_sources += 1;
                continue;
            }
            result.indexed_sources += 1;
            result.incomplete |= !source.complete;
            for frame in source.frames {
                let score = filmcraft_vision::similarity(&frame.vector, &query_vector)?;
                result.matches.push(Hit { item: id, name: item.name.chars().take(256).collect(), start: frame.start, end: frame.end, score, thumbnail: None });
            }
        }
        result.matches.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.item.cmp(&b.item)).then_with(|| a.start.cmp(&b.start)));
        result.matches.truncate(limit);
        for hit in result.matches.iter_mut().take(20) {
            cancelled(&progress)?;
            if let Some(src) = pool.full_res_source(&project, hit.item, &*services)
                && let Ok(frame) = src.video_frame(filmcraft_media::FrameRequest { time: hit.start, scale: 0.05 })
                && u64::from(frame.width) * u64::from(frame.height) <= 16_777_216
            {
                let stride = frame.width.max(frame.height).div_ceil(128).max(1) as usize;
                let (w, h, linear) = frame.to_linear_f32_decimated(stride);
                let mut rgba = vec![0; linear.len()];
                filmcraft_frame::linear_premul_to_srgb8(&linear, &mut rgba);
                hit.thumbnail = Some(filmcraft_frame::VideoFrame::rgba8(w as u32, h as u32, rgba));
            }
            pool.clear();
        }
        cancelled(&progress)?;
        let mut store = lock(&service.store);
        cancelled(&progress)?;
        store.result_project = Arc::downgrade(&project);
        store.result = Some(Arc::new(result));
        status(&progress, "Visual search complete");
        Ok(())
    })
}
fn download(s: &mut Session, p: &Value) -> Result<Value> {
    if !cfg!(feature = "visual-search") {
        return Err(bad("visualSearch.downloadModel", "this build has no visual-search feature"));
    }
    let service = s.visual_search.clone();
    job(s, p, "Downloading visual model", move |progress| {
        #[cfg(feature = "visual-search")]
        {
            filmcraft_speech::models::download_files(&service.root()?, crate::visual_model::FILES, &mut |done, total, _file| {
                progress.done.store(done, Ordering::Relaxed);
                progress.total.store(total, Ordering::Relaxed);
                status(&progress, "Downloading OpenCLIP model");
                !progress.cancel.load(Ordering::Relaxed)
            })
            .map_err(|e| e.to_string())?;
        }
        #[cfg(not(feature = "visual-search"))]
        {
            let _ = service;
            let _ = progress;
            Err("this build has no visual-search feature".into())
        }
        #[cfg(feature = "visual-search")]
        {
            Ok(())
        }
    })
}
fn inspect(s: &mut Session, _: &Value) -> Result<Value> {
    let service = &s.visual_search;
    let store = lock(&service.store);
    let installed =
        service.root().is_ok_and(|root| crate::visual_model::FILES.iter().all(|f| std::fs::metadata(root.join(f.name)).is_ok_and(|m| m.len() == f.size)));
    Ok(
        json!({"available":filmcraft_vision::available()||service.embedder.is_some(),"installed":installed||service.embedder.is_some(),"busy":service.busy.load(Ordering::Acquire),"model":MODEL_ID,
        "downloadBytes":crate::visual_model::FILES.iter().map(|f|f.size).sum::<u64>(),"license":"MIT","licenseUrl":"https://huggingface.co/laion/CLIP-ViT-B-32-laion2B-s34B-b79K/blob/1a25a446712ba5ee05982a381eed697ef9b435cf/README.md",
        "indexedSources":store.sources.len(),"samples":store.sources.values().map(|i|i.frames.len()).sum::<usize>(),"warnings":store.warnings,"result":if store.result_project.ptr_eq(&Arc::downgrade(&s.project)){store.result.clone()}else{None}}),
    )
}
fn open(s: &mut Session, p: &Value) -> Result<Value> {
    let id = integer(p, "result", u64::MAX, 0, 99)? as usize;
    let result = s.visual_search.result_for(&s.project).ok_or_else(|| bad("visualSearch.openResult", "no search results"))?;
    let hit = result.matches.get(id).ok_or_else(|| bad("visualSearch.openResult", "no such result"))?;
    let indexed = lock(&s.visual_search.store).sources.get(&hit.item).cloned().ok_or_else(|| bad("visualSearch.openResult", "index changed"))?;
    let media = s.project.item(hit.item).and_then(|i| i.as_media()).ok_or_else(|| bad("visualSearch.openResult", "source is gone"))?;
    if key(media, indexed.interval).ok().as_deref() != Some(&indexed.key) {
        return Err(bad("visualSearch.openResult", "source changed; reindex visuals"));
    }
    s.media.remove(hit.item);
    s.execute("source.open", json!({"item":hit.item.0}))?;
    s.execute("source.setPlayhead", json!({"time":hit.start.0}))?;
    Ok(json!({"item":hit.item.0,"start":hit.start.0,"end":hit.end.0}))
}
fn can_visual(s: &Session) -> std::result::Result<(), String> {
    if filmcraft_vision::available() || s.visual_search.embedder.is_some() {
        Ok(())
    } else {
        Err("local visual search is unavailable in this build (enable visual-search)".into())
    }
}
fn can_download_visual(_: &Session) -> std::result::Result<(), String> {
    if cfg!(feature = "visual-search") { Ok(()) } else { Err("visual model downloads are unavailable in this build (enable visual-search)".into()) }
}
pub fn commands() -> Vec<CommandSpec> {
    [
        ("visualSearch.inspect", "Inspect Visual Search", "{}", inspect as fn(&mut Session, &Value) -> Result<Value>),
        ("visualSearch.index", "Index Project Visuals", r#"{"items":[id]?,"intervalSeconds":2..60?,"maxFrames":1..5000?,"wait":bool?}"#, index),
        ("visualSearch.search", "Search Project Visuals", r#"{"query":str,"limit":1..100?,"wait":bool?}"#, search),
        ("visualSearch.openResult", "Open Visual Search Result", r#"{"result":0..99}"#, open),
        ("visualSearch.downloadModel", "Download Visual Search Model", r#"{"wait":bool?}"#, download),
    ]
    .into_iter()
    .map(|(id, label, params, run)| CommandSpec {
        id,
        label,
        params,
        run,
        enabled: if id == "visualSearch.inspect" {
            always
        } else if id == "visualSearch.downloadModel" {
            can_download_visual
        } else {
            can_visual
        },
        menu: &[],
        shortcut: None,
        journal: false,
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    #[test]
    fn hostile_cache_lengths_cannot_wrap_below_the_quota() {
        assert_eq!(cache_bytes([u64::MAX - 1, 100].into_iter()), u128::from(u64::MAX) + 99);
    }
    struct Model(Arc<AtomicUsize>);
    impl Embedder for Model {
        fn text(&self, _: &str) -> std::result::Result<Vec<f32>, String> {
            Ok(vec![1.0; 512])
        }
        fn image(&self, _: u32, _: u32, _: &[u8]) -> std::result::Result<Vec<f32>, String> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(vec![1.0; 512])
        }
    }
    fn fixture(name: &str) -> (Session, ItemId, Arc<AtomicUsize>, PathBuf) {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let id = s.active_sequence().unwrap().video_tracks[0].items[0].item;
        let calls = Arc::new(AtomicUsize::new(0));
        s.visual_search.embedder = Some(Arc::new(Model(calls.clone())));
        let dir = std::env::temp_dir().join(format!("filmcraft-visual-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        s.visual_search.cache_dir = Some(dir.clone());
        (s, id, calls, dir)
    }
    #[test]
    fn indexing_reuses_cache_and_search_opens_source_without_document_edits() {
        let (mut s, id, calls, dir) = fixture("cache");
        let project = s.project.clone();
        let p = json!({"items":[id.0],"maxFrames":2,"wait":true});
        s.execute("visualSearch.index", p.clone()).unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        s.execute("visualSearch.index", p).unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        s.execute("visualSearch.search", json!({"query":"sunset","wait":true})).unwrap();
        let result = s.visual_search.result_for(&s.project).unwrap();
        assert_eq!(result.matches.len(), 2);
        assert_eq!(result.matches[0].start, Tick::ZERO);
        assert!(result.incomplete);
        assert!(result.matches[0].thumbnail.is_some());
        s.execute("visualSearch.openResult", json!({"result":1})).unwrap();
        assert_eq!(s.state.source_item, Some(id));
        assert!(Arc::ptr_eq(&project, &s.project));
        s.execute("visualSearch.index", json!({"items":[id.0],"maxFrames":3,"wait":true})).unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 3, "partial cache resumes with one new sample");
        s.execute("visualSearch.search", json!({"query":"sunset","wait":true})).unwrap();
        let resumed = s.visual_search.result_for(&s.project).unwrap();
        assert_eq!(resumed.matches.len(), 3);
        assert_eq!(resumed.matches[2].start, Tick(TICKS_PER_SECOND * 4));
        s.execute("file.openDemoProject", json!({})).unwrap();
        assert!(s.visual_search.result_for(&s.project).is_none());
        assert!(s.execute("visualSearch.openResult", json!({"result":0})).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn cancellation_while_paused_keeps_existing_index_and_returns_an_error() {
        let (mut s, id, _, dir) = fixture("cancel");
        s.execute("visualSearch.index", json!({"items":[id.0],"maxFrames":1,"wait":true})).unwrap();
        let before = lock(&s.visual_search.store).sources.len();
        s.visual_search.set_playback_busy(true);
        // A different interval prevents a cache hit and forces the playback pause path.
        let r = s.execute("visualSearch.index", json!({"items":[id.0],"intervalSeconds":3,"wait":false})).unwrap();
        let job = r["job"].as_u64().unwrap();
        s.execute("jobs.cancel", json!({"job":job})).unwrap();
        for _ in 0..100 {
            if lock(&s.jobs.last().unwrap().result).is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(lock(&s.jobs.last().unwrap().result).as_ref().unwrap().is_err());
        assert_eq!(lock(&s.visual_search.store).sources.len(), before);
        assert!(!s.visual_search.busy.load(Ordering::Acquire));
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn replaced_files_are_read_fresh_without_polluting_playback_decoder_caches() {
        struct Pixels(Arc<Mutex<Vec<[u8; 4]>>>);
        impl Embedder for Pixels {
            fn text(&self, _: &str) -> std::result::Result<Vec<f32>, String> {
                Ok(vec![1.0; 512])
            }
            fn image(&self, _: u32, _: u32, rgba: &[u8]) -> std::result::Result<Vec<f32>, String> {
                lock(&self.0).push(rgba[..4].try_into().unwrap());
                Ok(vec![1.0; 512])
            }
        }
        let dir = std::env::temp_dir().join(format!("filmcraft-visual-replaced-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("source.png");
        image::RgbaImage::from_pixel(4, 4, image::Rgba([255, 0, 0, 255])).save(&path).unwrap();
        let mut s = Session::default();
        s.execute("file.import", json!({"paths":[path]})).unwrap();
        let id = s.project.items.values().find(|i| i.as_media().is_some()).unwrap().id;
        if let ItemKind::Media(media) = &mut Arc::make_mut(&mut s.project).items.get_mut(&id).unwrap().kind {
            media.info.duration = Tick(TICKS_PER_SECOND * 2);
        }
        let old = s.source(id).unwrap();
        assert_eq!(&old.video_frame(filmcraft_media::FrameRequest::full(Tick::ZERO)).unwrap().to_rgba8()[..4], &[255, 0, 0, 255]);
        std::thread::sleep(std::time::Duration::from_millis(2));
        image::RgbaImage::from_pixel(4, 4, image::Rgba([0, 0, 255, 255])).save(&path).unwrap();
        let pixels = Arc::new(Mutex::new(Vec::new()));
        s.visual_search.embedder = Some(Arc::new(Pixels(pixels.clone())));
        s.visual_search.cache_dir = Some(dir.join("cache"));
        let project = s.project.clone();
        s.execute("visualSearch.index", json!({"maxFrames":1,"wait":true})).unwrap();
        assert_eq!(&*lock(&pixels), &[[0, 0, 255, 255]]);
        assert_eq!(s.media.open_sources(), 1);
        s.execute("visualSearch.search", json!({"query":"blue","wait":true})).unwrap();
        s.execute("visualSearch.openResult", json!({"result":0})).unwrap();
        assert_eq!(&s.source(id).unwrap().video_frame(filmcraft_media::FrameRequest::full(Tick::ZERO)).unwrap().to_rgba8()[..4], &[0, 0, 255, 255]);
        assert!(Arc::ptr_eq(&project, &s.project));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn corrupt_cache_and_hostile_parameters_fail_safely() {
        let (mut s, id, _, dir) = fixture("hostile");
        let project = s.project.clone();
        for p in [
            json!({"maxFrames":0}),
            json!({"intervalSeconds":0}),
            json!({"intervalSeconds":2.5}),
            json!({"items":[999999]}),
            json!({"items":"all"}),
            json!({"wait":"yes"}),
        ] {
            assert!(s.execute("visualSearch.index", p).is_err());
        }
        assert!(s.execute("visualSearch.search", json!({"query":""})).is_err());
        assert!(s.execute("visualSearch.search", json!({"query":"x".repeat(1025)})).is_err());
        assert!(Arc::ptr_eq(&project, &s.project));
        let media = s.project.item(id).unwrap().as_media().unwrap().clone();
        let k = key(&media, 2).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{k}.json"));
        std::fs::write(&path, b"{bad}").unwrap();
        assert!(read_cache(&path, &k, media.duration(), 2).is_none());
        s.execute("visualSearch.index", json!({"items":[id.0],"maxFrames":1,"wait":true})).unwrap();
        assert!(read_cache(&path, &k, media.duration(), 2).is_some());
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(all(test, feature = "visual-search"))]
mod real_video {
    use super::*;
    #[test]
    #[ignore = "requires local CLIP weights; validates the built-in demo sunset query"]
    fn demo_sunset_query_finds_the_ocean_without_filename_matching() {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        s.visual_search.model_dir = Some(PathBuf::from(std::env::var("FILMCRAFT_CLIP_DIR").expect("model")));
        let dir = std::env::temp_dir().join(format!("filmcraft-demo-visual-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        s.visual_search.cache_dir = Some(dir.clone());
        let items: Vec<_> = s
            .project
            .items
            .values()
            .filter(|i| matches!(&i.kind,ItemKind::Media(m) if !m.offline && m.info.video.is_some() && matches!(m.media,MediaRef::Generator(filmcraft_media::Generator::Demo(_)))))
            .map(|i| i.id.0)
            .collect();
        let expected = s.active_sequence().unwrap().video_tracks[0].items[0].item;
        s.execute("visualSearch.index", json!({"items":items,"intervalSeconds":60,"wait":true})).unwrap();
        s.execute("visualSearch.search", json!({"query":"sunset over water","wait":true})).unwrap();
        let result = s.visual_search.result_for(&s.project).unwrap();
        for hit in &result.matches {
            eprintln!("demo_sunset_rank name={} score={:.3}", hit.name, hit.score);
        }
        assert_eq!(result.matches[0].item, expected);
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    #[ignore = "requires local CLIP weights and FILMCRAFT_VISUAL_VIDEO"]
    fn real_video_returns_source_ranges_and_reuses_the_index() {
        let path = std::env::var("FILMCRAFT_VISUAL_VIDEO").expect("video fixture");
        let mut s = Session::default();
        s.visual_search.model_dir = Some(PathBuf::from(std::env::var("FILMCRAFT_CLIP_DIR").expect("model")));
        let dir = std::env::temp_dir().join(format!("filmcraft-real-visual-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        s.visual_search.cache_dir = Some(dir.clone());
        s.execute("file.import", json!({"paths":[path]})).unwrap();
        let project = s.project.clone();
        let start = web_time::Instant::now();
        s.execute("visualSearch.index", json!({"wait":true})).unwrap();
        eprintln!("video_index_seconds={:.3}", start.elapsed().as_secs_f64());
        for (query, second) in [
            ("planet Earth seen from space", 0),
            ("Saturn rings", 2),
            ("an astronaut standing on the Moon", 4),
            ("a concrete pit with walls and metal structures", 6),
        ] {
            s.execute("visualSearch.search", json!({"query":query,"wait":true})).unwrap();
            let r = s.visual_search.result_for(&s.project).unwrap();
            eprintln!("query={query:?} top_start_seconds={:.3} score={:.3}", r.matches[0].start.seconds(), r.matches[0].score);
            assert_eq!(r.matches[0].start, Tick(TICKS_PER_SECOND * second));
            s.execute("visualSearch.openResult", json!({"result":0})).unwrap();
        }
        assert!(Arc::ptr_eq(&project, &s.project));
        let start = web_time::Instant::now();
        s.execute("visualSearch.index", json!({"wait":true})).unwrap();
        eprintln!("cached_video_index_seconds={:.3}", start.elapsed().as_secs_f64());
        let _ = std::fs::remove_dir_all(dir);
    }
}
