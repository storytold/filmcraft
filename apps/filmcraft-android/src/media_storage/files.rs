//! Only files that have never been handed to a project may be reclaimed.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError, TryLockError};

const STAGING_DIR: &str = ".FilmCraft-picker-staging";
const MARKER: &str = ".unfinished-imports-v1";
static TRANSFERS: Mutex<()> = Mutex::new(());
static NEXT_FOLDER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct FileUsage {
    pub files: u64,
    pub bytes: u64,
}

impl FileUsage {
    fn add(&mut self, len: u64) {
        self.files = self.files.saturating_add(1);
        self.bytes = self.bytes.saturating_add(len);
    }
}

#[derive(Debug, Default)]
pub(super) struct Report {
    pub completed: FileUsage,
    pub unfinished: FileUsage,
    pub removed: Option<FileUsage>,
}

fn busy() -> io::Error {
    io::Error::new(io::ErrorKind::WouldBlock, "An import is in progress. Refresh once it finishes.")
}

fn no_active_transfer() -> io::Result<MutexGuard<'static, ()>> {
    match TRANSFERS.try_lock() {
        Ok(guard) => Ok(guard),
        Err(TryLockError::Poisoned(error)) => Ok(error.into_inner()),
        Err(TryLockError::WouldBlock) => Err(busy()),
    }
}

pub(super) fn report(media: &Path, reclaim: bool) -> io::Result<Report> {
    let _guard = no_active_transfer()?;
    let completed = visit_batches(media, false)?;
    let stage = media.join(STAGING_DIR);
    let (unfinished, removed) = if staging_owned(&stage)? {
        let removed = if reclaim { Some(visit_batches(&stage, true)?) } else { None };
        (visit_batches(&stage, false)?, removed)
    } else {
        (FileUsage::default(), None)
    };
    Ok(Report { completed, unfinished, removed })
}

fn visit_batches(root: &Path, remove: bool) -> io::Result<FileUsage> {
    let mut usage = FileUsage::default();
    if !directory_present(root)? {
        return Ok(usage);
    }
    for batch in fs::read_dir(root)? {
        let batch = batch?;
        if !batch.file_type()?.is_dir() || !batch.file_name().to_str().is_some_and(import_folder_name) {
            continue;
        }
        for entry in fs::read_dir(batch.path())? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let len = entry.metadata()?.len();
            if remove {
                fs::remove_file(entry.path())?;
            }
            usage.add(len);
        }
        if remove {
            // The staging root is private to picker transfers. Never recurse into an unexpected
            // child, and never follow a link. Only empty transfer folders can be removed.
            let _ = fs::remove_dir(batch.path());
        }
    }
    Ok(usage)
}

fn import_folder_name(name: &str) -> bool {
    name.strip_prefix("Import-")
        .and_then(|suffix| suffix.split_once('-'))
        .is_some_and(|(pid, serial)| pid.parse::<u32>().is_ok() && serial.parse::<u64>().is_ok())
}

fn directory_present(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_dir() => Ok(true),
        Ok(_) => Err(io::Error::new(io::ErrorKind::InvalidData, "FilmCraft expected a regular media directory.")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn staging_owned(stage: &Path) -> io::Result<bool> {
    if !directory_present(stage)? {
        return Ok(false);
    }
    match fs::symlink_metadata(stage.join(MARKER)) {
        Ok(meta) => Ok(meta.file_type().is_file() && meta.len() == 0),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn prepare_stage(media: &Path) -> io::Result<PathBuf> {
    if !directory_present(media)? {
        fs::create_dir_all(media)?;
    }
    let stage = media.join(STAGING_DIR);
    match fs::create_dir(&stage) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists && directory_present(&stage)? => {}
        Err(error) => return Err(error),
    }
    if !staging_owned(&stage)? {
        // An empty folder can be left behind if the previous run stopped just before writing
        // the marker. A populated folder with no marker does not belong to our transfers.
        if fs::read_dir(&stage)?.next().is_some() {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, "The picker staging folder contains unrecognized files."));
        }
        let mut marker = OpenOptions::new().create_new(true).write(true).open(stage.join(MARKER))?;
        marker.flush()?;
    }
    Ok(stage)
}

pub(in crate::media_storage) fn copy_and_publish(media: &Path, copy_into: impl FnOnce(&Path) -> Result<Vec<PathBuf>, String>) -> Result<Vec<PathBuf>, String> {
    // Serialize picker writes and storage maintenance across app windows, even after the
    // initiating window has closed while its worker still holds a provider stream.
    let _guard = TRANSFERS.lock().unwrap_or_else(PoisonError::into_inner);
    let stage = prepare_stage(media).map_err(|error| format!("Could not prepare import storage. {error}"))?;
    let staged = copy_into(&stage)?;
    if staged.is_empty() {
        return Ok(staged);
    }
    publish(media, &stage, &staged).map_err(|error| format!("Could not finish importing the selected files. {error}"))
}

fn publish(media: &Path, stage: &Path, staged: &[PathBuf]) -> io::Result<Vec<PathBuf>> {
    let folder = staged
        .first()
        .and_then(|first| first.parent())
        .filter(|folder| {
            folder.parent() == Some(stage)
                && folder.file_name().and_then(|name| name.to_str()).is_some_and(import_folder_name)
                && staged.iter().all(|path| path.parent() == Some(*folder) && path.file_name().is_some())
        })
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "The picker returned files outside the active transfer."))?;
    if !directory_present(folder)? {
        return Err(io::Error::new(io::ErrorKind::NotFound, "The picker transfer folder disappeared."));
    }
    let finished = reserve_published_folder(media)?;
    match fs::rename(folder, &finished) {
        Ok(()) => Ok(staged.iter().filter_map(|path| path.file_name().map(|name| finished.join(name))).collect()),
        Err(error) => {
            let _ = fs::remove_dir(finished);
            Err(error)
        }
    }
}

