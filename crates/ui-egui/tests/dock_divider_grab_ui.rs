//! Panel dividers are easy to grab beside a scrolled panel's scroll bar (#2690).
//!
//! The visible gap between docked panels is 4 pt. A `ScrollArea`'s scroll bar senses the pointer
//! across a 10 pt strip along the panel's edge, invisible until hovered, and it used to win the
//! pointer everywhere outside the bare gap. Panel content now steps out of the divider's grab band,
//! so pressing a few points beside the gap resizes the split while the scroll bar keeps working.

use std::sync::mpsc::{Sender, channel};

use egui::{CursorIcon, Event, Modifiers, PointerButton, Pos2, Rect, pos2, vec2};
use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use filmcraft_ui_egui::dock::{DIVIDER_HIT_MARGIN, DockNode, PanelKind, SplitSize};
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
            assert!(self.harness.state().ui_error.is_none(), "the app caught a UI panic: {:?}", self.harness.state().ui_error);
        }
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).expect("control channel");
        for _ in 0..600 {
            self.frames(1);
            if let Ok(r) = reply.try_recv() {
                assert_eq!(r["ok"], true, "{method} {params}: {r}");
                return r["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn rect(&self, id: &str) -> Rect {
        let element = self.harness.state().auto.find(id).unwrap_or_else(|| panic!("{id} is registered"));
        Rect::from_min_size(pos2(element.rect[0], element.rect[1]), vec2(element.rect[2], element.rect[3]))
    }

    fn top(&mut self, id: &str) -> f32 {
        self.frames(2);
        self.rect(id).min.y
    }

    /// The first effect row in the Effect Controls list, to watch the list scroll.
    fn first_effect_row(&self) -> String {
        let rows = self.harness.state().auto.query("effectControls.effect.");
        rows.iter().map(|e| e.id.clone()).find(|id| id.split('.').count() == 3).expect("an effect row is laid out")
    }

    fn first_size(&self) -> f32 {
        match self.harness.state().ui.dock {
            DockNode::Split { size: SplitSize::FixedA(value), .. } => value,
            ref other => panic!("fixed-size mode changed: {other:?}"),
        }
    }

    /// Press at `from`, move down/right in steps by `delta`, release.
    fn drag(&mut self, from: Pos2, delta: egui::Vec2) {
        self.harness.event(Event::PointerMoved(from));
        self.frames(2);
        self.harness.event(Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
        self.frames(1);
        for step in 1..=4 {
            self.harness.event(Event::PointerMoved(from + delta * (step as f32 / 4.0)));
            self.frames(1);
        }
        self.harness.event(Event::PointerButton { pos: from + delta, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
        self.frames(3);
    }
}

/// Effect Controls (a clip selected, list scrolled) | Program, split by a vertical divider at x = 420.
fn effect_controls_beside_program() -> Driver {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).expect("demo project");
    let (tx, rx) = channel();
    let app = FilmcraftApp::new(session).with_control(rx);
    let harness = Harness::builder().with_size(egui::vec2(1400.0, 420.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
    let mut d = Driver { harness, tx };
    d.frames(4);
    let seq = d.ok("engine.execute", json!({"command": "sequence.inspect", "params": {}}));
    let clip = seq["video"][0]["items"][0]["clip"].as_u64().expect("first video clip");
    d.ok("engine.execute", json!({"command": "timeline.select", "params": {"clips": [clip]}}));
    d.harness.state_mut().ui.dock = DockNode::Split {
        vertical: false,
        size: SplitSize::FixedA(420.0),
        a: Box::new(DockNode::Tabs { panels: vec![PanelKind::EffectControls], active: 0 }),
        b: Box::new(DockNode::Tabs { panels: vec![PanelKind::Program], active: 0 }),
    };
    d.frames(4);
    // Scroll partway, so the panel has a live scroll bar beside the divider.
    d.ok("ui.scroll", json!({"id": "panel.EffectControls", "dy": -40.0}));
    d.frames(3);
    d
}

#[test]
fn divider_grabs_a_few_points_beside_a_scrolled_panels_scroll_bar() {
    let mut d = effect_controls_beside_program();
    // The registered rect is the whole grab band: the 4 pt gap plus the margin on either side.
    let band = d.rect("dock.gutter.");
    assert!(band.width() >= 4.0 + 2.0 * DIVIDER_HIT_MARGIN - 0.01, "the band is forgiving: {band:?}");
    let content = d.rect("panel.EffectControls");
    // The panel content stops short of the grab band, so its scroll bar cannot cover it.
    assert!(content.max.x <= band.min.x + 0.01, "content {content:?} overlaps the band {band:?}");
    let row = d.first_effect_row();
    let before_scroll = d.top(&row);
    // 3.5 pt left of the visible gap, inside the panel frame: before #2690 this belonged to the panel.
    let gap = band.shrink2(vec2(DIVIDER_HIT_MARGIN, 0.0));
    let grab = pos2(gap.min.x - 3.5, content.center().y);
    d.harness.event(Event::PointerMoved(grab));
    d.frames(2);
    assert_eq!(d.harness.output().platform_output.cursor_icon, CursorIcon::ResizeHorizontal, "hovering the band shows the resize cursor");
    d.drag(grab, vec2(20.0, 0.0));
    assert_eq!(d.first_size(), 440.0, "the divider moved with the pointer");
    assert_eq!(d.top(&row), before_scroll, "the panel did not scroll");

    // The other side of the gap, inside the Program monitor, grabs too.
    let band = d.rect("dock.gutter.");
    let gap = band.shrink2(vec2(DIVIDER_HIT_MARGIN, 0.0));
    d.drag(pos2(gap.max.x + 3.5, content.center().y), vec2(-30.0, 0.0));
    assert_eq!(d.first_size(), 410.0);
}

#[test]
fn scroll_bar_beside_a_divider_still_scrolls() {
    let mut d = effect_controls_beside_program();
    let row = d.first_effect_row();
    let before = d.top(&row);
    let content = d.rect("panel.EffectControls");
    // On the scroll bar itself: its 10 pt strip ends 6 pt inside the panel's edge, clear of the band.
    let bar = pos2(content.max.x - 10.0, content.min.y + 60.0);
    d.harness.event(Event::PointerMoved(bar));
    d.frames(2);
    assert_ne!(d.harness.output().platform_output.cursor_icon, CursorIcon::ResizeHorizontal, "the scroll bar is not part of the divider");
    d.drag(bar, vec2(0.0, 120.0));
    let after = d.top(&row);
    assert!(after < before - 20.0, "dragging the scroll bar scrolls the panel: {before} -> {after}");
    assert_eq!(d.first_size(), 420.0, "the split did not move");
}
