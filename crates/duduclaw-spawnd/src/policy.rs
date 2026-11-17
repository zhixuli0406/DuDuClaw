//! Root-side runtime selection. The gateway's family request never selects a profile.
use std::collections::{BTreeMap, BTreeSet};
use duduclaw_isolation::{bounded::Text, egress::ProfileId, execution::{ResourceLimits, RunTimeouts, RuntimeKind, NetworkPolicy},
    ids::{EmployeeId, Sha256Digest}, ipc::{ErrorCode, IoDelivery, QueueClass}};
use crate::{Result, admission::LaunchPolicy};

/// Names relative to the verified runtime mount. Never PATH-resolved.
#[derive(Clone, PartialEq, Eq)]
pub enum Executable {
    Elf { entry: Text<128>, digest: Sha256Digest },
    Script { interpreter: Text<128>, interpreter_digest: Sha256Digest,
        script: Text<128>, script_digest: Sha256Digest },
}

/// Construct only from F's verified, root-published runtime manifest. This type
/// deliberately has no Deserialize implementation and is not an IPC payload.
#[derive(Clone, PartialEq, Eq)]
pub struct PublishedRuntime {
    pub runtime: RuntimeKind,
    pub profile: ProfileId,
    pub executable: Executable,
    pub pinned_version: Text<64>,
    pub fixed_args: Vec<Text<4096>>,
    pub resources: ResourceLimits,
    pub timeouts: RunTimeouts,
    pub network: NetworkPolicy,
    pub mcp: bool,
    pub pty_allowed: bool,
    pub queue_class: QueueClass,
    /// Policy v3 bounded step range (R6). `None`: the record keeps whatever
    /// its signed `fixed_args` say (v1/v2 Claude: the fixed `--max-turns 50`).
    pub turn_limit: Option<TurnLimit>,
}

/// A root entitlement points to immutable profiles, not user-selected argv/env.
pub struct EmployeeEntitlement {
    pub employee: EmployeeId,
    pub profiles: Vec<ProfileId>,
    /// R6: the employee's root-owned step ceiling. It can only narrow the
    /// signed range; `None` means the signed maximum.
    pub max_turns: Option<u16>,
}

/// Absolute bound of any signed step range (plan §4.6 R6 item 1).
pub const TURN_LIMIT_CEILING: u16 = 1000;
/// The value v1/v2 signed Claude arguments fix (`--max-turns 50`).
pub const LEGACY_FIXED_MAX_TURNS: u16 = 50;
/// Working default for a v3 release template: min 1, default 50, max 200.
/// PENDING OWNER DECISION Q12 (plan「待 owner 決定」); the signed policy, not
/// this constant, is authoritative at run time.
pub const WORKING_DEFAULT_TURN_LIMIT: TurnLimit = TurnLimit { min: 1, max: 200, default: 50 };

/// Signed `turn_limit` of a policy v3 runtime record:
/// `1 ≤ min ≤ default ≤ max ≤ 1000`. Any other value invalidates the whole policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnLimit { pub min: u16, pub max: u16, pub default: u16 }
impl TurnLimit {
    pub fn validate(&self) -> Result<()> {
        if self.min < 1 || self.min > self.default || self.default > self.max || self.max > TURN_LIMIT_CEILING {
            return Err(ErrorCode::RuntimeDenied);
        }
        Ok(())
    }
    pub fn contains(&self, value: u16) -> bool { (self.min..=self.max).contains(&value) }
}

/// The single `turn_limit` rule (plan §4.6 R6 item 3 plus its fail-closed
/// paragraph). spawnd applies it at StartRun; the gateway applies the very
/// same function to the same root publication before it writes the step
/// budget into the system prompt, so the value it shows is the value enforced.
///
/// | signed range | request          | result                                              |
/// |--------------|------------------|-----------------------------------------------------|
/// | none         | none             | `Ok(None)`: signed fixed args apply (v1/v2: 50)     |
/// | none         | some             | refused                                             |
/// | some         | outside min..=max| refused (never clamped)                             |
/// | some         | none / in range  | `e = min(req.unwrap_or(default), cap.unwrap_or(max), max)`; `e < min` refused |
/// | some         | some(r), e ≠ r   | refused (the entitlement would silently lower it)   |
pub fn effective_max_turns(limit: Option<TurnLimit>, entitlement: Option<u16>, requested: Option<u16>) -> Result<Option<u16>> {
    let Some(limit) = limit else {
        return if requested.is_none() { Ok(None) } else { Err(ErrorCode::RuntimeDenied) };
    };
    limit.validate()?;
    if requested.is_some_and(|value| !limit.contains(value)) { return Err(ErrorCode::RuntimeDenied); }
    let effective = requested.unwrap_or(limit.default).min(entitlement.unwrap_or(limit.max)).min(limit.max);
    if effective < limit.min || requested.is_some_and(|value| value != effective) { return Err(ErrorCode::RuntimeDenied); }
    Ok(Some(effective))
}

