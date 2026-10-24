//! The file list the desktop puts on the system clipboard, for Edit ▸ Paste (#611).
//!
//! Files copied in a file manager arrive as `text/uri-list`. RFC 2483 §5 separates the URIs with
//! CRLF, and `arboard` splits the list on `\n` alone, so every path but the last keeps the `\r`
//! that ended its line (`file:///home/me/clip.mp4\r`). A path with that `\r` names no file, so
//! Paste produced "No such file or directory" for a file that is plainly there. The separators are
//! stripped here, before Paste hands the paths to the engine.

use std::path::PathBuf;

/// The clean paths of a `text/uri-list` a clipboard read returned. `\r` and `\n` left by the list's
/// separators are removed from both ends of each path, and an entry that is only a separator (the
/// blank line a trailing CRLF leaves, a skipped comment line) is dropped.
pub fn paste_paths(files: &[PathBuf]) -> Vec<String> {
    files.iter().filter_map(|f| clean(&f.to_string_lossy())).collect()
}

/// One path without the list separators around it, or `None` when nothing is left.
fn clean(raw: &str) -> Option<String> {
    let path = raw.trim_matches(['\r', '\n']);
    (!path.is_empty()).then(|| path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(list: &[&str]) -> Vec<String> {
        paste_paths(&list.iter().map(PathBuf::from).collect::<Vec<_>>())
    }

    #[test]
    fn a_crlf_uri_list_pastes_as_paths() {
        // `arboard` keeps the `\r` of the CRLF separators (RFC 2483 §5) on every line but the last.
        assert_eq!(
            paths(&["/home/me/clip a.mp4\r", "/home/me/clip b.mp4\r", "/home/me/clip c.mp4"]),
            ["/home/me/clip a.mp4", "/home/me/clip b.mp4", "/home/me/clip c.mp4"]
        );
        // The blank line a trailing CRLF leaves is not a path.
        assert_eq!(paths(&["/home/me/clip.mp4\r", ""]), ["/home/me/clip.mp4"]);
        // An LF-only list is unchanged, and spaces inside a path survive.
        assert_eq!(paths(&["/home/me/a b.mp4\n", "/home/me/c.mp4"]), ["/home/me/a b.mp4", "/home/me/c.mp4"]);
        // Nothing but separators pastes nothing instead of an empty path.
        assert!(paths(&["", "\r", "\n"]).is_empty());
    }
}
