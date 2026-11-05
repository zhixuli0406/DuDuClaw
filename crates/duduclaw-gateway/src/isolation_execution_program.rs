//! execute_program plans for the exact root-published, offline helper protocol.
//! Common MCP approvals and capability gates run before this producer. The
//! ResourceCheckedPlans wrapper additionally requires an AuthorizedWorkspace.
use std::sync::Arc;
use duduclaw_execution::{program_helper::{ProgramRequest, PROGRAM_ARGUMENT, PROGRAM_RUNTIME},
    code_map_helper, office_helper, recording_helper};
use duduclaw_isolation::{bounded::Text, capability::ResourceGrant, execution::*, ids::Sha256Digest};
use duduclaw_mcp_boundary::{RequestContext, VerificationError, execution::{ExecutionPlanFactory, PolicyFuture}};
use duduclaw_spawnd::{installed::GatewayPublication, policy::PublishedRuntime};
use serde_json::Value;

// Leave time for helper kill/reap and typed result delivery before B2 retires the run.
const RESULT_GRACE_MS: u64 = 5_000;

pub struct InstalledProgramPlans { generation: Sha256Digest, gateway_gid: u32 }
impl InstalledProgramPlans {
    pub fn new(generation: Sha256Digest, gateway_gid: u32) -> Result<Arc<Self>, VerificationError> {
        if generation.0 == [0; 32] || gateway_gid == 0 { return Err(VerificationError::Invalid); }
        let plans = Arc::new(Self { generation, gateway_gid });
        plans.publication()?;
        Ok(plans)
    }

    fn publication(&self) -> Result<GatewayPublication, VerificationError> {
        let publication = GatewayPublication::read(self.gateway_gid).map_err(|_| VerificationError::Unavailable)?;
        if publication.generation != self.generation { return Err(VerificationError::BindingMismatch); }
        let matching = publication.runtimes.iter().filter(|record| helper_runtime(&record.runtime))
            .collect::<Vec<_>>();
        if matching.is_empty() || matching.iter().any(|record| !valid_helper(&record.to_runtime())) {
            return Err(VerificationError::Unavailable);
        }
        Ok(publication)
    }
}

fn is_program_runtime(runtime: &RuntimeKind) -> bool {
    matches!(runtime, RuntimeKind::Registered(name) if name.as_str() == PROGRAM_RUNTIME)
}
fn valid_runtime(runtime: &PublishedRuntime) -> bool {
    is_program_runtime(&runtime.runtime) && !runtime.mcp && !runtime.pty_allowed
        && matches!(runtime.network, NetworkPolicy::Offline)
        && runtime.fixed_args.len() == 4 && runtime.fixed_args[0].as_str() == PROGRAM_ARGUMENT
        && runtime.fixed_args.iter().skip(1).all(|digest| duduclaw_execution::program_helper::valid_digest(digest.as_str()))
        && runtime.resources.output_total_bytes >= duduclaw_execution::program_helper::MAX_RESULT_BYTES as u64
        && runtime.timeouts.runtime_ms > 0 && runtime.timeouts.runtime_ms <= 300_000 + RESULT_GRACE_MS
}

