//! Gateway-side main-run plans from the fresh root publication. This is not D's
//! ExecutionPlanFactory (which authorizes MCP-triggered helper operations).
use duduclaw_isolation::{bounded::Text, execution::*, ids::{EmployeeId, RunId, Sha256Digest}};
use crate::policy::{RootPolicy, LEGACY_FIXED_MAX_TURNS};

/// Immutable generation pin from composite boot readiness. Every invocation
/// rereads root policy; a cached entitlement can never authorize a later run.
pub struct InstalledMainPlanFactory { generation: Sha256Digest, gateway_gid: u32 }
impl InstalledMainPlanFactory {
    pub fn new(generation: Sha256Digest, gateway_gid: u32) -> Result<Self, BackendError> {
        if generation.0 == [0; 32] || gateway_gid == 0 { return Err(BackendError::Denied); }
        Ok(Self { generation, gateway_gid })
    }

    #[cfg(target_os = "linux")]
    fn fresh_policy(&self) -> Result<RootPolicy, BackendError> {
        let publication = crate::installed::GatewayPublication::read(self.gateway_gid)
            .map_err(|_| BackendError::Unavailable)?;
        if publication.generation != self.generation { return Err(BackendError::Denied); }
        publication.policy().map_err(|_| BackendError::Denied)
    }

    /// R6: the step limit root will enforce for this employee's Claude run,
    /// derived from the fresh root publication with the same rule spawnd
    /// applies at StartRun. `desired` is the caller's own wish (the gateway
    /// resolution for a work turn; at most 50 for a utility call); it can only
    /// be narrowed. Pass the result unchanged to [`Self::claude`].
    #[cfg(target_os = "linux")]
    pub fn claude_max_turns(&self, employee: EmployeeId, desired: u64) -> Result<u16, BackendError> {
        claude_max_turns(&self.fresh_policy()?, employee, desired)
    }

    /// `max_turns` must be the value of [`Self::claude_max_turns`]; the plan
    /// re-derives it and refuses on any difference (e.g. a changed entitlement).
    #[cfg(target_os = "linux")]
    pub fn claude(&self, employee: EmployeeId, class: ExecutionClass,
        prompt: Vec<u8>, system_prompt: Vec<u8>, model: &str, max_turns: u16) -> Result<SpawnPolicy, BackendError> {
        claude_plan(&self.fresh_policy()?, employee, class, prompt, system_prompt, model, max_turns)
    }

    #[cfg(target_os = "linux")]
    pub fn other_runtime(&self, employee: EmployeeId, class: ExecutionClass,
        runtime: RuntimeKind, request: Vec<u8>) -> Result<SpawnPolicy, BackendError> {
        other_runtime_plan(&self.fresh_policy()?, employee, class, runtime, request)
    }
}

/// Gateway half of the R6 rule. Without a signed range the signed v1/v2
/// arguments fix `--max-turns 50`; with one, the narrowed request must be
/// exactly what `RootPolicy::max_turns` (spawnd's half) enforces, or Denied.
fn claude_max_turns(policy: &RootPolicy, employee: EmployeeId, desired: u64) -> Result<u16, BackendError> {
    let (runtime, _) = policy.derive(employee, &RuntimeKind::Claude, false).map_err(|_| BackendError::Denied)?;
    let Some(limit) = runtime.turn_limit else { return Ok(LEGACY_FIXED_MAX_TURNS); };
    let requested = crate::policy::narrowed_request(limit, policy.turn_cap(employee), desired);
    match policy.max_turns(employee, runtime, Some(requested)) {
        Ok(Some(value)) if value == requested => Ok(value),
        _ => Err(BackendError::Denied),
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
        timeouts: runtime.timeouts.clone(), inputs: RunInputs { prompt: input(request), attachments: vec![] },
        requested_max_turns: None })
}

/// The signed runtime must already contain this exact input protocol. These
/// arguments are never sent to root as authority. fd 4 is the sealed prompt;
/// Claude consumes the same bytes on stdin, and fd 5 is the system prompt.
/// This is the policy v1/v2 shape, which fixes `--max-turns 50`.
pub fn claude_arguments(model: &str) -> Result<Vec<String>, BackendError> {
    let mut args = claude_ranged_arguments(model)?;
    let position = args.iter().position(|arg| arg == "mcp__duduclaw__*").ok_or(BackendError::Denied)?;
    args.insert(position + 1, LEGACY_FIXED_MAX_TURNS.to_string());
    args.insert(position + 1, "--max-turns".to_owned());
    Ok(args)
}

