//! Scripted UI tests: the real `FilmcraftApp` runs headless under `egui_kittest` (no window, no
//! GPU) and is driven through the same JSON control channel that agents use over TCP/MCP
//! (`docs/control-protocol.md`). Each test opens the demo project, sends control requests
//! (`engine.execute`, `ui.menu.invoke`, `ui.click`, `ui.inspect`, …) between frames and asserts on
//! the replies and on `sequence.inspect` JSON.
//!
//! The harness steps egui frames itself: synthetic input queued by `ui.click`/`ui.key` is moved
//! into the next frame's `RawInput` through the app's own `raw_input_hook`, exactly as eframe does.
//! Screenshots (`ui.screenshot`) need a real viewport and are not covered here.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    /// The app with the demo project open, after its first frames (theme and fonts installed).
    fn demo() -> Self {
        Self::demo_with(false)
    }

    /// `render`: with a wgpu renderer so `Harness::render` can take screenshots (needs a GPU).
    fn demo_with(render: bool) -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let mut b = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000);
        if render {
            b = b.wgpu();
        }
        let harness = b.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        d
    }

    /// Run `n` frames, feeding queued synthetic input like eframe's `raw_input_hook`.
    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
        }
    }

    /// Send a control request and run frames until it is answered.
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

    /// `call` that must succeed; returns `result`.
    fn ok(&mut self, method: &str, params: Value) -> Value {
        let v = self.call(method, params.clone());
        assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
        v["result"].clone()
    }

    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }

    fn sequence(&mut self) -> Value {
        self.exec("sequence.inspect", json!({}))
    }

    fn inspect(&mut self) -> Value {
        self.ok("ui.inspect", json!({}))
    }

    fn element_ids(&mut self, prefix: &str) -> Vec<String> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }
}

/// Clips on a video track in `sequence.inspect` JSON.
fn track_clips(seq: &Value, track: usize) -> Vec<Value> {
    seq["video"][track]["items"].as_array().cloned().unwrap_or_default()
}

#[test]
fn demo_project_opens_headless_and_registers_widgets() {
    let mut d = Driver::demo();
    let ui = d.inspect();
    assert!(ui["activeSequence"].is_u64(), "demo sequence is active: {ui}");
    assert!(ui["elements"].as_u64().unwrap() > 50, "widgets registered: {}", ui["elements"]);
    let panels = d.element_ids("panel.");
    for p in ["panel.Timeline", "panel.Program", "panel.Project", "panel.tab.Timeline"] {
        assert!(panels.iter().any(|x| x == p), "{p} missing from {panels:?}");
    }
    let tools = d.element_ids("tools.");
    assert!(tools.len() >= 5, "tool buttons: {tools:?}");
    let seq = d.sequence();
    assert_eq!(track_clips(&seq, 0).len(), 6, "demo V1 clips: {seq}");
}

#[test]
fn razor_then_undo_through_the_control_channel() {
    let mut d = Driver::demo();
    let before = track_clips(&d.sequence(), 0).len();
    d.exec("playhead.set", json!({"seconds": 2.0}));
    let r = d.exec("timeline.razor", json!({"track": "V1"}));
    assert_eq!(r["cuts"], json!(1), "{r}");
    let after = track_clips(&d.sequence(), 0);
    assert_eq!(after.len(), before + 1);
    // the timeline shows the new clip on the next frame
    d.frames(2);
    let new_clip = after[1]["clip"].as_u64().unwrap();
    let at = d.ok("ui.timeline.locate", json!({"clip": new_clip}));
    assert!(at["x"].as_f64().is_some(), "{at}");
    // Undo from the menu path (UI command dispatch), then Redo by shortcut-free command id.
    d.ok("ui.menu.invoke", json!({"id": "edit.undo"}));
    assert_eq!(track_clips(&d.sequence(), 0).len(), before);
    d.exec("edit.redo", json!({}));
    assert_eq!(track_clips(&d.sequence(), 0).len(), before + 1);
}

