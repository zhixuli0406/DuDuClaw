//! Pinned, no-follow operator diagnostics. Never a credential destination.

use std::io;
use std::path::Path;

#[cfg(windows)]
const FILENAME: &str = "identity-endpoint";
const MAX_BYTES: u64 = 4096;

fn denied() -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, "Unsafe identity discovery path")
}

#[cfg(unix)]
mod platform {
    use super::*;
    use std::ffi::CString;
    use std::fs::{File, Metadata, OpenOptions};
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

    pub struct DiscoveryDirectory(File);

    fn private(metadata: &Metadata, directory: bool) -> io::Result<()> {
        // SAFETY: getuid has no pointer arguments or preconditions.
        if metadata.uid() != unsafe { libc::getuid() } || metadata.mode() & 0o077 != 0
            || if directory { !metadata.is_dir() } else { !metadata.is_file() } {
            return Err(denied());
        }
        Ok(())
    }

    fn open_at(directory: &File, name: &std::ffi::CStr, flags: libc::c_int) -> io::Result<File> {
        // SAFETY: the directory and NUL-terminated name outlive the call. The
        // mode argument is used only with O_CREAT; successful fds are owned.
        let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC, 0o600 as libc::c_uint) };
        if fd < 0 { return Err(io::Error::last_os_error()); }
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    impl DiscoveryDirectory {
        /// Duplicate the already validated directory, for relative IPC lifecycle operations.
        pub fn try_clone_directory(&self) -> io::Result<File> { self.0.try_clone() }

        pub fn open(home: &Path, create: bool) -> io::Result<Self> {
            let home = OpenOptions::new().read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC).open(home)?;
            let mut created = false;
            if create {
                // SAFETY: home is a live directory fd and the name is static.
                if unsafe { libc::mkdirat(home.as_raw_fd(), c"run".as_ptr(), 0o700) } == 0 {
                    created = true;
                } else {
                    let error = io::Error::last_os_error();
                    if error.kind() != io::ErrorKind::AlreadyExists { return Err(error); }
                }
            }
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            // SAFETY: stat points to writable storage; inspect it only on success.
            if unsafe { libc::fstatat(home.as_raw_fd(), c"run".as_ptr(), stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let stat = unsafe { stat.assume_init() };
            if stat.st_mode & libc::S_IFMT != libc::S_IFDIR
                || stat.st_uid != unsafe { libc::getuid() } || stat.st_mode & 0o077 != 0 {
                return Err(denied());
            }
            let directory = open_at(&home, c"run", libc::O_RDONLY | libc::O_DIRECTORY)?;
            let metadata = directory.metadata()?;
            private(&metadata, true)?;
            // Validate the opened object, not just the earlier pathname check.
            if metadata.dev() != stat.st_dev as u64 || metadata.ino() != stat.st_ino as u64 {
                return Err(denied());
            }
            if created { directory.set_permissions(std::fs::Permissions::from_mode(0o700))?; }
            Ok(Self(directory))
        }

        pub fn read(&self) -> io::Result<String> {
            // O_NONBLOCK also ensures a planted FIFO cannot hang discovery.
            let file = open_at(&self.0, c"identity-endpoint", libc::O_RDONLY | libc::O_NONBLOCK)?;
            private(&file.metadata()?, false)?;
            let mut text = String::new();
            file.take(MAX_BYTES + 1).read_to_string(&mut text)?;
            if text.len() as u64 > MAX_BYTES { return Err(io::ErrorKind::InvalidData.into()); }
            Ok(text)
        }

        pub fn write(&self, text: &str) -> io::Result<()> {
            let name = CString::new(format!(".identity-{}.tmp", uuid::Uuid::new_v4()))
                .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
            let mut file = open_at(&self.0, &name, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL)?;
            let result = (|| {
                file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
                file.write_all(text.as_bytes())?;
                file.sync_all()?;
                // SAFETY: both names are single components in the same pinned dirfd.
                if unsafe { libc::renameat(self.0.as_raw_fd(), name.as_ptr(),
                    self.0.as_raw_fd(), c"identity-endpoint".as_ptr()) } != 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            })();
            if result.is_err() {
                // SAFETY: unlink only the temporary entry inside the pinned directory.
                unsafe { libc::unlinkat(self.0.as_raw_fd(), name.as_ptr(), 0); }
            }
            result
        }

        pub fn remove(&self) -> io::Result<()> {
            // SAFETY: the directory stays open; unlinkat does not follow the file.
            if unsafe { libc::unlinkat(self.0.as_raw_fd(), c"identity-endpoint".as_ptr(), 0) } != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::fs::{File, OpenOptions};
    use std::io::{Read, Write};
    use std::os::windows::{ffi::OsStrExt, fs::{MetadataExt, OpenOptionsExt},
        io::{AsRawHandle, FromRawHandle}};
    use std::path::{Component, PathBuf};
    use std::ptr::{null, null_mut};
    use crate::identity_token::windows as acl;
    use windows_sys::Win32::{Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE},
        Security::{ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, EqualSid, GetAce,
            GetSecurityDescriptorControl, GetSecurityDescriptorDacl, IsValidAcl, IsValidSid,
            PROTECTED_DACL_SECURITY_INFORMATION, SE_DACL_PROTECTED, SECURITY_ATTRIBUTES, TOKEN_USER},
        Security::Authorization::{ConvertSidToStringSidW, GetSecurityInfo, SetSecurityInfo, SE_FILE_OBJECT},
        Storage::FileSystem::{CreateDirectoryW, CreateFileW, CREATE_NEW, FILE_ALL_ACCESS,
            FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_SHARE_READ, FILE_SHARE_WRITE, READ_CONTROL, WRITE_DAC},
        System::SystemServices::ACCESS_ALLOWED_ACE_TYPE};

    pub struct DiscoveryDirectory {
        path: PathBuf,
        // No delete sharing: checked ancestors cannot be renamed/replaced.
        _pins: Vec<File>,
    }

    fn check(file: &File, directory: bool) -> io::Result<()> {
        let metadata = file.metadata()?;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || if directory { !metadata.is_dir() } else { !metadata.is_file() } {
            return Err(denied());
        }
        Ok(())
    }

    fn wide(path: &Path) -> io::Result<Vec<u16>> {
        let value: Vec<u16> = path.as_os_str().encode_wide().collect();
        if value.contains(&0) { return Err(denied()); }
        Ok(value.into_iter().chain(Some(0)).collect())
    }

    fn owner_descriptor() -> io::Result<acl::LocalAllocation> {
        let user = acl::current_user()?;
        // SAFETY: current_user returns aligned TOKEN_USER storage retaining the SID.
        let sid = unsafe { (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid };
        let mut text = null_mut();
        if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 { return Err(io::Error::last_os_error()); }
        let _text = acl::LocalAllocation(text.cast());
        let mut length = 0;
        while unsafe { *text.add(length) } != 0 { length += 1; }
        let sid = String::from_utf16(unsafe { std::slice::from_raw_parts(text, length) })
            .map_err(|_| denied())?;
        // No inherited ACEs, SYSTEM or Administrators grants: only this user.
        acl::descriptor(&format!("O:{sid}D:P(A;;FA;;;{sid})"))
    }

    fn owner_only(file: &File) -> io::Result<()> {
        let user = acl::current_user()?;
        acl::validate_acl(file, &user, true)?;
        let mut dacl: *mut ACL = null_mut();
        let mut raw = null_mut();
        // SAFETY: live file handle and writable outputs; allocation freed below.
        let status = unsafe { GetSecurityInfo(file.as_raw_handle(), SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION, null_mut(), null_mut(), &mut dacl, null_mut(), &mut raw) };
        if status != 0 { return Err(io::Error::from_raw_os_error(status as i32)); }
        let _descriptor = acl::LocalAllocation(raw);
        let (mut control, mut revision) = (0, 0);
        if unsafe { GetSecurityDescriptorControl(raw, &mut control, &mut revision) } == 0
            || control & SE_DACL_PROTECTED == 0 || dacl.is_null()
            || unsafe { IsValidAcl(dacl) } == 0 || unsafe { (*dacl).AceCount } != 1 { return Err(denied()); }
        let mut ace = null_mut();
        if unsafe { GetAce(dacl, 0, &mut ace) } == 0 { return Err(denied()); }
        let header = unsafe { &*ace.cast::<ACE_HEADER>() };
        if u32::from(header.AceType) != ACCESS_ALLOWED_ACE_TYPE || header.AceFlags != 0
            || (header.AceSize as usize) < std::mem::size_of::<ACCESS_ALLOWED_ACE>() { return Err(denied()); }
        let ace = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
        let sid = (&ace.SidStart as *const u32).cast_mut().cast();
        let current = unsafe { (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid };
        if ace.Mask != FILE_ALL_ACCESS || unsafe { IsValidSid(sid) } == 0
            || unsafe { EqualSid(sid, current) } == 0 { return Err(denied()); }
        Ok(())
    }

    fn pin(path: &Path, update_acl: bool) -> io::Result<File> {
        let file = OpenOptions::new().read(true).share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .access_mode(GENERIC_READ | READ_CONTROL | if update_acl { WRITE_DAC } else { 0 })
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT).open(path)?;
        check(&file, true)?;
        Ok(file)
    }

    impl DiscoveryDirectory {
        pub fn open(home: &Path, create: bool) -> io::Result<Self> {
            let home = std::path::absolute(home)?;
            let mut path = PathBuf::new();
            let mut pins = Vec::new();
            for component in home.components() {
                path.push(component.as_os_str());
                if !matches!(component, Component::Prefix(_)) { pins.push(pin(&path, false)?); }
            }
            path.push("run");
            if create {
                let security = owner_descriptor()?;
                let attributes = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                    lpSecurityDescriptor: security.0, bInheritHandle: 0 };
                let target = wide(&path)?;
                // The directory is private from its first instant, even under a public home.
                if unsafe { CreateDirectoryW(target.as_ptr(), &attributes) } == 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() != io::ErrorKind::AlreadyExists { return Err(error); }
                }
                let directory = pin(&path, true)?;
                // Only tighten an existing directory with a trusted owner and no outsider writers.
                acl::validate_acl(&directory, &acl::current_user()?, false)?;
                let (mut present, mut defaulted, mut dacl) = (0, 0, null_mut());
                if unsafe { GetSecurityDescriptorDacl(security.0, &mut present, &mut dacl, &mut defaulted) } == 0
                    || present == 0 || dacl.is_null() { return Err(denied()); }
                let status = unsafe { SetSecurityInfo(directory.as_raw_handle(), SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    null_mut(), null_mut(), dacl, null()) };
                if status != 0 { return Err(io::Error::from_raw_os_error(status as i32)); }
                owner_only(&directory)?;
                pins.push(directory);
            } else {
                let directory = pin(&path, false)?;
                owner_only(&directory)?;
                pins.push(directory);
            }
            Ok(Self { path, _pins: pins })
        }

        pub fn read(&self) -> io::Result<String> {
            let file = OpenOptions::new().read(true).share_mode(FILE_SHARE_READ)
                .access_mode(GENERIC_READ | READ_CONTROL)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT).open(self.path.join(FILENAME))?;
            check(&file, false)?;
            owner_only(&file)?;
            let mut text = String::new();
            file.take(MAX_BYTES + 1).read_to_string(&mut text)?;
            if text.len() as u64 > MAX_BYTES { return Err(io::ErrorKind::InvalidData.into()); }
            Ok(text)
        }

        pub fn write(&self, text: &str) -> io::Result<()> {
            let temporary = self.path.join(format!(".identity-{}.tmp", uuid::Uuid::new_v4()));
            let security = owner_descriptor()?;
            let attributes = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: security.0, bInheritHandle: 0 };
            let target = wide(&temporary)?;
            let raw = unsafe { CreateFileW(target.as_ptr(), GENERIC_WRITE | READ_CONTROL,
                FILE_SHARE_READ, &attributes, CREATE_NEW, FILE_FLAG_OPEN_REPARSE_POINT, null_mut()) };
            if raw == INVALID_HANDLE_VALUE { return Err(io::Error::last_os_error()); }
            let mut file = unsafe { File::from_raw_handle(raw) };
            let result = (|| {
                check(&file, false)?;
                owner_only(&file)?;
                file.write_all(text.as_bytes())?;
                file.sync_all()
            })();
            drop(file);
            // rename moves the entry itself, never a reparse point's target.
            let result = result.and_then(|()| std::fs::rename(&temporary, self.path.join(FILENAME)));
            if result.is_err() { let _ = std::fs::remove_file(&temporary); }
            result
        }

        pub fn remove(&self) -> io::Result<()> {
            std::fs::remove_file(self.path.join(FILENAME))
        }
    }

    #[test]
    fn windows_discovery_has_explicit_current_user_only_dacl() {
        let home = tempfile::tempdir().unwrap();
        // Exercise tightening the inherited ACL on a preexisting run directory.
        std::fs::create_dir(home.path().join("run")).unwrap();
        let directory = DiscoveryDirectory::open(home.path(), true).unwrap();
        owner_only(directory._pins.last().unwrap()).unwrap();
        for text in ["first", "replacement"] {
            directory.write(text).unwrap();
            // read validates the protected, single current-user ACE on the actual file.
            assert_eq!(directory.read().unwrap(), text);
        }
        assert!(DiscoveryDirectory::open(home.path(), false).is_ok());
        directory.remove().unwrap();
    }
}

