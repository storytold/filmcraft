//! Effect drops target the selected group, preserve selection and undo as one edit.
use egui::{Event, Modifiers, PointerButton, Pos2, Rect, pos2, vec2};
use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use serde_json::json;

/// Advance one egui frame, forwarding synthetic input through the desktop input hook.
fn step(h: &mut Harness<'static, FilmcraftApp>) {
    let ctx = h.ctx.clone();
    let mut raw = std::mem::take(h.input_mut());
    eframe::App::raw_input_hook(h.state_mut(), &ctx, &mut raw);
    *h.input_mut() = raw;
    ctx.request_repaint();
    h.step();
}

/// Move the pointer and optionally press or release its primary button across frames.
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

/// Find the current screen bounds of an automation element, failing if it is missing.
fn rect(h: &Harness<'static, FilmcraftApp>, id: &str) -> Rect {
    let [x, y, w, height] = h.state().auto.elements.iter().find(|e| e.id == id).unwrap_or_else(|| panic!("missing {id}")).rect;
    Rect::from_min_size(pos2(x, y), vec2(w, height))
}

/// Build an Effects workspace with three linked clips and a deterministic Timeline zoom.
fn harness(effect: &str) -> Harness<'static, FilmcraftApp> {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("file.newSequence", json!({"name":"Effect drops","fps":24})).unwrap();
    let rate = s.sequence_rate();
    // Three clips let us distinguish dropping on the selected group from an unselected clip.
    for (at, frames) in [(0, 48), (48, 48), (96, 48)] {
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

/// Verify real effect and preset drops, their hover targets, selection, and single-step undo/redo.
#[test]
fn effect_drop_targets_selected_group_or_unselected_clip_and_undoes_once() {
    for (search, source, effect) in [
        ("Gaussian Blur", "effects.item.gaussian_blur", "gaussian_blur"),
        ("Blur In", "effects.preset.Blur In", "gaussian_blur"),
        ("Steel Blue", "effects.lumetriPreset.Steel Blue", "lumetri"),
    ] {
        for selected_target in [true, false] {
            let mut h = harness(search);
            let ids: Vec<_> = h.state().session.active_sequence().unwrap().video_tracks[0].items.iter().map(|i| i.id).collect();
            let selection = [ids[0], ids[1]];
            h.state_mut().session.execute("timeline.select", json!({"clips": selection.iter().map(|c| c.0).collect::<Vec<_>>()})).unwrap();
            step(&mut h);
            let selection = h.state().session.state.selection.clone();
            let target = if selected_target { ids[0] } else { ids[2] };
            let expected = if selected_target { selection.clone() } else { vec![target] };
            let from = rect(&h, source).center();
            let to = rect(&h, &format!("timeline.clip.{}", target.0)).center();
            let before = h.state().session.project.to_json();
            pointer(&mut h, from, Some(true));
            pointer(&mut h, from + vec2(0.0, 10.0), None);
            pointer(&mut h, to, None);
            assert_eq!(h.state().session.project.to_json(), before, "hover must not edit");
            for id in &ids {
                assert_eq!(h.state().auto.elements.iter().any(|e| e.id == format!("timeline.effectDropPreview.{}", id.0)), expected.contains(id));
            }
            if let Some(dir) = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR") {
                let dir = std::path::PathBuf::from(dir);
                std::fs::create_dir_all(&dir).unwrap();
                h.render().unwrap().save(dir.join(format!("effect-drop-{search}-{selected_target}.png"))).unwrap();
            }
            pointer(&mut h, to, Some(false));
            let expected_selection = filmcraft_engine::commands::with_links(&h.state().session, &expected);
            assert_eq!(h.state().session.state.selection, expected_selection);
            let q = h.state().session.active_sequence().unwrap();
            for id in &ids {
                let (_, item) = q.find_item(*id).unwrap();
                assert_eq!(item.effects.iter().filter(|e| e.effect == effect).count(), usize::from(expected.contains(id)));
            }
            let after = h.state().session.project.to_json();
            assert_ne!(before, after);
            h.state_mut().session.execute("edit.undo", json!({})).unwrap();
            assert_eq!(h.state().session.project.to_json(), before, "one undo removes the whole drop");
            h.state_mut().session.execute("edit.redo", json!({})).unwrap();
            assert_eq!(h.state().session.project.to_json(), after);
        }
    }
}

