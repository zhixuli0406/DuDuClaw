//! Descriptor-relative, no-follow access for skill layers and office execution.
use std::fs::{File, Metadata};
use std::io;
use std::path::{Component, Path};

fn refused(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

#[cfg(unix)]
mod imp {
    use super::*;
    use std::ffi::{CStr, CString};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::MetadataExt;

    pub struct SafeDir(File);

    fn name(value: &str) -> io::Result<CString> {
        if value.is_empty() || value == "." || value == ".." || value.contains(['/', '\\']) {
            return Err(refused("invalid path component"));
        }
        CString::new(value).map_err(|_| refused("NUL in path"))
    }

    fn errno_pointer() -> io::Result<*mut libc::c_int> {
        #[cfg(target_os = "linux")]
        { Ok(unsafe { libc::__errno_location() }) }
        #[cfg(target_vendor = "apple")]
        { Ok(unsafe { libc::__error() }) }
        #[cfg(target_os = "android")]
        { Ok(unsafe { libc::__errno() }) }
        #[cfg(not(any(target_os = "linux", target_vendor = "apple", target_os = "android")))]
        { Err(io::Error::new(io::ErrorKind::Unsupported, "directory enumeration errno unavailable")) }
    }

    #[cfg(target_os = "macos")]
    fn macos_system_path(path: &Path) -> io::Result<std::borrow::Cow<'_, Path>> {
        for (alias, target) in [("/var", "/private/var"), ("/tmp", "/private/tmp")] {
            if let Ok(tail) = path.strip_prefix(alias) {
                let metadata = std::fs::symlink_metadata(alias)?;
                if metadata.file_type().is_symlink() && metadata.uid() == 0
                    && Path::new("/").join(std::fs::read_link(alias)?) == Path::new(target) {
                    return Ok(std::borrow::Cow::Owned(Path::new(target).join(tail)));
                }
                break;
            }
        }
        Ok(std::borrow::Cow::Borrowed(path))
    }

    impl SafeDir {
        pub fn open(path: &Path) -> io::Result<Self> {
            if !path.is_absolute() {
                return Err(refused("absolute directory required"));
            }
            // macOS exposes system directories through root-owned aliases.
            // Resolve only that fixed prefix, never an untrusted layer or
            // ancestor. The target and the entire suffix still use O_NOFOLLOW.
            #[cfg(target_os = "macos")]
            let system_path = macos_system_path(path)?;
            #[cfg(target_os = "macos")]
            let path = system_path.as_ref();
            let mut dir = Self(File::open("/")?);
            for component in path.components() {
                match component {
                    Component::RootDir => {}
                    Component::Normal(part) => {
                        dir = dir.child(part.to_str().ok_or_else(|| refused("non-UTF8 path"))?)?;
                    }
                    _ => return Err(refused("non-normal directory component")),
                }
            }
            Ok(dir)
        }

        fn open_file(&self, value: &str, flags: i32, mode: u32) -> io::Result<File> {
            let value = name(value)?;
            // SAFETY: a valid directory fd and a NUL-terminated single component.
            let fd = unsafe { libc::openat(self.0.as_raw_fd(), value.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK, mode as libc::c_uint) };
            if fd < 0 { return Err(io::Error::last_os_error()); }
            // SAFETY: openat returned a new, uniquely owned descriptor.
            Ok(unsafe { File::from_raw_fd(fd) })
        }

        pub fn child(&self, value: &str) -> io::Result<Self> {
            // lstat semantics also provide an explicit refusal before the atomic open.
            let value_c = name(value)?;
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            let result = unsafe { libc::fstatat(self.0.as_raw_fd(), value_c.as_ptr(), stat.as_mut_ptr(), libc::AT_SYMLINK_NOFOLLOW) };
            if result < 0 { return Err(io::Error::last_os_error()); }
            let stat = unsafe { stat.assume_init() };
            if stat.st_mode & libc::S_IFMT != libc::S_IFDIR {
                return Err(refused("directory is a symlink or non-directory"));
            }
            Ok(Self(self.open_file(value, libc::O_RDONLY | libc::O_DIRECTORY, 0)?))
        }

        pub fn mkdir_child(&self, value: &str, mode: u32) -> io::Result<Self> {
            let value_c = name(value)?;
            let result = unsafe { libc::mkdirat(self.0.as_raw_fd(), value_c.as_ptr(), mode as libc::mode_t) };
            if result < 0 && io::Error::last_os_error().kind() != io::ErrorKind::AlreadyExists {
                return Err(io::Error::last_os_error());
            }
            self.child(value)
        }

        pub fn read_file(&self, value: &str) -> io::Result<File> {
            let file = self.open_file(value, libc::O_RDONLY, 0)?;
            let metadata = file.metadata()?;
            if !metadata.is_file() || metadata.nlink() != 1 {
                return Err(refused("regular single-link file required"));
            }
            Ok(file)
        }

        pub fn create_new(&self, value: &str, mode: u32) -> io::Result<File> {
            self.open_file(value, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL, mode)
        }

        pub fn metadata(&self) -> io::Result<Metadata> { self.0.metadata() }

        /// Restore directory traversal for disposal of an untrusted workspace.
        /// The caller retains the root handle from before launching the parser.
        /// Never chmod through a symlink, including during a directory swap.
        pub(crate) fn restore_tree_permissions(&self) -> io::Result<()> {
            use std::os::unix::fs::PermissionsExt;
            self.0.set_permissions(std::fs::Permissions::from_mode(0o700))?;
            for entry in self.list()? {
                let entry_c = name(&entry)?;
                let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
                if unsafe { libc::fstatat(self.0.as_raw_fd(), entry_c.as_ptr(),
                    stat.as_mut_ptr(), libc::AT_SYMLINK_NOFOLLOW) } < 0 {
                    return Err(io::Error::last_os_error());
                }
                if unsafe { stat.assume_init() }.st_mode & libc::S_IFMT != libc::S_IFDIR {
                    continue;
                }
                if unsafe { libc::fchmodat(self.0.as_raw_fd(), entry_c.as_ptr(),
                    0o700, libc::AT_SYMLINK_NOFOLLOW) } < 0 {
                    return Err(io::Error::last_os_error());
                }
                self.child(&entry)?.restore_tree_permissions()?;
            }
            Ok(())
        }

        /// Publish a completed file from a private source directory. Neither
        /// name is followed and an existing destination is never replaced.
        pub fn publish_new(&self, source: &Self, source_name: &str, target_name: &str) -> io::Result<()> {
            let from = name(source_name)?;
            let to = name(target_name)?;
            #[cfg(target_os = "linux")]
            let result = unsafe { libc::syscall(libc::SYS_renameat2,
                source.0.as_raw_fd(), from.as_ptr(), self.0.as_raw_fd(), to.as_ptr(), libc::RENAME_NOREPLACE) };
            #[cfg(target_os = "macos")]
            let result = unsafe { libc::renameatx_np(source.0.as_raw_fd(), from.as_ptr(),
                self.0.as_raw_fd(), to.as_ptr(), libc::RENAME_EXCL) };
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            return Err(io::Error::new(io::ErrorKind::Unsupported, "atomic exclusive publish unavailable"));
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            if result < 0 { return Err(io::Error::last_os_error()); }
            self.0.sync_all()
        }

        pub fn list(&self) -> io::Result<Vec<String>> {
            // A separate open file description keeps readdir's offset private.
            let dot = CString::new(".").unwrap();
            let fd = unsafe { libc::openat(self.0.as_raw_fd(), dot.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) };
            if fd < 0 { return Err(io::Error::last_os_error()); }
            let stream = unsafe { libc::fdopendir(fd) };
            if stream.is_null() {
                let error = io::Error::last_os_error();
                unsafe { libc::close(fd); }
                return Err(error);
            }
            let mut names = Vec::new();
            let result = (|| -> io::Result<()> {
                let errno = errno_pointer()?;
                loop {
                    unsafe { *errno = 0; }
                    let entry = unsafe { libc::readdir(stream) };
                    if entry.is_null() {
                        let code = unsafe { *errno };
                        if code != 0 { return Err(io::Error::from_raw_os_error(code)); }
                        break;
                    }
                    let value = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
                    let value = value.to_str().map_err(|_| io::Error::new(
                        io::ErrorKind::InvalidData, "non-UTF8 directory entry"))?;
                    if value != "." && value != ".." {
                        if names.len() >= 4096 { return Err(io::Error::other("directory entry limit exceeded")); }
                        names.push(value.to_string());
                    }
                }
                Ok(())
            })();
            unsafe { libc::closedir(stream); }
            result?;
            names.sort();
            Ok(names)
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use std::path::PathBuf;
    use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
    use windows_sys::Wdk::Storage::FileSystem::{NtCreateFile, FILE_CREATE, FILE_OPEN,
        FILE_OPEN_IF, FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE,
        FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT};
    use windows_sys::Win32::Foundation::{RtlNtStatusToDosError, UNICODE_STRING, ERROR_NO_MORE_FILES};
    use windows_sys::Win32::Storage::FileSystem::{GetFileInformationByHandle,
        GetFileInformationByHandleEx, BY_HANDLE_FILE_INFORMATION, FILE_ID_BOTH_DIR_INFO,
        FileIdBothDirectoryInfo, FileIdBothDirectoryRestartInfo, FILE_GENERIC_READ, FILE_GENERIC_WRITE};
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

    // Every child access is a single-component NtCreateFile relative to an
    // already opened directory handle. Enumeration also uses that handle;
    // pathname checks or share flags are not the reparse-race boundary.
    pub struct SafeDir(File);
    fn check_name(value: &str) -> io::Result<()> {
        if value.is_empty() || value == "." || value == ".."
            || value.contains(['/', '\\', ':', '\0']) || value.ends_with(['.', ' ']) {
            return Err(refused("invalid Windows path component"));
        }
        Ok(())
    }
    fn real_file(file: &File, directory: bool) -> io::Result<()> {
        let metadata = file.metadata()?;
        if metadata.file_attributes() & 0x400 != 0 || metadata.is_dir() != directory {
            return Err(refused("reparse point or wrong file type"));
        }
        if !directory {
            if !metadata.is_file() { return Err(refused("regular file required")); }
            let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
            let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) };
            if ok == 0 { return Err(io::Error::last_os_error()); }
            if unsafe { info.assume_init() }.nNumberOfLinks != 1 {
                return Err(refused("single-link file required"));
            }
        }
        Ok(())
    }
    impl SafeDir {
        pub fn open(path: &Path) -> io::Result<Self> {
            if !path.is_absolute() { return Err(refused("absolute directory required")); }
            let mut prefix = PathBuf::new();
            let mut result: Option<Self> = None;
            for component in path.components() {
                match component {
                    Component::Prefix(value) => prefix.push(value.as_os_str()),
                    Component::RootDir => {
                        prefix.push(component.as_os_str());
                        let file = std::fs::OpenOptions::new().read(true).share_mode(1)
                            .custom_flags(0x00200000 | 0x02000000).open(&prefix)?;
                        real_file(&file, true)?;
                        result = Some(Self(file));
                    }
                    Component::Normal(value) => {
                        result = Some(result.ok_or_else(|| refused("missing root"))?
                            .child(value.to_str().ok_or_else(|| refused("non-UTF8 path"))?)?);
                    }
                    _ => return Err(refused("non-normal directory component")),
                }
            }
            result.ok_or_else(|| refused("missing root"))
        }
        fn relative(&self, value: &str, directory: bool, disposition: u32, write: bool) -> io::Result<File> {
            check_name(value)?;
            let mut wide: Vec<u16> = value.encode_utf16().collect();
            let length = u16::try_from(wide.len() * 2).map_err(|_| refused("name too long"))?;
            let name = UNICODE_STRING { Length: length, MaximumLength: length, Buffer: wide.as_mut_ptr() };
            let attributes = OBJECT_ATTRIBUTES {
                Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
                RootDirectory: self.0.as_raw_handle(), ObjectName: &name,
                Attributes: 0x40 | 0x1000, // OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE
                SecurityDescriptor: std::ptr::null(), SecurityQualityOfService: std::ptr::null(),
            };
            let mut handle = std::ptr::null_mut();
            let mut status_block = std::mem::MaybeUninit::<IO_STATUS_BLOCK>::zeroed();
            let status = unsafe { NtCreateFile(&mut handle,
                if write { FILE_GENERIC_WRITE } else { FILE_GENERIC_READ },
                &attributes, status_block.as_mut_ptr(), std::ptr::null(), 0, 1,
                disposition, FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT
                    | if directory { FILE_DIRECTORY_FILE } else { FILE_NON_DIRECTORY_FILE },
                std::ptr::null(), 0) };
            if status < 0 {
                return Err(io::Error::from_raw_os_error(unsafe { RtlNtStatusToDosError(status) } as i32));
            }
            // SAFETY: NtCreateFile returned a uniquely owned successful handle.
            let file = unsafe { File::from_raw_handle(handle) };
            real_file(&file, directory)?;
            Ok(file)
        }
        pub fn child(&self, value: &str) -> io::Result<Self> {
            Ok(Self(self.relative(value, true, FILE_OPEN, false)?))
        }
        pub fn mkdir_child(&self, value: &str, _: u32) -> io::Result<Self> {
            Ok(Self(self.relative(value, true, FILE_OPEN_IF, false)?))
        }
        pub fn read_file(&self, value: &str) -> io::Result<File> {
            self.relative(value, false, FILE_OPEN, false)
        }
        pub fn create_new(&self, value: &str, _: u32) -> io::Result<File> {
            self.relative(value, false, FILE_CREATE, true)
        }
        pub fn metadata(&self) -> io::Result<Metadata> { self.0.metadata() }
        pub fn list(&self) -> io::Result<Vec<String>> {
            // u64 storage provides the native structure's required alignment.
            let mut storage = vec![0u64; 8192];
            let bytes_len = storage.len() * 8;
            let mut names = Vec::new();
            let mut first = true;
            loop {
                storage.fill(0);
                let ok = unsafe { GetFileInformationByHandleEx(self.0.as_raw_handle(),
                    if first { FileIdBothDirectoryRestartInfo } else { FileIdBothDirectoryInfo },
                    storage.as_mut_ptr().cast(), bytes_len as u32) };
                first = false;
                if ok == 0 {
                    let error = io::Error::last_os_error();
                    if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) { break; }
                    return Err(error);
                }
                let mut offset = 0;
                loop {
                    let header = std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, FileName);
                    if offset + std::mem::size_of::<FILE_ID_BOTH_DIR_INFO>() > bytes_len {
                        return Err(io::Error::other("invalid directory enumeration"));
                    }
                    let entry = unsafe { &*(storage.as_ptr().cast::<u8>().add(offset).cast::<FILE_ID_BOTH_DIR_INFO>()) };
                    let length = entry.FileNameLength as usize;
                    if length % 2 != 0 || offset + header + length > bytes_len {
                        return Err(io::Error::other("invalid directory name length"));
                    }
                    let wide = unsafe { std::slice::from_raw_parts(
                        storage.as_ptr().cast::<u8>().add(offset + header).cast::<u16>(), length / 2) };
                    let value = String::from_utf16(wide).map_err(|_| io::Error::new(
                        io::ErrorKind::InvalidData, "non-Unicode directory entry"))?;
                    if value != "." && value != ".." {
                        if names.len() >= 4096 { return Err(io::Error::other("directory entry limit exceeded")); }
                        names.push(value);
                    }
                    let next = entry.NextEntryOffset as usize;
                    if next == 0 { break; }
                    if next < header + length || next % 8 != 0 || offset + next >= bytes_len {
                        return Err(io::Error::other("invalid directory entry offset"));
                    }
                    offset += next;
                }
            }
            names.sort();
            Ok(names)
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use super::*;
    pub struct SafeDir;
    fn unsupported<T>() -> io::Result<T> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "secure no-follow directory access unavailable"))
    }
    impl SafeDir {
        pub fn open(_: &Path) -> io::Result<Self> { unsupported() }
        pub fn child(&self, _: &str) -> io::Result<Self> { unsupported() }
        pub fn mkdir_child(&self, _: &str, _: u32) -> io::Result<Self> { unsupported() }
        pub fn read_file(&self, _: &str) -> io::Result<File> { unsupported() }
        pub fn create_new(&self, _: &str, _: u32) -> io::Result<File> { unsupported() }
        pub fn metadata(&self) -> io::Result<Metadata> { unsupported() }
        pub fn list(&self) -> io::Result<Vec<String>> { unsupported() }
    }
}
pub use imp::SafeDir;

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_system_aliases_keep_the_suffix_nofollow() {
        use std::os::unix::fs::MetadataExt;
        // The check/readlink TOCTOU is harmless: the walk opens the fixed /private/... target from / with O_NOFOLLOW, never the alias.
        for (alias, target) in [("/var", "/private/var"), ("/tmp", "/private/tmp")] {
            let actual = SafeDir::open(Path::new(alias)).unwrap().metadata().unwrap();
            let expected = SafeDir::open(Path::new(target)).unwrap().metadata().unwrap();
            assert_eq!((actual.dev(), actual.ino()), (expected.dev(), expected.ino()));
        }
        assert!(SafeDir::open(Path::new("/etc")).is_err());
        let unknown_error = match SafeDir::open(Path::new("/variable-does-not-exist")) {
            Ok(_) => panic!("unknown path unexpectedly opened"),
            Err(error) => error,
        };
        assert_eq!(unknown_error.kind(), io::ErrorKind::NotFound);
        assert!(SafeDir::open(Path::new("/var/../tmp")).is_err());
        assert!(SafeDir::open(Path::new("/tmp/..")).is_err());
        // Exercise both temp-dir spellings without canonicalizing the fixture.
        for parent in [std::env::temp_dir(), std::path::PathBuf::from("/tmp")] {
            let temp = tempfile::tempdir_in(parent).unwrap();
            let root = temp.path();
            let dir = SafeDir::open(root).unwrap().mkdir_child("real", 0o700).unwrap();
            dir.mkdir_child("nested", 0o700).unwrap();
            dir.create_new("SKILL.md", 0o600).unwrap();
            assert!(SafeDir::open(&root.join("real")).unwrap().read_file("SKILL.md").is_ok());
            symlink("real", root.join("skills")).unwrap();
            assert!(SafeDir::open(&root.join("skills")).is_err());
            assert!(SafeDir::open(&root.join("skills/nested")).is_err());
            symlink("real/SKILL.md", root.join("linked.md")).unwrap();
            assert!(SafeDir::open(root).unwrap().read_file("linked.md").is_err());
        }
    }

    #[test]
    fn layer_roots_and_ancestors_refuse_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("real/pdf")).unwrap();
        symlink("real", root.join("skills")).unwrap();
        assert!(SafeDir::open(&root.join("skills")).is_err());
        assert!(SafeDir::open(&root.join("skills/pdf")).is_err());
    }

    #[test]
    fn open_handle_cannot_be_redirected_by_directory_swap() {
        use std::io::Read;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("skills")).unwrap();
        std::fs::create_dir(root.join("outside")).unwrap();
        std::fs::write(root.join("skills/SKILL.md"), "original").unwrap();
        std::fs::write(root.join("outside/SKILL.md"), "attacker").unwrap();
        let dir = SafeDir::open(&root.join("skills")).unwrap();
        std::fs::rename(root.join("skills"), root.join("old")).unwrap();
        symlink("outside", root.join("skills")).unwrap();
        let mut content = String::new();
        dir.read_file("SKILL.md").unwrap().read_to_string(&mut content).unwrap();
        assert_eq!(content, "original");
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::os::windows::fs::OpenOptionsExt;

    #[test]
    fn directory_handle_refuses_concurrent_writers_and_child_links() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("layer")).unwrap();
        let dir = SafeDir::open(&root.join("layer")).unwrap();
        // Share denial is defense in depth. Reparse-race safety comes from
        // NtCreateFile's RootDirectory and single-component object name.
        assert!(std::fs::OpenOptions::new().write(true).share_mode(7)
            .custom_flags(0x00200000 | 0x02000000).open(root.join("layer")).is_err());
        std::fs::write(root.join("layer/plain"), "content").unwrap();
        assert!(dir.read_file("plain").is_ok());
        drop(dir);
        let writer = std::fs::OpenOptions::new().write(true).share_mode(7)
            .custom_flags(0x00200000 | 0x02000000).open(root.join("layer")).unwrap();
        assert!(SafeDir::open(&root.join("layer")).is_err(),
            "an existing mutation handle must also prevent acquiring the safe chain");
        drop(writer);
    }
}
