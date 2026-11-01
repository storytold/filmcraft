//! Real-time audio DSP for FilmCraft (layer L1, dependency-free, `wasm32` clean).
//!
//! * [`loudness`] — ITU-R BS.1770-4 / EBU R128 loudness meter (momentary, short-term,
//!   integrated, loudness range, sample peak, true peak) and the loudness-normalisation helper.
//! * [`effects`] — clip/track audio effects behind the common [`AudioEffect`] trait, with a
//!   registry ([`effects()`]) describing every effect's parameters (range, unit, default) so the
//!   engine and UI can build controls generically.
//! * [`biquad`] — RBJ-cookbook biquads (TDF-II) with analytic magnitude response.
//! * [`sync`] — offset between two recordings of one event (GCC-PHAT, sample-accurate).
//! * [`resample`] — streaming, block-invariant rate conversion for playback on a device at another rate.
//! * [`channels`] — channel layouts, ITU-R BS.775 up/downmix, 5.1 mixdown types, the 5.1 panner.
//!
//! Conventions: audio is **planar f32** (`&mut [&mut [f32]]`, one slice per channel, all the same
//! length). `process` never allocates; all buffers are sized at construction. Parameters may be
//! changed between blocks and are smoothed per sample, so the output is independent of how the
//! stream is cut into blocks. Recursive state is flushed to zero when it decays below ~1e-30 so
//! denormals never appear (no FTZ/DAZ CPU flags needed, which keeps the crate `unsafe`-free and
//! portable to wasm).

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod biquad;
pub mod channels;
pub mod design;
pub mod ducking;
pub mod effects;
pub mod fft;
pub mod loudness;
pub mod oversample;
pub mod remix;
pub mod resample;
mod smooth;
pub mod sync;

pub use effects::{Category, EffectInfo, create_effect, effect_info, effects};
pub use loudness::{LoudnessMeter, normalize_gain_db};
pub use smooth::Smoothed;

/// Unit of a parameter value (for display and control construction).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Unit {
    /// Unitless scalar.
    None,
    Decibels,
    Hertz,
    Milliseconds,
    Seconds,
    Percent,
    /// Filter quality factor.
    Q,
    /// Compression ratio (`x:1`).
    Ratio,
    Semitones,
    Bpm,
    /// Pan / balance position, −100 (left) … +100 (right).
    Pan,
    /// Boolean toggle: 0 = off, 1 = on.
    Toggle,
    /// Index into [`ParamSpec::choices`].
    Choice,
}

/// Static description of one effect parameter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParamSpec {
    /// Stable id used by `set_param` / automation (e.g. `"threshold"`, `"b3.freq"`).
    pub id: &'static str,
    /// Human-readable label.
    pub name: &'static str,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub unit: Unit,
    /// Hint that the control should be logarithmic (frequencies, times).
    pub log_scale: bool,
    /// Option labels when `unit == Unit::Choice` (value is the index).
    pub choices: &'static [&'static str],
}

impl ParamSpec {
    /// A continuous (linear) parameter.
    pub const fn new(id: &'static str, name: &'static str, min: f32, max: f32, default: f32, unit: Unit) -> Self {
        ParamSpec { id, name, min, max, default, unit, log_scale: false, choices: &[] }
    }
    /// A continuous parameter best shown on a logarithmic control.
    pub const fn log(id: &'static str, name: &'static str, min: f32, max: f32, default: f32, unit: Unit) -> Self {
        ParamSpec { id, name, min, max, default, unit, log_scale: true, choices: &[] }
    }
    /// An on/off toggle.
    pub const fn toggle(id: &'static str, name: &'static str, default: bool) -> Self {
        ParamSpec { id, name, min: 0.0, max: 1.0, default: if default { 1.0 } else { 0.0 }, unit: Unit::Toggle, log_scale: false, choices: &[] }
    }
    /// A discrete choice; the value is the index into `choices`.
    pub const fn choice(id: &'static str, name: &'static str, choices: &'static [&'static str], default: usize) -> Self {
        ParamSpec { id, name, min: 0.0, max: (choices.len() - 1) as f32, default: default as f32, unit: Unit::Choice, log_scale: false, choices }
    }
    /// Clamp (and for choices/toggles, round) a value into this parameter's domain.
    /// NaN maps to the default.
    pub fn sanitize(&self, v: f32) -> f32 {
        if v.is_nan() {
            return self.default;
        }
        let v = v.clamp(self.min, self.max);
        match self.unit {
            Unit::Choice | Unit::Toggle => v.round(),
            _ => v,
        }
    }
}

/// A real-time audio effect operating in place on planar channels.
///
/// Implementations are constructed (allocating) with a sample rate and channel count; after that
/// `process`, `set_param` and `reset` never allocate. If `process` receives more channels than the
/// effect was built for, the extra channels are passed through untouched (channel-generic effects
/// such as gain process all of them).
pub trait AudioEffect: Send {
    /// Registry id (e.g. `"compressor"`).
    fn id(&self) -> &'static str;
    /// Parameter descriptions (same as the registry entry's).
    fn params(&self) -> &'static [ParamSpec];
    /// Set a parameter by id; the value is clamped to its range. Returns `false` for unknown ids.
    /// Takes effect smoothly from the next processed sample.
    fn set_param(&mut self, id: &str, value: f32) -> bool;
    /// Current (target) value of a parameter.
    fn param(&self, id: &str) -> Option<f32>;
    /// Clear all internal state (delay lines, envelopes, filter memories); smoothed parameters
    /// jump to their targets.
    fn reset(&mut self);
    /// Process one block in place. All channel slices must have the same length (the shortest
    /// length is used otherwise). Zero-length blocks are allowed.
    fn process(&mut self, channels: &mut [&mut [f32]]);
    /// Processing latency in samples (look-ahead / STFT delay) for delay compensation.
    fn latency(&self) -> usize {
        0
    }
    /// Analytic magnitude response (dB) at `freq` Hz for the current target settings, for
    /// filter/EQ effects (drives the graphical EQ editors). `None` when not applicable.
    fn response_db(&self, _freq: f64) -> Option<f64> {
        None
    }
    /// Static input → output level curve (dB) of a dynamics effect for `band` (0 for
    /// single-band processors), at the current settings. `None` when not applicable.
    fn transfer_db(&self, _band: usize, _input_db: f32) -> Option<f32> {
        None
    }
}

