//! Copy untrusted migration trees without following links or special files.
use super::{refused, Result};
use std::{ffi::{CStr, CString, OsStr}, fs::{self, File}, io, os::{fd::{AsRawFd, FromRawFd}, unix::{ffi::OsStrExt, fs::{MetadataExt, OpenOptionsExt, PermissionsExt}}}, path::{Component, Path, PathBuf}};

// Never derive migration authority from HOME or a caller-selected pathname.
fn sudo_identity(uid: Option<&OsStr>, gid: Option<&OsStr>) -> Result<(u32, u32)> {
    fn non_root_id(value: Option<&OsStr>, name: &str) -> Result<u32> {
        let value = value.and_then(OsStr::to_str)
            .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|value| *value != 0 && *value != u32::MAX)
            .ok_or_else(|| refused(format!("migration requires a valid non-root {name}; invoke sudo from your own account")))?;
        Ok(value)
    }
    Ok((non_root_id(uid, "SUDO_UID")?, non_root_id(gid, "SUDO_GID")?))
}

fn invoking_home(uid: u32) -> Result<PathBuf> {
    // getpwuid_r avoids process-global passwd storage in this multithreaded CLI.
    let mut size = 4096;
    loop {
        let mut buffer = vec![0u8; size];
        let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result = std::ptr::null_mut();
        let code = unsafe { libc::getpwuid_r(uid, &mut entry, buffer.as_mut_ptr().cast(), buffer.len(), &mut result) };
        if code == libc::ERANGE && size < 1024 * 1024 { size *= 2; continue; }
        if code != 0 || result.is_null() || entry.pw_uid != uid || entry.pw_dir.is_null() {
            return Err(refused("cannot resolve the invoking user's home with getpwuid"));
        }
        let home = PathBuf::from(OsStr::from_bytes(unsafe { CStr::from_ptr(entry.pw_dir) }.to_bytes()));
        if !home.is_absolute() { return Err(refused("invoking user's home must be absolute")); }
        return Ok(home);
    }
}

fn allowed_source_path(source: &Path, expected: &Path) -> Result<()> {
    for path in [source, expected] {
        if ["/var/lib/duduclaw-protected", "/var/lib/duduclaw", "/etc", "/root", "/proc", "/sys"]
            .iter().any(|root| path.starts_with(root)) {
            return Err(refused("migration refuses protected or system directories"));
        }
    }
    if source != expected {
        return Err(refused("migration source must be the invoking user's .duduclaw directory"));
    }
    Ok(())
}

fn source_owner(metadata: &fs::Metadata, uid: u32) -> Result<()> {
    if metadata.uid() != uid {
        return Err(refused("every migration source directory and file must be owned by SUDO_UID"));
    }
    Ok(())
}

fn authorized_source(source: &Path, home: &Path, uid: u32) -> Result<File> {
    let expected_path = home.join(".duduclaw");
    // Reject system roots before resolving, and repeat after resolution.
    for path in [source, expected_path.as_path()] { allowed_source_path(path, path)?; }
    let expected = fs::canonicalize(&expected_path)?;
    allowed_source_path(&fs::canonicalize(source)?, &expected)?;
    let source = open_source(source)?;
    let expected_file = open_source(&expected_path)?;
    let source_meta = source.metadata()?;
    let expected_meta = expected_file.metadata()?;
    source_owner(&source_meta, uid)?;
    // Compare the pinned objects as well as their names: an untrusted parent
    // may have been renamed between canonicalization and openat traversal.
    if source_meta.dev() != expected_meta.dev() || source_meta.ino() != expected_meta.ino() {
        return Err(refused("migration source changed while verifying the invoking user's home"));
    }
    let resolved = fs::read_link(format!("/proc/self/fd/{}", source.as_raw_fd()))?;
    allowed_source_path(&resolved, &expected)?;
    Ok(source)
}

// Open each original component relative to a pinned directory. Canonicalize
// followed by a pathname open would let a writable ancestor redirect root.
fn open_source(path: &Path) -> Result<File> {
    let mut directory = fs::OpenOptions::new().read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(if path.is_absolute() { "/" } else { "." })?;
    for component in path.components() {
        let name = match component {
            Component::RootDir | Component::CurDir => continue,
            Component::Normal(name) => name,
            Component::ParentDir => return Err(refused("migration source must not contain parent-directory components")),
            Component::Prefix(_) => return Err(refused("invalid migration source path")),
        };
        let name = CString::new(name.as_bytes()).map_err(|_| refused("invalid migration directory"))?;
        let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
        if fd < 0 { return Err(io::Error::last_os_error().into()); }
        directory = unsafe { File::from_raw_fd(fd) };
    }
    Ok(directory)
}

