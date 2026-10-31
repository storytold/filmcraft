//! Panels floated into their own OS windows, with a stand-in for the OS: each native viewport
//! runs its own frame at the desktop place craft-ui asks for, with the main window's content at
//! the desktop origin.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

/// What the tests give the native windows and what the windows publish. A test drives at most
/// one native window at a time.
#[derive(Default)]
struct WindowIo {
    focused: bool,
    events: Vec<egui::Event>,
    ime: Option<egui::output::IMEOutput>,
    reset_requests: Vec<egui::ImeResetId>,
}

thread_local! {
    static WINDOW: std::cell::RefCell<WindowIo> = std::cell::RefCell::new(WindowIo::default());
}

/// Native windows at their builders' places, reporting that place as their desktop rect.
fn native_host() {
    WINDOW.with(|w| *w.borrow_mut() = WindowIo::default());
    egui::Context::set_immediate_viewport_renderer(|ctx, mut viewport| {
        let size = viewport.builder.inner_size.unwrap_or(egui::vec2(400.0, 300.0));
        let place = egui::Rect::from_min_size(viewport.builder.position.unwrap_or_default(), size);
        let id = viewport.ids.this;
        let (focused, events) = WINDOW.with(|w| {
            let mut w = w.borrow_mut();
            (w.focused, std::mem::take(&mut w.events))
        });
        let info = egui::ViewportInfo { inner_rect: Some(place), outer_rect: Some(place), focused: Some(focused), ..Default::default() };
        let input = egui::RawInput {
            viewport_id: id,
            viewports: std::iter::once((id, info)).collect(),
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            time: ctx.input(|i| Some(i.time)),
            focused,
            events,
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| (viewport.viewport_ui_cb)(ui));
        out.textures_delta.clear();
        WINDOW.with(|w| {
            let mut w = w.borrow_mut();
            w.reset_requests.extend(out.platform_output.ime.as_ref().and_then(|ime| ime.reset_request));
            w.ime = out.platform_output.ime;
        });
    });
}

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    /// The demo project with a new sequence open and `source` in the Source Monitor.
    fn new(source: u64) -> Self {
        native_host();
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        s.execute("file.newSequence", json!({"name": "Native", "width": 1280, "height": 720, "fps": 24})).unwrap();
        s.prefs.timeline.clip_mismatch_warning = false;
        s.execute("source.open", json!({"item": source})).unwrap();
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(s).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |cc| {
            filmcraft_ui_egui::install_ime_reset_callbacks(&cc.egui_ctx);
            app
        });
        let mut d = Driver { harness, tx };
        d.harness.ctx.set_embed_viewports(false);
        // A main window that reports its desktop place, as under X11: craft-ui otherwise keeps
        // floating windows in-window.
        let main = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 980.0));
        let info = d.harness.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default();
        (info.inner_rect, info.outer_rect, info.focused) = (Some(main), Some(main), Some(true));
        d.frames(4);
        d
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            ctx.request_repaint();
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

    /// Float `panel` into its own OS window at `rect` on the desktop.
    fn float(&mut self, panel: &str, rect: [f32; 4]) {
        self.ok("ui.menu.invoke", json!({"id": "window.floatPanel", "params": {"panel": panel, "rect": rect}}));
        self.frames(4);
    }

    /// Every element whose id starts with `prefix`, as `ui.elements` reports it.
    fn elements(&mut self, prefix: &str) -> Vec<Value> {
        self.ok("ui.elements", json!({"prefix": prefix})).as_array().cloned().unwrap_or_default()
    }

    fn element(&mut self, id: &str) -> Value {
        self.elements(id).into_iter().find(|e| e["id"] == id).unwrap_or_else(|| panic!("no element {id}"))
    }

    fn app(&self) -> &FilmcraftApp {
        self.harness.state()
    }

    fn pointer(&mut self, pos: egui::Pos2, pressed: Option<bool>) {
        self.harness.input_mut().events.push(egui::Event::PointerMoved(pos));
        if pressed == Some(true) {
            self.frames(1);
        }
        if let Some(pressed) = pressed {
            self.harness.input_mut().events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
        }
        self.frames(1);
    }
}

fn rect(element: &Value) -> egui::Rect {
    let r: Vec<f32> = element["rect"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap() as f32).collect();
    egui::Rect::from_min_size(egui::pos2(r[0], r[1]), egui::vec2(r[2], r[3]))
}

