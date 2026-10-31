//! Edit ▸ Keyboard Shortcuts… (⌥⌘K): a keyboard drawn in code with every assigned key coloured
//! (application-wide / panel-specific / both), a searchable command list with a shortcut column
//! (click it and press keys to assign; "+" adds another shortcut), the "Key:" detail list,
//! conflict warnings, preset load / Save As / Delete / Export / Import, and Undo / Redo / Clear.
//! Edits apply live (menus update immediately); Cancel restores the set the dialog opened with.
//!
//! All state lives in the engine's shortcut set (`shortcuts.*` commands), so agents can do the
//! same over MCP. Automation ids: `shortcuts.preset`, `shortcuts.preset.<name>`, `shortcuts.saveAs`,
//! `shortcuts.saveAs.name`, `shortcuts.saveAs.ok`, `shortcuts.delete`, `shortcuts.export`,
//! `shortcuts.import`, `shortcuts.context`, `shortcuts.context.<panel>`, `shortcuts.search`,
//! `shortcuts.key.<Key>`, `shortcuts.mod.<Cmd|Ctrl|Alt|Shift>`, `shortcuts.row.<command>`,
//! `shortcuts.cell.<command>` (click = record), `shortcuts.add.<command>`, `shortcuts.assignKey`,
//! `shortcuts.undo`, `shortcuts.redo`, `shortcuts.clear`, `shortcuts.cancel`, `shortcuts.ok`.

use egui::text::LayoutJob;
use egui::{Align2, Color32, CornerRadius, FontId, Rect, RichText, Sense, Stroke, StrokeKind, TextFormat, pos2, vec2};
use filmcraft_engine::shortcuts::{APPLICATION, Chord, KEYS, Mods, PANELS, Platform};
use serde::Serialize;
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::native_dialogs::FileDialog;
use crate::theme::Tokens;

/// Key colours (our own palette): application-wide, panel-specific, unassigned, held modifier.
pub const APP_KEY: Color32 = Color32::from_rgb(0x74, 0x57, 0xc8);
pub const PANEL_KEY: Color32 = Color32::from_rgb(0x4a, 0xa6, 0x84);
pub const FREE_KEY: Color32 = Color32::from_rgb(0x5a, 0x5a, 0x5e);
const MOD_KEY: Color32 = Color32::from_rgb(0x2f, 0x62, 0xc0);
const WARN: Color32 = Color32::from_rgb(0xe8, 0xa0, 0x3c);

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorState {
    pub search: String,
    /// "Application" or a panel name (which panel's shortcuts the keyboard and assignments use).
    pub context: String,
    pub selected: Option<String>,
    /// Waiting for a key press: (command, add instead of replace).
    pub recording: Option<(String, bool)>,
    /// Modifier filter toggled on the drawn keyboard.
    pub mods: Mods,
    /// Key whose assignments the "Key:" list shows.
    pub key: Option<String>,
    pub message: String,
    /// Save As… name being typed (Some = the field is open).
    pub save_name: Option<String>,
}

impl EditorState {
    fn panel(&self) -> Option<&str> {
        if self.context.is_empty() || self.context == APPLICATION { None } else { Some(self.context.as_str()) }
    }
}

/// Open the dialog (Edit ▸ Keyboard Shortcuts…).
pub fn open(app: &mut FilmcraftApp) {
    app.session.shortcuts.begin_editing();
    app.shortcut_editor = EditorState { context: APPLICATION.into(), ..Default::default() };
    app.dialog = Some(crate::Dialog::Shortcuts);
}

/// The egui key for a canonical key name.
fn egui_key(name: &str) -> Option<egui::Key> {
    crate::menus::parse_shortcut(name).map(|(_, k)| k)
}

/// Canonical chord text from an egui key event.
pub fn chord_of(key: egui::Key, m: egui::Modifiers) -> Option<String> {
    let name = KEYS.iter().find(|k| egui_key(k) == Some(key))?;
    let mods = Mods { cmd: m.command || m.mac_cmd, ctrl: m.ctrl && cfg!(target_os = "macos"), alt: m.alt, shift: m.shift };
    Some(format!("{}{}", mods.prefix(), name))
}

fn label_of(app: &FilmcraftApp, id: &str) -> String {
    app.session
        .shortcuts
        .command(id)
        .map(|c| c.label)
        .or_else(|| filmcraft_engine::find_command(id).map(|c| c.label.to_string()))
        .map_or_else(|| id.to_string(), |l| crate::i18n::t(&l).to_string())
}

fn exec(app: &mut FilmcraftApp, cmd: &str, params: Value) -> Option<Value> {
    match app.session.execute(cmd, params) {
        Ok(v) => Some(v),
        Err(e) => {
            app.shortcut_editor.message = e.to_string();
            None
        }
    }
}

/// Assign a recorded chord.
fn assign(app: &mut FilmcraftApp, command: &str, keys: &str, add: bool) {
    let panel = app.shortcut_editor.panel().map(str::to_string);
    let p = Platform::current();
    if let Some(r) = exec(app, "shortcuts.set", json!({"command": command, "keys": keys, "panel": panel, "add": add})) {
        let disp = Chord::parse(keys).map(|c| c.display(p)).unwrap_or_else(|_| keys.into());
        let mut msg = format!("{disp} → {}", label_of(app, command));
        let moved: Vec<String> = r["reassigned"].as_array().into_iter().flatten().filter_map(|b| b["command"].as_str().map(|c| label_of(app, c))).collect();
        if !moved.is_empty() {
            msg.push_str(&tlf!(" (removed from {list})", list = moved.join(", ")));
        }
        app.shortcut_editor.message = msg;
    }
}

/// A modifier key on its own. egui reports pressing Shift, Ctrl, Alt or the Windows / Cmd key as a
/// key event of its own, before the key it modifies arrives (#164).
fn is_modifier_key(key: egui::Key) -> bool {
    use egui::Key::*;
    matches!(key, ShiftLeft | ShiftRight | ControlLeft | ControlRight | AltLeft | AltRight | SuperLeft | SuperRight)
}

/// Take the next key press while recording (Esc cancels). Modifier keys on their own are skipped:
/// the chord is recorded when the key they modify is pressed, with them as its modifiers.
fn record(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some((cmd, add)) = app.shortcut_editor.recording.clone() else { return };
    let got = ctx.input_mut(|i| {
        let mut got = None;
        i.events.retain(|e| match e {
            egui::Event::Key { key, pressed: true, modifiers, .. } if got.is_none() && !is_modifier_key(*key) => {
                got = Some((*key, *modifiers));
                false
            }
            egui::Event::Key { .. } | egui::Event::Text(_) => false,
            _ => true,
        });
        got
    });
    let Some((key, m)) = got else { return };
    app.shortcut_editor.recording = None;
    if key == egui::Key::Escape && !m.any() {
        app.shortcut_editor.message = tl!("Cancelled").into();
        return;
    }
    match chord_of(key, m) {
        Some(k) => assign(app, &cmd, &k, add),
        None => app.shortcut_editor.message = tlf!("{key} can't be used as a shortcut", key = format!("{key:?}")),
    }
}

