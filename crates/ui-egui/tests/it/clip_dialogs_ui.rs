//! Headless UI tests of the Edit / Clip / File menu dialogs (M3.10): Paste Attributes, Make
//! Subclip, Frame Hold Options, Modify ▸ Audio Channels, the Close Project prompt, and nested
//! menus (Clip ▸ Video Options ▸ Time Interpolation).

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_engine::project::{ItemKind, Label};
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    fn new() -> Self {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(s).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        d
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            // the control channel's clicks and keys enter through the input hook
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
        }
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                return v;
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let v = self.call(method, params.clone());
        assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
        v["result"].clone()
    }

    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }

    fn menu(&mut self, id: &str) -> Value {
        let r = self.ok("ui.menu.invoke", json!({"id": id}));
        self.frames(3);
        r
    }

    fn click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(3);
    }

    fn has(&mut self, id: &str) -> bool {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        v.as_array().unwrap().iter().any(|e| e["id"] == json!(id))
    }

    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }

    fn v1(&mut self, i: usize) -> filmcraft_engine::project::TrackItem {
        self.app().session.active_sequence().unwrap().video_tracks[0].items[i].clone()
    }

    fn item_named(&mut self, name: &str) -> u64 {
        self.app().session.project.items.values().find(|i| i.name == name).unwrap().id.0
    }

    /// The window is asked to close (Quit, Cmd+Q, its close button) for one frame.
    fn request_close(&mut self) {
        self.harness.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default().events.push(egui::ViewportEvent::Close);
        self.frames(1);
    }

    /// The app sent `cmd` to the window in the last frame.
    fn sent(&self, cmd: &egui::ViewportCommand) -> bool {
        self.harness.output().viewport_output.get(&egui::ViewportId::ROOT).is_some_and(|v| v.commands.contains(cmd))
    }

    /// Make an unsaved change.
    fn edit(&mut self) {
        let c = self.v1(0);
        self.exec("timeline.select", json!({"clips": [c.id.0]}));
        self.menu("edit.label.yellow");
        assert!(self.app().session.is_dirty());
    }
}

#[test]
fn paste_attributes_dialog() {
    let mut d = Driver::new();
    let (a, b) = (d.v1(0), d.v1(2));
    d.exec("effects.setParam", json!({"clip": a.id.0, "effect": "motion", "param": "rotation", "value": 45.0}));
    d.exec("effects.setParam", json!({"clip": a.id.0, "effect": "opacity", "param": "opacity", "value": 25.0}));
    d.exec("effects.apply", json!({"clips": [a.id.0], "effect": "gaussian_blur"}));
    d.exec("timeline.select", json!({"clips": [a.id.0]}));
    d.exec("edit.copy", json!({}));
    d.exec("timeline.select", json!({"clips": [b.id.0]}));
    assert_eq!(d.menu("edit.pasteAttributes")["dialog"], "pasteAttributes");
    for id in
        ["pasteAttributes.motion", "pasteAttributes.opacity", "pasteAttributes.volume", "pasteAttributes.effect.gaussian_blur", "pasteAttributes.scaleTimes"]
    {
        assert!(d.has(id), "{id}");
    }
    // untick Opacity and the blur, then OK
    d.click("pasteAttributes.opacity");
    d.click("pasteAttributes.effect.gaussian_blur");
    d.click("pasteAttributes.ok");
    assert!(d.app().ui.clip_dialog.is_none(), "closed on OK");
    let got = d.v1(2);
    assert_eq!(got.effect("motion").unwrap().param("rotation").unwrap().value.as_f64(), Some(45.0));
    assert_eq!(got.effect("opacity").unwrap().param("opacity").unwrap().value.as_f64(), Some(100.0));
    assert!(got.effect("gaussian_blur").is_none());
    // Cancel leaves the clip alone; with params the command runs directly (no dialog)
    d.menu("edit.removeAttributes");
    d.click("removeAttributes.cancel");
    assert!(d.app().ui.clip_dialog.is_none());
    assert_eq!(d.v1(2).effect("motion").unwrap().param("rotation").unwrap().value.as_f64(), Some(45.0));
    d.exec("edit.removeAttributes", json!({"opacity": false}));
    assert_eq!(d.v1(2).effect("motion").unwrap().param("rotation").unwrap().value.as_f64(), Some(0.0));
}

