//! RM-134 (v1.82): the hashed store for dashboard-issued MCP API keys.
//!
//! Until v1.81 every dashboard-issued `ddc_<env>_<32hex>` key lived in
//! `config.toml` with the **plaintext key as the TOML table name**, expired
//! hard after 30 days and could only be revoked by pasting it back. The keys
//! now live in `<home>/mcp_keys.db` (the *authority* store):
//!
//! - only the SHA-256 digest of the key is stored (the same hex digest
//!   [`crate::mcp_oauth_store::digest`] and the Google credential reference
//!   use, so a `CredentialRef` and an OAuth consent can point at a row);
//! - each row has a public random id, a unique name, scopes, creator,
//!   creation time, an optional expiry (`NULL` = never), disable / delete
//!   markers and a row `version` that every mutation increments;
//! - a deleted key keeps its row as a tombstone (hash retained, hidden from
//!   the dashboard) so a stale `[mcp_keys]` copy can never resurrect it;
//! - `mcp_internal_keys` holds the fingerprints of the gateway's own
//!   `gateway-internal` key(s), recorded by the gateway when it provisions or
//!   rotates that key (`duduclaw-gateway::mcp_internal_key`);
//! - `mcp_legacy_pending` and the `legacy_migration` meta row drive the
//!   one-time move out of `config.toml [mcp_keys]` (see [`Authority`]).
//!
//! Usage telemetry (`last_used_at`) is NOT in the authority store: the
//! tool-hosting processes write it to `<home>/mcp_keys_usage.db`
//! ([`record_usage`]), which holds nothing that authenticates anything and is
//! never read by an authenticator. The gateway merges it into the
//! `mcp_keys.list` answer when it builds that answer ([`usage_map`]).
//!
//! This module is the **read side** shared by every process that
//! authenticates these keys: the stdio `duduclaw mcp-server`, the
//! `duduclaw http-server`, the OAuth consent check in
//! [`crate::mcp_oauth_store`] and the gateway's Google bridge. Those open the
//! authority store strictly read-only ([`open_read_only`]: no create, SQLite
//! `SQLITE_OPEN_READ_ONLY`). Only the gateway opens it read-write
//! ([`open_for_gateway`], used by `duduclaw_auth::mcp_keys`).
//!
//! Fail closed: an existing store that cannot be opened, is not a private
//! regular file ([`crate::private_sqlite`]), lacks the tables or carries an
//! unexpected schema / `user_version` is a [`StoreError`], and every caller
//! treats that as "deny". Only a store file that does not exist means "no
//! store yet".

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::private_sqlite::{self, Access};

/// Store file name under the DuDuClaw home.
pub const STORE_FILE: &str = "mcp_keys.db";

/// Usage telemetry file name under the DuDuClaw home (never authenticates).
pub const USAGE_FILE: &str = "mcp_keys_usage.db";

/// The `client_id` of the gateway's own auto-rotated internal key. That key
/// stays in `config.toml [mcp_keys]` (`duduclaw-gateway::mcp_internal_key`);
/// only its fingerprint is recorded here (`mcp_internal_keys`). It is never
/// created as a dashboard key.
pub const RESERVED_INTERNAL_CLIENT_ID: &str = "gateway-internal";

/// Longest key name an Admin may choose (characters, not bytes).
pub const NAME_MAX_CHARS: usize = 60;

/// `last_used_at` is written at most once per key per this many seconds.
pub const LAST_USED_THROTTLE_SECS: i64 = 60;

/// The pre-v1.82 rule for a `[mcp_keys]` entry refused it once
/// `(now - created_at).num_days() > 30`. `num_days` truncates, so the first
/// refused instant is exactly `created_at + 31 days`; [`legacy_expires_at`]
/// is that instant (in whole seconds).
pub const LEGACY_REFUSED_AFTER_DAYS: i64 = 31;

/// Days before `expires_at` at which Admins are notified (each once).
pub const NOTICE_THRESHOLDS_DAYS: [i64; 2] = [7, 1];

/// `PRAGMA user_version` of the authority store. Pinned: any other value is
/// an unexpected schema (deny).
pub const SCHEMA_VERSION: i64 = 1;

/// `mcp_api_key_meta` row recording the `[mcp_keys]` migration state:
/// `pending:<ts>` once the gateway recorded the pending set, then
/// `complete:<ts>`.
pub const META_LEGACY_MIGRATION: &str = "legacy_migration";

/// The authority store schema. Applied verbatim by [`open_for_gateway`] and
/// pinned byte-for-byte by the importer catalog
/// `crates/duduclaw-import/src/mcp-keys-v182.sql` (its last bytes are this
/// text; `duduclaw-auth`'s `importer_catalog_matches_a_fresh_store` checks
/// both the bytes and the resulting schema).
pub const SCHEMA_SQL: &str = "CREATE TABLE IF NOT EXISTS mcp_api_keys (
    id TEXT PRIMARY KEY,
    key_hash TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    client_id TEXT NOT NULL,
    is_external INTEGER NOT NULL CHECK (is_external IN (0, 1)),
    env TEXT NOT NULL CHECK (env IN ('prod', 'staging', 'dev')),
    hint TEXT NOT NULL,
    scopes TEXT NOT NULL,
    created_by TEXT,
    created_at TEXT NOT NULL,
    expires_at TEXT,
    disabled_at TEXT,
    disabled_by TEXT,
    deleted_at TEXT,
    deleted_by TEXT,
    migrated INTEGER NOT NULL DEFAULT 0 CHECK (migrated IN (0, 1)),
    expiry_reviewed INTEGER NOT NULL DEFAULT 1 CHECK (expiry_reviewed IN (0, 1)),
    legacy_entry_hash TEXT,
    version INTEGER NOT NULL DEFAULT 1 CHECK (version >= 1),
    agent_binding TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_mcp_api_keys_name
    ON mcp_api_keys(name COLLATE NOCASE) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_mcp_api_keys_expiry
    ON mcp_api_keys(expires_at) WHERE deleted_at IS NULL AND disabled_at IS NULL;
CREATE TABLE IF NOT EXISTS mcp_api_key_meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS mcp_internal_keys (
    key_hash TEXT PRIMARY KEY,
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS mcp_legacy_pending (
    key_hash TEXT PRIMARY KEY,
    recorded_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS mcp_legacy_ignored (
    key_hash TEXT PRIMARY KEY,
    hint TEXT NOT NULL,
    first_seen_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS mcp_api_key_notices (
    key_id TEXT NOT NULL,
    threshold TEXT NOT NULL CHECK (threshold IN ('7d', '1d')),
    expires_at TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'delivered', 'closed')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    claimed_until TEXT,
    next_attempt_at TEXT,
    finished_at TEXT,
    claim_token TEXT,
    PRIMARY KEY (key_id, threshold, expires_at)
);
PRAGMA user_version = 1;
";

/// The usage telemetry schema (`mcp_keys_usage.db`).
pub const USAGE_SCHEMA_SQL: &str = "CREATE TABLE IF NOT EXISTS mcp_key_usage (
    key_id TEXT PRIMARY KEY,
    last_used_at TEXT NOT NULL
);
PRAGMA user_version = 1;
";

/// Every column of `mcp_api_keys`, in the order [`key_from_row`] reads them.
pub const KEY_COLUMNS: &str = "id, key_hash, name, client_id, is_external, env, hint, scopes, \
    created_by, created_at, expires_at, disabled_at, disabled_by, deleted_at, \
    migrated, expiry_reviewed, legacy_entry_hash, version, agent_binding";

/// `<home>/mcp_keys.db`.
pub fn store_path(home: &Path) -> PathBuf {
    home.join(STORE_FILE)
}

/// `<home>/mcp_keys_usage.db`.
pub fn usage_path(home: &Path) -> PathBuf {
    home.join(USAGE_FILE)
}

/// Hex SHA-256 of a presented key — the only form ever stored.
pub fn digest(raw: &str) -> String {
    crate::mcp_oauth_store::digest(raw)
}

/// The one timestamp format this store writes (`2026-11-13T08:00:00Z`).
/// Fixed width, so SQL string comparison orders it correctly. Fractional
/// seconds are truncated.
pub fn timestamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Parse a stored timestamp.
pub fn parse_timestamp(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw).ok().map(|dt| dt.with_timezone(&Utc))
}

