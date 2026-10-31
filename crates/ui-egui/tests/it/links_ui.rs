//! Headless UI test of the community links: Help menu commands, the header Discord button, the
//! About dialog and the Home (Import) screen.
//!
//! Set `FILMCRAFT_UI_SNAPSHOT_DIR=<dir>` to also render the window offscreen with wgpu and write
//! `links-*.png` there; without it no GPU is needed.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use filmcraft_ui_egui::links;
use serde_json::{Value, json};

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
                img.save(dir.join(format!("links-{name}.png"))).unwrap();
            }
            Err(e) => eprintln!("snapshot {name} skipped: {e}"),
        }
    }
}

#[test]
fn community_links_everywhere() {
    let mut d = Driver::demo();
    // Help menu: every link is a command that reports the URL it opened.
    for (id, _, url) in links::ALL {
        let r = d.ok("engine.execute", json!({"command": id, "params": {}}));
        assert_eq!(r["url"], json!(url), "{id}");
    }
    let menu = d.ok("ui.menu.list", json!({}));
    let text = menu.to_string();
    for (id, label, _) in links::ALL {
        assert!(text.contains(id) && text.contains(label), "Help menu lists {id}");
    }
    // Header: the Discord button is always there.
    assert_eq!(d.ids("header.discord"), vec!["header.discord".to_string()]);
    d.snapshot("header");
    // About dialog: Discord first, then website, app page, GitHub, issues.
    d.ok("engine.execute", json!({"command": "app.about", "params": {}}));
    d.frames(3);
    let about = d.ids("about.");
    for id in ["about.discord", "about.website", "about.appPage", "about.github", "about.reportIssue"] {
        assert!(about.iter().any(|a| a == id), "{id} in {about:?}");
    }
    d.snapshot("about");
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(3);
    assert!(d.ids("about.").is_empty(), "Escape closes About");
    // Home screen (Import mode): community links.
    d.ok("engine.execute", json!({"command": "mode.import", "params": {}}));
    d.frames(3);
    let home = d.ids("import.link.");
    for id in ["import.link.discord", "import.link.website", "import.link.appPage", "import.link.github"] {
        assert!(home.iter().any(|a| a == id), "{id} in {home:?}");
    }
    d.snapshot("home");
}
