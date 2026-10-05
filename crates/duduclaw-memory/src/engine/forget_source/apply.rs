//! The apply transaction of a forget plan.

use super::body::{self, Computation, Computed};
use super::*;
use crate::lineage::hooks::ApplyHookPoint;

fn e(x: impl std::fmt::Display) -> DuDuClawError {
    DuDuClawError::Memory(x.to_string())
}

/// Retire a planned plan as `stale` or `expired`. Its per-row content
/// hashes go too (M-2): a plan that will never run has no use for them, and
/// they could be matched against guessed text. The plan hash stays.
fn set_status(conn: &Connection, plan_id: &str, status: &str) -> Result<()> {
    let scrubbed = match SqliteMemoryEngine::load_plan(conn, plan_id)? {
        Some(p) => {
            let mut doc = p.document;
            for t in &mut doc.body.targets {
                t.content_sha256.clear();
            }
            Some(doc.canonical_json()?)
        }
        None => None,
    };
    match scrubbed {
        Some(json) => conn.execute(
            "UPDATE memory_forget_plans SET status = ?1, plan_json = ?3
             WHERE plan_id = ?2 AND status = 'planned'",
            params![status, plan_id, json],
        ),
        None => conn.execute(
            "UPDATE memory_forget_plans SET status = ?1 WHERE plan_id = ?2 AND status = 'planned'",
            params![status, plan_id],
        ),
    }
    .map_err(e)?;
    Ok(())
}

/// Counts-only difference between the planned and the recomputed targets.
fn diff(planned: &PlanBody, now: &PlanBody) -> StaleReason {
    use std::collections::HashMap;
    let key = |t: &TargetEntry| (t.store.clone(), t.id.clone());
    let a: HashMap<_, _> = planned.targets.iter().map(|t| (key(t), t)).collect();
    let b: HashMap<_, _> = now.targets.iter().map(|t| (key(t), t)).collect();
    let added = b.keys().filter(|k| !a.contains_key(*k)).count() as u64;
    let removed = a.keys().filter(|k| !b.contains_key(*k)).count() as u64;
    let changed = a
        .iter()
        .filter(|(k, t)| b.get(*k).is_some_and(|u| u != *t))
        .count() as u64;
    let mut other_parts = Vec::new();
    let mut part = |name: &str, same: bool| {
        if !same {
            other_parts.push(name.to_string());
        }
    };
    part("tombstones", planned.tombstones == now.tombstones);
    part("linked_turns", planned.linked_turns == now.linked_turns);
    part("reaffirm_only", planned.reaffirm_only == now.reaffirm_only);
    part("archive_ids", planned.archive_ids == now.archive_ids);
    part("lineage_only", planned.lineage_only == now.lineage_only);
    part("collateral", planned.collateral == now.collateral);
    part(
        "supersession_cuts",
        planned.supersession_cuts == now.supersession_cuts,
    );
    part(
        "entity_embeddings_orphaned",
        planned.entity_embeddings_orphaned == now.entity_embeddings_orphaned,
    );
    part("wiki_pages", planned.wiki_pages == now.wiki_pages);
    part(
        "review_cards_matching",
        planned.review_cards_matching == now.review_cards_matching,
    );
    part(
        "session_messages",
        planned.session_messages == now.session_messages,
    );
    part("steps", planned.steps == now.steps);
    part("not_covered", planned.not_covered == now.not_covered);
    StaleReason::Changed {
        targets_planned: planned.targets.len() as u64,
        targets_now: now.targets.len() as u64,
        added,
        removed,
        changed,
        other_parts,
    }
}

/// One reason, or [`StaleReason::Several`] when more than one holds.
fn combine(mut reasons: Vec<StaleReason>) -> StaleReason {
    if reasons.len() == 1 {
        reasons.remove(0)
    } else {
        StaleReason::Several { reasons }
    }
}

