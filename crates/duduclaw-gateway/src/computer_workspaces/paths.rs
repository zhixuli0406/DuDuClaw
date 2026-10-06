//! Where workspaces live on disk, and the checks a mount source must pass
//! (design §2, §4.2 step 4). Every path is derived by the gateway from the
//! home and a server-issued id; nothing here takes a caller path.

use std::io;
use std::path::{Path, PathBuf};

use crate::fs_safe::SafeDir;

/// `<home>/computer_workspaces` — fixed, never configurable.
pub const ROOT_DIR: &str = "computer_workspaces";
/// The only directory of a workspace that is mounted.
pub const DATA_DIR: &str = "data";
/// `<home>/computer_workspaces.db`.
pub const DB_FILE: &str = "computer_workspaces.db";
/// A root-only tmpfs (mode 0700, uid 0, gid 0) the workspace is mounted
/// under, so the browser's unprivileged user cannot even traverse to it
/// (review M1); only the gateway's root `docker exec` reads it.
pub const MOUNT_PARENT: &str = "/workspace";
/// The `--tmpfs` spec of [`MOUNT_PARENT`] (no device files, no exec, tiny:
/// nothing is meant to be written there).
pub const MOUNT_PARENT_TMPFS: &str =
    "/workspace:rw,nosuid,nodev,noexec,size=64k,mode=0700,uid=0,gid=0";
/// Where the workspace appears inside the container (read-only bind).
pub const MOUNT_TARGET: &str = "/workspace/files";

/// `ws-` + 32 lowercase hex digits.
pub fn valid_workspace_id(id: &str) -> bool {
    id.strip_prefix("ws-").is_some_and(|hex| {
        hex.len() == 32
            && hex
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    })
}

/// A fresh, never-reused workspace id.
pub fn new_workspace_id() -> String {
    format!("ws-{}", uuid::Uuid::new_v4().as_simple())
}

/// The canonical home (all derived paths hang off it).
pub fn canonical_home(home: &Path) -> io::Result<PathBuf> {
    std::fs::canonicalize(home)
}

/// `<canonical home>/computer_workspaces/<id>/data`.
pub fn data_dir(canonical_home: &Path, id: &str) -> PathBuf {
    canonical_home.join(ROOT_DIR).join(id).join(DATA_DIR)
}

fn invalid(msg: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

/// Make `<root>/<id>/data` (each level 0700) and return its canonical path.
#[cfg(unix)]
pub fn create_workspace_dirs(home: &Path, id: &str) -> io::Result<PathBuf> {
    if !valid_workspace_id(id) {
        return Err(invalid("invalid workspace id"));
    }
    let home = canonical_home(home)?;
    let data = data_dir(&home, id);
    crate::discovery::workspace::create_private_directory(&data)?;
    // New directory entries are durable only once their parents are synced.
    for dir in [
        home.clone(),
        home.join(ROOT_DIR),
        home.join(ROOT_DIR).join(id),
    ] {
        std::fs::File::open(&dir)?.sync_all()?;
    }
    let canonical = crate::discovery::workspace::canonical_real_directory(&data)?;
    if canonical != data {
        return Err(invalid("workspace directory resolves elsewhere"));
    }
    Ok(canonical)
}

/// Not supported off unix: nothing is created.
#[cfg(not(unix))]
pub fn create_workspace_dirs(_home: &Path, _id: &str) -> io::Result<PathBuf> {
    Err(io::Error::other(
        "computer workspaces are not supported on this platform",
    ))
}

/// Why a mount source was refused (closed set, audit only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsafeSource {
    InvalidId,
    Missing,
    Symlink,
    NotDirectory,
    WrongOwner,
    WrongMode,
    ResolvesElsewhere,
    BadCharacters,
    Unsupported,
}

impl UnsafeSource {
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidId => "invalid_id",
            Self::Missing => "missing",
            Self::Symlink => "symlink",
            Self::NotDirectory => "not_directory",
            Self::WrongOwner => "wrong_owner",
            Self::WrongMode => "wrong_mode",
            Self::ResolvesElsewhere => "resolves_elsewhere",
            Self::BadCharacters => "bad_characters",
            Self::Unsupported => "unsupported",
        }
    }
}

/// Check one level: a real directory (not followed), owned by this process's
/// uid, mode 0700.
#[cfg(unix)]
fn check_level(path: &Path, require_private: bool) -> Result<(), UnsafeSource> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(path).map_err(|_| UnsafeSource::Missing)?;
    if meta.file_type().is_symlink() {
        return Err(UnsafeSource::Symlink);
    }
    if !meta.is_dir() {
        return Err(UnsafeSource::NotDirectory);
    }
    // SAFETY: geteuid has no preconditions.
    if meta.uid() != unsafe { libc::geteuid() } {
        return Err(UnsafeSource::WrongOwner);
    }
    if require_private && meta.mode() & 0o777 != 0o700 {
        return Err(UnsafeSource::WrongMode);
    }
    Ok(())
}

