//! Reproducible S1–S7 GPU/CPU video pipeline benchmark harness for FilmCraft.
//!
//! Scenarios:
//! - S1: 1 clip 1080p H.264
//! - S2: 1 clip 4K HEVC 10-bit
//! - S3: 1 clip 4K AV1
//! - S4: 10 overlapping 1080p layers, mixed codecs (H.264, HEVC, AV1, ProRes, DNxHR), opacity + transform
//! - S5: 4 overlapping 4K layers + GPU effects + transitions (Push, Cross Dissolve)
//! - S6: Aggressive scrub/seek (random jumps + drag-stop settle) across 4K HEVC, 4K AV1, S4, S5
//! - S7: Export 4K 60s -> H.264 / HEVC / AV1

#![allow(dead_code)]

#[path = "bench/fixtures.rs"]
mod fixtures;
#[path = "bench/playback.rs"]
mod playback;

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use filmcraft_engine::Session;
use filmcraft_frame::transfers;
use filmcraft_gpu::{GpuCompositor, adapter, export_renderer};
use filmcraft_project::{
    ItemId, ParamValue, Project, SequenceSettings, TrackKind, Transition, TransitionAlign, TransitionId, find_effect, resolve_auto_points,
};
use filmcraft_time::{FrameRate, Tick, TimeRange};
use filmcraft_ui_egui::frames::{FrameKey, FrameServer, PREROLL_TIMEOUT_S, PlaybackMeter, Target, process_cpu_time};
use serde_json::{Value, json};

use crate::fixtures::fixture;
use crate::playback::{Bench, build_sequence, build_sequence_fx, import, ms, pct};

const ALL_CASES: &[&str] = &["s1", "s2", "s3", "s4", "s5", "s6_4k_hevc", "s6_4k_av1", "s6_s4_10layers", "s6_s5_4k_fx", "s7_h264", "s7_hevc", "s7_av1"];

#[derive(Clone, Copy, Default)]
struct GpuSample {
    sm: f64,
    mem: f64,
    enc: f64,
    dec: f64,
    fb_mb: f64,
}

#[derive(Clone, Copy, Default)]
struct GpuSummary {
    samples: usize,
    sm_mean: f64,
    sm_max: f64,
    mem_mean: f64,
    enc_mean: f64,
    enc_max: f64,
    dec_mean: f64,
    dec_max: f64,
    vram_mean_mb: f64,
    vram_max_mb: f64,
}

struct GpuMonitor {
    child: Option<Child>,
    reader: Option<JoinHandle<Vec<GpuSample>>>,
}

impl GpuMonitor {
    fn start() -> Self {
        let Ok(mut child) = Command::new("nvidia-smi").args(["dmon", "-s", "um", "-d", "1"]).stdout(Stdio::piped()).stderr(Stdio::null()).spawn() else {
            return Self { child: None, reader: None };
        };
        let stdout = child.stdout.take();
        let reader = stdout.map(|out| {
            std::thread::spawn(move || {
                let mut out_samples = Vec::new();
                let r = BufReader::new(out);
                for line in r.lines().map_while(Result::ok) {
                    let t = line.trim();
                    if t.is_empty() || t.starts_with('#') {
                        continue;
                    }
                    let cols: Vec<&str> = t.split_whitespace().collect();
                    if cols.len() >= 8 {
                        let parse = |idx: usize| -> f64 { cols.get(idx).and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0) };
                        out_samples.push(GpuSample { sm: parse(1), mem: parse(2), enc: parse(3), dec: parse(4), fb_mb: parse(7) });
                    }
                }
                out_samples
            })
        });
        Self { child: Some(child), reader }
    }

    fn stop(mut self) -> GpuSummary {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let samples = self.reader.take().and_then(|h| h.join().ok()).unwrap_or_default();
        if samples.is_empty() {
            return GpuSummary::default();
        }
        let n = samples.len() as f64;
        let max_of = |f: fn(&GpuSample) -> f64| samples.iter().map(f).fold(0.0, f64::max);
        let mean_of = |f: fn(&GpuSample) -> f64| samples.iter().map(f).sum::<f64>() / n;
        GpuSummary {
            samples: samples.len(),
            sm_mean: mean_of(|s| s.sm),
            sm_max: max_of(|s| s.sm),
            mem_mean: mean_of(|s| s.mem),
            enc_mean: mean_of(|s| s.enc),
            enc_max: max_of(|s| s.enc),
            dec_mean: mean_of(|s| s.dec),
            dec_max: max_of(|s| s.dec),
            vram_mean_mb: mean_of(|s| s.fb_mb),
            vram_max_mb: max_of(|s| s.fb_mb),
        }
    }
}

fn gpu_summary_json(g: GpuSummary) -> Value {
    json!({
        "samples": g.samples,
        "sm_mean_pct": g.sm_mean,
        "sm_max_pct": g.sm_max,
        "mem_mean_pct": g.mem_mean,
        "enc_mean_pct": g.enc_mean,
        "enc_max_pct": g.enc_max,
        "dec_mean_pct": g.dec_mean,
        "dec_max_pct": g.dec_max,
        "vram_mean_mb": g.vram_mean_mb,
        "vram_max_mb": g.vram_max_mb,
    })
}

fn process_ram_mb() -> (f64, f64, f64) {
    let pid = std::process::id();
    let script = format!("$p=Get-Process -Id {pid}; Write-Output \"$($p.WorkingSet64),$($p.PeakWorkingSet64),$($p.PrivateMemorySize64)\"");
    let Ok(out) = Command::new("powershell").args(["-NoProfile", "-Command", &script]).output() else {
        return (0.0, 0.0, 0.0);
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut parts = text.trim().split(',');
    let parse = |p: Option<&str>| p.and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0) / (1024.0 * 1024.0);
    (parse(parts.next()), parse(parts.next()), parse(parts.next()))
}

