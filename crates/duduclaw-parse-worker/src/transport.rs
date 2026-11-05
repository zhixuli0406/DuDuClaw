//! Linux seqpacket transport. Own every received fd before rejecting a packet.
use std::fs::File;
use std::io::{IoSlice, Read, Seek, SeekFrom, Write};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::time::Instant;
use nix::fcntl::{fcntl, FcntlArg, OFlag, SealFlag};
use nix::poll::{poll, PollFd, PollFlags};
use nix::sys::socket::{sendmsg, ControlMessage, MsgFlags};
use duduclaw_isolation::{ipc, worker::{ParseError, MAX_WORKER_FRAME_BYTES}};

pub fn wait(fd: &impl AsFd, events: PollFlags, deadline: Instant) -> Result<(), ParseError> {
    wait_checked(fd, events, deadline, &|| Ok(()))
}

/// A blocking pool job cannot be cancelled by dropping its async join handle.
/// Bound each poll so live authority is checked even if no peer event arrives.
pub fn wait_checked(fd: &impl AsFd, events: PollFlags, deadline: Instant,
    verify: &dyn Fn() -> Result<(), ParseError>) -> Result<(), ParseError> {
    loop {
        verify()?;
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() { return Err(ParseError::Timeout); }
        let millis = left.as_millis().saturating_add(1).min(50) as u16;
        let mut fds = [PollFd::new(fd.as_fd(), events)];
        match poll(&mut fds, millis) {
            Ok(0) => continue,
            Ok(_) if fds[0].revents().is_some_and(|flags| flags.contains(events)) => { verify()?; return Ok(()); },
            Ok(_) => return Err(ParseError::Unavailable),
            Err(nix::errno::Errno::EINTR) => continue,
            Err(_) => return Err(ParseError::Unavailable),
        }
    }
}

pub fn send(fd: &impl AsFd, payload: &[u8], files: &[File], deadline: Instant) -> Result<(), ParseError> {
    send_checked(fd, payload, files, deadline, &|| Ok(()))
}

pub fn send_checked(fd: &impl AsFd, payload: &[u8], files: &[File], deadline: Instant,
    verify: &dyn Fn() -> Result<(), ParseError>) -> Result<(), ParseError> {
    let frame = ipc::frame_payload(payload).map_err(|_| ParseError::LimitExceeded)?;
    let raw: Vec<_> = files.iter().map(AsRawFd::as_raw_fd).collect();
    if raw.len() > 24 { return Err(ParseError::InvalidContent); }
    wait_checked(fd, PollFlags::POLLOUT, deadline, verify)?;
    let ancillary = [ControlMessage::ScmRights(&raw)];
    let messages = if raw.is_empty() { &[][..] } else { &ancillary[..] };
    verify()?;
    let written = sendmsg::<()>(fd.as_fd().as_raw_fd(), &[IoSlice::new(&frame)], messages,
        MsgFlags::MSG_NOSIGNAL | MsgFlags::MSG_DONTWAIT, None).map_err(|_| ParseError::Unavailable)?;
    if written != frame.len() { return Err(ParseError::Unavailable); }
    Ok(())
}

/// nix's cmsgs() refuses iteration on MSG_CTRUNC, leaving received descriptors
/// unowned. Use recvmsg here so even a truncated ancillary set is closed first.
#[allow(unsafe_code)]
pub fn receive(fd: &impl AsFd, max_fds: usize, deadline: Instant) -> Result<(Vec<u8>, Vec<File>), ParseError> {
    receive_checked(fd, max_fds, deadline, &|| Ok(()))
}

