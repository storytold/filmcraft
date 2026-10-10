//! Headless UI tests of the handles of graphic layers in the Program monitor: point text scales
//! about its anchor point, a paragraph-text box is resized, a shape's Size follows the handle
//! while its opposite side and anchor point stay (docs/graphics.md).
//!
//! With `FILMCRAFT_UI_SHOTS=<dir>` the tests also render the UI with wgpu and save PNGs there.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    shots: Option<std::path::PathBuf>,
}

impl Driver {
    fn new() -> Self {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(s).with_control(rx);
        let shots = std::env::var_os("FILMCRAFT_UI_SHOTS").map(std::path::PathBuf::from);
        let mut b = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_step_dt(1.0 / 60.0).with_max_steps(10_000);
        if shots.is_some() {
            b = b.wgpu().with_pixels_per_point(1.0);
        }
        let harness = b.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, shots };
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

    fn shot(&mut self, name: &str) {
        let Some(dir) = self.shots.clone() else { return };
        self.frames(8);
        let img = self.harness.render().expect("wgpu render");
        std::fs::create_dir_all(&dir).unwrap();
        img.save(dir.join(format!("{name}.png"))).unwrap();
    }
}

impl Driver {
    fn rect(&mut self, id: &str) -> [f64; 4] {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == json!(id)).unwrap_or_else(|| panic!("no element {id}")).clone();
        let r = &e["rect"];
        [r[0].as_f64().unwrap(), r[1].as_f64().unwrap(), r[2].as_f64().unwrap(), r[3].as_f64().unwrap()]
    }
}

impl Driver {
    fn select(&mut self, clip: u64) -> String {
        self.exec("graphics.selectLayer", json!({"clip": clip, "layers": [0]}));
        self.ok("ui.set", json!({"tool": "Selection"}));
        self.frames(4);
        format!("program.layer.{clip}.0.")
    }

    /// A selected layer of each kind; returns the clip and the id prefix of the layer's handles
    /// (`handle.0`–`3` corners TL, TR, BR, BL; `handle.4`–`7` the top, right, bottom and left
    /// edges) and anchor point (`anchor`).
    fn point_text(&mut self) -> (u64, String) {
        let r = self.exec("graphics.newText", json!({"text": "Title", "position": [600, 500], "size": 120}));
        let clip = r["clip"].as_u64().unwrap();
        (clip, self.select(clip))
    }

    fn paragraph_text(&mut self) -> (u64, String) {
        let r = self.exec("graphics.newText", json!({"text": FOX, "position": [500, 300], "box": [700, 500], "size": 80}));
        let clip = r["clip"].as_u64().unwrap();
        (clip, self.select(clip))
    }

    fn shape(&mut self) -> String {
        let r = self.exec("graphics.newShape", json!({"shape": "rectangle", "position": [800, 500], "size": [500, 300]}));
        let clip = r["clip"].as_u64().unwrap();
        self.select(clip)
    }

    /// Centre of element `pre` + `what` ("handle.2", "anchor").
    fn at(&mut self, pre: &str, what: &str) -> (f64, f64) {
        let r = self.rect(&format!("{pre}{what}"));
        (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0)
    }

    fn handle(&mut self, pre: &str, n: usize) -> (f64, f64) {
        self.at(pre, &format!("handle.{n}"))
    }

    fn drag(&mut self, from: (f64, f64), by: (f64, f64)) {
        self.ok("ui.drag", json!({"from": {"x": from.0, "y": from.1}, "to": {"x": from.0 + by.0, "y": from.1 + by.1}, "steps": 6}));
        self.frames(4);
    }

    /// Press or let go of Shift, as a keyboard reports it (the control channel's drags carry
    /// modifiers only on their press and release).
    fn hold_shift(&mut self, on: bool) {
        let m = if on { egui::Modifiers::SHIFT } else { egui::Modifiers::NONE };
        self.harness.input_mut().events.push(egui::Event::ModifiersChanged(m));
        self.frames(1);
    }

    fn drag_handle(&mut self, pre: &str, n: usize, dx: f64, dy: f64) {
        let h = self.handle(pre, n);
        self.drag(h, (dx, dy));
    }