/// Parameter value storage shared by the effect implementations.
#[derive(Clone, Debug)]
pub(crate) struct ParamValues {
    specs: &'static [ParamSpec],
    values: Vec<f32>,
}

impl ParamValues {
    pub(crate) fn new(specs: &'static [ParamSpec]) -> Self {
        ParamValues { specs, values: specs.iter().map(|s| s.default).collect() }
    }
    pub(crate) fn index_of(&self, id: &str) -> Option<usize> {
        self.specs.iter().position(|s| s.id == id)
    }
    /// Returns `true` if the id exists.
    pub(crate) fn set(&mut self, id: &str, v: f32) -> bool {
        match self.index_of(id) {
            Some(i) => {
                self.values[i] = self.specs[i].sanitize(v);
                true
            }
            None => false,
        }
    }
    pub(crate) fn get(&self, id: &str) -> Option<f32> {
        self.index_of(id).map(|i| self.values[i])
    }
    /// Value of a parameter that is known to exist (0 otherwise — a programming error caught by
    /// debug builds).
    pub(crate) fn v(&self, id: &str) -> f32 {
        debug_assert!(self.get(id).is_some(), "unknown parameter {id}");
        self.get(id).unwrap_or(0.0)
    }
    pub(crate) fn on(&self, id: &str) -> bool {
        self.v(id) >= 0.5
    }
    pub(crate) fn idx(&self, id: &str) -> usize {
        self.v(id).max(0.0) as usize
    }
}

/// Generates the id/params/set_param/param methods of [`AudioEffect`] for a struct with a
/// `pv: ParamValues` field, a `PARAMS` const and an `apply_params(&mut self, snap: bool)` method.
macro_rules! param_plumbing {
    ($id:expr) => {
        fn id(&self) -> &'static str {
            $id
        }
        fn params(&self) -> &'static [$crate::ParamSpec] {
            Self::PARAMS
        }
        fn set_param(&mut self, id: &str, value: f32) -> bool {
            if self.pv.set(id, value) {
                self.apply_params(false);
                true
            } else {
                false
            }
        }
        fn param(&self, id: &str) -> Option<f32> {
            self.pv.get(id)
        }
    };
}
pub(crate) use param_plumbing;

/// Decibels → linear amplitude.
#[inline]
pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// Linear amplitude → decibels (−inf for 0).
#[inline]
pub fn gain_to_db(g: f32) -> f32 {
    20.0 * g.abs().log10()
}

/// Flush tiny values to zero (denormal protection for recursive state). Non-finite values are
/// zeroed too, so one NaN or infinite input sample cannot poison the state until `reset`.
#[inline(always)]
pub(crate) fn flush(x: f64) -> f64 {
    if (1e-30..=f64::MAX).contains(&x.abs()) { x } else { 0.0 }
}

/// f32 variant of [`flush`] for f32 delay lines.
#[inline(always)]
pub(crate) fn flush32(x: f32) -> f32 {
    if (1e-25..=f32::MAX).contains(&x.abs()) { x } else { 0.0 }
}

/// Common block length of planar channels.
#[inline]
pub(crate) fn block_len(channels: &[&mut [f32]]) -> usize {
    channels.iter().map(|c| c.len()).min().unwrap_or(0)
}

#[cfg(test)]
pub(crate) mod testutil {
    /// Tiny deterministic PRNG (xorshift64*) for tests.
    pub struct Rng(u64);
    impl Rng {
        pub fn new(seed: u64) -> Self {
            Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
        }
        pub fn next_u64(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        /// Uniform in [-1, 1).
        pub fn uniform(&mut self) -> f32 {
            ((self.next_u64() >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
        }
    }

    pub fn sine(freq: f64, amp: f64, sr: f64, n: usize, phase: f64) -> Vec<f32> {
        (0..n).map(|i| (amp * (2.0 * std::f64::consts::PI * freq * i as f64 / sr + phase).sin()) as f32).collect()
    }

    pub fn rms(x: &[f32]) -> f64 {
        if x.is_empty() {
            return 0.0;
        }
        (x.iter().map(|&v| (v as f64) * (v as f64)).sum::<f64>() / x.len() as f64).sqrt()
    }

    pub fn db(x: f64) -> f64 {
        20.0 * x.log10()
    }

    /// Amplitude of the `freq` component of `x` (single-bin DFT / Goertzel-style correlation).
    pub fn tone_amplitude(x: &[f32], freq: f64, sr: f64) -> f64 {
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (i, &v) in x.iter().enumerate() {
            let w = 2.0 * std::f64::consts::PI * freq * i as f64 / sr;
            re += v as f64 * w.cos();
            im += v as f64 * w.sin();
        }
        2.0 * (re * re + im * im).sqrt() / x.len() as f64
    }
}
