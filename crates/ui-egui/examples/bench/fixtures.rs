//! Benchmark fixtures, generated with ffmpeg (an external fixture generator only; never linked or
//! shipped) into `target/fixtures/playback/` on first use. Never committed.

use std::path::{Path, PathBuf};
use std::process::Command;

/// `<repo>/target/fixtures/playback`, or `$FILMCRAFT_FIXTURES/playback` (lets worktrees share one set).
pub fn fixtures_dir() -> PathBuf {
    match std::env::var_os("FILMCRAFT_FIXTURES") {
        Some(d) => PathBuf::from(d).join("playback"),
        None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/fixtures/playback"),
    }
}

pub fn ffmpeg() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("FILMCRAFT_FFMPEG") {
        return Some(PathBuf::from(p));
    }
    let fixed = ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg", "/usr/bin/ffmpeg"].iter().map(PathBuf::from).find(|p| p.exists());
    // otherwise whatever `ffmpeg` / `ffmpeg.exe` is on PATH (Windows has no fixed location)
    fixed.or_else(|| std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(format!("ffmpeg{}", std::env::consts::EXE_SUFFIX))).find(|p| p.is_file()))
}

/// A fixture: file name, lavfi video source, seconds, with audio, video encoder arguments.
struct Spec {
    name: &'static str,
    src: &'static str,
    secs: u32,
    audio: bool,
    codec: &'static [&'static str],
}

const X264: &[&str] = &["-c:v", "libx264", "-preset", "fast", "-crf", "20", "-pix_fmt", "yuv420p"];
const X265: &[&str] = &["-c:v", "libx265", "-preset", "fast", "-crf", "22", "-pix_fmt", "yuv420p", "-tag:v", "hvc1", "-x265-params", "log-level=error"];
const VP9: &[&str] = &["-c:v", "libvpx-vp9", "-deadline", "realtime", "-cpu-used", "8", "-b:v", "0", "-crf", "32", "-row-mt", "1", "-pix_fmt", "yuv420p"];
const AV1: &[&str] = &["-c:v", "libsvtav1", "-preset", "10", "-crf", "35", "-pix_fmt", "yuv420p", "-svtav1-params", "keyint=48"];
const X265_MAIN10: &[&str] = &[
    "-c:v",
    "libx265",
    "-preset",
    "fast",
    "-crf",
    "22",
    "-profile:v",
    "main10",
    "-pix_fmt",
    "yuv420p10le",
    "-tag:v",
    "hvc1",
    "-x265-params",
    "log-level=error",
];
const PRORES: &[&str] = &["-c:v", "prores_ks", "-profile:v", "3", "-pix_fmt", "yuv422p10le", "-vendor", "apl0"];

