//! Fixed root-owned seqpacket endpoint; bounded frames, no passed descriptors.
use std::{os::fd::{AsRawFd, FromRawFd, OwnedFd}, os::unix::fs::MetadataExt, path::Path};
use serde::{Serialize, de::DeserializeOwned};
use super::*;
fn invalid<T>() -> Result<T, Error> { Err(Error::Authentication) }
fn directory() -> Result<(), Error> {
    let path=Path::new("/run/duduclaw-spawnd");
    for part in path.ancestors() {
        let m=std::fs::symlink_metadata(part)?;
        if !m.is_dir() || m.uid()!=0 || m.mode()&0o022!=0 { return invalid(); }
    }
    Ok(())
}
fn socket() -> Result<OwnedFd, Error> {
    let raw=unsafe {libc::socket(libc::AF_UNIX,libc::SOCK_SEQPACKET|libc::SOCK_CLOEXEC,0)};
    if raw<0 {return Err(Error::Unavailable)}
    let fd=unsafe{OwnedFd::from_raw_fd(raw)};
    timeout(&fd)?; Ok(fd)
}
fn timeout(fd:&OwnedFd)->Result<(),Error>{
    let tv=libc::timeval{tv_sec:5,tv_usec:0};
    for opt in [libc::SO_RCVTIMEO,libc::SO_SNDTIMEO] {
        if unsafe{libc::setsockopt(fd.as_raw_fd(),libc::SOL_SOCKET,opt,(&tv as *const libc::timeval).cast(),std::mem::size_of_val(&tv) as _)}!=0 {return Err(Error::Unavailable)}
    } Ok(())
}
fn address()->libc::sockaddr_un{
    let mut addr:libc::sockaddr_un=unsafe{std::mem::zeroed()};addr.sun_family=libc::AF_UNIX as _;
    for (dst,src) in addr.sun_path.iter_mut().zip(SOCKET.bytes()){*dst=src as _;}addr
}
fn peer(fd:&OwnedFd)->Result<(u32,u32),Error>{
    let mut cred:libc::ucred=unsafe{std::mem::zeroed()};let mut len=std::mem::size_of_val(&cred) as libc::socklen_t;
    if unsafe{libc::getsockopt(fd.as_raw_fd(),libc::SOL_SOCKET,libc::SO_PEERCRED,(&mut cred as *mut libc::ucred).cast(),&mut len)}!=0 || len as usize!=std::mem::size_of_val(&cred){return invalid()}
    Ok((cred.uid,cred.gid))
}
pub fn send<T:Serialize>(fd:&OwnedFd,value:&T)->Result<(),Error>{
    let bytes=serde_json::to_vec(value)?;if bytes.len()>MAX_BYTES{return Err(Error::Limit)}
    let n=unsafe{libc::send(fd.as_raw_fd(),bytes.as_ptr().cast(),bytes.len(),libc::MSG_NOSIGNAL)};
    if n<0 || n as usize!=bytes.len(){return Err(Error::Unavailable)}Ok(())
}
pub fn receive<T:DeserializeOwned>(fd:&OwnedFd)->Result<T,Error>{
    let mut bytes=vec![0u8;MAX_BYTES];let mut iov=libc::iovec{iov_base:bytes.as_mut_ptr().cast(),iov_len:bytes.len()};
    let mut msg:libc::msghdr=unsafe{std::mem::zeroed()};msg.msg_iov=&mut iov;msg.msg_iovlen=1;
    // No ancillary buffer: the kernel closes rejected rights and sets MSG_CTRUNC.
    let n=unsafe{libc::recvmsg(fd.as_raw_fd(),&mut msg,libc::MSG_CMSG_CLOEXEC)};
    if n<=0 || msg.msg_flags&(libc::MSG_TRUNC|libc::MSG_CTRUNC)!=0{return Err(Error::Invalid)}
    Ok(serde_json::from_slice(&bytes[..n as usize])?)
}
pub fn request(request:&Request)->Result<Receipt,Error>{
    request.validate()?;directory()?;let fd=socket()?;let addr=address();
    if unsafe{libc::connect(fd.as_raw_fd(),(&addr as *const libc::sockaddr_un).cast(),std::mem::size_of_val(&addr) as _)}!=0{return Err(Error::Unavailable)}
    if peer(&fd)?!=(0,0){return invalid()}
    send(&fd,request)?;let response:Response=receive(&fd)?;
    if response.schema!=1 || response.nonce!=request.nonce{return invalid()}
    let receipt=response.receipt.ok_or(Error::Unavailable)?;
    let spec=request.validate()?;
    if receipt.binding!=request.item.binding || receipt.label!=spec.label || receipt.employee.0==[0;16]
        || receipt.bytes!=spec.bytes || receipt.inodes!=spec.inodes || receipt.principal_binding!=spec.binding
        || (receipt.uid<1000 && !(receipt.disabled && receipt.uid==0))
        || receipt.disabled!=(request.action==Action::Disable){return invalid()}
    Ok(receipt)
}
/// Caller is the root daemon holding the registry lock. This function never
/// accepts a socket path from a request or an environment variable.
pub fn bind(gateway_gid:u32)->Result<OwnedFd,Error>{
    directory()?;if unsafe{libc::geteuid()}!=0 || gateway_gid==0{return invalid()}
    match std::fs::symlink_metadata(SOCKET){
        Ok(m)=>{use std::os::unix::fs::FileTypeExt;if !m.file_type().is_socket() || m.uid()!=0{return invalid()}std::fs::remove_file(SOCKET)?;},
        Err(e) if e.kind()==std::io::ErrorKind::NotFound=>{},Err(e)=>return Err(e.into()),
    }
    let fd=socket()?;let addr=address();
    if unsafe{libc::bind(fd.as_raw_fd(),(&addr as *const libc::sockaddr_un).cast(),std::mem::size_of_val(&addr) as _)}!=0{return Err(Error::Unavailable)}
    let path=std::ffi::CString::new(SOCKET).map_err(|_|Error::Invalid)?;
    if unsafe{libc::chown(path.as_ptr(),0,gateway_gid)}!=0 || unsafe{libc::chmod(path.as_ptr(),0o660)}!=0 || unsafe{libc::listen(fd.as_raw_fd(),8)}!=0{return Err(Error::Unavailable)}
    Ok(fd)
}
pub fn accept(listener:&OwnedFd,gateway:(u32,u32))->Result<OwnedFd,Error>{
    let raw=unsafe{libc::accept4(listener.as_raw_fd(),std::ptr::null_mut(),std::ptr::null_mut(),libc::SOCK_CLOEXEC)};
    if raw<0{return Err(Error::Unavailable)}let fd=unsafe{OwnedFd::from_raw_fd(raw)};timeout(&fd)?;
    if gateway.0==0 || gateway.1==0 || peer(&fd)?!=gateway{return invalid()}Ok(fd)
}
