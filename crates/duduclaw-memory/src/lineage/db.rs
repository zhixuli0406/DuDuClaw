//! SQL side of source lineage: schema, the one tombstone-match expression,
//! resolving a [`Provenance`] into lineage rows, and inserting them.
//!
//! Every function here runs on a connection the caller has already locked
//! and, for writes, already put inside `BEGIN IMMEDIATE`.

use std::collections::BTreeMap;

use duduclaw_core::error::{DuDuClawError, Result};
use rusqlite::{Connection, OptionalExtension, params};

use super::{
    FenceReason, FenceRefusal, MAX_LINEAGE_SOURCES, Provenance, SYSTEM_SESSION_PREFIX,
    UNTRACKED_PARENT_KIND, UNTRACKED_SESSION, format_ts, source_digest,
};

fn mem_err(e: impl std::fmt::Display) -> DuDuClawError {
    DuDuClawError::Memory(e.to_string())
}

/// `memory_store` value for rows of `memories`.
pub(crate) const STORE_MEMORIES: &str = "memories";
/// `memory_store` value for rows of `key_facts`.
pub(crate) const STORE_KEY_FACTS: &str = "key_facts";

/// The single tombstone-match expression. `fs` names the tombstone table
/// (`forgotten_sources`, an attached copy, or the temp candidate table used by
/// plan); the other arguments are SQL expressions for the source being judged
/// (`?n` placeholders in Rust, `NEW.col` in a trigger, `o.col` in a join).
///
/// Message scope matches one message exactly. `session_upto` matches every
/// source of that session up to the watermark: by `session_messages.id` when
/// both sides have one, otherwise by the formatted observation time
/// ([`format_ts`] on both sides, so string order is time order).
pub(crate) fn tombstone_match_sql(
    fs: &str,
    agent: &str,
    session: &str,
    message: &str,
    seq: &str,
    observed: &str,
) -> String {
    // Two EXISTS, one per scope, so the message-scope lookup can use the
    // `(agent_id, source_session, scope, source_message)` index even when a
    // session holds thousands of message tombstones (one per forgotten turn).
    format!(
        "(EXISTS (SELECT 1 FROM {fs} f
                  WHERE f.agent_id = {agent} AND f.source_session = {session}
                    AND f.scope = 'message' AND f.source_message = {message})
          OR EXISTS (SELECT 1 FROM {fs} f
                  WHERE f.agent_id = {agent} AND f.source_session = {session}
                    AND f.scope = 'session_upto' AND (
                          ({seq} IS NOT NULL AND f.upto_seq IS NOT NULL AND {seq} <= f.upto_seq
                           AND (f.upto_time IS NULL OR {observed} <= f.upto_time))
                       OR (({seq} IS NULL OR f.upto_seq IS NULL)
                           AND f.upto_time IS NOT NULL AND {observed} <= f.upto_time))))"
    )
}