fn helper_runtime(runtime: &RuntimeKind) -> bool {
    match runtime {
        RuntimeKind::Registered(name) => name.as_str() == PROGRAM_RUNTIME || name.as_str() == code_map_helper::RUNTIME || name.as_str() == recording_helper::RUNTIME
            || name.as_str().strip_prefix("duduclaw-office-").and_then(|v| v.strip_suffix("-v1"))
                .and_then(|v| v.split_once('-')).is_some_and(|(skill, script)| office_helper::script_id(skill, script).is_ok()),
        _ => false,
    }
}
fn valid_helper(runtime: &PublishedRuntime) -> bool {
    if is_program_runtime(&runtime.runtime) { return valid_runtime(runtime); }
    let RuntimeKind::Registered(name) = &runtime.runtime else { return false; };
    if name.as_str() == recording_helper::RUNTIME {
        return valid_recording_runtime(runtime);
    }
    let protocol = if name.as_str() == code_map_helper::RUNTIME {
        runtime.fixed_args.iter().map(|v| v.as_str()).eq([code_map_helper::ARGUMENT])
    } else {
        let arguments: Vec<_> = runtime.fixed_args.iter().map(|v| v.as_str()).collect();
        arguments.len() == 5 && arguments[0] == office_helper::ARGUMENT
            && office_helper::script_id(arguments[1], arguments[2]).ok().as_deref() == Some(name.as_str())
            && duduclaw_execution::program_helper::valid_digest(arguments[3])
            && duduclaw_execution::program_helper::valid_digest(arguments[4])
    };
    helper_runtime(&runtime.runtime) && protocol && !runtime.mcp && !runtime.pty_allowed
        && matches!(runtime.network, NetworkPolicy::Offline)
        && runtime.resources.output_total_bytes >= code_map_helper::MAX_RESULT as u64
        && runtime.timeouts.runtime_ms > 0 && runtime.timeouts.runtime_ms <= 305_000
}
fn valid_recording_runtime(runtime: &PublishedRuntime) -> bool {
    matches!(&runtime.runtime, RuntimeKind::Registered(name) if name.as_str() == recording_helper::RUNTIME)
        && runtime.fixed_args.len() == 4 && runtime.fixed_args[0].as_str() == recording_helper::ARGUMENT
        && runtime.fixed_args.iter().skip(1).all(|arg| duduclaw_execution::program_helper::valid_digest(arg.as_str()))
        && !runtime.mcp && !runtime.pty_allowed && matches!(runtime.network, NetworkPolicy::Egress(_))
        && runtime.resources.output_total_bytes >= recording_helper::MAX_RESULT as u64
        && runtime.timeouts.runtime_ms >= 16_000 && runtime.timeouts.runtime_ms <= 320_000
}

struct HelperRequest { kind: RuntimeKind, bytes: Vec<u8>, timeout: u64 }
fn helper_request(context: &RequestContext, name: &str, arguments: &Value) -> Result<HelperRequest, VerificationError> {
    if !duduclaw_mcp_boundary::tool_policy::eligible(context, name) { return Err(VerificationError::Invalid); }
    let fields = arguments.as_object().ok_or(VerificationError::Invalid)?;
    if fields.get("workspace").is_some_and(|v| v.as_str() != Some("employee-work"))
        || !context.record().resources.as_slice().iter().any(|r| matches!(r,
            ResourceGrant::Workspace { handle } if handle.as_str() == "employee-work")) { return Err(VerificationError::Invalid); }
    let mut input = fields.clone(); input.remove("workspace");
    let (runtime, bytes, timeout) = match name {
        "browser_record_start" => {
            let request = recording_helper::Request::from_arguments(arguments, &"0".repeat(32))
                .map_err(|_| VerificationError::Invalid)?;
            (recording_helper::RUNTIME.into(), request.encode().map_err(|_| VerificationError::Invalid)?, request.max_seconds + 15)
        }
        "execute_program" => {
            let request = request(context, name, arguments)?;
            (PROGRAM_RUNTIME.to_owned(), request.encode().map_err(|_| VerificationError::Invalid)?, request.timeout_seconds)
        }
        "code_map" => {
            let bytes = serde_json::to_vec(&input).map_err(|_| VerificationError::Invalid)?;
            let request = code_map_helper::Request::decode(&bytes).map_err(|_| VerificationError::Invalid)?;
            (code_map_helper::RUNTIME.to_owned(), request.encode().map_err(|_| VerificationError::Invalid)?, 60)
        }
        "office_script" => {
            if input.keys().any(|key| !matches!(key.as_str(), "skill" | "script" | "args")) { return Err(VerificationError::Invalid); }
            let skill = input.get("skill").and_then(Value::as_str).ok_or(VerificationError::Invalid)?;
            let raw = input.get("script").and_then(Value::as_str).ok_or(VerificationError::Invalid)?;
            let script = raw.strip_suffix(".py").unwrap_or(raw);
            let runtime = office_helper::script_id(skill, script).map_err(|_| VerificationError::Invalid)?;
            let (home, agent) = crate::isolation_egress::employee_tool_identity(context)?;
            if duduclaw_agent::skill_state::read_disabled(&home, &agent).is_disabled(skill) { return Err(VerificationError::Invalid); }
            let bytes = serde_json::to_vec(&serde_json::json!({"args":input.get("args").cloned().unwrap_or(serde_json::json!([]))}))
                .map_err(|_| VerificationError::Invalid)?;
            let request = office_helper::Request::decode(&bytes).map_err(|_| VerificationError::Invalid)?;
            office_helper::paths(&request, skill, script).map_err(|_| VerificationError::Invalid)?;
            (runtime, request.encode().map_err(|_| VerificationError::Invalid)?, 180)
        }
        _ => return Err(VerificationError::Invalid),
    };
    Ok(HelperRequest { kind: RuntimeKind::Registered(Text::new(runtime).map_err(|_| VerificationError::Invalid)?), bytes, timeout })
}

