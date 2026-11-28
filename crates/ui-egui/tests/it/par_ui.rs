//! Monitors show pictures at their display aspect: a 1440 x 1080 clip with 4:3 pixels (here a
//! 144 x 108 Color Matte given 4:3 pixels) is drawn 16:9 in the Source monitor, and so is the
//! sequence New Sequence From Clip makes of it in the Program monitor. Square pixels stay 4:3.
//! Driven headless through the control channel.
//!
//! With `FILMCRAFT_UI_SHOTS=<dir>` the test also renders the UI with wgpu and writes `par-*.png`.

use std::sync::Arc;

use crate::support::{Drive, Launch, builder, driver, shots_dir, with_shots};
use filmcraft_engine::Session;
use filmcraft_engine::project::{ItemId, ItemKind};
use serde_json::{Value, json};

driver!(Driver);

impl Driver {
    fn new(session: Session) -> Self {
        Self::open(session, with_shots(builder()))
    }
    fn exec(&mut self, command: &str, params: Value) -> Value {
        let r = self.ok("engine.execute", json!({"command": command, "params": params}));
        self.frames(3);
        r
    }
    fn shot(&mut self, name: &str) {
        let Some(dir) = shots_dir() else { return };
        // give the frame workers time to deliver the pictures
        for _ in 0..8 {
            self.frames(3);
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
        let img = self.harness.render().expect("wgpu render");
        std::fs::create_dir_all(&dir).unwrap();
        img.save(dir.join(format!("par-{name}.png"))).unwrap();
    }
    /// Width over height of an element's rect.
    fn aspect(&mut self, id: &str) -> f64 {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == id).unwrap_or_else(|| panic!("no element {id}")).clone();
        let r: Vec<f64> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
        r[2] / r[3]
    }
}

/// A session with a 144 x 108 Color Matte whose pixels are `par`.
fn session_with_matte(par: (u32, u32)) -> (Session, ItemId) {
    let mut s = Session::default();
    let r = s.execute("file.newColorMatte", json!({"color": "#3366ff", "width": 144, "height": 108, "seconds": 2.0})).unwrap();
    let id = ItemId(r["item"].as_u64().unwrap());
    if let Some(ItemKind::Media(m)) = Arc::make_mut(&mut s.project).item_mut(id).map(|i| &mut i.kind) {
        m.info.video.as_mut().unwrap().par = par;
    }
    (s, id)
}

#[test]
fn monitors_show_non_square_pixels_at_the_display_aspect() {
    for (par, want) in [((4, 3), 16.0 / 9.0), ((1, 1), 4.0 / 3.0), ((0, 0), 4.0 / 3.0), ((u32::MAX, 1), 4.0 / 3.0)] {
        let (s, item) = session_with_matte(par);
        let mut d = Driver::new(s);
        d.exec("file.newSequenceFromClip", json!({"items": [item.0]}));
        d.exec("source.open", json!({"item": item.0}));
        let (src, prg) = (d.aspect("source.picture"), d.aspect("program.picture"));
        assert!((src - want).abs() < 0.01, "{par:?}: Source picture {src}");
        assert!((prg - want).abs() < 0.01, "{par:?}: Program picture {prg}");
        d.shot(&format!("monitors-{}x{}", par.0, par.1));
        // Interpret Footage changes the Source monitor's picture at once
        d.exec("clip.interpretFootage", json!({"items": [item.0], "pixelAspect": [2, 1]}));
        let src = d.aspect("source.picture");
        assert!((src - 8.0 / 3.0).abs() < 0.01, "{par:?}: interpreted 2:1, Source picture {src}");
    }
}
