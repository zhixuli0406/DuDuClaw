//! Gateway-side E invocation. All content parsing remains behind the injected
//! WorkerLauncher/WorkerSession transport; only bounded structured results return.
use std::sync::Arc;
use duduclaw_isolation::worker::{ParseRequest, ParseResponse, ParsedItem, ParseResult,
    MAX_WORKER_FRAME_BYTES, MAX_OUTPUT_BYTES};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use crate::{RequestContext, VerificationError, execution::PolicyFuture,
    tool_policy::{self, AuthorizedToolCall, ResourceResolver, Route}};

pub struct WorkerPlan {
    request: ParseRequest,
    /// Already authorized by ResourceRoots for this call and selected file.
    /// Never reopened by pathname. E snapshots it after launcher admission.
    input: crate::resources::AuthorizedFile,
}
impl WorkerPlan {
    pub fn new(request: ParseRequest, input: crate::resources::AuthorizedFile) -> Result<Self, VerificationError> {
        let plan = Self { request, input };
        validate_plan(&plan)?;
        Ok(plan)
    }
    pub fn request(&self) -> &ParseRequest { &self.request }
    /// Only the installed worker transport consumes this pair, after the bridge
    /// has compared the token with the current call and checked the inode.
    pub fn into_parts(self) -> Result<(ParseRequest, crate::resources::AuthorizedFile), VerificationError> {
        self.input.verify_live()?;
        Ok((self.request, self.input))
    }
}
pub struct WorkerReply {
    pub response: ParseResponse,
    pub outputs: Vec<Vec<u8>>,
}

/// Injected by trusted boot. The factory retains original handler ownership,
/// audience and approval gates. `finish` must apply the authorized resources at
/// use time, and may never execute instructions or paths returned by a parser.
pub trait WorkerPlanFactory: Send + Sync {
    fn supports(&self, tool: &str) -> bool;
    fn authorize_handler<'a>(&'a self, call: &'a AuthorizedToolCall) -> PolicyFuture<'a, ()>;
    fn prepare<'a>(&'a self, call: &'a AuthorizedToolCall) -> PolicyFuture<'a, WorkerPlan>;
    fn finish<'a>(&'a self, call: &'a AuthorizedToolCall, reply: WorkerReply) -> PolicyFuture<'a, Value>;
    /// Request-local host state (fetch metadata, publication receipt) never
    /// crosses the worker boundary and cannot be supplied by the parser.
    fn prepare_with_state<'a>(&'a self, call: &'a AuthorizedToolCall)
        -> PolicyFuture<'a, (WorkerPlan, Value)> {
        Box::pin(async move { self.prepare(call).await.map(|plan| (plan, Value::Null)) })
    }
    fn finish_with_state<'a>(&'a self, call: &'a AuthorizedToolCall, reply: WorkerReply,
        _state: Value) -> PolicyFuture<'a, Value> {
        self.finish(call, reply)
    }
}

/// Implemented by gateway's E adapter using its installed launcher. This is a
/// blocking, bounded invocation; D runs it on the blocking pool, not Tokio I/O.
pub type WorkerInvoke = dyn Fn(WorkerPlan) -> Result<WorkerReply, VerificationError> + Send + Sync;

pub struct WorkerToolBridge {
    factory: Arc<dyn WorkerPlanFactory>,
    resolver: Arc<dyn ResourceResolver>,
    invoke: Arc<WorkerInvoke>,
}
impl WorkerToolBridge {
    pub fn new(factory: Arc<dyn WorkerPlanFactory>, resolver: Arc<dyn ResourceResolver>,
        invoke: Arc<WorkerInvoke>) -> Self { Self { factory, resolver, invoke } }

    pub fn supports(&self, name: &str) -> bool {
        tool_policy::policy(name).is_some_and(|policy| policy.route == Route::Worker)
            && self.factory.supports(name)
    }

    pub async fn execute(&self, context: &RequestContext, name: &str, arguments: &Value) -> Value {
        match self.run(context, name, arguments).await {
            Ok(value) => value,
            Err(_) => json!({"content":[{"type":"text","text":"無法完成這次隔離處理，請查看工作狀態。"}],
                "isError":true, "execution_status":"unconfirmed"}),
        }
    }

    async fn run(&self, context: &RequestContext, name: &str, arguments: &Value)
        -> Result<Value, VerificationError> {
        if !self.supports(name) { return Err(VerificationError::Unavailable); }
        let call = tool_policy::authorize_async(context, name, arguments, self.resolver.clone()).await?;
        self.factory.authorize_handler(&call).await?;
        call.context().verify_live(crate::authority::now_seconds()?)?;
        let (mut plan, state) = crate::io_authority::scope(call.context().clone(),
            self.factory.prepare_with_state(&call)).await?;
        validate_plan(&plan)?;
        plan.input.verify(&call)?;
        self.factory.authorize_handler(&call).await?;
        let checked = tool_policy::authorize_async(context, name, arguments, self.resolver.clone()).await?;
        if call.resources() != checked.resources() { return Err(VerificationError::BindingMismatch); }
        plan.input.verify(&checked)?;
        let request = plan.request.clone();
        let invoke = self.invoke.clone();
        let invocation = crate::resources::InvocationGuard::new();
        plan.input.bind_invocation(&invocation)?;
        // A caller drop cannot detach authority from a worker: E retains its
        // launcher lease inside this blocking invocation until teardown.
        let live = checked.context().clone();
        let reply = tokio::task::spawn_blocking(move || {
            live.verify_live(crate::authority::now_seconds()?)?;
            plan.input.verify_live()?;
            invoke(plan)
        }).await
            .map_err(|_| VerificationError::Unavailable)??;
        validate_reply(&request, &reply)?;
        self.factory.authorize_handler(&checked).await?;
        let current = tool_policy::authorize_async(context, name, arguments, self.resolver.clone()).await?;
        if checked.resources() != current.resources() { return Err(VerificationError::BindingMismatch); }
        current.context().verify_live(crate::authority::now_seconds()?)?;
        let result = crate::io_authority::scope(current.context().clone(),
            self.factory.finish_with_state(&current, reply, state)).await?;
        if serde_json::to_vec(&result).map_err(|_| VerificationError::Invalid)?.len() > MAX_WORKER_FRAME_BYTES {
            return Err(VerificationError::Invalid);
        }
        Ok(result)
    }
}

