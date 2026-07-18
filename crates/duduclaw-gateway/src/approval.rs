//! Universal Human-in-the-Loop (HITL) `ApprovalBroker`.
//!
//! ONE interrupt/approval primitive — the LangGraph `interrupt()` /
//! OpenAI-SDK HITL equivalent — spanning **MCP tools**, **autopilot
//! actions**, and **bus tasks**. A caller that is about to perform a
//! sensitive action `request()`s approval (storing the exact payload to
//! re-dispatch), then either polls or `await_decision()`s. A human
//! decides through a messaging channel reply or the dashboard; on
//! approve, the caller re-reads the stored payload and re-dispatches.
//!
//! ## Why one broker (migration note)
//!
//! Three ad-hoc, in-process approval implementations exist today. This
//! broker is the intended single path they converge onto (they are NOT
//! deleted this pass — wiring is a follow-up):
//!
//! 1. **`browser_router.rs`** — `require_human_approval_for: Vec<String>`
//!    only *flags* a `BrowserRequest` (`requires_human_approval: bool`);
//!    there is no store, no decision channel, no TTL. Migration: when the
//!    router flags a request, call [`ApprovalBroker::request`] with
//!    `action_kind = "browser_action"` and `await_decision`; deny-on-expiry
//!    is then automatic instead of a dangling boolean.
//! 2. **`channel_sender.rs`** — a process-local `HashMap<user_id,
//!    oneshot::Sender<bool>>` (`wait_for_confirmation` /
//!    `resolve_confirmation`). Volatile (lost on restart), single-user,
//!    no audit trail, no cross-process visibility. Migration: keep the
//!    zh-TW reply-word matching (`is_confirmation_reply` /
//!    `is_denial_reply`) but resolve against a persisted approval id via
//!    [`ApprovalBroker::decide`] instead of an in-memory oneshot.
//! 3. **`duduclaw-governance` approval workflow** — policy-level approval
//!    gate. Migration: the governance `PolicyType::Permission` decision
//!    can enqueue an [`ApprovalBroker::request`] and gate on its result,
//!    unifying the audit trail in `approvals.db`.
//!
//! ## Decision sources
//!
//! - `agent.toml [capabilities] approval_required_tools = [...]` — parsed
//!   by [`approval_required_tools`]. The MCP dispatch path (owned by
//!   another agent this wave) will call [`ApprovalBroker::request`] +
//!   [`ApprovalBroker::await_decision`] before executing a listed tool.
//! - autopilot rule `require_approval = true` in the action JSON — checked
//!   by [`rule_requires_approval`] and wired into
//!   `autopilot_engine::execute_action` (see `with_approval_broker`).
//! - dashboard RPC `approvals.list / approvals.approve / approvals.deny`
//!   (to be added in `handlers.rs` later) → [`list_pending`] / [`decide`].
//!
//! ## Fail-closed conventions
//!
//! - **TTL expiry counts as DENY.** A pending approval past its TTL is
//!   marked `expired`; [`await_decision`] returns `Expired`, which callers
//!   MUST treat as a denial (never fall through to execute).
//! - **`decide` refuses to change a terminal state.** Once
//!   approved/denied/expired, a second decision is rejected (no silent
//!   flip). The `WHERE status = 'pending'` guard also closes the
//!   two-decider race.
//! - **Store idioms mirror `events_store.rs` / `autopilot_store.rs`**:
//!   parameterized SQL only, WAL + `busy_timeout`, self-healing schema.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;
use tracing::{info, warn};

// ── Constants ───────────────────────────────────────────────

/// Default TTL when a caller does not specify one. 1 hour.
pub const DEFAULT_TTL_SECONDS: i64 = 3600;

/// Max chars of `summary` rendered into a channel message (CJK-safe via
/// `truncate_chars`, never raw byte slicing).
const CHANNEL_SUMMARY_MAX_CHARS: usize = 500;

/// `decided_by` marker used when the TTL expiry path denies an approval.
pub const DECIDED_BY_TTL: &str = "system:ttl";

// ── Types ───────────────────────────────────────────────────

/// Opaque approval identifier (UUIDv4 string).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ApprovalId(String);

impl ApprovalId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for ApprovalId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ApprovalId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for ApprovalId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// Lifecycle status of an approval. Approved is the ONLY status a caller
/// may act on; every other terminal status is a denial (fail-closed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalStatus {
    Pending,
    Approved,
    Denied,
    Expired,
}

impl ApprovalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ApprovalStatus::Pending => "pending",
            ApprovalStatus::Approved => "approved",
            ApprovalStatus::Denied => "denied",
            ApprovalStatus::Expired => "expired",
        }
    }

    /// Parse from the DB text column. Unknown values fail closed to
    /// `Denied` (never `Approved`) so a corrupted row never authorizes.
    pub fn from_db(s: &str) -> Self {
        match s {
            "pending" => ApprovalStatus::Pending,
            "approved" => ApprovalStatus::Approved,
            "expired" => ApprovalStatus::Expired,
            _ => ApprovalStatus::Denied,
        }
    }

    /// True for any non-pending state (approved / denied / expired).
    pub fn is_terminal(self) -> bool {
        !matches!(self, ApprovalStatus::Pending)
    }

    /// True only when the caller is authorized to proceed.
    pub fn is_granted(self) -> bool {
        matches!(self, ApprovalStatus::Approved)
    }
}

