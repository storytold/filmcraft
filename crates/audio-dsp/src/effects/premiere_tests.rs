//! Behavioural tests of the Premiere audio-effect set: neutral settings are (delayed)
//! identity, output is deterministic, latency is constant and correct, and each effect does
//! what it says on generated signals (tones, noise, impulses), plus a realtime-factor check.

use super::*;
use crate::AudioEffect;
use crate::testutil::*;

const SR: f32 = 48000.0;

fn noise(seed: u64, n: usize, amp: f32) -> Vec<f32> {
    let mut r = Rng::new(seed);
    (0..n).map(|_| r.uniform() * amp).collect()
}

fn fx(id: &str, params: &[(&str, f32)]) -> Box<dyn AudioEffect> {
    let mut e = create_effect(id, SR, 2).unwrap_or_else(|| panic!("no effect {id}"));
    for (p, v) in params {
        assert!(e.set_param(p, *v), "{id}.{p}");
    }
    e.reset();
    e
}

fn run(e: &mut dyn AudioEffect, l: &[f32], r: &[f32]) -> (Vec<f32>, Vec<f32>) {
    let (mut a, mut b) = (l.to_vec(), r.to_vec());
    e.process(&mut [&mut a, &mut b]);
    (a, b)
}

/// Steady-state amplitude (relative, dB) of a tone through the effect (second half measured,
/// latency ignored because the tone is stationary).
fn tone_db(e: &mut dyn AudioEffect, freq: f64, amp: f32) -> f64 {
    e.reset();
    let n = SR as usize;
    let x = sine(freq, amp as f64, SR as f64, n, 0.0);
    let (y, _) = run(e, &x, &x);
    db(tone_amplitude(&y[n / 2..], freq, SR as f64) / amp as f64)
}

/// Settings under which each effect must pass audio through unchanged (apart from latency).
fn neutral(id: &str) -> Option<Vec<(&'static str, f32)>> {
    Some(match id {
        "graphic_eq_10" | "graphic_eq_20" | "graphic_eq_30" | "notch_filter" | "channel_mixer" | "stereo_expander" | "ambisonics_panner" | "loudness_meter"
        | "mute" | "mastering" | "fft_filter" => vec![],
        "parametric_eq_full" => vec![],
        "dynamics_rack" => vec![("comp_on", 0.0)],
        "multiband_compressor" => return None, // all-pass, not identity: see its own test
        "tube_compressor" => return None,      // saturation stage: near-identity at low level, tested separately
        "chorus_flanger" | "flanger" | "phaser" | "multitap_delay" | "distortion" | "guitar_suite" | "binauralizer" | "convolution_reverb" => {
            vec![("mix", 0.0)]
        }
        "analog_delay" => vec![("wet", 0.0), ("dry", 100.0)],
        "surround_reverb" => vec![("wet", 0.0), ("dry", 100.0)],
        "click_remover" => vec![("threshold", 100.0)],
        "scientific_filter" => return None,
        "vocal_enhancer" => return None,
        _ => return None,
    })
}

const NEW_IDS: &[&str] = &[
    "graphic_eq_10",
    "graphic_eq_20",
    "graphic_eq_30",
    "parametric_eq_full",
    "notch_filter",
    "scientific_filter",
    "fft_filter",
    "dynamics_rack",
    "multiband_compressor",
    "tube_compressor",
    "chorus_flanger",
    "flanger",
    "phaser",
    "analog_delay",
    "multitap_delay",
    "convolution_reverb",
    "surround_reverb",
    "click_remover",
    "channel_mixer",
    "distortion",
    "guitar_suite",
    "mastering",
    "vocal_enhancer",
    "stereo_expander",
    "binauralizer",
    "ambisonics_panner",
    "loudness_meter",
    "mute",
];

