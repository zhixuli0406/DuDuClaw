//! Handle-based key-file checks. Path metadata is not an authorization check.

use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    GetSecurityInfo, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, EqualSid, GetAce,
    GetTokenInformation, INHERIT_ONLY_ACE, IsValidAcl, IsValidSid, IsWellKnownSid,
    OWNER_SECURITY_INFORMATION, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser, WinBuiltinAdministratorsSid, WinLocalSystemSid,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_DIRECTORY,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TYPE_DISK, GetFileInformationByHandle,
    GetFileType, OPEN_EXISTING, READ_CONTROL,
};
use windows_sys::Win32::System::SystemServices::{ACCESS_ALLOWED_ACE_TYPE, ACCESS_DENIED_ACE_TYPE};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

struct LocalAllocation(*mut core::ffi::c_void);
impl Drop for LocalAllocation {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: these pointers come only from APIs documented to use LocalFree.
            unsafe { LocalFree(self.0); }
        }
    }
}

fn denied(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

// A pointer-aligned buffer owns the TOKEN_USER and its variable-length SID.
fn current_user() -> io::Result<Vec<usize>> {
    let mut raw = null_mut();
    // SAFETY: valid process pseudo-handle and output pointer; token is owned below.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut needed = 0;
    unsafe { GetTokenInformation(token.as_raw_handle(), TokenUser, null_mut(), 0, &mut needed); }
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buffer = vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
    if unsafe {
        GetTokenInformation(token.as_raw_handle(), TokenUser,
            buffer.as_mut_ptr().cast(), needed, &mut needed)
    } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(buffer)
}

fn user_sid(user: &[usize]) -> PSID {
    // SAFETY: only the buffer returned by current_user is passed here.
    unsafe { (*(user.as_ptr().cast::<TOKEN_USER>())).User.Sid }
}

fn descriptor(sddl: &str) -> io::Result<LocalAllocation> {
    let wide: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
    let mut raw = null_mut();
    // SAFETY: NUL-terminated input and valid output pointer; revision 1 is SDDL_REVISION_1.
    if unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(wide.as_ptr(), 1, &mut raw, null_mut()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(LocalAllocation(raw))
}

fn private_descriptor(user: &[usize]) -> io::Result<LocalAllocation> {
    let mut raw = null_mut();
    if unsafe { ConvertSidToStringSidW(user_sid(user), &mut raw) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let allocated = LocalAllocation(raw.cast());
    let mut len = 0;
    // SAFETY: ConvertSidToStringSidW returns a NUL-terminated string.
    while unsafe { *raw.add(len) } != 0 { len += 1; }
    let sid = String::from_utf16(unsafe { std::slice::from_raw_parts(raw, len) })
        .map_err(|_| io::Error::other("invalid current-user SID"))?;
    drop(allocated);
    // Do not inherit a permissive DACL from the containing directory.
    descriptor(&format!("O:{sid}D:P(A;;FA;;;{sid})(A;;FA;;;SY)(A;;FA;;;BA)"))
}

pub(super) fn open(path: &Path, write: bool, create_new: bool) -> io::Result<File> {
    let user = current_user()?;
    let security = if create_new { Some(private_descriptor(&user)?) } else { None };
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: security.as_ref().map_or(null_mut(), |sd| sd.0),
        bInheritHandle: 0,
    };
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    if wide.iter().take(wide.len() - 1).any(|c| *c == 0) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "NUL in identity key path"));
    }
    // SAFETY: pointers remain alive for this call. OPEN_REPARSE_POINT opens the
    // final component itself; even a replacement after a path check is inspected.
    let raw = unsafe {
        CreateFileW(wide.as_ptr(), GENERIC_READ | READ_CONTROL | if write { GENERIC_WRITE } else { 0 },
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            if create_new { &attributes } else { null() },
            if create_new { CREATE_NEW } else { OPEN_EXISTING },
            FILE_FLAG_OPEN_REPARSE_POINT, null_mut())
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let file = unsafe { File::from_raw_handle(raw) };
    validate(&file, &user)?;
    Ok(file)
}

