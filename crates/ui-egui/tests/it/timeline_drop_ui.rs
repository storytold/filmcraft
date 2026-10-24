//! Files dropped from the OS onto the Timeline. With Preferences ▸ Timeline ▸ "Place files dropped
//! onto the Timeline directly on the Timeline" (`drop_import_to_timeline`) they are also placed at
//! the pointer; with it off they are only imported to the bin.

use std::path::{Path, PathBuf};

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

fn harness() -> Harness<'static, FilmcraftApp> {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("file.newSequence", json!({"name":"Drop test","width":1280,"height":720,"fps":24})).unwrap();
    let mut h = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).build_eframe(move |_cc| FilmcraftApp::new(s));
    for _ in 0..4 {
        step(&mut h);
    }
    h
}

fn pointer_to(h: &mut Harness<'static, FilmcraftApp>, pos: egui::Pos2) {
    h.input_mut().events.push(egui::Event::PointerMoved(pos));
    step(h);
}

fn control_rect(h: &Harness<'static, FilmcraftApp>, id: &str) -> egui::Rect {
    let [x, y, w, height] = h.state().auto.elements.iter().find(|e| e.id == id).or_else(|| h.state().auto.find(id)).unwrap().rect;
    egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, height))
}

/// A point 60 px into the Timeline's clip area on the named track row.
fn point_on_row(h: &Harness<'static, FilmcraftApp>, row_control: &str) -> egui::Pos2 {
    let row = control_rect(h, row_control);
    let panel = control_rect(h, "panel.Timeline");
    egui::pos2(panel.min.x + h.state().ui.timeline.header_w + 60.0, row.center().y)
}

/// A file the OS dropped on the window, as the native integration hands it to egui.
#[derive(Debug)]
struct OsFile(PathBuf);

