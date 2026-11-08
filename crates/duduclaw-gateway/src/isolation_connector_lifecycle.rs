//! Admin provision/revoke of isolated connector routes (C3).
//!
//! Migration (C5) uses [`provision_migrated`]: the same validation,
//! transaction and readback, plus a provenance row naming the applied
//! confirmation proposal, so a re-apply never mints a second id.
//!
//! Provision registers exactly the one route an authenticated Admin asked for.
//! It never reads, scans or imports existing credentials, product OAuth files
//! or the legacy home account to create routes; product credential events
//! (C4) and migration confirmation (C5) are the only other writers.
//!
//! Revoke deactivates the catalog route FIRST. That commit moves the route's
//! generation and clears `active`, so every pinned `SelectedConnector` refuses
//! at its next `recheck_binding` (before credential acquisition, each request
//! and each retry) and the boundary sweep sees an inactive route. Only after
//! the deactivation is read back is in-process credential material dropped
//! (`AccountCredentialVault::remove`). A failure after the fence leaves the
//! route revoked; repeating revoke is safe and retries the removal.
//!
//! Database, computer (website login) and channel routes are provisioned
//! together with their per-account details (C6): the references are
//! validated against their own registries for the named protected employee,
//! and the route + details rows are committed in one catalog transaction.
//! Revoke deletes the details in the same transaction that fences the route.
//!
//! Audit rows carry the actor, opaque account id, provider, kind, binding
//! type, Personal owner user id, generation and (for details) the employee
//! id. Never labels, tokens, token references' values, tenant ids or email
//! addresses.
use std::{path::Path, sync::{Arc, OnceLock}};
use duduclaw_isolation::capability::EmployeeBinding;
use duduclaw_mcp_boundary::VerificationError;
use serde_json::{Value, json};
use crate::isolation_connector_clients::AccountCredentialVault;
use crate::isolation_connector_details::{self as detail_store, AccountDetailRecord, DetailEmployee, DetailError, DetailReference};
use crate::isolation_connectors::{AccountId, CatalogError, ConnectorCatalog, ConnectorKind, MigratedRoute, MigrationOrigin, Route};

/// Non-sensitive lifecycle failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LifecycleError {
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    /// The account details were missing, not applicable, or refused by
    /// their registry. Nothing was registered.
    #[error(transparent)]
    Detail(#[from] DetailError),
    /// The route is revoked and fenced, but in-process credential material
    /// could not be dropped. Repeating revoke retries only the removal.
    #[error("connector route revoked; credential removal pending")]
    MaterialPending,
    /// C5 migration only: another route already covers this account
    /// ([`MigratedRoute::Conflict`]). Nothing was registered.
    #[error("connector account already has a route")]
    Conflict,
}

/// The administrator performing a lifecycle change. Construct only after the
/// caller was re-read as a current Admin and presented a fresh step-up proof;
/// the value is used for audit attribution and carries no authority itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminActor { user_id: String }
impl AdminActor {
    pub fn verified(user_id: &str) -> Result<Self, CatalogError> {
        if user_id.is_empty() || user_id.len() > 128 || user_id == "system" || user_id.chars().any(char::is_control) {
            return Err(CatalogError::Invalid);
        }
        Ok(Self { user_id: user_id.to_owned() })
    }
    pub fn user_id(&self) -> &str { &self.user_id }
}

/// In-process credential material keyed by catalog account id. Implementations
/// must be idempotent: dropping an absent account succeeds.
pub trait CredentialMaterial: Send + Sync {
    fn drop_material(&self, account: &AccountId) -> Result<(), VerificationError>;
}
impl CredentialMaterial for AccountCredentialVault {
    fn drop_material(&self, account: &AccountId) -> Result<(), VerificationError> {
        self.remove(account.as_str())
    }
}

static MATERIAL: OnceLock<Arc<dyn CredentialMaterial>> = OnceLock::new();

/// Boot installs the process vault once (F1). Without it, revoke still fences
/// the route; there is then no in-process material to drop.
pub fn install_credential_material(material: Arc<dyn CredentialMaterial>) -> Result<(), CatalogError> {
    MATERIAL.set(material).map_err(|_| CatalogError::Unavailable)
}

/// The process vault installed by boot, if any. C4 credential events drop
/// in-process material through it after their catalog fence.
pub(crate) fn installed_material() -> Option<&'static dyn CredentialMaterial> {
    MATERIAL.get().map(|material| material.as_ref())
}

