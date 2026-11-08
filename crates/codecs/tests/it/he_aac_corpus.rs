//! HE-AAC files against ffmpeg's decode (#4507). HE-AAC can't be made with the ffmpeg that
//! generates the other fixtures (its encoders are AAC-LC only), and media is not committed, so
//! these tests read a local corpus: every `.m4a`, `.mp4`, `.mov`, `.mkv` and `.aac` file in the
//! directory `FILMCRAFT_AAC_CORPUS` names. Without it they print a note and pass.
//!
//! Suitable files: the `soundcraft/he_aac_*.m4a` fixtures of KNUFFIXX/artcraft-fixtures
//! (MIT OR Apache-2.0; sine tones, so they test length, rate and channels, not the high band) and
//! any broadband HE-AAC v1 / v2 recording. ffmpeg is only run, as the oracle.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::common::{bytes, ffmpeg_audio_f32};

const CORPUS_ENV: &str = "FILMCRAFT_AAC_CORPUS";

/// The corpus files, or `None` after a note when `FILMCRAFT_AAC_CORPUS` is not set.
fn corpus(test: &str) -> Option<Vec<PathBuf>> {
    let Some(dir) = std::env::var_os(CORPUS_ENV) else {
        eprintln!("{test}: {CORPUS_ENV} is not set; no HE-AAC corpus to test");
        return None;
    };
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{CORPUS_ENV}={}: {e}", Path::new(&dir).display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().and_then(|e| e.to_str()).is_some_and(|e| ["m4a", "mp4", "mov", "mkv", "aac"].contains(&e.to_ascii_lowercase().as_str())))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "{CORPUS_ENV}={}: no .m4a, .mp4, .mov, .mkv or .aac files", Path::new(&dir).display());
    Some(files)
}

/// The first audio stream's sample rate and channel count as ffprobe reports them.
fn probe(ffprobe: &Path, file: &Path) -> (u32, usize) {
    let out = Command::new(ffprobe)
        .args(["-v", "error", "-select_streams", "a:0", "-show_entries", "stream=sample_rate,channels", "-of", "csv=p=0"])
        .arg(file)
        .stdin(Stdio::null())
        .output()
        .expect("run ffprobe");
    let text = String::from_utf8_lossy(&out.stdout);
    let mut it = text.trim().split(',').map(|v| v.trim().parse::<u64>().unwrap());
    let (rate, channels) = (it.next().unwrap(), it.next().unwrap());
    (rate as u32, channels as usize)
}

/// One file decoded by FilmCraft and by ffmpeg, at the rate and channel count each reports.
struct Decoded {
    name: String,
    rate: u32,
    ours: Vec<Vec<f32>>,
    theirs: Vec<Vec<f32>>,
    /// Whether the file has a video stream (its duration is then not the audio's).
    video: bool,
    /// The length FilmCraft reports, in samples at `rate`.
    length: i64,
}

fn decode(ff: &Path, ffprobe: &Path, file: &Path) -> Decoded {
    let name = file.file_name().unwrap().to_string_lossy().into_owned();
    let src = filmcraft_codecs::open_bytes(&name, bytes(file)).unwrap_or_else(|e| panic!("{name}: {e}"));
    let info = src.info();
    let audio = info.audio().unwrap_or_else(|| panic!("{name}: no audio stream"));
    let (rate, channels) = (audio.sample_rate, audio.channels as usize);
    let (their_rate, their_channels) = probe(ffprobe, file);
    assert_eq!((rate, channels), (their_rate, their_channels), "{name}: rate and channels (FilmCraft, ffprobe)");
    let all = ffmpeg_audio_f32(ff, file, &[]);
    let theirs: Vec<Vec<f32>> = (0..channels).map(|c| all.iter().skip(c).step_by(channels).copied().collect()).collect();
    let length = info.duration.to_units_floor(i64::from(rate));
    let frames = theirs[0].len().max(usize::try_from(length).unwrap_or(0));
    let ours = src.audio(0, frames, rate).unwrap_or_else(|e| panic!("{name}: {e}")).channels;
    Decoded { name, rate, ours, theirs, video: info.video.is_some(), length }
}

fn mono(channels: &[Vec<f32>]) -> Vec<f32> {
    let n = channels.iter().map(Vec::len).min().unwrap_or(0);
    (0..n).map(|i| channels.iter().map(|c| c[i]).sum::<f32>() / channels.len() as f32).collect()
}

/// The lag of `b` behind `a` (in samples, within ±`max`) with the highest normalised
/// cross-correlation over `a[from..from + len]`, and that correlation.
fn best_lag(a: &[f32], b: &[f32], from: usize, len: usize, max: usize) -> (i64, f64) {
    let mut best = (0i64, f64::MIN);
    for lag in -(max as i64)..=max as i64 {
        let (mut ab, mut aa, mut bb) = (0f64, 0f64, 0f64);
        for i in from..from + len {
            let j = i as i64 + lag;
            let (Some(&x), Some(&y)) = (a.get(i), usize::try_from(j).ok().and_then(|j| b.get(j))) else { continue };
            ab += f64::from(x) * f64::from(y);
            aa += f64::from(x) * f64::from(x);
            bb += f64::from(y) * f64::from(y);
        }
        let r = if aa > 0.0 && bb > 0.0 { ab / (aa * bb).sqrt() } else { 0.0 };
        if r > best.1 {
            best = (lag, r);
        }
    }
    best
}

