//! Helpers shared by tests in separate engine modules.

pub(crate) struct TestDir(tempfile::TempDir);

impl TestDir {
    pub(crate) fn new(tag: &str) -> Self {
        Self(tempfile::Builder::new().prefix(&format!("fc-{tag}-")).tempdir().unwrap())
    }
}

impl std::ops::Deref for TestDir {
    type Target = std::path::Path;

    fn deref(&self) -> &Self::Target {
        self.0.path()
    }
}

impl AsRef<std::path::Path> for TestDir {
    fn as_ref(&self) -> &std::path::Path {
        self.0.path()
    }
}
