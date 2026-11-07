//! Cache accounting never deletes published files: saved documents and Undo may still use them.
use std::path::Path;

pub(crate) const MAX_RENDER_BYTES: u64 = 1024 * 1024 * 1024;

pub(crate) fn bytes(dir: &Path) -> Result<u64, String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(format!("cannot inspect linked cache: {error}")),
    };
    let mut total = 0_u64;
    for (count, entry) in entries.enumerate() {
        if count >= 100_000 {
            return Err("linked cache has too many files; choose a new cache before rendering".into());
        }
        let entry = entry.map_err(|e| e.to_string())?;
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        if kind.is_symlink() {
            return Err("linked cache contains a symbolic link".into());
        }
        if kind.is_file() {
            total = total.checked_add(entry.metadata().map_err(|e| e.to_string())?.len()).ok_or("linked cache size overflow")?;
        }
    }
    Ok(total)
}

pub(crate) fn allowance(dir: &Path, budget: u64) -> Result<u64, String> {
    let available = budget.saturating_sub(bytes(dir)?).min(MAX_RENDER_BYTES);
    if available == 0 {
        return Err(
            "Linked cache limit reached. Increase the maximum size in Settings > Media Cache; existing renders are kept for saved projects and Undo.".into()
        );
    }
    Ok(available)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accounting_preserves_files_and_applies_the_budget() {
        let dir = crate::media_test_util::tmp_dir("linked-cache-quota");
        let old = dir.join("saved-project.mov");
        std::fs::write(&old, b"keep this render").unwrap();
        let used = bytes(&dir).unwrap();
        assert_eq!(allowance(&dir, used + 10).unwrap(), 10);
        assert!(allowance(&dir, used).is_err());
        assert!(allowance(&dir, 0).is_err());
        assert_eq!(std::fs::read(&old).unwrap(), b"keep this render");
        assert_eq!(bytes(&dir).unwrap(), used);
    }
}
