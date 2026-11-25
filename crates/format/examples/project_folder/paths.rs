//! Rebase only file-backed originals and proxies. Keep opaque project fields and foreign
//! absolute paths verbatim. Never stat the media: an offline reference has the same identity.
use super::{ProjectRecords, invalid};
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
    if original == resolve(stored, new) {
        return Ok(None);
    }
    let target = original.map_err(invalid)?;
    let base = new.parent().ok_or_else(|| invalid("missing destination parent"))?;
    for candidate in relative_to(&target, base).into_iter().chain(std::iter::once(target.clone())) {
        if let Some(text) = candidate.to_str()
            && resolve(text, new).as_ref() == Ok(&target)
        {
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
