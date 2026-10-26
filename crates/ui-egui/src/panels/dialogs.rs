//! Modal dialogs (About; Keyboard Shortcuts in `shortcuts_dialog`; Settings in `settings`;
//! Recovery/Revert in `file_dialogs`).

use crate::{Dialog, FilmcraftApp, PendingClipMismatch};
use serde_json::{Value, json};

/// Route the first picture edit through the mismatch warning. The pending command and its exact
/// parameters are retained until the user chooses; cancelling leaves the project untouched.
pub(crate) fn place_with_mismatch(app: &mut FilmcraftApp, command: &str, params: Value) -> Result<Value, String> {
    let item = params
        .get("item")
        .and_then(Value::as_u64)
        .or_else(|| params.get("items").and_then(Value::as_array).and_then(|a| a.first()).and_then(Value::as_u64))
        .or_else(|| (command == "source.insert" || command == "source.overwrite").then_some(app.session.state.source_item).flatten().map(|i| i.0));
    let placing_picture = params.get("video").and_then(Value::as_bool) != Some(false)
        && !params.get("track").and_then(Value::as_str).is_some_and(|t| t.starts_with('A'))
        && (command == "timeline.place" || app.session.targeting().video_dest.is_some());
    if app.session.prefs.timeline.clip_mismatch_warning
        && placing_picture
        && let Some(item) = item
    {
        let check = app.session.execute("sequence.clipMismatch", json!({"item": item})).map_err(|e| e.to_string())?;
        if check["mismatch"] == true {
            app.pending_clip_mismatch =
                Some(PendingClipMismatch { command: command.into(), params, item: filmcraft_project::ItemId(item), settings: check["settings"].clone() });
            app.dialog = Some(Dialog::ClipMismatch);
            return Ok(json!({"dialog": "clipMismatch"}));
        }
    }
    app.session.execute(command, params).map_err(|e| e.to_string())
}

pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    crate::panels::media_dialogs::show(app, ctx);
    crate::panels::color_dialogs::show(app, ctx);
    crate::panels::clip_dialogs::show(app, ctx);
    crate::panels::menu_dialogs::show(app, ctx);
    crate::panels::graphics_templates::show(app, ctx);
    crate::panels::presets::save_dialog(app, ctx);
    crate::panels::audio_fx_editor::show(app, ctx);
    crate::panels::multicam::show_dialog(app, ctx);
    crate::panels::multicam::show_edit_cameras(app, ctx);
    crate::panels::monitor_view::dialogs(app, ctx);
    crate::panels::graphics::dialogs(app, ctx);
    crate::panels::workspaces::dialogs(app, ctx);
    crate::panels::voiceover::show(app, ctx);
    crate::panels::remix::show(app, ctx);
    crate::links::show_failed(app, ctx);
    crate::panels::interchange_export::show(app, ctx);
    crate::panels::frame_export::show(app, ctx);
    crate::panels::project_dialogs::show(app, ctx);
    crate::panels::media_browser::dialogs(app, ctx);
    crate::panels::misc::history_dialogs(app, ctx);
    track_dialogs(app, ctx);
    let Some(d) = app.dialog else { return };
    if let Some(still_open) = crate::panels::file_dialogs::show(app, ctx, d) {
        if !still_open && app.dialog == Some(d) {
            app.dialog = None;
        }
        return;
    }
    let mut open = true;
    match d {
        Dialog::About => {
            egui::Window::new(tl!("About FilmCraft"))
                .id(egui::Id::unique("About FilmCraft"))
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .pivot(egui::Align2::CENTER_CENTER)
                .default_pos(crate::widgets::dialog_center(ctx))
                .show(ctx, |ui| about(app, ui));
            if crate::widgets::escape_closes(ctx) {
                open = false;
            }
        }
        Dialog::Shortcuts => {
            if !crate::panels::shortcuts_dialog::show(app, ctx) && app.dialog == Some(d) {
                app.dialog = None;
            }
            return;
        }
        Dialog::AudioGain => {
            if !audio_gain(app, ctx) {
                app.dialog = None;
            }
            return;
        }
        Dialog::DeleteTracks => {
            if !delete_tracks(app, ctx) {
                app.dialog = None;
            }
            return;
        }
        Dialog::AddTracks => {
            if !add_tracks(app, ctx) {
                app.dialog = None;
            }
            return;
        }
        Dialog::TransitionDuration => {
            if !crate::panels::transition_controls::duration_dialog(app, ctx) && app.dialog == Some(d) {
                app.dialog = None;
            }
            return;
        }
        Dialog::SequenceSettings => {
            if !crate::panels::sequence_settings::show(app, ctx) && app.dialog == Some(d) {
                app.dialog = None;
            }
            return;
        }
        Dialog::ClipMismatch => {
            let Some(pending) = app.pending_clip_mismatch.clone() else {
                app.dialog = None;
                return;
            };
            let mut choice = None;
            egui::Window::new(tl!("Clip Mismatch Warning"))
                .id(egui::Id::unique("clip-mismatch-warning"))
                .collapsible(false)
                .resizable(false)
                .default_width(400.0)
                .pivot(egui::Align2::CENTER_CENTER)
                .default_pos(crate::widgets::dialog_center(ctx))
                .show(ctx, |ui| {
                    ui.label(tl!("This clip does not match the sequence's settings. Change the sequence to match the clip's settings?"));
                    ui.label(format!(
                        "{} × {}  •  {:.3} fps",
                        pending.settings["width"].as_u64().unwrap_or(0),
                        pending.settings["height"].as_u64().unwrap_or(0),
                        pending.settings["fps"].as_f64().unwrap_or(0.0)
                    ));
                    ui.horizontal(|ui| {
                        let change = ui.button(tl!("Change Sequence Settings"));
                        app.auto.add("clipMismatch.change", change.rect, "Change Sequence Settings");
                        if change.clicked() {
                            choice = Some(true);
                        }
                        let keep = ui.button(tl!("Keep Existing Settings"));
                        app.auto.add("clipMismatch.keep", keep.rect, "Keep Existing Settings");
                        if keep.clicked() {
                            choice = Some(false);
                        }
                    });
                });
            if let Some(change) = choice {
                app.dialog = None;
                app.pending_clip_mismatch = None;
                let result = if change {
                    app.session
                        .edit_group("Match Sequence and Place Clip", |s| {
                            s.execute("sequence.matchClip", json!({"item": pending.item.0}))?;
                            s.execute(&pending.command, pending.params)
                        })
                        .map_err(|e| e.to_string())
                } else {
                    app.session.execute(&pending.command, pending.params).map_err(|e| e.to_string())
                };
                if let Err(e) = result {
                    app.ui.status = e;
                }
            } else if crate::widgets::escape_closes(ctx) {
                app.dialog = None;
                app.pending_clip_mismatch = None;
            }
            return;
        }
        Dialog::NewSequence | Dialog::Preferences | Dialog::Recovery | Dialog::RevertConfirm => {}
    }
    if !open {
        app.dialog = None;
    }
}

