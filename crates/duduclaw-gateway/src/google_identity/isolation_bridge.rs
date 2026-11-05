//! Gateway-only Google account bridge. The existing sealed Google host owns
//! selection, refresh, effect leases and output scrubbing; no bearer is exposed
//! through this adapter, the capability record, environment or run filesystem.
use std::{collections::BTreeMap, future::Future, path::PathBuf, pin::Pin, sync::{Arc, Mutex}};
use duduclaw_auth::UserDb;
use duduclaw_core::google_identity::{self as gi, CredentialRef, Effective};
use duduclaw_isolation::ids::EmployeeId;
use duduclaw_knowledge::requester::RequesterIdentity;
use duduclaw_mcp_boundary::{RequestContext, VerificationError, tool_policy::AuthorizedToolCall};
use serde_json::{Value, json};
use super::{effect_authority::{Clock, SelectionStore}, home_source};
use crate::{google_personal::authority::PersonalSelection,
    isolation_connectors::{BoundConnectorExecutor, LiveConnectorSelectionPolicy,
        ProductConnectorAccount, SelectedConnector, provider_for_tool}};

/// These routes are installed by the trusted boot registry, never from a tool
/// argument. Personal routes are indexed by the actual users.db owner id.
#[derive(Default)]
pub struct GoogleAccountRoutes {
    pub personal: BTreeMap<String, ProductConnectorAccount>,
    pub home: Option<ProductConnectorAccount>,
}

pub struct GoogleIsolationBridge {
    home: PathBuf,
    agent: String,
    employee: EmployeeId,
    users: Option<Arc<UserDb>>,
    reference: CredentialRef,
    routes: GoogleAccountRoutes,
    // Freeze each logical source at first use. Refresh is not a new source;
    // owner/consent/ACL/source generation changes require rebuilding boot state.
    sources: Mutex<BTreeMap<String, String>>,
    selections: SelectionStore,
}
impl GoogleIsolationBridge {
    pub fn new(home: PathBuf, agent: String, employee: EmployeeId, users: Option<Arc<UserDb>>,
        reference: CredentialRef, routes: GoogleAccountRoutes) -> Result<Self, VerificationError> {
        if !home.is_absolute() || !reference.is_well_formed()
            || !duduclaw_core::is_valid_agent_id(&agent) || duduclaw_core::is_reserved_agent_id(&agent)
            || agent == duduclaw_core::UNTRUSTED_AGENT_ID || routes.personal.len() > 128
            || routes.personal.iter().any(|(id, route)| !matches!(route,
                ProductConnectorAccount::Personal { owner, .. } if owner == id))
            || routes.home.as_ref().is_some_and(|route| !matches!(route, ProductConnectorAccount::SharedHome { .. })) {
            return Err(VerificationError::Invalid);
        }
        let home = home.canonicalize().map_err(|_| VerificationError::Unavailable)?;
        Ok(Self { home, agent, employee, users, reference, routes,
            sources: Mutex::new(BTreeMap::new()), selections: SelectionStore::default() })
    }

    fn source(&self, tool: &str) -> Result<(ProductConnectorAccount, String), VerificationError> {
        super::credential_registry::authorize(&self.home, &self.reference, &self.agent, tool)
            .map_err(|_| VerificationError::BindingMismatch)?;
        if !crate::google_workspace::integration_enabled(&self.home) { return Err(VerificationError::Unavailable); }
        let effective = gi::effective_for(&self.home, Some(&self.agent));
        let people = super::requester_gate::people_for(&self.home, self.users.as_ref());
        let (personal, route) = match &effective.effective {
            Effective::Personal { .. } => {
                // Dedicated native calls have no authenticated channel requester.
                // Unknown stays Unknown: never promote the run's owner to caller.
                let original = PersonalSelection::capture(&self.home, &self.agent,
                    &RequesterIdentity::Unknown, people.as_ref(), tool)
                    .map_err(|_| VerificationError::BindingMismatch)?;
                if original.ensure_healthy_personal().is_ok() {
                    let route = self.routes.personal.get(original.user_id().ok_or(VerificationError::BindingMismatch)?)
                        .cloned().ok_or(VerificationError::BindingMismatch)?;
                    let digest = serde_json::to_value(&original).map_err(|_| VerificationError::Invalid)?;
                    return Ok((route, gi::google_arguments_digest(&digest)));
                }
                original.validate_original_unavailable(&self.home, &self.agent,
                    &RequesterIdentity::Unknown, people.as_ref(), tool).map_err(|_| VerificationError::BindingMismatch)?;
                (Some(original), self.routes.home.clone().ok_or(VerificationError::Unavailable)?)
            }
            Effective::Home => (None, self.routes.home.clone().ok_or(VerificationError::Unavailable)?),
            // This bridge does not reinterpret delegate/SA/Apps Script material
            // as OAuth. Their own source-owner adapters must be installed.
            _ => return Err(VerificationError::Unavailable),
        };
        let source = home_source::read_home_native_selection(&self.home, tool)
            .map_err(|_| VerificationError::Unavailable)?;
        let digest = source.binding_digest().map_err(|_| VerificationError::Unavailable)?;
        Ok((route, gi::google_arguments_digest(&json!({"source":digest,"personal":personal}))))
    }

