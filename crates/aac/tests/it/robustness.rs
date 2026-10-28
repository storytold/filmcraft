//! Fuzz-style robustness: mutated access units, configs and ADTS streams must never panic, and the
//! encoder must accept hostile input (NaN, ±inf, > full scale, silence, odd chunk sizes).

use crate::common;

use common::*;
use filmcraft_aac::{AudioSpecificConfig, Decoder, Encoder, EncoderConfig, split_adts};

fn corpus() -> Vec<(Vec<u8>, Vec<Vec<u8>>)> {
    let mut out = Vec::new();
    for (ch, rate, br) in [(1usize, 44100u32, 96_000u32), (2, 48000, 128_000), (6, 48000, 256_000), (7, 48000, 256_000)] {
        let n = rate as usize / 2;
        let sig: Vec<Vec<f32>> = (0..ch).map(|c| if c % 2 == 0 { clicks(rate, n) } else { chords(rate, n, 0.01) }).collect();
        let mut enc = Encoder::new(EncoderConfig::cbr(rate, ch, br)).unwrap();
        let refs: Vec<&[f32]> = sig.iter().map(|v| v.as_slice()).collect();
        let mut aus = enc.encode(&refs);
        aus.extend(enc.flush());
        out.push((enc.audio_specific_config(), aus));
    }
    out
}

#[test]
fn mutated_access_units_never_panic() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut rand = move |n: usize| ((rng.next() * 0.5 + 0.5) * n as f32) as usize % n.max(1);
    let mut decoded = 0usize;
    let mut errors = 0usize;
    for (asc, aus) in corpus() {
        let mut dec = Decoder::new(&asc).unwrap();
        for round in 0..3000 {
            let mut au = aus[rand(aus.len())].clone();
            match round % 6 {
                0 => {
                    for _ in 0..1 + rand(8) {
                        let i = rand(au.len());
                        au[i] ^= 1 << rand(8);
                    }
                }
                1 => {
                    let i = rand(au.len());
                    au[i] = rand(256) as u8;
                }
                2 => au.truncate(rand(au.len() + 1)),
                3 => au.extend((0..rand(64)).map(|_| rand(256) as u8)),
                4 => au = (0..rand(2000)).map(|_| rand(256) as u8).collect(),
                _ => {
                    // splice two access units
                    let other = &aus[rand(aus.len())];
                    let cut = rand(au.len());
                    au.truncate(cut);
                    au.extend_from_slice(&other[rand(other.len())..]);
                }
            }
            match dec.decode(&au) {
                Ok(pcm) => {
                    decoded += 1;
                    assert!(pcm.iter().all(|c| c.len() == 1024));
                }
                Err(_) => errors += 1,
            }
        }
    }
    println!("mutated AUs: {decoded} decoded, {errors} rejected, 0 panics");
}

#[test]
fn random_configs_and_adts_never_panic() {
    let mut rng = Rng(12345);
    let mut byte = move || ((rng.next() * 0.5 + 0.5) * 256.0) as u8;
    for _ in 0..20000 {
        let len = (byte() % 12) as usize;
        let asc: Vec<u8> = (0..len).map(|_| byte()).collect();
        if let Ok(cfg) = AudioSpecificConfig::parse(&asc) {
            let _ = cfg.channels();
            if let Ok(mut d) = Decoder::from_config(cfg) {
                let au: Vec<u8> = (0..(byte() as usize * 3)).map(|_| byte()).collect();
                let _ = d.decode(&au);
            }
        }
    }
    for _ in 0..2000 {
        let mut s = vec![0xFF, 0xF1];
        s.extend((0..(byte() as usize * 8)).map(|_| byte()));
        if let Ok(frames) = split_adts(&s)
            && let Ok(mut d) = Decoder::from_adts(&frames[0].0)
        {
            for (_, au) in frames {
                let _ = d.decode(au);
            }
        }
    }
}

#[test]
fn encoder_handles_hostile_input() {
    for ch in [1usize, 2, 6] {
        let mut enc = Encoder::new(EncoderConfig::cbr(44100, ch, 64_000 * ch as u32)).unwrap();
        let n = 5000;
        let evil: Vec<f32> = (0..n)
            .map(|i| match i % 7 {
                0 => f32::NAN,
                1 => f32::INFINITY,
                2 => -f32::INFINITY,
                3 => 40.0,
                4 => -1e30,
                _ => ((i as f32) * 0.37).sin(),
            })
            .collect();
        let chans: Vec<&[f32]> = (0..ch).map(|_| evil.as_slice()).collect();
        let mut aus = Vec::new();
        for chunk in [0usize, 1, 7, 1023, 1024, 1025, 3000] {
            let c: Vec<&[f32]> = chans.iter().map(|s| &s[..chunk.min(s.len())]).collect();
            aus.extend(enc.encode(&c));
        }
        // fewer channels than configured: treated as silence
        aus.extend(enc.encode(&[&evil[..100]]));
        aus.extend(enc.flush());
        let mut dec = Decoder::new(&enc.audio_specific_config()).unwrap();
        for au in &aus {
            let pcm = dec.decode(au).unwrap();
            assert!(pcm.iter().flatten().all(|v| v.is_finite()));
        }
        let expected = (1 + 7 + 1023 + 1024 + 1025 + 3000 + 100usize).div_ceil(1024) + 1;
        assert_eq!(aus.len(), expected);
    }
    // empty stream: flush yields nothing
    let mut enc = Encoder::new(EncoderConfig::cbr(48000, 2, 128_000)).unwrap();
    assert!(enc.flush().is_empty());
    // bad configs are rejected
    assert!(Encoder::new(EncoderConfig::cbr(44000, 2, 128_000)).is_err());
    assert!(Encoder::new(EncoderConfig::cbr(44100, 9, 128_000)).is_err());
    assert!(Encoder::new(EncoderConfig::cbr(8000, 2, 2_000_000)).is_err());
}

#[test]
fn roundtrip_and_delay() {
    // Own encoder → own decoder: priming is exactly 1024 samples and the stream covers the input.
    let rate = 48000;
    let n = 10_000;
    let sig = chords(rate, n, 0.0);
    let mut enc = Encoder::new(EncoderConfig::cbr(rate, 1, 128_000)).unwrap();
    assert_eq!(enc.priming_samples(), 1024);
    let mut aus = enc.encode(&[&sig]);
    aus.extend(enc.flush());
    let mut dec = Decoder::new(&enc.audio_specific_config()).unwrap();
    let mut out = Vec::new();
    for au in &aus {
        out.extend(dec.decode(au).unwrap().remove(0));
    }
    assert!(out.len() >= n + 1024);
    let s = snr(&sig, &out[1024..1024 + n]);
    assert!(s > 15.0, "roundtrip SNR {s}");
    // no energy in the priming region beyond pre-echo of the first attack
    let e: f32 = out[..512].iter().map(|v| v * v).sum();
    assert!(e < 1e-3);
}
