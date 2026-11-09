//! Gateway-side main-run plans from the fresh root publication. This is not D's
//! ExecutionPlanFactory (which authorizes MCP-triggered helper operations).
use duduclaw_isolation::{bounded::Text, execution::*, ids::{EmployeeId, RunId, Sha256Digest}};
use crate::policy::RootPolicy;

/// Immutable generation pin from composite boot readiness. Every invocation
/// rereads root policy; a cached entitlement can never authorize a later run.
pub struct InstalledMainPlanFactory { generation: Sha256Digest, gateway_gid: u32 }
impl InstalledMainPlanFactory {
    pub fn new(generation: Sha256Digest, gateway_gid: u32) -> Result<Self, BackendError> {
        if generation.0 == [0; 32] || gateway_gid == 0 { return Err(BackendError::Denied); }
        Ok(Self { generation, gateway_gid })
    }

    #[cfg(target_os = "linux")]
    pub fn claude(&self, employee: EmployeeId, class: ExecutionClass,
        prompt: Vec<u8>, system_prompt: Vec<u8>, model: &str) -> Result<SpawnPolicy, BackendError> {
        let publication = crate::installed::GatewayPublication::read(self.gateway_gid)
            .map_err(|_| BackendError::Unavailable)?;
        if publication.generation != self.generation { return Err(BackendError::Denied); }
        let policy = publication.policy().map_err(|_| BackendError::Denied)?;
        claude_plan(&policy, employee, class, prompt, system_prompt, model)
    }

    #[cfg(target_os = "linux")]
    pub fn other_runtime(&self, employee: EmployeeId, class: ExecutionClass,
        runtime: RuntimeKind, request: Vec<u8>) -> Result<SpawnPolicy, BackendError> {
        let publication = crate::installed::GatewayPublication::read(self.gateway_gid)
            .map_err(|_| BackendError::Unavailable)?;
        if publication.generation != self.generation { return Err(BackendError::Denied); }
        let policy = publication.policy().map_err(|_| BackendError::Denied)?;
        other_runtime_plan(&policy, employee, class, runtime, request)
    }
}

fn other_runtime_plan(policy: &RootPolicy, employee: EmployeeId, class: ExecutionClass,
    kind: RuntimeKind, request: Vec<u8>) -> Result<SpawnPolicy, BackendError> {
    if !matches!(class, ExecutionClass::EmployeeMain | ExecutionClass::Scheduled | ExecutionClass::Dispatch
        | ExecutionClass::Heartbeat | ExecutionClass::Mail | ExecutionClass::Reminder)
        || request.is_empty() || request.len() > duduclaw_isolation::ipc::MAX_PROMPT_BYTES as usize {
        return Err(BackendError::Denied);
    }
    let expected = match kind {
        RuntimeKind::OpenAiCompat => duduclaw_mcp_boundary::config::api_worker_arguments(),
        RuntimeKind::Codex | RuntimeKind::Grok => duduclaw_mcp_boundary::config::native_cli_arguments(&kind)
            .map_err(|_| BackendError::Denied)?,
        _ => duduclaw_mcp_boundary::config::registered_cli_arguments(&kind).map_err(|_| BackendError::Denied)?,
    };
    let pty = kind == RuntimeKind::Grok;
    let (runtime, _) = policy.derive(employee, &kind, pty).map_err(|_| BackendError::Denied)?;
    if !runtime.mcp || runtime.fixed_args.iter().map(|arg| arg.as_str()).ne(expected.iter().map(String::as_str))
        || request.len() as u64 > runtime.resources.memfd_total_bytes {
        return Err(BackendError::Denied);
    }
    // D adds the complete fixed protocol suffix from the same projection.
    Ok(SpawnPolicy { workspace_delivery: None, run: RunHandle(RunId([0; 16])), employee, class, runtime: kind,
        argv_allowlist: vec![], environment_allowlist: Default::default(),
        workspace: WorkspacePolicy { workspace: Text::new("employee-work".into()).map_err(|_| BackendError::Denied)?,
            relative_cwd: Text::new(String::new()).map_err(|_| BackendError::Denied)? },
        resources: runtime.resources.clone(), network: runtime.network.clone(), pty,
        capabilities: CapabilityHandover { mcp_bridge: true,
            egress_forwarder: matches!(runtime.network, NetworkPolicy::Egress(_)) },
        timeouts: runtime.timeouts.clone(), inputs: RunInputs { prompt: input(request), attachments: vec![] } })
}