#[test]
fn make_subclip_frame_hold_and_audio_channel_dialogs() {
    let mut d = Driver::new();
    let ocean = d.item_named("Ocean_Sunset.mp4");
    d.exec("source.open", json!({"item": ocean}));
    d.exec("project.setMarks", json!({"item": ocean, "in": filmcraft_time::FrameRate::FPS_23_976.tick_of(24).0}));
    d.menu("clip.makeSubclip");
    assert_eq!(d.app().ui.clip_dialog.as_ref().unwrap().params["startFrame"], 24);
    for id in ["makeSubclip.name", "makeSubclip.startFrame", "makeSubclip.endFrame", "makeSubclip.restrictTrims", "makeSubclip.ok"] {
        assert!(d.has(id), "{id}");
    }
    d.ok("ui.set", json!({"clipDialog": {"name": "Waves", "endFrame": 48}}));
    d.click("makeSubclip.ok");
    let sub = d.app().session.project.items.values().find(|i| i.name == "Waves").cloned().expect("subclip made");
    let ItemKind::Subclip { range, .. } = sub.kind else { panic!() };
    assert_eq!(range.duration, filmcraft_time::FrameRate::FPS_23_976.tick_of(24));

    // Frame Hold Options on the selected clip: Hold On ▸ Playhead
    let c = d.v1(1);
    d.exec("timeline.select", json!({"clips": [c.id.0]}));
    d.exec("playhead.set", json!({"time": (c.start + filmcraft_time::FrameRate::FPS_23_976.tick_of(5)).0}));
    d.menu("clip.frameHoldOptions");
    assert_eq!(d.app().ui.clip_dialog.as_ref().unwrap().params["enabled"], false, "Hold On starts unticked for a clip without a hold");
    d.click("frameHold.enabled");
    d.click("frameHold.holdOn.playhead");
    d.click("frameHold.holdFilters");
    d.click("frameHold.ok");
    let got = d.v1(1);
    assert_eq!(got.frame_hold, Some(c.source_in + filmcraft_time::FrameRate::FPS_23_976.tick_of(5)));
    assert!(got.hold_filters);

    // Modify ▸ Audio Channels (Shift+G) on a project item: Mono makes one clip per channel
    d.exec("project.select", json!({"items": [ocean]}));
    d.menu("clip.audioChannels");
    assert!(d.has("audioChannels.clip.0.ch.1"));
    d.click("audioChannels.format.mono");
    assert_eq!(d.app().ui.clip_dialog.as_ref().unwrap().params["clips"], json!([[0], [1]]));
    d.click("audioChannels.ok");
    let m = d.app().session.project.item(filmcraft_engine::project::ItemId(ocean)).unwrap().as_media().unwrap().interpret.audio_channels.clone().unwrap();
    assert_eq!(m.clips, vec![vec![0], vec![1]]);
}

#[test]
fn close_project_prompt_and_nested_menus() {
    let mut d = Driver::new();
    // nested menu paths reach the menu list
    let menus = d.ok("ui.menu.list", json!({}));
    let item = menus.as_array().unwrap().iter().find(|m| m["id"] == "clip.timeInterpolation.frameBlending").unwrap().clone();
    assert_eq!(item["path"], json!(["Clip", "Video Options", "Time Interpolation"]));
    let labels: Vec<&str> = menus.as_array().unwrap().iter().filter(|m| m["path"] == json!(["Edit", "Label"])).filter_map(|m| m["label"].as_str()).collect();
    assert_eq!(labels.len(), 17);
    // label from the menu
    let c = d.v1(0);
    d.exec("timeline.select", json!({"clips": [c.id.0]}));
    d.menu("edit.label.yellow");
    assert_eq!(d.v1(0).label, Label::Yellow);
    // the project is dirty: Close Project asks
    assert!(d.app().session.is_dirty());
    assert_eq!(d.menu("file.closeProject")["dialog"], "closeProject");
    assert!(d.has("closeProject.save") && d.has("closeProject.dontSave"));
    d.click("closeProject.cancel");
    assert!(!d.app().session.project.items.is_empty());
    d.menu("file.closeProject");
    d.click("closeProject.dontSave");
    assert!(d.app().session.project.items.is_empty(), "closed without saving");
    assert!(d.app().ui.clip_dialog.is_none());
}

