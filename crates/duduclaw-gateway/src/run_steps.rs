//! Durable per-run step-event store (G12 run inspector upgrade).
//!
//! The live `StepTracker` step stream and `TodoWrite` progress boards were
//! previously ephemeral — streamed to channel edit-in-place / WebChat frames
//! and gone. This module persists those events into `<home>/run_steps.db`
//! (SQLite, WAL, owner-only permissions like the other gateway stores) so
//! `runs.get` can replay real tool steps and todo snapshots.
//!
//! Design constraints (all deliberate):
//! - **Best-effort writes.** The reply flow must NEVER block or fail because
//!   of this store: open/insert errors are logged at `debug` and dropped.
//! - **Bounded.** Retention is pruned *on the write path* (every
//!   [`PRUNE_EVERY_INSERTS`]th insert) — no new background scheduler. Rows
//!   older than [`RETENTION_DAYS`] days are deleted globally, and each agent
//!   is capped at [`PER_AGENT_ROW_CAP`] most-recent rows.
//! - **No secrets on disk.** `payload_preview` comes from the same
//!   already-rendered progress labels the live stream shows, additionally
//!   passed through [`mask_secretish`] (values following secret-ish keys are
//!   redacted) and capped via `truncate_chars` (never byte slicing).
//! - Parameterized SQL only; idempotent schema (`CREATE TABLE IF NOT EXISTS`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use rusqlite::{params, Connection};
use tracing::debug;

/// Event kinds persisted by this store. `runs.get` renders these
/// kind-appropriately; anything else would be dishonest to claim.
pub const KIND_TOOL_STEP: &str = "tool_step";
pub const KIND_TODO_UPDATE: &str = "todo_update";

/// Days of history kept (time-based retention).
pub const RETENTION_DAYS: i64 = 7;
/// Max rows kept per agent (size-based retention).
pub const PER_AGENT_ROW_CAP: usize = 10_000;
/// Prune is piggybacked on every Nth insert (no dedicated scheduler).
pub const PRUNE_EVERY_INSERTS: u64 = 1_000;

/// CJK-safe char caps (project convention 1 — never raw byte slicing).
const LABEL_CHAR_CAP: usize = 120;
const PAYLOAD_PREVIEW_CHAR_CAP: usize = 500;

/// One persisted step row, as read back for `runs.get`.
#[derive(Debug, Clone, PartialEq)]
pub struct RunStepRow {
    pub agent_id: String,
    /// Channel session key ("telegram:12345", …) — matches `sessions.db` ids.
    pub session_key: String,
    /// RFC3339 UTC.
    pub ts: String,
    /// [`KIND_TOOL_STEP`] | [`KIND_TODO_UPDATE`].
    pub kind: String,
    /// Tool name for tool steps; "done/total" for todo boards.
    pub label: String,
    /// Already-rendered, secret-masked, char-capped preview.
    pub payload_preview: String,
    /// Per-invocation monotonic sequence (orders ties within one second).
    pub seq: i64,
}

/// SQLite-backed store. Same idioms as `approval.rs` / `custom_skills.rs`:
/// WAL + `busy_timeout`, self-healing schema, parameterized SQL only. Uses a
/// `std::sync::Mutex` (never held across an await) because appends happen
/// inline in the CLI streaming loop and must stay cheap.
pub struct RunStepStore {
    conn: Mutex<Connection>,
    /// Total inserts through this handle — drives piggybacked pruning.
    insert_count: AtomicU64,
}

