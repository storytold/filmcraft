//! An agent's click either presses what it names or says what is in the way: a modal dialog
//! (recovery, Settings) or a dialog window over the panel. Before, the click was swallowed and the
//! control channel still answered `ok`.

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

    fn elements(&mut self) -> Vec<Value> {
        self.ok("ui.elements", json!({"prefix": ""})).as_array().cloned().unwrap()
    }

    fn clips(&mut self) -> usize {
        self.harness.state().session.active_sequence().unwrap().video_tracks[0].items.len()
    }
}

fn centre(e: &Value) -> (f64, f64) {
    let r = e["rect"].as_array().unwrap();
    let f = |i: usize| r[i].as_f64().unwrap();
    (f(0) + f(2) / 2.0, f(1) + f(3) / 2.0)
}

#[test]
fn a_modal_dialog_turns_clicks_and_keys_behind_it_into_errors() {
    let mut d = Driver::new();
    let first = d.harness.state().session.active_sequence().unwrap().video_tracks[0].items[0].id.0;
    d.ok("engine.execute", json!({"command": "timeline.select", "params": {"clips": [first]}}));
    let before = d.clips();
    d.ok("ui.menu.invoke", json!({"id": "app.settings.general"}));
    d.frames(3);

    let modal = d.ok("ui.inspect", json!({}))["modal"].clone();
    let ids: Vec<&str> = modal["elements"].as_array().unwrap().iter().map(|e| e["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&"settings.ok") && ids.contains(&"settings.cancel"), "ui.inspect names the open dialog: {modal}");

    let v = d.call("ui.click", json!({"id": "tools.Razor"}));
    assert_eq!(v["ok"], json!(false), "a click behind the dialog fails: {v}");
    let e = v["error"].as_str().unwrap();
    assert!(e.contains("a dialog is open") && e.contains("settings.ok"), "and says how to answer it: {e}");
    assert_ne!(format!("{:?}", d.harness.state().ui.tool), "Razor");

    // a shortcut doesn't edit the timeline behind the dialog either
    d.ok("ui.key", json!({"key": "Delete"}));
    d.frames(3);
    assert_eq!(d.clips(), before, "Delete reached the timeline behind the Settings dialog");

    d.ok("ui.click", json!({"id": "settings.cancel"}));
    d.frames(3);
    assert!(d.ok("ui.inspect", json!({}))["modal"].is_null());
    d.ok("ui.click", json!({"id": "tools.Razor"}));
    d.frames(2);
    assert_eq!(format!("{:?}", d.harness.state().ui.tool), "Razor", "with the dialog answered the click lands");
}

#[test]
fn a_dialog_window_over_a_panel_button_is_reported() {
    let mut d = Driver::new();
    d.ok("ui.menu.invoke", json!({"id": "sequence.addTracks"}));
    d.frames(3);
    let els = d.elements();
    let dialog: Vec<&Value> = els.iter().filter(|e| e["id"].as_str().unwrap().starts_with("addTracks.")).collect();
    assert!(!dialog.is_empty(), "the Add Tracks dialog is up");
    let (x0, y0, x1, y1) =
        dialog.iter().map(|e| centre(e)).fold((f64::MAX, f64::MAX, f64::MIN, f64::MIN), |(a, b, c, d), (x, y)| (a.min(x), b.min(y), c.max(x), d.max(y)));
    let under = els
        .iter()
        .filter(|e| !e["id"].as_str().unwrap().starts_with("addTracks."))
        .find(|e| {
            let (x, y) = centre(e);
            (x0..=x1).contains(&x) && (y0..=y1).contains(&y)
        })
        .expect("some panel element lies under the dialog");
    let id = under["id"].as_str().unwrap().to_string();
    let v = d.call("ui.click", json!({"id": id}));
    assert_eq!(v["ok"], json!(false), "`{id}` under the dialog: {v}");
    let e = v["error"].as_str().unwrap();
    assert!(e.contains("covered by a dialog") && e.contains("addTracks.cancel"), "{e}");
    // the dialog's own buttons still work
    d.ok("ui.click", json!({"id": "addTracks.cancel"}));
    d.frames(3);
    assert!(!d.elements().iter().any(|e| e["id"].as_str().unwrap().starts_with("addTracks.")), "Cancel closed it");
}

#[test]
fn an_option_clicked_the_moment_its_dropdown_opens_is_picked() {
    // A popup's first frame is an invisible sizing pass that takes no clicks: clicking an option
    // straight after opening its dropdown used to fall through and close the list unchanged.
    let mut d = Driver::new();
    d.ok("ui.menu.invoke", json!({"id": "sequence.addTracks"}));
    let tracks = d.harness.state().session.active_sequence().unwrap().video_tracks.len();
    for n in [0, tracks, 1, 0, tracks] {
        d.ok("ui.click", json!({"id": "addTracks.video.placement"}));
        d.ok("ui.click", json!({"id": format!("addTracks.video.placement.option.{n}")}));
        assert_eq!(d.harness.state().ui.add_tracks.video_after, n, "picked option {n} straight after opening the list");
    }
}

#[test]
fn clicking_and_dragging_in_the_trim_monitor_doesnt_freeze_the_app() {
    // The drag handler read the pointer while holding egui's (non-reentrant) data lock: the first
    // click or drag on the Trim Monitor froze the app for good. Found by the UI crawl.
    let mut d = Driver::new();
    let cuts = |d: &mut Driver| -> Vec<(i64, i64)> {
        d.harness.state().session.active_sequence().unwrap().video_tracks[0].items.iter().map(|c| (c.start.0, c.end().0)).collect()
    };
    let before = cuts(&mut d);
    d.ok("engine.execute", json!({"command": "playhead.set", "params": {"time": before[0].1}}));
    d.ok("ui.menu.invoke", json!({"id": "trim.edit"}));
    d.frames(3);
    for id in ["trimMonitor.incoming", "trimMonitor.outgoing", "trimMonitor.roll"] {
        d.ok("ui.click", json!({"id": id}));
    }
    let roll = d.elements().into_iter().find(|e| e["id"] == "trimMonitor.roll").expect("the Trim Monitor is up");
    let (x, y) = centre(&roll);
    d.ok("ui.drag", json!({"from": {"id": "trimMonitor.roll"}, "to": {"x": x - 60.0, "y": y}}));
    d.frames(3);
    assert_ne!(cuts(&mut d), before, "a 60 px roll drag moved the edit");
}
