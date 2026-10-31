//! SILK output resampler (RFC 6716 §4.2.9: non-normative filter, normative delay budget).
//!
//! A windowed-sinc polyphase interpolator whose group delay matches the delay allocation of
//! RFC 6716 Table 54 (and the integer delays used when no rate conversion is needed), so that the
//! SILK output lines up with the CELT layer in Hybrid mode.

/// Dot product with eight independent partial sums, so the additions pipeline and the loop
/// vectorises instead of waiting on one 4-cycle add chain. Rounds differently from a
/// sequential sum (the resampler is not normative).
#[inline]
fn dot8(a: &[f32], b: &[f32]) -> f32 {
    let mut s = [0f32; 8];
    let (ca, ra) = a.as_chunks::<8>();
    let (cb, rb) = b.as_chunks::<8>();
    for (x, y) in ca.iter().zip(cb) {
        for l in 0..8 {
            s[l] += x[l] * y[l];
        }
    }
    let mut acc = ((s[0] + s[4]) + (s[1] + s[5])) + ((s[2] + s[6]) + (s[3] + s[7]));
    for (x, y) in ra.iter().zip(rb) {
        acc += x * y;
    }
    acc
}

#[derive(Clone)]
pub struct Resampler {
    fin: u32,
    fout: u32,
    /// Pure-delay mode (equal rates): delay line.
    copy_delay: usize,
    /// Polyphase taps per output phase.
    phases: Vec<(i64, Vec<f32>)>,
    period: u64,
    /// Total delay in units of 1/fout input samples.
    delay_q: i64,
    half: i64,
    future: i64,
    hist: Vec<f32>,
    /// Absolute input index of `hist[0]`.
    hist_start: i64,
    n_in: i64,
    n_out: u64,
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// Least-squares fractional-delay interpolator: taps at offsets `-past+1 ..= future` (in input
/// samples) approximating a delay of `frac` samples over `[0, wp]`, with zero response weighted
/// over `[ws, pi]` when `ws < pi` (decimation).
fn design_fractional(past: i64, future: i64, frac: f64, wp: f64, ws: f64) -> Vec<f64> {
    let offs: Vec<f64> = ((-past + 1)..=future).map(|m| m as f64).collect();
    let n = offs.len();
    // \int_a^b cos(w x) dw
    let ic = |x: f64, a: f64, b: f64| if x.abs() < 1e-12 { b - a } else { ((b * x).sin() - (a * x).sin()) / x };
    let stop_w = 30.0;
    const LOW_W: f64 = 0.05;
    let mut q = vec![vec![0f64; n + 1]; n];
    for i in 0..n {
        for k in 0..n {
            let d = offs[i] - offs[k];
            let mut v = ic(d, 0.0, 0.6 * wp) + LOW_W * ic(d, 0.6 * wp, wp);
            if ws < std::f64::consts::PI {
                v += stop_w * ic(d, ws, std::f64::consts::PI);
            }
            q[i][k] = v + if i == k { 1e-9 } else { 0.0 };
        }
        q[i][n] = ic(offs[i] - frac, 0.0, 0.6 * wp) + LOW_W * ic(offs[i] - frac, 0.6 * wp, wp);
    }
    // Gaussian elimination with partial pivoting.
    for c in 0..n {
        let p = (c..n).max_by(|&a, &b| q[a][c].abs().total_cmp(&q[b][c].abs())).unwrap_or(c);
        q.swap(c, p);
        let piv = q[c][c];
        for r in 0..n {
            if r != c {
                let f = q[r][c] / piv;
                if f != 0.0 {
                    for k in c..=n {
                        q[r][k] -= f * q[c][k];
                    }
                }
            }
        }
    }
    (0..n).map(|i| q[i][n] / q[i][i]).collect()
}

impl Resampler {
    pub fn new(fin: u32, fout: u32) -> Resampler {
        let mut r = Resampler {
            fin,
            fout,
            copy_delay: 0,
            phases: Vec::new(),
            period: 1,
            delay_q: 0,
            half: 0,
            future: 0,
            hist: Vec::new(),
            hist_start: 0,
            n_in: 0,
            n_out: 0,
        };
        if fin == fout {
            r.copy_delay = match fin {
                8000 => 4,
                12000 => 9,
                _ => 12,
            };
            r.hist = vec![0.0; r.copy_delay];
            return r;
        }
        // Delay in milliseconds per internal bandwidth (RFC 6716 Table 54).
        let delay_ms = match fin {
            8000 => 0.538,
            12000 => 0.692,
            _ => 0.706,
        };
        let d_in = delay_ms * fin as f64 / 1000.0; // input samples
        // Future taps are limited by the delay budget; past taps are free.
        let future = (d_in.floor() as i64).max(1);
        let past = 24i64;
        r.half = past;
        r.future = future;
        r.delay_q = (d_in * fout as f64).round() as i64;
        let g = gcd(fin, fout);
        r.period = (fout / g) as u64;
        let pi = std::f64::consts::PI;
        // Passband edge and (for decimation) stopband edge in radians per input sample.
        let (wp, ws) = if fout > fin { (0.94 * pi, pi) } else { (0.88 * pi * fout as f64 / fin as f64, pi * fout as f64 / fin as f64) };
        for j in 0..r.period as i64 {
            let num = j * fin as i64 - r.delay_q;
            let base = num.div_euclid(fout as i64);
            let frac = num.rem_euclid(fout as i64) as f64 / fout as f64;
            let taps = design_fractional(past, future, frac, wp, ws);
            r.phases.push((base, taps.iter().map(|&t| t as f32).collect()));
        }
        r.hist = vec![0.0; (past + future + 4) as usize];
        r.hist_start = -(past + future + 4);
        r
    }

