//! Exact media time for FilmCraft.
//!
//! All time is an integer number of [`Tick`]s at [`TICKS_PER_SECOND`] = 254 016 000 000/s. That
//! rate divides evenly into every broadcast frame duration (23.976, 24, 25, 29.97, 30, 48, 50,
//! 59.94, 60, 120 …) and every common audio sample duration (8 k … 192 kHz, including the 44.1 k
//! family), so edits, frame math and audio alignment never drift.
//!
//! Timecode (SMPTE drop/non-drop, frames, feet+frames, samples) is only a *display* of ticks.
//!
//! Every conversion is total: a zero or negative rate (from a damaged file or project) gives
//! zero instead of a division-by-zero panic, and an invalid [`FrameRate`] counts as the default.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

use serde::{Deserialize, Serialize};
use std::fmt;
use std::ops::{Add, AddAssign, Neg, Sub, SubAssign};

/// Ticks per second.
pub const TICKS_PER_SECOND: i64 = 254_016_000_000;

/// A point or duration in time, in ticks.
/// Arithmetic saturates at the i64 storage limits; representable results remain exact.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Tick(pub i64);

impl Tick {
    pub const ZERO: Tick = Tick(0);
    pub const MAX: Tick = Tick(i64::MAX / 4);
    pub const MIN: Tick = Tick(i64::MIN / 4);

    pub fn from_seconds_f64(s: f64) -> Tick {
        Tick((s * TICKS_PER_SECOND as f64).round() as i64)
    }
    pub fn seconds(self) -> f64 {
        self.0 as f64 / TICKS_PER_SECOND as f64
    }
    /// Exact conversion from a count of `units` at `per_second` (e.g. samples at 48 000).
    pub fn from_units(units: i64, per_second: i64) -> Tick {
        Tick(clamp_i64((units as i128 * TICKS_PER_SECOND as i128).checked_div(per_second as i128).unwrap_or(0)))
    }
    /// Floor conversion to a count of units at `per_second`.
    pub fn to_units_floor(self, per_second: i64) -> i64 {
        clamp_i64((self.0 as i128 * per_second as i128).div_euclid(TICKS_PER_SECOND as i128))
    }
    /// Conversion from a rational timestamp `pts * num / den` seconds (container timebases).
    pub fn from_rational(pts: i64, num: i64, den: i64) -> Tick {
        if den == 0 {
            return Tick::ZERO;
        }
        let Some(t) = (pts as i128 * num as i128).checked_mul(TICKS_PER_SECOND as i128) else {
            // A product beyond i128 divided by an i64 cannot fit back into i64.
            return Tick(if (pts < 0) ^ (num < 0) ^ (den < 0) { i64::MIN } else { i64::MAX });
        };
        Tick(clamp_i64(t.div_euclid(den as i128)))
    }
    /// Inverse of [`Tick::from_rational`], floored to the timebase.
    pub fn to_rational_floor(self, num: i64, den: i64) -> i64 {
        clamp_i64((self.0 as i128 * den as i128).checked_div_euclid(num as i128 * TICKS_PER_SECOND as i128).unwrap_or(0))
    }
    /// Inverse of [`Tick::from_rational`], rounded to the nearest timebase unit (halves up).
    ///
    /// For finding a sample by its timestamp: containers store timestamps rounded to their
    /// timebase (Matroska usually to 1 ms), so a stamp can sit up to half a unit after the true
    /// time. Flooring the requested time misses every frame whose stamp was rounded up.
    pub fn to_rational_round(self, num: i64, den: i64) -> i64 {
        let n = self.0 as i128 * den as i128;
        let unit = num as i128 * TICKS_PER_SECOND as i128;
        let (n, unit) = if unit < 0 { (-n, -unit) } else { (n, unit) };
        let (Some(q), Some(r)) = (n.checked_div_euclid(unit), n.checked_rem_euclid(unit)) else { return 0 };
        // 0 <= r < unit: round up from the halfway point
        clamp_i64(if r * 2 >= unit { q + 1 } else { q })
    }
    pub fn abs(self) -> Tick {
        Tick(self.0.saturating_abs())
    }
    pub fn min(self, o: Tick) -> Tick {
        Tick(self.0.min(o.0))
    }
    pub fn max(self, o: Tick) -> Tick {
        Tick(self.0.max(o.0))
    }
    pub fn clamp(self, lo: Tick, hi: Tick) -> Tick {
        // max/min rather than `clamp`, which panics when the bounds cross.
        Tick(self.0.max(lo.0).min(hi.0))
    }
    /// Scale by a rational factor (`num/den`), flooring.
    pub fn mul_ratio(self, num: i64, den: i64) -> Tick {
        Tick(clamp_i64((self.0 as i128 * num as i128).checked_div_euclid(den as i128).unwrap_or(0)))
    }
}

