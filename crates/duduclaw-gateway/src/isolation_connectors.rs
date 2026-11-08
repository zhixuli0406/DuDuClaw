//! Account selection for isolated MCP. No credential fallback or ambient account.
use std::{future::Future, path::Path, pin::Pin, sync::{Arc, Mutex}};
use duduclaw_isolation::{bounded::Text, capability::{EmployeeBinding, ResourceGrant}};
use duduclaw_mcp_boundary::{bindings::BindingAuthority, tool_policy::AuthorizedToolCall,
    RequestContext, VerificationError};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::Value;
#[cfg(test)]
use duduclaw_mcp_boundary::bindings::BindingStore;
use crate::isolation_connector_details::{AccountDetailRecord, DETAILS_SCHEMA, DETAILS_TABLE_NAME};
// v1.81 C4: scoped transitions + relink hints for product credential events.
#[path = "isolation_connector_relink.rs"]
mod relink;
pub(crate) use relink::{RELINK_PROVIDERS, RelinkSubject, RouteScope};
// v1.81 C5: provenance of routes created by an applied migration proposal.
#[path = "isolation_connector_origin.rs"]
mod origin;
pub(crate) use origin::{MigratedRoute, MigrationOrigin};

/// Production catalog file. It lives directly in the gateway home, beside the
/// other isolation stores (`isolation-forks.db`, `isolation-execution-jobs.db`,
/// `isolation_background.db`), and is opened only by [`ConnectorCatalog::open`].
pub const CATALOG_FILE: &str = "isolation-connectors.db";
/// Rows are never deleted, so relinking consumes capacity. Refuse, never evict.
const MAX_ROUTES: i64 = 100_000;
/// Upper bound on routes returned for one binding; more is treated as damage.
const MAX_ROUTES_PER_BINDING: usize = 1024;
/// Largest page [`ConnectorCatalog::list`] returns.
pub const MAX_LIST_PAGE: usize = 200;
const PROVIDERS: [&str; 9] = ["google", "notion", "github", "odoo", "database", "channel", "search", "model", "computer"];

/// Non-sensitive catalog failures. Every variant refuses the operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CatalogError {
    /// Store unreadable/unwritable, unsafe file, unexpected schema or damaged row.
    #[error("connector catalog unavailable")] Unavailable,
    /// Malformed account id, label, provider, kind or binding.
    #[error("connector catalog request invalid")] Invalid,
    #[error("connector account not found")] NotFound,
    /// The route has been revoked; revocation is final.
    #[error("connector account revoked")] Revoked,
}
impl From<CatalogError> for VerificationError {
    fn from(error: CatalogError) -> Self {
        match error {
            CatalogError::Unavailable => Self::Unavailable,
            CatalogError::Revoked => Self::Revoked,
            CatalogError::Invalid | CatalogError::NotFound => Self::Invalid,
        }
    }
}

/// Opaque connector account id: 128 bits from the OS RNG as 32 lowercase hex
/// characters. Minted only by [`ConnectorCatalog::register`]; never derived
/// from a label, provider account name or binding, and never reused because
/// catalog rows are never deleted.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AccountId(String);
impl AccountId {
    /// Parse an id received from a trusted caller (RPC, adapter). Format only;
    /// existence and state are decided by the catalog.
    pub fn parse(value: &str) -> Result<Self, CatalogError> {
        if value.len() == 32 && value.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            Ok(Self(value.to_owned()))
        } else { Err(CatalogError::Invalid) }
    }
    pub fn as_str(&self) -> &str { &self.0 }
    fn generate() -> Result<Self, CatalogError> {
        use rand::RngCore;
        let mut bytes = [0u8; 16];
        rand::rngs::OsRng.try_fill_bytes(&mut bytes).map_err(|_| CatalogError::Unavailable)?;
        Ok(Self(hex::encode(bytes)))
    }
}
impl std::fmt::Display for AccountId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.0) }
}

/// Who an account belongs to. Fixed at registration, never changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConnectorKind {
    /// One person's own account; binding `Personal(owner)`.
    Personal,
    /// A tenant-owned account usable by that tenant's Shared employees; binding `Shared(tenant)`.
    SharedTenant,
    /// The home-wide product account installed for the tenant; binding
    /// `Shared(tenant)`. The only kind an explicit product-home fallback may use.
    SharedHome,
}
impl ConnectorKind {
    pub fn as_str(self) -> &'static str {
        match self { Self::Personal => "personal", Self::SharedTenant => "shared_tenant", Self::SharedHome => "shared_home" }
    }
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "personal" => Self::Personal, "shared_tenant" => Self::SharedTenant, "shared_home" => Self::SharedHome,
            _ => return None,
        })
    }
    /// Personal kinds bind to a person, shared kinds to a tenant.
    pub fn admits(self, binding: &EmployeeBinding) -> bool {
        matches!((self, binding), (Self::Personal, EmployeeBinding::Personal(_))
            | (Self::SharedTenant | Self::SharedHome, EmployeeBinding::Shared(_)))
    }
}

