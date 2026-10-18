//! Dragging clips previews the transitions the drop will carry, exactly as the drop re-adds them
//! (`edit::move_items`): a fade follows its clip, a cut transition travels with its clips only
//! when every one of them moves — dragging a clip out from under one side of a cut leaves the
//! transition where it was until the release deletes it (Premiere) — an Option-drag adds a
//! traveling copy while the original stays.

use egui::{Event, Modifiers, PointerButton, Pos2, Rect, pos2, vec2};
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

fn pointer(h: &mut Harness<'static, FilmcraftApp>, pos: Pos2, pressed: Option<bool>) {
    h.input_mut().events.push(Event::PointerMoved(pos));
    if let Some(pressed) = pressed {
        h.input_mut().events.push(Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
    }
    step(h);
}

fn rect(h: &Harness<'static, FilmcraftApp>, id: &str) -> Rect {
    let [x, y, w, height] = h.state().auto.elements.iter().find(|e| e.id == id).unwrap_or_else(|| panic!("missing {id}")).rect;
    Rect::from_min_size(pos2(x, y), vec2(w, height))
}

fn build(s: Session) -> Harness<'static, FilmcraftApp> {
    let app = FilmcraftApp::new(s);
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

/// A sequence with a cross dissolve: between two clips at 0..48f and 48..96f, or a fade-in on the
/// single clip at 0..48f when `single`.
fn harness(single: bool) -> Harness<'static, FilmcraftApp> {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("file.newSequence", json!({"name":"Transition move","fps":24})).unwrap();
    let rate = s.sequence_rate();
    for (at, frames) in if single { vec![(0, 48)] } else { vec![(0, 48), (48, 48)] } {
        s.execute("timeline.place", json!({"item":5,"time":rate.tick_of(at).0,"duration":rate.tick_of(frames).0})).unwrap();
    }
    let seq = s.active_sequence().unwrap().clone();
    let clip = seq.video_tracks[0].items[if single { 0 } else { 1 }].id;
    s.execute("effects.apply", json!({"effect": "cross_dissolve", "clip": clip.0, "edge": "in"})).unwrap();
    build(s)
}

/// XYZ with a cut transition on each cut: X↔Y and Y↔Z.
fn harness_xyz() -> Harness<'static, FilmcraftApp> {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("file.newSequence", json!({"name":"Transition move","fps":24})).unwrap();
    let rate = s.sequence_rate();
    for (at, frames) in [(0, 48), (48, 48), (96, 48)] {
        s.execute("timeline.place", json!({"item":5,"time":rate.tick_of(at).0,"duration":rate.tick_of(frames).0})).unwrap();
    }
    let seq = s.active_sequence().unwrap().clone();
    let (y, z) = (seq.video_tracks[0].items[1].id, seq.video_tracks[0].items[2].id);
    for clip in [y, z] {
        s.execute("effects.apply", json!({"effect": "cross_dissolve", "clip": clip.0, "edge": "in"})).unwrap();
    }
    build(s)
}

/// A drag preview that keeps a transition's offset to its clip (it follows, not lags), and a drop
/// that re-adds the transition shifted by exactly the move.
#[test]
fn dragging_a_clip_carries_its_fade_in_the_preview_and_on_the_drop() {
    let mut h = harness(true);
    let q0 = h.state().session.active_sequence().unwrap().clone();
    let (a0, tr0) = (q0.video_tracks[0].items[0].clone(), q0.video_tracks[0].transitions[0].clone());
    let a_rect = rect(&h, &format!("timeline.clip.{}", a0.id.0));
    let tr_rect = rect(&h, &format!("timeline.transition.{}", tr0.id.0));
    assert_eq!(tr0.to, Some(a0.id), "the fade-in sits on the dragged clip");
    let gap_before = tr_rect.min.x - a_rect.min.x;
    let y = a_rect.center().y;
    // press clear of the fade band (it covers the left 75 px of the clip) and of the edge zones
    let x0 = a_rect.left() + 110.0;
    pointer(&mut h, pos2(x0, y), Some(true));
    for i in 1..=6 {
        pointer(&mut h, pos2(x0 + 20.0 * i as f32, y), None);
    }
    // mid-drag: the fade follows the clip's ghost, keeping its offset to the clip edge
    let gap_mid = rect(&h, &format!("timeline.transition.{}", tr0.id.0)).min.x - rect(&h, &format!("timeline.clip.{}", a0.id.0)).min.x;
    assert!((gap_mid - gap_before).abs() < 1.0, "fade lagged behind: gap {gap_before} -> {gap_mid}");
    pointer(&mut h, pos2(x0 + 140.0, y), Some(false));
    let q1 = h.state().session.active_sequence().unwrap().clone();
    let (a1, tr1) = (q1.video_tracks[0].items[0].clone(), q1.video_tracks[0].transitions[0].clone());
    assert_eq!(a1.start.0 - a0.start.0, tr1.start.0 - tr0.start.0, "the committed transition moved with the clip");
    assert_eq!((tr1.from, tr1.duration), (tr0.from, tr0.duration));
}

