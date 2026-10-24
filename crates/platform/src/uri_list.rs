//! Local file paths from a `text/uri-list` (RFC 2483), the format file managers hand over when
//! files are dropped on a window (Wayland drag-and-drop, `wayland_dnd`).
//!
//! One URI per line (CRLF or LF), `#` lines are comments. Only `file:` URIs naming this machine
//! (`file:///path`, `file://localhost/path`) are paths; anything else (`smb://`, `sftp://`, a
//! `file://` URI with another host) is skipped, since FilmCraft can only import local files.
//! Percent escapes are decoded to raw bytes, so names that are not UTF-8 survive on Unix.

use std::path::PathBuf;

/// At most this many paths come out of one drop; the rest are ignored.
pub const MAX_PATHS: usize = 10_000;

/// The local paths in `list`, in order. Malformed lines are skipped; never panics.
pub fn file_paths(list: &[u8]) -> Vec<PathBuf> {
    list.split(|&b| b == b'\n')
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line))
        .map(trim_ascii)
        .filter(|line| !line.is_empty() && !line.starts_with(b"#"))
        .filter_map(file_path)
        .take(MAX_PATHS)
        .collect()
}

fn trim_ascii(s: &[u8]) -> &[u8] {
    let start = s.iter().position(|b| !b.is_ascii_whitespace()).unwrap_or(s.len());
    let end = s.iter().rposition(|b| !b.is_ascii_whitespace()).map_or(start, |i| i.saturating_add(1));
    s.get(start..end).unwrap_or_default()
}

/// The path of one `file:` URI on this machine, or `None`.
fn file_path(uri: &[u8]) -> Option<PathBuf> {
    let scheme = uri.get(..5)?;
    if !scheme.eq_ignore_ascii_case(b"file:") {
        return None;
    }
    let rest = uri.get(5..)?;
    let path = match rest.strip_prefix(b"//") {
        // `file://host/path`: an empty host or `localhost` is this machine.
        Some(authority_and_path) => {
            let slash = authority_and_path.iter().position(|&b| b == b'/')?;
            let host = authority_and_path.get(..slash)?;
            if !host.is_empty() && !host.eq_ignore_ascii_case(b"localhost") {
                return None;
            }
            authority_and_path.get(slash..)?
        }
        // `file:/path` (no authority), which some senders use.
        None if rest.starts_with(b"/") => rest,
        None => return None,
    };
    // Query and fragment are not part of a file path.
    let end = path.iter().position(|&b| b == b'?' || b == b'#').unwrap_or(path.len());
    let decoded = percent_decode(path.get(..end)?);
    if decoded.is_empty() || decoded.contains(&0) {
        return None;
    }
    Some(bytes_to_path(decoded))
}

/// `%XX` escapes as bytes; a `%` without two hex digits after it stays as it is.
fn percent_decode(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while let Some(&b) = s.get(i) {
        if b == b'%'
            && let (Some(hi), Some(lo)) = (s.get(i + 1).and_then(|&h| hex(h)), s.get(i + 2).and_then(|&l| hex(l)))
        {
            out.push(hi << 4 | lo);
            i += 3;
        } else {
            out.push(b);
            i += 1;
        }
    }
    out
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(unix)]
fn bytes_to_path(bytes: Vec<u8>) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(std::ffi::OsString::from_vec(bytes))
}

#[cfg(not(unix))]
fn bytes_to_path(bytes: Vec<u8>) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(list: &str) -> Vec<String> {
        file_paths(list.as_bytes()).into_iter().map(|p| p.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn dolphin_style_list_with_crlf_and_escapes() {
        let list = "file:///home/oli/Videos/My%20Clip.mp4\r\nfile:///home/oli/Videos/caf%C3%A9.mov\r\n";
        assert_eq!(paths(list), ["/home/oli/Videos/My Clip.mp4", "/home/oli/Videos/café.mov"]);
    }

    #[test]
    fn localhost_lf_comments_and_blank_lines() {
        let list = "# from a file manager\nfile://localhost/tmp/a.wav\n\n  file:/tmp/b.png  \n";
        assert_eq!(paths(list), ["/tmp/a.wav", "/tmp/b.png"]);
    }

    #[test]
    fn remote_and_non_file_uris_are_skipped() {
        let list = "smb://server/share/a.mp4\nfile://otherhost/tmp/a.mp4\nhttps://example.com/a.mp4\nfile:relative.mp4\nFILE:///tmp/ok.mp4";
        assert_eq!(paths(list), ["/tmp/ok.mp4"]);
    }

    #[test]
    fn query_fragment_and_bad_escapes() {
        assert_eq!(paths("file:///tmp/a%2.mp4?x=1#frag"), ["/tmp/a%2.mp4"]);
        assert_eq!(paths("file:///tmp/100%"), ["/tmp/100%"]);
        // An escaped NUL can't be a path.
        assert!(paths("file:///tmp/a%00b").is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_names_keep_their_bytes() {
        use std::os::unix::ffi::OsStrExt;
        let p = file_paths(b"file:///tmp/%FF.mp4");
        assert_eq!(p.first().map(|p| p.as_os_str().as_bytes().to_vec()), Some(b"/tmp/\xFF.mp4".to_vec()));
    }

    #[test]
    fn hostile_input_never_panics_and_is_capped() {
        for list in [&b""[..], b"\r\n\r\n", b"file:", b"file://", b"file:///", b"%", b"file:///%", b"file:///%f", b"\xFF\xFE"] {
            let _ = file_paths(list);
        }
        let many = "file:///tmp/x\n".repeat(MAX_PATHS + 5);
        assert_eq!(file_paths(many.as_bytes()).len(), MAX_PATHS);
    }
}
