//! Revocable, epoch-bound capability lifecycle (PORTICO, arXiv:2606.22504).
//!
//! ## Why this exists
//!
//! [`crate::approval::ApprovalBroker`] answers a *one-shot* question — "may
//! this action happen once?". Its weakness (the exact PORTICO thesis): a
//! human approval is a **permanent** grant. Once approved, nothing ever
//! *takes the permission back*. An agent that was cleared to touch a
//! resource for one subgoal keeps that clearance for the rest of its life.
//!
//! PORTICO upgrades `request → decide` into `request → grant → invoke`:
//! approving mints an **epoch-bound capability handle** tied to a task
//! subgoal (a `scope_epoch` — a `task_id` / `session_id`). When the subgoal
//! closes, every handle in that epoch is **auto-revoked**; any later
//! `invoke` of the same handle is denied. Post-closure reuse is closed
//! (PORTICO's "10/10 blocked" acceptance).
//!
//! ## Lifecycle
//!
//! ```text
//!   ApprovalBroker.decide(approve) ──► grant(scope_epoch, ttl) ──► handle
//!                                            │
//!   tool executes ──► invoke(handle) ──► Ok / CapError (fail-closed)
//!                                            │
//!   subgoal done  ──► close_scope(epoch) ──► all handles revoked
//!                                            │
//!   invoke(handle) after closure ──► CapError::ScopeClosed  (always DENY)
//! ```
//!
//! ## Fail-closed conventions (the soul of this module)
//!
//! Every uncertain state is a **DENY**:
//! - Unknown / missing handle ⇒ [`CapError::NotFound`].
//! - Revoked handle ⇒ [`CapError::Revoked`].
//! - Past-TTL or unparseable `expires_at` ⇒ [`CapError::Expired`].
//! - Handle whose `scope_epoch` is closed ⇒ [`CapError::ScopeClosed`],
//!   *even if the row's own `revoked_at` was not yet stamped* (defence in
//!   depth: closure is authoritative, the per-row flag is a convenience).
//! - Granting **into** an already-closed scope is rejected up front
//!   ([`CapError::ScopeClosed`]) — a stale write can never mint a live
//!   handle after its subgoal ended.
//! - Any store/parse error propagates as [`CapError::Store`] — never a
//!   silent allow.
//!
//! ## Storage
//!
//! Shares the `approvals.db` file with [`crate::approval::ApprovalStore`]
//! (WAL supports concurrent connections) but owns two tables:
//! `capability_grants` and `capability_closed_scopes`. Mirrors the store
//! idioms elsewhere in the crate: `Mutex<Connection>`, WAL, `busy_timeout`,
//! self-healing schema, parameterized SQL only.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::approval::{ApprovalBroker, ApprovalId, DEFAULT_TTL_SECONDS};

// ── Errors ──────────────────────────────────────────────────

/// Why an [`CapabilityBroker::invoke`] / [`CapabilityBroker::grant`] was
/// refused. Every non-`Ok` outcome is a DENY — there is no "maybe".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapError {
    /// No grant with this handle id (never minted, or wrong id).
    NotFound,
    /// Handle was explicitly revoked (or its scope closed the row).
    Revoked,
    /// Past its TTL, or an unparseable `expires_at` (fail-closed).
    Expired,
    /// The handle's subgoal (`scope_epoch`) has been closed.
    ScopeClosed,
    /// Underlying store / serialization failure. Fail-closed: callers
    /// MUST treat this as a denial, never retry-into-allow.
    Store(String),
}

impl CapError {
    /// Short zh-TW reason for logs / channel surfaces.
    pub fn zh_reason(&self) -> String {
        match self {
            CapError::NotFound => "找不到此授權憑證（未曾核發或已作廢）".to_string(),
            CapError::Revoked => "授權憑證已被撤銷".to_string(),
            CapError::Expired => "授權憑證已逾期".to_string(),
            CapError::ScopeClosed => "子目標已關閉，授權已自動收回".to_string(),
            CapError::Store(e) => format!("授權存取錯誤（保守拒絕）：{e}"),
        }
    }
}

impl std::fmt::Display for CapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.zh_reason())
    }
}

impl std::error::Error for CapError {}

// ── Types ───────────────────────────────────────────────────

