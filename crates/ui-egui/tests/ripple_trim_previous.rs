//! Q (Ripple Trim Previous Edit to Playhead) with playback running: the running clock must stop,
//! or `advance_playback` recomputes the playhead from its anchor and carries it past the new edit
//! again. W (the next edit) removes the range starting at the playhead, so it does not move the
//! playhead and the running clock stays valid.

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

#[test]
fn ripple_trim_next_keeps_playback() {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).unwrap();
    let mut harness = Harness::builder().with_size(egui::vec2(1280.0, 800.0)).build_eframe(move |_cc| FilmcraftApp::new(session));
    harness.step();
    let ctx = harness.ctx.clone();

    // put the playhead inside a clip so W has a following edit (the clip's own end) to trim to
    let rate = harness.state().session.sequence_rate();
    let (ph, edge) = {
        let seq = harness.state().session.active_sequence().unwrap();
        let item = &seq.video_tracks[0].items[1];
        (item.start + rate.tick_of(6), item.end())
    };
    harness.state_mut().session.set_playhead(ph);
    assert!(edge > ph, "the clip must extend past the playhead for W to have a tail to trim");

    // play forward, then ripple-trim the next edit to the playhead: W removes [playhead, edit),
    // so the playhead already sits on the new edit and the running clock stays valid
    menus::invoke(harness.state_mut(), &ctx, "playback.forward", json!({"monitor": "program"})).unwrap();
    assert!(harness.state().playback.playing, "playback must be running");
    menus::invoke(harness.state_mut(), &ctx, "trim.rippleNext", json!({})).unwrap();

    // W actually cut: the clip's tail was trimmed from `edge` back to the playhead
    assert_eq!(harness.state().session.active_sequence().unwrap().video_tracks[0].items[1].end(), ph, "W must trim the clip's tail to the playhead");

    // the command itself leaves the playhead alone (the engine only parks it for Q) …
    assert_eq!(harness.state().session.playhead(), ph, "W must leave the playhead where it is");

    // … and with the clock running, playback continues forward from there: no backwards drift
    harness.step();
    harness.step();
    assert!(harness.state().playback.playing, "W must keep playback running");
    assert!(harness.state().session.playhead() >= ph, "the running clock must not carry the playhead backwards");
}
