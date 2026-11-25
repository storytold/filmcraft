//! Option-drag of a caption in the Timeline leaves it where it was and drops a copy where it is
//! released, as with clips. The real `FilmcraftApp` under `egui_kittest` with the demo project.

use crate::support::{Drive, Launch, driver};
use egui::{Modifiers, Pos2, Rect, pos2};
use serde_json::json;

driver!(pub(crate) Driver);

impl Driver {
    pub(crate) fn rect(&mut self, id: &str) -> Option<Rect> {
        self.frames(2);
        let els = self.ok("ui.elements", json!({"prefix": id}));
        let el = els.as_array()?.iter().find(|e| e["id"] == id)?.clone();
        let r: Vec<f32> = el["rect"].as_array()?.iter().filter_map(|v| v.as_f64().map(|v| v as f32)).collect();
        Some(Rect::from_min_size(pos2(*r.first()?, *r.get(1)?), egui::vec2(*r.get(2)?, *r.get(3)?)))
    }

    /// (start, end, text) of every caption on C1.
    pub(crate) fn captions(&mut self) -> Vec<(i64, i64, String)> {
        let r = self.exec("captions.list", json!({}));
        r["tracks"][0]["captions"]
            .as_array()
            .expect("captions")
            .iter()
            .map(|c| (c["start"].as_i64().expect("start"), c["end"].as_i64().expect("end"), c["text"].as_str().expect("text").to_string()))
            .collect()
    }

    /// Press at `from`, move to `to` over `steps` frames with `mods` held, release.
    pub(crate) fn drag_with(&mut self, from: Pos2, to: Pos2, steps: usize, mods: Modifiers) {
        let push = |d: &mut Self, e: egui::Event| d.harness.input_mut().events.push(e);
        push(self, egui::Event::ModifiersChanged(mods));
        push(self, egui::Event::PointerMoved(from));
        self.frames(1);
        push(self, egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: mods });
        self.frames(1);
        for i in 1..=steps {
            push(self, egui::Event::PointerMoved(from + (to - from) * (i as f32 / steps as f32)));
            self.frames(1);
        }
        push(self, egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: mods });
        self.frames(2);
        push(self, egui::Event::ModifiersChanged(Modifiers::NONE));
        self.frames(1);
    }
}

/// Option-drag of a caption leaves it where it was and drops a copy where it is released; a
/// plain drag still moves it.
#[test]
fn option_drag_copies_a_caption() {
    let mut d = Driver::demo_snap_off();
    let id = d.exec("captions.add", json!({"seconds": 1.0, "durationSeconds": 1.0, "text": "twin"}))["caption"].as_u64().expect("id");
    let before = d.captions();
    let r = d.rect(&format!("timeline.caption.{id}")).expect("caption block");
    d.drag_with(r.center(), r.center() + egui::vec2(r.width() * 2.0, 0.0), 20, Modifiers::ALT);
    let caps = d.captions();
    assert_eq!(caps.len(), 2, "a copy was made: {caps:?}");
    assert_eq!(caps[0], before[0], "the original stayed");
    assert_eq!(caps[1].2, "twin");
    assert!(caps[1].0 > before[0].1, "the copy is to the right: {caps:?}");
    // without Option it moves
    let r = d.rect(&format!("timeline.caption.{id}")).expect("caption block");
    d.drag_with(r.center(), r.center() - egui::vec2(r.width() * 0.5, 0.0), 20, Modifiers::NONE);
    assert_eq!(d.captions().len(), 2, "a plain drag does not copy");
}
