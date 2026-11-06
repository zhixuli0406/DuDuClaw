//! Private journal I/O: never follow a final reparse point or change a hard link's ACL.
use std::{fs::{File, OpenOptions}, io, os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::{AsRawHandle, FromRawHandle, OwnedHandle}}, path::Path, ptr::{null, null_mut}};
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
// Binary forms of S-1-5-18 and S-1-5-32-544. No token group membership
// or default-owner privileges are needed to validate an existing object.
const SYSTEM_SID: &[u8] = &[1, 1, 0, 0, 0, 0, 0, 5, 18, 0, 0, 0];
const ADMINISTRATORS_SID: &[u8] = &[1, 2, 0, 0, 0, 0, 0, 5, 32, 0, 0, 0, 32, 2, 0, 0];
fn owner_allowed(owner_sid: &[u8], token_user_sid: &[u8]) -> bool {
    owner_sid == token_user_sid || owner_sid == SYSTEM_SID || owner_sid == ADMINISTRATORS_SID
}
fn acl_allowed<'a>(owner: &[u8], user: &[u8], protected: bool,
    aces: impl IntoIterator<Item = (u8, &'a [u8])>) -> bool {
    protected && owner_allowed(owner, user) && aces.into_iter().all(|(kind, sid)| match kind {
        0 => owner_allowed(sid, user), // ACCESS_ALLOWED_ACE_TYPE
        1 => true, // ACCESS_DENIED_ACE_TYPE cannot grant access.
        _ => false, // Unsupported object/callback ACEs fail closed.
    })
}
fn validate_security(file: &File, user: &[usize]) -> io::Result<()> {
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
        aces.push((header.AceType, sid_bytes(sid.cast())?));
    }
    if !acl_allowed(sid_bytes(owner)?, sid_bytes(user_sid(user))?,
        control & SE_DACL_PROTECTED != 0, aces) { return Err(denied()); }
    Ok(())
}
fn step_error(step: &'static str, source: io::Error) -> io::Error {
    tracing::warn!(step, os_error = ?source.raw_os_error(), error = %source, "Google STT cleanup I/O failed");
    io::Error::new(source.kind(), format!("step={step} os_error={:?}: {source}", source.raw_os_error()))
}

pub(super) fn private_dir(path: &Path, create: bool) -> io::Result<File> {
    // Deferred to v1.81.0: walk from a trusted root with NtCreateFile RootDirectory +
    // OBJ_DONT_REPARSE for full ancestor-junction protection, and use handle-based temp rename.
    let user = token_information(TokenUser).map_err(|e| step_error("query_token_user", e))?;
    if create {
        let security = descriptor(&user, true).map_err(|e| step_error("directory_security_descriptor", e))?;
        let path = wide(path)?;
        let attributes = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: security.as_ptr(), bInheritHandle: 0 };
        if unsafe { CreateDirectoryW(path.as_ptr(), &attributes) } == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_ALREADY_EXISTS as i32) { return Err(step_error("create_journal_dir", error)); }
        }
    }
    // Keeping this handle prevents deletion/rename of the directory itself.
    // It allows adding/renaming files inside (required for atomic journal replacement).
    // This is not isolation from same-user attribute-only or ancestor changes.
    let file = OpenOptions::new().access_mode(FILE_GENERIC_READ)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS).open(path)
        .map_err(|e| step_error("open_journal_dir", e))?;
    validate_entry(&file, true).map_err(|e| step_error("validate_journal_dir_type", e))?;
    validate_security(&file, &user).map_err(|e| step_error("validate_journal_dir_acl", e))?;
    Ok(file)
}

pub(super) fn private_file(path: &Path, create: bool) -> io::Result<File> {
    open_file(path, create, true)
}

pub(super) fn new_temp_file(path: &Path) -> io::Result<File> {
    // tempfile's builder retries collisions; never open an existing candidate.
    open_file(path, true, false)
}

