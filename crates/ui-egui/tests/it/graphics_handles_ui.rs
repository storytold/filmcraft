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
    // Real Context output tokens, retained across later idle/control-channel frames.
    reset_requests: Vec<egui::ImeResetId>,
}

impl Driver {
    fn new() -> Self {
        Self::new_with(|_| {})
    }

    fn new_with(configure: impl FnOnce(&mut FilmcraftApp)) -> Self {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let (tx, rx) = channel();
        let mut app = FilmcraftApp::new(s).with_control(rx);
        configure(&mut app);
        let shots = std::env::var_os("FILMCRAFT_UI_SHOTS").map(std::path::PathBuf::from);
        let mut b = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_step_dt(1.0 / 60.0).with_max_steps(10_000);
        if shots.is_some() {
            b = b.wgpu().with_pixels_per_point(1.0);
        }
        let harness = b.build_eframe(move |cc| {
            filmcraft_ui_egui::install_ime_reset_callbacks(&cc.egui_ctx);
            app
        });
        let mut d = Driver { harness, tx, shots, reset_requests: Vec::new() };
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
            if let Some(request) = self.harness.output().platform_output.ime.as_ref().and_then(|ime| ime.reset_request) {
                self.reset_requests.push(request);
            }
        }
    }

    fn reset_completed(&self) -> egui::Event {
        egui::Event::ImeResetCompleted { request_id: *self.reset_requests.last().expect("a real Context reset request was published") }
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

    fn ime(&self) -> Option<egui::output::IMEOutput> {
        self.harness.output().platform_output.ime.clone()
    }

    /// Start point-text editing on the Program picture and return its clip id.
    fn start_point_text(&mut self) -> u64 {
        self.ok("ui.set", json!({"tool": "Type"}));
        self.frames(3);
        let pic = self.rect("program.picture");
        self.ok("ui.click", json!({"x": pic[0] + pic[2] * 0.3, "y": pic[1] + pic[3] * 0.2}));
        self.frames(3);
        self.exec("graphics.list", json!({}))["clip"].as_u64().unwrap()
    }
}

/// The text editor's caret and anchor.
fn caret(d: &Driver) -> (usize, usize) {
    d.harness.state().ui.gfx_edit.as_ref().map(|e| (e.caret, e.anchor)).expect("a text layer is being edited")
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
        // Control gestures use explicit modifiers and isolate ambient native keyboard state.
        let m = self.harness.ctx.input(|input| input.modifiers);
        self.ok(
            "ui.drag",
            json!({"from": {"x": from.0, "y": from.1}, "to": {"x": from.0 + by.0, "y": from.1 + by.1}, "steps": 6,
            "modifiers": {"shift":m.shift,"alt":m.alt,"ctrl":m.ctrl,"command":m.command}}),
        );
        self.frames(4);
    }

    fn hover(&mut self, at: (f64, f64)) {
        self.harness.input_mut().events.push(egui::Event::PointerMoved(egui::pos2(at.0 as f32, at.1 as f32)));
        self.frames(2);
    }

    fn cursor(&self) -> egui::CursorIcon {
        self.harness.output().platform_output.cursor_icon
    }

    /// Press or let go of Shift as a keyboard reports it. The drag fixture then explicitly
    /// forwards the held modifiers to the control gesture.
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

/// Chinese, Japanese and Korean input methods hand over the finished text as `Ime(Commit)`, not
/// `Text`; it is typed into the text being edited on the monitor (#1083).
#[test]
fn text_committed_by_an_input_method_is_typed_on_the_monitor() {
    let mut d = Driver::new();
    d.ok("ui.set", json!({"tool": "Type"}));
    d.frames(3);
    let pic = d.rect("program.picture");
    d.ok("ui.click", json!({"x": pic[0] + pic[2] * 0.3, "y": pic[1] + pic[3] * 0.2}));
    d.ok("ui.type", json!({"text": "A"}));
    // the events Microsoft Pinyin sends for "ni hao" picked as 你好
    let preedit = |text: &str| egui::Event::Ime(egui::ImeEvent::Preedit { text: text.into(), active_range_chars: None });
    d.harness.input_mut().events.extend([preedit("ni hao"), preedit(""), egui::Event::Ime(egui::ImeEvent::Commit("你好".into()))]);
    d.frames(4);
    let list = d.exec("graphics.list", json!({}));
    assert_eq!(list["layers"][0]["text"], "A你好");
}

/// The labels of the drawn marked text and of its active clause, if drawn.
fn drawn_preedit(d: &mut Driver) -> (Option<String>, Option<String>) {
    let elements = d.ok("ui.elements", json!({"prefix": "program.textEdit.preedit"}));
    let label = |id: &str| elements.as_array()?.iter().find(|e| e["id"] == json!(id))?["label"].as_str().map(str::to_string);
    (label("program.textEdit.preedit"), label("program.textEdit.preedit.clause"))
}

/// The marked text is drawn on the video with the clause being converted until the commit, which
/// types the converted text instead.
#[test]
fn ime_preedit_and_its_active_clause_are_drawn_until_the_commit() {
    let mut d = Driver::new();
    let clip = d.start_point_text();
    d.ok("ui.type", json!({"text": "A"}));
    d.frames(2);
    d.harness.input_mut().events.push(egui::Event::Ime(egui::ImeEvent::Preedit { text: "にほんご".into(), active_range_chars: Some(2..4) }));
    d.frames(3);
    assert_eq!(drawn_preedit(&mut d), (Some("にほんご".into()), Some("んご".into())));
    assert_eq!(layer_texts(&mut d, clip), vec!["A".to_string()], "the marked text is not in the layer's text");
    assert_eq!(drawn_preedit(&mut d).0.as_deref(), Some("にほんご"), "it stays drawn while composing");
    d.harness.input_mut().events.extend([ime_preedit(""), ime_commit("日本語")]);
    d.frames(2);
    assert_eq!(drawn_preedit(&mut d), (None, None));
    assert_eq!(layer_texts(&mut d, clip), vec!["A日本語".to_string()]);
}

/// #2445: Korean commits a syllable only when the next one starts. The syllable being composed
/// shows at the caret, and the one still being composed when the edit ends goes into the text,
/// once.
#[test]
fn korean_being_composed_shows_and_is_kept_when_the_edit_ends() {
    let mut d = Driver::new();
    let clip = d.start_point_text();
    d.ok("ui.type", json!({"text": "A"}));
    d.frames(2);
    // 한 composed from ㅎ, 하, 한, then 국 started: 한 is committed, 구 shows
    d.harness.input_mut().events.extend([ime_preedit("ㅎ"), ime_preedit("하"), ime_preedit("한")]);
    d.frames(3);
    assert_eq!(drawn_preedit(&mut d).0.as_deref(), Some("한"), "the syllable being composed shows");
    d.harness.input_mut().events.extend([ime_commit("한"), ime_preedit("구")]);
    d.frames(3);
    assert_eq!(layer_texts(&mut d, clip), vec!["A한".to_string()]);
    assert_eq!(drawn_preedit(&mut d).0.as_deref(), Some("구"));
    // moving to the Timeline ends the edit: the syllable still being composed is kept
    d.ok("ui.panel.show", json!({"panel": "Timeline"}));
    d.frames(3);
    assert!(d.harness.state().ui.gfx_edit.is_none());
    assert_eq!(layer_texts(&mut d, clip), vec!["A한구".to_string()]);
    // the input method's own late commit of it is not typed a second time
    d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 0, "caret": 7, "anchor": 7}}));
    d.frames(2);
    d.harness.input_mut().events.push(ime_commit("구"));
    d.frames(2);
    assert_eq!(layer_texts(&mut d, clip), vec!["A한구".to_string()]);
}

/// A commit, the next syllable and the release of a click that ends the edit, in one pass, keep
/// both syllables: the commit's text is written before the edit's end types the marked text.
#[test]
fn a_commit_and_a_preedit_with_the_click_that_ends_the_edit_keep_both() {
    let mut d = Driver::new();
    let clip = d.start_point_text();
    d.ok("ui.type", json!({"text": "A"}));
    d.frames(2);
    let pic = d.rect("program.picture");
    let at = egui::pos2((pic[0] + pic[2] * 0.7) as f32, (pic[1] + pic[3] * 0.8) as f32);
    let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    d.harness.input_mut().events.extend([egui::Event::PointerMoved(at), button(true)]);
    d.frames(1);
    d.harness.input_mut().events.extend([ime_commit("한"), ime_preedit("구"), button(false)]);
    d.frames(3);
    assert_eq!(layer_texts(&mut d, clip).first().map(String::as_str), Some("A한구"));
}

/// The Program monitor's IME context keeps its owner and generation through a reversed UTF-8
/// selection, a preedit, a commit that replaces the selection, an empty commit and typing.
#[test]
fn ime_context_survives_selection_preedit_commit_and_typing() {
    let mut d = Driver::new();
    let clip = d.start_point_text();
    d.ok("ui.type", json!({"text": "café😀"}));
    d.frames(3);
    let first = d.ime().expect("the focused text layer publishes an IME context");
    let (owner, generation) = (first.owner, first.context_generation);

    // Primary caret 5 (after é), anchor 2: a reversed selection of "fé".
    d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 0, "caret": 5, "anchor": 2}}));
    d.frames(3);
    let selected = d.ime().expect("selection keeps the context");
    assert_eq!((selected.owner, selected.context_generation), (owner, generation), "caret and selection moves keep the context");
    assert_eq!(caret(&d), (5, 2), "direction is preserved");

    // The marked text is not committed text.
    d.harness.input_mut().events.push(egui::Event::Ime(egui::ImeEvent::Preedit { text: "に".into(), active_range_chars: Some(0..1) }));
    d.frames(2);
    assert!(d.ime().is_some(), "preedit does not remove the context");
    assert_eq!(layer_texts(&mut d, clip), vec!["café😀".to_string()]);
    assert_eq!(caret(&d), (5, 2));

    // The commit replaces the selected "fé".
    d.harness.input_mut().events.push(egui::Event::Ime(egui::ImeEvent::Commit("語".into())));
    d.frames(1);
    let committed = d.ime().expect("the commit keeps the context");
    assert_eq!((committed.owner, committed.context_generation), (owner, generation));
    assert_eq!(layer_texts(&mut d, clip), vec!["ca語😀".to_string()]);
    assert_eq!(caret(&d), (5, 5));

    // An empty commit with no selection changes nothing.
    d.harness.input_mut().events.push(egui::Event::Ime(egui::ImeEvent::Commit(String::new())));
    d.frames(2);
    let repeated = d.ime().expect("the committed editor still publishes");
    assert_eq!((repeated.owner, repeated.context_generation), (owner, generation));
    assert_eq!(layer_texts(&mut d, clip), vec!["ca語😀".to_string()]);

    d.ok("ui.type", json!({"text": "X"}));
    d.frames(2);
    let typed = d.ime().expect("typing keeps the context");
    assert_eq!(typed.context_generation, generation);
    assert_eq!(layer_texts(&mut d, clip), vec!["ca語X😀".to_string()], "typing inserts at the retained caret before the emoji");
    assert_eq!(caret(&d), (6, 6));
}

