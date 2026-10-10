//! Edit / Clip / File menu dialogs (M3.10): Paste Attributes, Remove Attributes, Offline File,
//! Make Subclip, Edit Subclip, Modify ▸ Audio Channels, Modify ▸ Timecode, Frame Hold Options,
//! Field Options, Clip Speed / Duration, Nest… (Nested Sequence Name), and the Close Project and
//! Quit "save changes?" prompts.
//!
//! The open dialog lives in `UiState::clip_dialog` (serde): the engine command it runs and the
//! parameters being edited — the same JSON the command takes — so agents can open a dialog from
//! the menu, change `params` with `ui.set`, and click OK. Menu items and shortcuts invoked without
//! params open the dialog; with params the engine command runs directly.
//!
//! Automation ids (`<p>` = the dialog prefix below, `<key>` = a parameter):
//! - every dialog: `<p>.ok`, `<p>.cancel`, and `<p>.<key>` for each control;
//! - Paste Attributes `pasteAttributes.*`: `motion`, `opacity`, `timeRemapping`, `volume`,
//!   `channelVolume`, `panner`, `effects`, `effect.<id>`, `scaleTimes`;
//! - Remove Attributes `removeAttributes.*`: the same without `scaleTimes`;
//! - Offline File `offlineFile.*`: `fileName`, `name`, `tapeName`, `video`, `audio`, `width`,
//!   `height`, `fps`, `timecode`, `seconds`, `description`;
//! - Nested Sequence Name `nest.*`: `name`;
//! - Make Subclip `makeSubclip.*`: `name`, `startFrame`, `endFrame`, `restrictTrims`;
//! - Edit Subclip `editSubclip.*`: `startFrame`, `endFrame`, `restrictTrims`, `convertToMaster`;
//! - Audio Channels `audioChannels.*`: `format.<mono|stereo|5.1|adaptive>`, `count`,
//!   `clip.<n>.ch.<c>` (n, c 0-based);
//! - Timecode `timecode.*`: `timecode`, `tapeName`, `reset`;
//! - Frame Hold Options `frameHold.*`: `enabled`, `holdOn.<in|out|playhead|sourceTimecode|sequenceTime>`,
//!   `timecode`, `holdFilters`;
//! - Field Options `fieldOptions.*`: `reverseFieldDominance`, `processing.<none|alwaysDeinterlace|flickerRemoval>`;
//! - Clip Speed / Duration `speedDuration.*`: `speed`, `reverse`, `ripple`,
//!   `interpolation.<frameSampling|frameBlending|opticalFlow>`;
//! - Close Project `closeProject.save`, `closeProject.dontSave`, `closeProject.cancel`;
//! - Quit (asked when the window is closed with unsaved changes, see [`intercept_quit`])
//!   `quit.save`, `quit.dontSave`, `quit.cancel`.

use egui::{Align2, RichText};
use filmcraft_project::{AudioChannelMap, AudioChannels, ItemKind, TrackKind};
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::state::ClipDialogDraft;

type Elems = Vec<(String, egui::Rect, String)>;

fn push(elems: &mut Elems, id: impl Into<String>, r: &egui::Response, label: impl Into<String>) {
    elems.push((id.into(), r.rect, label.into()));
}

/// Dialog title and automation prefix of a command's dialog.
fn meta(command: &str) -> Option<(&'static str, &'static str)> {
    Some(match command {
        "edit.pasteAttributes" => (tl!("Paste Attributes"), "pasteAttributes"),
        "edit.removeAttributes" => (tl!("Remove Attributes"), "removeAttributes"),
        "file.newOfflineFile" => (tl!("New Offline File"), "offlineFile"),
        "clip.nest" => (tl!("Nested Sequence Name"), "nest"),
        "clip.makeSubclip" => (tl!("Make Subclip"), "makeSubclip"),
        "clip.editSubclip" => (tl!("Edit Subclip"), "editSubclip"),
        "clip.audioChannels" => (tl!("Modify Clip: Audio Channels"), "audioChannels"),
        "clip.modifyTimecode" => (tl!("Modify Clip: Timecode"), "timecode"),
        "clip.frameHoldOptions" => (tl!("Frame Hold Options"), "frameHold"),
        "clip.fieldOptions" => (tl!("Field Options"), "fieldOptions"),
        "clip.speedDuration" => (tl!("Clip Speed / Duration"), "speedDuration"),
        "file.closeProject" => (tl!("Save Project"), "closeProject"),
        "app.quit" => (tl!("Save Project"), "quit"),
        _ => return None,
    })
}