/// `at` truncated to the whole second the store keeps.
pub fn whole_second(at: DateTime<Utc>) -> DateTime<Utc> {
    parse_timestamp(&timestamp(at)).unwrap_or(at)
}

/// How far ahead of `now` a legacy `created_at` may lie (clock skew between
/// the machine that wrote it and this one) before the entry is refused.
pub const LEGACY_FUTURE_SKEW_SECS: i64 = 300;

/// v1.82 (RM-134 third review): a legacy `created_at` more than
/// [`LEGACY_FUTURE_SKEW_SECS`] ahead of `now` is not credible. Before, a
/// future date counted as age 0, so an entry written with a far-future date
/// never aged out (and won "newest entry" selections). Every place a legacy
/// entry's age is evaluated refuses such an entry.
pub fn legacy_created_at_credible(created_at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    created_at <= now + chrono::Duration::seconds(LEGACY_FUTURE_SKEW_SECS)
}

/// The pre-v1.82 `[mcp_keys]` age rule: accepted while
/// `(now - created_at).num_days() <= 30`. A `created_at` up to
/// [`LEGACY_FUTURE_SKEW_SECS`] in the future counts as age 0; one further
/// ahead is refused ([`legacy_created_at_credible`]). Shared by every legacy
/// read path so they cannot drift.
pub fn legacy_age_accepts(created_at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    legacy_created_at_credible(created_at, now)
        && now.signed_duration_since(created_at).num_days().max(0) <= LEGACY_REFUSED_AFTER_DAYS - 1
}

/// The instant [`legacy_age_accepts`] starts refusing a key created at
/// `created_at` (`created_at + 31 days`), truncated to the whole second the
/// store keeps — so a migrated key ends at the same second the old rule did
/// (less than one second earlier when the old `created_at` carried a
/// fraction of a second).
pub fn legacy_expires_at(created_at: DateTime<Utc>) -> DateTime<Utc> {
    whole_second(created_at + chrono::Duration::days(LEGACY_REFUSED_AFTER_DAYS))
}

/// Exact `ddc_<env>_<32 lowercase hex>` shape of a dashboard key.
pub fn is_key_format(raw: &str) -> bool {
    let Some(rest) = raw.strip_prefix("ddc_") else { return false };
    let hex = ["prod_", "staging_", "dev_"].iter().find_map(|env| rest.strip_prefix(env));
    hex.is_some_and(|hex| hex.len() == 32 && hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
}

/// The environment segment of a well-formed key (`prod` / `staging` / `dev`).
pub fn key_env(raw: &str) -> Option<&'static str> {
    if !is_key_format(raw) {
        return None;
    }
    ["prod", "staging", "dev"].into_iter().find(|env| {
        raw.strip_prefix("ddc_").and_then(|rest| rest.strip_prefix(env)).is_some_and(|r| r.starts_with('_'))
    })
}

/// Display hint for a key: its environment and last four hex characters
/// (`ddc_prod_…3f9a`). 16 of the 128 random bits; enough to tell keys apart
/// in a client's settings (or to find an entry in `config.toml`), useless for
/// guessing. Never a fingerprint.
pub fn key_hint(raw: &str) -> String {
    let env = key_env(raw).unwrap_or("prod");
    let tail: String = {
        let chars: Vec<char> = raw.chars().collect();
        chars[chars.len().saturating_sub(4)..].iter().collect()
    };
    format!("ddc_{env}_…{tail}")
}

/// A lowercase hex SHA-256 digest.
pub fn is_digest(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Length-checked, data-independent comparison.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Lifecycle state of a stored key at a point in time. Deleted outranks
/// disabled, which outranks expired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyState {
    Active,
    Disabled,
    Expired,
    Deleted,
}

impl KeyState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Disabled => "disabled",
            Self::Expired => "expired",
            Self::Deleted => "deleted",
        }
    }
}

/// One stored key. Carries the digest (needed to bind OAuth consent and the
/// Google credential reference) but never the key; `Debug` omits the digest
/// anyway so a stray `{:?}` in a log line stays harmless.
#[derive(Clone, PartialEq, Eq)]
pub struct StoredKey {
    pub id: String,
    pub key_hash: String,
    pub name: String,
    pub client_id: String,
    pub is_external: bool,
    pub env: String,
    pub hint: String,
    pub scopes: Vec<String>,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub disabled_at: Option<DateTime<Utc>>,
    pub disabled_by: Option<String>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub migrated: bool,
    pub expiry_reviewed: bool,
    pub legacy_entry_hash: Option<String>,
    /// Incremented by every mutation; compensation acts only on the exact
    /// version the compensated operation produced.
    pub version: i64,
    /// v1.82 (RM-134 third review): the AI employee (its `agents/<id>`
    /// directory id) a per-agent key acts for. Written once at creation —
    /// only for a per-agent key requested by the employee's exact directory
    /// id, or for a migrated legacy entry whose client id exactly equals an
    /// existing directory id and whose scopes are all per-employee scopes
    /// (fourth review) — and never changed by any mutation. `None` for
    /// every other key. A non-external key without a binding to an existing,
    /// non-deleted employee that is not younger than the key is refused
    /// every tool by the HTTP MCP server ([`key_binding_state`]); the
    /// gateway tombstones the keys of a deleted or removed employee, so they
    /// never work for a later employee with the same id.
    pub agent_binding: Option<String>,
}

impl std::fmt::Debug for StoredKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredKey")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("client_id", &self.client_id)
            .field("is_external", &self.is_external)
            .field("scopes", &self.scopes)
            .field("expires_at", &self.expires_at)
            .field("disabled", &self.disabled_at.is_some())
            .field("deleted", &self.deleted_at.is_some())
            .field("version", &self.version)
            .field("agent_binding", &self.agent_binding)
            .finish_non_exhaustive()
    }
}

impl StoredKey {
    /// State at `now`. A key is expired from the instant `expires_at` is
    /// reached.
    pub fn state(&self, now: DateTime<Utc>) -> KeyState {
        if self.deleted_at.is_some() {
            KeyState::Deleted
        } else if self.disabled_at.is_some() {
            KeyState::Disabled
        } else if self.expires_at.is_some_and(|at| now >= at) {
            KeyState::Expired
        } else {
            KeyState::Active
        }
    }

