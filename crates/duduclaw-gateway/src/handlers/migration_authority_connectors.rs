//! Migration of pre-isolation connector accounts into catalog routes
//! (native Linux isolation v1.81, task C5; `ConfirmationCategory::Connector`).
//!
//! When an install that predates protected mode moves into it, its existing
//! connector accounts become catalog routes ONLY through the confirmation
//! flow: [`import_legacy`] reads the legacy product stores of this home and
//! stages one Connector proposal per account; an Admin approves it on the
//! migration card (MFA step-up, CSRF); apply creates the route and its C6
//! details through C3's provisioning code path
//! ([`isolation_connector_lifecycle::provision_migrated`]: same detail
//! validation, same catalog transaction, same readback). Nothing here writes
//! the catalog directly, and staging never touches it: a rejected or expired
//! proposal creates nothing.
//!
//! Legacy source → proposal (one per account; `legacy_source` key):
//! - `person_credentials/store.json` record (owner_kind `user`, provider
//!   google) whose `owner_id` is a users.db user → google, `Personal(owner)`
//!   (`google_personal:<owner>`). An owner the legacy data cannot tie to a
//!   user of this installation is not proposed at all (counted as
//!   `unproven_personal`), never downgraded to a shared route.
//! - home OAuth token store (`mcp-oauth-tokens.json`, presence only) for
//!   google/notion/github → that provider, SharedHome (`home_oauth:<p>`).
//! - `config.toml [odoo]` (non-empty) → odoo, SharedHome (`config:odoo`).
//! - `config.toml [db_sources.<name>]` granted by name in a confirmed
//!   protected employee's `agent.toml [capabilities] db_sources` → database,
//!   SharedHome, details `{employee, source}` (`database:<name>:<agent>`).
//!   Wildcard grants are not accounts and are not proposed.
//! - website credential (`secrets/website_credentials.enc`, metadata only)
//!   allowed for a confirmed protected employee → computer, details
//!   `{employee, credential, created_by}` (`computer:<id>:<agent>`);
//!   `Personal(owner)` only when the credential was created by user `owner`
//!   AND the employee is that owner's Personal employee; SharedHome for a
//!   Shared employee. SR4 item 6: a credential created by anyone else than
//!   the Personal employee's owner is not proposed (`unproven_personal`),
//!   and the card shows `created_by` (the creator's user id).
//! - `agent_bindings.json` channel user bound to a confirmed protected
//!   employee → channel, details `{employee, channel, scope = bound user,
//!   status_label = channel, token_ref}` (`channel:<ch>:<agent>:<user>`):
//!   the employee's own `agent.toml [channels.<ch>]` bot ⇒ `agent` with the
//!   employee's binding (Personal ⇒ `Personal(owner)`, Shared ⇒
//!   SharedTenant); otherwise a self-configuring channel with its home
//!   marker ⇒ `none`, or the home bot `config.toml [channels]` ⇒ `home`,
//!   both SharedHome. No bot ⇒ nothing proposed.
//!
//! Proposals carry references only: provider, label, binding kind, Personal
//! owner user id, the legacy source key and the C6 detail references. Never
//! a token, password, URL, email or tenant id; there is no `credential`
//! field. Approval and every apply step re-derive the proposal from the live
//! legacy stores (it must still be exactly what the import would stage),
//! re-resolve the binding from users.db and the employee from its lifecycle;
//! an unreadable legacy store refuses (import stages nothing at all).
//! A route id is never reused: a re-apply finds the route its proposal
//! already created (catalog provenance row) and a revoked one stays revoked.
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use duduclaw_isolation::capability::EmployeeBinding;
use duduclaw_migration::confirmation::{ConfirmationError as Error, ConfirmationStore, ProposalValidator};
use duduclaw_migration::contract::{ConfirmationCategory, ConfirmationItem, ConfirmationStatus, ProposalField,
    ProposalOperationId, ProposalValue};
use duduclaw_migration::ids::RequestId;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use crate::isolation_connector_details::{self as detail_store, AccountDetailRecord, ChannelTokenRef, DetailEmployee,
    DetailError, DetailReference};
use crate::isolation_connector_lifecycle::{self as route_lifecycle, AdminActor, LifecycleError as RouteError};
use crate::isolation_connectors::{CatalogError, ConnectorCatalog, ConnectorKind, MigrationOrigin, CATALOG_FILE};
use crate::isolation_lifecycle::{self as employees, LifecycleError as EmployeeError, RootAllocation, RootAllocator};

type Result<T> = std::result::Result<T, Error>;

/// Providers whose legacy accounts C5 migrates.
const PROVIDERS: [&str; 7] = ["google", "notion", "github", "odoo", "database", "computer", "channel"];
const HOME_OAUTH: [(&str, &str); 3] = [
    ("google", "Google Workspace（全公司）"), ("notion", "Notion（全公司）"), ("github", "GitHub（全公司）")];