/// Where an approval decision originated. `decided_by` is stored as free
/// text; this enum standardizes the common producers for the eventual
/// wire-up (channel reply / dashboard RPC / TTL sweep / programmatic).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionSource {
    Channel,
    Dashboard,
    Ttl,
    Api,
}

impl DecisionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            DecisionSource::Channel => "channel",
            DecisionSource::Dashboard => "dashboard",
            DecisionSource::Ttl => DECIDED_BY_TTL,
            DecisionSource::Api => "api",
        }
    }
}

/// One row of the `approvals` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRecord {
    pub id: ApprovalId,
    pub agent_id: String,
    /// "mcp_tool" | "autopilot_action" | "bus_task" | "browser_action" | ...
    pub action_kind: String,
    /// Human-readable summary of what is being approved.
    pub summary: String,
    /// The exact thing to re-dispatch on approval (opaque JSON).
    pub payload: Value,
    pub status: ApprovalStatus,
    pub created_at: String,
    pub decided_at: Option<String>,
    pub decided_by: Option<String>,
    pub ttl_seconds: i64,
}

impl ApprovalRecord {
    /// The instant this approval expires (created_at + ttl). `None` if
    /// `created_at` is unparseable — treated as "already expired" by
    /// [`is_stale`] (fail-closed).
    fn expires_at(&self) -> Option<DateTime<Utc>> {
        let created = DateTime::parse_from_rfc3339(&self.created_at).ok()?;
        Some(created.with_timezone(&Utc) + chrono::Duration::seconds(self.ttl_seconds))
    }

    /// True if pending and past its TTL (or has an unparseable timestamp).
    fn is_stale(&self, now: DateTime<Utc>) -> bool {
        if self.status != ApprovalStatus::Pending {
            return false;
        }
        match self.expires_at() {
            Some(exp) => now >= exp,
            None => true, // unparseable created_at ⇒ fail closed
        }
    }
}

// ── Store ───────────────────────────────────────────────────

/// SQLite-backed persistence for approvals. Mirrors the `events_store` /
/// `autopilot_store` idioms: `Mutex<Connection>`, WAL, self-healing
/// schema, parameterized SQL only.
pub struct ApprovalStore {
    conn: Mutex<Connection>,
    #[allow(dead_code)]
    db_path: Option<PathBuf>,
}

impl ApprovalStore {
    /// Open (or create) the store at `<home>/approvals.db`.
    pub fn open(home_dir: &Path) -> Result<Self, String> {
        let db_path = home_dir.join("approvals.db");
        let conn = Connection::open(&db_path).map_err(|e| format!("open approvals store: {e}"))?;
        Self::init_schema(&conn)?;
        info!(?db_path, "ApprovalStore initialized");
        Ok(Self {
            conn: Mutex::new(conn),
            db_path: Some(db_path),
        })
    }

    /// In-memory store for tests (no file, no WAL persistence).
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

             CREATE TABLE IF NOT EXISTS approvals (
                 id           TEXT PRIMARY KEY,
                 agent_id     TEXT NOT NULL,
                 action_kind  TEXT NOT NULL,
                 summary      TEXT NOT NULL,
                 payload      TEXT NOT NULL DEFAULT '{}',
                 status       TEXT NOT NULL DEFAULT 'pending',
                 created_at   TEXT NOT NULL,
                 decided_at   TEXT,
                 decided_by   TEXT,
                 ttl_seconds  INTEGER NOT NULL DEFAULT 3600
             );

             CREATE INDEX IF NOT EXISTS idx_approvals_status ON approvals(status);
             CREATE INDEX IF NOT EXISTS idx_approvals_agent  ON approvals(agent_id);",
        )
        .map_err(|e| format!("init approvals schema: {e}"))?;
        Ok(())
    }

    async fn insert(&self, rec: &ApprovalRecord) -> Result<(), String> {
        let payload_text = rec.payload.to_string();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO approvals
                (id, agent_id, action_kind, summary, payload, status,
                 created_at, decided_at, decided_by, ttl_seconds)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                rec.id.as_str(),
                rec.agent_id,
                rec.action_kind,
                rec.summary,
                payload_text,
                rec.status.as_str(),
                rec.created_at,
                rec.decided_at,
                rec.decided_by,
                rec.ttl_seconds,
            ],
        )
        .map_err(|e| format!("insert approval: {e}"))?;
        Ok(())
    }

    async fn get(&self, id: &ApprovalId) -> Result<Option<ApprovalRecord>, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT id, agent_id, action_kind, summary, payload, status,
                    created_at, decided_at, decided_by, ttl_seconds
             FROM approvals WHERE id = ?1",
            params![id.as_str()],
            row_to_record,
        )
        .optional()
        .map_err(|e| format!("get approval: {e}"))
    }

    /// Transition a pending row to a terminal status. The
    /// `WHERE status = 'pending'` guard makes this idempotent-safe and
    /// closes the two-decider race — returns rows affected (0 = not
    /// pending / not found).
    async fn decide_if_pending(
        &self,
        id: &ApprovalId,
        status: ApprovalStatus,
        decided_by: &str,
        decided_at: &str,
    ) -> Result<usize, String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE approvals
             SET status = ?1, decided_by = ?2, decided_at = ?3
             WHERE id = ?4 AND status = 'pending'",
            params![status.as_str(), decided_by, decided_at, id.as_str()],
        )
        .map_err(|e| format!("decide approval: {e}"))
    }

    async fn list_pending(&self, agent_id: Option<&str>) -> Result<Vec<ApprovalRecord>, String> {
        let conn = self.conn.lock().await;
        match agent_id {
            Some(aid) => {
                let mut stmt = conn
                    .prepare(
                        "SELECT id, agent_id, action_kind, summary, payload, status,
                                created_at, decided_at, decided_by, ttl_seconds
                         FROM approvals
                         WHERE status = 'pending' AND agent_id = ?1
                         ORDER BY created_at ASC",
                    )
                    .map_err(|e| format!("prepare list_pending: {e}"))?;
                let rows = stmt
                    .query_map(params![aid], row_to_record)
                    .map_err(|e| format!("query list_pending: {e}"))?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| format!("collect list_pending: {e}"))?;
                Ok(rows)
            }
            None => {
                let mut stmt = conn
                    .prepare(
                        "SELECT id, agent_id, action_kind, summary, payload, status,
                                created_at, decided_at, decided_by, ttl_seconds
                         FROM approvals
                         WHERE status = 'pending'
                         ORDER BY created_at ASC",
                    )
                    .map_err(|e| format!("prepare list_pending: {e}"))?;
                let rows = stmt
                    .query_map([], row_to_record)
                    .map_err(|e| format!("query list_pending: {e}"))?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| format!("collect list_pending: {e}"))?;
                Ok(rows)
            }
        }
    }
}

