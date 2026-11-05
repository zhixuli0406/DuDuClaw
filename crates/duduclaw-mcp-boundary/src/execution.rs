//! Gateway-side handoff to Package A. No OS process is started here.
//!
//! This bridge does not replace handler authorization. An integration must supply
//! the existing handler's policy checks and its trusted operation-to-plan adapter;
//! knowing a tool's effect class alone is never sufficient to execute it.
use std::{future::Future, pin::Pin, sync::{Arc, atomic::{AtomicBool, Ordering}}, time::Duration};
use duduclaw_isolation::{capability::ResourceGrant, execution::{
    ExecutionBackend, ExecutionClass, NetworkPolicy, RunHandle, RuntimeKind, SpawnPolicy,
}};
use crate::{RequestContext, VerificationError, tool_policy::{self, Route}};
use serde_json::{Value, json};

pub type PolicyFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, VerificationError>> + Send + 'a>>;

/// Trusted Package A / handler integration, never constructed from MCP input.
/// Implementations must reuse the tool handler's ownership, record, audience,
/// device and approval checks. `authorize_handler` must not perform the effect.
/// `prepare` rechecks that authorization after reservation and converts the
/// validated operation to a root-registered runtime plus bounded fd inputs.
/// Arbitrary employee argv, environment and host paths cannot become policy.
/// There is intentionally no default implementation or blanket support claim.
pub trait ExecutionPlanFactory: Send + Sync {
    fn supports(&self, tool: &str) -> bool;
    fn collects_results(&self, _tool: &str) -> bool { false }
    fn cooperative_stop(&self, _tool: &str) -> bool { false }
    fn decode_result(&self, _tool: &str, _stdout: &[u8], _stderr: &[u8],
        _outcome: &duduclaw_isolation::execution::RunOutcome) -> Result<Value, VerificationError> {
        Err(VerificationError::Invalid)
    }
    fn authorize_control<'a>(&'a self, context: &'a RequestContext, tool: &'a str,
        _arguments: &'a Value) -> PolicyFuture<'a, ()> {
        Box::pin(async move { if tool_policy::eligible(context, tool) { Ok(()) } else { Err(VerificationError::Invalid) } })
    }
    fn authorize_handler<'a>(&'a self, context: &'a RequestContext, tool: &'a str,
        arguments: &'a Value) -> PolicyFuture<'a, ()>;
    fn prepare<'a>(&'a self, context: &'a RequestContext, tool: &'a str,
        arguments: &'a Value, reservation: RunHandle) -> PolicyFuture<'a, SpawnPolicy>;
}

pub struct ExecutionToolBridge {
    backend: Arc<dyn ExecutionBackend>,
    factory: Arc<dyn ExecutionPlanFactory>,
    jobs: Option<Arc<dyn crate::execution_jobs::ExecutionJobs>>,
}

impl ExecutionToolBridge {
    pub fn new(backend: Arc<dyn ExecutionBackend>, factory: Arc<dyn ExecutionPlanFactory>) -> Self {
        Self { backend, factory, jobs: None }
    }

    pub fn with_jobs(&self, jobs: Arc<dyn crate::execution_jobs::ExecutionJobs>) -> Self {
        Self { backend: self.backend.clone(), factory: self.factory.clone(), jobs: Some(jobs) }
    }

    /// Use this same predicate for tools/list and tools/call.
    pub fn supports(&self, name: &str) -> bool {
        execution_route(name).is_some() && self.factory.supports(name)
            && (!self.factory.collects_results(name) || self.jobs.is_some())
    }

