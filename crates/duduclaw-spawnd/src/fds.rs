//! Validate the actual gateway descriptors before admission; metadata is not authority.
use std::{io::{self, Seek, Write}, mem, os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd}};
use nix::{fcntl::{fcntl, FcntlArg, SealFlag}, sys::{memfd::{memfd_create, MemFdCreateFlag}, stat::fstat, statfs::fstatfs}};
use duduclaw_isolation::ipc::{InputFd, SpawnRequest, MAX_FRAME_BYTES};

fn invalid() -> io::Error { io::Error::new(io::ErrorKind::InvalidData, "invalid isolation descriptors") }

/// B2's sole inherited control descriptor must be a root-created Unix seqpacket.
pub fn validate_bootstrap(fd: BorrowedFd<'_>) -> io::Result<()> {
    use nix::sys::socket::{getsockopt, getsockname, sockopt, SockType, UnixAddr};
    let peer = getsockopt(&fd, sockopt::PeerCredentials)?;
    let _: UnixAddr = getsockname(fd.as_raw_fd())?;
    if peer.uid() != 0 || peer.gid() != 0 || getsockopt(&fd, sockopt::SockType)? != SockType::SeqPacket
        || fcntl(fd.as_raw_fd(), FcntlArg::F_GETFD)? & libc::FD_CLOEXEC == 0 { return Err(invalid()); }
    Ok(())
}

/// The daemon resolves both values from trusted installation state, never a frame.
#[derive(Debug, Clone, Copy)]
pub struct SnapshotAuthority { pub gateway_uid: u32, pub mount_id: u64 }

pub fn validate_start(request: &SpawnRequest, descriptors: &[OwnedFd], authority: SnapshotAuthority) -> io::Result<()> {
    let plan = request.fd_plan().map_err(|_| invalid())?;
    if descriptors.len() != plan.len() { return Err(invalid()); }
    // HX4: the helper input is the same sealed tmpfs memfd shape as a prompt;
    // the host revalidates it again right before `Start`.
    if let SpawnRequest::HelperCall { input_bytes, .. } = request {
        return validate_sealed(descriptors.first().ok_or_else(invalid)?.as_fd(), *input_bytes);
    }
    let SpawnRequest::StartRun { prompt_bytes, input_fds, workspace, .. } = request else { return Ok(()); };
    validate_sealed(descriptors[0].as_fd(), *prompt_bytes)?;
    for (input, fd) in input_fds.as_slice().iter().zip(descriptors.iter().skip(1)) {
        match input {
            InputFd::SealedMemfd { bytes } => validate_sealed(fd.as_fd(), *bytes)?,
            InputFd::ReadOnlySnapshot { bytes } => validate_snapshot(fd.as_fd(), *bytes, authority)?,
        }
    }
    if let Some(selection) = workspace {
        let fd = descriptors.last().ok_or_else(invalid)?.as_fd();
        validate_workspace_directory(fd, selection)?;
    }
    Ok(())
}

pub fn validate_workspace_directory(fd: BorrowedFd<'_>, selection: &duduclaw_isolation::ipc::WorkspaceSelection) -> io::Result<()> {
    selection.validate().map_err(|_| invalid())?;
    let stat = fstat(fd.as_raw_fd())?;
    let flags = fcntl(fd.as_raw_fd(), FcntlArg::F_GETFL)?;
    if stat.st_mode & libc::S_IFMT != libc::S_IFDIR || stat.st_dev != selection.device || stat.st_ino != selection.inode
        || flags & libc::O_ACCMODE != libc::O_RDONLY || flags & libc::O_PATH != 0
        || fcntl(fd.as_raw_fd(), FcntlArg::F_GETFD)? & libc::FD_CLOEXEC == 0 {
        return Err(invalid());
    }
    Ok(())
}

pub fn validate_sealed(fd: BorrowedFd<'_>, bytes: u64) -> io::Result<()> {
    let stat = fstat(fd.as_raw_fd())?;
    let flags = fcntl(fd.as_raw_fd(), FcntlArg::F_GETFL)?;
    let required = libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;
    if stat.st_mode & libc::S_IFMT != libc::S_IFREG || stat.st_size < 0 || stat.st_size as u64 != bytes
        || flags & libc::O_PATH != 0 || flags & libc::O_ACCMODE == libc::O_WRONLY
        || fstatfs(fd)?.filesystem_type().0 != libc::TMPFS_MAGIC
        || fcntl(fd.as_raw_fd(), FcntlArg::F_GET_SEALS)? & required != required {
        return Err(invalid());
    }
    if fcntl(fd.as_raw_fd(), FcntlArg::F_GETFD)? & libc::FD_CLOEXEC == 0 { return Err(invalid()); }
    Ok(())
}

fn validate_snapshot(fd: BorrowedFd<'_>, bytes: u64, authority: SnapshotAuthority) -> io::Result<()> {
    if authority.gateway_uid == 0 || authority.mount_id == 0 { return Err(invalid()); }
    let stat = fstat(fd.as_raw_fd())?;
    let flags = fcntl(fd.as_raw_fd(), FcntlArg::F_GETFL)?;
    if stat.st_mode & libc::S_IFMT != libc::S_IFREG || stat.st_nlink != 1 || stat.st_uid != authority.gateway_uid
        || stat.st_size < 0 || stat.st_size as u64 != bytes || flags & libc::O_ACCMODE != libc::O_RDONLY
        || flags & libc::O_PATH != 0 || fcntl(fd.as_raw_fd(), FcntlArg::F_GETFD)? & libc::FD_CLOEXEC == 0
        || mount_id(fd)? != authority.mount_id {
        return Err(invalid());
    }
    Ok(())
}

