//! Decode throughput and per-stage breakdown (ignored; run with
//! `cargo test --release -p filmcraft-av1 --test it perf:: -- --ignored --nocapture`).
//!
//! The machine may be busy (other builds), so single-threaded work is also measured with the
//! thread CPU clock, which is robust to load; wall-clock figures are printed next to the load
//! average.

use crate::common;
use common::*;
use filmcraft_av1::{DecodeStats, Decoder, Stage};
use std::time::Instant;

fn thread_cpu_secs() -> Option<f64> {
    #[cfg(unix)]
    {
        let t = rustix::time::clock_gettime(rustix::time::ClockId::ThreadCPUTime);
        Some(t.tv_sec as f64 + t.tv_nsec as f64 * 1e-9)
    }
    #[cfg(not(unix))]
    {
        None
    }
}

fn load_average() -> String {
    let out = std::process::Command::new("sysctl").args(["-n", "vm.loadavg"]).output().ok();
    if let Some(o) = out.filter(|o| o.status.success()) {
        return String::from_utf8_lossy(&o.stdout).trim().trim_matches(|c| c == '{' || c == '}').trim().to_string();
    }
    std::fs::read_to_string("/proc/loadavg").map(|s| s.split_whitespace().take(3).collect::<Vec<_>>().join(" ")).unwrap_or_else(|_| "?".into())
}

struct Run {
    wall: f64,
    cpu: Option<f64>,
    shown: usize,
    stats: DecodeStats,
}

fn run(frames: &[Vec<u8>], threads: usize) -> Run {
    let mut dec = Decoder::with_threads(threads);
    let t = Instant::now();
    let c0 = thread_cpu_secs();
    let mut shown = 0;
    for f in frames {
        shown += dec.decode(f).unwrap().len();
    }
    shown += dec.flush().len();
    let cpu = c0.and_then(|c0| thread_cpu_secs().map(|c1| c1 - c0));
    Run { wall: t.elapsed().as_secs_f64(), cpu, shown, stats: dec.stats() }
}

fn bench(spec: &Spec) {
    // AV1_PERF_ONLY=<substring>[,<substring>...] limits the run to matching fixtures.
    if std::env::var("AV1_PERF_ONLY").is_ok_and(|f| !f.split(',').any(|f| spec.name.contains(f))) {
        return;
    }
    let Some(ff) = ffmpeg() else { return };
    let path = make(&ff, spec);
    let frames = ivf_frames(&path);
    // Single-threaded: best of 3 by thread CPU time (falls back to wall time).
    let mut best: Option<Run> = None;
    for _ in 0..3 {
        let r = run(&frames, 1);
        let key = |r: &Run| r.cpu.unwrap_or(r.wall);
        if best.as_ref().is_none_or(|b| key(&r) < key(b)) {
            best = Some(r);
        }
    }
    let b = best.unwrap();
    let cpu_fps = b.cpu.map(|c| format!("{:6.1}", b.shown as f64 / c)).unwrap_or_else(|| "     ?".into());
    println!(
        "{:<26} {:>3} shown {:>3} decoded {:>3} tiles/frame | 1 thread: {} fps (thread CPU) {:6.1} fps (wall)",
        spec.name,
        b.shown,
        b.stats.frames,
        b.stats.tiles / b.stats.frames.max(1),
        cpu_fps,
        b.shown as f64 / b.wall
    );
    println!("{}", stage_line("    ms/frame:", &b.stats));
    // All cores: best of 3 by wall time.
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let mut best: Option<Run> = None;
    for _ in 0..3 {
        let r = run(&frames, threads);
        assert_eq!(r.shown, b.shown);
        if best.as_ref().is_none_or(|b| r.wall < b.wall) {
            best = Some(r);
        }
    }
    let m = best.unwrap();
    println!("    {threads} threads: {:6.1} fps (wall, load average {})", b.shown as f64 / m.wall, load_average());
    // Per-stage wall time on the frame workers (stages of different frames overlap).
    println!("{}", stage_line(&format!("    {threads} threads, ms/frame:"), &m.stats));
}

/// Per-stage milliseconds per frame and share of the total.
fn stage_line(label: &str, stats: &DecodeStats) -> String {
    let n = stats.frames.max(1) as f64;
    let total = stats.total_secs();
    let mut line = String::from(label);
    for s in Stage::ALL {
        let v = stats.secs(s);
        if v > 0.0005 {
            line += &format!(" {} {:.2} ({:.0}%)", s.name(), 1e3 * v / n, 100.0 * v / total);
        }
    }
    line
}

#[test]
#[ignore]
fn decode_throughput() {
    let p = |crf: &'static str, params: &'static str| -> Vec<&'static str> { vec!["-preset", "8", "-crf", crf, "-svtav1-params", params] };
    let src = "testsrc2=s=1920x1080:r=25,noise=alls=6:allf=t";
    bench(&Spec::new("perf_1080p_intra", src, 10, "yuv420p", &p("30", "keyint=1")));
    bench(&Spec::new("perf_1080p_gop", src, 60, "yuv420p", &p("30", "keyint=60")));
    bench(&Spec::new("perf_1080p_gop_10bit", src, 60, "yuv420p10le", &p("30", "keyint=60")));
    // Higher quality (more coefficients, finer partitions) and a multi-tile stream (4x2 tiles).
    bench(&Spec::new("perf_1080p_gop_hq", src, 60, "yuv420p", &p("18", "keyint=60")));
    bench(&Spec::new("perf_1080p_gop_tiles", src, 60, "yuv420p", &p("30", "keyint=60:tile-columns=2:tile-rows=1")));
    // Low delay (each frame references the previous one, as screen and camera recorders encode):
    // frames cannot overlap, so a frame's own parallelism decides the speed.
    bench(&Spec::new("perf_1080p_lowdelay", src, 60, "yuv420p", &p("30", "keyint=60:pred-struct=1")));
    // One tile at 2160p: the post-filters' row bands are most of the parallelism there.
    bench(&Spec::new("perf_2160p_gop", "testsrc2=s=3840x2160:r=25,noise=alls=6:allf=t", 30, "yuv420p", &p("30", "keyint=30")));
}