#[test]
fn neutral_settings_are_delayed_identity() {
    let n = 24000;
    let l = noise(1, n, 0.25);
    let r = noise(2, n, 0.25);
    let mut checked = 0;
    for id in NEW_IDS {
        let Some(p) = neutral(id) else { continue };
        let mut e = fx(id, &p);
        let lat = e.latency();
        let (a, b) = run(e.as_mut(), &l, &r);
        let mut worst = 0.0f32;
        for i in lat..n {
            worst = worst.max((a[i] - l[i - lat]).abs()).max((b[i] - r[i - lat]).abs());
        }
        assert!(worst < 2e-4, "{id}: neutral output differs by {worst}");
        checked += 1;
    }
    assert!(checked >= 20);
}

#[test]
fn every_effect_is_deterministic_and_latency_is_constant() {
    let n = 12000;
    let l = noise(3, n, 0.5);
    let r = noise(4, n, 0.5);
    for e in effects() {
        let mk = || {
            let mut f = (e.create)(SR, 2);
            for p in e.params {
                let v = match p.unit {
                    crate::Unit::Toggle => 1.0 - p.default,
                    crate::Unit::Choice => p.max,
                    _ => p.min + (p.max - p.min) * 0.61,
                };
                f.set_param(p.id, v);
            }
            f
        };
        let lat0 = (e.create)(SR, 2).latency();
        let mut a = mk();
        let mut b = mk();
        // The Hard Limiter's delay is its Look-Ahead Time, so that one parameter changes latency.
        if e.id != "limiter" {
            assert_eq!(a.latency(), lat0, "{}: latency depends on parameters", e.id);
        }
        assert_eq!(a.latency(), b.latency(), "{}: the same parameters disagree on latency", e.id);
        let ya = run(a.as_mut(), &l, &r);
        let yb = run(b.as_mut(), &l, &r);
        assert!(ya.0 == yb.0 && ya.1 == yb.1, "{}: not deterministic", e.id);
    }
}

#[test]
fn latency_is_where_the_impulse_lands() {
    // Effects with latency: an impulse comes out exactly `latency` samples later at neutral
    // settings (FFT filter / convolution / click remover / existing STFT effects).
    for (id, p) in [("fft_filter", vec![]), ("convolution_reverb", vec![("mix", 0.0)]), ("click_remover", vec![("threshold", 100.0)])] {
        let mut e = fx(id, &p);
        let lat = e.latency();
        assert!(lat > 0, "{id}");
        let n = lat + 4000;
        let mut l = vec![0.0f32; n];
        l[100] = 0.5;
        let (a, _) = run(e.as_mut(), &l, &l);
        let peak = a.iter().enumerate().max_by(|x, y| x.1.abs().total_cmp(&y.1.abs())).unwrap().0;
        assert_eq!(peak, 100 + lat, "{id}");
        assert!((a[peak] - 0.5).abs() < 1e-3, "{id}: {}", a[peak]);
    }
}

#[test]
fn graphic_eq_bands_hit_their_gain() {
    for (id, band, freq) in [("graphic_eq_10", "b6", 1000.0), ("graphic_eq_20", "b11", 1000.0), ("graphic_eq_30", "b17", 1000.0)] {
        let mut e = fx(id, &[(band, 12.0)]);
        let want = e.response_db(freq).unwrap();
        assert!((want - 12.0).abs() < 1.0, "{id}: analytic {want}");
        let got = tone_db(e.as_mut(), freq, 0.1);
        assert!((got - want).abs() < 0.1, "{id}: measured {got} vs {want}");
        // An octave+ away the boost has mostly gone.
        assert!(e.response_db(freq * 4.0).unwrap() < 1.0, "{id}");
        let mut flat = fx(id, &[("gain", -6.0)]);
        assert!((tone_db(flat.as_mut(), 300.0, 0.1) + 6.0).abs() < 0.05, "{id}: master gain");
    }
}