pub fn mount_id(fd: BorrowedFd<'_>) -> io::Result<u64> {
    let mut stat: libc::statx = unsafe { mem::zeroed() };
    // Linux statx AT_EMPTY_PATH inspects this fd, never a caller-supplied pathname.
    let result = unsafe { libc::statx(fd.as_raw_fd(), c"".as_ptr(), libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
        libc::STATX_MNT_ID, &mut stat) };
    if result != 0 { return Err(io::Error::last_os_error()); }
    if stat.stx_mask & libc::STATX_MNT_ID == 0 || stat.stx_mnt_id == 0 { return Err(invalid()); }
    Ok(stat.stx_mnt_id)
}

/// Root's bounded bootstrap plan is sealed before it is sent to B2. The plan's
/// schema/nonce validation remains the B2 seam; this function does not bless it.
pub fn sealed_plan(bytes: &[u8]) -> io::Result<OwnedFd> {
    if bytes.is_empty() || bytes.len() > MAX_FRAME_BYTES { return Err(invalid()); }
    let fd = memfd_create(c"duduclaw-run-plan", MemFdCreateFlag::MFD_CLOEXEC | MemFdCreateFlag::MFD_ALLOW_SEALING)?;
    let mut file = std::fs::File::from(fd);
    file.write_all(bytes)?;
    file.rewind()?;
    let seals = SealFlag::F_SEAL_SEAL | SealFlag::F_SEAL_SHRINK | SealFlag::F_SEAL_GROW | SealFlag::F_SEAL_WRITE;
    fcntl(file.as_raw_fd(), FcntlArg::F_ADD_SEALS(seals))?;
    validate_sealed(file.as_fd(), bytes.len() as u64)?;
    Ok(file.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_isolation::{bounded::List, execution::{RunHandle, RuntimeKind}, ids::{EmployeeId, RunId}};
    fn request() -> SpawnRequest {
        SpawnRequest::StartRun { run: RunHandle(RunId([1;16])), employee_id: EmployeeId([2;16]), runtime_request: RuntimeKind::Codex,
            task_ref: None, workspace: None, prompt_bytes: 2, input_fds: List::new(vec![]).unwrap(), pty: false }
    }
    #[test]
    fn sealed_fd_rejects_missing_seals_and_false_size() {
        let plain = memfd_create(c"unsealed", MemFdCreateFlag::MFD_CLOEXEC | MemFdCreateFlag::MFD_ALLOW_SEALING).unwrap();
        assert!(validate_sealed(plain.as_fd(), 0).is_err());
        let sealed = sealed_plan(b"{}").unwrap();
        assert!(validate_sealed(sealed.as_fd(), 2).is_ok());
        assert!(validate_sealed(sealed.as_fd(), 1).is_err());
        assert!(nix::unistd::write(&sealed, b"x").is_err());
    }
    #[test]
    fn helper_call_input_must_be_one_sealed_memfd_of_the_stated_size() {
        use duduclaw_isolation::{bounded::Text, ipc::HelperSlot};
        let authority = SnapshotAuthority { gateway_uid: 42, mount_id: 1 };
        let call = |input_bytes| SpawnRequest::HelperCall { run: RunHandle(RunId([1; 16])), employee_id: EmployeeId([2; 16]),
            helper: RuntimeKind::Registered(Text::new("duduclaw-program-v1".into()).unwrap()), slot: HelperSlot::Foreground,
            timeout_ms: 1_000, input_bytes };
        assert!(validate_start(&call(2), &[sealed_plan(b"{}").unwrap()], authority).is_ok());
        assert!(validate_start(&call(3), &[sealed_plan(b"{}").unwrap()], authority).is_err(), "size");
        assert!(validate_start(&call(2), &[], authority).is_err(), "count");
        assert!(validate_start(&call(2), &[sealed_plan(b"{}").unwrap(), sealed_plan(b"{}").unwrap()], authority).is_err());
        let unsealed = memfd_create(c"unsealed", MemFdCreateFlag::MFD_CLOEXEC | MemFdCreateFlag::MFD_ALLOW_SEALING).unwrap();
        assert!(validate_start(&call(0), &[unsealed], authority).is_err(), "seals");
        let file = std::fs::File::open("/dev/null").unwrap();
        assert!(validate_start(&call(0), &[file.into()], authority).is_err(), "not a memfd");
    }
    #[test]
    fn start_rejects_fd_count_mismatch_and_special_files() {
        let authority = SnapshotAuthority { gateway_uid: 42, mount_id: 1 };
        assert!(validate_start(&request(), &[], authority).is_err());
        let fd = sealed_plan(b"{}").unwrap();
        assert!(validate_start(&request(), &[fd], authority).is_ok());
        let file = std::fs::File::open("/dev/null").unwrap();
        assert!(validate_start(&request(), &[file.into()], authority).is_err());
    }
    #[test]
    fn snapshot_rejects_wrong_mount_owner_and_writable_fd() {
        let fd = sealed_plan(b"{}").unwrap();
        assert!(validate_snapshot(fd.as_fd(), 2, SnapshotAuthority { gateway_uid: 0, mount_id: 1 }).is_err());
        assert!(validate_snapshot(fd.as_fd(), 2, SnapshotAuthority { gateway_uid: 42, mount_id: 1 }).is_err());
    }
    #[test]
    fn sealed_plan_has_a_hard_size_bound() {
        assert!(sealed_plan(&[]).is_err());
        assert!(sealed_plan(&vec![0; MAX_FRAME_BYTES + 1]).is_err());
    }
}