/// Create the lineage tables, indexes and triggers (idempotent), and record
/// the database instance id on first run. Called from `init_tables`.
pub(crate) fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS memory_origins (
            memory_store       TEXT NOT NULL CHECK (memory_store IN ('memories','key_facts')),
            memory_id          TEXT NOT NULL,
            agent_id           TEXT NOT NULL,
            source_kind        TEXT NOT NULL,
            source_session     TEXT NOT NULL,
            source_message     TEXT NOT NULL DEFAULT '',
            source_seq         INTEGER,
            source_observed_at TEXT NOT NULL,
            source_hash        TEXT,
            role               TEXT NOT NULL CHECK (role IN ('direct','inherited','reaffirm')),
            via_memory_id      TEXT,
            created_at         TEXT NOT NULL,
            PRIMARY KEY (memory_store, memory_id, source_session, source_message)
        );
        CREATE INDEX IF NOT EXISTS idx_origins_source
            ON memory_origins(agent_id, source_session, source_message);
        CREATE INDEX IF NOT EXISTS idx_origins_seq
            ON memory_origins(agent_id, source_session, source_seq);
        CREATE INDEX IF NOT EXISTS idx_origins_memory
            ON memory_origins(memory_id);

        CREATE TABLE IF NOT EXISTS forgotten_sources (
            tombstone_id   TEXT PRIMARY KEY,
            agent_id       TEXT NOT NULL,
            source_session TEXT NOT NULL,
            scope          TEXT NOT NULL CHECK (scope IN ('message','session_upto')),
            source_message TEXT NOT NULL DEFAULT '',
            upto_seq       INTEGER,
            upto_time      TEXT,
            source_digest  TEXT NOT NULL,
            plan_id        TEXT NOT NULL,
            forgotten_at   TEXT NOT NULL
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_forgotten_unique
            ON forgotten_sources(agent_id, source_session, scope, source_message,
                                 COALESCE(upto_seq, -1), COALESCE(upto_time, ''));
        CREATE INDEX IF NOT EXISTS idx_forgotten_lookup
            ON forgotten_sources(agent_id, source_session);

        CREATE TABLE IF NOT EXISTS forgotten_memories (
            memory_store TEXT NOT NULL,
            memory_id    TEXT NOT NULL,
            agent_id     TEXT NOT NULL,
            plan_id      TEXT NOT NULL,
            forgotten_at TEXT NOT NULL,
            PRIMARY KEY (memory_store, memory_id)
        );

        CREATE TABLE IF NOT EXISTS memory_fence (
            agent_id     TEXT PRIMARY KEY,
            forget_epoch INTEGER NOT NULL DEFAULT 0,
            updated_at   TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS memory_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);

        -- Which conversation turn a user message started, recorded when one
        -- write carried both as direct sources (P2-B F1). A conversation-level
        -- fact, kept across namespaces and forgets: an employee dispatched
        -- from that turn records only the turn, and a forget of the message
        -- in its namespace reads the link from here.
        CREATE TABLE IF NOT EXISTS turn_message_links (
            source_session TEXT NOT NULL,
            user_message   TEXT NOT NULL,
            turn_message   TEXT NOT NULL,
            PRIMARY KEY (source_session, user_message, turn_message)
        );

        CREATE TABLE IF NOT EXISTS memory_forget_plans (
            plan_id        TEXT PRIMARY KEY,
            agent_id       TEXT NOT NULL,
            plan_hash      TEXT NOT NULL,
            plan_json      TEXT NOT NULL,
            forget_epoch   INTEGER NOT NULL,
            db_instance_id TEXT NOT NULL,
            created_at     TEXT NOT NULL,
            expires_at     TEXT NOT NULL,
            status         TEXT NOT NULL CHECK (status IN ('planned','applied','stale','expired')),
            applied_at     TEXT
        );
        CREATE TABLE IF NOT EXISTS memory_forget_steps (
            plan_id    TEXT NOT NULL,
            step       TEXT NOT NULL,
            target     TEXT NOT NULL,
            status     TEXT NOT NULL CHECK (status IN ('pending','done','failed')),
            attempts   INTEGER NOT NULL DEFAULT 0,
            last_error TEXT,
            updated_at TEXT NOT NULL,
            PRIMARY KEY (plan_id, step, target)
        );

        CREATE TRIGGER IF NOT EXISTS trg_memories_forgotten_id BEFORE INSERT ON memories
        WHEN EXISTS (SELECT 1 FROM forgotten_memories
                     WHERE memory_store = 'memories' AND memory_id = NEW.id)
        BEGIN SELECT RAISE(ABORT, 'duduclaw:memory_forgotten'); END;

        CREATE TRIGGER IF NOT EXISTS trg_key_facts_forgotten_id BEFORE INSERT ON key_facts
        WHEN EXISTS (SELECT 1 FROM forgotten_memories
                     WHERE memory_store = 'key_facts' AND memory_id = NEW.id)
        BEGIN SELECT RAISE(ABORT, 'duduclaw:memory_forgotten'); END;",
    )
    .map_err(|e| DuDuClawError::Memory(format!("lineage schema: {e}")))?;

    let origins_trigger = format!(
        "CREATE TRIGGER IF NOT EXISTS trg_origins_fence BEFORE INSERT ON memory_origins
         WHEN {}
         BEGIN SELECT RAISE(ABORT, 'duduclaw:source_forgotten'); END;",
        tombstone_match_sql(
            "forgotten_sources",
            "NEW.agent_id",
            "NEW.source_session",
            "NEW.source_message",
            "NEW.source_seq",
            "NEW.source_observed_at",
        )
    );
    conn.execute_batch(&origins_trigger)
        .map_err(|e| DuDuClawError::Memory(format!("lineage trigger: {e}")))?;

    let now = format_ts(chrono::Utc::now());
    super::links::backfill_turn_links(conn)?;
    conn.execute(
        "INSERT OR IGNORE INTO memory_meta (key, value) VALUES ('db_instance_id', ?1)",
        params![uuid::Uuid::new_v4().to_string()],
    )
    .map_err(mem_err)?;
    conn.execute(
        "INSERT OR IGNORE INTO memory_meta (key, value) VALUES ('lineage_schema_version', '1')",
        [],
    )
    .map_err(mem_err)?;
    conn.execute(
        "INSERT OR IGNORE INTO memory_meta (key, value) VALUES ('lineage_enabled_at', ?1)",
        params![now],
    )
    .map_err(mem_err)?;
    Ok(())
}

