//! #2751: when a hardware decoder declines a stream, the status bar says so once, with the
//! reason, and `perf.stats` lists it.

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use serde_json::json;

fn step(h: &mut Harness<'static, FilmcraftApp>) {
    let ctx = h.ctx.clone();
    let mut raw = std::mem::take(h.input_mut());
    eframe::App::raw_input_hook(h.state_mut(), &ctx, &mut raw);
    *h.input_mut() = raw;
    ctx.request_repaint();
    h.step();
}

#[test]
fn a_declined_hardware_decoder_is_told_once_with_its_reason() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let mut h = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).build_eframe(move |_cc| FilmcraftApp::new(s));
    for _ in 0..3 {
        step(&mut h);
    }
    let reason = "HEVC video: no HEVC decoder MFT (the HEVC Video Extensions are not installed)";
    filmcraft_codecs::hw::note_hw_declined_because(reason);
    step(&mut h);
    let status = h.state().ui.status.clone();
    assert!(status.contains(reason) && status.contains("CPU"), "{status}");
    // told once: a later frame doesn't put it back
    h.state_mut().ui.status.clear();
    for _ in 0..3 {
        step(&mut h);
    }
    assert!(h.state().ui.status.is_empty(), "{}", h.state().ui.status);
    // two reasons before the next frame: both are told (#2754 review)
    let (a, b) = (
        "AV1 video: no AV1 decoder MFT (the AV1 Video Extension is not installed)",
        "VP9 video: no VP9 decoder MFT (the VP9 Video Extensions are not installed)",
    );
    filmcraft_codecs::hw::note_hw_declined_because(a);
    filmcraft_codecs::hw::note_hw_declined_because(b);
    step(&mut h);
    let status = h.state().ui.status.clone();
    assert!(status.contains(a) && status.contains(b), "{status}");
    // and agents can read it
    let stats = h.state_mut().session.execute("perf.stats", json!({})).unwrap();
    let reasons = stats["decode"]["hardware"]["declinedReasons"].as_array().cloned().unwrap_or_default();
    assert!(reasons.contains(&json!(reason)), "{stats}");
}
