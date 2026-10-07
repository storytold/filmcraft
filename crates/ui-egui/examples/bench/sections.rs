//! The benchmark sections. Each returns one JSON row per measured case.

use std::sync::Arc;
use std::time::{Duration, Instant};

use filmcraft_engine::Session;
use filmcraft_project::{ItemId, Project, SequenceSettings, TrackKind};
use filmcraft_time::{FrameRate, Tick, TimeRange};
use filmcraft_ui_egui::frames::{FrameServer, process_cpu_time};
use serde_json::{Value, json};

use crate::Opts;
use crate::fixtures::fixture;
use crate::playback::{self, Bench, Display, load_avg, ms, pct};

fn median(v: &[f64]) -> f64 {
    pct(v, 0.5)
}

fn cpu_now() -> Duration {
    process_cpu_time().unwrap_or_default()
}

// ------------------------------------------------------------------------------------ decode

/// (fixture, codec, size)
const DECODE: &[(&str, &str, &str)] = &[
    ("dec_h264_1080.mp4", "H.264", "1080p"),
    ("dec_h264_2160.mp4", "H.264", "2160p"),
    ("dec_hevc_1080.mp4", "HEVC", "1080p"),
    ("dec_hevc_2160.mp4", "HEVC", "2160p"),
    ("dec_hevc10_2160.mp4", "HEVC Main 10", "2160p"),
    ("dec_vp9_1080.webm", "VP9", "1080p"),
    ("dec_vp9_2160.webm", "VP9", "2160p"),
    ("dec_av1_1080.mp4", "AV1", "1080p"),
    ("dec_av1_2160.mp4", "AV1", "2160p"),
    ("dec_prores_1080.mov", "ProRes 422 HQ", "1080p"),
    ("dec_prores_2160.mov", "ProRes 422 HQ", "2160p"),
];

/// Sequential decode of every frame through the media stack (container source, GOP cache,
/// decoder, frame conversion), as playback reads a clip. A fresh source per repeat.
pub fn decode(o: &Opts) -> Vec<Value> {
    let mut rows = Vec::new();
    for &(name, codec, size) in DECODE.iter().filter(|d| o.wants(d.0)) {
        let Some(path) = fixture(name) else {
            eprintln!("decode {name}: skipped (fixture unavailable)");
            continue;
        };
        let bytes: Arc<[u8]> = std::fs::read(&path).expect("read fixture").into();
        let mut fps = Vec::new();
        let mut cpu = Vec::new();
        let mut first = Vec::new();
        let mut frames = 0;
        let mut rate = FrameRate::FPS_23_976;
        let hw0 = filmcraft_codecs::hw::hw_stats();
        for _ in 0..o.repeat {
            let src = match filmcraft_codecs::open_bytes(name, bytes.clone()) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("decode {name}: {e}");
                    break;
                }
            };
            rate = src.info().frame_rate();
            let n = rate.frame_at(src.info().duration).max(1);
            let n = if o.quick { n.min(48) } else { n };
            frames = n;
            let (t0, c0) = (Instant::now(), cpu_now());
            for f in 0..n {
                if let Err(e) = src.video_frame(filmcraft_media::FrameRequest::full(rate.tick_of(f))) {
                    eprintln!("decode {name} frame {f}: {e}");
                    break;
                }
                if f == 0 {
                    first.push(ms(t0.elapsed()));
                }
            }
            let dt = t0.elapsed().as_secs_f64();
            fps.push(n as f64 / dt);
            cpu.push(ms(cpu_now() - c0) / n as f64);
        }
        if fps.is_empty() {
            continue;
        }
        let best = fps.iter().cloned().fold(0.0, f64::max);
        let hw = filmcraft_codecs::hw::hw_stats();
        let row = json!({
            "fixture": name, "codec": codec, "size": size, "frames": frames,
            "fps": best, "fps_min": fps.iter().cloned().fold(f64::MAX, f64::min), "fps_runs": fps,
            "cpu_ms_per_frame": median(&cpu), "first_frame_ms": median(&first), "realtime": best / rate.as_f64(),
            "decode_threads": rayon::current_num_threads(), "load": load_avg(),
            // OS hardware decoder activity during this row: pictures, decoders created, mid-stream
            // fallbacks, streams handed to software (all zero with --hw off)
            "hw_frames": hw.frames - hw0.frames, "hw_sessions": hw.sessions - hw0.sessions,
            "hw_fallbacks": hw.fallbacks - hw0.fallbacks, "hw_declined": hw.declined - hw0.declined,
        });
        eprintln!("decode {name}: {best:.1} fps");
        rows.push(row);
    }
    rows
}

