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

/// #2329: in the Effects panel what a folder holds is indented under it: an effect's label starts
/// right of its sub-folder's, a sub-folder's right of its top folder's, and the same for presets
/// and Lumetri presets.
#[test]
fn effects_are_indented_under_their_folders() {
    fn text_x(d: &mut Driver, text: &str) -> f32 {
        fn find(shapes: &[egui::Shape], text: &str) -> Option<f32> {
            shapes.iter().find_map(|s| match s {
                egui::Shape::Text(t) if t.galley.text() == text => Some(t.pos.x),
                egui::Shape::Vec(v) => find(v, text),
                _ => None,
            })
        }
        let shapes: Vec<egui::Shape> = d.harness.output().shapes.iter().map(|c| c.shape.clone()).collect();
        find(&shapes, text).unwrap_or_else(|| panic!("`{text}` is not drawn"))
    }
    let mut d = Driver::demo();
    d.ok("ui.panel.show", json!({"panel": "Effects"}));
    d.frames(3);
    for folder in ["Video Effects", "Video Effects/Blur & Sharpen", "Presets", "Lumetri Presets", "Lumetri Presets/Monochrome"] {
        d.click(&format!("effects.folder.{folder}"));
    }
    d.frames(2);
    let preset = d.app().session.presets.all()[0].name.clone();
    let lumetri = filmcraft_render::lumetri_presets::presets().into_iter().find(|p| p.folder == "Monochrome").unwrap().name;
    for (outer, inner) in [
        ("Video Effects", "Blur & Sharpen"),
        ("Blur & Sharpen", "Gaussian Blur"),
        ("Presets", preset.as_str()),
        ("Lumetri Presets", "Monochrome"),
        ("Monochrome", lumetri),
    ] {
        let (a, b) = (text_x(&mut d, outer), text_x(&mut d, inner));
        assert!(b >= a + 12.0, "`{inner}` ({b}) is not indented under `{outer}` ({a})");
    }
}

#[test]
fn metadata_custom_name_edits_without_renaming_clip() {
    let mut d = Driver::demo();
    let item = d.app().session.project.items.values().filter(|i| i.as_media().is_some()).map(|i| i.id).min().unwrap();
    let name = d.app().session.project.item(item).unwrap().name.clone();
    d.exec("metadata.set", json!({"item": item.0, "fields": {"metadata:Name": "", "metadata:Label": "custom label"}}));
    d.app().session.state.project_selection = vec![item];
    d.ok("ui.menu.invoke", json!({"id": "window.panel.Metadata"}));
    d.ok("ui.set", json!({"focused": "Project"}));
    d.frames(3);
    // Scroll the metadata panel to the stored fields, below the file properties.
    d.ok("ui.scroll", json!({"id": "metadata.field.Scene", "dy": -1500.0}));
    d.frames(3);
    d.click("metadata.field.metadata:Name");
    d.ok("ui.type", json!({"text": "original"}));
    d.frames(2);
    d.ok("ui.key", json!({"key": "Enter"}));
    d.frames(3);
    assert_eq!(d.app().session.project.item(item).unwrap().name, name);
    assert_eq!(d.app().session.project.item(item).unwrap().metadata["Name"], "original");
    d.snapshot("panels-metadata-custom", Some("metadata."));
    // Engine-visible identities remain separate from their display labels.
    let fields = filmcraft_engine::panels::metadata_fields(&d.app().session.project, item);
    assert!(fields.iter().any(|f| f.id == "Name" && f.value == name));
    assert!(fields.iter().any(|f| f.id == "metadata:Name" && f.value == "original"));
}
