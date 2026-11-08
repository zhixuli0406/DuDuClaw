//! HX6 security fix round (findings 1, 2): the recording helper's F_H may
//! carry `SelfOnlyCall::NestedSandbox` (Landlock + a stacked filter, nothing
//! else) and the `PR_SET_CHILD_SUBREAPER` prctl option; F_L holds the same
//! calls as L_restrict, so F_L ∩ F_H lets the helper install the nested
//! browser sandbox and nothing wider. Decoded BPF like `seccomp::tests`.
use super::*;
use super::tests::evaluate;
use crate::helper_policy::{seccomp_within_parent, nested_sandbox_calls, SelfOnlyCall, LANDLOCK_CALLS, MAX_SELF_ONLY,
    PR_SET_CHILD_SUBREAPER};

const ARCHES: [Architecture; 2] = [Architecture::X86_64, Architecture::Aarch64];
fn parent(arch: Architecture) -> RuntimeProfile {
    RuntimeProfile { allowed_syscalls: vec![arch.nr(0, 63), arch.nr(1, 64), arch.nr(41, 198), arch.execveat(), arch.exit_group()] }
}
fn profile(arch: Architecture) -> RuntimeProfile {
    RuntimeProfile { allowed_syscalls: vec![arch.nr(0, 63), arch.nr(41, 198), arch.execveat(), arch.exit_group()] }
}
fn f_h(arch: Architecture, self_only: &[SelfOnlyCall]) -> Result<Filter, FilterError> {
    let (parent, profile) = (parent(arch), profile(arch));
    helper(arch, &parent, parent.digest(arch).unwrap(), &profile, profile.digest(arch).unwrap(), self_only, true)
}

#[test]
fn hx6fix_nested_sandbox_self_only_is_landlock_and_stacked_filter_only() {
    for arch in ARCHES {
        let audit = arch.audit();
        let recording = f_h(arch, &[SelfOnlyCall::NestedSandbox,
            SelfOnlyCall::Prctl { options: vec![PR_SET_CHILD_SUBREAPER, 37, 39] }]).unwrap();
        let plain = f_h(arch, &[]).unwrap();
        let launcher = launcher(arch, &parent(arch), parent(arch).digest(arch).unwrap()).unwrap();
        // Stacking one more filter: SET_MODE_FILTER with flags 0 only.
        for filter in [&recording, &launcher] {
            assert_eq!(evaluate(filter, audit, arch.seccomp(), [1, 0, 0x1000, 0, 0, 0]), ALLOW, "{arch:?}");
            for args in [[0u64, 0, 0, 0, 0, 0], [1, 1, 0, 0, 0, 0] /* TSYNC */, [1, 8, 0, 0, 0, 0] /* NEW_LISTENER */,
                [2, 0, 0, 0, 0, 0], [3, 0, 0, 0, 0, 0]] {
                assert_eq!(evaluate(filter, audit, arch.seccomp(), args), DENY, "{arch:?} seccomp {args:?}");
            }
            for nr in LANDLOCK_CALLS { assert_eq!(evaluate(filter, audit, nr, [3, 1, 0, 0, 0, 0]), ALLOW, "{arch:?} {nr}"); }
            // Finding 2: the helper may become a child subreaper (F_L ∩ F_H).
            assert_eq!(evaluate(filter, audit, arch.prctl(), [u64::from(PR_SET_CHILD_SUBREAPER), 1, 0, 0, 0, 0]), ALLOW);
            assert_eq!(evaluate(filter, audit, arch.prctl(), [22 /* SET_SECCOMP */, 2, 0, 0, 0, 0]), DENY);
        }
        // Without the signed exception F_H holds none of them.
        for nr in nested_sandbox_calls(arch) { assert_eq!(evaluate(&plain, audit, nr, [1, 0, 0, 0, 0, 0]), DENY, "{arch:?} {nr}"); }
        assert_eq!(evaluate(&plain, audit, arch.prctl(), [u64::from(PR_SET_CHILD_SUBREAPER), 1, 0, 0, 0, 0]), DENY);
        // The common refusals stay embedded.
        for nr in denied_syscalls(arch) { assert_eq!(evaluate(&recording, audit, nr, [0; 6]), DENY); }
        // Policy: once, never next to a flat entry for the same calls, at most one of each kind.
        let (parent, flat) = (parent(arch), profile(arch));
        assert!(seccomp_within_parent(arch, &parent, &flat, &[SelfOnlyCall::NestedSandbox]));
        assert!(!seccomp_within_parent(arch, &parent, &flat, &[SelfOnlyCall::NestedSandbox, SelfOnlyCall::NestedSandbox]));
        let mut wide_parent = parent.clone(); wide_parent.allowed_syscalls.push(LANDLOCK_CALLS[0]);
        let mut wide = flat.clone(); wide.allowed_syscalls.push(LANDLOCK_CALLS[0]);
        assert!(!seccomp_within_parent(arch, &wide_parent, &wide, &[SelfOnlyCall::NestedSandbox]));
        let four = [SelfOnlyCall::NestedSandbox, SelfOnlyCall::Prlimit64SelfOnly,
            SelfOnlyCall::Prctl { options: vec![PR_SET_CHILD_SUBREAPER] }, SelfOnlyCall::Prlimit64SelfOnly];
        assert!(four.len() > MAX_SELF_ONLY && !seccomp_within_parent(arch, &parent, &flat, &four));
        assert_eq!(serde_json::to_value(SelfOnlyCall::NestedSandbox).unwrap(), serde_json::json!({"kind":"nested_sandbox"}));
    }
}
