//! Product credential lifecycle events → connector catalog (v1.81 C4).
//!
//! Product credential changes happen outside the isolation admin RPCs (a
//! person connects or disconnects their Google account, Google reports the
//! grant revoked, an Admin offboards a person, an Admin connects or removes
//! the home Notion/GitHub/Google OAuth). These functions keep the catalog
//! routes that protected mode selects from in step with those changes.
//!
//! | Event                                        | Catalog effect                                             |
//! |----------------------------------------------|------------------------------------------------------------|
//! | Personal Google disconnect (by the owner/admin) | revoke the owner's Personal google routes + relink hint (Google `sub`) |
//! | Personal Google consent revoked (`invalid_grant`) | same as disconnect                                     |
//! | Personal Google connect, same `sub` / first connect | advance active routes, publish, advance again, relink pending hints of that `sub` (NEW ids), cancel hints of other subs |
//! | Personal Google account switch (other `sub`) | revoke routes + cancel hints, publish; new account = `connector_setup_required` until an Admin provisions |
//! | Owner suspended / offboarded                 | revoke EVERY Personal route of the owner (all providers), cancel hints |
//! | Home OAuth disconnect (google/notion/github) | revoke that provider's SharedHome routes + relink hint ("home install") |
//! | Home OAuth connect (google/notion/github)    | advance, publish, advance, relink pending home hints        |
//!
//! Ordering: the catalog fence commits (and is read back) BEFORE product
//! credential material is dropped or replaced, so a pinned selection refuses
//! at its next checkpoint before the material changes. A fence failure
//! refuses the event (material untouched). After a drop, the fence is run
//! again so a reconnect that raced the disconnect cannot leave a relinked
//! route active without material.
//!
//! Crash safety: every catalog step is one SQLite transaction. An event
//! interrupted after its fence leaves the routes revoked (never active);
//! repeating the event is a no-op on the catalog and retries the drop.
//! Revocation is final: a reconnect never reactivates an old id.
//!
//! A home without `isolation-connectors.db` has no routes (protected mode
//! was never provisioned); events then change nothing in the catalog. The
//! file is never created here.
use std::path::Path;
use duduclaw_isolation::{bounded::Text, capability::EmployeeBinding};
use serde_json::{Value, json};
use crate::isolation_connectors::{CATALOG_FILE, CatalogError, ConnectorCatalog, ConnectorKind, RELINK_PROVIDERS,
    RelinkSubject, Route, RouteScope};

/// Bounded retries when a concurrent admin provision lands inside a fence.
const FENCE_ATTEMPTS: usize = 3;

/// Which routes an event concerns.
#[derive(Debug, Clone)]
pub(crate) struct Target {
    kind: ConnectorKind,
    binding: Option<EmployeeBinding>,
    provider: Option<&'static str>,
}
impl Target {
    /// One owner's Personal routes of one provider. `None` when `owner`
    /// cannot be a catalog binding (such an owner has no routes).
    pub(crate) fn personal(owner: &str, provider: &str) -> Option<Self> {
        let provider = RELINK_PROVIDERS.iter().copied().find(|known| *known == provider)?;
        Some(Self { kind: ConnectorKind::Personal, binding: Some(personal_binding(owner)?), provider: Some(provider) })
    }
    /// Every Personal route of one owner (offboarding / suspension).
    pub(crate) fn personal_all(owner: &str) -> Option<Self> {
        Some(Self { kind: ConnectorKind::Personal, binding: Some(personal_binding(owner)?), provider: None })
    }
    /// The SharedHome routes of a home-wide OAuth provider, any tenant.
    /// `None` for providers without catalog routes (Slack, custom).
    pub(crate) fn home(provider: &str) -> Option<Self> {
        let provider = RELINK_PROVIDERS.iter().copied().find(|known| *known == provider)?;
        Some(Self { kind: ConnectorKind::SharedHome, binding: None, provider: Some(provider) })
    }
    pub(crate) fn scope(&self) -> RouteScope<'_> {
        RouteScope { kind: self.kind, binding: self.binding.as_ref(), provider: self.provider }
    }
}
fn personal_binding(owner: &str) -> Option<EmployeeBinding> {
    let binding = EmployeeBinding::Personal(Text::new(owner.to_owned()).ok()?);
    crate::isolation_connectors::binding_id(&binding).ok()?;
    Some(binding)
}

/// How a (re)connect relates to the material it replaces.
#[derive(Debug, Clone)]
pub(crate) enum Reconnect {
    /// Same provider account (or a first connect): routes continue under a
    /// new generation; routes revoked by this account's earlier disconnect
    /// are recreated under NEW ids.
    Same(RelinkSubject),
    /// A different provider account: old routes are revoked (final) and the
    /// new account needs an Admin provision.
    Switch,
}