fn validate(file: &File, user: &[usize]) -> io::Result<()> {
    let handle = file.as_raw_handle();
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileType(handle) } != FILE_TYPE_DISK {
        return Err(denied("identity key must be a disk file"));
    }
    if unsafe { GetFileInformationByHandle(handle, &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if info.dwFileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
        return Err(denied("identity key must be a regular file, not a reparse point"));
    }
    let mut owner = null_mut();
    let mut dacl: *mut ACL = null_mut();
    let mut raw = null_mut();
    let status = unsafe {
        GetSecurityInfo(handle, SE_FILE_OBJECT, OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner, null_mut(), &mut dacl, null_mut(), &mut raw)
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let _security = LocalAllocation(raw);
    if owner.is_null() || unsafe { IsValidSid(owner) } == 0
        || unsafe { EqualSid(owner, user_sid(user)) } == 0
    {
        return Err(denied("identity key must be owned by the current account"));
    }
    if dacl.is_null() || unsafe { IsValidAcl(dacl) } == 0 {
        return Err(denied("identity key must have a restrictive DACL"));
    }
    // Unknown ACE formats fail closed instead of guessing their access effect.
    for index in 0..unsafe { (*dacl).AceCount } as u32 {
        let mut ace = null_mut();
        if unsafe { GetAce(dacl, index, &mut ace) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let header = unsafe { &*ace.cast::<ACE_HEADER>() };
        if u32::from(header.AceFlags) & INHERIT_ONLY_ACE != 0
            || u32::from(header.AceType) == ACCESS_DENIED_ACE_TYPE
        {
            continue;
        }
        if u32::from(header.AceType) != ACCESS_ALLOWED_ACE_TYPE
            || usize::from(header.AceSize) < std::mem::size_of::<ACCESS_ALLOWED_ACE>()
        {
            return Err(denied("identity key has an unsupported access-control entry"));
        }
        let allowed = ace.cast::<ACCESS_ALLOWED_ACE>();
        let sid = unsafe { std::ptr::addr_of!((*allowed).SidStart) }.cast_mut().cast();
        if unsafe { IsValidSid(sid) } == 0
            || (unsafe { EqualSid(sid, owner) } == 0
                && unsafe { IsWellKnownSid(sid, WinLocalSystemSid) } == 0
                && unsafe { IsWellKnownSid(sid, WinBuiltinAdministratorsSid) } == 0)
        {
            return Err(denied("identity key grants access outside its owner, SYSTEM and Administrators"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::windows::fs::{OpenOptionsExt, symlink_file};
    use windows_sys::Win32::Security::{GetSecurityDescriptorDacl, PROTECTED_DACL_SECURITY_INFORMATION};
    use windows_sys::Win32::Security::Authorization::SetSecurityInfo;
    use windows_sys::Win32::Storage::FileSystem::WRITE_DAC;

    fn set_dacl(path: &Path, sddl: Option<&str>) {
        let file = std::fs::OpenOptions::new().read(true)
            .access_mode(GENERIC_READ | READ_CONTROL | WRITE_DAC).open(path).unwrap();
        let sd = sddl.map(|text| descriptor(text).unwrap());
        let mut dacl = null_mut();
        if let Some(sd) = &sd {
            let mut present = 0;
            let mut defaulted = 0;
            assert_ne!(unsafe { GetSecurityDescriptorDacl(sd.0, &mut present, &mut dacl, &mut defaulted) }, 0);
            assert_ne!(present, 0);
        }
        assert_eq!(unsafe {
            SetSecurityInfo(file.as_raw_handle(), SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                null_mut(), null_mut(), dacl, null())
        }, 0);
    }

    #[test]
    fn windows_private_key_is_restrictive_at_creation_and_never_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key");
        let mut file = open(&path, true, true).unwrap();
        // Validate before writing any secret bytes.
        validate(&file, &current_user().unwrap()).unwrap();
        file.write_all(&[7; 32]).unwrap();
        file.sync_all().unwrap();
        assert!(open(&path, true, true).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), vec![7; 32]);
        assert!(open(&path, false, false).is_ok());
    }

    #[test]
    fn windows_world_readable_and_null_dacl_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key");
        drop(open(&path, true, true).unwrap());
        set_dacl(&path, Some("D:P(A;;FA;;;WD)"));
        assert!(open(&path, false, false).is_err());
        set_dacl(&path, None);
        assert!(open(&path, false, false).is_err());
    }

    #[test]
    fn windows_reparse_replacement_after_metadata_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key");
        let target = dir.path().join("target");
        drop(open(&path, true, true).unwrap());
        drop(open(&target, true, true).unwrap());
        // Simulate a replacement after the caller's path metadata check.
        assert!(std::fs::symlink_metadata(&path).unwrap().is_file());
        std::fs::remove_file(&path).unwrap();
        if let Err(error) = symlink_file(&target, &path) {
            if error.raw_os_error() == Some(1314) {
                eprintln!("SKIP: symlink creation requires Windows Developer Mode or SeCreateSymbolicLinkPrivilege");
                return;
            }
            panic!("could not create test reparse point: {error}");
        }
        assert!(open(&path, false, false).is_err());
    }
}
