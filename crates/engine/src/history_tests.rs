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

/// The command ids stored with the undo and redo steps, oldest first.
fn origins(s: &Session) -> (Vec<Option<&str>>, Vec<Option<&str>>) {
    (s.history.undo.iter().map(|h| h.2.as_deref()).collect(), s.history.redo.iter().map(|h| h.2.as_deref()).collect())
}

/// Rename the project's root bin: an undoable edit made without a command.
fn rename(s: &mut Session, name: &str) -> crate::Result<()> {
    s.edit("Rename", |p, _| {
        p.root.name = name.to_string();
        Ok(())
    })
}

/// Each undo step keeps the id of the command that made it (the History panel draws its icon),
/// through undo and redo, the history limit, merged and grouped steps. An edit made outside a
/// command has none.
#[test]
fn undo_steps_keep_the_command_that_made_them() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.history.clear();
    s.execute("file.newBin", json!({"name": "A"})).unwrap();
    s.execute("markers.add", json!({})).unwrap();
    assert_eq!(origins(&s), (vec![Some("file.newBin"), Some("markers.add")], vec![]));

    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(origins(&s), (vec![Some("file.newBin")], vec![Some("markers.add")]), "undo keeps the command");
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.redo", json!({})).unwrap();
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(origins(&s), (vec![Some("file.newBin"), Some("markers.add")], vec![]), "redo keeps the command");

    rename(&mut s, "Direct").unwrap();
    assert_eq!(origins(&s).0.last(), Some(&None), "an edit outside a command has no command");
    assert_eq!(s.command_origin, None, "the command is forgotten when it ends");

    s.history.limit = 3;
    s.execute("file.newBin", json!({"name": "B"})).unwrap();
    assert_eq!(origins(&s).0, vec![Some("markers.add"), None, Some("file.newBin")], "the oldest step goes, the rest keep theirs");

    s.command_origin = Some("test.merged".into());
    for name in ["M1", "M2"] {
        s.edit_merged("Merged", "merge-key", |p, _| {
            p.root.name = name.to_string();
            Ok(())
        })
        .unwrap();
    }
    assert_eq!(origins(&s).0.last(), Some(&Some("test.merged")), "a merged step keeps its command");

    s.command_origin = Some("test.group".into());
    let steps = s.history.undo.len();
    s.edit_group("Group", |s| {
        rename(s, "G1")?;
        rename(s, "G2")
    })
    .unwrap();
    s.command_origin = None;
    assert_eq!(s.history.undo.len(), steps, "the group is one step and the limit drops the oldest");
    assert_eq!(origins(&s).0.last(), Some(&Some("test.group")), "a grouped step keeps its command");
    assert_eq!(s.history.undo.last().map(|h| h.0.as_str()), Some("Group"));
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
