//! The Sous-titres tab: the edit window turns a line break into the next caption, right after the
//! first on the timeline.

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
        self.tx.send(req).expect("control channel");
        for _ in 0..600 {
            self.frames(1);
            if let Ok(r) = reply.try_recv() {
                assert_eq!(r["ok"], true, "{method} {params}: {r}");
                return r["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }
    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }
    fn ids(&mut self, prefix: &str) -> Vec<String> {
        self.frames(2);
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().expect("elements").iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }
    fn captions(&mut self) -> Vec<Value> {
        let r = self.exec("captions.list", json!({}));
        r["tracks"][0]["captions"].as_array().expect("captions").clone()
    }
}

#[test]
fn edit_window_splits_a_caption_at_a_line_break() {
    let dir = std::env::temp_dir().join(format!("filmcraft-caption-split-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let srt = dir.join("one.srt");
    std::fs::write(&srt, "1\n00:00:01,000 --> 00:00:05,000\nBonjour\n").expect("write srt");

    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).expect("demo project");
    let (tx, rx) = channel();
    let app = FilmcraftApp::new(session).with_control(rx);
    let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
    let mut d = Driver { harness, tx };
    d.frames(4);
    d.exec("captions.import", json!({"path": srt.to_string_lossy()}));
    d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
    let first = d.captions()[0]["id"].as_u64().expect("caption id");

    // the generate button is always listed (disabled while the demo has no transcript)
    assert!(d.ids("text.captions.fromTranscript").len() == 1, "the button is listed (disabled without a transcript)");
    d.exec("captions.select", json!({"captions": [first]}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "text.captions.splitText"}));
    d.frames(3);
    assert!(d.ids("captionSplit.").iter().any(|s| s == "captionSplit.text"), "the window opened");

    d.ok("ui.click", json!({"id": "captionSplit.text"}));
    d.ok("ui.key", json!({"key": "End"}));
    d.ok("ui.key", json!({"key": "Enter"}));
    d.ok("ui.type", json!({"text": "ça va"}));
    d.ok("ui.click", json!({"id": "captionSplit.apply"}));
    d.frames(3);
    let caps = d.captions();
    assert_eq!(caps.len(), 2, "{caps:?}");
    assert_eq!((caps[0]["text"].as_str(), caps[1]["text"].as_str()), (Some("Bonjour"), Some("ça va")), "{caps:?}");
    assert!(d.ids("captionSplit.").is_empty(), "the window closed");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Enter in a caption's text in the list cuts it at the cursor: what follows becomes the next
/// caption. Shift+Enter keeps both lines in one caption.
#[test]
fn enter_in_the_caption_text_cuts_it_at_the_cursor() {
    let dir = std::env::temp_dir().join(format!("filmcraft-caption-enter-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let srt = dir.join("one.srt");
    std::fs::write(&srt, "1\n00:00:01,000 --> 00:00:05,000\nBonjour ça va\n").expect("write srt");

    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).expect("demo project");
    let (tx, rx) = channel();
    let app = FilmcraftApp::new(session).with_control(rx);
    let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
    let mut d = Driver { harness, tx };
    d.frames(4);
    d.exec("captions.import", json!({"path": srt.to_string_lossy()}));
    d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
    let first = d.captions()[0]["id"].as_u64().expect("caption id");
    let field = format!("text.captions.{first}.text");

    d.ok("ui.click", json!({"id": field}));
    d.ok("ui.key", json!({"key": "Home"}));
    for _ in 0.."Bonjour".chars().count() {
        d.ok("ui.key", json!({"key": "ArrowRight"}));
    }
    d.ok("ui.key", json!({"key": "Enter"}));
    d.frames(4);
    let caps = d.captions();
    assert_eq!(caps.len(), 2, "{caps:?}");
    assert_eq!((caps[0]["text"].as_str(), caps[1]["text"].as_str()), (Some("Bonjour"), Some("ça va")), "{caps:?}");
    assert_eq!(caps[0]["end"], caps[1]["start"], "the second follows the first");
    let _ = std::fs::remove_dir_all(&dir);
}