/// Whether table `name` exists in schema `schema` (`main`, an attached name).
pub(crate) fn table_exists(conn: &Connection, schema: &str, name: &str) -> Result<bool> {
    conn.query_row(
        &format!("SELECT 1 FROM {schema}.sqlite_master WHERE type = 'table' AND name = ?1"),
        params![name],
        |_| Ok(()),
    )
    .optional()
    .map(|o| o.is_some())
    .map_err(mem_err)
}

/// This database's instance id (written once by [`init_schema`]).
pub(crate) fn db_instance_id(conn: &Connection) -> Result<String> {
    conn.query_row(
        "SELECT value FROM memory_meta WHERE key = 'db_instance_id'",
        [],
        |r| r.get(0),
    )
    .map_err(mem_err)
}

/// The namespace's forget epoch (0 before the first apply).
pub(crate) fn forget_epoch(conn: &Connection, agent_id: &str) -> Result<i64> {
    conn.query_row(
        "SELECT forget_epoch FROM memory_fence WHERE agent_id = ?1",
        params![agent_id],
        |r| r.get::<_, i64>(0),
    )
    .optional()
    .map(|v| v.unwrap_or(0))
    .map_err(mem_err)
}

/// Lineage role, ordered weakest → strongest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Role {
    Reaffirm,
    Inherited,
    Direct,
}

impl Role {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Reaffirm => "reaffirm",
            Self::Inherited => "inherited",
            Self::Direct => "direct",
        }
    }

    pub(crate) fn parse(s: &str) -> Self {
        match s {
            "direct" => Self::Direct,
            "inherited" => Self::Inherited,
            _ => Self::Reaffirm,
        }
    }
}

/// One lineage row to be written for a memory row.
#[derive(Debug, Clone)]
pub(crate) struct OriginRow {
    pub kind: String,
    pub session: String,
    pub message: String,
    pub seq: Option<i64>,
    pub observed_at: String,
    pub hash: Option<String>,
    pub role: Role,
    pub via: Option<String>,
}

impl OriginRow {
    pub(crate) fn digest(&self, agent_id: &str) -> String {
        source_digest(agent_id, &self.session, &self.message, "")
    }
}

type Resolved = std::result::Result<Vec<OriginRow>, FenceRefusal>;

fn refusal(reason: FenceReason, digest: Option<String>, parent: Option<String>) -> FenceRefusal {
    FenceRefusal {
        reason,
        source_digest: digest,
        parent_id: parent,
    }
}

