//! Ogg Opus / Vorbis against ffmpeg: ffmpeg (libopus, its Vorbis encoder) writes the files and
//! decodes them as the oracle. Length must match exactly (pre-skip and end trimming), the signal
//! must match (libopus: SNR ≥ 50 dB on CELT music, ≥ 10 dB on SILK/hybrid speech, as in
//! `filmcraft-opus`'s own oracle; Vorbis: SNR ≥ 60 dB), and any random window must equal the
//! continuous decode (granule-position seeking with pre-roll).

mod common;

use std::path::{Path, PathBuf};

use common::*;
use filmcraft_codecs::OggSource;
use filmcraft_media::{MediaKind, MediaSource};

const MUSIC: &[&str] = &[
    "-f",
    "lavfi",
    "-i",
    "sine=frequency=440:sample_rate=48000",
    "-f",
    "lavfi",
    "-i",
    "anoisesrc=color=pink:sample_rate=48000:amplitude=0.2:seed=3",
    "-filter_complex",
    "[0:a][1:a]amerge=inputs=2[a]",
    "-map",
    "[a]",
];

fn spec(name: &str) -> Vec<&'static str> {
    match name {
        "ogg_opus_music.opus" => [MUSIC, &["-t", "3", "-c:a", "libopus", "-b:a", "160k", "-application", "audio"]].concat(),
        "ogg_opus_trim.ogg" => [MUSIC, &["-t", "1.2345", "-c:a", "libopus", "-b:a", "96k", "-frame_duration", "10"]].concat(),
        "ogg_opus_speech.opus" => {
            vec![
                "-f",
                "lavfi",
                "-i",
                "anoisesrc=color=brown:sample_rate=16000:amplitude=0.3:seed=5",
                "-t",
                "2",
                "-ac",
                "1",
                "-c:a",
                "libopus",
                "-b:a",
                "12k",
                "-application",
                "voip",
            ]
        }
        // the same signal on both channels: ffmpeg's experimental Vorbis encoder (stereo only)
        // codes an independent second channel so poorly (1.7 dB SNR against the source, with its
        // own decoder) that decoders legitimately disagree on it
        "ogg_vorbis.ogg" => vec![
            "-f",
            "lavfi",
            "-i",
            "aevalsrc=0.5*sin(2*PI*440*t)+0.2*sin(2*PI*1234*t)*sin(2*PI*3*t):s=44100",
            "-t",
            "2",
            "-ac",
            "2",
            "-c:a",
            "vorbis",
            "-strict",
            "-2",
        ],
        other => panic!("unknown fixture {other}"),
    }
}

const ALL: &[&str] = &["ogg_opus_music.opus", "ogg_opus_trim.ogg", "ogg_opus_speech.opus", "ogg_vorbis.ogg"];

fn make(ff: &Path, name: &str) -> PathBuf {
    fixture(ff, name, &spec(name)).unwrap_or_else(|| panic!("could not generate {name}"))
}

fn snr_db(reference: &[f32], ours: &[f32]) -> f64 {
    let (mut s, mut n) = (0f64, 0f64);
    for (r, o) in reference.iter().zip(ours) {
        s += (*r as f64).powi(2);
        n += (*r as f64 - *o as f64).powi(2);
    }
    if n == 0.0 { f64::INFINITY } else { 10.0 * (s / n).log10() }
}

/// (our continuous decode interleaved, ffmpeg's decode interleaved, channels, rate)
fn decode_both(ff: &Path, file: &Path, decoder: &[&str]) -> (OggSource, Vec<f32>, Vec<f32>, usize, u32) {
    let src = OggSource::open(file.file_name().unwrap().to_str().unwrap(), bytes(file)).unwrap();
    let a = src.info().audio().cloned().unwrap();
    let ch = a.channels as usize;
    let mut args: Vec<&str> = decoder.to_vec();
    args.extend_from_slice(&["-i", file.to_str().unwrap(), "-map", "0:a:0", "-f", "f32le", "-"]);
    let want: Vec<f32> = ffmpeg_out(ff, &args).as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
    let n = want.len() / ch;
    let frames = src.info().duration.to_units_floor(a.sample_rate as i64) as usize;
    assert_eq!(frames, n, "{}: our length = ffmpeg's ({} samples)", file.display(), n);
    let buf = src.audio(0, n, a.sample_rate).unwrap();
    let mut ours = Vec::with_capacity(n * ch);
    for i in 0..n {
        for c in 0..ch {
            ours.push(buf.channels[c][i]);
        }
    }
    (src, ours, want, ch, a.sample_rate)
}

/// Random windows on a fresh source equal the continuous decode.
fn check_seeks(file: &Path, ours: &[f32], ch: usize, rate: u32, tol: f32) {
    let src = OggSource::open("seek", bytes(file)).unwrap();
    let n = ours.len() / ch;
    let mut rng = Rng(0xA5A5_1234);
    let mut worst = 0f32;
    for _ in 0..25 {
        let s = rng.below(n as u64 - 10) as usize;
        let len = 1 + rng.below(4000) as usize;
        let got = src.audio(s as i64, len, rate).unwrap();
        for k in 0..len.min(n - s) {
            for c in 0..ch {
                worst = worst.max((got.channels[c][k] - ours[(s + k) * ch + c]).abs());
            }
        }
    }
    assert!(worst <= tol, "{}: seek differs from continuous decode by {worst}", file.display());
    eprintln!("{}: seeks within {worst:e} of the continuous decode", file.display());
}

