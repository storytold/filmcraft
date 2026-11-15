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

    fn plan_preparer(&self) -> Option<filmcraft_export::PlanPreparer> {
        Some(self.0.plan_preparer())
    }

    fn render_planned(&mut self, plan: &filmcraft_render::plan::FramePlan, prep: Option<&dyn std::any::Any>) -> Option<filmcraft_render::Image> {
        self.0.render_planned(plan, prep)
    }

    fn render_planned_rgba8(&mut self, plan: &filmcraft_render::plan::FramePlan, prep: Option<&dyn std::any::Any>) -> Option<(u32, u32, Vec<u8>)> {
        self.0.render_planned_rgba8(plan, prep)
    }

    fn render_planned_gpu(
        &mut self,
        plan: &filmcraft_render::plan::FramePlan,
        prep: Option<&dyn std::any::Any>,
        target: &dyn filmcraft_export::GpuEncodeInput,
    ) -> filmcraft_export::GpuFrame {
        use filmcraft_export::GpuFrame;
        match self.0.render_planned_surface(plan, prep, target.as_any(), target.id(), target.width(), target.height()) {
            filmcraft_gpu::SurfaceRender::Rendered => GpuFrame::Rendered,
            filmcraft_gpu::SurfaceRender::NoImport => GpuFrame::NoImport,
            filmcraft_gpu::SurfaceRender::Unconfirmed => GpuFrame::Unconfirmed,
            filmcraft_gpu::SurfaceRender::Unavailable => GpuFrame::Unavailable,
        }
    }

    fn try_render(
        &mut self,
        project: &filmcraft_project::Project,
        seq: filmcraft_project::ItemId,
        t: filmcraft_time::Tick,
        opts: filmcraft_render::RenderOptions,
        sources: &dyn filmcraft_render::SourceProvider,
    ) -> filmcraft_export::Result<Option<filmcraft_render::Image>> {
        self.0.try_render(project, seq, t, opts, sources).map_err(filmcraft_export::ExportError::Encode)
    }

    fn try_render_planned(
        &mut self,
        plan: &filmcraft_render::plan::FramePlan,
        prep: Option<&dyn std::any::Any>,
    ) -> filmcraft_export::Result<Option<filmcraft_render::Image>> {
        self.0.try_render_planned(plan, prep).map_err(filmcraft_export::ExportError::Encode)
    }

    fn try_render_planned_rgba8(
        &mut self,
        plan: &filmcraft_render::plan::FramePlan,
        prep: Option<&dyn std::any::Any>,
    ) -> filmcraft_export::Result<Option<(u32, u32, Vec<u8>)>> {
        self.0.try_render_planned_rgba8(plan, prep).map_err(filmcraft_export::ExportError::Encode)
    }

    fn try_render_planned_gpu(
        &mut self,
        plan: &filmcraft_render::plan::FramePlan,
        prep: Option<&dyn std::any::Any>,
        target: &dyn filmcraft_export::GpuEncodeInput,
    ) -> filmcraft_export::Result<filmcraft_export::GpuFrame> {
        use filmcraft_export::GpuFrame;
        match self
            .0
            .try_render_planned_surface(plan, prep, target.as_any(), target.id(), target.width(), target.height())
            .map_err(filmcraft_export::ExportError::Encode)?
        {
            filmcraft_gpu::SurfaceRender::Rendered => Ok(GpuFrame::Rendered),
            filmcraft_gpu::SurfaceRender::NoImport => Ok(GpuFrame::NoImport),
            filmcraft_gpu::SurfaceRender::Unconfirmed => Ok(GpuFrame::Unconfirmed),
            filmcraft_gpu::SurfaceRender::Unavailable => Ok(GpuFrame::Unavailable),
        }
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

fn png(base: &std::path::Path, dir: &str, name: &str) -> Vec<u8> {
    // image_sequence_path flattens the path stem: <stem><index>.png in the temp dir itself.
    let index: u64 = name.trim_start_matches("img").trim_end_matches(".png").parse().unwrap_or(0);
    std::fs::read(base.join(format!("{dir}{index:03}.png"))).unwrap()
}

fn export_png(base: &std::path::Path, dir: &str, gpu: GpuRendering, size: (u32, u32)) {
    export_project_png(base, dir, gpu, size, false);
}

fn export_project_png(base: &std::path::Path, dir: &str, gpu: GpuRendering, (w, h): (u32, u32), adjusted: bool) {
    let (p, seq, m) = project_with(w, h, adjusted);
    let mut s = ExportSettings { format: Format::PngSequence, path: base.join(dir).to_string_lossy().into_owned(), ..Default::default() };
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

#[test]
fn gpu_export_matches_cpu_and_counts() {
    let Some(_) = filmcraft_gpu::ExportRenderer::new() else {
        eprintln!("SKIPPED (gpu export pipeline): no adapter");
        return;
    };
    // A fresh private folder, never a predictable name another user could have prepared; removed when the test ends.
    let mut b = tempfile::Builder::new();
    b.prefix("fc-gpu-export-");
    // owner-only, so another local user cannot swap an output for a symlink
    #[cfg(unix)]
    b.permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700));
    let tmp = b.tempdir().unwrap();
    let base = tmp.path();
    // Clean registry first: Auto with no factory must be byte-equal to Off.
    export_png(base, "off-cpu", GpuRendering::Off, (320, 180));
    export_png(base, "off-auto", GpuRendering::Auto, (320, 180));
    assert_eq!(png(base, "off-cpu", "img000.png"), png(base, "off-auto", "img000.png"), "Auto with no registered renderer must be byte-equal to Off");

    register_frame_renderer(|| Some(Box::new(GpuFrameRenderer(filmcraft_gpu::ExportRenderer::new().expect("adapter present")))));
    let sizes = [(320u32, 180u32), (17, 13), (1, 1), (257, 129)];
    for (i, size) in sizes.into_iter().enumerate() {
        export_png(base, "cpu", GpuRendering::Off, size);
        export_png(base, "gpu", GpuRendering::Auto, size);
        for f in ["000", "001"] {
            assert_parity(&png(base, "cpu", &format!("img{f}.png")), &png(base, "gpu", &format!("img{f}.png")), &format!("{:?} frame {f}", size));
        }
        let _ = i;
    }
    let GpuRenderStats { frames, .. } = filmcraft_export::gpu_render_stats();
    assert!(frames >= 8, "gpu frames counted across the parity exports: {frames}");

    // An adjustment layer is composited by the GPU too (a `PlanStep::Adjust`), not handed to the
    // CPU renderer, and matches the CPU export.
    let composited = filmcraft_gpu::export_renderer::timing::get().frames;
    let GpuRenderStats { fallbacks, .. } = filmcraft_export::gpu_render_stats();
    export_project_png(base, "adj-cpu", GpuRendering::Off, (320, 180), true);
    export_project_png(base, "adj-gpu", GpuRendering::Auto, (320, 180), true);
    for f in ["000", "001"] {
        assert_parity(&png(base, "adj-cpu", &format!("img{f}.png")), &png(base, "adj-gpu", &format!("img{f}.png")), &format!("adjustment layer frame {f}"));
    }
    assert_ne!(png(base, "adj-cpu", "img000.png"), png(base, "off-cpu", "img000.png"), "the adjustment layer changes the picture");
    let gpu = filmcraft_gpu::export_renderer::timing::get().frames - composited;
    assert_eq!(gpu, 2, "both frames with the adjustment layer composited on the GPU");
    assert_eq!(filmcraft_export::gpu_render_stats().fallbacks, fallbacks, "no frame with the adjustment layer fell back to the CPU");

    // Verify SDR RGBA8 fast path with post-readback VideoLimiter matches CPU export parity
    // and records 4 B/px readback instead of 8 B/px Rgba16Float + convert_ns.
    let (p, seq, m) = project_with(256, 144, false);
    let mut s_lim = ExportSettings { format: Format::PngSequence, path: base.join("lim-cpu").to_string_lossy().into_owned(), ..Default::default() };
    s_lim.range = Some(TimeRange::new(filmcraft_time::Tick::ZERO, filmcraft_time::FrameRate::FPS_24.tick_of(2)));
    s_lim.effects.video_limiter.enabled = true;
    s_lim.effects.video_limiter.min_percent = 10.0;
    s_lim.effects.video_limiter.max_percent = 80.0;
    s_lim.gpu_rendering = GpuRendering::Off;
    export(&p, seq, &s_lim, &m, &Progress::default()).unwrap();

    s_lim.path = base.join("lim-gpu").to_string_lossy().into_owned();
    s_lim.gpu_rendering = GpuRendering::Auto;
    let prev_convert = filmcraft_gpu::export_renderer::timing::get().convert;
    let prev_readback = filmcraft_frame::transfers::snapshot().export_readback_bytes;
    export(&p, seq, &s_lim, &m, &Progress::default()).unwrap();
    let convert_delta = filmcraft_gpu::export_renderer::timing::get().convert - prev_convert;
    let readback_delta = filmcraft_frame::transfers::snapshot().export_readback_bytes - prev_readback;
    assert!(convert_delta.is_zero(), "RGBA8 fast path must skip f16->f32 CPU conversion: {convert_delta:?}");
    assert_eq!(readback_delta, 2 * 256 * 144 * 4, "RGBA8 fast path must read 4 B/px");
    for f in ["000", "001"] {
        assert_parity(&png(base, "lim-cpu", &format!("img{f}.png")), &png(base, "lim-gpu", &format!("img{f}.png")), &format!("limiter RGBA8 frame {f}"));
    }

    // Verify a source whose GpuSurface fails both zero-copy import and CPU readback aborts
    // export with Err instead of silently falling back to CPU transparent/black pixels.
    #[derive(Debug)]
    struct FailingGpuSurface;
    impl filmcraft_frame::GpuSurface for FailingGpuSurface {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
        fn id(&self) -> usize {
            0xBAD_50FC
        }
        fn gpu_bytes(&self) -> u64 {
            64 * 64 * 4
        }
        fn bits(&self) -> u32 {
            8
        }
        fn chroma(&self) -> filmcraft_frame::Chroma {
            filmcraft_frame::Chroma::C444
        }
        fn to_cpu(&self) -> Result<filmcraft_frame::PixelData, String> {
            Err("synthetic source gpu surface readback failure".to_string())
        }
    }
    struct FailingGpuSource {
        info: filmcraft_media::MediaInfo,
    }
    impl MediaSource for FailingGpuSource {
        fn info(&self) -> &filmcraft_media::MediaInfo {
            &self.info
        }
        fn video_frame(&self, req: filmcraft_media::FrameRequest) -> filmcraft_media::Result<Arc<filmcraft_frame::VideoFrame>> {
            Ok(Arc::new(filmcraft_frame::VideoFrame {
                width: 64,
                height: 64,
                data: filmcraft_frame::PixelData::GpuSurface(Arc::new(FailingGpuSurface)),
                color: filmcraft_color::ColorInfo::REC709,
                par: (1, 1),
                pts: req.time,
            }))
        }
        fn audio(&self, _start: i64, _frames: usize, _sample_rate: u32) -> filmcraft_media::Result<filmcraft_frame::AudioBuffer> {
            Err(filmcraft_media::MediaError::NoStream("audio"))
        }
    }
    let fail_g = GeneratorSource::new(
        Generator::ColorMatte { color: [1.0, 0.0, 0.0, 1.0] },
        64,
        64,
        filmcraft_time::FrameRate::FPS_24,
        filmcraft_time::Tick(2 * TICKS_PER_SECOND),
    );
    let mut fail_proj = filmcraft_project::Project::new("fail-readback");
    let fail_info = fail_g.info().clone();
    let fail_item = fail_proj.add_item(
        "failing-gpu-source",
        Label::Iris,
        ItemKind::Media(Box::new(MediaClip {
            media: MediaRef::Generator(fail_g.generator.clone()),
            info: fail_info.clone(),
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
    let fail_seq = fail_proj.new_sequence(
        "s",
        SequenceSettings { width: 64, height: 64, frame_rate: filmcraft_time::FrameRate::FPS_24, ..Default::default() },
        1,
        1,
        None,
    );
    let fail_clip = fail_proj
        .make_track_item(
            fail_item,
            TrackKind::Video,
            filmcraft_time::Tick::ZERO,
            TimeRange::new(filmcraft_time::Tick::ZERO, filmcraft_time::FrameRate::FPS_24.tick_of(1)),
            filmcraft_time::FrameRate::FPS_24,
        )
        .unwrap();
    fail_proj.sequence_mut(fail_seq).unwrap().video_tracks[0].items.push(fail_clip);
    let mut fail_sources = SourceMap::default();
    fail_sources.0.insert(fail_item, Arc::new(FailingGpuSource { info: fail_info }));
    let fail_dir = base.join("fail-gpu-readback");
    std::fs::create_dir_all(&fail_dir).unwrap();
    let mut s_fail = ExportSettings { format: Format::PngSequence, path: fail_dir.join("img.png").to_string_lossy().into_owned(), ..Default::default() };
    s_fail.range = Some(TimeRange::new(filmcraft_time::Tick::ZERO, filmcraft_time::FrameRate::FPS_24.tick_of(1)));
    s_fail.gpu_rendering = GpuRendering::Auto;
    let fail_proj = Arc::new(fail_proj);
    let res = export(&fail_proj, fail_seq, &s_fail, &fail_sources, &Progress::default());
    assert!(
        matches!(res, Err(filmcraft_export::ExportError::Encode(ref msg)) if msg.contains("synthetic source gpu surface readback failure")),
        "export must fail rather than emit black pixels when GPU surface readback fails: {res:?}"
    );
    assert!(!fail_dir.join("img000.png").exists(), "failed GPU surface readback must not write output frames");
}