    fn layer(&mut self, clip: u64) -> Value {
        self.exec("graphics.list", json!({"clip": clip}))["layers"][0].clone()
    }
}

const FOX: &str = "The quick brown fox jumps over the lazy dog";

fn near(a: (f64, f64), b: (f64, f64)) -> bool {
    (a.0 - b.0).abs() < 1.5 && (a.1 - b.1).abs() < 1.5
}

fn num(v: &Value) -> f64 {
    v.as_f64().unwrap_or_else(|| panic!("not a number: {v}"))
}

#[test]
fn point_text_handles_scale_it_about_its_anchor() {
    let mut d = Driver::new();
    let (clip, pre) = d.point_text();
    let before = d.layer(clip);
    assert_eq!(before["textType"], "point");
    let (anchor, tl, br) = (d.at(&pre, "anchor"), d.handle(&pre, 0), d.handle(&pre, 2));
    assert!(
        (anchor.0 - tl.0).abs() < 1.5 && anchor.1 > tl.1 && anchor.1 < br.1,
        "the anchor is on the first baseline's left end: {anchor:?} in {tl:?}..{br:?}"
    );

    // a corner dragged down: the text grows, both axes alike, and the anchor does not move
    d.drag_handle(&pre, 2, 0.0, 40.0);
    let after = d.layer(clip);
    let f = num(&after["scale"]) / 100.0;
    assert!(f > 1.2, "bigger: {f}");
    assert_eq!(after["scale"], after["scaleWidth"], "uniform");
    assert_eq!(after["position"], before["position"]);
    assert_eq!(after["anchor"], before["anchor"]);
    assert!(near(d.at(&pre, "anchor"), anchor), "the anchor stays put");
    d.shot("point-text-scaled");
    let (tl2, br2) = (d.handle(&pre, 0), d.handle(&pre, 2));
    for (was, now) in [(tl, tl2), (br, br2)] {
        let want = (anchor.0 + (was.0 - anchor.0) * f, anchor.1 + (was.1 - anchor.1) * f);
        assert!(near(now, want), "every corner moves away from the anchor by the scale: {was:?} -> {now:?}, expected {want:?}");
    }
    // the handle is nearer than 60 points to the anchor, so it scales as if 60 points further away
    let reach = (br.1 - anchor.1) + 60.0;
    assert!((f - (1.0 + 40.0 / reach)).abs() < 0.02, "scale {f} for 40 points with the handle {} from the anchor", br.1 - anchor.1);

    // sideways travel of a corner does nothing
    d.drag_handle(&pre, 2, 80.0, 0.0);
    assert!((num(&d.layer(clip)["scale"]) / 100.0 - f).abs() < 0.01);

    // an edge handle scales the whole text too: the right edge follows the pointer, since it is
    // further than 60 points from the anchor
    let (right, top) = (d.handle(&pre, 5), d.handle(&pre, 4));
    d.drag_handle(&pre, 5, 50.0, 0.0);
    let l = d.layer(clip);
    assert_eq!(l["scale"], l["scaleWidth"], "still uniform");
    let (right2, top2) = (d.handle(&pre, 5), d.handle(&pre, 4));
    assert!((right2.0 - right.0 - 50.0).abs() < 2.0, "right edge follows the pointer: {right:?} -> {right2:?}");
    assert!(top2.1 < top.1 - 2.0, "and the text got taller with it: {top:?} -> {top2:?}");
    assert!(near(d.at(&pre, "anchor"), anchor));

    // towards the anchor shrinks; one undo step per drag
    let s = num(&l["scale"]);
    d.drag_handle(&pre, 4, 0.0, 30.0);
    assert!(num(&d.layer(clip)["scale"]) < s - 5.0);
    d.exec("edit.undo", json!({}));
    assert_eq!(num(&d.layer(clip)["scale"]), s);
}