/// Show the dialog; false when it closed.
pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) -> bool {
    record(app, ctx);
    let t = app.tokens;
    let screen = ctx.content_rect();
    let size = dialog_size(screen.size());
    let mut close: Option<bool> = None; // Some(true) = OK, Some(false) = Cancel
    let mut open = true;
    egui::Window::new(tl!("Keyboard Shortcuts"))
        .id(egui::Id::unique("Keyboard Shortcuts"))
        .id(egui::Id::unique("keyboard-shortcuts"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .fixed_size(size)
        .pivot(Align2::CENTER_CENTER)
        .default_pos(crate::widgets::dialog_center(ctx))
        .frame(egui::Frame::window(&ctx.global_style()).fill(t.panel_bg).inner_margin(14.0))
        .show(ctx, |ui| {
            ui.set_width(size.x);
            header(app, ui, &t);
            ui.add_space(8.0);
            let kb_h = keyboard_height(size);
            let (kb, _) = ui.allocate_exact_size(vec2(size.x, kb_h), Sense::hover());
            keyboard(app, ui, kb, &t);
            legend(ui, &t);
            ui.add_space(6.0);
            let rest_h = (size.y - kb_h - 160.0).max(160.0);
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(size.x * 0.6);
                    command_list(app, ui, rest_h, &t);
                });
                ui.add_space(12.0);
                ui.vertical(|ui| {
                    ui.set_width(size.x * 0.4 - 14.0);
                    key_detail(app, ui, rest_h, &t);
                });
            });
            ui.add_space(6.0);
            close = footer(app, ui, &t);
        });
    if !open {
        close = Some(false);
    }
    match close {
        Some(ok) => {
            if ok {
                app.session.shortcuts.end_editing();
            } else {
                app.session.shortcuts.cancel_editing();
            }
            app.shortcut_editor.recording = None;
            false
        }
        None => true,
    }
}

/// The dialog's inner size on a screen of `screen`.
fn dialog_size(screen: egui::Vec2) -> egui::Vec2 {
    vec2((screen.x - 40.0).clamp(760.0, 1320.0), (screen.y - 60.0).clamp(560.0, 900.0))
}

/// Height of the drawn keyboard in a dialog of `size`.
fn keyboard_height(size: egui::Vec2) -> f32 {
    (size.y * 0.42).clamp(220.0, 360.0)
}

fn small_button(app: &mut FilmcraftApp, ui: &mut egui::Ui, id: &str, label: &str, enabled: bool) -> bool {
    let r = ui.add_enabled(enabled, egui::Button::new(RichText::new(label).size(12.5)).min_size(vec2(0.0, 24.0)));
    app.auto.add(id, r.rect, label);
    r.clicked()
}

fn header(app: &mut FilmcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let presets = app.session.execute("shortcuts.presets", json!({})).unwrap_or_default();
    let active = presets["active"].as_str().unwrap_or("").to_string();
    let modified = presets["modified"].as_bool().unwrap_or(false);
    let builtin: Vec<String> = presets["builtin"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect();
    let custom: Vec<String> = presets["custom"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect();
    ui.horizontal(|ui| {
        ui.label(RichText::new(tl!("Preset:")).size(13.0).color(t.text_dim));
        let shown = if modified { tlf!("{active} (modified)", active) } else { active.clone() };
        let r = craft_ui::ComboBox::from_id_salt("shortcuts-preset").width(280.0).selected_text(shown).show_ui(ui, |ui| {
            for name in builtin.iter().chain(custom.iter()) {
                let r = ui.selectable_label(*name == active && !modified, name);
                app.auto.add(&format!("shortcuts.preset.{name}"), r.rect, name);
                if r.clicked() {
                    exec(app, "shortcuts.loadPreset", json!({"name": name}));
                    app.shortcut_editor.message = tlf!("Loaded “{name}”", name);
                }
            }
        });
        app.auto.add("shortcuts.preset", r.response.rect, "Preset");
        ui.add_space(8.0);
        if small_button(app, ui, "shortcuts.saveAs", tl!("Save As…"), true) {
            app.shortcut_editor.save_name = Some(if builtin.contains(&active) { tl!("My Shortcuts").into() } else { active.clone() });
        }
        let can_delete = custom.contains(&active);
        if small_button(app, ui, "shortcuts.delete", tl!("Delete"), can_delete) {
            exec(app, "shortcuts.deletePreset", json!({"name": active}));
            app.shortcut_editor.message = tlf!("Deleted preset “{active}”", active);
        }
        if small_button(app, ui, "shortcuts.export", tl!("Export…"), true) {
            app.pick_ui(FileDialog::save_as(tl!("Keyboard Shortcuts"), &["json"], &format!("{active}.json")), |app, paths| match paths.into_iter().next() {
                Some(path) => {
                    if exec(app, "shortcuts.export", json!({"path": path})).is_some() {
                        app.shortcut_editor.message = tlf!("Exported to {path}", path);
                    }
                }
                None => app.shortcut_editor.message = tl!("Export: no file chosen (agents: shortcuts.export {path})").into(),
            });
        }
        if small_button(app, ui, "shortcuts.import", tl!("Import…"), true) {
            // .kys (Premiere's) too, so choosing one says why it can't be read (#2489)
            app.pick_ui(FileDialog::open_file(tl!("Keyboard Shortcuts"), &["json", "kys"]), |app, paths| match paths.into_iter().next() {
                Some(path) => {
                    if let Some(r) = exec(app, "shortcuts.import", json!({"path": path})) {
                        app.shortcut_editor.message = tlf!("Imported “{name}”", name = r["name"].as_str().unwrap_or(""));
                    }
                }
                None => app.shortcut_editor.message = tl!("Import: no file chosen (agents: shortcuts.import {path})").into(),
            });
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(if cfg!(target_os = "macos") { tl!("Keyboard: US (macOS)") } else { tl!("Keyboard: US") }).size(12.0).color(t.text_dim));
        });
    });
    if let Some(mut name) = app.shortcut_editor.save_name.clone() {
        ui.horizontal(|ui| {
            ui.label(RichText::new(tl!("Save preset as:")).size(13.0).color(t.text_dim));
            let r = crate::widgets::accessible_name(ui.add(egui::TextEdit::singleline(&mut name).desired_width(260.0)), "Preset name");
            app.auto.add("shortcuts.saveAs.name", r.rect, "Preset name");
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if small_button(app, ui, "shortcuts.saveAs.ok", tl!("Save"), !name.trim().is_empty()) || enter {
                if exec(app, "shortcuts.savePreset", json!({"name": name})).is_some() {
                    app.shortcut_editor.message = tlf!("Saved preset “{name}”", name = name.trim());
                    app.shortcut_editor.save_name = None;
                } else {
                    app.shortcut_editor.save_name = Some(name.clone());
                }
            } else if small_button(app, ui, "shortcuts.saveAs.cancel", tl!("Cancel"), true) {
                app.shortcut_editor.save_name = None;
            } else {
                app.shortcut_editor.save_name = Some(name.clone());
            }
        });
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(tl!("Commands:")).size(13.0).color(t.text_dim));
        let cur = app.shortcut_editor.context.clone();
        let r = craft_ui::ComboBox::from_id_salt("shortcuts-context")
            .width(280.0)
            .selected_text(crate::i18n::t(if cur.is_empty() { APPLICATION } else { &cur }))
            .show_ui(ui, |ui| {
                for c in std::iter::once(APPLICATION).chain(PANELS.iter().copied()) {
                    let label = if c == APPLICATION { crate::i18n::t(c).to_string() } else { tlf!("{panel} Panel", panel = crate::i18n::t(c)) };
                    let r = ui.selectable_label(cur == c, label);
                    app.auto.add(&format!("shortcuts.context.{c}"), r.rect, c);
                    if r.clicked() {
                        app.shortcut_editor.context = c.to_string();
                    }
                }
            });
        app.auto.add("shortcuts.context", r.response.rect, "Commands");
    });
}