fn timeout_fits(seconds: u64, runtime_ms: u64) -> bool {
    seconds.checked_mul(1000).and_then(|milliseconds| milliseconds.checked_add(RESULT_GRACE_MS))
        .is_some_and(|needed| needed <= runtime_ms)
}

fn request(context: &RequestContext, name: &str, arguments: &Value) -> Result<ProgramRequest, VerificationError> {
    if name != "execute_program" || !duduclaw_mcp_boundary::tool_policy::eligible(context, name) {
        return Err(VerificationError::Invalid);
    }
    let object = arguments.as_object().ok_or(VerificationError::Invalid)?;
    if object.keys().any(|key| !matches!(key.as_str(), "code" | "language" | "timeout_seconds" | "workspace"))
        || object.get("workspace").is_some_and(|workspace| workspace.as_str() != Some("employee-work"))
        || !context.record().resources.as_slice().iter().any(|resource|
            matches!(resource, ResourceGrant::Workspace { handle } if handle.as_str() == "employee-work")) {
        return Err(VerificationError::Invalid);
    }
    let mut input = object.clone();
    input.remove("workspace");
    if !input.contains_key("timeout_seconds") { input.insert("timeout_seconds".into(), Value::from(30)); }
    let bytes = serde_json::to_vec(&input).map_err(|_| VerificationError::Invalid)?;
    ProgramRequest::decode(&bytes).map_err(|_| VerificationError::Invalid)
}

