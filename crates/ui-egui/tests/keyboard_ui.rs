//! Keyboard parity (M3.12): every Premiere-table entry names a real command, the active key sets
//! have no conflicts in the new commands, and a few of the new keys pressed headlessly.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_engine::shortcut_presets as presets;
use filmcraft_engine::shortcuts::{PREMIERE_PRESET, Platform};
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
        let mut app = FilmcraftApp::new(s).with_control(rx);
        app.hooks.open_path = Some(Box::new(|_, _| Ok(())));
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        d
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
        }
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
                return v["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }

    fn key(&mut self, key: &str) {
        self.ok("ui.key", json!({"key": key}));
        self.frames(3);
    }

    fn focus(&mut self, panel: &str) {
        self.ok("ui.set", json!({"focused": panel}));
        self.frames(2);
    }

    fn app(&self) -> &FilmcraftApp {
        self.harness.state()
    }
}

#[test]
fn alt_click_isolates_already_selected_linked_clip_before_delete() {
    for audio in [false, true] {
        let mut d = Driver::new();
        let video = d.app().session.active_sequence().unwrap().video_tracks[0].items[0].clone();
        let sound = d.app().session.active_sequence().unwrap().audio_tracks[0].items.iter().find(|i| i.link == video.link).unwrap().clone();
        let (selected, partner) = if audio { (sound.id, video.id) } else { (video.id, sound.id) };
        d.exec("timeline.select", json!({"clips": [selected.0]}));
        assert_eq!(d.app().session.state.selection.len(), 2);
        let r = d.app().auto.find(&format!("timeline.clip.{}", selected.0)).unwrap().rect;
        d.harness.input_mut().events.push(egui::Event::ModifiersChanged(egui::Modifiers::ALT));
        d.frames(1);
        d.ok("ui.click", json!({"x": r[0] + r[2] * 0.5, "y": r[1] + r[3] * 0.3, "modifiers": {"alt": true}}));
        d.frames(3);
        assert_eq!(d.app().session.state.selection, vec![selected]);
        d.harness.input_mut().events.push(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
        d.frames(1);
        d.key("Delete");
        assert!(d.app().session.active_sequence().unwrap().find_item(selected).is_none());
        assert!(d.app().session.active_sequence().unwrap().find_item(partner).is_some());
        d.exec("edit.undo", json!({}));
        assert!(d.app().session.active_sequence().unwrap().find_item(selected).is_some());
    }
}

/// Ids added for keyboard parity (engine + UI).
fn new_ids() -> Vec<String> {
    let mut v: Vec<String> = filmcraft_engine::keyboard::commands().iter().map(|c| c.id.to_string()).collect();
    v.extend(filmcraft_ui_egui::panels::keyboard::COMMANDS.iter().map(|c| c.id.to_string()));
    v
}

#[test]
fn premiere_table_has_no_dangling_ids_or_new_conflicts() {
    let ui: Vec<String> = filmcraft_ui_egui::menus::external_commands().into_iter().map(|c| c.id).collect();
    for (id, keys, panel) in presets::premiere() {
        assert!(filmcraft_engine::find_command(id).is_some() || ui.iter().any(|u| u == id), "Premiere table: `{id}` ({keys} {panel}) is not a command");
    }
    for (id, _, _) in presets::FILMCRAFT_PANEL.iter().chain(presets::PREMIERE_PANEL) {
        assert!(filmcraft_engine::find_command(id).is_some() || ui.iter().any(|u| u == id), "panel table: `{id}` is not a command");
    }
    let mut s = Session::default();
    s.shortcuts.register_external(filmcraft_ui_egui::menus::external_commands());
    let new = new_ids();
    for preset in [None, Some(PREMIERE_PRESET)] {
        if let Some(p) = preset {
            s.shortcuts.load_preset(p).unwrap();
        }
        for (ctx, key, ids) in s.shortcuts.conflicts(Platform::Mac) {
            assert!(!ids.iter().any(|i| new.contains(i)), "{preset:?}: {key} in {ctx} is bound to {ids:?}");
        }
    }
    // the Premiere preset binds every new command that Premiere has a key for
    for id in ["timeline.selectClipAtPlayhead", "file.exportFrame", "window.maximizeFrame", "timeline.nudgeRight", "clip.volumeUp"] {
        assert!(!s.shortcuts.for_command(id).is_empty(), "{id} has no key in the Premiere preset");
    }
}

#[test]
fn new_keys_work_in_the_app() {
    let mut d = Driver::new();
    let seq = d.exec("sequence.inspect", json!({}));
    let v1 = seq["video"][0]["items"].as_array().unwrap().clone();
    let second = &v1[1];
    let (clip, start) = (second["clip"].as_u64().unwrap(), second["start"].as_i64().unwrap());
    d.exec("playhead.set", json!({"time": start + 1_000_000_000}));

    // D selects the clip at the playhead
    d.focus("Timeline");
    d.key("D");
    assert!(d.app().session.state.selection.iter().any(|c| c.0 == clip), "D selects the clip under the playhead");

    // Cmd+Right (Timeline panel): nudge one frame right; undoable
    d.key("Cmd+Right");
    let rate = d.app().session.sequence_rate();
    let now = d.exec("sequence.inspect", json!({}))["video"][0]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["clip"].as_u64() == Some(clip))
        .map(|i| i["start"].as_i64().unwrap())
        .unwrap();
    assert_eq!(now, start + rate.tick_of(1).0);
    d.key("Cmd+Z");

    // Cmd+0 toggles all video targets
    let n0 = d.app().session.targeting().targeted.len();
    d.key("Cmd+0");
    assert!(d.app().session.targeting().targeted.len() < n0);
    d.key("Cmd+0");
    assert_eq!(d.app().session.targeting().targeted.len(), n0);

    // Shift+` maximizes the focused frame, again restores it
    d.key("Shift+`");
    assert_eq!(d.app().ui.keys.maximized, Some(filmcraft_ui_egui::dock::PanelKind::Timeline));
    let r = d.ok("ui.elements", json!({"prefix": "panel."}));
    let frames: Vec<&Value> = r.as_array().unwrap().iter().filter(|e| e["id"].as_str().is_some_and(|i| !i["panel.".len()..].contains('.'))).collect();
    assert_eq!(frames.len(), 1, "only the maximized panel is laid out: {r}");
    d.key("Shift+`");
    assert_eq!(d.app().ui.keys.maximized, None);

    // Shift+= expands all tracks, pressing again restores
    let h0 = d.app().ui.timeline.video_track_h;
    d.key("Shift+=");
    assert!(d.app().ui.timeline.video_track_h > h0);
    d.key("Shift+=");
    assert_eq!(d.app().ui.timeline.video_track_h, h0);

    // Ctrl+Shift+. cycles panel focus
    d.key("Ctrl+Shift+.");
    assert_ne!(d.app().ui.focused, filmcraft_ui_egui::dock::PanelKind::Timeline);

    // Project panel: Down moves the selection; Shift+\ toggles the view
    d.focus("Project");
    // the Icon view shows a bin's clips (sub-bins are folder cards): open the first bin in place
    let bin = d.app().session.project.root.children.iter().find_map(|e| if let filmcraft_project::BinEntry::Bin(b) = e { Some(b.id.0) } else { None }).unwrap();
    d.ok("ui.menu.invoke", json!({"id": "projectPanel.openBin", "params": {"bin": bin, "how": "inPlace"}}));
    d.exec("project.select", json!({"items": []}));
    d.key("Down");
    assert_eq!(d.app().session.state.project_selection.len(), 1);
    let first = d.app().session.state.project_selection[0];
    d.key("Down");
    assert_ne!(d.app().session.state.project_selection[0], first);
    let view0 = d.app().session.prefs.project_panel.view.mode;
    d.key("Shift+\\");
    assert_ne!(d.app().session.prefs.project_panel.view.mode, view0);

    // Cmd+Shift+1 zooms the Program monitor to 100%
    d.key("Cmd+Shift+1");
    assert_eq!(d.app().ui.program.zoom, Some(1.0));
    d.key("Cmd+Shift+0");
    assert_eq!(d.app().ui.program.zoom, None);
}

