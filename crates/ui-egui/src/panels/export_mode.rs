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
//! `presetManager.ok`, `presetManager.cancel`; Quick Export: `quickExport.path`,
//! `quickExport.preset.<name>`, `quickExport.go`, `quickExport.close`.

use egui::{Align2, Color32, Rect, Sense, pos2, vec2};
use filmcraft_engine::export::presets::{DEFAULT_PRESET, preset_key};
use filmcraft_engine::export::{
    AudioCodec, BitrateMode, ExportSettings, FieldOrder, Format, GpuRendering, H264Profile, HardwareEncoding, Multiplexer, MxfVideoCodec, Placement, Scaling,
    TextOverlay, builtin_presets, format_bytes,
};
use filmcraft_engine::time::{FrameRate, Tick};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::frames::{FrameKey, Target};
use crate::theme::Tokens;

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

// ---------------------------------------------------------------------------------------------
// left: destinations + queue
// ---------------------------------------------------------------------------------------------

fn queue_column(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let mut reg = Reg::default();
    let p = ui.painter();
    p.text(rect.min + vec2(14.0, 20.0), Align2::LEFT_CENTER, tl!("Destinations"), Tokens::semibold(13.0), t.text);
    let mf = Rect::from_min_size(rect.min + vec2(8.0, 38.0), vec2(rect.width() - 16.0, 26.0));
    p.rect_filled(mf, 4.0, t.row_selected);
    p.text(pos2(mf.min.x + 10.0, mf.center().y), Align2::LEFT_CENTER, tl!("Media File"), Tokens::ui(12.5), t.text);
    reg.add("export.destination.mediaFile", mf, "Media File");
    let qtop = mf.max.y + 18.0;
    let n = app.session.export_queue.items.len();
    p.text(pos2(rect.min.x + 14.0, qtop), Align2::LEFT_CENTER, tlf!("Queue ({n})", n), Tokens::semibold(13.0), t.text);
    let body = Rect::from_min_max(pos2(rect.min.x + 8.0, qtop + 14.0), pos2(rect.max.x - 8.0, rect.max.y - 40.0));
    let items: Vec<Value> = app.session.execute("export.queue.list", json!({})).ok().and_then(|v| v["items"].as_array().cloned()).unwrap_or_default();
    let mut action: Option<(&'static str, Value)> = None;
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body).id_salt("export-queue"));
    egui::ScrollArea::vertical().id_salt("export-queue-scroll").auto_shrink([false, false]).show(&mut child, |ui| {
        if items.is_empty() {
            ui.label(egui::RichText::new(tl!("Send exports here with Send to Queue, then start the queue.")).color(t.text_dim).size(11.5));
        }
        for (i, it) in items.iter().enumerate() {
            let id = it["id"].as_u64().unwrap_or(0);
            let status = it["status"].as_str().unwrap_or("");
            let frame = egui::Frame::NONE.fill(if i % 2 == 0 { t.row_alt } else { Color32::TRANSPARENT }).inner_margin(egui::Margin::same(6));
            let r = frame
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    let file = std::path::Path::new(it["path"].as_str().unwrap_or("")).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
                    ui.label(egui::RichText::new(format!("{} → {file}", it["sequenceName"].as_str().unwrap_or(""))).size(12.0).color(t.text));
                    let preset = it["preset"].as_str().filter(|s| !s.is_empty()).unwrap_or(CUSTOM);
                    let (label, col) = match status {
                        "ready" => (tl!("Ready").to_string(), t.text_dim),
                        "encoding" => {
                            (tlf!("Encoding {n}%", n = format!("{:.0}", it["progress"].as_f64().unwrap_or(0.0) * 100.0)) + &super::eta_suffix(it), t.accent)
                        }
                        "done" => (tl!("Done").to_string(), t.render_green),
                        "failed" => (tlf!("Failed: {e}", e = it["error"].as_str().unwrap_or("")), t.danger),
                        _ => (tl!("Cancelled").to_string(), t.text_faint),
                    };
                    if status == "encoding" {
                        // the progress and the time left on a line of their own: the column is narrow, and a wrapped "· 15 s left" reads badly
                        ui.label(egui::RichText::new(crate::i18n::t(preset)).size(11.0).color(t.text_dim));
                        ui.label(egui::RichText::new(label).size(11.0).color(col));
                    } else {
                        ui.label(egui::RichText::new(format!("{} · {label}", crate::i18n::t(preset))).size(11.0).color(col));
                    }
                    if status == "encoding" {
                        ui.add(egui::ProgressBar::new(it["progress"].as_f64().unwrap_or(0.0) as f32).desired_height(6.0));
                    }
                    ui.horizontal(|ui| {
                        let mut b = |ui: &mut egui::Ui, key: &'static str, text: &str, enabled: bool, cmd: &'static str, params: Value| {
                            let r = ui.add_enabled(enabled, egui::Button::new(egui::RichText::new(text).size(11.0)).small());
                            reg.add(format!("export.queue.item.{id}.{key}"), r.rect, text);
                            if r.clicked() {
                                action = Some((cmd, params));
                            }
                        };
                        b(ui, "up", "Up", i > 0 && status != "encoding", "export.queue.move", json!({"id": id, "by": -1}));
                        b(ui, "down", "Down", i + 1 < items.len(), "export.queue.move", json!({"id": id, "by": 1}));
                        if matches!(status, "ready" | "encoding") {
                            b(ui, "cancel", "Cancel", true, "export.queue.cancel", json!({"id": id}));
                        } else {
                            b(ui, "retry", "Retry", true, "export.queue.retry", json!({"id": id, "start": true}));
                        }
                        b(ui, "remove", "Remove", status != "encoding", "export.queue.remove", json!({"id": id}));
                    });
                })
                .response;
            reg.add(format!("export.queue.item.{id}"), r.rect, status);
        }
    });
    // footer
    let running = app.session.export_queue.running;
    let has_ready = items.iter().any(|i| i["status"] == "ready");
    let foot = Rect::from_min_max(pos2(rect.min.x + 8.0, rect.max.y - 34.0), pos2(rect.max.x - 8.0, rect.max.y - 6.0));
    let mut f = ui.new_child(egui::UiBuilder::new().max_rect(foot).layout(egui::Layout::left_to_right(egui::Align::Center)));
    let r = f.add_enabled(!running && has_ready, egui::Button::new(tl!("Start Queue")));
    reg.add("export.queue.start", r.rect, "Start Queue");
    if r.clicked() {
        action = Some(("export.queue.start", json!({})));
    }
    let r = f.add_enabled(running, egui::Button::new(tl!("Stop")));
    reg.add("export.queue.stop", r.rect, "Stop");
    if r.clicked() {
        action = Some(("export.queue.stop", json!({})));
    }
    let r =
        f.add_enabled(items.iter().any(|i| matches!(i["status"].as_str(), Some("done" | "failed" | "cancelled"))), egui::Button::new(tl!("Clear Finished")));
    reg.add("export.queue.clear", r.rect, "Clear Finished");
    if r.clicked() {
        action = Some(("export.queue.clear", json!({})));
    }
    reg.flush(app);
    if let Some((cmd, p)) = action
        && let Err(e) = app.session.execute(cmd, p)
    {
        app.ui.status = e.to_string();
    }
}

// ---------------------------------------------------------------------------------------------
// middle: settings
// ---------------------------------------------------------------------------------------------

const LABEL_W: f32 = 132.0;

/// A labelled settings row.
fn row<R>(ui: &mut egui::Ui, t: &Tokens, label: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.horizontal(|ui| {
        ui.add_sized(vec2(LABEL_W, 20.0), egui::Label::new(egui::RichText::new(label).color(t.text_dim).size(12.0)).truncate());
        add(ui)
    })
    .inner
}