#[test]
fn the_anchor_point_can_be_dragged_without_moving_the_layer() {
    let mut d = Driver::new();
    let (clip, pre) = d.point_text();
    d.exec("graphics.set", json!({"clip": clip, "props": {"scale": 200, "scale_width": 200}}));
    d.frames(4);
    let (anchor, tl, br) = (d.at(&pre, "anchor"), d.handle(&pre, 0), d.handle(&pre, 2));
    let before = d.layer(clip);
    d.drag(anchor, (90.0, -40.0));
    let after = d.layer(clip);
    assert!(near(d.at(&pre, "anchor"), (anchor.0 + 90.0, anchor.1 - 40.0)), "the anchor follows the pointer");
    assert!(near(d.handle(&pre, 0), tl) && near(d.handle(&pre, 2), br), "the layer stays where it is");
    // the position moves with the anchor; the anchor moves half as far in the layer's own pixels (scale 200)
    let (dp, da) = (num(&after["position"][0]) - num(&before["position"][0]), num(&after["anchor"][0]) - num(&before["anchor"][0]));
    assert!(dp > 10.0 && (da - dp / 2.0).abs() < 0.01, "position {dp}, anchor {da}");
    assert_eq!(after["scale"], before["scale"]);

    // now a handle scales about the moved anchor
    d.drag_handle(&pre, 2, 0.0, 30.0);
    assert!(near(d.at(&pre, "anchor"), (anchor.0 + 90.0, anchor.1 - 40.0)));
    assert!(num(&d.layer(clip)["scale"]) > 201.0);
}

#[test]
fn paragraph_text_handles_resize_the_box_and_the_text_reflows() {
    let mut d = Driver::new();
    let (clip, pre) = d.paragraph_text();
    let before = d.layer(clip);
    assert_eq!(before["textType"], "paragraph");
    assert_eq!(before["overflow"], false, "{before}");
    let (anchor, tl, br) = (d.at(&pre, "anchor"), d.handle(&pre, 0), d.handle(&pre, 2));
    assert!(near(anchor, tl), "the anchor is on the box's top-left corner: {anchor:?} vs {tl:?}");
    let k = (br.0 - tl.0) / 700.0; // screen points per canvas pixel

    // bottom-right corner: it follows the pointer on both axes, the top-left stays, nothing scales
    let (dx, dy) = (-70.0 * k, 50.0 * k);
    d.drag_handle(&pre, 2, dx, dy);
    let after = d.layer(clip);
    assert!(near(d.handle(&pre, 0), tl));
    assert!(near(d.handle(&pre, 2), (br.0 + dx, br.1 + dy)), "the corner follows the pointer");
    assert!((num(&after["box"][0]) - 630.0).abs() < 1.0 && (num(&after["box"][1]) - 550.0).abs() < 1.0, "{}", after["box"]);
    assert_eq!(after["overflow"], false);
    assert_eq!(after["scale"], 100.0);
    assert_eq!(after["position"], before["position"]);
    assert_eq!(after["anchor"], before["anchor"]);

    // a box too low for its text hides the rest (and says so); the font size is untouched
    d.drag_handle(&pre, 6, 0.0, -400.0 * k);
    let low = d.layer(clip);
    assert_eq!(low["overflow"], true, "{low}");
    d.shot("paragraph-text-overflow");
    assert_eq!(low["scale"], 100.0);
    d.exec("edit.undo", json!({}));
    assert_eq!(d.layer(clip)["overflow"], false);

    // the left edge: it follows the pointer, the right edge stays; the anchor stays on its spot
    // of the picture (now outside the box) and the position is untouched
    let (right, left) = (d.handle(&pre, 5), d.handle(&pre, 7));
    d.drag_handle(&pre, 7, 60.0 * k, 0.0);
    let moved = d.layer(clip);
    assert!(near(d.handle(&pre, 5), right));
    assert!(near(d.handle(&pre, 7), (left.0 + 60.0 * k, left.1)));
    assert!(near(d.at(&pre, "anchor"), anchor), "the anchor stays put");
    assert_eq!(moved["position"], before["position"]);
    assert!((num(&moved["anchor"][0]) + 60.0).abs() < 1.0 && num(&moved["anchor"][1]).abs() < 1e-6, "{}", moved["anchor"]);
    assert!((num(&moved["box"][0]) - 570.0).abs() < 1.0, "{}", moved["box"]);
    d.shot("paragraph-text-left-edge-moved");

    // a side dragged past its opposite stops at a small box instead of flipping
    d.drag_handle(&pre, 5, -900.0, 0.0);
    let w = num(&d.layer(clip)["box"][0]);
    assert!((w - 40.0).abs() < 0.5, "half the font size: {w}");
}

