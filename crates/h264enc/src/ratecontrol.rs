//! Frame-level rate control: constant QP, constant quality (CRF-like), 1-pass ABR with VBV, and 2-pass VBR.

use crate::nal::SliceType;

pub fn qp2qscale(qp: f64) -> f64 {
    0.85 * 2f64.powf((qp - 12.0) / 6.0)
}
pub fn qscale2qp(q: f64) -> f64 {
    12.0 + 6.0 * (q / 0.85).log2()
}

const QCOMP: f64 = 0.6;

fn type_idx(t: SliceType) -> usize {
    match t {
        SliceType::I => 0,
        SliceType::P => 1,
        SliceType::B => 2,
    }
}

/// Relative qscale factor per frame type (I frames finer, B frames coarser).
fn type_factor(t: SliceType) -> f64 {
    match t {
        SliceType::I => 1.0 / 1.4,
        SliceType::P => 1.0,
        SliceType::B => 1.3,
    }
}

/// Statistics of one frame from the first pass.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameStat {
    pub slice_type: SliceType,
    pub qscale: f64,
    pub bits: u64,
    pub cplx: f64,
}

/// First-pass statistics, serialisable as text.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PassStats {
    pub frames: Vec<FrameStat>,
    pub fps: f64,
}

impl PassStats {
    pub fn to_text(&self) -> String {
        let mut s = format!("h264enc-pass1 fps={}\n", self.fps);
        for f in &self.frames {
            let t = match f.slice_type {
                SliceType::I => 'I',
                SliceType::P => 'P',
                SliceType::B => 'B',
            };
            s.push_str(&format!("{t} {} {} {}\n", f.qscale, f.bits, f.cplx));
        }
        s
    }
    pub fn from_text(text: &str) -> Option<PassStats> {
        let mut lines = text.lines();
        let head = lines.next()?;
        let fps = head.strip_prefix("h264enc-pass1 fps=")?.trim().parse().ok()?;
        let mut frames = Vec::new();
        for l in lines {
            let mut it = l.split_whitespace();
            let t = match it.next()? {
                "I" => SliceType::I,
                "P" => SliceType::P,
                "B" => SliceType::B,
                _ => return None,
            };
            frames.push(FrameStat { slice_type: t, qscale: it.next()?.parse().ok()?, bits: it.next()?.parse().ok()?, cplx: it.next()?.parse().ok()? });
        }
        Some(PassStats { frames, fps })
    }
}

#[derive(Clone, Debug)]
pub enum RcKind {
    Qp(u8),
    Crf(f32),
    Abr { kbps: u32 },
    TwoPass { kbps: u32, plan: Vec<f64>, planned_bits: Vec<f64>, planned_total: f64, remaining_bits: Vec<f64> },
}

pub struct RateControl {
    kind: RcKind,
    fps: f64,
    /// bits ≈ coef * cplx / qscale per frame type.
    coef: [f64; 3],
    coef_n: [u32; 3],
    total_bits: f64,
    frames: u64,
    sum_s: f64,
    // VBV
    vbv_max_bits_per_frame: f64,
    vbv_size: f64,
    vbv_fill: f64,
    pub pass1: Vec<FrameStat>,
    model_done: f64,
    inflight: std::collections::VecDeque<f64>,
    last_q: f64,
    /// Virtual history for 1-pass ABR: (frame count, complexity of one virtual frame), fixed at the first frame.
    prior: Option<(f64, f64)>,
    /// Sum and count of the rate-model terms (`coef · cplx^QCOMP / type_factor`) of the coded P and B frames.
    inter_s: f64,
    inter_n: u64,
}

impl RateControl {
    pub fn new(kind: RcKind, fps: f64, vbv_maxrate_kbps: Option<u32>, vbv_buf_kbits: Option<u32>) -> Self {
        let max = vbv_maxrate_kbps.map_or(0.0, |k| k as f64 * 1000.0 / fps);
        let size = vbv_buf_kbits.map_or(0.0, |k| k as f64 * 1000.0);
        RateControl {
            kind,
            fps,
            coef: [1.6, 1.4, 1.2],
            coef_n: [0; 3],
            total_bits: 0.0,
            frames: 0,
            sum_s: 0.0,
            vbv_max_bits_per_frame: max,
            vbv_size: size,
            vbv_fill: size * 0.9,
            pass1: Vec::new(),
            model_done: 0.0,
            inflight: std::collections::VecDeque::new(),
            last_q: 0.0,
            prior: None,
            inter_s: 0.0,
            inter_n: 0,
        }
    }