/// Delete-surrounding excludes the selection, rounds partial UTF-8 ranges inward, and keeps a
/// reversed selection's direction while the queued edit is accepted.
#[test]
fn ime_delete_surrounding_keeps_selection_direction_and_unicode() {
    let mut d = Driver::new();
    let clip = d.start_point_text();
    d.ok("ui.type", json!({"text": "aé😀z"}));
    d.frames(3);
    // Select é (bytes 1..3), primary caret at its end (reversed).
    d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 0, "caret": 3, "anchor": 1}}));
    d.frames(3);
    d.harness.input_mut().events.push(egui::Event::Ime(egui::ImeEvent::DeleteSurroundingBytes { before_bytes: 1, after_bytes: 4 }));
    d.frames(1);
    assert!(d.ime().is_some(), "delete-surrounding keeps the context");
    assert_eq!(layer_texts(&mut d, clip), vec!["éz".to_string()], "one byte before and the four-byte emoji after are deleted; é is kept");
    assert_eq!(caret(&d), (2, 0), "primary caret stays after the anchor");
}

/// Losing focus ends the output; reopening the same layer keeps the stable owner but resets the
/// generation exactly once. A plain caret move does not reset it.
#[test]
fn ime_owner_is_stable_across_edits_and_resets_on_refocus() {
    let mut d = Driver::new();
    d.start_point_text();
    d.ok("ui.type", json!({"text": "Title"}));
    d.frames(3);
    let first = d.ime().expect("text editing has an IME context");
    let (owner, generation) = (first.owner, first.context_generation);

    d.ok("ui.key", json!({"key": "ArrowLeft"}));
    d.frames(2);
    let moved = d.ime().expect("caret movement keeps the context");
    assert_eq!(moved.owner, owner);
    assert_eq!(moved.context_generation, generation, "ordinary caret movement does not reset");

    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(2);
    assert!(d.ime().is_none(), "an unfocused, closed editor publishes no descriptor");

    let pic = d.rect("program.picture");
    d.ok("ui.click", json!({"x": pic[0] + pic[2] * 0.3, "y": pic[1] + pic[3] * 0.2}));
    d.frames(3);
    let reopened = d.ime().expect("the reopened editor publishes again");
    assert_eq!(reopened.owner, owner, "the sequence/clip/component owner is stable");
    assert_ne!(reopened.context_generation, generation, "a replacement session resets the generation once");
}

/// Typing and a commit in one pass apply in the order they came, in the same IME context.
#[test]
fn ime_typing_and_commit_in_one_pass_apply_in_order() {
    let mut d = Driver::new();
    let clip = d.start_point_text();
    d.frames(2);
    let before = d.ime().expect("the empty editor is focused");
    d.harness.input_mut().events.extend([egui::Event::Text("A".into()), egui::Event::Ime(egui::ImeEvent::Commit("B".into()))]);
    d.frames(1);
    let mixed = d.ime().expect("the mixed edit keeps the context");
    assert_eq!(mixed.context_generation, before.context_generation);
    assert_eq!(layer_texts(&mut d, clip), vec!["AB".to_string()]);
}

/// An Arrow key later in a raw batch uses the layout of the text from the start of that batch.
/// Text typed earlier in the same batch can move the caret onto a byte inside a multi-byte
/// character; the following Text/Paste/Cut must clamp it to a boundary instead of panicking.
#[test]
fn arrow_from_the_old_layout_does_not_split_text_inserted_in_the_same_batch() {
    let mut d = Driver::new();
    let clip = d.start_point_text();
    d.exec("graphics.setText", json!({"clip": clip, "layer": 0, "text": "ab\ncd"}));
    d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 0, "caret": 0, "anchor": 0}}));
    d.frames(2);

    // After "éé" the caret is at byte 4, on the old layout's second line. ArrowUp maps its x to
    // byte 1 of that old line, which is inside the first "é" in the current text; "x" then inserts
    // there. Before the fix this panicked on the non-boundary insert.
    d.harness.input_mut().events.extend([egui::Event::Text("éé".into()), key_press(egui::Key::ArrowUp), egui::Event::Text("x".into())]);
    d.frames(2);
    assert_eq!(d.exec("graphics.list", json!({"clip": clip}))["layers"][0]["text"], json!("xééab\ncd"));
}

fn ime_preedit(text: &str) -> egui::Event {
    egui::Event::Ime(egui::ImeEvent::Preedit { text: text.into(), active_range_chars: None })
}

fn ime_commit(text: &str) -> egui::Event {
    egui::Event::Ime(egui::ImeEvent::Commit(text.into()))
}

fn key_press(key: egui::Key) -> egui::Event {
    egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }
}

fn layer_texts(d: &mut Driver, clip: u64) -> Vec<String> {
    d.exec("graphics.list", json!({"clip": clip}))["layers"]
        .as_array()
        .map(|layers| layers.iter().map(|layer| layer["text"].as_str().unwrap_or("").to_string()).collect())
        .unwrap_or_default()
}

fn new_text_layer(d: &mut Driver, clip: Option<u64>, text: &str, x: f64) -> u64 {
    let mut p = json!({"text": text, "position": [x, 400], "size": 100});
    if let Some(clip) = clip {
        p["clip"] = json!(clip);
    }
    d.exec("graphics.newText", p)["clip"].as_u64().unwrap()
}

/// A commit from a composition whose target was replaced (`ui.set.gfxEdit`) must be dropped: the
/// text belongs to the old layer, not to whatever layer the new edit target names.
#[test]
fn ime_commit_after_target_replacement_is_dropped() {
    let mut d = Driver::new();
    let clip = new_text_layer(&mut d, None, "alpha", 500.0);
    new_text_layer(&mut d, Some(clip), "beta", 900.0);
    d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 0, "caret": 5, "anchor": 5}}));
    d.frames(3);
    d.harness.input_mut().events.push(ime_preedit("に"));
    d.frames(1);
    // The edit target moves to the other layer while the IME is still composing.
    d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 1, "caret": 4, "anchor": 4}}));
    d.frames(2);
    d.harness.input_mut().events.push(ime_commit("x"));
    d.frames(2);
    assert_eq!(layer_texts(&mut d, clip), vec!["alpha".to_string(), "beta".to_string()]);
}

/// A composition whose target was replaced can emit another non-empty pre-edit before its final
/// commit. That late pre-edit must not revive the old composition on the replacement layer, while
/// a genuinely new composition in a later raw batch must still work.
#[test]
fn ime_late_preedit_after_target_replacement_cannot_retarget() {
    let mut d = Driver::new();
    let clip = new_text_layer(&mut d, None, "alpha", 500.0);
    new_text_layer(&mut d, Some(clip), "beta", 900.0);
    d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 0, "caret": 5, "anchor": 5}}));
    d.frames(3);
    d.harness.input_mut().events.push(ime_preedit("に"));
    d.frames(1);
    // The edit target moves to the other layer while the old composition is still live.
    d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 1, "caret": 4, "anchor": 4}}));
    d.frames(2);
    // A late pre-edit from the replaced composition. The input method commits it in a later raw
    // batch, so the tombstone must survive the pre-edit batch.
    d.harness.input_mut().events.push(ime_preedit("にに"));
    d.frames(1);
    d.harness.input_mut().events.push(ime_commit("x"));
    d.frames(2);
    assert_eq!(layer_texts(&mut d, clip), vec!["alpha".to_string(), "beta".to_string()], "the late pre-edit and paired commit stayed on the old target");

    // Only the matching reset completion releases the old composition guard.
    let fence = d.reset_completed();
    d.harness.input_mut().events.extend([fence, ime_preedit("よ"), ime_commit("y")]);
    d.frames(2);
    assert_eq!(layer_texts(&mut d, clip), vec!["alpha".to_string(), "betay".to_string()], "a new composition on the current target still works");
}

/// An empty pre-edit clears the visible composition but not ownership of its pending terminal
/// commit. Switching targets before that commit arrives must still reject it.
#[test]
fn ime_empty_preedit_keeps_pending_commit_from_retargeting() {
    let mut d = Driver::new();
    let clip = new_text_layer(&mut d, None, "alpha", 500.0);
    new_text_layer(&mut d, Some(clip), "beta", 900.0);
    d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 0, "caret": 5, "anchor": 5}}));
    d.frames(3);
    d.harness.input_mut().events.push(ime_preedit("に"));
    d.frames(1);
    // The empty pre-edit dismisses the visible composition. Its commit is a separate event.
    d.harness.input_mut().events.push(ime_preedit(""));
    d.frames(1);
    d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 1, "caret": 4, "anchor": 4}}));
    d.frames(2);
    d.harness.input_mut().events.push(ime_commit("x"));
    d.frames(2);
    assert_eq!(
        layer_texts(&mut d, clip),
        vec!["alpha".to_string(), "beta".to_string()],
        "the pending commit of the dismissed composition stayed on the old target"
    );

    // An old terminal is not acknowledgement; the exact fence admits a fresh direct commit.
    let fence = d.reset_completed();
    d.harness.input_mut().events.extend([fence, ime_commit("y")]);
    d.frames(2);
    assert_eq!(layer_texts(&mut d, clip), vec!["alpha".to_string(), "betay".to_string()], "a direct commit after the terminal boundary is accepted");
}

/// A matching opaque fence is an ordered boundary; the fresh preedit after it survives to its
/// later commit, while the old terminal before it remains rejected.
#[test]
fn ime_preedit_after_reset_completion_in_one_batch_is_accepted() {
    let mut d = Driver::new();
    let clip = new_text_layer(&mut d, None, "alpha", 500.0);
    new_text_layer(&mut d, Some(clip), "beta", 900.0);
    d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 0, "caret": 5, "anchor": 5}}));
    d.frames(3);
    d.harness.input_mut().events.push(ime_preedit("に"));
    d.frames(1);
    d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 1, "caret": 4, "anchor": 4}}));
    d.frames(2);
    // Preserve original raw order: old terminal, actual completion, then fresh preedit.
    let fence = d.reset_completed();
    d.harness.input_mut().events.extend([ime_preedit(""), ime_commit("x"), fence, ime_preedit("よ")]);
    d.frames(1);
    assert_eq!(layer_texts(&mut d, clip), vec!["alpha".to_string(), "beta".to_string()], "the old commit was still rejected");
    d.harness.input_mut().events.push(ime_commit("y"));
    d.frames(2);
    assert_eq!(
        layer_texts(&mut d, clip),
        vec!["alpha".to_string(), "betay".to_string()],
        "the pre-edit after the old terminal commit started a new composition"
    );
}

