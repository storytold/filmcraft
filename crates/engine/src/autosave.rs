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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex, PoisonError};
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

/// A process-wide data root selected by a host before any FilmCraft service reads a path.
static DATA_DIR_OVERRIDE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Select the absolute data root once for this process.
pub fn set_data_dir_override(root: PathBuf) -> Result<(), String> {
    if root.as_os_str().is_empty() || !root.is_absolute() {
        return Err("the selected data root must be an absolute path".into());
    }
    DATA_DIR_OVERRIDE.set(root).map_err(|_| "the data directory was already selected".to_owned())
}

/// The data root selected by a host, if one has been installed.
pub fn data_dir_override() -> Option<&'static Path> {
    DATA_DIR_OVERRIDE.get().map(PathBuf::as_path)
}

/// The data directory. `FILMCRAFT_DATA_DIR` overrides; a portable install ([`PORTABLE_MARKER`] next
/// to the executable) uses `<exe dir>/data`; otherwise the per-user directory:
/// macOS `~/Library/Application Support/FilmCraft`, Windows `%APPDATA%\FilmCraft`,
/// elsewhere `$XDG_DATA_HOME/filmcraft` or `~/.local/share/filmcraft`.
pub fn default_data_dir() -> Option<PathBuf> {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    if let Some(root) = data_dir_override() {
        return Some(root.to_path_buf());
    }
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
    /// Fingerprint binding this metadata to the exact snapshot bytes (absent in old journals).
    #[serde(default)]
    pub snapshot_fingerprint: Option<[u8; 32]>,
    /// True when the app was closed normally while changes were unsaved (no crash).
    pub clean_exit: bool,
    /// Whether the project CONTENT was unsaved when this snapshot was written. Older versions
    /// journaled clean view-only changes; those entries must not be offered as unsaved work.
    /// Older journals without the field keep being offered (the default is true).
    #[serde(default = "journal_dirty_by_default")]
    pub dirty: bool,
    /// This journal contains only a compact ProjectView and is bound to the unchanged project.
    #[serde(default)]
    pub view_only: bool,
    #[serde(default)]
    pub project_fingerprint: Option<[u8; 32]>,
    /// Latest view when it changed after the dirty project snapshot was written. Kept in the
    /// small metadata file so a view-only update does not re-encode or rewrite project content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_view: Option<filmcraft_project::ProjectView>,
}

fn journal_dirty_by_default() -> bool {
    true
}

pub(crate) fn snapshot_fingerprint(bytes: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    sha2::Sha256::digest(bytes).into()
}

const MAX_RECOVERY_META_BYTES: u64 = 1024 * 1024;

fn read_recovery_snapshot(path: &Path) -> io::Result<Vec<u8>> {
    filmcraft_media::reader::read_regular_file_bounded(path, filmcraft_format::MAX_PROJECT_FILE_BYTES)
}

fn read_recovery_metadata(path: &Path) -> io::Result<Vec<u8>> {
    filmcraft_media::reader::read_regular_file_bounded(path, MAX_RECOVERY_META_BYTES)
}

fn metadata_for_snapshot(snapshot: &[u8], metadata: &Path) -> Option<JournalMeta> {
    let meta: JournalMeta = serde_json::from_slice(&read_recovery_metadata(metadata).ok()?).ok()?;
    if meta.snapshot_fingerprint.is_some_and(|fingerprint| fingerprint != snapshot_fingerprint(snapshot)) {
        return None;
    }
    Some(meta)
}

fn matching_journal_pair(snapshot: &Path, metadata: &Path) -> Option<(PathBuf, JournalMeta)> {
    let bytes = read_recovery_snapshot(snapshot).ok()?;
    Some((snapshot.to_path_buf(), metadata_for_snapshot(&bytes, metadata)?))
}

/// Unsaved changes left behind by a session that is no longer running.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryCandidate {
    /// Session directory name (pass to `file.recover {id}`).
    pub id: String,
    #[serde(skip)]
    pub dir: PathBuf,
    #[serde(skip)]
    pub(crate) snapshot_path: PathBuf,
    #[serde(flatten)]
    pub meta: JournalMeta,
}

/// Whether a view-only recovery candidate still belongs to the project file it was saved beside.
/// Read and hash the file off the UI thread through the same size-bounded path used by recovery.
pub fn view_recovery_project_changed(candidate: &RecoveryCandidate) -> bool {
    let Some(path) = candidate.meta.project_path.as_deref().filter(|path| !path.is_empty()) else { return false };
    let Some(expected) = candidate.meta.project_fingerprint else { return true };
    read_recovery_snapshot(Path::new(path)).map_or(true, |bytes| snapshot_fingerprint(&bytes) != expected)
}

const SNAPSHOT: &str = "snapshot.fcproj";
const META: &str = "session.json";
const PREVIOUS_SNAPSHOT: &str = "snapshot.previous.fcproj";
const PREVIOUS_META: &str = "session.previous.json";
const PENDING_META: &str = "session.pending.json";
const LOCK: &str = "lock";
const STARTING: &str = ".starting";

pub fn recovery_root(data_dir: &Path) -> PathBuf {
    data_dir.join("Recovery")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LockState {
    Stale,
    Active,
    Unknown,
}

fn classify_lock_error(error: fs::TryLockError) -> LockState {
    match error {
        fs::TryLockError::WouldBlock => LockState::Active,
        // An unsupported or failed lock cannot establish that the owner is gone.
        fs::TryLockError::Error(_) => LockState::Unknown,
    }
}

/// Is the session directory's lock free (owner gone)? Unknown errors are kept for safety.
fn lock_is_stale(dir: &Path) -> LockState {
    let f = match File::options().read(true).write(true).open(dir.join(LOCK)) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return LockState::Stale,
        Err(_) => return LockState::Unknown,
    };
    match f.try_lock() {
        Ok(()) => LockState::Stale,
        Err(e) => classify_lock_error(e),
    }
}