// ------------------------------------------------------------------------------------ seek

/// Cold seeks through the media stack, the three catch-up modes interleaved per target so they
/// see the same machine load: `full` (decode every picture from the keyframe), `keep 2 s` (what a
/// scrub request does: skip non-reference pictures more than 2 s before the target) and `late`
/// (playback catching up: skip every non-reference picture before the target). A fresh source per
/// seek (cold GOP cache). CPU time and samples decoded do not depend on load; wall time does.
pub fn seek(o: &Opts) -> Vec<Value> {
    let seeks = if o.quick { 4 } else { 12 };
    let modes: [(&str, Option<Tick>); 3] = [("full", None), ("keep 2 s", Some(Tick::from_seconds_f64(2.0))), ("late", Some(Tick::ZERO))];
    let mut rows = Vec::new();
    for name in ["a1080.mp4", "a2160.mp4", "hevc2160.mp4"].into_iter().filter(|n| o.wants(n)) {
        let Some(path) = fixture(name) else {
            eprintln!("seek {name}: skipped (fixture unavailable)");
            continue;
        };
        let bytes: Arc<[u8]> = std::fs::read(&path).expect("read fixture").into();
        let mut rng = 0x2545_f491_4f6c_dd1du64;
        let (mut wall, mut cpu, mut dec, mut skip) = ([vec![], vec![], vec![]], [vec![], vec![], vec![]], [0u64; 3], [0u64; 3]);
        for _ in 0..seeks {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            for (m, (_, margin)) in modes.iter().enumerate() {
                let src = filmcraft_codecs::open_bytes(name, bytes.clone()).expect("open");
                let rate = src.info().frame_rate();
                let n = rate.frame_at(src.info().duration).max(1);
                let t = rate.tick_of((rng % n as u64) as i64);
                let (g0, t0, c0) = (filmcraft_codecs::gop_stats(), Instant::now(), cpu_now());
                filmcraft_media::cancel::with_catch_up(*margin, || src.video_frame(filmcraft_media::FrameRequest::full(t))).expect("frame");
                wall[m].push(ms(t0.elapsed()));
                cpu[m].push(ms(cpu_now() - c0));
                let g = filmcraft_codecs::gop_stats() - g0;
                dec[m] += g.decoded;
                skip[m] += g.skipped;
            }
        }
        for (m, (mode, _)) in modes.iter().enumerate() {
            let row = json!({
                "fixture": name, "mode": mode, "seeks": seeks,
                "p50_ms": pct(&wall[m], 0.5), "p95_ms": pct(&wall[m], 0.95), "cpu_ms_mean": cpu[m].iter().sum::<f64>() / seeks as f64,
                "decoded_per_seek": dec[m] as f64 / seeks as f64, "skipped_per_seek": skip[m] as f64 / seeks as f64, "load": load_avg(),
            });
            eprintln!("seek {name} {mode}: p50 {:.0} ms, CPU {:.0} ms", pct(&wall[m], 0.5), cpu[m].iter().sum::<f64>() / seeks as f64);
            rows.push(row);
        }
    }
    rows
}

// ------------------------------------------------------------------------------------ playback

fn new_bench(s: &Session, seconds: f64) -> Bench {
    let server = FrameServer::new(s.media.clone(), s.services.clone(), s.previews.clone(), FrameServer::default_workers());
    server.set_profiling(true);
    Bench { server, refresh_hz: 60.0, seconds, draft: false }
}

