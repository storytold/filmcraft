//! Encoder quality/bitrate tests using ffmpeg as an external decoding oracle (skipped if absent).

use crate::common;

use common::*;
use filmcraft_aac::{BitrateMode, EncoderConfig, WindowShape};

struct Case {
    name: &'static str,
    rate: u32,
    signal: fn(u32, usize, usize) -> Vec<Vec<f32>>,
    channels: usize,
}

fn mono(v: Vec<f32>, ch: usize) -> Vec<Vec<f32>> {
    (0..ch).map(|c| if c == 0 { v.clone() } else { v.iter().map(|x| x * (0.8 - 0.1 * c as f32)).collect() }).collect()
}

fn sig_sweep(r: u32, n: usize, ch: usize) -> Vec<Vec<f32>> {
    mono(sweep(r, n, 0.5), ch)
}
fn sig_noise(_r: u32, n: usize, ch: usize) -> Vec<Vec<f32>> {
    (0..ch).map(|c| noise(n, 0.3, 7 + c as u64)).collect()
}
fn sig_speech(r: u32, n: usize, ch: usize) -> Vec<Vec<f32>> {
    mono(speech(r, n, 3), ch)
}
fn sig_chords(r: u32, n: usize, ch: usize) -> Vec<Vec<f32>> {
    (0..ch).map(|c| chords(r, n, 0.002 * c as f32)).collect()
}
fn sig_clicks(r: u32, n: usize, ch: usize) -> Vec<Vec<f32>> {
    mono(clicks(r, n), ch)
}

struct Row {
    label: String,
    target: u32,
    actual: f64,
    snr: f64,
    bw_snr: f64,
    bw: u32,
    seg: f64,
    peak: f32,
    shorts: u64,
    ours_vs_ff: f32,
}

fn run(case: &Case, bitrate: u32, cfg_mod: impl Fn(&mut EncoderConfig)) -> Option<Row> {
    let ff = ffmpeg()?;
    let secs = 2.0;
    let n = (case.rate as f32 * secs) as usize + 333;
    let input = (case.signal)(case.rate, n, case.channels);
    let mut cfg = EncoderConfig::cbr(case.rate, case.channels, bitrate);
    cfg_mod(&mut cfg);
    let enc = encode(cfg.clone(), &input);
    let tag = match cfg.mode {
        BitrateMode::Cbr(b) => format!("{b}"),
        BitrateMode::Vbr(q) => format!("q{q}"),
    };
    let path = fixtures().join(format!("enc_{}_{}ch_{}_{}.aac", case.name, case.channels, case.rate, tag));
    std::fs::write(&path, &enc.adts).unwrap();
    let (dec, err) = ffmpeg_decode(&ff, &path, case.channels);
    assert!(err.trim().is_empty(), "ffmpeg reported errors for {}: {err}", path.display());
    assert_eq!(dec[0].len(), enc.frames * 1024);
    assert!(dec[0].len() >= n + 1024, "decoded stream too short");
    // ffmpeg reorders channels to its native layouts: map each of our (AAC element order) decoded
    // channels to the matching ffmpeg channel.
    let ours = our_decode(&enc.adts);
    let mut taken = vec![false; case.channels];
    let mut snr_sum = 0.0;
    let mut seg_sum = 0.0;
    let mut bw_sum = 0.0;
    let mut peak = 0f32;
    let mut ours_vs_ff = 0f32;
    let mut counted = 0;
    for c in 0..case.channels {
        let (j, err) = (0..case.channels).filter(|&j| !taken[j]).map(|j| (j, max_abs_diff(&ours[c], &dec[j]))).min_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
        taken[j] = true;
        ours_vs_ff = ours_vs_ff.max(err);
        let d = &dec[j][1024..1024 + n];
        peak = peak.max(d.iter().fold(0f32, |m, v| m.max(v.abs())));
        if case.channels >= 6 && c == case.channels - 1 {
            continue; // LFE is band-limited to 240 Hz by design
        }
        snr_sum += snr(&input[c], d);
        let fc = enc.bandwidth as f32 * 0.97;
        bw_sum += snr(&lowpass(&input[c], fc, case.rate as f32), &lowpass(d, fc, case.rate as f32));
        seg_sum += seg_snr(&input[c], d);
        counted += 1;
    }
    let actual = enc.stats.bitrate(case.rate);
    Some(Row {
        label: format!("{:<7} {}ch {:>5}", case.name, case.channels, case.rate),
        target: if matches!(cfg.mode, BitrateMode::Vbr(_)) { 0 } else { bitrate },
        actual,
        snr: snr_sum / counted as f64,
        bw_snr: bw_sum / counted as f64,
        bw: enc.bandwidth,
        seg: seg_sum / counted as f64,
        peak,
        shorts: enc.stats.short_blocks,
        ours_vs_ff,
    })
}

fn print(rows: &[Row]) {
    println!(
        "{:<22} {:>8} {:>8} {:>7} {:>6} {:>7} {:>7} {:>7} {:>6} {:>6} {:>9}",
        "signal", "target", "actual", "dev%", "bw", "SNR", "bwSNR", "segSNR", "peak", "short", "us-vs-ff"
    );
    for r in rows {
        let (target, dev) = if r.target == 0 {
            ("vbr".to_string(), "-".to_string())
        } else {
            (r.target.to_string(), format!("{:+.2}", (r.actual / r.target as f64 - 1.0) * 100.0))
        };
        println!(
            "{:<22} {:>8} {:>8.0} {:>7} {:>6} {:>7.2} {:>7.2} {:>7.2} {:>6.3} {:>6} {:>9.2e}",
            r.label, target, r.actual, dev, r.bw, r.snr, r.bw_snr, r.seg, r.peak, r.shorts, r.ours_vs_ff
        );
    }
}

