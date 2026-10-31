//! Auto-save ring + crash-recovery journal, and the preferences that drive them.
//!
//! **Auto Save** (Premiere behaviour, Preferences ▸ Auto Save): every *interval* minutes, if the
//! project changed since the last auto-save, a copy is written to the `Auto-Save` folder next to the
//! project as `<name>-YYYY-MM-DD_HH-MM-SS.fcproj`, keeping at most *Maximum Project Versions* files.
//! Optionally the project file itself is saved too.
//!
//! **Recovery journal** (FilmCraft addition): while the project has unsaved changes, a snapshot is
//! written to `<data dir>/Recovery/<session>/snapshot.fcproj` shortly after each change (debounced,
//! and at least every *N* seconds while edits keep coming). Each running app holds an OS lock on
//! `<session>/lock`; when the process dies (crash, `kill -9`, power cut) the lock is released, so on
//! the next launch any session directory whose lock can be taken and that holds a snapshot is a
//! recovery candidate ("Recover unsaved changes from <time>?").
//!
//! **Threading.** The UI thread only hands the worker an `Arc<Project>` (the immutable snapshot the
//! engine already keeps for undo) — a pointer copy and a channel send. Serialization and all file
//! I/O happen on the worker thread, so neither ever stalls the UI.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread::JoinHandle;
use std::time::Duration;

use filmcraft_format::autosave as names;
use filmcraft_project::Project;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use web_time::{Instant, SystemTime, UNIX_EPOCH};

use crate::settings;

// ------------------------------------------------------------------ preferences

/// Preferences ▸ Auto Save.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AutoSavePrefs {
    /// "Automatically save projects".
    pub enabled: bool,
    /// "Automatically Save Every: N minute(s)".
    pub interval_minutes: u32,
    /// "Maximum Project Versions".
    pub max_versions: u32,
    /// "Auto Save also saves the current project(s)".
    pub save_current_project: bool,
    /// Keep a crash-recovery journal of unsaved changes.
    pub recovery_journal: bool,
    /// Longest time (s) unsaved changes may stay out of the journal while edits keep coming.
    pub recovery_interval_seconds: u32,
    /// Folders chosen in Project Settings ▸ Scratch Disks on this computer. A project file may
    /// name these, or folders inside its own folder; see
    /// [`crate::project_tools::trusted_scratch`].
    pub scratch_folders: Vec<String>,
}

impl Default for AutoSavePrefs {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_minutes: 5,
            max_versions: 20,
            save_current_project: false,
            recovery_journal: true,
            recovery_interval_seconds: 5,
            scratch_folders: Vec::new(),
        }
    }
}

impl AutoSavePrefs {
    fn clamp(&mut self) {
        self.interval_minutes = self.interval_minutes.clamp(1, 1440);
        self.max_versions = self.max_versions.clamp(1, 1000);
        self.recovery_interval_seconds = self.recovery_interval_seconds.clamp(1, 600);
    }
}

/// Settings ▸ Playback.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PlaybackPrefs {
    /// "Preroll: N seconds" (Play Around, trim-mode loop playback).
    pub preroll_seconds: f64,
    /// "Postroll: N seconds".
    pub postroll_seconds: f64,
    /// "Step forward/back many: N frames" (Shift+Right / Shift+Left).
    pub step_many_frames: u32,
    pub pause_encoder_queue: bool,
    pub enable_transmit: bool,
    pub disable_video_in_background: bool,
    /// Draft decoding while playing at 1/2 resolution or lower: H.264 non-reference pictures
    /// skip deblocking (faster; those frames are approximate). Never used for exports, renders or
    /// a paused frame. Off by default.
    pub draft_decode: bool,
    /// "Hardware decoding": `auto` (default: the system's hardware decoder for streams it
    /// supports, e.g. VideoToolbox H.264 / HEVC on macOS, falling back to FilmCraft's own decoder)
    /// or `off` (always FilmCraft's own decoders). Media already open keeps its decoder until it
    /// is reopened.
    pub hardware_decoding: String,
}

impl Default for PlaybackPrefs {
    fn default() -> Self {
        Self {
            preroll_seconds: 3.0,
            postroll_seconds: 2.0,
            step_many_frames: 5,
            pause_encoder_queue: true,
            enable_transmit: false,
            disable_video_in_background: true,
            draft_decode: false,
            hardware_decoding: "auto".into(),
        }
    }
}

/// Settings ▸ Trim.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TrimPrefs {
    /// "Large Trim Offset: N frames" (Trim Forward/Backward Many, the Trim Monitor's ±N buttons).
    pub large_trim_offset: u32,
    /// The audio Large Trim Offset (audio time units).
    pub large_trim_offset_audio: u32,
    /// "Allow Selection tool to choose Roll and Ripple trims without modifier key".
    pub selection_tool_roll_ripple: bool,
    pub tool_changes_trim_type: bool,
    pub shift_overlapping_clips: bool,
    pub ripple_adds_edits: bool,
    /// "Playhead position determines trim monitor loop playback" (else the edit point does).
    pub playhead_determines_loop: bool,
    pub dynamic_ripple_updates: bool,
}

impl Default for TrimPrefs {
    fn default() -> Self {
        Self {
            large_trim_offset: 5,
            large_trim_offset_audio: 100,
            selection_tool_roll_ripple: false,
            tool_changes_trim_type: true,
            shift_overlapping_clips: false,
            ripple_adds_edits: false,
            playhead_determines_loop: false,
            dynamic_ripple_updates: true,
        }
    }
}

