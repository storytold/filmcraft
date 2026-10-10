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

/// The header's appearance button and View ▸ Appearance ▸ Next Appearance Mode: Auto, Light, Dark,
/// then Auto again. The light and dark theme choices are kept.
pub fn cycle_appearance(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let next = app.session.prefs.appearance.next_mode();
    if let Err(e) = app.session.execute("prefs.set", json!({"key": "appearance.appearanceMode", "value": next})) {
        app.ui.status = e.to_string();
    }
    app.apply_prefs(ctx);
}

/// Keys the Appearance page draws as the mode selector and theme cards instead of plain fields.
const APPEARANCE_KEYS: &[&str] = &["appearance.appearanceMode", "appearance.lightTheme", "appearance.darkTheme", "appearance.colorTheme"];

/// A small FilmCraft editor painted with `t`'s colours: the header with the Import / Edit / Export
/// tabs, the Source and Program monitors, the Project panel, the tools, the Timeline with video and
/// audio clips and the playhead, and the audio meters (the Editing workspace).
fn theme_preview(ui: &mut Ui, t: &Tokens, width: f32) {
    use egui::pos2;
    let (rect, _) = ui.allocate_exact_size(vec2(width, 128.0), Sense::hover());
    let p = ui.painter_at(rect);
    let bar = |r: Rect, w: f32, c: Color32| p.rect_filled(Rect::from_min_size(r.min, vec2(w, 2.0)), 1.0, c);
    p.rect_filled(rect, 3.0, t.app_bg);
    // header: home, the three mode tabs (Edit active, underlined), the title, quick actions
    let header = Rect::from_min_size(rect.min, vec2(rect.width(), 12.0));
    p.rect_filled(header, CornerRadius { nw: 3, ne: 3, sw: 0, se: 0 }, t.header_bg);
    p.rect_filled(Rect::from_center_size(pos2(header.left() + 7.0, header.center().y), vec2(4.0, 4.0)), 1.0, t.text_dim);
    for (i, w) in [9.0, 6.0, 9.0].into_iter().enumerate() {
        let x = header.left() + 14.0 + i as f32 * 13.0;
        let c = if i == 1 { t.tab_text_active } else { t.tab_text };
        bar(Rect::from_min_size(pos2(x, header.center().y - 1.0), vec2(w, 2.0)), w, c);
        if i == 1 {
            p.rect_filled(Rect::from_min_size(pos2(x, header.bottom() - 2.5), vec2(w, 1.0)), 0.0, c);
        }
    }
    bar(Rect::from_min_size(pos2(header.center().x - 16.0, header.center().y - 1.0), vec2(32.0, 2.0)), 32.0, t.tab_text_active);
    for i in 0..4 {
        p.rect_filled(Rect::from_center_size(pos2(header.right() - 7.0 - i as f32 * 8.0, header.center().y), vec2(4.0, 4.0)), 1.0, t.icon);
    }
    let body = Rect::from_min_max(pos2(rect.left() + 2.0, header.bottom() + 2.0), rect.max - vec2(2.0, 2.0));
    let split_y = body.top() + body.height() * 0.5;
    let panel = |r: Rect, tab_w: f32| {
        p.rect_filled(r, 2.0, t.panel_bg);
        bar(Rect::from_min_size(r.min + vec2(4.0, 3.5), vec2(tab_w, 2.0)), tab_w, t.tab_text_active);
        Rect::from_min_max(r.min + vec2(3.0, 9.0), r.max - vec2(3.0, 3.0))
    };
    // Source monitor (left) and Program monitor (right)
    let source = panel(Rect::from_min_max(body.min, pos2(body.left() + body.width() * 0.36, split_y - 1.0)), 14.0);
    p.rect_filled(source, 0.0, t.monitor_bg);
    p.rect_filled(source.shrink2(vec2(6.0, 8.0)), 0.0, t.field_bg);
    let program = panel(Rect::from_min_max(pos2(body.left() + body.width() * 0.36 + 2.0, body.top()), pos2(body.right(), split_y - 1.0)), 16.0);
    p.rect_filled(program, 0.0, t.monitor_bg);
    let frame = Rect::from_center_size(program.center() - vec2(0.0, 3.0), vec2((program.height() - 9.0) * 16.0 / 9.0, program.height() - 9.0));
    p.rect_filled(frame, 0.0, Color32::from_rgb(0x2c, 0x4a, 0x2e));
    p.rect_filled(Rect::from_min_max(pos2(frame.left(), frame.bottom() - frame.height() * 0.3), frame.max), 0.0, Color32::from_rgb(0x4a, 0x3c, 0x30));
    p.circle_filled(pos2(frame.left() + frame.width() * 0.62, frame.center().y + 2.0), frame.height() * 0.26, Color32::from_rgb(0xc8, 0x2a, 0x26));
    bar(Rect::from_min_size(pos2(program.left() + 2.0, program.bottom() - 3.0), vec2(14.0, 2.0)), 14.0, t.timecode);
    // Project panel, tools, Timeline, meters
    let low = split_y + 1.0;
    let project = panel(Rect::from_min_max(pos2(body.left(), low), pos2(body.left() + body.width() * 0.24, body.bottom())), 12.0);
    for row in 0..2 {
        for col in 0..2 {
            let cell =
                Rect::from_min_size(project.min + vec2(col as f32 * (project.width() / 2.0), 2.0 + row as f32 * 18.0), vec2(project.width() / 2.0 - 3.0, 11.0));
            p.rect_filled(cell, 1.0, if (row + col) % 2 == 0 { t.field_bg } else { t.row_selected });
            bar(Rect::from_min_size(cell.left_bottom() + vec2(0.0, 2.0), vec2(cell.width() * 0.7, 2.0)), cell.width() * 0.7, t.text_faint);
        }
    }
    let tools = Rect::from_min_max(pos2(project.right() + 5.0, low), pos2(project.right() + 15.0, body.bottom()));
    p.rect_filled(tools, 2.0, t.panel_bg);
    for i in 0..5 {
        let c = if i == 0 { t.accent } else { t.icon };
        p.rect_filled(Rect::from_center_size(pos2(tools.center().x, tools.top() + 7.0 + i as f32 * 9.0), vec2(5.0, 5.0)), 1.0, c);
    }
    let meters = Rect::from_min_max(pos2(body.right() - 12.0, low), pos2(body.right(), body.bottom()));
    p.rect_filled(meters, 2.0, t.panel_bg);
    for (i, h) in [0.55, 0.62].into_iter().enumerate() {
        let x = meters.left() + 3.0 + i as f32 * 4.0;
        p.rect_filled(
            Rect::from_min_max(pos2(x, meters.bottom() - 3.0 - (meters.height() - 6.0) * h), pos2(x + 2.0, meters.bottom() - 3.0)),
            0.0,
            t.render_green,
        );
    }
    let tl = Rect::from_min_max(pos2(tools.right() + 2.0, low), pos2(meters.left() - 2.0, body.bottom()));
    p.rect_filled(tl, 2.0, t.tl_bg);
    bar(Rect::from_min_size(tl.min + vec2(4.0, 3.5), vec2(14.0, 2.0)), 14.0, t.tab_text_active);
    bar(Rect::from_min_size(tl.min + vec2(4.0, 9.0), vec2(16.0, 2.0)), 16.0, t.timecode);
    let heads_w = 22.0;
    let ruler = Rect::from_min_max(pos2(tl.left() + heads_w, tl.top() + 7.0), pos2(tl.right() - 2.0, tl.top() + 13.0));
    p.rect_filled(ruler, 0.0, t.tl_ruler_bg);
    let mut x = ruler.left() + 2.0;
    while x < ruler.right() {
        p.line_segment([pos2(x, ruler.bottom() - 2.0), pos2(x, ruler.bottom())], Stroke::new(1.0, t.tl_ruler_tick));
        x += 6.0;
    }
    let video = Label::Iris.rgb();
    let audio = Label::Caribbean.rgb();
    let music = Label::Forest.rgb();
    let track_h = ((tl.bottom() - ruler.bottom() - 3.0) / 4.0).max(4.0);
    for i in 0..4 {
        let y = ruler.bottom() + 1.0 + i as f32 * track_h;
        let head = Rect::from_min_size(pos2(tl.left() + 2.0, y), vec2(heads_w - 3.0, track_h - 1.0));
        p.rect_filled(head, 1.0, t.tl_header_bg);
        p.rect_filled(Rect::from_min_size(head.min + vec2(2.0, 1.5), vec2(5.0, track_h - 4.0)), 1.0, t.accent);
        let lane = Rect::from_min_max(pos2(ruler.left(), y), pos2(ruler.right(), y + track_h - 1.0));
        p.rect_filled(lane, 0.0, if i % 2 == 0 { t.tl_track_bg } else { t.tl_track_bg_alt });
        let clips: &[(f32, f32)] = match i {
            0 => &[(0.55, 0.75)],
            1 | 2 => &[(0.0, 0.22), (0.23, 0.48), (0.49, 0.7), (0.71, 0.9)],
            _ => &[(0.0, 0.9)],
        };
        let c = match i {
            0 | 1 => video,
            2 => audio,
            _ => music,
        };
        for (a, b) in clips {
            let r = Rect::from_min_max(pos2(lane.left() + lane.width() * a, lane.top() + 0.5), pos2(lane.left() + lane.width() * b, lane.bottom() - 0.5));
            p.rect_filled(r, 1.0, Color32::from_rgb(c[0], c[1], c[2]).gamma_multiply(0.85));
        }
    }
    let ph = ruler.left() + ruler.width() * 0.3;
    p.line_segment([pos2(ph, ruler.top()), pos2(ph, tl.bottom() - 2.0)], Stroke::new(1.0, t.playhead));
}

