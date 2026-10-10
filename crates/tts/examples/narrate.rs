//! `cargo run -p filmcraft-tts --release --features kokoro --example narrate -- <models dir> <voice id> <script.txt> <out.wav>`
//!
//! Speaks a script file with any FilmCraft voice (built in or natural) through the full pipeline
//! (normalizer, pronunciation, Kokoro) and writes a mono 32-bit float WAV.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<String> = std::env::args().collect();
    let (Some(models), Some(id), Some(script), Some(out)) = (a.get(1), a.get(2), a.get(3), a.get(4)) else {
        eprintln!("usage: narrate <models dir> <voice id> <script.txt> <out.wav>");
        std::process::exit(2);
    };
    let text = std::fs::read_to_string(script)?;
    let t0 = std::time::Instant::now();
    let v = filmcraft_tts::voice_in(id, Some(std::path::Path::new(models)))?;
    let t1 = std::time::Instant::now();
    let audio = v.synthesize(&text, &filmcraft_tts::Params::default())?;
    let t2 = std::time::Instant::now();
    let rate = audio.sample_rate;
    let data = (audio.samples.len() * 4) as u32;
    let mut w = Vec::with_capacity(44 + data as usize);
    for part in [
        &b"RIFF"[..],
        &(36 + data).to_le_bytes(),
        b"WAVEfmt ",
        &16u32.to_le_bytes(),
        &3u16.to_le_bytes(),
        &1u16.to_le_bytes(),
        &rate.to_le_bytes(),
        &(rate * 4).to_le_bytes(),
        &4u16.to_le_bytes(),
        &32u16.to_le_bytes(),
        b"data",
        &data.to_le_bytes(),
    ] {
        w.extend_from_slice(part);
    }
    for x in &audio.samples {
        w.extend_from_slice(&x.to_le_bytes());
    }
    std::fs::write(out, w)?;
    eprintln!(
        "{id}: load {:.2}s, synth {:.2}s for {:.2}s of audio (×{:.1} real time)",
        (t1 - t0).as_secs_f64(),
        (t2 - t1).as_secs_f64(),
        audio.seconds(),
        audio.seconds() / (t2 - t1).as_secs_f64()
    );
    Ok(())
}