/// User preferences (persisted as JSON in the per-user data directory). Keys are dotted camelCase
/// paths: `autoSave.enabled`, `autoSave.intervalMinutes`, `playback.prerollSeconds`, …
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Preferences {
    /// File layout version ([`crate::settings::PREFS_VERSION`]); older files are migrated on load.
    pub version: u32,
    pub general: settings::GeneralPrefs,
    pub appearance: settings::AppearancePrefs,
    pub audio: AudioPrefs,
    pub audio_hardware: settings::AudioHardwarePrefs,
    pub auto_save: AutoSavePrefs,
    pub color: settings::ColorPrefs,
    pub graphics: settings::GraphicsPrefs,
    pub labels: settings::LabelPrefs,
    pub media: MediaPrefs,
    pub media_analysis: settings::MediaAnalysisPrefs,
    pub media_cache: settings::MediaCachePrefs,
    pub memory: settings::MemoryPrefs,
    pub playback: PlaybackPrefs,
    pub plugins: settings::PluginPrefs,
    pub timeline: settings::TimelinePrefs,
    pub trim: TrimPrefs,
    pub essential_sound: EssentialSoundPrefs,
    pub guides: GuidePrefs,
    /// Voice-Over Record Settings (timeline track header ▸ right-click the microphone).
    pub voice_over: crate::voiceover::VoiceOverPrefs,
    /// Project panel: view, List view columns and sort, view presets, Freeform options.
    pub project_panel: crate::project_panel::ProjectPanelPrefs,
    /// Media Browser: Favorites, recent directories, file types, view, columns.
    pub media_browser: crate::media_browser::MediaBrowserPrefs,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            version: settings::PREFS_VERSION,
            general: Default::default(),
            appearance: Default::default(),
            audio: Default::default(),
            audio_hardware: Default::default(),
            auto_save: Default::default(),
            color: Default::default(),
            graphics: Default::default(),
            labels: Default::default(),
            media: Default::default(),
            media_analysis: Default::default(),
            media_cache: Default::default(),
            memory: Default::default(),
            playback: Default::default(),
            plugins: Default::default(),
            timeline: Default::default(),
            trim: Default::default(),
            essential_sound: Default::default(),
            guides: Default::default(),
            voice_over: Default::default(),
            project_panel: Default::default(),
            media_browser: Default::default(),
        }
    }
}

/// View ▸ Guide Templates: guide sets saved from the Program Monitor.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GuidePrefs {
    pub templates: Vec<GuideTemplate>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GuideTemplate {
    pub name: String,
    pub guides: Vec<Guide>,
}

/// A monitor guide: a vertical line at x = `position` or a horizontal line at y = `position`
/// (frame pixels).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Guide {
    pub vertical: bool,
    pub position: f64,
}

/// Settings ▸ Media.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MediaPrefs {
    /// "Indeterminate Media Timebase": the frame rate of stills (`29.97df`, `25`, …).
    pub indeterminate_timebase: String,
    /// "Timecode": `useMediaSource` | `generate`.
    pub timecode: String,
    /// "Frame Count": `startAt0` | `startAt1` | `timecodeConversion`.
    pub frame_count: String,
    /// "Default Media Scaling": `none` | `scaleToFrameSize` | `setToFrameSize`.
    pub default_media_scaling: String,
    pub write_xmp_id: bool,
    pub validate_content_credentials: bool,
    pub write_clip_markers_to_xmp: bool,
    pub enable_xmp_linking: bool,
    pub include_captions_on_import: bool,
    /// "Enable proxies": playback and monitors read attached proxies (the monitors' Toggle
    /// Proxies button). Export always uses full-resolution media.
    pub enable_proxies: bool,
    pub allow_duplicate_media: bool,
    pub create_folder_for_imported_projects: bool,
    pub auto_hide_dependent_clips: bool,
    pub import_image_sequences: bool,
    pub refresh_growing_files: bool,
    pub resume_growing_playback: bool,
    pub growing_refresh_seconds: u32,
    pub hardware_decoding: bool,
    pub prores_hardware_encoding: bool,
}

impl Default for MediaPrefs {
    fn default() -> Self {
        Self {
            indeterminate_timebase: "29.97df".into(),
            timecode: "useMediaSource".into(),
            frame_count: "startAt0".into(),
            default_media_scaling: "none".into(),
            write_xmp_id: false,
            validate_content_credentials: true,
            write_clip_markers_to_xmp: false,
            enable_xmp_linking: false,
            include_captions_on_import: true,
            enable_proxies: false,
            allow_duplicate_media: false,
            create_folder_for_imported_projects: false,
            auto_hide_dependent_clips: false,
            import_image_sequences: false,
            refresh_growing_files: true,
            resume_growing_playback: false,
            growing_refresh_seconds: 60,
            hardware_decoding: true,
            prores_hardware_encoding: true,
        }
    }
}

/// Essential Sound: user presets saved from the panel (built-in presets live in
/// `filmcraft_project::essential`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EssentialSoundPrefs {
    pub user_presets: Vec<UserSoundPreset>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UserSoundPreset {
    pub name: String,
    pub settings: filmcraft_project::EssentialSound,
}

/// Preferences ▸ Audio (the mixer-automation part) and the Track Mixer panel-menu toggle.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AudioPrefs {
    /// "Automatch Time" (s): how long Touch takes to return to the existing automation.
    pub automatch_time: f64,
    /// "Large Volume Adjustment" (dB) for the Increase/Decrease Clip Volume Many commands.
    pub large_volume_adjustment: f64,
    /// Automation Keyframe Optimization: "Linear keyframe thinning".
    pub linear_keyframe_thinning: bool,
    /// "Minimum time interval thinning".
    pub minimum_time_interval_thinning: bool,
    /// "Minimum time" (ms).
    pub minimum_time_ms: u32,
    /// Track Mixer ▸ "Switch to Touch after Write".
    pub switch_to_touch_after_write: bool,
    /// "5.1 Mixdown Type": `front` | `frontRear` | `frontLfe` | `frontRearLfe`.
    pub mixdown_type: String,
    pub sum_to_mono_in_source: bool,
    pub scrub_audio: bool,
    pub maintain_pitch_shuttling: bool,
    pub mute_input_during_recording: bool,
    pub generate_waveforms_on_import: bool,
    pub multithreaded_waveforms: bool,
    pub render_audio_with_video: bool,
    pub auto_tag_audio_types: bool,
    pub always_override_audio_tags: bool,
    /// "Render Edit in Audition files to": `scratch` | `nextToMedia`.
    pub edit_in_audition_location: String,
    /// Essential Sound Loudness Auto-Match targets (integrated LUFS) per audio type.
    pub dialogue_target_lufs: f64,
    pub music_target_lufs: f64,
    pub sfx_target_lufs: f64,
    pub ambience_target_lufs: f64,
}

