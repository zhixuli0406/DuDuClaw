//! WP3 — task-scoped, revocable capability grants (PORTICO, arXiv:2606.22504).
//!
//! ## What this is
//!
//! A *tool-name × task* grant ledger. An agent listed in
//! `agent.toml [capabilities] scoped_tools = [...]` loses the static right to
//! call those tools: each call is **denied unless the agent currently holds an
//! active grant** for that tool. A grant is minted by a human approval
//! (`capability_request` MCP tool) or atomically at a goal-loop kickoff, and is
//! **revoked when the task phase ends** (accept / reject / needs_human / cancel)
//! or when its hard TTL elapses — whichever comes first. This is the PORTICO
//! thesis applied at the v1.41 granularity: authorization does not outlive the
//! subgoal it was granted for.
//!
//! ## Relationship to [`crate::capability`]
//!
//! [`crate::capability`] (`CapabilityBroker`) is an earlier, **unwired**
//! bearer-handle design: a tool must present a `handle_id` at invoke time and
//! the check is keyed by an opaque epoch. That model does not fit the MCP
//! dispatch gate, where the LLM simply *names* a tool — there is no handle to
//! present. This module answers the question the dispatch gate actually asks:
//! *"does agent A currently hold an active grant for tool T?"* — keyed by
//! `(agent_id, tool)`, not by a bearer handle.
//!
//! The two modules share the `approvals.db` file (WAL → concurrent
//! connections) but own **different tables**: `crate::capability` owns
//! `capability_grants` (+ `capability_closed_scopes`); this module owns
//! `capability_task_grants`. They must never share a table name — the columns
//! are incompatible.
//!
//! ## Fail-closed conventions
//!
//! - A DB that will not open, a query error, or an unparseable `expires_at` all
//!   resolve to **no active grant** (deny). There is no retry-into-allow.
//! - Tool-name matching is **token-anchored** (base name before an optional
//!   `(` qualifier, case-insensitive) — never a substring test (project
//!   convention 2).
//! - `scoped_tools` / `grant_ttl_secs` parsing is fail-safe *empty / default*
//!   like the sibling `[capabilities]` parsers in [`crate::approval`]: the
//!   deny happens at the grant check, so a malformed toml must not brick the
//!   agent by making every tool look scoped.

use std::collections::HashSet;
use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use tokio::sync::Mutex;
use tracing::{info, warn};

// ── Constants ───────────────────────────────────────────────

/// Default grant TTL when the agent does not override it. 1 hour. A grant is a
/// backstop-bounded lease; the primary revocation is the task-phase-end sweep.
pub const DEFAULT_GRANT_TTL_SECS: i64 = 3600;

/// `granted_by` marker for a grant minted via the `capability_request` MCP tool.
pub const GRANTED_BY_REQUEST: &str = "capability_request";

/// `granted_by` marker for a grant minted atomically at goal-loop kickoff.
pub const GRANTED_BY_KICKOFF: &str = "kickoff_approval";

/// `revoke_reason` stamped by [`CapabilityGrantStore::expire_stale`].
pub const REVOKE_REASON_TTL: &str = "ttl_expired";

/// `revoke_reason` stamped when a task phase closes.
pub const REVOKE_REASON_PHASE_END: &str = "task_phase_end";

// ── Tool-name matching (token-anchored, never substring) ────

/// Base name of a tool entry: the part before an optional `(` qualifier,
/// trimmed. `Bash(git:*)` → `Bash`. Mirrors `CapabilitiesConfig::write_tools_allowed`.
fn tool_base(entry: &str) -> &str {
    entry.split('(').next().unwrap_or(entry).trim()
}

/// True when two tool names refer to the same tool (base name, case-insensitive).
pub fn tool_token_matches(a: &str, b: &str) -> bool {
    tool_base(a).eq_ignore_ascii_case(tool_base(b))
}

/// True when `tool` is present in `set` under token-anchored matching.
pub fn set_contains_tool(set: &HashSet<String>, tool: &str) -> bool {
    set.iter().any(|e| tool_token_matches(e, tool))
}

// ── Grant row ───────────────────────────────────────────────