/// Stable, persisted catalog key for an [`EmployeeBinding`]:
/// `Personal(owner)` ⇒ `"personal:" + owner`, `Shared(tenant)` ⇒ `"shared:" + tenant`.
/// The suffix is the exact bounded text (1..=128 bytes, no control characters,
/// not trimmed or case-folded). The fixed, distinct prefixes make the mapping
/// injective and reversible. Changing this format orphans every stored route,
/// which then refuses (fail closed); it must never be "migrated" by guessing.
pub fn binding_id(binding: &EmployeeBinding) -> Result<String, CatalogError> {
    let (prefix, value) = match binding {
        EmployeeBinding::Personal(owner) => ("personal:", owner.as_str()),
        EmployeeBinding::Shared(tenant) => ("shared:", tenant.as_str()),
    };
    if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
        return Err(CatalogError::Invalid);
    }
    Ok(format!("{prefix}{value}"))
}
fn parse_binding_id(stored: &str) -> Result<EmployeeBinding, CatalogError> {
    let text = |value: &str| Text::new(value.to_owned()).map_err(|_| CatalogError::Unavailable);
    let binding = if let Some(owner) = stored.strip_prefix("personal:") {
        EmployeeBinding::Personal(text(owner)?)
    } else if let Some(tenant) = stored.strip_prefix("shared:") {
        EmployeeBinding::Shared(text(tenant)?)
    } else { return Err(CatalogError::Unavailable) };
    if binding_id(&binding).map_err(|_| CatalogError::Unavailable)? != stored { return Err(CatalogError::Unavailable); }
    Ok(binding)
}

/// One catalog row as of the read. `generation` changes on every state change
/// (revoke, [`ConnectorCatalog::advance`]); selections pin it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub account: AccountId,
    /// Display label from registration. Never an identifier.
    pub label: String,
    pub provider: &'static str,
    pub kind: ConnectorKind,
    pub binding: EmployeeBinding,
    pub generation: u64,
    pub active: bool,
}

const ROUTES_TABLE: &str = "CREATE TABLE isolation_connector_routes (
    account TEXT PRIMARY KEY NOT NULL,
    label TEXT NOT NULL,
    provider TEXT NOT NULL,
    kind TEXT NOT NULL,
    binding_id TEXT NOT NULL,
    generation INTEGER NOT NULL,
    active INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    revoked_at INTEGER,
    CHECK(length(account)=32 AND account NOT GLOB '*[^0-9a-f]*'),
    CHECK(length(label) BETWEEN 1 AND 128),
    CHECK(provider IN ('google','notion','github','odoo','database','channel','search','model','computer')),
    CHECK((kind='personal' AND binding_id GLOB 'personal:?*')
        OR (kind IN ('shared_tenant','shared_home') AND binding_id GLOB 'shared:?*')),
    CHECK(length(binding_id) <= 160),
    CHECK(generation > 0),
    CHECK(active IN (0,1)),
    CHECK(created_at > 0),
    CHECK((active=1 AND revoked_at IS NULL) OR (active=0 AND revoked_at IS NOT NULL))
) STRICT";
const ROUTES_INDEX: &str = "CREATE INDEX isolation_connector_routes_by_binding
    ON isolation_connector_routes(binding_id, active)";
// Inserts start active with a generation above every existing one; an id is
// never inserted twice (also blocks INSERT OR REPLACE).
const INSERT_TRIGGER: &str = "CREATE TRIGGER isolation_connector_insert BEFORE INSERT ON isolation_connector_routes
    WHEN EXISTS(SELECT 1 FROM isolation_connector_routes WHERE account=NEW.account)
        OR NEW.active IS NOT 1 OR NEW.revoked_at IS NOT NULL
        OR NEW.generation <= (SELECT coalesce(max(generation),0) FROM isolation_connector_routes)
    BEGIN SELECT RAISE(ABORT,'immutable account route'); END";
// Identity columns never change; active only 1→0; a revoked row is final;
// generation always moves above the current global maximum.
const UPDATE_TRIGGER: &str = "CREATE TRIGGER isolation_connector_update BEFORE UPDATE ON isolation_connector_routes
    WHEN OLD.active IS NOT 1
        OR NEW.account IS NOT OLD.account OR NEW.label IS NOT OLD.label
        OR NEW.provider IS NOT OLD.provider OR NEW.kind IS NOT OLD.kind
        OR NEW.binding_id IS NOT OLD.binding_id OR NEW.created_at IS NOT OLD.created_at
        OR NEW.active > OLD.active
        OR NEW.generation <= (SELECT max(generation) FROM isolation_connector_routes)
    BEGIN SELECT RAISE(ABORT,'immutable account route'); END";
const DELETE_TRIGGER: &str = "CREATE TRIGGER isolation_connector_no_delete BEFORE DELETE ON isolation_connector_routes
    BEGIN SELECT RAISE(ABORT,'immutable account route'); END";
const SCHEMA: [(&str, &str, &str); 5] = [
    ("table", "isolation_connector_routes", ROUTES_TABLE),
    ("index", "isolation_connector_routes_by_binding", ROUTES_INDEX),
    ("trigger", "isolation_connector_insert", INSERT_TRIGGER),
    ("trigger", "isolation_connector_update", UPDATE_TRIGGER),
    ("trigger", "isolation_connector_no_delete", DELETE_TRIGGER),
];
const ROUTE_COLUMNS: &str = "account,label,provider,kind,binding_id,generation,active";

/// Gateway-owned account routing metadata and the single production
/// connector-account store. Registration happens only after the connector's
/// actual account has been provisioned; revocation is final. This holds no
/// secret and does not import the gateway's legacy home account.
pub struct ConnectorCatalog { connection: Mutex<Connection> }
impl ConnectorCatalog {
    /// Open `<home>/isolation-connectors.db` through the private-database
    /// opener (no-follow ancestor walk, owner-only parent, regular single-link
    /// file owned by the effective uid) and additionally require the database
    /// and any SQLite sidecar to be mode 0600 (no group/other bits), both
    /// before and after SQLite opens it. Anything else is `Unavailable`.
    /// Non-Unix platforms always refuse.
    pub fn open(home: &Path) -> Result<Self, CatalogError> {
        #[cfg(unix)]
        {
            verify_private_files(home)?;
            let connection = crate::fs_safe::sqlite::open(home, CATALOG_FILE).map_err(|_| CatalogError::Unavailable)?;
            verify_private_files(home)?;
            let catalog = Self::new(connection)?;
            // WAL/SHM now exist; SQLite gives them the database's mode.
            verify_private_files(home)?;
            Ok(catalog)
        }
        #[cfg(not(unix))]
        { let _ = home; Err(CatalogError::Unavailable) }
    }

