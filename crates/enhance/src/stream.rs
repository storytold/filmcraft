//! Runs a frame-based model inside a block-based audio effect.
//!
//! The host hands over blocks of any length at the sequence rate; a model takes fixed frames
//! ("hops") at its own rate and answers with a fixed delay. [`Streamer`] converts the rate when
//! the two differ, collects hops, and delays the result by a constant, reported latency so that
//! every block gets exactly as many samples back as it brought. The dry signal is delayed by the
//! same amount, so the wet/dry mix stays phase-aligned.

use std::collections::VecDeque;

use crate::resample::Sinc;

/// A model that enhances one hop at a time.
pub(crate) trait FrameModel: Send {
    /// The rate the model works at, Hz.
    fn sample_rate(&self) -> u32;
    /// Samples per frame and channel.
    fn hop(&self) -> usize;
    /// Output sample `n` of the model's stream belongs to input sample `n - delay()`.
    fn delay(&self) -> usize;
    /// Noise attenuation limit in dB (100 = unlimited) and the post-filter switch.
    fn configure(&mut self, atten_db: f32, post_filter: bool);
    /// Enhance one frame in place: `frame[c]` holds `hop()` samples of channel `c`.
    fn process(&mut self, frame: &mut [Vec<f32>]) -> Result<(), String>;
    /// Forget all signal history.
    fn reset(&mut self);
}

/// Sequence rates the effect accepts; outside it passes audio through.
pub(crate) const MIN_RATE: u32 = 8_000;
pub(crate) const MAX_RATE: u32 = 384_000;
/// Most channels one instance processes.
pub(crate) const MAX_CHANNELS: usize = 32;

pub(crate) struct Streamer {
    model: Box<dyn FrameModel>,
    hop: usize,
    failed: bool,
    up: Vec<Sinc>,
    down: Vec<Sinc>,
    /// Model-rate input waiting for a full hop.
    pending: Vec<Vec<f32>>,
    frame: Vec<Vec<f32>>,
    wet: Vec<VecDeque<f32>>,
    dry: Vec<VecDeque<f32>>,
    /// Zeros put in front of `wet` so a block never runs dry.
    slack: usize,
    latency: usize,
    mix: f32,
}

impl Streamer {
    /// `None` when the rates or the model's shape cannot be served.
    pub(crate) fn new(model: Box<dyn FrameModel>, host_rate: u32, channels: usize) -> Option<Self> {
        let (rate, hop, delay) = (model.sample_rate(), model.hop(), model.delay());
        if !(MIN_RATE..=MAX_RATE).contains(&host_rate)
            || !(MIN_RATE..=MAX_RATE).contains(&rate)
            || hop == 0
            || hop > 1 << 16
            || delay > 1 << 20
            || channels == 0
            || channels > MAX_CHANNELS
        {
            return None;
        }
        let (mut up, mut down) = (Vec::new(), Vec::new());
        let (slack, latency);
        if host_rate == rate {
            slack = hop;
            latency = delay + hop;
        } else {
            let (h, r) = (f64::from(host_rate), f64::from(rate));
            let u = Sinc::new(host_rate, rate, 0.0);
            let d0 = Sinc::new(rate, host_rate, 0.0);
            // the model's delay and the down filter's, in host samples, rounded up to a whole
            // sample by delaying the down filter a little more
            let model_side = (delay + d0.half()) as f64;
            let whole = (model_side * h / r).ceil();
            let extra = whole * r / h - model_side;
            slack = (hop as f64 * h / r).ceil() as usize + 8;
            latency = u.half() + whole as usize + slack;
            up = vec![u; channels];
            down = vec![Sinc::new(rate, host_rate, extra); channels];
        }
        let mut s = Streamer {
            model,
            hop,
            failed: false,
            up,
            down,
            pending: vec![Vec::with_capacity(hop * 4); channels],
            frame: vec![Vec::with_capacity(hop); channels],
            wet: vec![VecDeque::new(); channels],
            dry: vec![VecDeque::new(); channels],
            slack,
            latency,
            mix: 1.0,
        };
        s.prime();
        Some(s)
    }

    fn prime(&mut self) {
        for c in 0..self.wet.len() {
            self.pending[c].clear();
            self.wet[c].clear();
            self.wet[c].extend(std::iter::repeat_n(0.0, self.slack));
            self.dry[c].clear();
            self.dry[c].extend(std::iter::repeat_n(0.0, self.latency));
        }
        self.up.iter_mut().chain(self.down.iter_mut()).for_each(Sinc::reset);
    }

    pub(crate) fn latency(&self) -> usize {
        self.latency
    }

    /// The model stopped with an error; the effect now passes the (delayed) dry signal.
    pub(crate) fn failed(&self) -> bool {
        self.failed
    }

    pub(crate) fn reset(&mut self) {
        self.model.reset();
        self.prime();
    }

    pub(crate) fn configure(&mut self, atten_db: f32, post_filter: bool) {
        self.model.configure(atten_db, post_filter);
    }

