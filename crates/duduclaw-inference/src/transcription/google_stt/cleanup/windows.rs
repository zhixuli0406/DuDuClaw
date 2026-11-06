//! Private journal I/O: never follow a final reparse point or change a hard link's ACL.
use std::{fs::{File, OpenOptions}, io, os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::{AsRawHandle, FromRawHandle, OwnedHandle}}, path::Path, ptr::{null, null_mut}};
use windows_sys::Wdk::Storage::FileSystem::NtSetSecurityObject;
use windows_sys::Win32::{Foundation::*, Security::{*, Authorization::*}, Storage::FileSystem::*, System::Threading::*};

struct LocalAllocation(*mut std::ffi::c_void);
impl Drop for LocalAllocation { fn drop(&mut self) { unsafe { LocalFree(self.0); } } }

fn denied() -> io::Error { io::Error::new(io::ErrorKind::PermissionDenied, "unsafe cleanup entry") }
fn check(status: u32) -> io::Result<()> {
    if status == ERROR_SUCCESS { Ok(()) } else { Err(io::Error::from_raw_os_error(status as i32)) }
}
fn wide(path: &Path) -> io::Result<Vec<u16>> {
    let mut value: Vec<u16> = path.as_os_str().encode_wide().collect();
    if value.contains(&0) { return Err(denied()); }
    value.push(0);
    Ok(value)
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
    // The absolute descriptor borrows its DACL from this allocation.
    _dacl: LocalAllocation,
}
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
    let sddl: Vec<u16> = format!("D:P(A;{flags};FA;;;{sid})(A;{flags};FA;;;SY)(A;{flags};FA;;;BA)")
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
    let mut descriptor: SECURITY_DESCRIPTOR = unsafe { std::mem::zeroed() };
    let raw = std::ptr::addr_of_mut!(descriptor).cast();
    if unsafe { InitializeSecurityDescriptor(raw, 1) } == 0 { return Err(io::Error::last_os_error()); }
    // Explicit TokenUser ownership also works with elevated tokens whose default
    // owner is Administrators. Any failure (including ERROR_INVALID_OWNER) is fatal.
    if unsafe { SetSecurityDescriptorOwner(raw, user_sid(user), 0) } == 0 { return Err(io::Error::last_os_error()); }
    if unsafe { SetSecurityDescriptorDacl(raw, 1, acl, 0) } == 0 { return Err(io::Error::last_os_error()); }
    if unsafe { SetSecurityDescriptorControl(raw, SE_DACL_PROTECTED, SE_DACL_PROTECTED) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(SecurityDescriptor { descriptor, _dacl: allocation })
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
fn owner_allowed(owner_sid: &[u8], token_user_sid: &[u8]) -> bool {
    owner_sid == token_user_sid
}
fn protect(file: &File, user: &[usize], security: &SecurityDescriptor) -> io::Result<()> {
    // All objects must belong to TokenUser before changing their DACL.
    let mut owner = null_mut();
    let mut existing = null_mut();
    check(unsafe { GetSecurityInfo(file.as_raw_handle(), SE_FILE_OBJECT, OWNER_SECURITY_INFORMATION,
        &mut owner, null_mut(), null_mut(), null_mut(), &mut existing) })?;
    let _existing = LocalAllocation(existing);
    let sid_bytes = |sid: PSID| -> io::Result<&[u8]> {
        if sid.is_null() || unsafe { IsValidSid(sid) } == 0 { return Err(denied()); }
        Ok(unsafe { std::slice::from_raw_parts(sid.cast::<u8>(), GetLengthSid(sid) as usize) })
    };
    if !owner_allowed(sid_bytes(owner)?, sid_bytes(user_sid(user))?) {
        return Err(denied());
    }
    // SetSecurityInfo would recursively propagate inheritable ACEs to existing
    // children before their reparse/hardlink checks. Only change this handle.
    let status = unsafe { NtSetSecurityObject(file.as_raw_handle(),
        DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION, security.as_ptr()) };
    if status < 0 {
        Err(io::Error::from_raw_os_error(unsafe { RtlNtStatusToDosError(status) } as i32))
    } else { Ok(()) }
}

pub(super) fn private_dir(path: &Path, create: bool) -> io::Result<File> {
    // Deferred to v1.81.0: walk from a trusted root with NtCreateFile RootDirectory +
    // OBJ_DONT_REPARSE for full ancestor-junction protection, and use handle-based temp rename.
    let user = token_information(TokenUser)?;
    let security = descriptor(&user, true)?;
    if create {
        let path = wide(path)?;
        let attributes = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: security.as_ptr(), bInheritHandle: 0 };
        if unsafe { CreateDirectoryW(path.as_ptr(), &attributes) } == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_ALREADY_EXISTS as i32) { return Err(error); }
        }
    }
    // Keeping this handle prevents deletion/rename and denies data-write opens.
    // This is not isolation from same-user attribute-only or ancestor changes.
    let file = OpenOptions::new().access_mode(FILE_GENERIC_READ | WRITE_DAC)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS).open(path)?;
    validate_entry(&file, true)?;
    protect(&file, &user, &security)?;
    Ok(file)
}