    /// Called only after the shared MCP dispatcher gates. Same-employee runs
    /// cannot synchronously wait for a child under D8 admission: report the
    /// accepted handle, never fabricate the old synchronous tool result.
    pub async fn execute(&self, context: &RequestContext, name: &str, arguments: &Value) -> Value {
        let Some(route) = execution_route(name) else { return refused(); };
        if name == "browser_record_stop" { return self.stop_recording(context, arguments).await; }
        if self.factory.collects_results(name) && arguments.get("action").is_some() {
            return self.control(context, name, arguments).await;
        }
        if !self.supports(name) || !tool_policy::eligible(context, name)
            || crate::check_arguments(arguments).is_err()
            || self.factory.authorize_handler(context, name, arguments).await.is_err() {
            return refused();
        }
        let reservation = match self.backend.reserve(context.record().employee).await {
            Ok(run) if run.0 != context.record().run => run,
            // A broken backend returning the caller must never cause us to kill
            // the caller as though it were a newly reserved child.
            _ => return refused(),
        };
        let mut guard = ReservationGuard::new(self.backend.clone(), reservation);
        let plan = match self.factory.prepare(context, name, arguments, reservation).await {
            Ok(plan) if valid_plan(context, route, reservation, &plan) => plan,
            _ => return if guard.cancel().await { refused() } else { uncertain() },
        };
        if self.factory.collects_results(name) {
            let Some(jobs) = &self.jobs else { return refused(); };
            guard.jobs = Some(jobs.clone());
            let abandoned = guard.abandoned.clone();
            let (jobs, context, name) = (jobs.clone(), context.clone(), name.to_owned());
            if crate::authority::verify_blocking(move || {
                jobs.insert(&context, &name, reservation)?;
                // Drop may race this blocking transaction. If cleanup ran
                // before INSERT, the inserter must publish uncertainty itself.
                if abandoned.load(Ordering::Acquire) {
                    let _ = jobs.finish(reservation, json!({"status":"uncertain"}));
                }
                Ok(())
            }).await.is_err() {
                return if guard.cancel().await { refused() } else { uncertain() };
            }
        }
        match self.backend.spawn(plan).await {
            Ok(run) if run == reservation => {
                guard.disarm();
                if let Some(jobs) = self.jobs.as_ref().filter(|_| self.factory.collects_results(name)) {
                    let (backend, factory, jobs, name) = (self.backend.clone(), self.factory.clone(), jobs.clone(), name.to_owned());
                    tokio::spawn(async move { collect(backend, factory, jobs, name, run).await; });
                }
                json!({"content":[{"type":"text", "text":json!({
                    "status":"accepted", "completion":"pending", "id":crate::execution_jobs::handle_text(run), "job":crate::execution_jobs::handle_text(run), "run_handle":run.0.0.iter().map(|byte| format!("{byte:02x}")).collect::<String>(),
                    "message":"工作已交付執行佇列；尚未確認執行完成。"
                }).to_string()}], "isError":false})
            }
            // Even an error may follow admission. Cancel the reservation before
            // returning, and do not claim a confirmed pre-execution rejection.
            _ => { let _ = guard.cancel().await;
                if self.factory.collects_results(name) { uncertain_job(reservation) } else { uncertain() } }
        }
    }

    /// Exact operation alias: a stop capability can stop this actor's recording,
    /// never a program or a run selected solely by handle possession.
    async fn stop_recording(&self, context: &RequestContext, arguments: &Value) -> Value {
        let name = "browser_record_stop";
        if !self.supports(name) || !tool_policy::eligible(context, name)
            || crate::check_arguments(arguments).is_err()
            || self.factory.authorize_control(context, name, arguments).await.is_err() { return refused(); }
        let Some(fields) = arguments.as_object() else { return refused(); };
        if fields.keys().any(|key| !matches!(key.as_str(), "id" | "workspace")) { return refused(); }
        let Some(run) = arguments["id"].as_str().and_then(|id| crate::execution_jobs::parse_handle(id).ok()) else { return refused(); };
        if run.0 == context.record().run { return refused(); }
        let Some(jobs) = &self.jobs else { return refused(); };
        let (jobs, context) = (jobs.clone(), context.clone());
        match crate::authority::verify_blocking(move || {
            let current = jobs.read(&context, "browser_record_start", run)?;
            if current["status"] == "pending" { jobs.request_stop(&context, "browser_record_start", run)?; }
            jobs.read(&context, "browser_record_start", run)
        }).await {
            Ok(result) => result_envelope(result), Err(_) => refused(),
        }
    }

    /// Host-authority control over an authenticated boundary connection. It does
    /// not reserve a child, invent a plan, or synchronously wait for an employee.
    async fn control(&self, context: &RequestContext, name: &str, arguments: &Value) -> Value {
        if !self.supports(name) || crate::check_arguments(arguments).is_err()
            || self.factory.authorize_control(context, name, arguments).await.is_err() { return refused(); }
        let Some(fields) = arguments.as_object() else { return refused(); };
        if fields.keys().any(|key| !matches!(key.as_str(), "action" | "job" | "workspace")) { return refused(); }
        let Some(action @ ("status" | "cancel")) = arguments["action"].as_str() else { return refused(); };
        let Some(run) = arguments["job"].as_str().and_then(|value| crate::execution_jobs::parse_handle(value).ok()) else { return refused(); };
        if run.0 == context.record().run { return refused(); }
        let Some(jobs) = &self.jobs else { return refused(); };
        let (store, ctx, tool) = (jobs.clone(), context.clone(), name.to_owned());
        let result = match crate::authority::verify_blocking(move || store.read(&ctx, &tool, run)).await {
            Ok(result) => result, Err(_) => return refused(),
        };
        if action == "status" || result["status"] == "complete" { return result_envelope(result); }
        // A Cancel acknowledgement means requested, never retired. The host
        // collector alone publishes the terminal outcome after wait and EOF.
        if self.backend.kill(run).await.is_err() { return uncertain(); }
        let (store, ctx, tool) = (jobs.clone(), context.clone(), name.to_owned());
        if crate::authority::verify_blocking(move || store.cancel_requested(&ctx, &tool, run)).await.is_err() { return uncertain(); }
        result_envelope(json!({"job":crate::execution_jobs::handle_text(run), "status":"cancel_requested"}))
    }
}

