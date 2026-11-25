//! Directory handles used by wiki I/O. Names passed here are single components.
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::path::{Component, Path, PathBuf};
use super::{denied, invalid};

#[cfg(test)]
thread_local! {
    pub(crate) static LISTING_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[derive(Debug)]
pub(crate) struct Directory {
    pub path: PathBuf,
    file: File,
    // Windows operations are path based. Retaining every ancestor without
    // FILE_SHARE_DELETE prevents renaming/replacing the verified chain.
    #[cfg(windows)]
    ancestors: Vec<File>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind { File, Directory, Other }

pub(crate) enum Open { Read, Append, Lock, CreateNew }

fn component(name: &OsStr) -> io::Result<()> {
    let mut parts = Path::new(name).components();
    if !matches!(parts.next(), Some(Component::Normal(_))) || parts.next().is_some() {
        return Err(invalid("expected one path component"));
    }
    Ok(())
}

impl Directory {
    /// Only ancestors OUTSIDE the operator-selected wiki root are resolved.
    /// The root itself and every descendant are always opened no-follow.
    pub fn root(path: &Path) -> io::Result<Self> {
        let absolute = if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir()?.join(path) };
        let name = absolute.file_name().ok_or_else(|| invalid("missing wiki root name"))?;
        let parent = absolute.parent().ok_or_else(|| invalid("missing wiki root parent"))?.canonicalize()?;
        Self::open_absolute(&parent)?.child(name, false)
    }

    pub fn open_absolute(path: &Path) -> io::Result<Self> {
        // macOS exposes these OS-owned aliases outside wiki roots. Expand to
        // fixed system locations, never canonicalize an untrusted descendant.
        #[cfg(target_os = "macos")]
        let system_path = ["/var", "/tmp", "/etc"].iter().find_map(|prefix| {
            path.strip_prefix(prefix).ok().map(|suffix| Path::new("/private").join(prefix.trim_start_matches('/')).join(suffix))
        });
        #[cfg(target_os = "macos")]
        let path = system_path.as_deref().unwrap_or(path);
        let mut parts = path.components();
        #[cfg(unix)]
        let mut dir = {
            use std::os::unix::fs::OpenOptionsExt;
            if parts.next() != Some(Component::RootDir) { return Err(invalid("expected absolute directory")); }
            let file = std::fs::OpenOptions::new().read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC).open("/")?;
            Self { path: PathBuf::from("/"), file }
        };
        #[cfg(windows)]
        let mut dir = {
            let Some(Component::Prefix(prefix)) = parts.next() else { return Err(invalid("expected absolute directory")); };
            if parts.next() != Some(Component::RootDir) { return Err(invalid("expected rooted directory")); }
            let path = PathBuf::from(prefix.as_os_str()).join("\\");
            let file = windows::open_directory(&path)?;
            Self { path, file, ancestors: Vec::new() }
        };
        for part in parts {
            match part {
                Component::Normal(name) => dir = dir.child(name, false)?,
                Component::CurDir => {},
                _ => return Err(invalid("invalid directory component")),
            }
        }
        Ok(dir)
    }

    pub fn same(&self, other: &Self) -> io::Result<bool> {
        #[cfg(unix)] {
            use std::os::unix::fs::MetadataExt;
            let a = self.file.metadata()?;
            let b = other.file.metadata()?;
            Ok(a.dev() == b.dev() && a.ino() == b.ino())
        }
        #[cfg(windows)] { Ok(windows::identity(&self.file)? == windows::identity(&other.file)?) }
    }

    pub fn child(&self, name: &OsStr, create: bool) -> io::Result<Self> {
        component(name)?;
        #[cfg(unix)] {
            use std::os::fd::{AsRawFd, FromRawFd};
            let name_c = unix_name(name)?;
            if create {
                let result = unsafe { libc::mkdirat(self.file.as_raw_fd(), name_c.as_ptr(), 0o755) };
                if result < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() != io::ErrorKind::AlreadyExists { return Err(error); }
                }
            }
            let fd = unsafe { libc::openat(self.file.as_raw_fd(), name_c.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC) };
            if fd < 0 { return Err(io::Error::last_os_error()); }
            Ok(Self { path: self.path.join(name), file: unsafe { File::from_raw_fd(fd) } })
        }
        #[cfg(windows)] {
            self.verify_path()?;
            let path = self.path.join(name);
            if create {
                match std::fs::create_dir(&path) {
                    Ok(()) => {},
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {},
                    Err(e) => return Err(e),
                }
            }
            let file = windows::open_directory(&path)?;
            windows::verify_parent(&file, &self.file)?;
            self.verify_path()?;
            let mut ancestors = self.ancestors.iter().map(File::try_clone).collect::<io::Result<Vec<_>>>()?;
            ancestors.push(self.file.try_clone()?);
            Ok(Self { path, file, ancestors })
        }
    }

    pub fn descend(&self, relative: &Path, create: bool) -> io::Result<Self> {
        let mut dir = self.duplicate()?;
        for part in relative.components() {
            match part {
                Component::Normal(name) => dir = dir.child(name, create)?,
                Component::CurDir => {},
                _ => return Err(invalid("invalid relative directory")),
            }
        }
        Ok(dir)
    }

    fn duplicate(&self) -> io::Result<Self> {
        Ok(Self {
            path: self.path.clone(), file: self.file.try_clone()?,
            #[cfg(windows)]
            ancestors: self.ancestors.iter().map(File::try_clone).collect::<io::Result<Vec<_>>>()?,
        })
    }

    pub fn kind(&self, name: &OsStr) -> io::Result<Kind> {
        component(name)?;
        #[cfg(unix)] {
            use std::os::fd::AsRawFd;
            let name = unix_name(name)?;
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            if unsafe { libc::fstatat(self.file.as_raw_fd(), name.as_ptr(), stat.as_mut_ptr(), libc::AT_SYMLINK_NOFOLLOW) } < 0 {
                return Err(io::Error::last_os_error());
            }
            let mode = unsafe { stat.assume_init() }.st_mode & libc::S_IFMT;
            Ok(if mode == libc::S_IFREG { Kind::File } else if mode == libc::S_IFDIR { Kind::Directory } else { Kind::Other })
        }
        #[cfg(windows)] {
            self.verify_path()?;
            let meta = std::fs::symlink_metadata(self.path.join(name))?;
            self.verify_path()?;
            Ok(if super::is_link_metadata(&meta) { Kind::Other }
                else if meta.is_file() { Kind::File } else if meta.is_dir() { Kind::Directory } else { Kind::Other })
        }
    }

    pub fn open(&self, name: &OsStr, mode: Open) -> io::Result<File> {
        component(name)?;
        #[cfg(unix)]
        let file = {
            use std::os::fd::{AsRawFd, FromRawFd};
            let name = unix_name(name)?;
            let access = match mode {
                Open::Read => libc::O_RDONLY,
                Open::Append => libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND,
                Open::Lock => libc::O_WRONLY | libc::O_CREAT,
                Open::CreateNew => libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            };
            let fd = unsafe { libc::openat(self.file.as_raw_fd(), name.as_ptr(),
                access | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK, 0o600) };
            if fd < 0 { return Err(io::Error::last_os_error()); }
            unsafe { File::from_raw_fd(fd) }
        };
        #[cfg(windows)]
        let file = {
            use std::os::windows::fs::OpenOptionsExt;
            self.verify_path()?;
            let mut options = std::fs::OpenOptions::new();
            options.custom_flags(0x0020_0000 | 0x0200_0000).share_mode(3);
            match mode {
                Open::Read => { options.read(true); },
                // LockFileEx needs GENERIC_READ or GENERIC_WRITE; append-only
                // access lacks both. Keep append semantics and add GENERIC_READ.
                Open::Append => { options.read(true).append(true).create(true); },
                Open::Lock => { options.write(true).create(true); },
                Open::CreateNew => { options.write(true).create_new(true); },
            }
            let file = options.open(self.path.join(name))?;
            windows::verify_parent(&file, &self.file)?;
            self.verify_path()?;
            file
        };
        let meta = file.metadata()?;
        if super::is_link_metadata(&meta) || !meta.is_file() { return Err(denied("wiki file is not regular")); }
        Ok(file)
    }

    pub fn names(&self, cap: usize, checkpoint: &mut impl FnMut() -> io::Result<()>) -> io::Result<Vec<OsString>> {
        #[cfg(test)]
        LISTING_COUNT.with(|count| count.set(count.get() + 1));
        let mut names = Vec::new();
        #[cfg(unix)] {
            use std::os::fd::AsRawFd;
            use std::os::unix::ffi::OsStrExt;
            // openat(".") gives an independent directory offset (dup does not).
            let fd = unsafe { libc::openat(self.file.as_raw_fd(), c".".as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW) };
            if fd < 0 { return Err(io::Error::last_os_error()); }
            let stream = unsafe { libc::fdopendir(fd) };
            if stream.is_null() {
                let error = io::Error::last_os_error();
                unsafe { libc::close(fd); }
                return Err(error);
            }
            struct Stream(*mut libc::DIR);
            impl Drop for Stream { fn drop(&mut self) { unsafe { libc::closedir(self.0); } } }
            let stream = Stream(stream);
            loop {
                #[cfg(target_os = "linux")]
                unsafe { *libc::__errno_location() = 0; }
                #[cfg(target_os = "macos")]
                unsafe { *libc::__error() = 0; }
                let entry = unsafe { libc::readdir(stream.0) };
                if entry.is_null() {
                    let error = io::Error::last_os_error();
                    if error.raw_os_error() != Some(0) { return Err(error); }
                    break;
                }
                let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
                if name == b"." || name == b".." { continue; }
                checkpoint()?;
                if names.len() >= cap { return Err(denied(&format!("wiki directory entry limit exceeded (maximum {cap})"))); }
                names.push(OsStr::from_bytes(name).to_os_string());
            }
        }
        #[cfg(windows)] {
            self.verify_path()?;
            for entry in std::fs::read_dir(&self.path)? {
                checkpoint()?;
                if names.len() >= cap { return Err(denied(&format!("wiki directory entry limit exceeded (maximum {cap})"))); }
                names.push(entry?.file_name());
            }
            self.verify_path()?;
        }
        Ok(names)
    }

    pub fn remove(&self, name: &OsStr) -> io::Result<()> {
        component(name)?;
        #[cfg(unix)] {
            use std::os::fd::AsRawFd;
            let name = unix_name(name)?;
            if unsafe { libc::unlinkat(self.file.as_raw_fd(), name.as_ptr(), 0) } < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
        #[cfg(windows)] {
            self.verify_path()?;
            std::fs::remove_file(self.path.join(name))
        }
    }

    pub fn rename(&self, from: &OsStr, to: &OsStr) -> io::Result<()> {
        self.rename_to(from, self, to)
    }

    pub fn rename_to(&self, from: &OsStr, destination: &Self, to: &OsStr) -> io::Result<()> {
        component(from)?;
        component(to)?;
        #[cfg(unix)] {
            use std::os::fd::AsRawFd;
            let from = unix_name(from)?;
            let to = unix_name(to)?;
            if unsafe { libc::renameat(self.file.as_raw_fd(), from.as_ptr(), destination.file.as_raw_fd(), to.as_ptr()) } < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
        #[cfg(windows)] {
            // std::fs::rename is not handle-relative. Retain the no-delete-share
            // ancestor handles and recheck volume/file identity immediately
            // before rename. Unlike renameat, a path-based check-to-rename
            // interval remains; pinning relies on the filesystem honoring
            // Windows sharing locks. A final-name replacement can still race,
            // but rename replaces that entry rather than following it.
            self.verify_path()?;
            destination.verify_path()?;
            std::fs::rename(self.path.join(from), destination.path.join(to))
        }
    }

    #[cfg(windows)]
    fn verify_path(&self) -> io::Result<()> {
        let current = windows::open_directory(&self.path)?;
        if windows::identity(&current)? != windows::identity(&self.file)? {
            return Err(denied("wiki parent directory changed"));
        }
        Ok(())
    }
}