/// Clip ▸ Audio Gain…: Set Gain to / Adjust Gain by / Normalize Max Peak to / Normalize All Peaks to,
/// with the selection's peak amplitude. Returns whether the dialog stays open.
///
/// Automation ids: `audioGain.<set|adjust|normalizeMax|normalizeAll>` (radio buttons),
/// `audioGain.<mode>.value` (dB fields), `audioGain.peak`, `audioGain.ok`, `audioGain.cancel`.
fn audio_gain(app: &mut FilmcraftApp, ctx: &egui::Context) -> bool {
    let peak_id = egui::Id::unique("audio-gain-peak");
    let rev = app.session.revision;
    let cached: Option<(u64, Option<f64>)> = ctx.data(|d| d.get_temp(peak_id));
    let peak = match cached {
        Some((r, p)) if r == rev => p,
        _ => {
            let p = app.session.execute("clip.audioPeak", serde_json::json!({})).ok().and_then(|v| v["peakDb"].as_f64());
            ctx.data_mut(|d| d.insert_temp(peak_id, (rev, p)));
            p
        }
    };
    let mut draft = app.ui.audio_gain.clone();
    crate::widgets::revert_drag_on_escape(ctx, egui::Id::unique("audio-gain-before-drag"), &mut draft);
    let mut keep = true;
    let mut apply = false;
    let mut elems: Vec<(String, egui::Rect, String)> = Vec::new();
    egui::Window::new(tl!("Audio Gain"))
        .id(egui::Id::unique("Audio Gain"))
        .collapsible(false)
        .resizable(false)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(crate::widgets::dialog_center(ctx))
        .show(ctx, |ui| {
            ui.add_space(6.0);
            egui::Grid::new("audio-gain").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
                let rows: [(&str, &str, &mut f64); 4] = [
                    ("set", tl!("Set Gain to:"), &mut draft.set_db),
                    ("adjust", tl!("Adjust Gain by:"), &mut draft.adjust_db),
                    ("normalizeMax", tl!("Normalize Max Peak to:"), &mut draft.max_peak_db),
                    ("normalizeAll", tl!("Normalize All Peaks to:"), &mut draft.all_peaks_db),
                ];
                for (id, label, v) in rows {
                    let r = ui.radio(draft.mode == id, label);
                    elems.push((format!("audioGain.{id}"), r.rect, label.to_string()));
                    if r.clicked() {
                        draft.mode = id.to_string();
                    }
                    let f = ui.add_enabled(draft.mode == id, crate::widgets::number_drag(v).speed(0.1).range(-96.0..=96.0).suffix(" dB"));
                    elems.push((format!("audioGain.{id}.value"), f.rect, format!("{v:.1} dB")));
                    ui.end_row();
                }
            });
            ui.add_space(8.0);
            let pk_text = tlf!("Peak Amplitude: {peak}", peak = peak.map(|p| format!("{p:.1} dB")).unwrap_or_else(|| "—".into()));
            let pk = ui.label(&pk_text);
            elems.push(("audioGain.peak".into(), pk.rect, pk_text));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let c = ui.button(tl!("Cancel"));
                elems.push(("audioGain.cancel".into(), c.rect, "Cancel".into()));
                if c.clicked() {
                    keep = false;
                }
                let o = ui.add(egui::Button::new(egui::RichText::new(tl!("OK")).color(egui::Color32::WHITE)).fill(app.tokens.accent));
                elems.push(("audioGain.ok".into(), o.rect, "OK".into()));
                if o.clicked() {
                    apply = true;
                }
            });
        });
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    if crate::widgets::escape_closes(ctx) {
        keep = false;
    }
    if apply {
        let db = match draft.mode.as_str() {
            "set" => draft.set_db,
            "normalizeMax" => draft.max_peak_db,
            "normalizeAll" => draft.all_peaks_db,
            _ => draft.adjust_db,
        };
        if let Err(e) = app.session.execute("clip.audioGain", serde_json::json!({"mode": draft.mode, "db": db})) {
            app.ui.status = e.to_string();
        }
        keep = false;
    }
    app.ui.audio_gain = draft;
    keep
}