/// (key name, width in key units); "" = gap.
type KeyRow = &'static [(&'static str, f32)];
const MAIN_ROWS: [KeyRow; 6] = [
    &[
        ("Escape", 1.0),
        ("", 0.5),
        ("F1", 1.0),
        ("F2", 1.0),
        ("F3", 1.0),
        ("F4", 1.0),
        ("", 0.25),
        ("F5", 1.0),
        ("F6", 1.0),
        ("F7", 1.0),
        ("F8", 1.0),
        ("", 0.25),
        ("F9", 1.0),
        ("F10", 1.0),
        ("F11", 1.0),
        ("F12", 1.0),
    ],
    &[
        ("`", 1.0),
        ("1", 1.0),
        ("2", 1.0),
        ("3", 1.0),
        ("4", 1.0),
        ("5", 1.0),
        ("6", 1.0),
        ("7", 1.0),
        ("8", 1.0),
        ("9", 1.0),
        ("0", 1.0),
        ("-", 1.0),
        ("=", 1.0),
        ("Backspace", 2.0),
    ],
    &[
        ("Tab", 1.5),
        ("Q", 1.0),
        ("W", 1.0),
        ("E", 1.0),
        ("R", 1.0),
        ("T", 1.0),
        ("Y", 1.0),
        ("U", 1.0),
        ("I", 1.0),
        ("O", 1.0),
        ("P", 1.0),
        ("[", 1.0),
        ("]", 1.0),
        ("\\", 1.5),
    ],
    &[
        ("#Caps", 1.75),
        ("A", 1.0),
        ("S", 1.0),
        ("D", 1.0),
        ("F", 1.0),
        ("G", 1.0),
        ("H", 1.0),
        ("J", 1.0),
        ("K", 1.0),
        ("L", 1.0),
        (";", 1.0),
        ("'", 1.0),
        ("Enter", 2.25),
    ],
    &[
        ("#Shift", 2.25),
        ("Z", 1.0),
        ("X", 1.0),
        ("C", 1.0),
        ("V", 1.0),
        ("B", 1.0),
        ("N", 1.0),
        ("M", 1.0),
        (",", 1.0),
        (".", 1.0),
        ("/", 1.0),
        ("#Shift", 2.75),
    ],
    &[("#Ctrl", 1.5), ("#Alt", 1.25), ("#Cmd", 1.5), ("Space", 6.5), ("#Cmd", 1.5), ("#Alt", 1.25), ("#Ctrl", 1.5)],
];
/// Navigation cluster rows (x offset in units within the cluster).
const NAV_ROWS: [&[(&str, f32)]; 6] = [
    &[],
    &[("Insert", 0.0), ("Home", 1.0), ("PageUp", 2.0)],
    &[("Delete", 0.0), ("End", 1.0), ("PageDown", 2.0)],
    &[],
    &[("Up", 1.0)],
    &[("Left", 0.0), ("Down", 1.0), ("Right", 2.0)],
];

fn key_legend(name: &str, mac: bool) -> String {
    match name {
        "#Caps" => "Caps Lock".into(),
        "#Shift" => "⇧ Shift".into(),
        "#Ctrl" => {
            if mac {
                "⌃ Control".into()
            } else {
                "Win".into()
            }
        }
        "#Alt" => {
            if mac {
                "⌥ Option".into()
            } else {
                "Alt".into()
            }
        }
        "#Cmd" => {
            if mac {
                "⌘ Command".into()
            } else {
                "Ctrl".into()
            }
        }
        "Backspace" => {
            if mac {
                "⌫ Delete".into()
            } else {
                "Backspace".into()
            }
        }
        "Delete" => {
            if mac {
                "⌦ Fwd Del".into()
            } else {
                "Delete".into()
            }
        }
        "Enter" => "↩ Return".into(),
        "Escape" => "Esc".into(),
        "PageUp" => "Page Up".into(),
        "PageDown" => "Page Down".into(),
        "Up" => "↑".into(),
        "Down" => "↓".into(),
        "Left" => "←".into(),
        "Right" => "→".into(),
        k => k.into(),
    }
}

