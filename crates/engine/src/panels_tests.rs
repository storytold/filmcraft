//! Tests of the panel commands (`panels`): Events log, Metadata edits with undo.

use std::sync::Arc;

use filmcraft_project::{ItemId, Label};
use serde_json::{Value, json};

use crate::Session;
use crate::panels::Level;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

fn first_media(s: &Session) -> ItemId {
    s.project.items.values().filter(|i| i.as_media().is_some()).map(|i| i.id).min().expect("a media item")
}

fn field<'a>(v: &'a Value, name: &str) -> &'a Value {
    v["fields"].as_array().unwrap().iter().find(|f| f["name"] == name).unwrap_or_else(|| panic!("no field {name}: {v}"))
}

#[test]
fn metadata_edits_are_undoable_and_saved_in_the_project() {
    let mut s = demo();
    let id = first_media(&s);
    let name = s.project.item(id).unwrap().name.clone();
    let g = s.execute("metadata.get", json!({"item": id.0})).unwrap();
    assert_eq!(field(&g, "Name")["value"], json!(name));
    assert_eq!(field(&g, "Media Start")["editable"], json!(false));
    assert_eq!(field(&g, "Description")["value"], json!(""));
    for f in ["Frame Rate", "Media End", "Media Duration", "Video Info", "Video Codec", "Color Space", "File Path", "Scene", "Shot", "Log Note", "Tape Name"] {
        field(&g, f);
    }

    let undo0 = s.history.undo.len();
    s.execute("metadata.set", json!({"item": id.0, "field": "Description", "value": "Wide establishing shot"})).unwrap();
    assert_eq!(s.project.item(id).unwrap().metadata["Description"], "Wide establishing shot");
    assert_eq!(s.history.undo.len(), undo0 + 1);
    assert_eq!(s.history.undo.last().unwrap().0, "Edit Metadata");
    // several fields in one step, including the name and the label
    s.execute("metadata.set", json!({"item": id.0, "fields": {"Scene": "12", "Shot": "4B", "Name": "Hero Shot", "Label": "Mango"}})).unwrap();
    assert_eq!(s.history.undo.len(), undo0 + 2);
    let it = s.project.item(id).unwrap();
    assert_eq!((it.name.as_str(), it.label, it.metadata["Scene"].as_str(), it.metadata["Shot"].as_str()), ("Hero Shot", Label::Mango, "12", "4B"));
    // the same value again is not a new undo step
    s.execute("metadata.set", json!({"item": id.0, "field": "Scene", "value": "12"})).unwrap();
    assert_eq!(s.history.undo.len(), undo0 + 2);
    // undo / redo
    s.execute("edit.undo", json!({})).unwrap();
    let it = s.project.item(id).unwrap();
    assert_eq!((it.name.as_str(), it.metadata.get("Scene")), (name.as_str(), None));
    assert_eq!(it.metadata["Description"], "Wide establishing shot");
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(s.project.item(id).unwrap().name, "Hero Shot");
    // an empty value removes the field
    s.execute("metadata.set", json!({"item": id.0, "field": "Shot", "value": ""})).unwrap();
    assert!(!s.project.item(id).unwrap().metadata.contains_key("Shot"));
    // errors: read-only fields, bad labels, empty names; nothing changes
    let n = s.history.undo.len();
    for bad in [
        json!({"field": "Media Start", "value": "01:00:00:00"}),
        json!({"field": "Label", "value": "Plaid"}),
        json!({"field": "Name", "value": " "}),
        json!({}),
    ] {
        let mut p = bad.clone();
        p["item"] = json!(id.0);
        assert!(s.execute("metadata.set", p).is_err(), "{bad}");
    }
    assert_eq!(s.history.undo.len(), n);
    // saved in the project file and read back
    let bytes = filmcraft_format::encode(&s.project, false);
    let back = filmcraft_format::decode(&bytes).unwrap().project;
    assert_eq!(back.item(id).unwrap().metadata["Scene"], "12");
}

#[test]
fn metadata_targets_follow_the_selection() {
    let mut s = demo();
    let seq = s.active_sequence().unwrap().clone();
    let clip = &seq.video_tracks[0].items[0];
    // nothing selected in the Project panel: the timeline selection
    s.state.project_selection.clear();
    s.state.selection = vec![clip.id];
    let g = s.execute("metadata.get", json!({})).unwrap();
    assert_eq!(g["item"], json!(clip.item.0));
    // a timeline clip by id
    let g = s.execute("metadata.get", json!({"clip": clip.id.0})).unwrap();
    assert_eq!(g["item"], json!(clip.item.0));
    s.execute("metadata.set", json!({"clip": clip.id.0, "field": "Log Note", "value": "soft focus"})).unwrap();
    assert_eq!(s.project.item(clip.item).unwrap().metadata["Log Note"], "soft focus");
    // the Project panel selection wins
    let other = first_media(&s);
    s.state.project_selection = vec![other];
    assert_eq!(s.execute("metadata.get", json!({})).unwrap()["item"], json!(other.0));
    s.state.project_selection.clear();
    s.state.selection.clear();
    assert!(s.execute("metadata.get", json!({})).is_err());
}

