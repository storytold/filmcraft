//! Robustness: a panic while decoding/rendering a frame must not kill the frame worker (the
//! monitors would stay blank), and a panic in the UI pass must not close the app (losing unsaved
//! work) — it shows an error window and the session carries on.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

/// The tests run one at a time: fault injection (`inject_job_panics`) is process-wide, so frame
/// workers of a parallel test would consume the injected panics.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    fn demo() -> Self {
        filmcraft_ui_egui::crash::install(None);
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        d
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            // synthetic input (ui.click) enters through the app's raw input hook, as in eframe
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
                assert_eq!(v["ok"], json!(true), "{method} {params}: {v}");
                return v["result"].clone();
            }
        }
        panic!("no reply to {method}");
    }

    fn frame_stats(&mut self) -> Value {
        self.ok("engine.execute", json!({"command": "perf.stats"}))["frames"].clone()
    }

    /// Step until the frame workers are idle.
    fn settle(&mut self) {
        for _ in 0..400 {
            self.frames(1);
            if self.harness.state().frames.queue_len() == 0 {
                std::thread::sleep(std::time::Duration::from_millis(20));
                self.frames(1);
                if self.harness.state().frames.queue_len() == 0 {
                    return;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}

#[test]
fn frame_worker_survives_a_panicking_job_and_keeps_rendering() {
    let _serial = serial();
    let mut d = Driver::demo();
    d.harness.state_mut().enable_cpu_composite();
    d.settle();
    let jobs_before = d.frame_stats()["jobs"].as_u64().unwrap_or(0);
    let workers = d.harness.state().frames.workers() as u32;
    // more faults than there are workers: before the fix every worker died and nothing rendered
    filmcraft_ui_egui::frames::inject_job_panics(workers + 2);
    for t in [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0] {
        d.ok("engine.execute", json!({"command": "playhead.set", "params": {"seconds": t}}));
        d.settle();
    }
    let st = d.frame_stats();
    assert_eq!(st["failedJobs"].as_u64(), Some(workers as u64 + 2), "{st}");
    let done_after_faults = st["jobs"].as_u64().unwrap_or(0);
    assert!(done_after_faults > jobs_before, "workers still finish jobs after the panics: {st}");
    assert_eq!(d.harness.state().frames.queue_len(), 0, "no job stuck in the queue");
}

#[test]
fn a_ui_panic_shows_an_error_window_and_the_app_keeps_running() {
    let _serial = serial();
    let mut d = Driver::demo();
    d.harness.state_mut().panic_next_frame = true;
    d.frames(2);
    let msg = d.harness.state().ui_error.clone().expect("error recorded");
    assert!(msg.contains("injected UI fault"), "{msg}");
    // the session survived: commands still run and the error window is on screen
    d.ok("engine.execute", json!({"command": "playhead.set", "params": {"seconds": 2.0}}));
    let els = d.ok("ui.elements", json!({"prefix": "error."}));
    assert!(els.to_string().contains("error.dismiss"), "{els}");
    d.ok("ui.click", json!({"id": "error.dismiss"}));
    d.frames(3);
    assert!(d.harness.state().ui_error.is_none());
    // and the UI keeps drawing normally afterwards
    let els = d.ok("ui.elements", json!({"prefix": "timeline."}));
    assert!(els.as_array().is_some_and(|a| !a.is_empty()) || els.is_object(), "{els}");
}

/// Tiny windows and squeezed panels: dock splits under 40 points used to panic in
/// `dock::layout` (`clamp` with min > max), killing the app (web) or the UI pass.
#[test]
fn tiny_windows_and_squeezed_panels_do_not_panic() {
    let _serial = serial();
    filmcraft_ui_egui::crash::install(None);
    for (w, h) in [(120.0, 80.0), (300.0, 200.0), (60.0, 40.0), (900.0, 560.0)] {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let app = FilmcraftApp::new(session);
        let mut harness = Harness::builder().with_size(egui::vec2(w, h)).with_max_steps(10_000).build_eframe(move |_cc| app);
        for ws in ["Editing", "Color", "Audio", "Effects", "Captions and Graphics", "Assembly"] {
            harness.state_mut().set_workspace(ws);
            for _ in 0..3 {
                harness.step();
            }
        }
        assert!(harness.state().ui_error.is_none(), "{w}x{h}: {:?}", harness.state().ui_error);
    }
}