fn row_to_record(row: &rusqlite::Row) -> rusqlite::Result<ApprovalRecord> {
    let payload_text: String = row.get(4)?;
    let payload: Value = serde_json::from_str(&payload_text).unwrap_or(Value::Null);
    let status_text: String = row.get(5)?;
    Ok(ApprovalRecord {
        id: ApprovalId::from(row.get::<_, String>(0)?),
        agent_id: row.get(1)?,
        action_kind: row.get(2)?,
        summary: row.get(3)?,
        payload,
        status: ApprovalStatus::from_db(&status_text),
        created_at: row.get(6)?,
        decided_at: row.get(7)?,
        decided_by: row.get(8)?,
        ttl_seconds: row.get(9)?,
    })
}

// ── Broker ──────────────────────────────────────────────────

/// The single HITL approval primitive. Holds the [`ApprovalStore`] and
/// exposes the request → decide → poll/await lifecycle.
#[derive(Clone)]
pub struct ApprovalBroker {
    store: std::sync::Arc<ApprovalStore>,
}

impl ApprovalBroker {
    pub fn new(store: std::sync::Arc<ApprovalStore>) -> Self {
        Self { store }
    }

    /// Open the on-disk store and wrap it in a broker.
    pub fn open(home_dir: &Path) -> Result<Self, String> {
        Ok(Self::new(std::sync::Arc::new(ApprovalStore::open(home_dir)?)))
    }

    /// Record a new pending approval. `payload` is the exact thing to
    /// re-dispatch once approved. A non-positive `ttl` falls back to
    /// [`DEFAULT_TTL_SECONDS`] (a zero/negative TTL would mean "expire
    /// immediately", a fail-closed footgun for callers who forget it).
    pub async fn request(
        &self,
        agent_id: &str,
        action_kind: &str,
        summary: &str,
        payload: Value,
        ttl_seconds: i64,
    ) -> Result<ApprovalId, String> {
        let ttl = if ttl_seconds > 0 {
            ttl_seconds
        } else {
            DEFAULT_TTL_SECONDS
        };
        let rec = ApprovalRecord {
            id: ApprovalId::new(),
            agent_id: agent_id.to_string(),
            action_kind: action_kind.to_string(),
            summary: summary.to_string(),
            payload,
            status: ApprovalStatus::Pending,
            created_at: Utc::now().to_rfc3339(),
            decided_at: None,
            decided_by: None,
            ttl_seconds: ttl,
        };
        let id = rec.id.clone();
        self.store.insert(&rec).await?;
        info!(
            approval_id = %id,
            agent_id,
            action_kind,
            ttl_seconds = ttl,
            "approval requested"
        );
        Ok(id)
    }

    /// Fetch the full record (payload included) for re-dispatch.
    pub async fn get(&self, id: &ApprovalId) -> Result<Option<ApprovalRecord>, String> {
        self.store.get(id).await
    }

    /// Current status. Opportunistically expires the record first so a
    /// caller polling past the TTL observes `Expired` without needing a
    /// separate sweep.
    pub async fn poll(&self, id: &ApprovalId) -> Result<ApprovalStatus, String> {
        let rec = self
            .store
            .get(id)
            .await?
            .ok_or_else(|| format!("approval {id} not found"))?;
        if rec.is_stale(Utc::now()) {
            // Best-effort expire; ignore race (someone may have just decided).
            let _ = self
                .store
                .decide_if_pending(
                    id,
                    ApprovalStatus::Expired,
                    DECIDED_BY_TTL,
                    &Utc::now().to_rfc3339(),
                )
                .await?;
            // Re-read to report the authoritative post-expiry status.
            let fresh = self.store.get(id).await?;
            return Ok(fresh.map(|r| r.status).unwrap_or(ApprovalStatus::Expired));
        }
        Ok(rec.status)
    }

