//! Right-click renaming a Timeline track uses timeline.setTrack, which is undoable.

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

fn click(h: &mut Harness<'static, FilmcraftApp>, id: &str, button: egui::PointerButton) {
    let [x, y, w, height] = h.state().auto.find(id).expect("visible control").rect;
    let at = egui::pos2(x + w * 0.5, y + height * 0.5);
    h.input_mut().events.push(egui::Event::PointerMoved(at));
    step(h);
    h.input_mut().events.push(egui::Event::PointerButton { pos: at, button, pressed: true, modifiers: Default::default() });
    step(h);
    h.input_mut().events.push(egui::Event::PointerButton { pos: at, button, pressed: false, modifiers: Default::default() });
    step(h);
}

#[test]
fn rename_from_video_track_header_is_undoable() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).expect("demo project");
    let mut h = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_step_dt(1.0 / 60.0).build_eframe(move |_cc| FilmcraftApp::new(s));
    for _ in 0..5 { step(&mut h); }
    let original = h.state().session.active_sequence().expect("active sequence").video_tracks[0].name.clone();
    let id = h.state().session.active_sequence().expect("active sequence").video_tracks[0].id.0;
    click(&mut h, "timeline.track.V1.name", egui::PointerButton::Secondary);
    assert!(h.state().auto.find("timeline.track.V1.renameField").is_some(), "track rename popup should open");
    h.ctx.data_mut(|d| d.insert_temp(egui::Id::new(("timeline-track-rename", id)), "B-roll".to_string()));
    step(&mut h);
    click(&mut h, "timeline.track.V1.renameApply", egui::PointerButton::Primary);
    assert_eq!(h.state().session.active_sequence().expect("active sequence").video_tracks[0].name, "B-roll");
    h.state_mut().session.execute("edit.undo", json!({})).expect("undo rename");
    assert_eq!(h.state().session.active_sequence().expect("active sequence").video_tracks[0].name, original);
}