#[allow(unsafe_code)]
pub fn receive_checked(fd: &impl AsFd, max_fds: usize, deadline: Instant,
    verify: &dyn Fn() -> Result<(), ParseError>) -> Result<(Vec<u8>, Vec<File>), ParseError> {
    wait_checked(fd, PollFlags::POLLIN, deadline, verify)?;
    let mut bytes = vec![0u8; MAX_WORKER_FRAME_BYTES + 4];
    // Linux SCM_MAX_FD = 253. Leave space for credentials/unknown ancillary data.
    let mut control = [0usize; 512];
    let mut iov = libc::iovec { iov_base: bytes.as_mut_ptr().cast(), iov_len: bytes.len() };
    // SAFETY: all buffers live through recvmsg. Headers and data are accessed
    // only within the kernel-reported, separately checked control-buffer bounds.
    unsafe {
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = control.as_mut_ptr().cast();
        msg.msg_controllen = std::mem::size_of_val(&control);
        verify()?;
        let count = libc::recvmsg(fd.as_fd().as_raw_fd(), &mut msg,
            libc::MSG_CMSG_CLOEXEC | libc::MSG_DONTWAIT);
        if count < 0 { return Err(ParseError::Unavailable); }
        let mut files = Vec::new();
        let mut invalid = msg.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0;
        let base = msg.msg_control as usize;
        let end = base + msg.msg_controllen;
        let mut header = libc::CMSG_FIRSTHDR(&msg);
        while !header.is_null() {
            let start = header as usize;
            if start < base || start + std::mem::size_of::<libc::cmsghdr>() > end {
                invalid = true; break;
            }
            let length = (*header).cmsg_len;
            let minimum = libc::CMSG_LEN(0) as usize;
            if length < minimum || length > end.saturating_sub(start) { invalid = true; break; }
            if (*header).cmsg_level == libc::SOL_SOCKET && (*header).cmsg_type == libc::SCM_RIGHTS {
                let size = length - minimum;
                if size % std::mem::size_of::<libc::c_int>() != 0 { invalid = true; }
                let data = libc::CMSG_DATA(header).cast::<libc::c_int>();
                for index in 0..size / std::mem::size_of::<libc::c_int>() {
                    let raw = std::ptr::read_unaligned(data.add(index));
                    files.push(File::from_raw_fd(raw));
                }
            } else { invalid = true; }
            header = libc::CMSG_NXTHDR(&msg, header);
        }
        if invalid || files.len() > max_fds { return Err(ParseError::InvalidContent); }
        bytes.truncate(count as usize);
        let payload = ipc::payload(&bytes).map_err(|_| ParseError::InvalidContent)?.to_vec();
        Ok((payload, files))
    }
}

pub fn seals() -> SealFlag {
    SealFlag::F_SEAL_SEAL | SealFlag::F_SEAL_SHRINK | SealFlag::F_SEAL_GROW | SealFlag::F_SEAL_WRITE
}

/// Copy only bytes; filenames and parser-specific paths never cross the socket.
pub fn snapshot(reader: &mut impl Read) -> Result<File, ParseError> {
    use nix::sys::memfd::{memfd_create, MFdFlags};
    let owned = memfd_create("duduclaw-parse", MFdFlags::MFD_CLOEXEC | MFdFlags::MFD_ALLOW_SEALING)
        .map_err(|_| ParseError::Unavailable)?;
    let mut file = File::from(owned);
    let mut total = 0u64;
    let mut buf = [0u8; 16384];
    loop {
        let n = reader.read(&mut buf).map_err(|_| ParseError::InvalidContent)?;
        if n == 0 { break; }
        total += n as u64;
        if total > crate::policy::MAX_FILE_BYTES { return Err(ParseError::LimitExceeded); }
        file.write_all(&buf[..n]).map_err(|_| ParseError::Unavailable)?;
    }
    fcntl(&file, FcntlArg::F_ADD_SEALS(seals())).map_err(|_| ParseError::Unavailable)?;
    // Reopen our own sealed fd read-only, not any caller-controlled pathname.
    let ro = nix::fcntl::open(format!("/proc/self/fd/{}", file.as_raw_fd()).as_str(),
        OFlag::O_RDONLY | OFlag::O_CLOEXEC, nix::sys::stat::Mode::empty())
        .map_err(|_| ParseError::Unavailable)?;
    Ok(File::from(ro))
}

