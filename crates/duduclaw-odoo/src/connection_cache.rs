//! Short-lived connection metadata; never stores credentials or authorization decisions.
//!
//! Storage is `<home>/odoo/cache/connections/<identity-hash>.json`. On Unix the
//! directory chain is walked from the DuDuClaw home with `O_DIRECTORY |
//! O_NOFOLLOW` (a symlinked home or component is refused), every file is
//! opened relative to the pinned directory descriptor with `O_NOFOLLOW`, the
//! opened handle is `fstat`-checked, and writes go through a temp file in the
//! same directory plus `renameat`. Same pattern as duduclaw-core's
//! descriptor-pinned state stores. Any failure just disables the cache.
//!
//! Off Unix (Windows) there is no portable descriptor-relative API, and a
//! path-based check-then-use would be racy, so the cache is disabled there:
//! `load` always misses and `save` never writes. Every connection then simply
//! authenticates afresh, which is the safe default.
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use crate::edition::EditionGate;

const TTL_SECS: u64 = 6 * 60 * 60;
const MAX_ENTRY_BYTES: u64 = 64 * 1024;
#[cfg_attr(not(unix), allow(dead_code))]
const DIRS: [&str; 3] = ["odoo", "cache", "connections"];

#[derive(Serialize, Deserialize)]
pub(crate) struct CachedConnection {
    pub identity_hash: String,
    pub uid: i64,
    pub edition: EditionGate,
    pub server_version: String,
    pub created_at: u64,
}

pub(crate) fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

pub(crate) fn identity_hash(identity: &str) -> String {
    format!("{:x}", Sha256::digest(identity.as_bytes()))
}

/// Keys are SHA-256 hex digests; anything else never reaches the filesystem.
fn valid_key(key: &str) -> bool {
    key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit())
}

pub(crate) fn load(home: &Path, key: &str, time: u64) -> Option<CachedConnection> {
    if !valid_key(key) { return None; }
    let bytes = store::read(home, &format!("{key}.json")).ok()?;
    let entry: CachedConnection = serde_json::from_slice(&bytes).ok()?;
    if entry.identity_hash != key || entry.uid <= 0
        || time.checked_sub(entry.created_at)? >= TTL_SECS
        || entry.server_version.is_empty()
        || crate::schema::safe_version(&entry.server_version) != entry.server_version
        || entry.edition.odoo_version != entry.server_version
        || entry.edition.installed_modules.iter().any(|m| !crate::edition::is_ee_module(m)) {
        return None;
    }
    Some(entry)
}