#[test]
fn insert_from_source_ripples_the_sequence() {
    let mut d = Driver::demo();
    let project = d.exec("project.inspect", json!({}));
    let item = first_media_item(&project).unwrap_or_else(|| panic!("no media item in {project}"));
    let dur0 = d.sequence()["duration"].as_i64().unwrap();
    d.exec("source.open", json!({"item": item}));
    let rate = &d.sequence()["settings"]["frame_rate"];
    let (num, den) = (rate["num"].as_i64().unwrap(), rate["den"].as_i64().unwrap());
    let tick = |f: i64| f * 254_016_000_000 * den / num;
    d.exec("project.setMarks", json!({"item": item, "in": tick(24), "out": tick(47)}));
    d.exec("playhead.set", json!({"frame": 0}));
    d.exec("source.insert", json!({}));
    let dur1 = d.sequence()["duration"].as_i64().unwrap();
    assert!(dur1 > dur0, "insert lengthens the sequence: {dur0} → {dur1}");
    d.frames(2);
}

/// First movie item id anywhere in `project.inspect` JSON.
fn first_media_item(v: &Value) -> Option<u64> {
    match v {
        Value::Object(m) => {
            if let Some(id) = m.get("item").and_then(Value::as_u64)
                && m.get("type").and_then(Value::as_str).is_some_and(|t| t == "Movie")
            {
                return Some(id);
            }
            m.values().find_map(first_media_item)
        }
        Value::Array(a) => a.iter().find_map(first_media_item),
        _ => None,
    }
}

#[test]
fn apply_effect_appears_in_effect_controls() {
    let mut d = Driver::demo();
    let clip = track_clips(&d.sequence(), 0)[0]["clip"].as_u64().unwrap();
    d.exec("timeline.select", json!({"clips": [clip]}));
    d.exec("effects.apply", json!({"effect": "Gaussian Blur"}));
    let seq = d.sequence();
    let fx: Vec<String> = track_clips(&seq, 0)[0]["effects"].as_array().unwrap().iter().filter_map(|e| e["effect"].as_str().map(str::to_string)).collect();
    assert!(fx.iter().any(|e| e == "gaussian_blur"), "{fx:?}");
    d.ok("ui.panel.show", json!({"panel": "Effect Controls"}));
    d.frames(3);
    let ids = d.element_ids("effectControls.effect.");
    assert!(ids.iter().any(|i| i == "effectControls.effect.gaussian_blur"), "{ids:?}");
    d.ok("ui.menu.invoke", json!({"id": "edit.undo"}));
    d.frames(2);
    assert!(!d.element_ids("effectControls.effect.").iter().any(|i| i == "effectControls.effect.gaussian_blur"));
}

#[test]
fn spanish_search_finds_effects_and_presets_with_stable_automation_ids() {
    let mut d = Driver::demo();
    d.ok("ui.menu.invoke", json!({"id": "app.language.spanish"}));
    d.ok("ui.panel.show", json!({"panel": "Effects"}));
    d.ok("ui.set", json!({"effectsSearch": "DESENFOQUE"}));
    d.frames(3);
    assert!(d.element_ids("effects.item.").contains(&"effects.item.gaussian_blur".to_string()));
    d.ok("ui.set", json!({"effectsSearch": "ENTRADA CON DESENFOQUE"}));
    d.frames(3);
    assert!(d.element_ids("effects.preset.").contains(&"effects.preset.Blur In".to_string()));
    let clip = track_clips(&d.sequence(), 0)[0]["clip"].as_u64().unwrap();
    d.exec("timeline.select", json!({"clips": [clip]}));
    d.exec("effects.apply", json!({"effect": "gaussian_blur"}));
    d.ok("ui.panel.show", json!({"panel": "Effect Controls"}));
    d.frames(3);
    assert!(d.element_ids("effectControls.effect.").contains(&"effectControls.effect.gaussian_blur".to_string()));
    d.ok("ui.menu.invoke", json!({"id": "app.language.english"}));
}

