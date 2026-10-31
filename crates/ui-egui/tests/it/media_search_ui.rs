//! Project-wide transcript search through real UI controls and source navigation.
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
        let mut b = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000);
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

    fn ids(&mut self, prefix: &str) -> Vec<String> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
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

#[test]
fn search_unused_sources_and_open_a_hit_without_editing_the_document() {
    let mut d = Driver::demo();
    let item = d.harness.state().session.active_sequence().unwrap().audio_tracks[0].items[0].item;
    d.ok(
        "engine.execute",
        json!({"command":"transcript.set","params":{"item":item.0,"transcript":{"words":[
        {"text":"Hello,","start":254016000000_i64,"end":381024000000_i64},
        {"text":"world!","start":381024000000_i64,"end":508032000000_i64}]}}}),
    );
    d.harness.state_mut().session.state.active_sequence = None;
    d.ok("ui.set", json!({"workspace":"Captions and Graphics"}));
    d.harness.state_mut().ui.text_tab = "Search".into();
    d.frames(3);
    d.ok("ui.click", json!({"id":"text.search.query"}));
    d.ok("ui.type", json!({"text":"hello world"}));
    let mut ready = false;
    for _ in 0..100 {
        d.frames(2);
        if d.ids("text.search.result.").len() == 1 {
            ready = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(ready, "background query produced its result");
    d.snapshot("project-search");
    let project = d.harness.state().session.project.clone();
    d.ok("ui.click", json!({"id":"text.search.result.0"}));
    assert_eq!(d.harness.state().session.state.source_item, Some(item));
    let rate = filmcraft_engine::clip_ops::source_view(&d.harness.state().session, item).unwrap().rate;
    assert_eq!(d.harness.state().session.state.source_playhead, rate.snap(filmcraft_time::Tick(254016000000)));
    assert!(std::sync::Arc::ptr_eq(&project, &d.harness.state().session.project));
    d.snapshot("project-search-source");
}
