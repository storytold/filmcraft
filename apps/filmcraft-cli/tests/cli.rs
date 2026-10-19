//! End-to-end tests of the CLI binary (headless backend).

use std::io::Write;
use std::process::{Command, Output, Stdio};

use serde_json::Value;

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_filmcraft-cli")).args(args).output().expect("spawn filmcraft-cli")
}

fn json_out(o: &Output) -> Value {
    assert!(o.status.success(), "failed: {}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout).expect("JSON on stdout")
}

#[test]
fn help_and_usage_errors() {
    let h = cli(&["help"]);
    assert!(h.status.success());
    assert!(String::from_utf8_lossy(&h.stdout).contains("exec <id>"));
    assert_eq!(cli(&[]).status.code(), Some(2));
    assert_eq!(cli(&["frobnicate"]).status.code(), Some(2));
    assert_eq!(cli(&["exec", "file.newBin", "notkv"]).status.code(), Some(2));
    // Invalid --bridge is a usage error (exit 2) for both exec and mcp (#921).
    assert_eq!(cli(&["--bridge", "0.0.0.0:9876", "exec", "inspect.project"]).status.code(), Some(2));
    assert_eq!(cli(&["mcp", "--bridge", "0.0.0.0:9876"]).status.code(), Some(2));
}

#[test]
fn version_after_global_options() {
    let want = format!("filmcraft-cli {}\n", env!("CARGO_PKG_VERSION"));
    for args in [&["--version"][..], &["-V"], &["version"], &["--demo", "--version"], &["--compact", "-V"], &["--demo", "version"]] {
        let o = cli(args);
        assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
        assert_eq!(String::from_utf8_lossy(&o.stdout), want, "{args:?}");
    }
}

#[test]
fn long_help_matches_help_command() {
    let reference = cli(&["help"]);
    assert!(reference.status.success());
    let long_help = cli(&["--help"]);
    assert!(long_help.status.success(), "--help failed: {}", String::from_utf8_lossy(&long_help.stderr));
    assert_eq!(long_help.stdout, reference.stdout);
    assert!(long_help.stderr.is_empty());
    let short_help = cli(&["-h"]);
    assert!(short_help.status.success());
    assert_eq!(short_help.stdout, reference.stdout);
    let sub_help = cli(&["export", "--help"]);
    assert!(sub_help.status.success(), "export --help failed: {}", String::from_utf8_lossy(&sub_help.stderr));
    assert_eq!(sub_help.stdout, reference.stdout);
}

#[test]
fn probe_reports_mpeg_transport_and_program_streams() {
    let Some(ffmpeg) = ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg", "/usr/bin/ffmpeg"].into_iter().find(|p| std::path::Path::new(p).exists()) else {
        eprintln!("SKIPPED (probe_reports_mpeg_transport_and_program_streams): ffmpeg not found");
        return;
    };
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/cli-tests").join(format!("mpeg-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, fmt, extra) in [("a.ts", "mpegts", &["-mpegts_m2ts_mode", "0"][..]), ("b.vob", "vob", &[][..])] {
        let out = dir.join(name);
        let st = Command::new(ffmpeg)
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=25", "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000"])
            .args(["-t", "0.5", "-c:v", "mpeg2video", "-c:a", "mp2", "-ac", "2", "-f", fmt])
            .args(extra)
            .arg(&out)
            .status()
            .unwrap();
        assert!(st.success());
        let v = json_out(&cli(&["probe", out.to_str().unwrap()]));
        assert!(v["video"]["codec"].as_str().unwrap().starts_with("MPEG-2 Video"), "{v}");
        assert_eq!(v["audio_streams"][0]["codec"], "MPEG Audio");
        let streams = v["mpeg"]["streams"].as_array().unwrap();
        assert_eq!(streams.len(), 2, "{v}");
        assert_eq!(streams[0]["codec"], "MPEG-2 Video");
        assert!(streams[0]["pictures"]["I"].as_u64().unwrap() >= 1);
        assert_eq!(v["mpeg"]["format"], if fmt == "mpegts" { "MPEG-2 TS" } else { "MPEG-2 PS" });
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn exec_inspect_and_describe() {
    let seq = json_out(&cli(&["--demo", "inspect", "sequence", "--compact"]));
    assert!(seq["tracks"].is_array() || seq.is_object(), "{seq}");
    let d = json_out(&cli(&["describe", "timeline.razor"]));
    assert_eq!(d["id"], "timeline.razor");
    assert!(d["params"].is_string());
    let list = json_out(&cli(&["commands", "razor", "--json"]));
    assert!(list.as_array().unwrap().iter().any(|c| c["id"] == "timeline.razor"));
    // An unknown command fails with exit status 1.
    let bad = cli(&["--demo", "exec", "no.such.command"]);
    assert_eq!(bad.status.code(), Some(1));
}

#[test]
fn exec_save_as_then_reopen() {
    let dir = std::env::temp_dir().join(format!("fc-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let proj = dir.join("p.fcproj");
    let p = proj.to_str().unwrap();
    json_out(&cli(&["--demo", "exec", "file.newBin", "name=CLI Selects", "--save-as", p]));
    let tree = json_out(&cli(&["--project", p, "inspect", "project"]));
    assert!(tree.to_string().contains("CLI Selects"), "{tree}");
    // `run` from stdin, editing and saving back in place.
    let mut child = Command::new(env!("CARGO_BIN_EXE_filmcraft-cli"))
        .args(["--project", p, "--save", "run", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"# comment\n{\"id\":\"file.newBin\",\"params\":{\"name\":\"Second\"}}\n").unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let line: Value = serde_json::from_slice(out.stdout.split(|b| *b == b'\n').next().unwrap()).unwrap();
    assert_eq!(line["ok"], true);
    assert!(json_out(&cli(&["--project", p, "inspect", "project"])).to_string().contains("Second"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn run_keep_going_reports_failures() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_filmcraft-cli"))
        .args(["--demo", "--keep-going", "run", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"{\"id\":\"no.such\"}\n{\"id\":\"file.newBin\",\"params\":{\"name\":\"x\"}}\n").unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let lines: Vec<Value> = String::from_utf8_lossy(&out.stdout).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["ok"], false);
    assert_eq!(lines[1]["ok"], true);
}

#[test]
fn export_wav_waits_for_job() {
    let dir = std::env::temp_dir().join(format!("fc-cli-x-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("still.wav");
    let o = cli(&["--demo", "export", out.to_str().unwrap()]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(std::fs::metadata(&out).map(|m| m.len() > 44).unwrap_or(false));
    let _ = std::fs::remove_dir_all(&dir);
}

/// `export --preset`: a built-in preset by name (dash or en dash), a custom range, a user preset
/// from `--data-dir`, `--list-presets`, `--queue`, and a clean failure for an unknown preset.
#[test]
fn export_with_presets() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/export-tests").join(format!("cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let data = dir.join("data");
    let data_s = data.to_str().unwrap();
    let out = dir.join("proxy.mov");
    let o = cli(&["--demo", "--data-dir", data_s, "export", out.to_str().unwrap(), "--preset", "Apple ProRes 422 Proxy", "--start", "0", "--end", "0.25"]);
    let v = json_out(&o);
    assert_eq!(v["preset"], "Apple ProRes 422 Proxy");
    assert!(std::fs::metadata(&out).map(|m| m.len() > 1000).unwrap_or(false));
    if let Some(ffprobe) = ["/opt/homebrew/bin/ffprobe", "/usr/local/bin/ffprobe", "/usr/bin/ffprobe"].into_iter().find(|p| std::path::Path::new(p).exists()) {
        let p = Command::new(ffprobe)
            .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=codec_name,profile,width", "-of", "csv=p=0", out.to_str().unwrap()])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&p.stdout).trim(), "prores,Proxy,1920");
    }
    // the extension comes from the preset; a hyphen finds the en-dash name
    let base = dir.join("adaptive");
    json_out(&cli(&[
        "--demo",
        "--data-dir",
        data_s,
        "export",
        base.to_str().unwrap(),
        "--preset",
        "Match Source - Adaptive Low Bitrate",
        "--start",
        "0",
        "--end",
        "0.2",
    ]));
    assert!(dir.join("adaptive.mp4").exists());
    // a user preset saved into the data directory is found by later runs
    json_out(&cli(&[
        "--data-dir",
        data_s,
        "exec",
        "export.presets.save",
        "name=Tiny WAV",
        "from=Waveform Audio 48 kHz 16-bit",
        "settings={\"audio\":{\"channels\":1}}",
    ]));
    let list = json_out(&cli(&["--data-dir", data_s, "export", "--list-presets", "tiny"]));
    assert_eq!(list.as_array().unwrap().len(), 1, "{list}");
    let wav = dir.join("tiny.wav");
    json_out(&cli(&["--demo", "--data-dir", data_s, "export", wav.to_str().unwrap(), "--preset", "Tiny WAV", "--start", "0", "--end", "0.5"]));
    let b = std::fs::read(&wav).unwrap();
    assert_eq!(u16::from_le_bytes([b[22], b[23]]), 1, "mono from the user preset");
    // through the queue
    let q = dir.join("queued.wav");
    let v = json_out(&cli(&["--demo", "--data-dir", data_s, "export", q.to_str().unwrap(), "--preset", "Tiny WAV", "--queue", "--range", "entire"]));
    assert_eq!(v["items"][0]["status"], "done", "{v}");
    assert!(q.exists());
    // a format that changes the extension: the completion message names the file written (#341)
    let apv = dir.join("apv.mp4");
    let o =
        cli(&["--demo", "--data-dir", data_s, "export", apv.to_str().unwrap(), "--preset", "APV 422-10", "--start", "0", "--end", "0.1", "--scale", "0.125"]);
    let v = json_out(&o);
    let written = v["path"].as_str().unwrap().to_string();
    assert!(written.ends_with("apv.mp4.mov") && std::path::Path::new(&written).exists(), "{v}");
    let msg = String::from_utf8_lossy(&o.stderr);
    assert!(msg.contains(&format!("exported {written} in ")), "{msg}");
    let qa = dir.join("queued-apv.mp4");
    let o = cli(&[
        "--demo",
        "--data-dir",
        data_s,
        "export",
        qa.to_str().unwrap(),
        "--preset",
        "APV 422-10",
        "--queue",
        "--start",
        "0",
        "--end",
        "0.1",
        "--scale",
        "0.125",
    ]);
    let v = json_out(&o);
    let written = v["items"][0]["path"].as_str().unwrap().to_string();
    assert!(written.ends_with("queued-apv.mp4.mov"), "{v}");
    assert!(String::from_utf8_lossy(&o.stderr).contains(&format!("exported {written} in ")), "{}", String::from_utf8_lossy(&o.stderr));
    // unknown preset: exit status 1 and a message naming it
    let bad = cli(&["--demo", "--data-dir", data_s, "export", dir.join("x.mp4").to_str().unwrap(), "--preset", "No Such Preset"]);
    assert_eq!(bad.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("No Such Preset"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// `filmcraft-cli commands | head -c 10` panicked with "failed printing to stdout: Broken pipe
/// (os error 32)" and exit status 101 (#285).
#[test]
fn closed_stdout_ends_quietly() {
    use std::io::Read;
    for args in [&["commands"][..], &["help"], &["--demo", "commands", "--json"]] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_filmcraft-cli")).args(args).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        let mut first = [0u8; 10];
        child.stdout.as_mut().unwrap().read_exact(&mut first).unwrap();
        drop(child.stdout.take());
        let out = child.wait_with_output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success() && err.is_empty(), "{args:?}: {:?}: {err}", out.status);
    }
}

/// `run -` with stdout already closed: `run` reads its script to the end before it prints, so its
/// first write fails for sure. The rest of the script and the save still happen, and the exit status
/// still reports a failing command.
fn run_with_closed_stdout(args: &[&str], script: &[u8]) -> Output {
    let mut child =
        Command::new(env!("CARGO_BIN_EXE_filmcraft-cli")).args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    drop(child.stdout.take());
    child.stdin.take().unwrap().write_all(script).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn closed_stdout_still_runs_the_script_and_saves() {
    let dir = std::env::temp_dir().join(format!("fc-cli-pipe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let proj = dir.join("p.fcproj");
    let p = proj.to_str().unwrap();
    let script = b"{\"id\":\"file.newBin\",\"params\":{\"name\":\"Piped One\"}}\n{\"id\":\"file.newBin\",\"params\":{\"name\":\"Piped Two\"}}\n";
    let out = run_with_closed_stdout(&["--demo", "--save-as", p, "run", "-"], script);
    assert!(out.status.success(), "{:?}: {}", out.status, String::from_utf8_lossy(&out.stderr));
    let tree = json_out(&cli(&["--project", p, "inspect", "project"])).to_string();
    assert!(tree.contains("Piped One") && tree.contains("Piped Two"), "{tree}");

    // a failing command is still a failure, whether or not anyone reads stdout
    let out = run_with_closed_stdout(&["--demo", "run", "-"], b"{\"id\":\"no.such.command\"}\n");
    assert_eq!(out.status.code(), Some(1));
    let out = run_with_closed_stdout(
        &["--demo", "--keep-going", "run", "-"],
        b"{\"id\":\"no.such.command\"}\n{\"id\":\"file.newBin\",\"params\":{\"name\":\"x\"}}\n",
    );
    assert_eq!(out.status.code(), Some(1));
    let _ = std::fs::remove_dir_all(&dir);
}

/// `render` with a non-numeric or non-finite `--seconds` / `--scale` is a usage error (exit 2) and
/// writes nothing, instead of silently rendering frame 0 at full scale.
#[test]
fn render_rejects_bad_numbers() {
    let dir = std::env::temp_dir().join(format!("fc-cli-render-nan-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let still = dir.join("s.png").to_str().unwrap().to_string();
    for (opt, bad) in
        [("--seconds", "1s"), ("--seconds", "1,5"), ("--seconds", "abc"), ("--seconds", "NaN"), ("--seconds", "inf"), ("--scale", "xyz"), ("--scale", "NaN")]
    {
        let (secs, scale) = if opt == "--seconds" { (bad, "0.1") } else { ("1", bad) };
        let o = cli(&["--demo", "render", "--seconds", secs, "--scale", scale, "--out", &still]);
        let err = String::from_utf8_lossy(&o.stderr);
        assert_eq!(o.status.code(), Some(2), "{opt} {bad}: {err}");
        assert!(err.contains("filmcraft-cli:") && err.contains(opt), "{opt} {bad}: {err}");
        assert!(!std::path::Path::new(&still).exists(), "{opt} {bad} wrote a file");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// #1054: exporting or rendering a project whose media is missing fails (exit 1) and names the
/// offline items on stderr and in the result's `offlineMedia`, unless `--allow-offline` accepts the
/// placeholder frames; then it still warns.
#[test]
fn export_with_missing_media_fails_unless_allowed() {
    let dir = std::env::temp_dir().join(format!("fc-cli-1054-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = |n: &str| dir.join(n).to_str().unwrap().to_string();
    let (still, proj) = (f("still.png"), f("p.fcproj"));
    let o = cli(&["--demo", "render", "--seconds", "1", "--scale", "0.1", "--out", &still]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    json_out(&cli(&["exec", "file.newSequence", "name=s", "width=64", "height=36", "fps=24", "--save-as", &proj]));
    let paths = format!("paths=[\"{}\"]", still.replace('\\', "\\\\"));
    let item = json_out(&cli(&["--project", &proj, "--save", "exec", "file.import", &paths]))["items"][0].as_u64().unwrap();
    json_out(&cli(&["--project", &proj, "--save", "exec", "timeline.place", &format!("item={item}"), "track=V1", "frame=0", "audio=false"]));
    let export = |out: &str, extra: &[&str]| {
        let mut args = vec!["--project", proj.as_str(), "--compact", "export", out, "--format", "png", "--start", "0", "--end", "0.05"];
        args.extend_from_slice(extra);
        cli(&args)
    };

    let on = export(&f("on.png"), &[]);
    let v = json_out(&on);
    assert_eq!(v["offlineMedia"], serde_json::json!([]), "{v}");
    assert!(!String::from_utf8_lossy(&on.stderr).contains("warning"), "{}", String::from_utf8_lossy(&on.stderr));

    std::fs::remove_file(&still).unwrap();
    let off = export(&f("off.png"), &[]);
    let err = String::from_utf8_lossy(&off.stderr);
    assert_eq!(off.status.code(), Some(1), "{err}");
    assert!(err.contains("still.png") && err.contains("missing") && err.contains("--allow-offline"), "{err}");
    let v: Value = serde_json::from_slice(&off.stdout).expect("the result JSON is still printed");
    assert_eq!(v["offlineMedia"][0]["item"], item, "{v}");
    assert_eq!(v["offlineMedia"][0]["status"], "missing", "{v}");

    let allowed = export(&f("allowed.png"), &["--allow-offline"]);
    let err = String::from_utf8_lossy(&allowed.stderr);
    assert!(allowed.status.success(), "{err}");
    assert!(err.contains("still.png") && err.contains("missing"), "still warns: {err}");
    assert_eq!(json_out(&allowed)["offlineMedia"][0]["status"], "missing");

    let render = cli(&["--project", &proj, "render", "--seconds", "0", "--out", &f("r.png")]);
    let err = String::from_utf8_lossy(&render.stderr);
    assert_eq!(render.status.code(), Some(1), "{err}");
    assert!(err.contains("still.png") && err.contains("--allow-offline"), "{err}");
    let render = cli(&["--project", &proj, "render", "--seconds", "0", "--out", &f("r.png"), "--allow-offline"]);
    assert!(render.status.success(), "{}", String::from_utf8_lossy(&render.stderr));
    assert!(String::from_utf8_lossy(&render.stderr).contains("still.png"));
    let _ = std::fs::remove_dir_all(&dir);
}