    /// Use an already-opened connection (tests, in-memory). Creates the schema
    /// in an empty database; an existing schema must match exactly.
    pub fn new(mut connection: Connection) -> Result<Self, CatalogError> {
        let unavailable = |_| CatalogError::Unavailable;
        connection.busy_timeout(std::time::Duration::from_secs(2)).map_err(unavailable)?;
        connection.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")
            .map_err(unavailable)?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
        let present = |table: &str| -> Result<bool, CatalogError> {
            tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE tbl_name=?1)", [table], |row| row.get(0))
                .map_err(unavailable)
        };
        if !present("isolation_connector_routes")? {
            for (_, _, sql) in SCHEMA { tx.execute_batch(sql).map_err(unavailable)?; }
        }
        // v1.81 C6: a catalog created before the details table gains it; an
        // existing details table must match exactly like the routes table.
        if !present(DETAILS_TABLE_NAME)? {
            for (_, _, sql) in DETAILS_SCHEMA { tx.execute_batch(sql).map_err(unavailable)?; }
        }
        // v1.81 C4: likewise for the relink-hint table.
        if !present(relink::RELINK_TABLE_NAME)? {
            for (_, _, sql) in relink::RELINK_SCHEMA { tx.execute_batch(sql).map_err(unavailable)?; }
        }
        // v1.81 C5: likewise for the migration provenance table.
        if !present(origin::ORIGIN_TABLE_NAME)? {
            for (_, _, sql) in origin::ORIGIN_SCHEMA { tx.execute_batch(sql).map_err(unavailable)?; }
        }
        verify_schema(&tx)?;
        tx.commit().map_err(unavailable)?;
        Ok(Self { connection: Mutex::new(connection) })
    }

    /// Mint a new opaque account id for a provisioned account. The hint is
    /// stored only as a display label; registering the same label again (a
    /// relink) always yields a different id. `kind` must agree with `binding`.
    pub fn register(&self, account_label_hint: &str, provider: &str, kind: ConnectorKind, binding: &EmployeeBinding)
        -> Result<AccountId, CatalogError> {
        self.insert(account_label_hint, provider, kind, binding, None, None)
    }

    /// [`register`](Self::register) plus the account's provider details
    /// (C6), in the same transaction: either both exist, pinned to the new
    /// route's generation and binding, or neither does. The record holds
    /// references only, never a secret value.
    pub fn register_with_details(&self, account_label_hint: &str, provider: &str, kind: ConnectorKind,
        binding: &EmployeeBinding, details: &AccountDetailRecord) -> Result<AccountId, CatalogError> {
        self.insert(account_label_hint, provider, kind, binding, None, Some(details))
    }

    /// Test fixture: register with a caller-chosen id in the production id
    /// format, so fixtures can share an id with the binding test double.
    #[cfg(test)]
    pub(crate) fn register_fixture(&self, account: &str, provider: &str, kind: ConnectorKind, binding: &EmployeeBinding)
        -> Result<AccountId, CatalogError> {
        let account = AccountId::parse(account)?;
        self.insert(account.as_str(), provider, kind, binding, Some(account.clone()), None)
    }

    fn insert(&self, label: &str, provider: &str, kind: ConnectorKind, binding: &EmployeeBinding, fixed: Option<AccountId>,
        details: Option<&AccountDetailRecord>) -> Result<AccountId, CatalogError> {
        let unavailable = |_| CatalogError::Unavailable;
        let provider = known_provider(provider).ok_or(CatalogError::Invalid)?;
        if label.trim().is_empty() || label.len() > 128 || label.chars().any(char::is_control) || !kind.admits(binding) {
            return Err(CatalogError::Invalid);
        }
        let binding_id = binding_id(binding)?;
        let now = now_seconds()?;
        let mut connection = self.lock()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
        let (account, generation) = match fixed {
            Some(account) => {
                let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM isolation_connector_routes WHERE account=?1)",
                    [account.as_str()], |row| row.get(0)).map_err(unavailable)?;
                if exists { return Err(CatalogError::Invalid); }
                insert_route_as(&tx, &account, label, provider, kind, &binding_id, now).map(|generation| (account, generation))?
            }
            None => insert_route(&tx, label, provider, kind, &binding_id, now)?,
        };
        if let Some(details) = details {
            crate::isolation_connector_details::insert_row(&tx, &account, generation, &binding_id, provider, details, now)?;
        }
        tx.commit().map_err(unavailable)?;
        Ok(account)
    }

    /// Deactivate a route (final) and move its generation, so every pinned
    /// selection refuses at its next checkpoint. Revoking an already revoked
    /// route succeeds without changing the route (it only cancels a pending
    /// C4 relink hint, so the owner's reconnect cannot recreate it); an
    /// unknown id is `NotFound`.
    pub fn revoke(&self, account: &AccountId) -> Result<(), CatalogError> {
        self.transition(account, true).map(|_| ())
    }

    /// Move an active route's generation without deactivating it (credential
    /// re-consent, owner change, rotation): selections made before this call
    /// refuse; new selections pin the returned generation. Revoked ⇒ `Revoked`.
    pub fn advance(&self, account: &AccountId) -> Result<u64, CatalogError> {
        self.transition(account, false)
    }

    fn transition(&self, account: &AccountId, revoke: bool) -> Result<u64, CatalogError> {
        let unavailable = |_| CatalogError::Unavailable;
        let now = now_seconds()?;
        let mut connection = self.lock()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
        let state: Option<(i64, i64)> = tx.query_row(
            "SELECT active, generation FROM isolation_connector_routes WHERE account=?1",
            [account.as_str()], |row| Ok((row.get(0)?, row.get(1)?))).optional().map_err(unavailable)?;
        match state {
            None => return Err(CatalogError::NotFound),
            Some((0, generation)) if revoke => {
                // C4: an explicit revoke of an already revoked id still
                // cancels its pending relink hint (admin intent wins).
                relink::cancel(&tx, account)?;
                tx.commit().map_err(unavailable)?;
                return u64::try_from(generation).map_err(|_| CatalogError::Unavailable);
            }
            Some((0, _)) => return Err(CatalogError::Revoked),
            Some((1, _)) => {}
            Some(_) => return Err(CatalogError::Unavailable),
        }
        let next: i64 = tx.query_row("SELECT max(generation) FROM isolation_connector_routes", [], |row| row.get(0))
            .map_err(unavailable)?;
        let next = next.checked_add(1).ok_or(CatalogError::Unavailable)?;
        let changed = if revoke {
            tx.execute("UPDATE isolation_connector_routes SET active=0, revoked_at=?2, generation=?3
                WHERE account=?1 AND active=1", params![account.as_str(), now, next])
        } else {
            tx.execute("UPDATE isolation_connector_routes SET generation=?2 WHERE account=?1 AND active=1",
                params![account.as_str(), next])
        }.map_err(unavailable)?;
        if changed != 1 { return Err(CatalogError::Unavailable); }
        // C6: details are pinned to the generation that just ended; they go
        // with it in this same transaction (the delete trigger permits it
        // only now that the route is revoked or its generation moved).
        tx.execute("DELETE FROM isolation_connector_details WHERE account=?1", [account.as_str()])
            .map_err(unavailable)?;
        tx.commit().map_err(unavailable)?;
        u64::try_from(next).map_err(|_| CatalogError::Unavailable)
    }

    /// Current row for an id, active or revoked.
    pub fn route(&self, account: &AccountId) -> Result<Option<Route>, CatalogError> {
        let connection = self.lock()?;
        let raw = connection.query_row(
            &format!("SELECT {ROUTE_COLUMNS} FROM isolation_connector_routes WHERE account=?1"),
            [account.as_str()], raw_route).optional().map_err(|_| CatalogError::Unavailable)?;
        drop(connection);
        let route = raw.map(decode_route).transpose()?;
        if route.as_ref().is_some_and(|route| &route.account != account) { return Err(CatalogError::Unavailable); }
        Ok(route)
    }

    /// C6 reader: the current row for an id together with its provider
    /// details, both from ONE read snapshot. The details are `Some` only when
    /// the route is active and the stored row is pinned to exactly its
    /// current generation; details of an older generation never resolve.
    /// A row whose provider or binding disagrees with its route, or a row
    /// without a route, is damage (`Unavailable`).
    pub fn route_details(&self, account: &AccountId) -> Result<Option<(Route, Option<AccountDetailRecord>)>, CatalogError> {
        let unavailable = |_| CatalogError::Unavailable;
        let mut connection = self.lock()?;
        let tx = connection.transaction().map_err(unavailable)?;
        let raw = tx.query_row(
            &format!("SELECT {ROUTE_COLUMNS} FROM isolation_connector_routes WHERE account=?1"),
            [account.as_str()], raw_route).optional().map_err(unavailable)?;
        let stored = crate::isolation_connector_details::read_row(&tx, account)?;
        tx.finish().map_err(unavailable)?;
        drop(connection);
        let Some(raw) = raw else {
            return if stored.is_some() { Err(CatalogError::Unavailable) } else { Ok(None) };
        };
        let route = decode_route(raw)?;
        if &route.account != account { return Err(CatalogError::Unavailable); }
        let details = match stored {
            None => None,
            Some(stored) => {
                if stored.provider != route.provider || stored.binding_id != binding_id(&route.binding)? {
                    return Err(CatalogError::Unavailable);
                }
                (route.active && stored.generation == route.generation).then_some(stored.record)
            }
        };
        Ok(Some((route, details)))
    }

    /// Every active route whose binding is exactly `binding` (any kind),
    /// ordered by id. Callers must still filter by kind/provider.
    pub fn active_routes_for(&self, binding: &EmployeeBinding) -> Result<Vec<Route>, CatalogError> {
        let key = binding_id(binding)?;
        let connection = self.lock()?;
        let mut statement = connection.prepare(&format!(
            "SELECT {ROUTE_COLUMNS} FROM isolation_connector_routes WHERE binding_id=?1 AND active=1
             ORDER BY account LIMIT {}", MAX_ROUTES_PER_BINDING + 1)).map_err(|_| CatalogError::Unavailable)?;
        let raw = statement.query_map([key.as_str()], raw_route).map_err(|_| CatalogError::Unavailable)?
            .collect::<Result<Vec<_>, _>>().map_err(|_| CatalogError::Unavailable)?;
        drop(statement);
        drop(connection);
        if raw.len() > MAX_ROUTES_PER_BINDING { return Err(CatalogError::Unavailable); }
        let routes = raw.into_iter().map(decode_route).collect::<Result<Vec<_>, _>>()?;
        if routes.iter().any(|route| !route.active || &route.binding != binding) { return Err(CatalogError::Unavailable); }
        Ok(routes)
    }

    /// Admin listing: one page of routes, active and revoked, ordered by id and
    /// starting strictly after `after`. `limit` is clamped to 1..=[`MAX_LIST_PAGE`].
    /// Rows are re-validated like every other read; damage refuses the page.
    pub fn list(&self, after: Option<&AccountId>, limit: usize) -> Result<Vec<Route>, CatalogError> {
        let limit = limit.clamp(1, MAX_LIST_PAGE);
        let connection = self.lock()?;
        let mut statement = connection.prepare(&format!(
            "SELECT {ROUTE_COLUMNS} FROM isolation_connector_routes WHERE account > ?1 ORDER BY account LIMIT {limit}"))
            .map_err(|_| CatalogError::Unavailable)?;
        let raw = statement.query_map([after.map_or("", AccountId::as_str)], raw_route).map_err(|_| CatalogError::Unavailable)?
            .collect::<Result<Vec<_>, _>>().map_err(|_| CatalogError::Unavailable)?;
        drop(statement);
        drop(connection);
        let routes = raw.into_iter().map(decode_route).collect::<Result<Vec<_>, _>>()?;
        if routes.windows(2).any(|pair| pair[0].account >= pair[1].account)
            || after.is_some_and(|after| routes.first().is_some_and(|first| &first.account <= after)) {
            return Err(CatalogError::Unavailable);
        }
        Ok(routes)
    }

    /// Selection-side lookup by the string an adapter supplied. A string that
    /// is not a catalog id is simply unknown (refused by the caller).
    fn current_route(&self, account: &str) -> Result<Option<Route>, VerificationError> {
        let Ok(account) = AccountId::parse(account) else { return Ok(None) };
        Ok(self.route(&account)?)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, CatalogError> {
        self.connection.lock().map_err(|_| CatalogError::Unavailable)
    }
}

/// Insert one new active route under a freshly minted id, inside the
/// caller's write transaction (registration and C4 relink). Returns the id
/// and its generation (above every existing one).
fn insert_route(tx: &Transaction<'_>, label: &str, provider: &'static str, kind: ConnectorKind, binding_id: &str,
    now: i64) -> Result<(AccountId, i64), CatalogError> {
    for _ in 0..4 {
        let account = AccountId::generate()?;
        let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM isolation_connector_routes WHERE account=?1)",
            [account.as_str()], |row| row.get(0)).map_err(|_| CatalogError::Unavailable)?;
        if exists { continue; }
        return insert_route_as(tx, &account, label, provider, kind, binding_id, now).map(|generation| (account, generation));
    }
    Err(CatalogError::Unavailable)
}
fn insert_route_as(tx: &Transaction<'_>, account: &AccountId, label: &str, provider: &'static str, kind: ConnectorKind,
    binding_id: &str, now: i64) -> Result<i64, CatalogError> {
    let unavailable = |_| CatalogError::Unavailable;
    let (count, generation): (i64, i64) = tx.query_row(
        "SELECT count(*), coalesce(max(generation),0) FROM isolation_connector_routes", [],
        |row| Ok((row.get(0)?, row.get(1)?))).map_err(unavailable)?;
    if count >= MAX_ROUTES { return Err(CatalogError::Unavailable); }
    let generation = generation.checked_add(1).ok_or(CatalogError::Unavailable)?;
    tx.execute("INSERT INTO isolation_connector_routes
        (account,label,provider,kind,binding_id,generation,active,created_at,revoked_at)
        VALUES (?1,?2,?3,?4,?5,?6,1,?7,NULL)",
        params![account.as_str(), label, provider, kind.as_str(), binding_id, generation, now])
        .map_err(unavailable)?;
    Ok(generation)
}

type RawRoute = (String, String, String, String, String, i64, i64);
fn raw_route(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawRoute> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?))
}
/// Stored rows are re-validated; anything the writer could not have produced
/// is damage and refuses.
fn decode_route((account, label, provider, kind, binding, generation, active): RawRoute) -> Result<Route, CatalogError> {
    let bad = |_| CatalogError::Unavailable;
    let account = AccountId::parse(&account).map_err(bad)?;
    let provider = known_provider(&provider).ok_or(CatalogError::Unavailable)?;
    let kind = ConnectorKind::parse(&kind).ok_or(CatalogError::Unavailable)?;
    let binding = parse_binding_id(&binding)?;
    let generation = u64::try_from(generation).ok().filter(|g| *g > 0).ok_or(CatalogError::Unavailable)?;
    let active = match active { 1 => true, 0 => false, _ => return Err(CatalogError::Unavailable) };
    if !kind.admits(&binding) || label.is_empty() || label.len() > 512 { return Err(CatalogError::Unavailable); }
    Ok(Route { account, label, provider, kind, binding, generation, active })
}
fn known_provider(provider: &str) -> Option<&'static str> {
    PROVIDERS.iter().copied().find(|known| *known == provider)
}
fn now_seconds() -> Result<i64, CatalogError> {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok()).filter(|now| *now > 0)
        .ok_or(CatalogError::Unavailable)
}
/// The stored schema must be exactly ours: no missing, weakened or extra
/// trigger/index on the routes table. Whitespace is ignored because SQLite
/// normalizes it in `sqlite_master.sql`.
fn verify_schema(connection: &Connection) -> Result<(), CatalogError> {
    verify_table(connection, "isolation_connector_routes", &SCHEMA)?;
    verify_table(connection, DETAILS_TABLE_NAME, &DETAILS_SCHEMA)?;
    verify_table(connection, relink::RELINK_TABLE_NAME, &relink::RELINK_SCHEMA)?;
    verify_table(connection, origin::ORIGIN_TABLE_NAME, &origin::ORIGIN_SCHEMA)
}
fn verify_table(connection: &Connection, table: &str, schema: &[(&str, &str, &str)]) -> Result<(), CatalogError> {
    let unavailable = |_| CatalogError::Unavailable;
    let compact = |sql: &str| sql.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    let mut statement = connection.prepare(
        "SELECT type, name, sql FROM sqlite_master WHERE tbl_name=?1").map_err(unavailable)?;
    let objects = statement.query_map([table], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?,
        row.get::<_, Option<String>>(2)?))).map_err(unavailable)?
        .collect::<Result<Vec<_>, _>>().map_err(unavailable)?;
    let autoindex = format!("sqlite_autoindex_{table}_");
    let mut matched = 0;
    for (kind, name, sql) in objects {
        if kind == "index" && sql.is_none() && name.strip_prefix(autoindex.as_str())
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())) { continue; }
        let Some((_, _, expected)) = schema.iter().find(|(k, n, _)| *k == kind && *n == name) else {
            return Err(CatalogError::Unavailable);
        };
        if sql.as_deref().map(compact) != Some(compact(*expected)) { return Err(CatalogError::Unavailable); }
        matched += 1;
    }
    if matched != schema.len() { return Err(CatalogError::Unavailable); }
    Ok(())
}
/// The database and any SQLite sidecar that exists must be a regular,
/// single-link file owned by the effective uid with no group/other bits.
/// lstat only: never follows a link and opens no descriptor (closing one
/// would drop another connection's POSIX locks).
#[cfg(unix)]
fn verify_private_files(home: &Path) -> Result<(), CatalogError> {
    use std::os::unix::fs::MetadataExt;
    let uid = nix::unistd::geteuid().as_raw();
    for suffix in ["", "-journal", "-wal", "-shm"] {
        match std::fs::symlink_metadata(home.join(format!("{CATALOG_FILE}{suffix}"))) {
            Ok(meta) if meta.file_type().is_file() && meta.nlink() == 1 && meta.uid() == uid
                && meta.mode() & 0o077 == 0 => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err(CatalogError::Unavailable),
        }
    }
    Ok(())
}

