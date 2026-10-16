//! Drops whose files the OS only *promises*. macOS hands a video dragged out of Photos.app over as
//! a file promise and offers a JPEG poster frame as the drag's only real path, so the host resolves
//! the promise after the drop and reports the files through [`HostHooks::promised_drops`]; the UI
//! imports them as the drop they belong to.

use std::path::{Path, PathBuf};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::{FilmcraftApp, PromisedDrops};
use serde_json::json;

fn step(h: &mut Harness<'static, FilmcraftApp>) {
    let ctx = h.ctx.clone();
    let mut raw = std::mem::take(h.input_mut());
    eframe::App::raw_input_hook(h.state_mut(), &ctx, &mut raw);
    *h.input_mut() = raw;
    ctx.request_repaint();
    h.step();
}

/// A project with a sequence, and a host that resolves promised files.
fn harness() -> (Harness<'static, FilmcraftApp>, PromisedDrops) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("file.newSequence", json!({"name":"Promised","width":1280,"height":720,"fps":24})).unwrap();
    let shared = PromisedDrops::default();
    let hooked = shared.clone();
    let mut h = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).build_eframe(move |_cc| {
        let mut app = FilmcraftApp::new(s);
        app.hooks.promised_drops = Some(hooked);
        app
    });
    for _ in 0..4 {
        step(&mut h);
    }
    (h, shared)
}

fn fixture(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/fixtures/ui-egui");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    image::RgbaImage::from_pixel(64, 36, image::Rgba([40, 140, 220, 255])).save(&p).unwrap();
    p
}

fn item_names(h: &Harness<'static, FilmcraftApp>) -> Vec<String> {
    h.state().session.project.items.values().map(|i| i.name.clone()).collect()
}

/// The files of a drag that promised them arrive after the drop itself, between frames: they are
/// imported on the next frame, not dropped on the floor.
#[test]
fn promised_files_are_imported_once_the_host_has_them() {
    let (mut h, promised) = harness();
    let file = fixture("promised-drop.png");
    assert!(!item_names(&h).iter().any(|n| n == "promised-drop.png"));

    // The drop itself carries no path (the host took it over), and the file lands a moment later.
    promised.arrived.lock().unwrap().push(file.to_string_lossy().into_owned());
    step(&mut h);

    assert!(item_names(&h).iter().any(|n| n == "promised-drop.png"), "imported: {:?}", item_names(&h));
    assert!(promised.arrived.lock().unwrap().is_empty(), "the arrived files are taken, not imported again");
}

/// Each file is imported once: a promise reported in one frame is not re-imported in the next.
#[test]
fn an_arrived_file_is_imported_only_once() {
    let (mut h, promised) = harness();
    let file = fixture("promised-once.png");
    promised.arrived.lock().unwrap().push(file.to_string_lossy().into_owned());
    for _ in 0..4 {
        step(&mut h);
    }
    let imported = item_names(&h).iter().filter(|n| *n == "promised-once.png").count();
    assert_eq!(imported, 1, "imported {imported} times: {:?}", item_names(&h));
}

/// A promise is fulfilled into a directory chosen before the drop, so while files hover the host
/// is pointed at this project's (Project Settings ▸ Scratch Disks ▸ Captured).
#[test]
fn a_hovering_drag_points_the_host_at_the_projects_media_directory() {
    let (mut h, promised) = harness();
    assert_eq!(promised.destination.lock().unwrap().as_str(), "", "nothing is hovering yet");

    h.input_mut().hovered_files.push(Default::default());
    step(&mut h);
    let expected = filmcraft_engine::voiceover::captured_media_dir(&h.state().session, "Dropped Media");
    h.input_mut().hovered_files.clear();

    assert!(!expected.is_empty());
    assert_eq!(promised.destination.lock().unwrap().as_str(), expected);
}
