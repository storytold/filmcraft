//! Auto export fails closed. The separate strict renderer API returns resident GPU frames without
//! readback; explicit Off remains the CPU reference export path.

use filmcraft_export::{ExportSettings, Exporter, Format, GpuRendering, HardwareEncoding, Progress, export};
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{Generator, MediaSource};
use filmcraft_project::{ItemKind, Label, MediaClip, MediaRef, SequenceSettings, TrackKind};
use filmcraft_render::SourceMap;
use filmcraft_time::{TICKS_PER_SECOND, TimeRange};
use std::sync::Arc;

fn project(width: u32, height: u32) -> (Arc<filmcraft_project::Project>, filmcraft_project::ItemId, SourceMap) {
    let mut p = filmcraft_project::Project::new("x");
    let g = GeneratorSource::new(
        Generator::ColorMatte { color: [0.2, 0.5, 0.9, 1.0] },
        320,
        180,
        filmcraft_time::FrameRate::FPS_24,
        filmcraft_time::Tick(2 * TICKS_PER_SECOND),
    );
    let info = g.info().clone();
    let red = p.add_item(
        &info.name.clone(),
        Label::Iris,
        ItemKind::Media(MediaClip {
            media: MediaRef::Generator(g.generator.clone()),
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
    let seq = p.new_sequence("s", SequenceSettings { width, height, frame_rate: filmcraft_time::FrameRate::FPS_24, ..Default::default() }, 1, 1, None);
    let v = p
        .make_track_item(
            red,
            TrackKind::Video,
            filmcraft_time::Tick::ZERO,
            TimeRange::new(filmcraft_time::Tick::ZERO, filmcraft_time::FrameRate::FPS_24.tick_of(2)),
            filmcraft_time::FrameRate::FPS_24,
        )
        .unwrap();
    p.sequence_mut(seq).unwrap().video_tracks[0].items.push(v);
    let mut m = SourceMap::default();
    m.0.insert(red, Arc::new(g));
    (Arc::new(p), seq, m)
}

fn export_png(dir: &str, gpu: GpuRendering, (w, h): (u32, u32)) {
    let (p, seq, m) = project(w, h);
    let mut s = ExportSettings {
        format: Format::PngSequence,
        path: std::env::temp_dir().join(format!("fc-gpu-export-{dir}")).to_string_lossy().into_owned(),
        ..Default::default()
    };
    s.range = Some(TimeRange::new(filmcraft_time::Tick::ZERO, filmcraft_time::FrameRate::FPS_24.tick_of(2)));
    s.gpu_rendering = gpu;
    export(&p, seq, &s, &m, &Progress::default()).unwrap();
}

#[test]
fn gpu_auto_fails_closed_and_strict_renderer_stays_resident() {
    let (p, seq, sources) = project(320, 180);
    let auto_path = std::env::temp_dir().join("fc-auto-must-fail.png").to_string_lossy().into_owned();
    let auto = ExportSettings { format: Format::PngSequence, path: auto_path.clone(), gpu_rendering: GpuRendering::Auto, ..Default::default() };
    let progress = Progress::default();
    // PNG is not a stepped export: it goes through `export`, which refuses GPU Auto before any work.
    let error = export(&p, seq, &auto, &sources, &progress).expect_err("GPU Auto must fail closed");
    assert!(error.to_string().contains("GPU-resident renderer and VA-API encoder path"), "{error}");
    assert_eq!(progress.total.load(std::sync::atomic::Ordering::Relaxed), 0, "Auto must fail before frame work");
    assert!(!std::path::Path::new(&auto_path).exists(), "Auto must fail before output creation");
    let h264 = ExportSettings { format: Format::H264, hardware_encoding: HardwareEncoding::Auto, ..auto };
    let error = Exporter::new(p, seq, &h264, &progress).err().expect("GPU Auto must not use software encoder");
    assert!(error.to_string().contains("GPU-resident renderer and VA-API encoder path"), "{error}");

    let mut renderer = match filmcraft_gpu::ExportRenderer::new_required() {
        Ok(renderer) => renderer,
        Err(error) => {
            let unavailable = matches!(
                &error.reason,
                filmcraft_render::plan::EligibilityFailure::UnsupportedAdapter { .. }
                    | filmcraft_render::plan::EligibilityFailure::UnsupportedDeviceRequirement {
                        requirement: filmcraft_render::plan::DeviceRequirement::PhysicalGpu | filmcraft_render::plan::DeviceRequirement::DeviceAvailable,
                    }
            );
            if unavailable {
                eprintln!("SKIPPED strict GPU runtime (required AMD adapter/device unavailable): {error}");
                return;
            }
            panic!("strict GPU renderer setup failed on eligible hardware: {error}");
        }
    };
    let sizes = [(320u32, 180u32), (17, 13), (1, 1), (257, 129)];
    for size in sizes {
        // The strict renderer never materializes a generator on the CPU: it must refuse it, at any size.
        let (project, seq, sources) = project(size.0, size.1);
        let error = renderer
            .render_required(&project, seq, filmcraft_time::Tick::ZERO, filmcraft_render::RenderOptions::default(), &sources)
            .err()
            .unwrap_or_else(|| panic!("strict render of a generator source must fail closed for {size:?}"));
        assert!(
            matches!(&error.reason, filmcraft_render::plan::EligibilityFailure::CpuFallbackRequired { id: Some(id) } if id == "generator"),
            "strict render refuses CPU-materialized generators for {size:?}: {error}"
        );
        export_png(&format!("off-{}x{}", size.0, size.1), GpuRendering::Off, size);
    }
}
