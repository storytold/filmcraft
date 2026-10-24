//! An H.264 export with GPU rendering through the real GPU renderer: every frame takes the 4:2:0
//! path (`FrameRenderer::render_yuv420`), and the file decodes to the same pictures as the CPU
//! export. Its own test binary: `export_pipeline.rs` starts from an empty renderer registry.

use filmcraft_export::{ExportSettings, Format, FrameRenderer, GpuRendering, Progress, export, register_frame_renderer};
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{DemoScene, FrameRequest, Generator, MediaSource};
use filmcraft_project::{ItemKind, Label, MediaClip, MediaRef, SequenceSettings, TrackKind};
use filmcraft_render::SourceMap;
use filmcraft_time::{FrameRate, Tick, TimeRange};
use std::sync::Arc;

/// The wrapper the app and CLI register.
struct GpuFrameRenderer(filmcraft_gpu::ExportRenderer);

impl FrameRenderer for GpuFrameRenderer {
    fn render(
        &mut self,
        project: &filmcraft_project::Project,
        seq: filmcraft_project::ItemId,
        t: Tick,
        opts: filmcraft_render::RenderOptions,
        sources: &dyn filmcraft_render::SourceProvider,
    ) -> Option<filmcraft_render::Image> {
        self.0.render(project, seq, t, opts, sources)
    }

    fn render_yuv420(
        &mut self,
        project: &filmcraft_project::Project,
        seq: filmcraft_project::ItemId,
        t: filmcraft_time::Tick,
        opts: filmcraft_render::RenderOptions,
        sources: &dyn filmcraft_render::SourceProvider,
    ) -> Option<filmcraft_frame::VideoFrame> {
        self.0.render_yuv420(project, seq, t, opts, sources)
    }
}

fn project() -> (Arc<filmcraft_project::Project>, filmcraft_project::ItemId, SourceMap) {
    let rate = FrameRate::FPS_24;
    let mut p = filmcraft_project::Project::new("x");
    let g = GeneratorSource::new(Generator::Demo(DemoScene::OceanSunset), 320, 180, rate, rate.tick_of(24));
    let info = g.info().clone();
    let clip = p.add_item(
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
    let seq = p.new_sequence("s", SequenceSettings { width: 320, height: 180, frame_rate: rate, ..Default::default() }, 1, 1, None);
    let v = p.make_track_item(clip, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.tick_of(24)), rate).unwrap();
    p.sequence_mut(seq).unwrap().video_tracks[0].items.push(v);
    let mut m = SourceMap::default();
    m.0.insert(clip, Arc::new(g));
    (Arc::new(p), seq, m)
}

fn decode(path: &str) -> Vec<Arc<filmcraft_frame::VideoFrame>> {
    let bytes: Arc<[u8]> = std::fs::read(path).unwrap().into();
    let src = filmcraft_codecs::open_bytes(path, bytes).unwrap();
    let rate = src.info().frame_rate();
    (0..rate.frame_at(src.info().duration)).map(|i| src.video_frame(FrameRequest::full(rate.tick_of(i))).unwrap()).collect()
}

fn luma_psnr(a: &filmcraft_frame::VideoFrame, b: &filmcraft_frame::VideoFrame) -> f64 {
    let (x, y) = (a.luma8(), b.luma8());
    let mse = x.iter().zip(&y).map(|(p, q)| (f64::from(*p) - f64::from(*q)).powi(2)).sum::<f64>() / x.len() as f64;
    if mse == 0.0 { 99.0 } else { 10.0 * (255.0f64 * 255.0 / mse).log10() }
}

#[test]
fn h264_export_takes_the_gpu_yuv_path() {
    if filmcraft_gpu::ExportRenderer::new().is_none() {
        eprintln!("SKIPPED (gpu yuv export): no adapter");
        return;
    }
    register_frame_renderer(|| filmcraft_gpu::ExportRenderer::new().map(|r| Box::new(GpuFrameRenderer(r)) as Box<dyn FrameRenderer>));
    let (p, seq, m) = project();
    let dir = std::env::temp_dir().join(format!("fc-gpu-yuv-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = |n: &str| dir.join(n).to_string_lossy().to_string();
    let base = ExportSettings { format: Format::H264, include_audio: false, bitrate_kbps: 4000, ..Default::default() };
    let before = filmcraft_export::gpu_render_stats().yuv_frames;
    let gpu = ExportSettings { path: path("gpu.mp4"), gpu_rendering: GpuRendering::Auto, ..base.clone() };
    let report = export(&p, seq, &gpu, &m, &Progress::default()).unwrap();
    let taken = filmcraft_export::gpu_render_stats().yuv_frames - before;
    assert_eq!(taken, report.frames, "every frame through the GPU 4:2:0 pass");
    let cpu = ExportSettings { path: path("cpu.mp4"), gpu_rendering: GpuRendering::Off, ..base };
    export(&p, seq, &cpu, &m, &Progress::default()).unwrap();
    let (a, b) = (decode(&path("gpu.mp4")), decode(&path("cpu.mp4")));
    assert_eq!(a.len(), b.len());
    let worst = a.iter().zip(&b).map(|(x, y)| luma_psnr(x, y)).fold(99.0, f64::min);
    eprintln!("GPU 4:2:0 export vs CPU export: worst luma PSNR {worst:.1} dB over {} frames", a.len());
    assert!(worst >= 40.0, "GPU 4:2:0 export vs CPU export: {worst:.1} dB");
}