impl Add for Tick {
    type Output = Tick;
    fn add(self, o: Tick) -> Tick {
        Tick(self.0.saturating_add(o.0))
    }
}
impl Sub for Tick {
    type Output = Tick;
    fn sub(self, o: Tick) -> Tick {
        Tick(self.0.saturating_sub(o.0))
    }
}
impl Neg for Tick {
    type Output = Tick;
    fn neg(self) -> Tick {
        Tick(self.0.saturating_neg())
    }
}
impl AddAssign for Tick {
    fn add_assign(&mut self, o: Tick) {
        self.0 = self.0.saturating_add(o.0);
    }
}
impl SubAssign for Tick {
    fn sub_assign(&mut self, o: Tick) {
        self.0 = self.0.saturating_sub(o.0);
    }
}

/// A half-open time range `[start, start + duration)`.
/// Zero or negative durations are empty and never overlap another range.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TimeRange {
    pub start: Tick,
    pub duration: Tick,
}

impl TimeRange {
    pub fn new(start: Tick, duration: Tick) -> Self {
        Self { start, duration }
    }
    pub fn from_bounds(start: Tick, end: Tick) -> Self {
        Self { start, duration: end - start }
    }
    pub fn end(&self) -> Tick {
        self.start + self.duration
    }
    pub fn contains(&self, t: Tick) -> bool {
        t >= self.start && t < self.end()
    }
    pub fn overlaps(&self, o: &TimeRange) -> bool {
        !self.is_empty() && !o.is_empty() && self.start < o.end() && o.start < self.end()
    }
    pub fn intersect(&self, o: &TimeRange) -> Option<TimeRange> {
        let s = self.start.max(o.start);
        let e = self.end().min(o.end());
        (e > s).then(|| TimeRange::from_bounds(s, e))
    }
    pub fn is_empty(&self) -> bool {
        self.duration.0 <= 0
    }
}

/// A frame rate as an exact rational (`num / den` frames per second).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FrameRate {
    pub num: i64,
    pub den: i64,
}

impl Default for FrameRate {
    fn default() -> Self {
        FrameRate::FPS_23_976
    }
}

impl FrameRate {
    pub const FPS_23_976: FrameRate = FrameRate { num: 24000, den: 1001 };
    pub const FPS_24: FrameRate = FrameRate { num: 24, den: 1 };
    pub const FPS_25: FrameRate = FrameRate { num: 25, den: 1 };
    pub const FPS_29_97: FrameRate = FrameRate { num: 30000, den: 1001 };
    pub const FPS_30: FrameRate = FrameRate { num: 30, den: 1 };
    pub const FPS_48: FrameRate = FrameRate { num: 48, den: 1 };
    pub const FPS_50: FrameRate = FrameRate { num: 50, den: 1 };
    pub const FPS_59_94: FrameRate = FrameRate { num: 60000, den: 1001 };
    pub const FPS_60: FrameRate = FrameRate { num: 60, den: 1 };
    pub const FPS_119_88: FrameRate = FrameRate { num: 120000, den: 1001 };
    pub const FPS_120: FrameRate = FrameRate { num: 120, den: 1 };

    /// Rates offered in sequence settings (Premiere's list).
    pub const COMMON: [FrameRate; 11] = [
        Self::FPS_23_976,
        Self::FPS_24,
        Self::FPS_25,
        Self::FPS_29_97,
        Self::FPS_30,
        Self::FPS_48,
        Self::FPS_50,
        Self::FPS_59_94,
        Self::FPS_60,
        Self::FPS_119_88,
        Self::FPS_120,
    ];

    pub fn new(num: i64, den: i64) -> Self {
        let g = i128::from(gcd(num.unsigned_abs(), den.unsigned_abs()).max(1));
        FrameRate { num: (i128::from(num) / g) as i64, den: (i128::from(den) / g) as i64 }
    }

    /// Closest standard rate for a float (e.g. from a container's average rate).
    pub fn from_f64(fps: f64) -> Self {
        for r in Self::COMMON {
            if (r.as_f64() - fps).abs() < 0.005 {
                return r;
            }
        }
        FrameRate::new((fps * 1000.0).round() as i64, 1000)
    }

