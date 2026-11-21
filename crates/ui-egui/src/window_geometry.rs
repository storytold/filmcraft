//! Keeps the remembered window geometry current (#1994).
//!
//! eframe records the main window's normal (not maximized, not full-screen) position and size
//! only when it saves, every `auto_save_interval`, and keeps that record while the window is
//! maximized. A window moved or resized and then maximized before the next save would come back
//! at the old size when unmaximized after a relaunch. Two things prevent that:
//!
//! - [`Tracker::record`] writes the normal geometry seen last into the window record of every
//!   save made while the window is maximized or full-screen, so the record never depends on a
//!   save having happened in between. Only for a window whose geometry is remembered: one
//!   started for an agent reads the user's record and must not change it.
//! - A change to the normal geometry asks for a save soon after it, so little is lost if the
//!   app ends without its closing save.

use std::time::Duration;

/// How soon a save follows a change to the normal geometry: soon enough that little is lost when
/// the app ends without its closing save; long enough that a drag does not save every frame.
pub const CHANGED: Duration = Duration::from_millis(250);

/// eframe's own interval, when nothing has changed.
pub const UNCHANGED: Duration = Duration::from_secs(30);

/// The key eframe stores the main window's record under.
const WINDOW_KEY: &str = "window";

/// A normal geometry in the units eframe stores: the frame's position in physical pixels, the
/// window's scale factor, and the inner size in points.
#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
struct Geometry {
    outer_position_pixels: Option<Xy>,
    scale_factor: f32,
    inner_size_points: Xy,
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
struct Xy {
    x: f32,
    y: f32,
}

/// eframe's stored window record (`egui_winit::WindowSettings`), whose fields are private. The
/// test `the_record_reads_as_eframes_own` holds this to eframe's format.
#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
struct Record {
    normal_geometry: Geometry,
    fullscreen: bool,
    maximized: bool,
}

/// The normal geometry last seen, and whether a save has recorded it.
#[derive(Default)]
pub struct Tracker {
    last: Option<(egui::Pos2, egui::Vec2)>,
    /// `last` as eframe stores it.
    normal: Option<Geometry>,
    /// The window is maximized or full-screen now, so eframe's save keeps its older geometry.
    covered: bool,
    dirty: bool,
    /// This window's geometry is the one remembered. Off for a window that only reads the record,
    /// such as one started for an agent: its saves must leave the user's record as it is.
    remembered: bool,
}

impl Tracker {
    /// Whether this window's geometry is the one remembered (off until the desktop app says so).
    pub fn set_remembered(&mut self, on: bool) {
        self.remembered = on;
    }

    /// Note the root viewport's geometry for this frame. `zoom` is egui's zoom factor.
    pub fn observe(&mut self, info: &egui::ViewportInfo, zoom: f32) {
        self.covered = info.maximized == Some(true) || info.fullscreen == Some(true);
        if self.covered {
            return;
        }
        // A minimized window has neither rect.
        let (Some(outer), Some(inner)) = (info.outer_rect, info.inner_rect) else { return };
        let now = (outer.min, inner.size());
        if self.last.is_some_and(|l| l != now) {
            self.dirty = true;
        }
        self.last = Some(now);
        if let Some(scale) = info.native_pixels_per_point {
            // Rects are in points: pixels over zoom times scale. Positions are whole pixels.
            let pixels = outer.min * (zoom * scale);
            self.normal = Some(Geometry {
                outer_position_pixels: Some(Xy { x: pixels.x.round(), y: pixels.y.round() }),
                scale_factor: scale,
                inner_size_points: Xy { x: inner.width(), y: inner.height() },
            });
        }
    }

    /// The autosave interval eframe should use now.
    pub fn interval(&self) -> Duration {
        if self.dirty { CHANGED } else { UNCHANGED }
    }

