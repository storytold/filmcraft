//! Run the rolling recogniser over a raw mono 16 kHz f32 file at real-time pacing, as a live take
//! would feed it, and report when each word was committed.
//!
//! ```sh
//! ffmpeg -i speech.flac -ar 16000 -ac 1 -f f32le speech.f32
//! cargo run --release -p filmcraft-speech --features whisper --example stream -- <model dir> speech.f32 [--lang=en] [--window=6] [--hop=1.5] [--speed=1]
//! ```
//!
//! For each committed word: its time in the audio, the lag from its end to the moment it was
//! committed, and the text. At the end: the words, the lag (median, 90th percentile, longest), the
//! passes run, skipped and silent, the busy share of the worker, and the CPU time of the process.
//! With `<file>.txt` beside the audio, also the word error rate of the committed text.

use std::sync::Arc;
use std::time::{Duration, Instant};

use filmcraft_speech::Transcriber;
use filmcraft_speech::stream::{RollingConfig, RollingRecognizer, StreamingRecognizer};

fn cpu_seconds() -> Option<f64> {
    // utime + stime of this process, in clock ticks (Linux)
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let rest = stat.rsplit_once(')')?.1;
    let f: Vec<&str> = rest.split_whitespace().collect();
    let ticks: f64 = f.get(11)?.parse::<f64>().ok()? + f.get(12)?.parse::<f64>().ok()?;
    Some(ticks / 100.0)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("model dir"));
    let file = std::path::PathBuf::from(args.next().expect("audio file"));
    let mut config = RollingConfig::default();
    let mut speed = 1.0f64;
    for a in args {
        if let Some(v) = a.strip_prefix("--lang=") {
            config.language = Some(v.to_string());
        } else if let Some(v) = a.strip_prefix("--window=") {
            config.window = v.parse().expect("window seconds");
        } else if let Some(v) = a.strip_prefix("--hop=") {
            config.hop = v.parse().expect("hop seconds");
        } else if let Some(v) = a.strip_prefix("--speed=") {
            speed = v.parse().expect("speed");
        }
    }
    let id = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let audio: Vec<f32> = std::fs::read(&file).expect("read").as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
    let model: Arc<dyn Transcriber> = Arc::new(filmcraft_speech::whisper::Whisper::load(&dir, &id).expect("load"));
    let mut r = RollingRecognizer::new(id.clone(), Box::new(move || Ok(model)), config.clone());
    let cpu0 = cpu_seconds();
    let t0 = Instant::now();
    let block = 1_600; // 100 ms
    let mut pushed = 0usize;
    let mut words = Vec::new();
    let mut lags = Vec::new();
    let total = audio.len() as f64 / 16_000.0;
    // silence after the audio, as a narrator pausing before stopping the take
    let tail = vec![0.0f32; 3 * 16_000];
    let stream: Vec<f32> = audio.iter().copied().chain(tail).collect();
    while pushed < stream.len() {
        let due = ((t0.elapsed().as_secs_f64() * speed) * 16_000.0) as usize;
        if due > pushed {
            let end = due.min(stream.len()).min(pushed + block * 10);
            r.push(&stream[pushed..end]);
            pushed = end;
        }
        let now = pushed as f64 / 16_000.0;
        for w in r.poll().committed {
            let end = w.end.0 as f64 / 254_016_000_000.0;
            let lag = now - end;
            println!("{:7.2}s  +{lag:4.2}s  {}", w.start.0 as f64 / 254_016_000_000.0, w.text);
            lags.push(lag);
            words.push(w.text);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // let the last pass finish
    std::thread::sleep(Duration::from_secs(3));
    for w in r.poll().committed {
        println!("{:7.2}s  (after the end)  {}", w.start.0 as f64 / 254_016_000_000.0, w.text);
        words.push(w.text);
    }
    let wall = t0.elapsed().as_secs_f64();
    let stats = r.stats();
    r.stop();
    lags.sort_by(f64::total_cmp);
    let q = |p: f64| lags.get(((lags.len().saturating_sub(1)) as f64 * p) as usize).copied().unwrap_or(f64::NAN);
    println!("model {id}, window {}s, hop {}s, {total:.1}s of audio at {speed}x", config.window, config.hop);
    println!("{} words committed; lag median {:.2}s, p90 {:.2}s, max {:.2}s", words.len(), q(0.5), q(0.9), q(1.0));
    println!(
        "passes {} run, {} skipped, {} silent; worker busy {:.0}% of {wall:.1}s; longest pass {:.2}s",
        stats.passes,
        stats.skipped,
        stats.silent,
        stats.busy_seconds / wall * 100.0,
        stats.longest_pass_seconds
    );
    if let (Some(a), Some(b)) = (cpu0, cpu_seconds()) {
        println!("process CPU {:.1}s ({:.0}% of one core over the run)", b - a, (b - a) / wall * 100.0);
    }
    if let Ok(reference) = std::fs::read_to_string(file.with_extension("txt")) {
        let wer = filmcraft_speech::word_error_rate(&reference, &words.join(" "));
        println!("WER of the committed text: {:.1}%", wer * 100.0);
    }
}
