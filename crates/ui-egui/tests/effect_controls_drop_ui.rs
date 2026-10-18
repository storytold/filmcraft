//! Effect Controls accepts compatible effects on release, including linked audio.
use egui::{Event, Modifiers, PointerButton, Pos2, Rect, pos2, vec2};
use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_project::{ClipId, TrackKind};
use filmcraft_ui_egui::{FilmcraftApp, dock::PanelKind};
use serde_json::json;

fn step(h: &mut Harness<'static, FilmcraftApp>) {
    let ctx = h.ctx.clone();
    let mut raw = std::mem::take(h.input_mut());
    eframe::App::raw_input_hook(h.state_mut(), &ctx, &mut raw);
    *h.input_mut() = raw;
    ctx.request_repaint();
    h.step();
}

fn pointer(h: &mut Harness<'static, FilmcraftApp>, pos: Pos2, pressed: Option<bool>) {
    h.input_mut().events.push(Event::PointerMoved(pos));
    if pressed == Some(true) {
        step(h);
    }
    if let Some(pressed) = pressed {
        h.input_mut().events.push(Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
    }
    step(h);
}

fn rect(h: &Harness<'static, FilmcraftApp>, id: &str) -> Rect {
    let [x, y, w, height] = h.state().auto.elements.iter().find(|e| e.id == id).unwrap_or_else(|| panic!("missing {id}")).rect;
    Rect::from_min_size(pos2(x, y), vec2(w, height))
}

fn harness(kind: TrackKind, effect: &str) -> (Harness<'static, FilmcraftApp>, ClipId) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let clip = s.active_sequence().unwrap().tracks(kind)[0].items[0].id;
    s.execute("sequence.linkedSelection", json!({"on": false})).unwrap();
    s.execute("timeline.select", json!({"clips": [clip.0]})).unwrap();
    let mut app = FilmcraftApp::new(s);
    app.set_workspace("Effects");
    app.show_panel(PanelKind::EffectControls);
    app.ui.effects_search = effect.to_string();
    let mut builder = Harness::builder().with_size(vec2(1600.0, 980.0)).with_max_steps(10_000);
    if std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR").is_some() {
        builder = builder.wgpu();
    }
    let mut h = builder.build_eframe(move |_cc| app);
    for _ in 0..4 {
        step(&mut h);
    }
    (h, clip)
}

fn drag_to_controls(h: &mut Harness<'static, FilmcraftApp>, effect: &str) -> Pos2 {
    let from = rect(h, &format!("effects.item.{effect}")).center();
    let to = rect(h, "effectControls.effectDropTarget").center();
    pointer(h, from, Some(true));
    pointer(h, from + vec2(0.0, 10.0), None);
    pointer(h, to, None);
    to
}

#[test]
fn compatible_effect_drops_apply_once_and_undo_in_one_step() {
    for (kind, id, name) in [(TrackKind::Audio, "denoise", "DeNoise"), (TrackKind::Video, "gaussian_blur", "Gaussian Blur")] {
        let (mut h, clip) = harness(kind, name);
        let before = h.state().session.project.to_json();
        let to = drag_to_controls(&mut h, id);
        assert_eq!(h.state().session.project.to_json(), before, "hover must not apply");
        if let Some(dir) = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR") {
            let dir = std::path::PathBuf::from(dir);
            std::fs::create_dir_all(&dir).unwrap();
            h.render().unwrap().save(dir.join(format!("effect-controls-drop-{id}.png"))).unwrap();
        }
        pointer(&mut h, to, Some(false));
        assert_ne!(h.state().session.project.to_json(), before);
        assert!(h.state().session.active_sequence().unwrap().find_item(clip).unwrap().1.effects.iter().any(|e| e.effect == id));
        h.state_mut().session.execute("edit.undo", json!({})).unwrap();
        assert_eq!(h.state().session.project.to_json(), before);
    }
}

#[test]
fn incompatible_and_cancelled_drops_do_not_edit() {
    let (mut h, _) = harness(TrackKind::Audio, "Gaussian Blur");
    let before = h.state().session.project.to_json();
    let to = drag_to_controls(&mut h, "gaussian_blur");
    pointer(&mut h, to, Some(false));
    assert_eq!(h.state().session.project.to_json(), before);
    assert!(h.state().ui.status.contains("compatible"));
    let (mut h, _) = harness(TrackKind::Audio, "DeNoise");
    let before = h.state().session.project.to_json();
    let to = drag_to_controls(&mut h, "denoise");
    h.key_press(egui::Key::Escape);
    step(&mut h);
    pointer(&mut h, to, Some(false));
    assert_eq!(h.state().session.project.to_json(), before);
}

#[test]
fn audio_drop_on_a_video_selection_targets_its_displayed_linked_audio() {
    let (mut h, _) = harness(TrackKind::Audio, "DeNoise");
    let seq = h.state().session.active_sequence().unwrap();
    let (video, audio) = seq
        .video_tracks
        .iter()
        .flat_map(|t| &t.items)
        .find_map(|v| {
            let link = v.link?;
            seq.audio_tracks.iter().flat_map(|t| &t.items).find(|a| a.link == Some(link)).map(|a| (v.id, a.id))
        })
        .expect("demo contains linked audio/video");
    h.state_mut().session.execute("sequence.linkedSelection", json!({"on": true})).unwrap();
    h.state_mut().session.execute("timeline.select", json!({"clips": [video.0]})).unwrap();
    for _ in 0..3 {
        step(&mut h);
    }
    let before = h.state().session.project.to_json();
    let video_effects = h.state().session.active_sequence().unwrap().find_item(video).unwrap().1.effects.clone();
    let to = drag_to_controls(&mut h, "denoise");
    pointer(&mut h, to, Some(false));
    let seq = h.state().session.active_sequence().unwrap();
    assert!(seq.find_item(audio).unwrap().1.effects.iter().any(|e| e.effect == "denoise"));
    assert_eq!(seq.find_item(video).unwrap().1.effects, video_effects);
    h.state_mut().session.execute("edit.undo", json!({})).unwrap();
    assert_eq!(h.state().session.project.to_json(), before);
}
