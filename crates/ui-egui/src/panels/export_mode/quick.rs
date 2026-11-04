use super::settings::{draft_text, location_link};
use super::{Align2, Color32, DEFAULT_PRESET, FilmcraftApp, Reg, Sense, Tokens, builtin_presets, is_export_busy, json, pos2, preset_key, vec2};

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

/// The Quick Export path for the preset's extension `ext`, or None when `path` already fits. No
/// folder means the default one and no file name the sequence's; another format's extension is
/// swapped and any other kept, so a typed or chosen name survives a preset change.
fn quick_file(app: &FilmcraftApp, path: &str, seq_name: &str, ext: &str) -> Option<String> {
    let p = std::path::Path::new(path);
    let folder = path.ends_with(['/', '\\']);
    let dir = if folder { Some(p) } else { p.parent() }.filter(|d| !d.as_os_str().is_empty());
    let name = if folder { None } else { p.file_name().and_then(std::ffi::OsStr::to_str) }.filter(|n| !n.trim().is_empty());
    let cur = name.and_then(|n| std::path::Path::new(n).extension()).and_then(std::ffi::OsStr::to_str).unwrap_or("");
    if dir.is_some() && name.is_some() && cur.eq_ignore_ascii_case(ext) {
        return None;
    }
    let file = match name {
        // Sequence names come from the project, not from this Quick Export interaction. Keep
        // metadata in one filename component before joining it to the selected/default folder.
        None => format!("{}.{}", filmcraft_engine::export_tools::file_safe(seq_name), ext),
        Some(n) if cur.eq_ignore_ascii_case(ext) => n.to_string(),
        Some(n) if !cur.is_empty() && app.session.export_presets.all().iter().any(|x| x.settings.extension().eq_ignore_ascii_case(cur)) => {
            std::path::Path::new(n).with_extension(ext).to_string_lossy().to_string()
        }
        Some(n) => format!("{n}.{ext}"),
    };
    let dir = dir.map(std::path::Path::to_path_buf).unwrap_or_else(|| filmcraft_engine::export_tools::default_export_dir(&app.session));
    Some(dir.join(file).to_string_lossy().to_string())
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
    let format = preset.as_ref().map(|p| p.settings.format.label()).unwrap_or("MP4");
    let seq_name = app.session.project.item(seq).map(|i| i.name.clone()).unwrap_or_else(|| "Sequence".into());
    if let Some(p) = quick_file(app, &app.ui.export.quick_path, &seq_name, ext) {
        app.ui.export.quick_path = p;
    }
    let path = std::path::Path::new(&app.ui.export.quick_path);
    let file_name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let folder = path.parent().map(|d| d.to_string_lossy().to_string()).unwrap_or_default();
    let est = app
        .session
        .execute("export.resolve", json!({"preset": app.ui.export.quick_preset}))
        .ok()
        .and_then(|v| v["summary"]["estimated_size"].as_str().map(str::to_string));
    let presets = quick_presets(app);
    let mut reg = Reg::default();
    let mut go = false;
    let mut close = false;
    let mut browse = false;
    let mut typed: Option<String> = None;
    let busy = is_export_busy(app);
    let q = &mut app.ui.export;
    let area = egui::Area::new(egui::Id::unique("quick-export")).order(egui::Order::Foreground).fixed_pos(anchor).show(ctx, |ui| {
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
            ui.add_enabled_ui(!busy, |ui| {
                ui.label(egui::RichText::new(tl!("File Name & Location")).color(t.text_dim).size(11.5));
                typed = draft_text(ui, &mut reg, "quickExport.path", &file_name, tl!("File Name"));
                let r = location_link(ui, &t, &folder);
                reg.add("quickExport.location", r.rect, folder.clone());
                browse = r.clicked();
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
                        egui::Button::new(egui::RichText::new(tl!("Export")).color(Color32::WHITE))
                            .fill(t.accent)
                            .corner_radius(12.0)
                            .min_size(vec2(90.0, 26.0)),
                    );
                    reg.add("quickExport.go", r.rect, "Export");
                    go = r.clicked();
                });
            });
        });
    });
    let clicked_elsewhere = area.response.clicked_elsewhere();
    // the header click that opened the popup is not a click elsewhere
    let toggled = ctx.data_mut(|d| d.remove_temp::<bool>(egui::Id::unique("quick-export-toggled")).unwrap_or(false));
    reg.flush(app);
    if let Some(v) = typed {
        // a file name goes in the shown folder; a typed or pasted path replaces both
        app.ui.export.quick_path = if v.contains(['/', '\\']) { v } else { std::path::Path::new(&folder).join(v.trim()).to_string_lossy().to_string() };
    }
    if browse
        && !busy
        && let Some(p) = app.hooks.pick_save_as.as_mut().and_then(|f| f(format, &[ext], &app.ui.export.quick_path))
    {
        app.ui.export.quick_path = p;
    }
    if go && !busy {
        let p = json!({"preset": app.ui.export.quick_preset, "path": app.ui.export.quick_path});
        match app.session.execute("export.quick", p) {
            Ok(v) => app.ui.status = tlf!("Quick Export: {path}", path = v["path"].as_str().unwrap_or("")),
            Err(e) => app.ui.status = e.to_string(),
        }
        close = true;
    }
    if close || crate::widgets::escape_closes(ctx) || (clicked_elsewhere && !toggled) {
        app.ui.export.quick_open = false;
    }
}

/// A left-aligned selectable list row of a fixed width.
pub(super) fn list_row(ui: &mut egui::Ui, t: &Tokens, selected: bool, text: &str, width: f32) -> egui::Response {
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
