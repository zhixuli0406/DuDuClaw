//! Private local state with frozen Windows path ancestry and handle-based ACLs.
//!
//! Every directory handle excludes delete sharing, so a checked ancestor cannot
//! be renamed or replaced while this object is alive. Reparse points and network
//! paths are refused. File creation uses a protected DACL before any secret bytes
//! are written; replacement renames the open temporary handle into the pinned dir.
//!
//! The configured absolute home and local drive mapping are trusted inputs. The
//! drive mapping must remain stable: directory pins prevent directory-entry
//! swaps, not drive remapping by the same account or an administrator. This is
//! defense in depth, not isolation from another process running as the same user.
use std::{fs::File, io::{self, Read, Write}, path::{Component, Path, PathBuf, Prefix}, ptr::{null, null_mut}};
use std::os::windows::{ffi::OsStrExt, io::{AsRawHandle, FromRawHandle}};
use crate::identity_token::windows as acl;
use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CREATE_NEW, CreateDirectoryW, CreateFileW, DELETE,
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TYPE_DISK,
    FILE_RENAME_INFO, FILE_DISPOSITION_INFO, FileRenameInfo, FileDispositionInfo,
    GetDriveTypeW, GetFileInformationByHandle, GetFileType, OPEN_EXISTING, READ_CONTROL, SetFileInformationByHandle,
};

const MAX_BYTES: usize = 4096;

/// A private state directory whose complete local-disk path stays pinned.
/// This does not isolate processes running as the same Windows account.
pub struct SecureDirectory {
    path: PathBuf,
    directories: Vec<File>,
    user: Vec<usize>,
}

fn invalid(message: &'static str) -> io::Error { io::Error::new(io::ErrorKind::InvalidInput, message) }
fn denied(message: &'static str) -> io::Error { io::Error::new(io::ErrorKind::PermissionDenied, message) }
fn wide(path: &Path) -> io::Result<Vec<u16>> {
    let value: Vec<u16> = path.as_os_str().encode_wide().collect();
    if value.contains(&0) { return Err(invalid("NUL in private state path")); }
    Ok(value.into_iter().chain(Some(0)).collect())
}
fn name(name: &str) -> io::Result<()> {
    if name.is_empty() || name.len() > 128 || name == "." || name == ".."
        || !name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
        || name.ends_with('.') { return Err(invalid("invalid private state filename")); }
    // Reserve Win32 device aliases even when an extension is present.
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|prefix| stem.strip_prefix(prefix)
            .is_some_and(|n| n.len() == 1 && matches!(n.as_bytes()[0], b'1'..=b'9'))) {
        return Err(invalid("device names are not private state files"));
    }
    Ok(())
}
fn local_drive_type(kind: u32) -> bool {
    // Win32 DRIVE_* values (winbase.h). Keep this narrow instead of treating
    // FILE_TYPE_DISK as local: mapped SMB drives also report that file type.
    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_FIXED: u32 = 3;
    const DRIVE_RAMDISK: u32 = 6;
    matches!(kind, DRIVE_REMOVABLE | DRIVE_FIXED | DRIVE_RAMDISK)
}
fn disk_type(file: &File, directory: bool) -> io::Result<()> {
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileType(file.as_raw_handle()) } != FILE_TYPE_DISK { return Err(denied("private state must be on disk")); }
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 { return Err(io::Error::last_os_error()); }
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0) != directory
        || (!directory && info.nNumberOfLinks != 1) {
        return Err(denied("private state refuses reparse points, wrong types and hardlinks"));
    }
    Ok(())
}
fn pin_directory(path: &Path) -> io::Result<File> {
    let path = wide(path)?;
    // No FILE_SHARE_DELETE: once opened, this component cannot be swapped.
    let raw = unsafe { CreateFileW(path.as_ptr(), GENERIC_READ | READ_CONTROL,
        FILE_SHARE_READ | FILE_SHARE_WRITE, null(), OPEN_EXISTING,
        FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT, null_mut()) };
    if raw == INVALID_HANDLE_VALUE { return Err(io::Error::last_os_error()); }
    // SAFETY: successful CreateFileW returned one new owned handle.
    let file = unsafe { File::from_raw_handle(raw) };
    disk_type(&file, true)?;
    Ok(file)
}

