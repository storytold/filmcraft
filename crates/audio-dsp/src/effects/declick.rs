//! Automatic Click Remover: clicks are found as outliers of the linear-prediction residual and
//! replaced by autoregressive interpolation.
//!
//! Textbook method (Vaseghi & Rayner style): per 512-sample block an order-`p` LPC model is fitted
//! (autocorrelation + Levinson–Durbin over a Hann-windowed 1152-sample analysis span); samples
//! whose prediction error exceeds `k·σ` (σ from the median absolute residual) mark a click,
//! nearby marks are merged, and each region is re-synthesised by forward prediction from the
//! clean past and backward prediction from the future, cross-faded. Regions longer than the
//! maximum click length are left alone (they are transients, not clicks). Latency is one block
//! plus the look-ahead.

use crate::{AudioEffect, ParamSpec, ParamValues, Unit, block_len, param_plumbing};

const BLOCK: usize = 512;
const CONTEXT: usize = 512;
const LOOKAHEAD: usize = 128;
const SPAN: usize = CONTEXT + BLOCK + LOOKAHEAD;
const MAX_ORDER: usize = 32;

#[derive(Clone, Debug)]
struct Chan {
    buf: Vec<f32>,
    out: Vec<f32>,
    pos: usize,
}

impl Chan {
    fn new() -> Chan {
        Chan { buf: vec![0.0; SPAN], out: vec![0.0; BLOCK], pos: 0 }
    }
    fn reset(&mut self) {
        self.buf.iter_mut().for_each(|v| *v = 0.0);
        self.out.iter_mut().for_each(|v| *v = 0.0);
        self.pos = 0;
    }
}

/// Shared analysis scratch.
#[derive(Clone, Debug)]
struct Scratch {
    win: Vec<f32>,
    xw: Vec<f64>,
    resid: Vec<f32>,
    sorted: Vec<f32>,
    marks: Vec<bool>,
}

/// LPC coefficients `a[1..=p]` (prediction x̂[n] = Σ a_k x[n−k]) by Levinson–Durbin.
/// Returns `false` for silent / degenerate input.
fn lpc(x: &[f64], p: usize, a: &mut [f64; MAX_ORDER + 1]) -> bool {
    let mut r = [0.0f64; MAX_ORDER + 1];
    for (k, rk) in r.iter_mut().enumerate().take(p + 1) {
        *rk = x[k..].iter().zip(x).map(|(u, v)| u * v).sum();
    }
    if !r[0].is_finite() || r[0] <= 1e-12 {
        return false;
    }
    r[0] *= 1.0 + 1e-6; // white-noise correction
    let mut e = r[0];
    let mut tmp = [0.0f64; MAX_ORDER + 1];
    *a = [0.0; MAX_ORDER + 1];
    for i in 1..=p {
        let mut acc = r[i];
        for j in 1..i {
            acc -= a[j] * r[i - j];
        }
        let k = acc / e;
        tmp[..i].copy_from_slice(&a[..i]);
        a[i] = k;
        for j in 1..i {
            a[j] = tmp[j] - k * tmp[i - j];
        }
        e *= 1.0 - k * k;
        if e <= 1e-15 {
            return false;
        }
    }
    true
}

/// Automatic click remover.
pub struct ClickRemover {
    pv: ParamValues,
    ch: Vec<Chan>,
    s: Scratch,
    /// Number of clicks repaired so far (for metering / tests).
    pub repaired: usize,
}

impl ClickRemover {
    pub const PARAMS: &'static [ParamSpec] =
        &[ParamSpec::new("threshold", "Threshold", 1.0, 100.0, 30.0, Unit::None), ParamSpec::new("complexity", "Complexity", 1.0, 100.0, 16.0, Unit::None)];

