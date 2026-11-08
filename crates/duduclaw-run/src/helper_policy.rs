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
}
pub const PR_SET_PDEATHSIG: u32 = 1;
pub const PR_SET_DUMPABLE: u32 = 4;
pub const PR_SET_NO_NEW_PRIVS: u32 = 38;
/// Set options that only restrict the caller, plus read-only `PR_GET_*` queries
/// (PDEATHSIG, DUMPABLE, KEEPCAPS, NAME, SECCOMP, SECUREBITS, TIMERSLACK, NO_NEW_PRIVS, THP_DISABLE).
pub const PRCTL_SELF_ONLY: [u32; 12] = [PR_SET_PDEATHSIG, 2, 3, PR_SET_DUMPABLE, 7, 16, 21, 27, 30,
    PR_SET_NO_NEW_PRIVS, 39, 42];
pub fn prlimit64(arch: Architecture) -> u32 { match arch { Architecture::X86_64 => 302, Architecture::Aarch64 => 261 } }
pub fn prctl(arch: Architecture) -> u32 { match arch { Architecture::X86_64 => 157, Architecture::Aarch64 => 167 } }

/// Check 2 [F3]: the flat helper allowlist is a subset of the parent's, and every
/// self-only exception is well formed and not also granted unconstrained.
pub fn seccomp_within_parent(arch: Architecture, parent: &RuntimeProfile, helper: &RuntimeProfile,
    self_only: &[SelfOnlyCall]) -> bool {
    if !helper.allowed_syscalls.iter().all(|nr| parent.allowed_syscalls.contains(nr)) || self_only.len() > 2 { return false; }
    let mut seen = Vec::new();
    for call in self_only {
        let nr = match call {
            SelfOnlyCall::Prlimit64SelfOnly => prlimit64(arch),
            SelfOnlyCall::Prctl { options } => {
                let mut sorted = options.clone();
                sorted.sort_unstable();
                if options.is_empty() || sorted.windows(2).any(|pair| pair[0] == pair[1])
                    || !options.iter().all(|option| PRCTL_SELF_ONLY.contains(option)) { return false; }
                prctl(arch)
            }
        };
        if seen.contains(&nr) || helper.allowed_syscalls.contains(&nr) { return false; }
        seen.push(nr);
    }
    true
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
}