    /// Closest standard rate when `fps` is within `tolerance` of it, `tolerance` a fraction of the
    /// rate (`0.005` is half a percent), else the nearest 1/1000 rate. A rate a container reports
    /// from timestamps rounded to a coarse unit lands slightly off a standard rate: at 90 kHz,
    /// whole milliseconds leave a 30 fps clip's average at 30.02 or 29.95 (#1700).
    pub fn from_f64_within(fps: f64, tolerance: f64) -> Self {
        let mut best: Option<(f64, FrameRate)> = None;
        for r in Self::COMMON {
            let off = (r.as_f64() - fps).abs();
            if off <= tolerance * r.as_f64() && best.is_none_or(|(closest, _)| off < closest) {
                best = Some((off, r));
            }
        }
        best.map_or_else(|| Self::from_f64(fps), |(_, r)| r)
    }

    /// This rate if it is positive, else the default (a damaged file or project can carry a zero
    /// or negative rate; every frame computation uses this so it can't divide by zero).
    pub fn sane(self) -> FrameRate {
        if self.num > 0 && self.den > 0 { self } else { FrameRate::default() }
    }

    pub fn as_f64(self) -> f64 {
        self.num as f64 / self.den as f64
    }

    /// Exact duration of one frame (rounded down only for exotic rates).
    /// Values outside the integer tick range saturate at its limit.
    pub fn frame_duration(self) -> Tick {
        let r = self.sane();
        Tick(clamp_i64((TICKS_PER_SECOND as i128 * r.den as i128) / r.num as i128))
    }

    /// Index of the frame containing `t` (floor), saturated to the integer frame range.
    pub fn frame_at(self, t: Tick) -> i64 {
        clamp_i64(frame_index(self.sane(), t))
    }

    /// Start tick of frame `f` (ceiling division).
    ///
    /// Uses ceiling (not floor) so that `frame_at(tick_of(f)) == f` holds for every
    /// representable frame boundary with at least one tick per frame, even when the frame duration is not an integer number of ticks
    /// (exotic rates like 37.516 fps where `TICKS_PER_SECOND * den / num` has a
    /// fractional remainder).  Floor division truncates the remainder, causing each
    /// frame boundary to drift backward by one tick until `frame_at` returns `f - 1`
    /// instead of `f` — frame stepping then stalls or moves backward.
    /// Values outside the integer tick range saturate at its limit.
    pub fn tick_of(self, f: i64) -> Tick {
        Tick(clamp_i64(frame_boundary(self.sane(), i128::from(f))))
    }

    /// Snap `t` down to a frame boundary.
    pub fn snap(self, t: Tick) -> Tick {
        let r = self.sane();
        Tick(clamp_i64(frame_boundary(r, frame_index(r, t))))
    }

    /// Snap `t` to the nearest frame boundary.
    pub fn snap_nearest(self, t: Tick) -> Tick {
        let r = self.sane();
        // Keep the frame index and both boundaries wide until choosing the nearest. Saturating
        // an unrepresentable upper boundary first would make it seem closer than it really is.
        let f = frame_index(r, t);
        let a = frame_boundary(r, f);
        let b = frame_boundary(r, f.saturating_add(1));
        let t = i128::from(t.0);
        Tick(clamp_i64(if t - a <= b - t { a } else { b }))
    }

    /// Timecode base (frames counted per timecode second): 30 for 29.97, 24 for 23.976.
    pub fn timecode_base(self) -> i64 {
        let r = self.sane();
        (r.num / r.den).saturating_add(i64::from(r.num % r.den != 0)).max(1)
    }

    /// NTSC (x/1001) rates can use drop-frame timecode.
    pub fn is_ntsc(self) -> bool {
        self.den == 1001
    }

    /// Whether drop-frame counting applies (29.97 / 59.94 / 119.88).
    pub fn supports_drop_frame(self) -> bool {
        self.is_ntsc() && self.timecode_base() % 30 == 0
    }

    pub fn label(self) -> String {
        if self.den == 1 {
            format!("{}", self.num)
        } else {
            let v = self.as_f64();
            let s = format!("{v:.3}");
            let s = s.trim_end_matches('0').trim_end_matches('.');
            s.to_string()
        }
    }
}

impl fmt::Display for FrameRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} fps", self.label())
    }
}