fn gpu_export_frame_renderer_factory() -> Option<Box<dyn filmcraft_export::FrameRenderer>> {
    struct R(filmcraft_gpu::ExportRenderer);
    impl filmcraft_export::FrameRenderer for R {
        fn render(
            &mut self,
            project: &Project,
            seq: ItemId,
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
    }
    filmcraft_gpu::ExportRenderer::new().map(|r| Box::new(R(r)) as Box<dyn filmcraft_export::FrameRenderer>)
}

struct PreviewDisplay {
    gpu: Option<(GpuCompositor, eframe::wgpu::Device, String, String)>,
    last: Option<FrameKey>,
    present_ms: Vec<f64>,
    render_path: &'static str,
}

impl PreviewDisplay {
    fn new(use_gpu: bool) -> Self {
        if !use_gpu {
            return Self { gpu: None, last: None, present_ms: Vec::new(), render_path: "cpu" };
        }
        let instance = eframe::wgpu::Instance::new(eframe::wgpu::InstanceDescriptor::new_without_display_handle().with_env());
        let mut adapters = pollster::block_on(instance.enumerate_adapters(eframe::wgpu::Backends::all()));
        if let Some(named) = eframe::wgpu::Backends::from_env()
            && adapters.iter().any(|a| named.contains(a.get_info().backend.into()))
        {
            adapters.retain(|a| named.contains(a.get_info().backend.into()));
        }
        let pref = eframe::wgpu::PowerPreference::from_env().unwrap_or(eframe::wgpu::PowerPreference::HighPerformance);
        let Ok(chosen) = adapter::pick(&adapters, None, pref) else {
            return Self { gpu: None, last: None, present_ms: Vec::new(), render_path: "cpu" };
        };
        let info = chosen.get_info();
        let adapter_name = info.name.clone();
        let backend_name = format!("{:?}", info.backend);
        let desc = eframe::wgpu::DeviceDescriptor { required_features: filmcraft_gpu::requested_features(&chosen), ..Default::default() };
        let Ok((device, queue)) = pollster::block_on(chosen.request_device(&desc)) else {
            return Self { gpu: None, last: None, present_ms: Vec::new(), render_path: "cpu" };
        };
        let compositor = GpuCompositor::new(&device, &queue);
        Self { gpu: Some((compositor, device, adapter_name, backend_name)), last: None, present_ms: Vec::new(), render_path: "gpu" }
    }

    fn r16_unorm(&self) -> bool {
        self.gpu.as_ref().is_some_and(|(c, _, _, _)| c.supports_r16_unorm())
    }

    fn adapter_info(&self) -> (String, String) {
        match &self.gpu {
            Some((_, _, name, backend)) => (name.clone(), backend.clone()),
            None => ("CPU".to_string(), "CPU".to_string()),
        }
    }

    fn target(&self, seq: ItemId) -> Target {
        match &self.gpu {
            Some((c, _, _, _)) => Target::SequencePlan(seq, c.caps()),
            None => Target::Sequence(seq),
        }
    }

    fn refresh(&mut self, server: &FrameServer, key: FrameKey) -> bool {
        let t0 = Instant::now();
        let exact;
        let mut presented = false;
        if let Some((comp, dev, _, _)) = self.gpu.as_mut() {
            let plan = server.get_plan(&key).map(|p| (key, p));
            exact = plan.is_some();
            if let Some((k, plan)) = plan.or_else(|| server.nearest_plan(key, 6))
                && self.last != Some(k)
            {
                let _ = comp.composite_prepared(&plan.plan, Some(&plan.prepared));
                let _ = dev.poll(eframe::wgpu::PollType::wait_indefinitely());
                self.last = Some(k);
                presented = true;
            }
        } else {
            let img = server.get(&key).map(|i| (key, i));
            exact = img.is_some();
            let near = img.or_else(|| server.nearest(key.target, key.frame, key.size, key.revision, 6).map(|i| (FrameKey { frame: key.frame - 1, ..key }, i)));
            if let Some((k, img)) = near
                && self.last != Some(k)
            {
                let ci = egui::ColorImage::from_rgba_unmultiplied([img.w, img.h], &img.px);
                std::hint::black_box(&ci);
                self.last = Some(k);
                presented = true;
            }
        }
        if presented {
            self.present_ms.push(ms(t0.elapsed()));
        }
        exact
    }
}

/// Create a hardlink (or copy fallback) in `fixtures_dir()` so each S4 track owns a distinct clip/decoder.
fn distinct_clip(base_fixture: &str, alias_name: &str) -> Option<PathBuf> {
    let src = fixture(base_fixture)?;
    let dst = src.parent()?.join(alias_name);
    if !dst.exists() && std::fs::hard_link(&src, &dst).is_err() {
        std::fs::copy(&src, &dst).ok()?;
    }
    Some(dst)
}

fn build_s4_session() -> Option<(Session, ItemId)> {
    let mut s = Session::default();
    let specs: &[(&str, &str, f64, Option<(f64, f64)>, f64, f64)] = &[
        ("a1080.mp4", "s4_l01_h264_a.mp4", 100.0, None, 0.0, 100.0),
        ("hevc_1080.mp4", "s4_l02_hevc_a.mp4", 45.0, Some((360.0, 220.0)), 0.0, 85.0),
        ("av1_1080.mp4", "s4_l03_av1_a.mp4", 45.0, Some((1560.0, 220.0)), 4.0, 85.0),
        ("prores_1080.mov", "s4_l04_prores_a.mov", 45.0, Some((360.0, 860.0)), -4.0, 85.0),
        ("dnxhr_1080.mov", "s4_l05_dnxhr_a.mov", 45.0, Some((1560.0, 860.0)), 6.0, 85.0),
        ("b1080.mp4", "s4_l06_h264_b.mp4", 35.0, Some((960.0, 260.0)), 0.0, 75.0),
        ("c1080.mp4", "s4_l07_h264_c.mp4", 35.0, Some((960.0, 820.0)), 8.0, 75.0),
        ("hevc_1080.mp4", "s4_l08_hevc_b.mp4", 30.0, Some((540.0, 540.0)), -8.0, 65.0),
        ("av1_1080.mp4", "s4_l09_av1_b.mp4", 30.0, Some((1380.0, 540.0)), 10.0, 65.0),
        ("prores_1080.mov", "s4_l10_prores_b.mov", 25.0, Some((960.0, 540.0)), 15.0, 55.0),
    ];
    let mut layers = Vec::with_capacity(specs.len());
    for &(base, alias, scale, pos, rot, op) in specs {
        let p = distinct_clip(base, alias)?;
        let item = import(&mut s, &p);
        layers.push((item, scale, pos, rot, op));
    }
    let seq = build_sequence(&mut s, 1920, 1080, &layers, &[], 10.0);
    Some((s, seq))
}

fn add_track_transition(p: &mut Project, seq: ItemId, track: usize, effect_id: &str, span_f: (i64, i64), pair: (usize, usize), tid: u64) {
    let rate = FrameRate::FPS_23_976;
    let trk = &mut p.sequence_mut(seq).expect("seq").video_tracks[track];
    let id_from = trk.items.get(pair.0).map(|i| i.id);
    let id_to = trk.items.get(pair.1).map(|i| i.id);
    if let Some(def) = find_effect(effect_id) {
        trk.transitions.push(Transition {
            id: TransitionId(tid),
            effect: def.instance(),
            start: rate.tick_of(span_f.0),
            duration: rate.tick_of(span_f.1),
            from: id_from,
            to: id_to,
            align: TransitionAlign::CenterAtCut,
            reverse: false,
        });
    }
}

fn build_s5_session() -> Option<(Session, ItemId)> {
    let mut s = Session::default();
    let a4k = import(&mut s, &fixture("a2160.mp4")?);
    let hevc4k = import(&mut s, &fixture("hevc2160.mp4")?);
    let hevc10_4k = import(&mut s, &fixture("hevc10_2160.mp4")?);
    let av1_4k = import(&mut s, &fixture("av12160.mp4")?);
    let layers = [
        (a4k, 100.0, None, 0.0, 100.0),
        (hevc10_4k, 50.0, Some((960.0, 540.0)), 0.0, 80.0),
        (av1_4k, 40.0, Some((2880.0, 540.0)), 8.0, 75.0),
        (hevc4k, 35.0, Some((2880.0, 1620.0)), -6.0, 70.0),
    ];
    let per_layer: [&[&str]; 4] = [&["lumetri"], &["gaussian_blur", "brightness_contrast"], &["sharpen"], &["vignette"]];
    let seq = build_sequence_fx(&mut s, 3840, 2160, &layers, &per_layer, 10.0);
    // Split V1 at 4s to place a 1.5s GPU Push transition (`push`) and V2 at 6s for Cross Dissolve (`cross_dissolve`).
    let rate = FrameRate::FPS_23_976;
    s.edit("s5 transitions", |p, _| {
        let cut1 = rate.tick_of(96);
        let cut2 = rate.tick_of(144);
        // Replace V1 with two 5s clips at [0..cut1] and [cut1..10s]
        let end_10s = rate.snap(Tick::from_seconds_f64(10.0));
        let mut v1_a = p.make_track_item(a4k, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, cut1), rate).expect("v1a");
        if let Some(def) = find_effect("lumetri") {
            let mut e = def.instance();
            if let Some(x) = e.params.get_mut("temperature") {
                x.value = ParamValue::Float(18.0);
            }
            v1_a.effects.insert(0, e);
        }
        let mut v1_b = p.make_track_item(hevc4k, TrackKind::Video, cut1, TimeRange::new(Tick::ZERO, end_10s - cut1), rate).expect("v1b");
        if let Some(def) = find_effect("lumetri") {
            let mut e = def.instance();
            if let Some(x) = e.params.get_mut("contrast") {
                x.value = ParamValue::Float(20.0);
            }
            v1_b.effects.insert(0, e);
        }
        for it in [&mut v1_a, &mut v1_b] {
            for e in &mut it.effects {
                resolve_auto_points(e, (3840, 2160), (3840, 2160));
            }
        }
        p.sequence_mut(seq).expect("seq").video_tracks[0].items = vec![v1_a, v1_b];
        add_track_transition(p, seq, 0, "push", (80, 32), (0, 1), 1);

        // Split V2 at 6s (`cut2`) and add a `cross_dissolve`
        let mut v2_a = p.make_track_item(hevc10_4k, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, cut2), rate).expect("v2a");
        let mut v2_b = p.make_track_item(a4k, TrackKind::Video, cut2, TimeRange::new(Tick::ZERO, end_10s - cut2), rate).expect("v2b");
        for it in [&mut v2_a, &mut v2_b] {
            for e in &mut it.effects {
                resolve_auto_points(e, (3840, 2160), (3840, 2160));
            }
            if let Some(m) = it.effect_mut("motion") {
                if let Some(x) = m.params.get_mut("scale") {
                    x.value = ParamValue::Float(50.0);
                }
                if let Some(x) = m.params.get_mut("position") {
                    x.value = ParamValue::Vec2(filmcraft_geom::Vec2::new(960.0, 540.0));
                }
            }
            if let Some(o) = it.effect_mut("opacity").and_then(|o| o.params.get_mut("opacity")) {
                o.value = ParamValue::Float(80.0);
            }
        }
        p.sequence_mut(seq).expect("seq").video_tracks[1].items = vec![v2_a, v2_b];
        add_track_transition(p, seq, 1, "cross_dissolve", (132, 24), (0, 1), 2);
        Ok(())
    })
    .ok()?;
    Some((s, seq))
}

