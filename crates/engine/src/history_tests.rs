//! The History panel menu: `history.deleteRedoable` (Delete Redoable Actions) forgets the steps
//! Redo would bring back, and `history.clear` (Clear History) forgets every step. Neither one
//! changes the project or its unsaved state.

use serde_json::json;

use crate::Session;

/// The demo project with three labeled edits, then two of them undone.
fn three_edits_two_undone() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    for name in ["A", "B", "C"] {
        s.execute("file.newBin", json!({"name": name})).unwrap();
    }
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    s
}

#[test]
fn delete_redoable_actions_forgets_the_undone_steps_only() {
    let mut s = three_edits_two_undone();
    let before = s.execute("history.list", json!({})).unwrap();
    assert_eq!(before["redo"].as_array().unwrap().len(), 2, "{before}");
    let project = s.project.clone();
    let dirty = s.is_dirty();

    let r = s.execute("history.deleteRedoable", json!({})).unwrap();
    assert_eq!(r, json!({"deleted": 2}));
    let after = s.execute("history.list", json!({})).unwrap();
    assert_eq!(after["undo"], before["undo"], "the steps Undo takes back stay");
    assert_eq!(after["redo"], json!([]));
    assert_eq!(*s.project, *project, "the project does not change");
    assert_eq!(s.is_dirty(), dirty);
    assert!(s.execute("edit.redo", json!({})).is_err(), "nothing is left to redo");
    // Undo still takes back the step before
    s.execute("edit.undo", json!({})).unwrap();
    // with nothing to redo the command is disabled
    s.history.redo.clear();
    assert!(s.execute("history.deleteRedoable", json!({})).is_err());
}

#[test]
fn clear_history_forgets_every_step_and_keeps_the_project() {
    let mut s = three_edits_two_undone();
    let undo = s.history.undo.len();
    assert!(undo > 0);
    let project = s.project.clone();
    let dirty = s.is_dirty();

    let r = s.execute("history.clear", json!({})).unwrap();
    assert_eq!(r, json!({"deleted": undo + 2}));
    assert_eq!(s.execute("history.list", json!({})).unwrap(), json!({"undo": [], "redo": []}));
    assert_eq!(*s.project, *project, "the project does not change");
    assert_eq!(s.is_dirty(), dirty);
    assert!(s.execute("edit.undo", json!({})).is_err());
    assert!(s.execute("history.clear", json!({})).is_err(), "an empty history has nothing to clear");

    // a new edit starts a new history and can be undone
    s.execute("file.newBin", json!({"name": "D"})).unwrap();
    assert_eq!(s.history.undo.len(), 1);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(*s.project, *project);
}

#[test]
fn history_commands_ignore_unexpected_parameters() {
    for p in [json!(null), json!([1, 2]), json!({"deleted": -1}), json!("x")] {
        let mut s = three_edits_two_undone();
        let undo = s.history.undo.len();
        assert_eq!(s.execute("history.deleteRedoable", p.clone()).unwrap(), json!({"deleted": 2}), "{p}");
        assert_eq!(s.history.undo.len(), undo, "{p}");
        assert_eq!(s.execute("history.clear", p.clone()).unwrap(), json!({"deleted": undo}), "{p}");
        assert_eq!(s.execute("history.list", json!({})).unwrap(), json!({"undo": [], "redo": []}), "{p}");
    }
}
