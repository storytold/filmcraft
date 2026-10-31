//! Matroska audio made by ffmpeg, against ffmpeg's decode (#236). ffmpeg stamps blocks to the
//! millisecond, so at 44.1 kHz AAC (1024-sample), MP3 (1152) and AC-3 (1536) packets are each
//! stamped up to half a millisecond off their exact start. Read back block by block, as playback
//! reads it, a tone must stay as smooth as ffmpeg's decode of it: no holes or overlaps between
//! packets.

use crate::common;

use common::*;

/// Largest |second difference| of `x` from `from` on: a click shows up as a spike.
fn roughness(x: &[f32], from: usize) -> f32 {
    x.windows(3).skip(from).map(|w| (w[0] - 2.0 * w[1] + w[2]).abs()).fold(0.0, f32::max)
}

#[test]
fn matroska_audio_plays_without_holes() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let cases: [(&str, u32, &[&str]); 5] = [
        ("tone_aac_48k.mkv", 48_000, &["-c:a", "aac", "-b:a", "160k"]),
        ("tone_aac_44k.mkv", 44_100, &["-c:a", "aac", "-b:a", "160k"]),
        ("tone_mp3_44k.mkv", 44_100, &["-c:a", "libmp3lame", "-b:a", "192k"]),
        ("tone_mp2_44k.mkv", 44_100, &["-c:a", "mp2", "-b:a", "192k"]),
        ("tone_ac3_44k.mkv", 44_100, &["-c:a", "ac3", "-b:a", "192k"]),
    ];
    for (name, rate, codec) in cases {
        let input = format!("sine=frequency=440:sample_rate={rate}:duration=3");
        let args: Vec<&str> = ["-f", "lavfi", "-i", input.as_str(), "-af", "volume=4"].iter().copied().chain(codec.iter().copied()).collect();
        let Some(path) = fixture(&ff, name, &args) else {
            eprintln!("SKIPPED ({name}): this ffmpeg can't encode it");
            continue;
        };
        let src = filmcraft_codecs::open_bytes(name, bytes(&path)).unwrap();
        assert_eq!(src.info().audio().map(|a| a.sample_rate), Some(rate), "{name}");
        let frames = rate as usize * 29 / 10;
        let mut ours = Vec::with_capacity(frames);
        while ours.len() < frames {
            let n = 512.min(frames - ours.len());
            ours.extend_from_slice(&src.audio(ours.len() as i64, n, rate).unwrap().channels[0]);
        }
        let theirs = ffmpeg_audio_f32(&ff, &path, &[]);
        // past the codecs' start-up (priming, MP3's first frame)
        let skip = rate as usize / 10;
        let (r_ours, r_theirs) = (roughness(&ours, skip), roughness(&theirs[..theirs.len().min(frames)], skip));
        let peak = ours.iter().skip(skip).fold(0f32, |m, x| m.max(x.abs()));
        println!("{name}: peak {peak:.3}, max |second difference| ours {r_ours:.5}, ffmpeg {r_theirs:.5}");
        assert!(peak > 0.4, "{name}: the tone is missing (peak {peak})");
        assert!(r_ours < 2.0 * r_theirs + 0.005, "{name}: clicks between packets (|Δ²| {r_ours} vs ffmpeg's {r_theirs})");
    }
}

/// Mean |difference| between `ours[i]` and `theirs[i + lag]` over `from..from + len`.
fn mean_err(ours: &[f32], theirs: &[f32], from: usize, len: usize, lag: i64) -> f64 {
    (from..from + len).map(|i| (ours[i] as f64 - theirs[(i as i64 + lag) as usize] as f64).abs()).sum::<f64>() / len as f64
}

/// The lag in `lags` at which `ours` best matches `theirs` (least [`mean_err`]), and that error.
fn best_lag(ours: &[f32], theirs: &[f32], from: usize, len: usize, lags: std::ops::RangeInclusive<i64>) -> (i64, f64) {
    lags.map(|lag| (lag, mean_err(ours, theirs, from, len, lag))).min_by(|a, b| a.1.total_cmp(&b.1)).unwrap()
}

/// A reference signal we control: low-passed white noise from a fixed xorshift seed. Its
/// autocorrelation dies out within a few samples, so even a one-sample misalignment shows.
fn noise(n: usize) -> Vec<f32> {
    let (mut state, mut y) = (0x9E37_79B9_7F4A_7C15u64, 0f32);
    (0..n)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let white = (state >> 40) as f32 / (1u64 << 24) as f32 - 0.5;
            y = 0.5 * y + 0.4 * white;
            y
        })
        .collect()
}

