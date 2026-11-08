//! Gateway-only Google account bridge. The existing sealed Google host owns
//! selection, refresh, effect leases and output scrubbing; no bearer is exposed
//! through this adapter, the capability record, environment or run filesystem.
use std::{collections::BTreeMap, future::Future, path::PathBuf, pin::Pin, sync::{Arc, Mutex}};
use duduclaw_auth::UserDb;
use duduclaw_core::google_identity::{self as gi, CredentialRef, Effective};
use duduclaw_isolation::{capability::Principal, ids::EmployeeId};
use duduclaw_knowledge::requester::RequesterIdentity;
use duduclaw_mcp_boundary::{RequestContext, VerificationError, tool_policy::AuthorizedToolCall};
use serde_json::{Value, json};
use super::{effect_authority::{Clock, SelectionStore}, home_source,
    isolation_routes::{GoogleAccountRoutes, GoogleRoute, GoogleRoutesError}};
use crate::{google_personal::authority::PersonalSelection,
    isolation_egress::{IngressRequester, RunTurnResolver},
    isolation_connectors::{BoundConnectorExecutor, ConnectorCatalog, LiveConnectorSelectionPolicy,
        ProductConnectorAccount, SelectedConnector, provider_for_tool}};

/// The authenticated requester of one capability call, derived from the
/// capability principal plus the host's live run association only.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Requester {
    /// Authenticated person: a dashboard user, or a channel sender mapped
    /// through the verified channel→user binding. `user_id` is a users.db id.
    Human { user_id: String, identity: RequesterIdentity },
    /// Background / scheduled / delegated work. Never stands for a person.
    System(RequesterIdentity),
}
impl Requester {
    fn identity(&self) -> &RequesterIdentity {
        match self { Self::Human { identity, .. } | Self::System(identity) => identity }
    }
}

pub struct GoogleIsolationBridge {
    home: PathBuf,
    agent: String,
    employee: EmployeeId,
    users: Option<Arc<UserDb>>,
    reference: CredentialRef,
    catalog: Arc<ConnectorCatalog>,
    // Freeze each logical source at first use. Refresh is not a new source;
    // owner/consent/ACL/source generation changes require rebuilding boot state.
    sources: Mutex<BTreeMap<String, (GoogleRoute, String)>>,
    selections: SelectionStore,
    // Host-owned run association (the same resolver the isolation runtime
    // uses for per-run metadata). Never employee arguments or process env.
    turns: Arc<dyn RunTurnResolver>,
}
impl GoogleIsolationBridge {
    pub fn new(home: PathBuf, agent: String, employee: EmployeeId, users: Option<Arc<UserDb>>,
        reference: CredentialRef, catalog: Arc<ConnectorCatalog>, turns: Arc<dyn RunTurnResolver>)
        -> Result<Self, VerificationError> {
        if !home.is_absolute() || !reference.is_well_formed()
            || !duduclaw_core::is_valid_agent_id(&agent) || duduclaw_core::is_reserved_agent_id(&agent)
            || agent == duduclaw_core::UNTRUSTED_AGENT_ID {
            return Err(VerificationError::Invalid);
        }
        let home = home.canonicalize().map_err(|_| VerificationError::Unavailable)?;
        Ok(Self { home, agent, employee, users, reference, catalog,
            sources: Mutex::new(BTreeMap::new()), selections: SelectionStore::default(), turns })
    }

    /// Build the requester from the capability principal plus the live run
    /// association. Any lookup failure refuses; nothing falls back to Unknown,
    /// to the employee's owner or to the home account.
    fn requester(&self, context: &RequestContext) -> Result<Requester, VerificationError> {
        let resolved = self.turns.resolve_run(context)?;
        let metadata = resolved.metadata.ok_or(VerificationError::Unavailable)?;
        match (&context.record().principal, metadata.ingress.as_ref()) {
            (Principal::Human { user_id, .. }, Some(ingress)) => {
                let user_id = user_id.as_str();
                // Interactive ingress refuses nested/dispatched runs; a marker
                // on such a run is inconsistent host state, not a person.
                if user_id.is_empty() || metadata.delegation_depth != 0 || metadata.hop_depth != 0
                    || metadata.delegation_origin.is_some() || metadata.dispatch_run.is_some()
                    || metadata.upstream_unknown {
                    return Err(VerificationError::BindingMismatch);
                }
                let identity = match ingress {
                    IngressRequester::Dashboard => RequesterIdentity::DashboardUser { user_id: user_id.into() },
                    IngressRequester::Channel { channel, sender } => {
                        // Re-read the verified channel→user binding now; a raw
                        // channel sender id is never a dashboard user id.
                        let mapped = crate::trigger_route::access::verified_user_for_channel_sender(
                            &self.home, channel, sender).ok_or(VerificationError::Revoked)?;
                        if mapped != user_id { return Err(VerificationError::Revoked); }
                        RequesterIdentity::ChannelUser { channel: channel.clone(),
                            chat: metadata.reply_channel.clone().unwrap_or_default(),
                            sender: sender.clone(), user_id: Some(mapped) }
                    }
                };
                Ok(Requester::Human { user_id: user_id.into(), identity })
            }
            // Background records currently carry a Human owner principal but
            // no ingress marker: that is System work, never the owner in person.
            (Principal::Human { .. } | Principal::Background { .. }, None) =>
                Ok(Requester::System(RequesterIdentity::System { kind: "isolation_background".into(), on_behalf_of: None })),
            (Principal::Background { .. }, Some(_)) => Err(VerificationError::BindingMismatch),
        }
    }

