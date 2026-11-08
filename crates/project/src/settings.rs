use super::{Deserialize, Serialize, TICKS_PER_SECOND, Tick};

/// Project-level settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectSettings {
    pub renderer: String,
    pub video_display: filmcraft_time::TimeDisplay,
    pub audio_display_samples: bool,
    /// Kept for file compatibility; the editor uses Settings ▸ Timeline (user preferences) for
    /// still and transition default durations, as Premiere does.
    pub default_still_duration: Tick,
    pub default_transition_duration_frames: i64,
    pub default_audio_transition_duration: Tick,
    /// Project Settings ▸ Ingest Settings: what happens to media on import.
    #[serde(default)]
    pub ingest: IngestSettings,
    /// Project Settings ▸ General ▸ Action and Title Safe Areas: (horizontal, vertical) percent.
    #[serde(default = "title_safe_default")]
    pub title_safe: (f64, f64),
    #[serde(default = "action_safe_default")]
    pub action_safe: (f64, f64),
    /// Project Settings ▸ General ▸ Capture Format (`DV` or `HDV`; informational, FilmCraft has no
    /// tape capture). Schema v11.
    #[serde(default = "capture_format_default")]
    pub capture_format: String,
    /// Project Settings ▸ Scratch Disks. Schema v11.
    #[serde(default)]
    pub scratch: ScratchDisks,
}

fn title_safe_default() -> (f64, f64) {
    (20.0, 20.0)
}

fn action_safe_default() -> (f64, f64) {
    (10.0, 10.0)
}

fn capture_format_default() -> String {
    "DV".into()
}

/// Project Settings ▸ Scratch Disks: where generated files go. `None` = Same as Project (next to
/// the project file; the data directory for an unsaved project).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ScratchDisks {
    /// Captured and Generated media: proxies (when no ingest destination is set), Extract Audio.
    pub captured: Option<String>,
    /// Video and audio render previews (`<dir>/<project name>`).
    pub video_previews: Option<String>,
    pub audio_previews: Option<String>,
    /// Project Auto Save versions.
    pub auto_save: Option<String>,
}

/// Ingest on import (Project Settings ▸ Ingest Settings).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct IngestSettings {
    pub enabled: bool,
    pub action: IngestAction,
    /// Destination folder for copies, transcodes and proxies (None = next to the media:
    /// `<media dir>/Proxies` for proxies, `<media dir>/Ingest` for copies and transcodes).
    pub destination: Option<String>,
    /// Proxy / transcode preset id (the engine's `proxies::PRESETS`); empty = the default.
    pub preset: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IngestAction {
    /// Copy the files to the destination (verified), then use the copies.
    Copy,
    /// Transcode to the preset's format, then use the transcodes.
    Transcode,
    /// Create proxies (in the background) and attach them.
    #[default]
    CreateProxies,
    /// Copy, then create proxies of the copies.
    CopyAndCreateProxies,
}

impl Default for ProjectSettings {
    fn default() -> Self {
        Self {
            renderer: "FilmCraft GPU Acceleration (wgpu)".into(),
            video_display: filmcraft_time::TimeDisplay::Timecode,
            audio_display_samples: true,
            default_still_duration: Tick(5 * TICKS_PER_SECOND),
            default_transition_duration_frames: 24,
            default_audio_transition_duration: Tick(TICKS_PER_SECOND),
            ingest: IngestSettings::default(),
            title_safe: title_safe_default(),
            action_safe: action_safe_default(),
            capture_format: capture_format_default(),
            scratch: ScratchDisks::default(),
        }
    }
}

impl ProjectSettings {
    /// Width and height of the title-safe box as fractions of the frame. Each percent is the
    /// margin taken from that axis, half on each side, so the default 20 % leaves 0.8.
    pub fn title_safe_box(&self) -> (f64, f64) {
        (safe_fraction(self.title_safe.0), safe_fraction(self.title_safe.1))
    }

    /// Width and height of the action-safe box as fractions of the frame (default 10 %, 0.9).
    pub fn action_safe_box(&self) -> (f64, f64) {
        (safe_fraction(self.action_safe.0), safe_fraction(self.action_safe.1))
    }
}

/// The part of an axis left inside a safe margin of `percent`, held to the Project Settings range
/// (0–50 %) because a project file can carry any number.
fn safe_fraction(percent: f64) -> f64 {
    let p = if percent.is_finite() { percent.clamp(0.0, 50.0) } else { 0.0 };
    1.0 - p / 100.0
}
