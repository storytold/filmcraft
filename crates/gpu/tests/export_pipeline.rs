//! End-to-end GPU export parity: the registered `filmcraft-gpu` frame renderer drives the full
//! export pipeline (`filmcraft_export::export` → PNG sequence) and its frames must match the
//! `GpuRendering::Off` (CPU reference) frames within the GPU-vs-CPU parity tolerance — p99 of the
//! per-pixel max RGB difference over black ≤ 6 and mean < 1.5 (the golden-test criterion).

use filmcraft_export::{ExportSettings, Format, FrameRenderer, GpuRenderStats, GpuRendering, Progress, export, register_frame_renderer};
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{Generator, MediaSource};
use filmcraft_project::{ItemKind, Label, MediaClip, MediaRef, SequenceSettings, TrackKind};
use filmcraft_render::SourceMap;
use filmcraft_time::{TICKS_PER_SECOND, TimeRange};
use std::sync::Arc;

/// The same wrapper the app and CLI register: `filmcraft-gpu`'s renderer behind
/// `filmcraft_export`'s trait.
struct GpuFrameRenderer(filmcraft_gpu::ExportRenderer);

impl FrameRenderer for GpuFrameRenderer {
    fn render(
        &mut self,
        project: &filmcraft_project::Project,
        seq: filmcraft_project::ItemId,
        t: filmcraft_time::Tick,
        opts: filmcraft_render::RenderOptions,
        sources: &dyn filmcraft_render::SourceProvider,
    ) -> Option<filmcraft_render::Image> {
        self.0.render(project, seq, t, opts, sources)
    }

    fn caps(&self) -> filmcraft_render::plan::DeviceCaps {
        self.0.caps()
    }

    fn render_rgba8(
        &mut self,
        project: &filmcraft_project::Project,
        seq: filmcraft_project::ItemId,
        t: filmcraft_time::Tick,
        opts: filmcraft_render::RenderOptions,
        sources: &dyn filmcraft_render::SourceProvider,
    ) -> Option<(usize, usize, Vec<u8>)> {
        self.0.render_rgba8(project, seq, t, opts, sources)
    }
}

/// The colour matte on V1, and with `adjusted` an adjustment layer over it on V2 (Proc Amp and
/// Gaussian Blur, opaque, Normal, full frame: the kind the GPU runs as a `PlanStep::Adjust`).
fn project_with(width: u32, height: u32, adjusted: bool) -> (Arc<filmcraft_project::Project>, filmcraft_project::ItemId, SourceMap) {
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
        ItemKind::Media(Box::new(MediaClip {
            media: MediaRef::Generator(g.generator.clone()),
            info,
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
            identity: None,
        })),
        None,
    );
    let tracks = if adjusted { 2 } else { 1 };
    let seq = p.new_sequence("s", SequenceSettings { width, height, frame_rate: filmcraft_time::FrameRate::FPS_24, ..Default::default() }, tracks, 1, None);
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
    if adjusted {
        let rate = filmcraft_time::FrameRate::FPS_24;
        let a = p.add_item("Adjustment Layer", Label::Lavender, ItemKind::AdjustmentLayer { width, height, rate, duration: rate.tick_of(2) }, None);
        let mut ti =
            p.make_track_item(a, TrackKind::Video, filmcraft_time::Tick::ZERO, TimeRange::new(filmcraft_time::Tick::ZERO, rate.tick_of(2)), rate).unwrap();
        for (id, param, v) in [("proc_amp", "brightness", 25.0), ("gaussian_blur", "blurriness", 6.0)] {
            let mut e = filmcraft_project::find_effect(id).unwrap().instance();
            e.param_mut(param).unwrap().value = filmcraft_project::ParamValue::Float(v);
            ti.effects.insert(0, e);
        }
        p.sequence_mut(seq).unwrap().video_tracks[1].items.push(ti);
    }
    let mut m = SourceMap::default();
    m.0.insert(red, Arc::new(g));
    (Arc::new(p), seq, m)
}

