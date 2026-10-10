//! The Settings dialog (app menu ▸ Settings ▸ <category>; Edit ▸ Preferences in the in-window
//! menu bar; General is Cmd+,). One window titled "Preferences": the category list on the left,
//! the category's fields on the right, Help / Reset… / Cancel / OK at the bottom. The page layout
//! comes from the engine's field schema ([`filmcraft_engine::settings::categories`]); edits go to a
//! draft (`UiState::settings`, serde so `ui.inspect` / `ui.set` reach it) and are applied with
//! `prefs.set` on OK.
//!
//! Automation ids: `settings.category.<id>` (list rows), `settings.<key>` for every control (the
//! preference key, e.g. `settings.timeline.stillImageDuration`), `settings.<key>.<value>` for the
//! items of an open dropdown, `settings.<key>.browse` (folder fields), `settings.<key>.hex`
//! (colour fields), `settings.labels.colors.<label>.name|color`, `settings.mediaCache.clean`,
//! `settings.help`, `settings.reset`, `settings.cancel`, `settings.ok`.

use egui::{Align, Align2, Color32, CornerRadius, Frame, Layout, Margin, Rect, RichText, Sense, Stroke, Ui, vec2};
use filmcraft_engine::settings::{self, DeviceList, Field, Kind, Row};
use filmcraft_project::Label;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::theme::Tokens;
use crate::{AudioDevices, Dialog, FilmcraftApp};

/// Window size (Premiere's is 927 × 724 pt).
const W: f32 = 900.0;
const BODY_H: f32 = 600.0;
const LIST_W: f32 = 216.0;
const ROW_H: f32 = 24.6;
const LABEL_W: f32 = 250.0;

/// The open dialog: current page and the preferences being edited (the whole preferences
/// document, as `prefs.get` returns it).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SettingsDraft {
    pub page: String,
    pub values: Value,
    /// Result of the last button (Delete… media cache) or an error from OK.
    #[serde(default)]
    pub message: String,
    /// `mediaCache.info` when the Media Cache page was shown.
    #[serde(default)]
    pub cache_info: Value,
    /// Audio devices reported by the platform (Audio Hardware page).
    #[serde(default)]
    pub devices: Option<AudioDevices>,
}

/// Open the dialog on a category page.
pub fn open(app: &mut FilmcraftApp, page: &str) -> Result<Value, String> {
    let cat = settings::category(page).ok_or_else(|| format!("unknown settings category `{page}`"))?;
    app.ui.settings = Some(SettingsDraft { page: cat.id.into(), values: app.session.prefs.to_value(), ..Default::default() });
    app.dialog = Some(Dialog::Preferences);
    Ok(json!({"dialog": "settings", "page": cat.id}))
}

/// `app.settings.<category>` (and the older `app.preferences[.autoSave]` ids).
pub fn route(app: &mut FilmcraftApp, id: &str) -> Option<Result<Value, String>> {
    if let Some(page) = id.strip_prefix("app.settings.") {
        return Some(open(app, page));
    }
    match id {
        "app.settings" => Some(open(app, "general")),
        "app.preferences" | "app.preferences.autoSave" => Some(open(app, "autoSave")),
        _ => None,
    }
}

/// The colour of a label (Settings ▸ Labels).
pub fn label_color(app: &FilmcraftApp, l: Label) -> Color32 {
    let c = app.session.prefs.labels.rgb(l);
    Color32::from_rgb(c[0], c[1], c[2])
}

/// Choose a theme from View ▸ Appearance or `ui.set`: stored as Settings ▸ Appearance ▸ Color Theme.
pub fn set_theme(app: &mut FilmcraftApp, ctx: &egui::Context, k: crate::theme::ThemeKind) {
    if let Err(e) = app.session.execute("prefs.set", json!({"key": "appearance.colorTheme", "value": k.pref_name()})) {
        app.ui.status = e.to_string();
    }
    app.apply_prefs(ctx);
    app.set_theme(ctx, k);
}

