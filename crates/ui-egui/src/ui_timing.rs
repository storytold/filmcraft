//! How long the UI's own work takes per frame (`logic` + `ui`), for `perf.stats.ui` and the
//! slow-frame log. The fps reading is an average over frames and hides a single long one; a long
//! frame is exactly what a user sees as a freeze (an import probing every clip, a source opened on
//! the UI thread), so each one over [`SLOW_MS`] is logged with the half of the frame it was in.

use std::time::Duration;
use web_time::Instant;

/// A frame this long is logged as a warning.
pub const SLOW_MS: f64 = 250.0;

/// The two halves of an eframe frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Half {
    /// `App::logic`: control requests, commands, playback and preroll bookkeeping.
    Logic,
    /// `App::ui`: the panels (timeline, monitors, project panel…).
    Panels,
}

/// Per-frame timing; cumulative counters, read with [`UiTiming::stats`].
#[derive(Debug, Default)]
pub struct UiTiming {
    started: Option<Instant>,
    logic: Duration,
    panels: Duration,
    frames: u64,
    over_50: u64,
    over_100: u64,
    over_250: u64,
    longest_ms: f64,
    slow_logged: u64,
    /// The panels drawn this frame and how long each took ([`UiTiming::panel`]), so a slow frame
    /// names the panel it was in.
    panel_times: Vec<(&'static str, Duration)>,
    /// All the time [`UiTiming::exclude`] has left out, so a panel that opened the modal leaves
    /// it out too ([`UiTiming::panel_done`]).
    excluded: Duration,
}

/// When a panel started drawing, for [`UiTiming::panel_done`].
pub struct PanelStart {
    at: Instant,
    excluded: Duration,
}

impl UiTiming {
    /// A half of the frame starts.
    pub fn begin(&mut self) {
        self.started = Some(Instant::now());
    }

    /// Leave `waited` out of this frame: a modal the host showed on the UI thread (the native
    /// file dialogs on Windows and macOS) while the user chose a file. The window is the dialog's
    /// meanwhile, not frozen: a 3.8 s "slow frame" next to `file.saveAs` was the Save dialog open.
    pub fn exclude(&mut self, waited: Duration) {
        if let Some(t) = self.started.as_mut()
            && let Some(later) = t.checked_add(waited)
        {
            let later = later.min(Instant::now());
            self.excluded = self.excluded.saturating_add(later.saturating_duration_since(*t));
            *t = later;
        }
    }

    /// A panel starts drawing.
    pub fn panel_start(&self) -> PanelStart {
        PanelStart { at: Instant::now(), excluded: self.excluded }
    }

    /// Panel `name`, started at `start`, is drawn: its time, less any modal it showed meanwhile.
    pub fn panel_done(&mut self, name: &'static str, start: PanelStart) {
        let waited = self.excluded.saturating_sub(start.excluded);
        self.panel(name, start.at.elapsed().saturating_sub(waited));
    }

    /// Panel `name` took `took` to draw this frame (several of one name add up).
    pub fn panel(&mut self, name: &'static str, took: Duration) {
        if let Some((_, t)) = self.panel_times.iter_mut().find(|(n, _)| *n == name) {
            *t = t.saturating_add(took);
        } else if self.panel_times.len() < 64 {
            self.panel_times.push((name, took));
        }
    }

    /// The half that [`begin`](Self::begin) started ends. Ending [`Half::Panels`] closes the frame.
    pub fn end(&mut self, half: Half) {
        let Some(t0) = self.started.take() else { return };
        let took = t0.elapsed();
        match half {
            Half::Logic => self.logic = self.logic.saturating_add(took),
            Half::Panels => {
                self.panels = self.panels.saturating_add(took);
                self.close_frame();
            }
        }
    }

    fn close_frame(&mut self) {
        let (logic, panels) = (std::mem::take(&mut self.logic), std::mem::take(&mut self.panels));
        let ms = logic.saturating_add(panels).as_secs_f64() * 1e3;
        let mut by_panel = std::mem::take(&mut self.panel_times);
        by_panel.sort_by_key(|(_, t)| std::cmp::Reverse(*t));
        self.record_with(ms, logic.as_secs_f64() * 1e3, panels.as_secs_f64() * 1e3, &by_panel);
    }

    /// One frame of `ms` (`logic_ms` + `panels_ms`).
    #[cfg(test)]
    fn record(&mut self, ms: f64, logic_ms: f64, panels_ms: f64) {
        self.record_with(ms, logic_ms, panels_ms, &[]);
    }

