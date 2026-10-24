//! Shared helpers for oracle tests: ffmpeg/ffprobe discovery and fixture generation.
//! ffmpeg is used only as an external fixture generator and oracle (never linked).
#![allow(dead_code)]

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Find a tool silently (see `filmcraft_testkit::oracle`).
pub fn tool(name: &str) -> Option<PathBuf> {
    match name {
        "ffmpeg" => filmcraft_testkit::ffmpeg(),
        "ffprobe" => filmcraft_testkit::ffprobe(),
        _ => filmcraft_testkit::oracle::find_tool(name),
    }
}

/// ffmpeg, or `None` after printing `SKIPPED` (a failure with `FILMCRAFT_REQUIRE_ORACLES=1`).
pub fn ffmpeg() -> Option<PathBuf> {
    filmcraft_testkit::ffmpeg_or_skip("isobmff oracle")
}

/// ffprobe, or `None` after printing `SKIPPED` (a failure with `FILMCRAFT_REQUIRE_ORACLES=1`).
pub fn ffprobe() -> Option<PathBuf> {
    filmcraft_testkit::ffprobe_or_skip("isobmff oracle")
}

pub fn fixture_dir() -> PathBuf {
    filmcraft_testkit::fixtures_dir("isobmff")
}

pub fn out_dir() -> PathBuf {
    let d = fixture_dir().join("out");
    std::fs::create_dir_all(&d).unwrap();
    d
}

const V: &[&str] = &["-f", "lavfi", "-i", "testsrc2=size=160x120:rate=25"];
const A: &[&str] = &["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000"];

