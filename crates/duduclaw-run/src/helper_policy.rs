//! Root-signed in-run helper vocabulary (HX D8 revision §2.3, §2.4). Pure and
//! host-testable: spawnd's publication checks and the trampoline's launcher plan
//! share these closed enums, so neither side can name a path or syscall the other
//! did not review. Nothing here is deserialized from gateway IPC.
use std::ffi::CStr;
use crate::{landlock::{PathClass, Policy, MAX_KNOWN_ABI}, seccomp::{Architecture, RuntimeProfile}};

/// Check 5: at most this many helper bindings per parent profile.
pub const MAX_HELPER_BINDINGS: usize = 16;
/// Check 3: absolute helper timeout ceiling.
pub const HELPER_TIMEOUT_MAX_MS: u64 = duduclaw_isolation::ipc::MAX_HELPER_TIMEOUT_MS;
/// Check 7 [F16]: proposed flush window for a recording; HX6 sizes it from measurement.
pub const RECORDING_MIN_GRACE_MS: u64 = 12_000;
/// Root-opened cgroup descriptors in a launcher bootstrap, in this order:
/// `rt-runtime/cgroup.procs`, `rt-runtime/cgroup.kill`, `hx/fg/cgroup.procs`, `hx/rec/cgroup.procs`.
pub const HELPER_CGROUP_FDS: usize = 4;

/// The runtime's Landlock list. `runner::restrict_filesystem` installs exactly
/// this table, so check 6 is computed against the list the kernel enforces.
pub const RUNTIME_LANDLOCK: [(&CStr, PathClass); 15] = [
    (c"/usr", PathClass::ReadExecute), (c"/bin", PathClass::ReadExecute), (c"/sbin", PathClass::ReadExecute),
    (c"/lib", PathClass::ReadExecute), (c"/lib64", PathClass::ReadExecute), (c"/runtime", PathClass::ReadExecute),
    (c"/etc", PathClass::ReadOnly), (c"/config", PathClass::ReadOnly), (c"/proc", PathClass::ReadOnly),
    (c"/home/employee", PathClass::Persistent), (c"/work", PathClass::Persistent),
    (c"/tmp", PathClass::Ephemeral), (c"/run", PathClass::Ephemeral), (c"/dev/shm", PathClass::Ephemeral),
    (c"/dev", PathClass::Devices),
];
/// Paths no helper may reach, even though the runtime can [F9, F15].
pub const HELPER_FORBIDDEN: [&str; 6] = ["/home/employee", "/config", "/run", "/tmp", "/dev/shm", "/runtime"];

/// Fixed helper filesystem grants [F15]. Helper code (`/helpers/<id>`) and private
/// scratch (`/helper-tmp/<call>`) are the stated exception and are not variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HelperFs { Work, WorkRecordings, SystemRo }
impl HelperFs {
    pub const ALL: [Self; 3] = [Self::Work, Self::WorkRecordings, Self::SystemRo];
    pub fn paths(self) -> &'static [(&'static str, PathClass)] {
        match self {
            Self::SystemRo => &[("/usr", PathClass::ReadExecute), ("/lib", PathClass::ReadExecute),
                ("/lib64", PathClass::ReadExecute), ("/etc", PathClass::ReadOnly), ("/proc", PathClass::ReadOnly),
                ("/dev/null", PathClass::Devices), ("/dev/urandom", PathClass::Devices)],
            Self::Work => &[("/work", PathClass::Persistent)],
            Self::WorkRecordings => &[("/work/recordings", PathClass::Persistent)],
        }
    }
}

/// `hx/fg` or `hx/rec` (Q4 = A: one of each per run). One type for the signed
/// policy, the spawnd IPC (`HelperCall.slot`) and the launcher wire (HX3a).
pub use duduclaw_isolation::ipc::HelperSlot;

