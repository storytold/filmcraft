//! Hostile arguments (NaN, infinities, zero, extremes, mismatched lengths) to the free functions,
//! analysers and designers of `filmcraft-audio-dsp`: none may panic.

use filmcraft_audio_dsp::biquad::{Biquad, Coeffs, FilterType};
use filmcraft_audio_dsp::channels::{self, Layout, Mixdown, Pan51};
use filmcraft_audio_dsp::design::{self, Band, Family, Spec};
use filmcraft_audio_dsp::fft::Fft;
use filmcraft_audio_dsp::{LoudnessMeter, Smoothed, ducking, loudness, oversample, remix, resample::StreamResampler, sync};
use std::panic::{AssertUnwindSafe, catch_unwind};

/// Run `f` on a worker thread and fail fast if it does not finish: a regression to a hang must
/// not stall CI.
fn with_deadline(secs: u64, f: impl FnOnce() + Send + 'static) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        f();
        let _ = tx.send(());
    });
    match rx.recv_timeout(std::time::Duration::from_secs(secs)) {
        Ok(()) => {}
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => panic!("hostile-input test did not finish in {secs}s (hang)"),
        Err(_) => panic!("hostile-input test panicked"),
    }
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

const HOSTILE: [f32; 12] = [0.0, -0.0, 1.0, -1.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY, f32::MAX, f32::MIN, f32::MIN_POSITIVE, 1e-30, 1e30];

fn hostile_f64(rng: &mut Rng) -> f64 {
    [0.0, -0.0, 1.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY, f64::MAX, f64::MIN, f64::MIN_POSITIVE, 1e-300, 1e300, 48000.0, 20.0, 1e6][rng.below(15)]
}
fn hostile_f32(rng: &mut Rng) -> f32 {
    HOSTILE[rng.below(HOSTILE.len())]
}
fn hostile_u32(rng: &mut Rng) -> u32 {
    [0, 1, 2, 100, 8000, 44100, 48000, 192_000, u32::MAX, u32::MAX / 2, 1 << 20][rng.below(11)]
}
fn signal(rng: &mut Rng, n: usize) -> Vec<f32> {
    let mode = rng.below(3);
    (0..n)
        .map(|i| {
            if mode == 0 {
                hostile_f32(rng)
            } else if mode == 1 {
                (i as f32 * 0.05).sin()
            } else {
                0.0
            }
        })
        .collect()
}

macro_rules! guard {
    ($name:expr, $failures:expr, $body:block) => {
        let t0 = std::time::Instant::now();
        if catch_unwind(AssertUnwindSafe(|| $body)).is_err() {
            $failures.push($name);
        }
        assert!(t0.elapsed().as_secs() < 10, "{} took {:?}", $name, t0.elapsed());
        if std::env::var_os("FUZZ_TRACE").is_some() && t0.elapsed().as_millis() > 200 {
            eprintln!("slow: {} {:?}", $name, t0.elapsed());
        }
    };
}

#[test]
fn analysis_and_design_apis_survive_hostile_arguments() {
    with_deadline(120, body);
}