/// Run the whole apply on the locked connection. Returns the outcome and,
/// when something was applied, the namespace (for the graph-cache bump).
pub(super) fn apply_locked(
    engine: &SqliteMemoryEngine,
    conn: &Connection,
    plan_id: &str,
    external: &ExternalInputs,
    started: std::time::Instant,
) -> Result<(ApplyOutcome, Option<String>)> {
    let Some(plan) = SqliteMemoryEngine::load_plan(conn, plan_id)? else {
        return Ok((ApplyOutcome::NotFound, None));
    };
    match plan.status.as_str() {
        "applied" => return Ok((ApplyOutcome::AlreadyApplied, None)),
        "stale" => return Ok((ApplyOutcome::Stale(StaleReason::AlreadyStale), None)),
        "expired" => return Ok((ApplyOutcome::Expired, None)),
        _ => {}
    }
    let doc = &plan.document;
    if doc.db_instance_id != crate::lineage::db::db_instance_id(conn)? {
        return Ok((ApplyOutcome::DbMismatch, None));
    }
    let expired = parse_rfc3339(&doc.expires_at).is_none_or(|t| Utc::now() >= t);
    if expired {
        set_status(conn, plan_id, "expired")?;
        return Ok((ApplyOutcome::Expired, None));
    }

    engine.hooks.fire_apply(ApplyHookPoint::BeforeBegin)?;
    let prev_secure: i64 = conn
        .query_row("PRAGMA secure_delete", [], |r| r.get(0))
        .unwrap_or(0);
    conn.execute_batch("PRAGMA secure_delete = ON").map_err(e)?;
    let restore = |conn: &Connection| {
        let _ = conn.execute_batch(&format!("PRAGMA secure_delete = {prev_secure}"));
    };
    if let Err(err) = conn.execute_batch("BEGIN IMMEDIATE") {
        restore(conn);
        return Err(e(format!("forget apply could not start: {err}")));
    }

    let inner = (|| -> Result<std::result::Result<ApplyReport, StaleReason>> {
        // Every reason that holds is reported (not just the first): the
        // epoch, then whatever the recomputation finds.
        let mut reasons = Vec::new();
        let current = crate::lineage::db::forget_epoch(conn, &doc.agent_id)?;
        if current != doc.forget_epoch {
            reasons.push(StaleReason::Epoch {
                planned: doc.forget_epoch,
                current,
            });
        }
        let computed = match body::compute(
            conn,
            &doc.agent_id,
            &doc.selector,
            doc.limits.max_rows,
            external,
        )? {
            Computation::Ok(c) => Some(c),
            Computation::TooLarge(reason) => {
                reasons.push(StaleReason::TooLarge { reason });
                None
            }
        };
        if let Some(c) = &computed {
            let recomputed = PlanDocument {
                body: c.body.clone(),
                ..doc.clone()
            };
            if recomputed.plan_hash()? != plan.plan_hash {
                reasons.push(diff(&doc.body, &c.body));
            }
        }
        let computed = match computed {
            Some(c) if reasons.is_empty() => c,
            _ => return Ok(Err(combine(reasons))),
        };
        let report = write_apply(engine, conn, &plan, &computed)?;
        Ok(Ok(report))
    })();

    let outcome = match inner {
        Ok(Ok(mut report)) => {
            if let Err(err) = engine
                .hooks
                .fire_apply(ApplyHookPoint::BeforeCommit)
                .and_then(|()| conn.execute_batch("COMMIT").map_err(e))
            {
                let _ = conn.execute_batch("ROLLBACK");
                restore(conn);
                return Err(err);
            }
            restore(conn);
            engine.hooks.fire_apply(ApplyHookPoint::AfterCommit)?;
            housekeeping(conn);
            report.elapsed_ms = started.elapsed().as_millis() as u64;
            ApplyOutcome::Applied(report)
        }
        Ok(Err(reason)) => {
            let _ = conn.execute_batch("ROLLBACK");
            restore(conn);
            set_status(conn, plan_id, "stale")?;
            return Ok((ApplyOutcome::Stale(reason), None));
        }
        Err(err) => {
            let _ = conn.execute_batch("ROLLBACK");
            restore(conn);
            return Err(err);
        }
    };
    Ok((outcome, Some(doc.agent_id.clone())))
}

/// Best effort after commit: merge FTS segments and truncate the WAL so the
/// deleted text leaves the files sooner. Failures are only logged.
fn housekeeping(conn: &Connection) {
    for sql in [
        "INSERT INTO memories_fts(memories_fts) VALUES('optimize')",
        "INSERT INTO key_facts_fts(key_facts_fts) VALUES('optimize')",
    ] {
        if let Err(err) = conn.execute_batch(sql) {
            warn!("forget apply housekeeping failed: {err}");
        }
    }
    if let Err(err) = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(())) {
        warn!("forget apply WAL checkpoint failed: {err}");
    }
}

