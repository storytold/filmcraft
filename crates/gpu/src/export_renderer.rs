//! Off-screen GPU export renderer: composite a frame with the same
//! [`filmcraft_render::plan::plan_frame`] + [`GpuCompositor`] path the monitors use, then read
//! the **linear float accumulator** back (not the sRGB display texture), so the result matches
//! `filmcraft_render::render_sequence`'s premultiplied linear-light RGBA and the export pipeline's
//! placement, overlays and video limiter see exactly what the CPU renderer would have produced.
//!
//! Renderers share one off-screen wgpu device (no window), created by the first and kept for the
//! process, so an export can run several (one per export worker; each has its own compositor and
//! textures) without opening a device each. With no adapter (headless
//! machine, unsupported GPU) [`ExportRenderer::new`] returns `None` and the export falls back to
//! the CPU renderer; a `FramePlan::Image` plan (HDR / wide gamut / anything the plan sends to the
//! CPU) is returned unchanged — it already is the CPU image.

use crate::{GpuCompositor, prepare};
use filmcraft_project::{ItemId, Project};
use filmcraft_render::plan::{FramePlan, plan_frame};
use filmcraft_render::{Image, RenderOptions, SourceProvider};
use filmcraft_time::Tick;

/// Where export rendering spends its time (process-wide totals, for the bench).
pub mod timing {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    static SUBMIT_NS: AtomicU64 = AtomicU64::new(0);
    static MAP_WAIT_NS: AtomicU64 = AtomicU64::new(0);
    static CONVERT_NS: AtomicU64 = AtomicU64::new(0);
    static FRAMES: AtomicU64 = AtomicU64::new(0);

    /// Totals since the last [`reset`].
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct GpuTimings {
        /// Recording and submitting the frame's commands.
        pub submit: Duration,
        /// Waiting for the GPU and the staging buffer's mapping.
        pub map_wait: Duration,
        /// Half float → f32 conversion of the read-back pixels.
        pub convert: Duration,
        /// Frames composited by the GPU.
        pub frames: u64,
    }

    fn add(counter: &AtomicU64, d: Duration) {
        counter.fetch_add(u64::try_from(d.as_nanos()).unwrap_or(u64::MAX), Ordering::Relaxed);
    }

    pub(crate) fn add_submit(d: Duration) {
        add(&SUBMIT_NS, d);
    }

    pub(crate) fn add_map_wait(d: Duration) {
        add(&MAP_WAIT_NS, d);
    }

    pub(crate) fn add_convert(d: Duration) {
        add(&CONVERT_NS, d);
    }

    pub(crate) fn add_frame() {
        FRAMES.fetch_add(1, Ordering::Relaxed);
    }

    pub fn get() -> GpuTimings {
        let d = |c: &AtomicU64| Duration::from_nanos(c.load(Ordering::Relaxed));
        GpuTimings { submit: d(&SUBMIT_NS), map_wait: d(&MAP_WAIT_NS), convert: d(&CONVERT_NS), frames: FRAMES.load(Ordering::Relaxed) }
    }

    pub fn reset() {
        for c in [&SUBMIT_NS, &MAP_WAIT_NS, &CONVERT_NS, &FRAMES] {
            c.store(0, Ordering::Relaxed);
        }
    }
}

/// Set once the shared export device reported an uncaptured error (validation, out of memory)
/// or was lost: every export renders on the CPU from then on. The device is shared by every
/// renderer and lives in a `OnceLock`, so it is not recreated in this process.
static DEVICE_FAILED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Retire the shared export device, logging the first reason only.
fn mark_device_failed(why: &str) {
    if !DEVICE_FAILED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        log::error!("GPU export disabled for this session ({why}); exports render on the CPU");
    }
}

fn device_failed() -> bool {
    DEVICE_FAILED.load(std::sync::atomic::Ordering::SeqCst)
}

pub struct ExportRenderer {
    _instance: wgpu::Instance,
    #[cfg_attr(not(test), expect(dead_code, reason = "kept for future frame-accuracy flows (readback fences)"))]
    device: wgpu::Device,
    #[expect(dead_code, reason = "kept for future frame-accuracy flows (readback fences)")]
    queue: wgpu::Queue,
    compositor: GpuCompositor,
}

