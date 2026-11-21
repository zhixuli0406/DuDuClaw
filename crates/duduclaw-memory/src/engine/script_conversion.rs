//! Operator-driven prose conversion with transactional FTS/vector updates and restore records.
use super::*;
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::Path;
use sha2::{Digest, Sha256};

#[derive(Debug, Default)]
pub struct ScriptConversionReport {
    pub key_facts: usize,
    pub memories: usize,
    pub changed: usize,
    pub skipped_japanese: usize,
    pub embeddings_cleared: usize,
    pub samples: Vec<(String, String, String)>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct RestoreRow {
    agent_id: String,
    table: String,
    primary_key: String,
    column: String,
    before: String,
    after_hash: String,
}

fn text_hash(text: &str) -> String { format!("{:x}", Sha256::digest(text.as_bytes())) }
fn conversion_error(error: impl std::fmt::Display) -> DuDuClawError {
    DuDuClawError::Memory(error.to_string())
}

fn fts_rowids(conn: &rusqlite::Connection, agent: &str) -> Result<HashMap<String, Vec<i64>>> {
    let mut stmt = conn.prepare("SELECT memory_id, rowid FROM memories_fts WHERE agent_id = ?1").map_err(conversion_error)?;
    let rows = stmt.query_map(params![agent], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))).map_err(conversion_error)?;
    let mut map = HashMap::<String, Vec<i64>>::new();
    for row in rows { let (id, rowid) = row.map_err(conversion_error)?; map.entry(id).or_default().push(rowid); }
    Ok(map)
}

impl SqliteMemoryEngine {
    /// Namespaces in either text table, including legacy namespaces without an agent directory.
    pub async fn script_conversion_agents(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().await;
        crate::isolation_io::verify()?;
        let mut stmt = conn.prepare("SELECT agent_id FROM memories UNION SELECT agent_id FROM key_facts ORDER BY agent_id")
            .map_err(conversion_error)?;
        let rows = stmt.query_map([], |r| r.get(0)).map_err(conversion_error)?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(conversion_error)
    }

    /// Refresh only vectors belonging to the attached model. Foreign/unknown vectors are
    /// cleared and excluded from similarity checks until explicitly re-embedded.
    fn write_script_text(&self, conn: &rusqlite::Connection, agent: &str, table: &str, rowid: i64,
        id: &str, text: &str, fts: &HashMap<String, Vec<i64>>) -> Result<bool> {
        if table == "key_facts" {
            conn.execute("UPDATE key_facts SET fact = ?1 WHERE rowid = ?2 AND agent_id = ?3", params![text, rowid, agent]).map_err(conversion_error)?;
            conn.execute("DELETE FROM key_facts_fts WHERE rowid = ?1", params![rowid]).map_err(conversion_error)?;
            conn.execute("INSERT INTO key_facts_fts(rowid, fact) VALUES (?1, ?2)", params![rowid, text]).map_err(conversion_error)?;
            return Ok(false);
        }
        let (embedded, model): (bool, Option<String>) = conn.query_row(
            "SELECT embedding IS NOT NULL, embedding_model FROM memories WHERE id = ?1 AND agent_id = ?2",
            params![id, agent], |r| Ok((r.get(0)?, r.get(1)?))).map_err(conversion_error)?;
        conn.execute("UPDATE memories SET content = ?1, embedding = NULL WHERE id = ?2 AND agent_id = ?3", params![text, id, agent]).map_err(conversion_error)?;
        let matching = self.embedder.as_ref().filter(|e| Some(e.id()) == model.as_deref());
        if let Some(embedder) = matching {
            crate::vector::store_embedding(conn, agent, id, embedder.id(), &embedder.embed(text)?)?;
        }
        if let Some(rowids) = fts.get(id) {
            for rowid in rowids {
                conn.execute("DELETE FROM memories_fts WHERE rowid = ?1", params![rowid]).map_err(conversion_error)?;
            }
        }
        conn.execute("INSERT INTO memories_fts(content, agent_id, memory_id) VALUES (?1, ?2, ?3)", params![text, agent, id]).map_err(conversion_error)?;
        Ok(embedded && matching.is_none())
    }

