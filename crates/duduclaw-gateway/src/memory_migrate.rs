//! Boot-time self-heal for split memory databases.
//!
//! Background (2026-08-20 「關鍵洞察空白」incident): the live write path
//! points every engine at the shared `<home>/memory.db` (`server.rs`
//! `.with_memory_db`), while the read RPCs
//! (`handlers.rs::agent_memory_db_path`, skill-synthesis episodic evidence)
//! prefer a per-agent `agents/<id>/state/memory.db` when one exists — an
//! order that assumed per-agent files only exist on legacy installs. The
//! prediction-episodic writer in `channel_reply.rs` violated that assumption
//! by creating per-agent files on modern installs, so a single stray write
//! permanently flipped every dashboard memory read for that agent onto a
//! near-empty database while key facts and learned rules kept accumulating,
//! unseen, in the shared file.
//!
//! [`merge_per_agent_memory_dbs`] restores the invariant at every boot:
//! each `agents/<id>/state/memory.db` / `agents/<id>/memory.db` found is
//! merged row-by-row into the shared database — id-keyed, `INSERT`-only,
//! never overwriting an existing shared row — and the source file is then
//! renamed to `memory.db.merged-<timestamp>` so the next boot is a no-op.
//! Rows are copied **verbatim** (raw SQL, not re-stored through the engine)
//! on purpose: replaying them through `store_temporal` would re-run
//! supersession/conflict resolution and could retire newer shared-db facts
//! with stale per-agent ones.
//!
//! Failure policy: any error on one source file leaves that file untouched
//! (reads keep their previous, degraded-but-lossless behavior) and is
//! reported in the outcome; the gateway boot never aborts because of this
//! migration.

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use tracing::{info, warn};

/// What one boot-time merge pass did. `errors` are per-source-file and
/// non-fatal by design.
#[derive(Debug, Default)]
pub struct MergeOutcome {
    /// Source files successfully merged and archived.
    pub merged_files: usize,
    /// `memories` rows copied into the shared db across all sources.
    pub memories_rows: usize,
    /// `key_facts` rows copied into the shared db across all sources.
    pub key_facts_rows: usize,
    /// One entry per source file that failed (file is left in place).
    pub errors: Vec<String>,
}

/// Merge every per-agent `memory.db` under `<home>/agents/` into the shared
/// `<home>/memory.db`, then archive the merged source files. Idempotent:
/// archived files are renamed away, so a second pass finds nothing.
pub fn merge_per_agent_memory_dbs(home_dir: &Path) -> MergeOutcome {
    let mut outcome = MergeOutcome::default();
    let agents_dir = home_dir.join("agents");
    let Ok(entries) = std::fs::read_dir(&agents_dir) else {
        return outcome; // no agents dir — nothing to heal
    };
    let shared = home_dir.join("memory.db");

    for entry in entries.flatten() {
        let agent_dir = entry.path();
        if !agent_dir.is_dir() {
            continue;
        }
        for candidate in [
            agent_dir.join("state").join("memory.db"),
            agent_dir.join("memory.db"),
        ] {
            if !candidate.exists() {
                continue;
            }
            match merge_one(&candidate, &shared) {
                Ok((mem_rows, fact_rows)) => {
                    info!(
                        source = %candidate.display(),
                        memories = mem_rows,
                        key_facts = fact_rows,
                        "per-agent memory.db merged into shared db and archived"
                    );
                    outcome.merged_files += 1;
                    outcome.memories_rows += mem_rows;
                    outcome.key_facts_rows += fact_rows;
                }
                Err(e) => {
                    warn!(
                        source = %candidate.display(),
                        error = %e,
                        "per-agent memory.db merge failed — file left in place, \
                         reads keep resolving to it (degraded but lossless)"
                    );
                    outcome.errors.push(format!("{}: {e}", candidate.display()));
                }
            }
        }
    }
    outcome
}