fn intrinsic_video() -> [(&'static str, &'static str); 3] {
    [("motion", tl!("Motion")), ("opacity", tl!("Opacity")), ("timeRemapping", tl!("Time Remapping"))]
}
fn intrinsic_audio() -> [(&'static str, &'static str); 3] {
    [("volume", tl!("Volume")), ("channelVolume", tl!("Channel Volume")), ("panner", tl!("Panner"))]
}
fn formats() -> [(&'static str, &'static str); 4] {
    [("mono", tl!("Mono")), ("stereo", tl!("Stereo")), ("5.1", "5.1"), ("adaptive", tl!("Adaptive"))]
}
fn hold_on() -> [(&'static str, &'static str); 5] {
    [
        ("in", tl!("In Point")),
        ("out", tl!("Out Point")),
        ("playhead", tl!("Playhead")),
        ("sourceTimecode", tl!("Source Timecode")),
        ("sequenceTime", tl!("Sequence Time")),
    ]
}

/// Menu / shortcut entry points without params open a dialog. Returns None for other ids.
pub fn route(app: &mut FilmcraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    let empty = params.as_object().is_none_or(|m| m.is_empty());
    if !empty {
        return None;
    }
    // Save All on a never-saved project asks where to save, like Save
    if id == "file.saveAll" && app.session.path.is_none() {
        return Some(app.file_dialog("file.save", params));
    }
    if (id == "file.closeProject" && !app.session.is_dirty()) || id == "app.quit" {
        return None;
    }
    meta(id)?;
    if let Err(e) = filmcraft_engine::find_command(id).map_or(Ok(()), |c| (c.enabled)(&app.session)) {
        app.ui.status = e.clone();
        return Some(Err(e));
    }
    let (params, info) = defaults(app, id);
    app.ui.clip_dialog = Some(ClipDialogDraft { command: id.into(), params, info, error: String::new() });
    Some(Ok(json!({"dialog": meta(id).map(|m| m.1)})))
}

/// A request to close the window (Quit, the title bar's close button, the OS): with unsaved
/// changes it is cancelled and Save / Don't Save / Cancel is asked first, as in Premiere (#266).
/// Returns whether the close must be cancelled.
pub fn intercept_quit(app: &mut FilmcraftApp) -> bool {
    if app.quit_confirmed || !app.session.is_dirty() {
        return false;
    }
    if app.ui.clip_dialog.as_ref().is_none_or(|d| d.command != "app.quit") {
        app.ui.clip_dialog = Some(ClipDialogDraft { command: "app.quit".into(), params: json!({}), info: Value::Null, error: String::new() });
    }
    true
}