const PERSONAL_GOOGLE_LABEL: &str = "Google 個人帳號";
const ODOO_LABEL: &str = "Odoo（全公司）";
const MAX_CANDIDATES: usize = 1024;
const MAX_AGENTS: usize = 4096;
const MAX_LABEL: usize = 128;

/// Root allocator and audit actor for the connector category. Production
/// uses the root `duduclaw-spawnd` ReadBack and the live dashboard Admin.
#[derive(Clone)]
pub(crate) struct ConnectorEnv {
    root: Arc<dyn RootAllocation + Send + Sync>,
    actor: Option<String>,
}
impl Default for ConnectorEnv {
    fn default() -> Self { Self { root: Arc::new(RootAllocator), actor: None } }
}
impl ConnectorEnv {
    #[cfg(test)]
    pub(crate) fn fixture(root: Arc<dyn RootAllocation + Send + Sync>, actor: &str) -> Self {
        Self { root, actor: Some(actor.to_owned()) }
    }
    /// The verified Admin performing the apply/compensation (audit only).
    pub(super) fn actor(&self, live_user: Option<&str>) -> Result<AdminActor> {
        let user = self.actor.as_deref().or(live_user).ok_or(Error::Denied)?;
        AdminActor::verified(user).map_err(|_| Error::Denied)
    }
}

/// One legacy account as a proposed route. Stored (encrypted) as the apply
/// plan; every step re-derives it from the live legacy stores.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ConnectorPlan {
    /// Legacy source key: identifies the legacy account, derives the proposal id.
    source: String,
    provider: String,
    /// `personal` | `shared_tenant` | `shared_home`.
    route_kind: String,
    label: String,
    /// Personal owner user id; `None` for shared kinds.
    owner: Option<String>,
    details: Option<DetailsPlan>,
}

/// C6 detail references (never a secret value).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DetailsPlan {
    employee: String,
    #[serde(default, skip_serializing_if = "Option::is_none")] source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] credential: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] channel: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] status_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] token_ref: Option<String>,
    /// SR4 item 6: the users.db id of the person who created the website
    /// credential (`computer` only), shown on the card and re-proven with the
    /// rest of the plan at approval and apply.
    #[serde(default, skip_serializing_if = "Option::is_none")] created_by: Option<String>,
}
impl DetailsPlan {
    fn empty(employee: &str) -> Self {
        Self { employee: employee.to_owned(), source: None, credential: None, channel: None, scope: None,
            status_label: None, token_ref: None, created_by: None }
    }
    /// Exactly the provider's references; anything else is invalid.
    fn reference(&self, provider: &str) -> Result<DetailReference> {
        let Self { employee: _, source, credential, channel, scope, status_label, token_ref, created_by } = self;
        let channel_fields = channel.is_some() || scope.is_some() || status_label.is_some() || token_ref.is_some();
        // The creator is part of a computer plan only, and always there.
        let creator_ok = match (provider, created_by) {
            ("computer", Some(creator)) => plain(creator, 128),
            ("computer", None) => false,
            (_, created_by) => created_by.is_none(),
        };
        if !creator_ok { return Err(Error::Invalid); }
        match (provider, source, credential) {
            ("database", Some(source), None) if !channel_fields => Ok(DetailReference::Database { source: source.clone() }),
            ("computer", None, Some(credential)) if !channel_fields =>
                Ok(DetailReference::Computer { credential: credential.clone() }),
            ("channel", None, None) => match (channel, scope, status_label, token_ref) {
                (Some(channel), Some(scope), Some(status_label), Some(token)) => Ok(DetailReference::Channel {
                    channel: channel.clone(), scope: scope.clone(), status_label: status_label.clone(),
                    token: ChannelTokenRef::parse(token).ok_or(Error::Invalid)?, extra_id: None }),
                _ => Err(Error::Invalid),
            },
            _ => Err(Error::Invalid),
        }
    }
}

/// The proposal's `settings` field: canonical JSON, exactly one encoding.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    schema: u8,
    source: String,
    provider: String,
    kind: String,
    details: Option<DetailsPlan>,
}

impl ConnectorPlan {
    fn shared_home(source: String, provider: &str, label: &str) -> Self {
        Self { source, provider: provider.to_owned(), route_kind: "shared_home".into(), label: label.to_owned(),
            owner: None, details: None }
    }

