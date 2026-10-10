//! `cargo run -p filmcraft-tts --release --features kokoro --example kokoro_say -- <model dir> <voice.pt> "<phonemes>" out.wav [speed]`
//!
//! Speaks Kokoro phonemes with a voice pack and writes a 24 kHz mono 32-bit float WAV.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<String> = std::env::args().collect();
    let (Some(dir), Some(voice), Some(ph), Some(out)) = (a.get(1), a.get(2), a.get(3), a.get(4)) else {
        eprintln!("usage: kokoro_say <model dir> <voice.pt> <phonemes> <out.wav> [speed]");
        std::process::exit(2);
    };
    let speed: f64 = a.get(5).and_then(|s| s.parse().ok()).unwrap_or(1.0);
    let t0 = std::time::Instant::now();
    let k = filmcraft_tts::kokoro::Kokoro::load(std::path::Path::new(dir))?;
    let v = filmcraft_tts::kokoro::VoicePack::load(std::path::Path::new(voice))?;
    let t1 = std::time::Instant::now();
    let s = k.speak(ph, &v, speed, 1)?;
    let t2 = std::time::Instant::now();
    let rate = filmcraft_tts::kokoro::KOKORO_RATE;
    let mut w = Vec::with_capacity(44 + s.len() * 4);
    let data = (s.len() * 4) as u32;
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&3u16.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * 4).to_le_bytes());
    w.extend_from_slice(&4u16.to_le_bytes());
    w.extend_from_slice(&32u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data.to_le_bytes());
    for x in &s {
        w.extend_from_slice(&x.to_le_bytes());
    }
    std::fs::write(out, w)?;
    let secs = s.len() as f64 / f64::from(rate);
    eprintln!(
        "load {:.2}s, synth {:.2}s for {secs:.2}s of audio (×{:.1} real time), peak {:.3}",
        (t1 - t0).as_secs_f64(),
        (t2 - t1).as_secs_f64(),
        secs / (t2 - t1).as_secs_f64(),
        s.iter().fold(0f32, |m, x| m.max(x.abs()))
    );
    Ok(())
}
