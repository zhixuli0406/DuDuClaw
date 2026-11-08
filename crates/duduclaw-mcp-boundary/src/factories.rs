//! Trusted boot assembly shared by the gateway and its CLI-hosted dispatcher.
//! Factories are injected once into this immutable value, never selected by MCP.
use std::sync::Arc;
use duduclaw_isolation::{bounded::{List, Text}, execution::ExecutionBackend,
    worker::{ParseRequest, ParseResponse, ParseResult, ParsedItem, MAX_RESULT_TEXT_BYTES}};
use serde_json::Value;
use crate::{VerificationError, execution::{ExecutionPlanFactory, ExecutionToolBridge, PolicyFuture},
    parsing::{FileParser, TableOptions}, resources::AuthorizedFile, tool_policy::{AuthorizedToolCall, ResourceResolver}};

/// Pass this value to each dispatcher before admitting the first protected run.
/// Missing A or E deliberately leaves the corresponding routes unavailable.
#[derive(Default)]
pub struct GatewayFactories {
    #[cfg(feature = "binding-store")]
    pub forks: Option<Arc<dyn crate::forks::ForkToolService>>,
    pub execution: Option<Arc<ExecutionToolBridge>>,
    pub parser: Option<Arc<dyn FileParser>>,
    pub worker: Option<Arc<crate::worker_execution::WorkerToolBridge>>,
}