/// The closed set of argument-constrained, self-only calls F_H may hold without
/// the parent profile holding them [F3]. HX3a compiles these into BPF argument
/// checks; a flat allowlist entry for the same syscall is a different authority.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SelfOnlyCall {
    /// `prlimit64(0, …)`: the caller's own limits only.
    Prlimit64SelfOnly,
    /// `prctl(option, …)` with each option in [`PRCTL_SELF_ONLY`]; `PR_SET_DUMPABLE` with arg 0 only.
    Prctl { options: Vec<u32> },
    /// HX6 security fix round (findings 1, 2): the recording helper's nested
    /// browser sandbox. `landlock_create_ruleset`/`landlock_add_rule`/
    /// `landlock_restrict_self` (they only restrict the caller) and
    /// `seccomp(SECCOMP_SET_MODE_FILTER, 0, …)` (stacking a further filter;
    /// no TSYNC, listener or other operation). Recording bindings only
    /// (publication check); the same calls F_L holds as L_restrict.
    NestedSandbox,
}
pub const PR_SET_PDEATHSIG: u32 = 1;
pub const PR_SET_DUMPABLE: u32 = 4;
/// HX6 security fix round (finding 2): the caller adopts its own orphaned
/// descendants (so it can reap them); affects no process outside its subtree.
pub const PR_SET_CHILD_SUBREAPER: u32 = 36;
pub const PR_SET_NO_NEW_PRIVS: u32 = 38;
/// Set options that only restrict the caller or supervise its own children,
/// plus read-only `PR_GET_*` queries (PDEATHSIG, DUMPABLE, KEEPCAPS, NAME,
/// SECCOMP, SECUREBITS, TIMERSLACK, CHILD_SUBREAPER, NO_NEW_PRIVS, THP_DISABLE).
pub const PRCTL_SELF_ONLY: [u32; 14] = [PR_SET_PDEATHSIG, 2, 3, PR_SET_DUMPABLE, 7, 16, 21, 27, 30,
    PR_SET_CHILD_SUBREAPER, PR_GET_CHILD_SUBREAPER, PR_SET_NO_NEW_PRIVS, 39, 42];
pub const PR_GET_CHILD_SUBREAPER: u32 = 37;
/// HX6 security round 3 (N3): the `PRCTL_SELF_ONLY` options only the
/// recording binding (its nested browser sandbox's subreaper) may hold. Every
/// other binding is refused at publication (spawnd `validate_launcher`) and in
/// the trampoline's launcher plan, like [`SelfOnlyCall::NestedSandbox`].
pub const RECORDING_ONLY_PRCTL: [u32; 2] = [PR_SET_CHILD_SUBREAPER, PR_GET_CHILD_SUBREAPER];
/// N3: `self_only` holds a recording-only exception ([`SelfOnlyCall::NestedSandbox`]
/// or a [`RECORDING_ONLY_PRCTL`] option) while `slot` is not the recording slot.
pub fn recording_only_violation(slot: HelperSlot, self_only: &[SelfOnlyCall]) -> bool {
    slot != HelperSlot::Recording && self_only.iter().any(|call| match call {
        SelfOnlyCall::NestedSandbox => true,
        SelfOnlyCall::Prctl { options } => options.iter().any(|option| RECORDING_ONLY_PRCTL.contains(option)),
        SelfOnlyCall::Prlimit64SelfOnly => false,
    })
}
/// `landlock_create_ruleset`, `landlock_add_rule`, `landlock_restrict_self`
/// (the same numbers on x86_64 and asm-generic).
pub const LANDLOCK_CALLS: [u32; 3] = [444, 445, 446];
/// HX6 security fix round (finding 5): the signed browser files of the
/// recording binding, flat names under `/helpers` (fixed-argv digest order of
/// `duduclaw-browser-record-v1`). The launcher binds and grants them (RX) to
/// the recording helper only.
pub const RECORDING_BROWSER_FILES: [&str; 3] = ["node", "playwright.cjs", "chromium"];
pub fn prlimit64(arch: Architecture) -> u32 { match arch { Architecture::X86_64 => 302, Architecture::Aarch64 => 261 } }
pub fn prctl(arch: Architecture) -> u32 { match arch { Architecture::X86_64 => 157, Architecture::Aarch64 => 167 } }

