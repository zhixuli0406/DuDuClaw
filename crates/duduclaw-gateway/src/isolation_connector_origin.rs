//! Migration provenance of catalog routes (native Linux isolation v1.81, C5).
//!
//! Child module of `isolation_connectors`: it uses the catalog's private
//! connection and route insert, so a migrated route, its C6 details and its
//! provenance row are ONE catalog transaction checked by the same triggers
//! as C1/C3/C6.
//!
//! A provenance row records which applied migration Connector proposal
//! (operation = proposal id + revision, digest = proposal digest) created
//! which route. It makes the confirmation apply step idempotent across
//! crashes without ever minting a second route for the same proposal, and
//! lets compensation find (and revoke) exactly the route the proposal
//! created. Rows are written only in the transaction that inserts their
//! route, never updated and never deleted, so a revoked migrated route can
//! never be "re-applied" into a fresh id and no id is ever reused.
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use duduclaw_isolation::capability::EmployeeBinding;
use crate::isolation_connector_details::{self as details_table, AccountDetailRecord};
use super::{AccountId, CatalogError, ConnectorCatalog, ConnectorKind, binding_id, insert_route, known_provider, now_seconds};

pub(super) const ORIGIN_TABLE_NAME: &str = "isolation_connector_migrations";
/// Upper bound on same-scope routes compared for a detail conflict.
const MAX_SCOPE_ROUTES: usize = 1024;

const ORIGIN_TABLE: &str = "CREATE TABLE isolation_connector_migrations (
    operation TEXT PRIMARY KEY NOT NULL,
    digest TEXT NOT NULL,
    account TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    CHECK(length(operation) BETWEEN 34 AND 64 AND operation NOT GLOB '*[^0-9a-f-]*'),
    CHECK(length(digest)=64 AND digest NOT GLOB '*[^0-9a-f]*'),
    CHECK(length(account)=32 AND account NOT GLOB '*[^0-9a-f]*'),
    CHECK(created_at > 0)
) STRICT";
// Born only for the route inserted in the same transaction: it is active and
// holds the newest generation. One row per operation and per account.
const ORIGIN_INSERT_TRIGGER: &str = "CREATE TRIGGER isolation_connector_migrations_insert
    BEFORE INSERT ON isolation_connector_migrations
    WHEN EXISTS(SELECT 1 FROM isolation_connector_migrations WHERE operation=NEW.operation OR account=NEW.account)
        OR NOT EXISTS(SELECT 1 FROM isolation_connector_routes WHERE account=NEW.account AND active=1
            AND generation=(SELECT max(generation) FROM isolation_connector_routes))
    BEGIN SELECT RAISE(ABORT,'immutable migration origin'); END";
const ORIGIN_UPDATE_TRIGGER: &str = "CREATE TRIGGER isolation_connector_migrations_update
    BEFORE UPDATE ON isolation_connector_migrations
    BEGIN SELECT RAISE(ABORT,'immutable migration origin'); END";
const ORIGIN_DELETE_TRIGGER: &str = "CREATE TRIGGER isolation_connector_migrations_no_delete
    BEFORE DELETE ON isolation_connector_migrations
    BEGIN SELECT RAISE(ABORT,'immutable migration origin'); END";
pub(super) const ORIGIN_SCHEMA: [(&str, &str, &str); 4] = [
    ("table", ORIGIN_TABLE_NAME, ORIGIN_TABLE),
    ("trigger", "isolation_connector_migrations_insert", ORIGIN_INSERT_TRIGGER),
    ("trigger", "isolation_connector_migrations_update", ORIGIN_UPDATE_TRIGGER),
    ("trigger", "isolation_connector_migrations_no_delete", ORIGIN_DELETE_TRIGGER),
];

/// The applied migration proposal a route is created for: its operation
/// (lowercase hex proposal id, `-`, lowercase hex revision) and the
/// lowercase hex SHA-256 proposal digest. Carries no authority itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationOrigin { operation: String, digest: String }
impl MigrationOrigin {
    pub fn new(operation: &str, digest: &str) -> Result<Self, CatalogError> {
        let hex = |b: u8| matches!(b, b'0'..=b'9' | b'a'..=b'f');
        let operation_ok = (34..=64).contains(&operation.len())
            && operation.bytes().all(|b| hex(b) || b == b'-')
            && operation.split_once('-').is_some_and(|(id, revision)| id.len() == 32 && !revision.is_empty()
                && !revision.contains('-'));
        if !operation_ok || digest.len() != 64 || !digest.bytes().all(hex) { return Err(CatalogError::Invalid); }
        Ok(Self { operation: operation.to_owned(), digest: digest.to_owned() })
    }
}

