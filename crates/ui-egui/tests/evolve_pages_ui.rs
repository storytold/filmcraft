//! The Evolve workspace set, driven headless through the control channel: switching
//! sets, the page bar and its buttons, the Window ▸ Workspaces menu listing the pages in page
//! order, the Deliver page showing Export mode, page commands from the Standard set, saved
//! workspaces staying with their set, and the set surviving a restart through `workspaces.json`.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use filmcraft_ui_egui::dock::{EVOLVE_PAGES, PanelKind, WorkspaceSet};
use filmcraft_ui_egui::state::Mode;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    fn launch(data: &Path) -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        session.prefs_path = Some(data.join("preferences.json"));
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
        assert_eq!(v["ok"], json!(true), "{method} {params}: {v}");
        v["result"].clone()
    }

    fn menu(&mut self, id: &str, params: Value) -> Value {
        self.ok("ui.menu.invoke", json!({"command": id, "params": params}))
    }

    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }

    fn workspace(&mut self) -> String {
        self.app().ui.workspace.clone()
    }

    /// Ids of the on-screen elements with a prefix, after a frame has drawn the current state.
    fn elements(&mut self, prefix: &str) -> Vec<String> {
        self.frames(2);
        let found = self.ok("ui.elements", json!({"prefix": prefix}));
        found.as_array().unwrap().iter().map(|e| e["id"].as_str().unwrap().to_string()).collect()
    }

    /// Window ▸ Workspaces as (id, label, checked) in menu order.
    fn workspaces_menu(&mut self) -> Vec<(String, String, bool)> {
        let items = self.ok("ui.menu.list", json!({}));
        items
            .as_array()
            .unwrap()
            .iter()
            .filter(|i| i["path"] == json!(["Window", "Workspaces"]))
            .map(|i| (i["id"].as_str().unwrap().to_string(), i["label"].as_str().unwrap().to_string(), i["checked"] == json!(true)))
            .collect()
    }
}

