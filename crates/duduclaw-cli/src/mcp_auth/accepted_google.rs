//! A dispatcher-accepted Google lane is host-only call state, never wire data.
use super::{AuthenticatedCall, Principal};
use serde_json::Value;
use std::path::Path;

pub(crate) struct GoogleLane {
    receipt: AuthenticatedCall,
    tool: String,
}

/// Capture acceptance before any policy/approval waits. A custom internal
/// integration key without an employee remains on its existing Home path.
pub(crate) fn accept_google_lane(
    home: &Path,
    principal: &Principal,
    tool: &str,
) -> Option<GoogleLane> {
    super::dedicated_google_tool_allowed(home, principal, tool).then_some(())?;
    Some(GoogleLane {
        receipt: super::current_call_matches(principal)?,
        tool: tool.to_owned(),
    })
}

#[derive(Clone)]
pub(crate) struct AcceptedGoogleCall {
    receipt: AuthenticatedCall,
    tool: String,
    arguments: Value,
}

impl std::fmt::Debug for AcceptedGoogleCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcceptedGoogleCall")
            .field("employee", &self.principal().client_id)
            .field("tool", &self.tool)
            .finish_non_exhaustive()
    }
}

impl GoogleLane {
    /// Bind only the effective arguments after dispatcher rewrites/approvals.
    pub(crate) fn bind(self, arguments: Value) -> AcceptedGoogleCall {
        AcceptedGoogleCall {
            receipt: self.receipt,
            tool: self.tool,
            arguments,
        }
    }
}

impl AcceptedGoogleCall {
    pub(crate) fn principal(&self) -> &Principal {
        self.receipt.principal()
    }
    pub(crate) fn credential_ref(&self) -> Option<&duduclaw_core::google_identity::CredentialRef> {
        self.receipt.credential_ref()
    }
    pub(crate) fn matches(&self, tool: &str, arguments: &Value) -> bool {
        self.tool == tool && self.arguments == *arguments
    }
}

tokio::task_local! {
    static ACCEPTED_GOOGLE: Option<AcceptedGoogleCall>;
}

/// Box the real handler before constructing the scope (default-stack bound).
pub(crate) fn with_accepted_google_call<F: std::future::Future>(
    call: Option<AcceptedGoogleCall>,
    future: F,
) -> impl std::future::Future<Output = F::Output> {
    ACCEPTED_GOOGLE.scope(call, Box::pin(future))
}

pub(crate) fn current_accepted_google_call() -> Option<AcceptedGoogleCall> {
    ACCEPTED_GOOGLE.try_with(Clone::clone).ok().flatten()
}
