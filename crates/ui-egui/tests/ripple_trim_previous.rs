//! Q (Ripple Trim Previous Edit to Playhead) with playback running: the running clock must stop,
//! or `advance_playback` recomputes the playhead from its anchor and carries it past the new edit
//! again. W (the next edit) keeps its behaviour.

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::{FilmcraftApp, menus};
use serde_json::json;

#[test]
fn ripple_trim_previous_stops_playback() {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).unwrap();
    let mut harness = Harness::builder().with_size(egui::vec2(1280.0, 800.0)).build_eframe(move |_cc| FilmcraftApp::new(session));
    harness.step();
    let ctx = harness.ctx.clone();

    // put the playhead inside a clip that has an edit before it, so Q has something to trim
    let rate = harness.state().session.sequence_rate();
    let ph = {
        let seq = harness.state().session.active_sequence().unwrap();
        seq.video_tracks[0].items[1].start + rate.tick_of(6)
    };
    harness.state_mut().session.set_playhead(ph);

    // play forward, then ripple-trim the previous edit to the playhead
    menus::invoke(harness.state_mut(), &ctx, "playback.forward", json!({"monitor": "program"})).unwrap();
    assert!(harness.state().playback.playing, "playback must be running to exercise the regression");
    menus::invoke(harness.state_mut(), &ctx, "trim.ripplePrevious", json!({})).unwrap();

    assert!(!harness.state().playback.playing, "Q must pause playback");
}
