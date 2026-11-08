//! Admin provision/revoke of isolated connector routes (C3).
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
//! Audit rows carry the actor, opaque account id, provider, kind, binding
//! type, Personal owner user id and generation. Never labels, tokens, tenant
//! ids or email addresses.
use std::{path::Path, sync::{Arc, OnceLock}};
use duduclaw_isolation::capability::EmployeeBinding;
use duduclaw_mcp_boundary::VerificationError;
use serde_json::{Value, json};
use crate::isolation_connector_clients::AccountCredentialVault;
use crate::isolation_connectors::{AccountId, CatalogError, ConnectorCatalog, ConnectorKind, Route};

/// Non-sensitive lifecycle failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LifecycleError {
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    /// The route is revoked and fenced, but in-process credential material
    /// could not be dropped. Repeating revoke retries only the removal.
    #[error("connector route revoked; credential removal pending")]
    MaterialPending,
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

/// Register one route for an account the admin provisioned. `label` is a
/// display hint only; the id is always newly minted (a relink never reuses
/// one). `kind` must agree with `binding`. No credential store is consulted.
pub fn provision(home: &Path, provider: &str, kind: ConnectorKind, binding: &EmployeeBinding, label: &str,
    actor: &AdminActor) -> Result<AccountId, LifecycleError> {
    let result = ConnectorCatalog::open(home)
        .and_then(|catalog| catalog.register(label, provider, kind, binding).map(|account| (catalog, account)))
        .and_then(|(catalog, account)| {
            // Read back what selection will read.
            let route = catalog.route(&account)?.ok_or(CatalogError::Unavailable)?;
            if !route.active || route.provider != provider || route.kind != kind || &route.binding != binding {
                return Err(CatalogError::Unavailable);
            }
            Ok(route)
        });
    match result {
        Ok(route) => {
            audit(home, "isolation.connector_provisioned", actor, route_details(&route, "provisioned"));
            Ok(route.account)
        }
        Err(error) => {
            audit(home, "isolation.connector_provision_refused", actor,
                json!({"kind": kind.as_str(), "binding_type": binding_type(binding), "status": refusal(error)}));
            Err(error.into())
        }
    }
}

/// Revoke a route (final): fence through the catalog, then drop material.
pub fn revoke(home: &Path, account: &AccountId, actor: &AdminActor) -> Result<Route, LifecycleError> {
    revoke_with(home, account, actor, MATERIAL.get().map(|material| material.as_ref()))
}

pub(crate) fn revoke_with(home: &Path, account: &AccountId, actor: &AdminActor,
    material: Option<&dyn CredentialMaterial>) -> Result<Route, LifecycleError> {
    let fenced = ConnectorCatalog::open(home).and_then(|catalog| {
        catalog.revoke(account)?;
        // The fence must be durable and visible before any material goes away.
        let route = catalog.route(account)?.ok_or(CatalogError::Unavailable)?;
        if route.active || &route.account != account { return Err(CatalogError::Unavailable); }
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
