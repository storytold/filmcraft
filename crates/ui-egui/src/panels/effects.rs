//! Effects panel: searchable folder tree of presets, audio/video effects and transitions (from the
//! effect definitions). Drag onto a clip or edit point; double-click applies to the selection.
//!
//! Lumetri Presets: each preset row has a thumbnail (our Lumetri on the procedural preview
//! picture, `filmcraft_render::lumetri_presets`); double-click (or right-click ▸ Apply to Selected
//! Clips, `effects.presetMenu.apply`) applies it (`lumetri.applyPreset`).
//! When the panel is wide (maximized), clicking a Lumetri Presets folder shows its presets as a
//! thumbnail grid to the right of the tree. Automation ids: `effects.search`, `effects.lumetriPreset.<name>`,
//! `effects.presetGrid` (the grid area), `effects.presetGrid.<name>`.

use egui::{Align2, Color32, Rect, Sense, pos2, vec2};
use serde_json::json;

use crate::FilmcraftApp;
use crate::icons::{self, Icon};
use crate::theme::Tokens;

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let sr = Rect::from_min_size(rect.min + vec2(8.0, 6.0), vec2(rect.width() - 16.0, 22.0));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(sr));
    let mut q = app.ui.effects_search.clone();
    let sresp = crate::widgets::search_field(&mut child, &mut q, "Search effects", sr.width(), &t);
    app.auto.add("effects.search", sresp.rect, "Search effects");
    app.ui.effects_search = q.clone();
    let body = Rect::from_min_max(pos2(rect.min.x, sr.max.y + 6.0), rect.max);
    // wide panel: the tree on the left, the Lumetri Presets thumbnail grid on the right
    let grid_area = (body.width() > 640.0).then(|| Rect::from_min_max(pos2(body.min.x + 300.0, body.min.y), body.max));
    let body = match grid_area {
        Some(g) => Rect::from_min_max(body.min, pos2(g.min.x - 4.0, body.max.y)),
        None => body,
    };
    let mut bui = ui.new_child(egui::UiBuilder::new().max_rect(body).id_salt("fx-body"));
    bui.set_clip_rect(body);
    let filter = q.to_ascii_lowercase();
    let defs = filmcraft_project::effect_defs();
    // folder tree: top-level categories in Premiere's order
    let tops = filmcraft_project::vtransition::EFFECT_TOP_FOLDERS;
    let mut apply: Option<String> = None;
    let mut lumetri_apply: Option<String> = None;
    let mut preset_action: Option<(String, String)> = None;
    egui::ScrollArea::vertical().id_salt("fx-scroll").auto_shrink([false, false]).show(&mut bui, |ui| {
        for &top in tops {
            if top == "Presets" {
                let any = filter.is_empty() || app.session.presets.all().iter().any(|p| p.name.to_ascii_lowercase().contains(&filter));
                if any && folder_row(app, ui, top, 0, &filter) {
                    preset_action = crate::panels::presets::folder_rows(app, ui, &filter).or(preset_action.take());
                }
                continue;
            }
            if top == "Lumetri Presets" {
                if let Some(name) = lumetri_rows(app, ui, &filter) {
                    lumetri_apply = Some(name);
                }
                continue;
            }
            let mut items: Vec<_> = defs
                .iter()
                .filter(|d| d.category.first() == Some(&top))
                .filter(|d| filter.is_empty() || d.name.to_ascii_lowercase().contains(&filter))
                .collect();
            // Premiere lists effect folders and effects alphabetically (obsolete ones last)
            if top == "Video Effects" || top == "Legacy" {
                items.sort_by_key(|d| (d.category.get(1) == Some(&"Obsolete"), d.category.get(1).copied().unwrap_or(""), d.name.to_ascii_lowercase()));
            }
            if !filter.is_empty() && items.is_empty() {
                continue;
            }
            let open = folder_row(app, ui, top, 0, &filter);
            if !open {
                continue;
            }
            // Sub-folders in definition order; loose items (no sub-folder, e.g. Audio Effects ▸
            // Balance / Mute / Volume) come last, outside any folder.
            let sub = |d: &filmcraft_project::EffectDef| d.category.get(1).copied().unwrap_or("");
            let mut subs: Vec<&str> = items.iter().map(|d| sub(d)).collect();
            subs.dedup();
            subs.sort_by_key(|s| s.is_empty());
            let mut seen = Vec::new();
            for s in subs {
                if seen.contains(&s) {
                    continue;
                }
                seen.push(s);
                let key = format!("{top}/{s}");
                if !s.is_empty() && !folder_row(app, ui, &key, 1, &filter) {
                    continue;
                }
                for d in items.iter().filter(|d| sub(d) == s) {
                    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::click_and_drag());
                    if resp.hovered() {
                        ui.painter().rect_filled(r, 0.0, t.hover);
                    }
                    let x = r.min.x + 46.0;
                    let icon_r = Rect::from_center_size(pos2(x - 10.0, r.center().y), vec2(13.0, 13.0));
                    let is_tr = matches!(d.kind, filmcraft_project::EffectKind::VideoTransition | filmcraft_project::EffectKind::AudioTransition);
                    ui.painter().rect_stroke(icon_r, 2.0, egui::Stroke::new(1.0, t.text_dim), egui::StrokeKind::Inside);
                    if is_tr {
                        ui.painter().line_segment([icon_r.left_bottom(), icon_r.right_top()], egui::Stroke::new(1.0, t.text_dim));
                    } else {
                        ui.painter().text(icon_r.center(), Align2::CENTER_CENTER, "fx", Tokens::ui(8.0), t.text_dim);
                    }
                    ui.painter().text(pos2(x, r.center().y), Align2::LEFT_CENTER, d.name, Tokens::ui(12.0), t.text);
                    // badges: accelerated / 32-bit / YUV
                    let mut bx = r.max.x - 8.0;
                    for (on, label) in [(d.yuv, "YUV"), (d.float32, "32"), (d.accelerated, "⚡")] {
                        if on {
                            let br = Rect::from_min_size(pos2(bx - 22.0, r.min.y + 3.0), vec2(20.0, 14.0));
                            ui.painter().rect_filled(br, 2.0, Color32::from_rgb(48, 48, 48));
                            ui.painter().text(br.center(), Align2::CENTER_CENTER, label, Tokens::ui(8.5), t.text_dim);
                            bx -= 24.0;
                        }
                    }
                    app.auto.add(&format!("effects.item.{}", d.id), r, d.name);
                    if resp.drag_started() {
                        crate::panels::start_drag_effect(ui, d.id);
                    }
                    if resp.double_clicked() {
                        apply = Some(d.id.to_string());
                    }
                }
            }
        }
    });
    if let Some(g) = grid_area
        && let Some(name) = preset_grid(app, ui, g)
    {
        lumetri_apply = Some(name);
    }
    if let Some(name) = lumetri_apply
        && let Err(e) = app.session.execute("lumetri.applyPreset", json!({"name": name}))
    {
        app.ui.status = e.to_string();
    }
    if let Some((cmd, name)) = preset_action {
        crate::panels::presets::run(app, &cmd, &name);
    }
    if let Some(id) = apply {
        let r = app.session.execute("effects.apply", json!({"effect": id}));
        if let Err(e) = r {
            app.ui.status = e.to_string();
        }
    }
}

