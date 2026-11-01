//! Export mode (header "Export"), laid out like Premiere 26: Destinations and the export queue on
//! the left; the Media File settings in the middle (File Name, Location, Preset, Format, then the
//! Video, Audio, Multiplexer, Captions, Effects and Metadata sections); the preview, Range,
//! Scaling, the Summary with the estimated file size, and Send to Queue / Export on the right.
//! Also the Preset Manager (Preset ▸ More presets…) and the header's Quick Export popup.
//!
//! Every setting lives in [`ExportUi`] (serde, `ui.inspect` / `ui.set {"export": {…}}`) and every
//! action runs an engine command (`file.exportMedia`, `export.queue.*`, `export.presets.*`,
//! `export.quick`).
//!
//! Automation ids: `export.fileName`, `export.location`, `export.preset`, `export.preset.more`,
//! `export.format`, `export.section.<video|audio|multiplexer|captions|effects|metadata>`,
//! `export.video.*`, `export.audio.*`, `export.effects.*`, `export.metadata.*`, `export.range`,
//! `export.scaling`, `export.summary`, `export.estimate`, `export.sendToQueue`, `export.button`,
//! `export.queue.start|stop|clear`, `export.queue.item.<id>` and `….<id>.<up|down|cancel|retry|remove>`;
//! Preset Manager: `presetManager.search`, `presetManager.favoritesOnly`,
//! `presetManager.item.<name>`, `presetManager.favorite.<name>`, `presetManager.saveName`,
//! `presetManager.save`, `presetManager.delete`, `presetManager.import`, `presetManager.export`,
//! `presetManager.ok`, `presetManager.cancel`; Quick Export: `quickExport.path` (the file name),
//! `quickExport.location` (the folder; opens the save dialog), `quickExport.preset.<name>`,
//! `quickExport.go`, `quickExport.close`.

use egui::{Align2, Color32, Rect, Sense, pos2, vec2};
use filmcraft_engine::export::presets::{DEFAULT_PRESET, preset_key};
use filmcraft_engine::export::{
    AudioCodec, BitrateMode, ExportSettings, ExrCompression, FieldOrder, Format, GpuRendering, H264Profile, HardwareEncoding, Multiplexer, MxfVideoCodec,
    Placement, SampleFormat, Scaling, TextOverlay, builtin_presets, format_bytes,
};
use filmcraft_engine::time::{FrameRate, Tick};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::frames::{FrameKey, Target};
use crate::native_dialogs::FileDialog;
use crate::theme::Tokens;

mod presets;
mod preview;
mod queue;
mod quick;
mod settings;

use presets::preset_manager;
#[cfg(test)]
pub(crate) use preview::RANGES;
use preview::preview_column;
use queue::queue_column;
pub use quick::quick_export;
#[cfg(test)]
pub(crate) use settings::PARS;
use settings::settings_column;
#[cfg(test)]
use settings::{MAX_LOCATION_DISPLAY_BYTES, location_tail};

/// Export mode state.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ExportUi {
    /// The preset the settings came from ("Custom" once edited).
    pub preset: String,
    pub settings: ExportSettings,
    /// File name without extension, and the folder.
    pub file_name: String,
    pub location: String,
    /// `entire` | `inOut` | `workArea` | `custom`.
    pub range: String,
    pub custom_start: f64,
    pub custom_end: f64,
    /// Expanded settings sections.
    pub open_sections: Vec<String>,
    /// The sequence the file name was taken from.
    pub for_sequence: Option<u64>,
    /// Preset Manager (None = closed).
    pub manager: Option<PresetManager>,
    /// Quick Export popup.
    pub quick_open: bool,
    pub quick_path: String,
    pub quick_preset: String,
    /// The sequence and its In/Out marks the Range was last chosen for: when either changes, the
    /// Range follows them again (Source In/Out with marks set, else Entire Source; #2749). Part of
    /// the state, so a `ui.set` of other export fields keeps it (and a chosen Range stays).
    pub range_for: Option<(u64, Option<i64>, Option<i64>)>,
}

impl Default for ExportUi {
    fn default() -> Self {
        let preset = filmcraft_engine::export::presets::find_builtin(DEFAULT_PRESET).map(|p| p.settings).unwrap_or_default();
        ExportUi {
            preset: DEFAULT_PRESET.into(),
            settings: preset,
            file_name: String::new(),
            location: String::new(),
            range: "entire".into(),
            custom_start: 0.0,
            custom_end: 10.0,
            open_sections: vec!["video".into(), "audio".into()],
            for_sequence: None,
            manager: None,
            quick_open: false,
            quick_path: String::new(),
            quick_preset: String::new(),
            range_for: None,
        }
    }
}

/// The Preset Manager dialog.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PresetManager {
    pub query: String,
    pub favorites_only: bool,
    pub selected: String,
    pub save_name: String,
}

pub const CUSTOM: &str = "Custom";

/// Collected automation ids (registered after drawing, so widgets can borrow the app freely).
#[derive(Default)]
struct Reg(Vec<(String, Rect, String)>);

impl Reg {
    fn add(&mut self, id: impl Into<String>, r: Rect, label: impl Into<String>) {
        self.0.push((id.into(), r, label.into()));
    }
    fn flush(self, app: &mut FilmcraftApp) {
        for (id, r, l) in self.0 {
            app.auto.add(&id, r, &l);
        }
    }
}