fn build_case_session(case: &str) -> Option<(Session, ItemId)> {
    let mut s = Session::default();
    let seq = match case {
        "s1" => {
            let a = import(&mut s, &fixture("a1080.mp4")?);
            build_sequence(&mut s, 1920, 1080, &[(a, 100.0, None, 0.0, 100.0)], &[], 12.0)
        }
        "s2" => {
            let a = import(&mut s, &fixture("hevc10_2160.mp4")?);
            build_sequence(&mut s, 3840, 2160, &[(a, 100.0, None, 0.0, 100.0)], &[], 10.0)
        }
        "s3" | "s6_4k_av1" => {
            let a = import(&mut s, &fixture("av12160.mp4")?);
            build_sequence(&mut s, 3840, 2160, &[(a, 100.0, None, 0.0, 100.0)], &[], 10.0)
        }
        "s6_4k_hevc" => {
            let a = import(&mut s, &fixture("hevc2160.mp4")?);
            build_sequence(&mut s, 3840, 2160, &[(a, 100.0, None, 0.0, 100.0)], &[], 10.0)
        }
        "s4" | "s6_s4_10layers" => return build_s4_session(),
        "s5" | "s6_s5_4k_fx" => return build_s5_session(),
        _ => return None,
    };
    Some((s, seq))
}