/// Patch the open dialog's draft (`ui.set {"settings": {"page": "trim", "values": {key: value}}}`).
pub fn patch(app: &mut FilmcraftApp, p: &Value) -> Result<(), String> {
    let d = app.ui.settings.as_mut().ok_or("the Settings dialog is not open")?;
    if let Some(page) = p.get("page").and_then(Value::as_str) {
        d.page = settings::category(page).ok_or_else(|| format!("unknown settings category `{page}`"))?.id.into();
    }
    if let Some(m) = p.get("values").and_then(Value::as_object) {
        for (k, v) in m {
            let slot = d.values.pointer_mut(&ptr(k)).ok_or_else(|| format!("unknown preference `{k}`"))?;
            *slot = v.clone();
        }
    }
    Ok(())
}

fn ptr(key: &str) -> String {
    format!("/{}", key.replace('.', "/"))
}

fn get<'a>(v: &'a Value, key: &str) -> &'a Value {
    v.pointer(&ptr(key)).unwrap_or(&Value::Null)
}

fn put(v: &mut Value, key: &str, x: Value) {
    if let Some(s) = v.pointer_mut(&ptr(key)) {
        *s = x;
    }
}

/// Leaf values of `new` that differ from `old`, as dotted keys.
fn diff(prefix: &str, old: &Value, new: &Value, out: &mut Map<String, Value>) {
    match (old, new) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, nv) in b {
                let key = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
                diff(&key, a.get(k).unwrap_or(&Value::Null), nv, out);
            }
        }
        _ if old != new && !prefix.is_empty() => {
            out.insert(prefix.to_string(), new.clone());
        }
        _ => {}
    }
}

fn modal_frame(t: &Tokens) -> Frame {
    Frame::new().fill(t.panel_bg).stroke(Stroke::new(1.0, t.separator)).corner_radius(CornerRadius::same(10)).inner_margin(Margin::same(0))
}

fn button(app: &mut FilmcraftApp, ui: &mut Ui, id: &str, label: &str, primary: bool) -> bool {
    let t = app.tokens;
    let text = RichText::new(label).size(13.0).color(if primary { Color32::WHITE } else { t.text });
    let b = egui::Button::new(text)
        .min_size(vec2(82.0, 30.0))
        .corner_radius(CornerRadius::same(15))
        .fill(if primary { t.accent } else { Color32::TRANSPARENT })
        .stroke(if primary { Stroke::NONE } else { Stroke::new(1.5, t.text_faint) });
    let r = ui.add(b);
    app.auto.add(id, r.rect, label);
    r.clicked()
}

/// Premiere's "fieldset" group box: a 1-px rounded border with the title inset in the top edge.
fn group(ui: &mut Ui, t: &Tokens, title: &str, add: impl FnOnce(&mut Ui)) {
    ui.add_space(10.0);
    let r = Frame::new()
        .stroke(Stroke::new(1.0, t.field_border))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(Margin { left: 14, right: 14, top: 16, bottom: 10 })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            add(ui);
        });
    let rect = r.response.rect;
    let galley = ui.painter().layout_no_wrap(crate::i18n::t(title).to_string(), Tokens::ui(12.0), t.text_dim);
    let pos = rect.left_top() + vec2(12.0, -galley.size().y / 2.0);
    ui.painter().rect_filled(Rect::from_min_size(pos - vec2(4.0, 0.0), galley.size() + vec2(8.0, 0.0)), 0.0, t.panel_bg);
    ui.painter().galley(pos, galley, t.text_dim);
    ui.add_space(4.0);
}

/// A right-aligned field label in the fixed label column.
fn field_label(ui: &mut Ui, text: &str) {
    let w = LABEL_W.min(ui.available_width() * 0.45);
    ui.allocate_ui_with_layout(vec2(w, 24.0), Layout::right_to_left(Align::Center), |ui| {
        if !text.is_empty() {
            ui.label(RichText::new(format!("{}:", crate::i18n::t(text))).size(12.5));
        }
    });
}

fn unit(ui: &mut Ui, u: &str) {
    if !u.is_empty() {
        ui.label(RichText::new(crate::i18n::t(u)).size(12.5));
    }
}

fn choice_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}

