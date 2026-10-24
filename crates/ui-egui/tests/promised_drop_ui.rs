//! Drops whose files the OS only *promises*. macOS hands a video dragged out of Photos.app over as
//! a file promise and offers a JPEG poster frame as the drag's only real path, so the host resolves
//! the promise after the drop and reports the files through [`HostHooks::promised_drops`]; the UI
//! imports them as the drop they belong to.

use std::path::{Path, PathBuf};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::{FilmcraftApp, PromiseLost, PromisedDrops, PromisedFiles};
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

/// The host reports `file` as having arrived for a drop made at `dropped_at`.
fn arrive(promised: &PromisedDrops, file: &Path, dropped_at: Option<egui::Pos2>) {
    promised.arrived.lock().unwrap().push(PromisedFiles { paths: vec![file.to_string_lossy().into_owned()], dropped_at });
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
    arrive(&promised, &file, None);
    step(&mut h);

    assert!(item_names(&h).iter().any(|n| n == "promised-drop.png"), "imported: {:?}", item_names(&h));
    assert!(promised.arrived.lock().unwrap().is_empty(), "the arrived files are taken, not imported again");
}

/// Each file is imported once: a promise reported in one frame is not re-imported in the next.
#[test]
fn an_arrived_file_is_imported_only_once() {
    let (mut h, promised) = harness();
    let file = fixture("promised-once.png");
    arrive(&promised, &file, None);
    for _ in 0..4 {
        step(&mut h);
    }
    let imported = item_names(&h).iter().filter(|n| *n == "promised-once.png").count();
    assert_eq!(imported, 1, "imported {imported} times: {:?}", item_names(&h));
}

/// A promise is fulfilled into a directory chosen before the drop, and a drag that promises its
/// files reports nothing as hovering, so the host is pointed at this project's (Project Settings ▸
/// Scratch Disks ▸ Captured) whether or not anything is over the window.
#[test]
fn the_host_is_pointed_at_the_projects_media_directory_without_a_hover() {
    let (h, promised) = harness();
    let expected = filmcraft_engine::voiceover::captured_media_dir(&h.state().session, "Dropped Media");

    assert!(!expected.is_empty());
    assert!(h.input().hovered_files.is_empty(), "no drag is over the window");
    assert_eq!(promised.destination.lock().unwrap().as_str(), expected);
}

fn control_rect(h: &Harness<'static, FilmcraftApp>, id: &str) -> egui::Rect {
    let [x, y, w, height] = h.state().auto.elements.iter().find(|e| e.id == id).or_else(|| h.state().auto.find(id)).unwrap().rect;
    egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, height))
}

/// Clips on the active sequence's video tracks.
fn timeline_clips(h: &Harness<'static, FilmcraftApp>) -> usize {
    h.state().session.active_sequence().map(|q| q.video_tracks.iter().map(|t| t.items.len()).sum()).unwrap_or(0)
}

/// The files of a promised drop are placed where that drop landed, not where the pointer has got
/// to by the time a long download finishes: here the pointer has left the Timeline entirely, and
/// the file still lands on the track it was dropped on.
#[test]
fn a_promised_file_lands_where_its_drop_did() {
    let (mut h, promised) = harness();
    let file = fixture("promised-position.png");
    let panel = control_rect(&h, "panel.Timeline");
    let dropped_at = egui::pos2(panel.min.x + h.state().ui.timeline.header_w + 60.0, panel.min.y + 80.0);
    assert_eq!(timeline_clips(&h), 0);

    // The user moved on while the file downloaded, so the pointer is over the Project panel.
    let elsewhere = control_rect(&h, "panel.Project").center();
    h.input_mut().events.push(egui::Event::PointerMoved(elsewhere));
    step(&mut h);
    arrive(&promised, &file, Some(dropped_at));
    step(&mut h);
    step(&mut h);

    assert_eq!(timeline_clips(&h), 1, "placed on the Timeline where the drop landed, not where the pointer went");
}

/// A promise the dragging source never keeps (an iCloud video with nothing to download it over)
/// imports nothing at all: the drag's own path is the poster frame rather than the movie, and
/// importing that is the bug this exists to fix. The user is told instead, by the error toast and
/// Events panel entry that every failure the user did not just ask for goes to.
#[test]
fn a_promise_the_source_never_keeps_imports_nothing_and_says_so() {
    let (mut h, promised) = harness();
    let before = item_names(&h).len();

    promised.lost.lock().unwrap().push(PromiseLost { files: 1, reason: "offline".into() });
    step(&mut h);
    // The toast is raised as an engine event, which the next frame shows.
    step(&mut h);

    assert_eq!(item_names(&h).len(), before, "no stand-in is imported");
    let logged = h.state().session.log.entries.iter().any(|e| e.message.contains("could not be fetched: offline"));
    assert!(logged, "the reason is in the Events panel");
    assert_eq!(h.state().ui.status, "A dragged file could not be fetched: offline", "and in the status bar, which the user can see today");
}