fn measure_preview(case: &str, s: &Session, seq: ItemId, use_gpu: bool, seconds: f64) -> Value {
    let q = s.project.sequence(seq).expect("seq");
    let rate = q.settings.frame_rate;
    let project = s.project.clone();
    let mut display = PreviewDisplay::new(use_gpu);
    let (adapter_name, backend_name) = display.adapter_info();
    let workers = FrameServer::default_workers();
    let server = FrameServer::new(s.media.clone(), s.services.clone(), s.previews.clone(), workers);
    server.set_r16_unorm(display.r16_unorm());
    server.set_profiling(true);
    let target = display.target(seq);
    let scale = 1.0f32;
    let size = 1000u32;
    let period = Duration::from_secs_f64(1.0 / 60.0);
    let last_frame = (rate.frame_at(q.duration()) - 1).max(0);
    let end = Tick::from_seconds_f64(seconds).min(q.duration());

    let gpu_mon = GpuMonitor::start();
    let tx0 = transfers::snapshot();
    let hw0 = filmcraft_codecs::hw::hw_stats();
    let gop0 = filmcraft_codecs::gop_stats();
    let cpu0 = process_cpu_time().unwrap_or_default();

    // TTFF + Preroll
    let t_ttff = Instant::now();
    let mut ttff_ms = None;
    loop {
        let key = FrameKey { target, frame: 0, size, revision: s.revision, draft: false };
        server.schedule_playback(key, rate, scale, &project, 1.0, true);
        let exact = display.refresh(&server, key);
        if exact && ttff_ms.is_none() {
            ttff_ms = Some(ms(t_ttff.elapsed()));
        }
        if server.preroll_ready(key, 1.0, last_frame) || t_ttff.elapsed().as_secs_f64() >= PREROLL_TIMEOUT_S {
            if ttff_ms.is_none() {
                let key = FrameKey { target, frame: 0, size, revision: s.revision, draft: false };
                if display.refresh(&server, key) {
                    ttff_ms = Some(ms(t_ttff.elapsed()));
                }
            }
            break;
        }
        std::thread::sleep(period);
    }
    let preroll_ms = ms(t_ttff.elapsed());

    // Timed playback loop + A/V sync drift sampling
    let mut meter = PlaybackMeter::default();
    meter.start(1.0);
    let anchor = Instant::now();
    let mut next = anchor;
    let mut av_drift_ms = Vec::new();
    let mut max_frame_lag: i64 = 0;
    loop {
        let now = Instant::now();
        let t = Tick::from_seconds_f64((now - anchor).as_secs_f64());
        if t >= end {
            break;
        }
        let frame = rate.frame_at(t);
        let key = FrameKey { target, frame, size, revision: s.revision, draft: false };
        server.schedule_playback(key, rate, scale, &project, 1.0, false);
        let exact = display.refresh(&server, key);
        if ttff_ms.is_none() && display.last.is_some() {
            ttff_ms = Some(ms(t_ttff.elapsed()));
        }
        meter.refresh(frame, exact);
        if let Some(shown_k) = display.last {
            let lag = (frame - shown_k.frame).max(0);
            max_frame_lag = max_frame_lag.max(lag);
            let drift = ((t - rate.tick_of(shown_k.frame)).seconds() * 1000.0).abs();
            av_drift_ms.push(drift);
        }
        next += period;
        let after = Instant::now();
        if next > after {
            std::thread::sleep(next - after);
        } else {
            next = after;
        }
    }
    meter.finish();
    let wall_s = anchor.elapsed().as_secs_f64().max(0.001);
    server.stop_prefetch();
    std::thread::sleep(Duration::from_millis(50));

    let play_cpu_s = process_cpu_time().unwrap_or_default().saturating_sub(cpu0).as_secs_f64();
    let tx_play = transfers::snapshot() - tx0;
    let hw1 = filmcraft_codecs::hw::hw_stats();
    let gop_play = filmcraft_codecs::gop_stats() - gop0;

    // 12 random seeks on a fresh session after releasing playback compositor/caches so VRAM does not double.
    drop((display, server));
    let bench = Bench { server: FrameServer::new(s.media.clone(), s.services.clone(), s.previews.clone(), workers), refresh_hz: 60.0, seconds, draft: false };
    let mut disp_seek = PreviewDisplay::new(use_gpu);
    bench.server.set_r16_unorm(disp_seek.r16_unorm());
    let mut seek_lat_ms = Vec::new();
    let total_frames = rate.frame_at(q.duration()).max(1);
    let mut rng = 0x517c_c1b7_2722_0a95u64;
    for _ in 0..12 {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        let f = (rng % total_frames as u64) as i64;
        let key = FrameKey { target: disp_seek.target(seq), frame: f, size, revision: s.revision, draft: false };
        let t0 = Instant::now();
        loop {
            bench.server.request(key, rate.tick_of(f), scale, &project, 0);
            if disp_seek.refresh(&bench.server, key) {
                seek_lat_ms.push(ms(t0.elapsed()));
                break;
            }
            if t0.elapsed() >= Duration::from_secs(5) {
                seek_lat_ms.push(5000.0);
                break;
            }
            std::thread::sleep(Duration::from_millis(4));
        }
    }
    bench.server.retain_queue(|_| false);

    let gpu_sum = gpu_mon.stop();
    let (ram_ws_mb, ram_peak_mb, ram_priv_mb) = process_ram_mb();
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as f64;
    let shown = meter.shown;
    let dropped = meter.dropped;
    let total = (shown + dropped).max(1) as f64;
    let effective_fps = shown as f64 / wall_s;
    let target_fps = rate.as_f64();
    let cpu_cores_used = play_cpu_s / wall_s;
    let cpu_pct = (cpu_cores_used / cores) * 100.0;

    json!({
        "case": case,
        "kind": "preview",
        "adapter": adapter_name,
        "backend": backend_name,
        "render_path": display.render_path,
        "resolution": format!("{}x{}", q.settings.width, q.settings.height),
        "target_fps": target_fps,
        "effective_fps": effective_fps,
        "shown": shown,
        "dropped": dropped,
        "drop_pct": 100.0 * dropped as f64 / total,
        "ttff_ms": ttff_ms,
        "preroll_ms": preroll_ms,
        "seek_p50_ms": pct(&seek_lat_ms, 0.5),
        "seek_p95_ms": pct(&seek_lat_ms, 0.95),
        "ui_present_p50_ms": pct(&display.present_ms, 0.5),
        "ui_present_p95_ms": pct(&display.present_ms, 0.95),
        "av_drift_ms_p50": pct(&av_drift_ms, 0.5),
        "av_drift_ms_p95": pct(&av_drift_ms, 0.95),
        "av_drift_ms_max": pct(&av_drift_ms, 1.0),
        "av_lag_frames_max": max_frame_lag,
        "cpu_pct": cpu_pct,
        "cpu_cores_used": cpu_cores_used,
        "ram_ws_mb": ram_ws_mb,
        "ram_peak_mb": ram_peak_mb,
        "ram_priv_mb": ram_priv_mb,
        "gpu": gpu_summary_json(gpu_sum),
        "transfers_per_shown_frame": {
            "total_ops": tx_play.preview_ops() as f64 / shown.max(1) as f64,
            "total_mb": tx_play.preview_bytes() as f64 / (1e6 * shown.max(1) as f64),
            "decode_readback_ops": tx_play.decode_readback_ops as f64 / shown.max(1) as f64,
            "decode_readback_mb": tx_play.decode_readback_bytes as f64 / (1e6 * shown.max(1) as f64),
            "compositor_upload_ops": tx_play.compositor_upload_ops as f64 / shown.max(1) as f64,
            "compositor_upload_mb": tx_play.compositor_upload_bytes as f64 / (1e6 * shown.max(1) as f64),
        },
        "hw_decode": {
            "frames": hw1.frames.saturating_sub(hw0.frames),
            "sessions": hw1.sessions.saturating_sub(hw0.sessions),
            "declined": hw1.declined.saturating_sub(hw0.declined),
            "fallbacks": hw1.fallbacks.saturating_sub(hw0.fallbacks),
        },
        "gop": {
            "decoded": gop_play.decoded,
            "seeks": gop_play.seeks,
            "evicted": gop_play.evicted,
            "hits": gop_play.hits,
            "misses": gop_play.misses,
        },
    })
}

