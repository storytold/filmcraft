//! Which windowing system to start on, on Linux.
//!
//! A Wayland session runs FilmCraft as a native Wayland client: sharp at fractional scales, and
//! files dropped from a file manager are received by `filmcraft_platform::wayland_dnd` (winit 0.30
//! reports none on Wayland). `FILMCRAFT_X11=1` starts on X11 (Xwayland) instead, where winit
//! delivers drops itself; the drop position then comes from the X server. Without Wayland, X11 is
//! used as before. A local `DISPLAY` with no X server listening (a stale `ssh -X` leftover, a
//! desktop with Xwayland off, a socket file left by a stopped server) falls back to Wayland rather
//! than failing with winit's "failed to open display". With no display at all, say so plainly.

use std::path::Path;

/// The windowing system to ask winit for.
#[derive(Debug, PartialEq, Eq)]
pub enum Choice {
    X11,
    /// Let winit decide (Wayland when `WAYLAND_DISPLAY` is set).
    Default,
    /// Neither `DISPLAY` nor `WAYLAND_DISPLAY`: there is nothing to open a window on.
    None,
}

/// The X server number of a local `DISPLAY` (`:1`, `:1.0`, `unix:1`); `None` for a remote one
/// (`host:10.0`, as `ssh -X` sets) or one we can't read, which are left to X11 itself.
fn local_display_number(display: &str) -> Option<u32> {
    let rest = display.strip_prefix(':').or_else(|| display.strip_prefix("unix:"))?;
    rest.split('.').next()?.parse().ok()
}

/// Choose from the environment. `listening` says whether an X server accepts connections on a
/// socket path ([`x_server_listening`] in [`current`], a stub in tests).
pub fn choose(display: Option<&str>, wayland: Option<&str>, force_x11: bool, listening: impl Fn(&Path) -> bool) -> Choice {
    let display = display.filter(|d| !d.is_empty());
    let wayland = wayland.is_some_and(|w| !w.is_empty());
    match display {
        None if wayland => Choice::Default,
        None => Choice::None,
        Some(_) if wayland && !force_x11 => Choice::Default,
        Some(d) => match local_display_number(d) {
            Some(n) if wayland && !listening(&Path::new("/tmp/.X11-unix").join(format!("X{n}"))) => {
                log::warn!("DISPLAY={d} has no X server; starting on Wayland");
                Choice::Default
            }
            _ => Choice::X11,
        },
    }
}

/// [`choose`] for this process, decided once: every window start attempt and the drop hooks agree
/// on it.
pub fn current() -> &'static Choice {
    static CHOICE: std::sync::OnceLock<Choice> = std::sync::OnceLock::new();
    CHOICE.get_or_init(|| {
        let display = std::env::var("DISPLAY").ok();
        let wayland = std::env::var("WAYLAND_DISPLAY").ok();
        let force_x11 = std::env::var_os("FILMCRAFT_X11").is_some_and(|v| !v.is_empty() && v != "0");
        choose(display.as_deref(), wayland.as_deref(), force_x11, x_server_listening)
    })
}

/// Whether an X server accepts a connection on the Unix socket at `path`: a socket file left by a
/// stopped server refuses it. Connecting and closing at once costs the server nothing.
fn x_server_listening(path: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wayland_session_runs_natively_unless_x11_is_asked_for() {
        let all = |_: &Path| true;
        assert_eq!(choose(Some(":0"), Some("wayland-0"), false, all), Choice::Default);
        assert_eq!(choose(Some(":0"), Some("wayland-0"), true, all), Choice::X11);
        assert_eq!(choose(None, Some("wayland-0"), false, all), Choice::Default);
        assert_eq!(choose(Some(""), Some("wayland-0"), true, all), Choice::Default);
    }

    #[test]
    fn x11_without_wayland_and_stale_displays() {
        let none = |_: &Path| false;
        let all = |_: &Path| true;
        assert_eq!(choose(Some(":1"), None, false, all), Choice::X11);
        assert_eq!(choose(Some(":1"), None, false, none), Choice::X11);
        assert_eq!(choose(Some("host:10.0"), Some("wayland-0"), true, none), Choice::X11);
        // Asked for X11, but the local X server is gone: Wayland rather than failing.
        assert_eq!(choose(Some(":1"), Some("wayland-0"), true, none), Choice::Default);
        assert_eq!(choose(Some("unix:0"), Some("wayland-0"), true, none), Choice::Default);
        // The socket checked is the display's own.
        assert_eq!(choose(Some(":3"), Some("w"), true, |p: &Path| p.ends_with("X3")), Choice::X11);
        assert_eq!(choose(Some(":3"), Some("w"), true, |p: &Path| p.ends_with("X0")), Choice::Default);
    }

    #[test]
    fn a_socket_file_with_no_server_behind_it_is_not_listening() {
        let dir = std::env::temp_dir().join(format!("filmcraft-display-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("X99");
        let _ = std::fs::remove_file(&path);
        assert!(!x_server_listening(&path), "no file");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        assert!(x_server_listening(&path), "a live listener");
        drop(listener);
        assert!(path.exists() && !x_server_listening(&path), "a stale socket file");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_display_at_all() {
        let all = |_: &Path| true;
        assert_eq!(choose(None, None, false, all), Choice::None);
        assert_eq!(choose(Some(""), Some(""), true, all), Choice::None);
    }
}
