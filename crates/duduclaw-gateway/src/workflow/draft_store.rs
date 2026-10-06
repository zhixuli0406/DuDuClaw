//! A-owned immutable proposals/review evidence in the shared WorkflowStore.
//! Activation, grants, operations and cron authority have no write seam here.
use super::{FixtureAssertionResult, FixtureRunEvidence, WorkflowStore};
use crate::review_evidence::{ReviewAcceptance, ReviewSnapshot};
use crate::workflow_drafts::{WorkflowDraft, evaluate_fixture};
use rusqlite::{OptionalExtension, Transaction, params};

pub fn init_schema(tx: &Transaction<'_>) -> Result<(), String> {
    tx.execute_batch("CREATE TABLE IF NOT EXISTS workflow_schema_meta(owner TEXT PRIMARY KEY,version INTEGER NOT NULL);
      INSERT OR IGNORE INTO workflow_schema_meta VALUES('evidence',1);
      CREATE TABLE IF NOT EXISTS review_snapshots(snapshot_id TEXT PRIMARY KEY,task_id TEXT NOT NULL,
        authority_revision INTEGER NOT NULL,snapshot_hash TEXT NOT NULL,record_json TEXT NOT NULL);
      CREATE TRIGGER IF NOT EXISTS review_snapshot_immutable BEFORE UPDATE ON review_snapshots BEGIN SELECT
        RAISE(ABORT,'review snapshot immutable'); END;
      CREATE TRIGGER IF NOT EXISTS review_snapshot_retained BEFORE DELETE ON review_snapshots BEGIN SELECT
        RAISE(ABORT,'review snapshot retained'); END;
      CREATE TABLE IF NOT EXISTS review_acceptances(snapshot_id TEXT PRIMARY KEY,snapshot_hash TEXT NOT NULL,
        record_json TEXT NOT NULL);
      CREATE TRIGGER IF NOT EXISTS review_acceptance_immutable BEFORE UPDATE ON review_acceptances BEGIN SELECT
        RAISE(ABORT,'review acceptance immutable'); END;
      CREATE TRIGGER IF NOT EXISTS review_acceptance_retained BEFORE DELETE ON review_acceptances BEGIN SELECT
        RAISE(ABORT,'review acceptance retained'); END;
      CREATE TABLE IF NOT EXISTS workflow_drafts(draft_id TEXT NOT NULL,revision INTEGER NOT NULL,owner TEXT NOT NULL,
        draft_hash TEXT NOT NULL,disabled INTEGER NOT NULL CHECK(disabled=1),record_json TEXT NOT NULL,
        PRIMARY KEY(draft_id,revision));
      CREATE TRIGGER IF NOT EXISTS workflow_draft_immutable BEFORE UPDATE ON workflow_drafts BEGIN SELECT RAISE(ABORT,
        'workflow draft immutable'); END;
      CREATE TRIGGER IF NOT EXISTS workflow_draft_retained BEFORE DELETE ON workflow_drafts BEGIN SELECT RAISE(ABORT,
        'workflow draft retained'); END;
      CREATE TABLE IF NOT EXISTS workflow_fixture_results(run_id TEXT PRIMARY KEY,draft_id TEXT NOT NULL,
        revision INTEGER NOT NULL,fixture_id TEXT NOT NULL,evidence_hash TEXT NOT NULL,record_json TEXT NOT NULL,
        assertions_json TEXT NOT NULL);
      CREATE TRIGGER IF NOT EXISTS workflow_fixture_immutable BEFORE UPDATE ON workflow_fixture_results BEGIN SELECT
        RAISE(ABORT,'fixture result immutable'); END;
      CREATE TRIGGER IF NOT EXISTS workflow_fixture_retained BEFORE DELETE ON workflow_fixture_results BEGIN SELECT
        RAISE(ABORT,'fixture result retained'); END;
      CREATE TABLE IF NOT EXISTS review_artifact_audiences(snapshot_id TEXT NOT NULL,task_id TEXT NOT NULL,
        agent_id TEXT NOT NULL,archived_name TEXT NOT NULL,audience_json TEXT NOT NULL,PRIMARY KEY(snapshot_id,
        agent_id,archived_name));
      CREATE TRIGGER IF NOT EXISTS review_artifact_audience_immutable BEFORE UPDATE ON review_artifact_audiences BEGIN
        SELECT RAISE(ABORT,'artifact audience immutable'); END;
      CREATE TRIGGER IF NOT EXISTS review_artifact_audience_retained BEFORE DELETE ON review_artifact_audiences BEGIN
        SELECT RAISE(ABORT,'artifact audience retained'); END;")
        .map_err(|e| e.to_string())?;
    let version: i64 = tx
        .query_row(
            "SELECT version FROM workflow_schema_meta WHERE owner='evidence'",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if version != 1 {
        return Err("unsupported evidence schema version".into());
    }
    Ok(())
}
fn encode<T: serde::Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_string(value).map_err(|e| e.to_string())
}
fn decode<T: serde::de::DeserializeOwned>(value: String) -> Result<T, String> {
    serde_json::from_str(&value).map_err(|e| format!("invalid immutable evidence row: {e}"))
}

impl WorkflowStore {
    pub async fn initialize_evidence(&self) -> Result<(), String> {
        self.with_transaction(init_schema).await
    }
    pub async fn save_review_snapshot(&self, snapshot: &ReviewSnapshot) -> Result<(), String> {
        snapshot.validate()?;
        let json = encode(snapshot)?;
        self.with_transaction(|tx| {
            let existing: Option<String> = tx
                .query_row(
                    "SELECT record_json FROM review_snapshots WHERE snapshot_id=?1",
                    [&snapshot.snapshot_id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            if let Some(existing) = existing {
                return if existing == json {
                    Ok(())
                } else {
                    Err("review snapshot id reuse refused".into())
                };
            }
            tx.execute(
                "INSERT INTO review_snapshots VALUES(?1,?2,?3,?4,?5)",
                params![
                    snapshot.snapshot_id,
                    snapshot.task_id,
                    snapshot.authority_revision,
                    snapshot.snapshot_hash,
                    json
                ],
            )
            .map_err(|e| e.to_string())?;
            for a in &snapshot.artifacts {
                if let Some(name) = &a.archived_name {
                    let audience = if a.audience.is_empty() {
                        &snapshot.audience
                    } else {
                        &a.audience
                    };
                    tx.execute(
                        "INSERT INTO review_artifact_audiences VALUES(?1,?2,?3,?4,?5)",
                        params![
                            snapshot.snapshot_id,
                            snapshot.task_id,
                            a.agent_id,
                            name,
                            encode(audience)?
                        ],
                    )
                    .map_err(|e| e.to_string())?;
                }
            }
            Ok(())
        })
        .await
    }
    pub async fn review_snapshot(&self, id: &str) -> Result<Option<ReviewSnapshot>, String> {
        self.with_connection(|c| {
            let raw: Option<String> = c
                .query_row(
                    "SELECT record_json FROM review_snapshots WHERE snapshot_id=?1",
                    [id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            raw.map(decode).transpose()
        })
        .await
    }
    pub async fn latest_review_snapshot(
        &self,
        task: &str,
    ) -> Result<Option<ReviewSnapshot>, String> {
        self.with_connection(|c| {
            let raw: Option<String> = c
                .query_row(
                    "SELECT record_json FROM review_snapshots WHERE task_id=?1 ORDER BY rowid DESC LIMIT 1",
                    [task],
                    |r| r.get(0)
                )
                .optional()
                .map_err(|e| e.to_string())?;
            raw.map(decode).transpose()
        })
        .await
    }
    /// Caller already revalidated task/hash/files/audience and authenticated identity.
    pub async fn accept_review_snapshot(
        &self,
        acceptance: &ReviewAcceptance,
    ) -> Result<(), String> {
        if acceptance.accepted_by.is_empty() {
            return Err("review accepting identity required".into());
        }
        self.with_transaction(|tx| {
            let snapshot_hash: String = tx
                .query_row(
                    "SELECT snapshot_hash FROM review_snapshots WHERE snapshot_id=?1",
                    [&acceptance.snapshot_id],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            if snapshot_hash != acceptance.snapshot_hash {
                return Err("review acceptance snapshot mismatch".into());
            }
            let json = encode(acceptance)?;
            let old: Option<String> = tx
                .query_row(
                    "SELECT record_json FROM review_acceptances WHERE snapshot_id=?1",
                    [&acceptance.snapshot_id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            if let Some(old) = old {
                return if old == json {
                    Ok(())
                } else {
                    Err("snapshot already accepted".into())
                };
            }
            tx.execute(
                "INSERT INTO review_acceptances VALUES(?1,?2,?3)",
                params![acceptance.snapshot_id, acceptance.snapshot_hash, json],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
    }
    pub async fn save_draft(&self, draft: &WorkflowDraft) -> Result<(), String> {
        self.with_transaction(|tx| {
            let raw: String = tx
                .query_row(
                    "SELECT record_json FROM review_snapshots WHERE snapshot_id=?1",
                    [&draft.source_snapshot_id],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            let snapshot: ReviewSnapshot = decode(raw)?;
            draft.validate(&snapshot)?;
            let json = encode(draft)?;
            let old: Option<String> = tx
                .query_row(
                    "SELECT record_json FROM workflow_drafts WHERE draft_id=?1 AND revision=?2",
                    params![draft.draft_id, draft.revision],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            if let Some(old) = old {
                return if old == json {
                    Ok(())
                } else {
                    Err("draft revision reuse refused".into())
                };
            }
            tx.execute(
                "INSERT INTO workflow_drafts VALUES(?1,?2,?3,?4,1,?5)",
                params![
                    draft.draft_id,
                    draft.revision,
                    draft.owner,
                    draft.draft_hash,
                    json
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
    }
    pub async fn draft(&self, id: &str, revision: i64) -> Result<Option<WorkflowDraft>, String> {
        self.with_connection(|c| {
            let raw: Option<String> = c
                .query_row(
                    "SELECT record_json FROM workflow_drafts WHERE draft_id=?1 AND revision=?2",
                    params![id, revision],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            raw.map(decode).transpose()
        })
        .await
    }
    pub async fn list_drafts(
        &self,
        owner: &str,
        task: Option<&str>,
    ) -> Result<Vec<WorkflowDraft>, String> {
        Ok(self.list_drafts_page(owner, task, None).await?.0)
    }
    /// Cursor is an opaque host row position; owner and task filters apply
    /// before paging. Every page preserves both authorization scopes.
    pub async fn list_drafts_page(
        &self,
        owner: &str,
        task: Option<&str>,
        before: Option<i64>,
    ) -> Result<(Vec<WorkflowDraft>, Option<i64>), String> {
        self.with_connection(|c| {
            let mut q = c
                .prepare("SELECT rowid,record_json FROM workflow_drafts WHERE owner=?1 AND (?2 IS NULL
                    OR json_extract(record_json,'$.source_task')=?2) AND (?3 IS NULL OR rowid<?3) ORDER BY rowid DESC
                    LIMIT 101")
                .map_err(|e| e.to_string())?;
            let rows = q
                .query_map(params![owner, task, before], |r|
                    Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
                )
                .map_err(|e| e.to_string())?;
            let mut out = Vec::new();
            for row in rows {
                let (id, raw) = row.map_err(|e| e.to_string())?;
                out.push((id, decode::<WorkflowDraft>(raw)?));
            }
            let next = if out.len() > 100 {
                out.truncate(100);
                out.last().map(|(id, _)| *id)
            } else {
                None
            };
            Ok((out.into_iter().map(|(_, d)| d).collect(), next))
        })
        .await
    }
    /// Evidence must be obtained by WorkflowService::fixture_evidence, not RPC JSON.
    pub async fn save_fixture_evidence(
        &self,
        e: &FixtureRunEvidence,
    ) -> Result<Vec<FixtureAssertionResult>, String> {
        self.with_transaction(|tx| {
            let raw: String = tx
                .query_row(
                    "SELECT record_json FROM workflow_drafts WHERE draft_id=?1 AND revision=?2",
                    params![e.draft_id, e.revision],
                    |r| r.get(0)
                )
                .map_err(|e| e.to_string())?;
            let d: WorkflowDraft = decode(raw)?;
            let f = d
                .fixtures
                .iter()
                .find(|f| f.fixture_id == e.fixture_id)
                .ok_or("unknown draft fixture")?;
            if e.workflow_hash != d.revision_hash
                || e.skill_hash != d.definition.skill_revision_hash
                || e.kind != f.kind
                || e.input_hash != f.input_hash
                || e.assertion_hash != f.assertion_hash
                || e.policy_revision != d.creator_grant.policy_revision
                || e.run_id.is_empty()
            {
                return Err("fixture evidence fixed material mismatch".into());
            }
            let assertions = evaluate_fixture(&f.assertions, e);
            let json = encode(e)?;
            let ajson = encode(&assertions)?;
            let old: Option<(String, String)> = tx
                .query_row(
                    "SELECT record_json,assertions_json FROM workflow_fixture_results WHERE run_id=?1",
                    [&e.run_id],
                    |r| Ok((r.get(0)?, r.get(1)?))
                )
                .optional()
                .map_err(|e| e.to_string())?;
            if let Some((old, old_assertions)) = old {
                if old != json || old_assertions != ajson {
                    return Err("fixture run evidence replacement refused".into());
                }
                return Ok(assertions);
            }
            tx.execute(
                "INSERT INTO workflow_fixture_results VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![
                    e.run_id,
                    e.draft_id,
                    e.revision,
                    e.fixture_id,
                    crate::approval::payload_hash(
                        &serde_json::to_value(e).map_err(|x| x.to_string())?
                    ),
                    json,
                    ajson
                ]
            )
            .map_err(|e| e.to_string())?;
            Ok(assertions)
        })
        .await
    }
    pub async fn fixture_results(
        &self,
        id: &str,
        revision: i64,
    ) -> Result<Vec<(FixtureRunEvidence, Vec<FixtureAssertionResult>)>, String> {
        self.with_connection(|c| {
            let mut q = c
                .prepare("SELECT record_json,assertions_json FROM workflow_fixture_results WHERE draft_id=?1
                    AND revision=?2 ORDER BY rowid DESC")
                .map_err(|e| e.to_string())?;
            let rows = q
                .query_map(params![id, revision], |r|
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                )
                .map_err(|e| e.to_string())?;
            let mut out = Vec::new();
            for r in rows {
                let (e, a) = r.map_err(|e| e.to_string())?;
                out.push((decode(e)?, decode(a)?));
            }
            Ok(out)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot() -> ReviewSnapshot {
        let mut s = ReviewSnapshot {
            schema_version: 1,
            snapshot_id: "snapshot".into(),
            snapshot_hash: String::new(),
            task_id: "task".into(),
            authority_revision: 1,
            authority_snapshot_hash: "contract".into(),
            criteria_ledger: None,
            artifacts: vec![],
            captured_at: "2026-10-04T00:00:00Z".into(),
            audience: vec!["user:alice".into()],
            gaps: vec![],
            waiting_for: None,
            next_check_at: None,
        };
        s.snapshot_hash = s.compute_hash();
        s
    }
    #[tokio::test]
    async fn immutable_snapshot_and_acceptance_survive_reopen() {
        let home = tempfile::tempdir().unwrap();
        let store = WorkflowStore::open(home.path()).unwrap();
        store.initialize_evidence().await.unwrap();
        let s = snapshot();
        store.save_review_snapshot(&s).await.unwrap();
        store.save_review_snapshot(&s).await.unwrap();
        let mut changed = s.clone();
        changed.authority_revision = 2;
        changed.snapshot_hash = changed.compute_hash();
        assert!(store.save_review_snapshot(&changed).await.is_err());
        let a = ReviewAcceptance {
            snapshot_id: s.snapshot_id.clone(),
            snapshot_hash: s.snapshot_hash.clone(),
            accepted_by: "alice".into(),
            accepted_at: "2026-10-04T00:00:00Z".into(),
        };
        store.accept_review_snapshot(&a).await.unwrap();
        assert!(
            store
                .with_transaction(|tx| tx
                    .execute("UPDATE review_snapshots SET snapshot_hash='evil'", [])
                    .map(|_| ())
                    .map_err(|e| e.to_string()))
                .await
                .is_err()
        );
        // L-2: an acceptance cannot be removed any more than edited.
        assert!(
            store
                .with_transaction(|tx| tx
                    .execute("DELETE FROM review_acceptances", [])
                    .map(|_| ())
                    .map_err(|e| e.to_string()))
                .await
                .is_err()
        );
        assert!(store.review_acceptance("snapshot").await.unwrap().is_some());
        drop(store);
        let reopened = WorkflowStore::open(home.path()).unwrap();
        reopened.initialize_evidence().await.unwrap();
        assert_eq!(reopened.review_snapshot("snapshot").await.unwrap(), Some(s));
    }
    #[tokio::test]
    async fn task_filter_precedes_global_owner_limit() {
        let store = WorkflowStore::open_in_memory().unwrap();
        store.initialize_evidence().await.unwrap();
        store.with_transaction(|tx|{
            for index in 0..103 {
                let task=if index==0 {"old-task"}else{"other-task"};let id=format!("draft-{index}");
                let raw=serde_json::json!({
                    "schema_version": 1,
                    "draft_id": id,
                    "revision": 1,
                    "owner": "sales",
                    "source_task": task,
                    "skill_id": "skill",
                    "source_snapshot_id": "snapshot",
                    "source_evidence_hash": "source",
                    "revision_hash": "revision",
                    "definition": {
                                      "schema_version": 1,
                                      "workflow_id": id,
                                      "revision": 1,
                                      "skill_revision_hash": "skillhash",
                                      "input_schema": {"type":"null"},
                                      "output_schema": {"type":"null"},
                                      "required_capabilities": [],
                                      "steps": []
                                  },
                    "source_data": [],
                    "fixtures": [],
                    "creator_grant": {"actor":"sales","allowed_tools":[],"policy_revision":"policy"},
                    "effect_templates": {},
                    "audience": [],
                    "budget": {"per_run_micros":1,"monthly_micros":1,"max_consecutive_failures":1},
                    "input_max_age_seconds": 1,
                    "timezone": "Asia/Taipei",
                    "routine": null,
                    "stop_conditions": [],
                    "created_at": "2026-10-04T00:00:00Z",
                    "disabled": true,
                    "review_status": "draft",
                    "draft_hash": "hash"
                });
                tx.execute(
                    "INSERT INTO workflow_drafts VALUES(?1,1,'sales','hash',1,?2)",
                    params![id, raw.to_string()]
                )
                .map_err(|e| e.to_string())?;
            }
            Ok(())
        }).await.unwrap();
        let old = store.list_drafts("sales", Some("old-task")).await.unwrap();
        assert_eq!(old.len(), 1);
        assert_eq!(old[0].draft_id, "draft-0");
        assert!(
            store
                .list_drafts("foreign", Some("old-task"))
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .list_drafts("sales", Some("unknown"))
                .await
                .unwrap()
                .is_empty()
        );
        let (first, next) = store
            .list_drafts_page("sales", Some("other-task"), None)
            .await
            .unwrap();
        assert_eq!(first.len(), 100);
        assert_eq!(first[0].draft_id, "draft-102");
        let (second, end) = store
            .list_drafts_page("sales", Some("other-task"), next)
            .await
            .unwrap();
        assert_eq!(second.len(), 2);
        assert!(end.is_none());
        assert!(
            first
                .iter()
                .all(|a| second.iter().all(|b| a.draft_id != b.draft_id))
        );
    }
    #[tokio::test]
    async fn newer_owner_schema_fails_closed() {
        let store = WorkflowStore::open_in_memory().unwrap();
        store.initialize_evidence().await.unwrap();
        store
            .with_transaction(|tx| {
                tx.execute(
                    "UPDATE workflow_schema_meta SET version=99 WHERE owner='evidence'",
                    [],
                )
                .map(|_| ())
                .map_err(|e| e.to_string())
            })
            .await
            .unwrap();
        assert!(store.initialize_evidence().await.is_err());
    }
}

impl WorkflowStore {
    /// Every binding narrows access. Multiple task/snapshot constraints all apply.
    pub async fn artifact_audiences(
        &self,
        agent: &str,
        name: &str,
    ) -> Result<Vec<(String, Vec<String>)>, String> {
        self.with_connection(|c| {
            let mut q = c
                .prepare("SELECT task_id,audience_json FROM review_artifact_audiences WHERE agent_id=?1
                    AND archived_name=?2 ORDER BY rowid")
                .map_err(|e| e.to_string())?;
            let rows = q
                .query_map(params![agent, name], |r|
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                )
                .map_err(|e| e.to_string())?;
            let mut out = Vec::new();
            for r in rows {
                let (task, a) = r.map_err(|e| e.to_string())?;
                out.push((task, decode(a)?));
            }
            Ok(out)
        })
        .await
    }
    /// Commit privacy metadata before an archived file becomes visible.
    pub async fn bind_artifact_audience(
        &self,
        binding_id: &str,
        task: &str,
        agent: &str,
        name: &str,
        audience: &[String],
    ) -> Result<(), String> {
        let json = encode(&audience)?;
        self.with_transaction(|tx| {
            let old: Option<(String, String)> = tx
                .query_row(
                    "SELECT task_id,audience_json FROM review_artifact_audiences WHERE snapshot_id=?1 AND agent_id=?2
                        AND archived_name=?3",
                    params![binding_id, agent, name],
                    |r| Ok((r.get(0)?, r.get(1)?))
                )
                .optional()
                .map_err(|e| e.to_string())?;
            if let Some((t, a)) = old {
                return if t == task && a == json {
                    Ok(())
                } else {
                    Err("artifact audience binding replacement refused".into())
                };
            }
            tx.execute(
                "INSERT INTO review_artifact_audiences VALUES(?1,?2,?3,?4,?5)",
                params![binding_id, task, agent, name, json]
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
    }
    pub async fn review_acceptance(&self, id: &str) -> Result<Option<ReviewAcceptance>, String> {
        self.with_connection(|c| {
            let raw: Option<String> = c
                .query_row(
                    "SELECT record_json FROM review_acceptances WHERE snapshot_id=?1",
                    [id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            raw.map(decode).transpose()
        })
        .await
    }
}

/// Pure query shared with the CLI read-only bridge. A definition identity maps
/// to exactly one immutable draft; neither caller input nor JSON IDs override it.
pub(crate) fn lookup_draft_for_workflow(
    c: &rusqlite::Connection,
    workflow_id: &str,
    revision: i64,
) -> Result<Option<WorkflowDraft>, String> {
    let mut q = c
        .prepare("SELECT draft_id,revision,draft_hash,record_json FROM workflow_drafts WHERE json_extract(record_json,
            '$.definition.workflow_id')=?1 AND json_extract(record_json,'$.definition.revision')=?2 LIMIT 2")
        .map_err(|e| e.to_string())?;
    let rows = q
        .query_map(params![workflow_id, revision], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut result = None;
    for row in rows {
        if result.is_some() {
            return Err("ambiguous immutable workflow draft".into());
        }
        let (id, rev, hash, raw) = row.map_err(|e| e.to_string())?;
        let d: WorkflowDraft = decode(raw)?;
        if d.draft_id != id
            || d.revision != rev
            || d.draft_hash != hash
            || d.compute_hash() != hash
            || d.definition.workflow_id != workflow_id
            || d.definition.revision != revision
            || d.revision != revision
            || d.definition.hash() != d.revision_hash
        {
            return Err("immutable workflow draft material invalid".into());
        }
        let snapshot: String = c
            .query_row(
                "SELECT record_json FROM review_snapshots WHERE snapshot_id=?1",
                [&d.source_snapshot_id],
                |r| r.get(0),
            )
            .map_err(|_| "workflow source snapshot unavailable")?;
        let snapshot: ReviewSnapshot = decode(snapshot)?;
        if snapshot.compute_hash() != snapshot.snapshot_hash {
            return Err("workflow source snapshot invalid".into());
        }
        d.validate(&snapshot)?;
        result = Some(d);
    }
    Ok(result)
}
impl WorkflowStore {
    pub async fn draft_for_workflow(
        &self,
        workflow_id: &str,
        revision: i64,
    ) -> Result<Option<WorkflowDraft>, String> {
        self.with_connection(|c| lookup_draft_for_workflow(c, workflow_id, revision))
            .await
    }
}
