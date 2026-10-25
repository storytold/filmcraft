//! Window ▸ Workspaces, driven headless through the control channel: save a layout as a new
//! workspace, switch away and back, Reset to Saved Layout, save changes to a built-in and restore
//! it, rename and delete in the Edit Workspaces dialog, and the saved workspaces (and the one in use)
//! surviving a restart through `workspaces.json` in the data directory.
//!
//! Set `FILMCRAFT_UI_SNAPSHOT_DIR=<dir>` to also render offscreen with wgpu and write
//! `workspaces-*.png`.

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
    /// The demo project with its preferences (and workspaces) in `data`.
    fn launch(data: &Path) -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        session.prefs_path = Some(data.join("preferences.json"));
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let snapshots = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR").map(PathBuf::from);
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

    fn menu_err(&mut self, id: &str, params: Value) -> String {
        let v = self.call("ui.menu.invoke", json!({"id": id, "params": params}));
        assert_eq!(v["ok"], json!(false), "{id} {params} should fail: {v}");
        v["error"].as_str().unwrap_or_default().to_string()
    }

    fn click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(2);
    }

    /// Replace the text of the field `id`.
    fn retype(&mut self, id: &str, text: &str) {
        self.click(id);
        self.ok("ui.key", json!({"key": "A", "command": true}));
        self.ok("ui.type", json!({"text": text}));
        self.frames(2);
    }

    fn has(&mut self, id: &str) -> bool {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        v.as_array().unwrap().iter().any(|e| e["id"] == json!(id))
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

    /// Window ▸ Workspaces as (label, checked) in menu order.
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
    let d = std::env::temp_dir().join(format!("filmcraft-workspaces-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn saved_file(data: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(data.join("workspaces.json")).expect("workspaces.json written")).unwrap()
}

#[test]
fn save_switch_reset_rename_delete_and_restart() {
    let data = data_dir("cycle");
    let mut d = Driver::launch(&data);
    assert_eq!(d.workspace(), "Editing");
    assert!(d.shows(PanelKind::Metadata));

    // Save as New Workspace: the current layout under a new name, now the current workspace
    d.ok("ui.panel.close", json!({"panel": "Metadata"}));
    assert_eq!(d.menu("window.workspace.saveAs", json!({"name": "Cutting"}))["workspace"], "Cutting");
    assert_eq!(d.workspace(), "Cutting");
    assert_eq!(saved_file(&data)["saved"][0]["name"], "Cutting");
    let menu = d.workspaces_menu();
    let names: Vec<&str> = menu.iter().map(|(l, _)| l.as_str()).collect();
    assert_eq!(
        names,
        [
            "All Panels",
            "Assembly",
            "Audio",
            "Captions and Graphics",
            "Color",
            "Cutting",
            "Editing",
            "Effects",
            "Learning",
            "Review",
            "Reset to Saved Layout",
            "Save Changes to this Workspace",
            "Save as New Workspace…",
            "Edit Workspaces…"
        ],
        "one alphabetical list, then the layout commands"
    );
    assert!(menu.iter().any(|(l, c)| l == "Cutting" && *c), "the current workspace is checked: {menu:?}");
    d.snapshot("menu-list");

    // a name in use, an empty one or a command's name is refused
    assert!(d.menu_err("window.workspace.saveAs", json!({"name": "editing"})).contains("already exists"));
    assert!(d.menu_err("window.workspace.saveAs", json!({"name": "  "})).contains("needs a name"));
    assert!(d.menu_err("window.workspace.saveAs", json!({"name": "Reset"})).contains("can't be used"));

    // switch away and back (by menu and by ui.set): the saved layout comes back
    d.menu("window.workspace.color", json!({}));
    assert_eq!(d.workspace(), "Color");
    assert!(d.shows(PanelKind::LumetriColor));
    d.ok("ui.set", json!({"workspace": "cutting"}));
    assert_eq!(d.workspace(), "Cutting");
    assert!(!d.shows(PanelKind::Metadata) && !d.shows(PanelKind::LumetriColor));

    // Reset to Saved Layout undoes changes made since the save
    d.ok("ui.panel.close", json!({"panel": "Project"}));
    assert!(!d.shows(PanelKind::Project));
    d.menu("window.workspace.reset", json!({}));
    assert!(d.shows(PanelKind::Project) && !d.shows(PanelKind::Metadata));

    // Save Changes to this Workspace on a built-in; it can't be deleted, only restored
    d.ok("ui.set", json!({"workspace": "Assembly"}));
    d.ok("ui.panel.close", json!({"panel": "History"}));
    d.menu("window.workspace.saveChanges", json!({}));
    d.ok("ui.set", json!({"workspace": "Cutting"}));
    d.ok("ui.set", json!({"workspace": "Assembly"}));
    assert!(!d.shows(PanelKind::History), "the built-in keeps its saved changes");
    assert_eq!(d.menu("window.workspace.delete", json!({"name": "Assembly"}))["restored"], "Assembly");
    assert!(d.shows(PanelKind::History), "restoring the current built-in shows its original layout");
    assert!(d.menu_err("window.workspace.delete", json!({"name": "Assembly"})).contains("can't be deleted"));
    assert!(d.menu_err("window.workspace.rename", json!({"from": "Assembly", "to": "Mine"})).contains("keeps its name"));

    // rename follows the current workspace
    d.ok("ui.set", json!({"workspace": "Cutting"}));
    d.menu("window.workspace.rename", json!({"from": "Cutting", "to": "Fine Cut"}));
    assert_eq!(d.workspace(), "Fine Cut");
    assert!(d.call("ui.set", json!({"workspace": "Cutting"}))["ok"] == json!(false));
    drop(d);

    // restart: the saved workspaces and the one in use come back
    let mut d = Driver::launch(&data);
    assert_eq!(d.workspace(), "Fine Cut");
    assert!(!d.shows(PanelKind::Metadata));
    assert!(d.workspaces_menu().iter().any(|(l, c)| l == "Fine Cut" && *c));

    // delete the current user workspace: back to Editing, gone from the menu and the file
    assert_eq!(d.menu("window.workspace.delete", json!({"name": "Fine Cut"}))["deleted"], "Fine Cut");
    assert_eq!(d.workspace(), "Editing");
    assert!(!d.workspaces_menu().iter().any(|(l, _)| l == "Fine Cut"));
    assert_eq!(saved_file(&data)["saved"], json!([]));
    let _ = std::fs::remove_dir_all(&data);
}

#[test]
fn save_rename_and_delete_through_the_dialogs() {
    let data = data_dir("dialogs");
    let mut d = Driver::launch(&data);
    d.ok("ui.panel.close", json!({"panel": "Metadata"}));

    // Save as New Workspace… asks for a name
    assert_eq!(d.menu("window.workspace.saveAs", json!({}))["dialog"], "saveWorkspace");
    d.frames(2);
    d.retype("workspaces.save.name", "Mountain");
    d.snapshot("save-dialog");
    d.click("workspaces.save.ok");
    assert!(!d.has("workspaces.save.ok"), "the dialog closes");
    assert_eq!(d.workspace(), "Mountain");

    // Edit Workspaces…: a built-in can't be renamed or deleted; the user's own can
    d.menu("window.workspace.edit", json!({}));
    d.frames(2);
    let names = d.app().workspaces.saved.iter().map(|s| s.name.clone()).collect::<Vec<_>>();
    assert_eq!(names, ["Mountain"]);
    let row = |n: &str| {
        let all = filmcraft_ui_egui::dock::names(&Default::default());
        let mut all = all;
        all.push("Mountain".into());
        all.sort_by_key(|x| x.to_lowercase());
        format!("workspaces.edit.row.{}", all.iter().position(|x| x == n).unwrap())
    };
    d.click(&row("Editing"));
    d.click("workspaces.edit.delete");
    assert_eq!(d.workspace(), "Mountain", "a built-in with no saved changes can't be deleted");
    d.click(&row("Mountain"));
    d.retype("workspaces.edit.name", "Peak");
    d.snapshot("edit-dialog");
    d.click("workspaces.edit.rename");
    assert_eq!(d.workspace(), "Peak");
    assert_eq!(saved_file(&data)["saved"][0]["name"], "Peak");
    d.click("workspaces.edit.delete");
    assert_eq!(d.workspace(), "Editing");
    assert_eq!(saved_file(&data)["saved"], json!([]));
    d.frames(4); // the window shrinks to the shorter list
    d.click("workspaces.edit.close");
    assert!(d.app().ui.workspace_dialog.is_none(), "Close closes the dialog");
    let _ = std::fs::remove_dir_all(&data);
}
