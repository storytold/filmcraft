//! #4655: Step 1 Frame and Go to Edit Point stop playback where they land.
//!
//! `Session::execute` moved the playhead, but the UI's playback clock stayed on, so
//! `advance_playback` recomputed the playhead from its running anchor on the next pass and
//! overwrote the step. These commands now stop the transport on the frame they chose, as in
//! Premiere Pro.

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::{FilmcraftApp, menus};
use serde_json::json;

/// A demo project with the playhead on frame 24 and playback running.
fn playing() -> Harness<'static, FilmcraftApp> {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).unwrap();
    let rate = session.sequence_rate();
    session.set_playhead(rate.tick_of(24));
    let mut harness = Harness::builder().with_size(egui::vec2(1280.0, 800.0)).with_step_dt(1.0 / 60.0).build_eframe(move |_cc| FilmcraftApp::new(session));
    harness.step();
    harness.state_mut().play(1.0);
    harness.step();
    assert!(harness.state().playback.playing, "playback is running before the key");
    harness
}

/// The step keys stop playback and keep the frame they chose.
#[test]
fn stepping_the_playhead_stops_playback() {
    for (command, frames) in [("playhead.stepForward", 1), ("playhead.stepBack", -1)] {
        let mut harness = playing();
        let ctx = harness.ctx.clone();
        let rate = harness.state().session.sequence_rate();
        let want = rate.tick_of(rate.frame_at(harness.state().session.playhead()) + frames);
        menus::invoke(harness.state_mut(), &ctx, command, json!({})).expect(command);
        assert!(!harness.state().playback.playing, "{command} stops playback");
        assert_eq!(harness.state().session.playhead(), want, "{command} lands on its frame");
        // The bug: a running clock overwrote the step on the next pass.
        harness.step();
        assert_eq!(harness.state().session.playhead(), want, "{command} keeps its frame on the next pass");
    }
    // Settings ▸ Playback ▸ Step forward/back many.
    for (command, direction) in [("playhead.stepForward5", 1), ("playhead.stepBack5", -1)] {
        let mut harness = playing();
        let ctx = harness.ctx.clone();
        let rate = harness.state().session.sequence_rate();
        let n = i64::from(harness.state().session.prefs.playback.step_many_frames) * direction;
        let want = rate.tick_of(rate.frame_at(harness.state().session.playhead()) + n);
        menus::invoke(harness.state_mut(), &ctx, command, json!({})).expect(command);
        assert!(!harness.state().playback.playing, "{command} stops playback");
        assert_eq!(harness.state().session.playhead(), want, "{command} lands on its frame");
        harness.step();
        assert_eq!(harness.state().session.playhead(), want, "{command} keeps its frame on the next pass");
    }
    // Go to Edit Point: the demo's next edit is ahead of frame 24, its previous one behind it.
    for (command, forward) in [("playhead.nextEdit", true), ("playhead.prevEdit", false)] {
        let mut harness = playing();
        let ctx = harness.ctx.clone();
        let start = harness.state().session.playhead();
        menus::invoke(harness.state_mut(), &ctx, command, json!({})).expect(command);
        assert!(!harness.state().playback.playing, "{command} stops playback");
        let landed = harness.state().session.playhead();
        assert!(if forward { landed > start } else { landed < start }, "{command} moved the playhead: {landed:?}");
        harness.step();
        assert_eq!(harness.state().session.playhead(), landed, "{command} keeps its frame on the next pass");
    }
}

/// A step while stopped is unchanged: there is no transport to stop.
#[test]
fn stepping_while_stopped_leaves_the_transport_alone() {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).unwrap();
    let rate = session.sequence_rate();
    session.set_playhead(rate.tick_of(24));
    let mut harness = Harness::builder().with_size(egui::vec2(1280.0, 800.0)).build_eframe(move |_cc| FilmcraftApp::new(session));
    harness.step();
    let ctx = harness.ctx.clone();
    menus::invoke(harness.state_mut(), &ctx, "playhead.stepForward", json!({})).unwrap();
    assert!(!harness.state().playback.playing);
    assert_eq!(harness.state().session.playhead(), rate.tick_of(25));
}