    pub fn new(_sample_rate: f32, channels: usize) -> Self {
        let win = (0..SPAN).map(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * (i as f64 + 0.5) / SPAN as f64).cos()) as f32).collect();
        ClickRemover {
            pv: ParamValues::new(Self::PARAMS),
            ch: vec![Chan::new(); channels.max(1)],
            s: Scratch { win, xw: vec![0.0; SPAN], resid: vec![0.0; SPAN], sorted: vec![0.0; SPAN], marks: vec![false; SPAN] },
            repaired: 0,
        }
    }
    fn apply_params(&mut self, _snap: bool) {}

    /// Detection factor k (residual outliers above k·σ are clicks).
    fn k(&self) -> f32 {
        4.0 + 0.2 * self.pv.v("threshold")
    }
    fn order(&self) -> usize {
        (8.0 + 24.0 * (self.pv.v("complexity") - 1.0) / 99.0).round() as usize
    }
    fn max_len(&self) -> usize {
        (16.0 + 48.0 * (self.pv.v("complexity") - 1.0) / 99.0).round() as usize
    }

    fn analyse(c: &mut Chan, s: &mut Scratch, p: usize, k: f32, max_len: usize) -> usize {
        let buf = &mut c.buf;
        for i in 0..SPAN {
            s.xw[i] = (buf[i] * s.win[i]) as f64;
        }
        let mut a = [0.0f64; MAX_ORDER + 1];
        if !lpc(&s.xw, p, &mut a) {
            return 0;
        }
        // Prediction residual over the span.
        for n in 0..SPAN {
            s.resid[n] = if n < p {
                0.0
            } else {
                let mut pred = 0.0f64;
                for j in 1..=p {
                    pred += a[j] * buf[n - j] as f64;
                }
                (buf[n] as f64 - pred) as f32
            };
        }
        for n in 0..SPAN {
            s.sorted[n] = s.resid[n].abs();
        }
        let valid = &mut s.sorted[p..];
        let mid = valid.len() / 2;
        valid.select_nth_unstable_by(mid, |x, y| x.total_cmp(y));
        let sigma = valid[mid] / 0.6745;
        if sigma <= 1e-9 {
            return 0;
        }
        let thr = k * sigma;
        s.marks.iter_mut().for_each(|m| *m = false);
        let mut n = CONTEXT;
        let mut repaired = 0;
        while n < CONTEXT + BLOCK {
            if s.resid[n].abs() <= thr {
                n += 1;
                continue;
            }
            // Group outliers separated by ≤ 4 samples.
            let first = n;
            let mut last = n;
            let mut m = n + 1;
            while m < SPAN - p && m <= last + 4 {
                if s.resid[m].abs() > thr {
                    last = m;
                }
                m += 1;
            }
            let start = first.saturating_sub(2).max(p);
            let end = (last + 3).min(SPAN - p - 1);
            n = last + 1;
            if end <= start || end - start > max_len {
                continue;
            }
            // Forward prediction from the (clean) past.
            let len = end - start;
            let mut fwd = [0.0f32; 80];
            let mut bwd = [0.0f32; 80];
            for t in 0..len {
                let i = start + t;
                let mut pred = 0.0f64;
                for j in 1..=p {
                    let v = if i - j >= start { fwd[i - j - start] } else { buf[i - j] };
                    pred += a[j] * v as f64;
                }
                fwd[t] = pred as f32;
            }
            // Backward prediction from the future (stationary AR: same coefficients reversed).
            for t in (0..len).rev() {
                let i = start + t;
                let mut pred = 0.0f64;
                for j in 1..=p {
                    let v = if i + j < end { bwd[i + j - start] } else { buf[i + j] };
                    pred += a[j] * v as f64;
                }
                bwd[t] = pred as f32;
            }
            for t in 0..len {
                let w = (t + 1) as f32 / (len + 1) as f32;
                buf[start + t] = (1.0 - w) * fwd[t] + w * bwd[t];
            }
            repaired += 1;
        }
        repaired
    }
}

impl AudioEffect for ClickRemover {
    param_plumbing!("click_remover");
    fn latency(&self) -> usize {
        BLOCK + LOOKAHEAD
    }
    fn reset(&mut self) {
        self.ch.iter_mut().for_each(Chan::reset);
        self.repaired = 0;
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        let n = block_len(channels);
        let nch = channels.len().min(self.ch.len());
        let (p, k, max_len) = (self.order(), self.k(), self.max_len().min(80));
        for (ci, chan) in channels.iter_mut().enumerate().take(nch) {
            let c = &mut self.ch[ci];
            for v in chan[..n].iter_mut() {
                let x = *v;
                *v = c.out[c.pos];
                c.buf[SPAN - BLOCK + c.pos] = x;
                c.pos += 1;
                if c.pos == BLOCK {
                    c.pos = 0;
                    self.repaired += Self::analyse(c, &mut self.s, p, k, max_len);
                    // Emit the finished block, then slide the span.
                    c.out.copy_from_slice(&c.buf[CONTEXT..CONTEXT + BLOCK]);
                    c.buf.copy_within(BLOCK.., 0);
                }
            }
        }
    }
}
