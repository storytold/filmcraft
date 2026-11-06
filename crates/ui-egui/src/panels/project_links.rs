//! Styled composition picker; filesystem access uses the host's browser services.
use crate::FilmcraftApp;
use egui::{Context, Id};
use filmcraft_engine::media_browser::{self as mb, DirEntry};
use serde_json::{Value, json};

#[derive(Clone, Default)]
struct Dialog {
    executable: String,
    path: String,
    objects: Vec<Value>,
    object: String,
    message: String,
    bake_path: String,
    initialized: bool,
    folder: String,
    folder_edit: String,
    entries: Vec<DirEntry>,
    places: Vec<(String, String)>,
    browser_error: String,
}

fn id() -> Id {
    Id::unique("linkedProjects.dialog")
}

pub fn open(ctx: &Context) {
    ctx.data_mut(|d| {
        d.insert_temp(id(), Dialog::default());
    });
}

/// Explain session-only renderer authorization before the user attempts a final export.
/// This is a cheap readiness hint; the worker still checks revisions and cache validity.
pub(super) fn export_notice(app: &mut FilmcraftApp, ui: &mut egui::Ui) {
    let adapters = &app.session.project_links.adapters;
    // The first linked composition `seq` uses whose renderer this session has not enabled.
    let missing = |project: &filmcraft_project::Project, seq| {
        if project.project_links.is_empty() {
            return None;
        }
        let used = filmcraft_engine::project_tools::closure(project, &[seq]);
        project
            .project_links
            .iter()
            .find(|(id, link)| used.contains(id) && !link.baked && project.item(**id).is_some() && !adapters.contains_key(&link.provider))
            .map(|(_, link)| link.object_name.clone())
    };
    // Immediate export uses the active sequence; each ready queue item exports its own snapshot.
    let active = app.session.state.active_sequence.and_then(|seq| missing(&app.session.project, seq));
    let name = active.or_else(|| {
        app.session
            .export_queue
            .items
            .iter()
            .filter(|i| i.status == filmcraft_engine::export_tools::QueueStatus::Ready)
            .find_map(|i| missing(&i.project, i.sequence))
    });
    let Some(name) = name else { return };
    let message = tlf!("Export blocked: enable the renderer for linked composition {name} in this session, or bake it to ordinary media.", name);
    egui::Frame::new().fill(app.tokens.field_bg).inner_margin(8).show(ui, |ui| {
        ui.set_width((ui.available_width() - 16.0).max(0.0));
        let response = ui.add(egui::Label::new(egui::RichText::new(&message).color(app.tokens.danger)).wrap());
        app.auto.add_response("export.linkedProject.warning", &response, &message);
        let response = ui.button(tl!("Configure linked projects…"));
        app.auto.add_response("export.linkedProject.configure", &response, "Configure linked projects…");
        if response.clicked()
            && let Err(error) = crate::menus::invoke(app, ui.ctx(), "linkedProject.manage", json!({}))
        {
            app.ui.status = error;
        }
    });
    ui.add_space(8.0);
}

fn button(app: &mut FilmcraftApp, ui: &mut egui::Ui, id: &str, label: &str) -> bool {
    let r = ui.button(label);
    app.auto.add_response(id, &r, label);
    r.clicked()
}

fn navigate(app: &FilmcraftApp, state: &mut Dialog, folder: String) {
    state.browser_error.clear();
    match app.session.services.list_entries(&folder) {
        Some(Ok(entries)) => {
            if !state.folder.is_empty() && state.folder != folder {
                state.path.clear();
                state.objects.clear();
                state.object.clear();
            }
            state.entries = entries.into_iter().filter(|e| e.is_dir || e.name.to_ascii_lowercase().ends_with(".ecproj")).collect();
            state.entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
            if state.entries.len() > 2048 {
                state.entries.truncate(2048);
                state.browser_error = "Showing the first 2,048 entries. Enter a project path below to select another file.".into();
            }
            state.folder = folder.clone();
            state.folder_edit = folder;
        }
        Some(Err(e)) => state.browser_error = e.to_string(),
        None => state.browser_error = "Folder browsing is unavailable on this host. Enter the project path below.".into(),
    }
}