/// Initial parameters (and display info) of a command's dialog.
fn defaults(app: &FilmcraftApp, id: &str) -> (Value, Value) {
    let s = &app.session;
    match id {
        "edit.pasteAttributes" | "edit.removeAttributes" => {
            // effects offered: the copied clips' (Paste) or the selected clips' (Remove) standard effects
            let mut names: Vec<(String, String)> = Vec::new();
            let mut add = |e: &filmcraft_project::EffectInstance| {
                if let Some(d) = e.def().filter(|d| !d.intrinsic && !filmcraft_project::graphic::is_layer_id(d.id))
                    && !names.iter().any(|n| n.0 == d.id)
                {
                    names.push((d.id.to_string(), d.name.to_string()));
                }
            };
            let mut has_v = false;
            let mut has_a = false;
            if id == "edit.pasteAttributes" {
                for (k, _, it) in &s.state.clipboard {
                    has_v |= *k == TrackKind::Video;
                    has_a |= *k == TrackKind::Audio;
                    it.effects.iter().for_each(&mut add);
                }
            } else if let Some(q) = s.active_sequence() {
                for c in &s.state.selection {
                    if let Some((tid, it)) = q.find_item(*c) {
                        let v = q.video_tracks.iter().any(|t| t.id == tid);
                        has_v |= v;
                        has_a |= !v;
                        it.effects.iter().for_each(&mut add);
                    }
                }
            }
            let mut p = json!({"motion": has_v, "opacity": has_v, "timeRemapping": has_v, "volume": has_a, "channelVolume": has_a, "panner": has_a,
                "effects": names.iter().map(|n| n.0.clone()).collect::<Vec<_>>()});
            if id == "edit.pasteAttributes" {
                p["scaleTimes"] = json!(true);
            }
            (p, json!({"video": has_v, "audio": has_a, "effects": names}))
        }
        "file.newOfflineFile" => {
            let st = s.active_sequence().map(|q| q.settings.clone()).unwrap_or_default();
            (
                json!({"fileName": "Offline.mov", "name": "", "tapeName": "", "description": "", "video": true, "audio": true, "width": st.width, "height": st.height,
                    "fps": st.frame_rate.as_f64(), "timecode": "00:00:00:00", "seconds": 10.0}),
                Value::Null,
            )
        }
        "clip.nest" => (json!({"name": filmcraft_engine::commands::next_nested_name(s)}), Value::Null),
        "clip.makeSubclip" => {
            let d = filmcraft_engine::clip_ops::subclip_defaults(s, &json!({})).unwrap_or_else(|| json!({}));
            (json!({"name": d["name"], "startFrame": d["startFrame"], "endFrame": d["endFrame"], "restrictTrims": true}), json!({"fps": d["fps"]}))
        }
        "clip.editSubclip" => {
            let sub = s.state.project_selection.iter().find_map(|i| s.project.item(*i).filter(|it| matches!(it.kind, ItemKind::Subclip { .. })));
            let Some(it) = sub else { return (json!({}), Value::Null) };
            let ItemKind::Subclip { parent, range, restrict_trims } = &it.kind else { return (json!({}), Value::Null) };
            let rate = s.project.item(*parent).and_then(|p| p.as_media()).map(|m| m.frame_rate()).unwrap_or_default();
            (
                json!({"item": it.id.0, "startFrame": rate.frame_at(range.start), "endFrame": rate.frame_at(range.end()), "restrictTrims": restrict_trims, "convertToMaster": false}),
                json!({"name": it.name, "fps": rate.as_f64()}),
            )
        }
        "clip.audioChannels" => {
            let item = s.state.project_selection.iter().find_map(|i| s.project.item(*i).and_then(|it| it.as_media()).filter(|m| m.info.has_audio()));
            match item {
                Some(m) => {
                    let n = m.info.audio().map_or(2, |a| a.channels) as u16;
                    let map = m.interpret.audio_channels.clone().unwrap_or_else(|| AudioChannelMap::for_format(AudioChannels::Stereo, n));
                    (json!({"format": format_name(map.format), "clips": map.clips}), json!({"channels": n, "items": true}))
                }
                None => (json!({"channels": [0, 1]}), json!({"channels": 2, "items": false})),
            }
        }
        "clip.modifyTimecode" => {
            let it = s.state.project_selection.iter().find_map(|i| s.project.item(*i).and_then(|it| Some((it, it.as_media()?))));
            let (tc, tape) = it
                .map(|(it, m)| {
                    let f = m.info.start_timecode.unwrap_or(0);
                    (filmcraft_time::format_timecode_frames(f, m.frame_rate(), false), it.metadata.get("Tape Name").cloned().unwrap_or_default())
                })
                .unwrap_or_else(|| ("00:00:00:00".into(), String::new()));
            (json!({"timecode": tc, "tapeName": tape}), Value::Null)
        }
        "clip.frameHoldOptions" => {
            let it = s.active_sequence().and_then(|q| s.state.selection.iter().find_map(|c| q.find_item(*c).map(|x| x.1.clone())));
            let enabled = it.as_ref().is_none_or(|i| i.frame_hold.is_some());
            let hold_filters = it.as_ref().is_some_and(|i| i.hold_filters);
            let tc = it
                .as_ref()
                .map(|i| {
                    let rate = s.project.item(i.item).and_then(|p| p.as_media()).map(|m| m.frame_rate()).unwrap_or_default();
                    filmcraft_time::format_timecode_frames(rate.frame_at(i.frame_hold.unwrap_or(i.source_in)), rate, false)
                })
                .unwrap_or_else(|| "00:00:00:00".into());
            (json!({"enabled": enabled, "holdOn": "in", "timecode": tc, "holdFilters": hold_filters}), Value::Null)
        }
        "clip.fieldOptions" => (json!({"reverseFieldDominance": false, "processing": "none"}), Value::Null),
        "clip.speedDuration" => {
            // the first selected clip's current speed; its duration at that speed for the readout
            let q = s.active_sequence();
            let it = q.and_then(|q| s.state.selection.iter().find_map(|c| q.find_item(*c).map(|x| x.1.clone())));
            let fps = q.map(|q| q.settings.frame_rate.as_f64()).unwrap_or(24.0);
            let speed = it.as_ref().map_or(1.0, |i| i.speed);
            let interp = it.as_ref().map_or(filmcraft_project::TimeInterpolation::default(), |i| i.time_interpolation);
            (
                json!({"speed": (speed.abs() * 100.0 * 100.0).round() / 100.0, "reverse": speed < 0.0, "ripple": false, "interpolation": interp.name()}),
                json!({"duration": it.as_ref().map_or(0, |i| i.duration.0), "speed": speed.abs(), "fps": fps}),
            )
        }
        _ => (json!({}), Value::Null),
    }
}