    /// A Personal source is selectable only for the authenticated person who
    /// owns it, through the catalog route of the lifecycle owner only.
    fn personal_route(routes: &GoogleAccountRoutes, requester: &Requester, owner: Option<&str>)
        -> Result<GoogleRoute, VerificationError> {
        let Requester::Human { user_id, .. } = requester else {
            // TODO(B3): System may select a Personal source only with a
            // record-bound background authority that pins the employee binding
            // and owner (RecordAuthority + binding). Until B3 lands, refuse.
            return Err(VerificationError::BindingMismatch);
        };
        let owner = owner.filter(|owner| !owner.is_empty()).ok_or(VerificationError::BindingMismatch)?;
        if owner != user_id.as_str() { return Err(VerificationError::BindingMismatch); }
        let route = routes.personal.get(owner).cloned().ok_or(VerificationError::BindingMismatch)?;
        if !matches!(&route.product, ProductConnectorAccount::Personal { owner: routed, .. } if routed == owner) {
            return Err(VerificationError::BindingMismatch);
        }
        Ok(route)
    }

    fn source(&self, context: &RequestContext, tool: &str) -> Result<(GoogleRoute, String), VerificationError> {
        // Resolve first: a failed requester lookup refuses before any source is read.
        let requester = self.requester(context)?;
        let routes = GoogleAccountRoutes::read(&self.home, &self.agent, self.employee,
            &context.record().employee_binding, &self.catalog)?;
        super::credential_registry::authorize(&self.home, &self.reference, &self.agent, tool)
            .map_err(|_| VerificationError::BindingMismatch)?;
        if !crate::google_workspace::integration_enabled(&self.home) { return Err(VerificationError::Unavailable); }
        let effective = gi::effective_for(&self.home, Some(&self.agent));
        let people = super::requester_gate::people_for(&self.home, self.users.as_ref());
        let route = match &effective.effective {
            Effective::Personal { .. } => {
                // TODO(B3): the requester gate admits System { on_behalf_of: None },
                // so System must not even capture a Personal selection until
                // record-bound background authority exists. Refuse, no home fallback.
                if matches!(requester, Requester::System(_)) { return Err(VerificationError::BindingMismatch); }
                // Shared employees cannot select any person's private source.
                if routes.personal.is_empty() { return Err(VerificationError::BindingMismatch); }
                let who = requester.identity();
                let original = PersonalSelection::capture(&self.home, &self.agent,
                    who, people.as_ref(), tool)
                    .map_err(|_| VerificationError::BindingMismatch)?;
                // Unhealthy/unavailable Personal sources never fall back to home.
                original.ensure_healthy_personal().map_err(|_| VerificationError::Unavailable)?;
                let route = Self::personal_route(&routes, &requester, original.user_id())?;
                route.recheck(&self.catalog)?;
                let digest = serde_json::to_value(&original).map_err(|_| VerificationError::Invalid)?;
                return Ok((route, gi::google_arguments_digest(&digest)));
            }
            Effective::Home => routes.home.ok_or(GoogleRoutesError::SetupRequired)?,
            // This bridge does not reinterpret delegate/SA/Apps Script material
            // as OAuth. Their own source-owner adapters must be installed.
            _ => return Err(VerificationError::Unavailable),
        };
        let source = home_source::read_home_native_selection(&self.home, tool)
            .map_err(|_| VerificationError::Unavailable)?;
        let digest = source.binding_digest().map_err(|_| VerificationError::Unavailable)?;
        route.recheck(&self.catalog)?;
        Ok((route, gi::google_arguments_digest(&json!({"source":digest}))))
    }

    fn pin_source(&self, tool: &str, route: GoogleRoute, digest: String)
        -> Result<ProductConnectorAccount, VerificationError> {
        let product = route.product.clone();
        let proof = (route, digest);
        let mut sources = self.sources.lock().map_err(|_| VerificationError::Unavailable)?;
        if let Some(original) = sources.get(tool) {
            if original != &proof { return Err(VerificationError::BindingMismatch); }
        } else { sources.insert(tool.into(), proof); }
        Ok(product)
    }

    async fn owned(&self, selected: &SelectedConnector, call: &AuthorizedToolCall) -> Result<Value, VerificationError> {
        selected.verify_call(call)?;
        let tool = call.name();
        let args = call.arguments();
        let (route, _) = self.source(call.context(), tool)?;
        if route.product.account() != selected.account() || route.generation() != selected.generation()
            || route.kind() != selected.kind() {
            return Err(VerificationError::BindingMismatch);
        }
        let stamp = self.selections.select_dedicated_async(&self.home, self.users.as_ref(), &self.agent,
            &self.reference, tool, args, Clock::now()).await.map_err(|_| VerificationError::Unavailable)?;
        selected.recheck()?;
        if let Some(stamp) = stamp {
            // An independently captured original must agree with the account
            // chosen by isolation_connectors before any material is resolved.
            match stamp.approval.selected_identity {
                gi::ApprovedGoogleIdentity::Personal => {
                    if !matches!(&route.product, ProductConnectorAccount::Personal { owner, .. }
                        if owner == &stamp.approval.selected_credential_owner) {
                        return Err(VerificationError::BindingMismatch);
                    }
                }
                gi::ApprovedGoogleIdentity::HomeOAuthFallback => {
                    if !matches!(&route.product, ProductConnectorAccount::SharedHome { .. }) {
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
        let (route, digest) = self.source(context, tool)?;
        Ok(vec![self.pin_source(tool, route, digest)?])
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
