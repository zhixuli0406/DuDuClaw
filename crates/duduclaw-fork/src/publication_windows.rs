//! Windows host-only publication uses a protected global kernel mutex.
//! The name derives from the token SID and the pinned volume/file identity;
//! no ambient directory or workspace file can split or replace this lock.

use std::{fs::{File, OpenOptions}, marker::PhantomData, os::windows::{fs::OpenOptionsExt, io::AsRawHandle}, path::Path, rc::Rc};
use sha2::{Digest, Sha256};
use windows_sys::Win32::{Foundation::*, Security::{*, Authorization::*}, Storage::FileSystem::*, System::Threading::*};
use super::{ParentPublication, ForkError, Result};

struct Handle(HANDLE);
impl Drop for Handle { fn drop(&mut self) { unsafe { CloseHandle(self.0); } } }
struct LocalAllocation(*mut std::ffi::c_void);
impl Drop for LocalAllocation { fn drop(&mut self) { unsafe { LocalFree(self.0); } } }

// A mutex belongs to its acquiring OS thread. Rc makes this guard !Send/!Sync.
struct MutexGuard { handle: Handle, _thread: PhantomData<Rc<()>> }
impl Drop for MutexGuard { fn drop(&mut self) { unsafe { ReleaseMutex(self.handle.0); } } }

fn os_error(context: &str) -> ForkError {
    ForkError::Overlay(format!("{context}: {}", std::io::Error::last_os_error()))
}

fn pin_parent(path: &Path) -> Result<(File, Vec<u8>)> {
    pin_parent_with_access(path, FILE_GENERIC_READ)
}

pub(super) fn parent_identity(path: &Path) -> Result<Vec<u8>> { pin_parent(path).map(|(_, identity)| identity) }

fn pin_parent_with_access(path: &Path, access: u32) -> Result<(File, Vec<u8>)> {
    // Omitting FILE_SHARE_DELETE pins the destination against replacement for
    // the entire wait and publication, while allowing normal file reads/writes.
    let file = OpenOptions::new().access_mode(access).share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path).map_err(|error| ForkError::Overlay(format!("pin publication parent: {error}")))?;
    let mut basic: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    let mut id: FILE_ID_INFO = unsafe { std::mem::zeroed() };
    unsafe {
        if GetFileInformationByHandle(file.as_raw_handle(), &mut basic) == 0
            || GetFileInformationByHandleEx(file.as_raw_handle(), FileIdInfo,
                (&mut id as *mut FILE_ID_INFO).cast(), std::mem::size_of::<FILE_ID_INFO>() as u32) == 0 {
            return Err(os_error("read publication parent identity"));
        }
    }
    if basic.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0
        || basic.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(ForkError::Overlay("publication parent must be a pinned directory, not a reparse point".into()));
    }
    let mut key = id.VolumeSerialNumber.to_le_bytes().to_vec();
    key.extend_from_slice(&id.FileId.Identifier);
    Ok((file, key))
}

fn current_user() -> Result<(Vec<usize>, String)> {
    let mut raw = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) } == 0 {
        return Err(os_error("open publication user token"));
    }
    let token = Handle(raw);
    let mut needed = 0;
    unsafe { GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut needed); }
    if needed < std::mem::size_of::<TOKEN_USER>() as u32 { return Err(os_error("size publication user token")); }
    // usize allocation keeps TOKEN_USER and its trailing SID correctly aligned.
    let mut data = vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
    if unsafe { GetTokenInformation(token.0, TokenUser, data.as_mut_ptr().cast(), needed, &mut needed) } == 0 {
        return Err(os_error("read publication user token"));
    }
    let sid = unsafe { (*(data.as_ptr().cast::<TOKEN_USER>())).User.Sid };
    let mut text = std::ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 { return Err(os_error("format publication user SID")); }
    let allocation = LocalAllocation(text.cast());
    let mut len = 0;
    unsafe { while *text.add(len) != 0 { len += 1; } }
    let name = unsafe { String::from_utf16(std::slice::from_raw_parts(text, len)) }
        .map_err(|error| ForkError::Overlay(error.to_string()))?;
    drop(allocation);
    Ok((data, name))
}

fn descriptor(sddl: &str) -> Result<LocalAllocation> {
    let text: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
    let mut raw = std::ptr::null_mut();
    if unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(text.as_ptr(), SDDL_REVISION_1,
        &mut raw, std::ptr::null_mut()) } == 0 { return Err(os_error("create protected publication ACL")); }
    Ok(LocalAllocation(raw))
}

