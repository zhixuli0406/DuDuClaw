//! Symlink-safe file operations inside an **agent-writable** directory.
//!
//! The Antigravity runtime writes `<work_root>/.agents/mcp_config.json` and may
//! delete `<agent_dir>/.gemini/antigravity-cli/settings.json`. Both trees are
//! writable by the agent itself, so a planted symlink must never redirect the
//! gateway's read, write or delete outside them — under a native sandbox that
//! would be a confinement escape.
//!
//! Unix: every step after the trusted root is fd-relative (`openat` /
//! `mkdirat` / `renameat` / `unlinkat`) with `O_NOFOLLOW`, so a symlinked
//! intermediate directory or target is refused by the kernel and a swap
//! between "check" and "use" cannot redirect the operation. Temp files get a
//! random name and `O_CREAT|O_EXCL|O_NOFOLLOW`.
//!
//! Other platforms: best-effort path checks with `symlink_metadata` plus a
//! canonical-containment check (no fd-relative API in std).

use std::io;
use std::path::Path;

fn refused(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, msg.into())
}

/// Reject anything that is not a single, plain path component.
fn check_name(name: &str) -> io::Result<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
    {
        return Err(refused(format!("invalid path component {name:?}")));
    }
    Ok(())
}

fn tmp_name(name: &str) -> String {
    format!(".{name}.tmp-{}", uuid::Uuid::new_v4().simple())
}

#[cfg(unix)]
mod imp {
    use super::{check_name, refused, tmp_name};
    use nix::errno::Errno;
    use nix::fcntl::{OFlag, openat, renameat};
    use nix::sys::stat::{FileStat, Mode, SFlag, fstatat, mkdirat};
    use nix::unistd::{UnlinkatFlags, unlinkat};
    use std::io::{self, Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::path::Path;

    fn to_io(e: Errno) -> io::Error {
        io::Error::from_raw_os_error(e as i32)
    }

    fn kind(st: &FileStat) -> SFlag {
        SFlag::from_bits_truncate(st.st_mode & SFlag::S_IFMT.bits())
    }

    /// An open directory handle; every operation is relative to it.
    pub(crate) struct SafeDir {
        fd: OwnedFd,
    }

    impl SafeDir {
        /// Open a trusted root chosen by the gateway (symlinks in the root path
        /// itself are followed: the root is not agent-planted).
        pub(crate) fn open_root(path: &Path) -> io::Result<Self> {
            let raw = openat(
                None,
                path,
                OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC,
                Mode::empty(),
            )
            .map_err(to_io)?;
            // SAFETY: `openat` just returned this fd and nothing else owns it.
            Ok(Self {
                fd: unsafe { OwnedFd::from_raw_fd(raw) },
            })
        }

        fn stat(&self, name: &str) -> io::Result<Option<FileStat>> {
            match fstatat(
                Some(self.fd.as_raw_fd()),
                name,
                nix::fcntl::AtFlags::AT_SYMLINK_NOFOLLOW,
            ) {
                Ok(st) => Ok(Some(st)),
                Err(Errno::ENOENT) => Ok(None),
                Err(e) => Err(to_io(e)),
            }
        }

        /// Open child directory `name` without following a symlink. `create`
        /// makes it (0700) when absent. `Ok(None)` when absent and not created.
        pub(crate) fn child_dir(&self, name: &str, create: bool) -> io::Result<Option<SafeDir>> {
            check_name(name)?;
            match self.stat(name)? {
                None if create => match mkdirat(Some(self.fd.as_raw_fd()), name, Mode::S_IRWXU) {
                    Ok(()) | Err(Errno::EEXIST) => {}
                    Err(e) => return Err(to_io(e)),
                },
                None => return Ok(None),
                Some(st) if kind(&st) == SFlag::S_IFLNK => {
                    return Err(refused(format!("{name} is a symlink — refusing to follow it")));
                }
                Some(st) if kind(&st) != SFlag::S_IFDIR => {
                    return Err(refused(format!("{name} is not a directory")));
                }
                Some(_) => {}
            }
            let raw = openat(
                Some(self.fd.as_raw_fd()),
                name,
                OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
                Mode::empty(),
            )
            .map_err(|e| match e {
                Errno::ELOOP | Errno::ENOTDIR => {
                    refused(format!("{name} is not a real directory — refusing to follow it"))
                }
                other => to_io(other),
            })?;
            // SAFETY: freshly returned fd, owned by nobody else.
            Ok(Some(SafeDir {
                fd: unsafe { OwnedFd::from_raw_fd(raw) },
            }))
        }

        /// Read regular file `name`. `Ok(None)` when absent; a symlink or a
        /// non-regular file is refused.
        pub(crate) fn read_file(&self, name: &str) -> io::Result<Option<String>> {
            check_name(name)?;
            match self.stat(name)? {
                None => return Ok(None),
                Some(st) if kind(&st) != SFlag::S_IFREG => {
                    return Err(refused(format!(
                        "{name} is not a regular file (symlink or special) — refusing to read it"
                    )));
                }
                Some(_) => {}
            }
            let raw = match openat(
                Some(self.fd.as_raw_fd()),
                name,
                OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK,
                Mode::empty(),
            ) {
                Ok(fd) => fd,
                Err(Errno::ENOENT) => return Ok(None),
                Err(Errno::ELOOP) => {
                    return Err(refused(format!("{name} is a symlink — refusing to read it")));
                }
                Err(e) => return Err(to_io(e)),
            };
            // SAFETY: freshly returned fd, owned by nobody else.
            let mut file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(raw) });
            if !file.metadata()?.is_file() {
                return Err(refused(format!("{name} is not a regular file")));
            }
            let mut out = String::new();
            file.read_to_string(&mut out)?;
            Ok(Some(out))
        }