#[test]
fn parametric_eq_full_sections() {
    // 24 dB/oct Butterworth high-pass: −3 dB at the corner, ≈ −48 dB two octaves down.
    let hp = fx("parametric_eq_full", &[("hp_on", 1.0), ("hp_freq", 200.0), ("hp_slope", 1.0)]);
    assert!((hp.response_db(200.0).unwrap() + 3.01).abs() < 0.05, "corner {}", hp.response_db(200.0).unwrap());
    assert!(hp.response_db(50.0).unwrap() < -45.0);
    let mut e = fx("parametric_eq_full", &[("hp_on", 1.0), ("hp_freq", 200.0), ("hp_slope", 1.0), ("mid_gain", 9.0), ("master_gain", -3.0)]);
    assert!((e.response_db(1000.0).unwrap() - 6.0).abs() < 0.05);
    for f in [50.0, 200.0, 1000.0, 5000.0] {
        let want = e.response_db(f).unwrap();
        let got = tone_db(e.as_mut(), f, 0.1);
        assert!((got - want).abs() < 0.15, "{f} Hz: {got} vs {want}");
    }
}

#[test]
fn notch_filter_removes_tones() {
    let mut e = fx("notch_filter", &[("n1_on", 1.0), ("n1_freq", 1000.0), ("n1_gain", -60.0), ("width", 2.0)]);
    assert!(tone_db(e.as_mut(), 1000.0, 0.3) < -50.0);
    assert!(tone_db(e.as_mut(), 1200.0, 0.3).abs() < 0.5);
}

#[test]
fn scientific_filter_matches_its_design() {
    for (ty, mode) in [(3.0, 0.0), (1.0, 1.0), (2.0, 2.0), (0.0, 3.0)] {
        let mut e = fx("scientific_filter", &[("type", ty), ("mode", mode), ("order", 4.0), ("cutoff", 800.0), ("high_cutoff", 3000.0)]);
        for f in [200.0, 1000.0, 1500.0, 6000.0] {
            let want = e.response_db(f).unwrap();
            let got = tone_db(e.as_mut(), f, 0.1);
            if want > -60.0 {
                assert!((got - want).abs() < 0.2, "type {ty} mode {mode} {f} Hz: {got} vs {want}");
            } else {
                assert!(got < -55.0, "type {ty} mode {mode} {f} Hz: {got}");
            }
        }
    }
}

#[test]
fn fft_filter_applies_its_curve() {
    let mut e = fx("fft_filter", &[("p5_gain", -30.0), ("interp", 0.0)]);
    assert!((e.response_db(1500.0).unwrap() + 30.0).abs() < 1e-3);
    let got = tone_db(e.as_mut(), 1500.0, 0.3);
    assert!((got + 30.0).abs() < 1.5, "{got}");
    assert!(tone_db(e.as_mut(), 100.0, 0.3).abs() < 0.3);
}

#[test]
fn dynamics_rack_sections() {
    // Compressor: −6 dBFS sine (peak), −20 dB threshold, 4:1 → ≈ −16.5 dB peak.
    let mut e = fx("dynamics_rack", &[("comp_threshold", -20.0), ("comp_ratio", 4.0), ("comp_attack", 0.1), ("comp_release", 3000.0)]);
    let want = e.transfer_db(0, -6.0).unwrap();
    assert!((want + 16.5).abs() < 1e-4);
    let n = SR as usize;
    let x = sine(1000.0, 0.5, SR as f64, n, 0.0);
    let (y, _) = run(e.as_mut(), &x, &x);
    let peak = y[n / 2..].iter().fold(0.0f32, |a, v| a.max(v.abs()));
    assert!((gain_db(peak) - want).abs() < 0.3, "compressed peak {}", gain_db(peak));
    // Gate: a quiet tone under the gate threshold is closed by ≥ 60 dB.
    let mut g = fx("dynamics_rack", &[("comp_on", 0.0), ("gate_on", 1.0), ("gate_threshold", -30.0)]);
    let q = sine(1000.0, 0.01, SR as f64, n, 0.0);
    let (yq, _) = run(g.as_mut(), &q, &q);
    assert!(db(rms(&yq[n / 2..]) / rms(&q[n / 2..])) < -60.0);
    // Limiter: hard ceiling.
    let mut lim = fx("dynamics_rack", &[("comp_on", 0.0), ("lim_on", 1.0), ("lim_threshold", -6.0)]);
    let loud = noise(5, n, 1.0);
    let (yl, _) = run(lim.as_mut(), &loud, &loud);
    assert!(yl.iter().all(|v| v.abs() <= db_to_gain(-6.0) + 1e-6));
    let mut sc = fx("dynamics_rack", &[("comp_on", 0.0), ("lim_on", 1.0), ("lim_threshold", -6.0), ("soft_clip", 1.0)]);
    let (ys, _) = run(sc.as_mut(), &loud, &loud);
    assert!(ys.iter().all(|v| v.abs() <= db_to_gain(-6.0) + 1e-6));
}

