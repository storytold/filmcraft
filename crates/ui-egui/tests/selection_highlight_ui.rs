//! Selected items in the Project bin show a visible highlight (#1445): the List view's rows and
//! the Icon view's cards expose a `…selected` marker while their item is in the selection, and
//! lose it again when the selection goes away.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_project::BinEntry;
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

    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }

    fn click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(2);
    }

    fn ids(&mut self, prefix: &str) -> Vec<String> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }

    fn has(&mut self, id: &str) -> bool {
        self.ids(id).iter().any(|i| i == id)
    }

    /// Step frames until `id` is registered.
    fn wait_for(&mut self, id: &str) {
        for _ in 0..400 {
            if self.has(id) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
            self.frames(1);
        }
        panic!("{id} never appeared: {:?}", self.ids(id.split('.').next().unwrap()));
    }

    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }

    /// The demo's Footage bin and its media items.
    fn footage(&mut self) -> (u64, Vec<u64>) {
        let p = &self.app().session.project;
        let bin = p
            .root
            .children
            .iter()
            .find_map(|e| match e {
                BinEntry::Bin(b) if b.name == "Footage" => Some(b.clone()),
                _ => None,
            })
            .expect("Footage bin");
        let items = bin.children.iter().filter_map(|e| if let BinEntry::Item(i) = e { Some(i.0) } else { None }).collect();
        (bin.id.0, items)
    }
}

/// The demo project with the Project panel maximized.
fn project_driver() -> Driver {
    let mut d = Driver::demo();
    d.ok("ui.panel.show", json!({"panel": "Project"}));
    d.ok("ui.set", json!({"focused": "Project"}));
    d.ok("ui.menu.invoke", json!({"id": "window.maximizeFrame"}));
    d.frames(3);
    d
}

/// A selected item's row / card is highlighted, the highlight follows the selection, and nothing
/// is highlighted when the bin has no selection (#1445).
#[test]
fn selected_items_are_highlighted_in_the_list_view() {
    let mut d = project_driver();
    d.exec("project.view.set", json!({"view": "list"}));
    let (bin, items) = d.footage();
    d.click(&format!("project.bin.{bin}.toggle"));
    d.wait_for(&format!("project.item.{}", items[0]));
    let mark = |i: u64| format!("project.item.{i}.selected");
    // nothing selected yet: no highlight anywhere
    assert!(d.ids("project.item.").into_iter().all(|id| !id.ends_with(".selected")), "no highlight without a selection");
    d.exec("project.select", json!({"items": [items[0]]}));
    d.frames(2);
    assert!(d.has(&mark(items[0])), "the selected row shows a highlight");
    // the highlight follows a new selection
    d.exec("project.select", json!({"items": [items[1]]}));
    d.frames(2);
    assert!(!d.has(&mark(items[0])), "the old row lost the highlight");
    assert!(d.has(&mark(items[1])), "the new row shows the highlight");
    // deselecting removes it
    d.exec("project.select", json!({"items": []}));
    d.frames(2);
    assert!(!d.has(&mark(items[1])), "deselecting removes the highlight");
}

/// The Icon view's cards highlight their selection the same way (#1445).
#[test]
fn selected_items_are_highlighted_in_the_icon_view() {
    let mut d = project_driver();
    d.exec("project.view.set", json!({"view": "icon"}));
    let (bin, items) = d.footage();
    // open the Footage bin in place so its cards are on show
    d.click(&format!("project.bin.{bin}"));
    d.ok("ui.menu.invoke", json!({"id": "projectPanel.openBin", "params": {"bin": bin, "alt": false}}));
    d.frames(3);
    let card = format!("project.item.{}", items[0]);
    d.wait_for(&card);
    let mark = format!("project.item.{}.selected", items[0]);
    assert!(!d.has(&mark), "no highlight without a selection");
    d.exec("project.select", json!({"items": [items[0]]}));
    d.frames(2);
    assert!(d.has(&mark), "the selected card shows a highlight");
    d.exec("project.select", json!({"items": []}));
    d.frames(2);
    assert!(!d.has(&mark), "deselecting removes the highlight");
}
