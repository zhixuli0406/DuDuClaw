//! Gateway-side handoff to Package A. No OS process is started here.
//!
//! This bridge does not replace handler authorization. An integration must supply
//! the existing handler's policy checks and its trusted operation-to-plan adapter;
//! knowing a tool's effect class alone is never sufficient to execute it.
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};
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
    fn authorize_handler<'a>(&'a self, context: &'a RequestContext, tool: &'a str,
        arguments: &'a Value) -> PolicyFuture<'a, ()>;
    fn prepare<'a>(&'a self, context: &'a RequestContext, tool: &'a str,
        arguments: &'a Value, reservation: RunHandle) -> PolicyFuture<'a, SpawnPolicy>;
}

pub struct ExecutionToolBridge {
    backend: Arc<dyn ExecutionBackend>,
    factory: Arc<dyn ExecutionPlanFactory>,
}

impl ExecutionToolBridge {
    pub fn new(backend: Arc<dyn ExecutionBackend>, factory: Arc<dyn ExecutionPlanFactory>) -> Self {
        Self { backend, factory }
    }

    /// Use this same predicate for tools/list and tools/call.
    pub fn supports(&self, name: &str) -> bool {
        execution_route(name).is_some() && self.factory.supports(name)
    }

    /// Called only after the shared MCP dispatcher gates. Same-employee runs
    /// cannot synchronously wait for a child under D8 admission: report the
    /// accepted handle, never fabricate the old synchronous tool result.
    pub async fn execute(&self, context: &RequestContext, name: &str, arguments: &Value) -> Value {
        let Some(route) = execution_route(name) else { return refused(); };
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
        match self.backend.spawn(plan).await {
            Ok(run) if run == reservation => {
                guard.disarm();
                json!({"content":[{"type":"text", "text":json!({
                    "status":"accepted", "completion":"pending", "run_handle":run.0.0.iter().map(|byte| format!("{byte:02x}")).collect::<String>(),
                    "message":"工作已交付執行佇列；尚未確認執行完成。"
                }).to_string()}], "isError":false})
            }
            // Even an error may follow admission. Cancel the reservation before
            // returning, and do not claim a confirmed pre-execution rejection.
            _ => { let _ = guard.cancel().await; uncertain() }
        }
    }
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

/// Also revoke if the caller disconnects while awaiting plan construction or
/// spawn. A remains responsible for connection-loss cleanup and reservation TTL;
/// this best-effort Drop cleanup is an additional cancellation attempt.
struct ReservationGuard {
    backend: Arc<dyn ExecutionBackend>,
    run: RunHandle,
    armed: bool,
}
impl ReservationGuard {
    fn new(backend: Arc<dyn ExecutionBackend>, run: RunHandle) -> Self { Self { backend, run, armed: true } }
    fn disarm(&mut self) { self.armed = false; }
    async fn cancel(&mut self) -> bool {
        let cancelled = matches!(tokio::time::timeout(Duration::from_secs(5), self.backend.kill(self.run)).await,
            Ok(Ok(())));
        if cancelled { self.disarm(); }
        cancelled
    }
}
impl Drop for ReservationGuard {
    fn drop(&mut self) {
        if !self.armed { return; }
        let backend = self.backend.clone();
        let run = self.run;
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = tokio::time::timeout(Duration::from_secs(5), backend.kill(run)).await;
            });
        }
    }
}

#[cfg(test)]
#[path = "execution_tests.rs"]
mod tests;
