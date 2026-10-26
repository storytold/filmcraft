//! Control-only validation must not share keyboard ownership with native computer use.

use std::sync::{
    Arc,
    mpsc::{Sender, channel},
};

use egui::{Event, Id, Key, Modifiers, vec2};
use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::{
    FilmcraftApp,
    control::ControlRequest,
    dock::{DockNode, PanelKind, SplitSize},
};
use serde_json::{Value, json};

struct Driver {
    h: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    fn new(isolated: bool) -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).unwrap();
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session);
        let app = if isolated { app.with_control_input_only(rx) } else { app.with_control(rx) };
        let mut driver = Self { h: Harness::builder().with_size(vec2(1400.0, 900.0)).build_eframe(move |_| app), tx };
        driver.frames(4, true);
        driver
    }

    fn frames(&mut self, count: usize, native_focused: bool) {
        for _ in 0..count {
            let ctx = self.h.ctx.clone();
            let mut raw = std::mem::take(self.h.input_mut());
            raw.focused = native_focused;
            if let Some(viewport) = raw.viewports.get_mut(&egui::ViewportId::ROOT) {
                viewport.focused = Some(native_focused);
            }
            eframe::App::raw_input_hook(self.h.state_mut(), &ctx, &mut raw);
            *self.h.input_mut() = raw;
            self.h.step();
            assert!(self.h.state().ui_error.is_none());
        }
    }

    fn ok(&mut self, method: &str, params: Value, native_focused: bool) -> Value {
        let (request, reply) = ControlRequest::new(method, params);
        self.tx.send(request).unwrap();
        for _ in 0..60 {
            self.frames(1, native_focused);
            if let Ok(result) = reply.try_recv() {
                assert_eq!(result["ok"], true, "{result}");
                self.frames(2, native_focused);
                return result["result"].clone();
            }
        }
        panic!("no control reply to {method}");
    }
}

fn native_key(h: &mut Harness<'_, FilmcraftApp>, key: Key) {
    for pressed in [true, false] {
        // The backend supplies native events before raw_input_hook. Harness::event would
        // instead append them after that hook and bypass the input boundary under test.
        h.input_mut().events.push(Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE });
    }
}

#[test]
fn native_j_cannot_start_reverse_playback_during_control_only_editing() {
    let mut d = Driver::new(true);
    d.ok("engine.execute", json!({"command":"playhead.set","params":{"seconds":1}}), true);
    let before = d.ok("engine.execute", json!({"command":"sequence.inspect","params":{}}), true);
    let (request, reply) = ControlRequest::new("ui.key", json!({"key":"K","command":true}));
    d.tx.send(request).unwrap();
    d.frames(1, true); // queue the requested command; the native key arrives in its input frame
    native_key(&mut d.h, Key::J);
    d.frames(4, true);
    assert_eq!(reply.try_recv().unwrap()["ok"], true);
    assert!(!d.h.state().playback.playing, "native J must not contaminate the automated edit");
    let after = d.ok("engine.execute", json!({"command":"sequence.inspect","params":{}}), true);
    assert_eq!(after["playhead"], before["playhead"]);
    assert_eq!(after["video"][0]["items"].as_array().unwrap().len(), before["video"][0]["items"].as_array().unwrap().len() + 1);
}

#[test]
fn background_control_input_resizes_the_focused_divider_without_seeking() {
    background_divider(true);
}

#[test]
fn ordinary_control_input_also_resizes_a_background_divider() {
    background_divider(false);
}

fn background_divider(isolated: bool) {
    let mut d = Driver::new(isolated);
    d.h.state_mut().ui.dock = DockNode::Split {
        vertical: false,
        size: SplitSize::FixedA(400.0),
        a: Box::new(DockNode::Tabs { panels: vec![PanelKind::Project], active: 0 }),
        b: Box::new(DockNode::Tabs { panels: vec![PanelKind::Timeline], active: 0 }),
    };
    d.frames(3, true);
    let before_project = d.h.state().session.project.clone();
    let before_playheads = d.h.state().session.state.playheads.clone();
    let divider = Id::unique(("dock-gutter", String::new()));
    d.h.ctx.memory_mut(|memory| memory.request_focus(divider));
    d.frames(2, true);
    d.h.input_mut().events.push(Event::WindowFocused(false));
    d.ok("ui.key", json!({"key":"ArrowRight"}), false);
    assert!(matches!(d.h.state().ui.dock, DockNode::Split { size: SplitSize::FixedA(408.0), .. }), "control input must work without native activation");
    assert_eq!(d.h.state().session.state.playheads, before_playheads);
    assert!(Arc::ptr_eq(&d.h.state().session.project, &before_project));
    assert!(!d.h.state().playback.playing);
}