#[test]
fn type_tool_drag_makes_paragraph_text_and_a_click_point_text() {
    let mut d = Driver::new();
    d.ok("ui.set", json!({"tool": "Type"}));
    d.frames(3);
    let pic = d.rect("program.picture");
    let at = |fx: f64, fy: f64| (pic[0] + pic[2] * fx, pic[1] + pic[3] * fy);
    let (a, b) = (at(0.2, 0.5), at(0.6, 0.8));
    d.drag(a, (b.0 - a.0, b.1 - a.1));
    d.ok("ui.type", json!({"text": FOX}));
    let list = d.exec("graphics.list", json!({}));
    let clip = list["clip"].as_u64().unwrap();
    let l = &list["layers"][0];
    assert_eq!(l["text"], FOX);
    assert_eq!(l["textType"], "paragraph");
    let canvas = (num(&list["canvas"][0]), num(&list["canvas"][1]));
    assert!(
        (num(&l["box"][0]) - canvas.0 * 0.4).abs() < 2.0 && (num(&l["box"][1]) - canvas.1 * 0.3).abs() < 2.0,
        "the box is the dragged rectangle: {}",
        l["box"]
    );
    assert!((num(&l["quad"][0][0]) - canvas.0 * 0.2).abs() < 2.0 && (num(&l["quad"][0][1]) - canvas.1 * 0.5).abs() < 2.0, "{}", l["quad"]);
    assert_eq!(l["scale"], 100.0);
    // the text being typed is the box
    d.shot("paragraph-text-typing");
    let edit = d.rect("program.textEdit");
    assert!((edit[2] - (b.0 - a.0)).abs() < 3.0, "{edit:?}");

    // a click makes point text, in the same graphic
    d.ok("ui.key", json!({"key": "Escape"}));
    let c = at(0.3, 0.2);
    d.ok("ui.click", json!({"x": c.0, "y": c.1}));
    d.ok("ui.type", json!({"text": "Title"}));
    let list = d.exec("graphics.list", json!({"clip": clip}));
    assert_eq!(list["layers"][1]["textType"], "point");
    assert_eq!(list["layers"][1]["box"], json!(null));
}

#[test]
fn text_properties_switches_between_point_and_paragraph_text() {
    let mut d = Driver::new();
    d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
    let (clip, pre) = d.paragraph_text();
    let tl = d.handle(&pre, 0);
    d.ok("ui.click", json!({"id": "graphics.textProperties"}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "graphics.textProperties.type"}));
    d.frames(3);
    d.shot("text-properties-type-list");
    d.ok("ui.click", json!({"id": "graphics.textProperties.type.point"}));
    d.frames(3);
    assert_eq!(d.layer(clip)["textType"], "paragraph", "nothing happens before OK");
    d.shot("text-properties-dialog");
    d.ok("ui.click", json!({"id": "graphics.textProperties.ok"}));
    d.frames(4);
    let l = d.layer(clip);
    assert_eq!(l["textType"], "point");
    assert!(l["text"].as_str().unwrap().contains('\n'), "the wrapped lines are real lines now: {}", l["text"]);
    assert!(near(d.at(&pre, "anchor"), tl), "the anchor stays where the box's corner was");
    assert!(d.ok("ui.elements", json!({"prefix": "graphics.textProperties.ok"})).as_array().unwrap().is_empty(), "the dialog closed");
    // its handles scale it now
    d.drag_handle(&pre, 2, 0.0, 40.0);
    assert!(num(&d.layer(clip)["scale"]) > 101.0);

    // Cancel changes nothing
    d.ok("ui.click", json!({"id": "graphics.textProperties"}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "graphics.textProperties.type"}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "graphics.textProperties.type.paragraph"}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "graphics.textProperties.cancel"}));
    d.frames(3);
    assert_eq!(d.layer(clip)["textType"], "point");
}

