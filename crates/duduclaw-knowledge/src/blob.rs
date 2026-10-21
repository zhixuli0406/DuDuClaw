//! Original files, stored by content hash under
//! `<home>/knowledge/blobs/<sha[0..2]>/<sha256>`; uploads stream into
//! `<home>/knowledge/staging/<uuid>.part` first. The display file name is
//! never part of a path.
//!
//! URL sources are the one exception to "named by content": their blob is
//! named by `sha256("url\n" + url)` (the dedupe key) and holds the latest
//! fetched page, replaced atomically on each refetch.

use std::path::{Path, PathBuf};

use crate::{KbError, KbResult};

/// Is `s` a lowercase hex SHA-256?
pub fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Dedupe key and blob name of a URL source.
pub fn url_identity(url: &str) -> String {
    crate::sha256_hex(format!("url\n{url}").as_bytes())
}

#[derive(Debug, Clone)]
pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    /// Store under `<home>/knowledge`.
    pub fn for_home(home: &Path) -> Self {
        Self {
            root: crate::schema::knowledge_dir(home),
        }
    }

    pub fn staging_dir(&self) -> PathBuf {
        self.root.join("staging")
    }

    /// A fresh staging path.
    pub fn new_staging_path(&self) -> KbResult<PathBuf> {
        let dir = self.staging_dir();
        std::fs::create_dir_all(&dir)?;
        Ok(dir.join(format!("{}.part", uuid::Uuid::new_v4().simple())))
    }

    /// Path of the blob `sha`. Refuses anything that is not a hash.
    pub fn path_for(&self, sha: &str) -> KbResult<PathBuf> {
        if !is_sha256_hex(sha) {
            return Err(KbError::Invalid("檔案識別碼格式不正確。".to_string()));
        }
        Ok(self.root.join("blobs").join(&sha[..2]).join(sha))
    }

    /// Move a staged upload into place (dropping it if the blob exists).
    pub fn commit_staged(&self, staged: &Path, sha: &str) -> KbResult<PathBuf> {
        let dest = self.path_for(sha)?;
        if dest.exists() {
            std::fs::remove_file(staged)?;
            return Ok(dest);
        }
        if let Some(dir) = dest.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::rename(staged, &dest)?;
        Ok(dest)
    }

    /// Write `bytes` as the blob of their own hash; returns the hash.
    pub fn put(&self, bytes: &[u8]) -> KbResult<String> {
        let sha = crate::sha256_hex(bytes);
        self.put_named(&sha, bytes)?;
        Ok(sha)
    }

    /// Write (or atomically replace) blob `name` with `bytes`.
    pub fn put_named(&self, name: &str, bytes: &[u8]) -> KbResult<PathBuf> {
        let dest = self.path_for(name)?;
        let tmp = self.new_staging_path()?;
        std::fs::write(&tmp, bytes)?;
        if let Some(dir) = dest.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::rename(&tmp, &dest)?;
        Ok(dest)
    }

    /// Remove the blob `sha` (missing is fine).
    pub fn remove(&self, sha: &str) -> KbResult<()> {
        let path = self.path_for(sha)?;
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Remove staging files older than `max_age`.
    pub fn sweep_staging(&self, max_age: std::time::Duration) -> usize {
        let Ok(rd) = std::fs::read_dir(self.staging_dir()) else {
            return 0;
        };
        let mut n = 0;
        for entry in rd.flatten() {
            let old = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > max_age);
            if old && std::fs::remove_file(entry.path()).is_ok() {
                n += 1;
            }
        }
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_commit_and_refuse_bad_names() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::for_home(dir.path());
        let sha = store.put(b"hello").unwrap();
        assert!(store.path_for(&sha).unwrap().exists());
        let staged = store.new_staging_path().unwrap();
        std::fs::write(&staged, b"hello").unwrap();
        store.commit_staged(&staged, &sha).unwrap();
        assert!(!staged.exists(), "duplicate staging file dropped");
        assert!(store.path_for("../../etc/passwd").is_err());
        assert!(store.path_for(&"A".repeat(64)).is_err());
        store.remove(&sha).unwrap();
        store.remove(&sha).unwrap();
        assert!(!store.path_for(&sha).unwrap().exists());
    }
}