/// Narrow only after applying the storage bounds, never by a wrapping integer cast.
fn clamp_i64(value: i128) -> i64 {
    value.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

/// Callers pass a positive rate. Products of two i64 values fit in i128.
fn frame_index(rate: FrameRate, t: Tick) -> i128 {
    (i128::from(t.0) * i128::from(rate.num)).div_euclid(i128::from(TICKS_PER_SECOND) * i128::from(rate.den))
}

/// Ceiling of the rational boundary. An overflowing numerator divided by an i64 rate cannot
/// fit in an i64 tick, so saturating that intermediate preserves the public result. Boundaries
/// derived from frame_index remain within i128 (at most one frame away from an i64 tick).
fn frame_boundary(rate: FrameRate, frame: i128) -> i128 {
    let Some(n) = frame.checked_mul(i128::from(TICKS_PER_SECOND)).and_then(|n| n.checked_mul(i128::from(rate.den))) else {
        return if frame < 0 { i128::MIN } else { i128::MAX };
    };
    let d = i128::from(rate.num);
    // Flooring a fractional boundary puts the tick in the preceding frame.
    n.div_euclid(d) + i128::from(n.rem_euclid(d) != 0)
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// How time is displayed (Premiere: Timecode, Feet+Frames 16mm/35mm, Frames, Audio Samples).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TimeDisplay {
    #[default]
    Timecode,
    Frames,
    Feet16,
    Feet35,
    AudioSamples,
    Seconds,
}

impl TimeDisplay {
    pub const ALL: [TimeDisplay; 6] =
        [TimeDisplay::Timecode, TimeDisplay::Feet35, TimeDisplay::Feet16, TimeDisplay::Frames, TimeDisplay::AudioSamples, TimeDisplay::Seconds];
    pub fn label(self) -> &'static str {
        match self {
            TimeDisplay::Timecode => "Timecode",
            TimeDisplay::Frames => "Frames",
            TimeDisplay::Feet16 => "Feet + Frames 16mm",
            TimeDisplay::Feet35 => "Feet + Frames 35mm",
            TimeDisplay::AudioSamples => "Audio Samples",
            TimeDisplay::Seconds => "Seconds",
        }
    }
}

/// Drop-frame parameters: frames dropped per minute and nominal base.
fn df_params(rate: FrameRate) -> (i64, i64) {
    let base = rate.timecode_base();
    (base / 15, base) // 30 → 2, 60 → 4, 120 → 8
}

/// Convert a frame count to SMPTE fields `(negative, h, m, s, f)`.
pub fn frames_to_fields(frame: i64, rate: FrameRate, drop_frame: bool) -> (bool, i64, i64, i64, i64) {
    let neg = frame < 0;
    let mut n = i128::from(frame).abs();
    let (drop, base) = df_params(rate);
    let (drop, base) = (i128::from(drop), i128::from(base));
    if drop_frame && rate.supports_drop_frame() {
        let per_min = base * 60 - drop;
        let per_10 = per_min * 10 + drop;
        let d = n / per_10;
        let m = n % per_10;
        n = n.saturating_add(drop * 9 * d);
        if m > drop {
            n = n.saturating_add(drop * ((m - drop) / per_min));
        }
    }
    let f = n % base;
    let s = (n / base) % 60;
    let mi = (n / (base * 60)) % 60;
    let h = n / (base * 3600);
    (neg, clamp_i64(h), clamp_i64(mi), clamp_i64(s), clamp_i64(f))
}

/// Convert SMPTE fields back to a frame count.
pub fn fields_to_frames(h: i64, m: i64, s: i64, f: i64, rate: FrameRate, drop_frame: bool) -> i64 {
    let (drop, base) = df_params(rate);
    let seconds = i128::from(h) * 3600 + i128::from(m) * 60 + i128::from(s);
    // Nominal drop-frame labels can exceed i64 even when the final frame count fits.
    let mut n = seconds.saturating_mul(i128::from(base)).saturating_add(i128::from(f));
    if drop_frame && rate.supports_drop_frame() {
        let total_min = i128::from(h) * 60 + i128::from(m);
        n = n.saturating_sub(i128::from(drop) * (total_min - total_min / 10));
    }
    clamp_i64(n)
}

/// Format a frame count as SMPTE timecode (`HH:MM:SS:FF`, or `HH;MM;SS;FF` for drop-frame).
pub fn format_timecode_frames(frame: i64, rate: FrameRate, drop_frame: bool) -> String {
    let df = drop_frame && rate.supports_drop_frame();
    let (neg, h, m, s, f) = frames_to_fields(frame, rate, df);
    let sep = if df { ';' } else { ':' };
    let fw = if rate.timecode_base() >= 100 { 3 } else { 2 };
    format!("{}{h:02}{sep}{m:02}{sep}{s:02}{sep}{f:0fw$}", if neg { "-" } else { "" })
}

