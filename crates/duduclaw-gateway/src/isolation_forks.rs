//! Root-published fork snapshot plans and bounded D8-aware result collection.
//! This is a real helper producer, not authority to start a model branch: that
//! branch still needs the normal fresh D capability/turn/account/budget path.
use std::{sync::Arc, time::Duration};
use duduclaw_execution::fork_helper::{self, Request};
use duduclaw_isolation::{bounded::Text, execution::*, ids::Sha256Digest};
use duduclaw_mcp_boundary::{RequestContext, VerificationError, resources::AuthorizedWorkspace};
use duduclaw_spawnd::{installed::GatewayPublication, policy::PublishedRuntime};

pub struct InstalledForkSnapshots { generation: Sha256Digest, gateway_gid: u32 }
impl InstalledForkSnapshots {
    pub fn new(generation: Sha256Digest, gateway_gid: u32) -> Result<Arc<Self>, VerificationError> {
        if generation.0 == [0; 32] || gateway_gid == 0 { return Err(VerificationError::Invalid); }
        let factory = Arc::new(Self { generation, gateway_gid });
        factory.publication()?;
        Ok(factory)
    }
    fn publication(&self) -> Result<GatewayPublication, VerificationError> {
        let publication = GatewayPublication::read(self.gateway_gid).map_err(|_| VerificationError::Unavailable)?;
        if publication.generation != self.generation { return Err(VerificationError::BindingMismatch); }
        if !publication.runtimes.iter().any(|runtime| valid_runtime(&runtime.to_runtime())) {
            return Err(VerificationError::Unavailable);
        }
        Ok(publication)
    }

    /// The proof is captured by the host from its retained ResourceRoots and
    /// bound to this exact verified caller, never reconstructed from MCP paths.
    pub fn prepare(&self, context: &RequestContext, run: RunHandle, request: &Request,
        workspace: &AuthorizedWorkspace) -> Result<SpawnPolicy, VerificationError> {
        if !duduclaw_mcp_boundary::tool_policy::eligible(context, "fork_run")
            || run.0 == context.record().run { return Err(VerificationError::Invalid); }
        let policy = WorkspacePolicy { workspace: Text::new("employee-work".into()).map_err(|_| VerificationError::Invalid)?,
            relative_cwd: Text::new(".".into()).map_err(|_| VerificationError::Invalid)? };
        if !workspace.permits(context, &policy) { return Err(VerificationError::BindingMismatch); }
        let bytes = request.encode().map_err(|_| VerificationError::Invalid)?;
        let publication = self.publication()?;
        let root = publication.policy().map_err(|_| VerificationError::Invalid)?;
        let kind = RuntimeKind::Registered(Text::new(fork_helper::RUNTIME.into()).map_err(|_| VerificationError::Invalid)?);
        let (runtime, _) = root.derive(context.record().employee, &kind, false).map_err(|_| VerificationError::Invalid)?;
        if !valid_runtime(runtime) || bytes.len() as u64 > runtime.resources.memfd_total_bytes {
            return Err(VerificationError::Invalid);
        }
        Ok(SpawnPolicy { run, employee: context.record().employee, class: ExecutionClass::Fork,
            runtime: kind, argv_allowlist: runtime.fixed_args.clone(), environment_allowlist: Default::default(),
            workspace: policy, workspace_delivery: Some(workspace.delivery().map_err(|_| VerificationError::Invalid)?),
            resources: runtime.resources.clone(), network: NetworkPolicy::Offline, pty: false,
            capabilities: CapabilityHandover { mcp_bridge: false, egress_forwarder: false },
            timeouts: runtime.timeouts.clone(), inputs: RunInputs { prompt: RunInput {
                max_bytes: bytes.len() as u64, reader: Box::new(Input(std::io::Cursor::new(bytes))) }, attachments: vec![] } })
    }

