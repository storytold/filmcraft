//! Oracle test: BS.1770-4 / EBU R128 loudness against ffmpeg's `ebur128` filter.
//!
//! Each signal is generated here, written as a 32-bit float WAV and measured twice: by
//! `LoudnessMeter` and by `ffmpeg -af ebur128=peak=true:metadata=1,ametadata=print` (three decimals
//! per 100 ms frame). ffmpeg is an external oracle only (never linked).
//!
//! Tolerances (EBU Tech 3341 allows ±0.1 LU for M/S/I and +0.2/−0.4 dB for true peak; Tech 3342
//! allows ±1 LU for LRA):
//! - momentary (every 100 ms once 400 ms have been seen) and short-term (once 3 s have been seen):
//!   ±0.1 LU wherever ffmpeg reads above −70 LUFS;
//! - integrated: ±0.1 LU;
//! - loudness range: ±0.5 LU;
//! - true peak: ±0.2 dB (both are 4× oversampling estimates with different interpolators).

use filmcraft_audio_dsp::loudness::LoudnessMeter;
use std::f64::consts::PI;
use std::path::Path;
use std::process::Command;

const TOL_MS: f64 = 0.1;
const TOL_I: f64 = 0.1;
const TOL_LRA: f64 = 0.5;
const TOL_TP_DB: f64 = 0.2;

/// ffmpeg's per-100 ms frame values.
#[derive(Clone, Copy, Debug, Default)]
struct Frame {
    m: f64,
    s: f64,
    i: f64,
    lra: f64,
    true_peak: f64,
}

fn ffmpeg_ebur128(ff: &Path, wav: &Path) -> Vec<Frame> {
    let out = Command::new(ff)
        .args(["-hide_banner", "-nostats", "-v", "error", "-i"])
        .arg(wav)
        .args(["-af", "ebur128=peak=true:metadata=1,ametadata=mode=print:file=-", "-f", "null", "-"])
        .output()
        .expect("run ffmpeg");
    assert!(out.status.success(), "ffmpeg ebur128 failed: {}", String::from_utf8_lossy(&out.stderr));
    let mut frames = Vec::new();
    let mut cur: Option<Frame> = None;
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if line.starts_with("frame:") {
            frames.extend(cur.take());
            cur = Some(Frame::default());
            continue;
        }
        let (Some(f), Some((k, v))) = (cur.as_mut(), line.strip_prefix("lavfi.r128.").and_then(|l| l.split_once('='))) else { continue };
        let v: f64 = v.trim().parse().unwrap_or(f64::NAN);
        match k {
            "M" => f.m = v,
            "S" => f.s = v,
            "I" => f.i = v,
            "LRA" => f.lra = v,
            "true_peak" => f.true_peak = v,
            _ => {}
        }
    }
    frames.extend(cur);
    frames
}

/// Our measurements after each 100 ms block: (M, S) per block, plus the final meter.
fn ours(chans: &[Vec<f32>], sr: u32) -> (Vec<(f64, f64)>, LoudnessMeter) {
    let mut m = LoudnessMeter::new(sr as f64, chans.len());
    let block = sr as usize / 10;
    let n = chans[0].len();
    let mut per = Vec::new();
    let mut pos = 0;
    while pos < n {
        let end = (pos + block).min(n);
        let slices: Vec<&[f32]> = chans.iter().map(|c| &c[pos..end]).collect();
        m.process(&slices);
        per.push((m.momentary(), m.short_term()));
        pos = end;
    }
    (per, m)
}

struct Report {
    max_dm: f64,
    max_ds: f64,
    i: (f64, f64),
    lra: (f64, f64),
    tp: (f64, f64),
    /// True-peak reference used (analytic value when known, else ffmpeg).
    tp_ref: f64,
}