    #[cfg(test)]
    async fn convert_memory_script(&self, agent: &str, dry_run: bool) -> Result<ScriptConversionReport> {
        let dir = tempfile::tempdir().map_err(conversion_error)?;
        self.convert_memory_script_with_backup(agent, dry_run, &dir.path().join("restore.jsonl")).await
    }

    /// One transaction per namespace, bounded text batches, with a durable restore file before commit.
    /// No automatic background embedding rebuild is assumed.
    pub async fn convert_memory_script_with_backup(&self, agent_id: &str, dry_run: bool, backup: &Path) -> Result<ScriptConversionReport> {
        let conn = self.conn.lock().await;
        crate::isolation_io::verify()?;
        let convert = || -> Result<ScriptConversionReport> {
            if !dry_run { crate::label_gate::check(&conn, agent_id)?; }
            let mut file = if dry_run { None } else {
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)] {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                Some(std::io::BufWriter::new(options.open(backup).map_err(conversion_error)?))
            };
            let fts = if dry_run { HashMap::new() } else { fts_rowids(&conn, agent_id)? };
            let mut report = ScriptConversionReport::default();
            for (table, column) in [("key_facts", "fact"), ("memories", "content")] {
                let mut cursor = i64::MIN;
                loop {
                    let mut stmt = conn.prepare(&format!("SELECT rowid, id, {column} FROM {table} WHERE agent_id = ?1 AND rowid > ?2 ORDER BY rowid LIMIT 256")).map_err(conversion_error)?;
                    let rows = stmt.query_map(params![agent_id, cursor], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
                        .map_err(conversion_error)?.collect::<std::result::Result<Vec<_>, _>>().map_err(conversion_error)?;
                    drop(stmt);
                    if rows.is_empty() { break; }
                    for (rowid, id, before) in rows {
                        cursor = rowid;
                        if duduclaw_core::zh_variant::contains_kana(&before) { report.skipped_japanese += 1; continue; }
                        let after = duduclaw_core::zh_variant::to_traditional_preserving_literals(&before);
                        if before == after { continue; }
                        if table == "key_facts" { report.key_facts += 1; } else { report.memories += 1; }
                        report.changed += 1;
                        if report.samples.len() < 10 { report.samples.push((table.into(), before.clone(), after.clone())); }
                        if let Some(file) = file.as_mut() {
                            serde_json::to_writer(&mut *file, &RestoreRow { agent_id: agent_id.into(), table: table.into(), primary_key: id.clone(), column: column.into(), before, after_hash: text_hash(&after) }).map_err(conversion_error)?;
                            file.write_all(b"\n").map_err(conversion_error)?;
                        }
                    }
                }
            }
            if let Some(mut file) = file {
                file.flush().map_err(conversion_error)?;
                file.get_ref().sync_all().map_err(conversion_error)?;
                #[cfg(unix)]
                if let Some(parent) = backup.parent() { std::fs::File::open(parent).and_then(|dir| dir.sync_all()).map_err(conversion_error)?; }
                // The complete restore journal is durable before the first UPDATE.
                let records = std::io::BufReader::new(std::fs::File::open(backup).map_err(conversion_error)?);
                for line in records.lines() {
                    let row: RestoreRow = serde_json::from_str(&line.map_err(conversion_error)?).map_err(conversion_error)?;
                    if row.agent_id != agent_id || !matches!((row.table.as_str(), row.column.as_str()), ("key_facts", "fact") | ("memories", "content")) {
                        return Err(conversion_error("invalid conversion journal identity/table/column"));
                    }
                    let rowid: i64 = conn.query_row(&format!("SELECT rowid FROM {} WHERE id = ?1 AND agent_id = ?2", row.table),
                        params![row.primary_key, agent_id], |r| r.get(0)).map_err(conversion_error)?;
                    let after = duduclaw_core::zh_variant::to_traditional_preserving_literals(&row.before);
                    if text_hash(&after) != row.after_hash { return Err(conversion_error("restore journal changed during conversion")); }
                    if self.write_script_text(&conn, agent_id, &row.table, rowid, &row.primary_key, &after, &fts)? { report.embeddings_cleared += 1; }
                }
            }
            Ok(report)
        };
        if dry_run { convert() } else { Self::in_immediate_txn(&conn, convert) }
    }