#[test]
fn multiband_crossovers_sum_flat_and_bands_compress_independently() {
    let mut e = fx("multiband_compressor", &[("b1_ratio", 1.0), ("b2_ratio", 1.0), ("b3_ratio", 1.0), ("b4_ratio", 1.0)]);
    for f in [40.0, 120.0, 700.0, 2000.0, 6000.0, 10000.0, 15000.0] {
        let g = tone_db(e.as_mut(), f, 0.25);
        assert!(g.abs() < 0.02, "crossover sum not flat at {f}: {g}");
    }
    // Band 1 compresses a loud bass tone; a treble tone in band 3 is untouched.
    let mut c = fx("multiband_compressor", &[("b1_threshold", -30.0), ("b1_ratio", 4.0), ("b3_threshold", 0.0), ("b2_threshold", 0.0), ("b4_threshold", 0.0)]);
    assert!(tone_db(c.as_mut(), 60.0, 0.5) < -10.0);
    assert!(tone_db(c.as_mut(), 5000.0, 0.5).abs() < 0.1);
    assert!((c.transfer_db(0, -6.0).unwrap() + 24.0).abs() < 1e-4);
    // Solo band 4: low tones vanish.
    let mut s = fx("multiband_compressor", &[("b4_solo", 1.0), ("b4_ratio", 1.0)]);
    assert!(tone_db(s.as_mut(), 200.0, 0.25) < -40.0);
    assert!(tone_db(s.as_mut(), 16000.0, 0.25).abs() < 0.5);
}

#[test]
fn tube_compressor_levels() {
    let mut e = fx("tube_compressor", &[("threshold", -30.0), ("ratio", 4.0)]);
    // Low level: below the knee and practically linear.
    assert!(tone_db(e.as_mut(), 1000.0, 0.005).abs() < 0.05);
    // Above threshold: reduced roughly as the static curve says (RMS detector: sine RMS = peak − 3 dB).
    let level_rms = -6.0 - 3.01;
    let want = e.transfer_db(0, level_rms).unwrap() - level_rms;
    let got = tone_db(e.as_mut(), 1000.0, 0.5);
    assert!((got - want as f64).abs() < 1.5, "tube gain {got} vs static {want}");
}

#[test]
fn flanger_static_comb_notch() {
    // initial = final: a fixed delay of d + 1 samples, mixed 50/50 → notch at fs / (2(d+1)).
    let mut e = fx("flanger", &[("initial_delay", 1.0), ("final_delay", 1.0), ("feedback", 0.0), ("mix", 50.0)]);
    let d = 48.0 + 1.0;
    let notch = SR as f64 / (2.0 * d);
    assert!(tone_db(e.as_mut(), notch, 0.3) < -30.0);
    assert!(tone_db(e.as_mut(), 2.0 * notch, 0.3).abs() < 0.2);
}

