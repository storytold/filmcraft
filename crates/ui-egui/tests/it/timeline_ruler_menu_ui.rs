//! The Timeline ruler's own context menu (#1535): right-clicking the ruler, away from a marker,
//! opens the In/Out and marker menu at the playhead, not the clip menu of an earlier press.

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
    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
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
            if let Ok(r) = reply.try_recv() {
                assert_eq!(r["ok"], true, "{method} {params}: {r}");
                self.frames(2);
                return r["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }
    /// Centre of an element, from `ui.elements`.
    fn centre(&mut self, id: &str) -> (f64, f64) {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == id).unwrap_or_else(|| panic!("no element {id}")).clone();
        let r: Vec<f64> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
        (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0)
    }
}

fn has(d: &mut Driver, id: &str) -> bool {
    let v = d.ok("ui.elements", json!({"prefix": id}));
    v.as_array().unwrap().iter().any(|e| e["id"] == id)
}

#[test]
fn right_clicking_the_ruler_opens_the_ruler_menu_not_a_stale_clip_menu() {
    let mut d = Driver::demo();
    // an earlier right-click on a clip leaves the clip menu's state behind
    let clip = d.harness.state().session.active_sequence().unwrap().video_tracks[0].items[0].id.0;
    d.ok("ui.click", json!({"id": format!("timeline.clip.{clip}"), "button": "right"}));
    assert!(has(&mut d, "timeline.clipMenu.edit.cut"), "a right-click on a clip opens the clip menu");
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(2);
    // a right-click on the ruler, away from the markers, opens the ruler's menu
    let (x, y) = d.centre("timeline.ruler");
    d.ok("ui.click", json!({"x": x + 300.0, "y": y, "button": "right"}));
    assert!(has(&mut d, "timeline.rulerMenu.markers.markIn"), "Mark In is offered");
    assert!(has(&mut d, "timeline.rulerMenu.markers.markSelection"), "Mark Selection is offered");
    assert!(!has(&mut d, "timeline.clipMenu.edit.cut"), "and not the clip menu");
    // its entries act at the playhead
    d.ok("ui.click", json!({"id": "timeline.rulerMenu.markers.markIn"}));
    let playhead = d.harness.state().session.playhead();
    let q = d.harness.state().session.active_sequence().unwrap();
    assert_eq!(q.mark_in, Some(playhead), "Mark In set the In point at the playhead");
}

/// The work area bar sits in the ruler, and a right-click on it opens the same menu.
#[test]
fn right_clicking_the_work_area_bar_opens_the_ruler_menu() {
    let mut d = Driver::demo();
    d.ok("engine.execute", json!({"command": "sequence.setWorkArea", "params": {"start": 0, "end": 60 * 254_016_000_000_i64}}));
    d.frames(2);
    let (x, y) = d.centre("timeline.workArea");
    d.ok("ui.click", json!({"x": x, "y": y, "button": "right"}));
    assert!(has(&mut d, "timeline.rulerMenu.markers.markIn"), "the ruler's menu opens on the work area bar");
    assert!(!has(&mut d, "timeline.clipMenu.edit.cut"), "and not the clip menu");
}

/// Mark Selection in the ruler menu marks the span of the selected clips, whatever the playhead.
#[test]
fn mark_selection_in_the_ruler_menu_marks_the_selected_clips() {
    let mut d = Driver::demo();
    let (clip, start, end) = {
        let it = &d.harness.state().session.active_sequence().unwrap().video_tracks[0].items[0];
        (it.id.0, it.start, it.end())
    };
    d.ok("engine.execute", json!({"command": "timeline.select", "params": {"clips": [clip], "linked": false}}));
    d.ok("engine.execute", json!({"command": "playhead.set", "params": {"seconds": 7.0}}));
    d.frames(2);
    let (x, y) = d.centre("timeline.ruler");
    d.ok("ui.click", json!({"x": x + 300.0, "y": y, "button": "right"}));
    d.ok("ui.click", json!({"id": "timeline.rulerMenu.markers.markSelection"}));
    let frame = d.harness.state().session.sequence_rate().frame_duration();
    let q = d.harness.state().session.active_sequence().unwrap();
    assert_eq!(q.mark_in, Some(start), "the In point is the selection's start");
    assert_eq!(q.mark_out, Some(end - frame), "the Out point is its last frame");
}

/// Lock Markers in the ruler menu locks the sequence's markers, shows its checkmark in the
/// Markers menu while they are locked, and the markers draw darker.
#[test]
fn lock_markers_in_the_ruler_menu_locks_them_and_dims_them() {
    fn fills(shape: &egui::Shape, r: egui::Rect, out: &mut Vec<egui::Color32>) {
        match shape {
            egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| fills(s, r, out)),
            egui::Shape::Path(p) if p.visual_bounding_rect().intersects(r) => out.push(p.fill),
            _ => {}
        }
    }
    let mut d = Driver::demo();
    let id = d.ok("engine.execute", json!({"command": "markers.add", "params": {"seconds": 2.0}}))["marker"].as_u64().unwrap();
    d.frames(2);
    let rect = {
        let v = d.ok("ui.elements", json!({"prefix": format!("timeline.marker.{id}")}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == format!("timeline.marker.{id}")).unwrap().clone();
        let r: Vec<f32> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect();
        egui::Rect::from_min_size(egui::pos2(r[0], r[1]), egui::vec2(r[2], r[3]))
    };
    let painted = |d: &Driver| {
        let mut out = Vec::new();
        d.harness.output().shapes.iter().for_each(|c| fills(&c.shape, rect, &mut out));
        out
    };
    let [r, g, b] = {
        let app = d.harness.state();
        let m = app.session.active_sequence().unwrap().markers.iter().find(|m| m.id.0 == id).unwrap().clone();
        app.session.prefs.labels.rgb(m.color)
    };
    assert!(painted(&d).contains(&egui::Color32::from_rgb(r, g, b)), "unlocked, the marker's own colour");
    let (x, y) = d.centre("timeline.ruler");
    d.ok("ui.click", json!({"x": x + 300.0, "y": y, "button": "right"}));
    assert!(has(&mut d, "timeline.rulerMenu.markers.lock"), "Lock Markers is offered");
    d.ok("ui.click", json!({"id": "timeline.rulerMenu.markers.lock"}));
    assert!(d.harness.state().session.active_sequence().unwrap().markers_locked, "it locks the markers");
    let checked = filmcraft_ui_egui::menus::menu_items(d.harness.state()).into_iter().find(|i| i.id == "markers.lock").and_then(|i| i.checked);
    assert_eq!(checked, Some(true), "Markers ▸ Lock Markers shows its checkmark");
    d.frames(2);
    let dim = egui::Color32::from_rgb(r / 2, g / 2, b / 2);
    assert!(painted(&d).contains(&dim) && !painted(&d).contains(&egui::Color32::from_rgb(r, g, b)), "locked, the darker fill");
}
