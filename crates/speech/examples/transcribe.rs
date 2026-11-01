//! Transcribe raw mono 16 kHz f32 files and report the word error rate against `<file>.txt`.
//!
//! ```sh
//! ffmpeg -i speech.flac -ar 16000 -ac 1 -f f32le speech.f32
//! cargo run --release -p filmcraft-speech --features whisper --example transcribe -- <model dir> [--words] [--diarize] speech.f32…
//! ```

use filmcraft_speech::{Options, Transcriber, word_error_rate};

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("model dir"));
    let id = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let t0 = std::time::Instant::now();
    let w: Box<dyn Transcriber> = if id.starts_with("parakeet") {
        Box::new(filmcraft_speech::parakeet::Parakeet::load(&dir, &id).expect("load"))
    } else {
        Box::new(filmcraft_speech::whisper::Whisper::load(&dir, &id).expect("load"))
    };
    eprintln!("loaded {id} in {:.2}s", t0.elapsed().as_secs_f64());
    let mut show_words = false;
    let mut diarize = false;
    let mut language = None;
    let (mut errs, mut words, mut audio_s, mut cpu_s) = (0.0, 0usize, 0.0, 0.0);
    for a in args {
        match a.as_str() {
            "--words" => show_words = true,
            "--diarize" => diarize = true,
            l if l.starts_with("--lang=") => language = Some(l[7..].to_string()),
            p => {
                let audio: Vec<f32> = std::fs::read(p).expect("read").as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
                let t1 = std::time::Instant::now();
                let opts = Options { language: language.clone(), diarize, ..Default::default() };
                let t = w.transcribe(&audio, &opts, &mut |_, _| true).expect("transcribe");
                let secs = t1.elapsed().as_secs_f64();
                audio_s += audio.len() as f64 / 16_000.0;
                cpu_s += secs;
                let hyp = t.text();
                print!("{p} [{} {:.1}s]: {hyp}", t.language, secs);
                if let Ok(r) = std::fs::read_to_string(std::path::Path::new(p).with_extension("txt")) {
                    let n = r.split_whitespace().count();
                    let e = word_error_rate(&r, &hyp);
                    errs += e * n as f64;
                    words += n;
                    print!("  (WER {:.1}%)", e * 100.0);
                }
                println!();
                if show_words {
                    for x in &t.words {
                        println!(
                            "  {:7.2} {:7.2} {:>3} {}",
                            x.start.0 as f64 / 254_016_000_000.0,
                            x.end.0 as f64 / 254_016_000_000.0,
                            x.speaker.map(|s| s.to_string()).unwrap_or_default(),
                            x.text
                        );
                    }
                }
            }
        }
    }
    if words > 0 {
        println!(
            "total WER {:.2}% over {words} words; {audio_s:.1}s of audio in {cpu_s:.1}s ({:.1}× realtime)",
            errs / words as f64 * 100.0,
            audio_s / cpu_s.max(1e-9)
        );
    }
}
