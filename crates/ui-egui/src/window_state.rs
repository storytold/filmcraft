//! Saves and restores the desktop main window's settled rectangle (#1994).
//!
//! The native host reads [`WindowState`] before creating the hidden window. This module then
//! observes the root viewport and records a move or resize after it has settled, including one
//! made by an external window manager. Eframe's own window persistence is deliberately not used:
//! on macOS it reapplies position after native creation, which makes the two owners visibly fight.

use serde::{Deserialize, Serialize};
use std::io::Read as _;
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
    /// Native pixels per point when saved; Windows needs this to restore across monitor scales.
    #[serde(default = "default_scale_factor")]
    pub scale_factor: f32,
}

fn default_scale_factor() -> f32 {
    1.0
}

/// Attached display bounds. On Windows they are physical pixels; elsewhere they are logical points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DisplayBounds {
    /// macOS/Linux report logical bounds; Windows reports physical pixel bounds.
    pub rect: egui::Rect,
    pub scale_factor: f32,
    pub primary: bool,
}

impl WindowState {
    /// Read a valid saved rectangle. A missing file is the normal first-launch case; malformed or
    /// hostile state is reported and ignored so the caller uses its safe default.
    pub fn load(path: &Path) -> Option<Self> {
        match Self::load_result(path) {
            Ok(state) => state,
            Err(error) => {
                log::warn!("can't restore {FILE_NAME}: {error}");
                None
            }
        }
    }

    fn load_result(path: &Path) -> Result<Option<Self>, String> {
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.to_string()),
        };
        let mut bytes = Vec::with_capacity(MAX_FILE_BYTES as usize + 1);
        file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes).map_err(|error| error.to_string())?;
        if bytes.len() > MAX_FILE_BYTES as usize {
            return Err(format!("file is larger than {MAX_FILE_BYTES} bytes"));
        }
        let state: Self = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        state.valid().then_some(state).ok_or_else(|| "rectangle is outside the supported range".to_owned()).map(Some)
    }

    /// Apply the saved rectangle before eframe creates its initially hidden native window.
    pub fn apply_to(self, viewport: egui::ViewportBuilder, displays: Option<&[DisplayBounds]>) -> egui::ViewportBuilder {
        let viewport = viewport.with_inner_size([self.width, self.height]);
        match self.position_for(displays) {
            Some(position) => viewport.with_position(position),
            None => viewport,
        }
    }

    fn position_for(self, displays: Option<&[DisplayBounds]>) -> Option<[f32; 2]> {
        let Some(displays) = displays else { return Some([self.x, self.y]) };
        if cfg!(target_os = "windows") {
            return self.position_for_physical_displays(displays);
        }
        self.position_for_logical_displays(displays)
    }

    fn position_for_logical_displays(self, displays: &[DisplayBounds]) -> Option<[f32; 2]> {
        let saved = egui::Rect::from_min_size(egui::pos2(self.x, self.y), egui::vec2(self.width, self.height));
        if displays.iter().any(|display| saved.intersect(display.rect).area() > 0.0) {
            return Some([self.x, self.y]);
        }
        displays.iter().find(|display| display.primary).or_else(|| displays.first()).map(|display| [display.rect.min.x, display.rect.min.y])
    }

    /// Windows display bounds and native window positions are physical pixels, while the saved
    /// viewport position is in points. Convert it to pixels using its saved monitor scale, then
    /// to the primary monitor's points because winit initially creates the window there.
    fn position_for_physical_displays(self, displays: &[DisplayBounds]) -> Option<[f32; 2]> {
        let initial_display = displays.iter().find(|display| display.primary).or_else(|| displays.first())?;
        let initial_scale = initial_display.scale_factor;
        if !initial_scale.is_finite() || initial_scale <= 0.0 || !self.scale_factor.is_finite() || self.scale_factor <= 0.0 {
            return None;
        }
        let saved = egui::Rect::from_min_size(
            egui::pos2(self.x * self.scale_factor, self.y * self.scale_factor),
            egui::vec2(self.width * self.scale_factor, self.height * self.scale_factor),
        );
        let position_pixels = if displays.iter().any(|display| saved.intersect(display.rect).area() > 0.0) {
            egui::pos2(self.x * self.scale_factor, self.y * self.scale_factor)
        } else {
            initial_display.rect.min
        };
        Some([position_pixels.x / initial_scale, position_pixels.y / initial_scale])
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
            && (0.25..=16.0).contains(&self.scale_factor)
    }

    /// Egui reports viewport rectangles in interface points. Remove interface zoom to retain OS
    /// logical points, so changing FilmCraft's UI scale does not change the native window size.
    fn from_viewport(info: &egui::ViewportInfo, zoom: f32, native_maximized: bool) -> Option<Self> {
        let maximized = if cfg!(target_os = "macos") { native_maximized } else { info.maximized == Some(true) };
        if info.minimized == Some(true) || maximized || info.fullscreen == Some(true) || !zoom.is_finite() || zoom <= 0.0 {
            return None;
        }
        let (Some(outer), Some(inner)) = (info.outer_rect, info.inner_rect) else { return None };
        let scale_factor = info.native_pixels_per_point.unwrap_or(1.0);
        let state = Self { width: inner.width() * zoom, height: inner.height() * zoom, x: outer.min.x * zoom, y: outer.min.y * zoom, scale_factor };
        state.valid().then_some(state)
    }
}

