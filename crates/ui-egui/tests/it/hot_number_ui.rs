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
        self.prop(clip, "rotation")
    }
    fn prop(&self, clip: u64, name: &str) -> f64 {
        let s = &self.harness.state().session;
        let (_, it) = s.active_sequence().unwrap().find_item(ClipId(clip)).unwrap();
        let e = it.effects.iter().find(|e| e.effect == "graphic_text").unwrap();
        match e.param(name).unwrap().value_at(Tick::ZERO) {
            ParamValue::Float(v) => v,
            v => panic!("{name} is {v:?}"),
        }
    }
    fn playhead(&self) -> Tick {
        self.harness.state().session.playhead()
    }
    fn undo_len(&self) -> usize {
        self.harness.state().session.history.undo.len()
    }
    fn input(&mut self, events: impl IntoIterator<Item = egui::Event>) {
        self.harness.input_mut().events.extend(events);
        self.frames(1);
    }
    fn sent(&self, cmd: &egui::ViewportCommand) -> bool {
        self.harness.output().viewport_output.get(&egui::ViewportId::ROOT).is_some_and(|v| v.commands.contains(cmd))
    }
    fn moves_cursor_to(&self, to: egui::Pos2) -> bool {
        self.sent(&egui::ViewportCommand::CursorPosition(to))
    }
    fn grabs_cursor(&self, grab: egui::CursorGrab) -> bool {
        self.sent(&egui::ViewportCommand::CursorGrab(grab))
    }
    /// Presses at `at` and drags; whether the cursor got locked on the way. Once raw motion has
    /// arrived, the lock comes as the drag starts, before the next raw motion.
    fn drag_locks(&mut self, at: egui::Pos2, press: egui::Event) -> bool {
        let mut locked = false;
        for events in [
            vec![egui::Event::PointerMoved(at), press],
            vec![egui::Event::PointerMoved(at + egui::vec2(10.0, 0.0))],
            vec![egui::Event::MouseMoved(egui::vec2(100.0, 0.0))],
        ] {
            self.input(events);
            locked |= self.grabs_cursor(egui::CursorGrab::Locked);
        }
        locked
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

/// The cursor stops at the screen edge, the mouse does not: a dragged hot number follows the raw
/// motion with the cursor hidden and held where the press began, where it shows again on release (#4177).
#[test]
fn a_dragged_hot_number_keeps_changing_past_the_screen_edge() {
    let mut d = Driver::demo();
    let clip = d.text_layer();
    d.exec("graphics.set", json!({"clip": clip, "layer": 0, "props": {"rotation": 0.0}}));
    d.frames(2);
    let r = d.harness.state().auto.find("graphics.prop.rotation").expect("rotation field").rect;
    let at = egui::pos2(r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
    let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    d.input([egui::Event::PointerMoved(at), button(true)]);
    // past egui's drag threshold, then the pointer stays put as if held at the edge
    d.input([egui::Event::PointerMoved(at + egui::vec2(10.0, 0.0))]);
    for n in 0..4 {
        d.input([egui::Event::MouseMoved(egui::vec2(100.0, 0.0))]);
        assert_eq!(d.harness.output().platform_output.cursor_icon, egui::CursorIcon::None, "the cursor is hidden while dragging");
        assert!(d.moves_cursor_to(at), "the cursor is held where the press began");
        // Wayland moves only a locked cursor
        assert_eq!(d.grabs_cursor(egui::CursorGrab::Locked), n == 0, "the cursor is locked once, as the drag takes it");
    }
    d.input([egui::Event::PointerMoved(at), button(false)]);
    assert!(d.moves_cursor_to(at), "the cursor shows again where the press began");
    assert!(d.grabs_cursor(egui::CursorGrab::None), "and is let go");
    // 0.5° per point: 10 points of pointer travel alone would give 5°
    assert!(d.rotation(clip) >= 200.0, "the raw motion past the edge counts: {}", d.rotation(clip));
}

/// A field that goes away during the drag (its panel closed) does not keep the cursor locked: it is
/// let go when the button comes up.
#[test]
fn the_cursor_is_let_go_when_the_dragged_field_goes_away() {
    let mut d = Driver::demo();
    d.text_layer();
    let r = d.harness.state().auto.find("graphics.prop.rotation").expect("rotation field").rect;
    let at = egui::pos2(r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
    let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    d.input([egui::Event::PointerMoved(at), button(true)]);
    d.input([egui::Event::PointerMoved(at + egui::vec2(10.0, 0.0))]);
    d.input([egui::Event::MouseMoved(egui::vec2(100.0, 0.0))]);
    assert!(d.grabs_cursor(egui::CursorGrab::Locked));
    d.exec("timeline.select", json!({"clips": []}));
    d.frames(3);
    assert!(d.harness.state().auto.find("graphics.prop.rotation").is_none(), "the field is gone");
    assert!(!d.grabs_cursor(egui::CursorGrab::None), "the cursor stays locked while the button is down");
    d.input([button(false)]);
    assert!(d.grabs_cursor(egui::CursorGrab::None), "the cursor is let go");
    assert!(d.moves_cursor_to(at), "where the press began");
}

/// A button let go in another window (after Alt+Tab mid-drag) never reaches FilmCraft: losing focus
/// lets the cursor go and ends the drag, even once the window has focus again. A new press drags.
#[test]
fn the_cursor_is_let_go_when_the_window_loses_focus() {
    let mut d = Driver::demo();
    let clip = d.text_layer();
    d.exec("graphics.set", json!({"clip": clip, "layer": 0, "props": {"rotation": 0.0}}));
    d.frames(2);
    let r = d.harness.state().auto.find("graphics.prop.rotation").expect("rotation field").rect;
    let at = egui::pos2(r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
    let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    d.input([egui::Event::PointerMoved(at), button(true)]);
    d.input([egui::Event::PointerMoved(at + egui::vec2(10.0, 0.0))]);
    d.input([egui::Event::MouseMoved(egui::vec2(100.0, 0.0))]);
    assert!(d.grabs_cursor(egui::CursorGrab::Locked));
    d.background = true;
    d.input([egui::Event::WindowFocused(false)]);
    assert!(d.grabs_cursor(egui::CursorGrab::None), "the cursor is let go");
    assert!(d.moves_cursor_to(at), "where the press began");
    let rotation = d.rotation(clip);
    d.input([egui::Event::MouseMoved(egui::vec2(100.0, 0.0))]);
    assert!(!d.grabs_cursor(egui::CursorGrab::Locked), "nor taken again without focus");
    // back in FilmCraft with the button already up: egui still sees it held from the old press
    d.background = false;
    d.input([egui::Event::WindowFocused(true)]);
    for _ in 0..3 {
        d.input([egui::Event::MouseMoved(egui::vec2(100.0, 0.0))]);
        assert!(!d.grabs_cursor(egui::CursorGrab::Locked), "the old press does not take the cursor again");
        assert_ne!(d.harness.output().platform_output.cursor_icon, egui::CursorIcon::None, "nor hide it");
    }
    assert_eq!(d.rotation(clip), rotation, "nor move the value");
    // a new press drags as before
    d.input([button(false)]);
    assert!(d.drag_locks(at, button(true)), "a new drag takes the cursor");
    assert!(d.rotation(clip) > rotation, "and moves the value");
    d.input([egui::Event::PointerMoved(at), button(false)]);
    assert!(d.grabs_cursor(egui::CursorGrab::None));
}

/// The button let go outside FilmCraft, the next press comes with no release before it: a press on
/// another field drags that one, and the field dragged before focus went away stays put.
#[test]
fn a_press_on_another_field_after_focus_returns_drags_that_one() {
    let mut d = Driver::demo();
    let clip = d.text_layer();
    d.exec("graphics.set", json!({"clip": clip, "layer": 0, "props": {"rotation": 0.0, "scale": 100.0}}));
    d.frames(2);
    let center = |d: &Driver, id: &str| {
        let r = d.harness.state().auto.find(id).expect("field").rect;
        egui::pos2(r[0] + r[2] / 2.0, r[1] + r[3] / 2.0)
    };
    let (rotation_at, scale_at) = (center(&d, "graphics.prop.rotation"), center(&d, "graphics.prop.scale"));
    let press = |pos| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE };
    assert!(d.drag_locks(rotation_at, press(rotation_at)));
    d.background = true;
    d.input([egui::Event::WindowFocused(false)]);
    d.background = false;
    d.input([egui::Event::WindowFocused(true)]);
    let rotation = d.prop(clip, "rotation");
    assert!(d.drag_locks(scale_at, press(scale_at)), "the new press takes the cursor");
    assert!(d.prop(clip, "scale") > 100.0, "and moves the field it is on: {}", d.prop(clip, "scale"));
    assert_eq!(d.prop(clip, "rotation"), rotation, "not the one dragged before focus went away");
    let release = egui::Event::PointerButton { pos: scale_at, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE };
    d.input([egui::Event::PointerMoved(scale_at), release]);
    assert!(d.grabs_cursor(egui::CursorGrab::None));
}

/// Escape ends a drag with the button still down: the cursor is let go there and then.
#[test]
fn escape_lets_the_cursor_go_during_a_drag() {
    let mut d = Driver::demo();
    d.text_layer();
    let r = d.harness.state().auto.find("graphics.prop.rotation").expect("rotation field").rect;
    let at = egui::pos2(r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
    let press = egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE };
    assert!(d.drag_locks(at, press));
    let escape = egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE };
    d.input([escape]);
    assert!(d.grabs_cursor(egui::CursorGrab::None), "the cursor is let go");
    assert!(d.moves_cursor_to(at), "where the press began");
    d.input([egui::Event::MouseMoved(egui::vec2(100.0, 0.0))]);
    assert!(!d.grabs_cursor(egui::CursorGrab::Locked), "and not taken again while the button stays down");
}

/// A touch has no raw mouse motion: once a mouse sent some, a touch drag still follows the finger.
#[test]
fn a_touch_drag_follows_the_finger_after_raw_mouse_motion() {
    let mut d = Driver::demo();
    let clip = d.text_layer();
    d.exec("graphics.set", json!({"clip": clip, "layer": 0, "props": {"rotation": 0.0}}));
    d.input([egui::Event::MouseMoved(egui::vec2(1.0, 0.0))]);
    let r = d.harness.state().auto.find("graphics.prop.rotation").expect("rotation field").rect;
    let at = egui::pos2(r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
    let touch = |phase, pos| egui::Event::Touch { device_id: egui::TouchDeviceId(1), id: egui::TouchId(1), phase, pos, force: None };
    let button = |pressed, pos| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    d.input([touch(egui::TouchPhase::Start, at), egui::Event::PointerMoved(at), button(true, at)]);
    let mut to = at;
    for _ in 0..4 {
        to += egui::vec2(20.0, 0.0);
        d.input([touch(egui::TouchPhase::Move, to), egui::Event::PointerMoved(to)]);
        assert_ne!(d.harness.output().platform_output.cursor_icon, egui::CursorIcon::None, "a touch does not hide the cursor");
    }
    d.input([touch(egui::TouchPhase::End, to), button(false, to), egui::Event::PointerGone]);
    // 0.5° per point over 80 points, less what egui's drag threshold holds back
    assert!(d.rotation(clip) >= 30.0, "the touch drag moved the value: {}", d.rotation(clip));
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
