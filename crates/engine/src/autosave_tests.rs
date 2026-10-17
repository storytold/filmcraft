//! Preferences, auto-save ring and crash recovery through the command interface.

use super::*;
use serde_json::json;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("fc-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
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

fn journaled(s: &mut Session) -> u64 {
    s.poll_persistence();
    s.persistence.as_ref().unwrap().status.journaled_revision
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
    assert!(c.is_dirty(), "recovered changes are unsaved");
    assert_eq!(c.path.as_deref(), Some(proj.as_str()));
    assert!(c.recovery_candidates().is_empty());
    assert!(!c.is_enabled("file.recover"));
    // The recovered state is already in C's own journal (a crash now loses nothing).
    assert_eq!(journaled(&mut c), c.revision);
    c.execute("file.save", json!({})).unwrap();
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
fn invalid_live_dimensions_cannot_replace_the_last_reopenable_recovery_snapshot() {
    let dir = temp_dir("invalid-journal");
    let mut session = demo();
    start(&mut session, &dir);
    session.execute("sequence.addEdit", json!({"seconds": 1.0})).unwrap();
    let revision = session.revision;
    wait_for("valid recovery snapshot", || journaled(&mut session) == revision);
    let snapshot = session.persistence.as_ref().unwrap().session_dir.join("snapshot.fcproj");
    let valid_bytes = std::fs::read(&snapshot).unwrap();
    let invalid_item = std::sync::Arc::make_mut(&mut session.project).add_item(
        "oversized graphic",
        filmcraft_project::Label::Iris,
        filmcraft_project::ItemKind::Graphic { width: 32_769, height: 1, rate: filmcraft_time::FrameRate::FPS_24 },
        None,
    );
    session.revision += 1;
    session.sync_persistence();
    session.persistence.as_ref().unwrap().flush();
    session.poll_persistence();
    assert_eq!(std::fs::read(&snapshot).unwrap(), valid_bytes, "invalid state replaced the last reopenable recovery snapshot");
    assert!(session.persistence.as_ref().unwrap().status.last_error.as_deref().is_some_and(|error| error.contains("oversized graphic")));
    assert!(filmcraft_format::decode(&std::fs::read(&snapshot).unwrap()).is_ok());
    std::thread::sleep(std::time::Duration::from_millis(75));
    session.persistence.as_ref().unwrap().flush();
    let repeated_errors = session.persistence.as_mut().unwrap().drain_events();
    assert!(
        !repeated_errors.iter().any(|event| matches!(event, autosave::WorkerEvent::Error(_))),
        "unchanged invalid revision must not repeatedly report the same error: {repeated_errors:?}"
    );
    std::sync::Arc::make_mut(&mut session.project).items.get_mut(&invalid_item).unwrap().kind =
        filmcraft_project::ItemKind::Graphic { width: 1920, height: 1080, rate: filmcraft_time::FrameRate::FPS_24 };
    session.revision += 1;
    let recovered_revision = session.revision;
    session.sync_persistence();
    session.persistence.as_ref().unwrap().flush();
    assert_eq!(journaled(&mut session), recovered_revision, "a repaired revision must resume recovery writes");
    assert!(filmcraft_format::decode(&std::fs::read(&snapshot).unwrap()).is_ok());
    session.shutdown();
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn repaired_dimensions_keep_the_journal_deadline_during_continuous_edits() {
    let dir = temp_dir("repaired-journal-deadline");
    let mut cfg = autosave::AutosaveConfig::new(&dir);
    cfg.journal_debounce = std::time::Duration::from_secs(10);
    let prefs = autosave::AutoSavePrefs { enabled: false, recovery_interval_seconds: 1, ..Default::default() };
    let mut persistence = autosave::Persistence::start(&cfg, prefs).unwrap();
    let mut project = filmcraft_project::Project::default();
    let item = project.add_item(
        "oversized graphic",
        filmcraft_project::Label::Iris,
        filmcraft_project::ItemKind::Graphic { width: 32_769, height: 1, rate: filmcraft_time::FrameRate::FPS_24 },
        None,
    );
    persistence.send_dirty(std::sync::Arc::new(project.clone()), 1, None);
    persistence.flush();
    assert!(
        persistence.drain_events().iter().any(|event| matches!(event, autosave::WorkerEvent::Error(error) if error.contains("oversized graphic"))),
        "the first revision must be rejected"
    );

    project.items.get_mut(&item).unwrap().kind = filmcraft_project::ItemKind::Graphic { width: 1920, height: 1080, rate: filmcraft_time::FrameRate::FPS_24 };
    // Each valid edit arrives before the 10-second quiet debounce. Recovery still owes a
    // snapshot at the one-second maximum interval counted from the first repaired edit.
    for revision in 2..=13 {
        project.name = format!("repaired edit {revision}");
        persistence.send_dirty(std::sync::Arc::new(project.clone()), revision, None);
        std::thread::sleep(std::time::Duration::from_millis(150));
        persistence.drain_events();
        if persistence.status.journaled_revision >= 2 {
            break;
        }
    }
    assert!(persistence.status.journaled_revision >= 2, "continuous valid edits after a rejected revision must not starve recovery");
    let snapshot = persistence.session_dir.join("snapshot.fcproj");
    assert!(filmcraft_format::decode(&std::fs::read(&snapshot).unwrap()).is_ok());
    persistence.close();
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn unsupported_recovery_dimensions_leave_the_original_snapshot_for_repair() {
    let dir = temp_dir("invalid-recovery");
    std::fs::create_dir_all(&dir).unwrap();
    let mut project = filmcraft_project::Project::default();
    project.add_item(
        "oversized graphic",
        filmcraft_project::Label::Iris,
        filmcraft_project::ItemKind::Graphic { width: 32_769, height: 1, rate: filmcraft_time::FrameRate::FPS_24 },
        None,
    );
    let original = filmcraft_format::encode(&project, false);
    let path = dir.join("snapshot.fcproj");
    std::fs::write(&path, &original).unwrap();
    let candidate = autosave::RecoveryCandidate { id: "invalid".into(), dir: dir.clone(), meta: autosave::JournalMeta::default() };
    let error = autosave::load_candidate(&candidate).unwrap_err();
    assert!(error.contains("oversized graphic") && error.contains(&path.to_string_lossy().to_string()), "{error}");
    assert_eq!(std::fs::read(&path).unwrap(), original, "failed recovery must not erase the only snapshot");
    let _ = std::fs::remove_dir_all(&dir);
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