    /// A save is being made, and eframe has just written its window record to `storage`. While
    /// the window is maximized or full-screen that record holds the normal geometry of eframe's
    /// previous save, so put the one seen last in its place.
    pub fn record(&mut self, storage: &mut dyn eframe::Storage) {
        self.dirty = false;
        if !self.remembered {
            // eframe wrote nothing for this window, so the record in `storage` is the user's.
            return;
        }
        let (true, Some(normal)) = (self.covered, self.normal) else { return };
        // No record, or one in a format this does not know: leave it as eframe wrote it.
        let Some(mut record) = eframe::get_value::<Record>(storage, WINDOW_KEY) else { return };
        if (record.maximized || record.fullscreen) && record.normal_geometry != normal {
            record.normal_geometry = normal;
            eframe::set_value(storage, WINDOW_KEY, &record);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The window's scale factor in these tests.
    const SCALE: f32 = 1.5;

    fn info(x: f32, w: f32, maximized: bool) -> egui::ViewportInfo {
        egui::ViewportInfo {
            outer_rect: Some(egui::Rect::from_min_size(egui::pos2(x, 0.0), egui::vec2(w + 16.0, 600.0))),
            inner_rect: Some(egui::Rect::from_min_size(egui::pos2(x + 8.0, 30.0), egui::vec2(w, 560.0))),
            maximized: Some(maximized),
            fullscreen: Some(false),
            native_pixels_per_point: Some(SCALE),
            ..Default::default()
        }
    }

    #[derive(Default)]
    struct Store(std::collections::HashMap<String, String>);

    impl eframe::Storage for Store {
        fn get_string(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
        fn set_string(&mut self, key: &str, value: String) {
            self.0.insert(key.to_owned(), value);
        }
        fn remove_string(&mut self, key: &str) {
            self.0.remove(key);
        }
        fn flush(&mut self) {}
    }

    /// The record of a window whose normal geometry is `info(x, w, _)`'s, at zoom 1.
    fn record_of(x: f32, w: f32, maximized: bool) -> Record {
        Record {
            normal_geometry: Geometry {
                outer_position_pixels: Some(Xy { x: x * SCALE, y: 0.0 }),
                scale_factor: SCALE,
                inner_size_points: Xy { x: w, y: 560.0 },
            },
            fullscreen: false,
            maximized,
        }
    }

    fn remembered() -> Tracker {
        let mut t = Tracker::default();
        t.set_remembered(true);
        t
    }

    /// A window started for an agent opens from the user's record and writes nothing back. With
    /// that record maximized, an agent that unmaximizes, resizes and maximizes its window must
    /// not put its own geometry into it.
    #[test]
    fn a_window_that_is_not_remembered_leaves_the_users_maximized_record_alone() {
        let users = record_of(100.0, 800.0, true);
        for is_remembered in [false, true] {
            let mut t = Tracker::default();
            t.set_remembered(is_remembered);
            let mut store = Store::default();
            eframe::set_value(&mut store, WINDOW_KEY, &users);
            t.observe(&info(0.0, 1920.0, true), 1.0);
            t.observe(&info(140.0, 900.0, false), 1.0);
            t.observe(&info(0.0, 1920.0, true), 1.0);
            t.record(&mut store);
            let after = eframe::get_value::<Record>(&store, WINDOW_KEY);
            if is_remembered {
                assert_eq!(after, Some(record_of(140.0, 900.0, true)), "the user's own window");
            } else {
                assert_eq!(after, Some(users), "an agent's window");
            }
        }
        assert!(!Tracker::default().remembered, "off unless the desktop app turns it on");
    }

    /// #1994: resize, maximize before any save, save, relaunch, unmaximize. eframe's save while
    /// maximized writes the geometry of its previous save; the record must hold the resized one.
    #[test]
    fn a_resize_then_an_immediate_maximize_is_saved_with_the_resized_geometry() {
        let mut t = remembered();
        let mut store = Store::default();
        t.observe(&info(100.0, 800.0, false), 1.0);
        eframe::set_value(&mut store, WINDOW_KEY, &record_of(100.0, 800.0, false));
        t.record(&mut store);

        // Moved and resized, then maximized with no save in between.
        t.observe(&info(140.0, 900.0, false), 1.0);
        t.observe(&info(0.0, 1920.0, true), 1.0);
        // eframe's save: maximized, with the normal geometry of its previous save.
        eframe::set_value(&mut store, WINDOW_KEY, &record_of(100.0, 800.0, true));
        t.record(&mut store);

        // The next launch reads the record: maximized, and unmaximizing gives the resized window.
        let read: Record = eframe::get_value(&store, WINDOW_KEY).expect("a record");
        assert_eq!(read, record_of(140.0, 900.0, true));
    }

    #[test]
    fn a_save_of_a_normal_window_or_with_no_record_is_left_as_eframe_wrote_it() {
        let mut t = remembered();
        let mut store = Store::default();
        t.observe(&info(140.0, 900.0, false), 1.0);
        let written = record_of(141.0, 901.0, false);
        eframe::set_value(&mut store, WINDOW_KEY, &written);
        t.record(&mut store);
        assert_eq!(eframe::get_value::<Record>(&store, WINDOW_KEY), Some(written));

        // Maximized with no record in the storage: none is made.
        let mut store = Store::default();
        t.observe(&info(0.0, 1920.0, true), 1.0);
        t.record(&mut store);
        assert!(store.0.is_empty());
    }

    /// The position is stored in physical pixels: points times egui's zoom times the scale.
    #[test]
    fn the_position_is_stored_in_pixels_under_a_zoom() {
        let mut t = remembered();
        t.observe(&info(100.0, 800.0, false), 2.0);
        let normal = t.normal.expect("a geometry");
        assert_eq!(normal.outer_position_pixels, Some(Xy { x: 300.0, y: 0.0 }));
        assert_eq!(normal.inner_size_points, Xy { x: 800.0, y: 560.0 });
    }

    /// [`Record`] mirrors eframe's private `WindowSettings`: what this writes, eframe must read,
    /// and what eframe writes, this must read.
    #[test]
    fn the_record_reads_as_eframes_own() {
        let ours = record_of(140.0, 900.0, true);
        let mut store = Store::default();
        eframe::set_value(&mut store, WINDOW_KEY, &ours);
        let theirs: egui_winit::WindowSettings = eframe::get_value(&store, WINDOW_KEY).expect("eframe reads it");
        assert_eq!(theirs.inner_size_points(), egui::vec2(900.0, 560.0));

        let mut again = Store::default();
        eframe::set_value(&mut again, WINDOW_KEY, &theirs);
        assert_eq!(eframe::get_value::<Record>(&again, WINDOW_KEY), Some(ours));
    }

    #[test]
    fn a_move_or_resize_asks_for_a_save_soon_and_a_save_settles_it() {
        let mut t = remembered();
        t.observe(&info(100.0, 800.0, false), 1.0);
        assert_eq!(t.interval(), UNCHANGED, "the first geometry is the restored one");
        t.observe(&info(100.0, 800.0, false), 1.0);
        assert_eq!(t.interval(), UNCHANGED);
        t.observe(&info(140.0, 800.0, false), 1.0);
        assert_eq!(t.interval(), CHANGED, "moved");
        t.record(&mut Store::default());
        assert_eq!(t.interval(), UNCHANGED);
        t.observe(&info(140.0, 900.0, false), 1.0);
        assert_eq!(t.interval(), CHANGED, "resized");
    }

    #[test]
    fn maximizing_is_not_a_change_to_the_normal_geometry() {
        let mut t = remembered();
        t.observe(&info(100.0, 800.0, false), 1.0);
        t.observe(&info(0.0, 1920.0, true), 1.0);
        assert_eq!(t.interval(), UNCHANGED);
        // unmaximized back to the same geometry
        t.observe(&info(100.0, 800.0, false), 1.0);
        assert_eq!(t.interval(), UNCHANGED);
    }
}