/// A dropdown of (value, label) options; returns the chosen value.
fn combo(app: &mut FilmcraftApp, ui: &mut Ui, key: &str, current: &str, opts: &[(String, String)], width: f32) -> Option<String> {
    let shown = opts.iter().find(|o| o.0 == current).map(|o| o.1.clone()).unwrap_or_else(|| current.to_string());
    let mut chosen = None;
    let mut items: Vec<(String, Rect, String)> = Vec::new();
    let r = egui::ComboBox::from_id_salt(("settings", key)).selected_text(RichText::new(crate::i18n::t(&shown)).size(12.5)).width(width).height(480.0).show_ui(
        ui,
        |ui| {
            for (v, l) in opts {
                let r = ui.selectable_label(v == current, crate::i18n::t(l));
                items.push((format!("settings.{key}.{v}"), r.rect, l.clone()));
                if r.clicked() {
                    chosen = Some(v.clone());
                }
            }
        },
    );
    app.auto.add(&format!("settings.{key}"), r.response.rect, &shown);
    for (id, rect, l) in items {
        app.auto.add(&id, rect, &l);
    }
    chosen
}

fn static_opts(o: &[(&str, &str)]) -> Vec<(String, String)> {
    o.iter().map(|(v, l)| (v.to_string(), l.to_string())).collect()
}

fn device_opts(app: &mut FilmcraftApp, d: &mut SettingsDraft, list: DeviceList) -> Vec<(String, String)> {
    if d.devices.is_none() {
        d.devices = Some(app.audio.as_ref().map(|a| a.devices()).unwrap_or_default());
    }
    let dev = d.devices.clone().unwrap_or_default();
    let names = match list {
        DeviceList::Hosts => dev.hosts,
        DeviceList::Inputs => dev.inputs,
        DeviceList::Outputs => dev.outputs,
    };
    let mut v = vec![(String::new(), tl!("System Default").to_string())];
    v.extend(names.into_iter().map(|n| (n.clone(), n)));
    v
}

fn number(app: &mut FilmcraftApp, ui: &mut Ui, key: &str, value: f64, range: (f64, f64), decimals: usize, label: &str) -> Option<f64> {
    let mut x = value;
    let speed = ((range.1 - range.0) / 2000.0).clamp(0.05, 1.0);
    let dv = egui::DragValue::new(&mut x).range(range.0..=range.1).speed(speed).fixed_decimals(decimals);
    let r = ui.add_sized(vec2(84.0, 24.0), dv);
    app.auto.add(&format!("settings.{key}"), r.rect, label);
    (r.changed() && x != value).then_some(x)
}

