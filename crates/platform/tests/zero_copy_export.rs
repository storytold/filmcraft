//! End-to-end verification of the Windows Vulkan -> D3D11 -> NVENC zero-copy export pipeline:
//! - GPU compositor resolves directly into NVENC-owned D3D11 shared RGBA8 textures
//!   (`export_readback_bytes == 0`, `encode_upload_bytes == 0`) for H.264, HEVC, and AV1.
//! - Output files decode cleanly and match non-zero-copy exports visually.
//! - Features requiring CPU post-processing (e.g. Video Limiter) fall back transparently to
//!   RGBA8 readback + NVENC RGBA upload without failing.
#![cfg(target_os = "windows")]

use std::sync::Arc;

use filmcraft_export::{
    BitrateMode, ExportSettings, Format, FrameRenderer, GpuEncodeInput, GpuRendering, HardwareEncoding, Progress, export, gpu_render_stats, hw_encode_stats,
};
use filmcraft_frame::transfers;
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{DemoScene, FrameRequest, Generator, MediaSource};
use filmcraft_project::{ItemKind, Label, MediaClip, MediaRef, Project, SequenceSettings, TrackKind};
use filmcraft_render::SourceMap;
use filmcraft_time::{FrameRate, Tick, TimeRange};

struct GpuFrameRenderer(filmcraft_gpu::ExportRenderer);

impl FrameRenderer for GpuFrameRenderer {
    fn render(
        &mut self,
        project: &Project,
        seq: filmcraft_project::ItemId,
        t: Tick,
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
        target: &dyn GpuEncodeInput,
    ) -> filmcraft_export::GpuFrame {
        use filmcraft_export::GpuFrame;
        match self.0.render_planned_surface(plan, prep, target.as_any(), target.id(), target.width(), target.height()) {
            filmcraft_gpu::SurfaceRender::Rendered => GpuFrame::Rendered,
            filmcraft_gpu::SurfaceRender::NoImport => GpuFrame::NoImport,
            filmcraft_gpu::SurfaceRender::Unconfirmed => GpuFrame::Unconfirmed,
            filmcraft_gpu::SurfaceRender::Unavailable => GpuFrame::Unavailable,
        }
    }
}

fn project(w: u32, h: u32, frames: i64) -> (Arc<Project>, filmcraft_project::ItemId, SourceMap) {
    let rate = FrameRate::FPS_24;
    let mut p = Project::new("zero-copy-export");
    let g = GeneratorSource::new(Generator::Demo(DemoScene::OceanSunset), w, h, rate, rate.tick_of(frames));
    let info = g.info().clone();
    let clip = p.add_item(
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
    let seq = p.new_sequence("s", SequenceSettings { width: w, height: h, frame_rate: rate, ..Default::default() }, 1, 1, None);
    let v = p.make_track_item(clip, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.tick_of(frames)), rate).unwrap();
    p.sequence_mut(seq).unwrap().video_tracks[0].items.push(v);
    let mut m = SourceMap::default();
    m.0.insert(clip, Arc::new(g));
    (Arc::new(p), seq, m)
}

fn vulkan_external_win32_supported() -> bool {
    let inst = wgpu::Instance::new(wgpu::InstanceDescriptor { backends: wgpu::Backends::VULKAN, ..wgpu::InstanceDescriptor::new_without_display_handle() });
    let Ok(adapter) = pollster::block_on(
        inst.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() }),
    ) else {
        return false;
    };
    adapter.features().contains(wgpu::Features::VULKAN_EXTERNAL_MEMORY_WIN32)
}

fn decode_all(path: &str) -> Vec<Arc<filmcraft_frame::VideoFrame>> {
    let bytes: Arc<[u8]> = std::fs::read(path).unwrap().into();
    let src = filmcraft_codecs::open_bytes(path, bytes).unwrap();
    let rate = src.info().frame_rate();
    let n = rate.frame_at(src.info().duration);
    (0..n).map(|i| src.video_frame(FrameRequest::full(rate.tick_of(i))).unwrap()).collect()
}

fn luma_psnr(a: &filmcraft_frame::VideoFrame, b: &filmcraft_frame::VideoFrame) -> f64 {
    let (x, y) = (a.luma8(), b.luma8());
    let mse = x.iter().zip(&y).map(|(p, q)| (f64::from(*p) - f64::from(*q)).powi(2)).sum::<f64>() / x.len() as f64;
    if mse == 0.0 { 99.0 } else { 10.0 * (255.0f64 * 255.0 / mse).log10() }
}

