use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_time::{FrameRate, Tick};
use filmcraft_ui_egui::{FilmcraftApp, dock::PanelKind, menus};
use serde_json::json;
use std::sync::{Arc, Mutex};

fn step(h: &mut Harness<'static, FilmcraftApp>) {
    let ctx = h.ctx.clone();
    let mut raw = std::mem::take(h.input_mut());
    eframe::App::raw_input_hook(h.state_mut(), &ctx, &mut raw);
    *h.input_mut() = raw;
    ctx.request_repaint();
    h.step();
}
fn harness() -> Harness<'static, FilmcraftApp> {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name":"Program clip","width":128,"height":72,"fps":24})).unwrap();
    let item = filmcraft_engine::demo::add_generator(
        Arc::make_mut(&mut s.project),
        &s.media,
        filmcraft_media::generators::GeneratorSource::new(
            filmcraft_media::Generator::Demo(filmcraft_media::DemoScene::Aurora),
            128,
            72,
            FrameRate::FPS_24,
            Tick::from_seconds_f64(5.0),
        ),
        "Source clip",
        filmcraft_project::Label::Iris,
        None,
    );
    s.execute("source.open", json!({"item":item.0})).unwrap();
    let mut h = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).build_eframe(move |_cc| FilmcraftApp::new(s));
    for _ in 0..3 {
        step(&mut h);
    }
    h
}
fn click(h: &mut Harness<'static, FilmcraftApp>, id: &str) {
    fn center(h: &Harness<'static, FilmcraftApp>, id: &str) -> egui::Pos2 {
        let e = h.state().auto.elements.iter().find(|e| e.id == id).or_else(|| h.state().auto.find(id)).unwrap_or_else(|| {
            panic!(
                "missing {id}; available {:?}",
                h.state().auto.elements.iter().filter(|e| e.id.starts_with("exportFrame")).map(|e| (&e.id, &e.rect, &e.label)).collect::<Vec<_>>()
            )
        });
        let [x, y, w, height] = e.rect;
        egui::pos2(x + w / 2.0, y + height / 2.0)
    }
    step(h);
    let pos = center(h, id);
    h.input_mut().events.push(egui::Event::PointerMoved(pos));
    step(h);
    let pos = center(h, id);
    h.input_mut().events.push(egui::Event::PointerMoved(pos));
    for pressed in [true, false] {
        h.input_mut().events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
        step(h);
    }
    step(h);
}
fn has(h: &Harness<'static, FilmcraftApp>, id: &str) -> bool {
    h.state().auto.elements.iter().any(|e| e.id == id)
}

#[test]
fn both_camera_buttons_open_the_dialog_and_cancel_writes_nothing() {
    for monitor in ["source", "program"] {
        let mut h = harness();
        let saves = Arc::new(Mutex::new(0));
        let saved = saves.clone();
        h.state_mut().hooks.pick_save_as = Some(Box::new(move |_, _, _| {
            *saved.lock().unwrap() += 1;
            None
        }));
        click(&mut h, &format!("{monitor}.transport.exportFrame"));
        assert!(has(&h, "exportFrame.name"), "{monitor}: {}", h.state().ui.status);
        let top = h.state().auto.elements.iter().find(|e| e.id == "exportFrame.close").unwrap().rect[1];
        let bottom = h.state().auto.elements.iter().find(|e| e.id == "exportFrame.export").unwrap().rect[1];
        assert!(top >= 0.0 && bottom - top < 350.0, "dialog must stay compact and on screen: {top}..{bottom}");
        click(&mut h, "exportFrame.cancel");
        step(&mut h);
        assert!(!has(&h, "exportFrame.name"));
        assert_eq!(*saves.lock().unwrap(), 0);
    }
}

#[test]
fn program_export_ignores_source_focus_and_imports_the_saved_still() {
    let mut h = harness();
    h.state_mut().ui.focused = PanelKind::Source;
    let folder = std::env::temp_dir().join(format!("fc-program-frame-ui-{}", std::process::id()));
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join("Program clip.bmp");
    let out = folder.clone();
    h.state_mut().hooks.pick_folder = Some(Box::new(move || Some(out.to_string_lossy().into_owned())));
    let ctx = h.ctx.clone();
    menus::invoke(h.state_mut(), &ctx, "file.exportFrame", json!({"monitor":"program"})).unwrap();
    step(&mut h);
    let before = h.state().session.project.items.len();
    click(&mut h, "exportFrame.format");
    click(&mut h, "exportFrame.format.bmp");
    click(&mut h, "exportFrame.browse");
    click(&mut h, "exportFrame.import");
    click(&mut h, "exportFrame.export");
    assert!(path.exists(), "{}", h.state().ui.status);
    assert_eq!(h.state().session.project.items.len(), before + 1);
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(i32::from_le_bytes(bytes[18..22].try_into().unwrap()), 128);
    assert_eq!(i32::from_le_bytes(bytes[22..26].try_into().unwrap()).abs(), 72);
    assert!(!has(&h, "exportFrame.name"));
    drop(h);
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(folder).unwrap();
}

