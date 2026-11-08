//! Single execution entry point and CI effect inventory (design §4.1–§4.5).
use std::{collections::BTreeMap, future::Future, pin::Pin};
use serde::{Deserialize, Serialize};
use crate::{bounded::Text, egress::ProfileId, ids::{EmployeeId, RunId}, ContractError};

/// Object-safe asynchronous result without choosing an executor (§4.1).
pub type BackendFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, BackendError>> + Send + 'a>>;

/// Work classification; every employee-uid class shares admission (§4.1, §4.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionClass {
    EmployeeMain, Scheduled, Dispatch, Heartbeat, Mail, Reminder,
    Judge, Fork, Discovery, EmployeeTool, ThirdPartyMcp, ToolLoopMcp,
    ParseWorker, ImportWorker, InstallationRequester, AccountProbe,
    ModelProbe, InteractiveLogin,
}

/// Runtime family, independent of a model vendor (§4.1).
/// `Registered` is a root-registry identifier, never an executable path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "name", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuntimeKind { Claude, Codex, Grok, OpenAiCompat, Registered(Text<64>) }

/// A root-selected network policy, not part of gateway→spawnd IPC (§4.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkPolicy { Offline, Egress(ProfileId) }

/// Mandatory hard disk/inode quota; there is no unbounded option (§4.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiskQuota {
    LoopFilesystem { bytes: u64, inodes: u64 },
    ProjectQuota { bytes: u64, inodes: u64 },
}

/// Per-run resource ceilings; hierarchy-wide reservation is enforced by B/H (§4.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceLimits {
    pub memory_bytes: u64,
    /// 100 means one CPU; integer percent, never a relative scheduling weight.
    pub cpu_quota_percent: u32,
    pub tasks: u32,
    pub open_files: u32,
    pub ptys: u16,
    pub disk: DiskQuota,
    pub io_read_bytes_per_sec: u64,
    pub io_write_bytes_per_sec: u64,
    pub io_read_ops_per_sec: u64,
    pub io_write_ops_per_sec: u64,
    pub output_buffer_bytes: u32,
    pub output_total_bytes: u64,
    pub memfd_total_bytes: u64,
}

/// Deadlines in milliseconds, enforced by the host (§4.2, §4.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunTimeouts {
    pub queue_ms: u64,
    pub startup_ms: u64,
    pub runtime_ms: u64,
    pub pty_idle_ms: Option<u64>,
    pub termination_grace_ms: u64,
}

/// Server-owned workspace identity and relative cwd (§5, §6.3).
/// Implementations resolve via registry dirfds; these are not host paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspacePolicy {
    pub workspace: Text<128>,
    pub relative_cwd: Text<4096>,
}

/// Owned host proof retained until root has validated and mounted the same H directory.
/// Never deserialized; only a trusted resource authorization path should construct it.
pub struct WorkspaceDelivery {
    pub employee: EmployeeId,
    pub run: RunId,
    pub policy: WorkspacePolicy,
    pub directory: std::fs::File,
}
impl WorkspaceDelivery {
    pub fn selection(&self) -> Result<crate::ipc::WorkspaceSelection, BackendError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let stat = self.directory.metadata().map_err(|_| BackendError::Io)?;
            if !stat.is_dir() { return Err(BackendError::Denied); }
            let value = crate::ipc::WorkspaceSelection { handle: self.policy.workspace.clone(),
                relative_cwd: self.policy.relative_cwd.clone(), device: stat.dev(), inode: stat.ino() };
            value.validate().map_err(|_| BackendError::Denied)?;
            Ok(value)
        }
        #[cfg(not(unix))]
        { Err(BackendError::Unavailable) }
    }
}

/// Connection capabilities requested by internal policy (§4 fd matrix, §6.1).
/// No capability secrets travel in argv/env. Egress belongs to the forwarder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityHandover {
    pub mcp_bridge: bool,
    pub egress_forwarder: bool,
}

/// Trusted, in-process spawn policy (§4.1), deliberately without serde.
/// A validates this policy; B re-derives all privileged choices from root policy.
/// Argv is tokenized, environment is rebuilt from the allowlist, and these fields
/// are never copied into spawnd StartRun. Input content travels separately by fd.
pub struct SpawnPolicy {
    pub run: RunHandle,
    pub class: ExecutionClass,
    pub employee: EmployeeId,
    pub runtime: RuntimeKind,
    pub argv_allowlist: Vec<Text<4096>>,
    pub environment_allowlist: BTreeMap<String, Text<4096>>,
    pub workspace: WorkspacePolicy,
    pub workspace_delivery: Option<WorkspaceDelivery>,
    pub resources: ResourceLimits,
    pub network: NetworkPolicy,
    pub pty: bool,
    pub capabilities: CapabilityHandover,
    pub timeouts: RunTimeouts,
    pub inputs: RunInputs,
}

