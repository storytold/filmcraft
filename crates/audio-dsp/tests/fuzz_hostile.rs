//! Hostile parameters: every registered effect with absurd sample rates, channel counts, parameter
//! values and buffers must neither panic nor hang.

use filmcraft_audio_dsp::{create_effect, effects};
use std::panic::{AssertUnwindSafe, catch_unwind};

/// Run `f` on a worker thread and fail fast if it does not finish: a regression to a hang must
/// not stall CI.
fn with_deadline(secs: u64, f: impl FnOnce() + Send + 'static) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        f();
        let _ = tx.send(());
    });
    // a loaded runner may need longer: FILMCRAFT_HOSTILE_TIMEOUT_SECS overrides the budget
    let secs = std::env::var("FILMCRAFT_HOSTILE_TIMEOUT_SECS").ok().and_then(|v| v.parse::<u64>().ok()).filter(|&n| n > 0).unwrap_or(secs);
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

fn run(id: &str, sr: f32, ch: usize, rng: &mut Rng) {
    let Some(mut fx) = create_effect(id, sr, ch) else { return };
    let params = fx.params();
    for round in 0..6 {
        for p in params {
            let v = match round {
                0 => p.min,
                1 => p.max,
                2 => HOSTILE[rng.below(HOSTILE.len())],
                _ => p.min + (p.max - p.min) * (rng.below(1000) as f32 / 1000.0),
            };
            fx.set_param(p.id, v);
        }
        let lens = [0usize, 1, 3, 255, 4096, 20_000];
        let n = lens[rng.below(lens.len())];
        let nch = [0usize, 1, 2, ch, ch + 3][rng.below(5)];
        let mut bufs: Vec<Vec<f32>> = (0..nch)
            .map(|c| {
                let len = if rng.below(6) == 0 { n / 2 } else { n };
                let mode = (round + c) % 4;
                (0..len)
                    .map(|_| match mode {
                        0 => HOSTILE[rng.below(HOSTILE.len())],
                        1 => (rng.next() as i32 as f32) / 1e6,
                        2 => 0.0,
                        _ => (rng.next() as f32 / u64::MAX as f32) * 2.0 - 1.0,
                    })
                    .collect()
            })
            .collect();
        let mut refs: Vec<&mut [f32]> = bufs.iter_mut().map(|v| v.as_mut_slice()).collect();
        fx.process(&mut refs);
        let _ = fx.latency();
        for f in [0.0, 1.0, 1000.0, 1e9, f64::NAN, f64::INFINITY, -5.0] {
            let _ = fx.response_db(f);
        }
        for x in [f32::NAN, 0.0, -1e9, 1e9] {
            let _ = fx.transfer_db(0, x);
            let _ = fx.transfer_db(usize::MAX, x);
        }
        if round == 2 {
            fx.reset();
        }
    }
}

#[test]
fn effects_survive_hostile_setups() {
    with_deadline(120, body);
}

fn body() {
    let mut rng = Rng(0x1357_9bdf_2468_ace1);
    let rates = [0.0f32, 1.0, 100.0, 8000.0, 44100.0, 48000.0, 192_000.0, 768_000.0, -1.0, f32::NAN, f32::INFINITY, 1e-30];
    let chans = [0usize, 1, 2, 6, 9];
    let mut failures = Vec::new();
    for e in effects() {
        for &sr in &rates {
            for &ch in &chans {
                let r = catch_unwind(AssertUnwindSafe(|| run(e.id, sr, ch, &mut Rng(rng.next()))));
                if let Err(p) = r {
                    let msg = p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
                    failures.push(format!("{} sr={sr} ch={ch}: {msg}", e.id));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}
