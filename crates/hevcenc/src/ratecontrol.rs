//! Frame-level rate control: constant QP, constant quality and 1-pass ABR with VBV.
//!
//! Design mirrors `h264enc::ratecontrol` adapted to HEVC I/P only (no B-frames, no second-pass planning here).
//! `Qp` returns the fixed QP (`I` uses `QP - 2` saturating to 0); `Crf` rounds the float to a P-frame base with the same `-2`
//! I offset plus a bounded complexity nudge; `Cbr`/`Vbr` run 1-pass ABR with a 1 s VBV (`Cbr` sized at `kbps`, `Vbr` at
//! `max_kbps`, falling back to `target_kbps` when `max_kbps` is 0). Non-positive/non-finite `fps` falls back to 30.0 so
//! per-frame targets never divide by zero; zero complexities floor to a score of 1 so ratios/logs never see 0.
//! `Pass::Second` stays rejected in `Encoder::new`; this file only collects `Pass::First` stats for a follow-up.

use crate::RateControl;
use crate::lookahead::Complexity;

/// Per-picture statistics recorded by a [`Pass::First`](crate::Pass::First) run.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FrameStat {
    /// 0 = I/IDR, 1 = P.
    pub slice_type: u8,
    pub qscale: f32,
    pub bits: u64,
}

/// First-pass statistics, serialised between runs for two-pass VBR (follow-up).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PassStats {
    pub frames: Vec<FrameStat>,
}

impl PassStats {
    /// Serialise to the text format documented on [`PassStats::from_text`].
    pub fn to_text(&self) -> String {
        // Header first so a truncated file still fails the version check on read.
        let mut s = String::from("hevcenc-pass1 v1\n");
        for f in &self.frames {
            // Only 0/1 are emitted by phase 1; any other code encodes as P so output stays parseable.
            let t = if f.slice_type == 0 { 'I' } else { 'P' };
            s.push_str(&format!("{t} {} {}\n", f.qscale, f.bits));
        }
        s
    }

    /// Parse [`PassStats::to_text`] output; returns `None` on any garbage (mirrors `h264enc`, which returns `Option`).
    ///
    /// Exact format (UTF-8, `\n`-separated):
    /// ```text
    /// hevcenc-pass1 v1
    /// I <qscale: f32, finite> <bits: u64>
    /// P <qscale: f32, finite> <bits: u64>
    /// ...
    /// ```
    /// Header must be exactly `hevcenc-pass1 v1` (surrounding whitespace ignored); each body line must start with
    /// `I` (= 0) or `P` (= 1) followed by a finite `f32` qscale and a `u64` bit count. Extra tokens after `bits`
    /// are ignored (mirrors `h264enc`); blank lines, unknown letters/codes, non-numeric or non-finite fields, and a
    /// wrong header all return `None`. Empty stats serialise to the header alone.
    pub fn from_text(text: &str) -> Option<PassStats> {
        let mut lines = text.lines();
        let head = lines.next()?;
        // Strict version stamp; trim tolerates a trailing newline style only.
        if head.trim() != "hevcenc-pass1 v1" {
            return None;
        }
        let mut frames = Vec::new();
        for line in lines {
            let mut it = line.split_whitespace();
            let slice_type = match it.next()? {
                "I" => 0,
                "P" => 1,
                _ => return None,
            };
            let qscale: f32 = it.next()?.parse().ok()?;
            // NaN/infinite qscales would poison the bit model, so reject them as garbage.
            if !qscale.is_finite() {
                return None;
            }
            let bits: u64 = it.next()?.parse().ok()?;
            frames.push(FrameStat { slice_type, qscale, bits });
        }
        Some(PassStats { frames })
    }
}

/// Closed-loop frame controller; call `frame_qp` before coding, then `update` with the actual bits.
pub struct Rc {
    mode: RateControl,     // Requested mode; targets derive from it plus `fps` each use.
    fps: f64,              // Effective fps after the 30.0 fallback; keeps per-frame math finite.
    error: f64,            // Cumulative (actual − target) bits, clamped to ±8 frames to stop windup.
    vbv_size: f64,         // 1 s window in bits (0 for Qp/Crf, which skip VBV).
    vbv_fill: f64,         // Buffered bits; drains on `update`, refills at target per frame.
    mean: f64,             // Running mean of frame scores for the complexity nudge.
    mean_n: u64,           // Frames folded into `mean` so far.
    pending: Option<f64>,  // Score from `frame_qp` awaiting commit in `update`.
    collect: bool,         // True only via `new_first_pass`; gates first-pass collection.
    pass1: Vec<FrameStat>, // Collected stats; read via `first_pass_stats`.
}