/// A same-pass control text mutation replaces the text the composition belonged to. The old
/// descriptor must not be published (existing behaviour) and the composition's next commit must
/// not be inserted into the replacement text.
#[test]
fn ime_same_pass_control_text_mutation_invalidates_composition() {
    let mut d = Driver::new();
    let clip = d.start_point_text();
    d.ok("ui.type", json!({"text": "one"}));
    d.frames(3);
    d.harness.input_mut().events.push(ime_preedit("に"));
    d.frames(1);
    // `ui.menu.invoke` is drained after the overlay drew and changes the committed text.
    d.ok("ui.menu.invoke", json!({"id": "graphics.setText", "params": {"clip": clip, "layer": 0, "text": "changed"}}));
    assert!(d.ime().is_none(), "the old cursor rectangle must not be published for the changed text");

    d.harness.input_mut().events.push(ime_commit("x"));
    d.frames(2);
    assert_eq!(layer_texts(&mut d, clip), vec!["changed".to_string()], "the old composition's commit must not edit the replacement text");
    // The control replacement keeps the editor's existing numeric byte offsets (the editor only
    // clamps them into the new text, as it did before this migration), so place the caret at the
    // end explicitly: this regression tests that the rejected composition's tombstone retires,
    // not the unrelated caret policy.
    d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 0, "caret": 7, "anchor": 7}}));
    d.frames(1);

    let fence = d.reset_completed();
    d.harness.input_mut().events.extend([fence, ime_preedit("よ"), ime_commit("z")]);
    d.frames(2);
    assert_eq!(layer_texts(&mut d, clip), vec!["changedz".to_string()], "a new composition still starts after the text mutation");
}

/// A same-pass control request can change layout (font size, style, alignment, spacing, box,
/// transform) without changing the committed text or the layer identity. The pass must not
/// publish the cursor rectangle of the previous layout.
#[test]
fn ime_control_layout_change_does_not_publish_stale_cursor_rect() {
    let mut d = Driver::new();
    let clip = d.start_point_text();
    d.ok("ui.type", json!({"text": "one"}));
    d.frames(3);
    let before = d.ime().expect("the editor is observed before the layout change");
    d.ok("ui.menu.invoke", json!({"id": "graphics.set", "params": {"clip": clip, "layer": 0, "props": {"tracking": 200.0}}}));
    assert!(d.ime().is_none(), "the old cursor rectangle must not be published with the new layout");
    d.frames(1);
    let after = d.ime().expect("the next pass observes the new layout");
    assert_ne!(after.cursor_rect, before.cursor_rect, "the candidate rectangle moved with the wider tracking");
}

/// An explicit `ui.set.gfxEdit` names the layer at that numeric index now. A uid belonging to a
/// layer that a reorder moved elsewhere must not silently retarget the request.
#[test]
fn explicit_gfx_edit_index_selects_the_current_layer_not_a_stale_uid() {
    let mut d = Driver::new();
    let clip = new_text_layer(&mut d, None, "one", 400.0);
    new_text_layer(&mut d, Some(clip), "two", 900.0);
    let uids: Vec<u64> = d.exec("graphics.list", json!({"clip": clip}))["layers"].as_array().unwrap().iter().map(|l| l["uid"].as_u64().unwrap()).collect();
    d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 0, "caret": 3, "anchor": 3}}));
    d.frames(2);
    assert_eq!(d.harness.state().ui.gfx_edit.as_ref().map(|e| (e.layer, e.uid)), Some((0, uids[0])));

    d.exec("graphics.arrangeLayer", json!({"clip": clip, "layer": 0, "to": "front"}));
    d.frames(2);
    assert_eq!(d.harness.state().ui.gfx_edit.as_ref().map(|e| (e.layer, e.uid)), Some((1, uids[0])), "the edit follows the reordered layer");

    // Index 0 now holds "two", but the request still carries the old "one" uid. The explicit
    // index wins: the current layer at index 0 is selected.
    d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 0, "caret": 3, "anchor": 3, "uid": uids[0]}}));
    d.frames(2);
    assert_eq!(d.harness.state().ui.gfx_edit.as_ref().map(|e| (e.layer, e.uid)), Some((0, uids[1])), "the stale uid must not retarget the request");
    d.ok("ui.type", json!({"text": "X"}));
    d.frames(2);
    assert_eq!(layer_texts(&mut d, clip), vec!["twoX".to_string(), "one".to_string()]);
}

/// The fork sends `DeleteSurroundingBytes` immediately before the `Commit` it belongs to. A stale
/// delete must not lift the tombstone, or the paired commit would be inserted into the replacement
/// layer. The request-bound guard survives the input batch; its exact completion admits a later
/// direct commit (no pre-edit) on the new target.
#[test]
fn stale_delete_and_commit_pair_cannot_retarget_the_replacement_layer() {
    let mut d = Driver::new();
    let clip = new_text_layer(&mut d, None, "alpha", 500.0);
    new_text_layer(&mut d, Some(clip), "beta", 900.0);
    d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 0, "caret": 5, "anchor": 5}}));
    d.frames(3);
    d.harness.input_mut().events.push(ime_preedit("に"));
    d.frames(1);
    d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 1, "caret": 4, "anchor": 4}}));
    d.frames(2);
    d.harness.input_mut().events.extend([egui::Event::Ime(egui::ImeEvent::DeleteSurroundingBytes { before_bytes: 0, after_bytes: 0 }), ime_commit("x")]);
    d.frames(2);
    assert_eq!(
        layer_texts(&mut d, clip),
        vec!["alpha".to_string(), "beta".to_string()],
        "neither the delete nor its paired commit touched the replacement layer"
    );

    // The old pair does not release admission; the exact native fence does.
    let fence = d.reset_completed();
    d.harness.input_mut().events.extend([fence, ime_commit("y")]);
    d.frames(1);
    assert_eq!(layer_texts(&mut d, clip), vec!["alpha".to_string(), "betay".to_string()]);
}

/// An empty pre-edit is the input method dismissing its composition. When it arrives after the
/// target was replaced it must not clear the tombstone before the paired commit, but it also must
/// not poison the next legitimate commit-only composition.
#[test]
fn stale_empty_preedit_and_commit_pair_cannot_retarget_the_replacement_layer() {
    let mut d = Driver::new();
    let clip = new_text_layer(&mut d, None, "alpha", 500.0);
    new_text_layer(&mut d, Some(clip), "beta", 900.0);
    d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 0, "caret": 5, "anchor": 5}}));
    d.frames(3);
    d.harness.input_mut().events.push(ime_preedit("に"));
    d.frames(1);
    d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 1, "caret": 4, "anchor": 4}}));
    d.frames(2);
    d.harness.input_mut().events.extend([ime_preedit(""), ime_commit("x")]);
    d.frames(2);
    assert_eq!(layer_texts(&mut d, clip), vec!["alpha".to_string(), "beta".to_string()]);

    let fence = d.reset_completed();
    d.harness.input_mut().events.extend([fence, ime_commit("y")]);
    d.frames(2);
    assert_eq!(layer_texts(&mut d, clip), vec!["alpha".to_string(), "betay".to_string()], "later commit-only input still works");
}

/// A composition belongs to its layer's uid, not to the numeric index it was started at: a real
/// reorder moves the editor (and the commit) with the same layer.
#[test]
fn composition_follows_a_reordered_layer_by_uid() {
    let mut d = Driver::new();
    let clip = new_text_layer(&mut d, None, "one", 400.0);
    new_text_layer(&mut d, Some(clip), "two", 900.0);
    d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 0, "caret": 3, "anchor": 3}}));
    d.frames(3);
    d.harness.input_mut().events.push(ime_preedit("に"));
    d.frames(1);
    d.exec("graphics.arrangeLayer", json!({"clip": clip, "layer": 0, "to": "front"}));
    d.frames(2);
    assert_eq!(d.harness.state().ui.gfx_edit.as_ref().map(|e| e.layer), Some(1));
    d.harness.input_mut().events.push(ime_commit("x"));
    d.frames(2);
    assert_eq!(layer_texts(&mut d, clip), vec!["two".to_string(), "onex".to_string()], "the composition stayed on the layer it began on");
}

/// `DeleteSurroundingBytes { 0, 0 }` followed by an empty commit is the IME's way of replacing a
/// selection with nothing. The selection must be deleted.
#[test]
fn empty_ime_commit_replaces_the_selection() {
    let mut d = Driver::new();
    let clip = d.start_point_text();
    d.ok("ui.type", json!({"text": "abc"}));
    d.frames(2);
    d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 0, "caret": 0, "anchor": 3}}));
    d.frames(2);
    d.harness.input_mut().events.extend([egui::Event::Ime(egui::ImeEvent::DeleteSurroundingBytes { before_bytes: 0, after_bytes: 0 }), ime_commit("")]);
    d.frames(1);
    assert_eq!(layer_texts(&mut d, clip), vec![String::new()], "the selected text is replaced by the empty commit");
    assert_eq!(caret(&d), (0, 0));
}

/// Escape in the same raw batch as a commit cancels the edit; the later commit must not be typed.
#[test]
fn escape_before_a_commit_in_the_same_batch_cancels_it() {
    let mut d = Driver::new();
    let clip = d.start_point_text();
    d.ok("ui.type", json!({"text": "a"}));
    d.frames(2);
    d.harness.input_mut().events.extend([key_press(egui::Key::Escape), ime_commit("x")]);
    d.frames(2);
    assert_eq!(layer_texts(&mut d, clip), vec!["a".to_string()]);
    assert!(d.harness.state().ui.gfx_edit.is_none(), "Escape ended the edit");
    assert!(d.ime().is_none(), "the closed editor publishes no descriptor");
}