    /// [`Self::record`] with the panels' own times, slowest first.
    fn record_with(&mut self, ms: f64, logic_ms: f64, panels_ms: f64, by_panel: &[(&'static str, Duration)]) {
        if !ms.is_finite() || ms < 0.0 {
            return;
        }
        self.frames = self.frames.saturating_add(1);
        self.over_50 += u64::from(ms > 50.0);
        self.over_100 += u64::from(ms > 100.0);
        self.over_250 += u64::from(ms > SLOW_MS);
        self.longest_ms = self.longest_ms.max(ms);
        if ms > SLOW_MS {
            self.slow_logged = self.slow_logged.saturating_add(1);
            let half = if logic_ms >= panels_ms { "logic (commands, control requests, playback)" } else { "panels" };
            log::warn!(
                "slow UI frame: {ms:.0} ms ({half}: logic {logic_ms:.0} ms, panels {panels_ms:.0} ms{}); the window did not respond meanwhile",
                panel_breakdown(panels_ms, by_panel)
            );
        }
    }

    /// The panel that was slowest in the last slow frame, for tests.
    #[cfg(test)]
    fn last_breakdown(panels_ms: f64, by_panel: &[(&'static str, Duration)]) -> String {
        panel_breakdown(panels_ms, by_panel)
    }

    /// `perf.stats.ui` fields: frames counted, the longest, and how many went over 50 / 100 /
    /// 250 ms (cumulative: diff two readings for an interval).
    pub fn stats(&self) -> serde_json::Value {
        serde_json::json!({
            "frames": self.frames,
            "longestMs": (self.longest_ms * 10.0).round() / 10.0,
            "over50": self.over_50,
            "over100": self.over_100,
            "over250": self.over_250,
        })
    }
}

/// ` — Timeline 812 ms, Program 40 ms, other 15 ms`: the three slowest panels of a frame, and
/// what the panel half spent outside them (dialogs, menus, the status bar, the dock itself).
fn panel_breakdown(panels_ms: f64, by_panel: &[(&'static str, Duration)]) -> String {
    if by_panel.is_empty() {
        return String::new();
    }
    let mut parts: Vec<String> = by_panel.iter().take(3).map(|(n, t)| format!("{n} {:.0} ms", t.as_secs_f64() * 1e3)).collect();
    let inside: f64 = by_panel.iter().map(|(_, t)| t.as_secs_f64() * 1e3).sum();
    let other = panels_ms - inside;
    if other >= 1.0 {
        parts.push(format!("other {other:.0} ms"));
    }
    format!(" — {}", parts.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_modal_a_panel_opened_is_left_out_of_its_time() {
        // the frame left the wait out but the panel kept it, so the log named the panel as slow
        let mut t = UiTiming::default();
        t.begin();
        let start = t.panel_start();
        std::thread::sleep(Duration::from_millis(200));
        t.exclude(Duration::from_millis(200));
        t.panel_done("Project", start);
        let (name, took) = t.panel_times[0];
        assert_eq!(name, "Project");
        assert!(took < Duration::from_millis(100), "{took:?}");
    }

    #[test]
    fn long_frames_are_counted_by_threshold_and_the_longest_is_kept() {
        let mut t = UiTiming::default();
        for ms in [10.0, 60.0, 120.0, 300.0, 30.0] {
            t.record(ms, ms / 2.0, ms / 2.0);
        }
        let s = t.stats();
        assert_eq!(s["frames"], 5);
        assert_eq!(s["over50"], 3);
        assert_eq!(s["over100"], 2);
        assert_eq!(s["over250"], 1);
        assert_eq!(s["longestMs"], 300.0);
        assert_eq!(t.slow_logged, 1);
    }

    #[test]
    fn time_in_a_modal_file_dialog_is_not_a_slow_frame() {
        let mut t = UiTiming::default();
        t.begin();
        std::thread::sleep(Duration::from_millis(300));
        t.exclude(Duration::from_millis(300));
        t.end(Half::Panels);
        assert_eq!(t.stats()["over250"], 0, "{}", t.stats());
        assert_eq!(t.stats()["frames"], 1);
    }

    #[test]
    fn a_slow_frame_names_its_slowest_panels() {
        // a field report's slow frames said only "panels"
        let mut t = UiTiming::default();
        t.panel("Program", Duration::from_millis(40));
        t.panel("Timeline", Duration::from_millis(500));
        t.panel("Program", Duration::from_millis(10));
        t.panel("Project", Duration::from_millis(3));
        t.panel("Info", Duration::from_millis(1));
        let mut by: Vec<_> = std::mem::take(&mut t.panel_times);
        by.sort_by_key(|(_, d)| std::cmp::Reverse(*d));
        assert_eq!(UiTiming::last_breakdown(600.0, &by), " — Timeline 500 ms, Program 50 ms, Project 3 ms, other 46 ms");
        assert_eq!(UiTiming::last_breakdown(10.0, &[]), "");
    }

    #[test]
    fn a_frame_is_both_halves_and_hostile_values_are_ignored() {
        let mut t = UiTiming::default();
        t.end(Half::Panels); // nothing begun: nothing counted
        assert_eq!(t.stats()["frames"], 0);
        t.begin();
        t.end(Half::Logic);
        t.begin();
        t.end(Half::Panels);
        assert_eq!(t.stats()["frames"], 1);
        t.record(f64::NAN, 0.0, 0.0);
        t.record(-1.0, 0.0, 0.0);
        assert_eq!(t.stats()["frames"], 1);
    }
}
