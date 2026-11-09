//! The mouse position in screen pixels, for when the windowing layer has none.
//!
//! While files are dragged in from the OS file manager, winit on Windows delivers no cursor events
//! (it only reports hovered and dropped files), so the UI cannot learn where a drop landed.
//!
//! This is the one non-media OS call in this crate: `crates/platform` is the only crate allowed
//! `unsafe` (AGENTS.md §0.3, ADR 0001), so the single `GetCursorPos` call lives here rather than
//! in the UI. It is read-only, has a safe `Option`-returning API, and every other platform (and a
//! failed call) falls back to the last pointer position egui saw. Drop it once winit reports the
//! pointer position during an OS drag-and-drop.

/// The cursor's position on the virtual screen in physical pixels, or `None` where the OS cannot
/// say or this platform's windowing layer already reports it during a drag.
#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
pub fn cursor_screen_position() -> Option<(i32, i32)> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut p = POINT { x: 0, y: 0 };
    // SAFETY: `p` is a valid, writable POINT for the duration of the call.
    unsafe { GetCursorPos(&mut p) }.ok()?;
    Some((p.x, p.y))
}

/// Whether [`cursor_screen_position`] reports points rather than physical pixels: on macOS it reads
/// Quartz/CoreGraphics global points (`CGEventGetLocation`), not AppKit's bottom-left coordinates.
pub const CURSOR_IN_POINTS: bool = cfg!(target_os = "macos");

/// macOS: winit's AppKit backend reports no pointer position while files are dragged in from Finder (#4296),
/// so read it from Quartz: global points, origin at the main display's top-left, as winit's window positions use.
/// `None` when CoreGraphics returns a null event or a non-finite location; otherwise rounded to whole points
/// (negative on displays left of or above the main one), see [`quartz_point_to_px`].
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub fn cursor_screen_position() -> Option<(i32, i32)> {
    use std::ffi::c_void;
    /// CoreGraphics `CGPoint`: a location in global display points.
    #[repr(C)]
    struct CGPoint {
        x: f64,
        y: f64,
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        /// Creates an event stamped with the current input state (caller releases it).
        fn CGEventCreate(source: *const c_void) -> *mut c_void;
        /// The event's cursor location in global Quartz points.
        fn CGEventGetLocation(event: *const c_void) -> CGPoint;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        /// Releases a CoreFoundation object obtained under the Create rule.
        fn CFRelease(cf: *const c_void);
    }
    // SAFETY: a null source is allowed and yields an event stamped with the current state; the event is
    // checked for null, read once, and released exactly once (CGEventCreate follows the Create rule).
    let (x, y) = unsafe {
        let event = CGEventCreate(std::ptr::null());
        if event.is_null() {
            return None;
        }
        let p = CGEventGetLocation(event);
        CFRelease(event);
        (p.x, p.y)
    };
    quartz_point_to_px(x, y)
}

/// A Quartz global point rounded to whole points; `None` if either coordinate is non-finite. Out-of-range
/// values saturate (`as` casts), and negative coordinates (displays left of or above the main one) are kept.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn quartz_point_to_px(x: f64, y: f64) -> Option<(i32, i32)> {
    (x.is_finite() && y.is_finite()).then_some((x.round() as i32, y.round() as i32))
}

/// Elsewhere the windowing layer reports the pointer during a drag, so there is nothing to read.
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn cursor_screen_position() -> Option<(i32, i32)> {
    None
}

#[cfg(test)]
mod tests {
    use super::{CURSOR_IN_POINTS, quartz_point_to_px};

    /// Fractional points round to the nearest whole point.
    #[test]
    fn quartz_points_round_to_whole_points() {
        assert_eq!(quartz_point_to_px(10.4, 20.6), Some((10, 21)));
    }

    /// Displays left of or above the main one keep their negative coordinates.
    #[test]
    fn secondary_display_points_stay_negative() {
        assert_eq!(quartz_point_to_px(-1440.6, -12.2), Some((-1441, -12)));
    }

    /// NaN or infinite coordinates give no position.
    #[test]
    fn non_finite_points_are_none() {
        for (x, y) in [(f64::NAN, 0.0), (0.0, f64::INFINITY), (f64::NEG_INFINITY, f64::NAN)] {
            assert_eq!(quartz_point_to_px(x, y), None);
        }
    }

    /// Only macOS reports the cursor in points.
    #[test]
    fn only_macos_reports_points() {
        assert_eq!(CURSOR_IN_POINTS, cfg!(target_os = "macos"));
    }
}