/// One row of `capability_task_grants`.
#[derive(Debug, Clone)]
pub struct GrantRow {
    pub id: String,
    pub agent_id: String,
    /// `None` ⇒ agent-level (session) grant — the fallback when the MCP layer
    /// cannot associate a current task.
    pub task_id: Option<String>,
    pub tool: String,
    pub granted_at: String,
    pub granted_by: String,
    pub expires_at: String,
    pub revoked_at: Option<String>,
    pub revoke_reason: Option<String>,
}

impl GrantRow {
    /// Parsed expiry instant. `None` (⇒ treated as expired, fail-closed) if the
    /// timestamp cannot be parsed.
    fn expires_at_dt(&self) -> Option<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(&self.expires_at)
            .ok()
            .map(|d| d.with_timezone(&Utc))
    }

    /// True when the row is a live, unrevoked, unexpired grant at `now`.
    fn is_active(&self, now: DateTime<Utc>) -> bool {
        if self.revoked_at.is_some() {
            return false;
        }
        match self.expires_at_dt() {
            Some(exp) => now < exp,
            None => false, // unparseable ⇒ fail closed (inactive)
        }
    }
}

fn row_to_grant(row: &rusqlite::Row) -> rusqlite::Result<GrantRow> {
    Ok(GrantRow {
        id: row.get(0)?,
        agent_id: row.get(1)?,
        task_id: row.get(2)?,
        tool: row.get(3)?,
        granted_at: row.get(4)?,
        granted_by: row.get(5)?,
        expires_at: row.get(6)?,
        revoked_at: row.get(7)?,
        revoke_reason: row.get(8)?,
    })
}

// ── Schema (shared by async store + sync spawn helper) ──────

fn init_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA busy_timeout=5000;

         CREATE TABLE IF NOT EXISTS capability_task_grants (
             id            TEXT PRIMARY KEY,
             agent_id      TEXT NOT NULL,
             task_id       TEXT,
             tool          TEXT NOT NULL,
             granted_at    TEXT NOT NULL,
             granted_by    TEXT NOT NULL,
             expires_at    TEXT NOT NULL,
             revoked_at    TEXT,
             revoke_reason TEXT
         );

         CREATE INDEX IF NOT EXISTS idx_captg_agent ON capability_task_grants(agent_id);
         CREATE INDEX IF NOT EXISTS idx_captg_task  ON capability_task_grants(task_id);",
    )
    .map_err(|e| format!("init capability_task_grants schema: {e}"))?;
    Ok(())
}

