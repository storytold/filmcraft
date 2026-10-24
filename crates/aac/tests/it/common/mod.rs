#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use filmcraft_aac::{Decoder, Encoder, EncoderConfig, EncoderStats, split_adts};

/// ffmpeg (see `filmcraft_testkit::oracle`); `None` after printing `SKIPPED`.
pub fn ffmpeg() -> Option<PathBuf> {
    filmcraft_testkit::ffmpeg_or_skip("aac oracle")
}

pub fn fixtures() -> PathBuf {
    filmcraft_testkit::fixtures_dir("aac")
}

pub struct Rng(pub u64);
impl Rng {
    pub fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
    }
}

use std::f32::consts::PI;

/// Logarithmic sine sweep 40 Hz → 16 kHz.
pub fn sweep(rate: u32, n: usize, amp: f32) -> Vec<f32> {
    let (f0, f1) = (40f64, 16000f64.min(rate as f64 * 0.45));
    let t_total = n as f64 / rate as f64;
    let k = (f1 / f0).ln();
    (0..n)
        .map(|i| {
            let t = i as f64 / rate as f64;
            let phase = 2.0 * std::f64::consts::PI * f0 * t_total / k * ((t / t_total * k).exp() - 1.0);
            (phase.sin() as f32) * amp
        })
        .collect()
}

pub fn noise(n: usize, amp: f32, seed: u64) -> Vec<f32> {
    let mut r = Rng(seed);
    (0..n).map(|_| r.next() * amp).collect()
}

/// Speech-like: glottal pulse train (gliding pitch) through three resonant formant filters with a
/// syllabic envelope and pauses, plus a little fricative noise.
pub fn speech(rate: u32, n: usize, seed: u64) -> Vec<f32> {
    let mut r = Rng(seed);
    let fs = rate as f32;
    let formants = [(700.0f32, 130.0f32), (1220.0, 70.0), (2600.0, 160.0)];
    let mut states = [[0f32; 2]; 3];
    let mut phase = 0f32;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f32 / fs;
        let f0 = 120.0 + 30.0 * (2.0 * PI * 0.7 * t).sin();
        phase += f0 / fs;
        let pulse = if phase >= 1.0 {
            phase -= 1.0;
            1.0
        } else {
            0.0
        };
        let syll = (2.0 * PI * 3.0 * t).sin().max(0.0);
        let voiced = if (t * 1.3).fract() < 0.8 { syll } else { 0.0 };
        let fric = if (t * 1.3).fract() > 0.85 { 0.05 * r.next() } else { 0.0 };
        let mut s = 0f32;
        for (j, &(f, bw)) in formants.iter().enumerate() {
            let rr = (-PI * bw / fs).exp();
            let a1 = 2.0 * rr * (2.0 * PI * f / fs).cos();
            let a2 = -rr * rr;
            let y = pulse * voiced + a1 * states[j][0] + a2 * states[j][1];
            states[j][1] = states[j][0];
            states[j][0] = y;
            s += y * [1.0, 0.6, 0.3][j];
        }
        out.push(s * 0.08 + fric);
    }
    let peak = out.iter().fold(0f32, |m, v| m.max(v.abs()));
    out.iter().map(|v| v / peak * 0.7).collect()
}

/// Music-like: a sequence of decaying harmonic chords.
pub fn chords(rate: u32, n: usize, detune: f32) -> Vec<f32> {
    let fs = rate as f32;
    let roots = [220.0f32, 261.63, 196.0, 246.94];
    let chord_len = (fs * 0.5) as usize;
    (0..n)
        .map(|i| {
            let c = (i / chord_len) % roots.len();
            let t = (i % chord_len) as f32 / fs;
            let env = (-t * 3.0).exp() * (1.0 - (-t * 200.0).exp());
            let mut s = 0f32;
            for (m, ratio) in [1.0f32, 1.25, 1.5, 2.0].iter().enumerate() {
                let f = roots[c] * ratio * (1.0 + detune);
                for h in 1..8 {
                    s += (2.0 * PI * f * h as f32 * t + m as f32).sin() / (h * h) as f32;
                }
            }
            s * env * 0.18
        })
        .collect()
}

/// Clicks: sharp impulses every 250 ms over a quiet low tone (transient test).
pub fn clicks(rate: u32, n: usize) -> Vec<f32> {
    let fs = rate as f32;
    let period = (fs * 0.25) as usize;
    (0..n)
        .map(|i| {
            let t = i as f32 / fs;
            let k = i % period;
            let click = if k < 256 { 0.8 * (-(k as f32) / 24.0).exp() * (2.0 * PI * 1500.0 * k as f32 / fs).cos() } else { 0.0 };
            0.02 * (2.0 * PI * 330.0 * t).sin() + click
        })
        .collect()
}

pub struct Encoded {
    pub adts: Vec<u8>,
    pub raw_bytes: usize,
    pub frames: usize,
    pub stats: EncoderStats,
    pub asc: Vec<u8>,
    pub bandwidth: u32,
}

