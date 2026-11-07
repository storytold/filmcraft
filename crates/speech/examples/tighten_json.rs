//! Re-apply the word-bound passes (`filmcraft_speech::vad::tighten_words`) to a saved transcript.
//!
//! ```sh
//! cargo run --release -p filmcraft-speech --example tighten_json -- speech.f32 transcript.json out.json
//! ```
use filmcraft_project::Transcript;

fn main() {
    let mut args = std::env::args().skip(1);
    let audio_path = args.next().expect("audio.f32");
    let json_path = args.next().expect("transcript.json");
    let out_path = args.next().expect("out.json");
    let audio: Vec<f32> = std::fs::read(&audio_path).expect("read audio").as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
    let mut t: Transcript = serde_json::from_str(&std::fs::read_to_string(&json_path).expect("read json")).expect("parse json");
    t.normalize();
    let t0 = std::time::Instant::now();
    filmcraft_speech::vad::tighten_words(&audio, &mut t.words);
    eprintln!("tightened {} words in {:.2}s", t.words.len(), t0.elapsed().as_secs_f64());
    std::fs::write(&out_path, serde_json::to_string(&t).expect("json")).expect("write");
}
