//! Dialog buttons show the pointing hand while hovered: the crash-recovery prompt (Discard, Not Now,
//! Recover), the Revert confirmation (Yes, No) and Settings (Help, Reset…, OK, Cancel). egui leaves
//! `Visuals::interact_cursor` unset, so each dialog's `button` helper asks for the hand itself.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::support::{Drive, Launch, builder, demo_session, driver, egui_rect};
use egui::CursorIcon;
use filmcraft_engine::Session;
use filmcraft_engine::autosave::AutosaveConfig;
use serde_json::json;

driver!(Driver);

impl Driver {
    fn new(session: Session) -> Self {
        Self::open(session, builder())
    }
    fn menu(&mut self, id: &str) {
        self.ok("ui.menu.invoke", json!({"id": id}));
        self.frames(3);
    }
    /// An automation element's rect.
    fn rect(&mut self, id: &str) -> egui::Rect {
        egui_rect(self.expect_rect(id))
    }
    /// The cursor the app asks for with the pointer resting at `pos`.
    fn cursor_at(&mut self, pos: egui::Pos2) -> CursorIcon {
        self.harness.input_mut().events.push(egui::Event::PointerMoved(pos));
        self.frames(2);
        self.harness.output().platform_output.cursor_icon
    }
    fn assert_pointing_hand(&mut self, ids: &[&str]) {
        for id in ids {
            let at = self.rect(id).center();
            assert_eq!(self.cursor_at(at), CursorIcon::PointingHand, "hovering {id}");
        }
    }
}

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("fc-dialog-buttons-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn demo() -> Session {
    demo_session()
}

/// The next launch after a session crashed with unsaved edits: it has one recovery candidate.
fn session_after_crash(data: &Path) -> Session {
    let mut crashed = demo();
    let mut cfg = AutosaveConfig::new(data);
    cfg.journal_debounce = Duration::from_millis(20);
    crashed.start_autosave(cfg).unwrap();
    crashed.execute("sequence.addEdit", json!({"seconds": 1.5})).unwrap();
    let t0 = Instant::now();
    loop {
        crashed.poll_persistence();
        if crashed.persistence.as_ref().unwrap().status.journaled_revision == crashed.revision {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(10), "timed out waiting for the journal");
        std::thread::sleep(Duration::from_millis(10));
    }
    crashed.persistence.take().unwrap().simulate_crash();
    let mut next = Session::default();
    next.start_autosave(AutosaveConfig::new(data)).unwrap();
    assert_eq!(next.recovery_candidates().len(), 1);
    next
}

#[test]
fn recovery_prompt_buttons_show_the_pointing_hand() {
    let data = temp_dir("recovery");
    // The prompt opens by itself at start-up.
    let mut d = Driver::new(session_after_crash(&data));
    d.assert_pointing_hand(&["recovery.discard", "recovery.later", "recovery.recover"]);
    // Only the buttons: the empty space between Discard and Not Now keeps the default pointer.
    let (discard, later) = (d.rect("recovery.discard"), d.rect("recovery.later"));
    let gap = egui::pos2((discard.right() + later.left()) / 2.0, discard.center().y);
    assert_eq!(d.cursor_at(gap), CursorIcon::Default, "between the buttons");
    drop(d);
    let _ = std::fs::remove_dir_all(&data);
}

#[test]
fn revert_confirmation_buttons_show_the_pointing_hand() {
    let data = temp_dir("revert");
    let mut s = demo();
    s.execute("file.saveAs", json!({"path": data.join("film.fcproj").to_string_lossy()})).unwrap();
    s.execute("sequence.addEdit", json!({"seconds": 1.5})).unwrap();
    let mut d = Driver::new(s);
    d.menu("file.revert");
    d.assert_pointing_hand(&["revert.yes", "revert.no"]);
    drop(d);
    let _ = std::fs::remove_dir_all(&data);
}

#[test]
fn settings_buttons_show_the_pointing_hand() {
    let mut d = Driver::new(demo());
    d.menu("app.settings.general");
    d.assert_pointing_hand(&["settings.help", "settings.reset", "settings.ok", "settings.cancel"]);
}
