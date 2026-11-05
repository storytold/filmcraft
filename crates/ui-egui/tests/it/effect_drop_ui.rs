//! Effect drops target the selected group, preserve selection and undo as one edit.
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
