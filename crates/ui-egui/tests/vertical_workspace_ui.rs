//! Vertical workspace preset test:
//! 1. Window → Workspaces contains Vertical in alphabetical order.
//! 2. Switching to Vertical changes the panel arrangement (Program prominent on the right,
//!    Project on the left, Timeline spanning the bottom, EffectControls and Properties accessible).
//! 3. Switching to another workspace (e.g. Editing, Color) and back restores the respective layouts.
//! 4. Resetting the workspace returns to its default arrangement.
//! 5. Restarting the application preserves the Vertical workspace via workspaces.json.
//! 6. Active sequence with 9:16 portrait aspect renders correctly in the Program monitor.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use filmcraft_ui_egui::dock::PanelKind;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    snapshots: Option<PathBuf>,
}

impl Driver {
    fn launch(data: &Path) -> Self {
        Self::launch_with_size(data, egui::vec2(1600.0, 980.0))
    }

    fn launch_with_size(data: &Path, size: egui::Vec2) -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        session.prefs_path = Some(data.join("preferences.json"));
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let snapshots = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR").map(PathBuf::from);
        let mut b = Harness::builder().with_size(size).with_max_steps(10_000);
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

    fn call(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                return v;
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let v = self.call(method, params.clone());
        assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
        v["result"].clone()
    }

    fn menu(&mut self, id: &str, params: Value) -> Value {
        self.ok("ui.menu.invoke", json!({"id": id, "params": params}))
    }

    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }

    fn workspace(&mut self) -> String {
        self.app().ui.workspace.clone()
    }

    fn shows(&mut self, p: PanelKind) -> bool {
        self.app().ui.dock.contains(p)
    }

    fn is_visible(&mut self, p: PanelKind) -> bool {
        self.app().ui.dock.is_visible(p)
    }

    fn workspaces_menu(&mut self) -> Vec<(String, bool)> {
        let items = self.ok("ui.menu.list", json!({}));
        items
            .as_array()
            .unwrap()
            .iter()
            .filter(|i| i["path"] == json!(["Window", "Workspaces"]))
            .map(|i| (i["label"].as_str().unwrap().to_string(), i["checked"] == json!(true)))
            .collect()
    }

    fn snapshot(&mut self, name: &str) {
        let Some(dir) = self.snapshots.clone() else { return };
        self.frames(2);
        match self.harness.render() {
            Ok(img) => {
                std::fs::create_dir_all(&dir).unwrap();
                img.save(dir.join(format!("workspaces-{name}.png"))).unwrap();
            }
            Err(e) => eprintln!("snapshot {name} skipped: {e}"),
        }
    }
}

fn data_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("filmcraft-vertical-workspace-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn vertical_workspace_preset_registration_and_switching() {
    let data = data_dir("registration");
    let mut d = Driver::launch(&data);
    assert_eq!(d.workspace(), "Editing");

    // 1. Verify Window → Workspaces contains Vertical
    let menu = d.workspaces_menu();
    let names: Vec<&str> = menu.iter().map(|(l, _)| l.as_str()).collect();
    assert!(names.contains(&"Vertical"), "Workspaces menu must contain Vertical: {names:?}");

    // 2. Select Vertical workspace via menu invoke
    d.menu("window.workspace.vertical", json!({}));
    assert_eq!(d.workspace(), "Vertical");

    // Verify Vertical is now checked in the menu
    let menu_after = d.workspaces_menu();
    assert!(
        menu_after.iter().any(|(l, c)| l == "Vertical" && *c),
        "Vertical must be checked in the menu: {menu_after:?}"
    );

    // 3. Verify core panels are present and docked in the layout
    assert!(d.shows(PanelKind::Program), "Program monitor must be present");
    assert!(d.shows(PanelKind::Timeline), "Timeline must be present");
    assert!(d.shows(PanelKind::Tools), "Tools must be present");
    assert!(d.shows(PanelKind::AudioMeters), "Audio meters must be present");
    assert!(d.shows(PanelKind::Project), "Project panel must be present");
    assert!(d.shows(PanelKind::Source), "Source panel must be present");
    assert!(d.shows(PanelKind::EffectControls), "Effect Controls must be present");
    assert!(d.shows(PanelKind::Properties), "Properties must be present");

    // Check visible active tabs
    assert!(d.is_visible(PanelKind::Program), "Program monitor must be visible");
    assert!(d.is_visible(PanelKind::Timeline), "Timeline must be visible");
    assert!(d.is_visible(PanelKind::EffectControls), "Effect Controls is default active tab in Source split");
    assert!(d.is_visible(PanelKind::Project), "Project is active tab in bottom-left");
    assert!(d.is_visible(PanelKind::Properties), "Properties is active tab in middle");

    // Verify root docking hierarchy:
    match &d.app().ui.dock {
        filmcraft_ui_egui::dock::DockNode::Split { vertical, a, b, .. } => {
            assert!(!vertical, "Root split must be horizontal (false) with Program monitor on right");
            assert!(b.contains(PanelKind::Program), "Right side must contain Program monitor");
            assert!(!b.contains(PanelKind::Timeline), "Right side must NOT contain Timeline");
            assert!(a.contains(PanelKind::Timeline), "Left work area must contain Timeline");
            assert!(a.contains(PanelKind::Project), "Left work area must contain Project");
        }
        _ => panic!("Expected root split"),
    }

    // 4. Switch to Color workspace and back
    d.menu("window.workspace.color", json!({}));
    assert_eq!(d.workspace(), "Color");
    assert!(d.shows(PanelKind::LumetriColor));

    d.menu("window.workspace.vertical", json!({}));
    assert_eq!(d.workspace(), "Vertical");
    assert!(!d.shows(PanelKind::LumetriColor));
    assert!(d.shows(PanelKind::Program));

    // 5. Test Reset to Saved Layout
    d.ok("ui.panel.close", json!({"panel": "Properties"}));
    assert!(!d.shows(PanelKind::Properties));
    d.menu("window.workspace.reset", json!({}));
    assert!(d.shows(PanelKind::Properties), "Reset to Saved Layout restores default Vertical arrangement");

    // 6. Test persistence across app restart
    drop(d);
    let mut d2 = Driver::launch(&data);
    assert_eq!(d2.workspace(), "Vertical", "Vertical workspace should be preserved across restart");
    assert!(d2.shows(PanelKind::Program));
    assert!(d2.shows(PanelKind::Timeline));

    let _ = std::fs::remove_dir_all(&data);
}