impl ExecutionPlanFactory for InstalledProgramPlans {
    fn supports(&self, name: &str) -> bool {
        self.publication().is_ok_and(|publication| publication.runtimes.iter().any(|record| {
            let runtime = record.to_runtime();
            valid_helper(&runtime) && match name {
                "browser_record_start" | "browser_record_stop" => valid_recording_runtime(&runtime),
                "execute_program" => is_program_runtime(&runtime.runtime),
                "code_map" => matches!(&runtime.runtime, RuntimeKind::Registered(id) if id.as_str() == code_map_helper::RUNTIME),
                "office_script" => runtime.fixed_args.first().is_some_and(|arg| arg.as_str() == office_helper::ARGUMENT),
                _ => false,
            }
        }))
    }
    fn collects_results(&self, name: &str) -> bool { matches!(name, "execute_program" | "office_script" | "code_map" | "browser_record_start" | "browser_record_stop") }
    fn cooperative_stop(&self, name: &str) -> bool { name == "browser_record_start" }
    fn decode_result(&self, name: &str, stdout: &[u8], stderr: &[u8], outcome: &RunOutcome) -> Result<Value, VerificationError> {
        if !stderr.is_empty() || *outcome != RunOutcome::Exited(0) { return Err(VerificationError::Invalid); }
        if name == "code_map" { return code_map_helper::decode_result(stdout).map_err(|_| VerificationError::Invalid); }
        if name == "browser_record_start" { return recording_helper::decode_result(stdout).map_err(|_| VerificationError::Invalid); }
        if !matches!(name, "execute_program" | "office_script") { return Err(VerificationError::Invalid); }
        let result = duduclaw_execution::program_helper::ProgramResult::decode(stdout).map_err(|_| VerificationError::Invalid)?;
        serde_json::from_slice(&result.encode().map_err(|_| VerificationError::Invalid)?).map_err(|_| VerificationError::Invalid)
    }
    fn authorize_handler<'a>(&'a self, context: &'a RequestContext, name: &'a str,
        arguments: &'a Value) -> PolicyFuture<'a, ()> {
        Box::pin(async move {
            let request = helper_request(context, name, arguments)?;
            let publication = self.publication()?;
            let policy = publication.policy().map_err(|_| VerificationError::Invalid)?;
            let (runtime, _) = policy.derive(context.record().employee, &request.kind, false)
                .map_err(|_| VerificationError::Invalid)?;
            if !valid_helper(runtime) || !timeout_fits(request.timeout, runtime.timeouts.runtime_ms)
                || request.bytes.len() as u64 > runtime.resources.memfd_total_bytes { return Err(VerificationError::Invalid); }
            Ok(())
        })
    }
    fn prepare<'a>(&'a self, context: &'a RequestContext, name: &'a str,
        arguments: &'a Value, run: RunHandle) -> PolicyFuture<'a, SpawnPolicy> {
        Box::pin(async move {
            self.authorize_handler(context, name, arguments).await?;
            let mut request = helper_request(context, name, arguments)?;
            if name == "browser_record_start" {
                request.bytes = recording_helper::Request::from_arguments(arguments,
                    &duduclaw_mcp_boundary::execution_jobs::handle_text(run)).map_err(|_| VerificationError::Invalid)?
                    .encode().map_err(|_| VerificationError::Invalid)?;
            }
            let publication = self.publication()?;
            let policy = publication.policy().map_err(|_| VerificationError::Invalid)?;
            let (runtime, _) = policy.derive(context.record().employee, &request.kind, false)
                .map_err(|_| VerificationError::Invalid)?;
            if !valid_helper(runtime) || request.bytes.len() as u64 > runtime.resources.memfd_total_bytes
                || !timeout_fits(request.timeout, runtime.timeouts.runtime_ms) || run.0 == context.record().run {
                return Err(VerificationError::Invalid);
            }
            Ok(SpawnPolicy { workspace_delivery: None, run, employee: context.record().employee, class: ExecutionClass::EmployeeTool,
                runtime: request.kind, argv_allowlist: runtime.fixed_args.clone(), environment_allowlist: Default::default(),
                workspace: WorkspacePolicy { workspace: Text::new("employee-work".into()).map_err(|_| VerificationError::Invalid)?,
                    relative_cwd: Text::new(".".into()).map_err(|_| VerificationError::Invalid)? },
                resources: runtime.resources.clone(), network: runtime.network.clone(), pty: false,
                capabilities: CapabilityHandover { mcp_bridge: false, egress_forwarder: matches!(runtime.network, NetworkPolicy::Egress(_)) },
                timeouts: runtime.timeouts.clone(), inputs: RunInputs { prompt: RunInput {
                    max_bytes: request.bytes.len() as u64, reader: Box::new(ProgramInput(std::io::Cursor::new(request.bytes))) }, attachments: vec![] } })
        })
    }
}