/// HX security fix round, finding 2 (§2.4 "offline helper: F_H omits
/// `socket`"): every call that creates, names or connects a socket. The §2.2
/// control channel is spawnd's own socketpair, never a helper's, so an offline
/// helper needs none of them: `socket`, `socketpair`, `connect`, `bind`,
/// `listen`, `accept`, `accept4`.
pub fn offline_network_calls(arch: Architecture) -> [u32; 7] {
    match arch { Architecture::X86_64 => [41, 53, 42, 49, 50, 43, 288], Architecture::Aarch64 => [198, 199, 203, 200, 201, 202, 242] }
}
/// Finding 3: path-based metadata writes Landlock does not mediate (chmod,
/// utimes and xattr writes on every ABI; `truncate` below ABI 3), so a helper
/// could change files outside its grants (e.g. `/home/employee`) by path:
/// `truncate`, `chmod`, `fchmodat`, `fchmodat2`, `utime`, `utimes`,
/// `utimensat`, `futimesat`, `setxattr`, `lsetxattr`, `removexattr`,
/// `lremovexattr`, `setxattrat`, `removexattrat` (asm-generic has no `chmod`,
/// `utime`, `utimes`, `futimesat`). The fd forms (`ftruncate`, `fchmod`,
/// `fsetxattr`, `fremovexattr`) stay allowed: they act only on a descriptor
/// the helper's Landlock already let it open, and every employee-owned file a
/// helper can open lies inside one of its RW grants. `utimensat` is refused
/// in every form, including `futimens` (`utimensat(fd, NULL, …)`).
///
/// Round 3: the chown family is not mediated by Landlock on any ABI either:
/// `chown`, `lchown`, `fchownat` (path forms) and `fchown` (an unprivileged
/// helper can at most change the group of a file it owns, but there is no
/// reason any helper needs it, so it is refused with the path forms).
/// asm-generic has no `chown`/`lchown`.
pub fn landlock_uncovered_calls(arch: Architecture) -> &'static [u32] {
    match arch {
        Architecture::X86_64 => &[76, 90, 268, 452, 132, 235, 280, 261, 188, 189, 197, 198, 463, 466,
            92, 94, 260, 93],
        Architecture::Aarch64 => &[45, 53, 452, 88, 5, 6, 14, 15, 463, 466, 54, 55],
    }
}
/// Finding 4: IPC-namespace objects (shared with the runtime) that Landlock
/// does not scope: SysV shm/sem/msg and POSIX message queues.
pub fn ipc_namespace_calls(arch: Architecture) -> [u32; 18] {
    match arch {
        Architecture::X86_64 => [29, 30, 31, 67, 64, 65, 66, 220, 68, 69, 70, 71, 240, 241, 242, 243, 244, 245],
        Architecture::Aarch64 => [194, 196, 195, 197, 190, 193, 191, 192, 186, 189, 188, 187, 180, 181, 182, 183, 184, 185],
    }
}
/// The calls no F_H may hold (publication check) and that the trampoline's
/// F_H denies with EPERM before anything else: findings 3 and 4 always, the
/// socket family too for an offline (`!egress`) helper.
pub fn refused_helper_calls(arch: Architecture, egress: bool) -> Vec<u32> {
    let mut calls = landlock_uncovered_calls(arch).to_vec();
    calls.extend(ipc_namespace_calls(arch));
    if !egress { calls.extend(offline_network_calls(arch)); }
    calls
}
/// Publication form of [`refused_helper_calls`]: the first refused call the
/// signed flat F_H allowlist names, if any.
pub fn first_refused_helper_call(arch: Architecture, helper: &RuntimeProfile, egress: bool) -> Option<u32> {
    refused_helper_calls(arch, egress).into_iter().find(|nr| helper.allows(*nr))
}

/// Check 2 [F3]: the flat helper allowlist is a subset of the parent's, and every
/// self-only exception is well formed and not also granted unconstrained.
pub fn seccomp_within_parent(arch: Architecture, parent: &RuntimeProfile, helper: &RuntimeProfile,
    self_only: &[SelfOnlyCall]) -> bool {
    if !helper.allowed_syscalls.iter().all(|nr| parent.allowed_syscalls.contains(nr)) || self_only.len() > MAX_SELF_ONLY { return false; }
    let mut seen = Vec::new();
    for call in self_only {
        let calls = match call {
            SelfOnlyCall::Prlimit64SelfOnly => vec![prlimit64(arch)],
            SelfOnlyCall::Prctl { options } => {
                let mut sorted = options.clone();
                sorted.sort_unstable();
                if options.is_empty() || sorted.windows(2).any(|pair| pair[0] == pair[1])
                    || !options.iter().all(|option| PRCTL_SELF_ONLY.contains(option)) { return false; }
                vec![prctl(arch)]
            }
            SelfOnlyCall::NestedSandbox => nested_sandbox_calls(arch).to_vec(),
        };
        for nr in calls {
            if seen.contains(&nr) || helper.allowed_syscalls.contains(&nr) { return false; }
            seen.push(nr);
        }
    }
    true
}
/// At most one of each [`SelfOnlyCall`] kind.
pub const MAX_SELF_ONLY: usize = 3;
/// The syscalls [`SelfOnlyCall::NestedSandbox`] names: Landlock and `seccomp`.
pub fn nested_sandbox_calls(arch: Architecture) -> [u32; 4] {
    let [create, add, restrict] = LANDLOCK_CALLS;
    [create, add, restrict, match arch { Architecture::X86_64 => 317, Architecture::Aarch64 => 277 }]
}

