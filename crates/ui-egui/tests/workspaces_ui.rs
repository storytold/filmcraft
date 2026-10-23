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
    fn rect(&mut self, id: &str) -> egui::Rect {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == id).unwrap_or_else(|| panic!("missing {id}"));
        let r = e["rect"].as_array().unwrap();
        egui::Rect::from_min_size(
            egui::pos2(r[0].as_f64().unwrap() as f32, r[1].as_f64().unwrap() as f32),
            egui::vec2(r[2].as_f64().unwrap() as f32, r[3].as_f64().unwrap() as f32),
        )
    }

    fn drag_panel(&mut self, panel: &str, to: egui::Pos2) {
        // The active sequence tab has its close button at the left of its title.
        let inset = if panel == "Timeline" { 40.0 } else { 10.0 };
        let from = self.rect(&format!("panel.tab.{panel}")).left_center() + egui::vec2(inset, 0.0);
        self.ok("ui.drag", json!({"from": {"x": from.x, "y": from.y}, "to": {"x": to.x, "y": to.y}, "steps": 16}));
        self.frames(25);
    }

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
fn dragging_effects_to_top_right_then_splitting_and_saving_survives_restart() {
    let data = data_dir("panel-drag");
    let mut d = Driver::launch(&data);
    d.ok("ui.panel.show", json!({"panel": "Effects"}));
    d.frames(3);
    let project = d.app().session.project.clone();
    let top_right = d.rect("panel.Properties");
    d.drag_panel("Effects", top_right.center());
    let moved = d.rect("panel.Effects");
    assert!(moved.min.x > 1000.0 && moved.min.y < 100.0, "Effects moved to top right: {moved:?}");
    assert!(d.app().ui.dock.is_visible(PanelKind::Effects));
    d.snapshot("effects-docked-top-right");

    let program = d.rect("panel.Program");
    d.drag_panel("Effects", egui::pos2(program.max.x - 8.0, program.center().y));
    let effects = d.rect("panel.Effects");
    let program = d.rect("panel.Program");
    assert!(effects.min.x > program.max.x, "edge drop creates a separate region");
    assert!(
        [PanelKind::Properties, PanelKind::EssentialGraphics, PanelKind::Text].into_iter().any(|p| d.app().ui.dock.is_visible(p)),
        "the old group restores a remaining panel"
    );
    let mut all = Vec::new();
    d.app().ui.dock.panels(&mut all);
    assert_eq!(all.iter().filter(|p| **p == PanelKind::Effects).count(), 1);
    assert!(std::sync::Arc::ptr_eq(&project, &d.app().session.project), "layout moves do not edit the project");
    d.snapshot("effects-split-right");
    let saved = d.app().ui.dock.clone();
    d.menu("window.workspace.saveChanges", json!({}));
    d.menu("window.workspace.audio", json!({}));
    d.menu("window.workspace.editing", json!({}));
    assert_eq!(d.app().ui.dock, saved);
    drop(d);
    let mut restarted = Driver::launch(&data);
    assert_eq!(restarted.app().ui.dock, saved);
    let _ = std::fs::remove_dir_all(data);
}

#[test]
fn panel_drag_cancel_and_outside_drop_leave_the_layout_unchanged() {
    let data = data_dir("panel-cancel");
    let mut d = Driver::launch(&data);
    d.ok("ui.panel.show", json!({"panel": "Effects"}));
    d.frames(3);
    let original = d.app().ui.dock.clone();
    let from = d.rect("panel.tab.Effects").left_center() + egui::vec2(10.0, 0.0);
    let to = d.rect("panel.Properties").center();
    d.harness.input_mut().events.extend([
        egui::Event::PointerMoved(from),
        egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE },
    ]);
    d.frames(2);
    d.harness.input_mut().events.push(egui::Event::PointerMoved(to));
    d.frames(3);
    assert!(d.has("dock.drop.Properties.tab"), "dragging highlights the destination");
    d.snapshot("panel-drop-preview");
    d.ok("ui.key", json!({"key": "Escape"}));
    d.harness.input_mut().events.push(egui::Event::PointerButton {
        pos: to,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    d.frames(3);
    assert_eq!(d.app().ui.dock, original);
    d.drag_panel("Effects", egui::pos2(-50.0, -50.0));
    assert_eq!(d.app().ui.dock, original);

    for params in [
        json!({}),
        json!({"panel": "Effects", "anchor": "unknown"}),
        json!({"panel": "Effects", "anchor": "Program", "zone": "bad"}),
        json!({"panel": 1, "anchor": "Program"}),
    ] {
        d.menu_err("window.dockPanel", params);
        assert_eq!(d.app().ui.dock, original);
    }
    let _ = std::fs::remove_dir_all(data);
}

#[test]
fn panel_headers_reorder_tabs_and_compact_grips_and_timeline_can_move() {
    let data = data_dir("panel-headers");
    let mut d = Driver::launch(&data);
    d.ok("ui.panel.show", json!({"panel": "Effects"}));
    d.menu("window.dockPanel", json!({"panel": "Effects", "anchor": "Properties"}));
    d.frames(3);
    let properties = d.rect("panel.tab.Properties");
    d.drag_panel("Effects", properties.left_center() + egui::vec2(2.0, 0.0));
    let mut panels = Vec::new();
    d.app().ui.dock.panels(&mut panels);
    assert_eq!(panels.iter().position(|p| *p == PanelKind::Effects).unwrap() + 1, panels.iter().position(|p| *p == PanelKind::Properties).unwrap());

    let program = d.rect("panel.Program");
    d.drag_panel("Tools", program.center());
    assert!(d.app().ui.dock.is_visible(PanelKind::Tools), "the compact Tools grip is draggable");
    let open = d.app().session.state.open_sequences.clone();
    let effects = d.rect("panel.Effects");
    d.drag_panel("Timeline", effects.center());
    assert!(d.app().ui.dock.is_visible(PanelKind::Timeline));
    assert_eq!(d.app().session.state.open_sequences, open, "moving Timeline keeps sequence tabs open and ordered");
    assert!(d.rect("panel.Timeline").min.x > 1000.0);
    let before = d.app().ui.dock.clone();
    let first = open[0].0;
    let second = d.ok("engine.execute", json!({"command": "file.newSequence", "params": {"name": "B"}}))["sequence"].as_u64().unwrap();
    d.frames(3);
    d.ok(
        "ui.drag",
        json!({"from": {"id": format!("timeline.tab.{second}"), "fx": 0.4}, "to": {"id": format!("timeline.tab.{first}"), "fx": 0.2}, "steps": 12}),
    );
    d.frames(25);
    assert_eq!(d.app().session.state.open_sequences.iter().map(|s| s.0).collect::<Vec<_>>(), [second, first]);
    assert_eq!(d.app().ui.dock, before, "sequence reordering in a mixed group must not move the Timeline panel");
    let _ = std::fs::remove_dir_all(data);
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
