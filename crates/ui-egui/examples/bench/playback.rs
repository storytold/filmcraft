//! Headless playback benchmark (`bench_playback`; also the playback and scrub sections of
//! `bench`): drives the Program monitor's frame scheduler, prefetch and cache ([`FrameServer`],
//! [`PlaybackMeter`]) exactly as the UI does, without a window.
//!
//! ```sh
//! cargo xtask bench-playback                      # all scenarios, GPU path, Full + Half
//! cargo run --release -p filmcraft-ui-egui --example bench_playback -- --scenario stack3 --res full --cpu
//! ```
//!
//! Each play simulates display refreshes at `--refresh` Hz on the wall clock (as the app does
//! without an audio device) and counts shown/dropped frames with the same meter as the app. Wall-
//! clock numbers depend on machine load, so every run also reports CPU time (process and per
//! frame-worker thread), which does not. See `docs/testing.md` §5.
//!
//! Fixtures are generated with ffmpeg into `target/fixtures/playback/` on first use (never
//! committed). Scenarios that need them are skipped when ffmpeg is missing.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use filmcraft_engine::Session;
use filmcraft_project::{ItemId, ParamValue, Project, SequenceSettings, TrackKind, find_effect, resolve_auto_points};
use filmcraft_time::{FrameRate, Tick, TimeRange};
use filmcraft_ui_egui::frames::{FrameKey, FrameServer, JobRecord, PREROLL_TIMEOUT_S, PlaybackMeter, Target, process_cpu_time, thread_cpu_time};
use serde_json::{Value, json};

use crate::fixtures::fixture;

// ------------------------------------------------------------------------------------ projects

pub fn import(s: &mut Session, path: &Path) -> ItemId {
    let r = s.execute("file.import", json!({ "paths": [path.to_string_lossy()] })).expect("import");
    ItemId(r["items"].as_array().and_then(|a| a.first()).and_then(Value::as_u64).unwrap_or_else(|| panic!("import {}: {r}", path.display())))
}

/// One clip per track from t = 0 (`layers[0]` on V1), with (scale %, position, rotation°,
/// opacity %) Motion/Opacity settings per layer.
pub fn build_sequence(s: &mut Session, w: u32, h: u32, layers: &[(ItemId, f64, Option<(f64, f64)>, f64, f64)], effects: &[&str], secs: f64) -> ItemId {
    let rate = FrameRate::FPS_23_976;
    let sizes: Vec<(u32, u32)> = layers
        .iter()
        .map(|l| match s.project.item(l.0).map(|i| &i.kind) {
            Some(filmcraft_project::ItemKind::Media(m)) => m.info.video.as_ref().map(|v| (v.width, v.height)).unwrap_or((w, h)),
            _ => (w, h),
        })
        .collect();
    s.edit("bench sequence", |p: &mut Project, st| {
        let settings = SequenceSettings { width: w, height: h, frame_rate: rate, ..SequenceSettings::default() };
        let seq = p.new_sequence("Bench", settings, layers.len().max(1), 1, None);
        for (i, &(item, scale, pos, rot, opacity)) in layers.iter().enumerate() {
            let range = TimeRange::new(Tick::ZERO, rate.snap(Tick::from_seconds_f64(secs)));
            let mut v = p.make_track_item(item, TrackKind::Video, Tick::ZERO, range, rate).expect("track item");
            for e in &mut v.effects {
                resolve_auto_points(e, (w, h), sizes[i]);
            }
            if let Some(m) = v.effect_mut("motion") {
                if let Some(x) = m.params.get_mut("scale") {
                    x.value = ParamValue::Float(scale);
                }
                if let (Some(x), Some((px, py))) = (m.params.get_mut("position"), pos) {
                    x.value = ParamValue::Vec2(filmcraft_geom::Vec2::new(px, py));
                }
                if let Some(x) = m.params.get_mut("rotation") {
                    x.value = ParamValue::Float(rot);
                }
            }
            if let Some(o) = v.effect_mut("opacity").and_then(|o| o.params.get_mut("opacity")) {
                o.value = ParamValue::Float(opacity);
            }
            if i == 0 {
                for (k, id) in effects.iter().enumerate() {
                    let mut e = find_effect(id).unwrap_or_else(|| panic!("effect {id}")).instance();
                    let set: &[(&str, f64)] = match *id {
                        "lumetri" => &[("temperature", 18.0), ("contrast", 22.0)],
                        "sharpen" => &[("amount", 40.0)],
                        _ => &[],
                    };
                    for (name, val) in set {
                        if let Some(p) = e.params.get_mut(*name) {
                            p.value = ParamValue::Float(*val);
                        }
                    }
                    v.effects.insert(k, e);
                }
            }
            p.sequence_mut(seq).expect("seq").video_tracks[i].items.push(v);
        }
        st.active_sequence = Some(seq);
        st.open_sequences = vec![seq];
        st.playheads.insert(seq, Tick::ZERO);
        Ok(seq)
    })
    .expect("sequence")
}

