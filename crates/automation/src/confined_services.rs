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

    fn export_in_memory(&self) -> bool {
        self.inner.export_in_memory()
    }

    /// Allowed inside the read root; a refusal is an error, not `None`.
    fn list_entries(&self, dir: &str) -> Option<Result<Vec<DirEntry>>> {
        if let Err(e) = self.check(dir, Access::Read) {
            return Some(Err(e));
        }
        self.inner.list_entries(dir)
    }

    /// Drives and the home folder name places outside the roots.
    fn volumes(&self) -> Vec<Volume> {
        Vec::new()
    }

    fn home_dir(&self) -> Option<String> {
        None
    }
}
