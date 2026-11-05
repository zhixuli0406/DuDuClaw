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
}

/// A root entitlement points to immutable profiles, not user-selected argv/env.
pub struct EmployeeEntitlement {
    pub employee: EmployeeId,
    pub profiles: Vec<ProfileId>,
}

#[derive(Clone)]
pub struct RootPolicy { runtimes: Vec<PublishedRuntime>, employees: BTreeMap<EmployeeId, Vec<ProfileId>> }
impl RootPolicy {
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
        }
        let mut employees = BTreeMap::new();
        for entitlement in entitlements {
            let mut assigned = BTreeSet::new();
            if entitlement.profiles.is_empty() || entitlement.profiles.len() > 128
                || entitlement.profiles.iter().any(|profile| !profiles.contains(&(profile.0).0) || !assigned.insert((profile.0).0))
                || employees.insert(entitlement.employee, entitlement.profiles).is_some() { return Err(ErrorCode::RuntimeDenied); }
        }
        Ok(Self { runtimes, employees })
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
            mcp: true, pty_allowed: false, queue_class: QueueClass::Background }
    }
    #[test]
    fn entitlement_never_falls_back_and_ambiguous_profiles_deny() {
        let employee = EmployeeId([1; 16]);
        let first = runtime(1);
        let second = runtime(2);
        let policy = RootPolicy::from_verified_publication(vec![first.clone()], vec![EmployeeEntitlement {
            employee, profiles: vec![first.profile] }]).unwrap();
        assert!(policy.derive(employee, &RuntimeKind::Codex, false).is_ok());
        assert!(policy.derive(employee, &RuntimeKind::Claude, false).is_err());
        assert!(policy.derive(EmployeeId([2; 16]), &RuntimeKind::Codex, false).is_err());
        assert!(policy.derive(employee, &RuntimeKind::Codex, true).is_err());
        let policy = RootPolicy::from_verified_publication(vec![first.clone(), second.clone()], vec![EmployeeEntitlement {
            employee, profiles: vec![first.profile, second.profile] }]).unwrap();
        assert!(policy.derive(employee, &RuntimeKind::Codex, false).is_err());
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
    fn executable_names_are_registry_components_only() {
        for value in ["/bin/sh", "../node", "node/cli.js", "", "..", "node\0"] { assert!(registry_name(value).is_err()); }
        assert!(registry_name("node-22.1.0").is_ok());
    }
}
