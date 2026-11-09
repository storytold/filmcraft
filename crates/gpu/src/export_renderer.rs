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
use filmcraft_render::plan::{DeviceCaps, FramePlan, plan_frame};
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
                // The GPU the app's window asks for: the high-performance one (`WGPU_POWER_PREF`
                // overrides it, as for the window). With no preference the first adapter listed
                // can be an integrated GPU whose read-back memory is uncached.
                let power_preference = wgpu::PowerPreference::from_env().unwrap_or(wgpu::PowerPreference::HighPerformance);
                // the adapter `request_adapter` would return, except where that one's driver is
                // known to crash creating the device (`adapter::choose`); among the backends
                // `WGPU_BACKEND` names, as for the window, when this instance has any of them
                let mut adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()));
                if let Some(named) = wgpu::Backends::from_env()
                    && adapters.iter().any(|a| named.contains(a.get_info().backend.into()))
                {
                    adapters.retain(|a| named.contains(a.get_info().backend.into()));
                }
                let adapter = crate::adapter::pick(&adapters, None, power_preference).ok()?;
                log::info!("export rendering on {} ({:?})", adapter.get_info().name, adapter.get_info().backend);
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

    /// What this renderer's device runs: frames are planned with it, here and by the export
    /// before it takes this renderer.
    pub fn caps(&self) -> DeviceCaps {
        self.compositor.caps()
    }

    /// One frame as `render_sequence` would return it (premultiplied linear-light RGBA at the
    /// render size), or `None` when this frame must go to the CPU renderer.
    pub fn render(&mut self, project: &Project, seq: ItemId, t: Tick, opts: RenderOptions, sources: &dyn SourceProvider) -> Option<Image> {
        let plan = plan_frame(project, seq, t, opts, self.compositor.caps(), sources);
        match &plan {
            // Already the CPU image: return it unchanged instead of rendering it twice.
            FramePlan::Image(img) => Some(img.clone()),
            FramePlan::Layers { .. } | FramePlan::Composite { .. } => {
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

    /// A solid colour generator at 320×180 on V1 (V2 empty) plus a tone generator (mirrors the
    /// export tests' fixture-free project helper).
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
            p.new_sequence("s", SequenceSettings { width: 320, height: 180, frame_rate: filmcraft_time::FrameRate::FPS_24, ..Default::default() }, 2, 1, None);
        let r = filmcraft_time::FrameRate::FPS_24;
        let v = p.make_track_item(red, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, r.tick_of(24)), r).unwrap();
        let a = p.make_track_item(t, TrackKind::Audio, Tick::ZERO, TimeRange::new(Tick::ZERO, r.tick_of(24)), r).unwrap();
        p.sequence_mut(seq).unwrap().video_tracks[0].items.push(v);
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
        assert!(cpu.px.as_chunks::<4>().0.iter().any(|pixel| pixel[..3].iter().any(|x| *x > 0.1)), "solid fixture must contain a visible picture");
        // The linear premultiplied accumulator should be very close to the CPU float image.
        assert_eq!((cpu.w, cpu.h), (gpu.w, gpu.h));
        let max: f32 = cpu.px.iter().zip(&gpu.px).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        eprintln!("solid: max |GPU-CPU| linear {max:.6}");
        assert!(max < 0.02, "solid frame differs by {max}");
        assert_parity(&cpu, &gpu, "solid");
    }

    /// Without an effect stage the renderer says so (`DeviceCaps::NONE`): the export renders a
    /// frame with an adjustment layer on its own thread (`is_cpu_frame`) instead of taking this
    /// renderer, and a frame planned for it holds no adjustment step to composite on the CPU here.
    #[test]
    fn adjustment_layers_without_an_effect_stage_go_back_to_the_cpu() {
        let Some(mut r) = ExportRenderer::new() else {
            eprintln!("SKIPPED (gpu export adjustment): no adapter");
            return;
        };
        let (p, seq, m) = project(false);
        let mut p = Arc::unwrap_or_clone(p);
        let rate = filmcraft_time::FrameRate::FPS_24;
        let a = p.add_item("Adjustment Layer", Label::Lavender, ItemKind::AdjustmentLayer { width: 320, height: 180, rate, duration: rate.tick_of(24) }, None);
        let mut ti = p.make_track_item(a, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.tick_of(24)), rate).unwrap();
        let mut e = filmcraft_project::find_effect("proc_amp").unwrap().instance();
        e.param_mut("brightness").unwrap().value = filmcraft_project::ParamValue::Float(25.0);
        ti.effects.insert(0, e);
        p.sequence_mut(seq).unwrap().video_tracks[1].items.push(ti);
        let (t, opts) = (Tick::ZERO, RenderOptions::default());
        let cpu = filmcraft_render::render_sequence(&p, seq, t, opts, &m);
        if r.compositor.fx.is_some() {
            assert!(!filmcraft_render::plan::is_cpu_frame(&p, seq, t, opts, r.caps()), "the effect stage runs the adjustment layer");
            let gpu = r.render(&p, seq, t, opts, &m).expect("GPU rendered the frame");
            assert_parity(&cpu, &gpu, "adjustment layer");
            assert_eq!((r.compositor.gpu_adjustments, r.compositor.cpu_adjustments), (1, 0));
        }
        r.compositor.fx = None;
        assert_eq!(r.caps(), DeviceCaps::NONE);
        assert!(filmcraft_render::plan::is_cpu_frame(&p, seq, t, opts, r.caps()), "no effect stage: the export renders the frame without this renderer");
        let counts = (r.compositor.gpu_adjustments, r.compositor.cpu_adjustments);
        // asked anyway, it hands back the CPU renderer's picture, planned as one image
        let img = r.render(&p, seq, t, opts, &m).expect("the CPU image the frame was planned as");
        assert_eq!(img.px, cpu.px);
        assert_eq!((r.compositor.gpu_adjustments, r.compositor.cpu_adjustments), counts, "no adjustment step reached the compositor");
    }

    /// `g` as a media item of `p`, its source in `m`.
    fn media_item(p: &mut Project, m: &mut SourceMap, g: GeneratorSource) -> ItemId {
        let info = g.info().clone();
        let media = MediaClip {
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
        let item = p.add_item(&info.name, Label::Iris, ItemKind::Media(Box::new(media)), None);
        m.0.insert(item, Arc::new(g));
        item
    }

    /// `item` on video track `track` of `seq` from frame `start` for `frames` frames, scaled to
    /// the frame; returns the clip.
    fn put(p: &mut Project, seq: ItemId, track: usize, item: ItemId, start: i64, frames: i64) -> filmcraft_project::ClipId {
        let rate = filmcraft_time::FrameRate::FPS_24;
        let mut ti = p.make_track_item(item, TrackKind::Video, rate.tick_of(start), TimeRange::new(Tick::ZERO, rate.tick_of(frames)), rate).unwrap();
        ti.scale_to_frame = true;
        let id = ti.id;
        let q = p.sequence_mut(seq).unwrap();
        q.video_tracks[track].items.push(ti);
        q.video_tracks[track].sort();
        id
    }

    fn set_param(p: &mut Project, seq: ItemId, clip: filmcraft_project::ClipId, effect: &str, param: &str, v: f64) {
        let it = p.sequence_mut(seq).unwrap().find_item_mut(clip).unwrap().1;
        it.effect_mut(effect).unwrap().params.get_mut(param).unwrap().value = filmcraft_project::ParamValue::Float(v);
    }

    /// A full-frame adjustment layer on track `track` of `seq` for frames 0..48 with Proc Amp
    /// (brightness 25) and Gaussian Blur (6): the kind a device with an effect stage runs as a
    /// `PlanStep::Adjust`, the blur spreading the picture into any transparent part of the canvas.
    fn adjustment_layer(p: &mut Project, seq: ItemId, track: usize) {
        let rate = filmcraft_time::FrameRate::FPS_24;
        let a = p.add_item("Adjustment Layer", Label::Lavender, ItemKind::AdjustmentLayer { width: 320, height: 180, rate, duration: rate.tick_of(48) }, None);
        let clip = put(p, seq, track, a, 0, 48);
        let it = p.sequence_mut(seq).unwrap().find_item_mut(clip).unwrap().1;
        for (id, param, v) in [("gaussian_blur", "blurriness", 6.0), ("proc_amp", "brightness", 25.0)] {
            let mut e = filmcraft_project::find_effect(id).unwrap().instance();
            e.param_mut(param).unwrap().value = filmcraft_project::ParamValue::Float(v);
            it.effects.insert(0, e);
        }
    }

    /// The alpha of `gpu` against the CPU reference's, in 8-bit steps (`assert_parity` compares
    /// the colour over black, which hides a wrong alpha).
    fn assert_alpha_parity(cpu: &Image, gpu: &Image, what: &str) {
        let mut d: Vec<u32> =
            cpu.px.iter().skip(3).step_by(4).zip(gpu.px.iter().skip(3).step_by(4)).map(|(a, b)| ((a - b).abs() * 255.0).round() as u32).collect();
        d.sort_unstable();
        let p99 = d[d.len() * 99 / 100];
        let max = d.last().copied().unwrap_or(0);
        eprintln!("{what}: GPU vs CPU alpha p99 {p99}, max {max}");
        assert!(p99 <= 2 && max <= 8, "{what}: alpha parity p99 {p99}, max {max}");
    }

    /// A Push under an adjustment layer exports on the GPU (the Push and then the adjustment
    /// layer, both by the compositor) and matches `render_sequence` over black, forward and
    /// reversed, at full and half resolution.
    #[test]
    fn push_under_an_adjustment_layer_exports_on_the_gpu() {
        let Some(mut r) = ExportRenderer::new() else {
            eprintln!("SKIPPED (gpu push + adjustment export): no adapter");
            return;
        };
        if r.caps() == DeviceCaps::NONE {
            eprintln!("SKIPPED (gpu push + adjustment export): no effect stage on this device");
            return;
        }
        let mut p = Project::new("x");
        let mut m = SourceMap::default();
        let ocean = media_item(&mut p, &mut m, GeneratorSource::demo(filmcraft_media::DemoScene::OceanSunset));
        let blue = media_item(
            &mut p,
            &mut m,
            GeneratorSource::new(
                Generator::ColorMatte { color: [0.1, 0.4, 0.95, 1.0] },
                320,
                180,
                filmcraft_time::FrameRate::FPS_24,
                Tick(4 * TICKS_PER_SECOND),
            ),
        );
        let seq =
            p.new_sequence("s", SequenceSettings { width: 320, height: 180, frame_rate: filmcraft_time::FrameRate::FPS_24, ..Default::default() }, 2, 0, None);
        let a = put(&mut p, seq, 0, ocean, 0, 24);
        let b = put(&mut p, seq, 0, blue, 24, 24);
        adjustment_layer(&mut p, seq, 1);
        let rate = filmcraft_time::FrameRate::FPS_24;
        for reverse in [false, true] {
            p.sequence_mut(seq).unwrap().video_tracks[0].transitions = vec![filmcraft_project::Transition {
                id: filmcraft_project::TransitionId(1),
                effect: filmcraft_project::find_effect("push").unwrap().instance(),
                start: rate.tick_of(18),
                duration: rate.tick_of(12),
                from: Some(a),
                to: Some(b),
                align: Default::default(),
                reverse,
            }];
            for scale in [1.0, 0.5] {
                let opts = RenderOptions { scale, ..Default::default() };
                for f in [20, 24, 27] {
                    let (t, what) = (rate.tick_of(f), format!("Push + adjustment, reverse {reverse}, scale {scale}, frame {f}"));
                    assert!(!filmcraft_render::plan::is_cpu_frame(&p, seq, t, opts, r.caps()), "{what}: takes a GPU renderer");
                    let before = (r.compositor.gpu_transitions, r.compositor.gpu_adjustments, r.compositor.cpu_adjustments);
                    let gpu = r.render(&p, seq, t, opts, &m).expect("GPU frame");
                    let after = (r.compositor.gpu_transitions, r.compositor.gpu_adjustments, r.compositor.cpu_adjustments);
                    assert_eq!(after, (before.0 + 1, before.1 + 1, before.2), "{what}: the Push and the adjustment layer drawn on the GPU");
                    let cpu = filmcraft_render::render_sequence(&p, seq, t, opts, &m);
                    assert_parity(&cpu, &gpu, &what);
                }
            }
        }
    }

    /// Adjustment layers composited on the GPU match `render_sequence` (not the plan's own CPU
    /// executor) over a partly transparent canvas, one over another, over an empty canvas, over a
    /// nested sequence and inside one, at full and half resolution, alpha included.
    #[test]
    fn adjustment_layers_match_the_cpu_renderer() {
        let Some(mut r) = ExportRenderer::new() else {
            eprintln!("SKIPPED (gpu adjustment parity): no adapter");
            return;
        };
        if r.caps() == DeviceCaps::NONE {
            eprintln!("SKIPPED (gpu adjustment parity): no effect stage on this device");
            return;
        }
        let rate = filmcraft_time::FrameRate::FPS_24;
        let settings = SequenceSettings { width: 320, height: 180, frame_rate: rate, ..Default::default() };
        // V1 of `seq`: a 160×88 matte at 70 % opacity in the middle, unscaled, so transparent
        // around it and translucent inside, with its edges on whole pixels at both scales: the
        // comparison measures the adjustment layer, not how a resampled edge is covered
        let patch = |p: &mut Project, m: &mut SourceMap, seq: ItemId| {
            let g = media_item(p, m, GeneratorSource::new(Generator::ColorMatte { color: [0.9, 0.5, 0.2, 1.0] }, 160, 88, rate, Tick(4 * TICKS_PER_SECOND)));
            let c = put(p, seq, 0, g, 0, 48);
            p.sequence_mut(seq).unwrap().find_item_mut(c).unwrap().1.scale_to_frame = false;
            set_param(p, seq, c, "opacity", "opacity", 70.0);
        };
        // (what, project, sequence, sources, adjustment steps in the frame)
        let mut cases: Vec<(&str, Project, ItemId, SourceMap, u64)> = Vec::new();
        {
            let (mut p, mut m) = (Project::new("x"), SourceMap::default());
            let seq = p.new_sequence("s", settings.clone(), 2, 0, None);
            patch(&mut p, &mut m, seq);
            adjustment_layer(&mut p, seq, 1);
            cases.push(("a partly transparent canvas", p, seq, m, 1));
        }
        {
            let (mut p, mut m) = (Project::new("x"), SourceMap::default());
            let seq = p.new_sequence("s", settings.clone(), 3, 0, None);
            patch(&mut p, &mut m, seq);
            adjustment_layer(&mut p, seq, 1);
            adjustment_layer(&mut p, seq, 2);
            cases.push(("two adjustment layers, one over the other", p, seq, m, 2));
        }
        {
            // nothing below the adjustment layer: it adjusts an empty canvas
            let (mut p, m) = (Project::new("x"), SourceMap::default());
            let seq = p.new_sequence("s", settings.clone(), 2, 0, None);
            adjustment_layer(&mut p, seq, 1);
            cases.push(("an empty canvas", p, seq, m, 1));
        }
        {
            // the matte inside a nested sequence on V1, drawn with the outer layers
            let (mut p, mut m) = (Project::new("x"), SourceMap::default());
            let inner = p.new_sequence("inner", settings.clone(), 1, 0, None);
            patch(&mut p, &mut m, inner);
            let seq = p.new_sequence("s", settings.clone(), 2, 0, None);
            put(&mut p, seq, 0, inner, 0, 48);
            adjustment_layer(&mut p, seq, 1);
            cases.push(("a nested sequence below", p, seq, m, 1));
        }
        {
            // an adjustment layer inside a nested sequence adjusts only the nest (the matte), not
            // the red matte under the nest
            let (mut p, mut m) = (Project::new("x"), SourceMap::default());
            let red = media_item(
                &mut p,
                &mut m,
                GeneratorSource::new(Generator::ColorMatte { color: [0.9, 0.2, 0.1, 1.0] }, 320, 180, rate, Tick(4 * TICKS_PER_SECOND)),
            );
            let inner = p.new_sequence("inner", settings.clone(), 2, 0, None);
            patch(&mut p, &mut m, inner);
            adjustment_layer(&mut p, inner, 1);
            let seq = p.new_sequence("s", settings.clone(), 2, 0, None);
            put(&mut p, seq, 0, red, 0, 48);
            put(&mut p, seq, 1, inner, 0, 48);
            cases.push(("an adjustment layer inside a nested sequence", p, seq, m, 0));
        }
        for (what, p, seq, m, steps) in &cases {
            for scale in [1.0, 0.5] {
                let (t, opts) = (rate.tick_of(12), RenderOptions { scale, ..Default::default() });
                let what = format!("{what} at {scale}");
                assert!(!filmcraft_render::plan::is_cpu_frame(p, *seq, t, opts, r.caps()), "{what}: takes a GPU renderer");
                let before = (r.compositor.gpu_adjustments, r.compositor.cpu_adjustments);
                let gpu = r.render(p, *seq, t, opts, m).expect("GPU frame");
                assert_eq!((r.compositor.gpu_adjustments, r.compositor.cpu_adjustments), (before.0 + steps, before.1), "{what}: adjustment steps on the GPU");
                let cpu = filmcraft_render::render_sequence(p, *seq, t, opts, m);
                assert_parity(&cpu, &gpu, &what);
                assert_alpha_parity(&cpu, &gpu, &what);
            }
        }
    }

    /// Frames inside a Push transition (forward and reversed), which plans as a GPU composite,
    /// export on the GPU and match the CPU; a transition it cannot draw stays a CPU frame.
    #[test]
    fn transition_frames_export_on_the_gpu() {
        let Some(mut r) = ExportRenderer::new() else {
            eprintln!("SKIPPED (gpu transition export): no adapter");
            return;
        };
        let rate = filmcraft_time::FrameRate::FPS_24;
        let mut p = Project::new("x");
        let mut m = SourceMap::default();
        let mut clips = Vec::new();
        for (i, color) in [[0.9, 0.2, 0.1, 1.0], [0.1, 0.4, 0.95, 1.0]].into_iter().enumerate() {
            let g = GeneratorSource::new(Generator::ColorMatte { color }, 320, 180, rate, Tick(4 * TICKS_PER_SECOND));
            let info = g.info().clone();
            let media = MediaClip {
                media: MediaRef::Generator(g.generator.clone()),
                info,
                interpret: Default::default(),
                mark_in: None,
                mark_out: None,
                markers: vec![],
                offline: false,
                proxy: None,
                identity: None,
            };
            let item = p.add_item(&format!("c{i}"), Label::Iris, ItemKind::Media(Box::new(media)), None);
            m.0.insert(item, Arc::new(g));
            clips.push(item);
        }
        let seq = p.new_sequence("s", SequenceSettings { width: 320, height: 180, frame_rate: rate, ..Default::default() }, 1, 0, None);
        let mut ids = Vec::new();
        for (k, item) in clips.into_iter().enumerate() {
            let it = p.make_track_item(item, TrackKind::Video, rate.tick_of(24 * k as i64), TimeRange::new(Tick::ZERO, rate.tick_of(24)), rate).unwrap();
            ids.push(it.id);
            p.sequence_mut(seq).unwrap().video_tracks[0].items.push(it);
        }
        let opts = RenderOptions::default();
        for (id, reverse) in [("push", false), ("push", true)] {
            p.sequence_mut(seq).unwrap().video_tracks[0].transitions = vec![filmcraft_project::Transition {
                id: filmcraft_project::TransitionId(1),
                effect: filmcraft_project::find_effect(id).unwrap().instance(),
                start: rate.tick_of(18),
                duration: rate.tick_of(12),
                from: ids.first().copied(),
                to: ids.get(1).copied(),
                align: Default::default(),
                reverse,
            }];
            for f in [20, 24, 27] {
                let t = rate.tick_of(f);
                assert!(!filmcraft_render::plan::is_cpu_frame(&p, seq, t, opts, r.caps()), "{id} takes a GPU renderer");
                let before = r.compositor.gpu_transitions;
                let gpu = r.render(&p, seq, t, opts, &m).expect("GPU transition frame");
                assert_eq!(r.compositor.gpu_transitions, before + 1, "{id}: drawn on the GPU");
                assert_parity(&filmcraft_render::render_sequence(&p, seq, t, opts, &m), &gpu, &format!("{id} reverse={reverse} frame {f}"));
            }
        }
        // Film Dissolve has no GPU stage: the whole frame stays a CPU image
        p.sequence_mut(seq).unwrap().video_tracks[0].transitions[0].effect = filmcraft_project::find_effect("film_dissolve").unwrap().instance();
        assert!(filmcraft_render::plan::is_cpu_frame(&p, seq, rate.tick_of(24), opts, r.caps()));
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
        assert!(cpu.px.as_chunks::<4>().0.iter().any(|pixel| pixel[..3].iter().any(|x| *x > 0.1)), "transformed fixture must contain a visible picture");
        assert_parity(&cpu, &gpu, "transformed");
    }

    #[test]
    fn transformed_nested_picture_matches_cpu() {
        let Some(mut renderer) = ExportRenderer::new() else {
            eprintln!("SKIPPED (nested GPU parity): no adapter");
            return;
        };
        for textured in [false, true] {
            let (mut p, inner, mut sources) = project(false);
            let project = Arc::make_mut(&mut p);
            let media = *sources.0.iter().find(|(_, s)| s.info().has_video()).unwrap().0;
            let rate = filmcraft_time::FrameRate::FPS_24;
            if textured {
                let source = GeneratorSource::new(Generator::Demo(filmcraft_media::DemoScene::OceanSunset), 320, 180, rate, Tick(2 * TICKS_PER_SECOND));
                let ItemKind::Media(clip) = &mut project.item_mut(media).unwrap().kind else { panic!("expected generated media") };
                clip.info = source.info().clone();
                clip.media = MediaRef::Generator(source.generator.clone());
                sources.0.insert(media, Arc::new(source));
            }
            let range = TimeRange::new(Tick::ZERO, rate.tick_of(24));
            // Keep the source partly outside the nest: a future flattened plan must still clip it.
            project.sequence_mut(inner).unwrap().video_tracks[0].items[0].effect_mut("motion").unwrap().param_mut("position").unwrap().value =
                filmcraft_project::ParamValue::Vec2(filmcraft_geom::Vec2::new(240.0, 90.0));
            let outer = project.new_sequence("outer", project.sequence(inner).unwrap().settings.clone(), 1, 0, None);
            let mut nest = project.make_track_item(inner, TrackKind::Video, Tick::ZERO, range, rate).unwrap();
            let motion = nest.effect_mut("motion").unwrap();
            motion.param_mut("position").unwrap().value = filmcraft_project::ParamValue::Vec2(filmcraft_geom::Vec2::new(185.0, 75.0));
            motion.param_mut("scale").unwrap().value = filmcraft_project::ParamValue::Float(60.0);
            motion.param_mut("uniform_scale").unwrap().value = filmcraft_project::ParamValue::Bool(false);
            project.sequence_mut(outer).unwrap().video_tracks[0].items.push(nest);
            for (rotation, width) in [(0.0, 60.0), (19.0, 60.0), (0.0, 30.0), (0.0, 120.0)] {
                let motion = Arc::make_mut(&mut p).sequence_mut(outer).unwrap().video_tracks[0].items[0].effect_mut("motion").unwrap();
                motion.param_mut("rotation").unwrap().value = filmcraft_project::ParamValue::Float(rotation);
                motion.param_mut("scale_width").unwrap().value = filmcraft_project::ParamValue::Float(width);
                for scale in [1.0, 0.5] {
                    let opts = RenderOptions { scale, ..Default::default() };
                    let cpu = filmcraft_render::render_sequence(&p, outer, Tick::ZERO, opts, &sources);
                    assert!(cpu.px.as_chunks::<4>().0.iter().any(|pixel| pixel[..3].iter().any(|x| *x > 0.1)), "fixture must contain a visible picture");
                    let gpu = renderer.render(&p, outer, Tick::ZERO, opts, &sources).expect("GPU rendered the nest");
                    assert_parity(&cpu, &gpu, "transformed nest");
                }
            }
        }
    }

    #[test]
    fn gpu_render_impls_frame_renderer() {
        // dev-dependency: the same wrapper the app and CLI register.
        struct R(ExportRenderer);
        impl FrameRenderer for R {
            fn render(&mut self, project: &Project, seq: ItemId, t: Tick, opts: RenderOptions, sources: &dyn SourceProvider) -> Option<Image> {
                self.0.render(project, seq, t, opts, sources)
            }

            fn caps(&self) -> DeviceCaps {
                self.0.caps()
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