pub fn provider_for_tool(tool: &str) -> Option<&'static str> {
    Some(match tool {
        "google_status" | "gmail_search" | "gmail_read" | "gmail_create_draft" |
        "google_chat_spaces" | "google_chat_messages" |
        "calendar_list_events" | "calendar_create_event" | "sheets_read" | "sheets_append" |
        "forms_get" | "forms_list_responses" | "gtasks_lists" | "gtasks_list" | "gtasks_create" |
        "gtasks_complete" | "drive_search" | "drive_read" | "docs_read" | "docs_append" | "slides_read" => "google",
        "notion_status" | "notion_search" | "notion_page_read" | "notion_page_append" => "notion",
        "github_status" | "github_search_issues" | "github_issue_read" | "github_pr_read" | "github_issue_comment" => "github",
        "odoo_connect" | "odoo_status" | "odoo_crm_leads" | "odoo_crm_create_lead" | "odoo_crm_update_stage" |
        "odoo_sale_orders" | "odoo_sale_create_quotation" | "odoo_sale_confirm" | "odoo_inventory_products" |
        "odoo_inventory_check" | "odoo_invoice_list" | "odoo_payment_status" | "odoo_search" | "odoo_execute" |
        "odoo_report" | "odoo_partner_search" | "odoo_schema_overview" | "odoo_schema_fields" => "odoo",
        "db_sources" | "db_tables" | "db_select" | "db_query" => "database",
        "group_history_read" | "send_message" | "send_photo" | "send_sticker" |
        "channel_config" | "channel_config_list" | "channel_status" | "pairing_manage" => "channel",
        "web_search" => "search",
        "model_search" | "model_recommend" => "model",
        "computer_login" => "computer",
        _ => return None,
    })
}

