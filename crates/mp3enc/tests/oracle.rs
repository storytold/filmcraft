//! The encoder against ffmpeg as an external decoder (skipped without ffmpeg): ffprobe must see
//! an MP3 stream with our rate, channels and duration, ffmpeg must decode every frame without a
//! complaint, and its decode, aligned by [`DELAY`], must match the input within a signal-to-noise
//! ratio per bit rate. White noise loses what lies above the bit rate's low-pass, so its bound is
//! low; the bounds sit about 6 dB under what the encoder reaches today.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use filmcraft_mp3enc::{Config, DELAY, Encoder, StereoMode};

fn fixtures() -> PathBuf {
    filmcraft_testkit::fixtures_dir("mp3enc")
}

fn chords(rate: u32, n: usize, detune: f32) -> Vec<f32> {
    let f = [220.0, 277.18, 329.63, 440.0, 1318.5, 3520.0];
    let a = [0.2, 0.15, 0.12, 0.1, 0.05, 0.02];
    (0..n)
        .map(|i| {
            let t = i as f32 / rate as f32;
            f.iter().zip(a).map(|(f, a)| a * (2.0 * std::f32::consts::PI * f * (1.0 + detune) * t).sin()).sum::<f32>()
        })
        .collect()
}

fn noise(n: usize, amp: f32, seed: u64) -> Vec<f32> {
    let mut s = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            ((s >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0) * amp
        })
        .collect()
}

fn sweep(rate: u32, n: usize) -> Vec<f32> {
    let secs = n as f32 / rate as f32;
    let (f0, f1) = (40.0f32, rate as f32 * 0.4);
    let k = (f1 / f0).ln() / secs;
    (0..n).map(|i| 0.4 * (2.0 * std::f32::consts::PI * f0 * (((i as f32 / rate as f32) * k).exp() - 1.0) / k).sin()).collect()
}

fn encode(cfg: Config, input: &[Vec<f32>]) -> Vec<u8> {
    let mut enc = Encoder::new(cfg).unwrap();
    let mut out = Vec::new();
    let n = input[0].len();
    let mut pos = 0;
    while pos < n {
        let end = (pos + 1000).min(n);
        let planes: Vec<&[f32]> = input.iter().map(|c| &c[pos..end]).collect();
        out.extend(enc.encode(&planes));
        pos = end;
    }
    out.extend(enc.flush());
    out
}

fn ffmpeg_decode(ff: &Path, path: &Path, channels: usize) -> (Vec<Vec<f32>>, String) {
    let out = Command::new(ff).args(["-v", "error", "-i"]).arg(path).args(["-f", "f32le", "-acodec", "pcm_f32le", "-"]).stdin(Stdio::null()).output().unwrap();
    let samples: Vec<f32> = out.stdout.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
    let planar = (0..channels).map(|c| samples.iter().skip(c).step_by(channels).copied().collect()).collect();
    (planar, String::from_utf8_lossy(&out.stderr).into_owned())
}

/// SNR in dB of `dec` (aligned) against `input`.
fn snr(input: &[f32], dec: &[f32]) -> f64 {
    let (mut s, mut n) = (0f64, 0f64);
    for (a, b) in input.iter().zip(dec) {
        s += f64::from(*a) * f64::from(*a);
        n += f64::from(a - b) * f64::from(a - b);
    }
    10.0 * (s / n.max(1e-30)).log10()
}

struct Case {
    name: &'static str,
    rate: u32,
    channels: usize,
    kbps: u32,
    stereo: StereoMode,
    signal: fn(u32, usize, usize) -> Vec<f32>,
    min_snr: f64,
}

fn sig_chords(rate: u32, n: usize, c: usize) -> Vec<f32> {
    chords(rate, n, 0.003 * c as f32)
}
fn sig_noise(_rate: u32, n: usize, c: usize) -> Vec<f32> {
    noise(n, 0.25, 11 + c as u64)
}
fn sig_sweep(rate: u32, n: usize, c: usize) -> Vec<f32> {
    sweep(rate, n).iter().map(|v| v * (1.0 - 0.2 * c as f32)).collect()
}