#[test]
fn interactive_control_keeps_native_keyboard_and_menu_input() {
    let mut d = Driver::new(false);
    native_key(&mut d.h, Key::J);
    d.frames(2, true);
    assert!(d.h.state().playback.playing);
    assert_eq!(d.h.state().playback.speed, -1.0);
    d.ok("ui.playback", json!({"action":"stop"}), true);
    let (tx, rx) = channel();
    d.h.state_mut().command_inbox = Some(rx);
    tx.send("playback.reverse".into()).unwrap();
    d.frames(2, true);
    assert!(d.h.state().playback.playing);
    assert_eq!(d.h.state().playback.speed, -1.0);
}

#[test]
fn native_menu_commands_cannot_contaminate_control_only_validation() {
    let mut d = Driver::new(true);
    let (tx, rx) = channel();
    d.h.state_mut().command_inbox = Some(rx);
    tx.send("playback.reverse".into()).unwrap();
    d.frames(2, true);
    assert!(!d.h.state().playback.playing);
    // Intentional playback remains reachable through the control command path.
    d.ok("engine.execute", json!({"command":"playback.reverse","params":{}}), false);
    assert!(d.h.state().playback.playing);
    assert_eq!(d.h.state().playback.speed, -1.0);
}

#[test]
fn isolation_preserves_close_and_screenshot_delivery_without_claiming_os_focus() {
    let mut d = Driver::new(true);
    let mut raw = egui::RawInput { focused: false, ..Default::default() };
    let viewport = raw.viewports.get_mut(&egui::ViewportId::ROOT).unwrap();
    viewport.focused = Some(false);
    viewport.events.push(egui::ViewportEvent::Close);
    raw.events = vec![Event::Text("unrelated typing".into()), Event::WindowFocused(false)];
    raw.hovered_files.push(Default::default());
    #[derive(Debug)]
    struct UnrelatedFile;
    impl egui::DroppedFile for UnrelatedFile {
        fn path(&self) -> &std::path::Path {
            std::path::Path::new("/unrelated-input.txt")
        }
        #[cfg(not(target_arch = "wasm32"))]
        fn bytes(&self) -> Result<Vec<u8>, String> {
            Err("must not read unrelated native input".into())
        }
        #[cfg(target_arch = "wasm32")]
        fn bytes_async(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<u8>, String>> + '_>> {
            Box::pin(async { Err("must not read unrelated native input".into()) })
        }
    }
    raw.dropped_files.push(Arc::new(UnrelatedFile));
    let ctx = d.h.ctx.clone();
    eframe::App::raw_input_hook(d.h.state_mut(), &ctx, &mut raw);
    assert!(raw.focused);
    assert_eq!(raw.viewports[&egui::ViewportId::ROOT].focused, Some(false));
    assert_eq!(raw.viewports[&egui::ViewportId::ROOT].events, vec![egui::ViewportEvent::Close]);
    assert!(
        matches!(raw.events.as_slice(), [Event::ModifiersChanged(Modifiers::NONE)]),
        "native input is dropped; screenshot results use callbacks, not raw events"
    );
    assert!(raw.hovered_files.is_empty() && raw.dropped_files.is_empty());
    let state = filmcraft_ui_egui::control::inspect(d.h.state(), &ctx);
    assert_eq!(state["input"]["nativeWindowFocused"], false);
}

