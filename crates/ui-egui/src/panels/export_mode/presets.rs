use super::quick::list_row;
use super::settings::check;
use super::{Align2, Color32, FileDialog, FilmcraftApp, Rect, Reg, Sense, Value, apply_preset, json, pos2, preset_key, settings_json, vec2};

// ---------------------------------------------------------------------------------------------
// Preset Manager
// ---------------------------------------------------------------------------------------------

pub(super) fn preset_manager(app: &mut FilmcraftApp, ctx: &egui::Context) {
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
    let q = m.query.trim().to_string();
    egui::Window::new(tl!("Preset Manager"))
        .id(egui::Id::unique("Preset Manager"))
        .id(egui::Id::unique("preset-manager"))
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
                let r = crate::widgets::accessible_name(
                    ui.add(egui::TextEdit::singleline(&mut m.save_name).hint_text(tl!("Preset name")).desired_width(200.0)),
                    "Preset name",
                );
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
        Some("import") => app.pick_ui(FileDialog::open_file(tl!("Export presets"), &["json"]), |app, paths| match paths.into_iter().next() {
            Some(path) => {
                if let Err(e) = app.session.execute("export.presets.import", json!({"path": path})) {
                    app.ui.status = e.to_string();
                }
            }
            None => app.ui.status = tl!("Import: no file chosen (agents: export.presets.import {path})").into(),
        }),
        Some(_) => {
            let selected = m.selected.clone();
            app.pick_ui(FileDialog::save_as(tl!("Export presets"), &["json"], &format!("{selected}.json")), move |app, paths| match paths.into_iter().next() {
                Some(path) => {
                    if let Err(e) = app.session.execute("export.presets.export", json!({"path": path, "names": [selected]})) {
                        app.ui.status = e.to_string();
                    }
                }
                None => app.ui.status = tl!("Export: no file chosen (agents: export.presets.export {path, names})").into(),
            })
        }
        None => {}
    }
    if let Some(name) = apply
        && apply_preset(app, &name)
    {
        close = true;
    }
    app.ui.export.manager = if close || !open || crate::widgets::escape_closes(ctx) { None } else { Some(m) };
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