/// A combo box over `options` (label, enabled); returns the chosen index.
fn combo(ui: &mut egui::Ui, reg: &mut Reg, id: &str, selected: &str, options: &[(String, bool)], width: f32) -> Option<usize> {
    let mut chosen = None;
    let r = egui::ComboBox::from_id_salt(id).selected_text(crate::i18n::t(selected)).width(width).show_ui(ui, |ui| {
        for (i, (label, enabled)) in options.iter().enumerate() {
            let resp = ui.add_enabled(*enabled, egui::Button::selectable(label == selected, crate::i18n::t(label)));
            let resp = if *enabled { resp } else { resp.on_disabled_hover_text(tl!("Not supported by FilmCraft's encoders yet")) };
            reg.add(format!("{id}.option.{i}"), resp.rect, label.clone());
            if resp.clicked() {
                chosen = Some(i);
            }
        }
    });
    reg.add(id, r.response.rect, selected);
    chosen
}

fn opts(labels: &[&str]) -> Vec<(String, bool)> {
    labels.iter().map(|l| (l.to_string(), true)).collect()
}

fn section(ui: &mut egui::Ui, reg: &mut Reg, open: &mut Vec<String>, key: &str, title: &str, t: &Tokens) -> bool {
    let is_open = open.iter().any(|k| k == key);
    let (r, now) = crate::widgets::section_header(ui, egui::Id::new(("export-sec", key)), title, is_open, t, true);
    reg.add(format!("export.section.{key}"), r.rect, title);
    if now != is_open {
        if now {
            open.push(key.into());
        } else {
            open.retain(|k| k != key);
        }
    }
    now
}

fn drag(ui: &mut egui::Ui, reg: &mut Reg, id: &str, v: &mut f64, range: std::ops::RangeInclusive<f64>, speed: f64, suffix: &str, decimals: usize) -> bool {
    let r = ui.add(egui::DragValue::new(v).range(range).speed(speed).suffix(suffix).max_decimals(decimals));
    reg.add(id, r.rect, format!("{v}"));
    r.changed()
}

fn check(ui: &mut egui::Ui, reg: &mut Reg, id: &str, v: &mut bool, label: &str) -> bool {
    let r = ui.checkbox(v, label);
    reg.add(id, r.rect, label);
    r.changed()
}

fn text(ui: &mut egui::Ui, reg: &mut Reg, id: &str, v: &mut String, hint: &str) {
    let r = ui.add(egui::TextEdit::singleline(v).hint_text(hint).desired_width(f32::INFINITY));
    reg.add(id, r.rect, hint);
}

const RATES: [(&str, FrameRate); 11] = [
    ("10", FrameRate { num: 10, den: 1 }),
    ("12", FrameRate { num: 12, den: 1 }),
    ("15", FrameRate { num: 15, den: 1 }),
    ("23.976", FrameRate::FPS_23_976),
    ("24", FrameRate::FPS_24),
    ("25", FrameRate::FPS_25),
    ("29.97", FrameRate::FPS_29_97),
    ("30", FrameRate::FPS_30),
    ("50", FrameRate::FPS_50),
    ("59.94", FrameRate::FPS_59_94),
    ("60", FrameRate::FPS_60),
];

pub(crate) const PARS: [(&str, Option<(u32, u32)>); 6] = [
    ("Square Pixels (1.0)", None),
    ("D1/DV NTSC (0.9091)", Some((10, 11))),
    ("D1/DV PAL (1.0940)", Some((59, 54))),
    ("HD Anamorphic 1080 (1.333)", Some((4, 3))),
    ("DVCPRO HD (1.5)", Some((3, 2))),
    ("Anamorphic 2:1 (2.0)", Some((2, 1))),
];

