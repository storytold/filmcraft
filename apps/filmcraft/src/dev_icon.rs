//! Dev-time Linux desktop entry: the shell icon fallback.
//!
//! The Linux shell (Wayland *and* X11) resolves the window to a `.desktop` file by app id (set in
//! `main.rs` via `eframe`'s `with_app_id`) and takes the taskbar / window-list icon from its
//! `Icon=` entry in the hicolor theme. Packaging installs both
//! (`packaging/linux/ai.storyteller.filmcraft.desktop` and the hicolor PNGs) — but a source-tree
//! run has neither, so KDE/GNOME fall back to the generic Wayland icon. The fix is to ensure a
//! user-level entry for those runs: when no entry for the app id exists in any XDG data dir,
//! install one (plus the hicolor icon PNGs, read from this checkout's `assets/app-icon`, so
//! nothing is embedded in the binary) into `$XDG_DATA_HOME`/`~/.local/share`. An existing
//! packaged entry is never overridden; the user-owned dev copy is rewritten when it points at
//! another binary (a dev build moves between checkouts). The shell picks the icon up at the *next* app start;
//! `update-desktop-database`/`kbuildsycoca` is not needed for icon lookup.
//!
//! Opt-in only: nothing is written unless `FILMCRAFT_DEV_DESKTOP_ENTRY=1` is set, so ordinary
//! `cargo run`s (agents, worktrees, CI) never touch the user's profile. Packaged runs are
//! excluded even then — AppImage (`APPIMAGE`) mounts at a fresh `/tmp/.mount_*` each launch,
//! so its `Exec` path changes every time and it ships its own entry, and a Flatpak
//! (`FLATPAK_ID`) installs its entry through the manifest. `packaging/linux/install.sh` writes the packaged
//! `Exec=` the same two-layer way as here; keep the two in sync.
//!
//! Every failure is logged and skipped — a missing dev icon must never stop the app
//! ([`AGENTS.md`](/AGENTS.md) §0 "never crash").

/// Install the user-level desktop entry and icons for a source-tree run (Linux).
pub fn ensure_dev_desktop_entry() {
    // Packaged and sandboxed runs never write into the user's profile: an AppImage mounts at a
    // fresh /tmp/.mount_* every launch (the Exec path changes each time) and a Flatpak installs
    // its own entry.
    if std::env::var_os("APPIMAGE").is_some() || std::env::var_os("FLATPAK_ID").is_some() {
        return;
    }
    // Opt-in only: a plain `cargo run` (agents, worktrees, other checkouts) must not write into
    // the user's profile, and a packaged release never does (packages and install.sh install
    // the real entry).
    if std::env::var_os("FILMCRAFT_DEV_DESKTOP_ENTRY").is_none_or(|v| v != "1") {
        return;
    }
    if let Err(e) = ensure() {
        log::warn!("desktop entry (taskbar icon) not installed: {e}");
    }
}

/// Install missing files; `Ok` whether anything was written or an entry already existed.
fn ensure() -> Result<(), DevIconError> {
    let file_name = format!("{}.desktop", crate::APP_ID);
    let user_dir = user_data_dir()?;
    let exe = std::env::current_exe().map_err(DevIconError::Io)?;
    // Not `to_string_lossy`: a substituted U+FFFD would install an `Exec` that can never start
    // the app, and the same lossy string would match on every later run, so the broken entry
    // would never be rewritten or warned about.
    let exe = exe.to_str().ok_or(DevIconError::BadExecPath)?;
    if exe.contains('=') || exe.contains('%') || exe.chars().any(|c| c.is_control()) {
        // A desktop entry cannot carry these (install.sh rejects them for the same reason):
        // '=' separates key and value, '%' introduces a field code, control characters are
        // not valid in the string layer.
        return Err(DevIconError::BadExecPath);
    }
    for dir in search_dirs(&user_dir) {
        let candidate = dir.join("applications").join(&file_name);
        if !candidate.is_file() {
            continue;
        }
        // A packaged entry (any system dir) already gives the shell its icon; only our own
        // user-level copy is kept fresh.
        if dir != user_dir {
            return Ok(());
        }
        let body = std::fs::read_to_string(&candidate).map_err(DevIconError::Io)?;
        if desktop_file_exec_is_ours(&body, exe) {
            return Ok(());
        }
        break; // a dev entry from another checkout: fall through and rewrite it
    }
    for size in ICON_SIZES {
        let rel = std::path::Path::new("hicolor").join(format!("{size}x{size}")).join("apps").join(format!("{}.png", crate::APP_ID));
        let dest = user_dir.join("icons").join(&rel);
        if !dest.is_file()
            && let Some(parent) = dest.parent()
        {
            // The checkout this binary was built from; a missing icon only costs the icon.
            let src = std::path::Path::new(ICON_DIR).join(&rel);
            if !src.is_file() {
                log::warn!("dev desktop entry: icon {} not found; skipping it", src.display());
                continue;
            }
            std::fs::create_dir_all(parent).map_err(DevIconError::Io)?;
            std::fs::copy(&src, &dest).map_err(DevIconError::Io)?;
        }
    }
    let applications = user_dir.join("applications");
    std::fs::create_dir_all(&applications).map_err(DevIconError::Io)?;
    let body = desktop_file_content(crate::APP_ID, "FilmCraft", exe);
    std::fs::write(applications.join(&file_name), body).map_err(DevIconError::Io)?;
    Ok(())
}