impl SecureDirectory {
    /// Open an existing gateway home and create/open its private state child.
    /// All ancestors are pinned before the next path component is inspected.
    pub fn open(home: &Path, child: &str) -> io::Result<Self> {
        name(child)?;
        if !home.is_absolute() { return Err(invalid("private state home must be absolute")); }
        let user = acl::current_user()?;
        let mut path = PathBuf::new();
        let mut directories = Vec::new();
        for component in home.components() {
            match component {
                Component::Prefix(prefix) if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)) => path.push(prefix.as_os_str()),
                Component::RootDir => {
                    path.push(component.as_os_str());
                    let root = wide(&path)?;
                    if !local_drive_type(unsafe { GetDriveTypeW(root.as_ptr()) }) {
                        return Err(denied("private state requires a supported local drive"));
                    }
                    directories.push(pin_directory(&path)?);
                },
                Component::Normal(value) => {
                    let units: Vec<u16> = value.encode_wide().collect();
                    if units.contains(&0) || units.contains(&(b':' as u16))
                        || units.last().is_some_and(|v| matches!(*v, 32 | 46)) {
                        return Err(invalid("ambiguous Windows private state path"));
                    }
                    path.push(value);
                    directories.push(pin_directory(&path)?);
                },
                _ => return Err(invalid("private state refuses network, device and relative paths")),
            }
        }
        let home_handle = directories.last().ok_or_else(|| invalid("missing private state home"))?;
        acl::validate_acl(home_handle, &user, false)?;
        path.push(child);
        let security = acl::private_descriptor(&user)?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: security.0, bInheritHandle: 0,
        };
        let target = wide(&path)?;
        if unsafe { CreateDirectoryW(target.as_ptr(), &attributes) } == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_ALREADY_EXISTS as i32) { return Err(error); }
        }
        let directory = pin_directory(&path)?;
        acl::validate_acl(&directory, &user, true)?;
        directories.push(directory);
        Ok(Self { path, directories, user })
    }

    fn file(&self, filename: &str, write: bool, create: bool, rename: bool, share_write: bool) -> io::Result<File> {
        name(filename)?;
        let security = if create { Some(acl::private_descriptor(&self.user)?) } else { None };
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: security.as_ref().map_or(null_mut(), |s| s.0), bInheritHandle: 0,
        };
        let path = wide(&self.path.join(filename))?;
        let raw = unsafe { CreateFileW(path.as_ptr(), GENERIC_READ | READ_CONTROL
            | if write { GENERIC_WRITE } else { 0 } | if rename { DELETE } else { 0 },
            FILE_SHARE_READ | if share_write { FILE_SHARE_WRITE } else { 0 },
            if create { &attributes } else { null() }, if create { CREATE_NEW } else { OPEN_EXISTING },
            FILE_FLAG_OPEN_REPARSE_POINT, null_mut()) };
        if raw == INVALID_HANDLE_VALUE { return Err(io::Error::last_os_error()); }
        let file = unsafe { File::from_raw_handle(raw) };
        disk_type(&file, false)?;
        acl::validate_acl(&file, &self.user, true)?;
        Ok(file)
    }

    /// Open a persistent, non-replaceable lock file without truncating it.
    pub fn open_lock(&self, filename: &str) -> io::Result<File> {
        match self.file(filename, true, true, false, true) {
            Ok(file) => Ok(file),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => self.file(filename, true, false, false, true),
            Err(e) => Err(e),
        }
    }

    /// Bounded reads refuse unsafe permissions before exposing any bytes.
    pub fn read(&self, filename: &str) -> io::Result<Option<zeroize::Zeroizing<Vec<u8>>>> {
        let file = match self.file(filename, false, false, false, false) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let mut bytes = zeroize::Zeroizing::new(Vec::new());
        file.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > MAX_BYTES { return Err(io::ErrorKind::InvalidData.into()); }
        Ok(Some(bytes))
    }

    /// Write, flush and replace using the source handle and pinned directory.
    pub fn write_atomic(&self, filename: &str, bytes: &[u8]) -> io::Result<()> {
        name(filename)?;
        if bytes.len() > MAX_BYTES { return Err(io::ErrorKind::InvalidData.into()); }
        match self.file(filename, false, false, false, false) {
            Ok(_) => (),
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            Err(e) => return Err(e),
        }
        let temporary = format!(".private-{}.tmp", uuid::Uuid::new_v4());
        let mut file = self.file(&temporary, true, true, true, false)?;
        let mut renamed = false;
        let result = (|| {
            file.write_all(bytes)?;
            file.sync_all()?;
            let target: Vec<u16> = filename.encode_utf16().collect();
            let size = std::mem::offset_of!(FILE_RENAME_INFO, FileName) + target.len() * 2;
            let mut buffer = vec![0usize; size.div_ceil(std::mem::size_of::<usize>())];
            let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
            // SAFETY: pointer-aligned zeroed storage includes the complete variable
            // filename; pointers and the pinned root handle remain alive for the call.
            unsafe {
                (*info).Anonymous.ReplaceIfExists = 1;
                (*info).RootDirectory = self.directories.last().expect("pinned state directory").as_raw_handle();
                (*info).FileNameLength = (target.len() * 2) as u32;
                std::ptr::copy_nonoverlapping(target.as_ptr(), std::ptr::addr_of_mut!((*info).FileName).cast(), target.len());
                if SetFileInformationByHandle(file.as_raw_handle(), FileRenameInfo, info.cast(), size as u32) == 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            renamed = true;
            file.sync_all()
        })();
        if result.is_err() && !renamed {
            // Delete the temporary object through its handle, never a raceable path.
            // After a successful rename a flush failure preserves the landed file.
            let disposition = FILE_DISPOSITION_INFO { DeleteFile: 1 };
            unsafe { SetFileInformationByHandle(file.as_raw_handle(), FileDispositionInfo,
                (&disposition as *const FILE_DISPOSITION_INFO).cast(), std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32); }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::fs::{OpenOptionsExt, symlink_dir, symlink_file};
    use windows_sys::Win32::Security::{GetSecurityDescriptorDacl, DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION};
    use windows_sys::Win32::Security::Authorization::{SetSecurityInfo, SE_FILE_OBJECT};
    use windows_sys::Win32::Storage::FileSystem::WRITE_DAC;

    fn set_dacl(path: &Path, descriptor: Option<&acl::LocalAllocation>) {
        let file = std::fs::OpenOptions::new().read(true)
            .access_mode(GENERIC_READ | READ_CONTROL | WRITE_DAC)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path).unwrap();
        let mut dacl = null_mut();
        if let Some(descriptor) = descriptor {
            let (mut present, mut defaulted) = (0, 0);
            assert_ne!(unsafe { GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut dacl, &mut defaulted) }, 0);
            assert_ne!(present, 0);
        }
        assert_eq!(unsafe { SetSecurityInfo(file.as_raw_handle(), SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            null_mut(), null_mut(), dacl, null()) }, 0);
    }
    fn private_home() -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        let user = acl::current_user().unwrap();
        set_dacl(home.path(), Some(&acl::private_descriptor(&user).unwrap()));
        home
    }

    #[test]
    fn windows_private_state_drive_policy_excludes_mapped_network_drives() {
        for kind in [2, 3, 6] { assert!(local_drive_type(kind)); }
        // UNKNOWN, NO_ROOT_DIR, REMOTE, CDROM and unrecognized future types.
        for kind in [0, 1, 4, 5, 7, u32::MAX] { assert!(!local_drive_type(kind)); }
    }

    #[test]
    fn windows_private_state_roundtrip_atomic_replace_and_ancestor_pinning() {
        let home = private_home();
        let store = SecureDirectory::open(home.path(), "state").unwrap();
        assert!(store.read("credential.json").unwrap().is_none());
        store.write_atomic("credential.json", b"old").unwrap();
        assert_eq!(store.read("credential.json").unwrap().unwrap().as_slice(), b"old");
        store.write_atomic("credential.json", b"replacement").unwrap();
        assert_eq!(store.read("credential.json").unwrap().unwrap().as_slice(), b"replacement");
        let state = home.path().join("state");
        assert!(std::fs::rename(&state, home.path().join("swapped-state")).is_err());
        assert!(std::fs::rename(home.path(), home.path().with_extension("swapped-home")).is_err());
        let entries: Vec<_> = std::fs::read_dir(&state).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(entries.len(), 1);
        drop(store);
        std::fs::rename(&state, home.path().join("swapped-state")).unwrap();
    }

    #[test]
    fn windows_private_state_rejects_public_null_dacl_and_hardlinks() {
        for protection in ["public", "null", "hardlink"] {
            let home = private_home();
            let store = SecureDirectory::open(home.path(), "state").unwrap();
            store.write_atomic("credential.json", b"private").unwrap();
            let path = home.path().join("state/credential.json");
            match protection {
                "public" => set_dacl(&path, Some(&acl::descriptor("D:P(A;;FA;;;WD)").unwrap())),
                "null" => set_dacl(&path, None),
                "hardlink" => std::fs::hard_link(&path, home.path().join("linked.json")).unwrap(),
                _ => unreachable!(),
            }
            assert!(store.read("credential.json").is_err(), "{protection}");
            assert!(store.write_atomic("credential.json", b"replacement").is_err(), "{protection}");
        }
    }

    #[test]
    fn windows_private_state_rejects_unsafe_home_and_state_permissions() {
        for component in ["home", "state"] {
            let home = private_home();
            if component == "state" { drop(SecureDirectory::open(home.path(), "state").unwrap()); }
            let path = if component == "home" { home.path().to_path_buf() } else { home.path().join("state") };
            set_dacl(&path, Some(&acl::descriptor("D:P(A;;FA;;;WD)").unwrap()));
            assert!(SecureDirectory::open(home.path(), "state").is_err());
        }
    }

    #[test]
    fn windows_private_state_caps_reads_writes_and_rejects_device_names() {
        let home = private_home();
        let store = SecureDirectory::open(home.path(), "state").unwrap();
        store.write_atomic("credential.json", b"bounded").unwrap();
        assert!(store.write_atomic("credential.json", &vec![0; MAX_BYTES + 1]).is_err());
        assert_eq!(store.read("credential.json").unwrap().unwrap().as_slice(), b"bounded");
        let mut file = store.file("large.json", true, true, false, false).unwrap();
        file.write_all(&vec![0; MAX_BYTES + 1]).unwrap();
        drop(file);
        assert!(store.read("large.json").is_err());
        for filename in ["", "..", "../outside", "dir\\outside", "C:outside", "file:stream", "name.", "NUL", "COM1.json", "LPT9", "a\0b"] {
            assert!(store.write_atomic(filename, b"private").is_err(), "{filename:?}");
        }
        for path in [Path::new("relative"), Path::new(r"\\server\share\home"), Path::new(r"\\.\C:\home")] {
            assert!(SecureDirectory::open(path, "state").is_err());
        }
    }

    #[test]
    fn windows_private_state_refuses_directory_and_file_reparse_points() {
        let home = private_home();
        let outside = private_home();
        let alias = home.path().join("alias");
        if let Err(error) = symlink_dir(outside.path(), &alias) {
            if error.raw_os_error() == Some(1314) {
                eprintln!("SKIP: reparse-point test requires Windows Developer Mode or SeCreateSymbolicLinkPrivilege");
                return;
            }
            panic!("could not create directory reparse point: {error}");
        }
        assert!(SecureDirectory::open(&alias, "state").is_err());
        // The linked component is an ancestor, not only the final home directory.
        std::fs::create_dir(outside.path().join("nested")).unwrap();
        assert!(SecureDirectory::open(&alias.join("nested"), "state").is_err());
        symlink_dir(outside.path(), home.path().join("state")).unwrap();
        assert!(SecureDirectory::open(home.path(), "state").is_err());
        std::fs::remove_dir(home.path().join("state")).unwrap();
        let store = SecureDirectory::open(home.path(), "state").unwrap();
        let target = outside.path().join("target.json");
        std::fs::write(&target, b"outside").unwrap();
        symlink_file(&target, home.path().join("state/credential.json")).unwrap();
        assert!(store.read("credential.json").is_err());
        assert!(store.write_atomic("credential.json", b"replacement").is_err());
        assert_eq!(std::fs::read(target).unwrap(), b"outside");
    }

    #[test]
    fn windows_private_state_lock_file_is_pinned_and_contended() {
        let home = private_home();
        let store = SecureDirectory::open(home.path(), "state").unwrap();
        let lock = store.open_lock("relay.lock").unwrap();
        fs2::FileExt::try_lock_exclusive(&lock).unwrap();
        let second = store.open_lock("relay.lock").unwrap();
        assert!(fs2::FileExt::try_lock_exclusive(&second).is_err());
        assert!(std::fs::rename(home.path().join("state/relay.lock"), home.path().join("state/moved.lock")).is_err());
        drop(lock);
        fs2::FileExt::try_lock_exclusive(&second).unwrap();
    }
}
