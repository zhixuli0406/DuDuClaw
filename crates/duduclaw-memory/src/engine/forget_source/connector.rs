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

pub(super) fn matching_rows(conn: &Connection, agent: &str, reference: &str) -> Result<BTreeSet<(String, String)>> {
    let mut result = BTreeSet::new();
    let mut stmt = conn.prepare(
        "SELECT DISTINCT o.memory_store,o.memory_id FROM memory_origins o
         JOIN memory_connector_stamps s ON s.memory_store=o.memory_store AND s.memory_id=o.memory_id
         WHERE o.agent_id=?1 AND s.connector_ref=?2"
    ).map_err(err)?;
    for row in stmt.query_map(params![agent,reference], |r| Ok((r.get(0)?,r.get(1)?))).map_err(err)? {
        result.insert(row.map_err(err)?);
    }
    if reference.starts_with("channel:") {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT memory_store,memory_id,source_session FROM memory_origins
             WHERE agent_id=?1 AND source_kind!='system' AND role!='reaffirm'"
        ).map_err(err)?;
        for row in stmt.query_map(params![agent], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).map_err(err)? {
            let (store,id,session) = row.map_err(err)?;
            if channel_matches(reference,&session) { result.insert((store,id)); }
        }
    }
    if reference.starts_with("channel:") {
        let mut stmt = conn.prepare("SELECT id,source_session FROM key_facts WHERE agent_id=?1").map_err(err)?;
        for row in stmt.query_map(params![agent], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).map_err(err)? {
            let (id,session) = row.map_err(err)?;
            if channel_matches(reference,&session) { result.insert(("key_facts".into(),id)); }
        }
    }
    let system = system_ids(conn, agent)?;
    result.retain(|key| !system.contains(key));
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
           AND (NOT EXISTS(SELECT 1 FROM memory_connector_stamps s
                WHERE s.memory_store=l.store AND s.memory_id=l.id)
             OR EXISTS(SELECT 1 FROM memory_connector_stamps s
                WHERE s.memory_store=l.store AND s.memory_id=l.id AND s.connector_ref='unknown'))",
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
        assert!(plan.document.body.targets.iter().any(|row| row.id == survivor));
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
