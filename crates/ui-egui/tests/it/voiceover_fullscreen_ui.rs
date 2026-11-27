//! The voice-over preference "Full-screen Program while recording" (#5264), through the control
//! channel: with it on, a take shows the Program monitor full screen and the layout that was there
//! comes back when the take ends, however it ends; a view that was already full screen, or that
//! the user left by hand during the take, is never disturbed.

use crate::support::{Drive, Launch, driver};
use filmcraft_engine::voiceover::{AudioInput, InputFormat};
use filmcraft_ui_egui::dock::PanelKind;
use serde_json::json;

struct FailedInput;

impl AudioInput for FailedInput {
    fn devices(&self) -> Vec<String> {
        vec!["Failed Input".into()]
    }
    fn channels(&self, _device: &str) -> u16 {
        1
    }
    fn start(&mut self, _device: &str, sample_rate: u32) -> std::result::Result<InputFormat, String> {
        Ok(InputFormat { sample_rate, channels: 1 })
    }
    fn read(&mut self, frames: usize) -> Vec<Vec<f32>> {
        vec![vec![0.5; frames]]
    }
    fn error(&self) -> Option<String> {
        Some("ALSA poll returned POLLERR".into())
    }
    fn discard(&mut self) {}
    fn stop(&mut self) {}
}

driver!(Driver);