/// #790: ffmpeg stores the encoder priming as `CodecDelay` (1024 samples = 21.333 ms for AAC at
/// 48 kHz, 256 = 5.805 ms for AC-3 at 44.1 kHz, 1105 = 25.057 ms for MP3), and block timestamps to
/// the millisecond. Taking the rounded delay as the start of the first packet played the whole
/// track up to 16 samples late.
///
/// The strict check is version-independent: ffmpeg encodes a signal we generate, and our decode
/// must line up with that signal to the sample (the stream's `CodecDelay` trimmed exactly). ffmpeg's
/// own decode is compared only after a bounded best-lag search: how its CLI handles Matroska
/// priming depends on the version (9.x trims `CodecDelay`, 2023-24 builds keep the priming
/// samples), so its lag is reported, not asserted.
#[test]
fn matroska_audio_is_sample_aligned_with_ffmpeg() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let cases: [(&str, u32, &[&str]); 6] = [
        ("ref_aac_48k.mkv", 48_000, &["-c:a", "aac", "-b:a", "256k"]),
        ("ref_aac_44k.mkv", 44_100, &["-c:a", "aac", "-b:a", "256k"]),
        ("ref_ac3_44k.mkv", 44_100, &["-c:a", "ac3", "-b:a", "448k"]),
        ("ref_ac3_48k.mkv", 48_000, &["-c:a", "ac3", "-b:a", "448k"]),
        ("ref_eac3_48k.mkv", 48_000, &["-c:a", "eac3", "-b:a", "384k"]),
        ("ref_mp3_44k.mkv", 44_100, &["-c:a", "libmp3lame", "-b:a", "320k"]),
    ];
    for (name, rate, codec) in cases {
        let reference = noise(rate as usize * 2);
        let raw = format!("ref_noise_{rate}.f32");
        let raw_path = filmcraft_testkit::fixtures::generate(&dir().join(&raw), |tmp| {
            std::fs::write(tmp, reference.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>()).is_ok()
        })
        .unwrap();
        let (raw_arg, rate_arg) = (raw_path.to_str().unwrap().to_string(), rate.to_string());
        let args: Vec<&str> = ["-f", "f32le", "-ar", &rate_arg, "-ac", "1", "-i", &raw_arg].iter().copied().chain(codec.iter().copied()).collect();
        let Some(path) = fixture(&ff, name, &args) else {
            eprintln!("SKIPPED ({name}): this ffmpeg can't encode it");
            continue;
        };
        let file = bytes(&path);
        let mkv = filmcraft_matroska::open(file.clone()).unwrap();
        let delay_ns = mkv.tracks.first().map_or(0, |t| t.codec_delay_ns) as i64;
        let delay = (delay_ns * rate as i64 + 500_000_000) / 1_000_000_000;
        assert!(delay > 0, "{name}: no CodecDelay");
        let src = filmcraft_codecs::open_bytes(name, file).unwrap();
        let frames = rate as usize * 3 / 2;
        let mut ours = Vec::with_capacity(frames);
        while ours.len() < frames {
            let n = 1000.min(frames - ours.len());
            ours.extend_from_slice(&src.audio(ours.len() as i64, n, rate).unwrap().channels[0]);
        }
        let (from, len, bound) = (rate as usize / 4, rate as usize / 4, delay + 32);
        // strict: the encoder's input, to the sample
        let (lag, err) = best_lag(&ours, &reference, from, len, -bound..=bound);
        let level = reference[from..from + len].iter().map(|v| v.abs() as f64).sum::<f64>() / len as f64;
        assert_eq!(lag, 0, "{name}: our decode is {lag} samples off the encoded signal (CodecDelay {delay} samples)");
        assert!(err < 0.25 * level, "{name}: decode error {err:.2e} against a signal level of {level:.2e}");
        // ffmpeg's decode, wherever its version puts the priming
        let theirs = ffmpeg_audio_f32(&ff, &path, &[]);
        assert!(theirs.len() >= frames, "{name}: ffmpeg decoded {} samples", theirs.len());
        let (ff_lag, ff_err) = best_lag(&ours, &theirs, from, len, -bound..=bound);
        let priming = match ff_lag {
            0 => "trims CodecDelay".to_string(),
            l if l == delay => "keeps the priming samples".to_string(),
            l => format!("is {l} samples off"),
        };
        println!("{name}: CodecDelay {delay} samples; vs the encoded signal lag 0, error {err:.2e} (level {level:.2e}); ffmpeg {priming}, error {ff_err:.2e}");
        assert!(ff_err < 1e-3, "{name}: differs from ffmpeg's decode by {ff_err:.2e} at its best lag {ff_lag}");
        // read from the middle, as a seek does
        let at = rate as i64 / 2;
        let mid = src.audio(at, 2000, rate).unwrap().channels[0].clone();
        let diff = mid.iter().zip(&ours[at as usize..]).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
        assert!(diff < 1e-3, "{name}: a read from {at} differs from continuous decoding by {diff}");
    }
}