    /// Structural checks shared by staging, the card and apply.
    fn check(&self) -> Result<()> {
        if !PROVIDERS.contains(&self.provider.as_str()) || !plain(&self.label, MAX_LABEL) || !plain(&self.source, 512) {
            return Err(Error::Invalid);
        }
        let kind = route_kind(&self.route_kind).ok_or(Error::Invalid)?;
        match (kind, &self.owner) {
            (ConnectorKind::Personal, Some(owner)) if plain(owner, 128) => {}
            (ConnectorKind::SharedTenant | ConnectorKind::SharedHome, None) => {}
            _ => return Err(Error::Invalid),
        }
        match (detail_store::needs_details(&self.provider), &self.details) {
            (true, Some(details)) => {
                if !valid_agent(&details.employee) { return Err(Error::Invalid); }
                details.reference(&self.provider)?;
            }
            (false, None) => {}
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }

    /// The proposal fields, sorted like the confirmation store normalizes them.
    fn fields(&self) -> Result<Vec<ProposalField>> {
        let settings = serde_json::to_string(&Settings { schema: 1, source: self.source.clone(),
            provider: self.provider.clone(), kind: self.route_kind.clone(), details: self.details.clone() })?;
        let mut fields = vec![text_field("name", &self.label),
            text_field("binding_kind", if self.owner.is_some() { "personal" } else { "shared" }),
            text_field("settings", &settings)];
        if let Some(owner) = &self.owner { fields.push(text_field("owner_id", owner)); }
        fields.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(fields)
    }

    /// Apply step 1: re-validate against the live stores, then provision
    /// through C3 with this proposal's provenance.
    pub(super) fn publish(&self, home: &Path, item: &ConfirmationItem, env: &ConnectorEnv, actor: &AdminActor) -> Result<()> {
        if parse(item)? != *self { return Err(Error::Stale); }
        let fresh = fresh(home, env, self)?;
        let origin = origin(item)?;
        route_lifecycle::provision_migrated(home, &self.provider, fresh.kind, &fresh.binding, &self.label,
            fresh.details.as_ref().map(|(employee, reference)| (employee, reference)), &origin, actor)
            .map(|_| ()).map_err(route_error)
    }

    /// Apply step 2: the route this proposal created is active and is exactly
    /// what a fresh validation expects (provider, kind, binding, label, details).
    pub(super) fn read_back(&self, home: &Path, item: &ConfirmationItem, env: &ConnectorEnv) -> Result<()> {
        if parse(item)? != *self { return Err(Error::Stale); }
        let fresh = fresh(home, env, self)?;
        let origin = origin(item)?;
        let catalog = existing_catalog(home)?.ok_or(Error::Apply)?;
        let account = catalog.migrated_account(&origin).map_err(catalog_error)?.ok_or(Error::Apply)?;
        let (route, stored) = catalog.route_details(&account).map_err(catalog_error)?.ok_or(Error::Apply)?;
        let expected = fresh.details.map(|(employee, reference)| AccountDetailRecord {
            employee: employee.employee, agent: employee.agent, reference });
        if !route.active || route.provider != self.provider || route.kind != fresh.kind || route.binding != fresh.binding
            || route.label != self.label || stored != expected {
            return Err(Error::Apply);
        }
        Ok(())
    }

    /// Compensation (failed apply, reject after approval): revoke the route
    /// this proposal created through C3 revoke (fence, then drop material) and
    /// read the fence back. No route created ⇒ nothing to undo. Idempotent.
    pub(super) fn disable(&self, home: &Path, item: &ConfirmationItem, actor: &AdminActor) -> Result<()> {
        let origin = origin(item).map_err(|_| Error::Compensation)?;
        let Some(catalog) = existing_catalog(home).map_err(|_| Error::Compensation)? else { return Ok(()) };
        let Some(account) = catalog.migrated_account(&origin).map_err(|_| Error::Compensation)? else { return Ok(()) };
        drop(catalog);
        route_lifecycle::revoke(home, &account, actor).map_err(|_| Error::Compensation)?;
        let catalog = ConnectorCatalog::open(home).map_err(|_| Error::Compensation)?;
        let route = catalog.route(&account).map_err(|_| Error::Compensation)?.ok_or(Error::Compensation)?;
        if route.active { return Err(Error::Compensation); }
        Ok(())
    }
}

/// Parse a Connector confirmation item into its plan. Exact field set, one
/// canonical `settings` encoding, no secret field, and the proposal id must be
/// the one derived from the legacy source key.
pub(crate) fn parse(item: &ConfirmationItem) -> Result<ConnectorPlan> {
    if item.category != ConfirmationCategory::Connector { return Err(Error::Denied); }
    item.validate().map_err(|_| Error::Invalid)?;
    let (mut name, mut binding_kind, mut owner, mut settings) = (None, None, None, None);
    for field in &item.fields {
        let ProposalValue::Text { value } = &field.value else { return Err(Error::Denied) };
        let slot: &mut Option<&str> = match field.name.as_str() {
            "name" => &mut name, "binding_kind" => &mut binding_kind, "owner_id" => &mut owner,
            "settings" => &mut settings,
            // `credential`, `requires_reentry` and anything else: not a C5 proposal.
            _ => return Err(Error::Denied),
        };
        if slot.replace(value.as_str()).is_some() { return Err(Error::Invalid); }
    }
    let (label, binding_kind, settings) = (name.ok_or(Error::Invalid)?, binding_kind.ok_or(Error::Invalid)?,
        settings.ok_or(Error::Invalid)?);
    let decoded: Settings = serde_json::from_str(settings).map_err(|_| Error::Invalid)?;
    if decoded.schema != 1 { return Err(Error::Invalid); }
    let plan = ConnectorPlan { source: decoded.source, provider: decoded.provider, route_kind: decoded.kind,
        label: label.to_owned(), owner: owner.map(str::to_owned), details: decoded.details };
    plan.check()?;
    let expected_kind = if plan.owner.is_some() { "personal" } else { "shared" };
    if binding_kind != expected_kind { return Err(Error::Invalid); }
    let mut fields = item.fields.clone();
    fields.sort_by(|a, b| a.name.cmp(&b.name));
    if plan.fields()? != fields { return Err(Error::Invalid); }
    if item.binding.operation_id.proposal_id != proposal_id(&plan.source) { return Err(Error::Denied); }
    Ok(plan)
}

/// Approval / staging-time validation through the normal product checks.
pub(super) fn validate(home: &Path, item: &ConfirmationItem, env: &ConnectorEnv) -> Result<ConnectorPlan> {
    let plan = parse(item)?;
    fresh(home, env, &plan)?;
    Ok(plan)
}

/// Whether the migration card can offer apply for this item.
pub(crate) fn item_supported(item: &ConfirmationItem) -> bool { parse(item).is_ok() }

/// Admin card summary: provider, label, proposed binding/owner and the
/// detail references. No secret exists in a proposal to show.
///
/// SR4 item 6: `created_by` is the users.db id of the website credential's
/// creator (`computer` proposals; `null` otherwise), so the Admin sees whose
/// login a shared route would expose.
pub(crate) fn card(item: &ConfirmationItem) -> Option<Value> {
    let plan = parse(item).ok()?;
    let created_by = plan.details.as_ref().and_then(|details| details.created_by.clone());
    Some(json!({
        "provider": plan.provider, "label": plan.label, "kind": plan.route_kind,
        "binding": if plan.owner.is_some() { "personal" } else { "shared" },
        "owner_user_id": plan.owner, "details": plan.details, "legacy_source": plan.source,
        "created_by": created_by,
    }))
}

/// What one import run did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ImportSummary {
    /// New proposals (or new revisions of still-pending ones).
    pub(crate) staged: usize,
    /// Still pending with identical content.
    pub(crate) unchanged: usize,
    /// Already decided (approved, applied, rejected, expired): never re-staged.
    pub(crate) closed: usize,
    /// Personal credentials whose owner the legacy data does not prove.
    pub(crate) unproven_personal: usize,
}