    /// What an OAuth consent binds to: the row and its authority metadata
    /// (client, external flag, scopes). Name and expiry are not part of it —
    /// renaming a key must not cut its OAuth grants, and expiry is checked
    /// live on every use.
    pub fn consent_fingerprint(&self) -> String {
        digest(&format!(
            "mcp_key_store:v1\n{}\n{}\n{}\n{}",
            self.id,
            self.client_id,
            self.is_external,
            self.scopes.join(" ")
        ))
    }

    /// Stored scopes outside [`crate::mcp_scopes::EXTERNALLY_GRANTABLE_SCOPE_STRINGS`]
    /// for an external key (e.g. a migrated key that carried `odoo:read`):
    /// they are stored but never take effect for an external principal.
    /// The v1.76 employee Google lane is the one exception
    /// ([`crate::mcp_scopes::EMPLOYEE_GOOGLE_KEY_SCOPES`] on a key whose client
    /// is an employee), which the caller passes as `employee_key`.
    pub fn ineffective_scopes(&self, employee_key: bool) -> Vec<String> {
        if !self.is_external {
            return Vec::new();
        }
        self.scopes
            .iter()
            .filter(|s| {
                !crate::mcp_scopes::EXTERNALLY_GRANTABLE_SCOPE_STRINGS.contains(&s.as_str())
                    && !(employee_key && crate::mcp_scopes::EMPLOYEE_GOOGLE_KEY_SCOPES.contains(&s.as_str()))
            })
            .cloned()
            .collect()
    }
}

/// The store exists but could not be opened or read. The message names the
/// operation and the SQLite error; it never contains a key or a digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreError(pub String);

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MCP key store unavailable: {}", self.0)
    }
}

impl std::error::Error for StoreError {}

fn store_error(context: &str, e: impl std::fmt::Display) -> StoreError {
    StoreError(format!("{context}: {e}"))
}

fn conversion(e: impl std::fmt::Display) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(StoreError(e.to_string())),
    )
}

fn required_timestamp(raw: String) -> rusqlite::Result<DateTime<Utc>> {
    parse_timestamp(&raw).ok_or_else(|| conversion("malformed timestamp"))
}

fn optional_timestamp(raw: Option<String>) -> rusqlite::Result<Option<DateTime<Utc>>> {
    raw.map(required_timestamp).transpose()
}

/// Decode one row selected with [`KEY_COLUMNS`]. A malformed timestamp or
/// scope list is an error, never a silently "valid" key.
pub fn key_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredKey> {
    let scopes: String = row.get(7)?;
    let scopes: Vec<String> = serde_json::from_str(&scopes).map_err(conversion)?;
    Ok(StoredKey {
        id: row.get(0)?,
        key_hash: row.get(1)?,
        name: row.get(2)?,
        client_id: row.get(3)?,
        is_external: row.get::<_, i64>(4)? == 1,
        env: row.get(5)?,
        hint: row.get(6)?,
        scopes,
        created_by: row.get(8)?,
        created_at: required_timestamp(row.get(9)?)?,
        expires_at: optional_timestamp(row.get(10)?)?,
        disabled_at: optional_timestamp(row.get(11)?)?,
        disabled_by: row.get(12)?,
        deleted_at: optional_timestamp(row.get(13)?)?,
        migrated: row.get::<_, i64>(14)? == 1,
        expiry_reviewed: row.get::<_, i64>(15)? == 1,
        legacy_entry_hash: row.get(16)?,
        version: row.get(17)?,
        agent_binding: row.get(18)?,
    })
}

// ── Employee binding (RM-134 third review) ───────────────────────────────

/// Where a key's [`StoredKey::agent_binding`] stands right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingState {
    /// `agents/<id>/agent.toml` exists, parses, and its status is not `deleted`.
    Live,
    /// Not a valid agent directory id (or a reserved / internal one).
    Invalid,
    /// No `agents/<id>/agent.toml` (the employee was removed or renamed).
    Missing,
    /// `[agent] status = "deleted"`.
    Deleted,
    /// The file exists but could not be read or parsed (fail closed).
    Unreadable,
    /// v1.82 (RM-134 fourth review, Medium 1): an employee with this id
    /// exists, but its `agents/<id>` directory was created AFTER the key
    /// ([`key_binding_state`]). A key can never legitimately predate the
    /// employee it is bound to, so this is a later employee re-created under
    /// the id of a removed one.
    Recreated,
}

impl BindingState {
    /// Stable token for logs, audit records and the keys page.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Invalid => "binding_invalid",
            Self::Missing => "employee_missing",
            Self::Deleted => "employee_deleted",
            Self::Unreadable => "employee_unreadable",
            Self::Recreated => "employee_recreated",
        }
    }

    /// RM-134 fourth review (Medium 1): the employee the key was bound to is
    /// gone for good — removed, soft-deleted, or the binding is not an
    /// employee id at all. The gateway's key sweep tombstones such keys.
    /// Not [`Self::Unreadable`] (may be transient) and not
    /// [`Self::Recreated`]: that verdict rests on file-system birth times,
    /// which a restored or copied home also resets, so it refuses the key
    /// (and the keys page says why) but never destroys it. Both are refused
    /// meanwhile anyway.
    pub fn employee_gone(self) -> bool {
        matches!(self, Self::Missing | Self::Deleted | Self::Invalid)
    }
}

/// Filesystem timestamp granularity allowed for when comparing an employee
/// directory's birth time with a key's `created_at` (which the store keeps in
/// whole seconds): a directory born up to this many seconds after the key's
/// stored second is not "re-created".
pub const EMPLOYEE_BIRTH_SKEW_SECS: i64 = 2;

/// The birth (creation) time of the `agents/<id>` directory entry itself
/// (not following a symlink), where the platform reports one: macOS
/// (APFS / HFS+), Windows (NTFS / ReFS) and Linux through `statx` on file
/// systems that record it (ext4, xfs, btrfs, tmpfs on kernel 4.11+). `None`
/// elsewhere (e.g. many network file systems, older kernels) — the caller
/// then has no birth-time evidence either way. The directory, not
/// `agent.toml`: the gateway rewrites `agent.toml` by atomic replacement, so
/// that file is "born" again on every edit, while the directory is born once
/// per employee and survives every edit and same-volume rename.
pub fn employee_dir_birth(home: &Path, id: &str) -> Option<DateTime<Utc>> {
    let meta = std::fs::symlink_metadata(home.join("agents").join(id)).ok()?;
    meta.created().ok().map(DateTime::<Utc>::from)
}

/// Whether an employee directory born at `born` was created after a key
/// created at `key_created_at` (beyond [`EMPLOYEE_BIRTH_SKEW_SECS`]). No
/// birth time = no evidence = `false`.
pub fn employee_born_after(born: Option<DateTime<Utc>>, key_created_at: DateTime<Utc>) -> bool {
    born.is_some_and(|born| born > key_created_at + chrono::Duration::seconds(EMPLOYEE_BIRTH_SKEW_SECS))
}

/// [`binding_state`] for one stored key: additionally
/// [`BindingState::Recreated`] when the bound employee's directory was born
/// after the key was created ([`employee_dir_birth`]). Defence in depth
/// behind the gateway's tombstones (a deleted employee's keys are deleted
/// with it, and the key sweep deletes keys whose employee disappeared
/// outside the gateway): it closes the window in which an employee removed
/// outside the gateway is re-created under the same id before the next
/// sweep, wherever the platform reports a directory birth time.
///
/// Known cost: restoring or copying a home re-births every employee
/// directory (birth times are not restorable), so every per-employee key
/// created before the restore reads `Recreated` and must be re-issued (fail
/// closed). Such keys are refused, not tombstoned
/// ([`BindingState::employee_gone`]).
pub fn key_binding_state(home: &Path, binding: &str, key_created_at: DateTime<Utc>) -> BindingState {
    match binding_state(home, binding) {
        BindingState::Live if employee_born_after(employee_dir_birth(home, binding), key_created_at) => {
            BindingState::Recreated
        }
        state => state,
    }
}