#[test]
fn effects_panel_lists_the_premiere_transition_folders() {
    let mut d = Driver::demo();
    d.ok("ui.panel.show", json!({"panel": "Effects"}));
    d.frames(3);
    let folders = d.element_ids("effects.folder.");
    for f in filmcraft_project::vtransition::VIDEO_TRANSITION_FOLDERS {
        assert!(folders.contains(&format!("effects.folder.Video Transitions/{f}")), "{f}: {folders:?}");
    }
    assert!(folders.iter().any(|f| f == "effects.folder.Legacy"), "{folders:?}");
    // search opens the matching folders: every modern wipe, and the legacy ones
    d.ok("ui.set", json!({"effectsSearch": "wipe"}));
    d.frames(3);
    let items = d.element_ids("effects.item.");
    for id in
        ["clock_wipe", "linear_wipe", "neon_wipe", "panel_wipe", "plateau_wipe", "radial_wipe", "soft_wipe", "star_wipe", "stretch_wipe", "vr_gradient_wipe"]
    {
        assert!(items.iter().any(|i| i == &format!("effects.item.{id}")), "{id}: {items:?}");
    }
    d.ok("ui.set", json!({"effectsSearch": "legacy"}));
    d.frames(3);
    let items = d.element_ids("effects.item.");
    for id in ["additive_dissolve_legacy", "clock_wipe_legacy", "cross_dissolve_legacy", "push_legacy", "whip_legacy"] {
        assert!(items.iter().any(|i| i == &format!("effects.item.{id}")), "{id}: {items:?}");
    }
    // obsolete transitions stay out of the panel
    d.ok("ui.set", json!({"effectsSearch": "venetian"}));
    d.frames(3);
    assert!(d.element_ids("effects.item.").is_empty());
    // clicking a sub-folder toggles it
    d.ok("ui.set", json!({"effectsSearch": ""}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "effects.folder.Video Transitions/Dissolve"}));
    d.frames(2);
    assert!(!d.element_ids("effects.item.").iter().any(|i| i == "effects.item.cross_dissolve"));
    // applying from the panel's command path works for a legacy transition
    let cut = track_clips(&d.sequence(), 0)[1]["start"].as_i64().unwrap();
    d.exec("playhead.set", json!({"time": cut}));
    d.exec("sequence.applyVideoTransition", json!({"effect": "Iris Round"}));
    let seq = d.sequence();
    assert!(seq["video"][0]["transitions"].as_array().unwrap().iter().any(|t| t["effect"] == "iris_round"), "{seq}");
}

#[test]
fn playback_toggle_and_stop() {
    let mut d = Driver::demo();
    assert_eq!(d.inspect()["playback"]["playing"], json!(false));
    let r = d.ok("ui.menu.invoke", json!({"id": "playback.toggle"}));
    assert_eq!(r["playing"], json!(true), "{r}");
    d.frames(5);
    assert_eq!(d.inspect()["playback"]["playing"], json!(true));
    let r = d.ok("ui.playback", json!({"action": "stop"}));
    assert_eq!(r["playing"], json!(false), "{r}");
    assert_eq!(d.inspect()["playback"]["playing"], json!(false));
}

#[test]
fn clicking_a_tool_button_by_automation_id() {
    let mut d = Driver::demo();
    assert_ne!(d.inspect()["ui"]["tool"], json!("Razor"));
    d.ok("ui.click", json!({"id": "tools.Razor"}));
    d.frames(2);
    let ui = d.inspect();
    assert_eq!(ui["ui"]["tool"], json!("Razor"), "{}", ui["ui"]["tool"]);
    d.ok("ui.set", json!({"tool": "selection"}));
    assert_eq!(d.inspect()["ui"]["tool"], json!("Selection"));
}