pub(super) fn private_file(path: &Path, create: bool) -> io::Result<File> {
    let user = token_information(TokenUser)?;
    let security = descriptor(&user, false)?;
    let path = wide(path)?;
    let attributes = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: security.as_ptr(), bInheritHandle: 0 };
    let mut raw = unsafe { CreateFileW(path.as_ptr(), GENERIC_READ | GENERIC_WRITE | READ_CONTROL | WRITE_DAC,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        if create { &attributes } else { null() }, if create { CREATE_NEW } else { OPEN_EXISTING },
        FILE_FLAG_OPEN_REPARSE_POINT, null_mut()) };
    if create && raw == INVALID_HANDLE_VALUE {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_FILE_EXISTS as i32) { return Err(error); }
        raw = unsafe { CreateFileW(path.as_ptr(), GENERIC_READ | GENERIC_WRITE | READ_CONTROL | WRITE_DAC,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, null(), OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT, null_mut()) };
    }
    if raw == INVALID_HANDLE_VALUE { return Err(io::Error::last_os_error()); }
    let file = unsafe { File::from_raw_handle(raw) };
    validate_entry(&file, false)?;
    // Validate and protect the very same handle returned by CREATE_NEW; no reopen.
    protect(&file, &user, &security)?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcription_google_cleanup_windows_owner_requires_token_user() {
        // Pure SID comparisons avoid requiring owner-changing privileges in CI.
        let user = b"token-user";
        let default_owner = b"administrators";
        let foreign = b"foreign-owner";
        assert!(owner_allowed(user, user));
        assert!(!owner_allowed(default_owner, user));
        assert!(!owner_allowed(foreign, user));
    }

    fn assert_private(file: &File) {
        let mut acl = null_mut();
        let mut raw = null_mut();
        check(unsafe { GetSecurityInfo(file.as_raw_handle(), SE_FILE_OBJECT, DACL_SECURITY_INFORMATION,
            null_mut(), null_mut(), &mut acl, null_mut(), &mut raw) }).unwrap();
        let _security = LocalAllocation(raw);
        let mut control = 0;
        let mut revision = 0;
        assert_ne!(unsafe { GetSecurityDescriptorControl(raw, &mut control, &mut revision) }, 0);
        assert_ne!(control & SE_DACL_PROTECTED, 0);
        assert!(!acl.is_null());
        assert_eq!(unsafe { (*acl).AceCount }, 3);
        let user = token_information(TokenUser).unwrap();
        for index in 0..3 {
            let mut raw_ace = null_mut();
            assert_ne!(unsafe { GetAce(acl, index, &mut raw_ace) }, 0);
            let ace = unsafe { &*raw_ace.cast::<ACCESS_ALLOWED_ACE>() };
            assert_eq!(ace.Header.AceType, 0);
            assert_eq!(ace.Mask, FILE_ALL_ACCESS);
            let sid = std::ptr::addr_of!(ace.SidStart).cast_mut().cast();
            assert!(unsafe { EqualSid(sid, user_sid(&user)) != 0
                || IsWellKnownSid(sid, WinLocalSystemSid) != 0
                || IsWellKnownSid(sid, WinBuiltinAdministratorsSid) != 0 });
        }
    }

    #[test]
    fn transcription_google_cleanup_windows_acl_and_hardlink_refusal() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("private");
        let directory = private_dir(&path, true).unwrap();
        assert_private(&directory);
        assert!(OpenOptions::new().write(true).share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT).open(&path).is_err());
        let entry = path.join("record");
        let file = private_file(&entry, true).unwrap();
        assert_private(&file);
        drop(file);
        let link = home.path().join("alias");
        std::fs::hard_link(&entry, &link).unwrap();
        assert!(private_file(&entry, false).is_err());
        std::fs::remove_file(&link).unwrap();
        let file = private_file(&entry, false).unwrap();
        assert_private(&file);
    }

    #[test]
    fn transcription_google_cleanup_windows_directory_acl_does_not_change_existing_children() {
        fn dacl(file: &File) -> Vec<u8> {
            let mut acl = null_mut();
            let mut raw = null_mut();
            check(unsafe { GetSecurityInfo(file.as_raw_handle(), SE_FILE_OBJECT, DACL_SECURITY_INFORMATION,
                null_mut(), null_mut(), &mut acl, null_mut(), &mut raw) }).unwrap();
            let _security = LocalAllocation(raw);
            assert!(!acl.is_null());
            unsafe { std::slice::from_raw_parts(acl.cast::<u8>(), (*acl).AclSize as usize).to_vec() }
        }
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("private");
        let _directory = private_dir(&path, true).unwrap();
        let outside = home.path().join("outside");
        std::fs::write(&outside, b"unchanged").unwrap();
        let outside = File::open(&outside).unwrap();
        let before = dacl(&outside);
        std::fs::hard_link(home.path().join("outside"), path.join("linked")).unwrap();
        drop(private_dir(&path, false).unwrap());
        assert_eq!(dacl(&outside), before);
        assert!(private_file(&path.join("linked"), false).is_err());
        assert_eq!(dacl(&outside), before);
    }

    #[test]
    fn transcription_google_cleanup_windows_reparse_points_are_refused() {
        use std::os::windows::fs::{symlink_dir, symlink_file};
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let directory = home.path().join("journal");
        if let Err(error) = symlink_dir(outside.path(), &directory) {
            if error.raw_os_error() == Some(1314) {
                eprintln!("SKIP: Windows symlink creation privilege unavailable");
                return;
            }
            panic!("could not create directory reparse point: {error}");
        }
        assert!(private_dir(&directory, false).is_err());
        std::fs::remove_dir(&directory).unwrap();
        let _directory = private_dir(&directory, true).unwrap();
        let target = outside.path().join("target");
        std::fs::write(&target, b"unchanged").unwrap();
        let entry = directory.join("record");
        symlink_file(&target, &entry).unwrap();
        assert!(private_file(&entry, false).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"unchanged");
    }
}
