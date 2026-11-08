//! Production B1/B2 transport. Authority is supplied by the root publication;
//! incomplete installations retain a refusal, never synthesize a runtime profile.
use std::{collections::BTreeMap, ffi::CString, os::fd::{AsFd, AsRawFd, OwnedFd},
    sync::{Arc, atomic::{AtomicBool, Ordering}}};
use duduclaw_isolation::{execution::{RunHandle, RunOutcome, PtySize}, ids::{RunId, Sha256Digest}, ipc::ErrorCode};
use duduclaw_run::{linux::{bootstrap, fd, runner::{TrustedRun, Program, MountIdentity}, mounts::RootConfig, sealed_registry},
    seccomp::RuntimeProfile, status::StatusRecord};
use crate::{Result, bootstrap::{BootstrapPlan, PreparedBootstrap, Trampoline, TrampolineReadiness},
    bootstrap_mounts::PreparedMounts, admission::HostProgress};
fn unavailable() -> ErrorCode { ErrorCode::IsolationUnavailable }
fn checked<T,E>(result: std::result::Result<T,E>) -> Result<T> { result.map_err(|_| unavailable()) }

/// This is deliberately not deserializable from gateway input. F's root
/// publication must verify the signed architecture-specific filter digests.
pub struct NativeProfile {
    pub runtime: RuntimeProfile,
    pub runtime_digest: Sha256Digest,
    pub forwarder: Option<ForwarderProfile>,
    pub snapshot: crate::fds::SnapshotAuthority,
    pub tmpfs_bytes: u64,
    pub tmpfs_inodes: u64,
    pub proc_subset_pid: bool,
    /// HX helper table for this run, already filtered to the run employee's
    /// entitlements (`RootPolicy::helper_table`). `None` means no launcher.
    pub launcher: Option<LauncherProfile>,
}
pub struct LauncherProfile {
    pub helpers: Vec<HelperRun>,
    /// Read-only helper registry clone (`bootstrap_mounts::prepare_helpers`).
    pub mount: OwnedFd,
    pub tmpfs_bytes: u64,
    /// Launcher drain window F (§2.5 (b)); see `launcher_drain_ms`.
    pub drain_ms: u64,
}
/// F = parent `termination_grace_ms` − 2 s, at least 1 s, at most the trampoline bound.
pub fn launcher_drain_ms(termination_grace_ms: u64) -> u64 {
    termination_grace_ms.saturating_sub(2_000).clamp(1_000, duduclaw_run::linux::runner::MAX_LAUNCHER_DRAIN_MS)
}
pub struct HelperRun {
    /// Position in the parent's signed helper list.
    pub index: u8,
    pub binding: crate::helper_policy::HelperBinding,
    pub runtime: crate::policy::PublishedRuntime,
    pub executable: crate::bootstrap_mounts::VerifiedExecutable,
    pub script: Option<crate::bootstrap_mounts::VerifiedExecutable>,
}
/// Helper availability of one run; HX4 answers `HelperCall` from this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelperState {
    /// The parent has no (entitled) helpers, or the trampoline has no launcher yet.
    NotPlanned,
    Available,
    /// Readiness / per-call reason, e.g. `helper_cgroup_unavailable`.
    Unavailable(&'static str),
}
pub struct ForwarderProfile {
    pub entry: String,
    pub executable_digest: Sha256Digest,
    pub argv: Vec<CString>,
    pub seccomp: RuntimeProfile,
    pub seccomp_digest: Sha256Digest,
}
/// Supplied by the installer only after binary digest and isolated namespace /
/// Landlock self-tests have been read back. Kernel ABI probing alone is not it.
pub trait Installation: Send { fn readback(&mut self) -> Result<TrampolineReadiness>; }
pub struct MissingInstallation;
impl Installation for MissingInstallation { fn readback(&mut self) -> Result<TrampolineReadiness> { Err(unavailable()) } }

