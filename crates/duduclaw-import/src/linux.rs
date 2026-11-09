//! Checks on the worker side of the root-launcher handoff.
use std::{fs::File, os::{fd::{OwnedFd, AsRawFd}, unix::fs::MetadataExt}};
use rustix::{fs::{fcntl_getfl, OFlags}, process::{getuid, geteuid, set_dumpable_behavior, dumpable_behavior, DumpableBehavior}, thread::no_new_privs};
use crate::{Error, Result, MAX_SOURCE_BYTES};

/// Consume only the fixed input slots after the launcher has isolated this worker.
pub fn source_fd(fd: OwnedFd) -> Result<File> {
    if !matches!(fd.as_raw_fd(), 3 | 4) { return Err(Error::InvalidRequest); }
    received_source_fd(fd)
}

/// SCM_RIGHTS descriptors are validated before any legacy bytes are read.
pub fn received_source_fd(fd: OwnedFd) -> Result<File> {
    let flags = fcntl_getfl(&fd).map_err(|_| Error::Isolation)?;
    if flags & OFlags::RWMODE != OFlags::RDONLY || flags.contains(OFlags::PATH) { return Err(Error::Isolation); }
    let file = File::from(fd);
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() > MAX_SOURCE_BYTES {
        return Err(Error::Isolation);
    }
    Ok(file)
}

pub fn check_worker() -> Result<()> {
    if geteuid().is_root() || getuid() != geteuid()
        || !no_new_privs().map_err(|_| Error::Isolation)? { return Err(Error::Isolation); }
    set_dumpable_behavior(DumpableBehavior::NotDumpable).map_err(|_| Error::Isolation)?;
    if dumpable_behavior().map_err(|_| Error::Isolation)? != DumpableBehavior::NotDumpable {
        return Err(Error::Isolation);
    }
    // SAFETY: process-wide ceiling is set only in the dedicated, single-thread worker.
    unsafe { rusqlite::ffi::sqlite3_hard_heap_limit64(64 * 1024 * 1024); }
    Ok(())
}

/// Connect only to the root coordinator. Its private random socket directory is
/// traversable but not listable or writable by the transient DynamicUser.
pub fn handoff(path: &std::path::Path) -> Result<(crate::ImportRequest, File, Option<File>, std::os::unix::net::UnixStream)> {
    use std::{io::Read, os::{fd::FromRawFd, unix::net::UnixStream}};
    let mut stream=UnixStream::connect(path)?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(65)))?;
    stream.set_write_timeout(Some(std::time::Duration::from_secs(65)))?;
    let mut cred=libc::ucred {pid:0,uid:1,gid:1};let mut size=std::mem::size_of_val(&cred) as libc::socklen_t;
    // SAFETY: exact live buffers for SO_PEERCRED on the connected Unix socket.
    if unsafe {libc::getsockopt(stream.as_raw_fd(),libc::SOL_SOCKET,libc::SO_PEERCRED,
        (&mut cred as *mut libc::ucred).cast(),&mut size)}!=0
        || size as usize!=std::mem::size_of_val(&cred) || cred.uid!=0 {return Err(Error::Isolation);}
    let mut marker=[0u8;1];let mut control=[0usize;8];
    let mut iov=libc::iovec {iov_base:marker.as_mut_ptr().cast(),iov_len:1};
    let mut descriptors=Vec::new();
    // SAFETY: aligned ancillary buffer, exact iovec sizes; all received fds are
    // immediately given an owner, including those on rejected messages.
    unsafe {
        let mut message:libc::msghdr=std::mem::zeroed();message.msg_iov=&mut iov;message.msg_iovlen=1;
        message.msg_control=control.as_mut_ptr().cast();message.msg_controllen=std::mem::size_of_val(&control);
        let count=libc::recvmsg(stream.as_raw_fd(),&mut message,libc::MSG_CMSG_CLOEXEC);
        let mut header=libc::CMSG_FIRSTHDR(&message);
        let mut invalid=false;
        while !header.is_null() {
            if (*header).cmsg_level!=libc::SOL_SOCKET || (*header).cmsg_type!=libc::SCM_RIGHTS
                || (*header).cmsg_len<libc::CMSG_LEN(0) as usize {invalid=true;break;}
            let bytes=(*header).cmsg_len-libc::CMSG_LEN(0) as usize;
            if bytes%std::mem::size_of::<libc::c_int>()!=0 {invalid=true;break;}
            for index in 0..bytes/std::mem::size_of::<libc::c_int>() {
                descriptors.push(OwnedFd::from_raw_fd(*libc::CMSG_DATA(header).cast::<libc::c_int>().add(index)));
            }
            header=libc::CMSG_NXTHDR(&message,header);
        }
        if count!=1 || marker!=[b'F'] || invalid || message.msg_flags&(libc::MSG_CTRUNC|libc::MSG_TRUNC)!=0
            || !(1..=2).contains(&descriptors.len()) {return Err(Error::Isolation);}
    }
    let mut length=[0u8;4];stream.read_exact(&mut length)?;let length=u32::from_be_bytes(length) as usize;
    if length>crate::MAX_REQUEST_BYTES {return Err(Error::Limit);}
    let mut bytes=vec![0u8;length];stream.read_exact(&mut bytes)?;
    let request=crate::ImportRequest::decode(&bytes)?;
    if descriptors.len()!=1+usize::from(request.has_wal) {return Err(Error::InvalidRequest);}
    let mut end=[0u8;1];if stream.read(&mut end)?!=0 {return Err(Error::InvalidRequest);}
    let mut descriptors=descriptors.into_iter();
    let main=received_source_fd(descriptors.next().ok_or(Error::Isolation)?)?;
    let wal=descriptors.next().map(received_source_fd).transpose()?;
    Ok((request,main,wal,stream))
}