/// Find snapshots of sessions that are no longer running. This scan is read-only. `skip` is the
/// caller's own session directory.
pub fn scan_candidates(data_dir: &Path, skip: Option<&Path>) -> Vec<RecoveryCandidate> {
    let Ok(rd) = fs::read_dir(recovery_root(data_dir)) else { return Vec::new() };
    let mut out = Vec::new();
    for e in rd.filter_map(|e| e.ok()) {
        let dir = e.path();
        if dir.join(STARTING).exists() {
            continue;
        }
        if !dir.is_dir() || Some(dir.as_path()) == skip || lock_is_stale(&dir) != LockState::Stale {
            continue;
        }
        let snap = dir.join(SNAPSHOT);
        let previous_snap = dir.join(PREVIOUS_SNAPSHOT);
        if !snap.is_file() && !previous_snap.is_file() {
            // Empty leftovers are cleaned by the explicit stale-session cleanup pass, never by
            // candidate discovery. A scan must not mutate recovery data.
            continue;
        }
        // Metadata is bound to its snapshot. A failed replacement may leave the new pending meta
        // beside the previous snapshot; in that case use the preserved committed pair instead.
        let pending = matching_journal_pair(&snap, &dir.join(PENDING_META));
        let main = matching_journal_pair(&snap, &dir.join(META));
        let previous = matching_journal_pair(&previous_snap, &dir.join(PREVIOUS_META));
        let (snapshot_path, meta) = pending
            .or(main)
            .or(previous)
            .or_else(|| snap.is_file().then(|| (snap.clone(), JournalMeta { dirty: true, ..JournalMeta::default() })))
            .unwrap_or_else(|| (previous_snap.clone(), JournalMeta { dirty: true, ..JournalMeta::default() }));
        if !meta.dirty {
            // A clean project's view journal is useful while the app is running, but it is not
            // unsaved project content and must not be offered as crash recovery.
            continue;
        }
        out.push(RecoveryCandidate { id: e.file_name().to_string_lossy().into_owned(), dir, snapshot_path, meta });
    }
    out.sort_by(|a, b| b.meta.saved_unix.cmp(&a.meta.saved_unix).then(b.id.cmp(&a.id)));
    out
}

/// Remove stale sessions that contain no recoverable edits. Kept separate from scanning so
/// callers can inspect recovery candidates without changing the filesystem.
fn cleanup_stale_clean_sessions(data_dir: &Path, skip: Option<&Path>) {
    let Ok(rd) = fs::read_dir(recovery_root(data_dir)) else { return };
    for entry in rd.filter_map(Result::ok) {
        let dir = entry.path();
        if !dir.is_dir() || Some(dir.as_path()) == skip || dir.join(STARTING).exists() || lock_is_stale(&dir) != LockState::Stale {
            continue;
        }
        let snap = dir.join(SNAPSHOT);
        let previous_snap = dir.join(PREVIOUS_SNAPSHOT);
        if !snap.is_file() && !previous_snap.is_file() {
            // Avoid racing the short window between directory creation and lock acquisition.
            let recent = fs::metadata(&dir)
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_none_or(|age| age < Duration::from_secs(60));
            if !recent {
                let _ = fs::remove_dir_all(&dir);
            }
            continue;
        }
        let pending = matching_journal_pair(&snap, &dir.join(PENDING_META));
        let main = matching_journal_pair(&snap, &dir.join(META));
        let previous = matching_journal_pair(&previous_snap, &dir.join(PREVIOUS_META));
        let (_, meta) = pending.or(main).or(previous).unwrap_or_else(|| {
            let path = if snap.is_file() { snap } else { previous_snap };
            (path, JournalMeta { dirty: true, ..JournalMeta::default() })
        });
        if !meta.dirty {
            let _ = fs::remove_dir_all(&dir);
        }
    }
}

/// Load a candidate's snapshot.
pub fn load_candidate(c: &RecoveryCandidate) -> Result<filmcraft_format::Loaded, String> {
    let b = read_recovery_snapshot(&c.snapshot_path).map_err(|e| format!("{}: {e}", c.snapshot_path.display()))?;
    if c.meta.snapshot_fingerprint.is_some_and(|fingerprint| fingerprint != snapshot_fingerprint(&b)) {
        return Err(format!("{} changed after recovery candidates were scanned; rescan before recovery", c.snapshot_path.display()));
    }
    let mut loaded = filmcraft_format::decode(&b).map_err(|e| e.to_string())?;
    if let Some(view) = &c.meta.recovery_view {
        loaded.view = Some(view.clone());
    }
    Ok(loaded)
}

pub fn load_view_candidate(c: &RecoveryCandidate) -> Result<filmcraft_project::ProjectView, String> {
    let b = read_recovery_snapshot(&c.snapshot_path).map_err(|e| format!("{}: {e}", c.snapshot_path.display()))?;
    if c.meta.snapshot_fingerprint.is_some_and(|fingerprint| fingerprint != snapshot_fingerprint(&b)) {
        return Err(format!("{} changed after recovery candidates were scanned; rescan before recovery", c.snapshot_path.display()));
    }
    serde_json::from_slice(&b).map_err(|e| format!("{}: {e}", c.snapshot_path.display()))
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
    /// Bumped when only the stored Timeline view (zoom, scroll, sliders) changed. Tracked
    /// separately from `revision` so per-frame view changes never invalidate revision-keyed
    /// caches, while still reaching the autosave journal.
    pub view_epoch: u64,
    /// The view counter as of the last save: it must travel in the comparison too, or a save
    /// that catches the view up sends nothing and leaves the journal behind.
    pub saved_view_epoch: u64,
}

pub(crate) struct Snapshot {
    pub(crate) project: Arc<Project>,
    pub(crate) view: filmcraft_project::ProjectView,
    pub(crate) revision: u64,
    /// The session's view counter at this snapshot: a view change to content that is already
    /// dirty journals again even though `revision` is unchanged.
    pub(crate) view_epoch: u64,
    /// Whether the project content was unsaved when this snapshot was taken.
    pub(crate) content_dirty: bool,
    pub(crate) path: Option<String>,
    pub(crate) project_fingerprint: Option<[u8; 32]>,
}

/// Shared ordering fence for the background and foreground writers of the current project file.
#[derive(Default)]
pub(crate) struct ProjectFileFence {
    pub revision: u64,
    pub view_epoch: u64,
    pub path: Option<String>,
    pub fingerprint: Option<[u8; 32]>,
}

enum Msg {
    /// The project has unsaved changes at this revision. Boxed: the other messages are a few
    /// words and the enum is sent after every command.
    Dirty(Box<Snapshot>),
    /// Nothing unsaved (just saved / opened): drop the journal.
    Clean,
    Prefs(AutoSavePrefs),
    AutoSaveNow(Sender<Result<PathBuf, String>>),
    Flush(Sender<Result<Option<u64>, String>>),
    Stop,
}

