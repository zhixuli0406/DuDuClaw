//! Test databases must live in a private home, even when TMPDIR is shared.
use std::path::{Path, PathBuf};
use crate::UserDb;

pub(crate) struct TestDbPath {
    path: PathBuf,
    _home: tempfile::TempDir,
}

impl TestDbPath {
    pub(crate) fn new() -> Self {
        let mut builder = tempfile::Builder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(std::fs::Permissions::from_mode(0o700));
        }
        let home = builder.tempdir().unwrap();
        Self { path: home.path().join("users.db"), _home: home }
    }

    pub(crate) fn path(&self) -> &Path { &self.path }
}

pub(crate) fn test_db() -> (UserDb, TestDbPath) {
    let path = TestDbPath::new();
    (UserDb::new(path.path()).unwrap(), path)
}
