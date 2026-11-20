//! Overwrite trimming in the Timeline: dragging a clip edge past a cut eats the neighbour by
//! default, Alt keeps the old clamped trim, and caption edges overwrite too.

use std::sync::mpsc::{Sender, channel};

use egui::{Event, Modifiers, Pos2, pos2};
use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_project::ClipId;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

const CITY: &str = "City_Night_Drive.mp4";
const DUNES: &str = "Desert_Dunes.mp4";

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        session.execute("sequence.snap", json!({"on": false})).expect("snapping off");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_step_dt(1.0 / 60.0).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        d.ok("ui.elements", json!({"prefix": "timeline.clip."}));
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

    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }

    fn app(&self) -> &FilmcraftApp {
        self.harness.state()
    }

    fn drag(&mut self, from: Pos2, to: Pos2, steps: usize, mods: Modifiers) {
        let push = |d: &mut Self, e: Event| d.harness.input_mut().events.push(e);
        push(self, Event::ModifiersChanged(mods));
        push(self, Event::PointerMoved(from));
        self.frames(1);
        push(self, Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: mods });
        self.frames(1);
        for i in 1..=steps {
            push(self, Event::PointerMoved(from + (to - from) * (i as f32 / steps as f32)));
            self.frames(1);
        }
        push(self, Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: mods });
        self.frames(2);
        push(self, Event::ModifiersChanged(Modifiers::default()));
        self.frames(1);
    }

    fn v1_spans(&self) -> Vec<(String, ClipId, i64, i64)> {
        let seq = self.app().session.active_sequence().expect("sequence");
        seq.video_tracks[0].items.iter().map(|i| (i.name.clone(), i.id, i.start.0, i.end().0)).collect()
    }

    fn v1_named(&self, name: &str) -> (ClipId, i64, i64) {
        self.v1_spans().into_iter().find(|s| s.0 == name).map(|s| (s.1, s.2, s.3)).unwrap_or_else(|| panic!("no {name} on V1"))
    }

    fn edges(&mut self, clip: ClipId) -> (f32, f32, f32) {
        let i = self.ok("ui.timeline.locate", json!({"clip": clip.0, "edge": "in"}));
        let o = self.ok("ui.timeline.locate", json!({"clip": clip.0, "edge": "out"}));
        let f = |v: &Value, k: &str| v[k].as_f64().expect("number") as f32;
        let id = format!("timeline.clip.{}", clip.0);
        let els = self.ok("ui.elements", json!({"prefix": id}));
        let el = els.as_array().expect("elements").iter().find(|e| e["id"] == id.as_str()).unwrap_or_else(|| panic!("no {id}: {els}")).clone();
        let top = el["rect"][1].as_f64().expect("rect") as f32;
        (f(&i, "x") - 2.0, f(&o, "x") + 2.0, top + 8.0)
    }

    fn undo_steps(&self) -> usize {
        self.app().session.history.undo.len()
    }
}

/// Dragging a clip's Out edge past the cut eats the start of the next clip: the neighbour starts
/// where the edge lands instead of the trim stopping at the cut.
#[test]
fn dragging_an_out_edge_past_a_cut_overwrites_the_next_clip() {
    let mut d = Driver::demo();
    let city = d.v1_named(CITY).0;
    let (_, dunes_start, dunes_end) = d.v1_named(DUNES);
    let (_, x1, y) = d.edges(city);
    let (_, city_end) = (0, d.v1_named(CITY).2);
    assert_eq!(city_end, dunes_start, "the demo's two clips share a cut");
    let by = 60.0;
    let (undo0, nodes0) = (d.undo_steps(), d.v1_spans().len());
    d.drag(pos2(x1 - 3.0, y), pos2(x1 - 3.0 + by, y), 30, Modifiers::default());
    let (_, _, city_end1) = d.v1_named(CITY);
    assert!(city_end1 > city_end, "the edge moved past the cut: {city_end} → {city_end1}");
    let (_, dunes_start1, dunes_end1) = d.v1_named(DUNES);
    assert_eq!(dunes_start1, city_end1, "the next clip was pushed to where the edge landed");
    assert_eq!(dunes_end1, dunes_end, "and its tail was kept");
    assert_eq!(d.v1_spans().len(), nodes0, "nothing was lost");
    assert_eq!(d.undo_steps(), undo0 + 1, "one undo step");
}

/// Holding Alt keeps the old trim: the edge stops at the cut and the neighbour is untouched.
#[test]
fn alt_keeps_the_clamped_trim() {
    let mut d = Driver::demo();
    let city = d.v1_named(CITY).0;
    let dunes_before = d.v1_named(DUNES);
    let (_, x1, y) = d.edges(city);
    let city_before = d.v1_named(CITY);
    d.drag(pos2(x1 - 3.0, y), pos2(x1 - 3.0 + 60.0, y), 30, Modifiers { alt: true, ..Default::default() });
    assert_eq!(d.v1_named(CITY), city_before, "Alt clamped the Out edge at the cut");
    assert_eq!(d.v1_named(DUNES), dunes_before, "the neighbour is untouched");
}

/// A caption edge dragged over the next caption eats it: the covered caption is removed instead
/// of the trim stopping at its start.
#[test]
fn dragging_a_caption_out_edge_overwrites_the_next_caption() {
    let mut d = Driver::demo();
    let frame = d.app().session.sequence_rate().frame_duration().0;
    let make = |d: &mut Driver, at: i64, secs: f64| -> u64 {
        d.exec("captions.add", json!({"frame": at, "durationSeconds": secs, "text": "x"}))["caption"].as_u64().expect("caption")
    };
    let c1 = ClipId(make(&mut d, 30, 1.0));
    let c2 = ClipId(make(&mut d, 30 + 24, 1.0));
    let ids = |d: &Driver| -> Vec<u64> { d.app().session.state.caption_selection.clone().into_iter().map(|c| c.0).collect() };
    let _ = ids(&d);
    // the caption rows are painted on the frame after the edit lands
    d.frames(2);
    let els = d.ok("ui.elements", json!({"prefix": "timeline.caption."}));
    let rect_of = |els: &Value, id: ClipId| -> (f32, f32, f32) {
        let key = format!("timeline.caption.{}", id.0);
        let e = els.as_array().expect("elements").iter().find(|e| e["id"] == key.as_str()).unwrap_or_else(|| panic!("no {key}")).clone();
        let r = e["rect"].as_array().expect("rect");
        (r[0].as_f64().expect("x") as f32, r[2].as_f64().expect("w") as f32, r[3].as_f64().expect("h") as f32)
    };
    let (x, w, _) = rect_of(&els, c1);
    let (x2, _, h) = rect_of(&els, c2);
    let y = d.ok("ui.elements", json!({"prefix": format!("timeline.caption.{}", c1.0)}));
    let yy = y.as_array().expect("elements")[0]["rect"][1].as_f64().expect("y") as f32 + h / 2.0;
    let undo0 = d.undo_steps();
    // grab the right edge of the first caption and drag it past the second one's end
    let to = x2 + w + 6.0;
    d.drag(pos2(x + w - 2.0, yy), pos2(to, yy), 30, Modifiers::default());
    let seq = d.app().session.active_sequence().expect("sequence");
    assert!(seq.find_caption(c2).is_none(), "the covered caption was eaten");
    let (_, c1_end) = seq.find_caption(c1).expect("caption one");
    assert!(c1_end.start.0 + c1_end.duration.0 > 0, "and the dragged one grew");
    assert_eq!(d.undo_steps(), undo0 + 1, "one undo step");
    let _ = frame;
}
