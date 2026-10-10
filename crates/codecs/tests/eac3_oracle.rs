//! E-AC-3 (Dolby Digital Plus) audio in MP4, Matroska and MPEG-TS against ffmpeg: ffmpeg encodes
//! and muxes the files and decodes them as the oracle. Zero-bit mantissas carry decoder-specific
//! dither (A/52 §7.3.4), so the criterion is SNR against ffmpeg's decode, sample-aligned.

mod common;

use common::*;

const STEREO: &str = "aevalsrc=exprs=0.3*sin(2*PI*441*t)+0.1*sin(2*PI*3100*t)|0.25*sin(2*PI*660*t)+0.1*sin(2*PI*(500+4000*t)*t):s=48000";
const SURROUND: &str =
    "aevalsrc=exprs=0.3*sin(2*PI*300*t)|0.3*sin(2*PI*400*t)|0.3*sin(2*PI*500*t)|0.3*sin(2*PI*60*t)|0.2*sin(2*PI*700*t)|0.2*sin(2*PI*800*t):s=48000:c=5.1";

/// Worst channel SNR (dB) of `ours` (planar) against ffmpeg's interleaved `want`, with ours
/// shifted by `lag` samples, over the samples both cover past the first 1000.
fn snr(ours: &[Vec<f32>], want: &[f32], ch: usize, lag: i64) -> f64 {
    let n = want.len() / ch;
    (0..ch)
        .map(|c| {
            let (mut sig, mut err) = (0f64, 0f64);
            for i in 1000..n - 1000 {
                let Some(&a) = ours[c].get((i as i64 + lag) as usize) else { continue };
                let (a, b) = (a as f64, want[i * ch + c] as f64);
                sig += b * b;
                err += (a - b) * (a - b);
            }
            10.0 * (sig / err.max(1e-30)).log10()
        })
        .fold(f64::INFINITY, f64::min)
}

#[test]
fn eac3_in_mp4_matroska_and_transport_streams() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let cases: [(&str, &str, u32, &[&str]); 4] = [
        ("eac3_51.mp4", SURROUND, 6, &["-b:a", "384k"]),
        ("eac3_51.mkv", SURROUND, 6, &["-b:a", "384k"]),
        ("eac3_stereo.mov", STEREO, 2, &["-b:a", "192k"]),
        ("eac3_stereo.ts", STEREO, 2, &["-b:a", "192k"]),
    ];
    for (name, input, channels, enc) in cases {
        let args: Vec<&str> = ["-f", "lavfi", "-i", input, "-t", "2", "-c:a", "eac3"].into_iter().chain(enc.iter().copied()).collect();
        let f = fixture(&ff, name, &args).unwrap_or_else(|| panic!("could not write {name}"));
        let src = filmcraft_codecs::open_bytes(name, bytes(&f)).unwrap();
        let a = src.info().audio().cloned().unwrap();
        assert_eq!((a.sample_rate, a.channels), (48_000, channels), "{name}: {}", a.codec);
        let want = ffmpeg_audio_f32(&ff, &f, &[]);
        let ch = channels as usize;
        let n = want.len() / ch;
        // MPEG-TS audio starts at its first PTS relative to the presentation start
        let start = match name.ends_with(".ts") {
            true => filmcraft_codecs::MpegSource::open(name, bytes(&f)).unwrap().audio_extent().map_or(0, |e| e.0),
            false => 0,
        };
        let ours = src.audio(start, n, 48_000).unwrap().channels;
        assert_eq!(ours.len(), ch, "{name}");
        // Matroska stores CodecDelay (the encoder's 256-sample priming) in ns and timestamps in
        // ms, and we place packets at the rounded value (240 samples): a shared Matroska timing
        // limit (AAC and AC-3 alike), not a decoding one. Elsewhere the samples line up exactly.
        let (worst, lag) = (-32..=32).map(|lag| (snr(&ours, &want, ch, lag), lag)).fold((f64::MIN, 0), |a, b| if b.0 > a.0 { b } else { a });
        let max_lag = if name.ends_with(".mkv") { 16 } else { 0 };
        assert!(lag.abs() <= max_lag, "{name}: decode is {lag} samples off ffmpeg's");
        // a window read on a fresh source (random access) gives the same samples
        let fresh = filmcraft_codecs::open_bytes(name, bytes(&f)).unwrap();
        let at = n / 3;
        let win = fresh.audio(start + at as i64, 4800, 48_000).unwrap().channels;
        let mut seek_err = 0f32;
        for c in 0..ch {
            for (k, v) in win[c].iter().enumerate() {
                seek_err = seek_err.max((v - ours[c][at + k]).abs());
            }
        }
        println!(
            "{name}: {} ({} Hz, {ch} ch), worst channel SNR vs ffmpeg {worst:.1} dB (lag {lag}), random access max diff {seek_err:.1e}",
            a.codec, a.sample_rate
        );
        assert!(worst >= 50.0, "{name}: SNR {worst:.1} dB");
        assert!(seek_err <= 1e-6, "{name}: random-access audio differs by {seek_err}");
    }
}