impl GatewayFactories {
    pub fn assemble(backend: Option<Arc<dyn ExecutionBackend>>,
        plans: Option<Arc<dyn ExecutionPlanFactory>>, resolver: Arc<dyn ResourceResolver>,
        parser: Option<Arc<dyn FileParser>>, roots: Arc<crate::resources::ResourceRoots>) -> Result<Self, VerificationError> {
        let execution = match (backend, plans) {
            (Some(backend), Some(plans)) => Some(Arc::new(ExecutionToolBridge::new(backend, Arc::new(RootedPlans { plans, roots }), resolver))),
            (None, None) => None,
            _ => return Err(VerificationError::Unavailable),
        };
        Ok(Self { execution, parser, worker: None, #[cfg(feature = "binding-store")] forks: None })
    }

    pub fn with_execution_jobs(mut self, jobs: Arc<dyn crate::execution_jobs::ExecutionJobs>) -> Self {
        if let Some(bridge) = self.execution.take() {
            self.execution = Some(Arc::new(bridge.with_jobs(jobs)));
        }
        self
    }

    #[cfg(feature = "binding-store")]
    pub fn with_forks(mut self, forks: Arc<dyn crate::forks::ForkToolService>) -> Self {
        self.forks = Some(forks); self
    }

    pub fn with_worker(mut self, worker: Arc<crate::worker_execution::WorkerToolBridge>) -> Self {
        self.worker = Some(worker); self
    }
}

/// Supplies only boot-owned roots; authorization and final plan/resource checks
/// remain in ExecutionToolBridge, including after producer awaits.
struct RootedPlans {
    plans: Arc<dyn ExecutionPlanFactory>,
    roots: Arc<crate::resources::ResourceRoots>,
}
impl ExecutionPlanFactory for RootedPlans {
    fn supports(&self, name: &str) -> bool { self.plans.supports(name) }
    fn collects_results(&self, name: &str) -> bool { self.plans.collects_results(name) }
    fn cooperative_stop(&self, name: &str) -> bool { self.plans.cooperative_stop(name) }
    fn decode_result(&self, name: &str, stdout: &[u8], stderr: &[u8], outcome: &duduclaw_isolation::execution::RunOutcome)
        -> Result<Value, VerificationError> { self.plans.decode_result(name, stdout, stderr, outcome) }
    fn authorize_control<'a>(&'a self, context: &'a crate::RequestContext, name: &'a str, arguments: &'a Value)
        -> PolicyFuture<'a, ()> { self.plans.authorize_control(context, name, arguments) }
    fn authorize_handler<'a>(&'a self, call: &'a AuthorizedToolCall) -> PolicyFuture<'a, ()> {
        self.plans.authorize_handler(call)
    }
    fn prepare<'a>(&'a self, call: &'a AuthorizedToolCall, run: duduclaw_isolation::execution::RunHandle)
        -> PolicyFuture<'a, crate::execution::AuthorizedExecutionPlan> {
        self.plans.prepare_with_roots(call, run, self.roots.clone())
    }
    // HX5: the in-run helper path needs no roots (the helper uses the run's
    // own /work); forward it unchanged so the wrapper never hides it.
    fn prepare_in_run<'a>(&'a self, call: &'a AuthorizedToolCall)
        -> PolicyFuture<'a, crate::execution::AuthorizedHelperPlan> { self.plans.prepare_in_run(call) }
    fn decode_helper_result(&self, name: &str, stdout: &[u8], stderr: &[u8],
        outcome: &duduclaw_isolation::ipc::HelperOutcome) -> Result<Value, VerificationError> {
        self.plans.decode_helper_result(name, stdout, stderr, outcome)
    }
}

/// E's blocking client retains its admission/session lease through snapshot,
/// exchange and teardown. This function must use WorkerLauncher/WorkerSession;
/// it may not execute a parser or reopen a pathname in the gateway.
pub type WorkerTableCall = dyn Fn(ParseRequest, AuthorizedFile, TableOptions)
    -> Result<Value, VerificationError> + Send + Sync;

pub struct GatewayTableParser { invoke: Arc<WorkerTableCall> }
impl GatewayTableParser {
    pub fn new(invoke: Arc<WorkerTableCall>) -> Self { Self { invoke } }
}

impl FileParser for GatewayTableParser {
    fn parse<'a>(&'a self, call: AuthorizedToolCall, request: ParseRequest, mut input: AuthorizedFile,
        options: TableOptions) -> PolicyFuture<'a, ParseResponse> {
        Box::pin(async move {
            options.validate()?;
            request.validate().map_err(|_| VerificationError::Invalid)?;
            input.verify(&call)?;
            let expected = crate::parsing::authorized_table_request(&input, &options)?;
            if request.operation != expected.operation || request.inputs != expected.inputs
                || request.outputs != expected.outputs || request.limits != expected.limits {
                return Err(VerificationError::Invalid);
            }
            let invoke = self.invoke.clone();
            // A dropped/expired request fences queued work and future I/O, but
            // is not proof of worker teardown. E retains its lease and deadline.
            let invocation = crate::resources::InvocationGuard::new();
            input.bind_invocation(&invocation)?;
            let original = request.clone();
            let checked_options = options.clone();
            let live = call.context().clone();
            let value = tokio::task::spawn_blocking(move || {
                input.verify(&call)?;
                input.verify_live()?;
                invoke(request, input, options)
            })
                .await.map_err(|_| VerificationError::Unavailable)??;
            live.verify_live(crate::authority::now_seconds()?)?;
            let response = encode_table_result(&original, value)?;
            crate::parsing::decode_table_response(&original, &checked_options, response.clone())?;
            Ok(response)
        })
    }
}

fn encode_table_result(request: &ParseRequest, value: Value) -> Result<ParseResponse, VerificationError> {
    let encoded = serde_json::to_string(&value).map_err(|_| VerificationError::Invalid)?;
    if encoded.len() > MAX_RESULT_TEXT_BYTES { return Err(VerificationError::Invalid); }
    let mut chunks = Vec::new();
    let mut chunk = String::new();
    for ch in encoded.chars() {
        if chunk.len() + ch.len_utf8() > 8192 {
            chunks.push(ParsedItem::Text { input_slot: 0, page: None,
                text: Text::new(std::mem::take(&mut chunk)).map_err(|_| VerificationError::Invalid)? });
        }
        chunk.push(ch);
    }
    if !chunk.is_empty() {
        chunks.push(ParsedItem::Text { input_slot: 0, page: None,
            text: Text::new(chunk).map_err(|_| VerificationError::Invalid)? });
    }
    Ok(ParseResponse { version: request.version, request_id: request.request_id,
        result: ParseResult::Complete { items: List::new(chunks).map_err(|_| VerificationError::Invalid)? } })
}

#[cfg(test)]
#[path = "factories_tests.rs"]
mod tests;