fn body() {
    let mut rng = Rng(0x0bad_cafe_1234_5678);
    let mut failures: Vec<&str> = Vec::new();
    let (music, _) = remix::test_music(44100, 120.0, 8, 1);
    for _ in 0..100 {
        let (a, b, c, d, e) = (hostile_f64(&mut rng), hostile_f64(&mut rng), hostile_f64(&mut rng), hostile_f64(&mut rng), hostile_f64(&mut rng));
        let n = [0usize, 1, 2, 5, 1000, 20_000][rng.below(6)];
        let x = signal(&mut rng, n);
        let yn = [0usize, 7, 30_000][rng.below(3)];
        let y = signal(&mut rng, yn);
        let sr = hostile_u32(&mut rng).min(400_000);
        let sr_full = hostile_u32(&mut rng);

        guard!("biquad", failures, {
            let cf = Coeffs::design(FilterType::ALL[rng.below(7)], a, b, c, d);
            let _ = (cf.magnitude(b, a), cf.magnitude_db(e, a));
            let mut bq = Biquad::default();
            let mut buf = x.clone();
            bq.process(&cf, &mut buf);
            let _ = FilterType::from_index(usize::MAX);
        });
        guard!("design", failures, {
            let families = [Family::Butterworth, Family::Bessel, Family::Chebyshev, Family::Elliptic];
            let bands = [Band::LowPass, Band::HighPass, Band::BandPass, Band::BandStop];
            let spec = Spec {
                family: families[rng.below(4)],
                band: bands[rng.below(4)],
                order: [0usize, 1, 8, 100, usize::MAX][rng.below(5)],
                f1: a,
                f2: b,
                ripple_db: c,
                stop_db: d,
            };
            let s = design::design(&spec, e);
            let _ = s.magnitude_db(a, e);
        });
        guard!("fft", failures, {
            let fft = Fft::new([0usize, 1, 2, 3, 64, 1000, 4096][rng.below(7)]);
            let mut re = x.clone();
            let mut im = y.clone();
            fft.forward(&mut re, &mut im);
            fft.inverse(&mut re, &mut im);
        });
        guard!("loudness", failures, {
            let mut m = LoudnessMeter::new(a, [0usize, 1, 2, 6, 9][rng.below(5)]);
            let w: Vec<f64> = (0..rng.below(10)).map(|_| hostile_f64(&mut rng)).collect();
            m.set_channel_weights(&w);
            m.process(&[&x, &y]);
            m.process(&[]);
            m.process_interleaved(&x);
            let _ = (
                m.momentary(),
                m.short_term(),
                m.integrated(),
                m.loudness_range(),
                m.true_peak(),
                m.true_peak_channel(99),
                m.sample_peak_channel(99),
                m.summary(),
            );
            let _ = loudness::measure(&[&x, &y], b);
            let _ = (loudness::normalize_gain_db(a, b), loudness::normalize_gain_db_peak_limited(a, b, c, d));
            let _ = (loudness::k_prefilter(a), loudness::k_rlb(b), loudness::k_weighting_power(c, d), loudness::energy_to_lufs(e));
            let _ = loudness::default_channel_weights(rng.below(100));
        });
        guard!("oversample", failures, {
            let bank = oversample::PolyphaseBank::new([0usize, 1, 2, 4, 8, 1000][rng.below(6)]);
            let mut it = oversample::Interpolator::default();
            for &v in x.iter().take(5000) {
                let _ = it.push_peak(&bank, v);
            }
            let _ = oversample::true_peak_factor(a);
        });
        guard!("smoothed", failures, {
            let mut s = Smoothed::new(hostile_f32(&mut rng), hostile_u32(&mut rng).min(1 << 24));
            s.set(hostile_f32(&mut rng));
            for _ in 0..100 {
                let _ = s.tick();
            }
            let mut s = Smoothed::with_ms(hostile_f32(&mut rng), hostile_f32(&mut rng), hostile_f32(&mut rng));
            s.set(1.0);
            let _ = s.tick();
        });
        guard!("resample", failures, {
            let mut r = StreamResampler::new(sr_full, hostile_u32(&mut rng));
            for _ in 0..3 {
                let start = [0i64, 5, -5, i64::MAX, i64::MIN, 1 << 40][rng.below(6)];
                let want = [0usize, 1, 100, 5000][rng.below(4)];
                let src = x.clone();
                let _ = r.process(start, want, |_, k| vec![src.iter().cycle().take(k.min(5000)).copied().collect()]);
            }
        });
        guard!("sync", failures, {
            let opts = sync::SyncOptions {
                max_lag: Some(a as i64),
                coarse_rate: hostile_u32(&mut rng),
                max_coarse_samples: [0usize, 1, 100, usize::MAX][rng.below(4)],
                phat_beta: hostile_f32(&mut rng),
                refine_window: [0usize, 1, 100, 1 << 17][rng.below(4)],
            };
            let _ = sync::find_offset(&x, &y, sr, &opts);
            let _ = sync::find_offset(&x, &x, sr, &sync::SyncOptions::default());
            let _ = sync::mixdown(&[&x, &y]);
            let _ = sync::decimate(&x, [0usize, 1, 3, usize::MAX][rng.below(4)]);
        });
        guard!("ducking", failures, {
            let env = ducking::envelope_db(&[&x, &y], hostile_f32(&mut rng), hostile_f32(&mut rng), hostile_f32(&mut rng));
            let _ = ducking::activity(&env, hostile_f32(&mut rng), hostile_f32(&mut rng), hostile_f32(&mut rng), hostile_f32(&mut rng));
            let regions = vec![(a, b), (c, d), (e, a)];
            let _ = ducking::duck_keyframes(&regions, a, b, c, d, e);
            let _ = ducking::sensitivity_threshold_db(hostile_f32(&mut rng));
        });
        guard!("channels", failures, {
            let layouts = [Layout::Mono, Layout::Stereo, Layout::Surround51];
            let (from, to) = (layouts[rng.below(3)], layouts[rng.below(3)]);
            let _ = channels::convert(&[&x, &y], from, to, Mixdown::default());
            let _ = channels::convert(&[], from, to, Mixdown::default());
            let _ = channels::convert_owned(vec![x.clone(), y.clone(), vec![]], to, Mixdown::default());
            let p = Pan51 { x: hostile_f32(&mut rng), y: hostile_f32(&mut rng), center: hostile_f32(&mut rng), lfe: hostile_f32(&mut rng) };
            let m = channels::pan51_matrix(from, p);
            let _ = channels::apply_matrix(&m, &[&x]);
            let _ = channels::point_gains(p.x, p.y, p.center);
            let _ = channels::peaks(&[&x, &y]);
            let _ = Layout::from_channels(rng.below(100));
        });
        guard!("remix", failures, {
            let long: Vec<f32> = if n > 5000 { x.clone() } else { Vec::new() };
            let _ = remix::analyze(&[&long, &y], sr);
            let _ = remix::onset_envelope(&x, sr);
            let _ = remix::beat_lead(sr_full);
            let _ = remix::joints_for(a);
            if let Ok(an) = remix::analyze(&[&music[0]], [44100u32, 8000, 1][rng.below(3)]) {
                let target = [0i64, 1, -1, i64::MAX, i64::MIN, an.len * 2, an.len / 3][rng.below(7)];
                if let Ok(plan) = remix::plan(&an, target, remix::Params { segments: a, variations: b }) {
                    let _ = plan.to_text();
                    let mut rd = |_: i64, k: usize| vec![vec![0.0f32; k.min(10_000)]; 2];
                    let _ = remix::render(&plan, [0i64, 5, -5, 1 << 40][rng.below(4)], rng.below(3000), rng.below(4), &mut rd);
                }
            }
            let _ = remix::Plan::from_text(&format!("{} {} {}", a, rng.next(), b));
            let _ = remix::Plan::from_text("");
            let _ = remix::Plan::identity(i64::MAX, i64::MAX).joints();
        });
    }
    failures.sort();
    failures.dedup();
    assert!(failures.is_empty(), "panicked in: {failures:?}");
}
