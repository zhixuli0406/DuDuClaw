//! Gateway-only Google account bridge. The existing sealed Google host owns
//! selection, refresh, effect leases and output scrubbing; no bearer is exposed
//! through this adapter, the capability record, environment or run filesystem.
use std::{collections::{BTreeMap, BTreeSet}, future::Future, path::PathBuf, pin::Pin, sync::{Arc, Mutex}};
use duduclaw_auth::UserDb;
use duduclaw_core::google_identity::{self as gi, CredentialRef, Effective};
use duduclaw_isolation::{capability::{EmployeeBinding, Principal}, ids::EmployeeId};
use duduclaw_knowledge::requester::RequesterIdentity;
use duduclaw_mcp_boundary::{RequestContext, VerificationError, tool_policy::AuthorizedToolCall};
use serde_json::{Value, json};
use super::{effect_authority::{Clock, SelectionStore, TenantPin}, home_source,
    isolation_routes::{GoogleAccountRoutes, GoogleRoute, GoogleRoutesError},
    tenant_source::{self, Actor, SubjectAccount, SubjectState, TenantSource, TenantSourceKind}};
use crate::{google_personal::authority::PersonalSelection,
    isolation_egress::{IngressRequester, RunTurnResolver},
    isolation_connectors::{BoundConnectorExecutor, ConnectorCatalog, ConnectorKind, LiveConnectorSelectionPolicy,
        ProductConnectorAccount, SelectedConnector, provider_for_tool}};

