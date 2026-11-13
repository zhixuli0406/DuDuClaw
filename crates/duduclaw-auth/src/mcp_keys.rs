//! RM-134 (v1.82): management side of the hashed MCP API key store.
//!
//! The store file is `<home>/mcp_keys.db`, beside `users.db` (which holds the
//! RM-064 `status_tokens` this module is modelled on). It is a file of its
//! own so the importer can pin its schema exactly
//! (`crates/duduclaw-import/src/mcp-keys-v182.sql`) and so the separate
//! `mcp-server` / `http-server` processes can read it — strictly read-only —
//! without opening the identity database. The schema, the hardened open and
//! the read path every authenticator uses are `duduclaw_core::mcp_key_store`;
//! this module adds what only the gateway does:
//!
//! - issuing a key: the plaintext (`ddc_<env>_<32 hex>`, 128 random bits) is
//!   returned once in [`CreatedMcpKey`] (no `Debug` / `Serialize`) and only
//!   its SHA-256 is stored. An integration key is always EXTERNAL; the only
//!   internal key issued is a per-agent key ([`KeyBinding::Employee`]);
//! - Admin management by key id — rename, change expiry, disable / enable,
//!   delete (a tombstone: the digest stays so no stale copy can come back).
//!   Every mutation is `UPDATE … WHERE id = ? AND version = ?` and increments
//!   the row version; [`McpKeyStore::revert`] undoes exactly the columns one
//!   operation changed, and only while the row is still at the version that
//!   operation produced;
//! - the `[mcp_keys]` migration bookkeeping (pending set, completion marker,
//!   ignored entries) and [`McpKeyStore::import_legacy`];
//! - the gateway-internal key fingerprints (`mcp_internal_keys`);
//! - the expiry notices (claim → attempt → delivered / retry with backoff)
//!   and the one-time "review migrated expiry" hint.
//!
//! Callers write the security audit record (`duduclaw_security::audit`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Duration, Utc};
use duduclaw_core::mcp_key_store::{self as ks, KEY_COLUMNS, KeyState, MigrationState, StoredKey};
use ring::rand::{SecureRandom, SystemRandom};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

/// The expiry presets the dashboard offers (days from creation / change).
pub const EXPIRY_PRESET_DAYS: [i64; 3] = [30, 90, 365];
/// Furthest custom expiry accepted (days from now).
pub const MAX_CUSTOM_EXPIRY_DAYS: i64 = 3650;
/// Longest client id (the principal name the MCP server sees).
pub const CLIENT_ID_MAX_CHARS: usize = 128;
/// `mcp_api_key_meta` row holding the migration hint state.
const MIGRATION_HINT: &str = "migration_hint";
/// How long a claimed expiry notice is held by the sweep that claimed it.
pub const NOTICE_CLAIM_LEASE_SECS: i64 = 900;
/// First retry delay of a failed expiry notice; doubles per attempt.
pub const NOTICE_RETRY_BASE_SECS: i64 = 600;
/// Longest retry delay of a failed expiry notice.
pub const NOTICE_RETRY_MAX_SECS: i64 = 6 * 3600;

/// The Admin's expiry choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expiry {
    /// One of [`EXPIRY_PRESET_DAYS`].
    Days(i64),
    /// A custom instant, in the future and within [`MAX_CUSTOM_EXPIRY_DAYS`].
    At(DateTime<Utc>),
    /// No expiry. The RPC layer requires an explicit confirmation for this.
    Never,
}

impl Expiry {
    /// The `expires_at` this choice means at `now` (whole seconds).
    pub fn resolve(self, now: DateTime<Utc>) -> Result<Option<DateTime<Utc>>, McpKeyError> {
        let at = match self {
            Self::Days(days) if EXPIRY_PRESET_DAYS.contains(&days) => now + Duration::days(days),
            Self::At(at) if at > now && at <= now + Duration::days(MAX_CUSTOM_EXPIRY_DAYS) => at,
            Self::Never => return Ok(None),
            _ => return Err(McpKeyError::InvalidExpiry),
        };
        Ok(Some(ks::whole_second(at)))
    }
}

/// Who a new key is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyBinding {
    /// An outside integration: an EXTERNAL key, scopes from
    /// `duduclaw_core::mcp_scopes::EXTERNALLY_GRANTABLE_SCOPE_STRINGS`.
    Integration,
    /// The per-agent key of one AI employee (its directory id is the client
    /// id): an INTERNAL key that acts as that employee (RFC-21 per-agent
    /// keys, the v1.76 dedicated Google key), scopes from
    /// `duduclaw_core::mcp_scopes::EMPLOYEE_KEY_SCOPE_STRINGS` (never
    /// `admin`). The client id must be a valid agent directory id
    /// (`duduclaw_core::is_valid_agent_id`, exact — never an `[agent] name`
    /// or display name) and is stored as the key's immutable
    /// `agent_binding`. The RPC checks that the directory exists and the
    /// Personal gate (owner, or an Admin with a fresh audited step-up) runs
    /// before it; the store enforces the id shape and the scope list.
    Employee,
}

impl KeyBinding {
    /// Whether keys of this binding are external.
    pub fn is_external(self) -> bool {
        self == Self::Integration
    }
}

/// A key to issue. There is no `is_external` field: it follows from the
/// binding (an integration key is always external).
#[derive(Debug, Clone)]
pub struct NewMcpKey {
    pub name: String,
    pub client_id: String,
    pub binding: KeyBinding,
    /// `prod` | `staging` | `dev` (the key's prefix).
    pub env: String,
    pub scopes: Vec<String>,
    pub expiry: Expiry,
    /// Dashboard user id of the issuing Admin.
    pub created_by: Option<String>,
}

/// A freshly issued key. Deliberately no `Debug` / `Serialize`: `secret` is
/// the bearer credential and is shown to the Admin exactly once.
pub struct CreatedMcpKey {
    pub key: StoredKey,
    pub secret: String,
}

/// A `[mcp_keys.<plaintext>]` entry being moved into the store.
pub struct LegacyKey<'a> {
    pub raw: &'a str,
    pub client_id: &'a str,
    pub is_external: bool,
    pub scopes: Vec<String>,
    pub created_at: DateTime<Utc>,
    /// An existing `name` / `label` field of the entry, if any.
    pub label: Option<&'a str>,
    /// `duduclaw_core::mcp_oauth_store::legacy_entry_fingerprint` of the
    /// entry, so an OAuth consent bound to it survives the move.
    pub legacy_entry_hash: String,
    /// The employee binding to store (`StoredKey::agent_binding`). The
    /// migration sets it only for an internal entry whose `client_id`
    /// exactly equals an existing employee directory id; it must equal
    /// `client_id` when set.
    pub agent_binding: Option<&'a str>,
}

/// What [`McpKeyStore::import_legacy`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportOutcome {
    Inserted(StoredKey),
    /// The digest is already in the store (an earlier, interrupted migration;
    /// or a key deleted since — its tombstone wins).
    AlreadyPresent,
}

/// What one mutation did: the row before and after, and which columns it
/// changed. `after.version == before.version` means nothing was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyChange {
    pub kind: ChangeKind,
    pub before: StoredKey,
    pub after: StoredKey,
}

impl KeyChange {
    /// Whether this operation wrote anything.
    pub fn wrote(&self) -> bool {
        self.after.version != self.before.version
    }
}

/// The columns a mutation touches (and its compensation may put back).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// `name`.
    Rename,
    /// `expires_at`, `expiry_reviewed`.
    Expiry,
    /// `disabled_at`, `disabled_by` (cleared).
    Enable,
    /// `disabled_at`, `disabled_by` (set). Tightening: never compensated.
    Disable,
    /// `deleted_at`, `deleted_by`. Tightening: never compensated.
    Delete,
}

/// Outcome of a compensating write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Revert {
    /// The columns the operation changed are back to their previous values.
    Reverted,
    /// The row moved on since (another operation changed or deleted it), or
    /// nothing was written in the first place: left alone; the change stays
    /// applied.
    Superseded,
}

/// Expiry-notice thresholds ([`ks::NOTICE_THRESHOLDS_DAYS`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeThreshold {
    SevenDays,
    OneDay,
}

impl NoticeThreshold {
    pub fn days(self) -> i64 {
        match self {
            Self::SevenDays => 7,
            Self::OneDay => 1,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SevenDays => "7d",
            Self::OneDay => "1d",
        }
    }
}

/// One sweep's live claim on one expiry notice (third review, Low): the
/// random token [`McpKeyStore::claim_notice`] stored on the notice row.
/// [`McpKeyStore::confirm_notice_claim`], [`McpKeyStore::notice_delivered`]
/// and [`McpKeyStore::notice_failed`] compare-and-set on it, so a sweep
/// whose lease lapsed (and whose notice another sweep claimed since) can no
/// longer confirm, finalise or release that notice. Not a credential; never
/// logged anyway.
#[derive(Clone, PartialEq, Eq)]
pub struct NoticeClaim {
    token: String,
}

impl std::fmt::Debug for NoticeClaim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NoticeClaim(..)")
    }
}

/// Why a store operation was refused. Store details go to the log only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpKeyError {
    InvalidName,
    NameTaken,
    InvalidClientId,
    InvalidScopes,
    ScopeNotGrantable,
    InvalidEnv,
    InvalidExpiry,
    InvalidKey,
    NotFound,
    /// The row changed between read and write (another Admin acted first).
    Conflict,
    Store,
}

impl McpKeyError {
    /// Stable machine-readable code for the RPC error frame.
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidName => "mcp_key_invalid_name",
            Self::NameTaken => "mcp_key_name_taken",
            Self::InvalidClientId => "mcp_key_invalid_client_id",
            Self::InvalidScopes => "mcp_key_invalid_scopes",
            Self::ScopeNotGrantable => "mcp_key_scope_not_grantable",
            Self::InvalidEnv => "mcp_key_invalid_env",
            Self::InvalidExpiry => "mcp_key_invalid_expiry",
            Self::InvalidKey => "mcp_key_invalid_key",
            Self::NotFound => "mcp_key_not_found",
            Self::Conflict => "mcp_key_conflict",
            Self::Store => "mcp_key_store_unavailable",
        }
    }

    /// Bilingual message for the dashboard.
    pub fn message(self) -> &'static str {
        match self {
            Self::InvalidName => "名稱必填，最多 60 個字元，不可含控制字元。 / Name is required (at most 60 characters, no control characters).",
            Self::NameTaken => "已有同名的金鑰。 / A key with this name already exists.",
            Self::InvalidClientId => "用戶端代號無效（1–128 字元；不可為 gateway-internal、AI 員工代號或系統保留名稱；不可用 eph- 開頭）。 / Invalid client id (1–128 characters; not gateway-internal, an AI employee id or a reserved system name; may not start with eph-).",
            Self::InvalidScopes => "請至少選一個有效的權限範圍。 / Select at least one known scope.",
            Self::ScopeNotGrantable => "外部金鑰不能帶這個權限範圍。 / This scope cannot be granted to an external key.",
            Self::InvalidEnv => "環境必須是 prod、staging 或 dev。 / Environment must be prod, staging or dev.",
            Self::InvalidExpiry => "到期設定無效（30／90／365 天、未來 10 年內的日期，或確認後的不過期）。 / Invalid expiry (30/90/365 days, a future date within 10 years, or a confirmed no-expiry).",
            Self::InvalidKey => "金鑰格式無效。 / Invalid key format.",
            Self::NotFound => "找不到這把金鑰。 / Key not found.",
            Self::Conflict => "這把金鑰剛被其他人修改，請重新整理後再試。 / The key was just changed by someone else; reload and try again.",
            Self::Store => "MCP 金鑰儲存區暫時無法使用。 / The MCP key store is unavailable.",
        }
    }
}

fn store_error(e: impl std::fmt::Display) -> McpKeyError {
    tracing::warn!(error = %e, "MCP key store error");
    McpKeyError::Store
}

fn clean_name(raw: &str) -> Result<String, McpKeyError> {
    let name = raw.trim();
    if name.is_empty() || name.chars().count() > ks::NAME_MAX_CHARS || name.chars().any(char::is_control) {
        return Err(McpKeyError::InvalidName);
    }
    Ok(name.to_string())
}