/// Keep a partially offscreen selected clip's effect outline out of the Timeline headers.
#[test]
fn selected_effect_preview_is_clipped_when_another_selected_clip_is_offscreen() {
    let mut h = harness("Gaussian Blur");
    let ids: Vec<_> = h.state().session.active_sequence().unwrap().video_tracks[0].items.iter().map(|i| i.id).collect();
    h.state_mut().session.execute("timeline.select", json!({"clips": [ids[0].0, ids[1].0]})).unwrap();
    h.state_mut().ui.timeline.pps = 200.0;
    h.state_mut().ui.timeline.target_pps = 200.0;
    h.state_mut().ui.timeline.scroll = 1.0;
    h.state_mut().ui.timeline.target_scroll = 1.0;
    step(&mut h);
    let content = rect(&h, "timeline.tracks");
    let from = rect(&h, "effects.item.gaussian_blur").center();
    let target = rect(&h, &format!("timeline.clip.{}", ids[1].0));
    let to = pos2(target.right().min(content.right()) - 20.0, target.center().y);
    let before = h.state().session.project.to_json();
    pointer(&mut h, from, Some(true));
    pointer(&mut h, from + vec2(0.0, 10.0), None);
    pointer(&mut h, to, None);
    let preview = rect(&h, &format!("timeline.effectDropPreview.{}", ids[0].0));
    // The first clip spans 0..2 s while the view begins at 1 s, so half its outline is
    // geometrically over the headers. Its paint clip must prevent those pixels from appearing.
    let expected = Rect::from_min_max(pos2(content.left() - 200.0, preview.top()), pos2(content.left() + 200.0, preview.bottom()));
    let outlines: Vec<_> = h
        .output()
        .shapes
        .iter()
        .filter(
            |shape| matches!(&shape.shape, egui::Shape::Rect(r) if r.rect == expected && r.stroke.width == 2.0 && r.stroke.color == h.state().tokens.accent),
        )
        .collect();
    assert!(!outlines.is_empty(), "the offscreen clip still has a drop outline");
    for outline in outlines {
        assert!(content.contains_rect(outline.clip_rect), "preview paint clip {:?} escapes {content:?}", outline.clip_rect);
    }
    assert_eq!(h.state().session.project.to_json(), before, "hover must not edit");
    if let Some(dir) = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        h.render().unwrap().save(dir.join("effect-drop-offscreen.png")).unwrap();
    }
}

/// Preview linked audio recipients once even when linked selection is disabled or partly explicit.
#[test]
fn audio_effect_preview_includes_linked_recipients_and_deduplicates_them() {
    for include_audio in [false, true] {
        let mut h = harness("Parametric Equalizer");
        let q = h.state().session.active_sequence().unwrap();
        let video: Vec<_> = q.video_tracks[0].items.iter().map(|i| i.id).collect();
        let audio: Vec<_> = q.audio_tracks[0].items.iter().map(|i| i.id).collect();
        let mut selection = vec![video[0], video[1]];
        if include_audio {
            selection.push(audio[0]);
        }
        h.state_mut().session.state.linked_selection = false;
        h.state_mut().session.execute("timeline.select", json!({"clips": selection.iter().map(|c| c.0).collect::<Vec<_>>()})).unwrap();
        step(&mut h);
        let before = h.state().session.project.to_json();
        let from = rect(&h, "effects.item.parametric_eq").center();
        let to = rect(&h, &format!("timeline.clip.{}", video[0].0)).center();
        pointer(&mut h, from, Some(true));
        pointer(&mut h, from + vec2(0.0, 10.0), None);
        pointer(&mut h, to, None);
        for (i, id) in video.iter().enumerate().chain(audio.iter().enumerate()) {
            let count = h.state().auto.elements.iter().filter(|e| e.id == format!("timeline.effectDropPreview.{}", id.0)).count();
            assert_eq!(count, usize::from(i < 2), "preview recipient {id:?}");
        }
        assert_eq!(h.state().session.project.to_json(), before, "hover must not edit");
        pointer(&mut h, to, Some(false));
        assert_eq!(h.state().session.state.selection, selection);
        let q = h.state().session.active_sequence().unwrap();
        for (i, id) in audio.iter().enumerate() {
            assert_eq!(q.find_item(*id).unwrap().1.effects.iter().filter(|e| e.effect == "parametric_eq").count(), usize::from(i < 2));
        }
        for id in &video {
            assert!(!q.find_item(*id).unwrap().1.effects.iter().any(|e| e.effect == "parametric_eq"));
        }
        let after = h.state().session.project.to_json();
        h.state_mut().session.execute("edit.undo", json!({})).unwrap();
        assert_eq!(h.state().session.project.to_json(), before);
        h.state_mut().session.execute("edit.redo", json!({})).unwrap();
        assert_eq!(h.state().session.project.to_json(), after);
    }
}