/// Stage one Connector proposal per legacy connector account of this home.
/// Every legacy store is read before anything is written; any unreadable or
/// damaged store refuses the whole import. Never writes the catalog.
pub(crate) fn import_legacy(home: &Path) -> Result<ImportSummary> { import_legacy_with(home, &ConnectorEnv::default()) }

pub(crate) fn import_legacy_with(home: &Path, env: &ConnectorEnv) -> Result<ImportSummary> {
    let scanned = scan(home, env, None)?;
    let proposals = scanned.candidates.iter()
        .map(|plan| Ok((proposal_id(&plan.source), plan.fields()?)))
        .collect::<Result<Vec<(RequestId, Vec<ProposalField>)>>>()?;
    let mut summary = ImportSummary { unproven_personal: scanned.unproven, ..ImportSummary::default() };
    if proposals.is_empty() { return Ok(summary); }
    let mut store = open_confirmations(home)?;
    let items = store.list()?;
    for (id, fields) in proposals {
        let latest = items.iter().filter(|item| item.binding.operation_id.proposal_id == id)
            .max_by_key(|item| item.binding.operation_id.revision);
        // The decision status is checked before the content: a decided (or
        // in-flight) proposal is closed whatever the legacy stores say now.
        let revision = match latest {
            None => 1,
            Some(item) if item.category == ConfirmationCategory::Connector && item.status == ConfirmationStatus::Pending => {
                if item.fields == fields {
                    summary.unchanged += 1;
                    continue;
                }
                // Changed legacy content supersedes only an undecided proposal.
                item.binding.operation_id.revision.checked_add(1).ok_or(Error::Limit)?
            }
            // Approved, applying, applied, failed, rejecting, rejected,
            // expired, or another category's id: never re-staged.
            Some(_) => {
                summary.closed += 1;
                continue;
            }
        };
        store.stage_proposal(ProposalOperationId { proposal_id: id, revision }, ConfirmationCategory::Connector,
            fields, &Structural)?;
        summary.staged += 1;
    }
    Ok(summary)
}