    /// Call only from a detached host orchestrator, after responding to the
    /// employee. D8 queues the helper behind the caller; waiting in its tool
    /// handler would keep that caller's employee slot occupied indefinitely.
    pub async fn run(self: &Arc<Self>, backend: Arc<dyn ExecutionBackend>, plan: SpawnPolicy,
        expected_id: &str) -> Result<serde_json::Value, VerificationError> {
        if !valid_plan(&plan) { return Err(VerificationError::Invalid); }
        let run = plan.run;
        let timeout = plan.timeouts.queue_ms.saturating_add(plan.timeouts.startup_ms)
            .saturating_add(plan.timeouts.runtime_ms).saturating_add(10_000).min(3_600_000);
        let mut guard = Cleanup { backend: backend.clone(), run, armed: true };
        if backend.spawn(plan).await.map_err(|_| VerificationError::Unavailable)? != run {
            return Err(VerificationError::BindingMismatch);
        }
        let observed = tokio::time::timeout(Duration::from_millis(timeout), async {
            let mut io = loop {
                match backend.take_stdio(run).await {
                    Ok(io) => break io,
                    Err(BackendError::Unavailable) => tokio::time::sleep(Duration::from_millis(100)).await,
                    Err(_) => return Err(VerificationError::Unavailable),
                }
            };
            io.stdin.close().await.map_err(|_| VerificationError::Unavailable)?;
            let (stdout, stderr, outcome) = tokio::try_join!(
                read(io.stdout, fork_helper::MAX_RESULT), read(io.stderr, 8192),
                async { backend.wait(run, WaitOrigin::Gateway).await.map_err(|_| VerificationError::Unavailable) })?;
            if !stderr.is_empty() || outcome != RunOutcome::Exited(0) { return Err(VerificationError::Invalid); }
            let value = fork_helper::decode_result(&stdout).map_err(|_| VerificationError::Invalid)?;
            if value["id"].as_str() != Some(expected_id) { return Err(VerificationError::BindingMismatch); }
            Ok(value)
        }).await.map_err(|_| VerificationError::Unavailable)??;
        guard.armed = false;
        Ok(observed)
    }
}