fn keyboard(app: &mut FilmcraftApp, ui: &mut egui::Ui, area: Rect, t: &Tokens) {
    let mac = cfg!(target_os = "macos");
    // held modifiers count as the filter too (outside recording)
    let held = ui.input(|i| i.modifiers);
    let mut mods = app.shortcut_editor.mods;
    if app.shortcut_editor.recording.is_none() {
        mods.cmd |= held.command;
        mods.ctrl |= held.ctrl && mac;
        mods.alt |= held.alt;
        mods.shift |= held.shift;
    }
    let panel = app.shortcut_editor.panel().map(str::to_string);
    let map = app.session.shortcuts.keyboard(mods, panel.as_deref());
    let gap = 3.0;
    let (ux, uy) = key_units(area.size());
    let row_h = uy * 0.98;
    let origin = area.min;
    let mut clicked_key: Option<String> = None;
    let mut toggled: Option<&str> = None;
    let mut draw = |ui: &mut egui::Ui, r: Rect, name: &str| {
        let r = r.shrink(gap / 2.0);
        let is_mod = name.starts_with('#');
        let (app_cmd, panel_cmd) = map.get(name).cloned().unwrap_or_default();
        let mod_on = match name {
            "#Shift" => mods.shift,
            "#Alt" => mods.alt,
            "#Cmd" => mods.cmd,
            "#Ctrl" => mods.ctrl,
            _ => false,
        };
        // modifier keys appear twice: the right-hand ones get a ".right" suffix
        let right = is_mod && r.center().x > area.center().x;
        let id = match (is_mod, right) {
            (true, false) => format!("shortcuts.mod.{}", &name[1..]),
            (true, true) => format!("shortcuts.mod.{}.right", &name[1..]),
            _ => format!("shortcuts.key.{name}"),
        };
        let resp = ui.interact(r, egui::Id::unique(&id), Sense::click());
        let p = ui.painter();
        let base = if is_mod {
            if mod_on { MOD_KEY } else { FREE_KEY.gamma_multiply(0.8) }
        } else if app_cmd.is_some() {
            APP_KEY
        } else if panel_cmd.is_some() {
            PANEL_KEY
        } else {
            FREE_KEY
        };
        p.rect_filled(r, CornerRadius::same(3), base);
        if app_cmd.is_some() && panel_cmd.is_some() {
            // both: lower-right triangle in the panel colour
            p.add(egui::Shape::convex_polygon(vec![r.right_top(), r.right_bottom(), r.left_bottom()], PANEL_KEY, Stroke::NONE));
        }
        if resp.hovered() {
            p.rect_stroke(r, CornerRadius::same(3), Stroke::new(1.5, Color32::WHITE), StrokeKind::Inside);
        }
        if app.shortcut_editor.key.as_deref() == Some(name) {
            p.rect_stroke(r, CornerRadius::same(3), Stroke::new(2.0, t.accent), StrokeKind::Inside);
        }
        let fs = (uy * 0.2).clamp(8.5, 12.0);
        if let Some(cmd) = app_cmd.as_ref().or(panel_cmd.as_ref()) {
            // the name gets the key above its legend: 3 pt margins top and bottom, 1 pt between
            let legend_h = ui.fonts_mut(|f| f.row_height(&FontId::proportional(fs)));
            let space = vec2(r.width() - 8.0, r.height() - 7.0 - legend_h);
            let label = label_of(app, cmd);
            let fit = remembered_key_label(ui, &label, space, fs);
            let mut job = LayoutJob::single_section(fit.rows.join("\n"), TextFormat::simple(FontId::proportional(fit.size), Color32::WHITE));
            job.wrap.max_width = f32::INFINITY;
            let g = ui.fonts_mut(|f| f.layout_job(job));
            ui.painter().with_clip_rect(r).galley(r.min + vec2(4.0, 3.0), g, Color32::WHITE);
        }
        // the key's own legend stays inside the key too ("⌦ Fwd Del" ran into End)
        ui.painter().with_clip_rect(r).text(
            r.left_bottom() + vec2(4.0, -3.0),
            Align2::LEFT_BOTTOM,
            key_legend(name, mac),
            FontId::proportional(fs),
            Color32::from_white_alpha(220),
        );
        let tip = match (&app_cmd, &panel_cmd) {
            (Some(a), Some(b)) => format!("{} / {} ({})", label_of(app, a), label_of(app, b), panel.clone().unwrap_or_default()),
            (Some(a), None) => label_of(app, a),
            (None, Some(b)) => label_of(app, b),
            _ => key_legend(name, mac),
        };
        app.auto.add(&id, r, &tip);
        // a name cut short on the key reads in full on hover
        let resp = if app_cmd.is_some() || panel_cmd.is_some() { resp.on_hover_text(tip) } else { resp };
        if resp.clicked() {
            if is_mod {
                toggled = Some(match name {
                    "#Shift" => "Shift",
                    "#Alt" => "Alt",
                    "#Cmd" => "Cmd",
                    _ => "Ctrl",
                });
            } else if name != "#Caps" {
                clicked_key = Some(name.to_string());
            }
        }
    };
    for (ri, row) in MAIN_ROWS.iter().enumerate() {
        let mut x = origin.x;
        let y = origin.y + ri as f32 * row_h + if ri > 0 { uy * 0.15 } else { 0.0 };
        for (name, w) in row.iter() {
            let r = Rect::from_min_size(pos2(x, y), vec2(w * ux, row_h));
            if !name.is_empty() {
                draw(ui, r, name);
            }
            x += w * ux;
        }
    }
    let nav_x = origin.x + 15.5 * ux;
    for (ri, row) in NAV_ROWS.iter().enumerate() {
        let y = origin.y + ri as f32 * row_h + if ri > 0 { uy * 0.15 } else { 0.0 };
        for (name, off) in row.iter() {
            draw(ui, Rect::from_min_size(pos2(nav_x + off * ux, y), vec2(ux, row_h)), name);
        }
    }
    let ed = &mut app.shortcut_editor;
    if let Some(m) = toggled {
        match m {
            "Shift" => ed.mods.shift = !ed.mods.shift,
            "Alt" => ed.mods.alt = !ed.mods.alt,
            "Cmd" => ed.mods.cmd = !ed.mods.cmd,
            _ => ed.mods.ctrl = !ed.mods.ctrl,
        }
    }
    if let Some(k) = clicked_key {
        ed.key = Some(k);
    }
}

/// Key units across the drawn keyboard: 15 main columns, a half-unit gap and the navigation cluster.
const KEYBOARD_UNITS_W: f32 = 15.0 + 0.5 + 3.0;
/// Keys widen to fill the dialog up to this many times their height.
const MAX_KEY_ASPECT: f32 = 1.5;

/// Width and height of one key unit in a keyboard area of `size`. Six rows (plus the gap under the
/// function keys) fill the height; keys then widen to fill the width, up to [`MAX_KEY_ASPECT`] times
/// their height, so command names get the room the dialog has instead of square keys leaving a
/// quarter of it empty.
fn key_units(size: egui::Vec2) -> (f32, f32) {
    let uy = (size.y / 6.2).min(size.x / KEYBOARD_UNITS_W);
    let ux = (size.x / KEYBOARD_UNITS_W).min(uy * MAX_KEY_ASPECT);
    (ux, uy)
}

/// Smallest size a key's command name shrinks to before it is cut short.
const MIN_KEY_LABEL_FS: f32 = 7.5;

/// A key's command name as drawn: its rows, its font size and whether any of it was cut short.
#[derive(Clone, Debug, PartialEq)]
struct KeyLabel {
    rows: Vec<String>,
    size: f32,
    cut: bool,
}

/// [`fit_key_label`] with the app's fonts, remembered per name, room and size: a fit measures many
/// trial layouts (about 60 µs), and the keyboard draws some seventy names every frame.
fn remembered_key_label(ui: &egui::Ui, label: &str, space: egui::Vec2, max_fs: f32) -> KeyLabel {
    type Fits = std::collections::HashMap<(String, [u32; 4]), KeyLabel>;
    let id = egui::Id::unique("shortcuts.keyLabels");
    let key = (label.to_owned(), [space.x.to_bits(), space.y.to_bits(), max_fs.to_bits(), ui.ctx().pixels_per_point().to_bits()]);
    if let Some(fit) = ui.data_mut(|d| d.get_temp_mut_or_default::<Fits>(id).get(&key).cloned()) {
        return fit;
    }
    let fit =
        ui.fonts_mut(|f| fit_key_label(label, space, max_fs, |s, size| f.layout_no_wrap(s.to_owned(), FontId::proportional(size), Color32::WHITE).size()));
    ui.data_mut(|d| {
        let fits = d.get_temp_mut_or_default::<Fits>(id);
        // names, sizes and languages change rarely; a resize drag can add many sizes
        if fits.len() > 4096 {
            fits.clear();
        }
        fits.insert(key, fit.clone());
    });
    fit
}