/// A concrete current product selection, supplied by a trusted gateway adapter.
/// This is never deserialized from employee arguments. SharedHome must identify
/// the installed home account and its tenant after current per-employee config,
/// integration, requester and consent rules have all been checked.
#[derive(Clone, PartialEq, Eq)]
pub enum ProductConnectorAccount {
    Personal { account: String, owner: String },
    SharedHome { account: String, tenant: String },
}
impl ProductConnectorAccount {
    pub fn account(&self) -> &str {
        match self { Self::Personal { account, .. } | Self::SharedHome { account, .. } => account }
    }
    fn binding(&self) -> Result<EmployeeBinding, VerificationError> {
        match self {
            Self::Personal { owner, .. } => Text::new(owner.clone()).map(EmployeeBinding::Personal),
            Self::SharedHome { tenant, .. } => Text::new(tenant.clone()).map(EmployeeBinding::Shared),
        }.map_err(|_| VerificationError::Invalid)
    }
}

/// Catalog half of selection: the exact route must be active, for this
/// provider, carry the binding the product selection claims, and be of a kind
/// that selection may present. A Personal product account needs a Personal
/// route; a shared one needs a SharedTenant/SharedHome route, and an explicit
/// home fallback (binding differs from the employee's) only a SharedHome route.
fn route_admits(route: &Route, provider: &str, candidate: &ProductConnectorAccount, binding: &EmployeeBinding,
    employee: &EmployeeBinding) -> bool {
    let kind = match candidate {
        ProductConnectorAccount::Personal { .. } => route.kind == ConnectorKind::Personal,
        ProductConnectorAccount::SharedHome { .. } =>
            matches!(route.kind, ConnectorKind::SharedTenant | ConnectorKind::SharedHome)
                && (binding == employee || route.kind == ConnectorKind::SharedHome),
    };
    route.active && route.provider == provider && &route.binding == binding && kind
}

