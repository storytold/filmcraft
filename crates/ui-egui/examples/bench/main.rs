//! FilmCraft benchmark suite (`cargo xtask bench`): decode speed per codec, Program-monitor
//! playback and scrubbing through the real frame scheduler, timeline UI frame time with a large
//! sequence, export throughput, project save/open, and peak memory per section.
//!
//! ```sh
//! cargo xtask bench                                   # every section, writes target/bench/bench-<label>.{json,md}
//! cargo xtask bench --sections decode,scrub --repeat 3 --label after
//! cargo xtask bench --sections decode --hw off --label hw-off   # FilmCraft's own decoders only
//! cargo run --release -p filmcraft-ui-egui --example bench -- --section timeline   # one section, this process
//! ```
//!
//! Each section runs in its own child process (fresh caches, and its own peak resident set size,
//! measured with `/usr/bin/time`). Wall-clock results depend on machine load, so every row
//! records the load average, and CPU-time columns are reported alongside. Fixtures are generated
//! with ffmpeg into `target/fixtures/playback/` on first use (see `fixtures.rs`); sections whose
//! fixtures cannot be made are skipped. See `docs/performance.md`.

mod fixtures;
#[allow(dead_code)]
mod playback;
mod sections;

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

pub const SECTIONS: &[&str] = &["decode", "seek", "playback", "scrub", "timeline", "export", "project"];

#[derive(Clone)]
pub struct Opts {
    pub repeat: usize,
    pub quick: bool,
    pub gpu: bool,
    /// Only cases whose name contains this (decode fixtures, playback / scrub scenarios).
    pub only: Option<String>,
    /// Settings ▸ Playback ▸ Hardware decoding: `auto` (default, as the app) or `off`.
    pub hw: String,
}

impl Opts {
    pub fn wants(&self, name: &str) -> bool {
        self.only.as_ref().is_none_or(|o| name.contains(o.as_str()))
    }
}

fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "bench [--sections {}] [--repeat N] [--quick] [--cpu] [--only SUBSTRING] [--hw auto|off] [--label NAME] [--out DIR]\n       bench --section NAME [--json FILE]   (one section in this process)",
            SECTIONS.join(",")
        );
        return;
    }
    let opts = Opts {
        repeat: arg_value(&args, "--repeat").and_then(|v| v.parse().ok()).unwrap_or(1).max(1),
        quick: args.iter().any(|a| a == "--quick"),
        gpu: !args.iter().any(|a| a == "--cpu"),
        only: arg_value(&args, "--only"),
        hw: arg_value(&args, "--hw").unwrap_or_else(|| "auto".into()),
    };
    // as the desktop app: OS hardware decoders in front of ours, unless Hardware decoding is Off
    filmcraft_platform::register();
    filmcraft_codecs::hw::set_hardware_decoding(opts.hw != "off");
    if let Some(section) = arg_value(&args, "--section") {
        let v = run_section(&section, &opts);
        let text = serde_json::to_string_pretty(&v).unwrap_or_default();
        match arg_value(&args, "--json") {
            Some(p) => std::fs::write(&p, text).unwrap_or_else(|e| panic!("{p}: {e}")),
            None => println!("{text}"),
        }
        return;
    }
    orchestrate(&args, &opts);
}

fn run_section(name: &str, o: &Opts) -> Value {
    let t0 = std::time::Instant::now();
    let load0 = playback::load_avg();
    let rows = match name {
        "decode" => sections::decode(o),
        "seek" => sections::seek(o),
        "playback" => sections::playback(o),
        "scrub" => sections::scrub(o),
        "timeline" => sections::timeline(o),
        "export" => sections::export(o),
        "project" => sections::project(o),
        _ => panic!("unknown section {name} (one of {})", SECTIONS.join(", ")),
    };
    json!({"section": name, "rows": rows, "wall_s": t0.elapsed().as_secs_f64(), "load_before": load0, "load_after": playback::load_avg()})
}

