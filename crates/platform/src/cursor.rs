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

#[cfg(not(target_os = "windows"))]
pub fn cursor_screen_position() -> Option<(i32, i32)> {
    None
}
