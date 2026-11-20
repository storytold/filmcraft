//! Inspect and restore FilmCraft's portable project layout from a ZIP archive.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use zip::{CompressionMethod, ZipArchive};

const MAX_ENTRIES: usize = 250_000;

pub(super) struct Archive {
    zip: ZipArchive<BufReader<File>>,
    entries: Vec<Entry>,
    pub bytes: u64,
    pub project: PathBuf,
}

struct Entry {
    index: usize,
    path: PathBuf,
    size: u64,
    directory: bool,
}

impl Archive {
    pub fn open(path: &Path) -> Result<Self, String> {
        let input = File::open(path).map_err(|error| error.to_string())?;
        Self::inspect(ZipArchive::new(BufReader::new(input)).map_err(|error| format!("Could not read this ZIP. {error}"))?)
    }

    fn inspect(mut zip: ZipArchive<BufReader<File>>) -> Result<Self, String> {
        if zip.len() > MAX_ENTRIES {
            return Err("This backup contains too many entries.".into());
        }
        let mut entries = Vec::with_capacity(zip.len());
        let mut paths = HashSet::new();
        let mut project = None;
        let mut bytes = 0u64;
        for index in 0..zip.len() {
            let entry = zip.by_index_raw(index).map_err(|error| error.to_string())?;
            let name = entry.name().trim_end_matches('/');
            if entry.is_symlink()
                || entry.encrypted()
                || !matches!(entry.compression(), CompressionMethod::Stored | CompressionMethod::Deflated)
                || name.contains(['\\', ':', '\0'])
                || name.split('/').any(|part| part.is_empty() || part == "." || part == "..")
            {
                return Err("The backup contains an unsupported or unsafe archive entry.".into());
            }
            let path = entry.enclosed_name().ok_or("The backup contains an unsafe path.")?;
            let levels = path.components().count();
            if levels > 3 || !paths.insert(path.clone()) {
                return Err("The backup contains an unexpected or duplicate path.".into());
            }
            let directory = entry.is_dir();
            if !directory && levels == 1 && path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("fcproj")) {
                if project.replace(path.clone()).is_some() {
                    return Err("The backup contains multiple FilmCraft project files.".into());
                }
            }
            let size = entry.size();
            bytes = bytes.checked_add(size).ok_or("This backup is too large.")?;
            entries.push(Entry { index, path, size, directory });
        }
        let project = project.ok_or("The ZIP has no FilmCraft project at the top level.")?;
        Ok(Self { zip, entries, bytes, project })
    }

    pub fn extract(mut self, root: &Path, cancel: &AtomicBool, written: &AtomicU64) -> Result<PathBuf, String> {
        fs::create_dir(root).map_err(|error| error.to_string())?;
        let mut buffer = vec![0; 256 * 1024];
        for entry in self.entries {
            if cancel.load(Ordering::Relaxed) {
                return Err("Restore cancelled.".into());
            }
            let target = root.join(&entry.path);
            if entry.directory {
                fs::create_dir_all(&target).map_err(|error| error.to_string())?;
                continue;
            }
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            let mut source = self.zip.by_index(entry.index).map_err(|error| error.to_string())?;
            let mut output = OpenOptions::new().write(true).create_new(true).open(&target).map_err(|error| error.to_string())?;
            let mut left = entry.size;
            loop {
                if cancel.load(Ordering::Relaxed) {
                    return Err("Restore cancelled.".into());
                }
                let len = source.read(&mut buffer).map_err(|error| error.to_string())?;
                if len == 0 {
                    break;
                }
                left = left.checked_sub(len as u64).ok_or("An archive entry exceeds its declared size.")?;
                output.write_all(&buffer[..len]).map_err(|error| error.to_string())?;
                written.fetch_add(len as u64, Ordering::Relaxed);
            }
            if left != 0 {
                return Err("An archive entry is incomplete.".into());
            }
            output.sync_all().map_err(|error| error.to_string())?;
        }
        Ok(root.join(self.project))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use zip::write::SimpleFileOptions;

    fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in entries {
            zip.start_file(*name, SimpleFileOptions::default().compression_method(CompressionMethod::Deflated)).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn restores_a_project_and_nested_media() {
        let root = std::env::temp_dir().join(format!("filmcraft-restore-test-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let path = root.join("backup.zip");
        fs::write(&path, zip_bytes(&[("Edit.fcproj", b"test project"), ("Image sequences/9/shot01.png", b"test media")])).unwrap();
        let archive = Archive::open(&path).unwrap();
        assert_eq!(archive.bytes, 22);
        let restored = root.join("Restored");
        let project = archive.extract(&restored, &AtomicBool::default(), &AtomicU64::default()).unwrap();
        assert_eq!(project, restored.join("Edit.fcproj"));
        assert_eq!(fs::read(restored.join("Image sequences/9/shot01.png")).unwrap(), b"test media");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_paths_outside_the_backup_and_multiple_projects() {
        let root = std::env::temp_dir().join(format!("filmcraft-restore-invalid-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let path = root.join("backup.zip");
        for entry in ["../missing.mp4", "/missing.mp4", "C:\\missing.mp4", "Second.fcproj"] {
            fs::write(&path, zip_bytes(&[("Edit.fcproj", b"a"), (entry, b"b")])).unwrap();
            assert!(Archive::open(&path).is_err(), "{entry}");
        }
        fs::remove_dir_all(root).unwrap();
    }
}
