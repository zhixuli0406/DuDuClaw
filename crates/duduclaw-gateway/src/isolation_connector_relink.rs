//! Scoped route transitions for product credential events (v1.81 C4).
//!
//! Child module of `isolation_connectors`: it uses the catalog's private
//! connection and route decoder, so every transition here is one catalog
//! transaction checked by the same triggers as C1/C3.
//!
//! A *relink hint* remembers that a route was revoked because its product
//! credential was disconnected (or its consent revoked) by the account's own
//! owner, together with a domain-separated digest of the provider account
//! subject. Reconnecting the SAME account consumes the hint by registering a
//! NEW route (new opaque id, generation above every existing one) with the
//! predecessor's provider, kind, binding and display label. The revoked id is
//! never reactivated. Hints are written only in the transaction that revokes
//! the route, consumed at most once, cancelled by any admin revoke of that id
//! (`ConnectorCatalog::revoke`), by an account switch and by offboarding, and
//! are never deleted.
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use duduclaw_isolation::capability::EmployeeBinding;
use super::{AccountId, CatalogError, ConnectorCatalog, ConnectorKind, Route, ROUTE_COLUMNS, binding_id,
    decode_route, insert_route, now_seconds, raw_route};

pub(super) const RELINK_TABLE_NAME: &str = "isolation_connector_relink";
/// Only home/personal OAuth providers have product credential events.
pub(crate) const RELINK_PROVIDERS: [&str; 3] = ["google", "notion", "github"];
/// Upper bound on routes one event may touch; more is treated as damage.
const MAX_SCOPE_ROUTES: usize = 1024;

const RELINK_TABLE: &str = "CREATE TABLE isolation_connector_relink (
    account TEXT PRIMARY KEY NOT NULL,
    subject TEXT NOT NULL,
    state TEXT NOT NULL,
    successor TEXT,
    created_at INTEGER NOT NULL,
    CHECK(length(account)=32 AND account NOT GLOB '*[^0-9a-f]*'),
    CHECK(length(subject)=64 AND subject NOT GLOB '*[^0-9a-f]*'),
    CHECK((state IN ('pending','cancelled') AND successor IS NULL)
        OR (state='relinked' AND length(successor)=32 AND successor NOT GLOB '*[^0-9a-f]*')),
    CHECK(created_at > 0)
) STRICT";
// A hint starts pending, only for a route that is already revoked, of a
// provider/kind that has product credential events.
const RELINK_INSERT_TRIGGER: &str = "CREATE TRIGGER isolation_connector_relink_insert
    BEFORE INSERT ON isolation_connector_relink
    WHEN NEW.state IS NOT 'pending' OR NEW.successor IS NOT NULL
        OR NOT EXISTS(SELECT 1 FROM isolation_connector_routes WHERE account=NEW.account AND active=0
            AND provider IN ('google','notion','github') AND kind IN ('personal','shared_home'))
    BEGIN SELECT RAISE(ABORT,'immutable relink hint'); END";
// Pending → relinked/cancelled exactly once. A successor must be a different,
// active route with the predecessor's provider, kind and binding, and can
// succeed only one hint.
const RELINK_UPDATE_TRIGGER: &str = "CREATE TRIGGER isolation_connector_relink_update
    BEFORE UPDATE ON isolation_connector_relink
    WHEN OLD.state IS NOT 'pending'
        OR NEW.account IS NOT OLD.account OR NEW.subject IS NOT OLD.subject
        OR NEW.created_at IS NOT OLD.created_at
        OR NEW.state NOT IN ('relinked','cancelled')
        OR (NEW.state='relinked' AND (
            EXISTS(SELECT 1 FROM isolation_connector_relink WHERE successor=NEW.successor)
            OR NOT EXISTS(SELECT 1 FROM isolation_connector_routes s
                JOIN isolation_connector_routes p ON p.account=OLD.account
                WHERE s.account=NEW.successor AND s.active=1 AND s.account<>p.account
                    AND s.provider=p.provider AND s.kind=p.kind AND s.binding_id=p.binding_id)))
    BEGIN SELECT RAISE(ABORT,'immutable relink hint'); END";
const RELINK_DELETE_TRIGGER: &str = "CREATE TRIGGER isolation_connector_relink_no_delete
    BEFORE DELETE ON isolation_connector_relink
    BEGIN SELECT RAISE(ABORT,'immutable relink hint'); END";
