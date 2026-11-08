//! HX5 in-run helper path (D8 revision §3): synchronous result, transport and
//! target run checks, `(run, call)` cancel only, typed per-call results.
use super::*;
use std::sync::{Mutex, atomic::{AtomicBool, AtomicUsize, Ordering}};
use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord, EmployeeBinding, Lane,
    LiveRequestVerifier, Principal, ResourceGrant}, execution::{BackendError, BackendFuture, HelperFuture, PtySize,
    RunOutcome, RunPty, RunReader, RunStdio, SpawnPolicy, WaitOrigin}, ids::{RunId, Sha256Digest}};
use crate::{execution::{ExecutionPlanFactory, PolicyFuture}, tool_policy::{AuthorizedToolCall, ResourceResolver}};

const CALLER: RunId = RunId([3; 16]);
const EMPLOYEE: EmployeeId = EmployeeId([2; 16]);
fn call_id() -> HelperCallId { HelperCallId::new([7; 16]).unwrap() }

struct Live(Arc<AtomicBool>);
impl LiveRequestVerifier for Live {
    fn verify_live(&self, _: &CapabilityRecord, _: u64) -> Result<(), VerificationError> {
        if self.0.load(Ordering::SeqCst) { Ok(()) } else { Err(VerificationError::Revoked) }
    }
}
fn record(tool: &str) -> CapabilityRecord {
    CapabilityRecord {
        id: CapabilityId(Sha256Digest([1; 32])), employee: EMPLOYEE, run: CALLER, task: None,
        principal: Principal::Human { user_id: Text::new("owner".into()).unwrap(), generation: 1 },
        employee_binding: EmployeeBinding::Personal(Text::new("owner".into()).unwrap()), lane: Lane::Standard,
        tools: List::new(vec![Text::new(tool.into()).unwrap()]).unwrap(),
        resources: List::new(vec![ResourceGrant::Employee { id: EMPLOYEE },
            ResourceGrant::Workspace { handle: Text::new("employee-work".into()).unwrap() }]).unwrap(),
        policy_revision: 1, gateway_epoch: [4; 16], issued_at: 1, not_before: 1, expires_at: u64::MAX - 1,
        nonce: [5; 32], jti: [6; 16],
    }
}
/// A request that arrived on the caller run's own MCP transport.
fn transport_context(tool: &str, live: Arc<AtomicBool>) -> RequestContext {
    RequestContext::with_transport(record(tool), Arc::new(Live(live)), CALLER)
}
fn live() -> Arc<AtomicBool> { Arc::new(AtomicBool::new(true)) }