    /// Process one block in place. `mix` is the wet share, 0…1, reached linearly over the block.
    pub(crate) fn process(&mut self, channels: &mut [&mut [f32]], mix: f32) {
        let n = channels.iter().map(|c| c.len()).min().unwrap_or(0);
        if n == 0 {
            return;
        }
        let built = self.wet.len();
        for c in 0..built {
            match channels.get(c) {
                Some(x) => {
                    self.dry[c].extend(x[..n].iter().copied());
                    match self.up.get_mut(c) {
                        Some(u) => u.push(&x[..n], &mut self.pending[c]),
                        None => self.pending[c].extend_from_slice(&x[..n]),
                    }
                }
                // fewer channels than the effect was built for: the missing ones are silent
                None => {
                    self.dry[c].extend(std::iter::repeat_n(0.0, n));
                    let have = self.pending[c].len();
                    let want = self.pending.first().map_or(have, Vec::len);
                    self.pending[c].resize(want.max(have), 0.0);
                }
            }
        }
        self.run_frames();
        let target = if mix.is_finite() { mix.clamp(0.0, 1.0) } else { 1.0 };
        let step = (target - self.mix) / n as f32;
        for (c, x) in channels.iter_mut().enumerate().take(built) {
            let mut m = self.mix;
            for v in x[..n].iter_mut() {
                m += step;
                let d = self.dry[c].pop_front().unwrap_or(0.0);
                let w = self.wet[c].pop_front().unwrap_or(d);
                *v = if self.failed { d } else { d + (w - d) * m };
            }
        }
        self.mix = target;
    }