fn settings_column(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let mut reg = Reg::default();
    let mut open_manager = false;
    let mut chosen_preset: Option<String> = None;
    let (seq_w, seq_h, seq_rate, seq_sr) = app
        .session
        .active_sequence()
        .map(|q| (q.settings.width, q.settings.height, q.settings.frame_rate, q.settings.sample_rate))
        .unwrap_or((1920, 1080, FrameRate::FPS_24, 48_000));
    let has_captions = app.session.active_sequence().is_some_and(|q| !q.caption_tracks.is_empty());
    // an HDR sequence exports HDR (H.265 Main 10 where the hardware encoder has it) unless SDR is asked for
    let seq_hdr = app.session.active_sequence().is_some_and(|q| q.settings.color.working.is_hdr());
    let all_presets = app.session.export_presets.all();
    let favs: Vec<String> = all_presets.iter().filter(|p| app.session.export_presets.is_favorite(&p.name)).map(|p| p.name.clone()).collect();
    let mut pick_folder = false;
    let mut pick_overlay = false;
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(14.0, 10.0))).id_salt("export-settings"));
    let ex = &mut app.ui.export;
    egui::ScrollArea::vertical().id_salt("export-settings-scroll").auto_shrink([false, false]).show(&mut child, |ui| {
        ui.label(egui::RichText::new(tl!("Media File")).strong().size(14.0));
        ui.add_space(6.0);
        row(ui, &t, tl!("File Name"), |ui| text(ui, &mut reg, "export.fileName", &mut ex.file_name, tl!("Sequence name")));
        row(ui, &t, tl!("Location"), |ui| {
            let r = ui.small_button("…");
            reg.add("export.location.browse", r.rect, "Choose folder");
            pick_folder = r.clicked();
            text(ui, &mut reg, "export.location", &mut ex.location, tl!("Folder"));
        });
        // Preset: favourites, then every preset by category, then More presets…
        row(ui, &t, tl!("Preset"), |ui| {
            let r =
                egui::ComboBox::from_id_salt("export.preset").selected_text(crate::i18n::t(&ex.preset)).width(ui.available_width() - 4.0).show_ui(ui, |ui| {
                    if !favs.is_empty() {
                        ui.label(egui::RichText::new(tl!("Favorites")).color(t.text_dim).size(11.0));
                        for f in &favs {
                            if ui.selectable_label(*f == ex.preset, crate::i18n::t(f)).clicked() {
                                chosen_preset = Some(f.clone());
                            }
                        }
                        ui.separator();
                    }
                    let mut cat = String::new();
                    for p in &all_presets {
                        if p.category != cat {
                            cat = p.category.clone();
                            ui.label(egui::RichText::new(crate::i18n::t(&cat)).color(t.text_dim).size(11.0));
                        }
                        if ui.selectable_label(p.name == ex.preset, crate::i18n::t(&p.name)).clicked() {
                            chosen_preset = Some(p.name.clone());
                        }
                    }
                    ui.separator();
                    if ui.button(tl!("More presets…")).clicked() {
                        open_manager = true;
                    }
                });
            reg.add("export.preset", r.response.rect, ex.preset.clone());
        });
        ui.horizontal(|ui| {
            ui.add_space(LABEL_W + 8.0);
            let r = ui.link(tl!("More presets…"));
            reg.add("export.preset.more", r.rect, "More presets…");
            if r.clicked() {
                open_manager = true;
            }
        });
        row(ui, &t, tl!("Format"), |ui| {
            let labels: Vec<(String, bool)> = Format::ALL.iter().map(|f| (f.label().to_string(), filmcraft_engine::export::available(*f))).collect();
            if let Some(i) = combo(ui, &mut reg, "export.format", ex.settings.format.label(), &labels, ui.available_width() - 4.0) {
                let f = Format::ALL[i];
                if f != ex.settings.format {
                    // a format switch starts from that format's defaults (keeping size / rate / range)
                    let keep = ex.settings.clone();
                    ex.settings = ExportSettings {
                        format: f,
                        frame_size: keep.frame_size,
                        frame_rate: keep.frame_rate,
                        effects: keep.effects,
                        metadata: keep.metadata,
                        ..Default::default()
                    };
                }
            }
        });
        ui.add_space(8.0);
        let s = &mut ex.settings;
        if s.has_video() && section(ui, &mut reg, &mut ex.open_sections, "video", tl!("Video"), &t) {
            video_section(ui, &mut reg, s, &t, seq_w, seq_h, seq_hdr);
        }
        if (s.has_audio()
            || !s.has_video()
            || s.format.is_h26x()
            || matches!(s.format, Format::ProRes | Format::DnxHr | Format::Apv | Format::Mjpeg)
            || s.format.is_mxf())
            && !s.is_image_sequence()
            && s.format != Format::Gif
            && section(ui, &mut reg, &mut ex.open_sections, "audio", tl!("Audio"), &t)
        {
            audio_section(ui, &mut reg, s, &t, seq_sr);
        }
        if s.format.is_h26x() && section(ui, &mut reg, &mut ex.open_sections, "multiplexer", tl!("Multiplexer"), &t) {
            row(ui, &t, tl!("Multiplexer"), |ui| {
                let cur = if s.multiplexer == Multiplexer::Mp4 { "MP4" } else { "QuickTime" };
                if let Some(i) = combo(ui, &mut reg, "export.multiplexer", cur, &opts(&["MP4", "QuickTime"]), 160.0) {
                    s.multiplexer = if i == 0 { Multiplexer::Mp4 } else { Multiplexer::Mov };
                }
            });
        }
        if s.has_video() && section(ui, &mut reg, &mut ex.open_sections, "captions", tl!("Captions"), &t) {
            if !has_captions {
                ui.label(egui::RichText::new(tl!("The sequence has no caption track.")).color(t.text_dim).size(11.5));
            }
            row(ui, &t, tl!("Export Options"), |ui| {
                let modes = [tl!("None"), tl!("Burn Captions Into Video"), tl!("Create Sidecar File")];
                let cur = if s.burn_captions {
                    modes[1]
                } else if s.caption_sidecar.is_some() {
                    modes[2]
                } else {
                    modes[0]
                };
                if let Some(i) = combo(ui, &mut reg, "export.captions.mode", cur, &opts(&modes), 220.0) {
                    s.burn_captions = i == 1;
                    s.caption_sidecar = (i == 2).then(|| s.caption_sidecar.clone().unwrap_or_else(|| "srt".into()));
                }
            });
            if let Some(kind) = s.caption_sidecar.clone() {
                row(ui, &t, tl!("File Format"), |ui| {
                    let formats = [tl!("SubRip Subtitle (.srt)"), tl!("WebVTT (.vtt)")];
                    let cur = if kind == "vtt" { formats[1] } else { formats[0] };
                    if let Some(i) = combo(ui, &mut reg, "export.captions.format", cur, &opts(&formats), 220.0) {
                        s.caption_sidecar = Some(if i == 0 { "srt" } else { "vtt" }.into());
                    }
                });
            }
        }
        if section(ui, &mut reg, &mut ex.open_sections, "effects", tl!("Effects"), &t) {
            pick_overlay = effects_section(ui, &mut reg, s, &t);
        }
        if matches!(s.format, Format::H264 | Format::Hevc | Format::ProRes | Format::DnxHr | Format::Apv | Format::Mjpeg)
            && section(ui, &mut reg, &mut ex.open_sections, "metadata", tl!("Metadata"), &t)
        {
            let m = &mut s.metadata;
            for (key, label, v) in [
                ("title", tl!("Title"), &mut m.title),
                ("creator", tl!("Creator"), &mut m.creator),
                ("copyright", tl!("Copyright"), &mut m.copyright),
                ("description", tl!("Description"), &mut m.description),
                ("comment", tl!("Comment"), &mut m.comment),
            ] {
                row(ui, &t, label, |ui| text(ui, &mut reg, &format!("export.metadata.{key}"), v, ""));
            }
        }
        let _ = seq_rate;
    });
    if let Some(name) = chosen_preset {
        apply_preset(app, &name);
    }
    if open_manager {
        app.ui.export.manager = Some(PresetManager { selected: app.ui.export.preset.clone(), ..Default::default() });
    }
    if pick_folder && let Some(d) = app.hooks.pick_folder.as_mut().and_then(|f| f()) {
        app.ui.export.location = d;
    }
    if pick_overlay && let Some(f) = app.hooks.pick_open_file.as_mut().and_then(|f| f(tl!("Image"), &["png", "jpg", "jpeg"])) {
        app.ui.export.settings.effects.image_overlay.path = f;
        app.ui.export.settings.effects.image_overlay.enabled = true;
    }
    reg.flush(app);
}