fn check(r: &Row, min_seg: f64) {
    let dev = (r.actual / r.target as f64 - 1.0).abs();
    assert!(dev < 0.05, "{}: bitrate {} vs target {}", r.label, r.actual, r.target);
    assert!(r.peak < 1.0, "{}: clipping (peak {})", r.label, r.peak);
    assert!(r.seg > min_seg, "{}: segmental SNR {} too low", r.label, r.seg);
    assert!(r.ours_vs_ff < 1e-3, "{}: our decoder differs from ffmpeg by {}", r.label, r.ours_vs_ff);
}

#[test]
fn stereo_matrix_44k_48k() {
    if ffmpeg().is_none() {
        eprintln!("ffmpeg not found; skipping");
        return;
    }
    let cases = [
        Case { name: "sweep", rate: 44100, signal: sig_sweep, channels: 2 },
        Case { name: "noise", rate: 44100, signal: sig_noise, channels: 2 },
        Case { name: "speech", rate: 48000, signal: sig_speech, channels: 2 },
        Case { name: "chords", rate: 44100, signal: sig_chords, channels: 2 },
        Case { name: "clicks", rate: 48000, signal: sig_clicks, channels: 2 },
    ];
    let mut rows = Vec::new();
    for c in &cases {
        for br in [64_000, 128_000, 192_000, 320_000] {
            let r = run(c, br, |_| {}).unwrap();
            rows.push(r);
        }
    }
    print(&rows);
    for r in &rows {
        check(
            r,
            if r.label.starts_with("noise") {
                0.5
            } else if r.target >= 128_000 {
                8.0
            } else {
                3.0
            },
        );
    }
}

#[test]
fn mono_and_surround() {
    if ffmpeg().is_none() {
        eprintln!("ffmpeg not found; skipping");
        return;
    }
    let mut rows = Vec::new();
    for (c, br) in [
        (Case { name: "speech", rate: 44100, signal: sig_speech, channels: 1 }, 64_000),
        (Case { name: "chords", rate: 48000, signal: sig_chords, channels: 1 }, 96_000),
        (Case { name: "clicks", rate: 44100, signal: sig_clicks, channels: 1 }, 128_000),
        (Case { name: "chords", rate: 48000, signal: sig_chords, channels: 6 }, 320_000),
        (Case { name: "noise", rate: 48000, signal: sig_noise, channels: 6 }, 384_000),
    ] {
        rows.push(run(&c, br, |_| {}).unwrap());
    }
    print(&rows);
    for r in &rows {
        check(r, 3.0);
    }
}

#[test]
fn kbd_windows_no_tns_no_ms() {
    if ffmpeg().is_none() {
        return;
    }
    let c = Case { name: "kbd", rate: 44100, signal: sig_clicks, channels: 2 };
    let r = run(&c, 128_000, |cfg| {
        cfg.window_shape = WindowShape::Kbd;
        cfg.tns = false;
        cfg.ms_stereo = false;
    })
    .unwrap();
    print(std::slice::from_ref(&r));
    check(&r, 5.0);
}

#[test]
fn sample_rates_8k_to_96k() {
    if ffmpeg().is_none() {
        return;
    }
    let mut rows = Vec::new();
    for (rate, br) in [
        (8000u32, 24_000u32),
        (11025, 32_000),
        (12000, 32_000),
        (16000, 48_000),
        (22050, 64_000),
        (24000, 64_000),
        (32000, 96_000),
        (64000, 192_000),
        (88200, 256_000),
        (96000, 256_000),
    ] {
        let c = Case { name: "chords", rate, signal: sig_chords, channels: 2 };
        rows.push(run(&c, br, |_| {}).unwrap());
    }
    print(&rows);
    for r in &rows {
        check(r, 5.0);
    }
}

#[test]
fn channel_layouts_3_to_8() {
    if ffmpeg().is_none() {
        return;
    }
    let mut rows = Vec::new();
    for (ch, br) in [(3usize, 192_000u32), (4, 256_000), (5, 320_000), (7, 384_000), (8, 448_000)] {
        let c = Case { name: "chords", rate: 48000, signal: sig_chords, channels: ch };
        rows.push(run(&c, br, |_| {}).unwrap());
    }
    print(&rows);
    for r in &rows {
        check(r, 5.0);
    }
}

#[test]
fn vbr_quality_scale() {
    if ffmpeg().is_none() {
        return;
    }
    let mut rows = Vec::new();
    for sig in [sig_chords as fn(u32, usize, usize) -> Vec<Vec<f32>>, sig_speech] {
        for q in [1.0f32, 2.0, 3.0, 4.0, 5.0] {
            let c = Case { name: "vbr", rate: 44100, signal: sig, channels: 2 };
            let mut r = run(&c, 1, |cfg| cfg.mode = BitrateMode::Vbr(q)).unwrap();
            r.label = format!("vbr q{q} 2ch 44100");
            rows.push(r);
        }
    }
    print(&rows);
    for pair in rows.chunks(5) {
        for w in pair.windows(2) {
            assert!(w[1].actual > w[0].actual, "VBR bitrate must grow with quality");
            assert!(w[1].seg > w[0].seg - 0.5, "VBR quality must grow with quality");
        }
        for r in pair {
            assert!(r.peak < 1.0 && r.ours_vs_ff < 1e-3);
        }
    }
}