fn coalesce_dirty_updates(rx: &Receiver<Msg>, mut latest: Box<Snapshot>) -> (Box<Snapshot>, Option<Msg>) {
    loop {
        match rx.try_recv() {
            Ok(Msg::Dirty(next)) => latest = next,
            Ok(barrier) => return (latest, Some(barrier)),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => return (latest, None),
        }
    }
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
        view_epoch: u64,
        fingerprint: [u8; 32],
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
    /// A slider drag may update the UI every frame. Bound its handoffs to the worker; shutdown
    /// synchronizes the final state before flushing.
    pub(crate) last_view_sent: Option<Instant>,
    lock: Option<File>,
    tx: Sender<Msg>,
    events: Receiver<WorkerEvent>,
    thread: Option<JoinHandle<()>>,
    failure: Arc<WorkerFailure>,
}

impl Persistence {
    /// Create this session's journal directory (holding its lock), find recovery candidates left by
    /// dead sessions, and start the worker thread.
    pub(crate) fn start(cfg: &AutosaveConfig, prefs: AutoSavePrefs, project_file_fence: Arc<Mutex<ProjectFileFence>>) -> io::Result<Self> {
        let root = recovery_root(&cfg.data_dir);
        fs::create_dir_all(&root)?;
        let started = unix_now();
        let pid = std::process::id();
        // Unique even for several sessions in one process within a second (tests, future multi-window).
        static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let session_dir = root.join(if seq == 0 { format!("{started}-{pid}") } else { format!("{started}-{pid}-{seq}") });
        fs::create_dir(&session_dir)?;
        let starting = session_dir.join(STARTING);
        if let Err(error) = File::create(&starting) {
            let _ = fs::remove_dir_all(&session_dir);
            return Err(error);
        }
        let lock_path = session_dir.join(LOCK);
        let lock = match File::options().create(true).truncate(false).read(true).write(true).open(&lock_path) {
            Ok(lock) => lock,
            Err(error) => {
                let _ = fs::remove_file(&starting);
                let _ = fs::remove_dir_all(&session_dir);
                return Err(error);
            }
        };
        match lock.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => {
                let _ = fs::remove_file(&starting);
                let _ = fs::remove_dir_all(&session_dir);
                return Err(io::Error::new(io::ErrorKind::WouldBlock, "new recovery session lock is already held"));
            }
            // Unsupported locking only weakens stale detection; it must not stop the app. Scans
            // keep lock errors as Unknown, so they will leave this directory alone.
            Err(fs::TryLockError::Error(e)) => log::warn!("recovery session lock is unavailable: {e}"),
        }
        let _ = fs::remove_file(starting);
        cleanup_stale_clean_sessions(&cfg.data_dir, Some(&session_dir));
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
            journaled_view: 0,
            journaled_dirty: None,
            autosaved: 0,
            autosaved_view: 0,
            autosaved_path: None,
            first_pending: None,
            last_change: Instant::now(),
            next_autosave: Instant::now(),
            meta: JournalMeta { pid, started_unix: started, generator: filmcraft_format::generator(), ..Default::default() },
            project_file_fence,
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
            last_view_sent: None,
            lock: Some(lock),
            tx,
            events,
            thread: Some(thread),
            failure,
        })
    }

    pub(crate) fn send_dirty(&self, snapshot: Snapshot) {
        self.send(Msg::Dirty(Box::new(snapshot)));
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
            if let Some(mut m) = read_recovery_metadata(&self.session_dir.join(META)).ok().and_then(|b| serde_json::from_slice::<JournalMeta>(&b).ok()) {
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
    /// Revision currently in the journal (0 = none) / last auto-saved revision. The view
    /// counters sit beside them: a view change to dirty content must journal again at the same revision.
    journaled: u64,
    journaled_view: u64,
    /// Dirty baseline stored with the journaled revision and view epoch.
    journaled_dirty: Option<bool>,
    autosaved: u64,
    autosaved_view: u64,
    autosaved_path: Option<PathBuf>,
    first_pending: Option<Instant>,
    last_change: Instant,
    next_autosave: Instant,
    meta: JournalMeta,
    project_file_fence: Arc<Mutex<ProjectFileFence>>,
}

impl Worker {
    fn autosave_interval(&self) -> Duration {
        Duration::from_secs(self.prefs.interval_minutes as u64 * 60)
    }

    fn journal_due(&self) -> Option<Instant> {
        let s = self.latest.as_ref()?;
        if !self.prefs.recovery_journal || self.journal_matches(s) {
            return None;
        }
        if view_only_journal_update(s.revision, s.view_epoch, s.content_dirty, self.journaled, self.journaled_view, self.journaled_dirty) {
            // Slider motion can last much longer than the content debounce. Reset this longer
            // quiet timer on each view update, so a drag does not repeatedly serialize the whole
            // dirty project at the recovery interval.
            return Some(self.last_change + self.debounce.max(Duration::from_secs(2)));
        }
        let max = self.first_pending.unwrap_or(self.last_change) + Duration::from_secs(self.prefs.recovery_interval_seconds as u64);
        Some((self.last_change + self.debounce).min(max))
    }

    fn journal_matches(&self, s: &Snapshot) -> bool {
        journal_state_matches(s.revision, s.view_epoch, s.content_dirty, self.journaled, self.journaled_view, self.journaled_dirty)
    }

    fn run(mut self, rx: Receiver<Msg>) {
        let mut pending = None;
        loop {
            let due = [self.journal_due(), self.prefs.enabled.then_some(self.next_autosave)].into_iter().flatten().min();
            let msg = if let Some(msg) = pending.take() {
                Some(msg)
            } else {
                match due {
                    Some(d) => match rx.recv_timeout(d.saturating_duration_since(Instant::now())) {
                        Ok(m) => Some(m),
                        Err(RecvTimeoutError::Timeout) => None,
                        Err(RecvTimeoutError::Disconnected) => return,
                    },
                    None => match rx.recv() {
                        Ok(m) => Some(m),
                        Err(_) => return,
                    },
                }
            };
            match msg {
                Some(Msg::Dirty(s)) => {
                    let now = Instant::now();
                    self.first_pending.get_or_insert(now);
                    self.last_change = now;
                    // View drags can enqueue a full project view every frame. Collapse adjacent
                    // dirty updates before storing one, while preserving ordering barriers such
                    // as Clean, Flush, preferences, and shutdown.
                    let (latest, barrier) = coalesce_dirty_updates(&rx, s);
                    pending = barrier;
                    self.latest = Some(*latest);
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
                        Some(s) if self.journal_matches(s) => Ok(Some(self.journaled)),
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
                // A view-only dirty snapshot never becomes an auto-save version: there is no
                // unsaved content to save, and the ring would fill with copies of the project.
                if self.latest.as_ref().is_some_and(|s| s.content_dirty && (s.revision != self.autosaved || s.view_epoch != self.autosaved_view)) {
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
        let metadata_only_view_update = s.content_dirty
            && view_only_journal_update(s.revision, s.view_epoch, s.content_dirty, self.journaled, self.journaled_view, self.journaled_dirty)
            && self.meta.snapshot_fingerprint.is_some();
        let bytes = if metadata_only_view_update {
            None
        } else if s.content_dirty {
            Some(filmcraft_format::encode_with_view(&s.project, Some(&s.view), false))
        } else {
            Some(serde_json::to_vec(&s.view).map_err(|e| format!("recovery view: {e}"))?)
        };
        let serialize_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let t1 = Instant::now();
        let unix = unix_now();
        let mut meta = self.meta.clone();
        meta.saved_unix = unix;
        meta.revision = s.revision;
        meta.project_name = s.project.name.clone();
        meta.project_path = s.path.clone();
        if let Some(bytes) = &bytes {
            meta.bytes = bytes.len();
            meta.snapshot_fingerprint = Some(snapshot_fingerprint(bytes));
            meta.recovery_view = None;
        } else {
            meta.recovery_view = Some(s.view.clone());
        }
        meta.dirty = s.content_dirty;
        meta.view_only = !s.content_dirty;
        meta.project_fingerprint = s.project_fingerprint;
        let r = if let Some(bytes) = &bytes {
            write_recovery_pair(&self.session_dir, bytes, &meta, filmcraft_format::atomic_write)
        } else {
            write_recovery_metadata(&self.session_dir, &meta, filmcraft_format::atomic_write)
        };
        let write_ms = t1.elapsed().as_secs_f64() * 1000.0;
        match r {
            Ok(()) => {
                let snapshot_bytes = bytes.as_ref().map_or(meta.bytes, Vec::len);
                self.meta = meta;
                self.journaled = s.revision;
                self.journaled_view = s.view_epoch;
                self.journaled_dirty = Some(s.content_dirty);
                self.first_pending = None;
                let _ = self.events.send(WorkerEvent::Journaled { revision: s.revision, unix, serialize_ms, write_ms, bytes: snapshot_bytes });
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
        let _ = fs::remove_file(self.session_dir.join(PREVIOUS_META));
        let _ = fs::remove_file(self.session_dir.join(PREVIOUS_SNAPSHOT));
        let _ = fs::remove_file(self.session_dir.join(PENDING_META));
        self.journaled = 0;
        self.journaled_view = 0;
        self.journaled_dirty = None;
    }

    fn auto_save(&mut self) -> Result<PathBuf, String> {
        let Some(s) = &self.latest else { return Err("nothing to save".into()) };
        // A view-only journal entry is not an auto-save version. Keep explicit and periodic saves
        // aligned so a clean project does not rotate its history for Timeline settings alone.
        if !s.content_dirty {
            return Err(String::new());
        }
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
        let bytes = filmcraft_format::encode_with_view(&s.project, Some(&s.view), false);
        let serialize_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let autosave_prefix = format!("{}-", names::file_stem(&name));
        let existing_path = (self.autosaved == s.revision)
            .then(|| self.autosaved_path.as_ref())
            .flatten()
            .filter(|path| {
                path.is_file()
                    && path.parent() == Some(dir.as_path())
                    && path.file_name().is_some_and(|file_name| file_name.to_string_lossy().starts_with(&autosave_prefix))
            })
            .cloned();
        let view_changed = self.autosaved_view != s.view_epoch;
        let t1 = Instant::now();
        let unix = unix_now();
        let (r, wrote_version) = match existing_path {
            Some(path) if view_changed => (filmcraft_format::atomic_write(&path, &bytes).map(|()| path), true),
            Some(path) => (Ok(path), false),
            None => (names::write_auto_save(&dir, &name, unix, (self.local_offset)(unix), &bytes, self.prefs.max_versions as usize), true),
        };
        let write_ms = t1.elapsed().as_secs_f64() * 1000.0;
        match r {
            Ok(path) => {
                self.autosaved = s.revision;
                self.autosaved_view = s.view_epoch;
                self.autosaved_path = Some(path.clone());
                if wrote_version {
                    let _ = self.events.send(WorkerEvent::AutoSaved { path: path.clone(), revision: s.revision, unix, serialize_ms, write_ms, bytes: bytes.len() });
                }
                if self.prefs.save_current_project
                    && let Some(p) = &s.path
                {
                    let fence = self.project_file_fence.lock().unwrap_or_else(PoisonError::into_inner);
                    let is_current = project_snapshot_is_current(p, s.revision, s.view_epoch, &fence);
                    if is_current {
                        match filmcraft_format::atomic_write(Path::new(p), &bytes) {
                            Ok(()) => {
                                let _ = self.events.send(WorkerEvent::SavedProject {
                                    path: p.clone(),
                                    revision: s.revision,
                                    view_epoch: s.view_epoch,
                                    fingerprint: snapshot_fingerprint(&bytes),
                                });
                            }
                            Err(e) => self.error(format!("saving {p}: {e}")),
                        }
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

fn journal_state_matches(revision: u64, view_epoch: u64, dirty: bool, journaled: u64, journaled_view: u64, journaled_dirty: Option<bool>) -> bool {
    revision == journaled && view_epoch == journaled_view && journaled_dirty == Some(dirty)
}

fn view_only_journal_update(revision: u64, view_epoch: u64, dirty: bool, journaled: u64, journaled_view: u64, journaled_dirty: Option<bool>) -> bool {
    revision == journaled && view_epoch != journaled_view && journaled_dirty == Some(dirty)
}

fn project_snapshot_is_current(path: &str, revision: u64, view_epoch: u64, fence: &ProjectFileFence) -> bool {
    fence.path.as_deref() == Some(path) && (revision > fence.revision || (revision == fence.revision && view_epoch >= fence.view_epoch))
}

/// Keep dirty metadata ahead of a dirty snapshot and clean metadata behind a clean snapshot.
/// Otherwise an interruption can leave a dirty snapshot labelled clean and the scan deletes it.
fn write_recovery_pair(dir: &Path, snapshot: &[u8], meta: &JournalMeta, mut write: impl FnMut(&Path, &[u8]) -> io::Result<()>) -> io::Result<()> {
    if snapshot.len() as u64 > filmcraft_format::MAX_PROJECT_FILE_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "recovery snapshot exceeds the project file size limit"));
    }
    let meta_path = dir.join(META);
    let snapshot_path = dir.join(SNAPSHOT);
    let previous_snapshot = dir.join(PREVIOUS_SNAPSHOT);
    let previous_meta = dir.join(PREVIOUS_META);
    let pending_meta = dir.join(PENDING_META);
    if let Ok(old_snapshot) = read_recovery_snapshot(&snapshot_path) {
        // A failed metadata write can leave the durable snapshot paired with pending metadata
        // while the main metadata still describes the previous generation. Preserve whichever
        // record actually matches the snapshot before replacing the pending generation.
        let mut old_meta = metadata_for_snapshot(&old_snapshot, &pending_meta)
            .or_else(|| metadata_for_snapshot(&old_snapshot, &meta_path))
            .unwrap_or(JournalMeta { dirty: true, ..JournalMeta::default() });
        old_meta.bytes = old_snapshot.len();
        old_meta.snapshot_fingerprint = Some(snapshot_fingerprint(&old_snapshot));
        write(&previous_snapshot, &old_snapshot)?;
        write(&previous_meta, &serde_json::to_vec_pretty(&old_meta).map_err(io::Error::other)?)?;
    }
    let mut next = meta.clone();
    next.snapshot_fingerprint = Some(snapshot_fingerprint(snapshot));
    let metadata = serde_json::to_vec_pretty(&next).map_err(io::Error::other)?;
    write(&pending_meta, &metadata)?;
    let result = write(&snapshot_path, snapshot).and_then(|()| write(&meta_path, &metadata));
    if result.is_ok() {
        let _ = fs::remove_file(pending_meta);
        let _ = fs::remove_file(previous_snapshot);
        let _ = fs::remove_file(previous_meta);
    }
    result
}

/// Update recovery metadata without touching a durable project snapshot. The pending metadata is
/// written first so candidate discovery can use either complete metadata record after interruption.
fn write_recovery_metadata(dir: &Path, meta: &JournalMeta, mut write: impl FnMut(&Path, &[u8]) -> io::Result<()>) -> io::Result<()> {
    if !meta.dirty || meta.view_only || meta.recovery_view.is_none() || meta.snapshot_fingerprint.is_none() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "view update has no matching dirty recovery snapshot"));
    }
    let snapshot = fs::metadata(dir.join(SNAPSHOT))?;
    if !snapshot.is_file() || snapshot.len() != meta.bytes as u64 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "dirty recovery snapshot changed before its view update"));
    }
    let metadata = serde_json::to_vec_pretty(meta).map_err(io::Error::other)?;
    if metadata.len() as u64 > MAX_RECOVERY_META_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "recovery metadata exceeds its size limit"));
    }
    let pending_meta = dir.join(PENDING_META);
    write(&pending_meta, &metadata)?;
    let result = write(&dir.join(META), &metadata);
    if result.is_ok() {
        let _ = fs::remove_file(pending_meta);
    }
    result
}

#[cfg(test)]
mod durability_tests {
    use super::*;
    use crate::test_support::TestDir;

    #[test]
    fn recovery_keeps_original_when_worker_has_stopped() {
        let data = TestDir::new("pending-destination");
        let old_dir = recovery_root(&data).join("1-1");
        fs::create_dir_all(&old_dir).unwrap();
        let bytes = filmcraft_format::encode(&Project::default(), false);
        let original = old_dir.join(SNAPSHOT);
        fs::write(&original, &bytes).unwrap();

        let mut session = crate::Session::default();
        session.start_autosave(AutosaveConfig::new(data.as_ref().to_path_buf())).unwrap();
        session.persistence.as_mut().unwrap().stop_worker();
        let result = session.execute("file.recover", json!({"id": "1-1"}));

        assert!(original.is_file(), "a stopped worker must not consume the original snapshot");
        assert_eq!(fs::read(&original).unwrap(), bytes);
        assert_eq!(session.recovery_candidates().len(), 1);
        assert!(result.unwrap_err().to_string().contains("worker stopped"));
        session.shutdown();
    }

    #[test]
    fn queued_view_updates_coalesce_without_crossing_a_clean_barrier() {
        let (tx, rx) = channel();
        let snapshot = |name: &str, view_epoch| {
            Box::new(Snapshot {
                project: Arc::new(Project::new(name)),
                view: filmcraft_project::ProjectView::default(),
                revision: 4,
                view_epoch,
                content_dirty: true,
                path: None,
                project_fingerprint: None,
            })
        };
        assert!(tx.send(Msg::Dirty(snapshot("middle", 2))).is_ok());
        assert!(tx.send(Msg::Dirty(snapshot("latest", 3))).is_ok());
        assert!(tx.send(Msg::Clean).is_ok());
        assert!(tx.send(Msg::Dirty(snapshot("after clean", 4))).is_ok());

        let (latest, barrier) = coalesce_dirty_updates(&rx, snapshot("oldest", 1));

        assert_eq!(latest.view_epoch, 3, "only the newest adjacent full view snapshot should be retained");
        assert_eq!(latest.project.name, "latest");
        assert!(matches!(barrier, Some(Msg::Clean)), "coalescing must stop at ordering-sensitive messages");
        assert!(matches!(rx.try_recv(), Ok(Msg::Dirty(_))), "updates after the barrier must remain queued");
    }

    #[test]
    fn interrupted_dirty_view_metadata_update_keeps_the_snapshot_and_latest_view_recoverable() {
        let temp = tempfile::tempdir().unwrap();
        let session = recovery_root(temp.path()).join("1-1");
        fs::create_dir_all(&session).unwrap();
        let project = Project::new("Unsaved project content");
        let original_view = filmcraft_project::ProjectView::default();
        let snapshot = filmcraft_format::encode_with_view(&project, Some(&original_view), false);
        write_recovery_pair(
            &session,
            &snapshot,
            &JournalMeta { dirty: true, bytes: snapshot.len(), project_name: project.name.clone(), ..JournalMeta::default() },
            filmcraft_format::atomic_write,
        )
        .unwrap();
        let mut latest_view = original_view;
        latest_view.timeline_ui.inverted_controls = true;
        let mut metadata = metadata_for_snapshot(&snapshot, &session.join(META)).unwrap();
        metadata.recovery_view = Some(latest_view.clone());
        metadata.saved_unix += 1;

        let result = write_recovery_metadata(&session, &metadata, |path, bytes| {
            if path == session.join(META) {
                return Err(io::Error::other("interrupted after pending view metadata"));
            }
            filmcraft_format::atomic_write(path, bytes)
        });
        assert!(result.is_err());
        assert_eq!(fs::read(session.join(SNAPSHOT)).unwrap(), snapshot, "a view-only update must not rewrite dirty content");

        let candidates = scan_candidates(temp.path(), None);
        assert_eq!(candidates.len(), 1, "the pending metadata still matches the unchanged dirty snapshot");
        let recovered = load_candidate(&candidates[0]).unwrap();
        assert_eq!(recovered.project.name, project.name);
        assert_eq!(recovered.view, Some(latest_view));
    }

    #[test]
    fn recovery_snapshots_and_metadata_are_bounded_before_reading() {
        let dir = std::env::temp_dir().join(format!("fc-recovery-read-limits-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let snapshot = dir.join(SNAPSHOT);
        let metadata = dir.join(META);
        File::create(&snapshot).unwrap().set_len(filmcraft_format::MAX_PROJECT_FILE_BYTES + 1).unwrap();
        File::create(&metadata).unwrap().set_len(MAX_RECOVERY_META_BYTES + 1).unwrap();

        assert!(read_recovery_snapshot(&snapshot).is_err());
        assert!(read_recovery_metadata(&metadata).is_err());
        assert!(matching_journal_pair(&snapshot, &metadata).is_none());
        let candidate = RecoveryCandidate { id: "oversized".into(), dir: dir.clone(), snapshot_path: snapshot, meta: JournalMeta::default() };
        assert!(load_candidate(&candidate).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn lock_errors_do_not_classify_sessions_as_stale() {
        assert_eq!(classify_lock_error(fs::TryLockError::Error(io::Error::other("locking is unsupported"))), LockState::Unknown);
        assert_eq!(classify_lock_error(fs::TryLockError::WouldBlock), LockState::Active);
    }

    #[test]
    fn interrupted_dirty_snapshot_is_not_discarded_as_clean() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        let session = recovery_root(dir).join("1-1");
        fs::create_dir_all(&session).unwrap();
        let clean_snapshot = serde_json::to_vec(&filmcraft_project::ProjectView::default()).unwrap();
        write_recovery_pair(
            &session,
            &clean_snapshot,
            &JournalMeta { dirty: false, view_only: true, ..JournalMeta::default() },
            filmcraft_format::atomic_write,
        )
        .unwrap();
        let edited_project = Project::new("Unsaved content after clean view change");
        let edited_snapshot = filmcraft_format::encode_with_view(&edited_project, Some(&filmcraft_project::ProjectView::default()), false);
        let result = write_recovery_pair(&session, &edited_snapshot, &JournalMeta { dirty: true, ..JournalMeta::default() }, |path, bytes| {
            filmcraft_format::atomic_write(path, bytes)?;
            if path.ends_with(SNAPSHOT) { Err(io::Error::other("interrupted")) } else { Ok(()) }
        });
        assert!(result.is_err());
        let candidates = scan_candidates(dir, None);
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].meta.dirty, "recovery metadata must identify the replacement as unsaved");
        assert_eq!(load_candidate(&candidates[0]).unwrap().project.name, edited_project.name);
    }

    #[test]
    fn candidate_scans_are_read_only_and_idempotent_for_clean_journals() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        let session = recovery_root(dir).join("1-1");
        fs::create_dir_all(&session).unwrap();
        let snapshot = filmcraft_format::encode(&Project::default(), false);
        write_recovery_pair(&session, &snapshot, &JournalMeta { dirty: false, view_only: true, ..JournalMeta::default() }, filmcraft_format::atomic_write)
            .unwrap();

        assert!(scan_candidates(dir, None).is_empty());
        assert!(session.join(SNAPSHOT).is_file(), "candidate discovery must not delete clean view-only journals");
        assert!(scan_candidates(dir, None).is_empty());
        assert!(session.join(SNAPSHOT).is_file(), "repeated scans must remain read-only");
        cleanup_stale_clean_sessions(dir, None);
        assert!(!session.exists(), "the explicit stale-session cleanup removes clean view-only journals");
    }

    #[test]
    fn interrupted_dirty_replacement_keeps_the_last_good_snapshot_recoverable() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        let session = recovery_root(dir).join("1-1");
        fs::create_dir_all(&session).unwrap();
        let original = Project::new("Last good edit");
        let original_bytes = filmcraft_format::encode(&original, false);
        let original_meta = JournalMeta { dirty: true, ..JournalMeta::default() };
        write_recovery_pair(&session, &original_bytes, &original_meta, filmcraft_format::atomic_write).unwrap();

        let next = Project::new("Interrupted edit");
        let next_bytes = filmcraft_format::encode(&next, false);
        let result = write_recovery_pair(&session, &next_bytes, &JournalMeta { dirty: true, ..JournalMeta::default() }, |path, bytes| {
            if path == session.join(SNAPSHOT) {
                return Err(io::Error::other("interrupted before snapshot replacement"));
            }
            filmcraft_format::atomic_write(path, bytes)
        });
        assert!(result.is_err());

        let candidates = scan_candidates(dir, None);
        assert_eq!(candidates.len(), 1, "the previously committed dirty pair remains a recovery candidate");
        let recovered = load_candidate(&candidates[0]).unwrap();
        assert_eq!(recovered.project.name, original.name);
    }

    #[test]
    fn successive_interrupted_writes_keep_the_latest_durable_snapshot_recoverable() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        let session = recovery_root(dir).join("1-1");
        fs::create_dir_all(&session).unwrap();

        let original = Project::new("Snapshot A");
        write_recovery_pair(
            &session,
            &filmcraft_format::encode(&original, false),
            &JournalMeta { dirty: true, ..JournalMeta::default() },
            filmcraft_format::atomic_write,
        )
        .unwrap();

        let durable = Project::new("Snapshot B");
        let durable_bytes = filmcraft_format::encode(&durable, false);
        let interrupted_meta = write_recovery_pair(
            &session,
            &durable_bytes,
            &JournalMeta { dirty: true, project_path: Some("b.fcproj".into()), ..JournalMeta::default() },
            |path, bytes| {
                if path == session.join(META) {
                    return Err(io::Error::other("interrupted after snapshot replacement"));
                }
                filmcraft_format::atomic_write(path, bytes)
            },
        );
        assert!(interrupted_meta.is_err());

        let third = Project::new("Snapshot C");
        let interrupted_snapshot = write_recovery_pair(
            &session,
            &filmcraft_format::encode(&third, false),
            &JournalMeta { dirty: true, project_path: Some("c.fcproj".into()), ..JournalMeta::default() },
            |path, bytes| {
                if path == session.join(SNAPSHOT) {
                    return Err(io::Error::other("interrupted before snapshot replacement"));
                }
                filmcraft_format::atomic_write(path, bytes)
            },
        );
        assert!(interrupted_snapshot.is_err());

        let candidates = scan_candidates(dir, None);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].meta.project_path.as_deref(), Some("b.fcproj"));
        assert_eq!(load_candidate(&candidates[0]).unwrap().project.name, durable.name);
    }

    #[test]
    fn missing_metadata_keeps_the_original_snapshot_and_does_not_adopt_a_new_path() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        let session = recovery_root(dir).join("1-1");
        fs::create_dir_all(&session).unwrap();
        let old = Project::new("Original");
        let old_bytes = filmcraft_format::encode(&old, false);
        fs::write(session.join(SNAPSHOT), &old_bytes).unwrap();
        let replacement = filmcraft_format::encode(&Project::new("Replacement"), false);
        let meta = JournalMeta { project_path: Some("replacement.fcproj".into()), dirty: true, ..JournalMeta::default() };
        let result = write_recovery_pair(&session, &replacement, &meta, |path, bytes| {
            if path == session.join(SNAPSHOT) {
                return Err(io::Error::other("interrupted before snapshot replacement"));
            }
            filmcraft_format::atomic_write(path, bytes)
        });
        assert!(result.is_err());
        let candidates = scan_candidates(dir, None);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].meta.project_path, None, "a metadata-less snapshot must not inherit the pending destination");
        let loaded = load_candidate(&candidates[0]).unwrap();
        assert_eq!(loaded.project.name, "Original");
    }
}

