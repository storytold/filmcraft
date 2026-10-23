//! Effect presets in the UI: the Presets bin of the Effects panel (built-in and user presets; drag
//! onto a clip or double-click to apply to the selection; right-click a user preset to delete or
//! export it) and the Save Preset dialog (Effect Controls ▸ right-click an effect ▸ Save Preset…).
//!
//! Automation ids: `effects.preset.<name>`, `savePreset.name`, `savePreset.description`,
//! `savePreset.keyframes.<scale|anchorIn|anchorOut|none>`, `savePreset.ok`, `savePreset.cancel`.

use std::collections::BTreeMap;

use egui::{Align2, Rect, Sense, pos2, vec2};
use serde_json::json;

use crate::FilmcraftApp;
use crate::icons::{self, Icon};
use crate::native_dialogs::FileDialog;
use crate::state::SavePresetDraft;
use crate::theme::Tokens;
use filmcraft_engine::presets::EffectPreset;

/// One node of the Presets bin: sub-folders and preset indices, built from the `" ▸ "` separator
/// in preset names (Premiere bin paths arrive that way from `.prfpset` files).
#[derive(Default)]
struct PresetNode {
    folders: BTreeMap<String, PresetNode>,
    presets: Vec<usize>,
}

impl PresetNode {
    fn insert(&mut self, path: &[&str], index: usize) {
        match path.split_first() {
            Some((folder, rest)) if !rest.is_empty() => self.folders.entry((*folder).to_string()).or_default().insert(rest, index),
            Some((leaf, _)) if !leaf.is_empty() => self.presets.push(index),
            _ => self.presets.push(index),
        }
    }
}

fn preset_tree(presets: &[EffectPreset], filter: &str) -> PresetNode {
    let mut root = PresetNode::default();
    for (i, p) in presets.iter().enumerate() {
        if !crate::i18n::matches_query(&p.name, filter) {
            continue;
        }
        let parts: Vec<&str> = p.name.split(" ▸ ").map(str::trim).filter(|s| !s.is_empty()).collect();
        root.insert(&parts, i);
    }
    root
}

/// A small action row at the top of the Presets folder (Import, Scan).
fn action_row(app: &mut FilmcraftApp, ui: &mut egui::Ui, id: &str, icon: Icon, label: &str) -> bool {
    let t = app.tokens;
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(r, 0.0, t.hover);
    }
    let x = r.min.x + 30.0;
    icons::paint(ui.painter(), Rect::from_center_size(pos2(x, r.center().y), vec2(13.0, 13.0)), icon, t.text_dim);
    ui.painter().text(pos2(x + 12.0, r.center().y), Align2::LEFT_CENTER, label, Tokens::ui(12.0), t.text_dim);
    app.auto.add(id, r, label);
    resp.clicked()
}

fn preset_folder_row(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    key: &str,
    label: &str,
    depth: usize,
    filter: &str,
    action: &mut Option<(String, String)>,
) -> bool {
    let t = app.tokens;
    let open = !filter.is_empty() || app.ui.expanded_fx.iter().any(|k| k == key);
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(r, 0.0, t.hover);
    }
    let x = r.min.x + 30.0 + depth as f32 * 16.0;
    icons::paint(
        ui.painter(),
        Rect::from_center_size(pos2(x - 10.0, r.center().y), vec2(10.0, 10.0)),
        if open { Icon::ChevronDown } else { Icon::ChevronRight },
        t.text_dim,
    );
    icons::paint(ui.painter(), Rect::from_center_size(pos2(x + 4.0, r.center().y), vec2(13.0, 13.0)), Icon::Folder, t.text_dim);
    ui.painter().text(pos2(x + 16.0, r.center().y), Align2::LEFT_CENTER, label, Tokens::ui(12.0), t.text);
    app.auto.add(&format!("effects.presetFolder.{key}"), r, label);

    resp.context_menu(|ui| {
        if ui.button(tl!("Import Presets…")).clicked() {
            *action = Some(("presets.import".to_string(), String::new()));
            ui.close();
        }
        if ui.button(tl!("Open Presets Folder")).clicked() {
            *action = Some(("presets.revealFolder".to_string(), String::new()));
            ui.close();
        }
        if ui.button(tl!("Scan Presets Folder")).clicked() {
            *action = Some(("presets.scan".to_string(), String::new()));
            ui.close();
        }
    });

    if resp.clicked() {
        if open {
            app.ui.expanded_fx.retain(|k| k != key);
        } else {
            app.ui.expanded_fx.push(key.to_string());
        }
    }
    open
}