impl Driver {
    /// A demo project whose takes land in a scratch folder of this test.
    fn with_pref(on: bool, name: &str) -> (Self, std::path::PathBuf) {
        let mut d = Driver::demo();
        let dir = std::env::temp_dir().join(format!("filmcraft-vo-fs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        d.exec("file.projectSettings.scratchDisks", json!({"captured": dir.to_string_lossy()}));
        d.exec("audio.voiceover.settings", json!({"prerollSeconds": 0.0, "fullScreenProgram": on}));
        d.exec("playhead.set", json!({"seconds": 1.0}));
        (d, dir)
    }
    fn full(&self) -> bool {
        self.harness.state().ui.fullscreen_program
    }
    fn recording(&self) -> bool {
        self.harness.state().session.voiceover.recording()
    }
    /// Start a take the way the transport control does (`voiceover.recordToggle`).
    fn start_take(&mut self) {
        self.ok("ui.menu.invoke", json!({"id": "voiceover.recordToggle", "params": {"track": "A1"}}));
        self.frames(2);
        assert!(self.recording(), "{}", self.harness.state().ui.status);
    }
    fn esc(&mut self) {
        self.ok("ui.key", json!({"key": "Escape"}));
        self.frames(2);
    }
    fn discard_take(&mut self) {
        self.exec("audio.voiceover.stop", json!({"discard": true}));
        self.frames(3);
    }
}

#[test]
fn a_take_leaves_the_layout_alone_by_default() {
    let (mut d, dir) = Driver::with_pref(false, "off");
    assert_eq!(d.exec("audio.voiceover.settings", json!({}))["settings"]["fullScreenProgram"], false);
    d.start_take();
    assert!(!d.full(), "the preference is off");
    d.discard_take();
    assert!(!d.full());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_take_shows_the_program_full_screen_and_stopping_gives_the_layout_back() {
    let (mut d, dir) = Driver::with_pref(true, "stop");
    d.harness.state_mut().ui.focused = PanelKind::Timeline;
    d.start_take();
    assert!(d.full(), "the take entered the full-screen Program view");
    assert_eq!(d.harness.state().ui.focused, PanelKind::Program);
    // the countdown / recording indicator is drawn in that view
    assert!(!d.ids("voiceover.countdown.overlay").is_empty(), "the overlay is visible full screen");
    // the microphone-button path ends the take: stop with the command (headless time does not advance)
    let v = d.exec("audio.voiceover.stop", json!({"seconds": 2.0}));
    assert_eq!(v["placed"], true);
    d.ok("ui.playback", json!({"action": "stop"}));
    d.frames(3);
    assert!(!d.full(), "the layout is back");
    assert_eq!(d.harness.state().ui.focused, PanelKind::Timeline, "with the focus it had");
    assert!(!d.harness.state().ui.voiceover_cinema, "nothing is left over for the next take");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_view_that_was_already_full_screen_stays_after_the_take() {
    let (mut d, dir) = Driver::with_pref(true, "already");
    d.ok("ui.menu.invoke", json!({"id": "view.fullScreenProgram"}));
    assert!(d.full());
    d.start_take();
    assert!(d.full());
    d.discard_take();
    assert!(d.full(), "full screen before the take, full screen after");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn leaving_by_hand_does_not_stop_the_take_and_its_end_does_not_re_enter() {
    let (mut d, dir) = Driver::with_pref(true, "hand");
    d.harness.state_mut().ui.focused = PanelKind::Timeline;
    d.start_take();
    assert!(d.full());
    d.esc();
    assert!(!d.full(), "Esc leaves the view");
    assert!(d.recording(), "and the recording goes on");
    assert!(d.harness.state().playback.playing);
    assert_eq!(d.harness.state().ui.focused, PanelKind::Timeline);
    // the user moved on to another panel; the end of the take must not disturb that
    d.harness.state_mut().ui.focused = PanelKind::Project;
    d.discard_take();
    assert!(!d.full(), "the end of the take does not re-enter");
    assert_eq!(d.harness.state().ui.focused, PanelKind::Project, "nor touch the focus");
    // the command leaves it too
    d.start_take();
    assert!(d.full());
    d.ok("ui.menu.invoke", json!({"id": "view.fullScreenProgram"}));
    d.frames(2);
    assert!(!d.full() && d.recording());
    d.discard_take();
    assert!(!d.full());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_view_entered_by_hand_after_leaving_one_belongs_to_the_user() {
    let (mut d, dir) = Driver::with_pref(true, "mine");
    d.start_take();
    d.esc();
    assert!(!d.full());
    // entered again by hand during the same take: the take's end leaves it alone
    d.ok("ui.menu.invoke", json!({"id": "view.fullScreenProgram"}));
    d.frames(2);
    assert!(d.full());
    d.discard_take();
    assert!(d.full(), "the user's own full-screen view stays");
    // and does not make the next take think the view is its own
    d.start_take();
    d.discard_take();
    assert!(d.full());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn leaving_and_re_entering_within_one_frame_belongs_to_the_user() {
    let (mut d, dir) = Driver::with_pref(true, "same-frame");
    d.start_take();
    assert!(d.full());
    // two commands before any frame runs (a control-channel burst, a fast double key press)
    let ctx = d.harness.ctx.clone();
    for _ in 0..2 {
        let r = filmcraft_ui_egui::panels::keyboard::route(d.harness.state_mut(), &ctx, "view.fullScreenProgram", &json!({}));
        assert!(matches!(r, Some(Ok(_))));
    }
    assert!(d.full(), "left and entered again by hand");
    d.discard_take();
    assert!(d.full(), "the take's end leaves the user's own view alone");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_bare_start_before_the_next_frame_does_not_inherit_the_earlier_takes_view() {
    let (mut d, dir) = Driver::with_pref(true, "next-take");
    d.harness.state_mut().ui.focused = PanelKind::Timeline;
    d.start_take();
    assert!(d.full());
    // one take ends and the next is armed before any frame runs (one control-channel drain)
    let session = &mut d.harness.state_mut().session;
    session.execute("audio.voiceover.stop", json!({"seconds": 2.0})).unwrap();
    session.execute("audio.voiceover.start", json!({"track": "A1"})).unwrap();
    d.frames(3);
    assert!(d.recording(), "the second take is recording");
    assert!(!d.full(), "the first take's view ended with it; a bare start changes no layout");
    assert_eq!(d.harness.state().ui.focused, PanelKind::Timeline);
    // so the second take's end has nothing to give back, and leaves the user's own view alone
    d.ok("ui.menu.invoke", json!({"id": "view.fullScreenProgram"}));
    d.discard_take();
    assert!(d.full());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_discarded_take_gives_the_layout_back() {
    let (mut d, dir) = Driver::with_pref(true, "discard");
    d.start_take();
    assert!(d.full());
    d.discard_take();
    assert!(!d.full());
    // from the settings dialog's Discard take too: the dialog is hidden in the full-screen view,
    // so it is opened by hand first, as the user does after leaving
    d.start_take();
    d.esc();
    d.ok("ui.menu.invoke", json!({"id": "voiceover.settingsDialog"}));
    d.frames(4);
    d.click("voiceover.discard");
    assert!(!d.recording() && !d.full());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_take_that_plays_out_past_the_punch_out_and_post_roll_gives_the_layout_back() {
    let (mut d, dir) = Driver::with_pref(true, "punch");
    d.exec("markers.markIn", json!({"seconds": 1.0}));
    d.exec("markers.markOut", json!({"seconds": 2.0}));
    d.exec("audio.voiceover.settings", json!({"postrollSeconds": 2.0}));
    d.start_take();
    assert!(d.full());
    // playback reaches the Out point: still counting as the take
    d.exec("playhead.set", json!({"seconds": 2.2}));
    d.frames(3);
    assert!(d.recording() && d.full(), "inside the post-roll");
    // past Out + post-roll, with the full-screen view showing (no dialogs are drawn there)
    d.exec("playhead.set", json!({"seconds": 4.3}));
    d.frames(4);
    assert!(!d.recording(), "the take ended by itself");
    assert!(!d.full(), "and the layout is back");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_take_that_cannot_be_saved_gives_the_layout_back_and_stays_available_for_retry() {
    let (mut d, dir) = Driver::with_pref(true, "save-fails");
    d.start_take();
    assert!(d.full());
    // the recordings folder cannot be written: a file stands where the folder should be
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::write(&dir, b"not a folder").unwrap();
    d.exec("playhead.set", json!({"seconds": 3.0}));
    d.ok("ui.playback", json!({"action": "stop"}));
    d.frames(3);
    assert!(d.recording(), "the stopped take is kept for another try");
    assert!(!d.full(), "the error and the recovery controls are not hidden behind the full-screen view");
    assert!(!d.harness.state().ui.voiceover_cinema);
    // fix the folder and retry with the same control: the take is saved, and the view stays as it is
    std::fs::remove_file(&dir).unwrap();
    d.ok("ui.menu.invoke", json!({"id": "voiceover.recordToggle", "params": {"track": "A1"}}));
    d.frames(3);
    assert!(!d.recording(), "the retry saved the take");
    assert!(!d.full());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_full_screen_view_the_take_did_not_enter_stays_when_its_save_fails() {
    let (mut d, dir) = Driver::with_pref(true, "save-fails-kept");
    d.ok("ui.menu.invoke", json!({"id": "view.fullScreenProgram"}));
    d.start_take();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::write(&dir, b"not a folder").unwrap();
    d.exec("playhead.set", json!({"seconds": 3.0}));
    d.ok("ui.playback", json!({"action": "stop"}));
    d.frames(3);
    assert!(d.recording());
    assert!(d.full(), "the user's own full-screen view is not the take's to give back");
    let _ = std::fs::remove_file(&dir);
    d.discard_take();
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_input_error_ends_the_take_and_gives_the_layout_back() {
    let (mut d, dir) = Driver::with_pref(true, "error");
    d.harness.state_mut().session.voiceover.input = Some(Box::new(FailedInput));
    d.ok("ui.menu.invoke", json!({"id": "voiceover.recordToggle", "params": {"track": "A1"}}));
    d.frames(4);
    assert!(!d.recording());
    assert!(!d.full(), "the failed take does not keep the view");
    assert!(!d.harness.state().ui.voiceover_cinema);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_take_that_fails_to_start_does_not_change_the_layout() {
    let (mut d, dir) = Driver::with_pref(true, "locked");
    d.exec("timeline.setTrack", json!({"track": "A1", "locked": true}));
    let r = d.call("ui.menu.invoke", json!({"id": "voiceover.recordToggle", "params": {"track": "A1"}}));
    d.frames(2);
    assert_ne!(r["ok"], json!(true), "the locked record track refuses the take: {r}");
    assert!(!d.recording() && !d.full());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_settings_dialog_has_the_checkbox_and_saves_it() {
    let mut d = Driver::demo();
    d.ok("ui.menu.invoke", json!({"id": "voiceover.settingsDialog"}));
    d.frames(4);
    assert!(d.has("voiceover.fullScreenProgram"));
    d.click("voiceover.fullScreenProgram");
    d.click("voiceover.ok");
    assert_eq!(d.exec("audio.voiceover.settings", json!({}))["settings"]["fullScreenProgram"], true);
}