#[cfg(unix)]
fn unix_name(name: &OsStr) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(name.as_bytes()).map_err(|_| invalid("NUL in filename"))
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::ffi::OsStringExt;
    use std::ffi::c_void;

    #[repr(C)]
    #[derive(Default)]
    struct FileTime { low: u32, high: u32 }
    #[repr(C)]
    #[derive(Default)]
    struct FileInfo {
        attributes: u32, creation: FileTime, access: FileTime, write: FileTime,
        volume: u32, size_high: u32, size_low: u32, links: u32, id_high: u32, id_low: u32,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandle(handle: *mut c_void, info: *mut FileInfo) -> i32;
        fn GetFinalPathNameByHandleW(handle: *mut c_void, buffer: *mut u16, length: u32, flags: u32) -> u32;
    }

    pub fn open_directory(path: &Path) -> io::Result<File> {
        let file = std::fs::OpenOptions::new().read(true).share_mode(3)
            .custom_flags(0x0020_0000 | 0x0200_0000).open(path)?;
        let meta = file.metadata()?;
        if super::super::is_link_metadata(&meta) || !meta.is_dir() { return Err(denied("linked wiki directory")); }
        Ok(file)
    }

    pub fn identity(file: &File) -> io::Result<(u32, u32, u32)> {
        let mut info = FileInfo::default();
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((info.volume, info.id_high, info.id_low))
    }

    pub fn verify_parent(file: &File, expected: &File) -> io::Result<()> {
        let mut buffer = vec![0u16; 32768];
        let len = unsafe { GetFinalPathNameByHandleW(file.as_raw_handle(), buffer.as_mut_ptr(), buffer.len() as u32, 0) };
        if len == 0 { return Err(io::Error::last_os_error()); }
        if len as usize >= buffer.len() { return Err(denied("opened wiki path too long")); }
        let path = PathBuf::from(OsString::from_wide(&buffer[..len as usize]));
        let parent = open_directory(path.parent().ok_or_else(|| denied("opened wiki file has no parent"))?)?;
        if identity(&parent)? != identity(expected)? { return Err(denied("opened wiki parent identity changed")); }
        Ok(())
    }
}