/// Outcome of [`ConnectorCatalog::register_migrated`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigratedRoute {
    /// A new route (and its details) was inserted for this origin.
    Created(AccountId),
    /// This origin already created a route (a crash-safe re-apply). The
    /// caller must read it back; a revoked one stays revoked.
    Existing(AccountId),
    /// Another route already covers this account, so nothing was inserted:
    /// any route (active or revoked) of the same provider/kind/binding for a
    /// provider without details (a revoked one's successor is created only by
    /// the C4 relink or an admin C3 provision), or an active route with the
    /// identical details record.
    Conflict,
}

impl ConnectorCatalog {
    /// Register the route of an applied migration proposal, its C6 details
    /// (exactly when the provider needs them) and its provenance row in one
    /// transaction, or return the route this origin already created.
    #[allow(clippy::too_many_arguments)]
    pub fn register_migrated(&self, origin: &MigrationOrigin, label: &str, provider: &str, kind: ConnectorKind,
        binding: &EmployeeBinding, details: Option<&AccountDetailRecord>) -> Result<MigratedRoute, CatalogError> {
        let unavailable = |_| CatalogError::Unavailable;
        let provider = known_provider(provider).ok_or(CatalogError::Invalid)?;
        if label.trim().is_empty() || label.len() > 128 || label.chars().any(char::is_control) || !kind.admits(binding)
            || details_table::needs_details(provider) != details.is_some() {
            return Err(CatalogError::Invalid);
        }
        let binding_id = binding_id(binding)?;
        let now = now_seconds()?;
        let mut connection = self.lock()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
        if let Some((digest, account)) = read(&tx, &origin.operation)? {
            // The operation includes the revision, so its digest cannot change.
            if digest != origin.digest { return Err(CatalogError::Unavailable); }
            return Ok(MigratedRoute::Existing(account));
        }
        if conflict(&tx, provider, kind, &binding_id, details)? { return Ok(MigratedRoute::Conflict); }
        let (account, generation) = insert_route(&tx, label, provider, kind, &binding_id, now)?;
        if let Some(details) = details {
            details_table::insert_row(&tx, &account, generation, &binding_id, provider, details, now)?;
        }
        let changed = tx.execute("INSERT INTO isolation_connector_migrations (operation,digest,account,created_at)
            VALUES (?1,?2,?3,?4)", params![origin.operation, origin.digest, account.as_str(), now]).map_err(unavailable)?;
        if changed != 1 { return Err(CatalogError::Unavailable); }
        tx.commit().map_err(unavailable)?;
        Ok(MigratedRoute::Created(account))
    }

    /// The route this origin created, if any. A row for the operation with
    /// another digest is damage (`Unavailable`).
    pub fn migrated_account(&self, origin: &MigrationOrigin) -> Result<Option<AccountId>, CatalogError> {
        let connection = self.lock()?;
        let row = read(&connection, &origin.operation)?;
        drop(connection);
        match row {
            None => Ok(None),
            Some((digest, account)) if digest == origin.digest => Ok(Some(account)),
            Some(_) => Err(CatalogError::Unavailable),
        }
    }
}

fn read(connection: &Connection, operation: &str) -> Result<Option<(String, AccountId)>, CatalogError> {
    let row: Option<(String, String)> = connection.query_row(
        "SELECT digest, account FROM isolation_connector_migrations WHERE operation=?1", [operation],
        |row| Ok((row.get(0)?, row.get(1)?))).optional().map_err(|_| CatalogError::Unavailable)?;
    row.map(|(digest, account)| AccountId::parse(&account).map(|account| (digest, account))
        .map_err(|_| CatalogError::Unavailable)).transpose()
}

fn conflict(connection: &Connection, provider: &str, kind: ConnectorKind, binding_id: &str,
    details: Option<&AccountDetailRecord>) -> Result<bool, CatalogError> {
    let unavailable = |_| CatalogError::Unavailable;
    let Some(details) = details else {
        return connection.query_row("SELECT EXISTS(SELECT 1 FROM isolation_connector_routes
            WHERE provider=?1 AND kind=?2 AND binding_id=?3)", params![provider, kind.as_str(), binding_id],
            |row| row.get(0)).map_err(unavailable);
    };
    let mut statement = connection.prepare(&format!("SELECT account FROM isolation_connector_routes
        WHERE provider=?1 AND kind=?2 AND binding_id=?3 AND active=1 ORDER BY account LIMIT {}", MAX_SCOPE_ROUTES + 1))
        .map_err(unavailable)?;
    let accounts = statement.query_map(params![provider, kind.as_str(), binding_id], |row| row.get::<_, String>(0))
        .map_err(unavailable)?.collect::<Result<Vec<_>, _>>().map_err(unavailable)?;
    drop(statement);
    if accounts.len() > MAX_SCOPE_ROUTES { return Err(CatalogError::Unavailable); }
    for account in accounts {
        let account = AccountId::parse(&account).map_err(|_| CatalogError::Unavailable)?;
        if details_table::read_row(connection, &account)?.is_some_and(|stored| &stored.record == details) {
            return Ok(true);
        }
    }
    Ok(false)
}
