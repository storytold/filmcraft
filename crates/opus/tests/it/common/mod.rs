#![allow(dead_code)]

use std::path::PathBuf;

/// Plain SNR (dB) of `test` against `reference` over the common length.
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

const NBANDS: usize = 21;
const BANDS: [usize; NBANDS + 1] = [0, 2, 4, 6, 8, 10, 12, 14, 16, 20, 24, 28, 32, 40, 48, 56, 68, 80, 96, 120, 156, 200];
const WIN: usize = 480;
const STEP: usize = 120;
const NFREQ: usize = 240;
/// Calibration: white noise at 48 dB SNR maps to quality 0 (RFC 6716 §6.1).
const CAL: f64 = 0.144;

fn spectra(x: &[f32], channels: usize, ch: usize, rate: u32) -> Vec<[f64; NFREQ]> {
    // Work at 48 kHz bin spacing (100 Hz per bin); lower rates use proportionally fewer bins.
    let scale = 48000 / rate as usize;
    let win = WIN / scale;
    let step = STEP / scale;
    let n = x.len() / channels;
    let nframes = if n >= win { (n - win) / step + 1 } else { 0 };
    let w: Vec<f64> = (0..win).map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * (i as f64 + 0.5) / win as f64).cos()).collect();
    let nb = NFREQ / scale;
    let mut out = Vec::with_capacity(nframes);
    let tw = twiddles(win);
    let mut buf = vec![(0f64, 0f64); win];
    let mut spec = vec![(0f64, 0f64); win];
    let mut scratch = vec![(0f64, 0f64); win];
    for f in 0..nframes {
        for i in 0..win {
            buf[i] = (x[(f * step + i) * channels + ch] as f64 * w[i], 0.0);
        }
        fft(&buf, 1, &mut spec, &tw, &mut scratch);
        let mut ps = [0f64; NFREQ];
        for (k, p) in ps.iter_mut().enumerate().take(nb) {
            *p = spec[k].0 * spec[k].0 + spec[k].1 * spec[k].1;
        }
        out.push(ps);
    }
    out
}

/// `e^(-2πi·j/n)` for `j` in `0..n`: the twiddle factors of every sub-transform of a size-`n` FFT.
fn twiddles(n: usize) -> Vec<(f64, f64)> {
    (0..n)
        .map(|j| {
            let ang = -2.0 * std::f64::consts::PI * j as f64 / n as f64;
            (ang.cos(), ang.sin())
        })
        .collect()
}

/// Recursive mixed-radix DFT (sizes with factors 2, 3, 5) of `x[0], x[xs], x[2·xs], …` into
/// `out`. `tw` is `twiddles` of the top-level size, which `out.len()` divides; `scratch` holds at
/// least `out.len()` values.
fn fft(x: &[(f64, f64)], xs: usize, out: &mut [(f64, f64)], tw: &[(f64, f64)], scratch: &mut [(f64, f64)]) {
    let n = out.len();
    if n == 1 {
        out[0] = x[0];
        return;
    }
    let p = [2usize, 3, 5].into_iter().find(|p| n.is_multiple_of(*p)).unwrap_or(n);
    let m = n / p;
    let stride = tw.len() / n;
    for r in 0..p {
        fft(&x[r * xs..], xs * p, &mut out[r * m..(r + 1) * m], tw, scratch);
    }
    for (k, o) in scratch[..n].iter_mut().enumerate() {
        let j = k % m;
        let mut acc = (0f64, 0f64);
        // e = r·k mod n, kept below n without a division per term.
        let mut e = 0;
        for r in 0..p {
            let v = out[r * m + j];
            let (c, s) = tw[e * stride];
            acc.0 += v.0 * c - v.1 * s;
            acc.1 += v.0 * s + v.1 * c;
            e += k;
            if e >= n {
                e -= n;
            }
        }
        *o = acc;
    }
    out.copy_from_slice(&scratch[..n]);
}

