//! The mouse position in screen pixels, for when the windowing layer has none.
//!
//! While files are dragged in from the OS file manager, winit on Windows and X11 delivers no cursor
//! events (it only reports hovered and dropped files), so the UI cannot learn where a drop landed.
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

/// The cursor's position on the X11 root window, for a window winit opened on X11 (or Xwayland):
/// the file manager grabs the pointer during an XDND drag, so we see no motion, but the X server
/// still answers `QueryPointer`. Safe Rust over x11rb's own protocol code; the connection is
/// opened once, on first use. `None` without an X server or when the query fails. Not for a
/// Wayland window: Xwayland only knows where the pointer last was over an X11 window.
#[cfg(target_os = "linux")]
pub fn x11_cursor_screen_position() -> Option<(i32, i32)> {
    use std::sync::OnceLock;
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::ConnectionExt;
    use x11rb::rust_connection::RustConnection;
    static CONNECTION: OnceLock<Option<(RustConnection, u32)>> = OnceLock::new();
    let (conn, root) = CONNECTION
        .get_or_init(|| {
            let (conn, screen) = RustConnection::connect(None).map_err(|e| log::warn!("no X11 connection for the cursor position: {e}")).ok()?;
            let root = conn.setup().roots.get(screen)?.root;
            Some((conn, root))
        })
        .as_ref()?;
    let reply = conn.query_pointer(*root).ok()?.reply().ok()?;
    Some((i32::from(reply.root_x), i32::from(reply.root_y)))
}
