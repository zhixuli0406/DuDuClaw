//! Windows SQLite guards. No existing owner or ACL is ever repaired.
use std::{fs::{File, OpenOptions}, io, os::windows::{fs::OpenOptionsExt,
    io::{AsRawHandle, FromRawHandle, OwnedHandle}}, path::{Component, Path, PathBuf, Prefix}, ptr::null_mut};
use windows_sys::Win32::{Foundation::*, Security::{*, Authorization::*},
    Storage::FileSystem::*, System::{Threading::*, WindowsProgramming::{DRIVE_FIXED, DRIVE_REMOVABLE, DRIVE_RAMDISK}}};
use windows_sys::Wdk::Storage::FileSystem::{FILE_OPEN_IF, FILE_NON_DIRECTORY_FILE};
use crate::fs_safe::SafeDir;

// Fields drop in declaration order: SQLite closes (including its journals)
// before the no-delete-sharing DB handle and the pinned ancestor chain.
pub(crate) struct Connection {
    connection: rusqlite::Connection,
    _file: File,
    _directory: SafeDir,
}
impl std::ops::Deref for Connection {
    type Target = rusqlite::Connection;
    fn deref(&self) -> &Self::Target { &self.connection }
}
impl std::ops::DerefMut for Connection {
    fn deref_mut(&mut self) -> &mut Self::Target { &mut self.connection }
}

struct LocalAllocation(*mut std::ffi::c_void);
impl Drop for LocalAllocation { fn drop(&mut self) { unsafe { LocalFree(self.0); } } }