#[test]
fn subclip_in_the_source_monitor_and_project_menu() {
    let mut d = Driver::new();
    let r24 = filmcraft_time::FrameRate::FPS_23_976;
    let ocean = d.item_named("Ocean_Sunset.mp4");
    d.exec("project.setMarks", json!({"item": ocean, "in": r24.tick_of(24).0, "out": r24.tick_of(71).0}));
    d.exec("project.select", json!({"items": [ocean]}));
    let sub = d.exec("clip.makeSubclip", json!({"name": "Waves Sub"}))["item"].as_u64().unwrap();
    d.exec("source.open", json!({"item": sub}));
    d.frames(4);
    assert_eq!(d.app().session.state.source_playhead, r24.tick_of(24), "opens at the subclip's In");
    // the scrub bar spans the subclip: its left end is the In, its right end the last frame
    let bar = d.ok("ui.elements", json!({"prefix": "source.scrubBar"}))[0]["rect"].clone();
    let (x, y, w, h) = (bar[0].as_f64().unwrap(), bar[1].as_f64().unwrap(), bar[2].as_f64().unwrap(), bar[3].as_f64().unwrap());
    d.ok("ui.click", json!({"x": x + w - 1.0, "y": y + h / 2.0}));
    d.frames(3);
    assert_eq!(d.app().session.state.source_playhead, r24.tick_of(71));
    d.ok("ui.click", json!({"x": x + 1.0, "y": y + h / 2.0}));
    d.frames(3);
    assert_eq!(d.app().session.state.source_playhead, r24.tick_of(24));
    // Project panel ▸ right-click ▸ Edit Subclip… opens the dialog; Convert to Master Clip converts
    d.ok("ui.panel.show", json!({"panel": "Project"}));
    d.exec("project.view.set", json!({"view": "list"}));
    d.exec("project.select", json!({"items": [sub]}));
    d.frames(4);
    // the subclip is made in its master clip's bin: expand it
    let bin = d.app().session.project.root.parent_of(filmcraft_engine::project::ItemId(sub));
    if let Some(b) = bin.filter(|b| *b != d.app().session.project.root.id) {
        d.click(&format!("project.bin.{}.toggle", b.0));
        d.frames(3);
    }
    let row = format!("project.item.{sub}");
    if d.has(&row) {
        d.ok("ui.click", json!({"id": row, "button": "right"}));
        d.frames(3);
        assert!(d.has("project.itemMenu.editSubclip") && d.has("project.itemMenu.convertToMaster"));
        d.click("project.itemMenu.editSubclip");
        assert_eq!(d.app().ui.clip_dialog.as_ref().map(|c| c.command.clone()).as_deref(), Some("clip.editSubclip"));
        d.click("editSubclip.cancel");
        d.ok("ui.click", json!({"id": row, "button": "right"}));
        d.frames(3);
        d.click("project.itemMenu.convertToMaster");
        assert!(d.app().session.project.item(filmcraft_engine::project::ItemId(sub)).unwrap().as_media().is_some(), "converted");
    } else {
        panic!("subclip row not shown: {:?}", d.ok("ui.elements", json!({"prefix": "project.item."})));
    }
}

/// #201: the clip's right-click Speed/Duration… opens the dialog instead of applying 50%; OK
/// applies the speed typed there, Cancel leaves the clip alone.
#[test]
fn speed_duration_from_the_clip_menu_opens_the_dialog() {
    let mut d = Driver::new();
    let c = d.v1(0);
    d.exec("timeline.select", json!({"clips": [c.id.0]}));
    d.ok("ui.click", json!({"id": format!("timeline.clip.{}", c.id.0), "button": "right"}));
    d.frames(3);
    d.click("timeline.clipMenu.clip.speedDuration");
    assert_eq!(d.v1(0).speed, 1.0, "nothing applied yet");
    let dlg = d.app().ui.clip_dialog.clone().expect("the dialog opened");
    assert_eq!(dlg.command, "clip.speedDuration");
    assert_eq!(dlg.params["speed"], json!(100.0), "starts at the clip's speed");
    assert!(d.has("speedDuration.reverse") && d.has("speedDuration.interpolation.frameBlending"));
    d.click("speedDuration.cancel");
    assert!(d.app().ui.clip_dialog.is_none());
    assert_eq!(d.v1(0).speed, 1.0, "Cancel leaves it");

    d.menu("clip.speedDuration");
    if let Some(dlg) = d.app().ui.clip_dialog.as_mut() {
        dlg.params["speed"] = json!(200.0);
    }
    d.click("speedDuration.interpolation.frameBlending");
    d.click("speedDuration.ok");
    let got = d.v1(0);
    assert_eq!(got.speed, 2.0);
    assert_eq!(got.time_interpolation, filmcraft_project::TimeInterpolation::FrameBlending);
}

