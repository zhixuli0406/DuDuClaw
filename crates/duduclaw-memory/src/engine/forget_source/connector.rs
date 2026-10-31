//! Connector selectors share the source-forget transaction and deletion paths.

use std::collections::BTreeSet;
use super::*;

pub(super) fn system_ids(conn: &Connection, agent: &str) -> Result<BTreeSet<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT memory_store,memory_id FROM memory_origins
         WHERE agent_id=?1 AND source_kind='system'"
    ).map_err(err)?;
    let rows = stmt.query_map(params![agent], |r| Ok((r.get(0)?,r.get(1)?))).map_err(err)?;
    rows.collect::<std::result::Result<BTreeSet<_>,_>>().map_err(err)
}

fn channel_matches(reference: &str, session: &str) -> bool {
    let Some(channel) = reference.strip_prefix("channel:") else { return false; };
    let (kind, agent) = channel.split_once(':').map_or((channel, None), |(kind, agent)| (kind, Some(agent)));
    if !matches!(kind, "line" | "telegram" | "discord" | "slack" | "googlechat")
        || !session.starts_with(&format!("{kind}:")) { return false; }
    match agent {
        Some(agent) => session.split_once("#agent:").is_some_and(|(_, owner)| owner == agent),
        None => !session.contains("#agent:"),
    }
}

#[derive(Default)]
pub(super) struct Selection {
    pub rows: BTreeSet<(String, String)>,
    pub retained_shared: u64,
}

fn disconnected(conn: &Connection, reference: &str, observed: &str) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM disconnected_connectors
            WHERE connector_ref=?1 AND disconnected_at>=?2 AND verified=1)",
        params![reference,observed], |r| r.get(0),
    ).map_err(err)
}

fn channel_ref(session: &str) -> Option<String> {
    let kind = session.split_once(':')?.0;
    if !matches!(kind, "line" | "telegram" | "discord" | "slack" | "googlechat") { return None; }
    Some(match session.split_once("#agent:") {
        Some((_, agent)) => format!("channel:{kind}:{agent}"),
        None => format!("channel:{kind}"),
    })
}

/// Every owning origin must be accounted for. Reaffirmation is evidence of
/// corroboration, never permission to delete the surviving item.
fn ownership(conn: &Connection, agent: &str, store: &str, id: &str, selected: &str) -> Result<(bool, bool)> {
    let uncertain: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM memory_connector_stamps
         WHERE memory_store=?1 AND memory_id=?2 AND role='unknown')",
        params![store,id], |r| r.get(0),
    ).map_err(err)?;
    if uncertain { return Ok((false, true)); }
    let mut stmt = conn.prepare(
        "SELECT source_session,source_message,source_kind,source_observed_at,role FROM memory_origins
         WHERE agent_id=?1 AND memory_store=?2 AND memory_id=?3 AND role!='reaffirm'"
    ).map_err(err)?;
    let origins = stmt.query_map(params![agent,store,id], |r| Ok((
        r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,
        r.get::<_,String>(3)?,r.get::<_,String>(4)?
    ))).map_err(err)?.collect::<std::result::Result<Vec<_>,_>>().map_err(err)?;
    if origins.is_empty() { return Ok((false, true)); }
    let mut live = false;
    let mut unknown = false;
    let mut selected_owner = false;
    for (session,message,kind,observed,role) in origins {
        unknown |= matches!(kind.as_str(), "upstream_unknown" | "untracked_parent");
        let mut stmt = conn.prepare(
            "SELECT connector_ref,connector_observed_at FROM memory_connector_stamps
             WHERE memory_store=?1 AND memory_id=?2 AND source_session=?3 AND source_message=?4
               AND role=?5"
        ).map_err(err)?;
        let stamps = stmt.query_map(params![store,id,session,message,role],
            |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))
            .map_err(err)?.collect::<std::result::Result<Vec<_>,_>>().map_err(err)?;
        if stamps.is_empty() {
            if let Some(reference) = channel_ref(&session) {
                selected_owner |= reference == selected;
                live |= !disconnected(conn, &reference, &observed)?;
            } else if matches!(kind.as_str(), "upstream_unknown" | "untracked_parent") {
                unknown = true;
            } else {
                // A direct import, system source or unstamped local write is
                // an independent owner, not part of a connector disconnect.
                live = true;
            }
        }
        for (reference,at) in stamps {
            selected_owner |= reference == selected;
            if reference == "unknown" { unknown = true; }
            else { live |= reference == "none" || !disconnected(conn, &reference, &at)?; }
        }
    }
    Ok((selected_owner && !live && !unknown, unknown))
}