/// Staging validator: structure only. Approval runs the full [`validate`].
struct Structural;
impl ProposalValidator for Structural {
    fn validate_fresh(&self, item: &ConfirmationItem) -> Result<()> { parse(item).map(|_| ()) }
}

// ── Fresh validation ────────────────────────────────────────────────────────

struct Fresh {
    kind: ConnectorKind,
    binding: EmployeeBinding,
    details: Option<(DetailEmployee, DetailReference)>,
}

/// The plan must still be exactly what the legacy stores derive; then the
/// binding (users.db) and the employee (lifecycle) are resolved fresh and the
/// details go through C3/C6's own validation.
fn fresh(home: &Path, env: &ConnectorEnv, plan: &ConnectorPlan) -> Result<Fresh> {
    plan.check()?;
    let scanned = scan(home, env, Some(plan))?;
    let mut matching = scanned.candidates.iter().filter(|candidate| candidate.source == plan.source);
    let candidate = matching.next().ok_or(Error::Stale)?;
    if matching.next().is_some() || candidate != plan { return Err(Error::Stale); }
    let kind = route_kind(&plan.route_kind).ok_or(Error::Invalid)?;
    let binding = employees::connector_route_binding(home, plan.owner.as_deref()).map_err(employee_error)?;
    if !kind.admits(&binding) { return Err(Error::Invalid); }
    let details = match &plan.details {
        None => None,
        Some(requested) => {
            let employee = Employees::new(home, env)?.get(&requested.employee)?.ok_or(Error::Denied)?;
            let reference = requested.reference(&plan.provider)?;
            detail_store::validate_request(home, &plan.provider, kind, &binding, &employee, &reference)
                .map_err(detail_error)?;
            Some((employee, reference))
        }
    };
    Ok(Fresh { kind, binding, details })
}

// ── Legacy scan ─────────────────────────────────────────────────────────────

struct Scan { candidates: Vec<ConnectorPlan>, unproven: usize }

/// Derive the proposed routes from the legacy stores. `only` limits the scan
/// to the stores (and employee) that can produce that one plan; the same code
/// derives proposals at import and re-proves them at approval/apply.
fn scan(home: &Path, env: &ConnectorEnv, only: Option<&ConnectorPlan>) -> Result<Scan> {
    let wants = |provider: &str, kind: Option<&str>| only.is_none_or(|plan| plan.provider == provider
        && kind.is_none_or(|kind| plan.route_kind == kind));
    let only_agent = only.map(|plan| plan.details.as_ref().map(|details| details.employee.as_str()).unwrap_or(""));
    let mut out = Scan { candidates: Vec::new(), unproven: 0 };
    if wants("google", Some("personal")) { google_personal(home, &mut out)?; }
    if HOME_OAUTH.iter().any(|(provider, _)| wants(*provider, Some("shared_home"))) {
        let present = crate::mcp_oauth::stored_home_providers(home).map_err(|_| Error::Storage)?;
        for (provider, label) in HOME_OAUTH {
            if wants(provider, Some("shared_home")) && present.iter().any(|stored| stored == provider) {
                out.candidates.push(ConnectorPlan::shared_home(format!("home_oauth:{provider}"), provider, label));
            }
        }
    }
    let agent_family = wants("database", None) || wants("computer", None) || wants("channel", None);
    let config = if wants("odoo", None) || agent_family { read_toml(&home.join("config.toml"))? } else { toml::Table::new() };
    if wants("odoo", None) {
        match config.get("odoo") {
            None => {}
            Some(toml::Value::Table(odoo)) => if !odoo.is_empty() {
                out.candidates.push(ConnectorPlan::shared_home("config:odoo".into(), "odoo", ODOO_LABEL));
            },
            Some(_) => return Err(Error::Storage),
        }
    }
    if agent_family {
        let agents = agent_configs(home, only_agent)?;
        let mut staff = Employees::new(home, env)?;
        if wants("database", None) { databases(&config, &agents, &mut staff, &mut out)?; }
        if wants("computer", None) { computers(home, only_agent, &mut staff, &mut out)?; }
        if wants("channel", None) { channels(home, &config, &agents, only_agent, &mut staff, &mut out)?; }
    }
    out.candidates.sort_by(|a, b| a.source.cmp(&b.source));
    if out.candidates.len() > MAX_CANDIDATES { return Err(Error::Limit); }
    if out.candidates.windows(2).any(|pair| pair[0].source == pair[1].source) { return Err(Error::Invalid); }
    for candidate in &out.candidates { candidate.check()?; }
    Ok(out)
}

