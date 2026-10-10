//! The recovery prompt at launch: it opens after a crash (Enter recovers, Esc is Not Now) and
//! not after a normal quit with unsaved changes, which only gets a status-bar hint — a prompt
//! on every launch held the keyboard, so no shortcut worked until it was answered.

use egui_kittest::Harness;
use filmcraft_engine::{Session, autosave};
use filmcraft_ui_egui::{Dialog, FilmcraftApp};
use serde_json::json;

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("fc-ui-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn start(s: &mut Session, data: &std::path::Path) {
    let mut cfg = autosave::AutosaveConfig::new(data);
    cfg.journal_debounce = std::time::Duration::from_millis(20);
    s.start_autosave(cfg).unwrap();
}

/// A session with unsaved edits that reached its journal.
fn edited(data: &std::path::Path) -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    start(&mut s, data);
    s.execute("sequence.addEdit", json!({"seconds": 1.5})).unwrap();
    let t0 = std::time::Instant::now();
    loop {
        s.poll_persistence();
        if s.persistence.as_ref().unwrap().status.journaled_revision == s.revision {
            break;
        }
        assert!(t0.elapsed() < std::time::Duration::from_secs(10), "journal not written");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    s
}

fn launch(data: &std::path::Path) -> Harness<'static, FilmcraftApp> {
    let mut s = Session::default();
    start(&mut s, data);
    let app = FilmcraftApp::new(s);
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
    h.run_steps(3);
    h
}

#[test]
fn a_normal_quit_with_unsaved_changes_does_not_prompt() {
    let d = temp_dir("quit");
    let mut a = edited(&d);
    a.shutdown();
    let h = launch(&d);
    assert_eq!(h.state().dialog, None, "no prompt holding the keyboard");
    assert!(h.state().ui.status.contains("Recover Unsaved Changes"), "{}", h.state().ui.status);
    assert_eq!(h.state().session.recovery_candidates().len(), 1, "still recoverable from the File menu");
    drop(h);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn after_a_crash_the_prompt_opens_and_esc_dismisses_it() {
    let d = temp_dir("crash");
    let mut a = edited(&d);
    a.persistence.take().unwrap().simulate_crash();
    let mut h = launch(&d);
    assert_eq!(h.state().dialog, Some(Dialog::Recovery));
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    assert_eq!(h.state().dialog, None, "Esc is Not Now");
    assert_eq!(h.state().session.recovery_candidates().len(), 1, "nothing discarded");
    drop(h);
    let _ = std::fs::remove_dir_all(&d);
}
