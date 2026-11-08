//! Root-signed in-run helper vocabulary (HX D8 revision §2.3, §2.4). Pure and
//! host-testable: spawnd's publication checks and the trampoline's launcher plan
//! share these closed enums, so neither side can name a path or syscall the other
//! did not review. Nothing here is deserialized from gateway IPC.
use std::ffi::CStr;
use crate::{landlock::{PathClass, Policy, MAX_KNOWN_ABI}, seccomp::{Architecture, RuntimeProfile}};

/// Check 5: at most this many helper bindings per parent profile.
pub const MAX_HELPER_BINDINGS: usize = 16;
/// Check 3: absolute helper timeout ceiling.
pub const HELPER_TIMEOUT_MAX_MS: u64 = 300_000;
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

/// `hx/fg` or `hx/rec` (Q4 = A: one of each per run).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HelperSlot { Foreground, Recording }

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
