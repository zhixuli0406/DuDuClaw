use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use duduclaw_isolation::{capability::{CapabilityId, CapabilityRecord, EmployeeBinding, Lane,
    Principal, ResourceGrant}, ids::{EmployeeId, RunId, Sha256Digest}};
use crate::{parsing::{TableKind, table_request}, tool_policy::ToolPolicy};
use serde_json::json;

fn context(tool: &str) -> RequestContext {
    RequestContext::from_verified_record(CapabilityRecord {
        id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([2; 16]), run: RunId([3; 16]),
        task: None, principal: Principal::Human { user_id: Text::new("owner".into()).unwrap(), generation: 1 },
        employee_binding: EmployeeBinding::Personal(Text::new("owner".into()).unwrap()), lane: Lane::Standard,
        tools: List::new(vec![Text::new(tool.into()).unwrap()]).unwrap(),
        resources: List::new(vec![ResourceGrant::Employee { id: EmployeeId([2; 16]) },
            ResourceGrant::Workspace { handle: Text::new("work".into()).unwrap() },
            ResourceGrant::Workspace { handle: Text::new("other".into()).unwrap() }]).unwrap(),
        policy_revision: 1, gateway_epoch: [4; 16], issued_at: 1, not_before: 1, expires_at: 2,
        nonce: [5; 32], jti: [6; 16],
    })
}
struct Resolver { calls: AtomicUsize, changes: bool, refuses: bool }
impl ResourceResolver for Resolver {
    fn resolve(&self, context: &RequestContext, _: &ToolPolicy, _: &Value)
        -> Result<Vec<ResourceGrant>, VerificationError> {
        if self.refuses { return Err(VerificationError::Unavailable); }
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(vec![ResourceGrant::Employee { id: context.record().employee },
            ResourceGrant::Workspace { handle: Text::new(if self.changes && call > 0 { "other" } else { "work" }.into()).unwrap() }])
    }
}
fn resolver(changes: bool, refuses: bool) -> Arc<Resolver> {
    Arc::new(Resolver { calls: AtomicUsize::new(0), changes, refuses })
}
struct Plans(AtomicUsize);
impl ExecutionPlanFactory for Plans {
    fn supports(&self, name: &str) -> bool { name == "execute_program" }
    fn authorize_handler<'a>(&'a self, _: &'a RequestContext, _: &'a str, _: &'a Value) -> PolicyFuture<'a, ()> {
        Box::pin(async move { self.0.fetch_add(1, Ordering::SeqCst); Ok(()) })
    }
    fn prepare<'a>(&'a self, _: &'a RequestContext, _: &'a str, _: &'a Value, _: RunHandle)
        -> PolicyFuture<'a, SpawnPolicy> {
        Box::pin(async { Err(VerificationError::Unavailable) })
    }
}

#[test]
fn isolated_boot_missing_factories_stay_unavailable() {
    let factories = GatewayFactories::assemble(None, None, resolver(false, false), None, Arc::new(crate::resources::ResourceRoots::default())).unwrap();
    assert!(factories.execution.is_none());
    assert!(factories.parser.is_none());
    assert!(GatewayFactories::assemble(None, Some(Arc::new(Plans(AtomicUsize::new(0)))),
        resolver(false, false), None, Arc::new(crate::resources::ResourceRoots::default())).is_err());
}

#[tokio::test]
async fn isolated_boot_resource_authorization_precedes_factory() {
    let plans = Arc::new(Plans(AtomicUsize::new(0)));
    let checked = ResourceCheckedPlans { plans: plans.clone(), resolver: resolver(false, true), roots: Arc::new(crate::resources::ResourceRoots::default()) };
    assert!(checked.authorize_handler(&context("execute_program"), "execute_program", &json!({})).await.is_err());
    assert_eq!(plans.0.load(Ordering::SeqCst), 0);
    assert!(!checked.supports("execute_program_extra"));
}

