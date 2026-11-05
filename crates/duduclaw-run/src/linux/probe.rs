use crate::{Error, Result};
use std::os::fd::{FromRawFd, OwnedFd};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Readiness {
    pub kernel_major: u32,
    pub kernel_minor: u32,
    /// Actual kernel ABI; never replace this evidence with the policy ceiling.
    pub landlock_abi: u32,
    /// Policy selected for launch; actual enforcement is checked during launch.
    pub landlock_ruleset_abi: u32,
}

fn landlock_evidence(detected: u32) -> Result<(u32, u32)> {
    let policy = crate::landlock::Policy::for_abi(detected).map_err(|_| Error::UnsupportedKernel)?;
    Ok((detected, policy.abi()))
}

pub fn kernel_version(release: &str) -> Result<(u32, u32)> {
    let mut parts = release.split('.');
    let major = parts.next().and_then(|s| s.parse::<u32>().ok()).ok_or(Error::UnsupportedKernel)?;
    let minor = parts.next().and_then(|s| s.parse::<u32>().ok()).ok_or(Error::UnsupportedKernel)?;
    if (major, minor) < (5, 15) { return Err(Error::UnsupportedKernel); }
    Ok((major, minor))
}

/// Positive version/ABI checks plus harmless syscall-availability probes.
/// Actual namespaces, detached mounts, proc flags and enforcement must still
/// succeed in the launch state machine; this is not a systemd readiness receipt.
pub fn probe() -> Result<Readiness> {
    let mut name: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut name) } != 0 { return Err(Error::UnsupportedKernel); }
    let release = unsafe { std::ffi::CStr::from_ptr(name.release.as_ptr()) };
    let (kernel_major, kernel_minor) = kernel_version(release.to_str().map_err(|_| Error::UnsupportedKernel)?)?;
    let landlock_abi = crate::landlock::probe_abi().map_err(|_| Error::UnsupportedKernel)?;
    let (landlock_abi, landlock_ruleset_abi) = landlock_evidence(landlock_abi)?;
    // No actual process creation: size zero must fail EINVAL on a supported API.
    let clone = unsafe { libc::syscall(libc::SYS_clone3, std::ptr::null::<u8>(), 0usize) };
    if clone != -1 || super::errno() != libc::EINVAL { return Err(Error::UnsupportedKernel); }
    let root = unsafe { libc::open(c"/".as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
    if root < 0 { return Err(Error::UnsupportedKernel); }
    let root = unsafe { OwnedFd::from_raw_fd(root) };
    use std::os::fd::AsRawFd;
    super::fd::open_beneath(root.as_raw_fd(), c".", libc::O_PATH).map_err(|_| Error::UnsupportedKernel)?;
    let observation = super::fd::observe(root.as_raw_fd()).map_err(|_| Error::UnsupportedKernel)?;
    if observation.statx_mask & libc::STATX_MNT_ID == 0 { return Err(Error::UnsupportedKernel); }
    // File descriptor numbers are signed ints. This range cannot contain one.
    super::fd::close_range(i32::MAX as u32 + 1, u32::MAX).map_err(|_| Error::UnsupportedKernel)?;
    probe_mount_api()?;
    let action: u32 = 0x80000000; // SECCOMP_RET_KILL_PROCESS.
    if unsafe { libc::syscall(libc::SYS_seccomp, 2u32, 0u32, &action) } != 0 { return Err(Error::UnsupportedKernel); }
    Ok(Readiness { kernel_major, kernel_minor, landlock_abi, landlock_ruleset_abi })
}

fn probe_mount_api() -> Result<()> {
    use std::os::fd::AsRawFd;
    let context = unsafe { libc::syscall(libc::SYS_fsopen, c"tmpfs".as_ptr(), 1u32) };
    if context < 0 { return Err(Error::UnsupportedKernel); }
    let context = unsafe { OwnedFd::from_raw_fd(context as i32) };
    for (key,value) in [(c"size",c"4096"),(c"nr_inodes",c"4"),(c"mode",c"0700")] {
        if unsafe { libc::syscall(libc::SYS_fsconfig, context.as_raw_fd(), 1u32, key.as_ptr(), value.as_ptr(), 0i32) } != 0 {
            return Err(Error::UnsupportedKernel);
        }
    }
    if unsafe { libc::syscall(libc::SYS_fsconfig, context.as_raw_fd(), 6u32,
        std::ptr::null::<u8>(), std::ptr::null::<u8>(), 0i32) } != 0 { return Err(Error::UnsupportedKernel); }
    let mount = unsafe { libc::syscall(libc::SYS_fsmount, context.as_raw_fd(), 1u32, 2u32 | 4u32) };
    if mount < 0 { return Err(Error::UnsupportedKernel); }
    let mount = unsafe { OwnedFd::from_raw_fd(mount as i32) };
    // Actual detached mount, never attached to the host. Includes 5.14 NOSYMFOLLOW.
    #[repr(C)] struct Attr { set:u64, clear:u64, propagation:u64, userns:u64 }
    let attr = Attr { set: 1 | 2 | 4 | 8 | 0x00200000, clear: 0, propagation: libc::MS_PRIVATE, userns: 0 };
    if unsafe { libc::syscall(libc::SYS_mount_setattr, mount.as_raw_fd(), c"".as_ptr(),
        libc::AT_EMPTY_PATH as u32 | 0x8000u32, &attr, std::mem::size_of_val(&attr)) } != 0 { return Err(Error::UnsupportedKernel); }
    // Invalid source descriptors must be rejected, rather than blocked/unknown.
    let tree = unsafe { libc::syscall(libc::SYS_open_tree, -1i32, c"".as_ptr(),
        1u32 | libc::O_CLOEXEC as u32 | libc::AT_EMPTY_PATH as u32) };
    if tree != -1 || super::errno() != libc::EBADF { return Err(Error::UnsupportedKernel); }
    let moved = unsafe { libc::syscall(libc::SYS_move_mount, -1i32, c"".as_ptr(), -1i32, c"".as_ptr(), 4u32 | 0x40u32) };
    if moved != -1 || super::errno() != libc::EBADF { return Err(Error::UnsupportedKernel); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn readiness_preserves_detected_landlock_abi_and_policy_ceiling() {
        assert_eq!(landlock_evidence(0), Err(Error::UnsupportedKernel));
        for abi in 1..=7 { assert_eq!(landlock_evidence(abi), Ok((abi, abi))); }
        for abi in [8, 12, u32::MAX] { assert_eq!(landlock_evidence(abi), Ok((abi, 7))); }
    }

    #[test]
    #[ignore = "iso_root_: root Linux VM with mount APIs and Landlock; kernel evidence only"]
    fn iso_root_landlock_probe_reports_detected_and_selected_abi() {
        assert_eq!(unsafe { libc::geteuid() }, 0);
        let ready = probe().expect("required kernel APIs");
        assert_eq!(ready.landlock_abi, crate::landlock::probe_abi().unwrap());
        assert_eq!(ready.landlock_ruleset_abi,
            crate::landlock::Policy::for_abi(ready.landlock_abi).unwrap().abi());
    }
    #[test]
    fn readiness_version_requires_supported_kernel_not_distribution_name() {
        for version in ["5.15.0-ubuntu", "6.1.12", "6.8.0"] { assert!(kernel_version(version).is_ok()); }
        for version in ["5.14.99", "5", "Ubuntu 22.04", "6.x.0", "-1.8"] { assert!(kernel_version(version).is_err()); }
    }
}