/// Client id rules for an issued (external) key: 1–128 characters, no
/// control characters, never the reserved gateway-internal id (that key is
/// managed by the gateway), and never an ephemeral sub-agent id (`eph-…`,
/// case-insensitive; v1.81 P1e). Whether it names an AI employee or a
/// reserved system sender is checked by the RPC, which knows the home.
pub fn clean_client_id(raw: &str) -> Result<String, McpKeyError> {
    let id = raw.trim();
    let lowered = id.to_ascii_lowercase();
    if id.is_empty()
        || id.chars().count() > CLIENT_ID_MAX_CHARS
        || id.chars().any(char::is_control)
        || lowered == ks::RESERVED_INTERNAL_CLIENT_ID
        || lowered.starts_with("eph-")
    {
        return Err(McpKeyError::InvalidClientId);
    }
    Ok(id.to_string())
}

/// The scopes a key of this binding may carry.
pub fn grantable_scopes(binding: KeyBinding) -> &'static [&'static str] {
    match binding {
        KeyBinding::Integration => &duduclaw_core::mcp_scopes::EXTERNALLY_GRANTABLE_SCOPE_STRINGS,
        KeyBinding::Employee => &duduclaw_core::mcp_scopes::EMPLOYEE_KEY_SCOPE_STRINGS,
    }
}

/// The first requested scope a key of this binding may not carry, if any.
pub fn first_ungrantable(binding: KeyBinding, raw: &[String]) -> Option<String> {
    raw.iter().map(|s| s.trim()).find(|s| !grantable_scopes(binding).contains(s)).map(str::to_owned)
}

/// RM-134 fourth review (Medium 2): whether every (trimmed, non-empty) scope
/// is one a per-employee key may carry
/// (`duduclaw_core::mcp_scopes::EMPLOYEE_KEY_SCOPE_STRINGS` — never `admin`,
/// never an unknown string). Decides whether a migrated legacy entry may be
/// bound to an employee; such an entry with any other scope is imported
/// unbound and flagged for re-issue, its scopes kept as they were.
pub fn employee_key_scopes_allowed(scopes: &[String]) -> bool {
    scopes
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .all(|s| duduclaw_core::mcp_scopes::EMPLOYEE_KEY_SCOPE_STRINGS.contains(&s))
}

/// The `deleted_by` (and audit actor) of a key tombstoned because the
/// employee it was bound to is gone (fourth review, Medium 1).
pub const EMPLOYEE_GONE_ACTOR: &str = "system:employee_gone";

fn clean_scopes(binding: KeyBinding, raw: &[String]) -> Result<Vec<String>, McpKeyError> {
    let mut scopes: Vec<String> = Vec::with_capacity(raw.len());
    for scope in raw {
        let scope = scope.trim();
        if !duduclaw_core::mcp_scopes::MCP_SCOPE_STRINGS.contains(&scope) {
            return Err(McpKeyError::InvalidScopes);
        }
        if !grantable_scopes(binding).contains(&scope) {
            return Err(McpKeyError::ScopeNotGrantable);
        }
        if !scopes.iter().any(|s| s == scope) {
            scopes.push(scope.to_string());
        }
    }
    if scopes.is_empty() {
        return Err(McpKeyError::InvalidScopes);
    }
    Ok(scopes)
}

fn random_hex(bytes: usize) -> Result<String, McpKeyError> {
    let mut buf = vec![0u8; bytes];
    SystemRandom::new().fill(&mut buf).map_err(|_| McpKeyError::Store)?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

fn scopes_json(scopes: &[String]) -> Result<String, McpKeyError> {
    serde_json::to_string(scopes).map_err(store_error)
}

fn name_taken(conn: &Connection, name: &str, except_id: Option<&str>) -> Result<bool, McpKeyError> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM mcp_api_keys
           WHERE deleted_at IS NULL AND name = ?1 COLLATE NOCASE AND (?2 IS NULL OR id != ?2))",
        params![name, except_id],
        |r| r.get::<_, bool>(0),
    )
    .map_err(store_error)
}

fn row_by_id(conn: &Connection, id: &str) -> Result<StoredKey, McpKeyError> {
    conn.query_row(
        &format!("SELECT {KEY_COLUMNS} FROM mcp_api_keys WHERE id = ?1 AND deleted_at IS NULL"),
        [id],
        ks::key_from_row,
    )
    .optional()
    .map_err(store_error)?
    .ok_or(McpKeyError::NotFound)
}

fn any_row_by_id(conn: &Connection, id: &str) -> Result<StoredKey, McpKeyError> {
    conn.query_row(&format!("SELECT {KEY_COLUMNS} FROM mcp_api_keys WHERE id = ?1"), [id], ks::key_from_row)
        .optional()
        .map_err(store_error)?
        .ok_or(McpKeyError::NotFound)
}

fn opt_ts(at: Option<DateTime<Utc>>) -> Option<String> {
    at.map(ks::timestamp)
}