pub fn validate_inputs(request: &duduclaw_isolation::worker::ParseRequest, files: &mut [File]) -> Result<(), ParseError> {
    crate::policy::validate(request)?;
    if files.len() != request.expected_fd_count() { return Err(ParseError::InvalidContent); }
    for (input, file) in request.inputs.as_slice().iter().zip(files.iter_mut()) {
        let meta = file.metadata().map_err(|_| ParseError::InvalidContent)?;
        let flags = fcntl(&*file, FcntlArg::F_GETFL).map_err(|_| ParseError::InvalidContent)?;
        let actual_seals = fcntl(&*file, FcntlArg::F_GET_SEALS).map_err(|_| ParseError::InvalidContent)?;
        let fs = nix::sys::statfs::fstatfs(&*file).map_err(|_| ParseError::InvalidContent)?;
        if !meta.is_file() || meta.len() != input.size_bytes
            || flags & libc::O_ACCMODE != libc::O_RDONLY
            || actual_seals & seals().bits() != seals().bits()
            || fs.filesystem_type() != nix::sys::statfs::TMPFS_MAGIC {
            return Err(ParseError::InvalidContent);
        }
        file.seek(SeekFrom::Start(0)).map_err(|_| ParseError::InvalidContent)?;
    }
    let mut identities = std::collections::BTreeSet::new();
    use std::os::unix::fs::MetadataExt;
    for file in files.iter() {
        let meta = file.metadata().map_err(|_| ParseError::InvalidContent)?;
        if !identities.insert((meta.dev(), meta.ino())) { return Err(ParseError::InvalidContent); }
    }
    for file in files.iter().skip(request.inputs.as_slice().len()) {
        let meta = file.metadata().map_err(|_| ParseError::InvalidContent)?;
        let flags = fcntl(file, FcntlArg::F_GETFL).map_err(|_| ParseError::InvalidContent)?;
        let actual_seals = fcntl(file, FcntlArg::F_GET_SEALS).map_err(|_| ParseError::InvalidContent)?;
        let fs = nix::sys::statfs::fstatfs(file).map_err(|_| ParseError::InvalidContent)?;
        if !meta.is_file() || meta.len() != 0 || meta.nlink() != 0
            || flags & libc::O_ACCMODE != libc::O_RDWR || actual_seals != 0
            || fs.filesystem_type() != nix::sys::statfs::TMPFS_MAGIC {
            return Err(ParseError::InvalidContent);
        }
    }
    Ok(())
}

pub fn output_file() -> Result<File, ParseError> {
    use nix::sys::memfd::{memfd_create, MFdFlags};
    memfd_create("duduclaw-parse-output", MFdFlags::MFD_CLOEXEC | MFdFlags::MFD_ALLOW_SEALING)
        .map(File::from).map_err(|_| ParseError::Unavailable)
}

