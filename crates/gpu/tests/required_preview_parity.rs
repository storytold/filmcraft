//! Parity of the strict GPU readback (`ExportRenderer::render_required_pixels`, the Enter GPU
//! route) with the CPU reference `render_sequence`. Needs a GPU adapter: without one the test
//! prints SKIPPED and passes, like `export_pipeline.rs`. Measure on the AMD machine with:
//! `cargo test -p filmcraft-gpu --test required_preview_parity -- --nocapture`

use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{Generator, MediaSource};
use filmcraft_project::{ItemKind, Label, MediaClip, MediaRef, Project, SequenceSettings, TrackKind};
use filmcraft_render::{RenderOptions, SourceMap, render_sequence};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick, TimeRange};
use std::sync::Arc;

/// Largest absolute difference allowed per premultiplied linear channel. Both sides decode the
/// same source frames, so only the GPU compositing arithmetic differs (half-float intermediates).
const TOLERANCE: f32 = 2e-3;

/// One File-backed clip (strict path: decoded source) whose frames come from an in-memory
/// generator, so no codec or disk file is needed to compare the two renderers.
fn project(width: u32, height: u32) -> (Arc<Project>, filmcraft_project::ItemId, SourceMap) {
    let mut p = Project::new("required-parity");
    let g = GeneratorSource::new(Generator::BarsAndTone, 320, 180, FrameRate::FPS_24, Tick(2 * TICKS_PER_SECOND));
    let info = g.info().clone();
    let clip = p.add_item(
        &info.name.clone(),
        Label::Iris,
        ItemKind::Media(MediaClip {
            media: MediaRef::File { path: "required-parity.mov".into() },
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
    let seq = p.new_sequence("s", SequenceSettings { width, height, frame_rate: FrameRate::FPS_24, ..Default::default() }, 1, 1, None);
    let v = p
        .make_track_item(clip, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, FrameRate::FPS_24.tick_of(2)), FrameRate::FPS_24)
        .expect("video item on an empty track");
    p.sequence_mut(seq).expect("sequence").video_tracks[0].items.push(v);
    let mut m = SourceMap::default();
    m.0.insert(clip, Arc::new(g));
    (Arc::new(p), seq, m)
}

#[test]
fn required_gpu_frame_matches_cpu_reference() {
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
                eprintln!("SKIPPED required GPU parity (required AMD adapter/device unavailable): {error}");
                return;
            }
            panic!("strict GPU renderer setup failed on eligible hardware: {error}");
        }
    };
    for (w, h) in [(320u32, 180u32), (256, 144)] {
        let (p, seq, sources) = project(w, h);
        for t in [Tick::ZERO, FrameRate::FPS_24.tick_of(1)] {
            let cpu = render_sequence(&p, seq, t, RenderOptions::default(), &sources);
            let gpu = renderer
                .render_required_pixels(&p, seq, t, RenderOptions::default(), &sources)
                .unwrap_or_else(|error| panic!("required GPU render refused {w}x{h} at {t:?}: {error}"));
            assert_eq!((gpu.w, gpu.h), (cpu.w, cpu.h), "size differs for {w}x{h}");
            assert_eq!(gpu.px.len(), cpu.px.len(), "pixel count differs for {w}x{h}");
            let over = cpu.px.iter().zip(&gpu.px).filter(|(a, b)| (**a - **b).abs() > TOLERANCE).count();
            let max = cpu.px.iter().zip(&gpu.px).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
            eprintln!("required GPU vs CPU {w}x{h} at {t:?}: max delta {max:e}, {over} of {} values over {TOLERANCE:e}", cpu.px.len());
            assert_eq!(over, 0, "GPU frame {w}x{h} differs from the CPU reference (max delta {max:e})");
        }
    }
}