#[test]
fn chorus_flanger_and_phaser_modulate() {
    for id in ["chorus_flanger", "phaser"] {
        let mut e = fx(id, &[]);
        let x = noise(7, SR as usize, 0.3);
        let (y, _) = run(e.as_mut(), &x, &x);
        let ratio = db(rms(&y) / rms(&x));
        assert!(ratio.abs() < 6.0, "{id}: level {ratio}");
        let diff: f64 = y.iter().zip(&x).map(|(a, b)| ((a - b) as f64).powi(2)).sum();
        assert!(diff > 1.0, "{id}: no effect");
    }
    let mut cf = fx("chorus_flanger", &[("mode", 1.0)]);
    let x = noise(8, 4800, 0.3);
    let (y, _) = run(cf.as_mut(), &x, &x);
    assert!(y.iter().all(|v| v.is_finite()));
    // Static phaser (depth 0): 4 all-pass stages at 1 kHz, mixed 50/50 → a notch where the
    // chain's phase is −180°, i.e. each stage −45°.
    let mut ph = fx("phaser", &[("depth", 0.0), ("upper_freq", 1000.0), ("stages", 4.0), ("mix", 50.0)]);
    let a = util::AllPass1::coef(1000.0, SR);
    let phase = |f: f64| {
        let w = 2.0 * std::f64::consts::PI * f / SR as f64;
        // (a + e^{-jw}) / (1 + a e^{-jw})
        let (nr, ni) = (a + w.cos(), -w.sin());
        let (dr, di) = (1.0 + a * w.cos(), -a * w.sin());
        ni.atan2(nr) - di.atan2(dr)
    };
    let (mut lo, mut hi) = (10.0, 1000.0);
    for _ in 0..60 {
        let mid = 0.5 * (lo + hi);
        if 4.0 * phase(mid) > -std::f64::consts::PI { lo = mid } else { hi = mid }
    }
    assert!(tone_db(ph.as_mut(), lo, 0.3) < -25.0, "phaser notch at {lo}");
}

#[test]
fn delays_put_echoes_where_asked() {
    let n = SR as usize;
    let mut imp = vec![0.0f32; n];
    imp[0] = 1.0;
    let mut e = fx("analog_delay", &[("delay", 100.0), ("feedback", 0.0), ("dry", 0.0), ("wet", 100.0), ("mode", 2.0)]);
    let (y, _) = run(e.as_mut(), &imp, &imp);
    let peak = y.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).unwrap().0;
    assert!((peak as i64 - 4800).abs() <= 3, "analog echo at {peak}");
    let mut m =
        fx("multitap_delay", &[("delay1", 50.0), ("delay2", 120.0), ("level1", 0.0), ("level2", -6.0), ("level3", -96.0), ("level4", -96.0), ("mix", 100.0)]);
    let (y, _) = run(m.as_mut(), &imp, &imp);
    assert!((y[2400] - 1.0).abs() < 1e-4, "tap 1 {}", y[2400]);
    assert!((y[5760] - db_to_gain(-6.0)).abs() < 1e-4, "tap 2 {}", y[5760]);
    let elsewhere = y.iter().enumerate().filter(|(i, _)| *i != 2400 && *i != 5760).fold(0.0f32, |a, (_, v)| a.max(v.abs()));
    assert!(elsewhere < 1e-6);
    // Feedback repeats.
    let mut f = fx(
        "multitap_delay",
        &[("delay1", 50.0), ("feedback1", 50.0), ("level1", 0.0), ("level2", -96.0), ("level3", -96.0), ("level4", -96.0), ("mix", 100.0)],
    );
    let (y, _) = run(f.as_mut(), &imp, &imp);
    assert!((y[4800] - 0.5).abs() < 1e-4 && (y[7200] - 0.25).abs() < 1e-4);
}

