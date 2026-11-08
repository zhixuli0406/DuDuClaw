//! Signed in-run helper allowlist and its publication checks (HX D8 revision
//! §2.3 checks 1–7). A `LauncherRecord` lives inside the parent profile's signed
//! `NativeRecord`; it is never derived from IPC and only ever narrows the parent.
use duduclaw_isolation::{egress::ProfileId, execution::{DiskQuota, NetworkPolicy, RuntimeKind}, ids::Sha256Digest};
pub use duduclaw_run::helper_policy::{HelperFs, HelperSlot, SelfOnlyCall, HELPER_CGROUP_FDS, HELPER_TIMEOUT_MAX_MS,
    MAX_HELPER_BINDINGS, RECORDING_MIN_GRACE_MS};
use duduclaw_run::{helper_policy::{helper_fs_within_runtime, seccomp_within_parent}, seccomp::{self, Architecture, RuntimeProfile}};
use serde::{Deserialize, Serialize};
use crate::policy::{Executable, PublishedRuntime};

/// Same value as the gateway's `isolation_execution_program::RESULT_GRACE_MS`:
/// time for helper kill/reap and typed result delivery before the run retires.
pub const RESULT_GRACE_MS: u64 = 5_000;
/// Check 5: worst-case launcher descriptors (control socket, helper mounts,
/// executables/scripts, cgroup files). A run whose total still exceeds
/// `MAX_BOOTSTRAP_FDS` (many attachments) is refused by `TrustedRun::validate`.
pub const HELPER_FD_BUDGET: usize = 40;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LauncherRecord {
    /// Signed headroom kept for the runtime itself (check 3).
    pub reserve: RuntimeReserve,
    pub helpers: Vec<HelperBinding>,
    /// `/helper-tmp` tmpfs size; spawnd also bounds it by the parent's tmpfs budget.
    pub helper_tmpfs_bytes: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeReserve { pub memory_bytes: u64, pub tasks: u32, pub cpu_quota_percent: u32 }

/// One allowlisted helper. Its executable, argv, network and output budget come
/// from the helper's own signed `RuntimeRecord` (`profile`); everything here can
/// only narrow the parent run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelperBinding {
    pub profile: ProfileId,
    pub slot: HelperSlot,
    /// Flat F_H allowlist, digest bound like a runtime profile (`seccomp::runtime`).
    pub seccomp: RuntimeProfile,
    pub seccomp_digest: Sha256Digest,
    /// Argument-constrained self-only exceptions (check 2 [F3]).
    #[serde(default)]
    pub self_only: Vec<SelfOnlyCall>,
    pub memory_bytes: u64,
    pub tasks: u32,
    pub cpu_quota_percent: u32,
    /// `RLIMIT_NOFILE` and `RLIMIT_FSIZE`, hard = soft (§2.4 rlimits).
    pub open_files: u32,
    pub file_size_bytes: u64,
    pub timeout_ms: u64,
    pub filesystem: Vec<HelperFs>,
    /// Must equal "the helper's signed network policy is Egress" (check 4).
    pub egress: bool,
}

/// Readiness-reason classes (D8 revision §5); `as_reason` is the wire spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelperPolicyError {
    /// Check 1: missing, ambiguous or non-helper runtime record.
    RuntimeMissing,
    /// Checks 2, 3 and 6: the binding is not a narrowing of the parent.
    NotNarrowing,
    /// Check 4: an egress helper without an Egress parent of the same profile.
    ParentOffline,
    /// Check 5: binding count or descriptor budget.
    FdBudget,
    /// Check 7: recording without the minimum flush grace.
    ShortGrace,
    /// Malformed record (duplicates, slot/egress mismatch, zero limits).
    Invalid,
}
impl HelperPolicyError {
    pub fn as_reason(self) -> &'static str {
        match self {
            Self::RuntimeMissing => "helper_runtime_missing",
            Self::NotNarrowing | Self::ShortGrace => "helper_not_narrowing",
            Self::ParentOffline => "helper_parent_offline",
            Self::FdBudget | Self::Invalid => "helper_launcher_unavailable",
        }
    }
}

