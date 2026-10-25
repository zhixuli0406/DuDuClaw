//! Owned Closed operations. Generic transport and decoders stay unchanged.
use super::*;

pub enum OwnedScopedFailure {
    Denied,
    Unconfirmed,
}
impl OwnedScopedFailure {
    pub fn message(&self) -> &'static str {
        match self {
            Self::Denied => "Closed MCP authorization unavailable; operation refused",
            Self::Unconfirmed => {
                "Closed MCP result unconfirmed; remote execution may have occurred"
            }
        }
    }
}
pub enum OwnedScopedReply {
    Discover(Vec<McpToolDef>),
    Tool {
        arguments: Value,
        result: ToolCallResult,
        notice: Option<String>,
    },
}
#[async_trait::async_trait]
pub trait OwnedScopedOperation: Send {
    async fn execute(self: Box<Self>) -> Result<OwnedScopedReply, OwnedScopedFailure>;
}
/// Constructed only at the actual slot. No wire constructor or recipe outlet.
pub struct ClosedClientTarget<'a> {
    server: &'a str,
    recipe: &'a ScopedHttpRecipe,
    host_binding: &'a str,
    filter: &'a ToolFilter,
    deadline: tokio::time::Instant,
}
impl ClosedClientTarget<'_> {
    pub fn server(&self) -> &str {
        self.server
    }
    pub fn deadline(&self) -> tokio::time::Instant {
        self.deadline
    }
    pub fn matches_host_binding(&self, expected: &str) -> bool {
        self.host_binding == expected
    }
    pub fn matches_filter(&self, expected: &ToolFilter) -> bool {
        self.filter.allowed == expected.allowed && self.filter.denied == expected.denied
    }
    pub fn matches_recipe(&self, expected: &ScopedHttpRecipe) -> bool {
        self.recipe.url == expected.url
            && self.recipe.headers == expected.headers
            && self.recipe.operation_timeout == expected.operation_timeout
    }
}
/// Nonsecret metadata, never deserialized. Its constructor is registry-private.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClosedPostprocessPolicy {
    deadline: tokio::time::Instant,
    entered: bool,
}
impl ClosedPostprocessPolicy {
    pub(crate) fn expired(&self) -> bool {
        tokio::time::Instant::now() >= self.deadline
    }
    pub(crate) fn deadline(&self) -> tokio::time::Instant {
        self.deadline
    }
    pub(crate) fn before_execute_refusal(&self) -> bool {
        !self.entered
    }
    pub(crate) fn failure_message(&self) -> &'static str {
        if self.entered {
            OwnedScopedFailure::Unconfirmed.message()
        } else {
            OwnedScopedFailure::Denied.message()
        }
    }
    pub(crate) async fn wait<F: std::future::Future>(&self, future: F) -> Result<F::Output, ()> {
        if self.expired() {
            return Err(());
        }
        let value = tokio::time::timeout_at(self.deadline, future)
            .await
            .map_err(|_| ())?;
        if self.expired() { Err(()) } else { Ok(value) }
    }
    fn attach(self, mut outcome: ToolOutcome) -> ToolOutcome {
        outcome.closed_postprocess = Some(self);
        outcome
    }
}
fn failed(
    deadline: tokio::time::Instant,
    entered: bool,
    failure: OwnedScopedFailure,
) -> ToolOutcome {
    ClosedPostprocessPolicy { deadline, entered }
        .attach(ToolOutcome::error(failure.message()).without_ccr())
}
impl RegistryClient {
    pub fn scoped_http_closed(
        recipe: ScopedHttpRecipe,
        host_binding: String,
        guard: Arc<dyn ToolInterceptor>,
    ) -> Self {
        Self::ScopedHttp(ScopedHttpSlot {
            recipe,
            guard,
            mode: ScopedMode::Closed,
            host_binding,
            closed_filter: ToolFilter::default(),
        })
    }
}
impl ScopedHttpSlot {
    pub(super) async fn discover_closed(
        &self,
        server: &str,
        deadline: tokio::time::Instant,
    ) -> Result<Vec<McpToolDef>, McpError> {
        let refuse = || McpError::Io(OwnedScopedFailure::Denied.message().into());
        let unconfirmed = || McpError::Io(OwnedScopedFailure::Unconfirmed.message().into());
        if tokio::time::Instant::now() >= deadline {
            return Err(refuse());
        }
        let target = ClosedClientTarget {
            server,
            recipe: &self.recipe,
            host_binding: &self.host_binding,
            filter: &self.closed_filter,
            deadline,
        };
        let operation = tokio::time::timeout_at(
            deadline,
            self.guard
                .prepare_closed_client_call(target, ClientOperation::Discover, Value::Null),
        )
        .await
        .map_err(|_| refuse())?
        .map_err(|_| refuse())?;
        if tokio::time::Instant::now() >= deadline {
            return Err(refuse());
        }
        let result = tokio::time::timeout_at(deadline, operation.execute())
            .await
            .map_err(|_| unconfirmed())?
            .map_err(|error| McpError::Io(error.message().into()))?;
        if tokio::time::Instant::now() >= deadline {
            return Err(unconfirmed());
        }
        match result {
            OwnedScopedReply::Discover(defs) => Ok(defs),
            _ => Err(unconfirmed()),
        }
    }
}
impl ToolRegistry {
    pub(super) async fn call_closed_on_client(
        &self,
        idx: usize,
        name: &str,
        args: Value,
        deadline: tokio::time::Instant,
    ) -> Result<ToolOutcome, String> {
        let deny = || failed(deadline, false, OwnedScopedFailure::Denied);
        let uncertain = || failed(deadline, true, OwnedScopedFailure::Unconfirmed);
        if self.source_attestation_required.contains(name)
            && !self.source_verifiers.contains_key(name)
        {
            return Ok(deny());
        }
        let mut client = match tokio::time::timeout_at(deadline, self.clients[idx].lock()).await {
            Ok(c) => c,
            Err(_) => return Ok(deny()),
        };
        if tokio::time::Instant::now() >= deadline {
            return Ok(deny());
        }
        let server = self.server_names.get(idx).map(String::as_str).unwrap_or("");
        let RegistryClient::ScopedHttp(slot) = &mut *client else {
            return Ok(deny());
        };
        if slot.mode != ScopedMode::Closed {
            return Ok(deny());
        }
        let target = ClosedClientTarget {
            server,
            recipe: &slot.recipe,
            host_binding: &slot.host_binding,
            filter: &slot.closed_filter,
            deadline,
        };
        let operation = match tokio::time::timeout_at(
            deadline,
            slot.guard.prepare_closed_client_call(
                target,
                ClientOperation::Tool { name },
                args.clone(),
            ),
        )
        .await
        {
            Ok(Ok(operation)) => operation,
            _ => return Ok(deny()),
        };
        if tokio::time::Instant::now() >= deadline {
            return Ok(deny());
        }
        let reply = match tokio::time::timeout_at(deadline, operation.execute()).await {
            Ok(Ok(reply)) => reply,
            Ok(Err(error)) => return Ok(failed(deadline, true, error)),
            Err(_) => return Ok(uncertain()),
        };
        drop(client);
        let policy = ClosedPostprocessPolicy {
            deadline,
            entered: true,
        };
        if policy.expired() {
            return Ok(uncertain());
        }
        let OwnedScopedReply::Tool {
            arguments,
            mut result,
            notice,
        } = reply
        else {
            return Ok(uncertain());
        };
        if arguments != args {
            return Ok(uncertain());
        }
        if let Some(interceptor) = self.client_interceptors.get(&idx) {
            let mut content = serde_json::from_str::<Value>(&result.content)
                .unwrap_or_else(|_| Value::String(result.content.clone()));
            let original = content.clone();
            if policy.expired() {
                return Ok(uncertain());
            }
            interceptor.after_call(server, name, &args, &mut content);
            if policy.expired() {
                return Ok(uncertain());
            }
            if content != original {
                result.content = match content {
                    Value::String(text) => text,
                    value => value.to_string(),
                };
            }
        }
        let mut outcome = match policy
            .wait(self.outcome_from_result(name, &args, result))
            .await
        {
            Ok(outcome) => outcome,
            Err(_) => return Ok(uncertain()),
        };
        if let Some(notice) = notice {
            outcome = outcome.with_host_notice(&notice);
        }
        if policy.expired() {
            return Ok(uncertain());
        }
        Ok(policy.attach(outcome))
    }
}
