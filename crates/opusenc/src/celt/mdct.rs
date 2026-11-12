//! Inverse MDCT with the low-overlap window and in-place TDAC (RFC 6716 §4.3.7).
//!
//! The IMDCT is computed through an N/4-point complex FFT (pre-rotation, FFT, post-rotation),
//! producing the N/2 "folded" time samples; the window overlap region is then unfolded against the
//! folded tail of the previous block, which the caller keeps in place in the output buffer.

#[derive(Clone, Copy, Default, Debug)]
pub struct Cpx {
    pub re: f32,
    pub im: f32,
}

impl Cpx {
    #[inline]
    fn mul(self, o: Cpx) -> Cpx {
        Cpx { re: self.re * o.re - self.im * o.im, im: self.re * o.im + self.im * o.re }
    }
    #[inline]
    fn add(self, o: Cpx) -> Cpx {
        Cpx { re: self.re + o.re, im: self.im + o.im }
    }
    #[inline]
    fn sub(self, o: Cpx) -> Cpx {
        Cpx { re: self.re - o.re, im: self.im - o.im }
    }
}

/// Mixed-radix (2, 3, 4, 5) forward complex FFT, unscaled. Radix 2, 3 and 4 use dedicated
/// butterflies; radix 5 uses a generic one.
pub struct Fft {
    n: usize,
    factors: Vec<usize>,
    twiddle: Vec<Cpx>,
}

impl Fft {
    pub fn new(n: usize) -> Fft {
        let mut factors = Vec::new();
        let mut m = n;
        for p in [4usize, 2, 3, 5] {
            while m.is_multiple_of(p) {
                factors.push(p);
                m /= p;
            }
        }
        assert_eq!(m, 1, "unsupported FFT size {n}");
        let twiddle = (0..n)
            .map(|k| {
                let ph = -2.0 * std::f64::consts::PI * k as f64 / n as f64;
                Cpx { re: ph.cos() as f32, im: ph.sin() as f32 }
            })
            .collect();
        Fft { n, factors, twiddle }
    }

    pub fn process(&self, data: &mut [Cpx], scratch: &mut Vec<Cpx>) {
        scratch.clear();
        scratch.extend_from_slice(&data[..self.n]);
        self.rec(&mut data[..self.n], scratch, 1, self.n, 0, 1);
    }

    fn rec(&self, out: &mut [Cpx], inp: &[Cpx], in_stride: usize, n: usize, fi: usize, tw_stride: usize) {
        let p = self.factors[fi];
        let m = n / p;
        if m == 1 {
            for (i, o) in out.iter_mut().enumerate().take(p) {
                *o = inp[i * in_stride];
            }
        } else {
            for q in 0..p {
                self.rec(&mut out[q * m..(q + 1) * m], &inp[q * in_stride..], in_stride * p, m, fi + 1, tw_stride * p);
            }
        }
        let tw = &self.twiddle;
        match p {
            2 => {
                for k in 0..m {
                    let a0 = out[k];
                    let a1 = out[m + k].mul(tw[k * tw_stride]);
                    out[k] = a0.add(a1);
                    out[m + k] = a0.sub(a1);
                }
            }
            4 => {
                for k in 0..m {
                    let t0 = out[k];
                    let t1 = out[m + k].mul(tw[k * tw_stride]);
                    let t2 = out[2 * m + k].mul(tw[2 * k * tw_stride]);
                    let t3 = out[3 * m + k].mul(tw[3 * k * tw_stride]);
                    let s0 = t0.add(t2);
                    let d0 = t0.sub(t2);
                    let s1 = t1.add(t3);
                    let d1 = t1.sub(t3);
                    out[k] = s0.add(s1);
                    out[2 * m + k] = s0.sub(s1);
                    // -j * d1 = (d1.im, -d1.re)
                    out[m + k] = Cpx { re: d0.re + d1.im, im: d0.im - d1.re };
                    out[3 * m + k] = Cpx { re: d0.re - d1.im, im: d0.im + d1.re };
                }
            }
            3 => {
                // sin(-2*pi/3) = -sqrt(3)/2
                let h = tw[self.n / 3].im;
                for k in 0..m {
                    let t0 = out[k];
                    let t1 = out[m + k].mul(tw[k * tw_stride]);
                    let t2 = out[2 * m + k].mul(tw[2 * k * tw_stride]);
                    let s = t1.add(t2);
                    let d = t1.sub(t2);
                    let a = Cpx { re: t0.re - 0.5 * s.re, im: t0.im - 0.5 * s.im };
                    out[k] = t0.add(s);
                    // j * h * d = (-h * d.im, h * d.re)
                    out[m + k] = Cpx { re: a.re - h * d.im, im: a.im + h * d.re };
                    out[2 * m + k] = Cpx { re: a.re + h * d.im, im: a.im - h * d.re };
                }
            }
            _ => {
                // Radix 5 (at most once per transform): generic butterfly with the five roots of
                // unity looked up once.
                let mut w5 = [Cpx::default(); 5];
                for (q, w) in w5.iter_mut().enumerate() {
                    *w = tw[q * (self.n / p)];
                }
                let mut t = [Cpx::default(); 5];
                for k in 0..m {
                    t[0] = out[k];
                    for q in 1..p {
                        t[q] = out[q * m + k].mul(tw[q * k * tw_stride]);
                    }
                    for q2 in 0..p {
                        let mut acc = t[0];
                        for q in 1..p {
                            acc = acc.add(t[q].mul(w5[(q * q2) % p]));
                        }
                        out[q2 * m + k] = acc;
                    }
                }
            }
        }
    }
}

