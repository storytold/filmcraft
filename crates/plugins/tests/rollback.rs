//! A faulted or trapped plug-in call leaves the document unchanged.
//!
//! Every test drives a real `Session` (the demo project) and a real wasm guest through the real
//! `PluginHost`. The guest changes the document through `cmd.execute file.newBin`, which is the only
//! way a guest can. The checks compare the saved project bytes, not a flag.

mod common;

use std::sync::Arc;

use common::guests::{EDITS, edits_package};
use common::*;
use filmcraft_engine::project::Project;
use serde_json::{Value, json};

const CMD: &str = "plugin.test.edits";

/// A rig with the `edits` guest installed.
fn rig_with_edits() -> (filmcraft_engine::Session, Arc<filmcraft_plugins::PluginHost>) {
    let (session, host) = rig();
    install(&host, &edits_package());
    assert_active(&host, EDITS);
    (session, host)
}

fn run(session: &mut filmcraft_engine::Session, name: &str) -> Result<Value, filmcraft_engine::EngineError> {
    session.execute(&format!("{CMD}.{name}"), json!({}))
}

#[test]
fn a_trapping_command_that_already_edited_rolls_the_project_back_byte_for_byte() {
    let (mut session, host) = rig_with_edits();
    let before = project_bytes(&session);
    let undo_before = session.history.undo.len();
    let redo_before = session.history.redo.len();
    let revision_before = session.revision;
    let dirty_before = session.is_dirty();
    let sub = session.hooks.change_bus::<Arc<Project>>().subscribe();

    let result = run(&mut session, "trap");
    assert!(result.is_err(), "a trapping command must fail: {result:?}");

    // The edit really happened (two change events: the edit, then the revert) and is gone.
    let changes = session.hooks.change_bus::<Arc<Project>>().drain(sub);
    assert_eq!(changes.len(), 2, "one change for the guest's edit and one for the rollback: {changes:?}");
    assert_ne!(
        filmcraft_format::encode(&changes[0].snapshot, false),
        before,
        "the snapshot after the guest's edit differs from the original, so the guest did edit"
    );
    assert_eq!(project_bytes(&session), before, "the project JSON is the same bytes as before the call");
    assert_eq!(session.history.undo.len(), undo_before);
    assert_eq!(session.history.redo.len(), redo_before);
    assert_eq!(session.is_dirty(), dirty_before, "a clean document stays clean");
    assert!(session.revision > revision_before, "the revert is a change the feed reports");

    host.pump(&mut session);
    let events = session.drain_events();
    let toast = events.iter().find_map(|e| match e {
        filmcraft_engine::Event::Toast { message, error: true } => Some(message.clone()),
        _ => None,
    });
    let toast = toast.expect("the user is told");
    assert!(toast.contains("changes were undone"), "{toast}");
}

#[test]
fn a_plugin_error_answer_undoes_its_edits_too() {
    let (mut session, _host) = rig_with_edits();
    let before = project_bytes(&session);
    let undo_before = session.history.undo.len();
    let result = run(&mut session, "error");
    assert!(result.is_err());
    assert_eq!(project_bytes(&session), before);
    assert_eq!(session.history.undo.len(), undo_before);
}

#[test]
fn a_successful_command_keeps_its_edit() {
    let (mut session, _host) = rig_with_edits();
    let before = project_bytes(&session);
    let undo_before = session.history.undo.len();
    let result = run(&mut session, "ok").unwrap();
    assert_eq!(result, json!({"ok": true}));
    assert_ne!(project_bytes(&session), before, "the edit stays");
    assert_eq!(session.history.undo.len(), undo_before + 1);
}

#[test]
fn two_edits_in_one_plugin_command_are_two_undo_steps_each_one_per_cmd_execute() {
    // Contract: each `cmd.execute` is one undo step.
    let (mut session, _host) = rig_with_edits();
    let undo_before = session.history.undo.len();
    run(&mut session, "two").unwrap();
    assert_eq!(session.history.undo.len(), undo_before + 2);
}

#[test]
fn undo_after_a_plugin_edit_restores_the_original_project() {
    let (mut session, _host) = rig_with_edits();
    let before = project_bytes(&session);
    run(&mut session, "ok").unwrap();
    session.execute("edit.undo", json!({})).unwrap();
    assert_eq!(project_bytes(&session), before);
}

#[test]
fn the_redo_stack_comes_back_after_a_rollback() {
    let (mut session, _host) = rig_with_edits();
    // Make one redo step, so the guest's edit has something to clear.
    session.execute("file.newBin", json!({"name": "mine"})).unwrap();
    session.execute("edit.undo", json!({})).unwrap();
    assert!(session.history.can_redo());
    let before = project_bytes(&session);
    let redo_before = session.history.redo.len();
    assert!(run(&mut session, "trap").is_err());
    assert_eq!(project_bytes(&session), before);
    assert_eq!(session.history.redo.len(), redo_before, "an edit clears redo; the rollback must bring it back");
    // And redo still works on the restored stack.
    session.execute("edit.redo", json!({})).unwrap();
    assert_ne!(project_bytes(&session), before);
}

#[test]
fn a_dirty_document_stays_dirty_and_a_clean_one_stays_clean() {
    let (mut session, _host) = rig_with_edits();
    session.execute("file.newBin", json!({"name": "dirty it"})).unwrap();
    assert!(session.is_dirty());
    assert!(run(&mut session, "trap").is_err());
    assert!(session.is_dirty(), "the user's own unsaved edit is still unsaved");
}

#[test]
fn a_command_that_never_edits_takes_no_checkpoint_and_changes_nothing() {
    let (mut session, host) = rig_with_edits();
    let before = project_bytes(&session);
    let revision = session.revision;
    let answer = run(&mut session, "list").unwrap();
    assert!(answer["commands"].is_array());
    assert_eq!(project_bytes(&session), before);
    assert_eq!(session.revision, revision, "no rollback, so no revision bump");
    host.pump(&mut session);
}
