//! Rebase only file-backed originals and proxies. Keep opaque project fields and foreign
//! absolute paths verbatim. Never stat the media: an offline reference has the same identity.
use super::{ProjectRecords, invalid};
use craft_docpath::{PathKind, Refused};
use std::io;
use std::path::{Component, Path, PathBuf};

fn relative_to(target: &Path, base: &Path) -> Option<PathBuf> {
    let mut t = target.components().peekable();
    let mut b = base.components().peekable();
    while t.peek().is_some() && t.peek() == b.peek() {
        t.next();
        b.next();
    }
    let mut result = PathBuf::new();
    for part in b {
        if !matches!(part, Component::Normal(_)) {
            return None;
        }
        result.push("..");
    }
    result.extend(t);
    Some(result)
}

fn relocated(stored: &str, old: &Path, new: &Path) -> io::Result<Option<String>> {
    let resolve = |s: &str, doc: &Path| craft_docpath::resolve(s, Some(doc));
    let original = resolve(stored, old);
    // An explicit network path already names its target independently of the document.
    if original == resolve(stored, new) || craft_docpath::classify(stored) == PathKind::Network {
        return Ok(None);
    }
    let target = original.map_err(invalid)?;
    let base = new.parent().ok_or_else(|| invalid("missing destination parent"))?;
    for candidate in relative_to(&target, base).into_iter().chain(std::iter::once(target.clone())) {
        let Some(text) = candidate.to_str() else { continue };
        let at_destination = resolve(text, new);
        // The original document resolved this absolute target. A different volume can
        // retain the path as metadata even when media access there is denied.
        if at_destination.as_ref() == Ok(&target) || (candidate == target && at_destination == Err(Refused::Network)) {
            return Ok(Some(text.to_owned()));
        }
    }
    Err(invalid("cannot preserve the stored path at the new location"))
}

pub(super) fn rebase(records: &mut ProjectRecords, from: &Path, to: &Path) -> io::Result<()> {
    let old = std::path::absolute(from)?;
    let new = std::path::absolute(to)?;
    if old == new {
        return Ok(());
    }
    for (id, item) in &mut records.items {
        if let Some(media) = item.pointer_mut("/kind/Media") {
            for key in ["media", "proxy"] {
                if let Some(value) = media.get_mut(key).and_then(|v| v.pointer_mut("/File/path"))
                    && let Some(path) = value.as_str()
                    && let Some(updated) = relocated(path, &old, &new).map_err(|e| invalid(format!("item {id} {key}: {e}")))?
                {
                    *value = updated.into();
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::relocated;
    use craft_docpath::{Refused, resolve};
    use std::path::Path;

    #[test]
    fn unc_links_survive_a_local_archive_roundtrip() {
        let edit = Path::new(r"\\server\share\edit.fcproj");
        let archive = Path::new("C:/archive/folder/project.fcmod");
        let stored = r"media\a.mov";
        let target = resolve(stored, Some(edit)).unwrap();
        let archived = relocated(stored, edit, archive).unwrap().unwrap();
        assert_eq!(Path::new(&archived), target);
        assert_eq!(resolve(&archived, Some(archive)), Err(Refused::Network));
        assert_eq!(relocated(&archived, archive, edit).unwrap(), None);
        assert_eq!(resolve(&archived, Some(edit)).unwrap(), target);
        assert_eq!(relocated("//server/share/media/a.mov", edit, archive).unwrap(), None);
    }
}