/// Sequence ▸ Add Tracks…, laid out as Premiere Pro's dialog: "Add video tracks" (Amount,
/// Placement), "Add audio tracks" (Amount, Placement, Track type: Standard, 5.1, Adaptive, Mono)
/// and "Add audio submix tracks" (Amount, Placement, Track type: Stereo, 5.1, Adaptive, Mono).
/// Placement is "Before First Track" or after one of the tracks; the submix placement is off
/// while the sequence has no submix track. Returns whether the dialog stays open.
///
/// Automation ids, with `<kind>` = `video`, `audio` or `submix`: `addTracks.<kind>.amount`,
/// `addTracks.<kind>.placement` and, while its list is open, `addTracks.<kind>.placement.option.<n>`
/// (n tracks before the new ones; 0 = Before First Track); `addTracks.<audio|submix>.type` and
/// `addTracks.<audio|submix>.type.option.<standard|stereo|5.1|adaptive|mono>`; `addTracks.ok`,
/// `addTracks.cancel`.
fn add_tracks(app: &mut FilmcraftApp, ctx: &egui::Context) -> bool {
    let Some(seq) = app.session.active_sequence() else { return false };
    let names = |tracks: &[filmcraft_engine::project::Track]| tracks.iter().map(|t| t.name.clone()).collect::<Vec<_>>();
    let (vnames, anames, snames) = (names(&seq.video_tracks), names(&seq.audio_tracks), names(&seq.submix_tracks));
    let mut d = app.ui.add_tracks.clone();
    crate::widgets::revert_drag_on_escape(ctx, egui::Id::unique("add-tracks-before-drag"), &mut d);
    let mut keep = true;
    let mut apply = false;
    let mut elems: Vec<(String, egui::Rect, String)> = Vec::new();
    let audio_types: [(&str, &str); 4] = [("standard", tl!("Standard")), ("5.1", "5.1"), ("adaptive", tl!("Adaptive")), ("mono", tl!("Mono"))];
    let submix_types: [(&str, &str); 4] = [("stereo", tl!("Stereo")), ("5.1", "5.1"), ("adaptive", tl!("Adaptive")), ("mono", tl!("Mono"))];
    egui::Window::new(tl!("Add Tracks"))
        .id(egui::Id::unique("Add Tracks"))
        .collapsible(false)
        .resizable(false)
        .default_width(320.0)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(crate::widgets::dialog_center(ctx))
        .show(ctx, |ui| {
            // everything spans the width the window has: no empty band beside the fields
            ui.set_width(ui.available_width());
            // the label column is as wide as its widest label; the lists take the rest of the row,
            // up to the group's right edge (as in Premiere's dialog)
            let font = egui::TextStyle::Body.resolve(ui.style());
            let label_w = [tl!("Amount"), tl!("Placement"), tl!("Track type")]
                .iter()
                .map(|l| ui.painter().layout_no_wrap(l.to_string(), font.clone(), egui::Color32::WHITE).size().x)
                .fold(0.0, f32::max);
            let row_h = ui.spacing().interact_size.y;
            let label = |ui: &mut egui::Ui, text: &str| {
                ui.allocate_ui_with_layout(egui::vec2(label_w, row_h), egui::Layout::right_to_left(egui::Align::Center), |ui| ui.label(text));
            };
            let groups: [(&str, &str, &mut u32, &mut usize, &[String], Option<(&mut String, &[(&str, &str); 4])>); 3] = [
                ("video", tl!("Add video tracks"), &mut d.video, &mut d.video_after, &vnames, None),
                ("audio", tl!("Add audio tracks"), &mut d.audio, &mut d.audio_after, &anames, Some((&mut d.audio_type, &audio_types))),
                ("submix", tl!("Add audio submix tracks"), &mut d.submix, &mut d.submix_after, &snames, Some((&mut d.submix_type, &submix_types))),
            ];
            for (kind, title, amount, after, tracks, track_type) in groups {
                ui.add_space(4.0);
                ui.label(egui::RichText::new(title).strong());
                ui.group(|ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        label(ui, tl!("Amount"));
                        let r = ui.add(crate::widgets::number_drag(amount).range(0..=99).speed(0.1));
                        elems.push((format!("addTracks.{kind}.amount"), r.rect, amount.to_string()));
                    });
                    ui.horizontal(|ui| {
                        label(ui, tl!("Placement"));
                        *after = (*after).min(tracks.len());
                        let place = |n: usize| match n.checked_sub(1).and_then(|i| tracks.get(i)) {
                            Some(name) => tlf!("After {name}", name),
                            None => tl!("Before First Track").to_string(),
                        };
                        let shown = place(*after);
                        let list_w = ui.available_width();
                        // nowhere to choose from while the sequence has no track of the kind
                        ui.add_enabled_ui(!tracks.is_empty(), |ui| {
                            let r = egui::ComboBox::from_id_salt(("add-tracks-placement", kind)).selected_text(&shown).width(list_w).show_ui(ui, |ui| {
                                for n in 0..=tracks.len() {
                                    let label = place(n);
                                    let o = ui.selectable_value(after, n, &label);
                                    elems.push((format!("addTracks.{kind}.placement.option.{n}"), o.rect, label));
                                }
                            });
                            elems.push((format!("addTracks.{kind}.placement"), r.response.rect, shown.clone()));
                        });
                    });
                    if let Some((chosen, types)) = track_type {
                        ui.horizontal(|ui| {
                            label(ui, tl!("Track type"));
                            let shown = types.iter().find(|(id, _)| *id == chosen.as_str()).map_or(types[0].1, |(_, label)| label).to_string();
                            let list_w = ui.available_width();
                            let r = egui::ComboBox::from_id_salt(("add-tracks-type", kind)).selected_text(&shown).width(list_w).show_ui(ui, |ui| {
                                for (id, label) in types {
                                    let o = ui.selectable_value(chosen, id.to_string(), *label);
                                    elems.push((format!("addTracks.{kind}.type.option.{id}"), o.rect, label.to_string()));
                                }
                            });
                            elems.push((format!("addTracks.{kind}.type"), r.response.rect, shown));
                        });
                    }
                });
            }
            ui.add_space(10.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let o = ui.add(egui::Button::new(egui::RichText::new(tl!("OK")).color(egui::Color32::WHITE)).fill(app.tokens.accent));
                elems.push(("addTracks.ok".into(), o.rect, "OK".into()));
                if o.clicked() {
                    apply = true;
                }
                let c = ui.button(tl!("Cancel"));
                elems.push(("addTracks.cancel".into(), c.rect, "Cancel".into()));
                if c.clicked() {
                    keep = false;
                }
            });
        });
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    if crate::widgets::escape_closes(ctx) {
        keep = false;
    }
    if ctx.input(|i| i.key_pressed(egui::Key::Enter)) && !ctx.egui_wants_keyboard_input() {
        apply = true;
    }
    if apply {
        // nothing asked for: OK closes the dialog and changes nothing
        if d.video > 0 || d.audio > 0 || d.submix > 0 {
            let params = serde_json::json!({
                "video": d.video, "videoAfter": d.video_after,
                "audio": d.audio, "audioAfter": d.audio_after, "audioType": d.audio_type,
                "submix": d.submix, "submixAfter": d.submix_after, "submixType": d.submix_type,
            });
            if let Err(e) = app.session.execute("sequence.addTracks", params) {
                app.ui.status = e.to_string();
            }
        }
        keep = false;
    }
    app.ui.add_tracks = d;
    keep
}

