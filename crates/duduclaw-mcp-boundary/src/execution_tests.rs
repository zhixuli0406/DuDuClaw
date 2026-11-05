use super::*;
use std::{collections::BTreeMap, sync::atomic::{AtomicUsize, Ordering}};
use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord,
    EmployeeBinding, Lane, Principal}, execution::*, ids::{EmployeeId, RunId, Sha256Digest}};

fn context(tool: &str) -> RequestContext {
    RequestContext::from_verified_record(CapabilityRecord {
        id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([2; 16]), run: RunId([3; 16]),
        task: None, principal: Principal::Human { user_id: Text::new("owner".into()).unwrap(), generation: 1 },
        employee_binding: EmployeeBinding::Personal(Text::new("owner".into()).unwrap()), lane: Lane::Standard,
        tools: List::new(vec![Text::new(tool.into()).unwrap()]).unwrap(),
        resources: List::new(vec![ResourceGrant::Employee { id: EmployeeId([2; 16]) },
            ResourceGrant::Workspace { handle: Text::new("work".into()).unwrap() }]).unwrap(),
        policy_revision: 1, gateway_epoch: [4; 16], issued_at: 1, not_before: 1, expires_at: 2,
        nonce: [5; 32], jti: [6; 16],
    })
}

#[derive(Default)]
struct Backend {
    reserved: AtomicUsize,
    spawned: AtomicUsize,
    killed: AtomicUsize,
    waited: AtomicUsize,
    fail_spawn: bool,
    return_caller: bool,
}
impl ExecutionBackend for Backend {
    fn reserve(&self, employee: EmployeeId) -> BackendFuture<'_, RunHandle> {
        Box::pin(async move {
            assert_eq!(employee, EmployeeId([2; 16]));
            self.reserved.fetch_add(1, Ordering::SeqCst);
            Ok(RunHandle(RunId([if self.return_caller { 3 } else { 9 }; 16])))
        })
    }
    fn spawn(&self, policy: SpawnPolicy) -> BackendFuture<'_, RunHandle> {
        Box::pin(async move {
            self.spawned.fetch_add(1, Ordering::SeqCst);
            if self.fail_spawn { Err(BackendError::Unavailable) } else { Ok(policy.run) }
        })
    }
    fn wait(&self, _: RunHandle, _: WaitOrigin) -> BackendFuture<'_, RunOutcome> {
        Box::pin(async move { self.waited.fetch_add(1, Ordering::SeqCst); Err(BackendError::WouldDeadlock) })
    }
    fn kill(&self, run: RunHandle) -> BackendFuture<'_, ()> {
        Box::pin(async move {
            assert_eq!(run, RunHandle(RunId([9; 16])));
            self.killed.fetch_add(1, Ordering::SeqCst); Ok(())
        })
    }
    fn take_stdio(&self, _: RunHandle) -> BackendFuture<'_, RunStdio> {
        Box::pin(async { Err(BackendError::Denied) })
    }
    fn take_pty(&self, _: RunHandle) -> BackendFuture<'_, RunPty> {
        Box::pin(async { Err(BackendError::Denied) })
    }
    fn resize(&self, _: RunHandle, _: PtySize) -> BackendFuture<'_, ()> {
        Box::pin(async { Err(BackendError::Denied) })
    }
}

struct EmptyInput;
impl RunReader for EmptyInput {
    fn read<'a>(&'a mut self, _: &'a mut [u8]) -> BackendFuture<'a, usize> { Box::pin(async { Ok(0) }) }
}

#[derive(Default)]
struct Factory { refuse: bool, bad_employee: bool }
impl ExecutionPlanFactory for Factory {
    fn supports(&self, name: &str) -> bool { matches!(name, "execute_program" | "skill_extract") }
    fn authorize_handler<'a>(&'a self, _: &'a RequestContext, _: &'a str, _: &'a Value) -> PolicyFuture<'a, ()> {
        Box::pin(async move { if self.refuse { Err(VerificationError::Invalid) } else { Ok(()) } })
    }
    fn prepare<'a>(&'a self, context: &'a RequestContext, name: &'a str, _: &'a Value,
        reservation: RunHandle) -> PolicyFuture<'a, SpawnPolicy> {
        Box::pin(async move {
            let mut plan = plan(context, name, reservation);
            if self.bad_employee { plan.employee = EmployeeId([8; 16]); }
            Ok(plan)
        })
    }
}

fn plan(context: &RequestContext, name: &str, run: RunHandle) -> SpawnPolicy {
    SpawnPolicy {
        run, class: if name == "skill_extract" { ExecutionClass::ParseWorker } else { ExecutionClass::EmployeeTool },
        employee: context.record().employee, runtime: RuntimeKind::Registered(Text::new("tool-worker".into()).unwrap()),
        argv_allowlist: vec![], environment_allowlist: BTreeMap::new(),
        workspace: WorkspacePolicy { workspace: Text::new("work".into()).unwrap(), relative_cwd: Text::new(".".into()).unwrap() },
        resources: ResourceLimits { memory_bytes: 1024, cpu_quota_percent: 100, tasks: 2, open_files: 32, ptys: 0,
            disk: DiskQuota::LoopFilesystem { bytes: 1024, inodes: 32 }, io_read_bytes_per_sec: 1024,
            io_write_bytes_per_sec: 1024, io_read_ops_per_sec: 10, io_write_ops_per_sec: 10,
            output_buffer_bytes: 1024, output_total_bytes: 1024, memfd_total_bytes: 1024 },
        network: NetworkPolicy::Offline, pty: false,
        capabilities: CapabilityHandover { mcp_bridge: false, egress_forwarder: false },
        timeouts: RunTimeouts { queue_ms: 1000, startup_ms: 1000, runtime_ms: 1000, pty_idle_ms: None, termination_grace_ms: 100 },
        inputs: RunInputs { prompt: RunInput { reader: Box::new(EmptyInput), max_bytes: 1024 }, attachments: vec![] },
    }
}