impl RunStepStore {
    /// Open (or create) `<home>/run_steps.db`.
    pub fn open(home_dir: &Path) -> Result<Self, String> {
        let db_path = home_dir.join("run_steps.db");
        let conn =
            Connection::open(&db_path).map_err(|e| format!("open run_steps store: {e}"))?;
        Self::init_schema(&conn)?;
        // Owner-only, like the sibling session/key stores (0600). Best-effort
        // on non-unix (no mode bits there).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&db_path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(Self {
            conn: Mutex::new(conn),
            insert_count: AtomicU64::new(0),
        })
    }

    /// In-memory store for tests (no file, no WAL persistence).
    pub fn open_in_memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory().map_err(|e| format!("open in-memory: {e}"))?;
        Self::init_schema(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            insert_count: AtomicU64::new(0),
        })
    }

    fn init_schema(conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=5000;

             CREATE TABLE IF NOT EXISTS run_steps (
                 id              INTEGER PRIMARY KEY AUTOINCREMENT,
                 agent_id        TEXT NOT NULL,
                 session_key     TEXT NOT NULL,
                 ts              TEXT NOT NULL,
                 kind            TEXT NOT NULL,
                 label           TEXT NOT NULL,
                 payload_preview TEXT NOT NULL DEFAULT '',
                 seq             INTEGER NOT NULL DEFAULT 0
             );

             CREATE INDEX IF NOT EXISTS idx_run_steps_session ON run_steps(session_key, id);
             CREATE INDEX IF NOT EXISTS idx_run_steps_agent   ON run_steps(agent_id, id);
             CREATE INDEX IF NOT EXISTS idx_run_steps_ts      ON run_steps(ts);",
        )
        .map_err(|e| format!("init run_steps schema: {e}"))?;
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        // A poisoned lock only means another thread panicked mid-query; the
        // connection itself is still usable — recover instead of propagating.
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Append one step event. Label/preview are secret-masked and char-capped
    /// here (single enforcement point). Every [`PRUNE_EVERY_INSERTS`]th insert
    /// also runs retention pruning for the writing agent.
    pub fn append(
        &self,
        agent_id: &str,
        session_key: &str,
        kind: &str,
        label: &str,
        payload_preview: &str,
        seq: i64,
    ) -> Result<(), String> {
        let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let label = duduclaw_core::truncate_chars(&mask_secretish(label), LABEL_CHAR_CAP);
        let preview = duduclaw_core::truncate_chars(
            &mask_secretish(payload_preview),
            PAYLOAD_PREVIEW_CHAR_CAP,
        );
        {
            let conn = self.lock();
            conn.execute(
                "INSERT INTO run_steps (agent_id, session_key, ts, kind, label, payload_preview, seq)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![agent_id, session_key, ts, kind, label, preview, seq],
            )
            .map_err(|e| format!("insert run step: {e}"))?;
        }
        let n = self.insert_count.fetch_add(1, Ordering::Relaxed) + 1;
        if n % PRUNE_EVERY_INSERTS == 0 {
            // Retention errors must not fail the write that triggered them.
            if let Err(e) = self.prune(agent_id) {
                debug!(error = %e, "run_steps prune failed (ignored)");
            }
        }
        Ok(())
    }

    /// Fire-and-forget append for the reply hot path: any error is logged at
    /// `debug` and dropped — the streaming loop never sees a failure.
    pub fn append_best_effort(
        &self,
        agent_id: &str,
        session_key: &str,
        kind: &str,
        label: &str,
        payload_preview: &str,
        seq: i64,
    ) {
        if let Err(e) = self.append(agent_id, session_key, kind, label, payload_preview, seq) {
            debug!(error = %e, kind, "run_steps append failed (dropped, best-effort)");
        }
    }

    /// Retention: delete rows older than [`RETENTION_DAYS`] (all agents —
    /// one cheap indexed range delete) and cap **every** agent at
    /// [`PER_AGENT_ROW_CAP`] most-recent rows.
    ///
    /// The cap sweep is global (partitioned by `agent_id`), not scoped to the
    /// caller: the insert counter that triggers pruning is shared across all
    /// agents, so a hot agent would otherwise advance the counter while a cold
    /// agent's own rows never trigger a cap sweep and could grow unbounded
    /// between the 7-day age prunes. `agent_id` is retained in the signature
    /// only for call-site clarity/tests; it does not scope the delete.
    pub fn prune(&self, _agent_id: &str) -> Result<usize, String> {
        let cutoff = (chrono::Utc::now() - chrono::Duration::days(RETENTION_DAYS))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let conn = self.lock();
        let by_age = conn
            .execute("DELETE FROM run_steps WHERE ts < ?1", params![cutoff])
            .map_err(|e| format!("prune run_steps by age: {e}"))?;
        let by_cap = conn
            .execute(
                "DELETE FROM run_steps WHERE id IN (
                     SELECT id FROM (
                         SELECT id, ROW_NUMBER() OVER (
                             PARTITION BY agent_id ORDER BY id DESC
                         ) AS rn FROM run_steps
                     ) WHERE rn > ?1
                 )",
                params![PER_AGENT_ROW_CAP as i64],
            )
            .map_err(|e| format!("prune run_steps by cap: {e}"))?;
        Ok(by_age + by_cap)
    }

    /// Most-recent rows for one session key (newest-first in SQL, returned
    /// oldest-first for chronological merging). `runs.get` window-filters the
    /// result with its own timestamp parsing.
    pub fn recent_for_session(
        &self,
        session_key: &str,
        cap: usize,
    ) -> Result<Vec<RunStepRow>, String> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT agent_id, session_key, ts, kind, label, payload_preview, seq
                 FROM run_steps WHERE session_key = ?1
                 ORDER BY id DESC LIMIT ?2",
            )
            .map_err(|e| format!("prepare recent_for_session: {e}"))?;
        let mut rows = stmt
            .query_map(params![session_key, cap as i64], |row| {
                Ok(RunStepRow {
                    agent_id: row.get(0)?,
                    session_key: row.get(1)?,
                    ts: row.get(2)?,
                    kind: row.get(3)?,
                    label: row.get(4)?,
                    payload_preview: row.get(5)?,
                    seq: row.get(6)?,
                })
            })
            .map_err(|e| format!("query recent_for_session: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect recent_for_session: {e}"))?;
        rows.reverse(); // oldest-first
        Ok(rows)
    }

    /// Most-recent `tool_step` metadata `(session_key, ts)` — the cheap
    /// projection `runs.list` uses to count real steps per run window.
    /// Empty `agent_filter` means all agents (mirrors `query_run_msg_rows`).
    pub fn recent_tool_step_meta(
        &self,
        agent_filter: &str,
        cap: usize,
    ) -> Result<Vec<(String, String)>, String> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT session_key, ts FROM run_steps
                 WHERE kind = ?1 AND (?2 = '' OR agent_id = ?2)
                 ORDER BY id DESC LIMIT ?3",
            )
            .map_err(|e| format!("prepare recent_tool_step_meta: {e}"))?;
        let rows = stmt
            .query_map(params![KIND_TOOL_STEP, agent_filter, cap as i64], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| format!("query recent_tool_step_meta: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect recent_tool_step_meta: {e}"))?;
        Ok(rows)
    }

    #[cfg(test)]
    fn row_count(&self) -> usize {
        let conn = self.lock();
        conn.query_row("SELECT COUNT(*) FROM run_steps", [], |r| r.get::<_, i64>(0))
            .unwrap_or(0) as usize
    }
}