fn google_personal(home: &Path, out: &mut Scan) -> Result<()> {
    let records = crate::person_credentials::all_metadata(home).map_err(|_| Error::Storage)?;
    for record in records {
        // Proof of ownership: a user record of this installation with exactly
        // the legacy owner id. Anything less is not proposed as Personal.
        if record.owner_kind != "user" || record.provider != "google" || !plain(&record.owner_id, 128)
            || !employees::user_exists(home, &record.owner_id).map_err(employee_error)? {
            out.unproven += 1;
            continue;
        }
        out.candidates.push(ConnectorPlan { source: format!("google_personal:{}", record.owner_id),
            provider: "google".into(), route_kind: "personal".into(), label: PERSONAL_GOOGLE_LABEL.into(),
            owner: Some(record.owner_id), details: None });
    }
    Ok(())
}

fn databases(config: &toml::Table, agents: &BTreeMap<String, toml::Table>, staff: &mut Employees<'_>,
    out: &mut Scan) -> Result<()> {
    let sources = match config.get("db_sources") {
        None => return Ok(()),
        Some(toml::Value::Table(sources)) => sources,
        Some(_) => return Err(Error::Storage),
    };
    for (agent, table) in agents {
        let granted = match table.get("capabilities").and_then(|capabilities| capabilities.get("db_sources")) {
            None => continue,
            Some(toml::Value::Array(granted)) => granted,
            Some(_) => return Err(Error::Storage),
        };
        let mut names = BTreeSet::new();
        for name in granted { names.insert(name.as_str().ok_or(Error::Storage)?); }
        for name in names {
            // Exact registered names only; a wildcard grant names no account.
            if !duduclaw_db::config::is_valid_source_name(name) || !sources.get(name).is_some_and(toml::Value::is_table) {
                continue;
            }
            let Some(employee) = staff.get(agent)? else { continue };
            let mut details = DetailsPlan::empty(&employee.agent);
            details.source = Some(name.to_owned());
            out.candidates.push(ConnectorPlan { source: format!("database:{name}:{agent}"), provider: "database".into(),
                route_kind: "shared_home".into(), label: bounded_label("資料庫 ", name), owner: None,
                details: Some(details) });
        }
    }
    Ok(())
}

fn computers(home: &Path, only_agent: Option<&str>, staff: &mut Employees<'_>, out: &mut Scan) -> Result<()> {
    let records = crate::website_credentials::list(home).map_err(|_| Error::Storage)?;
    for record in records {
        if uuid::Uuid::parse_str(&record.id).is_err() { return Err(Error::Storage); }
        let allowed: BTreeSet<&str> = record.allowed_agents.iter().map(String::as_str).collect();
        for agent in allowed {
            if only_agent.is_some_and(|only| only != agent) { continue; }
            let Some(employee) = staff.get(agent)? else { continue };
            let Some((route_kind, owner)) = computer_route(&record.created_by, &employee.binding) else {
                // SR4 item 6: another person's login on someone's Personal
                // employee is never proposed (not even as a shared route).
                out.unproven += 1;
                continue;
            };
            let mut details = DetailsPlan::empty(&employee.agent);
            details.credential = Some(record.id.clone());
            details.created_by = Some(record.created_by.owner_id.clone());
            out.candidates.push(ConnectorPlan { source: format!("computer:{}:{agent}", record.id),
                provider: "computer".into(), route_kind: route_kind.into(), label: bounded_label("網站登入 ", &record.label),
                owner, details: Some(details) });
        }
    }
    Ok(())
}

/// SR4 item 6: the route a website credential allowed for an employee with
/// `binding` becomes. `None` ⇒ not proposed (counted `unproven_personal`):
/// a creator that is not a plain users.db user id, or a Personal employee
/// whose owner is not the credential's creator — another person's login is
/// never placed on someone's Personal employee, not even as a shared route.
fn computer_route(created_by: &crate::website_credentials::Owner, binding: &EmployeeBinding)
    -> Option<(&'static str, Option<String>)> {
    if created_by.owner_kind != "user" || !plain(&created_by.owner_id, 128) { return None; }
    match binding {
        EmployeeBinding::Personal(owner) if owner.as_str() == created_by.owner_id =>
            Some(("personal", Some(created_by.owner_id.clone()))),
        EmployeeBinding::Personal(_) => None,
        EmployeeBinding::Shared(_) => Some(("shared_home", None)),
    }
}