fn settings_json(s: &ExportSettings) -> Value {
    serde_json::to_value(s).unwrap_or_default()
}

/// Apply a preset by name (from the engine's library).
pub fn apply_preset(app: &mut FilmcraftApp, name: &str) -> bool {
    match app.session.export_presets.find(name) {
        Some(p) => {
            app.ui.export.settings = p.settings;
            app.ui.export.preset = p.name;
            true
        }
        None => {
            app.ui.status = tlf!("No export preset named `{name}`", name);
            false
        }
    }
}

/// Full output path of the current settings.
pub fn output_path(ex: &ExportUi) -> String {
    let name = if ex.file_name.trim().is_empty() { "Export" } else { ex.file_name.trim() };
    let dir = filmcraft_engine::export_tools::expand_home(ex.location.trim());
    let file = format!("{name}.{}", ex.settings.extension());
    if dir.is_empty() { file } else { std::path::Path::new(&dir).join(file).to_string_lossy().to_string() }
}

/// Engine params for the current settings (`file.exportMedia` / `export.queue.add`).
pub fn export_params(app: &FilmcraftApp) -> Value {
    let ex = &app.ui.export;
    let mut p = json!({"path": output_path(ex), "settings": settings_json(&ex.settings), "range": ex.range});
    if ex.range == "custom" {
        p["startSeconds"] = json!(ex.custom_start);
        p["endSeconds"] = json!(ex.custom_end);
    }
    p
}

fn sync_sequence(app: &mut FilmcraftApp) {
    let Some(seq) = app.session.state.active_sequence else { return };
    if app.ui.export.for_sequence != Some(seq.0) || app.ui.export.file_name.is_empty() {
        app.ui.export.file_name = app.session.project.item(seq).map(|i| i.name.clone()).unwrap_or_else(|| "Sequence".into());
        app.ui.export.for_sequence = Some(seq.0);
    }
    if app.ui.export.location.is_empty() {
        app.ui.export.location = filmcraft_engine::export_tools::default_export_dir(&app.session).to_string_lossy().to_string();
    }
    // As Premiere does: a sequence with In/Out marks exports that range unless another is chosen.
    // Only when the sequence or its marks change, so a Range picked by hand (or `ui.set`) stays.
    let marks = app.session.project.sequence(seq).map(|q| (q.mark_in.map(|t| t.0), q.mark_out.map(|t| t.0))).unwrap_or_default();
    let now = Some((seq.0, marks.0, marks.1));
    if app.ui.export.range_for != now {
        app.ui.export.range_for = now;
        app.ui.export.range = if marks.0.is_some() || marks.1.is_some() { "inOut" } else { "entire" }.into();
    }
}

pub(super) fn active_export_job(app: &FilmcraftApp) -> Option<filmcraft_engine::Job> {
    app.session
        .jobs
        .iter()
        .find(|j| !j.progress.finished.load(std::sync::atomic::Ordering::Relaxed) && (j.label.starts_with("Export") || j.label.starts_with("Quick Export")))
        .cloned()
}

pub(super) fn is_export_busy(app: &FilmcraftApp) -> bool {
    active_export_job(app).is_some() || app.session.export_queue.is_active()
}

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let left = Rect::from_min_size(rect.min, vec2(300.0, rect.height()));
    let right = Rect::from_min_max(pos2(rect.max.x - (rect.width() * 0.4).clamp(360.0, 640.0), rect.min.y), rect.max);
    let mid = Rect::from_min_max(pos2(left.max.x + 4.0, rect.min.y), pos2(right.min.x - 4.0, rect.max.y));
    for r in [left, mid, right] {
        ui.painter().rect_filled(r, t.radius, t.panel_bg);
    }
    let Some(seq_id) = app.session.state.active_sequence else {
        crate::dock::placeholder(ui, mid, &t, tl!("Open a sequence to export"));
        queue_column(app, ui, left);
        return;
    };
    sync_sequence(app);
    let before = settings_json(&app.ui.export.settings);
    queue_column(app, ui, left);
    settings_column(app, ui, mid);
    preview_column(app, ui, right, seq_id);
    // editing any setting turns the preset into "Custom"
    if settings_json(&app.ui.export.settings) != before
        && app.ui.export.preset != CUSTOM
        && app.session.export_presets.find(&app.ui.export.preset).is_none_or(|p| settings_json(&p.settings) != settings_json(&app.ui.export.settings))
    {
        app.ui.export.preset = CUSTOM.into();
    }
    preset_manager(app, ui.ctx());
    if app.session.export_queue.is_active() || app.session.jobs.iter().any(|j| !j.progress.finished.load(std::sync::atomic::Ordering::Relaxed)) {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
    }
}

#[cfg(test)]
mod location_tests {
    use super::*;

    #[test]
    fn location_tail_is_bounded_and_keeps_utf8_intact() {
        let input = format!("/{}終", "é".repeat(MAX_LOCATION_DISPLAY_BYTES));
        let (tail, truncated) = location_tail(&input);
        assert!(truncated);
        assert!(tail.len() <= MAX_LOCATION_DISPLAY_BYTES);
        assert!(tail.ends_with('終'));
        assert!(std::str::from_utf8(tail.as_bytes()).is_ok());
    }
}