fn validate_security(handle: HANDLE, sid: PSID, object: SE_OBJECT_TYPE, mask: u32, flags: u8) -> Result<()> {
    let mut owner = std::ptr::null_mut();
    let mut acl = std::ptr::null_mut();
    let mut raw = std::ptr::null_mut();
    let status = unsafe { GetSecurityInfo(handle, object,
        OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION, &mut owner,
        std::ptr::null_mut(), &mut acl, std::ptr::null_mut(), &mut raw) };
    if status != ERROR_SUCCESS { return Err(ForkError::Overlay(format!("read publication mutex security: {status}"))); }
    let allocation = LocalAllocation(raw);
    let mut control = 0;
    let mut revision = 0;
    let mut ace = std::ptr::null_mut();
    let valid = unsafe {
        !owner.is_null() && IsValidSid(owner) != 0 && EqualSid(owner, sid) != 0
            && GetSecurityDescriptorControl(raw, &mut control, &mut revision) != 0
            && control & SE_DACL_PROTECTED != 0 && !acl.is_null() && (*acl).AceCount == 1
            && GetAce(acl, 0, &mut ace) != 0 && !ace.is_null()
    };
    let valid = valid && unsafe {
        let header = &*ace.cast::<ACE_HEADER>();
        if header.AceType != 0 || header.AceSize < std::mem::size_of::<ACCESS_ALLOWED_ACE>() as u16 { return Err(ForkError::Overlay("publication object has an invalid ACE".into())); }
        let allowed = &*ace.cast::<ACCESS_ALLOWED_ACE>();
        // ACCESS_ALLOWED_ACE_TYPE is zero; no inherited/object/callback ACEs.
        allowed.Header.AceType == 0 && allowed.Header.AceFlags == flags
            && allowed.Mask == mask
            && IsValidSid(std::ptr::addr_of!(allowed.SidStart).cast_mut().cast()) != 0
            && EqualSid(std::ptr::addr_of!(allowed.SidStart).cast_mut().cast(), sid) != 0
    };
    drop(allocation);
    if !valid { return Err(ForkError::Overlay("publication mutex has unsafe owner or ACL".into())); }
    Ok(())
}

fn validate_mutex(handle: HANDLE, sid: PSID) -> Result<()> {
    validate_security(handle, sid, SE_KERNEL_OBJECT, MUTEX_ALL_ACCESS, 0)
}

pub(super) fn check_private_directory(path: &Path) -> Result<()> {
    let (user, _) = current_user()?;
    let sid = unsafe { (*(user.as_ptr().cast::<TOKEN_USER>())).User.Sid };
    let (directory, _) = pin_parent(path)?;
    validate_security(directory.as_raw_handle(), sid, SE_FILE_OBJECT, FILE_ALL_ACCESS,
        (OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE) as u8)
}

pub(super) fn make_private_directory(path: &Path) -> Result<()> {
    let (user, user_name) = current_user()?;
    let sid = unsafe { (*(user.as_ptr().cast::<TOKEN_USER>())).User.Sid };
    let security = descriptor(&format!("O:{user_name}D:P(A;OICI;0x001F01FF;;;{user_name})"))?;
    let attributes = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: security.0, bInheritHandle: 0 };
    if !path.exists() {
        if let Some(parent) = path.parent().filter(|parent| !parent.exists()) { make_private_directory(parent)?; }
        use std::os::windows::ffi::OsStrExt;
        let text: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        if unsafe { CreateDirectoryW(text.as_ptr(), &attributes) } == 0
            && unsafe { GetLastError() } != ERROR_ALREADY_EXISTS {
            return Err(os_error("create private recovery directory"));
        }
    }
    // Only tighten an owned real directory; never follow a junction or change
    // a foreign owner's entry while preparing private retained/recovery data.
    let (directory, _) = pin_parent_with_access(path, FILE_GENERIC_READ | WRITE_DAC)?;
    let mut owner = std::ptr::null_mut();
    let mut existing = std::ptr::null_mut();
    let status = unsafe { GetSecurityInfo(directory.as_raw_handle(), SE_FILE_OBJECT,
        OWNER_SECURITY_INFORMATION, &mut owner, std::ptr::null_mut(), std::ptr::null_mut(),
        std::ptr::null_mut(), &mut existing) };
    if status != ERROR_SUCCESS { return Err(ForkError::Overlay(format!("read recovery owner: {status}"))); }
    let existing = LocalAllocation(existing);
    if owner.is_null() || unsafe { EqualSid(owner, sid) } == 0 {
        return Err(ForkError::Overlay("recovery directory has a foreign owner".into()));
    }
    drop(existing);
    let mut present = 0;
    let mut defaulted = 0;
    let mut acl = std::ptr::null_mut();
    if unsafe { GetSecurityDescriptorDacl(security.0, &mut present, &mut acl, &mut defaulted) } == 0 || present == 0 || acl.is_null() {
        return Err(os_error("read private recovery ACL"));
    }
    let status = unsafe { SetSecurityInfo(directory.as_raw_handle(), SE_FILE_OBJECT,
        DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
        std::ptr::null_mut(), std::ptr::null_mut(), acl, std::ptr::null_mut()) };
    if status != ERROR_SUCCESS { return Err(ForkError::Overlay(format!("protect recovery directory: {status}"))); }
    validate_security(directory.as_raw_handle(), sid, SE_FILE_OBJECT, FILE_ALL_ACCESS,
        (OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE) as u8)
}

