//! Test media made with our own encoders (no ffmpeg needed): short movies of the procedural demo
//! scenes, written to temporary folders.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{DemoScene, Generator, MediaSource};
use filmcraft_project::{ItemId, ItemKind, Label, MediaClip, MediaRef, Project, SequenceSettings, TrackKind};
use filmcraft_time::{FrameRate, Tick, TimeRange};
use serde_json::json;

use crate::Session;

pub fn tmp_dir(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let d = std::env::temp_dir().join(format!("filmcraft-{name}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Write a `frames`-frame `w`×`h` 24 fps movie of a demo scene (with its audio) to `path`.
/// `.mov` = ProRes 422 HQ + PCM, `.mp4` = H.264 + AAC.
pub fn make_movie(path: &Path, scene: DemoScene, w: u32, h: u32, frames: i64) {
    let rate = FrameRate::FPS_24;
    let dur = rate.tick_of(frames);
    let src = GeneratorSource::new(Generator::Demo(scene), w, h, rate, dur);
    let info = src.info().clone();
    let mut p = Project::new("fixture");
    let clip = MediaClip {
        media: MediaRef::Generator(Generator::Demo(scene)),
        info,
        interpret: Default::default(),
        mark_in: None,
        mark_out: None,
        markers: vec![],
        offline: false,
        proxy: None,
        identity: None,
    };
    let item = p.add_item("scene", Label::Iris, ItemKind::Media(clip), None);
    let seq = p.new_sequence("s", SequenceSettings { width: w, height: h, frame_rate: rate, ..Default::default() }, 1, 1, None);
    let r = TimeRange::new(Tick::ZERO, dur);
    let mut v = p.make_track_item(item, TrackKind::Video, Tick::ZERO, r, rate).unwrap();
    for e in &mut v.effects {
        filmcraft_project::resolve_auto_points(e, (w, h), (w, h));
    }
    let a = p.make_track_item(item, TrackKind::Audio, Tick::ZERO, r, rate).unwrap();
    let q = p.sequence_mut(seq).unwrap();
    q.video_tracks[0].items.push(v);
    q.audio_tracks[0].items.push(a);
    let fmt = if path.extension().is_some_and(|e| e == "mp4") { filmcraft_export::Format::H264 } else { filmcraft_export::Format::ProRes };
    let settings =
        filmcraft_export::ExportSettings { format: fmt, path: path.to_string_lossy().into_owned(), bitrate_kbps: 4000, overwrite: true, ..Default::default() };
    let shared: filmcraft_media::SharedSource = Arc::new(src);
    let provider = move |id: ItemId| (id == item).then(|| shared.clone());
    filmcraft_export::export(&Arc::new(p), seq, &settings, &provider, &Default::default()).unwrap();
}

/// A session with `files` imported and a sequence (sized like the first file) holding each file
/// in turn on V1/A1. Returns (session, item ids, sequence id).
pub fn session_with(files: &[&Path]) -> (Session, Vec<ItemId>, ItemId) {
    let mut s = Session::default();
    let paths: Vec<String> = files.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    let r = s.execute("file.import", json!({"paths": paths})).unwrap();
    let items: Vec<ItemId> = r["items"].as_array().unwrap().iter().map(|v| ItemId(v.as_u64().unwrap())).collect();
    assert_eq!(items.len(), files.len(), "{r}");
    let info = s.project.item(items[0]).unwrap().as_media().unwrap().info.clone();
    let v = info.video.clone().unwrap();
    let rate = v.frame_rate;
    let mut p = (*s.project).clone();
    let seq = p.new_sequence("Seq", SequenceSettings { width: v.width, height: v.height, frame_rate: rate, ..Default::default() }, 1, 1, None);
    let mut at = Tick::ZERO;
    for &it in &items {
        let d = p.item(it).unwrap().duration();
        let range = TimeRange::new(Tick::ZERO, d);
        let mut vi = p.make_track_item(it, TrackKind::Video, at, range, rate).unwrap();
        for e in &mut vi.effects {
            filmcraft_project::resolve_auto_points(e, (v.width, v.height), (v.width, v.height));
        }
        let ai = p.make_track_item(it, TrackKind::Audio, at, range, rate).unwrap();
        let q = p.sequence_mut(seq).unwrap();
        q.video_tracks[0].items.push(vi);
        q.audio_tracks[0].items.push(ai);
        at += d;
    }
    s.project = Arc::new(p);
    s.state.active_sequence = Some(seq);
    s.state.open_sequences = vec![seq];
    (s, items, seq)
}

/// Render the active sequence at `frame` (scale 1) as straight RGBA8 over black.
pub fn frame_rgba(s: &mut Session, frame: i64, scale: f32) -> (usize, usize, Vec<u8>) {
    let t = s.sequence_rate().tick_of(frame);
    s.set_playhead(t);
    let img = s.render_program(scale).unwrap();
    (img.w, img.h, img.over_black_rgba8())
}

pub fn psnr(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    let mut se = 0.0;
    let mut n = 0.0;
    for (x, y) in a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0) {
        for c in 0..3 {
            let d = x[c] as f64 - y[c] as f64;
            se += d * d;
            n += 1.0;
        }
    }
    if se == 0.0 { f64::INFINITY } else { 10.0 * (255.0f64 * 255.0 / (se / n)).log10() }
}