/// Merge one source `memory.db` into `shared`, then archive the source.
/// Returns `(memories_rows, key_facts_rows)` copied.
fn merge_one(source: &Path, shared: &Path) -> Result<(usize, usize), String> {
    // Bring BOTH schemas up to the current version first (the engine's
    // constructor runs the idempotent column migrations), so the column
    // intersection below is total and the copy loses nothing.
    duduclaw_memory::SqliteMemoryEngine::new(source)
        .map_err(|e| format!("open source schema: {e}"))?;
    duduclaw_memory::SqliteMemoryEngine::new(shared)
        .map_err(|e| format!("open shared schema: {e}"))?;

    let mut conn = Connection::open(shared).map_err(|e| format!("open shared: {e}"))?;
    conn.execute_batch("PRAGMA busy_timeout=5000;")
        .map_err(|e| format!("busy_timeout: {e}"))?;
    let src_str = source
        .to_str()
        .ok_or_else(|| "source path is not valid UTF-8".to_string())?;
    conn.execute("ATTACH DATABASE ?1 AS src", [src_str])
        .map_err(|e| format!("attach source: {e}"))?;

    let copy_result = copy_all_tables(&mut conn);

    // DETACH regardless of copy success so the source file has no open
    // handle left when we rename it (or leave it for the next boot).
    let _ = conn.execute_batch("DETACH DATABASE src");
    drop(conn);

    let (mem_rows, fact_rows) = copy_result?;
    archive_source(source)?;
    Ok((mem_rows, fact_rows))
}

/// Copy every engine-owned table from `src` into `main` inside one
/// transaction. Row identity is the table's primary key; existing shared
/// rows always win.
fn copy_all_tables(conn: &mut Connection) -> Result<(usize, usize), String> {
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| format!("begin: {e}"))?;

    // ── memories + memories_fts ──────────────────────────────────────────
    // FTS rows must be inserted per copied id: `memories_fts` is a
    // standalone FTS5 table the engine populates alongside each insert —
    // a bare table-to-table copy would leave the copied rows unsearchable.
    let new_memory_ids = pending_ids(&tx, "memories")?;
    let mem_rows = if new_memory_ids.is_empty() {
        0
    } else {
        let cols = shared_columns(&tx, "memories")?;
        insert_missing(&tx, "memories", &cols)?;
        for id in &new_memory_ids {
            tx.execute(
                "INSERT INTO memories_fts (content, agent_id, memory_id)
                 SELECT content, agent_id, id FROM main.memories WHERE id = ?1",
                [id],
            )
            .map_err(|e| format!("memories_fts backfill: {e}"))?;
        }
        new_memory_ids.len()
    };

    // ── key_facts + key_facts_fts ────────────────────────────────────────
    // `key_facts_fts` is rowid-coupled to `key_facts` (see the engine's
    // `store_fact`), and rowids are re-assigned by the copy — so the FTS
    // row must be derived from the row's rowid in the TARGET db.
    let new_fact_ids = pending_ids(&tx, "key_facts")?;
    let fact_rows = if new_fact_ids.is_empty() {
        0
    } else {
        let cols = shared_columns(&tx, "key_facts")?;
        insert_missing(&tx, "key_facts", &cols)?;
        for id in &new_fact_ids {
            tx.execute(
                "INSERT INTO key_facts_fts (rowid, fact)
                 SELECT rowid, fact FROM main.key_facts WHERE id = ?1",
                [id],
            )
            .map_err(|e| format!("key_facts_fts backfill: {e}"))?;
        }
        new_fact_ids.len()
    };

    // ── entity graph side tables (composite PKs → OR IGNORE suffices) ───
    tx.execute_batch(
        "INSERT OR IGNORE INTO main.entity_alias (agent_id, canonical, alias, created_at)
             SELECT agent_id, canonical, alias, created_at FROM src.entity_alias;
         INSERT OR IGNORE INTO main.entity_embedding (agent_id, entity, model, vec, created_at)
             SELECT agent_id, entity, model, vec, created_at FROM src.entity_embedding;",
    )
    .map_err(|e| format!("entity tables: {e}"))?;

    // ── memories_archive (lazily created by run_decay — may be absent) ──
    if table_exists(&tx, "src", "memories_archive")? {
        // Same DDL the engine's `forget` uses, so both writers agree.
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS main.memories_archive (
                id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                content TEXT NOT NULL,
                timestamp TEXT NOT NULL,
                tags TEXT NOT NULL DEFAULT '[]',
                layer TEXT NOT NULL DEFAULT 'episodic',
                importance REAL NOT NULL DEFAULT 5.0,
                access_count INTEGER NOT NULL DEFAULT 0,
                last_accessed TEXT,
                source_event TEXT DEFAULT '',
                archived_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            INSERT OR IGNORE INTO main.memories_archive
                SELECT * FROM src.memories_archive;",
        )
        .map_err(|e| format!("memories_archive: {e}"))?;
    }

    tx.commit().map_err(|e| format!("commit: {e}"))?;
    Ok((mem_rows, fact_rows))
}