fn measure_scrub(case: &str, s: &Session, seq: ItemId, use_gpu: bool) -> Value {
    let workers = FrameServer::default_workers();
    let server = FrameServer::new(s.media.clone(), s.services.clone(), s.previews.clone(), workers);
    server.set_profiling(true);
    let mut display = PreviewDisplay::new(use_gpu);
    server.set_r16_unorm(display.r16_unorm());
    let (adapter_name, backend_name) = display.adapter_info();
    let q = s.project.sequence(seq).expect("seq");
    let rate = q.settings.frame_rate;
    let total_frames = rate.frame_at(q.duration()).max(1);
    let project = s.project.clone();
    let target = display.target(seq);
    let size = 1000u32;
    let scale = 1.0f32;
    let period = Duration::from_millis(16);
    let timeout = Duration::from_secs(6);

    let gpu_mon = GpuMonitor::start();
    let tx0 = transfers::snapshot();
    let cpu0 = process_cpu_time().unwrap_or_default();
    let t_wall0 = Instant::now();

    let mut rng = 0x9e37_79b9_7f4a_7c15u64;
    let mut next_rand = || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    let mut seek_ms = Vec::new();
    let mut settle_ms = Vec::new();
    let mut timeouts = 0usize;
    let mut drag_steps = 0usize;
    let mut drag_shown = 0usize;

    // TTFF = very first cold seek
    for i in 0..24 {
        let f = (next_rand() % total_frames as u64) as i64;
        let key = FrameKey { target, frame: f, size, revision: s.revision, draft: false };
        let t0 = Instant::now();
        loop {
            server.request(key, rate.tick_of(f), scale, &project, 0);
            if display.refresh(&server, key) {
                seek_ms.push(ms(t0.elapsed()));
                break;
            }
            if t0.elapsed() >= timeout {
                timeouts += 1;
                break;
            }
            std::thread::sleep(Duration::from_millis(4));
        }
        let _ = i;
    }

    for _ in 0..8 {
        let drag_frames = 12i64;
        let from = (next_rand() % (total_frames - drag_frames).max(1) as u64) as i64;
        for f in from..from + drag_frames {
            let key = FrameKey { target, frame: f.min(total_frames - 1), size, revision: s.revision, draft: false };
            server.request(key, rate.tick_of(key.frame), scale, &project, 0);
            if display.refresh(&server, key) {
                drag_shown += 1;
            }
            drag_steps += 1;
            std::thread::sleep(period);
        }
        let key = FrameKey { target, frame: (from + drag_frames).min(total_frames - 1), size, revision: s.revision, draft: false };
        let t0 = Instant::now();
        loop {
            server.request(key, rate.tick_of(key.frame), scale, &project, 0);
            if display.refresh(&server, key) {
                settle_ms.push(ms(t0.elapsed()));
                break;
            }
            if t0.elapsed() >= timeout {
                timeouts += 1;
                break;
            }
            std::thread::sleep(Duration::from_millis(4));
        }
    }

    server.retain_queue(|_| false);
    std::thread::sleep(Duration::from_millis(40));
    let wall_s = t_wall0.elapsed().as_secs_f64().max(0.001);
    let cpu_s = process_cpu_time().unwrap_or_default().saturating_sub(cpu0).as_secs_f64();
    let tx = transfers::snapshot() - tx0;
    let gpu_sum = gpu_mon.stop();
    let (ram_ws_mb, ram_peak_mb, _) = process_ram_mb();
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as f64;
    let cpu_cores_used = cpu_s / wall_s;
    let seeks_done = seek_ms.len().max(1) as f64;

    json!({
        "case": case,
        "kind": "scrub",
        "adapter": adapter_name,
        "backend": backend_name,
        "resolution": format!("{}x{}", q.settings.width, q.settings.height),
        "ttff_ms": seek_ms.first().copied().unwrap_or(f64::NAN),
        "seek_p50_ms": pct(&seek_ms, 0.5),
        "seek_p95_ms": pct(&seek_ms, 0.95),
        "seek_max_ms": pct(&seek_ms, 1.0),
        "drag_settle_p50_ms": pct(&settle_ms, 0.5),
        "drag_settle_p95_ms": pct(&settle_ms, 0.95),
        "drag_shown_pct": 100.0 * drag_shown as f64 / drag_steps.max(1) as f64,
        "timeouts": timeouts,
        "cpu_pct": (cpu_cores_used / cores) * 100.0,
        "cpu_cores_used": cpu_cores_used,
        "ram_ws_mb": ram_ws_mb,
        "ram_peak_mb": ram_peak_mb,
        "gpu": gpu_summary_json(gpu_sum),
        "transfers_per_seek": {
            "total_ops": tx.preview_ops() as f64 / seeks_done,
            "total_mb": tx.preview_bytes() as f64 / (1e6 * seeks_done),
        },
    })
}

