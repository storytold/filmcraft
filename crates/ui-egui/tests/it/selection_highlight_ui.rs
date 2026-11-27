//! Selected items in the Project bin show a visible highlight (#1445): the List view's rows and
//! the Icon view's cards expose a `…selected` marker while their item is in the selection, and
//! lose it again when the selection goes away.

use crate::support::{Drive, Launch, demo_footage, driver};
use serde_json::json;

driver!(Driver);

impl Driver {
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

    /// The demo's Footage bin and its media items.
    fn footage(&mut self) -> (u64, Vec<u64>) {
        demo_footage(self.app())
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