impl Default for AudioPrefs {
    fn default() -> Self {
        Self {
            automatch_time: 1.0,
            large_volume_adjustment: 6.0,
            linear_keyframe_thinning: true,
            minimum_time_interval_thinning: false,
            minimum_time_ms: 20,
            switch_to_touch_after_write: true,
            mixdown_type: "frontRear".into(),
            sum_to_mono_in_source: true,
            scrub_audio: true,
            maintain_pitch_shuttling: true,
            mute_input_during_recording: false,
            generate_waveforms_on_import: true,
            multithreaded_waveforms: false,
            render_audio_with_video: false,
            auto_tag_audio_types: true,
            always_override_audio_tags: false,
            edit_in_audition_location: "nextToMedia".into(),
            dialogue_target_lufs: -23.0,
            music_target_lufs: -25.0,
            sfx_target_lufs: -21.0,
            ambience_target_lufs: -30.0,
        }
    }
}

impl AudioPrefs {
    /// Auto-Match target for an audio type.
    pub fn loudness_target(&self, t: filmcraft_project::AudioType) -> f64 {
        match t {
            filmcraft_project::AudioType::Dialogue => self.dialogue_target_lufs,
            filmcraft_project::AudioType::Music => self.music_target_lufs,
            filmcraft_project::AudioType::Sfx => self.sfx_target_lufs,
            filmcraft_project::AudioType::Ambience => self.ambience_target_lufs,
        }
    }
    fn clamp(&mut self) {
        self.automatch_time = if self.automatch_time.is_finite() { self.automatch_time.clamp(0.0, 30.0) } else { 1.0 };
        self.large_volume_adjustment = if self.large_volume_adjustment.is_finite() { self.large_volume_adjustment.clamp(0.0, 96.0) } else { 6.0 };
        self.minimum_time_ms = self.minimum_time_ms.clamp(1, 10_000);
        for (v, d) in [
            (&mut self.dialogue_target_lufs, -23.0),
            (&mut self.music_target_lufs, -25.0),
            (&mut self.sfx_target_lufs, -21.0),
            (&mut self.ambience_target_lufs, -30.0),
        ] {
            *v = if v.is_finite() { v.clamp(-60.0, 0.0) } else { d };
        }
    }
}

impl Preferences {
    fn clamp(&mut self) {
        self.auto_save.clamp();
        self.audio.clamp();
        let secs = |v: f64| if v.is_finite() { v.clamp(0.0, 60.0) } else { 0.0 };
        self.playback.preroll_seconds = secs(self.playback.preroll_seconds);
        self.playback.postroll_seconds = secs(self.playback.postroll_seconds);
        self.trim.large_trim_offset = self.trim.large_trim_offset.clamp(1, 1000);
        self.labels.sanitize_labels();
        self.appearance.sync_color_theme();
        self.version = settings::PREFS_VERSION;
    }

    /// Parse a preferences document: migrate older layouts, repair values against the schema.
    pub fn from_value(mut v: Value) -> Self {
        settings::migrate(&mut v);
        settings::sanitize(&mut v, &Self::default().to_value());
        let mut p: Preferences = serde_json::from_value(v).unwrap_or_default();
        p.clamp();
        p
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or_default()
    }

    /// All leaf keys (`autoSave.enabled`, …).
    pub fn keys(&self) -> Vec<String> {
        fn walk(prefix: &str, v: &Value, out: &mut Vec<String>) {
            match v {
                Value::Object(m) => {
                    for (k, c) in m {
                        walk(&if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") }, c, out);
                    }
                }
                _ => out.push(prefix.to_string()),
            }
        }
        let mut out = Vec::new();
        walk("", &self.to_value(), &mut out);
        out
    }

    pub fn get(&self, key: &str) -> Option<Value> {
        let v = self.to_value();
        key.split('.').try_fold(&v, |v, k| v.get(k)).cloned()
    }

    /// Set one key (type-checked against the schema, values clamped to their ranges).
    pub fn set(&mut self, key: &str, value: Value) -> Result<(), String> {
        let mut v = self.to_value();
        let mut slot = &mut v;
        for k in key.split('.') {
            slot = slot.get_mut(k).ok_or_else(|| format!("unknown preference `{key}`"))?;
        }
        if slot.is_object() {
            return Err(format!("`{key}` is a group; set one of its keys"));
        }
        settings::validate(key, &value)?;
        let value = match (&*slot, value) {
            // Numeric choices (buffer size, sample rate) arrive as strings from the dialog.
            (Value::Number(_), Value::String(t)) if t.parse::<u64>().is_ok() => Value::from(t.parse::<u64>().unwrap_or_default()),
            // Accept 5.0 for integer prefs (agents often send floats).
            (Value::Number(n), Value::Number(m)) if n.is_u64() && !m.is_u64() => {
                Value::from(m.as_f64().filter(|f| f.is_finite() && *f >= 0.0).ok_or_else(|| format!("`{key}` must be a non-negative number"))?.round() as u64)
            }
            (_, v) => v,
        };
        *slot = value;
        let _: Preferences = serde_json::from_value(v.clone()).map_err(|e| format!("`{key}`: {e}"))?;
        settings::sanitize(&mut v, &Self::default().to_value());
        let mut p: Preferences = serde_json::from_value(v).map_err(|e| format!("`{key}`: {e}"))?;
        // Old clients that set the single Color Theme still pick the theme they name (before
        // `clamp`, which brings `colorTheme` back in line with the mode and theme choices).
        if key == "appearance.colorTheme" {
            let theme = p.appearance.color_theme.clone();
            p.appearance.select_theme(&theme);
        }
        p.clamp();
        *self = p;
        Ok(())
    }

    pub fn load(path: &Path) -> Self {
        fs::read(path).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).map(Self::from_value).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(d) = path.parent() {
            fs::create_dir_all(d)?;
        }
        filmcraft_format::atomic_write(path, &serde_json::to_vec_pretty(self).unwrap_or_default())
    }
}

/// A file with this name next to the executable makes the install portable: its data directory is
/// `<exe dir>/data` instead of the per-user one (the Windows portable zip ships it).
pub const PORTABLE_MARKER: &str = "portable.txt";

