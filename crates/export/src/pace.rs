//! Estimated time left of a job, from how fast it advanced over the last seconds.
//!
//! A job's `done` counter goes up in batches, and the speed of an export changes with what is on the
//! timeline (an intro, a banner, plain camera footage), so the estimate uses the speed of a trailing
//! window, not the average since the start, and it stays quiet until a second of readings shows
//! progress. The speed is measured between the times the count moved (a batch landed), and the
//! estimate counts down from the last of them, so it does not swing up while a batch is being made
//! and down when it lands. A batch that is well overdue shows as a falling speed, so a stalled job's
//! estimate grows instead of freezing.

use std::collections::VecDeque;
use std::time::Duration;

use web_time::Instant;

/// How far back the speed is measured.
const WINDOW: Duration = Duration::from_secs(15);
/// Readings closer together than this are one reading (the UI asks on every frame it draws).
const MIN_GAP: Duration = Duration::from_millis(100);
/// The estimate needs at least this much time between its first and last reading.
const MIN_SPAN: Duration = Duration::from_secs(1);
/// A batch this many times the mean interval between batches late means the job slowed down.
const LATE: u32 = 3;
/// The longest estimate given: a job that all but stopped would otherwise promise millennia.
const MAX_ETA: Duration = Duration::from_secs(100 * 3600);

/// Readings of a job's `done` counter over time.
#[derive(Default)]
pub(crate) struct Pace {
    /// (when, `done` then), oldest first.
    readings: VecDeque<(Instant, u64)>,
}

impl Pace {
    /// Note that the job had done `done` units at `now`.
    pub(crate) fn observe(&mut self, now: Instant, done: u64) {
        // a count that went back (a job that started over) starts the history again
        if self.readings.back().is_some_and(|(_, d)| done < *d) {
            self.readings.clear();
        }
        if self.readings.back().is_none_or(|(t, _)| now.saturating_duration_since(*t) >= MIN_GAP) {
            self.readings.push_back((now, done));
        }
        // keep the newest reading that is a whole window old as the baseline, and nothing before it
        while self.readings.len() > 2 && self.readings.get(1).is_some_and(|(t, _)| now.saturating_duration_since(*t) >= WINDOW) {
            self.readings.pop_front();
        }
        // an idle start (a loudness pass, a slow seek) is not the job's speed: measure from the last
        // reading before it moved, not from a baseline that makes the job look slow for a whole window
        if let Some(first) = self.readings.front().map(|r| r.1)
            && let Some(moved) = self.readings.iter().position(|(_, d)| *d != first)
            && moved >= 2
            && self.readings.get(moved - 1).is_some_and(|(t, _)| t.saturating_duration_since(self.readings[0].0) >= MIN_SPAN)
        {
            self.readings.drain(..moved - 1);
        }
    }

    /// The time `remaining` more units take at the pace of the readings, when they show one.
    pub(crate) fn eta(&self, remaining: u64) -> Option<Duration> {
        let (t0, d0) = *self.readings.front()?;
        let (t1, d1) = *self.readings.back()?;
        let span = t1.saturating_duration_since(t0);
        if span < MIN_SPAN || d1 <= d0 {
            return None;
        }
        // on time: the speed between batches, counting down since the last one landed
        let countdown = self
            .batches()
            .filter(|(_, gap, since)| *since <= gap.saturating_mul(LATE))
            .map(|(per_second, _, since)| remaining as f64 / per_second - since.as_secs_f64());
        let left = match countdown {
            Some(left) if left > 0.0 => left,
            // a batch is overdue, the countdown ran out with work left, or the count moves too
            // seldom to tell: the speed up to now
            _ => remaining as f64 / ((d1 - d0) as f64 / span.as_secs_f64()),
        };
        // `try_from_secs_f64` refuses what a `Duration` cannot hold; that is "more than the cap" here
        (left >= 0.0).then(|| Duration::try_from_secs_f64(left).map_or(MAX_ETA, |d| d.min(MAX_ETA)))
    }
}