/// A pointer press later in the batch is handled after `text_edit`; a commit after that press
/// belongs to the new target and must not be inserted into the old one.
#[test]
fn pointer_press_before_a_commit_does_not_edit_the_old_layer() {
    let mut d = Driver::new();
    let clip = new_text_layer(&mut d, None, "alpha", 500.0);
    new_text_layer(&mut d, Some(clip), "beta", 900.0);
    d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 0, "caret": 5, "anchor": 5}}));
    d.frames(3);
    let rect = d.rect(&format!("program.layer.{clip}.1"));
    d.harness.input_mut().events.extend([
        egui::Event::PointerButton {
            pos: egui::pos2((rect[0] + rect[2] / 2.0) as f32, (rect[1] + rect[3] / 2.0) as f32),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        },
        ime_commit("x"),
    ]);
    d.frames(2);
    assert_eq!(layer_texts(&mut d, clip), vec!["alpha".to_string(), "beta".to_string()]);
}

/// Moving within the same layer keeps its owner, but a known composition belongs to the old
/// caret. Real press/release input moves it; the late replacement arrives in a separate batch.
#[test]
fn ime_pointer_caret_move_rejects_next_batch_old_composition() {
    for terminal_pending in [false, true] {
        for bytes in [false, true] {
            let mut d = Driver::new();
            let clip = new_text_layer(&mut d, None, "alpha", 500.0);
            d.ok("ui.set", json!({"tool": "Type", "gfxEdit": {"clip": clip, "layer": 0, "caret": 5, "anchor": 5}}));
            d.frames(3);
            let rect = d.rect(&format!("program.layer.{clip}.0"));
            let pos = egui::pos2((rect[0] + 1.0) as f32, (rect[1] + rect[3] / 2.0) as f32);
            d.harness.input_mut().events.push(ime_preedit("に"));
            d.frames(1);
            if terminal_pending {
                d.harness.input_mut().events.push(ime_preedit(""));
                d.frames(1);
            }
            let before = d.ime().expect("a live graphics owner");
            // A click while composing keeps the marked text as typed.
            let typed = if terminal_pending { "alpha" } else { "alphaに" };
            d.harness.input_mut().events.extend([
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE },
            ]);
            d.frames(1);
            // A focus loss already disables the platform context; otherwise record its
            // one-shot interruption even when the release frame no longer repeats it.
            let press_interrupted = d.ime().is_none_or(|ime| ime.should_interrupt_composition);
            d.harness.input_mut().events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            });
            d.frames(1);
            let edit = d.harness.state().ui.gfx_edit.as_ref().expect("same-layer edit remains active");
            let caret = edit.caret;
            assert!(caret < 5 && edit.anchor == caret, "the real click must actually move the caret");
            let moved = d.ime().expect("same owner after pointer movement");
            assert_eq!(moved.owner, before.owner, "moving the caret does not replace the layer owner");
            assert!(press_interrupted || moved.should_interrupt_composition, "the platform cancels the known old composition");
            let undo = d.harness.state().session.history.undo.len();
            let delete = if bytes {
                egui::Event::Ime(egui::ImeEvent::DeleteSurroundingBytes { before_bytes: 0, after_bytes: 1 })
            } else {
                egui::Event::Ime(egui::ImeEvent::DeleteSurrounding { before_chars: 0, after_chars: 1 })
            };
            d.harness.input_mut().events.extend([delete, ime_commit("stale")]);
            d.frames(1);
            assert_eq!(layer_texts(&mut d, clip), vec![typed.to_string()], "old composition cannot edit the clicked caret");
            assert_eq!(d.harness.state().session.history.undo.len(), undo, "rejection creates no Undo transaction");
            // Rejected terminals do not release the guard. The actual reset fence does.
            let fence = d.reset_completed();
            d.harness.input_mut().events.extend([fence, ime_preedit("fresh"), ime_commit("X")]);
            d.frames(1);
            assert_eq!(layer_texts(&mut d, clip), vec![format!("{}X{}", &typed[..caret], &typed[caret..])]);
        }
    }
}

/// Control and keyboard caret moves are application-origin too, without changing stable owner
/// identity. A direct commit remains valid when no old composition was interrupted.
#[test]
fn ime_same_owner_selection_move_rejects_only_a_known_composition() {
    for control_move in [false, true] {
        for composing in [false, true] {
            let mut d = Driver::new();
            let clip = new_text_layer(&mut d, None, "alpha", 500.0);
            d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 0, "caret": 5, "anchor": 5}}));
            d.frames(3);
            let before = d.ime().expect("initial owner");
            if composing {
                d.harness.input_mut().events.push(ime_preedit("に"));
                d.frames(1);
            }
            if control_move {
                d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 0, "caret": 0, "anchor": 0}}));
            } else {
                d.harness.input_mut().events.extend([ime_preedit(""), key_press(egui::Key::Home)]);
                d.frames(1);
            }
            assert_eq!(d.harness.state().ui.gfx_edit.as_ref().map(|e| (e.caret, e.anchor)), Some((0, 0)));
            let moved = d.ime().expect("same owner after caret movement");
            assert_eq!((moved.owner, moved.context_generation), (before.owner, before.context_generation));
            assert_eq!(moved.should_interrupt_composition, composing);
            let undo = d.harness.state().session.history.undo.len();
            d.harness.input_mut().events.push(ime_commit("X"));
            d.frames(1);
            assert_eq!(layer_texts(&mut d, clip), vec![if composing { "alpha" } else { "Xalpha" }.to_string()]);
            if composing {
                assert_eq!(d.harness.state().session.history.undo.len(), undo);
            }
        }
    }
}

/// A focused custom text widget consumes raw Ctrl/Cmd+A itself, rather than the menu helper.
/// Its new selection must not receive the terminal event of an older composition next batch.
#[test]
fn ime_raw_select_all_rejects_old_terminal_input_and_preserves_direct_and_fresh_commits() {
    for phase in ["none", "active", "terminal_pending"] {
        for bytes in [false, true] {
            let mut d = Driver::new();
            let clip = new_text_layer(&mut d, None, "alpha", 500.0);
            d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 0, "caret": 5, "anchor": 5}}));
            d.frames(3);
            if phase != "none" {
                d.harness.input_mut().events.push(ime_preedit("に"));
                d.frames(1);
                if phase == "terminal_pending" {
                    d.harness.input_mut().events.push(ime_preedit(""));
                    d.frames(1);
                }
            }
            let before = d.ime().expect("the custom graphics editor is focused");
            assert_eq!(d.harness.ctx.memory(|m| m.focused()), Some(egui::Id::unique("gfx-text-edit")));
            assert!(d.harness.ctx.egui_wants_keyboard_input(), "raw Select All must reach the focused-widget path");
            let command = egui::Modifiers { ctrl: !cfg!(target_os = "macos"), mac_cmd: cfg!(target_os = "macos"), command: true, ..egui::Modifiers::NONE };
            // Keys are the input method's while it composes: its marked text empties first.
            d.harness.input_mut().events.extend([
                ime_preedit(""),
                egui::Event::ModifiersChanged(command),
                egui::Event::Key { key: egui::Key::A, physical_key: Some(egui::Key::A), pressed: true, repeat: false, modifiers: command },
            ]);
            d.frames(1);
            assert_eq!(d.harness.state().ui.gfx_edit.as_ref().map(|e| (e.anchor, e.caret)), Some((0, 5)), "the raw key really selects all text");
            let selected = d.ime().expect("the same custom owner reports its selection");
            assert_eq!((selected.owner, selected.context_generation), (before.owner, before.context_generation));
            assert_eq!(selected.should_interrupt_composition, phase != "none", "only the known old composition is interrupted");
            let undo = d.harness.state().session.history.undo.len();
            let deletion = if bytes {
                egui::Event::Ime(egui::ImeEvent::DeleteSurroundingBytes { before_bytes: 1, after_bytes: 0 })
            } else {
                egui::Event::Ime(egui::ImeEvent::DeleteSurrounding { before_chars: 1, after_chars: 0 })
            };
            // Surrounding deletion excludes the selected range; the terminal commit would replace
            // the whole selection if the old composition were not rejected. Release is in this
            // later batch too, so no control/menu invocation can stand in for the actual key.
            d.harness.input_mut().events.extend([
                egui::Event::Key { key: egui::Key::A, physical_key: Some(egui::Key::A), pressed: false, repeat: false, modifiers: command },
                egui::Event::ModifiersChanged(egui::Modifiers::NONE),
                deletion,
                ime_commit("X"),
            ]);
            d.frames(1);
            if phase == "none" {
                assert_eq!(layer_texts(&mut d, clip), vec!["X".to_string()], "without old preedit a direct commit replaces the selection");
                assert_eq!(d.harness.state().session.history.undo.len(), undo + 1);
            } else {
                assert_eq!(layer_texts(&mut d, clip), vec!["alpha".to_string()], "old terminal input cannot replace the new selection");
                assert_eq!(d.harness.state().session.history.undo.len(), undo, "rejected input creates no Undo transaction");
                assert!(!d.ime().expect("the unchanged selected editor remains focused").should_interrupt_composition, "the interruption is one-shot");
                let fence = d.reset_completed();
                d.harness.input_mut().events.extend([fence, ime_preedit("fresh"), ime_commit("Y")]);
                d.frames(1);
                assert_eq!(layer_texts(&mut d, clip), vec!["Y".to_string()], "a fresh composition replaces the selection normally");
                assert_eq!(d.harness.state().session.history.undo.len(), undo + 1);
            }
        }
    }
}

/// A surrounding replacement at the original composing caret is still accepted; the
/// interruption guard is not a blanket commit filter.
#[test]
fn ime_uninterrupted_same_owner_replacement_still_applies() {
    let mut d = Driver::new();
    let clip = new_text_layer(&mut d, None, "alpha", 500.0);
    d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 0, "caret": 5, "anchor": 5}}));
    d.frames(3);
    d.harness.input_mut().events.push(ime_preedit("に"));
    d.frames(1);
    d.harness.input_mut().events.extend([egui::Event::Ime(egui::ImeEvent::DeleteSurroundingBytes { before_bytes: 1, after_bytes: 0 }), ime_commit("X")]);
    d.frames(1);
    let ime = d.ime().expect("the replacement keeps the context");
    assert!(!ime.should_interrupt_composition);
    assert_eq!(layer_texts(&mut d, clip), vec!["alphX".to_string()]);
}