/// An app driven by the control channel alone is isolated from native input, drops included, and
/// a promised drop does not come through raw input: a drag on the machine running the automation
/// must not import into it.
#[test]
fn an_automation_owned_app_ignores_a_native_promised_drop() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let promised = PromisedDrops::default();
    let hooked = promised.clone();
    let mut h = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).build_eframe(move |_cc| {
        let (_tx, rx) = std::sync::mpsc::channel();
        let mut app = FilmcraftApp::new(s).with_control_input_only(rx);
        app.hooks.promised_drops = Some(hooked);
        app
    });
    for _ in 0..4 {
        step(&mut h);
    }
    let before = item_names(&h).len();
    let file = fixture("promised-automation.png");

    arrive(&promised, &file, None);
    promised.lost.lock().unwrap().push(PromiseLost { files: 1, reason: "offline".into() });
    step(&mut h);
    step(&mut h);

    assert_eq!(item_names(&h).len(), before, "nothing of a native drop is imported");
    assert!(h.state().ui.status.is_empty(), "and nothing of one is reported");
    assert!(promised.arrived.lock().unwrap().is_empty(), "the files are drained rather than left to pile up");
    assert!(promised.lost.lock().unwrap().is_empty());
}

/// Two drops can finish downloading in the time between one frame and the next, each with its own
/// landing point. Both are placed: holding only the newest would import the earlier drop to the
/// bin and quietly forget where it was dropped.
#[test]
fn two_drops_that_finish_between_frames_are_both_placed() {
    let (mut h, promised) = harness();
    let first = fixture("promised-first.png");
    let second = fixture("promised-second.png");
    let panel = control_rect(&h, "panel.Timeline");
    let x = panel.min.x + h.state().ui.timeline.header_w + 60.0;
    assert_eq!(timeline_clips(&h), 0);

    // The pointer is nowhere near either, as it would be after two long downloads.
    let elsewhere = control_rect(&h, "panel.Project").center();
    h.input_mut().events.push(egui::Event::PointerMoved(elsewhere));
    step(&mut h);

    // Both land before the next frame is drawn.
    arrive(&promised, &first, Some(egui::pos2(x, panel.min.y + 80.0)));
    arrive(&promised, &second, Some(egui::pos2(x + 240.0, panel.min.y + 80.0)));
    step(&mut h);
    step(&mut h);

    assert_eq!(timeline_clips(&h), 2, "both drops are placed, not just the last one to arrive");
    let names = item_names(&h);
    assert!(names.iter().any(|n| n.contains("promised-first")), "the earlier drop was imported: {names:?}");
    assert!(names.iter().any(|n| n.contains("promised-second")), "the later drop was imported: {names:?}");
}

/// The same two-drops-between-frames case on an *empty* Timeline, where the first drop has to
/// become the sequence before the second has anything to land on. Opening a sequence changes what
/// the rest of the frame is laying out, so the second is held over to the next frame rather than
/// left as a bin import with its landing point forgotten.
#[test]
fn on_an_empty_timeline_the_second_drop_is_placed_on_the_sequence_the_first_makes() {
    // An empty project: the demo one already has a sequence open, and this is about the Timeline
    // that has none, where the first drop has to make one.
    let s = Session::default();
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
    assert!(h.state().session.active_sequence().is_none(), "no sequence is open yet");

    let first = fixture("promised-empty-first.png");
    let second = fixture("promised-empty-second.png");
    let panel = control_rect(&h, "panel.Timeline");
    let x = panel.min.x + h.state().ui.timeline.header_w + 60.0;

    // Both land before the next frame is drawn, as two finished downloads do.
    arrive(&shared, &first, Some(egui::pos2(x, panel.min.y + 80.0)));
    arrive(&shared, &second, Some(egui::pos2(x + 240.0, panel.min.y + 80.0)));
    for _ in 0..4 {
        step(&mut h);
    }

    assert!(h.state().session.active_sequence().is_some(), "the first drop made the sequence");
    assert_eq!(timeline_clips(&h), 2, "and the second was placed on it, not left behind in the bin");
}