    /// Approve or deny a pending approval. Idempotent-safe: refuses to
    /// change a terminal state (a second decide — including double-approve
    /// — is rejected). The store's `WHERE status = 'pending'` guard closes
    /// the concurrent-decider race.
    pub async fn decide(
        &self,
        id: &ApprovalId,
        approve: bool,
        decided_by: &str,
    ) -> Result<(), String> {
        let rec = self
            .store
            .get(id)
            .await?
            .ok_or_else(|| format!("approval {id} not found"))?;
        if rec.status.is_terminal() {
            return Err(format!(
                "approval {id} already {} — refusing to change terminal state",
                rec.status.as_str()
            ));
        }
        let new_status = if approve {
            ApprovalStatus::Approved
        } else {
            ApprovalStatus::Denied
        };
        let n = self
            .store
            .decide_if_pending(id, new_status, decided_by, &Utc::now().to_rfc3339())
            .await?;
        if n == 0 {
            // Lost the race to another decider between get() and update.
            return Err(format!("approval {id} was decided concurrently"));
        }
        info!(approval_id = %id, decision = new_status.as_str(), decided_by, "approval decided");
        Ok(())
    }

    /// All pending approvals, optionally filtered to one agent. Sweeps
    /// stale rows first so the returned set never contains an expired
    /// pending row.
    pub async fn list_pending(
        &self,
        agent_id: Option<&str>,
    ) -> Result<Vec<ApprovalRecord>, String> {
        self.expire_stale().await?;
        self.store.list_pending(agent_id).await
    }

    /// Sweep: mark every pending approval past its TTL as `expired`.
    /// Returns the number expired. TTL expiry counts as DENY.
    pub async fn expire_stale(&self) -> Result<u64, String> {
        let now = Utc::now();
        let pending = self.store.list_pending(None).await?;
        let mut expired = 0u64;
        for rec in pending {
            if rec.is_stale(now) {
                let n = self
                    .store
                    .decide_if_pending(
                        &rec.id,
                        ApprovalStatus::Expired,
                        DECIDED_BY_TTL,
                        &now.to_rfc3339(),
                    )
                    .await?;
                expired += n as u64;
            }
        }
        if expired > 0 {
            info!(count = expired, "approvals expired by TTL (treated as deny)");
        }
        Ok(expired)
    }

    /// Block until the approval reaches a terminal state or its TTL
    /// elapses, polling every `poll_interval`. Returns `Expired` on TTL —
    /// which callers MUST treat as a denial (fail-closed). Max wait is
    /// bounded by the record's own TTL.
    pub async fn await_decision(
        &self,
        id: &ApprovalId,
        poll_interval: Duration,
    ) -> Result<ApprovalStatus, String> {
        // Bound the loop by the record's TTL so we can never wait forever.
        let deadline = {
            let rec = self
                .store
                .get(id)
                .await?
                .ok_or_else(|| format!("approval {id} not found"))?;
            rec.expires_at()
        };
        loop {
            let status = self.poll(id).await?;
            if status.is_terminal() {
                return Ok(status);
            }
            // Past deadline but poll() hasn't expired it yet (clock skew /
            // unparseable ts already handled inside poll) — force expire.
            if let Some(exp) = deadline {
                if Utc::now() >= exp {
                    let _ = self
                        .store
                        .decide_if_pending(
                            id,
                            ApprovalStatus::Expired,
                            DECIDED_BY_TTL,
                            &Utc::now().to_rfc3339(),
                        )
                        .await?;
                    return Ok(ApprovalStatus::Expired);
                }
            }
            tokio::time::sleep(poll_interval).await;
        }
    }
}

// ── Decision source: agent.toml [capabilities] ──────────────

/// Parse `agent.toml [capabilities] approval_required_tools = [...]` into a
/// set of tool names the MCP dispatch path must gate behind an approval.
///
/// **Fail-safe choice (documented):** a missing file, missing key, or a
/// malformed `[capabilities]` table returns an **empty set** (a `warn!`
/// is logged for the malformed case). This matches the project's
/// `CapabilitiesConfig` deny-by-default model where the *primary* gate is
/// `allowed_tools` / `denied_tools`; `approval_required_tools` is
/// **additive friction**, not the primary security gate. Failing it
/// closed (treat everything as approval-required) would brick every agent
/// on a typo — the wrong trade-off for a secondary, opt-in control. The
/// hard security boundary stays with the deny-list, which independently
/// fails closed.
pub fn approval_required_tools(agent_dir: &Path) -> HashSet<String> {
    let path = agent_dir.join("agent.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return HashSet::new();
    };
    let value: toml::Value = match toml::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            warn!(?path, error = %e, "malformed agent.toml — approval_required_tools defaults to empty (additive gate)");
            return HashSet::new();
        }
    };
    value
        .get("capabilities")
        .and_then(|c| c.get("approval_required_tools"))
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// True when a tool name is listed in the agent's
/// `approval_required_tools`. Exact match (no substring/`contains` — a
/// routing/security decision, per project convention).
pub fn tool_requires_approval(agent_dir: &Path, tool_name: &str) -> bool {
    approval_required_tools(agent_dir).contains(tool_name)
}