/// Gateway side: narrow the caller's own wish (the wave-1 resolution; at most
/// 50 for a utility call) into the signed range and the employee's root
/// ceiling. It can only lower a value; the result still goes through
/// [`effective_max_turns`], which refuses anything below `min`.
pub fn narrowed_request(limit: TurnLimit, entitlement: Option<u16>, desired: u64) -> u16 {
    u16::try_from(desired).unwrap_or(u16::MAX).min(entitlement.unwrap_or(limit.max)).min(limit.max)
}

/// A ranged record is a Claude MCP runtime whose signed arguments carry no
/// `--max-turns` and end with the fixed MCP suffix that spawnd inserts before.
pub fn validate_turn_limit(runtime: &PublishedRuntime) -> Result<()> {
    let Some(limit) = runtime.turn_limit else { return Ok(()); };
    limit.validate()?;
    let tail = duduclaw_mcp_boundary::config::claude_mcp_arguments();
    let arguments: Vec<&str> = runtime.fixed_args.iter().map(|arg| arg.as_str()).collect();
    let tail: Vec<&str> = tail.iter().map(String::as_str).collect();
    if runtime.runtime != RuntimeKind::Claude || !runtime.mcp || !arguments.ends_with(&tail)
        || arguments.iter().any(|arg| *arg == "--max-turns" || arg.starts_with("--max-turns=")) {
        return Err(ErrorCode::RuntimeDenied);
    }
    Ok(())
}

/// One row of a run's helper table. `index` is the launcher `Start.helper` byte.
pub struct HelperEntry<'a> { pub index: u8, pub binding: &'a crate::helper_policy::HelperBinding, pub runtime: &'a PublishedRuntime }

#[derive(Clone)]
pub struct RootPolicy { runtimes: Vec<PublishedRuntime>, employees: BTreeMap<EmployeeId, Vec<ProfileId>>,
    launchers: BTreeMap<[u8; 32], crate::helper_policy::LauncherRecord>, turn_caps: BTreeMap<EmployeeId, u16> }