/// Register one route for an account the admin provisioned. `label` is a
/// display hint only; the id is always newly minted (a relink never reuses
/// one). `kind` must agree with `binding`. No credential store is consulted
/// to create routes.
///
/// `details` is required exactly for the providers in
/// [`detail_store::DETAIL_PROVIDERS`] and refused for every other provider.
/// The employee must come from the employee lifecycle (never a request
/// field); its references are validated by [`detail_store::validate_request`]
/// and written with the route in one transaction.
pub fn provision(home: &Path, provider: &str, kind: ConnectorKind, binding: &EmployeeBinding, label: &str,
    details: Option<(&DetailEmployee, &DetailReference)>, actor: &AdminActor) -> Result<AccountId, LifecycleError> {
    provision_with(home, provider, kind, binding, label, details, actor, None)
}

/// C5: [`provision`] for an applied migration Connector proposal. Exactly
/// the same detail validation, catalog transaction and readback, with the
/// proposal's [`MigrationOrigin`] recorded in that same transaction. A
/// re-apply of the same origin returns (and reads back) the route it already
/// created instead of minting another id; if that route was revoked the
/// readback refuses with `Revoked` and nothing new is registered. Another
/// route already covering the account refuses with [`LifecycleError::Conflict`].
/// The caller has already re-validated the proposal (legacy evidence,
/// binding, employee) against the live product state.
#[allow(clippy::too_many_arguments)]
pub(crate) fn provision_migrated(home: &Path, provider: &str, kind: ConnectorKind, binding: &EmployeeBinding,
    label: &str, details: Option<(&DetailEmployee, &DetailReference)>, origin: &MigrationOrigin, actor: &AdminActor)
    -> Result<AccountId, LifecycleError> {
    provision_with(home, provider, kind, binding, label, details, actor, Some(origin))
}

#[allow(clippy::too_many_arguments)]
fn provision_with(home: &Path, provider: &str, kind: ConnectorKind, binding: &EmployeeBinding, label: &str,
    details: Option<(&DetailEmployee, &DetailReference)>, actor: &AdminActor, origin: Option<&MigrationOrigin>)
    -> Result<AccountId, LifecycleError> {
    let refused = |status: &str| json!({"provider": provider_name(provider), "kind": kind.as_str(),
        "binding_type": binding_type(binding), "status": status, "origin": origin_name(origin)});
    let record = match (detail_store::needs_details(provider), details) {
        (true, Some((employee, reference))) =>
            detail_store::validate_request(home, provider, kind, binding, employee, reference).map(Some),
        (true, None) => Err(DetailError::Required),
        (false, Some(_)) => Err(DetailError::NotApplicable),
        (false, None) => Ok(None),
    };
    let record = record.and_then(|record| google_tenant_source_gate(home, provider, kind).map(|()| record));
    let record = match record {
        Ok(record) => record,
        Err(error) => {
            audit(home, "isolation.connector_provision_refused", actor, refused(error.code()));
            return Err(error.into());
        }
    };
    let result = ConnectorCatalog::open(home)
        .and_then(|catalog| {
            let registered = match (origin, &record) {
                (None, Some(record)) => catalog.register_with_details(label, provider, kind, binding, record)
                    .map(|account| Some((account, false))),
                (None, None) => catalog.register(label, provider, kind, binding).map(|account| Some((account, false))),
                (Some(origin), record) => match catalog.register_migrated(origin, label, provider, kind, binding, record.as_ref())? {
                    MigratedRoute::Created(account) => Ok(Some((account, false))),
                    MigratedRoute::Existing(account) => Ok(Some((account, true))),
                    MigratedRoute::Conflict => Ok(None),
                },
            }?;
            Ok(registered.map(|(account, replayed)| (catalog, account, replayed)))
        })
        .and_then(|registered| {
            let Some((catalog, account, replayed)) = registered else { return Ok(None) };
            // Read back what selection and the details reader will read.
            let (route, stored) = catalog.route_details(&account)?.ok_or(CatalogError::Unavailable)?;
            // A replayed migration route that was revoked stays revoked.
            if !route.active { return Err(if replayed { CatalogError::Revoked } else { CatalogError::Unavailable }); }
            if route.provider != provider || route.kind != kind || &route.binding != binding
                || (origin.is_some() && route.label != label) || stored != record {
                return Err(CatalogError::Unavailable);
            }
            Ok(Some((route, replayed)))
        });
    match result {
        Ok(Some((route, replayed))) => {
            let mut audited = route_details(&route, if replayed { "provisioned_replay" } else { "provisioned" });
            audited["details"] = json!(record.is_some());
            audited["details_employee"] = json!(record.as_ref().map(|record: &AccountDetailRecord| hex::encode(record.employee.0)));
            audited["origin"] = origin_name(origin);
            audit(home, "isolation.connector_provisioned", actor, audited);
            Ok(route.account)
        }
        Ok(None) => {
            audit(home, "isolation.connector_provision_refused", actor, refused("conflict"));
            Err(LifecycleError::Conflict)
        }
        Err(error) => {
            audit(home, "isolation.connector_provision_refused", actor, refused(refusal(error)));
            Err(error.into())
        }
    }
}