/// #786: the clip's right-click Label menu lists the labels by the user's names (with their
/// colours, as Edit ▸ Label does) and each choice labels the clip.
#[test]
fn the_clip_menu_label_choices_use_the_user_names() {
    let mut d = Driver::new();
    let key = filmcraft_engine::settings::label_id(Label::Iris);
    d.app().session.prefs.labels.colors.get_mut(&key).unwrap().name = "Interviews".into();
    let c = d.v1(0);
    d.exec("timeline.select", json!({"clips": [c.id.0]}));
    d.ok("ui.click", json!({"id": format!("timeline.clip.{}", c.id.0), "button": "right"}));
    d.frames(3);
    d.click("timeline.clipMenu.edit.label");
    let choice = d.ok("ui.elements", json!({"prefix": "timeline.clipMenu.label.Iris"}));
    assert_eq!(choice[0]["label"], json!("Interviews"), "{choice}");
    d.click("timeline.clipMenu.label.Iris");
    assert_eq!(d.v1(0).label, Label::Iris);
}

/// #1018: Scene Edit Detection… is in the clip's right-click menu and opens its dialog.
#[test]
fn scene_edit_detection_from_the_clip_menu_opens_the_dialog() {
    let mut d = Driver::new();
    let c = d.v1(0);
    d.exec("timeline.select", json!({"clips": [c.id.0]}));
    d.ok("ui.click", json!({"id": format!("timeline.clip.{}", c.id.0), "button": "right"}));
    d.frames(3);
    d.click("timeline.clipMenu.clip.sceneEditDetection");
    for id in ["sceneDetect.applyCuts", "sceneDetect.createSubclips", "sceneDetect.generateMarkers", "sceneDetect.ok"] {
        assert!(d.has(id), "{id}");
    }
    d.click("sceneDetect.cancel");
    assert!(!d.has("sceneDetect.ok"));
}

#[test]
fn quitting_with_unsaved_changes_asks_to_save() {
    let mut d = Driver::new();
    d.edit();
    // the close is cancelled and the prompt asks instead
    d.request_close();
    assert!(d.sent(&egui::ViewportCommand::CancelClose), "the window must stay open");
    d.frames(2);
    assert!(d.has("quit.save") && d.has("quit.dontSave") && d.has("quit.cancel"));
    // Cancel keeps the app and the changes
    d.click("quit.cancel");
    assert!(d.app().ui.clip_dialog.is_none());
    assert!(d.app().session.is_dirty() && !d.app().quit_confirmed);
    d.request_close();
    assert!(d.sent(&egui::ViewportCommand::CancelClose), "asks again");
    d.frames(2);
    // Don't Save discards the changes (and their recovery snapshot) and closes the window
    d.click("quit.dontSave");
    assert!(d.app().quit_confirmed && !d.app().session.is_dirty());
    assert!(d.app().ui.clip_dialog.is_none());
    d.request_close();
    assert!(!d.sent(&egui::ViewportCommand::CancelClose), "the confirmed quit goes through");
}

/// #1943: the macOS menu bar's Quit (Cmd+Q) sends `app.quit` to the command inbox; with unsaved
/// changes that closes the window through the prompt instead of quitting outright.
#[test]
fn native_menu_quit_with_unsaved_changes_asks_to_save() {
    let mut d = Driver::new();
    d.edit();
    let (tx, rx) = channel();
    d.app().command_inbox = Some(rx);
    tx.send("app.quit".to_string()).unwrap();
    d.frames(1);
    assert!(d.sent(&egui::ViewportCommand::Close), "Quit asks the window to close");
    assert!(!d.app().quit_confirmed, "nothing is confirmed yet");
    // the window's close request is cancelled and the prompt asks instead
    d.request_close();
    assert!(d.sent(&egui::ViewportCommand::CancelClose), "the window must stay open");
    d.frames(2);
    assert!(d.has("quit.save") && d.has("quit.dontSave") && d.has("quit.cancel"));
    assert!(d.app().session.is_dirty());
}

