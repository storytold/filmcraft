//! Clicking empty space deselects, consistently (#683): the Timeline's empty space below and
//! between the tracks, a text layer being edited when another panel is clicked, and the Program
//! monitor's empty space around the picture (graphic layers, masks).

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use filmcraft_ui_egui::state::GfxEdit;
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
    fn rect(&mut self, id: &str) -> [f64; 4] {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == id).unwrap_or_else(|| panic!("no element {id}")).clone();
        let r: Vec<f64> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
        [r[0], r[1], r[2], r[3]]
    }
    fn click_at(&mut self, x: f64, y: f64) {
        self.ok("ui.click", json!({"x": x, "y": y}));
        self.frames(3);
    }
    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }
}

/// A click on the Timeline below the tracks deselects, as one on an empty stretch of a track does;
/// a drag from there draws a box selection.
#[test]
fn empty_timeline_space_below_the_tracks_deselects_and_starts_a_marquee() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"focused": "Timeline"}));
    let seq = d.exec("sequence.inspect", json!({}));
    let clip = seq["video"][0]["items"][0]["clip"].as_u64().unwrap();
    let area = d.rect("timeline.tracks");
    // a point in the track area that is on no track at all: going up from the area's bottom
    let x = area[0] + area[2] * 0.5;
    let y = (0..60)
        .map(|k| area[1] + area[3] - 3.0 - 4.0 * k as f64)
        .find(|y| d.ok("ui.timeline.hit", json!({"x": x, "y": y}))["hit"] == "none")
        .expect("the track area has space on no track");
    let below = (x, y);
    d.exec("timeline.select", json!({"clips": [clip]}));
    d.frames(2);
    d.click_at(below.0, below.1);
    assert!(d.app().session.state.selection.is_empty(), "a click below the tracks deselects");
    // a drag from below the tracks up over the clips selects them
    let top = d.rect("timeline.track.V1.locked");
    d.ok("ui.drag", json!({"from": {"x": area[0] + 4.0, "y": below.1}, "to": {"x": area[0] + area[2] * 0.6, "y": top[1] + top[3] / 2.0}, "steps": 8}));
    d.frames(3);
    assert!(!d.app().session.state.selection.is_empty(), "the box from below the tracks selected clips");
}

/// A text layer being edited stops being edited when another panel is clicked (its keys went into
/// the text before, #227); working in Properties, which styles the text, keeps the edit.
#[test]
fn clicking_another_panel_ends_text_editing() {
    let mut d = Driver::demo();
    let seq = d.exec("sequence.inspect", json!({}));
    let at = seq["playhead"].as_i64().unwrap();
    let clip = d.exec("graphics.newText", json!({"text": "Title", "time": at}))["clip"].as_u64().unwrap();
    d.ok("ui.panel.show", json!({"panel": "Properties"}));
    let edit = |d: &mut Driver| {
        d.app().ui.gfx_edit = Some(GfxEdit { clip, layer: 0, caret: 0, anchor: 0 });
        d.app().ui.focused = filmcraft_ui_egui::dock::PanelKind::Program;
        d.frames(3);
    };
    edit(&mut d);
    assert!(d.app().ui.gfx_edit.is_some());
    let props = d.rect("panel.Properties");
    d.click_at(props[0] + props[2] / 2.0, props[1] + props[3] - 10.0);
    assert!(d.app().ui.gfx_edit.is_some(), "a click in Properties keeps the edit");
    let tl = d.rect("timeline.tracks");
    d.click_at(tl[0] + tl[2] / 2.0, tl[1] + tl[3] - 6.0);
    assert!(d.app().ui.gfx_edit.is_none(), "a click in the Timeline ends it");
}

/// A click on the Program monitor around the picture lets go of the selected graphic layer and
/// mask; so does a click on the picture's empty space.
#[test]
fn empty_monitor_space_deselects_layers_and_masks() {
    let mut d = Driver::demo();
    let pic = d.rect("program.picture");
    let panel = d.rect("panel.Program");
    // the empty space beside or above the picture, a few points away from it
    let outside = if pic[0] - panel[0] > 20.0 { (pic[0] - 8.0, pic[1] + pic[3] / 2.0) } else { (pic[0] + pic[2] / 2.0, pic[1] + pic[3] + 8.0) };
    let seq = d.exec("sequence.inspect", json!({}));
    let clip = seq["video"][0]["items"][0]["clip"].as_u64().unwrap();
    // a graphic layer
    let at = seq["playhead"].as_i64().unwrap();
    let gclip = d.exec("graphics.newText", json!({"text": "Title", "time": at}))["clip"].as_u64().unwrap();
    d.exec("graphics.selectLayer", json!({"clip": gclip, "layers": [0]}));
    d.frames(2);
    assert!(!d.app().session.state.graphic_layers.is_empty());
    d.click_at(outside.0, outside.1);
    assert!(d.app().session.state.graphic_layers.is_empty(), "a click around the picture deselects the layer");
    // a mask: around the picture, and on the picture's empty space
    d.exec("timeline.select", json!({"clips": [clip]}));
    d.exec("effects.apply", json!({"clips": [clip], "effect": "gaussian_blur"}));
    let fx = {
        let s = &d.harness.state().session;
        let it = s.active_sequence().unwrap().find_item(filmcraft_project::ClipId(clip)).unwrap().1;
        it.effects.iter().position(|e| e.effect == "gaussian_blur").unwrap()
    };
    d.exec("masks.add", json!({"clip": clip, "effect": fx, "shape": "ellipse"}));
    for (x, y) in [outside, (pic[0] + 6.0, pic[1] + 6.0)] {
        d.exec("masks.select", json!({"clip": clip, "effect": fx, "mask": 0}));
        d.frames(2);
        d.click_at(x, y);
        assert!(d.app().session.state.selected_mask.is_none(), "a click at ({x}, {y}) deselects the mask");
    }
}