fn format_name(f: AudioChannels) -> &'static str {
    match f {
        AudioChannels::Mono => "mono",
        AudioChannels::Stereo => "stereo",
        AudioChannels::Surround51 => "5.1",
        AudioChannels::Adaptive => "adaptive",
    }
}

fn format_of(name: &str) -> AudioChannels {
    match name {
        "mono" => AudioChannels::Mono,
        "5.1" => AudioChannels::Surround51,
        "adaptive" => AudioChannels::Adaptive,
        _ => AudioChannels::Stereo,
    }
}

fn check(ui: &mut egui::Ui, elems: &mut Elems, pre: &str, p: &mut Value, key: &str, label: &str, enabled: bool) {
    let mut v = p.get(key).and_then(Value::as_bool).unwrap_or(false);
    let r = ui.add_enabled(enabled, egui::Checkbox::new(&mut v, label));
    push(elems, format!("{pre}.{key}"), &r, label);
    if r.changed() {
        p[key] = json!(v);
    }
}

fn text(ui: &mut egui::Ui, elems: &mut Elems, pre: &str, p: &mut Value, key: &str, label: &str, width: f32) {
    ui.horizontal(|ui| {
        ui.label(label);
        let mut v = p.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
        let r = ui.add(egui::TextEdit::singleline(&mut v).desired_width(width));
        push(elems, format!("{pre}.{key}"), &r, label);
        if r.changed() {
            p[key] = json!(v);
        }
    });
}

fn number(ui: &mut egui::Ui, elems: &mut Elems, pre: &str, p: &mut Value, key: &str, label: &str, range: std::ops::RangeInclusive<f64>, suffix: &str) {
    ui.horizontal(|ui| {
        ui.label(label);
        let mut v = p.get(key).and_then(Value::as_f64).unwrap_or(0.0);
        let r = ui.add(egui::DragValue::new(&mut v).range(range).suffix(suffix));
        push(elems, format!("{pre}.{key}"), &r, label);
        if r.changed() {
            p[key] = json!(v);
        }
    });
}

fn frames(ui: &mut egui::Ui, elems: &mut Elems, pre: &str, p: &mut Value, key: &str, label: &str, fps: f64) {
    ui.horizontal(|ui| {
        ui.label(label);
        let mut v = p.get(key).and_then(Value::as_i64).unwrap_or(0);
        let r = ui.add(egui::DragValue::new(&mut v).range(0..=i64::MAX / 4).suffix(" fr"));
        push(elems, format!("{pre}.{key}"), &r, label);
        if r.changed() {
            p[key] = json!(v);
        }
        let rate = filmcraft_time::FrameRate::from_f64(if fps > 0.0 { fps } else { 24.0 });
        ui.label(RichText::new(filmcraft_time::format_timecode_frames(v, rate, false)).monospace().weak());
    });
}