/// One theme card of the Appearance page: title, "Active" while the mode shows it, the preview and
/// a radio button per theme (automation ids `settings.<key>.<value>`).
#[allow(clippy::too_many_arguments)]
fn theme_card(app: &mut FilmcraftApp, ui: &mut Ui, d: &mut SettingsDraft, key: &str, title: &str, opts: &[(&str, &str)], active: bool, width: f32) {
    let t = app.tokens;
    let current = choice_text(get(&d.values, key));
    let shown = opts.iter().find(|(v, _)| *v == current).or(opts.first()).map_or("darkest", |(v, _)| v);
    let highlight = settings::parse_hex(&choice_text(get(&d.values, "appearance.highlightColor")));
    let contrast = get(&d.values, "appearance.accessibleContrast").as_bool().unwrap_or(false);
    let look = Tokens::for_kind(crate::theme::ThemeKind::from_pref(shown)).with_appearance(highlight, contrast);
    let r = Frame::new()
        .fill(t.field_bg)
        .stroke(Stroke::new(1.0, if active { t.accent } else { t.field_border }))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::same(10))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.set_width(width - 20.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(crate::i18n::t(title)).size(12.5).strong());
                    if active {
                        ui.label(RichText::new(tl!("Active")).size(11.0).color(t.accent));
                    }
                });
                ui.add_space(4.0);
                theme_preview(ui, &look, width - 20.0);
                ui.add_space(4.0);
                for (v, l) in opts {
                    let r = ui.radio(current == *v, RichText::new(crate::i18n::t(l)).size(12.5));
                    app.auto.add(&format!("settings.{key}.{v}"), r.rect, l);
                    if r.clicked() {
                        put(&mut d.values, key, json!(v));
                    }
                }
            });
        });
    app.auto.add(&format!("settings.{key}"), r.response.rect, title);
}