/// `<exe_dir>/data` when `exe_dir` holds [`PORTABLE_MARKER`], else `None`.
pub fn portable_data_dir(exe_dir: &Path) -> Option<PathBuf> {
    exe_dir.join(PORTABLE_MARKER).is_file().then(|| exe_dir.join("data"))
}

/// The data directory. `FILMCRAFT_DATA_DIR` overrides; a portable install ([`PORTABLE_MARKER`] next
/// to the executable) uses `<exe dir>/data`; otherwise the per-user directory:
/// macOS `~/Library/Application Support/FilmCraft`, Windows `%APPDATA%\FilmCraft`,
/// elsewhere `$XDG_DATA_HOME/filmcraft` or `~/.local/share/filmcraft`.
pub fn default_data_dir() -> Option<PathBuf> {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    if let Some(d) = env("FILMCRAFT_DATA_DIR") {
        return Some(d);
    }
    if let Some(d) = std::env::current_exe().ok().and_then(|exe| exe.parent().and_then(portable_data_dir)) {
        return Some(d);
    }
    if cfg!(target_os = "macos") {
        return env("HOME").map(|h| h.join("Library/Application Support/FilmCraft"));
    }
    if cfg!(windows) {
        return env("APPDATA").map(|h| h.join("FilmCraft"));
    }
    env("XDG_DATA_HOME").map(|d| d.join("filmcraft")).or_else(|| env("HOME").map(|h| h.join(".local/share/filmcraft")))
}

pub fn unix_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

// ------------------------------------------------------------------ recovery candidates

/// Snapshot meta written next to `snapshot.fcproj`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct JournalMeta {
    pub pid: u32,
    pub started_unix: i64,
    pub saved_unix: i64,
    pub revision: u64,
    pub project_name: String,
    pub project_path: Option<String>,
    pub generator: String,
    pub bytes: usize,
    /// True when the app was closed normally while changes were unsaved (no crash).
    pub clean_exit: bool,
}

/// Unsaved changes left behind by a session that is no longer running.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryCandidate {
    /// Session directory name (pass to `file.recover {id}`).
    pub id: String,
    #[serde(skip)]
    pub dir: PathBuf,
    #[serde(flatten)]
    pub meta: JournalMeta,
}

const SNAPSHOT: &str = "snapshot.fcproj";
const META: &str = "session.json";
const LOCK: &str = "lock";

pub fn recovery_root(data_dir: &Path) -> PathBuf {
    data_dir.join("Recovery")
}

/// Is the session directory's lock free (owner gone)?
fn lock_is_stale(dir: &Path) -> bool {
    let Ok(f) = File::options().read(true).write(true).open(dir.join(LOCK)) else { return true };
    match f.try_lock() {
        Ok(()) => true,
        Err(fs::TryLockError::WouldBlock) => false,
        // Locking unsupported on this file system: fall back to the meta's pid being gone is not
        // knowable portably, so treat as stale (the snapshot is offered, never deleted silently).
        Err(fs::TryLockError::Error(_)) => true,
    }
}

/// Find snapshots of sessions that are no longer running. Stale session directories without a
/// snapshot (nothing unsaved) are removed. `skip` is the caller's own session directory.
pub fn scan_candidates(data_dir: &Path, skip: Option<&Path>) -> Vec<RecoveryCandidate> {
    let Ok(rd) = fs::read_dir(recovery_root(data_dir)) else { return Vec::new() };
    let mut out = Vec::new();
    for e in rd.filter_map(|e| e.ok()) {
        let dir = e.path();
        if !dir.is_dir() || Some(dir.as_path()) == skip || !lock_is_stale(&dir) {
            continue;
        }
        let snap = dir.join(SNAPSHOT);
        if !snap.is_file() {
            // Empty leftover dirs (clean exit, or a crash before the first snapshot).
            let _ = fs::remove_dir_all(&dir);
            continue;
        }
        // Keep the snapshot even when session.json is missing or unreadable: the project
        // is still recoverable. Meta fields fall back to defaults.
        let meta = fs::read(dir.join(META)).ok().and_then(|b| serde_json::from_slice::<JournalMeta>(&b).ok()).unwrap_or_default();
        out.push(RecoveryCandidate { id: e.file_name().to_string_lossy().into_owned(), dir, meta });
    }
    out.sort_by(|a, b| b.meta.saved_unix.cmp(&a.meta.saved_unix).then(b.id.cmp(&a.id)));
    out
}

/// Load a candidate's snapshot.
pub fn load_candidate(c: &RecoveryCandidate) -> Result<filmcraft_format::Loaded, String> {
    let b = fs::read(c.dir.join(SNAPSHOT)).map_err(|e| format!("{}: {e}", c.dir.display()))?;
    filmcraft_format::decode(&b).map_err(|e| e.to_string())
}

pub fn discard_candidate(c: &RecoveryCandidate) -> io::Result<()> {
    fs::remove_dir_all(&c.dir)
}

// ------------------------------------------------------------------ worker

/// Host configuration for [`Persistence::start`].
#[derive(Clone)]
pub struct AutosaveConfig {
    pub data_dir: PathBuf,
    /// Local UTC offset (seconds) at a unix time — used for auto-save file names and display.
    pub local_offset: fn(i64) -> i32,
    /// Quiet period after a change before the journal is written.
    pub journal_debounce: Duration,
}

impl AutosaveConfig {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self { data_dir: data_dir.into(), local_offset: |_| 0, journal_debounce: Duration::from_millis(1000) }
    }
}

/// What the session last told the worker.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Sent {
    pub revision: u64,
    pub saved_revision: u64,
    pub path: Option<String>,
}

struct Snapshot {
    project: Arc<Project>,
    revision: u64,
    path: Option<String>,
}

enum Msg {
    /// The project has unsaved changes at this revision.
    Dirty(Snapshot),
    /// Nothing unsaved (just saved / opened): drop the journal.
    Clean,
    Prefs(AutoSavePrefs),
    AutoSaveNow(Sender<Result<PathBuf, String>>),
    Flush(Sender<Result<Option<u64>, String>>),
    Stop,
}

