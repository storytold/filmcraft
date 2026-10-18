//! Headless UI tests of the M8.9 / M12.6 panels: Lumetri Scopes (layouts, the settings menu,
//! footer), Metadata (edit a field by typing, undo), Timecode, Events, Progress and the Reference
//! Monitor, each opened from Window ▸ and driven by automation id through the control channel.
//!
//! Set `FILMCRAFT_UI_SNAPSHOT_DIR=<dir>` to also render offscreen with wgpu and write `panels-*.png`.

#![allow(dead_code)]

use std::sync::Arc;
use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    snapshots: Option<std::path::PathBuf>,
}

impl Driver {
    fn with(session: Session) -> Self {
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let snapshots = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR").map(std::path::PathBuf::from);
        let mut b = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000);
        if snapshots.is_some() {
            b = b.wgpu();
        }
        let harness = b.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, snapshots };
        d.frames(4);
        d
    }

    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        Self::with(session)
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

    fn click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(2);
    }

    fn ids(&mut self, prefix: &str) -> Vec<String> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }

    fn has(&mut self, id: &str) -> bool {
        self.ids(id).iter().any(|i| i == id)
    }

    fn label(&mut self, id: &str) -> String {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        v.as_array().unwrap().iter().find(|e| e["id"] == json!(id)).and_then(|e| e["label"].as_str().map(str::to_string)).unwrap_or_default()
    }

    /// Step frames until `id` is registered (frames render on worker threads).
    fn wait_for(&mut self, id: &str) {
        for _ in 0..400 {
            if self.has(id) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
            self.frames(1);
        }
        panic!("{id} never appeared: {:?}", self.ids(id.split('.').next().unwrap()));
    }

    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }

    fn ui_panels(&mut self) -> Value {
        self.ok("ui.inspect", json!({}))["ui"]["panels"].clone()
    }

    /// Offscreen render of the window (or the bounding box of the elements under `prefix`).
    fn snapshot(&mut self, name: &str, prefix: Option<&str>) {
        let Some(dir) = self.snapshots.clone() else { return };
        let crop = prefix.map(|p| {
            let v = self.ok("ui.elements", json!({"prefix": p}));
            let mut bb = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
            for e in v.as_array().unwrap() {
                let r = &e["rect"];
                let (x, y, w, h) = (r[0].as_f64().unwrap() as f32, r[1].as_f64().unwrap() as f32, r[2].as_f64().unwrap() as f32, r[3].as_f64().unwrap() as f32);
                bb = [bb[0].min(x), bb[1].min(y), bb[2].max(x + w), bb[3].max(y + h)];
            }
            bb
        });
        self.frames(2);
        let img = match self.harness.render() {
            Ok(i) => i,
            Err(e) => {
                eprintln!("snapshot {name} skipped: {e}");
                return;
            }
        };
        let ppp = img.width() as f32 / 1600.0;
        let img = match crop {
            Some(bb) => {
                let x0 = ((bb[0] - 24.0) * ppp).max(0.0) as u32;
                let y0 = ((bb[1] - 30.0) * ppp).max(0.0) as u32;
                let x1 = (((bb[2] + 16.0) * ppp) as u32).min(img.width());
                let y1 = (((bb[3] + 46.0) * ppp) as u32).min(img.height());
                image::imageops::crop_imm(&img, x0, y0, x1 - x0, y1 - y0).to_image()
            }
            None => img,
        };
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{name}.png"));
        img.save(&path).unwrap();
        eprintln!("snapshot: {}", path.display());
    }
}

#[test]
fn every_panel_opens_from_the_window_menu() {
    let mut d = Driver::demo();
    let menu = d.ok("ui.menu.list", json!({}));
    let window: Vec<String> =
        menu.as_array().unwrap().iter().filter(|m| m["path"] == json!(["Window"])).filter_map(|m| m["label"].as_str().map(str::to_string)).collect();
    for p in ["Events", "Lumetri Scopes", "Metadata", "Progress", "Project Notes", "Reference Monitor", "Timecode"] {
        assert!(window.iter().any(|w| w == p), "{p} not in Window: {window:?}");
    }
    for (panel, key) in [
        ("LumetriScopes", "scopes.wrench"),
        ("Metadata", "metadata."),
        ("Timecode", "timecode.row.0"),
        ("Events", "events.clearAll"),
        ("Progress", "progress.showFinished"),
        ("ReferenceMonitor", "reference.gang"),
        ("ProjectNotes", "notes.text"),
    ] {
        d.ok("ui.menu.invoke", json!({"id": format!("window.panel.{panel}")}));
        d.frames(3);
        assert!(d.has(&format!("panel.tab.{panel}")), "{panel} has no tab");
        assert!(!d.ids(key).is_empty(), "{panel}: no {key} widgets");
        assert_eq!(d.ok("ui.inspect", json!({}))["ui"]["focused"], json!(panel));
    }
}

/// #284 point 3: the panel menu's Maximize Frame keeps the layout (Restore Frame brings it back),
/// and closing every panel still leaves a dock that Window ▸ can reopen panels into.
#[test]
fn panel_menu_maximize_restores_and_an_empty_dock_reopens() {
    use filmcraft_ui_egui::dock::{DockNode, PanelKind};
    let mut d = Driver::demo();
    let layout = d.app().ui.dock.clone();
    d.click("panel.menu.Program");
    assert_eq!(d.label("panel.menu.Program.maximize"), "Maximize Frame");
    d.click("panel.menu.Program.maximize");
    assert_eq!(d.app().ui.keys.maximized, Some(PanelKind::Program));
    assert!(!d.has("panel.Timeline"), "only the maximized panel is shown");
    d.click("panel.menu.Program");
    assert_eq!(d.label("panel.menu.Program.maximize"), "Restore Frame");
    d.click("panel.menu.Program.maximize");
    assert_eq!(d.app().ui.keys.maximized, None);
    assert_eq!(d.app().ui.dock, layout, "the layout survives maximize / restore");
    assert!(d.has("panel.Timeline"));

    let mut all = Vec::new();
    layout.panels(&mut all);
    for p in all {
        d.ok("ui.panel.close", json!({"panel": p.id()}));
    }
    assert_eq!(d.app().ui.dock, DockNode::Tabs { panels: vec![], active: 0 });
    d.ok("ui.menu.invoke", json!({"id": "window.panel.Program"}));
    d.ok("ui.menu.invoke", json!({"id": "window.panel.Timeline"}));
    d.frames(3);
    assert!(d.has("panel.tab.Program") && d.has("panel.tab.Timeline"));
}

/// The wheel over a tab strip switches that group's tab: down = next, up = previous, one tab per
/// notch, smaller trackpad steps adding up to a notch, stopping at the ends. Over the panel's
/// content the wheel leaves the tabs alone.
#[test]
fn wheel_over_a_tab_strip_switches_tabs() {
    use filmcraft_ui_egui::dock::{DockNode, PanelKind};
    fn shown(d: &mut Driver) -> PanelKind {
        fn find(n: &DockNode) -> Option<PanelKind> {
            match n {
                DockNode::Tabs { panels, active } if panels.contains(&PanelKind::Project) => panels.get(*active).copied(),
                DockNode::Tabs { .. } => None,
                DockNode::Split { a, b, .. } => find(a).or_else(|| find(b)),
            }
        }
        find(&d.app().ui.dock).unwrap()
    }
    fn wheel(d: &mut Driver, dy: f64) {
        let id = format!("panel.tab.{}", shown(d).id());
        d.ok("ui.scroll", json!({"id": id, "dy": dy}));
        d.frames(2);
    }
    let mut d = Driver::demo();
    assert_eq!(shown(&mut d), PanelKind::Project);
    wheel(&mut d, -40.0);
    assert_eq!(shown(&mut d), PanelKind::MediaBrowser);
    wheel(&mut d, 40.0);
    assert_eq!(shown(&mut d), PanelKind::Project);
    wheel(&mut d, 120.0);
    assert_eq!(shown(&mut d), PanelKind::Project, "no wrap past the first tab");

    // a trackpad swipe in small steps moves one tab once the steps add up to a notch
    let r = d.ok("ui.elements", json!({"prefix": "panel.tab.Project"}))[0]["rect"].clone();
    let at = egui::pos2((r[0].as_f64().unwrap() + r[2].as_f64().unwrap() / 2.0) as f32, (r[1].as_f64().unwrap() + r[3].as_f64().unwrap() / 2.0) as f32);
    d.harness.input_mut().events.push(egui::Event::PointerMoved(at));
    d.frames(1);
    for step in 0..5 {
        d.harness.input_mut().events.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -10.0),
            modifiers: egui::Modifiers::NONE,
            phase: egui::TouchPhase::Move,
        });
        d.frames(1);
        let expect = if step < 3 { PanelKind::Project } else { PanelKind::MediaBrowser };
        assert_eq!(shown(&mut d), expect, "after {} pt", (step + 1) * 10);
    }

    d.ok("ui.scroll", json!({"id": "panel.MediaBrowser", "dy": -40.0}));
    d.frames(2);
    assert_eq!(shown(&mut d), PanelKind::MediaBrowser, "the wheel over the content keeps the tab");

    for _ in 0..10 {
        wheel(&mut d, -40.0);
    }
    assert_eq!(shown(&mut d), PanelKind::History, "no wrap past the last tab");
}