/// Run each section in a child process under `/usr/bin/time` (peak RSS), then write JSON and
/// Markdown reports.
fn orchestrate(args: &[String], o: &Opts) {
    let wanted: Vec<String> =
        arg_value(args, "--sections").map(|s| s.split(',').map(str::to_string).collect()).unwrap_or_else(|| SECTIONS.iter().map(|s| s.to_string()).collect());
    let label = arg_value(args, "--label").unwrap_or_else(|| "latest".into());
    let out = arg_value(args, "--out").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/bench"));
    std::fs::create_dir_all(&out).expect("output dir");
    let exe = std::env::current_exe().expect("exe");
    let machine = machine_info();
    println!("FilmCraft bench `{label}`: {}", machine["summary"].as_str().unwrap_or(""));
    let mut results = Vec::new();
    for name in &wanted {
        let tmp = out.join(format!(".section-{name}-{}.json", std::process::id()));
        let mut cmd = time_wrapper();
        cmd.arg(&exe).args(["--section", name, "--json"]).arg(&tmp).args(["--repeat", &o.repeat.to_string()]);
        if o.quick {
            cmd.arg("--quick");
        }
        if !o.gpu {
            cmd.arg("--cpu");
        }
        if let Some(only) = &o.only {
            cmd.args(["--only", only]);
        }
        cmd.args(["--hw", &o.hw]);
        eprintln!("== {name} (load {})", playback::load_avg());
        let output = cmd.stderr(std::process::Stdio::piped()).output().expect("run section");
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        // pass the child's progress output through (minus the time report)
        for l in child_lines(&stderr) {
            eprintln!("  {l}");
        }
        let mut v: Value = std::fs::read(&tmp)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_else(|| json!({"section": name, "error": format!("section failed: {}", output.status)}));
        let _ = std::fs::remove_file(&tmp);
        if let Some(rss) = peak_rss_bytes(&stderr) {
            v["peak_rss_mb"] = json!(rss as f64 / 1e6);
        }
        println!("{}", section_markdown(&v));
        results.push(v);
    }
    let doc = json!({"label": label, "machine": machine, "repeat": o.repeat, "quick": o.quick, "gpu": o.gpu, "hw": o.hw, "sections": results});
    let jp = out.join(format!("bench-{label}.json"));
    let mp = out.join(format!("bench-{label}.md"));
    std::fs::write(&jp, serde_json::to_string_pretty(&doc).unwrap_or_default()).expect("write json");
    std::fs::write(&mp, markdown(&doc)).expect("write md");
    println!("wrote {} and {}", jp.display(), mp.display());
}

fn time_wrapper() -> Command {
    let time = Path::new("/usr/bin/time");
    if time.exists() {
        let mut c = Command::new(time);
        c.arg(if cfg!(target_os = "macos") { "-l" } else { "-v" });
        c
    } else {
        // No `time`: run the section directly (no peak RSS).
        let mut c = Command::new("env");
        c.arg("--");
        c
    }
}

/// The child's own output: everything before the `time` report (macOS `-l`: a "… real … user …"
/// line; GNU `-v`: "Command being timed").
fn child_lines(stderr: &str) -> Vec<&str> {
    stderr.lines().take_while(|l| !(l.trim_start().starts_with("Command being timed") || (l.contains(" real ") && l.contains(" user ")))).collect()
}

/// Peak RSS in bytes from `/usr/bin/time -l` (macOS: bytes) or `-v` (GNU: kbytes).
fn peak_rss_bytes(stderr: &str) -> Option<u64> {
    for l in stderr.lines() {
        let t = l.trim();
        if let Some(n) = t.strip_suffix("maximum resident set size") {
            return n.trim().parse().ok();
        }
        if let Some(n) = t.strip_prefix("Maximum resident set size (kbytes):") {
            return n.trim().parse::<u64>().ok().map(|k| k * 1024);
        }
    }
    None
}