fn open_file(path: &Path, create: bool, open_existing: bool) -> io::Result<File> {
    let user = token_information(TokenUser).map_err(|e| step_error("query_token_user", e))?;
    let security = descriptor(&user, false).map_err(|e| step_error("file_security_descriptor", e))?;
    let path = wide(path)?;
    let attributes = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: security.as_ptr(), bInheritHandle: 0 };
    let mut raw = unsafe { CreateFileW(path.as_ptr(), GENERIC_READ | GENERIC_WRITE | READ_CONTROL,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        if create { &attributes } else { null() }, if create { CREATE_NEW } else { OPEN_EXISTING },
        FILE_FLAG_OPEN_REPARSE_POINT, null_mut()) };
    if create && open_existing && raw == INVALID_HANDLE_VALUE {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_FILE_EXISTS as i32) { return Err(step_error("create_journal_file", error)); }
        raw = unsafe { CreateFileW(path.as_ptr(), GENERIC_READ | GENERIC_WRITE | READ_CONTROL,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, null(), OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT, null_mut()) };
    }
    if raw == INVALID_HANDLE_VALUE { return Err(step_error("open_journal_file", io::Error::last_os_error())); }
    let file = unsafe { File::from_raw_handle(raw) };
    validate_entry(&file, false).map_err(|e| step_error("validate_journal_file_type", e))?;
    // Only inspect the opened handle. Existing owners and DACLs are never rewritten.
    validate_security(&file, &user).map_err(|e| step_error("validate_journal_file_acl", e))?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcription_google_cleanup_windows_owner_accepts_only_trusted_sids() {
        // Pure SID comparisons avoid requiring owner-changing privileges in CI.
        let user = b"token-user";
        let default_owner = ADMINISTRATORS_SID;
        let foreign = b"foreign-owner";
        assert!(owner_allowed(user, user));
        assert!(owner_allowed(default_owner, user));
        assert!(owner_allowed(SYSTEM_SID, user));
        assert!(!owner_allowed(foreign, user));
    }

    #[test]
    fn transcription_google_cleanup_windows_acl_validator_is_fail_closed() {
        let user = b"token-user".as_slice();
        let foreign = b"foreign".as_slice();
        let trusted = [(0, user), (0, SYSTEM_SID), (0, ADMINISTRATORS_SID)];
        for owner in [user, SYSTEM_SID, ADMINISTRATORS_SID] {
            assert!(acl_allowed(owner, user, true, trusted));
        }
        assert!(!acl_allowed(foreign, user, true, trusted));
        assert!(!acl_allowed(user, user, false, trusted));
        assert!(!acl_allowed(user, user, true, [(0, foreign)]));
        assert!(!acl_allowed(user, user, true, [(5, user)]));
        assert!(acl_allowed(user, user, true, [(1, foreign)]));
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
    fn transcription_google_cleanup_windows_temp_creation_is_private_and_exclusive() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("temp");
        let mut file = new_temp_file(&path).unwrap_or_else(|e| panic!("create private temp: {e:?}"));
        assert_private(&file);
        let user = token_information(TokenUser).unwrap();
        let mut owner = null_mut();
        let mut raw = null_mut();
        check(unsafe { GetSecurityInfo(file.as_raw_handle(), SE_FILE_OBJECT, OWNER_SECURITY_INFORMATION,
            &mut owner, null_mut(), null_mut(), null_mut(), &mut raw) }).unwrap();
        let _security = LocalAllocation(raw);
        assert_ne!(unsafe { EqualSid(owner, user_sid(&user)) }, 0);
        std::io::Write::write_all(&mut file, b"unchanged").unwrap();
        assert_eq!(new_temp_file(&path).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        drop(file);
        assert_eq!(std::fs::read(&path).unwrap(), b"unchanged");
        let existing = private_file(&path, false).unwrap();
        assert_private(&existing);
    }

    #[test]
    fn transcription_google_cleanup_windows_acl_and_hardlink_refusal() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("private");
        let directory = private_dir(&path, true).unwrap();
        assert_private(&directory);
        // The guard shares write for child renames, but withholds delete sharing for the directory itself.
        assert!(OpenOptions::new().access_mode(DELETE).share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
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
    fn transcription_google_cleanup_windows_directory_guard_allows_child_rename() {
        let home = tempfile::tempdir().unwrap();
        let directory = home.path().join("private");
        let _guard = private_dir(&directory, true).unwrap();
        let source = home.path().join("journal.tmp");
        let target = directory.join("journal.json");
        std::fs::write(&source, b"journal").unwrap();
        std::fs::rename(&source, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"journal");
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
        // Preparing the hard-link fixture requires a directory mutation handle.
        // Release the share guard, then reopen it for the actual validation.
        drop(_directory);
        std::fs::hard_link(home.path().join("outside"), path.join("linked")).unwrap();
        let directory = private_dir(&path, false).unwrap_or_else(|e| panic!("reopen cleanup directory: {e:?}"));
        assert_private(&directory);
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