#[test]
fn lumetri_scopes_layouts_menu_and_footer() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Color"}));
    d.ok("ui.panel.show", json!({"panel": "LumetriScopes"}));
    // the default: RGB parade, Rec. 709, 8 Bit, Clamp Signal
    d.wait_for("scopes.view.parade");
    assert_eq!(d.label("scopes.space"), "Rec. 709");
    let p = d.ui_panels();
    assert_eq!((p["scopes"]["shown"].clone(), p["scopes"]["scale"].clone(), p["scopes"]["clamp"].clone()), (json!(["parade"]), json!("bits8"), json!(true)));
    d.snapshot("panels-scopes-parade", Some("scopes."));
    // the wrench menu: add the YUV vectorscope, switch the waveform type through its submenu
    d.click("scopes.wrench");
    assert!(d.has("scopes.menu.vectorscopeYuv"), "{:?}", d.ids("scopes.menu"));
    d.click("scopes.menu.vectorscopeYuv");
    d.wait_for("scopes.view.vectorscopeYuv");
    assert!(d.has("scopes.view.parade"));
    // Clamp Signal in the footer
    d.click("scopes.clamp");
    assert_eq!(d.ui_panels()["scopes"]["clamp"], json!(false));
    // all five scopes in a grid, through ui.set; then a preset
    d.ok("ui.set", json!({"panels": {"scopes": {"shown": ["vectorscopeYuv", "vectorscopeHls", "histogram", "parade", "waveform"], "waveformType": "yc", "paradeType": "rgbWhite", "brightness": "bright", "targets": "100"}}}));
    for k in ["vectorscopeYuv", "vectorscopeHls", "histogram", "parade", "waveform"] {
        d.wait_for(&format!("scopes.view.{k}"));
    }
    let rects: Vec<Value> = d.ok("ui.elements", json!({"prefix": "scopes.view."})).as_array().unwrap().iter().map(|e| e["rect"].clone()).collect();
    assert_eq!(rects.len(), 5);
    d.snapshot("panels-scopes-five", Some("scopes."));
    d.click("scopes.wrench");
    d.ok("ui.click", json!({"id": "scopes.menu.presets"}));
    d.frames(3);
    d.wait_for("scopes.menu.preset.5");
    d.click("scopes.menu.preset.5");
    let s = d.ui_panels()["scopes"].clone();
    assert_eq!(s["preset"], json!("Four Scopes (YUV, float, no clamp)"), "{s}");
    assert_eq!((s["scale"].clone(), s["clamp"].clone(), s["paradeType"].clone()), (json!("float"), json!(false), json!("yuv")));
    assert_eq!(s["shown"].as_array().unwrap().len(), 4);
    // colour space: Rec. 601 shows in the footer
    d.ok("ui.set", json!({"panels": {"scopes": {"colorSpace": "rec601"}}}));
    d.frames(3);
    assert_eq!(d.label("scopes.space"), "Rec. 601");
    // the numbers behind it, for agents
    let r = d.exec("scopes.read", json!({"scopes": ["histogram"], "colorSpace": "601", "bins": false}));
    assert_eq!(r["matrix"], "Bt601");
    assert!(r["histogram"]["samples"].as_u64().unwrap() > 1000, "{r}");
}

#[test]
fn metadata_panel_edits_a_field_with_undo() {
    let mut d = Driver::demo();
    let item = d.app().session.project.items.values().filter(|i| i.as_media().is_some()).map(|i| i.id).min().unwrap();
    d.app().session.state.project_selection = vec![item];
    d.app().ui.focused = filmcraft_ui_egui::dock::PanelKind::Project;
    d.ok("ui.menu.invoke", json!({"id": "window.panel.Metadata"}));
    d.ok("ui.set", json!({"focused": "Project"}));
    d.frames(3);
    let name = d.app().session.project.item(item).unwrap().name.clone();
    assert_eq!(d.label("metadata.header"), format!("Clip: {name}"));
    for id in [
        "metadata.field.Name",
        "metadata.label",
        "metadata.field.MediaStart",
        "metadata.field.MediaDuration",
        "metadata.field.Scene",
        "metadata.field.LogNote",
        "metadata.field.FilePath",
        "metadata.section.File",
    ] {
        assert!(d.has(id), "{id} missing: {:?}", d.ids("metadata."));
    }
    d.snapshot("panels-metadata", Some("metadata."));
    // type into Scene and commit with Enter: one undo step
    let undo0 = d.app().session.history.undo.len();
    d.click("metadata.field.Scene");
    d.ok("ui.type", json!({"text": "42A"}));
    d.frames(2);
    d.ok("ui.key", json!({"key": "Enter"}));
    d.frames(3);
    assert_eq!(d.app().session.project.item(item).unwrap().metadata.get("Scene").map(String::as_str), Some("42A"));
    assert_eq!(d.app().session.history.undo.len(), undo0 + 1);
    assert_eq!(d.label("metadata.field.Scene"), "42A");
    d.exec("edit.undo", json!({}));
    d.frames(2);
    assert!(!d.app().session.project.item(item).unwrap().metadata.contains_key("Scene"));
    // the same edit from an agent
    d.exec("metadata.set", json!({"item": item.0, "field": "Tape Name", "value": "A001"}));
    d.frames(2);
    assert_eq!(d.label("metadata.field.TapeName"), "A001");
}

/// #620: notes typed into Project Notes land in the project; one typing session is one undo step.
#[test]
fn project_notes_panel_types_into_the_project_with_undo() {
    let mut d = Driver::demo();
    d.ok("ui.menu.invoke", json!({"id": "window.panel.ProjectNotes"}));
    d.frames(3);
    let undo0 = d.app().session.history.undo.len();
    d.click("notes.text");
    d.ok("ui.type", json!({"text": "Fix logo"}));
    d.frames(2);
    d.ok("ui.key", json!({"key": "Enter"}));
    d.ok("ui.type", json!({"text": "at 1:02"}));
    d.frames(3);
    assert_eq!(d.app().session.project.notes, "Fix logo\nat 1:02");
    assert_eq!(d.app().session.history.undo.len(), undo0 + 1);
    assert_eq!(d.label("notes.text"), "Fix logo\nat 1:02");
    d.exec("edit.undo", json!({}));
    d.frames(2);
    assert_eq!(d.app().session.project.notes, "");
    // an agent writes them, the panel shows them
    d.exec("project.setNotes", json!({"text": "From the script"}));
    d.frames(2);
    assert_eq!(d.label("notes.text"), "From the script");
}

