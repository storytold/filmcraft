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
//!
//! The network shares the user names in Settings ▸ Media ▸ Trusted Network Folders
//! ([`trusted_shares`]) are followed from any document, as the document's own share is
//! ([`resolve_trusting`]): a project saved on the local disk, or an interchange document imported
//! from it, keeps finding its media on a NAS.

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

/// [`resolve`], also following a network path on one of the `trusted` shares (`\\server\share`
/// roots, [`trusted_shares`]). A trusted share is treated as the document's own share would be,
/// with the same lexical checks (`craft_docpath`), so `..` cannot step out of it.
pub fn resolve_trusting(stored: &str, document: Option<&Path>, trusted: &[String]) -> Result<String, NotFollowed> {
    match resolve(stored, document) {
        Err(NotFollowed::Refused(Refused::Network)) => trusted
            .iter()
            .find_map(|root| {
                // A document at the share's root: paths on the same share are followed.
                let doc = format!("{}/_", root.trim_end_matches(['\\', '/']));
                craft_docpath::resolve_absolute(stored, Some(Path::new(&doc))).ok()
            })
            .and_then(|p| p.into_os_string().into_string().ok())
            .ok_or(NotFollowed::Refused(Refused::Network)),
        other => other,
    }
}

/// The trusted network shares of the setting `text` (Settings ▸ Media ▸ Trusted Network Folders):
/// `\\server\share` roots separated by `;` or line breaks; entries that are not a share are
/// ignored.
pub fn trusted_shares(text: &str) -> Vec<String> {
    text.split([';', '\n', '\r']).map(str::trim).filter(|t| craft_docpath::classify(t) == craft_docpath::PathKind::Network).map(str::to_string).collect()
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

    /// A path on another computer is followed only from a document on that share, or when the
    /// share is one of the user's trusted network folders; `..` cannot leave a trusted share.
    #[test]
    fn trusted_shares_are_followed_from_any_document() {
        let doc = std::env::temp_dir().join("films").join("cut.fcproj");
        let nas = r"\\nas\media\shots\clip_01.png";
        assert_eq!(resolve_trusting(nas, Some(&doc), &[]), Err(NotFollowed::Refused(Refused::Network)));
        let trusted = trusted_shares(" \\\\NAS\\media ; not a share;C:\\local\n//other/share/");
        assert_eq!(trusted, vec![r"\\NAS\media".to_string(), "//other/share/".to_string()]);
        // A share path is a file only on Windows; elsewhere it stays refused (not absolute there).
        assert_eq!(resolve_trusting(nas, Some(&doc), &trusted).is_ok_and(|p| p.ends_with("clip_01.png")), cfg!(windows));
        // A `file://host/` URL is never followed (`craft_docpath` refuses it on any share).
        assert_eq!(resolve_trusting("file://nas/media/x.mp4", Some(&doc), &trusted), Err(NotFollowed::Refused(Refused::Network)));
        assert_eq!(resolve_trusting(r"\\other\share\x.mp4", Some(&doc), &trusted).is_ok(), cfg!(windows));
        for refused in [r"\\nas\other\x.mp4", r"\\nas\media\..\other\x.mp4", r"\\third\media\x.mp4"] {
            assert!(resolve_trusting(refused, Some(&doc), &trusted).is_err(), "{refused}");
        }
        // Local paths and other refusals are unaffected.
        assert_eq!(resolve_trusting("clip.mp4", Some(&doc), &trusted), resolve("clip.mp4", Some(&doc)));
        assert_eq!(resolve_trusting("jar:take.mp4", Some(&doc), &trusted), Err(NotFollowed::Refused(Refused::Url)));
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