fn inspect(app: &mut FilmcraftApp, state: &mut Dialog) {
    // Never leave a composition from the previous file selected after an error.
    state.objects.clear();
    state.object.clear();
    match app.session.execute("linkedProject.inspect", json!({"path":state.path})) {
        Ok(v) => {
            state.objects = v["objects"].as_array().cloned().unwrap_or_default();
            state.object = state.objects.first().and_then(|o| o["id"].as_str()).unwrap_or("").into();
            state.message = if state.objects.is_empty() { "This project contains no compositions.".into() } else { String::new() };
        }
        Err(e) => state.message = e.to_string(),
    }
}

fn initialize(app: &mut FilmcraftApp, state: &mut Dialog) {
    state.initialized = true;
    if let Some(home) = app.session.services.home_dir() {
        state.places.push(("Home".into(), home));
    }
    for path in &app.session.prefs.media_browser.favorites {
        state.places.push((mb::base_name(path), path.clone()));
    }
    for volume in app.session.services.volumes() {
        state.places.push((volume.name, volume.path));
    }
    for path in &app.session.prefs.media_browser.recent {
        state.places.push((mb::base_name(path), path.clone()));
    }
    let mut folder = app.session.prefs.media_browser.last_dir.clone();
    if let Some(link) = app.session.project.project_links.values().find(|l| l.provider == "effectcraft") {
        state.path = link.project_path.clone();
        folder = mb::parent(&state.path).unwrap_or_default();
    }
    if folder.is_empty() {
        folder = app.session.services.home_dir().unwrap_or_default();
    }
    if !folder.is_empty() {
        navigate(app, state, folder);
    }
    if !state.path.is_empty() && app.session.project_links.adapters.contains_key("effectcraft") {
        inspect(app, state);
    }
}

fn browser(app: &mut FilmcraftApp, ui: &mut egui::Ui, state: &mut Dialog, height: f32) {
    ui.horizontal(|ui| {
        if button(app, ui, "linkedProjects.up", "Up")
            && let Some(parent) = mb::parent(&state.folder)
        {
            navigate(app, state, parent);
        }
        let r = ui.add(egui::TextEdit::singleline(&mut state.folder_edit).desired_width((ui.available_width() - 95.0).max(80.0)));
        app.auto.add_response("linkedProjects.folderPath", &r, "Folder path");
        let go = button(app, ui, "linkedProjects.go", "Go");
        if go || r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            navigate(app, state, state.folder_edit.clone());
        }
        if button(app, ui, "linkedProjects.reload", "Refresh") {
            navigate(app, state, state.folder.clone());
        }
    });
    ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), height), egui::Layout::left_to_right(egui::Align::Min), |ui| {
        ui.allocate_ui_with_layout(egui::vec2(165.0, height), egui::Layout::top_down(egui::Align::Min), |ui| {
            ui.strong("Folders");
            egui::ScrollArea::vertical().id_salt("linkedProjects.places").max_height(height - 24.0).show(ui, |ui| {
                for (index, (name, path)) in state.places.clone().into_iter().enumerate() {
                    let r = ui.add(egui::Button::selectable(state.folder == path, name).truncate());
                    app.auto.add_response(&format!("linkedProjects.place.{index}"), &r, &path);
                    if r.clicked() {
                        navigate(app, state, path);
                    }
                }
                ui.separator();
                let mut ancestors = Vec::new();
                let mut path = Some(state.folder.clone());
                for _ in 0..24 {
                    let Some(current) = path else { break };
                    if current.is_empty() {
                        break;
                    }
                    path = mb::parent(&current);
                    ancestors.push(current);
                }
                for (depth, path) in ancestors.into_iter().rev().enumerate() {
                    ui.horizontal(|ui| {
                        ui.add_space((depth.min(8) * 8) as f32);
                        let r = ui.add(egui::Button::selectable(state.folder == path, mb::base_name(&path)).truncate());
                        app.auto.add_response(&format!("linkedProjects.ancestor.{depth}"), &r, &path);
                        if r.clicked() {
                            navigate(app, state, path);
                        }
                    });
                }
            });
        });
        ui.separator();
        let files_width = ((ui.available_width() - 12.0) * 0.53).max(160.0);
        ui.allocate_ui_with_layout(egui::vec2(files_width, height), egui::Layout::top_down(egui::Align::Min), |ui| {
            ui.strong("EffectCraft projects");
            egui::ScrollArea::vertical().id_salt("linkedProjects.files").auto_shrink([false, false]).max_height(height - 24.0).show(ui, |ui| {
                for (index, entry) in state.entries.clone().into_iter().enumerate() {
                    let path = mb::join(&state.folder, &entry.name);
                    // Reserve room for the original vector folder icon; glyph availability varies by host.
                    let label = if entry.is_dir { format!("     {}", entry.name) } else { entry.name };
                    let r = ui.add(egui::Button::selectable(state.path == path, label).truncate());
                    if entry.is_dir {
                        crate::icons::paint(
                            ui.painter(),
                            egui::Rect::from_center_size(egui::pos2(r.rect.left() + 10.0, r.rect.center().y), egui::vec2(12.0, 12.0)),
                            crate::icons::Icon::Folder,
                            app.tokens.icon,
                        );
                    }
                    app.auto.add_response(&format!("linkedProjects.file.{index}"), &r, &path);
                    if r.clicked() {
                        if entry.is_dir {
                            navigate(app, state, path);
                        } else {
                            state.path = path;
                            inspect(app, state);
                        }
                    }
                }
                if state.entries.is_empty() {
                    ui.weak("No EffectCraft projects in this folder.");
                }
            });
        });
        ui.separator();
        ui.vertical(|ui| {
            ui.strong("Compositions");
            egui::ScrollArea::vertical().id_salt("linkedProjects.objects").auto_shrink([false, false]).max_height(height - 24.0).show(ui, |ui| {
                for object in &state.objects {
                    let key = object["id"].as_str().unwrap_or("");
                    let name = object["name"].as_str().unwrap_or("Composition");
                    let r = ui.add(egui::Button::selectable(state.object == key, name).truncate());
                    app.auto.add_response(&format!("linkedProjects.object.{key}"), &r, name);
                    if r.clicked() {
                        state.object = key.into();
                    }
                }
                if state.objects.is_empty() {
                    ui.weak("Select a project to list its compositions.");
                }
            });
        });
    });
    if !state.browser_error.is_empty() {
        ui.label(&state.browser_error);
    }
    ui.horizontal(|ui| {
        ui.label("Project:");
        let r = ui.add(egui::TextEdit::singleline(&mut state.path).desired_width((ui.available_width() - 135.0).max(80.0)));
        app.auto.add_response("linkedProjects.path", &r, "Project path");
        if r.changed() {
            state.objects.clear();
            state.object.clear();
        }
        if button(app, ui, "linkedProjects.inspect", "List compositions") {
            inspect(app, state);
        }
    });
}