/// The signed runtime must already contain this exact input protocol. These
/// arguments are never sent to root as authority. fd 4 is the sealed prompt;
/// Claude consumes the same bytes on stdin, and fd 5 is the system prompt.
pub fn claude_arguments(model: &str) -> Result<Vec<String>, BackendError> {
    if model.len() > 256 || model.chars().any(char::is_control) || model.trim() != model {
        return Err(BackendError::Denied);
    }
    let mut args: Vec<String> = ["-p", "--input-format", "text", "--output-format", "stream-json",
        "--verbose", "--system-prompt-file", "/proc/self/fd/5", "--tools", "", "--allowedTools", "mcp__duduclaw__*",
        "--max-turns", "50"].into_iter().map(str::to_owned).collect();
    if !model.is_empty() { args.extend(["--model".into(), model.into()]); }
    args.extend(["--strict-mcp-config".into(), "--mcp-config".into(), "/config/mcp.json".into()]);
    Ok(args)
}

fn claude_plan(policy: &RootPolicy, employee: EmployeeId, class: ExecutionClass,
    prompt: Vec<u8>, system_prompt: Vec<u8>, model: &str) -> Result<SpawnPolicy, BackendError> {
    if !matches!(class, ExecutionClass::EmployeeMain | ExecutionClass::Scheduled | ExecutionClass::Dispatch
        | ExecutionClass::Heartbeat | ExecutionClass::Mail | ExecutionClass::Reminder)
        || prompt.is_empty() || prompt.len() > duduclaw_isolation::ipc::MAX_PROMPT_BYTES as usize
        || system_prompt.len() > duduclaw_isolation::ipc::MAX_PROMPT_BYTES as usize {
        return Err(BackendError::Denied);
    }
    let (runtime, _) = policy.derive(employee, &RuntimeKind::Claude, false).map_err(|_| BackendError::Denied)?;
    let expected = claude_arguments(model)?;
    if !runtime.mcp || runtime.fixed_args.iter().map(|arg| arg.as_str()).ne(expected.iter().map(String::as_str))
        || (prompt.len() as u64).checked_add(system_prompt.len() as u64)
            .is_none_or(|total| total > runtime.resources.memfd_total_bytes) {
        return Err(BackendError::Denied);
    }
    // D adds its own three fixed MCP arguments after reservation. Do not add
    // them twice or copy anything from the legacy native Command builder.
    let mut argv_allowlist = runtime.fixed_args.clone();
    argv_allowlist.truncate(argv_allowlist.len() - 3);
    Ok(SpawnPolicy { workspace_delivery: None, run: RunHandle(RunId([0; 16])), employee, class, runtime: RuntimeKind::Claude,
        argv_allowlist, environment_allowlist: Default::default(),
        workspace: WorkspacePolicy { workspace: Text::new("employee-work".into()).map_err(|_| BackendError::Denied)?,
            relative_cwd: Text::new(String::new()).map_err(|_| BackendError::Denied)? },
        resources: runtime.resources.clone(), network: runtime.network.clone(), pty: false,
        capabilities: CapabilityHandover { mcp_bridge: true,
            egress_forwarder: matches!(runtime.network, NetworkPolicy::Egress(_)) },
        timeouts: runtime.timeouts.clone(), inputs: RunInputs {
            prompt: input(prompt), attachments: vec![input(system_prompt)] } })
}

