//! Saves and restores the desktop main window's settled rectangle (#1994).
//!
//! The native host reads [`WindowState`] before creating the hidden window. This module then
//! observes the root viewport and records a move or resize after it has settled, including one
//! made by an external window manager. Eframe's own window persistence is deliberately not used:
//! on macOS it reapplies position after native creation, which makes the two owners visibly fight.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use web_time::Instant;

/// File beside the other per-user FilmCraft state.
pub const FILE_NAME: &str = "window-state.json";

/// Safe first-launch bounds, shared with the native window builder.
pub const DEFAULT_WIDTH: f32 = 1600.0;
pub const DEFAULT_HEIGHT: f32 = 980.0;
pub const MIN_WIDTH: f32 = 900.0;
pub const MIN_HEIGHT: f32 = 560.0;

/// A drag can produce many geometry events. Save only after the rectangle has stayed put briefly.
const SETTLE_TIME: Duration = Duration::from_millis(250);

/// A failed optional-state write is retried without hammering a full or read-only disk.
const RETRY_TIME: Duration = Duration::from_secs(30);

/// Window-state files are tiny; refuse an input-sized allocation from a corrupt file.
const MAX_FILE_BYTES: u64 = 4 * 1024;

/// Broad sanity limits for data read from disk. The viewport builder clamps a valid size further
/// to the attached monitors before showing it.
const MAX_DIMENSION: f32 = 32_768.0;
const MAX_POSITION: f32 = 1_000_000.0;

/// Normal (not minimized, maximized or full-screen) main-window geometry in OS logical points.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WindowState {
    pub width: f32,
    pub height: f32,
    pub x: f32,
    pub y: f32,
}

impl WindowState {
    /// Read a valid saved rectangle. A missing file is the normal first-launch case; malformed or
    /// hostile state is reported and ignored so the caller uses its safe default.
    pub fn load(path: &Path) -> Option<Self> {
        match Self::load_result(path) {
            Ok(state) => state,
            Err(error) => {
                log::warn!("can't restore window state from {}: {error}", path.display());
                None
            }
        }
    }

    fn load_result(path: &Path) -> Result<Option<Self>, String> {
        let metadata = match std::fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.to_string()),
        };
        if metadata.len() > MAX_FILE_BYTES {
            return Err(format!("file is larger than {MAX_FILE_BYTES} bytes"));
        }
        let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
        let state: Self = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        state.valid().then_some(state).ok_or_else(|| "rectangle is outside the supported range".to_owned()).map(Some)
    }

    /// Apply the saved rectangle before eframe creates its initially hidden native window.
    pub fn apply_to(self, viewport: egui::ViewportBuilder) -> egui::ViewportBuilder {
        viewport.with_inner_size([self.width, self.height]).with_position([self.x, self.y])
    }

    fn valid(self) -> bool {
        self.width.is_finite()
            && self.height.is_finite()
            && self.x.is_finite()
            && self.y.is_finite()
            && (MIN_WIDTH..=MAX_DIMENSION).contains(&self.width)
            && (MIN_HEIGHT..=MAX_DIMENSION).contains(&self.height)
            && self.x.abs() <= MAX_POSITION
            && self.y.abs() <= MAX_POSITION
    }

    /// Egui reports viewport rectangles in interface points. Remove interface zoom to retain OS
    /// logical points, so changing FilmCraft's UI scale does not change the native window size.
    fn from_viewport(info: &egui::ViewportInfo, zoom: f32) -> Option<Self> {
        if info.minimized == Some(true) || info.maximized == Some(true) || info.fullscreen == Some(true) || !zoom.is_finite() || zoom <= 0.0 {
            return None;
        }
        let (Some(outer), Some(inner)) = (info.outer_rect, info.inner_rect) else { return None };
        let state = Self { width: inner.width() * zoom, height: inner.height() * zoom, x: outer.min.x * zoom, y: outer.min.y * zoom };
        state.valid().then_some(state)
    }
}

