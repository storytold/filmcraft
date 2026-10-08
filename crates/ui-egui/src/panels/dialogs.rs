//! Modal dialogs (About; Keyboard Shortcuts in `shortcuts_dialog`; Settings in `settings`;
//! Recovery/Revert in `file_dialogs`).

use crate::{Dialog, FilmcraftApp};

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
    crate::panels::comfyui::show(app, ctx);
    crate::panels::interchange_export::show(app, ctx);
    crate::panels::project_dialogs::show(app, ctx);
    crate::panels::media_browser::dialogs(app, ctx);
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
            egui::Window::new("About FilmCraft")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ctx, |ui| about(app, ui));
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
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
    let peak_id = egui::Id::new("audio-gain-peak");
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
    let mut keep = true;
    let mut apply = false;
    let mut elems: Vec<(String, egui::Rect, String)> = Vec::new();
    egui::Window::new("Audio Gain").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
        ui.add_space(6.0);
        egui::Grid::new("audio-gain").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
            let rows: [(&str, &str, &mut f64); 4] = [
                ("set", "Set Gain to:", &mut draft.set_db),
                ("adjust", "Adjust Gain by:", &mut draft.adjust_db),
                ("normalizeMax", "Normalize Max Peak to:", &mut draft.max_peak_db),
                ("normalizeAll", "Normalize All Peaks to:", &mut draft.all_peaks_db),
            ];
            for (id, label, v) in rows {
                let r = ui.radio(draft.mode == id, label);
                elems.push((format!("audioGain.{id}"), r.rect, label.to_string()));
                if r.clicked() {
                    draft.mode = id.to_string();
                }
                let f = ui.add_enabled(draft.mode == id, egui::DragValue::new(v).speed(0.1).range(-96.0..=96.0).suffix(" dB"));
                elems.push((format!("audioGain.{id}.value"), f.rect, format!("{v:.1} dB")));
                ui.end_row();
            }
        });
        ui.add_space(8.0);
        let pk_text = format!("Peak Amplitude: {}", peak.map(|p| format!("{p:.1} dB")).unwrap_or_else(|| "—".into()));
        let pk = ui.label(&pk_text);
        elems.push(("audioGain.peak".into(), pk.rect, pk_text));
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let c = ui.button("Cancel");
            elems.push(("audioGain.cancel".into(), c.rect, "Cancel".into()));
            if c.clicked() {
                keep = false;
            }
            let o = ui.add(egui::Button::new(egui::RichText::new("OK").color(egui::Color32::WHITE)).fill(app.tokens.accent));
            elems.push(("audioGain.ok".into(), o.rect, "OK".into()));
            if o.clicked() {
                apply = true;
            }
        });
    });
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
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
    let mut keep = true;
    let mut apply = false;
    let mut elems: Vec<(String, egui::Rect, String)> = Vec::new();
    const AUDIO_TYPES: [(&str, &str); 4] = [("standard", "Standard"), ("5.1", "5.1"), ("adaptive", "Adaptive"), ("mono", "Mono")];
    const SUBMIX_TYPES: [(&str, &str); 4] = [("stereo", "Stereo"), ("5.1", "5.1"), ("adaptive", "Adaptive"), ("mono", "Mono")];
    egui::Window::new("Add Tracks").collapsible(false).resizable(false).default_width(320.0).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
        // everything spans the width the window has: no empty band beside the fields
        ui.set_width(ui.available_width());
        // the label column is as wide as its widest label; the lists take the rest of the row,
        // up to the group's right edge (as in Premiere's dialog)
        let font = egui::TextStyle::Body.resolve(ui.style());
        let label_w = ["Amount", "Placement", "Track type"]
            .iter()
            .map(|l| ui.painter().layout_no_wrap(l.to_string(), font.clone(), egui::Color32::WHITE).size().x)
            .fold(0.0, f32::max);
        let row_h = ui.spacing().interact_size.y;
        let label = |ui: &mut egui::Ui, text: &str| {
            ui.allocate_ui_with_layout(egui::vec2(label_w, row_h), egui::Layout::right_to_left(egui::Align::Center), |ui| ui.label(text));
        };
        let groups: [(&str, &str, &mut u32, &mut usize, &[String], Option<(&mut String, &[(&str, &str); 4])>); 3] = [
            ("video", "Add video tracks", &mut d.video, &mut d.video_after, &vnames, None),
            ("audio", "Add audio tracks", &mut d.audio, &mut d.audio_after, &anames, Some((&mut d.audio_type, &AUDIO_TYPES))),
            ("submix", "Add audio submix tracks", &mut d.submix, &mut d.submix_after, &snames, Some((&mut d.submix_type, &SUBMIX_TYPES))),
        ];
        for (kind, title, amount, after, tracks, track_type) in groups {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(title).strong());
            ui.group(|ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    label(ui, "Amount");
                    let r = ui.add(egui::DragValue::new(amount).range(0..=99).speed(0.1));
                    elems.push((format!("addTracks.{kind}.amount"), r.rect, amount.to_string()));
                });
                ui.horizontal(|ui| {
                    label(ui, "Placement");
                    *after = (*after).min(tracks.len());
                    let place = |n: usize| match n.checked_sub(1).and_then(|i| tracks.get(i)) {
                        Some(name) => format!("After {name}"),
                        None => "Before First Track".to_string(),
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
                        label(ui, "Track type");
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
            let o = ui.add(egui::Button::new(egui::RichText::new("OK").color(egui::Color32::WHITE)).fill(app.tokens.accent));
            elems.push(("addTracks.ok".into(), o.rect, "OK".into()));
            if o.clicked() {
                apply = true;
            }
            let c = ui.button("Cancel");
            elems.push(("addTracks.cancel".into(), c.rect, "Cancel".into()));
            if c.clicked() {
                keep = false;
            }
        });
    });
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
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