// ── Shared per-home handle (opened once, reused by every reply turn) ──

fn store_cache() -> &'static Mutex<HashMap<PathBuf, Arc<RunStepStore>>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Arc<RunStepStore>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Shared store handle for `home_dir`. Open failure ⇒ `None` (logged at
/// `debug`) — callers skip persistence, never fail the reply.
pub fn shared_store(home_dir: &Path) -> Option<Arc<RunStepStore>> {
    {
        let cache = store_cache().lock().unwrap_or_else(|p| p.into_inner());
        if let Some(store) = cache.get(home_dir) {
            return Some(store.clone());
        }
    }
    // Open outside the lock (filesystem I/O), then publish.
    match RunStepStore::open(home_dir) {
        Ok(store) => {
            let arc = Arc::new(store);
            let mut cache = store_cache().lock().unwrap_or_else(|p| p.into_inner());
            Some(cache.entry(home_dir.to_path_buf()).or_insert(arc).clone())
        }
        Err(e) => {
            debug!(error = %e, "run_steps store unavailable — step persistence skipped");
            None
        }
    }
}

// ── Secret masking ──────────────────────────────────────────

/// Keys whose following value is redacted in previews. Matched
/// case-insensitively as substrings of the key token — previews are
/// human-facing breadcrumbs, so over-masking a value is always safer than
/// persisting a credential.
const SECRETISH_KEYS: &[&str] = &[
    "token", "secret", "passwd", "password", "api_key", "apikey", "authorization", "bearer",
    "credential", "private_key",
];

