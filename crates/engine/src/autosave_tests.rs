//! Preferences, auto-save ring and crash recovery through the command interface.

use super::*;
use crate::test_support::TestDir;
use serde_json::json;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

fn temp_dir(tag: &str) -> TestDir {
    TestDir::new(tag)
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let t0 = std::time::Instant::now();
    while !f() {
        assert!(t0.elapsed() < std::time::Duration::from_secs(10), "timed out waiting for {what}");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn start(s: &mut Session, data: &std::path::Path) {
    let mut cfg = autosave::AutosaveConfig::new(data);
    cfg.journal_debounce = std::time::Duration::from_millis(20);
    s.start_autosave(cfg).unwrap();
}

#[test]
fn view_recovery_check_detects_a_changed_project_file() {
    let temp = tempfile::tempdir().unwrap();
    let project_path = temp.path().join("project.fcproj");
    let recovery_dir = temp.path().join("recovery");
    std::fs::create_dir_all(&recovery_dir).unwrap();
    let original = filmcraft_format::encode(&filmcraft_project::Project::new("Original"), false);
    std::fs::write(&project_path, &original).unwrap();
    let candidate = autosave::RecoveryCandidate {
        id: "candidate".into(),
        dir: recovery_dir.clone(),
        snapshot_path: recovery_dir.join("snapshot.fcproj"),
        meta: autosave::JournalMeta {
            project_path: Some(project_path.to_string_lossy().into_owned()),
            project_fingerprint: Some(autosave::snapshot_fingerprint(&original)),
            view_only: true,
            ..Default::default()
        },
    };

    assert!(!autosave::view_recovery_project_changed(&candidate), "the unchanged project still matches its view journal");
    let changed = filmcraft_format::encode(&filmcraft_project::Project::new("Changed"), false);
    std::fs::write(&project_path, changed).unwrap();
    assert!(autosave::view_recovery_project_changed(&candidate), "the view journal is stale after its project file changes");
}

#[test]
fn a_clean_view_change_writes_a_compact_journal_but_is_not_offered_as_recovery() {
    let temp = tempfile::tempdir().unwrap();
    let d = temp.path().to_path_buf();
    let mut s = Session::default();
    let project_path = d.join("project.fcproj");
    s.execute("file.saveAs", json!({"path": project_path.to_string_lossy()})).unwrap();
    start(&mut s, &d);
    let session_dir = s.persistence.as_ref().unwrap().session_dir.clone();
    s.persistence.as_mut().unwrap().flush().unwrap();

    s.state.timeline_ui.inverted_controls = !s.state.timeline_ui.inverted_controls;
    let expected_setting = s.state.timeline_ui.inverted_controls;
    let revision = s.revision;
    let journals_before = s.persistence.as_ref().unwrap().status.journals_written;
    s.project_view_changed();
    s.persistence.as_mut().unwrap().flush().unwrap();
    s.poll_persistence();

    assert!(s.has_unsaved_changes(), "a clean project's view still needs the save/close prompt");
    let metadata: serde_json::Value = serde_json::from_slice(&std::fs::read(session_dir.join("session.json")).unwrap()).unwrap();
    assert_eq!(metadata["viewOnly"], true, "view-only recovery must remain distinct from content edits");
    assert_eq!(metadata["dirty"], false, "journaling the view must not mark project content dirty");
    assert_eq!(metadata["projectFingerprint"].as_array().map(Vec::len), Some(32));
    assert_eq!(s.revision, revision, "view-only changes re-journal without advancing the content revision");
    assert!(s.persistence.as_ref().unwrap().status.journals_written > journals_before);
    let view: filmcraft_project::ProjectView = serde_json::from_slice(&std::fs::read(session_dir.join("snapshot.fcproj")).unwrap()).unwrap();
    assert_eq!(view, s.project_view(), "the recovery payload contains only the current project view");
    let journaled_epoch = s.persistence.as_ref().unwrap().last_sent.as_ref().unwrap().view_epoch;
    let last_view_sent = s.persistence.as_ref().unwrap().last_view_sent.expect("the first view change records its coalescing time");
    s.state.timeline_ui.smooth_animations = true;
    s.view_changed();
    s.sync_persistence_at(last_view_sent + std::time::Duration::from_millis(1));
    assert_ne!(s.persistence.as_ref().unwrap().last_sent.as_ref().unwrap().view_epoch, s.view_epoch, "per-frame view updates are coalesced");
    s.shutdown();
    let final_view: filmcraft_project::ProjectView = serde_json::from_slice(&std::fs::read(session_dir.join("snapshot.fcproj")).unwrap()).unwrap();
    assert!(final_view.timeline_ui.smooth_animations, "shutdown flushes the latest view even when a drag update was coalesced");
    assert_eq!(journaled_epoch + 1, s.view_epoch);

    let mut recovered = Session::default();
    start(&mut recovered, &d);
    assert!(recovered.recovery_candidates().is_empty(), "clean view-only state is not unsaved project content");
    assert!(!session_dir.exists(), "stale clean view-only journals are removed during startup cleanup");
    assert_ne!(recovered.state.timeline_ui.inverted_controls, expected_setting, "a clean view-only change is not installed as recovery");
    recovered.shutdown();
}

#[test]
fn dirty_view_change_updates_recovery_view_without_rewriting_project_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path();
    let project_path = data.join("project.fcproj");
    let mut s = Session::default();
    s.execute("file.saveAs", json!({"path": project_path.to_string_lossy()})).unwrap();
    start(&mut s, data);
    s.execute("project.setNotes", json!({"text": "unsaved project content"})).unwrap();
    assert!(s.is_dirty());

    let session_dir = s.persistence.as_ref().unwrap().session_dir.clone();
    s.persistence.as_mut().unwrap().flush().unwrap();
    let snapshot_path = session_dir.join("snapshot.fcproj");
    let original_snapshot = std::fs::read(&snapshot_path).unwrap();

    s.state.timeline_ui.inverted_controls = !s.state.timeline_ui.inverted_controls;
    s.project_view_changed();
    let latest_view = s.project_view();
    s.persistence.as_mut().unwrap().flush().unwrap();

    assert_eq!(std::fs::read(&snapshot_path).unwrap(), original_snapshot, "view-only updates must leave the content snapshot untouched");
    let metadata: autosave::JournalMeta = serde_json::from_slice(&std::fs::read(session_dir.join("session.json")).unwrap()).unwrap();
    assert_eq!(metadata.recovery_view.as_ref(), Some(&latest_view), "the metadata carries the latest project view");
    assert_eq!(metadata.snapshot_fingerprint, Some(autosave::snapshot_fingerprint(&original_snapshot)));

    let candidate = autosave::RecoveryCandidate {
        id: session_dir.file_name().unwrap().to_string_lossy().into_owned(),
        dir: session_dir.clone(),
        snapshot_path,
        meta: metadata,
    };
    let loaded = autosave::load_candidate(&candidate).unwrap();
    assert_eq!(loaded.view, Some(latest_view), "recovery overlays the separately journaled view on the content snapshot");
    assert_eq!(loaded.project.notes, "unsaved project content");
    s.shutdown();
}

#[test]
fn changed_project_does_not_turn_a_clean_view_journal_into_recovery() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path();
    let project_path = data.join("project.fcproj");
    let mut original = Session::default();
    original.execute("file.saveAs", json!({"path": project_path.to_string_lossy()})).unwrap();
    start(&mut original, data);
    original.state.timeline_ui.inverted_controls = !original.state.timeline_ui.inverted_controls;
    original.project_view_changed();
    original.persistence.as_mut().unwrap().flush().unwrap();
    original.shutdown();

    std::fs::write(&project_path, b"changed after the view was journaled").unwrap();
    let mut recovery = Session::default();
    let expected_name = recovery.project.name.clone();
    start(&mut recovery, data);
    assert!(recovery.recovery_candidates().is_empty(), "clean view-only journals are not offered after an unexpected exit");
    assert_eq!(recovery.project.name, expected_name, "a clean view journal cannot install project content");
    recovery.shutdown();
}

