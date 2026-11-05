//! B1-to-B2 launch contract, separate from the immutable gateway IPC v1.
//! This is an explicit adapter proposal: no B2 implementation is silently
//! assumed compatible. Until an adapter verifies this revision, readiness denies.
use std::collections::BTreeMap;
use serde::Serialize;
use duduclaw_isolation::{execution::{RunHandle, NetworkPolicy}, ids::{EmployeeId, Sha256Digest},
    ipc::{ErrorCode, InputFd, MAX_FRAME_BYTES, MAX_INPUT_FDS}};
use crate::{Result, policy::{Executable, PublishedRuntime, runtime_environment}};

pub const BOOTSTRAP_VERSION: u16 = 1;
pub const BOOTSTRAP_DOMAIN: &str = "duduclaw-spawnd-to-run";
pub const BOOTSTRAP_FD: i32 = 0;
pub const MAX_BOOTSTRAP_FDS: usize = 80;
pub const TRAMPOLINE_ARGV: &[&str] = &[crate::systemd::TRAMPOLINE];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FdRole {
    Plan, Executable, Script, HomeMount, WorkMount, ProjectionMount, EtcMount, RuntimeMount,
    Stdin, Prompt, Attachment { index: u16 }, Mcp, Egress,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DescriptorPlan {
    /// Index in SCM_RIGHTS ordering, not a process fd number.
    pub index: u16,
    pub role: FdRole,
    /// Runtime-only destination; mounts/exec/egress/plan never survive there.
    pub runtime_fd: Option<u16>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExecutablePlan {
    Elf { entry: String, digest: Sha256Digest },
    Script { interpreter: String, interpreter_digest: Sha256Digest, script: String, script_digest: Sha256Digest },
}

/// Serialized into a sealed root-owned memfd. Root policy derives every value;
/// request frames never supply paths, uid, argv, environment or mount choices.
#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BootstrapPlan {
    pub domain: &'static str,
    pub version: u16,
    pub run: RunHandle,
    pub employee: EmployeeId,
    pub profile: duduclaw_isolation::egress::ProfileId,
    pub runtime_version: String,
    pub proxy_port: Option<u16>,
    pub uid: u32,
    pub gid: u32,
    pub supervisor_uid: u32,
    pub supervisor_gid: u32,
    pub nonce: [u8; 32],
    pub executable: ExecutablePlan,
    pub argv: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub input_fds: Vec<InputFd>,
    pub prompt_bytes: u64,
    pub descriptors: Vec<DescriptorPlan>,
    pub open_files: u32,
    pub memfd_total_bytes: u64,
    pub pty: bool,
    pub max_ptys: u16,
    pub pty_size: Option<duduclaw_isolation::execution::PtySize>,
    pub mcp: bool,
    pub egress: bool,
}
impl BootstrapPlan {
    #[allow(clippy::too_many_arguments)]
    pub fn from_runtime(run: RunHandle, employee: EmployeeId, uid: u32, gid: u32,
        supervisor_uid: u32, supervisor_gid: u32, nonce: [u8; 32], runtime: &PublishedRuntime,
        pty: bool, prompt_bytes: u64, input_fds: &[InputFd], proxy_port: Option<u16>) -> Result<Self> {
        let (executable, mut argv) = match &runtime.executable {
            Executable::Elf { entry, digest } => (ExecutablePlan::Elf { entry: entry.as_str().into(), digest: *digest },
                vec![format!("/runtime/{}", entry.as_str())]),
            Executable::Script { interpreter, interpreter_digest, script, script_digest } =>
                (ExecutablePlan::Script { interpreter: interpreter.as_str().into(), interpreter_digest: *interpreter_digest,
                    script: script.as_str().into(), script_digest: *script_digest },
                    vec![format!("/runtime/{}", interpreter.as_str()), format!("/runtime/{}", script.as_str())]),
        };
        argv.extend(runtime.fixed_args.iter().map(|arg| arg.as_str().to_owned()));
        let egress = matches!(runtime.network, NetworkPolicy::Egress(_));
        let mut plan = Self { domain: BOOTSTRAP_DOMAIN, version: BOOTSTRAP_VERSION, run, employee,
            profile: runtime.profile, runtime_version: runtime.pinned_version.as_str().into(), proxy_port,
            uid, gid, supervisor_uid, supervisor_gid, nonce, executable, argv,
            environment: runtime_environment(&runtime.network, proxy_port)?, input_fds: input_fds.to_vec(),
            prompt_bytes, descriptors: Vec::new(), open_files: runtime.resources.open_files,
            memfd_total_bytes: runtime.resources.memfd_total_bytes,
            pty, max_ptys: runtime.resources.ptys, pty_size: pty.then_some(duduclaw_isolation::execution::PtySize { rows: 24, columns: 80 }), mcp: runtime.mcp, egress };
        if pty && !runtime.pty_allowed { return Err(ErrorCode::RuntimeDenied); }
        plan.descriptors = plan.expected_descriptors();
        plan.validate()?;
        Ok(plan)
    }
    fn expected_descriptors(&self) -> Vec<DescriptorPlan> {
        let mut roles = vec![(FdRole::Plan, None), (FdRole::Executable, None)];
        if matches!(self.executable, ExecutablePlan::Script { .. }) { roles.push((FdRole::Script, None)); }
        roles.extend([(FdRole::HomeMount, None), (FdRole::WorkMount, None),
            (FdRole::ProjectionMount, None), (FdRole::EtcMount, None), (FdRole::RuntimeMount, None), (FdRole::Prompt, Some(4))]);
        if !self.pty { roles.push((FdRole::Stdin, Some(0))); }
        for index in 0..self.input_fds.len() {
            roles.push((FdRole::Attachment { index: index as u16 }, Some(5 + index as u16)));
        }
        if self.mcp { roles.push((FdRole::Mcp, Some(3))); }
        if self.egress { roles.push((FdRole::Egress, None)); }
        roles.into_iter().enumerate().map(|(index, (role, runtime_fd))|
            DescriptorPlan { index: index as u16, role, runtime_fd }).collect()
    }
    pub fn validate(&self) -> Result<()> {
        if self.pty_size.is_some() != self.pty || self.pty_size.is_some_and(|size| size.validate().is_err()) {
            return Err(ErrorCode::InvalidRequest);
        }
        if self.domain != BOOTSTRAP_DOMAIN || self.version != BOOTSTRAP_VERSION
            || !crate::storage::valid_employee_uid(self.uid) || self.gid != self.uid
            || self.supervisor_uid == 0 || self.supervisor_uid >= 1000 || self.supervisor_gid == 0 || self.supervisor_gid >= 1000
            || self.nonce == [0; 32] || self.input_fds.len() > MAX_INPUT_FDS
            || self.prompt_bytes > duduclaw_isolation::ipc::MAX_PROMPT_BYTES
            || self.descriptors.len() > MAX_BOOTSTRAP_FDS || self.descriptors != self.expected_descriptors()
            || self.open_files < 1024 + 2 * 72 + 16 || (self.pty && (self.max_ptys == 0 || self.max_ptys > 64))
            || self.argv.is_empty() || self.argv.len() > 130 || self.argv.iter().any(|arg| arg.len() > 4096 || arg.as_bytes().contains(&0)) {
            return Err(ErrorCode::IsolationUnavailable);
        }
        let component = |name: &str| !name.is_empty() && !matches!(name, "." | "..")
            && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
        let prefix = match &self.executable {
            ExecutablePlan::Elf { entry, .. } if component(entry) => vec![format!("/runtime/{entry}")],
            ExecutablePlan::Script { interpreter, script, .. } if component(interpreter) && component(script) =>
                vec![format!("/runtime/{interpreter}"), format!("/runtime/{script}")],
            _ => return Err(ErrorCode::RuntimeDenied),
        };
        let network = if self.egress { NetworkPolicy::Egress(self.profile) } else { NetworkPolicy::Offline };
        if self.argv.get(..prefix.len()) != Some(prefix.as_slice())
            || self.environment != runtime_environment(&network, self.proxy_port)?
            || self.runtime_version.is_empty() || matches!(self.runtime_version.as_str(), "latest" | "*")
            || self.runtime_version.len() > 64 {
            return Err(ErrorCode::RuntimeDenied);
        }
        let mut total = 0u64;
        let mut memfds = self.prompt_bytes;
        for input in &self.input_fds {
            let bytes = match input { InputFd::SealedMemfd { bytes } | InputFd::ReadOnlySnapshot { bytes } => *bytes };
            total = total.checked_add(bytes).ok_or(ErrorCode::ResourceLimit)?;
            if matches!(input, InputFd::SealedMemfd { .. }) {
                if bytes > 16 * 1024 * 1024 { return Err(ErrorCode::ResourceLimit); }
                memfds = memfds.checked_add(bytes).ok_or(ErrorCode::ResourceLimit)?;
            }
        }
        if self.memfd_total_bytes == 0 || self.memfd_total_bytes > 64 * 1024 * 1024
            || memfds > self.memfd_total_bytes || total > duduclaw_isolation::ipc::MAX_ATTACHMENT_TOTAL_BYTES { return Err(ErrorCode::ResourceLimit); }
        Ok(())
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        // At most 130 bounded args and 64 descriptors; the final frame bound also
        // accounts for JSON escaping without silently truncating any content.
        let bytes = serde_json::to_vec(self).map_err(|_| ErrorCode::Internal)?;
        if bytes.len() > MAX_FRAME_BYTES { return Err(ErrorCode::ResourceLimit); }
        Ok(bytes)
    }
    #[cfg(target_os = "linux")]
    pub fn seal(&self) -> Result<std::os::fd::OwnedFd> {
        crate::fds::sealed_plan(&self.encode()?).map_err(|_| ErrorCode::IsolationUnavailable)
    }
}

/// Content for B1's minimal read-only /etc projection. The adapter publishes
/// these exact regular files beneath a root-owned private anchor, fsyncs it,
/// clones a recursive private read-only/nosuid/nodev/nosymfollow mount and hands
/// only that detached mount to B2. Never bind the host /etc or shadow database.
pub fn minimal_etc(uid: u32, gid: u32) -> Result<BTreeMap<&'static str, String>> {
    if !crate::storage::valid_employee_uid(uid) || uid != gid { return Err(ErrorCode::InvalidRequest); }
    let user = format!("duduclaw-emp-{uid}");
    Ok(BTreeMap::from([
        ("passwd", format!("root:x:0:0:root:/nonexistent:/usr/sbin/nologin\n{user}:x:{uid}:{gid}:employee:/home/employee:/usr/sbin/nologin\n")),
        ("group", format!("root:x:0:\n{user}:x:{gid}:\n")),
        ("nsswitch.conf", "passwd: files\ngroup: files\nhosts: files\n".into()),
        ("hosts", "127.0.0.1 localhost\n::1 localhost\n".into()),
        ("resolv.conf", "nameserver 192.0.2.1\noptions attempts:1 timeout:1\n".into()),
    ]))
}