fn folder_row(app: &mut FilmcraftApp, ui: &mut egui::Ui, key: &str, depth: usize, filter: &str) -> bool {
    let t = app.tokens;
    let open = !filter.is_empty() || app.ui.expanded_fx.iter().any(|k| k == key);
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(r, 0.0, t.hover);
    }
    let x = r.min.x + 8.0 + depth as f32 * 16.0;
    icons::paint(
        ui.painter(),
        Rect::from_center_size(pos2(x + 4.0, r.center().y), vec2(10.0, 10.0)),
        if open { Icon::ChevronDown } else { Icon::ChevronRight },
        t.text_dim,
    );
    icons::paint(ui.painter(), Rect::from_center_size(pos2(x + 18.0, r.center().y), vec2(13.0, 13.0)), Icon::Folder, t.text_dim);
    let name = key.rsplit('/').next().unwrap_or(key);
    ui.painter().text(pos2(x + 30.0, r.center().y), Align2::LEFT_CENTER, name, Tokens::ui(12.0), t.text);
    app.auto.add(&format!("effects.folder.{key}"), r, name);
    if resp.clicked() {
        if open {
            app.ui.expanded_fx.retain(|k| k != key);
        } else {
            app.ui.expanded_fx.push(key.to_string());
        }
    }
    open
}

/// Right-click menu of a Lumetri preset: Apply to Selected Clips (automation id
/// `effects.presetMenu.apply`). True when chosen.
fn apply_menu(app: &mut FilmcraftApp, resp: &egui::Response) -> bool {
    let mut chosen = false;
    resp.context_menu(|ui| {
        let b = ui.button("Apply to Selected Clips");
        app.auto.add("effects.presetMenu.apply", b.rect, "Apply to Selected Clips");
        if b.clicked() {
            chosen = true;
            ui.close();
        }
    });
    chosen
}

/// A preset's thumbnail texture (rendered once per size, cached by name).
fn preset_texture(app: &mut FilmcraftApp, ctx: &egui::Context, p: &filmcraft_render::lumetri_presets::LumetriPreset, w: usize) -> egui::TextureId {
    let name = format!("lumetri-preset-{}-{w}", p.name);
    if let Some((id, _)) = app.texture_existing(&name) {
        return id;
    }
    let h = (w * 9 / 16).max(1);
    let img = filmcraft_render::lumetri_presets::thumbnail(p, w, h);
    let rgba = crate::frames::Rgba { w: img.w, h: img.h, px: img.over_black_rgba8() };
    let key = crate::frames::FrameKey {
        target: crate::frames::Target::Item(filmcraft_project::ItemId(u64::MAX)),
        frame: 0,
        size: w as u32,
        revision: 0,
        draft: false,
    };
    app.texture_for(ctx, &name, key, &rgba)
}