fn child(parent: &File, name: &std::ffi::OsStr, source_uid: u32) -> Result<File> {
    let name = CString::new(name.as_bytes()).map_err(|_| refused("invalid migration filename"))?;
    // O_PATH inspects the inode without opening devices, FIFOs or symlink targets.
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
    if fd < 0 { return Err(io::Error::last_os_error().into()); }
    let pinned = unsafe { File::from_raw_fd(fd) };
    let metadata = pinned.metadata()?;
    source_owner(&metadata, source_uid)?;
    if !metadata.is_dir() && !(metadata.is_file() && metadata.nlink() == 1) {
        return Err(refused("migration refuses symlinks, hard links and special files"));
    }
    // Reopen the pinned inode, never the caller-controlled filename.
    Ok(File::open(format!("/proc/self/fd/{}", pinned.as_raw_fd()))?)
}

fn owner(file: &File, uid: u32, gid: u32) -> Result<()> {
    if unsafe { libc::fchown(file.as_raw_fd(), uid, gid) } != 0 { return Err(io::Error::last_os_error().into()); }
    Ok(())
}

fn tree(source: &File, destination: &Path, uid: u32, gid: u32, source_uid: u32, depth: usize) -> Result<()> {
    if depth > 128 { return Err(refused("migration directory nesting exceeds 128")); }
    let before = source.metadata()?;
    source_owner(&before, source_uid)?;
    if !before.is_dir() { return Err(refused("migration source must be a directory")); }
    for entry in fs::read_dir(format!("/proc/self/fd/{}", source.as_raw_fd()))? {
        let name = entry?.file_name();
        let mut input = child(source, &name, source_uid)?;
        let metadata = input.metadata()?;
        source_owner(&metadata, source_uid)?;
        let output = destination.join(&name);
        if metadata.is_dir() {
            fs::create_dir(&output)?;
            fs::set_permissions(&output, fs::Permissions::from_mode(0o700))?;
            tree(&input, &output, uid, gid, source_uid, depth + 1)?;
            owner(&File::open(&output)?, uid, gid)?;
        } else if metadata.is_file() && metadata.nlink() == 1 {
            let mut target = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&output)?;
            io::copy(&mut input, &mut target)?;
            let after = input.metadata()?;
            source_owner(&after, source_uid)?;
            file_unchanged(&metadata, &after)?;
            owner(&target, uid, gid)?;
            // Keep runnable employee scripts while removing all group/other and
            // setuid/setgid permissions from the imported tree.
            target.set_permissions(fs::Permissions::from_mode(0o600 | (metadata.mode() & 0o100)))?;
            target.sync_all()?;
        } else { return Err(refused("migration refuses symlinks, hard links and special files")); }
    }
    let after = source.metadata()?;
    source_owner(&after, source_uid)?;
    directory_unchanged(&before, &after)?;
    File::open(destination)?.sync_all()?;
    Ok(())
}

fn file_unchanged(before: &fs::Metadata, after: &fs::Metadata) -> Result<()> {
    if before.len() != after.len() || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec() || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec() {
        return Err(refused("migration source changed; stop the old gateway before copying"));
    }
    Ok(())
}

fn directory_unchanged(before: &fs::Metadata, after: &fs::Metadata) -> Result<()> {
    if before.mtime() != after.mtime() || before.mtime_nsec() != after.mtime_nsec() {
        return Err(refused("migration source directory changed; stop the old gateway first"));
    }
    Ok(())
}

