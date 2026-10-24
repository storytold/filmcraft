//! Media paths stored in a document (a project, a project template, an interchange file) reach
//! the file system only through [`resolve`]: a relative path is taken from the document's folder,
//! and only local files, or files on the network share the document itself is on, are opened (see
//! `craft_docpath`). A refused path is handled like a missing file: the item renders the offline
//! slate and File ▸ Link Media… can reconnect it.
//!
//! Paths the user picks in this session (import, Link Media, file dialogs, the control channel)
//! are not stored-document paths and are followed as given. The media pool tells them apart
//! ([`crate::MediaPool::follow`]): it records the paths of each document it loads, with that
//! document, and forgets a path once the user picks it again.

use std::fmt;
use std::path::{Path, PathBuf};

pub use craft_docpath::Refused;

/// Why a stored path is not followed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotFollowed {
    /// The path is not a local file, or is not on the document's own share.
    Refused(Refused),
    /// The path is relative and the document has no file to take it from.
    NoDocumentFolder,
}

impl fmt::Display for NotFollowed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(r) => r.fmt(f),
            Self::NoDocumentFolder => f.write_str("the path is relative and the project has not been saved"),
        }
    }
}

impl std::error::Error for NotFollowed {}

/// The file to open for `stored`, a path or `file:` URL stored in the document at `document`
/// (None: a document without a file). A relative `document` is taken from the working directory,
/// where it was opened from.
pub fn resolve(stored: &str, document: Option<&Path>) -> Result<String, NotFollowed> {
    let document = document.filter(|d| !d.as_os_str().is_empty()).map(|d| std::path::absolute(d).unwrap_or_else(|_| d.to_path_buf()));
    let path: PathBuf = match craft_docpath::resolve_absolute(stored, document.as_deref()) {
        Ok(path) => path,
        // Without a document, a relative path resolves but stays relative.
        Err(Refused::Invalid) if document.is_none() && craft_docpath::resolve(stored, None).is_ok_and(|p| !foreign_root(&p)) => {
            return Err(NotFollowed::NoDocumentFolder);
        }
        Err(r) => return Err(NotFollowed::Refused(r)),
    };
    path.into_os_string().into_string().map_err(|_| NotFollowed::Refused(Refused::Invalid))
}

/// Whether an unresolved `craft_docpath::resolve` result starts with a drive (`C:`) or a share
/// root (`\\`) outside Windows. Such a path names no file here, and no document folder makes it one.
fn foreign_root(path: &Path) -> bool {
    if cfg!(windows) {
        return false;
    }
    let b = path.as_os_str().as_encoded_bytes();
    b.starts_with(br"\\") || matches!((b.first(), b.get(1)), (Some(l), Some(b':')) if l.is_ascii_alphabetic())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_are_taken_from_the_document_folder() {
        let dir = std::env::temp_dir().join("films");
        let doc = dir.join("cut.fcproj");
        assert_eq!(resolve("clip.mp4", Some(&doc)), Ok(dir.join("clip.mp4").to_string_lossy().into_owned()));
        assert_eq!(resolve("clip.mp4", None), Err(NotFollowed::NoDocumentFolder));
        assert_eq!(resolve(r"\\192.0.2.1\share\x.mp4", Some(&doc)), Err(NotFollowed::Refused(Refused::Network)));
        assert_eq!(resolve("file://192.0.2.1/s/x.mp4", Some(&doc)), Err(NotFollowed::Refused(Refused::Network)));
        assert_eq!(resolve("jar:take.mp4", Some(&doc)), Err(NotFollowed::Refused(Refused::Url)));
        // Colons in names: an alternate data stream or a URL on Windows, a plain character elsewhere.
        if cfg!(windows) {
            assert_eq!(resolve("media/take:2.mp4", Some(&doc)), Err(NotFollowed::Refused(Refused::Invalid)));
            assert_eq!(resolve("take:2.mp4", Some(&doc)), Err(NotFollowed::Refused(Refused::Url)));
        } else {
            assert_eq!(resolve("media/take:2.mp4", Some(&doc)), Ok(dir.join("media").join("take:2.mp4").to_string_lossy().into_owned()));
            assert_eq!(resolve("take:2.mp4", Some(&doc)), Ok(dir.join("take:2.mp4").to_string_lossy().into_owned()));
        }
    }

    /// Without a document a relative path waits for the document's folder; outside Windows a
    /// drive path names no file and is refused.
    #[test]
    fn foreign_drive_paths_without_a_document_are_refused() {
        for stored in ["clip.mp4", "../clip.mp4"] {
            assert_eq!(resolve(stored, None), Err(NotFollowed::NoDocumentFolder), "{stored}");
        }
        #[cfg(unix)]
        for stored in [r"C:\x", r"C:x", "file:///C:/x/clip.mp4"] {
            assert_eq!(resolve(stored, None), Err(NotFollowed::Refused(Refused::Invalid)), "{stored}");
        }
    }
}