/// What a fence does with relink hints.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Hint<'a> {
    /// The account's own disconnect / consent revoke: record a hint for each
    /// route this fence revokes, so reconnecting THIS account (subject)
    /// recreates it under a new id.
    Record(&'a RelinkSubject),
    /// Record nothing and keep existing hints (a repeated disconnect with no
    /// connection left).
    Leave,
    /// Administrator / account-switch / offboarding intent: record nothing
    /// and cancel every pending hint in scope.
    Cancel,
}
impl<'a> Hint<'a> {
    fn subject(self) -> Option<&'a RelinkSubject> {
        match self { Self::Record(subject) => Some(subject), Self::Leave | Self::Cancel => None }
    }
    fn status(self) -> &'static str {
        match self { Self::Record(_) => "fenced_relinkable", Self::Leave => "fenced", Self::Cancel => "fenced_final" }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EventFailure<E> {
    /// The catalog could not be fenced; product material was not touched.
    Catalog(CatalogError),
    /// The product operation itself failed (after a successful fence).
    Product(E),
}

/// The catalog of `home`, or `None` when the home has none. An existing but
/// unsafe/unreadable catalog is an error (never "no routes").
pub(crate) fn open(home: &Path) -> Result<Option<ConnectorCatalog>, CatalogError> {
    match std::fs::symlink_metadata(home.join(CATALOG_FILE)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(CatalogError::Unavailable),
        Ok(_) => ConnectorCatalog::open(home).map(Some),
    }
}

/// Revoke every active route of `target` (step 1 of a disconnect, consent
/// revoke or offboarding) and read back that none is active. `hint` decides
/// whether a later reconnect of the same account may recreate them under new
/// ids.
pub(crate) fn fence(home: &Path, target: &Target, hint: Hint<'_>, reason: &'static str)
    -> Result<Vec<Route>, CatalogError> {
    match open(home) {
        Ok(Some(catalog)) => fence_with(home, &catalog, target, hint, reason),
        Ok(None) => Ok(Vec::new()),
        Err(error) => {
            audit(home, "isolation.connector_event_refused", reason, target, &[], refusal(error));
            Err(error)
        }
    }
}

pub(crate) fn fence_with(home: &Path, catalog: &ConnectorCatalog, target: &Target, hint: Hint<'_>,
    reason: &'static str) -> Result<Vec<Route>, CatalogError> {
    let scope = target.scope();
    let attempt = || -> Result<(Vec<Route>, bool), CatalogError> {
        let mut revoked = Vec::new();
        for _ in 0..FENCE_ATTEMPTS {
            revoked.extend(catalog.revoke_scope(scope, hint.subject())?);
            if matches!(hint, Hint::Cancel) { catalog.cancel_relink_scope(scope)?; }
            // The fence must be durable and visible before material changes.
            if catalog.active_in_scope(scope)?.is_empty() { return Ok((revoked, true)); }
        }
        Ok((revoked, false))
    };
    let revoked = match attempt() {
        Ok((revoked, true)) => revoked,
        Ok((revoked, false)) => {
            audit(home, "isolation.connector_event_refused", reason, target, &revoked, "unsettled");
            return Err(CatalogError::Unavailable);
        }
        Err(error) => {
            audit(home, "isolation.connector_event_refused", reason, target, &[], refusal(error));
            return Err(error);
        }
    };
    if !revoked.is_empty() {
        audit(home, "isolation.connector_event_revoked", reason, target, &revoked, hint.status());
    }
    drop_in_process(home, target, &revoked, reason);
    Ok(revoked)
}

/// Fence → drop product material → fence again. A fence failure refuses
/// the event without calling `drop_material`.
pub(crate) fn revoke_then<T, E>(home: &Path, target: &Target, hint: Hint<'_>, reason: &'static str,
    drop_material: impl FnOnce() -> Result<T, E>) -> Result<T, EventFailure<E>> {
    fence(home, target, hint, reason).map_err(EventFailure::Catalog)?;
    let out = drop_material().map_err(EventFailure::Product)?;
    // A reconnect racing this event may have relinked/advanced a route
    // between the fence and the drop; it now has no material.
    if fence(home, target, hint, reason).is_err() {
        audit(home, "isolation.connector_event_incomplete", reason, target, &[], "post_drop_fence_failed");
    }
    Ok(out)
}

