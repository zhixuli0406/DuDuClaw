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
    collecting: bool,
    io_attempts: AtomicUsize,
    stop_bytes: Option<Arc<std::sync::Mutex<Vec<u8>>>>,
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
        Box::pin(async move {
            self.waited.fetch_add(1, Ordering::SeqCst);
            if self.collecting { tokio::time::sleep(Duration::from_millis(300)).await; Ok(RunOutcome::Exited(0)) }
            else { Err(BackendError::WouldDeadlock) }
        })
    }
    fn kill(&self, run: RunHandle) -> BackendFuture<'_, ()> {
        Box::pin(async move {
            assert_eq!(run, RunHandle(RunId([9; 16])));
            self.killed.fetch_add(1, Ordering::SeqCst); Ok(())
        })
    }
    fn take_stdio(&self, _: RunHandle) -> BackendFuture<'_, RunStdio> {
        Box::pin(async move {
            if !self.collecting { return Err(BackendError::Denied); }
            if self.io_attempts.fetch_add(1, Ordering::SeqCst) < 2 { return Err(BackendError::Unavailable); }
            let stdin: Box<dyn RunWriter> = match &self.stop_bytes {
                Some(bytes) => Box::new(RecordingWriter(bytes.clone())), None => Box::new(EmptyWriter),
            };
            Ok(RunStdio { stdin, stdout: Box::new(EmptyInput), stderr: Box::new(EmptyInput) })
        })
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
struct EmptyWriter;
impl RunWriter for EmptyWriter {
    fn write<'a>(&'a mut self, bytes: &'a [u8]) -> BackendFuture<'a, usize> { Box::pin(async move { Ok(bytes.len()) }) }
    fn close(&mut self) -> BackendFuture<'_, ()> { Box::pin(async { Ok(()) }) }
}