fn denied() -> io::Error { io::Error::new(io::ErrorKind::PermissionDenied, "secure database open unavailable") }
fn check(status: u32) -> io::Result<()> {
    if status == ERROR_SUCCESS { Ok(()) } else { Err(io::Error::from_raw_os_error(status as i32)) }
}
fn token_information(class: TOKEN_INFORMATION_CLASS) -> io::Result<Vec<usize>> {
    let mut raw = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut size = 0;
    if unsafe { GetTokenInformation(token.as_raw_handle(), class, null_mut(), 0, &mut size) } != 0 {
        return Err(denied());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32) { return Err(error); }
    let minimum = std::mem::size_of::<TOKEN_USER>();
    if (size as usize) < minimum { return Err(denied()); }
    let mut value = vec![0usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
    if unsafe { GetTokenInformation(token.as_raw_handle(), class, value.as_mut_ptr().cast(), size, &mut size) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(value)
}
fn user_sid(user: &[usize]) -> PSID { unsafe { (*(user.as_ptr().cast::<TOKEN_USER>())).User.Sid } }
struct SecurityDescriptor {
    descriptor: SECURITY_DESCRIPTOR,
    // Own both the DACL and the copied TokenUser owner SID referenced above.
    _security: LocalAllocation,
}
#[cfg(test)]
impl SecurityDescriptor {
    fn as_ptr(&self) -> PSECURITY_DESCRIPTOR {
        std::ptr::addr_of!(self.descriptor).cast_mut().cast()
    }
}
fn descriptor(user: &[usize], directory: bool) -> io::Result<SecurityDescriptor> {
    let mut raw = null_mut();
    if unsafe { ConvertSidToStringSidW(user_sid(user), &mut raw) } == 0 { return Err(io::Error::last_os_error()); }
    let allocation = LocalAllocation(raw.cast());
    let mut len = 0;
    unsafe { while *raw.add(len) != 0 { len += 1; } }
    let sid = String::from_utf16(unsafe { std::slice::from_raw_parts(raw, len) }).map_err(|_| denied())?;
    drop(allocation);
    // Directory ACEs also protect newly created temp files before they contain data.
    let flags = if directory { "OICI" } else { "" };
    let sddl: Vec<u16> = format!("O:{sid}D:P(A;{flags};FA;;;{sid})(A;{flags};FA;;;SY)(A;{flags};FA;;;BA)")
        .encode_utf16().chain(Some(0)).collect();
    let mut security = null_mut();
    if unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), 1, &mut security, null_mut()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let allocation = LocalAllocation(security);
    let mut acl = null_mut();
    let mut present = 0;
    let mut defaulted = 0;
    if unsafe { GetSecurityDescriptorDacl(security, &mut present, &mut acl, &mut defaulted) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if present == 0 || acl.is_null() { return Err(denied()); }
    let mut owner = null_mut();
    if unsafe { GetSecurityDescriptorOwner(security, &mut owner, &mut defaulted) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if owner.is_null() { return Err(denied()); }
    let mut descriptor: SECURITY_DESCRIPTOR = unsafe { std::mem::zeroed() };
    let raw = std::ptr::addr_of_mut!(descriptor).cast();
    if unsafe { InitializeSecurityDescriptor(raw, 1) } == 0 { return Err(io::Error::last_os_error()); }
    // Explicit TokenUser ownership also works with elevated tokens whose default
    // owner is Administrators. Any failure (including ERROR_INVALID_OWNER) is fatal.
    if unsafe { SetSecurityDescriptorOwner(raw, owner, 0) } == 0 { return Err(io::Error::last_os_error()); }
    if unsafe { SetSecurityDescriptorDacl(raw, 1, acl, 0) } == 0 { return Err(io::Error::last_os_error()); }
    if unsafe { SetSecurityDescriptorControl(raw, SE_DACL_PROTECTED, SE_DACL_PROTECTED) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(SecurityDescriptor { descriptor, _security: allocation })
}
fn validate_entry(file: &File, directory: bool) -> io::Result<()> {
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileType(file.as_raw_handle()) } != FILE_TYPE_DISK { return Err(denied()); }
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 { return Err(io::Error::last_os_error()); }
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0) != directory
        || (!directory && info.nNumberOfLinks != 1) { return Err(denied()); }
    Ok(())
}
// Binary forms of S-1-5-18 and S-1-5-32-544. No token group membership
// or default-owner privileges are needed to validate an existing object.
const SYSTEM_SID: &[u8] = &[1, 1, 0, 0, 0, 0, 0, 5, 18, 0, 0, 0];
const ADMINISTRATORS_SID: &[u8] = &[1, 2, 0, 0, 0, 0, 0, 5, 32, 0, 0, 0, 32, 2, 0, 0];
fn owner_allowed(owner_sid: &[u8], token_user_sid: &[u8]) -> bool {
    owner_sid == token_user_sid || owner_sid == SYSTEM_SID || owner_sid == ADMINISTRATORS_SID
}
fn acl_allowed<'a>(owner: &[u8], user: &[u8], protected: bool, strict_owner: bool,
    aces: impl IntoIterator<Item = (u8, &'a [u8])>) -> bool {
    protected && (owner == user || (!strict_owner && owner_allowed(owner, user))) && aces.into_iter().all(|(kind, sid)| match kind {
        0 => owner_allowed(sid, user), // ACCESS_ALLOWED_ACE_TYPE
        1 => true, // ACCESS_DENIED_ACE_TYPE cannot grant access.
        _ => false, // Unsupported object/callback ACEs fail closed.
    })
}
fn validate_security(file: &File, user: &[usize], require_protected: bool) -> io::Result<()> {
    let mut owner = null_mut();
    let mut acl = null_mut();
    let mut existing = null_mut();
    check(unsafe { GetSecurityInfo(file.as_raw_handle(), SE_FILE_OBJECT,
        OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
        &mut owner, null_mut(), &mut acl, null_mut(), &mut existing) })?;
    let _existing = LocalAllocation(existing);
    let sid_bytes = |sid: PSID| -> io::Result<&[u8]> {
        if sid.is_null() || unsafe { IsValidSid(sid) } == 0 { return Err(denied()); }
        Ok(unsafe { std::slice::from_raw_parts(sid.cast::<u8>(), GetLengthSid(sid) as usize) })
    };
    let mut control = 0;
    let mut revision = 0;
    if unsafe { GetSecurityDescriptorControl(existing, &mut control, &mut revision) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // A NULL DACL grants everyone access; never accept or repair it.
    if acl.is_null() || unsafe { IsValidAcl(acl) } == 0 { return Err(denied()); }
    let directory = file.metadata()?.is_dir();
    let mut file_inherits_acl = false;
    let mut aces = Vec::new();
    for index in 0..unsafe { (*acl).AceCount } {
        let mut raw = null_mut();
        if unsafe { GetAce(acl, index as u32, &mut raw) } == 0 { return Err(io::Error::last_os_error()); }
        let header = unsafe { &*raw.cast::<ACE_HEADER>() };
        if header.AceType > 1 { return Err(denied()); }
        let offset = std::mem::offset_of!(ACCESS_ALLOWED_ACE, SidStart);
        if (header.AceSize as usize) < offset + 8 { return Err(denied()); }
        let sid = unsafe { raw.cast::<u8>().add(offset) };
        let length = 8 + 4 * unsafe { *sid.add(1) } as usize;
        if offset + length > header.AceSize as usize { return Err(denied()); }
        if header.AceType == 0 && u32::from(header.AceFlags) & OBJECT_INHERIT_ACE != 0 {
            file_inherits_acl = true;
        }
        aces.push((header.AceType, sid_bytes(sid.cast())?));
    }
    // Without a file-inheritable grant Windows can use the token default
    // DACL for new journals, which we have not authorized.
    if directory && !file_inherits_acl { return Err(denied()); }
    if !acl_allowed(sid_bytes(owner)?, sid_bytes(user_sid(user))?,
        !require_protected || control & SE_DACL_PROTECTED != 0,
        require_protected && !directory, aces) { return Err(denied()); }
    Ok(())
}

fn local_home(home: &Path) -> io::Result<PathBuf> {
    let mut parts = home.components();
    let drive = match parts.next() {
        // canonicalize() returns VerbatimDisk on Windows. Only that precise
        // local-drive spelling is accepted, never UNC or device namespaces.
        Some(Component::Prefix(p)) => match p.kind() {
            Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) if drive.is_ascii_alphabetic() => drive,
            _ => return Err(denied()),
        },
        _ => return Err(denied()),
    };
    if parts.next() != Some(Component::RootDir) { return Err(denied()); }
    // components() normalizes interior '.', so inspect the raw spelling too.
    if home.to_str().ok_or_else(denied)?.split(['\\', '/']).any(|s| s == "." || s == "..") {
        return Err(denied());
    }
    let mut path = PathBuf::from(format!("{}:\\", drive as char));
    let root: Vec<u16> = path.to_str().ok_or_else(denied)?.encode_utf16().chain(Some(0)).collect();
    // A drive-letter mapping can still be a network drive.
    if !matches!(unsafe { GetDriveTypeW(root.as_ptr()) }, DRIVE_FIXED | DRIVE_REMOVABLE | DRIVE_RAMDISK) {
        return Err(denied());
    }
    for part in parts {
        let Component::Normal(name) = part else { return Err(denied()); };
        crate::fs_safe::check_name(name.to_str().ok_or_else(denied)?)?;
        path.push(name);
    }
    Ok(path)
}

fn identity(file: &File) -> io::Result<(u32, u32, u32)> {
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((info.dwVolumeSerialNumber, info.nFileIndexHigh, info.nFileIndexLow))
}

fn verify_journals(directory: &SafeDir, name: &str, user: &[usize]) -> io::Result<()> {
    for suffix in ["-journal", "-wal", "-shm"] {
        if let Some(file) = directory.open_read_checked(&format!("{name}{suffix}"), true)? {
            validate_entry(&file, false)?;
            // SQLite creates these with the directory's inherited ACL. They
            // need not be protected, but must still grant only trusted SIDs.
            validate_security(&file, user, false)?;
        }
    }
    Ok(())
}

pub(super) fn open(home: &Path, name: &str) -> Result<Connection, String> {
    open_inner(home, name).map_err(|_| "secure database open unavailable".into())
}

fn open_inner(home: &Path, name: &str) -> io::Result<Connection> {
    crate::fs_safe::check_name(name)?;
    let home = local_home(home)?;
    // Never canonicalize an untrusted home: walk every component no-follow.
    let directory = SafeDir::open_nofollow(&home, false)?;
    let user = token_information(TokenUser)?;
    // Home may inherit public read grants. The protected child blocks future
    // inheritance changes from home or its ancestors, including for sidecars
    // SQLite creates after this open. Never repair an existing child's ACL.
    let security = descriptor(&user, true)?;
    let directory = directory.child_dir_secure(super::PRIVATE_DIRECTORY, &security.descriptor)?;
    validate_entry(directory.directory_file(), true)?;
    validate_security(directory.directory_file(), &user, true)?;
    let security = descriptor(&user, false)?;
    let file = directory.relative_secure(name, FILE_GENERIC_READ | FILE_GENERIC_WRITE | READ_CONTROL,
        FILE_OPEN_IF, FILE_NON_DIRECTORY_FILE, Some(&security.descriptor))?;
    validate_entry(&file, false)?;
    validate_security(&file, &user, true)?;
    verify_journals(&directory, name, &user)?;
    let path = super::database_path(&home, name);
    let connection = rusqlite::Connection::open_with_flags(&path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(|_| denied())?;
    // The main file and every ancestor have denied delete sharing since before
    // SQLite reopened the path. Check that path resolution still names our file.
    let fresh = OpenOptions::new().access_mode(FILE_READ_ATTRIBUTES | READ_CONTROL)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT).open(&path)?;
    validate_entry(&fresh, false)?;
    validate_security(&fresh, &user, true)?;
    if identity(&file)? != identity(&fresh)? { return Err(denied()); }
    verify_journals(&directory, name, &user)?;
    Ok(Connection { connection, _file: file, _directory: directory })
}

#[cfg(test)]
#[path = "sqlite_windows_tests.rs"]
mod tests;