pub(super) fn with_parent_publication<T>(parent: &Path,
    action: impl FnOnce(&ParentPublication) -> Result<T>) -> Result<T> {
    let parent = parent.canonicalize().map_err(|error| ForkError::Overlay(format!("canonicalize publication parent: {error}")))?;
    let (_pinned, identity) = pin_parent(&parent)?;
    let (user, user_name) = current_user()?;
    let sid = unsafe { (*(user.as_ptr().cast::<TOKEN_USER>())).User.Sid };
    let name = format!("Global\\DuDuClawFork-{:x}-{:x}", Sha256::digest(user_name.as_bytes()), Sha256::digest(&identity));
    let security = descriptor(&format!("O:{user_name}D:P(A;;0x001F0001;;;{user_name})"))?;
    let attributes = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: security.0, bInheritHandle: 0 };
    let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let raw = unsafe { CreateMutexW(&attributes, 0, name.as_ptr()) };
    if raw.is_null() { return Err(os_error("open global publication mutex")); }
    let handle = Handle(raw);
    validate_mutex(handle.0, sid)?;
    match unsafe { WaitForSingleObject(handle.0, INFINITE) } {
        WAIT_OBJECT_0 | WAIT_ABANDONED => {},
        _ => return Err(os_error("wait for publication mutex")),
    }
    let guard = MutexGuard { handle, _thread: PhantomData };
    validate_mutex(guard.handle.0, sid)?;
    let (_current, current_identity) = pin_parent(&parent)?;
    if current_identity != identity { return Err(ForkError::Overlay("publication parent identity changed".into())); }
    action(&ParentPublication { parent, identity })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_owner_mutex_and_private_recovery_directory_are_validated() {
        let (user, user_name) = current_user().unwrap();
        let sid = unsafe { (*(user.as_ptr().cast::<TOKEN_USER>())).User.Sid };
        for protected in [true, false] {
            let marker = if protected { "P" } else { "" };
            let security = descriptor(&format!("O:{user_name}D:{marker}(A;;0x001F0001;;;{user_name})")).unwrap();
            let attributes = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: security.0, bInheritHandle: 0 };
            let name: Vec<u16> = format!("Global\\DuDuClawFork-Test-{}", uuid::Uuid::new_v4()).encode_utf16().chain(Some(0)).collect();
            let raw = unsafe { CreateMutexW(&attributes, 0, name.as_ptr()) };
            assert!(!raw.is_null());
            let handle = Handle(raw);
            assert_eq!(validate_mutex(handle.0, sid).is_ok(), protected);
        }
        let home = tempfile::tempdir().unwrap();
        let directory = home.path().join("recovery");
        make_private_directory(&directory).unwrap();
        check_private_directory(&directory).unwrap();
        std::fs::write(directory.join("source.txt"), "private source").unwrap();
        make_private_directory(&directory).unwrap();
        check_private_directory(&directory).unwrap();
    }

    #[test]
    fn namespace_squatter_with_broad_acl_is_rejected() {
        let (user, user_name) = current_user().unwrap();
        let sid = unsafe { (*(user.as_ptr().cast::<TOKEN_USER>())).User.Sid };
        let security = descriptor(&format!("O:{user_name}D:P(A;;0x001F0001;;;WD)" )).unwrap();
        let attributes = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: security.0, bInheritHandle: 0 };
        let name: Vec<u16> = format!("Global\\DuDuClawFork-Test-{}", uuid::Uuid::new_v4()).encode_utf16().chain(Some(0)).collect();
        let raw = unsafe { CreateMutexW(&attributes, 0, name.as_ptr()) };
        assert!(!raw.is_null());
        let handle = Handle(raw);
        assert!(validate_mutex(handle.0, sid).is_err());
    }
}
