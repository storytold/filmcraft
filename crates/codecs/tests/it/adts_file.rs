//! Standalone ADTS `.aac` files (#1291): File ▸ Import offers them, and FilmCraft's own AAC export
//! writes them (#814), so they must open as audio. The streams are made with our AAC encoder, which
//! prefixes each access unit with an ADTS header (ISO/IEC 13818-7 §6.2).

use std::sync::Arc;

use filmcraft_aac::{Encoder, EncoderConfig};

const RATE: u32 = 48_000;

/// `secs` of a 440 Hz tone at half scale in every channel, as an ADTS stream.
fn adts_tone(channels: usize, secs: u32) -> Vec<u8> {
    let tone: Vec<f32> = (0..RATE * secs).map(|i| (std::f32::consts::TAU * 440.0 * i as f32 / RATE as f32).sin() * 0.5).collect();
    let planar: Vec<&[f32]> = (0..channels).map(|_| tone.as_slice()).collect();
    let mut enc = Encoder::new(EncoderConfig { adts: true, ..EncoderConfig::cbr(RATE, channels, 128_000) }).unwrap();
    let mut out = Vec::new();
    for au in enc.encode(&planar).into_iter().chain(enc.flush()) {
        out.extend(au);
    }
    out
}

fn rms(s: &[f32]) -> f32 {
    (s.iter().map(|v| v * v).sum::<f32>() / s.len().max(1) as f32).sqrt()
}

#[test]
fn adts_aac_files_import_as_audio() {
    let src = filmcraft_codecs::open_bytes("tone.aac", Arc::from(adts_tone(2, 1))).unwrap();
    let info = src.info();
    let a = info.audio().unwrap();
    assert_eq!((a.sample_rate, a.channels), (RATE, 2));
    // One second plus the encoder's priming and the last frame's padding (at most two frames).
    let secs = info.duration.seconds();
    assert!((1.0..=1.0 + 2048.0 / RATE as f64).contains(&secs), "duration {secs}");
    let buf = src.audio(RATE as i64 / 4, RATE as usize / 2, RATE).unwrap();
    for (c, ch) in buf.channels.iter().enumerate() {
        let level = rms(ch);
        assert!((level - 0.5 / 2f32.sqrt()).abs() < 0.05, "channel {c} rms {level}");
    }
}

#[test]
fn damaged_adts_files_fail_without_panicking() {
    let good = adts_tone(1, 1);
    let mut cases: Vec<Vec<u8>> = (0..good.len().min(4096)).step_by(7).map(|n| good[..n].to_vec()).collect();
    for i in (0..good.len()).step_by(97) {
        let mut b = good.clone();
        b[i] ^= 1 << (i % 8);
        cases.push(b);
    }
    // A frame length of zero, and one running past the end of the file.
    let mut zero = good.clone();
    zero[3] &= !3;
    zero[4] = 0;
    zero[5] &= 0x1F;
    cases.push(zero);
    let mut long = good[..64].to_vec();
    long[3] |= 3;
    long[4] = 0xFF;
    cases.push(long);
    for (i, b) in cases.into_iter().enumerate() {
        let r = std::panic::catch_unwind(|| filmcraft_codecs::open_bytes("damaged.aac", Arc::from(b)).map(|s| s.audio(0, 4800, RATE).is_ok()));
        assert!(r.is_ok(), "case {i} panicked");
    }
}