/// Format a tick according to a display mode.
pub fn format_time(t: Tick, rate: FrameRate, drop_frame: bool, display: TimeDisplay, sample_rate: i64) -> String {
    let frame = rate.frame_at(t);
    match display {
        TimeDisplay::Timecode => format_timecode_frames(frame, rate, drop_frame),
        TimeDisplay::Frames => format!("{frame}"),
        TimeDisplay::Feet35 | TimeDisplay::Feet16 => {
            let per_ft = if display == TimeDisplay::Feet35 { 16 } else { 40 };
            let neg = frame < 0;
            let a = frame.unsigned_abs();
            format!("{}{}+{:02}", if neg { "-" } else { "" }, a / per_ft, a % per_ft)
        }
        TimeDisplay::AudioSamples => {
            let neg = t.0 < 0;
            let a = t.0.unsigned_abs();
            let tps = TICKS_PER_SECOND.unsigned_abs();
            let secs = a / tps;
            let rem = Tick((a % tps) as i64).to_units_floor(sample_rate);
            format!("{}{:02}:{:02}:{:02}:{rem:05}", if neg { "-" } else { "" }, secs / 3600, (secs / 60) % 60, secs % 60)
        }
        TimeDisplay::Seconds => {
            let s = format!("{:.3}", t.seconds());
            // A tiny negative time rounds to zero but keeps its sign: drop it.
            if s == "-0.000" { "0.000".to_string() } else { s }
        }
    }
}

/// Error from [`parse_timecode`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub String);

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ParseError {}

/// Parse user-typed timecode into a frame count, the way Premiere's timecode fields do:
/// - `01:02:03:04`, `01;02;03;04`, `1.2.3.4` (any of `:;.,` separators),
/// - bare digits are read right-aligned as `HHMMSSFF` (`1000` = 00:00:10:00),
/// - a leading `+`/`-` makes the value relative to `current` (`+15` = 15 frames later, `-1.00` = 1 s earlier).
pub fn parse_timecode(input: &str, rate: FrameRate, drop_frame: bool, current: i64) -> Result<i64, ParseError> {
    let s = input.trim();
    if s.is_empty() {
        return Err(ParseError("empty timecode".into()));
    }
    let (rel, body) = match s.as_bytes()[0] {
        b'+' => (Some(1), &s[1..]),
        b'-' => (Some(-1), &s[1..]),
        _ => (None, s),
    };
    let parts: Vec<&str> = body.split([':', ';', '.', ',']).collect();
    let nums: Vec<i64> = if parts.len() == 1 {
        let digits = parts[0];
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(ParseError(format!("not a timecode: `{input}`")));
        }
        // Right-aligned pairs: "12345" -> [1, 23, 45]; relative bare numbers are frames.
        if rel.is_some() {
            vec![digits.parse().map_err(|_| ParseError("number too large".into()))?]
        } else {
            let mut v = Vec::new();
            let b = digits.as_bytes();
            let mut end = b.len();
            while end > 0 {
                let start = end.saturating_sub(2);
                v.push(digits[start..end].parse::<i64>().unwrap_or(0));
                end = start;
            }
            v.reverse();
            v
        }
    } else {
        parts
            .iter()
            .map(|p| {
                if p.is_empty() {
                    Ok(0)
                } else if !p.bytes().all(|b| b.is_ascii_digit()) {
                    Err(ParseError(format!("bad field `{p}`")))
                } else {
                    p.parse::<i64>().map_err(|_| ParseError(format!("bad field `{p}`")))
                }
            })
            .collect::<Result<_, _>>()?
    };
    if nums.len() > 4 {
        return Err(ParseError("too many timecode fields".into()));
    }
    let mut f4 = [0i64; 4];
    let off = 4 - nums.len();
    f4[off..].copy_from_slice(&nums);
    let [h, m, sec, fr] = f4;
    // Overflowing fields (e.g. 90 frames) are allowed, as in Premiere.
    let base = rate.timecode_base();
    let frames = if nums.len() == 1 && rel.is_some() {
        fr
    } else if drop_frame && rate.supports_drop_frame() && m < 60 && sec < 60 && fr < base {
        fields_to_frames(h, m, sec, fr, rate, true)
    } else {
        // Saturating: typed or agent-sent timecodes can have absurdly large fields.
        h.saturating_mul(3600).saturating_add(m.saturating_mul(60)).saturating_add(sec).saturating_mul(base).saturating_add(fr)
    };
    Ok(match rel {
        Some(sign) => current.saturating_add(frames.saturating_mul(sign)),
        None => frames,
    })
}

/// Common audio sample rates offered in sequence settings.
pub const SAMPLE_RATES: [i64; 6] = [32_000, 44_100, 48_000, 88_200, 96_000, 192_000];

#[cfg(test)]
mod tests;
