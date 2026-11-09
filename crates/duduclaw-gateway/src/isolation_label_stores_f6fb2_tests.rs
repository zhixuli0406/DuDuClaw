//! F6fb2 follow-up round (F6 design r5 §3.3, H4): GDPR erasure of a
//! quarantined label through the real writer gate, gate triggers on tables
//! created lazily after open (memory.db `memories_archive`, sessions.db
//! `legacy_import_messages`), and full-text rows kept in step with base rows
//! by the in-engine re-key and the store purge. Child of
//! `isolation_label_stores_tests.rs` (same fixtures).
use super::*;
use duduclaw_memory::label_gate::{gated_tables, REFUSED, TRIGGER_PREFIX};

const CAROL: &str = "carol-desk";

fn has_trigger(db: &Connection, table: &str, name: &str) -> bool {
    gated_tables(db).unwrap().iter().any(|(t, names)| t == table && names.iter().any(|n| n == name))
}

/// FTS rows whose agent differs from their base row's.
async fn diverged(memory: &duduclaw_memory::SqliteMemoryEngine) -> i64 {
    let conn = memory.conn_for_maintenance().await;
    conn.query_row("SELECT COUNT(*) FROM memories_fts f JOIN memories m ON m.id = f.memory_id
        WHERE f.agent_id IS NOT m.agent_id", [], |r| r.get(0)).unwrap()
}

/// Item 1: a right-to-erasure request on a quarantined label deletes the
/// rows and writes its erasure record (base row and FTS row) through the
/// gate's erasure scope; outside that scope an ordinary insert of the label —
/// even one shaped like an erasure record — is still refused.
#[tokio::test]
async fn f6fb2_gdpr_erase_of_quarantined_label_succeeds() {
    use duduclaw_core::traits::MemoryEngine;
    install_memory_gate();
    let dir = private_home();
    let home = dir.path();
    let memory = duduclaw_memory::SqliteMemoryEngine::new(&home.join("memory.db")).unwrap();
    memory.store(LABEL, memory_entry(LABEL)).await.unwrap();
    memory.store(OTHER, memory_entry(OTHER)).await.unwrap();
    quarantine_row(home, LABEL, 1, "sealed");
    let _mode = Mode::protected(true);
    let summary = duduclaw_memory::gdpr_erase(&memory, LABEL, "remembers", true).await.unwrap();
    assert_eq!(summary.memories_deleted, 1, "only the quarantined label's row");
    let record = summary.tombstone_id.clone().expect("erasure record written");
    let conn = memory.conn_for_maintenance().await;
    let (agent, source): (String, String) = conn.query_row("SELECT agent_id, source_event FROM memories WHERE id = ?1",
        [&record], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!((agent.as_str(), source.as_str()), (LABEL, "gdpr_erasure"));
    let fts: i64 = conn.query_row("SELECT COUNT(*) FROM memories_fts WHERE memory_id = ?1 AND agent_id = ?2",
        params![record, LABEL], |r| r.get(0)).unwrap();
    assert_eq!(fts, 1);
    // The scope is that one statement: a look-alike insert is refused.
    let planted = conn.execute("INSERT INTO memories (id, agent_id, content, timestamp, source_event, predicate)
        VALUES ('planted', ?1, 'planted', 'now', 'gdpr_erasure', 'erasure_event')", [LABEL]);
    assert!(planted.unwrap_err().to_string().contains(REFUSED));
    let other: i64 = conn.query_row("SELECT COUNT(*) FROM memories WHERE agent_id = ?1", [OTHER], |r| r.get(0)).unwrap();
    assert_eq!(other, 1, "other labels untouched");
    drop(conn);
    let refused = memory.store(LABEL, memory_entry(LABEL)).await.unwrap_err();
    assert!(refused.to_string().contains(LABEL_QUARANTINED), "{refused}");
}

/// Item 2: tables created after the connection opened carry the gate:
/// `memories_archive` (first `forget`) and `legacy_import_messages` (first
/// legacy import). A forget of the quarantined label's own row still moves
/// it into the archive (narrowing), a raw archive insert of the label does not.
#[tokio::test]
async fn f6fb2_lazily_created_tables_gated() {
    use duduclaw_core::traits::MemoryEngine;
    install_memory_gate();
    let dir = private_home();
    let home = dir.path();
    let memory = duduclaw_memory::SqliteMemoryEngine::new(&home.join("memory.db")).unwrap();
    let (mine, theirs) = (memory_entry(LABEL), memory_entry(OTHER));
    let (mine_id, theirs_id) = (mine.id.clone(), theirs.id.clone());
    memory.store(LABEL, mine).await.unwrap();
    memory.store(OTHER, theirs).await.unwrap();
    let sessions = crate::session::SessionManager::new(&home.join("sessions.db")).unwrap();
    sessions.get_or_create("webchat:old", LABEL).await.unwrap_or_else(|_| panic!("session of the label"));
    {
        let conn = memory.conn_for_maintenance().await;
        let archive: i64 = conn.query_row("SELECT COUNT(*) FROM sqlite_master WHERE name = 'memories_archive'", [], |r| r.get(0)).unwrap();
        assert_eq!(archive, 0, "the archive is created lazily");
    }
    assert!(memory.forget(OTHER, &theirs_id).await.unwrap());
    quarantine_row(home, LABEL, 1, "sealed");
    let _mode = Mode::protected(true);
    {
        let conn = memory.conn_for_maintenance().await;
        for kind in ["i", "u"] {
            let name = format!("{TRIGGER_PREFIX}{kind}_memories_archive_c_agent_id");
            assert!(has_trigger(&conn, "memories_archive", &name), "{name}");
        }
        let raw = conn.execute("INSERT INTO memories_archive (id, agent_id, content, timestamp) VALUES ('planted', ?1, 'planted', 'now')",
            [LABEL]);
        assert!(raw.unwrap_err().to_string().contains(REFUSED));
        conn.execute("INSERT INTO memories_archive (id, agent_id, content, timestamp) VALUES ('fine', ?1, 'fine', 'now')", [OTHER]).unwrap();
        assert_eq!(conn.execute("UPDATE memories_archive SET agent_id = ?1 WHERE agent_id = ?2", [LABEL, OTHER]).unwrap(), 0,
            "nothing is re-keyed into the label's archive");
    }
    // The archive move of a forget is admitted for the forgetting label only.
    assert!(memory.forget(LABEL, &mine_id).await.unwrap());
    {
        let conn = memory.conn_for_maintenance().await;
        let count = |sql: &str| -> i64 { conn.query_row(sql, [&mine_id], |r| r.get(0)).unwrap() };
        assert_eq!(count("SELECT COUNT(*) FROM memories_archive WHERE id = ?1"), 1);
        assert_eq!(count("SELECT COUNT(*) FROM memories WHERE id = ?1"), 0);
        assert_eq!(count("SELECT COUNT(*) FROM memories_fts WHERE memory_id = ?1"), 0);
    }

    // sessions.db: the legacy import table, created by the first import (for
    // the other label, which the gate allows) after the pool opened.
    sessions.import_legacy_message("legacy:bob", OTHER, "record-1", "legacy", "hello").await.unwrap();
    let name = format!("{TRIGGER_PREFIX}i_legacy_import_messages_p_session_id");
    let mut held = Vec::new();
    let mut gated = 0;
    for _ in 0..4 {
        let conn = sessions.acquire().await;
        if has_trigger(&conn, "legacy_import_messages", &name) {
            gated += 1;
            let raw = conn.execute("INSERT INTO legacy_import_messages (record_id, session_id, payload)
                VALUES ('planted', 'webchat:old', '{}')", []);
            assert!(raw.unwrap_err().to_string().contains(REFUSED));
        }
        held.push(conn);
    }
    assert!(gated >= 1, "the importing connection carries the gate on the new table");
}

/// Item 4: an in-engine re-key (`reassign_agent`) on the engine connection,
/// whose triggers skip a quarantined label's rows, moves each FTS row only
/// with its base row — out of, into, and away from a quarantined label.
#[tokio::test]
async fn f6fb2_rekey_keeps_fts_in_step_with_skipped_rows() {
    use duduclaw_core::traits::MemoryEngine;
    install_memory_gate();
    let dir = private_home();
    let home = dir.path();
    let memory = duduclaw_memory::SqliteMemoryEngine::new(&home.join("memory.db")).unwrap();
    for label in [LABEL, LABEL, OTHER] { memory.store(label, memory_entry(label)).await.unwrap(); }
    quarantine_row(home, LABEL, 1, "sealed");
    let _mode = Mode::protected(true);
    let out = duduclaw_memory::reassign_agent(&memory, LABEL, CAROL).await.unwrap();
    assert_eq!(out.memories, 0, "nothing leaves a quarantined label");
    assert_eq!(diverged(&memory).await, 0, "FTS rows stayed with their skipped base rows");
    assert!(memory.search(CAROL, "remembers", 10).await.unwrap().is_empty(), "no quarantined content under the new label");
    let into = duduclaw_memory::reassign_agent(&memory, OTHER, LABEL).await.unwrap();
    assert_eq!(into.memories, 0, "nothing enters a quarantined label");
    assert_eq!(diverged(&memory).await, 0);
    let moved = duduclaw_memory::reassign_agent(&memory, OTHER, CAROL).await.unwrap();
    assert_eq!(moved.memories, 1, "a free label still re-keys");
    assert_eq!(diverged(&memory).await, 0, "and its FTS row moves with it");
    assert_eq!(memory.search(CAROL, "remembers", 10).await.unwrap().len(), 1);
}

/// Item 4: the store purge deletes the label's FTS rows with its base rows
/// in the same transaction — including `key_facts_fts`, which carries no
/// label column (rowid-linked) and survived a purge before.
#[tokio::test]
async fn f6fb2_purge_removes_fts_rows_with_base_rows() {
    use duduclaw_core::traits::MemoryEngine;
    let dir = private_home();
    let home = dir.path();
    let memory = duduclaw_memory::SqliteMemoryEngine::new(&home.join("memory.db")).unwrap();
    memory.store(LABEL, memory_entry(LABEL)).await.unwrap();
    memory.store(OTHER, memory_entry(OTHER)).await.unwrap();
    drop(memory);
    let db = Connection::open(home.join("memory.db")).unwrap();
    for (id, label) in [("kf-a", LABEL), ("kf-b", OTHER)] {
        db.execute("INSERT INTO key_facts (id, agent_id, fact, timestamp) VALUES (?1, ?2, 'secret ledger fact', 'now')",
            params![id, label]).unwrap();
        db.execute("INSERT INTO key_facts_fts (rowid, fact) SELECT rowid, fact FROM key_facts WHERE id = ?1", [id]).unwrap();
    }
    assert!(sqlite::FTS_COMPANIONS.iter().any(|(fts, ..)| *fts == "key_facts_fts"));
    assert!(store("memory").purge(home, LABEL).unwrap() > 0);
    let count = |sql: &str| -> i64 { db.query_row(sql, [], |r| r.get(0)).unwrap() };
    assert_eq!(count("SELECT COUNT(*) FROM key_facts_fts WHERE key_facts_fts MATCH 'ledger'"), 1, "only the other label's fact text");
    assert_eq!(count("SELECT COUNT(*) FROM key_facts_fts f JOIN key_facts k ON k.rowid = f.rowid WHERE k.id = 'kf-b'"), 1);
    assert_eq!(count(&format!("SELECT COUNT(*) FROM memories_fts WHERE agent_id = '{LABEL}'")), 0);
    assert_eq!(count(&format!("SELECT COUNT(*) FROM memories_fts WHERE agent_id = '{OTHER}'")), 1);
    assert_eq!(store("memory").count_under(home, LABEL), Ok(0));
}

/// F6 b3 N6: the GDPR erasure record (the one insert the gate admits on a
/// quarantined label) is not part of the store marks, so an erasure that
/// removed nothing leaves a same-id restore's mark re-check equal; the S10
/// zero count still counts the record, and rows an erasure deleted still
/// change the marks.
#[tokio::test]
async fn f6b3_gdpr_erasure_record_keeps_store_marks() {
    use crate::isolation_restore_wiring::current_marks;
    use duduclaw_core::traits::MemoryEngine;
    install_memory_gate();
    let dir = private_home();
    let home = dir.path();
    let memory = duduclaw_memory::SqliteMemoryEngine::new(&home.join("memory.db")).unwrap();
    memory.store(LABEL, memory_entry(LABEL)).await.unwrap();
    memory.store(OTHER, memory_entry(OTHER)).await.unwrap();
    quarantine_row(home, LABEL, 1, "sealed");
    let recorded = tombstone_effects(home, LABEL, EmployeeId([1; 16]), 0)["store_marks"].clone();
    assert!(recorded["memory"].as_u64().unwrap() >= 1, "{recorded}");
    assert_eq!(current_marks(home, LABEL).unwrap(), recorded);
    let _mode = Mode::protected(true);
    let summary = duduclaw_memory::gdpr_erase(&memory, LABEL, "nobody@example.com", true).await.unwrap();
    assert_eq!(summary.memories_deleted, 0);
    assert!(summary.tombstone_id.is_some(), "the erasure record was written");
    assert_eq!(current_marks(home, LABEL).unwrap(), recorded, "the record alone does not change the marks");
    let memory_store = store("memory");
    assert_eq!(memory_store.count_under(home, LABEL).unwrap(), memory_store.mark_under(home, LABEL).unwrap() + 2,
        "S10 still counts the record (base row and FTS row)");
    // A label without an erasure record: its marks are its plain count.
    assert_eq!(memory_store.mark_under(home, OTHER), memory_store.count_under(home, OTHER));
    // Rows the erasure deleted do change the marks.
    let summary = duduclaw_memory::gdpr_erase(&memory, LABEL, "remembers", true).await.unwrap();
    assert_eq!(summary.memories_deleted, 1);
    assert_ne!(current_marks(home, LABEL).unwrap(), recorded);
}