/// Program-monitor playback through the real scheduler (see `playback.rs`), frames shown vs
/// dropped over N seconds.
pub fn playback(o: &Opts) -> Vec<Value> {
    let seconds = if o.quick { 4.0 } else { 8.0 };
    // (scenario, resolution, scale, draft decoding)
    let cases: &[(&str, &str, f32, bool)] = &[
        ("h264-1080", "full", 1.0, false),
        ("stack3", "full", 1.0, false),
        ("stack3-blend", "full", 1.0, false),
        ("stack3-fx", "full", 1.0, false),
        ("h264-2160", "full", 1.0, false),
        ("h264-2160", "half", 0.5, false),
        ("h264-2160", "half draft", 0.5, true),
        ("h264-2160", "quarter", 0.25, false),
        ("h264-2160", "quarter draft", 0.25, true),
        ("hevc-2160", "full", 1.0, false),
        ("hevc-2160", "half", 0.5, false),
        ("hevc-2160", "half draft", 0.5, true),
        ("vp9-2160", "full", 1.0, false),
        ("vp9-2160", "half", 0.5, false),
        ("vp9-2160", "half draft", 0.5, true),
        ("av1-2160", "full", 1.0, false),
        ("av1-2160", "half", 0.5, false),
        ("av1-2160", "half draft", 0.5, true),
    ];
    let mut rows = Vec::new();
    for &(scenario, res, scale, draft) in cases.iter().filter(|c| o.wants(&format!("{} {}", c.0, c.1))) {
        let Some((s, seq)) = playback::scenario_session(scenario) else {
            eprintln!("playback {scenario}: skipped (fixtures unavailable)");
            continue;
        };
        let fps = s.project.sequence(seq).map(|q| q.settings.frame_rate.as_f64()).unwrap_or(24.0);
        for rep in 0..o.repeat {
            // A fresh frame server per run: caches start cold, as after opening a project.
            let mut bench = new_bench(&s, seconds);
            bench.draft = draft;
            let mut display = Display::new(o.gpu);
            let path = if display.gpu_available() { "gpu" } else { "cpu" };
            let hw0 = filmcraft_codecs::hw::hw_stats();
            let r = bench.play(&s, seq, &mut display, scale, Tick::ZERO, &format!("{scenario} {res} #{}", rep + 1));
            let mut v = playback::to_json(&r, scenario, res, path);
            let frames = (r.shown + r.dropped).max(1) as f64;
            v["drop_pct"] = json!(100.0 * r.dropped as f64 / frames);
            v["decode_ms"] = json!(r.src_wall.iter().sum::<f64>() / r.jobs.max(1) as f64);
            v["ui_p95_ms"] = json!(pct(&r.present, 0.95));
            v["seeks"] = json!(r.gop.seeks);
            v["skipped"] = json!(r.gop.skipped);
            v["draft_frames"] = json!(r.gop.draft);
            // OS hardware decoder activity during the run (zero with --hw off)
            let hw = filmcraft_codecs::hw::hw_stats();
            v["hw_frames"] = json!(hw.frames - hw0.frames);
            v["hw_sessions"] = json!(hw.sessions - hw0.sessions);
            v["hw_fallbacks"] = json!(hw.fallbacks - hw0.fallbacks);
            v["hw_zero_copy"] = json!(hw.zero_copy_frames - hw0.zero_copy_frames);
            v["load"] = json!(r.load);
            v["cores_needed"] = json!(r.process_cpu / frames * fps / 1000.0);
            eprintln!("playback {scenario} {res}: {}/{} shown/dropped (load {})", r.shown, r.dropped, r.load);
            rows.push(v);
        }
    }
    rows
}

// ------------------------------------------------------------------------------------ scrub

/// Scrubbing: random seeks and playhead drags at full resolution; time until the exact frame
/// under the playhead is on screen.
pub fn scrub(o: &Opts) -> Vec<Value> {
    let (seeks, drags) = if o.quick { (8, 3) } else { (24, 8) };
    let mut rows = Vec::new();
    for scenario in ["h264-1080", "h264-2160", "hevc-2160"].into_iter().filter(|s| o.wants(s)) {
        let Some((s, seq)) = playback::scenario_session(scenario) else {
            eprintln!("scrub {scenario}: skipped (fixtures unavailable)");
            continue;
        };
        for _ in 0..o.repeat {
            let bench = new_bench(&s, 0.0);
            let mut display = Display::new(o.gpu);
            let r = bench.scrub(&s, seq, &mut display, 1.0, seeks, drags, 24, Duration::from_secs(8));
            let positions = (seeks + drags) as f64;
            let row = json!({
                "scenario": scenario,
                "seek_p50_ms": pct(&r.seek_ms, 0.5), "seek_p95_ms": pct(&r.seek_ms, 0.95), "seek_max_ms": pct(&r.seek_ms, 1.0),
                "settle_p50_ms": pct(&r.settle_ms, 0.5), "settle_p95_ms": pct(&r.settle_ms, 0.95),
                "drag_shown_pct": 100.0 * r.drag_shown as f64 / r.drag_steps.max(1) as f64,
                "timeouts": r.timeouts, "gop_seeks": r.gop.seeks, "decoded": r.gop.decoded, "skipped": r.gop.skipped,
                "decoded_per_seek": r.gop.decoded as f64 / positions, "process_cpu_ms": r.process_cpu_ms, "load": r.load,
            });
            eprintln!("scrub {scenario}: seek p50 {:.0} ms p95 {:.0} ms (load {})", pct(&r.seek_ms, 0.5), pct(&r.seek_ms, 0.95), r.load);
            rows.push(row);
        }
    }
    rows
}

