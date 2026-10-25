//! Workflow acceptance tests (docs/gaps.md G6): real editing jobs done end to end the way an agent
//! does them, through the headless MCP server of the `filmcraft-cli` binary (`filmcraft-cli mcp`,
//! JSON-RPC over stdio) and the CLI itself, on media that looks like what people shoot:
//!
//! - a phone clip: H.264 + AAC in MP4 with a variable frame rate (dropped frames);
//! - a camera clip: long-GOP H.264 (one keyframe) in Matroska with two audio tracks (AAC, AC-3).
//!
//! Each test imports, edits (extract, audio gain, title; ripple trims, colour matte, cross
//! dissolve), exports (H.264 MP4; save the project, reopen it in a new process and export ProRes)
//! and checks the file with the ffprobe / ffmpeg oracles: streams, frame count, duration, which
//! source frame lands where, A/V sync, the title's pixels, the dissolve's mix, audio levels.
//!
//! The media is generated with ffmpeg into `target/fixtures/cli/workflows` at test time (never
//! committed); without ffmpeg the tests skip (`FILMCRAFT_REQUIRE_ORACLES=1` makes that a failure).
//! ffmpeg and ffprobe are external test oracles only.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

// ---------------------------------------------------------------------------------------------
// Fixtures

/// A phone-like clip: 4 s of 320×240 `testsrc2` at 30 fps with every frame `n % 7 == 3` dropped
/// (variable frame rate, the timestamps of the kept frames unchanged), H.264 + AAC stereo. The
/// whole picture flashes white on the frames at 1.0 s and 3.0 s, and a 1 kHz beep sounds from 1.0
/// to 1.1 s and from 3.0 to 3.1 s (silence otherwise): A/V sync markers.
fn phone_vfr_mp4(ffmpeg: &Path) -> Option<PathBuf> {
    let out = filmcraft_testkit::fixtures_dir("cli/workflows").join("phone_vfr_h264_aac.mp4");
    filmcraft_testkit::fixtures::generate(&out, |tmp| {
        Command::new(ffmpeg)
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=30:duration=4"])
            .args(["-f", "lavfi", "-i", "sine=frequency=1000:sample_rate=48000:duration=4"])
            .args([
                "-filter_complex",
                "[0:v]drawbox=x=0:y=0:w=iw:h=ih:color=white:t=fill:enable='between(t,1,1.02)+between(t,3,3.02)',\
                 select='not(eq(mod(n,7),3))'[v];\
                 [1:a]volume='if(between(t,1,1.1)+between(t,3,3.1),1,0)':eval=frame[a]",
            ])
            .args(["-map", "[v]", "-map", "[a]", "-c:v", "libx264", "-preset", "veryfast", "-pix_fmt", "yuv420p", "-fps_mode", "passthrough"])
            .args(["-video_track_timescale", "30000", "-c:a", "aac", "-b:a", "128k", "-ac", "2"])
            .arg(tmp)
            .status()
            .is_ok_and(|s| s.success())
    })
}

/// A camera-like clip: 6 s of 320×240 `testsrc2` at 25 fps as long-GOP H.264 (a single keyframe,
/// B-frames) in Matroska, with two stereo audio tracks: 440 Hz in AAC and 1320 Hz in AC-3.
fn camera_long_gop_mkv(ffmpeg: &Path) -> Option<PathBuf> {
    let out = filmcraft_testkit::fixtures_dir("cli/workflows").join("camera_long_gop_2audio.mkv");
    filmcraft_testkit::fixtures::generate(&out, |tmp| {
        Command::new(ffmpeg)
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=25:duration=6"])
            .args(["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=6"])
            .args(["-f", "lavfi", "-i", "sine=frequency=1320:sample_rate=48000:duration=6"])
            .args(["-map", "0", "-map", "1", "-map", "2", "-c:v", "libx264", "-preset", "veryfast", "-pix_fmt", "yuv420p"])
            .args(["-g", "300", "-keyint_min", "300", "-sc_threshold", "0", "-bf", "3"])
            .args(["-c:a:0", "aac", "-ac:a:0", "2", "-c:a:1", "ac3", "-ac:a:1", "2"])
            .arg(tmp)
            .status()
            .is_ok_and(|s| s.success())
    })
}