/// Fit a key's command name in `space` (the key above its legend): the largest size from `max_fs`
/// down to [`MIN_KEY_LABEL_FS`] at which the whole name fits, in rows broken only where a line may
/// break (between words, and between the characters of Chinese and Japanese). A name that doesn't
/// fit even then is cut short with "…". A word is never split across two rows: longer translations
/// used to break mid-word ("Desmar / car", "Centrali / zar"). `measure` is the size of one line of
/// text at a font size.
fn fit_key_label(label: &str, space: egui::Vec2, max_fs: f32, mut measure: impl FnMut(&str, f32) -> egui::Vec2) -> KeyLabel {
    let mut size = max_fs.max(MIN_KEY_LABEL_FS);
    loop {
        let row_h = measure("M", size).y.max(1.0);
        let max_rows = ((space.y / row_h).floor() as usize).clamp(1, 3);
        let (rows, cut) = wrap_label(label, space.x, max_rows, |s| measure(s, size).x);
        if !cut || size <= MIN_KEY_LABEL_FS {
            return KeyLabel { rows, size, cut };
        }
        size = (size - 0.5).max(MIN_KEY_LABEL_FS);
    }
}

/// `text` in at most `max_rows` rows no wider than `max_w` (as `width` measures them), broken only
/// between [`break_units`]. A unit wider than a row is cut short with "…", and so is the last row
/// when text is left over; the flag says whether anything was cut.
fn wrap_label(text: &str, max_w: f32, max_rows: usize, mut width: impl FnMut(&str) -> f32) -> (Vec<String>, bool) {
    let mut rows = Vec::new();
    let mut row = String::new();
    let mut cut = false;
    for (spaced, unit) in break_units(text) {
        let joined = match (row.is_empty(), spaced) {
            (true, _) => unit.clone(),
            (false, true) => format!("{row} {unit}"),
            (false, false) => format!("{row}{unit}"),
        };
        if width(&joined) <= max_w {
            row = joined;
            continue;
        }
        if !row.is_empty() {
            rows.push(std::mem::take(&mut row));
        }
        if width(&unit) <= max_w {
            row = unit;
        } else {
            rows.push(ellipsize(&unit, max_w, &mut width));
            cut = true;
        }
    }
    if !row.is_empty() {
        rows.push(row);
    }
    let max_rows = max_rows.max(1);
    if rows.len() > max_rows {
        rows.truncate(max_rows);
        if let Some(last) = rows.last_mut() {
            *last = ellipsize(last, max_w, &mut width);
        }
        cut = true;
    }
    (rows, cut)
}

/// `text` split where a line may break: at spaces, after a slash or hyphen ("Speed/ Duration"), and
/// between the characters of Chinese and Japanese, which don't put spaces between words (an opening
/// bracket stays with what follows it; closing punctuation, the long-vowel mark and small kana stay
/// with what precedes them). Each unit says whether a space preceded it.
fn break_units(text: &str) -> Vec<(bool, String)> {
    let mut units = Vec::new();
    let mut unit = String::new();
    let mut spaced = false;
    for c in text.chars() {
        if c.is_whitespace() {
            if !unit.is_empty() {
                units.push((spaced, std::mem::take(&mut unit)));
            }
            spaced = true;
            continue;
        }
        if let Some(prev) = unit.chars().last()
            && (breaks_between(prev) || breaks_between(c) || matches!(prev, '/' | '-'))
            && !opens(prev)
            && !closes(c)
        {
            units.push((spaced, std::mem::take(&mut unit)));
            spaced = false;
        }
        unit.push(c);
    }
    if !unit.is_empty() {
        units.push((spaced, unit));
    }
    units
}

/// Chinese characters, kana and full-width forms: a line may break on either side of them.
fn breaks_between(c: char) -> bool {
    matches!(c as u32,
        0x3000..=0x303F // CJK symbols and punctuation
        | 0x3040..=0x30FF // hiragana, katakana
        | 0x31F0..=0x31FF // katakana phonetic extensions
        | 0x3400..=0x4DBF // CJK extension A
        | 0x4E00..=0x9FFF // CJK unified ideographs
        | 0xF900..=0xFAFF // CJK compatibility ideographs
        | 0xFF00..=0xFFEF // half-width and full-width forms
        | 0x20000..=0x2FFFF) // CJK extensions B–F
}

/// Opening brackets: a line doesn't end with one.
fn opens(c: char) -> bool {
    matches!(c, '(' | '[' | '（' | '［' | '「' | '『' | '【' | '〔' | '〈' | '《' | '“' | '‘')
}

/// Closing punctuation, the long-vowel mark and small kana: a line doesn't start with one.
fn closes(c: char) -> bool {
    ")],.:;!?…、。，．：；！？）］」』】〕〉》”’・ーぁぃぅぇぉっゃゅょゎァィゥェォッャュョヮ".contains(c)
}

/// `text` cut short with "…" so that it fits `max_w` (just "…" when nothing more fits).
fn ellipsize(text: &str, max_w: f32, width: &mut impl FnMut(&str) -> f32) -> String {
    let mut chars: Vec<char> = text.trim_end_matches(['…', ' ']).chars().collect();
    loop {
        let s = format!("{}…", chars.iter().collect::<String>().trim_end());
        if chars.is_empty() || width(&s) <= max_w {
            return s;
        }
        chars.pop();
    }
}

fn legend(ui: &mut egui::Ui, t: &Tokens) {
    ui.horizontal(|ui| {
        for (c, text) in [
            (APP_KEY, tl!("Application shortcuts work whatever panel has focus")),
            (PANEL_KEY, tl!("Panel shortcuts override application shortcuts while that panel has focus")),
            (MOD_KEY, tl!("Modifier held / selected")),
        ] {
            let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
            ui.painter().rect_filled(r, 2.0, c);
            ui.label(RichText::new(text).size(12.0).color(t.text_dim));
            ui.add_space(14.0);
        }
    });
}

