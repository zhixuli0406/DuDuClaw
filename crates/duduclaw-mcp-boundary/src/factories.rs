//! Trusted boot assembly shared by the gateway and its CLI-hosted dispatcher.
//! Factories are injected once into this immutable value, never selected by MCP.
use std::{fs::File, sync::Arc};
use duduclaw_isolation::{bounded::{List, Text}, execution::{ExecutionBackend, RunHandle, SpawnPolicy},
    worker::{ParseRequest, ParseResponse, ParseResult, ParsedItem, MAX_RESULT_TEXT_BYTES}};
use serde_json::Value;
use crate::{RequestContext, VerificationError, execution::{ExecutionPlanFactory, ExecutionToolBridge, PolicyFuture},
    parsing::{FileParser, TableOptions}, tool_policy::{self, ResourceResolver}};

/// Pass this value to each dispatcher before admitting the first protected run.
/// Missing A or E deliberately leaves the corresponding routes unavailable.
#[derive(Default)]
pub struct GatewayFactories {
    pub execution: Option<Arc<ExecutionToolBridge>>,
    pub parser: Option<Arc<dyn FileParser>>,
    pub worker: Option<Arc<crate::worker_execution::WorkerToolBridge>>,
}

impl GatewayFactories {
    pub fn assemble(backend: Option<Arc<dyn ExecutionBackend>>,
        plans: Option<Arc<dyn ExecutionPlanFactory>>, resolver: Arc<dyn ResourceResolver>,
        parser: Option<Arc<dyn FileParser>>) -> Result<Self, VerificationError> {
        let execution = match (backend, plans) {
            (Some(backend), Some(plans)) => Some(Arc::new(ExecutionToolBridge::new(backend,
                Arc::new(ResourceCheckedPlans { plans, resolver })))),
            (None, None) => None,
            _ => return Err(VerificationError::Unavailable),
        };
        Ok(Self { execution, parser, worker: None })
    }

    pub fn with_worker(mut self, worker: Arc<crate::worker_execution::WorkerToolBridge>) -> Self {
        self.worker = Some(worker); self
    }
}

/// The injected handler factory chooses the registered operation and supplies
/// bounded inputs. D independently verifies resources before and after awaits.
struct ResourceCheckedPlans {
    plans: Arc<dyn ExecutionPlanFactory>,
    resolver: Arc<dyn ResourceResolver>,
}

impl ExecutionPlanFactory for ResourceCheckedPlans {
    fn supports(&self, tool: &str) -> bool { self.plans.supports(tool) }
    fn authorize_handler<'a>(&'a self, context: &'a RequestContext, tool: &'a str,
        arguments: &'a Value) -> PolicyFuture<'a, ()> {
        Box::pin(async move {
            let before = tool_policy::authorize(context, tool, arguments, self.resolver.as_ref())?;
            self.plans.authorize_handler(context, tool, arguments).await?;
            let after = tool_policy::authorize(context, tool, arguments, self.resolver.as_ref())?;
            if before.resources() != after.resources() { return Err(VerificationError::BindingMismatch); }
            Ok(())
        })
    }
    fn prepare<'a>(&'a self, context: &'a RequestContext, tool: &'a str,
        arguments: &'a Value, run: RunHandle) -> PolicyFuture<'a, SpawnPolicy> {
        Box::pin(async move {
            let before = tool_policy::authorize(context, tool, arguments, self.resolver.as_ref())?;
            self.plans.authorize_handler(context, tool, arguments).await?;
            let plan = self.plans.prepare(context, tool, arguments, run).await?;
            let after = tool_policy::authorize(context, tool, arguments, self.resolver.as_ref())?;
            if before.resources() != after.resources() { return Err(VerificationError::BindingMismatch); }
            Ok(plan)
        })
    }
}

/// E's blocking client retains its admission/session lease through snapshot,
/// exchange and teardown. This function must use WorkerLauncher/WorkerSession;
/// it may not execute a parser or reopen a pathname in the gateway.
pub type WorkerTableCall = dyn Fn(ParseRequest, File, TableOptions)
    -> Result<Value, VerificationError> + Send + Sync;

pub struct GatewayTableParser { invoke: Arc<WorkerTableCall> }
impl GatewayTableParser {
    pub fn new(invoke: Arc<WorkerTableCall>) -> Self { Self { invoke } }
}

impl FileParser for GatewayTableParser {
    fn parse<'a>(&'a self, _context: RequestContext, request: ParseRequest, input: File,
        options: TableOptions) -> PolicyFuture<'a, ParseResponse> {
        Box::pin(async move {
            options.validate()?;
            request.validate().map_err(|_| VerificationError::Invalid)?;
            let expected = crate::parsing::table_request(&input, &options)?;
            if request.operation != expected.operation || request.inputs != expected.inputs
                || request.outputs != expected.outputs || request.limits != expected.limits {
                return Err(VerificationError::Invalid);
            }
            let invoke = self.invoke.clone();
            // Do not wrap this in an async timeout: dropping a blocking task is
            // not proof of worker teardown. E enforces the wall-time deadline.
            let original = request.clone();
            let checked_options = options.clone();
            let value = tokio::task::spawn_blocking(move || invoke(request, input, options))
                .await.map_err(|_| VerificationError::Unavailable)??;
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