// The same traversal and refusals as `tree`, writing nothing: ownership,
// links/special files, nesting, and change detection while each file is read
// and each directory listed. A quiet source can still change later; `tree`
// repeats every check during the real copy.
fn walk(source: &File, source_uid: u32, depth: usize) -> Result<()> {
    if depth > 128 { return Err(refused("migration directory nesting exceeds 128")); }
    let before = source.metadata()?;
    source_owner(&before, source_uid)?;
    if !before.is_dir() { return Err(refused("migration source must be a directory")); }
    for entry in fs::read_dir(format!("/proc/self/fd/{}", source.as_raw_fd()))? {
        let name = entry?.file_name();
        let mut input = child(source, &name, source_uid)?;
        let metadata = input.metadata()?;
        source_owner(&metadata, source_uid)?;
        if metadata.is_dir() {
            walk(&input, source_uid, depth + 1)?;
        } else if metadata.is_file() && metadata.nlink() == 1 {
            io::copy(&mut input, &mut io::sink())?;
            let after = input.metadata()?;
            source_owner(&after, source_uid)?;
            file_unchanged(&metadata, &after)?;
        } else { return Err(refused("migration refuses symlinks, hard links and special files")); }
    }
    let after = source.metadata()?;
    source_owner(&after, source_uid)?;
    directory_unchanged(&before, &after)
}

fn authorize(source: &Path) -> Result<(u32, File)> {
    let sudo_uid = std::env::var_os("SUDO_UID");
    let sudo_gid = std::env::var_os("SUDO_GID");
    let (source_uid, _) = sudo_identity(sudo_uid.as_deref(), sudo_gid.as_deref())?;
    let invoking_home = invoking_home(source_uid)?;
    Ok((source_uid, authorized_source(source, &invoking_home, source_uid)?))
}