#[cfg(test)]
mod project_file_fence_tests {
    use super::*;
    use crate::Services;
    use std::sync::atomic::AtomicBool;

    fn utc_offset(_: i64) -> i32 {
        0
    }

    struct BlockProjectRead {
        path: String,
        entered: Sender<()>,
        release: Mutex<Receiver<()>>,
        blocked: AtomicBool,
    }

    impl Services for BlockProjectRead {
        fn read_file(&self, path: &str) -> io::Result<Vec<u8>> {
            if path == self.path && !self.blocked.swap(true, Ordering::AcqRel) {
                let _ = self.entered.send(());
                self.release.lock().unwrap_or_else(PoisonError::into_inner).recv().map_err(|_| io::Error::other("test read gate closed"))?;
            }
            crate::FsServices.read_file(path)
        }

        fn write_file(&self, path: &str, data: &[u8]) -> io::Result<()> {
            crate::FsServices.write_file(path, data)
        }

        fn file_size(&self, path: &str) -> io::Result<u64> {
            crate::FsServices.file_size(path)
        }
    }

    #[test]
    fn revert_fences_a_pending_autosave_until_the_opened_file_is_installed() {
        let temp = tempfile::tempdir().unwrap();
        let project_path = temp.path().join("project.fcproj");
        let project_path = project_path.to_string_lossy().into_owned();
        let mut session = crate::Session::default();
        session.execute("file.openDemoProject", json!({})).unwrap();
        session.execute("file.saveAs", json!({"path": project_path})).unwrap();
        let saved_bytes = fs::read(&project_path).unwrap();
        session.start_autosave(AutosaveConfig::new(temp.path().join("autosave"))).unwrap();
        let mut prefs = session.prefs.clone();
        prefs.auto_save.save_current_project = true;
        session.set_prefs(prefs).unwrap();
        session.execute("sequence.addEdit", serde_json::json!({"seconds": 1.0})).unwrap();

        let (entered_tx, entered_rx) = channel();
        let (release_tx, release_rx) = channel();
        session.services =
            Arc::new(BlockProjectRead { path: project_path.clone(), entered: entered_tx, release: Mutex::new(release_rx), blocked: AtomicBool::new(false) });
        let worker_tx = session.persistence.as_ref().unwrap().tx.clone();
        let (auto_save_tx, auto_save_rx) = channel();
        let opened = std::thread::spawn(move || {
            let result = session.execute("file.revert", json!({}));
            (session, result)
        });

        entered_rx.recv_timeout(Duration::from_secs(10)).expect("revert acquired the project-file fence before reading");
        assert!(worker_tx.send(Msg::AutoSaveNow(auto_save_tx)).is_ok(), "autosave worker is available");
        let autosave_dir = names::auto_save_dir(Path::new(&project_path));
        let deadline = Instant::now() + Duration::from_secs(10);
        while !fs::read_dir(&autosave_dir).is_ok_and(|mut entries| entries.next().is_some()) {
            assert!(Instant::now() < deadline, "pending autosave did not reach the project-file fence");
            std::thread::sleep(Duration::from_millis(10));
        }
        release_tx.send(()).unwrap();
        let (mut session, result) = opened.join().unwrap();
        result.unwrap();
        auto_save_rx.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();

        let disk_bytes = fs::read(&project_path).unwrap();
        assert_eq!(disk_bytes, saved_bytes, "the pending autosave must not overwrite the file after revert installs it");
        let loaded = filmcraft_format::decode(&disk_bytes).unwrap();
        assert_eq!(loaded.project, *session.project, "reopened state must match the bytes still present on disk");
        session.shutdown();
    }