/// SELECT all not-yet-revoked grant rows for one agent. Shared by the async
/// store and the sync spawn helper so the query lives in one place. Expiry is
/// filtered in Rust (parses `expires_at`, fail-closed) rather than a lexical SQL
/// compare, so an offset-format quirk can never keep an expired grant alive.
fn select_nonrevoked_for_agent(conn: &Connection, agent_id: &str) -> Result<Vec<GrantRow>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, agent_id, task_id, tool, granted_at, granted_by,
                    expires_at, revoked_at, revoke_reason
             FROM capability_task_grants
             WHERE agent_id = ?1 AND revoked_at IS NULL",
        )
        .map_err(|e| format!("prepare select_nonrevoked: {e}"))?;
    let rows = stmt
        .query_map(params![agent_id], row_to_grant)
        .map_err(|e| format!("query select_nonrevoked: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("collect select_nonrevoked: {e}"))?;
    Ok(rows)
}

// ── Async store ─────────────────────────────────────────────

/// SQLite-backed store for task-scoped capability grants. Mirrors the
/// `ApprovalStore` / `CapabilityStore` idioms: `Mutex<Connection>`, WAL,
/// `busy_timeout`, self-healing schema, parameterized SQL only.
pub struct CapabilityGrantStore {
    conn: Mutex<Connection>,
}

impl CapabilityGrantStore {
    /// Open (or create) the store at `<home>/approvals.db` (shared file, own
    /// table). Fail-closed: any open error propagates so callers can deny.
    pub fn open(home_dir: &Path) -> Result<Self, String> {
        let db_path = home_dir.join("approvals.db");
        let conn =
            Connection::open(&db_path).map_err(|e| format!("open capability grant store: {e}"))?;
        init_schema(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// In-memory store for tests.
    pub fn open_in_memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory().map_err(|e| format!("open in-memory: {e}"))?;
        init_schema(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Mint a grant. `task_id = None` records an agent-level (session) grant.
    /// A non-positive `ttl_seconds` falls back to [`DEFAULT_GRANT_TTL_SECS`]
    /// (a zero/negative TTL would silently expire immediately — a footgun).
    /// Returns the new grant id.
    pub async fn grant(
        &self,
        agent_id: &str,
        task_id: Option<&str>,
        tool: &str,
        granted_by: &str,
        ttl_seconds: i64,
    ) -> Result<String, String> {
        let ttl = if ttl_seconds > 0 {
            ttl_seconds
        } else {
            DEFAULT_GRANT_TTL_SECS
        };
        let now = Utc::now();
        let id = uuid::Uuid::new_v4().to_string();
        let expires_at = (now + chrono::Duration::seconds(ttl)).to_rfc3339();
        {
            let conn = self.conn.lock().await;
            conn.execute(
                "INSERT INTO capability_task_grants
                    (id, agent_id, task_id, tool, granted_at, granted_by, expires_at,
                     revoked_at, revoke_reason)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, NULL)",
                params![
                    id,
                    agent_id,
                    task_id,
                    tool,
                    now.to_rfc3339(),
                    granted_by,
                    expires_at,
                ],
            )
            .map_err(|e| format!("insert grant: {e}"))?;
        }
        info!(
            grant_id = %id,
            agent_id,
            task_id = task_id.unwrap_or("<agent-level>"),
            tool,
            granted_by,
            ttl_seconds = ttl,
            "capability grant minted (task-scoped)"
        );
        Ok(id)
    }

    /// All currently-active (unrevoked, unexpired) grants for an agent.
    /// Errors bubble up so the caller can fail closed.
    pub async fn active_grants(&self, agent_id: &str) -> Result<Vec<GrantRow>, String> {
        let now = Utc::now();
        let conn = self.conn.lock().await;
        let rows = select_nonrevoked_for_agent(&conn, agent_id)?;
        Ok(rows.into_iter().filter(|r| r.is_active(now)).collect())
    }

    /// True when the agent currently holds an active grant for `tool`.
    /// **Fail-closed:** any store/query error resolves to `false` (deny).
    /// Tool match is token-anchored, never substring.
    pub async fn has_active_grant(&self, agent_id: &str, tool: &str) -> bool {
        match self.active_grants(agent_id).await {
            Ok(rows) => rows.iter().any(|r| tool_token_matches(&r.tool, tool)),
            Err(e) => {
                warn!(agent_id, tool, error = %e, "capability grant lookup failed — denying (fail-closed)");
                false
            }
        }
    }

    /// Revoke every live grant bound to a task. Returns the number revoked.
    /// Idempotent (a second call revokes 0). Used at task-phase-end.
    pub async fn revoke_for_task(&self, task_id: &str, reason: &str) -> Result<u64, String> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE capability_task_grants
                 SET revoked_at = ?1, revoke_reason = ?2
                 WHERE task_id = ?3 AND revoked_at IS NULL",
                params![now, reason, task_id],
            )
            .map_err(|e| format!("revoke_for_task: {e}"))?;
        if n > 0 {
            info!(
                task_id,
                reason,
                revoked = n,
                "capability grants revoked (task phase end)"
            );
        }
        Ok(n as u64)
    }

    /// Sweep: stamp `revoked_at` on every unrevoked grant past its TTL (or with
    /// an unparseable expiry — fail-closed). Returns the number swept. Active
    /// lookups already exclude expired rows; this keeps the table tidy and
    /// makes expiry auditable. Housekeeping only — safe to call periodically.
    pub async fn expire_stale(&self) -> Result<u64, String> {
        let now = Utc::now();
        let now_s = now.to_rfc3339();
        let conn = self.conn.lock().await;
        // Collect unrevoked rows (any agent), decide expiry in Rust.
        let mut stmt = conn
            .prepare(
                "SELECT id, agent_id, task_id, tool, granted_at, granted_by,
                        expires_at, revoked_at, revoke_reason
                 FROM capability_task_grants
                 WHERE revoked_at IS NULL",
            )
            .map_err(|e| format!("prepare expire_stale: {e}"))?;
        let rows = stmt
            .query_map([], row_to_grant)
            .map_err(|e| format!("query expire_stale: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect expire_stale: {e}"))?;
        let mut swept = 0u64;
        for r in rows {
            if !r.is_active(now) {
                let n = conn
                    .execute(
                        "UPDATE capability_task_grants
                         SET revoked_at = ?1, revoke_reason = ?2
                         WHERE id = ?3 AND revoked_at IS NULL",
                        params![now_s, REVOKE_REASON_TTL, r.id],
                    )
                    .map_err(|e| format!("expire_stale update: {e}"))?;
                swept += n as u64;
            }
        }
        if swept > 0 {
            info!(count = swept, "capability grants expired by TTL");
        }
        Ok(swept)
    }
}

// ── agent.toml [capabilities] parsers ───────────────────────

/// Parse `agent.toml [capabilities] scoped_tools = [...]` — the third,
/// orthogonal tool list (alongside allowed/denied). Listed tools require an
/// active task-scoped grant to run.
///
/// **Fail-safe empty** (documented): a missing file/key or malformed toml
/// returns an empty set. The deny is enforced at the grant check; a malformed
/// toml must NOT make every tool look scoped and brick the agent. Mirrors the
/// `approval_required_tools` fail-safe.
pub fn scoped_tools(agent_dir: &Path) -> HashSet<String> {
    let path = agent_dir.join("agent.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return HashSet::new();
    };
    let value: toml::Value = match toml::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            warn!(?path, error = %e, "malformed agent.toml — scoped_tools defaults to empty (fail-safe)");
            return HashSet::new();
        }
    };
    value
        .get("capabilities")
        .and_then(|c| c.get("scoped_tools"))
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Parse `agent.toml [capabilities] grant_ttl_secs`. Absent / malformed / a
/// non-positive value all fall back to [`DEFAULT_GRANT_TTL_SECS`].
pub fn grant_ttl_secs(agent_dir: &Path) -> i64 {
    let path = agent_dir.join("agent.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return DEFAULT_GRANT_TTL_SECS;
    };
    let value: toml::Value = match toml::from_str(&text) {
        Ok(v) => v,
        Err(_) => return DEFAULT_GRANT_TTL_SECS,
    };
    value
        .get("capabilities")
        .and_then(|c| c.get("grant_ttl_secs"))
        .and_then(|t| t.as_integer())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_GRANT_TTL_SECS)
}