struct Bytes { bytes: Vec<u8>, offset: usize }
impl RunReader for Bytes {
    fn read<'a>(&'a mut self, out: &'a mut [u8]) -> BackendFuture<'a, usize> {
        Box::pin(async move {
            let count = out.len().min(self.bytes.len() - self.offset);
            out[..count].copy_from_slice(&self.bytes[self.offset..self.offset + count]);
            self.offset += count;
            Ok(count)
        })
    }
}
fn input(bytes: Vec<u8>) -> RunInput {
    RunInput { max_bytes: bytes.len() as u64, reader: Box::new(Bytes { bytes, offset: 0 }) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{EmployeeEntitlement, Executable, PublishedRuntime};
    use duduclaw_isolation::{egress::ProfileId, ipc::QueueClass};
    fn fixture() -> (EmployeeId, PublishedRuntime) {
        (EmployeeId([1; 16]), PublishedRuntime { runtime: RuntimeKind::Claude,
            profile: ProfileId(Sha256Digest([2; 32])), executable: Executable::Elf {
                entry: Text::new("claude".into()).unwrap(), digest: Sha256Digest([3; 32]) },
            pinned_version: Text::new("test-only".into()).unwrap(),
            fixed_args: claude_arguments("model").unwrap().into_iter().map(|arg| Text::new(arg).unwrap()).collect(),
            resources: ResourceLimits { memory_bytes: 1024 * 1024, cpu_quota_percent: 100, tasks: 16,
                open_files: 4096, ptys: 1, disk: DiskQuota::LoopFilesystem { bytes: 1024, inodes: 32 },
                io_read_bytes_per_sec: 1024, io_write_bytes_per_sec: 1024, io_read_ops_per_sec: 10,
                io_write_ops_per_sec: 10, output_buffer_bytes: 4096, output_total_bytes: 4096, memfd_total_bytes: 4096 },
            timeouts: RunTimeouts { queue_ms: 1000, startup_ms: 1000, runtime_ms: 1000,
                pty_idle_ms: None, termination_grace_ms: 100 }, network: NetworkPolicy::Offline,
            mcp: true, pty_allowed: false, queue_class: QueueClass::Background })
    }
    fn policy(employee: EmployeeId, runtime: PublishedRuntime) -> RootPolicy {
        let profile = runtime.profile;
        RootPolicy::from_verified_publication(vec![runtime], vec![EmployeeEntitlement {
            employee, profiles: vec![profile] }]).unwrap()
    }
    #[test]
    fn claude_plan_uses_root_limits_and_exact_signed_input_protocol() {
        let (employee, runtime) = fixture();
        let resources = runtime.resources.clone();
        let plan = claude_plan(&policy(employee, runtime), employee, ExecutionClass::Dispatch,
            b"prompt".to_vec(), b"system".to_vec(), "model").unwrap();
        assert_eq!(plan.resources, resources);
        assert_eq!(plan.inputs.prompt.max_bytes, 6);
        assert_eq!(plan.inputs.attachments.len(), 1);
        assert_eq!(plan.inputs.attachments[0].max_bytes, 6);
        assert!(plan.environment_allowlist.is_empty());
        assert!(!plan.pty);
        assert_eq!(plan.argv_allowlist.last().unwrap().as_str(), "model");
    }
    #[test]
    fn claude_plan_refuses_model_override_unsigned_input_shape_and_other_employee() {
        let (employee, runtime) = fixture();
        let good = policy(employee, runtime.clone());
        for (who, class, prompt, model) in [(employee, ExecutionClass::EmployeeMain, vec![1], "other"),
            (EmployeeId([9; 16]), ExecutionClass::EmployeeMain, vec![1], "model"),
            (employee, ExecutionClass::EmployeeTool, vec![1], "model"),
            (employee, ExecutionClass::Judge, vec![1], "model"),
            (employee, ExecutionClass::EmployeeMain, vec![], "model"),
            (employee, ExecutionClass::EmployeeMain, vec![1; 4097], "model")] {
            assert!(claude_plan(&good, who, class, prompt, vec![], model).is_err());
        }
        assert!(claude_plan(&good, employee, ExecutionClass::EmployeeMain,
            vec![1; 2048], vec![2; 2049], "model").is_err());
        let mut changed = runtime;
        changed.fixed_args.push(Text::new("--resume".into()).unwrap());
        assert!(claude_plan(&policy(employee, changed), employee, ExecutionClass::EmployeeMain,
            vec![1], vec![], "model").is_err());
        assert!(claude_arguments("model\n--flag").is_err());
        assert!(InstalledMainPlanFactory::new(Sha256Digest([0; 32]), 100).is_err());
    }

    #[test]
    fn other_runtime_plan_requires_signed_protocol_and_employee_entitlement() {
        let (employee, mut runtime) = fixture();
        let kind = RuntimeKind::Registered(Text::new("generic-cli-v1.qwen".into()).unwrap());
        runtime.runtime = kind.clone();
        runtime.fixed_args = duduclaw_mcp_boundary::config::registered_cli_arguments(&kind).unwrap()
            .into_iter().map(|arg| Text::new(arg).unwrap()).collect();
        let good = policy(employee, runtime.clone());
        let plan = other_runtime_plan(&good, employee, ExecutionClass::Dispatch, kind.clone(), b"request".to_vec()).unwrap();
        assert_eq!(plan.runtime, kind);
        assert!(plan.argv_allowlist.is_empty());
        assert!(plan.environment_allowlist.is_empty());
        assert_eq!(plan.resources, runtime.resources);
        assert_eq!(plan.inputs.prompt.max_bytes, 7);
        assert!(plan.inputs.attachments.is_empty());
        for (who, class, request) in [(EmployeeId([9; 16]), ExecutionClass::Dispatch, vec![1]),
            (employee, ExecutionClass::EmployeeTool, vec![1]), (employee, ExecutionClass::Judge, vec![1]),
            (employee, ExecutionClass::Dispatch, vec![]), (employee, ExecutionClass::Dispatch, vec![1; 4097])] {
            assert!(other_runtime_plan(&good, who, class, kind.clone(), request).is_err());
        }
        let mut changed = runtime.clone();
        changed.fixed_args.push(Text::new("--other-config".into()).unwrap());
        assert!(other_runtime_plan(&policy(employee, changed), employee, ExecutionClass::Dispatch,
            kind.clone(), vec![1]).is_err());
        runtime.mcp = false;
        assert!(other_runtime_plan(&policy(employee, runtime), employee, ExecutionClass::Dispatch, kind, vec![1]).is_err());
    }

    #[test]
    fn api_worker_plan_requires_exact_signed_worker_and_input_budget() {
        let (employee, mut runtime) = fixture();
        runtime.runtime = RuntimeKind::OpenAiCompat;
        runtime.fixed_args = duduclaw_mcp_boundary::config::api_worker_arguments().into_iter()
            .map(|arg| Text::new(arg).unwrap()).collect();
        let good = policy(employee, runtime.clone());
        let plan = other_runtime_plan(&good, employee, ExecutionClass::EmployeeMain,
            RuntimeKind::OpenAiCompat, vec![1]).unwrap();
        assert_eq!(plan.runtime, RuntimeKind::OpenAiCompat);
        assert!(plan.environment_allowlist.is_empty());
        assert!(plan.argv_allowlist.is_empty());
        assert!(plan.capabilities.mcp_bridge);
        runtime.fixed_args.clear();
        assert!(other_runtime_plan(&policy(employee, runtime), employee, ExecutionClass::EmployeeMain,
            RuntimeKind::OpenAiCompat, vec![1]).is_err());
        assert!(other_runtime_plan(&good, employee, ExecutionClass::EmployeeMain,
            RuntimeKind::OpenAiCompat, vec![1; 4097]).is_err());
        assert!(other_runtime_plan(&good, employee, ExecutionClass::Judge,
            RuntimeKind::OpenAiCompat, vec![1]).is_err());
    }
    #[test]
    fn native_cli_plans_bind_adapter_protocol_and_require_grok_pty_entitlement() {
        let (employee, base) = fixture();
        for kind in [RuntimeKind::Codex, RuntimeKind::Grok] {
            let mut runtime = base.clone();
            runtime.runtime = kind.clone();
            runtime.fixed_args = duduclaw_mcp_boundary::config::native_cli_arguments(&kind).unwrap()
                .into_iter().map(|v| Text::new(v).unwrap()).collect();
            let grok = kind == RuntimeKind::Grok;
            if grok {
                assert!(other_runtime_plan(&policy(employee,runtime.clone()),employee,ExecutionClass::Dispatch,kind.clone(),vec![1]).is_err());
                runtime.pty_allowed = true;
                runtime.timeouts.pty_idle_ms = Some(1000);
            }
            let plan = other_runtime_plan(&policy(employee,runtime),employee,ExecutionClass::Dispatch,kind.clone(),vec![1]).unwrap();
            assert_eq!(plan.pty,grok);
            assert!(plan.argv_allowlist.is_empty());
            assert!(plan.environment_allowlist.is_empty());
        }
    }

}