pub(super) fn matching_rows(conn: &Connection, agent: &str, reference: &str) -> Result<Selection> {
    let mut candidates = BTreeSet::new();
    let mut stmt = conn.prepare(
        "SELECT DISTINCT o.memory_store,o.memory_id FROM memory_origins o
         JOIN memory_connector_stamps s ON s.memory_store=o.memory_store AND s.memory_id=o.memory_id
         WHERE o.agent_id=?1 AND s.connector_ref=?2"
    ).map_err(err)?;
    for row in stmt.query_map(params![agent,reference], |r| Ok((r.get(0)?,r.get(1)?))).map_err(err)? {
        candidates.insert(row.map_err(err)?);
    }
    if reference.starts_with("channel:") {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT memory_store,memory_id,source_session FROM memory_origins
             WHERE agent_id=?1 AND source_kind!='system' AND role!='reaffirm'"
        ).map_err(err)?;
        for row in stmt.query_map(params![agent], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).map_err(err)? {
            let (store,id,session) = row.map_err(err)?;
            if channel_matches(reference,&session) { candidates.insert((store,id)); }
        }
    }
    let system = system_ids(conn, agent)?;
    let mut result = Selection::default();
    for (store,id) in candidates {
        if system.contains(&(store.clone(),id.clone())) { continue; }
        let (delete,unknown) = ownership(conn, agent, &store, &id, reference)?;
        if delete { result.rows.insert((store,id)); }
        else if !unknown {
            let live = super::body::row_exists(conn, &store, &id, agent)?;
            let archive = store == "memories"
                && crate::lineage::db::table_exists(conn,"main","memories_archive")?
                && super::body::archived(conn, &id, agent)?;
            if live || archive { result.retained_shared += 1; }
        }
    }
    Ok(result)
}

