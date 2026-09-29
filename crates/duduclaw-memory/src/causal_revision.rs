//! Immutable, source-verified revisions of human-curated causal claims.

use std::collections::HashSet;

use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::causal::{CausalStore, CausalStoreError, ClaimModality, EvidenceScope, now};
use crate::causal_alias::resolve_alias_tx;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimRevisionInput {
    pub expected_state: String,
    pub cause_variable: String,
    pub effect_variable: String,
    pub lag_min_seconds: i64,
    pub lag_max_seconds: i64,
    pub modality: ClaimModality,
    pub context: serde_json::Value,
    pub evidence_ids: Vec<String>,
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClaimRevision {
    pub id: String,
    pub old_claim_id: String,
    pub new_claim_id: String,
    pub reviewer: String,
    pub note: String,
    pub revised_at: i64,
}

impl CausalStore {
    /// Read bounded revision history for either side of a claim transition.
    pub fn claim_revisions(
        &self,
        scope: &EvidenceScope,
        claim_id: &str,
    ) -> Result<Vec<ClaimRevision>, CausalStoreError> {
        self.read_claim(scope, claim_id)?;
        let conn = self.open()?;
        let mut stmt = conn.prepare(
            "SELECT r.id,r.old_claim_id,r.new_claim_id,r.reviewer,r.note,r.revised_at
             FROM causal_claim_revisions r
             JOIN causal_claims old ON old.id=r.old_claim_id
             JOIN causal_claims new ON new.id=r.new_claim_id
             WHERE (r.old_claim_id=?1 OR r.new_claim_id=?1)
               AND old.tenant_id=?2 AND old.acl=?3
               AND new.tenant_id=?2 AND new.acl=?3
             ORDER BY r.revised_at DESC,r.rowid DESC LIMIT 100",
        )?;
        let rows = stmt.query_map(params![claim_id, scope.tenant_id, scope.acl], |row| {
            Ok(ClaimRevision {
                id: row.get(0)?,
                old_claim_id: row.get(1)?,
                new_claim_id: row.get(2)?,
                reviewer: row.get(3)?,
                note: row.get(4)?,
                revised_at: row.get(5)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// A revision creates a new candidate and permanently supersedes the old
    /// claim. Evidence is copied only after rechecking original source bytes.
    pub fn revise_claim(
        &self,
        scope: &EvidenceScope,
        claim_id: &str,
        reviewer: &str,
        input: &ClaimRevisionInput,
    ) -> Result<ClaimRevision, CausalStoreError> {
        let unique: HashSet<&str> = input.evidence_ids.iter().map(String::as_str).collect();
        if !scope.valid()
            || claim_id.trim().is_empty()
            || reviewer.trim().is_empty()
            || input.note.trim().is_empty()
            || input.note.len() > 4096
            || input.cause_variable.trim().is_empty()
            || input.effect_variable.trim().is_empty()
            || input.lag_min_seconds < 0
            || input.lag_max_seconds < input.lag_min_seconds
            || !input.context.is_object()
            || input.evidence_ids.is_empty()
            || input.evidence_ids.len() > 64
            || unique.len() != input.evidence_ids.len()
            || !matches!(
                input.expected_state.as_str(),
                "candidate" | "accepted" | "rejected" | "needs_review"
            )
        {
            return Err(CausalStoreError::InvalidInput);
        }

        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<String> = tx
            .query_row(
                "SELECT review_state FROM causal_claims WHERE id=?1 AND tenant_id=?2 AND acl=?3",
                params![claim_id, scope.tenant_id, scope.acl],
                |row| row.get(0),
            )
            .optional()?;
        let current = current.ok_or(CausalStoreError::NotFound)?;
        if current != input.expected_state {
            return Err(CausalStoreError::Conflict);
        }
        let (cause, cause_alias) = resolve_alias_tx(&tx, scope, &input.cause_variable)?;
        let (effect, effect_alias) = resolve_alias_tx(&tx, scope, &input.effect_variable)?;
        if cause == effect {
            return Err(CausalStoreError::InvalidInput);
        }

        // Validate every selected evidence span before writing any row. A
        // revoked, expired, erased, or digest-mismatched source fails closed.
        let mut selected = Vec::with_capacity(input.evidence_ids.len());
        let mut has_support = false;
        for evidence_id in &input.evidence_ids {
            let row: Option<(
                String,
                i64,
                i64,
                String,
                String,
                Option<String>,
                String,
                String,
                String,
            )> = tx
                .query_row(
                    "SELECT e.artifact_id,e.span_start,e.span_end,e.excerpt,e.stance,
                        e.speaker_id,e.extractor_version,a.content,a.content_sha256
                 FROM causal_evidence e JOIN causal_artifacts a ON a.id=e.artifact_id
                 WHERE e.id=?1 AND e.claim_id=?2 AND a.tenant_id=?3 AND a.acl=?4
                   AND a.invalidated_at IS NULL AND a.retention_at>?5
                   AND NOT EXISTS (SELECT 1 FROM causal_ccr_revoking r
                    WHERE r.tenant_id=a.tenant_id AND r.acl=a.acl
                     AND r.artifact_id=a.id AND r.version=a.version)
                   AND NOT EXISTS (SELECT 1 FROM causal_ccr_revocation_outbox o
                    WHERE o.tenant_id=a.tenant_id AND o.connector='causal'
                     AND o.artifact_id=a.id AND o.version=a.version
                     AND o.delivered_at IS NULL)",
                    params![evidence_id, claim_id, scope.tenant_id, scope.acl, now()],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                            row.get(6)?,
                            row.get(7)?,
                            row.get(8)?,
                        ))
                    },
                )
                .optional()?;
            let (artifact_id, start, end, excerpt, stance, speaker, extractor, content, digest) =
                row.ok_or(CausalStoreError::NotFound)?;
            let start: usize = start
                .try_into()
                .map_err(|_| CausalStoreError::InvalidInput)?;
            let end: usize = end.try_into().map_err(|_| CausalStoreError::InvalidInput)?;
            if content.is_empty()
                || format!("{:x}", Sha256::digest(content.as_bytes())) != digest
                || content.get(start..end) != Some(excerpt.as_str())
            {
                return Err(CausalStoreError::InvalidInput);
            }
            has_support |= stance == "supports";
            selected.push((
                artifact_id,
                start as i64,
                end as i64,
                excerpt,
                stance,
                speaker,
                extractor,
            ));
        }
        if !has_support {
            return Err(CausalStoreError::MissingSupport);
        }

        let revision = ClaimRevision {
            id: Uuid::new_v4().to_string(),
            old_claim_id: claim_id.into(),
            new_claim_id: Uuid::new_v4().to_string(),
            reviewer: reviewer.into(),
            note: input.note.clone(),
            revised_at: now(),
        };
        let mut context = serde_json::json!({
            "revision_of": claim_id,
            "proposal_context": input.context,
        });
        if cause_alias.is_some() || effect_alias.is_some() {
            context["original_variable_names"] = serde_json::json!({
                "cause": input.cause_variable, "effect": input.effect_variable,
            });
            context["alias_review_ids"] = serde_json::json!({
                "cause": cause_alias.as_ref().map(|(_, id)| id),
                "effect": effect_alias.as_ref().map(|(_, id)| id),
            });
        }
        tx.execute(
            "INSERT INTO causal_claims
             (id,tenant_id,acl,cause_variable,effect_variable,lag_min_seconds,
              lag_max_seconds,context_json,modality,created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                revision.new_claim_id,
                scope.tenant_id,
                scope.acl,
                cause,
                effect,
                input.lag_min_seconds,
                input.lag_max_seconds,
                context.to_string(),
                input.modality.as_str(),
                revision.revised_at
            ],
        )?;
        for (artifact_id, start, end, excerpt, stance, speaker, extractor) in selected {
            tx.execute(
                "INSERT INTO causal_evidence
                 (id,claim_id,artifact_id,span_start,span_end,excerpt,stance,speaker_id,extractor_version)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![Uuid::new_v4().to_string(),revision.new_claim_id,artifact_id,
                    start,end,excerpt,stance,speaker,extractor],
            )?;
        }
        for (alias, review_id) in [cause_alias.as_ref(), effect_alias.as_ref()]
            .into_iter()
            .flatten()
        {
            tx.execute(
                "INSERT OR IGNORE INTO causal_claim_alias_dependencies
                 (claim_id,alias,review_id) VALUES (?1,?2,?3)",
                params![revision.new_claim_id, alias, review_id],
            )?;
        }
        tx.execute(
            "UPDATE causal_effect_estimates SET estimate=NULL,lower_bound=NULL,upper_bound=NULL,
             diagnostics_json='{}',identification_state='claim_revised'
             WHERE model_id IN (SELECT model_id FROM causal_model_edges WHERE claim_id=?1)",
            [claim_id],
        )?;
        tx.execute(
            "UPDATE causal_claims SET review_state='superseded' WHERE id=?1 AND tenant_id=?2 AND acl=?3",
            params![claim_id,scope.tenant_id,scope.acl],
        )?;
        // This index is a reuse cache. Re-extraction must not append new
        // evidence to a permanently superseded historical claim.
        tx.execute(
            "DELETE FROM causal_lineage_claims WHERE claim_id=?1",
            [claim_id],
        )?;
        tx.execute(
            "INSERT INTO causal_claim_revisions
             (id,old_claim_id,new_claim_id,reviewer,note,revised_at)
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                revision.id,
                revision.old_claim_id,
                revision.new_claim_id,
                revision.reviewer,
                revision.note,
                revision.revised_at
            ],
        )?;
        tx.commit()?;
        Ok(revision)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::causal::EvidenceStance;

    #[test]
    fn revision_preserves_history_and_scrubs_dependent_effect() {
        let dir = tempfile::tempdir().unwrap();
        let store = CausalStore::new(dir.path().join("memory.db"));
        let scope = EvidenceScope {
            tenant_id: "t".into(),
            acl: "private".into(),
        };
        let source = store
            .add_artifact(
                &scope,
                "ticket",
                "1",
                "v1",
                "thread",
                "A leads to B",
                1,
                i64::MAX,
            )
            .unwrap();
        let old = store
            .add_claim(
                &scope,
                "A",
                "B",
                0,
                1,
                &serde_json::json!({}),
                ClaimModality::Asserted,
            )
            .unwrap();
        let evidence = store
            .add_evidence(
                &scope,
                &old.id,
                &source.id,
                0,
                1,
                "A",
                EvidenceStance::Supports,
                None,
                "test",
            )
            .unwrap();
        store
            .review_claim(&scope, &old.id, "reviewer", true)
            .unwrap();
        let input = ClaimRevisionInput {
            expected_state: "accepted".into(),
            cause_variable: "A".into(),
            effect_variable: "C".into(),
            lag_min_seconds: 0,
            lag_max_seconds: 2,
            modality: ClaimModality::Speculated,
            context: serde_json::json!({"why":"correction"}),
            evidence_ids: vec![evidence.id.clone()],
            note: "direction review".into(),
        };
        let conn = store.open().unwrap();
        conn.execute(
            "INSERT INTO causal_models
            (id,tenant_id,acl,name,version,treatment_variable_id,outcome_variable_id,
             population,window_start,window_end,created_at)
             VALUES ('model','t','private','test','v1','a','b','all',0,1,1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO causal_model_edges (model_id,claim_id) VALUES ('model',?1)",
            [&old.id],
        )
        .unwrap();
        conn.execute("INSERT INTO causal_effect_estimates
            (id,model_id,data_snapshot_id,method,code_sha256,estimate,lower_bound,upper_bound,
             diagnostics_json,identification_state,created_at)
             VALUES ('effect','model','source','test','hash',1.0,0.5,1.5,'{\"private\":1}','identified',1)", []).unwrap();
        drop(conn);
        let revision = store
            .revise_claim(&scope, &old.id, "editor", &input)
            .unwrap();
        assert_eq!(
            store.claim_revisions(&scope, &old.id).unwrap()[0].id,
            revision.id
        );
        assert_eq!(
            store
                .claim_revisions(&scope, &revision.new_claim_id)
                .unwrap()[0]
                .id,
            revision.id
        );
        assert_eq!(
            store.read_claim(&scope, &old.id).unwrap().review_state,
            "superseded"
        );
        let candidate = store.read_claim(&scope, &revision.new_claim_id).unwrap();
        assert_eq!(candidate.review_state, "candidate");
        assert_eq!(candidate.effect_variable, "C");
        assert_eq!(
            store
                .evidence_for_claim(&scope, &candidate.id)
                .unwrap()
                .len(),
            1
        );
        assert!(matches!(
            store.review_claim(&scope, &old.id, "reviewer", true),
            Err(CausalStoreError::Conflict)
        ));
        assert!(matches!(
            store.revise_claim(&scope, &old.id, "editor", &input),
            Err(CausalStoreError::Conflict)
        ));
        let conn = store.open().unwrap();
        let (estimate, diagnostics, state): (Option<f64>, String, String) = conn.query_row(
            "SELECT estimate,diagnostics_json,identification_state FROM causal_effect_estimates WHERE id='effect'",
            [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
        assert_eq!(estimate, None);
        assert_eq!(diagnostics, "{}");
        assert_eq!(state, "claim_revised");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM causal_claim_revisions", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn revoked_evidence_cannot_be_revised_and_transaction_rolls_back() {
        let dir = tempfile::tempdir().unwrap();
        let store = CausalStore::new(dir.path().join("memory.db"));
        let scope = EvidenceScope {
            tenant_id: "t".into(),
            acl: "private".into(),
        };
        let source = store
            .add_artifact(&scope, "ticket", "1", "v1", "thread", "A", 1, i64::MAX)
            .unwrap();
        let old = store
            .add_claim(
                &scope,
                "A",
                "B",
                0,
                1,
                &serde_json::json!({}),
                ClaimModality::Asserted,
            )
            .unwrap();
        let evidence = store
            .add_evidence(
                &scope,
                &old.id,
                &source.id,
                0,
                1,
                "A",
                EvidenceStance::Supports,
                None,
                "test",
            )
            .unwrap();
        let input = ClaimRevisionInput {
            expected_state: "candidate".into(),
            cause_variable: "A".into(),
            effect_variable: "C".into(),
            lag_min_seconds: 0,
            lag_max_seconds: 1,
            modality: ClaimModality::Asserted,
            context: serde_json::json!({}),
            evidence_ids: vec![evidence.id],
            note: "correction".into(),
        };
        store.begin_ccr_revocation(&scope, &source.id).unwrap();
        assert!(matches!(
            store.revise_claim(&scope, &old.id, "editor", &input),
            Err(CausalStoreError::NotFound)
        ));
        store.invalidate_artifact(&scope, &source.id).unwrap();
        assert!(matches!(
            store.revise_claim(&scope, &old.id, "editor", &input),
            Err(CausalStoreError::NotFound)
        ));
        assert_eq!(
            store.read_claim(&scope, &old.id).unwrap().review_state,
            "candidate"
        );
        assert_eq!(store.list_claim_ids(&scope, None, 10).unwrap().len(), 1);
    }
}