#[test]
fn timecode_panel_rows_and_modes() {
    let mut d = Driver::demo();
    d.exec("playhead.set", json!({"timecode": "00:00:02:00"}));
    d.ok("ui.menu.invoke", json!({"id": "window.panel.Timecode"}));
    d.frames(3);
    assert_eq!(d.label("timecode.row.0"), "00:00:02:00");
    d.click("timecode.addRow");
    assert_eq!(d.ui_panels()["timecode"]["rows"].as_array().unwrap().len(), 2);
    // the new row is a duration: the sequence's
    let (dur, df, rate) = {
        let q = d.app().session.active_sequence().unwrap();
        (q.duration(), q.settings.drop_frame, q.settings.frame_rate)
    };
    let want = filmcraft_time::format_time(dur, rate, df, filmcraft_time::TimeDisplay::Timecode, 48000);
    assert_eq!(d.label("timecode.row.1"), want);
    // frames display and Remaining through ui.set
    d.ok("ui.set", json!({"panels": {"timecode": {"rows": [{"mode": "current", "display": "Frames"}, {"mode": "remaining"}]}}}));
    d.frames(2);
    let frame = rate.frame_at(d.app().session.playhead());
    assert_eq!(d.label("timecode.row.0"), frame.to_string());
    let rem = filmcraft_time::format_time(dur - d.app().session.playhead(), rate, df, filmcraft_time::TimeDisplay::Timecode, 48000);
    assert_eq!(d.label("timecode.row.1"), rem);
    d.snapshot("panels-timecode", Some("timecode."));
    // right-click menu: Media Time
    d.ok("ui.click", json!({"id": "timecode.row.1", "button": "right"}));
    d.frames(2);
    if d.has("timecode.row.1.mode.media") {
        d.click("timecode.row.1.mode.media");
        assert_eq!(d.ui_panels()["timecode"]["rows"][1]["mode"], json!("media"));
    }
}

#[test]
fn events_and_progress_panels() {
    let mut d = Driver::demo();
    d.exec("events.clear", json!({}));
    d.ok("ui.menu.invoke", json!({"id": "window.panel.Events"}));
    d.frames(2);
    // a failed command shows up as an error row
    let r = d.call("engine.execute", json!({"command": "clip.rename", "params": {}}));
    assert_eq!(r["ok"], json!(false));
    d.frames(2);
    let rows = d.ids("events.row.");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(d.label(&rows[0]).contains("name"), "{}", d.label(&rows[0]));
    d.click(&rows[0]);
    assert!(d.has("events.details"));
    d.snapshot("panels-events", Some("events."));
    // filter: errors only still shows it; Clear All empties the list
    d.ok("ui.set", json!({"panels": {"events": {"level": "error"}}}));
    d.frames(2);
    assert_eq!(d.ids("events.row.").len(), 1);
    d.click("events.clearAll");
    assert!(d.ids("events.row.").is_empty());
    assert!(d.app().session.log.entries.is_empty());

    // Progress: a running job with a Cancel button
    let job = filmcraft_engine::Job {
        id: 4242,
        label: "Export demo.mp4".into(),
        progress: Arc::new(Default::default()),
        result: Arc::new(std::sync::Mutex::new(None)),
    };
    job.progress.total.store(10, std::sync::atomic::Ordering::Relaxed);
    job.progress.done.store(4, std::sync::atomic::Ordering::Relaxed);
    d.app().session.jobs.push(job.clone());
    d.ok("ui.menu.invoke", json!({"id": "window.panel.Progress"}));
    d.frames(3);
    assert!(d.label("progress.job.4242").contains("40 %"), "{}", d.label("progress.job.4242"));
    d.snapshot("panels-progress", Some("progress."));
    d.click("progress.cancel.4242");
    assert!(job.progress.cancel.load(std::sync::atomic::Ordering::Relaxed));
    job.progress.cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
    *job.result.lock().unwrap() = Some(Err("cancelled".into()));
    d.frames(3);
    assert!(!d.has("progress.cancel.4242"));
    assert!(d.label("progress.job.4242").contains("Cancelled"));
    // the Events panel logged the job
    let ev = d.exec("events.list", json!({}));
    let msgs: Vec<&str> = ev["entries"].as_array().unwrap().iter().filter_map(|e| e["message"].as_str()).collect();
    assert!(msgs.contains(&"Started: Export demo.mp4") && msgs.contains(&"Cancelled: Export demo.mp4"), "{msgs:?}");
    // hide finished jobs
    d.click("progress.showFinished");
    assert!(!d.has("progress.job.4242"));
}

#[test]
fn reference_monitor_parks_gangs_and_shows_scopes() {
    let mut d = Driver::demo();
    d.ok("ui.menu.invoke", json!({"id": "window.panel.ReferenceMonitor"}));
    d.frames(3);
    assert_eq!(d.label("reference.timecode"), "00:00:00:00");
    d.exec("playhead.set", json!({"timecode": "00:00:03:00"}));
    d.frames(2);
    // parked: the playhead moved, the reference did not
    assert_eq!(d.label("reference.timecode"), "00:00:00:00");
    d.click("reference.matchPlayhead");
    assert_eq!(d.label("reference.timecode"), "00:00:03:00");
    d.click("reference.stepForward");
    assert_eq!(d.label("reference.timecode"), "00:00:03:01");
    // ganged: follows the playhead
    d.click("reference.gang");
    assert_eq!(d.ui_panels()["reference"]["ganged"], json!(true));
    d.exec("playhead.set", json!({"timecode": "00:00:01:00"}));
    d.frames(2);
    assert_eq!(d.label("reference.timecode"), "00:00:01:00");
    d.wait_for("reference.picture");
    d.snapshot("panels-reference-composite", Some("reference."));
    // scopes of the reference frame
    d.ok("ui.set", json!({"panels": {"reference": {"display": "scopes"}}}));
    d.wait_for("reference.scopes.view.vectorscopeYuv");
    d.wait_for("reference.scopes.view.waveform");
    d.snapshot("panels-reference-scopes", Some("reference."));
}

#[test]
fn scopes_of_colour_bars() {
    let mut s = Session::default();
    let id = s.execute("file.newBarsAndTone", json!({"seconds": 2})).unwrap()["item"].as_u64().unwrap();
    s.execute("file.newSequenceFromClip", json!({"items": [id]})).unwrap();
    let mut d = Driver::with(s);
    d.ok(
        "ui.set",
        json!({"workspace": "Color", "panels": {"scopes": {"shown": ["vectorscopeYuv", "histogram", "parade", "waveform"], "waveformType": "luma"}}}),
    );
    d.ok("ui.panel.show", json!({"panel": "LumetriScopes"}));
    for k in ["vectorscopeYuv", "histogram", "parade", "waveform"] {
        d.wait_for(&format!("scopes.view.{k}"));
    }
    d.snapshot("panels-scopes-bars", Some("scopes."));
    // the numbers: the six 75 % bars sit on their targets
    let r = d.exec("scopes.read", json!({"scopes": ["vectorscopeYuv", "parade"], "columns": 8}));
    let peaks = r["vectorscopeYuv"]["peaks"].as_array().unwrap();
    let angles: Vec<f64> = peaks.iter().take(7).map(|p| p["angleDeg"].as_f64().unwrap()).collect();
    for t in filmcraft_scopes::targets(filmcraft_scopes::Matrix::Bt709, 191.0 / 255.0) {
        let a = filmcraft_scopes::angle_deg(t.cb, t.cr) as f64;
        assert!(angles.iter().any(|x| (x - a).abs() < 1.5), "{} at {a:.1}° not in {angles:?}", t.name);
    }
}

