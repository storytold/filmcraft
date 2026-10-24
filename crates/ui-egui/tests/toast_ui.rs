//! Headless UI test of toasts (#2443): what the engine raises with `Session::toast` and
//! `Session::error_toast` reaches the window, as automation elements `toast.<n>`, and goes away
//! on a click. The real `FilmcraftApp` under `egui_kittest`.
//!
//! Set `FILMCRAFT_UI_SNAPSHOT_DIR=<dir>` to also render the window offscreen with wgpu and write
//! `toasts.png` there; without it no GPU is needed.

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
        let mut b = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000);
        if std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR").is_some() {
            b = b.wgpu();
        }
        let harness = b.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        d
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            // Synthetic input (`ui.click`) reaches egui through the app's raw-input hook.
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
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
            if let Ok(v) = reply.try_recv() {
                assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
                return v["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }

    /// The toasts on screen, oldest first, by their text.
    fn toasts(&mut self) -> Vec<String> {
        let v = self.ok("ui.elements", json!({"prefix": "toast."}));
        v.as_array().unwrap().iter().map(|e| e["label"].as_str().unwrap_or_default().to_string()).collect()
    }

    fn snapshot(&mut self, name: &str) {
        let Some(dir) = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR").map(std::path::PathBuf::from) else { return };
        self.frames(2);
        let img = match self.harness.render() {
            Ok(i) => i,
            Err(e) => {
                eprintln!("snapshot {name} skipped: {e}");
                return;
            }
        };
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{name}.png"));
        img.save(&path).unwrap();
        eprintln!("snapshot: {}", path.display());
    }
}

#[test]
fn engine_toasts_are_drawn_and_dismissed() {
    let mut d = Driver::demo();
    assert!(d.toasts().is_empty());
    // A plain toast from an engine command, and an error toast the way background jobs raise them.
    d.ok("engine.execute", json!({"command": "audio.toggleScrubbing", "params": {}}));
    d.harness.state_mut().session.error_toast("masks.track", "Mask tracking failed: the mask is gone");
    d.frames(2);
    let shown = d.toasts();
    assert_eq!(shown.len(), 2, "{shown:?}");
    assert!(shown[0].starts_with("Audio during scrubbing"), "{shown:?}");
    assert_eq!(shown[1], "Mask tracking failed: the mask is gone");
    d.snapshot("toasts");
    d.ok("ui.click", json!({"id": "toast.0"}));
    d.frames(2);
    assert_eq!(d.toasts(), ["Mask tracking failed: the mask is gone"]);
}