fn result_envelope(result: Value) -> Value {
    json!({"content":[{"type":"text","text":result.to_string()}], "isError":false})
}

async fn read_result(mut reader: Box<dyn duduclaw_isolation::execution::RunReader>, limit: usize)
    -> Result<Vec<u8>, duduclaw_isolation::execution::BackendError> {
    let mut output = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 { return Ok(output); }
        if count > buffer.len() || output.len().saturating_add(count) > limit {
            return Err(duduclaw_isolation::execution::BackendError::Protocol);
        }
        output.extend_from_slice(&buffer[..count]);
    }
}
async fn collect(backend: Arc<dyn ExecutionBackend>, factory: Arc<dyn ExecutionPlanFactory>,
    jobs: Arc<dyn crate::execution_jobs::ExecutionJobs>, name: String, run: RunHandle) {
    let waiter_backend = backend.clone();
    let mut waiter = tokio::spawn(async move {
        waiter_backend.wait(run, duduclaw_isolation::execution::WaitOrigin::Gateway).await
    });
    let observed = async {
        let mut early_outcome = None;
        let mut io = loop {
            match backend.take_stdio(run).await {
                Ok(io) => break io,
                Err(duduclaw_isolation::execution::BackendError::Unavailable) => {
                    if waiter.is_finished() {
                        let outcome = (&mut waiter).await.map_err(|_| duduclaw_isolation::execution::BackendError::Unavailable)??;
                        if !matches!(outcome, duduclaw_isolation::execution::RunOutcome::Exited(_)
                            | duduclaw_isolation::execution::RunOutcome::Signalled(_)) {
                            return Ok((vec![], vec![], outcome));
                        }
                        early_outcome = Some(outcome);
                        break backend.take_stdio(run).await?;
                    }
                    // Queued under D8 is normal, not a transport failure. Do
                    // not cancel an in-flight TakeIo RPC with select/timeout.
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(error) => return Err(error),
            }
        };
        if !factory.cooperative_stop(&name) { io.stdin.close().await?; }
        let outcome = async {
            match early_outcome { Some(outcome) => Ok(outcome),
                None => (&mut waiter).await.map_err(|_| duduclaw_isolation::execution::BackendError::Unavailable)?, }
        };
        let drain = async {
            tokio::try_join!(read_result(io.stdout, 1024 * 1024), read_result(io.stderr, 64 * 1024), outcome)
        };
        tokio::pin!(drain);
        if factory.cooperative_stop(&name) {
            let stop = async {
                loop {
                    let (jobs, run) = (jobs.clone(), run);
                    let requested = crate::authority::verify_blocking(move || jobs.stop_requested(run)).await
                        .map_err(|_| duduclaw_isolation::execution::BackendError::Unavailable)?;
                    if requested { break; }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                write_stop(&mut *io.stdin).await
            };
            tokio::pin!(stop);
            tokio::select! {
                result = &mut drain => result,
                sent = &mut stop => { sent?; drain.await }
            }
        } else { drain.await }
    };
    // Root enforces the runtime deadline. This bounds a broken transport too;
    // timeout/cancellation acknowledgement alone never becomes completion.
    let result = match tokio::time::timeout(Duration::from_secs(1200), observed).await {
        Ok(Ok((stdout, stderr, outcome))) => match factory.decode_result(&name, &stdout, &stderr, &outcome) {
            Ok(value) if name != "browser_record_start" || value["id"] == crate::execution_jobs::handle_text(run) => value,
            _ => json!({"status":"invalid_result", "outcome":outcome}),
        },
        _ => {
            let _ = tokio::time::timeout(Duration::from_secs(5), backend.kill(run)).await;
            json!({"status":"uncertain"})
        }
    };
    if !waiter.is_finished() { waiter.abort(); }
    let _ = crate::authority::verify_blocking(move || jobs.finish(run, result)).await;
}

async fn write_stop(writer: &mut dyn duduclaw_isolation::execution::RunWriter)
    -> Result<(), duduclaw_isolation::execution::BackendError> {
    let bytes = b"stop\n";
    let mut offset = 0;
    while offset < bytes.len() {
        let count = writer.write(&bytes[offset..]).await?;
        if count == 0 || count > bytes.len() - offset { return Err(duduclaw_isolation::execution::BackendError::Protocol); }
        offset += count;
    }
    writer.close().await
}

fn execution_route(name: &str) -> Option<Route> {
    if matches!(name, "hardware_info" | "model_search" | "model_recommend") {
        return Some(Route::Execution);
    }
    match tool_policy::policy(name)?.route {
        Route::Execution => Some(Route::Execution),
        _ => None,
    }
}

fn valid_plan(context: &RequestContext, route: Route, reservation: RunHandle, plan: &SpawnPolicy) -> bool {
    let expected = match route {
        Route::Execution => ExecutionClass::EmployeeTool,
        Route::Worker => ExecutionClass::ParseWorker,
        _ => return false,
    };
    plan.run == reservation && plan.employee == context.record().employee
        && plan.class == expected && matches!(&plan.runtime, RuntimeKind::Registered(_))
        && (!matches!(route, Route::Worker) || matches!(&plan.network, NetworkPolicy::Offline))
        && !plan.pty && !plan.capabilities.mcp_bridge
        && (plan.workspace.relative_cwd.as_str() == "."
            || crate::resources::relative_path(plan.workspace.relative_cwd.as_str()).is_ok())
        && context.record().resources.as_slice().iter().any(|grant|
            matches!(grant, ResourceGrant::Workspace { handle } if handle == &plan.workspace.workspace))
}

fn refused() -> Value {
    json!({"content":[{"type":"text","text":"無法確認執行授權，這次操作未交付執行。"}], "isError":true})
}

fn uncertain() -> Value {
    json!({"content":[{"type":"text","text":"無法確認執行狀態，請先查看工作狀態再決定是否重試。"}],
        "isError":true, "execution_status":"uncertain"})
}

fn uncertain_job(run: RunHandle) -> Value {
    json!({"content":[{"type":"text","text":json!({"status":"uncertain",
        "job":crate::execution_jobs::handle_text(run)}).to_string()}], "isError":true,
        "execution_status":"uncertain"})
}

/// Also revoke if the caller disconnects while awaiting plan construction or
/// spawn. A remains responsible for connection-loss cleanup and reservation TTL;
/// this best-effort Drop cleanup is an additional cancellation attempt.
struct ReservationGuard {
    backend: Arc<dyn ExecutionBackend>,
    run: RunHandle,
    armed: bool,
    jobs: Option<Arc<dyn crate::execution_jobs::ExecutionJobs>>,
    abandoned: Arc<AtomicBool>,
}
impl ReservationGuard {
    fn new(backend: Arc<dyn ExecutionBackend>, run: RunHandle) -> Self {
        Self { backend, run, armed: true, jobs: None, abandoned: Arc::new(AtomicBool::new(false)) }
    }
    fn disarm(&mut self) { self.armed = false; }
    async fn cancel(&mut self) -> bool {
        let cancelled = matches!(tokio::time::timeout(Duration::from_secs(5), self.backend.kill(self.run)).await,
            Ok(Ok(())));
        if let Some(jobs) = &self.jobs {
            self.abandoned.store(true, Ordering::Release);
            let (jobs, run) = (jobs.clone(), self.run);
            let _ = crate::authority::verify_blocking(move || jobs.finish(run, json!({"status":"uncertain"}))).await;
        }
        if cancelled { self.disarm(); }
        cancelled
    }
}
impl Drop for ReservationGuard {
    fn drop(&mut self) {
        if !self.armed { return; }
        self.abandoned.store(true, Ordering::Release);
        let backend = self.backend.clone();
        let run = self.run;
        let jobs = self.jobs.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = tokio::time::timeout(Duration::from_secs(5), backend.kill(run)).await;
                if let Some(jobs) = jobs {
                    let _ = crate::authority::verify_blocking(move || jobs.finish(run, json!({"status":"uncertain"}))).await;
                }
            });
        }
    }
}

#[cfg(test)]
#[path = "execution_tests.rs"]
mod tests;
