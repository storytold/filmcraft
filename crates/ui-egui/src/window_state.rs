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
    /// Native pixels per logical point on this display.
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

    /// Apply the saved size and, where the backend permits, position before native display.
    pub fn apply_to(self, viewport: egui::ViewportBuilder, displays: Option<&[DisplayBounds]>) -> egui::ViewportBuilder {
        let viewport = viewport.with_inner_size([self.width, self.height]);
        #[cfg(target_os = "windows")]
        {
            let _ = displays;
            viewport
        }
        #[cfg(not(target_os = "windows"))]
        {
            match self.position_for(displays) {
                Some(position) => viewport.with_position(position),
                None => viewport,
            }
        }
    }

    #[cfg(not(target_os = "windows"))]
    fn position_for(self, displays: Option<&[DisplayBounds]>) -> Option<[f32; 2]> {
        let Some(displays) = displays else { return Some([self.x, self.y]) };
        self.position_for_logical_displays(displays)
    }

    fn position_for_logical_displays(self, displays: &[DisplayBounds]) -> Option<[f32; 2]> {
        let saved = egui::Rect::from_min_size(egui::pos2(self.x, self.y), egui::vec2(self.width, self.height));
        if displays.iter().any(|display| saved.intersect(display.rect).area() > 0.0) {
            return Some([self.x, self.y]);
        }
        displays.iter().find(|display| display.primary).or_else(|| displays.first()).map(|display| [display.rect.min.x, display.rect.min.y])
    }

    /// Windows display bounds and native window positions are physical pixels; viewport positions
    /// are points. Keep this out of the viewport builder because eframe reapplies that builder
    /// after a DPI change and would scale the logical position a second time.
    pub fn physical_position(self, displays: Option<&[DisplayBounds]>) -> Option<[i32; 2]> {
        if !self.scale_factor.is_finite() || !(0.25..=16.0).contains(&self.scale_factor) {
            return None;
        }
        let saved_position = egui::pos2(self.x * self.scale_factor, self.y * self.scale_factor);
        let position_pixels = match displays {
            Some(displays) if !displays.is_empty() => {
                let primary = displays.iter().find(|display| display.primary).or_else(|| displays.first())?;
                // eframe clamps the viewport size to the largest attached monitor's logical
                // dimensions before creating the window. Use that same size here when checking
                // whether the later native physical-position restore remains visible.
                let largest_monitor_points = displays.iter().fold(egui::Vec2::ZERO, |largest, display| {
                    if display.scale_factor.is_finite() && display.scale_factor > 0.0 {
                        largest.max(display.rect.size() / display.scale_factor)
                    } else {
                        largest
                    }
                });
                let clamped_size_points = egui::vec2(self.width.min(largest_monitor_points.x), self.height.min(largest_monitor_points.y));
                let visible = displays.iter().any(|display| {
                    let size_pixels = clamped_size_points * display.scale_factor;
                    let saved = egui::Rect::from_min_size(saved_position, size_pixels);
                    saved.intersect(display.rect).area() > 0.0
                });
                if visible { saved_position } else { primary.rect.min }
            }
            _ => saved_position,
        };
        Some([physical_coordinate(position_pixels.x)?, physical_coordinate(position_pixels.y)?])
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
    fn from_viewport(info: &egui::ViewportInfo, zoom: f32, maximized: Option<bool>) -> Option<Self> {
        let maximized = maximized?;
        if info.minimized == Some(true) || maximized || info.fullscreen == Some(true) || !zoom.is_finite() || zoom <= 0.0 {
            return None;
        }
        let (Some(outer), Some(inner)) = (info.outer_rect, info.inner_rect) else { return None };
        let scale_factor = info.native_pixels_per_point.unwrap_or(1.0);
        let state = Self { width: inner.width() * zoom, height: inner.height() * zoom, x: outer.min.x * zoom, y: outer.min.y * zoom, scale_factor };
        state.valid().then_some(state)
    }
}

fn physical_coordinate(position: f32) -> Option<i32> {
    if !position.is_finite() || position < i32::MIN as f32 || position > i32::MAX as f32 {
        return None;
    }
    Some(position.round() as i32)
}

/// Authoritative owner of the last normal rectangle for an interactive desktop session.
pub(crate) struct Tracker {
    path: Option<PathBuf>,
    current: Option<WindowState>,
    saved: Option<WindowState>,
    save_at: Option<Instant>,
    native_maximized: fn() -> Option<bool>,
}

impl Default for Tracker {
    fn default() -> Self {
        Self { path: None, current: None, saved: None, save_at: None, native_maximized: || Some(false) }
    }
}

impl Tracker {
    pub fn new(path: PathBuf, restored: Option<WindowState>, native_maximized: fn() -> Option<bool>) -> Self {
        Self { path: Some(path), current: restored, saved: restored, save_at: None, native_maximized }
    }

    /// Observe native geometry and save it after external and user-driven placement has settled.
    pub fn observe(&mut self, ctx: &egui::Context, info: &egui::ViewportInfo) {
        let now = Instant::now();
        let maximized = if cfg!(target_os = "macos") { (self.native_maximized)() } else { Some(info.maximized == Some(true)) };
        let Some(state) = WindowState::from_viewport(info, ctx.zoom_factor(), maximized) else { return };
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
        let state = WindowState::from_viewport(&info(50.0, 25.0, 800.0, 600.0), 1.5, Some(false));
        assert_eq!(state, Some(WindowState { width: 1200.0, height: 900.0, x: 75.0, y: 37.5, scale_factor: 1.0 }));
    }

