//! Footage EffectCraft can open: render the sources that are not plain files.
//!
//! The OTIO payload names media by file path. A generator, a title, a nested sequence has none.
//! [`resolve`] finds those clips in the nest, renders each clip's source range (plus handles) to a
//! ProRes 4444 movie with alpha in the link cache, and returns a copy of the project whose clips
//! point at those movies. The OTIO export reads that copy. The real project is not touched.
//!
//! The render runs as an ordinary export job ([`filmcraft_engine::export_tools::spawn_export`]) on
//! its own thread, so the UI thread never waits. [`resolve`] answers [`EditInError::Preparing`]
//! while a job runs. The dialog watches [`state`] and runs `link.editIn` again when it ends. A
//! movie is named by a hash of what it shows ([`Need::key`]), so the second run, and every later
//! one with the same source, is a cache hit.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use filmcraft_engine::Session;
use filmcraft_engine::export_tools::spawn_export;
use filmcraft_engine::project::{ClipId, ItemId, ItemKind, MediaClip, MediaRef, Project, SequenceSettings, TrackItem, TrackKind, effect, graphic};
use filmcraft_engine::time::{FrameRate, Tick, TimeRange};
use filmcraft_export::{ExportSettings, Format, Progress, Report};
use filmcraft_media::{MediaInfo, MediaKind, VideoStreamInfo};
use serde_json::json;

use crate::edit_in::EditInError;

/// Frames of source kept before and after the used range, so EffectCraft can trim outward.
pub const HANDLE_FRAMES: i64 = 12;

/// The cache stops growing here. The oldest movies go first when a render starts.
const CACHE_LIMIT_BYTES: u64 = 20 * 1024 * 1024 * 1024;

/// Changes when the render recipe changes, so old movies are not reused.
const KEY_VERSION: u32 = 1;

/// How long [`resolve`] sleeps between looks when the caller asked to wait.
const WAIT_STEP: Duration = Duration::from_millis(25);

/// Where the movies go and whether to block for them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrepareOptions {
    /// `None` uses [`default_cache_dir`].
    pub cache_dir: Option<PathBuf>,
    /// Block until the render ends. For callers that draw nothing.
    pub wait: bool,
}

/// One clip of the nest whose source has no file.
#[derive(Debug, Clone, PartialEq)]
pub struct Need {
    /// The clip in the nest sequence.
    pub clip: ClipId,
    /// The clip's source item (a subclip stays a subclip, the renderer follows it).
    pub item: ItemId,
    /// Content id of the movie: 32 hex characters.
    pub key: String,
    /// First rendered source time, in the source item's time.
    pub start: Tick,
    /// Rendered length.
    pub len: Tick,
    pub rate: FrameRate,
    pub width: u32,
    pub height: u32,
    /// The clip's own `source_in` and frame hold, to place it in the movie.
    source_in: Tick,
    hold: Option<Tick>,
}

impl Need {
    /// File name of the movie in the cache.
    pub fn file_name(&self) -> String {
        format!("{}.mov", self.key)
    }
}

/// The clips of `nest` that need a rendered movie. Only video clips count: plain files, and
/// sources with no picture (adjustment layers), keep their reference.
pub fn needs(project: &Project, nest: ItemId) -> Vec<Need> {
    let Some(sequence) = project.sequence(nest) else { return Vec::new() };
    let rate = sequence.settings.frame_rate;
    let frame = rate.frame_duration();
    let handle = Tick(frame.0.saturating_mul(HANDLE_FRAMES));
    let mut found = Vec::new();
    for track in sequence.tracks(TrackKind::Video) {
        for clip in &track.items {
            if !needs_render(project, clip.item, 0) {
                continue;
            }
            let Some(source) = project.item(clip.item) else { continue };
            let (mut first, mut last) = (clip.source_in, clip.source_out());
            if let Some(hold) = clip.frame_hold {
                first = first.min(hold);
                last = last.max(hold + frame);
            }
            let first = rate.snap(first);
            let last = rate.tick_of(rate.frame_at(Tick((last.0 - 1).max(first.0))) + 1).max(first + frame);
            let start = rate.snap(Tick((first.0 - handle.0).max(0)));
            let end = Tick((last.0 + handle.0).min(source.duration().0).max(last.0));
            let len = Tick(end.0 - start.0).max(frame);
            let (width, height) = natural_size(project, clip.item, 0).unwrap_or((sequence.settings.width, sequence.settings.height));
            let key = content_key(project, clip, start, len, rate, (width, height));
            found.push(Need { clip: clip.id, item: clip.item, key, start, len, rate, width, height, source_in: clip.source_in, hold: clip.frame_hold });
        }
    }
    found
}