#[test]
fn quit_save_on_a_new_project_uses_the_save_dialog_and_scripted_quit_does_not_ask() {
    let path = std::env::temp_dir().join(format!("fc-quit-save-{}.fcproj", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let mut d = Driver::new();
    d.app().session.path = None;
    d.edit();
    let picked = path.to_string_lossy().into_owned();
    let mut answers = vec![None, Some(picked)];
    d.app().hooks.pick_save = Some(Box::new(move |_| if answers.is_empty() { None } else { answers.remove(0) }));
    d.request_close();
    d.frames(2);
    // the Save dialog is cancelled: back to the prompt, nothing quits
    d.click("quit.save");
    assert!(d.app().ui.clip_dialog.is_some() && !d.app().quit_confirmed);
    // saved: the project is written and the window closes
    d.click("quit.save");
    assert!(d.app().quit_confirmed && !d.app().session.is_dirty());
    assert!(path.exists(), "saved to the chosen file");
    let _ = std::fs::remove_file(&path);
    d.request_close();
    assert!(!d.sent(&egui::ViewportCommand::CancelClose));
    // the control channel's app.quit quits without the prompt
    let mut d = Driver::new();
    d.edit();
    d.ok("app.quit", json!({}));
    assert!(d.app().quit_confirmed);
}

/// #580: Escape while dragging a value in a dialog puts the value back and keeps the dialog open;
/// Escape with no drag still closes it.
#[test]
fn escape_mid_drag_reverts_the_field_and_keeps_the_dialog() {
    let mut d = Driver::new();
    let c = d.v1(0);
    d.exec("timeline.select", json!({"clips": [c.id.0]}));
    d.menu("clip.speedDuration");
    let els = d.ok("ui.elements", json!({"prefix": "speedDuration.speed"}));
    let r: Vec<f32> = els.as_array().unwrap().iter().find(|e| e["id"] == "speedDuration.speed").expect("speed field")["rect"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap() as f32)
        .collect();
    let from = egui::pos2(r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
    let speed = |d: &mut Driver| d.app().ui.clip_dialog.as_ref().map(|g| g.params["speed"].clone());
    let drag = |d: &mut Driver, escape: bool| {
        let mut send = |e: egui::Event| {
            d.harness.input_mut().events.push(e);
            d.frames(1);
        };
        let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        send(egui::Event::PointerMoved(from));
        send(button(from, true));
        for k in 1..=10 {
            send(egui::Event::PointerMoved(from + egui::vec2(4.0 * k as f32, 0.0)));
            if escape && k == 5 {
                send(egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() });
            }
        }
        send(button(from + egui::vec2(40.0, 0.0), false));
        d.frames(3);
    };
    drag(&mut d, true);
    assert_eq!(speed(&mut d), Some(json!(100.0)), "the speed it had before the drag, and the dialog is still open");
    drag(&mut d, false);
    assert_ne!(speed(&mut d), Some(json!(100.0)), "without Escape the same drag changes the speed");
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(2);
    assert!(d.app().ui.clip_dialog.is_none(), "Escape with no drag closes the dialog");
    assert_eq!(d.v1(0).speed, 1.0, "and applies nothing");
}

#[test]
fn clip_context_menu_adds_a_marker_at_the_playhead_with_one_undo() {
    for audio in [false, true] {
        let mut d = Driver::new();
        d.exec("markers.clearAll", json!({}));
        d.exec("playhead.set", json!({"frame": 35}));
        let seq = d.app().session.active_sequence().unwrap();
        let clip = if audio { seq.audio_tracks[0].items[0].id } else { seq.video_tracks[0].items[0].id };
        let playhead = d.app().session.playhead();
        let before = d.app().session.project.clone();
        let history = d.app().session.history.undo.len();
        d.ok("ui.click", json!({"id": format!("timeline.clip.{}", clip.0), "button": "right", "fy": 0.3}));
        d.frames(3);
        assert!(d.has("timeline.clipMenu.markers.add"), "Add Marker is available on video and audio clips");
        d.click("timeline.clipMenu.markers.add");
        let markers = &d.app().session.active_sequence().unwrap().markers;
        assert_eq!(markers.len(), 1);
        assert_eq!(markers[0].start, playhead);
        assert_eq!(d.app().session.history.undo.len(), history + 1);
        let after = d.app().session.project.clone();
        d.exec("edit.undo", json!({}));
        assert_eq!(serde_json::to_value(&*d.app().session.project).unwrap(), serde_json::to_value(&*before).unwrap());
        d.exec("edit.redo", json!({}));
        assert_eq!(serde_json::to_value(&*d.app().session.project).unwrap(), serde_json::to_value(&*after).unwrap());
    }
}

/// A hand-made R3D NE item `name` at `path` (the SDK is optional; the dialog works on the project).
fn add_r3d(d: &mut Driver, name: &str, path: &str) -> u64 {
    let ocean = d.item_named("Ocean_Sunset.mp4");
    let app = d.app();
    let mut p = (*app.session.project).clone();
    let mut clip = p.item(filmcraft_engine::project::ItemId(ocean)).unwrap().as_media().unwrap().clone();
    clip.media = filmcraft_engine::project::MediaRef::File { path: path.into() };
    clip.info.video.as_mut().unwrap().codec = "R3D NE".into();
    let id = p.add_item(name, Label::Iris, ItemKind::Media(clip), None);
    app.session.project = std::sync::Arc::new(p);
    id.0
}

/// Clip ▸ Source Settings… on an R3D NE clip is a Camera Raw panel: every control in its three
/// columns with an automation id, the three buttons; a new project's clip decodes using the
/// project's settings until Decode Using ▸ Clip; a typed value is applied on OK as that clip's
/// setting, a set value gets an As Shot button that puts it back, and the control channel reads
/// and sets the same values through `clip.sourceSettings`.
#[test]
fn source_settings_camera_raw_panel_sets_and_resets_the_clip_settings() {
    use filmcraft_media::{DecodeUsing, Hundredths, RawGamma};
    let mut d = Driver::new();
    let id = add_r3d(&mut d, "DSC_0003.R3D", "/footage/DSC_0003.R3D");
    d.exec("project.select", json!({"items": [id]}));
    let develop = |d: &mut Driver| d.app().session.project.item(filmcraft_engine::project::ItemId(id)).unwrap().as_media().unwrap().interpret.develop;
    let r = d.menu("clip.sourceSettings");
    assert_eq!(r["dialog"], "sourceSettings", "{r}");
    for key in [
        "quality",
        "decodeUsing",
        "colorScience",
        "colorSpace",
        "gamma",
        "blendType",
        "blendBias",
        "metadataCurves",
        "creativeLut",
        "cdl",
        "iso",
        "toneMap",
        "rolloff",
        "hdrPeakNits",
        "chromaNoiseReduction",
        "flashingPixel",
        "kelvin",
        "tint",
        "exposure",
        "updateRmd",
        "useChanges",
        "useSettings",
        "ok",
    ] {
        assert!(d.has(&format!("sourceSettings.{key}")), "{key} missing: {:?}", d.ok("ui.elements", json!({"prefix": "sourceSettings."})));
    }
    let dialog = d.app().ui.extras.dialog.clone().unwrap();
    assert_eq!((dialog.params["decodeUsing"].as_str(), dialog.params["gamma"].as_str()), (Some("project"), Some("bt1886")), "the project's standard");
    assert!(!d.has("sourceSettings.kelvin.asShot"), "nothing is set yet, so nothing to put back");
    // Decode Using ▸ Clip: the clip gets settings of its own, from the project's
    d.ok("ui.set", json!({"menuDialog": {"decodeUsing": "clip"}}));
    d.frames(3);
    assert_eq!(develop(&mut d).map(|x| (x.decode_using, x.gamma)), Some((DecodeUsing::Clip, RawGamma::Bt1886)));
    // type a colour temperature into the Color Temp field and OK: the clip develops at 4300 K
    d.click("sourceSettings.kelvin");
    d.ok("ui.key", json!({"key": "Cmd+A"}));
    d.ok("ui.type", json!({"text": "4300"}));
    d.ok("ui.key", json!({"key": "Enter"}));
    d.frames(3);
    assert!(d.has("sourceSettings.kelvin.asShot"), "a set value gets its As Shot button: {:?}", d.ok("ui.elements", json!({"prefix": "sourceSettings."})));
    d.click("sourceSettings.ok");
    assert_eq!(develop(&mut d).map(|x| (x.kelvin, x.iso, x.tint, x.exposure)), Some((Some(4300), None, None, None)));
    // the control channel sets the rest; reopened, the dialog shows them set and puts them back
    d.exec("clip.sourceSettings", json!({"iso": 3200, "tint": -12.5, "exposure": 0.5}));
    assert_eq!(develop(&mut d).map(|x| (x.kelvin, x.iso, x.tint, x.exposure)), Some((Some(4300), Some(3200), Some(Hundredths(-1250)), Some(Hundredths(50)))));
    d.menu("clip.sourceSettings");
    for key in ["kelvin", "tint", "exposure"] {
        assert!(d.has(&format!("sourceSettings.{key}.asShot")), "{key}");
        d.click(&format!("sourceSettings.{key}.asShot"));
        d.frames(2);
        assert!(!d.has(&format!("sourceSettings.{key}.asShot")), "{key}");
    }
    d.click("sourceSettings.ok");
    assert_eq!(develop(&mut d).map(|x| (x.kelvin, x.iso, x.tint, x.exposure)), Some((None, Some(3200), None, None)));
    let r = d.exec("clip.sourceSettings", json!({}));
    assert_eq!(r["develop"]["kelvin"], Value::Null, "{r}");
    d.exec("edit.undo", json!({}));
    assert_eq!(develop(&mut d).and_then(|x| x.kelvin), Some(4300), "one undo step");
}

/// Clip ▸ Source Settings… previews live: a changed control is applied to the
/// clip while the dialog is open (the monitors develop the frame with it before OK), every change
/// of one dialog shares one undo step, Cancel (and Escape) puts the opening values back and leaves
/// no undo step, OK keeps them so one Undo restores the opening values, and a hostile value set
/// over the control channel is refused with the error in the dialog, the clip untouched.
#[test]
fn source_settings_dialog_previews_live_and_cancel_or_undo_revert_in_one_step() {
    use filmcraft_media::Hundredths;
    let mut d = Driver::new();
    let ocean = d.item_named("Ocean_Sunset.mp4");
    let id = {
        let app = d.app();
        let mut p = (*app.session.project).clone();
        let mut clip = p.item(filmcraft_engine::project::ItemId(ocean)).unwrap().as_media().unwrap().clone();
        clip.media = filmcraft_engine::project::MediaRef::File { path: "/footage/DSC_0005.R3D".into() };
        clip.info.video.as_mut().unwrap().codec = "R3D NE".into();
        let id = p.add_item("DSC_0005.R3D", Label::Iris, ItemKind::Media(clip), None);
        app.session.project = std::sync::Arc::new(p);
        id.0
    };
    d.exec("project.select", json!({"items": [id]}));
    d.exec("clip.sourceSettings", json!({"iso": 800}));
    let develop = |d: &mut Driver| d.app().session.project.item(filmcraft_engine::project::ItemId(id)).unwrap().as_media().unwrap().interpret.develop;
    let steps = |d: &mut Driver| d.app().session.history.undo.len();
    let opened = (develop(&mut d), steps(&mut d));
    // a drag of ISO and exposure: each change reaches the clip before OK, all of them one undo step
    d.menu("clip.sourceSettings");
    d.ok("ui.set", json!({"menuDialog": {"iso": 3200}}));
    d.frames(3);
    assert_eq!((develop(&mut d).unwrap().iso, steps(&mut d)), (Some(3200), opened.1 + 1), "applied live");
    d.ok("ui.set", json!({"menuDialog": {"iso": 6400}}));
    d.frames(3);
    d.ok("ui.set", json!({"menuDialog": {"exposure": 1.5}}));
    d.frames(3);
    let x = develop(&mut d).unwrap();
    assert_eq!((x.iso, x.exposure, steps(&mut d)), (Some(6400), Some(Hundredths(150)), opened.1 + 1), "one undo step for the dialog");
    assert!(d.app().ui.extras.source_preview.is_some());
    // a hostile value over the control channel (an ISO the decoder has not got; the number fields
    // keep their values within the decoder's ranges themselves): refused, shown, the clip as it was
    d.ok("ui.set", json!({"menuDialog": {"iso": 1250}}));
    d.frames(3);
    let dialog = d.app().ui.extras.dialog.clone().unwrap();
    assert!(dialog.error.to_ascii_lowercase().contains("iso"), "{}", dialog.error);
    assert_eq!((develop(&mut d).unwrap().iso, steps(&mut d)), (Some(6400), opened.1 + 1), "refused without applying");
    d.ok("ui.set", json!({"menuDialog": {"iso": 12800, "kelvin": 4300}}));
    d.frames(3);
    assert_eq!((develop(&mut d).unwrap().iso, develop(&mut d).unwrap().kelvin), (Some(12800), Some(4300)));
    assert!(d.app().ui.extras.dialog.clone().unwrap().error.is_empty(), "a good value clears the error");
    // Cancel: the opening values are back and the history is as before the dialog
    d.click("sourceSettings.cancel");
    let extras = d.app().ui.extras.clone();
    assert!(extras.dialog.is_none() && extras.source_preview.is_none(), "{:?} {:?}", extras.dialog, extras.source_preview);
    assert_eq!((develop(&mut d), steps(&mut d)), opened, "Cancel reverts without a trace");
    // Escape does the same
    d.menu("clip.sourceSettings");
    d.ok("ui.set", json!({"menuDialog": {"exposure": -2.0}}));
    d.frames(3);
    assert_eq!(develop(&mut d).unwrap().exposure, Some(Hundredths(-200)));
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(3);
    assert!(d.app().ui.extras.dialog.is_none());
    assert_eq!((develop(&mut d), steps(&mut d)), opened, "Escape reverts too");
    // OK keeps the previewed values as one undo step; Undo restores the opening values in one go
    d.menu("clip.sourceSettings");
    d.ok("ui.set", json!({"menuDialog": {"iso": 1600}}));
    d.frames(3);
    d.ok("ui.set", json!({"menuDialog": {"gamma": "srgb", "toneMap": "medium"}}));
    d.frames(3);
    d.click("sourceSettings.ok");
    assert!(d.app().ui.extras.dialog.is_none() && d.app().ui.extras.source_preview.is_none());
    let x = develop(&mut d).unwrap();
    assert_eq!((x.iso, x.gamma.id(), steps(&mut d)), (Some(1600), "srgb", opened.1 + 1));
    d.exec("edit.undo", json!({}));
    assert_eq!((develop(&mut d), steps(&mut d)), (opened.0, opened.1), "one Undo takes the whole dialog back");
    d.exec("edit.redo", json!({}));
    assert_eq!(develop(&mut d).unwrap().iso, Some(1600));
}

/// The Camera Raw panel's buttons and the project's Camera RAW settings: Use Changes copies the
/// shown clip's settings to every selected clip and Use Settings puts it back to what it records,
/// both inside the dialog's one undo step (Cancel takes them back); Update RMD without a decoder
/// says why in the dialog; File ▸ Project Settings ▸ Camera RAW changes what the clips that
/// decode using the project develop with, a gamma chosen alone bringing its colour space along.
#[test]
fn camera_raw_buttons_and_the_project_settings_tab() {
    use filmcraft_media::{DecodeUsing, RawColorSpace, RawDevelop, RawGamma, RawToneMap};
    let mut d = Driver::new();
    let a = add_r3d(&mut d, "A.R3D", "/footage/A.R3D");
    let b = add_r3d(&mut d, "B.R3D", "/footage/B.R3D");
    let develop = |d: &mut Driver, id: u64| d.app().session.project.item(filmcraft_engine::project::ItemId(id)).unwrap().as_media().unwrap().interpret.develop;
    d.exec("clip.sourceSettings", json!({"items": [a], "iso": 1600, "gamma": "srgb", "toneMap": "high"}));
    let opened = (develop(&mut d, a), develop(&mut d, b), d.app().session.history.undo.len());
    d.exec("project.select", json!({"items": [a, b]}));
    d.menu("clip.sourceSettings");
    d.click("sourceSettings.useChanges");
    d.frames(2);
    assert_eq!(develop(&mut d, b), develop(&mut d, a), "B has A's settings");
    assert_eq!(develop(&mut d, b).map(|x| (x.iso, x.gamma)), Some((Some(1600), RawGamma::Srgb)));
    d.click("sourceSettings.useSettings");
    d.frames(2);
    let x = develop(&mut d, a).unwrap();
    assert_eq!((x.decode_using, x.iso, x.tone_map), (DecodeUsing::Clip, None, RawToneMap::Clip), "as recorded");
    assert_eq!(d.app().ui.extras.dialog.clone().unwrap().params["iso"], Value::Null, "the panel shows it");
    // no decoder (the file does not exist): Update RMD says why, nothing changes
    d.click("sourceSettings.updateRmd");
    d.frames(2);
    let error = d.app().ui.extras.dialog.clone().unwrap().error;
    assert!(error.contains("RMD not written"), "{error}");
    d.click("sourceSettings.cancel");
    assert_eq!((develop(&mut d, a), develop(&mut d, b), d.app().session.history.undo.len()), opened, "Cancel takes it all back");
    // the project's Camera RAW tab
    let r = d.menu("file.projectSettings.cameraRaw");
    assert_eq!(r["dialog"], "projectSettings", "{r}");
    for key in
        ["tab.cameraRaw", "cameraRaw.quality", "cameraRaw.colorSpace", "cameraRaw.gamma", "cameraRaw.toneMap", "cameraRaw.kelvin", "cameraRaw.standard", "ok"]
    {
        assert!(d.has(&format!("projectSettings.{key}")), "{key} missing: {:?}", d.ok("ui.elements", json!({"prefix": "projectSettings."})));
    }
    assert!(!d.has("projectSettings.cameraRaw.decodeUsing"), "Decode Using is a clip setting");
    let mut cr = d.app().ui.extras.dialog.clone().unwrap().params["cameraRaw"].clone();
    assert_eq!(cr["gamma"], "bt1886");
    cr["gamma"] = json!("log3g10");
    d.ok("ui.set", json!({"menuDialog": {"cameraRaw": cr}}));
    d.frames(2);
    assert_eq!(d.app().ui.extras.dialog.clone().unwrap().params["cameraRaw"]["colorSpace"], "redWideGamutRgb", "the gamma brought its colour space");
    d.click("projectSettings.ok");
    let st = d.app().session.project.settings.camera_raw;
    assert_eq!((st.color_space, st.gamma), (RawColorSpace::RedWideGamutRgb, RawGamma::Log3G10));
    assert_eq!(d.exec("media.colorInfo", json!({"item": b}))["effective"], "log3g10-rwg", "B decodes using the project");
    assert_eq!(d.exec("media.colorInfo", json!({"item": a}))["effective"], "linear709", "A has settings of its own (sRGB, Rec. 709)");
    d.exec("edit.undo", json!({}));
    assert_eq!(d.app().session.project.settings.camera_raw, RawDevelop::STANDARD);
}
