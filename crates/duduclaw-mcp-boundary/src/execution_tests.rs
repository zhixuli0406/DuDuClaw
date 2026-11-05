use super::*;
use std::{collections::BTreeMap, sync::atomic::{AtomicUsize, Ordering}};
use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord,
    EmployeeBinding, Lane, Principal, ResourceGrant}, execution::*, ids::{EmployeeId, RunId, Sha256Digest}};

struct TestLive;
impl duduclaw_isolation::capability::LiveRequestVerifier for TestLive {
    fn verify_live(&self, _: &CapabilityRecord, _: u64) -> Result<(), VerificationError> { Ok(()) }
}
fn context(tool: &str) -> RequestContext {
    RequestContext::with_live_verifier(CapabilityRecord {
        id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([2; 16]), run: RunId([3; 16]),
        task: None, principal: Principal::Human { user_id: Text::new("owner".into()).unwrap(), generation: 1 },
        employee_binding: EmployeeBinding::Personal(Text::new("owner".into()).unwrap()), lane: Lane::Standard,
        tools: List::new(vec![Text::new(tool.into()).unwrap()]).unwrap(),
        resources: List::new(vec![ResourceGrant::Employee { id: EmployeeId([2; 16]) },
            ResourceGrant::Workspace { handle: Text::new("work".into()).unwrap() },
            ResourceGrant::Workspace { handle: Text::new("other".into()).unwrap() }]).unwrap(),
        policy_revision: 1, gateway_epoch: [4; 16], issued_at: 1, not_before: 1, expires_at: u64::MAX - 1,
        nonce: [5; 32], jti: [6; 16],
    }, Arc::new(TestLive))
}

struct Resolver;
impl ResourceResolver for Resolver {
    fn resolve(&self, context: &RequestContext, _: &tool_policy::ToolPolicy, _: &Value)
        -> Result<Vec<ResourceGrant>, VerificationError> {
        Ok(vec![ResourceGrant::Employee { id: context.record().employee },
            ResourceGrant::Workspace { handle: Text::new("work".into()).unwrap() }])
    }
}
fn authorized_call(context: &RequestContext) -> AuthorizedToolCall {
    tool_policy::authorize(context, "execute_program", &json!({}), &Resolver).unwrap()
}
fn bind_plan(call: &AuthorizedToolCall, policy: SpawnPolicy) -> AuthorizedExecutionPlan {
    let directory = test_workspace();
    let mut roots = crate::resources::ResourceRoots::default();
    roots.insert("work".into(), call.context().record().employee, directory).unwrap();
    AuthorizedExecutionPlan::new(roots.authorize_workspace(call, "work").unwrap(), policy).unwrap()
}

fn test_workspace() -> std::fs::File {
    static DIRECTORY: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    std::fs::File::open(DIRECTORY.get_or_init(|| tempfile::tempdir().unwrap()).path()).unwrap()
}