fn video_section(ui: &mut egui::Ui, reg: &mut Reg, s: &mut ExportSettings, t: &Tokens, seq_w: u32, seq_h: u32, seq_hdr: bool) {
    // frame size
    let mut match_size = s.frame_size.is_none();
    row(ui, t, tl!("Frame Size"), |ui| {
        if check(ui, reg, "export.video.matchSize", &mut match_size, tl!("Match Source")) {
            s.frame_size = if match_size { None } else { Some((seq_w, seq_h)) };
        }
    });
    if let Some((w, h)) = s.frame_size.as_mut() {
        row(ui, t, "", |ui| {
            let (mut fw, mut fh) = (*w as f64, *h as f64);
            drag(ui, reg, "export.video.width", &mut fw, 16.0..=8192.0, 1.0, "", 0);
            ui.label("×");
            drag(ui, reg, "export.video.height", &mut fh, 16.0..=8192.0, 1.0, "", 0);
            (*w, *h) = (fw as u32, fh as u32);
        });
    }
    // frame rate
    let mut match_rate = s.frame_rate.is_none();
    row(ui, t, tl!("Frame Rate"), |ui| {
        if check(ui, reg, "export.video.matchRate", &mut match_rate, tl!("Match Source")) {
            s.frame_rate = if match_rate { None } else { Some(FrameRate::FPS_29_97) };
        }
        if let Some(r) = s.frame_rate {
            let cur = RATES.iter().find(|(_, x)| *x == r).map(|(l, _)| l.to_string()).unwrap_or_else(|| r.label());
            let labels: Vec<(String, bool)> = RATES.iter().map(|(l, _)| (l.to_string(), true)).collect();
            if let Some(i) = combo(ui, reg, "export.video.fps", &cur, &labels, 90.0) {
                s.frame_rate = Some(RATES[i].1);
            }
        }
    });
    row(ui, t, tl!("Field Order"), |ui| {
        let o = [FieldOrder::Progressive, FieldOrder::UpperFirst, FieldOrder::LowerFirst];
        let labels: Vec<(String, bool)> = o.iter().map(|f| (f.label().to_string(), *f == FieldOrder::Progressive)).collect();
        if let Some(i) = combo(ui, reg, "export.video.fieldOrder", s.field_order.label(), &labels, 160.0) {
            s.field_order = o[i];
        }
    });
    row(ui, t, tl!("Aspect"), |ui| {
        let cur = PARS.iter().find(|(_, p)| *p == s.pixel_aspect).map(|(l, _)| l.to_string()).unwrap_or_else(|| tl!("Custom").into());
        let labels: Vec<(String, bool)> = PARS.iter().map(|(l, _)| (l.to_string(), true)).collect();
        if let Some(i) = combo(ui, reg, "export.video.aspect", &cur, &labels, 220.0) {
            s.pixel_aspect = PARS[i].1;
        }
    });
    if s.format.is_mxf() {
        row(ui, t, tl!("Video Codec"), |ui| {
            let labels: Vec<(String, bool)> = MxfVideoCodec::ALL.iter().map(|c| (c.label().to_string(), true)).collect();
            if let Some(i) = combo(ui, reg, "export.video.mxfCodec", s.mxf_video_codec.label(), &labels, 180.0) {
                s.mxf_video_codec = MxfVideoCodec::ALL[i];
            }
        });
    }
    if s.supports_alpha() {
        row(ui, t, tl!("Alpha"), |ui| {
            check(ui, reg, "export.video.alpha", &mut s.alpha, tl!("Include Alpha Channel"));
        });
    }
    match s.video_format() {
        f @ (Format::H264 | Format::Hevc) => {
            let hevc = f == Format::Hevc;
            if hevc {
                // the hardware encoder writes Main (8-bit 4:2:0), or Main 10 for HDR sequences where it can; level chosen by the encoder
                let label = match (filmcraft_engine::export::hdr_available(Format::Hevc), seq_hdr && !s.sdr) {
                    (true, true) => tl!("Main 10 (10-bit, HDR)"),
                    (true, false) => tl!("Main (8-bit; Main 10 for HDR sequences)"),
                    (false, _) => tl!("Main (8-bit)"),
                };
                row(ui, t, tl!("Profile"), |ui| ui.label(label));
                row(ui, t, tl!("Encoder"), |ui| ui.label(tl!("Hardware (H.265 has no software encoder)")));
            } else {
                row(ui, t, tl!("Profile"), |ui| {
                    let o = [H264Profile::Baseline, H264Profile::Main, H264Profile::High];
                    let labels: Vec<(String, bool)> = o.iter().map(|p| (p.label().to_string(), true)).collect();
                    if let Some(i) = combo(ui, reg, "export.video.profile", s.h264_profile.label(), &labels, 120.0) {
                        s.h264_profile = o[i];
                    }
                });
                row(ui, t, tl!("Level"), |ui| {
                    let levels: [Option<u8>; 13] =
                        [None, Some(30), Some(31), Some(32), Some(40), Some(41), Some(42), Some(50), Some(51), Some(52), Some(60), Some(61), Some(62)];
                    let lab = |l: &Option<u8>| l.map(|l| format!("{}.{}", l / 10, l % 10)).unwrap_or_else(|| tl!("Auto").into());
                    let labels: Vec<(String, bool)> = levels.iter().map(|l| (lab(l), true)).collect();
                    if let Some(i) = combo(ui, reg, "export.video.level", &lab(&s.h264_level), &labels, 120.0) {
                        s.h264_level = levels[i];
                    }
                });
            }
            row(ui, t, tl!("Bitrate Encoding"), |ui| {
                // a hardware encoder has one pass and no constant-quality mode: H.265 offers neither
                let o = [BitrateMode::Cbr, BitrateMode::Vbr1Pass, BitrateMode::Vbr2Pass, BitrateMode::Crf];
                let labels: Vec<(String, bool)> =
                    o.iter().map(|m| (m.label().to_string(), !(hevc && matches!(m, BitrateMode::Vbr2Pass | BitrateMode::Crf)))).collect();
                if let Some(i) = combo(ui, reg, "export.video.bitrateMode", s.bitrate_mode.label(), &labels, 140.0) {
                    s.bitrate_mode = o[i];
                }
            });
            if s.bitrate_mode == BitrateMode::Crf {
                row(ui, t, tl!("Quality (CRF)"), |ui| {
                    let mut crf = if s.crf.is_finite() { f64::from(s.crf) } else { f64::from(filmcraft_engine::export::DEFAULT_CRF) };
                    if drag(ui, reg, "export.video.crf", &mut crf, 0.0..=51.0, 0.1, "", 0) {
                        s.crf = crf.round() as f32;
                    }
                });
            } else if let Some(bpp) = s.adaptive_bitrate {
                row(ui, t, tl!("Target Bitrate"), |ui| {
                    ui.label(egui::RichText::new(tlf!("Adaptive ({bpp} bits per pixel)", bpp)).size(12.0));
                    let r = ui.small_button(tl!("Set"));
                    reg.add("export.video.fixedBitrate", r.rect, "Set a fixed bitrate");
                    if r.clicked() {
                        s.adaptive_bitrate = None;
                    }
                });
            } else {
                row(ui, t, tl!("Target Bitrate"), |ui| {
                    let mut mbps = s.bitrate_kbps as f64 / 1000.0;
                    if drag(ui, reg, "export.video.target", &mut mbps, 0.1..=800.0, 0.1, " Mbps", 1) {
                        s.bitrate_kbps = (mbps * 1000.0).round() as u32;
                    }
                });
                if s.bitrate_mode != BitrateMode::Cbr {
                    row(ui, t, tl!("Maximum Bitrate"), |ui| {
                        let mut mbps = s.max_bitrate_kbps.unwrap_or(s.bitrate_kbps * 3 / 2) as f64 / 1000.0;
                        if drag(ui, reg, "export.video.max", &mut mbps, 0.1..=1000.0, 0.1, " Mbps", 1) {
                            s.max_bitrate_kbps = Some((mbps * 1000.0).round() as u32);
                        }
                    });
                }
            }
            row(ui, t, tl!("Key Frame Distance"), |ui| {
                let mut on = s.keyframe_distance.is_some();
                if check(ui, reg, "export.video.keyframeOn", &mut on, "") {
                    s.keyframe_distance = on.then_some(48);
                }
                if let Some(k) = s.keyframe_distance.as_mut() {
                    let mut v = *k as f64;
                    drag(ui, reg, "export.video.keyframe", &mut v, 1.0..=600.0, 1.0, " frames", 0);
                    *k = v as u32;
                }
            });
            // The system's hardware encoder where there is one (VideoToolbox on macOS, NVENC on NVIDIA
            // GPUs on Windows); everything it does not take (two-pass, HDR, MXF) and every machine
            // without one keeps the built-in encoder.
            // H.265 has only the hardware encoder: choosing the format is the opt-in.
            if !hevc {
                row(ui, t, "Hardware Encoding", |ui| {
                    let mut on = s.hardware_encoding == HardwareEncoding::Auto;
                    if check(ui, reg, "export.video.hardwareEncoding", &mut on, "Use the hardware encoder when available") {
                        s.hardware_encoding = if on { HardwareEncoding::Auto } else { HardwareEncoding::Off };
                    }
                });
            }
        }
        Format::ProRes => {
            row(ui, t, tl!("Profile"), |ui| {
                let keys = ["proxy", "lt", "standard", "hq", "4444", "4444xq"];
                let labels =
                    ["Apple ProRes 422 Proxy", "Apple ProRes 422 LT", "Apple ProRes 422", "Apple ProRes 422 HQ", "Apple ProRes 4444", "Apple ProRes 4444 XQ"];
                let cur = keys.iter().position(|k| *k == s.prores_profile).unwrap_or(3);
                if let Some(i) = combo(ui, reg, "export.video.proresProfile", labels[cur], &opts(&labels), 220.0) {
                    s.prores_profile = keys[i].into();
                }
            });
        }
        Format::DnxHr => {
            row(ui, t, tl!("Profile"), |ui| {
                let keys = ["lb", "sq", "hq", "hqx"];
                let labels = ["DNxHR LB", "DNxHR SQ", "DNxHR HQ", "DNxHR HQX (10-bit)"];
                let cur = keys.iter().position(|k| *k == s.dnx_profile).unwrap_or(2);
                if let Some(i) = combo(ui, reg, "export.video.dnxProfile", labels[cur], &opts(&labels), 220.0) {
                    s.dnx_profile = keys[i].into();
                }
            });
        }
        Format::Apv => {
            row(ui, t, tl!("Profile"), |ui| {
                let keys = ["422-10", "422-12", "444-10", "444-12"];
                let labels = ["APV 422-10", "APV 422-12", "APV 444-10", "APV 444-12"];
                let cur = keys.iter().position(|k| *k == s.apv_profile).unwrap_or(0);
                if let Some(i) = combo(ui, reg, "export.video.apvProfile", labels[cur], &opts(&labels), 220.0) {
                    s.apv_profile = keys[i].into();
                }
            });
        }
        Format::Mjpeg => {
            row(ui, t, tl!("Quality"), |ui| {
                let mut q = s.quality as f64;
                drag(ui, reg, "export.video.quality", &mut q, 1.0..=100.0, 1.0, "", 0);
                s.quality = q as u8;
            });
        }
        _ => {}
    }
    // Composite the picture on the GPU (filmcraft-gpu) instead of the CPU reference renderer;
    // Auto falls back to the CPU wherever the GPU cannot render the frame.
    row(ui, t, tl!("GPU Rendering"), |ui| {
        let mut on = s.gpu_rendering == GpuRendering::Auto;
        if check(ui, reg, "export.gpuRendering", &mut on, tl!("Composite on the GPU when available")) {
            s.gpu_rendering = if on { GpuRendering::Auto } else { GpuRendering::Off };
        }
    });
    row(ui, t, "", |ui| check(ui, reg, "export.video.maxDepth", &mut s.render_at_max_depth, tl!("Render at Maximum Depth")));
    row(ui, t, "", |ui| check(ui, reg, "export.video.maxQuality", &mut s.max_render_quality, tl!("Use Maximum Render Quality")));
}