pub struct Imdct {
    /// Full MDCT length (2x the number of coefficients).
    n: usize,
    trig: Vec<f32>,
    fft: Fft,
}

impl Imdct {
    pub fn new(n: usize) -> Imdct {
        let trig = (0..n / 2).map(|i| (2.0 * std::f64::consts::PI * (i as f64 + 0.125) / n as f64).cos() as f32).collect();
        Imdct { n, trig, fft: Fft::new(n / 4) }
    }

    /// Inverse transform of `n/2` coefficients (the decoder's; here the tests' oracle for
    /// [`Self::forward`]) read from `input` at `stride`.
    ///
    /// Writes the folded block to `out[overlap/2 .. overlap/2 + n/2]` and unfolds the first
    /// `overlap` samples against the previous block's folded tail already stored in
    /// `out[0 .. overlap/2]`.
    #[cfg(test)]
    pub fn backward(&self, input: &[f32], stride: usize, out: &mut [f32], window: &[f32], overlap: usize, work: &mut ImdctScratch) {
        let n2 = self.n >> 1;
        let n4 = self.n >> 2;
        let t = &self.trig;
        let z = &mut work.z;
        z.clear();
        for i in 0..n4 {
            let x1 = input[2 * i * stride];
            let x2 = input[stride * (n2 - 1 - 2 * i)];
            let yr = x2 * t[i] + x1 * t[n4 + i];
            let yi = x1 * t[i] - x2 * t[n4 + i];
            z.push(Cpx { re: yi, im: yr });
        }
        self.fft.process(z, &mut work.scratch);
        let base = overlap >> 1;
        let y = &mut out[base..base + n2];
        for i in 0..n4.div_ceil(2) {
            let front = z[i];
            let back = z[n4 - 1 - i];
            let (re, im) = (front.im, front.re);
            let (t0, t1) = (t[i], t[n4 + i]);
            let yr0 = re * t0 + im * t1;
            let yi0 = re * t1 - im * t0;
            let (re, im) = (back.im, back.re);
            let (t0, t1) = (t[n4 - i - 1], t[n2 - i - 1]);
            let yr1 = re * t0 + im * t1;
            let yi1 = re * t1 - im * t0;
            y[2 * i] = yr0;
            y[n2 - 1 - 2 * i] = yi0;
            y[n2 - 2 - 2 * i] = yr1;
            y[2 * i + 1] = yi1;
        }
        // Mirror on both sides for TDAC.
        for i in 0..overlap / 2 {
            let x1 = out[overlap - 1 - i];
            let x2 = out[i];
            let w1 = window[i];
            let w2 = window[overlap - 1 - i];
            out[i] = x2 * w2 - x1 * w1;
            out[overlap - 1 - i] = x2 * w1 + x1 * w2;
        }
    }
}

impl Imdct {
    /// Forward MDCT (encoder): `block` holds `n/2 + overlap` time samples (the previous frame's
    /// last `overlap` samples, then this frame's `n/2`); `out` receives the `n/2` coefficients that
    /// [`Self::backward`] turns back into this block, so that the decoder's overlap-add returns
    /// the input delayed by `overlap` samples. Built as the exact inverse of `backward`: the
    /// window fold undoes its mirror, and the scaled transpose of its core is the core's inverse.
    pub fn forward(&self, block: &[f32], out: &mut [f32], window: &[f32], overlap: usize, work: &mut ImdctScratch) {
        let n2 = self.n >> 1;
        let h = overlap >> 1;
        let s = |i: usize| block.get(i).copied().unwrap_or(0.0);
        let w = |i: usize| window.get(i).copied().unwrap_or(0.0);
        let mut y = std::mem::take(&mut work.fold);
        y.clear();
        y.resize(n2, 0.0);
        for (m, v) in y.iter_mut().enumerate() {
            *v = if m < h {
                -s(h - 1 - m) * w(h - 1 - m) + s(h + m) * w(h + m)
            } else if m + h < n2 {
                s(m + h)
            } else {
                let i = m + h - n2;
                s(n2 + i) * w(overlap - 1 - i) + s(n2 + overlap - 1 - i) * w(i)
            };
        }
        self.core_transpose(&y, out, work);
        let scale = 4.0 / self.n as f32;
        for v in out.iter_mut().take(n2) {
            *v *= scale;
        }
        work.fold = y;
    }