/// Close every open (pending, claimed or not) expiry notice of key `id` in
/// the caller's transaction — except those announcing `keep_expiry`, which
/// still describe the key after the mutation. Called by every mutation that
/// can make a notice wrong (expiry change, disable, delete, and the
/// compensating reverts of an expiry change / enable), so a sweep that
/// claimed the notice earlier finds it closed and its pre-send check
/// ([`McpKeyStore::confirm_notice_claim`]) drops it.
fn close_open_notices(
    tx: &Connection,
    id: &str,
    keep_expiry: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<(), McpKeyError> {
    tx.execute(
        "UPDATE mcp_api_key_notices SET state = 'closed', finished_at = ?2, claimed_until = NULL, claim_token = NULL
          WHERE key_id = ?1 AND state = 'pending' AND (?3 IS NULL OR expires_at != ?3)",
        params![id, ks::timestamp(now), opt_ts(keep_expiry)],
    )
    .map_err(store_error)?;
    Ok(())
}

/// Exactly one row written, else the row moved on ([`McpKeyError::Conflict`]).
fn one_row(changed: rusqlite::Result<usize>) -> Result<(), McpKeyError> {
    match changed.map_err(store_error)? {
        1 => Ok(()),
        _ => Err(McpKeyError::Conflict),
    }
}

/// Read/write handle on `<home>/mcp_keys.db`. Cheap to open; the gateway
/// opens one per operation.
pub struct McpKeyStore {
    conn: Mutex<Connection>,
    path: PathBuf,
}

impl McpKeyStore {
    /// Open (creating, owner-only, through `duduclaw_core::private_sqlite`)
    /// the store; a brand-new file gets the schema, an existing one must
    /// already carry it exactly (`ks::open_for_gateway`).
    pub fn open(home: &Path) -> Result<Self, String> {
        let conn = ks::open_for_gateway(home).map_err(|e| e.to_string())?;
        Ok(Self { conn: Mutex::new(conn), path: ks::store_path(home) })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Run `work` in one IMMEDIATE transaction.
    fn write<T>(&self, work: impl FnOnce(&Connection) -> Result<T, McpKeyError>) -> Result<T, McpKeyError> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(store_error)?;
        let out = work(&tx)?;
        tx.commit().map_err(store_error)?;
        Ok(out)
    }

    /// Issue a key: external for an integration, internal for a per-agent
    /// key ([`KeyBinding`]). The secret is returned once; only its digest is
    /// stored.
    pub fn create(&self, req: NewMcpKey, now: DateTime<Utc>) -> Result<CreatedMcpKey, McpKeyError> {
        let name = clean_name(&req.name)?;
        let client_id = clean_client_id(&req.client_id)?;
        let env = match req.env.as_str() {
            "prod" | "staging" | "dev" => req.env.clone(),
            _ => return Err(McpKeyError::InvalidEnv),
        };
        let scopes = clean_scopes(req.binding, &req.scopes)?;
        let expires_at = req.expiry.resolve(now)?;
        // Third review (High): a per-agent key is bound, immutably, to the
        // employee's exact directory id. A display name, an `[agent] name`
        // or any other spelling is refused here (the RPC also checks that
        // the directory exists).
        let agent_binding = match req.binding {
            KeyBinding::Employee if duduclaw_core::is_valid_agent_id(&client_id) => Some(client_id.clone()),
            KeyBinding::Employee => return Err(McpKeyError::InvalidClientId),
            KeyBinding::Integration => None,
        };
        let secret = format!("ddc_{env}_{}", random_hex(16)?);
        if !ks::is_key_format(&secret) {
            return Err(McpKeyError::Store);
        }
        let id = format!("mk_{}", random_hex(12)?);
        let key = self.write(|tx| {
            if name_taken(tx, &name, None)? {
                return Err(McpKeyError::NameTaken);
            }
            tx.execute(
                "INSERT INTO mcp_api_keys(id, key_hash, name, client_id, is_external, env, hint, scopes,
                     created_by, created_at, expires_at, migrated, expiry_reviewed, version, agent_binding)
                 VALUES (?1, ?2, ?3, ?4, ?11, ?5, ?6, ?7, ?8, ?9, ?10, 0, 1, 1, ?12)",
                params![
                    id,
                    ks::digest(&secret),
                    name,
                    client_id,
                    env,
                    ks::key_hint(&secret),
                    scopes_json(&scopes)?,
                    req.created_by,
                    ks::timestamp(now),
                    opt_ts(expires_at),
                    req.binding.is_external() as i64,
                    agent_binding,
                ],
            )
            .map_err(store_error)?;
            row_by_id(tx, &id)
        })?;
        Ok(CreatedMcpKey { key, secret })
    }

    /// Every key not deleted, newest first.
    pub fn list(&self) -> Result<Vec<StoredKey>, McpKeyError> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {KEY_COLUMNS} FROM mcp_api_keys WHERE deleted_at IS NULL
                  ORDER BY created_at DESC, id"
            ))
            .map_err(store_error)?;
        let rows = stmt.query_map([], ks::key_from_row).map_err(store_error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_error)
    }

    /// One key that is not deleted.
    pub fn get(&self, id: &str) -> Result<StoredKey, McpKeyError> {
        row_by_id(&self.conn(), id)
    }

    /// One key, tombstones included (tests and compensation reports).
    pub fn get_any(&self, id: &str) -> Result<StoredKey, McpKeyError> {
        any_row_by_id(&self.conn(), id)
    }

    /// Whether the store holds this digest at all (tombstones included).
    pub fn contains_hash(&self, hash: &str) -> Result<bool, McpKeyError> {
        self.conn()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM mcp_api_keys WHERE key_hash = ?1)",
                [hash],
                |r| r.get::<_, bool>(0),
            )
            .map_err(store_error)
    }

    /// The row with this digest, tombstones included.
    pub fn by_hash(&self, hash: &str) -> Result<Option<StoredKey>, McpKeyError> {
        self.conn()
            .query_row(&format!("SELECT {KEY_COLUMNS} FROM mcp_api_keys WHERE key_hash = ?1"), [hash], ks::key_from_row)
            .optional()
            .map_err(store_error)
    }

    /// Rename (version-checked).
    pub fn rename(&self, id: &str, name: &str) -> Result<KeyChange, McpKeyError> {
        let name = clean_name(name)?;
        self.write(|tx| {
            let before = row_by_id(tx, id)?;
            if name_taken(tx, &name, Some(id))? {
                return Err(McpKeyError::NameTaken);
            }
            one_row(tx.execute(
                "UPDATE mcp_api_keys SET name = ?3, version = version + 1
                  WHERE id = ?1 AND version = ?2 AND deleted_at IS NULL",
                params![id, before.version, name],
            ))?;
            let after = row_by_id(tx, id)?;
            Ok(KeyChange { kind: ChangeKind::Rename, before, after })
        })
    }

    /// Change the expiry (version-checked). Marks the expiry as reviewed
    /// (clears the migration hint for this key). Notices are keyed by the
    /// expiry they announce, so a new date re-arms both automatically; every
    /// open notice for another date is closed in the same transaction.
    pub fn set_expiry(&self, id: &str, expiry: Expiry, now: DateTime<Utc>) -> Result<KeyChange, McpKeyError> {
        let expires_at = expiry.resolve(now)?;
        self.write(|tx| {
            let before = row_by_id(tx, id)?;
            one_row(tx.execute(
                "UPDATE mcp_api_keys SET expires_at = ?3, expiry_reviewed = 1, version = version + 1
                  WHERE id = ?1 AND version = ?2 AND deleted_at IS NULL",
                params![id, before.version, opt_ts(expires_at)],
            ))?;
            close_open_notices(tx, id, expires_at, now)?;
            let after = row_by_id(tx, id)?;
            Ok(KeyChange { kind: ChangeKind::Expiry, before, after })
        })
    }

    /// Disable (`true`) or enable (`false`), version-checked. Idempotent: a
    /// key already in the requested state is not written (no new version).
    /// Disabling closes the key's open expiry notices in the same transaction.
    pub fn set_disabled(&self, id: &str, disabled: bool, actor: &str, now: DateTime<Utc>) -> Result<KeyChange, McpKeyError> {
        let kind = if disabled { ChangeKind::Disable } else { ChangeKind::Enable };
        self.write(|tx| {
            let before = row_by_id(tx, id)?;
            if before.disabled_at.is_some() == disabled {
                return Ok(KeyChange { kind, before: before.clone(), after: before });
            }
            let changed = if disabled {
                tx.execute(
                    "UPDATE mcp_api_keys SET disabled_at = ?3, disabled_by = ?4, version = version + 1
                      WHERE id = ?1 AND version = ?2 AND deleted_at IS NULL AND disabled_at IS NULL",
                    params![id, before.version, ks::timestamp(now), actor],
                )
            } else {
                tx.execute(
                    "UPDATE mcp_api_keys SET disabled_at = NULL, disabled_by = NULL, version = version + 1
                      WHERE id = ?1 AND version = ?2 AND deleted_at IS NULL",
                    params![id, before.version],
                )
            };
            one_row(changed)?;
            if disabled {
                close_open_notices(tx, id, None, now)?;
            }
            let after = row_by_id(tx, id)?;
            Ok(KeyChange { kind, before, after })
        })
    }

    /// Delete: the row becomes a tombstone (hidden, refused, digest kept).
    /// Its open expiry notices are closed in the same transaction.
    pub fn delete(&self, id: &str, actor: &str, now: DateTime<Utc>) -> Result<KeyChange, McpKeyError> {
        self.write(|tx| {
            let before = row_by_id(tx, id)?;
            one_row(tx.execute(
                "UPDATE mcp_api_keys SET deleted_at = ?3, deleted_by = ?4, version = version + 1
                  WHERE id = ?1 AND version = ?2 AND deleted_at IS NULL",
                params![id, before.version, ks::timestamp(now), actor],
            ))?;
            close_open_notices(tx, id, None, now)?;
            let after = any_row_by_id(tx, id)?;
            Ok(KeyChange { kind: ChangeKind::Delete, before, after })
        })
    }

    /// RM-134 fourth review (Medium 1): permanently revoke every key bound to
    /// the employee `agent_id` (`agent_binding` equal, byte for byte) — the
    /// dashboard deletes that employee. Each row becomes a tombstone exactly
    /// like [`Self::delete`] (digest kept, so no stale copy can come back;
    /// never compensated), `deleted_by = actor`, its open expiry notices
    /// closed, all in ONE transaction. Keys bound to another id, unbound
    /// keys and existing tombstones are untouched. Returns one
    /// [`ChangeKind::Delete`] change per key, for the audit records.
    pub fn revoke_employee_keys(&self, agent_id: &str, actor: &str, now: DateTime<Utc>) -> Result<Vec<KeyChange>, McpKeyError> {
        self.write(|tx| {
            let bound: Vec<StoredKey> = {
                let mut stmt = tx
                    .prepare(&format!(
                        "SELECT {KEY_COLUMNS} FROM mcp_api_keys
                          WHERE agent_binding = ?1 AND deleted_at IS NULL ORDER BY created_at, id"
                    ))
                    .map_err(store_error)?;
                let rows = stmt.query_map([agent_id], ks::key_from_row).map_err(store_error)?;
                rows.collect::<Result<Vec<_>, _>>().map_err(store_error)?
            };
            let mut changes = Vec::with_capacity(bound.len());
            for before in bound {
                one_row(tx.execute(
                    "UPDATE mcp_api_keys SET deleted_at = ?3, deleted_by = ?4, version = version + 1
                      WHERE id = ?1 AND version = ?2 AND deleted_at IS NULL",
                    params![before.id, before.version, ks::timestamp(now), actor],
                ))?;
                close_open_notices(tx, &before.id, None, now)?;
                let after = any_row_by_id(tx, &before.id)?;
                changes.push(KeyChange { kind: ChangeKind::Delete, before, after });
            }
            Ok(changes)
        })
    }

    /// The compensating write for a loosening change whose audit record
    /// could not be written: put back ONLY the columns `change` touched, and
    /// only if the row is still at the exact version `change` produced (and
    /// not deleted). Anything else — a later rename, disable, delete, expiry
    /// change — leaves the row alone ([`Revert::Superseded`]). Tightening
    /// changes (disable, delete) are never compensated.
    pub fn revert(&self, change: &KeyChange) -> Result<Revert, McpKeyError> {
        if !change.wrote() {
            return Ok(Revert::Superseded);
        }
        let (id, version, before) = (&change.after.id, change.after.version, &change.before);
        if matches!(change.kind, ChangeKind::Disable | ChangeKind::Delete) {
            return Ok(Revert::Superseded);
        }
        // One transaction: the compensating write and (for an expiry or an
        // enable) closing the notices it makes wrong.
        self.write(|tx| {
            let changed = match change.kind {
                ChangeKind::Disable | ChangeKind::Delete => return Ok(Revert::Superseded),
                ChangeKind::Rename => tx.execute(
                    "UPDATE mcp_api_keys SET name = ?3, version = version + 1
                      WHERE id = ?1 AND version = ?2 AND deleted_at IS NULL",
                    params![id, version, before.name],
                ),
                ChangeKind::Expiry => tx.execute(
                    "UPDATE mcp_api_keys SET expires_at = ?3, expiry_reviewed = ?4, version = version + 1
                      WHERE id = ?1 AND version = ?2 AND deleted_at IS NULL",
                    params![id, version, opt_ts(before.expires_at), before.expiry_reviewed as i64],
                ),
                ChangeKind::Enable => tx.execute(
                    "UPDATE mcp_api_keys SET disabled_at = ?3, disabled_by = ?4, version = version + 1
                      WHERE id = ?1 AND version = ?2 AND deleted_at IS NULL AND disabled_at IS NULL",
                    params![id, version, opt_ts(before.disabled_at), before.disabled_by],
                ),
            };
            if changed.map_err(store_error)? == 0 {
                return Ok(Revert::Superseded);
            }
            match change.kind {
                ChangeKind::Expiry => close_open_notices(tx, id, before.expires_at, Utc::now())?,
                ChangeKind::Enable => close_open_notices(tx, id, None, Utc::now())?,
                _ => {}
            }
            Ok(Revert::Reverted)
        })
    }

    /// Withdraw a just-created key whose creation could not be audited (its
    /// secret is never handed out). Removed outright while it is still at
    /// its first version; otherwise tombstoned (a tightening, always safe).
    pub fn discard_created(&self, key: &StoredKey, now: DateTime<Utc>) -> Result<Revert, McpKeyError> {
        self.write(|tx| {
            let removed = tx
                .execute("DELETE FROM mcp_api_keys WHERE id = ?1 AND version = ?2", params![key.id, key.version])
                .map_err(store_error)?;
            if removed > 0 {
                return Ok(Revert::Reverted);
            }
            tx.execute(
                "UPDATE mcp_api_keys SET deleted_at = COALESCE(deleted_at, ?2),
                     deleted_by = COALESCE(deleted_by, 'system:audit_failure'), version = version + 1
                  WHERE id = ?1 AND deleted_at IS NULL",
                params![key.id, ks::timestamp(now)],
            )
            .map_err(store_error)?;
            Ok(Revert::Superseded)
        })
    }

    // ── `[mcp_keys]` migration bookkeeping ───────────────────────────────

    /// Where the migration stands.
    pub fn migration_state(&self) -> Result<MigrationState, McpKeyError> {
        ks::migration_state(&self.conn()).map_err(store_error)
    }

    /// Record the pending set — the fingerprints of the `[mcp_keys]` entries
    /// in `config.toml` as the gateway read it now — in one transaction,
    /// before anything is imported. Only the first call has an effect: once
    /// a pending set (or completion) is recorded it is never extended, so an
    /// entry added to `config.toml` later is never "pending".
    pub fn begin_legacy_migration(&self, hashes: &[String], now: DateTime<Utc>) -> Result<MigrationState, McpKeyError> {
        self.write(|tx| {
            let state = ks::migration_state(tx).map_err(store_error)?;
            if state != MigrationState::NotStarted {
                return Ok(state);
            }
            for hash in hashes {
                if !ks::is_digest(hash) {
                    return Err(McpKeyError::InvalidKey);
                }
                tx.execute(
                    "INSERT OR IGNORE INTO mcp_legacy_pending(key_hash, recorded_at) VALUES (?1, ?2)",
                    params![hash, ks::timestamp(now)],
                )
                .map_err(store_error)?;
            }
            tx.execute(
                "INSERT INTO mcp_api_key_meta(key, value) VALUES (?1, ?2)",
                params![ks::META_LEGACY_MIGRATION, format!("pending:{}", ks::timestamp(now))],
            )
            .map_err(store_error)?;
            Ok(MigrationState::Pending)
        })
    }

    /// The fingerprints still pending.
    pub fn pending_hashes(&self) -> Result<HashSet<String>, McpKeyError> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT key_hash FROM mcp_legacy_pending").map_err(store_error)?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).map_err(store_error)?;
        rows.collect::<Result<HashSet<_>, _>>().map_err(store_error)
    }

    /// One pending fingerprint is settled (moved and audited, or gone).
    pub fn clear_pending(&self, hash: &str) -> Result<(), McpKeyError> {
        self.conn()
            .execute("DELETE FROM mcp_legacy_pending WHERE key_hash = ?1", [hash])
            .map(|_| ())
            .map_err(store_error)
    }

    /// Write the persistent completion marker. Refused while anything is
    /// still pending.
    pub fn complete_migration(&self, now: DateTime<Utc>) -> Result<bool, McpKeyError> {
        self.write(|tx| {
            let left: i64 = tx.query_row("SELECT count(*) FROM mcp_legacy_pending", [], |r| r.get(0)).map_err(store_error)?;
            if left > 0 || ks::migration_state(tx).map_err(store_error)? != MigrationState::Pending {
                return Ok(false);
            }
            tx.execute(
                "UPDATE mcp_api_key_meta SET value = ?2 WHERE key = ?1",
                params![ks::META_LEGACY_MIGRATION, format!("complete:{}", ks::timestamp(now))],
            )
            .map_err(store_error)?;
            Ok(true)
        })
    }

    /// Note an ignored `[mcp_keys]` entry. `true` the first time (so it is
    /// logged once); the hint is the key's display hint, never a fingerprint.
    pub fn note_ignored(&self, hash: &str, hint: &str, now: DateTime<Utc>) -> Result<bool, McpKeyError> {
        self.conn()
            .execute(
                "INSERT OR IGNORE INTO mcp_legacy_ignored(key_hash, hint, first_seen_at) VALUES (?1, ?2, ?3)",
                params![hash, hint, ks::timestamp(now)],
            )
            .map(|changed| changed > 0)
            .map_err(store_error)
    }

    /// Move one `[mcp_keys]` entry in. Idempotent on the digest; behaviour is
    /// kept: client, external flag and scopes as they were, and `expires_at`
    /// the instant the pre-v1.82 rule started refusing the entry
    /// ([`ks::legacy_expires_at`], `created_at + 31 days` in whole seconds).
    /// The key is flagged for the Admin's expiry review.
    pub fn import_legacy(&self, entry: &LegacyKey<'_>, now: DateTime<Utc>) -> Result<ImportOutcome, McpKeyError> {
        let env = ks::key_env(entry.raw).ok_or(McpKeyError::InvalidKey)?;
        let hash = ks::digest(entry.raw);
        // A binding is only ever the entry's own client id, for an internal
        // key, in the exact agent-id shape (the caller checked the directory).
        if let Some(binding) = entry.agent_binding
            && (entry.is_external || binding != entry.client_id || !duduclaw_core::is_valid_agent_id(binding))
        {
            return Err(McpKeyError::InvalidClientId);
        }
        // Fourth review (Medium 2): and only for an entry whose scopes are
        // all per-employee scopes — a per-employee key never carries
        // `admin` (or anything outside that list). The migration imports
        // such an entry unbound instead; nothing is dropped or downgraded.
        if entry.agent_binding.is_some() && !employee_key_scopes_allowed(&entry.scopes) {
            return Err(McpKeyError::ScopeNotGrantable);
        }
        self.write(|tx| {
            let present: bool = tx
                .query_row("SELECT EXISTS(SELECT 1 FROM mcp_api_keys WHERE key_hash = ?1)", [&hash], |r| r.get(0))
                .map_err(store_error)?;
            if present {
                return Ok(ImportOutcome::AlreadyPresent);
            }
            let id = format!("mk_{}", random_hex(12)?);
            let short: String = id.chars().skip(3).take(8).collect();
            let mut name = entry
                .label
                .and_then(|label| clean_name(label).ok())
                .filter(|label| !matches!(name_taken(tx, label, None), Ok(true)))
                .unwrap_or_else(|| format!("migrated-{short}"));
            if name_taken(tx, &name, None)? {
                name = format!("migrated-{}", id.chars().skip(3).collect::<String>());
            }
            let created_at = ks::whole_second(entry.created_at);
            let expires_at = ks::legacy_expires_at(entry.created_at);
            let scopes: Vec<String> =
                entry.scopes.iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            tx.execute(
                "INSERT INTO mcp_api_keys(id, key_hash, name, client_id, is_external, env, hint, scopes,
                     created_by, created_at, expires_at, migrated, expiry_reviewed, legacy_entry_hash, version,
                     agent_binding)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9, ?10, 1, 0, ?11, 1, ?12)",
                params![
                    id,
                    hash,
                    name,
                    entry.client_id,
                    entry.is_external as i64,
                    env,
                    ks::key_hint(entry.raw),
                    scopes_json(&scopes)?,
                    ks::timestamp(created_at),
                    ks::timestamp(expires_at),
                    entry.legacy_entry_hash,
                    entry.agent_binding,
                ],
            )
            .map_err(store_error)?;
            tx.execute(
                "INSERT INTO mcp_api_key_meta(key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![MIGRATION_HINT, format!("pending:{}", ks::timestamp(now))],
            )
            .map_err(store_error)?;
            Ok(ImportOutcome::Inserted(row_by_id(tx, &id)?))
        })
    }

    // ── gateway-internal key fingerprints ────────────────────────────────

    /// The recorded internal key fingerprints.
    pub fn internal_key_hashes(&self) -> Result<HashSet<String>, McpKeyError> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT key_hash FROM mcp_internal_keys").map_err(store_error)?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).map_err(store_error)?;
        rows.collect::<Result<HashSet<_>, _>>().map_err(store_error)
    }

    /// Record the gateway's own internal key (idempotent; an existing record
    /// keeps its original `created_at`).
    pub fn record_internal_key(&self, raw: &str, created_at: DateTime<Utc>) -> Result<(), McpKeyError> {
        if !ks::is_key_format(raw) {
            return Err(McpKeyError::InvalidKey);
        }
        self.conn()
            .execute(
                "INSERT INTO mcp_internal_keys(key_hash, created_at) VALUES (?1, ?2)
                 ON CONFLICT(key_hash) DO NOTHING",
                params![ks::digest(raw), ks::timestamp(created_at)],
            )
            .map(|_| ())
            .map_err(store_error)
    }

    /// Drop every internal record whose fingerprint is not in `keep`.
    pub fn retain_internal_keys(&self, keep: &HashSet<String>) -> Result<usize, McpKeyError> {
        let recorded = self.internal_key_hashes()?;
        let conn = self.conn();
        let mut removed = 0;
        for hash in recorded.difference(keep) {
            removed += conn.execute("DELETE FROM mcp_internal_keys WHERE key_hash = ?1", [hash]).map_err(store_error)?;
        }
        Ok(removed)
    }

    // ── migration hint ────────────────────────────────────────────────────

    /// Whether the keys page should show the one-time "review the expiry of
    /// migrated keys" hint: a migration happened, the Admin has not
    /// dismissed it, and a migrated key whose expiry nobody changed remains.
    pub fn migration_hint_pending(&self) -> Result<bool, McpKeyError> {
        let conn = self.conn();
        let state: Option<String> = conn
            .query_row("SELECT value FROM mcp_api_key_meta WHERE key = ?1", [MIGRATION_HINT], |r| r.get(0))
            .optional()
            .map_err(store_error)?;
        if !state.is_some_and(|s| s.starts_with("pending")) {
            return Ok(false);
        }
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM mcp_api_keys
               WHERE migrated = 1 AND expiry_reviewed = 0 AND deleted_at IS NULL)",
            [],
            |r| r.get::<_, bool>(0),
        )
        .map_err(store_error)
    }

    /// Dismiss the migration hint for good (until another migration).
    /// Returns the previous state value (`None` = never set), so a caller
    /// whose audit fails can put it back with [`Self::restore_migration_hint`].
    pub fn dismiss_migration_hint(&self, now: DateTime<Utc>) -> Result<Option<String>, McpKeyError> {
        self.write(|tx| {
            let previous: Option<String> = tx
                .query_row("SELECT value FROM mcp_api_key_meta WHERE key = ?1", [MIGRATION_HINT], |r| r.get(0))
                .optional()
                .map_err(store_error)?;
            tx.execute(
                "INSERT INTO mcp_api_key_meta(key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![MIGRATION_HINT, format!("dismissed:{}", ks::timestamp(now))],
            )
            .map_err(store_error)?;
            Ok(previous)
        })
    }

    /// Put the hint state back to `previous` if it still reads `dismissed_at`.
    pub fn restore_migration_hint(&self, previous: Option<&str>, dismissed_at: DateTime<Utc>) -> Result<Revert, McpKeyError> {
        let ours = format!("dismissed:{}", ks::timestamp(dismissed_at));
        let changed = match previous {
            Some(value) => self.conn().execute(
                "UPDATE mcp_api_key_meta SET value = ?2 WHERE key = ?1 AND value = ?3",
                params![MIGRATION_HINT, value, ours],
            ),
            None => self.conn().execute(
                "DELETE FROM mcp_api_key_meta WHERE key = ?1 AND value = ?2",
                params![MIGRATION_HINT, ours],
            ),
        };
        Ok(if changed.map_err(store_error)? > 0 { Revert::Reverted } else { Revert::Superseded })
    }

    // ── expiry notices: claim → attempt → delivered / retry ───────────────

    /// Keys owing an expiry notice at `now`, with the most urgent threshold
    /// each owes. Only usable keys with an expiry ahead of `now`; a notice
    /// already delivered or closed, one claimed by a live sweep, and one
    /// waiting out its retry delay are not due. Notices that can no longer be
    /// met — key deleted or gone, expiry changed or passed — are closed
    /// without being sent; a pending 7-day notice is closed once the key is
    /// inside the 1-day window.
    pub fn due_notices(&self, now: DateTime<Utc>) -> Result<Vec<(StoredKey, NoticeThreshold)>, McpKeyError> {
        let keys = self.list()?;
        self.write(|tx| {
            let stamp = ks::timestamp(now);
            tx.execute(
                "UPDATE mcp_api_key_notices SET state = 'closed', finished_at = ?1, claimed_until = NULL
                  WHERE state = 'pending' AND NOT EXISTS (
                      SELECT 1 FROM mcp_api_keys k
                       WHERE k.id = mcp_api_key_notices.key_id AND k.deleted_at IS NULL
                         AND k.expires_at = mcp_api_key_notices.expires_at AND k.expires_at > ?1)",
                [&stamp],
            )
            .map_err(store_error)?;
            let mut due = Vec::new();
            for key in keys {
                if key.state(now) != KeyState::Active {
                    continue;
                }
                let Some(expires_at) = key.expires_at else { continue };
                let left = expires_at - now;
                let threshold = if left <= Duration::days(NoticeThreshold::OneDay.days()) {
                    tx.execute(
                        "UPDATE mcp_api_key_notices SET state = 'closed', finished_at = ?3, claimed_until = NULL
                          WHERE key_id = ?1 AND threshold = '7d' AND expires_at = ?2 AND state = 'pending'",
                        params![key.id, ks::timestamp(expires_at), stamp],
                    )
                    .map_err(store_error)?;
                    NoticeThreshold::OneDay
                } else if left <= Duration::days(NoticeThreshold::SevenDays.days()) {
                    NoticeThreshold::SevenDays
                } else {
                    continue;
                };
                let row: Option<(String, Option<String>, Option<String>)> = tx
                    .query_row(
                        "SELECT state, claimed_until, next_attempt_at FROM mcp_api_key_notices
                          WHERE key_id = ?1 AND threshold = ?2 AND expires_at = ?3",
                        params![key.id, threshold.as_str(), ks::timestamp(expires_at)],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .optional()
                    .map_err(store_error)?;
                let waiting = |at: &Option<String>| at.as_deref().is_some_and(|at| at > stamp.as_str());
                match row {
                    Some((state, _, _)) if state != "pending" => continue,
                    Some((_, claimed, next)) if waiting(&claimed) || waiting(&next) => continue,
                    _ => due.push((key, threshold)),
                }
            }
            Ok(due)
        })
    }

    /// Claim one notice before attempting it, so only one sweep sends it at
    /// a time; the claim is a lease ([`NOTICE_CLAIM_LEASE_SECS`]) so a sweep
    /// that died mid-send does not block it for good. Each claim stores a
    /// fresh random token on the notice row ([`NoticeClaim`]); every later
    /// step must present it. `None` when it is claimed elsewhere, delivered,
    /// closed, waiting out a retry delay, or the key's expiry / state changed
    /// since `key` was read.
    pub fn claim_notice(&self, key: &StoredKey, threshold: NoticeThreshold, now: DateTime<Utc>) -> Result<Option<NoticeClaim>, McpKeyError> {
        let Some(expires_at) = key.expires_at else { return Ok(None) };
        let (stamp, expiry) = (ks::timestamp(now), ks::timestamp(expires_at));
        let token = random_hex(16)?;
        self.write(|tx| {
            let current: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM mcp_api_keys WHERE id = ?1 AND expires_at = ?2 AND expires_at > ?3
                         AND deleted_at IS NULL AND disabled_at IS NULL)",
                    params![key.id, expiry, stamp],
                    |r| r.get(0),
                )
                .map_err(store_error)?;
            if !current {
                return Ok(None);
            }
            let lease = ks::timestamp(now + Duration::seconds(NOTICE_CLAIM_LEASE_SECS));
            let changed = tx
                .execute(
                    "INSERT INTO mcp_api_key_notices(key_id, threshold, expires_at, state, attempts, claimed_until, claim_token)
                     VALUES (?1, ?2, ?3, 'pending', 1, ?4, ?6)
                     ON CONFLICT(key_id, threshold, expires_at) DO UPDATE
                        SET attempts = attempts + 1, claimed_until = excluded.claimed_until,
                            claim_token = excluded.claim_token
                      WHERE state = 'pending'
                        AND (claimed_until IS NULL OR claimed_until <= ?5)
                        AND (next_attempt_at IS NULL OR next_attempt_at <= ?5)",
                    params![key.id, threshold.as_str(), expiry, lease, stamp, token],
                )
                .map_err(store_error)?;
            Ok((changed > 0).then(|| NoticeClaim { token: token.clone() }))
        })
    }

    /// Immediately before sending a claimed notice: re-check, in one
    /// transaction, that the key row is still exactly the `key` the notice
    /// was claimed for (same id, same row `version`, same `expires_at`, still
    /// usable, expiry still ahead of `now`) and that the notice is still open
    /// and held by THIS claim (`claim` token, `claimed_until > now`). `true`
    /// ⇒ send. Otherwise this claim is released (`claimed_until` and the
    /// token cleared, only while the token is still ours; a notice a mutation
    /// already closed stays closed) and the caller drops it without sending —
    /// a later sweep re-reads the key and decides afresh.
    pub fn confirm_notice_claim(
        &self,
        key: &StoredKey,
        threshold: NoticeThreshold,
        claim: &NoticeClaim,
        now: DateTime<Utc>,
    ) -> Result<bool, McpKeyError> {
        let Some(expires_at) = key.expires_at else { return Ok(false) };
        let (stamp, expiry) = (ks::timestamp(now), ks::timestamp(expires_at));
        self.write(|tx| {
            let key_current: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM mcp_api_keys
                       WHERE id = ?1 AND version = ?2 AND expires_at = ?3 AND expires_at > ?4
                         AND deleted_at IS NULL AND disabled_at IS NULL)",
                    params![key.id, key.version, expiry, stamp],
                    |r| r.get(0),
                )
                .map_err(store_error)?;
            let claim_live: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM mcp_api_key_notices
                       WHERE key_id = ?1 AND threshold = ?2 AND expires_at = ?3 AND state = 'pending'
                         AND claim_token = ?5 AND claimed_until IS NOT NULL AND claimed_until > ?4)",
                    params![key.id, threshold.as_str(), expiry, stamp, claim.token],
                    |r| r.get(0),
                )
                .map_err(store_error)?;
            if key_current && claim_live {
                return Ok(true);
            }
            tx.execute(
                "UPDATE mcp_api_key_notices SET claimed_until = NULL, claim_token = NULL
                  WHERE key_id = ?1 AND threshold = ?2 AND expires_at = ?3 AND state = 'pending'
                    AND claim_token = ?4",
                params![key.id, threshold.as_str(), expiry, claim.token],
            )
            .map_err(store_error)?;
            Ok(false)
        })
    }

    /// The claimed notice reached an Admin (sent, or durably queued for
    /// quiet hours). Delivering the 1-day notice also closes the 7-day one.
    ///
    /// Third review (Low): a compare-and-set on the notice row held by THIS
    /// claim (`claim` token, `claimed_until > now`) AND on the key row the
    /// notice was claimed for (id, row `version`, `expires_at`, still
    /// usable). `Ok(false)` when nothing matched — a key mutation closed the
    /// notice, the claim was released or taken over by another sweep after
    /// its lease lapsed, or the key changed — and the caller must then
    /// neither count nor audit a delivery.
    pub fn notice_delivered(
        &self,
        key: &StoredKey,
        threshold: NoticeThreshold,
        claim: &NoticeClaim,
        now: DateTime<Utc>,
    ) -> Result<bool, McpKeyError> {
        let Some(expires_at) = key.expires_at else { return Ok(false) };
        let (stamp, expiry) = (ks::timestamp(now), ks::timestamp(expires_at));
        self.write(|tx| {
            let marked = tx
                .execute(
                    "UPDATE mcp_api_key_notices
                        SET state = 'delivered', finished_at = ?4, claimed_until = NULL, claim_token = NULL
                      WHERE key_id = ?1 AND threshold = ?2 AND expires_at = ?3 AND state = 'pending'
                        AND claim_token = ?5 AND claimed_until IS NOT NULL AND claimed_until > ?4
                        AND EXISTS (SELECT 1 FROM mcp_api_keys k
                                     WHERE k.id = ?1 AND k.version = ?6 AND k.expires_at = ?3
                                       AND k.deleted_at IS NULL AND k.disabled_at IS NULL)",
                    params![key.id, threshold.as_str(), expiry, stamp, claim.token, key.version],
                )
                .map_err(store_error)?;
            if marked == 0 {
                return Ok(false);
            }
            if threshold == NoticeThreshold::OneDay {
                tx.execute(
                    "UPDATE mcp_api_key_notices SET state = 'closed', finished_at = ?3, claimed_until = NULL,
                            claim_token = NULL
                      WHERE key_id = ?1 AND threshold = '7d' AND expires_at = ?2 AND state = 'pending'",
                    params![key.id, expiry, stamp],
                )
                .map_err(store_error)?;
            }
            Ok(true)
        })
    }

    /// The claimed attempt failed: release THIS claim and schedule the next
    /// attempt (`NOTICE_RETRY_BASE_SECS · 2^(attempts-1)`, at most
    /// [`NOTICE_RETRY_MAX_SECS`]). Returns when it becomes due again.
    ///
    /// Fourth review (Low): ONE compare-and-set UPDATE with the same
    /// conditions as [`Self::notice_delivered`] — the notice row held by
    /// THIS claim (`claim` token, `claimed_until > now`) AND the key row the
    /// notice was claimed for (id, row `version`, `expires_at`, still
    /// usable); the backoff is computed from the row inside that statement.
    /// `Ok(None)` = stale (a send that hung past its lease, a claim taken
    /// over or released, a key that changed version, was disabled or
    /// deleted): nothing is written — no claim cleared, no retry scheduled —
    /// and the caller neither reschedules nor records a failure.
    pub fn notice_failed(
        &self,
        key: &StoredKey,
        threshold: NoticeThreshold,
        claim: &NoticeClaim,
        now: DateTime<Utc>,
    ) -> Result<Option<DateTime<Utc>>, McpKeyError> {
        let Some(expires_at) = key.expires_at else { return Ok(None) };
        let (stamp, expiry) = (ks::timestamp(now), ks::timestamp(expires_at));
        self.write(|tx| {
            // `next_attempt_at` = the whole second of `now` + the backoff, in
            // the store's one timestamp format (`ks::timestamp`).
            let delay: Option<i64> = tx
                .query_row(
                    "UPDATE mcp_api_key_notices
                        SET claimed_until = NULL, claim_token = NULL,
                            next_attempt_at = strftime('%Y-%m-%dT%H:%M:%SZ',
                                ?7 + min(?8 * (1 << (min(max(attempts, 1), 16) - 1)), ?9), 'unixepoch')
                      WHERE key_id = ?1 AND threshold = ?2 AND expires_at = ?3 AND state = 'pending'
                        AND claim_token = ?5 AND claimed_until IS NOT NULL AND claimed_until > ?4
                        AND EXISTS (SELECT 1 FROM mcp_api_keys k
                                     WHERE k.id = ?1 AND k.version = ?6 AND k.expires_at = ?3
                                       AND k.deleted_at IS NULL AND k.disabled_at IS NULL)
                      RETURNING min(?8 * (1 << (min(max(attempts, 1), 16) - 1)), ?9)",
                    params![
                        key.id,
                        threshold.as_str(),
                        expiry,
                        stamp,
                        claim.token,
                        key.version,
                        now.timestamp(),
                        NOTICE_RETRY_BASE_SECS,
                        NOTICE_RETRY_MAX_SECS,
                    ],
                    |r| r.get(0),
                )
                .optional()
                .map_err(store_error)?;
            Ok(delay.map(|delay| now + Duration::seconds(delay)))
        })
    }

    /// The recorded notice state for one key / threshold / expiry (tests,
    /// diagnostics): `(state, attempts)`.
    pub fn notice_state(&self, key: &StoredKey, threshold: NoticeThreshold) -> Result<Option<(String, i64)>, McpKeyError> {
        let Some(expires_at) = key.expires_at else { return Ok(None) };
        self.conn()
            .query_row(
                "SELECT state, attempts FROM mcp_api_key_notices WHERE key_id = ?1 AND threshold = ?2 AND expires_at = ?3",
                params![key.id, threshold.as_str(), ks::timestamp(expires_at)],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(store_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_key(name: &str, expiry: Expiry) -> NewMcpKey {
        NewMcpKey {
            name: name.into(),
            client_id: "ci-bot".into(),
            binding: KeyBinding::Integration,
            env: "prod".into(),
            scopes: vec!["memory:read".into(), "memory:read".into(), "wiki:read".into()],
            expiry,
            created_by: Some("admin-1".into()),
        }
    }

    fn store() -> (tempfile::TempDir, McpKeyStore) {
        let home = tempfile::tempdir().unwrap();
        let store = McpKeyStore::open(home.path()).unwrap();
        (home, store)
    }

    fn file_bytes(home: &Path) -> Vec<u8> {
        let mut all = std::fs::read(ks::store_path(home)).unwrap();
        for side in ["-wal", "-journal"] {
            if let Ok(more) = std::fs::read(format!("{}{side}", ks::store_path(home).display())) {
                all.extend(more);
            }
        }
        all
    }

    fn contains(haystack: &[u8], needle: &str) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle.as_bytes())
    }

    #[test]
    fn created_secret_is_returned_once_and_only_its_digest_is_stored() {
        let (home, store) = store();
        let now = Utc::now();
        let created = store.create(new_key("CI 機器人", Expiry::Days(90)), now).unwrap();
        assert!(ks::is_key_format(&created.secret));
        assert_eq!(created.key.name, "CI 機器人");
        assert_eq!(created.key.scopes, vec!["memory:read", "wiki:read"], "deduplicated, order kept");
        assert_eq!(created.key.created_by.as_deref(), Some("admin-1"));
        assert!(created.key.is_external, "every issued key is external");
        assert_eq!(created.key.version, 1);
        assert_eq!(ks::timestamp(created.key.expires_at.unwrap()), ks::timestamp(now + Duration::days(90)));
        assert!(created.key.id.starts_with("mk_"));
        assert_eq!(created.key.hint, ks::key_hint(&created.secret));
        drop(store);
        let bytes = file_bytes(home.path());
        let fragment: String = created.secret.chars().skip(created.secret.chars().count() - 32).take(20).collect();
        assert!(!contains(&bytes, &created.secret), "plaintext never stored");
        assert!(!contains(&bytes, &fragment), "no long fragment of the secret stored");
        assert!(contains(&bytes, &ks::digest(&created.secret)));
        // The read path authenticates it.
        let found = ks::lookup(home.path(), &created.secret).unwrap().unwrap();
        assert_eq!(found.id, created.key.id);
        assert_eq!(found.state(now), KeyState::Active);
    }

    #[cfg(unix)]
    #[test]
    fn store_file_is_owner_only_and_an_insecure_existing_file_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let (home, store) = store();
        drop(store);
        let path = ks::store_path(home.path());
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o664)).unwrap();
        assert!(McpKeyStore::open(home.path()).is_err(), "never opened, never repaired silently");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(McpKeyStore::open(home.path()).is_ok());
    }

    #[test]
    fn names_are_required_bounded_and_unique_case_insensitively() {
        let (_home, store) = store();
        let now = Utc::now();
        let long = "字".repeat(ks::NAME_MAX_CHARS + 1);
        for bad in ["", "   ", "a\nb", long.as_str()] {
            assert_eq!(store.create(new_key(bad, Expiry::Days(30)), now).err(), Some(McpKeyError::InvalidName), "{bad:?}");
        }
        store.create(new_key(&"字".repeat(ks::NAME_MAX_CHARS), Expiry::Days(30)), now).unwrap();
        let first = store.create(new_key("Zapier", Expiry::Days(30)), now).unwrap();
        assert_eq!(store.create(new_key("zapier", Expiry::Days(30)), now).err(), Some(McpKeyError::NameTaken));
        let other = store.create(new_key("Make", Expiry::Days(30)), now).unwrap();
        assert_eq!(store.rename(&other.key.id, " ZAPIER ").err(), Some(McpKeyError::NameTaken));
        // Deleting frees the name.
        store.delete(&first.key.id, "admin-1", now).unwrap();
        let change = store.rename(&other.key.id, "zapier").unwrap();
        assert_eq!((change.before.name.as_str(), change.after.name.as_str()), ("Make", "zapier"));
    }

    #[test]
    fn client_scope_env_and_expiry_are_validated() {
        let (_home, store) = store();
        let now = Utc::now();
        let mut req = new_key("x", Expiry::Days(30));
        for client in ["", "gateway-internal", "Gateway-Internal", "eph-123", "EPH-x", "a\tb"] {
            req.client_id = client.into();
            assert_eq!(store.create(req.clone(), now).err(), Some(McpKeyError::InvalidClientId), "{client}");
        }
        let mut req = new_key("y", Expiry::Days(30));
        req.scopes = vec![];
        assert_eq!(store.create(req.clone(), now).err(), Some(McpKeyError::InvalidScopes));
        req.scopes = vec!["memory:read".into(), "root".into()];
        assert_eq!(store.create(req.clone(), now).err(), Some(McpKeyError::InvalidScopes));
        // Known scopes outside the externally grantable list are refused.
        for scope in ["admin", "odoo:read", "google:read", "team:handoff", "messaging:read"] {
            req.scopes = vec![scope.into()];
            assert_eq!(store.create(req.clone(), now).err(), Some(McpKeyError::ScopeNotGrantable), "{scope}");
            assert_eq!(first_ungrantable(KeyBinding::Integration, &req.scopes), Some(scope.to_string()));
        }
        // A per-agent key is internal and never carries `admin` (or a scope
        // the v1.81 picker never offered).
        req.binding = KeyBinding::Employee;
        for scope in ["admin", "team:handoff", "knowledge:read"] {
            req.scopes = vec!["memory:read".into(), scope.into()];
            assert_eq!(store.create(req.clone(), now).err(), Some(McpKeyError::ScopeNotGrantable), "{scope}");
        }
        req.scopes = vec!["google:read".into(), "odoo:read".into()];
        let employee = store.create(req.clone(), now).unwrap().key;
        assert!(!employee.is_external);
        // Third review (High): bound, immutably, to the exact directory id.
        assert_eq!(employee.agent_binding.as_deref(), Some("ci-bot"));
        // A display name / `[agent] name` form is not a directory id (the
        // RPC additionally requires the exact directory to exist).
        for alias in ["CI Bot", "ci bot", "機器人", "ci.bot"] {
            let mut aliased = req.clone();
            aliased.name = format!("alias {alias}");
            aliased.client_id = alias.into();
            assert_eq!(store.create(aliased, now).err(), Some(McpKeyError::InvalidClientId), "{alias}");
        }
        // Integration keys never carry a binding, and no mutation adds one.
        let integration = store.create(new_key("integration", Expiry::Days(30)), now).unwrap().key;
        assert_eq!(integration.agent_binding, None);
        let renamed = store.rename(&employee.id, "renamed employee key").unwrap().after;
        let disabled = store.set_disabled(&renamed.id, true, "admin", now).unwrap().after;
        let enabled = store.set_disabled(&disabled.id, false, "admin", now).unwrap().after;
        let expiry = store.set_expiry(&enabled.id, Expiry::Days(90), now).unwrap().after;
        assert_eq!(expiry.agent_binding.as_deref(), Some("ci-bot"));
        let mut req = new_key("z", Expiry::Days(30));
        req.env = "production".into();
        assert_eq!(store.create(req, now).err(), Some(McpKeyError::InvalidEnv));
        for bad in [Expiry::Days(0), Expiry::Days(31), Expiry::Days(-30), Expiry::At(now), Expiry::At(now - Duration::days(1)),
            Expiry::At(now + Duration::days(MAX_CUSTOM_EXPIRY_DAYS + 1))] {
            assert_eq!(store.create(new_key("e", bad), now).err(), Some(McpKeyError::InvalidExpiry), "{bad:?}");
        }
        let never = store.create(new_key("never", Expiry::Never), now).unwrap();
        assert_eq!(never.key.expires_at, None);
        assert_eq!(never.key.state(now + Duration::days(36500)), KeyState::Active);
        let custom = now + Duration::days(45);
        let at = store.create(new_key("custom", Expiry::At(custom)), now).unwrap();
        assert_eq!(ks::timestamp(at.key.expires_at.unwrap()), ks::timestamp(custom));
    }

    #[test]
    fn every_mutation_is_versioned_and_reports_before_and_after() {
        let (home, store) = store();
        let now = Utc::now();
        let created = store.create(new_key("ops", Expiry::Days(30)), now).unwrap();
        let id = created.key.id.clone();
        let change = store.set_disabled(&id, true, "admin-2", now).unwrap();
        assert!(change.before.disabled_at.is_none());
        assert_eq!(change.after.disabled_by.as_deref(), Some("admin-2"));
        assert_eq!((change.before.version, change.after.version), (1, 2));
        assert_eq!(ks::lookup(home.path(), &created.secret).unwrap().unwrap().state(now), KeyState::Disabled);
        // Disabling twice keeps the first actor and time and writes nothing.
        let again = store.set_disabled(&id, true, "admin-3", now + Duration::minutes(1)).unwrap();
        assert_eq!(again.after.disabled_by.as_deref(), Some("admin-2"));
        assert!(!again.wrote());
        let enabled = store.set_disabled(&id, false, "admin-2", now).unwrap();
        assert!(enabled.after.disabled_at.is_none() && enabled.after.disabled_by.is_none());
        assert_eq!(enabled.after.version, 3);
        let never = store.set_expiry(&id, Expiry::Never, now).unwrap();
        assert_eq!(never.after.expires_at, None);
        let change = store.set_expiry(&id, Expiry::Days(365), now).unwrap();
        assert_eq!(change.before.expires_at, None);
        // The compensation puts the expiry back.
        assert_eq!(store.revert(&change).unwrap(), Revert::Reverted);
        assert_eq!(store.get(&id).unwrap().expires_at, None);
        let deleted = store.delete(&id, "admin-2", now).unwrap();
        assert_eq!(deleted.before.id, id);
        assert!(deleted.after.deleted_at.is_some());
        assert_eq!(store.get(&id).err(), Some(McpKeyError::NotFound));
        assert!(store.list().unwrap().is_empty());
        assert_eq!(store.set_disabled(&id, false, "admin-2", now).err(), Some(McpKeyError::NotFound));
        assert_eq!(store.set_expiry("mk_missing", Expiry::Days(30), now).err(), Some(McpKeyError::NotFound));
        // The tombstone keeps refusing the secret.
        assert_eq!(ks::lookup(home.path(), &created.secret).unwrap().unwrap().state(now), KeyState::Deleted);
        assert!(store.contains_hash(&ks::digest(&created.secret)).unwrap());
    }

    /// H2: Admin A renames, Admin B deletes, A's audit fails — A's
    /// compensation must not resurrect the key.
    #[test]
    fn compensation_never_undoes_a_later_disable_or_delete() {
        let (home, store) = store();
        let now = Utc::now();
        let created = store.create(new_key("shared", Expiry::Days(30)), now).unwrap();
        let id = created.key.id.clone();
        let rename = store.rename(&id, "renamed by A").unwrap();
        store.delete(&id, "admin-b", now).unwrap();
        assert_eq!(store.revert(&rename).unwrap(), Revert::Superseded);
        let row = store.get_any(&id).unwrap();
        assert!(row.deleted_at.is_some(), "still deleted");
        assert_eq!(row.name, "renamed by A", "the row was left alone");
        assert_eq!(ks::lookup(home.path(), &created.secret).unwrap().unwrap().state(now), KeyState::Deleted);

        // A disable after an enable: undoing the enable is superseded and
        // B's disable (actor, time) stays.
        let other = store.create(new_key("other", Expiry::Days(30)), now).unwrap();
        store.set_disabled(&other.key.id, true, "admin-a", now).unwrap();
        let enable = store.set_disabled(&other.key.id, false, "admin-a", now).unwrap();
        store.set_disabled(&other.key.id, true, "admin-b", now + Duration::seconds(5)).unwrap();
        assert_eq!(store.revert(&enable).unwrap(), Revert::Superseded);
        assert_eq!(store.get(&other.key.id).unwrap().disabled_by.as_deref(), Some("admin-b"));
        // An expiry change superseded by a later rename keeps both changes.
        let third = store.create(new_key("third", Expiry::Days(30)), now).unwrap();
        let expiry = store.set_expiry(&third.key.id, Expiry::Never, now).unwrap();
        store.rename(&third.key.id, "third renamed").unwrap();
        assert_eq!(store.revert(&expiry).unwrap(), Revert::Superseded);
        let row = store.get(&third.key.id).unwrap();
        assert_eq!((row.expires_at, row.name.as_str()), (None, "third renamed"));
        // A compensation touches only its own columns.
        let fourth = store.create(new_key("fourth", Expiry::Days(30)), now).unwrap();
        let rename = store.rename(&fourth.key.id, "fourth renamed").unwrap();
        assert_eq!(store.revert(&rename).unwrap(), Revert::Reverted);
        let row = store.get(&fourth.key.id).unwrap();
        assert_eq!(row.name, "fourth");
        assert_eq!(row.expires_at, fourth.key.expires_at);
        assert_eq!(row.version, 3);
        // Tightening is never compensated.
        let disable = store.set_disabled(&fourth.key.id, true, "admin", now).unwrap();
        assert_eq!(store.revert(&disable).unwrap(), Revert::Superseded);
        assert!(store.get(&fourth.key.id).unwrap().disabled_at.is_some());
    }

    #[test]
    fn a_compensation_from_a_stale_view_is_superseded() {
        let (home, store) = store();
        let created = store.create(new_key("race", Expiry::Days(30)), Utc::now()).unwrap();
        // Another handle bumps the version between our read and write.
        let conn = ks::open_for_gateway(home.path()).unwrap();
        conn.execute("UPDATE mcp_api_keys SET version = version + 1 WHERE id = ?1", [&created.key.id]).unwrap();
        // The mutation reads the current version inside its own transaction,
        // so it applies on top; the compensation of the earlier view does not.
        let stale = KeyChange {
            kind: ChangeKind::Rename,
            before: created.key.clone(),
            after: StoredKey { version: created.key.version + 1, name: "x".into(), ..created.key.clone() },
        };
        store.rename(&created.key.id, "fresh").unwrap();
        assert_eq!(store.revert(&stale).unwrap(), Revert::Superseded);
        assert_eq!(store.get(&created.key.id).unwrap().name, "fresh");
    }

    #[test]
    fn discarding_an_unaudited_creation_removes_or_tombstones_it() {
        let (_home, store) = store();
        let now = Utc::now();
        let created = store.create(new_key("unaudited", Expiry::Days(30)), now).unwrap();
        assert_eq!(store.discard_created(&created.key, now).unwrap(), Revert::Reverted);
        assert_eq!(store.get_any(&created.key.id).err(), Some(McpKeyError::NotFound));
        let created = store.create(new_key("touched", Expiry::Days(30)), now).unwrap();
        store.rename(&created.key.id, "touched again").unwrap();
        assert_eq!(store.discard_created(&created.key, now).unwrap(), Revert::Superseded);
        assert!(store.get_any(&created.key.id).unwrap().deleted_at.is_some(), "tombstoned, never live");
    }

    fn legacy<'a>(raw: &'a str, created_at: DateTime<Utc>, label: Option<&'a str>) -> LegacyKey<'a> {
        LegacyKey {
            raw,
            client_id: "claude-desktop",
            is_external: true,
            scopes: vec!["memory:read".into(), "future:scope".into()],
            created_at,
            label,
            legacy_entry_hash: "f".repeat(64),
            agent_binding: None,
        }
    }

    /// Third review (High): a migrated entry carries a binding only when the
    /// caller hands it one, and only its own client id on an internal key.
    #[test]
    fn legacy_import_binding_is_the_entrys_own_internal_client_id_or_nothing() {
        let (_home, store) = store();
        let now = Utc::now();
        let a = "ddc_prod_0123456789abcdef0123456789abcd01";
        let b = "ddc_prod_0123456789abcdef0123456789abcd02";
        let mut entry = legacy(a, now, None);
        entry.client_id = "sales";
        entry.is_external = false;
        entry.scopes = vec!["memory:read".into()];
        entry.agent_binding = Some("sales");
        let ImportOutcome::Inserted(bound) = store.import_legacy(&entry, now).unwrap() else { panic!() };
        assert_eq!(bound.agent_binding.as_deref(), Some("sales"));
        let mut entry = legacy(b, now, None);
        entry.client_id = "Sales Team";
        entry.is_external = false;
        assert_eq!(
            match store.import_legacy(&entry, now).unwrap() { ImportOutcome::Inserted(k) => k.agent_binding, _ => panic!() },
            None,
            "no binding unless the migration found the exact directory id"
        );
        // Existing directory ids may contain uppercase letters; binding is exact, without case folding.
        let mut entry = legacy("ddc_prod_0123456789abcdef0123456789abcd04", now, None);
        entry.client_id = "Sales";
        entry.is_external = false;
        entry.scopes = vec!["memory:read".into()];
        entry.agent_binding = Some("Sales");
        let ImportOutcome::Inserted(bound) = store.import_legacy(&entry, now).unwrap() else { panic!() };
        assert_eq!(bound.client_id, "Sales");
        assert_eq!(bound.agent_binding.as_deref(), Some("Sales"));
        assert!(!duduclaw_core::is_valid_agent_id("Sales Team"));
        for (client, external, binding) in [
            ("sales", false, "other"), ("sales", true, "sales"), ("Sales Team", false, "Sales Team"),
            ("Sales", false, "sales"),
        ] {
            let mut entry = legacy("ddc_prod_0123456789abcdef0123456789abcd03", now, None);
            entry.client_id = client;
            entry.is_external = external;
            entry.scopes = vec!["memory:read".into()];
            entry.agent_binding = Some(binding);
            assert_eq!(store.import_legacy(&entry, now).err(), Some(McpKeyError::InvalidClientId), "{client} {binding}");
        }
    }

    #[test]
    fn legacy_import_keeps_behaviour_and_is_idempotent() {
        let (home, store) = store();
        let now = Utc::now();
        let created_at = ks::parse_timestamp("2026-11-01T08:00:00Z").unwrap();
        let raw = "ddc_dev_0123456789abcdef0123456789abcdef";
        let ImportOutcome::Inserted(key) = store.import_legacy(&legacy(raw, created_at, None), now).unwrap() else {
            panic!("first import inserts");
        };
        assert!(key.name.starts_with("migrated-"));
        assert_eq!(key.name.chars().count(), "migrated-".len() + 8);
        assert_eq!(ks::timestamp(key.created_at), "2026-11-01T08:00:00Z");
        // L2: exactly the instant the old `num_days() > 30` rule refused it.
        let expires = key.expires_at.unwrap();
        assert_eq!(ks::timestamp(expires), "2026-12-02T08:00:00Z");
        assert!(ks::legacy_age_accepts(created_at, expires - Duration::seconds(1)));
        assert!(!ks::legacy_age_accepts(created_at, expires));
        assert_eq!(key.state(expires - Duration::seconds(1)), KeyState::Active);
        assert_eq!(key.state(expires), KeyState::Expired);
        assert_eq!(key.scopes, vec!["memory:read", "future:scope"], "scopes kept verbatim");
        assert_eq!(key.env, "dev");
        assert!(key.migrated && !key.expiry_reviewed && key.created_by.is_none());
        assert_eq!(store.import_legacy(&legacy(raw, created_at, None), now).unwrap(), ImportOutcome::AlreadyPresent);
        assert_eq!(store.list().unwrap().len(), 1);
        assert!(!contains(&file_bytes(home.path()), raw));
        // A deleted key's tombstone wins over a stale config copy.
        store.delete(&key.id, "admin", now).unwrap();
        assert_eq!(store.import_legacy(&legacy(raw, created_at, None), now).unwrap(), ImportOutcome::AlreadyPresent);
        assert!(store.list().unwrap().is_empty());
        assert_eq!(store.import_legacy(&legacy("ddc_prod_short", created_at, None), now).err(), Some(McpKeyError::InvalidKey));
    }

    #[test]
    fn legacy_label_is_used_when_valid_and_free() {
        let (_home, store) = store();
        let now = Utc::now();
        store.create(new_key("taken", Expiry::Days(30)), now).unwrap();
        let a = "ddc_prod_0123456789abcdef0123456789abcde1";
        let b = "ddc_prod_0123456789abcdef0123456789abcde2";
        let c = "ddc_prod_0123456789abcdef0123456789abcde3";
        let name = |outcome: ImportOutcome| match outcome { ImportOutcome::Inserted(k) => k.name, other => panic!("{other:?}") };
        assert_eq!(name(store.import_legacy(&legacy(a, now, Some(" Claude Desktop ")), now).unwrap()), "Claude Desktop");
        assert!(name(store.import_legacy(&legacy(b, now, Some("TAKEN")), now).unwrap()).starts_with("migrated-"));
        assert!(name(store.import_legacy(&legacy(c, now, Some("bad\u{7}label")), now).unwrap()).starts_with("migrated-"));
    }

    #[test]
    fn the_pending_set_is_recorded_once_and_completion_needs_it_empty() {
        let (home, store) = store();
        let now = Utc::now();
        let a = ks::digest("ddc_prod_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let b = ks::digest("ddc_prod_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        assert_eq!(store.migration_state().unwrap(), MigrationState::NotStarted);
        assert_eq!(store.begin_legacy_migration(std::slice::from_ref(&a), now).unwrap(), MigrationState::Pending);
        // A later call never extends the set.
        assert_eq!(store.begin_legacy_migration(std::slice::from_ref(&b), now).unwrap(), MigrationState::Pending);
        assert_eq!(store.pending_hashes().unwrap(), HashSet::from([a.clone()]));
        assert!(!store.complete_migration(now).unwrap(), "refused while pending");
        store.clear_pending(&a).unwrap();
        assert!(store.complete_migration(now).unwrap());
        assert_eq!(store.migration_state().unwrap(), MigrationState::Complete);
        assert_eq!(store.begin_legacy_migration(std::slice::from_ref(&b), now).unwrap(), MigrationState::Complete);
        assert!(store.pending_hashes().unwrap().is_empty());
        assert_eq!(ks::resolve_hash(home.path(), &b).unwrap(), ks::Authority::Unknown);
        assert!(store.note_ignored(&b, "ddc_prod_…bbbb", now).unwrap());
        assert!(!store.note_ignored(&b, "ddc_prod_…bbbb", now).unwrap(), "noted once");
    }

    #[test]
    fn internal_key_records_are_kept_and_pruned() {
        let (home, store) = store();
        let raw = "ddc_prod_cccccccccccccccccccccccccccccccc";
        let old = "ddc_prod_dddddddddddddddddddddddddddddddd";
        store.record_internal_key(raw, Utc::now()).unwrap();
        store.record_internal_key(old, Utc::now() - Duration::days(20)).unwrap();
        assert!(matches!(ks::resolve(home.path(), raw).unwrap(), ks::Authority::Internal(_)));
        assert_eq!(store.retain_internal_keys(&HashSet::from([ks::digest(raw)])).unwrap(), 1);
        assert_eq!(ks::resolve(home.path(), old).unwrap(), ks::Authority::Unknown);
        assert_eq!(store.record_internal_key("not-a-key", Utc::now()).err(), Some(McpKeyError::InvalidKey));
    }

    #[test]
    fn migration_hint_shows_until_dismissed_or_every_migrated_expiry_is_reviewed() {
        let (_home, store) = store();
        let now = Utc::now();
        assert!(!store.migration_hint_pending().unwrap());
        store.create(new_key("fresh", Expiry::Days(30)), now).unwrap();
        assert!(!store.migration_hint_pending().unwrap(), "new keys never raise it");
        let raw = "ddc_prod_0123456789abcdef0123456789abcdef";
        let ImportOutcome::Inserted(key) = store.import_legacy(&legacy(raw, now, None), now).unwrap() else { panic!() };
        assert!(store.migration_hint_pending().unwrap());
        store.set_expiry(&key.id, Expiry::Days(90), now).unwrap();
        assert!(!store.migration_hint_pending().unwrap(), "reviewed");
        let raw2 = "ddc_prod_0123456789abcdef0123456789abcde0";
        store.import_legacy(&legacy(raw2, now, None), now).unwrap();
        assert!(store.migration_hint_pending().unwrap());
        let previous = store.dismiss_migration_hint(now).unwrap();
        assert!(!store.migration_hint_pending().unwrap());
        // An unaudited dismissal is put back.
        assert_eq!(store.restore_migration_hint(previous.as_deref(), now).unwrap(), Revert::Reverted);
        assert!(store.migration_hint_pending().unwrap());
    }

    #[test]
    fn expiry_notices_are_claimed_attempted_and_delivered_once() {
        let (_home, store) = store();
        let now = Utc::now();
        let key = store.create(new_key("soon", Expiry::Days(30)), now).unwrap().key;
        store.create(new_key("forever", Expiry::Never), now).unwrap();
        let disabled = store.create(new_key("off", Expiry::Days(30)), now).unwrap().key;
        store.set_disabled(&disabled.id, true, "admin", now).unwrap();
        let at = |days_left: i64| key.expires_at.unwrap() - Duration::days(days_left);
        assert!(store.due_notices(at(8)).unwrap().is_empty());
        let due = store.due_notices(at(7)).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!((due[0].0.id.as_str(), due[0].1), (key.id.as_str(), NoticeThreshold::SevenDays));
        // Claim, fail: released with a retry delay.
        let claim = store.claim_notice(&due[0].0, NoticeThreshold::SevenDays, at(7)).unwrap().expect("claimed");
        assert!(store.claim_notice(&due[0].0, NoticeThreshold::SevenDays, at(7)).unwrap().is_none(), "claimed once");
        assert!(store.due_notices(at(7)).unwrap().is_empty(), "in flight");
        let next = store.notice_failed(&due[0].0, NoticeThreshold::SevenDays, &claim, at(7)).unwrap().unwrap();
        assert_eq!(next - at(7), Duration::seconds(NOTICE_RETRY_BASE_SECS));
        assert!(store.due_notices(at(7) + Duration::seconds(10)).unwrap().is_empty(), "backoff");
        let later = next + Duration::seconds(1);
        let due = store.due_notices(later).unwrap();
        assert_eq!(due.len(), 1, "retried after the delay");
        let claim = store.claim_notice(&due[0].0, NoticeThreshold::SevenDays, later).unwrap().expect("claimed");
        assert!(store.notice_delivered(&due[0].0, NoticeThreshold::SevenDays, &claim, later).unwrap());
        assert_eq!(store.notice_state(&key, NoticeThreshold::SevenDays).unwrap(), Some(("delivered".into(), 2)));
        assert!(store.due_notices(at(3)).unwrap().is_empty());
        let due = store.due_notices(at(1)).unwrap();
        assert_eq!(due[0].1, NoticeThreshold::OneDay);
        let claim = store.claim_notice(&due[0].0, NoticeThreshold::OneDay, at(1)).unwrap().expect("claimed");
        assert!(store.notice_delivered(&due[0].0, NoticeThreshold::OneDay, &claim, at(1)).unwrap());
        assert!(store.due_notices(key.expires_at.unwrap() - Duration::hours(12)).unwrap().is_empty());
        // Past expiry nothing more is owed.
        assert!(store.due_notices(at(-1)).unwrap().is_empty());
        // A new expiry re-arms both notices; a claim against the old expiry fails.
        let stale = store.get(&key.id).unwrap();
        store.set_expiry(&key.id, Expiry::Days(90), now).unwrap();
        assert!(store.claim_notice(&stale, NoticeThreshold::OneDay, now).unwrap().is_none());
        let fresh = store.get(&key.id).unwrap();
        assert_eq!(store.notice_state(&fresh, NoticeThreshold::SevenDays).unwrap(), None);
    }

    #[test]
    fn a_notice_that_can_no_longer_be_met_is_closed_without_sending() {
        let (_home, store) = store();
        let now = Utc::now();
        let key = store.create(new_key("late", Expiry::At(now + Duration::days(5))), now).unwrap().key;
        let due = store.due_notices(now).unwrap();
        let claim = store.claim_notice(&due[0].0, NoticeThreshold::SevenDays, now).unwrap().expect("claimed");
        store.notice_failed(&key, NoticeThreshold::SevenDays, &claim, now).unwrap();
        // Now inside the last day: the pending 7-day notice is closed, the
        // 1-day one is due.
        let inside = key.expires_at.unwrap() - Duration::hours(12);
        let due = store.due_notices(inside).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].1, NoticeThreshold::OneDay);
        assert_eq!(store.notice_state(&key, NoticeThreshold::SevenDays).unwrap().unwrap().0, "closed");
        let claim = store.claim_notice(&due[0].0, NoticeThreshold::OneDay, inside).unwrap().expect("claimed");
        store.notice_failed(&key, NoticeThreshold::OneDay, &claim, inside).unwrap();
        // The key is deleted before the retry: closed, never sent.
        store.delete(&key.id, "admin", inside).unwrap();
        assert!(store.due_notices(inside + Duration::hours(1)).unwrap().is_empty());
        assert_eq!(store.notice_state(&key, NoticeThreshold::OneDay).unwrap().unwrap().0, "closed");
        assert!(store.claim_notice(&key, NoticeThreshold::OneDay, inside + Duration::hours(1)).unwrap().is_none());
    }

    /// Third review (Low): confirm and finalise are bound to ONE live claim.
    /// A sweep whose lease lapsed and whose notice another sweep re-claimed
    /// can neither confirm, mark delivered nor release it; a finalise that
    /// matches nothing reports "not delivered".
    #[test]
    fn finalisation_is_bound_to_one_live_claim() {
        let (_home, store) = store();
        let now = Utc::now();
        let key = store.create(new_key("lease", Expiry::At(now + Duration::days(6))), now).unwrap().key;
        let first = store.claim_notice(&key, NoticeThreshold::SevenDays, now).unwrap().expect("claimed");
        assert!(store.confirm_notice_claim(&key, NoticeThreshold::SevenDays, &first, now).unwrap());
        // The first sweep stalls past its lease; a second sweep claims it.
        let lapsed = now + Duration::seconds(NOTICE_CLAIM_LEASE_SECS + 1);
        assert!(!store.confirm_notice_claim(&key, NoticeThreshold::SevenDays, &first, lapsed).unwrap(), "lease over");
        let second = store.claim_notice(&key, NoticeThreshold::SevenDays, lapsed).unwrap().expect("re-claimed");
        assert_ne!(first, second, "every claim has its own token");
        // The stale sweep's late steps change nothing.
        assert!(!store.notice_delivered(&key, NoticeThreshold::SevenDays, &first, lapsed).unwrap());
        assert_eq!(store.notice_failed(&key, NoticeThreshold::SevenDays, &first, lapsed).unwrap(), None);
        assert!(!store.confirm_notice_claim(&key, NoticeThreshold::SevenDays, &first, lapsed).unwrap());
        // The live claim is untouched by those and finalises once.
        assert!(store.confirm_notice_claim(&key, NoticeThreshold::SevenDays, &second, lapsed).unwrap());
        assert!(store.notice_delivered(&key, NoticeThreshold::SevenDays, &second, lapsed).unwrap());
        assert!(!store.notice_delivered(&key, NoticeThreshold::SevenDays, &second, lapsed).unwrap(), "only once");
        assert_eq!(store.notice_state(&key, NoticeThreshold::SevenDays).unwrap().unwrap().0, "delivered");
        // A key change after the final confirm (here a rename) makes the
        // finalise match nothing: reported as not delivered.
        let other = store.create(new_key("moved", Expiry::At(now + Duration::days(6))), now).unwrap().key;
        let claim = store.claim_notice(&other, NoticeThreshold::SevenDays, now).unwrap().expect("claimed");
        assert!(store.confirm_notice_claim(&other, NoticeThreshold::SevenDays, &claim, now).unwrap());
        store.rename(&other.id, "moved again").unwrap();
        assert!(!store.notice_delivered(&other, NoticeThreshold::SevenDays, &claim, now).unwrap());
        assert_eq!(store.notice_state(&other, NoticeThreshold::SevenDays).unwrap().unwrap().0, "pending");
    }

    /// The raw retry schedule of one notice (tests only).
    fn next_attempt_at(store: &McpKeyStore, key: &StoredKey, threshold: NoticeThreshold) -> Option<String> {
        store
            .conn()
            .query_row(
                "SELECT next_attempt_at FROM mcp_api_key_notices WHERE key_id = ?1 AND threshold = ?2",
                params![key.id, threshold.as_str()],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// Fourth review (Low): `notice_failed` is one CAS with the same
    /// conditions as `notice_delivered`. A failure reported after the lease
    /// lapsed, for a key that changed version, or for a disabled key is
    /// stale: nothing is cleared or rescheduled.
    #[test]
    fn notice_failed_is_a_cas_bound_to_the_live_claim_and_key_version() {
        let (_home, store) = store();
        let now = Utc::now();
        let t = NoticeThreshold::SevenDays;
        // 1. The send hung past the lease and then failed.
        let key = store.create(new_key("hung", Expiry::At(now + Duration::days(6))), now).unwrap().key;
        let claim = store.claim_notice(&key, t, now).unwrap().expect("claimed");
        let lapsed = now + Duration::seconds(NOTICE_CLAIM_LEASE_SECS + 1);
        assert_eq!(store.notice_failed(&key, t, &claim, lapsed).unwrap(), None, "lease over: stale");
        assert_eq!(next_attempt_at(&store, &key, t), None, "no retry pushed out");
        // The lapsed claim is simply re-claimable, with no backoff in the way.
        let again = store.claim_notice(&key, t, lapsed).unwrap().expect("re-claimed after the lease");
        assert_eq!(store.notice_state(&key, t).unwrap(), Some(("pending".into(), 2)));
        // The live claim's failure is recorded with the backoff.
        let next = store.notice_failed(&key, t, &again, lapsed).unwrap().expect("live claim");
        assert_eq!(next - lapsed, Duration::seconds(NOTICE_RETRY_BASE_SECS * 2));
        assert_eq!(next_attempt_at(&store, &key, t), Some(ks::timestamp(next)));

        // 2. The key changed version (here: renamed) after the claim.
        let moved = store.create(new_key("moved", Expiry::At(now + Duration::days(6))), now).unwrap().key;
        let claim = store.claim_notice(&moved, t, now).unwrap().expect("claimed");
        store.rename(&moved.id, "moved on").unwrap();
        assert_eq!(store.notice_failed(&moved, t, &claim, now).unwrap(), None, "old key version: stale");
        assert_eq!(next_attempt_at(&store, &moved, t), None);
        // The claim was not released by the stale report: it still holds.
        assert!(store.claim_notice(&moved, t, now).unwrap().is_none());
        // The current row's claim (after the lease) records normally.
        let current = store.get(&moved.id).unwrap();
        let claim = store.claim_notice(&current, t, lapsed).unwrap().expect("re-claimed");
        assert!(store.notice_failed(&current, t, &claim, lapsed).unwrap().is_some());

        // 3. The key was disabled after the claim (the notice is closed too).
        let off = store.create(new_key("off", Expiry::At(now + Duration::days(6))), now).unwrap().key;
        let claim = store.claim_notice(&off, t, now).unwrap().expect("claimed");
        let disabled = store.set_disabled(&off.id, true, "admin", now).unwrap().after;
        for view in [&off, &disabled] {
            assert_eq!(store.notice_failed(view, t, &claim, now).unwrap(), None, "disabled key: stale");
        }
        assert_eq!(next_attempt_at(&store, &off, t), None);
        assert_eq!(store.notice_state(&off, t).unwrap().unwrap().0, "closed");
    }

    /// Fourth review (Medium 1): deleting an employee tombstones every key
    /// bound to it — only those — in one transaction.
    #[test]
    fn revoking_an_employees_keys_tombstones_only_its_bound_keys() {
        let (home, store) = store();
        let now = Utc::now();
        let employee_key = |name: &str, client: &str| NewMcpKey {
            name: name.into(),
            client_id: client.into(),
            binding: KeyBinding::Employee,
            env: "prod".into(),
            scopes: vec!["memory:read".into()],
            expiry: Expiry::Never,
            created_by: Some("admin".into()),
        };
        let a1 = store.create(employee_key("alice 1", "alice"), now).unwrap();
        let a2 = store.create(employee_key("alice 2", "alice"), Utc::now()).unwrap();
        let bob = store.create(employee_key("bob", "bob"), now).unwrap();
        let integration = store.create(new_key("integration", Expiry::Days(30)), now).unwrap();
        let gone = store.create(employee_key("alice old", "alice"), now).unwrap();
        store.delete(&gone.key.id, "admin", now).unwrap();
        let claimed = store.claim_notice(&a1.key, NoticeThreshold::SevenDays, now).unwrap();
        assert!(claimed.is_none(), "no expiry, no notice");

        let changes = store.revoke_employee_keys("alice", EMPLOYEE_GONE_ACTOR, now).unwrap();
        let mut ids: Vec<&str> = changes.iter().map(|c| c.before.id.as_str()).collect();
        ids.sort();
        let mut expected = vec![a1.key.id.as_str(), a2.key.id.as_str()];
        expected.sort();
        assert_eq!(ids, expected, "only alice's live keys");
        for change in &changes {
            assert_eq!(change.kind, ChangeKind::Delete);
            assert!(change.after.deleted_at.is_some());
            assert_eq!(change.after.version, change.before.version + 1);
        }
        for secret in [&a1.secret, &a2.secret] {
            let row = ks::lookup(home.path(), secret).unwrap().unwrap();
            assert_eq!(row.state(now), KeyState::Deleted, "tombstone, digest kept");
            assert!(store.contains_hash(&ks::digest(secret)).unwrap());
        }
        for untouched in [&bob.secret, &integration.secret] {
            assert_eq!(ks::lookup(home.path(), untouched).unwrap().unwrap().state(now), KeyState::Active);
        }
        // Idempotent; and the revocation is never compensated.
        assert!(store.revoke_employee_keys("alice", EMPLOYEE_GONE_ACTOR, now).unwrap().is_empty());
        assert_eq!(store.revert(&changes[0]).unwrap(), Revert::Superseded);
        // A new key for a later employee with the same id is a new row and works.
        let fresh = store.create(employee_key("alice new", "alice"), Utc::now()).unwrap();
        assert_eq!(ks::lookup(home.path(), &fresh.secret).unwrap().unwrap().state(Utc::now()), KeyState::Active);
    }

    /// Fourth review (Medium 2): the store never binds a legacy entry whose
    /// scopes are not all per-employee scopes; unbound, its scopes are kept
    /// verbatim (nothing dropped, nothing downgraded).
    #[test]
    fn a_legacy_binding_requires_per_employee_scopes() {
        let (_home, store) = store();
        let now = Utc::now();
        assert!(employee_key_scopes_allowed(&["memory:read".into(), " google:read ".into(), "".into()]));
        assert!(employee_key_scopes_allowed(&[]));
        for bad in ["admin", "team:handoff", "knowledge:read", "future:scope"] {
            assert!(!employee_key_scopes_allowed(&["memory:read".into(), bad.into()]), "{bad}");
            let mut entry = legacy("ddc_prod_0123456789abcdef0123456789abcd11", now, None);
            entry.client_id = "alice";
            entry.is_external = false;
            entry.scopes = vec!["memory:read".into(), bad.into()];
            entry.agent_binding = Some("alice");
            assert_eq!(store.import_legacy(&entry, now).err(), Some(McpKeyError::ScopeNotGrantable), "{bad}");
        }
        let mut entry = legacy("ddc_prod_0123456789abcdef0123456789abcd12", now, None);
        entry.client_id = "alice";
        entry.is_external = false;
        entry.scopes = vec!["admin".into()];
        let ImportOutcome::Inserted(key) = store.import_legacy(&entry, now).unwrap() else { panic!() };
        assert_eq!((key.agent_binding, key.scopes), (None, vec!["admin".to_string()]));
    }

    /// Second review (Low): an expiry change, disable or delete closes the
    /// key's open notices in its own transaction, and the pre-send CAS
    /// (`confirm_notice_claim`: id + expiry + row version + live claim)
    /// refuses a claimed notice whose key moved on — a rename included, in
    /// which case the claim is only released and the notice stays owed.
    #[test]
    fn a_mutation_after_the_claim_closes_or_invalidates_the_notice() {
        let (_home, store) = store();
        let now = Utc::now();
        type Mutate = fn(&McpKeyStore, &str, DateTime<Utc>);
        let mutations: [(&str, Mutate, &str); 4] = [
            ("expiry", |s, id, now| { s.set_expiry(id, Expiry::Days(90), now).unwrap(); }, "closed"),
            ("disable", |s, id, now| { s.set_disabled(id, true, "admin", now).unwrap(); }, "closed"),
            ("delete", |s, id, now| { s.delete(id, "admin", now).unwrap(); }, "closed"),
            ("rename", |s, id, _| { s.rename(id, "renamed").unwrap(); }, "pending"),
        ];
        for (label, mutate, state_after) in mutations {
            let key = store.create(new_key(label, Expiry::At(now + Duration::days(6))), now).unwrap().key;
            let claim = store.claim_notice(&key, NoticeThreshold::SevenDays, now).unwrap().expect(label);
            assert!(store.confirm_notice_claim(&key, NoticeThreshold::SevenDays, &claim, now).unwrap(), "{label}: unchanged key is sent");
            mutate(&store, &key.id, now);
            assert!(!store.confirm_notice_claim(&key, NoticeThreshold::SevenDays, &claim, now).unwrap(), "{label}: dropped");
            assert_eq!(store.notice_state(&key, NoticeThreshold::SevenDays).unwrap().unwrap().0, state_after, "{label}");
            // A late "delivered" from the stale sweep changes nothing and says so.
            assert!(!store.notice_delivered(&key, NoticeThreshold::SevenDays, &claim, now).unwrap(), "{label}");
            assert_eq!(store.notice_state(&key, NoticeThreshold::SevenDays).unwrap().unwrap().0, state_after, "{label}");
        }
        // The renamed key's notice was released, so it is owed again with the
        // current row (and its new version) and confirms.
        let renamed = store.list().unwrap().into_iter().find(|k| k.name == "renamed").unwrap();
        let due = store.due_notices(now).unwrap();
        assert!(due.iter().any(|(k, t)| k.id == renamed.id && *t == NoticeThreshold::SevenDays));
        let claim = store.claim_notice(&renamed, NoticeThreshold::SevenDays, now).unwrap().expect("re-claimed");
        assert!(store.confirm_notice_claim(&renamed, NoticeThreshold::SevenDays, &claim, now).unwrap());
        // Without a live claim nothing is confirmed (a token from another
        // notice's claim does not fit this one).
        let other = store.create(new_key("unclaimed", Expiry::At(now + Duration::days(6))), now).unwrap().key;
        assert!(!store.confirm_notice_claim(&other, NoticeThreshold::SevenDays, &claim, now).unwrap());
    }

    #[test]
    fn importer_catalog_matches_a_fresh_store() {
        fn schema(conn: &Connection) -> Vec<(String, String, String, Option<String>)> {
            conn.prepare("SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY type, name, tbl_name, sql")
                .unwrap()
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        }
        let catalog = include_str!("../../duduclaw-import/src/mcp-keys-v182.sql");
        assert!(catalog.ends_with(ks::SCHEMA_SQL), "the catalog is a byte copy of SCHEMA_SQL after its header");
        let (home, store) = store();
        drop(store);
        let actual = Connection::open(ks::store_path(home.path())).unwrap();
        let expected = Connection::open_in_memory().unwrap();
        expected.execute_batch(catalog).unwrap();
        assert_eq!(schema(&actual), schema(&expected));
        let version = |c: &Connection| c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(version(&actual), ks::SCHEMA_VERSION);
        assert_eq!(version(&expected), ks::SCHEMA_VERSION);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_store_is_refused() {
        let home = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(home.path().join("elsewhere.db"), ks::store_path(home.path())).unwrap();
        assert!(McpKeyStore::open(home.path()).is_err());
        assert!(!home.path().join("elsewhere.db").exists());
    }
}