// ------------------------------------------------------------------------------------ big projects

/// A sequence of 20 tracks (12 video, 8 audio) × 50 clips = 1000 clips of the small clip fixture,
/// 1–3 s each with small gaps, varied source in-points. Returns the sequence and its clip count.
pub fn big_sequence(s: &mut Session, item: ItemId, name: &str, seed: u64) -> (ItemId, usize) {
    let rate = FrameRate::FPS_23_976;
    let src_frames = rate.frame_at(Tick::from_seconds_f64(19.0));
    let mut rng = seed | 1;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    s.edit("bench sequence", |p: &mut Project, st| {
        let settings = SequenceSettings { width: 1920, height: 1080, frame_rate: rate, ..SequenceSettings::default() };
        let seq = p.new_sequence(name, settings, 12, 8, None);
        let mut clips = 0;
        for t in 0..20 {
            let kind = if t < 12 { TrackKind::Video } else { TrackKind::Audio };
            let mut at = (next() % 24) as i64;
            let mut items = Vec::new();
            for _ in 0..50 {
                let len = 24 + (next() % 48) as i64;
                let src_in = (next() % (src_frames - len) as u64) as i64;
                let mut it =
                    p.make_track_item(item, kind, rate.tick_of(at), TimeRange::new(rate.tick_of(src_in), rate.tick_of(len)), rate).expect("track item");
                // "auto" (NaN) Motion points resolve against the frame, as the edit commands do
                for e in &mut it.effects {
                    filmcraft_project::resolve_auto_points(e, (1920, 1080), (640, 360));
                }
                items.push(it);
                at += len + (next() % 12) as i64;
                clips += 1;
            }
            let q = p.sequence_mut(seq).expect("seq");
            if t < 12 {
                q.video_tracks[t].items = items;
            } else {
                q.audio_tracks[t - 12].items = items;
            }
        }
        st.active_sequence = Some(seq);
        st.open_sequences = vec![seq];
        st.playheads.insert(seq, Tick::ZERO);
        Ok((seq, clips))
    })
    .expect("big sequence")
}

// ------------------------------------------------------------------------------------ timeline UI

struct Ui {
    harness: egui_kittest::Harness<'static, filmcraft_ui_egui::FilmcraftApp>,
    tx: std::sync::mpsc::Sender<filmcraft_ui_egui::control::ControlRequest>,
}

impl Ui {
    fn frame(&mut self) {
        let ctx = self.harness.ctx.clone();
        let mut raw = std::mem::take(self.harness.input_mut());
        eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
        *self.harness.input_mut() = raw;
        self.harness.step();
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = filmcraft_ui_egui::control::ControlRequest::new(method, params.clone());
        self.tx.send(req).expect("control");
        for _ in 0..600 {
            self.frame();
            if let Ok(v) = reply.try_recv() {
                return v;
            }
        }
        panic!("no reply to {method} {params}");
    }
}