    /// The transpose of the core of [`Self::backward`] (pre-rotation, FFT, post-rotation, without
    /// the window mirror): `y` holds `n/2` folded time samples, `out` receives `n/2` coefficients,
    /// unscaled.
    fn core_transpose(&self, y: &[f32], out: &mut [f32], work: &mut ImdctScratch) {
        let n2 = self.n >> 1;
        let n4 = self.n >> 2;
        let t = &self.trig;
        let at = |i: usize| y.get(i).copied().unwrap_or(0.0);
        let z = &mut work.z;
        z.clear();
        z.resize(n4, Cpx::default());
        for i in 0..n4.div_ceil(2) {
            let (t0, t1) = (t[i], t[n4 + i]);
            let (g0, g1) = (at(2 * i), at(n2 - 1 - 2 * i));
            z[i].re += g0 * t1 - g1 * t0;
            z[i].im += g0 * t0 + g1 * t1;
            let j = n4 - 1 - i;
            let (t0, t1) = (t[n4 - i - 1], t[n2 - i - 1]);
            let (g0, g1) = (at(n2 - 2 - 2 * i), at(2 * i + 1));
            z[j].re += g0 * t1 - g1 * t0;
            z[j].im += g0 * t0 + g1 * t1;
        }
        // The transpose of the forward DFT is the conjugate DFT: conj(F(conj(z))).
        for v in z.iter_mut() {
            v.im = -v.im;
        }
        self.fft.process(z, &mut work.scratch);
        for v in z.iter_mut() {
            v.im = -v.im;
        }
        for (i, w) in z.iter().enumerate() {
            let (ti, tn) = (t[i], t[n4 + i]);
            if let Some(o) = out.get_mut(2 * i) {
                *o = w.re * ti + w.im * tn;
            }
            if let Some(o) = out.get_mut(n2 - 1 - 2 * i) {
                *o = -w.re * tn + w.im * ti;
            }
        }
    }
}

#[derive(Default)]
pub struct ImdctScratch {
    z: Vec<Cpx>,
    scratch: Vec<Cpx>,
    fold: Vec<f32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fft_matches_dft() {
        for &n in &[15usize, 30, 60, 120, 240, 480, 960] {
            let fft = Fft::new(n);
            let x: Vec<Cpx> = (0..n).map(|i| Cpx { re: ((i * 7) % 13) as f32 - 6.0, im: ((i * 3) % 5) as f32 - 2.0 }).collect();
            let mut y = x.clone();
            fft.process(&mut y, &mut Vec::new());
            for k in [0, 1, 7, n / 2, n - 1] {
                let mut acc = (0f64, 0f64);
                for (i, v) in x.iter().enumerate() {
                    let ph = -2.0 * std::f64::consts::PI * (i * k) as f64 / n as f64;
                    acc.0 += v.re as f64 * ph.cos() - v.im as f64 * ph.sin();
                    acc.1 += v.re as f64 * ph.sin() + v.im as f64 * ph.cos();
                }
                assert!((acc.0 - y[k].re as f64).abs() < 1e-2 && (acc.1 - y[k].im as f64).abs() < 1e-2, "n={n} k={k}");
            }
        }
    }

    /// The previous generic implementation (every butterfly multiplies by every DFT root), kept
    /// to measure how far the specialised kernels move the output.
    fn reference_fft(n: usize, data: &mut [Cpx]) {
        let fft = Fft::new(n);
        let inp = data.to_vec();
        rec_ref(&fft, data, &inp, 1, n, 0, 1);
    }

    fn rec_ref(f: &Fft, out: &mut [Cpx], inp: &[Cpx], in_stride: usize, n: usize, fi: usize, tw_stride: usize) {
        let p = f.factors[fi];
        let m = n / p;
        if m == 1 {
            for (i, o) in out.iter_mut().enumerate().take(p) {
                *o = inp[i * in_stride];
            }
        } else {
            for q in 0..p {
                rec_ref(f, &mut out[q * m..(q + 1) * m], &inp[q * in_stride..], in_stride * p, m, fi + 1, tw_stride * p);
            }
        }
        let big = f.n;
        let mut t = [Cpx::default(); 5];
        for k in 0..m {
            for q in 0..p {
                let w = f.twiddle[(q * k * tw_stride) % big];
                t[q] = out[q * m + k].mul(w);
            }
            for q2 in 0..p {
                let mut acc = Cpx::default();
                for (q, tq) in t.iter().enumerate().take(p) {
                    let w = f.twiddle[((q * q2) % p) * (big / p)];
                    acc = acc.add(tq.mul(w));
                }
                out[q2 * m + k] = acc;
            }
        }
    }