fn preset_row(app: &mut FilmcraftApp, ui: &mut egui::Ui, p: &EffectPreset, depth: usize, action: &mut Option<(String, String)>) {
    let t = app.tokens;
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::click_and_drag());
    if resp.hovered() {
        ui.painter().rect_filled(r, 0.0, t.hover);
    }
    let x = r.min.x + 30.0 + depth as f32 * 16.0;
    icons::paint(
        ui.painter(),
        Rect::from_center_size(pos2(x, r.center().y), vec2(13.0, 13.0)),
        Icon::Sparkle,
        if p.builtin { t.text_dim } else { t.accent },
    );
    ui.painter().text(pos2(x + 12.0, r.center().y), Align2::LEFT_CENTER, crate::i18n::t(&p.name), Tokens::ui(12.0), t.text);
    app.auto.add(&format!("effects.preset.{}", p.name), r, &p.name);
    let tip = crate::i18n::t(if p.description.is_empty() { &p.name } else { &p.description }).to_string();
    let resp = resp.on_hover_text(tip);
    if resp.drag_started() {
        crate::panels::start_drag_effect(ui, &format!("preset:{}", p.name));
    }
    if resp.double_clicked() {
        *action = Some(("presets.apply".to_string(), p.name.clone()));
    }
    let source = app.session.presets.source_of(&p.name).map(str::to_string);
    resp.context_menu(|ui| {
        if ui.button(tl!("Apply to Selected Clips")).clicked() {
            *action = Some(("presets.apply".to_string(), p.name.clone()));
            ui.close();
        }
        if let Some(path) = &source {
            ui.separator();
            ui.label(format!("{} {path}", tl!("From preset file:")));
        }
        if !p.builtin && source.is_none() && ui.button(tl!("Delete Preset")).clicked() {
            *action = Some(("presets.delete".to_string(), p.name.clone()));
            ui.close();
        }
        if ui.button(tl!("Export Preset…")).clicked() {
            *action = Some(("presets.export".to_string(), p.name.clone()));
            ui.close();
        }
        if ui.button(tl!("Open Presets Folder")).clicked() {
            *action = Some(("presets.revealFolder".to_string(), String::new()));
            ui.close();
        }
        if ui.button(tl!("Import Presets…")).clicked() {
            *action = Some(("presets.import".to_string(), String::new()));
            ui.close();
        }
        if ui.button(tl!("Scan Presets Folder")).clicked() {
            *action = Some(("presets.scan".to_string(), String::new()));
            ui.close();
        }
    });
}

fn render_presets(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    node: &PresetNode,
    presets: &[EffectPreset],
    prefix: &str,
    depth: usize,
    filter: &str,
    action: &mut Option<(String, String)>,
) {
    for (name, child) in &node.folders {
        let path = if prefix.is_empty() { name.clone() } else { format!("{prefix} ▸ {name}") };
        let key = format!("Presets/{path}");
        if preset_folder_row(app, ui, &key, name, depth, filter, action) {
            render_presets(app, ui, child, presets, &path, depth + 1, filter, action);
        }
    }
    for &i in &node.presets {
        if let Some(p) = presets.get(i) {
            preset_row(app, ui, p, depth, action);
        }
    }
}

/// The rows of the Presets folder (inside the Effects panel's scroll area). Presets whose names
/// carry a `" ▸ "` bin path are shown as a nested folder tree, as in Premiere's Effects panel.
pub fn folder_rows(app: &mut FilmcraftApp, ui: &mut egui::Ui, filter: &str) -> Option<(String, String)> {
    let mut action = None;
    if filter.is_empty() {
        let import = action_row(app, ui, "effects.presets.import", Icon::Import, tl!("Import Presets…"));
        if import {
            action = Some(("presets.import".to_string(), String::new()));
        }
        let scan = action_row(app, ui, "effects.presets.scan", Icon::Reset, tl!("Scan Presets Folder"));
        if scan {
            action = Some(("presets.scan".to_string(), String::new()));
        }
    }
    let presets = app.session.presets.all();
    let tree = preset_tree(&presets, filter);
    render_presets(app, ui, &tree, &presets, "", 0, filter, &mut action);
    action
}