/// Resolve `prov` (plus `extra_parents`, e.g. a write's `derived_from`) into
/// the lineage rows of a new row, checking the fence: parents must exist and
/// not be forgotten, no source may match a tombstone, at most
/// [`MAX_LINEAGE_SOURCES`] rows. `Ok(Err(refusal))` = nothing may be written.
/// A malformed provenance or any SQL error is an `Err` (also: nothing written).
pub(crate) fn resolve(
    conn: &Connection,
    agent_id: &str,
    prov: &Provenance,
    extra_parents: &[String],
) -> Result<Resolved> {
    let now = format_ts(chrono::Utc::now());
    let mut rows: BTreeMap<(String, String), OriginRow> = BTreeMap::new();
    let add = |row: OriginRow, rows: &mut BTreeMap<(String, String), OriginRow>| {
        let key = (row.session.clone(), row.message.clone());
        match rows.get(&key) {
            Some(existing) if existing.role >= row.role => {}
            _ => {
                rows.insert(key, row);
            }
        }
    };

    let (direct, parents): (Vec<&super::SourceRef>, Vec<String>) = match prov {
        Provenance::Sources(v) => {
            if v.is_empty() {
                return Err(mem_err("provenance has no source"));
            }
            (v.iter().collect(), Vec::new())
        }
        Provenance::Derived { parents, extra } => (extra.iter().collect(), parents.clone()),
        Provenance::System { producer } => {
            if producer.trim().is_empty() || producer.contains(char::is_control) {
                return Err(mem_err("system provenance needs a producer name"));
            }
            add(
                OriginRow {
                    kind: "system".to_string(),
                    session: format!("{SYSTEM_SESSION_PREFIX}{producer}"),
                    message: String::new(),
                    seq: None,
                    observed_at: now.clone(),
                    hash: None,
                    role: Role::Direct,
                    via: None,
                },
                &mut rows,
            );
            (Vec::new(), Vec::new())
        }
    };
    for s in &direct {
        s.validate()
            .map_err(|e| mem_err(format!("invalid source: {e}")))?;
        add(
            OriginRow {
                kind: s.kind.as_str().to_string(),
                session: s.session.clone(),
                message: s.message.clone(),
                seq: s.seq,
                observed_at: format_ts(s.observed_at),
                hash: s.content_hash.clone(),
                role: Role::Direct,
                via: None,
            },
            &mut rows,
        );
    }

    let mut all_parents: Vec<String> = parents;
    for p in extra_parents {
        if !all_parents.contains(p) {
            all_parents.push(p.clone());
        }
    }
    if matches!(prov, Provenance::Derived { .. }) && all_parents.is_empty() && direct.is_empty() {
        return Err(mem_err(
            "derived provenance has neither parents nor sources",
        ));
    }
    let has_archive = table_exists(conn, "main", "memories_archive")?;
    for parent in &all_parents {
        if let Some(r) = parent_check(conn, agent_id, parent, has_archive)? {
            return Ok(Err(r));
        }
        let inherited: Vec<OriginRow> = parent_origins(conn, agent_id, parent)?
            .into_iter()
            .filter(|r| r.role != Role::Reaffirm)
            .collect();
        if inherited.is_empty() {
            add(
                OriginRow {
                    kind: UNTRACKED_PARENT_KIND.to_string(),
                    session: UNTRACKED_SESSION.to_string(),
                    message: parent.clone(),
                    seq: None,
                    observed_at: now.clone(),
                    hash: None,
                    role: Role::Inherited,
                    via: Some(parent.clone()),
                },
                &mut rows,
            );
        }
        for mut row in inherited {
            // A parent's corroborating (`reaffirm`) sources are not inherited
            // (M2 / M-7): they never decide a deletion (B.2), and a stable,
            // often-restated fact would otherwise push every row derived from
            // it past the source cap. Only direct and inherited lineage
            // counts towards the cap.
            row.role = Role::Inherited;
            row.via = Some(parent.clone());
            add(row, &mut rows);
        }
    }

    let rows: Vec<OriginRow> = rows.into_values().collect();
    if let Some(r) = first_forgotten(conn, agent_id, &rows)? {
        return Ok(Err(r));
    }
    if rows.len() > MAX_LINEAGE_SOURCES {
        return Ok(Err(refusal(FenceReason::LineageOverflow, None, None)));
    }
    Ok(Ok(rows))
}