/// Worker → session notifications.
#[derive(Clone, Debug)]
pub enum WorkerEvent {
    Journaled {
        revision: u64,
        unix: i64,
        serialize_ms: f64,
        write_ms: f64,
        bytes: usize,
    },
    AutoSaved {
        path: PathBuf,
        revision: u64,
        unix: i64,
        serialize_ms: f64,
        write_ms: f64,
        bytes: usize,
    },
    /// "Auto Save also saves the current project" wrote the project file at this revision.
    SavedProject {
        path: String,
        revision: u64,
    },
    Error(String),
}

const WORKER_STOPPED: &str =
    "Auto-save worker stopped; automatic saving and recovery journaling are unavailable. Use File > Save or Save As to save your changes.";

/// Shared by the thread boundary and callers: a stopped worker is one failure, even when
/// several queued requests or subsequent edits discover the disconnected channel.
struct WorkerFailure {
    reported: AtomicBool,
    events: Sender<WorkerEvent>,
}

impl WorkerFailure {
    fn report(&self) {
        if !self.reported.swap(true, Ordering::AcqRel) {
            log::error!("{WORKER_STOPPED}");
            let _ = self.events.send(WorkerEvent::Error(WORKER_STOPPED.into()));
        }
    }
}

/// Last activity, for `file.autoSaveStatus` and the UI.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub last_auto_save: Option<String>,
    pub last_auto_save_unix: Option<i64>,
    pub auto_saves_written: u64,
    pub last_journal_unix: Option<i64>,
    pub journaled_revision: u64,
    pub journals_written: u64,
    /// Worker-side cost of the last write (serialize = JSON encode of the snapshot).
    pub last_serialize_ms: f64,
    pub last_write_ms: f64,
    pub last_bytes: usize,
    pub last_error: Option<String>,
}

/// The running auto-save/recovery machinery of one session.
pub struct Persistence {
    pub data_dir: PathBuf,
    pub session_dir: PathBuf,
    pub local_offset: fn(i64) -> i32,
    pub candidates: Vec<RecoveryCandidate>,
    pub status: Status,
    pub(crate) last_sent: Option<Sent>,
    lock: Option<File>,
    tx: Sender<Msg>,
    events: Receiver<WorkerEvent>,
    thread: Option<JoinHandle<()>>,
    failure: Arc<WorkerFailure>,
}

impl Persistence {
    /// Create this session's journal directory (holding its lock), find recovery candidates left by
    /// dead sessions, and start the worker thread.
    pub fn start(cfg: &AutosaveConfig, prefs: AutoSavePrefs) -> io::Result<Self> {
        let root = recovery_root(&cfg.data_dir);
        fs::create_dir_all(&root)?;
        let started = unix_now();
        let pid = std::process::id();
        // Unique even for several sessions in one process within a second (tests, future multi-window).
        static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let session_dir = root.join(if seq == 0 { format!("{started}-{pid}") } else { format!("{started}-{pid}-{seq}") });
        fs::create_dir_all(&session_dir)?;
        let lock = File::options().create(true).truncate(false).read(true).write(true).open(session_dir.join(LOCK))?;
        // Unsupported locking only weakens stale detection; it must not stop the app.
        let _ = lock.try_lock();
        let candidates = scan_candidates(&cfg.data_dir, Some(&session_dir));
        let (tx, rx) = channel();
        let (etx, events) = channel();
        let failure = Arc::new(WorkerFailure { reported: AtomicBool::new(false), events: etx.clone() });
        let mut w = Worker {
            data_dir: cfg.data_dir.clone(),
            session_dir: session_dir.clone(),
            local_offset: cfg.local_offset,
            debounce: cfg.journal_debounce,
            prefs,
            events: etx,
            latest: None,
            journaled: 0,
            autosaved: 0,
            first_pending: None,
            last_change: Instant::now(),
            next_autosave: Instant::now(),
            meta: JournalMeta { pid, started_unix: started, generator: filmcraft_format::generator(), ..Default::default() },
        };
        w.next_autosave = Instant::now() + w.autosave_interval();
        let worker_failure = failure.clone();
        let thread = std::thread::Builder::new().name("filmcraft-autosave".into()).spawn(move || {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| w.run(rx))).is_err() {
                worker_failure.report();
            }
        })?;
        Ok(Self {
            data_dir: cfg.data_dir.clone(),
            session_dir,
            local_offset: cfg.local_offset,
            candidates,
            status: Status::default(),
            last_sent: None,
            lock: Some(lock),
            tx,
            events,
            thread: Some(thread),
            failure,
        })
    }

    pub(crate) fn send_dirty(&self, project: Arc<Project>, revision: u64, path: Option<String>) {
        self.send(Msg::Dirty(Snapshot { project, revision, path }));
    }

    pub(crate) fn send_clean(&self) {
        self.send(Msg::Clean);
    }

    pub fn set_prefs(&self, p: AutoSavePrefs) {
        self.send(Msg::Prefs(p));
    }

    fn send(&self, msg: Msg) {
        if self.tx.send(msg).is_err() {
            self.failure.report();
        }
    }

    fn stopped_error(&self) -> String {
        self.failure.report();
        WORKER_STOPPED.into()
    }

    pub(crate) fn is_running(&self) -> bool {
        !self.failure.reported.load(Ordering::Acquire) && self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }

    /// Write any pending journal now and return its confirmed revision (`None` when clean).
    /// Disabled journaling, failed writes and a stopped/unresponsive worker are errors.
    pub fn flush(&self) -> Result<Option<u64>, String> {
        let (tx, rx) = channel();
        self.tx.send(Msg::Flush(tx)).map_err(|_| self.stopped_error())?;
        rx.recv_timeout(Duration::from_secs(30)).map_err(|e| match e {
            RecvTimeoutError::Timeout => "recovery journal flush timed out".to_string(),
            RecvTimeoutError::Disconnected => {
                self.failure.report();
                "auto-save worker stopped before confirming the recovery journal".to_string()
            }
        })?
    }

    /// Write an auto-save now (if there are unsaved changes) and wait for the result.
    pub fn auto_save_now(&self) -> Result<Option<PathBuf>, String> {
        let (tx, rx) = channel();
        self.tx.send(Msg::AutoSaveNow(tx)).map_err(|_| self.stopped_error())?;
        match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(Ok(p)) => Ok(Some(p)),
            Ok(Err(e)) if e.is_empty() => Ok(None),
            Ok(Err(e)) => Err(e),
            Err(RecvTimeoutError::Timeout) => Err("auto-save timed out".into()),
            Err(RecvTimeoutError::Disconnected) => Err(self.stopped_error()),
        }
    }

    pub fn drain_events(&mut self) -> Vec<WorkerEvent> {
        let ev: Vec<_> = self.events.try_iter().collect();
        for e in &ev {
            match e {
                WorkerEvent::Journaled { revision, unix, serialize_ms, write_ms, bytes } => {
                    self.status.last_journal_unix = Some(*unix);
                    self.status.journaled_revision = *revision;
                    self.status.journals_written += 1;
                    (self.status.last_serialize_ms, self.status.last_write_ms, self.status.last_bytes) = (*serialize_ms, *write_ms, *bytes);
                }
                WorkerEvent::AutoSaved { path, unix, serialize_ms, write_ms, bytes, .. } => {
                    self.status.last_auto_save = Some(path.to_string_lossy().into_owned());
                    self.status.last_auto_save_unix = Some(*unix);
                    self.status.auto_saves_written += 1;
                    (self.status.last_serialize_ms, self.status.last_write_ms, self.status.last_bytes) = (*serialize_ms, *write_ms, *bytes);
                }
                WorkerEvent::SavedProject { .. } => {}
                WorkerEvent::Error(m) => self.status.last_error = Some(m.clone()),
            }
        }
        ev
    }

    /// Stop the worker. A session that still has a journal (closed with unsaved changes) keeps its
    /// directory — marked as a clean exit — so the changes are offered next time; otherwise the
    /// directory is removed.
    pub fn close(mut self) {
        if let Err(e) = self.flush() {
            log::warn!("auto-save at shutdown: {e}");
        }
        self.stop_worker();
        let snap = self.session_dir.join(SNAPSHOT);
        self.lock.take();
        if snap.is_file() {
            if let Some(mut m) = fs::read(self.session_dir.join(META)).ok().and_then(|b| serde_json::from_slice::<JournalMeta>(&b).ok()) {
                m.clean_exit = true;
                let _ = filmcraft_format::atomic_write(&self.session_dir.join(META), &serde_json::to_vec_pretty(&m).unwrap_or_default());
            }
        } else {
            let _ = fs::remove_dir_all(&self.session_dir);
        }
    }

    /// Stop the worker and release the lock *without* cleaning up — what a crash leaves behind.
    #[doc(hidden)]
    pub fn simulate_crash(mut self) {
        let _ = self.flush();
        self.stop_worker();
        self.lock.take();
    }

    fn stop_worker(&mut self) {
        let _ = self.tx.send(Msg::Stop);
        if let Some(t) = self.thread.take()
            && t.join().is_err()
        {
            self.failure.report();
        }
    }

    /// Display form of a unix time in local time.
    pub fn local_time(&self, unix: i64) -> String {
        names::display_time(unix, (self.local_offset)(unix))
    }

    pub fn candidates_json(&self) -> Value {
        Value::Array(
            self.candidates
                .iter()
                .map(|c| {
                    let mut v = serde_json::to_value(c).unwrap_or_default();
                    v["savedAt"] = json!(self.local_time(c.meta.saved_unix));
                    v
                })
                .collect(),
        )
    }
}