/// The last check before `docker run`: `<root>`, `<root>/<id>` and
/// `<root>/<id>/data` are real 0700 directories of this uid, no ancestor is a
/// symlink (macOS `/var`, `/tmp` aliases aside), the canonical path is the
/// derived one, and it carries no character that would break `--mount`.
#[cfg(unix)]
pub fn verify_mount_source(home: &Path, id: &str) -> Result<PathBuf, UnsafeSource> {
    if !valid_workspace_id(id) {
        return Err(UnsafeSource::InvalidId);
    }
    let home = canonical_home(home).map_err(|_| UnsafeSource::Missing)?;
    let root = home.join(ROOT_DIR);
    let ws = root.join(id);
    let data = ws.join(DATA_DIR);
    check_level(&root, true)?;
    check_level(&ws, true)?;
    check_level(&data, true)?;
    let canonical = crate::discovery::workspace::canonical_real_directory(&data)
        .map_err(|_| UnsafeSource::Symlink)?;
    if canonical != data {
        return Err(UnsafeSource::ResolvesElsewhere);
    }
    let text = canonical.to_str().ok_or(UnsafeSource::BadCharacters)?;
    if text
        .bytes()
        .any(|c| matches!(c, b',' | b'\n' | b'\r' | b'"' | 0))
    {
        return Err(UnsafeSource::BadCharacters);
    }
    Ok(canonical)
}

#[cfg(not(unix))]
pub fn verify_mount_source(_home: &Path, _id: &str) -> Result<PathBuf, UnsafeSource> {
    Err(UnsafeSource::Unsupported)
}

/// Open `<root>/<id>/data` without following any link below the canonical
/// home. `Ok(None)` when it does not exist.
pub fn open_data(home: &Path, id: &str) -> io::Result<Option<SafeDir>> {
    if !valid_workspace_id(id) {
        return Err(invalid("invalid workspace id"));
    }
    let home = canonical_home(home)?;
    let base = SafeDir::open_root(&home)?;
    base.open_path(&[ROOT_DIR, id, DATA_DIR], false)
}

/// Delete `<root>/<id>/` entirely (operator delete / failed-create cleanup).
/// The path comes only from the id; `<root>/<id>` must not be a link and its
/// canonical parent must be the canonical root. `remove_dir_all` does not
/// follow links inside. Absent is success.
pub fn remove_workspace_dir(home: &Path, id: &str) -> io::Result<()> {
    if !valid_workspace_id(id) {
        return Err(invalid("invalid workspace id"));
    }
    let home = canonical_home(home)?;
    let root = home.join(ROOT_DIR);
    let ws = root.join(id);
    match std::fs::symlink_metadata(&ws) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
        Ok(m) if m.file_type().is_symlink() || !m.is_dir() => {
            return Err(invalid("workspace path is not a real directory"));
        }
        Ok(_) => {}
    }
    let canonical_root = std::fs::canonicalize(&root)?;
    let parent = std::fs::canonicalize(&ws)?
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| invalid("no parent"))?;
    if canonical_root != root || parent != canonical_root {
        return Err(invalid("workspace path resolves outside the root"));
    }
    std::fs::remove_dir_all(&ws)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn ids_are_strict() {
        let id = new_workspace_id();
        assert!(valid_workspace_id(&id));
        for bad in [
            "",
            "ws-",
            "ws-0123",
            "WS-0123456789abcdef0123456789abcdef",
            "ws-0123456789ABCDEF0123456789abcdef",
            "ws-0123456789abcdef0123456789abcdef0",
            "ws-../../etc/passwd/aaaaaaaaaaaaaaaa",
            "new",
        ] {
            assert!(!valid_workspace_id(bad), "{bad}");
        }
    }

    #[test]
    fn created_dirs_verify_and_tampering_is_refused() {
        let home = tempfile::tempdir().unwrap();
        let id = new_workspace_id();
        let data = create_workspace_dirs(home.path(), &id).unwrap();
        assert_eq!(verify_mount_source(home.path(), &id).unwrap(), data);
        std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            verify_mount_source(home.path(), &id),
            Err(UnsafeSource::WrongMode)
        );
        std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o700)).unwrap();
        // data/ swapped for a link to somewhere else.
        let outside = tempfile::tempdir().unwrap();
        std::fs::remove_dir(&data).unwrap();
        symlink(outside.path(), &data).unwrap();
        assert_eq!(
            verify_mount_source(home.path(), &id),
            Err(UnsafeSource::Symlink)
        );
        assert!(open_data(home.path(), &id).is_err());
    }

    #[test]
    fn remove_only_touches_the_derived_directory() {
        let home = tempfile::tempdir().unwrap();
        let id = new_workspace_id();
        let data = create_workspace_dirs(home.path(), &id).unwrap();
        std::fs::write(data.join("a"), "x").unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("keep"), "k").unwrap();
        symlink(outside.path(), data.join("link")).unwrap();
        remove_workspace_dir(home.path(), &id).unwrap();
        assert!(!data.exists());
        assert!(outside.path().join("keep").exists());
        assert!(remove_workspace_dir(home.path(), &id).is_ok());
    }
}