/// A minted, epoch-bound capability. Produced by [`CapabilityBroker::grant`]
/// after an approval is decided. The `handle_id` is the bearer token an
/// executing tool presents to [`CapabilityBroker::invoke`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapabilityGrant {
    /// Opaque handle id (UUIDv4). What tools present at invoke time.
    pub handle_id: String,
    /// The approval this grant descends from (audit link).
    pub approval_id: String,
    /// Agent the capability is issued to.
    pub granted_to: String,
    /// What the capability authorizes ("mcp_tool" | "bus_task" | ...).
    pub action_kind: String,
    /// The subgoal/epoch key (a `task_id` / `session_id`). Closing this
    /// epoch revokes every handle bound to it.
    pub scope_epoch: String,
    pub granted_at: String,
    pub expires_at: String,
    /// Set once the handle is revoked (explicitly or via scope closure).
    pub revoked_at: Option<String>,
    /// Reserved: a machine-readable predicate describing *when* the subgoal
    /// closes (e.g. "task:done", "session:end"). Today closure is driven by
    /// an explicit [`CapabilityBroker::close_scope`] call; this field lets a
    /// future evaluator auto-close without a schema change.
    pub closure_predicate: Option<String>,
}

impl CapabilityGrant {
    /// Parsed expiry instant. `None` (⇒ treated as expired) if unparseable.
    fn expires_at_dt(&self) -> Option<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(&self.expires_at)
            .ok()
            .map(|d| d.with_timezone(&Utc))
    }

    /// True if past TTL or the timestamp cannot be parsed (fail-closed).
    fn is_expired(&self, now: DateTime<Utc>) -> bool {
        match self.expires_at_dt() {
            Some(exp) => now >= exp,
            None => true, // unparseable ⇒ fail closed
        }
    }
}

// ── Store ───────────────────────────────────────────────────

/// SQLite persistence for capability grants + the closed-scope ledger.
pub struct CapabilityStore {
    conn: Mutex<Connection>,
    #[allow(dead_code)]
    db_path: Option<PathBuf>,
}

impl CapabilityStore {
    /// Open (or create) the store at `<home>/approvals.db` (shared file with
    /// the approval store; separate tables).
    pub fn open(home_dir: &Path) -> Result<Self, String> {
        let db_path = home_dir.join("approvals.db");
        let conn = Connection::open(&db_path).map_err(|e| format!("open capability store: {e}"))?;
        Self::init_schema(&conn)?;
        info!(?db_path, "CapabilityStore initialized");
        Ok(Self {
            conn: Mutex::new(conn),
            db_path: Some(db_path),
        })
    }