// ── P2b: ActionGuard three-value irreversibility gate ───────────
//
// The tool-call approval decision is upgraded from binary (`approval_required_tools`
// = ask a human) to three-valued (Magentic-UI ActionGuard, arXiv:2507.22358 §
// action approval):
//   • Always irreversible (`irreversible_tools`)      → always ask a human.
//   • Maybe irreversible  (`maybe_irreversible_tools`) → call the ActionGuard LLM
//     judge on THIS specific call; risky → ask a human, safe → auto-proceed.
//   • Never (unlisted)                                 → the existing
//     allowed/denied/policy flow, no new friction.
//
// Relationship to the legacy `approval_required_tools`: **take-the-stricter**. The
// old field keeps its exact semantics (== always) and the new fields are additive,
// so no existing config changes behavior.

/// Parse `agent.toml [capabilities] irreversible_tools = [...]` — tools that are
/// **always** irreversible and must obtain human approval before running
/// (identical enforcement to `approval_required_tools`, but a separate, clearer
/// field for the ActionGuard model). Same fail-safe as
/// [`approval_required_tools`]: a missing file/key or malformed table returns an
/// empty set (additive gate; the primary security boundary stays with the
/// deny-list).
pub fn irreversible_tools(agent_dir: &Path) -> HashSet<String> {
    parse_capability_tool_list(agent_dir, "irreversible_tools")
}

/// Parse `agent.toml [capabilities] maybe_irreversible_tools = [...]` — tools
/// whose irreversibility is call-dependent, so the ActionGuard judge decides
/// per specific call. Same empty-on-error fail-safe as the siblings.
pub fn maybe_irreversible_tools(agent_dir: &Path) -> HashSet<String> {
    parse_capability_tool_list(agent_dir, "maybe_irreversible_tools")
}

/// Shared reader for a `[capabilities]` string-array field. Follows the exact
/// fail-safe contract of [`approval_required_tools`] (empty on missing/malformed,
/// `warn!` on malformed TOML). Kept private so the three public parsers share one
/// implementation without changing their documented semantics.
fn parse_capability_tool_list(agent_dir: &Path, key: &str) -> HashSet<String> {
    let path = agent_dir.join("agent.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return HashSet::new();
    };
    let value: toml::Value = match toml::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            warn!(?path, %key, error = %e, "malformed agent.toml — capability tool list defaults to empty (additive gate)");
            return HashSet::new();
        }
    };
    value
        .get("capabilities")
        .and_then(|c| c.get(key))
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// True when a tool is listed in `irreversible_tools` (always-irreversible).
/// Exact match — a routing/security decision (project convention 2).
pub fn tool_is_irreversible(agent_dir: &Path, tool_name: &str) -> bool {
    irreversible_tools(agent_dir).contains(tool_name)
}

/// True when a tool is listed in `maybe_irreversible_tools` (judge decides).
/// Exact match — a routing/security decision (project convention 2).
pub fn tool_is_maybe_irreversible(agent_dir: &Path, tool_name: &str) -> bool {
    maybe_irreversible_tools(agent_dir).contains(tool_name)
}

/// The ActionGuard gate resolved for one tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionGate {
    /// No new friction: fall through to the existing allowed/denied/policy flow.
    Auto,
    /// Must obtain human approval (ApprovalBroker) before running.
    RequireApproval,
    /// Ambiguous (maybe-irreversible): run the ActionGuard LLM judge on this
    /// specific call, then re-resolve with the verdict.
    ConsultJudge,
}

/// The ActionGuard judge's ruling on a maybe-irreversible call, already reduced
/// to a two-way (parse failure / timeout collapse to `Risky`, fail-closed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JudgeVerdict {
    /// Judge deemed this specific call safe / reversible → auto-proceed.
    Safe,
    /// Judge deemed it irreversible / risky, OR the judge itself failed
    /// (fail-closed) → escalate to human approval.
    Risky,
}

/// Pure, deterministic resolution of the ActionGuard three-value gate for one
/// tool call. Separated from the (hard-to-unit-test) dispatch path so the
/// take-the-stricter merge logic is directly testable.
///
/// Inputs:
/// - `in_always`: tool is in the always-irreversible set. This folds in the
///   legacy `approval_required_tools` + install-class gate at the call site, so
///   **always wins** — the strictest outcome regardless of the maybe set.
/// - `in_maybe`: tool is in `maybe_irreversible_tools`.
/// - `judge_verdict`: `None` = the judge has not run yet (caller must, hence
///   `ConsultJudge`); `Some(..)` = re-resolve a maybe-gate with the ruling.
pub fn resolve_action_gate(
    in_always: bool,
    in_maybe: bool,
    judge_verdict: Option<JudgeVerdict>,
) -> ActionGate {
    // Take-the-stricter: always beats maybe beats never.
    if in_always {
        return ActionGate::RequireApproval;
    }
    if in_maybe {
        return match judge_verdict {
            None => ActionGate::ConsultJudge,
            Some(JudgeVerdict::Risky) => ActionGate::RequireApproval,
            Some(JudgeVerdict::Safe) => ActionGate::Auto,
        };
    }
    ActionGate::Auto
}