struct ProgramInput(std::io::Cursor<Vec<u8>>);
impl RunReader for ProgramInput {
    fn read<'a>(&'a mut self, output: &'a mut [u8]) -> BackendFuture<'a, usize> {
        Box::pin(async move { std::io::Read::read(&mut self.0, output).map_err(|_| BackendError::Io) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_isolation::{bounded::List, capability::{CapabilityId, CapabilityRecord,
        EmployeeBinding, Lane, Principal}, ids::{EmployeeId, RunId}};
    use serde_json::json;
    fn context() -> RequestContext {
        RequestContext::from_verified_record(CapabilityRecord {
            id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([2; 16]), run: RunId([3; 16]),
            task: None, principal: Principal::Human { user_id: Text::new("owner".into()).unwrap(), generation: 1 },
            employee_binding: EmployeeBinding::Personal(Text::new("owner".into()).unwrap()), lane: Lane::Standard,
            tools: List::new(vec![Text::new("execute_program".into()).unwrap()]).unwrap(),
            resources: List::new(vec![ResourceGrant::Employee { id: EmployeeId([2; 16]) },
                ResourceGrant::Workspace { handle: Text::new("employee-work".into()).unwrap() }]).unwrap(),
            policy_revision: 1, gateway_epoch: [4; 16], issued_at: 1, not_before: 1, expires_at: 2,
            nonce: [5; 32], jti: [6; 16],
        })
    }
    #[test]
    fn program_plans_validate_code_map_and_typed_results_before_publication() {
        let mut record = context().record().clone();
        record.tools = List::new(vec![Text::new("code_map".into()).unwrap()]).unwrap();
        let context = RequestContext::from_verified_record(record);
        let prepared = helper_request(&context, "code_map", &json!({"query":"中文", "workspace":"employee-work"})).unwrap();
        assert_eq!(prepared.timeout, 60);
        assert!(matches!(prepared.kind, RuntimeKind::Registered(name) if name.as_str() == code_map_helper::RUNTIME));
        for field in ["argv", "cwd", "root", "employee_id"] {
            let mut args = json!({"query":"query"}); args[field] = json!("forged");
            assert!(helper_request(&context, "code_map", &args).is_err());
        }
        let factory = InstalledProgramPlans { generation: Sha256Digest([1; 32]), gateway_gid: 1 };
        let result = duduclaw_execution::program_helper::ProgramResult {
            status: duduclaw_execution::program_helper::ProgramStatus::Exited,
            exit_code: Some(0), signal: None, stdout: "hello".into(), stderr: String::new(), truncated: false,
        }.encode().unwrap();
        assert!(factory.decode_result("execute_program", &result, &[], &RunOutcome::Exited(0)).is_ok());
        assert!(factory.decode_result("execute_program", &result, b"unexpected", &RunOutcome::Exited(0)).is_err());
        assert!(factory.decode_result("execute_program", &result, &[], &RunOutcome::Cancelled).is_err());
        assert!(factory.decode_result("execute_program", br#"{"status":"exited","exit_code":0}"#, &[], &RunOutcome::Exited(0)).is_err());
        assert!(factory.decode_result("code_map", &result, &[], &RunOutcome::Exited(0)).is_err());
        assert!(factory.decode_result("fork_run", &result, &[], &RunOutcome::Exited(0)).is_err());
        assert!(InstalledProgramPlans::new(Sha256Digest([0; 32]), 1).is_err());
        assert!(InstalledProgramPlans::new(Sha256Digest([1; 32]), 0).is_err());
    }

    #[test]
    fn program_plans_reject_authority_and_unregistered_workspace_inputs() {
        let input = json!({"language":"python","code":"print('中文')"});
        let parsed = request(&context(), "execute_program", &input).unwrap();
        assert_eq!(parsed.timeout_seconds, 30);
        for key in ["argv", "env", "cwd", "root", "agent_id", "run_id", "workspace"] {
            let mut changed = input.clone(); changed[key] = json!("forged");
            assert!(request(&context(), "execute_program", &changed).is_err(), "{key}");
        }
        for name in ["office_script", "execute_program_extra", "fork_run"] {
            assert!(request(&context(), name, &input).is_err());
        }
        let mut record = context().record().clone();
        record.resources = List::new(vec![ResourceGrant::Employee { id: record.employee }]).unwrap();
        assert!(request(&RequestContext::from_verified_record(record), "execute_program", &input).is_err());
        assert!(request(&context(), "execute_program", &json!({"language":"bash","code":"true","timeout_seconds":301})).is_err());
    }

    #[test]
    fn program_plans_timeout_reserves_result_delivery_headroom() {
        assert!(!timeout_fits(30, 30_000));
        assert!(!timeout_fits(30, 34_999));
        assert!(timeout_fits(30, 35_000));
        assert!(!timeout_fits(300, 300_000));
        assert!(timeout_fits(300, 305_000));
        assert!(!timeout_fits(u64::MAX, u64::MAX));
    }

    #[test]
    fn program_plans_input_stream_delivers_only_bounded_protocol_bytes() {
        let request = request(&context(), "execute_program", &json!({"language":"javascript","code":"1+1"})).unwrap();
        let bytes = request.encode().unwrap();
        let mut input = ProgramInput(std::io::Cursor::new(bytes.clone()));
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        runtime.block_on(async {
            let mut output = vec![];
            let mut chunk = [0; 7];
            loop {
                let n = input.read(&mut chunk).await.unwrap();
                if n == 0 { break; }
                output.extend_from_slice(&chunk[..n]);
            }
            assert_eq!(output, bytes);
            assert_eq!(ProgramRequest::decode(&output).unwrap(), request);
        });
    }
}

#[cfg(test)]
mod recording_tests {
    use super::*;
    use duduclaw_isolation::{bounded::List, capability::{CapabilityId, CapabilityRecord, EmployeeBinding, Lane, Principal},
        egress::ProfileId, ids::{EmployeeId, RunId}, ipc::QueueClass};
    use serde_json::json;
    fn context() -> RequestContext {
        RequestContext::from_verified_record(CapabilityRecord {
            id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([2; 16]), run: RunId([3; 16]), task: None,
            principal: Principal::Human { user_id: Text::new("owner".into()).unwrap(), generation: 1 },
            employee_binding: EmployeeBinding::Personal(Text::new("owner".into()).unwrap()), lane: Lane::Standard,
            tools: List::new(vec![Text::new("browser_record_start".into()).unwrap()]).unwrap(),
            resources: List::new(vec![ResourceGrant::Employee { id: EmployeeId([2; 16]) },
                ResourceGrant::Workspace { handle: Text::new("employee-work".into()).unwrap() }]).unwrap(),
            policy_revision: 1, gateway_epoch: [4; 16], issued_at: 1, not_before: 1, expires_at: 2, nonce: [5; 32], jti: [6; 16],
        })
    }
    #[test]
    fn program_plans_recording_sealed_payload_cannot_override_identity_or_dependencies() {
        let prepared = helper_request(&context(), "browser_record_start", &json!({"url":"https://example.com", "headless":true})).unwrap();
        assert!(matches!(prepared.kind, RuntimeKind::Registered(ref id) if id.as_str() == recording_helper::RUNTIME));
        assert_eq!(prepared.timeout, 45);
        let request = recording_helper::Request::decode(&prepared.bytes).unwrap();
        assert_eq!(request.max_seconds, 30);
        assert_eq!(request.id, "0".repeat(32)); // Replaced only after backend reservation.
        for field in ["id", "root", "cwd", "run", "employee_id", "env", "proxy", "argv"] {
            let mut args = json!({"url":"https://example.com", "headless":true}); args[field] = json!("forged");
            assert!(helper_request(&context(), "browser_record_start", &args).is_err());
        }
        let factory = InstalledProgramPlans { generation: Sha256Digest([1; 32]), gateway_gid: 1 };
        assert!(factory.cooperative_stop("browser_record_start"));
        assert!(!factory.cooperative_stop("execute_program"));
        for name in ["fork_run", "merge_or_select", "terminate_branch"] { assert!(!factory.supports(name)); }
    }
    #[test]
    fn program_plans_recording_profile_requires_egress_and_exact_dependency_protocol() {
        let mut runtime = PublishedRuntime {
            runtime: RuntimeKind::Registered(Text::new(recording_helper::RUNTIME.into()).unwrap()),
            profile: ProfileId(Sha256Digest([1; 32])), executable: duduclaw_spawnd::policy::Executable::Elf {
                entry: Text::new("duduclaw-recording-helper".into()).unwrap(), digest: Sha256Digest([2; 32]) },
            pinned_version: Text::new("test-only".into()).unwrap(),
            fixed_args: [recording_helper::ARGUMENT.to_owned(), "a".repeat(64), "b".repeat(64), "c".repeat(64)].into_iter()
                .map(|v| Text::new(v).unwrap()).collect(),
            resources: ResourceLimits { memory_bytes: 1024, cpu_quota_percent: 100, tasks: 32, open_files: 4096, ptys: 0,
                disk: DiskQuota::LoopFilesystem { bytes: 1024, inodes: 32 }, io_read_bytes_per_sec: 1024, io_write_bytes_per_sec: 1024,
                io_read_ops_per_sec: 10, io_write_ops_per_sec: 10, output_buffer_bytes: 4096,
                output_total_bytes: recording_helper::MAX_RESULT as u64, memfd_total_bytes: recording_helper::MAX_REQUEST as u64 },
            timeouts: RunTimeouts { queue_ms: 1000, startup_ms: 1000, runtime_ms: 320_000, pty_idle_ms: None, termination_grace_ms: 1000 },
            network: NetworkPolicy::Egress(ProfileId(Sha256Digest([3; 32]))), mcp: false, pty_allowed: false, queue_class: QueueClass::Background,
        };
        assert!(valid_helper(&runtime));
        assert!(timeout_fits(315, runtime.timeouts.runtime_ms));
        runtime.network = NetworkPolicy::Offline; assert!(!valid_helper(&runtime));
        runtime.network = NetworkPolicy::Egress(ProfileId(Sha256Digest([3; 32])));
        runtime.mcp = true; assert!(!valid_helper(&runtime)); runtime.mcp = false;
        runtime.fixed_args.push(Text::new("--no-sandbox".into()).unwrap()); assert!(!valid_helper(&runtime));
    }
}
