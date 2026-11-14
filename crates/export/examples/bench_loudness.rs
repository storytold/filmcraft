//! Wall time of a WAV export with Export ▸ Effects ▸ Loudness Normalization on and off, over a
//! timeline of procedural demo audio on several tracks, each clip carrying Studio Reverb, DeNoise
//! and Dynamics. The difference between the two is what the loudness pass costs.
//!
//! ```text
//! cargo run --release -p filmcraft-export --example bench_loudness -- [--runs N] [--secs S] [--tracks T] [--channels 1|2|6]
//! ```
//!
//! Prints each run with a hash of the exported file, and the median and minimum of each mode.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;
use std::time::Instant;

use filmcraft_export::{ExportSettings, Format, LoudnessNormalization, Progress};
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{DemoScene, Generator, MediaSource, SharedSource};
use filmcraft_project::{ItemId, ItemKind, Label, MediaClip, MediaRef, Project, SequenceSettings, TrackKind};
use filmcraft_time::{FrameRate, Tick, TimeRange};

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

fn project(secs: u32, tracks: usize) -> (Arc<Project>, ItemId, ItemId, SharedSource) {
    let (w, h, rate) = (64, 36, FrameRate::FPS_23_976);
    let dur = rate.tick_of(rate.frame_at(Tick::from_seconds_f64(f64::from(secs))));
    let scene = Generator::Demo(DemoScene::CityNight);
    let src = GeneratorSource::new(scene.clone(), w, h, rate, dur);
    let mut p = Project::new("bench");
    let clip = MediaClip {
        media: MediaRef::Generator(scene),
        info: src.info().clone(),
        interpret: Default::default(),
        mark_in: None,
        mark_out: None,
        markers: vec![],
        offline: false,
        proxy: None,
        identity: None,
    };
    let item = p.add_item("scene", Label::Iris, ItemKind::Media(Box::new(clip)), None);
    let seq = p.new_sequence("s", SequenceSettings { width: w, height: h, frame_rate: rate, ..Default::default() }, 1, tracks, None);
    let mut items = Vec::new();
    for t in 0..tracks {
        // each track starts at its own source offset, so the tracks differ
        let off = Tick::from_seconds_f64(0.37 * t as f64);
        let range = TimeRange::new(off, Tick(dur.0 - off.0));
        let mut a = p.make_track_item(item, TrackKind::Audio, Tick::ZERO, range, rate).expect("audio item");
        for id in ["studio_reverb", "denoise", "dynamics"] {
            a.effects.push(filmcraft_project::find_effect(id).expect("effect").instance());
        }
        items.push(a);
    }
    let q = p.sequence_mut(seq).expect("sequence");
    for (t, a) in q.audio_tracks.iter_mut().zip(items) {
        t.items.push(a);
    }
    (Arc::new(p), seq, item, Arc::new(src))
}

/// FNV-1a of a file, read through a fixed buffer (exports can be larger than memory).
fn fnv1a(path: &std::path::Path) -> u64 {
    use std::io::Read;
    let mut file = std::fs::File::open(path).expect("output");
    let mut buf = vec![0u8; 1 << 20];
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    loop {
        let n = file.read(&mut buf).expect("read output");
        if n == 0 {
            return hash;
        }
        for b in &buf[..n] {
            hash = (hash ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3);
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let runs: usize = arg(&args, "--runs").and_then(|v| v.parse().ok()).unwrap_or(3).max(1);
    let secs: u32 = arg(&args, "--secs").and_then(|v| v.parse().ok()).unwrap_or(600).max(1);
    let tracks: usize = arg(&args, "--tracks").and_then(|v| v.parse().ok()).unwrap_or(4).max(1);
    let channels: u32 = arg(&args, "--channels").and_then(|v| v.parse().ok()).unwrap_or(2);
    let (p, seq, item, shared) = project(secs, tracks);
    let provider = move |id: ItemId| (id == item).then(|| shared.clone());
    let dir = std::env::temp_dir().join(format!("filmcraft-bench-loudness-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let out = dir.join("out.wav");
    println!("{secs} s, {tracks} tracks (Studio Reverb + DeNoise + Dynamics per clip), {channels}-channel 32-bit float WAV");
    for loud in [false, true] {
        let mut times = Vec::new();
        for i in 0..runs {
            let mut s = ExportSettings { format: Format::Wav, path: out.to_string_lossy().into_owned(), ..Default::default() };
            s.audio.channels = channels;
            s.audio.bits = 32;
            s.audio.float = true;
            s.effects.loudness = LoudnessNormalization { enabled: loud, target_lufs: -14.0, true_peak_dbtp: -1.0 };
            let prog = Progress::default();
            let t0 = Instant::now();
            filmcraft_export::export(&p, seq, &s, &provider, &prog).expect("export");
            let dt = t0.elapsed().as_secs_f64();
            let rep = prog.loudness.lock().ok().and_then(|g| *g);
            // FNV-1a of the file, to check that a change leaves the exported audio identical
            let hash = fnv1a(&out);
            eprintln!("loudness {loud} run {i}: {dt:.3} s, output {hash:016x}, {rep:?}");
            times.push(dt);
        }
        times.sort_by(f64::total_cmp);
        println!("loudness {}: runs {runs}, median {:.3} s, min {:.3} s", if loud { "on " } else { "off" }, times[runs / 2], times[0]);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