/// Draw the open dialog, if any.
pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut d) = app.ui.clip_dialog.clone() else { return };
    crate::widgets::revert_drag_on_escape(ctx, egui::Id::new("clip-dialog-before-drag"), &mut d);
    let Some((title, pre)) = meta(&d.command) else {
        app.ui.clip_dialog = None;
        return;
    };
    let mut elems: Elems = Vec::new();
    let mut action: Option<&'static str> = None;
    let project_name = app.session.project.name.clone();
    // a never-saved project is saved through the Save dialog
    let can_save = app.session.path.is_some() || app.hooks.pick_save.is_some();
    // the title is translated; the window keeps one id whatever the interface language
    let id = egui::Id::new(("clip-dialog", pre));
    egui::Window::new(title).id(id).collapsible(false).resizable(false).default_width(360.0).anchor(Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
        let p = &mut d.params;
        match d.command.as_str() {
            "edit.pasteAttributes" | "edit.removeAttributes" => {
                let (v, a) = (d.info["video"].as_bool().unwrap_or(true), d.info["audio"].as_bool().unwrap_or(true));
                ui.label(RichText::new(tl!("Video Attributes")).strong());
                for (k, l) in intrinsic_video() {
                    check(ui, &mut elems, pre, p, k, l, v);
                }
                ui.label(RichText::new(tl!("Audio Attributes")).strong());
                for (k, l) in intrinsic_audio() {
                    check(ui, &mut elems, pre, p, k, l, a);
                }
                let effects: Vec<(String, String)> = d.info["effects"]
                    .as_array()
                    .map(|x| x.iter().filter_map(|e| Some((e[0].as_str()?.to_string(), e[1].as_str()?.to_string()))).collect())
                    .unwrap_or_default();
                ui.label(RichText::new(tl!("Effects")).strong());
                if effects.is_empty() {
                    ui.label(RichText::new(tl!("No effects")).weak());
                }
                for (id, name) in &effects {
                    let mut on = p["effects"].as_array().is_some_and(|x| x.iter().any(|e| e.as_str() == Some(id)));
                    let r = ui.checkbox(&mut on, crate::i18n::t(name));
                    push(&mut elems, format!("{pre}.effect.{id}"), &r, name.as_str());
                    if r.changed() {
                        let mut list: Vec<Value> = p["effects"].as_array().cloned().unwrap_or_default();
                        list.retain(|e| e.as_str() != Some(id));
                        if on {
                            list.push(json!(id));
                        }
                        p["effects"] = Value::Array(list);
                    }
                }
                if d.command == "edit.pasteAttributes" {
                    ui.separator();
                    check(ui, &mut elems, pre, p, "scaleTimes", tl!("Scale Attribute Times"), true);
                }
            }
            "file.newOfflineFile" => {
                text(ui, &mut elems, pre, p, "fileName", tl!("File Name:"), 220.0);
                text(ui, &mut elems, pre, p, "name", tl!("Clip Name:"), 220.0);
                text(ui, &mut elems, pre, p, "tapeName", tl!("Tape Name:"), 220.0);
                text(ui, &mut elems, pre, p, "description", tl!("Description:"), 220.0);
                ui.horizontal(|ui| {
                    ui.label(tl!("Contains:"));
                    check(ui, &mut elems, pre, p, "video", tl!("Video"), true);
                    check(ui, &mut elems, pre, p, "audio", tl!("Audio"), true);
                });
                let has_v = p["video"].as_bool().unwrap_or(true);
                ui.add_enabled_ui(has_v, |ui| {
                    number(ui, &mut elems, pre, p, "width", tl!("Width:"), 16.0..=16384.0, " px");
                    number(ui, &mut elems, pre, p, "height", tl!("Height:"), 16.0..=16384.0, " px");
                    number(ui, &mut elems, pre, p, "fps", tl!("Frame Rate:"), 1.0..=240.0, " fps");
                });
                text(ui, &mut elems, pre, p, "timecode", tl!("Media Start:"), 120.0);
                number(ui, &mut elems, pre, p, "seconds", tl!("Duration:"), 0.05..=86_400.0, " s");
            }
            "clip.nest" => {
                // the name is ready to type over, and Enter accepts it
                let mut v = p["name"].as_str().unwrap_or_default().to_string();
                ui.horizontal(|ui| {
                    ui.label(tl!("Name:"));
                    let id = egui::Id::new("nest-name");
                    let r = ui.add(egui::TextEdit::singleline(&mut v).desired_width(240.0).id(id));
                    push(&mut elems, format!("{pre}.name"), &r, "Name:");
                    if r.changed() {
                        p["name"] = json!(v);
                    }
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        action = Some("ok");
                    }
                    let first = egui::Id::new("nest-name-focused");
                    if !ui.data(|d| d.get_temp::<bool>(first).unwrap_or(false)) {
                        ui.data_mut(|d| d.insert_temp(first, true));
                        r.request_focus();
                    }
                });
            }
            "clip.makeSubclip" | "clip.editSubclip" => {
                let fps = d.info["fps"].as_f64().unwrap_or(24.0);
                if d.command == "clip.makeSubclip" {
                    text(ui, &mut elems, pre, p, "name", tl!("Name:"), 240.0);
                } else {
                    ui.label(tlf!("Subclip: {name}", name = d.info["name"].as_str().unwrap_or_default()));
                }
                frames(ui, &mut elems, pre, p, "startFrame", tl!("Start:"), fps);
                frames(ui, &mut elems, pre, p, "endFrame", tl!("End:"), fps);
                check(ui, &mut elems, pre, p, "restrictTrims", tl!("Restrict Trims To Subclip Boundaries"), true);
                if d.command == "clip.editSubclip" {
                    check(ui, &mut elems, pre, p, "convertToMaster", tl!("Convert to Master Clip"), true);
                }
            }
            "clip.audioChannels" => {
                let n = d.info["channels"].as_u64().unwrap_or(2).max(1) as u16;
                if d.info["items"].as_bool().unwrap_or(false) {
                    ui.horizontal(|ui| {
                        ui.label(tl!("Clip Channel Format:"));
                        for (k, l) in formats() {
                            let r = ui.radio(p["format"].as_str() == Some(k), l);
                            push(&mut elems, format!("{pre}.format.{k}"), &r, l);
                            if r.clicked() {
                                p["format"] = json!(k);
                                p["clips"] = json!(AudioChannelMap::for_format(format_of(k), n).clips);
                            }
                        }
                    });
                    let mut clips: Vec<Vec<u16>> = serde_json::from_value(p["clips"].clone()).unwrap_or_default();
                    ui.horizontal(|ui| {
                        ui.label(tl!("Number of Audio Clips:"));
                        let mut count = clips.len() as u32;
                        let r = ui.add(egui::DragValue::new(&mut count).range(1..=n as u32));
                        push(&mut elems, format!("{pre}.count"), &r, "Number of Audio Clips");
                        if r.changed() {
                            let count = count.max(1) as usize;
                            while clips.len() < count {
                                let c = (clips.len() as u16).min(n - 1);
                                clips.push(vec![c]);
                            }
                            clips.truncate(count);
                        }
                    });
                    ui.label(RichText::new(tl!("Media Source Channels")).strong());
                    egui::Grid::new("audio-channel-matrix").striped(true).show(ui, |ui| {
                        ui.label("");
                        for c in 0..n {
                            ui.label(tlf!("Ch. {n}", n = c + 1));
                        }
                        ui.end_row();
                        for (ci, chans) in clips.iter_mut().enumerate() {
                            ui.label(tlf!("Clip {n}", n = ci + 1));
                            for c in 0..n {
                                let mut on = chans.contains(&c);
                                let r = ui.checkbox(&mut on, "");
                                push(&mut elems, format!("{pre}.clip.{ci}.ch.{c}"), &r, format!("Clip {} Ch. {}", ci + 1, c + 1));
                                if r.changed() {
                                    if on {
                                        chans.push(c);
                                        chans.sort_unstable();
                                    } else {
                                        chans.retain(|x| *x != c);
                                    }
                                }
                            }
                            ui.end_row();
                        }
                    });
                    p["clips"] = json!(clips);
                } else {
                    ui.label(tl!("Source channels the selected audio clips play:"));
                    let mut chans: Vec<u16> = serde_json::from_value(p["channels"].clone()).unwrap_or_default();
                    ui.horizontal(|ui| {
                        for c in 0..n.max(2) {
                            let mut on = chans.contains(&c);
                            let r = ui.checkbox(&mut on, tlf!("Ch. {n}", n = c + 1));
                            push(&mut elems, format!("{pre}.clip.0.ch.{c}"), &r, format!("Ch. {}", c + 1));
                            if r.changed() {
                                if on {
                                    chans.push(c);
                                    chans.sort_unstable();
                                } else {
                                    chans.retain(|x| *x != c);
                                }
                            }
                        }
                    });
                    p["channels"] = json!(chans);
                }
            }
            "clip.modifyTimecode" => {
                text(ui, &mut elems, pre, p, "timecode", tl!("Set Timecode:"), 120.0);
                text(ui, &mut elems, pre, p, "tapeName", tl!("Tape Name:"), 200.0);
                check(ui, &mut elems, pre, p, "reset", tl!("Use the file's timecode"), true);
            }
            "clip.frameHoldOptions" => {
                check(ui, &mut elems, pre, p, "enabled", tl!("Hold On"), true);
                let on = p["enabled"].as_bool().unwrap_or(true);
                ui.add_enabled_ui(on, |ui| {
                    for (k, l) in hold_on() {
                        let r = ui.radio(p["holdOn"].as_str() == Some(k), l);
                        push(&mut elems, format!("{pre}.holdOn.{k}"), &r, l);
                        if r.clicked() {
                            p["holdOn"] = json!(k);
                        }
                    }
                    if matches!(p["holdOn"].as_str(), Some("sourceTimecode" | "sequenceTime")) {
                        text(ui, &mut elems, pre, p, "timecode", tl!("Timecode:"), 120.0);
                    }
                    check(ui, &mut elems, pre, p, "holdFilters", tl!("Hold Filters"), true);
                });
            }
            "clip.fieldOptions" => {
                check(ui, &mut elems, pre, p, "reverseFieldDominance", tl!("Reverse Field Dominance"), true);
                ui.label(RichText::new(tl!("Processing Options")).strong());
                for (k, l) in [("none", tl!("None")), ("alwaysDeinterlace", tl!("Always Deinterlace")), ("flickerRemoval", tl!("Flicker Removal"))] {
                    let r = ui.radio(p["processing"].as_str() == Some(k), l);
                    push(&mut elems, format!("{pre}.processing.{k}"), &r, l);
                    if r.clicked() {
                        p["processing"] = json!(k);
                    }
                }
            }
            "clip.speedDuration" => {
                number(ui, &mut elems, pre, p, "speed", tl!("Speed:"), 0.01..=100_000.0, " %");
                // Duration follows the speed: the media shown stays the same
                let (dur, was, fps) =
                    (d.info["duration"].as_i64().unwrap_or(0), d.info["speed"].as_f64().unwrap_or(1.0), d.info["fps"].as_f64().unwrap_or(24.0));
                let now = p["speed"].as_f64().unwrap_or(100.0) / 100.0;
                if dur > 0 && now > 0.0 {
                    let rate = filmcraft_time::FrameRate::from_f64(if fps > 0.0 { fps } else { 24.0 });
                    let ticks = filmcraft_time::Tick((dur as f64 * was / now).round() as i64);
                    ui.horizontal(|ui| {
                        ui.label(tl!("Duration:"));
                        ui.label(RichText::new(filmcraft_time::format_timecode_frames(rate.frame_at(ticks), rate, false)).monospace());
                    });
                }
                check(ui, &mut elems, pre, p, "reverse", tl!("Reverse Speed"), true);
                check(ui, &mut elems, pre, p, "ripple", tl!("Ripple Edit, Shifting Trailing Clips"), true);
                ui.label(RichText::new(tl!("Time Interpolation")).strong());
                for m in filmcraft_project::TimeInterpolation::ALL {
                    let r = ui.radio(p["interpolation"].as_str() == Some(m.name()), crate::i18n::t(m.label()));
                    push(&mut elems, format!("{pre}.interpolation.{}", m.name()), &r, m.label());
                    if r.clicked() {
                        p["interpolation"] = json!(m.name());
                    }
                }
            }
            "file.closeProject" | "app.quit" => {
                ui.label(tlf!("Save changes to “{project_name}” before closing?", project_name));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let r = ui.button(tl!("Cancel"));
                    push(&mut elems, format!("{pre}.cancel"), &r, "Cancel");
                    if r.clicked() {
                        action = Some("cancel");
                    }
                    let r = ui.button(tl!("Don't Save"));
                    push(&mut elems, format!("{pre}.dontSave"), &r, "Don't Save");
                    if r.clicked() {
                        action = Some("dontSave");
                    }
                    let r = ui.add_enabled(can_save, egui::Button::new(tl!("Save")));
                    push(&mut elems, format!("{pre}.save"), &r, "Save");
                    if r.clicked() {
                        action = Some("save");
                    }
                });
                return;
            }
            _ => {}
        }
        if !d.error.is_empty() {
            ui.colored_label(egui::Color32::from_rgb(0xe0, 0x60, 0x60), &d.error);
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let r = ui.button(tl!("Cancel"));
            push(&mut elems, format!("{pre}.cancel"), &r, "Cancel");
            if r.clicked() {
                action = Some("cancel");
            }
            let r = ui.button(tl!("OK"));
            push(&mut elems, format!("{pre}.ok"), &r, "OK");
            if r.clicked() {
                action = Some("ok");
            }
        });
    });
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    if crate::widgets::escape_closes(ctx) {
        action = Some("cancel");
    }
    if action.is_some() {
        ctx.data_mut(|d| d.remove::<bool>(egui::Id::new("nest-name-focused")));
    }
    match action {
        Some("cancel") => {
            app.ui.clip_dialog = None;
            return;
        }
        Some("ok") => {
            let params = finish(&d);
            match app.session.execute(&d.command, params) {
                Ok(_) => {
                    app.ui.clip_dialog = None;
                    return;
                }
                Err(e) => d.error = e.to_string(),
            }
        }
        Some("dontSave") | Some("save") => {
            if action == Some("save") {
                let saved = if app.session.path.is_some() {
                    app.session.execute("file.save", json!({})).map(|_| ()).map_err(|e| e.to_string())
                } else {
                    app.file_dialog("file.save", &json!({})).map(|_| ())
                };
                if let Err(e) = saved {
                    app.ui.status = e;
                    app.ui.clip_dialog = None;
                    return;
                }
                // the Save dialog was cancelled: back to the prompt
                if app.session.is_dirty() {
                    app.ui.clip_dialog = Some(d);
                    return;
                }
            }
            // Quit ▸ Don't Save discards the changes: closing the project also clears their recovery
            // snapshot, so the next launch doesn't offer them back
            let close = d.command != "app.quit" || app.session.is_dirty();
            if close && let Err(e) = app.session.execute("file.closeProject", json!({"force": true})) {
                app.ui.status = e.to_string();
            }
            if d.command == "app.quit" {
                app.quit_confirmed = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            app.ui.clip_dialog = None;
            return;
        }
        _ => {}
    }
    app.ui.clip_dialog = Some(d);
}

/// The parameters sent on OK (dialog-only fields dropped or converted).
fn finish(d: &ClipDialogDraft) -> Value {
    let mut p = d.params.clone();
    match d.command.as_str() {
        "file.newOfflineFile" if p["name"].as_str().is_some_and(str::is_empty) => {
            p.as_object_mut().map(|m| m.remove("name"));
        }
        "clip.modifyTimecode" if p["reset"].as_bool() == Some(true) => {
            p.as_object_mut().map(|m| m.remove("timecode"));
        }
        // the timecode field only applies to Source Timecode / Sequence Time
        "clip.frameHoldOptions" if !matches!(p["holdOn"].as_str(), Some("sourceTimecode" | "sequenceTime")) => {
            p.as_object_mut().map(|m| m.remove("timecode"));
        }
        _ => {}
    }
    p
}