fn command_list(app: &mut FilmcraftApp, ui: &mut egui::Ui, height: f32, t: &Tokens) {
    let plat = Platform::current();
    ui.horizontal(|ui| {
        let mut q = app.shortcut_editor.search.clone();
        let r = crate::widgets::accessible_name(
            ui.add(egui::TextEdit::singleline(&mut q).hint_text(tl!("Search commands or keys")).desired_width(320.0)),
            "Search commands or keys",
        );
        app.auto.add("shortcuts.search", r.rect, "Search");
        app.shortcut_editor.search = q;
        if let Some(sel) = app.shortcut_editor.selected.clone() {
            ui.label(RichText::new(tlf!("Selected: {command}", command = label_of(app, &sel))).size(12.0).color(t.text_dim));
        }
    });
    ui.add_space(4.0);
    let rows = app.session.execute("shortcuts.list", json!({})).unwrap_or_default();
    let rows = rows.as_array().cloned().unwrap_or_default();
    let query = app.shortcut_editor.search.trim();
    let rows: Vec<_> = rows.into_iter().filter(|row| matches_search(row, query)).collect();
    let w = ui.available_width();
    // column header
    let (hr, _) = ui.allocate_exact_size(vec2(w, 22.0), Sense::hover());
    ui.painter().text(hr.left_center() + vec2(8.0, 0.0), Align2::LEFT_CENTER, tl!("Command"), Tokens::semibold(12.5), t.text_dim);
    ui.painter().text(pos2(hr.min.x + w * 0.58, hr.center().y), Align2::LEFT_CENTER, tl!("Shortcut"), Tokens::semibold(12.5), t.text_dim);
    egui::ScrollArea::vertical().id_salt("shortcuts-list").max_height(height - 50.0).auto_shrink([false, false]).show(ui, |ui| {
        let mut last_cat = String::new();
        for (i, row) in rows.iter().enumerate() {
            let id = row["id"].as_str().unwrap_or("").to_string();
            let cat = row["category"].as_str().unwrap_or("").to_string();
            if cat != last_cat {
                let (cr, _) = ui.allocate_exact_size(vec2(w, 22.0), Sense::hover());
                let c = cr.left_center() + vec2(9.0, 0.0);
                ui.painter().add(egui::Shape::convex_polygon(vec![c + vec2(-4.0, -2.5), c + vec2(4.0, -2.5), c + vec2(0.0, 3.0)], t.text_dim, Stroke::NONE));
                ui.painter().text(cr.left_center() + vec2(20.0, 0.0), Align2::LEFT_CENTER, crate::i18n::t(&cat), Tokens::semibold(12.0), t.text);
                last_cat = cat;
            }
            let (r, resp) = ui.allocate_exact_size(vec2(w, 26.0), Sense::click());
            let selected = app.shortcut_editor.selected.as_deref() == Some(id.as_str());
            let bg = if selected {
                t.row_selected
            } else if i % 2 == 0 {
                t.app_bg
            } else {
                t.row_alt
            };
            ui.painter().rect_filled(r, 0.0, bg);
            ui.painter().text(
                r.left_center() + vec2(22.0, 0.0),
                Align2::LEFT_CENTER,
                crate::i18n::t(row["label"].as_str().unwrap_or("")),
                Tokens::ui(12.5),
                t.text,
            );
            app.auto.add(&format!("shortcuts.row.{id}"), r, row["label"].as_str().unwrap_or(""));
            if resp.clicked() {
                app.shortcut_editor.selected = Some(id.clone());
            }
            // shortcut cell: click to record (replace); "+" adds another
            let cell = Rect::from_min_max(pos2(r.min.x + w * 0.58, r.min.y + 2.0), pos2(r.max.x - 34.0, r.max.y - 2.0));
            let recording = app.shortcut_editor.recording.as_ref().is_some_and(|(c, _)| *c == id);
            let text = if recording {
                tl!("Type a shortcut… (Esc cancels)").to_string()
            } else {
                row["shortcuts"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|b| {
                        let d = b["display"].as_str().unwrap_or("").to_string();
                        match b["panel"].as_str() {
                            Some(pn) => format!("{d} ({pn})"),
                            None => d,
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(",  ")
            };
            let cresp = ui.interact(cell, egui::Id::unique(("shortcut-cell", &id)), Sense::click());
            if recording {
                ui.painter().rect(cell, 3.0, t.field_bg, Stroke::new(1.5, t.accent), StrokeKind::Inside);
            } else if cresp.hovered() {
                ui.painter().rect_stroke(cell, 3.0, Stroke::new(1.0, t.field_border), StrokeKind::Inside);
            }
            ui.painter().text(cell.left_center() + vec2(6.0, 0.0), Align2::LEFT_CENTER, &text, Tokens::ui(12.5), if recording { t.accent } else { t.hot_text });
            app.auto.add(&format!("shortcuts.cell.{id}"), cell, &text);
            if cresp.clicked() {
                app.shortcut_editor.selected = Some(id.clone());
                app.shortcut_editor.recording = Some((id.clone(), false));
            }
            let plus = Rect::from_min_size(pos2(r.max.x - 30.0, r.min.y + 3.0), vec2(20.0, 20.0));
            let presp = ui.interact(plus, egui::Id::unique(("shortcut-add", &id)), Sense::click()).on_hover_text(tl!("Add another shortcut"));
            ui.painter().text(
                plus.center(),
                Align2::CENTER_CENTER,
                "+",
                Tokens::semibold(14.0),
                if presp.hovered() { t.tab_text_active } else { t.text_faint },
            );
            app.auto.add(&format!("shortcuts.add.{id}"), plus, "Add shortcut");
            if presp.clicked() {
                app.shortcut_editor.selected = Some(id.clone());
                app.shortcut_editor.recording = Some((id.clone(), true));
            }
        }
        if rows.is_empty() {
            ui.label(RichText::new(tl!("No matching commands")).color(t.text_dim));
        }
    });
    let _ = plat;
}

fn matches_search(row: &serde_json::Value, query: &str) -> bool {
    ["id", "label", "category"].iter().any(|key| crate::i18n::matches_query(row.get(key).and_then(serde_json::Value::as_str).unwrap_or(""), query))
        || row.get("shortcuts").and_then(serde_json::Value::as_array).into_iter().flatten().any(|binding| {
            ["keys", "display", "panel"].iter().any(|key| crate::i18n::matches_query(binding.get(key).and_then(serde_json::Value::as_str).unwrap_or(""), query))
        })
}

fn key_detail(app: &mut FilmcraftApp, ui: &mut egui::Ui, height: f32, t: &Tokens) {
    let plat = Platform::current();
    let key = app.shortcut_editor.key.clone();
    ui.horizontal(|ui| {
        ui.label(RichText::new(tl!("Key:")).size(13.0).strong().color(t.text));
        if let Some(k) = &key {
            ui.label(RichText::new(key_legend(k, plat.is_mac())).size(13.0).color(t.hot_text));
        }
    });
    let (hr, _) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
    ui.painter().text(hr.left_center() + vec2(8.0, 0.0), Align2::LEFT_CENTER, tl!("Modifiers"), Tokens::semibold(12.5), t.text_dim);
    ui.painter().text(hr.left_center() + vec2(110.0, 0.0), Align2::LEFT_CENTER, tl!("Command"), Tokens::semibold(12.5), t.text_dim);
    let list = key.as_ref().and_then(|k| app.session.execute("shortcuts.forKey", json!({"key": k})).ok()).unwrap_or_default();
    egui::ScrollArea::vertical().id_salt("shortcuts-key").max_height(height - 90.0).auto_shrink([false, false]).show(ui, |ui| {
        let w = ui.available_width();
        for (i, b) in list.as_array().into_iter().flatten().enumerate() {
            let (r, _) = ui.allocate_exact_size(vec2(w, 24.0), Sense::hover());
            ui.painter().rect_filled(r, 0.0, if i % 2 == 0 { t.app_bg } else { t.row_alt });
            let keys = b["keys"].as_str().unwrap_or("");
            let mods = Chord::parse(keys).map(|c| {
                let k = Chord { mods: c.mods, key: c.key };
                let d = k.display(plat);
                let legend = key_legend(c.key, plat.is_mac());
                let m = d.trim_end_matches(&legend).trim_end_matches(c.key).trim_end_matches('+').to_string();
                if m.is_empty() { tl!("None").to_string() } else { m }
            });
            ui.painter().text(r.left_center() + vec2(8.0, 0.0), Align2::LEFT_CENTER, mods.unwrap_or_default(), Tokens::ui(12.5), t.text);
            let cmd = b["command"].as_str().unwrap_or("");
            let label = match b["panel"].as_str() {
                Some(p) => format!("{} ({p})", label_of(app, cmd)),
                None => label_of(app, cmd),
            };
            ui.painter().text(r.left_center() + vec2(110.0, 0.0), Align2::LEFT_CENTER, label, Tokens::ui(12.5), t.text);
        }
        if key.is_some() && list.as_array().is_none_or(|a| a.is_empty()) {
            ui.label(RichText::new(tl!("Nothing is assigned to this key")).color(t.text_dim));
        }
        if key.is_none() {
            ui.label(RichText::new(tl!("Click a key on the keyboard to see its shortcuts")).color(t.text_dim));
        }
    });
    // assign the selected command to the clicked key with the selected modifiers
    if let (Some(k), Some(cmd)) = (key, app.shortcut_editor.selected.clone()) {
        let keys = format!("{}{}", app.shortcut_editor.mods.prefix(), k);
        let disp = Chord::parse(&keys).map(|c| c.display(plat)).unwrap_or(keys.clone());
        let label = tlf!("Assign {key} to {command}", key = disp, command = label_of(app, &cmd));
        if small_button(app, ui, "shortcuts.assignKey", &label, true) {
            assign(app, &cmd, &keys, false);
        }
    }
}

/// Bottom row: messages / conflicts, Undo / Redo / Clear, Cancel / OK. Returns Some(ok) to close.
fn footer(app: &mut FilmcraftApp, ui: &mut egui::Ui, t: &Tokens) -> Option<bool> {
    let plat = Platform::current();
    let conflicts = app.session.shortcuts.conflicts(plat);
    let mut close = None;
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.set_width(ui.available_width() - 420.0);
            for (ctx, keys, cmds) in conflicts.iter().take(2) {
                let disp = Chord::parse(keys).map(|c| c.display(plat)).unwrap_or(keys.clone());
                let names: Vec<String> = cmds.iter().map(|c| label_of(app, c)).collect();
                let r = ui.label(
                    RichText::new(tlf!("Conflict: {key} is assigned to {commands} ({ctx})", key = disp, commands = names.join(tl!(" and ")), ctx))
                        .size(12.0)
                        .color(WARN),
                );
                app.auto.add("shortcuts.conflict", r.rect, "conflict");
            }
            let msg = if app.shortcut_editor.message.is_empty() {
                tl!("Click a command's Shortcut cell and press keys to assign it; + adds another shortcut.").to_string()
            } else {
                app.shortcut_editor.message.clone()
            };
            let r = ui.label(RichText::new(msg.clone()).size(12.0).color(t.text_dim));
            app.auto.add("shortcuts.message", r.rect, &msg);
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let ok = ui.add(
                egui::Button::new(RichText::new(tl!("OK")).size(13.0).color(Color32::WHITE)).fill(t.accent).min_size(vec2(72.0, 28.0)).corner_radius(14.0),
            );
            app.auto.add("shortcuts.ok", ok.rect, "OK");
            if ok.clicked() {
                close = Some(true);
            }
            let cancel = ui.add(egui::Button::new(RichText::new(tl!("Cancel")).size(13.0)).min_size(vec2(72.0, 28.0)).corner_radius(14.0));
            app.auto.add("shortcuts.cancel", cancel.rect, "Cancel");
            if cancel.clicked() {
                close = Some(false);
            }
            ui.add_space(16.0);
            let sel = app.shortcut_editor.selected.clone();
            let panel = app.shortcut_editor.panel().map(str::to_string);
            if small_button(app, ui, "shortcuts.clear", tl!("Clear"), sel.is_some())
                && let Some(c) = sel
                && let Some(r) = exec(app, "shortcuts.clear", json!({"command": c, "panel": panel.clone().unwrap_or_else(|| APPLICATION.into())}))
            {
                app.shortcut_editor.message = tlf!("Cleared {n} shortcut(s) of {command}", n = r["removed"], command = label_of(app, &c));
            }
            let (cu, cr) = (app.session.shortcuts.can_undo(), app.session.shortcuts.can_redo());
            if small_button(app, ui, "shortcuts.redo", tl!("Redo"), cr) {
                exec(app, "shortcuts.redo", json!({}));
            }
            if small_button(app, ui, "shortcuts.undo", tl!("Undo"), cu) {
                exec(app, "shortcuts.undo", json!({}));
            }
        });
    });
    close
}

