//! Headless UI test of the caption track font picker in the Text panel's Captions tab: picking a
//! family sets the track style's font, and macOS-private (`.`-prefixed) faces are not offered.

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
            if let Ok(v) = reply.try_recv() {
                assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
                return v["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn ids(&mut self, prefix: &str) -> Vec<String> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }

    fn font(&mut self, track: u64) -> String {
        let r = self.ok("engine.execute", json!({"command": "captions.list", "params": {}}));
        let t = r["tracks"].as_array().unwrap().iter().find(|t| t["id"] == json!(track)).expect("track").clone();
        t["style"]["font"].as_str().unwrap().to_string()
    }
}

#[test]
fn caption_font_picker_sets_the_track_font() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
    let track = d.ok("engine.execute", json!({"command": "captions.newTrack", "params": {"format": "Subtitle"}}))["track"].as_u64().unwrap();
    d.ok("engine.execute", json!({"command": "captions.add", "params": {"track": track, "text": "Hello", "seconds": 1.0}}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "text.tab.Captions"}));
    d.frames(3);
    assert_eq!(d.font(track), "Inter");

    d.ok("ui.click", json!({"id": "text.captions.style.font"}));
    d.frames(3);
    let options = d.ids("text.captions.style.font.option.");
    assert!(!options.is_empty(), "the font list is open");
    assert!(!options.iter().any(|o| o.starts_with("text.captions.style.font.option..")), "private faces offered: {options:?}");
    // with system fonts the list is long: scroll until the bundled serif is on screen
    let serif = "text.captions.style.font.option.Noto Serif";
    for _ in 0..200 {
        let shown = d.ids("text.captions.style.font.option.");
        if shown.iter().any(|o| o == serif) {
            break;
        }
        // wheel over a row in the middle (the edge rows are clipped by the popup)
        let Some(mid) = shown.get(shown.len() / 2) else { break };
        d.ok("ui.scroll", json!({"id": mid, "dy": -120.0}));
        d.frames(2);
    }
    d.ok("ui.click", json!({"id": serif}));
    d.frames(3);
    assert_eq!(d.font(track), "Noto Serif");
}

#[test]
fn selected_captions_can_be_upgraded_to_graphics_together() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
    d.ok("ui.click", json!({"id": "text.tab.Captions"}));
    let first = d.ok("engine.execute", json!({"command": "captions.add", "params": {"text": "First", "seconds": 600}}))["caption"].as_u64().unwrap();
    let second = d.ok("engine.execute", json!({"command": "captions.add", "params": {"text": "Second", "seconds": 604}}))["caption"].as_u64().unwrap();
    d.ok("engine.execute", json!({"command": "captions.select", "params": {"captions": [first, second]}}));
    d.frames(3);

    let first_start = d
        .harness
        .state()
        .session
        .active_sequence()
        .unwrap()
        .caption_tracks
        .iter()
        .flat_map(|track| &track.captions)
        .find(|caption| caption.id.0 == first)
        .unwrap()
        .start;
    let text_field = d.ids(&format!("text.captions.{first}.text"));
    assert_eq!(text_field.len(), 1, "the selected caption text editor is available");
    d.ok("ui.click", json!({"id": text_field[0]}));
    d.ok("ui.key", json!({"key": "Cmd+A"}));
    d.ok("ui.type", json!({"text": "Edited first"}));

    let button = d.ids("text.captions.upgrade");
    assert_eq!(button.len(), 1, "the caption toolbar exposes the batch upgrade action");
    let undo_count = d.harness.state().session.history.undo.len();
    let initial_video_items = d.harness.state().session.active_sequence().unwrap().video_tracks.iter().map(|track| track.items.len()).sum::<usize>();
    d.ok("ui.click", json!({"id": button[0]}));
    let sequence = d.harness.state().session.active_sequence().unwrap();
    assert_eq!(sequence.caption_tracks.iter().map(|track| track.captions.len()).sum::<usize>(), 0);
    assert_eq!(sequence.video_tracks.iter().map(|track| track.items.len()).sum::<usize>(), initial_video_items + 2);
    assert_eq!(d.harness.state().session.history.undo.len(), undo_count + 2, "the pending text edit and toolbar upgrade each have one undo step");
    let graphic_clip =
        sequence.video_tracks.iter().flat_map(|track| &track.items).find(|item| item.start == first_start).expect("the first caption became a graphic").id.0;
    let graphic_layers = d.ok("engine.execute", json!({"command": "graphics.list", "params": {"clip": graphic_clip}}));
    assert_eq!(graphic_layers["layers"][0]["text"], "Edited first", "the upgrade commits pending text before deleting the caption");

    d.ok("engine.execute", json!({"command": "edit.undo", "params": {}}));
    let sequence = d.harness.state().session.active_sequence().unwrap();
    assert_eq!(sequence.caption_tracks.iter().map(|track| track.captions.len()).sum::<usize>(), 2);
    assert_eq!(sequence.video_tracks.iter().map(|track| track.items.len()).sum::<usize>(), initial_video_items);
    let restored = sequence.caption_tracks.iter().flat_map(|track| &track.captions).find(|caption| caption.id.0 == first).expect("undo restores the caption");
    assert_eq!(restored.text, "Edited first", "undoing the upgrade preserves the committed caption edit");
}