#[tokio::test]
async fn isolated_execution_handler_refusal_precedes_reservation() {
    let backend = Arc::new(Backend::default());
    let bridge = ExecutionToolBridge::new(backend.clone(), Arc::new(Factory { refuse: true, ..Default::default() }));
    let result = bridge.execute(&context("execute_program"), "execute_program", &json!({})).await;
    assert_eq!(result["isError"], true);
    assert_eq!(backend.reserved.load(Ordering::SeqCst), 0);
    assert_eq!(backend.spawned.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn isolated_execution_bad_plan_cancels_without_spawn() {
    let backend = Arc::new(Backend::default());
    let bridge = ExecutionToolBridge::new(backend.clone(), Arc::new(Factory { bad_employee: true, ..Default::default() }));
    assert_eq!(bridge.execute(&context("execute_program"), "execute_program", &json!({})).await["isError"], true);
    assert_eq!(backend.spawned.load(Ordering::SeqCst), 0);
    assert_eq!(backend.killed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn isolated_execution_spawn_failure_is_uncertain_and_cancelled() {
    let backend = Arc::new(Backend { fail_spawn: true, ..Default::default() });
    let bridge = ExecutionToolBridge::new(backend.clone(), Arc::new(Factory::default()));
    let result = bridge.execute(&context("execute_program"), "execute_program", &json!({})).await;
    assert_eq!(result["execution_status"], "uncertain");
    assert_eq!(backend.killed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn isolated_execution_returns_queued_handle_without_waiting() {
    for tool in ["execute_program"] {
        let backend = Arc::new(Backend::default());
        let bridge = ExecutionToolBridge::new(backend.clone(), Arc::new(Factory::default()));
        let result = bridge.execute(&context(tool), tool, &json!({})).await;
        assert_eq!(result["isError"], false);
        let body: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["status"], "accepted");
        assert_eq!(body["completion"], "pending");
        assert_eq!(backend.spawned.load(Ordering::SeqCst), 1);
        assert_eq!(backend.waited.load(Ordering::SeqCst), 0);
        assert_eq!(backend.killed.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn isolated_execution_never_cancels_a_forged_caller_reservation() {
    let backend = Arc::new(Backend { return_caller: true, ..Default::default() });
    let bridge = ExecutionToolBridge::new(backend.clone(), Arc::new(Factory::default()));
    assert_eq!(bridge.execute(&context("execute_program"), "execute_program", &json!({})).await["isError"], true);
    assert_eq!(backend.spawned.load(Ordering::SeqCst), 0);
    assert_eq!(backend.killed.load(Ordering::SeqCst), 0);
}

#[test]
fn isolated_execution_plan_requires_registered_runtime_workspace_and_class() {
    let context = context("execute_program");
    let run = RunHandle(RunId([9; 16]));
    let mut policy = plan(&context, "execute_program", run);
    assert!(valid_plan(&context, Route::Execution, run, &policy));
    policy.runtime = RuntimeKind::Claude;
    assert!(!valid_plan(&context, Route::Execution, run, &policy));
    policy = plan(&context, "execute_program", run);
    policy.workspace.workspace = Text::new("other".into()).unwrap();
    assert!(!valid_plan(&context, Route::Execution, run, &policy));
    policy = plan(&context, "execute_program", run);
    policy.workspace.relative_cwd = Text::new("../other".into()).unwrap();
    assert!(!valid_plan(&context, Route::Execution, run, &policy));
    policy = plan(&context, "execute_program", run);
    assert!(!valid_plan(&context, Route::Worker, run, &policy));
    policy = plan(&context, "skill_extract", run);
    policy.network = NetworkPolicy::Egress(duduclaw_isolation::egress::ProfileId(Sha256Digest([0; 32])));
    assert!(!valid_plan(&context, Route::Worker, run, &policy));
    let bridge = ExecutionToolBridge::new(Arc::new(Backend::default()), Arc::new(Factory::default()));
    for name in ["memory_search", "unknown", "execute_program_extra", "skill_extract"] { assert!(!bridge.supports(name)); }
}

struct PendingFactory { preparing: tokio::sync::Notify }
impl ExecutionPlanFactory for PendingFactory {
    fn supports(&self, name: &str) -> bool { name == "execute_program" }
    fn authorize_handler<'a>(&'a self, _: &'a RequestContext, _: &'a str, _: &'a Value) -> PolicyFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn prepare<'a>(&'a self, _: &'a RequestContext, _: &'a str, _: &'a Value,
        _: RunHandle) -> PolicyFuture<'a, SpawnPolicy> {
        Box::pin(async move { self.preparing.notify_one(); std::future::pending().await })
    }
}

#[tokio::test]
async fn isolated_execution_aborted_preparation_cancels_reserved_run() {
    let backend = Arc::new(Backend::default());
    let factory = Arc::new(PendingFactory { preparing: tokio::sync::Notify::new() });
    let bridge = ExecutionToolBridge::new(backend.clone(), factory.clone());
    let task = tokio::spawn(async move { bridge.execute(&context("execute_program"), "execute_program", &json!({})).await });
    factory.preparing.notified().await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(1), async {
        while backend.killed.load(Ordering::SeqCst) == 0 { tokio::task::yield_now().await; }
    }).await.unwrap();
    assert_eq!(backend.spawned.load(Ordering::SeqCst), 0);
}