fn measure_export_60s(case: &str, format: &str, use_gpu: bool, hw_off: bool) -> Value {
    let Some(v_path) = fixture("a2160.mp4") else {
        return json!({"case": case, "kind": "export", "status": "skipped", "error": "missing a2160.mp4"});
    };
    let Some(a_path) = fixture("a1080.mp4") else {
        return json!({"case": case, "kind": "export", "status": "skipped", "error": "missing a1080.mp4"});
    };
    filmcraft_engine::export::register_frame_renderer(gpu_export_frame_renderer_factory);
    let mut s = Session::default();
    let v_item = import(&mut s, &v_path);
    let a_item = import(&mut s, &a_path);
    let rate = FrameRate::FPS_23_976;
    let target_secs = 60.0;
    let chunk_frames = 240i64; // 10s chunks of a2160.mp4 chained 6 times = 1,440 frames (~60.06s)
    let seq = s
        .edit("s7 60s 4k sequence", |p: &mut Project, st| {
            let settings = SequenceSettings { width: 3840, height: 2160, frame_rate: rate, ..SequenceSettings::default() };
            let seq = p.new_sequence("Bench60s", settings, 1, 1, None);
            for k in 0..6 {
                let start = rate.tick_of(k * chunk_frames);
                let dur = rate.tick_of(chunk_frames);
                let mut v = p.make_track_item(v_item, TrackKind::Video, start, TimeRange::new(Tick::ZERO, dur), rate).expect("v");
                for e in &mut v.effects {
                    resolve_auto_points(e, (3840, 2160), (3840, 2160));
                }
                p.sequence_mut(seq).expect("seq").video_tracks[0].items.push(v);
            }
            let a_chunk = rate.tick_of(480); // 20s from a1080.mp4 x3 = 60s
            for k in 0..3 {
                let start = rate.tick_of(k * 480);
                let a = p.make_track_item(a_item, TrackKind::Audio, start, TimeRange::new(Tick::ZERO, a_chunk), rate).expect("a");
                p.sequence_mut(seq).expect("seq").audio_tracks[0].items.push(a);
            }
            st.active_sequence = Some(seq);
            st.open_sequences = vec![seq];
            st.playheads.insert(seq, Tick::ZERO);
            Ok(seq)
        })
        .expect("s7 sequence");

    let frames = s.project.sequence(seq).map(|q| q.settings.frame_rate.frame_at(q.duration())).unwrap_or(0);
    let dir = tempfile::Builder::new().prefix("filmcraft-s7-").tempdir().expect("tmp dir");
    let out = dir.path().join(format!("{case}.mp4"));

    filmcraft_export::reset_export_lock_wait();
    export_renderer::timing::reset();
    let stages0: Vec<_> = filmcraft_export::stage_times();
    let tx0 = transfers::snapshot();
    let hw0 = filmcraft_engine::export::hw_encode_stats();
    let gpu0 = filmcraft_engine::export::gpu_render_stats();
    let gpu_mon = GpuMonitor::start();
    let cpu0 = process_cpu_time().unwrap_or_default();
    let t0 = Instant::now();

    let hw_arg = if hw_off { "off" } else { "auto" };
    let gpu_arg = if use_gpu && !hw_off { "auto" } else { "off" };
    let exec_res = s.execute(
        "file.exportMedia",
        json!({
            "path": out.to_string_lossy(),
            "format": format,
            "hardwareEncoding": hw_arg,
            "gpuRendering": gpu_arg,
            "wait": true
        }),
    );
    let wall_s = t0.elapsed().as_secs_f64().max(0.001);
    let cpu_s = process_cpu_time().unwrap_or_default().saturating_sub(cpu0).as_secs_f64();
    let gpu_sum = gpu_mon.stop();
    let (ram_ws_mb, ram_peak_mb, _) = process_ram_mb();
    let tx = transfers::snapshot() - tx0;
    let hw = filmcraft_engine::export::hw_encode_stats();
    let gpu = filmcraft_engine::export::gpu_render_stats();
    let gpu_timings = export_renderer::timing::get();
    let lock_wait = filmcraft_export::export_lock_wait();
    let stages1 = filmcraft_export::stage_times();
    let mut stage_ms = serde_json::Map::new();
    for ((st, a), (_, b)) in stages0.iter().zip(stages1.iter()) {
        stage_ms.insert(st.name().to_string(), json!(b.saturating_sub(*a) as f64 / 1e6));
    }

    let job_err = s.jobs.last().and_then(|j| j.progress.error.lock().ok().and_then(|g| g.clone()));
    let err = exec_res.err().map(|e| e.to_string()).or(job_err);
    let file_mb = std::fs::metadata(&out).map(|m| m.len() as f64 / 1e6).unwrap_or(0.0);

    let av_drift_ms = if err.is_none() && out.exists() {
        let _opened = filmcraft_media::open_path(&out, &filmcraft_codecs::openers()).is_ok();
        Command::new("ffprobe")
            .args(["-v", "quiet", "-print_format", "json", "-show_streams"])
            .arg(&out)
            .output()
            .ok()
            .and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok())
            .and_then(|v| {
                let streams = v["streams"].as_array()?;
                let vid = streams.iter().find(|s| s["codec_type"] == "video")?;
                let aud = streams.iter().find(|s| s["codec_type"] == "audio")?;
                let parse_f = |s: &Value, k: &str| s[k].as_str().and_then(|x| x.parse::<f64>().ok()).unwrap_or(0.0);
                let v_end = parse_f(vid, "start_time") + parse_f(vid, "duration");
                let a_end = parse_f(aud, "start_time") + parse_f(aud, "duration");
                let start_diff = (parse_f(vid, "start_time") - parse_f(aud, "start_time")).abs();
                Some(start_diff.max((v_end - a_end).abs()) * 1000.0)
            })
    } else {
        None
    };

    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as f64;
    let cpu_cores_used = cpu_s / wall_s;
    let export_fps = if err.is_none() { frames as f64 / wall_s } else { 0.0 };
    let realtime_x = if err.is_none() { export_fps / rate.as_f64() } else { 0.0 };
    let f_div = frames.max(1) as f64;

    json!({
        "case": case,
        "kind": "export",
        "format": format,
        "status": if err.is_some() { "failed" } else { "ok" },
        "error": err,
        "resolution": "3840x2160",
        "duration_s": target_secs,
        "frames": frames,
        "wall_s": wall_s,
        "export_fps": export_fps,
        "realtime_x": realtime_x,
        "mbytes": file_mb,
        "av_drift_ms": av_drift_ms,
        "hw_frames": hw.frames.saturating_sub(hw0.frames),
        "gpu_frames": gpu.frames.saturating_sub(gpu0.frames),
        "gpu_fallbacks": gpu.fallbacks.saturating_sub(gpu0.fallbacks),
        "cpu_pct": (cpu_cores_used / cores) * 100.0,
        "cpu_cores_used": cpu_cores_used,
        "ram_ws_mb": ram_ws_mb,
        "ram_peak_mb": ram_peak_mb,
        "gpu": gpu_summary_json(gpu_sum),
        "transfers_per_frame": {
            "total_ops": tx.export_ops() as f64 / f_div,
            "total_mb": tx.export_bytes() as f64 / (1e6 * f_div),
            "decode_readback_ops": tx.decode_readback_ops as f64 / f_div,
            "decode_readback_mb": tx.decode_readback_bytes as f64 / (1e6 * f_div),
            "compositor_upload_ops": tx.compositor_upload_ops as f64 / f_div,
            "compositor_upload_mb": tx.compositor_upload_bytes as f64 / (1e6 * f_div),
            "export_readback_ops": tx.export_readback_ops as f64 / f_div,
            "export_readback_mb": tx.export_readback_bytes as f64 / (1e6 * f_div),
            "encode_upload_ops": tx.encode_upload_ops as f64 / f_div,
            "encode_upload_mb": tx.encode_upload_bytes as f64 / (1e6 * f_div),
        },
        "gpu_renderer_timings_ms": {
            "lock_wait": lock_wait.as_secs_f64() * 1000.0,
            "submit": gpu_timings.submit.as_secs_f64() * 1000.0,
            "map_wait": gpu_timings.map_wait.as_secs_f64() * 1000.0,
            "convert": gpu_timings.convert.as_secs_f64() * 1000.0,
            "frames": gpu_timings.frames,
        },
        "stages_ms": stage_ms,
    })
}

