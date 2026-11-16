//! Linux-only trusted OS adapters. No runtime paths are accepted from RPC callers.
pub mod files;
pub mod transport;
pub mod download;
pub mod service;
pub mod launcher;
pub mod activation;
pub mod trust_admin;
pub mod emergency;
pub mod bootstrap;
pub mod bootstrap_verify;
pub mod trust_init;

use crate::{Error, Result};
pub const STATE_ROOT: &str = "/var/lib/duduclaw-updated";
pub const CONFIG_ROOT: &str = "/etc/duduclaw-updated";
pub const INSTALL_ROOT: &str = "/opt/duduclaw-isolated";

pub fn harden() -> Result<()> {
    // SAFETY: scalar prctl operations do not dereference pointers.
    unsafe {
        if libc::geteuid() != 0 || libc::prctl(libc::PR_SET_DUMPABLE, 0) != 0
            || libc::prctl(libc::PR_GET_DUMPABLE) != 0
            || libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
            || libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) != 1 { return Err(Error::Maintenance); }
        libc::umask(0o077);
    }
    Ok(())
}
/// Real AND effective uid 0 (plan §4.1 step 1). `harden` checks only the effective
/// uid; root entry points call this right after it, before reading anything.
pub fn require_root() -> Result<()> {
    // SAFETY: getuid/geteuid take no arguments and cannot fail.
    if unsafe { libc::getuid() } != 0 || unsafe { libc::geteuid() } != 0 { return Err(Error::Maintenance); }
    Ok(())
}
pub fn now() -> Result<u64> {
    Ok(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| Error::Maintenance)?.as_secs())
}