fn audio_section(ui: &mut egui::Ui, reg: &mut Reg, s: &mut ExportSettings, t: &Tokens, seq_sr: u32) {
    let audio_only = !s.has_video();
    if !audio_only {
        row(ui, t, "", |ui| check(ui, reg, "export.audio.include", &mut s.include_audio, tl!("Export Audio")));
        if !s.include_audio {
            return;
        }
    }
    row(ui, t, tl!("Audio Format"), |ui| {
        let fixed = s.format.is_h26x() && s.multiplexer == Multiplexer::Mp4 || audio_only || s.format.is_mxf();
        let cur = match s.audio_codec() {
            AudioCodec::Aac => "AAC",
            _ => tl!("Uncompressed (PCM)"),
        };
        let labels =
            vec![("AAC".to_string(), !audio_only), (tl!("Uncompressed (PCM)").to_string(), !(s.format.is_h26x() && s.multiplexer == Multiplexer::Mp4))];
        if fixed {
            ui.label(cur);
        } else if let Some(i) = combo(ui, reg, "export.audio.codec", cur, &labels, 180.0) {
            s.audio.codec = if i == 0 { AudioCodec::Aac } else { AudioCodec::Pcm };
        }
    });
    row(ui, t, tl!("Sample Rate"), |ui| {
        let rates: [Option<u32>; 5] = [None, Some(32_000), Some(44_100), Some(48_000), Some(96_000)];
        let lab = |r: &Option<u32>| r.map(|r| format!("{r} Hz")).unwrap_or_else(|| tlf!("Match Source ({seq_sr} Hz)", seq_sr));
        let labels: Vec<(String, bool)> = rates.iter().map(|r| (lab(r), true)).collect();
        if let Some(i) = combo(ui, reg, "export.audio.sampleRate", &lab(&s.audio.sample_rate), &labels, 180.0) {
            s.audio.sample_rate = rates[i];
        }
    });
    row(ui, t, tl!("Channels"), |ui| {
        let names = [tl!("Mono"), tl!("Stereo"), "5.1"];
        let cur = match s.audio.channels {
            1 => names[0],
            6 => names[2],
            _ => names[1],
        };
        if let Some(i) = combo(ui, reg, "export.audio.channels", cur, &opts(&names), 120.0) {
            s.audio.channels = [1, 2, 6][i.min(2)];
        }
    });
    if s.audio_codec() == AudioCodec::Aac {
        row(ui, t, tl!("Audio Bitrate"), |ui| {
            let rates = [128u32, 160, 192, 256, 320];
            let labels: Vec<(String, bool)> = rates.iter().map(|r| (format!("{r} kbps"), true)).collect();
            if let Some(i) = combo(ui, reg, "export.audio.bitrate", &format!("{} kbps", s.audio.bitrate_kbps), &labels, 120.0) {
                s.audio.bitrate_kbps = rates[i];
            }
        });
    } else {
        row(ui, t, tl!("Sample Size"), |ui| {
            let sizes = [tl!("16 bit"), tl!("24 bit")];
            let cur = if s.audio.bits >= 24 { sizes[1] } else { sizes[0] };
            if let Some(i) = combo(ui, reg, "export.audio.bits", cur, &opts(&sizes), 120.0) {
                s.audio.bits = if i == 0 { 16 } else { 24 };
            }
        });
    }
}

fn placement_combo(ui: &mut egui::Ui, reg: &mut Reg, id: &str, p: &mut Placement) {
    let labels: Vec<(String, bool)> = Placement::ALL.iter().map(|x| (x.label().to_string(), true)).collect();
    if let Some(i) = combo(ui, reg, id, p.label(), &labels, 150.0) {
        *p = Placement::ALL[i];
    }
}

fn text_overlay(ui: &mut egui::Ui, reg: &mut Reg, key: &str, o: &mut TextOverlay, t: &Tokens, text_label: &str) {
    row(ui, t, text_label, |ui| text(ui, reg, &format!("export.effects.{key}.text"), &mut o.text, ""));
    row(ui, t, tl!("Position"), |ui| placement_combo(ui, reg, &format!("export.effects.{key}.position"), &mut o.placement));
    row(ui, t, tl!("Size / Opacity"), |ui| {
        let (mut sz, mut op) = (o.size_percent as f64, o.opacity as f64);
        drag(ui, reg, &format!("export.effects.{key}.size"), &mut sz, 1.0..=50.0, 0.2, " %", 1);
        drag(ui, reg, &format!("export.effects.{key}.opacity"), &mut op, 0.0..=100.0, 1.0, " %", 0);
        (o.size_percent, o.opacity) = (sz as f32, op as f32);
    });
}

/// Returns whether the image overlay's Browse… was clicked.
fn effects_section(ui: &mut egui::Ui, reg: &mut Reg, s: &mut ExportSettings, t: &Tokens) -> bool {
    let mut browse = false;
    let audio = s.has_audio() || !s.has_video();
    let video = s.has_video();
    let fx = &mut s.effects;
    if video {
        check(ui, reg, "export.effects.imageOverlay", &mut fx.image_overlay.enabled, tl!("Image Overlay"));
        if fx.image_overlay.enabled {
            let o = &mut fx.image_overlay;
            row(ui, t, tl!("Image"), |ui| {
                let r = ui.small_button(tl!("Browse…"));
                reg.add("export.effects.imageOverlay.browse", r.rect, "Browse…");
                browse = r.clicked();
                text(ui, reg, "export.effects.imageOverlay.path", &mut o.path, tl!("PNG or JPEG file"));
            });
            row(ui, t, tl!("Position"), |ui| placement_combo(ui, reg, "export.effects.imageOverlay.position", &mut o.placement));
            row(ui, t, tl!("Size / Opacity"), |ui| {
                let (mut sz, mut op) = (o.size_percent as f64, o.opacity as f64);
                drag(ui, reg, "export.effects.imageOverlay.size", &mut sz, 1.0..=100.0, 0.5, " %", 1);
                drag(ui, reg, "export.effects.imageOverlay.opacity", &mut op, 0.0..=100.0, 1.0, " %", 0);
                (o.size_percent, o.opacity) = (sz as f32, op as f32);
            });
        }
        check(ui, reg, "export.effects.nameOverlay", &mut fx.name_overlay.enabled, tl!("Name Overlay"));
        if fx.name_overlay.enabled {
            text_overlay(ui, reg, "nameOverlay", &mut fx.name_overlay, t, tl!("Name"));
        }
        check(ui, reg, "export.effects.timecodeOverlay", &mut fx.timecode_overlay.enabled, tl!("Timecode Overlay"));
        if fx.timecode_overlay.enabled {
            text_overlay(ui, reg, "timecodeOverlay", &mut fx.timecode_overlay, t, tl!("Prefix"));
        }
        check(ui, reg, "export.effects.videoLimiter", &mut fx.video_limiter.enabled, tl!("Video Limiter"));
        if fx.video_limiter.enabled {
            row(ui, t, tl!("Luma Min / Max"), |ui| {
                let (mut lo, mut hi) = (fx.video_limiter.min_percent as f64, fx.video_limiter.max_percent as f64);
                drag(ui, reg, "export.effects.videoLimiter.min", &mut lo, 0.0..=50.0, 0.5, " %", 1);
                drag(ui, reg, "export.effects.videoLimiter.max", &mut hi, 50.0..=100.0, 0.5, " %", 1);
                (fx.video_limiter.min_percent, fx.video_limiter.max_percent) = (lo as f32, hi as f32);
            });
        }
    }
    if audio {
        check(ui, reg, "export.effects.loudness", &mut fx.loudness.enabled, tl!("Loudness Normalization"));
        if fx.loudness.enabled {
            row(ui, t, tl!("Target Loudness"), |ui| {
                drag(ui, reg, "export.effects.loudness.target", &mut fx.loudness.target_lufs, -40.0..=-5.0, 0.1, " LUFS", 1);
            });
            row(ui, t, tl!("True Peak Limit"), |ui| {
                drag(ui, reg, "export.effects.loudness.truePeak", &mut fx.loudness.true_peak_dbtp, -9.0..=0.0, 0.1, " dBTP", 1);
            });
        }
    }
    browse
}