/// Ids present in `src.<table>` but not in `main.<table>`.
fn pending_ids(conn: &Connection, table: &str) -> Result<Vec<String>, String> {
    let sql = format!(
        "SELECT id FROM src.{table} WHERE id NOT IN (SELECT id FROM main.{table})"
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| format!("{table} ids: {e}"))?;
    let ids = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| format!("{table} ids: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("{table} ids: {e}"))?;
    Ok(ids)
}

/// Column names present in BOTH `main.<table>` and `src.<table>`, in the
/// main table's order. Both schemas are engine-migrated to current right
/// before this runs, so in practice this is the full column set — the
/// intersection is defense in depth against a source the engine could not
/// fully migrate.
fn shared_columns(conn: &Connection, table: &str) -> Result<String, String> {
    let list = |schema: &str| -> Result<Vec<String>, String> {
        let sql = format!("PRAGMA {schema}.table_info({table})");
        let mut stmt = conn.prepare(&sql).map_err(|e| format!("table_info: {e}"))?;
        let cols = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|e| format!("table_info: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("table_info: {e}"))?;
        Ok(cols)
    };
    let main_cols = list("main")?;
    let src_cols = list("src")?;
    let cols: Vec<String> = main_cols
        .into_iter()
        .filter(|c| src_cols.contains(c))
        .collect();
    if cols.is_empty() {
        return Err(format!("{table}: no shared columns"));
    }
    Ok(cols.join(", "))
}

/// `INSERT INTO main.<table> (cols) SELECT cols FROM src.<table>` for rows
/// whose id is not yet in main.
fn insert_missing(conn: &Connection, table: &str, cols: &str) -> Result<(), String> {
    let sql = format!(
        "INSERT INTO main.{table} ({cols})
         SELECT {cols} FROM src.{table}
         WHERE id NOT IN (SELECT id FROM main.{table})"
    );
    conn.execute_batch(&sql)
        .map_err(|e| format!("{table} copy: {e}"))?;
    Ok(())
}

fn table_exists(conn: &Connection, schema: &str, table: &str) -> Result<bool, String> {
    let sql = format!(
        "SELECT 1 FROM {schema}.sqlite_master WHERE type = 'table' AND name = ?1 LIMIT 1"
    );
    conn.query_row(&sql, [table], |_| Ok(()))
        .map(|_| true)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(false),
            other => Err(format!("sqlite_master: {other}")),
        })
}