    pub fn rates(&self) -> (u32, u32) {
        (self.fin, self.fout)
    }

    /// Resamples `input` (16-bit scale) and appends the output samples.
    pub fn process(&mut self, input: &[i16], out: &mut Vec<f32>) {
        if self.fin == self.fout {
            for &v in input {
                self.hist.push(v as f32);
            }
            let n = self.hist.len() - self.copy_delay;
            out.extend_from_slice(&self.hist[..n]);
            self.hist.drain(..n);
            return;
        }
        self.hist.extend(input.iter().map(|&v| v as f32));
        self.n_in += input.len() as i64;
        let total_out = (self.n_in as u64 * self.fout as u64) / self.fin as u64;
        // phases[ph] repeats every `period` outputs, advancing the input position by `step`
        // (= fin * period / fout, exact because period = fout / gcd).
        let step = self.fin as i64 * self.period as i64 / self.fout as i64;
        let mut cycle = (self.n_out / self.period) as i64;
        let mut ph = (self.n_out % self.period) as usize;
        while self.n_out < total_out {
            let (base0, ref taps) = self.phases[ph];
            let base = base0 + cycle * step;
            ph += 1;
            if ph as u64 == self.period {
                ph = 0;
                cycle += 1;
            }
            let first = base - self.half + 1;
            let i0 = first - self.hist_start;
            let mut acc = 0f32;
            if i0 >= 0 && i0 as usize + taps.len() <= self.hist.len() {
                // Window fully inside the history (always, after the first call).
                let win = &self.hist[i0 as usize..i0 as usize + taps.len()];
                acc = dot8(win, taps);
            } else {
                for (m, &t) in taps.iter().enumerate() {
                    let idx = first + m as i64 - self.hist_start;
                    if idx >= 0 && (idx as usize) < self.hist.len() {
                        acc += self.hist[idx as usize] * t;
                    }
                }
            }
            out.push(acc);
            self.n_out += 1;
        }
        // Trim history, keeping enough for the next taps.
        let keep_from = self.n_in - (self.half + self.future + 4);
        if keep_from > self.hist_start {
            let drop = (keep_from - self.hist_start) as usize;
            self.hist.drain(..drop.min(self.hist.len()));
            self.hist_start = keep_from;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsample_sine_and_delay() {
        for &(fin, fout) in &[(16000u32, 48000u32), (8000, 48000), (12000, 48000), (16000, 24000), (16000, 8000), (12000, 16000)] {
            let mut r = Resampler::new(fin, fout);
            let f = 440.0;
            let n = fin as usize / 10;
            let x: Vec<i16> = (0..n).map(|i| ((2.0 * std::f64::consts::PI * f * i as f64 / fin as f64).sin() * 10000.0) as i16).collect();
            let mut y = Vec::new();
            for c in x.chunks(fin as usize / 50) {
                r.process(c, &mut y);
            }
            assert_eq!(y.len(), n * fout as usize / fin as usize);
            // Compare against the ideal delayed sine.
            let delay_ms = match fin {
                8000 => 0.538,
                12000 => 0.692,
                _ => 0.706,
            };
            let mut err = 0f64;
            let mut sig = 0f64;
            for (j, &v) in y.iter().enumerate().skip(fout as usize / 100) {
                let t = j as f64 / fout as f64 - delay_ms / 1000.0;
                let ideal = (2.0 * std::f64::consts::PI * f * t).sin() * 10000.0;
                err += (v as f64 - ideal).powi(2);
                sig += ideal * ideal;
            }
            let snr = 10.0 * (sig / err).log10();
            assert!(snr > 35.0, "{fin}->{fout}: {snr} dB");
        }
    }

    #[test]
    fn dot8_matches_sequential_sum() {
        for len in [1usize, 7, 8, 9, 35, 36, 71] {
            let a: Vec<f32> = (0..len).map(|i| ((i * 37 % 19) as f32 - 9.0) * 1000.0).collect();
            let b: Vec<f32> = (0..len).map(|i| ((i * 11 % 7) as f32 - 3.0) / 7.0).collect();
            let seq: f64 = a.iter().zip(&b).map(|(&x, &y)| x as f64 * y as f64).sum();
            assert!((dot8(&a, &b) as f64 - seq).abs() < 1e-2, "len {len}");
        }
    }
}