fn run_single(case: &str, use_gpu: bool, hw_off: bool, seconds: f64) -> Value {
    filmcraft_platform::register();
    filmcraft_codecs::hw::set_hardware_decoding(!hw_off);
    match case {
        "s1" | "s2" | "s3" | "s4" | "s5" => match build_case_session(case) {
            Some((s, seq)) => measure_preview(case, &s, seq, use_gpu, seconds),
            None => json!({"case": case, "kind": "preview", "status": "skipped", "error": "fixture unavailable"}),
        },
        "s6_4k_hevc" | "s6_4k_av1" | "s6_s4_10layers" | "s6_s5_4k_fx" => match build_case_session(case) {
            Some((s, seq)) => measure_scrub(case, &s, seq, use_gpu),
            None => json!({"case": case, "kind": "scrub", "status": "skipped", "error": "fixture unavailable"}),
        },
        "s7_h264" => measure_export_60s(case, "h264", use_gpu, hw_off),
        "s7_hevc" => measure_export_60s(case, "hevc", use_gpu, hw_off),
        "s7_av1" => measure_export_60s(case, "av1", use_gpu, hw_off),
        other => json!({"case": other, "status": "error", "error": "unknown case"}),
    }
}

fn fmt_num(v: &Value, ptr: &str, prec: usize) -> String {
    match v.pointer(ptr).and_then(Value::as_f64) {
        Some(x) if x.is_finite() => format!("{x:.prec$}"),
        _ => "–".to_string(),
    }
}