// ---------------------------------------------------------------------------------------------
// right: preview, range, summary, buttons
// ---------------------------------------------------------------------------------------------

pub(crate) const RANGES: [(&str, &str); 4] = [("entire", "Entire Source"), ("inOut", "Source In/Out"), ("workArea", "Work Area"), ("custom", "Custom")];

fn preview_column(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect, seq_id: filmcraft_engine::project::ItemId) {
    let t = app.tokens;
    let mut reg = Reg::default();
    let Some(q) = app.session.active_sequence().cloned() else { return };
    let rate = q.settings.frame_rate;
    // preview (aspect of the output frame)
    let r = app.ui.export.settings.resolve(q.settings.width, q.settings.height, rate, q.settings.sample_rate);
    let pv_area = Rect::from_min_size(rect.min + vec2(14.0, 14.0), vec2(rect.width() - 28.0, (rect.height() * 0.42).max(160.0)));
    let pic = crate::panels::monitor::fit(pv_area, r.width as f32, r.height as f32);
    ui.painter().rect_filled(pic, 0.0, Color32::BLACK);
    if app.ui.export.settings.has_video() {
        let frame = rate.frame_at(app.session.playhead());
        let key = FrameKey { target: Target::Sequence(seq_id), frame, size: 500, revision: app.session.revision, draft: false };
        let project = app.session.project.clone();
        app.frames.request(key, rate.tick_of(frame), 0.5, &project, 0);
        if let Some(img) = app.frames.get(&key) {
            let ctx = ui.ctx().clone();
            let tex = app.texture_for(&ctx, "export-preview", key, &img);
            let inner = match app.ui.export.settings.scaling {
                Scaling::StretchToFill => pic,
                Scaling::ScaleToFit => crate::panels::monitor::fit(pic, q.settings.width as f32, q.settings.height as f32),
                Scaling::ScaleToFill => {
                    let s = (pic.width() / q.settings.width as f32).max(pic.height() / q.settings.height as f32);
                    Rect::from_center_size(pic.center(), vec2(q.settings.width as f32 * s, q.settings.height as f32 * s))
                }
            };
            ui.painter().with_clip_rect(pic).image(tex, inner, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
    } else {
        ui.painter().text(pic.center(), Align2::CENTER_CENTER, tl!("Audio only"), Tokens::ui(13.0), t.text_dim);
    }
    reg.add("export.preview", pic, "Preview");
    // range + scaling
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(Rect::from_min_max(pos2(rect.min.x + 14.0, pv_area.max.y + 8.0), pos2(rect.max.x - 14.0, rect.max.y - 50.0)))
            .id_salt("export-right"),
    );
    let ex = &mut app.ui.export;
    let has_work_area = q.work_area.is_some();
    child.horizontal(|ui| {
        ui.label(egui::RichText::new(tl!("Range")).color(t.text_dim));
        let cur = RANGES.iter().find(|(k, _)| *k == ex.range).map(|x| x.1).unwrap_or("Entire Source");
        let labels: Vec<(String, bool)> = RANGES.iter().map(|(k, l)| (l.to_string(), *k != "workArea" || has_work_area)).collect();
        if let Some(i) = combo(ui, &mut reg, "export.range", cur, &labels, 140.0) {
            ex.range = RANGES[i].0.into();
        }
        if ex.range == "custom" {
            drag(ui, &mut reg, "export.range.start", &mut ex.custom_start, 0.0..=86_400.0, 0.04, " s", 2);
            drag(ui, &mut reg, "export.range.end", &mut ex.custom_end, 0.0..=86_400.0, 0.04, " s", 2);
        }
    });
    if ex.settings.has_video() {
        child.horizontal(|ui| {
            ui.label(egui::RichText::new(tl!("Scaling")).color(t.text_dim));
            let o =
                [(Scaling::ScaleToFit, tl!("Scale To Fit")), (Scaling::ScaleToFill, tl!("Scale To Fill")), (Scaling::StretchToFill, tl!("Stretch To Fill"))];
            let cur = o.iter().find(|(x, _)| *x == ex.settings.scaling).map_or(o[0].1, |x| x.1);
            let labels: Vec<(String, bool)> = o.iter().map(|(_, l)| (l.to_string(), true)).collect();
            if let Some(i) = combo(ui, &mut reg, "export.scaling", cur, &labels, 140.0) {
                ex.settings.scaling = o[i].0;
            }
        });
    }
    // summary
    let range = range_ticks(ex, &q);
    let sum = ex.settings.summary(q.settings.width, q.settings.height, rate, q.settings.sample_rate, range.1 - range.0);
    child.add_space(8.0);
    let resp = egui::Frame::NONE
        .fill(t.field_bg)
        .corner_radius(4.0)
        .inner_margin(egui::Margin::same(10))
        .show(&mut child, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new(tl!("Summary")).strong());
            egui::Grid::new("export-summary-grid").num_columns(2).spacing(vec2(10.0, 3.0)).show(ui, |ui| {
                let line = |ui: &mut egui::Ui, k: &str, v: &str| {
                    ui.label(egui::RichText::new(crate::i18n::t(k)).color(t.text_dim).size(11.5));
                    ui.add(egui::Label::new(egui::RichText::new(v).size(11.5)).wrap_mode(egui::TextWrapMode::Wrap).halign(egui::Align::Min));
                    ui.end_row();
                };
                line(ui, "Output", &output_path(ex));
                line(ui, "", &sum.format);
                line(ui, "", &sum.video);
                line(ui, "", &sum.audio);
                line(
                    ui,
                    "Source",
                    &tlf!(
                        "Sequence, {w}x{h}, {fps} fps, {rate} Hz",
                        w = q.settings.width,
                        h = q.settings.height,
                        fps = rate.label(),
                        rate = q.settings.sample_rate
                    ),
                );
                line(
                    ui,
                    "Range",
                    &format!(
                        "{} ({:.2} s)",
                        crate::i18n::t(RANGES.iter().find(|(k, _)| *k == ex.range).map(|x| x.1).unwrap_or("")),
                        (range.1 - range.0).seconds()
                    ),
                );
            });
        })
        .response;
    reg.add("export.summary", resp.rect, sum.video.clone());
    child.add_space(4.0);
    let est = child.label(egui::RichText::new(tlf!("Estimated file size: {size}", size = format_bytes(sum.estimated_bytes))).color(t.text_dim));
    reg.add("export.estimate", est.rect, sum.estimated_size.clone());
    // buttons
    let go = Rect::from_min_size(pos2(rect.max.x - 120.0, rect.max.y - 44.0), vec2(104.0, 30.0));
    let queue = Rect::from_min_size(pos2(go.min.x - 132.0, go.min.y), vec2(122.0, 30.0));
    let qresp = ui.interact(queue, egui::Id::new("export-queue-send"), Sense::click());
    ui.painter().rect_stroke(queue, 15.0, egui::Stroke::new(1.0, if qresp.hovered() { t.text } else { t.text_dim }), egui::StrokeKind::Inside);
    ui.painter().text(queue.center(), Align2::CENTER_CENTER, tl!("Send to Queue"), Tokens::semibold(12.5), t.text);
    reg.add("export.sendToQueue", queue, "Send to Queue");
    let resp = ui.interact(go, egui::Id::new("export-go"), Sense::click());
    ui.painter().rect_filled(go, 15.0, if resp.hovered() { t.accent_hover } else { t.accent });
    ui.painter().text(go.center(), Align2::CENTER_CENTER, tl!("Export"), Tokens::semibold(13.0), Color32::WHITE);
    reg.add("export.button", go, "Export");
    reg.flush(app);
    if resp.clicked() {
        let r = app.session.execute("file.exportMedia", export_params(app));
        match r {
            Ok(v) => app.ui.status = tlf!("Exporting {path}", path = v["path"].as_str().unwrap_or("")),
            Err(e) => app.ui.status = e.to_string(),
        }
    }
    if qresp.clicked() {
        let mut p = export_params(app);
        if app.ui.export.preset != CUSTOM {
            // the queue shows which preset an item came from
            p["preset"] = json!(app.ui.export.preset);
        }
        match app.session.execute("export.queue.add", p) {
            Ok(_) => app.ui.status = tl!("Added to the export queue").into(),
            Err(e) => app.ui.status = e.to_string(),
        }
    }
}