/// Confine scrolled previews to their own track panes and omit fully hidden outlines.
#[test]
fn effect_preview_stays_in_its_video_or_audio_pane_after_vertical_scroll() {
    for (case, video_scroll, audio_scroll, audio_height) in
        [("video bottom", 50.0, 0.0, 100.0), ("audio top", 0.0, 50.0, 100.0), ("audio bottom", 0.0, 0.0, 200.0), ("hidden video", 10_000.0, 0.0, 100.0)]
    {
        let mut h = harness("Parametric Equalizer");
        let q = h.state().session.active_sequence().unwrap();
        let video = q.video_tracks[0].items[0].id;
        let audio = q.audio_tracks[0].items[0].id;
        h.state_mut().session.execute("timeline.select", json!({"clips": [video.0, audio.0]})).unwrap();
        h.state_mut().ui.timeline.video_track_h = 100.0;
        h.state_mut().ui.timeline.audio_track_h = audio_height;
        h.state_mut().ui.timeline.v_scroll = video_scroll;
        h.state_mut().ui.timeline.a_scroll = audio_scroll;
        step(&mut h);
        let layout = h.state().tl.layout.as_ref().unwrap().clone();
        let hovered = if case == "video bottom" { video } else { audio };
        let (tid, it) = h.state().session.active_sequence().unwrap().find_item(hovered).unwrap();
        let row = layout.rows.iter().find(|r| r.track == tid).unwrap();
        let shown = layout.shown(row).intersect(layout.content);
        let to = pos2((layout.x_of(it.start) + layout.x_of(it.end())) / 2.0, shown.center().y);
        assert!(shown.is_positive(), "{case}: hover stays in a visible track");
        let from = rect(&h, "effects.item.parametric_eq").center();
        let before = h.state().session.project.to_json();
        pointer(&mut h, from, Some(true));
        pointer(&mut h, from + vec2(0.0, 10.0), None);
        pointer(&mut h, to, None);
        for id in [video, audio] {
            let (tid, it) = h.state().session.active_sequence().unwrap().find_item(id).unwrap();
            let row = layout.rows.iter().find(|r| r.track == tid).unwrap();
            let expected = Rect::from_min_max(pos2(layout.x_of(it.start), row.rect.top()), pos2(layout.x_of(it.end()), row.rect.bottom()));
            let pane = layout.shown(row).intersect(layout.content);
            let visible = expected.intersect(pane).is_positive();
            let previews: Vec<_> = h.state().auto.elements.iter().filter(|e| e.id == format!("timeline.effectDropPreview.{}", id.0)).collect();
            assert_eq!(previews.len(), usize::from(visible), "{case}: preview visibility for {id:?}");
            let outlines: Vec<_> = h
                .output()
                .shapes
                .iter()
                .filter(
                    |s| matches!(&s.shape, egui::Shape::Rect(r) if r.rect == expected && r.stroke.width == 2.0 && r.stroke.color == h.state().tokens.accent),
                )
                .collect();
            assert_eq!(!outlines.is_empty(), visible, "{case}: painted outline visibility");
            for outline in outlines {
                assert!(pane.contains_rect(outline.clip_rect), "{case}: {:?} escapes {pane:?}", outline.clip_rect);
            }
            for preview in previews {
                let [x, y, w, height] = preview.rect;
                assert!(pane.contains_rect(Rect::from_min_size(pos2(x, y), vec2(w, height))), "{case}: automation must match the visible preview");
            }
        }
        assert_eq!(h.state().session.project.to_json(), before, "hover must not edit");
        if let Some(dir) = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR") {
            let dir = std::path::PathBuf::from(dir);
            std::fs::create_dir_all(&dir).unwrap();
            h.render().unwrap().save(dir.join(format!("effect-drop-{case}.png"))).unwrap();
        }
    }
}
