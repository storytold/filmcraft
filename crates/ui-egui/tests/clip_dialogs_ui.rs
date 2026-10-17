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

    fn audio_fixture(&mut self, widths: &[u32], stream: usize, mapping: Vec<u16>) -> filmcraft_engine::project::ClipId {
        use filmcraft_engine::project::TrackKind;
        use filmcraft_engine::time::{Tick, TimeRange};
        let s = &mut self.app().session;
        let template = s.active_sequence().unwrap().audio_tracks[0].items[0].clone();
        let mut media = s.project.resolve_media(template.item).unwrap().1.clone();
        let audio = media.info.audio_streams[0].clone();
        media.info.audio_streams = widths.iter().map(|channels| filmcraft_media::AudioStreamInfo { channels: *channels, ..audio.clone() }).collect();
        media.interpret.audio_channels = None;
        let sequence = s.state.active_sequence.unwrap();
        let rate = s.sequence_rate();
        let p = std::sync::Arc::make_mut(&mut s.project);
        let source = p.add_item("Channel fixture", Label::Iris, ItemKind::Media(media), None);
        let start = p.sequence(sequence).unwrap().audio_tracks[0].items.iter().map(|clip| clip.end()).max().unwrap_or(Tick::ZERO);
        let mut clip = p.make_track_item(source, TrackKind::Audio, start, TimeRange::new(Tick::ZERO, Tick::from_seconds_f64(1.0)), rate).unwrap();
        clip.audio_stream = stream;
        clip.source_channels = mapping;
        let id = clip.id;
        p.sequence_mut(sequence).unwrap().audio_tracks[0].items.push(clip);
        id
    }

    fn audio_clip(&mut self, id: filmcraft_engine::project::ClipId) -> filmcraft_engine::project::TrackItem {
        self.app().session.active_sequence().unwrap().find_item(id).unwrap().1.clone()
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

#[test]
fn audio_channels_project_valid_extra_rows_survive_untouched_ok() {
    use filmcraft_engine::project::{AudioChannelMap, AudioChannels};
    let mut d = Driver::new();
    let clip = d.audio_fixture(&[2], 0, vec![]);
    let item = d.audio_clip(clip).item;
    let map = AudioChannelMap { format: AudioChannels::Mono, clips: vec![vec![1, 0], vec![1], vec![0]] };
    std::sync::Arc::make_mut(&mut d.app().session.project).item_mut(item).unwrap().as_media_mut().unwrap().interpret.audio_channels = Some(map.clone());
    d.exec("project.select", json!({"items": [item.0]}));
    let before = d.app().session.project.clone();
    let undo = d.app().session.history.undo.len();
    let redo = d.app().session.history.redo.len();
    d.menu("clip.audioChannels");
    assert_eq!(d.app().ui.clip_dialog.as_ref().unwrap().params["clips"], json!(map.clips), "drawing must not clamp valid stored rows");
    d.click("audioChannels.ok");
    assert!(d.app().ui.clip_dialog.is_none());
    assert_eq!(d.app().session.project, before);
    assert_eq!(d.app().session.history.undo.len(), undo);
    assert_eq!(d.app().session.history.redo.len(), redo);
}

#[test]
fn audio_channels_malformed_drafts_are_preserved_until_explicit_correction() {
    for project in [false, true] {
        for value in [json!([0, "bad"]), json!(null), json!({}), json!(1)] {
            let mut d = Driver::new();
            let clip = d.audio_fixture(&[2], 0, vec![1]);
            let item = d.audio_clip(clip).item;
            if project {
                d.exec("project.select", json!({"items": [item.0]}));
            } else {
                d.app().session.state.project_selection.clear();
                d.exec("timeline.select", json!({"clips": [clip.0]}));
            }
            d.menu("clip.audioChannels");
            let key = if project { "clips" } else { "channels" };
            let supplied = if project { json!([value]) } else { value };
            d.ok("ui.set", json!({"clipDialog": {key: supplied.clone()}}));
            let before = d.app().session.project.clone();
            let undo = d.app().session.history.undo.len();
            let redo = d.app().session.history.redo.len();
            d.frames(3);
            assert_eq!(d.app().ui.clip_dialog.as_ref().unwrap().params[key], supplied, "rendering must retain malformed input for validation");
            d.click("audioChannels.ok");
            assert!(d.app().ui.clip_dialog.is_some());
            assert!(!d.app().ui.clip_dialog.as_ref().unwrap().error.is_empty());
            assert_eq!(d.app().session.project, before);
            assert_eq!(d.app().session.history.undo.len(), undo);
            assert_eq!(d.app().session.history.redo.len(), redo);
            d.click("audioChannels.cancel");
            assert!(d.app().ui.clip_dialog.is_none());
        }
    }
}

#[test]
fn audio_channels_wide_dialog_keeps_actions_visible_with_larger_text() {
    use filmcraft_ui_egui::i18n::Language;
    let mut d = Driver::new();
    d.app().ui.language = Language::Fr;
    d.harness.set_size(egui::vec2(900.0, 700.0));
    d.frames(4);
    d.harness.ctx.global_style_mut(|style| {
        for font in style.text_styles.values_mut() {
            font.size *= 1.5;
        }
    });
    let clip = d.audio_fixture(&[64], 0, vec![1, 99, 0]);
    d.app().session.state.project_selection.clear();
    d.exec("timeline.select", json!({"clips": [clip.0]}));
    let before = d.app().session.project.clone();
    d.menu("clip.audioChannels");
    let controls = d.ok("ui.elements", json!({"prefix": "audioChannels."}));
    for id in ["audioChannels.repair", "audioChannels.cancel", "audioChannels.ok"] {
        let control = controls.as_array().unwrap().iter().find(|e| e["id"] == id).unwrap();
        let rect = &control["rect"];
        assert!(rect[1].as_f64().unwrap() >= 0.0, "{id} above the viewport: {rect}");
        assert!(rect[1].as_f64().unwrap() + rect[3].as_f64().unwrap() <= 700.0, "{id} below the viewport: {rect}");
    }
    d.click("audioChannels.repair");
    assert!(d.app().ui.clip_dialog.is_none());
    assert_eq!(d.audio_clip(clip).source_channels, vec![1, 0]);
    d.exec("edit.undo", json!({}));
    assert_eq!(d.app().session.project, before);
    d.menu("clip.audioChannels");
    d.click("audioChannels.cancel");
    assert_eq!(d.app().session.project, before);
}

#[test]
fn audio_channels_oversized_dialog_contexts_return_recoverable_errors() {
    for project in [false, true] {
        let mut d = Driver::new();
        let clip = d.audio_fixture(&[65], 0, vec![0]);
        let item = d.audio_clip(clip).item;
        if project {
            d.exec("project.select", json!({"items": [item.0]}));
        } else {
            d.app().session.state.project_selection.clear();
            d.exec("timeline.select", json!({"clips": [clip.0]}));
        }
        let before = d.app().session.project.clone();
        d.menu("clip.audioChannels");
        assert!(!d.app().ui.clip_dialog.as_ref().unwrap().error.is_empty());
        assert!(!d.has("audioChannels.clip.0.ch.0"));
        d.click("audioChannels.cancel");
        assert_eq!(d.app().session.project, before);
        d.exec("clip.audioChannels", if project { json!({"format": "mono", "clips": [[64]]}) } else { json!({"channels": [64]}) });
    }

    let mut d = Driver::new();
    let clip = d.audio_fixture(&[2], 0, vec![]);
    let item = d.audio_clip(clip).item;
    std::sync::Arc::make_mut(&mut d.app().session.project).item_mut(item).unwrap().as_media_mut().unwrap().interpret.audio_channels =
        Some(filmcraft_engine::project::AudioChannelMap { format: filmcraft_engine::project::AudioChannels::Mono, clips: vec![vec![0]; 2049] });
    d.exec("project.select", json!({"items": [item.0]}));
    let before = d.app().session.project.clone();
    d.menu("clip.audioChannels");
    assert!(!d.app().ui.clip_dialog.as_ref().unwrap().error.is_empty());
    assert!(!d.has("audioChannels.clip.0.ch.0"));
    d.click("audioChannels.cancel");
    assert_eq!(d.app().session.project, before);
}

#[test]
fn audio_channels_recovery_automation_labels_follow_the_interface_language() {
    use filmcraft_ui_egui::i18n::Language;
    let mut d = Driver::new();
    let clip = d.audio_fixture(&[2], 0, vec![99]);
    d.app().session.state.project_selection.clear();
    d.app().ui.language = Language::Es;
    d.exec("timeline.select", json!({"clips": [clip.0]}));
    d.menu("clip.audioChannels");
    let elements = d.ok("ui.elements", json!({"prefix": "audioChannels."}));
    for (id, label) in [("audioChannels.repair", "Aplicar asignación corregida"), ("audioChannels.default", "Usar canales predeterminados")] {
        let element = elements.as_array().unwrap().iter().find(|element| element["id"] == id).unwrap();
        assert_eq!(element["label"], label);
    }
    d.click("audioChannels.repair");
    assert!(d.app().ui.clip_dialog.is_none());
}

#[test]
fn audio_channels_direct_legacy_repair_preserves_order_and_history() {
    let mut d = Driver::new();
    let clip = d.audio_fixture(&[2], 0, vec![1, 99, 0]);
    d.app().session.state.project_selection.clear();
    d.exec("timeline.select", json!({"clips": [clip.0]}));
    let before = d.app().session.project.clone();
    let selection = d.app().session.state.selection.clone();
    let playhead = d.app().session.playhead();
    let undo = d.app().session.history.undo.len();
    d.menu("clip.audioChannels");
    assert_eq!(d.app().ui.clip_dialog.as_ref().unwrap().params["channels"], json!([1, 0]));
    assert!(!d.app().ui.clip_dialog.as_ref().unwrap().error.is_empty());
    d.click("audioChannels.repair");
    assert!(d.app().ui.clip_dialog.is_none(), "explicit corrected-map action accepts and closes");
    assert_eq!(d.audio_clip(clip).source_channels, vec![1, 0], "retain valid left/right order");
    assert_eq!(d.app().session.state.selection, selection);
    assert_eq!(d.app().session.playhead(), playhead);
    assert_eq!(d.app().session.history.undo.len(), undo + 1);
    let after = d.app().session.project.clone();
    d.exec("edit.undo", json!({}));
    assert_eq!(d.app().session.project, before);
    d.exec("edit.redo", json!({}));
    assert_eq!(d.app().session.project, after);
    d.menu("clip.audioChannels");
    assert!(!d.has("audioChannels.repair"), "valid maps need no repair hurdle");
    d.click("audioChannels.ok");
    assert!(d.app().ui.clip_dialog.is_none());
    assert_eq!(d.app().session.project, after);
}

#[test]
fn audio_channels_legacy_map_can_choose_default_without_checkbox_bookkeeping() {
    let mut d = Driver::new();
    let clip = d.audio_fixture(&[2], 0, vec![99]);
    d.app().session.state.project_selection.clear();
    d.exec("timeline.select", json!({"clips": [clip.0]}));
    let before = d.app().session.project.clone();
    let undo = d.app().session.history.undo.len();
    d.menu("clip.audioChannels");
    d.click("audioChannels.default");
    d.click("audioChannels.ok");
    assert!(d.app().ui.clip_dialog.is_none());
    assert!(d.audio_clip(clip).source_channels.is_empty());
    assert_eq!(d.app().session.history.undo.len(), undo + 1);
    d.exec("edit.undo", json!({}));
    assert_eq!(d.app().session.project, before);
}

#[test]
fn audio_channels_project_repair_retains_valid_ordered_rows() {
    use filmcraft_engine::project::{AudioChannelMap, AudioChannels};
    let mut d = Driver::new();
    let clip = d.audio_fixture(&[2], 0, vec![]);
    let item = d.audio_clip(clip).item;
    std::sync::Arc::make_mut(&mut d.app().session.project).item_mut(item).unwrap().as_media_mut().unwrap().interpret.audio_channels =
        Some(AudioChannelMap { format: AudioChannels::Stereo, clips: vec![vec![1, 99, 0], vec![99], vec![1]] });
    d.exec("project.select", json!({"items": [item.0]}));
    let before = d.app().session.project.clone();
    let undo = d.app().session.history.undo.len();
    d.menu("clip.audioChannels");
    assert_eq!(d.app().ui.clip_dialog.as_ref().unwrap().params["clips"], json!([[1, 0], [1]]));
    d.click("audioChannels.repair");
    assert!(d.app().ui.clip_dialog.is_none());
    let map = d.app().session.project.item(item).unwrap().as_media().unwrap().interpret.audio_channels.as_ref().unwrap();
    assert_eq!(map.clips, vec![vec![1, 0], vec![1]]);
    assert_eq!(d.app().session.history.undo.len(), undo + 1);
    d.exec("edit.undo", json!({}));
    assert_eq!(d.app().session.project, before);
}

#[test]
fn audio_channels_mono_unchanged_ok_preserves_mapping_and_reopens() {
    let mut d = Driver::new();
    let clip = d.app().session.active_sequence().unwrap().audio_tracks[0].items[0].clone();
    let source = d.app().session.project.resolve_media(clip.item).unwrap().0;
    assert_eq!(clip.audio_stream, 0);
    std::sync::Arc::make_mut(&mut d.app().session.project).item_mut(source).unwrap().as_media_mut().unwrap().info.audio_streams[0].channels = 1;
    d.app().session.state.project_selection.clear();
    d.exec("timeline.select", json!({"clips": [clip.id.0]}));
    for mapping in [vec![], vec![0]] {
        let sequence = d.app().session.state.active_sequence.unwrap();
        std::sync::Arc::make_mut(&mut d.app().session.project).sequence_mut(sequence).unwrap().find_item_mut(clip.id).unwrap().1.source_channels =
            mapping.clone();
        let before = d.app().session.project.clone();
        let selection = d.app().session.state.selection.clone();
        let playhead = d.app().session.playhead();
        d.menu("clip.audioChannels");
        assert!(d.app().ui.clip_dialog.is_some());
        d.click("audioChannels.ok");
        assert!(d.app().ui.clip_dialog.is_none(), "unchanged mono OK must close: {:?}", d.app().ui.clip_dialog);
        assert_eq!(d.app().session.project, before, "unchanged OK preserves the mapping and all timeline work");
        assert_eq!(d.app().session.state.selection, selection);
        assert_eq!(d.app().session.playhead(), playhead);
        d.menu("clip.audioChannels");
        assert_eq!(d.app().ui.clip_dialog.as_ref().unwrap().params["channels"], json!(mapping));
        assert_eq!(d.app().ui.clip_dialog.as_ref().unwrap().info["channels"], 1);
        assert!(d.has("audioChannels.clip.0.ch.0"));
        assert!(!d.has("audioChannels.clip.0.ch.1"), "mono must not offer an absent second channel");
        d.click("audioChannels.cancel");
        assert!(d.app().ui.clip_dialog.is_none());
    }
}

#[test]
fn audio_channels_dialog_revalidates_selection_locks_and_missing_targets() {
    let mut d = Driver::new();
    let first = d.audio_fixture(&[1], 0, vec![]);
    let second = d.audio_fixture(&[6], 0, vec![5, 2]);
    d.app().session.state.project_selection.clear();
    d.exec("timeline.select", json!({"clips": [first.0]}));
    d.menu("clip.audioChannels");
    d.exec("timeline.select", json!({"clips": [second.0]}));
    let before = d.app().session.project.clone();
    let undo = d.app().session.history.undo.len();
    let redo = d.app().session.history.redo.len();
    d.click("audioChannels.ok");
    assert!(d.app().ui.clip_dialog.as_ref().unwrap().error.contains("audio targets changed"));
    assert_eq!(d.app().session.project, before);
    assert_eq!(d.app().session.history.undo.len(), undo);
    assert_eq!(d.app().session.history.redo.len(), redo);
    assert_eq!(d.app().session.state.selection, vec![second]);
    d.click("audioChannels.cancel");
    d.menu("clip.audioChannels");
    assert_eq!(d.app().ui.clip_dialog.as_ref().unwrap().info["channels"], 6);
    d.click("audioChannels.ok");
    assert!(d.app().ui.clip_dialog.is_none());
    assert_eq!(d.app().session.history.undo.len(), undo);
    assert_eq!(d.app().session.history.redo.len(), redo);

    for missing in [false, true] {
        d.exec("timeline.select", json!({"clips": [first.0]}));
        d.menu("clip.audioChannels");
        let sequence = d.app().session.state.active_sequence.unwrap();
        let p = std::sync::Arc::make_mut(&mut d.app().session.project);
        let track = &mut p.sequence_mut(sequence).unwrap().audio_tracks[0];
        if missing {
            track.items.retain(|clip| clip.id != first);
        } else {
            track.locked = true;
        }
        let before = d.app().session.project.clone();
        let undo = d.app().session.history.undo.len();
        d.click("audioChannels.ok");
        assert!(!d.app().ui.clip_dialog.as_ref().unwrap().error.is_empty());
        assert_eq!(d.app().session.project, before);
        assert_eq!(d.app().session.history.undo.len(), undo);
        if !missing {
            d.click("audioChannels.clip.0.ch.0");
            d.click("audioChannels.ok");
            assert!(d.app().ui.clip_dialog.is_some(), "a changed submit cannot bypass the lock either");
            assert_eq!(d.app().session.project, before);
            let error = d.call("engine.execute", json!({"command": "clip.audioChannels", "params": {"channels": [0]}}));
            assert_eq!(error["ok"], false, "the engine and dialog share locked-target validation");
            std::sync::Arc::make_mut(&mut d.app().session.project).sequence_mut(sequence).unwrap().audio_tracks[0].locked = false;
        }
        d.click("audioChannels.cancel");
        assert!(d.app().ui.clip_dialog.is_none());
    }
}

#[test]
fn audio_channels_project_dialog_uses_checked_common_counts_and_preserves_maps() {
    let mut d = Driver::new();
    let mono = d.audio_fixture(&[1], 0, vec![]);
    let surround = d.audio_fixture(&[6], 0, vec![]);
    let items = [d.audio_clip(mono).item, d.audio_clip(surround).item];
    d.exec("project.select", json!({"items": items.iter().map(|i| i.0).collect::<Vec<_>>()}));
    let before = d.app().session.project.clone();
    let undo = d.app().session.history.undo.len();
    d.menu("clip.audioChannels");
    assert_eq!(d.app().ui.clip_dialog.as_ref().unwrap().info["channels"], 1);
    assert_eq!(d.app().ui.clip_dialog.as_ref().unwrap().params["clips"], json!([[0]]));
    assert!(d.has("audioChannels.clip.0.ch.0"));
    assert!(!d.has("audioChannels.clip.0.ch.1"));
    d.click("audioChannels.ok");
    assert!(d.app().ui.clip_dialog.is_none());
    assert_eq!(d.app().session.project, before);
    assert_eq!(d.app().session.history.undo.len(), undo);
    d.menu("clip.audioChannels");
    d.click("audioChannels.format.mono");
    d.click("audioChannels.ok");
    assert!(d.app().ui.clip_dialog.is_none());
    for item in items {
        let map = d.app().session.project.item(item).unwrap().as_media().unwrap().interpret.audio_channels.as_ref().unwrap();
        assert_eq!(map.clips, vec![vec![0]], "offered maps must be valid for every selected source");
    }
    for channels in [0, 65_536] {
        std::sync::Arc::make_mut(&mut d.app().session.project).item_mut(items[0]).unwrap().as_media_mut().unwrap().info.audio_streams[0].channels = channels;
        let before = d.app().session.project.clone();
        d.menu("clip.audioChannels");
        assert!(!d.has("audioChannels.clip.0.ch.0"));
        assert!(d.has("audioChannels.cancel"));
        d.click("audioChannels.ok");
        assert!(!d.app().ui.clip_dialog.as_ref().unwrap().error.is_empty());
        assert_eq!(d.app().session.project, before);
        d.click("audioChannels.cancel");
    }
}

#[test]
fn audio_channels_dialog_uses_stored_maps_and_selected_stream_bounds() {
    let mut d = Driver::new();
    for (widths, stream, mapping) in [(vec![2], 0, vec![1, 0]), (vec![6], 0, vec![5, 2]), (vec![6, 1], 1, vec![])] {
        let count = widths[stream];
        let clip = d.audio_fixture(&widths, stream, mapping.clone());
        d.app().session.state.project_selection.clear();
        d.exec("timeline.select", json!({"clips": [clip.0]}));
        let before = d.app().session.project.clone();
        let selection = d.app().session.state.selection.clone();
        d.menu("clip.audioChannels");
        assert_eq!(d.app().ui.clip_dialog.as_ref().unwrap().params["channels"], json!(mapping));
        assert_eq!(d.app().ui.clip_dialog.as_ref().unwrap().info["channels"], count);
        for c in 0..count {
            assert!(d.has(&format!("audioChannels.clip.0.ch.{c}")));
        }
        assert!(!d.has(&format!("audioChannels.clip.0.ch.{count}")));
        d.click("audioChannels.ok");
        assert!(d.app().ui.clip_dialog.is_none(), "unchanged valid mapping closes");
        assert_eq!(d.app().session.project, before);
        assert_eq!(d.app().session.state.selection, selection);
        d.menu("clip.audioChannels");
        d.click("audioChannels.clip.0.ch.0");
        let changed: Vec<u16> = serde_json::from_value(d.app().ui.clip_dialog.as_ref().unwrap().params["channels"].clone()).unwrap();
        assert!(changed.iter().all(|c| u32::from(*c) < count));
        d.click("audioChannels.ok");
        assert!(
            d.app().ui.clip_dialog.is_none(),
            "an offered changed mapping must submit: {:?}; selection: {:?}",
            d.harness.state().ui.clip_dialog,
            d.harness.state().session.state.selection
        );
        assert_eq!(d.audio_clip(clip).source_channels, changed);
        assert_eq!(d.app().session.state.selection, selection);
    }
}

#[test]
fn audio_channels_dialog_mixed_targets_preserve_each_map_and_offer_common_bounds() {
    let mut d = Driver::new();
    let mono = d.audio_fixture(&[1], 0, vec![]);
    let surround = d.audio_fixture(&[6], 0, vec![5, 2]);
    d.app().session.state.project_selection.clear();
    d.exec("timeline.select", json!({"clips": [mono.0, surround.0]}));
    let before = d.app().session.project.clone();
    let selection = d.app().session.state.selection.clone();
    d.menu("clip.audioChannels");
    assert_eq!(d.app().ui.clip_dialog.as_ref().unwrap().info["channels"], 1);
    assert!(d.has("audioChannels.clip.0.ch.0"));
    assert!(!d.has("audioChannels.clip.0.ch.1"));
    d.click("audioChannels.ok");
    assert!(d.app().ui.clip_dialog.is_none());
    assert_eq!(d.app().session.project, before, "unchanged OK preserves heterogeneous maps, including []");
    assert_eq!(d.app().session.state.selection, selection);
    d.menu("clip.audioChannels");
    d.click("audioChannels.clip.0.ch.0");
    let changed: Vec<u16> = serde_json::from_value(d.app().ui.clip_dialog.as_ref().unwrap().params["channels"].clone()).unwrap();
    assert!(changed.iter().all(|c| *c == 0));
    d.click("audioChannels.ok");
    assert!(
        d.app().ui.clip_dialog.is_none(),
        "mixed changed submit: {:?}; selection: {:?}",
        d.harness.state().ui.clip_dialog,
        d.harness.state().session.state.selection
    );
    assert_eq!(d.audio_clip(mono).source_channels, changed);
    assert_eq!(d.audio_clip(surround).source_channels, changed);
    assert_eq!(d.app().session.state.selection, selection);
}

#[test]
fn audio_channels_dialog_resolves_subclips_and_nested_master_widths() {
    use filmcraft_engine::project::{AudioChannels, TrackKind};
    use filmcraft_engine::time::{Tick, TimeRange};
    let mut d = Driver::new();
    let subclip = d.audio_fixture(&[2, 6], 1, vec![5]);
    let s = &mut d.app().session;
    let sequence = s.state.active_sequence.unwrap();
    let parent = s.active_sequence().unwrap().find_item(subclip).unwrap().1.item;
    let p = std::sync::Arc::make_mut(&mut s.project);
    let sub = p.add_item(
        "Audio subclip",
        Label::Iris,
        ItemKind::Subclip { parent, range: TimeRange::new(Tick::ZERO, Tick::from_seconds_f64(1.0)), restrict_trims: true },
        None,
    );
    p.sequence_mut(sequence).unwrap().find_item_mut(subclip).unwrap().1.item = sub;
    let mut cases = vec![(subclip, 6)];
    for master in [AudioChannels::Mono, AudioChannels::Surround51] {
        let s = &mut d.app().session;
        let rate = s.sequence_rate();
        let p = std::sync::Arc::make_mut(&mut s.project);
        let nested = p.new_sequence("Nested channels", filmcraft_engine::project::SequenceSettings { audio_master: master, ..Default::default() }, 0, 1, None);
        let width = filmcraft_render::audio::master_layout(p.sequence(nested).unwrap()).channels();
        let start = p.sequence(sequence).unwrap().audio_tracks[0].items.iter().map(|clip| clip.end()).max().unwrap_or(Tick::ZERO);
        let clip = p.make_track_item(nested, TrackKind::Audio, start, TimeRange::new(Tick::ZERO, Tick::from_seconds_f64(1.0)), rate).unwrap();
        let id = clip.id;
        p.sequence_mut(sequence).unwrap().audio_tracks[0].items.push(clip);
        cases.push((id, width));
    }
    for (clip, count) in cases {
        d.app().session.state.project_selection.clear();
        d.exec("timeline.select", json!({"clips": [clip.0]}));
        d.menu("clip.audioChannels");
        assert_eq!(d.app().ui.clip_dialog.as_ref().unwrap().info["channels"], count);
        assert!(d.has(&format!("audioChannels.clip.0.ch.{}", count - 1)));
        assert!(!d.has(&format!("audioChannels.clip.0.ch.{count}")));
        d.click("audioChannels.ok");
        assert!(d.app().ui.clip_dialog.is_none());
    }
}

#[test]
fn audio_channels_dialog_invalid_stream_metadata_keeps_a_recoverable_draft() {
    let mut d = Driver::new();
    for (width, stream) in [(0, 0), (2, 1)] {
        let clip = d.audio_fixture(&[width], stream, vec![]);
        d.app().session.state.project_selection.clear();
        d.exec("timeline.select", json!({"clips": [clip.0]}));
        let before = d.app().session.project.clone();
        d.menu("clip.audioChannels");
        assert!(d.has("audioChannels.cancel"));
        assert!(!d.has("audioChannels.clip.0.ch.0"), "no guessed channels for unavailable metadata");
        d.click("audioChannels.ok");
        let draft = d.app().ui.clip_dialog.as_ref().unwrap();
        assert!(!draft.error.is_empty());
        assert_eq!(d.app().session.project, before);
        d.click("audioChannels.cancel");
        assert!(d.app().ui.clip_dialog.is_none());
    }
}

#[test]
fn audio_channels_legacy_mono_maps_can_be_corrected_and_undone() {
    let mut d = Driver::new();
    for mapping in [vec![0, 1], vec![1]] {
        let clip = d.audio_fixture(&[1], 0, mapping.clone());
        d.app().session.state.project_selection.clear();
        d.exec("timeline.select", json!({"clips": [clip.0]}));
        let before = d.app().session.project.clone();
        let selection = d.app().session.state.selection.clone();
        let playhead = d.app().session.playhead();
        let undo = d.app().session.history.undo.len();
        d.menu("clip.audioChannels");
        d.click("audioChannels.ok");
        assert!(d.app().ui.clip_dialog.is_some(), "unchanged OK cannot silently normalize a legacy invalid map");
        assert!(!d.app().ui.clip_dialog.as_ref().unwrap().error.is_empty());
        assert_eq!(d.app().session.project, before);
        assert_eq!(d.app().session.history.undo.len(), undo);
        assert!(d.has("audioChannels.clip.0.ch.0"), "a legacy map must leave actual correction controls available: {:?}", d.app().ui.clip_dialog);
        assert!(!d.has("audioChannels.clip.0.ch.1"), "an absent source channel must not be offered");
        if d.app().ui.clip_dialog.as_ref().unwrap().params["channels"].as_array().unwrap().contains(&json!(0)) {
            d.click("audioChannels.clip.0.ch.0");
        }
        d.click("audioChannels.clip.0.ch.0");
        assert_eq!(d.app().ui.clip_dialog.as_ref().unwrap().params["channels"], json!([0]), "an edited replacement contains no invisible invalid channel");
        let sequence = d.app().session.state.active_sequence.unwrap();
        std::sync::Arc::make_mut(&mut d.app().session.project).sequence_mut(sequence).unwrap().audio_tracks[0].locked = true;
        let locked = d.app().session.project.clone();
        d.click("audioChannels.ok");
        assert!(d.app().ui.clip_dialog.is_some(), "a correction must still revalidate the track lock");
        assert_eq!(d.app().ui.clip_dialog.as_ref().unwrap().params["channels"], json!([0]), "failure retains the corrected draft");
        assert_eq!(d.app().session.project, locked);
        assert_eq!(d.app().session.history.undo.len(), undo);
        std::sync::Arc::make_mut(&mut d.app().session.project).sequence_mut(sequence).unwrap().audio_tracks[0].locked = false;
        d.click("audioChannels.ok");
        assert!(d.app().ui.clip_dialog.is_none(), "explicit correction must close after acceptance: {:?}", d.app().ui.clip_dialog);
        assert_eq!(d.audio_clip(clip).source_channels, vec![0]);
        assert_eq!(d.app().session.state.selection, selection);
        assert_eq!(d.app().session.playhead(), playhead);
        assert_eq!(d.app().session.history.undo.len(), undo + 1);
        d.exec("edit.undo", json!({}));
        assert_eq!(d.app().session.project, before, "Undo restores the complete original project, including its legacy map");
        assert_eq!(d.audio_clip(clip).source_channels, mapping);
        d.menu("clip.audioChannels");
        assert!(d.has("audioChannels.clip.0.ch.0"));
        d.click("audioChannels.cancel");
        assert!(d.app().ui.clip_dialog.is_none());
        assert_eq!(d.app().session.project, before, "Cancel also preserves the legacy map");
    }
}

#[test]
fn audio_channels_legacy_project_maps_can_be_corrected_atomically_and_undone() {
    use filmcraft_engine::project::{AudioChannelMap, AudioChannels};
    let mut d = Driver::new();
    let mono = d.audio_fixture(&[1], 0, vec![]);
    let stereo = d.audio_fixture(&[2], 0, vec![]);
    let items = [d.audio_clip(mono).item, d.audio_clip(stereo).item];
    std::sync::Arc::make_mut(&mut d.app().session.project).item_mut(items[0]).unwrap().as_media_mut().unwrap().interpret.audio_channels =
        Some(AudioChannelMap { format: AudioChannels::Stereo, clips: vec![vec![0, 1]] });
    d.exec("project.select", json!({"items": items.iter().map(|item| item.0).collect::<Vec<_>>()}));
    let before = d.app().session.project.clone();
    let selection = d.app().session.state.project_selection.clone();
    let undo = d.app().session.history.undo.len();
    d.menu("clip.audioChannels");
    d.click("audioChannels.ok");
    assert!(d.app().ui.clip_dialog.is_some(), "unchanged project OK must preserve and report the legacy map");
    assert_eq!(d.app().session.project, before);
    assert_eq!(d.app().session.history.undo.len(), undo);
    assert!(d.has("audioChannels.clip.0.ch.0"));
    assert!(!d.has("audioChannels.clip.0.ch.1"));
    d.click("audioChannels.clip.0.ch.0");
    d.click("audioChannels.clip.0.ch.0");
    assert_eq!(d.app().ui.clip_dialog.as_ref().unwrap().params["clips"], json!([[0]]));
    d.click("audioChannels.ok");
    assert!(d.app().ui.clip_dialog.is_none(), "explicit project correction must close: {:?}", d.app().ui.clip_dialog);
    for item in items {
        let map = d.app().session.project.item(item).unwrap().as_media().unwrap().interpret.audio_channels.as_ref().unwrap();
        assert_eq!(map.clips, vec![vec![0]], "one valid replacement applies to every target atomically");
    }
    assert_eq!(d.app().session.state.project_selection, selection);
    assert_eq!(d.app().session.history.undo.len(), undo + 1);
    d.exec("edit.undo", json!({}));
    assert_eq!(d.app().session.project, before);
}