impl egui::DroppedFile for OsFile {
    fn path(&self) -> &Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

/// Drops `path` with the pointer over `row_control`, as the OS does: the pointer is hovering first,
/// then one frame carries the dropped file. The file is cleared afterwards, as egui only reports it once.
fn drop_on_row(h: &mut Harness<'static, FilmcraftApp>, row_control: &str, path: &Path) {
    let at = point_on_row(h, row_control);
    pointer_to(h, at);
    h.input_mut().dropped_files.push(std::sync::Arc::new(OsFile(path.to_path_buf())));
    step(h);
    h.input_mut().dropped_files.clear();
    step(h);
    settle_imports(h);
}

/// Dropped files import in the background: step frames until the import has added them and the
/// Timeline has had a frame to place them.
fn settle_imports(h: &mut Harness<'static, FilmcraftApp>) {
    for _ in 0..600 {
        if h.state().session.background_imports.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
        step(h);
    }
    step(h);
    step(h);
}

/// (video clips, audio clips) on the active sequence.
fn clip_counts(h: &Harness<'static, FilmcraftApp>) -> (usize, usize) {
    let q = h.state().session.active_sequence().unwrap();
    (q.video_tracks.iter().map(|t| t.items.len()).sum(), q.audio_tracks.iter().map(|t| t.items.len()).sum())
}

fn fixture(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/fixtures/ui-egui");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

/// A small generated PNG still.
fn still(name: &str) -> PathBuf {
    let p = fixture(name);
    image::RgbaImage::from_pixel(64, 36, image::Rgba([40, 140, 220, 255])).save(&p).unwrap();
    p
}

/// A 0.5 s mono 48 kHz 16-bit WAV tone (audio only).
fn tone(name: &str) -> PathBuf {
    let p = fixture(name);
    let rate = 48_000u32;
    let n = rate / 2;
    let mut data = Vec::with_capacity(n as usize * 2);
    for i in 0..n {
        let s = (f32::sin(2.0 * std::f32::consts::PI * 440.0 * i as f32 / rate as f32) * 0.5 * i16::MAX as f32) as i16;
        data.extend_from_slice(&s.to_le_bytes());
    }
    let mut wav = Vec::with_capacity(44 + data.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&rate.to_le_bytes());
    wav.extend_from_slice(&(rate * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
    wav.extend_from_slice(&data);
    std::fs::write(&p, wav).unwrap();
    p
}

#[test]
fn still_dropped_on_the_video_row_is_placed_at_the_pointer() {
    let mut h = harness();
    let items_before = h.state().session.project.items.len();
    drop_on_row(&mut h, "timeline.track.V1.target", &still("drop_still_video_row.png"));
    assert_eq!(h.state().session.project.items.len(), items_before + 1, "the still is imported to the bin");
    assert_eq!(clip_counts(&h), (1, 0), "status: {}", h.state().ui.status);
}

/// A numbered still with Settings ▸ Media ▸ Import image sequences on is imported at once (as the
/// image sequence it starts), not in the background. The drop used to wait only for a background
/// import, so this file went to the bin and never reached the Timeline.
#[test]
fn a_file_the_drop_imports_at_once_is_placed_too() {
    let mut h = harness();
    h.state_mut().session.prefs.media.import_image_sequences = true;
    let dir = fixture("drop_sequence");
    std::fs::create_dir_all(&dir).unwrap();
    for n in 1..=3 {
        image::RgbaImage::from_pixel(64, 36, image::Rgba([40, 140, 220, 255])).save(dir.join(format!("frame.{n:04}.png"))).unwrap();
    }
    drop_on_row(&mut h, "timeline.track.V1.target", &dir.join("frame.0001.png"));
    assert!(h.state().session.background_imports.is_empty());
    assert_eq!(clip_counts(&h), (1, 0), "the sequence is on the timeline; status: {}", h.state().ui.status);
}

/// A second drop while the first one's files were still importing replaced the first: its files
/// went to the bin only. Each drop is now placed where it was dropped.
#[test]
fn a_drop_while_another_is_importing_places_both() {
    let mut h = harness();
    let stills: Vec<PathBuf> = (0..6).map(|i| still(&format!("drop_first_{i}.png"))).collect();
    let (v1, a1) = (point_on_row(&h, "timeline.track.V1.target"), point_on_row(&h, "timeline.track.A1.target"));
    pointer_to(&mut h, v1);
    for f in &stills {
        h.input_mut().dropped_files.push(std::sync::Arc::new(OsFile(f.clone())));
    }
    step(&mut h);
    h.input_mut().dropped_files.clear();
    // the next frame, before the first import has necessarily finished
    pointer_to(&mut h, a1);
    h.input_mut().dropped_files.push(std::sync::Arc::new(OsFile(tone("drop_second.wav"))));
    step(&mut h);
    h.input_mut().dropped_files.clear();
    settle_imports(&mut h);
    assert_eq!(clip_counts(&h), (6, 1), "both drops are placed; status: {}", h.state().ui.status);
}

/// A drop whose import finished while the window was in Import mode (no Timeline drawn) was
/// taken from the queue and cleared at the end of the frame: back in Edit mode the files were in
/// the bin only. The drop now waits for the Timeline.
#[test]
fn a_drop_that_finishes_in_another_mode_is_placed_back_in_edit() {
    let mut h = harness();
    let at = point_on_row(&h, "timeline.track.V1.target");
    pointer_to(&mut h, at);
    h.input_mut().dropped_files.push(std::sync::Arc::new(OsFile(still("drop_other_mode.png"))));
    step(&mut h);
    h.input_mut().dropped_files.clear();
    // the import finishes on a later frame: switch away first
    h.state_mut().ui.mode = filmcraft_ui_egui::state::Mode::Import;
    settle_imports(&mut h);
    for _ in 0..4 {
        step(&mut h);
    }
    assert_eq!(h.state().session.project.items.values().filter(|it| it.name == "drop_other_mode.png").count(), 1, "imported meanwhile");
    assert_eq!(clip_counts(&h), (0, 0), "not placed while the Timeline is not shown");
    h.state_mut().ui.mode = filmcraft_ui_egui::state::Mode::Edit;
    step(&mut h);
    step(&mut h);
    assert_eq!(clip_counts(&h), (1, 0), "placed at the drop point back in Edit mode; status: {}", h.state().ui.status);
}

/// A drop waiting for the Timeline holds item ids of its project. When another project was
/// opened meanwhile, the ids were placed in that project, where the same id can be another item.
#[test]
fn a_waiting_drop_is_forgotten_when_the_project_is_replaced() {
    let mut h = harness();
    let at = point_on_row(&h, "timeline.track.V1.target");
    pointer_to(&mut h, at);
    let file = still("drop_then_replaced.png");
    h.input_mut().dropped_files.push(std::sync::Arc::new(OsFile(file.clone())));
    step(&mut h);
    h.input_mut().dropped_files.clear();
    h.state_mut().ui.mode = filmcraft_ui_egui::state::Mode::Import;
    settle_imports(&mut h);
    let dropped = *h.state().session.project.items.iter().find(|(_, it)| it.name == "drop_then_replaced.png").map(|(id, _)| id).expect("imported");
    // the same project again, so the next import gets the id the waiting drop holds
    let s = &mut h.state_mut().session;
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("file.newSequence", json!({"name":"Drop test","width":1280,"height":720,"fps":24})).unwrap();
    let r = s.execute("file.import", json!({"paths": [file.to_string_lossy()]})).unwrap();
    assert_eq!(r["items"][0].as_u64(), Some(dropped.0), "the replacement project has an item with the dropped id");
    h.state_mut().ui.mode = filmcraft_ui_egui::state::Mode::Edit;
    for _ in 0..3 {
        step(&mut h);
    }
    assert_eq!(clip_counts(&h), (0, 0), "nothing from the old project's drop is placed; status: {}", h.state().ui.status);
}

#[test]
fn still_dropped_on_an_audio_row_goes_to_the_video_track_without_error() {
    // a video-only item over an audio row: the picture takes the matching video track, no error
    let mut h = harness();
    drop_on_row(&mut h, "timeline.track.A1.target", &still("drop_still_audio_row.png"));
    assert_eq!(clip_counts(&h), (1, 0), "status: {}", h.state().ui.status);
}

#[test]
fn tone_dropped_on_a_video_row_goes_to_the_audio_track_without_error() {
    // an audio-only item over a video row: the sound takes the matching audio track, no error
    let mut h = harness();
    drop_on_row(&mut h, "timeline.track.V1.target", &tone("drop_tone_video_row.wav"));
    assert_eq!(clip_counts(&h), (0, 1), "status: {}", h.state().ui.status);
}

/// Empty project: no sequence yet. Dropping on the empty Timeline must import *and* make a sequence.
fn empty_harness() -> Harness<'static, FilmcraftApp> {
    let mut h = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).build_eframe(move |_cc| FilmcraftApp::new(Session::default()));
    for _ in 0..4 {
        step(&mut h);
    }
    h
}

fn drop_on_empty_timeline(h: &mut Harness<'static, FilmcraftApp>, path: &Path) {
    let panel = control_rect(h, "panel.Timeline");
    pointer_to(h, panel.center());
    h.input_mut().dropped_files.push(std::sync::Arc::new(OsFile(path.to_path_buf())));
    step(h);
    h.input_mut().dropped_files.clear();
    step(h);
    settle_imports(h);
}

#[test]
fn still_dropped_on_empty_timeline_creates_a_sequence() {
    let mut h = empty_harness();
    assert!(h.state().session.active_sequence().is_none());
    drop_on_empty_timeline(&mut h, &still("drop_still_empty_timeline.png"));
    assert_eq!(h.state().session.project.items.len(), 2, "still plus the new sequence");
    assert!(h.state().session.active_sequence().is_some(), "status: {}", h.state().ui.status);
    assert_eq!(clip_counts(&h), (1, 0), "status: {}", h.state().ui.status);
}

/// A field report: 353 files dropped together were imported on the UI thread (the window froze
/// for 4.8 s) and placed with a command each. Now the files import in the background, the window
/// keeps drawing, and when they are in one command makes the sequence: one Undo takes it back.
#[test]
fn several_files_dropped_on_empty_timeline_import_meanwhile_and_make_one_sequence() {
    let mut h = empty_harness();
    let files = [still("drop_many_a.png"), still("drop_many_b.png"), tone("drop_many_c.wav")];
    let panel = control_rect(&h, "panel.Timeline");
    pointer_to(&mut h, panel.center());
    for f in &files {
        h.input_mut().dropped_files.push(std::sync::Arc::new(OsFile(f.clone())));
    }
    step(&mut h);
    h.input_mut().dropped_files.clear();
    assert!(h.state().session.history.undo.is_empty() || h.state().session.active_sequence().is_none(), "nothing placed during the drop frame");
    settle_imports(&mut h);
    let q = h.state().session.active_sequence().expect("a sequence from the dropped files");
    let clips: usize = q.video_tracks.iter().chain(&q.audio_tracks).map(|t| t.items.len()).sum();
    assert!(clips >= files.len(), "every file is on the timeline: {clips} clips, status {}", h.state().ui.status);
    let undo = h.state().session.history.undo.len();
    let last = h.state().session.history.undo.last().map(|u| u.0.clone());
    assert_eq!(last.as_deref(), Some("New Sequence"), "one step for the sequence and its clips");
    h.state_mut().session.undo();
    assert!(h.state().session.active_sequence().is_none_or(|q| q.video_tracks.iter().chain(&q.audio_tracks).all(|t| t.items.is_empty())));
    assert_eq!(h.state().session.history.undo.len(), undo - 1);
}

#[test]
fn with_drop_import_off_empty_timeline_does_not_create_a_sequence() {
    let mut h = empty_harness();
    h.state_mut().session.prefs.timeline.drop_import_to_timeline = false;
    drop_on_empty_timeline(&mut h, &still("drop_still_empty_pref_off.png"));
    // not dropped onto the timeline: the file opens in the background and the frames add it
    let started = std::time::Instant::now();
    while h.state().session.project.items.is_empty() && started.elapsed() < std::time::Duration::from_secs(10) {
        step(&mut h);
    }
    assert_eq!(h.state().session.project.items.len(), 1, "imported to the bin only");
    assert!(h.state().session.active_sequence().is_none());
}

#[test]
fn with_drop_import_off_the_file_is_only_imported() {
    let mut h = harness();
    h.state_mut().session.prefs.timeline.drop_import_to_timeline = false;
    let items_before = h.state().session.project.items.len();
    drop_on_row(&mut h, "timeline.track.V1.target", &still("drop_still_pref_off.png"));
    // not placed on the timeline: the file opens in the background and the frames add it
    let started = std::time::Instant::now();
    while h.state().session.project.items.len() == items_before && started.elapsed() < std::time::Duration::from_secs(10) {
        step(&mut h);
    }
    assert_eq!(h.state().session.project.items.len(), items_before + 1, "the still is imported to the bin");
    assert_eq!(clip_counts(&h), (0, 0), "status: {}", h.state().ui.status);
}

/// Several files dropped at once land end to end, and one undo takes all of them off the Timeline.
#[test]
fn several_files_dropped_together_are_placed_in_one_undo_step() {
    let mut h = harness();
    let at = point_on_row(&h, "timeline.track.V1.target");
    pointer_to(&mut h, at);
    for name in ["drop_many_a.png", "drop_many_b.png", "drop_many_c.png"] {
        h.input_mut().dropped_files.push(std::sync::Arc::new(OsFile(still(name))));
    }
    step(&mut h);
    h.input_mut().dropped_files.clear();
    step(&mut h);
    settle_imports(&mut h);
    assert_eq!(clip_counts(&h), (3, 0), "status: {}", h.state().ui.status);
    let spans: Vec<(i64, i64)> = {
        let q = h.state().session.active_sequence().unwrap();
        let mut v: Vec<_> = q.video_tracks.iter().flat_map(|t| t.items.iter()).map(|i| (i.start.0, i.end().0)).collect();
        v.sort();
        v
    };
    assert!(spans.windows(2).all(|w| w[0].1 == w[1].0), "end to end: {spans:?}");
    h.state_mut().session.execute("edit.undo", serde_json::json!({})).unwrap();
    assert_eq!(clip_counts(&h), (0, 0), "one undo removes every placed clip");
}

fn float_panel(h: &mut Harness<'static, FilmcraftApp>, panel: &str, rect: [f32; 4]) {
    let ctx = h.ctx.clone();
    filmcraft_ui_egui::menus::invoke(h.state_mut(), &ctx, "window.floatPanel", json!({"panel": panel, "rect": rect})).unwrap();
    for _ in 0..4 {
        step(h);
    }
}

fn drop_at(h: &mut Harness<'static, FilmcraftApp>, pos: egui::Pos2, path: &Path) {
    pointer_to(h, pos);
    h.input_mut().dropped_files.push(std::sync::Arc::new(OsFile(path.to_path_buf())));
    step(h);
    h.input_mut().dropped_files.clear();
    step(h);
    settle_imports(h);
}

fn hover_at(h: &mut Harness<'static, FilmcraftApp>, pos: egui::Pos2) -> bool {
    pointer_to(h, pos);
    h.input_mut().hovered_files.push(Default::default());
    step(h);
    let preview = h.state().auto.elements.iter().any(|element| element.id == "timeline.fileDropPreview");
    h.input_mut().hovered_files.clear();
    step(h);
    preview
}

#[test]
fn floating_panel_blocks_os_drop_and_hover_but_uncovered_timeline_still_accepts() {
    for scale in [1.0, 2.0] {
        let mut h = harness();
        h.ctx.set_pixels_per_point(scale);
        for _ in 0..4 {
            step(&mut h);
        }
        let at = point_on_row(&h, "timeline.track.V1.target");
        float_panel(&mut h, "Info", [at.x - 20.0, at.y - 60.0, 320.0, 200.0]);
        let project = h.state().session.project.clone();
        let covered_preview = hover_at(&mut h, at);
        assert!(std::sync::Arc::ptr_eq(&project, &h.state().session.project), "hover does not edit the project");
        let items_before = h.state().session.project.items.len();
        drop_at(&mut h, at, &still(&format!("drop_covered_{scale}.png")));
        assert_eq!(h.state().session.project.items.len(), items_before + 1);
        assert_eq!(clip_counts(&h), (0, 0), "a covered Timeline must not be edited");
        assert!(!covered_preview, "a covered Timeline must not preview a placement");
        float_panel(&mut h, "Info", [5.0, 5.0, 320.0, 200.0]);
        let at = point_on_row(&h, "timeline.track.V1.target");
        assert!(hover_at(&mut h, at));
        drop_at(&mut h, at, &still(&format!("drop_uncovered_{scale}.png")));
        assert_eq!(clip_counts(&h), (1, 0));
    }
}

#[test]
fn floating_timeline_accepts_once_and_another_float_can_occlude_it() {
    let mut h = harness();
    float_panel(&mut h, "Timeline", [300.0, 180.0, 950.0, 640.0]);
    let at = point_on_row(&h, "timeline.track.V1.target");
    assert!(hover_at(&mut h, at));
    drop_at(&mut h, at, &still("drop_floating_timeline.png"));
    assert_eq!(clip_counts(&h), (1, 0));
    float_panel(&mut h, "Info", [at.x - 20.0, at.y - 60.0, 320.0, 200.0]);
    assert!(!hover_at(&mut h, at));
    drop_at(&mut h, at, &still("drop_occluded_floating_timeline.png"));
    assert_eq!(clip_counts(&h), (1, 0));
}

#[test]
fn offscreen_float_uses_rendered_clamp_and_maximized_timeline_ignores_hidden_floats() {
    let mut h = harness();
    let at = point_on_row(&h, "timeline.track.V1.target");
    // The very wide offscreen float is clamped back to x=0, over this row.
    float_panel(&mut h, "Info", [-5000.0, at.y - 60.0, 1500.0, 200.0]);
    let visible = control_rect(&h, "panel.Info");
    assert!(visible.contains(at), "rendered={visible:?}, drop={at:?}, floats={:?}", h.state().ui.floating);
    assert!(!hover_at(&mut h, at));
    drop_at(&mut h, at, &still("drop_clamped_occlusion.png"));
    assert_eq!(clip_counts(&h), (0, 0));
    h.state_mut().ui.keys.maximized = Some(filmcraft_ui_egui::dock::PanelKind::Timeline);
    for _ in 0..4 {
        step(&mut h);
    }
    let at = point_on_row(&h, "timeline.track.V1.target");
    assert!(hover_at(&mut h, at));
    drop_at(&mut h, at, &still("drop_maximized_timeline.png"));
    assert_eq!(clip_counts(&h), (1, 0));
}

#[test]
fn os_cursor_hook_selects_destination_when_egui_pointer_has_no_motion() {
    let mut h = harness();
    let at = point_on_row(&h, "timeline.track.V1.target");
    float_panel(&mut h, "Info", [at.x - 20.0, at.y - 60.0, 320.0, 200.0]);
    // Physical pixels at 1x, with the synthetic viewport origin fixed at zero.
    h.state_mut().hooks.cursor_screen_position = Some(Box::new(move || Some((at.x.round() as i32, at.y.round() as i32))));
    if let Some(viewport) = h.input_mut().viewports.get_mut(&egui::ViewportId::ROOT) {
        viewport.inner_rect = Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 980.0)));
    }
    h.input_mut().hovered_files.push(Default::default());
    step(&mut h);
    assert!(!h.state().auto.elements.iter().any(|element| element.id == "timeline.fileDropPreview"));
    h.input_mut().hovered_files.clear();
    h.input_mut().dropped_files.push(std::sync::Arc::new(OsFile(still("drop_os_hook_covered.png"))));
    step(&mut h);
    h.input_mut().dropped_files.clear();
    step(&mut h);
    settle_imports(&mut h);
    assert_eq!(clip_counts(&h), (0, 0));
    h.state_mut().ui.floating.clear();
    h.input_mut().dropped_files.push(std::sync::Arc::new(OsFile(still("drop_os_hook_visible.png"))));
    step(&mut h);
    h.input_mut().dropped_files.clear();
    step(&mut h);
    settle_imports(&mut h);
    assert_eq!(clip_counts(&h), (1, 0));
}
