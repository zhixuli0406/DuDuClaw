//! HX security fix round (findings 2–4), trampoline layer: F_H denies the
//! socket family for an offline helper, and the path-based metadata writes
//! Landlock does not mediate plus SysV/POSIX-mq IPC for every helper, with
//! EPERM, whatever the signed list says. Decoded-BPF like `seccomp::tests`.
use super::*;
use super::tests::evaluate;
use crate::helper_policy::{ipc_namespace_calls, landlock_uncovered_calls, offline_network_calls};

const ARCHES: [Architecture; 2] = [Architecture::X86_64, Architecture::Aarch64];
/// A parent that itself allows every refused call (so only F_H can deny them).
fn wide_parent(arch: Architecture) -> RuntimeProfile {
    let mut calls = vec![arch.nr(0, 63), arch.nr(1, 64), arch.nr(77, 46) /* ftruncate */, arch.execveat(), arch.exit_group()];
    calls.extend(offline_network_calls(arch));
    calls.extend_from_slice(landlock_uncovered_calls(arch));
    calls.extend(ipc_namespace_calls(arch));
    RuntimeProfile { allowed_syscalls: calls }
}
fn filter(arch: Architecture, egress: bool) -> Filter {
    let parent = wide_parent(arch);
    // The helper list deliberately names every refused call too.
    let profile = parent.clone();
    helper(arch, &parent, parent.digest(arch).unwrap(), &profile, profile.digest(arch).unwrap(), &[], egress).unwrap()
}

#[test]
fn hxfix_offline_helper_socket_eperm_and_uncovered_calls_denied() {
    for arch in ARCHES {
        let (offline, egress) = (filter(arch, false), filter(arch, true));
        let audit = arch.audit();
        // Finding 2: every family, and socketpair/connect/bind/listen/accept/accept4.
        for family in [1u64, 2, 10, 16] {
            assert_eq!(evaluate(&offline, audit, arch.nr(41, 198), [family, 1, 0, 0, 0, 0]), DENY, "{arch:?} {family}");
        }
        for nr in offline_network_calls(arch) { assert_eq!(evaluate(&offline, audit, nr, [1, 1, 0, 0, 0, 0]), DENY, "{arch:?} {nr}"); }
        // The egress helper keeps the parent's socket rules (AF_INET allowed, AF_PACKET still EPERM).
        assert_eq!(evaluate(&egress, audit, arch.nr(41, 198), [2, 1, 0, 0, 0, 0]), ALLOW);
        assert_eq!(evaluate(&egress, audit, arch.nr(41, 198), [17, 1, 0, 0, 0, 0]), DENY);
        // Findings 3 and 4: denied in both, even though the signed list holds them.
        for nr in landlock_uncovered_calls(arch).iter().copied().chain(ipc_namespace_calls(arch)) {
            for filter in [&offline, &egress] { assert_eq!(evaluate(filter, audit, nr, [0; 6]), DENY, "{arch:?} {nr}"); }
        }
        // fd forms and ordinary calls are untouched.
        for nr in [arch.nr(0, 63), arch.nr(1, 64), arch.nr(77, 46), arch.execveat()] {
            assert_eq!(evaluate(&offline, audit, nr, [3, 0, 0, 0, 0, 0]), ALLOW, "{arch:?} {nr}");
        }
        // Common refusals and the foreign-arch kill still come first.
        for nr in denied_syscalls(arch) { assert_eq!(evaluate(&offline, audit, nr, [0; 6]), DENY); }
        assert_eq!(evaluate(&offline, 0x40000003, 0, [0; 6]), KILL);
    }
}
