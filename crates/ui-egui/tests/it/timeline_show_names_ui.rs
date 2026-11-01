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
    // the demo's linked clips share a name: give the audio one its own, so each toggle is seen alone
    let (video, audio_clip) = {
        let q = d.harness.state().session.active_sequence().unwrap();
        (q.video_tracks[0].items[0].name.clone(), q.audio_tracks[0].items[0].id)
    };
    let audio = "Ambience take";
    d.ok("engine.execute", json!({"command": "clip.rename", "params": {"clip": audio_clip.0, "name": audio}}));
    d.frames(3);
    let shows = |d: &Driver, name: &str| texts(d).iter().any(|t| t.contains(name));
    assert!(shows(&d, &video), "the video clip's name is drawn by default");
    assert!(shows(&d, audio), "so is the audio clip's");
    d.ok("ui.set", json!({"timeline": {"showVideoNames": false}}));
    d.frames(3);
    assert!(!shows(&d, &video), "Show Video Names off hides the video clip names");
    assert!(shows(&d, audio), "and leaves the audio names");
    d.ok("ui.set", json!({"timeline": {"showVideoNames": true, "showAudioNames": false}}));
    d.frames(3);
    assert!(shows(&d, &video));
    assert!(!shows(&d, audio), "Show Audio Names off hides the audio clip names");
}

/// A hidden name keeps the speed of a retimed clip, which nothing else on the clip shows.
#[test]
fn a_hidden_name_keeps_the_speed_label() {
    let mut d = Driver::demo();
    let clip = d.harness.state().session.active_sequence().unwrap().video_tracks[0].items[0].id;
    d.ok("engine.execute", json!({"command": "clip.speedDuration", "params": {"clips": [clip.0], "speed": 50.0}}));
    d.ok("ui.set", json!({"timeline": {"showVideoNames": false}}));
    d.frames(3);
    assert!(texts(&d).iter().any(|t| t.contains("[50%]")), "the speed is still drawn: {:?}", texts(&d));
}

/// The Timeline panel's own menu has the same two toggles, each with an automation id.
#[test]
fn the_timeline_panel_menu_has_the_name_toggles() {
    let mut d = Driver::demo();
    for (id, video) in [("panel.menu.Timeline.videoNames", true), ("panel.menu.Timeline.audioNames", false)] {
        d.ok("ui.click", json!({"id": "panel.menu.Timeline"}));
        d.ok("ui.click", json!({"id": id}));
        let v = &d.harness.state().ui.timeline;
        assert!(!(if video { v.show_video_names } else { v.show_audio_names }), "{id} turns its names off");
        d.ok("ui.click", json!({"id": "panel.menu.Timeline"}));
        d.ok("ui.click", json!({"id": id}));
        let v = &d.harness.state().ui.timeline;
        assert!(if video { v.show_video_names } else { v.show_audio_names }, "{id} turns them on again");
        d.frames(2);
    }
}