#[derive(Default)]
struct Backend {
    reserved: AtomicUsize,
    spawned: AtomicUsize,
    killed: AtomicUsize,
    waited: AtomicUsize,
    fail_spawn: bool,
    return_caller: bool,
    wrong_workspace: bool,
    legacy_only: bool,
}
impl ExecutionBackend for Backend {
    fn supports_authorized_workspace(&self) -> bool { !self.legacy_only }
    fn spawn_authorized(&self, policy: SpawnPolicy, authority: Arc<dyn ExecutionAuthority>) -> BackendFuture<'_, RunHandle> {
        Box::pin(async move {
            let wrong = tempfile::tempdir().unwrap();
            let directory = if self.wrong_workspace { std::fs::File::open(wrong.path()).unwrap() } else { test_workspace() };
            authority.verify_workspace(policy.workspace.workspace.as_str(), &directory)?;
            authority.verify_live()?;
            self.spawn(policy).await
        })
    }
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
    fn authorize_handler<'a>(&'a self, _: &'a AuthorizedToolCall) -> PolicyFuture<'a, ()> {
        Box::pin(async move { if self.refuse { Err(VerificationError::Invalid) } else { Ok(()) } })
    }
    fn prepare<'a>(&'a self, call: &'a AuthorizedToolCall,
        reservation: RunHandle) -> PolicyFuture<'a, AuthorizedExecutionPlan> {
        Box::pin(async move {
            let mut plan = plan(call.context(), call.name(), reservation);
            if self.bad_employee { plan.employee = EmployeeId([8; 16]); }
            Ok(bind_plan(call, plan))
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
    let bridge = ExecutionToolBridge::new(backend.clone(), Arc::new(Factory { refuse: true, ..Default::default() }), Arc::new(Resolver));
    let result = bridge.execute(&context("execute_program"), "execute_program", &json!({})).await;
    assert_eq!(result["isError"], true);
    assert_eq!(backend.reserved.load(Ordering::SeqCst), 0);
    assert_eq!(backend.spawned.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn isolated_execution_bad_plan_cancels_without_spawn() {
    let backend = Arc::new(Backend::default());
    let bridge = ExecutionToolBridge::new(backend.clone(), Arc::new(Factory { bad_employee: true, ..Default::default() }), Arc::new(Resolver));
    assert_eq!(bridge.execute(&context("execute_program"), "execute_program", &json!({})).await["isError"], true);
    assert_eq!(backend.spawned.load(Ordering::SeqCst), 0);
    assert_eq!(backend.killed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn isolated_execution_spawn_failure_is_uncertain_and_cancelled() {
    let backend = Arc::new(Backend { fail_spawn: true, ..Default::default() });
    let bridge = ExecutionToolBridge::new(backend.clone(), Arc::new(Factory::default()), Arc::new(Resolver));
    let result = bridge.execute(&context("execute_program"), "execute_program", &json!({})).await;
    assert_eq!(result["execution_status"], "uncertain");
    assert_eq!(backend.killed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn isolated_execution_returns_queued_handle_without_waiting() {
    for tool in ["execute_program"] {
        let backend = Arc::new(Backend::default());
        let bridge = ExecutionToolBridge::new(backend.clone(), Arc::new(Factory::default()), Arc::new(Resolver));
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
    let bridge = ExecutionToolBridge::new(backend.clone(), Arc::new(Factory::default()), Arc::new(Resolver));
    assert_eq!(bridge.execute(&context("execute_program"), "execute_program", &json!({})).await["isError"], true);
    assert_eq!(backend.spawned.load(Ordering::SeqCst), 0);
    assert_eq!(backend.killed.load(Ordering::SeqCst), 0);
}

#[test]
fn isolated_execution_plan_requires_registered_runtime_workspace_and_class() {
    let context = context("execute_program");
    let call = authorized_call(&context);
    let run = RunHandle(RunId([9; 16]));
    let mut authorized = bind_plan(&call, plan(&context, "execute_program", run));
    assert!(valid_plan(&call, Route::Execution, run, &authorized));
    authorized.policy.runtime = RuntimeKind::Claude;
    assert!(!valid_plan(&call, Route::Execution, run, &authorized));
    authorized.policy = plan(&context, "execute_program", run);
    // Both workspaces are capability grants: only THIS call's work is valid.
    authorized.policy.workspace.workspace = Text::new("other".into()).unwrap();
    assert!(!valid_plan(&call, Route::Execution, run, &authorized));
    authorized.policy = plan(&context, "execute_program", run);
    authorized.policy.workspace.relative_cwd = Text::new("../other".into()).unwrap();
    assert!(!valid_plan(&call, Route::Execution, run, &authorized));
    authorized.policy = plan(&context, "execute_program", run);
    assert!(!valid_plan(&call, Route::Worker, run, &authorized));
    authorized.policy = plan(&context, "skill_extract", run);
    authorized.policy.network = NetworkPolicy::Egress(duduclaw_isolation::egress::ProfileId(Sha256Digest([0; 32])));
    assert!(!valid_plan(&call, Route::Worker, run, &authorized));
    let bridge = ExecutionToolBridge::new(Arc::new(Backend::default()), Arc::new(Factory::default()), Arc::new(Resolver));
    for name in ["memory_search", "unknown", "execute_program_extra", "skill_extract"] { assert!(!bridge.supports(name)); }
}

#[test]
fn isolated_execution_plan_cannot_be_built_from_another_selected_workspace() {
    let context = context("execute_program");
    let call = authorized_call(&context);
    let directory = tempfile::tempdir().unwrap();
    let mut roots = crate::resources::ResourceRoots::default();
    roots.insert("other".into(), context.record().employee, std::fs::File::open(directory.path()).unwrap()).unwrap();
    assert!(roots.authorize_workspace(&call, "other").is_err());
    let mut policy = plan(&context, "execute_program", RunHandle(RunId([9; 16])));
    policy.workspace.workspace = Text::new("other".into()).unwrap();
    roots.insert("work".into(), context.record().employee, std::fs::File::open(directory.path()).unwrap()).unwrap();
    assert!(AuthorizedExecutionPlan::new(roots.authorize_workspace(&call, "work").unwrap(), policy).is_err());
}

struct PendingFactory { preparing: tokio::sync::Notify }
impl ExecutionPlanFactory for PendingFactory {
    fn supports(&self, name: &str) -> bool { name == "execute_program" }
    fn authorize_handler<'a>(&'a self, _: &'a AuthorizedToolCall) -> PolicyFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn prepare<'a>(&'a self, _: &'a AuthorizedToolCall,
        _: RunHandle) -> PolicyFuture<'a, AuthorizedExecutionPlan> {
        Box::pin(async move { self.preparing.notify_one(); std::future::pending().await })
    }
}

#[tokio::test]
async fn isolated_execution_aborted_preparation_cancels_reserved_run() {
    let backend = Arc::new(Backend::default());
    let factory = Arc::new(PendingFactory { preparing: tokio::sync::Notify::new() });
    let bridge = ExecutionToolBridge::new(backend.clone(), factory.clone(), Arc::new(Resolver));
    let task = tokio::spawn(async move { bridge.execute(&context("execute_program"), "execute_program", &json!({})).await });
    factory.preparing.notified().await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(1), async {
        while backend.killed.load(Ordering::SeqCst) == 0 { tokio::task::yield_now().await; }
    }).await.unwrap();
    assert_eq!(backend.spawned.load(Ordering::SeqCst), 0);
}

struct ChangingResolver { calls: AtomicUsize, refuse: bool }
impl ResourceResolver for ChangingResolver {
    fn resolve(&self, context: &RequestContext, _: &tool_policy::ToolPolicy, _: &Value)
        -> Result<Vec<ResourceGrant>, VerificationError> {
        if self.refuse { return Err(VerificationError::Unavailable); }
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(vec![ResourceGrant::Employee { id: context.record().employee },
            ResourceGrant::Workspace { handle: Text::new(if call == 0 { "work" } else { "other" }.into()).unwrap() }])
    }
}

#[tokio::test]
async fn isolated_execution_resource_refusal_precedes_reservation() {
    let backend = Arc::new(Backend::default());
    let bridge = ExecutionToolBridge::new(backend.clone(), Arc::new(Factory::default()),
        Arc::new(ChangingResolver { calls: AtomicUsize::new(0), refuse: true }));
    assert_eq!(bridge.execute(&context("execute_program"), "execute_program", &json!({})).await["isError"], true);
    assert_eq!(backend.reserved.load(Ordering::SeqCst), 0);
    assert_eq!(backend.spawned.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn isolated_execution_resource_change_after_prepare_cancels_before_spawn() {
    let backend = Arc::new(Backend::default());
    let bridge = ExecutionToolBridge::new(backend.clone(), Arc::new(Factory::default()),
        Arc::new(ChangingResolver { calls: AtomicUsize::new(0), refuse: false }));
    assert_eq!(bridge.execute(&context("execute_program"), "execute_program", &json!({})).await["isError"], true);
    assert_eq!(backend.reserved.load(Ordering::SeqCst), 1);
    assert_eq!(backend.spawned.load(Ordering::SeqCst), 0);
    assert_eq!(backend.killed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn legacy_backend_is_not_ready_and_never_receives_a_protected_spawn() {
    let backend = Arc::new(Backend { legacy_only: true, ..Default::default() });
    let bridge = ExecutionToolBridge::new(backend.clone(), Arc::new(Factory::default()), Arc::new(Resolver));
    assert!(!bridge.supports("execute_program"));
    assert_eq!(bridge.execute(&context("execute_program"), "execute_program", &json!({})).await["isError"], true);
    assert_eq!(backend.reserved.load(Ordering::SeqCst), 0);
    assert_eq!(backend.spawned.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn backend_cannot_substitute_a_same_named_different_workspace_root() {
    let backend = Arc::new(Backend { wrong_workspace: true, ..Default::default() });
    let bridge = ExecutionToolBridge::new(backend.clone(), Arc::new(Factory::default()), Arc::new(Resolver));
    assert_eq!(bridge.execute(&context("execute_program"), "execute_program", &json!({})).await["isError"], true);
    assert_eq!(backend.spawned.load(Ordering::SeqCst), 0);
    assert_eq!(backend.killed.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn execution_resource_rechecks_run_off_reactor_and_refuse_revocation() {
    struct ThreadCheckedResolver {
        reactor: std::thread::ThreadId,
        calls: AtomicUsize,
        revoke_on_recheck: bool,
    }
    impl ResourceResolver for ThreadCheckedResolver {
        fn resolve(&self, context: &RequestContext, policy: &tool_policy::ToolPolicy, arguments: &Value)
            -> Result<Vec<ResourceGrant>, VerificationError> {
            assert_ne!(std::thread::current().id(), self.reactor);
            let count = self.calls.fetch_add(1, Ordering::SeqCst);
            if self.revoke_on_recheck && count > 0 { return Err(VerificationError::Revoked); }
            Resolver.resolve(context, policy, arguments)
        }
    }
    for revoke_on_recheck in [false, true] {
        let backend = Arc::new(Backend::default());
        let resolver = Arc::new(ThreadCheckedResolver { reactor: std::thread::current().id(),
            calls: AtomicUsize::new(0), revoke_on_recheck });
        let bridge = ExecutionToolBridge::new(backend.clone(), Arc::new(Factory::default()), resolver.clone());
        let result = bridge.execute(&context("execute_program"), "execute_program", &json!({})).await;
        assert_eq!(resolver.calls.load(Ordering::SeqCst), if revoke_on_recheck { 2 } else { 3 });
        assert_eq!(result["isError"], revoke_on_recheck);
        assert_eq!(backend.spawned.load(Ordering::SeqCst), usize::from(!revoke_on_recheck));
        assert_eq!(backend.killed.load(Ordering::SeqCst), usize::from(revoke_on_recheck));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn execution_resource_change_during_final_handler_check_cancels_before_spawn() {
    use std::sync::atomic::AtomicBool;
    struct FinalCheckFactory { checks: AtomicUsize, changed: Arc<AtomicBool> }
    impl ExecutionPlanFactory for FinalCheckFactory {
        fn supports(&self, name: &str) -> bool { name == "execute_program" }
        fn authorize_handler<'a>(&'a self, _: &'a AuthorizedToolCall) -> PolicyFuture<'a, ()> {
            Box::pin(async move {
                if self.checks.fetch_add(1, Ordering::SeqCst) == 1 {
                    tokio::task::yield_now().await;
                    self.changed.store(true, Ordering::SeqCst);
                }
                Ok(())
            })
        }
        fn prepare<'a>(&'a self, call: &'a AuthorizedToolCall, reservation: RunHandle)
            -> PolicyFuture<'a, AuthorizedExecutionPlan> {
            Box::pin(async move { Ok(bind_plan(call, plan(call.context(), call.name(), reservation))) })
        }
    }
    struct FinalCheckResolver { changed: Arc<AtomicBool>, calls: AtomicUsize }
    impl ResourceResolver for FinalCheckResolver {
        fn resolve(&self, context: &RequestContext, policy: &tool_policy::ToolPolicy, arguments: &Value)
            -> Result<Vec<ResourceGrant>, VerificationError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut resources = Resolver.resolve(context, policy, arguments)?;
            if self.changed.load(Ordering::SeqCst) {
                // The selected workspace is still present; changing any part of
                // the call's authorized resource set must invalidate its token.
                resources.push(ResourceGrant::Workspace { handle: Text::new("other".into()).unwrap() });
            }
            Ok(resources)
        }
    }
    let changed = Arc::new(AtomicBool::new(false));
    let backend = Arc::new(Backend::default());
    let factory = Arc::new(FinalCheckFactory { checks: AtomicUsize::new(0), changed: changed.clone() });
    let resolver = Arc::new(FinalCheckResolver { changed, calls: AtomicUsize::new(0) });
    let bridge = ExecutionToolBridge::new(backend.clone(), factory.clone(), resolver.clone());
    let result = bridge.execute(&context("execute_program"), "execute_program", &json!({})).await;
    assert_eq!(result["isError"], true);
    assert_eq!(factory.checks.load(Ordering::SeqCst), 2);
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 3);
    assert_eq!(backend.reserved.load(Ordering::SeqCst), 1);
    assert_eq!(backend.killed.load(Ordering::SeqCst), 1);
    assert_eq!(backend.spawned.load(Ordering::SeqCst), 0);
}