// Effective fps; non-positive/non-finite input falls back to 30.0 to avoid divide-by-zero.
fn effective_fps(fps: f64) -> f64 {
    if fps.is_finite() && fps > 0.0 { fps } else { 30.0 }
}

// Exact CRF mapping: base P-frame QP is round(crf) clamped 0..=51; non-finite falls back to 26.
fn crf_base_qp(crf: f32) -> u8 {
    if !crf.is_finite() {
        return 26;
    }
    crf.round().clamp(0.0, 51.0) as u8
}

// Scalar cost of a frame; saturating sum covers spatial + temporal, floored at 1 for flat frames.
fn frame_score(c: &Complexity) -> f64 {
    c.intra.saturating_add(c.inter).max(1) as f64
}

// Bounded nudge from the score ratio vs the running mean: 2 * log2(score/mean) clamped to ±2; 0 with no history.
fn complexity_nudge(score: f64, mean: f64, n: u64) -> f64 {
    if n == 0 || !mean.is_finite() || mean <= 0.0 || !score.is_finite() || score <= 0.0 {
        return 0.0;
    }
    (score / mean).log2().clamp(-1.0, 1.0) * 2.0
}

// Average-bit target per frame; Qp/Crf have none (0.0) and callers gate on the mode first.
fn abr_target_bpf(mode: &RateControl, fps: f64) -> f64 {
    let kbps = match *mode {
        RateControl::Cbr { kbps } => kbps.max(1) as f64,
        RateControl::Vbr { target_kbps, .. } => target_kbps.max(1) as f64,
        RateControl::Crf(_) | RateControl::Qp(_) => return 0.0,
    };
    kbps * 1000.0 / fps
}

// VBV capacity: 1 s at kbps (Cbr) or max_kbps (Vbr); Vbr with max 0 behaves like Cbr at target.
fn vbv_capacity_bits(mode: &RateControl) -> f64 {
    match *mode {
        RateControl::Cbr { kbps } => kbps.max(1) as f64 * 1000.0,
        RateControl::Vbr { target_kbps, max_kbps } => {
            if max_kbps == 0 {
                target_kbps.max(1) as f64 * 1000.0
            } else {
                max_kbps.max(1) as f64 * 1000.0
            }
        }
        RateControl::Crf(_) | RateControl::Qp(_) => 0.0,
    }
}

impl Rc {
    /// Build for single-pass encoding; `_total_frames` is reserved for second-pass planning and currently unused.
    pub fn new(rate: RateControl, fps: f64, _total_frames: Option<u64>) -> Self {
        let eff = effective_fps(fps);
        let size = vbv_capacity_bits(&rate);
        Self { mode: rate, fps: eff, error: 0.0, vbv_size: size, vbv_fill: size * 0.9, mean: 0.0, mean_n: 0, pending: None, collect: false, pass1: Vec::new() }
    }

    /// Build for a first-pass run; only this constructor makes `first_pass_stats` return `Some`.
    pub fn new_first_pass(rate: RateControl, fps: f64, _total_frames: Option<u64>) -> Self {
        let eff = effective_fps(fps);
        let size = vbv_capacity_bits(&rate);
        Self { mode: rate, fps: eff, error: 0.0, vbv_size: size, vbv_fill: size * 0.9, mean: 0.0, mean_n: 0, pending: None, collect: true, pass1: Vec::new() }
    }

    /// Choose the QP for the next frame; I/IDR frames use QP − 2 (saturating). Always 0..=51, never NaN.
    ///
    /// `Qp` returns the fixed QP; `Crf` returns `round(crf)` plus a ±2 complexity nudge; `Cbr`/`Vbr` add the ABR
    /// error term (gain 1.5 QP per frame-worth of error, clamped to ±6), the same ±2 complexity nudge, and a VBV
    /// term (up to +8 QP as the buffer drains from half to empty) pushing QP up when overshoot fills the buffer.
    pub fn frame_qp(&mut self, is_idr: bool, c: &Complexity) -> u8 {
        let score = frame_score(c);
        // Commit happens in `update` so the mean only tracks actually coded frames.
        self.pending = Some(score);
        let idr_offset = if is_idr { -2.0 } else { 0.0 };
        match self.mode {
            // Fixed QP; I frames get 2 finer QPs for the same reason intra costs more bits.
            RateControl::Qp(q) => {
                let base = q.min(51);
                if is_idr { base.saturating_sub(2) } else { base }
            }
            // Constant quality: base plus bounded content adaptivity, I offset as above.
            RateControl::Crf(crf) => {
                let base = crf_base_qp(crf) as f64;
                let qp = base + complexity_nudge(score, self.mean, self.mean_n) + idr_offset;
                if !qp.is_finite() {
                    return 26;
                }
                qp.round().clamp(0.0, 51.0) as u8
            }
            // 1-pass ABR: fixed 26 anchor corrected by rate error, complexity and VBV fullness.
            RateControl::Cbr { .. } | RateControl::Vbr { .. } => {
                let target = abr_target_bpf(&self.mode, self.fps);
                if !target.is_finite() || target <= 0.0 {
                    return 26;
                }
                let err_qp = (self.error / target).clamp(-4.0, 4.0) * 1.5;
                let cplx_qp = complexity_nudge(score, self.mean, self.mean_n);
                // Low buffer means sustained overshoot; raise QP proportionally to the shortfall.
                let vbv_qp = if self.vbv_size > 0.0 {
                    let full = (self.vbv_fill / self.vbv_size).clamp(0.0, 1.0);
                    if full < 0.5 { (0.5 - full) * 16.0 } else { 0.0 }
                } else {
                    0.0
                };
                let qp = 26.0 + err_qp + cplx_qp + vbv_qp + idr_offset;
                if !qp.is_finite() {
                    return 26;
                }
                qp.round().clamp(0.0, 51.0) as u8
            }
        }
    }

