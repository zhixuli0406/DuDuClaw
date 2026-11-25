//! Local named pipes with a protected DACL containing only the current user SID.
use std::{io, os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle}};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows_sys::Win32::{Foundation::LocalFree, Security::{GetTokenInformation, TokenUser,
    TOKEN_QUERY, TOKEN_USER, SECURITY_ATTRIBUTES, Authorization::{ConvertSidToStringSidW,
    ConvertStringSecurityDescriptorToSecurityDescriptorW}}, System::Threading::{GetCurrentProcess, OpenProcessToken}};

struct LocalAllocation(*mut std::ffi::c_void);
impl Drop for LocalAllocation {
    fn drop(&mut self) { unsafe { LocalFree(self.0); } }
}

pub(super) fn create(name: &str, first: bool) -> io::Result<NamedPipeServer> {
    let mut raw = std::ptr::null_mut();
    // SAFETY: current process pseudo-handle and writable output handle.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut length = 0;
    unsafe { GetTokenInformation(token.as_raw_handle(), TokenUser, std::ptr::null_mut(), 0, &mut length); }
    if length < std::mem::size_of::<TOKEN_USER>() as u32 { return Err(io::ErrorKind::InvalidData.into()); }
    // usize storage provides alignment for TOKEN_USER and the variable-length SID.
    let mut buffer = vec![0usize; (length as usize).div_ceil(std::mem::size_of::<usize>())];
    if unsafe { GetTokenInformation(token.as_raw_handle(), TokenUser, buffer.as_mut_ptr().cast(), length, &mut length) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let mut sid = std::ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut sid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let _sid = LocalAllocation(sid.cast());
    let mut count = 0;
    while unsafe { *sid.add(count) } != 0 { count += 1; }
    let sid_text = String::from_utf16(unsafe { std::slice::from_raw_parts(sid, count) })
        .map_err(|_| io::ErrorKind::InvalidData)?;
    let sddl: Vec<u16> = format!("D:P(A;;GA;;;{sid_text})").encode_utf16().chain(Some(0)).collect();
    let mut descriptor = std::ptr::null_mut();
    if unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), 1, &mut descriptor, std::ptr::null_mut()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let _descriptor = LocalAllocation(descriptor);
    let mut attributes = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor, bInheritHandle: 0 };
    // All pointers remain live until CreateNamedPipe returns. Tokio adds OVERLAPPED;
    // remote clients are rejected by the kernel, even if they share the user SID.
    unsafe { ServerOptions::new().first_pipe_instance(first).reject_remote_clients(true)
        .max_instances(65).create_with_security_attributes_raw(name,
            (&mut attributes as *mut SECURITY_ATTRIBUTES).cast()) }
}