/// Implements today's live product authority, not a startup snapshot. Selection
/// is repeated before credentials, retries and effects, so changing configuration
/// cannot keep an earlier home fallback alive. Missing adapters fail closed.
pub trait LiveConnectorSelectionPolicy: Send + Sync {
    fn supports(&self, tool: &str) -> bool;
    fn candidates(&self, context: &RequestContext, tool: &str, provider: &str)
        -> Result<Vec<ProductConnectorAccount>, VerificationError>;
    /// Argument selectors may narrow product-authorized accounts, never add one.
    /// Rechecks receive the exact original arguments retained by selection.
    fn candidates_for_call(&self, context: &RequestContext, tool: &str, provider: &str, _arguments: &Value)
        -> Result<Vec<ProductConnectorAccount>, VerificationError> {
        self.candidates(context, tool, provider)
    }
}

/// Constructible only through selection and live checks. Executors must use
/// this exact account; never select defaults from home, environment or config.
pub struct SelectedConnector {
    account: String,
    provider: &'static str,
    binding: EmployeeBinding,
    /// Catalog route kind and generation observed at selection. Every
    /// checkpoint requires the route to still be active with exactly these.
    kind: ConnectorKind,
    generation: u64,
    tool: String,
    arguments: Value,
    product_selection: (Arc<dyn LiveConnectorSelectionPolicy>, ProductConnectorAccount),
    context: RequestContext,
    bindings: Arc<dyn BindingAuthority>,
    catalog: Arc<ConnectorCatalog>,
}
impl SelectedConnector {
    pub fn account(&self) -> &str { &self.account }
    pub fn provider(&self) -> &'static str { self.provider }
    pub fn binding(&self) -> &EmployeeBinding { &self.binding }
    pub fn kind(&self) -> ConnectorKind { self.kind }
    pub fn generation(&self) -> u64 { self.generation }
    pub fn context(&self) -> &RequestContext { &self.context }
    /// Carry account/product revocation to every legacy storage or HTTP checkpoint.
    pub fn io_context(self: &Arc<Self>) -> RequestContext {
        struct Selection(Arc<SelectedConnector>);
        impl duduclaw_isolation::capability::LiveRequestVerifier for Selection {
            fn verify_live(&self, _: &duduclaw_isolation::capability::CapabilityRecord, _: u64)
                -> Result<(), VerificationError> { self.0.recheck() }
        }
        RequestContext::with_live_verifier(self.context.record().clone(), Arc::new(Selection(self.clone())))
    }
    /// Only for a sealed source owner that independently checks its original
    /// product proof on every effect (Google). Full selection is checked before
    /// entering and after leaving that owner; nested source locks never reenter
    /// the product selector. Binding/catalog/run revocation is still live.
    pub(crate) fn binding_io_context(self: &Arc<Self>) -> RequestContext {
        struct Binding(Arc<SelectedConnector>);
        impl duduclaw_isolation::capability::LiveRequestVerifier for Binding {
            fn verify_live(&self, _: &duduclaw_isolation::capability::CapabilityRecord, _: u64)
                -> Result<(), VerificationError> { self.0.recheck_binding() }
        }
        RequestContext::with_live_verifier(self.context.record().clone(), Arc::new(Binding(self.clone())))
    }
    /// A directory operation may inspect only separately authorized members.
    /// Each member repeats the full binding/catalog/product selection path.
    pub fn collection_member(&self, account: &str) -> Result<Self, VerificationError> {
        if self.tool != "db_sources" { return Err(VerificationError::Invalid); }
        self.recheck()?;
        let mut arguments = self.arguments.clone();
        arguments.as_object_mut().ok_or(VerificationError::Invalid)?
            .insert("connector_account".into(), Value::String(account.to_owned()));
        Self::select_with_policy(&self.context, &self.tool, &arguments, self.bindings.clone(),
            self.catalog.clone(), Some(self.product_selection.0.clone()))
    }