/// Seal before size/hash/readback. A successful reply is not an authorization
/// to publish: caller policy still applies to these untrusted bytes.
pub fn verified_outputs(request: &duduclaw_isolation::worker::ParseRequest,
    response: &duduclaw_isolation::worker::ParseResponse, files: Vec<File>)
    -> Result<Vec<Vec<u8>>, ParseError>
{
    use duduclaw_isolation::worker::{ParseResult, ParsedItem};
    use sha2::{Digest, Sha256};
    let ParseResult::Complete { items } = &response.result else { return Err(ParseError::InvalidContent); };
    let mut results = Vec::new();
    for (output, mut file) in request.outputs.as_slice().iter()
        .zip(files.into_iter().skip(request.inputs.as_slice().len())) {
        fcntl(&file, FcntlArg::F_ADD_SEALS(seals())).map_err(|_| ParseError::InvalidContent)?;
        let artifacts: Vec<_> = items.as_slice().iter().filter_map(|item| match item {
            ParsedItem::OutputArtifact { output_slot, size_bytes, sha256, .. } if *output_slot == output.slot =>
                Some((*size_bytes, sha256)),
            _ => None,
        }).collect();
        let [(size, digest)] = artifacts.as_slice() else { return Err(ParseError::InvalidContent); };
        if *size > output.max_bytes.get() || file.metadata().map_err(|_| ParseError::InvalidContent)?.len() != *size {
            return Err(ParseError::LimitExceeded);
        }
        file.seek(SeekFrom::Start(0)).map_err(|_| ParseError::InvalidContent)?;
        let mut bytes = Vec::new();
        file.take(output.max_bytes.get() + 1).read_to_end(&mut bytes).map_err(|_| ParseError::InvalidContent)?;
        if bytes.len() as u64 != *size || Sha256::digest(&bytes).as_slice() != digest.0 {
            return Err(ParseError::InvalidContent);
        }
        results.push(bytes);
    }
    if results.len() != request.outputs.as_slice().len() { return Err(ParseError::InvalidContent); }
    Ok(results)
}

