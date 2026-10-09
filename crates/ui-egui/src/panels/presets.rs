//! Effect presets in the UI: the Presets bin of the Effects panel (built-in and user presets; drag
//! onto a clip or double-click to apply to the selection; right-click a user preset to delete or
//! export it) and the Save Preset dialog (Effect Controls ▸ right-click an effect ▸ Save Preset…).
//!
//! Automation ids: `effects.preset.<name>`, `savePreset.name`, `savePreset.description`,
//! `savePreset.keyframes.<scale|anchorIn|anchorOut|none>`, `savePreset.ok`, `savePreset.cancel`,
//! `savePreset.close` (the title-bar ×); a preset's right-click menu
//! `effects.preset.<name>.<delete|export|import>`.

use egui::{Align2, Rect, Sense, pos2, vec2};
use serde_json::json;

use crate::FilmcraftApp;
use crate::icons::{self, Icon};
use crate::state::SavePresetDraft;
use crate::theme::Tokens;

/// The rows of the Presets folder (inside the Effects panel's scroll area).
pub fn folder_rows(app: &mut FilmcraftApp, ui: &mut egui::Ui, filter: &str) -> Option<(String, String)> {
    let t = app.tokens;
    let mut action = None;
    for p in app.session.presets.all() {
        if !filter.is_empty() && !p.name.to_ascii_lowercase().contains(filter) {
            continue;
        }
        let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::click_and_drag());
        if resp.hovered() {
            ui.painter().rect_filled(r, 0.0, t.hover);
        }
        let x = r.min.x + 30.0;
        icons::paint(
            ui.painter(),
            Rect::from_center_size(pos2(x, r.center().y), vec2(13.0, 13.0)),
            Icon::Sparkle,
            if p.builtin { t.text_dim } else { t.accent },
        );
        ui.painter().text(pos2(x + 12.0, r.center().y), Align2::LEFT_CENTER, &p.name, Tokens::ui(12.0), t.text);
        app.auto.add(&format!("effects.preset.{}", p.name), r, &p.name);
        let tip = if p.description.is_empty() { p.name.clone() } else { p.description.clone() };
        let resp = resp.on_hover_text(tip);
        if resp.drag_started() {
            crate::panels::start_drag_effect(ui, &format!("preset:{}", p.name));
        }
        if resp.double_clicked() {
            action = Some(("presets.apply".to_string(), p.name.clone()));
        }
        resp.context_menu(|ui| {
            if !p.builtin {
                let b = ui.button("Delete Preset");
                app.auto.add(&format!("effects.preset.{}.delete", p.name), b.rect, "Delete Preset");
                if b.clicked() {
                    action = Some(("presets.delete".to_string(), p.name.clone()));
                    ui.close();
                }
            }
            let b = ui.button("Export Preset…");
            app.auto.add(&format!("effects.preset.{}.export", p.name), b.rect, "Export Preset…");
            if b.clicked() {
                action = Some(("presets.export".to_string(), p.name.clone()));
                ui.close();
            }
            let b = ui.button("Import Presets…");
            app.auto.add(&format!("effects.preset.{}.import", p.name), b.rect, "Import Presets…");
            if b.clicked() {
                action = Some(("presets.import".to_string(), String::new()));
                ui.close();
            }
        });
    }
    action
}

/// Run a Presets-bin action.
pub fn run(app: &mut FilmcraftApp, cmd: &str, name: &str) {
    let r = match cmd {
        "presets.apply" => app.session.execute(cmd, json!({"preset": name})),
        "presets.delete" => app.session.execute(cmd, json!({"name": name})),
        "presets.export" => {
            let Some(path) = app.hooks.pick_save_as.as_mut().and_then(|f| f("Effect presets", &["json"], &format!("{name}.json"))) else { return };
            app.session.execute(cmd, json!({"path": path, "names": [name]}))
        }
        "presets.import" => {
            let Some(path) = app.hooks.pick_open_file.as_mut().and_then(|f| f("Effect presets", &["json"])) else { return };
            app.session.execute(cmd, json!({"path": path}))
        }
        _ => return,
    };
    if let Err(e) = r {
        app.ui.status = e.to_string();
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
    let w = egui::Window::new("Save Preset").open(&mut open).collapsible(false).resizable(false).default_width(340.0).show(ctx, |ui| {
        ui.horizontal(|ui| {
            ui.label("Name:");
            let r = ui.text_edit_singleline(&mut d.name);
            app.auto.add("savePreset.name", r.rect, "Name");
        });
        ui.add_space(4.0);
        ui.label(egui::RichText::new("Type").strong());
        for (key, label) in [("scale", "Scale"), ("anchorIn", "Anchor to In Point"), ("anchorOut", "Anchor to Out Point"), ("none", "Without keyframes")] {
            let r = ui.radio(d.keyframes == key, label);
            app.auto.add(&format!("savePreset.keyframes.{key}"), r.rect, label);
            if r.clicked() {
                d.keyframes = key.into();
            }
        }
        ui.add_space(4.0);
        ui.label("Description:");
        let r = ui.text_edit_multiline(&mut d.description);
        app.auto.add("savePreset.description", r.rect, "Description");
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let c = ui.button("Cancel");
            app.auto.add("savePreset.cancel", c.rect, "Cancel");
            let o = ui.add_enabled(!d.name.trim().is_empty(), egui::Button::new("OK"));
            app.auto.add("savePreset.ok", o.rect, "OK");
            apply = o.clicked();
            close = apply || c.clicked();
        });
    });
    if let Some(w) = w {
        app.auto.add("savePreset.close", crate::widgets::window_close_rect(ctx, w.response.rect, None), "Close");
    }
    app.ui.save_preset = if close || !open { None } else { Some(d.clone()) };
    if apply {
        let r = app.session.execute(
            "presets.save",
            json!({"clip": d.clip, "effects": d.effects, "name": d.name.trim(), "description": d.description, "keyframes": d.keyframes}),
        );
        match r {
            Ok(_) => app.ui.status = format!("Saved preset \u{201c}{}\u{201d}", d.name.trim()),
            Err(e) => app.ui.status = e.to_string(),
        }
    }
}