/// Measure `chans` with both meters and check the tolerances. `tp_truth`: the analytic true peak
/// (dBTP) when known; it then replaces ffmpeg as the true-peak reference.
fn compare(ff: &Path, name: &'static str, chans: &[Vec<f32>], sr: u32, tp_truth: Option<f64>) -> Report {
    let dir = filmcraft_testkit::fixtures_dir("audio-dsp/loudness");
    let wav = dir.join(format!("{name}_{sr}.wav"));
    let tmp = filmcraft_testkit::temp_path(&wav);
    let refs: Vec<&[f32]> = chans.iter().map(|c| c.as_slice()).collect();
    filmcraft_testkit::wav::write_f32(&tmp, &refs, sr).unwrap();
    let frames = ffmpeg_ebur128(ff, &tmp);
    let _ = std::fs::remove_file(&tmp);
    let (per, meter) = ours(chans, sr);
    assert!(frames.len() + 1 >= per.len() && !frames.is_empty(), "{name}: ffmpeg gave {} frames, we have {} blocks", frames.len(), per.len());
    let (mut max_dm, mut max_ds, mut n_m, mut n_s) = (0f64, 0f64, 0, 0);
    for (k, (f, (m, s))) in frames.iter().zip(&per).enumerate() {
        let full_blocks = k + 1;
        if full_blocks >= 4 && f.m > -70.0 {
            let d = (f.m - m).abs();
            assert!(d <= TOL_MS, "{name}: momentary at {:.1} s: ffmpeg {:.3}, ours {m:.3}", full_blocks as f64 / 10.0, f.m);
            max_dm = max_dm.max(d);
            n_m += 1;
        }
        if full_blocks >= 30 && f.s > -70.0 {
            let d = (f.s - s).abs();
            assert!(d <= TOL_MS, "{name}: short-term at {:.1} s: ffmpeg {:.3}, ours {s:.3}", full_blocks as f64 / 10.0, f.s);
            max_ds = max_ds.max(d);
            n_s += 1;
        }
    }
    let last = frames.last().unwrap();
    let ff_tp_db = 20.0 * last.true_peak.log10();
    let r = Report {
        tp_ref: tp_truth.unwrap_or(ff_tp_db),
        max_dm,
        max_ds,
        i: (meter.integrated(), last.i),
        lra: (meter.loudness_range(), last.lra),
        tp: (meter.true_peak_dbtp(), ff_tp_db),
    };
    eprintln!(
        "{name} @ {sr} Hz: I ours {:.3} / ffmpeg {:.3} LUFS; LRA {:.2} / {:.2} LU; TP {:.3} / {:.3} dBTP; max |ΔM| {:.4} over {n_m}, max |ΔS| {:.4} over {n_s}",
        r.i.0, r.i.1, r.lra.0, r.lra.1, r.tp.0, r.tp.1, r.max_dm, r.max_ds
    );
    assert!(n_m > 0, "{name}: no momentary values compared");
    assert!((r.i.0 - r.i.1).abs() <= TOL_I, "{name}: integrated ours {:.3} vs ffmpeg {:.3}", r.i.0, r.i.1);
    assert!((r.lra.0 - r.lra.1).abs() <= TOL_LRA, "{name}: LRA ours {:.3} vs ffmpeg {:.3}", r.lra.0, r.lra.1);
    match tp_truth {
        Some(t) => {
            eprintln!("{name}: analytic true peak {t:.3} dBTP: ours off by {:+.3} dB, ffmpeg off by {:+.3} dB", r.tp.0 - t, r.tp.1 - t);
            assert!((r.tp.0 - t).abs() <= TOL_TP_DB, "{name}: true peak ours {:.3} vs analytic {t:.3} dBTP", r.tp.0);
        }
        None => assert!((r.tp.0 - r.tp.1).abs() <= TOL_TP_DB, "{name}: true peak ours {:.3} vs ffmpeg {:.3} dBTP", r.tp.0, r.tp.1),
    }
    r
}

// ---- signals ----

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    }
}

fn db(x: f64) -> f64 {
    10f64.powf(x / 20.0)
}

fn sine(freq: f64, amp: f64, sr: u32, secs: f64, phase: f64) -> Vec<f32> {
    (0..(secs * sr as f64) as usize).map(|i| (amp * (2.0 * PI * freq * i as f64 / sr as f64 + phase).sin()) as f32).collect()
}

/// Pink noise (Paul Kellet's refined filter on white noise), scaled to `amp` RMS-ish.
fn pink(sr: u32, secs: f64, amp: f64, seed: u64) -> Vec<f32> {
    let mut r = Rng(seed);
    let (mut b0, mut b1, mut b2, mut b3, mut b4, mut b5, mut b6) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    (0..(secs * sr as f64) as usize)
        .map(|_| {
            let w = r.next();
            b0 = 0.99886 * b0 + w * 0.0555179;
            b1 = 0.99332 * b1 + w * 0.0750759;
            b2 = 0.96900 * b2 + w * 0.1538520;
            b3 = 0.86650 * b3 + w * 0.3104856;
            b4 = 0.55000 * b4 + w * 0.5329522;
            b5 = -0.7616 * b5 - w * 0.0168980;
            let y = b0 + b1 + b2 + b3 + b4 + b5 + b6 + w * 0.5362;
            b6 = w * 0.115926;
            (y * 0.11 * amp) as f32
        })
        .collect()
}