fn stored_items_cte(conn: &Connection) -> Result<String> {
    let archive = if crate::lineage::db::table_exists(conn,"main","memories_archive")? {
        "UNION SELECT 'memories',id FROM memories_archive WHERE agent_id=?1"
    } else { "" };
    Ok(format!("WITH items(store,id) AS (
        SELECT 'memories',id FROM memories WHERE agent_id=?1
        UNION SELECT 'key_facts',id FROM key_facts WHERE agent_id=?1 {archive})"))
}

pub(super) fn system_count(conn: &Connection, agent: &str) -> Result<u64> {
    let sql = format!("{} SELECT COUNT(*) FROM items l
        WHERE EXISTS(SELECT 1 FROM memory_origins o WHERE o.agent_id=?1
            AND o.memory_store=l.store AND o.memory_id=l.id AND o.source_kind='system')",
        stored_items_cte(conn)?);
    conn.query_row(&sql,params![agent],|row| row.get(0)).map_err(err)
}

pub(super) fn unattributed_count(
    conn: &Connection, agent: &str,
    memories: &std::collections::BTreeMap<String,String>,
    facts: &std::collections::BTreeMap<String,String>,
    archives: &BTreeSet<String>,
) -> Result<u64> {
    // Count memory items, not their live/archive storage copies.
    let sql = format!("{} SELECT l.store,l.id FROM items l
         WHERE NOT EXISTS(SELECT 1 FROM memory_origins o
             WHERE o.agent_id=?1 AND o.memory_store=l.store AND o.memory_id=l.id AND o.source_kind='system')
           AND (NOT EXISTS(SELECT 1 FROM memory_origins o
                WHERE o.agent_id=?1 AND o.memory_store=l.store AND o.memory_id=l.id AND o.role!='reaffirm')
             OR NOT EXISTS(SELECT 1 FROM memory_connector_stamps s
                WHERE s.memory_store=l.store AND s.memory_id=l.id AND s.role IN ('direct','inherited'))
             OR EXISTS(SELECT 1 FROM memory_connector_stamps s
                WHERE s.memory_store=l.store AND s.memory_id=l.id AND (s.connector_ref='unknown' OR s.role='unknown'))
             OR EXISTS(SELECT 1 FROM memory_origins o
                WHERE o.agent_id=?1 AND o.memory_store=l.store AND o.memory_id=l.id
                  AND o.role!='reaffirm' AND o.source_kind IN ('upstream_unknown','untracked_parent')))",
        stored_items_cte(conn)?);
    let mut stmt = conn.prepare(&sql).map_err(err)?;
    let mut count = 0;
    for row in stmt.query_map(params![agent], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).map_err(err)? {
        let (store,id) = row.map_err(err)?;
        let deleting = if store == "memories" {
            memories.contains_key(&id) || archives.contains(&id)
        } else { facts.contains_key(&id) };
        if !deleting { count += 1; }
    }
    Ok(count)
}

/// An approval may accept fewer identical rows, never additional or changed content.
pub fn connector_plan_is_subset(approved: &ForgetPlan, refreshed: &ForgetPlan) -> bool {
    let a = &approved.document;
    let b = &refreshed.document;
    if a.selector.connector_ref.is_none() || a.selector.connector_ref != b.selector.connector_ref
        || a.agent_id != b.agent_id || a.db_instance_id != b.db_instance_id
        || b.body.entity_embeddings_orphaned > a.body.entity_embeddings_orphaned {
        return false;
    }
    let orphan_entities: BTreeSet<_> = a.body.orphan_entity_ids.iter().collect();
    if !b.body.orphan_entity_ids.iter().all(|id| orphan_entities.contains(id)) { return false; }
    let targets: BTreeSet<_> = a.body.targets.iter()
        .map(|t| (t.store.as_str(),t.id.as_str(),t.content_sha256.as_str())).collect();
    let target_ids: BTreeSet<_> = a.body.targets.iter()
        .map(|t| (t.store.as_str(),t.id.as_str())).collect();
    let archives: BTreeSet<_> = a.body.archive_ids.iter().map(String::as_str).collect();
    let lineage: BTreeSet<_> = a.body.lineage_only.iter()
        .map(|id| (id.store.as_str(),id.id.as_str())).collect();
    let cuts: BTreeSet<_> = a.body.supersession_cuts.iter()
        .map(|c| (c.deleted.as_str(),c.other.as_str(),c.relation.as_str(),c.action.as_str())).collect();
    let steps: BTreeSet<_> = a.body.steps.iter().map(|s| (s.step.as_str(),s.target.as_str())).collect();
    b.body.targets.iter().all(|t| targets.contains(&(t.store.as_str(),t.id.as_str(),t.content_sha256.as_str())))
        && b.body.archive_ids.iter().all(|id| archives.contains(id.as_str()))
        && b.body.lineage_only.iter().all(|id| {
            let key = (id.store.as_str(),id.id.as_str());
            lineage.contains(&key) || target_ids.contains(&key)
                || (id.store == "memories" && archives.contains(id.id.as_str()))
        })
        && b.body.supersession_cuts.iter().all(|c|
            cuts.contains(&(c.deleted.as_str(),c.other.as_str(),c.relation.as_str(),c.action.as_str())))
        && b.body.steps.iter().all(|s| steps.contains(&(s.step.as_str(),s.target.as_str())))
}

impl SqliteMemoryEngine {
    /// Record the completed disconnect even when forgetting was not requested.
    pub async fn record_connector_disconnect(&self, reference: &str) -> Result<()> {
        if !crate::lineage::connectors::valid_ref(reference) || matches!(reference, "unknown" | "none") {
            return Err(err("invalid connector reference"));
        }
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO disconnected_connectors(connector_ref,disconnected_at,revision,verified) VALUES(?1,?2,?3,0)
             ON CONFLICT(connector_ref) DO UPDATE SET disconnected_at=MAX(disconnected_at,excluded.disconnected_at),
                revision=excluded.revision,verified=0",
            params![reference,crate::format_ts(chrono::Utc::now()),uuid::Uuid::new_v4().to_string()],
        ).map_err(err)?;
        Ok(())
    }

    pub async fn disconnected_connector_refs(&self) -> Result<Vec<(String, String)>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare("SELECT connector_ref,revision FROM disconnected_connectors ORDER BY connector_ref").map_err(err)?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?,r.get(1)?))).map_err(err)?;
        rows.collect::<std::result::Result<Vec<_>,_>>().map_err(err)
    }

    /// Confirm only the exact disconnect revisions whose authoritative state
    /// the host read. A delayed old notification creates a new, unverified
    /// revision and cannot inherit a previous refresh's permission to delete.
    pub async fn retain_disconnected_connectors(&self, confirmed: &[(String, String)]) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute("UPDATE disconnected_connectors SET verified=0", []).map_err(err)?;
        let refs = {
            let mut stmt = conn.prepare("SELECT connector_ref,revision FROM disconnected_connectors").map_err(err)?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?,r.get(1)?))).map_err(err)?;
            rows.collect::<std::result::Result<Vec<(String,String)>,_>>().map_err(err)?
        };
        for (reference,revision) in refs {
            if confirmed.contains(&(reference.clone(),revision.clone())) {
                conn.execute("UPDATE disconnected_connectors SET verified=1 WHERE connector_ref=?1 AND revision=?2",
                    params![reference,revision]).map_err(err)?;
            } else {
                conn.execute("DELETE FROM disconnected_connectors WHERE connector_ref=?1 AND revision=?2",
                    params![reference,revision]).map_err(err)?;
            }
        }
        Ok(())
    }

    /// Persisted in the deletion transaction, so a crashed worker can report exact counts.
    pub async fn get_forget_apply_report(&self, plan_id: &str) -> Result<Option<ApplyReport>> {
        let conn = self.conn.lock().await;
        let json: Option<String> = conn.query_row(
            "SELECT report_json FROM memory_forget_apply_reports WHERE plan_id=?1",
            params![plan_id], |r| r.get(0),
        ).optional().map_err(err)?;
        json.map(|text| serde_json::from_str(&text).map_err(err)).transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_channel_does_not_select_employee_bots() {
        assert!(channel_matches("channel:telegram", "telegram:123"));
        assert!(!channel_matches("channel:telegram", "telegram:123#agent:alice"));
        assert!(channel_matches("channel:telegram:alice", "telegram:123#agent:alice"));
        assert!(!channel_matches("channel:telegram:alice", "telegram:123#agent:alice-other"));
        assert!(!channel_matches("channel:telegram", "telegramish:123"));
    }
}