    #[test]
    fn viewport_geometry_retains_native_scale_for_windows_restore() {
        let mut viewport = info(1500.0, 100.0, 1000.0, 700.0);
        viewport.native_pixels_per_point = Some(2.0);
        let state = WindowState::from_viewport(&viewport, 1.0, Some(false));
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
        let mut tracker = Tracker::new(path.clone(), None, || Some(false));
        tracker.current = Some(state);
        tracker.save_now();
        assert_eq!(WindowState::load(&path), Some(state));

        let viewport = state.apply_to(egui::ViewportBuilder::default(), None);
        assert_eq!(viewport.inner_size, Some(egui::vec2(1440.0, 900.0)));
        assert_eq!(viewport.position, (!cfg!(target_os = "windows")).then_some(egui::pos2(120.0, 80.0)));
    }

    #[test]
    fn offscreen_position_moves_to_primary_display_before_creation() {
        let state = WindowState { width: 1440.0, height: 900.0, x: 2400.0, y: 80.0, scale_factor: 1.0 };
        let displays = [DisplayBounds { rect: egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1920.0, 1080.0)), scale_factor: 1.0, primary: true }];
        let viewport = state.apply_to(egui::ViewportBuilder::default(), Some(&displays));
        assert_eq!(viewport.position, (!cfg!(target_os = "windows")).then_some(egui::pos2(0.0, 0.0)));
    }

    #[test]
    fn position_on_attached_secondary_display_is_preserved() {
        let state = WindowState { width: 1440.0, height: 900.0, x: 2400.0, y: 80.0, scale_factor: 1.0 };
        let displays = [
            DisplayBounds { rect: egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1920.0, 1080.0)), scale_factor: 1.0, primary: true },
            DisplayBounds { rect: egui::Rect::from_min_size(egui::pos2(1920.0, 0.0), egui::vec2(2560.0, 1440.0)), scale_factor: 1.0, primary: false },
        ];
        let viewport = state.apply_to(egui::ViewportBuilder::default(), Some(&displays));
        assert_eq!(viewport.position, (!cfg!(target_os = "windows")).then_some(egui::pos2(2400.0, 80.0)));
    }

    #[test]
    fn windows_saved_position_uses_saved_scale_before_initial_creation() {
        let state = WindowState { width: 1000.0, height: 700.0, x: 1500.0, y: 100.0, scale_factor: 2.0 };
        let displays = [
            DisplayBounds { rect: egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1920.0, 1080.0)), scale_factor: 1.0, primary: true },
            DisplayBounds { rect: egui::Rect::from_min_size(egui::pos2(1920.0, 0.0), egui::vec2(3840.0, 2160.0)), scale_factor: 2.0, primary: false },
        ];

        assert_eq!(state.physical_position(Some(&displays)), Some([3000, 200]));
    }

    #[test]
    fn windows_saved_position_keeps_equal_scale_placement() {
        let state = WindowState { width: 1000.0, height: 700.0, x: 2400.0, y: 80.0, scale_factor: 1.0 };
        let displays = [
            DisplayBounds { rect: egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1920.0, 1080.0)), scale_factor: 1.0, primary: true },
            DisplayBounds { rect: egui::Rect::from_min_size(egui::pos2(1920.0, 0.0), egui::vec2(2560.0, 1440.0)), scale_factor: 1.0, primary: false },
        ];

        assert_eq!(state.physical_position(Some(&displays)), Some([2400, 80]));
    }

    #[test]
    fn windows_offscreen_position_falls_back_to_primary_pixels() {
        let state = WindowState { width: 1000.0, height: 700.0, x: 2400.0, y: 80.0, scale_factor: 2.0 };
        let displays =
            [DisplayBounds { rect: egui::Rect::from_min_size(egui::pos2(-1920.0, 0.0), egui::vec2(1920.0, 1080.0)), scale_factor: 1.0, primary: true }];

        assert_eq!(state.physical_position(Some(&displays)), Some([-1920, 0]));
    }

    #[test]
    fn logical_display_bounds_are_not_scaled_again() {
        let state = WindowState { width: 800.0, height: 700.0, x: 1000.0, y: 100.0, scale_factor: 2.0 };
        let displays = [DisplayBounds { rect: egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1920.0, 1080.0)), scale_factor: 1.0, primary: true }];
        assert_eq!(state.position_for_logical_displays(&displays), Some([1000.0, 100.0]));
    }

    #[test]
    fn native_maximize_does_not_replace_normal_geometry_on_macos() {
        if cfg!(target_os = "macos") {
            assert_eq!(WindowState::from_viewport(&info(0.0, 0.0, 1600.0, 900.0), 1.0, Some(true)), None);
        }
    }

    #[test]
    fn unknown_native_maximized_state_does_not_replace_normal_geometry() {
        assert_eq!(WindowState::from_viewport(&info(0.0, 0.0, 1600.0, 900.0), 1.0, None), None);
    }

    #[test]
    fn windows_position_uses_eframe_clamped_size_for_visibility() {
        let state = WindowState { width: 2400.0, height: 700.0, x: -2200.0, y: 100.0, scale_factor: 1.0 };
        let displays = [DisplayBounds { rect: egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1920.0, 1080.0)), scale_factor: 1.0, primary: true }];
        assert_eq!(state.physical_position(Some(&displays)), Some([0, 0]));
    }
}
