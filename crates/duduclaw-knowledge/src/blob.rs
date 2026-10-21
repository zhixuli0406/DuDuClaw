//! Original files, stored by content hash under
//! `<home>/knowledge/blobs/<sha[0..2]>/<sha256>`; uploads stream into
//! `<home>/knowledge/staging/<uuid>.part` first. The display file name is
//! never part of a path.
//!
//! URL sources are the one exception to "named by content": their blob is
//! named by `sha256("url\n" + url)` (the dedupe key) and holds the latest
//! fetched page, replaced atomically on each refetch.
//!
//! ## Removal
//!
//! A blob file is written before the source row that references it, so
//! "no row references it" is not enough to delete it: an upload in flight
//! would lose its file. A file is removed only when, inside an IMMEDIATE
//! transaction (no new reference can be inserted meanwhile), no source
//! references it **and** it has not been written or reused for
//! [`GC_GRACE`]. Every write and every reuse refreshes the file's mtime.
//! Deleting a source removes its file at once when it is old enough; younger
//! files are left to [`BlobStore::gc`], which the gateway runs periodically.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use rusqlite::Connection;

use crate::{KbError, KbResult};

/// How long an unreferenced blob is kept after its last write or reuse.
pub const GC_GRACE: Duration = Duration::from_secs(600);

fn touch(path: &Path) -> KbResult<()> {
    let f = std::fs::OpenOptions::new().write(true).open(path)?;
    f.set_modified(SystemTime::now())?;
    Ok(())
}

/// Is `s` a lowercase hex SHA-256?
pub fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
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
            // Reused: restart the grace period so a concurrent sweep keeps it.
            touch(&dest)?;
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

    /// Remove blob `sha` if no source references it and it is older than
    /// `grace`. Returns whether the file was removed.
    pub fn remove_if_unreferenced(
        &self,
        conn: &mut Connection,
        sha: &str,
        grace: Duration,
    ) -> KbResult<bool> {
        let path = self.path_for(sha)?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let refs: i64 = tx.query_row(
            "SELECT COUNT(*) FROM sources WHERE blob_sha256 = ?1",
            [sha],
            |r| r.get(0),
        )?;
        let old_enough = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age >= grace);
        let removed = refs == 0 && old_enough && {
            self.remove(sha)?;
            true
        };
        tx.commit()?;
        Ok(removed)
    }

    /// Remove every unreferenced blob older than `grace`.
    pub fn gc(&self, conn: &mut Connection, grace: Duration) -> KbResult<usize> {
        let Ok(dirs) = std::fs::read_dir(self.root.join("blobs")) else {
            return Ok(0);
        };
        let mut removed = 0;
        for dir in dirs.flatten() {
            let Ok(files) = std::fs::read_dir(dir.path()) else {
                continue;
            };
            for f in files.flatten() {
                let name = f.file_name().to_string_lossy().into_owned();
                if is_sha256_hex(&name) && self.remove_if_unreferenced(conn, &name, grace)? {
                    removed += 1;
                }
            }
        }
        Ok(removed)
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

    fn age(path: &Path, secs: u64) {
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(secs))
            .unwrap();
    }

    #[test]
    fn removal_needs_no_reference_and_the_grace_period() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::for_home(dir.path());
        let mut conn = crate::schema::open_in_memory().unwrap();
        let cfg = crate::limits::KnowledgeConfig::default();
        let c = crate::store::create_collection(
            &mut conn,
            &crate::store::NewCollection {
                name: "A".into(),
                ..Default::default()
            },
            "u",
            &cfg,
        )
        .unwrap();
        let referenced = store.put(b"kept").unwrap();
        let fresh = store.put(b"fresh").unwrap();
        let old = store.put(b"old").unwrap();
        crate::sources::add_source(
            &mut conn,
            &crate::sources::test_support::new_source(&c.id, &referenced),
            &cfg,
        )
        .unwrap();
        for sha in [&referenced, &old] {
            age(&store.path_for(sha).unwrap(), 3600);
        }
        assert!(
            !store
                .remove_if_unreferenced(&mut conn, &referenced, GC_GRACE)
                .unwrap(),
            "referenced"
        );
        assert!(
            !store
                .remove_if_unreferenced(&mut conn, &fresh, GC_GRACE)
                .unwrap(),
            "inside grace"
        );
        assert_eq!(store.gc(&mut conn, GC_GRACE).unwrap(), 1);
        assert!(!store.path_for(&old).unwrap().exists());
        assert!(store.path_for(&fresh).unwrap().exists());
        // Reuse of an old file restarts its grace period.
        age(&store.path_for(&fresh).unwrap(), 3600);
        let staged = store.new_staging_path().unwrap();
        std::fs::write(&staged, b"fresh").unwrap();
        store.commit_staged(&staged, &fresh).unwrap();
        assert!(
            !store
                .remove_if_unreferenced(&mut conn, &fresh, GC_GRACE)
                .unwrap()
        );
    }
}
