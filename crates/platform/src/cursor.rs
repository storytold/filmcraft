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

/// Whether [`cursor_screen_position`] reports points (macOS: AppKit's coordinates) rather than physical pixels.
pub const CURSOR_IN_POINTS: bool = cfg!(target_os = "macos");

/// macOS: winit's AppKit backend reports no pointer position while files are dragged in from Finder (#4296),
/// so read it from Quartz: global points, origin at the main display's top-left, as winit's window positions use.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub fn cursor_screen_position() -> Option<(i32, i32)> {
    use std::ffi::c_void;
    #[repr(C)]
    struct CGPoint {
        x: f64,
        y: f64,
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventCreate(source: *const c_void) -> *mut c_void;
        fn CGEventGetLocation(event: *const c_void) -> CGPoint;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
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
    (x.is_finite() && y.is_finite()).then_some((x.round() as i32, y.round() as i32))
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn cursor_screen_position() -> Option<(i32, i32)> {
    None
}