        /// Replace regular file `name` atomically: random temp name created with
        /// `O_EXCL|O_NOFOLLOW`, then `renameat`. A symlink or non-regular file
        /// at `name` is refused. New files are 0600; an existing file keeps its
        /// permission bits.
        pub(crate) fn write_file_atomic(&self, name: &str, contents: &str) -> io::Result<()> {
            check_name(name)?;
            let mode = match self.stat(name)? {
                None => 0o600,
                Some(st) if kind(&st) != SFlag::S_IFREG => {
                    return Err(refused(format!(
                        "{name} is not a regular file (symlink or special) — refusing to replace it"
                    )));
                }
                Some(st) => (st.st_mode as u32) & 0o7777,
            };
            let tmp = tmp_name(name);
            let raw = openat(
                Some(self.fd.as_raw_fd()),
                tmp.as_str(),
                OFlag::O_WRONLY
                    | OFlag::O_CREAT
                    | OFlag::O_EXCL
                    | OFlag::O_NOFOLLOW
                    | OFlag::O_CLOEXEC,
                Mode::from_bits_truncate(0o600),
            )
            .map_err(to_io)?;
            // SAFETY: freshly returned fd, owned by nobody else.
            let mut file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(raw) });
            let written = (|| -> io::Result<()> {
                file.write_all(contents.as_bytes())?;
                use std::os::unix::fs::PermissionsExt;
                file.set_permissions(std::fs::Permissions::from_mode(mode))?;
                file.sync_all()?;
                renameat(
                    Some(self.fd.as_raw_fd()),
                    tmp.as_str(),
                    Some(self.fd.as_raw_fd()),
                    name,
                )
                .map_err(to_io)
            })();
            if written.is_err() {
                let _ = unlinkat(Some(self.fd.as_raw_fd()), tmp.as_str(), UnlinkatFlags::NoRemoveDir);
            }
            written
        }

        /// Remove regular file `name` (the directory entry, never a target).
        pub(crate) fn remove_file(&self, name: &str) -> io::Result<()> {
            check_name(name)?;
            unlinkat(Some(self.fd.as_raw_fd()), name, UnlinkatFlags::NoRemoveDir).map_err(to_io)
        }
    }
}

#[cfg(not(unix))]
mod imp {
    use super::{check_name, refused, tmp_name};
    use std::io::{self, Write};
    use std::path::{Path, PathBuf};

    /// Path-based fallback: checks with `symlink_metadata` before every use
    /// and verifies canonical containment in the root.
    pub(crate) struct SafeDir {
        path: PathBuf,
        root: PathBuf,
    }

    impl SafeDir {
        pub(crate) fn open_root(path: &Path) -> io::Result<Self> {
            let canon = path.canonicalize()?;
            if !canon.is_dir() {
                return Err(refused("root is not a directory"));
            }
            Ok(Self {
                path: canon.clone(),
                root: canon,
            })
        }

        fn entry(&self, name: &str) -> io::Result<Option<std::fs::Metadata>> {
            match std::fs::symlink_metadata(self.path.join(name)) {
                Ok(m) => Ok(Some(m)),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e),
            }
        }