/// Timeline UI frame time with a 1000-clip / 20-track sequence: the real app under
/// `egui_kittest` (no window), per frame: app update (layout + painting), tessellation, and the
/// wgpu render of the frame when a GPU adapter exists.
pub fn timeline(o: &Opts) -> Vec<Value> {
    let Some(path) = fixture("clip360.mp4") else {
        eprintln!("timeline: skipped (fixture unavailable)");
        return Vec::new();
    };
    let gpu = o.gpu && Display::new(true).gpu_available();
    let frames = if o.quick { 40 } else { 120 };
    let mut rows = Vec::new();
    for _ in 0..o.repeat {
        let mut s = Session::default();
        let item = playback::import(&mut s, &path);
        let (_, clips) = big_sequence(&mut s, item, "Timeline 1000", 7);
        let (tx, rx) = std::sync::mpsc::channel();
        let app = filmcraft_ui_egui::FilmcraftApp::new(s).with_control(rx);
        let mut b = egui_kittest::Harness::builder().with_size(egui::vec2(1920.0, 1080.0)).with_max_steps(100_000);
        if gpu {
            b = b.wgpu();
        }
        let harness = b.build_eframe(move |_cc| app);
        let mut ui = Ui { harness, tx };
        for _ in 0..8 {
            ui.frame();
        }
        ui.call("ui.set", json!({"timeline": {"videoTrackHeight": 34, "audioTrackHeight": 30}}));
        for (view, setup) in
            [("fit: all 1000 clips", json!({"timeline": {"fit": true}})), ("zoomed, scrolling", json!({"timeline": {"pps": 120.0, "scroll": 0.0}}))]
        {
            ui.call("ui.set", setup);
            // settle the zoom animation, let thumbnails and waveforms arrive
            let t_settle = Instant::now();
            while t_settle.elapsed() < Duration::from_millis(if o.quick { 800 } else { 2500 }) {
                ui.frame();
                std::thread::sleep(Duration::from_millis(5));
            }
            let (mut upd, mut tess, mut rend, mut total) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
            let mut vertices = 0usize;
            let mut shapes = 0usize;
            let scrolling = view.starts_with("zoomed");
            for i in 0..frames {
                // Pointer moving over the timeline (hover feedback), and a scroll step when zoomed.
                let x = 300.0 + (i as f32 * 13.0) % 1500.0;
                ui.harness.hover_at(egui::pos2(x, 820.0));
                if scrolling {
                    let tl = &mut ui.harness.state_mut().ui.timeline;
                    tl.target_scroll += 0.2;
                    tl.scroll = tl.target_scroll;
                }
                let t0 = Instant::now();
                ui.frame();
                let u = ms(t0.elapsed());
                let out = ui.harness.output();
                shapes = out.shapes.len();
                let t1 = Instant::now();
                let prims = ui.harness.ctx.tessellate(out.shapes.clone(), out.pixels_per_point);
                let te = ms(t1.elapsed());
                vertices = prims
                    .iter()
                    .map(|p| match &p.primitive {
                        egui::epaint::Primitive::Mesh(m) => m.vertices.len(),
                        _ => 0,
                    })
                    .sum();
                let r = if gpu {
                    let t2 = Instant::now();
                    let _ = ui.harness.render();
                    Some(ms(t2.elapsed()))
                } else {
                    None
                };
                upd.push(u);
                tess.push(te);
                total.push(u + r.unwrap_or(te));
                if let Some(r) = r {
                    rend.push(r);
                }
            }
            eprintln!("timeline {view}: update p50 {:.2} ms, p95 {:.2} ms", pct(&upd, 0.5), pct(&upd, 0.95));
            rows.push(json!({
                "view": view, "clips": clips, "frames": frames, "gpu": gpu,
                "update_p50_ms": pct(&upd, 0.5), "update_p95_ms": pct(&upd, 0.95),
                "tess_p50_ms": pct(&tess, 0.5), "tess_p95_ms": pct(&tess, 0.95),
                "render_p50_ms": pct(&rend, 0.5), "render_p95_ms": pct(&rend, 0.95),
                "total_p50_ms": pct(&total, 0.5), "total_p95_ms": pct(&total, 0.95),
                "shapes": shapes, "vertices": vertices, "load": load_avg(),
            }));
        }
    }
    rows
}

// ------------------------------------------------------------------------------------ export