#[cfg(not(any(unix, windows)))]
mod platform {
    use super::*;

    pub struct DiscoveryDirectory;

    impl DiscoveryDirectory {
        pub fn open(_: &Path, _: bool) -> io::Result<Self> { Err(denied()) }
        pub fn read(&self) -> io::Result<String> { Err(denied()) }
        pub fn write(&self, _: &str) -> io::Result<()> { Err(denied()) }
        pub fn remove(&self) -> io::Result<()> { Err(denied()) }
    }
}

pub use platform::DiscoveryDirectory;

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
    use crate::run_credential::IDENTITY_ENDPOINT_FILE;

    #[test]
    fn creates_private_directory_and_file_and_replaces_atomically() {
        let home = tempfile::tempdir().unwrap();
        let directory = DiscoveryDirectory::open(home.path(), true).unwrap();
        directory.write("first").unwrap();
        let run = std::fs::metadata(home.path().join("run")).unwrap();
        let file = std::fs::metadata(home.path().join(IDENTITY_ENDPOINT_FILE)).unwrap();
        assert!(run.is_dir());
        assert!(file.is_file());
        assert_eq!(run.mode() & 0o777, 0o700);
        assert_eq!(file.mode() & 0o777, 0o600);
        assert_eq!(run.uid(), unsafe { libc::getuid() });
        assert_eq!(file.uid(), unsafe { libc::getuid() });
        assert_eq!(directory.read().unwrap(), "first");
        directory.write("second").unwrap();
        assert_eq!(directory.read().unwrap(), "second");
        assert_eq!(std::fs::read_dir(home.path().join("run")).unwrap().count(), 1);
    }

    #[test]
    fn refuses_symlinked_and_nonprivate_run_directories() {
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let run = home.path().join("run");
        symlink(outside.path(), &run).unwrap();
        for create in [false, true] {
            assert!(DiscoveryDirectory::open(home.path(), create).is_err());
        }
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
        std::fs::remove_file(&run).unwrap();
        std::fs::create_dir(&run).unwrap();
        for mode in [0o770, 0o750, 0o707] {
            std::fs::set_permissions(&run, std::fs::Permissions::from_mode(mode)).unwrap();
            for create in [false, true] {
                assert!(DiscoveryDirectory::open(home.path(), create).is_err());
            }
            // Do not silently chmod an unsafe preexisting directory.
            assert_eq!(std::fs::metadata(&run).unwrap().mode() & 0o777, mode);
        }
    }

    #[test]
    fn reader_refuses_symlinks_nonprivate_files_and_nonregular_files() {
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let directory = DiscoveryDirectory::open(home.path(), true).unwrap();
        let file = home.path().join(IDENTITY_ENDPOINT_FILE);
        let target = outside.path().join("target");
        std::fs::write(&target, "attacker").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&target, &file).unwrap();
        assert!(directory.read().is_err());
        directory.write("trusted").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "attacker");
        assert_eq!(directory.read().unwrap(), "trusted");
        for mode in [0o640, 0o620, 0o604] {
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(mode)).unwrap();
            assert!(directory.read().is_err());
        }
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir(&file).unwrap();
        assert!(directory.read().is_err());
    }

    #[test]
    fn write_and_cleanup_stay_in_the_opened_directory_after_path_replacement() {
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let directory = DiscoveryDirectory::open(home.path(), true).unwrap();
        let original = home.path().join("original-run");
        std::fs::rename(home.path().join("run"), &original).unwrap();
        symlink(outside.path(), home.path().join("run")).unwrap();
        std::fs::write(outside.path().join("identity-endpoint"), "untouched").unwrap();
        directory.write("trusted").unwrap();
        assert_eq!(std::fs::read_to_string(original.join("identity-endpoint")).unwrap(), "trusted");
        directory.remove().unwrap();
        assert!(!original.join("identity-endpoint").exists());
        assert_eq!(std::fs::read_to_string(outside.path().join("identity-endpoint")).unwrap(), "untouched");
    }
}