/// RT60 by Schroeder backward integration (−5 … −25 dB fit, extrapolated).
fn rt60(ir: &[f32]) -> f64 {
    let mut e: Vec<f64> = ir.iter().map(|&v| (v as f64) * (v as f64)).collect();
    for i in (0..e.len() - 1).rev() {
        e[i] += e[i + 1];
    }
    let total = e[0];
    let t = |level: f64| e.iter().position(|&v| 10.0 * (v / total).log10() < level).unwrap_or(e.len()) as f64 / SR as f64;
    (t(-25.0) - t(-5.0)) * 3.0
}

#[test]
fn convolution_reverb_impulses() {
    for (k, want_rt) in [(0usize, 0.45), (2, 1.9), (4, 1.6)] {
        let mut e = fx("convolution_reverb", &[("impulse", k as f32), ("mix", 100.0), ("width", 100.0)]);
        let lat = e.latency();
        let n = (SR * 4.0) as usize;
        let mut imp = vec![0.0f32; n];
        imp[0] = 1.0;
        let (l, r) = run(e.as_mut(), &imp, &imp);
        let (l, r) = (&l[lat..], &r[lat..]);
        let el: f64 = l.iter().map(|&v| (v as f64).powi(2)).sum();
        assert!((el - 1.0).abs() < 0.02, "impulse {k}: energy {el}");
        let rt = rt60(l);
        assert!((rt - want_rt).abs() < want_rt * 0.25, "impulse {k}: RT60 {rt} vs {want_rt}");
        // Decorrelated stereo.
        let c: f64 = l.iter().zip(r).map(|(a, b)| (*a as f64) * (*b as f64)).sum();
        assert!(c.abs() < 0.3, "impulse {k}: L/R correlation {c}");
    }
    // Room size shortens the tail.
    let mut a = fx("convolution_reverb", &[("impulse", 2.0), ("mix", 100.0), ("room_size", 40.0)]);
    let n = (SR * 3.0) as usize;
    let mut imp = vec![0.0f32; n];
    imp[0] = 1.0;
    let (l, _) = run(a.as_mut(), &imp, &imp);
    let rt = rt60(&l[a.latency()..]);
    assert!((rt - 0.76).abs() < 0.25, "scaled RT60 {rt}");
}

#[test]
fn surround_reverb_tail_decays() {
    let mut e = fx("surround_reverb", &[("decay", 1.0), ("dry", 0.0), ("wet", 100.0)]);
    let n = (SR * 3.0) as usize;
    let mut imp = vec![0.0f32; n];
    imp[0] = 1.0;
    let (l, r) = run(e.as_mut(), &imp, &imp);
    let seg = |s: f32, e: f32| rms(&l[(s * SR) as usize..(e * SR) as usize]);
    assert!(seg(0.05, 0.3) > 1e-4);
    let drop = db(seg(1.6, 1.9) / seg(0.1, 0.4));
    assert!(drop < -45.0 && drop > -120.0, "RT60≈1 s: 1.5 s later {drop} dB");
    assert!(r.iter().all(|v| v.is_finite()));
}

#[test]
fn click_remover_repairs_clicks() {
    let n = SR as usize * 2;
    let clean: Vec<f32> = sine(440.0, 0.3, SR as f64, n, 0.0).iter().zip(noise(9, n, 0.01)).map(|(a, b)| a + b).collect();
    let mut dirty = clean.clone();
    for k in 0..30 {
        let at = 3000 + k * 3001;
        dirty[at] += 0.6;
        dirty[at + 1] -= 0.4;
    }
    let mut e = fx("click_remover", &[]);
    let lat = e.latency();
    let (y, _) = run(e.as_mut(), &dirty, &dirty);
    let err = |s: &[f32], off: usize| -> f64 { (lat + 2000..n).map(|i| ((s[i] - clean[i - off]) as f64).powi(2)).sum::<f64>().sqrt() };
    let before = err(&dirty[..], 0);
    let after = err(&y, lat);
    assert!(db(after / before) < -20.0, "error {before} → {after}");
}