fn target_exists(target: &Path, force: bool) -> Result<bool> {
    let exists = match fs::symlink_metadata(target) {
        Ok(_) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    if exists && !force { return Err(refused("migration target exists; use --force-migrate to preserve it as a backup and replace it")); }
    Ok(exists)
}

/// Every source check `copy` makes that can be made up front, before the
/// installer creates the account or anything else. Creates nothing.
pub(super) fn preflight(source: &Path, force: bool) -> Result<()> {
    let (source_uid, source) = authorize(source)?;
    target_exists(&Path::new("/var/lib/duduclaw").join(".duduclaw"), force)?;
    walk(&source, source_uid, 0)
}

fn rename_no_replace(source: &Path, destination: &Path) -> Result<()> {
    let source = CString::new(source.as_os_str().as_bytes()).map_err(|_| refused("invalid source path"))?;
    let destination = CString::new(destination.as_os_str().as_bytes()).map_err(|_| refused("invalid destination path"))?;
    if unsafe { libc::syscall(libc::SYS_renameat2, libc::AT_FDCWD, source.as_ptr(), libc::AT_FDCWD, destination.as_ptr(), libc::RENAME_NOREPLACE) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(())
}

pub(super) fn copy(source: &Path, uid: u32, gid: u32, force: bool) -> Result<()> {
    let home = Path::new("/var/lib/duduclaw");
    let target = home.join(".duduclaw");
    let (source_uid, source) = authorize(source)?;
    let exists = target_exists(&target, force)?;
    // The top-level temporary directory remains root-owned 0700 throughout.
    let staging = tempfile::Builder::new().permissions(fs::Permissions::from_mode(0o700)).prefix(".duduclaw-migrate-").tempdir_in("/var/lib")?;
    let staged = staging.path().join("data");
    fs::create_dir(&staged)?;
    fs::set_permissions(&staged, fs::Permissions::from_mode(0o700))?;
    tree(&source, &staged, uid, gid, source_uid, 0)?;
    owner(&File::open(&staged)?, uid, gid)?;
    if exists {
        let backup = home.join(format!(".duduclaw.previous-{}", uuid::Uuid::new_v4()));
        rename_no_replace(&target, &backup)?;
        println!("原資料保留於 {}", backup.display());
    }
    rename_no_replace(&staged, &target)?;
    File::open(home)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn service_migration_requires_non_root_sudo_identity() {
        let id = Some(OsStr::new("1000"));
        assert!(sudo_identity(None, id).is_err());
        assert!(sudo_identity(id, None).is_err());
        for bad in ["0", "", "abc", "-1", "+1000", " 1000", "4294967295", "4294967296"] {
            assert!(sudo_identity(Some(OsStr::new(bad)), id).is_err(), "SUDO_UID={bad}");
            assert!(sudo_identity(id, Some(OsStr::new(bad))).is_err(), "SUDO_GID={bad}");
        }
        assert_eq!(sudo_identity(id, Some(OsStr::new("1001"))).unwrap(), (1000, 1001));
    }

    #[test]
    fn service_migration_requires_exact_invoking_user_directory() {
        let root = tempfile::tempdir().unwrap();
        let home = fs::canonicalize(root.path()).unwrap();
        let source = home.join(".duduclaw");
        let other = home.join("other");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&other).unwrap();
        let uid = unsafe { libc::getuid() };
        assert!(authorized_source(&source, &home, uid).is_ok());
        assert!(authorized_source(&other, &home, uid).is_err());
        for path in ["/var/lib/duduclaw-protected/root.key", "/var/lib/duduclaw/.duduclaw", "/etc/.duduclaw", "/root/.duduclaw", "/proc/self", "/sys/devices"] {
            assert!(allowed_source_path(Path::new(path), Path::new(path)).is_err());
        }
    }

    #[test]
    fn service_migration_refuses_file_owned_by_another_uid_in_tree() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("foreign"), b"not the invoking user's data").unwrap();
        let directory = File::open(root.path()).unwrap();
        let owner_uid = directory.metadata().unwrap().uid();
        let other_uid = if owner_uid == 1000 { 1001 } else { 1000 };
        // The real file is owned by owner_uid, while the migration claims
        // other_uid. This exercises the exact per-entry guard without chown.
        let error = child(&directory, OsStr::new("foreign"), other_uid).unwrap_err();
        assert!(error.to_string().contains("owned by SUDO_UID"));
        assert!(child(&directory, OsStr::new("foreign"), owner_uid).is_ok());
        fs::create_dir(root.path().join("foreign-directory")).unwrap();
        assert!(child(&directory, OsStr::new("foreign-directory"), other_uid).is_err());
    }

    #[test]
    fn service_migration_rejects_links_in_every_source_component() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::create_dir(source.join("data")).unwrap();
        symlink(&source, root.path().join("alias")).unwrap();
        // /tmp may be a symlink on some distributions: choose the resolved test
        // fixture root, while the path under test still carries the unsafe link.
        let root = fs::canonicalize(root.path()).unwrap();
        assert!(open_source(&root.join("source/data")).is_ok());
        assert!(open_source(&root.join("alias/data")).is_err());
        assert!(open_source(&root.join("alias")).is_err());
        assert!(open_source(&root.join("source/../source/data")).is_err());
    }

    #[test]
    fn service_migration_preflight_walk_refuses_before_any_copy() {
        let uid = unsafe { libc::getuid() };
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("agents/a")).unwrap();
        fs::write(root.path().join("agents/a/config.toml"), b"data").unwrap();
        fs::write(root.path().join("config.toml"), b"data").unwrap();
        let before: Vec<_> = fs::read_dir(root.path()).unwrap().map(|entry| entry.unwrap().file_name()).collect();
        assert!(walk(&File::open(root.path()).unwrap(), uid, 0).is_ok());
        let after: Vec<_> = fs::read_dir(root.path()).unwrap().map(|entry| entry.unwrap().file_name()).collect();
        assert_eq!(before, after, "the walk writes nothing");
        let other = if uid == 1000 { 1001 } else { 1000 };
        assert!(walk(&File::open(root.path()).unwrap(), other, 0).is_err());
        // A nested symlink or hard link refuses the whole walk, as copy would.
        symlink("config.toml", root.path().join("agents/a/link")).unwrap();
        assert!(walk(&File::open(root.path()).unwrap(), uid, 0).is_err());
        fs::remove_file(root.path().join("agents/a/link")).unwrap();
        fs::hard_link(root.path().join("config.toml"), root.path().join("agents/hard")).unwrap();
        assert!(walk(&File::open(root.path()).unwrap(), uid, 0).is_err());
        fs::remove_file(root.path().join("agents/hard")).unwrap();
        assert!(walk(&File::open(root.path()).unwrap(), uid, 0).is_ok());
    }

    #[test]
    fn service_migration_pins_regular_files_and_refuses_special_entries() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("file"), b"data").unwrap();
        symlink("file", root.path().join("link")).unwrap();
        let directory = File::open(root.path()).unwrap();
        assert!(child(&directory, std::ffi::OsStr::new("file"), unsafe { libc::getuid() }).is_ok());
        assert!(child(&directory, std::ffi::OsStr::new("link"), unsafe { libc::getuid() }).is_err());
        fs::hard_link(root.path().join("file"), root.path().join("hard")).unwrap();
        assert!(child(&directory, std::ffi::OsStr::new("hard"), unsafe { libc::getuid() }).is_err());
        let fifo = CString::new(root.path().join("fifo").as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(child(&directory, std::ffi::OsStr::new("fifo"), unsafe { libc::getuid() }).is_err());
    }
}