    #[test]
    fn stale_snapshot_cannot_write_after_save_or_document_replacement() {
        let fence = ProjectFileFence { revision: 10, view_epoch: 7, path: Some("project.fcproj".into()), fingerprint: None };
        assert!(!project_snapshot_is_current("project.fcproj", 9, 20, &fence), "older content must not replace a save");
        assert!(!project_snapshot_is_current("project.fcproj", 10, 6, &fence), "older view state must not replace a saved view");
        assert!(!project_snapshot_is_current("other.fcproj", 12, 8, &fence), "a replaced document's destination must not be written");
        assert!(project_snapshot_is_current("project.fcproj", 10, 7, &fence));
        assert!(project_snapshot_is_current("project.fcproj", 11, 7, &fence));
    }

    #[test]
    fn a_view_change_updates_the_existing_auto_save_version() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().to_path_buf();
        let (events, _received) = channel();
        let mut worker = Worker {
            data_dir: dir.clone(),
            session_dir: dir.join("session"),
            local_offset: utc_offset,
            debounce: Duration::from_secs(1),
            prefs: AutoSavePrefs::default(),
            events,
            latest: Some(Snapshot {
                project: Arc::new(Project::new("View update")),
                view: filmcraft_project::ProjectView::default(),
                revision: 4,
                view_epoch: 1,
                content_dirty: true,
                path: None,
                project_fingerprint: None,
            }),
            journaled: 0,
            journaled_view: 0,
            journaled_dirty: None,
            autosaved: 0,
            autosaved_view: 0,
            autosaved_path: None,
            first_pending: None,
            last_change: Instant::now(),
            next_autosave: Instant::now(),
            meta: JournalMeta::default(),
            project_file_fence: Arc::new(Mutex::new(ProjectFileFence::default())),
        };

