//! Unix home resolution and descriptor-relative state access.
//!
//! Resolve links only from trusted parents, then retain the directory identity.
//! All state opens are relative to that descriptor, never a re-resolved path.
use std::{collections::VecDeque, ffi::{CString, OsString}, fs::{File, Metadata, OpenOptions}, io,
    os::{fd::{AsRawFd, FromRawFd}, unix::{ffi::{OsStrExt, OsStringExt}, fs::{MetadataExt, OpenOptionsExt}}},
    path::{Component, Path, PathBuf}, sync::Arc};

#[derive(Clone, Debug)]
pub struct TrustedHome {
    directory: Arc<File>,
    source: PathBuf,
    path: PathBuf,
    identity: (u64, u64),
}

fn denied() -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, "unsafe or replaced home directory")
}

fn identity(m: &Metadata) -> (u64, u64) { (m.dev(), m.ino()) }

fn validate(m: &Metadata) -> io::Result<()> {
    // SAFETY: geteuid has no preconditions.
    if !m.is_dir() || m.uid() != unsafe { libc::geteuid() } || m.mode() & 0o022 != 0 {
        return Err(denied());
    }
    Ok(())
}

fn name(value: &std::ffi::OsStr) -> io::Result<CString> {
    CString::new(value.as_bytes()).map_err(|_| denied())
}

