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

#[cfg(target_os = "linux")]
mod late_request_job_events {
    use super::*;
    use std::sync::TryLockError;
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    struct ExportCleanup {
        dir: std::path::PathBuf,
        started: bool,
        job: Option<crate::Job>,
    }

    impl Drop for ExportCleanup {
        fn drop(&mut self) {
            let Some(job) = &self.job else {
                if !self.started {
                    let _ = std::fs::remove_dir_all(&self.dir);
                }
                return; // An unknown producer may still own the directory.
            };
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                let terminal = match job.result.try_lock() {
                    Ok(result) => result.is_some(),
                    Err(TryLockError::Poisoned(error)) => error.into_inner().is_some(),
                    Err(TryLockError::WouldBlock) => false,
                };
                if terminal {
                    let _ = std::fs::remove_dir_all(&self.dir);
                    return;
                }
                job.progress.cancel.store(true, Ordering::Relaxed);
                if Instant::now() >= deadline {
                    return; // Preserve the directory if terminal ownership is uncertain.
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    fn snapshot(s: &mut Session) -> Value {
        let history = s.execute("history.list", json!({})).unwrap();
        json!({"project": s.project.as_ref(), "editor": s.state, "history": history,
            "revision": s.revision, "savedRevision": s.saved_revision, "path": s.path})
    }

    fn assert_real_io_error_event(deliver_before_request: bool) {
        let mut s = demo();
        s.execute("events.clear", json!({})).unwrap();
        assert!(s.jobs.is_empty());
        let before = snapshot(&mut s);
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("fc-job-event-write-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let mut cleanup = ExportCleanup { dir, started: false, job: None };
        let parent = cleanup.dir.join("blocked");
        let original = b"ordinary file, not an output directory";
        std::fs::write(&parent, original).unwrap();
        let output = parent.join("clip.wav");
        let path = output.to_str().unwrap().to_string();
        let previous_jobs = s.jobs.len();
        cleanup.started = true;
        let started = s.execute(
            "file.exportMedia",
            json!({"path": path, "format": "wav", "range": "custom",
            "startSeconds": 0, "endSeconds": 0.02, "captionSidecar": "none", "wait": false}),
        );
        cleanup.job = s.jobs.get(previous_jobs).cloned(); // Capture ownership before any result assertion.
        let started = started.unwrap();
        assert_eq!(s.jobs.len(), previous_jobs + 1);
        let id = started["job"].as_u64().unwrap();
        let job = cleanup.job.as_ref().unwrap().clone();
        assert_eq!(job.id, id);
        let deadline = Instant::now() + Duration::from_secs(30);
        let terminal = loop {
            let listed = s.execute("jobs.list", json!({})).unwrap();
            let current = listed.as_array().unwrap().iter().find(|entry| entry["id"] == id).unwrap().clone();
            if current["finished"] == true {
                break current;
            }
            assert!(Instant::now() < deadline, "the real export did not publish a terminal result");
            std::thread::sleep(Duration::from_millis(5));
        };
        let error = terminal["result"]["error"].as_str().expect("real output failure").to_string();
        assert!(error.starts_with("I/O: ") && error.contains(&path), "{error}");
        assert_ne!(error, "cancelled");
        assert!(!job.progress.cancel.load(Ordering::Relaxed));
        assert!(!job.progress.cancelled.load(Ordering::Relaxed));
        assert_eq!(std::fs::read(&parent).unwrap(), original);
        assert!(!output.exists());
        assert!(s.log.entries.is_empty(), "jobs.list must not deliver the first job Events");
        let first = deliver_before_request.then(|| s.execute("events.list", json!({})).unwrap());
        s.execute("jobs.cancel", json!({"job": id})).unwrap();
        assert!(job.progress.cancel.load(Ordering::Relaxed));
        assert!(!job.progress.cancelled.load(Ordering::Relaxed));
        assert_eq!(job.to_json()["result"], terminal["result"]);
        assert_eq!(snapshot(&mut s), before);
        assert_eq!(std::fs::read(&parent).unwrap(), original);
        assert!(!output.exists());
        let delivered = s.execute("events.list", json!({})).unwrap();
        if let Some(first) = first {
            assert_eq!(delivered, first, "an already delivered failure must not change after a late request");
        }
        let entries = delivered["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2, "one Started and one terminal Event");
        assert_eq!(entries[0]["source"], "job");
        assert_eq!(entries[0]["message"], format!("Started: {}", job.label));
        assert_eq!(entries[1]["source"], "job");
        assert_eq!(entries[1]["level"], "error", "a late request must not relabel the real I/O failure");
        assert_eq!(entries[1]["message"], format!("Failed: {}: {error}", job.label));
        assert_eq!(entries[0]["count"], 1);
        assert_eq!(entries[1]["count"], 1);
        s.poll_persistence();
        assert_eq!(s.execute("events.list", json!({})).unwrap(), delivered, "terminal delivery remains once-only");
        assert_eq!(snapshot(&mut s), before);
    }

    #[test]
    fn real_io_failure_before_first_event_keeps_failed_after_late_request() {
        assert_real_io_error_event(false);
    }

    #[test]
    fn real_io_failure_logged_before_late_request_stays_unchanged() {
        assert_real_io_error_event(true);
    }
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
