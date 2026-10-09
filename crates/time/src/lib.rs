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
        Tick((units as i128 * TICKS_PER_SECOND as i128).checked_div(per_second as i128).unwrap_or(0) as i64)
    }
    /// Floor conversion to a count of units at `per_second`.
    pub fn to_units_floor(self, per_second: i64) -> i64 {
        (self.0 as i128 * per_second as i128).div_euclid(TICKS_PER_SECOND as i128) as i64
    }
    /// Conversion from a rational timestamp `pts * num / den` seconds (container timebases).
    pub fn from_rational(pts: i64, num: i64, den: i64) -> Tick {
        let t = pts as i128 * num as i128 * TICKS_PER_SECOND as i128;
        Tick(t.checked_div_euclid(den as i128).unwrap_or(0) as i64)
    }
    /// Inverse of [`Tick::from_rational`], floored to the timebase.
    pub fn to_rational_floor(self, num: i64, den: i64) -> i64 {
        (self.0 as i128 * den as i128).checked_div_euclid(num as i128 * TICKS_PER_SECOND as i128).unwrap_or(0) as i64
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
        (if r * 2 >= unit { q + 1 } else { q }) as i64
    }
    pub fn abs(self) -> Tick {
        Tick(self.0.abs())
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
        Tick((self.0 as i128 * num as i128).checked_div_euclid(den as i128).unwrap_or(0) as i64)
    }
}

impl Add for Tick {
    type Output = Tick;
    fn add(self, o: Tick) -> Tick {
        Tick(self.0 + o.0)
    }
}
impl Sub for Tick {
    type Output = Tick;
    fn sub(self, o: Tick) -> Tick {
        Tick(self.0 - o.0)
    }
}
impl Neg for Tick {
    type Output = Tick;
    fn neg(self) -> Tick {
        Tick(-self.0)
    }
}
impl AddAssign for Tick {
    fn add_assign(&mut self, o: Tick) {
        self.0 += o.0;
    }
}
impl SubAssign for Tick {
    fn sub_assign(&mut self, o: Tick) {
        self.0 -= o.0;
    }
}