/// The Lumetri Presets folder of the tree: sub-folders and preset rows with thumbnails. Returns
/// the preset to apply (double-click).
fn lumetri_rows(app: &mut FilmcraftApp, ui: &mut egui::Ui, filter: &str) -> Option<String> {
    use filmcraft_render::lumetri_presets as lp;
    let t = app.tokens;
    let top = "Lumetri Presets";
    let presets: Vec<lp::LumetriPreset> = lp::presets().into_iter().filter(|p| filter.is_empty() || p.name.to_ascii_lowercase().contains(filter)).collect();
    if !filter.is_empty() && presets.is_empty() {
        return None;
    }
    if !folder_row(app, ui, top, 0, filter) {
        return None;
    }
    let mut apply = None;
    for folder in lp::FOLDERS {
        let items: Vec<&lp::LumetriPreset> = presets.iter().filter(|p| p.folder == folder).collect();
        if items.is_empty() {
            continue;
        }
        let key = format!("{top}/{folder}");
        let was_open = !filter.is_empty() || app.ui.expanded_fx.contains(&key);
        let open = folder_row(app, ui, &key, 1, filter);
        // the folder clicked last is the one the wide panel shows as a grid (`folder_row` toggles
        // the expanded state on a click and returns the state it drew)
        let now = !filter.is_empty() || app.ui.expanded_fx.contains(&key);
        if now != was_open {
            app.ui.lumetri_grid_folder = Some(folder.to_string());
        }
        if !open {
            continue;
        }
        for p in items {
            let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::click());
            if resp.hovered() {
                ui.painter().rect_filled(r, 0.0, t.hover);
            }
            let tex = preset_texture(app, ui.ctx(), p, 64);
            let tr = Rect::from_min_size(pos2(r.min.x + 40.0, r.min.y + 3.0), vec2(32.0, 18.0));
            ui.painter().image(tex, tr, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            ui.painter().text(pos2(tr.max.x + 8.0, r.center().y), Align2::LEFT_CENTER, p.name, Tokens::ui(12.0), t.text);
            app.auto.add(&format!("effects.lumetriPreset.{}", p.name), r, p.name);
            let resp = resp.on_hover_text(p.description);
            if apply_menu(app, &resp) {
                apply = Some(p.name.to_string());
            }
            if resp.double_clicked() {
                apply = Some(p.name.to_string());
            }
        }
    }
    apply
}

/// The wide Effects panel's thumbnail grid of the selected Lumetri Presets folder. Returns the
/// preset to apply (double-click).
fn preset_grid(app: &mut FilmcraftApp, ui: &mut egui::Ui, area: Rect) -> Option<String> {
    use filmcraft_render::lumetri_presets as lp;
    let t = app.tokens;
    let folder = app.ui.lumetri_grid_folder.clone()?;
    app.auto.add("effects.presetGrid", area, &folder);
    let presets: Vec<lp::LumetriPreset> = lp::presets().into_iter().filter(|p| p.folder == folder).collect();
    let (cw, ch, gap, label_h) = (150.0f32, 84.0f32, 6.0f32, 18.0f32);
    let cols = (((area.width() - gap) / (cw + gap)).floor() as usize).max(1);
    let mut apply = None;
    for (k, p) in presets.iter().enumerate() {
        let x = area.min.x + gap + (k % cols) as f32 * (cw + gap);
        let y = area.min.y + gap + (k / cols) as f32 * (ch + label_h + gap);
        let cell = Rect::from_min_size(pos2(x, y), vec2(cw, ch + label_h));
        if cell.max.y > area.max.y {
            break;
        }
        let pic = Rect::from_min_size(cell.min, vec2(cw, ch));
        let tex = preset_texture(app, ui.ctx(), p, 160);
        let resp = ui.interact(cell, egui::Id::new(("lumetri-grid", p.name)), Sense::click()).on_hover_text(p.description);
        ui.painter().rect_filled(cell, 3.0, if resp.hovered() { t.hover } else { t.field_bg });
        ui.painter().image(tex, pic.shrink(2.0), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        let label = ui.painter().layout(p.name.to_string(), Tokens::ui(11.0), t.text, cw - 6.0);
        ui.painter().galley(pos2(cell.min.x + 4.0, pic.max.y + 2.0), label, t.text);
        app.auto.add(&format!("effects.presetGrid.{}", p.name), cell, p.name);
        if apply_menu(app, &resp) {
            apply = Some(p.name.to_string());
        }
        if resp.double_clicked() {
            apply = Some(p.name.to_string());
        }
    }
    apply
}