fn run(ff: &Path, c: &Case) -> f64 {
    let n = c.rate as usize * 2 + 777;
    let input: Vec<Vec<f32>> = (0..c.channels).map(|ch| (c.signal)(c.rate, n, ch)).collect();
    let cfg = Config { sample_rate: c.rate, channels: c.channels, bitrate_kbps: c.kbps, stereo: c.stereo };
    let bytes = encode(cfg, &input);
    let path = fixtures().join(format!("{}_{}_{}ch_{}k.mp3", c.name, c.rate, c.channels, c.kbps));
    std::fs::write(&path, &bytes).unwrap();
    let (dec, err) = ffmpeg_decode(ff, &path, c.channels);
    assert!(err.trim().is_empty(), "{}: ffmpeg reported: {err}", path.display());
    let d = DELAY as usize;
    assert!(dec[0].len() >= n + d, "{}: decoded {} samples, need {}", c.name, dec[0].len(), n + d);
    // the bit rate is exact: every frame has the nominal size
    let frame_samples = if c.rate >= 32_000 { 1152 } else { 576 };
    let frames = dec[0].len() / frame_samples;
    let nominal = c.kbps as f64 * 1000.0 / 8.0 * (frames * frame_samples) as f64 / c.rate as f64;
    assert!((bytes.len() as f64 / nominal - 1.0).abs() < 0.01, "{}: {} bytes vs {nominal}", c.name, bytes.len());
    if let Some(probe) = filmcraft_testkit::ffprobe() {
        let out = Command::new(probe)
            .args(["-v", "error", "-show_entries", "stream=codec_name,sample_rate,channels:format=duration", "-of", "default=nw=1"])
            .arg(&path)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        let field = |k: &str| text.lines().find_map(|l| l.strip_prefix(&format!("{k}="))).unwrap_or("").to_string();
        assert_eq!(field("codec_name"), "mp3", "{text}");
        assert_eq!(field("sample_rate"), c.rate.to_string(), "{text}");
        assert_eq!(field("channels"), c.channels.to_string(), "{text}");
        let dur: f64 = field("duration").parse().unwrap();
        let expect = (frames * frame_samples) as f64 / c.rate as f64;
        assert!((dur - expect).abs() < 2.0 * frame_samples as f64 / c.rate as f64, "{}: duration {dur} vs {expect}", c.name);
    }
    let s = (0..c.channels).map(|ch| snr(&input[ch], &dec[ch][d..d + n])).sum::<f64>() / c.channels as f64;
    println!("{:<8} {:>5} Hz {}ch {:>3} kbps {:?}: SNR {s:.2} dB", c.name, c.rate, c.channels, c.kbps, c.stereo);
    s
}

#[test]
fn ffmpeg_decodes_our_streams_at_every_rate() {
    let Some(ff) = filmcraft_testkit::ffmpeg_or_skip("mp3enc oracle") else { return };
    use StereoMode::*;
    let cases = [
        Case { name: "chords", rate: 44_100, channels: 2, kbps: 128, stereo: JointStereo, signal: sig_chords, min_snr: 42.0 },
        Case { name: "chords", rate: 44_100, channels: 2, kbps: 320, stereo: JointStereo, signal: sig_chords, min_snr: 60.0 },
        Case { name: "sweep", rate: 48_000, channels: 2, kbps: 192, stereo: Stereo, signal: sig_sweep, min_snr: 22.0 },
        Case { name: "noise", rate: 48_000, channels: 2, kbps: 128, stereo: JointStereo, signal: sig_noise, min_snr: 2.5 },
        Case { name: "chords", rate: 32_000, channels: 1, kbps: 64, stereo: JointStereo, signal: sig_chords, min_snr: 40.0 },
        Case { name: "chords", rate: 22_050, channels: 2, kbps: 64, stereo: JointStereo, signal: sig_chords, min_snr: 33.0 },
        Case { name: "sweep", rate: 24_000, channels: 1, kbps: 48, stereo: JointStereo, signal: sig_sweep, min_snr: 24.0 },
        Case { name: "chords", rate: 16_000, channels: 1, kbps: 32, stereo: JointStereo, signal: sig_chords, min_snr: 30.0 },
    ];
    let mut failed = Vec::new();
    for c in &cases {
        let s = run(&ff, c);
        if s < c.min_snr {
            failed.push(format!("{} {} Hz {} kbps: {s:.2} < {}", c.name, c.rate, c.kbps, c.min_snr));
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}
