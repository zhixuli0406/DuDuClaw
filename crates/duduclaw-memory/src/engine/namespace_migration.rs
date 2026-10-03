//! Moving memory rows between namespaces (v1.68.0 namespace unification).
//!
//! Before v1.68.0 every gateway-spawned employee wrote its MCP memory-tool
//! rows into one shared namespace (`internal/gateway-internal`). Those rows
//! are never moved automatically; an operator moves them with
//! `duduclaw memory migrate-namespace`, which drives the primitives here.
//!
//! A move re-keys a row in place — its id never changes, so every
//! `supersedes` / `superseded_by` pointer still names the same row. A row
//! that would become a current fact in the target namespace is judged by the
//! supersession trust guard exactly like a new write there:
//!
//! * the same value is already current in the target → the row moves as
//!   expired history pointing at the surviving fact (which is reaffirmed);
//! * it predates the target's reigning fact → it moves as a bounded
//!   historical segment;
//! * the target's current fact is strictly more trusted → refused: either
//!   held for human review in the target (converted in place into a held
//!   claim, the same shape a burst release produces) or left where it is;
//! * otherwise it supersedes the target's current fact(s).
//!
//! Chains that straddle namespaces: pointers are ids and are left as they
//! are. When a moved row supersedes target rows and already carries a
//! `supersedes` pointer from its old chain, that pointer is kept and the
//! superseded target ids are recorded in `metadata.namespace_migration`.
//! `get_history` filters by namespace, so each side shows only its own rows.

use super::*;

/// One row of a namespace, all columns except the embedding vector.
#[derive(Debug, Clone, Serialize)]
pub struct NamespaceRow {
    pub id: String,
    pub agent_id: String,
    pub content: String,
    pub timestamp: String,
    pub tags: Vec<String>,
    pub layer: String,
    pub importance: f64,
    pub access_count: i64,
    pub last_accessed: Option<String>,
    pub source_event: Option<String>,
    pub valid_from: Option<String>,
    pub valid_until: Option<String>,
    pub superseded_by: Option<String>,
    pub supersedes: Option<String>,
    pub subject: Option<String>,
    pub predicate: Option<String>,
    pub object: Option<String>,
    pub confidence: f64,
    pub metadata: serde_json::Value,
    pub origin: Option<String>,
    pub origin_trust: f64,
    pub derived_from: Option<String>,
    pub ingested_at: Option<String>,
    pub invalidated_by_event: Option<String>,
    pub invalidated_at: Option<String>,
    pub quarantined: bool,
}

/// What to do with a row the supersession guard refuses in the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnRefused {
    /// Convert it into a held claim in the target for human review. A row
    /// longer than `max_chars` cannot be shown in full on a review card and is
    /// left in place instead.
    Hold { max_chars: usize },
    /// Leave it in the source namespace.
    Skip,
}

/// What a move did (or, in a dry run, would do) to one row.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "disposition", rename_all = "snake_case")]
pub enum MigrationDisposition {
    /// Re-keyed; it was not a current fact (no triple, or already expired).
    Moved,
    /// Re-keyed as a current fact; nothing in the target competed with it.
    MovedCurrent,
    /// Re-keyed as the current fact, superseding these target rows.
    MovedSuperseding { superseded: Vec<String> },
    /// Re-keyed as a bounded historical segment (it predates the target's
    /// current fact).
    MovedAsHistory,
    /// The same value was already current in the target: re-keyed as expired
    /// history pointing at that row, which is reaffirmed.
    MovedDuplicate { of: String },
    /// Refused by the guard and converted into a held claim in the target.
    Held { refusal: crate::supersession_guard::SupersessionRefusal },
    /// Refused, and an identical claim is already held in the target:
    /// re-keyed as closed history.
    DuplicateOfHeld { held_id: String },
    /// Refused by the guard and left in the source namespace.
    Refused {
        refusal: crate::supersession_guard::SupersessionRefusal,
        /// `too_long` when holding was asked for but the statement is too
        /// long for a review card; `None` when skipping was asked for.
        not_held_reason: Option<String>,
    },
    /// A quarantined / held row in the source: left in place (its review
    /// card names the source namespace).
    SkippedQuarantined,
    /// Already in the target namespace (a re-run).
    AlreadyInTarget,
    /// No row with this id in the source namespace.
    NotFound,
}