#[test]
fn focused_source_command_captures_the_frame_and_save_cancel_keeps_the_dialog() {
    let mut h = harness();
    h.state_mut().ui.focused = PanelKind::Source;
    h.state_mut().session.state.source_playhead = FrameRate::FPS_24.tick_of(12);
    let expected_path = std::env::temp_dir().join(format!("fc-source-frame-ui-{}.expected.png", std::process::id()));
    h.state_mut().session.execute("file.exportFrame", json!({"target":"source","path":expected_path.to_string_lossy()})).unwrap();
    let expected = std::fs::read(&expected_path).unwrap();
    std::fs::remove_file(expected_path).unwrap();
    let ctx = h.ctx.clone();
    menus::invoke(h.state_mut(), &ctx, "file.exportFrame", json!({})).unwrap();
    step(&mut h);
    h.state_mut().session.state.source_playhead = Tick::ZERO;
    h.state_mut().hooks.pick_folder = Some(Box::new(|| None));
    click(&mut h, "exportFrame.browse");
    assert!(has(&h, "exportFrame.name"));
    let folder = std::env::temp_dir().join(format!("fc-source-frame-ui-{}", std::process::id()));
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join("Source clip.png");
    let out = folder.clone();
    h.state_mut().hooks.pick_folder = Some(Box::new(move || Some(out.to_string_lossy().into_owned())));
    click(&mut h, "exportFrame.browse");
    click(&mut h, "exportFrame.export");
    assert!(path.exists(), "{}", h.state().ui.status);
    assert_eq!(h.state().session.state.source_playhead, Tick::ZERO);
    assert_eq!(std::fs::read(&path).unwrap(), expected, "the dialog must export its captured frame");
    drop(h);
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(folder).unwrap();
}

#[test]
fn depth_dropdown_is_format_aware_and_existing_files_require_confirmation() {
    let mut h = harness();
    let ctx = h.ctx.clone();
    menus::invoke(h.state_mut(), &ctx, "file.exportFrame", json!({"monitor":"source"})).unwrap();
    step(&mut h);
    click(&mut h, "exportFrame.depth");
    click(&mut h, "exportFrame.depth.16");
    let folder = std::env::temp_dir().join(format!("fc-frame-depth-ui-{}", std::process::id()));
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join("Source clip.png");
    std::fs::write(&path, b"keep me").unwrap();
    let out = folder.clone();
    h.state_mut().hooks.pick_folder = Some(Box::new(move || Some(out.to_string_lossy().into_owned())));
    click(&mut h, "exportFrame.browse");
    click(&mut h, "exportFrame.export");
    assert!(has(&h, "exportFrame.replace"));
    assert_eq!(std::fs::read(&path).unwrap(), b"keep me");
    click(&mut h, "exportFrame.replace");
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(bytes[24], 16);
    menus::invoke(h.state_mut(), &ctx, "file.exportFrame", json!({"monitor":"source"})).unwrap();
    step(&mut h);
    click(&mut h, "exportFrame.format");
    click(&mut h, "exportFrame.format.jpg");
    click(&mut h, "exportFrame.depth");
    click(&mut h, "exportFrame.depth.16");
    assert!(h.state().auto.elements.iter().any(|e| e.id == "exportFrame.depth" && e.label == "8 Bit"));
    drop(h);
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(folder).unwrap();
}