fn load_ids(conn: &Connection, c: &Computed) -> Result<()> {
    conn.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS forget_ids (store TEXT NOT NULL, id TEXT NOT NULL,
                                                    PRIMARY KEY (store, id));
         DELETE FROM temp.forget_ids;",
    )
    .map_err(e)?;
    let mut ins = conn
        .prepare_cached("INSERT OR IGNORE INTO temp.forget_ids (store, id) VALUES (?1, ?2)")
        .map_err(e)?;
    for id in &c.memory_ids {
        ins.execute(params!["memories", id]).map_err(e)?;
    }
    for id in &c.fact_ids {
        ins.execute(params!["key_facts", id]).map_err(e)?;
    }
    for id in &c.archive_ids {
        ins.execute(params!["archive", id]).map_err(e)?;
    }
    Ok(())
}

/// Every write of an apply, inside the caller's transaction.
fn write_apply(
    engine: &SqliteMemoryEngine,
    conn: &Connection,
    plan: &ForgetPlan,
    c: &Computed,
) -> Result<ApplyReport> {
    let doc = &plan.document;
    let agent = doc.agent_id.as_str();
    let now = format_ts(Utc::now());
    let mut report = ApplyReport {
        plan_id: plan.plan_id.clone(),
        agent_id: agent.to_string(),
        plan_hash: plan.plan_hash.clone(),
        untracked_in_namespace_planned: doc.body.untracked_in_namespace,
        untracked_in_namespace_at_apply: c.body.untracked_in_namespace,
        other_namespaces_referencing_planned: doc.body.other_namespaces_referencing,
        other_namespaces_referencing_at_apply: c.body.other_namespaces_referencing,
        ..Default::default()
    };

    // Tombstones first: from here on no write can carry the source.
    for t in &c.tombstones {
        report.tombstones_written += conn
            .execute(
                "INSERT OR IGNORE INTO forgotten_sources
                    (tombstone_id, agent_id, source_session, scope, source_message,
                     upto_seq, upto_time, source_digest, plan_id, forgotten_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    uuid::Uuid::new_v4().to_string(),
                    agent,
                    t.session,
                    t.scope,
                    t.message,
                    t.upto_seq,
                    t.upto_time,
                    t.digest(agent),
                    plan.plan_id,
                    now
                ],
            )
            .map_err(e)? as u64;
    }
    {
        let mut ins = conn
            .prepare_cached(
                "INSERT OR IGNORE INTO forgotten_memories
                    (memory_store, memory_id, agent_id, plan_id, forgotten_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )
            .map_err(e)?;
        let all = c
            .memory_ids
            .iter()
            .chain(c.archive_ids.iter())
            .map(|i| ("memories", i.as_str()))
            .chain(c.fact_ids.iter().map(|i| ("key_facts", i.as_str())))
            .chain(c.lineage_only.iter().map(|(s, i)| (s.as_str(), i.as_str())));
        for (store, id) in all {
            ins.execute(params![store, id, agent, plan.plan_id, now])
                .map_err(e)?;
        }
    }
    engine.hooks.fire_apply(ApplyHookPoint::AfterTombstones)?;

    load_ids(conn, c)?;
    conn.execute(
        "DELETE FROM memories_fts WHERE agent_id = ?1 AND memory_id IN
             (SELECT id FROM temp.forget_ids WHERE store IN ('memories', 'archive'))",
        params![agent],
    )
    .map_err(e)?;
    report.memories_deleted = conn
        .execute(
            "DELETE FROM memories WHERE agent_id = ?1 AND id IN
                 (SELECT id FROM temp.forget_ids WHERE store = 'memories')",
            params![agent],
        )
        .map_err(e)? as u64;
    if crate::lineage::db::table_exists(conn, "main", "memories_archive")? {
        report.archive_deleted = conn
            .execute(
                "DELETE FROM memories_archive WHERE agent_id = ?1 AND id IN
                     (SELECT id FROM temp.forget_ids WHERE store IN ('memories', 'archive'))",
                params![agent],
            )
            .map_err(e)? as u64;
    }
    conn.execute(
        "DELETE FROM key_facts_fts WHERE rowid IN
             (SELECT k.rowid FROM key_facts k
              WHERE k.agent_id = ?1
                AND k.id IN (SELECT id FROM temp.forget_ids WHERE store = 'key_facts'))",
        params![agent],
    )
    .map_err(e)?;
    report.key_facts_deleted = conn
        .execute(
            "DELETE FROM key_facts WHERE agent_id = ?1 AND id IN
                 (SELECT id FROM temp.forget_ids WHERE store = 'key_facts')",
            params![agent],
        )
        .map_err(e)? as u64;
    {
        let mut del = conn
            .prepare_cached(
                "DELETE FROM memory_origins
                 WHERE memory_store = ?1 AND memory_id = ?2 AND agent_id = ?3
                   AND source_session = ?4 AND source_message = ?5 AND role = 'reaffirm'",
            )
            .map_err(e)?;
        for (store, id, session, message) in &c.reaffirm_rows {
            report.reaffirm_lineage_removed += del
                .execute(params![store, id, agent, session, message])
                .map_err(e)? as u64;
        }
    }
    // The deleted rows' own lineage goes too (M1 / M-2): it names the
    // forgotten conversation and carries message hashes; `forgotten_memories`
    // already keeps the ids from coming back.
    conn.execute(
        "DELETE FROM memory_origins WHERE agent_id = ?1 AND (
             (memory_store = 'memories' AND memory_id IN
                 (SELECT id FROM temp.forget_ids WHERE store IN ('memories', 'archive')))
          OR (memory_store = 'key_facts' AND memory_id IN
                 (SELECT id FROM temp.forget_ids WHERE store = 'key_facts')))",
        params![agent],
    )
    .map_err(e)?;
    {
        let mut del = conn
            .prepare_cached(
                "DELETE FROM memory_origins
                 WHERE memory_store = ?1 AND memory_id = ?2 AND agent_id = ?3",
            )
            .map_err(e)?;
        for (store, id) in &c.lineage_only {
            del.execute(params![store, id, agent]).map_err(e)?;
        }
    }
    engine.hooks.fire_apply(ApplyHookPoint::AfterDeletes)?;

    // Supersession chain repair (§7.2, D2: a predecessor stays closed).
    let marker = serde_json::json!({ "at": now, "plan_id": plan.plan_id }).to_string();
    for cut in &c.cuts {
        let sql = if cut.predecessor {
            "UPDATE memories SET superseded_by = NULL,
                 metadata = CASE WHEN json_valid(metadata)
                                 THEN json_set(metadata, '$.lineage_chain_cut', json(?1))
                                 ELSE metadata END
             WHERE id = ?2 AND agent_id = ?3 AND superseded_by = ?4"
        } else {
            "UPDATE memories SET supersedes = NULL,
                 metadata = CASE WHEN json_valid(metadata)
                                 THEN json_set(metadata, '$.lineage_chain_cut', json(?1))
                                 ELSE metadata END
             WHERE id = ?2 AND agent_id = ?3 AND supersedes = ?4"
        };
        report.supersession_links_cut += conn
            .execute(sql, params![marker, cut.other, agent, cut.deleted])
            .map_err(e)? as u64;
    }

    for entity in &c.orphan_entities {
        report.entity_embeddings_deleted += conn
            .execute(
                "DELETE FROM entity_embedding WHERE agent_id = ?1 AND entity = ?2",
                params![agent, entity],
            )
            .map_err(e)? as u64;
    }

    for s in &c.body.steps {
        report.steps_pending += conn
            .execute(
                "INSERT OR IGNORE INTO memory_forget_steps
                    (plan_id, step, target, status, attempts, updated_at)
                 VALUES (?1, ?2, ?3, 'pending', 0, ?4)",
                params![plan.plan_id, s.step, s.target, now],
            )
            .map_err(e)? as u64;
    }

    conn.execute(
        "INSERT INTO memory_fence (agent_id, forget_epoch, updated_at) VALUES (?1, 1, ?2)
         ON CONFLICT(agent_id) DO UPDATE SET forget_epoch = forget_epoch + 1,
                                             updated_at = excluded.updated_at",
        params![agent, now],
    )
    .map_err(e)?;
    report.forget_epoch = crate::lineage::db::forget_epoch(conn, agent)?;
    // M-2: the stored plan keeps its hash (audit) but not the content
    // hashes of what it deleted — a low-entropy memory could be confirmed by
    // brute force from them.
    let mut scrubbed = plan.document.clone();
    for t in &mut scrubbed.body.targets {
        t.content_sha256.clear();
    }
    conn.execute(
        "UPDATE memory_forget_plans SET status = 'applied', applied_at = ?1, plan_json = ?3
         WHERE plan_id = ?2",
        params![now, plan.plan_id, scrubbed.canonical_json()?],
    )
    .map_err(e)?;
    Ok(report)
}