fn to_markdown(doc: &Value) -> String {
    let mut md = String::new();
    let label = doc["label"].as_str().unwrap_or("baseline");
    md.push_str(&format!("# FilmCraft S1–S7 Benchmark (`{label}`)\n\n"));
    let empty = Vec::new();
    let rows = doc["cases"].as_array().unwrap_or(&empty);

    md.push_str("## Preview (S1–S5)\n\n");
    md.push_str("| Case | Res | Eff FPS | Drop % | TTFF ms | Seek p50/p95 ms | UI p95 ms | A/V Drift p95/max ms | CPU % | GPU 3D/Dec % | VRAM Max MB | Peak RAM MB | CPU↔GPU Ops/f | CPU↔GPU MB/f |\n");
    md.push_str("|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n");
    for r in rows.iter().filter(|r| r["kind"] == "preview") {
        md.push_str(&format!(
            "| {} | {} | {} | {} | {} | {}/{} | {} | {}/{} | {} | {}/{} | {} | {} | {} | {} |\n",
            r["case"].as_str().unwrap_or("?"),
            r["resolution"].as_str().unwrap_or("?"),
            fmt_num(r, "/effective_fps", 2),
            fmt_num(r, "/drop_pct", 1),
            fmt_num(r, "/ttff_ms", 1),
            fmt_num(r, "/seek_p50_ms", 1),
            fmt_num(r, "/seek_p95_ms", 1),
            fmt_num(r, "/ui_present_p95_ms", 2),
            fmt_num(r, "/av_drift_ms_p95", 1),
            fmt_num(r, "/av_drift_ms_max", 1),
            fmt_num(r, "/cpu_pct", 1),
            fmt_num(r, "/gpu/sm_mean_pct", 1),
            fmt_num(r, "/gpu/dec_mean_pct", 1),
            fmt_num(r, "/gpu/vram_max_mb", 0),
            fmt_num(r, "/ram_peak_mb", 0),
            fmt_num(r, "/transfers_per_shown_frame/total_ops", 2),
            fmt_num(r, "/transfers_per_shown_frame/total_mb", 2),
        ));
    }

    md.push_str("\n## Scrub / Seek (S6)\n\n");
    md.push_str("| Case | Res | TTFF ms | Seek p50/p95 ms | Drag Settle p50/p95 ms | Drag Shown % | CPU % | GPU 3D/Dec % | VRAM Max MB | Peak RAM MB | CPU↔GPU Ops/seek | CPU↔GPU MB/seek |\n");
    md.push_str("|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n");
    for r in rows.iter().filter(|r| r["kind"] == "scrub") {
        md.push_str(&format!(
            "| {} | {} | {} | {}/{} | {}/{} | {} | {} | {}/{} | {} | {} | {} | {} |\n",
            r["case"].as_str().unwrap_or("?"),
            r["resolution"].as_str().unwrap_or("?"),
            fmt_num(r, "/ttff_ms", 1),
            fmt_num(r, "/seek_p50_ms", 1),
            fmt_num(r, "/seek_p95_ms", 1),
            fmt_num(r, "/drag_settle_p50_ms", 1),
            fmt_num(r, "/drag_settle_p95_ms", 1),
            fmt_num(r, "/drag_shown_pct", 1),
            fmt_num(r, "/cpu_pct", 1),
            fmt_num(r, "/gpu/sm_mean_pct", 1),
            fmt_num(r, "/gpu/dec_mean_pct", 1),
            fmt_num(r, "/gpu/vram_max_mb", 0),
            fmt_num(r, "/ram_peak_mb", 0),
            fmt_num(r, "/transfers_per_seek/total_ops", 2),
            fmt_num(r, "/transfers_per_seek/total_mb", 2),
        ));
    }

    md.push_str("\n## Export 4K 60s (S7)\n\n");
    md.push_str("| Case | Format | Status | Wall s | FPS | Realtime x | CPU % | GPU 3D/Dec/Enc % | VRAM Max MB | Peak RAM MB | CPU↔GPU Ops/f | CPU↔GPU MB/f | A/V Drift ms |\n");
    md.push_str("|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n");
    for r in rows.iter().filter(|r| r["kind"] == "export") {
        let status = match r["error"].as_str() {
            Some(e) => format!("ERR: {e}"),
            None => r["status"].as_str().unwrap_or("ok").to_string(),
        };
        md.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {}/{}/{} | {} | {} | {} | {} | {} |\n",
            r["case"].as_str().unwrap_or("?"),
            r["format"].as_str().unwrap_or("?"),
            status,
            fmt_num(r, "/wall_s", 2),
            fmt_num(r, "/export_fps", 2),
            fmt_num(r, "/realtime_x", 2),
            fmt_num(r, "/cpu_pct", 1),
            fmt_num(r, "/gpu/sm_mean_pct", 1),
            fmt_num(r, "/gpu/dec_mean_pct", 1),
            fmt_num(r, "/gpu/enc_mean_pct", 1),
            fmt_num(r, "/gpu/vram_max_mb", 0),
            fmt_num(r, "/ram_peak_mb", 0),
            fmt_num(r, "/transfers_per_frame/total_ops", 2),
            fmt_num(r, "/transfers_per_frame/total_mb", 2),
            fmt_num(r, "/av_drift_ms", 2),
        ));
    }
    md
}

fn arg_val(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let use_gpu = !args.iter().any(|a| a == "--cpu");
    let hw_off = args.iter().any(|a| a == "--hw-off");
    let seconds = arg_val(&args, "--seconds").and_then(|v| v.parse::<f64>().ok()).unwrap_or(8.0);

    if let Some(single) = arg_val(&args, "--single") {
        let res = run_single(&single, use_gpu, hw_off, seconds);
        let text = serde_json::to_string_pretty(&res).unwrap_or_default();
        if let Some(p) = arg_val(&args, "--json") {
            std::fs::write(&p, &text).expect("write single json");
        } else {
            println!("{text}");
        }
        return;
    }

    let label = arg_val(&args, "--label").unwrap_or_else(|| "baseline".to_string());
    let cases: Vec<String> =
        arg_val(&args, "--cases").map(|s| s.split(',').map(str::to_string).collect()).unwrap_or_else(|| ALL_CASES.iter().map(|s| s.to_string()).collect());
    let out_dir = arg_val(&args, "--out").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/bench"));
    std::fs::create_dir_all(&out_dir).expect("create bench out dir");
    let exe = std::env::current_exe().expect("current exe");
    let results = Arc::new(Mutex::new(Vec::<Value>::new()));

    for case in &cases {
        eprintln!("=== Running {case} ===");
        let tmp = out_dir.join(format!(".tmp-{case}-{}.json", std::process::id()));
        let mut cmd = Command::new(&exe);
        cmd.args(["--single", case, "--json"]).arg(&tmp).args(["--seconds", &seconds.to_string()]);
        if !use_gpu {
            cmd.arg("--cpu");
        }
        if hw_off {
            cmd.arg("--hw-off");
        }
        let status = cmd.status().expect("spawn single case");
        let val: Value = std::fs::read(&tmp)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_else(|| json!({"case": case, "status": "failed", "error": format!("exit {status}")}));
        let _ = std::fs::remove_file(&tmp);
        eprintln!("  -> {}", serde_json::to_string(&val).unwrap_or_default());
        results.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(val);
    }

    let rows = results.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    let doc = json!({
        "label": label,
        "gpu": use_gpu,
        "hw": if hw_off { "off" } else { "auto" },
        "seconds": seconds,
        "cases": rows,
    });
    let json_path = out_dir.join(format!("s1_s7-{label}.json"));
    let md_path = out_dir.join(format!("s1_s7-{label}.md"));
    std::fs::write(&json_path, serde_json::to_string_pretty(&doc).unwrap_or_default()).expect("write json");
    let md = to_markdown(&doc);
    std::fs::write(&md_path, &md).expect("write md");
    println!("{md}");
    eprintln!("Saved {} and {}", json_path.display(), md_path.display());
}