impl Drop for Persistence {
    fn drop(&mut self) {
        self.stop_worker();
    }
}

struct Worker {
    data_dir: PathBuf,
    session_dir: PathBuf,
    local_offset: fn(i64) -> i32,
    debounce: Duration,
    prefs: AutoSavePrefs,
    events: Sender<WorkerEvent>,
    /// Newest unsaved state (None = clean).
    latest: Option<Snapshot>,
    /// Revision currently in the journal (0 = none) / last auto-saved revision.
    journaled: u64,
    autosaved: u64,
    first_pending: Option<Instant>,
    last_change: Instant,
    next_autosave: Instant,
    meta: JournalMeta,
}

impl Worker {
    fn autosave_interval(&self) -> Duration {
        Duration::from_secs(self.prefs.interval_minutes as u64 * 60)
    }

    fn journal_due(&self) -> Option<Instant> {
        let s = self.latest.as_ref()?;
        if !self.prefs.recovery_journal || s.revision == self.journaled {
            return None;
        }
        let max = self.first_pending.unwrap_or(self.last_change) + Duration::from_secs(self.prefs.recovery_interval_seconds as u64);
        Some((self.last_change + self.debounce).min(max))
    }

    fn run(mut self, rx: Receiver<Msg>) {
        loop {
            let due = [self.journal_due(), self.prefs.enabled.then_some(self.next_autosave)].into_iter().flatten().min();
            let msg = match due {
                Some(d) => match rx.recv_timeout(d.saturating_duration_since(Instant::now())) {
                    Ok(m) => Some(m),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return,
                },
                None => match rx.recv() {
                    Ok(m) => Some(m),
                    Err(_) => return,
                },
            };
            match msg {
                Some(Msg::Dirty(s)) => {
                    let now = Instant::now();
                    self.first_pending.get_or_insert(now);
                    self.last_change = now;
                    self.latest = Some(s);
                }
                Some(Msg::Clean) => {
                    self.latest = None;
                    self.first_pending = None;
                    self.clear_journal();
                }
                Some(Msg::Prefs(p)) => {
                    let was = self.autosave_interval();
                    let journal_was = self.prefs.recovery_journal;
                    self.prefs = p;
                    if self.autosave_interval() != was {
                        self.next_autosave = Instant::now() + self.autosave_interval();
                    }
                    if journal_was && !self.prefs.recovery_journal {
                        self.clear_journal();
                    }
                }
                Some(Msg::AutoSaveNow(reply)) => {
                    let r = if self.latest.is_some() { self.auto_save() } else { Err(String::new()) };
                    let _ = reply.send(r);
                }
                Some(Msg::Flush(ack)) => {
                    let result = match self.latest.as_ref() {
                        None => Ok(None),
                        Some(_) if !self.prefs.recovery_journal => Err("recovery journal is disabled".to_string()),
                        Some(s) if s.revision == self.journaled => Ok(Some(self.journaled)),
                        Some(_) => self.write_journal().map(Some),
                    };
                    let _ = ack.send(result);
                }
                Some(Msg::Stop) => return,
                None => {}
            }
            let now = Instant::now();
            if self.journal_due().is_some_and(|d| d <= now) {
                // The worker reports write failures through its event channel.
                let _ = self.write_journal();
            }
            if self.prefs.enabled && self.next_autosave <= now {
                if self.latest.as_ref().is_some_and(|s| s.revision != self.autosaved) {
                    let _ = self.auto_save();
                }
                self.next_autosave = now + self.autosave_interval();
            }
        }
    }

