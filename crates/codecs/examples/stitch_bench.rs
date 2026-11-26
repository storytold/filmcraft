//! Measure the INSV stitch's throughput: `cargo run --release -p filmcraft-codecs
//! --example stitch_bench -- in.insv [frames]`.
//!
//! Opens the file (joining a spanned recording's parts, like `insv_stitch`), warms
//! the bake with one frame, then decodes + stitches `frames` consecutive frames
//! (default 24) and prints ms/frame and fps. The seam alignment must keep the
//! result at or above the source's frame rate for real-time playback.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use filmcraft_media::{MediaError, SharedSource};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: stitch_bench in.insv [frames]");
        std::process::exit(2);
    }
    let n: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(24).clamp(1, 1000);
    let src = match open(Path::new(&args[1])) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let info = src.info();
    let Some(v) = info.video.clone() else {
        eprintln!("no video stream");
        std::process::exit(1);
    };
    let t0 = filmcraft_time::Tick(info.duration.0 * 3 / 10);
    if src.video_frame(filmcraft_media::FrameRequest::full(t0)).is_err() {
        eprintln!("warm frame failed");
        std::process::exit(1);
    }
    let dt = filmcraft_time::FrameRate { num: v.frame_rate.num.max(1), den: v.frame_rate.den.max(1) }.frame_duration();
    let start = Instant::now();
    for i in 0..n {
        let t = filmcraft_time::Tick(t0.0.saturating_add((i as i64 + 1).saturating_mul(dt.0)));
        if src.video_frame(filmcraft_media::FrameRequest::full(t)).is_err() {
            eprintln!("frame {i} failed");
            std::process::exit(1);
        }
    }
    let el = start.elapsed().as_secs_f64().max(1e-6);
    println!("{:.1} ms/frame ({n} frames, {:.1} fps) at {}x{}", el / n as f64 * 1000.0, n as f64 / el, v.width, v.height);
}

/// Open one `.insv` by path, joining the sibling parts when it belongs to a spanned
/// recording (the same discovery `insv_stitch` does, over the filesystem).
fn open(path: &Path) -> Result<SharedSource, MediaError> {
    let single = |p: &Path| -> Result<SharedSource, MediaError> {
        let r = filmcraft_media::reader::FileReader::open(p).map_err(|e| MediaError::Io(format!("{}: {e}", p.display())))?;
        let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        filmcraft_media::reader::open_reader(
            &name,
            Arc::new(r) as filmcraft_media::SharedReader,
            &filmcraft_codecs::reader_openers(),
            &filmcraft_codecs::openers(),
        )
    };
    let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_string()) else { return single(path) };
    let Some((prefix, _)) = filmcraft_codecs::insta360::spanned_part(&name) else { return single(path) };
    let Some(dir) = path.parent() else { return single(path) };
    let Ok(entries) = std::fs::read_dir(dir) else { return single(path) };
    let mut parts: Vec<(u32, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            filmcraft_codecs::insta360::spanned_part(&n).filter(|(p, _)| *p == prefix).map(|(_, seg)| (seg, e.path()))
        })
        .collect();
    parts.sort_by_key(|(seg, _)| *seg);
    if parts.len() < 2 {
        return single(path);
    }
    let opened: Result<Vec<SharedSource>, MediaError> = parts.iter().map(|(_, p)| single(p)).collect();
    match opened.and_then(|v| filmcraft_codecs::SpannedSource::new(&name, v).map(|s| Arc::new(s) as SharedSource)) {
        // parts that will not join (or did not open): the file still plays standalone
        Ok(s) => Ok(s),
        Err(_) => single(path),
    }
}