/// Opus quality metric in the spirit of `opus_compare` (RFC 6716 §6.1): a per-band,
/// masking-weighted spectral distortion between reference and test, mapped so that 100 means
/// identical output and 0 is the pass threshold (white noise at 48 dB SNR). Phase-insensitive
/// (compares power spectra), as the RFC requires tolerance to resampler phase differences.
pub fn opus_quality(reference: &[f32], test: &[f32], channels: usize, rate: u32) -> f64 {
    let n = reference.len().min(test.len());
    let (reference, test) = (&reference[..n], &test[..n]);
    let scale = 48000 / rate as usize;
    let mut total_err = 0f64;
    let mut count = 0usize;
    for ch in 0..channels {
        let xr = spectra(reference, channels, ch, rate);
        let xt = spectra(test, channels, ch, rate);
        let nf = xr.len();
        // Band energies of the reference with temporal and frequency spreading (masking).
        let mut mask = vec![[0f64; NBANDS]; nf];
        for f in 0..nf {
            for b in 0..NBANDS {
                let (lo, hi) = (BANDS[b] / scale, (BANDS[b + 1] / scale).max(BANDS[b] / scale + 1));
                let e: f64 = xr[f][lo..hi.min(NFREQ / scale)].iter().sum::<f64>() / (hi - lo) as f64;
                mask[f][b] = e;
            }
        }
        for f in 0..nf {
            for b in 1..NBANDS {
                mask[f][b] += 0.1 * mask[f][b - 1];
            }
            for b in (0..NBANDS - 1).rev() {
                mask[f][b] += 0.03 * mask[f][b + 1];
            }
        }
        for f in 1..nf {
            for b in 0..NBANDS {
                mask[f][b] += 0.5 * mask[f - 1][b];
            }
        }
        for f in (0..nf.saturating_sub(1)).rev() {
            for b in 0..NBANDS {
                mask[f][b] += 0.1 * mask[f + 1][b];
            }
        }
        for f in 0..nf {
            let mut ef = 0f64;
            let mut nb = 0;
            for b in 0..NBANDS {
                let (lo, hi) = (BANDS[b] / scale, BANDS[b + 1] / scale);
                if hi <= lo {
                    continue;
                }
                let m = mask[f][b] + 1e-9 * (WIN * WIN) as f64 / (scale * scale) as f64;
                let mut eb = 0f64;
                for k in lo..hi {
                    let d = xt[f][k].sqrt() - xr[f][k].sqrt();
                    eb += d * d;
                }
                eb /= (hi - lo) as f64 * m;
                ef += eb;
                nb += 1;
            }
            total_err += (ef / nb.max(1) as f64).powi(2);
            count += 1;
        }
    }
    let err = (total_err / count.max(1) as f64).sqrt() / CAL;
    100.0 * (1.0 - 0.5 * (1.0 + err).ln() / 1.13f64.ln())
}

/// ffmpeg (see `filmcraft_testkit::oracle`); `None` after printing `SKIPPED`.
pub fn ffmpeg() -> Option<PathBuf> {
    filmcraft_testkit::ffmpeg_or_skip("opus oracle")
}

pub fn fixtures() -> PathBuf {
    filmcraft_testkit::fixtures_dir("opus/oracle")
}

/// Minimal Ogg demuxer: returns (packets, last granule position).
pub fn ogg_packets(data: &[u8]) -> (Vec<Vec<u8>>, i64) {
    let mut packets = Vec::new();
    let mut cur = Vec::new();
    let mut pos = 0;
    let mut granule = 0i64;
    while pos + 27 <= data.len() && &data[pos..pos + 4] == b"OggS" {
        let g = i64::from_le_bytes(data[pos + 6..pos + 14].try_into().unwrap());
        let nseg = data[pos + 26] as usize;
        let table = &data[pos + 27..pos + 27 + nseg];
        let mut body = pos + 27 + nseg;
        for &l in table {
            cur.extend_from_slice(&data[body..body + l as usize]);
            body += l as usize;
            if l < 255 {
                packets.push(std::mem::take(&mut cur));
            }
        }
        if g >= 0 {
            granule = g;
        }
        pos = body;
    }
    (packets, granule)
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

/// Logarithmic sine sweep 40 Hz -> 20 kHz at 48 kHz.
pub fn sweep(n: usize, amp: f32) -> Vec<f32> {
    let (f0, f1) = (40f64, 20000f64);
    let t_total = n as f64 / 48000.0;
    let k = (f1 / f0).ln();
    (0..n)
        .map(|i| {
            let t = i as f64 / 48000.0;
            ((2.0 * std::f64::consts::PI * f0 * t_total / k * ((t / t_total * k).exp() - 1.0)).sin() as f32) * amp
        })
        .collect()
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

/// Speech-like: glottal pulses through formant resonators with a syllabic envelope.
pub fn speech(n: usize, seed: u64) -> Vec<f32> {
    let mut r = Rng(seed);
    let fs = 48000.0f32;
    let formants = [(700.0f32, 130.0f32), (1220.0, 70.0), (2600.0, 160.0)];
    let mut states = [[0f32; 2]; 3];
    let mut phase = 0f32;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f32 / fs;
        let f0 = 120.0 + 30.0 * (2.0 * std::f32::consts::PI * 0.7 * t).sin();
        phase += f0 / fs;
        let pulse = if phase >= 1.0 {
            phase -= 1.0;
            1.0
        } else {
            0.0
        };
        let syll = (2.0 * std::f32::consts::PI * 3.0 * t).sin().max(0.0);
        let voiced = if (t * 1.3).fract() < 0.8 { syll } else { 0.0 };
        let fric = if (t * 1.3).fract() > 0.85 { 0.05 * r.next() } else { 0.0 };
        let mut s = 0f32;
        for (j, &(f, bw)) in formants.iter().enumerate() {
            let rr = (-std::f32::consts::PI * bw / fs).exp();
            let a1 = 2.0 * rr * (2.0 * std::f32::consts::PI * f / fs).cos();
            let a2 = -rr * rr;
            let y = pulse * voiced + a1 * states[j][0] + a2 * states[j][1];
            states[j][1] = states[j][0];
            states[j][0] = y;
            s += y;
        }
        out.push(s * 0.05 + fric);
    }
    out
}