struct Resolver;
impl ResourceResolver for Resolver {
    fn resolve(&self, context: &RequestContext, _: &tool_policy::ToolPolicy, _: &Value)
        -> Result<Vec<ResourceGrant>, VerificationError> {
        Ok(vec![ResourceGrant::Employee { id: context.record().employee },
            ResourceGrant::Workspace { handle: Text::new("employee-work".into()).unwrap() }])
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Wait { Exit, Pending, RunEnded, NotLive, Uncertain }

struct Backend {
    features: SpawndFeatures,
    implements: bool,
    wait: Wait,
    pending_output: bool,
    started: AtomicUsize,
    waited: AtomicUsize,
    cancelled: AtomicUsize,
    killed: AtomicUsize,
    reserved: AtomicUsize,
    plans: Mutex<Vec<(RunHandle, EmployeeId)>>,
}
impl Backend {
    fn new(wait: Wait) -> Self {
        Self { features: SpawndFeatures::HELPER_CALLS, implements: true, wait, pending_output: false,
            started: AtomicUsize::new(0), waited: AtomicUsize::new(0), cancelled: AtomicUsize::new(0),
            killed: AtomicUsize::new(0), reserved: AtomicUsize::new(0), plans: Mutex::new(vec![]) }
    }
    fn count(counter: &AtomicUsize) -> usize { counter.load(Ordering::SeqCst) }
}
struct Bytes(Option<Vec<u8>>);
impl RunReader for Bytes {
    fn read<'a>(&'a mut self, buffer: &'a mut [u8]) -> BackendFuture<'a, usize> {
        Box::pin(async move {
            match self.0.take() {
                Some(bytes) => { buffer[..bytes.len()].copy_from_slice(&bytes); Ok(bytes.len()) }
                None => Ok(0),
            }
        })
    }
}
struct Never;
impl RunReader for Never {
    fn read<'a>(&'a mut self, _: &'a mut [u8]) -> BackendFuture<'a, usize> { Box::pin(std::future::pending()) }
}
impl ExecutionBackend for Backend {
    fn reserve(&self, _: EmployeeId) -> BackendFuture<'_, RunHandle> {
        self.reserved.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(BackendError::Denied) })
    }
    fn spawn(&self, _: SpawnPolicy) -> BackendFuture<'_, RunHandle> { Box::pin(async { Err(BackendError::Denied) }) }
    fn wait(&self, _: RunHandle, _: WaitOrigin) -> BackendFuture<'_, RunOutcome> { Box::pin(async { Err(BackendError::Denied) }) }
    // [F8] Never reachable from the in-run path: counted to prove it.
    fn kill(&self, _: RunHandle) -> BackendFuture<'_, ()> {
        self.killed.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
    fn kill_authorized(&self, _: RunHandle, _: Arc<dyn duduclaw_isolation::execution::CancelAuthority>)
        -> BackendFuture<'_, ()> {
        self.killed.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
    fn take_stdio(&self, _: RunHandle) -> BackendFuture<'_, RunStdio> { Box::pin(async { Err(BackendError::Denied) }) }
    fn take_pty(&self, _: RunHandle) -> BackendFuture<'_, RunPty> { Box::pin(async { Err(BackendError::Denied) }) }
    fn resize(&self, _: RunHandle, _: PtySize) -> BackendFuture<'_, ()> { Box::pin(async { Err(BackendError::Denied) }) }
    fn supports_helper_calls(&self) -> bool { self.implements }
    fn helper_features(&self, run: RunHandle) -> SpawndFeatures {
        if run.0 == CALLER { self.features } else { SpawndFeatures::NONE }
    }
    fn start_helper(&self, plan: duduclaw_isolation::execution::HelperPlan) -> HelperFuture<'_, HelperStart> {
        Box::pin(async move {
            self.started.fetch_add(1, Ordering::SeqCst);
            self.plans.lock().unwrap().push((plan.run, plan.employee));
            assert_eq!(plan.slot, HelperSlot::Foreground);
            let stdout: Box<dyn RunReader> = if self.pending_output { Box::new(Never) }
                else { Box::new(Bytes(Some(b"hello".to_vec()))) };
            Ok(HelperStart { call: call_id(), effective_timeout_ms: 30_000, stdout, stderr: Box::new(Bytes(None)) })
        })
    }
    fn wait_helper(&self, run: RunHandle, call: HelperCallId) -> HelperFuture<'_, HelperOutcome> {
        Box::pin(async move {
            assert_eq!((run.0, call), (CALLER, call_id()));
            self.waited.fetch_add(1, Ordering::SeqCst);
            match self.wait {
                Wait::Exit => Ok(HelperOutcome::Exited(0)),
                Wait::RunEnded => Ok(HelperOutcome::RunEnded),
                Wait::NotLive => Err(HelperError::NotLive),
                Wait::Uncertain => Err(HelperError::Uncertain),
                Wait::Pending => std::future::pending().await,
            }
        })
    }
    fn cancel_helper(&self, run: RunHandle, call: HelperCallId) -> HelperFuture<'_, ()> {
        Box::pin(async move {
            // Only ever the (run, call) of this job, never a run cancel.
            assert_eq!((run.0, call), (CALLER, call_id()));
            self.cancelled.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}

#[derive(Default)]
struct Factory { target: Option<RunHandle>, employee: Option<EmployeeId> }
impl ExecutionPlanFactory for Factory {
    fn supports(&self, name: &str) -> bool { tool_policy::IN_RUN_HELPER_TOOLS.contains(&name) }
    fn authorize_handler<'a>(&'a self, _: &'a AuthorizedToolCall) -> PolicyFuture<'a, ()> { Box::pin(async { Ok(()) }) }
    fn prepare<'a>(&'a self, _: &'a AuthorizedToolCall, _: RunHandle) -> PolicyFuture<'a, crate::execution::AuthorizedExecutionPlan> {
        panic!("in-run tools never prepare a child-run plan")
    }
    fn prepare_in_run<'a>(&'a self, call: &'a AuthorizedToolCall) -> PolicyFuture<'a, AuthorizedHelperPlan> {
        Box::pin(async move {
            let record = call.context().record();
            AuthorizedHelperPlan::new(self.target.unwrap_or(RunHandle(record.run)), self.employee.unwrap_or(record.employee),
                RuntimeKind::Registered(Text::new("duduclaw-program-v1".into()).unwrap()), 60_000, b"{}".to_vec(), 1024)
        })
    }
    fn decode_helper_result(&self, _: &str, stdout: &[u8], stderr: &[u8], outcome: &HelperOutcome)
        -> Result<Value, VerificationError> {
        if !stderr.is_empty() || *outcome != HelperOutcome::Exited(0) { return Err(VerificationError::Invalid); }
        Ok(json!({"status":"exited", "stdout": String::from_utf8_lossy(stdout)}))
    }
}

fn bridge(backend: &Arc<Backend>, factory: Factory) -> ExecutionToolBridge {
    ExecutionToolBridge::new(backend.clone(), Arc::new(factory), Arc::new(Resolver))
}
fn body(result: &Value) -> Value {
    serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap_or(Value::Null)
}
async fn until(counter: &AtomicUsize, value: usize) {
    for _ in 0..200 {
        if counter.load(Ordering::SeqCst) >= value { return; }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("counter never reached {value}");
}

#[tokio::test]
async fn execute_program_returns_result_synchronously() {
    for tool in tool_policy::IN_RUN_HELPER_TOOLS {
        let backend = Arc::new(Backend::new(Wait::Exit));
        let result = bridge(&backend, Factory::default()).execute(&transport_context(tool, live()), tool, &json!({})).await;
        assert_eq!(result["isError"], false, "{tool}: {result}");
        let body = body(&result);
        assert_eq!(body["status"], "exited");
        assert_eq!(body["stdout"], "hello");
        assert_eq!(body["effective_timeout_ms"], 30_000);
        assert!(body.get("job").is_none() && body.get("completion").is_none(), "no pending job");
        assert_eq!(*backend.plans.lock().unwrap(), vec![(RunHandle(CALLER), EMPLOYEE)]);
        assert_eq!((Backend::count(&backend.started), Backend::count(&backend.waited)), (1, 1));
        assert_eq!((Backend::count(&backend.reserved), Backend::count(&backend.killed), Backend::count(&backend.cancelled)), (0, 0, 0));
    }
}

#[tokio::test]
async fn helper_target_must_equal_capability_run() {
    for factory in [Factory { target: Some(RunHandle(RunId([9; 16]))), ..Default::default() },
        Factory { employee: Some(EmployeeId([8; 16])), ..Default::default() }] {
        let backend = Arc::new(Backend::new(Wait::Exit));
        let result = bridge(&backend, factory).execute(&transport_context("execute_program", live()),
            "execute_program", &json!({})).await;
        assert_eq!(result["isError"], true);
        assert_eq!(Backend::count(&backend.started), 0, "a plan for another run/employee is never sent");
        assert_eq!(Backend::count(&backend.reserved), 0);
    }
}

#[tokio::test]
async fn transport_run_must_equal_record_run() {
    let backend = Arc::new(Backend::new(Wait::Exit));
    let other = RequestContext::with_transport(record("execute_program"), Arc::new(Live(live())), RunId([9; 16]));
    let result = bridge(&backend, Factory::default()).execute(&other, "execute_program", &json!({})).await;
    assert_eq!(result["isError"], true);
    assert_eq!(Backend::count(&backend.started), 0);
}

#[tokio::test]
async fn admin_context_cannot_issue_helper_call() {
    // Admin-RPC, delegated and record-only contexts carry no transport run.
    let backend = Arc::new(Backend::new(Wait::Exit));
    let bridge = bridge(&backend, Factory::default());
    for context in [RequestContext::with_live_verifier(record("execute_program"), Arc::new(Live(live()))),
        RequestContext::from_verified_record(record("execute_program"))] {
        assert_eq!(context.transport_run(), None);
        assert_eq!(bridge.execute(&context, "execute_program", &json!({})).await["isError"], true);
    }
    assert_eq!((Backend::count(&backend.started), Backend::count(&backend.reserved)), (0, 0));
}

#[tokio::test]
async fn in_run_job_cancel_never_kills_run() {
    // A transport/outcome failure after start cancels only the (run, call).
    let backend = Arc::new(Backend::new(Wait::Uncertain));
    let result = bridge(&backend, Factory::default()).execute(&transport_context("code_map", live()), "code_map", &json!({})).await;
    assert_eq!(result["execution_status"], "uncertain");
    assert_eq!(Backend::count(&backend.cancelled), 1);
    assert_eq!(Backend::count(&backend.killed), 0, "kill/kill_authorized(run) are unreachable for in-run jobs");
    let job = InRunJob { run: RunHandle(CALLER), call: call_id() };
    assert_eq!(job.key(), format!("{}:{}", crate::execution_jobs::handle_text(RunHandle(CALLER)), "07".repeat(16)));
    // The in-run tools are not child-run execution routes.
    for tool in tool_policy::IN_RUN_HELPER_TOOLS { assert!(super::super::execution_route(tool).is_none()); }
}

#[tokio::test]
async fn revocation_mid_call_kills_helper() {
    let mut backend = Backend::new(Wait::Pending);
    backend.pending_output = true;
    let backend = Arc::new(backend);
    let flag = live();
    let bridge = Arc::new(bridge(&backend, Factory::default()));
    let (task_bridge, context) = (bridge.clone(), transport_context("execute_program", flag.clone()));
    let task = tokio::spawn(async move { task_bridge.execute(&context, "execute_program", &json!({})).await });
    until(&backend.started, 1).await;
    flag.store(false, Ordering::SeqCst);
    let result = tokio::time::timeout(Duration::from_secs(5), task).await.unwrap().unwrap();
    assert_eq!(result["isError"], true);
    assert_eq!(Backend::count(&backend.cancelled), 1, "revocation sends HelperCancel");
    assert_eq!(Backend::count(&backend.killed), 0);
}

#[tokio::test]
async fn mcp_cancelled_maps_to_helper_cancel() {
    // server.rs drops the call future on `notifications/cancelled` for the
    // request and on connection close; the armed guard sends HelperCancel.
    let mut backend = Backend::new(Wait::Pending);
    backend.pending_output = true;
    let backend = Arc::new(backend);
    let bridge = Arc::new(bridge(&backend, Factory::default()));
    let (task_bridge, context) = (bridge.clone(), transport_context("office_script", live()));
    let task = tokio::spawn(async move { task_bridge.execute(&context, "office_script", &json!({})).await });
    until(&backend.started, 1).await;
    task.abort();
    let _ = task.await;
    until(&backend.cancelled, 1).await;
    assert_eq!(Backend::count(&backend.killed), 0);
    assert_eq!(Backend::count(&backend.reserved), 0);
    assert!(crate::server::cancels_request(
        &json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":4}}), &json!(4)));
}

#[tokio::test]
async fn run_end_mid_call_returns_run_ended() {
    for wait in [Wait::RunEnded, Wait::NotLive] {
        let backend = Arc::new(Backend::new(wait));
        let result = bridge(&backend, Factory::default()).execute(&transport_context("execute_program", live()),
            "execute_program", &json!({})).await;
        assert_eq!(result["isError"], true);
        assert_eq!(result["helper_status"], "run_ended");
        assert_eq!(body(&result)["status"], "run_ended");
        // No retry, nothing persisted, no cancel of an ended call.
        assert_eq!((Backend::count(&backend.started), Backend::count(&backend.cancelled)), (1, 0));
    }
}

#[tokio::test]
async fn gateway_refuses_helper_without_spawnd_feature() {
    let mut backend = Backend::new(Wait::Exit);
    backend.features = SpawndFeatures::WORK_BROKER;
    let backend = Arc::new(backend);
    let result = bridge(&backend, Factory::default()).execute(&transport_context("execute_program", live()),
        "execute_program", &json!({})).await;
    assert_eq!(result["isError"], true);
    assert_eq!(result["helper_status"], "helper_launcher_unavailable");
    assert_eq!(body(&result)["reason"], "spawnd_feature_missing");
    assert_eq!(Backend::count(&backend.started), 0);
    // A backend without helper support never lists or runs the tools.
    let mut legacy = Backend::new(Wait::Exit);
    legacy.implements = false;
    let legacy = Arc::new(legacy);
    let bridge = bridge(&legacy, Factory::default());
    assert!(!bridge.supports("execute_program"));
    assert_eq!(bridge.execute(&transport_context("execute_program", live()), "execute_program", &json!({})).await["isError"], true);
    assert_eq!((Backend::count(&legacy.started), Backend::count(&legacy.reserved)), (0, 0));
}