/// Authoritative owner of the last normal rectangle for an interactive desktop session.
pub(crate) struct Tracker {
    path: Option<PathBuf>,
    current: Option<WindowState>,
    saved: Option<WindowState>,
    save_at: Option<Instant>,
    native_maximized: fn() -> bool,
}

impl Default for Tracker {
    fn default() -> Self {
        Self { path: None, current: None, saved: None, save_at: None, native_maximized: || false }
    }
}

impl Tracker {
    pub fn new(path: PathBuf, restored: Option<WindowState>, native_maximized: fn() -> bool) -> Self {
        Self { path: Some(path), current: restored, saved: restored, save_at: None, native_maximized }
    }

    /// Observe native geometry and save it after external and user-driven placement has settled.
    pub fn observe(&mut self, ctx: &egui::Context, info: &egui::ViewportInfo) {
        let now = Instant::now();
        let Some(state) = WindowState::from_viewport(info, ctx.zoom_factor(), (self.native_maximized)()) else { return };
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
                log::warn!("can't save {FILE_NAME}: {error}");
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
            native_pixels_per_point: Some(1.0),
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
        let state = WindowState::from_viewport(&info(50.0, 25.0, 800.0, 600.0), 1.5, false);
        assert_eq!(state, Some(WindowState { width: 1200.0, height: 900.0, x: 75.0, y: 37.5, scale_factor: 1.0 }));
    }

    #[test]
    fn viewport_geometry_retains_native_scale_for_windows_restore() {
        let mut viewport = info(1500.0, 100.0, 1000.0, 700.0);
        viewport.native_pixels_per_point = Some(2.0);
        let state = WindowState::from_viewport(&viewport, 1.0, false);
        assert_eq!(state.map(|state| state.scale_factor), Some(2.0));
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
        std::fs::write(&path, br#"{"width":1200,"height":800,"x":100,"y":50}"#).expect("write legacy state");
        assert_eq!(WindowState::load(&path).map(|state| state.scale_factor), Some(1.0));
        std::fs::write(&path, vec![b' '; MAX_FILE_BYTES as usize + 1]).expect("write oversized state");
        assert_eq!(WindowState::load(&path), None);
    }

    #[test]
    fn saved_rectangle_round_trips_and_applies_before_creation() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join(FILE_NAME);
        let state = WindowState { width: 1440.0, height: 900.0, x: 120.0, y: 80.0, scale_factor: 1.0 };
        let mut tracker = Tracker::new(path.clone(), None, || false);
        tracker.current = Some(state);
        tracker.save_now();
        assert_eq!(WindowState::load(&path), Some(state));

        let viewport = state.apply_to(egui::ViewportBuilder::default(), None);
        assert_eq!(viewport.inner_size, Some(egui::vec2(1440.0, 900.0)));
        assert_eq!(viewport.position, Some(egui::pos2(120.0, 80.0)));
    }

    #[test]
    fn offscreen_position_moves_to_primary_display_before_creation() {
        let state = WindowState { width: 1440.0, height: 900.0, x: 2400.0, y: 80.0, scale_factor: 1.0 };
        let displays = [DisplayBounds { rect: egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1920.0, 1080.0)), scale_factor: 1.0, primary: true }];
        let viewport = state.apply_to(egui::ViewportBuilder::default(), Some(&displays));
        assert_eq!(viewport.position, Some(egui::pos2(0.0, 0.0)));
    }

    #[test]
    fn position_on_attached_secondary_display_is_preserved() {
        let state = WindowState { width: 1440.0, height: 900.0, x: 2400.0, y: 80.0, scale_factor: 1.0 };
        let displays = [
            DisplayBounds { rect: egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1920.0, 1080.0)), scale_factor: 1.0, primary: true },
            DisplayBounds { rect: egui::Rect::from_min_size(egui::pos2(1920.0, 0.0), egui::vec2(2560.0, 1440.0)), scale_factor: 1.0, primary: false },
        ];
        let viewport = state.apply_to(egui::ViewportBuilder::default(), Some(&displays));
        assert_eq!(viewport.position, Some(egui::pos2(2400.0, 80.0)));
    }

    #[test]
    fn windows_saved_position_uses_saved_scale_before_initial_creation() {
        let state = WindowState { width: 1000.0, height: 700.0, x: 1500.0, y: 100.0, scale_factor: 2.0 };
        let displays = [
            DisplayBounds { rect: egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1920.0, 1080.0)), scale_factor: 1.0, primary: true },
            DisplayBounds { rect: egui::Rect::from_min_size(egui::pos2(1920.0, 0.0), egui::vec2(3840.0, 2160.0)), scale_factor: 2.0, primary: false },
        ];

        assert_eq!(state.position_for_physical_displays(&displays), Some([3000.0, 200.0]));
    }

    #[test]
    fn logical_display_bounds_are_not_scaled_again() {
        let state = WindowState { width: 800.0, height: 700.0, x: 1000.0, y: 100.0, scale_factor: 2.0 };
        let displays = [DisplayBounds { rect: egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1920.0, 1080.0)), scale_factor: 2.0, primary: true }];
        assert_eq!(state.position_for_logical_displays(&displays), Some([1000.0, 100.0]));
    }

    #[test]
    fn native_maximize_does_not_replace_normal_geometry_on_macos() {
        if cfg!(target_os = "macos") {
            assert_eq!(WindowState::from_viewport(&info(0.0, 0.0, 1600.0, 900.0), 1.0, true), None);
        }
    }
}