#[cfg(test)]
mod localization_tests {
    use super::*;

    #[test]
    fn command_search_accepts_translated_labels_ids_and_keys() {
        let row = json!({"id": "edit.undo", "label": "Undo", "category": "Edit", "shortcuts": [{"keys": "Cmd+Z", "display": "Ctrl+Z"}]});
        for l in crate::i18n::Language::ALL {
            crate::i18n::set_current(l);
            let undo = l.tr("Undo").to_lowercase();
            for query in [undo.as_str(), "undo", "edit.undo", "ctrl+z", "cmd+z"] {
                assert!(matches_search(&row, query), "{}: {query}", l.code());
            }
            let redo = l.tr("Redo").to_lowercase();
            assert!(!matches_search(&row, &redo), "{}: {redo}", l.code());
        }
        crate::i18n::set_current(crate::i18n::Language::En);
    }

    #[test]
    fn command_search_accepts_turkish_casing() {
        crate::i18n::set_current(crate::i18n::Language::Tr);
        let row = json!({"id": "marker.in_out", "label": "Add Range Marker to In and Out", "category": "Marker", "shortcuts": []});
        for query in ["ÇIKIŞ", "çıkış", "Çıkış"] {
            assert!(matches_search(&row, query), "{query}");
        }
        crate::i18n::set_current(crate::i18n::Language::En);
    }
}

#[cfg(test)]
mod key_label_tests {
    use super::*;

    /// One unit per character: easy to reason about in the expected rows.
    fn chars(s: &str) -> f32 {
        s.chars().count() as f32
    }

    #[test]
    fn key_names_break_between_words_not_inside_them() {
        assert_eq!(wrap_label("Desmarcar tudo", 10.0, 2, chars), (vec!["Desmarcar".to_string(), "tudo".to_string()], false));
        assert_eq!(wrap_label("Undo", 10.0, 2, chars), (vec!["Undo".to_string()], false));
    }