const RECORDING_RUNTIME: &str = "duduclaw-browser-record-v1";
/// Structural half of the gateway's `valid_helper` (`isolation_execution_program.rs`).
/// The gateway independently re-checks each helper's protocol argv at its load.
fn helper_id(runtime: &RuntimeKind) -> Option<&str> {
    let RuntimeKind::Registered(name) = runtime else { return None; };
    let name = name.as_str();
    let office = name.strip_prefix("duduclaw-office-").and_then(|v| v.strip_suffix("-v1"))
        .and_then(|v| v.split_once('-')).is_some_and(|(skill, script)| !skill.is_empty() && !script.is_empty()
            && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_'));
    (matches!(name, "duduclaw-program-v1" | "duduclaw-code-map-v1" | RECORDING_RUNTIME) || office).then_some(name)
}
pub fn executable_fds(runtime: &PublishedRuntime) -> usize {
    match runtime.executable { Executable::Elf { .. } => 1, Executable::Script { .. } => 2 }
}
/// Every helper runtime is published in the same signed generation registry, so
/// the launcher needs one read-only helper registry mount (check 5 dedup).
pub const HELPER_MOUNTS: usize = 1;
pub fn launcher_fd_cost(helpers: &[&PublishedRuntime]) -> usize {
    1 + HELPER_MOUNTS + helpers.iter().map(|r| executable_fds(r)).sum::<usize>() + HELPER_CGROUP_FDS
}

/// Checks 1–7. `parent_seccomp` is the parent's signed runtime filter from the
/// same `NativeRecord`; `runtimes` is the whole verified publication.
pub fn validate_launcher(arch: Architecture, parent: &PublishedRuntime, parent_seccomp: &RuntimeProfile,
    launcher: &LauncherRecord, runtimes: &[PublishedRuntime]) -> Result<(), HelperPolicyError> {
    use HelperPolicyError::*;
    if helper_id(&parent.runtime).is_some() { return Err(Invalid); }
    // Check 5: binding count first, so an oversized table never reaches later work.
    if launcher.helpers.is_empty() || launcher.helpers.len() > MAX_HELPER_BINDINGS { return Err(FdBudget); }
    let reserve = launcher.reserve;
    let limits = &parent.resources;
    if reserve.memory_bytes == 0 || reserve.tasks == 0 || reserve.cpu_quota_percent == 0
        || launcher.helper_tmpfs_bytes == 0 { return Err(Invalid); }
    let memory = limits.memory_bytes.checked_sub(reserve.memory_bytes).ok_or(NotNarrowing)?;
    let tasks = limits.tasks.checked_sub(reserve.tasks).ok_or(NotNarrowing)?;
    let cpu = limits.cpu_quota_percent.checked_sub(reserve.cpu_quota_percent).ok_or(NotNarrowing)?;
    let disk = match limits.disk { DiskQuota::LoopFilesystem { bytes, .. } | DiskQuota::ProjectQuota { bytes, .. } => bytes };
    let timeouts = &parent.timeouts;
    let run_budget = timeouts.runtime_ms.checked_sub(timeouts.termination_grace_ms)
        .and_then(|value| value.checked_sub(RESULT_GRACE_MS)).ok_or(NotNarrowing)?;
    let mut profiles = Vec::new();
    let mut kinds = Vec::new();
    let mut resolved = Vec::new();
    for binding in &launcher.helpers {
        // Check 1: exactly one signed helper record; never the parent itself.
        let mut matching = runtimes.iter().filter(|r| r.profile == binding.profile);
        let helper = matching.next().ok_or(RuntimeMissing)?;
        if matching.next().is_some() || helper.profile == parent.profile { return Err(RuntimeMissing); }
        let id = helper_id(&helper.runtime).ok_or(RuntimeMissing)?;
        if helper.mcp || helper.pty_allowed { return Err(RuntimeMissing); }
        if profiles.contains(&binding.profile) || kinds.contains(&&helper.runtime) { return Err(Invalid); }
        profiles.push(binding.profile); kinds.push(&helper.runtime);
        let recording = id == RECORDING_RUNTIME;
        if recording != (binding.slot == HelperSlot::Recording) { return Err(Invalid); }
        // Check 2 [F3].
        if seccomp::runtime(arch, &binding.seccomp, binding.seccomp_digest).is_err()
            || !seccomp_within_parent(arch, parent_seccomp, &binding.seccomp, &binding.self_only) { return Err(NotNarrowing); }
        // Check 3.
        if binding.memory_bytes == 0 || binding.tasks == 0 || binding.cpu_quota_percent == 0 || binding.open_files == 0
            || binding.file_size_bytes == 0 || binding.timeout_ms == 0 { return Err(Invalid); }
        if binding.memory_bytes > memory || binding.tasks > tasks || binding.cpu_quota_percent > cpu
            || binding.open_files > limits.open_files || binding.file_size_bytes > disk
            || binding.timeout_ms > HELPER_TIMEOUT_MAX_MS || binding.timeout_ms > run_budget
            || binding.timeout_ms > helper.timeouts.runtime_ms { return Err(NotNarrowing); }
        // Check 4: network authority never exceeds the parent's.
        match (&helper.network, &parent.network) {
            (NetworkPolicy::Offline, _) if !binding.egress => {},
            (NetworkPolicy::Egress(helper), NetworkPolicy::Egress(parent)) if binding.egress && helper == parent => {},
            (NetworkPolicy::Egress(_), _) if binding.egress => return Err(ParentOffline),
            _ => return Err(Invalid),
        }
        if recording && !binding.egress { return Err(Invalid); }
        // Check 6 [F15]: fixed grants inside the runtime list; recordings only for the recording parent.
        let mut grants = binding.filesystem.clone();
        grants.sort_unstable(); grants.dedup();
        if grants.is_empty() || grants.len() != binding.filesystem.len()
            || !grants.iter().all(|grant| helper_fs_within_runtime(*grant)) { return Err(NotNarrowing); }
        if grants.contains(&HelperFs::Work) == recording || grants.contains(&HelperFs::WorkRecordings) != recording {
            return Err(NotNarrowing);
        }
        // Check 7 [F16].
        if recording && timeouts.termination_grace_ms < RECORDING_MIN_GRACE_MS { return Err(ShortGrace); }
        resolved.push(helper);
    }
    // Check 5: descriptor budget with helper mounts deduplicated.
    if launcher_fd_cost(&resolved) > HELPER_FD_BUDGET { return Err(FdBudget); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_isolation::{bounded::Text, execution::{ResourceLimits, RunTimeouts}, ipc::QueueClass};
    const ARCH: Architecture = Architecture::X86_64;
    fn runtime(kind: RuntimeKind, byte: u8, network: NetworkPolicy) -> PublishedRuntime {
        PublishedRuntime { runtime: kind, profile: ProfileId(Sha256Digest([byte; 32])),
            executable: Executable::Elf { entry: Text::new("entry".into()).unwrap(), digest: Sha256Digest([1; 32]) },
            pinned_version: Text::new("1.0.0".into()).unwrap(), fixed_args: vec![],
            resources: ResourceLimits { memory_bytes: 4 << 30, cpu_quota_percent: 200, tasks: 512, open_files: 4096, ptys: 1,
                disk: DiskQuota::LoopFilesystem { bytes: 8 << 30, inodes: 1 << 20 }, io_read_bytes_per_sec: 1,
                io_write_bytes_per_sec: 1, io_read_ops_per_sec: 1, io_write_ops_per_sec: 1, output_buffer_bytes: 4096,
                output_total_bytes: 1 << 20, memfd_total_bytes: 1 << 20 },
            timeouts: RunTimeouts { queue_ms: 1000, startup_ms: 1000, runtime_ms: 3_600_000, pty_idle_ms: None,
                termination_grace_ms: 15_000 },
            network, mcp: false, pty_allowed: false, queue_class: QueueClass::Background }
    }
    fn registered(name: &str) -> RuntimeKind { RuntimeKind::Registered(Text::new(name.into()).unwrap()) }
    fn egress() -> NetworkPolicy { NetworkPolicy::Egress(ProfileId(Sha256Digest([7; 32]))) }
    fn parent_profile() -> RuntimeProfile { RuntimeProfile { allowed_syscalls: vec![0, 1, 3, 41, 42, 59, 231, 322] } }
    fn profile(calls: &[u32]) -> (RuntimeProfile, Sha256Digest) {
        let profile = RuntimeProfile { allowed_syscalls: calls.to_vec() };
        let digest = profile.digest(ARCH).unwrap();
        (profile, digest)
    }
    fn binding(byte: u8, slot: HelperSlot) -> HelperBinding {
        let (seccomp, seccomp_digest) = profile(&[0, 1, 3, 231, 322]);
        let recording = slot == HelperSlot::Recording;
        HelperBinding { profile: ProfileId(Sha256Digest([byte; 32])), slot, seccomp, seccomp_digest,
            self_only: vec![], memory_bytes: 1 << 30, tasks: 64, cpu_quota_percent: 100, open_files: 1024,
            file_size_bytes: 1 << 30, timeout_ms: 300_000,
            filesystem: vec![HelperFs::SystemRo, if recording { HelperFs::WorkRecordings } else { HelperFs::Work }],
            egress: recording }
    }
    fn launcher(helpers: Vec<HelperBinding>) -> LauncherRecord {
        LauncherRecord { reserve: RuntimeReserve { memory_bytes: 1 << 30, tasks: 64, cpu_quota_percent: 50 }, helpers,
            helper_tmpfs_bytes: 64 << 20 }
    }
    struct Fixture { parent: PublishedRuntime, runtimes: Vec<PublishedRuntime> }
    fn fixture(parent_network: NetworkPolicy) -> Fixture {
        let parent = runtime(RuntimeKind::Claude, 1, parent_network);
        let program = runtime(registered("duduclaw-program-v1"), 2, NetworkPolicy::Offline);
        let recording = runtime(registered("duduclaw-browser-record-v1"), 3, egress());
        Fixture { runtimes: vec![parent.clone(), program, recording], parent }
    }
    fn check(f: &Fixture, launcher: &LauncherRecord) -> Result<(), HelperPolicyError> {
        validate_launcher(ARCH, &f.parent, &parent_profile(), launcher, &f.runtimes)
    }

    #[test]
    fn narrowing_offline_and_recording_bindings_are_accepted() {
        let f = fixture(egress());
        check(&f, &launcher(vec![binding(2, HelperSlot::Foreground), binding(3, HelperSlot::Recording)])).unwrap();
    }
    #[test]
    fn publication_rejects_helper_syscall_outside_parent() {
        let f = fixture(NetworkPolicy::Offline);
        let mut helper = binding(2, HelperSlot::Foreground);
        // 2 = open, absent from the parent profile.
        (helper.seccomp, helper.seccomp_digest) = profile(&[0, 1, 2, 3, 231, 322]);
        assert_eq!(check(&f, &launcher(vec![helper.clone()])), Err(HelperPolicyError::NotNarrowing));
        // A digest that does not bind the listed calls is refused as well.
        (helper.seccomp, _) = profile(&[0, 231, 322]);
        assert_eq!(check(&f, &launcher(vec![helper])), Err(HelperPolicyError::NotNarrowing));
    }
    #[test]
    fn publication_rejects_helper_wider_than_parent() {
        let f = fixture(NetworkPolicy::Offline);
        let base = binding(2, HelperSlot::Foreground);
        let parent = &f.parent.resources;
        let widen: [fn(&mut HelperBinding, &ResourceLimits); 6] = [
            |b, p| b.memory_bytes = p.memory_bytes, // ignores the runtime reserve
            |b, p| b.tasks = p.tasks,
            |b, p| b.cpu_quota_percent = p.cpu_quota_percent,
            |b, p| b.open_files = p.open_files + 1,
            |b, _| b.timeout_ms = HELPER_TIMEOUT_MAX_MS + 1,
            |b, _| b.file_size_bytes = 9 << 30,
        ];
        for apply in widen {
            let mut helper = base.clone(); apply(&mut helper, parent);
            assert_eq!(check(&f, &launcher(vec![helper])), Err(HelperPolicyError::NotNarrowing));
        }
        // Timeout must also leave the parent's grace and result window.
        let mut short = fixture(NetworkPolicy::Offline);
        short.parent.timeouts.runtime_ms = 60_000; short.parent.timeouts.termination_grace_ms = 10_000;
        let mut helper = base.clone(); helper.timeout_ms = 45_001;
        assert_eq!(check(&short, &launcher(vec![helper.clone()])), Err(HelperPolicyError::NotNarrowing));
        helper.timeout_ms = 45_000;
        check(&short, &launcher(vec![helper])).unwrap();
        // A reserve larger than the parent cannot underflow into a wide budget.
        let mut wide = launcher(vec![base]); wide.reserve.memory_bytes = parent.memory_bytes + 1;
        assert_eq!(check(&f, &wide), Err(HelperPolicyError::NotNarrowing));
    }
    #[test]
    fn publication_allows_prlimit64_pid0_only() {
        let f = fixture(NetworkPolicy::Offline);
        let mut helper = binding(2, HelperSlot::Foreground);
        helper.self_only = vec![SelfOnlyCall::Prlimit64SelfOnly,
            SelfOnlyCall::Prctl { options: vec![duduclaw_run::helper_policy::PR_SET_PDEATHSIG,
                duduclaw_run::helper_policy::PR_SET_DUMPABLE] }];
        check(&f, &launcher(vec![helper.clone()])).unwrap();
        // Unconstrained prlimit64 (302) is not in the parent and is refused.
        let mut flat = helper.clone(); flat.self_only.clear();
        (flat.seccomp, flat.seccomp_digest) = profile(&[0, 1, 3, 231, 302, 322]);
        assert_eq!(check(&f, &launcher(vec![flat])), Err(HelperPolicyError::NotNarrowing));
        // prctl options outside the self-only list (PR_SET_SECCOMP = 22, PR_SET_MM = 35) are refused.
        for option in [22, 35] {
            let mut wide = helper.clone(); wide.self_only = vec![SelfOnlyCall::Prctl { options: vec![option] }];
            assert_eq!(check(&f, &launcher(vec![wide])), Err(HelperPolicyError::NotNarrowing));
        }
        assert!(serde_json::from_value::<SelfOnlyCall>(serde_json::json!({"kind":"prlimit64_any_pid"})).is_err());
    }
    #[test]
    fn publication_rejects_helperfs_outside_runtime_list() {
        let f = fixture(egress());
        // Recording grants are only for the recording parent, /work never for it.
        let mut helper = binding(2, HelperSlot::Foreground); helper.filesystem = vec![HelperFs::WorkRecordings];
        assert_eq!(check(&f, &launcher(vec![helper])), Err(HelperPolicyError::NotNarrowing));
        let mut helper = binding(3, HelperSlot::Recording); helper.filesystem.push(HelperFs::Work);
        assert_eq!(check(&f, &launcher(vec![helper])), Err(HelperPolicyError::NotNarrowing));
        let mut helper = binding(2, HelperSlot::Foreground); helper.filesystem = vec![];
        assert_eq!(check(&f, &launcher(vec![helper])), Err(HelperPolicyError::NotNarrowing));
        let mut helper = binding(2, HelperSlot::Foreground); helper.filesystem.push(HelperFs::Work);
        assert_eq!(check(&f, &launcher(vec![helper])), Err(HelperPolicyError::NotNarrowing));
        // The closed enum cannot name a forbidden path, and each variant's paths
        // are inside the runtime list minus the forbidden set.
        let mut value = serde_json::to_value(binding(2, HelperSlot::Foreground)).unwrap();
        value["filesystem"] = serde_json::json!(["system_ro", "home_employee"]);
        assert!(serde_json::from_value::<HelperBinding>(value).is_err());
        for grant in HelperFs::ALL {
            for (path, _) in grant.paths() {
                assert!(!duduclaw_run::helper_policy::HELPER_FORBIDDEN.iter().any(|f| path.starts_with(f)), "{path}");
            }
        }
        assert!(!duduclaw_run::helper_policy::path_within_runtime("/home/employee",
            duduclaw_run::landlock::PathClass::ReadOnly));
    }
    #[test]
    fn publication_rejects_short_grace_for_recording() {
        let mut f = fixture(egress());
        f.parent.timeouts.termination_grace_ms = RECORDING_MIN_GRACE_MS - 1;
        assert_eq!(check(&f, &launcher(vec![binding(3, HelperSlot::Recording)])), Err(HelperPolicyError::ShortGrace));
        // Foreground helpers do not need the recording flush window.
        check(&f, &launcher(vec![binding(2, HelperSlot::Foreground)])).unwrap();
        f.parent.timeouts.termination_grace_ms = RECORDING_MIN_GRACE_MS;
        check(&f, &launcher(vec![binding(3, HelperSlot::Recording)])).unwrap();
    }
    #[test]
    fn egress_helper_requires_same_egress_parent() {
        let recording = || launcher(vec![binding(3, HelperSlot::Recording)]);
        assert_eq!(check(&fixture(NetworkPolicy::Offline), &recording()), Err(HelperPolicyError::ParentOffline));
        let other = NetworkPolicy::Egress(ProfileId(Sha256Digest([8; 32])));
        assert_eq!(check(&fixture(other), &recording()), Err(HelperPolicyError::ParentOffline));
        check(&fixture(egress()), &recording()).unwrap();
        // The signed egress flag must match the helper record exactly.
        let mut helper = binding(2, HelperSlot::Foreground); helper.egress = true;
        assert_eq!(check(&fixture(egress()), &launcher(vec![helper])), Err(HelperPolicyError::Invalid));
        // Offline helpers are always allowed, also under an Egress parent.
        check(&fixture(egress()), &launcher(vec![binding(2, HelperSlot::Foreground)])).unwrap();
    }
    #[test]
    fn helper_table_fd_budget_enforced() {
        let mut f = fixture(NetworkPolicy::Offline);
        let mut helpers = Vec::new();
        for index in 0..17u8 {
            let byte = 100 + index;
            let mut record = runtime(registered(&format!("duduclaw-office-skill{index}-run-v1")), byte, NetworkPolicy::Offline);
            record.executable = Executable::Script { interpreter: Text::new("node".into()).unwrap(),
                interpreter_digest: Sha256Digest([1; 32]), script: Text::new("main.js".into()).unwrap(),
                script_digest: Sha256Digest([2; 32]) };
            f.runtimes.push(record);
            let mut helper = binding(byte, HelperSlot::Foreground); helper.memory_bytes = 1 << 20; helper.tasks = 1;
            helpers.push(helper);
        }
        assert_eq!(check(&f, &launcher(helpers.clone())), Err(HelperPolicyError::FdBudget));
        // 16 script helpers: 1 control + 1 mount + 32 exec + 4 cgroup = 38 <= 40.
        helpers.truncate(16);
        check(&f, &launcher(helpers.clone())).unwrap();
        let refs: Vec<_> = f.runtimes[3..19].iter().collect();
        assert_eq!(launcher_fd_cost(&refs), 38);
        // Fixed run descriptors without attachments (5 mounts, ELF + script, prompt,
        // stdin, MCP, forwarder pair) plus the full table still fit the bootstrap.
        assert!(launcher_fd_cost(&refs) + 12 <= crate::bootstrap::MAX_BOOTSTRAP_FDS);
        // Duplicate bindings of one helper are refused rather than double-counted.
        helpers.truncate(1); helpers.push(helpers[0].clone());
        assert_eq!(check(&f, &launcher(helpers)), Err(HelperPolicyError::Invalid));
    }
    #[test]
    fn helper_records_must_be_signed_non_mcp_helpers() {
        let mut f = fixture(NetworkPolicy::Offline);
        assert_eq!(check(&f, &launcher(vec![binding(9, HelperSlot::Foreground)])), Err(HelperPolicyError::RuntimeMissing));
        f.runtimes[1].mcp = true;
        assert_eq!(check(&f, &launcher(vec![binding(2, HelperSlot::Foreground)])), Err(HelperPolicyError::RuntimeMissing));
        f.runtimes[1].mcp = false; f.runtimes[1].runtime = RuntimeKind::Codex;
        assert_eq!(check(&f, &launcher(vec![binding(2, HelperSlot::Foreground)])), Err(HelperPolicyError::RuntimeMissing));
        // A parent cannot bind itself.
        assert_eq!(check(&f, &launcher(vec![binding(1, HelperSlot::Foreground)])), Err(HelperPolicyError::RuntimeMissing));
    }
}
