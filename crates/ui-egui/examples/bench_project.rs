//! Real-project benchmark: opens an existing `.fcproj` (a **copy**) with a headless `Session`,
//! then measures what the Program monitor does â€” preroll/TTFF, playback at a given preview scale,
//! random seeks, present (UI) cost, the frame server's own decode/render profile, CPU and GPU.
//!
//! ```sh
//! cargo run --release -p filmcraft-ui-egui --example bench_project -- \
//!     --project target/fixtures/publi-test/publi-notholy.fcproj \
//!     --scale 1.0 --seconds 20 --seek-count 30 --label pub-head-full-r1 --out target/bench
//! ```
//!
//! The harness never saves: it only opens the copy and renders frames.
//!
//! Both builds of the test protocol use this same file; the baseline tree carries a copy adapted
//! only where the older API differs (see `target/bench/publi-harness-compat.diff`).

#![allow(dead_code)]

#[path = "bench/fixtures.rs"]
mod fixtures;
#[path = "bench/playback.rs"]
mod playback;

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use filmcraft_engine::Session;
use filmcraft_gpu::{GpuCompositor, adapter};
use filmcraft_project::ItemId;
use filmcraft_time::Tick;
use filmcraft_ui_egui::frames::{FrameKey, FrameServer, PREROLL_TIMEOUT_S, PlaybackMeter, Target, process_cpu_time};
use serde_json::{Value, json};

// ------------------------------------------------------------------------------------ gpu sampler

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

fn process_ram_mb() -> f64 {
    let pid = std::process::id();
    let script = format!("$p=Get-Process -Id {pid}; Write-Output \"$($p.WorkingSet64)\"");
    let Ok(out) = Command::new("powershell").args(["-NoProfile", "-Command", &script]).output() else {
        return 0.0;
    };
    String::from_utf8_lossy(&out.stdout).trim().parse::<f64>().unwrap_or(0.0) / (1024.0 * 1024.0)
}

// ------------------------------------------------------------------------------------ display