    async fn owned(&self, selected: &SelectedConnector, call: &AuthorizedToolCall) -> Result<Value, VerificationError> {
        selected.verify_call(call)?;
        let tool = call.name();
        let args = call.arguments();
        let stamp = self.selections.select_dedicated_async(&self.home, self.users.as_ref(), &self.agent,
            &self.reference, tool, args, Clock::now()).await.map_err(|_| VerificationError::Unavailable)?;
        selected.recheck()?;
        if let Some(stamp) = stamp {
            // An independently captured original must agree with the account
            // chosen by isolation_connectors before any material is resolved.
            match stamp.approval.selected_identity {
                gi::ApprovedGoogleIdentity::Personal => {
                    let route = self.routes.personal.get(&stamp.approval.selected_credential_owner)
                        .ok_or(VerificationError::BindingMismatch)?;
                    if route.account() != selected.account() { return Err(VerificationError::BindingMismatch); }
                }
                gi::ApprovedGoogleIdentity::HomeOAuthFallback => {
                    if self.routes.home.as_ref().map(|r| r.account()) != Some(selected.account()) {
                        return Err(VerificationError::BindingMismatch);
                    }
                }
                _ => return Err(VerificationError::Unavailable),
            }
            self.selections.bind_dedicated(&self.home, self.users.as_ref(), &self.agent, &self.reference,
                tool, &stamp.opaque_id, &stamp.original_arguments_digest,
                &stamp.approval.account_binding_digest, args, Clock::now()).map_err(|_| VerificationError::Unavailable)?;
            selected.recheck()?;
            let pending = self.selections.claim_dedicated(&self.home, self.users.as_ref(), &self.agent,
                &self.reference, tool, &stamp.opaque_id, args, Clock::now()).map_err(|_| VerificationError::Unavailable)?;
            selected.recheck()?;
            if pending.is_metadata_status() { return pending.metadata_status().map_err(|_| VerificationError::Unavailable); }
            pending.execute_native(args.clone()).await.map_err(|_| VerificationError::Unavailable)
        } else {
            selected.recheck()?;
            super::effect_authority::execute_immediate_native(&self.home, self.users.as_ref(), &self.agent,
                &self.reference, tool, args.clone()).await.map_err(|_| VerificationError::Unavailable)
        }
    }
}
impl LiveConnectorSelectionPolicy for GoogleIsolationBridge {
    fn supports(&self, tool: &str) -> bool { provider_for_tool(tool) == Some("google") }
    fn candidates(&self, context: &RequestContext, tool: &str, provider: &str)
        -> Result<Vec<ProductConnectorAccount>, VerificationError> {
        context.verify_live(duduclaw_mcp_boundary::authority::now_seconds()?)?;
        if context.record().employee != self.employee || provider != "google" || !LiveConnectorSelectionPolicy::supports(self, tool) {
            return Err(VerificationError::BindingMismatch);
        }
        let (route, digest) = self.source(tool)?;
        let mut sources = self.sources.lock().map_err(|_| VerificationError::Unavailable)?;
        if let Some(original) = sources.get(tool) {
            if original != &digest { return Err(VerificationError::BindingMismatch); }
        } else { sources.insert(tool.into(), digest); }
        Ok(vec![route])
    }
}
impl BoundConnectorExecutor for GoogleIsolationBridge {
    fn supports(&self, tool: &str) -> bool { LiveConnectorSelectionPolicy::supports(self, tool) }
    fn execute<'a>(&'a self, selected: SelectedConnector, call: AuthorizedToolCall)
        -> Pin<Box<dyn Future<Output = Result<Value, VerificationError>> + Send + 'a>> {
        Box::pin(async move {
            selected.verify_call(&call)?;
            let selected = Arc::new(selected);
            let context = selected.binding_io_context();
            let result = duduclaw_mcp_boundary::io_authority::scope(context,
                tokio::time::timeout(std::time::Duration::from_secs(120), self.owned(&selected, &call))).await;
            // A late failure is never described as proof that a write did not run.
            match result {
                Ok(Ok(value)) if selected.recheck().is_ok() => Ok(value),
                _ => Ok(json!({"isError":true,"content":[{"type":"text","text":
                    "Google 操作結果無法確認。若有寫入，請先到 Google 檢查，勿直接重試。"}]})),
            }
        })
    }
}

#[cfg(test)]
#[path = "isolation_bridge_tests.rs"]
mod tests;
