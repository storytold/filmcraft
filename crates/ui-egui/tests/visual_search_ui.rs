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

struct Model;
impl filmcraft_vision::Embedder for Model {
    fn text(&self, _: &str) -> Result<Vec<f32>, String> {
        Ok(vec![1.0; 512])
    }
    fn image(&self, _: u32, _: u32, _: &[u8]) -> Result<Vec<f32>, String> {
        Ok(vec![1.0; 512])
    }
}
impl Driver {
    fn wait_visual(&mut self) {
        for _ in 0..400 {
            self.frames(2);
            let info = self.ok("engine.execute", json!({"command":"visualSearch.inspect","params":{}}));
            if info["busy"] == false {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("visual job timed out");
    }
    fn exercise_visual_search(&mut self, query: &str) {
        self.ok("ui.set", json!({"workspace":"Captions and Graphics"}));
        self.harness.state_mut().ui.text_tab = "Search".into();
        self.frames(3);
        self.ok("ui.click", json!({"id":"text.search.visuals"}));
        self.ok("ui.click", json!({"id":"text.visual.index"}));
        self.wait_visual();
        self.ok("ui.click", json!({"id":"text.visual.query"}));
        self.ok("ui.type", json!({"text":query}));
        self.ok("ui.click", json!({"id":"text.visual.search"}));
        self.wait_visual();
        self.frames(3);
        assert!(!self.ids("text.visual.result.").is_empty());
        self.snapshot("visual-results");
        let project = self.harness.state().session.project.clone();
        self.ok("ui.click", json!({"id":"text.visual.result.0"}));
        assert!(self.harness.state().session.state.source_item.is_some());
        assert!(std::sync::Arc::ptr_eq(&project, &self.harness.state().session.project));
        let mut painted = false;
        for _ in 0..500 {
            self.frames(2);
            let app = self.harness.state();
            let item = app.session.state.source_item.unwrap();
            let rate = filmcraft_engine::clip_ops::source_view(&app.session, item).unwrap().rate;
            let frame = rate.frame_at(app.session.state.source_playhead);
            let revision = app.item_revision(item);
            for size in 1..=1000 {
                let key = filmcraft_ui_egui::frames::FrameKey { target: filmcraft_ui_egui::frames::Target::Item(item), frame, size, revision, draft: false };
                if let Some(image) = app.frames.get(&key) {
                    assert!(image.px.as_chunks::<4>().0.iter().any(|p| p[0] > 30 || p[1] > 30 || p[2] > 30), "source picture is not black");
                    painted = true;
                    break;
                }
            }
            if painted {
                self.frames(3);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(painted, "the matching source frame was rendered");
        self.snapshot("visual-source");
    }
}
#[test]
fn visual_controls_index_search_and_open_results() {
    let mut d = Driver::demo();
    let session = &mut d.harness.state_mut().session;
    session.visual_search.embedder = Some(std::sync::Arc::new(Model));
    session.visual_search.cache_dir = Some(std::env::temp_dir().join(format!("visual-ui-cache-{}", std::process::id())));
    d.exercise_visual_search("sunset over water");
}
#[test]
#[ignore = "requires visual-search feature, local model and FILMCRAFT_VISUAL_VIDEO"]
fn real_model_video_search_in_the_ui() {
    let mut d = Driver::demo();
    let session = &mut d.harness.state_mut().session;
    session.execute("file.newProject", json!({})).unwrap();
    session.visual_search.model_dir = Some(std::path::PathBuf::from(std::env::var("FILMCRAFT_CLIP_DIR").expect("model")));
    session.visual_search.cache_dir = Some(std::env::temp_dir().join(format!("visual-real-ui-cache-{}", std::process::id())));
    session.execute("file.import", json!({"paths":[std::env::var("FILMCRAFT_VISUAL_VIDEO").expect("video")]})).unwrap();
    d.exercise_visual_search("an astronaut standing on the Moon");
    assert_eq!(d.harness.state().session.state.source_playhead, filmcraft_time::Tick::from_seconds_f64(4.0));
}