fn reserve_published_folder(media: &Path) -> io::Result<PathBuf> {
    for _ in 0..128 {
        let serial = NEXT_FOLDER.fetch_add(1, Ordering::Relaxed);
        let folder = media.join(format!("Import-{}-{serial}", std::process::id()));
        match fs::create_dir(&folder) {
            Ok(()) => return Ok(folder),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::other("No unused import folder was found."))
}

#[cfg(test)]
mod tests {
    use super::*;

    static TESTS: Mutex<()> = Mutex::new(());
    static TEST_NUMBER: AtomicU64 = AtomicU64::new(1);

    struct TestMedia(PathBuf);

    impl TestMedia {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("filmcraft-media-storage-test-{}-{}", std::process::id(), TEST_NUMBER.fetch_add(1, Ordering::Relaxed),));
            fs::create_dir(&root).unwrap();
            Self(root)
        }

        fn put(&self, path: &str, bytes: &[u8]) -> PathBuf {
            let path = self.0.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, bytes).unwrap();
            path
        }
    }

    impl Drop for TestMedia {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn reclaim_only_removes_unfinished_picker_data() {
        let _guard = TESTS.lock().unwrap();
        let media = TestMedia::new();
        let published = media.put("Import-1-1/current.mp4", b"current and undo");
        let older_project = media.put("Import-1-2/older.mp4", b"saved anywhere");
        let unrelated = media.put("Artwork/logo.png", b"unmanaged");
        prepare_stage(&media.0).unwrap();
        let trailing = media.put(&format!("{STAGING_DIR}/Import-1-3/partial.mp4"), b"partial");
        let unknown_batch = media.put(&format!("{STAGING_DIR}/archive/notes.txt"), b"keep");

        let before = report(&media.0, false).unwrap();
        assert_eq!(before.completed, FileUsage { files: 2, bytes: 30 });
        assert_eq!(before.unfinished, FileUsage { files: 1, bytes: 7 });
        assert!(trailing.is_file());

        let after = report(&media.0, true).unwrap();
        assert_eq!(after.removed, Some(before.unfinished));
        assert_eq!(after.completed, before.completed);
        assert_eq!(after.unfinished, FileUsage::default());
        assert!(!trailing.exists());
        for file in [published, older_project, unrelated, unknown_batch] {
            assert!(file.is_file());
        }
    }

    #[test]
    fn copy_is_published_as_a_batch_and_blocks_concurrent_reclaim() {
        let _guard = TESTS.lock().unwrap();
        let media = TestMedia::new();
        let copied = copy_and_publish(&media.0, |stage| {
            let unfinished = media.put(&format!("{STAGING_DIR}/Import-1-1/frame.mov"), b"complete");
            assert_eq!(unfinished.parent().and_then(Path::parent), Some(stage));
            assert_eq!(report(&media.0, true).unwrap_err().kind(), io::ErrorKind::WouldBlock);
            assert!(unfinished.is_file());
            Ok(vec![unfinished])
        })
        .unwrap();
        assert_eq!(copied[0].parent().and_then(Path::parent), Some(media.0.as_path()));
        assert_eq!(fs::read(&copied[0]).unwrap(), b"complete");
        let after = report(&media.0, true).unwrap();
        assert_eq!(after.completed, FileUsage { files: 1, bytes: 8 });
        assert_eq!(after.unfinished, FileUsage::default());
        assert!(copied[0].is_file());
    }

    #[test]
    fn unknown_staging_folders_are_not_claimed_or_removed() {
        let _guard = TESTS.lock().unwrap();
        let media = TestMedia::new();
        let unknown = media.put(&format!("{STAGING_DIR}/Import-1-1/unsorted.mkv"), b"user data");
        assert_eq!(report(&media.0, true).unwrap().removed, None);
        assert!(copy_and_publish(&media.0, |_| Ok(Vec::new())).is_err());
        assert!(unknown.is_file());
    }

    #[cfg(unix)]
    #[test]
    fn reclaim_does_not_follow_links_or_descend_into_unknown_subfolders() {
        let _guard = TESTS.lock().unwrap();
        let media = TestMedia::new();
        let target = media.put("Import-1-1/keep.mp4", b"protected");
        prepare_stage(&media.0).unwrap();
        let trailing = media.put(&format!("{STAGING_DIR}/Import-1-2/partial.mov"), b"partial");
        let nested = media.put(&format!("{STAGING_DIR}/Import-1-2/unknown/keep"), b"protected");
        let linked = trailing.parent().unwrap().join("link.mp4");
        std::os::unix::fs::symlink(&target, &linked).unwrap();
        std::os::unix::fs::symlink(target.parent().unwrap(), media.0.join(STAGING_DIR).join("Import-1-3")).unwrap();

        let after = report(&media.0, true).unwrap();
        assert_eq!(after.removed, Some(FileUsage { files: 1, bytes: 7 }));
        assert!(!trailing.exists());
        assert_eq!(fs::read(&linked).unwrap(), b"protected");
        assert_eq!(fs::read(&nested).unwrap(), b"protected");
    }
}