/// Checkpoint the source's WAL, then rename `memory.db` →
/// `memory.db.merged-<utc-timestamp>` and clean up empty sidecars. The
/// archive keeps the bytes on disk for manual recovery; the rename is what
/// makes `agent_memory_db_path` fall through to the shared db.
fn archive_source(source: &Path) -> Result<(), String> {
    {
        let conn = Connection::open(source).map_err(|e| format!("reopen source: {e}"))?;
        let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
    }
    let archived: PathBuf = source.with_extension(format!(
        "db.merged-{}",
        chrono::Utc::now().format("%Y%m%dT%H%M%S")
    ));
    std::fs::rename(source, &archived).map_err(|e| format!("archive rename: {e}"))?;
    for sidecar in ["-wal", "-shm"] {
        let mut p = source.as_os_str().to_owned();
        p.push(sidecar);
        let p = PathBuf::from(p);
        if p.exists() {
            let _ = std::fs::remove_file(&p); // empty after TRUNCATE checkpoint
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_core::traits::MemoryEngine as _;
    use duduclaw_core::types::{MemoryEntry, MemoryLayer};
    use duduclaw_memory::SqliteMemoryEngine;

    fn entry(agent_id: &str, content: &str) -> MemoryEntry {
        MemoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: agent_id.to_string(),
            content: content.to_string(),
            timestamp: chrono::Utc::now(),
            tags: vec![],
            embedding: None,
            layer: MemoryLayer::Episodic,
            importance: 5.0,
            access_count: 0,
            last_accessed: None,
            source_event: "test".to_string(),
        }
    }

    fn count(db: &Path, sql: &str) -> i64 {
        let conn = Connection::open(db).unwrap();
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    /// The incident shape: stray per-agent state db + richer shared db.
    /// After the merge the shared db holds both, FTS finds the migrated
    /// rows, and the stray file is archived out of the resolution path.
    #[tokio::test]
    async fn merges_stray_state_db_into_shared() {
        let home = tempfile::tempdir().unwrap();
        let state_dir = home.path().join("agents").join("trader").join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let stray = state_dir.join("memory.db");
        let shared = home.path().join("memory.db");

        {
            let eng = SqliteMemoryEngine::new(&stray).unwrap();
            eng.store("trader", entry("trader", "Prediction deviation: expected 0.7"))
                .await
                .unwrap();
            eng.store_fact("trader", "user prefers daily strategy briefings", "telegram", "c1", "s1")
                .await
                .unwrap();
        }
        {
            let eng = SqliteMemoryEngine::new(&shared).unwrap();
            eng.store_fact("trader", "position: 36 shares of 00919", "telegram", "c1", "s2")
                .await
                .unwrap();
        }

        let outcome = merge_per_agent_memory_dbs(home.path());
        assert_eq!(outcome.merged_files, 1, "errors: {:?}", outcome.errors);
        assert_eq!(outcome.memories_rows, 1);
        assert_eq!(outcome.key_facts_rows, 1);
        assert!(outcome.errors.is_empty());

        // Shared db now holds both facts and the episodic row.
        assert_eq!(count(&shared, "SELECT count(*) FROM key_facts"), 2);
        assert_eq!(count(&shared, "SELECT count(*) FROM memories"), 1);
        // Migrated rows are FTS-searchable in the shared db.
        assert_eq!(
            count(&shared, "SELECT count(*) FROM key_facts_fts WHERE fact MATCH 'briefings'"),
            1
        );
        assert_eq!(
            count(&shared, "SELECT count(*) FROM memories_fts WHERE memories_fts MATCH 'deviation'"),
            1
        );
        // Stray file archived; read resolution falls through to shared.
        assert!(!stray.exists());
        let archived = std::fs::read_dir(&state_dir)
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().starts_with("memory.db.merged-"));
        assert!(archived, "archive file missing");
    }

    #[tokio::test]
    async fn second_run_is_a_noop() {
        let home = tempfile::tempdir().unwrap();
        let agent_dir = home.path().join("agents").join("a");
        std::fs::create_dir_all(&agent_dir).unwrap();
        // Legacy location (`agents/<id>/memory.db`) is also picked up.
        let legacy = agent_dir.join("memory.db");
        {
            let eng = SqliteMemoryEngine::new(&legacy).unwrap();
            eng.store("a", entry("a", "legacy row")).await.unwrap();
        }

        let first = merge_per_agent_memory_dbs(home.path());
        assert_eq!(first.merged_files, 1);
        let second = merge_per_agent_memory_dbs(home.path());
        assert_eq!(second.merged_files, 0);
        assert!(second.errors.is_empty());
        assert_eq!(
            count(&home.path().join("memory.db"), "SELECT count(*) FROM memories"),
            1
        );
    }

    #[tokio::test]
    async fn existing_shared_rows_are_never_overwritten() {
        let home = tempfile::tempdir().unwrap();
        let state_dir = home.path().join("agents").join("a").join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let stray = state_dir.join("memory.db");
        let shared = home.path().join("memory.db");

        let mut shared_entry = entry("a", "shared version — must survive");
        shared_entry.id = "fixed-id".to_string();
        let mut stray_entry = entry("a", "stray version — must NOT replace shared");
        stray_entry.id = "fixed-id".to_string();

        SqliteMemoryEngine::new(&shared)
            .unwrap()
            .store("a", shared_entry)
            .await
            .unwrap();
        SqliteMemoryEngine::new(&stray)
            .unwrap()
            .store("a", stray_entry)
            .await
            .unwrap();

        let outcome = merge_per_agent_memory_dbs(home.path());
        assert_eq!(outcome.merged_files, 1);
        assert_eq!(outcome.memories_rows, 0, "colliding id must not copy");
        let content: String = Connection::open(&shared)
            .unwrap()
            .query_row("SELECT content FROM memories WHERE id = 'fixed-id'", [], |r| r.get(0))
            .unwrap();
        assert!(content.starts_with("shared version"));
    }

    #[tokio::test]
    async fn unreadable_source_is_reported_and_left_in_place() {
        let home = tempfile::tempdir().unwrap();
        let state_dir = home.path().join("agents").join("bad").join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let garbage = state_dir.join("memory.db");
        std::fs::write(&garbage, b"this is not a sqlite database").unwrap();

        let outcome = merge_per_agent_memory_dbs(home.path());
        assert_eq!(outcome.merged_files, 0);
        assert_eq!(outcome.errors.len(), 1);
        assert!(garbage.exists(), "failed source must stay in place");
    }
}