#[tokio::test]
async fn isolated_boot_resource_change_during_factory_is_refused() {
    let plans = Arc::new(Plans(AtomicUsize::new(0)));
    let checked = ResourceCheckedPlans { plans: plans.clone(), resolver: resolver(true, false), roots: Arc::new(crate::resources::ResourceRoots::default()) };
    assert!(matches!(checked.authorize_handler(&context("execute_program"), "execute_program", &json!({})).await,
        Err(VerificationError::BindingMismatch)));
    assert_eq!(plans.0.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn isolated_boot_plan_cannot_select_workspace_outside_resolved_grants() {
    use duduclaw_isolation::execution::*;
    struct Input;
    impl RunReader for Input {
        fn read<'a>(&'a mut self, _: &'a mut [u8]) -> BackendFuture<'a, usize> { Box::pin(async { Ok(0) }) }
    }
    struct OtherWorkspace;
    impl ExecutionPlanFactory for OtherWorkspace {
        fn supports(&self, name: &str) -> bool { name == "execute_program" }
        fn authorize_handler<'a>(&'a self, _: &'a RequestContext, _: &'a str, _: &'a Value) -> PolicyFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
        fn prepare<'a>(&'a self, context: &'a RequestContext, _: &'a str, _: &'a Value,
            run: RunHandle) -> PolicyFuture<'a, SpawnPolicy> {
            Box::pin(async move { Ok(SpawnPolicy {
                workspace_delivery: None, run, employee: context.record().employee, class: ExecutionClass::EmployeeTool,
                runtime: RuntimeKind::Registered(Text::new("test-helper".into()).unwrap()),
                argv_allowlist: vec![], environment_allowlist: Default::default(),
                workspace: WorkspacePolicy { workspace: Text::new("other".into()).unwrap(),
                    relative_cwd: Text::new(".".into()).unwrap() },
                resources: ResourceLimits { memory_bytes: 1024, cpu_quota_percent: 100, tasks: 2,
                    open_files: 32, ptys: 0, disk: DiskQuota::LoopFilesystem { bytes: 1024, inodes: 32 },
                    io_read_bytes_per_sec: 1024, io_write_bytes_per_sec: 1024, io_read_ops_per_sec: 10,
                    io_write_ops_per_sec: 10, output_buffer_bytes: 1024, output_total_bytes: 1024,
                    memfd_total_bytes: 1024 },
                network: NetworkPolicy::Offline, pty: false,
                capabilities: CapabilityHandover { mcp_bridge: false, egress_forwarder: false },
                timeouts: RunTimeouts { queue_ms: 1000, startup_ms: 1000, runtime_ms: 1000,
                    pty_idle_ms: None, termination_grace_ms: 100 },
                inputs: RunInputs { prompt: RunInput { reader: Box::new(Input), max_bytes: 1 }, attachments: vec![] },
            }) })
        }
    }
    let context = context("execute_program");
    // Both workspaces occur in the capability, but this operation resolves only work.
    let checked = ResourceCheckedPlans { plans: Arc::new(OtherWorkspace), resolver: resolver(false, false),
        roots: Arc::new(crate::resources::ResourceRoots::default()) };
    assert!(matches!(checked.prepare(&context, "execute_program", &json!({}), RunHandle(RunId([9; 16]))).await,
        Err(VerificationError::BindingMismatch)));
}

fn options() -> TableOptions {
    TableOptions { kind: TableKind::Csv, delimiter: b',', has_header: true, offset: 0, limit: 2, sheet: None }
}
fn table() -> Value { json!({"columns":["name"],"rows":[{"name":"員工"}],"truncated":false}) }

#[tokio::test]
async fn isolated_worker_adapter_delivers_authorized_fd_and_bounded_options() {
    let invoked = Arc::new(AtomicUsize::new(0));
    let counter = invoked.clone();
    let parser = GatewayTableParser::new(Arc::new(move |request, file, options| {
        counter.fetch_add(1, Ordering::SeqCst);
        assert!(file.metadata().unwrap().is_file());
        assert_eq!(request.inputs.as_slice().len(), 1);
        assert_eq!(options.limit, 2);
        Ok(table())
    }));
    let file = tempfile::tempfile().unwrap();
    let request = table_request(&file, &options()).unwrap();
    let response = parser.parse(context("csv_read"), request.clone(), file, options()).await.unwrap();
    assert_eq!(crate::parsing::decode_table_response(&request, &options(), response).unwrap()["row_count"], 1);
    assert_eq!(invoked.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn isolated_worker_adapter_rejects_mismatched_input_and_untrusted_result() {
    let invoked = Arc::new(AtomicUsize::new(0));
    let counter = invoked.clone();
    let parser = GatewayTableParser::new(Arc::new(move |_, _, _| {
        counter.fetch_add(1, Ordering::SeqCst);
        let mut value = table(); value["path"] = json!("forged"); Ok(value)
    }));
    let file = tempfile::tempfile().unwrap();
    let mut request = table_request(&file, &options()).unwrap();
    request.inputs = List::new(vec![duduclaw_isolation::worker::InputFd { slot: 0,
        format: duduclaw_isolation::worker::ContentFormat::Text, size_bytes: 100 }]).unwrap();
    assert!(parser.parse(context("csv_read"), request, file, options()).await.is_err());
    assert_eq!(invoked.load(Ordering::SeqCst), 0);
    let file = tempfile::tempfile().unwrap();
    let request = table_request(&file, &options()).unwrap();
    assert!(parser.parse(context("csv_read"), request, file, options()).await.is_err());
    assert_eq!(invoked.load(Ordering::SeqCst), 1);
}

#[test]
fn isolated_worker_adapter_bounds_utf8_result_chunks() {
    let file = tempfile::tempfile().unwrap();
    let request = table_request(&file, &options()).unwrap();
    let value = json!({"columns":["name"],"rows":[{"name":"員".repeat(2700)}],"truncated":false});
    let response = encode_table_result(&request, value).unwrap();
    assert!(crate::parsing::decode_table_response(&request, &options(), response).is_ok());
    assert!(encode_table_result(&request, json!("員".repeat(MAX_RESULT_TEXT_BYTES))).is_err());
}