/// Owned input stream plus a hard byte budget (§4 fd matrix). The backend copies
/// prompt/small attachments into sealed memfds and large attachments into private
/// read-only snapshots before IPC. A stream cannot specify a path or mount source.
pub struct RunInput {
    pub reader: Box<dyn RunReader>,
    pub max_bytes: u64,
}

/// Input handover for spawn (§4.1). Enforce ipc's prompt, count and aggregate
/// limits while reading; reject overflow, never truncate a prompt or attachment.
pub struct RunInputs {
    pub prompt: RunInput,
    pub attachments: Vec<RunInput>,
}

/// Opaque server-issued handle; possession alone is not authorization (§4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunHandle(pub RunId);

/// Host-derived waiter identity, never supplied by employee tool arguments (§4.5).
/// The backend resolves both handles through its registry and refuses a synchronous
/// wait when they belong to the same employee. Gateway waits have no active run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitOrigin { Gateway, Run(RunHandle) }

/// Host-observed terminal reason; never contains paths or command output (§4.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunOutcome {
    Exited(i32), Signalled(u32), Cancelled, QueueTimeout, RuntimeTimeout,
    IdleTimeout, Rejected, IsolationFailure,
}

/// Bounded, caller-safe execution failures (§4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BackendError {
    #[error("execution unavailable")] Unavailable,
    #[error("execution denied")] Denied,
    #[error("same-employee wait must remain asynchronous")] WouldDeadlock,
    #[error("run not found")] NotFound,
    #[error("operation timed out")] Timeout,
    #[error("execution I/O failed")] Io,
    #[error("execution protocol failed")] Protocol,
}

/// Portable asynchronous byte reader owned by the backend (§4.1, §4.4).
pub trait RunReader: Send {
    /// Bounded reads; zero means EOF. Implementations enforce output budgets.
    fn read<'a>(&'a mut self, buf: &'a mut [u8]) -> BackendFuture<'a, usize>;
}

/// Portable asynchronous byte writer owned by the backend (§4.1).
pub trait RunWriter: Send {
    /// Write at most the supplied bytes with backpressure.
    fn write<'a>(&'a mut self, buf: &'a [u8]) -> BackendFuture<'a, usize>;
    /// Close the input side.
    fn close(&mut self) -> BackendFuture<'_, ()>;
}

/// Pipe access returned once; the backend retains lifecycle authority (§4.1).
pub struct RunStdio {
    pub stdin: Box<dyn RunWriter>,
    pub stdout: Box<dyn RunReader>,
    pub stderr: Box<dyn RunReader>,
}

/// PTY access returned once, never a PID or serialized fd number (§4, §4.1).
pub struct RunPty {
    pub input: Box<dyn RunWriter>,
    pub output: Box<dyn RunReader>,
}

/// Nonzero terminal dimensions (§4 Resize).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PtySize { pub rows: u16, pub columns: u16 }
impl PtySize {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.rows == 0 || self.columns == 0 { return Err(ContractError::InvalidValue("pty size")); }
        Ok(())
    }
}