/// Sequence ▸ Delete Tracks…: "Delete Video Tracks" / "Delete Audio Tracks" (and, when the sequence
/// has caption tracks, "Delete Caption Tracks") checkboxes, each with a track choice (All Empty
/// Tracks or one track). Returns whether the dialog stays open.
///
/// Automation ids: `deleteTracks.video`, `deleteTracks.audio`, `deleteTracks.captions` (checkboxes),
/// `deleteTracks.<kind>.target` (track menus), `deleteTracks.<kind>.option.<empty|V1|A2|C1…>`
/// (menu entries while open), `deleteTracks.ok`, `deleteTracks.cancel`.
fn delete_tracks(app: &mut FilmcraftApp, ctx: &egui::Context) -> bool {
    let Some(seq) = app.session.active_sequence() else { return false };
    let names = |n: usize, p: &str| (1..=n).map(|i| format!("{p}{i}")).collect::<Vec<_>>();
    let vnames = names(seq.video_tracks.len(), "V");
    let anames = names(seq.audio_tracks.len(), "A");
    let cnames = names(seq.caption_tracks.len(), "C");
    let mut draft = app.ui.delete_tracks.clone();
    let mut keep = true;
    let mut apply = false;
    let mut elems: Vec<(String, egui::Rect, String)> = Vec::new();
    egui::Window::new(tl!("Delete Tracks"))
        .id(egui::Id::unique("Delete Tracks"))
        .collapsible(false)
        .resizable(false)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(crate::widgets::dialog_center(ctx))
        .show(ctx, |ui| {
            ui.set_min_width(300.0);
            for (kind, title, delete, on, target, list) in [
                ("video", tl!("Video Tracks"), tl!("Delete Video Tracks"), &mut draft.video, &mut draft.video_target, &vnames),
                ("audio", tl!("Audio Tracks"), tl!("Delete Audio Tracks"), &mut draft.audio, &mut draft.audio_target, &anames),
                ("captions", tl!("Caption Tracks"), tl!("Delete Caption Tracks"), &mut draft.captions, &mut draft.captions_target, &cnames),
            ] {
                if list.is_empty() {
                    continue;
                }
                ui.add_space(4.0);
                ui.label(egui::RichText::new(title).strong());
                let label = delete.to_string();
                let c = ui.checkbox(on, &label);
                elems.push((format!("deleteTracks.{kind}"), c.rect, label));
                ui.horizontal(|ui| {
                    ui.add_space(22.0);
                    let shown = if target == "empty" { tl!("All Empty Tracks").to_string() } else { target.clone() };
                    ui.add_enabled_ui(*on, |ui| {
                        let r = egui::ComboBox::from_id_salt(("delete-tracks", kind)).selected_text(&shown).width(170.0).show_ui(ui, |ui| {
                            let e = ui.selectable_value(target, "empty".to_string(), tl!("All Empty Tracks"));
                            elems.push((format!("deleteTracks.{kind}.option.empty"), e.rect, "All Empty Tracks".into()));
                            for n in list {
                                let r = ui.selectable_value(target, n.clone(), n);
                                elems.push((format!("deleteTracks.{kind}.option.{n}"), r.rect, n.clone()));
                            }
                        });
                        elems.push((format!("deleteTracks.{kind}.target"), r.response.rect, shown));
                    });
                });
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let c = ui.button(tl!("Cancel"));
                elems.push(("deleteTracks.cancel".into(), c.rect, "Cancel".into()));
                if c.clicked() {
                    keep = false;
                }
                let o = ui.add_enabled(
                    draft.video || draft.audio || (draft.captions && !cnames.is_empty()),
                    egui::Button::new(egui::RichText::new(tl!("OK")).color(egui::Color32::WHITE)).fill(app.tokens.accent),
                );
                elems.push(("deleteTracks.ok".into(), o.rect, "OK".into()));
                if o.clicked() {
                    apply = true;
                }
            });
        });
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    if crate::widgets::escape_closes(ctx) {
        keep = false;
    }
    if apply {
        let mut p = serde_json::Map::new();
        if draft.video {
            p.insert("video".into(), serde_json::json!(draft.video_target));
        }
        if draft.audio {
            p.insert("audio".into(), serde_json::json!(draft.audio_target));
        }
        if draft.captions && !cnames.is_empty() {
            p.insert("captions".into(), serde_json::json!(draft.captions_target));
        }
        if let Err(e) = app.session.execute("sequence.deleteTracks", serde_json::Value::Object(p)) {
            app.ui.status = e.to_string();
        }
        keep = false;
    }
    app.ui.delete_tracks = draft;
    keep
}