/// The hicolor icon sizes installed for the taskbar (a subset large enough for panel and topbar;
/// the shell picks the nearest size, so 256 and 512 cover both).
const ICON_SIZES: [u16; 2] = [256, 512];

/// The app icons in the source tree this binary was built from (a path, not embedded bytes:
/// the entry is a dev-checkout convenience and release binaries must not carry the PNGs).
const ICON_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/app-icon");

/// The user data dir we install into: `$XDG_DATA_HOME`, else `$HOME/.local/share` (XDG base
/// directory spec). Untrusted environment: an empty or relative `XDG_DATA_HOME` means the
/// default, and a missing or relative `HOME` is an error — never a guess that would write
/// into the working directory.
fn user_data_dir() -> Result<std::path::PathBuf, DevIconError> {
    if let Some(v) = std::env::var_os("XDG_DATA_HOME")
        && !v.is_empty()
        && std::path::Path::new(&v).is_absolute()
    {
        return Ok(std::path::PathBuf::from(v));
    }
    match std::env::var_os("HOME").filter(|h| !h.is_empty() && std::path::Path::new(h).is_absolute()) {
        Some(home) => Ok(std::path::PathBuf::from(home).join(".local").join("share")),
        None => Err(DevIconError::NoHome),
    }
}

/// The XDG data dirs to search for an existing entry, user dir first, then `$XDG_DATA_DIRS` (or
/// the spec default `/usr/local/share:/usr/share`). Only absolute paths are taken; the spec
/// requires them and a relative one would read the working directory.
fn search_dirs(user: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut dirs = vec![user.to_path_buf()];
    let system = std::env::var_os("XDG_DATA_DIRS")
        .filter(|v| !v.is_empty())
        .map(|v| v.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    for dir in system.split(':') {
        if std::path::Path::new(dir).is_absolute() {
            dirs.push(std::path::PathBuf::from(dir));
        }
    }
    dirs
}

/// Whether the `.desktop` body's `Exec` points at our binary exactly (a stale dev entry from
/// another checkout — `/a/filmcraft-old` is not `/a/filmcraft` — does not, and is rewritten; a
/// packaged `Exec=filmcraft` never matches a source-tree path).
fn desktop_file_exec_is_ours(body: &str, exe: &str) -> bool {
    exec_line(body).is_some_and(|arg| arg == exe)
}

/// The first `Exec=` argument (the executable), unescaped through both Desktop Entry layers:
/// first the string layer of the file value (`\\` `\s` `\n` `\t` `\r`, per
/// <https://specifications.freedesktop.org/desktop-entry/latest/>), then the shell layer of the
/// argument (double quotes, and `\` before `` ` `"` `\` `$` inside them). `None` when there is no
/// `Exec=` line or its first argument cannot be parsed.
fn exec_line(body: &str) -> Option<String> {
    let value = body.lines().find(|l| l.starts_with("Exec=")).and_then(|l| l.strip_prefix("Exec="))?;
    let value = unescape_string_value(value);
    if let Some(rest) = value.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = rest.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => return Some(out),
                '\\' => match chars.next() {
                    Some(e @ ('\\' | '"' | '`' | '$')) => out.push(e),
                    Some(e) => {
                        out.push('\\');
                        out.push(e);
                    }
                    None => return None, // a dangling escape ends the argument
                },
                c => out.push(c),
            }
        }
        None // no closing quote
    } else {
        Some(value.split(' ').next().unwrap_or_default().to_string())
    }
}

/// Undo the desktop entry's string layer: `\\` `\s` `\n` `\t` `\r` are the defined sequences; a
/// backslash before anything else is kept as the character itself (lenient, like desktop file
/// parsers in the wild).
fn unescape_string_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(e) => out.push(e),
            None => out.push('\\'),
        }
    }
    out
}

/// The `.desktop` body: same fields as `packaging/linux/ai.storyteller.filmcraft.desktop`, with
/// `Exec` pointing at this exact binary (a dev build isn't on `$PATH`, so `TryExec` is left out)
/// and the icon by theme name.
fn desktop_file_content(app_id: &str, name: &str, exe: &str) -> String {
    let mut out = String::new();
    out.push_str("[Desktop Entry]\n");
    out.push_str("Type=Application\n");
    out.push_str(&format!("Name={name}\n"));
    out.push_str("GenericName=Video Editor\n");
    out.push_str("Comment=Edit video, color and sound\n");
    out.push_str(&format!("Exec=\"{}\" %F\n", escape_exec_argument(exe)));
    out.push_str(&format!("Icon={app_id}\n"));
    out.push_str("Terminal=false\n");
    out.push_str("StartupNotify=true\n");
    out.push_str(&format!("StartupWMClass={app_id}\n"));
    out.push_str("Categories=AudioVideo;Video;AudioVideoEditing;\n");
    out.push_str("Keywords=video;editor;film;timeline;color;grading;nle;\n");
    out
}