fn draw_field(app: &mut FilmcraftApp, ui: &mut Ui, d: &mut SettingsDraft, f: &Field) {
    let enabled = f.enabled_by.is_none_or(|k| get(&d.values, k).as_bool().unwrap_or(true));
    let cur = get(&d.values, f.key).clone();
    ui.add_enabled_ui(enabled, |ui| {
        ui.horizontal(|ui| {
            if f.indent {
                ui.add_space(22.0);
            }
            match f.kind {
                Kind::Bool => {
                    let mut b = cur.as_bool().unwrap_or(false);
                    let r = ui.checkbox(&mut b, RichText::new(crate::i18n::t(f.label)).size(12.5));
                    app.auto.add(&format!("settings.{}", f.key), r.rect, f.label);
                    if r.changed() {
                        put(&mut d.values, f.key, json!(b));
                    }
                }
                Kind::Int { min, max, unit: u } => {
                    field_label(ui, f.label);
                    if let Some(x) = number(app, ui, f.key, cur.as_f64().unwrap_or(min), (min, max), 0, f.label) {
                        put(&mut d.values, f.key, json!(x.round() as u64));
                    }
                    unit(ui, u);
                }
                Kind::Float { min, max, decimals, unit: u } => {
                    field_label(ui, f.label);
                    if let Some(x) = number(app, ui, f.key, cur.as_f64().unwrap_or(min), (min, max), decimals, f.label) {
                        put(&mut d.values, f.key, json!(x));
                    }
                    unit(ui, u);
                }
                Kind::Duration { unit_key } => {
                    field_label(ui, f.label);
                    let u = choice_text(get(&d.values, unit_key));
                    let (range, decimals) = if u == "frames" { ((1.0, 100_000.0), 0) } else { ((0.01, 3600.0), 2) };
                    if let Some(x) = number(app, ui, f.key, cur.as_f64().unwrap_or(1.0), range, decimals, f.label) {
                        put(&mut d.values, f.key, json!(if u == "frames" { x.round() } else { x }));
                    }
                    if let Some(v) = combo(app, ui, unit_key, &u, &static_opts(settings::DURATION_UNITS), 90.0) {
                        put(&mut d.values, unit_key, json!(v));
                    }
                }
                Kind::Choice(opts) => {
                    field_label(ui, f.label);
                    if let Some(v) = combo(app, ui, f.key, &choice_text(&cur), &static_opts(opts), 300.0) {
                        let x = if cur.is_number() { v.parse::<u64>().map(Value::from).unwrap_or(json!(v)) } else { json!(v) };
                        put(&mut d.values, f.key, x);
                    }
                }
                Kind::Device(list) => {
                    field_label(ui, f.label);
                    let opts = device_opts(app, d, list);
                    if let Some(v) = combo(app, ui, f.key, &choice_text(&cur), &opts, 300.0) {
                        put(&mut d.values, f.key, json!(v));
                    }
                }
                Kind::Label => {
                    field_label(ui, f.label);
                    let opts: Vec<(String, String)> = Label::ALL.iter().map(|l| (l.name().to_string(), app.session.prefs.labels.name(*l))).collect();
                    let cur_s = choice_text(&cur);
                    if let Some(l) = Label::from_name(&cur_s) {
                        let (sw, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                        let c = settings::parse_hex(&choice_text(get(&d.values, &format!("labels.colors.{}.color", settings::label_id(l))))).unwrap_or(l.rgb());
                        ui.painter().rect_filled(sw, 2.0, Color32::from_rgb(c[0], c[1], c[2]));
                    }
                    if let Some(v) = combo(app, ui, f.key, &cur_s, &opts, 160.0) {
                        put(&mut d.values, f.key, json!(v));
                    }
                }
                Kind::Text | Kind::Font | Kind::Path => {
                    field_label(ui, f.label);
                    let mut s = choice_text(&cur);
                    let hint = match f.kind {
                        Kind::Path => tl!("Default location"),
                        Kind::Font => tl!("Font family"),
                        _ => "",
                    };
                    let r = ui.add(egui::TextEdit::singleline(&mut s).desired_width(if matches!(f.kind, Kind::Path) { 300.0 } else { 220.0 }).hint_text(hint));
                    app.auto.add(&format!("settings.{}", f.key), r.rect, f.label);
                    if r.changed() {
                        put(&mut d.values, f.key, json!(s));
                    }
                    if matches!(f.kind, Kind::Path) {
                        let b = ui.button(tl!("Browse…"));
                        app.auto.add(&format!("settings.{}.browse", f.key), b.rect, "Browse…");
                        if b.clicked()
                            && let Some(dir) = app.hooks.pick_folder.as_mut().and_then(|p| p())
                        {
                            put(&mut d.values, f.key, json!(dir));
                        }
                    }
                }
                Kind::Color => {
                    field_label(ui, f.label);
                    color_field(app, ui, d, f.key, f.label);
                }
            }
        });
    });
}

/// A colour swatch (opens a picker) and its `#rrggbb` text.
fn color_field(app: &mut FilmcraftApp, ui: &mut Ui, d: &mut SettingsDraft, key: &str, label: &str) {
    let hex = choice_text(get(&d.values, key));
    let mut rgb = settings::parse_hex(&hex).unwrap_or([128, 128, 128]);
    let r = egui::color_picker::color_edit_button_srgb(ui, &mut rgb);
    app.auto.add(&format!("settings.{key}"), r.rect, label);
    if r.changed() {
        put(&mut d.values, key, json!(settings::hex(rgb)));
    }
    let mut text = choice_text(get(&d.values, key));
    let t = ui.add(egui::TextEdit::singleline(&mut text).desired_width(70.0).font(egui::TextStyle::Monospace));
    app.auto.add(&format!("settings.{key}.hex"), t.rect, label);
    if t.changed() && settings::parse_hex(&text).is_some() {
        put(&mut d.values, key, json!(settings::hex(settings::parse_hex(&text).unwrap_or(rgb))));
    }
}

fn note(ui: &mut Ui, t: &Tokens, text: &str) {
    ui.add(egui::Label::new(RichText::new(crate::i18n::t(text)).size(11.5).color(t.text_dim)).wrap());
}

fn mb(bytes: u64) -> String {
    if bytes >= 1 << 30 { format!("{:.1} GB", bytes as f64 / (1u64 << 30) as f64) } else { format!("{:.1} MB", bytes as f64 / (1u64 << 20) as f64) }
}

fn custom(app: &mut FilmcraftApp, ui: &mut Ui, d: &mut SettingsDraft, name: &str) {
    let t = app.tokens;
    match name {
        "labelColors" => {
            let tt = t;
            group(ui, &tt, tl!("Label Colors"), |ui| {
                egui::Grid::new("settings-label-colors").num_columns(2).spacing(vec2(10.0, 5.0)).show(ui, |ui| {
                    for l in Label::ALL.iter() {
                        let id = settings::label_id(*l);
                        let nk = format!("labels.colors.{id}.name");
                        let mut name = choice_text(get(&d.values, &nk));
                        let r = ui.add_sized(vec2(136.0, 22.0), egui::TextEdit::singleline(&mut name));
                        app.auto.add(&format!("settings.{nk}"), r.rect, l.name());
                        if r.changed() {
                            put(&mut d.values, &nk, json!(name));
                        }
                        let ck = format!("labels.colors.{id}.color");
                        let mut rgb = settings::parse_hex(&choice_text(get(&d.values, &ck))).unwrap_or(l.rgb());
                        let c = egui::color_picker::color_edit_button_srgb(ui, &mut rgb);
                        app.auto.add(&format!("settings.{ck}"), c.rect, l.name());
                        if c.changed() {
                            put(&mut d.values, &ck, json!(settings::hex(rgb)));
                        }
                        ui.end_row();
                    }
                });
            });
        }
        "memoryInfo" => {
            let (used, budget) = app.frames.cache_usage();
            note(ui, &t, &tlf!("Decoded frames held for the monitors and thumbnails: {used} of {budget}.", used = mb(used as u64), budget = mb(budget as u64)));
            note(ui, &t, tl!("FilmCraft keeps the rest of the RAM free for other applications."));
        }
        "mediaCacheInfo" => {
            if d.cache_info.is_null() {
                d.cache_info = app.session.execute("mediaCache.info", json!({})).unwrap_or(json!({}));
            }
            let loc = d.cache_info["location"].as_str().unwrap_or(tl!("not available in this session")).to_string();
            let size = mb(d.cache_info["bytes"].as_u64().unwrap_or(0));
            let files = d.cache_info["files"].as_u64().unwrap_or(0);
            note(ui, &t, &tlf!("Currently: {loc} — {files} file(s), {size}", loc, files, size));
        }
        "outputMapping" => {
            let ch = d.devices.as_ref().map(|x| x.output_channels).unwrap_or(0);
            let l = get(&d.values, "audioHardware.mapLeft").as_u64().unwrap_or(0) + 1;
            let r = get(&d.values, "audioHardware.mapRight").as_u64().unwrap_or(1) + 1;
            let dev = if ch > 0 { tlf!(" ({ch} output channels)", ch) } else { String::new() };
            note(
                ui,
                &t,
                &tlf!(
                    "Programme left → device output {l}, right → device output {r}{dev}. Outputs beyond the device's channels fall back to 1 and 2.",
                    l,
                    r,
                    dev
                ),
            );
        }
        "autoSaveStatus" => {
            let status = app.session.execute("file.autoSaveStatus", json!({})).unwrap_or_default();
            if let Some(dir) = status["autoSaveDir"].as_str() {
                note(ui, &t, &tlf!("Auto-saves go to: {dir}", dir));
            }
            if let Some(dir) = status["sessionDir"].as_str() {
                note(ui, &t, &tlf!("Recovery copy: {dir}", dir));
            }
            let last = match (status["lastAutoSaveAt"].as_str(), status["lastJournalAt"].as_str()) {
                (Some(a), Some(j)) => tlf!("Last auto-save {a} · last recovery copy {j}", a, j),
                (Some(a), None) => tlf!("Last auto-save {a}", a),
                (None, Some(j)) => tlf!("Last recovery copy {j}", j),
                (None, None) => String::new(),
            };
            if !last.is_empty() {
                note(ui, &t, &last);
            }
        }
        _ => {}
    }
}

fn rows(app: &mut FilmcraftApp, ui: &mut Ui, d: &mut SettingsDraft, list: &[Row]) {
    let t = app.tokens;
    let mut skip = false;
    for (i, r) in list.iter().enumerate() {
        if std::mem::take(&mut skip) {
            continue;
        }
        // Labels: Label Colors on the left, the row after it (Label Defaults) on the right
        if matches!(r, Row::Custom("labelColors")) {
            let next = list.get(i + 1).copied();
            ui.horizontal_top(|ui| {
                ui.allocate_ui(vec2(250.0, 0.0), |ui| {
                    ui.vertical(|ui| custom(app, ui, d, "labelColors"));
                });
                ui.add_space(12.0);
                if let Some(n) = next {
                    ui.vertical(|ui| rows(app, ui, d, &[n]));
                }
            });
            skip = true;
            continue;
        }
        match r {
            Row::Field(f) => draw_field(app, ui, d, f),
            Row::Group(title, inner) => group(ui, &t, title, |ui| rows(app, ui, d, inner)),
            Row::Note(text) => note(ui, &t, text),
            Row::Button { id, label, command } => {
                ui.horizontal(|ui| {
                    field_label(ui, tl!("Remove Media Cache Files"));
                    let b = ui.button(crate::i18n::t(label));
                    app.auto.add(&format!("settings.{id}"), b.rect, label);
                    if b.clicked() {
                        d.message = match app.session.execute(command, json!({})) {
                            Ok(v) => tlf!("Deleted {n} file(s), {size}.", n = v["files"].as_u64().unwrap_or(0), size = mb(v["bytes"].as_u64().unwrap_or(0))),
                            Err(e) => e.to_string(),
                        };
                        d.cache_info = Value::Null;
                    }
                });
            }
            Row::Custom(name) => custom(app, ui, d, name),
        }
        ui.add_space(2.0);
    }
}

/// Draw the dialog; returns whether it stays open.
pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) -> bool {
    let Some(mut d) = app.ui.settings.take() else { return false };
    let t = app.tokens;
    let mut close = false;
    let mut apply = false;
    let resp = egui::Modal::new(egui::Id::new("settings-modal")).frame(modal_frame(&t)).show(ctx, |ui| {
        ui.set_width(W);
        // title bar (Premiere keeps "Preferences" as the window title)
        let (bar, _) = ui.allocate_exact_size(vec2(W, 30.0), Sense::hover());
        ui.painter().rect_filled(bar, CornerRadius { nw: 10, ne: 10, sw: 0, se: 0 }, t.header_bg);
        ui.painter().text(bar.center(), Align2::CENTER_CENTER, tl!("Preferences"), Tokens::semibold(13.0), t.text_dim);
        app.auto.add("settings.window", bar, "Preferences");
        ui.add_space(14.0);
        ui.horizontal_top(|ui| {
            ui.add_space(20.0);
            // category list
            let (list, _) = ui.allocate_exact_size(vec2(LIST_W, BODY_H), Sense::hover());
            ui.painter().rect(list, 2.0, t.field_bg, Stroke::new(1.0, t.separator), egui::StrokeKind::Inside);
            for (i, c) in settings::categories().iter().enumerate() {
                let row = Rect::from_min_size(list.min + vec2(1.0, 1.0 + i as f32 * ROW_H), vec2(LIST_W - 2.0, ROW_H));
                let r = ui.interact(row, egui::Id::new(("settings-cat", c.id)), Sense::click());
                let sel = d.page == c.id;
                if sel {
                    ui.painter().rect_filled(row, 0.0, t.row_alt);
                } else if r.hovered() {
                    ui.painter().rect_filled(row, 0.0, t.hover.gamma_multiply(0.6));
                }
                ui.painter().text(
                    row.left_center() + vec2(8.0, 0.0),
                    Align2::LEFT_CENTER,
                    crate::i18n::t(c.title),
                    Tokens::ui(12.5),
                    if sel { t.text } else { t.text_dim },
                );
                app.auto.add(&format!("settings.category.{}", c.id), row, c.title);
                if r.clicked() {
                    d.page = c.id.into();
                    d.message.clear();
                }
            }
            ui.add_space(20.0);
            // the page
            let Some(cat) = settings::category(&d.page) else { return };
            ui.vertical(|ui| {
                ui.set_width(W - LIST_W - 60.0);
                ui.set_height(BODY_H);
                egui::ScrollArea::vertical().id_salt(("settings-page", cat.id)).auto_shrink([false, false]).max_height(BODY_H).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    rows(app, ui, &mut d, cat.rows);
                    if !d.message.is_empty() {
                        ui.add_space(6.0);
                        note(ui, &t, &d.message);
                    }
                });
            });
        });
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.add_space(20.0);
            if button(app, ui, "settings.help", tl!("Help"), false) {
                crate::links::open(ctx, &format!("{}/blob/main/docs/project-files.md#settings", crate::links::GITHUB));
            }
            if button(app, ui, "settings.reset", tl!("Reset…"), false) {
                // this category back to its defaults (OK applies it)
                let defaults = filmcraft_engine::autosave::Preferences::default().to_value();
                let keep = get(&d.values, "general.recentProjects").clone();
                d.values[d.page.as_str()] = defaults[d.page.as_str()].clone();
                put(&mut d.values, "general.recentProjects", keep);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(20.0);
                if button(app, ui, "settings.ok", tl!("OK"), true) {
                    apply = true;
                }
                if button(app, ui, "settings.cancel", tl!("Cancel"), false) {
                    close = true;
                }
            });
        });
        ui.add_space(14.0);
    });
    if apply {
        let mut changes = Map::new();
        diff("", &app.session.prefs.to_value(), &d.values, &mut changes);
        match if changes.is_empty() { Ok(Value::Null) } else { app.session.execute("prefs.set", json!({ "values": changes })) } {
            Ok(_) => {
                close = true;
                app.apply_prefs(ctx);
            }
            Err(e) => d.message = e.to_string(),
        }
    }
    if resp.should_close() || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        close = true;
    }
    if !close {
        app.ui.settings = Some(d);
    }
    !close
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_lists_changed_leaves() {
        let a = json!({"timeline": {"x": 1, "y": "a"}, "labels": {"colors": {"rose": {"name": "Rose"}}}});
        let b = json!({"timeline": {"x": 2, "y": "a"}, "labels": {"colors": {"rose": {"name": "Hero"}}}});
        let mut out = Map::new();
        diff("", &a, &b, &mut out);
        assert_eq!(Value::Object(out), json!({"timeline.x": 2, "labels.colors.rose.name": "Hero"}));
    }

    #[test]
    fn selection_tool_trim_kinds() {
        use crate::panels::timeline::selection_trim_kind as k;
        assert_eq!(k(false, false, false, 0.0, true), "trim");
        assert_eq!(k(false, true, false, 0.0, true), "ripple");
        assert_eq!(k(false, true, true, 5.0, true), "roll");
        // Settings ▸ Trim ▸ Allow Selection tool to choose Roll and Ripple trims without modifier key
        assert_eq!(k(true, false, false, 1.0, true), "roll");
        assert_eq!(k(true, false, false, 6.0, true), "ripple");
        assert_eq!(k(true, false, false, 1.0, false), "ripple", "no clip on the other side: ripple");
    }
}