/// Export throughput of a 1080p H.264 sequence (clip with AAC audio) to H.264 + AAC and ProRes.
pub fn export(o: &Opts) -> Vec<Value> {
    let Some(path) = fixture("a1080.mp4") else {
        eprintln!("export: skipped (fixture unavailable)");
        return Vec::new();
    };
    let secs = if o.quick { 3.0 } else { 8.0 };
    let dir = std::env::temp_dir().join(format!("filmcraft-bench-export-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("tmp dir");
    let mut rows = Vec::new();
    for (format, label, ext) in [("h264", "H.264 + AAC (MP4)", "mp4"), ("prores", "ProRes 422 (MOV)", "mov")].into_iter().filter(|f| o.wants(f.0)) {
        let mut runs = Vec::new();
        let mut cpu = Vec::new();
        let mut bytes = 0u64;
        let mut frames = 0i64;
        for _ in 0..o.repeat {
            let mut s = Session::default();
            let a = playback::import(&mut s, &path);
            let seq = playback::build_sequence(&mut s, 1920, 1080, &[(a, 100.0, None, 0.0, 100.0)], &[], secs);
            // build_sequence adds video only: add the clip's audio on A1 for the AAC encode
            s.edit("audio", |p, _| {
                let rate = FrameRate::FPS_23_976;
                let it = p
                    .make_track_item(a, TrackKind::Audio, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.snap(Tick::from_seconds_f64(secs))), rate)
                    .expect("audio item");
                p.sequence_mut(seq).expect("seq").audio_tracks[0].items.push(it);
                Ok(())
            })
            .expect("audio");
            frames = s.project.sequence(seq).map(|q| q.settings.frame_rate.frame_at(q.duration())).unwrap_or(0);
            let out = dir.join(format!("out.{ext}"));
            let (t0, c0) = (Instant::now(), cpu_now());
            let r = s.execute("file.exportMedia", json!({"path": out.to_string_lossy(), "format": format, "wait": true}));
            let dt = t0.elapsed().as_secs_f64();
            if let Err(e) = r {
                eprintln!("export {format}: {e}");
                break;
            }
            if let Some(e) = s.jobs.last().and_then(|j| j.progress.error.lock().ok().and_then(|g| g.clone())) {
                eprintln!("export {format}: {e}");
                break;
            }
            runs.push(dt);
            cpu.push(ms(cpu_now() - c0) / frames.max(1) as f64);
            bytes = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
        }
        if runs.is_empty() {
            continue;
        }
        let best = runs.iter().cloned().fold(f64::MAX, f64::min);
        eprintln!("export {format}: {frames} frames in {best:.1} s");
        rows.push(json!({
            "format": label, "frames": frames, "seconds": best, "fps": frames as f64 / best,
            "realtime": frames as f64 / best / FrameRate::FPS_23_976.as_f64(), "cpu_ms_per_frame": median(&cpu),
            "mbytes": bytes as f64 / 1e6, "runs_s": runs, "load": load_avg(),
        }));
    }
    let _ = std::fs::remove_dir_all(&dir);
    rows
}

// ------------------------------------------------------------------------------------ project

/// Save and open a large project: 5 sequences × 1000 clips (20 tracks each).
pub fn project(o: &Opts) -> Vec<Value> {
    let Some(path) = fixture("clip360.mp4") else {
        eprintln!("project: skipped (fixture unavailable)");
        return Vec::new();
    };
    let mut s = Session::default();
    let item = playback::import(&mut s, &path);
    let mut clips = 0;
    for k in 0..5 {
        clips += big_sequence(&mut s, item, &format!("Sequence {}", k + 1), 11 + k as u64).1;
    }
    let dir = std::env::temp_dir().join(format!("filmcraft-bench-project-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("tmp dir");
    let file = dir.join("big.fcproj");
    let n = o.repeat.max(3);
    let (mut save, mut open) = (Vec::new(), Vec::new());
    for _ in 0..n {
        let t0 = Instant::now();
        s.execute("file.saveAs", json!({"path": file.to_string_lossy()})).expect("save");
        save.push(ms(t0.elapsed()));
        let mut s2 = Session::default();
        let t1 = Instant::now();
        s2.execute("file.open", json!({"path": file.to_string_lossy()})).expect("open");
        open.push(ms(t1.elapsed()));
        assert_eq!(s2.project.items.len(), s.project.items.len());
    }
    let mbytes = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0) as f64 / 1e6;
    let _ = std::fs::remove_dir_all(&dir);
    eprintln!("project: save {:.0} ms, open {:.0} ms ({clips} clips, {mbytes:.1} MB)", median(&save), median(&open));
    let row = |what: &str, v: &[f64]| json!({"what": what, "clips": clips, "mbytes": mbytes, "p50_ms": median(v), "min_ms": v.iter().cloned().fold(f64::MAX, f64::min), "runs_ms": v, "load": load_avg()});
    vec![row("save (file.saveAs)", &save), row("open (file.open)", &open)]
}
