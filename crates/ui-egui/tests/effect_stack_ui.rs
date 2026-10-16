//! Effect header editing through real pointer/keyboard events and the control channel.
use egui::{Event, Modifiers, PointerButton, Pos2, Rect, pos2, vec2};
use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_project::{ClipId, EffectInstance};
use filmcraft_ui_egui::{FilmcraftApp, control::ControlRequest};
use serde_json::{Value, json};
use std::sync::mpsc::{Sender, channel};

struct Driver {
    h: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    clip: u64,
}
impl Driver {
    fn new() -> Self {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let clip = s.active_sequence().unwrap().video_tracks[0].items[0].id.0;
        s.execute("timeline.select", json!({"clips":[clip],"linked":false})).unwrap();
        for effect in ["drop_shadow", "rounded_crop"] {
            s.execute("effects.apply", json!({"clips":[clip],"effect":effect})).unwrap();
        }
        let (tx, rx) = channel();
        let mut app = FilmcraftApp::new(s).with_control(rx);
        app.set_workspace("Effects");
        // Collapsed rows expose the whole stack and leave both drag targets visible.
        let it = app.session.active_sequence().unwrap().find_item(ClipId(clip)).unwrap().1;
        app.ui.collapsed_fx = (0..it.effects.len()).map(|i| format!("{clip}:{i}")).collect();
        let mut builder = Harness::builder().with_size(vec2(1600.0, 980.0)).with_max_steps(10_000);
        if std::env::var_os("FILMCRAFT_UI_SHOTS").is_some() {
            builder = builder.wgpu();
        }
        let h = builder.build_eframe(move |_| app);
        let mut d = Self { h, tx, clip };
        d.frames(5);
        d
    }
    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            let ctx = self.h.ctx.clone();
            let mut raw = std::mem::take(self.h.input_mut());
            eframe::App::raw_input_hook(self.h.state_mut(), &ctx, &mut raw);
            *self.h.input_mut() = raw;
            ctx.request_repaint();
            self.h.step();
        }
    }
    fn call(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                assert_eq!(v["ok"], true, "{method} {params}: {v}");
                return v["result"].clone();
            }
        }
        panic!("no reply to {method}")
    }
    fn exec(&mut self, id: &str, p: Value) -> Value {
        self.call("engine.execute", json!({"command":id,"params":p}))
    }
    fn rect(&self, id: &str) -> Rect {
        let e = self.h.state().auto.elements.iter().find(|e| e.id == id).unwrap_or_else(|| panic!("missing {id}"));
        let [x, y, w, h] = e.rect;
        Rect::from_min_size(pos2(x, y), vec2(w, h))
    }
    fn pointer(&mut self, pos: Pos2, pressed: Option<bool>) {
        self.h.input_mut().events.push(Event::PointerMoved(pos));
        if pressed == Some(true) {
            self.frames(1);
        }
        if let Some(pressed) = pressed {
            self.h.input_mut().events.push(Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
        }
        self.frames(2);
    }
    fn header(&mut self, name: &str) {
        let p = self.rect(&format!("effectControls.effect.{name}")).center();
        self.pointer(p, Some(true));
        self.pointer(p, Some(false));
    }
    fn key(&mut self, key: &str) {
        self.call("ui.key", json!({"key":key}));
        self.frames(3);
    }
    fn effects(&self, clip: u64) -> Vec<EffectInstance> {
        self.h.state().session.active_sequence().unwrap().find_item(ClipId(clip)).unwrap().1.effects.clone()
    }
    fn shot(&mut self, name: &str) {
        if let Some(dir) = std::env::var_os("FILMCRAFT_UI_SHOTS") {
            self.frames(4);
            let dir = std::path::PathBuf::from(dir);
            std::fs::create_dir_all(&dir).unwrap();
            self.h.render().unwrap().save(dir.join(format!("{name}.png"))).unwrap();
        }
    }
}

#[test]
fn header_delete_duplicate_and_copy_paste_to_another_clip() {
    let mut d = Driver::new();
    d.header("drop_shadow");
    d.key("Cmd+C");
    assert!(d.h.state().session.state.effect_clipboard.is_some());
    let before = d.effects(d.clip);
    d.key("Cmd+D");
    assert_eq!(d.effects(d.clip).len(), before.len() + 1);
    d.key("Delete");
    assert_eq!(d.effects(d.clip), before, "Delete removes the duplicate, not the clip");
    d.key("Cmd+Z");
    assert_eq!(d.effects(d.clip).len(), before.len() + 1);
    let other = d.h.state().session.active_sequence().unwrap().video_tracks[0].items[1].id.0;
    // Click a different timeline clip: stale effect selection must not determine the paste target.
    d.exec("timeline.select", json!({"clips":[other],"linked":false}));
    d.h.state_mut().ui.focused = filmcraft_ui_egui::dock::PanelKind::Timeline;
    let old = d.effects(other).len();
    d.key("Cmd+V");
    assert_eq!(d.effects(other).len(), old + 1);
    assert!(d.effects(other).iter().any(|e| e.effect == "drop_shadow"));
    d.key("Cmd+C");
    assert!(d.h.state().session.state.effect_clipboard.is_none(), "timeline Copy returns to clip clipboard");
}

#[test]
fn drag_changes_order_only_on_release_and_escape_cancels() {
    let mut d = Driver::new();
    let from = d.rect("effectControls.effect.drop_shadow").center();
    let r = d.rect("effectControls.effect.rounded_crop");
    let to = pos2(r.center().x, r.bottom() - 2.0);
    let before = d.effects(d.clip);
    let undo = d.h.state().session.history.undo.len();
    d.pointer(from, Some(true));
    d.pointer(from + vec2(0.0, 8.0), None);
    d.pointer(to, None);
    assert_eq!(d.effects(d.clip), before);
    d.shot("effect-stack-drag");
    d.pointer(to, Some(false));
    let after = d.effects(d.clip);
    assert!(after.iter().position(|e| e.effect == "rounded_crop") < after.iter().position(|e| e.effect == "drop_shadow"));
    assert_eq!(d.h.state().session.history.undo.len(), undo + 1);
    d.key("Cmd+Z");
    assert_eq!(d.effects(d.clip), before);
    // Collapse again after structural editing invalidated index-based UI state.
    d.h.state_mut().ui.collapsed_fx = (0..before.len()).map(|i| format!("{}:{i}", d.clip)).collect();
    d.frames(3);
    let from = d.rect("effectControls.effect.drop_shadow").center();
    let to = d.rect("effectControls.effect.rounded_crop").center();
    d.pointer(from, Some(true));
    d.pointer(from + vec2(0.0, 8.0), None);
    d.pointer(to, None);
    d.key("Escape");
    d.pointer(to, Some(false));
    assert_eq!(d.effects(d.clip), before);
}

#[test]
fn delete_protects_intrinsics_and_never_falls_through_to_clip_deletion() {
    let mut d = Driver::new();
    let before = d.effects(d.clip);
    d.header("motion");
    d.key("Delete");
    assert_eq!(d.effects(d.clip), before);
    d.header("drop_shadow");
    d.key("Backspace");
    assert_eq!(d.effects(d.clip).len(), before.len() - 1);
    d.key("Delete"); // no effect selected after removal
    assert_eq!(d.effects(d.clip).len(), before.len() - 1);
}
