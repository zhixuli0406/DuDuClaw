//! Operator-driven, character-level script conversion. No model is involved.
use super::*;

#[derive(Debug, Default)]
pub struct ScriptConversionReport {
    pub key_facts: usize,
    pub memories: usize,
    pub changed: usize,
    pub embeddings_cleared: usize,
    pub samples: Vec<(String, String, String)>,
}

impl SqliteMemoryEngine {
    /// Namespaces in either text table, including legacy namespaces without an agent directory.
    pub async fn script_conversion_agents(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().await;
        crate::isolation_io::verify()?;
        let mut stmt = conn.prepare("SELECT agent_id FROM memories UNION SELECT agent_id FROM key_facts ORDER BY agent_id")
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        let rows = stmt.query_map([], |r| r.get(0))
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| DuDuClawError::Memory(e.to_string()))
    }

    /// One transaction per namespace, including both FTS tables. Ambiguous characters stay unchanged.
    /// Clear changed rows' embeddings: vectors for the old text must never remain searchable.
    /// The normal lazy embedding path can rebuild them using the configured provider.
    pub async fn convert_memory_script(&self, agent_id: &str, dry_run: bool) -> Result<ScriptConversionReport> {
        let conn = self.conn.lock().await;
        crate::isolation_io::verify()?;
        let convert = || -> Result<ScriptConversionReport> {
            if !dry_run { crate::label_gate::check(&conn, agent_id)?; }
            let mut report = ScriptConversionReport::default();
            for (table, column) in [("key_facts", "fact"), ("memories", "content")] {
                let mut stmt = conn.prepare(&format!("SELECT rowid, id, {column} FROM {table} WHERE agent_id = ?1 ORDER BY rowid"))
                    .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
                let rows = stmt.query_map(params![agent_id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
                    .map_err(|e| DuDuClawError::Memory(e.to_string()))?
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
                drop(stmt);
                for (rowid, id, before) in rows {
                    if !duduclaw_core::contains_simplified(&before) { continue; }
                    if table == "key_facts" { report.key_facts += 1; } else { report.memories += 1; }
                    let after = duduclaw_core::to_traditional(&before);
                    if report.samples.len() < 10 {
                        report.samples.push((table.to_string(), before.clone(), after.clone()));
                    }
                    if before == after { continue; }
                    report.changed += 1;
                    if dry_run { continue; }
                    let mut write = || -> rusqlite::Result<()> {
                        if table == "key_facts" {
                            conn.execute("UPDATE key_facts SET fact = ?1 WHERE rowid = ?2 AND agent_id = ?3", params![after, rowid, agent_id])?;
                            conn.execute("DELETE FROM key_facts_fts WHERE rowid = ?1", params![rowid])?;
                            conn.execute("INSERT INTO key_facts_fts(rowid, fact) VALUES (?1, ?2)", params![rowid, after])?;
                        } else {
                            let embedded: bool = conn.query_row("SELECT embedding IS NOT NULL FROM memories WHERE id = ?1 AND agent_id = ?2", params![id, agent_id], |r| r.get(0))?;
                            conn.execute("UPDATE memories SET content = ?1, embedding = NULL, embedding_model = NULL WHERE id = ?2 AND agent_id = ?3", params![after, id, agent_id])?;
                            conn.execute("DELETE FROM memories_fts WHERE memory_id = ?1", params![id])?;
                            conn.execute("INSERT INTO memories_fts(content, agent_id, memory_id) VALUES (?1, ?2, ?3)", params![after, agent_id, id])?;
                            if embedded { report.embeddings_cleared += 1; }
                        }
                        Ok(())
                    };
                    write().map_err(|e| DuDuClawError::Memory(e.to_string()))?;
                }
            }
            Ok(report)
        };
        if dry_run { convert() } else { Self::in_immediate_txn(&conn, convert) }
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