/// The UI side of the Program monitor: presents the frame it gets (GPU composite or texture
/// conversion) and times that work, as `gpu_present` / `texture_for` do in the app.
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
                // Wait for the GPU so the time includes the upload and the draw.
                let _ = dev.poll(eframe::wgpu::PollType::wait_indefinitely());
                self.last = Some(k);
                presented = true;
            }
        } else {
            let img = server.get(&key).map(|i| (key, i));
            exact = img.is_some();
            let near = img.or_else(|| server.nearest_with_key(FrameKey { draft: false, ..key }, 6));
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

// ------------------------------------------------------------------------------------ helpers

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn pct(v: &[f64], p: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    s[((s.len() - 1) as f64 * p).round() as usize]
}

fn arg_val(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned()
}

fn cores() -> f64 {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as f64
}

/// FrameServer counters before/after a phase, as deltas.
struct StatsSnap {
    stats: filmcraft_ui_egui::frames::FrameStats,
}

impl StatsSnap {
    fn take(server: &FrameServer) -> Self {
        Self { stats: server.stats() }
    }
}

fn stats_json(before: &StatsSnap, after: &StatsSnap, wall_s: f64, cores: f64, cpu_ms: f64, shown: u64) -> Value {
    let (a, b) = (&before.stats, &after.stats);
    let jobs = b.jobs.saturating_sub(a.jobs);
    let src = b.source_ms - a.source_ms;
    let render = b.render_ms - a.render_ms;
    let n = jobs.max(1) as f64;
    let render_recent: Vec<f32> = b.recent.iter().map(|r| r.1).collect();
    let src_recent: Vec<f32> = b.recent.iter().map(|r| r.0).collect();
    let recent_f64 = |v: Vec<f32>| -> Vec<f64> { v.into_iter().map(f64::from).collect() };
    let (rr, sr) = (recent_f64(render_recent), recent_f64(src_recent));
    let req = b.request_hits.saturating_sub(a.request_hits) + b.request_misses.saturating_sub(a.request_misses);
    json!({
        "jobs": jobs,
        "cancelled": b.cancelled.saturating_sub(a.cancelled),
        "failed": b.failed.saturating_sub(a.failed),
        "prefetch": b.prefetch.saturating_sub(a.prefetch),
        "preview": b.preview.saturating_sub(a.preview),
        "request_hit_rate": if req == 0 { 0.0 } else { b.request_hits.saturating_sub(a.request_hits) as f64 / req as f64 },
        "decodeMs": {"mean": src / n, "p50": pct(&sr, 0.5), "p95": pct(&sr, 0.95)},
        "renderMs": {"mean": render / n, "p50": pct(&rr, 0.5), "p95": pct(&rr, 0.95), "recent_samples": rr.len()},
        "jobMsMean": (src + render) / n,
        "jobs_per_s": jobs as f64 / wall_s.max(0.001),
        "jobs_per_shown_frame": jobs as f64 / shown.max(1) as f64,
        "cpu_pct": cpu_ms / 1000.0 / wall_s.max(0.001) / cores * 100.0,
        "cpu_cores_used": cpu_ms / 1000.0 / wall_s.max(0.001),
    })
}

// ------------------------------------------------------------------------------------ main

fn main() {
    filmcraft_platform::register();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let project_path = arg_val(&args, "--project").expect("--project <fcproj>");
    let scale: f32 = arg_val(&args, "--scale").and_then(|v| v.parse().ok()).unwrap_or(1.0);
    let seconds: f64 = arg_val(&args, "--seconds").and_then(|v| v.parse().ok()).unwrap_or(20.0);
    let seek_count: usize = arg_val(&args, "--seek-count").and_then(|v| v.parse().ok()).unwrap_or(30);
    let label = arg_val(&args, "--label").unwrap_or_else(|| "run".to_string());
    let out_dir = arg_val(&args, "--out").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/bench"));
    let no_gpu = args.iter().any(|a| a == "--cpu");

    let mut s = Session::default();
    if let Err(e) = s.execute("file.open", json!({"path": project_path})) {
        eprintln!("file.open failed: {e}");
        std::process::exit(1);
    }
    let Some(seq) = s.state.active_sequence else {
        eprintln!("project has no active sequence");
        std::process::exit(1);
    };
    let Some(q) = s.project.sequence(seq) else {
        eprintln!("active sequence {seq:?} missing");
        std::process::exit(1);
    };
    let seq_name = s.project.item(seq).map(|i| i.name.clone()).unwrap_or_default();
    let rate = q.settings.frame_rate;
    let (sw, sh) = (q.settings.width, q.settings.height);
    let duration = q.duration();
    let total_frames = rate.frame_at(duration).max(1);
    let last_frame = (total_frames - 1).max(0);
    let project = s.project.clone();
    let revision = s.revision;
    let workers = FrameServer::default_workers();
    let size = (scale * 1000.0) as u32;
    let period = Duration::from_secs_f64(1.0 / 60.0);
    let cores = cores();
    let seconds = seconds.min((duration - rate.tick_of(4)).seconds().max(1.0));

    let mut display = PreviewDisplay::new(!no_gpu);
    let (adapter_name, backend_name) = display.adapter_info();
    let render_path = display.render_path.to_string();
    let r16 = display.r16_unorm();

    // ---- phase 1: preroll / TTFF + playback -------------------------------------------------
    let server = FrameServer::new(s.media.clone(), s.services.clone(), s.previews.clone(), workers);
    server.set_r16_unorm(display.r16_unorm());
    server.set_profiling(true);
    let target = display.target(seq);
    let t_start = Instant::now();
    let mut ttff_ms: Option<f64> = None;
    loop {
        let key = FrameKey { target, frame: 0, size, revision, draft: false };
        server.schedule_playback(key, rate, scale, &project, 1.0, true);
        let exact = display.refresh(&server, key);
        if exact && ttff_ms.is_none() {
            ttff_ms = Some(ms(t_start.elapsed()));
        }
        if server.preroll_ready(key, 1.0, last_frame) || t_start.elapsed().as_secs_f64() >= PREROLL_TIMEOUT_S {
            if ttff_ms.is_none() && display.refresh(&server, key) {
                ttff_ms = Some(ms(t_start.elapsed()));
            }
            break;
        }
        std::thread::sleep(period);
    }
    let preroll_ms = ms(t_start.elapsed());
    let first_frame_in_preroll = ttff_ms.is_some();
    // The preroll gives up after `PREROLL_TIMEOUT_S`; keep asking for frame 0 so TTFF is a real
    // number rather than "never" (the play clock below still starts now).
    if ttff_ms.is_none() {
        let key = FrameKey { target, frame: 0, size, revision, draft: false };
        let cap = Duration::from_secs(30);
        while ttff_ms.is_none() && t_start.elapsed() < cap {
            server.schedule_playback(key, rate, scale, &project, 1.0, true);
            if display.refresh(&server, key) {
                ttff_ms = Some(ms(t_start.elapsed()));
            }
            std::thread::sleep(period);
        }
    }

    let ram0 = process_ram_mb();
    let mut meter = PlaybackMeter::default();
    meter.start(1.0);
    display.present_ms.clear();
    let gpu_play = GpuMonitor::start();
    let before = StatsSnap::take(&server);
    let cpu0 = process_cpu_time().unwrap_or_default();
    let gop0 = filmcraft_codecs::gop_stats();
    let anchor = Instant::now();
    let mut next = anchor;
    let mut present_samples = Vec::new();
    loop {
        let now = Instant::now();
        let t = Tick::from_seconds_f64((now - anchor).as_secs_f64());
        if t >= Tick::from_seconds_f64(seconds) {
            break;
        }
        let frame = rate.frame_at(t);
        let key = FrameKey { target, frame, size, revision, draft: false };
        server.schedule_playback(key, rate, scale, &project, 1.0, false);
        let exact = display.refresh(&server, key);
        meter.refresh(frame, exact);
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
    std::thread::sleep(Duration::from_millis(100));
    present_samples.extend(display.present_ms.iter().copied());
    let cpu_ms = ms(process_cpu_time().unwrap_or_default().saturating_sub(cpu0));
    let after = StatsSnap::take(&server);
    let gop_play = filmcraft_codecs::gop_stats() - gop0;
    let gpu_play_sum = gpu_play.stop();
    let ram1 = process_ram_mb();

    let shown = meter.shown;
    let dropped = meter.dropped;
    let total = (shown + dropped).max(1) as f64;
    let play = json!({
        "effective_fps": shown as f64 / wall_s,
        "target_fps": rate.as_f64(),
        "wall_s": wall_s,
        "shown": shown,
        "dropped": dropped,
        "drop_pct": 100.0 * dropped as f64 / total,
        "on_time_pct": 100.0 * shown as f64 / total,
        "ttff_ms": ttff_ms,
        "preroll_ms": preroll_ms,
        "first_frame_in_preroll": first_frame_in_preroll,
        "ui_present_p50_ms": pct(&present_samples, 0.5),
        "ui_present_p95_ms": pct(&present_samples, 0.95),
        "present_samples": present_samples.len(),
        "cpu_pct": cpu_ms / 1000.0 / wall_s / cores * 100.0,
        "cpu_cores_used": cpu_ms / 1000.0 / wall_s,
        "ram_ws_mb": ram1,
        "ram_delta_mb": ram1 - ram0,
        "gpu": gpu_summary_json(gpu_play_sum),
        "profile": stats_json(&before, &after, wall_s, cores, cpu_ms, shown),
        "gop": {
            "decoded": gop_play.decoded,
            "seeks": gop_play.seeks,
            "evicted": gop_play.evicted,
            "hits": gop_play.hits,
            "misses": gop_play.misses,
        },
    });
    drop((display, server));

    // ---- phase 2: random seeks ---------------------------------------------------------------
    let mut display = PreviewDisplay::new(!no_gpu);
    let server = FrameServer::new(s.media.clone(), s.services.clone(), s.previews.clone(), workers);
    server.set_r16_unorm(display.r16_unorm());
    server.set_profiling(true);
    let target = display.target(seq);
    let gpu_seek = GpuMonitor::start();
    let before = StatsSnap::take(&server);
    let cpu0 = process_cpu_time().unwrap_or_default();
    let gop0 = filmcraft_codecs::gop_stats();
    let t_seek = Instant::now();
    let mut seek_ms: Vec<f64> = Vec::new();
    let mut timeouts = 0usize;
    let mut rng = 0x2545_f491_4f6c_dd1du64;
    let cap = Duration::from_secs(20);
    for _ in 0..seek_count {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        let frame = (rng % total_frames.max(1) as u64) as i64;
        let key = FrameKey { target, frame, size, revision, draft: false };
        let t0 = Instant::now();
        loop {
            server.request(key, rate.tick_of(frame), scale, &project, 0);
            if display.refresh(&server, key) {
                seek_ms.push(ms(t0.elapsed()));
                break;
            }
            if t0.elapsed() >= cap {
                timeouts += 1;
                seek_ms.push(ms(cap));
                break;
            }
            std::thread::sleep(period);
        }
    }
    let seek_wall = t_seek.elapsed().as_secs_f64().max(0.001);
    server.retain_queue(|_| false);
    std::thread::sleep(Duration::from_millis(100));
    let seek_cpu_ms = ms(process_cpu_time().unwrap_or_default().saturating_sub(cpu0));
    let after = StatsSnap::take(&server);
    let gop_seek = filmcraft_codecs::gop_stats() - gop0;
    let gpu_seek_sum = gpu_seek.stop();
    let seek = json!({
        "count": seek_count,
        "timeouts": timeouts,
        "p50_ms": pct(&seek_ms, 0.5),
        "p95_ms": pct(&seek_ms, 0.95),
        "max_ms": pct(&seek_ms, 1.0),
        "wall_s": seek_wall,
        "cpu_pct": seek_cpu_ms / 1000.0 / seek_wall / cores * 100.0,
        "gpu": gpu_summary_json(gpu_seek_sum),
        "profile": stats_json(&before, &after, seek_wall, cores, seek_cpu_ms, seek_count as u64),
        "gop": {
            "decoded": gop_seek.decoded,
            "seeks": gop_seek.seeks,
            "evicted": gop_seek.evicted,
            "hits": gop_seek.hits,
            "misses": gop_seek.misses,
        },
    });

    let doc = json!({
        "label": label,
        "project": project_path,
        "sequence": seq_name,
        "resolution": format!("{sw}x{sh}"),
        "sequence_fps": rate.as_f64(),
        "duration_s": duration.seconds(),
        "frames": total_frames,
        "scale": scale,
        "size_key": size,
        "seconds": seconds,
        "draft_decode": false,
        "render_path": render_path,
        "adapter": adapter_name,
        "backend": backend_name,
        "r16_unorm": r16,
        "workers": workers,
        "play": play,
        "seek": seek,
    });

    let _ = std::fs::create_dir_all(&out_dir);
    let json_path = out_dir.join(format!("{}.json", doc["label"].as_str().unwrap_or("run")));
    if let Ok(text) = serde_json::to_string_pretty(&doc)
        && let Err(e) = std::fs::write(&json_path, text)
    {
        eprintln!("write {}: {e}", json_path.display());
    }
    // One line on stdout so a shell loop can log the run's headline numbers.
    let p = &doc["play"];
    println!(
        "{}: fps={:.2} drop={:.1}% ttff={} preroll={:.0}ms seek50={:.0} seek95={:.0} ui95={:.1}Ms cpu={:.0}% gpu3d={:.0}% dec={:.0}% vram={:.0}MB",
        doc["label"].as_str().unwrap_or("run"),
        p["effective_fps"].as_f64().unwrap_or(0.0),
        p["drop_pct"].as_f64().unwrap_or(0.0),
        p["ttff_ms"].as_f64().map(|v| format!("{v:.0}")).unwrap_or_else(|| "â€“".into()),
        p["preroll_ms"].as_f64().unwrap_or(0.0),
        doc["seek"]["p50_ms"].as_f64().unwrap_or(0.0),
        doc["seek"]["p95_ms"].as_f64().unwrap_or(0.0),
        p["ui_present_p95_ms"].as_f64().unwrap_or(0.0),
        p["cpu_pct"].as_f64().unwrap_or(0.0),
        p["gpu"]["sm_mean_pct"].as_f64().unwrap_or(0.0),
        p["gpu"]["dec_mean_pct"].as_f64().unwrap_or(0.0),
        p["gpu"]["vram_max_mb"].as_f64().unwrap_or(0.0),
    );
    println!("json: {}", json_path.display());
}
