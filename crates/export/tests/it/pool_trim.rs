//! A standalone export gives back the float images it left on the frame pool
//! (`filmcraft_frame::pool`) when it ends, however it ends; one part of a batch keeps them. One test in this file on purpose: the pool is shared by every test of a binary,
//! and its numbers mean something only while nothing else renders.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use filmcraft_export::{ExportSettings, Exporter, Format, Progress, Step, export};
use filmcraft_frame::pool;
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{Generator, MediaSource};
use filmcraft_project::{ItemId, ItemKind, Label, MediaClip, MediaRef, Project, SequenceSettings, TrackKind};
use filmcraft_render::SourceMap;
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick, TimeRange};

/// A red matte filling a 320×180 sequence for a second.
fn project() -> (Arc<Project>, ItemId, SourceMap) {
    let mut p = Project::new("x");
    let rate = FrameRate::FPS_24;
    let g = GeneratorSource::new(Generator::ColorMatte { color: [1.0, 0.0, 0.0, 1.0] }, 320, 180, rate, Tick(2 * TICKS_PER_SECOND));
    let info = g.info().clone();
    let clip = MediaClip {
        media: MediaRef::Generator(g.generator.clone()),
        info: info.clone(),
        interpret: Default::default(),
        mark_in: None,
        mark_out: None,
        markers: vec![],
        offline: false,
        proxy: None,
        identity: None,
    };
    let red = p.add_item(&info.name, Label::Iris, ItemKind::Media(clip), None);
    let seq = p.new_sequence("s", SequenceSettings { width: 320, height: 180, frame_rate: rate, ..Default::default() }, 1, 1, None);
    let item = p.make_track_item(red, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.tick_of(24)), rate).unwrap();
    p.sequence_mut(seq).unwrap().video_tracks[0].items.push(item);
    let mut sources = SourceMap::default();
    sources.0.insert(red, Arc::new(g));
    (Arc::new(p), seq, sources)
}

fn idle_mb() -> f64 {
    pool::stats().f32_idle_bytes as f64 / 1e6
}

#[test]
fn the_float_shelf_is_empty_when_a_standalone_export_ends_however_it_ended() {
    let (p, seq, sources) = project();
    let dir = std::env::temp_dir().join(format!("fc-pool-trim-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let settings =
        |format: Format, name: &str| ExportSettings { format, path: dir.join(name).to_string_lossy().to_string(), include_audio: false, ..Default::default() };
    // an image the pool could keep, standing in for what the frames of an export leave (the frames
    // themselves do too: the checks below fail without the trim)
    let leave_something = || {
        pool::recycle_f32(vec![0.0; 100_000]);
        assert!(pool::stats().f32_idle_bytes > 0);
    };
    let check = |what: &str| assert_eq!(pool::stats().f32_idle_bytes, 0, "after {what}: {:.1} MB idle", idle_mb());

    // images, which run in `export` itself
    leave_something();
    export(&p, seq, &settings(Format::PngSequence, "seq.png"), &sources, &Progress::default()).unwrap();
    check("an image sequence");

    // an encoded video, run to the end
    leave_something();
    export(&p, seq, &settings(Format::ProRes, "a.mov"), &sources, &Progress::default()).unwrap();
    check("a finished video");

    // cancelled before the first frame
    leave_something();
    let cancelled = Progress::default();
    cancelled.cancel.store(true, Ordering::Relaxed);
    assert!(export(&p, seq, &settings(Format::ProRes, "b.mov"), &sources, &cancelled).is_err());
    check("a cancelled video");

    // an encoder that is not there: the pipeline was built, the export never started
    leave_something();
    assert!(export(&p, seq, &settings(Format::Hevc, "c.mp4"), &sources, &Progress::default()).is_err());
    check("an export that could not start");

    // stepped by the host (the web build), dropped half way: 6 frames of 24
    let progress = Progress::default();
    let mut ex = Exporter::new(p.clone(), seq, &settings(Format::ProRes, "d.mov"), &progress).unwrap();
    ex.set_batch(2);
    for _ in 0..3 {
        assert!(matches!(ex.step(&sources, &progress), Ok(Step::Progress)));
    }
    leave_something();
    drop(ex);
    check("an export dropped half way");

    // one part of a batch (a render-preview segment, a proxy): the next part reuses the images
    leave_something();
    let part = ExportSettings { part_of_batch: true, ..settings(Format::ProRes, "e.mov") };
    export(&p, seq, &part, &sources, &Progress::default()).unwrap();
    assert!(pool::stats().f32_idle_bytes > 0, "a part of a batch keeps the float images");
    pool::trim_f32();

    let _ = std::fs::remove_dir_all(&dir);
}