/// Two colour mattes joined by a 4-frame Push transition (`FramePlan::Composite` with
/// `PlanStep::Transition`) across frames 2..6.
fn project_with_push_transition(width: u32, height: u32) -> (Arc<filmcraft_project::Project>, filmcraft_project::ItemId, SourceMap) {
    let mut p = filmcraft_project::Project::new("push");
    let rate = filmcraft_time::FrameRate::FPS_24;
    let mut m = SourceMap::default();
    let mut clips = Vec::new();
    for (i, color) in [[0.9, 0.2, 0.1, 1.0], [0.1, 0.4, 0.95, 1.0]].into_iter().enumerate() {
        let g = GeneratorSource::new(Generator::ColorMatte { color }, width, height, rate, filmcraft_time::Tick(2 * TICKS_PER_SECOND));
        let info = g.info().clone();
        let item = p.add_item(
            &format!("c{i}"),
            Label::Iris,
            ItemKind::Media(Box::new(MediaClip {
                media: MediaRef::Generator(g.generator.clone()),
                info,
                interpret: Default::default(),
                mark_in: None,
                mark_out: None,
                markers: vec![],
                offline: false,
                proxy: None,
                identity: None,
            })),
            None,
        );
        m.0.insert(item, Arc::new(g));
        clips.push(item);
    }
    let seq = p.new_sequence("s", SequenceSettings { width, height, frame_rate: rate, ..Default::default() }, 1, 0, None);
    let a =
        p.make_track_item(clips[0], TrackKind::Video, filmcraft_time::Tick::ZERO, TimeRange::new(filmcraft_time::Tick::ZERO, rate.tick_of(4)), rate).unwrap();
    let b = p.make_track_item(clips[1], TrackKind::Video, rate.tick_of(4), TimeRange::new(filmcraft_time::Tick::ZERO, rate.tick_of(4)), rate).unwrap();
    let (a_id, b_id) = (a.id, b.id);
    let q = p.sequence_mut(seq).unwrap();
    q.video_tracks[0].items.extend([a, b]);
    q.video_tracks[0].transitions.push(filmcraft_project::Transition {
        id: filmcraft_project::TransitionId(1),
        effect: filmcraft_project::find_effect("push").unwrap().instance(),
        start: rate.tick_of(2),
        duration: rate.tick_of(4),
        from: Some(a_id),
        to: Some(b_id),
        align: Default::default(),
        reverse: false,
    });
    (Arc::new(p), seq, m)
}

fn png(dir: &str, name: &str) -> Vec<u8> {
    // image_sequence_path flattens the path stem: <stem><index>.png in the temp dir itself.
    let index: u64 = name.trim_start_matches("img").trim_end_matches(".png").parse().unwrap_or(0);
    std::fs::read(std::env::temp_dir().join(format!("fc-gpu-export-{dir}{index:03}.png"))).unwrap()
}

fn export_png(dir: &str, gpu: GpuRendering, size: (u32, u32)) {
    export_project_png(dir, gpu, size, false);
}

fn export_project_png(dir: &str, gpu: GpuRendering, (w, h): (u32, u32), adjusted: bool) {
    let (p, seq, m) = project_with(w, h, adjusted);
    let mut s = ExportSettings {
        format: Format::PngSequence,
        path: std::env::temp_dir().join(format!("fc-gpu-export-{dir}")).to_string_lossy().into_owned(),
        ..Default::default()
    };
    s.range = Some(TimeRange::new(filmcraft_time::Tick::ZERO, filmcraft_time::FrameRate::FPS_24.tick_of(2)));
    s.gpu_rendering = gpu;
    export(&p, seq, &s, &m, &Progress::default()).unwrap();
}

fn assert_parity(cpu_png: &[u8], gpu_png: &[u8], what: &str) {
    let a = image::load_from_memory(cpu_png).unwrap().to_rgba8();
    let b = image::load_from_memory(gpu_png).unwrap().to_rgba8();
    assert_eq!(a.dimensions(), b.dimensions(), "{what}: size mismatch");
    let d: Vec<u32> = a
        .as_raw()
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.as_raw().as_chunks::<4>().0)
        .map(|(x, y)| (0..3).map(|k| (x[k] as u32).abs_diff(y[k] as u32)).max().unwrap_or(0))
        .collect();
    let mut d = d;
    d.sort_unstable();
    let p99 = d[d.len() * 99 / 100];
    let mean = d.iter().sum::<u32>() as f64 / d.len() as f64;
    eprintln!("{what}: GPU vs CPU p99 {p99}, mean {mean:.3}");
    assert!(p99 <= 6 && mean < 1.5, "{what}: GPU parity p99 {p99}, mean {mean:.3}");
}