/// The header of a panel in its own OS window (the ≡ menu, the Timeline's sequence tabs) is
/// drawn before its body, and carries the window from the frame it is drawn in.
#[test]
fn a_native_windows_header_elements_carry_its_window() {
    let mut d = Driver::new(5);
    d.float("Timeline", [900.0, 120.0, 680.0, 420.0]);
    let body = d.element("panel.Timeline");
    assert!(body["window"].is_u64(), "the floated Timeline is in its own OS window: {body}");
    let menu = d.element("panel.menu.Timeline");
    assert_eq!(menu["window"], body["window"], "{menu}");
    let tabs = d.elements("timeline.tab.");
    assert!(!tabs.is_empty(), "the open sequence has a tab");
    for tab in tabs {
        assert_eq!(tab["window"], body["window"], "{tab}");
    }
}

/// The ≡ menu of a panel in its own OS window opens in that window, not in the main one (#3633).
#[test]
fn a_native_windows_panel_menu_opens_in_its_window() {
    let mut d = Driver::new(5);
    d.float("Timeline", [900.0, 120.0, 680.0, 420.0]);
    let body = d.element("panel.Timeline");
    let at = rect(&d.element("panel.menu.Timeline")).center();
    let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    WINDOW.with(|w| w.borrow_mut().focused = true);
    window_frame(&mut d, vec![egui::Event::PointerMoved(at)]);
    window_frame(&mut d, vec![button(true)]);
    window_frame(&mut d, vec![button(false)]);
    d.frames(3);
    let items = d.elements("panel.menu.Timeline.undock");
    assert_eq!(items.len(), 1, "the menu is open: {items:?}");
    assert_eq!(items[0]["window"], body["window"], "the menu is in the floating window: {}", items[0]);
}

/// A floating panel's menu stays inside its window when that is narrower than the menu, and a click in
/// another window dismisses it (#3633 review).
#[test]
fn a_floating_panel_menu_fits_its_window_and_closes_on_a_click_elsewhere() {
    let mut d = Driver::new(5);
    d.float("Timeline", [900.0, 120.0, 160.0, 420.0]);
    let at = rect(&d.element("panel.menu.Timeline")).center();
    let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    WINDOW.with(|w| w.borrow_mut().focused = true);
    window_frame(&mut d, vec![egui::Event::PointerMoved(at)]);
    window_frame(&mut d, vec![button(true)]);
    window_frame(&mut d, vec![button(false)]);
    d.frames(3);
    let scroll = d.elements("panel.menu.scroll");
    assert_eq!(scroll.len(), 1, "the menu is open: {scroll:?}");
    let r = rect(&scroll[0]);
    assert!(r.max.x <= 160.0 + 0.5, "the menu fits the 160 pt window: {r:?}");
    // a click in the main window
    let main = egui::pos2(60.0, 600.0);
    d.pointer(main, Some(true));
    d.pointer(main, Some(false));
    d.frames(3);
    assert!(d.elements("panel.menu.Timeline.undock").is_empty(), "a click in another window closes the menu");
}

/// A main-window panel's ≡ menu opens in the main window while another panel is in its own OS window (#3633).
#[test]
fn a_main_window_panel_menu_stays_in_the_main_window() {
    let mut d = Driver::new(5);
    d.float("Timeline", [900.0, 120.0, 680.0, 420.0]);
    d.ok("ui.click", json!({"id": "panel.menu.Program"}));
    d.frames(3);
    let items = d.elements("panel.menu.Program.undock");
    assert_eq!(items.len(), 1, "the menu is open: {items:?}");
    assert!(items[0]["window"].is_null(), "the menu is in the main window: {}", items[0]);
}

/// The Save changes? prompt of Quit blocks the panels in native windows as Settings does, so
/// they don't take the keyboard from it.
#[test]
fn the_quit_prompt_blocks_native_windows() {
    let mut d = Driver::new(5);
    d.float("Timeline", [900.0, 120.0, 680.0, 420.0]);
    assert_eq!(d.element("panel.menu.Timeline")["enabled"], json!(true));
    d.ok("engine.execute", json!({"command": "sequence.addTracks", "params": {}}));
    assert!(d.app().session.is_dirty());
    d.harness.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default().events.push(egui::ViewportEvent::Close);
    d.frames(3);
    assert!(d.elements("quit.").iter().any(|e| e["id"] == "quit.cancel"), "Quit asks first");
    assert_eq!(d.element("panel.menu.Timeline")["enabled"], json!(false), "the native window is blocked");
}

