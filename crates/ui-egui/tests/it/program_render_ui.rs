//! `program.render` through the control channel: the MCP bridge's `render_frame` uses it to get a
//! frame while the app window is hidden. It must be the frame headless mode renders (the Program
//! preview's media, over black) and a query: no playhead move, no undo step.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.harness.step();
        d
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params);
        self.tx.send(req).unwrap();
        for _ in 0..200 {
            self.harness.step();
            if let Ok(v) = reply.try_recv() {
                return v;
            }
        }
        panic!("no reply to {method}");
    }
}

fn decode(path: &std::path::Path) -> image::RgbaImage {
    image::open(path).unwrap().to_rgba8()
}

#[test]
fn program_render_is_the_headless_frame_and_changes_nothing() {
    let mut d = Driver::demo();
    let dir = tempfile::tempdir().unwrap();
    let (playhead, undo) = {
        let s = &d.harness.state().session;
        (s.playhead(), s.history.undo.len())
    };
    let path = dir.path().join("f.png");
    let v = d.call("program.render", json!({"path": path.to_string_lossy(), "seconds": 3.0, "maxSide": 480}));
    assert_eq!(v["ok"], json!(true), "{v}");
    let got = decode(&path);
    assert!(got.width().max(got.height()) <= 480, "scaled to maxSide");
    // the same picture headless render_frame returns at that time and size
    let s = &d.harness.state().session;
    let (w, h) = s.active_sequence().map(|q| (q.settings.width, q.settings.height)).unwrap();
    let want = s.try_render_program_at((480.0 / w.max(h) as f32).min(1.0), filmcraft_time::Tick::from_seconds_f64(3.0)).unwrap();
    assert_eq!((got.width(), got.height()), (want.w as u32, want.h as u32));
    assert_eq!(got.as_raw(), &want.over_black_rgba8(), "the headless frame, over black");
    assert_eq!((s.playhead(), s.history.undo.len()), (playhead, undo), "a query: no playhead move, no undo step");

    // past the last clip: black, opaque (not a transparent PNG)
    let end = s.active_sequence().unwrap().duration().seconds();
    let v = d.call("program.render", json!({"path": path.to_string_lossy(), "seconds": end + 5.0, "maxSide": 64}));
    assert_eq!(v["ok"], json!(true), "{v}");
    assert!(decode(&path).pixels().all(|p| p.0 == [0, 0, 0, 255]));

    // maxSide 0 is no limit: the sequence's own size
    let v = d.call("program.render", json!({"path": path.to_string_lossy(), "seconds": 1.0, "maxSide": 0}));
    assert_eq!(v["ok"], json!(true), "{v}");
    assert_eq!((v["result"]["width"].as_u64(), v["result"]["height"].as_u64()), (Some(u64::from(w)), Some(u64::from(h))), "{v}");

    // without a path the PNG comes back in the reply; a bad time is refused
    let v = d.call("program.render", json!({"maxSide": 64}));
    assert!(v["result"]["pngBase64"].as_str().is_some_and(|b| !b.is_empty()), "{v}");
    assert_eq!(d.call("program.render", json!({"seconds": -1.0}))["ok"], json!(false));
}