#[test]
fn unknown_methods_and_commands_fail_cleanly() {
    let mut d = Driver::demo();
    let v = d.call("ui.nope", json!({}));
    assert_eq!(v["ok"], json!(false));
    let v = d.call("engine.execute", json!({"command": "no.such.command"}));
    assert_eq!(v["ok"], json!(false));
}

/// End of the first V1 clip (ticks).
fn first_cut(d: &mut Driver) -> (u64, i64) {
    let c = &track_clips(&d.sequence(), 0)[0];
    (c["clip"].as_u64().unwrap(), c["start"].as_i64().unwrap() + c["duration"].as_i64().unwrap())
}

#[test]
fn trim_monitor_buttons_and_dynamic_jkl_trimming() {
    let mut d = Driver::demo();
    for t in ["V2", "V3", "A2", "A3"] {
        d.exec("timeline.setTrack", json!({"track": t, "syncLock": false}));
    }
    let (clip, cut) = first_cut(&mut d);
    let fd = {
        let rate = &d.sequence()["settings"]["frame_rate"];
        254_016_000_000 * rate["den"].as_i64().unwrap() / rate["num"].as_i64().unwrap()
    };
    // Selecting an edit point turns the Program monitor into the Trim Monitor.
    assert!(d.element_ids("trimMonitor.").is_empty());
    d.exec("trim.selectEditPoint", json!({"clip": clip, "edge": "out", "kind": "roll"}));
    d.frames(3);
    let ids = d.element_ids("trimMonitor.");
    for id in [
        "trimMonitor.outgoing",
        "trimMonitor.incoming",
        "trimMonitor.outShift",
        "trimMonitor.inShift",
        "trimMonitor.forward",
        "trimMonitor.backwardMany",
        "trimMonitor.applyTransition",
    ] {
        assert!(ids.iter().any(|x| x == id), "{id} missing from {ids:?}");
    }
    // +1 button rolls the cut one frame
    d.ok("ui.click", json!({"id": "trimMonitor.forward"}));
    d.frames(2);
    assert_eq!(first_cut(&mut d).1, cut + fd);
    let info = d.exec("trim.monitor", json!({}));
    assert_eq!(info["outShift"], json!(1), "{info}");
    let undo0 = d.exec("history.list", json!({}))["undo"].as_array().unwrap().len();

    // L starts a dynamic trim; the edit moves while frames run; K commits one undo step.
    d.ok("ui.key", json!({"key": "L"}));
    d.frames(4);
    let live = d.exec("trim.monitor", json!({}));
    assert!(live["dynamic"]["offsetFrames"].as_i64().unwrap() > 0, "trimming live: {live}");
    assert_eq!(d.exec("history.list", json!({}))["undo"].as_array().unwrap().len(), undo0, "no undo steps while trimming");
    d.ok("ui.key", json!({"key": "K"}));
    d.frames(2);
    let after = first_cut(&mut d).1;
    assert!(after > cut + fd, "the cut moved later: {cut} -> {after}");
    let hist = d.exec("history.list", json!({}));
    let undo = hist["undo"].as_array().unwrap();
    assert_eq!(undo.len(), undo0 + 1, "{hist}");
    assert_eq!(undo.last().unwrap(), &json!("Dynamic Rolling Edit"));
    assert!(d.exec("trim.monitor", json!({}))["dynamic"].is_null());
    // J trims backward
    d.ok("ui.key", json!({"key": "J"}));
    d.frames(2);
    d.ok("ui.key", json!({"key": "K"}));
    d.frames(1);
    assert!(first_cut(&mut d).1 < after, "J trimmed backward");
    // Undo twice: back to the +1 state, then Space loops around the edit
    d.ok("ui.menu.invoke", json!({"id": "edit.undo"}));
    d.ok("ui.menu.invoke", json!({"id": "edit.undo"}));
    assert_eq!(first_cut(&mut d).1, cut + fd);
    d.ok("ui.key", json!({"key": "Space"}));
    d.frames(2);
    assert!(d.exec("trim.monitor", json!({}))["playAround"].is_object());
    d.ok("ui.key", json!({"key": "Space"}));
    d.frames(1);
    assert!(d.exec("trim.monitor", json!({}))["playAround"].is_null());
    // exit trim mode: the normal Program monitor returns
    d.ok("ui.click", json!({"id": "trimMonitor.exit"}));
    d.frames(2);
    assert!(d.element_ids("trimMonitor.").is_empty());
    assert!(d.element_ids("program.").iter().any(|x| x == "program.picture"));
}