impl MigrationDisposition {
    /// Whether the row now lives (or would live) in the target namespace.
    pub fn moved(&self) -> bool {
        matches!(
            self,
            Self::Moved
                | Self::MovedCurrent
                | Self::MovedSuperseding { .. }
                | Self::MovedAsHistory
                | Self::MovedDuplicate { .. }
                | Self::Held { .. }
                | Self::DuplicateOfHeld { .. }
        )
    }
}

const ROW_COLUMNS: &str = "id, agent_id, content, timestamp, tags, layer, importance,
     access_count, last_accessed, source_event, valid_from, valid_until,
     superseded_by, supersedes, subject, predicate, object, confidence,
     metadata, origin, origin_trust, derived_from, ingested_at,
     invalidated_by_event, invalidated_at, quarantined";

fn row_to_namespace_row(r: &rusqlite::Row<'_>) -> std::result::Result<NamespaceRow, rusqlite::Error> {
    let tags_raw: Option<String> = r.get(4)?;
    let metadata_raw: Option<String> = r.get(18)?;
    Ok(NamespaceRow {
        id: r.get(0)?,
        agent_id: r.get(1)?,
        content: r.get(2)?,
        timestamp: r.get(3)?,
        tags: tags_raw
            .as_deref()
            .and_then(|t| serde_json::from_str(t).ok())
            .unwrap_or_default(),
        layer: r.get(5)?,
        importance: r.get::<_, Option<f64>>(6)?.unwrap_or(0.0),
        access_count: r.get::<_, Option<i64>>(7)?.unwrap_or(0),
        last_accessed: r.get(8)?,
        source_event: r.get(9)?,
        valid_from: r.get(10)?,
        valid_until: r.get(11)?,
        superseded_by: r.get(12)?,
        supersedes: r.get(13)?,
        subject: r.get(14)?,
        predicate: r.get(15)?,
        object: r.get(16)?,
        confidence: r.get::<_, Option<f64>>(17)?.unwrap_or(1.0),
        metadata: metadata_raw
            .as_deref()
            .and_then(|m| serde_json::from_str(m).ok())
            .unwrap_or_else(|| serde_json::json!({})),
        origin: r.get(19)?,
        origin_trust: r.get::<_, Option<f64>>(20)?.unwrap_or(1.0),
        derived_from: r.get(21)?,
        ingested_at: r.get(22)?,
        invalidated_by_event: r.get(23)?,
        invalidated_at: r.get(24)?,
        quarantined: r.get::<_, Option<i64>>(25)?.unwrap_or(0) != 0,
    })
}

fn mem_err(e: impl std::fmt::Display) -> DuDuClawError {
    DuDuClawError::Memory(e.to_string())
}

/// `metadata` with a `namespace_migration` marker added (an unparseable or
/// non-object metadata string is kept as it is).
fn with_migration_marker(
    metadata: &serde_json::Value,
    from: &str,
    to: &str,
    at: &str,
    superseded: &[String],
) -> serde_json::Value {
    let mut m = metadata.clone();
    if let Some(obj) = m.as_object_mut() {
        let mut marker = serde_json::json!({ "from": from, "to": to, "at": at });
        if !superseded.is_empty() {
            marker["superseded_in_target"] = serde_json::json!(superseded);
        }
        obj.insert("namespace_migration".to_string(), marker);
    }
    m
}

