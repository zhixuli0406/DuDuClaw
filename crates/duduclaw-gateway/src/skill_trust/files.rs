//! Descriptor-relative reads: directory replacements never redirect a read.
use std::{fs::File, io::{self, Read}, path::{Path, PathBuf}};
use duduclaw_core::fs_safety::is_link_metadata;

pub(super) fn safe_relative(value: &str) -> bool {
    !value.is_empty() && !value.contains(['\\', ':', '\r', '\n', '\0'])
        && value.split('/').all(|s| !s.is_empty() && s != "." && s != ".."
            && !s.ends_with(['.', ' ']) && !s.chars().any(char::is_control))
        && Path::new(value).components().all(|c| matches!(c, std::path::Component::Normal(_)))
}

pub(super) fn segment(value: &str) -> bool {
    safe_relative(value) && !value.contains('/')
}

fn check_entry_count(count: usize) -> io::Result<()> {
    if count >= 10_000 { return Err(denied()); }
    Ok(())
}

#[test]
fn entry_limit_refuses_before_allocating_entry_10001() {
    assert!(check_entry_count(9_999).is_ok());
    assert!(check_entry_count(10_000).is_err());
}

fn denied() -> io::Error { io::Error::other("unsafe skill evidence") }

pub(super) struct Directory {
    path: PathBuf,
    // On Windows, retained handles deny FILE_SHARE_DELETE on the entire chain.
    // On Unix, the final descriptor anchors every child open and enumeration.
    handles: Vec<File>,
}

impl Directory {
    // Resolve only the home root. Evidence descendants still use no-follow opens.
    pub(super) fn open_home(home: &Path) -> io::Result<Self> {
        Self::open(&std::fs::canonicalize(home)?)
    }

