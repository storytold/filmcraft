//! Multi-camera UI: the Program monitor's Multi-Camera view (angle grid next to the program) and
//! the Synchronize, Merge Clips and Create Multi-Camera Source Sequence dialogs.
//!
//! The grid is one frame job ([`Target::MulticamGrid`]): every shown angle rendered at the grid
//! cell's size (reduced resolution) and tiled, prefetched like the program while playing.
//! Clicking an angle (or keys 1–9) cuts to it while recording (playing with Multi-Camera Record
//! on) and switches the clip at the playhead when stopped; Ctrl/⌘-click switches the video only.
//!
//! Automation ids:
//! - Multi-Camera view: `program.multicam.grid`, `program.multicam.angle.<n>` (n = 1-based camera
//!   in shown order, so cameras on page 2 of a 4×4 grid are 17…), `program.multicam.record`,
//!   `program.multicam.pagePrev`, `program.multicam.pageNext` (when there is more than one page).
//! - Edit Cameras dialog (`multicam.editCamerasDialog`): `editCameras.name.<angle>`,
//!   `editCameras.enabled.<angle>`, `editCameras.thumb.<angle>`, `editCameras.ok`, `editCameras.cancel`.
//! - Synchronize: `sync.method.<in|out|timecode|marker|audio>`, `sync.ignoreHours`, `sync.marker`,
//!   `sync.offset`, `sync.track`, `sync.ok`, `sync.cancel`.
//! - Merge Clips: `merge.name`, `merge.method.<…>`, `merge.ignoreHours`, `merge.marker`,
//!   `merge.offset`, `merge.removeVideoAudio`, `merge.ok`, `merge.cancel`.
//! - Create Multi-Camera Source Sequence: `mcam.name`, `mcam.method.<…>`, `mcam.ignoreHours`,
//!   `mcam.marker`, `mcam.offset`, `mcam.audio.<camera1|all|switch>`,
//!   `mcam.cameraNames.<clip|track|metadata>`, `mcam.processedBin`, `mcam.ok`, `mcam.cancel`.

use egui::{Align2, Color32, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};
use filmcraft_project::ItemId;
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::frames::{FrameKey, Target};
use crate::state::SyncDraft;
use crate::theme::Tokens;

type Elems = Vec<(String, Rect, String)>;

/// Menu / shortcut entry points: the three dialogs (without params) and the view toggles.
pub fn route(app: &mut FilmcraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    match id {
        "multicam.toggleView" => {
            app.ui.program.multicam = params.get("enabled").and_then(Value::as_bool).unwrap_or(!app.ui.program.multicam);
            return Some(Ok(json!({"multicam": app.ui.program.multicam})));
        }
        "multicam.editCamerasDialog" => {
            let Some(src) = filmcraft_engine::multicam::multicam_clip_at(&app.session, app.session.playhead()).map(|c| c.1.item) else {
                return Some(Err("no multi-camera clip at the playhead".into()));
            };
            let cams = app.session.project.sequence(src).map(|q| q.cameras()).unwrap_or_default();
            app.ui.edit_cameras = Some(crate::state::EditCamerasDraft {
                sequence: src.0,
                names: cams.cameras.iter().map(|c| c.name.clone()).collect(),
                enabled: cams.cameras.iter().map(|c| c.enabled).collect(),
            });
            return Some(Ok(json!({"dialog": "editCameras", "sequence": src.0})));
        }
        "multicam.recordToggle" => {
            app.ui.multicam_record = params.get("enabled").and_then(Value::as_bool).unwrap_or(!app.ui.multicam_record);
            app.ui.status = if app.ui.multicam_record { tl!("Multi-Camera Record on") } else { tl!("Multi-Camera Record off") }.to_string();
            return Some(Ok(json!({"record": app.ui.multicam_record})));
        }
        _ => {}
    }
    if !params.as_object().is_none_or(|m| m.is_empty()) {
        return None;
    }
    let kind = match id {
        "clip.synchronize" => "synchronize",
        "clip.mergeClips" => "merge",
        "clip.createMulticam" => "multicam",
        _ => return None,
    };
    if let Err(e) = filmcraft_engine::find_command(id).map_or(Ok(()), |c| (c.enabled)(&app.session)) {
        return Some(Err(e));
    }
    let items: Vec<u64> = app.session.state.project_selection.iter().map(|i| i.0).collect();
    let first = app.session.state.project_selection.first().and_then(|i| app.session.project.item(*i)).map(|i| i.name.clone()).unwrap_or_default();
    let name = match kind {
        "merge" => tlf!("{first} - Merged", first),
        "multicam" => tlf!("{first} Multicam", first),
        _ => String::new(),
    };
    app.ui.sync_dialog = Some(SyncDraft { kind: kind.into(), items, name, ..Default::default() });
    Some(Ok(json!({"dialog": kind})))
}

