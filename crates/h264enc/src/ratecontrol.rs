//! Frame-level rate control: constant QP, constant quality (CRF-like), 1-pass ABR with VBV, and 2-pass VBR.

use crate::nal::SliceType;

pub fn qp2qscale(qp: f64) -> f64 {
    0.85 * 2f64.powf((qp - 12.0) / 6.0)
}
pub fn qscale2qp(q: f64) -> f64 {
    12.0 + 6.0 * (q / 0.85).log2()
}

const QCOMP: f64 = 0.6;

/// How far, in QP, the second pass may plan an I or P frame from the quantiser the first pass coded it with, and how
/// much finer than that quantiser it may code any frame. Its size model, bits ∝ 1/qscale from the first pass, only
/// holds near that quantiser. Much finer, a picture the first pass coded almost all as skips (a few kbit of headers)
/// codes its references' quantisation error instead: on slow moves over a detailed still, ten times its predicted
/// size and more.
const PASS1_QP_RANGE: f64 = 6.0;

/// How far, in QP, the second pass's correction may fall from one frame to the next. Near the end of a clip the
/// leftover budget is shared by a few frames, and a correction that drops at once codes B frames far finer than the
/// P frame they reference, at many times their predicted size. Rising stays free: coarser frames only get cheaper.
const CORRECTION_QP_FALL: f64 = 2.0;

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
    TwoPass { kbps: u32, plan: Vec<f64>, planned_bits: Vec<f64>, target_total: f64, remaining_bits: Vec<f64>, finest: Vec<f64> },
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
    /// Lowest QPY constant quality may reach: 0, or -QpBdOffset above 8 bits.
    pub qp_min: f64,
    /// The second pass's correction of the plan for the previous frame (0 before the first).
    last_correction: f64,
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
            qp_min: 0.0,
            last_correction: 0.0,
        }
    }

    /// Plan a second pass from first-pass statistics.
    pub fn plan_two_pass(stats: &PassStats, kbps: u32) -> RcKind {
        let n = stats.frames.len().max(1);
        let target = kbps as f64 * 1000.0 * n as f64 / stats.fps;
        let c: Vec<f64> = stats.frames.iter().map(|f| (f.bits as f64 * f.qscale).max(1.0)).collect();
        let range = 2f64.powf(PASS1_QP_RANGE / 6.0);
        let qs = |rf: f64| -> Vec<f64> {
            // A B frame takes the quantiser of the I or P frame coded just before it, one of its references, with the
            // B offset, as constant quality does. Planned from its own first-pass size, often all skips against a finer
            // reference, it lands finer than that reference once the reference gets coarser, and codes the
            // reference's quantisation error.
            let mut anchor = None;
            stats
                .frames
                .iter()
                .zip(&c)
                .map(|(f, &ci)| {
                    let q = match (f.slice_type, anchor) {
                        (SliceType::B, Some(a)) => a * type_factor(SliceType::B),
                        // max/min rather than clamp: statistics read from text may hold a NaN or negative qscale
                        (t, _) => (rf * ci.powf(1.0 - QCOMP) * type_factor(t)).max(f.qscale / range).min(f.qscale * range),
                    };
                    if f.slice_type != SliceType::B {
                        anchor = Some(q / type_factor(f.slice_type));
                    }
                    q.clamp(qp2qscale(0.0), qp2qscale(51.0))
                })
                .collect()
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
        // `remaining_bits[i]` is the inclusive suffix for frame `i`; retain an
        // empty suffix so callers can index it without a special case.
        let mut remaining_bits = vec![0.0; planned_bits.len() + 1];
        for i in (0..planned_bits.len()).rev() {
            remaining_bits[i] = remaining_bits[i + 1] + planned_bits[i];
        }
        // the finest qscale each frame may get
        let finest = stats.frames.iter().map(|f| f.qscale / range).collect();
        // the range can keep the plan from reaching the target, so the second pass aims at the target itself
        RcKind::TwoPass { kbps, plan, planned_bits, target_total: target, remaining_bits, finest }
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
                .clamp(self.qp_min, 51.0);
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
                // A quarter second of bits: with a longer horizon the correction is so weak that a short clip
                // that starts over target never pays the excess back (+10..14% on 2 s of high motion at 1 s).
                // Never shorter than one frame's budget: at a low frame rate the horizon would be a fraction of a
                // frame and the correction would overshoot in the other direction every frame.
                let abr_buffer = (bpf * self.fps.max(1.0) * 0.25).max(bpf);
                let overflow = (1.0 + (self.total_bits - bpf * self.frames as f64) / abr_buffer).clamp(0.5, 2.0);
                rf *= overflow;
                let q = rf * cplx.powf(1.0 - QCOMP) * type_factor(t);
                qscale2qp(q)
            }
            RcKind::TwoPass { kbps, plan, planned_bits, target_total, remaining_bits, finest } => {
                // Closed-loop second pass: `m` measures how far real frame sizes deviate from the first-pass model
                // (bits ∝ 1/qscale) at the qscales actually used; the remaining plan is rescaled so its predicted
                // size fits the remaining budget.
                let i = index_in_coding_order.min(plan.len().saturating_sub(1));
                let bpf = *kbps as f64 * 1000.0 / self.fps;
                let target_total = *target_total;
                let smooth = 0.5 * bpf * self.fps.max(1.0);
                let m = ((self.total_bits + smooth) / (self.model_done + smooth)).clamp(0.3, 3.0);
                let inflight: f64 = self.inflight.iter().sum();
                let remaining_plan = remaining_bits[i];
                let remaining_budget = (target_total - self.total_bits - m * inflight).max(0.05 * remaining_plan.max(1.0));
                let mut f = (m * remaining_plan / remaining_budget).clamp(0.25, 4.0);
                if self.last_correction > 0.0 {
                    f = f.max(self.last_correction / 2f64.powf(CORRECTION_QP_FALL / 6.0));
                }
                self.last_correction = f;
                let q = (plan[i] * f).max(finest.get(i).copied().unwrap_or(0.0));
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

        let RcKind::TwoPass { planned_bits, target_total, remaining_bits, .. } = RateControl::plan_two_pass(&stats, 1_000) else {
            panic!("two-pass planning must return a two-pass rate-control plan");
        };

        assert_eq!(remaining_bits.len(), planned_bits.len() + 1);
        // 1000 kbit/s over three frames at 24 fps
        assert!((target_total - 125_000.0).abs() < 1e-6);
        for i in 0..planned_bits.len() {
            assert!((remaining_bits[i] - planned_bits[i..].iter().sum::<f64>()).abs() < f64::EPSILON);
        }
        assert_eq!(remaining_bits[planned_bits.len()], 0.0);
    }

    /// First-pass statistics of `n` P frames coded at QP 30 with 10 kbit each, at 24 fps (240 kbit/s).
    fn flat_stats(n: usize) -> PassStats {
        PassStats { fps: 24.0, frames: vec![FrameStat { slice_type: SliceType::P, qscale: qp2qscale(30.0), bits: 10_000, cplx: 100.0 }; n] }
    }

    #[test]
    fn second_pass_keeps_frames_near_their_first_pass_quantiser() {
        // ten times the first pass's rate, and frames that cost what the first pass measured: without a limit the
        // plan and the loop take every frame down to about QP 10, where the size model no longer holds
        let mut rc = RateControl::new(RateControl::plan_two_pass(&flat_stats(48), 2_400), 24.0, None, None);
        for i in 0..48 {
            let qp = rc.frame_qp(SliceType::P, 100.0, i);
            assert!(qp >= 30.0 - PASS1_QP_RANGE - 1e-9, "frame {i}: QP {qp:.2}, first pass 30");
            rc.update(SliceType::P, qp, 100.0, 10_000);
        }
    }

    #[test]
    fn second_pass_correction_falls_by_at_most_two_qp_a_frame() {
        // the first half costs three times its prediction and the rest almost nothing: the surplus that turns up
        // late is shared by fewer and fewer frames, and the correction drops by several QP at once
        let mut rc = RateControl::new(RateControl::plan_two_pass(&flat_stats(24), 240), 24.0, None, None);
        let mut last: Option<f64> = None;
        for i in 0..24 {
            let qp = rc.frame_qp(SliceType::P, 100.0, i);
            if let Some(prev) = last {
                assert!(qp >= prev - CORRECTION_QP_FALL - 1e-9, "frame {i}: QP {prev:.2} -> {qp:.2}");
            }
            last = Some(qp);
            let predicted = 10_000.0 * qp2qscale(30.0) / qp2qscale(qp);
            rc.update(SliceType::P, qp, 100.0, (predicted * if i < 12 { 3.0 } else { 0.02 }) as u64);
        }
    }

    #[test]
    fn two_pass_plan_codes_b_frames_coarser_than_their_anchor() {
        // B frames the first pass coded almost all as skips, 10 QP above their P frames: planned from their own size
        // they would land about 5 QP finer than the P frames
        let p = FrameStat { slice_type: SliceType::P, qscale: qp2qscale(30.0), bits: 50_000, cplx: 100.0 };
        let b = FrameStat { slice_type: SliceType::B, qscale: qp2qscale(40.0), bits: 2_000, cplx: 100.0 };
        let mut frames = vec![FrameStat { slice_type: SliceType::I, ..p }];
        for _ in 0..8 {
            frames.extend([p, b, b]);
        }
        let stats = PassStats { fps: 24.0, frames };
        let RcKind::TwoPass { plan, .. } = RateControl::plan_two_pass(&stats, 1_000) else {
            panic!("two-pass planning must return a two-pass rate-control plan");
        };
        for i in (0..stats.frames.len()).filter(|&i| stats.frames[i].slice_type == SliceType::B) {
            let anchor = (0..i).rev().find(|&k| stats.frames[k].slice_type != SliceType::B).unwrap();
            assert!(plan[i] >= plan[anchor], "frame {i}: QP {:.2} under its anchor's {:.2}", qscale2qp(plan[i]), qscale2qp(plan[anchor]));
        }
    }
}
