//! Zooming the Timeline about a pivot: the wheel (or a pinch) zooms in and out while the time under
//! the pointer, or under the playhead, stays at the same screen x.
//!
//! Plain numbers, no egui, so the maths is unit-tested without a window. Every input may be hostile
//! (a script's wheel, a damaged state): nothing here panics or divides by zero, and
//! [`zoom_at_cursor`], [`pivot_x`] and [`wheel_zoom_factor`] only return finite numbers.
//!
//! The view is `scroll` (the time, in seconds, at the left edge of the track area) and `pps`
//! (points a second). With the scroll counted in points, `x_scroll = scroll * pps`, the rules are
//!
//! ```text
//! time_under_pivot = (x_scroll + x) / old_pps
//! new_x_scroll     = time_under_pivot * new_pps - x      (never below 0)
//! ```
//!
//! which in seconds is `new_scroll = time_under_pivot - x / new_pps`.

/// The smallest zoom the Timeline reaches, in points a second.
pub const MIN_PPS: f64 = 0.05;
/// The largest zoom the Timeline reaches, in points a second.
pub const MAX_PPS: f64 = 24_000.0;

/// What one point of wheel turns into: a 40-point wheel notch (one line) zooms by a quarter, and the
/// opposite turn undoes it exactly (the factor is an exponential, so zooming in and out is symmetric).
const WHEEL_ZOOM_PER_POINT: f64 = 0.005_578_59;
/// One batch of wheel events never zooms by more than this factor (or less than its inverse).
const MAX_STEP: f64 = 2.0;

/// The zoom factor of `delta` points of wheel (positive = zoom in, as the wheel turned up).
pub fn wheel_zoom_factor(delta: f64) -> f64 {
    if !delta.is_finite() {
        return 1.0;
    }
    (delta * WHEEL_ZOOM_PER_POINT).exp().clamp(1.0 / MAX_STEP, MAX_STEP)
}

/// How the Timeline is scaled and scrolled.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scale {
    /// Points a second.
    pub pps: f64,
    /// The time at the left edge of the track area, in seconds.
    pub scroll: f64,
}

impl Scale {
    /// The time at `x` points from the left edge of the track area.
    pub fn time_at(self, x: f64) -> f64 {
        if valid(self.pps) { self.scroll + x / self.pps } else { 0.0 }
    }
}

/// The view after a zoom.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Zoomed {
    /// The new zoom, in points a second (within the limits).
    pub pps: f64,
    /// The new time at the left edge (never below 0).
    pub scroll: f64,
    /// The time that was under the pivot before the zoom, and stays under it.
    pub anchor_time: f64,
}

/// Zoom by `factor` about the point `cursor_x` points from the left edge of the track area (the
/// pointer, or the playhead: see [`pivot_x`]), so the time under it stays under it.
///
/// `shown` is the view on screen, where the time under the pivot is read from. `target_pps` is the
/// zoom the view is animating toward: successive wheel notches multiply it, so a fast spin adds up
/// before the view has caught up. Pass the same zoom twice when the view is at rest. The result
/// stays within `min_zoom..=max_zoom` (given in either order), and its scroll is never negative: at
/// the start of the Timeline the pivot slides instead of the view scrolling before time zero.
pub fn zoom_at_cursor(shown: Scale, target_pps: f64, cursor_x: f64, factor: f64, min_zoom: f64, max_zoom: f64) -> Zoomed {
    let (lo, hi) = limits(min_zoom, max_zoom);
    let base = if valid(target_pps) {
        target_pps
    } else if valid(shown.pps) {
        shown.pps
    } else {
        lo
    };
    let factor = if valid(factor) { factor } else { 1.0 };
    let pps = (base * factor).min(hi).max(lo);

    let x = if cursor_x.is_finite() { cursor_x } else { 0.0 };
    let scroll = if shown.scroll.is_finite() { shown.scroll.max(0.0) } else { 0.0 };
    let time = Scale { pps: shown.pps, scroll }.time_at(x);
    let anchor_time = if time.is_finite() { time } else { scroll };

    let new_scroll = (anchor_time - x / pps).max(0.0);
    Zoomed { pps, scroll: if new_scroll.is_finite() { new_scroll } else { 0.0 }, anchor_time }
}

/// Where a zoom pivots, in points from the left edge of the track area `width` points wide: the
/// playhead at `playhead_x` when the setting asks for it (`Some`) and it is in view, otherwise the
/// pointer. A playhead out of view cannot hold the pivot (the picture would slide away from under
/// the pointer), so the zoom falls back to the pointer.
pub fn pivot_x(cursor_x: f64, playhead_x: Option<f64>, width: f64) -> f64 {
    match playhead_x {
        Some(p) if p.is_finite() && width.is_finite() && p >= 0.0 && p <= width => p,
        _ if cursor_x.is_finite() => cursor_x,
        _ => 0.0,
    }
}