    /// Feed back actual frame bits; updates the ABR error, the VBV level and the complexity mean.
    pub fn update(&mut self, bits: u64, _is_idr: bool) {
        // Phase 1 shares one buffer/mean across types; the flag stays for call-site symmetry.
        let b = bits as f64; // u64 → f64 is always finite (may round, never NaN).
        match self.mode {
            RateControl::Cbr { .. } | RateControl::Vbr { .. } => {
                let target = abr_target_bpf(&self.mode, self.fps);
                if target.is_finite() && target > 0.0 {
                    // Clamp the integrator to ±8 frames so one scene cut cannot wedge QP.
                    let limit = 8.0 * target;
                    self.error = (self.error + b - target).clamp(-limit, limit);
                    // Classic VBV: drain the coded bits, refill one frame worth, clamp to the window.
                    self.vbv_fill = (self.vbv_fill - b + target).clamp(0.0, self.vbv_size);
                }
            }
            RateControl::Crf(_) | RateControl::Qp(_) => {}
        }
        // Fold the pending score into a cumulative mean; bounded magnitude, no overflow path.
        if let Some(s) = self.pending.take()
            && s.is_finite()
            && s > 0.0
        {
            let n = self.mean_n as f64;
            self.mean = (self.mean * n + s) / (n + 1.0);
            self.mean_n = self.mean_n.saturating_add(1);
        }
    }

    /// Collect one first-pass stat; no-op unless built with `new_first_pass` to keep single-pass memory flat.
    pub fn record_first_pass(&mut self, stat: FrameStat) {
        if self.collect {
            self.pass1.push(stat);
        }
    }

