//! SQLite-based session management with 50k token compression.
//!
//! Uses a connection pool (multiple connections) to avoid the single-Mutex
//! bottleneck (BE-H1). Each operation acquires a connection from the pool,
//! runs the blocking SQLite call via `spawn_blocking`, and returns it.

use std::path::{Path, PathBuf};

use duduclaw_core::error::{DuDuClawError, Result};
use rusqlite::{Connection, OptionalExtension, params};
use tokio::sync::Mutex;
use tracing::info;

/// Threshold (in tokens) above which a session should be compressed.
const COMPRESSION_THRESHOLD: u32 = 50_000;

/// Escape LIKE metacharacters so a contact/session prefix containing `%`/`_`
/// matches literally under an `ESCAPE '\'` clause (GDPR prefix matching).
fn gdpr_like_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Number of connections in the pool.
const POOL_SIZE: usize = 4;

// Cancellation records are UI/audit metadata, not the approval fencing store.
const WEBCHAT_CANCEL_RETENTION_DAYS: u64 = 90;
const WEBCHAT_CANCEL_PER_SESSION_CAP: i64 = 1_000;

/// A conversation session for a specific agent.
pub struct Session {
    pub id: String,
    pub agent_id: String,
    pub summary: String,
    pub total_tokens: u32,
    pub last_active: String,
    pub model: String,
    /// G4 lineage counter: starts at 1, bumped every time the session is
    /// compressed (auto or `/compact`). Surfaced in `/status` as "#N".
    pub lineage: u32,
    /// RM-173: the row's `created_at` — its identity (the turn generation,
    /// see [`SessionManager::turn_generation`]), read in the same
    /// transaction that created/loaded the row.
    pub created_at: String,
}

/// A single message within a session.
pub struct SessionMessage {
    pub role: String,
    pub content: String,
    pub tokens: u32,
    pub timestamp: String,
}

/// Display-only WebChat row. Never use this type to build model context.
pub(crate) struct WebChatHistoryMessage {
    pub id: i64,
    pub message: SessionMessage,
    pub notice_kind: Option<String>,
    pub client_msg_id: Option<String>,
}

/// One row of `list_title_candidates` — a session the background auto-titler
/// may need to (re)title.
pub struct TitleCandidate {
    pub session_id: String,
    pub agent_id: String,
    pub turn_count: u32,
    pub titled_through_turn: u32,
}

/// A lightweight summary of a stored session, for the WebChat history picker
/// (WP3). `title` is the stored auto-title when present, otherwise the
/// session's first still-visible user message, CJK-safe truncated — enough to
/// recognise the conversation without loading it.
pub struct SessionSummary {
    pub id: String,
    pub agent_id: String,
    pub title: String,
    pub last_active: String,
    pub turn_count: u32,
    pub total_tokens: u32,
    pub lineage: u32,
    /// RM-173: why an archived row is archived (`user` / `retention`);
    /// `None` for a live row. See [`ARCHIVE_REASON_USER`].
    pub archive_reason: Option<String>,
}

/// RM-173: `sessions.archive_reason` of a conversation the user cleared
/// (`/new`, a channel reset, a deleted Discord thread). Such a row is the
/// user's own: team history never lists or releases it, a manager can never
/// restore it, and new activity never un-archives it.
pub const ARCHIVE_REASON_USER: &str = "user";
/// RM-173: `sessions.archive_reason` of a conversation archived by the
/// inactivity maintenance task. (Rows archived before the column existed were
/// all user clears — `delete_session` was the only writer of `archived_at` —
/// and are migrated to [`ARCHIVE_REASON_USER`], see
/// `SessionManager::migrate_legacy_user_clears`.)
pub const ARCHIVE_REASON_RETENTION: &str = "retention";

/// RM-173: error text of a write refused because the conversation the turn
/// started in was cleared (or purged and recreated) while the turn ran. See
/// [`SessionManager::append_delivery_message_in`].
pub(crate) const CLEARED_DURING_TURN: &str = "conversation cleared during the turn";

/// RM-173: `true` for the [`CLEARED_DURING_TURN`] refusal.
pub(crate) fn is_cleared_during_turn(e: &DuDuClawError) -> bool {
    matches!(e, DuDuClawError::Gateway(m) if m == CLEARED_DURING_TURN)
}

/// RM-173: one-time migration marker (table `session_migrations`).
const MIGRATION_LEGACY_USER_CLEARS: &str = "rm173_legacy_user_clears";

/// RM-173: the `SET` fragment every "new activity" write uses. It lifts an
/// archive caused by retention only; a user-cleared row (normally already
/// re-keyed away by [`SessionManager::delete_session`]) stays archived. Both
/// `CASE`s read the pre-update row, as SQLite evaluates all `SET`
/// expressions against the old values.
pub(crate) const UNARCHIVE_ON_ACTIVITY: &str =
    "archived_at = CASE WHEN archive_reason = 'user' THEN archived_at ELSE NULL END, \
     archive_reason = CASE WHEN archive_reason = 'user' THEN archive_reason ELSE NULL END";

/// RM-173: suffix marking a user-cleared conversation's moved id.
const CLEARED_ID_MARKER: &str = "#cleared:";

/// RM-173: SQL predicate over `sessions` for a row labelled a user clear but
/// still at its ORIGINAL id (labelled, never moved: a crash between the
/// legacy migration's two steps, or a move that failed). Such a row must
/// never take new activity — its content would re-enter the model context —
/// so it is moved away first ([`SessionManager::repair_stuck_clear`]) or the
/// write is refused. The `'#cleared:'` literal is [`CLEARED_ID_MARKER`].
pub(crate) const STUCK_USER_CLEAR: &str =
    "(archive_reason IS 'user' AND instr(id, '#cleared:') = 0)";

/// RM-173: what one retention run did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RetentionOutcome {
    pub archived: u64,
    pub purged: u64,
}

/// Max characters kept for a session title (first user message). CJK-safe.
const SESSION_TITLE_MAX_CHARS: usize = 80;

/// Hard cap on how many sessions a single `list_sessions` call returns, so a
/// caller cannot force an unbounded scan/response.
const SESSION_LIST_MAX_LIMIT: usize = 200;

/// SQLite-backed session manager with a simple connection pool (BE-H1).
pub struct SessionManager {
    pool: Vec<Mutex<Connection>>,
    #[allow(dead_code)] // Retained for future diagnostics/reconnect
    db_path: PathBuf,
}

impl SessionManager {
    /// Open (or create) a session database at `db_path` and initialize tables.
    pub fn new(db_path: &Path) -> Result<Self> {
        let first = Connection::open(db_path).map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        Self::init_tables(&first)?;

        let mut pool = vec![Mutex::new(first)];
        for _ in 1..POOL_SIZE {
            let conn =
                Connection::open(db_path).map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
            // Enable WAL mode for better concurrent read performance
            conn.execute_batch(
                "PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000; PRAGMA foreign_keys=ON;",
            )
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
            pool.push(Mutex::new(conn));
        }

        info!(
            ?db_path,
            pool_size = POOL_SIZE,
            "Session manager initialized with connection pool"
        );
        Ok(Self {
            pool,
            db_path: db_path.to_path_buf(),
        })
    }

    /// Acquire a connection from the pool.
    ///
    /// Tries non-blocking acquisition across all connections first. If all busy,
    /// sleeps 1ms before retrying to avoid CPU spin (R4-M2). Logs a warning
    /// after 1000 consecutive misses to surface pool exhaustion.
    ///
    /// `pub(crate)` so `session_portability.rs` (G4 handoff/undo/rollback)
    /// can extend `SessionManager` from its own module without widening
    /// the public API.
    pub(crate) async fn acquire(&self) -> tokio::sync::MutexGuard<'_, Connection> {
        let mut attempts = 0u32;
        loop {
            for conn in &self.pool {
                if let Ok(guard) = conn.try_lock() {
                    return guard;
                }
            }
            attempts += 1;
            if attempts >= 1000 {
                tracing::error!("Session pool exhausted after 1000 attempts — possible deadlock");
                attempts = 0;
            }
            // Short sleep instead of yield_now to avoid CPU spin-loop (R4-M2)
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    }

    fn init_tables(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
            PRAGMA busy_timeout=5000;
            PRAGMA foreign_keys=ON;

            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                summary TEXT DEFAULT '',
                total_tokens INTEGER DEFAULT 0,
                last_active TEXT NOT NULL,
                model TEXT DEFAULT 'auto',
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS session_messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL REFERENCES sessions(id),
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                tokens INTEGER DEFAULT 0,
                timestamp TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_messages_session
                ON session_messages(session_id);

            CREATE TABLE IF NOT EXISTS webchat_turn_cancellations (
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                turn_id TEXT NOT NULL,
                msg_id TEXT NOT NULL,
                cancelled_at TEXT NOT NULL,
                PRIMARY KEY(session_id, turn_id)
            );

            CREATE INDEX IF NOT EXISTS idx_webchat_cancellations_age
                ON webchat_turn_cancellations(cancelled_at);
            CREATE INDEX IF NOT EXISTS idx_webchat_cancellations_session_age
                ON webchat_turn_cancellations(session_id, cancelled_at DESC, turn_id DESC);

            CREATE TABLE IF NOT EXISTS session_ccr_references (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                user_message_id INTEGER NOT NULL,
                ccr_id TEXT NOT NULL,
                record_json TEXT NOT NULL,
                expires_at INTEGER NOT NULL,
                created_at TEXT NOT NULL,
                UNIQUE(session_id, user_message_id, ccr_id)
            );
            CREATE INDEX IF NOT EXISTS idx_session_ccr_references_session
                ON session_ccr_references(session_id, id);",
        )
        .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;

        // Phase 4: Add hidden column for hide/restore mechanism (Sculptor, arXiv 2508.04664)
        let _ = conn.execute(
            "ALTER TABLE session_messages ADD COLUMN hidden INTEGER NOT NULL DEFAULT 0",
            [],
        );

        // Display-only failures stay hidden from all context readers.
        let _ = conn.execute(
            "ALTER TABLE session_messages ADD COLUMN notice_kind TEXT NULL",
            [],
        );

        // Instruction Pinning: persist extracted task instructions across turns
        // (arXiv 2505.06120 — combats U-shaped attention degradation in multi-turn)
        let _ = conn.execute(
            "ALTER TABLE sessions ADD COLUMN pinned_instructions TEXT DEFAULT ''",
            [],
        );

        // #13 (2026-05-12): async summarization columns. The background
        // task in `session_summarizer_task::tick` writes Haiku-generated
        // bullet summaries of older turns; channel_reply reads them to
        // substitute for the verbatim history slice. All three columns
        // start NULL — pre-13 sessions keep their existing behaviour
        // because the consumer falls back to verbatim assembly when
        // `summary_of_prior` is NULL/empty.
        let _ = conn.execute("ALTER TABLE sessions ADD COLUMN summary_of_prior TEXT", []);
        let _ = conn.execute(
            "ALTER TABLE sessions ADD COLUMN summarized_through_turn INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE sessions ADD COLUMN last_summarized_at TEXT",
            [],
        );

        // WP5 T5.3: soft-delete. `archived_at` NULL = live; set = archived
        // (hidden from normal listings, still replayable / searchable). Real
        // deletion is `purge_session`.
        let _ = conn.execute("ALTER TABLE sessions ADD COLUMN archived_at TEXT", []);
        // RM-173: WHY a row is archived — `user` (a user cleared it: never
        // shown to managers, never un-archived by activity) or `retention`
        // (maintenance task). NULL while live (a NULL on an archived row
        // reads as `retention`).
        let _ = conn.execute("ALTER TABLE sessions ADD COLUMN archive_reason TEXT", []);
        // RM-173: when a user cleared this conversation. Sticky — set on
        // clear, NEVER reset by a restore — so a restored user clear stays out
        // of team history and stays restorable only by an Admin or its own
        // WebChat owner.
        let _ = conn.execute("ALTER TABLE sessions ADD COLUMN user_cleared_at TEXT", []);
        // (Legacy archived rows are migrated at the end, once every table a
        // clear touches exists — see `migrate_legacy_user_clears`.)

        // G4 session portability (2026-07-11):
        // - `undone_at` on messages: /undo //rollback tombstone (NULL = live).
        //   Distinct from `hidden` (Sculptor hide/restore) so undone turns are
        //   NOT restorable via restore_message and keep a clean audit trail.
        // - `lineage` on sessions: visible generation counter, bumped on every
        //   compression ("#2" after the first compress).
        // - `checkpoint_message_id` on sessions: turn-id watermark recorded
        //   just before each user turn is appended (i.e. before every agent
        //   run); /rollback tombstones everything after it.
        let _ = conn.execute("ALTER TABLE session_messages ADD COLUMN undone_at TEXT", []);
        // Pending WebChat replies are tombstoned from insertion. Only the
        // final delivery fence may publish them; ordinary restore cannot.
        let _ = conn.execute("ALTER TABLE session_messages ADD COLUMN delivery_pending INTEGER NOT NULL DEFAULT 0", []);
        let _ = conn.execute("ALTER TABLE session_messages ADD COLUMN client_msg_id TEXT", []);
        let _ = conn.execute(
            "ALTER TABLE sessions ADD COLUMN lineage INTEGER NOT NULL DEFAULT 1",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE sessions ADD COLUMN checkpoint_message_id INTEGER NOT NULL DEFAULT 0",
            [],
        );

        // Session auto-titles (2026-07-29): a short LLM-generated title that
        // follows the discussion, written by the background titler task
        // (`session_titler_task`). NULL/empty `title` ⇒ listings fall back to
        // the first user message. `titled_through_turn` records the turn count
        // when the title was last generated so the titler can refresh it after
        // the conversation moves on.
        let _ = conn.execute("ALTER TABLE sessions ADD COLUMN title TEXT", []);
        let _ = conn.execute(
            "ALTER TABLE sessions ADD COLUMN titled_through_turn INTEGER NOT NULL DEFAULT 0",
            [],
        );