    /// Restore all records atomically, refusing intervening edits, missing rows and invalid columns.
    pub async fn restore_memory_script(&self, path: &Path) -> Result<ScriptConversionReport> {
        let conn = self.conn.lock().await;
        crate::isolation_io::verify()?;
        Self::in_immediate_txn(&conn, || {
            let file = std::io::BufReader::new(std::fs::File::open(path).map_err(conversion_error)?);
            let mut maps = HashMap::new();
            let mut report = ScriptConversionReport::default();
            for line in file.lines() {
                let row: RestoreRow = serde_json::from_str(&line.map_err(conversion_error)?).map_err(conversion_error)?;
                if !matches!((row.table.as_str(), row.column.as_str()), ("key_facts", "fact") | ("memories", "content")) {
                    return Err(conversion_error("invalid restore table/column"));
                }
                crate::label_gate::check(&conn, &row.agent_id)?;
                let (rowid, current): (i64, String) = conn.query_row(&format!("SELECT rowid, {} FROM {} WHERE id = ?1 AND agent_id = ?2", row.column, row.table),
                    params![row.primary_key, row.agent_id], |r| Ok((r.get(0)?, r.get(1)?))).map_err(conversion_error)?;
                if text_hash(&current) != row.after_hash { return Err(conversion_error(format!("restore refused: {}/{} was edited after conversion", row.table, row.primary_key))); }
                if !maps.contains_key(&row.agent_id) { maps.insert(row.agent_id.clone(), fts_rowids(&conn, &row.agent_id)?); }
                if self.write_script_text(&conn, &row.agent_id, &row.table, rowid, &row.primary_key, &row.before, &maps[&row.agent_id])? { report.embeddings_cleared += 1; }
                report.changed += 1;
                if row.table == "key_facts" { report.key_facts += 1; } else { report.memories += 1; }
            }
            Ok(report)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn convert_script_dry_run_apply_and_fts() {
        let home = tempfile::tempdir().unwrap();
        let engine = SqliteMemoryEngine::new(&home.path().join("memory.db")).unwrap();
        {
            let conn = engine.conn.lock().await;
            for agent in ["a", "b"] {
                conn.execute("INSERT INTO memories(id, agent_id, content, timestamp, embedding, embedding_model) VALUES (?1, ?1, '用户记忆', '2026-11-21', X'00', 'old')", params![agent]).unwrap();
                conn.execute("INSERT INTO memories_fts(content, agent_id, memory_id) VALUES ('用户记忆', ?1, ?1)", params![agent]).unwrap();
                conn.execute("INSERT INTO key_facts(id, agent_id, fact, timestamp) VALUES (?1, ?1, '用户事实', '2026-11-21')", params![agent]).unwrap();
                conn.execute("INSERT INTO key_facts_fts(rowid, fact) VALUES (last_insert_rowid(), '用户事实')", []).unwrap();
            }
        }
        let dry = engine.convert_memory_script("a", true).await.unwrap();
        assert_eq!((dry.key_facts, dry.memories, dry.changed), (1, 1, 2));
        assert_eq!(dry.samples.len(), 2);
        {
            let conn = engine.conn.lock().await;
            let text: String = conn.query_row("SELECT content FROM memories WHERE id = 'a'", [], |r| r.get(0)).unwrap();
            assert_eq!(text, "用户记忆");
        }
        let applied = engine.convert_memory_script("a", false).await.unwrap();
        assert_eq!(applied.embeddings_cleared, 1);
        assert_eq!(engine.convert_memory_script("a", false).await.unwrap().changed, 0);
        let conn = engine.conn.lock().await;
        for (table, query) in [("memories_fts", "用戶記憶"), ("key_facts_fts", "用戶事實")] {
            let count: i64 = conn.query_row(&format!("SELECT count(*) FROM {table} WHERE {table} MATCH ?1"), params![query], |r| r.get(0)).unwrap();
            assert_eq!(count, 1);
        }
        let other: String = conn.query_row("SELECT content FROM memories WHERE id = 'b'", [], |r| r.get(0)).unwrap();
        assert_eq!(other, "用户记忆");
        let embedding: Option<Vec<u8>> = conn.query_row("SELECT embedding FROM memories WHERE id = 'a'", [], |r| r.get(0)).unwrap();
        assert!(embedding.is_none());
    }
}

#[cfg(test)]
mod atomicity_tests {
    use super::*;

    #[tokio::test]
    async fn convert_script_rolls_back_both_tables_on_failure() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        {
            let conn = engine.conn.lock().await;
            conn.execute_batch("INSERT INTO key_facts(id, agent_id, fact, timestamp) VALUES ('k', 'a', '用户', '2026-11-21');
                INSERT INTO key_facts_fts(rowid, fact) VALUES (last_insert_rowid(), '用户');
                INSERT INTO memories(id, agent_id, content, timestamp) VALUES ('m', 'a', '记忆', '2026-11-21');
                CREATE TEMP TRIGGER refuse_conversion BEFORE UPDATE OF content ON memories BEGIN SELECT RAISE(ABORT, 'test rollback'); END;").unwrap();
        }
        assert!(engine.convert_memory_script("a", false).await.is_err());
        let conn = engine.conn.lock().await;
        let fact: String = conn.query_row("SELECT fact FROM key_facts WHERE id = 'k'", [], |r| r.get(0)).unwrap();
        assert_eq!(fact, "用户");
        let fts: String = conn.query_row("SELECT fact FROM key_facts_fts LIMIT 1", [], |r| r.get(0)).unwrap();
        assert_eq!(fts, "用户");
    }
}

#[cfg(test)]
mod safety_tests {
    use super::*;
    use crate::vector::{EmbeddingProvider, NgramHashEmbedder};

    async fn seed(engine: &SqliteMemoryEngine, id: &str, text: &str, model: Option<&str>) {
        let conn = engine.conn.lock().await;
        conn.execute("INSERT INTO memories(id, agent_id, content, timestamp, embedding, embedding_model) VALUES (?1, 'a', ?2, '2026-11-21', X'00', ?3)", params![id, text, model]).unwrap();
        conn.execute("INSERT INTO memories_fts(content, agent_id, memory_id) VALUES (?1, 'a', ?2)", params![text, id]).unwrap();
    }

    #[tokio::test]
    async fn conversion_skips_kana_and_preserves_literals_and_counts_actual_changes() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        for (id, text) in [("jp", "会社の会議"), ("half", "ｶﾀｶﾅ 用户"), ("url", "https://站/用户"),
            ("code", "`用户`"), ("prose", "用户 https://站/用户 `记忆`")] {
            seed(&engine, id, text, None).await;
        }
        let dry = engine.convert_memory_script("a", true).await.unwrap();
        assert_eq!((dry.changed, dry.memories, dry.skipped_japanese), (1, 1, 2));
        let applied = engine.convert_memory_script("a", false).await.unwrap();
        assert_eq!((applied.changed, applied.skipped_japanese), (1, 2));
        let conn = engine.conn.lock().await;
        let text: String = conn.query_row("SELECT content FROM memories WHERE id = 'prose'", [], |r| r.get(0)).unwrap();
        assert_eq!(text, "用戶 https://站/用户 `记忆`");
        let text: String = conn.query_row("SELECT content FROM memories WHERE id = 'jp'", [], |r| r.get(0)).unwrap();
        assert_eq!(text, "会社の会議");
    }

    #[tokio::test]
    async fn conversion_reembeds_matching_model_and_clears_foreign_model() {
        let embedder = std::sync::Arc::new(NgramHashEmbedder::new());
        let engine = SqliteMemoryEngine::in_memory().unwrap().with_embedder(embedder.clone());
        seed(&engine, "matching", "用户记忆", Some(embedder.id())).await;
        seed(&engine, "foreign", "用户记忆", Some("foreign-v1")).await;
        seed(&engine, "unknown", "用户记忆", None).await;
        let report = engine.convert_memory_script("a", false).await.unwrap();
        assert_eq!(report.embeddings_cleared, 2);
        let conn = engine.conn.lock().await;
        let (blob, model): (Vec<u8>, String) = conn.query_row("SELECT embedding, embedding_model FROM memories WHERE id = 'matching'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(blob, crate::vector::encode_vec(&embedder.embed("用戶記憶").unwrap()));
        assert_eq!(model, embedder.id());
        for id in ["foreign", "unknown"] {
            let blob: Option<Vec<u8>> = conn.query_row("SELECT embedding FROM memories WHERE id = ?1", params![id], |r| r.get(0)).unwrap();
            assert!(blob.is_none());
        }
    }

    #[tokio::test]
    async fn restore_checks_hash_and_rolls_back_then_restores_fts_and_embeddings() {
        let home = tempfile::tempdir().unwrap();
        let backup = home.path().join("restore.jsonl");
        let engine = SqliteMemoryEngine::in_memory().unwrap()
            .with_embedder(std::sync::Arc::new(NgramHashEmbedder::new()));
        seed(&engine, "m", "用户记忆", Some(crate::vector::NGRAM_MODEL_ID)).await;
        {
            let conn = engine.conn.lock().await;
            conn.execute_batch("INSERT INTO key_facts(id, agent_id, fact, timestamp) VALUES ('k', 'a', '用户事实', '2026-11-21');
                INSERT INTO key_facts_fts(rowid, fact) VALUES (last_insert_rowid(), '用户事实');").unwrap();
        }
        engine.convert_memory_script_with_backup("a", false, &backup).await.unwrap();
        let records: Vec<RestoreRow> = std::fs::read_to_string(&backup).unwrap().lines().map(|s| serde_json::from_str(s).unwrap()).collect();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].before, "用户事实");
        assert_eq!(records[0].after_hash, text_hash("用戶事實"));
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&backup).unwrap().permissions().mode() & 0o777, 0o600);
        }
        {
            let conn = engine.conn.lock().await;
            conn.execute("UPDATE memories SET content = 'edited' WHERE id = 'm'", []).unwrap();
        }
        assert!(engine.restore_memory_script(&backup).await.is_err());
        {
            let conn = engine.conn.lock().await;
            let fact: String = conn.query_row("SELECT fact FROM key_facts WHERE id = 'k'", [], |r| r.get(0)).unwrap();
            assert_eq!(fact, "用戶事實", "earlier restore writes roll back on a later conflict");
            conn.execute("UPDATE memories SET content = '用戶記憶' WHERE id = 'm'", []).unwrap();
        }
        assert_eq!(engine.restore_memory_script(&backup).await.unwrap().changed, 2);
        let conn = engine.conn.lock().await;
        let (text, embedding): (String, Vec<u8>) = conn.query_row("SELECT content, embedding FROM memories WHERE id = 'm'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(text, "用户记忆");
        assert_eq!(embedding, crate::vector::encode_vec(&NgramHashEmbedder::new().embed(&text).unwrap()));
        let fts: String = conn.query_row("SELECT content FROM memories_fts WHERE memory_id = 'm'", [], |r| r.get(0)).unwrap();
        assert_eq!(fts, text);
        let fact: String = conn.query_row("SELECT fact FROM key_facts_fts LIMIT 1", [], |r| r.get(0)).unwrap();
        assert_eq!(fact, "用户事实");
    }

