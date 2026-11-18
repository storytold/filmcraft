//! Keeps the remembered window geometry current (#1994).
//!
//! eframe records the main window's normal (not maximized, not full-screen) position and size
//! only when it saves, every `auto_save_interval`, and keeps that record while the window is
//! maximized. A window moved or resized and then maximized before the next save would come back
//! at the old size when unmaximized after a relaunch. So a change to the normal geometry asks for
//! a save soon after it, while the window still shows that geometry.

use std::time::Duration;

/// How soon a save follows a change to the normal geometry. Short enough that the change is
/// recorded before a maximize that follows it; long enough that a drag does not save every frame.
pub const CHANGED: Duration = Duration::from_millis(250);

/// eframe's own interval, when nothing has changed.
pub const UNCHANGED: Duration = Duration::from_secs(30);

/// The normal geometry last seen, and whether a save has recorded it.
#[derive(Default)]
pub struct Tracker {
    last: Option<(egui::Pos2, egui::Vec2)>,
    dirty: bool,
}

impl Tracker {
    /// Note the root viewport's geometry for this frame.
    pub fn observe(&mut self, info: &egui::ViewportInfo) {
        if info.maximized == Some(true) || info.fullscreen == Some(true) {
            return;
        }
        let (Some(outer), Some(inner)) = (info.outer_rect, info.inner_rect) else { return };
        let now = (outer.min, inner.size());
        if self.last.is_some_and(|l| l != now) {
            self.dirty = true;
        }
        self.last = Some(now);
    }

    /// The autosave interval eframe should use now.
    pub fn interval(&self) -> Duration {
        if self.dirty { CHANGED } else { UNCHANGED }
    }

    /// A save has recorded the geometry.
    pub fn saved(&mut self) {
        self.dirty = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(x: f32, w: f32, maximized: bool) -> egui::ViewportInfo {
        egui::ViewportInfo {
            outer_rect: Some(egui::Rect::from_min_size(egui::pos2(x, 0.0), egui::vec2(w + 16.0, 600.0))),
            inner_rect: Some(egui::Rect::from_min_size(egui::pos2(x + 8.0, 30.0), egui::vec2(w, 560.0))),
            maximized: Some(maximized),
            fullscreen: Some(false),
            ..Default::default()
        }
    }

    #[test]
    fn a_move_or_resize_asks_for_a_save_soon_and_a_save_settles_it() {
        let mut t = Tracker::default();
        t.observe(&info(100.0, 800.0, false));
        assert_eq!(t.interval(), UNCHANGED, "the first geometry is the restored one");
        t.observe(&info(100.0, 800.0, false));
        assert_eq!(t.interval(), UNCHANGED);
        t.observe(&info(140.0, 800.0, false));
        assert_eq!(t.interval(), CHANGED, "moved");
        t.saved();
        assert_eq!(t.interval(), UNCHANGED);
        t.observe(&info(140.0, 900.0, false));
        assert_eq!(t.interval(), CHANGED, "resized");
    }

    #[test]
    fn maximizing_is_not_a_change_to_the_normal_geometry() {
        let mut t = Tracker::default();
        t.observe(&info(100.0, 800.0, false));
        t.observe(&info(0.0, 1920.0, true));
        assert_eq!(t.interval(), UNCHANGED);
        // unmaximized back to the same geometry
        t.observe(&info(100.0, 800.0, false));
        assert_eq!(t.interval(), UNCHANGED);
    }
}