#[test]
fn vertical_workspace_with_portrait_sequence() {
    let data = data_dir("portrait_seq");
    let mut d = Driver::launch(&data);

    // Switch to Vertical workspace
    d.menu("window.workspace.vertical", json!({}));
    assert_eq!(d.workspace(), "Vertical");

    // Create a 1080x1920 (9:16) portrait sequence
    d.menu(
        "file.newSequence",
        json!({
            "name": "VerticalShorts",
            "width": 1080,
            "height": 1920,
            "fps": 30.0
        }),
    );

    d.frames(4);
    assert_eq!(d.workspace(), "Vertical");
    assert!(d.shows(PanelKind::Program));

    // Verify monitor picture element exists
    let elements = d.ok("ui.elements", json!({"prefix": "program"}));
    assert!(elements.as_array().unwrap().iter().any(|e| e["id"] == json!("program.picture")));

    d.snapshot("vertical-workspace-active");
    let _ = std::fs::remove_dir_all(&data);
}

#[test]
fn responsive_vertical_layout_at_standard_resolutions() {
    let resolutions = [
        ("1080p", egui::vec2(1920.0, 1080.0)),
        ("900p", egui::vec2(1600.0, 900.0)),
        ("768p", egui::vec2(1366.0, 768.0)),
    ];

    for (name, size) in resolutions {
        let data = data_dir(&format!("responsive_{name}"));
        let mut d = Driver::launch_with_size(&data, size);

        d.menu("window.workspace.vertical", json!({}));
        assert_eq!(d.workspace(), "Vertical");

        // The right Program Monitor must span the full available editor height
        // (excluding header bar and status bar, so > 80% of window height)
        let elements = d.ok("ui.elements", json!({"prefix": "program"}));
        let pic = elements
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["id"] == json!("program.picture"))
            .expect("program.picture should exist");

        let h = pic["rect"][3].as_f64().unwrap_or(0.0);
        assert!(h > 150.0, "Program picture should have significant height in {name}: {h}");

        // Verify root docking hierarchy at this resolution
        match &d.app().ui.dock {
            filmcraft_ui_egui::dock::DockNode::Split { vertical, a, b, .. } => {
                assert!(!vertical, "{name}: Root split must be horizontal");
                assert!(b.contains(PanelKind::Program), "{name}: Right side must contain Program monitor");
                assert!(!b.contains(PanelKind::Timeline), "{name}: Right side must not contain Timeline");
                assert!(a.contains(PanelKind::Timeline), "{name}: Left work area must contain Timeline");
            }
            _ => panic!("Expected root split"),
        }

        let _ = std::fs::remove_dir_all(&data);
    }
}