fn validate_plan(plan: &WorkerPlan) -> Result<(), VerificationError> {
    plan.request.validate().map_err(|_| VerificationError::Invalid)?;
    plan.input.verify_operation(plan.request.operation)?;
    // Match E's implementation ceilings rather than the broader wire maxima.
    if plan.request.inputs.as_slice().len() != 1 || plan.request.outputs.as_slice().len() > 1
        || plan.request.limits.memory_bytes.get() > 512 * 1024 * 1024
        || plan.request.limits.wall_time_ms.get() > 30_000 {
        return Err(VerificationError::Invalid);
    }
    let meta = plan.input.metadata()?;
    if !meta.is_file() || meta.len() > 64 * 1024 * 1024
        || plan.request.inputs.as_slice()[0].size_bytes != meta.len() {
        return Err(VerificationError::Invalid);
    }
    Ok(())
}

fn validate_reply(request: &ParseRequest, reply: &WorkerReply) -> Result<(), VerificationError> {
    reply.response.validate_for(request).map_err(|_| VerificationError::Invalid)?;
    if serde_json::to_vec(&reply.response).map_err(|_| VerificationError::Invalid)?.len() > MAX_WORKER_FRAME_BYTES
        || reply.outputs.len() != request.outputs.as_slice().len() {
        return Err(VerificationError::Invalid);
    }
    let ParseResult::Complete { items } = &reply.response.result else {
        return Err(VerificationError::Unavailable);
    };
    for (declared, bytes) in request.outputs.as_slice().iter().zip(&reply.outputs) {
        if bytes.len() as u64 > declared.max_bytes.get() || bytes.len() as u64 > MAX_OUTPUT_BYTES {
            return Err(VerificationError::Invalid);
        }
        let matching: Vec<_> = items.as_slice().iter().filter_map(|item| match item {
            ParsedItem::OutputArtifact { output_slot, size_bytes, sha256, .. } if *output_slot == declared.slot =>
                Some((*size_bytes, sha256)),
            _ => None,
        }).collect();
        let [(size, digest)] = matching.as_slice() else { return Err(VerificationError::Invalid); };
        if *size != bytes.len() as u64 || Sha256::digest(bytes).as_slice() != digest.0 {
            return Err(VerificationError::Invalid);
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "worker_execution_tests.rs"]
mod tests;

/// Ordered trusted factories. Preserve explicitly installed product producers
/// and fill only their unsupported tools with the boot-owned defaults.
pub struct CompositeWorkerPlans {
    factories: Vec<Arc<dyn WorkerPlanFactory>>,
}
impl CompositeWorkerPlans {
    pub fn new(factories: Vec<Arc<dyn WorkerPlanFactory>>) -> Result<Self, VerificationError> {
        if factories.is_empty() || factories.len() > 16 { return Err(VerificationError::Invalid); }
        Ok(Self { factories })
    }
    fn select(&self, name: &str) -> Result<&dyn WorkerPlanFactory, VerificationError> {
        self.factories.iter().find(|factory| factory.supports(name)).map(|factory| factory.as_ref())
            .ok_or(VerificationError::Unavailable)
    }
}
impl WorkerPlanFactory for CompositeWorkerPlans {
    fn supports(&self, name: &str) -> bool { self.select(name).is_ok() }
    fn authorize_handler<'a>(&'a self, call: &'a AuthorizedToolCall) -> PolicyFuture<'a, ()> {
        Box::pin(async move { self.select(call.name())?.authorize_handler(call).await })
    }
    fn prepare<'a>(&'a self, call: &'a AuthorizedToolCall) -> PolicyFuture<'a, WorkerPlan> {
        Box::pin(async move { self.select(call.name())?.prepare(call).await })
    }
    fn finish<'a>(&'a self, call: &'a AuthorizedToolCall, reply: WorkerReply) -> PolicyFuture<'a, Value> {
        Box::pin(async move { self.select(call.name())?.finish(call, reply).await })
    }
    fn prepare_with_state<'a>(&'a self, call: &'a AuthorizedToolCall) -> PolicyFuture<'a, (WorkerPlan, Value)> {
        Box::pin(async move { self.select(call.name())?.prepare_with_state(call).await })
    }
    fn finish_with_state<'a>(&'a self, call: &'a AuthorizedToolCall, reply: WorkerReply, state: Value) -> PolicyFuture<'a, Value> {
        Box::pin(async move { self.select(call.name())?.finish_with_state(call, reply, state).await })
    }
}