/// A usable positive number.
fn valid(v: f64) -> bool {
    v.is_finite() && v > 0.0
}

/// The zoom limits as (smallest, largest): swapped when given the wrong way round, the Timeline's
/// own when they are not usable numbers.
fn limits(min_zoom: f64, max_zoom: f64) -> (f64, f64) {
    if valid(min_zoom) && valid(max_zoom) { (min_zoom.min(max_zoom), min_zoom.max(max_zoom)) } else { (MIN_PPS, MAX_PPS) }
}

/// Zoom a normalized `0.0..=1.0` time range `current` (`None` = `0.0..=1.0`) by `factor` (> 1 zooms in,
/// < 1 zooms out) around `cursor_u` (`0.0..=1.0` across the visible bar) or `playhead_f` (`0.0..=1.0`
/// across the whole clip/sequence when the playhead anchor is active). If the playhead is within the
/// visible `[s, e]` window, its screen position holds the pivot; otherwise `cursor_u` holds it.
/// Returns `None` when the zoomed range covers the full `0.0..=1.0` extent.
pub fn zoom_fraction_range(current: Option<[f64; 2]>, cursor_u: f64, playhead_f: Option<f64>, factor: f64, min_span: f64) -> Option<[f64; 2]> {
    let (s, e) = match current {
        Some([a, b]) if a.is_finite() && b.is_finite() && a >= 0.0 && b <= 1.0 && a < b => (a, b),
        _ => (0.0, 1.0),
    };
    let span = (e - s).clamp(1e-9, 1.0);
    let min_span = if min_span.is_finite() && min_span > 0.0 { min_span.clamp(1e-6, 1.0) } else { 1e-4 };
    let factor = if valid(factor) { factor } else { 1.0 };
    let new_span = (span / factor).clamp(min_span, 1.0);
    if new_span >= 1.0 - 1e-6 {
        return None;
    }
    let cursor_u = if cursor_u.is_finite() { cursor_u.clamp(0.0, 1.0) } else { 0.5 };
    let u = match playhead_f {
        Some(pf) if pf.is_finite() && pf >= s - 1e-9 && pf <= e + 1e-9 => ((pf - s) / span).clamp(0.0, 1.0),
        _ => cursor_u,
    };
    let anchor_f = (s + u * span).clamp(0.0, 1.0);
    let new_s = (anchor_f - u * new_span).clamp(0.0, (1.0 - new_span).max(0.0));
    let new_e = (new_s + new_span).min(1.0);
    Some([new_s, new_e])
}