impl Driver {
    /// Let background frame workers deliver, then render the window to `<tmp>/<name>.png`.
    fn screenshot(&mut self, name: &str) -> std::path::PathBuf {
        for _ in 0..30 {
            std::thread::sleep(std::time::Duration::from_millis(40));
            self.frames(1);
        }
        let img = self.harness.render().expect("render");
        let png = filmcraft_ui_egui::control::encode_png(img.as_raw(), img.width(), img.height()).expect("png");
        let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("ui-screenshots");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{name}.png"));
        std::fs::write(&path, png).unwrap();
        eprintln!("screenshot: {}", path.display());
        path
    }
}

/// Renders the Trim Monitor and the Keyboard Shortcuts dialog to PNGs for visual review (needs a
/// GPU): `cargo test -p filmcraft-ui-egui --test scripted -- --ignored screenshots`.
#[test]
#[ignore]
fn screenshots() {
    let mut d = Driver::demo_with(true);
    for t in ["V2", "V3", "A2", "A3"] {
        d.exec("timeline.setTrack", json!({"track": t, "syncLock": false}));
    }
    d.exec("playhead.set", json!({"seconds": 4.5}));
    d.exec("trim.selectNearest", json!({"kind": "rippleOut"}));
    d.exec("trim.forwardMany", json!({}));
    d.screenshot("trim-monitor-ripple");
    d.exec("trim.toggleType", json!({}));
    d.ok("ui.key", json!({"key": "L"}));
    d.frames(3);
    d.screenshot("trim-monitor-dynamic-roll");
    d.ok("ui.key", json!({"key": "K"}));
    d.frames(2);
    d.exec("trim.clear", json!({}));
    // Keyboard Shortcuts dialog: Premiere-compatible preset, Timeline panel context, key K selected
    d.ok("ui.key", json!({"key": "Cmd+Alt+K"}));
    d.frames(2);
    d.screenshot("shortcuts-dialog");
    d.ok("ui.click", json!({"id": "shortcuts.context"}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "shortcuts.context.Timeline"}));
    d.ok("ui.click", json!({"id": "shortcuts.key.K"}));
    d.ok("ui.click", json!({"id": "shortcuts.search"}));
    d.ok("ui.type", json!({"text": "trim"}));
    d.frames(2);
    d.screenshot("shortcuts-dialog-timeline-trim");
    d.ok("ui.click", json!({"id": "shortcuts.mod.Cmd"}));
    d.frames(2);
    d.screenshot("shortcuts-dialog-cmd");
}

