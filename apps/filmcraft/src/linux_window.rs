//! File-manager drops on Linux. winit 0.30's Wayland backend does not deliver file drops;
//! use X11 (including XWayland) when a connection succeeds. Native Wayland remains available
//! without X11 or explicitly via FILMCRAFT_WINDOW_BACKEND=wayland. No environment mutation.
//! X11 also omits cursor motion during a file drag, so query its position through safe x11rb.

use std::ffi::OsStr;
use std::sync::Arc;

use winit::raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::ConnectionExt;
use x11rb::rust_connection::RustConnection;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    X11,
    Wayland,
}

fn requested_backend(value: Option<&OsStr>) -> Result<Option<Backend>, String> {
    match value {
        None => Ok(None),
        Some(v) if v == "x11" => Ok(Some(Backend::X11)),
        Some(v) if v == "wayland" => Ok(Some(Backend::Wayland)),
        Some(v) => Err(format!("FILMCRAFT_WINDOW_BACKEND must be x11 or wayland, got {v:?}")),
    }
}

pub fn backend() -> Result<Option<Backend>, String> {
    let requested = std::env::var_os("FILMCRAFT_WINDOW_BACKEND");
    if let Some(backend) = requested_backend(requested.as_deref())? {
        return Ok(Some(backend));
    }
    if std::env::var_os("DISPLAY").is_some_and(|v| !v.is_empty()) {
        match Cursor::connect() {
            Ok(_) => {
                log::info!("using X11/XWayland for file-manager drag and drop");
                return Ok(Some(Backend::X11));
            }
            Err(e) => log::warn!("X11 unavailable; using the default window backend: {e}"),
        }
    }
    Ok(None)
}

pub fn event_loop_hook(backend: Backend) -> eframe::EventLoopBuilderHook {
    Box::new(move |builder| match backend {
        Backend::X11 => {
            use winit::platform::x11::EventLoopBuilderExtX11;
            builder.with_x11();
        }
        Backend::Wayland => {
            use winit::platform::wayland::EventLoopBuilderExtWayland;
            builder.with_wayland();
        }
    })
}

struct Cursor {
    connection: RustConnection,
    root: u32,
}

impl Cursor {
    fn connect() -> Result<Self, String> {
        let (connection, screen) = x11rb::connect(None).map_err(|e| e.to_string())?;
        let root = connection.setup().roots.get(screen).ok_or("X11 returned an invalid screen index")?.root;
        Ok(Self { connection, root })
    }

    fn position(&self) -> Option<(i32, i32)> {
        let reply = self.connection.query_pointer(self.root).ok()?.reply().ok()?;
        reply.same_screen.then_some((i32::from(reply.root_x), i32::from(reply.root_y)))
    }
}

pub fn cursor_hook(window: &winit::window::Window) -> Option<Box<dyn Fn() -> Option<(i32, i32)>>> {
    // A native Wayland window must not use coordinates from another display's X11 root.
    let display = window.display_handle().ok()?;
    if !matches!(display.as_raw(), RawDisplayHandle::Xlib(_) | RawDisplayHandle::Xcb(_)) {
        return None;
    }
    match Cursor::connect() {
        Ok(cursor) => {
            let cursor = Arc::new(cursor);
            Some(Box::new(move || cursor.position()))
        }
        Err(e) => {
            log::warn!("file-drop cursor unavailable: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_backend_is_validated_without_changing_the_environment() {
        assert_eq!(requested_backend(None).unwrap(), None);
        assert_eq!(requested_backend(Some(OsStr::new("x11"))).unwrap(), Some(Backend::X11));
        assert_eq!(requested_backend(Some(OsStr::new("wayland"))).unwrap(), Some(Backend::Wayland));
        for value in ["", "auto", "X11", "unknown", "wayland\0x11"] {
            let error = requested_backend(Some(OsStr::new(value))).unwrap_err();
            assert!(error.contains("FILMCRAFT_WINDOW_BACKEND"));
        }
        use std::os::unix::ffi::OsStrExt;
        assert!(requested_backend(Some(OsStr::from_bytes(&[0xff]))).is_err());
    }
}