fn valid_runtime(runtime: &PublishedRuntime) -> bool {
    matches!(&runtime.runtime, RuntimeKind::Registered(name) if name.as_str() == fork_helper::RUNTIME)
        && runtime.fixed_args.iter().map(|arg| arg.as_str()).eq([fork_helper::ARGUMENT])
        && !runtime.mcp && !runtime.pty_allowed && matches!(runtime.network, NetworkPolicy::Offline)
        && runtime.resources.output_total_bytes >= fork_helper::MAX_RESULT as u64
        && runtime.resources.memfd_total_bytes >= fork_helper::MAX_REQUEST as u64
        && (1..=305_000).contains(&runtime.timeouts.runtime_ms)
}
fn valid_plan(plan: &SpawnPolicy) -> bool {
    plan.class == ExecutionClass::Fork && !plan.pty && !plan.capabilities.mcp_bridge
        && !plan.capabilities.egress_forwarder && matches!(plan.network, NetworkPolicy::Offline)
        && matches!(&plan.runtime, RuntimeKind::Registered(name) if name.as_str() == fork_helper::RUNTIME)
        && plan.workspace.workspace.as_str() == "employee-work" && plan.workspace.relative_cwd.as_str() == "."
        && plan.workspace_delivery.is_some()
}
struct Input(std::io::Cursor<Vec<u8>>);
impl RunReader for Input {
    fn read<'a>(&'a mut self, output: &'a mut [u8]) -> BackendFuture<'a, usize> {
        Box::pin(async move { std::io::Read::read(&mut self.0, output).map_err(|_| BackendError::Io) })
    }
}
async fn read(mut stream: Box<dyn RunReader>, limit: usize) -> Result<Vec<u8>, VerificationError> {
    let mut bytes = vec![];
    let mut buffer = [0; 8192];
    loop {
        let count = stream.read(&mut buffer).await.map_err(|_| VerificationError::Unavailable)?;
        if count == 0 { return Ok(bytes); }
        if count > buffer.len() || bytes.len().saturating_add(count) > limit { return Err(VerificationError::Invalid); }
        bytes.extend_from_slice(&buffer[..count]);
    }
}
struct Cleanup { backend: Arc<dyn ExecutionBackend>, run: RunHandle, armed: bool }
impl Drop for Cleanup {
    fn drop(&mut self) {
        if self.armed && let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let (backend, run) = (self.backend.clone(), self.run);
            runtime.spawn(async move { let _ = tokio::time::timeout(Duration::from_secs(5), backend.kill(run)).await; });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_isolation::{egress::ProfileId, ipc::QueueClass};
    use duduclaw_spawnd::policy::Executable;
    fn runtime() -> PublishedRuntime {
        PublishedRuntime { runtime: RuntimeKind::Registered(Text::new(fork_helper::RUNTIME.into()).unwrap()),
            profile: ProfileId(Sha256Digest([1; 32])),
            executable: Executable::Elf { entry: Text::new("duduclaw-fork-snapshot-helper".into()).unwrap(), digest: Sha256Digest([2; 32]) },
            pinned_version: Text::new("fixture".into()).unwrap(),
            fixed_args: vec![Text::new(fork_helper::ARGUMENT.into()).unwrap()],
            resources: ResourceLimits { memory_bytes: 1024*1024, cpu_quota_percent: 100, tasks: 16, open_files: 4096,
                ptys: 1, disk: DiskQuota::LoopFilesystem { bytes: 1024, inodes: 32 },
                io_read_bytes_per_sec: 1024, io_write_bytes_per_sec: 1024, io_read_ops_per_sec: 10,
                io_write_ops_per_sec: 10, output_buffer_bytes: 4096,
                output_total_bytes: fork_helper::MAX_RESULT as u64, memfd_total_bytes: fork_helper::MAX_REQUEST as u64 },
            timeouts: RunTimeouts { queue_ms: 1000, startup_ms: 1000, runtime_ms: 60_000,
                pty_idle_ms: None, termination_grace_ms: 100 },
            network: NetworkPolicy::Offline, mcp: false, pty_allowed: false, queue_class: QueueClass::Background }
    }
    #[test]
    fn fork_snapshot_plans_require_exact_offline_bounded_protocol() {
        let good = runtime();
        assert!(valid_runtime(&good));
        let mut changed = good.clone(); changed.mcp = true; assert!(!valid_runtime(&changed));
        let mut changed = good.clone(); changed.pty_allowed = true; assert!(!valid_runtime(&changed));
        let mut changed = good.clone(); changed.network = NetworkPolicy::Egress(ProfileId(Sha256Digest([3; 32])));
        assert!(!valid_runtime(&changed));
        let mut changed = good.clone(); changed.fixed_args.push(Text::new("--path=/host".into()).unwrap());
        assert!(!valid_runtime(&changed));
        let mut changed = good.clone(); changed.resources.output_total_bytes -= 1; assert!(!valid_runtime(&changed));
        let mut changed = good.clone(); changed.resources.memfd_total_bytes -= 1; assert!(!valid_runtime(&changed));
        let mut changed = good.clone(); changed.timeouts.runtime_ms = 0; assert!(!valid_runtime(&changed));
        let mut changed = good; changed.timeouts.runtime_ms = 305_001; assert!(!valid_runtime(&changed));
    }
    struct Bytes(std::io::Cursor<Vec<u8>>);
    impl RunReader for Bytes {
        fn read<'a>(&'a mut self, out: &'a mut [u8]) -> BackendFuture<'a, usize> {
            Box::pin(async move { std::io::Read::read(&mut self.0, out).map_err(|_| BackendError::Io) })
        }
    }
    #[tokio::test]
    async fn fork_snapshot_collector_rejects_oversize_and_preserves_exact_bound() {
        assert_eq!(read(Box::new(Bytes(std::io::Cursor::new(vec![1; 8192]))), 8192).await.unwrap().len(), 8192);
        assert!(read(Box::new(Bytes(std::io::Cursor::new(vec![1; 8193]))), 8192).await.is_err());
    }
}
