//! The wrench menu's Show Video Names / Show Audio Names hide the name of the clips on video or on
//! audio tracks.

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
        d.frames(4);
        d
    }
    fn frames(&mut self, n: usize) {
        for _ in 0..n {
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
            if let Ok(r) = reply.try_recv() {
                assert_eq!(r["ok"], true, "{method} {params}: {r}");
                self.frames(2);
                return r["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }
}

/// Every text drawn in the last frame.
fn texts(d: &Driver) -> Vec<String> {
    d.harness
        .output()
        .shapes
        .iter()
        .filter_map(|s| match &s.shape {
            egui::epaint::Shape::Text(t) => Some(t.galley.text().to_string()),
            _ => None,
        })
        .collect()
}

#[test]
fn the_name_toggles_hide_the_clip_names_of_video_or_audio_tracks() {
    let mut d = Driver::demo();
    let (video, audio) = {
        let q = d.harness.state().session.active_sequence().unwrap();
        (q.video_tracks[0].items[0].name.clone(), q.audio_tracks[0].items[0].name.clone())
    };
    d.frames(3);
    let shows = |d: &Driver, name: &str| texts(d).iter().any(|t| t.contains(name));
    assert!(shows(&d, &video), "the video clip's name is drawn by default");
    assert!(shows(&d, &audio), "so is the audio clip's");
    d.ok("ui.set", json!({"timeline": {"showVideoNames": false}}));
    d.frames(3);
    assert!(!shows(&d, &video) || video == audio, "Show Video Names off hides the video clip names");
    assert!(shows(&d, &audio), "and leaves the audio names");
    d.ok("ui.set", json!({"timeline": {"showVideoNames": true, "showAudioNames": false}}));
    d.frames(3);
    assert!(shows(&d, &video));
    assert!(!shows(&d, &audio) || video == audio, "Show Audio Names off hides the audio clip names");
}