/// Adapter acceptance with real Context output IDs; these injected controls are not an OS test.
#[test]
fn ime_reset_fence_admits_the_first_fresh_composition_without_an_old_terminal() {
    for terminal_pending in [false, true] {
        for split in [false, true] {
            let mut d = Driver::new();
            let clip = new_text_layer(&mut d, None, "alpha", 500.0);
            new_text_layer(&mut d, Some(clip), "beta", 900.0);
            d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 0, "caret": 5, "anchor": 5}}));
            d.frames(2);
            d.harness.input_mut().events.push(ime_preedit("に"));
            d.frames(1);
            if terminal_pending {
                d.harness.input_mut().events.push(ime_preedit(""));
                d.frames(1);
            }
            d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 1, "caret": 4, "anchor": 4}}));
            d.frames(2);
            let fence = d.reset_completed();
            let requests = d.reset_requests.len();
            let undo = d.harness.state().session.history.undo.len();
            // A nonterminal stale update/deletion and arbitrary idle time cannot release it.
            d.harness
                .input_mut()
                .events
                .extend([ime_preedit("old"), egui::Event::Ime(egui::ImeEvent::DeleteSurroundingBytes { before_bytes: 1, after_bytes: 0 })]);
            d.frames(5);
            assert_eq!(d.reset_requests.len(), requests, "unchanged waiting passes must not issue new requests");
            assert_eq!(layer_texts(&mut d, clip), vec!["alpha".to_owned(), "beta".to_owned()]);
            assert_eq!(d.harness.state().session.history.undo.len(), undo);
            d.harness.input_mut().events.push(fence);
            if split {
                d.frames(1); // F alone, with no fabricated old commit before it.
            }
            d.harness.input_mut().events.extend([ime_preedit("が"), ime_commit("が")]);
            d.frames(1);
            assert_eq!(layer_texts(&mut d, clip), vec!["alpha".to_owned(), "betaが".to_owned()], "terminal_pending={terminal_pending}, split={split}");
            assert_eq!(d.harness.state().session.history.undo.len(), undo + 1);
            d.exec("edit.undo", json!({}));
            assert_eq!(layer_texts(&mut d, clip), vec!["alpha".to_owned(), "beta".to_owned()]);
        }
    }
}

#[test]
fn ime_reset_local_caret_changes_supersede_a_completed_fence_before_the_next_event() {
    for two_moves in [false, true] {
        let mut d = Driver::new();
        let clip = new_text_layer(&mut d, None, "alpha", 500.0);
        d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 0, "caret": 5, "anchor": 5}}));
        d.frames(2);
        d.harness.input_mut().events.push(ime_preedit("に"));
        d.frames(1);
        d.harness.input_mut().events.extend([ime_preedit(""), key_press(egui::Key::Home)]);
        d.frames(1);
        let first = d.reset_completed();
        let first_id = *d.reset_requests.last().unwrap();
        let undo = d.harness.state().session.history.undo.len();
        let mut events = vec![first.clone(), key_press(egui::Key::End)];
        if two_moves {
            events.push(key_press(egui::Key::Home));
        }
        events.extend([first, egui::Event::Ime(egui::ImeEvent::DeleteSurrounding { before_chars: 1, after_chars: 1 }), ime_commit("stale")]);
        d.harness.input_mut().events.extend(events);
        d.frames(1);
        let final_id = *d.reset_requests.last().unwrap();
        assert_ne!(first_id, final_id, "the locally staged caret superseded the already completed request");
        let caret = if two_moves { 0 } else { 5 };
        assert_eq!(d.harness.state().ui.gfx_edit.as_ref().map(|e| (e.caret, e.anchor)), Some((caret, caret)));
        assert_eq!(layer_texts(&mut d, clip), vec!["alpha".to_owned()]);
        assert_eq!(d.harness.state().session.history.undo.len(), undo);
        let foreign = egui::Context::default().request_ime_reset(egui::ViewportId::from_hash_of("foreign-film-window")).unwrap();
        d.harness.input_mut().events.extend([
            egui::Event::ImeResetCompleted { request_id: first_id },
            egui::Event::ImeResetCompleted { request_id: foreign },
            ime_commit("still stale"),
            egui::Event::ImeResetCompleted { request_id: final_id },
            ime_preedit("が"),
            egui::Event::ImeResetCompleted { request_id: final_id }, // duplicate during fresh Active
            ime_commit("が"),
        ]);
        d.frames(1);
        assert_eq!(layer_texts(&mut d, clip), vec![if two_moves { "がalpha" } else { "alphaが" }.to_owned()]);
        assert_eq!(d.harness.state().session.history.undo.len(), undo + 1);
    }
}

#[test]
fn ime_reset_hidden_logic_or_missing_geometry_rearms_the_same_model_caret_once() {
    for hidden_logic in [false, true] {
        let mut d = Driver::new();
        let clip = new_text_layer(&mut d, None, "alpha", 500.0);
        d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 0, "caret": 5, "anchor": 5}}));
        d.frames(2);
        d.harness.input_mut().events.push(ime_preedit("に"));
        d.frames(1);
        d.harness.input_mut().events.extend([ime_preedit(""), key_press(egui::Key::Home)]);
        d.frames(1);
        let first = *d.reset_requests.last().unwrap();
        let request_count = d.reset_requests.len();
        let edit = d.harness.state().ui.gfx_edit.clone();
        d.frames(4);
        assert_eq!(d.reset_requests.len(), request_count, "logic before normal UI cannot suspend/rearm every frame");
        if hidden_logic {
            let ctx = d.harness.ctx.clone();
            for _ in 0..3 {
                let output = ctx.run_logic(&egui::RawInput::default(), |ctx| {
                    d.harness.state_mut().logic_inner(ctx);
                    d.harness.state_mut().finish_ime_reset_logic(ctx);
                });
                assert!(output.platform_output.ime.is_none());
            }
        } else {
            d.harness.state_mut().ui.mode = filmcraft_ui_egui::state::Mode::Import;
            d.frames(3);
            assert!(d.ime().is_none());
            d.harness.state_mut().ui.mode = filmcraft_ui_egui::state::Mode::Edit;
        }
        assert_eq!(d.harness.state().ui.gfx_edit, edit, "the model target/caret stayed the same");
        d.frames(1);
        let current = *d.reset_requests.last().unwrap();
        assert_ne!(current, first, "reactivation must publish a NEW token even for the unchanged caret");
        let after_activation = d.reset_requests.len();
        d.frames(4);
        assert_eq!(d.reset_requests.len(), after_activation);
        d.harness.input_mut().events.extend([egui::Event::ImeResetCompleted { request_id: first }, ime_commit("old")]);
        d.frames(1);
        assert_eq!(layer_texts(&mut d, clip), vec!["alpha".to_owned()]);
        d.harness.input_mut().events.push(egui::Event::ImeResetCompleted { request_id: current });
        d.frames(1);
        d.harness.input_mut().events.extend([ime_preedit("が"), ime_commit("が")]);
        d.frames(1);
        assert_eq!(layer_texts(&mut d, clip), vec!["がalpha".to_owned()]);
    }
}

#[test]
fn ime_reset_discarded_request_then_no_descriptor_rearms_on_return() {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    struct OutputWitness(Arc<Mutex<Vec<egui::ImeResetId>>>);
    impl egui::Plugin for OutputWitness {
        fn debug_name(&self) -> &'static str {
            "Film reset output witness"
        }
        fn output_hook(&mut self, _ctx: &egui::Context, output: &mut egui::FullOutput) {
            if let Some(id) = output.platform_output.ime.as_ref().and_then(|ime| ime.reset_request) {
                self.0.lock().unwrap().push(id);
            }
        }
    }
    let mut d = Driver::new();
    let clip = new_text_layer(&mut d, None, "alpha", 500.0);
    d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 0, "caret": 5, "anchor": 5}}));
    d.frames(2);
    d.harness.input_mut().events.push(ime_preedit("に"));
    d.frames(1);
    let observed = Arc::new(Mutex::new(Vec::new()));
    d.harness.ctx.add_plugin(OutputWitness(Arc::clone(&observed)));
    let phase = Arc::new(AtomicUsize::new(0));
    let begin_phase = Arc::clone(&phase);
    let stock = egui::Id::unique("film-reset-missing-custom-geometry");
    d.harness.ctx.on_begin_pass(
        "Film reset discarded second-pass absence",
        Arc::new(move |ui| {
            if begin_phase.load(Ordering::Relaxed) == 1 {
                ui.memory_mut(|memory| memory.request_focus(stock));
                begin_phase.store(2, Ordering::Relaxed);
            }
        }),
    );
    d.harness.ctx.on_end_pass(
        "Film reset discard first request output",
        Arc::new(move |ui| {
            if phase.compare_exchange(0, 1, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
                ui.ctx().request_discard("exercise actual request consumption followed by final None");
            }
        }),
    );
    d.harness.input_mut().events.extend([ime_preedit(""), key_press(egui::Key::Home)]);
    d.frames(1);
    assert!(d.harness.output().platform_output.num_completed_passes >= 2);
    assert!(d.ime().is_none(), "the final discarded rerun published no custom descriptor");
    let first = *observed.lock().unwrap().last().expect("the first discarded pass carried an actual request");
    assert_eq!(d.harness.state().ui.gfx_edit.as_ref().map(|e| (e.caret, e.anchor)), Some((0, 0)));
    d.harness.ctx.memory_mut(|memory| memory.surrender_focus(stock));
    d.frames(1);
    let current = *d.reset_requests.last().expect("returning custom output rearms");
    assert_ne!(first, current);
    d.harness.input_mut().events.extend([
        egui::Event::ImeResetCompleted { request_id: first },
        ime_commit("old"),
        egui::Event::ImeResetCompleted { request_id: current },
        ime_preedit("が"),
        ime_commit("が"),
    ]);
    d.frames(1);
    assert_eq!(layer_texts(&mut d, clip), vec!["がalpha".to_owned()]);
}