#[test]
fn keyboard_shortcuts_dialog_assigns_live_and_cancel_restores() {
    let mut d = Driver::demo();
    let menu_shortcut = |d: &mut Driver, id: &str| -> Value {
        let items = d.ok("ui.menu.list", json!({}));
        items.as_array().unwrap().iter().find(|i| i["id"] == id).map(|i| i["shortcut"].clone()).unwrap_or(Value::Null)
    };
    assert_eq!(menu_shortcut(&mut d, "sequence.addEdit"), json!("Cmd+K"));
    assert_eq!(menu_shortcut(&mut d, "app.keyboardShortcuts"), json!("Cmd+Alt+K"));
    // Edit ▸ Keyboard Shortcuts… by its shortcut
    d.ok("ui.key", json!({"key": "Cmd+Alt+K"}));
    d.frames(2);
    assert_eq!(d.inspect()["dialog"], json!("Shortcuts"));
    let keys = d.element_ids("shortcuts.key.");
    assert!(keys.len() > 60, "drawn keyboard: {}", keys.len());
    // search, then click the Shortcut cell and press new keys
    d.ok("ui.click", json!({"id": "shortcuts.search"}));
    d.ok("ui.type", json!({"text": "add edit"}));
    d.frames(2);
    assert!(d.element_ids("shortcuts.row.").iter().any(|x| x == "shortcuts.row.sequence.addEdit"));
    d.ok("ui.click", json!({"id": "shortcuts.cell.sequence.addEdit"}));
    d.frames(1);
    d.ok("ui.key", json!({"key": "Cmd+Shift+J"}));
    d.frames(2);
    assert_eq!(menu_shortcut(&mut d, "sequence.addEdit"), json!("Cmd+Shift+J"), "the menu shows the new key live");
    let cmds = d.ok("engine.commands", json!({}));
    let ae = cmds.as_array().unwrap().iter().find(|c| c["id"] == "sequence.addEdit").unwrap().clone();
    assert_eq!(ae["shortcut"], json!("Cmd+Shift+J"), "command list too: {ae}");
    // reassigning a used key takes it from the other command (and says so)
    d.ok("ui.click", json!({"id": "shortcuts.cell.sequence.addEdit"}));
    d.frames(1);
    d.ok("ui.key", json!({"key": "Cmd+Shift+K"}));
    d.frames(2);
    assert_eq!(menu_shortcut(&mut d, "sequence.addEditAllTracks"), Value::Null);
    let msg = d.element_ids("shortcuts.message");
    assert_eq!(msg.len(), 1);
    // Undo / Redo inside the dialog
    d.ok("ui.click", json!({"id": "shortcuts.undo"}));
    d.frames(1);
    assert_eq!(menu_shortcut(&mut d, "sequence.addEditAllTracks"), json!("Cmd+Shift+K"));
    assert_eq!(menu_shortcut(&mut d, "sequence.addEdit"), json!("Cmd+Shift+J"));
    d.ok("ui.click", json!({"id": "shortcuts.redo"}));
    d.frames(1);
    assert_eq!(menu_shortcut(&mut d, "sequence.addEdit"), json!("Cmd+Shift+K"));
    // Cancel restores everything
    d.ok("ui.click", json!({"id": "shortcuts.cancel"}));
    d.frames(2);
    assert!(d.inspect()["dialog"].is_null());
    assert_eq!(menu_shortcut(&mut d, "sequence.addEdit"), json!("Cmd+K"));
    assert_eq!(menu_shortcut(&mut d, "sequence.addEditAllTracks"), json!("Cmd+Shift+K"));

    // Assign + OK, then the new key really runs the command
    d.ok("ui.menu.invoke", json!({"id": "app.keyboardShortcuts"}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "shortcuts.search"}));
    d.ok("ui.type", json!({"text": "add edit"}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "shortcuts.add.sequence.addEdit"}));
    d.frames(1);
    d.ok("ui.key", json!({"key": "Cmd+Shift+J"}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "shortcuts.ok"}));
    d.frames(2);
    let n0 = track_clips(&d.sequence(), 0).len();
    d.exec("playhead.set", json!({"seconds": 2.0}));
    d.ok("ui.set", json!({"focused": "Timeline"}));
    d.ok("ui.key", json!({"key": "Cmd+Shift+J"}));
    d.frames(2);
    assert_eq!(track_clips(&d.sequence(), 0).len(), n0 + 1, "Cmd+Shift+J adds an edit");
    d.ok("ui.key", json!({"key": "Cmd+K"}));
    d.frames(2);
    d.exec("playhead.set", json!({"seconds": 3.0}));
    d.ok("ui.key", json!({"key": "Cmd+K"}));
    d.frames(2);
    assert_eq!(track_clips(&d.sequence(), 0).len(), n0 + 2, "the original Cmd+K was kept (added, not replaced)");

    // Panel-specific: Left in the History panel is Undo; in the Timeline it steps back
    d.ok("ui.set", json!({"focused": "History"}));
    d.ok("ui.key", json!({"key": "Left"}));
    d.frames(2);
    assert_eq!(track_clips(&d.sequence(), 0).len(), n0 + 1, "History ▸ Left = Step Backward (undo)");
    d.ok("ui.set", json!({"focused": "Timeline"}));
    let ph = d.sequence()["playhead"].as_i64().unwrap();
    d.ok("ui.key", json!({"key": "Left"}));
    d.frames(2);
    assert!(d.sequence()["playhead"].as_i64().unwrap() < ph, "Timeline ▸ Left steps back");
    assert_eq!(track_clips(&d.sequence(), 0).len(), n0 + 1);
}