/// A source that FilmCraft draws itself: a generator, a graphic, a nested sequence.
fn needs_render(project: &Project, item: ItemId, depth: u8) -> bool {
    if depth > 8 {
        return false;
    }
    match project.item(item).map(|item| &item.kind) {
        Some(ItemKind::Media(media)) => matches!(media.media, MediaRef::Generator(_)),
        Some(ItemKind::Sequence(_) | ItemKind::Graphic { .. }) => true,
        Some(ItemKind::Subclip { parent, .. }) => needs_render(project, *parent, depth + 1),
        Some(ItemKind::AdjustmentLayer { .. }) | None => false,
    }
}

/// The audio clips of `nest` whose source is a generator. A generator has no file, and a render
/// carries no sound, so EffectCraft could only turn each of them into a grey solid.
fn generator_sound(project: &Project, nest: ItemId) -> Vec<ClipId> {
    let Some(sequence) = project.sequence(nest) else { return Vec::new() };
    let is_generator = |item: ItemId| is_generator(project, item, 0);
    sequence.tracks(TrackKind::Audio).iter().flat_map(|track| &track.items).filter(|clip| is_generator(clip.item)).map(|clip| clip.id).collect()
}

fn is_generator(project: &Project, item: ItemId, depth: u8) -> bool {
    if depth > 8 {
        return false;
    }
    match project.item(item).map(|item| &item.kind) {
        Some(ItemKind::Media(media)) => matches!(media.media, MediaRef::Generator(_)),
        Some(ItemKind::Subclip { parent, .. }) => is_generator(project, *parent, depth + 1),
        _ => false,
    }
}

/// The size the source draws at.
fn natural_size(project: &Project, item: ItemId, depth: u8) -> Option<(u32, u32)> {
    if depth > 8 {
        return None;
    }
    match &project.item(item)?.kind {
        ItemKind::Media(media) => media.info.video.as_ref().map(|video| (video.width, video.height)),
        ItemKind::Sequence(sequence) => Some((sequence.settings.width, sequence.settings.height)),
        ItemKind::Graphic { width, height, .. } => Some((*width, *height)),
        ItemKind::Subclip { parent, .. } => natural_size(project, *parent, depth + 1),
        ItemKind::AdjustmentLayer { .. } => None,
    }
}

/// Hash of everything that decides the pixels of the movie.
fn content_key(project: &Project, clip: &TrackItem, start: Tick, len: Tick, rate: FrameRate, size: (u32, u32)) -> String {
    let mut seen = HashSet::new();
    let mut sources = BTreeMap::new();
    gather(project, clip.item, &mut seen, &mut sources);
    let layers: Vec<_> = clip.effects.iter().filter(|effect| graphic::is_layer(effect)).collect();
    let recipe = json!({
        "v": KEY_VERSION,
        "format": "prores4444-alpha",
        "start": start.0,
        "len": len.0,
        "rate": [rate.num, rate.den],
        "size": [size.0, size.1],
        "graphic": clip.graphic,
        "layers": layers,
        // `start` reads the shared layers and settings of a source graphic when the clip has none of its own.
        "source_graphic": project.source_graphics.get(&clip.item).map(|shared| json!({"layers": shared.layers, "meta": shared.meta})),
        "sources": sources,
    });
    content_id(recipe.to_string().as_bytes())
}