/// (fixture name, ffmpeg arguments after the global options, output file name derived from the name).
pub fn fixture_specs() -> Vec<(&'static str, Vec<&'static str>)> {
    let cat = |parts: &[&[&'static str]]| parts.concat();
    vec![
        ("h264_bframes.mp4", cat(&[V, A, &["-t", "2", "-c:v", "libx264", "-bf", "2", "-g", "12", "-pix_fmt", "yuv420p", "-c:a", "aac"]])),
        ("h264.mov", cat(&[V, A, &["-t", "2", "-c:v", "libx264", "-bf", "3", "-g", "25", "-pix_fmt", "yuv420p", "-c:a", "pcm_s16le"]])),
        ("hevc.mp4", cat(&[V, &["-t", "2", "-c:v", "libx265", "-tag:v", "hvc1", "-x265-params", "log-level=error:bframes=3", "-pix_fmt", "yuv420p"]])),
        ("prores.mov", cat(&[V, A, &["-t", "1", "-c:v", "prores_ks", "-profile:v", "3", "-c:a", "pcm_s24le"]])),
        ("prores4444.mov", cat(&[V, &["-t", "0.5", "-c:v", "prores_ks", "-profile:v", "4", "-pix_fmt", "yuva444p10le"]])),
        ("mjpeg.mov", cat(&[V, &["-t", "1", "-c:v", "mjpeg", "-q:v", "5"]])),
        (
            "dnxhd.mov",
            vec!["-f", "lavfi", "-i", "testsrc2=size=1280x720:rate=25", "-t", "0.2", "-c:v", "dnxhd", "-profile:v", "dnxhr_lb", "-pix_fmt", "yuv422p"],
        ),
        ("pcm_f32.mov", cat(&[A, &["-t", "1", "-c:a", "pcm_f32le"]])),
        ("pcm_s16be.mov", cat(&[A, &["-t", "1", "-c:a", "pcm_s16be"]])),
        ("aac.mp4", cat(&[A, &["-t", "2", "-c:a", "aac", "-b:a", "96k"]])),
        ("aac_stereo44.m4a", vec!["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=44100", "-t", "1.5", "-ac", "2", "-c:a", "aac"]),
        (
            "frag.mp4",
            cat(&[
                V,
                A,
                &["-t", "3", "-c:v", "libx264", "-bf", "2", "-g", "25", "-pix_fmt", "yuv420p", "-c:a", "aac", "-movflags", "frag_keyframe+empty_moov"],
            ]),
        ),
        (
            "frag_dash.mp4",
            cat(&[
                V,
                &["-t", "3", "-c:v", "libx264", "-bf", "2", "-g", "25", "-pix_fmt", "yuv420p", "-movflags", "frag_keyframe+empty_moov+default_base_moof"],
            ]),
        ),
        ("faststart.mp4", cat(&[V, A, &["-t", "2", "-c:v", "libx264", "-bf", "2", "-pix_fmt", "yuv420p", "-c:a", "aac", "-movflags", "+faststart"]])),
        ("tmcd_2997df.mov", vec!["-f", "lavfi", "-i", "testsrc2=size=160x120:rate=30000/1001", "-t", "1", "-c:v", "mjpeg", "-timecode", "01:00:00;00"]),
        (
            "tmcd_23976.mov",
            vec!["-f", "lavfi", "-i", "testsrc2=size=160x120:rate=24000/1001", "-t", "1", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-timecode", "01:00:00:00"],
        ),
        ("h264_2997.mp4", vec!["-f", "lavfi", "-i", "testsrc2=size=160x120:rate=30000/1001", "-t", "2", "-c:v", "libx264", "-bf", "2", "-pix_fmt", "yuv420p"]),
        ("flac.mp4", cat(&[A, &["-t", "1", "-c:a", "flac"]])),
        ("alac.m4a", cat(&[A, &["-t", "1", "-c:a", "alac"]])),
        ("opus.mp4", cat(&[A, &["-t", "1", "-c:a", "libopus"]])),
        ("ac3.mp4", cat(&[A, &["-t", "1", "-c:a", "ac3"]])),
        ("vp9.mp4", cat(&[V, &["-t", "1", "-c:v", "libvpx-vp9", "-deadline", "realtime", "-cpu-used", "8"]])),
        ("av1.mp4", cat(&[V, &["-t", "1", "-c:v", "libsvtav1", "-preset", "12"]])),
        (
            "colr_pasp.mov",
            cat(&[
                V,
                &[
                    "-t",
                    "0.5",
                    "-c:v",
                    "libx264",
                    "-pix_fmt",
                    "yuv420p",
                    "-color_primaries",
                    "bt709",
                    "-color_trc",
                    "bt709",
                    "-colorspace",
                    "bt709",
                    "-aspect",
                    "16:9",
                ],
            ]),
        ),
    ]
}

fn run_ffmpeg(ff: &Path, args: &[&str], out: &Path) -> bool {
    let tmp = filmcraft_testkit::temp_path(out);
    let st = Command::new(ff).args(["-v", "error", "-y", "-nostdin"]).args(args).arg(&tmp).output();
    match st {
        Ok(o) if o.status.success() => std::fs::rename(&tmp, out).is_ok(),
        Ok(o) => {
            eprintln!("ffmpeg failed for {}: {}", out.display(), String::from_utf8_lossy(&o.stderr));
            let _ = std::fs::remove_file(&tmp);
            false
        }
        Err(_) => false,
    }
}

/// Path to a fixture, generating it with ffmpeg if needed. `None` if ffmpeg is missing or the
/// encoder isn't available.
pub fn fixture(name: &str) -> Option<PathBuf> {
    let ff = ffmpeg()?;
    let path = fixture_dir().join(name);
    if path.exists() {
        return Some(path);
    }
    if name == "editlist_cut.mp4" {
        let src = fixture("h264_bframes.mp4")?;
        let s = src.to_str()?.to_string();
        return run_ffmpeg(&ff, &["-ss", "0.5", "-i", &s, "-c", "copy"], &path).then_some(path);
    }
    let (_, args) = fixture_specs().into_iter().find(|(n, _)| *n == name)?;
    run_ffmpeg(&ff, &args, &path).then_some(path)
}

pub fn all_fixture_names() -> Vec<&'static str> {
    let mut v: Vec<&str> = fixture_specs().iter().map(|(n, _)| *n).collect();
    v.push("editlist_cut.mp4");
    v
}

pub fn probe_json(path: &Path, what: &[&str]) -> Option<Value> {
    let fp = tool("ffprobe")?;
    let o = Command::new(fp).args(["-v", "error", "-of", "json"]).args(what).arg(path).output().ok()?;
    if !o.status.success() {
        return None;
    }
    serde_json::from_slice(&o.stdout).ok()
}

pub fn packets(path: &Path) -> Vec<Value> {
    probe_json(path, &["-show_packets"]).and_then(|v| v["packets"].as_array().cloned()).unwrap_or_default()
}

pub fn streams(path: &Path) -> Vec<Value> {
    probe_json(path, &["-show_streams"]).and_then(|v| v["streams"].as_array().cloned()).unwrap_or_default()
}

pub fn int(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

/// Decode a file with ffmpeg to the null muxer; returns stderr (empty on success).
pub fn ffmpeg_decode_errors(path: &Path) -> Option<String> {
    let ff = tool("ffmpeg")?;
    let o = Command::new(ff).args(["-v", "error", "-nostdin", "-i"]).arg(path).args(["-f", "null", "-"]).output().ok()?;
    let mut s = String::from_utf8_lossy(&o.stderr).into_owned();
    if !o.status.success() {
        s.push_str("\n(ffmpeg exited with failure)");
    }
    Some(s)
}