#[test]
fn shape_corner_handles_scale_away_from_the_opposite_corner() {
    let mut d = Driver::new();
    let pre = d.shape();
    // bottom-right: grows down and to the right, the top-left stays
    let (tl, br) = (d.handle(&pre, 0), d.handle(&pre, 2));
    d.drag_handle(&pre, 2, 60.0, 30.0);
    let (tl2, br2) = (d.handle(&pre, 0), d.handle(&pre, 2));
    assert!(near(tl, tl2), "top-left stays put: {tl:?} -> {tl2:?}");
    assert!(br2.0 > br.0 + 20.0 && br2.1 > br.1 + 5.0, "bottom-right follows the pointer: {br:?} -> {br2:?}");
    // bottom-left: grows down and to the left, the top-right stays
    let (tr, bl) = (d.handle(&pre, 1), d.handle(&pre, 3));
    d.drag_handle(&pre, 3, -60.0, 30.0);
    let (tr2, bl2) = (d.handle(&pre, 1), d.handle(&pre, 3));
    assert!(near(tr, tr2), "top-right stays put: {tr:?} -> {tr2:?}");
    assert!(bl2.0 < bl.0 - 20.0 && bl2.1 > bl.1 + 5.0, "bottom-left follows the pointer: {bl:?} -> {bl2:?}");
}

/// Width and height of a layer's box in its own pixels.
fn box_size(l: &Value) -> (f64, f64) {
    let b = &l["localBounds"];
    (num(&b[2]) - num(&b[0]), num(&b[3]) - num(&b[1]))
}

#[test]
fn shape_handles_change_its_size_and_shift_keeps_the_ratio() {
    let mut d = Driver::new();
    let pre = d.shape();
    let clip: u64 = pre.split('.').nth(2).unwrap().parse().unwrap();
    // a corner straight right: only wider, as in Premiere; the Size changes, not the Scale
    let (tl, br, anchor) = (d.handle(&pre, 0), d.handle(&pre, 2), d.at(&pre, "anchor"));
    let before = d.layer(clip);
    d.drag_handle(&pre, 2, 60.0, 0.0);
    let l = d.layer(clip);
    assert_eq!((num(&l["scale"]), num(&l["scaleWidth"])), (100.0, 100.0), "{l}");
    let (w, h) = box_size(&l);
    assert!(w > 510.0 && (h - 300.0).abs() < 1e-3, "wider, as tall: {w} x {h}");
    let (tl2, br2) = (d.handle(&pre, 0), d.handle(&pre, 2));
    assert!(near(tl, tl2), "top-left stays put: {tl:?} -> {tl2:?}");
    assert!(near(br2, (br.0 + 60.0, br.1)), "bottom-right follows the pointer: {br:?} -> {br2:?}");
    assert!(near(d.at(&pre, "anchor"), anchor), "the anchor point stays put");
    assert_eq!(l["position"], before["position"]);

    // with Shift both sides grow alike from the opposite corner
    d.hold_shift(true);
    d.drag_handle(&pre, 2, 40.0, 0.0);
    d.hold_shift(false);
    let l2 = d.layer(clip);
    let (w2, h2) = box_size(&l2);
    assert!(h2 > 310.0, "taller too: {w2} x {h2}");
    assert!((w2 / h2 - w / h).abs() < 1e-3, "same proportions: {w}x{h} -> {w2}x{h2}");
    assert!(near(d.handle(&pre, 0), tl), "top-left still stays put");
    assert_eq!(num(&l2["scale"]), 100.0);
    // one undo step per drag
    d.exec("edit.undo", json!({}));
    assert_eq!(box_size(&d.layer(clip)), (w, h));
}

#[test]
fn polygon_handles_keep_the_opposite_corner() {
    let mut d = Driver::new();
    let r = d.exec("graphics.newPolygon", json!({"sides": 5, "position": [800, 500], "size": [300, 300]}));
    let pre = d.select(r["clip"].as_u64().unwrap());
    // a pentagon's box is not centred on the shape's origin; the top-left corner stays anyway
    let (tl, br) = (d.handle(&pre, 0), d.handle(&pre, 2));
    d.drag_handle(&pre, 2, 50.0, 40.0);
    assert!(near(d.handle(&pre, 0), tl), "top-left stays put");
    assert!(near(d.handle(&pre, 2), (br.0 + 50.0, br.1 + 40.0)), "bottom-right follows the pointer");
}