/// The kind of `item` and of every item a nested sequence reaches. Names and labels do not count.
fn gather(project: &Project, item: ItemId, seen: &mut HashSet<ItemId>, out: &mut BTreeMap<u64, serde_json::Value>) {
    if !seen.insert(item) {
        return;
    }
    let Some(found) = project.item(item) else { return };
    out.insert(item.0, serde_json::to_value(&found.kind).unwrap_or_default());
    match &found.kind {
        ItemKind::Sequence(sequence) => {
            for track in sequence.video_tracks.iter().chain(&sequence.audio_tracks) {
                for clip in &track.items {
                    gather(project, clip.item, seen, out);
                }
            }
        }
        ItemKind::Subclip { parent, .. } => gather(project, *parent, seen, out),
        ItemKind::Media(_) | ItemKind::AdjustmentLayer { .. } | ItemKind::Graphic { .. } => {}
    }
}

/// 128-bit FNV-1a as 32 hex characters. It names cache files, so it must not change between
/// builds: `DefaultHasher` makes no such promise.
pub fn content_id(bytes: &[u8]) -> String {
    const OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;
    let mut hash = OFFSET;
    for byte in bytes {
        hash ^= u128::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{hash:032x}")
}

/// `<user cache>/artcraft/link/prepared`. `CRAFT_LINK_CACHE_DIR` replaces the whole path.
pub fn default_cache_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("CRAFT_LINK_CACHE_DIR").filter(|dir| !dir.is_empty()) {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME").filter(|home| !home.is_empty()).map(PathBuf::from);
    let base = if cfg!(target_os = "macos") {
        home.map(|home| home.join("Library/Caches"))
    } else if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CACHE_HOME").filter(|dir| !dir.is_empty()).map(PathBuf::from).or_else(|| home.map(|home| home.join(".cache")))
    };
    base.unwrap_or_else(std::env::temp_dir).join("artcraft").join("link").join("prepared")
}

// ---------------------------------------------------------------------------------------------
// Running renders
// ---------------------------------------------------------------------------------------------

type JobResult = Arc<Mutex<Option<Result<Report, String>>>>;

struct Running {
    key: String,
    part: PathBuf,
    dest: PathBuf,
    cancelled: bool,
    progress: Arc<Progress>,
    result: JobResult,
}

struct Registry {
    running: Vec<Running>,
    /// Why a render failed, by key. The next call for that key reports it once.
    failed: BTreeMap<String, String>,
}

static REGISTRY: Mutex<Registry> = Mutex::new(Registry { running: Vec::new(), failed: BTreeMap::new() });
static PART_COUNTER: AtomicU64 = AtomicU64::new(0);

fn lock() -> std::sync::MutexGuard<'static, Registry> {
    REGISTRY.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the dialog looks at each frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Renders are running. `done` and `total` count frames over all of them.
    Running { done: u64, total: u64 },
    /// Nothing runs. The command can run again.
    Idle,
}

/// Move finished renders into the cache and record failures. Returns the state afterwards.
pub fn state() -> State {
    let mut registry = lock();
    settle(&mut registry);
    let live: Vec<_> = registry.running.iter().filter(|job| !job.cancelled).collect();
    if live.is_empty() {
        return State::Idle;
    }
    let done = live.iter().map(|job| job.progress.done.load(Ordering::Relaxed)).sum();
    let total = live.iter().map(|job| job.progress.total.load(Ordering::Relaxed)).sum();
    State::Running { done, total }
}

/// Stop the renders. Their part files go when the threads end.
pub fn cancel() {
    let mut registry = lock();
    for job in &mut registry.running {
        job.cancelled = true;
        job.progress.cancel.store(true, Ordering::Relaxed);
    }
    settle(&mut registry);
}

fn settle(registry: &mut Registry) {
    let mut index = 0;
    while index < registry.running.len() {
        let finished = registry.running.get(index).and_then(|job| job.result.lock().unwrap_or_else(PoisonError::into_inner).clone());
        let Some(outcome) = finished else {
            index += 1;
            continue;
        };
        let job = registry.running.remove(index);
        if job.cancelled {
            let _ = std::fs::remove_file(&job.part);
            continue;
        }
        let moved = outcome.and_then(|_| {
            let size = std::fs::metadata(&job.part).map(|meta| meta.len()).unwrap_or(0);
            if size == 0 {
                return Err("the render wrote no file".to_string());
            }
            std::fs::rename(&job.part, &job.dest).map_err(|err| format!("cannot store {}: {err}", job.dest.display()))
        });
        if let Err(message) = moved {
            let _ = std::fs::remove_file(&job.part);
            registry.failed.insert(job.key, message);
        }
    }
}

