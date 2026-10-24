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
//! The other direction is [`store`], used when a document is written: a path is resolved against
//! the document it came from, then stored relative to the new document's folder when that
//! round-trips, so a project and the media beside it survive moving to another folder, drive,
//! share or machine (#3067).

use std::fmt;
use std::path::{Component, Path, PathBuf};

use filmcraft_project::{MediaRef, Project};

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

/// `p` with every media and proxy `File` path in its stored form for a document written to
/// `document` (see [`store`]): paths beside the document are stored relative to its folder, so a
/// project and its media move together between drives, shares and machines. `document` = None (a
/// document without a file) keeps the paths as they are.
pub(crate) fn project_for_document<'p>(
    p: &'p Project,
    document: Option<&str>,
    follow: impl Fn(&str) -> Result<String, NotFollowed>,
) -> std::borrow::Cow<'p, Project> {
    // The web host opens only the files the user gave it, under their own virtual paths: a stored
    // path is the key, so it is stored as given.
    if cfg!(target_arch = "wasm32") {
        return std::borrow::Cow::Borrowed(p);
    }
    let Some(document) = document.filter(|d| !d.is_empty()).map(PathBuf::from) else {
        return std::borrow::Cow::Borrowed(p);
    };
    let document = std::path::absolute(&document).unwrap_or(document);
    std::borrow::Cow::Owned(rewrite_paths(p, |path| store(path, &document, &follow)))
}

/// Give each stored `File` path of `p` its file, resolved against `document` ([`resolve`]): a
/// loaded project holds openable paths, the stored form is only what a document writes. A path
/// that cannot be followed stays as stored — the media pool records it with the document and
/// refuses it, so the item reads as missing rather than opening something else.
pub(crate) fn absolutize(p: &mut Project, document: Option<&str>) {
    // Web stored paths are the keys of the files the user gave the host: they stay as stored.
    if cfg!(target_arch = "wasm32") {
        return;
    }
    let document = document.filter(|d| !d.is_empty()).map(PathBuf::from);
    for it in p.items.values_mut() {
        if let Some(m) = it.as_media_mut() {
            for r in [&mut m.media].into_iter().chain(m.proxy.iter_mut()) {
                if let MediaRef::File { path } = r
                    && let Ok(file) = resolve(path, document.as_deref())
                {
                    *path = file;
                }
            }
        }
    }
}

/// `p` with every media and proxy `File` path resolved by `follow`: the file a stored path points
/// to. Interchange documents name files for other apps to open, so they take the resolved path
/// rather than the stored one; a path `follow` cannot resolve is kept.
pub(crate) fn resolved_project(p: &Project, follow: impl Fn(&str) -> Result<String, NotFollowed>) -> Project {
    rewrite_paths(p, |path| follow(path).unwrap_or_else(|_| path.to_string()))
}

fn rewrite_paths(p: &Project, f: impl Fn(&str) -> String) -> Project {
    let mut out = p.clone();
    for it in out.items.values_mut() {
        if let Some(m) = it.as_media_mut() {
            if let MediaRef::File { path } = &mut m.media {
                *path = f(path);
            }
            if let Some(MediaRef::File { path }) = &mut m.proxy {
                *path = f(path);
            }
        }
    }
    out
}

/// The string to store for `path` in a document at `document` (made absolute by the caller):
/// `follow` resolves the path against the document it was stored in ([`crate::MediaPool::follow`],
/// which knows each stored path's document), then the resolved file is written relative to
/// `document`'s folder when [`resolve`] takes it back to the same file — the form that survives
/// the project and its media moving together. Anything else keeps an absolute path: a file on
/// another drive or share cannot be reached relatively, and a path `follow` cannot resolve is
/// stored as it was.
pub(crate) fn store(path: &str, document: &Path, follow: impl Fn(&str) -> Result<String, NotFollowed>) -> String {
    let Ok(file) = follow(path) else { return path.to_string() };
    if !Path::new(&file).is_absolute() {
        return path.to_string();
    }
    // `follow` returns stored paths already normalised; a path the user gave is normalised here so
    // the round-trip check below compares like with like (`resolve` renders `.`/`..` away).
    let file = resolve(&file, None).unwrap_or(file);
    if let Some(rel) = relative(Path::new(&file), document)
        && resolve(&rel, Some(document)).as_deref() == Ok(file.as_str())
    {
        return rel;
    }
    file
}

/// `target` relative to `document`'s folder, `..` climbing out of it; `None` when they share no
/// root: a file cannot be reached relatively from another drive or share. The result uses `/`,
/// which [`resolve`] reads as a separator on every platform.
fn relative(target: &Path, document: &Path) -> Option<String> {
    let t: Vec<_> = target.components().collect();
    let d: Vec<_> = document.parent()?.components().collect();
    let common = t.iter().zip(d.iter()).take_while(|(a, b)| same_root_comp(a, b)).count();
    if common == 0 {
        return None;
    }
    let mut rel = String::new();
    for _ in common..d.len() {
        rel.push_str("../");
    }
    for (i, c) in t.iter().enumerate().skip(common) {
        if i > common {
            rel.push('/');
        }
        rel.push_str(&c.as_os_str().to_string_lossy());
    }
    (!rel.is_empty()).then_some(rel)
}