pub(super) const RELINK_SCHEMA: [(&str, &str, &str); 4] = [
    ("table", "isolation_connector_relink", RELINK_TABLE),
    ("trigger", "isolation_connector_relink_insert", RELINK_INSERT_TRIGGER),
    ("trigger", "isolation_connector_relink_update", RELINK_UPDATE_TRIGGER),
    ("trigger", "isolation_connector_relink_no_delete", RELINK_DELETE_TRIGGER),
];

/// Domain-separated SHA-256 of a provider account subject. The raw subject
/// (Google `sub`, email, workspace id) is never stored in the catalog.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct RelinkSubject(String);
impl RelinkSubject {
    /// `identity` is the provider's stable account subject (Google `sub`).
    pub(crate) fn of(provider: &str, identity: &str) -> Result<Self, CatalogError> {
        if !RELINK_PROVIDERS.contains(&provider) || identity.is_empty() || identity.len() > 1024
            || identity.chars().any(char::is_control) {
            return Err(CatalogError::Invalid);
        }
        let mut digest = Sha256::new();
        for part in [b"duduclaw-connector-relink-v1".as_slice(), provider.as_bytes(), identity.as_bytes()] {
            digest.update((part.len() as u64).to_be_bytes());
            digest.update(part);
        }
        Ok(Self(hex::encode(digest.finalize())))
    }
    /// The home-wide installation of `provider` (Notion/GitHub/Google Home):
    /// the product holds one home account per provider and no stable account
    /// subject for all of them, so the logical account is "the home install".
    pub(crate) fn home(provider: &str) -> Result<Self, CatalogError> { Self::of(provider, "home-installation") }
}
impl std::fmt::Debug for RelinkSubject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("RelinkSubject(..)") }
}

/// Which active routes a product credential event concerns.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RouteScope<'a> {
    pub(crate) kind: ConnectorKind,
    /// `None` = every binding of `kind` (home installs are tenant-wide).
    pub(crate) binding: Option<&'a EmployeeBinding>,
    /// `None` = every provider (offboarding).
    pub(crate) provider: Option<&'static str>,
}
impl RouteScope<'_> {
    fn params(&self) -> Result<(String, Option<String>, Option<String>), CatalogError> {
        if let Some(binding) = self.binding {
            if !self.kind.admits(binding) { return Err(CatalogError::Invalid); }
        }
        if self.provider.is_some_and(|provider| super::known_provider(provider).is_none()) {
            return Err(CatalogError::Invalid);
        }
        Ok((self.kind.as_str().to_owned(), self.binding.map(binding_id).transpose()?,
            self.provider.map(str::to_owned)))
    }
}
const SCOPE_FILTER: &str = "kind=?1 AND (?2 IS NULL OR binding_id=?2) AND (?3 IS NULL OR provider=?3)";
const PREDECESSOR_FILTER: &str = "p.kind=?1 AND (?2 IS NULL OR p.binding_id=?2) AND (?3 IS NULL OR p.provider=?3)";