fn parent_check(
    conn: &Connection,
    agent_id: &str,
    parent: &str,
    has_archive: bool,
) -> Result<Option<FenceRefusal>> {
    let forgotten = conn
        .prepare_cached(
            "SELECT 1 FROM forgotten_memories WHERE memory_store = 'memories' AND memory_id = ?1",
        )
        .and_then(|mut s| s.query_row(params![parent], |_| Ok(())).optional())
        .map_err(mem_err)?
        .is_some();
    if forgotten {
        return Ok(Some(refusal(
            FenceReason::ParentForgotten,
            None,
            Some(parent.to_string()),
        )));
    }
    let live = conn
        .prepare_cached("SELECT 1 FROM memories WHERE id = ?1 AND agent_id = ?2")
        .and_then(|mut s| {
            s.query_row(params![parent, agent_id], |_| Ok(()))
                .optional()
        })
        .map_err(mem_err)?
        .is_some();
    let archived = !live
        && has_archive
        && conn
            .prepare_cached("SELECT 1 FROM memories_archive WHERE id = ?1 AND agent_id = ?2")
            .and_then(|mut s| {
                s.query_row(params![parent, agent_id], |_| Ok(()))
                    .optional()
            })
            .map_err(mem_err)?
            .is_some();
    if !live && !archived {
        return Ok(Some(refusal(
            FenceReason::ParentMissing,
            None,
            Some(parent.to_string()),
        )));
    }
    Ok(None)
}

/// The lineage rows of one `memories` row.
pub(crate) fn parent_origins(
    conn: &Connection,
    agent_id: &str,
    memory_id: &str,
) -> Result<Vec<OriginRow>> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT source_kind, source_session, source_message, source_seq,
                    source_observed_at, source_hash, role, via_memory_id
             FROM memory_origins
             WHERE memory_store = 'memories' AND memory_id = ?1 AND agent_id = ?2",
        )
        .map_err(mem_err)?;
    let rows = stmt
        .query_map(params![memory_id, agent_id], |r| {
            Ok(OriginRow {
                kind: r.get(0)?,
                session: r.get(1)?,
                message: r.get(2)?,
                seq: r.get(3)?,
                observed_at: r.get(4)?,
                hash: r.get(5)?,
                role: Role::parse(&r.get::<_, String>(6)?),
                via: r.get(7)?,
            })
        })
        .map_err(mem_err)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(mem_err)
}

/// The first row of `rows` that matches a tombstone of `agent_id`, as a
/// refusal carrying only its digest.
pub(crate) fn first_forgotten(
    conn: &Connection,
    agent_id: &str,
    rows: &[OriginRow],
) -> Result<Option<FenceRefusal>> {
    let sql = format!(
        "SELECT {}",
        tombstone_match_sql("forgotten_sources", "?1", "?2", "?3", "?4", "?5")
    );
    let mut stmt = conn.prepare_cached(&sql).map_err(mem_err)?;
    for row in rows {
        let hit: bool = stmt
            .query_row(
                params![agent_id, row.session, row.message, row.seq, row.observed_at],
                |r| r.get(0),
            )
            .map_err(mem_err)?;
        if hit {
            return Ok(Some(refusal(
                FenceReason::SourceForgotten,
                Some(row.digest(agent_id)),
                None,
            )));
        }
    }
    Ok(None)
}