/// Help ▸ About FilmCraft, in three tabs: About (version and the ArtCraft community links),
/// Contributors and Models (the credits compiled in from `contributors/contributors.json`, see
/// `crate::credits` and docs/contributors.md).
///
/// Automation ids: `about.tab.about`, `about.tab.contributors`, `about.tab.models`; the About tab's
/// links and the credits controls are listed on [`about_tab`] and [`crate::credits::contributors_ui`].
fn about(app: &mut FilmcraftApp, ui: &mut egui::Ui) {
    let tab_id = egui::Id::unique("about_tab");
    let mut tab = ui.data_mut(|d| d.get_temp::<u8>(tab_id)).unwrap_or(0);
    ui.horizontal(|ui| {
        for (i, (id, label)) in [("about", tl!("About")), ("contributors", tl!("Contributors")), ("models", tl!("Models"))].into_iter().enumerate() {
            let i = i as u8;
            let r = ui.selectable_label(tab == i, label);
            app.auto.add(&format!("about.tab.{id}"), r.rect, label);
            if r.clicked() {
                tab = i;
            }
        }
    });
    ui.data_mut(|d| d.insert_temp(tab_id, tab));
    ui.separator();
    let t = app.tokens;
    match tab {
        1 => {
            ui.set_width(680.0);
            crate::credits::contributors_ui(ui, &t, &mut app.auto);
        }
        2 => {
            ui.set_width(680.0);
            crate::credits::models_ui(ui);
        }
        _ => about_tab(app, ui),
    }
}