/// Largest `agent.toml` [`binding_state`] reads (same cap the gateway's
/// employee listing uses).
const AGENT_TOML_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// Whether `<home>/agents` holds an entry named exactly `id` (byte-equal
/// file name, never a case-folded match). `Err` = `agents/` unreadable.
fn exact_agent_dir_exists(home: &Path, id: &str) -> Result<bool, ()> {
    let entries = match std::fs::read_dir(home.join("agents")) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(()),
    };
    for entry in entries {
        let entry = entry.map_err(|_| ())?;
        if entry.file_name().to_str() == Some(id) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether `binding` names an AI employee that exists right now and is not
/// deleted. Exact directory id only (never a display name, never a prefix or
/// case-folded match). Anything that cannot be confirmed is not [`BindingState::Live`].
pub fn binding_state(home: &Path, binding: &str) -> BindingState {
    if !crate::is_valid_agent_id(binding)
        || binding == crate::UNTRUSTED_AGENT_ID
        || crate::is_internal_agent_dir(binding)
    {
        return BindingState::Invalid;
    }
    // Exact directory name, even on a case-insensitive filesystem (where
    // `agents/ALICE` would otherwise open `agents/alice`).
    match exact_agent_dir_exists(home, binding) {
        Ok(true) => {}
        Ok(false) => return BindingState::Missing,
        Err(()) => return BindingState::Unreadable,
    }
    let path = home.join("agents").join(binding).join("agent.toml");
    let meta = match std::fs::metadata(&path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return BindingState::Missing,
        Err(_) => return BindingState::Unreadable,
    };
    if !meta.is_file() {
        return BindingState::Missing;
    }
    if meta.len() > AGENT_TOML_MAX_BYTES {
        return BindingState::Unreadable;
    }
    let Ok(text) = std::fs::read_to_string(&path) else { return BindingState::Unreadable };
    let Ok(table) = text.parse::<toml::Table>() else { return BindingState::Unreadable };
    match table.get("agent").and_then(|agent| agent.get("status")) {
        None => BindingState::Live,
        Some(toml::Value::String(status)) if status.trim().eq_ignore_ascii_case("deleted") => BindingState::Deleted,
        Some(toml::Value::String(_)) => BindingState::Live,
        Some(_) => BindingState::Unreadable,
    }
}

// ── Schema pinning ────────────────────────────────────────────────────────

type SchemaRow = (String, String, String, Option<String>);

fn schema_rows(conn: &Connection) -> rusqlite::Result<Vec<SchemaRow>> {
    let mut stmt = conn.prepare("SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY type, name, tbl_name, sql")?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
    rows.collect()
}

fn expected(sql: &'static str, cell: &'static OnceLock<Vec<SchemaRow>>) -> &'static [SchemaRow] {
    cell.get_or_init(|| {
        // Built from our own constant; failure would be a programming error
        // and yields an empty schema that no real store matches (deny).
        Connection::open_in_memory()
            .and_then(|conn| conn.execute_batch(sql).map(|()| conn))
            .and_then(|conn| schema_rows(&conn))
            .unwrap_or_default()
    })
}

fn verify(conn: &Connection, sql: &'static str, cell: &'static OnceLock<Vec<SchemaRow>>, what: &str) -> Result<(), StoreError> {
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| store_error("read user_version", e))?;
    if version != SCHEMA_VERSION {
        return Err(StoreError(format!("{what}: unexpected user_version {version}")));
    }
    let actual = schema_rows(conn).map_err(|e| store_error("read schema", e))?;
    let expected = expected(sql, cell);
    if expected.is_empty() || actual != expected {
        return Err(StoreError(format!("{what}: unexpected schema")));
    }
    Ok(())
}

static AUTHORITY_SCHEMA: OnceLock<Vec<SchemaRow>> = OnceLock::new();
static USAGE_SCHEMA: OnceLock<Vec<SchemaRow>> = OnceLock::new();

/// The authority store carries exactly [`SCHEMA_SQL`] and
/// [`SCHEMA_VERSION`]. A missing table, an extra object (table, index,
/// trigger, view) or any other `user_version` is an error.
pub fn verify_schema(conn: &Connection) -> Result<(), StoreError> {
    verify(conn, SCHEMA_SQL, &AUTHORITY_SCHEMA, "mcp_keys.db")
}

fn private_open(home: &Path, name: &str, access: Access) -> Result<Option<Connection>, StoreError> {
    let conn = private_sqlite::open(home, name, access).map_err(|e| store_error("open", e))?;
    if let Some(conn) = &conn {
        conn.busy_timeout(Duration::from_secs(2)).map_err(|e| store_error("busy timeout", e))?;
    }
    Ok(conn)
}

/// Open the authority store strictly read-only (no create, `SQLITE_OPEN_READ_ONLY`)
/// and verify its schema. `Ok(None)` = the file does not exist (no store yet).
pub fn open_read_only(home: &Path) -> Result<Option<Connection>, StoreError> {
    let Some(conn) = private_open(home, STORE_FILE, Access::ReadOnly)? else { return Ok(None) };
    verify_schema(&conn)?;
    Ok(Some(conn))
}

/// The gateway's read-write open: creates the store (owner-only) when it is
/// missing and applies [`SCHEMA_SQL`] to a brand-new, empty file inside one
/// transaction; any existing file must already carry the pinned schema.
/// Only `duduclaw_auth::mcp_keys` (the gateway) and tests call this.
pub fn open_for_gateway(home: &Path) -> Result<Connection, StoreError> {
    let mut conn = private_open(home, STORE_FILE, Access::Create)?
        .ok_or_else(|| StoreError("create mcp_keys.db".into()))?;
    conn.busy_timeout(Duration::from_secs(5)).map_err(|e| store_error("busy timeout", e))?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| store_error("begin", e))?;
    let version: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0)).map_err(|e| store_error("read user_version", e))?;
    let objects: i64 = tx
        .query_row("SELECT count(*) FROM sqlite_schema", [], |r| r.get(0))
        .map_err(|e| store_error("read schema", e))?;
    if version == 0 && objects == 0 {
        tx.execute_batch(SCHEMA_SQL).map_err(|e| store_error("apply schema", e))?;
    }
    verify_schema(&tx)?;
    tx.commit().map_err(|e| store_error("commit", e))?;
    Ok(conn)
}

// ── Migration state and the legacy decision ──────────────────────────────

/// Where the `[mcp_keys]` migration stands, from the gateway's own records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationState {
    /// The store exists but the gateway has not recorded the pending set yet.
    NotStarted,
    /// The pending set is recorded; entries in it may still be read from
    /// `config.toml` until they are moved.
    Pending,
    /// Every legacy entry was imported or tombstoned. `config.toml` is never
    /// consulted for a non-internal key again.
    Complete,
}

