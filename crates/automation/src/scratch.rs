//! Folders for tests that remove themselves, also when an assertion fails.

use std::ops::Deref;
use std::path::Path;

/// A fresh folder in the temporary directory, removed with everything in it when it is dropped, so
/// a test that fails halfway leaves nothing behind either.
pub(crate) struct Scratch(tempfile::TempDir);

impl Scratch {
    /// A new, empty folder whose name starts `fc-{name}-`.
    pub(crate) fn new(name: &str) -> Self {
        Self(tempfile::Builder::new().prefix(&format!("fc-{name}-")).tempdir().expect("a folder in the temporary directory"))
    }
}

impl Deref for Scratch {
    type Target = Path;

    /// The folder.
    fn deref(&self) -> &Path {
        self.0.path()
    }
}