#[test]
fn opus_celt_music_matches_libopus() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let f = make(&ff, "ogg_opus_music.opus");
    let (src, ours, want, ch, rate) = decode_both(&ff, &f, &["-c:a", "libopus"]);
    assert_eq!(src.info().container, "Ogg");
    assert_eq!(src.info().kind, MediaKind::AudioOnly);
    assert_eq!((rate, ch), (48_000, 2));
    assert_eq!(src.info().audio().unwrap().codec, "Opus");
    let snr = snr_db(&want, &ours);
    eprintln!("{}: {} samples, SNR {snr:.1} dB vs libopus", f.display(), want.len() / ch);
    assert!(snr >= 50.0, "SNR {snr:.1} dB");
    check_seeks(&f, &ours, ch, rate, 2e-3);
}

#[test]
fn opus_end_trimming_and_10ms_frames() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let f = make(&ff, "ogg_opus_trim.ogg");
    let (src, ours, want, ch, rate) = decode_both(&ff, &f, &["-c:a", "libopus"]);
    // 1.2345 s is not a whole number of 10 ms packets: the last page's granule trims the end
    let t = filmcraft_ogg::OpusTiming::of(&src.file().streams[0], filmcraft_opus::OpusHead::parse(&src.file().streams[0].headers[0]).unwrap().pre_skip as u32);
    let decoded: i64 = t.durations.iter().map(|&d| d as i64).sum::<i64>() - t.pre_skip as i64;
    assert!(t.total < decoded, "end trimmed: {} < {}", t.total, decoded);
    assert_eq!(t.total, 59_256, "1.2345 s at 48 kHz");
    let snr = snr_db(&want, &ours);
    assert!(snr >= 50.0, "SNR {snr:.1} dB");
    check_seeks(&f, &ours, ch, rate, 2e-3);
}

#[test]
fn opus_silk_speech_matches_libopus() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let f = make(&ff, "ogg_opus_speech.opus");
    let (_src, ours, want, ch, rate) = decode_both(&ff, &f, &["-c:a", "libopus"]);
    assert_eq!(ch, 1);
    let snr = snr_db(&want, &ours);
    eprintln!("{}: SNR {snr:.1} dB vs libopus", f.display());
    assert!(snr >= 10.0, "SNR {snr:.1} dB");
    check_seeks(&f, &ours, ch, rate, 2e-2);
}

#[test]
fn vorbis_matches_ffmpeg() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let f = make(&ff, "ogg_vorbis.ogg");
    let (src, ours, want, ch, rate) = decode_both(&ff, &f, &[]);
    assert_eq!(src.info().audio().unwrap().codec, "Vorbis");
    assert_eq!((rate, ch), (44_100, 2));
    let snr = snr_db(&want, &ours);
    eprintln!("{}: SNR {snr:.1} dB vs ffmpeg's Vorbis decoder", f.display());
    assert!(snr >= 60.0, "SNR {snr:.1} dB");
    check_seeks(&f, &ours, ch, rate, 0.0);
}

#[test]
fn opened_through_open_bytes_and_robust_to_damage() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let f = make(&ff, "ogg_opus_music.opus");
    let s = filmcraft_codecs::open_bytes("x.opus", bytes(&f)).unwrap();
    assert_eq!(s.info().container, "Ogg");
    let full = std::fs::read(&f).unwrap();
    let mut rng = Rng(42);
    for k in 1..12 {
        let cut = full.len() * k / 12;
        if let Ok(s) = OggSource::open("cut", full[..cut].to_vec().into()) {
            let n = s.info().duration.to_units_floor(48_000);
            let _ = s.audio(rng.below(n.max(1) as u64) as i64, 5000, 48_000).unwrap();
        }
    }
    for _ in 0..40 {
        let mut g = full.clone();
        for _ in 0..6 {
            let at = rng.below(g.len() as u64) as usize;
            g[at] = rng.below(256) as u8;
        }
        if let Ok(s) = OggSource::open("bad", g.into()) {
            let _ = s.audio(0, 48_000, 48_000);
            let _ = s.audio(30_000, 4_000, 44_100);
        }
    }
}

/// `cargo xtask fixtures codecs`: build every Ogg fixture.
#[test]
#[ignore]
fn generate_fixtures() {
    let Some(ff) = filmcraft_testkit::ffmpeg() else {
        for n in ALL {
            filmcraft_testkit::fixtures::report(n, filmcraft_testkit::fixtures::Status::Skipped);
        }
        return;
    };
    for n in ALL {
        let out = dir().join(n);
        filmcraft_testkit::fixtures::generate_and_report(n, std::slice::from_ref(&out), || fixture(&ff, n, &spec(n)));
    }
}