/// Waveform peaks are cached per item id; ids repeat across projects, so opening another project
/// must not reuse the previous project's peaks (it showed a long mission-audio clip as silent).
#[test]
fn waveform_peaks_do_not_survive_opening_another_project() {
    let mut d = Driver::demo();
    let mut first = None;
    for _ in 0..2000 {
        d.frames(1);
        let peaks = d.harness.state().tl.peaks.lock().unwrap().iter().map(|(k, v)| (*k, v.clone())).next();
        if peaks.is_some() {
            first = peaks;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let (item, old) = first.expect("the demo timeline computes waveform peaks");
    // Reopen the same project from disk: same item ids, new media pool.
    let path = std::env::temp_dir().join(format!("fc-peaks-{}.fcproj", std::process::id()));
    let path = path.to_string_lossy().to_string();
    d.exec("file.saveAs", json!({"path": path}));
    d.exec("file.open", json!({"path": path}));
    d.frames(2);
    let _ = std::fs::remove_file(&path);
    let now = d.harness.state().tl.peaks.lock().unwrap().get(&item).cloned();
    assert!(now.is_none_or(|p| !std::sync::Arc::ptr_eq(&p, &old)), "peaks from the previous project were reused");
}

/// #164: on a real keyboard, pressing Alt+Shift+J sends each modifier key on its own first (egui
/// reports modifier keys as key events). The shortcut recorder took the Alt press and refused it
/// ("AltLeft can't be used as a shortcut"), so no chord with Shift, Ctrl or Alt could be recorded.
#[test]
fn recording_a_shortcut_skips_the_modifier_key_presses() {
    let mut d = Driver::demo();
    d.ok("ui.menu.invoke", json!({"id": "app.keyboardShortcuts"}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "shortcuts.search"}));
    d.ok("ui.type", json!({"text": "add edit"}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "shortcuts.add.sequence.addEdit"}));
    d.frames(1);
    // what a real keyboard sends for Alt+Shift+J: Alt, then Shift, then J
    let alt = egui::Modifiers { alt: true, ..Default::default() };
    let alt_shift = egui::Modifiers { alt: true, shift: true, ..Default::default() };
    let key = |key, modifiers| egui::Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers };
    let events = &mut d.harness.input_mut().events;
    events.push(key(egui::Key::AltLeft, alt));
    events.push(key(egui::Key::ShiftLeft, alt_shift));
    events.push(key(egui::Key::J, alt_shift));
    d.frames(2);
    d.ok("ui.click", json!({"id": "shortcuts.ok"}));
    d.frames(2);
    // added next to Cmd+K (`shortcuts.get` lists every chord of the command)
    let got = d.ok("engine.execute", json!({"command": "shortcuts.get", "params": {"command": "sequence.addEdit"}})).to_string();
    assert!(got.contains("Alt+Shift+J"), "{got}");
    assert!(got.contains("Cmd+K"), "{got}");
}