#[test]
fn events_log_failed_commands_jobs_and_messages() {
    let mut s = demo();
    s.execute("events.clear", json!({})).unwrap();
    assert!(s.execute("clip.rename", json!({})).is_err());
    assert!(s.execute("clip.rename", json!({})).is_err());
    let l = s.execute("events.list", json!({})).unwrap();
    let e = &l["entries"].as_array().unwrap()[0];
    assert_eq!((e["level"].as_str(), e["source"].as_str(), e["count"].as_u64()), (Some("error"), Some("clip.rename"), Some(2)));
    assert!(e["message"].as_str().unwrap().contains("name"), "{e}");
    // a disabled command is a warning
    s.history.undo.clear();
    assert!(s.execute("edit.undo", json!({})).is_err());
    assert_eq!(s.log.entries.back().map(|e| (e.level, e.source.as_str())), Some((Level::Warning, "edit.undo")));
    // messages are info
    s.toast("Saved");
    s.error_toast("autosave", "disk full");
    let l = s.execute("events.list", json!({"level": "error"})).unwrap();
    assert!(l["entries"].as_array().unwrap().iter().all(|e| e["level"] == "error"));
    assert_eq!(l["counts"]["error"], json!(2));
    assert_eq!(l["counts"]["info"], json!(1));
    let last = l["entries"].as_array().unwrap().last().unwrap()["id"].as_u64().unwrap();
    assert!(s.execute("events.list", json!({"since": last})).unwrap()["entries"].as_array().unwrap().is_empty());
    // nested commands report only the outer failure; queries and successes add nothing
    let before = s.log.entries.len();
    s.execute("project.inspect", json!({})).unwrap();
    assert_eq!(s.log.entries.len(), before);
    s.execute("events.clear", json!({})).unwrap();
    assert!(s.log.entries.is_empty());
}

#[test]
fn events_report_background_jobs() {
    let mut s = demo();
    s.execute("events.clear", json!({})).unwrap();
    let job = |id, label: &str| crate::Job { id, label: label.into(), progress: Arc::new(Default::default()), result: Arc::new(std::sync::Mutex::new(None)) };
    let (a, b, c) = (job(901, "Export a.mp4"), job(902, "Create Proxies"), job(903, "Scene Edit Detection"));
    s.jobs.extend([a.clone(), b.clone(), c.clone()]);
    s.poll_persistence();
    assert_eq!(s.log.entries.iter().filter(|e| e.message.starts_with("Started:")).count(), 3);
    *a.result.lock().unwrap() =
        Some(Ok(filmcraft_export::Report { path: "a.mp4".into(), frames: 1, seconds: 0.1, bytes: 1, render_fps: 10.0, extra_files: Vec::new() }));
    *b.result.lock().unwrap() = Some(Err("disk full".into()));
    c.progress.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    *c.result.lock().unwrap() = Some(Err("cancelled".into()));
    let l = s.execute("events.list", json!({})).unwrap();
    let msgs: Vec<(String, String)> =
        l["entries"].as_array().unwrap().iter().map(|e| (e["level"].as_str().unwrap().into(), e["message"].as_str().unwrap().into())).collect();
    assert!(msgs.contains(&("info".into(), "Finished: Export a.mp4".into())), "{msgs:?}");
    assert!(msgs.contains(&("error".into(), "Failed: Create Proxies: disk full".into())), "{msgs:?}");
    assert!(msgs.contains(&("warning".into(), "Cancelled: Scene Edit Detection".into())), "{msgs:?}");
    // reported once
    s.poll_persistence();
    assert_eq!(s.log.entries.len(), 6);
}

/// #620: project notes are saved in the project file; typing (one `merge` key) is one undo step.
#[test]
fn project_notes_are_saved_and_typing_is_one_undo_step() {
    let mut s = demo();
    assert_eq!(s.execute("project.notes", json!({})).unwrap()["text"], "");
    let undo0 = s.history.undo.len();
    for text in ["T", "To", "To do: fix the logo at 00:01:02:03"] {
        s.execute("project.setNotes", json!({"text": text, "merge": "1"})).unwrap();
    }
    assert_eq!(s.history.undo.len(), undo0 + 1, "one typing session, one undo step");
    assert_eq!(s.history.undo.last().unwrap().0, "Edit Project Notes");
    // the same text again changes nothing and adds no step
    let r = s.execute("project.setNotes", json!({"text": "To do: fix the logo at 00:01:02:03"})).unwrap();
    assert_eq!(r["changed"], json!(false));
    assert_eq!(s.history.undo.len(), undo0 + 1);

    let loaded = filmcraft_format::decode(&filmcraft_format::encode(&s.project, false)).unwrap();
    assert_eq!(loaded.project.notes, "To do: fix the logo at 00:01:02:03");
    // an older file without notes still loads, with none
    let mut doc: Value = serde_json::from_slice(&filmcraft_format::encode(&filmcraft_project::Project::new("old"), false)).unwrap();
    doc["schema_version"] = json!(14);
    assert_eq!(filmcraft_format::decode(&serde_json::to_vec(&doc).unwrap()).unwrap().project.notes, "");

    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.notes, "");
}

#[test]
fn project_notes_refuse_bad_input() {
    let mut s = demo();
    assert!(s.execute("project.setNotes", json!({})).is_err());
    assert!(s.execute("project.setNotes", json!({"text": 5})).is_err());
    let huge = "x".repeat(crate::panels::MAX_NOTES_BYTES + 1);
    assert!(s.execute("project.setNotes", json!({"text": huge})).is_err());
    assert_eq!(s.project.notes, "");
}