#[test]
fn channel_and_stereo_tools() {
    let n = 4800;
    let l = noise(10, n, 0.3);
    let r = noise(11, n, 0.3);
    let mut sw = fx("channel_mixer", &[("l_from_l", 0.0), ("l_from_r", 100.0), ("r_from_l", 100.0), ("r_from_r", 0.0), ("invert_r", 1.0)]);
    let (a, b) = run(sw.as_mut(), &l, &r);
    assert!(a.iter().zip(&r).all(|(x, y)| (x - y).abs() < 1e-6));
    assert!(b.iter().zip(&l).all(|(x, y)| (x + y).abs() < 1e-6));
    let mut mono = fx("stereo_expander", &[("expand", 0.0)]);
    let (a, b) = run(mono.as_mut(), &l, &r);
    assert!(a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-6));
    let mut wide = fx("stereo_expander", &[("expand", 200.0), ("center_pan", 100.0)]);
    let (a, b) = run(wide.as_mut(), &l, &l);
    // centre fully right: identical L/R input → nothing left
    assert!(a.iter().all(|v| v.abs() < 1e-6) && b.iter().zip(&l).all(|(x, y)| (x - y).abs() < 1e-6));
    // Mute ramps to silence.
    let mut m = fx("mute", &[("mute", 1.0)]);
    let (a, _) = run(m.as_mut(), &l, &r);
    assert!(a.iter().all(|v| *v == 0.0));
}

#[test]
fn distortion_harmonics() {
    let n = SR as usize;
    let x = sine(500.0, 0.5, SR as f64, n, 0.0);
    let mut sym = fx("distortion", &[("drive", 24.0), ("curve", 1.0)]);
    let (y, _) = run(sym.as_mut(), &x, &x);
    let h = |s: &[f32], k: f64| tone_amplitude(&s[n / 2..], 500.0 * k, SR as f64);
    assert!(h(&y, 3.0) > 0.1 * h(&y, 1.0), "hard clip makes odd harmonics");
    assert!(h(&y, 2.0) < 1e-3 * h(&y, 1.0), "symmetric: no even harmonics");
    let mut asym = fx("distortion", &[("drive", 24.0), ("symmetry", 60.0)]);
    let (y, _) = run(asym.as_mut(), &x, &x);
    assert!(h(&y, 2.0) > 0.02 * h(&y, 1.0), "asymmetric: even harmonics");
    // GuitarSuite with everything off is near-transparent at low level.
    let mut gs = fx("guitar_suite", &[("compressor", 0.0), ("distortion", 0.0), ("amp", 0.0), ("filter", 0.0)]);
    assert!(tone_db(gs.as_mut(), 1000.0, 0.01).abs() < 0.05);
    let mut gs2 = fx("guitar_suite", &[]);
    let (y, _) = run(gs2.as_mut(), &x, &x);
    assert!(y.iter().all(|v| v.is_finite()) && rms(&y) > 0.01);
}

#[test]
fn mastering_and_vocal_enhancer() {
    let mut e = fx("mastering", &[("eq_low", 6.0)]);
    assert!((e.response_db(20.0).unwrap() - 6.0).abs() < 0.3);
    assert!((tone_db(e.as_mut(), 30.0, 0.1) - e.response_db(30.0).unwrap()).abs() < 0.1);
    let n = SR as usize;
    let x = noise(12, n, 0.3);
    let mut loud = fx("mastering", &[("loudness", 100.0)]);
    let (y, _) = run(loud.as_mut(), &x, &x);
    assert!(y.iter().all(|v| v.abs() <= db_to_gain(-0.3) + 1e-6));
    assert!(rms(&y) > 1.5 * rms(&x));
    let mut v = fx("vocal_enhancer", &[("mode", 0.0)]);
    assert!(v.response_db(40.0).unwrap() < -6.0);
    assert!(v.response_db(3000.0).unwrap() > 2.0);
    // Below the compressor threshold the measured response is the EQ.
    assert!((tone_db(v.as_mut(), 3000.0, 0.02) - v.response_db(3000.0).unwrap()).abs() < 0.1);
    let mut music = fx("vocal_enhancer", &[("mode", 2.0)]);
    assert!(music.response_db(1500.0).unwrap() < -3.0);
    assert!((tone_db(music.as_mut(), 1500.0, 0.3) - music.response_db(1500.0).unwrap()).abs() < 0.1);
}

