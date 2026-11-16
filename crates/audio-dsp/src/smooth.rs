//! Per-sample parameter smoothing (linear ramps) to avoid zipper noise.

/// A value that ramps linearly to its target over a fixed number of samples.
#[derive(Clone, Copy, Debug)]
pub struct Smoothed {
    current: f64,
    target: f64,
    step: f64,
    remaining: u32,
    ramp: u32,
}

impl Smoothed {
    /// `ramp_samples` = length of a full transition (0 = no smoothing).
    pub fn new(value: f32, ramp_samples: u32) -> Self {
        Smoothed { current: value as f64, target: value as f64, step: 0.0, remaining: 0, ramp: ramp_samples }
    }
    /// Convenience: ramp length from a time in milliseconds.
    pub fn with_ms(value: f32, sample_rate: f32, ms: f32) -> Self {
        Self::new(value, (sample_rate * ms / 1000.0).round().max(0.0) as u32)
    }
    /// Start ramping towards `target`.
    pub fn set(&mut self, target: f32) {
        let t = target as f64;
        if t == self.target {
            return;
        }
        self.target = t;
        if self.ramp == 0 {
            self.current = t;
            self.remaining = 0;
        } else {
            self.remaining = self.ramp;
            self.step = (t - self.current) / self.ramp as f64;
        }
    }
    /// Jump straight to the target.
    pub fn snap(&mut self) {
        self.current = self.target;
        self.remaining = 0;
    }
    /// Advance one sample and return the new value.
    #[inline]
    pub fn tick(&mut self) -> f32 {
        if self.remaining > 0 {
            self.remaining -= 1;
            if self.remaining == 0 {
                self.current = self.target;
            } else {
                self.current += self.step;
            }
        }
        self.current as f32
    }
    /// Current value without advancing.
    #[inline]
    pub fn value(&self) -> f32 {
        self.current as f32
    }
    pub fn target(&self) -> f32 {
        self.target as f32
    }
    pub fn is_smoothing(&self) -> bool {
        self.remaining > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ramps_linearly_and_lands_exactly() {
        let mut s = Smoothed::new(0.0, 4);
        s.set(1.0);
        let v: Vec<f32> = (0..6).map(|_| s.tick()).collect();
        assert_eq!(v, vec![0.25, 0.5, 0.75, 1.0, 1.0, 1.0]);
        assert!(!s.is_smoothing());
    }

    #[test]
    fn retarget_mid_ramp_is_continuous() {
        let mut s = Smoothed::new(0.0, 10);
        s.set(1.0);
        for _ in 0..5 {
            s.tick();
        }
        let mid = s.value();
        s.set(0.0);
        let n = s.tick();
        assert!((n - mid).abs() <= 0.1 + 1e-6);
    }

    #[test]
    fn zero_ramp_is_immediate() {
        let mut s = Smoothed::new(0.0, 0);
        s.set(3.0);
        assert_eq!(s.value(), 3.0);
    }
}