fn channels(home: &Path, config: &toml::Table, agents: &BTreeMap<String, toml::Table>, only_agent: Option<&str>,
    staff: &mut Employees<'_>, out: &mut Scan) -> Result<()> {
    let raw = super::read_optional(&home.join("agent_bindings.json"))?;
    if raw.trim().is_empty() { return Ok(()); }
    let state: Value = serde_json::from_str(&raw).map_err(|_| Error::Storage)?;
    // Only the durable bindings are read; pending bind-token digests are not.
    let Some(bindings) = state.get("bindings") else { return Ok(()) };
    for (channel, users) in bindings.as_object().ok_or(Error::Storage)? {
        let users = users.as_object().ok_or(Error::Storage)?;
        if !crate::channel_settings::VALID_CHANNEL_TYPES.contains(&channel.as_str()) { continue; }
        for (user, agent) in users {
            let agent = agent.as_str().ok_or(Error::Storage)?;
            // Bounded so the source key stays within its limit; never a wildcard.
            if only_agent.is_some_and(|only| only != agent) || !plain(user, 256) || user == "*" { continue; }
            let Some(employee) = staff.get(agent)? else { continue };
            let own_bot = agents.get(agent).is_some_and(|table| agent_bot(table, channel));
            let (token, route_kind, owner) = if own_bot {
                match &employee.binding {
                    EmployeeBinding::Personal(owner) => (ChannelTokenRef::Agent, "personal", Some(owner.as_str().to_owned())),
                    EmployeeBinding::Shared(_) => (ChannelTokenRef::Agent, "shared_tenant", None),
                }
            } else if crate::channel_sender::sender_self_configures(channel) {
                let Some(marker) = crate::channel_sender::self_config_marker_field(channel) else { continue };
                if !home_secret(config, marker) { continue; }
                (ChannelTokenRef::None, "shared_home", None)
            } else {
                let Some(field) = crate::otp_delivery::token_field(channel) else { continue };
                if !home_secret(config, field) { continue; }
                (ChannelTokenRef::Home, "shared_home", None)
            };
            let mut details = DetailsPlan::empty(&employee.agent);
            details.channel = Some(channel.clone());
            details.scope = Some(user.clone());
            details.status_label = Some(channel.clone());
            details.token_ref = Some(token.as_str().to_owned());
            out.candidates.push(ConnectorPlan { source: format!("channel:{channel}:{agent}:{user}"),
                provider: "channel".into(), route_kind: route_kind.into(), label: bounded_label(channel, " 機器人"),
                owner, details: Some(details) });
        }
    }
    Ok(())
}

/// The agent's own `[channels.<channel>]` bot token (plain or `_enc`) is set.
fn agent_bot(table: &toml::Table, channel: &str) -> bool {
    table.get("channels").and_then(|channels| channels.get(channel)).and_then(toml::Value::as_table)
        .is_some_and(|section| ["bot_token_enc", "bot_token"].iter()
            .any(|key| section.get(*key).and_then(toml::Value::as_str).is_some_and(|value| !value.is_empty())))
}
fn home_secret(config: &toml::Table, field: &str) -> bool {
    crate::config_crypto::describe_config_field(config, "channels", field).configured
}

/// Confirmed protected employees, read lazily through the A2 lifecycle.
struct Employees<'a> {
    home: &'a Path,
    env: &'a ConnectorEnv,
    confirmed: bool,
    cache: BTreeMap<String, Option<DetailEmployee>>,
}
impl<'a> Employees<'a> {
    fn new(home: &'a Path, env: &'a ConnectorEnv) -> Result<Self> {
        // No confirmed-employee index yet ⇒ no protected employee to pin details to.
        let confirmed = match std::fs::symlink_metadata(home.join("migration-authority").join("apply.db")) {
            Ok(metadata) if metadata.file_type().is_file() => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            _ => return Err(Error::Storage),
        };
        Ok(Self { home, env, confirmed, cache: BTreeMap::new() })
    }
    /// `None` for an agent that is not a live confirmed protected employee;
    /// an unreadable lifecycle store refuses.
    fn get(&mut self, agent: &str) -> Result<Option<DetailEmployee>> {
        if !self.confirmed || !valid_agent(agent) { return Ok(None); }
        if let Some(known) = self.cache.get(agent) { return Ok(known.clone()); }
        let found = match employees::read_lifecycle_with(self.home, agent, self.env.root.as_ref()) {
            Ok(lifecycle) if lifecycle.label == agent =>
                Some(DetailEmployee { employee: lifecycle.employee, agent: lifecycle.label, binding: lifecycle.binding }),
            Ok(_) => None,
            Err(EmployeeError::Unavailable) => return Err(Error::Storage),
            Err(_) => None,
        };
        if self.cache.len() >= MAX_AGENTS { return Err(Error::Limit); }
        self.cache.insert(agent.to_owned(), found.clone());
        Ok(found)
    }
}

/// `agents/<id>/agent.toml` of every agent directory (or only `only`).
fn agent_configs(home: &Path, only: Option<&str>) -> Result<BTreeMap<String, toml::Table>> {
    let mut configs = BTreeMap::new();
    let entries = match std::fs::read_dir(home.join("agents")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(configs),
        Err(_) => return Err(Error::Storage),
    };
    for entry in entries {
        let entry = entry.map_err(|_| Error::Storage)?;
        let Ok(name) = entry.file_name().into_string() else { continue };
        if !valid_agent(&name) || only.is_some_and(|only| only != name) { continue; }
        if !std::fs::symlink_metadata(entry.path()).map_err(|_| Error::Storage)?.file_type().is_dir() { continue; }
        let table = read_toml(&entry.path().join("agent.toml"))?;
        if table.is_empty() { continue; }
        if configs.len() >= MAX_AGENTS { return Err(Error::Limit); }
        configs.insert(name, table);
    }
    Ok(configs)
}