fn assert_adjustment_layer_export() {
    let composited = filmcraft_gpu::export_renderer::timing::get().frames;
    let GpuRenderStats { fallbacks, .. } = filmcraft_export::gpu_render_stats();
    export_project_png("adj-cpu", GpuRendering::Off, (320, 180), true);
    export_project_png("adj-gpu", GpuRendering::Auto, (320, 180), true);
    for f in ["000", "001"] {
        assert_parity(&png("adj-cpu", &format!("img{f}.png")), &png("adj-gpu", &format!("img{f}.png")), &format!("adjustment layer frame {f}"));
    }
    assert_ne!(png("adj-cpu", "img000.png"), png("off-cpu", "img000.png"), "the adjustment layer changes the picture");
    let gpu = filmcraft_gpu::export_renderer::timing::get().frames - composited;
    assert_eq!(gpu, 2, "both frames with the adjustment layer composited on the GPU");
    assert_eq!(filmcraft_export::gpu_render_stats().fallbacks, fallbacks, "no frame with the adjustment layer fell back to the CPU");
}

fn assert_push_transition_export() {
    let (push_p, push_seq, push_m) = project_with_push_transition(320, 180);
    let export_push = |dir: &str, gpu: GpuRendering| {
        let mut s = ExportSettings {
            format: Format::PngSequence,
            path: std::env::temp_dir().join(format!("fc-gpu-export-{dir}")).to_string_lossy().into_owned(),
            ..Default::default()
        };
        s.range = Some(TimeRange::new(filmcraft_time::FrameRate::FPS_24.tick_of(2), filmcraft_time::FrameRate::FPS_24.tick_of(4)));
        s.gpu_rendering = gpu;
        export(&push_p, push_seq, &s, &push_m, &Progress::default()).unwrap();
    };
    let composited = filmcraft_gpu::export_renderer::timing::get().frames;
    let GpuRenderStats { fallbacks, .. } = filmcraft_export::gpu_render_stats();
    export_push("push-cpu", GpuRendering::Off);
    export_push("push-gpu", GpuRendering::Auto);
    for f in ["000", "001", "002", "003"] {
        assert_parity(&png("push-cpu", &format!("img{f}.png")), &png("push-gpu", &format!("img{f}.png")), &format!("push transition frame {f}"));
    }
    assert_ne!(png("push-cpu", "img001.png"), png("push-cpu", "img002.png"), "the Push transition moves across frames");
    let gpu = filmcraft_gpu::export_renderer::timing::get().frames - composited;
    assert_eq!(gpu, 4, "all four Push transition frames composited on the GPU");
    assert_eq!(filmcraft_export::gpu_render_stats().fallbacks, fallbacks, "no Push transition frame fell back to the CPU");
}

#[test]
fn gpu_export_matches_cpu_and_counts() {
    let Some(_) = filmcraft_gpu::ExportRenderer::new() else {
        eprintln!("SKIPPED (gpu export pipeline): no adapter");
        return;
    };
    // Clean registry first: Auto with no factory must be byte-equal to Off.
    export_png("off-cpu", GpuRendering::Off, (320, 180));
    export_png("off-auto", GpuRendering::Auto, (320, 180));
    assert_eq!(png("off-cpu", "img000.png"), png("off-auto", "img000.png"), "Auto with no registered renderer must be byte-equal to Off");

    register_frame_renderer(|| Some(Box::new(GpuFrameRenderer(filmcraft_gpu::ExportRenderer::new().expect("adapter present")))));
    let sizes = [(320u32, 180u32), (17, 13), (1, 1), (257, 129)];
    for (i, size) in sizes.into_iter().enumerate() {
        export_png("cpu", GpuRendering::Off, size);
        export_png("gpu", GpuRendering::Auto, size);
        for f in ["000", "001"] {
            assert_parity(&png("cpu", &format!("img{f}.png")), &png("gpu", &format!("img{f}.png")), &format!("{:?} frame {f}", size));
        }
        let _ = i;
    }
    let GpuRenderStats { frames, .. } = filmcraft_export::gpu_render_stats();
    assert!(frames >= 8, "gpu frames counted across the parity exports: {frames}");

    // An adjustment layer is composited by the GPU too (a `PlanStep::Adjust`), not handed to the
    // CPU renderer, and matches the CPU export.
    assert_adjustment_layer_export();

    // A Push transition (`FramePlan::Composite` with `PlanStep::Transition`) also exports on the
    // GPU `render_rgba8` path without falling back to the CPU, and matches the CPU export.
    assert_push_transition_export();
}