/// Audit marker for where a provision came from; never the operation id.
fn origin_name(origin: Option<&MigrationOrigin>) -> Value {
    Value::String(if origin.is_some() { "migration" } else { "admin" }.to_owned())
}

/// v1.81 E4: a Google SharedTenant route stands for a tenant credential
/// source (sealed delegation key or `[integrations.google_service_account]`).
/// Refuse to register one while no such source is installed, so a tenant
/// route can never be provisioned onto the home credential. The subject and
/// requester rules are enforced per call by the Google bridge.
fn google_tenant_source_gate(home: &Path, provider: &str, kind: ConnectorKind) -> Result<(), DetailError> {
    if provider != "google" || kind != ConnectorKind::SharedTenant { return Ok(()); }
    match crate::google_identity::tenant_source::installed(home) {
        Ok(true) => Ok(()),
        Ok(false) => Err(DetailError::Unknown),
        Err(_) => Err(DetailError::Unavailable),
    }
}

/// Only a known provider name is ever written to the audit log.
fn provider_name(provider: &str) -> Value {
    const KNOWN: [&str; 9] = ["google", "notion", "github", "odoo", "database", "channel", "search", "model", "computer"];
    KNOWN.iter().find(|known| **known == provider).map_or(Value::Null, |known| Value::String((*known).to_owned()))
}

/// Revoke a route (final): fence through the catalog, then drop material.
pub fn revoke(home: &Path, account: &AccountId, actor: &AdminActor) -> Result<Route, LifecycleError> {
    revoke_with(home, account, actor, MATERIAL.get().map(|material| material.as_ref()))
}

pub(crate) fn revoke_with(home: &Path, account: &AccountId, actor: &AdminActor,
    material: Option<&dyn CredentialMaterial>) -> Result<Route, LifecycleError> {
    let fenced = ConnectorCatalog::open(home).and_then(|catalog| {
        // Same transaction: deactivate, move the generation, delete details.
        catalog.revoke(account)?;
        // The fence must be durable and visible before any material goes away.
        let (route, details) = catalog.route_details(account)?.ok_or(CatalogError::Unavailable)?;
        if route.active || &route.account != account || details.is_some() { return Err(CatalogError::Unavailable); }
        Ok(route)
    });
    let route = match fenced {
        Ok(route) => route,
        Err(error) => {
            audit(home, "isolation.connector_revoke_refused", actor,
                json!({"account": account.as_str(), "status": refusal(error)}));
            return Err(error.into());
        }
    };
    audit(home, "isolation.connector_revoked", actor, route_details(&route, "fenced"));
    let Some(material) = material else { return Ok(route) };
    if material.drop_material(account).is_err() {
        audit(home, "isolation.connector_material_removal_failed", actor,
            json!({"account": account.as_str(), "provider": route.provider, "status": "pending"}));
        return Err(LifecycleError::MaterialPending);
    }
    audit(home, "isolation.connector_material_removed", actor,
        json!({"account": account.as_str(), "provider": route.provider, "status": "removed"}));
    Ok(route)
}

pub(crate) fn binding_type(binding: &EmployeeBinding) -> &'static str {
    match binding { EmployeeBinding::Personal(_) => "personal", EmployeeBinding::Shared(_) => "shared" }
}
fn route_details(route: &Route, status: &str) -> Value {
    // Personal owner is a user id admins already see; a tenant id is never written.
    let owner = match &route.binding {
        EmployeeBinding::Personal(owner) => Value::String(owner.as_str().to_owned()),
        EmployeeBinding::Shared(_) => Value::Null,
    };
    json!({"account": route.account.as_str(), "provider": route.provider, "kind": route.kind.as_str(),
        "binding_type": binding_type(&route.binding), "owner_user_id": owner,
        "generation": route.generation, "active": route.active, "status": status})
}
fn refusal(error: CatalogError) -> &'static str {
    match error {
        CatalogError::Unavailable => "unavailable",
        CatalogError::Invalid => "invalid",
        CatalogError::NotFound => "not_found",
        CatalogError::Revoked => "revoked",
    }
}
fn audit(home: &Path, event: &str, actor: &AdminActor, mut details: Value) {
    details["actor_user_id"] = Value::String(actor.user_id.clone());
    crate::security_autopilot::audit_and_emit(home, &duduclaw_security::audit::AuditEvent::new(
        event, "isolation-connectors", duduclaw_security::audit::Severity::Info, details));
}

#[cfg(test)]
#[path = "isolation_connector_lifecycle_tests.rs"]
mod tests;