#[test]
fn white_header_moves_both_dialogs_and_controls_still_work() {
    for monitor in ["source", "program"] {
        let mut h = harness();
        click(&mut h, &format!("{monitor}.transport.exportFrame"));
        step(&mut h);
        let header = h.state().auto.elements.iter().find(|e| e.id == "exportFrame.header").unwrap().rect;
        let name = h.state().auto.elements.iter().find(|e| e.id == "exportFrame.name").unwrap().label.clone();
        assert_eq!(name, if monitor == "source" { "Source clip" } else { "Program clip" });
        let from = egui::pos2(header[0] + header[2] / 2.0, header[1] + header[3] / 2.0);
        h.input_mut().events.push(egui::Event::PointerMoved(from));
        step(&mut h);
        h.input_mut().events.push(egui::Event::PointerButton {
            pos: from,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        step(&mut h);
        for i in 1..=8 {
            let offset = egui::vec2(15.0 * i as f32, 7.5 * i as f32);
            h.input_mut().events.push(egui::Event::PointerMoved(from + offset));
            step(&mut h);
        }
        let to = from + egui::vec2(120.0, 60.0);
        h.input_mut().events.push(egui::Event::PointerButton {
            pos: to,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        step(&mut h);
        step(&mut h);
        let moved = h.state().auto.elements.iter().find(|e| e.id == "exportFrame.header").unwrap().rect;
        assert!((moved[0] - header[0] - 120.0).abs() < 2.0 && (moved[1] - header[1] - 60.0).abs() < 2.0, "{monitor}: {header:?} -> {moved:?}");
        assert_eq!(h.state().auto.elements.iter().find(|e| e.id == "exportFrame.name").unwrap().label, name);
        click(&mut h, "exportFrame.format");
        click(&mut h, "exportFrame.format.bmp");
        assert_eq!(h.state().auto.elements.iter().find(|e| e.id == "exportFrame.format").unwrap().label, "BMP");
        click(&mut h, "exportFrame.close");
        assert!(!has(&h, "exportFrame.name"));
    }
}

#[test]
fn exported_frame_timecode_matches_each_monitor_and_stays_captured() {
    for (rate, drop_frame) in [
        (FrameRate::FPS_24, false),
        (FrameRate::FPS_25, false),
        (FrameRate::FPS_23_976, false),
        (FrameRate::FPS_29_97, false),
        (FrameRate::FPS_29_97, true),
        (FrameRate::FPS_59_94, true),
    ] {
        let mut h = harness();
        let seq = h.state().session.state.active_sequence.unwrap();
        let settings = &mut Arc::make_mut(&mut h.state_mut().session.project).sequence_mut(seq).unwrap().settings;
        settings.frame_rate = rate;
        settings.drop_frame = drop_frame;
        h.state_mut().session.set_playhead(rate.tick_of(rate.timecode_base() * 65 + 4));
        h.state_mut().session.state.source_playhead = FrameRate::FPS_24.tick_of(67);
        step(&mut h);
        for monitor in ["source", "program"] {
            let expected = h.state().auto.elements.iter().find(|e| e.id == format!("{monitor}.timecode")).unwrap().label.clone();
            click(&mut h, &format!("{monitor}.transport.exportFrame"));
            let displayed = h.state().auto.elements.iter().find(|e| e.id == "exportFrame.timecode").unwrap().label.clone();
            assert_eq!(displayed, expected, "{monitor}, {rate:?}, drop-frame {drop_frame}");
            if monitor == "source" {
                assert_eq!(displayed, "00:00:02:19");
                h.state_mut().session.state.source_playhead = Tick::ZERO;
            } else {
                h.state_mut().session.set_playhead(Tick::ZERO);
            }
            step(&mut h);
            assert_eq!(
                h.state().auto.elements.iter().find(|e| e.id == "exportFrame.timecode").unwrap().label,
                displayed,
                "captured time must remain fixed after either playhead moves"
            );
            click(&mut h, "exportFrame.close");
        }
    }
}

#[test]
fn clicking_outside_dismisses_without_export_and_dropdowns_stay_open() {
    for monitor in ["source", "program"] {
        let mut h = harness();
        let count = h.state().session.project.items.len();
        click(&mut h, &format!("{monitor}.transport.exportFrame"));
        click(&mut h, "exportFrame.format");
        assert!(has(&h, "exportFrame.name"));
        click(&mut h, "exportFrame.format.tiff");
        click(&mut h, "exportFrame.depth");
        click(&mut h, "exportFrame.depth.16");
        assert!(has(&h, "exportFrame.name"));
        assert_eq!(h.state().auto.elements.iter().find(|e| e.id == "exportFrame.depth").unwrap().label, "16 Bit");
        click(&mut h, "header.mode.export");
        assert!(!has(&h, "exportFrame.name"));
        assert_eq!(h.state().ui.mode, filmcraft_ui_egui::state::Mode::Export);
        assert_eq!(h.state().session.project.items.len(), count);
    }
    let mut h = harness();
    click(&mut h, "source.transport.exportFrame");
    let pos = egui::pos2(150.0, 180.0);
    h.input_mut().events.push(egui::Event::PointerMoved(pos));
    step(&mut h);
    for pressed in [true, false] {
        h.input_mut().events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
        step(&mut h);
    }
    step(&mut h);
    assert!(!has(&h, "exportFrame.name"), "clicking the background monitor must cancel the draft");
}
