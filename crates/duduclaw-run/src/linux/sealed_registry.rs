//! B1's root-only manifest transport. Not a gateway wire format or disk registry.
//! The authenticated one-shot handshake is the only entry point; no pathname,
//! environment override or unsealed employee fd can supply launch authority.
use std::{io::Write, os::fd::{AsRawFd, FromRawFd, OwnedFd}};
use duduclaw_isolation::{ids::RunId, ipc::MAX_FRAME_BYTES};
use serde::{Deserialize, Serialize};
use crate::{Error, Result, validation::{Access, FileKind, REQUIRED_SEALS, TMPFS_MAGIC}};
use super::{bootstrap::{self, Challenge, RootRegistry}, fd, runner::TrustedRun};

const DOMAIN: &str = "duduclaw-root-launch-v1";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    domain: String,
    run: RunId,
    nonce: [u8; 32],
    plan: TrustedRun,
}

#[derive(Default)]
pub struct SealedRegistry { consumed: bool }
impl RootRegistry for SealedRegistry {
    fn take_run(&mut self, _: RunId) -> Result<TrustedRun> { Err(Error::RegistryUnavailable) }
    fn take_received_run(&mut self, run: RunId, nonce: [u8; 32], fds: &mut Vec<OwnedFd>) -> Result<TrustedRun> {
        if std::mem::replace(&mut self.consumed, true) || fds.is_empty() { return Err(Error::RegistryUnavailable); }
        let manifest = fds.remove(0);
        decode(manifest, run, nonce)
    }
}

fn decode(manifest: OwnedFd, run: RunId, nonce: [u8; 32]) -> Result<TrustedRun> {
    let observed = fd::observe(manifest.as_raw_fd())?;
    if observed.uid != 0 || observed.kind != FileKind::Regular || observed.fs_type != TMPFS_MAGIC
        || !observed.cloexec || !matches!(observed.access, Access::Read | Access::ReadWrite)
        || observed.seals.is_none_or(|seals| seals & REQUIRED_SEALS != REQUIRED_SEALS)
        || observed.size == 0 || observed.size > MAX_FRAME_BYTES as u64 {
        return Err(Error::Descriptors);
    }
    // pread-style offset isolation: the root sender's offset is never trusted.
    let mut bytes = vec![0; observed.size as usize];
    use std::os::unix::fs::FileExt;
    let file = std::fs::File::from(manifest);
    file.read_exact_at(&mut bytes, 0).map_err(|_| Error::Descriptors)?;
    let manifest: Manifest = serde_json::from_slice(&bytes).map_err(|_| Error::Bootstrap)?;
    if manifest.domain != DOMAIN || manifest.run != run || manifest.nonce != nonce || nonce == [0; 32] {
        return Err(Error::Bootstrap);
    }
    manifest.plan.validate()?;
    Ok(manifest.plan)
}

/// Root B1 calls after unit/cgroup readback and cancellation fencing. The exact
/// descriptors are already opened and digest-checked from its root publication.
pub fn send(fd: i32, challenge: Challenge, run: RunId, plan: TrustedRun, rights: &[i32]) -> Result<()> {
    if unsafe { libc::geteuid() } != 0 || challenge.nonce == [0; 32]
        || rights.len() != plan.expected_fd_count() || rights.len() + 1 > bootstrap::MAX_BOOTSTRAP_FDS {
        return Err(Error::Descriptors);
    }
    plan.validate()?;
    let bytes = serde_json::to_vec(&Manifest { domain: DOMAIN.into(), run, nonce: challenge.nonce, plan })
        .map_err(|_| Error::Bootstrap)?;
    if bytes.len() > MAX_FRAME_BYTES { return Err(Error::Descriptors); }
    let raw = unsafe { libc::memfd_create(c"duduclaw-root-plan".as_ptr(), libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING) };
    if raw < 0 { return Err(Error::Descriptors); }
    let mut file = unsafe { std::fs::File::from_raw_fd(raw) };
    file.write_all(&bytes).map_err(|_| Error::Descriptors)?;
    if unsafe { libc::fcntl(raw, libc::F_ADD_SEALS, REQUIRED_SEALS as i32) } != 0 { return Err(Error::Descriptors); }
    let mut descriptors = vec![raw]; descriptors.extend_from_slice(rights);
    bootstrap::send_offer(fd, challenge, run, &descriptors)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sealed_registry_never_resolves_a_path_or_retries_a_failed_offer() {
        let mut registry = SealedRegistry::default();
        assert!(registry.take_run(RunId([1; 16])).is_err());
        assert!(registry.take_received_run(RunId([1; 16]), [1; 32], &mut vec![]).is_err());
        assert!(registry.consumed);
        let mut fds = vec![std::fs::File::open("/dev/null").unwrap().into()];
        assert!(registry.take_received_run(RunId([1; 16]), [1; 32], &mut fds).is_err());
        assert_eq!(fds.len(), 1);
    }

    #[test]
    fn manifest_refuses_unsealed_and_special_descriptors() {
        assert!(decode(std::fs::File::open("/dev/null").unwrap().into(), RunId([1; 16]), [1; 32]).is_err());
        let raw = unsafe { libc::memfd_create(c"unsealed-plan".as_ptr(), libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING) };
        assert!(raw >= 0);
        let mut file = unsafe { std::fs::File::from_raw_fd(raw) };
        file.write_all(b"{}").unwrap();
        assert!(decode(file.into(), RunId([1; 16]), [1; 32]).is_err());
    }
}

#[cfg(test)]
#[path = "sealed_registry_root_tests.rs"]
mod root_tests;
