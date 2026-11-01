//! Hot numbers (the blue values in Properties, Effect Controls, the Audio Clip Mixer and Lumetri),
//! driven headless through the control channel: once a click turns one into a text field, Left and
//! Right move the caret inside it (#3372) instead of moving focus out, which committed the value.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_project::{ClipId, ParamValue};
use filmcraft_time::Tick;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    /// The window is in the background: egui sees it focused only on frames carrying control input.
    background: bool,
}

impl Driver {
    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, background: false };
        d.frames(4);
        d
    }
    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            raw.focused = !self.background;
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
    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }
    /// A new text layer, selected and showing in Properties; its clip.
    fn text_layer(&mut self) -> u64 {
        let clip = self.exec("graphics.newText", json!({"text": "Hi"}))["clip"].as_u64().expect("graphic clip");
        self.exec("timeline.select", json!({"clips": [clip]}));
        self.ok("ui.panel.show", json!({"panel": "Properties"}));
        self.frames(4);
        clip
    }
    fn rotation(&self, clip: u64) -> f64 {
        let s = &self.harness.state().session;
        let (_, it) = s.active_sequence().unwrap().find_item(ClipId(clip)).unwrap();
        let e = it.effects.iter().find(|e| e.effect == "graphic_text").unwrap();
        match e.param("rotation").unwrap().value_at(Tick::ZERO) {
            ParamValue::Float(v) => v,
            v => panic!("rotation is {v:?}"),
        }
    }
    fn playhead(&self) -> Tick {
        self.harness.state().session.playhead()
    }
    fn undo_len(&self) -> usize {
        self.harness.state().session.history.undo.len()
    }
}

#[test]
fn arrow_keys_move_the_caret_instead_of_committing() {
    let mut d = Driver::demo();
    let clip = d.text_layer();
    let field = "graphics.prop.rotation";
    let set = |d: &mut Driver| {
        d.exec("graphics.set", json!({"clip": clip, "layer": 0, "props": {"rotation": 12.5}}));
        d.frames(2);
        assert_eq!(d.rotation(clip), 12.5);
    };
    // Left on the selected "12.5" lands before its last digit, the next Left before the point:
    // Backspace then removes the "2"
    set(&mut d);
    let (playhead, undo) = (d.playhead(), d.undo_len());
    d.ok("ui.click", json!({"id": field}));
    d.ok("ui.key", json!({"key": "Left"}));
    assert_eq!(d.playhead(), playhead, "Left does not step the playhead");
    assert_eq!(d.undo_len(), undo, "Left does not commit the value");
    d.ok("ui.key", json!({"key": "Left"}));
    assert_eq!(d.undo_len(), undo, "nor does a second Left");
    d.ok("ui.key", json!({"key": "Backspace"}));
    d.ok("ui.type", json!({"text": "7"}));
    d.ok("ui.key", json!({"key": "Enter"}));
    assert_eq!(d.rotation(clip), 17.5, "12.5 with its middle digit replaced");
    assert_eq!(d.playhead(), playhead);
    // Right goes to the end: Backspace removes the last digit, then a new one is typed
    set(&mut d);
    let undo = d.undo_len();
    d.ok("ui.click", json!({"id": field}));
    d.ok("ui.key", json!({"key": "Right"}));
    assert_eq!(d.playhead(), playhead, "Right does not step the playhead");
    assert_eq!(d.undo_len(), undo, "Right does not commit the value");
    d.ok("ui.key", json!({"key": "Backspace"}));
    d.ok("ui.type", json!({"text": "9"}));
    d.ok("ui.key", json!({"key": "Enter"}));
    assert_eq!(d.rotation(clip), 12.9, "12.5 with its last digit replaced");
    assert_eq!(d.playhead(), playhead);
}

/// Control input reaches a window in the background: egui sees it focused only on the frames that
/// carry the input, and the frames between must not take the field's focus again.
#[test]
fn arrow_keys_move_the_caret_in_a_background_window() {
    let mut d = Driver::demo();
    let clip = d.text_layer();
    d.exec("graphics.set", json!({"clip": clip, "layer": 0, "props": {"rotation": 12.5}}));
    d.background = true;
    let undo = d.undo_len();
    for (method, params) in [
        ("ui.click", json!({"id": "graphics.prop.rotation"})),
        ("ui.key", json!({"key": "Left"})),
        ("ui.key", json!({"key": "Left"})),
        ("ui.key", json!({"key": "Backspace"})),
        ("ui.type", json!({"text": "7"})),
    ] {
        d.ok(method, params.clone());
        d.frames(4);
        assert_eq!(d.undo_len(), undo, "nothing is committed before Enter ({method} {params})");
    }
    d.ok("ui.key", json!({"key": "Enter"}));
    assert_eq!(d.rotation(clip), 17.5, "12.5 with its middle digit replaced");
    // the click still selects the whole value, so typing replaces it
    d.ok("ui.click", json!({"id": "graphics.prop.rotation"}));
    d.frames(4);
    d.ok("ui.type", json!({"text": "7"}));
    d.ok("ui.key", json!({"key": "Enter"}));
    assert_eq!(d.rotation(clip), 7.0, "the typed value replaced 17.5");
}

/// The plain text fields in Properties already kept the arrow keys: the layer's Source Text.
#[test]
fn arrow_keys_move_the_caret_in_the_source_text() {
    let mut d = Driver::demo();
    let clip = d.text_layer();
    let text = |d: &Driver| {
        let s = &d.harness.state().session;
        let (_, it) = s.active_sequence().unwrap().find_item(ClipId(clip)).unwrap();
        let e = it.effects.iter().find(|e| e.effect == "graphic_text").unwrap();
        match e.param("text").unwrap().value_at(Tick::ZERO) {
            ParamValue::Text(t) => t,
            v => panic!("text is {v:?}"),
        }
    };
    d.ok("ui.set", json!({"focused": "Properties"}));
    d.ok("ui.menu.invoke", json!({"id": "window.maximizeFrame"}));
    d.frames(4);
    let playhead = d.playhead();
    d.ok("ui.click", json!({"id": "graphics.sourceText"}));
    d.ok("ui.key", json!({"key": "Cmd+A"}));
    d.ok("ui.type", json!({"text": "abc"}));
    d.ok("ui.key", json!({"key": "Left"}));
    d.ok("ui.key", json!({"key": "Left"}));
    d.ok("ui.key", json!({"key": "Right"}));
    d.ok("ui.type", json!({"text": "X"}));
    assert_eq!(text(&d), "abXc");
    assert_eq!(d.playhead(), playhead, "the arrows do not step the playhead");
}