#[test]
fn project_view_changes_are_save_dirty_without_marking_content_dirty() {
    let mut s = Session::default();
    let revision = s.revision;
    let saved_revision = s.saved_revision;
    let saved_view_epoch = s.saved_view_epoch;

    s.project_view_changed();

    assert_eq!(s.revision, revision, "view-only changes must not invalidate content caches");
    assert_eq!(s.saved_revision, saved_revision, "view-only changes must not change the saved content revision");
    assert_eq!(s.view_epoch, saved_view_epoch + 1);
    assert!(s.has_unsaved_changes(), "the view change still needs to be saved");
    assert!(!s.is_dirty(), "view-only changes must remain distinct from content edits");
}

fn journaled(s: &mut Session) -> u64 {
    s.poll_persistence();
    s.persistence.as_ref().unwrap().status.journaled_revision
}

fn discard_fixture(tag: &str) -> (Session, TestDir) {
    let d = temp_dir(tag);
    let mut s = Session::default();
    start(&mut s, &d);
    let candidates = ["first", "second", "third"]
        .into_iter()
        .map(|id| {
            let dir = d.join(id);
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(dir.join("snapshot.fcproj"), b"retained snapshot").unwrap();
            autosave::RecoveryCandidate { id: id.into(), snapshot_path: dir.join("snapshot.fcproj"), dir, meta: Default::default() }
        })
        .collect();
    s.persistence.as_mut().unwrap().candidates = candidates;
    (s, d)
}

