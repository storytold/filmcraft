//! Transition drag feedback: snaps to Start / Center / End at Cut, actual span and alignment, no
//! edit until release, one undo step.
use egui::{Event, Modifiers, PointerButton, Pos2, Rect, pos2, vec2};
use egui_kittest::Harness;
use filmcraft_engine::{Session, commands::preview_transition};
use filmcraft_project::TrackKind;
use filmcraft_time::Tick;
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
    // Two 2 s clips: the cut between them has room on both sides for every alignment.
    for (at, frames) in [(0, 48), (48, 48)] {
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
fn transition_drag_snaps_to_start_center_and_end_at_cut_and_commits_that_span() {
    for (kind, effect, name) in [(TrackKind::Video, "cross_dissolve", "Cross Dissolve"), (TrackKind::Audio, "constant_power", "Constant Power")] {
        let mut h = harness(name);
        let q = h.state().session.active_sequence().unwrap();
        let rate = q.settings.frame_rate;
        let clip = q.tracks(kind)[0].items[1].id;
        let clip_rect = rect(&h, &format!("timeline.clip.{}", clip.0));
        let from = rect(&h, &format!("effects.item.{effect}")).center();
        let before = h.state().session.project.to_json();
        let dur = if kind == TrackKind::Video {
            h.state().session.prefs.timeline.video_transition_duration(rate)
        } else {
            h.state().session.prefs.timeline.audio_transition_duration(rate)
        };
        let half = dur.mul_ratio(1, 2);
        // The cut is the second clip's start; the pointer lands on the outgoing clip (End),
        // on the cut itself (Center) or on the incoming clip (Start).
        let cut = clip_rect.left();
        let px_per_tick = clip_rect.width() / rate.tick_of(48).0 as f32;
        let x_of = |t: Tick| cut + (t - rate.tick_of(48)).0 as f32 * px_per_tick;
        let content = rect(&h, "timeline.tracks");
        let mut expected = Vec::new();
        pointer(&mut h, from, Some(true));
        pointer(&mut h, from + vec2(0.0, 10.0), None);
        for (align, label, tick) in [
            ("end", "End at Cut", rate.tick_of(30)),
            ("center", "Center at Cut", rate.tick_of(50)),
            ("start", "Start at Cut", rate.tick_of(48) + half + rate.tick_of(2)),
        ] {
            let to = pos2(x_of(tick), clip_rect.center().y);
            pointer(&mut h, to, None);
            let preview = h.state().auto.elements.iter().find(|e| e.id == "timeline.transitionDropPreview").expect("transition preview while dragging");
            assert_eq!(preview.label, label);
            assert_eq!(h.state().session.project.to_json(), before, "hover must not edit");
            let (_, planned) = preview_transition(&h.state().session, &json!({"effect":effect,"clip":clip.0,"edge":"in","align":align}), kind).unwrap();
            let expected_left = (cut + (planned.start - rate.tick_of(48)).0 as f32 * px_per_tick).max(content.left());
            let expected_right = (expected_left + planned.duration.0 as f32 * px_per_tick).min(content.right());
            assert!(
                (preview.rect[0] - expected_left).abs() < 1.0 && (preview.rect[2] - (expected_right - expected_left)).abs() < 1.0,
                "{kind:?} {align}: preview {:?}, cut {clip_rect:?}, planned {planned:?}, pps {}",
                preview.rect,
                h.state().ui.timeline.pps
            );
            assert_eq!(planned.duration, dur);
            // the plan honours the alignment: End at Cut ends on the cut, Start at Cut begins on it
            let expected_start = match align {
                "end" => rate.tick_of(48) - dur,
                "start" => rate.tick_of(48),
                _ => rate.snap(rate.tick_of(48) - half),
            };
            assert_eq!(planned.start, expected_start, "{align}");
            expected.push((align, planned, to));
            if let Some(dir) = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR") {
                let dir = std::path::PathBuf::from(dir);
                std::fs::create_dir_all(&dir).unwrap();
                h.render().unwrap().save(dir.join(format!("transition-{effect}-{align}.png"))).unwrap();
            }
        }
        // Release on the Start at Cut position: the committed transition is exactly the plan.
        let (align, planned, to) = expected.pop().unwrap();
        pointer(&mut h, to, Some(false));
        let applied = h.state().session.active_sequence().unwrap().tracks(kind)[0].transitions.last().unwrap();
        assert_eq!((applied.start, applied.duration, applied.from, applied.to), (planned.start, planned.duration, planned.from, planned.to), "{align}");
        h.state_mut().session.execute("edit.undo", json!({})).unwrap();
        assert_eq!(h.state().session.project.to_json(), before);
        step(&mut h);
        assert!(!h.state().auto.elements.iter().any(|e| e.id == "timeline.transitionDropPreview"));
    }
}

#[test]
fn transition_drag_at_a_one_sided_clip_end_stays_on_the_edge() {
    let mut h = harness("Cross Dissolve");
    let q = h.state().session.active_sequence().unwrap();
    let rate = q.settings.frame_rate;
    let clip = q.video_tracks[0].items[1].id;
    let clip_rect = rect(&h, &format!("timeline.clip.{}", clip.0));
    let from = rect(&h, "effects.item.cross_dissolve").center();
    let before = h.state().session.project.to_json();
    let dur = h.state().session.prefs.timeline.video_transition_duration(rate);
    // The last clip's Out edge: a fade to nothing, which keeps its edge whatever the pointer says.
    let px_per_tick = clip_rect.width() / rate.tick_of(48).0 as f32;
    let to = pos2(clip_rect.right() - rate.tick_of(6).0 as f32 * px_per_tick, clip_rect.center().y);
    pointer(&mut h, from, Some(true));
    pointer(&mut h, from + vec2(0.0, 10.0), None);
    pointer(&mut h, to, None);
    let preview = h.state().auto.elements.iter().find(|e| e.id == "timeline.transitionDropPreview").expect("transition preview while dragging");
    assert_eq!(preview.label, "End at Cut");
    let (_, planned) =
        preview_transition(&h.state().session, &json!({"effect":"cross_dissolve","clip":clip.0,"edge":"out","align":"center"}), TrackKind::Video).unwrap();
    assert_eq!(planned.start, rate.tick_of(96) - dur, "a fade to nothing ends on its clip's edge");
    let content = rect(&h, "timeline.tracks");
    let expected_left = (clip_rect.right() + (planned.start - rate.tick_of(96)).0 as f32 * px_per_tick).max(content.left());
    let expected_right = (expected_left + planned.duration.0 as f32 * px_per_tick).min(content.right());
    assert!(
        (preview.rect[0] - expected_left).abs() < 1.0 && (preview.rect[2] - (expected_right - expected_left)).abs() < 1.0,
        "one-sided preview {:?}, planned {planned:?}",
        preview.rect
    );
    assert_eq!(h.state().session.project.to_json(), before, "hover must not edit");
    pointer(&mut h, to, Some(false));
    let applied = h.state().session.active_sequence().unwrap().video_tracks[0].transitions.last().unwrap();
    assert_eq!((applied.start, applied.duration, applied.from, applied.to), (planned.start, planned.duration, planned.from, planned.to));
    h.state_mut().session.execute("edit.undo", json!({})).unwrap();
    assert_eq!(h.state().session.project.to_json(), before);
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