/// Policy v3 shape of a ranged Claude record (R6): no `--max-turns`; spawnd
/// inserts `--max-turns <effective>` before the fixed MCP suffix per run.
pub fn claude_ranged_arguments(model: &str) -> Result<Vec<String>, BackendError> {
    if model.len() > 256 || model.chars().any(char::is_control) || model.trim() != model {
        return Err(BackendError::Denied);
    }
    let mut args: Vec<String> = crate::policy::CLAUDE_FIXED_PREFIX.into_iter().map(str::to_owned).collect();
    if !model.is_empty() { args.extend(["--model".into(), model.into()]); }
    args.extend(duduclaw_mcp_boundary::config::claude_mcp_arguments());
    Ok(args)
}

fn claude_plan(policy: &RootPolicy, employee: EmployeeId, class: ExecutionClass,
    prompt: Vec<u8>, system_prompt: Vec<u8>, model: &str, max_turns: u16) -> Result<SpawnPolicy, BackendError> {
    if !matches!(class, ExecutionClass::EmployeeMain | ExecutionClass::Scheduled | ExecutionClass::Dispatch
        | ExecutionClass::Heartbeat | ExecutionClass::Mail | ExecutionClass::Reminder)
        || prompt.is_empty() || prompt.len() > duduclaw_isolation::ipc::MAX_PROMPT_BYTES as usize
        || system_prompt.len() > duduclaw_isolation::ipc::MAX_PROMPT_BYTES as usize {
        return Err(BackendError::Denied);
    }
    let (runtime, _) = policy.derive(employee, &RuntimeKind::Claude, false).map_err(|_| BackendError::Denied)?;
    // R6: the fixed part of the signed arguments is compared exactly; the
    // step limit must be the very value spawnd will enforce for this run.
    let (expected, requested_max_turns) = match runtime.turn_limit {
        None if max_turns == LEGACY_FIXED_MAX_TURNS => (claude_arguments(model)?, None),
        None => return Err(BackendError::Denied),
        Some(_) => match policy.max_turns(employee, runtime, Some(max_turns)) {
            Ok(Some(value)) if value == max_turns => (claude_ranged_arguments(model)?, Some(max_turns)),
            _ => return Err(BackendError::Denied),
        },
    };
    if !runtime.mcp || runtime.fixed_args.iter().map(|arg| arg.as_str()).ne(expected.iter().map(String::as_str))
        || (prompt.len() as u64).checked_add(system_prompt.len() as u64)
            .is_none_or(|total| total > runtime.resources.memfd_total_bytes) {
        return Err(BackendError::Denied);
    }
    // D adds its own three fixed MCP arguments after reservation. Do not add
    // them twice or copy anything from the legacy native Command builder.
    // A ranged record gets the same `--max-turns <n>` root inserts before them.
    let mut argv_allowlist = runtime.fixed_args.clone();
    argv_allowlist.truncate(argv_allowlist.len() - 3);
    if let Some(value) = requested_max_turns {
        argv_allowlist.extend([Text::new("--max-turns".into()).map_err(|_| BackendError::Denied)?,
            Text::new(value.to_string()).map_err(|_| BackendError::Denied)?]);
    }
    Ok(SpawnPolicy { workspace_delivery: None, run: RunHandle(RunId([0; 16])), employee, class, runtime: RuntimeKind::Claude,
        argv_allowlist, environment_allowlist: Default::default(),
        workspace: WorkspacePolicy { workspace: Text::new("employee-work".into()).map_err(|_| BackendError::Denied)?,
            relative_cwd: Text::new(String::new()).map_err(|_| BackendError::Denied)? },
        resources: runtime.resources.clone(), network: runtime.network.clone(), pty: false,
        capabilities: CapabilityHandover { mcp_bridge: true,
            egress_forwarder: matches!(runtime.network, NetworkPolicy::Egress(_)) },
        timeouts: runtime.timeouts.clone(), inputs: RunInputs {
            prompt: input(prompt), attachments: vec![input(system_prompt)] }, requested_max_turns })
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
            mcp: true, pty_allowed: false, queue_class: QueueClass::Background, turn_limit: None })
    }
    fn policy(employee: EmployeeId, runtime: PublishedRuntime) -> RootPolicy {
        capped_policy(employee, runtime, None)
    }
    fn capped_policy(employee: EmployeeId, runtime: PublishedRuntime, max_turns: Option<u16>) -> RootPolicy {
        let profile = runtime.profile;
        RootPolicy::from_verified_publication(vec![runtime], vec![EmployeeEntitlement {
            employee, profiles: vec![profile], max_turns }]).unwrap()
    }
    /// A policy v3 Claude record with the given signed range.
    fn ranged(limit: crate::policy::TurnLimit) -> (EmployeeId, PublishedRuntime) {
        let (employee, mut runtime) = fixture();
        runtime.fixed_args = claude_ranged_arguments("model").unwrap().into_iter().map(|arg| Text::new(arg).unwrap()).collect();
        runtime.turn_limit = Some(limit);
        (employee, runtime)
    }
    #[test]
    fn claude_plan_uses_root_limits_and_exact_signed_input_protocol() {
        let (employee, runtime) = fixture();
        let resources = runtime.resources.clone();
        let plan = claude_plan(&policy(employee, runtime), employee, ExecutionClass::Dispatch,
            b"prompt".to_vec(), b"system".to_vec(), "model", 50).unwrap();
        assert!(claude_arguments("model").unwrap().windows(2).any(|w| w[0] == "--max-turns" && w[1] == "50"));
        assert_eq!(plan.resources, resources);
        assert_eq!(plan.inputs.prompt.max_bytes, 6);
        assert_eq!(plan.inputs.attachments.len(), 1);
        assert_eq!(plan.inputs.attachments[0].max_bytes, 6);
        assert!(plan.environment_allowlist.is_empty());
        assert!(!plan.pty);
        assert_eq!(plan.argv_allowlist.last().unwrap().as_str(), "model");
        // v1/v2 (no signed range): the fixed 50, and no request on the wire.
        assert_eq!(plan.requested_max_turns, None);
    }
    #[test]
    fn legacy_claude_arguments_are_byte_identical_to_the_fixed_fifty_shape() {
        let legacy: Vec<String> = ["-p", "--input-format", "text", "--output-format", "stream-json",
            "--verbose", "--system-prompt-file", "/proc/self/fd/5", "--tools", "", "--allowedTools", "mcp__duduclaw__*",
            "--max-turns", "50", "--model", "model", "--strict-mcp-config", "--mcp-config", "/config/mcp.json"]
            .into_iter().map(str::to_owned).collect();
        assert_eq!(claude_arguments("model").unwrap(), legacy);
        let ranged = claude_ranged_arguments("").unwrap();
        assert!(!ranged.iter().any(|arg| arg.starts_with("--max-turns")));
        assert!(ranged.ends_with(&duduclaw_mcp_boundary::config::claude_mcp_arguments()));
    }
    #[test]
    fn legacy_policy_keeps_fifty_and_refuses_any_other_value() {
        let (employee, runtime) = fixture();
        let good = capped_policy(employee, runtime, Some(10));
        // An entitlement cap cannot change a v1/v2 record: it stays 50.
        for desired in [1, 10, 50, 200, 10_000] { assert_eq!(claude_max_turns(&good, employee, desired), Ok(50)); }
        for value in [1, 49, 51, 200] {
            assert!(claude_plan(&good, employee, ExecutionClass::EmployeeMain, vec![1], vec![], "model", value).is_err());
        }
    }
    #[test]
    fn ranged_claude_plan_carries_exactly_the_value_root_enforces() {
        let limit = crate::policy::WORKING_DEFAULT_TURN_LIMIT;
        let (employee, runtime) = ranged(limit);
        let good = policy(employee, runtime.clone());
        // Work turn: the gateway's own resolution, narrowed into the signed range.
        assert_eq!(claude_max_turns(&good, employee, 120), Ok(120));
        assert_eq!(claude_max_turns(&good, employee, 10_000), Ok(200));
        // Utility call: asks at most 50, so never above 50.
        assert_eq!(claude_max_turns(&good, employee, 50), Ok(50));
        let plan = claude_plan(&good, employee, ExecutionClass::EmployeeMain, vec![1], vec![], "model", 120).unwrap();
        assert_eq!(plan.requested_max_turns, Some(120));
        let argv: Vec<_> = plan.argv_allowlist.iter().map(|arg| arg.as_str()).collect();
        assert_eq!(&argv[argv.len() - 4..], ["--model", "model", "--max-turns", "120"]);
        // Out of the signed range, or 0: refused, never clamped.
        for value in [0, 201, 1000, u16::MAX] {
            assert!(claude_plan(&good, employee, ExecutionClass::EmployeeMain, vec![1], vec![], "model", value).is_err());
        }
        // The employee's root ceiling narrows; a request above it is refused.
        let capped = capped_policy(employee, runtime.clone(), Some(30));
        assert_eq!(claude_max_turns(&capped, employee, 120), Ok(30));
        assert!(claude_plan(&capped, employee, ExecutionClass::EmployeeMain, vec![1], vec![], "model", 120).is_err());
        assert_eq!(claude_plan(&capped, employee, ExecutionClass::EmployeeMain, vec![1], vec![], "model", 30)
            .unwrap().requested_max_turns, Some(30));
        // A wish below the signed minimum is refused, not raised.
        let (employee, runtime) = ranged(crate::policy::TurnLimit { min: 5, max: 200, default: 50 });
        assert!(claude_max_turns(&policy(employee, runtime), employee, 3).is_err());
        // A legacy-shaped record cannot claim a range, and vice versa.
        let (employee, mut runtime) = ranged(limit);
        runtime.fixed_args = claude_arguments("model").unwrap().into_iter().map(|arg| Text::new(arg).unwrap()).collect();
        assert!(RootPolicy::from_verified_publication(vec![runtime], vec![EmployeeEntitlement {
            employee, profiles: vec![ProfileId(Sha256Digest([2; 32]))], max_turns: None }]).is_err());
    }
    /// The gateway's value and spawnd's enforced value come from one rule on
    /// one publication: for every range, ceiling and wish, what the gateway
    /// writes into the step-budget sentence is exactly the `--max-turns` root
    /// puts into the bootstrap argv for that run (or both refuse).
    #[test]
    fn gateway_and_spawnd_compute_the_same_step_limit() {
        use crate::policy::TurnLimit;
        let limits = [TurnLimit { min: 1, max: 200, default: 50 }, TurnLimit { min: 5, max: 1000, default: 1000 },
            TurnLimit { min: 50, max: 50, default: 50 }, TurnLimit { min: 1, max: 1, default: 1 }];
        for limit in limits {
            for cap in [None, Some(1), Some(4), Some(30), Some(50), Some(999), Some(1000)] {
                for desired in [0u64, 1, 3, 30, 49, 50, 51, 200, 201, 999, 1000, 1001, u64::MAX] {
                    let (employee, runtime) = ranged(limit);
                    let root = capped_policy(employee, runtime, cap);
                    let (runtime, _) = root.derive(employee, &RuntimeKind::Claude, false).unwrap();
                    let gateway = claude_max_turns(&root, employee, desired);
                    let Ok(value) = gateway else {
                        // Refused on the gateway: nothing in range would be enforced either.
                        let request = crate::policy::narrowed_request(limit, cap, desired);
                        assert!(root.max_turns(employee, runtime, Some(request)).is_err());
                        continue;
                    };
                    assert!(value <= limit.max && cap.is_none_or(|cap| value <= cap) && u64::from(value) <= desired.max(1));
                    let plan = claude_plan(&root, employee, ExecutionClass::EmployeeMain, vec![1], vec![], "model", value).unwrap();
                    // spawnd's half, at StartRun, from the same request.
                    let enforced = root.max_turns(employee, runtime, plan.requested_max_turns).unwrap();
                    assert_eq!(enforced, Some(value));
                    let bootstrap = crate::bootstrap::BootstrapPlan::from_runtime(RunHandle(RunId([1; 16])), employee,
                        70000, 70000, 400, 400, [3; 32], runtime, false, 1, &[], None, enforced).unwrap();
                    let position = bootstrap.argv.iter().position(|arg| arg == "--max-turns").unwrap();
                    assert_eq!(bootstrap.argv.get(position + 1).map(String::as_str), Some(value.to_string().as_str()));
                    assert_eq!(bootstrap.argv.iter().filter(|arg| *arg == "--max-turns").count(), 1);
                }
            }
        }
        // Legacy: the gateway's 50 is the `--max-turns 50` of the signed args.
        let (employee, runtime) = fixture();
        let root = policy(employee, runtime);
        let (runtime, _) = root.derive(employee, &RuntimeKind::Claude, false).unwrap();
        assert_eq!(claude_max_turns(&root, employee, 200), Ok(LEGACY_FIXED_MAX_TURNS));
        assert_eq!(root.max_turns(employee, runtime, None), Ok(None));
        let bootstrap = crate::bootstrap::BootstrapPlan::from_runtime(RunHandle(RunId([1; 16])), employee,
            70000, 70000, 400, 400, [3; 32], runtime, false, 1, &[], None, None).unwrap();
        assert!(bootstrap.argv.windows(2).any(|w| w[0] == "--max-turns" && w[1] == "50"));
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
            assert!(claude_plan(&good, who, class, prompt, vec![], model, 50).is_err());
        }
        assert!(claude_plan(&good, employee, ExecutionClass::EmployeeMain,
            vec![1; 2048], vec![2; 2049], "model", 50).is_err());
        // An unsigned argument shape: root refuses the whole publication now
        // (R6 review round), so no plan can be derived from it at all.
        let mut changed = runtime;
        changed.fixed_args.push(Text::new("--resume".into()).unwrap());
        assert!(RootPolicy::from_verified_publication(vec![changed], vec![EmployeeEntitlement {
            employee, profiles: vec![ProfileId(Sha256Digest([2; 32]))], max_turns: None }]).is_err());
        assert!(claude_arguments("model\n--flag").is_err());
        assert!(claude_ranged_arguments("model\n--flag").is_err());
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