/// Mask values that follow secret-ish key indicators in an arbitrary
/// already-rendered label (e.g. a Bash `command` summary like
/// `curl -H "Authorization: Bearer sk-…"`). Values end at whitespace or a
/// closing quote. Pure, allocation-light, no regex dependency.
pub(crate) fn mask_secretish(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while !rest.is_empty() {
        // Find the earliest secret-ish key occurrence (case-insensitive).
        let lower = rest.to_lowercase();
        let hit = SECRETISH_KEYS
            .iter()
            .filter_map(|k| lower.find(*k).map(|pos| (pos, k.len())))
            .min_by_key(|(pos, _)| *pos);
        let Some((pos, key_len)) = hit else {
            out.push_str(rest);
            break;
        };
        // `lower` and `rest` have identical char/byte structure only for
        // ASCII; to stay boundary-safe on CJK input, re-check the boundary.
        if !rest.is_char_boundary(pos) || !rest.is_char_boundary(pos + key_len) {
            // Multi-byte case-fold shifted offsets — bail out conservatively
            // by masking nothing further (labels are already short).
            out.push_str(rest);
            break;
        }
        out.push_str(&rest[..pos + key_len]);
        rest = &rest[pos + key_len..];
        // Skip separators between the key and its value (`: `, `=`, `" `…).
        let after_sep = rest
            .find(|c: char| !matches!(c, ':' | '=' | ' ' | '"' | '\''))
            .unwrap_or(rest.len());
        out.push_str(&rest[..after_sep]);
        rest = &rest[after_sep..];
        // Redact the value: everything up to whitespace / quote / `&`. A
        // scheme word that is itself a secret-ish key ("Bearer" after
        // "Authorization:") is kept and the token AFTER it is redacted —
        // otherwise `Authorization: Bearer sk-…` would consume "Bearer" as
        // the value and leak the real credential.
        loop {
            let val_end = rest
                .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '&'))
                .unwrap_or(rest.len());
            if val_end == 0 {
                break;
            }
            let token = &rest[..val_end];
            if SECRETISH_KEYS.contains(&token.to_lowercase().as_str()) {
                out.push_str(token);
                rest = &rest[val_end..];
                let sep = rest
                    .find(|c: char| !matches!(c, ':' | '=' | ' ' | '"' | '\''))
                    .unwrap_or(rest.len());
                out.push_str(&rest[..sep]);
                rest = &rest[sep..];
                continue;
            }
            out.push_str("[REDACTED]");
            rest = &rest[val_end..];
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_append_and_read_back() {
        let store = RunStepStore::open_in_memory().unwrap();
        store
            .append("bruno", "telegram:1", KIND_TOOL_STEP, "Read", "src/lib.rs", 1)
            .unwrap();
        store
            .append("bruno", "telegram:1", KIND_TODO_UPDATE, "1/3", "📋 任務進度(1/3 完成)", 2)
            .unwrap();
        // A different session — must not leak into telegram:1 reads.
        store
            .append("bruno", "discord:9", KIND_TOOL_STEP, "Bash", "ls", 1)
            .unwrap();

        let rows = store.recent_for_session("telegram:1", 100).unwrap();
        assert_eq!(rows.len(), 2);
        // Oldest-first ordering for chronological merge.
        assert_eq!(rows[0].kind, KIND_TOOL_STEP);
        assert_eq!(rows[0].label, "Read");
        assert_eq!(rows[0].payload_preview, "src/lib.rs");
        assert_eq!(rows[0].seq, 1);
        assert_eq!(rows[1].kind, KIND_TODO_UPDATE);
        assert!(rows[1].payload_preview.contains("任務進度"));
        // RFC3339 timestamps parse.
        assert!(chrono::DateTime::parse_from_rfc3339(&rows[0].ts).is_ok());
    }

    #[test]
    fn preview_and_label_are_char_capped_cjk_safe() {
        let store = RunStepStore::open_in_memory().unwrap();
        let long_cjk = "測".repeat(2000);
        store
            .append("a", "s:1", KIND_TOOL_STEP, &long_cjk, &long_cjk, 1)
            .unwrap();
        let rows = store.recent_for_session("s:1", 10).unwrap();
        assert!(rows[0].label.chars().count() <= LABEL_CHAR_CAP + 1); // +1 for ellipsis
        assert!(rows[0].payload_preview.chars().count() <= PAYLOAD_PREVIEW_CHAR_CAP + 1);
    }

    #[test]
    fn prune_caps_per_agent_rows() {
        let store = RunStepStore::open_in_memory().unwrap();
        // Insert over the cap for one agent (direct inserts, then one prune).
        for i in 0..(PER_AGENT_ROW_CAP + 50) {
            store
                .append("busy", "s:1", KIND_TOOL_STEP, "T", "p", i as i64)
                .unwrap();
        }
        // A second over-cap agent that does NOT trigger the prune itself:
        // the global insert counter fires prune under "busy", but the cap
        // sweep must still trim "also-busy" (regression for the per-agent
        // skew where only the triggering agent was capped).
        for i in 0..(PER_AGENT_ROW_CAP + 30) {
            store
                .append("also-busy", "s:3", KIND_TOOL_STEP, "T", "p", i as i64)
                .unwrap();
        }
        store.append("quiet", "s:2", KIND_TOOL_STEP, "T", "p", 1).unwrap();
        store.prune("busy").unwrap();
        // Both over-cap agents capped by one prune; quiet agent untouched.
        assert_eq!(store.row_count(), PER_AGENT_ROW_CAP * 2 + 1);
        // Newest rows survive: the highest seq must still be present.
        let rows = store.recent_for_session("s:1", PER_AGENT_ROW_CAP).unwrap();
        assert_eq!(rows.last().unwrap().seq, (PER_AGENT_ROW_CAP + 50 - 1) as i64);
        let rows3 = store.recent_for_session("s:3", PER_AGENT_ROW_CAP).unwrap();
        assert_eq!(rows3.last().unwrap().seq, (PER_AGENT_ROW_CAP + 30 - 1) as i64);
    }

    #[test]
    fn prune_drops_rows_older_than_retention() {
        let store = RunStepStore::open_in_memory().unwrap();
        store.append("a", "s:1", KIND_TOOL_STEP, "New", "p", 1).unwrap();
        // Backdate one row past the retention window (test-only direct SQL).
        let old_ts = (chrono::Utc::now() - chrono::Duration::days(RETENTION_DAYS + 1))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        {
            let conn = store.lock();
            conn.execute(
                "INSERT INTO run_steps (agent_id, session_key, ts, kind, label, payload_preview, seq)
                 VALUES ('a', 's:1', ?1, 'tool_step', 'Old', '', 0)",
                params![old_ts],
            )
            .unwrap();
        }
        assert_eq!(store.row_count(), 2);
        store.prune("a").unwrap();
        let rows = store.recent_for_session("s:1", 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "New");
    }

    #[test]
    fn append_best_effort_swallows_errors() {
        let store = RunStepStore::open_in_memory().unwrap();
        // Break the store: drop the table so every insert fails.
        {
            let conn = store.lock();
            conn.execute_batch("DROP TABLE run_steps").unwrap();
        }
        // Must not panic and must not propagate the error.
        store.append_best_effort("a", "s:1", KIND_TOOL_STEP, "Read", "x", 1);
        // The strict variant does report it.
        assert!(store.append("a", "s:1", KIND_TOOL_STEP, "Read", "x", 2).is_err());
    }

    #[test]
    fn recent_tool_step_meta_filters_kind_and_agent() {
        let store = RunStepStore::open_in_memory().unwrap();
        store.append("a", "s:1", KIND_TOOL_STEP, "Read", "", 1).unwrap();
        store.append("a", "s:1", KIND_TODO_UPDATE, "1/2", "", 2).unwrap();
        store.append("b", "s:2", KIND_TOOL_STEP, "Bash", "", 1).unwrap();
        assert_eq!(store.recent_tool_step_meta("", 100).unwrap().len(), 2);
        let only_a = store.recent_tool_step_meta("a", 100).unwrap();
        assert_eq!(only_a.len(), 1);
        assert_eq!(only_a[0].0, "s:1");
    }

    #[test]
    fn mask_secretish_redacts_values_after_secret_keys() {
        assert_eq!(
            mask_secretish("curl -H \"Authorization: Bearer sk-abc123\" https://x"),
            "curl -H \"Authorization: Bearer [REDACTED]\" https://x"
        );
        assert_eq!(mask_secretish("api_key=sk-live-99 --verbose"), "api_key=[REDACTED] --verbose");
        assert_eq!(mask_secretish("export TOKEN=abc"), "export TOKEN=[REDACTED]");
        // Plain paths / CJK labels pass through untouched.
        assert_eq!(mask_secretish("src/lib.rs"), "src/lib.rs");
        assert_eq!(mask_secretish("正在讀取 檔案.md"), "正在讀取 檔案.md");
    }
}