/// #1075: child backgrounds must not cover the focus outline shared with Program. Check the
/// rendered primitives, including Media Browser's parent group, in both requested themes.
#[test]
fn focused_panel_outline_is_not_covered_by_child_backgrounds() {
    fn rectangles<'a>(shape: &'a egui::Shape, clip: egui::Rect, out: &mut Vec<(&'a egui::epaint::RectShape, egui::Rect)>) {
        match shape {
            egui::Shape::Rect(rect) => out.push((rect, clip)),
            egui::Shape::Vec(shapes) => shapes.iter().for_each(|shape| rectangles(shape, clip, out)),
            _ => {}
        }
    }
    let mut d = Driver::demo();
    for theme in ["dark", "light"] {
        d.ok("ui.set", json!({"theme": theme}));
        for panel in ["Program", "Text", "LumetriScopes", "EssentialSound", "Timeline", "TextToSpeech", "MediaBrowser"] {
            d.ok("ui.panel.show", json!({"panel": panel}));
            d.ok("ui.set", json!({"focused": panel}));
            d.frames(3);
            d.snapshot(&format!("focus-{theme}-{panel}"), None);
            let mut rects = Vec::new();
            for shape in &d.harness.output().shapes {
                rectangles(&shape.shape, shape.clip_rect, &mut rects);
            }
            let focus = d.harness.state().tokens.focus;
            let outlines: Vec<_> = rects
                .iter()
                .enumerate()
                .filter(|(_, (r, _))| r.stroke.color == focus && r.stroke.width == 1.0 && r.rect.width() > 100.0 && r.rect.height() > 80.0)
                .collect();
            assert_eq!(outlines.len(), 1, "{theme} {panel}: one focus outline for the whole parent group");
            let (at, (outline, _)) = outlines[0];
            let bounds = outline.rect;
            let strips = [
                egui::Rect::from_min_max(bounds.min, egui::pos2(bounds.max.x, bounds.min.y + 1.0)),
                egui::Rect::from_min_max(egui::pos2(bounds.min.x, bounds.max.y - 1.0), bounds.max),
                egui::Rect::from_min_max(bounds.min, egui::pos2(bounds.min.x + 1.0, bounds.max.y)),
                egui::Rect::from_min_max(egui::pos2(bounds.max.x - 1.0, bounds.min.y), bounds.max),
            ];
            for (child, clip) in &rects[at + 1..] {
                if child.fill.a() != 0 {
                    let painted = child.rect.intersect(*clip);
                    assert!(
                        strips.iter().all(|edge| !edge.intersect(painted).is_positive()),
                        "{theme} {panel}: a later child background {painted:?} covers the focus outline {bounds:?}"
                    );
                }
            }
        }
    }
}

/// Select two different sources using the same Project-row and Timeline-clip clicks as an editor.
fn inspector_selection_driver() -> (Driver, filmcraft_project::ItemId, filmcraft_project::ItemId, filmcraft_project::ClipId) {
    let mut d = Driver::demo();
    let seq = d.app().session.active_sequence().unwrap();
    let a = seq.video_tracks[0].items[0].item;
    let clip = seq.video_tracks[0].items.iter().find(|clip| clip.item != a).unwrap();
    let (b, clip) = (clip.item, clip.id);
    let bin = d
        .app()
        .session
        .project
        .root
        .children
        .iter()
        .find_map(|entry| match entry {
            filmcraft_project::BinEntry::Bin(bin) if bin.name == "Footage" => Some(bin.id.0),
            _ => None,
        })
        .unwrap();
    d.exec("project.view.set", json!({"view":"list"}));
    d.ok("ui.menu.invoke", json!({"id":"window.panel.Metadata"}));
    d.ok("ui.menu.invoke", json!({"id":"window.panel.Info"}));
    d.click("panel.tab.Project");
    d.ok("ui.menu.invoke", json!({"id":"projectPanel.openBin","params":{"bin":bin,"how":"inPlace"}}));
    d.click(&format!("project.item.{}", a.0));
    assert_eq!(d.app().session.state.project_selection, vec![a]);
    d.click(&format!("timeline.clip.{}", clip.0));
    assert!(d.app().session.state.selection.contains(&clip));
    (d, a, b, clip)
}

#[test]
fn metadata_keeps_the_timeline_item_when_its_tab_and_field_take_focus() {
    let (mut d, a, b, _) = inspector_selection_driver();
    let b_name = d.app().session.project.item(b).unwrap().name.clone();
    assert_eq!(d.label("metadata.header"), format!("Clip: {b_name}"));
    d.click("panel.tab.Metadata");
    assert_eq!(d.label("metadata.header"), format!("Clip: {b_name}"), "inspector focus must not reactivate the old Project selection");
    let before = serde_json::to_value(&d.app().session.project).unwrap();
    let undo = d.app().session.history.undo.len();
    d.click("metadata.field.Scene");
    d.ok("ui.type", json!({"text":"42B"}));
    d.ok("ui.key", json!({"key":"Enter"}));
    d.frames(3);
    assert_eq!(d.app().session.project.item(b).unwrap().metadata.get("Scene").map(String::as_str), Some("42B"));
    assert!(!d.app().session.project.item(a).unwrap().metadata.contains_key("Scene"));
    assert_eq!(d.app().session.history.undo.len(), undo + 1);
    let after = serde_json::to_value(&d.app().session.project).unwrap();
    d.exec("edit.undo", json!({}));
    assert_eq!(serde_json::to_value(&d.app().session.project).unwrap(), before);
    d.exec("edit.redo", json!({}));
    assert_eq!(serde_json::to_value(&d.app().session.project).unwrap(), after);
    d.snapshot("panels-metadata-timeline-selection", Some("metadata."));
}

#[test]
fn info_follows_the_timeline_item_and_keeps_sequence_information() {
    let (mut d, _, b, _) = inspector_selection_driver();
    let name = d.app().session.project.item(b).unwrap().name.clone();
    d.click("panel.tab.Info");
    assert!(d.harness.query_by_label(&name).is_some(), "Info must show the selected Timeline item's name");
    assert_eq!(d.label("info.item"), name, "the item header belongs to Info, not another panel");
    assert!(!d.label("info.sequence").is_empty());
    assert!(d.harness.query_by_label("Settings:").is_some(), "Info keeps its lower sequence-information section");
    d.snapshot("panels-info-timeline-selection", None);
}

#[test]
fn inspectors_follow_reselected_project_items_and_clear_without_stale_targets() {
    let (mut d, a, b, clip) = inspector_selection_driver();
    let a_name = d.app().session.project.item(a).unwrap().name.clone();
    let b_name = d.app().session.project.item(b).unwrap().name.clone();
    let timeline_selection = d.app().session.state.selection.clone();
    let undo = d.app().session.history.undo.len();
    let playhead = d.app().session.playhead();
    d.click("panel.tab.Info");
    assert_eq!(d.label("info.item"), b_name);
    // Selection ownership also follows actual source clicks while both inspectors are hidden.
    d.ok("ui.menu.invoke", json!({"id":"window.panel.Source"}));
    d.click("panel.tab.Project");
    d.click(&format!("project.item.{}", a.0));
    d.click(&format!("timeline.clip.{}", clip.0));
    d.ok("ui.menu.invoke", json!({"id":"window.panel.Metadata"}));
    d.frames(3);
    assert_eq!(d.label("metadata.header"), format!("Clip: {b_name}"));
    d.click("panel.tab.Project");
    d.click(&format!("project.item.{}", a.0));
    d.click("panel.tab.Metadata");
    assert_eq!(d.label("metadata.header"), format!("Clip: {a_name}"));
    d.click("panel.tab.Info");
    assert_eq!(d.label("info.item"), a_name);
    assert_eq!(d.app().session.state.selection, timeline_selection);
    assert_eq!(d.app().session.state.project_selection, vec![a]);
    assert_eq!(d.app().session.history.undo.len(), undo, "inspector focus does not create edits");
    assert_eq!(d.app().session.playhead(), playhead);

    // Preserve Metadata's existing fallback in both directions when a selection is cleared.
    d.click(&format!("timeline.clip.{}", clip.0));
    d.ok("ui.key", json!({"key":"Cmd+Shift+A"}));
    assert!(d.app().session.state.selection.is_empty());
    d.click("panel.tab.Metadata");
    assert_eq!(d.label("metadata.header"), format!("Clip: {a_name}"));
    d.click("panel.tab.Info");
    assert_eq!(d.label("info.item"), a_name);
    d.click(&format!("timeline.clip.{}", clip.0));
    d.click("panel.tab.Project");
    d.ok("ui.key", json!({"key":"Cmd+Shift+A"}));
    assert!(d.app().session.state.project_selection.is_empty());
    d.click("panel.tab.Info");
    assert_eq!(d.label("info.item"), b_name);
    d.click("panel.tab.Metadata");
    assert_eq!(d.label("metadata.header"), format!("Clip: {b_name}"));
    d.ok("ui.set", json!({"theme":"light"}));
    d.click("panel.tab.Info");
    assert_eq!(d.label("info.item"), b_name, "theme and inspector changes keep the selection source");
    d.snapshot("panels-info-timeline-selection-light", Some("info."));

    d.click(&format!("timeline.clip.{}", clip.0));
    d.ok("ui.key", json!({"key":"Cmd+Shift+A"}));
    assert!(d.app().session.state.selection.is_empty());
    d.click("panel.tab.Metadata");
    assert!(d.has("metadata.empty"));
    d.click("panel.tab.Info");
    assert!(!d.has("info.item"));
    assert!(!d.label("info.sequence").is_empty(), "active sequence details remain with no clip selected");

    d.click("panel.tab.Project");
    d.click(&format!("project.item.{}", a.0));
    d.click("panel.tab.Info");
    assert_eq!(d.label("info.item"), a_name);
    d.exec("file.newProject", json!({"name":"Fresh inspector project"}));
    d.frames(3);
    assert!(!d.has("info.item"), "no item identity is retained from the old project");
    assert!(!d.has("info.sequence"));
    d.click("panel.tab.Metadata");
    assert!(d.has("metadata.empty"));
}