/// v1.81 E4: argument keys that would name a Google identity/impersonation
/// subject. No native Google tool accepts any of them (exact key equality;
/// `subject` stays allowed because it is a mail subject line). Their presence
/// refuses the call: the subject is admin configuration only.
const IDENTITY_SELECTOR_ARGUMENTS: [&str; 14] = ["delegate", "impersonate", "impersonate_user", "impersonation",
    "as_user", "act_as", "run_as", "on_behalf_of", "google_identity", "identity", "user_email", "subject_email",
    "sub", "principal"];

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
    // SR4 item 2(iv): refused subject states already audited by this build.
    noticed: Mutex<BTreeSet<String>>,
}
/// Bound on [`GoogleIsolationBridge::noticed`] (one build per source digest).
const MAX_NOTICED: usize = 64;
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
            sources: Mutex::new(BTreeMap::new()), selections: SelectionStore::default(), turns,
            noticed: Mutex::new(BTreeSet::new()) })
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

    /// v1.81 E4: a tenant source (delegation key / service account) is served
    /// only through the Shared employee's own SharedTenant route of its pinned
    /// tenant, only for configured scopes, and only for a subject admitted by
    /// the D-5 rule ([`tenant_source::admit_subject`]). `accounts` are the
    /// users.db accounts whose normalized email equals the configured subject.
    fn tenant_route(routes: &GoogleAccountRoutes, binding: &EmployeeBinding, requester: &Requester,
        source: &TenantSource, accounts: &[SubjectAccount], tool: &str) -> Result<GoogleRoute, VerificationError> {
        // Tenant sources are Shared-only: a Personal employee never reaches one.
        let EmployeeBinding::Shared(tenant) = binding else { return Err(VerificationError::BindingMismatch) };
        let route = routes.tenant.clone().ok_or(GoogleRoutesError::SetupRequired)?;
        if route.kind() != ConnectorKind::SharedTenant || route.binding() != binding
            || !matches!(&route.product, ProductConnectorAccount::SharedHome { tenant: routed, .. }
                if routed.as_str() == tenant.as_str()) {
            return Err(VerificationError::BindingMismatch);
        }
        if !source.serves(tool) { return Err(VerificationError::BindingMismatch); }
        let actor = match requester {
            Requester::Human { user_id, .. } => Actor::Human(user_id.as_str()),
            Requester::System(_) => Actor::System,
        };
        tenant_source::admit_subject(source, actor, accounts)?;
        Ok(route)
    }

    fn tenant_selection(&self, context: &RequestContext, routes: &GoogleAccountRoutes, requester: &Requester,
        source: &TenantSource, people: &dyn super::requester_gate::People, tool: &str)
        -> Result<(GoogleRoute, String), VerificationError> {
        // Without a readable users.db nobody can prove the subject is (or is
        // not) a person: refuse rather than guess.
        let db = people.user_db().ok_or(VerificationError::Unavailable)?;
        // SR4 item 1: carries this employee's Admin attestation of the subject.
        // SR4 round 3 (N2): only an attestation of the employee id and
        // incarnation these routes were read (and pinned) for counts.
        let accounts = tenant_source::subject_accounts(db, &self.agent, Some(routes.pinned), source.subject())?;
        self.notice_refused_subject(source, &accounts);
        let route = Self::tenant_route(routes, &context.record().employee_binding, requester, source, &accounts, tool)?;
        // Use-time recheck: the source (config + key material) and the catalog
        // row must be exactly what was decided on.
        if tenant_source::read(&self.home, &self.agent)?.as_ref() != Some(source) {
            return Err(VerificationError::BindingMismatch);
        }
        route.recheck(&self.catalog)?;
        Ok((route, source.digest()))
    }

    /// SR4 items 1 / 2(iv): a subject refused because a staff account claims
    /// a designated mailbox, or because no Admin attested the delegate person,
    /// is audited for Admins once per build, source and state (the call stays
    /// refused; the dashboard shows the same state, `subject_notice`).
    fn notice_refused_subject(&self, source: &TenantSource, accounts: &[SubjectAccount]) {
        let state = tenant_source::subject_state(source, accounts);
        if !matches!(state, SubjectState::MailboxHeldByAccount | SubjectState::AttestationRequired) { return; }
        let key = format!("{}:{}", state.code(), source.digest());
        {
            let Ok(mut noticed) = self.noticed.lock() else { return };
            if noticed.len() >= MAX_NOTICED || !noticed.insert(key) { return; }
        }
        crate::security_autopilot::audit_and_emit(&self.home, &duduclaw_security::audit::AuditEvent::new(
            "google_tenant_subject_refused", self.agent.as_str(), duduclaw_security::audit::Severity::Warning,
            json!({ "agent_id": self.agent, "state": state.code(), "subject": source.subject() })));
    }

    fn source(&self, context: &RequestContext, tool: &str) -> Result<(GoogleRoute, String), VerificationError> {
        self.decide(context, tool).map(|(route, digest, _)| (route, digest))
    }

    /// The route, its source proof and (on the tenant lane) the tenant source
    /// decided on — the proof the host lane is bound to (SR4 item 4).
    fn decide(&self, context: &RequestContext, tool: &str)
        -> Result<(GoogleRoute, String, Option<TenantSource>), VerificationError> {
        // Resolve first: a failed requester lookup refuses before any source is read.
        let requester = self.requester(context)?;
        // Unreadable config/key material refuses before any route is read.
        let tenant = tenant_source::read(&self.home, &self.agent)?;
        let routes = GoogleAccountRoutes::read(&self.home, &self.agent, self.employee,
            &context.record().employee_binding, &self.catalog, tenant.as_ref().map(TenantSource::kind))?;
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
                return Ok((route, gi::google_arguments_digest(&digest), None));
            }
            // v1.81 E4: domain-wide delegation is a tenant source. The subject
            // is the admin-written selector, never an argument.
            Effective::Delegate { email } => {
                let source = tenant.as_ref().filter(|source| source.kind() == TenantSourceKind::Delegate
                    && source.subject() == email.as_str()).ok_or(VerificationError::BindingMismatch)?;
                let (route, digest) = self.tenant_selection(context, &routes, &requester, source, people.as_ref(), tool)?;
                return Ok((route, digest, Some(source.clone())));
            }
            // A configured service account replaces home OAuth on the home
            // path; in protected mode it is a tenant source, never SharedHome.
            Effective::Home => match tenant.as_ref() {
                Some(source) if source.kind() == TenantSourceKind::ServiceAccount => {
                    let (route, digest) =
                        self.tenant_selection(context, &routes, &requester, source, people.as_ref(), tool)?;
                    return Ok((route, digest, Some(source.clone())));
                }
                Some(_) => return Err(VerificationError::BindingMismatch),
                None => routes.home.ok_or(GoogleRoutesError::SetupRequired)?,
            },
            // This bridge does not reinterpret Apps Script material as OAuth.
            _ => return Err(VerificationError::Unavailable),
        };
        let source = home_source::read_home_native_selection(&self.home, tool)
            .map_err(|_| VerificationError::Unavailable)?;
        let digest = source.binding_digest().map_err(|_| VerificationError::Unavailable)?;
        route.recheck(&self.catalog)?;
        Ok((route, gi::google_arguments_digest(&json!({"source":digest})), None))
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
        Self::refuse_identity_arguments(args)?;
        let (route, digest, tenant) = self.decide(call.context(), tool)?;
        if route.product.account() != selected.account() || route.generation() != selected.generation()
            || route.kind() != selected.kind() {
            return Err(VerificationError::BindingMismatch);
        }
        // SR4 item 4: the call runs on exactly the source proof pinned at
        // selection, and the host lane is bound to that pinned tenant source
        // (it refuses a config it re-reads that differs from this decision).
        self.pin_source(tool, route.clone(), digest)?;
        let tenant = tenant.as_ref().map(TenantPin::of);
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
            super::effect_authority::execute_immediate_native_pinned(&self.home, self.users.as_ref(), &self.agent,
                &self.reference, tool, args.clone(), tenant).await.map_err(|_| VerificationError::Unavailable)
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
    fn candidates_for_call(&self, context: &RequestContext, tool: &str, provider: &str, arguments: &Value)
        -> Result<Vec<ProductConnectorAccount>, VerificationError> {
        Self::refuse_identity_arguments(arguments)?;
        self.candidates(context, tool, provider)
    }
}
impl GoogleIsolationBridge {
    /// An employee can never choose whose Google account is used.
    fn refuse_identity_arguments(arguments: &Value) -> Result<(), VerificationError> {
        let Some(object) = arguments.as_object() else { return Err(VerificationError::Invalid) };
        if object.keys().any(|key| IDENTITY_SELECTOR_ARGUMENTS.iter().any(|selector| *selector == key.as_str())) {
            return Err(VerificationError::BindingMismatch);
        }
        Ok(())
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
#[cfg(test)]
#[path = "isolation_tenant_tests.rs"]
mod tenant_tests;
