use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
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
    s.execute("file.newSequence", json!({"name":"Toasts","width":128,"height":72,"fps":24})).unwrap();
    let mut h = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).build_eframe(move |_cc| FilmcraftApp::new(s));
    for _ in 0..3 {
        step(&mut h);
    }
    h
}

fn element(h: &Harness<'static, FilmcraftApp>, id: &str) -> Option<[f32; 4]> {
    h.state().auto.elements.iter().find(|e| e.id == id).map(|e| e.rect)
}

fn click(h: &mut Harness<'static, FilmcraftApp>, id: &str) {
    let [x, y, w, height] = element(h, id).unwrap_or_else(|| panic!("missing {id}"));
    let pos = egui::pos2(x + w / 2.0, y + height / 2.0);
    h.input_mut().events.push(egui::Event::PointerMoved(pos));
    step(h);
    for pressed in [true, false] {
        h.input_mut().events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
        step(h);
    }
}

/// Export Frame puts up a toast in the lower right corner with the path; Show in Folder hands the file
/// to the file manager, and the close button takes the toast down.
#[test]
fn an_exported_frame_shows_a_toast_that_reveals_the_file_and_closes() {
    let mut h = harness();
    let revealed = Arc::new(Mutex::new(Vec::new()));
    let seen = revealed.clone();
    h.state_mut().hooks.open_path = Some(Box::new(move |path: &str, reveal: bool| {
        seen.lock().unwrap().push((path.to_string(), reveal));
        Ok(())
    }));
    let dir = std::env::temp_dir().join(format!("filmcraft-toasts-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("frame.png").to_string_lossy().replace('\\', "/");
    let out = h.state_mut().session.execute("file.exportFrame", json!({"path": path, "format": "png"})).unwrap();
    // the path as the engine wrote it (native separators)
    let path = out["path"].as_str().unwrap().to_string();
    step(&mut h);
    step(&mut h);
    let toast = h.state().auto.elements.iter().find(|e| e.id == "toast.0").cloned().expect("a toast for the exported frame");
    assert!(toast.label.contains("Exported frame to") && toast.label.contains("frame.png"), "{}", toast.label);
    // lower right, above the status bar
    let [x, y, w, height] = toast.rect;
    assert!(x + w > 1600.0 - 40.0 && y + height > 980.0 - 80.0 && y + height < 980.0, "{:?}", toast.rect);
    assert!(element(&h, "toast.0.reveal").is_some());
    click(&mut h, "toast.0.reveal");
    assert_eq!(*revealed.lock().unwrap(), [(path.clone(), true)]);
    assert!(element(&h, "toast.0").is_some(), "showing the file leaves the toast up");
    // FILMCRAFT_TOAST_SNAPSHOT=<file.png> saves what the window looks like with the toast up
    if let Ok(out) = std::env::var("FILMCRAFT_TOAST_SNAPSHOT") {
        h.render().expect("render").save(out).unwrap();
    }
    click(&mut h, "toast.0.close");
    step(&mut h);
    assert!(element(&h, "toast.0").is_none());
    assert!(h.state().toasts.items.is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

/// An engine error is a toast too (with no Show in Folder), and one that could not show its file says so.
#[test]
fn errors_are_toasts_and_a_failed_reveal_says_why() {
    let mut h = harness();
    h.state_mut().session.error_toast("test", "Auto-save failed: disk full");
    step(&mut h);
    step(&mut h);
    assert_eq!(h.state().toasts.items.len(), 1);
    assert!(h.state().toasts.items[0].error);
    assert!(element(&h, "toast.0").is_some() && element(&h, "toast.0.reveal").is_none());
    h.state_mut().hooks.open_path = Some(Box::new(|_: &str, _: bool| Err("no file manager".into())));
    h.state_mut().session.toast_file("Exported frame to /nowhere/f.png", "/nowhere/f.png");
    step(&mut h);
    step(&mut h);
    click(&mut h, "toast.1.reveal");
    let last = h.state().toasts.items.last().unwrap();
    assert!(last.error && last.message.contains("no file manager"), "{last:?}");
}