#[test]
fn nvenc_zero_copy_export_and_fallback() {
    filmcraft_platform::register();
    filmcraft_export::register_frame_renderer(|| filmcraft_gpu::ExportRenderer::new().map(|r| Box::new(GpuFrameRenderer(r)) as Box<dyn FrameRenderer>));

    if !filmcraft_platform::nvenc::available() || !vulkan_external_win32_supported() {
        eprintln!("SKIPPED: NVENC or Vulkan Win32 external memory unavailable on this host");
        return;
    }

    let dir = tempfile::Builder::new().prefix("fc-zc-export-").tempdir().unwrap();
    let nframes = 48i64;
    let (p, seq, m) = project(1280, 720, nframes);

    // The zero-copy and the readback exports of one codec differ by the video limiter (which forces
    // the RGBA8 readback + NVENC upload path) and by two lossy encodes of the same pictures, so only
    // a floor is meaningful: one stale, cut or blank frame in the middle of an otherwise perfect
    // export must fail here, whatever the mean says.
    let min_psnr = 28.0;

    // 1. Zero-copy export for H.264, HEVC, and AV1 (when available on the GPU), each compared frame
    //    by frame with the same sequence through the readback fallback.
    let formats = [
        (Format::H264, "h264_zc.mp4", true),
        (Format::Hevc, "hevc_zc.mp4", filmcraft_platform::nvenc::hevc_available()),
        (Format::Av1, "av1_zc.mp4", filmcraft_platform::nvenc::av1_available()),
    ];
    for (format, name, enabled) in formats {
        if !enabled {
            continue;
        }
        let path = dir.path().join(name).to_string_lossy().to_string();
        let settings = ExportSettings {
            format,
            path: path.clone(),
            include_audio: false,
            bitrate_kbps: 4000,
            bitrate_mode: BitrateMode::Vbr1Pass,
            hardware_encoding: HardwareEncoding::Auto,
            gpu_rendering: GpuRendering::Auto,
            ..Default::default()
        };
        let t0 = transfers::snapshot();
        let g0 = gpu_render_stats();
        let h0 = hw_encode_stats();
        let report = export(&p, seq, &settings, &m, &Progress::default()).unwrap();
        let t = transfers::snapshot() - t0;
        let g = gpu_render_stats();
        let h = hw_encode_stats();

        assert_eq!(report.encoder.as_deref(), Some("NVENC"), "{name}");
        assert_eq!(report.frames, nframes as u64, "{name}");
        // the counters are process-wide and a batch may render or encode around its frames, so
        // they are a floor, not an exact count
        assert!(h.frames - h0.frames >= nframes as u64, "{name}: every frame encoded by NVENC ({h:?})");
        assert!(g.frames - g0.frames >= nframes as u64, "{name}: every frame rendered on GPU ({g:?})");
        assert_eq!(g.fallbacks - g0.fallbacks, 0, "{name}: zero GPU fallbacks");
        assert_eq!(t.export_readback_bytes, 0, "{name}: zero-copy must not read back to CPU");
        assert_eq!(t.encode_upload_bytes, 0, "{name}: zero-copy must not upload to NVENC from CPU");

        let decoded = decode_all(&path);
        assert_eq!(decoded.len(), nframes as usize, "{name}: decoded frame count");
        if let Some(ff) = filmcraft_testkit::ffmpeg() {
            let out = std::process::Command::new(ff).args(["-v", "error", "-i", &path, "-f", "null", "-"]).output().unwrap();
            assert!(out.stderr.is_empty(), "{name} ffmpeg errors: {}", String::from_utf8_lossy(&out.stderr));
        }

        // 2. The same frames through the readback fallback: the CPU Video Limiter turns zero-copy
        //    off, so NVENC gets RGBA8 read back from the GPU.
        let fb_path = dir.path().join(format!("{name}_fallback.mp4")).to_string_lossy().to_string();
        let mut fb_settings = ExportSettings {
            format,
            path: fb_path.clone(),
            include_audio: false,
            bitrate_kbps: 4000,
            bitrate_mode: BitrateMode::Vbr1Pass,
            hardware_encoding: HardwareEncoding::Auto,
            gpu_rendering: GpuRendering::Auto,
            ..Default::default()
        };
        fb_settings.effects.video_limiter.enabled = true;
        let tf0 = transfers::snapshot();
        let fb_report = export(&p, seq, &fb_settings, &m, &Progress::default()).unwrap();
        let tf = transfers::snapshot() - tf0;
        assert_eq!(fb_report.encoder.as_deref(), Some("NVENC"), "{name}: fallback encoder");
        assert!(tf.export_readback_bytes > 0, "{name}: video limiter must trigger GPU readback");
        assert!(tf.encode_upload_bytes > 0, "{name}: video limiter must trigger CPU->NVENC upload");

        let fb_frames = decode_all(&fb_path);
        assert_eq!(fb_frames.len(), decoded.len(), "{name}: fallback decoded frame count");
        let psnrs: Vec<f64> = decoded.iter().zip(&fb_frames).map(|(a, b)| luma_psnr(a, b)).collect();
        let worst = psnrs.iter().copied().fold(f64::INFINITY, f64::min);
        let mean = psnrs.iter().sum::<f64>() / psnrs.len() as f64;
        eprintln!("{name}: frame PSNR min {worst:.1} dB, mean {mean:.1} dB");
        assert!(worst > min_psnr, "{name}: worst-frame PSNR {worst:.1} dB (mean {mean:.1}), zero-copy and fallback disagree on a frame");
    }
}
