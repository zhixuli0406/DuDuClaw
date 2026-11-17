//! The one way the trampoline creates a child together with its pidfd: namespace
//! init (`runner::launch`, §4 step 7) and every HX helper (`launcher`).
//!
//! Legacy `clone`, never `clone3`: systemd's `RestrictNamespaces=` (set on every
//! run unit) and our own `seccomp::common` return ENOSYS for `clone3`, whose flags
//! a seccomp filter cannot inspect. `CLONE_PIDFD | SIGCHLD` carries no namespace
//! flag; a new PID namespace still comes only from a preceding `unshare`.
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use crate::seccomp::Architecture;

/// `CLONE_PIDFD` (Linux 5.2 for legacy `clone`; the kernel minimum is 5.15).
pub(crate) const CLONE_PIDFD: u64 = 0x1000;
/// Exactly these flags; nothing else is ever passed.
pub(crate) const FLAGS: u64 = CLONE_PIDFD | libc::SIGCHLD as u64;

/// Raw `clone` arguments in kernel order. Stack NULL gives fork semantics;
/// `CLONE_PIDFD` stores the pidfd at `parent_tid`. `child_tid` and `tls` are 0
/// (no SETTID/SETTLS flag) but are placed by the per-architecture ABI anyway:
/// x86_64 (flags, stack, parent_tid, child_tid, tls);
/// aarch64 (flags, stack, parent_tid, tls, child_tid).
pub(crate) fn clone_arguments(arch: Architecture, pidfd_slot: usize) -> [usize; 5] {
    abi_order(arch, FLAGS as usize, 0, pidfd_slot, 0, 0)
}
fn abi_order(arch: Architecture, flags: usize, stack: usize, parent_tid: usize, child_tid: usize, tls: usize) -> [usize; 5] {
    match arch {
        Architecture::X86_64 => [flags, stack, parent_tid, child_tid, tls],
        Architecture::Aarch64 => [flags, stack, parent_tid, tls, child_tid],
    }
}

pub(crate) enum Cloned {
    /// In the child. It never holds or receives the pidfd.
    Child,
    Parent { pid: libc::pid_t, pidfd: OwnedFd },
}

pub(crate) enum CloneError {
    /// No child exists.
    Failed,
    /// A child exists but no valid pidfd came back: the caller must kill and reap it.
    NoPidfd(libc::pid_t),
}

/// # Safety
/// Fork semantics: call from a single-threaded process only; the child continues
/// on a copy of this stack and must leave via exec or `_exit`.
pub(crate) unsafe fn clone_with_pidfd() -> Result<Cloned, CloneError> {
    let arch = Architecture::native().map_err(|_| CloneError::Failed)?;
    let mut pidfd: RawFd = -1;
    let [a, b, c, d, e] = clone_arguments(arch, &mut pidfd as *mut RawFd as usize);
    let pid = unsafe { libc::syscall(libc::SYS_clone, a, b, c, d, e) };
    if pid < 0 { return Err(CloneError::Failed); }
    if pid == 0 { return Ok(Cloned::Child); }
    let pid = pid as libc::pid_t;
    if pidfd < 0 { return Err(CloneError::NoPidfd(pid)); }
    Ok(Cloned::Parent { pid, pidfd: unsafe { OwnedFd::from_raw_fd(pidfd) } })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clone_flags_are_exactly_pidfd_and_sigchld() {
        assert_eq!(FLAGS, 0x1000 | 17);
        assert_eq!(FLAGS, libc::CLONE_PIDFD as u64 | libc::SIGCHLD as u64);
        // No namespace, thread, VM or SETTID/SETTLS flag; high half clear.
        let forbidden = (libc::CLONE_NEWNS | libc::CLONE_NEWCGROUP | libc::CLONE_NEWUTS | libc::CLONE_NEWIPC
            | libc::CLONE_NEWUSER | libc::CLONE_NEWPID | libc::CLONE_NEWNET | 0x80 | libc::CLONE_VM
            | libc::CLONE_THREAD | libc::CLONE_FILES | libc::CLONE_SETTLS | libc::CLONE_PARENT_SETTID
            | libc::CLONE_CHILD_SETTID | libc::CLONE_CHILD_CLEARTID) as u32 as u64;
        assert_eq!(FLAGS & forbidden, 0);
        assert_eq!(FLAGS >> 32, 0);
    }

    #[test]
    fn clone_arguments_follow_each_architecture_abi() {
        let slot = 0x7fff_1234usize;
        for arch in [Architecture::X86_64, Architecture::Aarch64] {
            let args = clone_arguments(arch, slot);
            assert_eq!(args[0], FLAGS as usize);
            assert_eq!(args[1], 0, "NULL stack: fork semantics");
            assert_eq!(args[2], slot, "CLONE_PIDFD writes the pidfd to parent_tid");
            assert_eq!(&args[3..], &[0, 0]);
        }
        // Distinct sentinels: child_tid = 4, tls = 5.
        assert_eq!(abi_order(Architecture::X86_64, 1, 2, 3, 4, 5), [1, 2, 3, 4, 5]);
        assert_eq!(abi_order(Architecture::Aarch64, 1, 2, 3, 4, 5), [1, 2, 3, 5, 4]);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn clone_arguments_native_x86_64() {
        assert_eq!(Architecture::native(), Ok(Architecture::X86_64));
        assert_eq!(clone_arguments(Architecture::native().unwrap(), 9), [FLAGS as usize, 0, 9, 0, 0]);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn clone_arguments_native_aarch64() {
        assert_eq!(Architecture::native(), Ok(Architecture::Aarch64));
        assert_eq!(clone_arguments(Architecture::native().unwrap(), 9), [FLAGS as usize, 0, 9, 0, 0]);
    }
}
