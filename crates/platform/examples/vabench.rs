//! VA-API decode speed: decodes an MP4's video track through the hardware path several times and
//! prints milliseconds per picture (median, min, max over the runs) and pictures per second.
//!
//! `cargo run --release -p filmcraft-platform --example vabench -- <file.mp4> [runs]`
//!
//! Linux only; says so and exits when there is no VA-API decoder for the stream.

use std::time::Instant;

fn main() {
    if let Err(e) = run() {
        eprintln!("vabench: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("usage: vabench <file.mp4> [runs]")?;
    let runs: usize = match args.next() {
        Some(r) => r.parse().map_err(|_| format!("runs: not a number: {r}"))?,
        None => 7,
    };
    let runs = runs.clamp(1, 1000);
    let bytes = std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
    let file = filmcraft_isobmff::open(bytes.clone()).map_err(|e| e.to_string())?;
    let t = file.track_of_kind(filmcraft_isobmff::TrackKind::Video).ok_or("no video track")?;
    let track = file.tracks.get(t).ok_or("no video track")?;
    let entry = track.entries.first().ok_or("no sample entry")?;
    let mut samples = Vec::with_capacity(track.samples.len());
    for (i, s) in track.samples.iter().enumerate() {
        samples.push((file.read_sample(&bytes, t, i).map_err(|e| e.to_string())?, s.pts));
    }

    let mut decoder = match filmcraft_platform::vaapi_factory(entry) {
        Some(Ok(d)) => d,
        Some(Err(e)) => return Err(e.to_string()),
        None => return Err("no VA-API decoder for this stream (is libva on the library path, and is there a render node?)".into()),
    };
    println!("{path}: {} samples, {}", samples.len(), decoder.name());

    let mut per_picture = Vec::with_capacity(runs);
    // one warm-up run (driver start-up, page faults), not counted
    for run in 0..=runs {
        decoder.reset();
        let start = Instant::now();
        let mut pictures = 0usize;
        for (s, pts) in &samples {
            pictures += decoder.decode(s, *pts).map_err(|e| e.to_string())?.len();
        }
        pictures += decoder.flush().len();
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        if !decoder.name().starts_with("VA-API") {
            return Err(format!("left the hardware path ({})", decoder.name()));
        }
        if pictures == 0 {
            return Err("no pictures decoded".into());
        }
        if run > 0 {
            per_picture.push(elapsed / pictures as f64);
        }
    }
    per_picture.sort_by(f64::total_cmp);
    let (Some(min), Some(max), Some(median)) = (per_picture.first(), per_picture.last(), per_picture.get(per_picture.len() / 2)) else {
        return Err("no runs".into());
    };
    println!("ms/picture over {runs} runs: median {median:.2}  min {min:.2}  max {max:.2}  ({:.1} fps at the median)", 1000.0 / median);
    Ok(())
}
