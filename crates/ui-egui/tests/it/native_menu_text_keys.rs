//! The macOS menu bar takes the Edit menu's key equivalents before the window does: with a text
//! field focused they edit the field, and only without one do they run the project command.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::{Sender, channel};

use crate::support::{Drive, build, builder, demo_session, driver, start};
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::menus::{self, MenuItem};
use serde_json::json;

driver! {
    Driver {
        menu: Sender<String>,
        sent: Rc<RefCell<Vec<MenuItem>>>,
    }
}

impl Driver {
    fn new() -> Self {
        let s = demo_session();
        let (tx, rx) = channel();
        let (menu, inbox) = channel();
        let sent: Rc<RefCell<Vec<MenuItem>>> = Rc::default();
        let sink = sent.clone();
        let mut app = FilmcraftApp::new(s).with_control(rx);
        app.command_inbox = Some(inbox);
        app.hooks.menu_changed = Some(Box::new(move |items: &[MenuItem]| *sink.borrow_mut() = items.to_vec()));
        let harness = build(builder(), app);
        start(Driver { harness, tx, menu, sent })
    }

    /// A native menu choice, then the frames that deliver what it queued for the window.
    fn menu(&mut self, id: &str) {
        self.menu.send(id.into()).unwrap();
        self.frames(4);
    }

    fn undo_steps(&mut self) -> usize {
        self.ok("engine.execute", json!({"command": "history.list", "params": {}}))["undo"].as_array().unwrap().len()
    }

    fn enabled(&self, id: &str) -> Option<bool> {
        self.sent.borrow().iter().find(|it| it.id == id).map(|it| it.enabled)
    }

    fn search(&mut self) -> String {
        self.harness.state().ui.project_search.clone()
    }
}

#[test]
fn edit_menu_keys_edit_the_focused_text_field_and_otherwise_the_project() {
    let mut d = Driver::new();
    d.ok("engine.execute", json!({"command": "markers.markIn", "params": {"seconds": 1.0}}));
    let steps = d.undo_steps();
    assert!(steps > 0);
    assert_eq!(d.enabled("edit.paste"), Some(false), "nothing copied in the project");

    let elements = d.ok("ui.elements", json!({"prefix": "project"}));
    let field = elements.as_array().unwrap().iter().filter_map(|e| e["id"].as_str()).find(|id| id.ends_with(".search")).unwrap().to_string();
    d.ok("ui.click", json!({"id": field}));
    d.ok("ui.type", json!({"text": "abc"}));
    d.frames(3);
    assert_eq!(d.search(), "abc");
    assert_eq!(menus::text_field_window(&d.harness.ctx), Some(egui::ViewportId::ROOT));
    assert_eq!(d.enabled("edit.paste"), Some(true), "Paste stays enabled for the field");
    assert!(menus::edits_text("edit.paste") && !menus::edits_text("file.newProject"));

    d.menu("edit.selectAll");
    d.menu("edit.cut");
    assert_eq!(d.search(), "", "Select All and Cut act on the field");
    d.menu("edit.undo");
    assert_eq!(d.search(), "abc", "Undo undoes the field's cut");
    assert_eq!(d.undo_steps(), steps, "the project history is untouched");

    d.harness.ctx.memory_mut(|m| m.stop_text_input());
    d.frames(3);
    assert_eq!(menus::text_field_window(&d.harness.ctx), None);
    d.menu("edit.undo");
    assert_eq!(d.undo_steps(), steps - 1, "without a focused field, Undo undoes the project");
}
