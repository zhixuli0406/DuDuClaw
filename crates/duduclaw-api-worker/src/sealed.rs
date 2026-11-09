use std::os::fd::FromRawFd;
use crate::MAX_INPUT;
pub fn sealed_request() -> std::result::Result<Vec<u8>, &'static str> {
    use std::os::unix::fs::FileExt;
    // Duplicate only the fixed sealed descriptor. Never reconstruct a host path.
    // SAFETY: fcntl accepts an invalid descriptor and returns EBADF; no borrowed fd is fabricated.
    let fd = unsafe { libc::fcntl(4, libc::F_DUPFD_CLOEXEC, 6) };
    if fd < 0 { return Err("sealed input unavailable"); }
    // SAFETY: the successful duplicate above is fresh and uniquely owned.
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    let seals = rustix::fs::fcntl_get_seals(&file).map_err(|_| "sealed input invalid")?;
    let required = rustix::fs::SealFlags::SEAL | rustix::fs::SealFlags::SHRINK
        | rustix::fs::SealFlags::GROW | rustix::fs::SealFlags::WRITE;
    let metadata = file.metadata().map_err(|_| "sealed input unavailable")?;
    if !seals.contains(required) || !metadata.is_file()
        || metadata.len() == 0 || metadata.len() > MAX_INPUT as u64 { return Err("sealed input invalid"); }
    let mut bytes = vec![0; metadata.len() as usize];
    file.read_exact_at(&mut bytes, 0).map_err(|_| "sealed input read failed")?;
    // Keep conversation input out of the bridge child descriptor set.
    // SAFETY: fd 4 was validated through duplication and has not been closed or reassigned.
    if unsafe { libc::fcntl(4, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 { return Err("input fencing failed"); }
    Ok(bytes)
}

