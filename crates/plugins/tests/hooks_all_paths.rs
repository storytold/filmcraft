//! With a plug-in `before` hook that vetoes an edit, every way a user can reach the edit
//! behaves as the command-layer contract says: the edit does not happen, the document and its
//! revision are untouched, and the user hears the plug-in's reason.
//!
//! The paths are the real ones, driven through a real `FilmcraftApp` in a kittest harness:
//!
//! | Path | How |
//! |---|---|
//! | menu command | control method `ui.menu.invoke` (the same `menus::invoke` the menu bar calls) |
//! | keyboard | control method `ui.key` pressing the command's bound shortcut |
//! | control server | control method `engine.execute` |
//! | undo and redo | the three paths above on `edit.undo` and `edit.redo` |
//!
//! The same actions run once with the plug-in disabled. Without that, "nothing changed" could
//! also mean "the path never reached the command".

mod common;

use common::driver::Driver;
use common::guests::{VETO, veto_package};
use common::install;
use serde_json::{Value, json};

fn vetoed(reply: &Value) -> bool {
    reply["ok"] == json!(false) && reply["error"].as_str().is_some_and(|e| e.contains("vetoed by"))
}

/// The plug-in vetoes these ids.
const GATED: [&str; 3] = ["file.newBin", "edit.undo", "edit.redo"];

fn driver_with_gate() -> Driver {
    let d = Driver::new();
    install(&d.host, &veto_package(&GATED));
    d
}

fn set_gate(d: &mut Driver, on: bool) {
    if on {
        d.host.manager.enable(VETO).unwrap();
    } else {
        d.host.manager.disable(VETO).unwrap();
    }
}

#[test]
fn a_vetoed_edit_changes_nothing_by_menu_key_and_control() {
    let mut d = driver_with_gate();
    assert_eq!(d.session().shortcuts.primary("file.newBin").as_deref(), Some("Cmd+B"), "the key press below must hit this command");
    let before = d.doc();

    // `ui.menu.invoke` runs in the UI pass; the plug-in notice is pumped by the next frame's logic pass.
    let status_after = |d: &mut Driver| {
        d.frames(2);
        Some(d.harness.state().ui.status.clone()).filter(|m| !m.is_empty())
    };

    // menu command
    let r = d.call("ui.menu.invoke", json!({"id": "file.newBin", "params": {"name": "by menu"}}));
    assert!(vetoed(&r), "{r}");
    assert_eq!(d.doc(), before, "menu path: nothing changed");
    let t = status_after(&mut d).expect("the user hears why (menu)");
    assert!(t.contains("blocked file.newBin") && t.contains("vetoed by the test plug-in"), "{t}");
    d.harness.state_mut().ui.status.clear();

    // control server
    let r = d.call("engine.execute", json!({"command": "file.newBin", "params": {"name": "by control"}}));
    assert!(vetoed(&r), "{r}");
    assert_eq!(d.doc(), before, "control path: nothing changed");
    let t = status_after(&mut d).expect("the user hears why (control)");
    assert!(t.contains("blocked file.newBin"), "{t}");
    d.harness.state_mut().ui.status.clear();

    // keyboard: the shortcut of the command
    d.key("B", true);
    assert_eq!(d.doc(), before, "keyboard path: nothing changed");
    let t = status_after(&mut d).expect("the user hears why (keyboard)");
    assert!(t.contains("blocked file.newBin"), "{t}");
}

#[test]
fn the_same_three_paths_edit_when_the_plugin_is_off() {
    let mut d = driver_with_gate();
    set_gate(&mut d, false);
    let (b0, r0, u0, _) = d.doc();

    d.ok("ui.menu.invoke", json!({"id": "file.newBin", "params": {"name": "by menu"}}));
    let (b1, r1, u1, _) = d.doc();
    assert_ne!(b1, b0);
    assert!(r1 > r0);
    assert_eq!(u1, u0 + 1);

    d.ok("engine.execute", json!({"command": "file.newBin", "params": {"name": "by control"}}));
    let (b2, _, u2, _) = d.doc();
    assert_ne!(b2, b1);
    assert_eq!(u2, u1 + 1);

    d.key("B", true);
    let (b3, _, u3, _) = d.doc();
    assert_ne!(b3, b2, "the key press reaches file.newBin when nothing vetoes it");
    assert_eq!(u3, u2 + 1);
}

#[test]
fn a_vetoed_undo_and_redo_change_nothing_by_menu_key_and_control() {
    let mut d = driver_with_gate();
    // Set up a step to undo, and then a step to redo, with the gate off.
    set_gate(&mut d, false);
    d.ok("engine.execute", json!({"command": "file.newBin", "params": {"name": "to undo"}}));
    let after_edit = d.doc();
    set_gate(&mut d, true);

    // undo is vetoed on every path
    let r = d.call("ui.menu.invoke", json!({"id": "edit.undo"}));
    assert!(vetoed(&r), "{r}");
    assert_eq!(d.doc(), after_edit, "menu undo");
    let r = d.call("engine.execute", json!({"command": "edit.undo", "params": {}}));
    assert!(vetoed(&r), "{r}");
    assert_eq!(d.doc(), after_edit, "control undo");
    assert_eq!(d.session().shortcuts.primary("edit.undo").as_deref(), Some("Cmd+Z"));
    d.key("Z", true);
    assert_eq!(d.doc(), after_edit, "keyboard undo");

    // undo for real (gate off), then redo is vetoed on every path
    set_gate(&mut d, false);
    d.ok("engine.execute", json!({"command": "edit.undo", "params": {}}));
    let after_undo = d.doc();
    assert_eq!(after_undo.3, 1, "one redo step is waiting");
    assert_eq!(after_undo.2 + 1, after_edit.2);
    set_gate(&mut d, true);

    let r = d.call("ui.menu.invoke", json!({"id": "edit.redo"}));
    assert!(vetoed(&r), "{r}");
    assert_eq!(d.doc(), after_undo, "menu redo");
    let r = d.call("engine.execute", json!({"command": "edit.redo", "params": {}}));
    assert!(vetoed(&r), "{r}");
    assert_eq!(d.doc(), after_undo, "control redo");
    assert_eq!(d.session().shortcuts.primary("edit.redo").as_deref(), Some("Cmd+Shift+Z"));
    d.call("ui.key", json!({"key": "Z", "ctrl": true, "shift": true}));
    d.frames(4);
    assert_eq!(d.doc(), after_undo, "keyboard redo");

    // and redo works when the gate is off, on the key path
    set_gate(&mut d, false);
    d.call("ui.key", json!({"key": "Z", "ctrl": true, "shift": true}));
    d.frames(4);
    let redone = d.doc();
    assert_eq!(redone.2, after_edit.2, "redo put the step back");
    assert_eq!(redone.0, after_edit.0);
}

#[test]
fn a_command_the_hook_does_not_match_is_not_vetoed() {
    let mut d = driver_with_gate();
    let r = d.call("engine.execute", json!({"command": "sequence.snap", "params": {"on": true}}));
    assert_eq!(r["ok"], json!(true), "{r}");
}