/// Sequence ▸ Delete Tracks…: "Delete Video Tracks" / "Delete Audio Tracks" checkboxes, each with a
/// track choice (All Empty Tracks or one track). Returns whether the dialog stays open.
///
/// Automation ids: `deleteTracks.video`, `deleteTracks.audio` (checkboxes),
/// `deleteTracks.video.target`, `deleteTracks.audio.target` (track menus),
/// `deleteTracks.<kind>.option.<empty|V1|A2…>` (menu entries while open), `deleteTracks.ok`,
/// `deleteTracks.cancel`.
fn delete_tracks(app: &mut FilmcraftApp, ctx: &egui::Context) -> bool {
    let Some(seq) = app.session.active_sequence() else { return false };
    let names = |n: usize, p: &str| (1..=n).map(|i| format!("{p}{i}")).collect::<Vec<_>>();
    let vnames = names(seq.video_tracks.len(), "V");
    let anames = names(seq.audio_tracks.len(), "A");
    let mut draft = app.ui.delete_tracks.clone();
    let mut keep = true;
    let mut apply = false;
    let mut elems: Vec<(String, egui::Rect, String)> = Vec::new();
    egui::Window::new("Delete Tracks").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
        ui.set_min_width(300.0);
        for (kind, title, on, target, list) in [
            ("video", "Video Tracks", &mut draft.video, &mut draft.video_target, &vnames),
            ("audio", "Audio Tracks", &mut draft.audio, &mut draft.audio_target, &anames),
        ] {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(title).strong());
            let label = format!("Delete {title}");
            let c = ui.checkbox(on, &label);
            elems.push((format!("deleteTracks.{kind}"), c.rect, label));
            ui.horizontal(|ui| {
                ui.add_space(22.0);
                let shown = if target == "empty" { "All Empty Tracks".to_string() } else { target.clone() };
                ui.add_enabled_ui(*on, |ui| {
                    let r = egui::ComboBox::from_id_salt(("delete-tracks", kind)).selected_text(&shown).width(170.0).show_ui(ui, |ui| {
                        let e = ui.selectable_value(target, "empty".to_string(), "All Empty Tracks");
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
            let c = ui.button("Cancel");
            elems.push(("deleteTracks.cancel".into(), c.rect, "Cancel".into()));
            if c.clicked() {
                keep = false;
            }
            let o =
                ui.add_enabled(draft.video || draft.audio, egui::Button::new(egui::RichText::new("OK").color(egui::Color32::WHITE)).fill(app.tokens.accent));
            elems.push(("deleteTracks.ok".into(), o.rect, "OK".into()));
            if o.clicked() {
                apply = true;
            }
        });
    });
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
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
    let tab_id = egui::Id::new("about_tab");
    let mut tab = ui.data_mut(|d| d.get_temp::<u8>(tab_id)).unwrap_or(0);
    ui.horizontal(|ui| {
        for (i, (id, label)) in [("about", "About"), ("contributors", "Contributors"), ("models", "Models")].into_iter().enumerate() {
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
    ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
    ui.label("A clean-room, pure-Rust non-linear video editor. Part of the ArtCraft family.");
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
            links::open(ui.ctx(), url);
        }
    };
    link(ui, "discord", Icon::Chat, "Join the ArtCraft Discord", links::DISCORD, true);
    ui.add_space(6.0);
    link(ui, "website", Icon::Globe, "getartcraft.com", links::WEBSITE, false);
    link(ui, "appPage", Icon::Globe, "FilmCraft on getartcraft.com", links::APP_PAGE, false);
    link(ui, "github", Icon::Code, "Source code on GitHub", links::GITHUB, false);
    link(ui, "reportIssue", Icon::Code, "Report an issue", links::ISSUES, false);
    ui.add_space(10.0);
    ui.label(egui::RichText::new("MIT OR Apache-2.0. Fonts: Inter, Noto Serif and JetBrains Mono (SIL OFL 1.1).").small().color(t.text_dim));
}