/// Read the migration state from an open authority store.
pub fn migration_state(conn: &Connection) -> Result<MigrationState, StoreError> {
    let value: Option<String> = conn
        .query_row("SELECT value FROM mcp_api_key_meta WHERE key = ?1", [META_LEGACY_MIGRATION], |r| r.get(0))
        .optional()
        .map_err(|e| store_error("read migration state", e))?;
    Ok(match value.as_deref() {
        None => MigrationState::NotStarted,
        Some(v) if v.starts_with("complete:") => MigrationState::Complete,
        Some(v) if v.starts_with("pending:") => MigrationState::Pending,
        Some(_) => return Err(StoreError("unexpected migration state".into())),
    })
}

/// Why a `[mcp_keys]` entry may still be read from `config.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyWindow {
    /// `mcp_keys.db` does not exist: no v1.82 gateway has started on this
    /// home (e.g. a stdio `mcp-server` launched before the upgraded gateway
    /// ever ran). The one bootstrap window: the whole `[mcp_keys]` table is
    /// read under the pre-v1.82 rules. It closes for good the moment the
    /// gateway creates the store.
    Bootstrap,
    /// The gateway recorded this fingerprint as "pending legacy" (from
    /// `config.toml` as it read it when it started the migration) and has
    /// not finished moving it.
    Pending,
}

/// The gateway's own internal key, as recorded at provisioning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InternalKey {
    pub created_at: DateTime<Utc>,
}

/// What the authority store says about a presented key's fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Authority {
    /// A dashboard key. Its row decides alone (any `config.toml` copy is
    /// ignored); the caller checks [`StoredKey::state`].
    Stored(StoredKey),
    /// The gateway's own `gateway-internal` key: accepted only as that
    /// client, only with its `config.toml` entry present and inside the
    /// 30-day rule (both the entry's and the recorded `created_at`).
    Internal(InternalKey),
    /// The `config.toml` entry may be read under the pre-v1.82 rules.
    Legacy(LegacyWindow),
    /// Not a key: deny, whatever `config.toml` says.
    Unknown,
}

/// Decide a fingerprint. The order is fixed:
///
/// 1. no `mcp_keys.db` file → [`LegacyWindow::Bootstrap`];
/// 2. an existing store that cannot be read, or has an unexpected schema →
///    `Err` (deny);
/// 3. a key row with this digest → [`Authority::Stored`] (an indexed equality
///    lookup on the digest, then a constant-time comparison of the matched
///    row's digest);
/// 4. a recorded internal fingerprint → [`Authority::Internal`];
/// 5. migration [`MigrationState::Pending`] and the digest in the recorded
///    pending set → [`LegacyWindow::Pending`];
/// 6. anything else → [`Authority::Unknown`] — in particular every
///    fingerprint once the migration is complete, a config entry added after
///    the pending set was recorded, and an entry that merely claims
///    `client_id = "gateway-internal"`.
pub fn resolve_hash(home: &Path, hash: &str) -> Result<Authority, StoreError> {
    if !is_digest(hash) {
        return Ok(Authority::Unknown);
    }
    let Some(conn) = open_read_only(home)? else { return Ok(Authority::Legacy(LegacyWindow::Bootstrap)) };
    resolve_with(&conn, hash)
}

/// [`resolve_hash`] for a presented key.
pub fn resolve(home: &Path, raw: &str) -> Result<Authority, StoreError> {
    resolve_hash(home, &digest(raw))
}

fn resolve_with(conn: &Connection, hash: &str) -> Result<Authority, StoreError> {
    let found = conn
        .query_row(&format!("SELECT {KEY_COLUMNS} FROM mcp_api_keys WHERE key_hash = ?1"), [hash], key_from_row)
        .optional()
        .map_err(|e| store_error("lookup", e))?;
    if let Some(key) = found {
        return Ok(if constant_time_eq(key.key_hash.as_bytes(), hash.as_bytes()) {
            Authority::Stored(key)
        } else {
            Authority::Unknown
        });
    }
    let internal: Option<String> = conn
        .query_row("SELECT created_at FROM mcp_internal_keys WHERE key_hash = ?1", [hash], |r| r.get(0))
        .optional()
        .map_err(|e| store_error("lookup internal", e))?;
    if let Some(created) = internal {
        let created_at = parse_timestamp(&created).ok_or_else(|| StoreError("malformed internal key record".into()))?;
        return Ok(Authority::Internal(InternalKey { created_at }));
    }
    if migration_state(conn)? != MigrationState::Pending {
        return Ok(Authority::Unknown);
    }
    let pending: bool = conn
        .query_row("SELECT EXISTS(SELECT 1 FROM mcp_legacy_pending WHERE key_hash = ?1)", [hash], |r| r.get(0))
        .map_err(|e| store_error("lookup pending", e))?;
    Ok(if pending { Authority::Legacy(LegacyWindow::Pending) } else { Authority::Unknown })
}

/// Look a key up by its digest in the key table. `Ok(None)` = not a stored
/// dashboard key (see [`resolve_hash`] for what else it may be); `Err` = the
/// store exists and could not be read (deny).
pub fn lookup_hash(home: &Path, hash: &str) -> Result<Option<StoredKey>, StoreError> {
    match resolve_hash(home, hash)? {
        Authority::Stored(key) => Ok(Some(key)),
        _ => Ok(None),
    }
}

/// [`lookup_hash`] for a presented key (the key itself never reaches SQL).
pub fn lookup(home: &Path, raw: &str) -> Result<Option<StoredKey>, StoreError> {
    lookup_hash(home, &digest(raw))
}

/// The recorded fingerprints of the gateway-internal key. `Ok(None)` = no
/// store yet (the bootstrap window).
pub fn internal_key_hashes(home: &Path) -> Result<Option<HashSet<String>>, StoreError> {
    let Some(conn) = open_read_only(home)? else { return Ok(None) };
    let mut stmt = conn.prepare("SELECT key_hash FROM mcp_internal_keys").map_err(|e| store_error("list internal", e))?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0)).map_err(|e| store_error("list internal", e))?;
    rows.collect::<Result<HashSet<_>, _>>().map(Some).map_err(|e| store_error("list internal", e))
}

/// Every key usable at `now` (not deleted, not disabled, not expired).
pub fn usable_keys(home: &Path, now: DateTime<Utc>) -> Result<Vec<StoredKey>, StoreError> {
    let Some(conn) = open_read_only(home)? else { return Ok(Vec::new()) };
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {KEY_COLUMNS} FROM mcp_api_keys
              WHERE deleted_at IS NULL AND disabled_at IS NULL ORDER BY created_at, id"
        ))
        .map_err(|e| store_error("list", e))?;
    let rows = stmt.query_map([], key_from_row).map_err(|e| store_error("list", e))?;
    let keys = rows.collect::<Result<Vec<_>, _>>().map_err(|e| store_error("list", e))?;
    Ok(keys.into_iter().filter(|k| k.state(now) == KeyState::Active).collect())
}

/// Whether any key exists (a non-deleted dashboard key or a recorded
/// internal key). Used by the explicit unauthenticated opt-in
/// (`DUDUCLAW_MCP_ALLOW_UNAUTHENTICATED`), which only applies when no key is
/// configured anywhere.
pub fn has_keys(home: &Path) -> Result<bool, StoreError> {
    let Some(conn) = open_read_only(home)? else { return Ok(false) };
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM mcp_api_keys WHERE deleted_at IS NULL)
             OR EXISTS(SELECT 1 FROM mcp_internal_keys)",
        [],
        |r| r.get::<_, bool>(0),
    )
    .map_err(|e| store_error("count", e))
}

