//! Label speakers for an existing transcript against its audio and print the speaker turns.
//!
//! ```sh
//! cargo run --release -p filmcraft-speech --example diarize_words -- speech.f32 transcript.json [max_speakers] [labelled.json]
//! ```
//! `speech.f32` is raw mono 16 kHz f32 (`ffmpeg -i x -ar 16000 -ac 1 -f f32le x.f32`); the JSON is a
//! `filmcraft_project::Transcript`. Prints one line per speaker turn (start, end, speaker, words, mean
//! level in dBFS) and a summary, so clustering can be judged without re-running recognition.

use filmcraft_project::Transcript;
use filmcraft_speech::diarize::{Params, diarize};

fn main() {
    let mut args = std::env::args().skip(1);
    let audio_path = args.next().expect("audio.f32");
    let json_path = args.next().expect("transcript.json");
    let max_speakers: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(6);
    let out_path = args.next();
    let audio: Vec<f32> = std::fs::read(&audio_path).expect("read audio").as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
    let mut t: Transcript = serde_json::from_str(&std::fs::read_to_string(&json_path).expect("read json")).expect("parse json");
    t.normalize();
    let t0 = std::time::Instant::now();
    let n = diarize(&audio, &mut t, &Params { max_speakers, ..Default::default() });
    let secs = t0.elapsed().as_secs_f64();
    let sec = |x: filmcraft_time::Tick| x.0 as f64 / 254_016_000_000.0;
    let level = |a: f64, b: f64| -> f64 {
        let (i, j) = (((a * 16_000.0) as usize).min(audio.len()), ((b * 16_000.0) as usize).min(audio.len()));
        if j <= i {
            return -120.0;
        }
        let e = audio[i..j].iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>() / (j - i) as f64;
        10.0 * (e + 1e-12).log10()
    };
    let mut i = 0;
    let mut counts = vec![0usize; n.max(1)];
    while i < t.words.len() {
        let sp = t.words[i].speaker.unwrap_or(0);
        let mut j = i;
        while j < t.words.len() && t.words[j].speaker.unwrap_or(0) == sp {
            j += 1;
        }
        let (a, b) = (sec(t.words[i].start), sec(t.words[j - 1].end));
        let text: Vec<&str> = t.words[i..j].iter().map(|w| w.text.as_str()).collect();
        let text = text.join(" ");
        let shown: String = text.chars().take(90).collect();
        println!("{a:8.2} {b:8.2}  S{}  {:6.1} dBFS  {}", sp + 1, level(a, b), shown);
        counts[sp as usize] += j - i;
        i = j;
    }
    println!("speakers: {n} ({:?} words each) in {secs:.2}s", counts);
    if let Some(out) = out_path {
        std::fs::write(&out, serde_json::to_string(&t).expect("json")).expect("write");
    }
}