/// The export range in sequence ticks (for the summary).
fn range_ticks(ex: &ExportUi, q: &filmcraft_engine::project::Sequence) -> (Tick, Tick) {
    let fd = q.settings.frame_rate.frame_duration();
    let end = q.duration().max(fd);
    match ex.range.as_str() {
        "inOut" => {
            let a = q.mark_in.unwrap_or(Tick::ZERO);
            (a, q.mark_out.map(|o| o + fd).unwrap_or(end).max(a + fd))
        }
        "workArea" => q.work_area.map(|w| (w.start, w.end())).unwrap_or((Tick::ZERO, end)),
        "custom" => (Tick::from_seconds_f64(ex.custom_start.max(0.0)), Tick::from_seconds_f64(ex.custom_end.max(ex.custom_start + 0.001))),
        _ => (Tick::ZERO, end),
    }
}

// ---------------------------------------------------------------------------------------------
// Preset Manager
// ---------------------------------------------------------------------------------------------

fn preset_manager(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut m) = app.ui.export.manager.clone() else { return };
    let t = app.tokens;
    let mut reg = Reg::default();
    let mut open = true;
    let mut close = false;
    let mut apply: Option<String> = None;
    let mut cmd: Option<(&'static str, Value)> = None;
    let mut pick: Option<&'static str> = None;
    let all = app.session.export_presets.all();
    let favorite = |n: &str| app.session.export_presets.is_favorite(n);
    let q = m.query.to_lowercase();
    egui::Window::new(tl!("Preset Manager"))
        .id(egui::Id::new("Preset Manager"))
        .id(egui::Id::new("preset-manager"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size(vec2(560.0, 520.0))
        .pivot(Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                let r = crate::widgets::search_field(ui, &mut m.query, tl!("Search presets"), 300.0, &t);
                reg.add("presetManager.search", r.rect, "Search");
                check(ui, &mut reg, "presetManager.favoritesOnly", &mut m.favorites_only, tl!("Favorites only"));
            });
            ui.separator();
            egui::ScrollArea::vertical().max_height(340.0).auto_shrink([false, false]).show(ui, |ui| {
                let mut cat = String::new();
                for p in all.iter().filter(|p| {
                    (crate::i18n::matches_query(&p.name, &q) || crate::i18n::matches_query(&p.description, &q) || crate::i18n::matches_query(&p.category, &q))
                        && (!m.favorites_only || favorite(&p.name))
                }) {
                    if p.category != cat {
                        cat = p.category.clone();
                        ui.add_space(4.0);
                        ui.label(egui::RichText::new(crate::i18n::t(&cat)).strong().color(t.text_dim).size(11.5));
                    }
                    ui.horizontal(|ui| {
                        let fav = favorite(&p.name);
                        let (sr, sresp) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
                        paint_star(ui.painter(), sr.shrink(2.0), fav, if fav { Color32::from_rgb(0xf5, 0xc2, 0x42) } else { t.text_faint });
                        reg.add(format!("presetManager.favorite.{}", p.name), sr, if fav { "Remove from favorites" } else { "Add to favorites" });
                        if sresp.on_hover_text(tl!("Show in the Preset menu")).clicked() {
                            cmd = Some(("export.presets.favorite", json!({"name": p.name})));
                        }
                        let sel = preset_key(&m.selected) == preset_key(&p.name);
                        let r = list_row(ui, &t, sel, crate::i18n::t(&p.name), 300.0);
                        reg.add(format!("presetManager.item.{}", p.name), r.rect, p.name.clone());
                        if r.clicked() {
                            m.selected = p.name.clone();
                        }
                        if r.double_clicked() {
                            apply = Some(p.name.clone());
                        }
                        let r = r.on_hover_text(crate::i18n::t(&p.description));
                        let _ = r;
                        ui.label(egui::RichText::new(p.settings.format.label()).color(t.text_dim).size(11.0));
                        if !p.builtin {
                            ui.label(egui::RichText::new(tl!("User")).color(t.accent).size(10.5));
                        }
                    });
                }
            });
            ui.separator();
            let sel = all.iter().find(|p| preset_key(&p.name) == preset_key(&m.selected)).cloned();
            if let Some(p) = &sel {
                ui.label(egui::RichText::new(crate::i18n::t(&p.description)).color(t.text_dim).size(11.5));
            }
            ui.horizontal(|ui| {
                ui.label(tl!("Save current settings as:"));
                let r = ui.add(egui::TextEdit::singleline(&mut m.save_name).hint_text(tl!("Preset name")).desired_width(200.0));
                reg.add("presetManager.saveName", r.rect, "Preset name");
                let r = ui.add_enabled(!m.save_name.trim().is_empty(), egui::Button::new(tl!("Save")));
                reg.add("presetManager.save", r.rect, "Save");
                if r.clicked() {
                    cmd = Some(("export.presets.save", json!({"name": m.save_name.trim(), "settings": settings_json(&app.ui.export.settings)})));
                    m.selected = m.save_name.trim().to_string();
                }
            });
            ui.horizontal(|ui| {
                let r = ui.button(tl!("Import…"));
                reg.add("presetManager.import", r.rect, "Import…");
                if r.clicked() {
                    pick = Some("import");
                }
                let r = ui.add_enabled(sel.is_some(), egui::Button::new(tl!("Export…")));
                reg.add("presetManager.export", r.rect, "Export…");
                if r.clicked() {
                    pick = Some("export");
                }
                let r = ui.add_enabled(sel.as_ref().is_some_and(|p| !p.builtin), egui::Button::new(tl!("Delete")));
                reg.add("presetManager.delete", r.rect, "Delete");
                if r.clicked() {
                    cmd = Some(("export.presets.delete", json!({"name": m.selected})));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let r = ui.add_enabled(sel.is_some(), egui::Button::new(tl!("OK")));
                    reg.add("presetManager.ok", r.rect, "OK");
                    if r.clicked() {
                        apply = Some(m.selected.clone());
                    }
                    let r = ui.button(tl!("Cancel"));
                    reg.add("presetManager.cancel", r.rect, "Cancel");
                    close |= r.clicked();
                });
            });
        });
    reg.flush(app);
    if let Some((c, p)) = cmd
        && let Err(e) = app.session.execute(c, p)
    {
        app.ui.status = e.to_string();
    }
    match pick {
        Some("import") => match app.hooks.pick_open_file.as_mut().and_then(|f| f(tl!("Export presets"), &["json"])) {
            Some(path) => {
                if let Err(e) = app.session.execute("export.presets.import", json!({"path": path})) {
                    app.ui.status = e.to_string();
                }
            }
            None => app.ui.status = tl!("Import: no file chosen (agents: export.presets.import {path})").into(),
        },
        Some(_) => match app.hooks.pick_save_as.as_mut().and_then(|f| f(tl!("Export presets"), &["json"], &format!("{}.json", m.selected))) {
            Some(path) => {
                if let Err(e) = app.session.execute("export.presets.export", json!({"path": path, "names": [m.selected]})) {
                    app.ui.status = e.to_string();
                }
            }
            None => app.ui.status = tl!("Export: no file chosen (agents: export.presets.export {path, names})").into(),
        },
        None => {}
    }
    if let Some(name) = apply
        && apply_preset(app, &name)
    {
        close = true;
    }
    app.ui.export.manager = if close || !open || ctx.input(|i| i.key_pressed(egui::Key::Escape)) { None } else { Some(m) };
}