#[test]
fn shape_edge_handles_stretch_one_axis() {
    let mut d = Driver::new();
    let pre = d.shape();
    // bottom edge: taller downwards, the top edge and the width stay
    let (tl, tr, bottom) = (d.handle(&pre, 0), d.handle(&pre, 1), d.handle(&pre, 6));
    d.drag_handle(&pre, 6, 0.0, 40.0);
    let (tl2, tr2, bottom2) = (d.handle(&pre, 0), d.handle(&pre, 1), d.handle(&pre, 6));
    assert!(near(tl, tl2) && near(tr, tr2), "top edge stays put: {tl:?} {tr:?} -> {tl2:?} {tr2:?}");
    assert!((bottom2.1 - bottom.1 - 40.0).abs() < 2.0 && (bottom2.0 - bottom.0).abs() < 1.5, "bottom follows the pointer: {bottom:?} -> {bottom2:?}");
    // right edge: wider to the right, the left edge and the height stay
    let (tl, bl, right) = (d.handle(&pre, 0), d.handle(&pre, 3), d.handle(&pre, 5));
    d.drag_handle(&pre, 5, 50.0, 0.0);
    let (tl2, bl2, right2) = (d.handle(&pre, 0), d.handle(&pre, 3), d.handle(&pre, 5));
    assert!(near(tl, tl2) && near(bl, bl2), "left edge stays put: {tl:?} {bl:?} -> {tl2:?} {bl2:?}");
    assert!((right2.0 - right.0 - 50.0).abs() < 2.0 && (right2.1 - right.1).abs() < 1.5, "right follows the pointer: {right:?} -> {right2:?}");
    // top edge: taller upwards, the bottom edge stays
    let (bl, top) = (d.handle(&pre, 3), d.handle(&pre, 4));
    d.drag_handle(&pre, 4, 0.0, -30.0);
    let (bl2, top2) = (d.handle(&pre, 3), d.handle(&pre, 4));
    assert!(near(bl, bl2), "bottom edge stays put: {bl:?} -> {bl2:?}");
    assert!((top2.1 - top.1 + 30.0).abs() < 2.0, "top follows the pointer: {top:?} -> {top2:?}");
}

#[test]
fn a_hidden_layer_has_no_box_and_cannot_be_clicked() {
    let mut d = Driver::new();
    let pre = d.shape();
    let clip: u64 = pre.split('.').nth(2).unwrap().parse().unwrap();
    let layer = format!("program.layer.{clip}.0");
    let ids = |d: &mut Driver| -> Vec<String> {
        let v = d.ok("ui.elements", json!({"prefix": layer}));
        v.as_array().unwrap().iter().map(|e| e["id"].as_str().unwrap().to_string()).collect()
    };
    assert_eq!(ids(&mut d).len(), 10, "the layer, its eight handles and its anchor: {:?}", ids(&mut d));
    let r = d.rect(&layer);
    let inside = (r[0] + r[2] / 4.0, r[1] + r[3] / 2.0);

    // visibility off: still selected in the panel, but nothing of it is on the monitor
    d.exec("graphics.set", json!({"clip": clip, "props": {"enabled": false}}));
    d.frames(4);
    assert_eq!(ids(&mut d), Vec::<String>::new());
    d.exec("graphics.selectLayer", json!({"clip": clip, "layers": []}));
    d.ok("ui.click", json!({"x": inside.0, "y": inside.1}));
    d.frames(4);

    // visibility on: the click where it was selected nothing, so it has no handles; now it can be clicked
    d.exec("graphics.set", json!({"clip": clip, "layer": 0, "props": {"enabled": true}}));
    d.frames(4);
    assert_eq!(ids(&mut d), vec![layer.clone()], "not selected: no handles");
    // (elsewhere on it, so that the two clicks are not a double click)
    d.ok("ui.click", json!({"x": r[0] + r[2] * 0.75, "y": inside.1}));
    d.frames(4);
    assert_eq!(ids(&mut d).len(), 10);
}