// ------------------------------------------------------------------------------------ display

/// The UI side of a monitor: presents the frame it gets (GPU composite or texture conversion)
/// and times that work, as `gpu_present` / `texture_for` do in the app.
pub struct Display {
    gpu: Option<(filmcraft_gpu::GpuCompositor, eframe::wgpu::Device)>,
    last: Option<FrameKey>,
    present: Vec<Duration>,
    ui_cpu: Duration,
    uploaded: u64,
}

impl Display {
    pub fn new(gpu: bool) -> Self {
        let gpu = if gpu {
            // Windows with `FILMCRAFT_DXC_DIR` (a folder with dxcompiler.dll and dxil.dll): a DX12
            // device compiling with DXC, and zero-copy hardware decoding on it, as the desktop app
            // does when it finds DXC (`apps/filmcraft/src/dx12.rs`). `--hw off` keeps CPU pictures.
            #[cfg(windows)]
            let dxc = std::env::var_os("FILMCRAFT_DXC_DIR").map(|d| std::path::Path::new(&d).join("dxcompiler.dll"));
            #[cfg(not(windows))]
            let dxc: Option<std::path::PathBuf> = None;
            let instance = match &dxc {
                Some(dll) => eframe::wgpu::Instance::new(eframe::wgpu::InstanceDescriptor {
                    backends: eframe::wgpu::Backends::DX12,
                    backend_options: eframe::wgpu::BackendOptions {
                        dx12: eframe::wgpu::Dx12BackendOptions {
                            shader_compiler: eframe::wgpu::Dx12Compiler::DynamicDxc { dxc_path: dll.to_string_lossy().into_owned() },
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                    ..eframe::wgpu::InstanceDescriptor::new_without_display_handle()
                }),
                None => eframe::wgpu::Instance::default(),
            };
            let adapter = pollster::block_on(instance.request_adapter(&eframe::wgpu::RequestAdapterOptions {
                power_preference: eframe::wgpu::PowerPreference::HighPerformance,
                ..Default::default()
            }))
            .ok();
            let features = adapter.as_ref().map(|a| a.features() & eframe::wgpu::Features::TEXTURE_FORMAT_16BIT_NORM).unwrap_or_default();
            adapter
                .and_then(|a| pollster::block_on(a.request_device(&eframe::wgpu::DeviceDescriptor { required_features: features, ..Default::default() })).ok())
                .map(|(d, q)| {
                    let c = filmcraft_gpu::GpuCompositor::new(&d, &q);
                    #[cfg(windows)]
                    if dxc.is_some() && filmcraft_codecs::hw::hardware_decoding() {
                        eprintln!("zero-copy decoding: {}", filmcraft_platform::media_foundation::enable_zero_copy(&d));
                    }
                    (c, d)
                })
        } else {
            None
        };
        Self { gpu, last: None, present: Vec::new(), ui_cpu: Duration::ZERO, uploaded: 0 }
    }

    pub fn gpu_available(&self) -> bool {
        self.gpu.is_some()
    }

    fn target(&self, seq: ItemId) -> Target {
        if self.gpu.is_some() { Target::SequencePlan(seq) } else { Target::Sequence(seq) }
    }

    /// Show the exact frame, or the nearest earlier cached one; returns whether it was exact.
    fn refresh(&mut self, server: &FrameServer, key: FrameKey) -> bool {
        let (c0, t0) = (thread_cpu_time().unwrap_or_default(), Instant::now());
        let exact;
        let mut presented = false;
        if let Some((comp, dev)) = self.gpu.as_mut() {
            let plan = server.get_plan(&key).map(|p| (key, p));
            exact = plan.is_some();
            if let Some((k, plan)) = plan.or_else(|| server.nearest_plan(key, 6))
                && self.last != Some(k)
            {
                let before = comp.uploaded_bytes;
                let _ = comp.composite_prepared(&plan.plan, Some(&plan.prepared));
                // Wait for the GPU so the time includes the upload and the draw.
                let _ = dev.poll(eframe::wgpu::PollType::wait_indefinitely());
                self.uploaded += comp.uploaded_bytes - before;
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
                self.uploaded += img.px.len() as u64;
                self.last = Some(k);
                presented = true;
            }
        }
        if presented {
            self.present.push(t0.elapsed());
        }
        self.ui_cpu += thread_cpu_time().unwrap_or_default().saturating_sub(c0);
        exact
    }
}

// ------------------------------------------------------------------------------------ measuring

#[derive(Default)]
pub struct PlayReport {
    pub label: String,
    pub seconds: f64,
    pub shown: u64,
    pub dropped: u64,
    /// Due frames whose job finished before the frame was due.
    pub on_time: usize,
    pub due: usize,
    /// ms: queue→done latency, service time, (due − done) slack of due frames.
    pub latency: Vec<f64>,
    pub service: Vec<f64>,
    pub slack: Vec<f64>,
    /// ms per job: worker thread CPU, source fetch (decode) wall and thread CPU.
    pub job_cpu: Vec<f64>,
    pub src_wall: Vec<f64>,
    pub src_cpu: Vec<f64>,
    pub present: Vec<f64>,
    pub process_cpu: f64,
    pub ui_cpu: f64,
    pub jobs: usize,
    pub wasted_jobs: usize,
    pub cancelled_jobs: usize,
    pub preview_jobs: usize,
    pub gop: filmcraft_codecs::GopStats,
    pub uploaded_mb: f64,
    pub load: String,
    pub preroll_ms: f64,
}

pub fn pct(v: &[f64], p: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    s[((s.len() - 1) as f64 * p).round() as usize]
}

pub fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

pub fn load_avg() -> String {
    Command::new("sysctl")
        .args(["-n", "vm.loadavg"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().trim_matches(['{', '}', ' ']).to_string())
        .unwrap_or_default()
}

pub struct Bench {
    pub server: FrameServer,
    pub refresh_hz: f64,
    pub seconds: f64,
    /// Play with draft decoding (Settings ▸ Playback ▸ Draft decoding), as the monitor does at
    /// 1/2 resolution or lower.
    pub draft: bool,
}

impl Bench {
    /// Play `seconds` from `start` at `scale`, refreshing like the UI.
    pub fn play(&self, s: &Session, seq: ItemId, display: &mut Display, scale: f32, start: Tick, label: &str) -> PlayReport {
        let q = s.project.sequence(seq).expect("seq");
        let rate = q.settings.frame_rate;
        let end = (start + Tick::from_seconds_f64(self.seconds)).min(q.duration());
        let project = s.project.clone();
        let target = display.target(seq);
        let size = (scale * 1000.0) as u32;
        let mut meter = PlaybackMeter::default();
        meter.start(1.0);
        self.server.take_records();
        let gop0 = filmcraft_codecs::gop_stats();
        let (p0, ui0, up0) = (process_cpu_time().unwrap_or_default(), display.ui_cpu, display.uploaded);
        display.present.clear();
        let period = Duration::from_secs_f64(1.0 / self.refresh_hz);
        // Preroll as the app does: refresh without moving the playhead until the first frames are
        // ready (or the timeout), then start the clock.
        let first = rate.frame_at(start);
        let last_frame = rate.frame_at(q.duration()) - 1;
        let t_pre = Instant::now();
        loop {
            let key = FrameKey { target, frame: first, size, revision: s.revision, draft: self.draft };
            self.server.schedule_playback(key, rate, scale, &project, 1.0, true);
            let exact = display.refresh(&self.server, key);
            meter.refresh(first, exact);
            if self.server.preroll_ready(key, 1.0, last_frame) || t_pre.elapsed().as_secs_f64() >= PREROLL_TIMEOUT_S {
                break;
            }
            std::thread::sleep(period);
        }
        let preroll_ms = ms(t_pre.elapsed());
        let anchor = Instant::now();
        let mut due_at: HashMap<i64, Instant> = HashMap::new();
        let mut next = anchor;
        let mut frame;
        loop {
            let now = Instant::now();
            let t = start + Tick::from_seconds_f64((now - anchor).as_secs_f64());
            if t >= end {
                break;
            }
            frame = rate.frame_at(t);
            due_at.entry(frame).or_insert(now);
            let key = FrameKey { target, frame, size, revision: s.revision, draft: self.draft };
            self.server.schedule_playback(key, rate, scale, &project, 1.0, false);
            let exact = display.refresh(&self.server, key);
            meter.refresh(frame, exact);
            next += period;
            let now = Instant::now();
            if next > now {
                std::thread::sleep(next - now);
            } else {
                next = now;
            }
        }
        meter.finish();
        let wall = anchor.elapsed().as_secs_f64();
        // Stop as the app does (drop and cancel prefetch), letting running jobs wind down so their
        // CPU is counted.
        self.server.stop_prefetch();
        std::thread::sleep(Duration::from_millis(50));
        let process_cpu = ms(process_cpu_time().unwrap_or_default().saturating_sub(p0));
        let recs: Vec<JobRecord> = self.server.take_records();
        let mut by_frame: HashMap<i64, JobRecord> = HashMap::new();
        for r in &recs {
            if r.key.target == target && r.key.size == size {
                by_frame.insert(r.key.frame, *r);
            }
        }
        let mut rep = PlayReport {
            label: label.to_string(),
            seconds: wall,
            shown: meter.shown,
            dropped: meter.dropped,
            process_cpu,
            ui_cpu: ms(display.ui_cpu - ui0),
            jobs: recs.len(),
            gop: filmcraft_codecs::gop_stats() - gop0,
            uploaded_mb: (display.uploaded - up0) as f64 / 1e6,
            load: load_avg(),
            preroll_ms,
            ..Default::default()
        };
        let last = rate.frame_at(end - Tick(1));
        for f in first..=last {
            let Some(due) = due_at.get(&f) else { continue };
            rep.due += 1;
            if let Some(r) = by_frame.get(&f) {
                if r.finished <= *due {
                    rep.on_time += 1;
                    rep.slack.push(ms(*due - r.finished));
                } else {
                    rep.slack.push(-ms(r.finished - *due));
                }
            }
        }
        for r in &recs {
            rep.latency.push(ms(r.finished - r.queued));
            rep.service.push(ms(r.finished - r.started));
            rep.job_cpu.push(ms(r.cpu));
            rep.src_wall.push(ms(r.source_wall));
            rep.src_cpu.push(ms(r.source_cpu));
            if !due_at.contains_key(&r.key.frame) || r.key.target != target {
                rep.wasted_jobs += 1;
            }
            if r.preview {
                rep.preview_jobs += 1;
            }
            if r.cancelled {
                rep.cancelled_jobs += 1;
            }
        }
        rep.present = display.present.iter().map(|d| ms(*d)).collect();
        rep
    }

    /// Scrub: jump to a new random frame every `dwell` and measure how long the exact frame takes.
    pub fn seek_storm(&self, s: &Session, seq: ItemId, display: &mut Display, scale: f32, jumps: usize, dwell: Duration) -> PlayReport {
        let q = s.project.sequence(seq).expect("seq");
        let rate = q.settings.frame_rate;
        let frames = rate.frame_at(q.duration()).max(1);
        let project = s.project.clone();
        let target = display.target(seq);
        let size = (scale * 1000.0) as u32;
        self.server.take_records();
        let gop0 = filmcraft_codecs::gop_stats();
        let p0 = process_cpu_time().unwrap_or_default();
        let t0 = Instant::now();
        let mut rng = 0x2545_f491_4f6c_dd1du64;
        let mut rep = PlayReport { label: format!("seek storm ({jumps} jumps, {} ms)", dwell.as_millis()), ..Default::default() };
        let period = Duration::from_secs_f64(1.0 / self.refresh_hz);
        for _ in 0..jumps {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            let frame = (rng % frames as u64) as i64;
            let key = FrameKey { target, frame, size, revision: s.revision, draft: false };
            let start = Instant::now();
            rep.due += 1;
            loop {
                // As the monitor does while not playing: request the frame under the playhead.
                self.server.request(key, rate.tick_of(frame), scale, &project, 0);
                if display.refresh(&self.server, key) {
                    rep.on_time += 1;
                    rep.latency.push(ms(start.elapsed()));
                    break;
                }
                if start.elapsed() >= dwell {
                    rep.dropped += 1;
                    break;
                }
                std::thread::sleep(period);
            }
            let left = dwell.saturating_sub(start.elapsed());
            std::thread::sleep(left);
        }
        self.server.retain_queue(|_| false);
        std::thread::sleep(Duration::from_millis(50));
        rep.seconds = t0.elapsed().as_secs_f64();
        rep.process_cpu = ms(process_cpu_time().unwrap_or_default().saturating_sub(p0));
        let recs = self.server.take_records();
        rep.jobs = recs.len();
        for r in &recs {
            rep.service.push(ms(r.finished - r.started));
            rep.job_cpu.push(ms(r.cpu));
            rep.src_wall.push(ms(r.source_wall));
            rep.src_cpu.push(ms(r.source_cpu));
        }
        rep.shown = rep.on_time as u64;
        rep.gop = filmcraft_codecs::gop_stats() - gop0;
        rep.load = load_avg();
        rep
    }

    /// Scrub latency, two ways, through the monitor's request path:
    /// - `seeks` random jumps: time from the request to the exact frame on screen (no dwell cap:
    ///   every jump waits for its frame, up to `timeout`);
    /// - `drags` playhead drags: the playhead moves `drag_frames` frames, one step per display
    ///   refresh (each refresh asks for the frame under the playhead, superseding the previous
    ///   request), then stops; time from the stop until the frame under it is on screen.
    pub fn scrub(
        &self,
        s: &Session,
        seq: ItemId,
        display: &mut Display,
        scale: f32,
        seeks: usize,
        drags: usize,
        drag_frames: i64,
        timeout: Duration,
    ) -> ScrubReport {
        let q = s.project.sequence(seq).expect("seq");
        let rate = q.settings.frame_rate;
        let frames = rate.frame_at(q.duration()).max(1);
        let project = s.project.clone();
        let target = display.target(seq);
        let size = (scale * 1000.0) as u32;
        let period = Duration::from_secs_f64(1.0 / self.refresh_hz);
        let gop0 = filmcraft_codecs::gop_stats();
        let p0 = process_cpu_time().unwrap_or_default();
        let mut rep = ScrubReport { load: load_avg(), ..Default::default() };
        let mut rng = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        let wait = |display: &mut Display, key: FrameKey, start: Instant| -> Option<f64> {
            loop {
                self.server.request(key, rate.tick_of(key.frame), scale, &project, 0);
                if display.refresh(&self.server, key) {
                    return Some(ms(start.elapsed()));
                }
                if start.elapsed() >= timeout {
                    return None;
                }
                std::thread::sleep(period);
            }
        };
        for _ in 0..seeks {
            let frame = (next() % frames as u64) as i64;
            let key = FrameKey { target, frame, size, revision: s.revision, draft: false };
            match wait(display, key, Instant::now()) {
                Some(l) => rep.seek_ms.push(l),
                None => rep.timeouts += 1,
            }
        }
        for _ in 0..drags {
            let from = (next() % (frames - drag_frames).max(1) as u64) as i64;
            for f in from..from + drag_frames {
                let key = FrameKey { target, frame: f.min(frames - 1), size, revision: s.revision, draft: false };
                self.server.request(key, rate.tick_of(key.frame), scale, &project, 0);
                if display.refresh(&self.server, key) {
                    rep.drag_shown += 1;
                }
                rep.drag_steps += 1;
                std::thread::sleep(period);
            }
            let key = FrameKey { target, frame: (from + drag_frames).min(frames - 1), size, revision: s.revision, draft: false };
            match wait(display, key, Instant::now()) {
                Some(l) => rep.settle_ms.push(l),
                None => rep.timeouts += 1,
            }
        }
        self.server.retain_queue(|_| false);
        std::thread::sleep(Duration::from_millis(50));
        rep.process_cpu_ms = ms(process_cpu_time().unwrap_or_default().saturating_sub(p0));
        rep.gop = filmcraft_codecs::gop_stats() - gop0;
        rep
    }
}

/// Result of [`Bench::scrub`].
#[derive(Default)]
pub struct ScrubReport {
    pub seek_ms: Vec<f64>,
    pub settle_ms: Vec<f64>,
    pub timeouts: usize,
    pub drag_steps: usize,
    pub drag_shown: usize,
    pub process_cpu_ms: f64,
    pub gop: filmcraft_codecs::GopStats,
    pub load: String,
}

pub fn print_header() {
    println!(
        "{:<34} {:>6} {:>5} {:>5} {:>7} | {:>6} {:>6} {:>6} | {:>6} {:>6} | {:>6} {:>6} {:>6} | {:>6} {:>6} | {:>7} {:>6} | {:>5} {:>5} {:>5} {:>4} | load",
        "scenario",
        "wall s",
        "shown",
        "drop",
        "ontime",
        "lat50",
        "lat95",
        "lat99",
        "svc50",
        "svc95",
        "cpu/j",
        "src/j",
        "srcC/j",
        "ui p50",
        "ui p95",
        "cpu ms/f",
        "cores",
        "seeks",
        "dec",
        "waste",
        "canc"
    );
}

pub fn print_row(r: &PlayReport, fps: f64) {
    let frames = (r.shown + r.dropped).max(1) as f64;
    let cpu_per_frame = r.process_cpu / frames;
    println!(
        "{:<34} {:>6.2} {:>5} {:>5} {:>6.0}% | {:>6.1} {:>6.1} {:>6.1} | {:>6.1} {:>6.1} | {:>6.1} {:>6.1} {:>6.1} | {:>6.2} {:>6.2} | {:>7.1} {:>6.2} | {:>5} {:>5} {:>5} {:>4} | {}",
        r.label,
        r.seconds,
        r.shown,
        r.dropped,
        100.0 * r.on_time as f64 / r.due.max(1) as f64,
        pct(&r.latency, 0.5),
        pct(&r.latency, 0.95),
        pct(&r.latency, 0.99),
        pct(&r.service, 0.5),
        pct(&r.service, 0.95),
        r.job_cpu.iter().sum::<f64>() / r.jobs.max(1) as f64,
        r.src_wall.iter().sum::<f64>() / r.jobs.max(1) as f64,
        r.src_cpu.iter().sum::<f64>() / r.jobs.max(1) as f64,
        pct(&r.present, 0.5),
        pct(&r.present, 0.95),
        cpu_per_frame,
        cpu_per_frame * fps / 1000.0,
        r.gop.seeks,
        r.gop.decoded,
        r.wasted_jobs,
        r.cancelled_jobs,
        r.load
    );
}

pub fn to_json(r: &PlayReport, scenario: &str, res: &str, path: &str) -> Value {
    let frames = (r.shown + r.dropped).max(1) as f64;
    json!({
        "scenario": scenario, "res": res, "path": path, "play": r.label, "wall_s": r.seconds,
        "shown": r.shown, "dropped": r.dropped, "on_time": r.on_time, "due": r.due,
        "latency_ms": {"p50": pct(&r.latency, 0.5), "p95": pct(&r.latency, 0.95), "p99": pct(&r.latency, 0.99), "max": pct(&r.latency, 1.0)},
        "service_ms": {"p50": pct(&r.service, 0.5), "p95": pct(&r.service, 0.95)},
        "slack_ms": {"p5": pct(&r.slack, 0.05), "p50": pct(&r.slack, 0.5)},
        "job_cpu_ms_mean": r.job_cpu.iter().sum::<f64>() / r.jobs.max(1) as f64,
        "source_wall_ms_mean": r.src_wall.iter().sum::<f64>() / r.jobs.max(1) as f64,
        "source_cpu_ms_mean": r.src_cpu.iter().sum::<f64>() / r.jobs.max(1) as f64,
        "present_ms": {"p50": pct(&r.present, 0.5), "p95": pct(&r.present, 0.95), "max": pct(&r.present, 1.0)},
        "ui_cpu_ms": r.ui_cpu, "process_cpu_ms": r.process_cpu, "cpu_ms_per_frame": r.process_cpu / frames,
        "jobs": r.jobs, "wasted_jobs": r.wasted_jobs, "cancelled_jobs": r.cancelled_jobs, "preview_jobs": r.preview_jobs, "uploaded_mb": r.uploaded_mb,
        "gop": {"hits": r.gop.hits, "misses": r.gop.misses, "seeks": r.gop.seeks, "decoded": r.gop.decoded, "evicted": r.gop.evicted, "skipped": r.gop.skipped, "decode_ms": r.gop.decode_ns as f64 / 1e6},
        "loadavg": r.load, "preroll_ms": r.preroll_ms,
    })
}

// ------------------------------------------------------------------------------------ main

struct Args {
    scenarios: Vec<String>,
    res: Vec<(String, f32)>,
    gpu: bool,
    seconds: f64,
    refresh: f64,
    workers: usize,
    json: Option<String>,
    repeat: usize,
}

fn parse_args() -> Args {
    let mut a = Args {
        scenarios: vec![],
        res: vec![("full".into(), 1.0), ("half".into(), 0.5)],
        gpu: true,
        seconds: 8.0,
        refresh: 60.0,
        workers: FrameServer::default_workers(),
        json: None,
        repeat: 1,
    };
    let mut it = std::env::args().skip(1);
    while let Some(x) = it.next() {
        match x.as_str() {
            "--scenario" | "-s" => a.scenarios.extend(it.next().unwrap_or_default().split(',').map(str::to_string)),
            "--res" => {
                a.res = it
                    .next()
                    .unwrap_or_default()
                    .split(',')
                    .map(|r| match r {
                        "half" => ("half".to_string(), 0.5),
                        "quarter" => ("quarter".to_string(), 0.25),
                        _ => ("full".to_string(), 1.0),
                    })
                    .collect()
            }
            "--cpu" => a.gpu = false,
            "--gpu" => a.gpu = true,
            "--seconds" => a.seconds = it.next().and_then(|v| v.parse().ok()).unwrap_or(8.0),
            "--refresh" => a.refresh = it.next().and_then(|v| v.parse().ok()).unwrap_or(60.0),
            "--workers" => a.workers = it.next().and_then(|v| v.parse().ok()).unwrap_or(a.workers),
            "--repeat" => a.repeat = it.next().and_then(|v| v.parse().ok()).unwrap_or(1),
            "--json" => a.json = it.next(),
            "--help" | "-h" => {
                println!(
                    "bench_playback [--scenario h264-1080,stack3,stack3-blend,stack3-fx,h264-2160,hevc-2160,vp9-2160,av1-2160,demo,after-preview,seek-storm] [--res full,half] [--cpu|--gpu] [--seconds 8] [--refresh 60] [--workers N] [--repeat N] [--json out.json]"
                );
                std::process::exit(0);
            }
            other => eprintln!("ignoring argument {other}"),
        }
    }
    if a.scenarios.is_empty() {
        a.scenarios = ["h264-1080", "stack3", "stack3-blend", "stack3-fx", "h264-2160", "hevc-2160", "demo", "after-preview", "seek-storm"]
            .iter()
            .map(|s| s.to_string())
            .collect();
    }
    a
}

/// Build the session for a scenario (None when its fixtures are unavailable).
pub fn scenario_session(name: &str) -> Option<(Session, ItemId)> {
    let mut s = Session::default();
    let seq = match name {
        "h264-1080" | "seek-storm" => {
            let a = import(&mut s, &fixture("a1080.mp4")?);
            build_sequence(&mut s, 1920, 1080, &[(a, 100.0, None, 0.0, 100.0)], &[], 20.0)
        }
        "stack3" | "stack3-blend" | "stack3-fx" => {
            let (a, b, c) = (fixture("a1080.mp4")?, fixture("b1080.mp4")?, fixture("c1080.mp4")?);
            let (a, b, c) = (import(&mut s, &a), import(&mut s, &b), import(&mut s, &c));
            let seq = build_sequence(
                &mut s,
                1920,
                1080,
                &[(a, 100.0, None, 0.0, 100.0), (b, 60.0, Some((700.0, 420.0)), 0.0, 70.0), (c, 40.0, Some((1450.0, 760.0)), 12.0, 85.0)],
                &[],
                20.0,
            );
            if name == "stack3-blend" {
                // V2 in Screen, V3 in Overlay
                s.edit("blend modes", |p, _| {
                    let q = p.sequence_mut(seq).expect("seq");
                    for (track, mode) in [(1, "Screen"), (2, "Overlay")] {
                        let i = filmcraft_project::effect::BLEND_MODES.iter().position(|b| *b == mode).expect("blend mode") as u32;
                        for it in &mut q.video_tracks[track].items {
                            if let Some(b) = it.effect_mut("opacity").and_then(|o| o.params.get_mut("blend")) {
                                b.value = ParamValue::Choice(i);
                            }
                        }
                    }
                    Ok(())
                })
                .ok()?;
            }
            if name == "stack3-fx" {
                // V2: Brightness & Contrast + Gaussian Blur; V3: Tint
                s.edit("effects", |p, _| {
                    let q = p.sequence_mut(seq).expect("seq");
                    let fx: [(usize, &str, &[(&str, f64)]); 3] = [
                        (1, "brightness_contrast", &[("brightness", 12.0), ("contrast", 25.0)]),
                        (1, "gaussian_blur", &[("blurriness", 8.0)]),
                        (2, "tint", &[("amount", 70.0)]),
                    ];
                    for (track, id, params) in fx {
                        for it in &mut q.video_tracks[track].items {
                            let mut e = find_effect(id).expect("effect").instance();
                            for (k, v) in params {
                                if let Some(x) = e.params.get_mut(*k) {
                                    x.value = ParamValue::Float(*v);
                                }
                            }
                            it.effects.push(e);
                        }
                    }
                    Ok(())
                })
                .ok()?;
            }
            seq
        }
        "h264-2160" | "seek-storm-2160" => {
            let a = import(&mut s, &fixture("a2160.mp4")?);
            build_sequence(&mut s, 3840, 2160, &[(a, 100.0, None, 0.0, 100.0)], &[], 10.0)
        }
        "hevc-2160" | "seek-storm-hevc-2160" => {
            let a = import(&mut s, &fixture("hevc2160.mp4")?);
            build_sequence(&mut s, 3840, 2160, &[(a, 100.0, None, 0.0, 100.0)], &[], 10.0)
        }
        "vp9-2160" => {
            let a = import(&mut s, &fixture("vp92160.webm")?);
            build_sequence(&mut s, 3840, 2160, &[(a, 100.0, None, 0.0, 100.0)], &[], 10.0)
        }
        "av1-2160" => {
            let a = import(&mut s, &fixture("av12160.mp4")?);
            build_sequence(&mut s, 3840, 2160, &[(a, 100.0, None, 0.0, 100.0)], &[], 10.0)
        }
        "after-preview" => {
            let a = import(&mut s, &fixture("a1080.mp4")?);
            let seq = build_sequence(&mut s, 1920, 1080, &[(a, 100.0, None, 0.0, 100.0)], &["lumetri", "sharpen", "levels", "tint"], 20.0);
            s.edit("marks", |p, _| {
                let q = p.sequence_mut(seq).expect("seq");
                q.mark_in = Some(Tick::ZERO);
                q.mark_out = Some(q.settings.frame_rate.tick_of(8 * 24));
                Ok(())
            })
            .ok()?;
            seq
        }
        "demo" => {
            s.execute("file.openDemoProject", json!({})).ok()?;
            s.state.active_sequence?
        }
        _ => {
            eprintln!("unknown scenario {name}");
            return None;
        }
    };
    Some((s, seq))
}

/// `bench_playback` / `cargo xtask bench-playback`.
pub fn cli_main() {
    let args = parse_args();
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    println!(
        "FilmCraft playback bench: {} path, {} frame workers, {} cores, refresh {} Hz, {} s per play; load avg {}",
        if args.gpu { "GPU" } else { "CPU" },
        args.workers,
        cores,
        args.refresh,
        args.seconds,
        load_avg()
    );
    println!(
        "lat = queue→ready per job, svc = worker time per job, cpu/j = worker thread CPU, src/j = source fetch (decode) wall, srcC/j = its thread CPU,\nui = present time on the UI thread (GPU upload+draw / texture conversion), cpu ms/f = process CPU per due frame, cores = CPU cores that rate needs at the sequence fps,\nseeks/dec = decoder restarts / samples decoded, waste = finished jobs for frames never due, canc = jobs cancelled while running (all times ms)\n"
    );
    print_header();
    let mut out = Vec::new();
    for name in &args.scenarios {
        for (res_name, scale) in &args.res {
            if name == "seek-storm" && res_name != "full" && args.res.len() > 1 {
                continue;
            }
            let Some((mut s, seq)) = scenario_session(name) else {
                println!("{name}: skipped (fixtures unavailable: is ffmpeg installed?)");
                continue;
            };
            let fps = s.project.sequence(seq).map(|q| q.settings.frame_rate.as_f64()).unwrap_or(24.0);
            for rep_i in 0..args.repeat {
                // A fresh frame server per run so caches start cold, as after opening a project.
                let server = FrameServer::new(s.media.clone(), s.services.clone(), s.previews.clone(), args.workers);
                server.set_profiling(true);
                let bench = Bench { server, refresh_hz: args.refresh, seconds: args.seconds, draft: false };
                let mut display = Display::new(args.gpu);
                if args.gpu && display.gpu.is_none() {
                    eprintln!("no GPU adapter; using the CPU path");
                }
                let path = if display.gpu.is_some() { "gpu" } else { "cpu" };
                let tag = |l: &str| if args.repeat > 1 { format!("{name} {res_name} {l} #{}", rep_i + 1) } else { format!("{name} {res_name} {l}") };
                let mut reports = Vec::new();
                match name.as_str() {
                    "seek-storm" => {
                        let mut r = bench.seek_storm(&s, seq, &mut display, *scale, 40, Duration::from_millis(150));
                        r.label = tag("seek");
                        reports.push(r);
                    }
                    "after-preview" => {
                        // Live play (effects rendered per frame), render previews, then play twice.
                        reports.push(bench.play(&s, seq, &mut display, *scale, Tick::ZERO, &tag("live")));
                        let t0 = Instant::now();
                        s.execute("sequence.renderEffectsInToOut", json!({"wait": true})).expect("render previews");
                        eprintln!("  rendered previews in {:.1}s (load {})", t0.elapsed().as_secs_f64(), load_avg());
                        reports.push(bench.play(&s, seq, &mut display, *scale, Tick::ZERO, &tag("1st after")));
                        reports.push(bench.play(&s, seq, &mut display, *scale, Tick::ZERO, &tag("2nd after")));
                        s.previews.delete(None);
                    }
                    _ => reports.push(bench.play(&s, seq, &mut display, *scale, Tick::ZERO, &tag("play"))),
                }
                for r in &reports {
                    print_row(r, fps);
                    out.push(to_json(r, name, res_name, path));
                }
            }
        }
    }
    if let Some(p) = args.json {
        let doc = json!({"cores": cores, "workers": args.workers, "refresh_hz": args.refresh, "seconds": args.seconds, "runs": out});
        if let Err(e) = std::fs::write(&p, serde_json::to_string_pretty(&doc).unwrap_or_default()) {
            eprintln!("{p}: {e}");
        }
    }
}