/// The ordinary multipass case: the first pass's request is discarded, but the final rerun
/// publishes the same custom editor. Core must carry R1 (not a newly allocated token or an
/// untokened interrupt), and that exact fence then admits a fresh suffix without an old terminal.
#[test]
fn ime_reset_discarded_pass_keeps_the_same_custom_request_and_admits_a_fresh_suffix() {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    struct OutputWitness(Arc<Mutex<Vec<(egui::ImeResetId, bool)>>>);
    impl egui::Plugin for OutputWitness {
        fn debug_name(&self) -> &'static str {
            "Film reset carried-request witness"
        }
        fn output_hook(&mut self, _ctx: &egui::Context, output: &mut egui::FullOutput) {
            if let Some(ime) = output.platform_output.ime.as_ref()
                && let Some(id) = ime.reset_request
            {
                self.0.lock().unwrap().push((id, ime.should_interrupt_composition));
            }
        }
    }
    let mut d = Driver::new();
    let clip = new_text_layer(&mut d, None, "alpha", 500.0);
    d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 0, "caret": 5, "anchor": 5}}));
    d.frames(2);
    d.harness.input_mut().events.push(ime_preedit("に"));
    d.frames(1);
    let owner = d.ime().expect("the custom editor published its descriptor").owner;
    let observed = Arc::new(Mutex::new(Vec::new()));
    d.harness.ctx.add_plugin(OutputWitness(Arc::clone(&observed)));
    let phase = Arc::new(AtomicUsize::new(0));
    let end_phase = Arc::clone(&phase);
    d.harness.ctx.on_end_pass(
        "Film reset discard one same-custom pass",
        Arc::new(move |ui| {
            if end_phase.compare_exchange(0, 1, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
                ui.ctx().request_discard("exercise request carry into a same-custom final pass");
            }
        }),
    );
    d.harness.input_mut().events.extend([ime_preedit(""), key_press(egui::Key::Home)]);
    d.frames(1);
    assert!(d.harness.output().platform_output.num_completed_passes >= 2);
    let ime = d.ime().expect("the final custom descriptor was published");
    assert_eq!(ime.owner, owner, "the final descriptor is the same custom editor");
    let id = ime.reset_request.expect("the final custom descriptor carries R1");
    assert!(ime.should_interrupt_composition);
    let seen = observed.lock().unwrap();
    // `output_hook` runs per pass, before core applies the discarded-pass carry in
    // `FullOutput::append`. Only the discarded pass publishes R1 here; the final rerun's own
    // pass output has no request, and the combined output above proves the carry.
    assert_eq!(seen.as_slice(), &[(id, true)], "exactly the discarded pass emitted R1 as a real interruption");
    drop(seen);
    // No fabricated old terminal: only the exact completion boundary and a fresh composition.
    d.harness.input_mut().events.extend([egui::Event::ImeResetCompleted { request_id: id }, ime_preedit("が"), ime_commit("が")]);
    d.frames(1);
    assert_eq!(layer_texts(&mut d, clip), vec!["がalpha".to_owned()]);
}

/// A host picker and an in-app modal own input. Entering either must retire/suspend the guard
/// without allocating a NEW request; leaving it rearms exactly one fresh boundary.
#[test]
fn ime_reset_native_picker_and_in_app_modal_do_not_allocate_a_new_request() {
    use filmcraft_ui_egui::Dialog;
    use filmcraft_ui_egui::native_dialogs::FileDialog;
    use std::sync::{Arc, Mutex};
    let senders: Arc<Mutex<Vec<Sender<Vec<String>>>>> = Arc::new(Mutex::new(Vec::new()));
    let picker_senders = Arc::clone(&senders);
    let mut d = Driver::new_with(move |app| {
        app.hooks.run_dialog = Some(Box::new(move |_dialog| {
            let (tx, rx) = channel();
            picker_senders.lock().unwrap().push(tx);
            rx
        }));
    });
    let clip = new_text_layer(&mut d, None, "alpha", 500.0);
    d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 0, "caret": 5, "anchor": 5}}));
    d.frames(2);
    d.harness.input_mut().events.push(ime_preedit("に"));
    d.frames(1);
    d.harness.input_mut().events.extend([ime_preedit(""), key_press(egui::Key::Home)]);
    d.frames(1);
    let first = *d.reset_requests.last().expect("the interruption published a fence");

    let opened = d.harness.state_mut().pick(FileDialog::OpenProject, |_, _| Ok(Value::Null)).unwrap();
    assert_eq!(opened["dialog"], json!("open"));
    d.frames(3);
    assert!(d.harness.state().native_dialog_open());
    let requests = d.reset_requests.len();
    assert!(d.ime().is_none_or(|ime| ime.reset_request.is_none()), "an open picker cannot publish a NEW reset");
    assert_eq!(d.reset_requests.len(), requests, "an open picker cannot allocate a NEW reset");
    d.harness.input_mut().events.extend([ime_preedit("old"), ime_commit("old")]);
    d.frames(2);
    assert_eq!(d.reset_requests.len(), requests);
    assert_eq!(layer_texts(&mut d, clip), vec!["alpha".to_owned()], "picker-owned input cannot edit the editor");
    if let Some(tx) = senders.lock().unwrap().pop() {
        tx.send(Vec::new()).unwrap();
    }
    d.frames(2);
    assert!(!d.harness.state().native_dialog_open());
    let after_picker = *d.reset_requests.last().expect("returning from the picker rearms");
    assert_ne!(after_picker, first);
    let request_count = d.reset_requests.len();
    d.frames(3);
    assert_eq!(d.reset_requests.len(), request_count, "waiting after rearm must not churn IDs");

    d.harness.state_mut().dialog = Some(Dialog::About);
    d.frames(3);
    assert_eq!(d.reset_requests.len(), request_count, "an in-app modal cannot allocate a NEW reset");
    d.harness.state_mut().dialog = None;
    d.frames(2);
    let latest = *d.reset_requests.last().expect("leaving the in-app modal rearms");
    assert_ne!(latest, after_picker);
    d.harness.input_mut().events.extend([
        egui::Event::ImeResetCompleted { request_id: after_picker },
        ime_commit("old"),
        egui::Event::ImeResetCompleted { request_id: latest },
        ime_preedit("が"),
        ime_commit("が"),
    ]);
    d.frames(1);
    assert_eq!(layer_texts(&mut d, clip), vec!["がalpha".to_owned()]);
}

#[test]
fn ime_reset_pointer_and_escape_suffix_controls_cannot_edit_the_abandoned_target() {
    for escape in [false, true] {
        let mut d = Driver::new();
        let clip = new_text_layer(&mut d, None, "alpha", 500.0);
        d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 0, "caret": 5, "anchor": 5}}));
        d.frames(2);
        d.harness.input_mut().events.push(ime_preedit("に"));
        d.frames(1);
        d.harness.input_mut().events.extend([ime_preedit(""), key_press(egui::Key::Home)]);
        d.frames(1);
        let old = *d.reset_requests.last().unwrap();
        // An Escape right after a composition ends is the input method's; this one is the user's.
        d.frames(4);
        let boundary = if escape {
            key_press(egui::Key::Escape)
        } else {
            egui::Event::PointerButton { pos: egui::pos2(-20.0, -20.0), button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE }
        };
        let undo = d.harness.state().session.history.undo.len();
        d.harness.input_mut().events.extend([
            ime_commit("old prefix"),
            egui::Event::ImeResetCompleted { request_id: old },
            boundary,
            egui::Event::Text("wrong caret".into()),
            egui::Event::Paste("wrong paste".into()),
            egui::Event::Cut,
            egui::Event::ImeResetCompleted { request_id: old },
            ime_preedit("が"),
            ime_commit("が"),
        ]);
        d.frames(1);
        assert_eq!(layer_texts(&mut d, clip), vec!["alpha".to_owned()]);
        assert_eq!(d.harness.state().session.history.undo.len(), undo);
        if escape {
            assert!(d.harness.state().ui.gfx_edit.is_none());
            d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 0, "caret": 0, "anchor": 0}}));
        } else {
            d.harness.input_mut().events.push(egui::Event::PointerButton {
                pos: egui::pos2(-20.0, -20.0),
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            });
        }
        d.frames(2);
        let current = *d.reset_requests.last().unwrap();
        assert_ne!(old, current);
        d.harness.input_mut().events.extend([
            egui::Event::ImeResetCompleted { request_id: old },
            ime_commit("old suffix"),
            egui::Event::ImeResetCompleted { request_id: current },
            ime_preedit("が"),
            ime_commit("が"),
        ]);
        d.frames(1);
        assert_eq!(layer_texts(&mut d, clip), vec!["がalpha".to_owned()]);
    }
}

#[test]
fn ime_reset_stock_text_edit_keeps_its_output_and_custom_return_gets_a_new_fence() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    struct StockField {
        enabled: Arc<AtomicBool>,
        text: String,
        id: egui::Id,
    }
    impl egui::Plugin for StockField {
        fn debug_name(&self) -> &'static str {
            "Film reset stock-field control"
        }
        fn on_begin_pass(&mut self, ui: &mut egui::Ui) {
            if self.enabled.load(Ordering::Relaxed) {
                ui.memory_mut(|memory| memory.request_focus(self.id));
                ui.add(egui::TextEdit::singleline(&mut self.text).id(self.id));
            }
        }
    }
    let mut d = Driver::new();
    let clip = new_text_layer(&mut d, None, "alpha", 500.0);
    d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 0, "caret": 5, "anchor": 5}}));
    d.frames(2);
    let owner = d.ime().unwrap().owner;
    d.harness.input_mut().events.push(ime_preedit("に"));
    d.frames(1);
    d.harness.input_mut().events.extend([ime_preedit(""), key_press(egui::Key::Home)]);
    d.frames(1);
    let first = *d.reset_requests.last().unwrap();
    let edit = d.harness.state().ui.gfx_edit.clone();
    let enabled = Arc::new(AtomicBool::new(true));
    let id = egui::Id::unique("film-reset-stock-text-field");
    d.harness.ctx.add_plugin(StockField { enabled: Arc::clone(&enabled), text: String::new(), id });
    d.harness.input_mut().events.extend([egui::Event::ImeResetCompleted { request_id: first }, egui::Event::Text("stock".into())]);
    d.frames(1);
    let stock = d.ime().expect("the actual stock TextEdit retains its descriptor");
    assert_ne!(stock.owner, owner);
    assert!(stock.reset_request.is_none(), "a previously dispatched custom request cannot cause a NEW stock request");
    assert_eq!(stock.surrounding_text.available().map(|p| p.text.as_str()), Some("stock"), "the stock field's own descriptor");
    assert_eq!(d.harness.state().ui.gfx_edit, edit);
    assert_eq!(layer_texts(&mut d, clip), vec!["alpha".to_owned()]);
    enabled.store(false, Ordering::Relaxed);
    d.harness.ctx.memory_mut(|memory| memory.surrender_focus(id));
    d.frames(2);
    let current = *d.reset_requests.last().unwrap();
    assert_ne!(current, first);
    assert_eq!(d.ime().unwrap().owner, owner);
    d.harness.input_mut().events.extend([
        egui::Event::ImeResetCompleted { request_id: first },
        ime_commit("old"),
        egui::Event::ImeResetCompleted { request_id: current },
        ime_preedit("が"),
        ime_commit("が"),
    ]);
    d.frames(1);
    assert_eq!(layer_texts(&mut d, clip), vec!["がalpha".to_owned()]);
}