    pub fn tool(&self) -> &str { &self.tool }
    /// Bind an account selection to the precise authorized operation. Sharing a
    /// provider, record or account never lets a status/read selection authorize
    /// another tool or a different recipient/body/query.
    pub fn verify_call(&self, call: &AuthorizedToolCall) -> Result<(), VerificationError> {
        self.recheck()?;
        let connectors: Vec<_> = call.resources().iter().filter_map(|resource| match resource {
            ResourceGrant::Connector { account, binding } => Some((account.as_str(), binding)),
            _ => None,
        }).collect();
        if call.context().record() != self.context.record() || call.name() != self.tool
            || call.arguments() != &self.arguments || provider_for_tool(call.name()) != Some(self.provider)
            || connectors != [(self.account.as_str(), &self.binding)] {
            return Err(VerificationError::BindingMismatch);
        }
        call.context().verify_live(duduclaw_mcp_boundary::authority::now_seconds()?)
    }
    /// Repeat immediately before credential acquisition and every effect/retry.
    pub fn recheck(&self) -> Result<(), VerificationError> {
        let now = duduclaw_mcp_boundary::authority::now_seconds()?;
        self.context.verify_live(now)?;
        let (policy, selected) = &self.product_selection;
        if !policy.supports(&self.tool) { return Err(VerificationError::Unavailable); }
        let candidates = policy.candidates_for_call(&self.context, &self.tool, self.provider, &self.arguments)?;
        if candidates.len() > 128 || candidates.iter().filter(|candidate| *candidate == selected).count() != 1 {
            return Err(VerificationError::BindingMismatch);
        }
        self.recheck_binding()
    }
    /// Source owners validate their pinned product proof under their own locks.
    /// This checkpoint may run while those locks are held and therefore must
    /// never invoke product selection or reopen the source's metadata store.
    fn recheck_binding(&self) -> Result<(), VerificationError> {
        let record = self.context.record();
        self.context.verify_live(duduclaw_mcp_boundary::authority::now_seconds()?)?;
        let bindings = &self.bindings;
        // The authority also compares the route generation pinned here (C2).
        if self.binding == record.employee_binding {
            bindings.authorize_connector(record.employee, &record.employee_binding, &record.principal, &self.account,
                self.generation).map_err(|_| VerificationError::BindingMismatch)?;
        } else if matches!(&self.product_selection.1, ProductConnectorAccount::SharedHome { .. }) {
            bindings.authorize_product_home_connector(record.employee, &record.employee_binding,
                &record.principal, &self.account, self.generation, &self.binding)
                .map_err(|_| VerificationError::BindingMismatch)?;
        } else { return Err(VerificationError::BindingMismatch); }
        // Same route, same state: any revoke/advance/relink since selection
        // moved the generation. Store errors propagate and refuse as well.
        let route = self.catalog.current_route(&self.account)?.ok_or(VerificationError::BindingMismatch)?;
        if route.generation != self.generation || route.kind != self.kind
            || !route_admits(&route, self.provider, &self.product_selection.1, &self.binding, &record.employee_binding) {
            return Err(VerificationError::BindingMismatch);
        }
        // Policy and binding reads may run concurrently with a lifecycle revoke.
        // Make the authority read the final operation before returning to I/O.
        self.context.verify_live(duduclaw_mcp_boundary::authority::now_seconds()?)
    }
    pub fn select_with_policy(context: &RequestContext, tool: &str, arguments: &Value,
        bindings: Arc<dyn BindingAuthority>, catalog: Arc<ConnectorCatalog>,
        product_policy: Option<Arc<dyn LiveConnectorSelectionPolicy>>) -> Result<Self, VerificationError> {
        let provider = provider_for_tool(tool).ok_or(VerificationError::Invalid)?;
        let employee_scope = context.record().employee_scope()
            && duduclaw_mcp_boundary::tool_policy::policy(tool)
                .is_some_and(|policy| policy.route == duduclaw_mcp_boundary::tool_policy::Route::Connector);
        if !context.record().tools.as_slice().iter().any(|allowed| allowed.as_str() == tool) {
            return Err(VerificationError::Invalid);
        }
        let explicit = arguments.get("connector_account").map(|v| v.as_str().ok_or(VerificationError::Invalid)).transpose()?;
        // Credentials cannot be declared/overridden through an isolated call.
        if arguments.as_object().is_none_or(|o| o.keys().any(|key|
            matches!(key.as_str(), "access_token" | "refresh_token" | "api_key" | "password" | "credentials" | "token"
                | "account" | "account_id" | "credential_id" | "connection_string" | "base_url" | "endpoint"))) {
            return Err(VerificationError::Invalid);
        }
        let policy = product_policy.ok_or(VerificationError::Unavailable)?;
        if !policy.supports(tool) { return Err(VerificationError::Unavailable); }
        let candidates = policy.candidates_for_call(context, tool, provider, arguments)?;
        if candidates.len() > 128 { return Err(VerificationError::Invalid); }
        let mut permitted = Vec::new();
        for candidate in candidates {
            if explicit.is_some_and(|account| account != candidate.account()) { continue; }
            let binding = candidate.binding()?;
            if !employee_scope && !context.record().io_resources().any(|grant|
                matches!(grant, ResourceGrant::Connector { account, binding: allowed }
                    if account.as_str() == candidate.account() && allowed == &binding)) { continue; }
            // The product adapter cannot authorize another person's private
            // credential, even if a requester/consent implementation errs.
            if let EmployeeBinding::Personal(owner) = &binding {
                if !context.record().personal_owner_matches(owner.as_str()) { continue; }
            }
            if let (EmployeeBinding::Shared(employee), EmployeeBinding::Shared(account)) =
                (&context.record().employee_binding, &binding) {
                if employee != account { continue; }
            }
            let Some(route) = catalog.current_route(candidate.account())? else { continue };
            if route_admits(&route, provider, &candidate, &binding, &context.record().employee_binding) {
                permitted.push((candidate, binding, route.kind, route.generation));
            }
        }
        if tool == "db_sources" && explicit.is_none() {
            // A collection anchor is chosen only after the same owner, finite
            // grant, catalog AND binding checks as each individual member.
            permitted.sort_by(|(a,..),(b,..)|a.account().cmp(b.account()));
            for (candidate,binding,kind,generation) in permitted {
                let selected=Self { account:candidate.account().to_owned(),provider,binding,kind,generation,tool:tool.to_owned(),
                    arguments:arguments.clone(),product_selection:(policy.clone(),candidate),context:context.clone(),
                    bindings:bindings.clone(),catalog:catalog.clone() };
                if selected.recheck().is_ok() { return Ok(selected); }
            }
            return Err(VerificationError::BindingMismatch);
        }
        if permitted.len() != 1 { return Err(VerificationError::BindingMismatch); }
        let (candidate, binding, kind, generation) = permitted.remove(0);
        let selected = Self { account: candidate.account().to_owned(), provider, binding, kind, generation,
            tool: tool.to_owned(), arguments: arguments.clone(), product_selection: (policy, candidate),
            context: context.clone(), bindings, catalog };
        selected.recheck()?;
        Ok(selected)
    }
}

/// Boot injects account-specific connector adapters. A missing adapter refuses;
/// an adapter must retain all provider/recipient/egress gates and recheck before
/// sending. It must never forward the call to a legacy home-account selector.
pub trait BoundConnectorExecutor: Send + Sync {
    fn supports(&self, tool: &str) -> bool;
    fn execute<'a>(&'a self, selected: SelectedConnector, call: AuthorizedToolCall)
        -> Pin<Box<dyn Future<Output = Result<Value, VerificationError>> + Send + 'a>>;
}

#[cfg(test)]
#[path = "isolation_connectors_tests.rs"]
mod tests;