    fn run_frames(&mut self) {
        let hop = self.hop;
        while self.pending.iter().all(|p| p.len() >= hop) {
            for (f, p) in self.frame.iter_mut().zip(self.pending.iter_mut()) {
                f.clear();
                f.extend(p.drain(..hop));
            }
            if !self.failed
                && let Err(e) = self.model.process(&mut self.frame)
            {
                log::warn!("neural speech enhancement stopped: {e}");
                self.failed = true;
            }
            for (c, f) in self.frame.iter_mut().enumerate() {
                // a model that returned the wrong amount would shift the stream
                f.resize(hop, 0.0);
                match self.down.get_mut(c) {
                    Some(d) => d.push(f, &mut self.wet[c]),
                    None => self.wet[c].extend(f.iter().copied()),
                }
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A stand-in model: delays by `delay` samples and scales by `gain` (a "noise reduction" that
    /// is easy to measure), at `rate` with frames of `hop`.
    pub(crate) struct Mock {
        pub rate: u32,
        pub hop: usize,
        pub delay: usize,
        pub gain: f32,
        pub line: Vec<VecDeque<f32>>,
        pub fail_after: Option<usize>,
        pub frames: usize,
    }

    impl Mock {
        pub(crate) fn new(rate: u32, hop: usize, delay: usize) -> Self {
            Mock { rate, hop, delay, gain: 1.0, line: Vec::new(), fail_after: None, frames: 0 }
        }
    }

    impl FrameModel for Mock {
        fn sample_rate(&self) -> u32 {
            self.rate
        }
        fn hop(&self) -> usize {
            self.hop
        }
        fn delay(&self) -> usize {
            self.delay
        }
        fn configure(&mut self, atten_db: f32, _: bool) {
            self.gain = 10f32.powf(-atten_db / 20.0);
        }
        fn process(&mut self, frame: &mut [Vec<f32>]) -> Result<(), String> {
            self.frames += 1;
            if self.fail_after.is_some_and(|n| self.frames > n) {
                return Err("mock failure".into());
            }
            self.line.resize(frame.len(), std::iter::repeat_n(0.0, self.delay).collect());
            for (f, l) in frame.iter_mut().zip(self.line.iter_mut()) {
                assert_eq!(f.len(), self.hop);
                for v in f.iter_mut() {
                    l.push_back(*v);
                    *v = l.pop_front().unwrap_or(0.0) * self.gain;
                }
            }
            Ok(())
        }
        fn reset(&mut self) {
            self.line.clear();
        }
    }

    fn tone(rate: u32, n: usize, phase: f64) -> Vec<f32> {
        (0..n).map(|i| (0.6 * (std::f64::consts::TAU * 700.0 * i as f64 / f64::from(rate) + phase).sin()) as f32).collect()
    }

    /// Run `x` (stereo: the second channel is inverted) through in uneven blocks.
    fn run(s: &mut Streamer, x: &[f32], mix: f32) -> (Vec<f32>, Vec<f32>) {
        let (mut l, mut r): (Vec<f32>, Vec<f32>) = (x.to_vec(), x.iter().map(|v| -v).collect());
        let sizes = [256usize, 1, 480, 37, 1024, 3, 255];
        let (mut i, mut k) = (0, 0);
        while i < x.len() {
            let e = (i + sizes[k % sizes.len()]).min(x.len());
            s.process(&mut [&mut l[i..e], &mut r[i..e]], mix);
            i = e;
            k += 1;
        }
        (l, r)
    }

    #[test]
    fn at_the_model_rate_the_output_is_the_input_delayed_by_exactly_the_latency() {
        let mut s = Streamer::new(Box::new(Mock::new(48_000, 480, 1440)), 48_000, 2).unwrap();
        assert_eq!(s.latency(), 1920);
        let x = tone(48_000, 48_000, 0.3);
        let (l, r) = run(&mut s, &x, 1.0);
        let lat = s.latency();
        assert!(l[..lat].iter().all(|v| *v == 0.0));
        for i in lat..x.len() {
            assert_eq!(l[i], x[i - lat], "sample {i}");
            assert_eq!(r[i], -x[i - lat], "sample {i}");
        }
    }

    #[test]
    fn at_other_rates_wet_and_dry_stay_aligned_at_the_reported_latency() {
        for rate in [44_100u32, 96_000, 32_000, 22_050] {
            let mut s = Streamer::new(Box::new(Mock::new(48_000, 480, 1440)), rate, 2).unwrap();
            let lat = s.latency();
            let x = tone(rate, rate as usize, 0.0);
            let (wet, _) = run(&mut s, &x, 1.0);
            let mut worst = 0f32;
            for i in (lat + rate as usize / 5)..x.len() {
                worst = worst.max((wet[i] - x[i - lat]).abs());
            }
            assert!(worst < 4e-3, "{rate} Hz: wet is off by {worst}");
            // the dry path is exact
            s.reset();
            let (dry, _) = run(&mut s, &x, 0.0);
            // the mix ramps from 1 to 0 over the first block
            for i in (lat + 256)..x.len() {
                assert_eq!(dry[i], x[i - lat], "{rate} Hz sample {i}");
            }
        }
    }

    #[test]
    fn the_mix_blends_wet_and_dry() {
        let mut m = Mock::new(48_000, 480, 0);
        m.gain = 0.0;
        let mut s = Streamer::new(Box::new(m), 48_000, 2).unwrap();
        let x = vec![1.0f32; 9600];
        let (l, _) = run(&mut s, &x, 0.25);
        let lat = s.latency();
        // wet is silence, so 75 % dry remains once the ramp has settled
        assert!(l[lat + 512..].iter().all(|v| (v - 0.75).abs() < 1e-6));
    }

    #[test]
    fn a_failing_model_leaves_the_dry_signal() {
        let mut m = Mock::new(48_000, 480, 960);
        m.gain = 0.0;
        m.fail_after = Some(3);
        let mut s = Streamer::new(Box::new(m), 48_000, 2).unwrap();
        let x = tone(48_000, 24_000, 0.0);
        let (l, _) = run(&mut s, &x, 1.0);
        assert!(s.failed());
        let lat = s.latency();
        for i in 12_000..x.len() {
            assert_eq!(l[i], x[i - lat]);
        }
    }

    #[test]
    fn reset_starts_the_stream_afresh() {
        let mut s = Streamer::new(Box::new(Mock::new(48_000, 480, 1440)), 44_100, 2).unwrap();
        let x = tone(44_100, 10_000, 0.0);
        let first = run(&mut s, &x, 1.0);
        s.reset();
        assert_eq!(run(&mut s, &x, 1.0), first);
    }

    #[test]
    fn every_block_gets_its_length_back_whatever_the_channel_count() {
        let mut s = Streamer::new(Box::new(Mock::new(48_000, 480, 1440)), 44_100, 2).unwrap();
        for n in [0usize, 1, 5, 4096] {
            let mut mono = vec![0.25f32; n];
            s.process(&mut [&mut mono], 1.0);
            let (mut a, mut b, mut c) = (vec![0.25f32; n], vec![0.25f32; n / 2], vec![0.5f32; n]);
            s.process(&mut [&mut a, &mut b, &mut c], 0.5);
            assert!(mono.iter().chain(&a).chain(&b).all(|v| v.is_finite()));
            // channels beyond the two the effect was built for pass through untouched
            assert!(c.iter().all(|v| *v == 0.5));
        }
        s.process(&mut [], 1.0);
    }

    #[test]
    fn unusable_shapes_are_refused() {
        assert!(Streamer::new(Box::new(Mock::new(48_000, 480, 0)), 0, 2).is_none());
        assert!(Streamer::new(Box::new(Mock::new(48_000, 480, 0)), u32::MAX, 2).is_none());
        assert!(Streamer::new(Box::new(Mock::new(48_000, 0, 0)), 48_000, 2).is_none());
        assert!(Streamer::new(Box::new(Mock::new(0, 480, 0)), 48_000, 2).is_none());
        assert!(Streamer::new(Box::new(Mock::new(48_000, 480, 0)), 48_000, 0).is_none());
        assert!(Streamer::new(Box::new(Mock::new(48_000, 480, usize::MAX)), 48_000, 2).is_none());
    }
}