#[cfg(test)]
mod engine_tests {
    use super::*;
    use crate::lineage::test_support::*;
    use crate::lineage::Provenance;
    use duduclaw_core::traits::MemoryEngine;

    async fn connector_plan(engine: &SqliteMemoryEngine, reference: &str) -> ForgetPlan {
        engine.record_connector_disconnect(reference).await.unwrap();
        let observed = engine.disconnected_connector_refs().await.unwrap();
        engine.retain_disconnected_connectors(&observed).await.unwrap();
        match engine.plan_forget_connector(AGENT, reference, PlanOptions::default()).await.unwrap() {
            PlanOutcome::Planned(plan) => *plan,
            other => panic!("expected plan, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn google_forget_deletes_search_recall_and_facts_with_exact_durable_counts() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let provenance = src(msg("telegram:google", 1)).with_connector_refs(vec!["google:home".into()]);
        let id = put(&engine, "private google lunch preference", TemporalMeta::default(), provenance.clone()).await;
        engine.store_fact(AGENT, "Google lunch is at noon", "telegram", "google", "telegram:google", provenance).await.unwrap();
        let legacy = put(&engine, "legacy unrelated note", TemporalMeta::default(), src(msg("telegram:old", 1))).await;
        let unknown = put(&engine, "unavailable ledger note", TemporalMeta::default(),
            src(msg("telegram:unknown", 1)).with_connector_refs(vec!["unknown".into()])).await;
        let system = put(&engine, "system retained", TemporalMeta::default(),
            Provenance::test_only().with_connector_refs(vec!["google:home".into()])).await;
        let plan = connector_plan(&engine, "google:home").await;
        assert_eq!(plan.document.body.untracked_in_namespace, 2);
        assert_eq!(plan.document.body.system_rows, 1);
        assert_eq!(parse_rfc3339(&plan.document.expires_at).unwrap()
            - parse_rfc3339(&plan.document.created_at).unwrap(), chrono::Duration::days(7));
        let report = apply(&engine, &plan.plan_id).await;
        assert_eq!(report.memories_deleted, 1);
        assert_eq!(report.key_facts_deleted, 1);
        let durable = engine.get_forget_apply_report(&plan.plan_id).await.unwrap().unwrap();
        assert_eq!(durable.memories_deleted, report.memories_deleted);
        assert_eq!(durable.key_facts_deleted, report.key_facts_deleted);
        assert!(engine.search(AGENT, "google lunch", 20).await.unwrap().iter().all(|m| m.id != id));
        assert!(engine.list_recent(AGENT, 100).await.unwrap().iter().all(|m| m.id != id));
        assert!(engine.get_by_ids(AGENT, &[id]).await.unwrap().is_empty());
        assert!(engine.search_facts(AGENT,"lunch",10).await.unwrap().is_empty());
        assert_eq!(count(&engine, "SELECT COUNT(*) FROM key_facts").await, 0);
        assert!(exists(&engine,&legacy).await && exists(&engine,&unknown).await && exists(&engine,&system).await);
        assert!(matches!(engine.apply_forget_plan(&plan.plan_id, &ExternalInputs::default()).await.unwrap(), ApplyOutcome::AlreadyApplied));
    }

    // Regression tests added for F1/F2; not run in this change.
    #[tokio::test]
    async fn shared_connection_owners_are_retained_until_both_disconnect() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let shared = put(&engine, "joint google and database note", TemporalMeta::default(),
            src(msg("telegram:joint", 1)).with_connector_refs(vec!["google:home".into(), "db:live".into()])).await;
        engine.record_connector_disconnect("google:home").await.unwrap();
        let first = connector_plan(&engine, "google:home").await;
        assert!(first.document.body.targets.is_empty());
        assert_eq!(first.document.body.retained_shared, 1);
        let report = apply(&engine, &first.plan_id).await;
        assert_eq!(report.retained_shared, 1);
        assert!(exists(&engine, &shared).await);
        engine.record_connector_disconnect("db:live").await.unwrap();
        let second = connector_plan(&engine, "db:live").await;
        assert_eq!(second.document.body.retained_shared, 0);
        assert!(second.document.body.targets.iter().any(|row| row.id == shared));
        apply(&engine, &second.plan_id).await;
        assert!(!exists(&engine, &shared).await);
    }

    #[tokio::test]
    async fn google_reaffirmation_does_not_own_a_database_memory_or_its_child() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let survivor = put(&engine, "database owns this stable fact", TemporalMeta::default(),
            src(msg("cron:db", 1)).with_connector_refs(vec!["db:live".into()])).await;
        {
            let conn = engine.conn_for_maintenance().await;
            let provenance = src(msg("telegram:google", 1)).with_connector_refs(vec!["google:home".into()]);
            let rows = crate::lineage::db::resolve(&conn, AGENT, &provenance, &[]).unwrap().unwrap();
            let skipped = std::sync::atomic::AtomicU64::new(0);
            assert!(crate::lineage::db::add_reaffirm(&conn, &survivor, AGENT, &rows, &skipped).unwrap().is_none());
        }
        let child = put(&engine, "derived database fact", TemporalMeta::default(),
            Provenance::derived(vec![survivor.clone()])).await;
        let plan = connector_plan(&engine, "google:home").await;
        assert!(plan.document.body.targets.is_empty());
        assert_eq!(plan.document.body.retained_shared, 1);
        apply(&engine, &plan.plan_id).await;
        assert!(exists(&engine, &survivor).await && exists(&engine, &child).await);
    }