    fn error(&self, e: String) {
        log::warn!("auto-save: {e}");
        let _ = self.events.send(WorkerEvent::Error(e));
    }

    fn write_journal(&mut self) -> Result<u64, String> {
        let Some(s) = &self.latest else { return Err("no unsaved recovery state to write".into()) };
        let t0 = Instant::now();
        let bytes = filmcraft_format::encode(&s.project, false);
        let serialize_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let t1 = Instant::now();
        let unix = unix_now();
        let mut meta = self.meta.clone();
        meta.saved_unix = unix;
        meta.revision = s.revision;
        meta.project_name = s.project.name.clone();
        meta.project_path = s.path.clone();
        meta.bytes = bytes.len();
        // Snapshot first, then the meta that makes it a candidate.
        let r = filmcraft_format::atomic_write(&self.session_dir.join(SNAPSHOT), &bytes)
            .and_then(|_| filmcraft_format::atomic_write(&self.session_dir.join(META), &serde_json::to_vec_pretty(&meta).unwrap_or_default()));
        let write_ms = t1.elapsed().as_secs_f64() * 1000.0;
        match r {
            Ok(()) => {
                self.journaled = s.revision;
                self.first_pending = None;
                let _ = self.events.send(WorkerEvent::Journaled { revision: s.revision, unix, serialize_ms, write_ms, bytes: bytes.len() });
                Ok(s.revision)
            }
            Err(e) => {
                // Try again at the next interval rather than spinning.
                self.first_pending = Some(Instant::now());
                self.last_change = Instant::now();
                let message = format!("recovery journal: {e}");
                self.error(message.clone());
                Err(message)
            }
        }
    }

    fn clear_journal(&mut self) {
        let _ = fs::remove_file(self.session_dir.join(META));
        let _ = fs::remove_file(self.session_dir.join(SNAPSHOT));
        self.journaled = 0;
    }

    fn auto_save(&mut self) -> Result<PathBuf, String> {
        let Some(s) = &self.latest else { return Err("nothing to save".into()) };
        let scratch = crate::project_tools::trusted_scratch(s.project.settings.scratch.auto_save.as_deref(), s.path.as_deref(), &self.prefs.scratch_folders);
        let (dir, name) = match (&s.path, scratch) {
            // Project Settings ▸ Scratch Disks ▸ Project Auto Save
            (Some(p), Some(scratch)) => {
                let p = Path::new(p);
                (scratch, p.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| s.project.name.clone()))
            }
            (Some(p), None) => {
                let p = Path::new(p);
                (names::auto_save_dir(p), p.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| s.project.name.clone()))
            }
            (None, _) => (self.data_dir.join(names::AUTO_SAVE_DIR), s.project.name.clone()),
        };
        let t0 = Instant::now();
        let bytes = filmcraft_format::encode(&s.project, false);
        let serialize_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let t1 = Instant::now();
        let unix = unix_now();
        let r = names::write_auto_save(&dir, &name, unix, (self.local_offset)(unix), &bytes, self.prefs.max_versions as usize);
        let write_ms = t1.elapsed().as_secs_f64() * 1000.0;
        match r {
            Ok(path) => {
                self.autosaved = s.revision;
                let _ = self.events.send(WorkerEvent::AutoSaved { path: path.clone(), revision: s.revision, unix, serialize_ms, write_ms, bytes: bytes.len() });
                if self.prefs.save_current_project
                    && let Some(p) = &s.path
                {
                    match filmcraft_format::atomic_write(Path::new(p), &bytes) {
                        Ok(()) => {
                            let _ = self.events.send(WorkerEvent::SavedProject { path: p.clone(), revision: s.revision });
                        }
                        Err(e) => self.error(format!("saving {p}: {e}")),
                    }
                }
                Ok(path)
            }
            Err(e) => {
                let m = format!("auto-save to {}: {e}", dir.display());
                self.error(m.clone());
                Err(m)
            }
        }
    }
}

#[cfg(test)]
mod durability_tests {
    use super::*;

