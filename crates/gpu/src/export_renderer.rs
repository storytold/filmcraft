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
    #[expect(dead_code, reason = "kept for future frame-accuracy flows (readback fences)")]
    device: wgpu::Device,
    #[expect(dead_code, reason = "kept for future frame-accuracy flows (readback fences)")]
    queue: wgpu::Queue,
    compositor: GpuCompositor,
}

impl ExportRenderer {
    /// The off-screen device, or `None` when this host has no usable adapter.
    pub fn new() -> Option<Self> {
        // `None` is kept too: a machine without an adapter does not search again on every export
        static SHARED: std::sync::OnceLock<Option<(wgpu::Instance, wgpu::Device, wgpu::Queue)>> = std::sync::OnceLock::new();
        let (instance, device, queue) = SHARED
            .get_or_init(|| {
                let instance = wgpu::Instance::default();
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
    pub fn render(&mut self, project: &Project, seq: ItemId, t: Tick, opts: RenderOptions, sources: &dyn SourceProvider) -> Option<Image> {
        let plan = plan_frame(project, seq, t, opts, sources);
        match &plan {
            // Already the CPU image: return it unchanged instead of rendering it twice.
            FramePlan::Image(img) => Some(img.clone()),
            FramePlan::Layers { .. } => {
                if device_failed() {
                    return None;
                }
                let prep = prepare(&plan);
                let (w, h, px) = self.compositor.render_export_prepared(&plan, Some(&prep))?;
                // an error reported while this frame was drawn leaves its pixels untrusted
                if device_failed() {
                    return None;
                }
                timing::add_frame();
                Some(Image { w: w as usize, h: h as usize, px })
            }
            // Transitions: the export path leaves them to the CPU reference renderer.
            FramePlan::Composite { .. } => None,
        }
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
            )
        };
        let red = add(&mut p, &g);
        let t = add(&mut p, &tone);
        let seq =
            p.new_sequence("s", SequenceSettings { width: 320, height: 180, frame_rate: filmcraft_time::FrameRate::FPS_24, ..Default::default() }, 1, 1, None);
        let r = filmcraft_time::FrameRate::FPS_24;
        let _v = p.make_track_item(red, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, r.tick_of(24)), r).unwrap();
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
        let cpu = filmcraft_render::render_sequence(&p, seq, t, RenderOptions::default(), &m);
        let gpu = r.render(&p, seq, t, RenderOptions::default(), &m).expect("GPU rendered the frame");
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
        let cpu = filmcraft_render::render_sequence(&p, seq, t, RenderOptions::default(), &m);
        let gpu = r.render(&p, seq, t, RenderOptions::default(), &m).expect("GPU rendered the frame");
        assert_parity(&cpu, &gpu, "transformed");
    }

    #[test]
    fn gpu_render_impls_frame_renderer() {
        // dev-dependency: the same wrapper the app and CLI register.
        struct R(ExportRenderer);
        impl FrameRenderer for R {
            fn render(&mut self, project: &Project, seq: ItemId, t: Tick, opts: RenderOptions, sources: &dyn SourceProvider) -> Option<Image> {
                self.0.render(project, seq, t, opts, sources)
            }
        }
        let Some(r) = ExportRenderer::new() else {
            eprintln!("SKIPPED (gpu export parity): no adapter");
            return;
        };
        let (p, seq, m) = project(false);
        let gpu = R(r).render(&p, seq, Tick::ZERO, RenderOptions::default(), &m).expect("rendered");
        let cpu = filmcraft_render::render_sequence(&p, seq, Tick::ZERO, RenderOptions::default(), &m);
        assert_parity(&cpu, &gpu, "frame renderer");
    }

    #[test]
    fn f16_roundtrip() {
        for v in [0.0f32, 1.0, 0.5, 2.0, 1e-3, 65504.0, -1.0, -0.25] {
            assert!((f16_to_f32(f32_to_f16(v)) - v).abs() <= EPS * v.abs().max(1.0), "f16 {v}");
        }
    }
}
