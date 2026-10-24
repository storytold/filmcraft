//! Which display server the window opens on (Linux).
//!
//! winit 0.30 — what eframe 0.36 uses — has no drag-and-drop on its native Wayland backend:
//! `WindowEvent::DroppedFile` / `HoveredFile` come only from its X11, Windows and macOS backends
//! (`winit/src/platform_impl/linux/`; upstream [rust-windowing/winit#1881]), so a file dragged in
//! from the desktop does nothing on a Wayland session. XWayland (the X11 backend) delivers drops,
//! so a Wayland session opens through it unless the user keeps native Wayland with
//! `FILMCRAFT_NATIVE_WAYLAND=1`. Native Wayland drops return once egui moves to winit 0.31, where
//! Wayland drag-and-drop landed ([rust-windowing/winit#4571]).
//!
//! The decision reads the environment only, so it is a pure function that tests cover.
//!
//! [rust-windowing/winit#1881]: https://github.com/rust-windowing/winit/issues/1881
//! [rust-windowing/winit#4571]: https://github.com/rust-windowing/winit/pull/4571

/// Whether the session is Wayland, exactly as winit decides it: `WAYLAND_DISPLAY` or
/// `WAYLAND_SOCKET` is set to a non-empty value (an empty one counts as unset).
pub fn is_wayland_session(get: impl Fn(&str) -> Option<String>) -> bool {
    ["WAYLAND_DISPLAY", "WAYLAND_SOCKET"].iter().any(|k| get(k).is_some_and(|v| !v.is_empty()))
}

/// Whether to open the window through X11/XWayland instead of the session's own display server:
/// a Wayland session where XWayland is available (`DISPLAY` is set), unless `FILMCRAFT_NATIVE_WAYLAND`
/// keeps native Wayland. Off Wayland (a plain X11 session), or with no XWayland, the session's own
/// back end is used as before.
pub fn prefer_x11(get: impl Fn(&str) -> Option<String>) -> bool {
    let set = |k: &str| get(k).is_some_and(|v| !v.is_empty() && v != "0");
    is_wayland_session(&get) && !set("FILMCRAFT_NATIVE_WAYLAND") && set("DISPLAY")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: std::collections::HashMap<String, String> = pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
        move |k: &str| map.get(k).cloned()
    }

    #[test]
    fn only_a_wayland_session_with_xwayland_prefers_x11() {
        // A Wayland session with XWayland opens through it: drops arrive there, not on Wayland.
        assert!(prefer_x11(env(&[("WAYLAND_DISPLAY", "wayland-0"), ("DISPLAY", ":0")])));
        // A `WAYLAND_SOCKET` session counts as Wayland too (winit accepts either).
        assert!(prefer_x11(env(&[("WAYLAND_SOCKET", "12"), ("DISPLAY", ":0")])));
        // A plain X11 session is left alone.
        assert!(!prefer_x11(env(&[("DISPLAY", ":0")])));
        // No XWayland (`DISPLAY` unset): stay on Wayland rather than fail to open a window.
        assert!(!prefer_x11(env(&[("WAYLAND_DISPLAY", "wayland-0")])));
        // The escape hatch keeps native Wayland, whatever else is set.
        assert!(!prefer_x11(env(&[("WAYLAND_DISPLAY", "wayland-0"), ("DISPLAY", ":0"), ("FILMCRAFT_NATIVE_WAYLAND", "1")])));
        // Empty values count as unset, exactly as winit treats them.
        assert!(!prefer_x11(env(&[("WAYLAND_DISPLAY", ""), ("DISPLAY", ":0")])));
        assert!(!prefer_x11(env(&[("WAYLAND_DISPLAY", "wayland-0"), ("DISPLAY", "")])));
        // `FILMCRAFT_NATIVE_WAYLAND=0` is not an opt-out; any other non-empty value is.
        assert!(prefer_x11(env(&[("WAYLAND_DISPLAY", "wayland-0"), ("DISPLAY", ":0"), ("FILMCRAFT_NATIVE_WAYLAND", "0")])));
        assert!(!prefer_x11(env(&[("WAYLAND_DISPLAY", "wayland-0"), ("DISPLAY", ":0"), ("FILMCRAFT_NATIVE_WAYLAND", "yes")])));
    }
}