/// About ▸ About: version, licence and the ArtCraft community links. Joining the Discord is the
/// first, accented button.
///
/// Automation ids: `about.discord`, `about.website`, `about.appPage`, `about.github`,
/// `about.reportIssue`.
fn about_tab(app: &mut FilmcraftApp, ui: &mut egui::Ui) {
    use crate::icons::{self, Icon};
    use crate::links;
    let t = app.tokens;
    ui.set_width(380.0);
    // ArtCraft wordmark (first-party trademark, docs/brand/).
    let (r, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 30.0), egui::Sense::hover());
    crate::brand::paint_wordmark(ui, egui::pos2(r.min.x, r.center().y), 20.0, app.ui.dark);
    ui.add_space(6.0);
    ui.heading("FilmCraft");
    ui.label(tlf!("Version {version}", version = env!("CARGO_PKG_VERSION")));
    ui.label(tl!("A clean-room, pure-Rust non-linear video editor. Part of the ArtCraft family."));
    ui.add_space(10.0);
    let mut link = |ui: &mut egui::Ui, id: &str, icon: Icon, label: &str, url: &str, primary: bool| {
        let size = egui::vec2(ui.available_width(), if primary { 36.0 } else { 28.0 });
        let (r, resp) = ui.allocate_exact_size(size, egui::Sense::click());
        let resp = resp.on_hover_text(url);
        app.auto.add(&format!("about.{id}"), r, label);
        let bg = if primary {
            if resp.hovered() { t.accent_hover } else { t.accent }
        } else if resp.hovered() {
            t.hover
        } else {
            egui::Color32::TRANSPARENT
        };
        ui.painter().rect_filled(r, 6.0, bg);
        let fg = if primary { egui::Color32::WHITE } else { t.text };
        let ir = egui::Rect::from_center_size(egui::pos2(r.min.x + 20.0, r.center().y), egui::vec2(18.0, 18.0));
        icons::paint(ui.painter(), ir, icon, fg);
        ui.painter().text(
            egui::pos2(r.min.x + 40.0, r.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            crate::theme::Tokens::ui(if primary { 14.0 } else { 13.0 }),
            fg,
        );
        if resp.clicked() {
            links::open(app, ui.ctx(), url);
        }
    };
    link(ui, "discord", Icon::Chat, tl!("Join the ArtCraft Discord"), links::DISCORD, true);
    ui.add_space(6.0);
    link(ui, "website", Icon::Globe, "getartcraft.com", links::WEBSITE, false);
    link(ui, "appPage", Icon::Globe, tl!("FilmCraft on getartcraft.com"), links::APP_PAGE, false);
    link(ui, "github", Icon::Code, tl!("Source code on GitHub"), links::GITHUB, false);
    link(ui, "reportIssue", Icon::Code, tl!("Report an issue"), links::ISSUES, false);
    ui.add_space(10.0);
    ui.label(egui::RichText::new(tl!("MIT OR Apache-2.0. Fonts: Inter, Noto Serif and JetBrains Mono (SIL OFL 1.1).")).small().color(t.text_dim));
}

pub fn track_dialogs(app: &mut FilmcraftApp, ctx: &egui::Context) {
    rename_track_dialog(app, ctx);
    delete_track_confirm_modal(app, ctx);
    output_channels_dialog(app, ctx);
    customize_header_dialog(app, ctx);
}

fn rename_track_dialog(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut draft) = app.ui.timeline.rename_track.clone() else { return };
    let mut keep = true;
    let mut apply = false;
    let mut cancel = false;
    egui::Window::new(tl!("Rename Track"))
        .id(egui::Id::unique("rename-track-dialog"))
        .collapsible(false)
        .resizable(false)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(crate::widgets::dialog_center(ctx))
        .default_width(280.0)
        .show(ctx, |ui| {
            ui.add_space(4.0);
            ui.label(tl!("Track Name:"));
            let text_resp = ui.add(egui::TextEdit::singleline(&mut draft.name).desired_width(260.0));
            app.auto.add("renameTrack.input", text_resp.rect, &draft.name);
            let focus_id = egui::Id::unique("rename-track-focused");
            if !ctx.data(|d| d.get_temp::<bool>(focus_id)).unwrap_or(false) {
                text_resp.request_focus();
                ctx.data_mut(|d| d.insert_temp(focus_id, true));
            }
            if text_resp.lost_focus() && ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                apply = true;
            }
            ui.add_space(10.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let ok_btn = ui.add(egui::Button::new(egui::RichText::new(tl!("OK")).color(egui::Color32::WHITE)).fill(app.tokens.accent));
                app.auto.add("renameTrack.ok", ok_btn.rect, "OK");
                if ok_btn.clicked() {
                    apply = true;
                }
                let cancel_btn = ui.button(tl!("Cancel"));
                app.auto.add("renameTrack.cancel", cancel_btn.rect, "Cancel");
                if cancel_btn.clicked() {
                    cancel = true;
                }
            });
        });
    if crate::widgets::escape_closes(ctx) || cancel {
        keep = false;
    }
    if apply {
        let name = draft.name.trim().to_string();
        if !name.is_empty()
            && let Err(e) = app.session.execute("timeline.setTrack", json!({"track": draft.track.0, "name": name}))
        {
            app.ui.status = e.to_string();
        }
        keep = false;
    }
    if !keep {
        ctx.data_mut(|d| d.remove_temp::<bool>(egui::Id::unique("rename-track-focused")));
    }
    app.ui.timeline.rename_track = if keep { Some(draft) } else { None };
}

