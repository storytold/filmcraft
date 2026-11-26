//! Stitch an Insta360 `.insv` to an equirectangular frame: `cargo run --release -p
//! filmcraft-codecs --example insv_stitch -- in.insv [out.ppm] [seconds]`.
//!
//! Opens the file with a random-access reader (the INSV source decodes both fisheye
//! tracks and projects them with the trailer's factory calibration; the parts of a
//! spanned recording — `VID_…_00_054.insv`, `…_055`, … — are joined into one clip),
//! grabs the frame at `seconds` (default: 30% in) and writes it as binary PPM.
//! Prints the container, size and rate.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use filmcraft_media::{MediaError, SharedSource};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: insv_stitch in.insv [out.ppm] [seconds]");
        std::process::exit(2);
    }
    let path = Path::new(&args[1]);
    let out = args.get(2).map(String::as_str).unwrap_or("out.ppm");
    let secs: f64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(-1.0);
    let src = match open(path) {
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
    println!("container: {}", info.container);
    println!("{}x{} {:.3} fps, {:.1}s", v.width, v.height, v.frame_rate.as_f64(), info.duration.seconds());
    let t = if secs >= 0.0 { filmcraft_time::Tick::from_seconds_f64(secs) } else { filmcraft_time::Tick(info.duration.0 * 3 / 10) };
    // Optional 4th arg: decode that many CONSECUTIVE frames from t (the seam's
    // temporal smoothing only engages when frames arrive in order, as in playback).
    let frames: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(1).clamp(1, 600);
    let dt = filmcraft_time::FrameRate { num: v.frame_rate.num.max(1), den: v.frame_rate.den.max(1) }.frame_duration();
    let mut f = match src.video_frame(filmcraft_media::FrameRequest::full(t)) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    for i in 0..frames {
        if i > 0 {
            let ti = filmcraft_time::Tick(t.0.saturating_add((i as i64).saturating_mul(dt.0)));
            f = match src.video_frame(filmcraft_media::FrameRequest::full(ti)) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            };
        }
        // With more than one frame requested, write each as out.NNN.ppm so the
        // seam's temporal behaviour can be inspected across the range.
        let path = if frames > 1 { PathBuf::from(format!("{out}.{i:03}.ppm")) } else { PathBuf::from(out) };
        let rgba = f.to_rgba8();
        let mut ppm = format!("P6\n{} {}\n255\n", f.width, f.height).into_bytes();
        ppm.extend(rgba.as_chunks::<4>().0.iter().flat_map(|px| px[..3].iter().copied()));
        match std::fs::write(&path, &ppm) {
            Ok(()) => println!("wrote {} ({}x{})", path.display(), f.width, f.height),
            Err(e) => {
                eprintln!("{}: {e}", path.display());
                std::process::exit(1);
            }
        }
    }
}

/// Open one `.insv` by path, joining the sibling parts when it belongs to a spanned
/// recording (the same discovery the engine's media pool does, over the filesystem).
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
