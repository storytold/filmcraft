//! The bridge between a guest call and the session: what a rollback puts back, and what a caller on
//! another thread learns about a job that waits for the session thread.
//!
//! These tests use the `Bridge` directly, with no wasm guest, so every step is visible.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use filmcraft_engine::Session;
use filmcraft_plugins::bridge::{Bridge, Unavailable};
use serde_json::json;

fn demo_session() -> Session {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).unwrap();
    session
}

/// A faulted call that ran `file.newProject` puts the document back with the path it saves to.
/// Without the path the restored document would have no save destination.
#[test]
fn a_rollback_restores_the_path_the_document_saves_to() {
    let mut session = demo_session();
    let saved_at = std::env::temp_dir().join(format!("bridge-jobs-{}", std::process::id())).join("Film.fcproj").to_string_lossy().into_owned();
    session.path = Some(saved_at.clone());
    let before = filmcraft_format::encode(&session.project, false);
    let bridge = Bridge::new(Duration::from_secs(1));

    let (path_during, outcome) = bridge.scoped(&mut session, || {
        let scope = bridge.begin();
        bridge.execute("file.newProject", &json!({"name": "Replaced"})).expect("file.newProject");
        let during = bridge.with_session(|s| s.path.clone()).expect("session");
        (during, bridge.end(scope, true))
    });

    assert_eq!(path_during, None, "the command cleared the path, so the test checks a real change");
    assert_eq!(outcome, None, "the rollback ran");
    assert_eq!(filmcraft_format::encode(&session.project, false), before, "the project came back");
    assert_eq!(session.path.as_deref(), Some(saved_at.as_str()), "the document saves where it did before");
}

/// A faulted call that replaced the document (`file.open`, or a template that clears the pool in
/// place) puts back the media pool. The relative media paths of the original project must still
/// resolve against the original project file, not against the template.
#[test]
fn a_rollback_restores_the_media_pool_and_the_document_of_each_stored_path() {
    let dir = std::env::temp_dir().join(format!("bridge-pool-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let template = dir.join("Template.fcproj");
    std::fs::write(&template, filmcraft_format::encode(&demo_session().project, false)).unwrap();
    let template = template.to_string_lossy().into_owned();
    let original = dir.join("Film.fcproj").to_string_lossy().into_owned();
    let replacing = [("file.newProjectFromTemplate", json!({"template": template, "force": true})), ("file.open", json!({"path": template}))];

    for (command, params) in replacing {
        let mut session = demo_session();
        session.path = Some(original.clone());
        session.media.add_stored_paths(["clips/a.mov".to_owned()], Some(original.as_str()));
        let resolved = session.media.follow("clips/a.mov").expect("a stored path follows its document");
        assert_ne!(resolved, "clips/a.mov", "the path resolves against the document folder, so the test checks a real mapping");
        let bridge = Bridge::new(Duration::from_secs(1));

        let (during, outcome) = bridge.scoped(&mut session, || {
            let scope = bridge.begin();
            bridge.execute(command, &params).unwrap_or_else(|e| panic!("{command}: {e:?}"));
            let during = bridge.with_session(|s| s.media.follow("clips/a.mov").ok()).expect("session");
            (during, bridge.end(scope, true))
        });

        assert_eq!(during.as_deref(), Some("clips/a.mov"), "{command} dropped the mapping, so the test checks a real change");
        assert_eq!(outcome, None, "{command}: the rollback ran");
        assert_eq!(session.media.follow("clips/a.mov").ok().as_deref(), Some(resolved.as_str()), "{command}: the stored path lost its document");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A document with unsaved edits stays unsaved after a rollback, even when the faulted call ran a
/// command that marks the document clean. A clean report would let autosave drop the recovery
/// journal of work the user has not saved.
#[test]
fn a_rollback_keeps_a_dirty_document_dirty_after_the_call_made_it_clean() {
    let mut session = demo_session();
    session.execute("file.newBin", json!({"name": "unsaved"})).unwrap();
    assert!(session.is_dirty());
    let saved_before = session.saved_revision;
    let bridge = Bridge::new(Duration::from_secs(1));

    let (clean_during, outcome) = bridge.scoped(&mut session, || {
        let scope = bridge.begin();
        bridge.execute("file.newProject", &json!({"name": "Replaced"})).expect("file.newProject");
        let during = bridge.with_session(|s| !s.is_dirty()).expect("session");
        (during, bridge.end(scope, true))
    });

    assert!(clean_during, "the command marked the document clean, so the test checks a real change");
    assert_eq!(outcome, None, "the rollback ran");
    assert!(session.is_dirty(), "the unsaved edit is still unsaved");
    assert_eq!(session.saved_revision, saved_before, "the document is dirty against the revision it was saved at");
}

/// The caller's wait ends while the session thread runs the job. The job still edits the document,
/// so the caller must get its answer, not a timeout that reads as "nothing happened".
#[test]
fn a_job_that_runs_past_the_wait_still_answers_its_caller() {
    let mut session = demo_session();
    let bridge = Bridge::new(Duration::from_millis(300));
    let caller = {
        let bridge = Arc::clone(&bridge);
        std::thread::spawn(move || {
            bridge.with_session(|_session| {
                std::thread::sleep(Duration::from_millis(900));
                7_u32
            })
        })
    };
    while bridge.queued() == 0 {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(bridge.pump(&mut session), 1, "the session thread took the job");
    assert_eq!(caller.join().unwrap(), Ok(7));
}

/// A job nobody has started when the wait ends is cancelled, and a later pump does not run it.
#[test]
fn a_job_nobody_started_times_out_and_never_runs() {
    let mut session = demo_session();
    let bridge = Bridge::new(Duration::from_millis(50));
    let ran = Arc::new(AtomicBool::new(false));
    let caller = {
        let (bridge, ran) = (Arc::clone(&bridge), Arc::clone(&ran));
        std::thread::spawn(move || bridge.with_session(move |_session| ran.store(true, Ordering::SeqCst)))
    };
    assert_eq!(caller.join().unwrap(), Err(Unavailable::TimedOut));
    assert_eq!(bridge.pump(&mut session), 0, "a cancelled job is dropped");
    assert!(!ran.load(Ordering::SeqCst), "the cancelled job must not run");
}