fn delete_track_confirm_modal(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(confirm) = app.ui.timeline.delete_track_confirm.clone() else { return };
    let mut keep = true;
    let mut delete = false;
    let mut cancel = false;
    egui::Window::new(tl!("Delete Track"))
        .id(egui::Id::unique("delete-track-confirm-modal"))
        .collapsible(false)
        .resizable(false)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(crate::widgets::dialog_center(ctx))
        .default_width(380.0)
        .show(ctx, |ui| {
            ui.add_space(6.0);
            let msg = tlf!(
                "Track '{name}' contains {count} clip(s). Deleting the track will remove these clips. Are you sure you want to delete this track?",
                name = confirm.name,
                count = confirm.clip_count
            );
            ui.label(egui::RichText::new(msg).size(13.0));
            ui.add_space(14.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let del_btn =
                    ui.add(egui::Button::new(egui::RichText::new(tl!("Delete")).color(egui::Color32::WHITE)).fill(egui::Color32::from_rgb(0xc0, 0x39, 0x2b)));
                app.auto.add("deleteTrack.confirm", del_btn.rect, "Delete");
                if del_btn.clicked() {
                    delete = true;
                }
                let cancel_btn = ui.button(tl!("Cancel"));
                app.auto.add("deleteTrack.cancel", cancel_btn.rect, "Cancel");
                if cancel_btn.clicked() {
                    cancel = true;
                }
            });
        });
    if crate::widgets::escape_closes(ctx) || cancel {
        keep = false;
    }
    if delete {
        if let Err(e) = app.session.execute("sequence.deleteTrack", json!({"track": confirm.track.0})) {
            app.ui.status = e.to_string();
        }
        keep = false;
    }
    app.ui.timeline.delete_track_confirm = if keep { Some(confirm) } else { None };
}

fn output_channels_dialog(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut draft) = app.ui.timeline.output_channels_dialog.clone() else { return };
    let Some(seq) = app.session.active_sequence() else {
        app.ui.timeline.output_channels_dialog = None;
        return;
    };
    let mut keep = true;
    let mut apply = false;
    let mut cancel = false;
    egui::Window::new(tl!("Track Output Channel Assignments"))
        .id(egui::Id::unique("output-channels-dialog"))
        .collapsible(false)
        .resizable(false)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(crate::widgets::dialog_center(ctx))
        .default_width(340.0)
        .show(ctx, |ui| {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(format!("{}: {}", tl!("Track"), draft.track_name)).strong());
            ui.add_space(6.0);
            ui.group(|ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(tl!("Channel format:"));
                    let format_str = match draft.channels {
                        filmcraft_project::AudioChannels::Mono => "Mono",
                        filmcraft_project::AudioChannels::Stereo => "Stereo",
                        filmcraft_project::AudioChannels::Surround51 => "5.1",
                        filmcraft_project::AudioChannels::Adaptive => "Adaptive",
                    };
                    let combo = egui::ComboBox::from_id_salt("output-channels-format").selected_text(format_str).width(160.0).show_ui(ui, |ui| {
                        for (ch, name) in [
                            (filmcraft_project::AudioChannels::Stereo, "Stereo"),
                            (filmcraft_project::AudioChannels::Surround51, "5.1"),
                            (filmcraft_project::AudioChannels::Mono, "Mono"),
                            (filmcraft_project::AudioChannels::Adaptive, "Adaptive"),
                        ] {
                            ui.selectable_value(&mut draft.channels, ch, name);
                        }
                    });
                    app.auto.add("outputChannels.format", combo.response.rect, format_str);
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(tl!("Output destination:"));
                    let out_name = draft.output.and_then(|o| seq.mix_track(o)).map(|s| s.name.clone()).unwrap_or_else(|| tl!("Mix").to_string());
                    let combo = egui::ComboBox::from_id_salt("output-channels-dest").selected_text(&out_name).width(160.0).show_ui(ui, |ui| {
                        ui.selectable_value(&mut draft.output, None, tl!("Mix"));
                        for sub in &seq.submix_tracks {
                            if sub.id != draft.track {
                                ui.selectable_value(&mut draft.output, Some(sub.id), &sub.name);
                            }
                        }
                    });
                    app.auto.add("outputChannels.destination", combo.response.rect, &out_name);
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(tl!("Channel mapping:"));
                    let map_label = draft.input_map.label();
                    let combo = egui::ComboBox::from_id_salt("output-channels-mapping").selected_text(map_label).width(160.0).show_ui(ui, |ui| {
                        for m in filmcraft_project::InputMap::ALL {
                            ui.selectable_value(&mut draft.input_map, m, m.label());
                        }
                    });
                    app.auto.add("outputChannels.mapping", combo.response.rect, map_label);
                });
            });
            ui.add_space(10.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let ok_btn = ui.add(egui::Button::new(egui::RichText::new(tl!("OK")).color(egui::Color32::WHITE)).fill(app.tokens.accent));
                app.auto.add("outputChannels.ok", ok_btn.rect, "OK");
                if ok_btn.clicked() {
                    apply = true;
                }
                let cancel_btn = ui.button(tl!("Cancel"));
                app.auto.add("outputChannels.cancel", cancel_btn.rect, "Cancel");
                if cancel_btn.clicked() {
                    cancel = true;
                }
            });
        });
    if crate::widgets::escape_closes(ctx) || cancel {
        keep = false;
    }
    if apply {
        let ch_str = match draft.channels {
            filmcraft_project::AudioChannels::Mono => "mono",
            filmcraft_project::AudioChannels::Stereo => "stereo",
            filmcraft_project::AudioChannels::Surround51 => "5.1",
            filmcraft_project::AudioChannels::Adaptive => "adaptive",
        };
        let out_val = draft.output.map(|o| json!(o.0)).unwrap_or(json!("Mix"));
        let p = json!({
            "strip": draft.track.0,
            "channels": ch_str,
            "output": out_val,
            "inputMap": draft.input_map.label(),
        });
        if let Err(e) = app.session.execute("mixer.setStrip", p) {
            app.ui.status = e.to_string();
        }
        keep = false;
    }
    app.ui.timeline.output_channels_dialog = if keep { Some(draft) } else { None };
}