/// Escape `s` for the `Exec=` value's two layers, like `packaging/linux/install.sh`: first the
/// shell layer (`` ` `` `"` `$` `\` get a backslash, and the argument is double-quoted), then
/// the string layer of the file value (every backslash doubled).
fn escape_exec_argument(s: &str) -> String {
    let mut shell = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        if matches!(c, '`' | '"' | '$' | '\\') {
            shell.push('\\');
        }
        shell.push(c);
    }
    shell.replace('\\', "\\\\")
}

/// Everything that can keep the dev icon out of the taskbar; logged, never fatal.
#[derive(Debug)]
enum DevIconError {
    Io(std::io::Error),
    NoHome,
    BadExecPath,
}

impl std::fmt::Display for DevIconError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DevIconError::Io(e) => write!(f, "{e}"),
            DevIconError::NoHome => write!(f, "no home directory for the user data dir"),
            DevIconError::BadExecPath => {
                write!(f, "the binary path has a character a desktop entry cannot carry (=, % or a control character), or is not valid UTF-8")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The entry mirrors the packaged one, derived from the real file so the two cannot drift:
    /// minus `TryExec` (a dev build isn't on `$PATH`), plus the quoted dev `Exec` path.
    /// The icons the entry installs exist in the checkout.
    #[test]
    fn icon_sources_exist_in_the_checkout() {
        for size in ICON_SIZES {
            let p = std::path::Path::new(ICON_DIR).join(format!("hicolor/{size}x{size}/apps/ai.storyteller.filmcraft.png"));
            assert!(p.is_file(), "{}", p.display());
        }
    }

    #[test]
    fn desktop_file_matches_the_packaged_fields() {
        let packaged = include_str!("../../../packaging/linux/ai.storyteller.filmcraft.desktop");
        let expected = packaged
            .lines()
            .filter(|l| !l.starts_with("TryExec="))
            .map(|l| if l.starts_with("Exec=") { "Exec=\"/target/debug/filmcraft\" %F" } else { l })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let body = desktop_file_content("ai.storyteller.filmcraft", "FilmCraft", "/target/debug/filmcraft");
        assert_eq!(body, expected);
    }

    /// Both layers are escaped, like install.sh: shell first (`` ` `` `"` `$` `\` get a
    /// backslash), then *every* backslash doubled for the file value — including the escape
    /// backslashes just added.
    #[test]
    fn exec_escapes_both_desktop_entry_layers() {
        assert_eq!(escape_exec_argument("/opt/film craft/filmcraft"), "/opt/film craft/filmcraft");
        // a\b → shell a\\b → string a\\\\b
        assert_eq!(escape_exec_argument("a\\b"), "a\\\\\\\\b");
        // a$b → shell a\$b → string a\\$b
        assert_eq!(escape_exec_argument("a$b"), "a\\\\$b");
    }

    /// A written entry reads back as the exact binary it was written for, through both layers.
    #[test]
    fn exec_round_trips_through_both_layers() {
        for exe in ["/home/dev/Projects/filmcraft/target/debug/filmcraft", "/opt/film craft/filmcraft", "/a$b\"c`d\\e"] {
            let body = desktop_file_content("ai.storyteller.filmcraft", "FilmCraft", exe);
            assert_eq!(exec_line(&body).as_deref(), Some(exe));
            assert!(desktop_file_exec_is_ours(&body, exe));
        }
    }

    /// Only the exact executable counts: another checkout's path, a path with our path as a
    /// prefix, and a packaged `Exec=filmcraft` are all someone else's entry.
    #[test]
    fn desktop_file_exec_is_ours_matches_only_our_binary() {
        let body = desktop_file_content("ai.storyteller.filmcraft", "FilmCraft", "/a/filmcraft");
        assert!(!desktop_file_exec_is_ours(&body, "/a/filmcraft-old"));
        assert!(!desktop_file_exec_is_ours(&body, "/b/filmcraft"));
        assert!(!desktop_file_exec_is_ours("Exec=filmcraft %F\n", "/a/filmcraft"));
        assert!(!desktop_file_exec_is_ours("Name=filmcraft\n", "/a/filmcraft"));
        assert!(!desktop_file_exec_is_ours("Exec=\"dangling\n", "/a/filmcraft"));
    }

    /// The string layer is undone before the argument is split: `\s` becomes a space, which
    /// ends a bare argument (the shell layer's quoting, not `\s`, is how a space travels), and
    /// `\\` is one backslash. install.sh always quotes, so a space in our own `Exec` never
    /// splits.
    #[test]
    fn exec_reads_the_string_layer() {
        assert_eq!(exec_line("Exec=/a\\sb %F\n").as_deref(), Some("/a"));
        assert_eq!(exec_line("Exec=\"/a\\sb\" %F\n").as_deref(), Some("/a b"));
        assert_eq!(exec_line("Exec=/a\\\\b %F\n").as_deref(), Some("/a\\b"));
    }
}