/// Readback from a compatible B2 adapter's actual probes, never config booleans.
#[derive(Debug, Clone, Copy, Default)]
pub struct TrampolineReadiness {
    pub version: u16,
    pub executable_verified: bool,
    pub landlock_self_test: bool,
    pub namespace_self_test: bool,
    pub detected_landlock_abi: u32,
    pub selected_landlock_abi: u32,
}
impl TrampolineReadiness {
    pub fn check(&self) -> Result<()> {
        if self.version != BOOTSTRAP_VERSION || !self.executable_verified
            || !self.landlock_self_test || !self.namespace_self_test
            || self.detected_landlock_abi == 0
            || self.selected_landlock_abi != self.detected_landlock_abi.min(7) {
            return Err(ErrorCode::IsolationUnavailable);
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
pub struct PreparedBootstrap { pub bootstrap_for_unit: std::os::fd::OwnedFd }

/// B1's adapter must seal the plan, create a root-authenticated seqpacket pair,
/// validate/prepare all mount and executable fds, and deliver the exact manifest.
/// B2 must verify nonce, version, seals, statx/mount identities and descriptor
/// types, reject unknown fields/rights/truncation, and never accept host paths.
/// The root manifest may contain up to 80 fds: adapters must use a dedicated
/// bounded bootstrap transport (gateway transport caps rights at 65).
///
/// prepare keeps the runtime release barrier closed. bind_unit validates the
/// actual systemd receipt/cgroup and the cancellation fence immediately before release. poll may report Running only
/// after every namespace, uid/gid, capability, seccomp, Landlock and fd readback;
/// PTY requires a returned master from the new per-run devpts, never host devpts.
/// All nonce exchanges are one-shot, run-bound and deadline-bounded. close drops
/// every original descriptor; no inherited root fd remains after cleanup.
#[cfg(target_os = "linux")]
pub trait Trampoline: Send {
    fn readiness(&mut self) -> Result<TrampolineReadiness>;
    fn prepare(&mut self, plan: BootstrapPlan, sealed_plan: std::os::fd::OwnedFd, mounts: crate::bootstrap_mounts::PreparedMounts,
        inputs: Vec<std::os::fd::OwnedFd>, stdin: Option<std::os::fd::OwnedFd>, mcp: Option<std::os::fd::OwnedFd>,
        egress: Option<std::os::fd::OwnedFd>, profile: crate::native_trampoline::NativeProfile) -> Result<PreparedBootstrap>;
    fn bind_unit(&mut self, run: RunHandle, unit: &crate::systemd::linux::StartedUnit, cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Result<()>;
    fn poll(&mut self, run: RunHandle) -> Result<crate::admission::HostProgress>;
    fn resize(&mut self, run: RunHandle, size: duduclaw_isolation::execution::PtySize) -> Result<()>;
    /// Returns the validated per-run master exactly once after Running.
    fn take_pty(&mut self, run: RunHandle) -> Result<std::os::fd::OwnedFd>;
    fn close(&mut self, run: RunHandle) -> Result<()>;
}
#[cfg(target_os = "linux")]
pub struct UnavailableTrampoline;
#[cfg(target_os = "linux")]
impl Trampoline for UnavailableTrampoline {
    fn readiness(&mut self) -> Result<TrampolineReadiness> { Err(ErrorCode::IsolationUnavailable) }
    fn prepare(&mut self, _: BootstrapPlan, _: std::os::fd::OwnedFd, _: crate::bootstrap_mounts::PreparedMounts, _: Vec<std::os::fd::OwnedFd>,
        _: Option<std::os::fd::OwnedFd>, _: Option<std::os::fd::OwnedFd>, _: Option<std::os::fd::OwnedFd>, _: crate::native_trampoline::NativeProfile) -> Result<PreparedBootstrap> { Err(ErrorCode::IsolationUnavailable) }
    fn bind_unit(&mut self, _: RunHandle, _: &crate::systemd::linux::StartedUnit, _: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Result<()> { Err(ErrorCode::IsolationUnavailable) }
    fn poll(&mut self, _: RunHandle) -> Result<crate::admission::HostProgress> { Err(ErrorCode::IsolationUnavailable) }
    fn resize(&mut self, _: RunHandle, _: duduclaw_isolation::execution::PtySize) -> Result<()> { Err(ErrorCode::IsolationUnavailable) }
    fn take_pty(&mut self, _: RunHandle) -> Result<std::os::fd::OwnedFd> { Err(ErrorCode::IsolationUnavailable) }
    fn close(&mut self, _: RunHandle) -> Result<()> { Err(ErrorCode::IsolationUnavailable) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_isolation::{bounded::Text, egress::ProfileId,
        execution::{ResourceLimits, RunTimeouts, RuntimeKind, DiskQuota}, ids::RunId, ipc::QueueClass};
    fn runtime() -> PublishedRuntime {
        PublishedRuntime { runtime: RuntimeKind::Codex, profile: ProfileId(Sha256Digest([1; 32])),
            executable: Executable::Elf { entry: Text::new("codex".into()).unwrap(), digest: Sha256Digest([2; 32]) },
            pinned_version: Text::new("1.2.3".into()).unwrap(), fixed_args: vec![],
            resources: ResourceLimits { memory_bytes: 1024 * 1024, cpu_quota_percent: 100, tasks: 16,
                open_files: 4096, ptys: 16, disk: DiskQuota::LoopFilesystem { bytes: 1024, inodes: 32 },
                io_read_bytes_per_sec: 1024, io_write_bytes_per_sec: 1024, io_read_ops_per_sec: 10,
                io_write_ops_per_sec: 10, output_buffer_bytes: 4096, output_total_bytes: 4096, memfd_total_bytes: 4096 },
            timeouts: RunTimeouts { queue_ms: 1000, startup_ms: 1000, runtime_ms: 1000,
                pty_idle_ms: None, termination_grace_ms: 100 }, network: NetworkPolicy::Offline,
            mcp: true, pty_allowed: true, queue_class: QueueClass::Background }
    }
    fn plan() -> BootstrapPlan {
        BootstrapPlan::from_runtime(RunHandle(RunId([1; 16])), EmployeeId([2; 16]), 70000, 70000,
            400, 400, [5; 32], &runtime(), false, 2, &[], None).unwrap()
    }
    #[test]
    fn bootstrap_exact_order_nonce_identity_and_fd_limit_are_required() {
        let valid = plan(); assert!(valid.encode().is_ok());
        assert_eq!(TRAMPOLINE_ARGV, &["/usr/lib/duduclaw/duduclaw-run"]);
        assert_eq!(valid.argv, ["/runtime/codex"]);
        let mounts: Vec<_> = valid.descriptors.iter().filter(|fd| matches!(fd.role,
            FdRole::HomeMount | FdRole::WorkMount | FdRole::ProjectionMount | FdRole::EtcMount | FdRole::RuntimeMount)).collect();
        assert_eq!(mounts.len(), 5);
        assert!(mounts.iter().all(|fd| fd.runtime_fd.is_none()));
        assert_eq!(valid.descriptors.iter().filter(|fd| fd.role == FdRole::ProjectionMount).count(), 1);
        assert_eq!(valid.descriptors.iter().find(|fd| fd.role == FdRole::Stdin).unwrap().runtime_fd, Some(0));
        assert_eq!(valid.descriptors.iter().find(|fd| fd.role == FdRole::Prompt).unwrap().runtime_fd, Some(4));
        let mut bad = valid.clone(); bad.nonce = [0; 32]; assert!(bad.validate().is_err());
        let mut bad = valid.clone(); bad.descriptors.swap(1, 2); assert!(bad.validate().is_err());
        let mut bad = valid.clone(); bad.descriptors[0].runtime_fd = Some(3); assert!(bad.validate().is_err());
        let mut bad = valid.clone(); bad.open_files = 1024; assert!(bad.validate().is_err());
        let mut bad = valid.clone(); bad.uid = 61184; assert!(bad.validate().is_err());
        let mut bad = valid; bad.supervisor_uid = 0; assert!(bad.validate().is_err());
        assert!(TrampolineReadiness::default().check().is_err());
        let mut bad = plan(); bad.environment.insert("LD_PRELOAD".into(), "/tmp/a".into()); assert!(bad.validate().is_err());
        let mut bad = plan(); bad.argv[0] = "/bin/sh".into(); assert!(bad.validate().is_err());
    }
    #[test]
    fn bootstrap_sixty_four_attachments_have_unique_runtime_slots() {
        let inputs = vec![InputFd::SealedMemfd { bytes: 1 }; 64];
        let plan = BootstrapPlan::from_runtime(RunHandle(RunId([1; 16])), EmployeeId([2; 16]), 70000, 70000,
            400, 400, [5; 32], &runtime(), false, 0, &inputs, None).unwrap();
        let destinations: Vec<_> = plan.descriptors.iter().filter_map(|entry| entry.runtime_fd).collect();
        let unique: std::collections::BTreeSet<_> = destinations.iter().copied().collect();
        assert_eq!(unique.len(), destinations.len());
        assert!(unique.contains(&68));
        assert!(plan.descriptors.len() <= MAX_BOOTSTRAP_FDS);
    }
    #[test]
    fn minimal_etc_contains_only_fixed_local_identity() {
        let files = minimal_etc(70000, 70000).unwrap();
        assert_eq!(files.len(), 5);
        assert_eq!(files["passwd"].lines().count(), 2);
        assert!(files["passwd"].lines().any(|line| line == "duduclaw-emp-70000:x:70000:70000:employee:/home/employee:/usr/sbin/nologin"));
        assert!(!files.contains_key("shadow"));
        assert!(minimal_etc(0, 0).is_err());
    }
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires a root-capable Linux acceptance VM; validates real sealed bootstrap memfd"]
    fn iso_root_bootstrap_plan_seals_and_readback() {
        use std::{io::{Read, Seek, SeekFrom}, os::fd::AsFd};
        assert_eq!(unsafe { libc::geteuid() }, 0);
        let plan = plan(); let bytes = plan.encode().unwrap(); let fd = plan.seal().unwrap();
        crate::fds::validate_sealed(fd.as_fd(), bytes.len() as u64).unwrap();
        assert!(nix::unistd::write(&fd, b"tamper").is_err());
        let mut file = std::fs::File::from(fd); file.seek(SeekFrom::Start(0)).unwrap();
        let mut actual = Vec::new(); file.read_to_end(&mut actual).unwrap(); assert_eq!(actual, bytes);
    }
}