/// Insert `rows` as the lineage of `(store, memory_id)`. On a key conflict the
/// stronger role is kept (direct > inherited > reaffirm). The insert trigger
/// re-checks every row against the tombstones.
pub(crate) fn insert_origins(
    conn: &Connection,
    store: &str,
    memory_id: &str,
    agent_id: &str,
    rows: &[OriginRow],
) -> Result<()> {
    let now = format_ts(chrono::Utc::now());
    let mut stmt = conn
        .prepare_cached(
            "INSERT INTO memory_origins
                (memory_store, memory_id, agent_id, source_kind, source_session,
                 source_message, source_seq, source_observed_at, source_hash, role,
                 via_memory_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(memory_store, memory_id, source_session, source_message) DO UPDATE SET
                role = CASE
                    WHEN (CASE excluded.role WHEN 'direct' THEN 3 WHEN 'inherited' THEN 2 ELSE 1 END)
                       > (CASE memory_origins.role WHEN 'direct' THEN 3 WHEN 'inherited' THEN 2 ELSE 1 END)
                    THEN excluded.role ELSE memory_origins.role END",
        )
        .map_err(mem_err)?;
    for row in rows {
        stmt.execute(params![
            store,
            memory_id,
            agent_id,
            row.kind,
            row.session,
            row.message,
            row.seq,
            row.observed_at,
            row.hash,
            row.role.as_str(),
            row.via,
            now,
        ])
        .map_err(|e| DuDuClawError::Memory(format!("lineage insert: {e}")))?;
    }
    super::links::record_turn_links(conn, rows)
}

/// Number of lineage rows `(store, memory_id)` would have after adding `rows`
/// (keys already present are not counted twice).
pub(crate) fn count_after_merge(
    conn: &Connection,
    store: &str,
    memory_id: &str,
    rows: &[OriginRow],
) -> Result<usize> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT source_session, source_message FROM memory_origins
             WHERE memory_store = ?1 AND memory_id = ?2",
        )
        .map_err(mem_err)?;
    let mut keys: std::collections::HashSet<(String, String)> = stmt
        .query_map(params![store, memory_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(mem_err)?
        .collect::<std::result::Result<_, _>>()
        .map_err(mem_err)?;
    for r in rows {
        keys.insert((r.session.clone(), r.message.clone()));
    }
    Ok(keys.len())
}

/// Record the lineage of a reaffirming write on the surviving row: every
/// source of the write becomes a `reaffirm` row (an existing stronger role on
/// the same source is kept).
///
/// A forgotten source still refuses the reaffirmation. Past
/// [`MAX_LINEAGE_SOURCES`] the reaffirmation itself still happens (a stable
/// fact is corroborated by many conversations and must not start refusing
/// writes): the new `reaffirm` rows are simply not recorded and `skipped` is
/// incremented. Only `direct` / `inherited` lineage is capped as a refusal.
pub(crate) fn add_reaffirm(
    conn: &Connection,
    survivor_id: &str,
    agent_id: &str,
    rows: &[OriginRow],
    skipped: &std::sync::atomic::AtomicU64,
) -> Result<Option<FenceRefusal>> {
    // The write's own (direct) sources still link its turn to its message,
    // even though they are recorded on the survivor as `reaffirm` (F6).
    super::links::record_turn_links(conn, rows)?;
    let as_reaffirm: Vec<OriginRow> = rows
        .iter()
        .cloned()
        .map(|mut r| {
            r.role = Role::Reaffirm;
            r
        })
        .collect();
    if count_after_merge(conn, STORE_MEMORIES, survivor_id, &as_reaffirm)? > MAX_LINEAGE_SOURCES {
        // Not inserted, so the trigger will not see them: check the fence here.
        if let Some(r) = first_forgotten(conn, agent_id, &as_reaffirm)? {
            return Ok(Some(r));
        }
        skipped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return Ok(None);
    }
    insert_origins(conn, STORE_MEMORIES, survivor_id, agent_id, &as_reaffirm)?;
    Ok(None)
}

/// Copy the lineage of `from_id` onto `to_id` as `reaffirm` rows (a row that
/// is closed out as a duplicate of a surviving row corroborates it).
pub(crate) fn copy_as_reaffirm(
    conn: &Connection,
    from_agent: &str,
    from_id: &str,
    to_agent: &str,
    to_id: &str,
    skipped: &std::sync::atomic::AtomicU64,
) -> Result<Option<FenceRefusal>> {
    let rows = parent_origins(conn, from_agent, from_id)?;
    if rows.is_empty() {
        return Ok(None);
    }
    add_reaffirm(conn, to_id, to_agent, &rows, skipped)
}

/// Re-key the lineage of one row to another namespace.
pub(crate) fn rekey_origins(
    conn: &Connection,
    store: &str,
    memory_id: &str,
    from: &str,
    to: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE memory_origins SET agent_id = ?1
         WHERE memory_store = ?2 AND memory_id = ?3 AND agent_id = ?4",
        params![to, store, memory_id, from],
    )
    .map_err(mem_err)?;
    Ok(())
}