    /// Plan a second pass from first-pass statistics.
    pub fn plan_two_pass(stats: &PassStats, kbps: u32) -> RcKind {
        let n = stats.frames.len().max(1);
        let target = kbps as f64 * 1000.0 * n as f64 / stats.fps;
        let c: Vec<f64> = stats.frames.iter().map(|f| (f.bits as f64 * f.qscale).max(1.0)).collect();
        let qs = |rf: f64| -> Vec<f64> {
            stats.frames.iter().zip(&c).map(|(f, &ci)| (rf * ci.powf(1.0 - QCOMP) * type_factor(f.slice_type)).clamp(qp2qscale(0.0), qp2qscale(51.0))).collect()
        };
        let bits_for = |q: &[f64]| -> f64 { stats.frames.iter().zip(q).map(|(f, &qi)| f.bits as f64 * f.qscale / qi).sum() };
        // bisection on log(rf)
        let (mut lo, mut hi) = (-30.0f64, 30.0f64);
        for _ in 0..100 {
            let mid = 0.5 * (lo + hi);
            if bits_for(&qs(mid.exp())) > target {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let plan = qs((0.5 * (lo + hi)).exp());
        let planned_bits: Vec<f64> = stats.frames.iter().zip(&plan).map(|(f, &q)| f.bits as f64 * f.qscale / q).collect();
        let planned_total = planned_bits.iter().sum();
        // `remaining_bits[i]` is the inclusive suffix for frame `i`; retain an
        // empty suffix so callers can index it without a special case.
        let mut remaining_bits = vec![0.0; planned_bits.len() + 1];
        for i in (0..planned_bits.len()).rev() {
            remaining_bits[i] = remaining_bits[i + 1] + planned_bits[i];
        }
        RcKind::TwoPass { kbps, plan, planned_bits, planned_total, remaining_bits }
    }

    /// Choose the frame QP (fractional) for frame `index` (display order) of `t` with lookahead complexity `cplx`.
    pub fn frame_qp(&mut self, t: SliceType, cplx: f64, index_in_coding_order: usize) -> f64 {
        let ti = type_idx(t);
        let cplx = cplx.max(1.0);
        let qp = match &self.kind {
            RcKind::Qp(q) => {
                let q = *q as f64;
                return if t == SliceType::B { (q + 2.0).min(51.0) } else { q };
            }
            RcKind::Crf(c) => {
                let c = *c as f64;
                return (c + match t {
                    SliceType::I => -2.0,
                    SliceType::P => 0.0,
                    SliceType::B => 1.5,
                })
                .clamp(0.0, 51.0);
            }
            RcKind::Abr { kbps } => {
                let bpf = *kbps as f64 * 1000.0 / self.fps;
                // rate factor so that past frames at this rf would have hit the target
                let s_here = self.coef[ti] * cplx.powf(QCOMP) / type_factor(t);
                // Virtual history of one second of inter frames so the first I frame gets a realistic share of the
                // budget instead of a single frame's worth. A virtual frame costs what the P and B frames coded so far
                // cost on average; before the first of them, a P frame of 40% of the first frame's complexity under
                // the current model. Re-estimated every frame: the initial guess of `coef` can be several times off,
                // and a history frozen at that guess starves the opening second of bits.
                let fps = self.fps;
                let (pn0, c0) = *self.prior.get_or_insert_with(|| (fps.max(1.0), if t == SliceType::I { 0.4 * cplx } else { cplx }));
                let per_frame = if self.inter_n > 0 { self.inter_s / self.inter_n as f64 } else { self.coef[1] * c0.powf(QCOMP) };
                let fade = (1.0 - self.frames as f64 / pn0).max(0.0);
                let (pn, ps) = (pn0 * fade, pn0 * fade * per_frame);
                let s = self.sum_s + s_here + ps;
                let wanted = bpf * (self.frames as f64 + 1.0 + pn);
                let mut rf = s / wanted;
                let abr_buffer = bpf * self.fps.max(1.0);
                let overflow = (1.0 + (self.total_bits - bpf * self.frames as f64) / abr_buffer).clamp(0.5, 2.0);
                rf *= overflow;
                let q = rf * cplx.powf(1.0 - QCOMP) * type_factor(t);
                qscale2qp(q)
            }
            RcKind::TwoPass { kbps, plan, planned_bits, planned_total, remaining_bits } => {
                // Closed-loop second pass: `m` measures how far real frame sizes deviate from the first-pass model
                // (bits ∝ 1/qscale) at the qscales actually used; the remaining plan is rescaled so its predicted
                // size fits the remaining budget.
                let i = index_in_coding_order.min(plan.len().saturating_sub(1));
                let bpf = *kbps as f64 * 1000.0 / self.fps;
                let target_total = *planned_total;
                let smooth = 0.5 * bpf * self.fps.max(1.0);
                let m = ((self.total_bits + smooth) / (self.model_done + smooth)).clamp(0.3, 3.0);
                let inflight: f64 = self.inflight.iter().sum();
                let remaining_plan = remaining_bits[i];
                let remaining_budget = (target_total - self.total_bits - m * inflight).max(0.05 * remaining_plan.max(1.0));
                let f = (m * remaining_plan / remaining_budget).clamp(0.25, 4.0);
                let q = plan[i] * f;
                self.inflight.push_back(planned_bits[i] * plan[i] / q);
                qscale2qp(q)
            }
        };
        let mut qp = qp.clamp(1.0, 51.0);
        // VBV: make sure the predicted frame size fits in the buffer.
        if self.vbv_size > 0.0 {
            let fill = self.vbv_fill;
            for _ in 0..60 {
                let pred = self.coef[ti] * cplx / qp2qscale(qp);
                if pred <= fill * 0.8 || qp >= 51.0 {
                    break;
                }
                qp += 0.5;
            }
        }
        self.last_q = qp;
        qp
    }

    /// Update after coding a frame with actual `bits`.
    pub fn update(&mut self, t: SliceType, qp: f64, cplx: f64, bits: u64) {
        let ti = type_idx(t);
        let cplx = cplx.max(1.0);
        let q = qp2qscale(qp);
        let c = bits as f64 * q / cplx;
        let n = self.coef_n[ti];
        let w = if n == 0 { 1.0 } else { 0.35f64.max(1.0 / (n + 1) as f64) };
        self.coef[ti] = self.coef[ti] * (1.0 - w) + c * w;
        self.coef_n[ti] += 1;
        // If a type has never been observed, keep it proportional to observed ones.
        for k in 0..3 {
            if self.coef_n[k] == 0 {
                self.coef[k] = self.coef[ti] * [1.2, 1.0, 0.85][k] / [1.2, 1.0, 0.85][ti];
            }
        }
        let s_frame = self.coef[ti] * cplx.powf(QCOMP) / type_factor(t);
        self.sum_s += s_frame;
        if t != SliceType::I {
            self.inter_s += s_frame;
            self.inter_n += 1;
        }
        self.total_bits += bits as f64;
        if let Some(p) = self.inflight.pop_front() {
            self.model_done += p;
        }
        self.frames += 1;
        if self.vbv_size > 0.0 {
            self.vbv_fill = (self.vbv_fill - bits as f64 + self.vbv_max_bits_per_frame).min(self.vbv_size);
            if self.vbv_fill < 0.0 {
                self.vbv_fill = 0.0;
            }
        }
    }

    pub fn record_pass1(&mut self, t: SliceType, qp: f64, cplx: f64, bits: u64) {
        self.pass1.push(FrameStat { slice_type: t, qscale: qp2qscale(qp), bits, cplx });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_pass_plan_caches_total_and_inclusive_suffixes() {
        let stats = PassStats {
            fps: 24.0,
            frames: vec![
                FrameStat { slice_type: SliceType::I, qscale: 1.0, bits: 2_000, cplx: 20.0 },
                FrameStat { slice_type: SliceType::P, qscale: 1.5, bits: 1_000, cplx: 10.0 },
                FrameStat { slice_type: SliceType::B, qscale: 2.0, bits: 500, cplx: 5.0 },
            ],
        };

        let RcKind::TwoPass { planned_bits, planned_total, remaining_bits, .. } = RateControl::plan_two_pass(&stats, 1_000) else {
            panic!("two-pass planning must return a two-pass rate-control plan");
        };

        assert_eq!(remaining_bits.len(), planned_bits.len() + 1);
        assert!((planned_total - planned_bits.iter().sum::<f64>()).abs() < f64::EPSILON);
        for i in 0..planned_bits.len() {
            assert!((remaining_bits[i] - planned_bits[i..].iter().sum::<f64>()).abs() < f64::EPSILON);
        }
        assert_eq!(remaining_bits[planned_bits.len()], 0.0);
    }
}