        // E2 (2026-07-12): `list_sessions` orders by `last_active DESC` (optionally
        // scoped to one agent). Without this index SQLite full-scans `sessions` and
        // sorts every row before applying LIMIT — the WP3 SessionHistoryMenu popover
        // refetches on every open, so this is a hot read path. The composite
        // (agent_id, last_active) serves the agent-scoped popover directly.
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_sessions_last_active
                ON sessions(agent_id, last_active)",
            [],
        );

        // WP5 T5.1: reply-to → session mapping. When the bot sends a reply we
        // record which session that outbound message belongs to; when the user
        // replies to that message, we resume the same session instead of the
        // per-channel main session.
        let _ = conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS message_session_map (
                channel TEXT NOT NULL,
                message_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                created_at TEXT NOT NULL,
                PRIMARY KEY (channel, message_id)
            );",
        );
        // Ignore errors — columns already exist on subsequent runs

        // B5 session origin (2026-10): where each conversation comes from and,
        // for a signed-in WebChat account, who owns it. NULL on rows written
        // before this migration (see `session_origin::backfill`).
        crate::session_origin::store::migrate(conn);
        Self::migrate_legacy_user_clears(conn)?;
        Self::sweep_webchat_cancellations(conn, WEBCHAT_CANCEL_RETENTION_DAYS)?;

        Ok(())
    }

    /// RM-173 one-time migration (marker `rm173_legacy_user_clears` in
    /// `session_migrations`). Before RM-173 the ONLY writer of
    /// `sessions.archived_at` was `delete_session` — a user's `/new`, a
    /// channel reset, a deleted Discord thread — so every row archived before
    /// `archive_reason` existed is a user clear. Each one is:
    ///
    /// 1. labelled `archive_reason = 'user'` with `user_cleared_at` set (one
    ///    statement in its own committed transaction, before anything else —
    ///    the privacy-relevant part cannot be lost to a later failure);
    /// 2. moved to `{id}#cleared:{archived_at ms}` exactly like
    ///    [`Self::clear_for_user`], so the next message on the original id
    ///    starts a genuinely fresh conversation. Every move runs in its own
    ///    savepoint; a move that fails is rolled back alone and logged,
    ///    leaving that row labelled `user` at its original id (still hidden
    ///    from team history; every write path refuses or repairs such a row —
    ///    see [`STUCK_USER_CLEAR`]).
    ///
    /// Step 2's work list is read from the DB on EVERY start — any
    /// [`STUCK_USER_CLEAR`] row, whatever left it there (a crash between the
    /// steps, a failed move, an older build that wrote the marker anyway).
    /// The marker is written only once step 2 left nothing behind
    /// (`failed == 0`) and only short-circuits step 1, so rows archived later
    /// with a NULL reason (only direct SQL does that) are not relabelled.
    fn migrate_legacy_user_clears(conn: &Connection) -> Result<()> {
        let gw = |e: rusqlite::Error| DuDuClawError::Gateway(e.to_string());
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS session_migrations (
                name TEXT PRIMARY KEY,
                applied_at TEXT NOT NULL
            );",
        )
        .map_err(gw)?;
        let done: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM session_migrations WHERE name = ?1)",
                params![MIGRATION_LEGACY_USER_CLEARS],
                |row| row.get(0),
            )
            .map_err(gw)?;

        // Step 1 (until the marker exists): label (privacy first, committed
        // on its own).
        let labelled = if done {
            0
        } else {
            let tx = conn.unchecked_transaction().map_err(gw)?;
            let n = tx
                .execute(
                    "UPDATE sessions
                     SET archive_reason = 'user', user_cleared_at = COALESCE(user_cleared_at, archived_at)
                     WHERE archived_at IS NOT NULL AND archive_reason IS NULL",
                    [],
                )
                .map_err(gw)?;
            tx.commit().map_err(gw)?;
            n
        };

        // Step 2 (every start): every labelled-but-unmoved row, from the DB.
        let stuck: Vec<String> = {
            let mut stmt = conn
                .prepare(&format!("SELECT id FROM sessions WHERE {STUCK_USER_CLEAR}"))
                .map_err(gw)?;
            let mapped = stmt.query_map([], |r| r.get::<_, String>(0)).map_err(gw)?;
            mapped.collect::<rusqlite::Result<Vec<_>>>().map_err(gw)?
        };
        if done && stuck.is_empty() {
            return Ok(());
        }
        let mut tx = conn.unchecked_transaction().map_err(gw)?;
        let (mut moved, mut failed) = (0u64, 0u64);
        for id in &stuck {
            let sp = tx.savepoint().map_err(gw)?;
            match Self::repair_stuck_clear(&sp, id) {
                Ok(Some(new_id)) => {
                    sp.commit().map_err(gw)?;
                    moved += 1;
                    tracing::debug!(session_id = %id, cleared_to = %new_id, "user clear moved");
                }
                Ok(None) => drop(sp),
                Err(e) => {
                    // Dropping the savepoint rolls back this row's move only.
                    drop(sp);
                    failed += 1;
                    tracing::warn!(
                        session_id = %id, error = %e,
                        "user clear could not be moved; retried at the next start"
                    );
                }
            }
        }
        if !done && failed == 0 {
            tx.execute(
                "INSERT OR IGNORE INTO session_migrations (name, applied_at) VALUES (?1, ?2)",
                params![MIGRATION_LEGACY_USER_CLEARS, chrono::Utc::now().to_rfc3339()],
            )
            .map_err(gw)?;
        }
        tx.commit().map_err(gw)?;
        if labelled > 0 || !stuck.is_empty() {
            info!(
                labelled,
                moved, failed, "RM-173: legacy cleared conversations marked as user clears"
            );
        }
        Ok(())
    }

    /// RM-173 repair of a [`STUCK_USER_CLEAR`] row: move it off its original
    /// id like [`Self::clear_for_user`] (`Ok(None)`: `session_id` is not such
    /// a row). Keeps the row's `archived_at` — unless the row took activity
    /// after it, then the repair time, so retention never counts a
    /// conversation that was in use as long archived. Must run inside a
    /// transaction or savepoint (see [`Self::move_to_cleared`]).
    pub(crate) fn repair_stuck_clear(tx: &Connection, session_id: &str) -> Result<Option<String>> {
        let archived_at: Option<Option<String>> = tx
            .query_row(
                &format!(
                    "SELECT CASE WHEN archived_at IS NULL
                                   OR COALESCE(julianday(last_active) > julianday(archived_at), 0)
                                 THEN NULL ELSE archived_at END
                     FROM sessions WHERE id = ?1 AND {STUCK_USER_CLEAR}"
                ),
                params![session_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        let Some(archived_at) = archived_at else {
            return Ok(None);
        };
        let now = chrono::Utc::now();
        let cleared_at = archived_at.unwrap_or_else(|| now.to_rfc3339());
        let stamp = chrono::DateTime::parse_from_rfc3339(&cleared_at)
            .map(|t| t.timestamp_millis())
            .unwrap_or_else(|_| now.timestamp_millis());
        Self::move_to_cleared(tx, session_id, stamp, &cleared_at).map(Some)
    }

    /// The data move shared by [`Self::clear_for_user`] and
    /// [`Self::migrate_legacy_user_clears`]: re-key `session_id` (and
    /// everything keyed by it) to a fresh `{id}#cleared:{stamp}` id, labelled a
    /// user clear (`archived_at` = `cleared_at`; `user_cleared_at` =
    /// `cleared_at` unless already set). Must run inside a transaction (or savepoint) — the
    /// foreign keys are deferred to its commit. Returns the new id.
    fn move_to_cleared(
        tx: &Connection,
        session_id: &str,
        mut stamp: i64,
        cleared_at: &str,
    ) -> Result<String> {
        let gw = |e: rusqlite::Error| DuDuClawError::Gateway(e.to_string());
        // A fresh id: `{id}#cleared:{unix_ms}`, bumped past any collision.
        let new_id = loop {
            let candidate = format!("{session_id}{CLEARED_ID_MARKER}{stamp}");
            let taken: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sessions WHERE id = ?1)",
                    params![candidate],
                    |row| row.get(0),
                )
                .map_err(gw)?;
            if !taken {
                break candidate;
            }
            stamp += 1;
        };
        // `session_messages` / `webchat_turn_cancellations` reference
        // `sessions(id)` without ON UPDATE; checking the foreign keys at
        // commit (this pragma resets when the transaction ends) lets parent
        // and children move in any order within this one transaction.
        tx.execute_batch("PRAGMA defer_foreign_keys = ON;").map_err(gw)?;
        tx.execute(
            "UPDATE sessions SET id = ?2,
                    archived_at = ?3,
                    archive_reason = 'user',
                    user_cleared_at = COALESCE(user_cleared_at, ?3)
             WHERE id = ?1",
            params![session_id, new_id, cleared_at],
        )
        .map_err(gw)?;
        for sql in [
            "UPDATE session_messages SET session_id = ?2 WHERE session_id = ?1",
            "UPDATE webchat_turn_cancellations SET session_id = ?2 WHERE session_id = ?1",
        ] {
            tx.execute(sql, params![session_id, new_id]).map_err(gw)?;
        }
        let notices: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master
                 WHERE type = 'table' AND name = 'meeting_completion_notices')",
                [],
                |row| row.get(0),
            )
            .map_err(gw)?;
        if notices {
            tx.execute(
                "UPDATE meeting_completion_notices SET session_id = ?2 WHERE session_id = ?1",
                params![session_id, new_id],
            )
            .map_err(gw)?;
        }
        for sql in [
            "DELETE FROM session_ccr_references WHERE session_id = ?1",
            "DELETE FROM message_session_map WHERE session_id = ?1",
        ] {
            tx.execute(sql, params![session_id]).map_err(gw)?;
        }
        Ok(new_id)
    }

    /// Get an existing session or create a new one.
    ///
    /// Uses INSERT OR IGNORE + SELECT to avoid the TOCTOU race where two
    /// concurrent callers both see "not found" and both attempt INSERT,
    /// causing one to fail with a UNIQUE constraint error.
    ///
    /// A row this creates records origin `unknown` (B5 session origin); it
    /// never stamps a legacy row. Live turns use
    /// [`crate::session_origin::ensure_turn_session`] instead, which records
    /// where the conversation comes from; this stays for tests and the
    /// `migrate-from` import.
    pub async fn get_or_create(&self, session_id: &str, agent_id: &str) -> Result<Session> {
        self.create_inner(
            session_id,
            agent_id,
            &crate::session_origin::SessionOrigin::unknown(),
            false,
        )
        .await
    }

    /// Append a message to the session and update token count.
    pub async fn append_message(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
        tokens: u32,
    ) -> Result<()> {
        self.append_message_with_id(session_id, role, content, tokens)
            .await
            .map(|_| ())
    }

    /// Append a message and return its stable row ID for turn-bound artifacts.
    pub async fn append_message_with_id(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
        tokens: u32,
    ) -> Result<i64> {
        self.append_delivery_message(session_id, role, content, tokens, None, false).await
    }

    pub(crate) async fn append_delivery_message(
        &self, session_id: &str, role: &str, content: &str, tokens: u32,
        client_msg_id: Option<&str>, pending: bool,
    ) -> Result<i64> {
        self.append_delivery_message_in(session_id, role, content, tokens, client_msg_id, pending, None)
            .await
    }

    /// [`Self::append_delivery_message`] bound to one incarnation of the
    /// session row. With `generation = Some(g)` (the [`Self::turn_generation`]
    /// captured when the turn started) the row's identity is re-read in the
    /// same IMMEDIATE transaction as the insert; if the row is gone (a user
    /// cleared it — `/new` moves it away) or is a different one (cleared and
    /// recreated by a newer message, or purged and recreated), nothing is
    /// written and the call fails with [`CLEARED_DURING_TURN`]. A late reply
    /// therefore never lands in — or recreates — the fresh conversation.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn append_delivery_message_in(
        &self, session_id: &str, role: &str, content: &str, tokens: u32,
        client_msg_id: Option<&str>, pending: bool, generation: Option<&str>,
    ) -> Result<i64> {
        let mut guard = self.acquire().await;
        let now = chrono::Utc::now().to_rfc3339();
        // (Named `conn` so the statements below read as before.)
        let conn = guard
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| DuDuClawError::Gateway(format!("begin transaction: {e}")))?;

        // RM-173: a user clear that was labelled but never moved
        // ([`STUCK_USER_CLEAR`]) must not take this message — the cleared
        // content would come back into the model's context. Move it away
        // now (committed) and refuse the write: the conversation was
        // cleared, and the next turn (`create_inner`) starts a fresh one.
        let repaired = Self::repair_stuck_clear(&conn, session_id)?;
        if let Some(moved) = repaired {
            conn.commit()
                .map_err(|e| DuDuClawError::Gateway(format!("commit transaction: {e}")))?;
            info!(
                session_id, role, cleared_to = %moved,
                "message dropped: the conversation was a user clear left at its original id (moved now)"
            );
            return Err(DuDuClawError::Gateway(CLEARED_DURING_TURN.into()));
        }

        if let Some(expected) = generation {
            let current: Option<String> = conn
                .query_row(
                    "SELECT created_at FROM sessions WHERE id = ?1",
                    params![session_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
            if current.as_deref() != Some(expected) {
                // Dropping the transaction rolls back (nothing was written).
                info!(
                    session_id, role,
                    recreated = current.is_some(),
                    "message dropped: the conversation was cleared while the turn ran"
                );
                return Err(DuDuClawError::Gateway(CLEARED_DURING_TURN.into()));
            }
        }

        // G4 checkpoint: every user turn starts a (potentially file-writing)
        // agent run, so record the turn-id watermark just before it. We
        // cannot know in advance which runs will write files (the CLI
        // runtimes own tool use), so this is the conservative superset.
        // /rollback tombstones everything appended after this watermark.
        if role == "user" {
            conn.execute(
                "UPDATE sessions SET checkpoint_message_id = (
                     SELECT COALESCE(MAX(id), 0) FROM session_messages
                     WHERE session_id = ?1
                 ) WHERE id = ?1",
                params![session_id],
            )
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        }

        conn.execute(
            "INSERT INTO session_messages (session_id, role, content, tokens, timestamp,
                                          client_msg_id, delivery_pending, undone_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![session_id, role, content, tokens, now, client_msg_id, pending,
                pending.then_some(&now)],
        )
        .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        let message_id = conn.last_insert_rowid();

        // RM-173: a new message is new activity, so an archived (收起)
        // conversation comes back in the same statement that bumps
        // `last_active` — otherwise it stays hidden from every listing while
        // it is in use, and a purge setting could delete a live conversation.
        // Only a retention archive is lifted ([`UNARCHIVE_ON_ACTIVITY`]).
        conn.execute(
            &format!(
                "UPDATE sessions SET total_tokens = total_tokens + ?1, last_active = ?2,
                                     {UNARCHIVE_ON_ACTIVITY}
                 WHERE id = ?3"
            ),
            params![tokens, now, session_id],
        )
        .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;

        conn.commit()
            .map_err(|e| DuDuClawError::Gateway(format!("commit transaction: {e}")))?;
        Ok(message_id)
    }

    /// Store only structured provenance for tool originals already committed
    /// to CCR. The source user turn is checked again when references are read,
    /// so an undone turn cannot keep advertising its old handles.
    pub async fn append_ccr_saved_results(
        &self,
        session_id: &str,
        user_message_id: i64,
        records: &[duduclaw_llm::CcrSavedResult],
    ) -> Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        let conn = self.acquire().await;
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| DuDuClawError::Gateway(format!("begin transaction: {e}")))?;
        let active: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM session_messages WHERE id=?1 AND session_id=?2
             AND role='user' AND hidden=0 AND undone_at IS NULL)",
                params![user_message_id, session_id],
                |row| row.get(0),
            )
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        if !active
            || records
                .iter()
                .any(|record| record.scope.session_id != session_id)
        {
            return Err(DuDuClawError::Gateway(
                "CCR reference does not match an active user turn".into(),
            ));
        }
        let now = chrono::Utc::now().to_rfc3339();
        for record in records {
            let json =
                serde_json::to_string(record).map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
            tx.execute(
                "INSERT OR IGNORE INTO session_ccr_references
                 (session_id, user_message_id, ccr_id, record_json, expires_at, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    session_id,
                    user_message_id,
                    record.id,
                    json,
                    record.expires_at,
                    now
                ],
            )
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        }
        tx.execute(
            "DELETE FROM session_ccr_references WHERE session_id=?1 AND (expires_at<=?2 OR id NOT IN (
                SELECT id FROM session_ccr_references WHERE session_id=?1
                ORDER BY id DESC LIMIT 100
            ))",
            params![session_id, chrono::Utc::now().timestamp()],
        ).map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        tx.commit()
            .map_err(|e| DuDuClawError::Gateway(format!("commit transaction: {e}")))?;
        Ok(())
    }

    /// Recent references whose source user turn is still visible. CCR must
    /// revalidate each record before any handle is shown to a model.
    pub async fn get_ccr_saved_results(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<duduclaw_llm::CcrSavedResult>> {
        let conn = self.acquire().await;
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "DELETE FROM session_ccr_references WHERE session_id=?1 AND expires_at<=?2",
            params![session_id, now],
        )
        .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        let mut stmt = conn
            .prepare(
                "SELECT r.record_json FROM session_ccr_references r
             JOIN session_messages m ON m.id=r.user_message_id AND m.session_id=r.session_id
             WHERE r.session_id=?1 AND r.expires_at>?3
             AND m.role='user' AND m.hidden=0 AND m.undone_at IS NULL
             ORDER BY r.id DESC LIMIT ?2",
            )
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        let rows = stmt
            .query_map(params![session_id, limit.clamp(1, 20) as i64, now], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        let mut results = Vec::new();
        for row in rows {
            let json = row.map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
            if let Ok(record) = serde_json::from_str::<duduclaw_llm::CcrSavedResult>(&json) {
                results.push(record);
            }
        }
        Ok(results)
    }

    /// Control records never enter conversation history or prompt content. The
    /// owner check and insert share one write transaction; absent sessions stay absent.
    pub(crate) async fn record_webchat_cancel(
        &self, session_id: &str, agent: &str, turn_id: &str, msg_id: &str,
    ) -> Result<()> {
        let mut conn = self.acquire().await;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        let owner: Option<String> = tx.query_row(
            "SELECT agent_id FROM sessions WHERE id=?1", [session_id], |r| r.get(0),
        ).optional().map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        if let Some(owner) = owner {
            if owner != agent {
                return Err(DuDuClawError::Gateway("cancel session owner changed".into()));
            }
            tx.execute("INSERT OR IGNORE INTO webchat_turn_cancellations
                (session_id,turn_id,msg_id,cancelled_at) VALUES (?1,?2,?3,?4)",
                params![session_id, turn_id, msg_id, chrono::Utc::now().to_rfc3339()])
                .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        }
        // Enforce the per-session bound in the same transaction as insertion.
        // The periodic sweep applies any shorter configured session retention.
        tx.execute(
            "DELETE FROM webchat_turn_cancellations WHERE session_id=?1 AND
             (julianday(cancelled_at) < julianday('now') - ?2 OR turn_id NOT IN (
                 SELECT turn_id FROM webchat_turn_cancellations WHERE session_id=?1
                 ORDER BY cancelled_at DESC, turn_id DESC LIMIT ?3
             ))",
            params![session_id, WEBCHAT_CANCEL_RETENTION_DAYS, WEBCHAT_CANCEL_PER_SESSION_CAP],
        ).map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        tx.commit().map_err(|e| DuDuClawError::Gateway(e.to_string()))
    }

    fn sweep_webchat_cancellations(conn: &Connection, session_retention_days: u64) -> Result<()> {
        conn.execute(
            "DELETE FROM webchat_turn_cancellations
             WHERE julianday(cancelled_at) < julianday('now') - ?1
                OR NOT EXISTS (SELECT 1 FROM sessions
                               WHERE sessions.id=webchat_turn_cancellations.session_id)",
            params![session_retention_days.min(WEBCHAT_CANCEL_RETENTION_DAYS)],
        ).map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        // Also trim existing databases that predate the insertion-time cap.
        conn.execute(
            "DELETE FROM webchat_turn_cancellations WHERE rowid IN (
                 SELECT cancellation_rowid FROM (
                     SELECT rowid AS cancellation_rowid,
                            ROW_NUMBER() OVER (PARTITION BY session_id
                                ORDER BY cancelled_at DESC, turn_id DESC) AS position
                     FROM webchat_turn_cancellations
                 ) WHERE position > ?1
             )",
            [WEBCHAT_CANCEL_PER_SESSION_CAP],
        ).map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        Ok(())
    }

    /// Only compress() (and handoff of its stored summary) writes this column.
    /// System messages can also be provenance/control records, never summaries.
    pub(crate) async fn compression_summary(&self, session_id: &str) -> Result<String> {
        self.acquire().await.query_row(
            "SELECT COALESCE(summary, '') FROM sessions WHERE id=?1", [session_id], |r| r.get(0),
        ).optional().map(|v| v.unwrap_or_default())
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))
    }

    /// Persist a display-only failure without affecting context or token totals.
    pub(crate) async fn record_webchat_failure(&self, session_id: &str, text: &str) -> Result<()> {
        let masked = duduclaw_security::credential_detect::mask(text);
        let text = duduclaw_core::truncate_chars(&masked, 2000);
        let conn = self.acquire().await;
        conn.execute(
            "INSERT INTO session_messages
             (session_id, role, content, tokens, timestamp, hidden, notice_kind)
             VALUES (?1, 'assistant', ?2, 0, ?3, 1, 'turn_failure')",
            params![session_id, text, chrono::Utc::now().to_rfc3339()],
        ).map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        Ok(())
    }

    /// Only the WebChat history display may read hidden failure notices.
    pub(crate) async fn webchat_history(&self, session_id: &str) -> Result<Vec<WebChatHistoryMessage>> {
        let conn = self.acquire().await;
        let mut stmt = conn.prepare(
            "SELECT id, role, content, tokens, timestamp, notice_kind, client_msg_id FROM session_messages
             WHERE session_id = ?1 AND undone_at IS NULL
               AND (hidden = 0 OR notice_kind = 'turn_failure') ORDER BY id ASC",
        ).map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        let rows = stmt.query_map([session_id], |row| Ok(WebChatHistoryMessage {
            id: row.get(0)?,
            message: SessionMessage {
                role: row.get(1)?, content: row.get(2)?, tokens: row.get(3)?, timestamp: row.get(4)?,
            },
            notice_kind: row.get(5)?,
            client_msg_id: row.get(6)?,
        })).map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))
    }

    /// Retrieve all messages for a session, ordered by timestamp.
    pub async fn get_messages(&self, session_id: &str) -> Result<Vec<SessionMessage>> {
        let conn = self.acquire().await;

        let mut stmt = conn
            .prepare(
                "SELECT role, content, tokens, timestamp
                 FROM session_messages
                 WHERE session_id = ?1 AND hidden = 0 AND undone_at IS NULL
                 ORDER BY id ASC",
            )
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;

        let rows = stmt
            .query_map(params![session_id], |row| {
                Ok(SessionMessage {
                    role: row.get(0)?,
                    content: row.get(1)?,
                    tokens: row.get(2)?,
                    timestamp: row.get(3)?,
                })
            })
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;

        let mut messages = Vec::new();
        for row in rows {
            let msg = row.map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
            messages.push(msg);
        }

        Ok(messages)
    }

    /// Row ids of [`Self::get_messages`]' messages, in the same order (same
    /// filter). Lets display paths tie a message to records keyed by its id
    /// (knowledge-base citations) without changing `SessionMessage`.
    pub async fn get_message_ids(&self, session_id: &str) -> Result<Vec<i64>> {
        let conn = self.acquire().await;
        let mut stmt = conn
            .prepare(
                "SELECT id FROM session_messages
                 WHERE session_id = ?1 AND hidden = 0 AND undone_at IS NULL
                 ORDER BY id ASC",
            )
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        let rows = stmt
            .query_map(params![session_id], |row| row.get::<_, i64>(0))
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))
    }

    /// Check whether the session's token count exceeds the compression threshold.
    pub async fn should_compress(&self, session_id: &str) -> bool {
        let conn = self.acquire().await;

        conn.query_row(
            "SELECT total_tokens FROM sessions WHERE id = ?1",
            params![session_id],
            |row| row.get::<_, u32>(0),
        )
        .map(|tokens| tokens > COMPRESSION_THRESHOLD)
        .unwrap_or(false)
    }

    /// Replace all messages with a summary and reset the token count.
    ///
    /// Uses a SQLite transaction to ensure atomicity (BE-M7).
    ///
    /// Note: the DELETE below removes EVERY row for the session, including
    /// tombstoned (`undone_at`) and hidden ones — soft-deleted rows are
    /// therefore retained for audit only *until the next compression*, not
    /// indefinitely (see `session_portability.rs` module docs).
    pub async fn compress(&self, session_id: &str, summary: &str) -> Result<()> {
        self.compress_checked(session_id, summary, None).await
    }

    /// Background summaries must not discard rows that arrived while the
    /// utility model was running, including a newly published staged reply.
    pub(crate) async fn compress_if_unchanged(
        &self, session_id: &str, summary: &str, expected_ids: &[i64],
    ) -> Result<()> {
        self.compress_checked(session_id, summary, Some(expected_ids)).await
    }

    async fn compress_checked(
        &self, session_id: &str, summary: &str, expected_ids: Option<&[i64]>,
    ) -> Result<()> {
        let masked_summary = duduclaw_security::credential_detect::mask(summary);
        let summary = masked_summary.as_str();
        let conn = self.acquire().await;
        let now = chrono::Utc::now().to_rfc3339();

        let tx = conn
            .unchecked_transaction()
            .map_err(|e| DuDuClawError::Gateway(format!("begin transaction: {e}")))?;

        let pending: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM session_messages WHERE session_id=?1 AND delivery_pending=1)",
            [session_id], |row| row.get(0),
        ).map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        if pending {
            return Err(DuDuClawError::Gateway("session delivery still pending".into()));
        }
        if let Some(expected) = expected_ids {
            let mut stmt = tx.prepare(
                "SELECT id FROM session_messages WHERE session_id=?1
                 AND hidden=0 AND undone_at IS NULL ORDER BY id ASC",
            ).map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
            let current = stmt.query_map([session_id], |row| row.get::<_, i64>(0))
                .map_err(|e| DuDuClawError::Gateway(e.to_string()))?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
            if current.as_slice() != expected {
                return Err(DuDuClawError::Gateway("session changed during compression".into()));
            }
        }

        tx.execute(
            "DELETE FROM session_ccr_references WHERE session_id = ?1",
            params![session_id],
        )
        .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        tx.execute(
            "DELETE FROM session_messages WHERE session_id = ?1",
            params![session_id],
        )
        .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;

        tx.execute(
            "INSERT INTO session_messages (session_id, role, content, tokens, timestamp)
             VALUES (?1, 'system', ?2, 0, ?3)",
            params![session_id, summary, now],
        )
        .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;

        // G4 lineage: each compression starts a new visible generation
        // ("#2", "#3", …) so users can tell a compressed session apart.
        tx.execute(
            "UPDATE sessions SET summary = ?1, total_tokens = 0, last_active = ?2,
                    lineage = COALESCE(lineage, 1) + 1
             WHERE id = ?3",
            params![summary, now, session_id],
        )
        .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;

        tx.commit()
            .map_err(|e| DuDuClawError::Gateway(format!("commit transaction: {e}")))?;

        info!(session_id, "Session compressed");
        Ok(())
    }

    /// Force-compress a session immediately, truncating message history and resetting
    /// the token counter. Returns the number of tokens freed.
    ///
    /// Uses a 10-second timeout to prevent long-running compression from starving the
    /// connection pool (SEC2-M25).
    pub async fn force_compress(&self, session_id: &str) -> Result<u32> {
        let compress_result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let tokens: u32 = {
                let conn = self.acquire().await;
                conn.query_row(
                    "SELECT total_tokens FROM sessions WHERE id = ?1",
                    params![session_id],
                    |row| row.get(0),
                )
                .unwrap_or(0)
            };

            let summary = "[Session force-compressed — history cleared to free token budget]";
            self.compress(session_id, summary).await?;
            Ok::<u32, DuDuClawError>(tokens)
        })
        .await;

        match compress_result {
            Ok(Ok(tokens)) => Ok(tokens),
            Ok(Err(e)) => {
                tracing::warn!(session_id, error = %e, "Session force_compress failed");
                Err(e)
            }
            Err(_) => {
                tracing::warn!(session_id, "Session force_compress timed out after 10s");
                Err(DuDuClawError::Gateway("compression timed out".to_string()))
            }
        }
    }

    /// A user cleared this conversation (`/new`, a channel reset, a deleted
    /// Discord thread). Used by every reset command.
    ///
    /// WP5 T5.3 made this a soft delete (archive). RM-173: channel session ids
    /// are reused (`line:{sender}`, `telegram:{chat}`, …), so archiving in
    /// place let the next message un-archive the same row and the whole old
    /// conversation came back — in listings, search and the agent's context.
    /// A clear therefore MOVES the conversation, in one transaction, to a new
    /// id `{id}#cleared:{unix_ms}` with `archive_reason = 'user'` and the
    /// sticky `user_cleared_at` mark (kept even if the row is later restored):
    ///
    /// | data keyed by the session id (sessions.db) | on clear |
    /// |---|---|
    /// | `sessions` row (summary, pinned instructions, title, lineage, origin columns, created_at …) | moved |
    /// | `session_messages` | moved |
    /// | `webchat_turn_cancellations` | moved |
    /// | `meeting_completion_notices` (when the table exists) | moved |
    /// | `session_ccr_references` | deleted (expiring tool-output pointers whose stored scope names the OLD id; moving them would hand a restored conversation handles scoped to the fresh one) |
    /// | `message_session_map` | deleted (a reply to an old bot message must not resume the cleared context; same as `purge_session`) |
    ///
    /// The original id then has no row: the next message creates a genuinely
    /// fresh session with normal origin stamping. Messages are kept on the
    /// moved row (still restorable by its owner / an Admin, never by a
    /// manager). Data in other databases (run steps, CCR store, approvals,
    /// AG-UI run state) stays on the old id; none of it is conversation
    /// content that the model context or history listings read by session id.
    ///
    /// Idempotent — no row, or a row already cleared by a user (archived as
    /// `user` under a `#cleared:` id), is a no-op. A row labelled `user` but
    /// still at its original id ([`STUCK_USER_CLEAR`]) is NOT already
    /// cleared: it is moved now (repair).
    ///
    /// A turn still running in the cleared conversation cannot write its
    /// reply into the fresh one: its writes are bound to the row it started
    /// in ([`Self::turn_generation`] / `append_delivery_message_in`).
    /// WebChat schedules aimed at the cleared id are paused.
    pub async fn delete_session(&self, session_id: &str) -> Result<()> {
        self.clear_for_user(session_id).await.map(|_| ())
    }

    /// [`Self::delete_session`], returning the id the conversation moved to
    /// (`None`: nothing to clear).
    pub async fn clear_for_user(&self, session_id: &str) -> Result<Option<String>> {
        let conn = self.acquire().await;
        // Scope the connection guard + transaction so neither is held
        // across the `.await` below (keeps the future `Send`).
        let new_id = {
            let now = chrono::Utc::now();
            let now_str = now.to_rfc3339();
            let tx = conn
                .unchecked_transaction()
                .map_err(|e| DuDuClawError::Gateway(format!("begin transaction: {e}")))?;
            let already_cleared: Option<bool> = tx
                .query_row(
                    "SELECT archived_at IS NOT NULL AND archive_reason IS 'user'
                            AND instr(id, ?2) > 0
                     FROM sessions WHERE id = ?1",
                    params![session_id, CLEARED_ID_MARKER],
                    |row| row.get::<_, Option<bool>>(0).map(|v| v.unwrap_or(false)),
                )
                .optional()
                .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
            if already_cleared != Some(false) {
                tx.commit()
                    .map_err(|e| DuDuClawError::Gateway(format!("commit transaction: {e}")))?;
                None
            } else {
                let new_id =
                    Self::move_to_cleared(&tx, session_id, now.timestamp_millis(), &now_str)?;
                tx.commit()
                    .map_err(|e| DuDuClawError::Gateway(format!("commit transaction: {e}")))?;
                info!(session_id, cleared_to = %new_id, "Session cleared by user (moved + archived)");
                Some(new_id)
            }
        };
        drop(conn);
        let Some(new_id) = new_id else {
            return Ok(None);
        };
        // RM-173 (e): schedules / reminders created in this WebChat
        // conversation report into its id, which now has no row — every run
        // would be refused. Pause (never delete) the schedules aimed at it.
        if session_id.starts_with("webchat:") {
            if let Some(home) = self.db_path.parent() {
                crate::scheduled_webchat::pause_for_session(
                    home,
                    session_id,
                    "WebChat 對話已被使用者清除（/new），排程已暫停 (the WebChat conversation was cleared; schedule paused)",
                )
                .await;
            }
        }
        Ok(Some(new_id))
    }

    /// Un-archive a session so it reappears in normal listings.
    pub async fn unarchive_session(&self, session_id: &str) -> Result<()> {
        self.restore_archived(session_id).await.map(|_| ())
    }

    /// RM-173: restore an archived (收起) conversation, whatever its reason.
    /// Returns the `archived_at` it had, or `None` when the row does not exist
    /// or was not archived (idempotent; nothing is written then).
    /// Authorization — including who may restore a user-cleared row (see
    /// [`Self::archive_state`] / [`Self::user_cleared_at`]) — is the caller's
    /// job. `user_cleared_at` is deliberately NOT reset: a restored user clear
    /// stays the user's own (out of team history; restorable again only by an
    /// Admin or its own WebChat owner).
    pub async fn restore_archived(&self, session_id: &str) -> Result<Option<String>> {
        let conn = self.acquire().await;
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| DuDuClawError::Gateway(format!("begin transaction: {e}")))?;
        let archived: Option<String> = tx
            .query_row(
                "SELECT archived_at FROM sessions WHERE id = ?1 AND archived_at IS NOT NULL",
                params![session_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        if archived.is_some() {
            tx.execute(
                "UPDATE sessions SET archived_at = NULL, archive_reason = NULL WHERE id = ?1",
                params![session_id],
            )
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        }
        tx.commit()
            .map_err(|e| DuDuClawError::Gateway(format!("commit transaction: {e}")))?;
        Ok(archived)
    }

    /// RM-173: the sticky `user_cleared_at` mark (`Ok(None)`: never cleared
    /// by a user, or no row). Survives a restore.
    pub async fn user_cleared_at(&self, session_id: &str) -> Result<Option<String>> {
        let conn = self.acquire().await;
        conn.query_row(
            "SELECT user_cleared_at FROM sessions WHERE id = ?1",
            params![session_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map(Option::flatten)
        .map_err(|e| DuDuClawError::Gateway(e.to_string()))
    }

    /// RM-173 race guard: the identity of the row `session_id` names right
    /// now — its `created_at`, which a clear moves away with the row and a
    /// recreated row never repeats (the same stamp `scheduled_webchat` binds
    /// schedules to). Captured at turn start; `Ok(None)`: no row.
    pub async fn turn_generation(&self, session_id: &str) -> Result<Option<String>> {
        let conn = self.acquire().await;
        conn.query_row(
            "SELECT created_at FROM sessions WHERE id = ?1",
            params![session_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| DuDuClawError::Gateway(e.to_string()))
    }

    /// RM-173: `archived_at` of an existing session (`Ok(None)`: live or no row).
    pub async fn archived_at(&self, session_id: &str) -> Result<Option<String>> {
        let conn = self.acquire().await;
        conn.query_row(
            "SELECT archived_at FROM sessions WHERE id = ?1",
            params![session_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map(Option::flatten)
        .map_err(|e| DuDuClawError::Gateway(e.to_string()))
    }

    /// RM-173: `(archived_at, archive_reason)` of an archived session
    /// (`Ok(None)`: live or no row). A NULL reason reads as `retention`.
    pub async fn archive_state(&self, session_id: &str) -> Result<Option<(String, String)>> {
        let conn = self.acquire().await;
        conn.query_row(
            "SELECT archived_at, COALESCE(archive_reason, 'retention')
             FROM sessions WHERE id = ?1 AND archived_at IS NOT NULL",
            params![session_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|e| DuDuClawError::Gateway(e.to_string()))
    }

    /// Hard delete — irreversibly removes the session and its messages. This is
    /// the pre-WP5 `delete_session` behaviour, now behind an explicit name.
    pub async fn purge_session(&self, session_id: &str) -> Result<()> {
        let conn = self.acquire().await;
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| DuDuClawError::Gateway(format!("begin transaction: {e}")))?;

        tx.execute(
            "DELETE FROM webchat_turn_cancellations WHERE session_id = ?1",
            params![session_id],
        )
        .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        tx.execute(
            "DELETE FROM session_ccr_references WHERE session_id = ?1",
            params![session_id],
        )
        .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        tx.execute(
            "DELETE FROM session_messages WHERE session_id = ?1",
            params![session_id],
        )
        .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;

        tx.execute(
            "DELETE FROM message_session_map WHERE session_id = ?1",
            params![session_id],
        )
        .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;

        tx.execute("DELETE FROM sessions WHERE id = ?1", params![session_id])
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;

        tx.commit()
            .map_err(|e| DuDuClawError::Gateway(format!("commit transaction: {e}")))?;

        info!(session_id, "Session purged (hard delete)");
        drop(conn);
        // RM-124: schedules / reminders that report into this WebChat
        // conversation die with it (delivery is independently refused for a
        // recreated conversation with the same id).
        if session_id.starts_with("webchat:") {
            if let Some(home) = self.db_path.parent() {
                crate::scheduled_webchat::cancel_for_session(home, session_id).await;
            }
        }
        Ok(())
    }

    // ── WP5 T5.1: reply-to → session mapping ───────────────────────

    /// Record that an outbound bot message belongs to `session_id`, so a later
    /// user reply to that message can resume the same session.
    pub async fn record_message_session(
        &self,
        channel: &str,
        message_id: &str,
        session_id: &str,
    ) -> Result<()> {
        let conn = self.acquire().await;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT OR REPLACE INTO message_session_map (channel, message_id, session_id, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![channel, message_id, session_id, now],
        )
        .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        Ok(())
    }

    /// Resolve the session a user is replying to, given the replied-to message
    /// id on a channel. `None` ⇒ not a tracked reply ⇒ caller uses the main
    /// per-channel session.
    pub async fn session_for_reply(
        &self,
        channel: &str,
        message_id: &str,
    ) -> Result<Option<String>> {
        let conn = self.acquire().await;
        let r: std::result::Result<String, _> = conn.query_row(
            "SELECT session_id FROM message_session_map WHERE channel = ?1 AND message_id = ?2",
            params![channel, message_id],
            |row| row.get(0),
        );
        match r {
            Ok(s) => Ok(Some(s)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(DuDuClawError::Gateway(format!("session_for_reply: {e}"))),
        }
    }

    /// GDPR: session ids belonging to a contact. A "contact" here is the
    /// `"<channel>:<chat_id>"` session-id prefix (sessions are keyed by that
    /// string; threads add a `:<tid>` suffix). Matches the exact id, any
    /// `<contact>:*` and any `<contact>#*` (RM-173 user-cleared
    /// `#cleared:` rows, `#agent:` buckets; an exact-prefix compare, no LIKE).
    /// LIKE wildcards in the contact are escaped so a literal `%`/`_` cannot
    /// widen the match.
    pub async fn sessions_for_contact(&self, contact: &str) -> Result<Vec<String>> {
        let conn = self.acquire().await;
        let like = format!("{}:%", gdpr_like_escape(contact));
        let mut stmt = conn
            .prepare("SELECT id FROM sessions WHERE id = ?1 OR substr(id, 1, length(?1) + 1) = ?1 || '#' OR id LIKE ?2 ESCAPE '\\'")
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        let rows = stmt
            .query_map(params![contact, like], |r| r.get::<_, String>(0))
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| DuDuClawError::Gateway(e.to_string()))?);
        }
        Ok(out)
    }

    /// GDPR erase: hard-delete every session (and its messages) belonging to a
    /// contact prefix, in one transaction. Returns `(sessions_deleted,
    /// messages_deleted)`. Messages are deleted first via a subquery that still
    /// sees the `sessions` rows.
    pub async fn erase_sessions_for_contact(&self, contact: &str) -> Result<(u64, u64)> {
        let conn = self.acquire().await;
        let like = format!("{}:%", gdpr_like_escape(contact));
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| DuDuClawError::Gateway(format!("begin transaction: {e}")))?;
        // RM-124: exact WebChat ids, captured inside the tx before the delete.
        let webchat_ids = Self::webchat_ids(
            &tx,
            "SELECT id FROM sessions WHERE id = ?1 OR substr(id, 1, length(?1) + 1) = ?1 || '#' OR id LIKE ?2 ESCAPE '\\'",
            params![contact, like],
        )?;
        tx.execute(
            "DELETE FROM webchat_turn_cancellations WHERE session_id IN (
                 SELECT id FROM sessions WHERE id = ?1 OR substr(id, 1, length(?1) + 1) = ?1 || '#' OR id LIKE ?2 ESCAPE '\\'
             )",
            params![contact, like],
        )
        .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        tx.execute(
            "DELETE FROM session_ccr_references WHERE session_id IN (
                 SELECT id FROM sessions WHERE id = ?1 OR substr(id, 1, length(?1) + 1) = ?1 || '#' OR id LIKE ?2 ESCAPE '\\'
             )",
            params![contact, like],
        )
        .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        let messages = tx
            .execute(
                "DELETE FROM session_messages WHERE session_id IN (
                     SELECT id FROM sessions WHERE id = ?1 OR substr(id, 1, length(?1) + 1) = ?1 || '#' OR id LIKE ?2 ESCAPE '\\'
                 )",
                params![contact, like],
            )
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        let sessions = tx
            .execute(
                "DELETE FROM sessions WHERE id = ?1 OR substr(id, 1, length(?1) + 1) = ?1 || '#' OR id LIKE ?2 ESCAPE '\\'",
                params![contact, like],
            )
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        tx.commit()
            .map_err(|e| DuDuClawError::Gateway(format!("commit transaction: {e}")))?;
        if sessions > 0 {
            info!(contact, sessions, messages, "GDPR: erased contact sessions");
        }
        drop(conn);
        self.cancel_webchat_schedules(&webchat_ids).await;
        Ok((sessions as u64, messages as u64))
    }

    /// WP5 T5.3: inactive sessions are ARCHIVED (not hard-deleted). Archives
    /// sessions whose `last_active` is older than `max_age_hours` and NEVER
    /// purges (RM-173: purging is an explicit admin setting, see
    /// [`Self::apply_retention`]). Returns the number newly archived.
    pub async fn cleanup_inactive(&self, max_age_hours: u64) -> Result<u64> {
        self.cleanup_with_policy(Some(chrono::Duration::hours(max_age_hours as i64)), None)
            .await
            .map(|o| o.archived)
    }

    /// Archive after `max_age_hours`, then purge sessions archived longer than
    /// `purge_after_days` (an explicit purge; tests and tools only — the
    /// gateway maintenance task uses [`Self::apply_retention`]).
    pub async fn cleanup_inactive_with_retention(
        &self,
        max_age_hours: u64,
        purge_after_days: u64,
    ) -> Result<u64> {
        self.cleanup_with_policy(
            Some(chrono::Duration::hours(max_age_hours as i64)),
            Some(purge_after_days),
        )
        .await
        .map(|o| o.archived)
    }

    /// RM-173: the admin `[sessions]` settings. `0` disables a step:
    /// `archive_after_days = 0` never archives, `purge_after_days = 0` (the
    /// default) never deletes anything.
    pub async fn apply_retention(
        &self,
        settings: crate::session_retention::Retention,
    ) -> Result<RetentionOutcome> {
        self.cleanup_with_policy(
            settings
                .archive_after()
                .map(|days| chrono::Duration::days(days as i64)),
            settings.purge_after(),
        )
        .await
    }

    async fn cleanup_with_policy(
        &self,
        archive_after: Option<chrono::Duration>,
        purge_after_days: Option<u64>,
    ) -> Result<RetentionOutcome> {
        let conn = self.acquire().await;
        let now = chrono::Utc::now();
        let now_str = now.to_rfc3339();

        conn.execute(
            "DELETE FROM session_ccr_references WHERE expires_at<=?1",
            params![now.timestamp()],
        )
        .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;

        Self::sweep_webchat_cancellations(
            &conn,
            purge_after_days.unwrap_or(WEBCHAT_CANCEL_RETENTION_DAYS),
        )?;

        // Step 1: archive inactive live sessions (soft).
        let archived = match archive_after {
            Some(age) => conn
                .execute(
                    "UPDATE sessions SET archived_at = ?2, archive_reason = 'retention'
                     WHERE last_active < ?1 AND archived_at IS NULL",
                    params![(now - age).to_rfc3339(), now_str],
                )
                .map_err(|e| DuDuClawError::Gateway(e.to_string()))?,
            None => 0,
        };

        // Step 2: purge sessions archived beyond the retention window — only
        // when a purge period is explicitly configured.
        let Some(purge_after_days) = purge_after_days else {
            if archived > 0 {
                info!(archived, "Session cleanup: archived (purge disabled)");
            }
            return Ok(RetentionOutcome { archived: archived as u64, purged: 0 });
        };
        let purge_cutoff = (now - chrono::Duration::days(purge_after_days as i64)).to_rfc3339();
        let max_age_hours = archive_after.map(|a| a.num_hours());
        // RM-173: never purge a row that took activity after it was archived
        // (`last_active` newer than `archived_at`) — that is a conversation
        // in use whose archive mark is stale (e.g. a user clear left at its
        // original id), not an old archive. An unparseable time keeps the
        // plain `archived_at` rule.
        let purgeable = "archived_at IS NOT NULL AND archived_at < ?1 \
             AND NOT COALESCE(julianday(last_active) > julianday(archived_at), 0)";
        // One transaction, so every statement below sees the same rows.
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| DuDuClawError::Gateway(format!("begin transaction: {e}")))?;
        // RM-124: remember the exact WebChat ids before they disappear.
        let purge_webchat_ids = Self::webchat_ids(
            &tx,
            &format!("SELECT id FROM sessions WHERE {purgeable}"),
            params![purge_cutoff],
        )?;
        for child in ["webchat_turn_cancellations", "session_ccr_references", "session_messages"] {
            tx.execute(
                &format!(
                    "DELETE FROM {child} WHERE session_id IN (
                        SELECT id FROM sessions WHERE {purgeable}
                    )"
                ),
                params![purge_cutoff],
            )
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        }
        let purged = tx
            .execute(
                &format!("DELETE FROM sessions WHERE {purgeable}"),
                params![purge_cutoff],
            )
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        tx.commit()
            .map_err(|e| DuDuClawError::Gateway(format!("commit transaction: {e}")))?;

        if archived > 0 || purged > 0 {
            info!(
                archived,
                purged, ?max_age_hours, purge_after_days, "Session cleanup: archived + purged"
            );
        }
        drop(conn);
        self.cancel_webchat_schedules(&purge_webchat_ids).await;
        Ok(RetentionOutcome { archived: archived as u64, purged: purged as u64 })
    }

    /// Ids (only `webchat:` ones) returned by `sql`, captured before a hard delete.
    fn webchat_ids(
        conn: &Connection,
        sql: &str,
        params: impl rusqlite::Params,
    ) -> Result<Vec<String>> {
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        let rows = stmt
            .query_map(params, |r| r.get::<_, String>(0))
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        let mut out = Vec::new();
        for r in rows {
            let id = r.map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
            if id.starts_with("webchat:") {
                out.push(id);
            }
        }
        Ok(out)
    }

    /// RM-124: after a batch hard delete has committed (and the sessions.db
    /// connection is released), cancel schedules / reminders aimed at each id.
    async fn cancel_webchat_schedules(&self, ids: &[String]) {
        if ids.is_empty() {
            return;
        }
        if let Some(home) = self.db_path.parent() {
            for id in ids {
                crate::scheduled_webchat::cancel_for_session(home, id).await;
            }
        }
    }

    // ── Instruction Pinning ────────────────────────────────────

    /// Get pinned task instructions for a session.
    ///
    /// Returns empty string if no instructions are pinned.
    pub async fn get_pinned(&self, session_id: &str) -> Result<String> {
        let conn = self.acquire().await;
        let result: std::result::Result<String, _> = conn.query_row(
            "SELECT COALESCE(pinned_instructions, '') FROM sessions WHERE id = ?1",
            params![session_id],
            |row| row.get(0),
        );
        match result {
            Ok(s) => Ok(s),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(String::new()),
            Err(e) => Err(DuDuClawError::Gateway(format!("get_pinned: {e}"))),
        }
    }

    /// Set (or replace) pinned task instructions for a session.
    pub async fn set_pinned(&self, session_id: &str, instructions: &str) -> Result<()> {
        let conn = self.acquire().await;
        conn.execute(
            "UPDATE sessions SET pinned_instructions = ?1 WHERE id = ?2",
            params![duduclaw_security::credential_detect::mask(instructions), session_id],
        )
        .map_err(|e| DuDuClawError::Gateway(format!("set_pinned: {e}")))?;
        Ok(())
    }

    /// Read the cached summary of older turns for this session (#13).
    ///
    /// Returns `(summary_text, summarized_through_turn)`. When no
    /// summary has been generated yet, summary_text is empty and
    /// summarized_through_turn is 0 — caller falls back to verbatim
    /// history.
    pub async fn get_summary(&self, session_id: &str) -> Result<(String, u32)> {
        let conn = self.acquire().await;
        let result: std::result::Result<(Option<String>, i64), _> = conn.query_row(
            "SELECT summary_of_prior, summarized_through_turn \
             FROM sessions WHERE id = ?1",
            params![session_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        );
        match result {
            Ok((s, t)) => Ok((s.unwrap_or_default(), t.max(0) as u32)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok((String::new(), 0)),
            Err(e) => Err(DuDuClawError::Gateway(format!("get_summary: {e}"))),
        }
    }

    /// Write (or overwrite) the summary for a session (#13).
    ///
    /// Caller has the responsibility to set `through_turn` to the
    /// inclusive turn index covered by `summary`. The background
    /// summarizer task is the only writer in production; tests can
    /// invoke this directly.
    pub async fn set_summary(
        &self,
        session_id: &str,
        summary: &str,
        through_turn: u32,
    ) -> Result<()> {
        let conn = self.acquire().await;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE sessions
             SET summary_of_prior = ?1,
                 summarized_through_turn = ?2,
                 last_summarized_at = ?3
             WHERE id = ?4",
            params![duduclaw_security::credential_detect::mask(summary), through_turn as i64, now, session_id],
        )
        .map_err(|e| DuDuClawError::Gateway(format!("set_summary: {e}")))?;
        Ok(())
    }

    /// Scan all sessions and emit summarization candidates (#13).
    ///
    /// One row per session — the background task then runs
    /// `session_summarizer::decide_summarization` to filter / order /
    /// quota-limit them. We intentionally don't filter at the SQL
    /// level: the policy lives in the pure module and is easier to
    /// test there.
    pub async fn list_summary_candidates(
        &self,
    ) -> Result<Vec<crate::session_summarizer::SummaryCandidate>> {
        let conn = self.acquire().await;
        let mut stmt = conn
            .prepare(
                "SELECT s.id, s.agent_id,
                        (SELECT COUNT(*) FROM session_messages m WHERE m.session_id = s.id AND m.hidden = 0 AND m.undone_at IS NULL) AS turn_count,
                        COALESCE(s.summarized_through_turn, 0) AS summarized_through_turn,
                        s.last_summarized_at
                 FROM sessions s",
            )
            .map_err(|e| DuDuClawError::Gateway(format!("list_summary_candidates prepare: {e}")))?;
        let rows = stmt
            .query_map([], |row| {
                let id: String = row.get(0)?;
                let agent_id: String = row.get(1)?;
                let turn_count: i64 = row.get(2)?;
                let summarized_through_turn: i64 = row.get(3)?;
                let last: Option<String> = row.get(4)?;
                Ok((id, agent_id, turn_count, summarized_through_turn, last))
            })
            .map_err(|e| DuDuClawError::Gateway(format!("list_summary_candidates query: {e}")))?;

        let now = chrono::Utc::now();
        let mut out = Vec::new();
        for row in rows.flatten() {
            let (id, agent_id, turn_count, summarized_through_turn, last) = row;
            let seconds_since_last_summary = last
                .as_deref()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|t| (now - t.with_timezone(&chrono::Utc)).num_seconds().max(0) as u64);
            out.push(crate::session_summarizer::SummaryCandidate {
                session_id: id,
                agent_id,
                turn_count: turn_count.max(0) as u32,
                summarized_through_turn: summarized_through_turn.max(0) as u32,
                seconds_since_last_summary,
            });
        }
        Ok(out)
    }

    /// Fetch the first N turns of a session for summarization, in order
    /// (#13). Returns the rendered transcript ready for the prompt.
    pub async fn read_first_n_turns_text(&self, session_id: &str, n: u32) -> Result<String> {
        let turns = self.read_first_n_turns(session_id, n).await?;
        let mut out = String::new();
        for (role, content) in turns {
            out.push_str(&role);
            out.push_str(": ");
            out.push_str(&content);
            out.push('\n');
        }
        Ok(out)
    }

    /// Bounded structured read for the background summarizer. Keeping turn
    /// boundaries lets it protect exact sections within each message.
    pub async fn read_first_n_turns(
        &self,
        session_id: &str,
        n: u32,
    ) -> Result<Vec<(String, String)>> {
        let conn = self.acquire().await;
        let mut stmt = conn
            .prepare(
                "SELECT role, content FROM session_messages
                 WHERE session_id = ?1 AND hidden = 0 AND undone_at IS NULL
                 ORDER BY id ASC
                 LIMIT ?2",
            )
            .map_err(|e| DuDuClawError::Gateway(format!("read_first_n prepare: {e}")))?;
        let rows = stmt
            .query_map(params![session_id, n as i64], |row| {
                let role: String = row.get(0)?;
                let content: String = row.get(1)?;
                Ok((role, content))
            })
            .map_err(|e| DuDuClawError::Gateway(format!("read_first_n query: {e}")))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| DuDuClawError::Gateway(format!("read_first_n rows: {e}")))
    }

    /// Fetch the LAST N turns of a session, in chronological order. Used by
    /// the auto-titler so a refreshed title reflects where the discussion has
    /// moved, not just how it started.
    pub async fn read_last_n_turns_text(&self, session_id: &str, n: u32) -> Result<String> {
        let conn = self.acquire().await;
        let mut stmt = conn
            .prepare(
                "SELECT role, content FROM (
                     SELECT id, role, content FROM session_messages
                     WHERE session_id = ?1 AND hidden = 0 AND undone_at IS NULL
                     ORDER BY id DESC
                     LIMIT ?2
                 ) ORDER BY id ASC",
            )
            .map_err(|e| DuDuClawError::Gateway(format!("read_last_n prepare: {e}")))?;
        let rows = stmt
            .query_map(params![session_id, n as i64], |row| {
                let role: String = row.get(0)?;
                let content: String = row.get(1)?;
                Ok((role, content))
            })
            .map_err(|e| DuDuClawError::Gateway(format!("read_last_n query: {e}")))?;
        let mut out = String::new();
        for (role, content) in rows.flatten() {
            out.push_str(&role);
            out.push_str(": ");
            out.push_str(&content);
            out.push('\n');
        }
        Ok(out)
    }

    /// List recently-active, still-live sessions for the auto-title task.
    ///
    /// `active_within_secs` bounds the backlog: sessions idle longer are never
    /// (re)titled, so a store full of historical sessions doesn't trigger an
    /// LLM stampede right after an upgrade. Ordered newest-activity first so
    /// the per-tick cap spends its budget on what the user is looking at.
    pub async fn list_title_candidates(
        &self,
        active_within_secs: u64,
    ) -> Result<Vec<TitleCandidate>> {
        let cutoff = (chrono::Utc::now()
            - chrono::Duration::seconds(active_within_secs.min(i64::MAX as u64) as i64))
        .to_rfc3339();
        let conn = self.acquire().await;
        let mut stmt = conn
            .prepare(
                "SELECT s.id,
                        (SELECT COUNT(*) FROM session_messages m
                           WHERE m.session_id = s.id AND m.hidden = 0 AND m.undone_at IS NULL),
                        COALESCE(s.titled_through_turn, 0), s.agent_id
                 FROM sessions s
                 WHERE s.archived_at IS NULL AND s.last_active >= ?1
                 ORDER BY s.last_active DESC",
            )
            .map_err(|e| DuDuClawError::Gateway(format!("list_title_candidates prepare: {e}")))?;
        let rows = stmt
            .query_map(params![cutoff], |row| {
                Ok(TitleCandidate {
                    session_id: row.get(0)?,
                    turn_count: row.get::<_, i64>(1)?.max(0) as u32,
                    titled_through_turn: row.get::<_, i64>(2)?.max(0) as u32,
                    agent_id: row.get(3)?,
                })
            })
            .map_err(|e| DuDuClawError::Gateway(format!("list_title_candidates query: {e}")))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| DuDuClawError::Gateway(e.to_string()))?);
        }
        Ok(out)
    }

    /// Persist an auto-generated session title (CJK-safe cap) together with
    /// the turn count it was generated at.
    pub async fn set_title(&self, session_id: &str, title: &str, through_turn: u32) -> Result<()> {
        let masked = duduclaw_security::credential_detect::mask(title);
        let clean = duduclaw_core::truncate_chars(masked.trim(), SESSION_TITLE_MAX_CHARS);
        let conn = self.acquire().await;
        conn.execute(
            "UPDATE sessions SET title = ?1, titled_through_turn = ?2 WHERE id = ?3",
            params![clean, through_turn as i64, session_id],
        )
        .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        Ok(())
    }

    /// Read back the stored auto-title (empty when never titled). Test/debug
    /// aid for the titler task.
    pub async fn get_title(&self, session_id: &str) -> Result<(String, u32)> {
        let conn = self.acquire().await;
        conn.query_row(
            "SELECT COALESCE(title, ''), COALESCE(titled_through_turn, 0)
             FROM sessions WHERE id = ?1",
            params![session_id],
            |row| Ok((row.get(0)?, row.get::<_, i64>(1)?.max(0) as u32)),
        )
        .map_err(|e| DuDuClawError::Gateway(e.to_string()))
    }

    /// Hide a message from the active context (Sculptor hide/restore).
    ///
    /// The message remains in the database and can be restored later.
    pub async fn hide_message(&self, session_id: &str, message_id: i64) -> Result<bool> {
        let conn = self.acquire().await;
        let rows = conn
            .execute(
                "UPDATE session_messages SET hidden = 1 WHERE id = ?1 AND session_id = ?2",
                params![message_id, session_id],
            )
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        Ok(rows > 0)
    }

    /// Revoke an already-persisted message whose CCR source lease could not be
    /// held through delivery.
    ///
    /// The row is both tombstoned (`undone_at`, so `get_messages` and every
    /// dashboard view stop returning it) and rewritten to `replacement`, so no
    /// source-derived wording survives the refusal on disk. The session's
    /// `total_tokens` is deliberately left alone: it is an append-only cost
    /// counter, not a context-size measure of the live rows.
    ///
    /// Returns `true` when a live row was actually rewritten.
    pub async fn revoke_message(
        &self,
        session_id: &str,
        message_id: i64,
        replacement: &str,
    ) -> Result<bool> {
        let conn = self.acquire().await;
        let now = chrono::Utc::now().to_rfc3339();
        let rows = conn
            .execute(
                "UPDATE session_messages SET content = ?1, undone_at = ?2, delivery_pending = 0
                 WHERE id = ?3 AND session_id = ?4 AND (undone_at IS NULL OR delivery_pending = 1)",
                params![replacement, now, message_id, session_id],
            )
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        Ok(rows > 0)
    }

    /// Publish under the users.db generation lease AND the live config/registry
    /// policy locks. No await separates the final policy check and UPDATE.
    pub(crate) async fn publish_webchat_message(
        &self, session_id: &str, message_id: i64, users: &duduclaw_auth::UserDb,
        generation: u64, policy: &crate::webchat::access::TargetPolicy, allowed: impl FnOnce() -> bool,
    ) -> Result<bool> {
        let conn = self.acquire().await;
        conn.busy_timeout(std::time::Duration::ZERO)
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        let result = users.with_acl_generation(generation, || {
            policy.with_current(|| {
                if !allowed() { return Ok(false); }
                conn.execute(
                    "UPDATE session_messages SET undone_at = NULL, delivery_pending = 0
                     WHERE id = ?1 AND session_id = ?2 AND role = 'assistant'
                       AND delivery_pending = 1 AND undone_at IS NOT NULL",
                    params![message_id, session_id],
                ).map(|rows| rows == 1).map_err(|e| DuDuClawError::Gateway(e.to_string()))
            }).map_err(DuDuClawError::Gateway).and_then(|result| result)
        }).map_err(DuDuClawError::Gateway).and_then(|result| result);
        if let Err(error) = conn.busy_timeout(std::time::Duration::from_secs(5)) {
            tracing::warn!(%error, "Session connection timeout restore failed");
        }
        result
    }

    /// Restore a previously hidden message back to active context.
    pub async fn restore_message(&self, session_id: &str, message_id: i64) -> Result<bool> {
        let conn = self.acquire().await;
        let rows = conn
            .execute(
                "UPDATE session_messages SET hidden = 0 WHERE id = ?1 AND session_id = ?2 AND notice_kind IS NULL",
                params![message_id, session_id],
            )
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
        Ok(rows > 0)
    }

    /// Search hidden messages for potential restoration.
    pub async fn search_hidden_messages(
        &self,
        session_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<(i64, SessionMessage)>> {
        let conn = self.acquire().await;
        // Escape LIKE wildcards to prevent wildcard injection
        let escaped_query = query
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let pattern = format!("%{escaped_query}%");
        let safe_limit = limit.min(200); // Cap at 200 to prevent memory abuse

        let mut stmt = conn
            .prepare(
                "SELECT id, role, content, tokens, timestamp
                 FROM session_messages
                 WHERE session_id = ?1 AND hidden = 1 AND undone_at IS NULL AND notice_kind IS NULL
                   AND content LIKE ?2 ESCAPE '\\'
                 ORDER BY id DESC
                 LIMIT ?3",
            )
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;

        let rows = stmt
            .query_map(params![session_id, pattern, safe_limit as i64], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    SessionMessage {
                        role: row.get(1)?,
                        content: row.get(2)?,
                        tokens: row.get(3)?,
                        timestamp: row.get(4)?,
                    },
                ))
            })
            .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;

        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| DuDuClawError::Gateway(e.to_string()))?);
        }
        Ok(out)
    }

    // ── WP3: WebChat session history list + resume ─────────────────

    /// List stored sessions for the WebChat history picker (WP3), newest first.
    ///
    /// - `agent_id`: `Some` restricts to a single agent; `None` lists across
    ///   every agent. Authz (admins-only for the unscoped case) is enforced by
    ///   the RPC layer — this method does not filter by user.
    /// - Archived (soft-deleted) sessions are excluded, matching the normal
    ///   listing semantics elsewhere in this module.
    /// - `title` prefers the stored auto-title (background titler task) and
    ///   falls back to the first still-visible user message, truncated to
    ///   [`SESSION_TITLE_MAX_CHARS`] characters (CJK-safe via `truncate_chars`);
    ///   empty when the session has neither.
    /// - `limit` is clamped to [`SESSION_LIST_MAX_LIMIT`].
    pub async fn list_sessions(
        &self,
        agent_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SessionSummary>> {
        self.list_sessions_in(agent_id, limit, false).await
    }

    /// [`Self::list_sessions`] over either live (`archived = false`) or only
    /// archived (`archived = true`, RM-173 「已收起的對話」) sessions.
    pub async fn list_sessions_in(
        &self,
        agent_id: Option<&str>,
        limit: usize,
        archived: bool,
    ) -> Result<Vec<SessionSummary>> {
        let archive_filter = if archived {
            "archived_at IS NOT NULL"
        } else {
            "archived_at IS NULL"
        };
        let conn = self.acquire().await;
        let capped = limit.min(SESSION_LIST_MAX_LIMIT) as i64;

        // E2: compute the per-row title/turn correlated subqueries ONLY for the
        // rows already narrowed by the inner LIMIT, instead of every row in
        // `sessions`. The inner query does the filter + `ORDER BY last_active
        // DESC LIMIT` (served by `idx_sessions_last_active`); the outer query
        // then runs the two subqueries against that small derived set `s`. The
        // two variants differ only in the agent filter, kept as separate static
        // SQL so parameters bind positionally (no string interpolation of
        // values, no injection surface). Result shape and ordering are unchanged.
        let outer_head = "SELECT s.id, s.agent_id, s.last_active, s.total_tokens,
                    COALESCE(s.lineage, 1), s.title,
                    (SELECT COUNT(*) FROM session_messages m
                       WHERE m.session_id = s.id AND m.hidden = 0 AND m.undone_at IS NULL) AS turns,
                    (SELECT m2.content FROM session_messages m2
                       WHERE m2.session_id = s.id AND m2.role = 'user'
                         AND m2.hidden = 0 AND m2.undone_at IS NULL
                       ORDER BY m2.id ASC LIMIT 1) AS first_user,
                    s.archive_reason
             FROM (";
        let outer_tail = ") s ORDER BY s.last_active DESC";

        let map_row = |row: &rusqlite::Row<'_>| -> rusqlite::Result<SessionSummary> {
            // Prefer the auto-generated title (background titler); fall back to
            // the first still-visible user message for untitled sessions.
            let stored: Option<String> = row.get(5)?;
            let first_user: Option<String> = row.get(7)?;
            // The fallback is a raw stored user message, which carries the
            // `[sender_id: …]` plumbing line — without stripping it every
            // untitled conversation is listed as "[sender_id: webchat:…]".
            let first_user_clean = first_user
                .as_deref()
                .map(crate::channel_reply::strip_sender_prefix)
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let picked = stored
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .or(first_user_clean);
            let title = picked
                .map(|s| duduclaw_core::truncate_chars(s, SESSION_TITLE_MAX_CHARS))
                .unwrap_or_default();
            Ok(SessionSummary {
                id: row.get(0)?,
                agent_id: row.get(1)?,
                last_active: row.get(2)?,
                total_tokens: row.get(3)?,
                lineage: row.get::<_, i64>(4)?.max(1) as u32,
                turn_count: row.get::<_, i64>(6)?.max(0) as u32,
                title,
                archive_reason: row.get(8)?,
            })
        };

        let mut out = Vec::new();
        match agent_id {
            Some(a) => {
                let sql = format!(
                    "{outer_head}SELECT id, agent_id, last_active, total_tokens, lineage, title, \
                     archive_reason \
                     FROM sessions \
                     WHERE {archive_filter} AND agent_id = ?1 \
                     ORDER BY last_active DESC LIMIT ?2{outer_tail}"
                );
                let mut stmt = conn
                    .prepare(&sql)
                    .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
                let rows = stmt
                    .query_map(params![a, capped], map_row)
                    .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
                for r in rows {
                    out.push(r.map_err(|e| DuDuClawError::Gateway(e.to_string()))?);
                }
            }
            None => {
                let sql = format!(
                    "{outer_head}SELECT id, agent_id, last_active, total_tokens, lineage, title, \
                     archive_reason \
                     FROM sessions \
                     WHERE {archive_filter} \
                     ORDER BY last_active DESC LIMIT ?1{outer_tail}"
                );
                let mut stmt = conn
                    .prepare(&sql)
                    .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
                let rows = stmt
                    .query_map(params![capped], map_row)
                    .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
                for r in rows {
                    out.push(r.map_err(|e| DuDuClawError::Gateway(e.to_string()))?);
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn webchat_client_message_id_survives_reopen_without_acceptance_frame() {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();
        mgr.get_or_create("webchat:owned", "worker").await.unwrap();
        let id = mgr.append_delivery_message("webchat:owned", "user", "same text", 2,
            Some("client-turn-1"), false).await.unwrap();
        mgr.append_delivery_message("webchat:owned", "user", "same text", 2,
            Some("client-turn-2"), false).await.unwrap();
        drop(mgr);
        let mgr = SessionManager::new(tmp.path()).unwrap();
        let history = mgr.webchat_history("webchat:owned").await.unwrap();
        assert_eq!(history[0].id, id);
        assert_eq!(history[0].message.role, "user");
        assert_eq!(history[0].client_msg_id.as_deref(), Some("client-turn-1"));
        assert_eq!(history[1].client_msg_id.as_deref(), Some("client-turn-2"));
        assert!(mgr.webchat_history("webchat:other").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn webchat_pending_publication_requires_current_acl_and_final_fence() {
        let tmp = NamedTempFile::new().unwrap();
        let user_file = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();
        let users = duduclaw_auth::UserDb::new(user_file.path()).unwrap();
        mgr.get_or_create("webchat:owned", "worker").await.unwrap();
        let id = mgr.append_delivery_message("webchat:owned", "assistant", "private reply", 2,
            None, true).await.unwrap();
        let generation = users.acl_generation().unwrap();
        Connection::open(user_file.path()).unwrap().execute(
            "UPDATE auth_meta SET value=value+1 WHERE key='acl_generation'", [],
        ).unwrap();
        assert!(mgr.publish_webchat_message("webchat:owned", id, &users, generation, &crate::webchat::access::TargetPolicy::test_admin(),
            || panic!("a stale generation must not enter the publication callback")).await.is_err());
        let generation = users.acl_generation().unwrap();
        assert!(!mgr.publish_webchat_message("webchat:owned", id, &users, generation, &crate::webchat::access::TargetPolicy::test_admin(), || false).await.unwrap());
        assert!(mgr.get_messages("webchat:owned").await.unwrap().is_empty());
        assert!(mgr.webchat_history("webchat:owned").await.unwrap().is_empty());
        assert!(mgr.publish_webchat_message("webchat:owned", id, &users, generation, &crate::webchat::access::TargetPolicy::test_admin(), || true).await.unwrap());
        assert_eq!(mgr.get_messages("webchat:owned").await.unwrap()[0].content, "private reply");
        assert!(!mgr.publish_webchat_message("webchat:owned", id, &users, generation, &crate::webchat::access::TargetPolicy::test_admin(), || true).await.unwrap());
    }

    #[tokio::test]
    async fn compression_preserves_pending_and_newly_published_replies() {
        let tmp = NamedTempFile::new().unwrap();
        let user_file = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();
        let users = duduclaw_auth::UserDb::new(user_file.path()).unwrap();
        mgr.get_or_create("webchat:owned", "worker").await.unwrap();
        mgr.append_message("webchat:owned", "user", "question", 2).await.unwrap();
        let before_reply = mgr.get_message_ids("webchat:owned").await.unwrap();
        let id = mgr.append_delivery_message("webchat:owned", "assistant", "private reply", 2,
            None, true).await.unwrap();
        assert!(mgr.compress("webchat:owned", "incomplete summary").await.is_err());
        assert!(mgr.compress_if_unchanged("webchat:owned", "incomplete summary", &before_reply).await.is_err());
        assert_eq!(mgr.compression_summary("webchat:owned").await.unwrap(), "");
        assert!(mgr.publish_webchat_message("webchat:owned", id, &users,
            users.acl_generation().unwrap(), &crate::webchat::access::TargetPolicy::test_admin(), || true).await.unwrap());
        // A previous background job may finish after this publication. Its
        // snapshot omitted the reply, so it cannot delete the completed turn.
        assert!(mgr.compress_if_unchanged("webchat:owned", "old summary", &before_reply).await.is_err());
        assert_eq!(mgr.get_messages("webchat:owned").await.unwrap().len(), 2);
        let after_reply = mgr.get_message_ids("webchat:owned").await.unwrap();
        mgr.compress_if_unchanged("webchat:owned", "complete summary", &after_reply).await.unwrap();
        assert_eq!(mgr.compression_summary("webchat:owned").await.unwrap(), "complete summary");
    }

    #[tokio::test]
    async fn webchat_scrubbed_pending_reply_cannot_be_published() {
        let tmp = NamedTempFile::new().unwrap();
        let user_file = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();
        let users = duduclaw_auth::UserDb::new(user_file.path()).unwrap();
        mgr.get_or_create("webchat:owned", "worker").await.unwrap();
        let id = mgr.append_delivery_message("webchat:owned", "assistant", "private reply", 2,
            None, true).await.unwrap();
        assert!(mgr.revoke_message("webchat:owned", id, "refused").await.unwrap());
        assert!(!mgr.publish_webchat_message("webchat:owned", id, &users,
            users.acl_generation().unwrap(), &crate::webchat::access::TargetPolicy::test_admin(), || true).await.unwrap());
        assert!(mgr.webchat_history("webchat:owned").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn webchat_cancellation_purge_uses_second_connection_and_explicit_delete() {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();
        for connection in &mgr.pool {
            let enabled: i64 = connection.lock().await
                .query_row("PRAGMA foreign_keys", [], |row| row.get(0)).unwrap();
            assert_eq!(enabled, 1, "every pooled connection enforces foreign keys");
        }
        for foreign_keys in [true, false] {
            mgr.get_or_create("webchat:purge", "agent").await.unwrap();
            mgr.record_webchat_cancel("webchat:purge", "agent", "turn", "msg")
                .await.unwrap();
            // Holding the first connection forces purge_session onto the second.
            let first = mgr.pool[0].lock().await;
            mgr.pool[1].lock().await.execute_batch(if foreign_keys {
                "PRAGMA foreign_keys=ON"
            } else {
                "PRAGMA foreign_keys=OFF"
            }).unwrap();
            mgr.purge_session("webchat:purge").await.unwrap();
            let count: i64 = first.query_row(
                "SELECT COUNT(*) FROM webchat_turn_cancellations", [], |row| row.get(0),
            ).unwrap();
            assert_eq!(count, 0, "explicit deletion does not depend on cascade");
            mgr.pool[1].lock().await.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        }
    }

    #[tokio::test]
    async fn webchat_cancellation_sweep_uses_shorter_session_retention_or_ninety_days() {
        for (retention_days, old_days) in [(30, 31), (180, 91)] {
            let tmp = NamedTempFile::new().unwrap();
            let mgr = SessionManager::new(tmp.path()).unwrap();
            mgr.get_or_create("webchat:retained", "agent").await.unwrap();
            mgr.record_webchat_cancel("webchat:retained", "agent", "old", "old-msg")
                .await.unwrap();
            mgr.record_webchat_cancel("webchat:retained", "agent", "recent", "new-msg")
                .await.unwrap();
            mgr.acquire().await.execute(
                "UPDATE webchat_turn_cancellations SET cancelled_at=?1 WHERE turn_id='old'",
                [(chrono::Utc::now() - chrono::Duration::days(old_days)).to_rfc3339()],
            ).unwrap();
            mgr.cleanup_inactive_with_retention(24, retention_days).await.unwrap();
            let turns: Vec<String> = mgr.acquire().await
                .prepare("SELECT turn_id FROM webchat_turn_cancellations").unwrap()
                .query_map([], |row| row.get(0)).unwrap()
                .collect::<std::result::Result<_, _>>().unwrap();
            assert_eq!(turns, vec!["recent"]);
            assert!(mgr.session_agent("webchat:retained").await.unwrap().is_some());
        }
    }

    #[tokio::test]
    async fn webchat_cancellation_cap_preserves_latest_and_other_sessions() {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();
        for session in ["webchat:busy", "webchat:other"] {
            mgr.get_or_create(session, "agent").await.unwrap();
        }
        mgr.record_webchat_cancel("webchat:other", "agent", "other", "msg")
            .await.unwrap();
        {
            let mut conn = mgr.acquire().await;
            let tx = conn.transaction().unwrap();
            for index in 0..WEBCHAT_CANCEL_PER_SESSION_CAP + 1 {
                tx.execute(
                    "INSERT INTO webchat_turn_cancellations VALUES
                     ('webchat:busy', ?1, 'old-msg', datetime('now', '-1 day'))",
                    [format!("old-{index}")],
                ).unwrap();
            }
            tx.commit().unwrap();
        }
        mgr.record_webchat_cancel("webchat:busy", "agent", "latest", "new-msg")
            .await.unwrap();
        let conn = mgr.acquire().await;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM webchat_turn_cancellations WHERE session_id='webchat:busy'",
            [], |row| row.get(0),
        ).unwrap();
        assert_eq!(count, WEBCHAT_CANCEL_PER_SESSION_CAP);
        for turn in ["latest", "other"] {
            let exists: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM webchat_turn_cancellations WHERE turn_id=?1)",
                [turn], |row| row.get(0),
            ).unwrap();
            assert!(exists);
        }
    }

    #[tokio::test]
    async fn ccr_references_follow_source_turn_undo_and_compression() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();
        mgr.get_or_create("support:s1", "support").await.unwrap();
        let user_id = mgr
            .append_message_with_id("support:s1", "user", "What is the SLA?", 5)
            .await
            .unwrap();
        let saved = duduclaw_llm::CcrSavedResult {
            scope: duduclaw_llm::CcrScope {
                tenant_id: "local".into(),
                agent_id: "support".into(),
                session_id: "support:s1".into(),
                source_acl: "principal-a".into(),
            },
            source_tool: "support-mcp/search".into(),
            source_call_id: "call-1".into(),
            id: "handle-1".into(),
            original_bytes: 9000,
            expires_at: chrono::Utc::now().timestamp() + 1800,
        };
        mgr.append_ccr_saved_results("support:s1", user_id, &[saved.clone()])
            .await
            .unwrap();
        assert_eq!(
            mgr.get_ccr_saved_results("support:s1", 5).await.unwrap(),
            vec![saved.clone()]
        );
        assert_eq!(mgr.get_messages("support:s1").await.unwrap().len(), 1);

        mgr.acquire()
            .await
            .execute(
                "UPDATE session_messages SET undone_at=?1 WHERE id=?2",
                params![chrono::Utc::now().to_rfc3339(), user_id],
            )
            .unwrap();
        assert!(
            mgr.get_ccr_saved_results("support:s1", 5)
                .await
                .unwrap()
                .is_empty()
        );

        let next_id = mgr
            .append_message_with_id("support:s1", "user", "Try again", 3)
            .await
            .unwrap();
        let mut expired = saved.clone();
        expired.id = "expired-handle".into();
        expired.expires_at = chrono::Utc::now().timestamp() - 1;
        mgr.append_ccr_saved_results("support:s1", next_id, &[expired])
            .await
            .unwrap();
        assert!(
            mgr.get_ccr_saved_results("support:s1", 5)
                .await
                .unwrap()
                .is_empty()
        );
        mgr.append_ccr_saved_results("support:s1", next_id, &[saved])
            .await
            .unwrap();
        mgr.compress("support:s1", "summary").await.unwrap();
        assert!(
            mgr.get_ccr_saved_results("support:s1", 5)
                .await
                .unwrap()
                .is_empty()
        );
        let count: i64 = mgr
            .acquire()
            .await
            .query_row(
                "SELECT COUNT(*) FROM session_ccr_references WHERE session_id=?1",
                params!["support:s1"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }
    #[tokio::test]
    async fn wp5_soft_delete_archives_then_purge_removes() {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();
        mgr.get_or_create("s1", "a").await.unwrap();
        mgr.append_message("s1", "user", "hi", 2).await.unwrap();

        // delete = archive under a moved id (RM-173): messages survive,
        // replayable there; the original id starts fresh.
        mgr.delete_session("s1").await.unwrap();
        let moved = mgr.list_sessions_in(Some("a"), 10, true).await.unwrap()[0].id.clone();
        assert!(moved.starts_with("s1#cleared:"), "{moved}");
        assert_eq!(
            mgr.get_messages(&moved).await.unwrap().len(),
            1,
            "archived session keeps messages"
        );
        assert!(mgr.get_messages("s1").await.unwrap().is_empty());

        // unarchive brings it back to live.
        mgr.unarchive_session(&moved).await.unwrap();
        assert!(mgr.archived_at(&moved).await.unwrap().is_none());

        // purge = hard delete: messages gone.
        mgr.purge_session(&moved).await.unwrap();
        assert_eq!(
            mgr.get_messages(&moved).await.unwrap().len(),
            0,
            "purge removes messages"
        );
    }

    #[tokio::test]
    async fn e2_list_sessions_limit_order_and_title() {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();

        // Three sessions for the same agent; each user turn bumps last_active.
        // Append in s1 → s2 → s3 order so s3 is the most recently active.
        for (sid, first) in [
            ("s1", "第一個問題"),
            ("s2", "second question"),
            ("s3", "third"),
        ] {
            mgr.get_or_create(sid, "agent-x").await.unwrap();
            mgr.append_message(sid, "user", first, 3).await.unwrap();
            mgr.append_message(sid, "assistant", "reply", 3)
                .await
                .unwrap();
            // Tiny gap so rfc3339 last_active timestamps are strictly ordered.
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        // A session for a DIFFERENT agent — must not leak into the scoped list.
        mgr.get_or_create("other", "agent-y").await.unwrap();
        mgr.append_message("other", "user", "不該出現", 3)
            .await
            .unwrap();

        // Agent-scoped list: newest first, correct title (first user turn) and
        // turn_count (visible messages only).
        let scoped = mgr.list_sessions(Some("agent-x"), 10).await.unwrap();
        assert_eq!(scoped.len(), 3, "only agent-x sessions");
        assert_eq!(scoped[0].id, "s3", "newest last_active first");
        assert_eq!(scoped[0].title, "third");
        assert_eq!(scoped[0].turn_count, 2, "2 visible messages");
        assert!(scoped.iter().all(|s| s.agent_id == "agent-x"));

        // LIMIT is honored AFTER ordering: the single row is the newest one.
        let limited = mgr.list_sessions(Some("agent-x"), 1).await.unwrap();
        assert_eq!(limited.len(), 1);
        assert_eq!(
            limited[0].id, "s3",
            "LIMIT keeps the newest, not an arbitrary row"
        );

        // Unscoped list spans agents (admin path) and stays newest-first.
        let all = mgr.list_sessions(None, 10).await.unwrap();
        assert_eq!(all.len(), 4);
        assert_eq!(
            all[0].id, "other",
            "the other-agent session was appended last"
        );

        // Archived sessions drop out of both listings.
        mgr.delete_session("s3").await.unwrap();
        let after = mgr.list_sessions(Some("agent-x"), 10).await.unwrap();
        assert_eq!(after.len(), 2);
        assert!(after.iter().all(|s| s.id != "s3"));
    }

    #[tokio::test]
    async fn wp5_reply_to_session_mapping() {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();
        mgr.get_or_create("telegram:42:task-A", "a").await.unwrap();
        mgr.record_message_session("telegram", "msg-777", "telegram:42:task-A")
            .await
            .unwrap();

        // Replying to msg-777 resumes the mapped session.
        assert_eq!(
            mgr.session_for_reply("telegram", "msg-777")
                .await
                .unwrap()
                .as_deref(),
            Some("telegram:42:task-A")
        );
        // Unknown message / wrong channel ⇒ None (caller uses main session).
        assert_eq!(
            mgr.session_for_reply("telegram", "nope").await.unwrap(),
            None
        );
        assert_eq!(
            mgr.session_for_reply("slack", "msg-777").await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn test_hide_restore_message() {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();

        // Create session and add messages
        mgr.get_or_create("test-session", "test-agent")
            .await
            .unwrap();
        mgr.append_message("test-session", "user", "hello", 5)
            .await
            .unwrap();
        mgr.append_message("test-session", "assistant", "world", 5)
            .await
            .unwrap();

        // Should have 2 visible messages
        let msgs = mgr.get_messages("test-session").await.unwrap();
        assert_eq!(msgs.len(), 2);

        // Hide first message (id=1)
        let hidden = mgr.hide_message("test-session", 1).await.unwrap();
        assert!(hidden);

        // Should have 1 visible message
        let msgs = mgr.get_messages("test-session").await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "world");

        // Restore
        let restored = mgr.restore_message("test-session", 1).await.unwrap();
        assert!(restored);

        // Should have 2 visible messages again
        let msgs = mgr.get_messages("test-session").await.unwrap();
        assert_eq!(msgs.len(), 2);
    }

    #[tokio::test]
    async fn test_gdpr_erase_sessions_for_contact() {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();

        // A contact's base chat + a thread of it, plus an unrelated contact.
        mgr.get_or_create("telegram:12345", "a").await.unwrap();
        mgr.append_message("telegram:12345", "user", "hi", 1)
            .await
            .unwrap();
        mgr.get_or_create("telegram:12345:77", "a").await.unwrap();
        mgr.append_message("telegram:12345:77", "user", "thread", 1)
            .await
            .unwrap();
        mgr.get_or_create("telegram:99999", "a").await.unwrap();
        mgr.append_message("telegram:99999", "user", "other", 1)
            .await
            .unwrap();
        // A prefix-collision guard: `telegram:123456` must NOT match `telegram:12345`.
        mgr.get_or_create("telegram:123456", "a").await.unwrap();

        let ids = mgr.sessions_for_contact("telegram:12345").await.unwrap();
        assert_eq!(
            ids.len(),
            2,
            "exact id + its thread, not the 99999 or 123456"
        );

        let (sessions, messages) = mgr
            .erase_sessions_for_contact("telegram:12345")
            .await
            .unwrap();
        assert_eq!(sessions, 2);
        assert_eq!(messages, 2);

        // The unrelated contacts survive.
        assert_eq!(mgr.get_messages("telegram:99999").await.unwrap().len(), 1);
        assert!(
            mgr.sessions_for_contact("telegram:123456")
                .await
                .unwrap()
                .len()
                == 1
        );
        // The erased contact is gone.
        assert!(
            mgr.sessions_for_contact("telegram:12345")
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn test_search_hidden_messages() {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();

        mgr.get_or_create("test-session", "test-agent")
            .await
            .unwrap();
        mgr.append_message("test-session", "tool", "search result: found 5 items", 10)
            .await
            .unwrap();
        mgr.append_message("test-session", "assistant", "I found 5 items", 5)
            .await
            .unwrap();

        // Hide the tool result
        mgr.hide_message("test-session", 1).await.unwrap();

        // Search hidden messages
        let found = mgr
            .search_hidden_messages("test-session", "search result", 10)
            .await
            .unwrap();
        assert_eq!(found.len(), 1);
        assert!(found[0].1.content.contains("search result"));
    }

    #[tokio::test]
    async fn webchat_notice_migration_preserves_legacy_messages() {
        let tmp = NamedTempFile::new().unwrap();
        let conn = Connection::open(tmp.path()).unwrap();
        conn.execute_batch(
            "CREATE TABLE session_messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL,
                role TEXT NOT NULL, content TEXT NOT NULL, tokens INTEGER DEFAULT 0,
                timestamp TEXT NOT NULL, hidden INTEGER NOT NULL DEFAULT 0, undone_at TEXT
             );
             INSERT INTO session_messages(session_id, role, content, timestamp)
             VALUES ('legacy', 'user', 'old question', '2026-11-01T00:00:00Z');",
        ).unwrap();
        drop(conn);
        let mgr = SessionManager::new(tmp.path()).unwrap();
        mgr.get_or_create("legacy", "agent").await.unwrap();
        let old = mgr.webchat_history("legacy").await.unwrap();
        assert_eq!(old.len(), 1);
        assert!(old[0].notice_kind.is_none());
        assert_eq!(old[0].message.content, "old question");
        mgr.record_webchat_failure("legacy", "failed").await.unwrap();
        assert_eq!(mgr.get_messages("legacy").await.unwrap().len(), 1);
        assert_eq!(mgr.webchat_history("legacy").await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn webchat_failures_are_display_only_and_cannot_be_restored() {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();
        mgr.get_or_create("s1", "agent").await.unwrap();
        mgr.append_message("s1", "user", "question", 5).await.unwrap();
        let hidden = mgr.append_message_with_id("s1", "assistant", "ordinary hidden", 5).await.unwrap();
        mgr.hide_message("s1", hidden).await.unwrap();
        let redacted = mgr.append_message_with_id("s1", "assistant", "redacted", 5).await.unwrap();
        mgr.revoke_message("s1", redacted, "redacted").await.unwrap();
        mgr.record_webchat_failure("s1", "failure password: fixture-secret").await.unwrap();
        let display = mgr.webchat_history("s1").await.unwrap();
        assert_eq!(display.len(), 2);
        assert_eq!(display[0].message.content, "question");
        assert_eq!(display[1].notice_kind.as_deref(), Some("turn_failure"));
        assert!(!display[1].message.content.contains("fixture-secret"));
        let failure_id = display[1].id;
        let conn = mgr.acquire().await;
        let flags: (i64, String) = conn.query_row(
            "SELECT hidden, notice_kind FROM session_messages WHERE id=?1", [failure_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap();
        assert_eq!(flags, (1, "turn_failure".into()));
        drop(conn);
        assert!(!mgr.restore_message("s1", failure_id).await.unwrap());
        assert!(mgr.search_hidden_messages("s1", "failure", 10).await.unwrap().is_empty());
        assert_eq!(mgr.get_messages("s1").await.unwrap().len(), 1);
        assert_eq!(mgr.get_message_ids("s1").await.unwrap().len(), 1);
        assert_eq!(mgr.read_first_n_turns("s1", 100).await.unwrap(),
            vec![("user".to_string(), "question".to_string())]);
        assert_eq!(mgr.read_first_n_turns_text("s1", 100).await.unwrap(), "user: question\n");
        assert_eq!(mgr.read_last_n_turns_text("s1", 100).await.unwrap(), "user: question\n");
        assert_eq!(mgr.list_summary_candidates().await.unwrap()[0].turn_count, 1);
        mgr.record_webchat_failure("s1", &"錯".repeat(2100)).await.unwrap();
        let display = mgr.webchat_history("s1").await.unwrap();
        assert_eq!(display.last().unwrap().message.content.chars().count(), 2000);
        // Undo/redaction still hides a notice from the display.
        mgr.revoke_message("s1", failure_id, "redacted").await.unwrap();
        assert!(mgr.webchat_history("s1").await.unwrap().iter().all(|m| m.id != failure_id));
        drop(mgr);
        let reopened = SessionManager::new(tmp.path()).unwrap();
        assert_eq!(reopened.webchat_history("s1").await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_get_messages_excludes_hidden() {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();

        mgr.get_or_create("s1", "agent").await.unwrap();
        mgr.append_message("s1", "user", "msg1", 5).await.unwrap();
        mgr.append_message("s1", "assistant", "msg2", 5)
            .await
            .unwrap();
        mgr.append_message("s1", "user", "msg3", 5).await.unwrap();

        // Hide middle message
        mgr.hide_message("s1", 2).await.unwrap();

        let msgs = mgr.get_messages("s1").await.unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "msg1");
        assert_eq!(msgs[1].content, "msg3");
    }

    #[tokio::test]
    async fn test_pinned_instructions_roundtrip() {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();
        mgr.get_or_create("pin-test", "agent").await.unwrap();

        // Initially empty
        let pinned = mgr.get_pinned("pin-test").await.unwrap();
        assert!(pinned.is_empty());

        // Set pinned
        mgr.set_pinned(
            "pin-test",
            "- Goal: build two teams\n- Constraint: use Rust",
        )
        .await
        .unwrap();
        let pinned = mgr.get_pinned("pin-test").await.unwrap();
        assert!(pinned.contains("build two teams"));
        assert!(pinned.contains("use Rust"));

        // Update pinned (accumulate)
        let updated = format!("{pinned}\n- 用戶確認：PM 每天 8:00 回報");
        mgr.set_pinned("pin-test", &updated).await.unwrap();
        let pinned = mgr.get_pinned("pin-test").await.unwrap();
        assert!(pinned.contains("PM 每天 8:00 回報"));
    }

    #[tokio::test]
    async fn test_pinned_survives_compression() {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();
        mgr.get_or_create("compress-pin", "agent").await.unwrap();

        // Set pinned + add messages
        mgr.set_pinned("compress-pin", "- Goal: deploy v2.0")
            .await
            .unwrap();
        mgr.append_message("compress-pin", "user", "hello", 5)
            .await
            .unwrap();
        mgr.append_message("compress-pin", "assistant", "world", 5)
            .await
            .unwrap();

        // Compress — should delete messages but keep pinned
        mgr.compress("compress-pin", "Summary: user said hello")
            .await
            .unwrap();

        // Messages gone (replaced with summary)
        let msgs = mgr.get_messages("compress-pin").await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "system");

        // Pinned survived!
        let pinned = mgr.get_pinned("compress-pin").await.unwrap();
        assert_eq!(pinned, "- Goal: deploy v2.0");
    }

    #[tokio::test]
    async fn test_pinned_nonexistent_session() {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();

        // Non-existent session returns empty (not error)
        let pinned = mgr.get_pinned("does-not-exist").await.unwrap();
        assert!(pinned.is_empty());
    }

    // ── WP3: session list + resume support ─────────────────────────

    #[tokio::test]
    async fn wp3_list_sessions_by_agent_newest_first_with_titles() {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();

        // Agent "alice": two sessions, plus one belonging to another agent.
        mgr.get_or_create("webchat:s1", "alice").await.unwrap();
        mgr.append_message("webchat:s1", "user", "第一個問題：怎麼部署", 6)
            .await
            .unwrap();
        mgr.append_message("webchat:s1", "assistant", "答覆", 2)
            .await
            .unwrap();

        mgr.get_or_create("webchat:s2", "alice").await.unwrap();
        mgr.append_message("webchat:s2", "user", "second question about billing", 5)
            .await
            .unwrap();

        mgr.get_or_create("webchat:other", "bob").await.unwrap();
        mgr.append_message("webchat:other", "user", "bob's private chat", 4)
            .await
            .unwrap();

        // Scoped list only returns alice's sessions, newest (s2) first.
        let list = mgr.list_sessions(Some("alice"), 50).await.unwrap();
        assert_eq!(list.len(), 2, "only alice's sessions, not bob's");
        assert_eq!(list[0].id, "webchat:s2", "most recently active first");
        assert_eq!(list[0].title, "second question about billing");
        assert_eq!(list[1].id, "webchat:s1");
        // CJK title survives whole (well under the char cap).
        assert_eq!(list[1].title, "第一個問題：怎麼部署");
        // Turn counts reflect visible messages.
        assert_eq!(list[1].turn_count, 2);
        assert_eq!(list[0].turn_count, 1);

        // Unscoped list (admin path) sees every agent's sessions.
        let all = mgr.list_sessions(None, 50).await.unwrap();
        assert_eq!(all.len(), 3);
    }

    #[tokio::test]
    async fn wp3_title_truncates_cjk_safely() {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();
        mgr.get_or_create("webchat:long", "alice").await.unwrap();
        // 200 CJK chars — longer than SESSION_TITLE_MAX_CHARS (80).
        let long: String = "測".repeat(200);
        mgr.append_message("webchat:long", "user", &long, 100)
            .await
            .unwrap();

        let list = mgr.list_sessions(Some("alice"), 50).await.unwrap();
        assert_eq!(list.len(), 1);
        // Truncated to exactly 80 *characters* (not bytes) — no panic, no
        // mid-codepoint split.
        assert_eq!(list[0].title.chars().count(), SESSION_TITLE_MAX_CHARS);
        assert!(list[0].title.chars().all(|c| c == '測'));
    }

    #[tokio::test]
    async fn wp3_archived_sessions_excluded_from_list() {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();
        mgr.get_or_create("webchat:live", "alice").await.unwrap();
        mgr.append_message("webchat:live", "user", "live one", 2)
            .await
            .unwrap();
        mgr.get_or_create("webchat:gone", "alice").await.unwrap();
        mgr.append_message("webchat:gone", "user", "archive me", 2)
            .await
            .unwrap();

        mgr.delete_session("webchat:gone").await.unwrap(); // soft delete = archive

        let list = mgr.list_sessions(Some("alice"), 50).await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "webchat:live");
    }

    #[tokio::test]
    async fn wp3_session_agent_resolves_owner_and_missing() {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();
        mgr.get_or_create("webchat:s1", "alice").await.unwrap();

        assert_eq!(
            mgr.session_agent("webchat:s1").await.unwrap().as_deref(),
            Some("alice")
        );
        // Unknown id → None (caller must fail closed, not open a new session).
        assert_eq!(mgr.session_agent("webchat:nope").await.unwrap(), None);
    }

    #[tokio::test]
    async fn wp3_resume_writes_into_specified_session() {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();

        // Two distinct sessions for the same agent.
        mgr.get_or_create("webchat:old", "alice").await.unwrap();
        mgr.append_message("webchat:old", "user", "old turn", 2)
            .await
            .unwrap();
        mgr.get_or_create("webchat:new", "alice").await.unwrap();

        // Resuming "old" appends there, leaving "new" untouched — proving a
        // resume targets the requested session, not a fresh one.
        mgr.append_message("webchat:old", "user", "resumed turn", 3)
            .await
            .unwrap();

        let old_msgs = mgr.get_messages("webchat:old").await.unwrap();
        assert_eq!(old_msgs.len(), 2);
        assert_eq!(old_msgs[1].content, "resumed turn");
        assert_eq!(mgr.get_messages("webchat:new").await.unwrap().len(), 0);
    }
}

/// RM-173: retention settings, purge-off default and un-archive on activity.
#[cfg(test)]
mod rm173_retention_tests {
    use super::*;
    use crate::session_retention::Retention;
    use tempfile::NamedTempFile;

    async fn aged(mgr: &SessionManager, id: &str, active_days_ago: i64) {
        mgr.get_or_create(id, "agent").await.unwrap();
        mgr.append_message(id, "user", "hello", 1).await.unwrap();
        let at = (chrono::Utc::now() - chrono::Duration::days(active_days_ago)).to_rfc3339();
        mgr.acquire()
            .await
            .execute("UPDATE sessions SET last_active=?2 WHERE id=?1", params![id, at])
            .unwrap();
    }

    async fn archived_days_ago(mgr: &SessionManager, id: &str, days: i64) {
        let at = (chrono::Utc::now() - chrono::Duration::days(days)).to_rfc3339();
        mgr.acquire()
            .await
            .execute("UPDATE sessions SET archived_at=?2 WHERE id=?1", params![id, at])
            .unwrap();
    }

    #[tokio::test]
    async fn rm173_archive_uses_the_configured_days() {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();
        aged(&mgr, "four-days", 4).await;
        aged(&mgr, "forty-days", 40).await;
        // Default 30 days: the old 72-hour rule no longer archives a 4-day lull.
        let out = mgr.apply_retention(Retention::default()).await.unwrap();
        assert_eq!(out, RetentionOutcome { archived: 1, purged: 0 });
        assert!(mgr.archived_at("forty-days").await.unwrap().is_some());
        assert!(mgr.archived_at("four-days").await.unwrap().is_none());
        // A shorter setting archives the other one too.
        let three = Retention { archive_after_days: 3, purge_after_days: 0 };
        assert_eq!(mgr.apply_retention(three).await.unwrap().archived, 1);
        assert!(mgr.archived_at("four-days").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn rm173_archive_zero_never_archives() {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();
        aged(&mgr, "ancient", 3000).await;
        let never = Retention { archive_after_days: 0, purge_after_days: 0 };
        assert_eq!(mgr.apply_retention(never).await.unwrap(), RetentionOutcome::default());
        assert!(mgr.archived_at("ancient").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn rm173_purge_is_disabled_by_default_and_enabled_by_setting() {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();
        aged(&mgr, "long-archived", 900).await;
        aged(&mgr, "recently-archived", 900).await;
        mgr.apply_retention(Retention::default()).await.unwrap();
        archived_days_ago(&mgr, "long-archived", 800).await;
        archived_days_ago(&mgr, "recently-archived", 5).await;

        // Default: nothing is ever deleted, however old the archive.
        let out = mgr.apply_retention(Retention::default()).await.unwrap();
        assert_eq!(out.purged, 0);
        assert_eq!(mgr.get_messages("long-archived").await.unwrap().len(), 1);
        // archive-only helper never purges either.
        mgr.cleanup_inactive(24).await.unwrap();
        assert_eq!(mgr.get_messages("long-archived").await.unwrap().len(), 1);

        // Explicit purge setting: counted from archived_at.
        let purge = Retention { archive_after_days: 30, purge_after_days: 365 };
        assert_eq!(mgr.apply_retention(purge).await.unwrap().purged, 1);
        assert!(mgr.session_agent("long-archived").await.unwrap().is_none());
        assert!(mgr.get_messages("long-archived").await.unwrap().is_empty());
        assert_eq!(mgr.get_messages("recently-archived").await.unwrap().len(), 1);
    }

    /// Archive like the inactivity task does (reason `retention`).
    async fn retention_archive(mgr: &SessionManager, id: &str) {
        mgr.acquire()
            .await
            .execute(
                "UPDATE sessions SET archived_at=?2, archive_reason='retention' WHERE id=?1",
                params![id, chrono::Utc::now().to_rfc3339()],
            )
            .unwrap();
    }

    #[tokio::test]
    async fn rm173_new_message_unarchives_in_the_same_statement() {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();
        for (id, role) in [("webchat:u", "user"), ("line:c", "assistant")] {
            mgr.get_or_create(id, "agent").await.unwrap();
            mgr.append_message(id, "user", "first", 1).await.unwrap();
            retention_archive(&mgr, id).await;
            assert!(mgr.archived_at(id).await.unwrap().is_some());
            assert!(mgr.list_sessions(Some("agent"), 10).await.unwrap().iter().all(|s| s.id != id));
            mgr.append_message(id, role, "back again", 1).await.unwrap();
            assert!(mgr.archived_at(id).await.unwrap().is_none(), "{id}");
            assert!(mgr.archive_state(id).await.unwrap().is_none());
            assert!(mgr.list_sessions(Some("agent"), 10).await.unwrap().iter().any(|s| s.id == id));
        }
        // The WebChat delivery path (client message id / pending reply) too.
        retention_archive(&mgr, "webchat:u").await;
        mgr.append_delivery_message("webchat:u", "user", "again", 1, Some("c-1"), false)
            .await
            .unwrap();
        assert!(mgr.archived_at("webchat:u").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn rm173_activity_never_unarchives_a_user_cleared_row() {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();
        mgr.get_or_create("line:U1", "agent").await.unwrap();
        mgr.append_message("line:U1", "user", "first", 1).await.unwrap();
        let moved = mgr.clear_for_user("line:U1").await.unwrap().unwrap();
        // Something addressing the moved id directly still cannot lift it.
        mgr.append_message(&moved, "assistant", "late", 1).await.unwrap();
        let (_, reason) = mgr.archive_state(&moved).await.unwrap().unwrap();
        assert_eq!(reason, ARCHIVE_REASON_USER);
        // A retention run never relabels it either.
        mgr.apply_retention(Retention { archive_after_days: 1, purge_after_days: 0 })
            .await
            .unwrap();
        assert_eq!(mgr.archive_state(&moved).await.unwrap().unwrap().1, ARCHIVE_REASON_USER);
        // Clearing an already-cleared row is a no-op; a missing row too.
        assert!(mgr.clear_for_user(&moved).await.unwrap().is_none());
        assert!(mgr.clear_for_user("line:never").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn rm173_new_on_a_reused_channel_id_starts_a_genuinely_fresh_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("sessions.db");
        let mgr = SessionManager::new(&db).unwrap();
        let id = "line:Ureused";
        let channel = crate::session_origin::SessionOrigin::of_kind(
            crate::session_origin::OriginKind::Channel,
        );
        mgr.get_or_create_with_origin(id, "agent", &channel).await.unwrap();
        mgr.append_message(id, "user", "PRE-CLEAR-SECRET question", 3).await.unwrap();
        mgr.append_message(id, "assistant", "PRE-CLEAR-SECRET answer", 3).await.unwrap();
        mgr.record_message_session("line", "bot-msg-1", id).await.unwrap();
        mgr.record_webchat_cancel(id, "agent", "turn-1", "msg-1").await.unwrap();
        let user_turn = mgr.get_message_ids(id).await.unwrap()[0];
        mgr.acquire()
            .await
            .execute(
                "INSERT INTO session_ccr_references
                 (session_id,user_message_id,ccr_id,record_json,expires_at,created_at)
                 VALUES (?1,?2,'ccr-1','{}',?3,'now')",
                params![id, user_turn, chrono::Utc::now().timestamp() + 3600],
            )
            .unwrap();

        // `/new`.
        let moved = mgr.clear_for_user(id).await.unwrap().expect("cleared");
        assert!(moved.starts_with("line:Ureused#cleared:"), "{moved}");
        assert!(mgr.session_agent(id).await.unwrap().is_none(), "no row at the old id");
        assert_eq!(
            mgr.archive_state(&moved).await.unwrap().map(|(_, r)| r).as_deref(),
            Some(ARCHIVE_REASON_USER)
        );
        // Everything that must follow it moved, in the same transaction.
        assert_eq!(mgr.get_messages(&moved).await.unwrap().len(), 2);
        let conn = mgr.acquire().await;
        let count = |sql: &str, sid: &str| -> i64 {
            conn.query_row(sql, params![sid], |r| r.get(0)).unwrap()
        };
        let cancels = "SELECT COUNT(*) FROM webchat_turn_cancellations WHERE session_id=?1";
        assert_eq!((count(cancels, id), count(cancels, &moved)), (0, 1));
        let ccr = "SELECT COUNT(*) FROM session_ccr_references WHERE session_id=?1";
        assert_eq!((count(ccr, id), count(ccr, &moved)), (0, 0), "dropped, not moved");
        let origin: Option<String> = conn
            .query_row("SELECT origin_kind FROM sessions WHERE id=?1", params![moved], |r| r.get(0))
            .unwrap();
        assert_eq!(origin.as_deref(), Some("channel"), "the row's own columns moved with it");
        let mut fk = conn.prepare("PRAGMA foreign_key_check").unwrap();
        assert!(fk.query([]).unwrap().next().unwrap().is_none(), "no dangling children");
        drop(fk);
        drop(conn);
        // A reply to an old bot message must not resume the cleared context.
        assert_eq!(mgr.session_for_reply("line", "bot-msg-1").await.unwrap(), None);

        // The next message creates a fresh session (normal origin stamping).
        mgr.get_or_create_with_origin(id, "agent", &channel).await.unwrap();
        mgr.append_message(id, "user", "brand new topic", 3).await.unwrap();
        let context = mgr.get_messages(id).await.unwrap();
        assert_eq!(context.len(), 1);
        assert!(context.iter().all(|m| !m.content.contains("PRE-CLEAR-SECRET")));
        assert_eq!(mgr.compression_summary(id).await.unwrap(), "");
        assert_eq!(
            mgr.session_origin(id).await.unwrap().map(|o| o.kind()),
            Some(crate::session_origin::OriginKind::Channel)
        );
        // The cleared row stays archived despite the activity on the old id.
        assert!(mgr.archive_state(&moved).await.unwrap().is_some());

        // Lists and search never show the old content on the live side.
        let live = mgr.list_sessions(Some("agent"), 10).await.unwrap();
        assert_eq!(live.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), [id]);
        assert_eq!(live[0].title, "brand new topic");
        assert_eq!(live[0].turn_count, 1);
        assert!(crate::search_index::search_conversations(&db, None, "PRE-CLEAR-SECRET", 10).is_empty());
        assert_eq!(crate::search_index::search_conversations(&db, None, "brand new", 10).len(), 1);
        // The user's own 「已收起」 list shows it, labelled.
        let archived = mgr.list_sessions_in(Some("agent"), 10, true).await.unwrap();
        assert_eq!(archived.len(), 1);
        assert_eq!(archived[0].id, moved);
        assert_eq!(archived[0].archive_reason.as_deref(), Some(ARCHIVE_REASON_USER));

        // GDPR erase of the contact reaches the cleared row too.
        let ids = mgr.sessions_for_contact(id).await.unwrap();
        assert_eq!(ids.len(), 2, "{ids:?}");
        assert_eq!(mgr.erase_sessions_for_contact(id).await.unwrap(), (2, 3));
    }

    /// N1: before RM-173 the only writer of `archived_at` was `delete_session`
    /// (a user's `/new`), so every legacy archived row is a user clear: it is
    /// labelled `user`, moved off its reusable id, hidden from team history,
    /// and the next message on the original id starts a fresh conversation.
    #[tokio::test]
    async fn rm173_migration_moves_legacy_archives_as_user_clears() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("sessions.db");
        {
            let c = Connection::open(&db).unwrap();
            c.execute_batch(
                "CREATE TABLE sessions (id TEXT PRIMARY KEY, agent_id TEXT NOT NULL,
                   summary TEXT DEFAULT '', total_tokens INTEGER DEFAULT 0,
                   last_active TEXT NOT NULL, model TEXT DEFAULT 'auto',
                   created_at TEXT NOT NULL, archived_at TEXT);
                 CREATE TABLE session_messages (id INTEGER PRIMARY KEY AUTOINCREMENT,
                   session_id TEXT NOT NULL REFERENCES sessions(id), role TEXT NOT NULL,
                   content TEXT NOT NULL, tokens INTEGER DEFAULT 0, timestamp TEXT NOT NULL);
                 INSERT INTO sessions(id,agent_id,last_active,created_at,archived_at)
                   VALUES ('line:Uold','sales','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z','2026-02-01T00:00:00Z'),
                          ('line:Ulive','sales','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z',NULL);
                 INSERT INTO session_messages(session_id,role,content,timestamp)
                   VALUES ('line:Uold','user','LEGACY-CLEARED-SECRET','2026-01-01T00:00:00Z'),
                          ('line:Ulive','user','still here','2026-01-01T00:00:00Z');",
            )
            .unwrap();
        }
        let mgr = SessionManager::new(&db).unwrap();
        // Moved: no row at the reusable id any more.
        assert!(mgr.session_agent("line:Uold").await.unwrap().is_none());
        let moved = mgr.sessions_for_contact("line:Uold").await.unwrap();
        assert_eq!(moved.len(), 1, "{moved:?}");
        let moved = &moved[0];
        assert!(moved.starts_with("line:Uold#cleared:"), "{moved}");
        let (archived_at, reason) = mgr.archive_state(moved).await.unwrap().unwrap();
        assert_eq!(reason, ARCHIVE_REASON_USER);
        assert_eq!(archived_at, "2026-02-01T00:00:00Z", "original archive time kept");
        assert_eq!(mgr.user_cleared_at(moved).await.unwrap().as_deref(), Some("2026-02-01T00:00:00Z"));
        assert_eq!(mgr.get_messages(moved).await.unwrap().len(), 1, "messages moved with it");
        // A live row is untouched.
        assert!(mgr.archive_state("line:Ulive").await.unwrap().is_none());
        assert!(mgr.user_cleared_at("line:Ulive").await.unwrap().is_none());

        // Hidden from team history (the reader's own SQL filter).
        let conn = mgr.acquire().await;
        let visible: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE user_cleared_at IS NULL
                 AND NOT (archived_at IS NOT NULL AND archive_reason IS 'user')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(visible, 1, "only the live row is team-history eligible");
        drop(conn);

        // The next message on the original id is a genuinely fresh session.
        mgr.get_or_create("line:Uold", "sales").await.unwrap();
        mgr.append_message("line:Uold", "user", "new topic", 1).await.unwrap();
        let context = mgr.get_messages("line:Uold").await.unwrap();
        assert_eq!(context.len(), 1);
        assert!(context.iter().all(|m| !m.content.contains("LEGACY-CLEARED-SECRET")));
        assert!(mgr.archive_state(moved).await.unwrap().is_some(), "the clear stays archived");

        // One-time: rows archived later with a NULL reason are not relabelled.
        mgr.acquire()
            .await
            .execute("UPDATE sessions SET archived_at='2026-03-01T00:00:00Z' WHERE id='line:Ulive'", [])
            .unwrap();
        drop(mgr);
        let mgr = SessionManager::new(&db).unwrap();
        assert_eq!(
            mgr.archive_state("line:Ulive").await.unwrap().unwrap().1,
            ARCHIVE_REASON_RETENTION
        );
        assert!(mgr.session_agent("line:Ulive").await.unwrap().is_some(), "not moved");
    }

    /// N2: a restore keeps the sticky `user_cleared_at`.
    #[tokio::test]
    async fn rm173_restore_keeps_the_user_cleared_mark() {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();
        mgr.get_or_create("line:U1", "agent").await.unwrap();
        mgr.append_message("line:U1", "user", "first", 1).await.unwrap();
        let moved = mgr.clear_for_user("line:U1").await.unwrap().unwrap();
        let mark = mgr.user_cleared_at(&moved).await.unwrap();
        assert!(mark.is_some());
        assert!(mgr.restore_archived(&moved).await.unwrap().is_some());
        assert!(mgr.archive_state(&moved).await.unwrap().is_none(), "live again");
        assert_eq!(mgr.user_cleared_at(&moved).await.unwrap(), mark, "sticky");
    }

    /// Race: a reply finishing after `/new` is dropped — it neither hits the
    /// moved row's foreign key nor lands in (or recreates) the fresh row.
    #[tokio::test]
    async fn rm173_reply_after_clear_is_dropped_not_written_into_the_fresh_conversation() {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();
        let id = "telegram:42";
        mgr.get_or_create(id, "agent").await.unwrap();
        let generation = mgr.turn_generation(id).await.unwrap().unwrap();
        mgr.append_delivery_message_in(id, "user", "question", 1, None, false, Some(&generation))
            .await
            .unwrap();
        // `/new` while the agent is still answering.
        let moved = mgr.clear_for_user(id).await.unwrap().unwrap();
        let late = mgr
            .append_delivery_message_in(id, "assistant", "LATE-REPLY", 1, None, false, Some(&generation))
            .await
            .unwrap_err();
        assert!(is_cleared_during_turn(&late));
        assert!(mgr.session_agent(id).await.unwrap().is_none(), "not recreated");
        // The user already wrote again: a fresh row exists at the same id.
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        mgr.get_or_create(id, "agent").await.unwrap();
        let late = mgr
            .append_delivery_message_in(id, "assistant", "LATE-REPLY", 1, None, false, Some(&generation))
            .await
            .unwrap_err();
        assert!(is_cleared_during_turn(&late));
        assert!(mgr.get_messages(id).await.unwrap().is_empty(), "fresh conversation untouched");
        assert!(mgr.get_messages(&moved).await.unwrap().iter().all(|m| m.content != "LATE-REPLY"));
        // The fresh turn's own generation writes normally.
        let fresh = mgr.turn_generation(id).await.unwrap().unwrap();
        assert_ne!(fresh, generation);
        mgr.append_delivery_message_in(id, "assistant", "ok", 1, None, false, Some(&fresh))
            .await
            .unwrap();
        assert_eq!(mgr.get_messages(id).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn rm173_archived_listing_and_restore_are_exact() {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();
        aged(&mgr, "live", 1).await;
        aged(&mgr, "gone", 1).await;
        retention_archive(&mgr, "gone").await;
        let archived = mgr.list_sessions_in(Some("agent"), 10, true).await.unwrap();
        assert_eq!(archived.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["gone"]);
        assert_eq!(archived[0].archive_reason.as_deref(), Some(ARCHIVE_REASON_RETENTION));
        assert!(mgr.restore_archived("gone").await.unwrap().is_some());
        assert!(mgr.archive_state("gone").await.unwrap().is_none(), "reason cleared too");
        assert!(mgr.restore_archived("gone").await.unwrap().is_none(), "idempotent");
        assert!(mgr.restore_archived("missing").await.unwrap().is_none());
        assert!(mgr.list_sessions_in(Some("agent"), 10, true).await.unwrap().is_empty());
        assert_eq!(mgr.list_sessions(Some("agent"), 10).await.unwrap().len(), 2);
    }

    // ── M1: user clears labelled but never moved ("stuck") ──────────

    /// A user clear left at its ORIGINAL id: exactly what migration step 1
    /// commits before a crash (or a failed move) leaves behind.
    async fn stuck_clear(mgr: &SessionManager, id: &str) {
        mgr.get_or_create(id, "agent").await.unwrap();
        mgr.append_message(id, "user", "OLD-CLEARED-SECRET", 1).await.unwrap();
        let active = (chrono::Utc::now() - chrono::Duration::hours(2)).to_rfc3339();
        let cleared = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
        mgr.acquire()
            .await
            .execute(
                "UPDATE sessions SET last_active=?2, archived_at=?3, archive_reason='user',
                        user_cleared_at=?3 WHERE id=?1",
                params![id, active, cleared],
            )
            .unwrap();
    }

    async fn marker_written(mgr: &SessionManager) -> bool {
        mgr.acquire()
            .await
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM session_migrations WHERE name=?1)",
                params![MIGRATION_LEGACY_USER_CLEARS],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// The one moved row of a contact (asserts it is a `#cleared:` user clear).
    async fn moved_clear(mgr: &SessionManager, id: &str) -> String {
        assert!(mgr.session_agent(id).await.unwrap().is_none(), "no row left at {id}");
        let ids = mgr.sessions_for_contact(id).await.unwrap();
        assert_eq!(ids.len(), 1, "{ids:?}");
        assert!(ids[0].starts_with(&format!("{id}{CLEARED_ID_MARKER}")), "{}", ids[0]);
        assert_eq!(mgr.archive_state(&ids[0]).await.unwrap().unwrap().1, ARCHIVE_REASON_USER);
        ids[0].clone()
    }

    #[tokio::test]
    async fn rm173_labelled_but_unmoved_clears_are_moved_at_the_next_start() {
        assert!(STUCK_USER_CLEAR.contains(&format!("'{CLEARED_ID_MARKER}'")));
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("sessions.db");
        let mgr = SessionManager::new(&db).unwrap();
        // Crash after step 1: labels committed, no moves, no marker.
        stuck_clear(&mgr, "line:Ucrash").await;
        mgr.acquire().await.execute("DELETE FROM session_migrations", []).unwrap();
        drop(mgr);
        let mgr = SessionManager::new(&db).unwrap();
        let moved = moved_clear(&mgr, "line:Ucrash").await;
        let messages = mgr.get_messages(&moved).await.unwrap();
        assert_eq!(messages.len(), 1, "messages moved with it");
        assert!(marker_written(&mgr).await);

        // Marker already written (an older build wrote it despite a failed
        // move): step 2 still runs from the DB at the next start.
        stuck_clear(&mgr, "line:Ulegacy").await;
        drop(mgr);
        let mgr = SessionManager::new(&db).unwrap();
        let moved = moved_clear(&mgr, "line:Ulegacy").await;
        let (archived_at, _) = mgr.archive_state(&moved).await.unwrap().unwrap();
        assert_eq!(mgr.user_cleared_at(&moved).await.unwrap(), Some(archived_at), "clear time kept");
    }

    #[tokio::test]
    async fn rm173_a_failed_move_leaves_the_marker_unwritten_and_is_retried() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("sessions.db");
        let mgr = SessionManager::new(&db).unwrap();
        stuck_clear(&mgr, "line:Ufail").await;
        stuck_clear(&mgr, "line:Uok").await;
        {
            let conn = mgr.acquire().await;
            conn.execute("DELETE FROM session_migrations", []).unwrap();
            conn.execute_batch(
                "CREATE TRIGGER fail_move BEFORE UPDATE OF id ON sessions
                 WHEN OLD.id = 'line:Ufail'
                 BEGIN SELECT RAISE(ABORT, 'injected move failure'); END;",
            )
            .unwrap();
        }
        drop(mgr);
        let mgr = SessionManager::new(&db).expect("a failed move never fails the start");
        assert!(!marker_written(&mgr).await, "marker only once nothing is left");
        assert!(mgr.session_agent("line:Ufail").await.unwrap().is_some(), "rolled back alone");
        moved_clear(&mgr, "line:Uok").await;

        mgr.acquire().await.execute_batch("DROP TRIGGER fail_move;").unwrap();
        drop(mgr);
        let mgr = SessionManager::new(&db).unwrap();
        let moved = moved_clear(&mgr, "line:Ufail").await;
        assert_eq!(mgr.get_messages(&moved).await.unwrap().len(), 1);
        assert!(marker_written(&mgr).await);
    }

    #[tokio::test]
    async fn rm173_new_on_a_stuck_clear_repairs_it() {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();
        stuck_clear(&mgr, "telegram:7").await;
        let moved = mgr
            .clear_for_user("telegram:7")
            .await
            .unwrap()
            .expect("a user clear at its original id is not already cleared");
        assert_eq!(moved_clear(&mgr, "telegram:7").await, moved);
        assert_eq!(mgr.get_messages(&moved).await.unwrap().len(), 1);
        // Now genuinely cleared: a second `/new` is a no-op.
        assert!(mgr.clear_for_user(&moved).await.unwrap().is_none());
        assert!(mgr.clear_for_user("telegram:7").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn rm173_a_message_on_a_stuck_clear_starts_fresh_without_old_context() {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();

        // A write with no turn start is refused, and the row is moved away.
        stuck_clear(&mgr, "line:Ua").await;
        let err = mgr.append_message("line:Ua", "assistant", "LATE", 1).await.unwrap_err();
        assert!(is_cleared_during_turn(&err));
        let moved = moved_clear(&mgr, "line:Ua").await;
        assert!(mgr.get_messages(&moved).await.unwrap().iter().all(|m| m.content != "LATE"));

        // A turn start repairs it and the turn runs in a fresh conversation.
        stuck_clear(&mgr, "line:Ub").await;
        let session = mgr.get_or_create("line:Ub", "agent").await.unwrap();
        assert_eq!(
            mgr.turn_generation("line:Ub").await.unwrap().as_deref(),
            Some(session.created_at.as_str()),
            "the generation comes back with the row it names"
        );
        mgr.append_delivery_message_in("line:Ub", "user", "new topic", 1, None, false, Some(&session.created_at))
            .await
            .unwrap();
        let context = mgr.get_messages("line:Ub").await.unwrap();
        assert_eq!(context.len(), 1);
        assert!(context.iter().all(|m| !m.content.contains("OLD-CLEARED-SECRET")));
        assert!(mgr.archive_state("line:Ub").await.unwrap().is_none(), "live, fresh row");
        let ids = mgr.sessions_for_contact("line:Ub").await.unwrap();
        assert_eq!(ids.len(), 2, "fresh row + the moved clear: {ids:?}");
    }

    #[tokio::test]
    async fn rm173_purge_skips_a_row_active_after_its_archive() {
        let tmp = NamedTempFile::new().unwrap();
        let mgr = SessionManager::new(tmp.path()).unwrap();
        aged(&mgr, "in-use", 900).await;
        aged(&mgr, "old", 900).await;
        archived_days_ago(&mgr, "in-use", 800).await;
        archived_days_ago(&mgr, "old", 800).await;
        // Activity after the archive mark (a stale mark on a conversation in use).
        mgr.acquire()
            .await
            .execute(
                "UPDATE sessions SET last_active=?2 WHERE id=?1",
                params!["in-use", (chrono::Utc::now() - chrono::Duration::days(1)).to_rfc3339()],
            )
            .unwrap();
        let purge = Retention { archive_after_days: 30, purge_after_days: 365 };
        assert_eq!(mgr.apply_retention(purge).await.unwrap().purged, 1);
        assert!(mgr.session_agent("old").await.unwrap().is_none());
        assert!(mgr.session_agent("in-use").await.unwrap().is_some(), "active: never purged");
        assert_eq!(mgr.get_messages("in-use").await.unwrap().len(), 1);
    }
}

#[cfg(test)]
mod credential_summary_tests {
    use super::*;

    // Security regression; not run (owner forbids builds).
    #[tokio::test]
    async fn pinned_and_title_sinks_mask_before_truncation() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let manager = SessionManager::new(file.path()).unwrap();
        manager.get_or_create("test:s1", "test").await.unwrap();
        manager.set_pinned("test:s1", "password: fixture-pinned").await.unwrap();
        assert!(!manager.get_pinned("test:s1").await.unwrap().contains("fixture-pinned"));
        let title = format!("{} sk-{}", "a".repeat(SESSION_TITLE_MAX_CHARS - 5), "Z".repeat(48));
        manager.set_title("test:s1", &title, 1).await.unwrap();
        let (title, _) = manager.get_title("test:s1").await.unwrap();
        assert!(!title.contains("sk-"));
    }


    #[tokio::test]
    async fn summary_and_compression_mask_contextual_credentials() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let manager = SessionManager::new(file.path()).unwrap();
        manager.get_or_create("test:s1", "test").await.unwrap();
        manager.set_summary("test:s1", "密碼：fixture-value", 1).await.unwrap();
        let (summary, _) = manager.get_summary("test:s1").await.unwrap();
        assert!(!summary.contains("fixture-value"));
        manager.compress("test:s1", "password: fixture-value").await.unwrap();
        let messages = manager.get_messages("test:s1").await.unwrap();
        assert!(messages.iter().all(|m| !m.content.contains("fixture-value")));
    }
}