/// A Source Monitor drag in the main window reaches the Timeline in its own OS window, and drops
/// the clip there.
#[test]
fn a_source_drag_drops_on_a_timeline_in_its_own_window() {
    let mut d = Driver::new(5);
    let place = egui::pos2(900.0, 120.0);
    d.float("Timeline", [place.x, place.y, 680.0, 420.0]);
    let clips = |d: &Driver| d.app().session.active_sequence().map_or(0, |q| q.video_tracks.iter().map(|t| t.items.len()).sum::<usize>());
    assert_eq!(clips(&d), 0);
    let row = d.element("timeline.track.V1.target");
    assert!(row["window"].is_u64(), "{row}");
    let panel = rect(&d.element("panel.Timeline"));
    // In the native window's coordinates, then on the desktop, where the main window's
    // content starts at the origin.
    let local = egui::pos2(panel.min.x + d.app().ui.timeline.header_w + 60.0, rect(&row).center().y);
    let to = place + local.to_vec2();
    let from = rect(&d.element("source.drag.video")).center();
    d.pointer(from, Some(true));
    d.pointer(from + egui::vec2(0.0, 9.0), None);
    d.pointer(to, None);
    d.pointer(to, None);
    d.pointer(to, Some(false));
    d.frames(3);
    assert_eq!(clips(&d), 1, "status: {}", d.app().ui.status);
}

/// The window's events for its next frame, and that frame.
fn window_frame(d: &mut Driver, events: Vec<egui::Event>) {
    WINDOW.with(|w| w.borrow_mut().events.extend(events));
    d.frames(1);
}

fn window_ime() -> Option<egui::output::IMEOutput> {
    WINDOW.with(|w| w.borrow().ime.clone())
}

fn ime_preedit(text: &str) -> egui::Event {
    egui::Event::Ime(egui::ImeEvent::Preedit { text: text.into(), active_range_chars: None })
}

fn ime_commit(text: &str) -> egui::Event {
    egui::Event::Ime(egui::ImeEvent::Commit(text.into()))
}

/// The Program monitor's text editor in its own OS window: the window publishes the composer's
/// descriptor every pass, so the marked text survives; a click while composing suspends the
/// window's reset fence, the window's next pass rearms it, and its completion admits a commit.
#[test]
fn a_floating_program_monitor_composes_and_rearms_its_ime_fence() {
    let mut d = Driver::new(5);
    let clip = d.ok("engine.execute", json!({"command": "graphics.newText", "params": {"text": "alpha", "position": [640, 360], "size": 120}}))["clip"]
        .as_u64()
        .unwrap();
    d.float("Program", [900.0, 120.0, 680.0, 520.0]);
    WINDOW.with(|w| w.borrow_mut().focused = true);
    d.ok("ui.set", json!({"gfxEdit": {"clip": clip, "layer": 0, "caret": 5, "anchor": 5}}));
    d.frames(3);
    let text = |d: &mut Driver| d.ok("engine.execute", json!({"command": "graphics.list", "params": {"clip": clip}}))["layers"][0]["text"].clone();
    let first = window_ime().expect("the floating editor publishes in its own window");

    window_frame(&mut d, vec![ime_preedit("に")]);
    for _ in 0..3 {
        d.frames(1);
        let ime = window_ime().expect("the window keeps publishing while composing");
        assert_eq!((ime.owner, ime.context_generation), (first.owner, first.context_generation), "the same IME context");
        assert!(!ime.should_interrupt_composition);
    }
    let preedit = d.element("program.textEdit.preedit");
    assert_eq!(preedit["label"], json!("に"), "the marked text is drawn in the window");
    assert!(preedit["window"].is_u64(), "{preedit}");
    window_frame(&mut d, vec![ime_preedit(""), ime_commit("日本")]);
    assert_eq!(text(&mut d), json!("alpha日本"));

    // A click in the text while composing suspends the window's fence.
    let at = rect(&d.element("program.textEdit")).center();
    window_frame(&mut d, vec![ime_preedit("ご")]);
    let press = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    WINDOW.with(|w| w.borrow_mut().reset_requests.clear());
    window_frame(&mut d, vec![egui::Event::PointerMoved(at), press(true)]);
    window_frame(&mut d, vec![press(false)]);
    d.frames(2);
    let fence = WINDOW.with(|w| w.borrow().reset_requests.last().copied()).expect("the window's next passes rearm its fence");
    window_frame(&mut d, vec![ime_commit("x")]);
    assert_eq!(text(&mut d), json!("alpha日本ご"), "the fence blocks a commit until it completes");
    window_frame(&mut d, vec![egui::Event::ImeResetCompleted { request_id: fence }, ime_preedit("が"), ime_commit("が")]);
    let after = text(&mut d);
    assert!(after.as_str().is_some_and(|t| t.contains('が')), "the completed fence admits the commit: {after}");
}