/// Common entry point (§4.1). Boxed futures keep this trait object-safe, enabling
/// `dyn ExecutionBackend` without async-trait/Tokio dependencies. Implementations
/// must reject unsupported isolation, never spawn under the gateway uid as fallback.
/// `spawn` returns queued handles immediately. `wait` on one's own active employee
/// is refused/deferred to avoid §4.5 self-deadlock. Cancellation kills the entire
/// cgroup and revokes capabilities; completion is not proof that admission is free.
pub trait ExecutionBackend: Send + Sync {
    /// Reserve an expiring server-issued id so D can mint the bound capability
    /// before spawn. Unused reservations expire and consume bounded queue space.
    fn reserve(&self, employee: EmployeeId) -> BackendFuture<'_, RunHandle>;
    /// Consume policy.run's reservation and return the same handle, even queued.
    fn spawn(&self, policy: SpawnPolicy) -> BackendFuture<'_, RunHandle>;
    /// Protected tool execution needs the selected workspace, not just a handle
    /// that a backend might reinterpret. Unsupported backends stay unavailable.
    fn supports_authorized_workspace(&self) -> bool { false }
    /// Implementations must retain the actual root used for execution, compare
    /// it with `authority.verify_workspace`, and recheck live authority after
    /// admission/input waits immediately before handing work to the host. They
    /// must never reopen or replace the checked root between check and use.
    fn spawn_authorized(&self, _policy: SpawnPolicy, _authority: std::sync::Arc<dyn ExecutionAuthority>)
        -> BackendFuture<'_, RunHandle> { Box::pin(async { Err(BackendError::Denied) }) }
    fn wait(&self, run: RunHandle, origin: WaitOrigin) -> BackendFuture<'_, RunOutcome>;
    fn kill(&self, run: RunHandle) -> BackendFuture<'_, ()>;
    /// Employee-requested cancellation must recheck the target and live authority
    /// after all admission/transport waits, immediately before sending Cancel.
    /// Host cleanup keeps using `kill`, including cleanup after revocation.
    fn kill_authorized(&self, _run: RunHandle, _authority: std::sync::Arc<dyn CancelAuthority>)
        -> BackendFuture<'_, ()> { Box::pin(async { Err(BackendError::Denied) }) }
    fn take_stdio(&self, run: RunHandle) -> BackendFuture<'_, RunStdio>;
    fn take_pty(&self, run: RunHandle) -> BackendFuture<'_, RunPty>;
    fn resize(&self, run: RunHandle, size: PtySize) -> BackendFuture<'_, ()>;

    /// HX5 (§3 step 4): whether this backend implements in-run helper calls at
    /// all. A per-run feature check (`helper_features`) still gates every call.
    fn supports_helper_calls(&self) -> bool { false }
    /// [F17] Feature bits spawnd advertised on the control connection that
    /// reserved `run` (`SpawnResponse::Reserved.features`). Unknown run: none.
    fn helper_features(&self, _run: RunHandle) -> crate::ipc::SpawndFeatures { crate::ipc::SpawndFeatures::NONE }
    /// Start one helper inside the live run `plan.run` (the caller's own run),
    /// on that run's own control connection. Never reserves or spawns a run.
    fn start_helper(&self, _plan: HelperPlan) -> HelperFuture<'_, HelperStart> {
        Box::pin(async { Err(HelperError::Unavailable) })
    }
    /// Root-determined end of a started call (polls `HelperWait`).
    fn wait_helper(&self, _run: RunHandle, _call: crate::ipc::HelperCallId)
        -> HelperFuture<'_, crate::ipc::HelperOutcome> {
        Box::pin(async { Err(HelperError::Unavailable) })
    }
    /// Kill the call's slot cgroup (queued or running). Never cancels the run [F8].
    fn cancel_helper(&self, _run: RunHandle, _call: crate::ipc::HelperCallId) -> HelperFuture<'_, ()> {
        Box::pin(async { Err(HelperError::Unavailable) })
    }
    /// HX6: ask a recording-slot call to flush and exit (`HelperStop`: the
    /// launcher writes `stop\n` to its stdin). `Ok` is root's acceptance only;
    /// the end is `wait_helper`'s `HelperEnded`. Never stops the run [F8].
    fn stop_helper(&self, _run: RunHandle, _call: crate::ipc::HelperCallId) -> HelperFuture<'_, ()> {
        Box::pin(async { Err(HelperError::Unavailable) })
    }
}

/// HX5 helper-call future; errors are typed per-call results (§5).
pub type HelperFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, HelperError>> + Send + 'a>>;

/// One in-run helper call (HX §3). Built only by the boundary from a verified
/// capability: `run` is the caller's own run and `employee` its employee. The
/// helper is a lookup key; spawnd re-derives it from the run's employee.
pub struct HelperPlan {
    pub run: RunHandle,
    pub employee: EmployeeId,
    pub helper: RuntimeKind,
    pub slot: crate::ipc::HelperSlot,
    /// The tool's own bound; spawnd answers with the effective value.
    pub timeout_ms: u64,
    /// Sealed into a memfd by the backend (`MAX_HELPER_INPUT_BYTES`).
    pub input: Vec<u8>,
    /// Combined stdout + stderr budget of the returned readers.
    pub output_limit: u64,
}

/// Reply to a started helper call: the spawnd call id and the call's own pipes.
pub struct HelperStart {
    pub call: crate::ipc::HelperCallId,
    pub effective_timeout_ms: u64,
    pub stdout: Box<dyn RunReader>,
    pub stderr: Box<dyn RunReader>,
}

