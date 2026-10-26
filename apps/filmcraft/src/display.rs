//! Which display server the window opens on (Linux).
//!
//! The `storytold/winit` fork this app pins reports dropped files from its Wayland backend through
//! the core `wl_data_device` protocol (patch W15, the 0.30 backport of
//! [rust-windowing/winit#4729]): `WindowEvent::DroppedFile` / `HoveredFile` arrive on a Wayland
//! session, so it opens natively and a file dragged in from the desktop is taken as it is on X11.
//! (The crates.io winit 0.30 the released build uses has no Wayland drops —
//! [rust-windowing/winit#1881] — which is why an XWayland fallback stays.)
//!
//! `FILMCRAFT_XWAYLAND=1` still opens a Wayland session through XWayland (the X11 backend) for
//! sessions where Wayland drag-and-drop misbehaves; that needs XWayland, so it also requires
//! `DISPLAY`.
//!
//! The decision reads the environment only, so it is a pure function that tests cover.
//!
//! [rust-windowing/winit#1881]: https://github.com/rust-windowing/winit/issues/1881
//! [rust-windowing/winit#4729]: https://github.com/rust-windowing/winit/pull/4729

/// Whether the session is Wayland, exactly as winit decides it: `WAYLAND_DISPLAY` or
/// `WAYLAND_SOCKET` is set to a non-empty value (an empty one counts as unset).
pub fn is_wayland_session(get: impl Fn(&str) -> Option<String>) -> bool {
    ["WAYLAND_DISPLAY", "WAYLAND_SOCKET"].iter().any(|k| get(k).is_some_and(|v| !v.is_empty()))
}

/// Whether to open the window through X11/XWayland instead of the session's own display server:
/// only a Wayland session that opts in with `FILMCRAFT_XWAYLAND` where XWayland is available
/// (`DISPLAY` is set). Off Wayland (a plain X11 session), or without the opt-in, the session's own
/// back end is used.
pub fn prefer_x11(get: impl Fn(&str) -> Option<String>) -> bool {
    let set = |k: &str| get(k).is_some_and(|v| !v.is_empty() && v != "0");
    is_wayland_session(&get) && set("FILMCRAFT_XWAYLAND") && set("DISPLAY")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A lookup over a fixed environment, in the shape `prefer_x11` reads.
    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: std::collections::HashMap<String, String> = pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
        move |k: &str| map.get(k).cloned()
    }

    #[test]
    /// Only `FILMCRAFT_XWAYLAND` moves a Wayland session to XWayland, and only when XWayland is there.
    fn xwayland_is_opted_in_on_a_wayland_session() {
        // A Wayland session opens natively by default.
        assert!(!prefer_x11(env(&[("WAYLAND_DISPLAY", "wayland-0"), ("DISPLAY", ":0")])));
        // `FILMCRAFT_XWAYLAND=1` opens through XWayland.
        assert!(prefer_x11(env(&[("WAYLAND_DISPLAY", "wayland-0"), ("DISPLAY", ":0"), ("FILMCRAFT_XWAYLAND", "1")])));
        // A `WAYLAND_SOCKET` session counts as Wayland too (winit accepts either).
        assert!(prefer_x11(env(&[("WAYLAND_SOCKET", "12"), ("DISPLAY", ":0"), ("FILMCRAFT_XWAYLAND", "1")])));
        // A plain X11 session is left alone.
        assert!(!prefer_x11(env(&[("DISPLAY", ":0"), ("FILMCRAFT_XWAYLAND", "1")])));
        // No XWayland (`DISPLAY` unset): stay on native Wayland rather than fail to open a window.
        assert!(!prefer_x11(env(&[("WAYLAND_DISPLAY", "wayland-0"), ("FILMCRAFT_XWAYLAND", "1")])));
        // Empty values count as unset, exactly as winit treats them.
        assert!(!prefer_x11(env(&[("WAYLAND_DISPLAY", ""), ("DISPLAY", ":0"), ("FILMCRAFT_XWAYLAND", "1")])));
        assert!(!prefer_x11(env(&[("WAYLAND_DISPLAY", "wayland-0"), ("DISPLAY", ""), ("FILMCRAFT_XWAYLAND", "1")])));
        // `FILMCRAFT_XWAYLAND=0` is not an opt-in; any other non-empty value is.
        assert!(!prefer_x11(env(&[("WAYLAND_DISPLAY", "wayland-0"), ("DISPLAY", ":0"), ("FILMCRAFT_XWAYLAND", "0")])));
        assert!(prefer_x11(env(&[("WAYLAND_DISPLAY", "wayland-0"), ("DISPLAY", ":0"), ("FILMCRAFT_XWAYLAND", "yes")])));
    }
}