    /// In-memory store for tests.
    pub fn open_in_memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory().map_err(|e| format!("open in-memory: {e}"))?;
        Self::init_schema(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            db_path: None,
        })
    }

    fn init_schema(conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=5000;

             CREATE TABLE IF NOT EXISTS capability_grants (
                 handle_id         TEXT PRIMARY KEY,
                 approval_id       TEXT NOT NULL,
                 granted_to        TEXT NOT NULL,
                 action_kind       TEXT NOT NULL,
                 scope_epoch       TEXT NOT NULL,
                 granted_at        TEXT NOT NULL,
                 expires_at        TEXT NOT NULL,
                 revoked_at        TEXT,
                 closure_predicate TEXT
             );

             CREATE INDEX IF NOT EXISTS idx_cap_scope ON capability_grants(scope_epoch);
             CREATE INDEX IF NOT EXISTS idx_cap_agent ON capability_grants(granted_to);

             CREATE TABLE IF NOT EXISTS capability_closed_scopes (
                 scope_epoch TEXT PRIMARY KEY,
                 closed_at   TEXT NOT NULL
             );",
        )
        .map_err(|e| format!("init capability schema: {e}"))?;
        Ok(())
    }

    async fn insert(&self, g: &CapabilityGrant) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO capability_grants
                (handle_id, approval_id, granted_to, action_kind, scope_epoch,
                 granted_at, expires_at, revoked_at, closure_predicate)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                g.handle_id,
                g.approval_id,
                g.granted_to,
                g.action_kind,
                g.scope_epoch,
                g.granted_at,
                g.expires_at,
                g.revoked_at,
                g.closure_predicate,
            ],
        )
        .map_err(|e| format!("insert grant: {e}"))?;
        Ok(())
    }

    async fn get(&self, handle_id: &str) -> Result<Option<CapabilityGrant>, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT handle_id, approval_id, granted_to, action_kind, scope_epoch,
                    granted_at, expires_at, revoked_at, closure_predicate
             FROM capability_grants WHERE handle_id = ?1",
            params![handle_id],
            row_to_grant,
        )
        .optional()
        .map_err(|e| format!("get grant: {e}"))
    }

    async fn is_scope_closed(&self, scope_epoch: &str) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(1) FROM capability_closed_scopes WHERE scope_epoch = ?1",
                params![scope_epoch],
                |r| r.get(0),
            )
            .map_err(|e| format!("is_scope_closed: {e}"))?;
        Ok(n > 0)
    }

    /// Mark a scope closed and revoke all its still-live handles in one
    /// transaction. Returns the number of handles revoked.
    async fn close_scope(&self, scope_epoch: &str, at: &str) -> Result<u64, String> {
        let mut conn = self.conn.lock().await;
        let tx = conn
            .transaction()
            .map_err(|e| format!("close_scope tx: {e}"))?;
        tx.execute(
            "INSERT INTO capability_closed_scopes (scope_epoch, closed_at)
             VALUES (?1, ?2)
             ON CONFLICT(scope_epoch) DO NOTHING",
            params![scope_epoch, at],
        )
        .map_err(|e| format!("mark scope closed: {e}"))?;
        let revoked = tx
            .execute(
                "UPDATE capability_grants
                 SET revoked_at = ?1
                 WHERE scope_epoch = ?2 AND revoked_at IS NULL",
                params![at, scope_epoch],
            )
            .map_err(|e| format!("revoke scope handles: {e}"))?;
        tx.commit()
            .map_err(|e| format!("commit close_scope: {e}"))?;
        Ok(revoked as u64)
    }

    /// Stamp `revoked_at` on one handle if not already revoked. Returns rows
    /// affected (0 = missing or already revoked).
    async fn revoke(&self, handle_id: &str, at: &str) -> Result<usize, String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE capability_grants
             SET revoked_at = ?1
             WHERE handle_id = ?2 AND revoked_at IS NULL",
            params![at, handle_id],
        )
        .map_err(|e| format!("revoke handle: {e}"))
    }

    async fn list_for_scope(&self, scope_epoch: &str) -> Result<Vec<CapabilityGrant>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT handle_id, approval_id, granted_to, action_kind, scope_epoch,
                        granted_at, expires_at, revoked_at, closure_predicate
                 FROM capability_grants WHERE scope_epoch = ?1
                 ORDER BY granted_at ASC",
            )
            .map_err(|e| format!("prepare list_for_scope: {e}"))?;
        let rows = stmt
            .query_map(params![scope_epoch], row_to_grant)
            .map_err(|e| format!("query list_for_scope: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect list_for_scope: {e}"))?;
        Ok(rows)
    }
}

fn row_to_grant(row: &rusqlite::Row) -> rusqlite::Result<CapabilityGrant> {
    Ok(CapabilityGrant {
        handle_id: row.get(0)?,
        approval_id: row.get(1)?,
        granted_to: row.get(2)?,
        action_kind: row.get(3)?,
        scope_epoch: row.get(4)?,
        granted_at: row.get(5)?,
        expires_at: row.get(6)?,
        revoked_at: row.get(7)?,
        closure_predicate: row.get(8)?,
    })
}

// ── Broker ──────────────────────────────────────────────────

/// The revocable-capability primitive. Wraps a [`CapabilityStore`] and
/// exposes the `grant → invoke → close_scope / revoke` lifecycle.
#[derive(Clone)]
pub struct CapabilityBroker {
    store: std::sync::Arc<CapabilityStore>,
}

impl CapabilityBroker {
    pub fn new(store: std::sync::Arc<CapabilityStore>) -> Self {
        Self { store }
    }

    /// Open the on-disk store (shared `approvals.db`) and wrap it.
    pub fn open(home_dir: &Path) -> Result<Self, String> {
        Ok(Self::new(std::sync::Arc::new(CapabilityStore::open(
            home_dir,
        )?)))
    }