/// Settings ▸ Appearance: the Appearance Mode selector above the light and dark theme cards.
fn appearance_modes(app: &mut FilmcraftApp, ui: &mut Ui, d: &mut SettingsDraft) {
    let key = "appearance.appearanceMode";
    let mode = choice_text(get(&d.values, key));
    ui.horizontal(|ui| {
        field_label(ui, "Appearance Mode");
        if let Some(v) = combo(app, ui, key, &mode, &static_opts(settings::APPEARANCE_MODES), 200.0) {
            put(&mut d.values, key, json!(v));
        }
    });
    let mode = choice_text(get(&d.values, key));
    let light_active = mode == "light" || (mode == "auto" && app.system_theme(ui.ctx()) == Some(egui::Theme::Light));
    ui.add_space(6.0);
    let width = ((ui.available_width() - 12.0) / 2.0).max(200.0);
    ui.horizontal_top(|ui| {
        theme_card(app, ui, d, "appearance.lightTheme", "Light Theme", settings::LIGHT_THEMES, light_active, width);
        ui.add_space(4.0);
        theme_card(app, ui, d, "appearance.darkTheme", "Dark Theme", settings::DARK_THEMES, !light_active, width);
    });
    ui.add_space(6.0);
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
        "appearanceModes" => appearance_modes(app, ui, d),
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
            Row::Field(f) if APPEARANCE_KEYS.contains(&f.key) => continue,
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
                crate::links::open(app, ctx, &format!("{}/blob/main/docs/project-files.md#settings", crate::links::GITHUB));
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
    use crate::theme::ThemeKind;

    /// #457: on a desktop that reports 150%, UI Scale 100% makes a point one physical pixel. It is
    /// applied once, so Ctrl+- still zooms for the session, and Auto gives the system's scale back.
    #[test]
    fn ui_scale_overrides_the_system_scale_and_auto_restores_it() {
        let ctx = egui::Context::default();
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        // a frame applying the settings, then the next one, where a new zoom takes effect
        let pass = |app: &mut FilmcraftApp| {
            let mut input = egui::RawInput::default();
            input.viewports.entry(egui::ViewportId::ROOT).or_default().native_pixels_per_point = Some(1.5);
            ctx.run_ui(input.clone(), |ui| app.apply_prefs(ui.ctx())).textures_delta.clear();
            ctx.run_ui(input, |_| {}).textures_delta.clear();
            ctx.zoom_factor()
        };
        assert_eq!(pass(&mut app), 1.0, "Auto keeps the system's scale");
        app.session.execute("prefs.set", json!({"key": "appearance.uiScale", "value": "100"})).unwrap();
        assert_eq!(pass(&mut app), 1.0 / 1.5, "100% is one physical pixel per point");
        ctx.set_zoom_factor(0.9 / 1.5); // Ctrl+-
        assert_eq!(pass(&mut app), 0.9 / 1.5, "a session zoom isn't undone every frame");
        assert!(app.session.execute("prefs.set", json!({"key": "appearance.uiScale", "value": "130"})).is_err());
        app.session.execute("prefs.set", json!({"key": "appearance.uiScale", "value": "auto"})).unwrap();
        assert_eq!(pass(&mut app), 1.0);
    }

    #[test]
    fn appearance_button_cycles_modes_without_losing_theme_choices() {
        let ctx = egui::Context::default();
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        app.apply_prefs(&ctx);
        assert_eq!((app.session.prefs.appearance.appearance_mode.as_str(), app.tokens.kind), ("dark", ThemeKind::Dark), "new users keep Darkest");
        app.session.execute("prefs.set", json!({"values": {"appearance.darkTheme": "dark"}})).unwrap();
        app.hooks.system_theme = Some(Box::new(|_| Some(egui::Theme::Dark)));
        for (mode, shown) in [("auto", ThemeKind::Medium), ("light", ThemeKind::Light), ("dark", ThemeKind::Medium), ("auto", ThemeKind::Medium)] {
            cycle_appearance(&mut app, &ctx);
            assert_eq!(app.session.prefs.appearance.appearance_mode, mode);
            assert_eq!(app.tokens.kind, shown, "{mode}");
            assert_eq!((app.session.prefs.appearance.dark_theme.as_str(), app.session.prefs.appearance.light_theme.as_str()), ("dark", "light"));
        }
        // View ▸ Appearance picks a theme and fixes the mode to its family
        set_theme(&mut app, &ctx, ThemeKind::Light);
        assert_eq!((app.session.prefs.appearance.appearance_mode.as_str(), app.tokens.kind), ("light", ThemeKind::Light));
        set_theme(&mut app, &ctx, ThemeKind::Dark);
        assert_eq!((app.session.prefs.appearance.appearance_mode.as_str(), app.tokens.kind), ("dark", ThemeKind::Dark));
        assert_eq!(app.session.prefs.appearance.dark_theme, "darkest");
    }

    /// Auto follows the host's reading of the system appearance as it changes, and falls back to
    /// dark without an answer.
    #[test]
    fn auto_follows_a_stubbed_system_theme() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU8, Ordering};
        let ctx = egui::Context::default();
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        let system = Arc::new(AtomicU8::new(2));
        let read = system.clone();
        app.hooks.system_theme = Some(Box::new(move |_| match read.load(Ordering::Relaxed) {
            1 => Some(egui::Theme::Dark),
            2 => Some(egui::Theme::Light),
            _ => None,
        }));
        app.session.execute("prefs.set", json!({"key": "appearance.appearanceMode", "value": "auto"})).unwrap();
        app.apply_prefs(&ctx);
        assert_eq!(app.tokens.kind, ThemeKind::Light);
        assert!(!app.ui.dark);
        assert_eq!(ctx.theme(), egui::Theme::Light, "egui's style follows FilmCraft's choice");
        system.store(1, Ordering::Relaxed);
        app.apply_prefs(&ctx);
        assert_eq!(app.tokens.kind, ThemeKind::Dark);
        assert_eq!(ctx.theme(), egui::Theme::Dark);
        system.store(2, Ordering::Relaxed);
        app.apply_prefs(&ctx);
        assert_eq!(app.tokens.kind, ThemeKind::Light);
        system.store(0, Ordering::Relaxed);
        app.apply_prefs(&ctx);
        assert_eq!(app.tokens.kind, ThemeKind::Dark, "no answer: dark");
        // a fixed mode ignores the system
        app.session.execute("prefs.set", json!({"key": "appearance.appearanceMode", "value": "dark"})).unwrap();
        system.store(2, Ordering::Relaxed);
        app.apply_prefs(&ctx);
        assert_eq!(app.tokens.kind, ThemeKind::Dark);
    }

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