fn components(path: &str) -> Option<Vec<&str>> {
    let rest = path.strip_prefix('/')?;
    if rest.is_empty() { return Some(vec![]); }
    let parts: Vec<_> = rest.split('/').collect();
    if parts.iter().any(|part| part.is_empty() || *part == "." || *part == "..") { return None; }
    Some(parts)
}
fn beneath(path: &[&str], root: &[&str]) -> bool { path.len() >= root.len() && path[..root.len()] == *root }

/// Check 6 [F15]: `path` with `class` is within the runtime's Landlock list (the
/// union of every enclosing rule) and neither inside nor above a forbidden path.
pub fn path_within_runtime(path: &str, class: PathClass) -> bool {
    let Some(parts) = components(path) else { return false; };
    if parts.is_empty() { return false; }
    for forbidden in HELPER_FORBIDDEN {
        let Some(forbidden) = components(forbidden) else { return false; };
        if beneath(&parts, &forbidden) || beneath(&forbidden, &parts) { return false; }
    }
    let Ok(policy) = Policy::for_abi(MAX_KNOWN_ABI) else { return false; };
    let mut granted = 0;
    for (root, parent) in RUNTIME_LANDLOCK {
        let Some(root) = root.to_str().ok().and_then(components) else { return false; };
        if beneath(&parts, &root) { granted |= policy.rights(parent); }
    }
    let wanted = policy.rights(class);
    wanted != 0 && wanted & !granted == 0
}
pub fn helper_fs_within_runtime(grant: HelperFs) -> bool {
    grant.paths().iter().all(|(path, class)| path_within_runtime(path, *class))
}

/// `/helpers`: one read-only bind per signed helper file (HX3b). Never in the runtime's list.
pub const HELPERS_ROOT: &str = "/helpers";
/// `/helper-tmp`: per-run scratch tmpfs; `/helper-tmp/<call>` per call [F9]. Never in the runtime's list.
pub const HELPER_TMP_ROOT: &str = "/helper-tmp";
/// The launcher's own Landlock rules besides the helper union [F12]. `/dev/null`
/// backs its fds 0/1/2 and each helper's fd 3.
pub const LAUNCHER_FIXED: [(&str, PathClass); 3] = [(HELPERS_ROOT, PathClass::ReadExecute),
    (HELPER_TMP_ROOT, PathClass::Ephemeral), ("/dev/null", PathClass::Devices)];

/// `path` is inside or above a path no helper (and so no launcher) may reach.
pub fn overlaps_forbidden(path: &str) -> bool {
    let Some(parts) = components(path) else { return true; };
    HELPER_FORBIDDEN.iter().any(|forbidden| match components(forbidden) {
        Some(forbidden) => beneath(&parts, &forbidden) || beneath(&forbidden, &parts),
        None => true,
    })
}