impl RootPolicy {
    /// R6: the employee's root-owned step ceiling, if any.
    pub fn turn_cap(&self, employee: EmployeeId) -> Option<u16> { self.turn_caps.get(&employee).copied() }
    /// R6: the step limit spawnd enforces for this run of `employee` on
    /// `runtime` (see [`effective_max_turns`]). `Ok(None)`: no range is signed.
    pub fn max_turns(&self, employee: EmployeeId, runtime: &PublishedRuntime, requested: Option<u16>) -> Result<Option<u16>> {
        effective_max_turns(runtime.turn_limit, self.turn_cap(employee), requested)
    }
    /// Attach the parent profiles' signed helper allowlists after checks 1–7
    /// (HX D8 revision §2.3). Any failing launcher fails the whole publication.
    pub fn with_launchers(mut self, arch: duduclaw_run::seccomp::Architecture,
        launchers: Vec<(ProfileId, duduclaw_run::seccomp::RuntimeProfile, crate::helper_policy::LauncherRecord)>) -> Result<Self> {
        if launchers.len() > 128 { return Err(ErrorCode::RuntimeDenied); }
        for (profile, parent_seccomp, launcher) in launchers {
            let parent = self.runtimes.iter().find(|runtime| runtime.profile == profile).ok_or(ErrorCode::RuntimeDenied)?;
            crate::helper_policy::validate_launcher(arch, parent, &parent_seccomp, &launcher, &self.runtimes)
                .map_err(|_| ErrorCode::RuntimeDenied)?;
            if self.launchers.insert((profile.0).0, launcher).is_some() { return Err(ErrorCode::RuntimeDenied); }
        }
        Ok(self)
    }
    /// The helper table of a run of `parent` for `employee`, always derived from
    /// the run entry's employee, never from a request. Check 1's entitlement half
    /// is applied here because entitlements are host data, not signed policy: a
    /// helper the employee is not entitled to is absent (refused), not substituted.
    /// Indices are the positions in the signed list, so they are stable per profile.
    pub fn helper_table(&self, employee: EmployeeId, parent: &PublishedRuntime) -> Vec<HelperEntry<'_>> {
        let Some(profiles) = self.employees.get(&employee) else { return vec![]; };
        let Some(launcher) = self.launchers.get(&(parent.profile.0).0) else { return vec![]; };
        if !profiles.contains(&parent.profile) { return vec![]; }
        launcher.helpers.iter().enumerate().filter_map(|(index, binding)| {
            if !profiles.contains(&binding.profile) { return None; }
            let runtime = self.runtimes.iter().find(|runtime| runtime.profile == binding.profile)?;
            Some(HelperEntry { index: u8::try_from(index).ok()?, binding, runtime })
        }).collect()
    }
    /// HX4: whether any parent profile has a signed launcher (the "launcher
    /// plan active" half of the `helper_calls` feature bit).
    pub fn launchers_active(&self) -> bool { self.launchers.values().any(|launcher| !launcher.helpers.is_empty()) }
    pub fn helper_tmpfs_bytes(&self, parent: &PublishedRuntime) -> Option<u64> {
        self.launchers.get(&(parent.profile.0).0).map(|launcher| launcher.helper_tmpfs_bytes)
    }
    /// Per call (§2.3): re-derive the helper for the run's own employee and map it
    /// to its index in that run's table; absent means refused. An index is a
    /// position in the *current* signed list, so after a reload it may name a
    /// different binding: `HelperEngine::start` also requires the entry's kind,
    /// profile and `helper_policy::binding_digest` to equal the run's table
    /// (security fix round, finding 1).
    pub fn derive_helper(&self, employee: EmployeeId, parent: &PublishedRuntime, helper: &RuntimeKind) -> Result<HelperEntry<'_>> {
        let mut matching = self.helper_table(employee, parent).into_iter().filter(|entry| entry.runtime.runtime == *helper);
        let entry = matching.next().ok_or(ErrorCode::RuntimeDenied)?;
        if matching.next().is_some() { return Err(ErrorCode::RuntimeDenied); }
        Ok(entry)
    }
    pub fn from_verified_publication(runtimes: Vec<PublishedRuntime>, entitlements: Vec<EmployeeEntitlement>) -> Result<Self> {
        if runtimes.is_empty() || runtimes.len() > 128 || entitlements.len() > 30_000 { return Err(ErrorCode::RuntimeDenied); }
        let mut profiles = BTreeSet::new();
        for runtime in &runtimes {
            if !profiles.insert((runtime.profile.0).0) || runtime.pinned_version.as_str().is_empty()
                || matches!(runtime.pinned_version.as_str(), "latest" | "*")
                || runtime.fixed_args.len() > 128 || runtime.fixed_args.iter().any(|arg| arg.as_str().as_bytes().contains(&0)) {
                return Err(ErrorCode::RuntimeDenied);
            }
            match &runtime.executable {
                Executable::Elf { entry, .. } => registry_name(entry.as_str())?,
                Executable::Script { interpreter, script, .. } => {
                    registry_name(interpreter.as_str())?; registry_name(script.as_str())?;
                }
            }
            // D-Bus policy construction validates all hard resource ceilings;
            // use a placeholder device identity only for this pure validation.
            crate::systemd::UnitPolicy::new(duduclaw_isolation::ids::RunId([0; 16]), EmployeeId([0; 16]),
                &runtime.resources, &runtime.timeouts, 1, 1).map_err(|_| ErrorCode::RuntimeDenied)?;
            // R6: an invalid signed range fails the whole publication.
            validate_turn_limit(runtime)?;
        }
        let mut employees = BTreeMap::new();
        let mut turn_caps = BTreeMap::new();
        for entitlement in entitlements {
            let mut assigned = BTreeSet::new();
            if entitlement.profiles.is_empty() || entitlement.profiles.len() > 128
                || entitlement.max_turns.is_some_and(|cap| !(1..=TURN_LIMIT_CEILING).contains(&cap))
                || entitlement.profiles.iter().any(|profile| !profiles.contains(&(profile.0).0) || !assigned.insert((profile.0).0))
                || employees.insert(entitlement.employee, entitlement.profiles).is_some() { return Err(ErrorCode::RuntimeDenied); }
            if let Some(cap) = entitlement.max_turns { turn_caps.insert(entitlement.employee, cap); }
        }
        Ok(Self { runtimes, employees, launchers: BTreeMap::new(), turn_caps })
    }
    pub fn derive(&self, employee: EmployeeId, request: &RuntimeKind, pty: bool) -> Result<(&PublishedRuntime, LaunchPolicy)> {
        let profiles = self.employees.get(&employee).ok_or(ErrorCode::RuntimeDenied)?;
        let mut matching = self.runtimes.iter().filter(|runtime| runtime.runtime == *request && profiles.contains(&runtime.profile));
        let runtime = matching.next().ok_or(ErrorCode::RuntimeDenied)?;
        // Ambiguous publications cannot silently select a weaker profile.
        if matching.next().is_some() || (pty && !runtime.pty_allowed) { return Err(ErrorCode::RuntimeDenied); }
        let policy = LaunchPolicy { queue_class: runtime.queue_class, queue_ms: runtime.timeouts.queue_ms,
            startup_ms: runtime.timeouts.startup_ms, runtime_ms: runtime.timeouts.runtime_ms,
            pty_idle_ms: if pty { Some(runtime.timeouts.pty_idle_ms.ok_or(ErrorCode::RuntimeDenied)?) } else { None },
            ptys: runtime.resources.ptys, pty_size: pty.then_some(duduclaw_isolation::execution::PtySize { rows: 24, columns: 80 }), delivery: IoDelivery { mcp: runtime.mcp, pty } };
        policy.validate()?;
        Ok((runtime, policy))
    }
}
fn registry_name(name: &str) -> Result<()> {
    if name.is_empty() || matches!(name, "." | "..")
        || !name.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')) {
        return Err(ErrorCode::RuntimeDenied);
    }
    Ok(())
}