// ── Usage telemetry (`mcp_keys_usage.db`) ─────────────────────────────────

/// Last stamp per (home, key id) in this process — no open at all inside the
/// throttle window.
fn stamps() -> &'static Mutex<HashMap<(PathBuf, String), DateTime<Utc>>> {
    static STAMPS: OnceLock<Mutex<HashMap<(PathBuf, String), DateTime<Utc>>>> = OnceLock::new();
    STAMPS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn open_usage(home: &Path, access: Access) -> Result<Option<Connection>, StoreError> {
    let Some(conn) = private_open(home, USAGE_FILE, access)? else { return Ok(None) };
    if access == Access::Create {
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).map_err(|e| store_error("usage version", e))?;
        let objects: i64 = conn
            .query_row("SELECT count(*) FROM sqlite_schema", [], |r| r.get(0))
            .map_err(|e| store_error("usage schema", e))?;
        if version == 0 && objects == 0 {
            conn.execute_batch(&format!("BEGIN IMMEDIATE; {USAGE_SCHEMA_SQL} COMMIT;"))
                .map_err(|e| store_error("usage schema", e))?;
        }
    }
    verify(&conn, USAGE_SCHEMA_SQL, &USAGE_SCHEMA, "mcp_keys_usage.db")?;
    Ok(Some(conn))
}

/// Record that `key_id` authenticated at `now`, in the usage file (created
/// owner-only when missing). At most once per key per
/// [`LAST_USED_THROTTLE_SECS`]: no open at all when this process stamped the
/// key inside that window, and the conditional upsert lets only one of
/// several racing processes win a given minute. Returns whether a row was
/// written. Never touches the authority store; a failure must never deny the
/// call it records.
pub fn record_usage(home: &Path, key_id: &str, now: DateTime<Utc>) -> Result<bool, StoreError> {
    let slot = (home.to_path_buf(), key_id.to_owned());
    {
        let mut map = stamps().lock().unwrap_or_else(|e| e.into_inner());
        if map.get(&slot).is_some_and(|at| now.signed_duration_since(*at).num_seconds() < LAST_USED_THROTTLE_SECS) {
            return Ok(false);
        }
        if map.len() > 4096 {
            map.clear();
        }
        // Stamped before the write: a failing file is retried a minute later,
        // not on every call.
        map.insert(slot, now);
    }
    let conn = open_usage(home, Access::Create)?.ok_or_else(|| StoreError("create usage file".into()))?;
    let cutoff = timestamp(now - chrono::Duration::seconds(LAST_USED_THROTTLE_SECS));
    conn.execute(
        "INSERT INTO mcp_key_usage(key_id, last_used_at) VALUES (?1, ?2)
         ON CONFLICT(key_id) DO UPDATE SET last_used_at = excluded.last_used_at
          WHERE mcp_key_usage.last_used_at <= ?3",
        params![key_id, timestamp(now), cutoff],
    )
    .map(|changed| changed > 0)
    .map_err(|e| store_error("record usage", e))
}