/// A regular file (≤ 1 MiB, never followed through a symlink) parsed as
/// TOML; absent ⇒ empty. Unreadable or malformed ⇒ `Storage` (refuse).
fn read_toml(path: &Path) -> Result<toml::Table> {
    let raw = super::read_optional(path)?;
    if raw.trim().is_empty() { return Ok(toml::Table::new()); }
    raw.parse().map_err(|_| Error::Storage)
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Domain-separated SHA-256 of the legacy source key, first 16 bytes. One
/// legacy account always maps to the same proposal id, so a re-import never
/// stages a second proposal (and never re-proposes a decided one).
fn proposal_id(source: &str) -> RequestId {
    let mut hash = Sha256::new();
    hash.update(b"duduclaw-c5-connector-proposal-v1\0");
    hash.update(source.as_bytes());
    let digest: [u8; 32] = hash.finalize().into();
    let mut id = [0u8; 16];
    id.iter_mut().zip(digest.iter()).for_each(|(out, byte)| *out = *byte);
    RequestId(id)
}

fn origin(item: &ConfirmationItem) -> Result<MigrationOrigin> {
    MigrationOrigin::new(&super::operation(&item.binding), &hex::encode(item.binding.proposal_digest.0))
        .map_err(|_| Error::Invalid)
}

/// The catalog only if it already exists (compensation/readback never create it).
fn existing_catalog(home: &Path) -> Result<Option<ConnectorCatalog>> {
    match std::fs::symlink_metadata(home.join(CATALOG_FILE)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Ok(_) => ConnectorCatalog::open(home).map(Some).map_err(|_| Error::Storage),
        Err(_) => Err(Error::Storage),
    }
}

fn open_confirmations(home: &Path) -> Result<ConfirmationStore> {
    let path = home.join("migration-confirmations.db");
    match std::fs::symlink_metadata(&path) {
        // Same creation as import staging (private file, schema, no proposals).
        Err(error) if error.kind() == std::io::ErrorKind::NotFound =>
            super::stage_import_proposals(home, &[]).map_err(|_| Error::Storage)?,
        Ok(metadata) if metadata.file_type().is_file() && metadata.len() <= 64 * 1024 * 1024 => {}
        _ => return Err(Error::Storage),
    }
    ConfirmationStore::open_existing(&path)
}

fn route_kind(kind: &str) -> Option<ConnectorKind> {
    Some(match kind {
        "personal" => ConnectorKind::Personal,
        "shared_tenant" => ConnectorKind::SharedTenant,
        "shared_home" => ConnectorKind::SharedHome,
        _ => return None,
    })
}
fn plain(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}
fn valid_agent(agent: &str) -> bool {
    duduclaw_core::is_valid_agent_id(agent) && !duduclaw_core::is_reserved_agent_id(agent)
        && agent != duduclaw_core::UNTRUSTED_AGENT_ID && agent.len() <= 128
}
/// `prefix` + `value` cut at a character boundary to the catalog label limit.
fn bounded_label(prefix: &str, value: &str) -> String {
    let mut label = String::new();
    for c in prefix.chars().chain(value.chars()).filter(|c| !c.is_control()) {
        if label.len() + c.len_utf8() > MAX_LABEL { break; }
        label.push(c);
    }
    label.trim().to_owned()
}
fn text_field(name: &str, value: &str) -> ProposalField {
    ProposalField { name: name.to_owned(), value: ProposalValue::Text { value: value.to_owned() } }
}
fn employee_error(error: EmployeeError) -> Error {
    match error {
        EmployeeError::Unavailable => Error::Storage,
        EmployeeError::Denied => Error::Denied,
        EmployeeError::Stale | EmployeeError::Tombstoned => Error::Stale,
    }
}
fn detail_error(error: DetailError) -> Error {
    match error { DetailError::Unavailable => Error::Storage, _ => Error::Denied }
}
fn catalog_error(error: CatalogError) -> Error {
    match error { CatalogError::Unavailable => Error::Storage, CatalogError::Revoked => Error::Stale, _ => Error::Apply }
}
fn route_error(error: RouteError) -> Error {
    match error {
        RouteError::Catalog(CatalogError::Unavailable) | RouteError::Detail(DetailError::Unavailable) => Error::Storage,
        RouteError::Catalog(CatalogError::Revoked) | RouteError::Conflict => Error::Stale,
        RouteError::Catalog(_) | RouteError::Detail(_) => Error::Denied,
        RouteError::MaterialPending => Error::Compensation,
    }
}

#[cfg(test)]
#[path = "migration_authority_connectors_tests.rs"]
mod tests;