#[test]
fn focus_reports_real_widget_ownership_and_rejects_bad_targets_without_changing_it() {
    let mut d = Driver::new(true);
    let focus = d.ok("ui.focus", json!({"id":"tools.Selection"}), false);
    assert_eq!(focus["focusRequested"], "tools.Selection");
    let state = d.ok("ui.inspect", json!({}), false);
    assert_eq!(state["keyboardFocus"]["widgetId"], focus["widgetId"]);
    assert_eq!(state["keyboardFocus"]["elementIds"], json!(["tools.Selection"]));
    assert_eq!(state["keyboardFocus"]["active"], true);
    assert_eq!(state["input"]["nativeWindowFocused"], false);
    assert_eq!(state["input"]["effectiveWindowFocused"], true);
    let focused = d.h.ctx.memory(|memory| memory.focused());
    for params in [
        json!({"id":7}),
        json!({"id":"missing"}),
        json!({"id":"panel.Tools"}),
        json!({"id":"tools.Rolling"}),
        json!({"id":"tools.Selection","activate":true}),
        json!([]),
    ] {
        let (request, _) = ControlRequest::new("ui.focus", params);
        let ctx = d.h.ctx.clone();
        let filmcraft_ui_egui::control::Outcome::Done(result) = filmcraft_ui_egui::control::handle(d.h.state_mut(), &ctx, &request) else {
            panic!("bad focus target must fail immediately");
        };
        assert_eq!(result["ok"], false, "{result}");
        assert_eq!(d.h.ctx.memory(|memory| memory.focused()), focused);
    }
    d.ok("ui.key", json!({"key":"Tab"}), false);
    let state = d.ok("ui.inspect", json!({}), false);
    assert_eq!(state["keyboardFocus"]["elementIds"], json!(["tools.TrackSelectForward"]));
    assert_ne!(state["keyboardFocus"]["widgetId"], focus["widgetId"]);
    assert!(!d.h.state().playback.playing);
}

#[test]
fn modified_control_click_adds_to_timeline_selection() {
    for isolated in [true, false] {
        let mut d = Driver::new(isolated);
        d.ok("ui.set", json!({"timeline":{"fit":true}}), false);
        d.frames(30, false);
        let seq = d.h.state().session.active_sequence().unwrap();
        let first = seq.video_tracks[0].items[0].id;
        let second = seq.video_tracks[0].items[1].id;
        let project = d.h.state().session.project.clone();
        d.ok("engine.execute", json!({"command":"timeline.select","params":{"clips":[first.0]}}), false);
        let position = d.ok("ui.timeline.locate", json!({"clip":second.0}), false);
        d.ok("ui.click", json!({"x":position["x"],"y":position["y"],"shift":true}), false);
        assert!(d.h.state().session.state.selection.contains(&first), "Shift-click must retain the first selected clip (isolated={isolated})");
        assert!(d.h.state().session.state.selection.contains(&second));
        assert!(Arc::ptr_eq(&d.h.state().session.project, &project));
        assert!(!d.h.state().playback.playing);
        assert_eq!(d.h.ctx.input(|input| input.modifiers), Modifiers::NONE);
    }
}

#[test]
fn modified_control_drag_keeps_shift_until_release_and_clears_it_afterward() {
    let mut d = Driver::new(true);
    let (request, reply) = ControlRequest::new(
        "ui.drag",
        json!({
            "from":{"id":"dock.gutter."},"to":{"id":"dock.gutter.","fy":3.0},"steps":4,"shift":true,
        }),
    );
    d.tx.send(request).unwrap();
    let mut held_frames = 0;
    for _ in 0..60 {
        d.frames(1, false);
        if d.h.ctx.input(|input| input.pointer.any_down() || input.pointer.any_released()) {
            held_frames += 1;
            assert!(d.h.ctx.input(|input| input.modifiers.shift), "the synthetic gesture owns Shift throughout press, movement and release");
        }
        if let Ok(result) = reply.try_recv() {
            assert_eq!(result["ok"], true);
            break;
        }
    }
    assert!(held_frames >= 4);
    d.frames(3, false);
    assert_eq!(d.h.ctx.input(|input| input.modifiers), Modifiers::NONE);
}

/// CodeRabbit on #2006: in control-only mode the native menu's commands are ignored, but its Quit
/// item (now `app.quit` through the command inbox, #1943) must still close the window, as the
/// predefined Quit item did.
#[test]
fn native_quit_still_quits_in_control_only_mode() {
    let mut d = Driver::new(true);
    let (tx, rx) = channel();
    d.h.state_mut().command_inbox = Some(rx);
    // another menu command is ignored
    let project = d.h.state().session.project.clone();
    tx.send("file.newBin".to_string()).unwrap();
    d.frames(2, true);
    assert!(Arc::ptr_eq(&project, &d.h.state().session.project), "menu commands do not run in control-only mode");
    tx.send("app.quit".to_string()).unwrap();
    d.frames(1, true);
    let closing = d.h.output().viewport_output.get(&egui::ViewportId::ROOT).is_some_and(|v| v.commands.contains(&egui::ViewportCommand::Close));
    assert!(closing, "Quit asks the window to close");
}