/// The concrete collision the uid-less fingerprint missed: two layers with identical static
/// geometry (position, size, no name) but different text. Reordering, deleting and undoing must
/// keep typing in the layer the user started editing, not in whatever now occupies its index.
#[test]
fn identical_layers_with_different_text_keep_the_edit_on_the_started_layer() {
    let mut d = Driver::new();
    let clip = d.exec("graphics.newText", json!({"text": "one", "position": [960, 540], "size": 100}))["clip"].as_u64().unwrap();
    d.exec("graphics.newText", json!({"text": "two", "position": [960, 540], "size": 100, "clip": clip}));
    let uids: Vec<u64> = d.exec("graphics.list", json!({"clip": clip}))["layers"].as_array().unwrap().iter().map(|l| l["uid"].as_u64().unwrap()).collect();
    assert_eq!(uids.len(), 2);
    assert!(uids.iter().all(|uid| *uid != 0));
    assert_ne!(uids[0], uids[1], "identical geometry and different text get distinct identities");
    d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 0, "caret": 3, "anchor": 3}}));
    d.frames(3);
    assert_eq!(d.harness.state().ui.gfx_edit.as_ref().map(|e| (e.layer, e.uid)), Some((0, uids[0])));

    // Move the edited layer to the front. The other layer now sits at index 0 (same fingerprint).
    d.exec("graphics.arrangeLayer", json!({"clip": clip, "layer": 0, "to": "front"}));
    d.frames(2);
    assert_eq!(d.harness.state().ui.gfx_edit.as_ref().map(|e| (e.layer, e.uid)), Some((1, uids[0])));
    d.ok("ui.type", json!({"text": "X"}));
    d.frames(2);
    assert_eq!(layer_texts(&mut d, clip), vec!["two".to_string(), "oneX".to_string()], "typing followed the uid, not index 0");

    // Delete the layer below it; the edit stays on "oneX" and keeps typing there.
    d.exec("graphics.deleteLayer", json!({"clip": clip, "layer": 0}));
    d.frames(2);
    assert_eq!(d.harness.state().ui.gfx_edit.as_ref().map(|e| (e.layer, e.uid)), Some((0, uids[0])));
    d.ok("ui.type", json!({"text": "Y"}));
    d.frames(2);
    assert_eq!(layer_texts(&mut d, clip), vec!["oneXY".to_string()]);

    // Undo the typing and then the deletion: the same uid is edited at its restored index.
    d.exec("edit.undo", json!({}));
    d.frames(2);
    d.exec("edit.undo", json!({}));
    d.frames(2);
    assert_eq!(d.harness.state().ui.gfx_edit.as_ref().map(|e| (e.layer, e.uid)), Some((1, uids[0])));
    d.ok("ui.type", json!({"text": "Z"}));
    d.frames(2);
    assert_eq!(layer_texts(&mut d, clip), vec!["two".to_string(), "oneXZ".to_string()]);
}

/// Every graphic layer created by `graphics.newText` has a stable uid. The edit target follows
/// that uid through a real reorder, a real deletion below it, and undo/redo.
#[test]
fn edit_target_follows_reorder_delete_and_undo() {
    let mut d = Driver::new();
    let clip = new_text_layer(&mut d, None, "one", 400.0);
    new_text_layer(&mut d, Some(clip), "two", 800.0);
    new_text_layer(&mut d, Some(clip), "three", 1200.0);
    d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 0, "caret": 3, "anchor": 3}}));
    d.frames(3);
    let uid = d.harness.state().ui.gfx_edit.as_ref().map(|e| e.uid).unwrap_or(0);
    assert_ne!(uid, 0, "the created layer has a stable uid");
    assert_eq!(d.harness.state().ui.gfx_edit.as_ref().map(|e| e.layer), Some(0));

    // Reorder layer 0 (the edited "one") to the front, then type: the text must follow it.
    d.exec("graphics.arrangeLayer", json!({"clip": clip, "layer": 0, "to": "front"}));
    d.frames(2);
    assert_eq!(d.harness.state().ui.gfx_edit.as_ref().map(|e| e.layer), Some(2), "the edit follows the reordered layer");
    d.ok("ui.type", json!({"text": "A"}));
    d.frames(2);
    assert_eq!(layer_texts(&mut d, clip), vec!["two".to_string(), "three".to_string(), "oneA".to_string()]);

    // Undo the typing and then the arrange: the edit follows the same layer back to index 0.
    d.exec("edit.undo", json!({}));
    d.frames(2);
    d.exec("edit.undo", json!({}));
    d.frames(2);
    assert_eq!(d.harness.state().ui.gfx_edit.as_ref().map(|e| e.layer), Some(0));
    d.ok("ui.type", json!({"text": "B"}));
    d.frames(2);
    assert_eq!(layer_texts(&mut d, clip), vec!["oneB".to_string(), "two".to_string(), "three".to_string()]);

    // Edit "two" (index 1), delete the layer below it, and keep typing into "two".
    d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 1, "caret": 3, "anchor": 3}}));
    d.frames(2);
    d.exec("graphics.deleteLayer", json!({"clip": clip, "layer": 0}));
    d.frames(2);
    assert_eq!(d.harness.state().ui.gfx_edit.as_ref().map(|e| e.layer), Some(0));
    d.ok("ui.type", json!({"text": "D"}));
    d.frames(2);
    assert_eq!(layer_texts(&mut d, clip), vec!["twoD".to_string(), "three".to_string()]);
}

/// Replacing the project clears the edit and the IME owner even though `file.newProject` emits
/// only `ProjectChanged`, and a project with no sequence has no editor to observe.
#[test]
fn new_project_without_a_sequence_clears_the_ime_edit() {
    let mut d = Driver::new();
    d.start_point_text();
    d.ok("ui.type", json!({"text": "x"}));
    d.frames(2);
    assert!(d.ime().is_some(), "the text editor owns the IME before the replacement");
    d.exec("file.newProject", json!({"name": "Fresh"}));
    d.frames(3);
    assert!(d.harness.state().ui.gfx_edit.is_none(), "the replaced document has no edit target");
    assert!(d.ime().is_none(), "no sequence means no visible editor and no descriptor");
}

/// `engine.execute`/`ui.menu.invoke` run after the UI pass. A same-pass text change leaves the
/// drawn caret stale, so that pass publishes no descriptor; the next pass draws and publishes
/// the caret of the new text.
#[test]
fn ime_descriptor_follows_control_text_mutations_in_the_same_pass() {
    let mut d = Driver::new();
    let clip = d.start_point_text();
    d.ok("ui.type", json!({"text": "one"}));
    d.frames(3);
    let before = d.ime().expect("the editor publishes");
    // `ui.menu.invoke` is one of the requests drained after the frame, unlike `engine.execute`.
    d.ok("ui.menu.invoke", json!({"id": "graphics.setText", "params": {"clip": clip, "layer": 0, "text": "a much longer line"}}));
    assert!(d.ime().is_none(), "the old caret must not be published with the changed text");
    d.frames(1);
    let after = d.ime().expect("the next pass publishes the new layout");
    assert_ne!(after.cursor_rect, before.cursor_rect, "the candidate window moved with the longer line");
    d.frames(1);
    let stable = d.ime().expect("a settled pass publishes again");
    assert_eq!(stable.cursor_rect, after.cursor_rect, "the published geometry matches the next pass");
}

/// A hostile `ui.set.gfxEdit` offset inside a code point is clamped by the editor's next pass,
/// which keeps publishing and leaves the text whole.
#[test]
fn invalid_gfx_edit_offsets_are_clamped() {
    let mut d = Driver::new();
    let clip = d.start_point_text();
    d.ok("ui.type", json!({"text": "é😀"}));
    d.frames(3);
    d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 0, "caret": 1, "anchor": 2}}));
    d.frames(1);
    assert!(d.ime().is_some(), "the clamped editor publishes");
    assert_eq!(caret(&d), (0, 2));
    assert_eq!(layer_texts(&mut d, clip), vec!["é😀".to_string()]);
}