fn push(elems: &mut Elems, id: impl Into<String>, r: &egui::Response, label: impl Into<String>) {
    elems.push((id.into(), r.rect, label.into()));
}

/// Synchronize Point choices: (key, label).
fn methods() -> [(&'static str, &'static str); 5] {
    [("in", tl!("In Points")), ("out", tl!("Out Points")), ("timecode", tl!("Timecode")), ("marker", tl!("Clip Marker")), ("audio", tl!("Audio"))]
}

/// Draw the open Synchronize / Merge Clips / Create Multi-Camera dialog.
pub fn show_dialog(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut d) = app.ui.sync_dialog.clone() else { return };
    let (title, pre) = match d.kind.as_str() {
        "merge" => (tl!("Merge Clips"), "merge"),
        "multicam" => (tl!("Create Multi-Camera Source Sequence"), "mcam"),
        _ => (tl!("Synchronize Clips"), "sync"),
    };
    let mut elems: Elems = Vec::new();
    let mut action: Option<&str> = None;
    let names: Vec<String> = d.items.iter().filter_map(|i| app.session.project.item(ItemId(*i)).map(|it| it.name.clone())).collect();
    crate::dialog_style::Window::new(title)
        .id(egui::Id::new(("sync-dialog", pre)))
        .collapsible(false)
        .resizable(false)
        .default_width(440.0)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            if pre != "sync" {
                ui.horizontal(|ui| {
                    ui.label(if pre == "merge" { tl!("Clip Name:") } else { tl!("Sequence Name:") });
                    let r = ui.add(egui::TextEdit::singleline(&mut d.name).desired_width(300.0));
                    push(&mut elems, format!("{pre}.name"), &r, "name");
                });
                ui.label(RichText::new(tlf!("{n} clip(s): {names}", n = names.len(), names = names.join(", "))).weak().small());
                ui.add_space(4.0);
            } else {
                let n = app.session.state.selection.len();
                ui.label(RichText::new(tlf!("{n} clip(s) selected; the clip on the reference track stays put.", n)).weak().small());
            }
            ui.group(|ui| {
                ui.set_min_width(400.0);
                ui.label(RichText::new(tl!("Synchronize Point")).strong());
                for (m, label) in methods() {
                    ui.horizontal(|ui| {
                        let r = ui.radio(d.method == m, label);
                        push(&mut elems, format!("{pre}.method.{m}"), &r, label);
                        if r.clicked() {
                            d.method = m.into();
                        }
                        match m {
                            "timecode" => {
                                let r = ui.add_enabled(d.method == "timecode", egui::Checkbox::new(&mut d.ignore_hours, tl!("Ignore Hours")));
                                push(&mut elems, format!("{pre}.ignoreHours"), &r, "Ignore Hours");
                            }
                            "marker" => {
                                let r = ui.add_enabled(
                                    d.method == "marker",
                                    egui::TextEdit::singleline(&mut d.marker).hint_text(tl!("first marker")).desired_width(140.0),
                                );
                                push(&mut elems, format!("{pre}.marker"), &r, "marker name");
                            }
                            _ => {}
                        }
                    });
                }
                ui.horizontal(|ui| {
                    ui.label(if pre == "merge" { tl!("Offset Audio by:") } else { tl!("Offset:") });
                    let r = ui.add(egui::DragValue::new(&mut d.offset).range(-100_000..=100_000).suffix(tl!(" frames")));
                    push(&mut elems, format!("{pre}.offset"), &r, "offset");
                });
            });
            match pre {
                "sync" => {
                    ui.horizontal(|ui| {
                        ui.label(tl!("Reference track:"));
                        let r = ui.add(egui::TextEdit::singleline(&mut d.track).hint_text(tl!("lowest (V1…)")).desired_width(90.0));
                        push(&mut elems, "sync.track", &r, "reference track");
                    });
                }
                "merge" => {
                    let r = ui.checkbox(&mut d.remove_video_audio, tl!("Remove Audio from AV Clip"));
                    push(&mut elems, "merge.removeVideoAudio", &r, "Remove Audio from AV Clip");
                }
                _ => {
                    ui.group(|ui| {
                        ui.set_min_width(400.0);
                        ui.label(RichText::new(tl!("Audio")).strong());
                        ui.horizontal(|ui| {
                            for (a, label) in [("camera1", tl!("Camera 1")), ("all", tl!("All Cameras")), ("switch", tl!("Switch Audio"))] {
                                let r = ui.radio(d.audio == a, label);
                                push(&mut elems, format!("mcam.audio.{a}"), &r, label);
                                if r.clicked() {
                                    d.audio = a.into();
                                }
                            }
                        });
                        ui.label(RichText::new(tl!("Camera Names")).strong());
                        ui.horizontal(|ui| {
                            for (a, label) in [("clip", tl!("Clip Names")), ("track", tl!("Enumerate Cameras")), ("metadata", tl!("Camera Angle Metadata"))] {
                                let r = ui.radio(d.camera_names == a, label);
                                push(&mut elems, format!("mcam.cameraNames.{a}"), &r, label);
                                if r.clicked() {
                                    d.camera_names = a.into();
                                }
                            }
                        });
                    });
                    let r = ui.checkbox(&mut d.processed_bin, tl!("Move source clips to Processed Clips bin"));
                    push(&mut elems, "mcam.processedBin", &r, "Move source clips to Processed Clips bin");
                }
            }
            if !d.message.is_empty() {
                ui.colored_label(Color32::from_rgb(0xe0, 0x8a, 0x6a), &d.message);
            }
            ui.add_space(6.0);
            crate::dialog_style::actions(ui, |ui| {
                let r = ui.add(crate::dialog_style::secondary(tl!("Cancel")));
                push(&mut elems, format!("{pre}.cancel"), &r, "Cancel");
                if r.clicked() {
                    action = Some("cancel");
                }
                let r = ui.add(crate::dialog_style::primary(tl!("OK")));
                push(&mut elems, format!("{pre}.ok"), &r, "OK");
                if r.clicked() {
                    action = Some("ok");
                }
            });
        });
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        action = Some("cancel");
    }
    match action {
        Some("cancel") => {
            app.ui.sync_dialog = None;
            return;
        }
        Some("ok") => {
            let mut p = json!({"method": d.method, "offset": d.offset, "ignoreHours": d.ignore_hours});
            if !d.marker.is_empty() {
                p["marker"] = json!(d.marker);
            }
            let cmd = match pre {
                "merge" => {
                    p["items"] = json!(d.items);
                    p["removeVideoAudio"] = json!(d.remove_video_audio);
                    if !d.name.is_empty() {
                        p["name"] = json!(d.name);
                    }
                    "clip.mergeClips"
                }
                "mcam" => {
                    p["items"] = json!(d.items);
                    p["audio"] = json!(d.audio);
                    p["cameraNames"] = json!(d.camera_names);
                    p["processedBin"] = json!(d.processed_bin);
                    if !d.name.is_empty() {
                        p["name"] = json!(d.name);
                    }
                    "clip.createMulticam"
                }
                _ => {
                    if !d.track.is_empty() {
                        p["track"] = json!(d.track);
                    }
                    "clip.synchronize"
                }
            };
            match app.session.execute(cmd, p) {
                Ok(v) => {
                    app.ui.status = match pre {
                        "sync" => tlf!("Synchronized {n} clip group(s)", n = v["moved"]),
                        "merge" => tl!("Merged clip created").into(),
                        _ => tlf!("Multi-camera source sequence with {n} camera(s)", n = v["cameras"].as_array().map_or(0, Vec::len)),
                    };
                    app.ui.sync_dialog = None;
                    return;
                }
                Err(e) => d.message = e.to_string(),
            }
        }
        _ => {}
    }
    app.ui.sync_dialog = Some(d);
}

