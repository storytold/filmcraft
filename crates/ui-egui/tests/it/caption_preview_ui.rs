//! Caption-card seeking and explicit bounded playback through real UI widgets.
use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::{FilmcraftApp, control::ControlRequest};
use serde_json::{Value, json};
use std::sync::mpsc::{Sender, channel};
struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    snapshots: Option<std::path::PathBuf>,
}

impl Driver {
    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let snapshots = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR").map(std::path::PathBuf::from);
        let mut b = Harness::builder().with_step_dt(1.0 / 60.0).with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000);
        if snapshots.is_some() {
            b = b.wgpu();
        }
        let harness = b.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, snapshots };
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
            if let Ok(v) = reply.try_recv() {
                assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
                return v["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn snapshot(&mut self, name: &str) {
        let Some(dir) = self.snapshots.clone() else { return };
        self.frames(2);
        match self.harness.render() {
            Ok(img) => {
                std::fs::create_dir_all(&dir).unwrap();
                img.save(dir.join(format!("transcript-{name}.png"))).unwrap();
            }
            Err(e) => eprintln!("snapshot {name} skipped: {e}"),
        }
    }
}

fn fixture() -> (Driver, Vec<u64>) {
    let mut d = Driver::demo();
    let mut captions = Vec::new();
    for (seconds, text) in [(1, "First line"), (3, "Second line"), (5, "Third line")] {
        let id =
            d.ok("engine.execute", json!({"command":"captions.add","params":{"seconds":seconds,"durationSeconds":1,"text":text}}))["caption"].as_u64().unwrap();
        captions.push(id);
    }
    d.ok("ui.set", json!({"workspace":"Captions and Graphics"}));
    d.ok("ui.panel.show", json!({"panel":"Text"}));
    d.frames(3);
    d.ok("ui.click", json!({"id":"text.tab.Captions"}));
    d.frames(3);
    (d, captions)
}
fn blank_click(d: &mut Driver, id: u64) {
    let elements = d.ok("ui.elements", json!({"prefix":format!("text.captions.{id}.row")}));
    let r = elements[0]["rect"].as_array().unwrap();
    let x = r[0].as_f64().unwrap() + r[2].as_f64().unwrap() - 10.0;
    let y = r[1].as_f64().unwrap() + r[3].as_f64().unwrap() - 7.0;
    d.ok("ui.click", json!({"x":x,"y":y}));
    d.frames(3);
}
#[test]
fn caption_card_background_and_number_seek_without_playing_or_editing() {
    let (mut d, ids) = fixture();
    let before = d.harness.state().session.project.clone();
    let undo = d.harness.state().session.history.undo.len();
    for index in [1, 2, 0] {
        blank_click(&mut d, ids[index]);
        assert_eq!(
            d.harness.state().session.playhead(),
            d.harness.state().session.active_sequence().unwrap().find_caption(filmcraft_project::ClipId(ids[index])).unwrap().1.start
        );
        assert_eq!(d.harness.state().session.state.caption_selection, vec![filmcraft_project::ClipId(ids[index])]);
        assert!(!d.harness.state().playback.playing);
    }
    d.ok("ui.click", json!({"id":format!("text.captions.{}.goto",ids[1])}));
    d.frames(3);
    assert_eq!(
        d.harness.state().session.playhead(),
        d.harness.state().session.active_sequence().unwrap().find_caption(filmcraft_project::ClipId(ids[1])).unwrap().1.start
    );
    assert_eq!(d.harness.state().session.project, before);
    assert_eq!(d.harness.state().session.history.undo.len(), undo);
    d.snapshot("caption-cards-seek");
}
#[test]
fn caption_text_and_timecode_keep_editing_focus_and_never_autoplay() {
    let (mut d, ids) = fixture();
    d.ok("engine.execute", json!({"command":"playhead.set","params":{"seconds":0}}));
    d.ok("ui.click", json!({"id":format!("text.captions.{}.text",ids[1])}));
    d.ok("ui.key", json!({"key":"A","command":true}));
    d.ok("ui.type", json!({"text":"Edited words"}));
    d.ok("ui.key", json!({"key":"Space"}));
    assert!(!d.harness.state().playback.playing);
    assert_eq!(d.harness.state().session.playhead(), filmcraft_time::Tick::ZERO);
    d.ok("ui.click", json!({"id":format!("text.captions.{}.in",ids[1])}));
    d.frames(3);
    assert_eq!(d.harness.state().session.active_sequence().unwrap().find_caption(filmcraft_project::ClipId(ids[1])).unwrap().1.text.trim(), "Edited words");
    assert!(!d.harness.state().playback.playing);
    assert_eq!(d.harness.state().session.playhead(), filmcraft_time::Tick::ZERO);
    assert!(d.harness.ctx.memory(|m| m.focused().is_some()), "the timecode remains a real editor");
    d.snapshot("caption-card-edit");
}
#[test]
fn caption_play_button_previews_only_its_range_and_second_click_stops() {
    let (mut d, ids) = fixture();
    let before = d.harness.state().session.project.clone();
    let undo = d.harness.state().session.history.undo.len();
    d.harness.state_mut().playback.looping = true;
    d.ok("ui.click", json!({"id":format!("text.captions.{}.play",ids[1])}));
    d.frames(3);
    assert!(d.harness.state().playback.playing);
    assert!(!d.harness.state().playback.looping);
    assert_eq!(
        d.harness.state().playback.stop_at,
        Some(d.harness.state().session.active_sequence().unwrap().find_caption(filmcraft_project::ClipId(ids[1])).unwrap().1.end())
    );
    assert!(!d.harness.state().session.mixrec.active());
    d.snapshot("caption-card-preview");
    d.ok("ui.click", json!({"id":format!("text.captions.{}.play",ids[1])}));
    d.frames(3);
    assert!(!d.harness.state().playback.playing);
    assert!(d.harness.state().playback.looping);
    assert_eq!(d.harness.state().session.project, before);
    assert_eq!(d.harness.state().session.history.undo.len(), undo);
}
