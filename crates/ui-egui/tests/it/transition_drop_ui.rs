//! Transition drag feedback: actual span and edge, no edit until release, one undo step.
use egui::{Event, Modifiers, PointerButton, Pos2, Rect, pos2, vec2};
use egui_kittest::Harness;
use filmcraft_engine::{Session, commands::preview_transition};
use filmcraft_project::TrackKind;
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

fn harness(effect: &str) -> Harness<'static, FilmcraftApp> {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("file.newSequence", json!({"name":"Transition drops","fps":24})).unwrap();
    let rate = s.sequence_rate();
    // A short clip touching a longer clip: either edge must be distinguishable on screen.
    for (at, frames) in [(0, 48), (48, 6)] {
        s.execute("timeline.place", json!({"item":5,"time":rate.tick_of(at).0,"duration":rate.tick_of(frames).0})).unwrap();
    }
    let mut app = FilmcraftApp::new(s);
    app.set_workspace("Effects");
    app.ui.effects_search = effect.to_string();
    let mut builder = Harness::builder().with_size(vec2(1600.0, 980.0)).with_max_steps(10_000);
    if std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR").is_some() {
        builder = builder.wgpu();
    }
    let mut h = builder.build_eframe(move |_cc| app);
    for _ in 0..4 {
        step(&mut h);
    }
    // Opening the new sequence restores its own view; set the test zoom after that initial fit.
    h.state_mut().ui.timeline.pps = 60.0;
    h.state_mut().ui.timeline.target_pps = 60.0;
    h.state_mut().ui.timeline.fit_pending = false;
    step(&mut h);
    h
}

#[test]
fn transition_drag_previews_both_edges_of_short_clips_and_commits_that_span() {
    for (kind, effect, name) in [(TrackKind::Video, "cross_dissolve", "Cross Dissolve"), (TrackKind::Audio, "constant_power", "Constant Power")] {
        let mut h = harness(name);
        let clip = h.state().session.active_sequence().unwrap().tracks(kind)[0].items[1].id;
        let clip_rect = rect(&h, &format!("timeline.clip.{}", clip.0));
        let from = rect(&h, &format!("effects.item.{effect}")).center();
        let before = h.state().session.project.to_json();
        pointer(&mut h, from, Some(true));
        pointer(&mut h, from + vec2(0.0, 10.0), None);
        for (edge, fraction) in [("in", 0.2), ("out", 0.8)] {
            let to = pos2(clip_rect.left() + clip_rect.width() * fraction, clip_rect.center().y);
            pointer(&mut h, to, None);
            let preview = h.state().auto.elements.iter().find(|e| e.id == "timeline.transitionDropPreview").expect("transition preview while dragging");
            assert_eq!(preview.label, edge);
            assert_eq!(h.state().session.project.to_json(), before, "hover must not edit");
            let (_, planned) = preview_transition(&h.state().session, &json!({"effect":effect,"clip":clip.0,"edge":edge}), kind).unwrap();
            let rate = h.state().session.sequence_rate();
            let short = &h.state().session.active_sequence().unwrap().tracks(kind)[0].items[1];
            let px_per_tick = clip_rect.width() / short.duration.0 as f32;
            let expected_left = clip_rect.left() + (planned.start - short.start).0 as f32 * px_per_tick;
            let content = rect(&h, "timeline.tracks");
            let expected_right = (expected_left + planned.duration.0 as f32 * px_per_tick).min(content.right());
            let expected_left = expected_left.max(content.left());
            assert!((preview.rect[0] - expected_left).abs() < 1.0);
            assert!(
                (preview.rect[2] - (expected_right - expected_left)).abs() < 1.0,
                "{kind:?} {edge}: preview {:?}, clip {clip_rect:?}, planned {planned:?}, pps {}",
                preview.rect,
                h.state().ui.timeline.pps
            );
            assert_eq!(
                planned.duration,
                if kind == TrackKind::Video {
                    h.state().session.prefs.timeline.video_transition_duration(rate)
                } else {
                    h.state().session.prefs.timeline.audio_transition_duration(rate)
                }
            );
            if let Some(dir) = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR") {
                let dir = std::path::PathBuf::from(dir);
                std::fs::create_dir_all(&dir).unwrap();
                h.render().unwrap().save(dir.join(format!("transition-{effect}-{edge}.png"))).unwrap();
            }
        }
        let to = pos2(clip_rect.left() + clip_rect.width() * 0.8, clip_rect.center().y);
        let (_, planned) = preview_transition(&h.state().session, &json!({"effect":effect,"clip":clip.0,"edge":"out"}), kind).unwrap();
        pointer(&mut h, to, Some(false));
        let applied = h.state().session.active_sequence().unwrap().tracks(kind)[0].transitions.last().unwrap();
        assert_eq!((applied.start, applied.duration, applied.from, applied.to), (planned.start, planned.duration, planned.from, planned.to));
        h.state_mut().session.execute("edit.undo", json!({})).unwrap();
        assert_eq!(h.state().session.project.to_json(), before);
        step(&mut h);
        assert!(!h.state().auto.elements.iter().any(|e| e.id == "timeline.transitionDropPreview"));
    }
}

#[test]
fn incompatible_and_locked_tracks_do_not_offer_a_transition_preview() {
    for locked in [false, true] {
        let mut h = harness("Cross Dissolve");
        let kind = if locked { TrackKind::Video } else { TrackKind::Audio };
        let q = h.state().session.active_sequence().unwrap();
        let (track, clip) = (q.tracks(kind)[0].id, q.tracks(kind)[0].items[1].id);
        if locked {
            let s = &mut h.state_mut().session;
            std::sync::Arc::make_mut(&mut s.project).sequence_mut(s.state.active_sequence.unwrap()).unwrap().track_mut(track).unwrap().locked = true;
            step(&mut h);
        }
        let to = rect(&h, &format!("timeline.clip.{}", clip.0)).center();
        let from = rect(&h, "effects.item.cross_dissolve").center();
        let before = h.state().session.project.to_json();
        pointer(&mut h, from, Some(true));
        pointer(&mut h, from + vec2(0.0, 10.0), None);
        pointer(&mut h, to, None);
        assert!(!h.state().auto.elements.iter().any(|e| e.id == "timeline.transitionDropPreview"));
        pointer(&mut h, to, Some(false));
        assert_eq!(h.state().session.project.to_json(), before);
    }
}