    /// Collected first-pass stats; `Some` only when built with `new_first_pass`.
    pub fn first_pass_stats(&self) -> Option<PassStats> {
        if self.collect { Some(PassStats { frames: self.pass1.clone() }) } else { None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RateControl;

    fn cx(intra: u64, inter: u64) -> Complexity {
        Complexity { intra, inter }
    }

    #[test]
    fn qp_fixed_with_idr_minus_two() {
        let c = cx(100, 100);
        let mut rc = Rc::new(RateControl::Qp(28), 30.0, None);
        assert_eq!(rc.frame_qp(false, &c), 28);
        assert_eq!(rc.frame_qp(true, &c), 26);
        // Saturating floor keeps tiny QPs in range.
        let mut tiny = Rc::new(RateControl::Qp(1), 30.0, None);
        assert_eq!(tiny.frame_qp(false, &c), 1);
        assert_eq!(tiny.frame_qp(true, &c), 0);
        let mut zero = Rc::new(RateControl::Qp(0), 30.0, None);
        assert_eq!(zero.frame_qp(true, &c), 0);
    }

    #[test]
    fn crf_rounds_with_idr_minus_two() {
        let c = cx(1000, 1000);
        // First frame has no mean history, so the mapping is exact here.
        for (crf, base) in [(18.0f32, 18u8), (23.0, 23), (30.7, 31)] {
            let mut rc = Rc::new(RateControl::Crf(crf), 30.0, None);
            assert_eq!(rc.frame_qp(false, &c), base, "crf {crf}");
            assert_eq!(rc.frame_qp(true, &c), base.saturating_sub(2), "crf {crf} idr");
        }
    }

    #[test]
    fn cbr_converges_toward_target() {
        // Tolerance: mean of the last 120 of 240 frames within 20% of target; the fake encoder doubles bits
        // every −6 QP around 26 but runs 2× hot, so the loop must lift QP (~32) to hold the average.
        let fps = 30.0;
        let target = 1000.0 * 1000.0 / fps;
        let mut rc = Rc::new(RateControl::Cbr { kbps: 1000 }, fps, None);
        let c = cx(5000, 5000);
        let fake_bits = |qp: u8| (2.0 * target * 2f64.powf((26.0 - f64::from(qp)) / 6.0)).round().max(1.0) as u64;
        let mut tail = 0.0f64;
        for i in 0..240 {
            let qp = rc.frame_qp(false, &c);
            let bits = fake_bits(qp);
            rc.update(bits, false);
            if i >= 120 {
                tail += bits as f64;
            }
        }
        let avg = tail / 120.0;
        assert!((avg - target).abs() / target < 0.20, "avg {avg} vs target {target}");
    }

    #[test]
    fn vbv_pushes_qp_up_on_repeated_overshoot() {
        let mut rc = Rc::new(RateControl::Cbr { kbps: 500 }, 30.0, None);
        let c = cx(2000, 2000);
        let first = rc.frame_qp(false, &c);
        // Sustain 4×-target frames; error integrator plus draining VBV must lift QP well above start.
        let huge = (4.0 * 500.0 * 1000.0 / 30.0) as u64;
        for _ in 0..30 {
            let _ = rc.frame_qp(false, &c);
            rc.update(huge, false);
        }
        let later = rc.frame_qp(false, &c);
        assert!(later > first.saturating_add(4), "first {first} later {later}");
    }

    #[test]
    fn pass_stats_text_round_trip_and_rejects_garbage() {
        let stats = PassStats {
            frames: vec![
                FrameStat { slice_type: 0, qscale: 12.5, bits: 1234 },
                FrameStat { slice_type: 1, qscale: 0.85, bits: 56789 },
                FrameStat { slice_type: 1, qscale: 51.0, bits: 0 },
            ],
        };
        let text = stats.to_text();
        assert!(text.starts_with("hevcenc-pass1 v1\n"));
        assert_eq!(PassStats::from_text(&text), Some(stats));
        // Header alone is valid empty stats.
        assert_eq!(PassStats::from_text("hevcenc-pass1 v1\n"), Some(PassStats { frames: Vec::new() }));
        // Garbage of every shape is None (mirrors h264enc's Option choice).
        for bad in [
            "",
            "garbage\n",
            "h264enc-pass1 fps=30\nI 1.0 2 3\n",
            "hevcenc-pass1 v2\nI 1.0 2\n",
            "hevcenc-pass1 v2\nI 1.0 2\n",
            "hevcenc-pass1 v1\nX 1.0 2\n",
            "hevcenc-pass1 v1\nI nan 2\n",
            "hevcenc-pass1 v1\nI inf 2\n",
            "hevcenc-pass1 v1\nI 1.0\n",
            "hevcenc-pass1 v1\n\n",
            "hevcenc-pass1 v1\nI 1.0 abc\n",
        ] {
            assert_eq!(PassStats::from_text(bad), None, "input {bad:?}");
        }
    }

    #[test]
    fn bad_fps_falls_back_without_nan() {
        // fps ≤ 0 or non-finite must behave like 30 fps; zero complexities must not divide by zero.
        for fps in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut rc = Rc::new(RateControl::Cbr { kbps: 800 }, fps, None);
            let flat = cx(0, 0);
            for _ in 0..5 {
                let qp = rc.frame_qp(false, &flat);
                assert!(qp <= 51);
                rc.update(1000, false);
                let qi = rc.frame_qp(true, &flat);
                assert!(qi <= 51);
                rc.update(1000, true);
            }
            let mut crf = Rc::new(RateControl::Crf(23.0), fps, None);
            assert!(crf.frame_qp(true, &flat) <= 51);
            assert!(crf.frame_qp(false, &cx(0, 0)) <= 51);
        }
    }

    #[test]
    fn first_pass_stats_gated_on_constructor() {
        let stat = FrameStat { slice_type: 0, qscale: 10.0, bits: 999 };
        let mut single = Rc::new(RateControl::Cbr { kbps: 500 }, 30.0, None);
        single.record_first_pass(stat.clone());
        assert_eq!(single.first_pass_stats(), None);
        let mut first = Rc::new_first_pass(RateControl::Cbr { kbps: 500 }, 30.0, None);
        assert_eq!(first.first_pass_stats(), Some(PassStats { frames: Vec::new() }));
        first.record_first_pass(stat.clone());
        assert_eq!(first.first_pass_stats(), Some(PassStats { frames: vec![stat] }));
    }
}