pub fn encode(cfg: EncoderConfig, planar: &[Vec<f32>]) -> Encoded {
    let mut enc = Encoder::new(EncoderConfig { adts: true, ..cfg }).unwrap();
    let mut adts = Vec::new();
    // feed in uneven chunks
    let n = planar[0].len();
    let mut pos = 0;
    let mut step = 700;
    while pos < n {
        let end = (pos + step).min(n);
        let chunk: Vec<&[f32]> = planar.iter().map(|c| &c[pos..end]).collect();
        for au in enc.encode(&chunk) {
            adts.extend(au);
        }
        pos = end;
        step = step * 7 % 3001 + 100;
    }
    for au in enc.flush() {
        adts.extend(au);
    }
    let stats = enc.stats();
    Encoded { raw_bytes: stats.bytes as usize, frames: stats.frames as usize, stats, asc: enc.audio_specific_config(), bandwidth: enc.bandwidth(), adts }
}

/// Decode with ffmpeg → planar f32 (ffmpeg's own channel order), plus stderr.
pub fn ffmpeg_decode(ff: &Path, file: &Path, channels: usize) -> (Vec<Vec<f32>>, String) {
    let out = Command::new(ff).args(["-v", "error", "-i"]).arg(file).args(["-f", "f32le", "-"]).output().unwrap();
    let data = out.stdout;
    let n = data.len() / 4 / channels;
    let mut planar = vec![Vec::with_capacity(n); channels];
    for i in 0..n {
        for c in 0..channels {
            let o = (i * channels + c) * 4;
            planar[c].push(f32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]]));
        }
    }
    (planar, String::from_utf8_lossy(&out.stderr).into_owned())
}

/// Encode with ffmpeg's AAC encoder to ADTS.
pub fn ffmpeg_encode(ff: &Path, planar: &[Vec<f32>], rate: u32, args: &[&str], out: &Path) {
    let raw = out.with_extension("f32");
    let ch = planar.len();
    let mut bytes = Vec::with_capacity(planar[0].len() * ch * 4);
    for i in 0..planar[0].len() {
        for c in planar {
            bytes.extend_from_slice(&c[i].to_le_bytes());
        }
    }
    std::fs::write(&raw, bytes).unwrap();
    let layout = match ch {
        1 => "mono",
        2 => "stereo",
        3 => "3.0",
        4 => "4.0",
        5 => "5.0",
        6 => "5.1",
        _ => "7.1(wide)",
    };
    let st = Command::new(ff)
        .args(["-v", "error", "-y", "-f", "f32le", "-ar", &rate.to_string(), "-ch_layout", layout, "-i"])
        .arg(&raw)
        .args(["-c:a", "aac"])
        .args(args)
        .args(["-f", "adts"])
        .arg(out)
        .status()
        .unwrap();
    assert!(st.success());
}

/// Decode an ADTS file with our decoder.
pub fn our_decode(data: &[u8]) -> Vec<Vec<f32>> {
    let frames = split_adts(data).unwrap();
    let mut dec = Decoder::from_adts(&frames[0].0).unwrap();
    let mut out: Vec<Vec<f32>> = Vec::new();
    for (_, au) in frames {
        let pcm = dec.decode(au).unwrap();
        out.resize(pcm.len(), Vec::new());
        for (o, p) in out.iter_mut().zip(pcm) {
            o.extend(p);
        }
    }
    out
}

/// SNR in dB of `dec` against `refr`.
pub fn snr(refr: &[f32], dec: &[f32]) -> f64 {
    let n = refr.len().min(dec.len());
    let (mut s, mut e) = (0f64, 0f64);
    for i in 0..n {
        s += (refr[i] as f64).powi(2);
        e += (refr[i] as f64 - dec[i] as f64).powi(2);
    }
    10.0 * (s / e.max(1e-20)).log10()
}

/// Segmental SNR (1024-sample segments above -50 dBFS, each clamped to [-10, 60] dB).
pub fn seg_snr(refr: &[f32], dec: &[f32]) -> f64 {
    let n = refr.len().min(dec.len());
    let mut acc = 0f64;
    let mut cnt = 0;
    for s in (0..n.saturating_sub(1024)).step_by(1024) {
        let (mut p, mut e) = (0f64, 0f64);
        for i in s..s + 1024 {
            p += (refr[i] as f64).powi(2);
            e += (refr[i] as f64 - dec[i] as f64).powi(2);
        }
        if p / 1024.0 < 1e-5 {
            continue;
        }
        acc += (10.0 * (p / e.max(1e-20)).log10()).clamp(-10.0, 60.0);
        cnt += 1;
    }
    if cnt == 0 { 0.0 } else { acc / cnt as f64 }
}

pub fn max_abs_diff(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).fold(0f32, |m, (x, y)| m.max((x - y).abs()))
}

/// Windowed-sinc (Blackman, 401 taps) low-pass at `fc` Hz, zero-phase (delay-compensated).
pub fn lowpass(x: &[f32], fc: f32, fs: f32) -> Vec<f32> {
    let taps = 401usize;
    let m = (taps - 1) as f32;
    let wc = fc / fs;
    let h: Vec<f32> = (0..taps)
        .map(|i| {
            let n = i as f32 - m / 2.0;
            let sinc = if n == 0.0 { 2.0 * wc } else { (2.0 * PI * wc * n).sin() / (PI * n) };
            let w = 0.42 - 0.5 * (2.0 * PI * i as f32 / m).cos() + 0.08 * (4.0 * PI * i as f32 / m).cos();
            sinc * w
        })
        .collect();
    let half = taps / 2;
    (0..x.len())
        .map(|i| {
            let mut acc = 0f32;
            for (k, hk) in h.iter().enumerate() {
                let j = i as isize + k as isize - half as isize;
                if j >= 0 && (j as usize) < x.len() {
                    acc += hk * x[j as usize];
                }
            }
            acc
        })
        .collect()
}