    /// Every size the CELT frame sizes use (N/4 for N = 2 * {120, 240, 480, 960}, plus the
    /// short-block sizes) agrees with the previous implementation to f32 rounding.
    #[test]
    fn fft_matches_reference_implementation() {
        for &n in &[15usize, 30, 60, 120, 240, 480, 960] {
            let fft = Fft::new(n);
            let x: Vec<Cpx> =
                (0..n).map(|i| Cpx { re: (((i * 7919) % 2003) as f32 - 1000.0) / 500.0, im: (((i * 104729) % 1009) as f32 - 500.0) / 250.0 }).collect();
            let mut new = x.clone();
            fft.process(&mut new, &mut Vec::new());
            let mut old = x.clone();
            reference_fft(n, &mut old);
            let peak = old.iter().map(|c| c.re.abs().max(c.im.abs())).fold(0f32, f32::max);
            let max_diff = new.iter().zip(&old).map(|(a, b)| (a.re - b.re).abs().max((a.im - b.im).abs())).fold(0f32, f32::max);
            assert!(max_diff <= 1e-5 * peak, "n={n} max_diff={max_diff} peak={peak}");
        }
    }

    /// Forward then the decoder's overlap-add (the same buffer moves as `CeltDecoder`) returns
    /// the input, delayed by the overlap.
    #[test]
    fn forward_is_inverted_by_the_decoder_overlap_add() {
        const OV: usize = 120;
        const DBS: usize = 2048;
        let window: Vec<f32> = (0..OV)
            .map(|i| {
                let x = std::f64::consts::FRAC_PI_2 * (i as f64 + 0.5) / OV as f64;
                (std::f64::consts::FRAC_PI_2 * x.sin() * x.sin()).sin() as f32
            })
            .collect();
        for frame in [120usize, 240, 480, 960] {
            let im = Imdct::new(2 * frame);
            let len = frame * 12;
            let input: Vec<f32> = (0..len).map(|t| (t as f32 * 0.031).sin() * 3000.0 + ((t * 7919) % 211) as f32 - 105.0).collect();
            let mut hist = vec![0f32; OV];
            let mut mem = vec![0f32; DBS + OV];
            let mut output = Vec::new();
            let mut work = ImdctScratch::default();
            let mut coeffs = vec![0f32; frame];
            for f in 0..len / frame {
                let mut block = hist.clone();
                block.extend_from_slice(&input[f * frame..(f + 1) * frame]);
                im.forward(&block, &mut coeffs, &window, OV, &mut work);
                hist.copy_from_slice(&block[frame..]);
                mem.copy_within(frame..DBS + OV / 2, 0);
                im.backward(&coeffs, 1, &mut mem[DBS - frame..], &window, OV, &mut work);
                output.extend_from_slice(&mem[DBS - frame..DBS]);
            }
            let worst = (OV..len).map(|t| (output[t] - input[t - OV]).abs()).fold(0f32, f32::max);
            assert!(worst < 0.05, "frame {frame}: worst error {worst}");
        }
    }

    /// The folded output must equal the direct IMDCT formula folded by TDAC symmetry.
    #[test]
    fn imdct_matches_direct_formula() {
        let n = 240;
        let n2 = n / 2;
        let im = Imdct::new(n);
        let x: Vec<f32> = (0..n2).map(|k| ((k * 37 % 11) as f32 - 5.0) / 5.0).collect();
        let overlap = 0;
        let mut out = vec![0f32; n2];
        im.backward(&x, 1, &mut out, &[], overlap, &mut ImdctScratch::default());
        // Direct IMDCT y[t] = sum X[k] cos(2pi/N (t + 1/2 + N/4)(k + 1/2)), t in 0..N.
        let y: Vec<f64> = (0..n)
            .map(|t| (0..n2).map(|k| x[k] as f64 * (2.0 * std::f64::consts::PI / n as f64 * (t as f64 + 0.5 + n as f64 / 4.0) * (k as f64 + 0.5)).cos()).sum())
            .collect();
        // The folded half: out[i] relates to y in the middle half; check proportionality.
        let mid: Vec<f64> = (0..n2).map(|i| y[n / 4 + i]).collect();
        let dot: f64 = mid.iter().zip(&out).map(|(a, b)| a * *b as f64).sum();
        let ea: f64 = mid.iter().map(|a| a * a).sum();
        let eb: f64 = out.iter().map(|b| (*b as f64).powi(2)).sum();
        let corr = dot.abs() / (ea.sqrt() * eb.sqrt());
        assert!(corr > 0.999, "corr {corr}");
    }
}