impl ExportRenderer {
    /// The off-screen device, or `None` when this host has no usable adapter or this build no
    /// wgpu backend.
    pub fn new() -> Option<Self> {
        // `None` is kept too: a machine without an adapter does not search again on every export
        static SHARED: std::sync::OnceLock<Option<(wgpu::Instance, wgpu::Device, wgpu::Queue)>> = std::sync::OnceLock::new();
        let (instance, device, queue) = SHARED
            .get_or_init(|| {
                // A build without a wgpu backend for this platform (`cargo build -p filmcraft-cli`
                // alone: this crate asks for none) has no GPU to offer, and `Instance::default()`
                // panics there.
                if wgpu::Instance::enabled_backend_features().is_empty() {
                    return None;
                }
                let instance = crate::adapter::native_instance();
                let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
                let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
                // wgpu's default handler panics on a validation or out-of-memory error; log it and
                // send the remaining frames to the CPU instead.
                device.on_uncaptured_error(std::sync::Arc::new(|e: wgpu::Error| mark_device_failed(&format!("GPU error: {e}"))));
                device.set_device_lost_callback(|reason, msg| mark_device_failed(&format!("GPU device lost ({reason:?}): {msg}")));
                Some((instance, device, queue))
            })
            .as_ref()?
            .clone();
        if device_failed() {
            return None;
        }
        let compositor = GpuCompositor::new(&device, &queue);
        Some(Self { _instance: instance, device, queue, compositor })
    }