fn open_at(dir: &File, value: &std::ffi::OsStr, flags: libc::c_int) -> io::Result<File> {
    let value = name(value)?;
    // SAFETY: dir is live and value is NUL terminated. The result is owned once.
    let fd = unsafe { libc::openat(dir.as_raw_fd(), value.as_ptr(),
        flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK, 0o600 as libc::c_uint) };
    if fd < 0 { return Err(io::Error::last_os_error()); }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn parts(path: &Path) -> VecDeque<OsString> {
    path.components().filter_map(|c| match c {
        Component::Normal(s) => Some(s.to_owned()),
        Component::ParentDir => Some("..".into()),
        _ => None,
    }).collect()
}

impl TrustedHome {
    pub fn resolve(path: &Path) -> io::Result<Self> {
        let source = if path.is_absolute() { path.to_owned() } else { std::env::current_dir()?.join(path) };
        let mut pending = parts(&source);
        let root = || OpenOptions::new().read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC).open("/");
        let mut dir = root()?;
        let mut resolved = PathBuf::from("/");
        let mut links = 0;
        while let Some(part) = pending.pop_front() {
            let component = name(&part)?;
            let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
            // SAFETY: live descriptor, valid C string and writable stat storage.
            if unsafe { libc::fstatat(dir.as_raw_fd(), component.as_ptr(), st.as_mut_ptr(), libc::AT_SYMLINK_NOFOLLOW) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let st = unsafe { st.assume_init() };
            if st.st_mode & libc::S_IFMT == libc::S_IFLNK {
                links += 1;
                if links > 40 { return Err(denied()); }
                let parent = dir.metadata()?;
                let uid = unsafe { libc::geteuid() };
                // Root-owned system links (/var, /tmp on macOS) are allowed
                // only in root-owned, non-writable parents.
                if parent.mode() & 0o022 != 0
                    || !(parent.uid() == uid || parent.uid() == 0)
                    || !(st.st_uid == uid || (st.st_uid == 0 && parent.uid() == 0)) {
                    return Err(denied());
                }
                let mut target = vec![0u8; 65536];
                // SAFETY: target is writable for its full length.
                let count = unsafe { libc::readlinkat(dir.as_raw_fd(), component.as_ptr(), target.as_mut_ptr().cast(), target.len()) };
                if count < 0 { return Err(io::Error::last_os_error()); }
                if count as usize == target.len() { return Err(denied()); }
                target.truncate(count as usize);
                let mut after = std::mem::MaybeUninit::<libc::stat>::uninit();
                if unsafe { libc::fstatat(dir.as_raw_fd(), component.as_ptr(), after.as_mut_ptr(), libc::AT_SYMLINK_NOFOLLOW) } != 0 {
                    return Err(io::Error::last_os_error());
                }
                let after = unsafe { after.assume_init() };
                if (st.st_dev, st.st_ino, st.st_uid) != (after.st_dev, after.st_ino, after.st_uid) { return Err(denied()); }
                let target = PathBuf::from(OsString::from_vec(target));
                if target.is_absolute() { dir = root()?; resolved = PathBuf::from("/"); }
                let mut next = parts(&target);
                next.append(&mut pending);
                pending = next;
            } else {
                let next = open_at(&dir, &part, libc::O_RDONLY | libc::O_DIRECTORY)?;
                let metadata = next.metadata()?;
                if (metadata.dev(), metadata.ino()) != (st.st_dev as u64, st.st_ino as u64) { return Err(denied()); }
                if part == ".." { resolved.pop(); } else { resolved.push(part); }
                dir = next;
            }
        }
        let metadata = dir.metadata()?;
        validate(&metadata)?;
        Ok(Self { directory: Arc::new(dir), source, path: resolved, identity: identity(&metadata) })
    }

    pub fn path(&self) -> &Path { &self.path }
    pub fn directory(&self) -> &File { &self.directory }
    pub fn same_directory(&self, other: &Self) -> bool { self.identity == other.identity }

    /// Recheck the spelling and permissions, but continue using the original FD.
    pub fn verify(&self) -> io::Result<()> {
        validate(&self.directory.metadata()?)?;
        let current = Self::resolve(&self.source)?;
        if !self.same_directory(&current) || self.path != current.path { return Err(denied()); }
        Ok(())
    }

    pub fn child_directory(&self, child: &str, create: bool) -> io::Result<Self> {
        self.verify()?;
        let component = Self::component(child)?;
        if create {
            if unsafe { libc::mkdirat(self.directory.as_raw_fd(), component.as_ptr(), 0o700) } != 0 {
                let e = io::Error::last_os_error();
                if e.kind() != io::ErrorKind::AlreadyExists { return Err(e); }
            }
        }
        let dir = open_at(&self.directory, std::ffi::OsStr::new(child), libc::O_RDONLY | libc::O_DIRECTORY)?;
        let m = dir.metadata()?;
        validate(&m)?;
        Ok(Self { directory: Arc::new(dir), source: self.source.join(child), path: self.path.join(child), identity: identity(&m) })
    }

    fn component(child: &str) -> io::Result<CString> {
        if child.is_empty() || child == "." || child == ".." || child.contains('/') { return Err(denied()); }
        name(std::ffi::OsStr::new(child))
    }

    pub fn open_file(&self, child: &str, flags: libc::c_int) -> io::Result<File> {
        Self::component(child)?;
        self.verify()?;
        let file = open_at(&self.directory, std::ffi::OsStr::new(child), flags)?;
        let m = file.metadata()?;
        if !m.is_file() || m.uid() != unsafe { libc::geteuid() } || m.mode() & 0o022 != 0 || m.nlink() != 1 {
            return Err(denied());
        }
        Ok(file)
    }

    pub fn lock_file(&self, child: &str) -> io::Result<crate::owner_lock::OwnerLock> {
        let file = self.open_file(child, libc::O_RDWR | libc::O_CREAT)?;
        fs2::FileExt::lock_exclusive(&file)?;
        self.verify()?;
        Ok(crate::owner_lock::OwnerLock::new(file))
    }

    pub fn remove_file(&self, child: &str) -> io::Result<()> {
        self.verify()?;
        let child = Self::component(child)?;
        if unsafe { libc::unlinkat(self.directory.as_raw_fd(), child.as_ptr(), 0) } == 0 { return Ok(()); }
        let e = io::Error::last_os_error();
        if e.kind() == io::ErrorKind::NotFound { Ok(()) } else { Err(e) }
    }

    pub fn rename_file(&self, from: &str, to: &str) -> io::Result<()> {
        self.verify()?;
        let from = Self::component(from)?;
        let to = Self::component(to)?;
        if unsafe { libc::renameat(self.directory.as_raw_fd(), from.as_ptr(), self.directory.as_raw_fd(), to.as_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        self.directory.sync_all()
    }
}
