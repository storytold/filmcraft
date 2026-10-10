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

#[test]
fn save_project_actions_are_right_aligned_and_cancel_preserves_work() {
    let mut d = Driver::new();
    // A saved project path enables Save; Cancel must never touch the filesystem.
    d.app().session.path = Some("dialog.fcproj".into());
    d.exec("file.newBin", json!({"name":"Unsaved change"}));
    let before = d.app().session.project.to_json();
    let style = d.harness.ctx.global_style();
    d.menu("file.closeProject");
    let elements = d.ok("ui.elements", json!({"prefix":"closeProject."}));
    let rect = |id: &str| {
        let e = elements.as_array().unwrap().iter().find(|e| e["id"] == id).unwrap();
        let r = &e["rect"];
        egui::Rect::from_min_size(
            egui::pos2(r[0].as_f64().unwrap() as f32, r[1].as_f64().unwrap() as f32),
            egui::vec2(r[2].as_f64().unwrap() as f32, r[3].as_f64().unwrap() as f32),
        )
    };
    let (save, discard, cancel) = (rect("closeProject.save"), rect("closeProject.dontSave"), rect("closeProject.cancel"));
    assert!(save.right() <= discard.left() && discard.right() <= cancel.left());
    let window = d.harness.ctx.memory(|m| m.area_rect(egui::Id::new(("clip-dialog", "closeProject")))).unwrap();
    assert!((window.right() - cancel.right()).abs() < 20.0, "action group stays at the right edge: {window:?} {cancel:?}");
    assert_eq!(d.harness.ctx.global_style().visuals.override_text_color, style.visuals.override_text_color);
    d.click("closeProject.cancel");
    assert!(d.app().ui.clip_dialog.is_none());
    assert!(d.app().session.is_dirty());
    assert_eq!(d.app().session.project.to_json(), before);
}