/// Launcher Landlock [F12]: `LAUNCHER_FIXED` plus the union of the bound
/// helpers' `HelperFs` paths, deduplicated. `WorkRecordings` contributes `/work`
/// here (still an upper bound of every helper layer, which stacks underneath
/// with the exact `/work/recordings`), so a runtime that leaves something odd at
/// `/work/recordings` can fail one recording call but never the launcher's setup.
/// Never `/home/employee`, `/config`, `/run`, `/tmp`, `/dev/shm`, `/runtime`.
pub fn launcher_landlock(bindings: &[&[HelperFs]]) -> Vec<(&'static str, PathClass)> {
    let mut rules: Vec<(&'static str, PathClass)> = LAUNCHER_FIXED.to_vec();
    for grant in bindings.iter().flat_map(|grants| grants.iter()) {
        let paths: &[(&'static str, PathClass)] = match grant {
            HelperFs::WorkRecordings => HelperFs::Work.paths(),
            other => other.paths(),
        };
        for rule in paths {
            if !rules.iter().any(|(path, _)| path == &rule.0) { rules.push(*rule); }
        }
    }
    rules
}

/// One helper's own Landlock rules from its grants (the per-call
/// `/helpers/<file>` and `/helper-tmp/<call>` rules are added by the launcher).
pub fn helper_landlock(grants: &[HelperFs]) -> Vec<(&'static str, PathClass)> {
    let mut rules: Vec<(&'static str, PathClass)> = Vec::new();
    for rule in grants.iter().flat_map(|grant| grant.paths()) {
        if !rules.iter().any(|(path, _)| path == &rule.0) { rules.push(*rule); }
    }
    rules
}

/// The flat registry name of a signed helper file path `/helpers/<name>`. The
/// generation registry is flat (spawnd `open_verified`), and `/helpers` holds
/// exactly one bind per name, so nested paths are refused.
pub fn helper_file_name(path: &[u8]) -> Option<&str> {
    let name = std::str::from_utf8(path.strip_prefix(b"/helpers/")?).ok()?;
    (!name.is_empty() && name != "." && name != ".." && name.len() <= 255
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn profile(calls: &[u32]) -> RuntimeProfile { RuntimeProfile { allowed_syscalls: calls.to_vec() } }
    #[test]
    fn every_helper_fs_variant_is_inside_the_runtime_list() {
        for grant in HelperFs::ALL { assert!(helper_fs_within_runtime(grant), "{grant:?}"); }
    }
    #[test]
    fn forbidden_and_unlisted_paths_are_refused() {
        for path in ["/home/employee", "/home/employee/.ssh", "/home", "/config/x", "/run/duduclaw", "/tmp",
            "/dev/shm/x", "/runtime/node", "/opt", "/", "/work/../home", "/work//x", "relative"] {
            assert!(!path_within_runtime(path, PathClass::ReadOnly), "{path}");
        }
        // A class wider than the enclosing runtime rule is refused.
        assert!(!path_within_runtime("/etc", PathClass::Persistent));
        assert!(!path_within_runtime("/usr", PathClass::Persistent));
        assert!(path_within_runtime("/work/recordings", PathClass::Persistent));
        assert!(path_within_runtime("/dev/urandom", PathClass::Devices));
        assert!(!path_within_runtime("/dev", PathClass::Devices));
    }
    #[test]
    fn helper_fs_rejects_unknown_variants() {
        assert!(serde_json::from_str::<HelperFs>("\"home\"").is_err());
        assert_eq!(serde_json::from_str::<HelperFs>("\"work_recordings\"").unwrap(), HelperFs::WorkRecordings);
    }
    /// Every subset of the closed `HelperFs` set, as the launcher would see it.
    fn every_grant_set() -> Vec<Vec<HelperFs>> {
        (0..(1u8 << HelperFs::ALL.len())).map(|mask| HelperFs::ALL.iter().enumerate()
            .filter(|(i, _)| mask & (1 << i) != 0).map(|(_, grant)| *grant).collect()).collect()
    }
    #[test]
    fn launcher_landlock_excludes_home_config_run_tmp_shm() {
        let policy = Policy::for_abi(MAX_KNOWN_ABI).unwrap();
        for grants in every_grant_set() {
            let rules = launcher_landlock(&[grants.as_slice()]);
            for (path, _) in &rules {
                assert!(!overlaps_forbidden(path), "launcher rule {path} touches a forbidden path");
                for forbidden in ["/home/employee", "/config", "/run", "/tmp", "/dev/shm", "/runtime", "/"] {
                    assert_ne!(*path, forbidden);
                }
            }
            for fixed in LAUNCHER_FIXED { assert!(rules.contains(&fixed), "{fixed:?}"); }
            let mut paths: Vec<_> = rules.iter().map(|(path, _)| *path).collect();
            paths.sort_unstable(); paths.dedup();
            assert_eq!(paths.len(), rules.len(), "duplicate launcher rule");
            // Union is an upper bound of every helper layer stacked underneath.
            for (path, class) in helper_landlock(&grants) {
                let wanted = policy.rights(class);
                let granted = rules.iter().filter(|(root, _)| path == *root || path.starts_with(&format!("{root}/")))
                    .fold(0, |rights, (_, class)| rights | policy.rights(*class));
                assert_eq!(wanted & !granted, 0, "{path} not covered by the launcher layer");
            }
        }
        assert_eq!(launcher_landlock(&[]), LAUNCHER_FIXED.to_vec());
        // Lookalikes are not forbidden; real forbidden subtrees and parents are.
        for path in [HELPER_TMP_ROOT, HELPERS_ROOT, "/dev/null", "/work", "/tmpfoo"] { assert!(!overlaps_forbidden(path), "{path}"); }
        for path in ["/tmp/x", "/run", "/home", "/dev/shm/a", "/", "/config", "relative", "/work/../tmp"] {
            assert!(overlaps_forbidden(path), "{path}");
        }
    }
    #[test]
    fn helper_layer_rules_stay_inside_runtime_list() {
        for grants in every_grant_set() {
            for (path, class) in helper_landlock(&grants) {
                assert!(path_within_runtime(path, class), "{path}");
                assert!(!overlaps_forbidden(path));
            }
        }
    }
    #[test]
    fn helper_file_names_are_flat_registry_names() {
        assert_eq!(helper_file_name(b"/helpers/node"), Some("node"));
        assert_eq!(helper_file_name(b"/helpers/driver.cjs"), Some("driver.cjs"));
        for bad in [&b"/helpers/"[..], b"/helpers/..", b"/helpers/.", b"/helpers/a/b", b"/runtime/node", b"helpers/x",
            b"/helpers/a b", b"/helpers/\xff", b"/helpers//x"] {
            assert_eq!(helper_file_name(bad), None, "{bad:?}");
        }
    }
    #[test]
    fn self_only_exceptions_are_closed_and_not_duplicated() {
        let arch = Architecture::X86_64;
        let parent = profile(&[0, 1, 231, 322]);
        assert!(seccomp_within_parent(arch, &parent, &profile(&[0, 231, 322]), &[]));
        assert!(!seccomp_within_parent(arch, &parent, &profile(&[0, 2, 231, 322]), &[]));
        assert!(seccomp_within_parent(arch, &parent, &profile(&[231, 322]), &[SelfOnlyCall::Prlimit64SelfOnly,
            SelfOnlyCall::Prctl { options: vec![PR_SET_NO_NEW_PRIVS, PR_SET_DUMPABLE] }]));
        // Unconstrained prlimit64/prctl must come from the parent, never the exception.
        assert!(!seccomp_within_parent(arch, &parent, &profile(&[231, 302, 322]), &[]));
        assert!(!seccomp_within_parent(arch, &profile(&[231, 302, 322]), &profile(&[231, 302, 322]),
            &[SelfOnlyCall::Prlimit64SelfOnly]));
        for options in [vec![], vec![22], vec![1, 1], vec![PR_SET_DUMPABLE, 59]] {
            assert!(!seccomp_within_parent(arch, &parent, &profile(&[231, 322]), &[SelfOnlyCall::Prctl { options }]));
        }
        assert!(!seccomp_within_parent(arch, &parent, &profile(&[231, 322]),
            &[SelfOnlyCall::Prlimit64SelfOnly, SelfOnlyCall::Prlimit64SelfOnly]));
    }
    /// HX6 security round 3 (N3): the subreaper options and the nested
    /// sandbox are recording-only; every other self-only call is slot-neutral.
    #[test]
    fn hx6r3_subreaper_prctl_options_are_recording_only() {
        for option in RECORDING_ONLY_PRCTL { assert!(PRCTL_SELF_ONLY.contains(&option), "{option}"); }
        let subreaper = |options: Vec<u32>| vec![SelfOnlyCall::Prctl { options }];
        for self_only in [subreaper(vec![PR_SET_CHILD_SUBREAPER]), subreaper(vec![PR_SET_NO_NEW_PRIVS, PR_GET_CHILD_SUBREAPER]),
            vec![SelfOnlyCall::NestedSandbox], vec![SelfOnlyCall::Prlimit64SelfOnly, SelfOnlyCall::NestedSandbox]] {
            assert!(recording_only_violation(HelperSlot::Foreground, &self_only), "{self_only:?}");
            assert!(!recording_only_violation(HelperSlot::Recording, &self_only), "{self_only:?}");
        }
        for self_only in [vec![], subreaper(vec![PR_SET_NO_NEW_PRIVS, PR_SET_DUMPABLE, PR_SET_PDEATHSIG]),
            vec![SelfOnlyCall::Prlimit64SelfOnly]] {
            assert!(!recording_only_violation(HelperSlot::Foreground, &self_only), "{self_only:?}");
        }
    }
    #[test]
    fn hxfix_refused_helper_call_tables() {
        for arch in [Architecture::X86_64, Architecture::Aarch64] {
            let offline = refused_helper_calls(arch, false);
            let egress = refused_helper_calls(arch, true);
            let mut sorted = offline.clone(); sorted.sort_unstable(); sorted.dedup();
            assert_eq!(sorted.len(), offline.len(), "{arch:?} duplicate refused call");
            // Socket family only for offline helpers; metadata and IPC always.
            for nr in offline_network_calls(arch) { assert!(offline.contains(&nr) && !egress.contains(&nr), "{nr}"); }
            for nr in landlock_uncovered_calls(arch).iter().chain(ipc_namespace_calls(arch).iter()) {
                assert!(offline.contains(nr) && egress.contains(nr), "{nr}");
            }
            // The fd forms and the calls every helper needs are never refused.
            let keep: [u32; 8] = match arch {
                Architecture::X86_64 => [77 /* ftruncate */, 91 /* fchmod */, 190 /* fsetxattr */, 199 /* fremovexattr */,
                    0, 1, 322, 231],
                Architecture::Aarch64 => [46, 52, 7, 16, 63, 64, 281, 94],
            };
            for nr in keep { assert!(!offline.contains(&nr), "{arch:?} {nr}"); }
            // Round 3: the chown family (path forms and fchown) is refused for every helper.
            let chown: &[u32] = match arch {
                Architecture::X86_64 => &[92 /* chown */, 94 /* lchown */, 260 /* fchownat */, 93 /* fchown */],
                Architecture::Aarch64 => &[54 /* fchownat */, 55 /* fchown */],
            };
            for nr in chown {
                assert!(offline.contains(nr) && egress.contains(nr), "{arch:?} chown {nr}");
                assert_eq!(first_refused_helper_call(arch, &profile(&[*nr]), true), Some(*nr));
            }
            assert_eq!(first_refused_helper_call(arch, &profile(&keep), false), None);
            let socket = offline_network_calls(arch)[0];
            assert_eq!(first_refused_helper_call(arch, &profile(&[socket]), false), Some(socket));
            assert_eq!(first_refused_helper_call(arch, &profile(&[socket]), true), None);
        }
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn hxfix_refused_helper_calls_match_native_uapi() {
        let arch = Architecture::native().unwrap();
        let network = [libc::SYS_socket, libc::SYS_socketpair, libc::SYS_connect, libc::SYS_bind, libc::SYS_listen,
            libc::SYS_accept, libc::SYS_accept4];
        assert_eq!(offline_network_calls(arch), network.map(|nr| nr as u32));
        #[cfg(target_arch = "x86_64")]
        let metadata = vec![libc::SYS_truncate, libc::SYS_chmod, libc::SYS_fchmodat, 452, libc::SYS_utime, libc::SYS_utimes,
            libc::SYS_utimensat, libc::SYS_futimesat, libc::SYS_setxattr, libc::SYS_lsetxattr, libc::SYS_removexattr,
            libc::SYS_lremovexattr, 463, 466, libc::SYS_chown, libc::SYS_lchown, libc::SYS_fchownat, libc::SYS_fchown];
        #[cfg(not(target_arch = "x86_64"))]
        let metadata = vec![libc::SYS_truncate, libc::SYS_fchmodat, 452, libc::SYS_utimensat, libc::SYS_setxattr,
            libc::SYS_lsetxattr, libc::SYS_removexattr, libc::SYS_lremovexattr, 463, 466, libc::SYS_fchownat,
            libc::SYS_fchown];
        assert_eq!(landlock_uncovered_calls(arch), metadata.into_iter().map(|nr| nr as u32).collect::<Vec<_>>().as_slice());
        let ipc = [libc::SYS_shmget, libc::SYS_shmat, libc::SYS_shmctl, libc::SYS_shmdt, libc::SYS_semget, libc::SYS_semop,
            libc::SYS_semctl, libc::SYS_semtimedop, libc::SYS_msgget, libc::SYS_msgsnd, libc::SYS_msgrcv, libc::SYS_msgctl,
            libc::SYS_mq_open, libc::SYS_mq_unlink, libc::SYS_mq_timedsend, libc::SYS_mq_timedreceive, libc::SYS_mq_notify,
            libc::SYS_mq_getsetattr];
        assert_eq!(ipc_namespace_calls(arch), ipc.map(|nr| nr as u32));
    }
}