/// Rebuilt from zero; no gateway, daemon or login-session variable is copied.
pub fn runtime_environment(network: &NetworkPolicy, proxy_port: Option<u16>) -> Result<BTreeMap<String, String>> {
    let mut env: BTreeMap<_, _> = [("HOME", "/home/employee"), ("PATH", "/usr/bin:/bin"),
        ("XDG_RUNTIME_DIR", "/run/duduclaw"), ("TMPDIR", "/tmp"), ("LANG", "C.UTF-8")]
        .into_iter().map(|(key, value)| (key.to_owned(), value.to_owned())).collect();
    match (network, proxy_port) {
        (NetworkPolicy::Offline, None) => {},
        (NetworkPolicy::Egress(_), Some(port)) if port != 0 => {
            for key in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
                env.insert(key.into(), format!("http://127.0.0.1:{port}"));
            }
        }
        _ => return Err(ErrorCode::IsolationUnavailable),
    }
    Ok(env)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn runtime(byte: u8) -> PublishedRuntime {
        PublishedRuntime { runtime: RuntimeKind::Codex, profile: ProfileId(Sha256Digest([byte; 32])),
            executable: Executable::Elf { entry: Text::new("codex".into()).unwrap(), digest: Sha256Digest([1; 32]) },
            pinned_version: Text::new("1.2.3".into()).unwrap(), fixed_args: vec![],
            resources: ResourceLimits { memory_bytes: 1024 * 1024, cpu_quota_percent: 100, tasks: 16,
                open_files: 4096, ptys: 16, disk: duduclaw_isolation::execution::DiskQuota::LoopFilesystem { bytes: 1024, inodes: 32 },
                io_read_bytes_per_sec: 1024, io_write_bytes_per_sec: 1024, io_read_ops_per_sec: 10,
                io_write_ops_per_sec: 10, output_buffer_bytes: 4096, output_total_bytes: 4096, memfd_total_bytes: 4096 },
            timeouts: RunTimeouts { queue_ms: 1000, startup_ms: 1000, runtime_ms: 1000,
                pty_idle_ms: None, termination_grace_ms: 100 }, network: NetworkPolicy::Offline,
            mcp: true, pty_allowed: false, queue_class: QueueClass::Background, turn_limit: None }
    }
    #[test]
    fn entitlement_never_falls_back_and_ambiguous_profiles_deny() {
        let employee = EmployeeId([1; 16]);
        let first = runtime(1);
        let second = runtime(2);
        let policy = RootPolicy::from_verified_publication(vec![first.clone()], vec![EmployeeEntitlement {
            employee, profiles: vec![first.profile], max_turns: None }]).unwrap();
        assert!(policy.derive(employee, &RuntimeKind::Codex, false).is_ok());
        assert!(policy.derive(employee, &RuntimeKind::Claude, false).is_err());
        assert!(policy.derive(EmployeeId([2; 16]), &RuntimeKind::Codex, false).is_err());
        assert!(policy.derive(employee, &RuntimeKind::Codex, true).is_err());
        let policy = RootPolicy::from_verified_publication(vec![first.clone(), second.clone()], vec![EmployeeEntitlement {
            employee, profiles: vec![first.profile, second.profile], max_turns: None }]).unwrap();
        assert!(policy.derive(employee, &RuntimeKind::Codex, false).is_err());
    }

    /// A v3-shaped Claude record: signed arguments without `--max-turns`,
    /// ending with the fixed MCP suffix.
    fn ranged_claude(limit: TurnLimit) -> PublishedRuntime {
        let mut claude = runtime(7);
        claude.runtime = RuntimeKind::Claude;
        claude.fixed_args = ["-p", "--allowedTools", "mcp__duduclaw__*"].into_iter().map(str::to_owned)
            .chain(duduclaw_mcp_boundary::config::claude_mcp_arguments())
            .map(|arg| Text::new(arg).unwrap()).collect();
        claude.turn_limit = Some(limit);
        claude
    }
    #[test]
    fn turn_limit_truth_table() {
        let limit = TurnLimit { min: 5, max: 200, default: 50 };
        let some = Some(limit);
        // (signed range, entitlement cap, request) -> enforced value (None = refused)
        let rows: &[(Option<TurnLimit>, Option<u16>, Option<u16>, Option<Option<u16>>)] = &[
            // No signed range (v1/v2, or a v3 record without turn_limit): fixed args apply.
            (None, None, None, Some(None)),
            (None, Some(10), None, Some(None)),
            (None, None, Some(50), None),
            (None, None, Some(1), None),
            // Absent request means the signed default, narrowed by the entitlement.
            (some, None, None, Some(Some(50))),
            (some, Some(30), None, Some(Some(30))),
            (some, Some(500), None, Some(Some(50))),
            (some, Some(4), None, None),
            // Requests inside the range are taken as they are.
            (some, None, Some(5), Some(Some(5))),
            (some, None, Some(120), Some(Some(120))),
            (some, None, Some(200), Some(Some(200))),
            (some, Some(120), Some(120), Some(Some(120))),
            (some, Some(1000), Some(200), Some(Some(200))),
            // Outside the signed range: refused, never clamped.
            (some, None, Some(0), None),
            (some, None, Some(4), None),
            (some, None, Some(201), None),
            (some, None, Some(u16::MAX), None),
            // Above the employee's ceiling: refused (the run would get less than asked).
            (some, Some(30), Some(31), None),
            (some, Some(4), Some(5), None),
        ];
        for (index, (limit, cap, requested, expected)) in rows.iter().enumerate() {
            let actual = effective_max_turns(*limit, *cap, *requested).ok();
            assert_eq!(actual, *expected, "row {index}: {limit:?} cap {cap:?} request {requested:?}");
        }
        // An invalid signed range refuses every request, including the default.
        for bad in [TurnLimit { min: 0, max: 200, default: 50 }, TurnLimit { min: 60, max: 200, default: 50 },
            TurnLimit { min: 1, max: 40, default: 50 }, TurnLimit { min: 1, max: 1001, default: 50 }] {
            assert!(bad.validate().is_err(), "{bad:?}");
            assert!(effective_max_turns(Some(bad), None, None).is_err());
            assert!(effective_max_turns(Some(bad), None, Some(50)).is_err());
        }
        assert!(TurnLimit { min: 1, max: 1000, default: 1000 }.validate().is_ok());
        assert!(WORKING_DEFAULT_TURN_LIMIT.validate().is_ok());
        assert_eq!((WORKING_DEFAULT_TURN_LIMIT.min, WORKING_DEFAULT_TURN_LIMIT.default, WORKING_DEFAULT_TURN_LIMIT.max), (1, 50, 200));
    }
    #[test]
    fn gateway_narrowing_never_exceeds_signed_maximum_or_entitlement() {
        let limit = WORKING_DEFAULT_TURN_LIMIT;
        for desired in [0u64, 1, 49, 50, 51, 199, 200, 201, 10_000, u64::MAX] {
            for cap in [None, Some(1), Some(30), Some(200), Some(1000)] {
                let request = narrowed_request(limit, cap, desired);
                assert!(request <= limit.max && cap.is_none_or(|cap| request <= cap));
                assert!(u64::from(request) <= desired);
                // Whatever the gateway sends after narrowing is either enforced
                // exactly as sent, or refused (below min); never changed.
                match effective_max_turns(Some(limit), cap, Some(request)) {
                    Ok(value) => assert_eq!(value, Some(request)),
                    Err(_) => assert!(request < limit.min),
                }
            }
        }
        // A utility call (asks at most 50) is never above 50 under any range.
        for limit in [WORKING_DEFAULT_TURN_LIMIT, TurnLimit { min: 1, max: 1000, default: 1000 }] {
            let request = narrowed_request(limit, None, 10_000u64.min(50));
            assert_eq!(effective_max_turns(Some(limit), None, Some(request)).unwrap(), Some(50));
        }
    }
    #[test]
    fn publication_rejects_invalid_ranges_and_entitlement_caps_as_a_whole() {
        let employee = EmployeeId([1; 16]);
        let good = ranged_claude(WORKING_DEFAULT_TURN_LIMIT);
        let entitled = |max_turns| vec![EmployeeEntitlement { employee, profiles: vec![good.profile], max_turns }];
        let policy = RootPolicy::from_verified_publication(vec![good.clone()], entitled(Some(30))).unwrap();
        assert_eq!(policy.turn_cap(employee), Some(30));
        let (claude, _) = policy.derive(employee, &RuntimeKind::Claude, false).unwrap();
        assert_eq!(policy.max_turns(employee, claude, None), Ok(Some(30)));
        assert_eq!(policy.max_turns(employee, claude, Some(30)), Ok(Some(30)));
        assert!(policy.max_turns(employee, claude, Some(31)).is_err());
        assert!(policy.max_turns(EmployeeId([2; 16]), claude, Some(31)).is_ok());
        for cap in [0, TURN_LIMIT_CEILING + 1] {
            assert!(RootPolicy::from_verified_publication(vec![good.clone()], entitled(Some(cap))).is_err());
        }
        let mut bad = good.clone(); bad.turn_limit = Some(TurnLimit { min: 60, max: 200, default: 50 });
        assert!(RootPolicy::from_verified_publication(vec![bad], entitled(None)).is_err());
        // A ranged record must not also carry a fixed `--max-turns`.
        for smuggled in [vec!["--max-turns", "500"], vec!["--max-turns=500"]] {
            let mut bad = good.clone();
            for (offset, arg) in smuggled.into_iter().enumerate() { bad.fixed_args.insert(offset, Text::new(arg.into()).unwrap()); }
            assert!(RootPolicy::from_verified_publication(vec![bad], entitled(None)).is_err());
        }
        // Only a Claude MCP record with the fixed MCP suffix can carry a range.
        let mut bad = good.clone(); bad.fixed_args.pop();
        assert!(RootPolicy::from_verified_publication(vec![bad], entitled(None)).is_err());
        let mut bad = good.clone(); bad.mcp = false;
        assert!(RootPolicy::from_verified_publication(vec![bad], entitled(None)).is_err());
        let mut bad = runtime(3); bad.turn_limit = Some(WORKING_DEFAULT_TURN_LIMIT);
        let profile = bad.profile;
        assert!(RootPolicy::from_verified_publication(vec![bad], vec![EmployeeEntitlement {
            employee, profiles: vec![profile], max_turns: None }]).is_err());
    }
    #[test]
    fn fixed_environment_never_inherits_proxy_bypasses_or_credentials() {
        let env = runtime_environment(&NetworkPolicy::Offline, None).unwrap();
        assert_eq!(env.len(), 5);
        let profile = ProfileId(Sha256Digest([1; 32]));
        let env = runtime_environment(&NetworkPolicy::Egress(profile), Some(8080)).unwrap();
        for key in ["NO_PROXY", "no_proxy", "ALL_PROXY", "all_proxy", "FTP_PROXY", "ftp_proxy", "LD_PRELOAD", "SSH_AUTH_SOCK"] {
            assert!(!env.contains_key(key));
        }
        assert_eq!(env["HTTP_PROXY"], "http://127.0.0.1:8080");
        assert!(runtime_environment(&NetworkPolicy::Egress(profile), None).is_err());
        assert!(runtime_environment(&NetworkPolicy::Offline, Some(8080)).is_err());
    }
    #[test]
    fn helper_table_derives_from_run_employee_entitlement() {
        use crate::helper_policy::*;
        use duduclaw_run::seccomp::{Architecture, RuntimeProfile};
        let arch = Architecture::X86_64;
        let mut parent = runtime(1);
        parent.resources.memory_bytes = 1 << 30; parent.timeouts.runtime_ms = 600_000; parent.timeouts.termination_grace_ms = 15_000;
        let mut helper = runtime(2);
        helper.runtime = RuntimeKind::Registered(Text::new("duduclaw-program-v1".into()).unwrap()); helper.mcp = false;
        helper.timeouts.runtime_ms = 305_000;
        let seccomp = RuntimeProfile { allowed_syscalls: vec![0, 231, 322] };
        let binding = HelperBinding { profile: helper.profile, slot: HelperSlot::Foreground, seccomp_digest: seccomp.digest(arch).unwrap(),
            seccomp, self_only: vec![], memory_bytes: 1 << 20, tasks: 4, cpu_quota_percent: 50, open_files: 64,
            file_size_bytes: 512, timeout_ms: 60_000, filesystem: vec![HelperFs::SystemRo, HelperFs::Work], egress: false };
        let launcher = LauncherRecord { reserve: RuntimeReserve { memory_bytes: 1 << 20, tasks: 4, cpu_quota_percent: 10 }, helpers: vec![binding],
            helper_tmpfs_bytes: 4096 };
        let (entitled, other) = (EmployeeId([1; 16]), EmployeeId([2; 16]));
        let policy = RootPolicy::from_verified_publication(vec![parent.clone(), helper.clone()], vec![
            EmployeeEntitlement { employee: entitled, profiles: vec![parent.profile, helper.profile], max_turns: None },
            EmployeeEntitlement { employee: other, profiles: vec![parent.profile], max_turns: None }]).unwrap()
            // HX3b: the parent must hold every call the launcher issues under F_L.
            .with_launchers(arch, vec![(parent.profile, RuntimeProfile { allowed_syscalls: arch.launcher_parent_calls().to_vec() },
                launcher.clone())]).unwrap();
        assert_eq!(policy.helper_table(entitled, &parent).len(), 1);
        assert_eq!(policy.derive_helper(entitled, &parent, &helper.runtime).unwrap().index, 0);
        // Not entitled to the helper: absent, never another helper.
        assert!(policy.helper_table(other, &parent).is_empty());
        assert!(policy.derive_helper(other, &parent, &helper.runtime).is_err());
        assert!(policy.derive_helper(entitled, &parent, &RuntimeKind::Claude).is_err());
        // A parent filter narrower than the helper fails the whole publication.
        assert!(RootPolicy::from_verified_publication(vec![parent.clone(), helper], vec![]).unwrap()
            .with_launchers(arch, vec![(parent.profile, RuntimeProfile { allowed_syscalls: vec![231, 322] }, launcher)]).is_err());
    }
    #[test]
    fn executable_names_are_registry_components_only() {
        for value in ["/bin/sh", "../node", "node/cli.js", "", "..", "node\0"] { assert!(registry_name(value).is_err()); }
        assert!(registry_name("node-22.1.0").is_ok());
    }
}