fn block_discard(dir: &std::path::Path) {
    // An ordinary file cannot be removed as a directory, on Windows or Unix.
    std::fs::remove_dir_all(dir).unwrap();
    std::fs::write(dir, b"occupied directory path").unwrap();
}

fn recovery_ids(s: &mut Session) -> Vec<String> {
    s.execute("file.recoveryList", json!({})).unwrap().as_array().unwrap().iter().map(|v| v["id"].as_str().unwrap().to_owned()).collect()
}

#[test]
fn discard_failure_preserves_candidates_and_allows_retry() {
    let (mut s, d) = discard_fixture("discard-first-error");
    block_discard(&d.join("first"));
    assert!(s.execute("file.discardRecovery", json!({"all": true})).is_err());
    assert_eq!(recovery_ids(&mut s), ["first", "second", "third"]);
    assert!(s.is_enabled("file.discardRecovery"));
    assert!(d.join("second/snapshot.fcproj").exists());
    assert!(d.join("third/snapshot.fcproj").exists());
    std::fs::remove_file(d.join("first")).unwrap();
    std::fs::create_dir(d.join("first")).unwrap();
    assert_eq!(s.execute("file.discardRecovery", json!({"all": true})).unwrap()["discarded"], 3);
    assert!(recovery_ids(&mut s).is_empty());
    s.shutdown();
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn discard_failure_keeps_only_not_yet_deleted_candidates() {
    let (mut s, d) = discard_fixture("discard-second-error");
    block_discard(&d.join("second"));
    assert!(s.execute("file.discardRecovery", json!({"all": true})).is_err());
    assert_eq!(recovery_ids(&mut s), ["second", "third"]);
    assert!(!d.join("first").exists());
    assert!(d.join("third/snapshot.fcproj").exists());
    s.shutdown();
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn discard_selected_failure_keeps_unselected_candidates() {
    let (mut s, d) = discard_fixture("discard-selected-error");
    block_discard(&d.join("second"));
    assert!(s.execute("file.discardRecovery", json!({"id": "second"})).is_err());
    assert_eq!(recovery_ids(&mut s), ["first", "second", "third"]);
    assert!(d.join("first/snapshot.fcproj").exists());
    assert!(d.join("third/snapshot.fcproj").exists());
    s.shutdown();
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn discard_default_failure_does_not_select_another_candidate() {
    let (mut s, d) = discard_fixture("discard-default-error");
    block_discard(&d.join("first"));
    let error = s.execute("file.discardRecovery", json!({})).unwrap_err().to_string();
    assert!(error.contains("first"), "{error}");
    assert_eq!(recovery_ids(&mut s), ["first", "second", "third"]);
    assert!(d.join("second/snapshot.fcproj").exists());
    s.shutdown();
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn discard_selection_and_unknown_id_preserve_other_candidates() {
    let (mut s, d) = discard_fixture("discard-selection");
    assert!(s.execute("file.discardRecovery", json!({"id": "unknown"})).is_err());
    assert_eq!(recovery_ids(&mut s), ["first", "second", "third"]);
    assert_eq!(s.execute("file.discardRecovery", json!({"id": "second"})).unwrap()["discarded"], 1);
    assert_eq!(recovery_ids(&mut s), ["first", "third"]);
    assert!(!d.join("second").exists());
    assert_eq!(s.execute("file.discardRecovery", json!({})).unwrap()["discarded"], 1);
    assert_eq!(recovery_ids(&mut s), ["third"]);
    assert!(!d.join("first").exists());
    assert!(d.join("third/snapshot.fcproj").exists());
    s.shutdown();
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn preferences_get_set_and_persist() {
    let d = temp_dir("prefs");
    let mut s = Session::default();
    start(&mut s, &d);
    assert_eq!(s.execute("prefs.get", json!({"key": "autoSave.intervalMinutes"})).unwrap(), json!(5));
    assert_eq!(s.execute("prefs.get", json!({"key": "autoSave.maxVersions"})).unwrap(), json!(20));
    s.execute("prefs.set", json!({"key": "autoSave.intervalMinutes", "value": 12.0})).unwrap();
    s.execute("prefs.set", json!({"values": {"autoSave.maxVersions": 5000, "autoSave.saveCurrentProject": true}})).unwrap();
    assert_eq!(s.prefs.auto_save.interval_minutes, 12);
    assert_eq!(s.prefs.auto_save.max_versions, 1000, "clamped");
    assert!(s.execute("prefs.set", json!({"key": "autoSave.nope", "value": 1})).is_err());
    assert!(s.execute("prefs.set", json!({"key": "autoSave.enabled", "value": "yes"})).is_err());
    let keys = s.execute("prefs.get", json!({})).unwrap()["keys"].clone();
    assert!(keys.as_array().unwrap().iter().any(|k| k == "autoSave.recoveryJournal"));
    s.shutdown();
    let mut t = Session::default();
    start(&mut t, &d);
    assert_eq!(t.prefs.auto_save.interval_minutes, 12);
    assert!(t.prefs.auto_save.save_current_project);
    t.shutdown();
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn crash_recovery_end_to_end() {
    let d = temp_dir("recover");
    let proj = d.join("film.fcproj").to_string_lossy().to_string();
    // Session A: saved project, then unsaved edits, then a crash.
    let mut a = demo();
    let timeline_ui = filmcraft_project::TimelineUiSettings {
        smooth_animations: true,
        alternate_slider_position: true,
        playhead_side_seconds: 23.0,
        shift_drag_speed: 2.5,
        control_center_on_playhead: "Alt + P".into(),
        ..Default::default()
    };
    a.state.timeline_ui = timeline_ui.clone();
    start(&mut a, &d);
    a.execute("file.saveAs", json!({"path": proj})).unwrap();
    a.execute("sequence.addEdit", json!({"seconds": 1.5})).unwrap();
    a.execute("sequence.addEdit", json!({"seconds": 2.5})).unwrap();
    let rev = a.revision;
    wait_for("journal", || journaled(&mut a) == rev);
    let st = a.execute("file.autoSaveStatus", json!({})).unwrap();
    assert!(st["status"]["lastBytes"].as_u64().unwrap() > 1000, "{st}");
    let edited = (*a.project).clone();
    // Session B starts while A is alive: A's journal is not stale.
    let mut b = Session::default();
    start(&mut b, &d);
    assert!(b.recovery_candidates().is_empty(), "a live session's journal is never offered");
    b.execute("file.open", json!({"path": proj})).unwrap();
    assert_eq!(b.state.timeline_ui, timeline_ui, "Save and Open preserve project Timeline settings");
    b.shutdown();
    a.persistence.take().unwrap().simulate_crash();

    // Session C: next launch.
    let mut c = Session::default();
    start(&mut c, &d);
    assert_eq!(c.recovery_candidates().len(), 1);
    let list = c.execute("file.recoveryList", json!({})).unwrap();
    assert_eq!(list[0]["projectName"], "film", "named after the file it was saved as");
    assert_eq!(list[0]["projectPath"], proj.as_str());
    assert_eq!(list[0]["cleanExit"], false);
    assert_eq!(list[0]["savedAt"].as_str().unwrap().len(), 19);
    c.execute("file.recover", json!({})).unwrap();
    assert_eq!(*c.project, edited);
    assert_eq!(c.state.timeline_ui, timeline_ui, "recovery restores project Timeline settings");
    assert!(c.is_dirty(), "recovered changes are unsaved");
    assert_eq!(c.path.as_deref(), Some(proj.as_str()));
    assert!(c.recovery_candidates().is_empty());
    assert!(!c.is_enabled("file.recover"));
    // The recovered state is already in C's own journal (a crash now loses nothing).
    assert_eq!(journaled(&mut c), c.revision);
    c.execute("file.save", json!({})).unwrap();
    let mut reopened = Session::default();
    reopened.execute("file.open", json!({"path": proj})).unwrap();
    assert_eq!(reopened.state.timeline_ui, timeline_ui, "saving recovered settings keeps them on the next open");
    wait_for("journal cleared", || {
        c.poll_persistence();
        !c.persistence.as_ref().unwrap().session_dir.join("snapshot.fcproj").exists()
    });
    let session_dir = c.persistence.as_ref().unwrap().session_dir.clone();
    c.shutdown();
    assert!(!session_dir.exists(), "clean exit with nothing unsaved removes the journal");
    let mut e = Session::default();
    start(&mut e, &d);
    assert!(e.recovery_candidates().is_empty());
    e.shutdown();
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn quitting_with_unsaved_changes_keeps_them_for_next_launch() {
    let d = temp_dir("quitdirty");
    let mut a = demo();
    start(&mut a, &d);
    a.execute("sequence.addEdit", json!({"seconds": 1.0})).unwrap();
    a.shutdown();
    let mut b = Session::default();
    start(&mut b, &d);
    let list = b.execute("file.recoveryList", json!({})).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["cleanExit"], true);
    b.execute("file.discardRecovery", json!({})).unwrap();
    assert!(b.recovery_candidates().is_empty());
    b.shutdown();
    let mut c = Session::default();
    start(&mut c, &d);
    assert!(c.recovery_candidates().is_empty(), "discarded for good");
    c.shutdown();
    let _ = std::fs::remove_dir_all(&d);
}

/// A disabled or unwritable replacement journal must never consume the original recovery copy.
fn recovery_preserves_original(tag: &str, block: Option<&str>, journal_enabled: bool) {
    let d = temp_dir(tag);
    let old_dir = autosave::recovery_root(&d).join("1-1");
    std::fs::create_dir_all(&old_dir).unwrap();
    let project = (*demo().project).clone();
    let bytes = filmcraft_format::encode(&project, false);
    let original = old_dir.join("snapshot.fcproj");
    std::fs::write(&original, &bytes).unwrap();

    let mut s = Session::default();
    let mut cfg = autosave::AutosaveConfig::new(d.to_path_buf());
    cfg.journal_debounce = std::time::Duration::from_secs(60);
    s.start_autosave(cfg).unwrap();
    s.execute("prefs.set", json!({"key": "autoSave.recoveryJournal", "value": journal_enabled})).unwrap();
    if let Some(name) = block {
        // A directory where the atomic write needs a file reliably fails on Windows and Unix.
        std::fs::create_dir(s.persistence.as_ref().unwrap().session_dir.join(name)).unwrap();
    }
    let result = s.execute("file.recover", json!({"id": "1-1"}));
    assert!(original.is_file(), "failed replacement must preserve the original snapshot");
    assert_eq!(std::fs::read(&original).unwrap(), bytes);
    assert_eq!(s.recovery_candidates().len(), 1, "the original must remain discoverable");
    assert!(s.persistence.as_ref().unwrap().is_running(), "ordinary journal errors must not mark the worker stopped");
    assert!(result.is_err(), "lack of a confirmed replacement must be reported");
    assert_eq!(*s.project, project, "the recovered project remains open for Save As");
    assert!(s.is_dirty());
    s.shutdown();
    let found = autosave::scan_candidates(&d, None);
    assert!(found.iter().any(|c| c.id == "1-1"), "another launch can still recover the original");
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn recovery_keeps_original_when_snapshot_write_fails() {
    recovery_preserves_original("recover-snapshot-fails", Some("snapshot.fcproj"), true);
}

#[test]
fn recovery_keeps_original_when_metadata_write_fails() {
    recovery_preserves_original("recover-meta-fails", Some("session.json"), true);
}

#[test]
fn recovery_keeps_original_when_journal_is_disabled() {
    recovery_preserves_original("recover-journal-disabled", None, false);
}

#[test]
fn recovery_can_retry_after_a_failed_journal_write() {
    let d = temp_dir("recover-retry");
    let old_dir = autosave::recovery_root(&d).join("1-1");
    std::fs::create_dir_all(&old_dir).unwrap();
    let project = (*demo().project).clone();
    std::fs::write(old_dir.join("snapshot.fcproj"), filmcraft_format::encode(&project, false)).unwrap();
    let mut s = Session::default();
    let mut cfg = autosave::AutosaveConfig::new(d.to_path_buf());
    cfg.journal_debounce = std::time::Duration::from_secs(60);
    s.start_autosave(cfg).unwrap();
    let obstruction = s.persistence.as_ref().unwrap().session_dir.join("snapshot.fcproj");
    std::fs::create_dir(&obstruction).unwrap();
    assert!(s.execute("file.recover", json!({"id": "1-1"})).is_err());
    assert!(old_dir.join("snapshot.fcproj").is_file());
    std::fs::remove_dir(&obstruction).unwrap();
    s.execute("file.recover", json!({"id": "1-1"})).unwrap();
    assert!(!old_dir.exists());
    assert!(s.recovery_candidates().is_empty());
    let replacement = std::fs::read(&obstruction).unwrap();
    assert_eq!(filmcraft_format::decode(&replacement).unwrap().project, project);
    assert_eq!(journaled(&mut s), s.revision);
    s.shutdown();
    std::fs::remove_dir_all(&d).unwrap();
}

#[cfg(windows)]
#[test]
fn recovery_cleanup_failure_reports_open_project_and_safe_retry() {
    use std::os::windows::fs::OpenOptionsExt;

    let d = temp_dir("recover-cleanup-failure");
    let old_dir = autosave::recovery_root(&d).join("1-1");
    std::fs::create_dir_all(&old_dir).unwrap();
    let project = filmcraft_project::Project::default();
    let original = old_dir.join("snapshot.fcproj");
    std::fs::write(&original, filmcraft_format::encode(&project, false)).unwrap();
    // Permit the loader's read while denying deletion of this old snapshot.
    let held = std::fs::OpenOptions::new().read(true).share_mode(3).open(&original).unwrap();
    let mut s = Session::default();
    start(&mut s, &d);
    let error = s.execute("file.recover", json!({"id": "1-1"})).unwrap_err().to_string();
    assert_eq!(*s.project, project, "recovery already installed the project");
    assert!(s.is_dirty());
    assert_eq!(recovery_ids(&mut s), ["1-1"], "failed cleanup must keep the candidate listed");
    let replacement = &s.persistence.as_ref().unwrap().session_dir;
    assert!(replacement.join("snapshot.fcproj").is_file());
    assert!(replacement.join("session.json").is_file());
    assert!(error.contains("Recovered project is open") && error.contains("new recovery copy is confirmed"), "{error}");
    assert!(error.contains("candidate remains listed") && error.contains("Save As"), "{error}");
    assert!(error.contains("if the original recovery copy remains intact") && error.contains("unsaved edits"), "{error}");
    drop(held);
    s.shutdown();
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn auto_save_ring_rotates_and_can_save_the_project() {
    let d = temp_dir("ring");
    let proj = d.join("ring.fcproj").to_string_lossy().to_string();
    let mut s = demo();
    start(&mut s, &d);
    s.execute("prefs.set", json!({"values": {"autoSave.maxVersions": 2}})).unwrap();
    s.execute("file.saveAs", json!({"path": proj})).unwrap();
    // Nothing unsaved: no auto-save.
    assert!(s.execute("file.autoSaveNow", json!({})).unwrap()["path"].is_null());
    let mut written = Vec::new();
    for i in 0..3 {
        s.execute("sequence.addEdit", json!({"seconds": 1.0 + i as f64})).unwrap();
        let r = s.execute("file.autoSaveNow", json!({})).unwrap();
        written.push(r["path"].as_str().unwrap().to_string());
        // File names have one-second resolution.
        std::thread::sleep(std::time::Duration::from_millis(1100));
    }
    let sep = std::path::MAIN_SEPARATOR;
    assert!(written[0].contains(&format!("{sep}Auto-Save{sep}ring-")), "{}", written[0]);
    let list = s.execute("file.listAutoSaves", json!({})).unwrap();
    let files: Vec<String> = list["files"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
    assert_eq!(files, vec![written[2].clone(), written[1].clone()], "newest first, oldest pruned");
    let mut t = Session::default();
    t.execute("file.open", json!({"path": written[2]})).unwrap();
    assert_eq!(*t.project, *s.project);
    assert!(s.is_dirty(), "auto-save does not save the project itself by default");
    s.execute("prefs.set", json!({"key": "autoSave.saveCurrentProject", "value": true})).unwrap();
    let seq = s.state.active_sequence.unwrap();
    s.state
        .timeline_views
        .insert(seq, filmcraft_project::SequenceView { pps: 53.0, scroll: 2.0, v_scroll: 0.0, a_scroll: 0.0, video_track_h: 70.0, audio_track_h: 32.0 });
    s.view_changed();
    s.sync_persistence();
    s.execute("file.autoSaveNow", json!({})).unwrap();
    assert_eq!(s.saved_view_epoch, s.view_epoch, "auto-saving the current project also saves its Timeline view");
    let saved_view = filmcraft_format::decode(&std::fs::read(&proj).unwrap()).unwrap().view.unwrap();
    assert_eq!(saved_view.sequences.get(&seq).map(|v| (v.pps, v.scroll)), Some((53.0, 2.0)));
    s.execute("sequence.addEdit", json!({"seconds": 5.0})).unwrap();
    s.execute("file.autoSaveNow", json!({})).unwrap();
    wait_for("project saved", || {
        s.poll_persistence();
        !s.is_dirty()
    });
    let mut t = Session::default();
    t.execute("file.open", json!({"path": proj})).unwrap();
    assert_eq!(*t.project, *s.project);
    s.shutdown();
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn scan_keeps_snapshot_when_session_metadata_is_unreadable() {
    // #894: a stale session with a snapshot must not be deleted because session.json is missing
    // or corrupt — the recovered project would vanish from the list and the file would be gone.
    let d = temp_dir("recover-nometa");
    let root = autosave::recovery_root(&d);
    let sess = root.join("123-1");
    std::fs::create_dir_all(&sess).unwrap();
    let snap = filmcraft_format::encode(&filmcraft_project::Project::default(), false);
    std::fs::write(sess.join("snapshot.fcproj"), snap).unwrap();
    std::fs::write(sess.join("session.json"), b"not json {").unwrap();
    let found = autosave::scan_candidates(&d, None);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, "123-1");
    assert!(sess.join("snapshot.fcproj").is_file(), "snapshot must survive the scan");
    std::fs::remove_file(sess.join("session.json")).unwrap();
    let found = autosave::scan_candidates(&d, None);
    assert_eq!(found.len(), 1, "missing metadata still lists the snapshot");
    assert!(sess.join("snapshot.fcproj").is_file());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn recovery_rechecks_snapshot_fingerprint_after_scan() {
    use sha2::Digest;

    let d = temp_dir("recover-snapshot-changed");
    let session = autosave::recovery_root(&d).join("123-1");
    std::fs::create_dir_all(&session).unwrap();
    let snapshot = filmcraft_format::encode(&filmcraft_project::Project::new("Scanned"), false);
    let fingerprint: [u8; 32] = sha2::Sha256::digest(&snapshot).into();
    std::fs::write(session.join("snapshot.fcproj"), &snapshot).unwrap();
    let metadata = autosave::JournalMeta { snapshot_fingerprint: Some(fingerprint), dirty: true, ..Default::default() };
    std::fs::write(session.join("session.json"), serde_json::to_vec(&metadata).unwrap()).unwrap();

    let candidate = autosave::scan_candidates(&d, None).pop().unwrap();
    let replacement = filmcraft_format::encode(&filmcraft_project::Project::new("Replaced"), false);
    std::fs::write(&candidate.snapshot_path, replacement).unwrap();
    let error = autosave::load_candidate(&candidate).unwrap_err();
    assert!(error.contains("changed after"), "{error}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn recovery_scan_leaves_starting_session_directories_alone() {
    let d = temp_dir("recover-startup-stage");
    let staging = autosave::recovery_root(&d).join("123-1");
    std::fs::create_dir_all(&staging).unwrap();
    assert!(autosave::scan_candidates(&d, None).is_empty());
    assert!(staging.is_dir(), "a concurrent scan must not remove a just-created directory before its lock is installed");
    std::fs::write(staging.join(".starting"), []).unwrap();
    assert!(autosave::scan_candidates(&d, None).is_empty());
    assert!(staging.is_dir(), "a concurrent scan must leave a session with its startup marker alone");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn portable_marker_keeps_data_next_to_the_executable() {
    // #139: a portable install must not write to %APPDATA%.
    let d = temp_dir("portable");
    assert_eq!(autosave::portable_data_dir(&d), None);
    std::fs::create_dir_all(d.join(autosave::PORTABLE_MARKER)).unwrap();
    assert_eq!(autosave::portable_data_dir(&d), None, "a directory is not the marker");
    std::fs::remove_dir_all(d.join(autosave::PORTABLE_MARKER)).unwrap();
    std::fs::write(d.join(autosave::PORTABLE_MARKER), "").unwrap();
    assert_eq!(autosave::portable_data_dir(&d), Some(d.join("data")));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn journal_costs_the_ui_thread_only_a_handoff() {
    // Per change the UI thread pays an Arc clone and a channel send; encoding happens on the worker.
    let d = temp_dir("cost");
    let mut s = demo();
    start(&mut s, &d);
    let t0 = std::time::Instant::now();
    for i in 0..50 {
        s.execute("playhead.set", json!({"seconds": i as f64 * 0.1})).unwrap();
        let _ = s.execute("sequence.addEdit", json!({}));
    }
    let per_cmd = t0.elapsed() / 100;
    let rev = s.revision;
    wait_for("journal", || journaled(&mut s) == rev);
    assert!(per_cmd < std::time::Duration::from_millis(50), "{per_cmd:?}");
    s.shutdown();
    let _ = std::fs::remove_dir_all(&d);
}

/// Scratch Disks folders named in a project file are used only when they are inside the project's
/// folder or were chosen on this computer: otherwise auto-saves and previews go to the default
/// location, nothing is written to the named folder, and the user is told.
#[test]
fn project_scratch_folders_outside_the_project_are_not_used() {
    let d = temp_dir("scratch-trust");
    let outside = d.join("elsewhere");
    let show = d.join("show");
    std::fs::create_dir_all(&show).unwrap();
    let proj = show.join("cut.fcproj").to_string_lossy().to_string();
    let mut a = demo();
    let named = outside.to_string_lossy().to_string();
    a.edit("scratch", |p, _| {
        p.settings.scratch.auto_save = Some(named.clone());
        p.settings.scratch.video_previews = Some(named.clone());
        Ok(())
    })
    .unwrap();
    a.execute("file.saveAs", json!({"path": proj})).unwrap();
    let _ = std::fs::remove_dir_all(&outside);

    let mut s = Session::default();
    start(&mut s, &d.join("data"));
    s.execute("file.open", json!({"path": proj})).unwrap();
    assert!(
        s.drain_events().iter().any(|e| matches!(e, crate::Event::Toast { message, .. } if message.contains(&named))),
        "the user is told the folder is not used"
    );
    assert_eq!(crate::project_tools::previews_dir(&s), Some(show.join("FilmCraft Previews").join("cut")));
    s.execute("sequence.addEdit", json!({"seconds": 1.0})).unwrap();
    let r = s.execute("file.autoSaveNow", json!({})).unwrap();
    let written = std::path::PathBuf::from(r["path"].as_str().unwrap());
    assert!(written.starts_with(show.join("Auto-Save")), "{}", written.display());
    assert!(!outside.exists(), "nothing written to the folder the project names");

    // A folder inside the project's folder is used as is.
    let inner = show.join("Saves").to_string_lossy().to_string();
    s.edit("scratch", |p, _| {
        p.settings.scratch.auto_save = Some(inner.clone());
        Ok(())
    })
    .unwrap();
    s.sync_persistence();
    let r = s.execute("file.autoSaveNow", json!({})).unwrap();
    assert!(std::path::Path::new(r["path"].as_str().unwrap()).starts_with(&inner));

    // Choosing the outside folder in Scratch Disks makes it usable; network shares never are.
    s.execute("file.projectSettings.scratchDisks", json!({"autoSave": named})).unwrap();
    let r = s.execute("file.autoSaveNow", json!({})).unwrap();
    assert!(std::path::Path::new(r["path"].as_str().unwrap()).starts_with(&outside));
    assert!(s.execute("file.projectSettings.scratchDisks", json!({"autoSave": r"\\server\share\saves"})).is_err());
    assert!(crate::project_tools::trusted_scratch(Some(r"\\server\share"), Some(&proj), &[r"\\server\share".into()]).is_none());
    assert!(crate::project_tools::trusted_scratch(Some(&format!("{}/../x", show.display())), Some(&proj), &[]).is_none());
    s.shutdown();
    let _ = std::fs::remove_dir_all(&d);
}

/// The Captured Audio and Video and Audio Previews folders a project names follow the same rule:
/// voice-over takes, pasted images and proxies go next to the project unless the folder is
/// inside the project's folder; folders on another computer are never used.
#[test]
fn project_captured_folder_outside_the_project_is_not_used() {
    let d = temp_dir("scratch-captured");
    let show = d.join("show");
    std::fs::create_dir_all(&show).unwrap();
    let proj = show.join("cut.fcproj").to_string_lossy().to_string();
    let mut s = demo();
    s.execute("file.saveAs", json!({"path": proj})).unwrap();
    let mut named = vec![d.join("elsewhere").to_string_lossy().to_string(), r"\\192.0.2.1\share\cap".to_string(), "file://192.0.2.1/s/cap".to_string()];
    if cfg!(unix) {
        named.push(format!("/{}", show.join("Captured").display()));
    }
    let beside = show.to_string_lossy().to_string();
    for dir in &named {
        s.edit("scratch", |p, _| {
            p.settings.scratch.captured = Some(dir.clone());
            p.settings.scratch.audio_previews = Some(dir.clone());
            Ok(())
        })
        .unwrap();
        assert_eq!(crate::voiceover::media_dir(&s, &serde_json::Value::Null, "Captured").unwrap(), beside, "{dir}");
        let paths = crate::project_tools::scratch_paths(&s);
        assert_eq!(paths["captured"]["path"], json!(beside), "{dir}");
        assert_eq!(paths["audioPreviews"]["path"], json!(beside), "{dir}");
    }
    let inner = show.join("Captured").to_string_lossy().to_string();
    s.edit("scratch", |p, _| {
        p.settings.scratch.captured = Some(inner.clone());
        Ok(())
    })
    .unwrap();
    assert_eq!(crate::voiceover::media_dir(&s, &serde_json::Value::Null, "Captured").unwrap(), inner);
    assert!(s.execute("file.projectSettings.scratchDisks", json!({"captured": "file://192.0.2.1/s/cap"})).is_err());
    let _ = std::fs::remove_dir_all(&d);
}