    /// Mint an epoch-bound capability handle from a decided approval.
    ///
    /// Fail-closed guards:
    /// - Refuses to grant into an already-closed scope ([`CapError::ScopeClosed`])
    ///   — no stale write can mint a live handle after its subgoal ended.
    /// - A non-positive `ttl` falls back to [`DEFAULT_TTL_SECONDS`] (a
    ///   zero/negative TTL would silently expire immediately).
    pub async fn grant(
        &self,
        approval_id: &str,
        granted_to: &str,
        action_kind: &str,
        scope_epoch: &str,
        ttl_seconds: i64,
        closure_predicate: Option<String>,
    ) -> Result<CapabilityGrant, CapError> {
        // Stale-write guard: cannot grant into a closed scope.
        if self
            .store
            .is_scope_closed(scope_epoch)
            .await
            .map_err(CapError::Store)?
        {
            warn!(
                scope_epoch,
                approval_id, "refused to grant into a closed scope (stale write)"
            );
            return Err(CapError::ScopeClosed);
        }
        let ttl = if ttl_seconds > 0 {
            ttl_seconds
        } else {
            DEFAULT_TTL_SECONDS
        };
        let now = Utc::now();
        let grant = CapabilityGrant {
            handle_id: uuid::Uuid::new_v4().to_string(),
            approval_id: approval_id.to_string(),
            granted_to: granted_to.to_string(),
            action_kind: action_kind.to_string(),
            scope_epoch: scope_epoch.to_string(),
            granted_at: now.to_rfc3339(),
            expires_at: (now + chrono::Duration::seconds(ttl)).to_rfc3339(),
            revoked_at: None,
            closure_predicate,
        };
        self.store.insert(&grant).await.map_err(CapError::Store)?;
        info!(
            handle_id = %grant.handle_id,
            approval_id,
            granted_to,
            action_kind,
            scope_epoch,
            ttl_seconds = ttl,
            "capability granted (epoch-bound)"
        );
        Ok(grant)
    }

    /// Convenience: mint a grant directly from an [`ApprovalBroker`] record.
    /// Only an **approved** approval may be granted (any other status ⇒
    /// [`CapError::Revoked`], fail-closed). `granted_to` / `action_kind` are
    /// copied from the approval record.
    pub async fn grant_from_approval(
        &self,
        approvals: &ApprovalBroker,
        approval_id: &ApprovalId,
        scope_epoch: &str,
        ttl_seconds: i64,
        closure_predicate: Option<String>,
    ) -> Result<CapabilityGrant, CapError> {
        let rec = approvals
            .get(approval_id)
            .await
            .map_err(CapError::Store)?
            .ok_or(CapError::NotFound)?;
        if !rec.status.is_granted() {
            // Never mint a capability from an unapproved/denied/expired approval.
            return Err(CapError::Revoked);
        }
        self.grant(
            approval_id.as_str(),
            &rec.agent_id,
            &rec.action_kind,
            scope_epoch,
            ttl_seconds,
            closure_predicate,
        )
        .await
    }

    /// The authorization check a tool runs before acting. Fully fail-closed:
    /// every uncertain outcome is a distinct [`CapError`] DENY.
    ///
    /// Check order (all must pass): handle exists → not revoked → scope not
    /// closed → not expired. Scope-closure is checked independently of the
    /// row's own `revoked_at` so a closure that raced the row update still
    /// denies.
    pub async fn invoke(&self, handle_id: &str) -> Result<(), CapError> {
        let grant = self
            .store
            .get(handle_id)
            .await
            .map_err(CapError::Store)?
            .ok_or(CapError::NotFound)?;
        if grant.revoked_at.is_some() {
            return Err(CapError::Revoked);
        }
        // Authoritative: even if this row's revoked_at wasn't stamped, a
        // closed scope means the subgoal ended ⇒ DENY.
        if self
            .store
            .is_scope_closed(&grant.scope_epoch)
            .await
            .map_err(CapError::Store)?
        {
            return Err(CapError::ScopeClosed);
        }
        if grant.is_expired(Utc::now()) {
            return Err(CapError::Expired);
        }
        Ok(())
    }

    /// Fetch a grant (audit / inspection). `None` if the handle is unknown.
    pub async fn get(&self, handle_id: &str) -> Result<Option<CapabilityGrant>, CapError> {
        self.store.get(handle_id).await.map_err(CapError::Store)
    }

    /// Close a subgoal: mark the epoch closed and revoke every live handle
    /// bound to it. Idempotent (closing twice revokes 0 the second time).
    /// Returns the number of handles revoked by this call.
    pub async fn close_scope(&self, scope_epoch: &str) -> Result<u64, CapError> {
        let n = self
            .store
            .close_scope(scope_epoch, &Utc::now().to_rfc3339())
            .await
            .map_err(CapError::Store)?;
        if n > 0 {
            info!(
                scope_epoch,
                revoked = n,
                "subgoal closed — capabilities auto-revoked"
            );
        }
        Ok(n)
    }