/// Off macOS the menus show `Ctrl+…`, so that is what agents send: a `Ctrl` chord through
/// `ui.key` (and `ctrl` in `ui.click` modifiers) must act like the physical key, which egui-winit
/// reports as `ctrl` + `command`. Bindings are written `Cmd+…`, so this failed to match (#245).
#[cfg(not(target_os = "macos"))]
#[test]
fn ctrl_chords_fire_shortcuts_off_macos() {
    let mut d = Driver::new();
    let seq = d.exec("sequence.inspect", json!({}));
    let second = &seq["video"][0]["items"].as_array().unwrap()[1];
    let (clip, start) = (second["clip"].as_u64().unwrap(), second["start"].as_i64().unwrap());
    let start_of = |d: &mut Driver| {
        d.exec("sequence.inspect", json!({}))["video"][0]["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["clip"].as_u64() == Some(clip))
            .map(|i| i["start"].as_i64().unwrap())
            .unwrap()
    };
    d.exec("playhead.set", json!({"time": start + 1_000_000_000}));
    d.focus("Timeline");
    d.key("D");
    assert!(d.app().session.state.selection.iter().any(|c| c.0 == clip));
    // Ctrl+Right nudges one frame right, like Cmd+Right; Ctrl+Z undoes it
    let rate = d.app().session.sequence_rate();
    d.key("Ctrl+Right");
    assert_eq!(start_of(&mut d), start + rate.tick_of(1).0, "Ctrl+Right nudges");
    d.key("Ctrl+Z");
    assert_eq!(start_of(&mut d), start, "Ctrl+Z undoes");
    // the same chord spelt with the modifier flag
    d.ok("ui.key", json!({"key": "Right", "ctrl": true}));
    d.frames(3);
    assert_eq!(start_of(&mut d), start + rate.tick_of(1).0, "`ctrl: true` nudges");
    d.ok("ui.key", json!({"key": "Z", "ctrl": true}));
    d.frames(3);
    assert_eq!(start_of(&mut d), start, "`ctrl: true` undoes");
    // and the bare key still does not
    d.key("Z");
    assert_eq!(start_of(&mut d), start);
}

#[test]
fn delete_key_clears_the_selected_timeline_clip() {
    // #243: on Windows and Linux keyboards Delete is the forward-delete key; with the Timeline
    // focused it ran Project ▸ Clear (nothing selected there), so the clip stayed.
    let mut d = Driver::new();
    let items = |d: &mut Driver| d.exec("sequence.inspect", json!({}))["video"][0]["items"].as_array().unwrap().clone();
    let second = items(&mut d)[1].clone();
    let (clip, start) = (second["clip"].as_u64().unwrap(), second["start"].as_i64().unwrap());
    d.exec("playhead.set", json!({"time": start + 1_000_000_000}));
    d.focus("Timeline");
    d.key("D");
    assert!(d.app().session.state.selection.iter().any(|c| c.0 == clip), "D selects the clip under the playhead");
    d.key("Delete");
    assert!(!items(&mut d).iter().any(|i| i["clip"].as_u64() == Some(clip)), "Delete removed the selected clip");
    d.key("Cmd+Z");
    assert!(items(&mut d).iter().any(|i| i["clip"].as_u64() == Some(clip)), "and undo brings it back");
}

#[test]
fn shift_delete_ripple_deletes_with_the_timeline_focused() {
    // #680: the Timeline's own `Delete` (Clear) was tried before the app-wide `Shift+Delete`
    // (Ripple Delete), and egui ignores the extra Shift, so Shift+Delete left a gap.
    let mut d = Driver::new();
    // the demo's other tracks have clips in the way of a sync-locked ripple
    for t in ["V2", "V3", "A2", "A3"] {
        d.exec("timeline.setTrack", json!({"track": t, "syncLock": false}));
    }
    let items = |d: &mut Driver| d.exec("sequence.inspect", json!({}))["video"][0]["items"].as_array().unwrap().clone();
    let v1 = items(&mut d);
    let (clip, start) = (v1[1]["clip"].as_u64().unwrap(), v1[1]["start"].as_i64().unwrap());
    let next = v1[2]["clip"].as_u64().unwrap();
    d.exec("playhead.set", json!({"time": start + 1_000_000_000}));
    d.focus("Timeline");
    d.key("D");
    assert!(d.app().session.state.selection.iter().any(|c| c.0 == clip), "D selects the clip under the playhead");
    d.key("Shift+Delete");
    let after = items(&mut d);
    assert!(!after.iter().any(|i| i["clip"].as_u64() == Some(clip)), "Shift+Delete removed the selected clip");
    let moved = after.iter().find(|i| i["clip"].as_u64() == Some(next)).map(|i| i["start"].as_i64().unwrap());
    assert_eq!(moved, Some(start), "and closed the gap");
}