fn is_cached(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.len() > 0)
}

/// Make the project that the OTIO export reads: every clip in `nest` that has no file points at a
/// rendered movie. Returns the session project itself when no clip needs one.
///
/// - Everything is cached: the copy comes back.
/// - A render is running or was just started: [`EditInError::Preparing`]. With `wait` the call blocks instead.
/// - A render failed: [`EditInError::PrepareFailed`] with the reason, once.
pub fn resolve(session: &mut Session, nest: ItemId, options: &PrepareOptions) -> Result<Arc<Project>, EditInError> {
    let project = Arc::clone(&session.project);
    let wanted = needs(&project, nest);
    let silent = generator_sound(&project, nest);
    if wanted.is_empty() && silent.is_empty() {
        return Ok(project);
    }
    let dir = options.cache_dir.clone().unwrap_or_else(default_cache_dir);
    std::fs::create_dir_all(&dir).map_err(|err| EditInError::PrepareFailed(format!("cannot create the link cache {}: {err}", dir.display())))?;
    let mut started = false;
    let mut seen = HashSet::new();
    for need in &wanted {
        if !seen.insert(need.key.clone()) {
            continue;
        }
        let dest = dir.join(need.file_name());
        if is_cached(&dest) {
            touch(&dest);
            continue;
        }
        let mut registry = lock();
        settle(&mut registry);
        if let Some(message) = registry.failed.remove(&need.key) {
            return Err(EditInError::PrepareFailed(message));
        }
        if registry.running.iter().any(|job| job.key == need.key && !job.cancelled) {
            started = true;
            continue;
        }
        drop(registry);
        prune(&dir, CACHE_LIMIT_BYTES);
        start(session, &project, nest, need, &dir)?;
        started = true;
    }
    if started && options.wait {
        wait_for(&wanted, &dir)?;
    } else if started {
        return Err(EditInError::Preparing(serde_json::Value::Null));
    }
    if let Some(message) = wanted.iter().find_map(|need| lock().failed.remove(&need.key)) {
        return Err(EditInError::PrepareFailed(message));
    }
    patched(&project, nest, &wanted, &silent, &dir)
}

fn wait_for(wanted: &[Need], dir: &Path) -> Result<(), EditInError> {
    loop {
        let mut registry = lock();
        settle(&mut registry);
        if let Some(message) = wanted.iter().find_map(|need| registry.failed.remove(&need.key)) {
            return Err(EditInError::PrepareFailed(message));
        }
        let waiting = wanted.iter().any(|need| registry.running.iter().any(|job| job.key == need.key && !job.cancelled));
        drop(registry);
        if !waiting {
            return match wanted.iter().find(|need| !is_cached(&dir.join(need.file_name()))) {
                Some(missing) => Err(EditInError::PrepareFailed(format!("the render of {} was stopped", missing.file_name()))),
                None => Ok(()),
            };
        }
        std::thread::sleep(WAIT_STEP);
    }
}

fn touch(path: &Path) {
    if let Ok(file) = std::fs::OpenOptions::new().append(true).open(path) {
        let _ = file.set_modified(SystemTime::now());
    }
}

