//! Settings ▸ Playback ▸ "Stop returns the playhead to where playback started" and the alternate
//! Play/Stop key (`playback.toggleOther`, Ctrl+Shift+Space): the stop goes where the setting says,
//! and the alternate key does the opposite, for both monitors; the setting is also in the
//! monitors' and the Timeline's settings menus.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_time::Tick;
use filmcraft_ui_egui::control::ControlRequest;
use filmcraft_ui_egui::{FilmcraftApp, menus};
use serde_json::{Value, json};

type H = Harness<'static, FilmcraftApp>;

fn harness(returns: bool) -> H {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).unwrap();
    session.execute("source.open", json!({"item": 5})).unwrap();
    session.execute("prefs.set", json!({"key": "playback.stopReturnsToStart", "value": returns})).unwrap();
    let mut h = Harness::builder().with_step_dt(0.25).with_size(egui::vec2(1280.0, 800.0)).build_eframe(move |_cc| FilmcraftApp::new(session));
    h.step();
    h
}

fn invoke(h: &mut H, id: &str, params: Value) {
    let ctx = h.ctx.clone();
    menus::invoke(h.state_mut(), &ctx, id, params).unwrap();
}

/// Play the Program monitor from 2 s until it has moved on, stop it with `stop`, and return where
/// it began and where it ended up.
fn program_run(returns: bool, stop: &str) -> (Tick, Tick) {
    let mut h = harness(returns);
    h.state_mut().session.set_playhead(Tick::from_seconds_f64(2.0));
    let start = h.state().session.playhead();
    invoke(&mut h, "playback.toggle", json!({"monitor": "program"}));
    for _ in 0..80 {
        h.step();
        if h.state().session.playhead() > start + Tick::from_seconds_f64(0.5) {
            break;
        }
    }
    assert!(h.state().playback.playing && h.state().session.playhead() > start, "playback moved on from {start:?}");
    invoke(&mut h, stop, json!({"monitor": "program"}));
    h.step();
    assert!(!h.state().playback.playing);
    (start, h.state().session.playhead())
}

/// The same for the Source monitor, with its own playhead.
fn source_run(returns: bool, stop: &str) -> (Tick, Tick) {
    let mut h = harness(returns);
    h.state_mut().session.execute("source.setPlayhead", json!({"seconds": 2})).unwrap();
    let start = h.state().session.state.source_playhead;
    invoke(&mut h, "playback.toggle", json!({"monitor": "source"}));
    for _ in 0..80 {
        h.step();
        if h.state().session.state.source_playhead > start + Tick::from_seconds_f64(0.5) {
            break;
        }
    }
    assert!(h.state().source_playback.clock.playing, "the Source monitor plays");
    invoke(&mut h, stop, json!({"monitor": "source"}));
    h.step();
    assert!(!h.state().source_playback.clock.playing);
    (start, h.state().session.state.source_playhead)
}

#[test]
fn off_space_and_k_stay_and_the_alternate_key_returns_to_the_start() {
    for run in [program_run, source_run] {
        for stop in ["playback.toggle", "playback.stop"] {
            let (start, end) = run(false, stop);
            assert!(end > start, "{stop} stays where playback got to: {start:?} to {end:?}");
        }
        let (start, end) = run(false, "playback.toggleOther");
        assert_eq!(end, start, "the alternate key goes back to where playback began");
    }
}

#[test]
fn on_space_and_k_return_to_the_start_and_the_alternate_key_stays() {
    for run in [program_run, source_run] {
        for stop in ["playback.toggle", "playback.stop"] {
            let (start, end) = run(true, stop);
            assert_eq!(end, start, "{stop} goes back to where playback began");
        }
        let (start, end) = run(true, "playback.toggleOther");
        assert!(end > start, "the alternate key stays where playback got to: {start:?} to {end:?}");
    }
}

/// Playing faster (L again) or looping does not move where a stop goes back to.
#[test]
fn a_speed_change_keeps_the_start_position() {
    let mut h = harness(false);
    h.state_mut().session.set_playhead(Tick::from_seconds_f64(2.0));
    let start = h.state().session.playhead();
    invoke(&mut h, "playback.toggle", json!({"monitor": "program"}));
    for _ in 0..40 {
        h.step();
    }
    invoke(&mut h, "playback.forward", json!({"monitor": "program"}));
    for _ in 0..20 {
        h.step();
    }
    assert!(h.state().session.playhead() > start);
    invoke(&mut h, "playback.toggleOther", json!({"monitor": "program"}));
    h.step();
    assert_eq!(h.state().session.playhead(), start);
}

/// Running into the end of the sequence stops where the setting says: Space would have.
#[test]
fn the_end_of_the_sequence_stops_where_the_setting_says() {
    for returns in [false, true] {
        let mut h = harness(returns);
        let duration = h.state().session.active_sequence().unwrap().duration();
        h.state_mut().session.set_playhead(duration - Tick::from_seconds_f64(1.0));
        let start = h.state().session.playhead();
        invoke(&mut h, "playback.toggle", json!({"monitor": "program"}));
        for _ in 0..80 {
            h.step();
            if !h.state().playback.playing {
                break;
            }
        }
        assert!(!h.state().playback.playing, "playback ran out at the end");
        let end = h.state().session.playhead();
        if returns {
            assert_eq!(end, start, "back where it began");
        } else {
            assert!(end > start, "at the end: {start:?} to {end:?}");
        }
    }
}

#[test]
fn the_toggle_command_switches_the_setting() {
    let mut h = harness(false);
    invoke(&mut h, "playback.toggleStopReturn", json!({}));
    assert!(h.state().session.prefs.playback.stop_returns_to_start);
    invoke(&mut h, "playback.toggleStopReturn", json!({}));
    assert!(!h.state().session.prefs.playback.stop_returns_to_start);
}

struct Driver {
    harness: H,
    tx: Sender<ControlRequest>,
}

impl Driver {
    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).unwrap();
        session.execute("source.open", json!({"item": 5})).unwrap();
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        d
    }
    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
        }
    }
    fn ok(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                assert_eq!(v["ok"], true, "{method} {params}: {v}");
                return v["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }
    fn on(&mut self) -> bool {
        self.harness.state().session.prefs.playback.stop_returns_to_start
    }
}

/// The checkbox is in the Program Monitor's, the Source Monitor's and the Timeline's settings menu,
/// and each switches the one setting.
#[test]
fn the_settings_menus_carry_the_checkbox() {
    let mut d = Driver::demo();
    assert!(!d.on(), "off by default");
    for (button, checkbox) in [
        ("program.settings", "program.settings.stopReturnsToStart"),
        ("source.settings", "source.settings.stopReturnsToStart"),
        ("timeline.toggle.settings", "timeline.settings.stopReturnsToStart"),
    ] {
        let before = d.on();
        d.ok("ui.click", json!({"id": button}));
        d.frames(2);
        d.ok("ui.click", json!({"id": checkbox}));
        d.frames(2);
        assert_eq!(d.on(), !before, "{checkbox} switches the setting");
    }
}