pub fn connect() -> Result<OwnedFd, ParseError> {
    use nix::sys::socket::{self, AddressFamily, SockFlag, SockType, UnixAddr};
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let metadata = std::fs::symlink_metadata(crate::policy::SOCKET_PATH).map_err(|_| ParseError::Unavailable)?;
    let parent = std::fs::symlink_metadata("/run/duduclaw-parse").map_err(|_| ParseError::Unavailable)?;
    if !metadata.file_type().is_socket() || metadata.uid() != 0 || metadata.mode() & 0o007 != 0
        || !parent.is_dir() || parent.uid() != 0 || parent.mode() & 0o022 != 0 {
        return Err(ParseError::Unavailable);
    }
    let fd = socket::socket(AddressFamily::Unix, SockType::SeqPacket,
        SockFlag::SOCK_CLOEXEC | SockFlag::SOCK_NONBLOCK, None).map_err(|_| ParseError::Unavailable)?;
    socket::setsockopt(&fd, socket::sockopt::SndBuf, &(MAX_WORKER_FRAME_BYTES * 2))
        .map_err(|_| ParseError::Unavailable)?;
    let address = UnixAddr::new(crate::policy::SOCKET_PATH).map_err(|_| ParseError::Unavailable)?;
    socket::connect(fd.as_raw_fd(), &address).map_err(|_| ParseError::Unavailable)?;
    let peer = socket::getsockopt(&fd, socket::sockopt::PeerCredentials).map_err(|_| ParseError::Unavailable)?;
    if peer.uid() != 0 { return Err(ParseError::Unavailable); }
    Ok(fd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use nix::sys::socket::{socketpair, AddressFamily, SockType, SockFlag};
    use duduclaw_isolation::worker::{ContentFormat, ParseOperation};

    fn pair() -> (OwnedFd, OwnedFd) {
        socketpair(AddressFamily::Unix, SockType::SeqPacket, None, SockFlag::SOCK_CLOEXEC).unwrap()
    }
    fn deadline() -> Instant { Instant::now() + Duration::from_secs(2) }

    #[test]
    fn sealed_fd_roundtrip_and_exact_count() {
        let (a, b) = pair();
        let input = snapshot(&mut &b"hello"[..]).unwrap();
        send(&a, b"{}", &[input], deadline()).unwrap();
        let (payload, mut inputs) = receive(&b, 1, deadline()).unwrap();
        assert_eq!(payload, b"{}");
        let request = crate::client::request(ParseOperation::ExtractDocument, ContentFormat::Text, 5);
        validate_inputs(&request, &mut inputs).unwrap();
        let flags = fcntl(&inputs[0], FcntlArg::F_GETFD).unwrap();
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
        let mut body = String::new();
        inputs[0].read_to_string(&mut body).unwrap();
        assert_eq!(body, "hello");
        assert!(validate_inputs(&request, &mut []).is_err());
        let mut writable = tempfile::tempfile().unwrap();
        writable.write_all(b"hello").unwrap();
        assert!(validate_inputs(&request, &mut [writable]).is_err());
    }

    #[test]
    fn rejected_ancillary_closes_received_descriptors() {
        use std::os::unix::fs::MetadataExt;
        let (a, b) = pair();
        let input = snapshot(&mut &b"fd cleanup"[..]).unwrap();
        let identity = input.metadata().unwrap();
        let references = || std::fs::read_dir("/proc/self/fd").unwrap().filter_map(Result::ok)
            .filter_map(|entry| std::fs::metadata(entry.path()).ok())
            .filter(|meta| meta.dev() == identity.dev() && meta.ino() == identity.ino()).count();
        let count = references();
        send(&a, b"{}", std::slice::from_ref(&input), deadline()).unwrap();
        assert!(matches!(receive(&b, 0, deadline()), Err(ParseError::InvalidContent)));
        assert_eq!(references(), count);
        // Valid rights attached to an invalid length prefix are closed as well.
        let raw = [input.as_raw_fd()];
        sendmsg::<()>(a.as_raw_fd(), &[IoSlice::new(b"\0\0\0\x05{}")],
            &[ControlMessage::ScmRights(&raw)], MsgFlags::empty(), None).unwrap();
        assert!(receive(&b, 1, deadline()).is_err());
        assert_eq!(references(), count);
    }

    #[test]
    fn malformed_frame_and_silent_peer_are_bounded() {
        let (a, b) = pair();
        sendmsg::<()>(a.as_raw_fd(), &[IoSlice::new(b"bad")], &[], MsgFlags::empty(), None).unwrap();
        assert!(matches!(receive(&b, 0, deadline()), Err(ParseError::InvalidContent)));
        let start = Instant::now();
        assert!(matches!(receive(&b, 0, start + Duration::from_millis(5)), Err(ParseError::Timeout)));
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn maximum_frame_roundtrip_and_truncation_cleanup() {
        use nix::sys::socket::{setsockopt, sockopt};
        use std::os::unix::fs::MetadataExt;
        let (a, b) = pair();
        setsockopt(&a, sockopt::SndBuf, &(MAX_WORKER_FRAME_BYTES * 2)).unwrap();
        let payload = vec![b'x'; MAX_WORKER_FRAME_BYTES];
        send(&a, &payload, &[], deadline()).unwrap();
        assert_eq!(receive(&b, 0, deadline()).unwrap().0, payload);
        // One extra byte forces MSG_TRUNC; attached rights must still be owned
        // and dropped by receive before returning InvalidContent.
        let file = snapshot(&mut &b"truncated"[..]).unwrap();
        let identity = file.metadata().unwrap();
        let references = || std::fs::read_dir("/proc/self/fd").unwrap().filter_map(Result::ok)
            .filter_map(|entry| std::fs::metadata(entry.path()).ok())
            .filter(|meta| meta.dev() == identity.dev() && meta.ino() == identity.ino()).count();
        let count = references();
        let frame = vec![b'x'; MAX_WORKER_FRAME_BYTES + 5];
        let raw = [file.as_raw_fd()];
        sendmsg::<()>(a.as_raw_fd(), &[IoSlice::new(&frame)],
            &[ControlMessage::ScmRights(&raw)], MsgFlags::empty(), None).unwrap();
        assert!(matches!(receive(&b, 1, deadline()), Err(ParseError::InvalidContent)));
        assert_eq!(references(), count);
    }
    #[test]
    fn writable_output_is_sealed_before_size_and_checksum_readback() {
        use duduclaw_isolation::{bounded::List, ids::Sha256Digest, worker::*};
        use sha2::{Digest, Sha256};
        let mut request = crate::client::request(ParseOperation::ExtractDocument, ContentFormat::Text, 5);
        request.outputs = List::new(vec![OutputFd { slot: 1, max_bytes: 16u64.try_into().unwrap() }]).unwrap();
        let input = snapshot(&mut &b"hello"[..]).unwrap();
        let mut output = output_file().unwrap();
        let duplicate = output.try_clone().unwrap();
        let mut files = vec![input, output.try_clone().unwrap()];
        validate_inputs(&request, &mut files).unwrap();
        output.write_all(b"result").unwrap();
        assert!(validate_inputs(&request, &mut files).is_err(), "nonempty output rejected at admission");
        let response = ParseResponse { version: WORKER_PROTOCOL_VERSION, request_id: request.request_id,
            result: ParseResult::Complete { items: List::new(vec![ParsedItem::OutputArtifact {
                output_slot: 1, format: ContentFormat::Text, size_bytes: 6,
                sha256: Sha256Digest(Sha256::digest(b"result").into()),
            }]).unwrap() } };
        assert_eq!(verified_outputs(&request, &response, files).unwrap(), vec![b"result".to_vec()]);
        assert_eq!(fcntl(&duplicate, FcntlArg::F_GET_SEALS).unwrap() & seals().bits(), seals().bits());
        assert!(output.write_all(b"changed").is_err());
        assert!(output.set_len(0).is_err());
        let mut corrupt = output_file().unwrap();
        corrupt.write_all(b"wrong!").unwrap();
        let input = snapshot(&mut &b"hello"[..]).unwrap();
        assert!(verified_outputs(&request, &response, vec![input, corrupt]).is_err());
    }

    #[test]
    fn checked_send_revalidates_after_writable_poll_before_fd_delivery() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (sender, receiver) = pair();
        let checks = AtomicUsize::new(0);
        let verify = || if checks.fetch_add(1, Ordering::SeqCst) < 2 { Ok(()) }
            else { Err(ParseError::Unavailable) };
        let input = snapshot(&mut &b"private input"[..]).unwrap();
        assert_eq!(send_checked(&sender, b"{}", &[input], deadline(), &verify), Err(ParseError::Unavailable));
        assert_eq!(checks.load(Ordering::SeqCst), 3);
        let mut buffer = [0; 32];
        assert_eq!(nix::sys::socket::recv(receiver.as_raw_fd(), &mut buffer, MsgFlags::MSG_DONTWAIT),
            Err(nix::errno::Errno::EAGAIN));
    }

    #[test]
    fn checked_receive_revalidates_after_readable_poll_without_consuming_packet() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (sender, receiver) = pair();
        send(&sender, b"{}", &[], deadline()).unwrap();
        let checks = AtomicUsize::new(0);
        let verify = || if checks.fetch_add(1, Ordering::SeqCst) < 2 { Ok(()) }
            else { Err(ParseError::Unavailable) };
        assert!(matches!(receive_checked(&receiver, 0, deadline(), &verify), Err(ParseError::Unavailable)));
        assert_eq!(checks.load(Ordering::SeqCst), 3);
        assert_eq!(receive(&receiver, 0, deadline()).unwrap().0, b"{}");
    }

    #[test]
    fn checked_receive_observes_revocation_while_peer_remains_silent() {
        use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
        let (_sender, receiver) = pair();
        let live = Arc::new(AtomicBool::new(true));
        let revoked = live.clone();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            revoked.store(false, Ordering::SeqCst);
        });
        let start = Instant::now();
        let result = receive_checked(&receiver, 0, start + Duration::from_secs(5),
            &|| if live.load(Ordering::SeqCst) { Ok(()) } else { Err(ParseError::Unavailable) });
        thread.join().unwrap();
        assert!(matches!(result, Err(ParseError::Unavailable)));
        assert!(start.elapsed() < Duration::from_secs(2));
    }

}