impl Pace {
    /// The speed between the first and the last time the count moved in the window, the mean time
    /// between those moves, and the time since the last one; `None` before two moves a second apart.
    fn batches(&self) -> Option<(f64, Duration, Duration)> {
        let now = self.readings.back()?.0;
        let mut moves = self.readings.iter().zip(self.readings.iter().skip(1)).filter(|(a, b)| b.1 != a.1).map(|(_, b)| *b);
        let (ta, da) = moves.next()?;
        let (n, (tb, db)) = moves.fold((0u32, (ta, da)), |(n, _), m| (n.saturating_add(1), m));
        let span = tb.saturating_duration_since(ta);
        if span < MIN_SPAN || db <= da {
            return None;
        }
        Some(((db - da) as f64 / span.as_secs_f64(), span.checked_div(n)?, now.saturating_duration_since(tb)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(t0: Instant, secs: f64) -> Instant {
        t0 + Duration::from_secs_f64(secs)
    }

    /// Feed `readings` (seconds, done) and ask for the time left of `total`.
    fn eta(readings: &[(f64, u64)], total: u64) -> Option<f64> {
        let t0 = Instant::now();
        let mut p = Pace::default();
        for (s, d) in readings {
            p.observe(at(t0, *s), *d);
        }
        let done = readings.last().map_or(0, |r| r.1);
        p.eta(total.saturating_sub(done)).map(|d| d.as_secs_f64())
    }

    /// A job at `fps` units per second, read every `step` seconds for `secs`.
    fn steady(fps: f64, step: f64, secs: f64) -> Vec<(f64, u64)> {
        (0..=(secs / step) as u64).map(|i| (i as f64 * step, (i as f64 * step * fps) as u64)).collect()
    }

    #[test]
    fn a_steady_job_gives_the_time_it_has_left() {
        // 50 units a second, 4 s in, of 1000: 800 units left = 16 s
        let e = eta(&steady(50.0, 0.15, 4.0), 1000).unwrap();
        assert!((e - 16.0).abs() < 0.5, "{e}");
        // and the estimate falls as the job goes on: 10 s in, 500 left
        let e = eta(&steady(50.0, 0.15, 10.0), 1000).unwrap();
        assert!((e - 10.0).abs() < 0.5, "{e}");
    }

    #[test]
    fn nothing_is_promised_before_there_is_progress_to_measure() {
        assert_eq!(eta(&[], 100), None);
        assert_eq!(eta(&[(0.0, 0)], 100), None, "one reading");
        assert_eq!(eta(&[(0.0, 0), (0.5, 20)], 100), None, "less than a second of readings");
        assert_eq!(eta(&[(0.0, 5), (2.0, 5)], 100), None, "nothing done in two seconds (a loudness pass, a seek)");
        // then, once frames flow
        assert!(eta(&[(0.0, 0), (2.0, 0), (3.0, 30)], 100).is_some());
    }

    #[test]
    fn the_estimate_follows_a_change_of_speed_within_the_window() {
        // 100 units a second for 20 s, then 20 a second for 20 s (a banner comes in): the last
        // 15 s dominate, so with 10 000 units to do the estimate is near the slow speed's
        let mut r = steady(100.0, 0.15, 20.0);
        let (t, d) = *r.last().unwrap();
        r.extend((1..=133).map(|i| (t + i as f64 * 0.15, d + (i as f64 * 0.15 * 20.0) as u64)));
        let done = r.last().unwrap().1;
        let e = eta(&r, 10_000).unwrap();
        let slow = (10_000 - done) as f64 / 20.0;
        let fast = (10_000 - done) as f64 / 100.0;
        assert!((e - slow).abs() < (e - fast).abs(), "estimate {e}, slow {slow}, fast {fast}");
    }

    #[test]
    fn an_idle_start_does_not_hold_the_estimate_back() {
        // 20 s with nothing done (a loudness pass), then 50 units a second for 3 s: the pace is 50 a
        // second, not what a 15 s window that is mostly idle would say
        let mut r: Vec<(f64, u64)> = (0..=133).map(|i| (i as f64 * 0.15, 0)).collect();
        let t = r.last().unwrap().0;
        r.extend((1..=20).map(|i| (t + i as f64 * 0.15, (i as f64 * 0.15 * 50.0) as u64)));
        let e = eta(&r, 1000).unwrap();
        let done = r.last().unwrap().1;
        assert!((e - (1000 - done) as f64 / 50.0).abs() < 1.0, "{e}");
        // the short flat spells between batches of a running job are not an idle start
        let batches: Vec<(f64, u64)> = (0..=40).map(|i| (i as f64 * 0.15, (i / 2) as u64 * 8)).collect();
        assert!(eta(&batches, 1000).is_some());
    }

    #[test]
    fn a_stall_makes_the_estimate_grow() {
        let mut r = steady(50.0, 0.15, 4.0);
        let before = eta(&r, 1000).unwrap();
        let (t, d) = *r.last().unwrap();
        r.extend((1..=40).map(|i| (t + i as f64 * 0.15, d)));
        let after = eta(&r, 1000).unwrap();
        assert!(after > before * 1.5, "{before} -> {after}");
    }

    /// A job that finishes 16 units every 2 s (8 a second, as an export's batches of frames), read
    /// every 0.1 s for `secs`; the estimate at each reading, with the time of the reading.
    fn batched(secs: f64, total: u64) -> Vec<(f64, Option<f64>)> {
        let t0 = Instant::now();
        let mut p = Pace::default();
        (0..=(secs * 10.0) as u64)
            .map(|i| {
                let s = i as f64 * 0.1;
                let done = (i / 20) * 16;
                p.observe(at(t0, s), done);
                (s, p.eta(total.saturating_sub(done)).map(|d| d.as_secs_f64()))
            })
            .collect()
    }

    #[test]
    fn the_estimate_counts_down_between_batches_instead_of_jumping() {
        // 20 000 units at 8 a second end at 2500 s: once the window is full, each estimate plus the
        // time it was made says so, at every reading, not only when a batch lands. Measured to the
        // end of the window it swung by a batch (16 of ~120 units), ~13 %.
        let ends: Vec<f64> = batched(40.0, 20_000).into_iter().filter(|r| r.0 >= 20.0).map(|(s, e)| s + e.unwrap_or(f64::NAN)).collect();
        let (lo, hi) = ends.iter().fold((f64::MAX, f64::MIN), |(lo, hi), e| (lo.min(*e), hi.max(*e)));
        assert!(hi - lo < 5.0 && (lo - 2500.0).abs() < 5.0, "the end moved between {lo} and {hi}");
    }

    #[test]
    fn the_last_batch_never_shows_nothing_left_before_it_lands() {
        // batches of 16 every 2 s; at 20 s 160 of 176 are done, and the last batch is a little late
        let t0 = Instant::now();
        let mut p = Pace::default();
        for i in 0..=200u64 {
            p.observe(at(t0, i as f64 * 0.1), (i / 20) * 16);
        }
        for i in 201..=260u64 {
            p.observe(at(t0, i as f64 * 0.1), 160);
            let left = p.eta(16).map(|d| d.as_secs_f64());
            // the countdown runs out at 22 s; until the batch was overdue at 26 s it said 0
            assert!(left.is_some_and(|l| l > 0.0), "{:.1} s in: {left:?} left of 16 units", i as f64 * 0.1);
        }
    }

    #[test]
    fn a_late_batch_makes_the_estimate_grow() {
        // batches every 2 s for 20 s, then 8 s with nothing done (four batch intervals)
        let t0 = Instant::now();
        let mut p = Pace::default();
        for i in 0..=200u64 {
            p.observe(at(t0, i as f64 * 0.1), (i / 20) * 16);
        }
        let before = p.eta(1000 - 160).unwrap();
        for i in 201..=280u64 {
            p.observe(at(t0, i as f64 * 0.1), 160);
        }
        let after = p.eta(1000 - 160).unwrap();
        assert!(after > before, "{before:?} -> {after:?}");
    }

    #[test]
    fn a_count_that_goes_back_starts_over() {
        // a second pass, or a job reusing its counters: the old history must not count
        let mut r = steady(50.0, 0.15, 4.0);
        let (t, _) = *r.last().unwrap();
        r.extend((1..=20).map(|i| (t + i as f64 * 0.15, (i as f64 * 0.15 * 10.0) as u64)));
        let e = eta(&r, 100).unwrap();
        let done = r.last().unwrap().1;
        assert!((e - (100 - done) as f64 / 10.0).abs() < 1.0, "{e}");
    }

    #[test]
    fn readings_that_arrive_together_count_once_and_the_history_stays_small() {
        let t0 = Instant::now();
        let mut p = Pace::default();
        for _ in 0..1000 {
            p.observe(t0, 1);
        }
        assert_eq!(p.readings.len(), 1);
        // an hour of 150 ms readings keeps about a window of them
        for i in 0..24_000u64 {
            p.observe(at(t0, 1.0 + i as f64 * 0.15), 1 + i);
        }
        assert!(p.readings.len() <= (WINDOW.as_millis() / MIN_GAP.as_millis()) as usize + 3, "{}", p.readings.len());
    }

    #[test]
    fn a_job_reports_its_time_left_until_it_is_done() {
        use std::sync::atomic::Ordering;
        let t0 = Instant::now();
        let progress = crate::Progress::default();
        // no total yet, nothing to estimate
        assert_eq!(progress.eta_at(t0), None);
        progress.total.store(1000, Ordering::Relaxed);
        let mut last = None;
        for i in 0..=60u64 {
            progress.done.store(i * 5, Ordering::Relaxed); // 50 units a second, read every 0.1 s
            last = progress.eta_at(at(t0, i as f64 * 0.1));
        }
        // 6 s in, 300 done, 700 left at 50 a second
        let left = last.expect("a second of readings shows progress").as_secs_f64();
        assert!((left - 14.0).abs() < 0.5, "{left}");
        // done: nothing left to wait for
        progress.done.store(1000, Ordering::Relaxed);
        assert_eq!(progress.eta_at(at(t0, 7.0)), None);
        // a finished job (cancelled, failed) has none either, whatever it had left
        let other = crate::Progress::default();
        other.total.store(1000, Ordering::Relaxed);
        for i in 0..=30u64 {
            other.done.store(i * 5, Ordering::Relaxed);
            other.eta_at(at(t0, i as f64 * 0.1));
        }
        assert!(other.eta_at(at(t0, 3.1)).is_some());
        other.finished.store(true, Ordering::Relaxed);
        assert_eq!(other.eta_at(at(t0, 3.2)), None);
    }

    #[test]
    fn extreme_numbers_give_a_capped_estimate_or_none_and_never_panic() {
        // almost no progress against a huge total: capped
        let e = eta(&[(0.0, 0), (10.0, 1)], u64::MAX).unwrap();
        assert!((e - MAX_ETA.as_secs_f64()).abs() < 1.0, "{e}");
        // huge counters
        assert!(eta(&[(0.0, u64::MAX - 100), (2.0, u64::MAX - 50)], u64::MAX).is_some());
        // a clock that does not move forward: no estimate, no division by zero
        assert_eq!(eta(&[(1.0, 0), (1.0, 50), (1.0, 100)], 1000), None);
        // nothing left
        let t0 = Instant::now();
        let mut p = Pace::default();
        p.observe(t0, 0);
        p.observe(at(t0, 2.0), 100);
        assert_eq!(p.eta(0), Some(Duration::ZERO));
    }
}