/// A rotation in progress: the old generation is fenced, the new material
/// is not yet published.
#[must_use]
pub(crate) struct Rotation { target: Target, reconnect: Reconnect, reason: &'static str }

/// Step 1 of a (re)connect, before the new material is written.
pub(crate) fn begin_rotation(home: &Path, target: Target, reconnect: Reconnect, reason: &'static str)
    -> Result<Rotation, CatalogError> {
    match open(home) {
        Ok(Some(catalog)) => match &reconnect {
            Reconnect::Same(_) => {
                let advanced = catalog.advance_scope(target.scope()).inspect_err(|error|
                    audit(home, "isolation.connector_event_refused", reason, &target, &[], refusal(*error)))?;
                if !advanced.is_empty() {
                    audit(home, "isolation.connector_event_advanced", reason, &target, &advanced, "fenced");
                }
            }
            Reconnect::Switch => { fence_with(home, &catalog, &target, Hint::Cancel, reason)?; }
        },
        Ok(None) => {}
        Err(error) => {
            audit(home, "isolation.connector_event_refused", reason, &target, &[], refusal(error));
            return Err(error);
        }
    }
    Ok(Rotation { target, reconnect, reason })
}

impl Rotation {
    /// Step 2, after the new material is published: settle the generation
    /// (selections made during publication refuse) and recreate routes this
    /// account lost to its own earlier disconnect. Returns the active routes
    /// in scope afterwards.
    pub(crate) fn finish(self, home: &Path) -> Result<Vec<Route>, CatalogError> {
        let Rotation { target, reconnect, reason } = self;
        let result = (|| {
            let Some(catalog) = open(home)? else { return Ok(Vec::new()) };
            let scope = target.scope();
            match &reconnect {
                Reconnect::Same(subject) => {
                    let advanced = catalog.advance_scope(scope)?;
                    if !advanced.is_empty() {
                        audit(home, "isolation.connector_event_advanced", reason, &target, &advanced, "settled");
                    }
                    let created = catalog.relink_scope(scope, subject)?;
                    if !created.is_empty() {
                        audit(home, "isolation.connector_event_relinked", reason, &target, &created, "relinked");
                    }
                }
                Reconnect::Switch => { catalog.cancel_relink_scope(scope)?; }
            }
            catalog.active_in_scope(scope)
        })();
        if let Err(error) = result {
            audit(home, "isolation.connector_event_incomplete", reason, &target, &[], refusal(error));
        }
        result
    }
    /// The publication failed: nothing to recreate. The fence stays.
    pub(crate) fn abandon(self) {}
}

/// Fence → publish → settle. A fence failure refuses the event without
/// calling `publish`; a settle failure is audited (routes stay fenced or on
/// a generation that refuses older selections) and the publication stands.
pub(crate) fn rotate<T, E>(home: &Path, target: Target, reconnect: Reconnect, reason: &'static str,
    publish: impl FnOnce() -> Result<T, E>) -> Result<T, EventFailure<E>> {
    let rotation = begin_rotation(home, target, reconnect, reason).map_err(EventFailure::Catalog)?;
    match publish() {
        Ok(out) => {
            let _ = rotation.finish(home);
            Ok(out)
        }
        Err(error) => {
            rotation.abandon();
            Err(EventFailure::Product(error))
        }
    }
}

/// After the catalog fence: drop the per-account in-process material (C3
/// vault) of every revoked route. A failure leaves the fence in place.
fn drop_in_process(home: &Path, target: &Target, revoked: &[Route], reason: &'static str) {
    let Some(material) = crate::isolation_connector_lifecycle::installed_material() else { return };
    for route in revoked {
        if material.drop_material(&route.account).is_err() {
            audit(home, "isolation.connector_material_removal_failed", reason, target, std::slice::from_ref(route), "pending");
        }
    }
}

fn refusal(error: CatalogError) -> &'static str {
    match error {
        CatalogError::Unavailable => "unavailable",
        CatalogError::Invalid => "invalid",
        CatalogError::NotFound => "not_found",
        CatalogError::Revoked => "revoked",
    }
}

/// Opaque ids, provider, kind, generation and the Personal owner user id
/// only: never labels, tenant ids, provider subjects, emails or tokens.
fn audit(home: &Path, event: &str, reason: &'static str, target: &Target, routes: &[Route], status: &str) {
    let owner = match &target.binding {
        Some(EmployeeBinding::Personal(owner)) => Value::String(owner.as_str().to_owned()),
        _ => Value::Null,
    };
    let accounts = routes.iter().map(|route| json!({"account": route.account.as_str(), "provider": route.provider,
        "generation": route.generation, "active": route.active})).collect::<Vec<_>>();
    let severity = if event.ends_with("_refused") || event.ends_with("_incomplete") || event.ends_with("_failed") {
        duduclaw_security::audit::Severity::Warning
    } else { duduclaw_security::audit::Severity::Info };
    crate::security_autopilot::audit_and_emit(home, &duduclaw_security::audit::AuditEvent::new(
        event, "isolation-connectors", severity, json!({"reason": reason, "kind": target.kind.as_str(),
            "provider": target.provider, "owner_user_id": owner, "accounts": accounts, "status": status})));
}

#[cfg(test)]
#[path = "isolation_connector_events_tests.rs"]
mod tests;
/// v1.81 E3: real catalog-backed selections for refresh-path tests.
#[cfg(test)]
#[path = "isolation_connector_e3_test_support.rs"]
pub(crate) mod e3_test_support;
