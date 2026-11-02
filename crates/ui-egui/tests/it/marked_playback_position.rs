use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_time::Tick;
use filmcraft_ui_egui::{FilmcraftApp, menus};
use serde_json::json;

#[test]
fn program_range_playback_rests_on_inclusive_out_frame() {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).unwrap();
    let rate = session.sequence_rate();
    let out = rate.tick_of(24);
    let fd = rate.frame_duration();
    session.execute("markers.markIn", json!({"time": 0})).unwrap();
    session.execute("markers.markOut", json!({"time": out.0})).unwrap();

    let mut harness = Harness::builder().with_size(egui::vec2(1280.0, 800.0)).with_step_dt(0.25).build_eframe(move |_cc| FilmcraftApp::new(session));
    harness.step();
    let ctx = harness.ctx.clone();

    for command in ["playback.inToOut", "playback.toOut"] {
        harness.state_mut().session.set_playhead(Tick::ZERO);
        menus::invoke(harness.state_mut(), &ctx, command, json!({"monitor": "program"})).unwrap();
        assert_eq!(harness.state().playback.stop_at, Some(out + fd));
        assert_eq!(harness.state().playback.rest_at, Some(out));
        for _ in 0..40 {
            harness.step();
            if !harness.state().playback.playing {
                break;
            }
        }
        assert!(!harness.state().playback.playing, "{command} did not stop");
        assert_eq!(harness.state().session.playhead(), out, "{command} must rest on Out");
        assert_eq!(harness.state().playback.rest_at, None);
    }

    // Ordinary end-of-sequence playback still rests on the last visible frame.
    let duration = harness.state().session.active_sequence().unwrap().duration();
    harness.state_mut().session.set_playhead(duration - rate.tick_of(6));
    menus::invoke(harness.state_mut(), &ctx, "playback.forward", json!({"monitor": "program"})).unwrap();
    for _ in 0..40 {
        harness.step();
        if !harness.state().playback.playing {
            break;
        }
    }
    assert!(!harness.state().playback.playing);
    assert_eq!(harness.state().session.playhead(), duration - fd);

    // The deliberately extended preroll/postroll command is not snapped to Out.
    menus::invoke(harness.state_mut(), &ctx, "playback.inToOutPreroll", json!({"monitor": "program"})).unwrap();
    let post = Tick::from_seconds_f64(harness.state().session.prefs.playback.postroll_seconds);
    assert!(post > Tick::ZERO);
    assert_eq!(harness.state().playback.stop_at, Some(out + fd + post));
    assert_eq!(harness.state().playback.rest_at, None);
    harness.state_mut().stop();
}