#[derive(Default)]
struct Factory { refuse: bool, bad_employee: bool, collects: bool }
impl ExecutionPlanFactory for Factory {
    fn supports(&self, name: &str) -> bool { matches!(name, "execute_program" | "skill_extract") }
    fn collects_results(&self, _: &str) -> bool { self.collects }
    fn decode_result(&self, _: &str, stdout: &[u8], stderr: &[u8], outcome: &RunOutcome) -> Result<Value, VerificationError> {
        if !stdout.is_empty() || !stderr.is_empty() || *outcome != RunOutcome::Exited(0) { return Err(VerificationError::Invalid); }
        Ok(json!({"status":"finished"}))
    }
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
        workspace_delivery: None, run, class: if name == "skill_extract" { ExecutionClass::ParseWorker } else { ExecutionClass::EmployeeTool },
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

#[cfg(feature = "binding-store")]
#[tokio::test]
async fn isolated_execution_collector_retries_queued_io_without_cancelling() {
    use crate::execution_jobs::{ExecutionJobs, SqliteExecutionJobs};
    let home = tempfile::tempdir().unwrap();
    let jobs = SqliteExecutionJobs::open(&home.path().join("jobs.db")).unwrap();
    let backend = Arc::new(Backend { collecting: true, ..Default::default() });
    let bridge = ExecutionToolBridge::new(backend.clone(), Arc::new(Factory { collects: true, ..Default::default() }))
        .with_jobs(jobs.clone());
    let owner = context("execute_program");
    let result = bridge.execute(&owner, "execute_program", &json!({})).await;
    assert_eq!(result["isError"], false);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let state = jobs.read(&owner, "execute_program", RunHandle(RunId([9; 16]))).unwrap();
            if state["status"] == "complete" { assert_eq!(state["result"]["status"], "finished"); break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    assert!(backend.io_attempts.load(Ordering::SeqCst) >= 3);
    assert_eq!(backend.killed.load(Ordering::SeqCst), 0);
}

#[cfg(feature = "binding-store")]
#[tokio::test]
async fn isolated_execution_failed_spawn_persists_uncertainty_and_job_selector() {
    use crate::execution_jobs::{ExecutionJobs, SqliteExecutionJobs};
    let home = tempfile::tempdir().unwrap();
    let jobs = SqliteExecutionJobs::open(&home.path().join("jobs.db")).unwrap();
    let backend = Arc::new(Backend { fail_spawn: true, ..Default::default() });
    let bridge = ExecutionToolBridge::new(backend, Arc::new(Factory { collects: true, ..Default::default() }))
        .with_jobs(jobs.clone());
    let owner = context("execute_program");
    let result = bridge.execute(&owner, "execute_program", &json!({})).await;
    assert_eq!(result["execution_status"], "uncertain");
    assert!(result["content"][0]["text"].as_str().unwrap().contains("09090909"));
    assert_eq!(jobs.read(&owner, "execute_program", RunHandle(RunId([9; 16]))).unwrap()["status"], "uncertain");
}

#[cfg(feature = "binding-store")]
#[tokio::test]
async fn isolated_execution_aborted_insert_cannot_leave_a_pending_record() {
    use crate::execution_jobs::{ExecutionJobs, SqliteExecutionJobs};
    struct BlockingJobs {
        store: Arc<SqliteExecutionJobs>, started: tokio::sync::Notify,
        release: (std::sync::Mutex<bool>, std::sync::Condvar),
    }
    impl ExecutionJobs for BlockingJobs {
        fn insert(&self, context: &RequestContext, tool: &str, run: RunHandle) -> Result<(), VerificationError> {
            self.started.notify_one();
            let (lock, ready) = &self.release;
            let mut release = lock.lock().unwrap();
            while !*release { release = ready.wait(release).unwrap(); }
            self.store.insert(context, tool, run)
        }
        fn read(&self, context: &RequestContext, tool: &str, run: RunHandle) -> Result<Value, VerificationError> {
            self.store.read(context, tool, run)
        }
        fn finish(&self, run: RunHandle, result: Value) -> Result<(), VerificationError> { self.store.finish(run, result) }
        fn cancel_requested(&self, context: &RequestContext, tool: &str, run: RunHandle) -> Result<(), VerificationError> {
            self.store.cancel_requested(context, tool, run)
        }
    }
    let home = tempfile::tempdir().unwrap();
    let jobs = Arc::new(BlockingJobs { store: SqliteExecutionJobs::open(&home.path().join("jobs.db")).unwrap(),
        started: tokio::sync::Notify::new(), release: (std::sync::Mutex::new(false), std::sync::Condvar::new()) });
    let backend = Arc::new(Backend::default());
    let bridge = ExecutionToolBridge::new(backend.clone(), Arc::new(Factory { collects: true, ..Default::default() }))
        .with_jobs(jobs.clone());
    let request = tokio::spawn(async move { bridge.execute(&context("execute_program"), "execute_program", &json!({})).await });
    jobs.started.notified().await;
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    *jobs.release.0.lock().unwrap() = true;
    jobs.release.1.notify_all();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if jobs.read(&context("execute_program"), "execute_program", RunHandle(RunId([9; 16])))
                .is_ok_and(|result| result["status"] == "uncertain") { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    assert_eq!(backend.spawned.load(Ordering::SeqCst), 0);
}

struct RecordingFactory;
impl ExecutionPlanFactory for RecordingFactory {
    fn supports(&self, name: &str) -> bool { matches!(name, "browser_record_start" | "browser_record_stop") }
    fn collects_results(&self, _: &str) -> bool { true }
    fn cooperative_stop(&self, name: &str) -> bool { name == "browser_record_start" }
    fn decode_result(&self, _: &str, _: &[u8], _: &[u8], _: &RunOutcome) -> Result<Value, VerificationError> {
        Ok(json!({"id":"09".repeat(16),"status":"recorded"}))
    }
    fn authorize_handler<'a>(&'a self, _: &'a RequestContext, _: &'a str, _: &'a Value) -> PolicyFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn prepare<'a>(&'a self, context: &'a RequestContext, name: &'a str, _: &'a Value, run: RunHandle) -> PolicyFuture<'a, SpawnPolicy> {
        Box::pin(async move { Ok(plan(context, name, run)) })
    }
}

#[cfg(feature = "binding-store")]
#[tokio::test]
async fn isolated_recording_stop_is_owned_durable_control_without_reservation() {
    use crate::execution_jobs::{ExecutionJobs, SqliteExecutionJobs};
    let home = tempfile::tempdir().unwrap();
    let jobs = SqliteExecutionJobs::open(&home.path().join("jobs.db")).unwrap();
    let run = RunHandle(RunId([9; 16]));
    let owner = context("browser_record_start");
    jobs.insert(&owner, "browser_record_start", run).unwrap();
    let backend = Arc::new(Backend::default());
    let bridge = ExecutionToolBridge::new(backend.clone(), Arc::new(RecordingFactory)).with_jobs(jobs.clone());
    let stop = context("browser_record_stop");
    let args = json!({"id":"09".repeat(16)});
    let stopped = bridge.execute(&stop, "browser_record_stop", &args).await;
    assert_ne!(stopped["isError"], true, "{stopped}");
    assert_eq!(jobs.read(&owner, "browser_record_start", run).unwrap()["status"], "stop_requested");
    assert!(jobs.stop_requested(run).unwrap());
    assert_eq!(backend.reserved.load(Ordering::SeqCst), 0);
    assert_eq!(backend.spawned.load(Ordering::SeqCst), 0);
    assert_eq!(backend.killed.load(Ordering::SeqCst), 0);
    let mut thief = stop.record().clone();
    thief.principal = Principal::Human { user_id: Text::new("thief".into()).unwrap(), generation: 1 };
    assert_eq!(bridge.execute(&RequestContext::from_verified_record(thief), "browser_record_stop", &args).await["isError"], true);
    for args in [json!({"id":"09".repeat(16),"action":"cancel"}), json!({"id":"../recording"}), json!({"id":"03".repeat(16)})] {
        assert_eq!(bridge.execute(&stop, "browser_record_stop", &args).await["isError"], true);
    }
    jobs.finish(run, json!({"status":"recorded"})).unwrap();
    assert_ne!(bridge.execute(&stop, "browser_record_stop", &args).await["isError"], true);
    assert_eq!(jobs.read(&owner, "browser_record_start", run).unwrap()["status"], "complete");
}

#[cfg(feature = "binding-store")]
#[tokio::test]
async fn isolated_recording_stop_cannot_target_program_job_or_use_start_capability() {
    use crate::execution_jobs::{ExecutionJobs, SqliteExecutionJobs};
    let home = tempfile::tempdir().unwrap();
    let jobs = SqliteExecutionJobs::open(&home.path().join("jobs.db")).unwrap();
    let run = RunHandle(RunId([9; 16]));
    jobs.insert(&context("execute_program"), "execute_program", run).unwrap();
    let backend = Arc::new(Backend::default());
    let bridge = ExecutionToolBridge::new(backend.clone(), Arc::new(RecordingFactory)).with_jobs(jobs);
    let args = json!({"id":"09".repeat(16)});
    for caller in [context("browser_record_stop"), context("browser_record_start")] {
        assert_eq!(bridge.execute(&caller, "browser_record_stop", &args).await["isError"], true);
    }
    assert_eq!(backend.reserved.load(Ordering::SeqCst), 0);
    assert_eq!(backend.killed.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn isolated_recording_stop_writer_handles_partial_writes_and_refuses_zero_progress() {
    struct Writer { bytes: Vec<u8>, zero: bool, closed: bool }
    impl RunWriter for Writer {
        fn write<'a>(&'a mut self, bytes: &'a [u8]) -> BackendFuture<'a, usize> {
            Box::pin(async move { if self.zero { return Ok(0); } self.bytes.push(bytes[0]); Ok(1) })
        }
        fn close(&mut self) -> BackendFuture<'_, ()> { Box::pin(async move { self.closed = true; Ok(()) }) }
    }
    let mut good = Writer { bytes: vec![], zero: false, closed: false };
    write_stop(&mut good).await.unwrap();
    assert_eq!(good.bytes, b"stop\n"); assert!(good.closed);
    let mut stalled = Writer { bytes: vec![], zero: true, closed: false };
    assert!(write_stop(&mut stalled).await.is_err()); assert!(!stalled.closed);
}

struct RecordingWriter(Arc<std::sync::Mutex<Vec<u8>>>);
impl RunWriter for RecordingWriter {
    fn write<'a>(&'a mut self, bytes: &'a [u8]) -> BackendFuture<'a, usize> {
        Box::pin(async move { self.0.lock().unwrap().extend_from_slice(bytes); Ok(bytes.len()) })
    }
    fn close(&mut self) -> BackendFuture<'_, ()> { Box::pin(async { Ok(()) }) }
}
#[cfg(feature = "binding-store")]
#[tokio::test]
async fn isolated_recording_collector_delivers_persisted_stop_and_only_then_collects_terminal_result() {
    use crate::execution_jobs::{ExecutionJobs, SqliteExecutionJobs};
    let home = tempfile::tempdir().unwrap();
    let jobs = SqliteExecutionJobs::open(&home.path().join("jobs.db")).unwrap();
    let bytes = Arc::new(std::sync::Mutex::new(vec![]));
    let backend = Arc::new(Backend { collecting: true, stop_bytes: Some(bytes.clone()), ..Default::default() });
    let bridge = ExecutionToolBridge::new(backend.clone(), Arc::new(RecordingFactory)).with_jobs(jobs.clone());
    let response = bridge.execute(&context("browser_record_start"), "browser_record_start", &json!({})).await;
    assert_ne!(response["isError"], true, "{response}");
    let response = bridge.execute(&context("browser_record_stop"), "browser_record_stop", &json!({"id":"09".repeat(16)})).await;
    assert_ne!(response["isError"], true, "{response}");
    assert_eq!(jobs.read(&context("browser_record_start"), "browser_record_start", RunHandle(RunId([9; 16]))).unwrap()["status"], "stop_requested");
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let result = jobs.read(&context("browser_record_start"), "browser_record_start", RunHandle(RunId([9; 16]))).unwrap();
            if result["status"] == "complete" { assert_eq!(result["result"]["status"], "recorded"); break; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.unwrap();
    assert_eq!(*bytes.lock().unwrap(), b"stop\n");
    assert_eq!(backend.killed.load(Ordering::SeqCst), 0);
}
