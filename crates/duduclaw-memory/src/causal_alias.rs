//! Reviewed, scoped variable-name aliases for future causal extraction.
//!
//! Changing an alias never rewrites a historical claim. It demotes claims
//! that depended on the prior mapping and scrubs effects from linked models.

use rusqlite::{OptionalExtension, Transaction, params};
use serde::Serialize;
use uuid::Uuid;

use crate::causal::{CausalStore, CausalStoreError, EvidenceScope, now};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VariableAlias {
    pub scope: EvidenceScope,
    pub alias: String,
    pub canonical_name: String,
    pub review_id: String,
    pub reviewer: String,
    pub reviewed_at: i64,
}

pub(crate) fn resolve_alias_tx(
    tx: &Transaction<'_>,
    scope: &EvidenceScope,
    name: &str,
) -> Result<(String, Option<(String, String)>), CausalStoreError> {
    let mapping: Option<(String, String)> = tx
        .query_row(
            "SELECT canonical_name,review_id FROM causal_variable_aliases
         WHERE tenant_id=?1 AND acl=?2 AND alias=?3",
            params![scope.tenant_id, scope.acl, name],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    Ok(match mapping {
        Some((canonical, review_id)) => (canonical, Some((name.into(), review_id))),
        None => (name.into(), None),
    })
}

fn invalidate_alias_dependents(
    tx: &Transaction<'_>,
    scope: &EvidenceScope,
    alias: &str,
    review_id: &str,
) -> Result<(), CausalStoreError> {
    tx.execute(
        "UPDATE causal_effect_estimates SET estimate=NULL,lower_bound=NULL,upper_bound=NULL,
         diagnostics_json='{}',identification_state='alias_changed'
         WHERE model_id IN (
             SELECT me.model_id FROM causal_model_edges me
             JOIN causal_claim_alias_dependencies d ON d.claim_id=me.claim_id
             JOIN causal_claims c ON c.id=d.claim_id
             WHERE d.alias=?1 AND d.review_id=?2 AND c.tenant_id=?3 AND c.acl=?4
         )",
        params![alias, review_id, scope.tenant_id, scope.acl],
    )?;
    tx.execute(
        "UPDATE causal_claims SET review_state='needs_review',reviewer=NULL,reviewed_at=NULL
         WHERE tenant_id=?3 AND acl=?4 AND review_state NOT IN ('rejected','superseded') AND id IN (
             SELECT claim_id FROM causal_claim_alias_dependencies
             WHERE alias=?1 AND review_id=?2
         )",
        params![alias, review_id, scope.tenant_id, scope.acl],
    )?;
    Ok(())
}

impl CausalStore {
    /// Resolve an exact name within the caller's scope. An unreviewed name is
    /// returned unchanged; no fuzzy matching or cross-scope lookup occurs.
    pub fn resolve_variable_name(
        &self,
        scope: &EvidenceScope,
        name: &str,
    ) -> Result<String, CausalStoreError> {
        if !scope.valid() || name.trim().is_empty() {
            return Err(CausalStoreError::InvalidInput);
        }
        let mut conn = self.open()?;
        let tx = conn.transaction()?;
        let (resolved, _) = resolve_alias_tx(&tx, scope, name)?;
        Ok(resolved)
    }

    pub fn list_variable_aliases(
        &self,
        scope: &EvidenceScope,
        limit: usize,
    ) -> Result<Vec<VariableAlias>, CausalStoreError> {
        if !scope.valid() {
            return Err(CausalStoreError::InvalidInput);
        }
        let conn = self.open()?;
        let mut stmt = conn.prepare(
            "SELECT alias,canonical_name,review_id,reviewer,reviewed_at
             FROM causal_variable_aliases WHERE tenant_id=?1 AND acl=?2
             ORDER BY alias LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            params![scope.tenant_id, scope.acl, limit.clamp(1, 100) as i64],
            |row| {
                Ok(VariableAlias {
                    scope: scope.clone(),
                    alias: row.get(0)?,
                    canonical_name: row.get(1)?,
                    review_id: row.get(2)?,
                    reviewer: row.get(3)?,
                    reviewed_at: row.get(4)?,
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// A human maps an exact source name to an already registered canonical
    /// variable. Revisions invalidate claims derived under the old mapping.
    pub fn set_variable_alias(
        &self,
        scope: &EvidenceScope,
        alias: &str,
        canonical_name: &str,
        reviewer: &str,
    ) -> Result<VariableAlias, CausalStoreError> {
        self.set_variable_alias_checked(scope, alias, canonical_name, reviewer, None)
    }

    /// `None` expects a new alias; `Some(id)` expects that exact current
    /// review version. This is the form exposed to human curation clients.
    pub fn set_variable_alias_if_review(
        &self,
        scope: &EvidenceScope,
        alias: &str,
        canonical_name: &str,
        reviewer: &str,
        expected_review_id: Option<&str>,
    ) -> Result<VariableAlias, CausalStoreError> {
        self.set_variable_alias_checked(
            scope,
            alias,
            canonical_name,
            reviewer,
            Some(expected_review_id),
        )
    }

    fn set_variable_alias_checked(
        &self,
        scope: &EvidenceScope,
        alias: &str,
        canonical_name: &str,
        reviewer: &str,
        expected_review_id: Option<Option<&str>>,
    ) -> Result<VariableAlias, CausalStoreError> {
        if !scope.valid()
            || [alias, canonical_name, reviewer]
                .iter()
                .any(|value| value.trim().is_empty() || *value != value.trim() || value.len() > 256)
            || alias == canonical_name
        {
            return Err(CausalStoreError::InvalidInput);
        }
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let canonical_exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM causal_variables
             WHERE tenant_id=?1 AND acl=?2 AND name=?3)",
            params![scope.tenant_id, scope.acl, canonical_name],
            |row| row.get(0),
        )?;
        let alias_is_variable: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM causal_variables
             WHERE tenant_id=?1 AND acl=?2 AND name=?3)",
            params![scope.tenant_id, scope.acl, alias],
            |row| row.get(0),
        )?;
        let canonical_is_alias: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM causal_variable_aliases
             WHERE tenant_id=?1 AND acl=?2 AND alias=?3)",
            params![scope.tenant_id, scope.acl, canonical_name],
            |row| row.get(0),
        )?;
        if !canonical_exists || alias_is_variable || canonical_is_alias {
            return Err(CausalStoreError::InvalidInput);
        }
        let existing: Option<(String, String, String, i64)> = tx
            .query_row(
                "SELECT canonical_name,review_id,reviewer,reviewed_at
             FROM causal_variable_aliases WHERE tenant_id=?1 AND acl=?2 AND alias=?3",
                params![scope.tenant_id, scope.acl, alias],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        if expected_review_id
            .is_some_and(|expected| existing.as_ref().map(|(_, id, _, _)| id.as_str()) != expected)
        {
            return Err(CausalStoreError::Conflict);
        }
        if let Some((old_canonical, old_review_id, old_reviewer, old_reviewed_at)) = &existing {
            if old_canonical == canonical_name {
                return Ok(VariableAlias {
                    scope: scope.clone(),
                    alias: alias.into(),
                    canonical_name: old_canonical.clone(),
                    review_id: old_review_id.clone(),
                    reviewer: old_reviewer.clone(),
                    reviewed_at: *old_reviewed_at,
                });
            }
            invalidate_alias_dependents(&tx, scope, alias, old_review_id)?;
        }
        let review_id = Uuid::new_v4().to_string();
        let reviewed_at = now();
        tx.execute(
            "INSERT INTO causal_alias_reviews
             (id,tenant_id,acl,alias,canonical_name,decision,reviewer,reviewed_at)
             VALUES (?1,?2,?3,?4,?5,'mapped',?6,?7)",
            params![
                review_id,
                scope.tenant_id,
                scope.acl,
                alias,
                canonical_name,
                reviewer,
                reviewed_at
            ],
        )?;
        tx.execute(
            "INSERT INTO causal_variable_aliases
             (tenant_id,acl,alias,canonical_name,review_id,reviewer,reviewed_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7)
             ON CONFLICT(tenant_id,acl,alias) DO UPDATE SET
             canonical_name=excluded.canonical_name,review_id=excluded.review_id,
             reviewer=excluded.reviewer,reviewed_at=excluded.reviewed_at",
            params![
                scope.tenant_id,
                scope.acl,
                alias,
                canonical_name,
                review_id,
                reviewer,
                reviewed_at
            ],
        )?;
        tx.commit()?;
        Ok(VariableAlias {
            scope: scope.clone(),
            alias: alias.into(),
            canonical_name: canonical_name.into(),
            review_id,
            reviewer: reviewer.into(),
            reviewed_at,
        })
    }

    pub fn revoke_variable_alias(
        &self,
        scope: &EvidenceScope,
        alias: &str,
        reviewer: &str,
    ) -> Result<(), CausalStoreError> {
        self.revoke_variable_alias_checked(scope, alias, reviewer, None)
    }

    pub fn revoke_variable_alias_if_review(
        &self,
        scope: &EvidenceScope,
        alias: &str,
        reviewer: &str,
        expected_review_id: &str,
    ) -> Result<(), CausalStoreError> {
        if expected_review_id.trim().is_empty() {
            return Err(CausalStoreError::InvalidInput);
        }
        self.revoke_variable_alias_checked(scope, alias, reviewer, Some(expected_review_id))
    }

    fn revoke_variable_alias_checked(
        &self,
        scope: &EvidenceScope,
        alias: &str,
        reviewer: &str,
        expected_review_id: Option<&str>,
    ) -> Result<(), CausalStoreError> {
        if !scope.valid() || alias.trim().is_empty() || reviewer.trim().is_empty() {
            return Err(CausalStoreError::InvalidInput);
        }
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let review_id: String = tx
            .query_row(
                "SELECT review_id FROM causal_variable_aliases
             WHERE tenant_id=?1 AND acl=?2 AND alias=?3",
                params![scope.tenant_id, scope.acl, alias],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(CausalStoreError::NotFound)?;
        if expected_review_id.is_some_and(|expected| expected != review_id) {
            return Err(CausalStoreError::Conflict);
        }
        invalidate_alias_dependents(&tx, scope, alias, &review_id)?;
        tx.execute(
            "DELETE FROM causal_variable_aliases
             WHERE tenant_id=?1 AND acl=?2 AND alias=?3",
            params![scope.tenant_id, scope.acl, alias],
        )?;
        tx.execute(
            "INSERT INTO causal_alias_reviews
             (id,tenant_id,acl,alias,canonical_name,decision,reviewer,reviewed_at)
             VALUES (?1,?2,?3,?4,NULL,'revoked',?5,?6)",
            params![
                Uuid::new_v4().to_string(),
                scope.tenant_id,
                scope.acl,
                alias,
                reviewer,
                now()
            ],
        )?;
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::causal::{ClaimModality, EvidenceStance, ProposedCausalClaim};
    use crate::causal_model::{ModelDraft, VariableKind};

    #[test]
    fn reviewed_alias_canonicalizes_future_claims_and_revision_invalidates_dependents() {
        let dir = tempfile::tempdir().unwrap();
        let store = CausalStore::new(dir.path().join("causal.db"));
        let scope = EvidenceScope {
            tenant_id: "a".into(),
            acl: "private".into(),
        };
        let other = EvidenceScope {
            tenant_id: "b".into(),
            acl: "private".into(),
        };
        let staffing = store
            .register_variable(
                &scope,
                "staffing",
                "v1",
                "staff on shift",
                "people",
                VariableKind::Count,
            )
            .unwrap();
        store
            .register_variable(
                &scope,
                "capacity",
                "v1",
                "daily capacity",
                "tickets",
                VariableKind::Count,
            )
            .unwrap();
        let backlog = store
            .register_variable(
                &scope,
                "backlog",
                "v1",
                "unresolved tickets",
                "tickets",
                VariableKind::Count,
            )
            .unwrap();
        let mapping = store
            .set_variable_alias_if_review(&scope, "headcount", "staffing", "reviewer-1", None)
            .unwrap();
        assert!(matches!(
            store.set_variable_alias_if_review(&scope, "headcount", "capacity", "reviewer-2", None),
            Err(CausalStoreError::Conflict)
        ));
        assert_eq!(
            store
                .set_variable_alias(&scope, "headcount", "staffing", "reviewer-2")
                .unwrap()
                .review_id,
            mapping.review_id
        );
        assert_eq!(
            store.resolve_variable_name(&scope, "headcount").unwrap(),
            "staffing"
        );
        assert_eq!(
            store.resolve_variable_name(&other, "headcount").unwrap(),
            "headcount"
        );
        assert_eq!(store.list_variable_aliases(&scope, 10).unwrap().len(), 1);
        assert!(matches!(
            store.set_variable_alias(&scope, "staffing", "capacity", "reviewer"),
            Err(CausalStoreError::InvalidInput)
        ));
        assert!(matches!(
            store.register_variable(
                &scope,
                "headcount",
                "v1",
                "bad collision",
                "people",
                VariableKind::Count
            ),
            Err(CausalStoreError::InvalidInput)
        ));
        let text = "headcount reduced backlog";
        let source = store
            .add_artifact(&scope, "ticket", "t1", "v1", "thread-1", text, 1, i64::MAX)
            .unwrap();
        let proposal = ProposedCausalClaim {
            cause_variable: "headcount".into(),
            effect_variable: "backlog".into(),
            lag_min_seconds: 0,
            lag_max_seconds: 86_400,
            modality: ClaimModality::Asserted,
            stance: EvidenceStance::Supports,
            span_start: 0,
            span_end: 9,
            excerpt: "headcount".into(),
            speaker_id: None,
            context: serde_json::json!({}),
        };
        let (claim, evidence) = store
            .ingest_extracted_claim(&scope, &source.id, "Why?", "v1", &proposal)
            .unwrap();
        assert_eq!(claim.cause_variable, "staffing");
        assert_eq!(evidence.excerpt, "headcount");
        assert!(claim.context_json.contains("original_variable_names"));
        store
            .review_claim(&scope, &claim.id, "reviewer", true)
            .unwrap();
        let model = store
            .create_model(
                &scope,
                &ModelDraft {
                    name: "pilot".into(),
                    version: "v1".into(),
                    treatment_variable_id: staffing.id.clone(),
                    outcome_variable_id: backlog.id.clone(),
                    population: "support".into(),
                    window_start: 0,
                    window_end: 100,
                    variable_ids: vec![staffing.id, backlog.id],
                    claim_ids: vec![claim.id.clone()],
                },
            )
            .unwrap();
        let conn = rusqlite::Connection::open(store.path()).unwrap();
        conn.execute(
            "INSERT INTO causal_effect_estimates
             (id,model_id,data_snapshot_id,method,code_sha256,estimate,lower_bound,upper_bound,
              diagnostics_json,identification_state,created_at)
             VALUES ('effect',?1,?2,'test','test',1.0,0.5,1.5,'{}','test',1)",
            params![model.id, source.id],
        )
        .unwrap();
        let revised = store
            .set_variable_alias_if_review(
                &scope,
                "headcount",
                "capacity",
                "reviewer-2",
                Some(&mapping.review_id),
            )
            .unwrap();
        assert_ne!(revised.review_id, mapping.review_id);
        assert_eq!(
            store.claim_state(&scope, &claim.id).unwrap(),
            "needs_review"
        );
        assert!(matches!(
            store.review_claim(&scope, &claim.id, "reviewer", true),
            Err(CausalStoreError::Conflict)
        ));
        let (estimate, state): (Option<f64>, String) = conn.query_row(
            "SELECT estimate,identification_state FROM causal_effect_estimates WHERE id='effect'",
            [], |row| Ok((row.get(0)?,row.get(1)?)),
        ).unwrap();
        assert!(estimate.is_none());
        assert_eq!(state, "alias_changed");
        let (new_claim, new_evidence) = store
            .ingest_extracted_claim(&scope, &source.id, "Why?", "v1", &proposal)
            .unwrap();
        assert_ne!(new_claim.id, claim.id);
        assert_eq!(new_claim.cause_variable, "capacity");
        assert_eq!(new_evidence.excerpt, "headcount");
        assert!(matches!(
            store.revoke_variable_alias_if_review(
                &scope,
                "headcount",
                "reviewer-3",
                &mapping.review_id
            ),
            Err(CausalStoreError::Conflict)
        ));
        store
            .revoke_variable_alias_if_review(&scope, "headcount", "reviewer-3", &revised.review_id)
            .unwrap();
        assert_eq!(
            store.resolve_variable_name(&scope, "headcount").unwrap(),
            "headcount"
        );
        assert_eq!(
            store.claim_state(&scope, &new_claim.id).unwrap(),
            "needs_review"
        );
        assert!(matches!(
            store.review_claim(&scope, &new_claim.id, "reviewer", true),
            Err(CausalStoreError::Conflict)
        ));
        assert!(store.list_variable_aliases(&scope, 10).unwrap().is_empty());
        let restored = store
            .set_variable_alias_if_review(&scope, "headcount", "capacity", "reviewer-4", None)
            .unwrap();
        assert_ne!(restored.review_id, revised.review_id);
        let (restored_claim, _) = store
            .ingest_extracted_claim(&scope, &source.id, "Why?", "v1", &proposal)
            .unwrap();
        assert_ne!(restored_claim.id, new_claim.id);
        store
            .review_claim(&scope, &restored_claim.id, "reviewer-4", true)
            .unwrap();
    }
}