        pub(crate) fn child_dir(&self, name: &str, create: bool) -> io::Result<Option<SafeDir>> {
            check_name(name)?;
            let p = self.path.join(name);
            match self.entry(name)? {
                None if create => std::fs::create_dir(&p).or_else(|e| {
                    if e.kind() == io::ErrorKind::AlreadyExists {
                        Ok(())
                    } else {
                        Err(e)
                    }
                })?,
                None => return Ok(None),
                Some(m) if m.file_type().is_symlink() || !m.is_dir() => {
                    return Err(refused(format!("{name} is not a real directory")));
                }
                Some(_) => {}
            }
            let m = std::fs::symlink_metadata(&p)?;
            if m.file_type().is_symlink() || !m.is_dir() {
                return Err(refused(format!("{name} is not a real directory")));
            }
            let canon = p.canonicalize()?;
            if !canon.starts_with(&self.root) {
                return Err(refused(format!("{name} resolves outside the root")));
            }
            Ok(Some(SafeDir {
                path: canon,
                root: self.root.clone(),
            }))
        }

        pub(crate) fn read_file(&self, name: &str) -> io::Result<Option<String>> {
            check_name(name)?;
            match self.entry(name)? {
                None => Ok(None),
                Some(m) if m.file_type().is_symlink() || !m.is_file() => {
                    Err(refused(format!("{name} is not a regular file")))
                }
                Some(_) => std::fs::read_to_string(self.path.join(name)).map(Some),
            }
        }

        pub(crate) fn write_file_atomic(&self, name: &str, contents: &str) -> io::Result<()> {
            check_name(name)?;
            let perms = match self.entry(name)? {
                None => None,
                Some(m) if m.file_type().is_symlink() || !m.is_file() => {
                    return Err(refused(format!("{name} is not a regular file")));
                }
                Some(m) => Some(m.permissions()),
            };
            let tmp = self.path.join(tmp_name(name));
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)?;
            let res = (|| -> io::Result<()> {
                f.write_all(contents.as_bytes())?;
                if let Some(p) = perms {
                    std::fs::set_permissions(&tmp, p)?;
                }
                drop(f);
                std::fs::rename(&tmp, self.path.join(name))
            })();
            if res.is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
            res
        }

        pub(crate) fn remove_file(&self, name: &str) -> io::Result<()> {
            check_name(name)?;
            std::fs::remove_file(self.path.join(name))
        }
    }
}

pub(crate) use imp::SafeDir;

/// Convenience: is `path` itself a symlink (without following it)?
pub(crate) fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn symlinked_child_dir_is_refused_and_nothing_is_created_outside() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), root.path().join(".agents")).unwrap();
        let dir = SafeDir::open_root(root.path()).unwrap();
        assert!(dir.child_dir(".agents", true).is_err());
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }

    #[test]
    fn symlinked_file_is_neither_read_nor_replaced() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("victim.json");
        std::fs::write(&victim, "SECRET").unwrap();
        symlink(&victim, root.path().join("f.json")).unwrap();
        let dir = SafeDir::open_root(root.path()).unwrap();
        assert!(dir.read_file("f.json").is_err());
        assert!(dir.write_file_atomic("f.json", "{}").is_err());
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "SECRET");
        assert!(is_symlink(&root.path().join("f.json")));
    }

    #[test]
    fn writes_are_atomic_new_files_0600_existing_mode_kept() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let dir = SafeDir::open_root(root.path()).unwrap();
        dir.write_file_atomic("a.json", "1").unwrap();
        let p = root.path().join("a.json");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "1");
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        dir.write_file_atomic("a.json", "2").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "2");
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o644);
        // No temp files left behind.
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn a_planted_predictable_temp_name_is_not_followed() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("victim");
        std::fs::write(&victim, "KEEP").unwrap();
        // Plant symlinks at every name the old code would have used.
        for name in [
            format!("a.json.tmp-{}", std::process::id()),
            "a.json.tmp".to_string(),
            ".a.json.tmp".to_string(),
        ] {
            symlink(&victim, root.path().join(name)).unwrap();
        }
        let dir = SafeDir::open_root(root.path()).unwrap();
        dir.write_file_atomic("a.json", "new").unwrap();
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "KEEP");
        assert_eq!(std::fs::read_to_string(root.path().join("a.json")).unwrap(), "new");
    }

    #[test]
    fn invalid_names_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let dir = SafeDir::open_root(root.path()).unwrap();
        for bad in ["", ".", "..", "a/b", "../x"] {
            assert!(dir.read_file(bad).is_err(), "{bad:?}");
        }
    }
}