fn sh(cmd: &str, args: &[&str]) -> Option<String> {
    Command::new(cmd).args(args).output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn machine_info() -> Value {
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let cpu = sh("sysctl", &["-n", "machdep.cpu.brand_string"])
        .or_else(|| {
            std::fs::read_to_string("/proc/cpuinfo")
                .ok()
                .and_then(|c| c.lines().find(|l| l.starts_with("model name")).and_then(|l| l.split(':').nth(1)).map(|s| s.trim().to_string()))
        })
        .or_else(|| sh("powershell", &["-NoProfile", "-Command", "(Get-CimInstance Win32_Processor | Select-Object -First 1).Name"]))
        .unwrap_or_else(|| "unknown CPU".into());
    let mem_gb = sh("sysctl", &["-n", "hw.memsize"]).and_then(|m| m.parse::<u64>().ok()).map(|b| b as f64 / (1u64 << 30) as f64);
    let os = sh("uname", &["-sr"]).unwrap_or_default();
    let date = sh("date", &["-u", "+%Y-%m-%d %H:%M UTC"]).unwrap_or_default();
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let commit = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let summary = format!(
        "{cpu}, {cores} cores{}, {os}, commit {commit}, {date}, load {}",
        mem_gb.map(|g| format!(", {g:.0} GB")).unwrap_or_default(),
        playback::load_avg()
    );
    json!({"cpu": cpu, "cores": cores, "memory_gb": mem_gb, "os": os, "commit": commit, "date": date, "rayon_threads": rayon::current_num_threads(), "load": playback::load_avg(), "summary": summary})
}

// ------------------------------------------------------------------------------------ markdown

fn f(v: &Value, k: &str, prec: usize) -> String {
    match v.get(k) {
        Some(Value::Number(n)) => match n.as_f64() {
            Some(x) if x.is_finite() => format!("{x:.prec$}"),
            _ => "–".into(),
        },
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => "–".into(),
    }
}

/// One section as a Markdown table.
pub fn section_markdown(v: &Value) -> String {
    let name = v["section"].as_str().unwrap_or("?");
    let rows = v["rows"].as_array().cloned().unwrap_or_default();
    let mut s = format!("### {name}\n\n");
    if let Some(e) = v.get("error") {
        s += &format!("failed: {e}\n");
        return s;
    }
    let (cols, prec): (&[(&str, &str)], usize) = match name {
        "decode" => (
            &[
                ("fixture", "fixture"),
                ("codec", "codec"),
                ("size", "size"),
                ("frames", "frames"),
                ("fps", "fps"),
                ("fps_min", "fps (min)"),
                ("cpu_ms_per_frame", "CPU ms/frame"),
                ("first_frame_ms", "first frame ms"),
                ("realtime", "× real time"),
                ("hw_frames", "hw frames"),
                ("hw_sessions", "hw sessions"),
                ("hw_fallbacks", "hw fallbacks"),
                ("load", "load"),
            ],
            1,
        ),
        "playback" => (
            &[
                ("scenario", "scenario"),
                ("res", "res"),
                ("path", "path"),
                ("shown", "shown"),
                ("dropped", "dropped"),
                ("drop_pct", "drop %"),
                ("cpu_ms_per_frame", "CPU ms/frame"),
                ("decode_ms", "decode ms/job"),
                ("ui_p95_ms", "UI p95 ms"),
                ("seeks", "seeks"),
                ("skipped", "skipped"),
                ("draft_frames", "draft"),
                ("hw_frames", "hw frames"),
                ("hw_sessions", "hw sessions"),
                ("hw_fallbacks", "hw fallbacks"),
                ("hw_zero_copy", "zero-copy frames"),
                ("load", "load"),
            ],
            1,
        ),
        "seek" => (
            &[
                ("fixture", "fixture"),
                ("mode", "mode"),
                ("seeks", "seeks"),
                ("p50_ms", "p50 ms"),
                ("p95_ms", "p95 ms"),
                ("cpu_ms_mean", "CPU ms/seek"),
                ("decoded_per_seek", "decoded/seek"),
                ("skipped_per_seek", "skipped/seek"),
                ("load", "load"),
            ],
            1,
        ),
        "scrub" => (
            &[
                ("scenario", "scenario"),
                ("seek_p50_ms", "seek p50 ms"),
                ("seek_p95_ms", "seek p95 ms"),
                ("settle_p50_ms", "drag-stop p50 ms"),
                ("settle_p95_ms", "drag-stop p95 ms"),
                ("drag_shown_pct", "drag frames shown %"),
                ("timeouts", "timeouts"),
                ("decoded_per_seek", "decoded/seek"),
                ("load", "load"),
            ],
            1,
        ),
        "timeline" => (
            &[
                ("view", "view"),
                ("clips", "clips"),
                ("frames", "frames"),
                ("update_p50_ms", "update p50 ms"),
                ("update_p95_ms", "update p95 ms"),
                ("tess_p50_ms", "tessellate p50 ms"),
                ("render_p50_ms", "wgpu render p50 ms"),
                ("total_p50_ms", "total p50 ms"),
                ("total_p95_ms", "total p95 ms"),
                ("vertices", "vertices"),
                ("load", "load"),
            ],
            2,
        ),
        "export" => (
            &[
                ("format", "format"),
                ("frames", "frames"),
                ("seconds", "wall s"),
                ("fps", "fps"),
                ("realtime", "× real time"),
                ("cpu_ms_per_frame", "CPU ms/frame"),
                ("mbytes", "MB"),
                ("load", "load"),
            ],
            2,
        ),
        "project" => (&[("what", "operation"), ("clips", "clips"), ("mbytes", "file MB"), ("p50_ms", "p50 ms"), ("min_ms", "min ms"), ("load", "load")], 1),
        _ => (&[], 1),
    };
    s += &format!("| {} |\n|{}|\n", cols.iter().map(|c| c.1).collect::<Vec<_>>().join(" | "), cols.iter().map(|_| "---").collect::<Vec<_>>().join("|"));
    for r in &rows {
        s += &format!("| {} |\n", cols.iter().map(|c| f(r, c.0, prec)).collect::<Vec<_>>().join(" | "));
    }
    if let Some(m) = v.get("peak_rss_mb") {
        s += &format!(
            "\npeak RSS {:.0} MB; section wall {:.0} s; load {} → {}\n",
            m.as_f64().unwrap_or(0.0),
            v["wall_s"].as_f64().unwrap_or(0.0),
            f(v, "load_before", 0),
            f(v, "load_after", 0)
        );
    }
    s
}

fn markdown(doc: &Value) -> String {
    let m = &doc["machine"];
    let mut s = format!(
        "# FilmCraft benchmark `{}`\n\n- {}\n- repeat {} (best/median as noted), quick {}, monitor path {}\n\n",
        doc["label"].as_str().unwrap_or(""),
        m["summary"].as_str().unwrap_or(""),
        doc["repeat"],
        doc["quick"],
        if doc["gpu"].as_bool() == Some(true) { "GPU" } else { "CPU" }
    );
    for sec in doc["sections"].as_array().into_iter().flatten() {
        s += &section_markdown(sec);
        s += "\n";
    }
    s
}