const SPECS: &[Spec] = &[
    // Playback scenarios (also used by `bench_playback`).
    Spec { name: "a1080.mp4", src: "testsrc2=s=1920x1080:r=24000/1001:d=20,noise=alls=6:allf=t", secs: 20, audio: true, codec: X264 },
    Spec { name: "b1080.mp4", src: "mandelbrot=s=1920x1080:r=24000/1001,trim=duration=20,noise=alls=6:allf=t", secs: 20, audio: false, codec: X264 },
    Spec { name: "c1080.mp4", src: "smptehdbars=s=1920x1080:r=24000/1001:d=20,noise=alls=10:allf=t+u", secs: 20, audio: false, codec: X264 },
    Spec { name: "a2160.mp4", src: "testsrc2=s=3840x2160:r=24000/1001:d=10,noise=alls=6:allf=t", secs: 10, audio: false, codec: X264 },
    Spec { name: "hevc2160.mp4", src: "testsrc2=s=3840x2160:r=24000/1001:d=10,noise=alls=6:allf=t", secs: 10, audio: false, codec: X265 },
    Spec { name: "vp92160.webm", src: "testsrc2=s=3840x2160:r=24000/1001:d=10,noise=alls=6:allf=t", secs: 10, audio: false, codec: VP9 },
    Spec { name: "av12160.mp4", src: "testsrc2=s=3840x2160:r=24000/1001:d=10,noise=alls=6:allf=t", secs: 10, audio: false, codec: AV1 },
    // Decode-speed matrix: 1080p (5 s) and 2160p (3 s) per codec, same picture content (ProRes
    // 2 s / 1 s: intra-only, every frame costs the same, and the files are large).
    Spec { name: "dec_h264_1080.mp4", src: "testsrc2=s=1920x1080:r=24000/1001:d=5,noise=alls=6:allf=t", secs: 5, audio: false, codec: X264 },
    Spec { name: "dec_h264_2160.mp4", src: "testsrc2=s=3840x2160:r=24000/1001:d=3,noise=alls=6:allf=t", secs: 3, audio: false, codec: X264 },
    Spec { name: "dec_hevc_1080.mp4", src: "testsrc2=s=1920x1080:r=24000/1001:d=5,noise=alls=6:allf=t", secs: 5, audio: false, codec: X265 },
    Spec { name: "dec_hevc_2160.mp4", src: "testsrc2=s=3840x2160:r=24000/1001:d=3,noise=alls=6:allf=t", secs: 3, audio: false, codec: X265 },
    Spec { name: "dec_hevc10_2160.mp4", src: "testsrc2=s=3840x2160:r=24000/1001:d=3,noise=alls=6:allf=t", secs: 3, audio: false, codec: X265_MAIN10 },
    Spec { name: "dec_vp9_1080.webm", src: "testsrc2=s=1920x1080:r=24000/1001:d=5,noise=alls=6:allf=t", secs: 5, audio: false, codec: VP9 },
    Spec { name: "dec_vp9_2160.webm", src: "testsrc2=s=3840x2160:r=24000/1001:d=3,noise=alls=6:allf=t", secs: 3, audio: false, codec: VP9 },
    Spec { name: "dec_av1_1080.mp4", src: "testsrc2=s=1920x1080:r=24000/1001:d=5,noise=alls=6:allf=t", secs: 5, audio: false, codec: AV1 },
    Spec { name: "dec_av1_2160.mp4", src: "testsrc2=s=3840x2160:r=24000/1001:d=3,noise=alls=6:allf=t", secs: 3, audio: false, codec: AV1 },
    Spec { name: "dec_prores_1080.mov", src: "testsrc2=s=1920x1080:r=24000/1001:d=2,noise=alls=6:allf=t", secs: 2, audio: false, codec: PRORES },
    Spec { name: "dec_prores_2160.mov", src: "testsrc2=s=3840x2160:r=24000/1001:d=1,noise=alls=6:allf=t", secs: 1, audio: false, codec: PRORES },
    // Small clip media for the 1000-clip timeline / project benchmarks (video + audio).
    Spec { name: "clip360.mp4", src: "testsrc2=s=640x360:r=24000/1001:d=20", secs: 20, audio: true, codec: X264 },
];

/// The fixture `name`, generating it on first use (None when ffmpeg or its encoder is missing).
pub fn fixture(name: &str) -> Option<PathBuf> {
    let dir = fixtures_dir();
    let path = dir.join(name);
    if path.exists() {
        return Some(path);
    }
    let ff = ffmpeg()?;
    let spec = SPECS.iter().find(|f| f.name == name)?;
    std::fs::create_dir_all(&dir).ok()?;
    eprintln!("generating {}", path.display());
    // Write under a temporary name and rename, so a concurrent run never sees half a file.
    let ext = Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("mp4");
    let tmp = dir.join(format!(".{}.{}.tmp.{ext}", name, std::process::id()));
    let mut c = Command::new(ff);
    c.env("SVT_LOG", "1");
    c.args(["-v", "error", "-y", "-f", "lavfi", "-i", spec.src]);
    if spec.audio {
        c.args(["-f", "lavfi", "-i", &format!("sine=f=440:d={}", spec.secs), "-c:a", "aac", "-shortest"]);
    }
    c.args(spec.codec);
    if ext != "webm" {
        c.args(["-movflags", "+faststart"]);
    }
    c.arg(&tmp);
    let ok = c.status().ok()?.success();
    if !ok {
        let _ = std::fs::remove_file(&tmp);
        eprintln!("  ffmpeg failed for {name} (encoder missing?)");
        return None;
    }
    std::fs::rename(&tmp, &path).ok()?;
    Some(path)
}
