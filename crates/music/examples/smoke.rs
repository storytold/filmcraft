//! Generate one cue with the local provider and write it as a WAV.
//!
//! `cargo run -p filmcraft-music --features native,download --example smoke -- <models dir> <out.wav> [seconds] [seed]`

use std::collections::HashMap;
use std::time::Instant;

use filmcraft_music::{Params, catalog, wav};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (Some(models), Some(out)) = (args.get(1), args.get(2)) else {
        eprintln!("usage: smoke <models dir> <out.wav> [seconds] [seed]");
        return;
    };
    let seconds: f64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(30.0);
    let seed: u64 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(1234);
    let t0 = Instant::now();
    let provider = match catalog::stable_audio_local(Some(std::path::Path::new(models)), &HashMap::new()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("cannot start: {e}");
            return;
        }
    };
    eprintln!("loaded in {:.1}s", t0.elapsed().as_secs_f64());
    let params = Params {
        prompt: "Warm, slow ambient piano and strings, reflective documentary underscore".into(),
        bpm: Some(70.0),
        duration: seconds,
        seed: Some(seed),
        ..Default::default()
    };
    let t1 = Instant::now();
    let audio = provider.generate_with_progress(&params, &mut |p| {
        eprintln!("  {:>3.0}%  {:.1}s", p * 100.0, t1.elapsed().as_secs_f64());
        true
    });
    match audio {
        Ok(a) => {
            eprintln!("generated {:.1}s of audio in {:.1}s", a.seconds(), t1.elapsed().as_secs_f64());
            if let Err(e) = std::fs::write(out, wav::encode_f32(&a)) {
                eprintln!("write failed: {e}");
            }
        }
        Err(e) => eprintln!("generation failed: {e}"),
    }
}