fn active_in(tx: &Transaction<'_>, scope: &RouteScope<'_>) -> Result<Vec<Route>, CatalogError> {
    let unavailable = |_| CatalogError::Unavailable;
    let (kind, binding, provider) = scope.params()?;
    let mut statement = tx.prepare(&format!(
        "SELECT {ROUTE_COLUMNS} FROM isolation_connector_routes WHERE active=1 AND {SCOPE_FILTER}
         ORDER BY account LIMIT {}", MAX_SCOPE_ROUTES + 1)).map_err(unavailable)?;
    let raw = statement.query_map(params![kind, binding, provider], raw_route).map_err(unavailable)?
        .collect::<Result<Vec<_>, _>>().map_err(unavailable)?;
    if raw.len() > MAX_SCOPE_ROUTES { return Err(CatalogError::Unavailable); }
    let routes = raw.into_iter().map(decode_route).collect::<Result<Vec<_>, _>>()?;
    if routes.iter().any(|route| !route.active || route.kind != scope.kind
        || scope.binding.is_some_and(|binding| &route.binding != binding)
        || scope.provider.is_some_and(|provider| route.provider != provider)) {
        return Err(CatalogError::Unavailable);
    }
    Ok(routes)
}

/// Pending hints in scope: (predecessor route, subject digest).
fn pending_in(tx: &Transaction<'_>, scope: &RouteScope<'_>) -> Result<Vec<(Route, String)>, CatalogError> {
    let unavailable = |_| CatalogError::Unavailable;
    let (kind, binding, provider) = scope.params()?;
    let columns = ROUTE_COLUMNS.split(',').map(|column| format!("p.{column}")).collect::<Vec<_>>().join(",");
    let mut statement = tx.prepare(&format!(
        "SELECT {columns}, r.subject FROM isolation_connector_relink r
         JOIN isolation_connector_routes p ON p.account=r.account
         WHERE r.state='pending' AND {PREDECESSOR_FILTER} ORDER BY p.account LIMIT {}", MAX_SCOPE_ROUTES + 1))
        .map_err(unavailable)?;
    let rows = statement.query_map(params![kind, binding, provider], |row| Ok((raw_route(row)?, row.get::<_, String>(7)?)))
        .map_err(unavailable)?.collect::<Result<Vec<_>, _>>().map_err(unavailable)?;
    if rows.len() > MAX_SCOPE_ROUTES { return Err(CatalogError::Unavailable); }
    rows.into_iter().map(|(raw, subject)| {
        let route = decode_route(raw)?;
        // A hint always names a revoked route (insert trigger; revocation is final).
        if route.active { return Err(CatalogError::Unavailable); }
        Ok((route, subject))
    }).collect()
}

fn to_generation(generation: i64) -> Result<u64, CatalogError> {
    u64::try_from(generation).ok().filter(|generation| *generation > 0).ok_or(CatalogError::Unavailable)
}

fn next_generation(tx: &Transaction<'_>) -> Result<i64, CatalogError> {
    let max: i64 = tx.query_row("SELECT coalesce(max(generation),0) FROM isolation_connector_routes", [], |row| row.get(0))
        .map_err(|_| CatalogError::Unavailable)?;
    max.checked_add(1).ok_or(CatalogError::Unavailable)
}

impl ConnectorCatalog {
    fn scoped<T>(&self, f: impl FnOnce(&Transaction<'_>, i64) -> Result<T, CatalogError>) -> Result<T, CatalogError> {
        let now = now_seconds()?;
        let mut connection = self.lock()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| CatalogError::Unavailable)?;
        let out = f(&tx, now)?;
        tx.commit().map_err(|_| CatalogError::Unavailable)?;
        Ok(out)
    }

    /// Every active route in scope, from one read.
    pub(crate) fn active_in_scope(&self, scope: RouteScope<'_>) -> Result<Vec<Route>, CatalogError> {
        let mut connection = self.lock()?;
        let tx = connection.transaction().map_err(|_| CatalogError::Unavailable)?;
        let routes = active_in(&tx, &scope)?;
        tx.finish().map_err(|_| CatalogError::Unavailable)?;
        Ok(routes)
    }

    /// Number of pending relink hints in scope.
    pub(crate) fn pending_relinks(&self, scope: RouteScope<'_>) -> Result<usize, CatalogError> {
        let mut connection = self.lock()?;
        let tx = connection.transaction().map_err(|_| CatalogError::Unavailable)?;
        let count = pending_in(&tx, &scope)?.len();
        tx.finish().map_err(|_| CatalogError::Unavailable)?;
        Ok(count)
    }

    /// Revoke (final) every active route in scope in ONE transaction, each
    /// with a generation above every existing one, deleting their details.
    /// With `relink`, a pending hint is recorded for each route THIS call
    /// revoked (never for a route that was already revoked, e.g. by an
    /// admin). Returns the post-revoke rows of the routes it revoked; an
    /// empty scope succeeds without change.
    pub(crate) fn revoke_scope(&self, scope: RouteScope<'_>, relink: Option<&RelinkSubject>) -> Result<Vec<Route>, CatalogError> {
        let unavailable = |_| CatalogError::Unavailable;
        self.scoped(|tx, now| {
            let routes = active_in(tx, &scope)?;
            let mut revoked = Vec::with_capacity(routes.len());
            for mut route in routes {
                let generation = next_generation(tx)?;
                let changed = tx.execute("UPDATE isolation_connector_routes SET active=0, revoked_at=?2, generation=?3
                    WHERE account=?1 AND active=1", params![route.account.as_str(), now, generation]).map_err(unavailable)?;
                if changed != 1 { return Err(CatalogError::Unavailable); }
                tx.execute("DELETE FROM isolation_connector_details WHERE account=?1", [route.account.as_str()])
                    .map_err(unavailable)?;
                if let Some(subject) = relink.filter(|_| RELINK_PROVIDERS.contains(&route.provider)
                    && matches!(route.kind, ConnectorKind::Personal | ConnectorKind::SharedHome)) {
                    tx.execute("INSERT INTO isolation_connector_relink (account,subject,state,successor,created_at)
                        VALUES (?1,?2,'pending',NULL,?3)", params![route.account.as_str(), subject.0, now])
                        .map_err(unavailable)?;
                }
                route.active = false;
                route.generation = to_generation(generation)?;
                revoked.push(route);
            }
            Ok(revoked)
        })
    }

    /// Move the generation of every active route in scope (one transaction):
    /// selections pinned before this call refuse at their next checkpoint.
    pub(crate) fn advance_scope(&self, scope: RouteScope<'_>) -> Result<Vec<Route>, CatalogError> {
        let unavailable = |_| CatalogError::Unavailable;
        self.scoped(|tx, _| {
            let routes = active_in(tx, &scope)?;
            let mut advanced = Vec::with_capacity(routes.len());
            for mut route in routes {
                let generation = next_generation(tx)?;
                let changed = tx.execute("UPDATE isolation_connector_routes SET generation=?2 WHERE account=?1 AND active=1",
                    params![route.account.as_str(), generation]).map_err(unavailable)?;
                if changed != 1 { return Err(CatalogError::Unavailable); }
                tx.execute("DELETE FROM isolation_connector_details WHERE account=?1", [route.account.as_str()])
                    .map_err(unavailable)?;
                route.generation = to_generation(generation)?;
                advanced.push(route);
            }
            Ok(advanced)
        })
    }

    /// Reconnect of the account named by `subject`: every pending hint in
    /// scope with exactly that subject becomes a NEW route (fresh id and
    /// generation; predecessor's provider/kind/binding/label) and is marked
    /// relinked to it; pending hints in scope for any OTHER subject are
    /// cancelled (they belong to another account). One transaction.
    pub(crate) fn relink_scope(&self, scope: RouteScope<'_>, subject: &RelinkSubject) -> Result<Vec<Route>, CatalogError> {
        let unavailable = |_| CatalogError::Unavailable;
        self.scoped(|tx, now| {
            let mut created = Vec::new();
            for (predecessor, stored) in pending_in(tx, &scope)? {
                if stored != subject.0 {
                    cancel(tx, &predecessor.account)?;
                    continue;
                }
                let binding = binding_id(&predecessor.binding)?;
                let (account, generation) = insert_route(tx, &predecessor.label, predecessor.provider,
                    predecessor.kind, &binding, now)?;
                let changed = tx.execute("UPDATE isolation_connector_relink SET state='relinked', successor=?2
                    WHERE account=?1 AND state='pending'", params![predecessor.account.as_str(), account.as_str()])
                    .map_err(unavailable)?;
                if changed != 1 { return Err(CatalogError::Unavailable); }
                created.push(Route { account, generation: to_generation(generation)?, active: true, ..predecessor });
            }
            Ok(created)
        })
    }

    /// Cancel every pending hint in scope (account switch, offboarding).
    pub(crate) fn cancel_relink_scope(&self, scope: RouteScope<'_>) -> Result<usize, CatalogError> {
        self.scoped(|tx, _| {
            let pending = pending_in(tx, &scope)?;
            for (predecessor, _) in &pending { cancel(tx, &predecessor.account)?; }
            Ok(pending.len())
        })
    }
}

/// Admin revoke of an id (also of an already revoked one) cancels its hint,
/// so an owner's later reconnect can never undo an administrator's decision.
pub(super) fn cancel(tx: &Transaction<'_>, account: &AccountId) -> Result<(), CatalogError> {
    let state: Option<String> = tx.query_row("SELECT state FROM isolation_connector_relink WHERE account=?1",
        [account.as_str()], |row| row.get(0)).optional().map_err(|_| CatalogError::Unavailable)?;
    if state.as_deref() == Some("pending") {
        tx.execute("UPDATE isolation_connector_relink SET state='cancelled' WHERE account=?1 AND state='pending'",
            [account.as_str()]).map_err(|_| CatalogError::Unavailable)?;
    }
    Ok(())
}