/// Delete the oldest movies until the folder fits in `limit` bytes.
fn prune(dir: &Path, limit: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(SystemTime, u64, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "mov"))
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            Some((meta.modified().ok()?, meta.len(), entry.path()))
        })
        .collect();
    let mut total: u64 = files.iter().map(|(_, size, _)| size).sum();
    files.sort();
    for (_, size, path) in files {
        if total <= limit {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

/// Start the export job for one movie.
fn start(session: &mut Session, project: &Arc<Project>, nest: ItemId, need: &Need, dir: &Path) -> Result<(), EditInError> {
    let fail = |text: String| EditInError::PrepareFailed(text);
    let sequence = project.sequence(nest).ok_or_else(|| fail("the nest is gone".to_string()))?;
    let mut scratch = (**project).clone();
    let settings = SequenceSettings { width: need.width, height: need.height, frame_rate: need.rate, par: (1, 1), ..sequence.settings.clone() };
    let source = project.item(need.item).ok_or_else(|| fail("the clip's source is gone".to_string()))?;
    let name = source.name.clone();
    let original = sequence.find_item(need.clip).map(|(_, clip)| clip.clone()).ok_or_else(|| fail("the clip is gone".to_string()))?;
    let scratch_sequence = scratch.new_sequence("Prepared for EffectCraft", settings, 1, 0, None);
    let mut item = scratch
        .make_track_item(need.item, TrackKind::Video, Tick::ZERO, TimeRange::new(need.start, need.len), need.rate)
        .ok_or_else(|| fail("the clip's source is gone".to_string()))?;
    // Raw source: no speed, no motion, no effects. A title keeps its text and shapes, which are the picture.
    item.duration = need.len;
    item.source_in = need.start;
    if original.graphic.is_some() {
        item.graphic = original.graphic.clone();
        item.effects = effect::intrinsic_video();
        item.effects.extend(original.effects.iter().filter(|effect| graphic::is_layer(effect)).cloned());
    }
    let track = scratch
        .sequence_mut(scratch_sequence)
        .and_then(|sequence| sequence.video_tracks.first_mut())
        .ok_or_else(|| fail("the scratch sequence has no video track".to_string()))?;
    track.items.push(item);

    let part = dir.join(format!("{}.{}.{}.part", need.key, std::process::id(), PART_COUNTER.fetch_add(1, Ordering::Relaxed)));
    let export = ExportSettings {
        format: Format::ProRes,
        path: part.to_string_lossy().into_owned(),
        prores_profile: "4444".to_string(),
        alpha: true,
        include_audio: false,
        ..Default::default()
    };
    let job = spawn_export(session, Arc::new(scratch), scratch_sequence, export, format!("Preparing {name} for EffectCraft"), false)
        .map_err(|err| fail(err.to_string()))?;
    let job = session.jobs.iter().find(|candidate| candidate.id == job).ok_or_else(|| fail("the render job was lost".to_string()))?;
    lock().running.push(Running {
        key: need.key.clone(),
        part,
        dest: dir.join(need.file_name()),
        cancelled: false,
        progress: Arc::clone(&job.progress),
        result: Arc::clone(&job.result),
    });
    Ok(())
}

/// A copy of `project` whose clips in `nest` read the cached movies. The audio clips in `silent`
/// are left out of the copy.
fn patched(project: &Arc<Project>, nest: ItemId, wanted: &[Need], silent: &[ClipId], dir: &Path) -> Result<Arc<Project>, EditInError> {
    let mut copy = (**project).clone();
    let mut items: HashMap<&str, ItemId> = HashMap::new();
    for need in wanted {
        let item = match items.get(need.key.as_str()) {
            Some(item) => *item,
            None => {
                let original = project.item(need.item).ok_or_else(|| EditInError::Failed("the clip's source is gone".to_string()))?;
                let path = dir.join(need.file_name()).to_string_lossy().into_owned();
                let id = copy.add_item(&original.name, original.label, ItemKind::Media(Box::new(prepared_media(need, &path, &original.name))), None);
                items.insert(need.key.as_str(), id);
                id
            }
        };
        let sequence = copy.sequence_mut(nest).ok_or_else(|| EditInError::Failed("the nest is gone".to_string()))?;
        let Some((_, clip)) = sequence.find_item_mut(need.clip) else { continue };
        clip.item = item;
        clip.source_in = Tick((need.source_in.0 - need.start.0).max(0));
        clip.frame_hold = need.hold.map(|hold| Tick((hold.0 - need.start.0).max(0)));
        clip.multicam = None;
        clip.graphic = None;
        clip.effects.retain(|effect| !graphic::is_layer(effect));
    }
    if let Some(sequence) = copy.sequence_mut(nest) {
        for track in &mut sequence.audio_tracks {
            track.items.retain(|clip| !silent.contains(&clip.id));
        }
    }
    Ok(Arc::new(copy))
}

fn prepared_media(need: &Need, path: &str, name: &str) -> MediaClip {
    MediaClip {
        media: MediaRef::File { path: path.to_string() },
        info: MediaInfo {
            name: name.to_string(),
            kind: MediaKind::Movie,
            duration: need.len,
            video: Some(VideoStreamInfo {
                width: need.width,
                height: need.height,
                frame_rate: need.rate,
                par: (1, 1),
                codec: "Apple ProRes 4444".to_string(),
                pixel_format: "yuva444p12".to_string(),
                color: Default::default(),
                color_assumed: None,
                has_alpha: true,
                bitrate: None,
                hdr: None,
            }),
            audio_streams: Vec::new(),
            container: "QuickTime".to_string(),
            start_timecode: None,
            file_size: None,
        },
        interpret: Default::default(),
        mark_in: None,
        mark_out: None,
        markers: Vec::new(),
        offline: false,
        proxy: None,
        identity: None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use filmcraft_media::Generator;

    use super::*;
    use crate::fixtures::generator_nest;

    #[test]
    fn content_id_is_stable_and_separates_inputs() {
        // Fixed vectors: a cache file written by one build must be found by the next.
        assert_eq!(content_id(b""), "6c62272e07bb014262b821756295c58d");
        assert_eq!(content_id(b"a").len(), 32);
        assert_ne!(content_id(b"a"), content_id(b"b"));
    }

    #[test]
    fn a_generator_clip_needs_a_render_with_handles_and_the_clip_offset() {
        let (session, seq, clip) = generator_nest(64, 36, 48);
        let found = needs(&session.project, seq);
        assert_eq!(found.len(), 1);
        let need = &found[0];
        assert_eq!(need.clip, clip);
        let rate = FrameRate::FPS_24;
        // The clip uses frames 6..16. Twelve frames of handle reach back to the start.
        assert_eq!(need.start, Tick::ZERO);
        assert_eq!(need.len, rate.tick_of(28));
        assert_eq!((need.width, need.height), (64, 36));
        assert_eq!(need.key.len(), 32);
    }

    #[test]
    fn the_key_follows_the_pixels_not_the_name() {
        let (mut session, seq, _) = generator_nest(64, 36, 48);
        let before = needs(&session.project, seq)[0].key.clone();
        let item = session.project.sequence(seq).unwrap().video_tracks[0].items[0].item;
        session.project_mut().item_mut(item).unwrap().name = "renamed".to_string();
        assert_eq!(needs(&session.project, seq)[0].key, before, "a rename keeps the movie");
        if let Some(ItemKind::Media(media)) = session.project_mut().item_mut(item).map(|item| &mut item.kind) {
            media.media = MediaRef::Generator(Generator::BarsAndTone);
        }
        assert_ne!(needs(&session.project, seq)[0].key, before, "another source needs another movie");
    }

    #[test]
    fn the_key_follows_the_shared_layers_of_a_source_graphic() {
        use filmcraft_engine::project::{SourceGraphic, Vec2};

        let (mut session, seq, _) = generator_nest(64, 36, 48);
        let item = session.project.sequence(seq).unwrap().video_tracks[0].items[0].item;
        let text = |words: &str| SourceGraphic { layers: vec![graphic::new_text_layer(words, Vec2::default(), 40.0)], meta: None };
        session.project_mut().source_graphics.insert(item, text("before"));
        let before = needs(&session.project, seq)[0].key.clone();
        // The clip keeps its own effects: only the shared layers changed.
        session.project_mut().source_graphics.insert(item, text("after"));
        assert_ne!(needs(&session.project, seq)[0].key, before, "edited text needs a new movie");
        session.project_mut().source_graphics.insert(item, text("before"));
        assert_eq!(needs(&session.project, seq)[0].key, before, "the same layers find the same movie");
    }

    #[test]
    fn a_file_clip_needs_nothing() {
        let (mut session, seq, _) = generator_nest(64, 36, 48);
        let item = session.project.sequence(seq).unwrap().video_tracks[0].items[0].item;
        if let Some(ItemKind::Media(media)) = session.project_mut().item_mut(item).map(|item| &mut item.kind) {
            media.media = MediaRef::File { path: "/nowhere/clip.mov".to_string() };
        }
        assert!(needs(&session.project, seq).is_empty());
    }
}