#[test]
fn inspector_source_defaults_when_loading_an_older_ui_state() {
    let ui = filmcraft_ui_egui::state::UiState::default();
    let mut saved = serde_json::to_value(&ui).unwrap();
    saved["panels"].as_object_mut().unwrap().remove("inspectorSource");
    let restored: filmcraft_ui_egui::state::UiState = serde_json::from_value(saved).unwrap();
    assert_eq!(restored.panels.inspector_source, filmcraft_ui_egui::panels::panel_state::InspectorSource::Project);
    assert_eq!(restored.panels.scopes, ui.panels.scopes);
}

#[test]
fn middle_closing_an_inactive_project_tab_keeps_the_inspector_target() {
    let (mut d, _, b, _) = inspector_selection_driver();
    let name = d.app().session.project.item(b).unwrap().name.clone();
    d.click("panel.tab.Info");
    assert_eq!(d.label("info.item"), name);
    assert!(d.has("panel.tab.Project"), "the inactive Project tab must be visible before closing it");
    let project = serde_json::to_value(&d.app().session.project).unwrap();
    let state = d.exec("state.inspect", json!({}));
    let history = d.exec("history.list", json!({}));
    let revision = d.app().session.revision;

    d.ok("ui.click", json!({"id":"panel.tab.Project", "button":"middle"}));
    d.frames(3);
    assert!(!d.has("panel.tab.Project"), "middle-click closes the inactive tab");
    assert_eq!(serde_json::to_value(&d.app().session.project).unwrap(), project);
    assert_eq!(d.exec("state.inspect", json!({})), state);
    assert_eq!(d.exec("history.list", json!({})), history);
    assert_eq!(d.app().session.revision, revision);
    let focused = d.app().ui.focused;
    let source = d.app().ui.panels.inspector_source;
    assert_eq!(d.label("info.item"), name, "closing a tab must not select its stale item; focused={focused:?}, source={source:?}");
}