/// Typed per-call refusals (HX §5 readiness/per-call results). No paths or content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HelperError {
    /// spawnd did not advertise `helper_calls` [F17]; nothing was sent.
    #[error("helper launcher unavailable")] FeatureMissing,
    #[error("helper busy")] Busy,
    #[error("run time exhausted")] RunTimeExhausted,
    #[error("helper unavailable")] Unavailable,
    #[error("helper cgroup unavailable")] CgroupUnavailable,
    /// Unknown run, not Running, or not this employee's run.
    #[error("run not live")] NotLive,
    #[error("helper call denied")] Denied,
    /// HX6: root has no such helper in this run's signed table (spawnd
    /// `runtime_denied`). For the recording this is the offline parent: an
    /// egress binding exists only under an Egress parent (publication check 4).
    #[error("helper absent from the run")] Absent,
    /// A request may have reached root; the outcome is unknown.
    #[error("helper call uncertain")] Uncertain,
}
impl HelperError {
    /// Stable result/readiness code.
    pub fn code(self) -> &'static str {
        match self {
            Self::FeatureMissing => "helper_launcher_unavailable",
            Self::Busy => "helper_busy",
            Self::RunTimeExhausted => "run_time_exhausted",
            Self::Unavailable => "helper_unavailable",
            Self::CgroupUnavailable => "helper_cgroup_unavailable",
            Self::NotLive => "run_ended",
            Self::Denied => "refused",
            Self::Absent => "helper_runtime_missing",
            Self::Uncertain => "uncertain",
        }
    }
}

/// An in-process control token, bound to an already ownership-checked job.
/// No serialized handle or employee input can construct this authority.
pub trait CancelAuthority: Send + Sync {
    fn verify_cancel(&self, employee: EmployeeId, run: RunHandle) -> Result<(), BackendError>;
}

/// Boundary-owned selection token passed intact to a protected tool backend.
/// A backend validates its real execution root against this authority; an
/// implementation that only compares workspace names does not satisfy it.
pub trait ExecutionAuthority: Send + Sync {
    fn verify_live(&self) -> Result<(), BackendError>;
    fn verify_workspace(&self, handle: &str, directory: &std::fs::File) -> Result<(), BackendError>;
}

/// Effects that require an explicit CI inventory entry (§4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectClass { FileRead, FileWrite, ContentParse, ProcessSpawn, Network, Connector, SideEffect, PrivilegedIpc }

/// Effective identity of an inventoried function (§3, §4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionIdentity { Gateway, Employee, ParseWorker, ImportWorker, SpawnDaemon, Trampoline, Egress, Updater, Downloader }

/// Function-level CI declaration; gateway exceptions require a rationale (§4.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivilegedEffect {
    pub symbol: String,
    pub effects: Vec<EffectClass>,
    pub identity: ExecutionIdentity,
    pub rationale: String,
}

/// Exact-name inventory. CI must compare discovered effects against this registry;
/// defining an entry does not scan source code or grant runtime authority (§4.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PrivilegedEffectRegistry(pub Vec<PrivilegedEffect>);
impl PrivilegedEffectRegistry {
    pub fn validate(&self) -> Result<(), ContractError> {
        let mut seen = std::collections::BTreeSet::new();
        for entry in &self.0 {
            if entry.symbol.is_empty() || entry.effects.is_empty() || entry.rationale.trim().is_empty()
                || !seen.insert(&entry.symbol) {
                return Err(ContractError::InvalidValue("effect registry"));
            }
        }
        Ok(())
    }
    pub fn lookup(&self, symbol: &str) -> Option<&PrivilegedEffect> {
        self.0.iter().find(|entry| entry.symbol == symbol)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn backend_is_object_safe_and_registry_lookup_is_exact() {
        fn accepts_dyn(_: Option<&dyn ExecutionBackend>) {}
        accepts_dyn(None);
        let registry = PrivilegedEffectRegistry(vec![PrivilegedEffect {
            symbol: "probe::spawn".into(), effects: vec![EffectClass::ProcessSpawn],
            identity: ExecutionIdentity::Gateway, rationale: "Registered account probe".into(),
        }]);
        assert!(registry.validate().is_ok());
        assert!(registry.lookup("probe::spawn").is_some());
        assert!(registry.lookup("evil::probe::spawn").is_none());
        let duplicate = PrivilegedEffectRegistry(vec![registry.0[0].clone(), registry.0[0].clone()]);
        assert!(duplicate.validate().is_err());
    }
}