// ── Spawn-layer helper (sync, defense-in-depth) ─────────────

/// The tools an agent's spawned CLI should have added to `--disallowedTools`
/// because they are `scoped_tools` **without** an active grant. Both the bare
/// tool name and its `mcp__duduclaw__<name>` namespaced form are returned, so
/// the disallow matches whether the tool is a native CLI tool or a DuDuClaw MCP
/// tool (extra non-matching entries are harmless no-ops).
///
/// This is the *auxiliary* enforcement layer — the MCP dispatch gate is the
/// primary, complete-mediation choke point. It is a sync helper (opening a
/// fresh short-lived rusqlite connection) so it can be called from the sync
/// `prepare_claude_cmd` as well as the async PTY spawn path.
///
/// **Fail-closed:** if the grant store cannot be opened/queried, *every*
/// scoped tool is disallowed (all scoped, no grant honored). An empty
/// `scoped_tools` returns an empty vec with zero DB work (the common path).
pub fn scoped_disallow_for_agent_dir(agent_dir: &Path) -> Vec<String> {
    let scoped = scoped_tools(agent_dir);
    if scoped.is_empty() {
        return Vec::new(); // zero-overhead common path
    }
    // Derive home_dir + agent_id from `<home>/agents/<agent_id>`.
    let agent_id = agent_dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let home_dir = agent_dir.parent().and_then(|p| p.parent());

    // Set of tools the agent currently has an active grant for. On ANY failure
    // → empty set (⇒ all scoped tools disallowed, fail-closed).
    let granted: HashSet<String> = match home_dir {
        Some(home) if !agent_id.is_empty() => {
            active_grant_tools_sync(home, agent_id).unwrap_or_default()
        }
        _ => HashSet::new(),
    };

    let mut out = Vec::new();
    for tool in scoped {
        let has_grant = granted.iter().any(|g| tool_token_matches(g, &tool));
        if !has_grant {
            out.push(format!("mcp__duduclaw__{tool}"));
            out.push(tool);
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Synchronous read of the set of tools an agent currently holds an active
/// grant for. Opens a fresh short-lived connection to the shared `approvals.db`
/// (WAL → coexists with the async store). Returns `Err` on any store failure so
/// the caller can fail closed.
fn active_grant_tools_sync(home_dir: &Path, agent_id: &str) -> Result<HashSet<String>, String> {
    let db_path = home_dir.join("approvals.db");
    let conn = Connection::open(&db_path)
        .map_err(|e| format!("open capability grant store (sync): {e}"))?;
    // busy_timeout for the WAL-shared file; ignore PRAGMA errors (best-effort).
    let _ = conn.busy_timeout(std::time::Duration::from_millis(5000));
    init_schema(&conn)?;
    let now = Utc::now();
    let rows = select_nonrevoked_for_agent(&conn, agent_id)?;
    Ok(rows
        .into_iter()
        .filter(|r| r.is_active(now))
        .map(|r| r.tool)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> CapabilityGrantStore {
        CapabilityGrantStore::open_in_memory().unwrap()
    }

    fn tmp_agent_dir() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("duduclaw-capgrant-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    // ── grant / active / has_active ─────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn grant_then_has_active() {
        let s = store();
        s.grant(
            "agent-x",
            Some("task-1"),
            "send_message",
            GRANTED_BY_REQUEST,
            60,
        )
        .await
        .unwrap();
        assert!(s.has_active_grant("agent-x", "send_message").await);
        // A different tool is not granted.
        assert!(!s.has_active_grant("agent-x", "execute_program").await);
        // A different agent does not inherit the grant.
        assert!(!s.has_active_grant("agent-y", "send_message").await);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tool_match_is_token_anchored_not_substring() {
        let s = store();
        s.grant("a", Some("t"), "send", GRANTED_BY_REQUEST, 60)
            .await
            .unwrap();
        // "send" must NOT match "send_message" (substring would; token-anchored won't).
        assert!(!s.has_active_grant("a", "send_message").await);
        assert!(s.has_active_grant("a", "send").await);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn qualified_tool_matches_base_name() {
        let s = store();
        s.grant("a", Some("t"), "Bash", GRANTED_BY_REQUEST, 60)
            .await
            .unwrap();
        // A qualified query resolves to the base name.
        assert!(s.has_active_grant("a", "Bash(git:*)").await);
    }

    // ── TTL expiry ──────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn grant_expires_by_ttl() {
        let s = store();
        // Insert an already-expired grant directly.
        {
            let conn = s.conn.lock().await;
            conn.execute(
                "INSERT INTO capability_task_grants
                    (id, agent_id, task_id, tool, granted_at, granted_by, expires_at, revoked_at, revoke_reason)
                 VALUES ('g1','agent-x','task-1','send_message', ?1, 'capability_request', ?2, NULL, NULL)",
                params![
                    (Utc::now() - chrono::Duration::seconds(600)).to_rfc3339(),
                    (Utc::now() - chrono::Duration::seconds(1)).to_rfc3339(),
                ],
            )
            .unwrap();
        }
        // Expired ⇒ not active (fail-closed).
        assert!(!s.has_active_grant("agent-x", "send_message").await);
        // expire_stale stamps it revoked.
        assert_eq!(s.expire_stale().await.unwrap(), 1);
        let rows = {
            let conn = s.conn.lock().await;
            let mut stmt = conn
                .prepare("SELECT revoke_reason FROM capability_task_grants WHERE id='g1'")
                .unwrap();
            stmt.query_row([], |r| r.get::<_, Option<String>>(0))
                .unwrap()
        };
        assert_eq!(rows.as_deref(), Some(REVOKE_REASON_TTL));
        // Second sweep is a no-op.
        assert_eq!(s.expire_stale().await.unwrap(), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unparseable_expiry_is_inactive() {
        let s = store();
        {
            let conn = s.conn.lock().await;
            conn.execute(
                "INSERT INTO capability_task_grants
                    (id, agent_id, task_id, tool, granted_at, granted_by, expires_at, revoked_at, revoke_reason)
                 VALUES ('g2','a','t','send_message', ?1, 'x', 'not-a-timestamp', NULL, NULL)",
                params![Utc::now().to_rfc3339()],
            )
            .unwrap();
        }
        assert!(!s.has_active_grant("a", "send_message").await);
    }

    // ── revoke_for_task ─────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn revoke_for_task_kills_grants() {
        let s = store();
        s.grant(
            "a",
            Some("task-1"),
            "send_message",
            GRANTED_BY_REQUEST,
            3600,
        )
        .await
        .unwrap();
        s.grant(
            "a",
            Some("task-1"),
            "execute_program",
            GRANTED_BY_REQUEST,
            3600,
        )
        .await
        .unwrap();
        s.grant(
            "a",
            Some("task-2"),
            "send_message",
            GRANTED_BY_REQUEST,
            3600,
        )
        .await
        .unwrap();
        assert!(s.has_active_grant("a", "send_message").await);

        let n = s
            .revoke_for_task("task-1", REVOKE_REASON_PHASE_END)
            .await
            .unwrap();
        assert_eq!(n, 2);
        // task-1's send_message is gone, but task-2 still holds one.
        assert!(s.has_active_grant("a", "send_message").await); // task-2 grant remains
        assert!(!s.has_active_grant("a", "execute_program").await);

        // Kill task-2 too.
        assert_eq!(
            s.revoke_for_task("task-2", REVOKE_REASON_PHASE_END)
                .await
                .unwrap(),
            1
        );
        assert!(!s.has_active_grant("a", "send_message").await);
        // Idempotent.
        assert_eq!(
            s.revoke_for_task("task-1", REVOKE_REASON_PHASE_END)
                .await
                .unwrap(),
            0
        );
    }

    // ── fail-closed: unreadable store ───────────────────────

    #[test]
    fn grant_store_unreadable_denies_via_sync_helper() {
        // Point at a home whose approvals.db path is actually a directory →
        // Connection::open fails → the sync helper returns Err → scoped_disallow
        // treats every scoped tool as ungranted (disallowed).
        let dir = tmp_agent_dir();
        std::fs::write(
            dir.join("agent.toml"),
            "[capabilities]\nscoped_tools = [\"send_message\"]\n",
        )
        .unwrap();
        // Make <home>/approvals.db a directory so open() fails.
        let home = dir.parent().unwrap();
        let bad_db = home.join("approvals.db");
        // Only create the blocker if it doesn't already exist as a file.
        if !bad_db.exists() {
            std::fs::create_dir_all(&bad_db).unwrap();
        }
        let disallow = scoped_disallow_for_agent_dir(&dir);
        // Fail-closed: the scoped tool is disallowed (both forms present).
        assert!(disallow.contains(&"send_message".to_string()));
        assert!(disallow.contains(&"mcp__duduclaw__send_message".to_string()));
        let _ = std::fs::remove_dir_all(home.join(dir.file_name().unwrap()));
        let _ = std::fs::remove_dir_all(&bad_db);
    }

    // ── scoped_tools parser ─────────────────────────────────

    #[test]
    fn scoped_tools_parse_present_absent_malformed() {
        let dir = tmp_agent_dir();
        std::fs::write(
            dir.join("agent.toml"),
            "[capabilities]\nscoped_tools = [\"send_message\", \"execute_program\"]\n",
        )
        .unwrap();
        let set = scoped_tools(&dir);
        assert!(set.contains("send_message"));
        assert!(set.contains("execute_program"));
        assert!(set_contains_tool(&set, "send_message"));
        assert!(!set_contains_tool(&set, "memory_search"));
        std::fs::remove_dir_all(&dir).unwrap();

        // Absent key → empty.
        let dir2 = tmp_agent_dir();
        std::fs::write(
            dir2.join("agent.toml"),
            "[capabilities]\nallowed_tools = []\n",
        )
        .unwrap();
        assert!(scoped_tools(&dir2).is_empty());
        std::fs::remove_dir_all(&dir2).unwrap();

        // Malformed → empty (fail-safe, never panic).
        let dir3 = tmp_agent_dir();
        std::fs::write(dir3.join("agent.toml"), "not = valid [[[").unwrap();
        assert!(scoped_tools(&dir3).is_empty());
        std::fs::remove_dir_all(&dir3).unwrap();

        // Missing file → empty.
        let dir4 = tmp_agent_dir();
        std::fs::remove_dir_all(&dir4).unwrap();
        assert!(scoped_tools(&dir4).is_empty());
    }

    #[test]
    fn grant_ttl_secs_parse() {
        let dir = tmp_agent_dir();
        std::fs::write(
            dir.join("agent.toml"),
            "[capabilities]\ngrant_ttl_secs = 120\n",
        )
        .unwrap();
        assert_eq!(grant_ttl_secs(&dir), 120);
        std::fs::remove_dir_all(&dir).unwrap();

        // Non-positive / absent → default.
        let dir2 = tmp_agent_dir();
        std::fs::write(
            dir2.join("agent.toml"),
            "[capabilities]\ngrant_ttl_secs = 0\n",
        )
        .unwrap();
        assert_eq!(grant_ttl_secs(&dir2), DEFAULT_GRANT_TTL_SECS);
        std::fs::remove_dir_all(&dir2).unwrap();

        let dir3 = tmp_agent_dir();
        std::fs::write(dir3.join("agent.toml"), "[capabilities]\n").unwrap();
        assert_eq!(grant_ttl_secs(&dir3), DEFAULT_GRANT_TTL_SECS);
        std::fs::remove_dir_all(&dir3).unwrap();
    }

    // ── spawn-layer disallow synthesis ──────────────────────

    #[test]
    fn non_scoped_agent_has_empty_disallow() {
        // No scoped_tools → zero-overhead empty (byte-identical to legacy).
        let dir = tmp_agent_dir();
        std::fs::write(
            dir.join("agent.toml"),
            "[capabilities]\nallowed_tools = []\n",
        )
        .unwrap();
        assert!(scoped_disallow_for_agent_dir(&dir).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn spawn_disallow_reflects_active_grant() {
        // Build a real <home>/agents/<id> layout so the sync helper can derive
        // home + agent_id and read the shared approvals.db.
        let home = std::env::temp_dir().join(format!("duduclaw-caphome-{}", uuid::Uuid::new_v4()));
        let agent_id = "agent-z";
        let agent_dir = home.join("agents").join(agent_id);
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(
            agent_dir.join("agent.toml"),
            "[capabilities]\nscoped_tools = [\"send_message\", \"execute_program\"]\n",
        )
        .unwrap();

        // No grants yet → both scoped tools disallowed (bare + namespaced).
        let d0 = scoped_disallow_for_agent_dir(&agent_dir);
        assert!(d0.contains(&"send_message".to_string()));
        assert!(d0.contains(&"mcp__duduclaw__send_message".to_string()));
        assert!(d0.contains(&"execute_program".to_string()));

        // Grant send_message via the on-disk store (same approvals.db).
        let s = CapabilityGrantStore::open(&home).unwrap();
        s.grant(
            agent_id,
            Some("task-1"),
            "send_message",
            GRANTED_BY_REQUEST,
            3600,
        )
        .await
        .unwrap();

        let d1 = scoped_disallow_for_agent_dir(&agent_dir);
        // Granted tool drops out of the disallow list; ungranted stays.
        assert!(!d1.contains(&"send_message".to_string()));
        assert!(!d1.contains(&"mcp__duduclaw__send_message".to_string()));
        assert!(d1.contains(&"execute_program".to_string()));

        std::fs::remove_dir_all(&home).unwrap();
    }
}