/// `key id → last use` from the usage file, for display only. A missing
/// file is empty. Rows with an unparseable time are skipped.
pub fn usage_map(home: &Path) -> Result<HashMap<String, DateTime<Utc>>, StoreError> {
    let Some(conn) = open_usage(home, Access::ReadOnly)? else { return Ok(HashMap::new()) };
    let mut stmt = conn
        .prepare("SELECT key_id, last_used_at FROM mcp_key_usage")
        .map_err(|e| store_error("read usage", e))?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| store_error("read usage", e))?;
    let mut out = HashMap::new();
    for row in rows {
        let (id, at) = row.map_err(|e| store_error("read usage", e))?;
        if let Some(at) = parse_timestamp(&at) {
            out.insert(id, at);
        }
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// A store created the way the gateway creates it.
    pub(crate) fn store(home: &Path) -> Connection {
        open_for_gateway(home).unwrap()
    }

    pub(crate) fn insert(
        conn: &Connection,
        id: &str,
        raw: &str,
        client: &str,
        external: bool,
        expires: Option<DateTime<Utc>>,
        legacy_entry_hash: Option<String>,
    ) {
        conn.execute(
            "INSERT INTO mcp_api_keys(id, key_hash, name, client_id, is_external, env, hint, scopes, created_at, expires_at, legacy_entry_hash)
             VALUES (?1, ?2, ?1, ?3, ?4, 'prod', ?5, '[\"memory:read\"]', ?6, ?7, ?8)",
            params![id, digest(raw), client, external as i64, key_hint(raw), timestamp(Utc::now()), expires.map(timestamp), legacy_entry_hash],
        )
        .unwrap();
    }

    pub(crate) fn set_meta(conn: &Connection, value: &str) {
        conn.execute(
            "INSERT INTO mcp_api_key_meta(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![META_LEGACY_MIGRATION, value],
        )
        .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    const KEY: &str = "ddc_prod_0123456789abcdef0123456789abcdef";

    #[test]
    fn key_shape_hint_and_env() {
        assert!(is_key_format(KEY));
        assert!(!is_key_format("ddc_prod_0123456789ABCDEF0123456789abcdef"));
        assert!(!is_key_format("ddc_prodx_0123456789abcdef0123456789abcdef"));
        assert!(!is_key_format(&format!("{KEY}0")));
        assert_eq!(key_env(KEY), Some("prod"));
        assert_eq!(key_env("ddc_staging_0123456789abcdef0123456789abcdef"), Some("staging"));
        assert_eq!(key_hint(KEY), "ddc_prod_…cdef");
        assert!(!key_hint(KEY).contains("0123456789abcdef0123"));
    }

    #[test]
    fn only_a_missing_file_is_no_store_and_every_damaged_store_is_an_error() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(resolve(home.path(), KEY).unwrap(), Authority::Legacy(LegacyWindow::Bootstrap));
        assert_eq!(lookup(home.path(), KEY).unwrap(), None);
        assert!(!has_keys(home.path()).unwrap());
        assert!(usable_keys(home.path(), Utc::now()).unwrap().is_empty());
        // Garbage bytes (owner-only, so only the content is wrong).
        let path = store_path(home.path());
        let write_private = |bytes: &[u8]| {
            let _ = std::fs::remove_file(&path);
            drop(private_sqlite::open(home.path(), STORE_FILE, Access::Create).unwrap());
            std::fs::write(&path, bytes).unwrap();
        };
        write_private(b"not a database, definitely not");
        assert!(resolve(home.path(), KEY).is_err(), "unreadable store must deny");
        assert!(has_keys(home.path()).is_err());
        // An empty file (created, schema never applied) is not "no store".
        write_private(b"");
        assert!(resolve(home.path(), KEY).is_err(), "a database without the table must deny");
        // A database with other tables only.
        write_private(b"");
        Connection::open(&path).unwrap().execute_batch("CREATE TABLE other(x)").unwrap();
        assert!(resolve(home.path(), KEY).is_err());
        // The right tables with a wrong user_version, or an extra object.
        write_private(b"");
        Connection::open(&path).unwrap().execute_batch(&SCHEMA_SQL.replace("user_version = 1", "user_version = 2")).unwrap();
        assert!(resolve(home.path(), KEY).is_err(), "wrong user_version");
        write_private(b"");
        Connection::open(&path).unwrap().execute_batch(&format!("{SCHEMA_SQL} CREATE TABLE planted(x);")).unwrap();
        assert!(resolve(home.path(), KEY).is_err(), "unexpected schema");
        write_private(b"");
        Connection::open(&path)
            .unwrap()
            .execute_batch(&format!("{SCHEMA_SQL} CREATE TRIGGER t AFTER INSERT ON mcp_api_keys BEGIN SELECT 1; END;"))
            .unwrap();
        assert!(resolve(home.path(), KEY).is_err(), "planted trigger");
        // The gateway's own open refuses them as well, without repairing.
        assert!(open_for_gateway(home.path()).is_err());
        // A fresh store is fine.
        std::fs::remove_file(&path).unwrap();
        store(home.path());
        assert_eq!(resolve(home.path(), KEY).unwrap(), Authority::Unknown, "store exists, migration not started");
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_or_shared_store_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let target = home.path().join("elsewhere.db");
        store(home.path());
        std::fs::rename(store_path(home.path()), &target).unwrap();
        std::os::unix::fs::symlink(&target, store_path(home.path())).unwrap();
        assert!(resolve(home.path(), KEY).is_err());
        std::fs::remove_file(store_path(home.path())).unwrap();
        std::fs::rename(&target, store_path(home.path())).unwrap();
        assert!(resolve(home.path(), KEY).is_ok());
        std::fs::set_permissions(store_path(home.path()), std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(resolve(home.path(), KEY).is_err(), "group/world-readable store is refused");
    }

    #[test]
    fn states_follow_markers_and_expiry_instant() {
        let home = tempfile::tempdir().unwrap();
        let conn = store(home.path());
        let at = Utc::now() + chrono::Duration::hours(1);
        insert(&conn, "mk_1", KEY, "ci-bot", true, Some(at), None);
        let key = lookup(home.path(), KEY).unwrap().unwrap();
        assert_eq!(key.version, 1);
        assert_eq!(key.state(at - chrono::Duration::seconds(1)), KeyState::Active);
        assert_eq!(key.state(at), KeyState::Expired);
        conn.execute("UPDATE mcp_api_keys SET disabled_at = ?1", [timestamp(Utc::now())]).unwrap();
        assert_eq!(lookup(home.path(), KEY).unwrap().unwrap().state(at), KeyState::Disabled);
        conn.execute("UPDATE mcp_api_keys SET deleted_at = ?1", [timestamp(Utc::now())]).unwrap();
        assert_eq!(lookup(home.path(), KEY).unwrap().unwrap().state(Utc::now()), KeyState::Deleted);
        assert!(usable_keys(home.path(), Utc::now()).unwrap().is_empty());
        assert!(!format!("{key:?}").contains(&digest(KEY)), "Debug never prints the digest");
    }

    #[test]
    fn lookup_needs_the_exact_key_and_a_malformed_row_is_an_error() {
        let home = tempfile::tempdir().unwrap();
        let conn = store(home.path());
        insert(&conn, "mk_1", KEY, "ci-bot", true, None, None);
        assert!(lookup(home.path(), KEY).unwrap().is_some());
        assert_eq!(lookup(home.path(), "ddc_prod_0123456789abcdef0123456789abcdee").unwrap(), None);
        assert_eq!(resolve_hash(home.path(), "not-a-digest").unwrap(), Authority::Unknown);
        assert_eq!(resolve_hash(home.path(), &digest(KEY).to_uppercase()).unwrap(), Authority::Unknown);
        conn.execute("UPDATE mcp_api_keys SET expires_at = 'tomorrow-ish'", []).unwrap();
        assert!(lookup(home.path(), KEY).is_err(), "an undatable expiry never reads as no-expiry");
        conn.execute("UPDATE mcp_api_keys SET expires_at = NULL, scopes = 'memory:read'", []).unwrap();
        assert!(lookup(home.path(), KEY).is_err());
    }

    #[test]
    fn legacy_window_internal_and_marker_rules() {
        let home = tempfile::tempdir().unwrap();
        let conn = store(home.path());
        let pending = "ddc_prod_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let internal = "ddc_prod_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let late = "ddc_prod_cccccccccccccccccccccccccccccccc";
        // The store exists but no pending set: nothing from config.
        assert_eq!(resolve(home.path(), pending).unwrap(), Authority::Unknown);
        conn.execute(
            "INSERT INTO mcp_internal_keys(key_hash, created_at) VALUES (?1, ?2)",
            params![digest(internal), timestamp(Utc::now())],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO mcp_legacy_pending(key_hash, recorded_at) VALUES (?1, ?2)",
            params![digest(pending), timestamp(Utc::now())],
        )
        .unwrap();
        set_meta(&conn, "pending:2026-11-13T00:00:00Z");
        assert_eq!(resolve(home.path(), pending).unwrap(), Authority::Legacy(LegacyWindow::Pending));
        assert_eq!(resolve(home.path(), late).unwrap(), Authority::Unknown, "added after the pending set");
        assert!(matches!(resolve(home.path(), internal).unwrap(), Authority::Internal(_)));
        assert_eq!(internal_key_hashes(home.path()).unwrap().unwrap(), HashSet::from([digest(internal)]));
        set_meta(&conn, "complete:2026-11-13T00:00:00Z");
        assert_eq!(resolve(home.path(), pending).unwrap(), Authority::Unknown, "never config after the marker");
        assert!(matches!(resolve(home.path(), internal).unwrap(), Authority::Internal(_)));
        assert!(has_keys(home.path()).unwrap(), "a recorded internal key counts");
        set_meta(&conn, "garbage");
        assert!(resolve(home.path(), pending).is_err());
    }

    #[test]
    fn legacy_rule_boundary_and_migrated_expiry_agree() {
        let created = parse_timestamp("2026-10-01T08:00:00Z").unwrap();
        let ends = legacy_expires_at(created);
        assert_eq!(timestamp(ends), "2026-11-01T08:00:00Z");
        assert!(legacy_age_accepts(created, ends - chrono::Duration::seconds(1)));
        assert!(legacy_age_accepts(created, ends - chrono::Duration::nanoseconds(1)));
        assert!(!legacy_age_accepts(created, ends));
        // A fractional created_at: the store's whole second is never later
        // than the old refusal instant.
        let fractional = created + chrono::Duration::milliseconds(700);
        let ends = legacy_expires_at(fractional);
        assert!(ends <= fractional + chrono::Duration::days(31));
        assert!(fractional + chrono::Duration::days(31) - ends < chrono::Duration::seconds(1));
        assert!(legacy_age_accepts(fractional, ends - chrono::Duration::seconds(1)));
        // Third review (Medium): a created_at within the clock-skew allowance
        // is age 0; further in the future it is not credible and refused.
        assert!(legacy_age_accepts(created + chrono::Duration::seconds(LEGACY_FUTURE_SKEW_SECS), created));
        assert!(!legacy_age_accepts(created + chrono::Duration::seconds(LEGACY_FUTURE_SKEW_SECS + 1), created));
        assert!(!legacy_age_accepts(created + chrono::Duration::days(5), created));
        assert!(!legacy_created_at_credible(created + chrono::Duration::days(3650), created));
    }

    #[test]
    fn binding_state_needs_an_existing_non_deleted_employee_by_exact_id() {
        let home = tempfile::tempdir().unwrap();
        let write = |id: &str, body: &str| {
            let dir = home.path().join("agents").join(id);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("agent.toml"), body).unwrap();
        };
        write("alice", "[agent]\nname = \"Alice Chen\"\nstatus = \"active\"\n");
        write("bob", "[agent]\nname = \"Bob\"\nstatus = \"deleted\"\n");
        write("carol", "[agent\nbroken");
        write("dave", "[agent]\nname = \"Dave\"\n");
        assert_eq!(binding_state(home.path(), "alice"), BindingState::Live);
        assert_eq!(binding_state(home.path(), "dave"), BindingState::Live, "no status = active");
        assert_eq!(binding_state(home.path(), "bob"), BindingState::Deleted);
        assert_eq!(binding_state(home.path(), "carol"), BindingState::Unreadable);
        assert_eq!(binding_state(home.path(), "erin"), BindingState::Missing);
        for invalid in ["Alice Chen", "ALICE", "", "../alice", crate::UNTRUSTED_AGENT_ID] {
            assert_ne!(binding_state(home.path(), invalid), BindingState::Live, "{invalid:?}");
        }
        std::fs::remove_dir_all(home.path().join("agents/alice")).unwrap();
        assert_eq!(binding_state(home.path(), "alice"), BindingState::Missing, "removed employee");
    }

    /// Fourth review (Medium 1): a key can never predate the employee it is
    /// bound to. Where the platform reports a directory birth time, an
    /// employee directory born after the key reads `Recreated` (not Live);
    /// a key created after the directory stays Live, and editing
    /// `agent.toml` (atomic replacement) does not change that.
    #[test]
    fn a_key_older_than_its_employee_directory_is_refused_as_recreated() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("agents").join("alice");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), "[agent]\nname = \"Alice\"\n").unwrap();
        let now = Utc::now();
        // A key issued after the directory exists (stored in whole seconds).
        assert_eq!(key_binding_state(home.path(), "alice", whole_second(now)), BindingState::Live);
        // agent.toml rewritten by replacement: the directory's birth is kept.
        let tmp = dir.join(".agent.toml.tmp");
        std::fs::write(&tmp, "[agent]\nname = \"Alice v2\"\n").unwrap();
        std::fs::rename(&tmp, dir.join("agent.toml")).unwrap();
        assert_eq!(key_binding_state(home.path(), "alice", whole_second(now)), BindingState::Live);
        // An old key against this (younger) directory.
        let old_key = now - chrono::Duration::days(90);
        let expected = if employee_dir_birth(home.path(), "alice").is_some() {
            BindingState::Recreated
        } else {
            // No birth time on this platform / file system: no evidence; the
            // gateway's tombstones are the guarantee there.
            BindingState::Live
        };
        assert_eq!(key_binding_state(home.path(), "alice", old_key), expected);
        // Refused, but never tombstoned on birth-time evidence alone.
        assert!(!BindingState::Recreated.employee_gone());
        assert_eq!(BindingState::Recreated.as_str(), "employee_recreated");
        // The other states pass through unchanged.
        assert_eq!(key_binding_state(home.path(), "erin", old_key), BindingState::Missing);
        // Pure comparison: granularity allowance, and no evidence = not after.
        assert!(!employee_born_after(None, old_key));
        assert!(!employee_born_after(Some(now + chrono::Duration::seconds(EMPLOYEE_BIRTH_SKEW_SECS)), now));
        assert!(employee_born_after(Some(now + chrono::Duration::seconds(EMPLOYEE_BIRTH_SKEW_SECS + 1)), now));
        for gone in [BindingState::Missing, BindingState::Deleted, BindingState::Invalid] {
            assert!(gone.employee_gone(), "{gone:?}");
        }
        assert!(!BindingState::Unreadable.employee_gone() && !BindingState::Live.employee_gone());
    }

    #[test]
    fn a_stored_binding_is_read_back() {
        let home = tempfile::tempdir().unwrap();
        let conn = store(home.path());
        insert(&conn, "mk_1", KEY, "alice", false, None, None);
        assert_eq!(lookup(home.path(), KEY).unwrap().unwrap().agent_binding, None);
        conn.execute("UPDATE mcp_api_keys SET agent_binding = 'alice'", []).unwrap();
        assert_eq!(lookup(home.path(), KEY).unwrap().unwrap().agent_binding.as_deref(), Some("alice"));
    }

    #[test]
    fn readers_cannot_write_the_authority_store() {
        let home = tempfile::tempdir().unwrap();
        let conn = store(home.path());
        insert(&conn, "mk_1", KEY, "ci-bot", true, None, None);
        drop(conn);
        let ro = open_read_only(home.path()).unwrap().unwrap();
        assert!(ro.execute("UPDATE mcp_api_keys SET name = 'x'", []).is_err());
        assert!(ro.execute("DELETE FROM mcp_api_keys", []).is_err());
        assert_eq!(lookup(home.path(), KEY).unwrap().unwrap().name, "mk_1");
    }

    #[test]
    fn usage_goes_to_its_own_file_at_most_once_a_minute() {
        let home = tempfile::tempdir().unwrap();
        let conn = store(home.path());
        insert(&conn, "mk_1", KEY, "ci-bot", true, None, None);
        drop(conn);
        let before = std::fs::read(store_path(home.path())).unwrap();
        let t0 = Utc::now();
        assert!(record_usage(home.path(), "mk_1", t0).unwrap());
        for s in [0, 1, 30, 59] {
            assert!(!record_usage(home.path(), "mk_1", t0 + chrono::Duration::seconds(s)).unwrap());
        }
        assert!(record_usage(home.path(), "mk_1", t0 + chrono::Duration::seconds(61)).unwrap());
        assert_eq!(timestamp(usage_map(home.path()).unwrap()["mk_1"]), timestamp(t0 + chrono::Duration::seconds(61)));
        assert_eq!(std::fs::read(store_path(home.path())).unwrap(), before, "the authority store is untouched");
        // Another process (fresh throttle) still loses a minute already taken.
        stamps().lock().unwrap().clear();
        assert!(!record_usage(home.path(), "mk_1", t0 + chrono::Duration::seconds(90)).unwrap());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(usage_path(home.path())).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn a_tampered_usage_file_changes_nothing_about_authority() {
        let home = tempfile::tempdir().unwrap();
        let conn = store(home.path());
        insert(&conn, "mk_1", KEY, "ci-bot", true, None, None);
        drop(conn);
        record_usage(home.path(), "mk_1", Utc::now()).unwrap();
        let usage = Connection::open(usage_path(home.path())).unwrap();
        usage.execute("UPDATE mcp_key_usage SET last_used_at = 'never'", []).unwrap();
        usage.execute("INSERT INTO mcp_key_usage VALUES ('mk_forged', '2026-01-01T00:00:00Z')", []).unwrap();
        drop(usage);
        let key = lookup(home.path(), KEY).unwrap().unwrap();
        assert_eq!(key.state(Utc::now()), KeyState::Active);
        assert!(!usage_map(home.path()).unwrap().contains_key("mk_1"), "unparseable rows are skipped");
        std::fs::write(usage_path(home.path()), b"garbage, not sqlite").unwrap();
        assert!(usage_map(home.path()).is_err());
        assert_eq!(lookup(home.path(), KEY).unwrap().unwrap().state(Utc::now()), KeyState::Active);
    }
}