        let first = worker.auto_save().unwrap();
        let latest = worker.latest.as_mut().unwrap();
        latest.view_epoch = 2;
        latest.view.timeline_ui.zoom_bar_hidden = true;
        let second = worker.auto_save().unwrap();
        let retry = worker.auto_save().unwrap();

        assert_eq!(second, first, "a view-only refresh updates the existing version");
        assert_eq!(retry, first, "retrying an already autosaved revision does not create another version");
        assert_eq!(names::list_auto_saves(&dir.join(names::AUTO_SAVE_DIR), "View update"), [first.clone()]);
        let saved = filmcraft_format::decode(&fs::read(first).unwrap()).unwrap();
        assert!(saved.view.unwrap().timeline_ui.zoom_bar_hidden, "the existing file contains the latest Timeline settings");
    }

    #[test]
    fn autosave_worker_skips_a_snapshot_older_than_the_saved_file_fence() {
        let dir = std::env::temp_dir().join(format!("fc-project-file-fence-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("project.fcproj");
        let current = Project::new("Newer saved project");
        let current_bytes = filmcraft_format::encode(&current, false);
        fs::write(&path, &current_bytes).unwrap();
        let project_path = path.to_string_lossy().into_owned();
        let fence = Arc::new(Mutex::new(ProjectFileFence { revision: 2, view_epoch: 1, path: Some(project_path.clone()), fingerprint: None }));
        let (events, received) = channel();
        let prefs = AutoSavePrefs { save_current_project: true, ..AutoSavePrefs::default() };
        let mut worker = Worker {
            data_dir: dir.clone(),
            session_dir: dir.join("session"),
            local_offset: utc_offset,
            debounce: Duration::from_secs(1),
            prefs,
            events,
            latest: Some(Snapshot {
                project: Arc::new(Project::new("Stale worker snapshot")),
                view: filmcraft_project::ProjectView::default(),
                revision: 1,
                view_epoch: 1,
                content_dirty: true,
                path: Some(project_path),
                project_fingerprint: None,
            }),
            journaled: 0,
            journaled_view: 0,
            journaled_dirty: None,
            autosaved: 0,
            autosaved_view: 0,
            autosaved_path: None,
            first_pending: None,
            last_change: Instant::now(),
            next_autosave: Instant::now(),
            meta: JournalMeta::default(),
            project_file_fence: fence,
        };

        worker.auto_save().unwrap();

        assert_eq!(fs::read(&path).unwrap(), current_bytes, "a stale worker snapshot must not overwrite the newer project file");
        assert!(!received.try_iter().any(|event| matches!(event, WorkerEvent::SavedProject { .. })));
        fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod journal_baseline_tests {
    use super::*;

    #[test]
    fn a_changed_dirty_baseline_requires_a_journal_refresh() {
        assert!(journal_state_matches(12, 4, true, 12, 4, Some(true)));
        assert!(!journal_state_matches(12, 4, false, 12, 4, Some(true)), "a save can change dirtiness without changing either counter");
        assert!(!journal_state_matches(12, 4, true, 12, 4, None), "an unrecorded journal has no matching state");
    }

    #[test]
    fn journals_without_a_dirty_field_remain_recoverable_for_compatibility() {
        let metadata: JournalMeta = serde_json::from_str(r#"{"revision":7,"viewOnly":true}"#).unwrap();
        assert!(metadata.dirty);
    }

    #[test]
    fn a_view_only_journal_update_is_detected_at_the_same_content_revision() {
        assert!(view_only_journal_update(12, 5, true, 12, 4, Some(true)));
        assert!(view_only_journal_update(12, 5, false, 12, 4, Some(false)));
        assert!(!view_only_journal_update(13, 5, true, 12, 4, Some(true)));
        assert!(!view_only_journal_update(12, 4, true, 12, 4, Some(true)));
        assert!(!view_only_journal_update(12, 5, false, 12, 4, Some(true)));
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

    #[test]
    fn explicit_auto_save_does_not_rotate_versions_for_a_clean_view_change() {
        let dir = std::env::temp_dir().join(format!("fc-worker-clean-view-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let mut prefs = Preferences::default();
        prefs.auto_save.recovery_journal = false;
        prefs.save(&dir.join("preferences.json")).unwrap();
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        s.start_autosave(AutosaveConfig::new(&dir)).unwrap();
        s.view_changed();
        s.sync_persistence();

        let result = s.execute("file.autoSaveNow", json!({})).unwrap();
        assert!(result["path"].is_null(), "a view-only change keeps the explicit autosave no-op result");
        assert!(!dir.join(names::AUTO_SAVE_DIR).exists(), "a clean view-only snapshot must not create an autosave version");
        s.shutdown();
        fs::remove_dir_all(dir).unwrap();
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