    #[tokio::test]
    async fn conversion_batches_more_than_256_rows_and_updates_fts() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        for i in 0..520 { seed(&engine, &format!("m{i}"), "用户记忆", None).await; }
        let report = engine.convert_memory_script("a", false).await.unwrap();
        assert_eq!(report.changed, 520);
        let conn = engine.conn.lock().await;
        let count: i64 = conn.query_row("SELECT count(*) FROM memories_fts WHERE content = '用戶記憶'", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 520);
    }

    #[tokio::test]
    async fn conversion_and_restore_preserve_conversation_lineage_and_visibility() {
        fn snapshot(conn: &rusqlite::Connection, sql: &str) -> Vec<Vec<rusqlite::types::Value>> {
            let mut stmt = conn.prepare(sql).unwrap();
            let columns = stmt.column_count();
            stmt.query_map([], |row| (0..columns).map(|i| row.get(i)).collect())
                .unwrap().collect::<std::result::Result<Vec<_>, _>>().unwrap()
        }
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let backup = dir.path().join("restore.jsonl");
        let ids = vec!["one".to_string(), "two".to_string()];
        for (id, session) in [("one", "telegram:A"), ("two", "telegram:B")] {
            seed(&engine, id, "用户记忆", None).await;
            let conn = engine.conn.lock().await;
            conn.execute("UPDATE memories SET source_event = 'conversation_distill' WHERE id = ?1", params![id]).unwrap();
            conn.execute("INSERT INTO key_facts(id, agent_id, fact, timestamp) VALUES (?1, 'a', '用户事实', '2026-11-21')", params![id]).unwrap();
            conn.execute("INSERT INTO key_facts_fts(rowid, fact) VALUES (last_insert_rowid(), '用户事实')", []).unwrap();
            for table in ["memories", "key_facts"] {
                conn.execute("INSERT INTO memory_origins(memory_store, memory_id, agent_id, source_kind, source_session, source_observed_at, role, created_at)
                    VALUES (?1, ?2, 'a', ?3, ?4, '2026-11-21', 'direct', '2026-11-21')",
                    params![table, id, crate::lineage::SourceKind::ChannelMessage.as_str(), session]).unwrap();
            }
        }
        let origins_sql = "SELECT * FROM memory_origins ORDER BY memory_store, memory_id, source_session, source_message";
        let metadata_sql = "SELECT id, agent_id, timestamp, source_event FROM memories ORDER BY id";
        let (origins, metadata) = {
            let conn = engine.conn.lock().await;
            (snapshot(&conn, origins_sql), snapshot(&conn, metadata_sql))
        };
        // The operator converts both conversations, retaining each reader's boundary.
        assert_eq!(engine.convert_memory_script_with_backup("a", false, &backup).await.unwrap().changed, 4);
        for restored in [false, true] {
            if restored { assert_eq!(engine.restore_memory_script(&backup).await.unwrap().changed, 4); }
            {
                let conn = engine.conn.lock().await;
                assert_eq!(snapshot(&conn, origins_sql), origins);
                assert_eq!(snapshot(&conn, metadata_sql), metadata);
                let expected = if restored { "用户记忆" } else { "用戶記憶" };
                let count: i64 = conn.query_row("SELECT count(*) FROM memories_fts WHERE content = ?1", params![expected], |r| r.get(0)).unwrap();
                assert_eq!(count, 2);
            }
            for (session, hidden) in [(Some("telegram:A"), vec!["two"]), (Some("telegram:B"), vec!["one"]), (None, vec!["one", "two"])] {
                let actual = engine.ids_hidden_from_conversation("a", &ids, session).await.unwrap();
                assert_eq!(actual.len(), hidden.len());
                for id in hidden { assert!(actual.contains(id)); }
            }
        }
    }

    #[tokio::test]
    async fn backup_creation_failure_prevents_any_conversion() {
        let dir = tempfile::tempdir().unwrap();
        let backup = dir.path().join("exists.jsonl");
        std::fs::write(&backup, "keep").unwrap();
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        seed(&engine, "m", "用户", None).await;
        assert!(engine.convert_memory_script_with_backup("a", false, &backup).await.is_err());
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), "keep");
        let conn = engine.conn.lock().await;
        let text: String = conn.query_row("SELECT content FROM memories WHERE id = 'm'", [], |r| r.get(0)).unwrap();
        assert_eq!(text, "用户");
    }
}
