//! The file services of a headless session while `--automation-read-root/--automation-write-root`
//! are in force (#985). [`crate::roots`] checks the paths a command is given; this checks every
//! path the session then opens through its services, whoever chose it. That covers what no command
//! parameter names: the media a project file points at (which `render_frame` would hand back) and
//! the folder a command picks for itself.
//!
//! A refused read looks like an unreadable file to the engine, so media outside the read root shows
//! as offline. The bridge can't be covered this way: the session lives in the desktop app.

use std::io::{Error, ErrorKind, Result};
use std::sync::Arc;

use filmcraft_engine::Services;
use filmcraft_engine::media_browser::{DirEntry, Volume};

use crate::roots::{Access, AutomationRoots};

pub struct ConfinedServices {
    inner: Arc<dyn Services>,
    roots: Arc<AutomationRoots>,
}

impl ConfinedServices {
    /// Check what `inner` is asked to open against `roots`.
    pub fn new(inner: Arc<dyn Services>, roots: Arc<AutomationRoots>) -> Self {
        Self { inner, roots }
    }

    /// [`AutomationRoots::check`] as an I/O error, which the engine reports like an unreadable file.
    fn check(&self, path: &str, access: Access) -> Result<()> {
        self.roots.check(path, access).map_err(|e| Error::new(ErrorKind::PermissionDenied, e))
    }
}

/// `entry` as the desktop host lists it: links followed, so that an alias of a folder shows as a folder.
fn listed(entry: &std::fs::DirEntry) -> DirEntry {
    let md = std::fs::metadata(entry.path()).ok();
    let modified = md.as_ref().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
    DirEntry {
        name: entry.file_name().to_string_lossy().to_string(),
        is_dir: md.as_ref().is_some_and(|m| m.is_dir()),
        size: md.as_ref().filter(|m| m.is_file()).map(|m| m.len()),
        modified: modified.map(|d| d.as_secs()),
    }
}

impl Services for ConfinedServices {
    /// Allowed inside the read root.
    fn read_file(&self, path: &str) -> Result<Vec<u8>> {
        self.check(path, Access::Read)?;
        self.inner.read_file(path)
    }

    /// Allowed inside the write root.
    fn write_file(&self, path: &str, data: &[u8]) -> Result<()> {
        self.check(path, Access::Write)?;
        self.inner.write_file(path, data)
    }

    /// Allowed inside the write root.
    fn write_file_new(&self, path: &str, data: &[u8]) -> Result<()> {
        self.check(path, Access::Write)?;
        self.inner.write_file_new(path, data)
    }

    /// Allowed inside either root: commands pick a free file name in the write root this way, and
    /// a refusal would read as "free" and overwrite the file that is there.
    fn file_size(&self, path: &str) -> Result<u64> {
        self.check(path, Access::Read).or_else(|_| self.check(path, Access::Write))?;
        self.inner.file_size(path)
    }

    /// Allowed inside the read root.
    fn read_range(&self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
        self.check(path, Access::Read)?;
        self.inner.read_range(path, offset, len)
    }

    /// Allowed inside the read root; a refusal is an error, not `None`, which would mean the host has no reader.
    fn reader(&self, path: &str) -> Option<Result<filmcraft_media::SharedReader>> {
        if let Err(e) = self.check(path, Access::Read) {
            return Some(Err(e));
        }
        self.inner.reader(path)
    }

    /// Allowed inside the read root; a refusal is an error, not `None`, which would mean the host can't list.
    fn list_dir(&self, dir: &str) -> Option<Result<Vec<String>>> {
        if let Err(e) = self.check(dir, Access::Read) {
            return Some(Err(e));
        }
        self.inner.list_dir(dir)
    }

    /// The host's loader, checking each frame against the read root when it is loaded.
    fn file_loader(&self) -> Option<filmcraft_media::sequence::FrameLoader> {
        let inner = self.inner.file_loader()?;
        let roots = self.roots.clone();
        Some(Arc::new(move |path: &str| {
            roots.check(path, Access::Read).map_err(|e| Error::new(ErrorKind::PermissionDenied, e))?;
            inner(path)
        }))
    }

    /// Whether exports stay in memory and are written through [`Services::write_file`], which this wrapper checks.
    fn export_in_memory(&self) -> bool {
        self.inner.export_in_memory()
    }

    /// A derived output (an AAF `<stem> Media` mixdown, an OMF audio folder) must sit inside the write root.
    fn confine_write(&self, path: &str) -> Result<()> {
        self.check(path, Access::Write)
    }

    /// Allowed inside the read root; a refusal is an error, not `None`. The host's own listing follows every link to
    /// say whether it is a folder, which would look at a target outside the root, and on Windows contact its host,
    /// before any check. So a link is put to the roots first and left out when they refuse it, and the rest is
    /// listed as the host lists it.
    fn list_entries(&self, dir: &str) -> Option<Result<Vec<DirEntry>>> {
        if let Err(e) = self.check(dir, Access::Read) {
            return Some(Err(e));
        }
        let read = match std::fs::read_dir(if dir.is_empty() { "." } else { dir }) {
            Ok(read) => read,
            Err(e) => return Some(Err(e)),
        };
        let allowed = |entry: &std::fs::DirEntry| {
            let is_link = entry.file_type().is_ok_and(|kind| kind.is_symlink());
            !is_link || entry.path().to_str().is_some_and(|path| self.roots.check(path, Access::Read).is_ok())
        };
        Some(Ok(read.flatten().filter(allowed).map(|entry| listed(&entry)).collect()))
    }

    /// Drives and the home folder name places outside the roots.
    fn volumes(&self) -> Vec<Volume> {
        Vec::new()
    }

    /// No home directory: reporting one would name a folder outside the roots.
    fn home_dir(&self) -> Option<String> {
        None
    }
}