/// In-place radix-2 complex FFT (`re.len()` a power of two).
fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -std::f64::consts::TAU / len as f64;
        for start in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (s, c) = (ang * k as f64).sin_cos();
                let (a, b) = (start + k, start + k + len / 2);
                let (tr, ti) = (re[b] * c - im[b] * s, re[b] * s + im[b] * c);
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
            }
        }
        len <<= 1;
    }
}

const FFT: usize = 4096;
/// Width of the bands the spectra are compared in.
const BAND_HZ: f64 = 1500.0;

/// Mean power per `BAND_HZ` band of `x` (Hann-windowed 4096-point frames, half overlap), in dB
/// relative to a full-scale sine.
fn band_levels(x: &[f32], rate: u32) -> Vec<f64> {
    let window: Vec<f64> = (0..FFT).map(|i| 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / FFT as f64).cos()).collect();
    let bin_hz = f64::from(rate) / FFT as f64;
    let bands = (f64::from(rate) / 2.0 / BAND_HZ) as usize;
    let mut power = vec![0f64; bands];
    let mut frames = 0;
    for start in (0..x.len().saturating_sub(FFT)).step_by(FFT / 2) {
        let mut re: Vec<f64> = x[start..start + FFT].iter().zip(&window).map(|(&v, w)| f64::from(v) * w).collect();
        let mut im = vec![0f64; FFT];
        fft(&mut re, &mut im);
        for k in 1..FFT / 2 {
            let band = (k as f64 * bin_hz / BAND_HZ) as usize;
            if let Some(p) = power.get_mut(band) {
                *p += re[k] * re[k] + im[k] * im[k];
            }
        }
        frames += 1;
    }
    // a full-scale sine puts (N/4)² of Hann-windowed power into its bins
    let full_scale = (FFT as f64 / 4.0).powi(2) * 1.5;
    power.iter().map(|&p| 10.0 * (p / frames.max(1) as f64 / full_scale).max(1e-20).log10()).collect()
}

/// FilmCraft reports each HE-AAC file at ffmpeg's rate, channel count and length, and its
/// decode lines up with ffmpeg's to the sample: storytold/soundcraft#712 (half length, double
/// speed) and #740 (v2 as mono) in FilmCraft's import.
#[test]
fn he_aac_corpus_rate_channels_length_and_sync() {
    let Some(files) = corpus("he_aac_corpus_rate_channels_length_and_sync") else { return };
    let ff = filmcraft_testkit::require_ffmpeg!();
    let ffprobe = filmcraft_testkit::require_ffprobe!();
    for file in files {
        let d = decode(&ff, &ffprobe, &file);
        let theirs_len = d.theirs[0].len() as i64;
        if !d.video {
            assert_eq!(d.length, theirs_len, "{}: length in samples (FilmCraft, ffmpeg)", d.name);
        }
        // 1 s from 0.5 s in, against ffmpeg's decode a quarter of a second either way
        let (a, b) = (mono(&d.theirs), mono(&d.ours));
        let from = d.rate as usize / 2;
        let (lag, r) = best_lag(&a, &b, from, (d.rate as usize).min(a.len().saturating_sub(from)), d.rate as usize / 4);
        eprintln!("{}: {} Hz, {} channels, {theirs_len} samples, lag {lag}, correlation {r:.4}", d.name, d.rate, d.ours.len());
        assert!(r > 0.9, "{}: FilmCraft's decode doesn't match ffmpeg's (correlation {r:.3} at lag {lag})", d.name);
        assert!(lag.abs() <= 1, "{}: FilmCraft's decode is {lag} samples behind ffmpeg's", d.name);
    }
}

/// Every band ffmpeg's decode has content in (above -70 dB), FilmCraft's has within 3 dB:
/// the high band SBR rebuilds is there (storytold/effectcraft#2893: muffled).
#[test]
#[ignore = "SBR is not decoded yet (#4507): only the AAC-LC core's band is there"]
fn he_aac_corpus_full_bandwidth() {
    let Some(files) = corpus("he_aac_corpus_full_bandwidth") else { return };
    let ff = filmcraft_testkit::require_ffmpeg!();
    let ffprobe = filmcraft_testkit::require_ffprobe!();
    let mut failures = Vec::new();
    for file in files {
        let d = decode(&ff, &ffprobe, &file);
        for (c, (ours, theirs)) in d.ours.iter().zip(&d.theirs).enumerate() {
            let n = ours.len().min(theirs.len());
            let (ours, theirs) = (band_levels(&ours[..n], d.rate), band_levels(&theirs[..n], d.rate));
            for (band, (o, t)) in ours.iter().zip(&theirs).enumerate() {
                let (lo, hi) = (band as f64 * BAND_HZ, (band + 1) as f64 * BAND_HZ);
                if *t > -70.0 && (o - t).abs() > 3.0 {
                    failures.push(format!("{} channel {c}, {lo:.0}–{hi:.0} Hz: {o:.1} dB, ffmpeg {t:.1} dB", d.name));
                }
            }
        }
    }
    assert!(failures.is_empty(), "band levels differ from ffmpeg's by more than 3 dB:\n{}", failures.join("\n"));
}