    /// One frame as `render_sequence` would return it (premultiplied linear-light RGBA at the
    /// render size), or `None` when this frame must go to the CPU renderer.
    pub fn render(&mut self, project: &Project, seq: ItemId, t: Tick, opts: RenderOptions, sources: &dyn SourceProvider) -> Result<Option<Image>, String> {
        let plan = plan_frame(project, seq, t, opts, sources)?;
        Ok(match &plan {
            // Already the CPU image: return it unchanged instead of rendering it twice.
            FramePlan::Image(img) => Some(img.clone()),
            FramePlan::Layers { .. } => {
                if device_failed() {
                    return Ok(None);
                }
                let prep = prepare(&plan);
                let Some((w, h, px)) = self.compositor.render_export_prepared(&plan, Some(&prep))? else { return Ok(None) };
                // an error reported while this frame was drawn leaves its pixels untrusted
                if device_failed() {
                    return Ok(None);
                }
                timing::add_frame();
                Some(Image { w: w as usize, h: h as usize, px })
            }
            // Transitions: the export path leaves them to the CPU reference renderer.
            FramePlan::Composite { .. } => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{f16_to_f32, f32_to_f16};
    use filmcraft_export::FrameRenderer;
    use filmcraft_media::generators::GeneratorSource;
    use filmcraft_media::{Generator, MediaSource};
    use filmcraft_project::{ItemKind, Label, MediaClip, MediaRef, Project, SequenceSettings, TrackKind};
    use filmcraft_render::SourceMap;
    use filmcraft_time::{TICKS_PER_SECOND, Tick, TimeRange};
    use std::sync::Arc;

    const EPS: f32 = 1e-4;

    /// A solid colour generator at 320×180 plus a tone generator (mirrors the export tests'
    /// fixture-free project helper).
    fn project(transform: bool) -> (Arc<Project>, ItemId, SourceMap) {
        let mut p = Project::new("x");
        let g = GeneratorSource::new(
            Generator::ColorMatte { color: [0.2, 0.5, 0.9, 1.0] },
            320,
            180,
            filmcraft_time::FrameRate::FPS_24,
            Tick(2 * TICKS_PER_SECOND),
        );
        let tone = GeneratorSource::new(Generator::Tone { hz: 440.0, db: -6.0 }, 320, 180, filmcraft_time::FrameRate::FPS_24, Tick(2 * TICKS_PER_SECOND));
        let add = |p: &mut Project, g: &GeneratorSource| {
            let info = g.info().clone();
            p.add_item(
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
            )
        };
        let red = add(&mut p, &g);
        let t = add(&mut p, &tone);
        let seq =
            p.new_sequence("s", SequenceSettings { width: 320, height: 180, frame_rate: filmcraft_time::FrameRate::FPS_24, ..Default::default() }, 1, 1, None);
        let r = filmcraft_time::FrameRate::FPS_24;
        let v = p.make_track_item(red, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, r.tick_of(24)), r).unwrap();
        p.sequence_mut(seq).unwrap().video_tracks[0].items.push(v);
        let a = p.make_track_item(t, TrackKind::Audio, Tick::ZERO, TimeRange::new(Tick::ZERO, r.tick_of(24)), r).unwrap();
        if transform {
            // Motion + opacity as fixed effects (the way the golden transform scene does it).
            if let Some(clip) = p.sequence_mut(seq).unwrap().video_tracks[0].items.last_mut() {
                let m = clip.effect_mut("motion").unwrap();
                if let Some(pm) = m.param_mut("scale") {
                    pm.value = filmcraft_project::ParamValue::Float(45.0);
                }
                if let Some(pm) = m.param_mut("rotation") {
                    pm.value = filmcraft_project::ParamValue::Float(20.0);
                }
                if let Some(pm) = m.param_mut("position") {
                    pm.value = filmcraft_project::ParamValue::Vec2(filmcraft_geom::Vec2::new(215.0, 70.0));
                }
                if let Some(o) = clip.effect_mut("opacity")
                    && let Some(pm) = o.param_mut("opacity")
                {
                    pm.value = filmcraft_project::ParamValue::Float(70.0);
                }
            }
        }
        p.sequence_mut(seq).unwrap().audio_tracks[0].items.push(a);
        let mut m = SourceMap::default();
        m.0.insert(red, Arc::new(g));
        m.0.insert(t, Arc::new(tone));
        (Arc::new(p), seq, m)
    }

    /// GPU parity: p99 of the per-pixel max RGB difference (over black, 8-bit) ≤ 6 and mean < 1.5
    /// — the same criterion the golden GPU-vs-CPU parity tests use.
    fn assert_parity(cpu: &Image, gpu: &Image, what: &str) {
        assert_eq!((cpu.w, cpu.h), (gpu.w, gpu.h), "{what}: size mismatch");
        assert!(cpu.over_black_rgba8().as_chunks::<4>().0.iter().any(|px| px[..3].iter().any(|&v| v != 0)), "{what}: empty reference picture");
        let to8 = |img: &Image| -> Vec<u8> { img.over_black_rgba8() };
        let (a, b) = (to8(cpu), to8(gpu));
        let mut d: Vec<u32> = a
            .as_chunks::<4>()
            .0
            .iter()
            .zip(b.as_chunks::<4>().0)
            .map(|(x, y)| (0..3).map(|k| (x[k] as u32).abs_diff(y[k] as u32)).max().unwrap_or(0))
            .collect();
        d.sort_unstable();
        let p99 = d[d.len() * 99 / 100];
        let mean = d.iter().sum::<u32>() as f64 / d.len() as f64;
        eprintln!("{what}: GPU vs CPU over-black p99 {p99}, mean {mean:.3}");
        assert!(p99 <= 6 && mean < 1.5, "{what}: GPU parity p99 {p99}, mean {mean:.3}");
    }

    #[test]
    fn solid_frame_matches_cpu() {
        let Some(mut r) = ExportRenderer::new() else {
            eprintln!("SKIPPED (gpu export parity): no adapter");
            return;
        };
        let (p, seq, m) = project(false);
        let t = Tick::ZERO;
        let cpu = filmcraft_render::render_sequence(&p, seq, t, RenderOptions::default(), &m).unwrap();
        let gpu = r.render(&p, seq, t, RenderOptions::default(), &m).unwrap().expect("GPU rendered the frame");
        // The linear premultiplied accumulator should be very close to the CPU float image.
        assert_eq!((cpu.w, cpu.h), (gpu.w, gpu.h));
        let max: f32 = cpu.px.iter().zip(&gpu.px).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        eprintln!("solid: max |GPU-CPU| linear {max:.6}");
        assert!(max < 0.02, "solid frame differs by {max}");
        assert_parity(&cpu, &gpu, "solid");
    }

    #[test]
    fn transformed_frame_matches_cpu() {
        let Some(mut r) = ExportRenderer::new() else {
            eprintln!("SKIPPED (gpu export parity): no adapter");
            return;
        };
        let (p, seq, m) = project(true);
        let t = filmcraft_time::FrameRate::FPS_24.tick_of(10);
        let cpu = filmcraft_render::render_sequence(&p, seq, t, RenderOptions::default(), &m).unwrap();
        let gpu = r.render(&p, seq, t, RenderOptions::default(), &m).unwrap().expect("GPU rendered the frame");
        assert_parity(&cpu, &gpu, "transformed");
    }

    #[test]
    fn gpu_render_impls_frame_renderer() {
        // dev-dependency: the same wrapper the app and CLI register.
        struct R(ExportRenderer);
        impl FrameRenderer for R {
            fn render(&mut self, project: &Project, seq: ItemId, t: Tick, opts: RenderOptions, sources: &dyn SourceProvider) -> Result<Option<Image>, String> {
                self.0.render(project, seq, t, opts, sources)
            }
        }
        let Some(r) = ExportRenderer::new() else {
            eprintln!("SKIPPED (gpu export parity): no adapter");
            return;
        };
        let (p, seq, m) = project(false);
        let gpu = R(r).render(&p, seq, Tick::ZERO, RenderOptions::default(), &m).unwrap().expect("rendered");
        let cpu = filmcraft_render::render_sequence(&p, seq, Tick::ZERO, RenderOptions::default(), &m).unwrap();
        assert_parity(&cpu, &gpu, "frame renderer");
    }

    #[test]
    fn f16_roundtrip() {
        for v in [0.0f32, 1.0, 0.5, 2.0, 1e-3, 65504.0, -1.0, -0.25] {
            assert!((f16_to_f32(f32_to_f16(v)) - v).abs() <= EPS * v.abs().max(1.0), "f16 {v}");
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn destroyed_device_exports_changed_frames_on_cpu() {
        use filmcraft_export::{ExportSettings, Format, GpuRendering, Progress, export, gpu_render_stats, image_sequence_path, register_frame_renderer};
        use std::sync::atomic::Ordering;
        use std::time::{Duration, Instant};

        const CHILD_ENV: &str = "FILMCRAFT_EXPORT_DEVICE_LOSS_CHILD";
        const TEST: &str = "export_renderer::tests::destroyed_device_exports_changed_frames_on_cpu";
        // One minute bounds driver/readback hangs; not a performance threshold.
        const DEADLINE: Duration = Duration::from_secs(60);
        // Poll only process completion; no GPU scheduling or timing claims.
        const POLL_INTERVAL: Duration = Duration::from_millis(10);
        if std::env::var_os(CHILD_ENV).is_none() {
            // Shared OnceLock + failure flag must not poison other GPU tests.
            let mut child =
                std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact", TEST, "--nocapture"]).env(CHILD_ENV, "1").spawn().unwrap();
            let start = Instant::now();
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    assert!(status.success(), "device-loss child: {status}");
                    return;
                }
                if start.elapsed() >= DEADLINE {
                    let kill = child.kill();
                    let wait = child.wait();
                    panic!("device-loss child exceeded {DEADLINE:?}; kill={kill:?}, wait={wait:?}");
                }
                std::thread::sleep(POLL_INTERVAL);
            }
        }

        struct Renderer(ExportRenderer);
        impl FrameRenderer for Renderer {
            fn render(&mut self, p: &Project, seq: ItemId, t: Tick, opts: RenderOptions, sources: &dyn SourceProvider) -> Result<Option<Image>, String> {
                self.0.render(p, seq, t, opts, sources)
            }
        }
        fn factory() -> Option<Box<dyn FrameRenderer>> {
            ExportRenderer::new().map(|r| Box::new(Renderer(r)) as Box<dyn FrameRenderer>)
        }

        let Some(mut renderer) = ExportRenderer::new() else {
            eprintln!("SKIPPED (device-loss output): no adapter");
            return;
        };
        let (p, seq, sources) = project(false);
        let mut p = Arc::try_unwrap(p).unwrap();
        let rate = filmcraft_time::FrameRate::FPS_24;
        let item = p.sequence(seq).unwrap().video_tracks[0].items[0].item;
        p.sequence_mut(seq).unwrap().video_tracks[0].items[0].duration = rate.tick_of(1);
        let mut second = p.make_track_item(item, TrackKind::Video, rate.tick_of(1), TimeRange::new(Tick::ZERO, rate.tick_of(1)), rate).unwrap();
        // Half opacity makes the second frame distinct without another decoder or format.
        second.effect_mut("opacity").unwrap().param_mut("opacity").unwrap().value = filmcraft_project::ParamValue::Float(50.0);
        p.sequence_mut(seq).unwrap().video_tracks[0].items.push(second);
        let p = Arc::new(p);
        for frame in 0..2 {
            assert!(
                matches!(plan_frame(&p, seq, rate.tick_of(frame), RenderOptions::default(), &sources).unwrap(), FramePlan::Layers { layers, .. } if !layers.is_empty())
            );
        }

        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("filmcraft-device-loss-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        register_frame_renderer(factory);
        let run = |name: &str, mode| {
            let settings = ExportSettings {
                format: Format::PngSequence,
                path: dir.join(format!("{name}.png")).to_str().unwrap().to_owned(),
                range: Some(TimeRange::new(Tick::ZERO, rate.tick_of(2))),
                gpu_rendering: mode,
                ..Default::default()
            };
            let progress = Progress::default();
            let report = export(&p, seq, &settings, &sources, &progress).unwrap();
            assert_eq!(report.frames, 2);
            assert!(progress.total.load(Ordering::Relaxed) > 0, "export must report work");
            assert_eq!(progress.done.load(Ordering::Relaxed), progress.total.load(Ordering::Relaxed));
            (0..report.frames).map(|i| std::fs::read(image_sequence_path(&settings.path, i, report.frames)).unwrap()).collect::<Vec<_>>()
        };
        let reference = run("cpu", GpuRendering::Off);
        assert_ne!(reference[0], reference[1], "reference must change between frames");
        for bytes in &reference {
            let image = image::load_from_memory(bytes).unwrap().to_rgba8();
            assert!(image.pixels().any(|px| px.0[..3].iter().any(|&v| v != 0)), "reference must contain visible content");
        }
        let healthy = run("healthy", GpuRendering::Auto);
        assert_eq!(gpu_render_stats().frames, reference.len() as u64, "healthy control must use the registered GPU renderer");
        assert_eq!(gpu_render_stats().fallbacks, 0);
        assert_eq!(timing::get().frames, reference.len() as u64, "healthy control must composite on the GPU");
        for (frame, bytes) in healthy.iter().enumerate() {
            let gpu = image::load_from_memory(bytes).unwrap().to_rgba8();
            let cpu = image::load_from_memory(&reference[frame]).unwrap().to_rgba8();
            assert_eq!(gpu.dimensions(), cpu.dimensions());
            // Flat mattes have no edge outliers; use the existing parity limit for every channel.
            assert!(gpu.as_raw().iter().zip(cpu.as_raw()).all(|(a, b)| a.abs_diff(*b) <= 6), "healthy PNG differs from CPU reference");
        }
        let before = gpu_render_stats().frames;
        renderer.device.destroy();
        let _ = renderer.device.poll(wgpu::PollType::wait_indefinitely());
        assert!(device_failed(), "actual device destruction must reach the failure callback");
        assert!(
            renderer.render(&p, seq, Tick::ZERO, RenderOptions::default(), &sources).unwrap().is_none(),
            "existing renderer must decline lost-device frames"
        );
        assert!(factory().is_none(), "new exports must decline the lost shared device");
        let recovered = run("recovered", GpuRendering::Auto);
        assert_eq!(recovered, reference, "post-loss persisted PNGs must match GPU-Off bytes");
        assert_eq!(gpu_render_stats().frames, before, "post-loss export must not count GPU frames");
        assert_eq!(gpu_render_stats().fallbacks, 0, "factory decline bypasses the renderer pool");
        eprintln!("device-loss output: healthy GPU -> destroyed device -> CPU export; two changed PNGs match GPU-Off bytes");
        for entry in std::fs::read_dir(&dir).unwrap() {
            std::fs::remove_file(entry.unwrap().path()).unwrap();
        }
        std::fs::remove_dir(dir).unwrap();
    }
}