/// Whether any lineage row of `(store, memory_id)` (namespace `lineage_agent`)
/// matches a tombstone of namespace `fence_agent` in table `fs`.
pub(crate) fn lineage_hits_tombstone(
    conn: &Connection,
    fs: &str,
    origins: &str,
    store: &str,
    memory_id: &str,
    lineage_agent: &str,
    fence_agent: &str,
) -> Result<bool> {
    let sql = format!(
        "SELECT EXISTS (SELECT 1 FROM {origins} o
                        WHERE o.memory_store = ?1 AND o.memory_id = ?2 AND o.agent_id = ?3
                          AND {})",
        tombstone_match_sql(
            fs,
            "?4",
            "o.source_session",
            "o.source_message",
            "o.source_seq",
            "o.source_observed_at"
        )
    );
    conn.query_row(
        &sql,
        params![store, memory_id, lineage_agent, fence_agent],
        |r| r.get(0),
    )
    .map_err(mem_err)
}

/// Copy namespace `from`'s source tombstones to namespace `to` (in table
/// `dst_fs`, which may live in an attached database). Existing identical
/// tombstones are kept. Returns the number of new tombstones.
pub(crate) fn copy_tombstones(
    conn: &Connection,
    src_fs: &str,
    dst_fs: &str,
    from: &str,
    to: &str,
) -> Result<usize> {
    type OriginRow = (String, String, String, Option<i64>, Option<String>, String);
    let rows: Vec<OriginRow> = {
        let mut stmt = conn
            .prepare(&format!(
                "SELECT source_session, scope, source_message, upto_seq, upto_time, plan_id
                 FROM {src_fs} WHERE agent_id = ?1"
            ))
            .map_err(mem_err)?;
        let it = stmt
            .query_map(params![from], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })
            .map_err(mem_err)?;
        it.collect::<std::result::Result<_, _>>().map_err(mem_err)?
    };
    let now = format_ts(chrono::Utc::now());
    let mut n = 0;
    for (session, scope, message, upto_seq, upto_time, plan_id) in rows {
        let upto = tombstone_upto_label(upto_seq, upto_time.as_deref());
        let digest = source_digest(to, &session, &message, &upto);
        n += conn
            .execute(
                &format!(
                    "INSERT OR IGNORE INTO {dst_fs}
                        (tombstone_id, agent_id, source_session, scope, source_message,
                         upto_seq, upto_time, source_digest, plan_id, forgotten_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"
                ),
                params![
                    uuid::Uuid::new_v4().to_string(),
                    to,
                    session,
                    scope,
                    message,
                    upto_seq,
                    upto_time,
                    digest,
                    plan_id,
                    now
                ],
            )
            .map_err(mem_err)?;
    }
    Ok(n)
}

/// The `upto` part of a session watermark's digest.
pub(crate) fn tombstone_upto_label(upto_seq: Option<i64>, upto_time: Option<&str>) -> String {
    match (upto_seq, upto_time) {
        (None, None) => String::new(),
        (s, t) => format!(
            "seq:{}|time:{}",
            s.map(|v| v.to_string()).unwrap_or_default(),
            t.unwrap_or("")
        ),
    }
}