pub fn show(app: &mut FilmcraftApp, ctx: &Context) {
    let Some(mut state) = ctx.data_mut(|d| d.get_temp::<Dialog>(id())) else { return };
    if !state.initialized {
        initialize(app, &mut state);
    }
    let mut open = true;
    let t = app.tokens;
    let available = ctx.content_rect().size();
    let width = (available.x - 48.0).clamp(300.0, 920.0);
    let height = (available.y - 380.0).clamp(120.0, 290.0);
    let response = egui::Modal::new(id()).frame(style::frame(&t)).show(ctx, |ui| {
        ui.set_width(width);
        style::header(ui, "Import EffectCraft Composition", width);
        egui::Frame::new().inner_margin(18).show(ui, |ui| {
            let enabled = app.session.project_links.adapters.contains_key("effectcraft");
            ui.label("Choose a project and composition. Saved changes update the linked clip automatically.");
            egui::CollapsingHeader::new(if enabled { "Renderer ready · Settings" } else { "Enable EffectCraft renderer" }).default_open(!enabled).show(
                ui,
                |ui| {
                    ui.label("Select your installed effectcraft-cli executable for this session:");
                    ui.horizontal(|ui| {
                        let r = ui.add(egui::TextEdit::singleline(&mut state.executable).desired_width((ui.available_width() - 120.0).max(80.0)));
                        app.auto.add_response("linkedProjects.renderer", &r, "Renderer executable");
                        if button(app, ui, "linkedProjects.enable", "Enable renderer") {
                            state.message = match app.session.execute("linkedProject.configure", json!({"executable":state.executable})) {
                                Ok(_) => "Renderer enabled for this session".into(),
                                Err(e) => e.to_string(),
                            };
                        }
                    });
                },
            );
            ui.separator();
            browser(app, ui, &mut state, height);
            let status = app.session.execute("linkedProject.status", json!({})).unwrap_or(Value::Null);
            let watch = progress_row(ui, &status);
            app.auto.add_response("linkedProjects.watchStatus", &watch, rendering_message(&status).unwrap_or("Watching for saved changes"));
            egui::CollapsingHeader::new("Manage linked items").show(ui, |ui| management(app, ui, &mut state, &status));
            if !state.message.is_empty() {
                ui.label(&state.message);
            }
            ui.separator();
            style::actions(ui, |ui| {
                let r = ui.add_enabled(enabled && !state.object.is_empty(), style::primary("Import"));
                app.auto.add_response("linkedProjects.import", &r, "Import linked composition");
                if r.clicked() {
                    let bin = app.import_bin().0;
                    match app.session.execute("linkedProject.import", json!({"path":state.path,"object":state.object,"bin":bin})) {
                        Ok(_) => {
                            app.ui.status = "Linked composition imported; rendering in background".into();
                            open = false;
                        }
                        Err(e) => state.message = e.to_string(),
                    }
                }
                let r = ui.add(style::secondary("Cancel"));
                app.auto.add_response("linkedProjects.close", &r, "Cancel");
                if r.clicked() {
                    open = false;
                }
            });
            if status["running"].as_bool() == Some(true) {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        });
    });
    if response.should_close() && !ctx.input(|i| i.pointer.any_down()) {
        open = false;
    }
    if open {
        ctx.data_mut(|d| d.insert_temp(id(), state));
    } else {
        ctx.data_mut(|d| d.remove::<Dialog>(id()));
    }
}

fn management(app: &mut FilmcraftApp, ui: &mut egui::Ui, state: &mut Dialog, status: &Value) {
    ui.label("New .mov destination for Bake (existing files are never overwritten):");
    let r = ui.add(egui::TextEdit::singleline(&mut state.bake_path).desired_width(f32::INFINITY));
    app.auto.add_response("linkedProjects.bakePath", &r, "Bake destination");
    egui::ScrollArea::vertical().id_salt("linkedProjects.items").max_height(120.0).show(ui, |ui| {
        for row in status["items"].as_array().into_iter().flatten() {
            let item = row["item"].as_u64().unwrap_or(0);
            ui.push_id(item, |ui| {
                ui.group(|ui| {
                    ui.label(row["link"]["object_name"].as_str().unwrap_or("Linked item"));
                    ui.label(row["status"]["message"].as_str().unwrap_or("Waiting for renderer"));
                    ui.horizontal(|ui| {
                        for (suffix, label, command, params) in [
                            ("bake", "Bake", "linkedProject.bake", json!({"item":item,"path":state.bake_path})),
                            ("restore", "Restore link", "linkedProject.bake", json!({"item":item,"restore":true})),
                            ("relink", "Relink to selection", "linkedProject.relink", json!({"item":item,"path":state.path,"object":state.object})),
                        ] {
                            if button(app, ui, &format!("linkedProjects.{item}.{suffix}"), label)
                                && let Err(e) = app.session.execute(command, params)
                            {
                                state.message = e.to_string();
                            }
                        }
                    });
                });
            });
        }
    });
    ui.horizontal(|ui| {
        for (suffix, label, command) in [("refresh", "Refresh", "linkedProject.refresh"), ("cancel", "Cancel render", "linkedProject.cancel")] {
            if button(app, ui, &format!("linkedProjects.{suffix}"), label)
                && let Err(e) = app.session.execute(command, json!({}))
            {
                state.message = e.to_string();
            }
        }
    });
}

// Background scans are brief and recurring. Keep their row and widget identities
// present while idle so the linked-item area never jumps when a scan starts/ends.
fn rendering_message(status: &Value) -> Option<&str> {
    status["progress"].as_str().filter(|message| status["running"].as_bool() == Some(true) && !message.trim().is_empty())
}

fn progress_row(ui: &mut egui::Ui, status: &Value) -> egui::Response {
    let message = rendering_message(status);
    ui.push_id("linkedProjects.progress", |ui| {
        ui.horizontal(|ui| {
            let size = egui::vec2(20.0, 20.0);
            if message.is_some() {
                ui.add_sized(size, egui::Spinner::new());
            } else {
                ui.allocate_space(size);
            }
            ui.add_sized([ui.available_width(), 20.0], egui::Label::new(message.unwrap_or("Watching for saved changes")).truncate())
        })
        .inner
    })
    .inner
}

/// EffectCraft's app-icon background, assets/app-icon/effectcraft-small.svg.
pub const EFFECTCRAFT_PINK: egui::Color32 = egui::Color32::from_rgb(0xe0, 0x36, 0x8f);

pub fn linked_color(app: &FilmcraftApp, item: filmcraft_project::ItemId) -> Option<egui::Color32> {
    app.session.project.project_links.get(&item).filter(|link| link.provider == "effectcraft" && !link.baked).map(|_| EFFECTCRAFT_PINK)
}

/// Composition-picker chrome, kept local while shared dialog styling is a separate PR.
/// These primitives match FilmCraft's shared modal styling without depending on that PR.
pub mod style {
    use crate::theme::Tokens;
    use egui::{Color32, CornerRadius, Frame, InnerResponse, Rect, RichText, Stroke, Ui, vec2};
    pub const RADIUS: u8 = 6;
    pub const HEADER_BG: Color32 = Color32::WHITE;
    pub const HEADER_TEXT: Color32 = Color32::from_gray(90);
    pub const PRIMARY_BG: Color32 = Color32::from_rgb(0x2f, 0x6b, 0xdf);

    pub fn frame(t: &Tokens) -> Frame {
        Frame::new().fill(t.panel_bg).stroke(Stroke::new(1.0, t.separator)).corner_radius(RADIUS).inner_margin(0)
    }

    /// Header for blocking Modal containers and custom draggable dialogs such as Export Frame.
    pub fn header(ui: &mut Ui, title: &str, width: f32) -> Rect {
        let (bar, _) = ui.allocate_exact_size(vec2(width, 30.0), egui::Sense::hover());
        ui.painter().rect_filled(bar, CornerRadius { nw: RADIUS, ne: RADIUS, sw: 0, se: 0 }, HEADER_BG);
        ui.painter().text(bar.left_center() + vec2(10.0, 0.0), egui::Align2::LEFT_CENTER, title, Tokens::ui(13.0), HEADER_TEXT);
        bar
    }

    pub fn primary(label: impl Into<String>) -> egui::Button<'static> {
        egui::Button::new(RichText::new(label.into()).color(Color32::WHITE)).fill(PRIMARY_BG).min_size(vec2(88.0, 28.0)).corner_radius(14)
    }

    pub fn secondary(label: impl Into<String>) -> egui::Button<'static> {
        egui::Button::new(label.into()).min_size(vec2(88.0, 28.0)).corner_radius(14)
    }

    /// Right-aligned actions; callers list Cancel first, then the primary action.
    pub fn actions<R>(ui: &mut Ui, contents: impl FnOnce(&mut Ui) -> R) -> InnerResponse<R> {
        ui.allocate_ui_with_layout(vec2(ui.available_width(), 30.0), egui::Layout::right_to_left(egui::Align::Center), contents)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watch_text_does_not_blink_during_routine_checks() {
        let ctx = Context::default();
        let mut frames = Vec::new();
        for status in [
            json!({"running":false,"progress":null}),
            json!({"running":true,"progress":""}),
            json!({"running":true,"progress":"   "}),
            json!({"running":false,"progress":null}),
        ] {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                ui.set_width(580.0);
                let _ = progress_row(ui, &status);
            });
            output.textures_delta.clear();
            let text = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) => Some((text.galley.text().to_owned(), text.pos)),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(text.len(), 1);
            assert_eq!(text[0].0, "Watching for saved changes");
            frames.push(text);
        }
        assert!(frames.windows(2).all(|pair| pair[0] == pair[1]), "text and position must remain unchanged while checking");
        assert_eq!(rendering_message(&json!({"running":true,"progress":"Rendering title"})), Some("Rendering title"));
    }

    #[test]
    fn background_checks_keep_the_linked_item_box_in_place() {
        let ctx = Context::default();
        let mut positions = Vec::new();
        for status in [
            json!({"running":false,"progress":null}),
            json!({"running":true,"progress":"Checking / rendering a long composition name from an external project"}),
            json!({"running":false,"progress":null}),
        ] {
            let mut rect = egui::Rect::NOTHING;
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                ui.scope(|ui| {
                    ui.set_width(580.0);
                    let _ = progress_row(ui, &status);
                    rect = ui
                        .group(|ui| {
                            ui.label("One Year Later");
                            ui.button("Bake")
                        })
                        .response
                        .rect;
                });
            });
            output.textures_delta.clear();
            positions.push(rect);
        }
        assert_eq!(positions[0], positions[1], "a background check must not move or resize the linked-item box");
        assert_eq!(positions[0], positions[2]);
    }
}
