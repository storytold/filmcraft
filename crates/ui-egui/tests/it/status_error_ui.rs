//! An error in the status bar is easy to miss at the size and colour of a note (#1267): it is
//! drawn larger, in the danger colour, with its own automation id, and stays until it is clicked
//! away or twenty seconds have passed. A plain status is as before.

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;

fn app() -> Harness<'static, FilmcraftApp> {
    app_stepping(0.25)
}

/// The app with `dt` seconds between frames, for the tests that wait out the status timeout.
fn app_stepping(dt: f32) -> Harness<'static, FilmcraftApp> {
    let mut session = Session::default();
    session.execute("file.openDemoProject", serde_json::json!({})).expect("demo project");
    let mut h =
        Harness::builder().with_size(egui::vec2(1400.0, 900.0)).with_step_dt(dt).with_max_steps(10_000).build_eframe(move |_cc| FilmcraftApp::new(session));
    for _ in 0..4 {
        h.step();
    }
    h
}

fn has(h: &Harness<'static, FilmcraftApp>, id: &str) -> bool {
    h.state().auto.elements.iter().any(|e| e.id == id)
}

#[test]
fn an_error_is_marked_and_a_note_is_not() {
    let mut h = app();
    h.state_mut().ui.status = "Saved".into();
    h.step();
    h.step();
    assert!(!has(&h, "status.error"), "a note is not an error");
    h.state_mut().ui.fail("Cannot export: the folder is read-only");
    h.step();
    h.step();
    assert!(has(&h, "status.error"), "an error has its own element");
    assert!(h.state().ui.status_is_error());
    // a later note replaces it, and the emphasis goes with the text
    h.state_mut().ui.status = "Saved".into();
    h.step();
    h.step();
    assert!(!h.state().ui.status_is_error());
    assert!(!has(&h, "status.error"));
}

#[test]
fn a_click_on_the_status_bar_dismisses_an_error() {
    let mut h = app();
    h.state_mut().ui.fail("Import failed: unsupported file");
    h.step();
    h.step();
    let e = h.state().auto.elements.iter().find(|e| e.id == "status.error").expect("the error is shown").rect;
    // beside the text: a click on the text selects it
    let at = egui::pos2(e[0] + e[2] + 200.0, e[1] + e[3] / 2.0);
    h.event(egui::Event::PointerMoved(at));
    h.step();
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() });
        h.step();
    }
    h.step();
    assert!(h.state().ui.status.is_empty(), "the click cleared it");
}

#[test]
fn the_same_error_again_restarts_its_time() {
    let mut h = app_stepping(1.0);
    h.state_mut().ui.fail("Audio device lost, retrying");
    for _ in 0..15 {
        h.step();
    }
    assert!(h.state().ui.status_is_error(), "still shown after 15 s");
    h.state_mut().ui.fail("Audio device lost, retrying");
    for _ in 0..10 {
        h.step();
    }
    assert!(h.state().ui.status_is_error(), "25 s after the first, 10 s after the second");
    for _ in 0..15 {
        h.step();
    }
    assert!(h.state().ui.status.is_empty(), "gone 20 s after the last");
}

#[test]
fn a_note_with_the_text_of_an_earlier_error_is_not_an_error() {
    let mut h = app();
    h.state_mut().ui.fail("Could not save");
    h.step();
    h.step();
    assert!(has(&h, "status.error"));
    // dismissed, then the same words as a plain note
    h.state_mut().ui.status.clear();
    h.step();
    h.state_mut().ui.status = "Could not save".into();
    h.step();
    h.step();
    assert!(!h.state().ui.status_is_error());
    assert!(!has(&h, "status.error"));
    // and after another note in between
    h.state_mut().ui.fail("Could not save");
    h.step();
    h.state_mut().ui.status = "Saved".into();
    h.step();
    h.state_mut().ui.status = "Could not save".into();
    h.step();
    h.step();
    assert!(!h.state().ui.status_is_error());
}

/// A message longer than the room ends in an ellipsis before the job indicator, and shows whole in
/// a tooltip.
#[test]
fn a_long_error_stops_before_the_job_indicator() {
    use std::sync::atomic::Ordering;

    use filmcraft_engine::Job;
    let mut h = app();
    let job = Job { id: 900, label: "Exporting".into(), progress: Default::default(), result: Default::default() };
    job.progress.total.store(1000, Ordering::Relaxed);
    job.progress.done.store(250, Ordering::Relaxed);
    h.state_mut().session.jobs.push(job);
    h.state_mut().ui.fail("Export failed: ".to_string() + &"the destination folder cannot be written to, ".repeat(20));
    h.step();
    h.step();
    let rect = |h: &Harness<'static, FilmcraftApp>, id: &str| h.state().auto.elements.iter().find(|e| e.id == id).unwrap_or_else(|| panic!("no {id}")).rect;
    let (error, bar) = (rect(&h, "status.error"), rect(&h, "status.job.progress"));
    assert!(error[0] + error[2] <= bar[0], "the error ends at {}, the indicator begins at {}", error[0] + error[2], bar[0]);
}
