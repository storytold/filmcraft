//! The connected displays as the dock wants them: desktop points, sanitized.
//!
//! eframe hides winit's monitor list and the egui fork reports none, so the list comes from
//! the host's `HostHooks::display_list` (`filmcraft_platform::monitors`, read-only OS calls, polled every couple of seconds so a monitor
//! plugged or unplugged mid-session is noticed). craft-ui uses it to keep floating panel windows
//! reachable: a window saved on a monitor that is gone comes back on the main window's monitor.
//! Everything the OS reports is treated as hostile (non-finite DPI, empty or enormous sizes,
//! hundreds of displays).

use craft_ui::docking::Monitor;
/// One display, in physical pixels on the virtual desktop, as the host reports it.
#[derive(Clone, Debug, PartialEq)]
pub struct Display {
    /// The device name the OS gives it, if any.
    pub name: Option<String>,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    /// Effective DPI divided by 96 (1.0 = 100 %).
    pub dpi_scale: f32,
}

/// The host's way to list the connected displays (empty where it cannot).
pub type DisplayList = dyn Fn() -> Vec<Display>;

/// How often the OS is asked which displays exist, in seconds.
const POLL_SECS: f64 = 2.0;
/// Displays kept; a real desktop has a handful.
const MAX_MONITORS: usize = 16;
/// The largest side of a display kept, in points. Larger is a bad report.
const MAX_SIDE: f32 = 32_768.0;
/// The largest distance from the origin kept, in points.
const MAX_COORD: f32 = 1_000_000.0;

/// The displays last read from the OS, in physical pixels.
#[derive(Default)]
pub(crate) struct Monitors {
    displays: Vec<Display>,
    polled_at: Option<f64>,
}

impl Monitors {
    /// Ask the OS again when the last answer is older than [`POLL_SECS`] (`now` in seconds).
    pub(crate) fn refresh(&mut self, now: f64, list: Option<&DisplayList>) {
        if self.polled_at.is_some_and(|at| now.is_finite() && now >= at && now - at < POLL_SECS) {
            return;
        }
        self.polled_at = Some(now);
        self.displays = list.map(|list| list()).unwrap_or_default();
    }

    /// The displays in desktop points at `pixels_per_point` (the main window's, the unit its own
    /// position is reported in).
    pub(crate) fn points(&self, pixels_per_point: f32) -> Vec<Monitor> {
        to_points(&self.displays, pixels_per_point)
    }

    /// The raw displays, for the control channel.
    pub(crate) fn displays(&self) -> &[Display] {
        &self.displays
    }
}

/// `displays` as dock monitors. Entries with an unusable size or position are dropped, so the
/// result is empty (and the dock keeps its fallback) when the OS reports nothing sensible.
pub(crate) fn to_points(displays: &[Display], pixels_per_point: f32) -> Vec<Monitor> {
    let ppp = if pixels_per_point.is_finite() && pixels_per_point > 0.0 { pixels_per_point } else { 1.0 };
    displays
        .iter()
        .filter_map(|d| {
            let rect = [d.x as f32 / ppp, d.y as f32 / ppp, d.width as f32 / ppp, d.height as f32 / ppp];
            let sane = rect.iter().all(|v| v.is_finite())
                && rect[2] >= 1.0
                && rect[3] >= 1.0
                && rect[2] <= MAX_SIDE
                && rect[3] <= MAX_SIDE
                && rect[0].abs() <= MAX_COORD
                && rect[1].abs() <= MAX_COORD;
            sane.then(|| Monitor { name: d.name.clone(), rect, scale: ppp })
        })
        .take(MAX_MONITORS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display(x: i32, y: i32, width: i32, height: i32) -> Display {
        Display { name: Some("D".into()), x, y, width, height, dpi_scale: 1.0 }
    }

    #[test]
    fn displays_become_points_at_the_main_windows_scale() {
        let out = to_points(&[display(0, 0, 3840, 2160), display(-1920, 0, 1920, 1080)], 2.0);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].rect, [0.0, 0.0, 1920.0, 1080.0]);
        assert_eq!(out[1].rect, [-960.0, 0.0, 960.0, 540.0]);
        assert_eq!(out[0].scale, 2.0);
    }

    #[test]
    fn hostile_reports_are_dropped_not_trusted() {
        let bad = [
            display(0, 0, 0, 1080),
            display(0, 0, 1920, -5),
            display(0, 0, i32::MAX, i32::MAX),
            display(i32::MIN, i32::MIN, 100, 100),
            display(i32::MAX, 0, 100, 100),
        ];
        assert!(to_points(&bad, 1.0).is_empty());
        let many: Vec<Display> = (0..500).map(|i| display(i * 10, 0, 10, 10)).collect();
        assert_eq!(to_points(&many, 1.0).len(), MAX_MONITORS);
    }

    #[test]
    fn a_bad_scale_falls_back_to_one() {
        for ppp in [0.0, -2.0, f32::NAN, f32::INFINITY] {
            let out = to_points(&[display(0, 0, 1920, 1080)], ppp);
            assert_eq!(out.first().map(|m| m.rect), Some([0.0, 0.0, 1920.0, 1080.0]), "{ppp}");
        }
    }

    #[test]
    fn polling_is_throttled_and_survives_a_backwards_clock() {
        let mut m = Monitors::default();
        m.refresh(10.0, None);
        let at = m.polled_at;
        m.refresh(10.5, None);
        assert_eq!(m.polled_at, at);
        m.refresh(5.0, None);
        assert_eq!(m.polled_at, Some(5.0));
        m.refresh(f64::NAN, None);
        assert!(m.polled_at.is_some_and(|t| t.is_nan()));
    }
}