    #[tokio::test]
    async fn independent_direct_origin_and_shared_descendant_stay() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let root = put(&engine, "google source root", TemporalMeta::default(),
            src(msg("telegram:google", 1)).with_connector_refs(vec!["google:home".into()])).await;
        let child = put(&engine, "child also independently supplied", TemporalMeta::default(),
            Provenance::Derived { parents: vec![root.clone()], extra: vec![msg("telegram:live", 1)] }).await;
        let plan = connector_plan(&engine, "google:home").await;
        assert!(plan.document.body.targets.iter().any(|row| row.id == root));
        assert!(!plan.document.body.targets.iter().any(|row| row.id == child));
        assert_eq!(plan.document.body.retained_shared, 1);
        apply(&engine, &plan.plan_id).await;
        assert!(exists(&engine, &child).await);
    }

    #[tokio::test]
    async fn unknown_origins_and_legacy_parent_stamps_never_become_google_ownership() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let unknown = put(&engine, "unknown upstream remains private", TemporalMeta::default(),
            Provenance::Sources(vec![msg("telegram:google", 1),
                crate::lineage::SourceRef::upstream_unknown("run:unknown", t0())])
                .with_connector_refs(vec!["google:home".into()])).await;
        let legacy = put(&engine, "legacy ambiguous owner", TemporalMeta::default(),
            src(msg("telegram:legacy", 1)).with_connector_refs(vec!["db:legacy".into()])).await;
        {
            let conn = engine.conn_for_maintenance().await;
            conn.execute("UPDATE memory_connector_stamps SET role='unknown',source_session='',source_message='' WHERE memory_id=?1",
                params![legacy]).unwrap();
        }
        let child = put(&engine, "derived from ambiguous source", TemporalMeta::default(),
            Provenance::derived(vec![legacy.clone()]).with_connector_refs(vec!["google:home".into()])).await;
        let plan = connector_plan(&engine, "google:home").await;
        assert!(plan.document.body.targets.is_empty());
        assert_eq!(plan.document.body.untracked_in_namespace, 3);
        apply(&engine, &plan.plan_id).await;
        for id in [unknown, legacy, child] { assert!(exists(&engine, &id).await); }
    }

    #[tokio::test]
    async fn reconnect_invalidates_pending_deletion_even_for_selected_owner() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let id = put(&engine, "google reconnect survivor", TemporalMeta::default(),
            src(msg("telegram:google", 1)).with_connector_refs(vec!["google:home".into()])).await;
        let plan = connector_plan(&engine, "google:home").await;
        assert_eq!(plan.document.body.targets.len(), 1);
        engine.retain_disconnected_connectors(&[]).await.unwrap();
        assert!(matches!(engine.apply_forget_plan(&plan.plan_id, &ExternalInputs::default()).await.unwrap(), ApplyOutcome::Stale(_)));
        assert!(exists(&engine, &id).await);
    }

    #[tokio::test]
    async fn delayed_disconnect_cannot_reuse_an_older_verified_revision() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let id = put(&engine, "shared reconnect race survivor", TemporalMeta::default(),
            src(msg("telegram:joint", 1)).with_connector_refs(vec!["google:home".into(), "db:live".into()])).await;
        engine.record_connector_disconnect("db:live").await.unwrap();
        let approved = connector_plan(&engine, "google:home").await;
        assert_eq!(approved.document.body.targets.len(), 1);
        let observed = engine.disconnected_connector_refs().await.unwrap();
        // Reconnect fences the old revision, then an old disconnect callback
        // arrives after publication. It must start unverified with a new id.
        {
            let conn = engine.conn_for_maintenance().await;
            conn.execute("DELETE FROM disconnected_connectors WHERE connector_ref='db:live'", []).unwrap();
        }
        engine.record_connector_disconnect("db:live").await.unwrap();
        engine.retain_disconnected_connectors(&observed).await.unwrap();
        assert!(matches!(engine.apply_forget_plan(&approved.plan_id, &ExternalInputs::default()).await.unwrap(), ApplyOutcome::Stale(_)));
        assert!(exists(&engine, &id).await);
    }

    #[tokio::test]
    async fn same_count_orphan_embedding_swap_invalidates_hash_and_subset() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        put(&engine, "Google contains private material", triple("google", "contains", "private"),
            src(msg("telegram:google", 1)).with_connector_refs(vec!["google:home".into()])).await;
        {
            let conn = engine.conn_for_maintenance().await;
            conn.execute("INSERT INTO entity_embedding(agent_id,entity,model,vec)
                VALUES(?1,'google','test',X'00000000')", params![AGENT]).unwrap();
        }
        let approved = connector_plan(&engine, "google:home").await;
        assert_eq!(approved.document.body.orphan_entity_ids, vec!["google"]);
        {
            let conn = engine.conn_for_maintenance().await;
            conn.execute("UPDATE entity_embedding SET entity='private' WHERE agent_id=?1 AND entity='google'", params![AGENT]).unwrap();
        }
        let refreshed = connector_plan(&engine, "google:home").await;
        assert_eq!(approved.document.body.entity_embeddings_orphaned, refreshed.document.body.entity_embeddings_orphaned);
        assert_eq!(refreshed.document.body.orphan_entity_ids, vec!["private"]);
        assert!(!connector_plan_is_subset(&approved, &refreshed));
        let mut swapped = approved.document.clone();
        swapped.body.orphan_entity_ids = refreshed.document.body.orphan_entity_ids.clone();
        assert_ne!(approved.plan_hash, swapped.plan_hash().unwrap());
        assert!(matches!(engine.apply_forget_plan(&approved.plan_id, &ExternalInputs::default()).await.unwrap(), ApplyOutcome::Stale(_)));
        assert_eq!(count(&engine, "SELECT COUNT(*) FROM entity_embedding").await, 1);
    }

    #[tokio::test]
    async fn refreshed_connector_scope_accepts_shrink_but_refuses_growth() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let provenance = || src(msg("telegram:google", 1)).with_connector_refs(vec!["google:home".into()]);
        let first = put(&engine, "first connector fact", TemporalMeta::default(), provenance()).await;
        let approved = connector_plan(&engine,"google:home").await;
        put(&engine, "second connector fact", TemporalMeta::default(), provenance()).await;
        let grown = connector_plan(&engine,"google:home").await;
        assert!(!connector_plan_is_subset(&approved,&grown));
        {
            let conn = engine.conn_for_maintenance().await;
            conn.execute("DELETE FROM memories WHERE id=?1", params![first]).unwrap();
        }
        let shrunk = connector_plan(&engine,"google:home").await;
        // A removed row can still retain lineage; that is bounded by the approved set.
        assert!(connector_plan_is_subset(&grown,&shrunk));
    }

    #[tokio::test]
    async fn live_archive_copies_are_counted_hashed_and_require_approval_when_changed() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let id = put(&engine,"google live content",TemporalMeta::default(),
            src(msg("telegram:google",1)).with_connector_refs(vec!["google:home".into()])).await;
        let without_archive = connector_plan(&engine,"google:home").await;
        {
            let conn = engine.conn_for_maintenance().await;
            conn.execute_batch("CREATE TABLE memories_archive (
                id TEXT PRIMARY KEY,agent_id TEXT NOT NULL,content TEXT NOT NULL,timestamp TEXT NOT NULL);").unwrap();
            conn.execute("INSERT INTO memories_archive(id,agent_id,content,timestamp)
                SELECT id,agent_id,content,timestamp FROM memories WHERE id=?1",params![id]).unwrap();
        }
        let with_archive = connector_plan(&engine,"google:home").await;
        assert_eq!(with_archive.document.body.archive_ids,vec![id.clone()]);
        assert_eq!(with_archive.document.body.targets.iter().filter(|t| t.store == "memories").count(),1);
        assert_eq!(with_archive.document.body.targets.iter().filter(|t| t.store == "archive").count(),1);
        assert!(!connector_plan_is_subset(&without_archive,&with_archive));
        assert!(matches!(engine.apply_forget_plan(&without_archive.plan_id,&ExternalInputs::default()).await.unwrap(),
            ApplyOutcome::Stale(_)));
        assert!(matches!(engine.plan_forget_connector(AGENT,"google:home",PlanOptions {
            max_rows: Some(1),ttl_minutes: None,
        }).await.unwrap(),PlanOutcome::TooLarge { .. }));
        {
            let conn = engine.conn_for_maintenance().await;
            conn.execute("UPDATE memories_archive SET content='changed archive copy' WHERE id=?1",params![id]).unwrap();
        }
        let changed_archive = connector_plan(&engine,"google:home").await;
        assert!(!connector_plan_is_subset(&with_archive,&changed_archive));
        assert!(matches!(engine.apply_forget_plan(&with_archive.plan_id,&ExternalInputs::default()).await.unwrap(),
            ApplyOutcome::Stale(_)));
        assert!(exists(&engine,&id).await);
        assert_eq!(count(&engine,"SELECT COUNT(*) FROM memories_archive").await,1);
        let report = apply(&engine,&changed_archive.plan_id).await;
        assert_eq!(report.memories_deleted,1);
        assert_eq!(report.archive_deleted,1);
        assert_eq!(count(&engine,"SELECT COUNT(*) FROM memories_archive").await,0);
    }

    #[tokio::test]
    async fn newly_orphaned_embedding_requires_approval_again() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        put(&engine,"Google holds the private document",triple("google","contains","private"),
            src(msg("telegram:google",1)).with_connector_refs(vec!["google:home".into()])).await;
        let approved = connector_plan(&engine,"google:home").await;
        assert_eq!(approved.document.body.entity_embeddings_orphaned,0);
        {
            let conn = engine.conn_for_maintenance().await;
            conn.execute("INSERT INTO entity_embedding(agent_id,entity,model,vec)
                VALUES(?1,'google','test',X'00000000')",params![AGENT]).unwrap();
        }
        let refreshed = connector_plan(&engine,"google:home").await;
        assert_eq!(refreshed.document.body.entity_embeddings_orphaned,1);
        assert!(!connector_plan_is_subset(&approved,&refreshed));
        assert!(matches!(engine.apply_forget_plan(&approved.plan_id,&ExternalInputs::default()).await.unwrap(),
            ApplyOutcome::Stale(_)));
        assert_eq!(count(&engine,"SELECT COUNT(*) FROM entity_embedding").await,1);
    }

    #[tokio::test]
    async fn unattributed_counts_archive_only_items_without_double_counting_live_copies() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let live = put(&engine,"unstamped existing live note",TemporalMeta::default(),src(msg("telegram:old",1))).await;
        let archived = put(&engine,"historical travel itinerary",TemporalMeta::default(),src(msg("telegram:old",2))).await;
        {
            let conn = engine.conn_for_maintenance().await;
            conn.execute_batch("CREATE TABLE memories_archive (
                id TEXT PRIMARY KEY,agent_id TEXT NOT NULL,content TEXT NOT NULL,timestamp TEXT NOT NULL);").unwrap();
            conn.execute("INSERT INTO memories_archive(id,agent_id,content,timestamp)
                SELECT id,agent_id,content,timestamp FROM memories WHERE id IN (?1,?2)",params![live,archived]).unwrap();
            conn.execute("DELETE FROM memories WHERE id=?1",params![archived]).unwrap();
        }
        let plan = connector_plan(&engine,"google:home").await;
        assert_eq!(plan.document.body.untracked_in_namespace,2);
        assert!(plan.document.body.targets.is_empty());
        let report = apply(&engine,&plan.plan_id).await;
        assert_eq!(report.untracked_in_namespace_at_apply,2);
        assert_eq!(report.memories_deleted,0);
        assert_eq!(report.archive_deleted,0);
        assert!(exists(&engine,&live).await);
        assert_eq!(count(&engine,"SELECT COUNT(*) FROM memories_archive").await,2);
    }

    #[tokio::test]
    async fn system_count_uses_existing_items_and_deduplicates_archive_copies() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let live = put(&engine,"sunrise system template",TemporalMeta::default(),Provenance::test_only()).await;
        let archived = put(&engine,"network baseline manifest",TemporalMeta::default(),Provenance::test_only()).await;
        let gone = put(&engine,"deleted retention placeholder",TemporalMeta::default(),Provenance::test_only()).await;
        {
            let conn = engine.conn_for_maintenance().await;
            conn.execute_batch("CREATE TABLE memories_archive (
                id TEXT PRIMARY KEY,agent_id TEXT NOT NULL,content TEXT NOT NULL,timestamp TEXT NOT NULL);").unwrap();
            conn.execute("INSERT INTO memories_archive(id,agent_id,content,timestamp)
                SELECT id,agent_id,content,timestamp FROM memories WHERE id IN (?1,?2)",params![live,archived]).unwrap();
            conn.execute("DELETE FROM memories WHERE id IN (?1,?2)",params![archived,gone]).unwrap();
            assert_eq!(system_ids(&conn,AGENT).unwrap().len(),3);
        }
        let plan = connector_plan(&engine,"google:home").await;
        assert_eq!(plan.document.body.system_rows,2);
        assert_eq!(plan.document.body.untracked_in_namespace,0);
        let report = apply(&engine,&plan.plan_id).await;
        assert_eq!(report.system_rows,2);
        assert_eq!(report.memories_deleted,0);
        assert_eq!(report.archive_deleted,0);
    }

    #[tokio::test]
    async fn connector_plan_never_uses_the_empty_legacy_session() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        engine.store_fact(AGENT,"legacy fact without a session","","","",src(msg("telegram:old",1))).await.unwrap();
        {
            let conn = engine.conn_for_maintenance().await;
            conn.execute("DELETE FROM memory_origins WHERE memory_store='key_facts'",[]).unwrap();
        }
        let plan = connector_plan(&engine,"google:home").await;
        assert!(plan.document.body.targets.is_empty());
        assert_eq!(plan.document.body.untracked_in_namespace,1);
        let report = apply(&engine,&plan.plan_id).await;
        assert_eq!(report.key_facts_deleted,0);
        assert_eq!(count(&engine,"SELECT COUNT(*) FROM key_facts").await,1);
    }

    #[tokio::test]
    async fn delayed_distillation_uses_turn_time_not_new_source_time() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let turn_started = chrono::Utc::now() - chrono::Duration::hours(1);
        let plan = connector_plan(&engine, "google:home").await;
        apply(&engine, &plan.plan_id).await;
        let mut late_source = msg("cron:bot", 1);
        late_source.observed_at = chrono::Utc::now() + chrono::Duration::seconds(1);
        let late = src(late_source.clone()).with_connector_refs_at(vec!["google:home".into()], turn_started);
        assert!(matches!(try_put(&engine, "delayed old private result", TemporalMeta::default(), late).await,
            crate::supersession_guard::TemporalWriteOutcome::Fenced(_)));
        let fresh = src(late_source.clone()).with_connector_refs_at(
            vec!["google:home".into()], late_source.observed_at,
        );
        let id = put(&engine, "new connection turn", TemporalMeta::default(), fresh).await;
        let conn = engine.conn_for_maintenance().await;
        let stored: String = conn.query_row(
            "SELECT connector_observed_at FROM memory_connector_stamps WHERE memory_id=?1 AND connector_ref='google:home'",
            params![id], |r| r.get(0),
        ).unwrap();
        assert_eq!(stored, crate::format_ts(late_source.observed_at));
        assert!(conn.execute(
            "UPDATE memory_connector_stamps SET connector_observed_at=?1 WHERE memory_id=?2",
            params![crate::format_ts(turn_started), id],
        ).is_err());
    }

    #[tokio::test]
    async fn reaffirm_lineage_cap_still_records_connector_stamp() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let sources = (1..=crate::lineage::MAX_LINEAGE_SOURCES)
            .map(|i| msg("telegram:full", i as i64)).collect();
        let survivor = put(&engine, "full lineage survivor", TemporalMeta::default(), Provenance::Sources(sources)).await;
        let provenance = src(msg("cron:another", 1)).with_connector_refs_at(
            vec!["google:home".into()], chrono::Utc::now(),
        );
        {
            let conn = engine.conn_for_maintenance().await;
            let rows = crate::lineage::db::resolve(&conn, AGENT, &provenance, &[]).unwrap().unwrap();
            let skipped = std::sync::atomic::AtomicU64::new(0);
            assert!(crate::lineage::db::add_reaffirm(&conn, &survivor, AGENT, &rows, &skipped).unwrap().is_none());
            assert_eq!(skipped.load(std::sync::atomic::Ordering::Relaxed), 1);
            assert_eq!(crate::lineage::connectors::load(&conn, "memories", &survivor).unwrap().len(), 1);
        }
        let plan = connector_plan(&engine, "google:home").await;
        assert!(!plan.document.body.targets.iter().any(|row| row.id == survivor));
        assert_eq!(plan.document.body.retained_shared, 1);
        apply(&engine, &plan.plan_id).await;
        assert!(exists(&engine, &survivor).await);
    }

    #[tokio::test]
    async fn connector_stamps_fence_old_turns_and_inherit_to_derived_rows() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let provenance = src(msg("telegram:google",1)).with_connector_refs(vec!["google:home".into()]);
        let parent = put(&engine,"google root",TemporalMeta::default(),provenance.clone()).await;
        let derived = put(&engine,"derived answer",TemporalMeta::default(),Provenance::derived(vec![parent])).await;
        let plan = connector_plan(&engine,"google:home").await;
        assert!(plan.document.body.targets.iter().any(|target| target.id == derived));
        apply(&engine,&plan.plan_id).await;
        assert!(matches!(try_put(&engine,"late old turn",TemporalMeta::default(),provenance).await,
            crate::supersession_guard::TemporalWriteOutcome::Fenced(_)));
    }
}