/// A five-pointed star (favourite marker), drawn from scratch.
fn paint_star(p: &egui::Painter, r: Rect, filled: bool, color: Color32) {
    let c = r.center();
    let (ro, ri) = (r.width().min(r.height()) / 2.0, r.width().min(r.height()) / 5.0);
    let pts: Vec<egui::Pos2> = (0..10)
        .map(|i| {
            let a = std::f32::consts::PI * (i as f32 / 5.0) - std::f32::consts::FRAC_PI_2;
            let rr = if i % 2 == 0 { ro } else { ri };
            pos2(c.x + rr * a.cos(), c.y + rr * a.sin())
        })
        .collect();
    if filled {
        // triangles from the centre keep the concave outline exact
        for i in 0..10 {
            p.add(egui::Shape::convex_polygon(vec![c, pts[i], pts[(i + 1) % 10]], color, egui::Stroke::NONE));
        }
    }
    p.add(egui::Shape::closed_line(pts, egui::Stroke::new(1.0, color)));
}

// ---------------------------------------------------------------------------------------------
// Quick Export (header)
// ---------------------------------------------------------------------------------------------

/// Presets offered by Quick Export: favourites first, then the common delivery presets.
fn quick_presets(app: &FilmcraftApp) -> Vec<String> {
    let mut v: Vec<String> =
        app.session.export_presets.all().iter().filter(|p| app.session.export_presets.is_favorite(&p.name)).map(|p| p.name.clone()).collect();
    for p in builtin_presets().into_iter().filter(|p| p.category == "Match Source" || p.category == "Web & Social").map(|p| p.name) {
        if !v.contains(&p) {
            v.push(p);
        }
    }
    v
}

/// The Quick Export popup under the header button at `anchor`.
pub fn quick_export(app: &mut FilmcraftApp, ctx: &egui::Context, anchor: egui::Pos2) {
    if !app.ui.export.quick_open {
        return;
    }
    let t = app.tokens;
    let Some(seq) = app.session.state.active_sequence else {
        app.ui.export.quick_open = false;
        app.ui.status = tl!("Quick Export: open a sequence first").into();
        return;
    };
    if app.ui.export.quick_preset.is_empty() {
        app.ui.export.quick_preset = app.session.export_queue.quick_preset.clone().unwrap_or_else(|| DEFAULT_PRESET.to_string());
    }
    let preset = app.session.export_presets.find(&app.ui.export.quick_preset);
    let ext = preset.as_ref().map(|p| p.settings.extension()).unwrap_or("mp4");
    if app.ui.export.quick_path.is_empty() || !app.ui.export.quick_path.ends_with(&format!(".{ext}")) {
        let name = app.session.project.item(seq).map(|i| i.name.clone()).unwrap_or_else(|| "Sequence".into());
        let dir = match app.ui.export.quick_path.rsplit_once(['/', '\\']) {
            Some((d, _)) if !d.is_empty() => d.to_string(),
            _ => filmcraft_engine::export_tools::default_export_dir(&app.session).to_string_lossy().to_string(),
        };
        app.ui.export.quick_path = std::path::Path::new(&dir).join(format!("{name}.{ext}")).to_string_lossy().to_string();
    }
    let est = app
        .session
        .execute("export.resolve", json!({"preset": app.ui.export.quick_preset}))
        .ok()
        .and_then(|v| v["summary"]["estimated_size"].as_str().map(str::to_string));
    let presets = quick_presets(app);
    let mut reg = Reg::default();
    let mut go = false;
    let mut close = false;
    let q = &mut app.ui.export;
    let area = egui::Area::new(egui::Id::new("quick-export")).order(egui::Order::Foreground).fixed_pos(anchor).show(ctx, |ui| {
        egui::Frame::popup(ui.style()).show(ui, |ui| {
            ui.set_width(340.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(tl!("Quick Export")).strong().size(14.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let r = ui.small_button(tl!("Close"));
                    reg.add("quickExport.close", r.rect, "Close");
                    close = r.clicked();
                });
            });
            ui.label(egui::RichText::new(tl!("File Name & Location")).color(t.text_dim).size(11.5));
            let r = ui.add(egui::TextEdit::singleline(&mut q.quick_path).desired_width(f32::INFINITY));
            reg.add("quickExport.path", r.rect, "File Name & Location");
            ui.add_space(4.0);
            ui.label(egui::RichText::new(tl!("Preset")).color(t.text_dim).size(11.5));
            egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                for name in &presets {
                    let r = list_row(ui, &t, preset_key(name) == preset_key(&q.quick_preset), crate::i18n::t(name), 320.0);
                    reg.add(format!("quickExport.preset.{name}"), r.rect, name.clone());
                    if r.clicked() {
                        q.quick_preset = name.clone();
                    }
                }
            });
            ui.add_space(4.0);
            ui.label(egui::RichText::new(tlf!("Estimated file size: {size}", size = est.as_deref().unwrap_or("–"))).color(t.text_dim).size(11.5));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let r = ui.add(
                    egui::Button::new(egui::RichText::new(tl!("Export")).color(Color32::WHITE)).fill(t.accent).corner_radius(12.0).min_size(vec2(90.0, 26.0)),
                );
                reg.add("quickExport.go", r.rect, "Export");
                go = r.clicked();
            });
        });
    });
    let clicked_elsewhere = area.response.clicked_elsewhere();
    // the header click that opened the popup is not a click elsewhere
    let toggled = ctx.data_mut(|d| d.remove_temp::<bool>(egui::Id::new("quick-export-toggled")).unwrap_or(false));
    reg.flush(app);
    if go {
        let p = json!({"preset": app.ui.export.quick_preset, "path": app.ui.export.quick_path});
        match app.session.execute("export.quick", p) {
            Ok(v) => app.ui.status = tlf!("Quick Export: {path}", path = v["path"].as_str().unwrap_or("")),
            Err(e) => app.ui.status = e.to_string(),
        }
        close = true;
    }
    if close || ctx.input(|i| i.key_pressed(egui::Key::Escape)) || (clicked_elsewhere && !toggled) {
        app.ui.export.quick_open = false;
    }
}

/// A left-aligned selectable list row of a fixed width.
fn list_row(ui: &mut egui::Ui, t: &Tokens, selected: bool, text: &str, width: f32) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(width, 20.0), Sense::click());
    let bg = if selected {
        t.accent
    } else if resp.hovered() {
        t.hover
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(r, 3.0, bg);
    ui.painter().text(pos2(r.min.x + 6.0, r.center().y), Align2::LEFT_CENTER, text, Tokens::ui(12.5), if selected { Color32::WHITE } else { t.text });
    resp
}