    /// Explicitly revoke a single handle (before its scope closes). No-op if
    /// already revoked or missing.
    pub async fn revoke(&self, handle_id: &str) -> Result<(), CapError> {
        let n = self
            .store
            .revoke(handle_id, &Utc::now().to_rfc3339())
            .await
            .map_err(CapError::Store)?;
        if n > 0 {
            info!(handle_id, "capability handle revoked");
        }
        Ok(())
    }

    /// All grants issued under one scope (audit view).
    pub async fn list_for_scope(
        &self,
        scope_epoch: &str,
    ) -> Result<Vec<CapabilityGrant>, CapError> {
        self.store
            .list_for_scope(scope_epoch)
            .await
            .map_err(CapError::Store)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn broker() -> CapabilityBroker {
        CapabilityBroker::new(std::sync::Arc::new(
            CapabilityStore::open_in_memory().unwrap(),
        ))
    }

    async fn a_grant(b: &CapabilityBroker, epoch: &str, ttl: i64) -> CapabilityGrant {
        b.grant("appr-1", "agent-x", "mcp_tool", epoch, ttl, None)
            .await
            .unwrap()
    }

    // ── happy path ──────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn grant_then_invoke_ok() {
        let b = broker();
        let g = a_grant(&b, "task-1", 60).await;
        assert_eq!(g.scope_epoch, "task-1");
        assert!(g.revoked_at.is_none());
        // A live handle invokes cleanly.
        assert_eq!(b.invoke(&g.handle_id).await, Ok(()));
    }

    // ── PORTICO core: post-closure reuse is ALWAYS denied ───

