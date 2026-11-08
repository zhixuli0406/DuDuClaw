//! Root-only production manifest transport tests. These do not execute a unit
//! or validate runtime mounts; root-fixture covers the trampoline launch path.
use super::*;
use std::{ffi::CString, os::fd::AsRawFd};
use super::super::{mounts::RootConfig, runner::{MountIdentity, Program}};
use crate::{seccomp::{Architecture, RuntimeProfile}, validation::Identity};

const RUN: RunId = RunId([41; 16]);

fn root() { assert_eq!(unsafe { libc::geteuid() }, 0, "requires native root Linux; no skip"); }
fn pair() -> (OwnedFd, OwnedFd) {
    let mut fds = [-1; 2];
    assert_eq!(unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
        0, fds.as_mut_ptr()) }, 0);
    unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
}
fn plan() -> TrustedRun {
    let seccomp = RuntimeProfile { allowed_syscalls: vec![libc::SYS_read as u32, libc::SYS_write as u32,
        libc::SYS_execveat as u32, libc::SYS_exit_group as u32] };
    let seccomp_digest = seccomp.digest(Architecture::native().unwrap()).unwrap();
    TrustedRun { pty_size: None,
        root: RootConfig { employee_uid: 70000, employee_gid: 70000, pty: false, pty_limit: 1,
            work_noexec: true, tmp_noexec: true, tmpfs_bytes: 1024 * 1024, tmpfs_inodes: 64 },
        mounts: [MountIdentity { mount: 17, fs_type: TMPFS_MAGIC }; 5],
        supervisor_uid: 997, supervisor_gid: 997, gateway_uid: 998, gateway_mount: 0,
        program: Program { identity: Identity { dev_major: 0, dev_minor: 1, inode: 2, mount: 17 }, script: None,
            argv: vec![CString::new("fixture").unwrap()], environment: vec![], seccomp, seccomp_digest },
        forwarder: None, prompt_bytes: 0, attachments: vec![], mcp: false, proc_subset_pid: true, stdin: false,
        launcher: None,
    }
}
#[test]
fn sealed_transport_test_plan_obeys_production_policy_validation() { plan().validate().unwrap(); }

fn manifest(run: RunId, nonce: [u8; 32], sealed: bool) -> OwnedFd {
    let raw = unsafe { libc::memfd_create(c"root-manifest-test".as_ptr(), libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING) };
    assert!(raw >= 0);
    let mut file = unsafe { std::fs::File::from_raw_fd(raw) };
    file.write_all(&serde_json::to_vec(&Manifest { domain: DOMAIN.into(), run, nonce, plan: plan() }).unwrap()).unwrap();
    if sealed { assert_eq!(unsafe { libc::fcntl(raw, libc::F_ADD_SEALS, REQUIRED_SEALS as i32) }, 0); }
    file.into()
}

#[test]
#[ignore = "requires root native Linux; production bootstrap transport only, no systemd/unit launch"]
fn iso_root_e2e_sealed_registry_transport_roundtrip() {
    root();
    let (sender, receiver) = pair();
    let expected = serde_json::to_value(plan()).unwrap();
    let receive = std::thread::spawn(move || {
        let mut registry = SealedRegistry::default();
        let (plan, rights) = bootstrap::handshake(receiver.as_raw_fd(), &mut registry).unwrap();
        assert!(registry.consumed);
        assert_eq!(rights.len(), plan.expected_fd_count());
        for right in &rights { assert!(fd::observe(right.as_raw_fd()).unwrap().cloexec); }
        assert!(registry.take_received_run(RUN, [1; 32], &mut vec![]).is_err());
        (serde_json::to_value(plan).unwrap(), rights.iter().map(|right|
            fd::observe(right.as_raw_fd()).unwrap().identity).collect::<Vec<_>>())
    });
    let challenge = bootstrap::receive_challenge(sender.as_raw_fd()).unwrap();
    let rights: Vec<std::fs::File> = (0..plan().expected_fd_count()).map(|index|
        std::fs::File::open(if index % 2 == 0 { "/dev/null" } else { "/dev/zero" }).unwrap()).collect();
    let identities = rights.iter().map(|right| fd::observe(right.as_raw_fd()).unwrap().identity).collect::<Vec<_>>();
    send(sender.as_raw_fd(), challenge, RUN, plan(), &rights.iter().map(AsRawFd::as_raw_fd).collect::<Vec<_>>()).unwrap();
    let (actual, received_identities) = receive.join().unwrap();
    assert_eq!(actual, expected);
    assert_eq!(received_identities, identities, "manifest fd must be consumed without shifting the ordered launch rights");
}

#[test]
#[ignore = "requires root native Linux sealed memfd ownership and statx"]
fn iso_root_e2e_sealed_registry_rejects_manifest_nonce_run_mismatch() {
    root();
    for mode in 0..3 {
        let (sender, receiver) = pair();
        let receive = std::thread::spawn(move || {
            let mut registry = SealedRegistry::default();
            let error = bootstrap::handshake(receiver.as_raw_fd(), &mut registry).err();
            assert!(registry.consumed);
            let mut retry = vec![manifest(RUN, [1; 32], true)];
            assert!(matches!(registry.take_received_run(RUN, [1; 32], &mut retry), Err(Error::RegistryUnavailable)));
            assert_eq!(retry.len(), 1, "failed offers consume the registry without consuming a retry's fd");
            error
        });
        let challenge = bootstrap::receive_challenge(sender.as_raw_fd()).unwrap();
        let mut nonce = challenge.nonce;
        let run = if mode == 0 { RunId([42; 16]) } else { RUN };
        if mode == 1 { nonce[0] ^= 1; }
        if mode == 2 { nonce = [0; 32]; }
        let mismatched = manifest(run, nonce, true);
        // The outer offer uses the genuine challenge; only the sealed root plan
        // is mismatched, so this reaches production manifest binding checks.
        bootstrap::send_offer(sender.as_raw_fd(), challenge, RUN, &[mismatched.as_raw_fd()]).unwrap();
        assert_eq!(receive.join().unwrap(), Some(Error::Bootstrap));
    }
}

#[test]
#[ignore = "requires root native Linux; actual SCM_RIGHTS handshake rejects an unsealed manifest"]
fn iso_root_e2e_sealed_registry_rejects_unsealed_manifest() {
    root();
    let (sender, receiver) = pair();
    let receive = std::thread::spawn(move || {
        bootstrap::handshake(receiver.as_raw_fd(), &mut SealedRegistry::default()).err()
    });
    let challenge = bootstrap::receive_challenge(sender.as_raw_fd()).unwrap();
    let unsealed = manifest(RUN, challenge.nonce, false);
    bootstrap::send_offer(sender.as_raw_fd(), challenge, RUN, &[unsealed.as_raw_fd()]).unwrap();
    assert_eq!(receive.join().unwrap(), Some(Error::Descriptors));
}