    #[test]
    fn names_also_break_after_a_slash_or_hyphen() {
        assert_eq!(wrap_label("Speed/Duration…", 10.0, 2, chars), (vec!["Speed/".to_string(), "Duration…".to_string()], false));
        assert_eq!(wrap_label("Ripple-Delete", 8.0, 2, chars), (vec!["Ripple-".to_string(), "Delete".to_string()], false));
    }

    #[test]
    fn a_word_wider_than_the_key_is_cut_short_not_split() {
        assert_eq!(wrap_label("Centralizar", 8.0, 2, chars), (vec!["Central…".to_string()], true));
    }

    #[test]
    fn names_left_over_end_the_last_row_with_an_ellipsis() {
        let (rows, cut) = wrap_label("Ferramenta Seleção de Faixa", 10.0, 2, chars);
        assert_eq!(rows, ["Ferramenta", "Seleção d…"]);
        assert!(cut);
    }

    #[test]
    fn chinese_and_japanese_break_between_characters() {
        let units = |s: &str| break_units(s).into_iter().map(|(_, u)| u).collect::<Vec<_>>();
        assert_eq!(units("選択ツール"), ["選", "択", "ツー", "ル"]);
        assert_eq!(units("（テスト）"), ["（テ", "ス", "ト）"]);
        assert_eq!(units("H.264 書き出し"), ["H.264", "書", "き", "出", "し"]);
        assert_eq!(wrap_label("選択ツール", 3.0, 2, chars), (vec!["選択".to_string(), "ツール".to_string()], false));
    }

    #[test]
    fn a_name_shrinks_before_it_is_cut() {
        let measure = |s: &str, size: f32| vec2(chars(s) * size * 0.5, size * 1.2);
        let fit = fit_key_label("Desmarcar tudo", vec2(40.0, 30.0), 12.0, measure);
        assert_eq!(fit, KeyLabel { rows: vec!["Desmarcar".into(), "tudo".into()], size: 8.5, cut: false });
    }

    #[test]
    fn empty_and_degenerate_keys_do_not_panic() {
        let measure = |s: &str, size: f32| vec2(chars(s) * size * 0.5, size * 1.2);
        assert_eq!(fit_key_label("", vec2(0.0, 0.0), 12.0, measure), KeyLabel { rows: vec![], size: 12.0, cut: false });
        let fit = fit_key_label("Desmarcar", vec2(-5.0, f32::NAN), f32::NAN, measure);
        assert_eq!(fit.rows, ["…"]);
        assert!(fit.cut);
    }

    #[test]
    fn keys_widen_to_fill_the_dialog_up_to_one_and_a_half_times_their_height() {
        let (ux, uy) = key_units(vec2(1320.0, 360.0));
        assert!((uy - 360.0 / 6.2).abs() < 0.01);
        assert!((ux - 1320.0 / KEYBOARD_UNITS_W).abs() < 0.01, "{ux}");
        let (ux, uy) = key_units(vec2(3000.0, 360.0));
        assert!((ux - uy * MAX_KEY_ASPECT).abs() < 0.01);
    }

    /// Report, not a check: how many command names on the drawn keyboard are cut short, per
    /// interface language and screen size, with square keys (before) and widened ones.
    /// `cargo test --release -p filmcraft-ui-egui key_label_report -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn key_label_report() {
        use filmcraft_engine::shortcuts::BUILTIN_PRESETS;
        filmcraft_text::fonts::scan_system();
        let ctx = egui::Context::default();
        crate::theme::install(&ctx, &Tokens::for_kind(crate::theme::ThemeKind::Dark));
        ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear();
        let mut session = filmcraft_engine::Session::default();
        let mut keyed: std::collections::BTreeSet<(&'static str, String)> = std::collections::BTreeSet::new();
        for preset in BUILTIN_PRESETS {
            session.shortcuts.load_preset(preset).unwrap();
            for b in &session.shortcuts.bindings {
                let Ok(c) = Chord::parse(&b.keys) else { continue };
                let label = session
                    .shortcuts
                    .command(&b.command)
                    .map(|c| c.label)
                    .or_else(|| filmcraft_engine::find_command(&b.command).map(|c| c.label.to_string()))
                    .unwrap_or_else(|| b.command.clone());
                keyed.insert((c.key, label));
            }
        }
        let width_units = |key: &str| -> Option<f32> {
            MAIN_ROWS
                .iter()
                .flat_map(|r| r.iter())
                .find(|(n, _)| *n == key)
                .map(|(_, w)| *w)
                .or_else(|| NAV_ROWS.iter().flat_map(|r| r.iter()).find(|(n, _)| *n == key).map(|_| 1.0))
        };
        let screens = [(800.0, 600.0), (1280.0, 800.0), (1440.0, 900.0), (1512.0, 982.0), (1728.0, 1117.0)];
        eprintln!("{} (key, name) pairs across {} presets", keyed.len(), BUILTIN_PRESETS.len());
        let started = std::time::Instant::now();
        let mut fits = 0;
        for lang in crate::i18n::Language::ALL {
            crate::i18n::set_current(lang);
            for (sw, sh) in screens {
                let size = dialog_size(vec2(sw, sh));
                let area = vec2(size.x, keyboard_height(size));
                let square = (area.x / KEYBOARD_UNITS_W).min(area.y / 6.2);
                let mut line = format!("{:6} {sw}x{sh}:", lang.code());
                for (geometry, (ux, uy)) in [("square", (square, square)), ("wide", key_units(area))] {
                    let fs = (uy * 0.2).clamp(8.5, 12.0);
                    let (mut cut, mut small, mut examples) = (0, 0, Vec::new());
                    let mut n = 0;
                    for (key, label) in &keyed {
                        let Some(w) = width_units(key) else { continue };
                        let label = crate::i18n::t(label).to_string();
                        let r = vec2(w * ux - 3.0, uy * 0.98 - 3.0);
                        let fit = ctx.fonts_mut(|f| {
                            let legend_h = f.row_height(&FontId::proportional(fs));
                            fit_key_label(&label, vec2(r.x - 8.0, r.y - 7.0 - legend_h), fs, |s, size| {
                                f.layout_no_wrap(s.to_owned(), FontId::proportional(size), Color32::WHITE).size()
                            })
                        });
                        fits += 1;
                        n += 1;
                        if fit.cut {
                            cut += 1;
                            examples.push((label.chars().count(), format!("{label} → {}", fit.rows.join(" / "))));
                        } else if fit.size < 9.0 {
                            small += 1;
                        }
                    }
                    examples.sort();
                    let shortest = examples.first().map_or(0, |e| e.0);
                    line += &format!("  {geometry} u={ux:.0}x{uy:.0}: {cut}/{n} cut (shortest {shortest} chars), {small} under 9 pt");
                    if geometry == "wide" && sw == 1512.0 {
                        for (len, e) in &examples {
                            line += &format!("\n        [{len}] {e}");
                        }
                    }
                }
                eprintln!("{line}");
            }
        }
        crate::i18n::set_current(crate::i18n::Language::En);
        eprintln!("{fits} fits in {:?} ({:.1} µs each)", started.elapsed(), started.elapsed().as_secs_f64() * 1e6 / fits as f64);
    }
}
