//! The Script window keeps wrapped text, candle coverage and marks aligned in one scroll space.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_project::ItemKind;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    snapshots: Option<std::path::PathBuf>,
}

impl Driver {
    fn new() -> Self {
        Self::with_size(egui::vec2(1600.0, 980.0))
    }

    fn with_size(size: egui::Vec2) -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let snapshots = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR").map(std::path::PathBuf::from);
        let mut builder = Harness::builder().with_size(size).with_max_steps(10_000);
        if snapshots.is_some() {
            builder = builder.wgpu();
        }
        let harness = builder.build_eframe(move |_cc| app);
        let mut driver = Self { harness, tx, snapshots };
        driver.frames(4);
        driver
    }

    fn frames(&mut self, count: usize) {
        for _ in 0..count {
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
        }
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let (request, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(request).expect("control channel");
        for _ in 0..600 {
            self.frames(1);
            if let Ok(value) = reply.try_recv() {
                assert_eq!(value["ok"], true, "{method} {params}: {value}");
                return value["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }

    fn rect(&mut self, id: &str) -> [f64; 4] {
        self.frames(2);
        let value = self.ok("ui.elements", json!({"prefix": id}));
        let element = value.as_array().expect("elements").iter().find(|entry| entry["id"] == id).unwrap_or_else(|| panic!("missing {id}: {value}"));
        let coordinates = element["rect"].as_array().expect("rect");
        [0, 1, 2, 3].map(|index| coordinates[index].as_f64().expect("coordinate"))
    }

    fn snapshot(&mut self, name: &str) {
        let Some(directory) = self.snapshots.clone() else { return };
        std::fs::create_dir_all(&directory).expect("snapshot directory");
        self.frames(2);
        match self.harness.render() {
            Ok(image) => image.save(directory.join(format!("script-{name}.png"))).expect("save script snapshot"),
            Err(error) => eprintln!("script snapshot skipped: {error}"),
        }
    }
}

#[test]
fn wrapped_line_and_candle_share_height_and_find_chooses_a_take() {
    let mut driver = Driver::new();
    let source = driver.harness.state_mut().session.project.items.values().find(|item| matches!(item.kind, ItemKind::Media(_))).expect("demo media").id.0;
    let long = "This is a deliberately long screenplay line. ".repeat(12);
    let mut text = format!("{long}\nA second, short line.");
    for index in 0..45 {
        text.push_str(&format!("\nScene filler {index}"));
    }
    text.push_str("\nFind this distinctive final scene");
    let script = driver.exec("script.importText", json!({"name": "Script QA", "text": text}))["item"].as_u64().expect("script id");
    let doc = driver.exec("script.inspect", json!({"item": script}));
    let first = doc["document"]["lines"][0]["id"].as_u64().expect("first line");
    let second = doc["document"]["lines"][1]["id"].as_u64().expect("second line");
    let last = doc["document"]["lines"].as_array().expect("lines").last().expect("last")["id"].as_u64().expect("last line id");
    let take = driver.exec("script.addTake", json!({"item": script, "source": source, "startLine": first, "endLine": second}))["take"].as_u64().expect("take");
    driver.ok("ui.menu.invoke", json!({"id": "window.workspace.script"}));
    driver.ok("ui.menu.invoke", json!({"id": "script.openPanel", "params": {"item": script}}));
    driver.frames(4);
    let row = driver.rect(&format!("script.line.{first}"));
    let candle = driver.rect(&format!("script.take.{take}.line.{first}"));
    assert!(row[3] > 30.0, "long logical line should wrap: {row:?}");
    assert!((row[1] - candle[1]).abs() < 1.0 && (row[3] - candle[3]).abs() < 1.0, "text and candle must stay aligned: {row:?} {candle:?}");
    driver.snapshot("wrapped-dark");
    driver.ok("ui.set", json!({"theme": "light"}));
    driver.snapshot("wrapped-light");
    let second_take =
        driver.exec("script.addTake", json!({"item": script, "source": source, "startLine": second, "endLine": second}))["take"].as_u64().expect("second take");
    let result = driver.ok("ui.menu.invoke", json!({"id": "script.findInScript", "params": {"item": source}}));
    assert_eq!(result["matches"], 2);
    assert_eq!(result["chooser"], true);
    driver.frames(3);
    let _ = driver.rect("script.search");
    assert_ne!(take, second_take);
    driver.ok("ui.click", json!({"id": "script.search"}));
    driver.ok("ui.type", json!({"text": "distinctive final scene"}));
    driver.frames(4);
    let found = driver.rect(&format!("script.line.{last}"));
    assert!(found[3] >= 30.0, "search should scroll the matching logical line into view: {found:?}");
    driver.snapshot("search-light");
}

#[test]
fn narrow_script_panel_keeps_take_controls_accessible() {
    let mut driver = Driver::with_size(egui::vec2(620.0, 700.0));
    let source = driver.harness.state_mut().session.project.items.values().find(|item| matches!(item.kind, ItemKind::Media(_))).expect("demo media").id.0;
    let script =
        driver.exec("script.importText", json!({"name": "Narrow Script", "text": "First line\nSecond line\nThird line"}))["item"].as_u64().expect("script");
    let doc = driver.exec("script.inspect", json!({"item": script}));
    let first = doc["document"]["lines"][0]["id"].as_u64().expect("first line");
    let third = doc["document"]["lines"][2]["id"].as_u64().expect("third line");
    let take = driver.exec("script.addTake", json!({"item": script, "source": source, "startLine": first, "endLine": third}))["take"].as_u64().expect("take");
    driver.ok("ui.menu.invoke", json!({"id": "window.workspace.script"}));
    driver.ok("ui.menu.invoke", json!({"id": "script.openPanel", "params": {"item": script}}));
    driver.harness.state_mut().ui.script.selected_lines = Some((first, first));
    driver.harness.state_mut().ui.script.selected_take = Some(take);
    driver.frames(4);
    let _ = driver.rect("script.labels.set");
    let _ = driver.rect("script.take.bottomDown");
    driver.snapshot("narrow-controls");
}

#[test]
fn opening_script_panel_preserves_workspace_and_empty_state_has_actions() {
    let mut driver = Driver::new();
    driver.ok("ui.menu.invoke", json!({"id": "window.panel.Script"}));
    let state = driver.ok("ui.inspect", json!({}));
    assert_eq!(state["ui"]["workspace"], "Editing");
    let _ = driver.rect("script.new");
    let _ = driver.rect("script.import");
    driver.ok("ui.menu.invoke", json!({"id": "window.workspace.script"}));
    driver.snapshot("empty-dark");
}