pub(crate) fn save(home: &Path, entry: &CachedConnection) -> std::io::Result<()> {
    if !valid_key(&entry.identity_hash) {
        return Err(std::io::Error::other("invalid Odoo connection cache key"));
    }
    let bytes = serde_json::to_vec(entry).map_err(std::io::Error::other)?;
    if bytes.len() as u64 > MAX_ENTRY_BYTES {
        return Err(std::io::Error::other("Odoo connection cache entry too large"));
    }
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let temp = format!(".{}.{}.{}.tmp", entry.identity_hash, std::process::id(),
        SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    store::write_atomic(home, &format!("{}.json", entry.identity_hash), &temp, &bytes)
}

/// Descriptor-pinned directory helpers, also reused by the schema store's
/// stale-profile cleanup.
#[cfg(unix)]
pub(crate) mod store {
    use std::ffi::CString;
    use std::fs::{File, OpenOptions, Permissions};
    use std::io::{self, Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    use std::path::Path;
    use super::{DIRS, MAX_ENTRY_BYTES};

    fn refused(what: &str) -> io::Error {
        io::Error::new(io::ErrorKind::PermissionDenied, format!("unsafe Odoo connection cache {what}"))
    }

    fn euid() -> u32 {
        // SAFETY: geteuid has no preconditions and cannot fail.
        unsafe { libc::geteuid() }
    }

    pub(crate) fn basename(name: &str) -> io::Result<CString> {
        if name.is_empty() || name == "." || name == ".." || name.contains('/') {
            return Err(refused("name"));
        }
        CString::new(name).map_err(|_| refused("name"))
    }

    /// Open `name` relative to the pinned `dir`, never following a symlink.
    /// O_NONBLOCK avoids hanging on a planted FIFO before fstat rejects it.
    pub(crate) fn open_at(dir: &File, name: &str, flags: i32, mode: libc::mode_t) -> io::Result<File> {
        let name = basename(name)?;
        // SAFETY: `dir` is an open descriptor and `name` is NUL-terminated.
        let fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK, mode as libc::c_uint) };
        if fd < 0 { return Err(io::Error::last_os_error()); }
        // SAFETY: openat returned a fresh descriptor owned only by this File.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    /// Walk home → odoo → cache → connections without following symlinks.
    fn cache_dir(home: &Path, create: bool) -> io::Result<File> {
        open_dir_chain(home, &DIRS, create)
    }

    /// Walk `home` → `dirs[0]` → … without following symlinks at any step
    /// (the home itself included). Intermediate directories must be ours and
    /// not group/other writable; the leaf must be private (created 0700).
    pub(crate) fn open_dir_chain(home: &Path, dirs: &[&str], create: bool) -> io::Result<File> {
        let mut dir = OpenOptions::new().read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(home)?;
        for (index, component) in dirs.iter().enumerate() {
            let leaf = index + 1 == dirs.len();
            if create {
                let name = basename(component)?;
                // SAFETY: `dir` is an open directory descriptor; `name` is NUL-terminated.
                if unsafe { libc::mkdirat(dir.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() != io::ErrorKind::AlreadyExists { return Err(error); }
                }
            }
            dir = open_at(&dir, component, libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
            let meta = dir.metadata()?;
            if !meta.is_dir() || meta.uid() != euid() { return Err(refused("directory")); }
            if leaf && create && meta.mode() & 0o077 != 0 {
                dir.set_permissions(Permissions::from_mode(0o700))?;
            }
            let mode = dir.metadata()?.mode();
            if mode & 0o022 != 0 || (leaf && mode & 0o077 != 0) { return Err(refused("directory mode")); }
        }
        Ok(dir)
    }

    /// fstat the opened handle: a single-link private regular file of ours.
    fn validate_file(file: &File) -> io::Result<()> {
        let meta = file.metadata()?;
        if !meta.is_file() || meta.uid() != euid() || meta.nlink() != 1
            || meta.mode() & 0o777 != 0o600 || meta.len() > MAX_ENTRY_BYTES {
            return Err(refused("file"));
        }
        Ok(())
    }

    pub(super) fn read(home: &Path, name: &str) -> io::Result<Vec<u8>> {
        let dir = cache_dir(home, false)?;
        let file = open_at(&dir, name, libc::O_RDONLY, 0)?;
        validate_file(&file)?;
        let mut bytes = Vec::new();
        file.take(MAX_ENTRY_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_ENTRY_BYTES { return Err(refused("file size")); }
        Ok(bytes)
    }

    pub(super) fn write_atomic(home: &Path, name: &str, temp: &str, bytes: &[u8]) -> io::Result<()> {
        let dir = cache_dir(home, true)?;
        // Refuse a planted symlink, FIFO, directory or hard link at the target.
        match open_at(&dir, name, libc::O_RDONLY, 0) {
            Ok(existing) => validate_file(&existing)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Err(refused("file")),
        }
        let temp_c = basename(temp)?;
        let target_c = basename(name)?;
        let result = (|| {
            let mut file = open_at(&dir, temp, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL, 0o600)?;
            // The process umask must not loosen or tighten the expected mode.
            file.set_permissions(Permissions::from_mode(0o600))?;
            validate_file(&file)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            // SAFETY: both names are NUL-terminated basenames under the same pinned dir.
            if unsafe { libc::renameat(dir.as_raw_fd(), temp_c.as_ptr(), dir.as_raw_fd(), target_c.as_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            dir.sync_all()
        })();
        if result.is_err() {
            // SAFETY: only our own unique temp basename under the pinned dir is removed.
            unsafe { libc::unlinkat(dir.as_raw_fd(), temp_c.as_ptr(), 0); }
        }
        result
    }
}

/// Off Unix the cache is disabled rather than implemented with a racy
/// path-based check-then-use: reads always miss and writes are dropped
/// without touching the filesystem.
#[cfg(not(unix))]
mod store {
    use std::io;
    use std::path::Path;

    pub(super) fn read(_home: &Path, _name: &str) -> io::Result<Vec<u8>> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "Odoo connection cache disabled on this platform"))
    }

    pub(super) fn write_atomic(_home: &Path, _name: &str, _temp: &str, _bytes: &[u8]) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct TestHome(PathBuf);
    impl TestHome {
        fn new() -> Self {
            static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!("odoo-connection-cache-{}-{}-{}", std::process::id(),
                SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos(),
                N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &Path { &self.0 }
    }
    impl Drop for TestHome { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }

    #[cfg_attr(not(unix), allow(dead_code))]
    fn path(home: &Path, key: &str) -> PathBuf {
        home.join("odoo").join("cache").join("connections").join(format!("{key}.json"))
    }

    fn entry(key: &str) -> CachedConnection {
        let mut edition = EditionGate::unknown();
        edition.odoo_version = "19.0".into();
        CachedConnection { identity_hash: key.to_string(), uid: 7, edition,
            server_version: "19.0".into(), created_at: 100 }
    }

    #[cfg(not(unix))]
    #[test]
    fn cache_is_disabled_off_unix() {
        let home = TestHome::new();
        let key = identity_hash("server/db/user/credential-hash/policy");
        save(home.path(), &entry(&key)).unwrap();
        assert!(load(home.path(), &key, 101).is_none());
        assert!(!home.path().join("odoo").exists(), "nothing may be written off Unix");
    }

    #[cfg(unix)]
    #[test]
    fn hit_miss_expiry_rotation_and_corruption() {
        let home = TestHome::new();
        let key = identity_hash("server/db/user/credential-hash/policy");
        assert!(load(home.path(), &key, 100).is_none());
        save(home.path(), &entry(&key)).unwrap();
        assert_eq!(load(home.path(), &key, 101).unwrap().uid, 7);
        assert!(load(home.path(), &key, 100 + TTL_SECS - 1).is_some());
        assert!(load(home.path(), &key, 100 + TTL_SECS).is_none());
        assert!(load(home.path(), &key, 99).is_none());
        assert!(load(home.path(), &identity_hash("rotated credential"), 101).is_none());
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(path(home.path(), &key)).unwrap().permissions().mode() & 0o777, 0o600);
            let dir = path(home.path(), &key).parent().unwrap().to_path_buf();
            assert_eq!(std::fs::metadata(dir).unwrap().permissions().mode() & 0o777, 0o700);
        }
        // Overwrite keeps working (rename over an existing regular file).
        save(home.path(), &entry(&key)).unwrap();
        std::fs::write(path(home.path(), &key), "invalid").unwrap();
        assert!(load(home.path(), &key, 101).is_none());
        // Non-hex keys never touch the filesystem.
        assert!(load(home.path(), "../escape", 101).is_none());
        assert!(save(home.path(), &entry("../escape")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_cache_file_is_refused_and_never_written_through() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let home = TestHome::new();
        let outside = TestHome::new();
        let key = identity_hash("symlinked file");
        // A valid-looking entry outside the cache, with private permissions.
        save(outside.path(), &entry(&key)).unwrap();
        let foreign = path(outside.path(), &key);
        assert!(load(outside.path(), &key, 101).is_some());
        save(home.path(), &entry(&identity_hash("bootstrap dirs"))).unwrap();
        let target = path(home.path(), &key);
        symlink(&foreign, &target).unwrap();
        assert!(load(home.path(), &key, 101).is_none());
        let before = std::fs::read(&foreign).unwrap();
        assert!(save(home.path(), &entry(&key)).is_err());
        assert_eq!(std::fs::read(&foreign).unwrap(), before);
        assert!(std::fs::symlink_metadata(&target).unwrap().file_type().is_symlink());
        // A hard link to the same inode is refused as well.
        std::fs::remove_file(&target).unwrap();
        std::fs::hard_link(&foreign, &target).unwrap();
        assert!(load(home.path(), &key, 101).is_none());
        std::fs::remove_file(&target).unwrap();
        // A group-readable entry is refused.
        save(home.path(), &entry(&key)).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert!(load(home.path(), &key, 101).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_parent_or_home_is_refused() {
        use std::os::unix::fs::symlink;
        let key = identity_hash("symlinked parent");
        for component in ["odoo", "odoo/cache", "odoo/cache/connections"] {
            let home = TestHome::new();
            let outside = TestHome::new();
            save(outside.path(), &entry(&key)).unwrap();
            let link = home.path().join(component);
            std::fs::create_dir_all(link.parent().unwrap()).unwrap();
            symlink(outside.path().join(component), &link).unwrap();
            assert!(load(home.path(), &key, 101).is_none(), "{component}");
            assert!(save(home.path(), &entry(&identity_hash("other"))).is_err(), "{component}");
            assert!(!path(outside.path(), &identity_hash("other")).exists(), "{component}");
        }
        // The DuDuClaw home itself may not be a symlink either.
        let real = TestHome::new();
        save(real.path(), &entry(&key)).unwrap();
        let holder = TestHome::new();
        let linked_home = holder.path().join("home");
        symlink(real.path(), &linked_home).unwrap();
        assert!(load(&linked_home, &key, 101).is_none());
        assert!(save(&linked_home, &entry(&key)).is_err());
    }
}