    #[test]
    fn recovery_keeps_original_when_worker_has_stopped() {
        let d = std::env::temp_dir().join(format!("fc-recover-worker-stopped-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        let old_dir = recovery_root(&d).join("1-1");
        fs::create_dir_all(&old_dir).unwrap();
        let bytes = filmcraft_format::encode(&Project::default(), false);
        let original = old_dir.join(SNAPSHOT);
        fs::write(&original, &bytes).unwrap();
        let mut session = crate::Session::default();
        session.start_autosave(AutosaveConfig::new(&d)).unwrap();
        session.persistence.as_mut().unwrap().stop_worker();
        let result = session.execute("file.recover", json!({"id": "1-1"}));
        assert!(original.is_file(), "a stopped worker must not consume the original snapshot");
        assert_eq!(fs::read(&original).unwrap(), bytes);
        assert_eq!(session.recovery_candidates().len(), 1);
        assert!(result.unwrap_err().to_string().contains("worker stopped"));
        session.shutdown();
        fs::remove_dir_all(&d).unwrap();
    }
}

#[cfg(test)]
mod worker_failure_tests {
    use super::*;
    use crate::{Event, Session};

    fn session(tag: &str, offset: fn(i64) -> i32) -> (Session, PathBuf) {
        let dir = std::env::temp_dir().join(format!("fc-worker-{tag}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let mut prefs = Preferences::default();
        prefs.auto_save.recovery_journal = false;
        prefs.save(&dir.join("preferences.json")).unwrap();
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let mut cfg = AutosaveConfig::new(&dir);
        cfg.local_offset = offset;
        s.start_autosave(cfg).unwrap();
        s.execute("sequence.addEdit", json!({"seconds": 0.5})).unwrap();
        (s, dir)
    }

    fn failure_toasts(s: &mut Session) -> usize {
        s.drain_events().into_iter().filter(|e| matches!(e, Event::Toast { error: true, .. })).count()
    }

    fn verify_report_and_foreground_save(s: &mut Session, dir: &Path) {
        s.poll_persistence();
        assert_eq!(s.execute("file.autoSaveStatus", json!({})).unwrap()["running"], false);
        let message = s.persistence.as_ref().unwrap().status.last_error.as_deref().unwrap();
        assert!(message.contains("stopped") && message.contains("Save"), "{message}");
        assert_eq!(failure_toasts(s), 1, "worker failure must be visible exactly once");
        let log: Vec<_> = s.log.entries.iter().filter(|e| e.source == "autosave").collect();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].count, 1);
        let before = s.revision;
        s.execute("sequence.addEdit", json!({"seconds": 1.0})).unwrap();
        assert!(s.revision > before && s.is_dirty(), "the edit was applied despite autosave failure");
        s.execute("prefs.set", json!({"key": "autoSave.maxVersions", "value": 7})).unwrap();
        s.poll_persistence();
        s.poll_persistence();
        assert_eq!(failure_toasts(s), 0, "later handoffs must not repeat the failure");
        let path = dir.join("manual.fcproj");
        s.execute("file.saveAs", json!({"path": path.to_string_lossy()})).unwrap();
        assert!(!s.is_dirty());
        let loaded = filmcraft_format::decode(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(loaded.project, *s.project);
        s.poll_persistence();
        assert_eq!(failure_toasts(s), 0);
    }

    #[test]
    fn worker_panic_is_contained_and_reported() {
        fn broken_clock(_: i64) -> i32 {
            panic!("synthetic worker clock failure")
        }
        let (mut s, dir) = session("panic", broken_clock);
        let error = s.execute("file.autoSaveNow", json!({})).unwrap_err().to_string();
        let start = Instant::now();
        while !s.persistence.as_ref().unwrap().thread.as_ref().unwrap().is_finished() {
            assert!(start.elapsed() < Duration::from_secs(10), "worker did not exit");
            std::thread::sleep(Duration::from_millis(10));
        }
        let joined = s.persistence.as_mut().unwrap().thread.take().unwrap().join();
        assert!(joined.is_ok(), "panic escaped the autosave worker boundary");
        assert!(error.contains("stopped") && !error.contains("timed out"), "{error}");
        verify_report_and_foreground_save(&mut s, &dir);
        s.shutdown();
        fs::remove_dir_all(dir).unwrap();
    }

    fn disconnected_handoff(tag: &str) {
        let (mut s, dir) = session(tag, |_| 0);
        s.persistence.as_mut().unwrap().stop_worker();
        match tag {
            "dirty" => {
                s.execute("sequence.addEdit", json!({"seconds": 2.0})).unwrap();
            }
            "clean" => {
                s.execute("file.saveAs", json!({"path": dir.join("initial.fcproj").to_string_lossy()})).unwrap();
            }
            "prefs" => {
                s.execute("prefs.set", json!({"key": "autoSave.maxVersions", "value": 3})).unwrap();
            }
            "flush" => {
                assert!(s.persistence.as_ref().unwrap().flush().unwrap_err().contains("worker stopped"));
            }
            "request" => {
                assert!(s.execute("file.autoSaveNow", json!({})).unwrap_err().to_string().contains("stopped"));
            }
            _ => panic!("unknown test handoff"),
        }
        verify_report_and_foreground_save(&mut s, &dir);
        s.shutdown();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn worker_disconnect_dirty_reports_once() {
        disconnected_handoff("dirty");
    }
    #[test]
    fn worker_disconnect_clean_reports_once() {
        disconnected_handoff("clean");
    }
    #[test]
    fn worker_disconnect_preferences_reports_once() {
        disconnected_handoff("prefs");
    }
    #[test]
    fn worker_disconnect_flush_reports_once() {
        disconnected_handoff("flush");
    }
    #[test]
    fn worker_disconnect_flush_reply_reports_once() {
        let (mut s, dir) = session("flush-reply", |_| 0);
        s.persistence.as_mut().unwrap().stop_worker();
        let (tx, rx) = channel();
        s.persistence.as_mut().unwrap().tx = tx;
        let worker = std::thread::spawn(move || match rx.recv().unwrap() {
            Msg::Flush(reply) => drop(reply),
            _ => panic!("expected a flush request"),
        });
        let error = s.persistence.as_ref().unwrap().flush().unwrap_err();
        worker.join().unwrap();
        assert!(error.contains("worker stopped"), "{error}");
        verify_report_and_foreground_save(&mut s, &dir);
        s.shutdown();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn worker_disconnect_request_reports_once() {
        disconnected_handoff("request");
    }

    #[test]
    fn worker_remains_available_after_an_ordinary_write_error() {
        let (mut s, dir) = session("write-error", |_| 0);
        let blocked = dir.join(names::AUTO_SAVE_DIR);
        fs::write(&blocked, b"occupied directory path").unwrap();
        assert!(s.execute("file.autoSaveNow", json!({})).is_err());
        s.poll_persistence();
        assert_eq!(s.execute("file.autoSaveStatus", json!({})).unwrap()["running"], true);
        assert!(s.persistence.as_ref().unwrap().status.last_error.as_deref().unwrap().contains("auto-save to"));
        assert_eq!(failure_toasts(&mut s), 1);
        fs::remove_file(&blocked).unwrap();
        let saved = s.execute("file.autoSaveNow", json!({})).unwrap();
        assert!(Path::new(saved["path"].as_str().unwrap()).is_file());
        assert!(s.is_dirty(), "auto-save should not mark the foreground project saved");
        s.shutdown();
        fs::remove_dir_all(dir).unwrap();
    }
}