#[test]
fn binaural_and_ambisonic_rendering() {
    let n = SR as usize;
    // A correlated low tone stays at unity.
    let mut b = fx("binauralizer", &[]);
    assert!(tone_db(b.as_mut(), 100.0, 0.3).abs() < 0.5);
    // Left-only input reaches the right ear later (interaural time difference).
    let mut imp = vec![0.0f32; 2000];
    imp[100] = 1.0;
    let zero = vec![0.0f32; 2000];
    let (l, r) = run(b.as_mut(), &imp, &zero);
    let pk = |s: &[f32]| s.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).unwrap().0;
    let itd = pk(&r) as f64 - pk(&l) as f64;
    let a = 0.0875f64;
    let th = 30f64.to_radians();
    let want = a / 343.0 * (th + th.sin()) * SR as f64;
    assert!((itd - want).abs() <= 1.5, "ITD {itd} samples vs {want}");
    assert!(l[pk(&l)].abs() > r[pk(&r)].abs(), "head shadow: far ear quieter");
    // Ambisonic pan 180°: the left source ends up on the right.
    let mut p = fx("ambisonics_panner", &[("pan", 180.0)]);
    let x = noise(13, n, 0.3);
    let (l, r) = run(p.as_mut(), &x, &zero[..0].iter().copied().chain(std::iter::repeat_n(0.0, n)).collect::<Vec<_>>());
    assert!(rms(&r) > 3.0 * rms(&l), "rotated: L {} R {}", rms(&l), rms(&r));
    let (g, _) = special::AmbisonicsPanner::source_gains(30.0, 0.0, 0.0, 0.0);
    assert!((g - 1.0).abs() < 1e-6);
}

#[test]
fn loudness_meter_passes_through_and_measures() {
    let n = SR as usize * 3;
    let x = sine(1000.0, 0.1, SR as f64, n, 0.0);
    let mut m = special::LoudnessMeterFx::new(SR, 2);
    let (mut a, mut b) = (x.clone(), x.clone());
    m.process(&mut [&mut a, &mut b]);
    assert_eq!(a, x);
    let mut reference = crate::LoudnessMeter::new(SR as f64, 2);
    reference.process(&[&x, &x]);
    assert!((m.meter().integrated() - reference.integrated()).abs() < 1e-9);
    assert!(m.meter().integrated().is_finite());
}

#[test]
fn realtime_factor_sanity() {
    let secs = 2.0;
    let n = (SR * secs) as usize;
    let l = noise(14, n, 0.3);
    let r = noise(15, n, 0.3);
    let mut slowest = (f64::INFINITY, "");
    for e in effects() {
        let mut f = (e.create)(SR, 2);
        let (mut a, mut b) = (l.clone(), r.clone());
        let t0 = std::time::Instant::now();
        for (ca, cb) in a.chunks_mut(512).zip(b.chunks_mut(512)) {
            f.process(&mut [ca, cb]);
        }
        let dt = t0.elapsed().as_secs_f64().max(1e-9);
        let rtf = secs as f64 / dt;
        if rtf < slowest.0 {
            slowest = (rtf, e.id);
        }
        assert!(rtf > 1.0, "{}: only {rtf:.1}× realtime", e.id);
    }
    eprintln!("slowest effect: {} at {:.0}× realtime", slowest.1, slowest.0);
}

fn gain_db(x: f32) -> f32 {
    crate::gain_to_db(x)
}

fn db_to_gain(x: f32) -> f32 {
    crate::db_to_gain(x)
}