    #[tokio::test(flavor = "current_thread")]
    async fn close_scope_denies_all_reuse() {
        let b = broker();
        // Three handles under one subgoal (PORTICO's N-of-N reuse test).
        let mut handles = Vec::new();
        for _ in 0..3 {
            handles.push(a_grant(&b, "task-close", 3600).await.handle_id);
        }
        // All live before closure.
        for h in &handles {
            assert_eq!(b.invoke(h).await, Ok(()));
        }
        // Close the subgoal → all three revoked in one call.
        let revoked = b.close_scope("task-close").await.unwrap();
        assert_eq!(revoked, 3);
        // Every subsequent invoke is denied — 3/3 blocked.
        for h in &handles {
            assert_eq!(
                b.invoke(h).await,
                Err(CapError::Revoked),
                "post-closure reuse must be denied"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invoke_after_close_reports_scope_closed_even_if_row_not_stamped() {
        // Defence in depth: mark a scope closed directly (no row update) and
        // confirm invoke still denies via the closed-scope ledger.
        let b = broker();
        let g = a_grant(&b, "epoch-ledger", 3600).await;
        // Directly close the scope in the ledger without touching the row's
        // revoked_at (simulates a closure racing the row update).
        b.store
            .conn
            .lock()
            .await
            .execute(
                "INSERT INTO capability_closed_scopes (scope_epoch, closed_at) VALUES (?1, ?2)",
                params!["epoch-ledger", Utc::now().to_rfc3339()],
            )
            .unwrap();
        assert_eq!(b.invoke(&g.handle_id).await, Err(CapError::ScopeClosed));
    }

    // ── expiry ──────────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn expired_handle_denied() {
        let b = broker();
        // Insert an already-expired grant directly.
        let g = CapabilityGrant {
            handle_id: uuid::Uuid::new_v4().to_string(),
            approval_id: "a".into(),
            granted_to: "agent".into(),
            action_kind: "mcp_tool".into(),
            scope_epoch: "e".into(),
            granted_at: (Utc::now() - chrono::Duration::seconds(600)).to_rfc3339(),
            expires_at: (Utc::now() - chrono::Duration::seconds(1)).to_rfc3339(),
            revoked_at: None,
            closure_predicate: None,
        };
        b.store.insert(&g).await.unwrap();
        assert_eq!(b.invoke(&g.handle_id).await, Err(CapError::Expired));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unparseable_expiry_fails_closed() {
        let b = broker();
        let g = CapabilityGrant {
            handle_id: uuid::Uuid::new_v4().to_string(),
            approval_id: "a".into(),
            granted_to: "agent".into(),
            action_kind: "mcp_tool".into(),
            scope_epoch: "e".into(),
            granted_at: Utc::now().to_rfc3339(),
            expires_at: "not-a-timestamp".into(),
            revoked_at: None,
            closure_predicate: None,
        };
        b.store.insert(&g).await.unwrap();
        // Unparseable expiry ⇒ Expired (never a silent allow).
        assert_eq!(b.invoke(&g.handle_id).await, Err(CapError::Expired));
    }

    // ── unknown / revoked ───────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn unknown_handle_denied() {
        let b = broker();
        assert_eq!(b.invoke("never-minted").await, Err(CapError::NotFound));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn explicit_revoke_denies() {
        let b = broker();
        let g = a_grant(&b, "task-r", 3600).await;
        b.revoke(&g.handle_id).await.unwrap();
        assert_eq!(b.invoke(&g.handle_id).await, Err(CapError::Revoked));
        // Revoking again is a harmless no-op.
        b.revoke(&g.handle_id).await.unwrap();
    }

    // ── stale write: grant into a closed scope ──────────────

    #[tokio::test(flavor = "current_thread")]
    async fn grant_into_closed_scope_rejected() {
        let b = broker();
        // Open, close, then attempt to grant into the dead epoch.
        let _ = a_grant(&b, "task-dead", 3600).await;
        b.close_scope("task-dead").await.unwrap();
        let err = b
            .grant("appr-2", "agent-x", "mcp_tool", "task-dead", 3600, None)
            .await
            .unwrap_err();
        assert_eq!(err, CapError::ScopeClosed, "stale write must be blocked");
        // The dead scope has exactly the one original (now revoked) handle —
        // the stale grant never landed.
        let all = b.list_for_scope("task-dead").await.unwrap();
        assert_eq!(all.len(), 1);
        assert!(all[0].revoked_at.is_some());
    }

    // ── ttl fallback ────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn non_positive_ttl_uses_default() {
        let b = broker();
        let g = a_grant(&b, "task-ttl", 0).await;
        let granted = DateTime::parse_from_rfc3339(&g.granted_at).unwrap();
        let expires = DateTime::parse_from_rfc3339(&g.expires_at).unwrap();
        let secs = (expires - granted).num_seconds();
        assert_eq!(secs, DEFAULT_TTL_SECONDS);
    }

    // ── idempotent close ────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn close_scope_is_idempotent() {
        let b = broker();
        a_grant(&b, "task-idem", 3600).await;
        assert_eq!(b.close_scope("task-idem").await.unwrap(), 1);
        // Second close revokes nothing new.
        assert_eq!(b.close_scope("task-idem").await.unwrap(), 0);
    }

    // ── integration with ApprovalBroker ─────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn grant_from_approval_requires_approved() {
        use crate::approval::{ApprovalBroker, ApprovalStore};
        let approvals = ApprovalBroker::new(std::sync::Arc::new(
            ApprovalStore::open_in_memory().unwrap(),
        ));
        let caps = broker();

        let aid = approvals
            .request(
                "agent-x",
                "mcp_tool",
                "run Bash",
                json!({"tool": "Bash"}),
                300,
            )
            .await
            .unwrap();
        // Pending approval cannot mint a capability (fail-closed).
        assert_eq!(
            caps.grant_from_approval(&approvals, &aid, "task-1", 300, None)
                .await,
            Err(CapError::Revoked)
        );
        // Approve, then a grant is minted carrying the approval's agent/kind.
        approvals
            .decide(&aid, true, "dashboard:alice")
            .await
            .unwrap();
        let g = caps
            .grant_from_approval(&approvals, &aid, "task-1", 300, None)
            .await
            .unwrap();
        assert_eq!(g.granted_to, "agent-x");
        assert_eq!(g.action_kind, "mcp_tool");
        assert_eq!(g.approval_id, aid.as_str());
        assert_eq!(caps.invoke(&g.handle_id).await, Ok(()));

        // Closing the task epoch revokes the approval-derived capability.
        caps.close_scope("task-1").await.unwrap();
        assert_eq!(caps.invoke(&g.handle_id).await, Err(CapError::Revoked));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn grant_from_missing_approval_denied() {
        use crate::approval::{ApprovalBroker, ApprovalId, ApprovalStore};
        let approvals = ApprovalBroker::new(std::sync::Arc::new(
            ApprovalStore::open_in_memory().unwrap(),
        ));
        let caps = broker();
        let ghost = ApprovalId::new();
        assert_eq!(
            caps.grant_from_approval(&approvals, &ghost, "task-1", 300, None)
                .await,
            Err(CapError::NotFound)
        );
    }
}