/// A half-open time range `[start, start + duration)`.
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
        self.start < o.end() && o.start < self.end()
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
        let g = gcd(num.abs(), den.abs()).max(1);
        FrameRate { num: num / g, den: den / g }
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

    /// This rate if it is positive, else the default (a damaged file or project can carry a zero
    /// or negative rate; every frame computation uses this so it can't divide by zero).
    pub fn sane(self) -> FrameRate {
        if self.num > 0 && self.den > 0 { self } else { FrameRate::default() }
    }

    pub fn as_f64(self) -> f64 {
        self.num as f64 / self.den as f64
    }

    /// Exact duration of one frame (rounded down only for exotic rates).
    pub fn frame_duration(self) -> Tick {
        let r = self.sane();
        Tick(((TICKS_PER_SECOND as i128 * r.den as i128) / r.num as i128) as i64)
    }

    /// Index of the frame containing `t` (floor).
    pub fn frame_at(self, t: Tick) -> i64 {
        let r = self.sane();
        (t.0 as i128 * r.num as i128).div_euclid(TICKS_PER_SECOND as i128 * r.den as i128) as i64
    }

    /// Start tick of frame `f` (ceiling division).
    ///
    /// Uses ceiling (not floor) so that `frame_at(tick_of(f)) == f` holds for every
    /// frame index, even when the frame duration is not an integer number of ticks
    /// (exotic rates like 37.516 fps where `TICKS_PER_SECOND * den / num` has a
    /// fractional remainder).  Floor division truncates the remainder, causing each
    /// frame boundary to drift backward by one tick until `frame_at` returns `f - 1`
    /// instead of `f` — frame stepping then stalls or moves backward.
    /// Ceiling is correct because the frame duration is always > 1 tick, so
    /// `floor(ceil(f·D) / D) == f` for every integer `f`.
    pub fn tick_of(self, f: i64) -> Tick {
        let r = self.sane();
        let n = f as i128 * TICKS_PER_SECOND as i128 * r.den as i128;
        Tick((-(-n).div_euclid(r.num as i128)) as i64)
    }

    /// Snap `t` down to a frame boundary.
    pub fn snap(self, t: Tick) -> Tick {
        self.tick_of(self.frame_at(t))
    }

    /// Snap `t` to the nearest frame boundary.
    pub fn snap_nearest(self, t: Tick) -> Tick {
        let a = self.snap(t);
        let b = self.tick_of(self.frame_at(t) + 1);
        if (t - a) <= (b - t) { a } else { b }
    }

    /// How far (either side) a time can be from a frame boundary and still count as on it: a
    /// thousandth of a frame. Cuts made before `tick_of` rounded up (FilmCraft 0.4.0 and
    /// earlier), or summed from rounded durations, sit a tick or two before the boundary of the
    /// frame they belong to at rates whose frame is not a whole number of ticks (58.824 fps
    /// screen recordings, 37.516 fps phone footage…).
    pub fn boundary_slack(self) -> Tick {
        Tick((self.frame_duration().0 / 1000).max(1))
    }

    /// The frame boundary `t` is on, when it is within [`FrameRate::boundary_slack`] of one.
    fn near_boundary(self, t: Tick) -> Option<Tick> {
        let f = self.frame_at(t);
        let (a, b) = (self.tick_of(f), self.tick_of(f.saturating_add(1)));
        let slack = self.boundary_slack().0;
        if t.0.saturating_sub(a.0) <= slack {
            Some(a)
        } else if b.0.saturating_sub(t.0) <= slack {
            Some(b)
        } else {
            None
        }
    }

    /// Snap `t` down to a frame boundary (the frame that contains it), except that a time a hair
    /// before a boundary counts as on it. The playhead parks here: a cut stored a tick before
    /// frame 253 puts the playhead on 253, not on 252, the last frame of the outgoing clip.
    pub fn snap_frame(self, t: Tick) -> Tick {
        self.near_boundary(t).unwrap_or_else(|| self.snap(t))
    }

    /// Where the playhead goes for an edit point at `t`: the first frame that shows what comes
    /// after the edit. That is the boundary at or after `t` (an edit part-way through a frame
    /// leaves that frame showing what came before), with a time within
    /// [`FrameRate::boundary_slack`] of a boundary counted as on it.
    pub fn snap_edit(self, t: Tick) -> Tick {
        self.near_boundary(t).unwrap_or_else(|| self.tick_of(self.frame_at(t).saturating_add(1)))
    }

    /// Timecode base (frames counted per timecode second): 30 for 29.97, 24 for 23.976.
    pub fn timecode_base(self) -> i64 {
        let r = self.sane();
        ((r.num + r.den - 1) / r.den).max(1)
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

fn gcd(mut a: i64, mut b: i64) -> i64 {
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
    let mut n = frame.saturating_abs();
    let (drop, base) = df_params(rate);
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
    (neg, h, mi, s, f)
}

/// Convert SMPTE fields back to a frame count.
pub fn fields_to_frames(h: i64, m: i64, s: i64, f: i64, rate: FrameRate, drop_frame: bool) -> i64 {
    let (drop, base) = df_params(rate);
    let mut n = h.saturating_mul(3600).saturating_add(m.saturating_mul(60)).saturating_add(s).saturating_mul(base).saturating_add(f);
    if drop_frame && rate.supports_drop_frame() {
        let total_min = h.saturating_mul(60).saturating_add(m);
        n = n.saturating_sub(drop.saturating_mul(total_min - total_min / 10));
    }
    n
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
            let a = frame.abs();
            format!("{}{}+{:02}", if neg { "-" } else { "" }, a / per_ft, a % per_ft)
        }
        TimeDisplay::AudioSamples => {
            let secs = t.0.div_euclid(TICKS_PER_SECOND);
            let rem = Tick(t.0.rem_euclid(TICKS_PER_SECOND)).to_units_floor(sample_rate);
            format!("{:02}:{:02}:{:02}:{rem:05}", secs / 3600, (secs / 60) % 60, secs % 60)
        }
        TimeDisplay::Seconds => format!("{:.3}", t.seconds()),
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
            .map(|p| if p.is_empty() { Ok(0) } else { p.parse::<i64>().map_err(|_| ParseError(format!("bad field `{p}`"))) })
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
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Damaged files and projects can carry zero / negative rates and timebases, and users or
    /// agents can type huge timecodes: these used to panic (division by zero, overflow,
    /// crossed `clamp` bounds).
    #[test]
    fn hostile_rates_and_timecodes_never_panic() {
        let zero = FrameRate { num: 0, den: 0 };
        for r in [
            zero,
            FrameRate { num: 0, den: 1 },
            FrameRate { num: 25, den: 0 },
            FrameRate { num: -30, den: 1 },
            FrameRate::from_f64(0.0),
            FrameRate::from_f64(f64::NAN),
        ] {
            assert_eq!(r.tick_of(10), FrameRate::default().tick_of(10));
            assert_eq!(r.frame_at(Tick(TICKS_PER_SECOND)), FrameRate::default().frame_at(Tick(TICKS_PER_SECOND)));
            assert!(r.frame_duration() > Tick::ZERO);
            assert!(r.timecode_base() >= 1);
            let _ = r.snap_nearest(Tick(12345));
            let _ = format_time(Tick(TICKS_PER_SECOND), r, true, TimeDisplay::Timecode, 48_000);
        }
        assert_eq!(Tick::from_rational(100, 1, 0), Tick::ZERO);
        assert_eq!(Tick::from_units(100, 0), Tick::ZERO);
        assert_eq!(Tick(5).to_rational_floor(0, 1), 0);
        assert_eq!(Tick(5).to_rational_round(0, 1), 0);
        for t in [i64::MIN, -1, 0, 1, i64::MAX] {
            for (n, d) in [(i64::MIN, 1), (-1, 1000), (1, i64::MAX), (i64::MAX, i64::MIN), (1, 0)] {
                let _ = Tick(t).to_rational_round(n, d);
            }
        }
        assert_eq!(Tick(5).mul_ratio(1, 0), Tick::ZERO);
        assert_eq!(Tick(5).clamp(Tick(10), Tick(0)), Tick(0));
        let _ = format_timecode_frames(i64::MIN, FrameRate::FPS_29_97, true);
        for tc in ["99999999999:99:99:99", "9223372036854775807", "+9223372036854775807", "99999999999;59;59;29"] {
            let _ = parse_timecode(tc, FrameRate::FPS_29_97, true, i64::MAX);
            let _ = parse_timecode(tc, FrameRate::FPS_25, false, i64::MIN);
        }
    }

    #[test]
    fn every_common_rate_is_exact() {
        for r in FrameRate::COMMON {
            let d = r.frame_duration();
            assert_eq!(d.0 as i128 * r.num as i128, TICKS_PER_SECOND as i128 * r.den as i128, "{r}");
        }
        for sr in SAMPLE_RATES.iter().chain(&[8000, 11025, 16000, 22050, 176_400]) {
            assert_eq!(TICKS_PER_SECOND % sr, 0, "{sr}");
        }
    }

    #[test]
    fn drop_frame_known_values() {
        let r = FrameRate::FPS_29_97;
        assert_eq!(format_timecode_frames(0, r, true), "00;00;00;00");
        assert_eq!(format_timecode_frames(1799, r, true), "00;00;59;29");
        assert_eq!(format_timecode_frames(1800, r, true), "00;01;00;02");
        assert_eq!(format_timecode_frames(17982, r, true), "00;10;00;00");
        assert_eq!(format_timecode_frames(107892, r, true), "01;00;00;00");
        assert_eq!(format_timecode_frames(1800, r, false), "00:01:00:00");
        let r60 = FrameRate::FPS_59_94;
        assert_eq!(format_timecode_frames(3600, r60, true), "00;01;00;04");
    }

    #[test]
    fn parse_forms() {
        let r = FrameRate::FPS_25;
        assert_eq!(parse_timecode("00:00:10:00", r, false, 0).unwrap(), 250);
        assert_eq!(parse_timecode("1000", r, false, 0).unwrap(), 250);
        assert_eq!(parse_timecode("1.00", r, false, 0).unwrap(), 25);
        assert_eq!(parse_timecode("+15", r, false, 100).unwrap(), 115);
        assert_eq!(parse_timecode("-1.00", r, false, 100).unwrap(), 75);
        assert_eq!(parse_timecode("00;01;00;02", FrameRate::FPS_29_97, true, 0).unwrap(), 1800);
        assert!(parse_timecode("abc", r, false, 0).is_err());
    }

    #[test]
    fn display_modes() {
        let r = FrameRate::FPS_24;
        let t = r.tick_of(40);
        assert_eq!(format_time(t, r, false, TimeDisplay::Feet35, 48000), "2+08");
        assert_eq!(format_time(t, r, false, TimeDisplay::Feet16, 48000), "1+00");
        assert_eq!(format_time(t, r, false, TimeDisplay::Frames, 48000), "40");
        assert_eq!(format_time(Tick::from_units(48_001, 48_000), r, false, TimeDisplay::AudioSamples, 48000), "00:00:01:00001");
    }

    #[test]
    fn rational_conversions() {
        // 90 kHz MPEG timebase
        let t = Tick::from_rational(90_000, 1, 90_000);
        assert_eq!(t.0, TICKS_PER_SECOND);
        assert_eq!(t.to_rational_floor(1, 90_000), 90_000);
        assert_eq!(t.to_rational_round(1, 90_000), 90_000);
        // 1 ms timebase (Matroska): to the nearest millisecond, halves up
        let ms = |t: Tick| t.to_rational_round(1, 1000);
        assert_eq!(ms(FrameRate::FPS_30.tick_of(1)), 33);
        assert_eq!(ms(FrameRate::FPS_30.tick_of(2)), 67);
        assert_eq!(FrameRate::FPS_30.tick_of(2).to_rational_floor(1, 1000), 66);
        assert_eq!(ms(FrameRate::FPS_29_97.tick_of(15)), 501, "500.5 ms");
        assert_eq!(ms(Tick::from_rational(-4, 1, 10_000)), 0, "-0.4 ms");
        assert_eq!(ms(Tick::from_rational(-5, 1, 10_000)), 0, "-0.5 ms");
        assert_eq!(ms(Tick::from_rational(-6, 1, 10_000)), -1, "-0.6 ms");
        // a negative timebase is nonsense, but rounds the same way
        assert_eq!(FrameRate::FPS_30.tick_of(2).to_rational_round(-1, -1000), 67);
        assert_eq!(FrameRate::FPS_29_97.tick_of(15).to_rational_round(-1, -1000), 501);
        assert_eq!(FrameRate::from_f64(29.97), FrameRate::FPS_29_97);
        assert_eq!(FrameRate::FPS_23_976.label(), "23.976");
    }

    #[test]
    fn exotic_frame_rate_roundtrip() {
        // Issue #301: 37.516 fps footage.  The frame duration is
        // 254_016_000_000 * 250 / 9379 = 6_770_871_094.999… ticks — not an
        // integer.  Floor division in `tick_of` truncated to 6_770_871_094,
        // losing ~1 tick/frame so `frame_at(tick_of(f))` returned `f - 1`.
        // Ceiling division makes the round-trip exact for all integer frames.
        let rates: &[(FrameRate, &str)] = &[
            (FrameRate::from_f64(37.516), "37.516"),
            (FrameRate::from_f64(37.5161), "37.5161"),
            (FrameRate::from_f64(12.345), "12.345"),
            (FrameRate::from_f64(59.9401), "59.9401"),
            (FrameRate::from_f64(23.9761), "23.9761"),
        ];
        for &(r, name) in rates {
            // Round-trip over a wide range including negatives.
            let bad: Vec<i64> = (-20_000..20_000).filter(|&f| r.frame_at(r.tick_of(f)) != f).collect();
            assert!(bad.is_empty(), "{name}: {} round-trip failures, first {bad:?}", r);

            // snap(tick_of(f)) == tick_of(f): snapping a frame boundary is a no-op.
            for f in -100i64..100 {
                assert_eq!(r.snap(r.tick_of(f)), r.tick_of(f), "{name}: snap(tick_of({f}))");
            }

            // Step-back: the last tick of frame f is one tick before tick_of(f+1),
            // and frame_at of that is still f.  Also check the frame_duration path:
            // frame_at(tick_of(f) - frame_duration()) == f - 1.
            for f in 1..2000i64 {
                let last_tick = r.tick_of(f + 1) - Tick(1);
                assert_eq!(r.frame_at(last_tick), f, "{name}: last tick of frame {f}");
                let prev = r.tick_of(f) - r.frame_duration();
                assert_eq!(r.frame_at(prev), f - 1, "{name}: tick_of({f}) - frame_duration");
            }
        }

        // Stepping simulation: frame_at(playhead) + 1, tick_of, snap.
        // Before the fix this stalled at frame 0 (step forward did nothing).
        let r = FrameRate::from_f64(37.516);
        let mut tick = Tick::ZERO;
        for expected in 0..50 {
            let frame = r.frame_at(tick);
            assert_eq!(frame, expected, "stepping forward at frame {expected}");
            tick = r.tick_of(frame + 1);
            tick = r.snap(tick); // set_playhead calls snap
        }

        // Stepping backward from frame 50 should land on 49, 48, … 0
        tick = r.tick_of(50);
        let mut last = 50;
        while last > 0 {
            let frame = r.frame_at(tick);
            let target = frame - 1;
            if target < 0 {
                break;
            }
            tick = r.snap(r.tick_of(target));
            let landed = r.frame_at(tick);
            assert_eq!(landed, target, "stepping backward to frame {target}");
            last = landed;
        }
    }

    /// A 58.824 fps screen recording (7353/125) cut in FilmCraft 0.4.0, whose `tick_of` rounded
    /// down: every cut sits one tick before its frame boundary. A plain floor snap parked the
    /// playhead a whole frame before the cut (on the outgoing clip's last frame).
    #[test]
    fn edits_a_tick_before_a_boundary_count_as_on_it() {
        let r = FrameRate::new(7353, 125);
        // a real cut from such a project: the end of a clip at frame 253
        let cut = Tick(1_092_514_075_887);
        assert_eq!(r.tick_of(253), Tick(1_092_514_075_888));
        assert_eq!(r.snap(cut), r.tick_of(252), "the plain floor lands a frame early");
        assert_eq!(r.snap_frame(cut), r.tick_of(253));
        assert_eq!(r.snap_edit(cut), r.tick_of(253));
        // a tick after a boundary is on it too
        assert_eq!(r.snap_frame(r.tick_of(253) + Tick(1)), r.tick_of(253));
        assert_eq!(r.snap_edit(r.tick_of(253) + Tick(1)), r.tick_of(253));
        for rate in [r, FrameRate::from_f64(37.516), FrameRate::FPS_29_97, FrameRate::FPS_24, FrameRate::FPS_60] {
            let d = rate.frame_duration();
            for f in [0i64, 1, 7, 253, 11_761] {
                let b = rate.tick_of(f);
                // boundaries are fixed points
                assert_eq!(rate.snap_frame(b), b, "{rate} frame {f}");
                assert_eq!(rate.snap_edit(b), b, "{rate} frame {f}");
                // mid-frame: snap_frame keeps the frame, snap_edit goes to the next one
                let mid = b + Tick(d.0 / 2);
                assert_eq!(rate.snap_frame(mid), b, "{rate} mid frame {f}");
                assert_eq!(rate.snap_edit(mid), rate.tick_of(f + 1), "{rate} mid frame {f}");
                // the last tick of a frame is "on" the next boundary
                let last = rate.tick_of(f + 1) - Tick(1);
                assert_eq!(rate.snap_frame(last), rate.tick_of(f + 1), "{rate} last tick of {f}");
            }
        }
        // hostile values never panic
        for t in [i64::MIN, -1, 0, 1, i64::MAX] {
            for rate in [r, FrameRate { num: 0, den: 0 }, FrameRate { num: -1, den: 7 }] {
                let _ = rate.snap_frame(Tick(t));
                let _ = rate.snap_edit(Tick(t));
            }
        }
    }

    #[test]
    fn exotic_frame_rate_durations() {
        // frame_duration uses floor (not round) so that `end - frame_duration()`
        // lands on the previous frame, not two frames back, for exotic rates.
        let r = FrameRate::from_f64(37.516);
        // 254_016_000_000 * 250 / 9379 = 6_770_871_094.999… → floor = 6_770_871_094
        assert_eq!(r.frame_duration().0, 6_770_871_094);

        // Standard rates must remain exact (unchanged).
        assert_eq!(FrameRate::FPS_23_976.frame_duration().0, 10_594_584_000);
        assert_eq!(FrameRate::FPS_24.frame_duration().0, 10_584_000_000);
        assert_eq!(FrameRate::FPS_25.frame_duration().0, 10_160_640_000);
        assert_eq!(FrameRate::FPS_29_97.frame_duration().0, 8_475_667_200);
        assert_eq!(FrameRate::FPS_30.frame_duration().0, 8_467_200_000);
        assert_eq!(FrameRate::FPS_60.frame_duration().0, 4_233_600_000);
    }

    proptest! {
        #[test]
        fn frame_tick_roundtrip(f in -1_000_000i64..10_000_000, ri in 0usize..11) {
            let r = FrameRate::COMMON[ri];
            prop_assert_eq!(r.frame_at(r.tick_of(f)), f);
            prop_assert_eq!(r.frame_at(r.tick_of(f) + r.frame_duration() - Tick(1)), f);
        }

        #[test]
        fn exotic_frame_tick_roundtrip(f in -100_000i64..100_000, num in 1u64..200_000, den in 1u64..2_000) {
            // Skip rates that divide evenly (those are covered by COMMON above).
            let r = FrameRate::new(num as i64, den as i64);
            if (TICKS_PER_SECOND as i128 * r.den as i128) % r.num as i128 == 0 {
                return Ok(());
            }
            prop_assert_eq!(r.frame_at(r.tick_of(f)), f, "rate={}/{}, f={}", r.num, r.den, f);
        }

        #[test]
        fn drop_frame_bijection(f in 0i64..(24 * 107_892)) {
            let r = FrameRate::FPS_29_97;
            let (_, h, m, s, fr) = frames_to_fields(f, r, true);
            // dropped labels never appear
            prop_assert!(!(s == 0 && fr < 2 && m % 10 != 0));
            prop_assert_eq!(fields_to_frames(h, m, s, fr, r, true), f);
            let txt = format_timecode_frames(f, r, true);
            prop_assert_eq!(parse_timecode(&txt, r, true, 0).unwrap(), f);
        }

        #[test]
        fn ndf_parse_roundtrip(f in 0i64..10_000_000, ri in 0usize..11) {
            let r = FrameRate::COMMON[ri];
            let txt = format_timecode_frames(f, r, false);
            prop_assert_eq!(parse_timecode(&txt, r, false, 0).unwrap(), f);
        }
    }
}