/// Both clips of a cut transition move together: the transition travels with them in the preview
/// and is re-added at the shared delta.
#[test]
fn dragging_both_clips_of_a_cut_carries_the_transition() {
    let mut h = harness(false);
    let q0 = h.state().session.active_sequence().unwrap().clone();
    let (a0, b0, tr0) = (q0.video_tracks[0].items[0].clone(), q0.video_tracks[0].items[1].clone(), q0.video_tracks[0].transitions[0].clone());
    h.state_mut().session.execute("timeline.select", json!({"clips": [a0.id.0, b0.id.0]})).unwrap();
    step(&mut h);
    let a_rect = rect(&h, &format!("timeline.clip.{}", a0.id.0));
    let tr_rect = rect(&h, &format!("timeline.transition.{}", tr0.id.0));
    let gap_before = tr_rect.min.x - a_rect.min.x;
    let y = a_rect.center().y;
    pointer(&mut h, a_rect.center(), Some(true));
    for i in 1..=6 {
        pointer(&mut h, pos2(a_rect.center().x + 20.0 * i as f32, y), None);
    }
    let gap_mid = rect(&h, &format!("timeline.transition.{}", tr0.id.0)).min.x - rect(&h, &format!("timeline.clip.{}", a0.id.0)).min.x;
    assert!((gap_mid - gap_before).abs() < 1.0, "transition lagged: gap {gap_before} -> {gap_mid}");
    pointer(&mut h, pos2(a_rect.center().x + 140.0, y), Some(false));
    let q1 = h.state().session.active_sequence().unwrap().clone();
    let (a1, b1, tr1) = (q1.video_tracks[0].items[0].clone(), q1.video_tracks[0].items[1].clone(), q1.video_tracks[0].transitions[0].clone());
    assert_eq!(a1.start.0 - a0.start.0, tr1.start.0 - tr0.start.0);
    assert_eq!(b1.start.0 - b0.start.0, tr1.start.0 - tr0.start.0, "the transition moved with both clips");
}

/// XYZ with cut transitions on both cuts: dragging YZ leaves the X↔Y transition where it is
/// while dragging (a preview of the drop that deletes it, as in Premiere) and the Y↔Z one
/// travels with the pair.
#[test]
fn dragging_two_clips_drops_the_transition_on_the_cut_that_stayed() {
    let mut h = harness_xyz();
    let q0 = h.state().session.active_sequence().unwrap().clone();
    let (x0, y0, z0) = (q0.video_tracks[0].items[0].clone(), q0.video_tracks[0].items[1].clone(), q0.video_tracks[0].items[2].clone());
    let (xy0, yz0) = (q0.video_tracks[0].transitions[0].clone(), q0.video_tracks[0].transitions[1].clone());
    h.state_mut().session.execute("timeline.select", json!({"clips": [y0.id.0, z0.id.0]})).unwrap();
    step(&mut h);
    let y_rect = rect(&h, &format!("timeline.clip.{}", y0.id.0));
    let xy_rect = rect(&h, &format!("timeline.transition.{}", xy0.id.0));
    let yz_rect = rect(&h, &format!("timeline.transition.{}", yz0.id.0));
    let gap_before = yz_rect.min.x - y_rect.min.x;
    let y_center = y_rect.center().y;
    // press in the middle of Y, clear of both transition bands and the edge zones
    let x0p = y_rect.left() + 60.0;
    pointer(&mut h, pos2(x0p, y_center), Some(true));
    for i in 1..=6 {
        pointer(&mut h, pos2(x0p + 20.0 * i as f32, y_center), None);
    }
    // mid-drag: the X↔Y transition stays on its cut (the drop will remove it), the Y↔Z one
    // follows the pair
    assert_eq!(rect(&h, &format!("timeline.transition.{}", xy0.id.0)), xy_rect, "the X↔Y transition stays on its cut while dragging");
    let gap_mid = rect(&h, &format!("timeline.transition.{}", yz0.id.0)).min.x - rect(&h, &format!("timeline.clip.{}", y0.id.0)).min.x;
    assert!((gap_mid - gap_before).abs() < 1.0, "Y↔Z transition lagged: gap {gap_before} -> {gap_mid}");
    pointer(&mut h, pos2(x0p + 140.0, y_center), Some(false));
    let q1 = h.state().session.active_sequence().unwrap().clone();
    let (x1, y1, z1) = (q1.video_tracks[0].items[0].clone(), q1.video_tracks[0].items[1].clone(), q1.video_tracks[0].items[2].clone());
    assert_eq!(x1.start, x0.start, "X is not part of the drag");
    assert_eq!(y1.start.0 - y0.start.0, z1.start.0 - z0.start.0, "Y and Z moved together");
    assert_eq!(q1.video_tracks[0].transitions.len(), 1, "the X↔Y transition was dropped with the move");
    assert!(!q1.video_tracks[0].transitions.iter().any(|t| t.id == xy0.id), "its id is gone");
    let yz1 = &q1.video_tracks[0].transitions[0];
    assert_eq!(yz1.start.0 - yz0.start.0, y1.start.0 - y0.start.0, "the Y↔Z transition traveled with the pair");
}