/// Run a Presets-bin action.
pub fn run(app: &mut FilmcraftApp, ctx: &egui::Context, cmd: &str, name: &str) {
    match cmd {
        "presets.apply" => {
            if let Err(e) = app.session.execute(cmd, json!({"preset": name})) {
                app.ui.status = e.to_string();
            }
        }
        "presets.delete" => {
            if let Err(e) = app.session.execute(cmd, json!({"name": name})) {
                app.ui.status = e.to_string();
            }
        }
        "presets.revealFolder" => match app.session.presets.library_dir() {
            Some(dir) => {
                if let Err(e) = crate::panels::menu_dialogs::open_path(app, ctx, &dir.to_string_lossy(), false) {
                    app.ui.status = e;
                }
            }
            None => app.ui.status = tl!("No presets folder: the app has no data directory").to_string(),
        },
        "presets.export" => {
            let name = name.to_string();
            app.pick_ui(FileDialog::save_as(tl!("Effect presets"), &["json"], &format!("{name}.json")), move |app, paths| {
                let Some(path) = paths.into_iter().next() else { return };
                if let Err(e) = app.session.execute("presets.export", json!({"path": path, "names": [name]})) {
                    app.ui.status = e.to_string();
                }
            });
        }
        "presets.import" => {
            app.pick_ui(FileDialog::open_files(&["prfpset", "json"]), |app, paths| {
                let mut imported = 0usize;
                let mut skipped = 0usize;
                let mut warnings = 0usize;
                let mut errors = Vec::new();
                for path in paths {
                    match app.session.execute("presets.import", json!({"path": path})) {
                        Ok(v) => {
                            imported += v["imported"].as_array().map_or(0, Vec::len);
                            skipped += v["skipped"].as_array().map_or(0, Vec::len);
                            warnings += v["warning_count"].as_u64().unwrap_or(0) as usize;
                        }
                        Err(e) => errors.push(e.to_string()),
                    }
                }
                if imported > 0 && !app.ui.expanded_fx.contains(&"Presets".to_string()) {
                    app.ui.expanded_fx.push("Presets".to_string());
                }
                app.ui.status = if let Some(e) = errors.first() {
                    if imported > 0 {
                        tlf!("Imported {n} preset(s); not imported: {error}", n = imported, error = e)
                    } else {
                        e.clone()
                    }
                } else if skipped > 0 && warnings > 0 {
                    tlf!(
                        "Imported {n} preset(s); {skipped} skipped, {warnings} with unsupported parameters",
                        n = imported,
                        skipped = skipped,
                        warnings = warnings
                    )
                } else if skipped > 0 {
                    tlf!("Imported {n} preset(s); {skipped} skipped", n = imported, skipped = skipped)
                } else if warnings > 0 {
                    tlf!("Imported {n} preset(s); {warnings} with unsupported parameters", n = imported, warnings = warnings)
                } else {
                    tlf!("Imported {n} preset(s)", n = imported)
                };
            });
        }
        "presets.scan" => match app.session.execute("presets.scan", json!({})) {
            Ok(v) => {
                let imported = v["imported"].as_u64().unwrap_or(0);
                let removed = v["removed"].as_u64().unwrap_or(0);
                let changed = v["changed"].as_bool().unwrap_or(false);
                app.ui.status = if let Some(e) = v["errors"].as_array().and_then(|a| a.first()).and_then(|v| v.as_str()) {
                    e.to_string()
                } else if imported > 0 || removed > 0 {
                    tlf!("Preset library updated: {imported} loaded, {removed} removed", imported = imported, removed = removed)
                } else if changed {
                    tl!("Preset library updated").to_string()
                } else {
                    tl!("Preset library is up to date").to_string()
                };
            }
            Err(e) => app.ui.status = e.to_string(),
        },
        _ => {}
    }
}

/// Open the Save Preset dialog for effects of a clip.
pub fn open_save(app: &mut FilmcraftApp, clip: u64, effects: Vec<usize>, name: &str) {
    app.ui.save_preset = Some(SavePresetDraft { clip, effects, name: name.to_string(), description: String::new(), keyframes: "scale".into() });
}

pub fn save_dialog(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut d) = app.ui.save_preset.clone() else { return };
    let mut open = true;
    let mut close = false;
    let mut apply = false;
    egui::Window::new(tl!("Save Preset")).id(egui::Id::new("Save Preset")).open(&mut open).collapsible(false).resizable(false).default_width(340.0).show(
        ctx,
        |ui| {
            ui.horizontal(|ui| {
                ui.label(tl!("Name:"));
                let r = ui.text_edit_singleline(&mut d.name);
                app.auto.add("savePreset.name", r.rect, "Name");
            });
            ui.add_space(4.0);
            ui.label(egui::RichText::new(tl!("Type")).strong());
            for (key, label) in [
                ("scale", tl!("Scale")),
                ("anchorIn", tl!("Anchor to In Point")),
                ("anchorOut", tl!("Anchor to Out Point")),
                ("none", tl!("Without keyframes")),
            ] {
                let r = ui.radio(d.keyframes == key, label);
                app.auto.add(&format!("savePreset.keyframes.{key}"), r.rect, label);
                if r.clicked() {
                    d.keyframes = key.into();
                }
            }
            ui.add_space(4.0);
            ui.label(tl!("Description:"));
            let r = ui.text_edit_multiline(&mut d.description);
            app.auto.add("savePreset.description", r.rect, "Description");
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let c = ui.button(tl!("Cancel"));
                app.auto.add("savePreset.cancel", c.rect, "Cancel");
                let o = ui.add_enabled(!d.name.trim().is_empty(), egui::Button::new(tl!("OK")));
                app.auto.add("savePreset.ok", o.rect, "OK");
                apply = o.clicked();
                close = apply || c.clicked();
            });
        },
    );
    app.ui.save_preset = if close || !open { None } else { Some(d.clone()) };
    if apply {
        let r = app.session.execute(
            "presets.save",
            json!({"clip": d.clip, "effects": d.effects, "name": d.name.trim(), "description": d.description, "keyframes": d.keyframes}),
        );
        match r {
            Ok(_) => app.ui.status = tlf!("Saved preset “{name}”", name = d.name.trim()),
            Err(e) => app.ui.status = e.to_string(),
        }
    }
}
