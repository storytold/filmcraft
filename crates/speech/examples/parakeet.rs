//! Transcribe raw mono 16 kHz f32 files with Parakeet TDT and report the word error rate against
//! `<file>.txt`; optionally dump features, encoder output and tokens for parity checks.
//!
//! ```sh
//! ffmpeg -i speech.flac -ar 16000 -ac 1 -f f32le speech.f32
//! cargo run --release -p filmcraft-speech --features parakeet --example parakeet -- <model dir or .nemo> [--words] [--lang=de] [--dump=<dir>] speech.f32…
//! ```
//!
//! `--dump` writes `<name>.mel.f32` (`n_mels × frames`), `<name>.enc.f32` (`frames × d_model`) and
//! `<name>.json` (tokens with frames and durations, words with times in seconds, the model's own
//! word times before tightening as `raw_words`); `--dump-words` writes only the JSON words.

use filmcraft_speech::parakeet::Parakeet;
use filmcraft_speech::{Options, Transcriber, word_error_rate};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = std::path::PathBuf::from(args.next().expect("model dir or .nemo file"));
    let t0 = std::time::Instant::now();
    let id = path.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let p = if path.is_dir() { Parakeet::load(&path, &id) } else { Parakeet::load_file(&path, &id) }.expect("load");
    eprintln!("loaded {id} in {:.2}s", t0.elapsed().as_secs_f64());
    let (mut show_words, mut diarize, mut language, mut dump, mut words_only) = (false, false, None, None, false);
    let (mut errs, mut words, mut audio_s, mut cpu_s) = (0.0, 0usize, 0.0, 0.0);
    let secs = |t: filmcraft_time::Tick| t.0 as f64 / 254_016_000_000.0;
    for a in args {
        match a.as_str() {
            "--words" => show_words = true,
            "--diarize" => diarize = true,
            l if l.starts_with("--lang=") => language = Some(l[7..].to_string()),
            d if d.starts_with("--dump=") => dump = Some(std::path::PathBuf::from(&d[7..])),
            d if d.starts_with("--dump-words=") => {
                dump = Some(std::path::PathBuf::from(&d[13..]));
                words_only = true;
            }
            f => {
                let audio: Vec<f32> = std::fs::read(f).expect("read").as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
                let t1 = std::time::Instant::now();
                let opts = Options { language: language.clone(), diarize, ..Default::default() };
                let t = p.transcribe(&audio, &opts, &mut |_, _| true).expect("transcribe");
                let el = t1.elapsed().as_secs_f64();
                audio_s += audio.len() as f64 / 16_000.0;
                cpu_s += el;
                let hyp = t.text();
                print!("{f} [{} {el:.2}s]: {hyp}", t.language);
                if let Ok(r) = std::fs::read_to_string(std::path::Path::new(f).with_extension("txt")) {
                    let n = r.split_whitespace().count();
                    let e = word_error_rate(&r, &hyp);
                    errs += e * n as f64;
                    words += n;
                    print!("  (WER {:.1}%)", e * 100.0);
                }
                println!();
                if show_words {
                    for w in &t.words {
                        println!("  {:8.3} {:8.3} {:.2} {}", secs(w.start), secs(w.end), w.confidence, w.text);
                    }
                }
                if let Some(dir) = &dump {
                    std::fs::create_dir_all(dir).expect("dump dir");
                    let name = std::path::Path::new(f).file_stem().unwrap().to_string_lossy().to_string();
                    let raw = |v: &[f32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
                    // tensors and tokens only for clips encoded in one pass
                    let short = !words_only && audio.len() as f64 <= filmcraft_speech::parakeet::MAX_PIECE_SECONDS * 16_000.0;
                    let (mut frames, mut ef, mut toks) = (0, 0, Vec::new());
                    if short {
                        let (fr, mel) = p.features(&audio);
                        std::fs::write(dir.join(format!("{name}.mel.f32")), raw(&mel)).unwrap();
                        let (e, enc) = p.encode(&audio).expect("encode");
                        std::fs::write(dir.join(format!("{name}.enc.f32")), raw(&enc)).unwrap();
                        (frames, ef) = (fr, e);
                        toks = p.tokens(&audio, &mut |_| true).expect("tokens");
                    }
                    let raw_words = p.recognise(&audio, &mut |_| true).expect("recognise");
                    let json = serde_json::json!({
                        "frames": frames, "enc_frames": ef,
                        "tokens": toks.iter().map(|t| t.id).collect::<Vec<_>>(),
                        "timestamps": toks.iter().map(|t| t.frame).collect::<Vec<_>>(),
                        "durations": toks.iter().map(|t| t.duration).collect::<Vec<_>>(),
                        "text": p.detokenize(&toks.iter().map(|t| t.id).collect::<Vec<_>>()),
                        "words": t.words.iter().map(|w| serde_json::json!({"word": w.text, "start": secs(w.start), "end": secs(w.end), "confidence": w.confidence})).collect::<Vec<_>>(),
                        "raw_words": raw_words.iter().map(|w| serde_json::json!({"word": w.text, "start": secs(w.start), "end": secs(w.end)})).collect::<Vec<_>>(),
                        "seconds": el,
                    });
                    std::fs::write(dir.join(format!("{name}.json")), serde_json::to_string_pretty(&json).unwrap()).unwrap();
                }
            }
        }
    }
    if words > 0 {
        println!(
            "total WER {:.2}% over {words} words; {audio_s:.1}s of audio in {cpu_s:.1}s (RTF {:.4}, {:.1}x realtime)",
            errs / words as f64 * 100.0,
            cpu_s / audio_s.max(1e-9),
            audio_s / cpu_s.max(1e-9)
        );
    }
}