    pub(super) fn open(home: &Path) -> io::Result<Self> {
        let meta = std::fs::symlink_metadata(home)?;
        if is_link_metadata(&meta) || !meta.is_dir() { return Err(denied()); }
        let mut opts = std::fs::OpenOptions::new();
        opts.read(true);
        #[cfg(unix)] {
            use std::os::unix::fs::OpenOptionsExt;
            opts.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        #[cfg(windows)] {
            use std::os::windows::fs::OpenOptionsExt;
            opts.custom_flags(0x0200_0000 | 0x0020_0000).share_mode(0x1 | 0x2);
        }
        let file = opts.open(home)?;
        let meta = file.metadata()?;
        if is_link_metadata(&meta) || !meta.is_dir() { return Err(denied()); }
        Ok(Self { path: home.to_owned(), handles: vec![file] })
    }

    pub(super) fn child(&self, name: &str) -> io::Result<Self> {
        if !segment(name) { return Err(denied()); }
        #[cfg(unix)]
        let file = self.open_at(name, true)?;
        #[cfg(not(unix))]
        let file = Self::open(&self.path.join(name))?.handles.remove(0);
        let mut handles = self.handles.iter().map(File::try_clone).collect::<io::Result<Vec<_>>>()?;
        handles.push(file);
        Ok(Self { path: self.path.join(name), handles })
    }

    pub(super) fn descend(&self, relative: &str) -> io::Result<Self> {
        if !safe_relative(relative) { return Err(denied()); }
        let mut parts = relative.split('/');
        let mut dir = self.child(parts.next().ok_or_else(denied)?)?;
        for part in parts { dir = dir.child(part)?; }
        Ok(dir)
    }

    #[cfg(unix)]
    fn open_at(&self, name: &str, directory: bool) -> io::Result<File> {
        use std::os::fd::{AsRawFd, FromRawFd};
        let name = std::ffi::CString::new(name).map_err(|_| denied())?;
        let parent = self.handles.last().ok_or_else(denied)?.as_raw_fd();
        let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: parent is retained; name and output storage are valid.
        if unsafe { libc::fstatat(parent, name.as_ptr(), metadata.as_mut_ptr(), libc::AT_SYMLINK_NOFOLLOW) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let metadata = unsafe { metadata.assume_init() };
        let kind = metadata.st_mode & libc::S_IFMT;
        if kind != if directory { libc::S_IFDIR } else { libc::S_IFREG } { return Err(denied()); }
        let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK
            | if directory { libc::O_DIRECTORY } else { 0 };
        // SAFETY: live parent descriptor and valid NUL-terminated child name.
        let fd = unsafe { libc::openat(parent, name.as_ptr(), flags) };
        if fd < 0 { return Err(io::Error::last_os_error()); }
        let file = unsafe { File::from_raw_fd(fd) };
        let opened = file.metadata()?;
        use std::os::unix::fs::MetadataExt;
        if opened.dev() != metadata.st_dev as u64 || opened.ino() != metadata.st_ino as u64
            || (directory && !opened.is_dir()) || (!directory && !opened.is_file())
        { return Err(denied()); }
        Ok(file)
    }

    #[cfg(unix)]
    pub(super) fn sync(&self) -> io::Result<()> {
        self.handles.last().ok_or_else(denied)?.sync_all()
    }

    pub(super) fn read(&self, name: &str, limit: u64) -> io::Result<Vec<u8>> {
        if !segment(name) { return Err(denied()); }
        #[cfg(unix)]
        let file = self.open_at(name, false)?;
        #[cfg(not(unix))]
        let file = duduclaw_core::fs_safety::open_regular_file_nofollow(&self.path.join(name))?;
        if file.metadata()?.len() > limit { return Err(denied()); }
        let mut bytes = Vec::new();
        file.take(limit + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > limit { return Err(denied()); }
        Ok(bytes)
    }

    pub(super) fn names(&self) -> io::Result<Vec<String>> {
        #[cfg(unix)] {
            use std::os::fd::IntoRawFd;
            // A new descriptor has its own enumeration offset (dup does not).
            let fd = self.open_at(".", true)?.into_raw_fd();
            let ptr = unsafe { libc::fdopendir(fd) };
            if ptr.is_null() {
                let error = io::Error::last_os_error();
                unsafe { libc::close(fd); }
                return Err(error);
            }
            struct Entries(*mut libc::DIR);
            impl Drop for Entries {
                fn drop(&mut self) { unsafe { libc::closedir(self.0); } }
            }
            let entries = Entries(ptr);
            let mut names = Vec::new();
            loop {
                nix::errno::Errno::clear();
                let entry = unsafe { libc::readdir(entries.0) };
                if entry.is_null() {
                    let error = nix::errno::Errno::last_raw();
                    if error != 0 { return Err(io::Error::from_raw_os_error(error)); }
                    break;
                }
                // SAFETY: readdir supplies a NUL-terminated name until the next call.
                let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) }
                    .to_str().map_err(|_| denied())?;
                if name == "." || name == ".." { continue; }
                check_entry_count(names.len())?;
                if !segment(name) { return Err(denied()); }
                names.push(name.to_owned());
            }
            Ok(names)
        }
        #[cfg(not(unix))] {
            let mut names = Vec::new();
            for entry in std::fs::read_dir(&self.path)? {
                check_entry_count(names.len())?;
                let name = entry?.file_name().into_string().map_err(|_| denied())?;
                if !segment(&name) { return Err(denied()); }
                names.push(name);
            }
            Ok(names)
        }
    }
}

pub(super) fn snapshot(root: &Directory) -> io::Result<std::collections::BTreeMap<String, Vec<u8>>> {
    fn walk(dir: &Directory, prefix: &str, depth: usize, total: &mut usize,
        count: &mut usize, out: &mut std::collections::BTreeMap<String, Vec<u8>>) -> io::Result<()> {
        if depth > 64 { return Err(denied()); }
        for name in dir.names()? {
            *count += 1;
            if *count > 10_000 { return Err(denied()); }
            let relative = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
            // Both opens reject links and special files; an inaccessible directory
            // cannot fall through to being read as a file.
            match dir.child(&name) {
                Ok(child) => walk(&child, &relative, depth + 1, total, count, out)?,
                Err(_) => {
                    let bytes = dir.read(&name, 64 * 1024 * 1024)?;
                    *total = total.checked_add(bytes.len()).ok_or_else(denied)?;
                    if *total > 64 * 1024 * 1024 { return Err(denied()); }
                    out.insert(relative, bytes);
                }
            }
        }
        Ok(())
    }
    let mut files = std::collections::BTreeMap::new();
    walk(root, "", 0, &mut 0, &mut 0, &mut files)?;
    Ok(files)
}