/// Option-drag copies the clip and its fade: the original keeps its transition at home, the copy
/// shows a ghost beside it while dragging and keeps its own transition on the drop.
#[test]
fn option_drag_copies_show_a_ghost_transition_with_the_copy() {
    let mut h = harness(true);
    let q0 = h.state().session.active_sequence().unwrap().clone();
    let a0 = q0.video_tracks[0].items[0].clone();
    let tr0 = q0.video_tracks[0].transitions[0].clone();
    let a_rect = rect(&h, &format!("timeline.clip.{}", a0.id.0));
    let tr_rect = rect(&h, &format!("timeline.transition.{}", tr0.id.0));
    let y = a_rect.center().y;
    // hold Option for the whole drag, as the real drag does
    h.input_mut().events.push(egui::Event::ModifiersChanged(Modifiers { alt: true, ..Default::default() }));
    let x0 = a_rect.left() + 110.0; // clear of the fade band and the edge zones
    pointer(&mut h, pos2(x0, y), Some(true));
    for i in 1..=6 {
        pointer(&mut h, pos2(x0 + 20.0 * i as f32, y), None);
    }
    // a ghost transition beside the copy; the original stays exactly where it was
    let ghost = rect(&h, &format!("timeline.transition.{}.copy", tr0.id.0));
    assert_eq!(rect(&h, &format!("timeline.transition.{}", tr0.id.0)), tr_rect, "the original transition stays at home");
    assert!((ghost.min.x - tr_rect.min.x) > 60.0, "ghost {ghost:?} did not follow the copy from {tr_rect:?}");
    pointer(&mut h, pos2(x0 + 140.0, y), Some(false));
    h.input_mut().events.push(egui::Event::ModifiersChanged(Modifiers::NONE));
    step(&mut h);
    let q1 = h.state().session.active_sequence().unwrap().clone();
    assert_eq!(q1.video_tracks[0].transitions.len(), 2, "the copy carried its own transition");
    let (home, copy_tr) = (q1.video_tracks[0].transitions[0].clone(), q1.video_tracks[0].transitions[1].clone());
    assert_eq!(home.start, tr0.start, "the original's transition stayed");
    assert_ne!(copy_tr.id, tr0.id, "the copy's transition is a fresh one");
    let copy = q1.video_tracks[0].items.iter().find(|i| i.id != a0.id).expect("a new clip appeared");
    assert_eq!(copy_tr.to, Some(copy.id), "the copy's fade sits on the copy");
}

/// Dragging one side of a cut transition to another track drops it (Premiere); the preview leaves
/// it where it is, and the drop removes it.
#[test]
fn dragging_one_side_to_another_track_drops_the_transition() {
    let mut h = harness(false);
    // a clip far to the right on V2 marks that track's row
    let q = h.state().session.active_sequence().unwrap().clone();
    let (v2, rate) = (q.video_tracks[1].id, h.state().session.sequence_rate());
    h.state_mut().session.execute("timeline.place", json!({"item":5,"time":rate.tick_of(200).0,"duration":rate.tick_of(24).0,"track": v2.0})).unwrap();
    step(&mut h);
    let q0 = h.state().session.active_sequence().unwrap().clone();
    let (a0, tr0) = (q0.video_tracks[0].items[0].clone(), q0.video_tracks[0].transitions[0].clone());
    let a_rect = rect(&h, &format!("timeline.clip.{}", a0.id.0));
    let v2_rect = rect(&h, &format!("timeline.clip.{}", q0.video_tracks[1].items[0].id.0));
    let tr_rect = rect(&h, &format!("timeline.transition.{}", tr0.id.0));
    pointer(&mut h, a_rect.center(), Some(true));
    for i in 1..=6 {
        let p = pos2(a_rect.center().x + 20.0 * i as f32, a_rect.center().y + (v2_rect.center().y - a_rect.center().y) * i as f32 / 6.0);
        // move into the V2 row first (track crossing), so the pointer ends up there
        pointer(&mut h, p, None);
    }
    assert_eq!(rect(&h, &format!("timeline.transition.{}", tr0.id.0)), tr_rect, "the doomed transition stays at home while dragging");
    pointer(&mut h, pos2(a_rect.center().x + 140.0, v2_rect.center().y), Some(false));
    let q1 = h.state().session.active_sequence().unwrap().clone();
    assert!(q1.video_tracks[0].transitions.is_empty(), "the clip moved to V2 and its transition was dropped");
    assert!(q1.video_tracks.iter().any(|t| t.items.iter().any(|i| i.id == a0.id && t.id == v2)), "the clip landed on V2");
}
