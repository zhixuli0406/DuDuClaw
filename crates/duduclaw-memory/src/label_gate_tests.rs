//! F6fb2: the narrowing-write scope (GDPR erasure record, archive moves) and
//! the gate on lazily created tables, with a refusing gate installed on the
//! engine connection only (the process-wide gate stays uninstalled here).
use super::*;
use crate::engine::SqliteMemoryEngine;
use chrono::Utc;
use duduclaw_core::traits::MemoryEngine;
use duduclaw_core::types::{MemoryEntry, MemoryLayer};

const Q: &str = "q-agent";

fn refuse_q(_: Option<&Path>, label: &str) -> Result<(), String> {
    if label == Q { Err("refused".to_owned()) } else { Ok(()) }
}

fn entry(id: &str, agent: &str, content: &str, age_days: i64) -> MemoryEntry {
    MemoryEntry {
        id: id.to_owned(), agent_id: agent.to_owned(), content: content.to_owned(),
        timestamp: Utc::now() - chrono::Duration::days(age_days), tags: vec![], embedding: None,
        layer: MemoryLayer::Episodic, importance: 1.0, access_count: 0, last_accessed: None, source_event: "f6fb2".to_owned(),
    }
}

/// An engine with rows for `q-agent` and `other`, then the refusing gate on
/// its connection (as if `q-agent` had just been quarantined).
async fn gated_engine(age_days: i64) -> SqliteMemoryEngine {
    let engine = SqliteMemoryEngine::in_memory().unwrap();
    engine.store(Q, entry("q1", Q, "user:alice likes tea", age_days)).await.unwrap();
    engine.store("other", entry("o1", "other", "user:alice likes coffee", age_days)).await.unwrap();
    let conn = engine.conn_for_maintenance().await;
    install_write_triggers(&conn, refuse_q, &LABEL_COLUMNS, None, MEMORY_RULES).unwrap();
    drop(conn);
    engine
}

fn raw_insert(conn: &Connection, id: &str, agent: &str) -> rusqlite::Result<usize> {
    conn.execute("INSERT INTO memories (id, agent_id, content, timestamp, source_event, predicate)
        VALUES (?1, ?2, 'planted', 'now', 'gdpr_erasure', 'erasure_event')", [id, agent])
}

fn count(conn: &Connection, sql: &str, id: &str) -> i64 { conn.query_row(sql, [id], |r| r.get(0)).unwrap() }

/// Item 1: the erasure record of a refused label is written (base + FTS);
/// a look-alike ordinary insert of the label is still refused afterwards.
#[tokio::test]
async fn f6fb2_gdpr_erase_of_quarantined_label_succeeds() {
    let engine = gated_engine(0).await;
    let summary = crate::gdpr::gdpr_erase(&engine, Q, "user:alice", true).await.unwrap();
    assert_eq!(summary.memories_deleted, 1);
    let record = summary.tombstone_id.unwrap();
    let conn = engine.conn_for_maintenance().await;
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM memories WHERE id = ?1 AND agent_id = 'q-agent'", &record), 1);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM memories_fts WHERE memory_id = ?1", &record), 1);
    assert!(raw_insert(&conn, "planted", Q).unwrap_err().to_string().contains(REFUSED));
    raw_insert(&conn, "fine", "other").unwrap();
    drop(conn);
    assert!(engine.store(Q, entry("q2", Q, "new", 0)).await.is_err(), "ordinary writes stay refused");
}

/// Item 1: the scope admits only its own statement, only its label, and
/// ends with the statement (also on panic).
#[tokio::test]
async fn f6fb2_narrowing_scope_is_one_statement_and_one_label() {
    let engine = gated_engine(0).await;
    let conn = engine.conn_for_maintenance().await;
    narrowing_write(NarrowingWrite::ErasureRecord, Some(Q), || raw_insert(&conn, "in-scope", Q)).unwrap();
    let wrong = narrowing_write(NarrowingWrite::ErasureRecord, Some("other"), || raw_insert(&conn, "wrong-label", Q));
    assert!(wrong.unwrap_err().to_string().contains(REFUSED), "a scope for another label admits nothing");
    assert!(raw_insert(&conn, "after", Q).unwrap_err().to_string().contains(REFUSED), "the scope ended");
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(||
        narrowing_write::<()>(NarrowingWrite::ArchiveMove, None, || panic!("inside the scope"))));
    assert!(panicked.is_err());
    assert!(!narrowing_admits(Q), "a panic ends the scope too");
    assert!(raw_insert(&conn, "after-panic", Q).unwrap_err().to_string().contains(REFUSED));
}

/// Item 2: the archive created lazily by the first `forget` carries the gate
/// (raw insert of the refused label aborts); `forget` and decay still move a
/// refused label's rows into it (narrowing), so a sweep never aborts.
#[tokio::test]
async fn f6fb2_lazy_archive_gated_and_archive_moves_admitted() {
    let engine = gated_engine(100).await;
    {
        let conn = engine.conn_for_maintenance().await;
        assert!(!gated_tables(&conn).unwrap().iter().any(|(t, _)| t == "memories_archive"), "no archive yet");
    }
    assert!(engine.forget(Q, "q1").await.unwrap(), "a forget of the refused label still runs");
    {
        let conn = engine.conn_for_maintenance().await;
        let names = gated_tables(&conn).unwrap().into_iter().find(|(t, _)| t == "memories_archive").map(|(_, n)| n).unwrap();
        for kind in ["i", "u"] { assert!(names.contains(&format!("{TRIGGER_PREFIX}{kind}_memories_archive_c_agent_id"))); }
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM memories_archive WHERE id = ?1", "q1"), 1);
        let planted = conn.execute("INSERT INTO memories_archive (id, agent_id, content, timestamp) VALUES ('p', ?1, 'p', 'now')", [Q]);
        assert!(planted.unwrap_err().to_string().contains(REFUSED));
        // The forget's own refresh kept the refusing gate (not re-registered).
        assert!(raw_insert(&conn, "still-refused", Q).is_err());
        drop(conn);
    }
    // Decay (all agents): an old row of the refused label is archived too.
    // (The fixture plants it through a scope: ordinary writes are refused.)
    assert!(engine.store(Q, entry("q-new", Q, "new", 0)).await.is_err());
    {
        let conn = engine.conn_for_maintenance().await;
        let old = (Utc::now() - chrono::Duration::days(200)).to_rfc3339();
        narrowing_write(NarrowingWrite::ArchiveMove, Some(Q), || conn.execute(
            "INSERT INTO memories (id, agent_id, content, timestamp, importance) VALUES ('q-old', ?1, 'old', ?2, 0.5)",
            [Q, old.as_str()])).unwrap();
    }
    let report = crate::decay::run_decay(&engine, &crate::decay::MemoryDecayPolicy::default()).await;
    assert!(report.archived >= 2, "the sweep archived both labels' rows: {report:?}");
    let conn = engine.conn_for_maintenance().await;
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM memories_archive WHERE id = ?1", "q-old"), 1);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM memories_archive WHERE id = ?1", "o1"), 1);
}