/// #2430: a click (no drag) in the text being edited on the monitor puts the caret there; before,
/// only a press that moved a little did, and typing went on at the end.
#[test]
fn a_click_in_the_edited_text_moves_the_caret() {
    let mut d = Driver::new();
    d.ok("ui.set", json!({"tool": "Type"}));
    d.frames(3);
    let pic = d.rect("program.picture");
    d.ok("ui.click", json!({"x": pic[0] + pic[2] * 0.3, "y": pic[1] + pic[3] * 0.2}));
    d.ok("ui.type", json!({"text": "Title"}));
    d.frames(2);
    let text = d.rect("program.textEdit");
    // just inside the first letter: the caret goes before it
    d.ok("ui.click", json!({"x": text[0] + 1.0, "y": text[1] + text[3] / 2.0}));
    d.frames(2);
    d.ok("ui.type", json!({"text": "X"}));
    d.frames(2);
    assert_eq!(d.exec("graphics.list", json!({}))["layers"][0]["text"], "XTitle");
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

#[test]
fn graphic_resize_handles_show_their_screen_space_direction() {
    let mut d = Driver::new();
    let r = d.exec("graphics.newShape", json!({"shape": "rectangle", "position": [800, 500], "size": [500, 300]}));
    let clip = r["clip"].as_u64().unwrap();
    let pre = d.select(clip);

    for (handle, expected) in [
        (0, egui::CursorIcon::ResizeNwSe),
        (1, egui::CursorIcon::ResizeNeSw),
        (2, egui::CursorIcon::ResizeNwSe),
        (3, egui::CursorIcon::ResizeNeSw),
        (4, egui::CursorIcon::ResizeVertical),
        (5, egui::CursorIcon::ResizeHorizontal),
        (6, egui::CursorIcon::ResizeVertical),
        (7, egui::CursorIcon::ResizeHorizontal),
    ] {
        let at = d.handle(&pre, handle);
        d.hover(at);
        assert_eq!(d.cursor(), expected, "handle {handle}");
    }

    d.exec("graphics.set", json!({"clip": clip, "props": {"rotation": 45}}));
    d.frames(4);
    let at = d.handle(&pre, 4);
    d.hover(at);
    assert!(
        matches!(d.cursor(), egui::CursorIcon::ResizeNeSw | egui::CursorIcon::ResizeNwSe),
        "rotated edge handle should use a diagonal cursor, got {:?}",
        d.cursor()
    );
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

/// The Corner Radius of the shape layer of graphic clip `clip`.
fn corner_radius(d: &mut Driver, clip: u64) -> f64 {
    let s = &d.harness.state().session;
    let (_, it) = s.active_sequence().unwrap().find_item(filmcraft_project::ClipId(clip)).unwrap();
    let e = it.effects.iter().find(|e| e.effect == "graphic_shape").unwrap();
    e.param("corner_radius").unwrap().value_at(filmcraft_time::Tick::ZERO).as_f64().unwrap()
}

#[test]
fn a_rectangles_corner_circles_round_all_its_corners() {
    let mut d = Driver::new();
    let pre = d.shape();
    let clip: u64 = pre.split('.').nth(2).unwrap().parse().unwrap();
    let (tl, br) = (d.handle(&pre, 0), d.handle(&pre, 2));
    let k = (br.0 - tl.0) / 500.0; // screen points per layer pixel
    let corners: Vec<(f64, f64)> = (0..4).map(|n| d.at(&pre, &format!("corner.{n}"))).collect();
    // just inside each corner, on its diagonal
    assert!(corners[0].0 > tl.0 + 5.0 && corners[0].1 > tl.1 + 5.0 && corners[2].0 < br.0 - 5.0 && corners[2].1 < br.1 - 5.0, "{corners:?}");

    // the top-left circle dragged 40 points down and right: a radius of 40 points' travel
    d.drag(corners[0], (40.0, 40.0));
    let r = corner_radius(&mut d, clip);
    assert!((r - 40.0 / k).abs() < 2.0, "radius {r}, expected {}", 40.0 / k);
    // the circles moved in to the radius, all four alike
    let (tl2, br2) = (d.handle(&pre, 0), d.handle(&pre, 2));
    assert!(near(tl, tl2) && near(br, br2), "the box stays");
    let c0 = d.at(&pre, "corner.0");
    let c2 = d.at(&pre, "corner.2");
    assert!(near(c0, (tl.0 + r * k, tl.1 + r * k)) && near(c2, (br.0 - r * k, br.1 - r * k)), "{c0:?} {c2:?}");

    // any circle drags all four; past the middle it stops at half the shorter side
    d.drag(c2, (-300.0, -300.0));
    assert_eq!(corner_radius(&mut d, clip), 150.0);
    // back out squares them; one undo step per drag
    let c1 = d.at(&pre, "corner.1");
    d.drag(c1, (200.0, -200.0));
    assert_eq!(corner_radius(&mut d, clip), 0.0);
    d.exec("edit.undo", json!({}));
    assert_eq!(corner_radius(&mut d, clip), 150.0);
}

#[test]
fn only_rectangles_big_enough_get_corner_circles() {
    let mut d = Driver::new();
    let r = d.exec("graphics.newShape", json!({"shape": "ellipse", "position": [800, 500], "size": [500, 300]}));
    let pre = d.select(r["clip"].as_u64().unwrap());
    assert!(d.ok("ui.elements", json!({"prefix": format!("{pre}corner")})).as_array().unwrap().is_empty(), "an ellipse has none");
    let r = d.exec("graphics.newShape", json!({"shape": "rectangle", "position": [800, 500], "size": [20, 20]}));
    let pre = d.select(r["clip"].as_u64().unwrap());
    assert!(d.ok("ui.elements", json!({"prefix": format!("{pre}corner")})).as_array().unwrap().is_empty(), "a tiny rectangle has none");
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
    assert_eq!(ids(&mut d).len(), 14, "the layer, its eight handles, four corner circles and its anchor: {:?}", ids(&mut d));
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
    assert_eq!(ids(&mut d).len(), 14);
}

/// A double-click on a text layer edits it with all its text selected, so typing replaces it; while
/// it is edited, Select All selects the text again, not the timeline clips, as in Premiere (#1102).
#[test]
fn double_click_and_select_all_select_the_text_being_edited() {
    let mut d = Driver::new();
    let (clip, pre) = d.point_text();
    let (a, b) = (d.handle(&pre, 0), d.handle(&pre, 2));
    d.ok("ui.click", json!({"x": (a.0 + b.0) / 2.0, "y": (a.1 + b.1) / 2.0, "count": 2}));
    d.frames(4);
    let edit = |d: &Driver| d.harness.state().ui.gfx_edit.as_ref().map(|e| (e.clip, e.layer, e.anchor, e.caret));
    assert_eq!(edit(&d), Some((clip, 0, 0, "Title".len())), "the double-click selects the whole text");

    // caret at the end, then Select All as the native macOS menu sends it (it takes Cmd+A first)
    if let Some(e) = d.harness.state_mut().ui.gfx_edit.as_mut() {
        (e.anchor, e.caret) = (5, 5);
    }
    let selection = d.harness.state().session.state.selection.clone();
    let (tx, rx) = channel();
    d.harness.state_mut().command_inbox = Some(rx);
    tx.send("edit.selectAll".to_string()).unwrap();
    d.frames(2);
    assert_eq!(edit(&d), Some((clip, 0, 0, 5)), "Select All selects the text being edited");
    assert_eq!(d.harness.state().session.state.selection, selection, "and leaves the timeline selection alone");
}

/// #1953: while a graphic's text is edited, the native macOS menu's Copy and Cut (the menu bar
/// takes ⌘C and ⌘X first) copy the selected text, not the graphic clip, and Paste asks the window
/// for the clipboard text instead of pasting clips.
#[test]
fn native_edit_menu_copies_the_graphic_text_being_edited() {
    let mut d = Driver::new();
    let (clip, pre) = d.point_text();
    let (a, b) = (d.handle(&pre, 0), d.handle(&pre, 2));
    d.ok("ui.click", json!({"x": (a.0 + b.0) / 2.0, "y": (a.1 + b.1) / 2.0, "count": 2}));
    d.frames(4);
    let editing = |d: &Driver| d.harness.state().ui.gfx_edit.as_ref().map(|e| (e.clip, e.anchor, e.caret));
    assert_eq!(editing(&d), Some((clip, 0, "Title".len())), "the double-click selects the whole text");
    let project = d.harness.state().session.project.clone();
    let (tx, rx) = channel();
    d.harness.state_mut().command_inbox = Some(rx);
    tx.send("edit.copy".to_string()).unwrap();
    let mut copied = None;
    for _ in 0..4 {
        d.frames(1);
        copied = copied.or_else(|| {
            d.harness.output().platform_output.commands.iter().find_map(|c| match c {
                egui::OutputCommand::CopyText(t) => Some(t.clone()),
                _ => None,
            })
        });
    }
    assert_eq!(copied.as_deref(), Some("Title"), "Copy copies the selected text");
    assert!(std::sync::Arc::ptr_eq(&project, &d.harness.state().session.project), "no clip was copied or changed");
    assert!(d.harness.state().ui.gfx_edit.is_some(), "still editing the text");
    tx.send("edit.paste".to_string()).unwrap();
    d.frames(1);
    let asked = d.harness.output().viewport_output.get(&egui::ViewportId::ROOT).is_some_and(|v| v.commands.contains(&egui::ViewportCommand::RequestPaste));
    assert!(asked, "Paste asks the window for the clipboard text");
    assert!(std::sync::Arc::ptr_eq(&project, &d.harness.state().session.project), "no clips were pasted");
}

/// Position of each layer of `clip`, back first.
fn positions(d: &mut Driver, clip: u64) -> Vec<(f64, f64)> {
    let l = d.exec("graphics.list", json!({"clip": clip}));
    l["layers"].as_array().unwrap().iter().map(|x| (num(&x["position"][0]), num(&x["position"][1]))).collect()
}

/// #2186: Shift-click adds layers of a graphic to the selection, Cmd/Ctrl-click takes them out,
/// and dragging a selected layer moves every selected layer together, as one undo step.
#[test]
fn shift_click_selects_several_layers_and_a_drag_moves_them_together() {
    let mut d = Driver::new();
    let r = d.exec("graphics.newShape", json!({"shape": "rectangle", "position": [500, 400], "size": [240, 160]}));
    let clip = r["clip"].as_u64().unwrap();
    d.exec("graphics.newShape", json!({"shape": "ellipse", "clip": clip, "position": [1000, 400], "size": [200, 200]}));
    d.exec("graphics.newShape", json!({"shape": "rectangle", "clip": clip, "position": [1400, 800], "size": [200, 120]}));
    d.select(clip);
    let centre = |d: &mut Driver, l: usize| {
        let r = d.rect(&format!("program.layer.{clip}.{l}"));
        (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0)
    };
    let click = |d: &mut Driver, at: (f64, f64), m: Value| {
        d.ok("ui.click", json!({"x": at.0, "y": at.1, "modifiers": m}));
        d.frames(3);
    };
    let sel = |d: &mut Driver| d.harness.state().session.state.graphic_layers.clone();
    d.frames(10);
    let (c0, c1, c2) = (centre(&mut d, 0), centre(&mut d, 1), centre(&mut d, 2));
    click(&mut d, c1, json!({"shift": true}));
    assert_eq!(sel(&mut d), [0, 1], "Shift-click adds a layer");
    d.shot("graphic-layers-multi-select");

    // dragging one of them moves both by the same amount; the third stays
    let before = positions(&mut d, clip);
    let undo = d.harness.state().session.history.undo.len();
    // (away from the anchor point in the middle, which drags only the anchor)
    let r0 = d.rect(&format!("program.layer.{clip}.0"));
    assert!(near(c0, (r0[0] + r0[2] / 2.0, r0[1] + r0[3] / 2.0)), "the monitor settled");
    let grab = (r0[0] + r0[2] / 4.0, r0[1] + r0[3] / 2.0);
    d.drag(grab, (63.0, 41.0));
    let after = positions(&mut d, clip);
    let moved = |i: usize| (after[i].0 - before[i].0, after[i].1 - before[i].1);
    assert!(moved(0).0 > 20.0 && moved(0).1 > 10.0, "{before:?} → {after:?}");
    assert!(near(moved(0), moved(1)), "both moved alike: {:?} {:?}", moved(0), moved(1));
    assert_eq!(after[2], before[2]);
    assert_eq!(d.harness.state().session.history.undo.len(), undo + 1, "one undo step");
    assert_eq!(sel(&mut d), [0, 1], "the selection stays");
    d.exec("edit.undo", json!({}));
    assert_eq!(positions(&mut d, clip), before);

    // Cmd/Ctrl-click takes a layer out; Shift-click on a selected one too
    click(&mut d, c1, json!({"command": true}));
    assert_eq!(sel(&mut d), [0]);
    click(&mut d, c0, json!({"shift": true}));
    assert!(sel(&mut d).is_empty());
    // a plain click selects only the layer clicked
    click(&mut d, c2, json!({}));
    assert_eq!(sel(&mut d), [2]);

    // Shift-dragging an unselected layer adds it and moves the lot
    let before = positions(&mut d, clip);
    d.hold_shift(true);
    d.drag(grab, (-50.0, 30.0));
    d.hold_shift(false);
    let after = positions(&mut d, clip);
    assert_eq!(sel(&mut d), [2, 0]);
    let moved = |i: usize| (after[i].0 - before[i].0, after[i].1 - before[i].1);
    assert!(moved(0).0 < -20.0 && near(moved(0), moved(2)), "{before:?} → {after:?}");
    assert_eq!(after[1], before[1]);
}