impl SqliteMemoryEngine {
    /// Every row of `namespace` (valid or not), oldest first.
    pub async fn list_namespace_rows(&self, namespace: &str) -> Result<Vec<NamespaceRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {ROW_COLUMNS} FROM memories WHERE agent_id = ?1
                 ORDER BY COALESCE(valid_from, timestamp) ASC, timestamp ASC, id ASC"
            ))
            .map_err(mem_err)?;
        let rows = stmt
            .query_map(params![namespace], row_to_namespace_row)
            .map_err(mem_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(mem_err)?);
        }
        Ok(out)
    }

    /// Entity aliases registered under `namespace` as `(canonical, alias)`.
    pub async fn namespace_alias_count(&self, namespace: &str) -> Result<usize> {
        Ok(self.list_entity_aliases(namespace).await?.len())
    }

    /// Move rows `ids` from namespace `from` to `to` (see the module doc).
    ///
    /// Runs in one `BEGIN IMMEDIATE` transaction; `dry_run` computes every
    /// disposition and rolls back, so the plan printed is exactly what a real
    /// run would do against the current database. Rows are processed oldest
    /// world-time first. Idempotent: a row already in `to` reports
    /// [`MigrationDisposition::AlreadyInTarget`]. With `move_aliases`, the
    /// source's entity aliases are copied into the target (a target alias
    /// with the same surface form wins) and removed from the source; the
    /// number copied is returned alongside the dispositions.
    pub async fn migrate_namespace_rows(
        &self,
        from: &str,
        to: &str,
        ids: &[String],
        on_refused: OnRefused,
        move_aliases: bool,
        dry_run: bool,
    ) -> Result<(Vec<(String, MigrationDisposition)>, usize)> {
        if from == to {
            return Err(DuDuClawError::Memory(
                "migrate_namespace_rows: from and to are identical".to_string(),
            ));
        }
        let conn = self.conn.lock().await;
        conn.execute_batch("BEGIN IMMEDIATE").map_err(mem_err)?;
        let work = (|| -> Result<(Vec<(String, MigrationDisposition)>, usize)> {
            // Oldest world-time first so a chain's older rows land before the
            // row that is current.
            let mut ordered: Vec<(String, String)> = Vec::new();
            for id in ids {
                let key: Option<String> = conn
                    .query_row(
                        "SELECT COALESCE(valid_from, timestamp) FROM memories WHERE id = ?1",
                        params![id],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(mem_err)?;
                ordered.push((key.unwrap_or_default(), id.clone()));
            }
            ordered.sort();
            ordered.dedup_by(|a, b| a.1 == b.1);
            let now = Utc::now().to_rfc3339();
            let mut out = Vec::new();
            for (_, id) in ordered {
                let d = self.migrate_one_locked(&conn, from, to, &id, on_refused, &now)?;
                out.push((id, d));
            }
            let aliases = if move_aliases {
                let copied = conn
                    .execute(
                        "INSERT OR IGNORE INTO entity_alias (agent_id, canonical, alias, created_at)
                         SELECT ?2, canonical, alias, created_at FROM entity_alias WHERE agent_id = ?1",
                        params![from, to],
                    )
                    .map_err(mem_err)?;
                conn.execute("DELETE FROM entity_alias WHERE agent_id = ?1", params![from])
                    .map_err(mem_err)?;
                copied
            } else {
                0
            };
            Ok((out, aliases))
        })();
        let finish = if dry_run || work.is_err() { "ROLLBACK" } else { "COMMIT" };
        if let Err(e) = conn.execute_batch(finish) {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(DuDuClawError::Memory(format!("{finish} failed: {e}")));
        }
        drop(conn);
        if !dry_run && work.is_ok() {
            self.bump_graph_generation(from);
            self.bump_graph_generation(to);
        }
        work
    }

    fn rekey_locked(
        conn: &Connection,
        from: &str,
        to: &str,
        id: &str,
        metadata: &serde_json::Value,
    ) -> Result<()> {
        conn.execute(
            "UPDATE memories SET agent_id = ?1, metadata = ?2 WHERE id = ?3 AND agent_id = ?4",
            params![to, metadata.to_string(), id, from],
        )
        .map_err(mem_err)?;
        conn.execute(
            "UPDATE memories_fts SET agent_id = ?1 WHERE memory_id = ?2 AND agent_id = ?3",
            params![to, id, from],
        )
        .map_err(mem_err)?;
        Ok(())
    }

    fn migrate_one_locked(
        &self,
        conn: &Connection,
        from: &str,
        to: &str,
        id: &str,
        on_refused: OnRefused,
        now: &str,
    ) -> Result<MigrationDisposition> {
        let row: Option<NamespaceRow> = conn
            .query_row(
                &format!("SELECT {ROW_COLUMNS} FROM memories WHERE id = ?1 AND agent_id = ?2"),
                params![id, from],
                row_to_namespace_row,
            )
            .optional()
            .map_err(mem_err)?;
        let Some(row) = row else {
            let in_target: Option<i64> = conn
                .query_row(
                    "SELECT 1 FROM memories WHERE id = ?1 AND agent_id = ?2",
                    params![id, to],
                    |r| r.get(0),
                )
                .optional()
                .map_err(mem_err)?;
            return Ok(if in_target.is_some() {
                MigrationDisposition::AlreadyInTarget
            } else {
                MigrationDisposition::NotFound
            });
        };
        if row.quarantined {
            return Ok(MigrationDisposition::SkippedQuarantined);
        }

        let (subj, pred) = match (&row.subject, &row.predicate) {
            (Some(s), Some(p)) if row.valid_until.is_none() => (s.clone(), p.clone()),
            _ => {
                let meta = with_migration_marker(&row.metadata, from, to, now, &[]);
                Self::rekey_locked(conn, from, to, id, &meta)?;
                return Ok(MigrationDisposition::Moved);
            }
        };

        let active = Self::load_active_triple(conn, to, &subj, &pred)?;
        let clean: Vec<&ActiveTriple> = active.iter().filter(|r| !r.quarantined).collect();
        let origin_name = row
            .origin
            .clone()
            .unwrap_or_else(|| crate::origin::UNATTRIBUTED.name.to_string());

        // Same value already current in the target → this row is history.
        if let Some(survivor) = clean
            .iter()
            .find(|r| object_opt_eq(&row.object, &r.object) && r.content.trim() == row.content.trim())
        {
            let source_event = row.source_event.clone().unwrap_or_default();
            let new_meta = append_reaffirmed_by(&survivor.metadata, &source_event, &origin_name);
            conn.execute(
                "UPDATE memories SET metadata = ?1, access_count = access_count + 1
                 WHERE id = ?2 AND agent_id = ?3",
                params![new_meta, survivor.id, to],
            )
            .map_err(mem_err)?;
            let meta = with_migration_marker(&row.metadata, from, to, now, &[]);
            Self::rekey_locked(conn, from, to, id, &meta)?;
            conn.execute(
                "UPDATE memories
                 SET valid_until = ?1, superseded_by = ?2,
                     invalidated_by_event = 'namespace_migration_duplicate', invalidated_at = ?1
                 WHERE id = ?3 AND agent_id = ?4",
                params![now, survivor.id, id, to],
            )
            .map_err(mem_err)?;
            return Ok(MigrationDisposition::MovedDuplicate { of: survivor.id.clone() });
        }

        // Older than the target's reigning fact → a bounded historical segment.
        let row_vf = row
            .valid_from
            .as_deref()
            .or(Some(row.timestamp.as_str()))
            .and_then(parse_rfc3339);
        if Self::predates_reigning(row_vf, &clean) {
            let bound = row_vf.and_then(|vf| {
                clean
                    .iter()
                    .filter_map(|r| r.valid_from.as_deref().and_then(parse_rfc3339))
                    .filter(|dt| *dt > vf)
                    .min()
                    .map(|dt| dt.to_rfc3339())
            });
            let meta = with_migration_marker(&row.metadata, from, to, now, &[]);
            Self::rekey_locked(conn, from, to, id, &meta)?;
            conn.execute(
                "UPDATE memories SET valid_until = ?1 WHERE id = ?2 AND agent_id = ?3",
                params![bound, id, to],
            )
            .map_err(mem_err)?;
            return Ok(MigrationDisposition::MovedAsHistory);
        }

        // The supersession trust guard, exactly as for a new write.
        if self.supersession_trust_guard {
            let trust = crate::supersession_guard::existing_fact_trust(
                row.origin_trust,
                row.origin.as_deref(),
            );
            if let Some(refusal) = Self::guard_verdict(&subj, &pred, &origin_name, trust, &active) {
                self.supersession_refusals
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let max_chars = match on_refused {
                    OnRefused::Skip => {
                        return Ok(MigrationDisposition::Refused { refusal, not_held_reason: None });
                    }
                    OnRefused::Hold { max_chars } => max_chars,
                };
                if row.content.chars().count() > max_chars {
                    return Ok(MigrationDisposition::Refused {
                        refusal,
                        not_held_reason: Some("too_long".to_string()),
                    });
                }
                if let Some(held_id) = Self::find_pending_held_claim_locked(
                    conn,
                    to,
                    &subj,
                    &pred,
                    row.object.as_deref(),
                )? {
                    let meta = with_migration_marker(&row.metadata, from, to, now, &[]);
                    Self::rekey_locked(conn, from, to, id, &meta)?;
                    conn.execute(
                        "UPDATE memories
                         SET valid_until = ?1,
                             invalidated_by_event = 'namespace_migration_duplicate_held',
                             invalidated_at = ?1
                         WHERE id = ?2 AND agent_id = ?3",
                        params![now, id, to],
                    )
                    .map_err(mem_err)?;
                    return Ok(MigrationDisposition::DuplicateOfHeld { held_id });
                }
                let mut meta = with_migration_marker(&row.metadata, from, to, now, &[]);
                if let Some(obj) = meta.as_object_mut() {
                    obj.insert(
                        "held_claim".to_string(),
                        serde_json::json!({
                            "subject": subj,
                            "predicate": pred,
                            "object": row.object,
                            "conflicts_with": refusal.existing_id,
                            "held_at": now,
                        }),
                    );
                    obj.insert("held_from_migration".to_string(), serde_json::json!(true));
                } else {
                    meta = serde_json::json!({
                        "held_claim": {
                            "subject": subj,
                            "predicate": pred,
                            "object": row.object,
                            "conflicts_with": refusal.existing_id,
                            "held_at": now,
                        },
                        "held_from_migration": true,
                    });
                }
                Self::rekey_locked(conn, from, to, id, &meta)?;
                conn.execute(
                    "UPDATE memories SET subject = NULL, predicate = NULL, object = NULL,
                                         quarantined = 1
                     WHERE id = ?1 AND agent_id = ?2",
                    params![id, to],
                )
                .map_err(mem_err)?;
                return Ok(MigrationDisposition::Held { refusal });
            }
        }

        // Supersede the target's current fact(s), like any write.
        let superseded: Vec<String> = clean.iter().map(|r| r.id.clone()).collect();
        for r in &clean {
            conn.execute(
                "UPDATE memories
                 SET valid_until = ?1, superseded_by = ?2,
                     invalidated_by_event = 'namespace_migration', invalidated_at = ?1
                 WHERE id = ?3",
                params![now, id, r.id],
            )
            .map_err(mem_err)?;
        }
        let meta = with_migration_marker(&row.metadata, from, to, now, &superseded);
        Self::rekey_locked(conn, from, to, id, &meta)?;
        if let Some(first) = clean.first() {
            conn.execute(
                "UPDATE memories SET supersedes = COALESCE(supersedes, ?1)
                 WHERE id = ?2 AND agent_id = ?3",
                params![first.id, id, to],
            )
            .map_err(mem_err)?;
        }
        Ok(if superseded.is_empty() {
            MigrationDisposition::MovedCurrent
        } else {
            MigrationDisposition::MovedSuperseding { superseded }
        })
    }

    /// Expire every currently valid row of `namespace` (`valid_until = now`,
    /// `invalidated_by_event = 'namespace_archived'`) and tag it `tag`, so it
    /// stops matching reads but stays for history. Returns the number of rows
    /// archived (computed and rolled back on `dry_run`). Idempotent.
    pub async fn archive_namespace(&self, namespace: &str, tag: &str, dry_run: bool) -> Result<usize> {
        let conn = self.conn.lock().await;
        conn.execute_batch("BEGIN IMMEDIATE").map_err(mem_err)?;
        let work = (|| -> Result<usize> {
            let mut stmt = conn
                .prepare("SELECT id, tags FROM memories WHERE agent_id = ?1 AND valid_until IS NULL")
                .map_err(mem_err)?;
            let rows: Vec<(String, Option<String>)> = stmt
                .query_map(params![namespace], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(mem_err)?
                .collect::<std::result::Result<_, _>>()
                .map_err(mem_err)?;
            drop(stmt);
            let now = Utc::now().to_rfc3339();
            for (id, tags) in &rows {
                let mut list: Vec<String> = tags
                    .as_deref()
                    .and_then(|t| serde_json::from_str(t).ok())
                    .unwrap_or_default();
                if !list.iter().any(|t| t == tag) {
                    list.push(tag.to_string());
                }
                let tags_json = serde_json::to_string(&list).map_err(mem_err)?;
                conn.execute(
                    "UPDATE memories
                     SET valid_until = ?1, invalidated_by_event = 'namespace_archived',
                         invalidated_at = ?1, tags = ?2
                     WHERE id = ?3 AND agent_id = ?4",
                    params![now, tags_json, id, namespace],
                )
                .map_err(mem_err)?;
            }
            Ok(rows.len())
        })();
        let finish = if dry_run || work.is_err() { "ROLLBACK" } else { "COMMIT" };
        if let Err(e) = conn.execute_batch(finish) {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(DuDuClawError::Memory(format!("{finish} failed: {e}")));
        }
        drop(conn);
        if !dry_run && work.is_ok() {
            self.bump_graph_generation(namespace);
        }
        work
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::TemporalMeta;
    use duduclaw_core::types::MemoryLayer;

    const POOL: &str = "internal/gateway-internal";

    fn entry(content: &str) -> MemoryEntry {
        MemoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: String::new(),
            content: content.to_string(),
            timestamp: Utc::now(),
            tags: vec![],
            embedding: None,
            layer: MemoryLayer::Semantic,
            importance: 5.0,
            access_count: 0,
            last_accessed: None,
            source_event: "mcp_internal".to_string(),
        }
    }

    fn triple(s: &str, p: &str, o: &str, origin: &str) -> TemporalMeta {
        TemporalMeta {
            subject: Some(s.into()),
            predicate: Some(p.into()),
            object: Some(o.into()),
            origin: Some(origin.into()),
            ..Default::default()
        }
    }

    async fn store(e: &SqliteMemoryEngine, ns: &str, content: &str, meta: TemporalMeta) -> String {
        e.store_temporal(ns, entry(content), meta).await.unwrap()
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[tokio::test]
    async fn plain_rows_move_and_rerun_is_idempotent() {
        let e = SqliteMemoryEngine::in_memory().unwrap();
        let a = store(&e, POOL, "likes tea", TemporalMeta::default()).await;
        let (plan, _) = e
            .migrate_namespace_rows(POOL, "agnes", &ids(&[&a]), OnRefused::Skip, false, true)
            .await
            .unwrap();
        assert_eq!(plan[0].1, MigrationDisposition::Moved);
        // Dry run changed nothing.
        assert_eq!(e.list_namespace_rows(POOL).await.unwrap().len(), 1);
        let (done, _) = e
            .migrate_namespace_rows(POOL, "agnes", &ids(&[&a]), OnRefused::Skip, false, false)
            .await
            .unwrap();
        assert_eq!(done[0].1, MigrationDisposition::Moved);
        assert!(e.list_namespace_rows(POOL).await.unwrap().is_empty());
        let moved = e.get_by_id("agnes", &a).await.unwrap().expect("readable in target");
        assert_eq!(moved.content, "likes tea");
        // FTS follows the row.
        assert_eq!(e.search("agnes", "tea", 5).await.unwrap().len(), 1);
        let (again, _) = e
            .migrate_namespace_rows(POOL, "agnes", &ids(&[&a]), OnRefused::Skip, false, false)
            .await
            .unwrap();
        assert_eq!(again[0].1, MigrationDisposition::AlreadyInTarget);
    }

    #[tokio::test]
    async fn equal_trust_row_supersedes_target_fact() {
        let e = SqliteMemoryEngine::in_memory().unwrap();
        let old = store(&e, "agnes", "tz Taipei", triple("user:1", "tz", "Taipei", "agent_derived")).await;
        let moved = store(&e, POOL, "tz Tokyo", triple("user:1", "tz", "Tokyo", "user_profile")).await;
        let (d, _) = e
            .migrate_namespace_rows(POOL, "agnes", &ids(&[&moved]), OnRefused::Skip, false, false)
            .await
            .unwrap();
        assert_eq!(d[0].1, MigrationDisposition::MovedSuperseding { superseded: vec![old.clone()] });
        let hist = e.get_history("agnes", "user:1", "tz").await.unwrap();
        assert_eq!(hist.len(), 2);
    }

    #[tokio::test]
    async fn guard_refusal_holds_or_skips() {
        let e = SqliteMemoryEngine::in_memory().unwrap();
        let protected = TemporalMeta {
            origin_trust: Some(1.0),
            ..triple("user:1", "tz", "Taipei", "operator")
        };
        let op = store(&e, "agnes", "tz Taipei", protected).await;
        let low = store(&e, POOL, "tz Tokyo", triple("user:1", "tz", "Tokyo", "user_profile")).await;

        let (skip, _) = e
            .migrate_namespace_rows(POOL, "agnes", &ids(&[&low]), OnRefused::Skip, false, false)
            .await
            .unwrap();
        assert!(matches!(skip[0].1, MigrationDisposition::Refused { .. }));
        assert_eq!(e.list_namespace_rows(POOL).await.unwrap().len(), 1, "skip leaves it in place");

        let (held, _) = e
            .migrate_namespace_rows(POOL, "agnes", &ids(&[&low]), OnRefused::Hold { max_chars: 600 }, false, false)
            .await
            .unwrap();
        assert!(matches!(held[0].1, MigrationDisposition::Held { .. }));
        let view = e.held_claim_view("agnes", &low).await.unwrap().expect("held claim");
        assert_eq!(view.conflicts_with.as_deref(), Some(op.as_str()));
        assert_eq!(view.object.as_deref(), Some("Tokyo"));
        // The operator fact is still current.
        let at = e.get_at("agnes", "user:1", "tz", Utc::now()).await.unwrap().unwrap();
        assert_eq!(at.id, op);
    }

    #[tokio::test]
    async fn too_long_refusal_is_left_in_place() {
        let e = SqliteMemoryEngine::in_memory().unwrap();
        let protected = TemporalMeta { origin_trust: Some(1.0), ..triple("s", "p", "a", "operator") };
        store(&e, "agnes", "a", protected).await;
        let long = "x".repeat(700);
        let low = store(&e, POOL, &long, triple("s", "p", "b", "agent_derived")).await;
        let (d, _) = e
            .migrate_namespace_rows(POOL, "agnes", &ids(&[&low]), OnRefused::Hold { max_chars: 600 }, false, false)
            .await
            .unwrap();
        assert!(matches!(
            &d[0].1,
            MigrationDisposition::Refused { not_held_reason: Some(r), .. } if r == "too_long"
        ));
    }

    #[tokio::test]
    async fn chain_moves_whole_and_duplicate_reaffirms() {
        let e = SqliteMemoryEngine::in_memory().unwrap();
        let first = store(&e, POOL, "tz A", triple("u", "tz", "A", "agent_derived")).await;
        let second = store(&e, POOL, "tz B", triple("u", "tz", "B", "agent_derived")).await;
        // Target already holds the current value B (distilled).
        let survivor = store(&e, "agnes", "tz B", triple("u", "tz", "B", "agent_derived")).await;
        let (d, _) = e
            .migrate_namespace_rows(POOL, "agnes", &ids(&[&second, &first]), OnRefused::Skip, false, false)
            .await
            .unwrap();
        let by_id: std::collections::HashMap<_, _> = d.into_iter().collect();
        assert_eq!(by_id[&first], MigrationDisposition::Moved, "expired history just moves");
        assert_eq!(by_id[&second], MigrationDisposition::MovedDuplicate { of: survivor.clone() });
        let at = e.get_at("agnes", "u", "tz", Utc::now()).await.unwrap().unwrap();
        assert_eq!(at.id, survivor);
        // The moved chain keeps its pointer.
        let rows = e.list_namespace_rows("agnes").await.unwrap();
        let second_row = rows.iter().find(|r| r.id == second).unwrap();
        assert_eq!(second_row.supersedes.as_deref(), Some(first.as_str()));
    }

    #[tokio::test]
    async fn archive_expires_and_tags() {
        let e = SqliteMemoryEngine::in_memory().unwrap();
        store(&e, POOL, "left behind", TemporalMeta::default()).await;
        assert_eq!(e.archive_namespace(POOL, "ns-archived", true).await.unwrap(), 1);
        assert_eq!(e.search(POOL, "behind", 5).await.unwrap().len(), 1, "dry run kept it");
        assert_eq!(e.archive_namespace(POOL, "ns-archived", false).await.unwrap(), 1);
        assert!(e.search(POOL, "behind", 5).await.unwrap().is_empty());
        let rows = e.list_namespace_rows(POOL).await.unwrap();
        assert!(rows[0].tags.iter().any(|t| t == "ns-archived"));
        assert_eq!(e.archive_namespace(POOL, "ns-archived", false).await.unwrap(), 0);
    }
}