/// Component equality for the shared-prefix walk of [`relative`]: the two leading components are
/// the same root — on Windows also when drive letters differ only in case (`C:` and `c:` are the
/// same drive). Compared case-insensitively there only; a name that differs in case still points
/// to a different folder where the filesystem says so.
fn same_root_comp(a: &Component, b: &Component) -> bool {
    use std::path::Prefix;
    match (a, b) {
        (Component::Prefix(x), Component::Prefix(y)) => match (x.kind(), y.kind()) {
            (Prefix::Disk(l), Prefix::Disk(m)) | (Prefix::VerbatimDisk(l), Prefix::VerbatimDisk(m)) => l.eq_ignore_ascii_case(&m),
            _ => x == y,
        },
        _ => a == b,
    }
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

    fn as_given(p: &str) -> Result<String, NotFollowed> {
        Ok(p.to_string())
    }

    #[test]
    fn store_writes_paths_relative_to_the_document() {
        let dir = std::env::temp_dir().join("films");
        let doc = dir.join("cut.fcproj");
        let dir = dir.to_string_lossy().replace('\\', "/");
        // beside, inside and above the document's folder: relative forms resolve back
        assert_eq!(store(&format!("{dir}/a.mp4"), &doc, as_given), "a.mp4");
        assert_eq!(store(&format!("{dir}/media/a.mp4"), &doc, as_given), "media/a.mp4");
        let media = Path::new(&dir).parent().map(|p| p.join("m.mp4").to_string_lossy().replace('\\', "/"));
        if let Some(m) = media {
            assert_eq!(store(&m, &doc, as_given), "../m.mp4");
        }
        // a stored relative path from an older document is rebased onto the new one
        let old = std::env::temp_dir().join("old").join("cut.fcproj");
        let follow = |p: &str| resolve(p, Some(&old));
        let rel = store("media/a.mp4", &doc, follow);
        assert_eq!(resolve(&rel, Some(&doc)), resolve("media/a.mp4", Some(&old)));
        // a relative path with no document behind it is stored as it was
        assert_eq!(store("media/a.mp4", &doc, |_| Err(NotFollowed::NoDocumentFolder)), "media/a.mp4");
        assert_eq!(store(r"\\192.0.2.1\share\x.mp4", &doc, |_| Err(NotFollowed::Refused(Refused::Network))), r"\\192.0.2.1\share\x.mp4");
    }

    #[test]
    fn store_keeps_a_path_that_cannot_go_relative() {
        // another drive has no relative form (Windows; on Unix every path shares the one root)
        if cfg!(windows) {
            assert_eq!(store(r"D:\m\a.mp4", Path::new(r"C:\p\c.fcproj"), as_given), r"D:\m\a.mp4");
        } else {
            assert_eq!(store("/m/a.mp4", Path::new("/p/c.fcproj"), as_given), "../m/a.mp4");
        }
        // a network path stays as given: it is only allowed on the document's own share
        let doc = Path::new(if cfg!(windows) { r"\\srv\share\p\c.fcproj" } else { "/p/c.fcproj" });
        assert_eq!(store(r"\\other\share\a.mp4", doc, as_given), r"\\other\share\a.mp4");
    }

    #[test]
    fn project_for_document_rewrites_media_and_proxy_paths() {
        let dir = std::env::temp_dir().join("films");
        let doc = dir.join("cut.fcproj");
        let mut p = Project::new("t");
        let info = filmcraft_media::MediaInfo {
            name: "a.mp4".into(),
            kind: filmcraft_media::MediaKind::Movie,
            duration: filmcraft_time::Tick(1),
            video: None,
            audio_streams: vec![],
            container: String::new(),
            start_timecode: None,
            file_size: None,
        };
        let id = p.add_item(
            "a",
            filmcraft_project::Label::Blue,
            filmcraft_project::ItemKind::Media(filmcraft_project::MediaClip {
                media: MediaRef::File { path: dir.join("a.mp4").to_string_lossy().into_owned() },
                info,
                interpret: Default::default(),
                mark_in: None,
                mark_out: None,
                markers: vec![],
                offline: false,
                proxy: Some(MediaRef::File { path: dir.join("a.proxy.mp4").to_string_lossy().into_owned() }),
                identity: None,
            }),
            None,
        );
        let stored = project_for_document(&p, Some(doc.to_str().unwrap_or_default()), as_given);
        let m = stored.item(id).and_then(|i| i.as_media());
        assert_eq!(m.map(|m| &m.media), Some(&MediaRef::File { path: "a.mp4".into() }));
        assert_eq!(m.and_then(|m| m.proxy.as_ref()), Some(&MediaRef::File { path: "a.proxy.mp4".into() }));
        // the session's project keeps the paths it had
        let m = p.item(id).and_then(|i| i.as_media());
        assert!(matches!(m.map(|m| &m.media), Some(MediaRef::File { path }) if path.starts_with(dir.to_string_lossy().as_ref())));
        assert!(matches!(project_for_document(&p, None, as_given), std::borrow::Cow::Borrowed(_)));
    }
}
