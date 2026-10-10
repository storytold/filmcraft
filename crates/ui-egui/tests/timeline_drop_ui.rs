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

#[test]
fn with_drop_import_off_the_file_is_only_imported() {
    let mut h = harness();
    h.state_mut().session.prefs.timeline.drop_import_to_timeline = false;
    let items_before = h.state().session.project.items.len();
    drop_on_row(&mut h, "timeline.track.V1.target", &still("drop_still_pref_off.png"));
    assert_eq!(h.state().session.project.items.len(), items_before + 1, "the still is imported to the bin");
    assert_eq!(clip_counts(&h), (0, 0), "status: {}", h.state().ui.status);
}