/// Authoritative owner of the last normal rectangle for an interactive desktop session.
#[derive(Default)]
pub(crate) struct Tracker {
    path: Option<PathBuf>,
    current: Option<WindowState>,
    saved: Option<WindowState>,
    save_at: Option<Instant>,
}

impl Tracker {
    pub fn new(path: PathBuf, restored: Option<WindowState>) -> Self {
        Self { path: Some(path), current: restored, saved: restored, save_at: None }
    }

    /// Observe native geometry and save it after external and user-driven placement has settled.
    pub fn observe(&mut self, ctx: &egui::Context, info: &egui::ViewportInfo) {
        let now = Instant::now();
        let Some(state) = WindowState::from_viewport(info, ctx.zoom_factor()) else { return };
        if self.current != Some(state) {
            self.current = Some(state);
            self.save_at = Some(now + SETTLE_TIME);
        }
        self.maybe_save(now, ctx);
    }

    fn maybe_save(&mut self, now: Instant, ctx: &egui::Context) {
        let Some(save_at) = self.save_at else { return };
        if now < save_at {
            ctx.request_repaint_after(save_at - now);
            return;
        }
        if self.write_current() {
            self.save_at = None;
        } else {
            self.save_at = Some(now + RETRY_TIME);
            ctx.request_repaint_after(RETRY_TIME);
        }
    }

    /// Synchronously record the last observed rectangle before native teardown.
    pub fn save_now(&mut self) {
        if self.current != self.saved {
            let _ = self.write_current();
        }
    }

    fn write_current(&mut self) -> bool {
        let (Some(path), Some(state)) = (self.path.as_deref(), self.current) else { return true };
        if self.saved == Some(state) {
            return true;
        }
        let result = serde_json::to_vec_pretty(&state).map_err(|error| error.to_string()).and_then(|bytes| {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            filmcraft_format::atomic_write(path, &bytes).map_err(|error| error.to_string())
        });
        match result {
            Ok(()) => {
                self.saved = Some(state);
                true
            }
            Err(error) => {
                log::warn!("can't save window state to {}: {error}", path.display());
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(x: f32, y: f32, width: f32, height: f32) -> egui::ViewportInfo {
        egui::ViewportInfo {
            outer_rect: Some(egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(width + 16.0, height + 40.0))),
            inner_rect: Some(egui::Rect::from_min_size(egui::pos2(x + 8.0, y + 32.0), egui::vec2(width, height))),
            minimized: Some(false),
            maximized: Some(false),
            fullscreen: Some(false),
            ..Default::default()
        }
    }

    #[test]
    fn viewport_geometry_removes_interface_zoom() {
        let state = WindowState::from_viewport(&info(50.0, 25.0, 800.0, 600.0), 1.5);
        assert_eq!(state, Some(WindowState { width: 1200.0, height: 900.0, x: 75.0, y: 37.5 }));
    }

    #[test]
    fn missing_corrupt_and_hostile_files_fall_back() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join(FILE_NAME);
        assert_eq!(WindowState::load(&path), None);
        std::fs::write(&path, b"not json").expect("write corrupt state");
        assert_eq!(WindowState::load(&path), None);
        std::fs::write(&path, br#"{"width":899,"height":700,"x":0,"y":0}"#).expect("write hostile state");
        assert_eq!(WindowState::load(&path), None);
    }

    #[test]
    fn saved_rectangle_round_trips_and_applies_before_creation() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join(FILE_NAME);
        let state = WindowState { width: 1440.0, height: 900.0, x: 120.0, y: 80.0 };
        let mut tracker = Tracker::new(path.clone(), None);
        tracker.current = Some(state);
        tracker.save_now();
        assert_eq!(WindowState::load(&path), Some(state));

        let viewport = state.apply_to(egui::ViewportBuilder::default());
        assert_eq!(viewport.inner_size, Some(egui::vec2(1440.0, 900.0)));
        assert_eq!(viewport.position, Some(egui::pos2(120.0, 80.0)));
    }
}
