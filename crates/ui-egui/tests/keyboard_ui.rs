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

    /// A real key press as egui-winit reports it: `key` (logical, or the physical key when the
    /// layout's character has no egui name) and `physical`.
    fn press(&mut self, key: egui::Key, physical: egui::Key, m: egui::Modifiers) {
        for pressed in [true, false] {
            self.harness.input_mut().events.push(egui::Event::Key { key, physical_key: Some(physical), pressed, repeat: false, modifiers: m });
        }
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

/// Every Premiere command the `.kys` import maps names a FilmCraft command (engine or UI).
#[test]
fn premiere_file_import_names_real_commands() {
    use filmcraft_engine::premiere_kys::{COMMANDS, PANEL_COMMANDS};
    let ui: Vec<String> = filmcraft_ui_egui::menus::external_commands().into_iter().map(|c| c.id).collect();
    let mut bad = Vec::new();
    for (premiere, id) in COMMANDS.iter().map(|(p, i)| (*p, *i)).chain(PANEL_COMMANDS.iter().map(|(_, p, i)| (*p, *i))) {
        if filmcraft_engine::find_command(id).is_none() && !ui.iter().any(|u| u == id) {
            bad.push(format!("{premiere} → {id}"));
        }
    }
    assert!(bad.is_empty(), "dangling ids: {bad:#?}");
}

/// A German keyboard: keys that type characters egui has no name for (Ö, <), keys whose
/// character moves with Shift (Shift+0 is `=`), + (a US ]) and QWERTZ letters all run their
/// shortcuts; modifiers must match exactly.
#[test]
fn shortcuts_work_on_a_german_keyboard() {
    use egui::{Key, Modifiers};
    use filmcraft_ui_egui::dock::PanelKind;
    let mut d = Driver::new();
    d.focus("Timeline");
    // Shift+0 (Multi-Camera View) types `=` on a German keyboard
    let mc = d.app().ui.program.multicam;
    d.press(Key::Equals, Key::Num0, Modifiers::SHIFT);
    assert_ne!(d.app().ui.program.multicam, mc, "Shift+0 toggles the Multi-Camera view");
    d.press(Key::Equals, Key::Num0, Modifiers::SHIFT);
    // Z and Y: the labels win
    d.press(Key::Z, Key::Y, Modifiers::NONE);
    assert_eq!(d.app().ui.tool, filmcraft_ui_egui::state::Tool::Zoom);
    d.press(Key::Y, Key::Z, Modifiers::NONE);
    assert_eq!(d.app().ui.tool, filmcraft_ui_egui::state::Tool::Slip);
    // Ö (US ;), + (US ]) and < (the ISO key) take shortcuts like any other key
    for (keys, key, physical) in
        [(";", Key::Semicolon, Key::Semicolon), ("]", Key::Plus, Key::CloseBracket), ("IntlBackslash", Key::IntlBackslash, Key::IntlBackslash)]
    {
        d.exec("shortcuts.set", json!({"command": "window.maximizeFrame", "keys": keys}));
        d.frames(2);
        d.press(key, physical, Modifiers::NONE);
        assert_eq!(d.app().ui.keys.maximized, Some(PanelKind::Timeline), "{keys}");
        d.press(key, physical, Modifiers::NONE);
        assert_eq!(d.app().ui.keys.maximized, None, "{keys}");
    }
    // exact modifiers: Alt+D is not D (Select Clip at Playhead)
    let seq = d.exec("sequence.inspect", json!({}));
    let second = &seq["video"][0]["items"].as_array().unwrap()[1];
    d.exec("playhead.set", json!({"time": second["start"].as_i64().unwrap() + 1_000_000_000}));
    d.exec("edit.deselectAll", json!({}));
    d.press(Key::D, Key::D, Modifiers::ALT);
    assert!(d.app().session.state.selection.is_empty(), "Alt+D ran D");
    d.press(Key::D, Key::D, Modifiers::NONE);
    assert!(!d.app().session.state.selection.is_empty(), "D selects");
}

/// A Premiere Pro `.kys` file imported in the app drives the keyboard, labels follow its layout.
#[test]
fn an_imported_premiere_file_drives_the_keyboard() {
    use egui::{Key, Modifiers};
    let dir = std::env::temp_dir().join(format!("fc-kys-ui-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let item = |i: usize, cmd: &str, code: u32, shift: bool| {
        format!(
            "<item.{i}><virtualkey>{code}</virtualkey><modifier.ctrl>false</modifier.ctrl><modifier.alt>false</modifier.alt><modifier.shift>{shift}</modifier.shift><commandname>{cmd}</commandname></item.{i}>"
        )
    };
    let ch = |c: char| 0x8000_0000u32 | c as u32;
    let text = format!(
        "<?xml version=\"1.0\"?><PremiereData><shortcuts><context.global>{}{}{}</context.global><context.timeline>{}</context.timeline><platform>windows</platform></shortcuts></PremiereData>",
        item(0, "cmd.tools.07slip", ch('Y'), false),
        item(1, "cmd.toggle.maximize.focused.frame", ch('Ü'), true),
        item(2, "cmd.tools.06razor", ch('<'), false),
        item(3, "cmd.timeline.move.cti.to.cursor", 38, false),
    );
    let path = dir.join("Premiere.kys");
    std::fs::write(&path, text).unwrap();
    let mut d = Driver::new();
    let r = d.exec("shortcuts.import", json!({"path": path.to_string_lossy()}));
    assert_eq!(r["imported"], json!(4), "{r}");
    d.frames(2);
    d.focus("Timeline");
    // the German Y key and < key
    d.press(Key::Y, Key::Z, Modifiers::NONE);
    assert_eq!(d.app().ui.tool, filmcraft_ui_egui::state::Tool::Slip);
    d.press(Key::IntlBackslash, Key::IntlBackslash, Modifiers::NONE);
    assert_eq!(d.app().ui.tool, filmcraft_ui_egui::state::Tool::Razor);
    // Shift+Ü maximizes the focused frame
    d.press(Key::OpenBracket, Key::OpenBracket, Modifiers::SHIFT);
    assert!(d.app().ui.keys.maximized.is_some());
    d.press(Key::OpenBracket, Key::OpenBracket, Modifiers::SHIFT);
    // menus show the German labels (the layout is the app's, not a process-wide test setting)
    let layout = d.app().session.prefs.general.key_layout();
    assert_eq!(layout, filmcraft_engine::shortcuts::KeyLayout::De);
    let shown = filmcraft_engine::shortcuts::Chord::parse("Shift+[").unwrap().display_in(Platform::current(), layout);
    assert_eq!(shown, if cfg!(target_os = "macos") { "⇧Ü" } else { "Shift+Ü" });
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn move_playhead_to_cursor_and_mixer_menu_commands() {
    let mut d = Driver::new();
    d.focus("Timeline");
    let layout = d.app().tl.layout.clone().expect("the Timeline is laid out");
    let rate = d.app().session.sequence_rate();
    let x = layout.x_of(filmcraft_time::Tick::from_seconds_f64(2.0));
    let r = d.ok("ui.menu.invoke", json!({"id": "timeline.playheadToCursor", "params": {"x": x}}));
    let t = r["time"].as_i64().unwrap();
    assert_eq!(rate.snap_nearest(filmcraft_time::Tick(t)).0, t, "on a frame");
    assert!((filmcraft_time::Tick(t).seconds() - 2.0).abs() < 0.1, "{t}");
    // off the Timeline it says so instead of moving
    let (req, reply) = ControlRequest::new("ui.menu.invoke", json!({"id": "timeline.playheadToCursor", "params": {"x": -5.0}}));
    d.tx.send(req).unwrap();
    let v = loop {
        d.frames(1);
        if let Ok(v) = reply.try_recv() {
            break v;
        }
    };
    assert_eq!(v["ok"], json!(false), "{v}");
    assert_eq!(d.app().session.playhead().0, t);
    // Audio Track Mixer ▸ Meter Input(s) Only and Show/Hide Tracks as commands
    let on = d.app().ui.mixer_meter_input_only;
    d.ok("ui.menu.invoke", json!({"id": "mixer.meterInputOnly"}));
    assert_ne!(d.app().ui.mixer_meter_input_only, on);
    let r = d.ok("ui.menu.invoke", json!({"id": "mixer.showHideTracks"}));
    assert_eq!(r["dialog"], json!("showHideTracks"));
    // the Rectangle and Ellipse tools can have keys
    let ui: Vec<String> = filmcraft_ui_egui::menus::external_commands().into_iter().map(|c| c.id).collect();
    assert!(ui.iter().any(|c| c == "tool.rectangle") && ui.iter().any(|c| c == "tool.ellipse"));
}