/// Speech-like: syllable bursts (80–300 ms) of a pitched, formant-ish signal with pauses
/// (50–600 ms) and phrase-level level changes, so gating and the M/S windows are exercised.
fn speech(sr: u32, secs: f64, seed: u64) -> Vec<f32> {
    let mut r = Rng(seed);
    let n = (secs * sr as f64) as usize;
    let mut out = vec![0f32; n];
    let mut pos = 0usize;
    let mut phrase_gain = 1.0;
    while pos < n {
        if r.next() > 0.7 {
            phrase_gain = db(-12.0 * (r.next() + 1.0) / 2.0);
        }
        let syl = ((0.08 + 0.22 * (r.next() + 1.0) / 2.0) * sr as f64) as usize;
        let f0 = 110.0 + 60.0 * (r.next() + 1.0) / 2.0;
        let (f1, f2) = (500.0 + 400.0 * r.next(), 1500.0 + 700.0 * r.next());
        for i in 0..syl.min(n - pos) {
            let t = i as f64 / sr as f64;
            let env = (PI * i as f64 / syl as f64).sin().powi(2);
            let mut v = 0.0;
            for h in 1..=20 {
                let f = f0 * h as f64;
                if f > sr as f64 / 2.2 {
                    break;
                }
                let formant = (-((f - f1) / 200.0).powi(2)).exp() + 0.5 * (-((f - f2) / 300.0).powi(2)).exp() + 0.05;
                v += formant * (2.0 * PI * f * t).sin() / h as f64;
            }
            out[pos + i] = (0.3 * phrase_gain * env * v + 0.003 * r.next()) as f32;
        }
        pos += syl;
        let gap = ((0.05 + 0.55 * (r.next() + 1.0) / 2.0) * sr as f64) as usize;
        pos += gap;
    }
    out
}

fn concat(parts: &[Vec<f32>]) -> Vec<f32> {
    parts.concat()
}

#[test]
fn loudness_matches_ffmpeg_ebur128() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let sr = 48_000;
    let mut reports = Vec::new();

    // 1. Steady 997 Hz sine, −20 dBFS, both channels (dual mono adds +3 dB).
    let s = sine(997.0, db(-20.0), sr, 10.0, 0.0);
    reports.push(compare(&ff, "sine_997", &[s.clone(), s], sr, Some(-20.0)));

    // 2. Independent pink noise per channel.
    reports.push(compare(&ff, "pink_noise", &[pink(sr, 15.0, 0.5, 1), pink(sr, 15.0, 0.5, 2)], sr, None));

    // 3. Speech-like bursts and pauses (exercises the relative gate and LRA).
    let sp = speech(sr, 25.0, 7);
    let sp2: Vec<f32> = sp.iter().map(|v| v * 0.7).collect();
    reports.push(compare(&ff, "speech_like", &[sp, sp2], sr, None));

    // 4. Stereo with silence and quiet passages: loud tone, digital silence, a −30 dBFS tone on L
    //    only, a −50 dBFS passage (below the relative gate) and a −80 dBFS passage (below the
    //    absolute gate).
    let l = concat(&[
        sine(440.0, db(-18.0), sr, 5.0, 0.0),
        vec![0.0; 3 * sr as usize],
        sine(1000.0, db(-30.0), sr, 5.0, 0.0),
        pink(sr, 4.0, db(-50.0), 3),
        pink(sr, 4.0, db(-80.0), 4),
    ]);
    let r = concat(&[
        sine(440.0, db(-18.0), sr, 5.0, 0.3),
        vec![0.0; 3 * sr as usize],
        vec![0.0; 5 * sr as usize],
        pink(sr, 4.0, db(-50.0), 5),
        pink(sr, 4.0, db(-80.0), 6),
    ]);
    reports.push(compare(&ff, "gated_stereo", &[l, r], sr, None));

    // 5. Inter-sample peaks: fs/4 at 45° phase, 0 dBFS true peak but −3 dBFS sample peak.
    let x = sine(sr as f64 / 4.0, 0.9, sr, 5.0, PI / 4.0);
    reports.push(compare(&ff, "intersample_peak", &[x.clone(), x], sr, Some(20.0 * 0.9f64.log10())));

    // 6. Mono speech-like at 44.1 kHz (K-weighting derived for another rate; true peak at 4×).
    let sr2 = 44_100;
    reports.push(compare(&ff, "speech_mono_44k", &[speech(sr2, 20.0, 11)], sr2, None));

    // 7. Pink noise at 96 kHz (2× true-peak oversampling).
    reports.push(compare(&ff, "pink_noise_96k", &[pink(96_000, 8.0, 0.4, 21), pink(96_000, 8.0, 0.4, 22)], 96_000, None));

    let worst = |f: fn(&Report) -> f64| reports.iter().map(f).fold(0f64, f64::max);
    eprintln!(
        "worst: |ΔM| {:.3}, |ΔS| {:.3}, |ΔI| {:.3}, |ΔLRA| {:.3}, |ΔTP| {:.3} dB",
        worst(|r| r.max_dm),
        worst(|r| r.max_ds),
        worst(|r| (r.i.0 - r.i.1).abs()),
        worst(|r| (r.lra.0 - r.lra.1).abs()),
        worst(|r| (r.tp.0 - r.tp_ref).abs()),
    );
}