/// A revision for grid frames: changes when the multi-camera source (or its cameras' media)
/// changes, not with every edit of the cut (cuts recorded live must not throw away the grid).
fn grid_revision(app: &FilmcraftApp, src: ItemId) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    if let Some(q) = app.session.project.sequence(src) {
        serde_json::to_vec(q).unwrap_or_default().hash(&mut h);
        for t in &q.video_tracks {
            for it in &t.items {
                app.item_revision(it.item).hash(&mut h);
            }
        }
    }
    h.finish()
}

/// The angle grid of the Multi-Camera view in `area`.
pub fn grid(app: &mut FilmcraftApp, ui: &mut egui::Ui, area: Rect) {
    let t = app.tokens;
    let ctx = ui.ctx().clone();
    ui.painter().rect_filled(area, 0.0, t.panel_bg);
    app.auto.add("program.multicam.grid", area, "Multi-Camera");
    let playhead = app.session.playhead();
    let info = filmcraft_engine::multicam::inspect_at(&app.session, playhead);
    let Some(src) = info["source"].as_u64().map(ItemId) else {
        ui.painter().text(area.center(), Align2::CENTER_CENTER, tl!("No multi-camera clip at the playhead"), Tokens::ui(12.0), t.text_dim);
        return;
    };
    let Some(q) = app.session.project.sequence(src) else { return };
    let cams = q.cameras();
    let shown = cams.shown_angles();
    let view = app.session.state.multicam_view.clone();
    let lay = view.page_layout(shown.len());
    let (cols, rows) = (lay.cols, lay.rows);
    let (sw, sh) = (q.settings.width as f32, q.settings.height as f32);
    let rate = q.settings.frame_rate;
    // page arrows under the grid when the angles don't fit on one page
    let paged = lay.pages > 1;
    let grid_area = if paged { Rect::from_min_max(area.min, pos2(area.max.x, area.max.y - 24.0)) } else { area };
    let pic = crate::panels::monitor::fit(grid_area.shrink(2.0), sw * cols as f32, sh * rows as f32);
    let (cw, ch) = (pic.width() / cols as f32, pic.height() / rows as f32);
    // decode and composite at the cell's size (never more than the playback resolution; lower
    // while playing with Auto-Adjust Multi-Camera Playback Quality)
    let cell_px = cw * ctx.pixels_per_point();
    let scale = crate::panels::monitor::quantize_scale(filmcraft_render::multicam::grid_cell_scale(
        q.settings.width,
        cell_px,
        app.ui.program.res.scale(),
        app.playback.playing,
        view.auto_quality,
        lay.per_page,
    ));
    let mt = Tick(info["sourceTime"].as_i64().unwrap_or(0));
    let frame = rate.frame_at(mt);
    let target = Target::MulticamGrid(src, view.layout.unwrap_or(0) as u8, lay.page as u16);
    let key = FrameKey { target, frame, size: (scale * 1000.0) as u32, revision: grid_revision(app, src), draft: false };
    let project = app.session.project.clone();
    if app.playback.playing {
        app.frames.schedule_playback(key, rate, scale, &project, app.playback.speed, app.playback.preroll.is_some());
    } else {
        app.frames.request(key, rate.tick_of(frame), scale, &project, 0);
    }
    let tex = match app.frames.get(&key) {
        Some(img) => Some(app.texture_for(&ctx, "monitor-multicam", key, &img)),
        None => match app.frames.nearest(key.target, frame, key.size, key.revision, 6) {
            Some(img) => Some(app.texture_for(&ctx, "monitor-multicam", FrameKey { frame: frame - 1, ..key }, &img)),
            None => app.texture_existing("monitor-multicam").map(|(id, _)| id),
        },
    };
    ui.painter().rect_filled(pic, 0.0, t.monitor_bg);
    if let Some(tex) = tex {
        ui.painter().image(tex, pic, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    }
    let current = info["angle"].as_u64().map(|a| a as usize);
    let recording = app.session.mcrec.active();
    let mut click: Option<(usize, bool)> = None;
    for (k, &angle) in shown.iter().enumerate().skip(lay.first).take(lay.count) {
        let cell_i = k - lay.first;
        let cell = Rect::from_min_size(pic.min + vec2((cell_i % cols) as f32 * cw, (cell_i / cols) as f32 * ch), vec2(cw, ch));
        let name = cams.cameras.get(angle).map(|c| c.name.clone()).unwrap_or_default();
        let resp = ui.interact(cell, egui::Id::new(("multicam-angle", k)), Sense::click()).on_hover_text(tlf!("Camera {n} — {name}", n = k + 1, name));
        app.auto.add(&format!("program.multicam.angle.{}", k + 1), cell, &name);
        if resp.clicked() {
            // modifiers held now, or carried by the click event (synthetic input)
            let video_only = ui.input(|i| {
                let ev = i.events.iter().any(|e| matches!(e, egui::Event::PointerButton { modifiers: m, .. } if m.ctrl || m.command));
                i.modifiers.ctrl || i.modifiers.command || ev
            });
            click = Some((k + 1, video_only));
        }
        if resp.hovered() {
            ui.painter().rect_stroke(cell.shrink(1.0), 0.0, Stroke::new(1.0, Color32::from_white_alpha(90)), StrokeKind::Inside);
        }
        if current == Some(angle) {
            // the active angle: yellow, red while recording
            let col = if recording { Color32::from_rgb(0xe0, 0x3a, 0x3a) } else { Color32::from_rgb(0xf2, 0xc9, 0x4c) };
            ui.painter().rect_stroke(cell.shrink(1.5), 0.0, Stroke::new(3.0, col), StrokeKind::Inside);
        }
        let label = format!("{}  {name}", k + 1);
        let galley = ui.painter().layout_no_wrap(label, Tokens::ui(11.0), Color32::WHITE);
        let lr = Rect::from_min_size(cell.left_bottom() + vec2(6.0, -galley.size().y - 8.0), galley.size() + vec2(8.0, 4.0));
        ui.painter().rect_filled(lr, 3.0, Color32::from_black_alpha(160));
        ui.painter().galley(lr.min + vec2(4.0, 2.0), galley, Color32::WHITE);
    }
    if paged {
        let bar = Rect::from_min_max(pos2(area.min.x, area.max.y - 22.0), area.max);
        let mid = bar.center();
        ui.painter().text(mid, Align2::CENTER_CENTER, tlf!("Page {page} of {pages}", page = lay.page + 1, pages = lay.pages), Tokens::ui(11.0), t.text_dim);
        for (id, label, dx, cmd) in [("pagePrev", "◀", -70.0, "multicam.prevPage"), ("pageNext", "▶", 70.0, "multicam.nextPage")] {
            let r = Rect::from_center_size(mid + vec2(dx, 0.0), vec2(26.0, 18.0));
            let enabled = if cmd == "multicam.prevPage" { lay.page > 0 } else { lay.page + 1 < lay.pages };
            let resp = ui.interact(r, egui::Id::new(("multicam-page", id)), Sense::click()).on_hover_text(if dx < 0.0 {
                tl!("Previous page")
            } else {
                tl!("Next page")
            });
            ui.painter().rect_filled(r, 3.0, if resp.hovered() && enabled { t.hover } else { t.field_bg });
            ui.painter().text(r.center(), Align2::CENTER_CENTER, label, Tokens::ui(10.0), if enabled { t.text } else { t.text_faint });
            app.auto.add(&format!("program.multicam.{id}"), r, label);
            if resp.clicked() && enabled {
                let _ = app.session.execute(cmd, json!({}));
            }
        }
    }
    // record indicator
    let rr = Rect::from_min_size(area.min + vec2(8.0, 8.0), vec2(64.0, 18.0));
    let rec_on = app.ui.multicam_record;
    let resp = ui.interact(rr, egui::Id::new("multicam-record"), Sense::click()).on_hover_text(tl!("Multi-Camera Record On/Off (0)"));
    app.auto.add("program.multicam.record", rr, "Multi-Camera Record");
    let dot = if recording {
        Color32::from_rgb(0xe0, 0x3a, 0x3a)
    } else if rec_on {
        Color32::from_rgb(0x9a, 0x3a, 0x3a)
    } else {
        t.text_faint
    };
    ui.painter().rect_filled(rr, 3.0, Color32::from_black_alpha(140));
    ui.painter().circle_filled(rr.left_center() + vec2(9.0, 0.0), 4.5, dot);
    ui.painter().text(rr.left_center() + vec2(18.0, 0.0), Align2::LEFT_CENTER, if recording { "REC" } else { tl!("Record") }, Tokens::ui(10.5), Color32::WHITE);
    if resp.clicked() {
        app.ui.multicam_record = !app.ui.multicam_record;
    }
    if let Some((n, video_only)) = click
        && let Err(e) = app.session.execute("multicam.cut", json!({"camera": n, "videoOnly": video_only}))
    {
        app.ui.status = e.to_string();
    }
}

/// The Edit Cameras dialog: a thumbnail, name and on/off switch per camera (`multicam.editCameras`).
pub fn show_edit_cameras(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut d) = app.ui.edit_cameras.clone() else { return };
    let src = ItemId(d.sequence);
    let Some(q) = app.session.project.sequence(src).cloned() else {
        app.ui.edit_cameras = None;
        return;
    };
    let rate = q.settings.frame_rate;
    let mt = filmcraft_engine::multicam::inspect_at(&app.session, app.session.playhead())["sourceTime"].as_i64().unwrap_or(0);
    let frame = rate.frame_at(Tick(mt));
    let rev = grid_revision(app, src);
    let mut elems: Elems = Vec::new();
    let mut action: Option<&str> = None;
    let thumb_w = 96.0;
    let scale = crate::panels::monitor::quantize_scale((thumb_w * ctx.pixels_per_point() / q.settings.width.max(1) as f32).clamp(1.0 / 32.0, 1.0));
    let project = app.session.project.clone();
    let mut thumbs = Vec::new();
    for a in 0..d.names.len() {
        let has_video = q.angle_video_track_index(a).is_some();
        let key = FrameKey { target: Target::MulticamAngle(src, a as u32), frame, size: (scale * 1000.0) as u32, revision: rev, draft: false };
        let tex = if has_video {
            app.frames.request(key, rate.tick_of(frame), scale, &project, 2);
            app.frames.get(&key).map(|img| app.texture_for(ctx, &format!("edit-cameras-{}-{a}", src.0), key, &img))
        } else {
            None
        };
        thumbs.push((has_video, tex));
    }
    let th = thumb_w * q.settings.height.max(1) as f32 / q.settings.width.max(1) as f32;
    let tokens = app.tokens;
    crate::dialog_style::Window::new(tl!("Edit Cameras"))
        .id(egui::Id::new("Edit Cameras"))
        .collapsible(false)
        .resizable(false)
        .default_width(420.0)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(RichText::new(tl!("Cameras shown in the Multi-Camera view (uncheck to hide).")).weak().small());
            egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                for a in 0..d.names.len() {
                    ui.horizontal(|ui| {
                        let r = ui.checkbox(&mut d.enabled[a], "");
                        push(&mut elems, format!("editCameras.enabled.{a}"), &r, "Enabled");
                        let (tr, _) = ui.allocate_exact_size(vec2(thumb_w, th), Sense::hover());
                        ui.painter().rect_filled(tr, 2.0, tokens.monitor_bg);
                        match thumbs[a] {
                            (_, Some(tex)) => {
                                ui.painter().image(tex, tr, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                            }
                            (false, None) => {
                                ui.painter().text(tr.center(), Align2::CENTER_CENTER, tl!("Audio"), Tokens::ui(10.0), tokens.text_dim);
                            }
                            _ => {}
                        }
                        elems.push((format!("editCameras.thumb.{a}"), tr, format!("Camera {}", a + 1)));
                        ui.label(format!("{}", a + 1));
                        let r = ui.add(egui::TextEdit::singleline(&mut d.names[a]).desired_width(180.0));
                        push(&mut elems, format!("editCameras.name.{a}"), &r, "Camera name");
                    });
                }
            });
            ui.add_space(6.0);
            crate::dialog_style::actions(ui, |ui| {
                let r = ui.add(crate::dialog_style::secondary(tl!("Cancel")));
                push(&mut elems, "editCameras.cancel", &r, "Cancel");
                if r.clicked() {
                    action = Some("cancel");
                }
                let r = ui.add(crate::dialog_style::primary(tl!("OK")));
                push(&mut elems, "editCameras.ok", &r, "OK");
                if r.clicked() {
                    action = Some("ok");
                }
            });
        });
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        action = Some("cancel");
    }
    match action {
        Some("cancel") => app.ui.edit_cameras = None,
        Some("ok") => {
            let cameras: Vec<Value> = (0..d.names.len()).map(|a| json!({"angle": a, "name": d.names[a], "enabled": d.enabled[a]})).collect();
            match app.session.execute("multicam.editCameras", json!({"sequence": src.0, "cameras": cameras})) {
                Ok(_) => app.ui.edit_cameras = None,
                Err(e) => {
                    app.ui.status = e.to_string();
                    app.ui.edit_cameras = Some(d);
                }
            }
        }
        _ => app.ui.edit_cameras = Some(d),
    }
}

/// Start a recording pass when playback starts in the Multi-Camera view.
pub fn on_play(app: &mut FilmcraftApp) {
    if app.ui.program.multicam && app.ui.multicam_record && !app.session.mcrec.active() && (app.playback.speed - 1.0).abs() < 1e-9 {
        let t = app.session.playhead();
        if filmcraft_engine::multicam::multicam_clip_at(&app.session, t).is_some() {
            let _ = app.session.execute("multicam.recordStart", json!({"time": t.0}));
        }
    }
}

/// End the recording pass (one undo step) when playback stops.
pub fn on_stop(app: &mut FilmcraftApp) {
    if app.session.mcrec.active() {
        let t = app.session.playhead();
        match app.session.execute("multicam.recordStop", json!({"time": t.0})) {
            Ok(v) if v["cuts"].as_u64().unwrap_or(0) > 0 => app.ui.status = tlf!("Recorded {n} multi-camera cut(s)", n = v["cuts"]),
            Ok(_) => {}
            Err(e) => app.ui.status = e.to_string(),
        }
    }
}
