//! Decode-rate probe: N pullers requesting consecutive frames of one INSV, to
//! separate per-sample decode cost from lock and driver serialization.
//! `cargo run --release -p filmcraft-platform --example hwrate -- in.insv [threads] [frames]`.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use filmcraft_media::SharedSource;

fn open(path: &Path) -> Result<SharedSource, filmcraft_media::MediaError> {
    let r = filmcraft_media::reader::FileReader::open(path).map_err(|e| filmcraft_media::MediaError::Io(format!("{}: {e}", path.display())))?;
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    filmcraft_media::reader::open_reader(&name, Arc::new(r) as filmcraft_media::SharedReader, &filmcraft_codecs::reader_openers(), &filmcraft_codecs::openers())
}

fn main() {
    filmcraft_platform::register();
    filmcraft_codecs::hw::set_hardware_decoding(true);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = args.first().map(Path::new).filter(|p| p.is_file()) else {
        eprintln!("usage: hwrate in.insv [threads] [frames] [seekbase]");
        std::process::exit(2);
    };
    let threads: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1).clamp(1, 32);
    let frames: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(96).clamp(1, 10_000);
    let base: f64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(600.0);
    let src = open(path).expect("open");
    let info = src.info();
    let rate = info.video.clone().expect("video").frame_rate;
    let dt = rate.frame_duration();
    let next = Arc::new(AtomicUsize::new(0));
    let t0 = Instant::now();
    let mut joins = Vec::new();
    for _ in 0..threads {
        let (src, next, dt) = (src.clone(), next.clone(), dt);
        joins.push(std::thread::spawn(move || {
            let mut got = 0usize;
            loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                if i >= frames {
                    break;
                }
                let t = filmcraft_time::Tick::from_seconds_f64(base) + filmcraft_time::Tick(dt.0 * (i as i64 + 1));
                match src.video_frame(filmcraft_media::FrameRequest::full(t)) {
                    Ok(_) => got += 1,
                    Err(e) => eprintln!("frame {i}: {e}"),
                }
            }
            got
        }));
    }
    let done: usize = joins.into_iter().map(|j| j.join().unwrap_or(0)).sum();
    let secs = t0.elapsed().as_secs_f64();
    let g = filmcraft_codecs::gop_stats();
    println!(
        "{} threads: {done} frames in {secs:.1}s = {:.1} frames/s | samples {} seek {} skipped {} cache-hits {} ({}) cached {:.0}MB live-decoders {}",
        threads,
        done as f64 / secs,
        g.decoded,
        g.seeks,
        g.skipped,
        g.hits,
        g.misses,
        filmcraft_codecs::cached_bytes() as f64 / 1e6,
        filmcraft_codecs::live_decoders()
    );
}
