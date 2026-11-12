//! Test signals and measures.

pub fn snr_db(reference: &[f32], test: &[f32]) -> f64 {
    let n = reference.len().min(test.len());
    let mut s = 0f64;
    let mut e = 0f64;
    for i in 0..n {
        s += (reference[i] as f64).powi(2);
        e += (reference[i] as f64 - test[i] as f64).powi(2);
    }
    if e == 0.0 { 200.0 } else { 10.0 * (s.max(1e-20) / e).log10() }
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

/// Music-like: harmonic chords with vibrato changing every 0.5 s, decaying noise bursts.
pub fn music(n: usize, seed: u64) -> Vec<f32> {
    let mut r = Rng(seed);
    let notes = [220.0f32, 277.2, 329.6, 440.0, 349.2, 523.3, 392.0, 659.3];
    (0..n)
        .map(|i| {
            let t = i as f32 / 48000.0;
            let chord = (t * 2.0) as usize;
            let mut s = 0.0;
            for k in 0..3 {
                let f = notes[(chord * 3 + k * 2) % notes.len()] * (1.0 + 0.003 * (2.0 * std::f32::consts::PI * 5.0 * t).sin());
                for h in 1..6 {
                    s += (2.0 * std::f32::consts::PI * f * h as f32 * t).sin() * 0.08 / h as f32;
                }
            }
            let env = (-(t * 2.0).fract() * 3.0).exp();
            s * (0.5 + 0.5 * env) + 0.02 * r.next() * env
        })
        .collect()
}