fn data_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("filmcraft-evolve-pages-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn page_bar_pages_menu_deliver_and_restart() {
    let data = data_dir("pages");
    let mut d = Driver::launch(&data);
    assert_eq!(d.workspace(), "Editing");
    assert!(d.elements("pages.").is_empty(), "no page bar in the Standard set");
    let menu = d.workspaces_menu();
    assert!(menu.iter().any(|(id, _, _)| id == "window.workspace.editing") && !menu.iter().any(|(id, _, _)| id.starts_with("window.page.")));

    // Window ▸ Workspace Set ▸ Evolve Pages: the Edit page, the page bar, the pages in
    // page order in Window ▸ Workspaces
    let r = d.menu("window.workspaceSet.evolve", json!({}));
    assert_eq!((r["workspaceSet"].as_str(), r["workspace"].as_str()), (Some("evolve"), Some("Edit")));
    assert_eq!(d.app().workspaces.set, WorkspaceSet::Evolve);
    // the set and its page are saved together
    let file: Value = serde_json::from_slice(&std::fs::read(data.join("workspaces.json")).unwrap()).unwrap();
    assert_eq!((file["set"].as_str(), file["current"].as_str()), (Some("evolve"), Some("Edit")));
    let ids = d.elements("pages.");
    for page in EVOLVE_PAGES {
        assert!(ids.contains(&format!("pages.{}", page.to_ascii_lowercase())), "{ids:?}");
    }
    assert!(ids.contains(&"pages.bar".to_string()));
    let menu = d.workspaces_menu();
    let pages: Vec<&str> = menu.iter().filter(|(id, _, _)| id.starts_with("window.page.")).map(|(_, l, _)| l.as_str()).collect();
    assert_eq!(pages, EVOLVE_PAGES, "{menu:?}");
    assert!(menu.iter().any(|(id, _, c)| id == "window.page.edit" && *c), "the Edit page is checked: {menu:?}");
    assert!(!menu.iter().any(|(id, _, _)| id == "window.workspace.editing"), "the Standard workspaces are the other set: {menu:?}");
    let set_menu: Vec<(String, bool)> = d
        .ok("ui.menu.list", json!({}))
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["path"] == json!(["Window", "Workspace Set"]))
        .map(|i| (i["id"].as_str().unwrap().to_string(), i["checked"] == json!(true)))
        .collect();
    assert_eq!(set_menu, [("window.workspaceSet.standard".to_string(), false), ("window.workspaceSet.evolve".to_string(), true)]);
    assert!(d.app().ui.dock.contains(PanelKind::EffectControls) && d.app().ui.dock.contains(PanelKind::Timeline));

    // a click on a page button switches the page; the Color page has Lumetri and the scopes
    d.ok("ui.click", json!({"id": "pages.color"}));
    d.frames(2);
    assert_eq!(d.workspace(), "Color");
    assert!(d.app().ui.dock.is_visible(PanelKind::LumetriColor) && d.app().ui.dock.is_visible(PanelKind::LumetriScopes));
    assert_ne!(d.app().ui.dock, filmcraft_ui_egui::dock::workspace("Color"), "the Evolve Color page, not the Standard Color workspace");

    // Deliver is the export settings and queue (Export mode); any other page returns to Edit mode
    d.menu("window.page.deliver", json!({}));
    assert_eq!((d.workspace().as_str(), d.app().ui.mode), ("Deliver", Mode::Export));
    d.ok("ui.set", json!({"workspace": "audio"}));
    assert_eq!((d.workspace().as_str(), d.app().ui.mode), ("Audio", Mode::Edit));
    assert!(d.app().ui.dock.is_visible(PanelKind::AudioTrackMixer));

    // ui.inspect reports the set and the set's workspaces
    let inspect = d.ok("ui.inspect", json!({}));
    assert_eq!(inspect["workspaceSet"], "evolve");
    assert_eq!(inspect["workspaces"].as_array().unwrap().len(), EVOLVE_PAGES.len());

    // a workspace saved on a page belongs to the Evolve set
    d.menu("window.workspace.saveAs", json!({"name": "Grade"}));
    assert_eq!(d.workspace(), "Grade");
    assert!(d.workspaces_menu().iter().any(|(id, l, c)| id == "window.workspace.grade" && l == "Grade" && *c));

    // a Standard workspace key (Alt+Shift+1 → window.workspace.editing) switches back to the Standard set
    let r = d.menu("window.workspace.editing", json!({}));
    assert_eq!((r["workspace"].as_str(), r["workspaceSet"].as_str()), (Some("Editing"), Some("standard")));
    assert_eq!(d.app().workspaces.set, WorkspaceSet::Standard);
    assert!(d.elements("pages.").is_empty(), "the page bar goes with the set");
    assert!(!d.workspaces_menu().iter().any(|(_, l, _)| l == "Grade"), "Grade stays with the Evolve set");
    // and a page command from the Standard set switches to the Evolve set
    d.menu("window.page.cut", json!({}));
    assert_eq!((d.workspace().as_str(), d.app().workspaces.set), ("Cut", WorkspaceSet::Evolve));
    // unknown names are errors, not crashes
    assert_eq!(d.call("ui.menu.invoke", json!({"command": "window.page.compositing", "params": {}}))["ok"], false);
    assert_eq!(d.call("ui.set", json!({"workspaceSet": "nonsense"}))["ok"], false);

    // the set and the page in use come back after a restart
    d.ok("ui.set", json!({"workspaceSet": "evolve", "workspace": "Color"}));
    let file: Value = serde_json::from_slice(&std::fs::read(data.join("workspaces.json")).unwrap()).unwrap();
    assert_eq!((file["set"].as_str(), file["current"].as_str()), (Some("evolve"), Some("Color")));
    assert_eq!(file["saved"][0]["set"], "evolve");
    drop(d);
    let mut d = Driver::launch(&data);
    d.frames(4);
    assert_eq!((d.workspace().as_str(), d.app().workspaces.set), ("Color", WorkspaceSet::Evolve));
    assert!(!d.elements("pages.").is_empty());
    // back to the Standard set: a page with a namesake keeps its name (the Standard Color
    // workspace), one without falls back to Editing
    d.ok("ui.set", json!({"workspaceSet": "standard"}));
    assert_eq!(d.workspace(), "Color");
    assert_eq!(d.app().ui.dock, filmcraft_ui_egui::dock::workspace("Color"));
    d.ok("ui.set", json!({"workspaceSet": "evolve", "workspace": "Cut"}));
    d.ok("ui.set", json!({"workspaceSet": "standard"}));
    assert_eq!(d.workspace(), "Editing", "Cut has no namesake in the Standard set");
    let _ = std::fs::remove_dir_all(&data);
}

#[test]
fn builtin_names_of_either_set_cannot_be_saved() {
    let data = data_dir("reserved");
    let mut d = Driver::launch(&data);
    // the Standard set: the Evolve pages' names are reserved too (a saved "Cut" would count as a
    // built-in workspace that can never be renamed or deleted)
    for name in ["Cut", "media", "Deliver"] {
        let r = d.call("ui.menu.invoke", json!({"command": "window.workspace.saveAs", "params": {"name": name}}));
        assert_eq!(r["ok"], false, "{name}: {r}");
        assert!(r["error"].as_str().unwrap_or_default().contains("built-in"), "{name}: {r}");
    }
    // and the Evolve set: the Standard workspaces' names, for Save as New and Rename
    d.menu("window.workspaceSet.evolve", json!({}));
    for name in ["Editing", "assembly"] {
        let r = d.call("ui.menu.invoke", json!({"command": "window.workspace.saveAs", "params": {"name": name}}));
        assert_eq!(r["ok"], false, "{name}: {r}");
    }
    d.menu("window.workspace.saveAs", json!({"name": "Grade"}));
    assert_eq!(d.call("ui.menu.invoke", json!({"command": "window.workspace.rename", "params": {"from": "Grade", "to": "Editing"}}))["ok"], false);
    assert!(d.app().workspaces.saved.iter().all(|s| s.name == "Grade"), "{:?}", d.app().workspaces.saved);
    let _ = std::fs::remove_dir_all(&data);
}

