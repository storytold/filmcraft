//! Exercise CPU-reference export entry points under nested Rayon and oversized stepped batches.
//! Each case runs in a bounded child so a regression fails instead of hanging the test runner.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use filmcraft_export::{ExportSettings, Exporter, Format, FrameRenderer, GpuRendering, Progress, Step, export, gpu_render_stats, register_frame_renderer};
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{Generator, MediaSource};
use filmcraft_project::{ItemId, ItemKind, Label, MediaClip, MediaRef, Project, SequenceSettings, TrackKind};
use filmcraft_render::SourceMap;
use filmcraft_time::{FrameRate, Tick, TimeRange};

const CHILD: &str = "FILMCRAFT_RENDERER_REENTRANCY_CASE";
const FRAMES: i64 = 6;
static CREATED: AtomicUsize = AtomicUsize::new(0);

fn factory() -> Option<Box<dyn FrameRenderer>> {
    CREATED.fetch_add(1, Ordering::Relaxed);
    None
}

fn project() -> (Arc<Project>, ItemId, SourceMap) {
    let mut project = Project::new("renderer reentrancy");
    let rate = FrameRate::FPS_24;
    let duration = rate.tick_of(FRAMES);
    let seq = project.new_sequence("test", SequenceSettings { width: 32, height: 32, frame_rate: rate, ..Default::default() }, 1, 1, None);
    let mut sources = SourceMap::default();
    for (generator, kind) in
        [(Generator::ColorMatte { color: [1.0, 0.0, 0.0, 1.0] }, TrackKind::Video), (Generator::Tone { hz: 440.0, db: -6.0 }, TrackKind::Audio)]
    {
        let source = GeneratorSource::new(generator, 32, 32, rate, duration);
        let info = source.info().clone();
        let id = project.add_item(
            &info.name.clone(),
            Label::Iris,
            ItemKind::Media(MediaClip {
                media: MediaRef::Generator(source.generator.clone()),
                info,
                interpret: Default::default(),
                mark_in: None,
                mark_out: None,
                markers: vec![],
                offline: false,
                proxy: None,
                identity: None,
            }),
            None,
        );
        let item = project.make_track_item(id, kind, Tick::ZERO, TimeRange::new(Tick::ZERO, duration), rate).unwrap();
        let sequence = project.sequence_mut(seq).unwrap();
        match kind {
            TrackKind::Video => sequence.video_tracks[0].items.push(item),
            TrackKind::Audio => sequence.audio_tracks[0].items.push(item),
        }
        sources.0.insert(id, Arc::new(source));
    }
    (Arc::new(project), seq, sources)
}

fn run_case(case: &str) {
    let (threads, format, overlap, audio) = match case {
        "png" => (1, Format::PngSequence, false, false),
        "gif" => (1, Format::Gif, false, false),
        "stepped" => (1, Format::ProRes, false, false),
        "overlap-audio" => (8, Format::ProRes, true, true),
        _ => panic!("unknown regression case: {case}"),
    };
    register_frame_renderer(factory);
    let (project, seq, sources) = project();
    let dir = std::env::temp_dir().join(format!("fc-renderer-reentrancy-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    let settings = ExportSettings {
        format,
        path: dir.join("output").to_string_lossy().into_owned(),
        gpu_rendering: GpuRendering::Off,
        include_audio: audio,
        ..Default::default()
    };
    let progress = Progress::default();
    rayon::ThreadPoolBuilder::new().num_threads(threads).build().unwrap().install(|| {
        if format == Format::ProRes {
            let mut exporter = Exporter::new(project.clone(), seq, &settings, &progress).unwrap();
            // Host overrides must not bypass the resource-capacity bound.
            exporter.set_batch(64);
            exporter.set_overlap(overlap);
            loop {
                if let Step::Done(report) = exporter.step(&sources, &progress).unwrap() {
                    assert_eq!(report.frames, FRAMES as u64);
                    break;
                }
            }
            if overlap {
                assert!(exporter.overlapped_steps() > 0);
            }
        } else {
            assert_eq!(export(&project, seq, &settings, &sources, &progress).unwrap().frames, FRAMES as u64);
        }
    });
    assert_eq!(CREATED.load(Ordering::Relaxed), 0, "Off must not invoke GPU renderer factories");
    assert_eq!(gpu_render_stats().fallbacks, 0, "Off is explicit CPU reference mode, not a fallback");
    assert_eq!(progress.done.load(Ordering::Relaxed), FRAMES as u64);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn explicit_cpu_exports_finish_every_path() {
    if let Ok(case) = std::env::var(CHILD) {
        let (done, finished) = std::sync::mpsc::channel();
        let diagnostic = case.clone();
        let watchdog = std::thread::spawn(move || {
            if finished.recv_timeout(Duration::from_secs(30)).is_err() {
                eprintln!("{diagnostic}: CPU export path stalled (30-second deadline)");
                // Exit only this private synthetic test child, without signaling any process.
                std::process::exit(124);
            }
        });
        run_case(&case);
        done.send(()).unwrap();
        watchdog.join().unwrap();
        return;
    }
    for case in ["png", "gif", "stepped", "overlap-audio"] {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "renderer_reentrancy::explicit_cpu_exports_finish_every_path", "--nocapture"])
            .env(CHILD, case)
            .spawn()
            .unwrap();
        let status = child.wait().unwrap();
        assert!(status.success(), "{case}: child failed with {status}");
    }
}