/// F1: whether the operator has explicitly opted an agent OUT of the
/// install-class MCP approval gate via `agent.toml [capabilities]
/// auto_approve_install = true`.
///
/// **Fail-closed:** a missing file, missing key, malformed table, or a
/// non-bool value all return `false` (the gate stays ON). Only an explicit
/// `true` disables the gate — the WP5 requirement is that MCP-reached
/// install-class tools need human approval by default, and the caller holding
/// `Scope::Admin` (the default internal principal) is NOT a bypass. This is
/// the sole exemption an operator can grant.
pub fn auto_approve_install(agent_dir: &Path) -> bool {
    let path = agent_dir.join("agent.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return false;
    };
    let value: toml::Value = match toml::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            warn!(?path, error = %e, "malformed agent.toml — auto_approve_install defaults to false (gate stays on)");
            return false;
        }
    };
    value
        .get("capabilities")
        .and_then(|c| c.get("auto_approve_install"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

// ── Decision source: autopilot rule ─────────────────────────

/// True when an autopilot rule's `action` JSON opts into human approval
/// via `require_approval = true`. Absent / non-bool ⇒ `false` (no gate).
pub fn rule_requires_approval(action: &Value) -> bool {
    action
        .get("require_approval")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

// ── Notification surface (channel) ──────────────────────────

/// Minimal XML/markup escape for values interpolated into channel text
/// or an XML-delimited prompt block (project convention: prompts use XML
/// delimiters for injection resistance).
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Render a zh-TW, XML-safe approval prompt for a messaging channel. The
/// channel_sender path (wired later) sends this with inline approve/deny
/// buttons; a text-only channel matches the reply against the existing
/// `is_confirmation_reply` / `is_denial_reply` word lists and calls
/// [`ApprovalBroker::decide`].
pub fn pending_summary_for_channel(record: &ApprovalRecord) -> String {
    let agent = xml_escape(&record.agent_id);
    let kind = xml_escape(&record.action_kind);
    let summary = xml_escape(&duduclaw_core::truncate_chars(
        &record.summary,
        CHANNEL_SUMMARY_MAX_CHARS,
    ));
    format!(
        "🔔 需要您的核准\n\
         代理：{agent}\n\
         動作：{kind}\n\
         摘要：{summary}\n\
         編號：{id}\n\
         回覆「確認」核准，或「取消」拒絕（{ttl} 秒後自動拒絕）。",
        id = record.id,
        ttl = record.ttl_seconds,
    )
}

// ── Dashboard RPC shape (documentation) ─────────────────────
//
// To be added in `handlers.rs` (owned this wave — NOT edited here):
//
//   approvals.list   { agent_id?: string } -> ApprovalRecord[]   → list_pending()
//   approvals.approve{ id: string }        -> { ok: true }        → decide(id, true,  "dashboard:<user>")
//   approvals.deny   { id: string }        -> { ok: true }        → decide(id, false, "dashboard:<user>")
//
// Every approve/deny should append an Activity Feed row
// (`task_store::append_activity`, event_type "approval_decided") so the
// dashboard Activity tab shows the human decision, mirroring how
// `autopilot_engine` records rule fires. On approve, the caller re-reads
// `record.payload` and re-dispatches (e.g. re-enqueue on `bus_queue.jsonl`
// for a `bus_task`, re-run the MCP tool for an `mcp_tool`).

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn broker() -> ApprovalBroker {
        ApprovalBroker::new(std::sync::Arc::new(ApprovalStore::open_in_memory().unwrap()))
    }

    #[tokio::test(flavor = "current_thread")]
    async fn request_creates_pending() {
        let b = broker();
        let id = b
            .request("agent-1", "mcp_tool", "run Bash rm -rf", json!({"tool":"Bash"}), 60)
            .await
            .unwrap();
        assert_eq!(b.poll(&id).await.unwrap(), ApprovalStatus::Pending);
        let rec = b.get(&id).await.unwrap().unwrap();
        assert_eq!(rec.agent_id, "agent-1");
        assert_eq!(rec.payload, json!({"tool":"Bash"}));
        assert_eq!(rec.ttl_seconds, 60);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn non_positive_ttl_falls_back_to_default() {
        let b = broker();
        let id = b.request("a", "bus_task", "s", json!({}), 0).await.unwrap();
        let rec = b.get(&id).await.unwrap().unwrap();
        assert_eq!(rec.ttl_seconds, DEFAULT_TTL_SECONDS);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn decide_approve_transition() {
        let b = broker();
        let id = b.request("a", "mcp_tool", "s", json!({}), 60).await.unwrap();
        b.decide(&id, true, "dashboard:alice").await.unwrap();
        assert_eq!(b.poll(&id).await.unwrap(), ApprovalStatus::Approved);
        let rec = b.get(&id).await.unwrap().unwrap();
        assert_eq!(rec.decided_by.as_deref(), Some("dashboard:alice"));
        assert!(rec.decided_at.is_some());
        assert!(rec.status.is_granted());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn decide_deny_transition() {
        let b = broker();
        let id = b.request("a", "mcp_tool", "s", json!({}), 60).await.unwrap();
        b.decide(&id, false, "channel:user").await.unwrap();
        let status = b.poll(&id).await.unwrap();
        assert_eq!(status, ApprovalStatus::Denied);
        assert!(!status.is_granted());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn double_approve_refused() {
        let b = broker();
        let id = b.request("a", "mcp_tool", "s", json!({}), 60).await.unwrap();
        b.decide(&id, true, "u1").await.unwrap();
        // Second decide on a terminal state is refused (no silent flip).
        let err = b.decide(&id, true, "u2").await.unwrap_err();
        assert!(err.contains("terminal"), "unexpected: {err}");
        // And a contradictory decision is likewise refused.
        assert!(b.decide(&id, false, "u3").await.is_err());
        // Original decider is preserved.
        let rec = b.get(&id).await.unwrap().unwrap();
        assert_eq!(rec.decided_by.as_deref(), Some("u1"));
        assert_eq!(rec.status, ApprovalStatus::Approved);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn decide_missing_id_errs() {
        let b = broker();
        let ghost = ApprovalId::new();
        assert!(b.decide(&ghost, true, "u").await.is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ttl_expiry_treated_as_deny() {
        let b = broker();
        // ttl of 0 would default; force an already-expired row via -1 stored
        // directly is not possible through request(), so insert manually.
        let rec = ApprovalRecord {
            id: ApprovalId::new(),
            agent_id: "a".into(),
            action_kind: "bus_task".into(),
            summary: "s".into(),
            payload: json!({}),
            status: ApprovalStatus::Pending,
            // created 10 minutes ago with 1s ttl ⇒ long expired.
            created_at: (Utc::now() - chrono::Duration::seconds(600)).to_rfc3339(),
            decided_at: None,
            decided_by: None,
            ttl_seconds: 1,
        };
        let id = rec.id.clone();
        b.store.insert(&rec).await.unwrap();

        // expire_stale sweeps it.
        let n = b.expire_stale().await.unwrap();
        assert_eq!(n, 1);
        let status = b.poll(&id).await.unwrap();
        assert_eq!(status, ApprovalStatus::Expired);
        assert!(!status.is_granted(), "expired must NOT be granted (fail-closed)");
        let stored = b.get(&id).await.unwrap().unwrap();
        assert_eq!(stored.decided_by.as_deref(), Some(DECIDED_BY_TTL));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn poll_expires_stale_on_read() {
        let b = broker();
        let rec = ApprovalRecord {
            id: ApprovalId::new(),
            agent_id: "a".into(),
            action_kind: "bus_task".into(),
            summary: "s".into(),
            payload: json!({}),
            status: ApprovalStatus::Pending,
            created_at: (Utc::now() - chrono::Duration::seconds(600)).to_rfc3339(),
            decided_at: None,
            decided_by: None,
            ttl_seconds: 1,
        };
        let id = rec.id.clone();
        b.store.insert(&rec).await.unwrap();
        // poll() alone (no explicit sweep) must observe Expired.
        assert_eq!(b.poll(&id).await.unwrap(), ApprovalStatus::Expired);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn list_pending_filters_by_agent() {
        let b = broker();
        b.request("agent-a", "mcp_tool", "s", json!({}), 60).await.unwrap();
        b.request("agent-a", "bus_task", "s", json!({}), 60).await.unwrap();
        b.request("agent-b", "mcp_tool", "s", json!({}), 60).await.unwrap();

        let all = b.list_pending(None).await.unwrap();
        assert_eq!(all.len(), 3);
        let only_a = b.list_pending(Some("agent-a")).await.unwrap();
        assert_eq!(only_a.len(), 2);
        assert!(only_a.iter().all(|r| r.agent_id == "agent-a"));

        // Decided rows drop out of pending.
        let id = only_a[0].id.clone();
        b.decide(&id, true, "u").await.unwrap();
        assert_eq!(b.list_pending(Some("agent-a")).await.unwrap().len(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn list_pending_sweeps_expired() {
        let b = broker();
        // one live, one already-expired
        b.request("a", "mcp_tool", "live", json!({}), 60).await.unwrap();
        let stale = ApprovalRecord {
            id: ApprovalId::new(),
            agent_id: "a".into(),
            action_kind: "bus_task".into(),
            summary: "stale".into(),
            payload: json!({}),
            status: ApprovalStatus::Pending,
            created_at: (Utc::now() - chrono::Duration::seconds(600)).to_rfc3339(),
            decided_at: None,
            decided_by: None,
            ttl_seconds: 1,
        };
        b.store.insert(&stale).await.unwrap();
        let pending = b.list_pending(None).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].summary, "live");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn await_decision_returns_promptly_when_decided() {
        let b = broker();
        let id = b.request("a", "mcp_tool", "s", json!({}), 60).await.unwrap();
        let b2 = b.clone();
        let id2 = id.clone();
        // decide almost immediately from another task
        tokio::spawn(async move {
            b2.decide(&id2, true, "u").await.unwrap();
        });
        let status = b
            .await_decision(&id, Duration::from_millis(5))
            .await
            .unwrap();
        assert_eq!(status, ApprovalStatus::Approved);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn await_decision_returns_expired_past_ttl() {
        let b = broker();
        // insert an already-expired pending row
        let rec = ApprovalRecord {
            id: ApprovalId::new(),
            agent_id: "a".into(),
            action_kind: "bus_task".into(),
            summary: "s".into(),
            payload: json!({}),
            status: ApprovalStatus::Pending,
            created_at: (Utc::now() - chrono::Duration::seconds(600)).to_rfc3339(),
            decided_at: None,
            decided_by: None,
            ttl_seconds: 1,
        };
        let id = rec.id.clone();
        b.store.insert(&rec).await.unwrap();
        let status = b
            .await_decision(&id, Duration::from_millis(5))
            .await
            .unwrap();
        assert_eq!(status, ApprovalStatus::Expired);
    }

    // ── decision-source parsers ─────────────────────────────

    fn write_agent_toml(dir: &Path, body: &str) {
        std::fs::write(dir.join("agent.toml"), body).unwrap();
    }

    fn tmp_agent_dir() -> PathBuf {
        let p = std::env::temp_dir().join(format!("duduclaw-approval-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn approval_required_tools_present() {
        let dir = tmp_agent_dir();
        write_agent_toml(
            &dir,
            "[capabilities]\napproval_required_tools = [\"Bash\", \"send_to_agent\"]\n",
        );
        let set = approval_required_tools(&dir);
        assert!(set.contains("Bash"));
        assert!(set.contains("send_to_agent"));
        assert!(tool_requires_approval(&dir, "Bash"));
        assert!(!tool_requires_approval(&dir, "Read"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn approval_required_tools_absent_key_is_empty() {
        let dir = tmp_agent_dir();
        write_agent_toml(&dir, "[capabilities]\nallowed_tools = []\n");
        assert!(approval_required_tools(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn approval_required_tools_missing_file_is_empty() {
        let dir = tmp_agent_dir();
        assert!(approval_required_tools(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn approval_required_tools_malformed_fails_safe_empty() {
        let dir = tmp_agent_dir();
        write_agent_toml(&dir, "this is not = valid toml [[[");
        // Malformed ⇒ empty set (additive gate), never a panic.
        assert!(approval_required_tools(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── P2b: ActionGuard three-value gate ──────────────────────────────────

    #[test]
    fn irreversible_tool_lists_parse_present() {
        let dir = tmp_agent_dir();
        write_agent_toml(
            &dir,
            "[capabilities]\nirreversible_tools = [\"send_email\"]\nmaybe_irreversible_tools = [\"Bash\", \"http_post\"]\n",
        );
        assert!(tool_is_irreversible(&dir, "send_email"));
        assert!(!tool_is_irreversible(&dir, "Bash"));
        assert!(tool_is_maybe_irreversible(&dir, "Bash"));
        assert!(tool_is_maybe_irreversible(&dir, "http_post"));
        assert!(!tool_is_maybe_irreversible(&dir, "send_email"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn irreversible_tool_lists_absent_and_missing_are_empty() {
        // Absent keys.
        let dir = tmp_agent_dir();
        write_agent_toml(&dir, "[capabilities]\nallowed_tools = []\n");
        assert!(irreversible_tools(&dir).is_empty());
        assert!(maybe_irreversible_tools(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
        // Missing file entirely.
        let dir2 = tmp_agent_dir();
        let _ = std::fs::remove_dir_all(&dir2); // remove so agent.toml is absent
        assert!(irreversible_tools(&dir2).is_empty());
        assert!(maybe_irreversible_tools(&dir2).is_empty());
    }

    #[test]
    fn irreversible_tool_lists_malformed_fail_safe_empty() {
        let dir = tmp_agent_dir();
        write_agent_toml(&dir, "not = valid toml [[[");
        assert!(irreversible_tools(&dir).is_empty());
        assert!(maybe_irreversible_tools(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_action_gate_take_the_stricter() {
        use ActionGate::*;
        use JudgeVerdict::*;
        // Never listed → auto.
        assert_eq!(resolve_action_gate(false, false, None), Auto);
        // Always (folds in legacy approval_required_tools) → approval, regardless
        // of maybe membership or any judge verdict.
        assert_eq!(resolve_action_gate(true, false, None), RequireApproval);
        assert_eq!(resolve_action_gate(true, true, Some(Safe)), RequireApproval);
        // Maybe, judge not yet run → consult judge.
        assert_eq!(resolve_action_gate(false, true, None), ConsultJudge);
        // Maybe, judge ruled safe → auto; risky (incl. fail-closed) → approval.
        assert_eq!(resolve_action_gate(false, true, Some(Safe)), Auto);
        assert_eq!(resolve_action_gate(false, true, Some(Risky)), RequireApproval);
    }

    #[test]
    fn rule_requires_approval_parsing() {
        assert!(rule_requires_approval(&json!({"require_approval": true})));
        assert!(!rule_requires_approval(&json!({"require_approval": false})));
        assert!(!rule_requires_approval(&json!({"type": "delegate"})));
        assert!(!rule_requires_approval(&json!({"require_approval": "yes"})));
    }

    #[test]
    fn channel_summary_is_zh_tw_and_xml_safe() {
        let rec = ApprovalRecord {
            id: ApprovalId::new(),
            agent_id: "sales-bot".into(),
            action_kind: "autopilot_action".into(),
            summary: "delete <all> records & drop table".into(),
            payload: json!({}),
            status: ApprovalStatus::Pending,
            created_at: Utc::now().to_rfc3339(),
            decided_at: None,
            decided_by: None,
            ttl_seconds: 300,
        };
        let msg = pending_summary_for_channel(&rec);
        assert!(msg.contains("需要您的核准"));
        assert!(msg.contains("確認"));
        assert!(msg.contains("&lt;all&gt;"));
        assert!(msg.contains("&amp;"));
        assert!(!msg.contains("<all>"));
    }

    #[test]
    fn status_from_db_fails_closed_on_unknown() {
        assert_eq!(ApprovalStatus::from_db("garbage"), ApprovalStatus::Denied);
        assert!(!ApprovalStatus::from_db("garbage").is_granted());
    }
}