#[test]
fn workspace_set_switch_completes_when_the_file_cannot_be_written() {
    let data = data_dir("unwritable");
    // a directory where workspaces.json goes: every save fails
    std::fs::create_dir_all(data.join("workspaces.json")).unwrap();
    let mut d = Driver::launch(&data);
    let r = d.call("ui.menu.invoke", json!({"command": "window.workspaceSet.evolve", "params": {}}));
    assert_eq!(r["ok"], false, "the save error reaches the caller: {r}");
    // the set, the page and its layout still switched together
    assert_eq!((d.app().workspaces.set, d.workspace().as_str()), (WorkspaceSet::Evolve, "Edit"));
    assert_eq!(d.app().ui.dock, filmcraft_ui_egui::dock::evolve_page("Edit"));
    assert!(d.app().ui.status.contains("saving workspaces"), "{}", d.app().ui.status);
    // a page or workspace asked for by name still comes up (in its set), and the error is returned
    for (command, set, name) in [
        ("window.workspace.assembly", WorkspaceSet::Standard, "Assembly"),
        ("window.page.color", WorkspaceSet::Evolve, "Color"),
        ("window.workspace.learning", WorkspaceSet::Standard, "Learning"),
    ] {
        let r = d.call("ui.menu.invoke", json!({"command": command, "params": {}}));
        assert_eq!(r["ok"], false, "{command}: {r}");
        assert_eq!((d.app().workspaces.set, d.workspace().as_str()), (set, name), "{command}");
        assert_eq!(d.app().ui.dock, filmcraft_ui_egui::dock::workspace_in(set, name), "{command}");
    }
    let r = d.call("ui.set", json!({"workspaceSet": "evolve", "workspace": "Audio"}));
    assert_eq!(r["ok"], false, "{r}");
    assert_eq!((d.app().workspaces.set, d.workspace().as_str()), (WorkspaceSet::Evolve, "Audio"));
    // a page of the set in use: shown, and the failed save is reported too
    let r = d.call("ui.menu.invoke", json!({"command": "window.page.cut", "params": {}}));
    assert_eq!(r["ok"], false, "{r}");
    assert_eq!((d.app().workspaces.set, d.workspace().as_str()), (WorkspaceSet::Evolve, "Cut"));
    let _ = std::fs::remove_dir_all(&data);
}

#[test]
fn standard_workspace_ids_switch_sets_even_on_a_namesake_page() {
    let data = data_dir("namesake");
    let mut d = Driver::launch(&data);
    d.menu("window.page.edit", json!({}));
    // Window ▸ Workspaces ▸ Color (Alt+Shift+3) of the Standard set, though Evolve has a Color page
    let r = d.menu("window.workspace.color", json!({}));
    assert_eq!((r["workspace"].as_str(), r["workspaceSet"].as_str()), (Some("Color"), Some("standard")));
    assert_eq!((d.app().workspaces.set, d.workspace().as_str()), (WorkspaceSet::Standard, "Color"));
    assert_eq!(d.app().ui.dock, filmcraft_ui_egui::dock::workspace("Color"));
    // the Color page is `window.page.color`
    d.menu("window.page.color", json!({}));
    assert_eq!((d.app().workspaces.set, d.workspace().as_str()), (WorkspaceSet::Evolve, "Color"));
    assert_eq!(d.app().ui.dock, filmcraft_ui_egui::dock::evolve_page("Color"));
    // a workspace of the user's own in the Evolve set is still found by its id there
    d.menu("window.workspace.saveAs", json!({"name": "Grade"}));
    d.menu("window.page.edit", json!({}));
    let r = d.menu("window.workspace.grade", json!({}));
    assert_eq!(r["workspace"], "Grade");
    assert_eq!((d.app().workspaces.set, d.workspace().as_str()), (WorkspaceSet::Evolve, "Grade"));
    let _ = std::fs::remove_dir_all(&data);
}

#[test]
fn leaving_deliver_for_the_standard_set_returns_to_edit_mode() {
    let data = data_dir("deliver-standard");
    let mut d = Driver::launch(&data);
    // Window ▸ Workspace Set ▸ Standard Workspaces from the Deliver page
    d.menu("window.page.deliver", json!({}));
    assert_eq!((d.workspace().as_str(), d.app().ui.mode), ("Deliver", Mode::Export));
    d.ok("ui.set", json!({"workspaceSet": "standard"}));
    assert_eq!((d.app().workspaces.set, d.workspace().as_str(), d.app().ui.mode), (WorkspaceSet::Standard, "Editing", Mode::Edit));
    assert_eq!(d.app().ui.dock, filmcraft_ui_egui::dock::workspace("Editing"));
    // and through a Standard workspace's own command
    d.menu("window.page.deliver", json!({}));
    assert_eq!(d.app().ui.mode, Mode::Export);
    d.menu("window.workspace.assembly", json!({}));
    assert_eq!((d.app().workspaces.set, d.workspace().as_str(), d.app().ui.mode), (WorkspaceSet::Standard, "Assembly", Mode::Edit));
    assert_eq!(d.app().ui.dock, filmcraft_ui_egui::dock::workspace("Assembly"));
    let _ = std::fs::remove_dir_all(&data);
}