#[derive(Default)]
struct Stages { employee: bool, supervisor: bool, init: bool, isolated: bool }
impl Stages {
    fn ready(&self) -> bool { self.employee && self.supervisor && self.init && self.isolated }
    fn record(&mut self, record: StatusRecord) -> Result<Option<RunOutcome>> {
        let bit = match record {
            StatusRecord::EmployeeReady { .. } => &mut self.employee,
            StatusRecord::SupervisorReady => &mut self.supervisor,
            StatusRecord::ChildrenForked => &mut self.init,
            StatusRecord::IsolationReady => &mut self.isolated,
            StatusRecord::Exited(code) => return Ok(Some(RunOutcome::Exited(i32::from(code)))),
            StatusRecord::Signalled(signal) => return Ok(Some(RunOutcome::Signalled(u32::from(signal)))),
            StatusRecord::Error(_) => return Err(unavailable()),
            StatusRecord::Pty | StatusRecord::PtyIdentity { .. } => return Err(unavailable()),
        };
        if *bit { return Err(unavailable()); }
        *bit = true;
        Ok(None)
    }
}
struct Pending {
    socket: OwnedFd,
    manifest: Option<(TrustedRun, Vec<OwnedFd>)>,
    cancelled: Option<Arc<AtomicBool>>,
    stages: Stages,
    prepared_at_ms: u64,
    terminal: Option<RunOutcome>,
    pty: bool,
    uid: u32,
    master: Option<crate::native_pty::ReturnedPty>,
    /// spawnd's end of the per-run launcher control socketpair (§2.2).
    control: Option<OwnedFd>,
    /// Signed indices in run-table order: `Start.helper` is a position in this list.
    helper_indices: Vec<u8>,
    /// Launcher rights already in `manifest` (cgroup fds are added at bind).
    launcher_rights: usize,
    cgroups: Option<crate::helper_cgroup::linux::RunCgroups>,
    helpers: HelperState,
}
impl NativeTrampoline {
    pub fn helper_state(&self, run: RunHandle) -> Option<HelperState> { self.runs.get(&run.0).map(|pending| pending.helpers) }
    /// HX4: the run-table position of a helper derived from `entry.employee`.
    pub fn helper_position(&self, run: RunHandle, signed_index: u8) -> Option<u8> {
        let pending = self.runs.get(&run.0)?;
        if pending.helpers != HelperState::Available { return None; }
        pending.helper_indices.iter().position(|index| *index == signed_index).and_then(|p| u8::try_from(p).ok())
    }
    /// HX4: spawnd's control socket end and root-held slot cgroups of a run.
    pub fn helper_channel(&mut self, run: RunHandle) -> Option<(&OwnedFd, &mut crate::helper_cgroup::linux::RunCgroups)> {
        let pending = self.runs.get_mut(&run.0)?;
        if pending.helpers != HelperState::Available { return None; }
        Some((pending.control.as_ref()?, pending.cgroups.as_mut()?))
    }
}
const HELPER_CGROUP_FDS: usize = duduclaw_run::helper_policy::HELPER_CGROUP_FDS;
/// Build the trampoline's helper table and its descriptors (control socket end,
/// helper registry mount, per helper executable and optional script).
fn launcher_plan(profile: LauncherProfile, control: OwnedFd) -> Result<(duduclaw_run::linux::runner::LauncherPlan, Vec<OwnedFd>, Vec<u8>)> {
    use duduclaw_run::linux::runner::{HelperProgram, LauncherPlan};
    let observed = checked(fd::observe(profile.mount.as_raw_fd()))?;
    let mounts = vec![MountIdentity { mount: observed.identity.mount, fs_type: observed.fs_type }];
    let mut rights = vec![control, profile.mount];
    let mut helpers = Vec::new();
    let mut indices = Vec::new();
    for helper in profile.helpers {
        let (mut argv, script_name) = match &helper.runtime.executable {
            crate::policy::Executable::Elf { entry, .. } => (vec![format!("/helpers/{}", entry.as_str())], None),
            crate::policy::Executable::Script { interpreter, script, .. } =>
                (vec![format!("/helpers/{}", interpreter.as_str()), format!("/helpers/{}", script.as_str())], Some(script.as_str().to_owned())),
        };
        argv.extend(helper.runtime.fixed_args.iter().map(|arg| arg.as_str().to_owned()));
        let script = match (script_name, &helper.script) {
            (Some(name), Some(value)) => Some((checked(fd::observe(value.fd.as_raw_fd()))?.identity, checked(CString::new(name))?)),
            (None, None) => None,
            _ => return Err(unavailable()),
        };
        let binding = helper.binding;
        helpers.push(HelperProgram {
            program: Program { identity: checked(fd::observe(helper.executable.fd.as_raw_fd()))?.identity, script,
                argv: argv.into_iter().map(|arg| checked(CString::new(arg))).collect::<Result<_>>()?,
                environment: vec![], seccomp: binding.seccomp, seccomp_digest: binding.seccomp_digest },
            self_only: binding.self_only, mount: 0, slot: binding.slot, filesystem: binding.filesystem,
            open_files: binding.open_files, file_size_bytes: binding.file_size_bytes, timeout_ms: binding.timeout_ms,
            egress: binding.egress });
        rights.push(helper.executable.fd);
        if let Some(script) = helper.script { rights.push(script.fd); }
        indices.push(helper.index);
    }
    Ok((LauncherPlan { helpers, mounts, tmpfs_bytes: profile.tmpfs_bytes, drain_ms: profile.drain_ms }, rights, indices))
}
pub struct NativeTrampoline { installation: Box<dyn Installation>, runs: BTreeMap<RunId, Pending> }
impl NativeTrampoline {
    pub fn new(installation: Box<dyn Installation>) -> Self { Self { installation, runs: BTreeMap::new() } }
}
impl Default for NativeTrampoline { fn default() -> Self { Self::new(Box::new(MissingInstallation)) } }
impl Trampoline for NativeTrampoline {
    fn readiness(&mut self) -> Result<TrampolineReadiness> {
        let result = self.installation.readback()?;
        result.check()?;
        Ok(result)
    }
    fn prepare(&mut self, plan: BootstrapPlan, sealed: OwnedFd, mounts: PreparedMounts,
        inputs: Vec<OwnedFd>, stdin: Option<OwnedFd>, mcp: Option<OwnedFd>, egress: Option<OwnedFd>,
        profile: NativeProfile) -> Result<PreparedBootstrap> {
        plan.validate()?;
        if self.runs.contains_key(&plan.run.0) || self.runs.len() >= 128
            || mounts.projection_run != plan.run || inputs.len() != 1 + plan.input_fds.len()
            || stdin.is_some() == plan.pty || mcp.is_some() != plan.mcp || egress.is_some() != plan.egress
            || profile.forwarder.is_some() != plan.egress { return Err(unavailable()); }
        let bytes = plan.encode()?;
        checked(crate::fds::validate_sealed(sealed.as_fd(), bytes.len() as u64))?;
        use std::os::unix::fs::FileExt;
        let mut actual = vec![0; bytes.len()];
        checked(std::fs::File::from(sealed).read_exact_at(&mut actual, 0))?;
        if actual != bytes { return Err(unavailable()); }
        let PreparedMounts { home, work, projection, etc, runtime, executable, script, .. } = mounts;
        let identities: Vec<_> = [&home,&work,&projection,&runtime,&etc].into_iter()
            .map(|value| checked(fd::observe(value.as_raw_fd())).map(|value|
                MountIdentity { mount: value.identity.mount, fs_type: value.fs_type })).collect::<Result<_>>()?;
        let program = Program {
            identity: checked(fd::observe(executable.fd.as_raw_fd()))?.identity,
            script: match (&plan.executable, &script) {
                (crate::bootstrap::ExecutablePlan::Script { script: name, .. }, Some(value)) =>
                    Some((checked(fd::observe(value.fd.as_raw_fd()))?.identity, checked(CString::new(name.as_str()))?)),
                (crate::bootstrap::ExecutablePlan::Elf { .. }, None) => None,
                _ => return Err(unavailable()),
            },
            argv: plan.argv.iter().map(|arg| checked(CString::new(arg.as_str()))).collect::<Result<_>>()?,
            environment: plan.environment.iter().map(|(key,value)| checked(CString::new(format!("{key}={value}")))).collect::<Result<_>>()?,
            seccomp: profile.runtime, seccomp_digest: profile.runtime_digest,
        };
        let forwarder = if let Some(value) = profile.forwarder {
            let verified = checked(crate::bootstrap_mounts::open_verified(runtime.as_fd(), &value.entry,
                value.executable_digest, true, std::time::Instant::now() + std::time::Duration::from_secs(15)))?;
            if value.argv.first().map(|arg| arg.as_bytes()) != Some(format!("/runtime/{}", value.entry).as_bytes()) {
                return Err(unavailable());
            }
            Some((Program { identity: checked(fd::observe(verified.fd.as_raw_fd()))?.identity,
                script: None, argv: value.argv, environment: vec![], seccomp: value.seccomp,
                seccomp_digest: value.seccomp_digest }, verified.fd))
        } else { None };
        let mut rights = vec![home,work,projection,runtime,etc,executable.fd];
        if let Some(script) = script { rights.push(script.fd); }
        rights.extend(inputs); rights.extend(stdin); rights.extend(mcp);
        let forwarder = forwarder.map(|(program, executable)| { rights.extend(egress); rights.push(executable); program });
        // HX launcher descriptors follow the forwarder; cgroup fds come at bind.
        let (launcher, control, helper_indices, launcher_rights) = match profile.launcher {
            Some(launcher) if duduclaw_run::linux::runner::LAUNCHER_SUPPORTED && !launcher.helpers.is_empty() => {
                let (spawnd_end, launcher_end) = checked(nix::sys::socket::socketpair(nix::sys::socket::AddressFamily::Unix,
                    nix::sys::socket::SockType::SeqPacket, None, nix::sys::socket::SockFlag::SOCK_CLOEXEC))?;
                let (plan, extra, indices) = launcher_plan(launcher, launcher_end)?;
                let count = extra.len();
                rights.extend(extra);
                (Some(plan), Some(spawnd_end), indices, count)
            }
            _ => (None, None, vec![], 0),
        };
        let helpers = if launcher.is_some() { HelperState::Available } else { HelperState::NotPlanned };
        let trusted = TrustedRun { root: RootConfig { employee_uid: plan.uid, employee_gid: plan.gid,
            pty: plan.pty, pty_limit: plan.max_ptys.max(1), work_noexec: true, tmp_noexec: true,
            tmpfs_bytes: profile.tmpfs_bytes, tmpfs_inodes: profile.tmpfs_inodes },
            mounts: identities.try_into().map_err(|_| unavailable())?, supervisor_uid: plan.supervisor_uid,
            supervisor_gid: plan.supervisor_gid, gateway_uid: profile.snapshot.gateway_uid,
            gateway_mount: profile.snapshot.mount_id, program, forwarder, prompt_bytes: plan.prompt_bytes,
            attachments: plan.input_fds, mcp: plan.mcp, proc_subset_pid: profile.proc_subset_pid, stdin: !plan.pty, pty_size: plan.pty_size,
            launcher };
        checked(trusted.validate())?;
        let pending_cgroup_fds = if trusted.launcher.is_some() { HELPER_CGROUP_FDS } else { 0 };
        if trusted.expected_fd_count() != rights.len() + pending_cgroup_fds { return Err(unavailable()); }
        let (socket, bootstrap_for_unit) = checked(nix::sys::socket::socketpair(nix::sys::socket::AddressFamily::Unix,
            nix::sys::socket::SockType::SeqPacket, None, nix::sys::socket::SockFlag::SOCK_CLOEXEC))?;
        self.runs.insert(plan.run.0, Pending { socket, manifest: Some((trusted,rights)), cancelled: None,
            stages: Stages::default(), prepared_at_ms: checked(crate::server::coordination_time_ms())?, terminal: None, pty: plan.pty, uid: plan.uid, master: None,
            control, helper_indices, launcher_rights, cgroups: None, helpers });
        Ok(PreparedBootstrap { bootstrap_for_unit })
    }
    fn bind_unit(&mut self, run: RunHandle, unit: &crate::systemd::linux::StartedUnit, cancelled: Arc<AtomicBool>) -> Result<()> {
        let pending = self.runs.get_mut(&run.0).ok_or(ErrorCode::NotFound)?;
        if pending.cancelled.is_some() || crate::systemd::parse_run_unit(&unit.name) != Some(run.0)
            || !unit.cgroup.starts_with("/sys/fs/cgroup") || cancelled.load(Ordering::Acquire) { return Err(unavailable()); }
        // StartedUnit can only be obtained from Systemd's checked start/adopt.
        // The monitor has installed this cgroup before releasing the B2 barrier.
        let challenge = checked(bootstrap::receive_challenge(pending.socket.as_raw_fd()))?;
        let (mut plan, mut rights) = pending.manifest.take().ok_or_else(unavailable)?;
        if plan.launcher.is_some() {
            // The waiting trampoline is the unit's only process: spawnd, as root
            // outside the unit's mount namespace, builds the HX cgroup leaves now,
            // before any employee-uid process exists (D8 revision §2.4 [F6]).
            match crate::helper_cgroup::linux::prepare_run(&unit.cgroup) {
                Ok((cgroups, fds)) => { rights.extend(fds); pending.cgroups = Some(cgroups); }
                Err(reason) => {
                    // Fail closed for helpers only; the run starts without a launcher.
                    let keep = rights.len().checked_sub(pending.launcher_rights).ok_or_else(unavailable)?;
                    rights.truncate(keep);
                    plan.launcher = None;
                    pending.control = None; pending.helper_indices.clear();
                    pending.helpers = HelperState::Unavailable(reason.as_reason());
                }
            }
        }
        if cancelled.load(Ordering::Acquire) { return Err(unavailable()); }
        let raw: Vec<_> = rights.iter().map(AsRawFd::as_raw_fd).collect();
        checked(sealed_registry::send(pending.socket.as_raw_fd(), challenge, run.0, plan, &raw))?;
        pending.cancelled = Some(cancelled);
        Ok(())
    }
    fn poll(&mut self, run: RunHandle) -> Result<HostProgress> {
        let occupied_mounts: Vec<_> = self.runs.values().filter_map(|pending|
            pending.master.as_ref().map(|master| master.mount_id)).collect();
        let pending = self.runs.get_mut(&run.0).ok_or(ErrorCode::NotFound)?;
        if pending.cancelled.as_ref().is_some_and(|flag| flag.load(Ordering::Acquire)) { return Err(unavailable()); }
        if let Some(outcome) = &pending.terminal { return Ok(HostProgress::Ended(outcome.clone())); }
        // Drain a bounded number; terminal receipts win over readiness in a batch.
        for _ in 0..8 {
            let mut poll = libc::pollfd { fd: pending.socket.as_raw_fd(), events: libc::POLLIN, revents: 0 };
            let count = unsafe { libc::poll(&mut poll, 1, 0) };
            if count < 0 { return Err(unavailable()); }
            if count == 0 { break; }
            if poll.revents & libc::POLLIN == 0 { return Err(unavailable()); }
            let (record, rights) = checked(bootstrap::receive_status(pending.socket.as_raw_fd()))?;
            if let StatusRecord::PtyIdentity { mount_id } = record {
                if !pending.pty || pending.master.is_some() || occupied_mounts.contains(&mount_id) { return Err(unavailable()); }
                pending.master = Some(crate::native_pty::ReturnedPty::validate(
                    rights.ok_or_else(unavailable)?, mount_id, pending.uid)?);
                continue;
            }
            if rights.is_some() { return Err(unavailable()); }
            if let Some(outcome) = pending.stages.record(record)? {
                pending.terminal = Some(outcome.clone());
                return Ok(HostProgress::Ended(outcome));
            }
        }
        // Non-PTY work has no idle timer. Do not fabricate activity each poll;
        // the receipt shares the coordinator clock and predates asynchronous polling.
        if pending.stages.ready() && pending.cancelled.is_some() && (!pending.pty || pending.master.is_some()) {
            Ok(HostProgress::Running { last_activity_ms: pending.prepared_at_ms })
        } else { Ok(HostProgress::Starting) }
    }
    fn resize(&mut self, run: RunHandle, size: PtySize) -> Result<()> {
        let pending = self.runs.get(&run.0).ok_or(ErrorCode::NotFound)?;
        if pending.terminal.is_some() || !pending.stages.ready() { return Err(ErrorCode::NotReady); }
        pending.master.as_ref().ok_or(ErrorCode::NotReady)?.resize(size)
    }
    fn take_pty(&mut self, run: RunHandle) -> Result<OwnedFd> {
        let pending = self.runs.get_mut(&run.0).ok_or(ErrorCode::NotFound)?;
        if !pending.stages.ready() { return Err(ErrorCode::NotReady); }
        pending.master.as_mut().ok_or(ErrorCode::NotReady)?.take()
    }
    fn close(&mut self, run: RunHandle) -> Result<()> { self.runs.remove(&run.0); Ok(()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn admission_requires_all_isolation_receipts_and_rejects_duplicates() {
        let mut stages = Stages::default();
        for record in [StatusRecord::SupervisorReady, StatusRecord::EmployeeReady { landlock_abi: 7 }, StatusRecord::IsolationReady] {
            assert!(stages.record(record).unwrap().is_none()); assert!(!stages.ready());
        }
        assert!(stages.record(StatusRecord::ChildrenForked).unwrap().is_none());
        assert!(stages.ready());
        assert!(stages.record(StatusRecord::ChildrenForked).is_err());
        assert!(stages.record(StatusRecord::Error(1)).is_err());
    }
    #[test]
    fn launcher_drain_window_is_grace_minus_two_seconds_bounded() {
        assert_eq!(launcher_drain_ms(15_000), 13_000);
        assert_eq!(launcher_drain_ms(crate::helper_policy::RECORDING_MIN_GRACE_MS), 10_000);
        for short in [0, 100, 2_000, 2_999] { assert_eq!(launcher_drain_ms(short), 1_000); }
        assert_eq!(launcher_drain_ms(u64::MAX), duduclaw_run::linux::runner::MAX_LAUNCHER_DRAIN_MS);
        assert!(duduclaw_run::linux::runner::LAUNCHER_SUPPORTED, "HX3b: the trampoline forks the launcher");
    }
    #[test]
    fn production_default_never_invents_installation_evidence() {
        assert!(NativeTrampoline::default().readiness().is_err());
    }
}