/// Pan a normalized `0.0..=1.0` time range `current` horizontally by `delta_points` on a bar
/// `width` points wide. Returns `None` when not zoomed in.
pub fn pan_fraction_range(current: Option<[f64; 2]>, delta_points: f64, width: f64) -> Option<[f64; 2]> {
    let [s, e] = current?;
    if !(s.is_finite() && e.is_finite() && s >= 0.0 && e <= 1.0 && s < e) {
        return None;
    }
    let span = e - s;
    if span >= 1.0 - 1e-6 || !(width.is_finite() && width > 1.0 && delta_points.is_finite()) {
        return current;
    }
    let df = -delta_points / width * span;
    let new_s = (s + df).clamp(0.0, (1.0 - span).max(0.0));
    Some([new_s, (new_s + span).min(1.0)])
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f64 = 1e-9;

    fn rest(pps: f64, scroll: f64) -> Scale {
        Scale { pps, scroll }
    }

    /// Zoom a view at rest by a wheel of `delta` points.
    fn wheel(view: Scale, x: f64, delta: f64) -> Zoomed {
        zoom_at_cursor(view, view.pps, x, wheel_zoom_factor(delta), MIN_PPS, MAX_PPS)
    }

    #[test]
    fn the_time_under_the_cursor_does_not_move() {
        let mut checked = 0;
        for scroll in [0.0, 3.0, 120.5, 4000.0] {
            for pps in [0.5, 50.0, 1000.0, 20_000.0] {
                for x in [0.0, 1.0, 333.3, 1599.0] {
                    for delta in [-400.0, -80.0, -20.0, 5.0, 20.0, 40.0, 200.0] {
                        let view = rest(pps, scroll);
                        let before = view.time_at(x);
                        let z = wheel(view, x, delta);
                        let after = Scale { pps: z.pps, scroll: z.scroll }.time_at(x);
                        assert_eq!(z.anchor_time, before, "the time read under the cursor is the one before the zoom");
                        // a scroll pinned at 0 is the one case where the time may move (see the next test)
                        if z.scroll > 0.0 {
                            assert!(
                                (after - before).abs() <= EPS * before.abs().max(1.0),
                                "scroll {scroll} pps {pps} x {x} delta {delta}: {before} became {after}"
                            );
                            checked += 1;
                        }
                    }
                }
            }
        }
        assert!(checked > 200, "the sweep must mostly leave the scroll off 0, or it proves nothing: {checked}");
    }

    #[test]
    fn it_is_the_rule_in_points() {
        // time = (x_scroll + x) / old_pps ; new_x_scroll = time * new_pps - x
        let (pps, scroll, x) = (80.0, 12.0, 420.0);
        let z = zoom_at_cursor(rest(pps, scroll), pps, x, 1.5, MIN_PPS, MAX_PPS);
        let time = (scroll * pps + x) / pps;
        let new_x_scroll = time * z.pps - x;
        assert_eq!(z.pps, 120.0);
        assert!((z.scroll * z.pps - new_x_scroll).abs() < EPS * new_x_scroll.abs(), "{} points vs {new_x_scroll}", z.scroll * z.pps);
        assert!((z.anchor_time - 17.25).abs() < EPS);
    }

    #[test]
    fn zooming_out_at_the_start_cannot_scroll_before_time_zero() {
        // 0.9 s sits 50 points in; at 1 point a second the start would have to be 49.1 s before 0
        let z = zoom_at_cursor(rest(100.0, 0.4), 100.0, 50.0, 0.01, MIN_PPS, MAX_PPS);
        assert!((z.pps - 1.0).abs() < 1e-12, "{z:?}");
        assert_eq!(z.scroll, 0.0);
        for scroll in [0.0, 0.2, 5.0] {
            for x in [0.0, 10.0, 900.0] {
                for factor in [0.5, 0.9, 1.0, 1.1, 2.0] {
                    let z = zoom_at_cursor(rest(40.0, scroll), 40.0, x, factor, MIN_PPS, MAX_PPS);
                    assert!(z.scroll >= 0.0 && z.scroll.is_finite(), "{z:?}");
                }
            }
        }
    }

    #[test]
    fn the_zoom_stays_within_its_limits() {
        let at = |pps: f64, factor: f64, lo: f64, hi: f64| zoom_at_cursor(rest(pps, 7.0), pps, 200.0, factor, lo, hi);
        // the Timeline's own limits
        assert_eq!(at(100.0, 1e9, MIN_PPS, MAX_PPS).pps, MAX_PPS);
        assert_eq!(at(100.0, 1e-9, MIN_PPS, MAX_PPS).pps, MIN_PPS);
        // other limits, given in either order
        assert_eq!(at(4.0, 10.0, 2.0, 8.0).pps, 8.0);
        assert_eq!(at(4.0, 0.01, 2.0, 8.0).pps, 2.0);
        assert_eq!(at(4.0, 10.0, 8.0, 2.0).pps, 8.0, "crossed limits are swapped, not a panic");
        assert_eq!(at(4.0, 1.5, 8.0, 2.0).pps, 6.0);
        // already at a limit: the zoom stays and the picture does not slide
        let z = at(MAX_PPS, 2.0, MIN_PPS, MAX_PPS);
        assert_eq!(z.pps, MAX_PPS);
        assert!((z.scroll - 7.0).abs() < EPS, "{z:?}");
        let z = at(MIN_PPS, 0.5, MIN_PPS, MAX_PPS);
        assert_eq!(z.pps, MIN_PPS);
        assert!((z.scroll - 7.0).abs() < EPS, "{z:?}");
    }

    #[test]
    fn a_view_still_catching_up_keeps_adding_up() {
        // on screen: 50 pps scrolled to 10 s; the zoom is already heading for 80 pps
        let shown = rest(50.0, 10.0);
        let z = zoom_at_cursor(shown, 80.0, 300.0, 1.25, MIN_PPS, MAX_PPS);
        assert_eq!(z.pps, 100.0, "the notch multiplies where the zoom is going, not where it is");
        assert_eq!(z.anchor_time, shown.time_at(300.0), "the pivot is the time the pointer is over on screen");
        assert!((Scale { pps: z.pps, scroll: z.scroll }.time_at(300.0) - z.anchor_time).abs() < EPS);
    }

    #[test]
    fn zooming_in_and_out_the_same_amount_comes_back() {
        let start = rest(37.5, 21.0);
        let mut view = start;
        for _ in 0..5 {
            let z = wheel(view, 612.0, 40.0);
            view = Scale { pps: z.pps, scroll: z.scroll };
        }
        assert!((view.pps / start.pps - 1.25_f64.powi(5)).abs() < 1e-3);
        for _ in 0..5 {
            let z = wheel(view, 612.0, -40.0);
            view = Scale { pps: z.pps, scroll: z.scroll };
        }
        assert!((view.pps - start.pps).abs() < 1e-9 * start.pps, "{view:?}");
        assert!((view.scroll - start.scroll).abs() < 1e-6, "{view:?}");
    }

    #[test]
    fn a_wheel_notch_zooms_by_a_quarter_and_the_opposite_turn_undoes_it() {
        assert!((wheel_zoom_factor(40.0) - 1.25).abs() < 1e-4);
        assert!((wheel_zoom_factor(-40.0) - 0.8).abs() < 1e-4);
        for d in [1.0, 20.0, 40.0, 100.0] {
            assert!((wheel_zoom_factor(d) * wheel_zoom_factor(-d) - 1.0).abs() < 1e-12, "{d}");
        }
        assert_eq!(wheel_zoom_factor(0.0), 1.0);
        // a huge spin is one capped step
        assert_eq!(wheel_zoom_factor(1e9), MAX_STEP);
        assert_eq!(wheel_zoom_factor(-1e9), 1.0 / MAX_STEP);
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(wheel_zoom_factor(bad), 1.0);
        }
    }

    #[test]
    fn the_playhead_holds_the_pivot_only_while_it_is_in_view() {
        assert_eq!(pivot_x(100.0, None, 800.0), 100.0, "the setting asks for the pointer");
        assert_eq!(pivot_x(100.0, Some(300.0), 800.0), 300.0);
        assert_eq!(pivot_x(100.0, Some(0.0), 800.0), 0.0);
        assert_eq!(pivot_x(100.0, Some(800.0), 800.0), 800.0);
        for out in [-1.0, 800.5, 1e9, f64::NAN, f64::INFINITY] {
            assert_eq!(pivot_x(100.0, Some(out), 800.0), 100.0, "{out}");
        }
        assert_eq!(pivot_x(100.0, Some(300.0), f64::NAN), 100.0);
        assert_eq!(pivot_x(f64::NAN, None, 800.0), 0.0);
    }

    /// The playhead as the pivot: it stays at the same x while everything else moves around it.
    #[test]
    fn the_playhead_stays_where_it_is_on_screen() {
        let view = rest(60.0, 5.0);
        let playhead_time = 9.0;
        let x = (playhead_time - view.scroll) * view.pps;
        let pivot = pivot_x(10.0, Some(x), 1500.0);
        let z = wheel(view, pivot, 40.0);
        let moved = (playhead_time - z.scroll) * z.pps;
        assert!((moved - x).abs() < 1e-6, "the playhead went from {x} to {moved}");
    }

    #[test]
    fn hostile_numbers_do_no_harm() {
        let evil = [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0, -0.0, -1.0, 1.0, f64::MAX, f64::MIN, f64::MIN_POSITIVE, 1e-320, 1e300];
        for &a in &evil {
            for &b in &evil {
                for &c in &evil {
                    for &d in &evil {
                        let view = Scale { pps: a, scroll: b };
                        let z = zoom_at_cursor(view, c, d, a, b, c);
                        assert!(z.pps.is_finite() && z.pps > 0.0, "{view:?} {c} {d}: {z:?}");
                        assert!(z.scroll.is_finite() && z.scroll >= 0.0, "{view:?} {c} {d}: {z:?}");
                        assert!(z.anchor_time.is_finite(), "{view:?} {c} {d}: {z:?}");
                        let (lo, hi) = limits(b, c);
                        assert!(z.pps >= lo && z.pps <= hi, "{z:?} outside {lo}..{hi}");
                        assert!(pivot_x(a, Some(b), c).is_finite(), "pivot of {a} {b} {c}");
                        assert!(wheel_zoom_factor(d).is_finite() && wheel_zoom_factor(d) > 0.0, "factor of {d}");
                    }
                }
            }
        }
    }

    #[test]
    fn fraction_range_zoom_and_pan_keep_pivot_and_bounds() {
        let z1 = zoom_fraction_range(None, 0.25, None, 2.0, 0.05).unwrap();
        assert!((z1[1] - z1[0] - 0.5).abs() < 1e-9);
        assert!((z1[0] + 0.25 * (z1[1] - z1[0]) - 0.25).abs() < 1e-9);
        // Zooming back out to 1.0 returns None (full range)
        assert_eq!(zoom_fraction_range(Some(z1), 0.25, None, 0.5, 0.05), None);
        // Playhead anchor keeps playhead at same relative screen position
        let z_ph = zoom_fraction_range(None, 0.1, Some(0.6), 2.0, 0.05).unwrap();
        let u_after = (0.6 - z_ph[0]) / (z_ph[1] - z_ph[0]);
        assert!((u_after - 0.6).abs() < 1e-9);
        // Pan stays within 0.0..=1.0
        let p = pan_fraction_range(Some([0.2, 0.7]), -100.0, 500.0).unwrap();
        assert!((p[0] - 0.3).abs() < 1e-9 && (p[1] - 0.8).abs() < 1e-9);
    }
}