/// A scratch directory under the workspace `target/`, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let d = filmcraft_testkit::workspace_root().join("target").join("workflow-tests").join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Scratch(d)
    }
    fn path(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().into_owned()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ---------------------------------------------------------------------------------------------
// A minimal MCP client for `filmcraft-cli mcp` (newline-delimited JSON-RPC over stdio)

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Mcp {
    /// Start the headless server (an empty project) and initialise the session. `data_dir` keeps
    /// user presets away from the real data directory.
    fn start(data_dir: &str) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_filmcraft-cli"))
            .args(["--data-dir", data_dir, "mcp"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn filmcraft-cli mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut m = Mcp { child, stdin, stdout, next_id: 1 };
        let init =
            m.request("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "workflow-tests", "version": "1"}}));
        assert_eq!(init["result"]["serverInfo"]["name"], "filmcraft", "{init}");
        m.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        m
    }

    fn send(&mut self, msg: &Value) {
        writeln!(self.stdin, "{msg}").unwrap();
        self.stdin.flush().unwrap();
    }

    /// Send a request and return its response (notifications in between are skipped).
    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let mut line = String::new();
            assert!(self.stdout.read_line(&mut line).unwrap() > 0, "the MCP server closed stdout while `{method}` was pending");
            let v: Value = serde_json::from_str(&line).unwrap_or_else(|e| panic!("not JSON-RPC: {e}: {line}"));
            if v["id"] == id {
                return v;
            }
        }
    }

    /// Call a tool; its text result parsed as JSON. Panics on a protocol or tool error.
    fn tool(&mut self, name: &str, arguments: Value) -> Value {
        let r = self.request("tools/call", json!({"name": name, "arguments": arguments}));
        let text = r["result"]["content"][0]["text"].as_str().unwrap_or_default().to_string();
        assert!(r["error"].is_null() && r["result"]["isError"] == false, "{name} {arguments}: {r}");
        serde_json::from_str(&text).unwrap_or(Value::String(text))
    }

    /// `command_run`: one engine command by id.
    fn run(&mut self, id: &str, params: Value) -> Value {
        self.tool("command_run", json!({"id": id, "params": params}))
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---------------------------------------------------------------------------------------------
// Oracles

fn probe(ffprobe: &Path, path: &str) -> Value {
    let out = Command::new(ffprobe).args(["-v", "error", "-of", "json", "-count_frames", "-show_format", "-show_streams", path]).output().unwrap();
    assert!(out.status.success(), "ffprobe {path}: {}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).unwrap()
}

fn streams<'a>(j: &'a Value, kind: &str) -> Vec<&'a Value> {
    j["streams"].as_array().unwrap().iter().filter(|s| s["codec_type"] == kind).collect()
}

fn num(v: &Value) -> f64 {
    v.as_str().and_then(|s| s.parse().ok()).or_else(|| v.as_f64()).unwrap_or(f64::NAN)
}

/// Run ffmpeg decoding `input` (with `pre` input options) to raw data on stdout.
fn ffmpeg_raw(ffmpeg: &Path, pre: &[&str], input: &str, args: &[&str]) -> Vec<u8> {
    let out = Command::new(ffmpeg).args(["-v", "error"]).args(pre).args(["-i", input]).args(args).arg("-").output().unwrap();
    assert!(out.status.success(), "ffmpeg {input}: {}", String::from_utf8_lossy(&out.stderr));
    out.stdout
}

/// Size of the frames [`frames`] returns.
const FW: usize = 80;
const FH: usize = 60;

/// Every video frame of `input`, scaled to 80×60 in `pix_fmt` (`gray` or `rgb24`). `vf` goes
/// first in the filter chain (e.g. `fps=30` to see a VFR file as an editor shows it).
fn frames(ffmpeg: &Path, input: &str, vf: Option<&str>, pix_fmt: &str) -> Vec<Vec<u8>> {
    let chain = match vf {
        Some(f) => format!("{f},scale={FW}:{FH}:flags=area,format={pix_fmt}"),
        None => format!("scale={FW}:{FH}:flags=area,format={pix_fmt}"),
    };
    let bpp = if pix_fmt == "gray" { 1 } else { 3 };
    let raw = ffmpeg_raw(ffmpeg, &[], input, &["-map", "0:v:0", "-vf", &chain, "-fps_mode", "passthrough", "-f", "rawvideo"]);
    raw.chunks_exact(FW * FH * bpp).map(<[u8]>::to_vec).collect()
}

/// Mean absolute difference of two equally sized frames.
fn diff(a: &[u8], b: &[u8]) -> f64 {
    a.iter().zip(b).map(|(x, y)| f64::from(x.abs_diff(*y))).sum::<f64>() / a.len().max(1) as f64
}

/// Assert that `got` shows source frame `want`: it is (one of) the closest source frames, and close.
fn assert_shows(got: &[u8], src: &[Vec<u8>], want: usize, what: &str) {
    let d: Vec<f64> = src.iter().map(|s| diff(got, s)).collect();
    let best = d.iter().copied().fold(f64::INFINITY, f64::min);
    let at = d.iter().position(|x| *x == best).unwrap_or(0);
    assert!(d[want] < 6.0 && d[want] <= best + 0.25, "{what}: expected source frame {want} (diff {:.2}), closest is {at} (diff {best:.2})", d[want]);
}

/// The first channel of audio stream `stream` of `input`, mono-mixed, as f32 at 48 kHz.
fn pcm(ffmpeg: &Path, input: &str, stream: usize) -> Vec<f32> {
    let map = format!("0:a:{stream}");
    let raw = ffmpeg_raw(ffmpeg, &[], input, &["-map", &map, "-ac", "1", "-ar", "48000", "-f", "f32le"]);
    raw.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect()
}

/// Times (s) where the signal rises above a quarter of its peak, at least 0.2 s apart.
fn onsets(x: &[f32]) -> Vec<f64> {
    let peak = x.iter().fold(0f32, |m, v| m.max(v.abs()));
    let mut out = Vec::new();
    let mut last = f64::NEG_INFINITY;
    for (i, block) in x.chunks(48).enumerate() {
        let t = i as f64 * 48.0 / 48_000.0;
        if block.iter().any(|v| v.abs() > peak / 4.0) && t - last > 0.2 {
            out.push(t);
        }
        if block.iter().any(|v| v.abs() > peak / 4.0) {
            last = t;
        }
    }
    out
}

fn rms(x: &[f32], from: f64, to: f64) -> f64 {
    let s = x.get((from * 48_000.0) as usize..((to * 48_000.0) as usize).min(x.len())).unwrap_or_default();
    (s.iter().map(|v| f64::from(*v) * f64::from(*v)).sum::<f64>() / s.len().max(1) as f64).sqrt()
}

/// Amplitude of the `freq` component of 100 ms of `x` from `at` seconds (Goertzel).
fn tone(x: &[f32], at: f64, freq: f64) -> f64 {
    let start = (at * 48_000.0) as usize;
    let s = x.get(start..(start + 4800).min(x.len())).unwrap_or_default();
    let c = 2.0 * (2.0 * std::f64::consts::PI * freq / 48_000.0).cos();
    let (mut s1, mut s2) = (0.0, 0.0);
    for v in s {
        let s0 = f64::from(*v) + c * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    (s1 * s1 + s2 * s2 - c * s1 * s2).max(0.0).sqrt() * 2.0 / s.len().max(1) as f64
}

fn db(ratio: f64) -> f64 {
    20.0 * ratio.log10()
}

fn clips_on<'a>(seq: &'a Value, kind: &str, track: usize) -> &'a Vec<Value> {
    seq[kind][track]["items"].as_array().unwrap()
}

// ---------------------------------------------------------------------------------------------
// Workflows

/// Phone footage: import a VFR H.264/AAC clip, make a sequence from it, extract a second from the
/// middle (Mark In / Mark Out / Extract), lower the audio 6 dB, put a title over the start, and
/// export H.264 MP4 through MCP. The VFR clip plays at its 30 fps base rate (a dropped frame
/// holds the one before), the cut lands on the right frames, picture and sound stay in sync
/// across the cut, the title is in the picture only while it is on the timeline.
#[test]
fn phone_vfr_clip_extract_gain_title_to_h264_mp4() {
    let (ffmpeg, ffprobe) = filmcraft_testkit::require_oracles!();
    let Some(src) = phone_vfr_mp4(&ffmpeg) else {
        return filmcraft_testkit::skip("phone workflow", "ffmpeg could not make the VFR H.264/AAC fixture (libx264?)");
    };
    let src = src.to_string_lossy().into_owned();
    let dir = Scratch::new("phone");
    let mut m = Mcp::start(&dir.path("data"));

    let imported = m.tool("media_import", json!({"paths": [src]}));
    assert_eq!(imported["errors"], json!([]), "{imported}");
    let item = imported["items"][0].as_u64().unwrap();
    let made = m.run("file.newSequenceFromClip", json!({"items": [item]}));
    assert!(made["sequence"].is_u64(), "{made}");
    let seq = m.tool("sequence_inspect", json!({}));
    assert_eq!((seq["settings"]["width"].as_u64(), seq["settings"]["height"].as_u64()), (Some(320), Some(240)), "{}", seq["settings"]);
    assert_eq!(seq["settings"]["frame_rate"], json!({"num": 30, "den": 1}), "a VFR phone clip makes a sequence at its base rate");
    assert_eq!(seq["durationFrames"], 120, "4 s at 30 fps");

    // Extract 1.5 s .. 2.5 s; Mark Out includes its frame, so 31 frames go and 89 remain.
    m.run("markers.markIn", json!({"seconds": 1.5}));
    m.run("markers.markOut", json!({"seconds": 2.5}));
    m.run("sequence.extract", json!({}));
    m.run("edit.selectAll", json!({}));
    let gain = m.run("clip.audioGain", json!({"mode": "set", "db": -6.0}));
    assert_eq!(gain["clips"], 2, "both audio pieces: {gain}");
    m.run("edit.deselectAll", json!({}));
    m.run("graphics.newText", json!({"text": "HELLO", "size": 40, "time": 0, "seconds": 0.5}));
    let seq = m.tool("sequence_inspect", json!({}));
    assert_eq!(seq["durationFrames"], 89, "{seq}");
    assert_eq!(clips_on(&seq, "video", 0).len(), 2);
    assert_eq!(clips_on(&seq, "audio", 0).len(), 2);

    let out = dir.path("phone_edit.mp4");
    let r = m.run("file.exportMedia", json!({"path": out, "format": "h264", "range": "entire", "wait": true}));
    assert_eq!(r["result"]["frames"], 89, "{r}");

    // container and streams
    let j = probe(&ffprobe, &out);
    let (v, a) = (streams(&j, "video"), streams(&j, "audio"));
    assert_eq!((v.len(), a.len()), (1, 1), "{j}");
    assert_eq!((v[0]["codec_name"].as_str(), v[0]["width"].as_u64(), v[0]["height"].as_u64()), (Some("h264"), Some(320), Some(240)));
    assert_eq!(v[0]["r_frame_rate"], "30/1");
    assert_eq!(v[0]["nb_read_frames"], "89");
    assert_eq!((a[0]["codec_name"].as_str(), a[0]["sample_rate"].as_str(), a[0]["channels"].as_u64()), (Some("aac"), Some("48000"), Some(2)));
    let duration = num(&j["format"]["duration"]);
    assert!((duration - 89.0 / 30.0).abs() < 0.06, "duration {duration}");
    assert!((num(&a[0]["duration"]) - 89.0 / 30.0).abs() < 0.06, "audio as long as the picture: {}", a[0]["duration"]);

    // picture: frame k of the export is frame k (before the cut) or k + 31 (after) of the source
    // as an editor plays it (CFR 30, a dropped frame holding the previous one)
    let src_frames = frames(&ffmpeg, &src, Some("fps=30"), "gray");
    let got = frames(&ffmpeg, &out, None, "gray");
    assert!(src_frames.len() >= 120, "{} source frames", src_frames.len());
    assert_eq!(got.len(), 89);
    for k in [20, 32, 40, 44, 45, 50, 70, 88] {
        let want = if k < 45 { k } else { k + 31 };
        assert_shows(&got[k], &src_frames, want, &format!("export frame {k}"));
    }
    // the flash frames: 30 (with 31 holding it, as the source dropped its frame 31) and 90 - 31
    let mean = |f: &Vec<u8>| f.iter().map(|v| f64::from(*v)).sum::<f64>() / f.len() as f64;
    let flashes: Vec<usize> = got.iter().enumerate().filter(|(_, f)| mean(f) > 200.0).map(|(i, _)| i).collect();
    assert_eq!(flashes, [30, 31, 59], "white frames");

    // the title: over frames 0..15 only (its pixels differ from the source there, nowhere after)
    let blocks_changed = |k: usize| {
        let mut n = 0;
        for by in (0..FH).step_by(10) {
            for bx in (0..FW).step_by(10) {
                let mut d = 0.0;
                for y in by..by + 10 {
                    for x in bx..bx + 10 {
                        d += f64::from(got[k][y * FW + x].abs_diff(src_frames[k][y * FW + x]));
                    }
                }
                n += usize::from(d / 100.0 > 15.0);
            }
        }
        n
    };
    for k in [2, 8, 14] {
        assert!(blocks_changed(k) >= 2, "title missing on frame {k}");
    }
    for k in [16, 25] {
        assert_eq!(blocks_changed(k), 0, "title still on frame {k}");
    }

    // sound: beeps where the flashes are (A/V sync across the extract), 6 dB down
    let src_pcm = pcm(&ffmpeg, &src, 0);
    let got_pcm = pcm(&ffmpeg, &out, 0);
    let beeps = onsets(&got_pcm);
    assert_eq!(beeps.len(), 2, "beeps at {beeps:?}");
    for (beep, flash) in beeps.iter().zip([30.0 / 30.0, 59.0 / 30.0]) {
        assert!((beep - flash).abs() <= 1.0 / 30.0, "beep at {beep:.3} s, flash at {flash:.3} s");
    }
    let change = db(rms(&got_pcm, 1.01, 1.09) / rms(&src_pcm, 1.01, 1.09));
    assert!((change + 6.0).abs() < 1.0, "audio gain -6 dB measured {change:.2} dB");
    let change = db(rms(&got_pcm, 1.98, 2.06) / rms(&src_pcm, 3.01, 3.09));
    assert!((change + 6.0).abs() < 1.0, "second beep {change:.2} dB");
}

/// Camera footage: import a long-GOP H.264 Matroska file with two audio tracks through MCP, make
/// a sequence (both tracks land on A1 and A2), ripple-trim 2.6 s off the head (decoding starts at
/// the only keyframe) and 15 frames off the tail, add a red colour matte with a centred cross
/// dissolve, save the project; then a new `filmcraft-cli` process opens it and exports ProRes.
/// The first exported frame is source frame 65, the dissolve mixes the two pictures, both audio
/// tracks are in the mix at their own level and stop where the clip does.
#[test]
fn camera_long_gop_two_audio_tracks_trim_dissolve_to_prores() {
    let (ffmpeg, ffprobe) = filmcraft_testkit::require_oracles!();
    let Some(src) = camera_long_gop_mkv(&ffmpeg) else {
        return filmcraft_testkit::skip("camera workflow", "ffmpeg could not make the long-GOP Matroska fixture (libx264?)");
    };
    let src = src.to_string_lossy().into_owned();
    let dir = Scratch::new("camera");
    let data = dir.path("data");
    let project = dir.path("camera.fcproj");
    {
        let mut m = Mcp::start(&data);
        let imported = m.tool("media_import", json!({"paths": [src]}));
        assert_eq!(imported["errors"], json!([]), "{imported}");
        let item = imported["items"][0].as_u64().unwrap();
        let made = m.run("file.newSequenceFromClip", json!({"items": [item]}));
        let clips: Vec<u64> = made["clips"].as_array().unwrap().iter().filter_map(Value::as_u64).collect();
        assert_eq!(clips.len(), 3, "a video clip and one clip per audio track: {made}");
        let seq = m.tool("sequence_inspect", json!({}));
        assert_eq!(seq["settings"]["frame_rate"], json!({"num": 25, "den": 1}));
        assert_eq!(seq["audio"].as_array().map(Vec::len), Some(2), "two audio tracks");
        let video = clips_on(&seq, "video", 0)[0]["clip"].as_u64().unwrap();

        // 65 frames off the head and 15 off the tail, the linked audio following
        m.run("timeline.trim", json!({"clip": video, "edge": "in", "mode": "ripple", "deltaFrames": 65}));
        m.run("timeline.trim", json!({"clip": video, "edge": "out", "mode": "ripple", "deltaFrames": -15}));
        let seq = m.tool("sequence_inspect", json!({}));
        let end = clips_on(&seq, "video", 0)[0]["endFrame"].as_u64().unwrap();
        for t in 0..2 {
            let a = &clips_on(&seq, "audio", t)[0];
            assert_eq!((a["startFrame"].as_u64(), a["endFrame"].as_u64(), a["sourceIn"].as_i64()), (Some(0), Some(end), Some(660_441_600_000)), "A{}", t + 1);
        }

        let matte = m.run("file.newColorMatte", json!({"color": "#ff0000", "seconds": 2.0}))["item"].as_u64().unwrap();
        let placed = m.run("timeline.place", json!({"item": matte, "track": "V1", "frame": end, "insert": false}));
        let matte_clip = placed["clips"][0].as_u64().unwrap();
        m.run("sequence.applyVideoTransition", json!({"clip": matte_clip, "edge": "in", "frames": 10, "effect": "cross_dissolve"}));
        let seq = m.tool("sequence_inspect", json!({}));
        let tr = &seq["video"][0]["transitions"][0];
        assert_eq!((tr["effect"].as_str(), tr["align"].as_str()), (Some("cross_dissolve"), Some("center")), "{tr}");
        m.run("file.saveAs", json!({"path": project}));
    }
    assert_eq!(end_frame_of(&project), Some(71), "the saved project holds the edit");

    // a fresh process opens the saved project and renders it
    let out = dir.path("camera_edit.mov");
    let o = Command::new(env!("CARGO_BIN_EXE_filmcraft-cli")).args(["--data-dir", &data, "--project", &project, "export", &out]).output().unwrap();
    assert!(o.status.success(), "export: {}", String::from_utf8_lossy(&o.stderr));
    let r: Value = serde_json::from_slice(&o.stdout).unwrap();
    let total = 71 + 50;
    assert_eq!(r["result"]["frames"], total, "{r}");

    let j = probe(&ffprobe, &out);
    let (v, a) = (streams(&j, "video"), streams(&j, "audio"));
    assert_eq!((v.len(), a.len()), (1, 1), "{j}");
    assert_eq!((v[0]["codec_name"].as_str(), v[0]["width"].as_u64(), v[0]["height"].as_u64()), (Some("prores"), Some(320), Some(240)));
    assert_eq!(v[0]["r_frame_rate"], "25/1");
    assert_eq!(v[0]["nb_read_frames"], total.to_string());
    assert_eq!((a[0]["sample_rate"].as_str(), a[0]["channels"].as_u64()), (Some("48000"), Some(2)));
    let duration = num(&j["format"]["duration"]);
    assert!((duration - f64::from(total) / 25.0).abs() < 0.05, "duration {duration}");

    // picture: frame k is source frame 65 + k up to the dissolve (frames 66..76), red after it
    let src_frames = frames(&ffmpeg, &src, None, "rgb24");
    let got = frames(&ffmpeg, &out, None, "rgb24");
    assert_eq!((src_frames.len(), got.len()), (150, total as usize));
    for k in [0, 1, 2, 30, 64] {
        assert_shows(&got[k], &src_frames, 65 + k, &format!("export frame {k}"));
    }
    let red = &got[100];
    assert!(red.as_chunks::<3>().0.iter().all(|[r, g, b]| *r > 230 && *g < 25 && *b < 25), "the matte is red");
    // the dissolve: frame = (1 - a)·camera + a·red, `a` rising through about 0.5 at the cut
    let mix = |k: usize| {
        let (cam, e) = (&src_frames[65 + k], &got[k]);
        let (mut num, mut den) = (0.0, 0.0);
        for ((c, r), e) in cam.iter().zip(red).zip(e) {
            let (c, r, e) = (f64::from(*c), f64::from(*r), f64::from(*e));
            num += (e - c) * (r - c);
            den += (r - c) * (r - c);
        }
        num / den
    };
    let alphas: Vec<f64> = (64..=78).map(mix).collect();
    assert!(alphas[0].abs() < 0.05 && (alphas[14] - 1.0).abs() < 0.05, "outside the dissolve: {alphas:?}");
    assert!(alphas.windows(2).all(|w| w[1] >= w[0] - 0.02), "the dissolve progresses: {alphas:?}");
    assert!((alphas[7] - 0.5).abs() < 0.12, "half way at the cut (frame 71): {alphas:?}");

    // sound: both tracks in the mix at their own level while the clip plays, silence on the matte
    let got_pcm = pcm(&ffmpeg, &out, 0);
    let (t1, t2) = (pcm(&ffmpeg, &src, 0), pcm(&ffmpeg, &src, 1));
    for at in [0.2, 1.5, 2.6] {
        let (low, high) = (tone(&got_pcm, at, 440.0), tone(&got_pcm, at, 1320.0));
        let (want_low, want_high) = (tone(&t1, at + 2.6, 440.0), tone(&t2, at + 2.6, 1320.0));
        assert!(
            db(low / want_low).abs() < 1.0 && db(high / want_high).abs() < 1.0,
            "at {at} s: 440 Hz {low:.4} (want {want_low:.4}), 1320 Hz {high:.4} (want {want_high:.4})"
        );
    }
    assert!(rms(&got_pcm, 3.0, 4.8) < 1e-3, "silence under the matte");
}

/// `endFrame` of the first clip on V1 of the saved project's active sequence, read back through
/// `filmcraft-cli --project … inspect sequence`.
fn end_frame_of(project: &str) -> Option<u64> {
    let o = Command::new(env!("CARGO_BIN_EXE_filmcraft-cli")).args(["--project", project, "inspect", "sequence"]).output().ok()?;
    let v: Value = serde_json::from_slice(&o.stdout).ok()?;
    v["video"][0]["items"][0]["endFrame"].as_u64()
}