fn customize_header_dialog(app: &mut FilmcraftApp, ctx: &egui::Context) {
    if !app.ui.timeline.customize_header_dialog {
        return;
    }
    let mut open = true;
    let mut reset = false;
    let mut custom = app.ui.timeline.header_customization.clone();
    egui::Window::new(tl!("Customize Track Header Controls"))
        .id(egui::Id::unique("customize-track-header-dialog"))
        .collapsible(false)
        .resizable(false)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(crate::widgets::dialog_center(ctx))
        .default_width(320.0)
        .show(ctx, |ui| {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(tl!("Common Controls")).strong());
            ui.group(|ui| {
                ui.set_width(ui.available_width());
                let c = ui.checkbox(&mut custom.show_track_name, tl!("Track Name & Number"));
                app.auto.add("customizeTrackHeader.trackName", c.rect, "Track Name & Number");
                let c = ui.checkbox(&mut custom.show_source_patch, tl!("Source Patch"));
                app.auto.add("customizeTrackHeader.sourcePatch", c.rect, "Source Patch");
                let c = ui.checkbox(&mut custom.show_lock, tl!("Track Lock"));
                app.auto.add("customizeTrackHeader.lock", c.rect, "Track Lock");
                let c = ui.checkbox(&mut custom.show_target, tl!("Track Targeting"));
                app.auto.add("customizeTrackHeader.target", c.rect, "Track Targeting");
                let c = ui.checkbox(&mut custom.show_sync_lock, tl!("Sync Lock"));
                app.auto.add("customizeTrackHeader.syncLock", c.rect, "Sync Lock");
            });
            ui.add_space(8.0);
            ui.label(egui::RichText::new(tl!("Video Track Controls")).strong());
            ui.group(|ui| {
                ui.set_width(ui.available_width());
                let c = ui.checkbox(&mut custom.show_visibility, tl!("Video Visibility / Output"));
                app.auto.add("customizeTrackHeader.visibility", c.rect, "Video Visibility / Output");
            });
            ui.add_space(8.0);
            ui.label(egui::RichText::new(tl!("Audio Track Controls")).strong());
            ui.group(|ui| {
                ui.set_width(ui.available_width());
                let c = ui.checkbox(&mut custom.show_mute, tl!("Mute (M)"));
                app.auto.add("customizeTrackHeader.mute", c.rect, "Mute");
                let c = ui.checkbox(&mut custom.show_solo, tl!("Solo (S)"));
                app.auto.add("customizeTrackHeader.solo", c.rect, "Solo");
                let c = ui.checkbox(&mut custom.show_voiceover, tl!("Voice-Over Record"));
                app.auto.add("customizeTrackHeader.voiceover", c.rect, "Voice-Over Record");
                let c = ui.checkbox(&mut custom.show_automation, tl!("Track Keyframes / Automation"));
                app.auto.add("customizeTrackHeader.automation", c.rect, "Track Keyframes / Automation");
            });
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                let r_btn = ui.button(tl!("Reset to Default"));
                app.auto.add("customizeTrackHeader.reset", r_btn.rect, "Reset to Default");
                if r_btn.clicked() {
                    reset = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let done_btn = ui.add(egui::Button::new(egui::RichText::new(tl!("Done")).color(egui::Color32::WHITE)).fill(app.tokens.accent));
                    app.auto.add("customizeTrackHeader.done", done_btn.rect, "Done");
                    if done_btn.clicked() {
                        open = false;
                    }
                });
            });
        });
    if reset {
        custom = crate::state::TrackHeaderCustomization::default();
    }
    app.ui.timeline.header_customization = custom;
    if crate::widgets::escape_closes(ctx) {
        open = false;
    }
    app.ui.timeline.customize_header_dialog = open;
}