#[cfg(target_os = "linux")]
mod progress_export_io_errors {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Mutex, mpsc};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    struct ExportDir(std::path::PathBuf);

    impl Drop for ExportDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn assert_async_write_failure(parent_name: &str, contains_cancel: bool) {
        // A relative destination keeps checkout and TMPDIR names out of the error-string control.
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = ExportDir(std::path::Path::new("target").join(format!("progress-write-error-{}-{stamp}-{parent_name}", std::process::id())));
        std::fs::create_dir_all(&dir.0).unwrap();
        let parent = dir.0.join(parent_name);
        let original = b"ordinary file, not an output directory";
        std::fs::write(&parent, original).unwrap();
        let output = parent.join("clip.wav");
        let path = output.to_str().unwrap().to_string();
        assert_eq!(path.to_ascii_lowercase().contains("cancel"), contains_cancel);

        let mut d = Driver::demo();
        let before_jobs: Vec<u64> = d.app().session.jobs.iter().map(|job| job.id).collect();
        let started = d.exec(
            "file.exportMedia",
            json!({
                "path": path,
                "preset": "Waveform Audio 48 kHz 16-bit",
                "range": "custom",
                "startSeconds": 0.0,
                "endSeconds": 0.02,
                "captionSidecar": "none",
                "wait": false
            }),
        );
        let id = started["job"].as_u64().expect("the async request must return a job, not a synchronous path error");
        assert!(!before_jobs.contains(&id));
        assert_eq!(d.app().session.jobs.len(), before_jobs.len() + 1);
        assert_eq!(started["path"], json!(path));
        let job = d.app().session.jobs.iter().find(|job| job.id == id).unwrap().clone();
        let deadline = Instant::now() + Duration::from_secs(30);
        let error = loop {
            let result = job.result.lock().unwrap().clone();
            if let Some(result) = result {
                break result.expect_err("the regular-file parent must make the actual WAV export fail");
            }
            assert!(Instant::now() < deadline, "async export job {id} did not finish");
            d.frames(1);
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(job.progress.finished.load(Ordering::Relaxed));
        assert!(!job.progress.cancel.load(Ordering::Relaxed), "no cancellation was requested");
        assert!(error.starts_with("I/O: "), "must reach the actual output write failure: {error}");
        assert!(error.contains(&path), "the failure must name the requested destination: {error}");
        assert_eq!(error.to_ascii_lowercase().contains("cancel"), contains_cancel, "{error}");
        assert_ne!(error, "cancelled");
        assert_eq!(std::fs::read(&parent).unwrap(), original);
        assert!(!output.exists(), "no WAV can be created through a regular-file parent");

        let events = d.exec("events.list", json!({}));
        let failed = format!("Failed: {}: {error}", job.label);
        assert!(
            events["entries"].as_array().unwrap().iter().any(|entry| { entry["source"] == "job" && entry["level"] == "error" && entry["message"] == failed }),
            "the real terminal failure must be logged: {events}"
        );
        d.ok("ui.menu.invoke", json!({"id": "window.panel.Progress"}));
        if !d.app().ui.panels.progress.show_finished {
            d.click("progress.showFinished");
        }
        let element = format!("progress.job.{id}");
        d.wait_for(&element);
        assert_eq!(d.label(&element), format!("{}: Failed: {error}", job.label), "an I/O failure must keep its diagnostic");
        assert!(!job.progress.cancel.load(Ordering::Relaxed));
        assert!(!job.progress.cancelled.load(Ordering::Relaxed));

        let result = job.to_json()["result"].clone();
        d.exec("jobs.cancel", json!({"job": id}));
        d.frames(2);
        assert!(job.progress.cancel.load(Ordering::Relaxed), "the late request reached the completed job");
        assert!(!job.progress.cancelled.load(Ordering::Relaxed), "a request cannot change the observed terminal cause");
        assert_eq!(job.to_json()["result"], result);
        assert_eq!(d.label(&element), format!("{}: Failed: {error}", job.label));
    }

    #[test]
    fn async_export_io_error_with_cancel_in_path_stays_failed() {
        assert_async_write_failure("cancel-output", true);
    }

    #[test]
    fn async_export_io_error_with_neutral_path_stays_failed() {
        assert_async_write_failure("ordinary-output", false);
    }

    fn export_dir(name: &str) -> ExportDir {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = ExportDir(std::path::Path::new("target").join(format!("progress-terminal-{}-{stamp}-{name}", std::process::id())));
        std::fs::create_dir_all(&dir.0).unwrap();
        dir
    }

    fn progress_row(d: &mut Driver, id: u64) -> String {
        d.ok("ui.menu.invoke", json!({"id": "window.panel.Progress"}));
        if !d.app().ui.panels.progress.show_finished {
            d.click("progress.showFinished");
        }
        let element = format!("progress.job.{id}");
        d.wait_for(&element);
        element
    }

    struct PausedAudio {
        inner: filmcraft_media::SharedSource,
        first: AtomicBool,
        entered: mpsc::Sender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }

    impl filmcraft_media::MediaSource for PausedAudio {
        fn info(&self) -> &filmcraft_media::MediaInfo {
            self.inner.info()
        }
        fn video_frame(&self, req: filmcraft_media::FrameRequest) -> filmcraft_media::Result<Arc<filmcraft_frame::VideoFrame>> {
            self.inner.video_frame(req)
        }
        fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<filmcraft_frame::AudioBuffer> {
            if self.first.swap(false, Ordering::Relaxed) {
                self.entered.send(()).map_err(|e| filmcraft_media::MediaError::Io(e.to_string()))?;
                // A failed test drops the sender; a timeout also releases the producer.
                self.release.lock().unwrap().recv_timeout(Duration::from_secs(30)).map_err(|e| filmcraft_media::MediaError::Io(e.to_string()))?;
            }
            self.inner.audio(start, frames, sample_rate)
        }
    }

    // Own the output until the asynchronous producer has published its terminal result.
    // Session/Job drops do not join the export thread.
    struct PausedExportCleanup {
        dir: Option<ExportDir>,
        release: Option<mpsc::Sender<()>>,
        job: Option<filmcraft_engine::Job>,
    }

    impl Drop for PausedExportCleanup {
        fn drop(&mut self) {
            if let Some(job) = &self.job {
                job.progress.cancel.store(true, Ordering::Relaxed);
            }
            // Disconnect the gate even when a test assertion failed before releasing it.
            drop(self.release.take());
            let started = Instant::now();
            let terminal = match &self.job {
                Some(job) => loop {
                    let done = match job.result.try_lock() {
                        Ok(result) => result.is_some(),
                        Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner().is_some(),
                        Err(std::sync::TryLockError::WouldBlock) => false,
                    };
                    if done {
                        break true;
                    }
                    if started.elapsed() >= Duration::from_secs(30) {
                        break false;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                },
                None => false,
            };
            if !terminal && let Some(dir) = self.dir.take() {
                // A failed/unknown producer may still use the output. Preserve its fixture.
                std::mem::forget(dir);
            }
        }
    }

    #[test]
    fn actual_export_cancellation_is_reported_by_the_progress_panel() {
        let dir = export_dir("stopped");
        let path = dir.0.join("stopped.wav").to_str().unwrap().to_string();
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).unwrap();
        // Finish UI initialization and open Progress before the producer's finite gate starts.
        let mut d = Driver::with(session);
        d.ok("ui.menu.invoke", json!({"id": "window.panel.Progress"}));
        d.frames(3);
        if !d.app().ui.panels.progress.show_finished {
            d.click("progress.showFinished");
        }
        let session = &mut d.app().session;
        let item = session.active_sequence().unwrap().audio_tracks[0].items[0].item;
        let inner = session.source(item).unwrap();
        assert_eq!(inner.info().audio_streams.len(), 1);
        let (entered, ready) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        session.media.insert(item, Arc::new(PausedAudio { inner, first: AtomicBool::new(true), entered, release: Mutex::new(resume) }));
        let previous_jobs = session.jobs.len();
        let mut cleanup = PausedExportCleanup { dir: Some(dir), release: Some(release), job: None };
        let started = session.execute(
            "file.exportMedia",
            json!({"path": path, "format": "wav", "range": "custom", "startSeconds": 0, "endSeconds": 2,
                "captionSidecar": "none", "wait": false}),
        );
        // Capture the appended job before checking the command result or any returned field.
        cleanup.job = session.jobs.get(previous_jobs).cloned();
        let started = started.unwrap();
        let id = started["job"].as_u64().unwrap();
        let job = session.jobs.iter().find(|job| job.id == id).unwrap().clone();
        // No UI frames run after replacing the source until the export owns its first read.
        ready.recv_timeout(Duration::from_secs(30)).expect("the real export must read the gated source");
        assert!(job.result.lock().unwrap().is_none());
        assert!(!job.progress.cancelled.load(Ordering::Relaxed));
        let element = format!("progress.job.{id}");
        d.wait_for(&element);
        let cancel = format!("progress.cancel.{id}");
        d.wait_for(&cancel);
        assert!(job.result.lock().unwrap().is_none(), "the producer must still be paused before Cancel");
        d.click(&cancel);
        assert!(job.progress.cancel.load(Ordering::Relaxed));
        assert!(job.result.lock().unwrap().is_none(), "the producer is still paused");
        cleanup.release.as_ref().unwrap().send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let result = loop {
            if let Some(result) = job.result.lock().unwrap().clone() {
                break result;
            }
            assert!(Instant::now() < deadline, "the actual cancelled producer did not finish");
            d.frames(1);
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(result.unwrap_err(), "cancelled");
        assert!(job.progress.cancelled.load(Ordering::Relaxed), "the producer must publish its observed cancellation");
        assert!(job.progress.finished.load(Ordering::Relaxed));
        d.frames(2);
        assert_eq!(d.label(&element), format!("{}: Cancelled", job.label));
        assert!(!d.has(&format!("progress.cancel.{id}")));
    }

    #[test]
    fn completed_export_stays_done_after_a_late_cancel_request() {
        let dir = export_dir("completed");
        let path = dir.0.join("completed.wav").to_str().unwrap().to_string();
        let mut d = Driver::demo();
        let completed = d.exec(
            "file.exportMedia",
            json!({"path": path, "format": "wav", "range": "custom", "startSeconds": 0, "endSeconds": 0.02,
                "captionSidecar": "none", "wait": true}),
        );
        let id = completed["job"].as_u64().unwrap();
        let job = d.app().session.jobs.iter().find(|job| job.id == id).unwrap().clone();
        assert!(job.result.lock().unwrap().as_ref().unwrap().is_ok());
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(b"RIFF"));
        assert_eq!(completed["result"]["bytes"], json!(bytes.len()));
        assert!(bytes.len() > 44);
        let element = progress_row(&mut d, id);
        assert_eq!(d.label(&element), format!("{}: Done", job.label));
        let result = job.to_json()["result"].clone();
        d.exec("jobs.cancel", json!({"job": id}));
        d.frames(2);
        assert!(job.progress.cancel.load(Ordering::Relaxed));
        assert!(!job.progress.cancelled.load(Ordering::Relaxed));
        assert_eq!(job.to_json()["result"], result);
        assert_eq!(d.label(&element), format!("{}: Done", job.label));
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
}

mod progress_keyboard_diagnostics {
    use super::*;
    use egui::{Event, Id, Key, Modifiers, Shape};
    use filmcraft_engine::Job;
    use filmcraft_ui_egui::dock::{DockNode, PanelKind};
    use std::sync::atomic::Ordering;

    fn job(id: u64, label: &str, error: Option<&str>) -> Job {
        let job = Job {
            id,
            label: label.into(),
            progress: Arc::new(Default::default()),
            result: Arc::new(std::sync::Mutex::new(error.map(|error| Err(error.to_owned())))),
        };
        job.progress.total.store(10, Ordering::Relaxed);
        job.progress.done.store(if error.is_some() { 10 } else { 4 }, Ordering::Relaxed);
        job.progress.finished.store(error.is_some(), Ordering::Relaxed);
        *job.progress.error.lock().unwrap() = error.map(str::to_owned);
        job
    }

    fn tab(d: &mut Driver, modifiers: Modifiers) {
        for pressed in [true, false] {
            d.harness.event(Event::PointerGone);
            d.harness.event(Event::Key { key: Key::Tab, physical_key: None, pressed, repeat: false, modifiers });
            d.frames(1);
        }
        d.frames(2);
        assert!(d.harness.state().ui_error.is_none());
        assert!(d.harness.ctx.input(|input| input.pointer.hover_pos().is_none()));
    }

    fn painted(d: &Driver, predicate: impl Fn(&Shape) -> bool) -> bool {
        fn any(shape: &Shape, predicate: &dyn Fn(&Shape) -> bool) -> bool {
            predicate(shape)
                || match shape {
                    Shape::Vec(shapes) => shapes.iter().any(|shape| any(shape, predicate)),
                    _ => false,
                }
        }
        d.harness.output().shapes.iter().any(|shape| any(&shape.shape, &predicate))
    }

    fn full_detail(d: &Driver, text: &str) -> bool {
        painted(d, |shape| matches!(shape, Shape::Text(shape) if shape.galley.text() == text))
    }

    fn assert_focused_detail(d: &Driver, id: Id, text: &str) {
        assert_eq!(d.harness.ctx.memory(|memory| memory.focused()), Some(id));
        let response = d.harness.ctx.read_response(id).unwrap();
        assert!(response.has_focus());
        assert!(!response.hovered());
        assert!(full_detail(d, text), "keyboard focus must paint the complete diagnostic");
        assert!(
            painted(d, |shape| matches!(shape, Shape::Rect(rect)
                if rect.rect == response.rect.shrink(2.0)
                    && rect.stroke.color == d.harness.state().tokens.focus
                    && rect.stroke.width == 1.5)),
            "the completed row needs a visible focus outline"
        );
    }

    fn document_state(d: &mut Driver) -> Value {
        let app = d.app();
        let history = app.session.execute("history.list", json!({})).unwrap();
        json!({
            "project": app.session.project.as_ref(),
            "editor": app.session.state,
            "history": history,
            "revision": app.session.revision,
            "savedRevision": app.session.saved_revision,
            "path": app.session.path,
            "playing": app.playback.playing,
        })
    }

    fn job_state(jobs: &[Job]) -> Value {
        json!(
            jobs.iter()
                .map(|job| {
                    let mut public = job.to_json();
                    // ETA is a time-dependent observation, not a job mutation.
                    public.as_object_mut().unwrap().remove("etaSeconds");
                    json!({
                        "public": public,
                        "cancel": job.progress.cancel.load(Ordering::Relaxed),
                        "cancelled": job.progress.cancelled.load(Ordering::Relaxed),
                        "producerFinished": job.progress.finished.load(Ordering::Relaxed),
                        "error": job.progress.error.lock().unwrap().clone(),
                    })
                })
                .collect::<Vec<_>>()
        )
    }

    #[test]
    fn completed_progress_diagnostics_support_keyboard_focus_without_hover() {
        for screen_reader in [false, true] {
            let error = format!("I/O: /exports/{}final.wav: permission denied\nComplete diagnostic tail: Ω", "long-component/".repeat(18),);
            let jobs = vec![
                job(8801, "Older completed control", Some("older diagnostic")),
                job(8802, "Running control", None),
                job(8803, "Export final.wav", Some(&error)),
            ];
            let older = Id::new(("progress-job", 8801_u64));
            let running_row = Id::new(("progress-job", 8802_u64));
            let cancel = Id::new(("progress-cancel", 8802_u64));
            let failed = Id::new(("progress-job", 8803_u64));
            let inserted = Id::new(("progress-job", 8804_u64));
            let text = format!("Export final.wav: Failed: {error}");

            let mut d = Driver::demo();
            {
                let app = d.app();
                app.ui.show_header = false;
                app.ui.show_status_bar = false;
                app.ui.dock = DockNode::Tabs { panels: vec![PanelKind::Progress], active: 0 };
                app.ui.focused = PanelKind::Progress;
                app.ui.panels.progress.show_finished = true;
                app.session.prefs.general.show_tool_tips = false;
                app.session.jobs = jobs.clone();
            }
            d.harness.ctx.memory_mut(|memory| memory.options.screen_reader = screen_reader);
            d.harness.event(Event::PointerGone);
            d.frames(4);

            // Preconditions already supported by the old hover-only row.
            assert_eq!(d.label("progress.job.8803"), text);
            assert!(d.has("progress.cancel.8802"));
            assert!(!d.has("progress.cancel.8803"));
            let before_document = document_state(&mut d);
            let before_jobs = job_state(&jobs);

            for _ in 0..128 {
                tab(&mut d, Modifiers::NONE);
                if d.harness.ctx.memory(|memory| memory.focused()) == Some(failed) {
                    break;
                }
            }
            assert_eq!(
                d.harness.ctx.memory(|memory| memory.focused()),
                Some(failed),
                "Tab must reach the completed diagnostic row; screen_reader={screen_reader}",
            );
            assert_focused_detail(&d, failed, &text);

            // Inserting at the front of the reversed display must preserve identity.
            d.app().session.jobs.push(job(8804, "Inserted completed control", Some("inserted diagnostic")));
            d.frames(3);
            assert_focused_detail(&d, failed, &text);
            d.app().session.jobs.retain(|job| job.id != 8804);
            d.frames(3);
            assert_focused_detail(&d, failed, &text);

            // No tooltip child or running-row focus stop may intervene.
            tab(&mut d, Modifiers::NONE);
            assert_eq!(d.harness.ctx.memory(|memory| memory.focused()), Some(cancel));
            assert!(!full_detail(&d, &text));
            // With the tooltip gone, this name must belong to the row's semantics.
            assert!(d.harness.query_by_label(&text).is_some());

            tab(&mut d, Modifiers::NONE);
            assert_eq!(d.harness.ctx.memory(|memory| memory.focused()), Some(older));
            tab(&mut d, Modifiers::SHIFT);
            assert_eq!(d.harness.ctx.memory(|memory| memory.focused()), Some(cancel));
            tab(&mut d, Modifiers::SHIFT);
            assert_focused_detail(&d, failed, &text);
            assert_eq!(document_state(&mut d), before_document);
            assert_eq!(job_state(&jobs), before_jobs);

            // Do not click the checkbox: that would move focus before hiding the row.
            d.app().ui.panels.progress.show_finished = false;
            d.frames(3);
            let focus = d.harness.ctx.memory(|memory| memory.focused());
            assert!(![failed, older, inserted, running_row].into_iter().any(|id| focus == Some(id)));
            assert!(!d.has("progress.job.8803"));
            assert!(!d.has("progress.job.8801"));
            assert!(d.harness.query_by_label(&text).is_none());
            assert!(!full_detail(&d, &text));

            for _ in 0..128 {
                tab(&mut d, Modifiers::NONE);
                let focus = d.harness.ctx.memory(|memory| memory.focused());
                assert!(![failed, older, inserted, running_row].into_iter().any(|id| focus == Some(id)));
                if focus == Some(cancel) {
                    break;
                }
            }
            assert_eq!(d.harness.ctx.memory(|memory| memory.focused()), Some(cancel));
            assert_eq!(job_state(&d.app().session.jobs), before_jobs);

            // Removing the final running row must also release its existing control.
            d.app().session.jobs.clear();
            d.frames(3);
            let focus = d.harness.ctx.memory(|memory| memory.focused());
            assert!(![failed, older, inserted, running_row, cancel].into_iter().any(|id| focus == Some(id)));
            assert!(d.ids("progress.job.").is_empty());
            assert!(d.ids("progress.cancel.").is_empty());
            assert!(d.harness.query_by_label("No background jobs.").is_some());
            assert!(d.harness.query_by_label(&text).is_none());
            assert!(!full_detail(&d, &text));
            assert_eq!(document_state(&mut d), before_document);
            assert_eq!(job_state(&jobs), before_jobs);
        }
    }

    #[test]
    fn completed_progress_tooltip_respects_disabled_hover_after_keyboard_focus() {
        // The enabled case controls for real hover; the disabled case is the regression.
        for show_tool_tips in [true, false] {
            let error = "I/O: /exports/hover-control.wav: permission denied\nFull hover diagnostic tail: Ω";
            let jobs = vec![job(8811, "Running hover control", None), job(8812, "Export hover control", Some(error))];
            let failed = Id::new(("progress-job", 8812_u64));
            let cancel = Id::new(("progress-cancel", 8811_u64));
            let text = format!("Export hover control: Failed: {error}");

            let mut d = Driver::demo();
            {
                let app = d.app();
                app.ui.show_header = false;
                app.ui.show_status_bar = false;
                app.ui.dock = DockNode::Tabs { panels: vec![PanelKind::Progress], active: 0 };
                app.ui.focused = PanelKind::Progress;
                app.ui.panels.progress.show_finished = true;
                app.session.prefs.general.show_tool_tips = show_tool_tips;
                app.session.jobs = jobs.clone();
            }
            d.harness.ctx.memory_mut(|memory| memory.options.screen_reader = false);
            d.harness.event(Event::PointerGone);
            d.frames(4);
            assert_eq!(d.label("progress.job.8812"), text);
            assert!(d.has("progress.cancel.8811"));
            assert!(!d.has("progress.cancel.8812"));
            let before_document = document_state(&mut d);
            let before_jobs = job_state(&jobs);

            for _ in 0..128 {
                tab(&mut d, Modifiers::NONE);
                if d.harness.ctx.memory(|memory| memory.focused()) == Some(failed) {
                    break;
                }
            }
            assert_focused_detail(&d, failed, &text);

            // Keep a real pointer over the registered row as keyboard focus leaves.
            let row = d.harness.ctx.read_response(failed).unwrap();
            assert!(row.rect.width() > 0.0 && row.rect.height() > 0.0);
            let pointer = row.rect.center();
            d.harness.event(Event::PointerMoved(pointer));
            d.frames(3);
            let row = d.harness.ctx.read_response(failed).unwrap();
            assert!(row.has_focus());
            assert!(row.hovered());
            assert!(full_detail(&d, &text));

            // Do not use tab(): its PointerGone would hide the defect.
            for pressed in [true, false] {
                d.harness.event(Event::Key { key: Key::Tab, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE });
                d.frames(1);
            }
            d.frames(2);
            assert!(d.harness.state().ui_error.is_none());
            assert_eq!(d.harness.ctx.memory(|memory| memory.focused()), Some(cancel));
            let row = d.harness.ctx.read_response(failed).unwrap();
            assert!(!row.has_focus());
            assert!(row.hovered(), "the completed row must still be hovered after Tab");
            assert_eq!(d.harness.ctx.input(|input| input.pointer.hover_pos()), Some(pointer));
            assert_eq!(d.app().session.prefs.general.show_tool_tips, show_tool_tips);
            assert_eq!(document_state(&mut d), before_document);
            assert_eq!(job_state(&d.app().session.jobs), before_jobs);
            assert_eq!(job_state(&jobs), before_jobs);

            if show_tool_tips {
                assert!(full_detail(&d, &text), "enabled hover must preserve the diagnostic after keyboard focus leaves");
            } else {
                assert!(!full_detail(&d, &text), "disabled automatic detail must disappear after keyboard focus leaves the hovered completed row");
            }

            // Cleanup is reached only if the preference-specific assertion passed.
            d.harness.event(Event::PointerGone);
            d.frames(3);
            assert!(d.harness.state().ui_error.is_none());
            assert_eq!(d.harness.ctx.memory(|memory| memory.focused()), Some(cancel));
            assert!(!d.harness.ctx.read_response(failed).unwrap().hovered());
            assert!(!full_detail(&d, &text));
            assert!(d.harness.query_by_label(&text).is_some());
            assert_eq!(document_state(&mut d), before_document);
            assert_eq!(job_state(&jobs), before_jobs);
        }
    }

    #[test]
    fn completed_progress_tooltip_respects_disabled_hover_on_cancel_after_keyboard_focus() {
        for show_tool_tips in [true, false] {
            let error = "I/O: /exports/grace-control.wav: permission denied\nFocused diagnostic before Cancel hover: Ω";
            let jobs = vec![job(8821, "Running grace control", None), job(8822, "Export grace control", Some(error))];
            let failed = Id::new(("progress-job", 8822_u64));
            let cancel = Id::new(("progress-cancel", 8821_u64));
            let text = format!("Export grace control: Failed: {error}");

            // The ordinary Driver advances 0.25 s per frame, beyond egui's 0.2 s grace.
            let mut session = Session::default();
            session.execute("file.openDemoProject", json!({})).expect("demo project");
            let (tx, rx) = channel();
            let app = FilmcraftApp::new(session).with_control(rx);
            let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).with_step_dt(1.0 / 60.0).build_eframe(move |_cc| app);
            let mut d = Driver { harness, tx, snapshots: None };
            d.frames(4);
            {
                let app = d.app();
                app.ui.show_header = false;
                app.ui.show_status_bar = false;
                app.ui.dock = DockNode::Tabs { panels: vec![PanelKind::Progress], active: 0 };
                app.ui.focused = PanelKind::Progress;
                app.ui.panels.progress.show_finished = true;
                app.session.prefs.general.show_tool_tips = show_tool_tips;
                app.session.jobs = jobs.clone();
            }
            d.harness.ctx.memory_mut(|memory| memory.options.screen_reader = false);
            d.harness.event(Event::PointerGone);
            d.frames(4);
            assert_eq!(d.label("progress.job.8822"), text);
            assert!(d.has("progress.cancel.8821"));
            assert!(!d.has("progress.cancel.8822"));
            let before_document = document_state(&mut d);
            let before_jobs = job_state(&jobs);

            for _ in 0..128 {
                tab(&mut d, Modifiers::NONE);
                if d.harness.ctx.memory(|memory| memory.focused()) == Some(failed) {
                    break;
                }
            }
            assert_focused_detail(&d, failed, &text);

            let control = d.harness.ctx.read_response(cancel).unwrap();
            assert!(control.rect.width() > 0.0 && control.rect.height() > 0.0);
            let pointer = control.rect.center();
            d.harness.event(Event::PointerMoved(pointer));
            d.frames(1);
            assert!(d.harness.ctx.read_response(cancel).unwrap().hovered());
            assert!(d.harness.ctx.read_response(failed).unwrap().has_focus());
            assert!(full_detail(&d, &text));
            let last_focused_detail_time = d.harness.ctx.input(|input| input.time);

            // Keep the pointer over Cancel while real Tab changes keyboard focus.
            for pressed in [true, false] {
                d.harness.event(Event::Key { key: Key::Tab, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE });
                d.frames(1);
            }
            d.frames(2);
            let elapsed = d.harness.ctx.input(|input| input.time) - last_focused_detail_time;
            assert!(elapsed > 0.0 && elapsed < 0.2, "the observed transition must stay within the default tooltip grace: {elapsed}");
            assert!(d.harness.state().ui_error.is_none());
            assert_eq!(d.harness.ctx.memory(|memory| memory.focused()), Some(cancel));
            let control = d.harness.ctx.read_response(cancel).unwrap();
            assert!(control.has_focus());
            assert!(control.hovered());
            assert!(!d.harness.ctx.read_response(failed).unwrap().has_focus());
            assert_eq!(d.harness.ctx.input(|input| input.pointer.hover_pos()), Some(pointer));
            assert!(!full_detail(&d, &text), "the previous completed-row tooltip must be gone");
            assert_eq!(d.app().session.prefs.general.show_tool_tips, show_tool_tips);
            assert_eq!(document_state(&mut d), before_document);
            assert_eq!(job_state(&d.app().session.jobs), before_jobs);
            assert_eq!(job_state(&jobs), before_jobs);

            if show_tool_tips {
                assert!(full_detail(&d, "Cancel"), "enabled hover must show the Cancel tooltip after the focused diagnostic");
            } else {
                assert!(!full_detail(&d, "Cancel"), "disabled automatic Cancel detail must not inherit grace from the focused diagnostic");
            }

            // No baseline credit for cleanup if the disabled assertion above fails.
            d.harness.event(Event::PointerGone);
            d.frames(3);
            assert!(d.harness.state().ui_error.is_none());
            assert_eq!(d.harness.ctx.memory(|memory| memory.focused()), Some(cancel));
            assert!(!d.harness.ctx.read_response(cancel).unwrap().hovered());
            assert!(!full_detail(&d, "Cancel"));
            assert!(!full_detail(&d, &text));
            assert_eq!(document_state(&mut d), before_document);
            assert_eq!(job_state(&jobs), before_jobs);
        }
    }
}
