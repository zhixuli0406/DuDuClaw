//! Reviewed, source-backed negative-control outcome exclusion plans.
//! A reviewed DAG and protocol cannot prove the exclusion or shared
//! confounding assumptions; the resulting contrast remains a diagnostic.

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::causal::{CausalStore, CausalStoreError, EvidenceScope, now};
use crate::causal_identify::descendants;
use crate::causal_model::AssumptionVerdict;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NegativeControlReadiness {
    Unknown { reasons: Vec<String> },
    Reviewed { review_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NegativeControlReviewRecord {
    pub id: String,
    pub model_id: String,
    pub variable_id: String,
    pub protocol_artifact_id: String,
    pub protocol_sha256: String,
    pub verdict: String,
    pub rationale: String,
    pub reviewer: String,
    pub reviewed_at: i64,
}

fn variable_is_eligible(
    conn: &Connection,
    scope: &EvidenceScope,
    model_id: &str,
    variable_id: &str,
) -> Result<bool, CausalStoreError> {
    let model: Option<(String, String, String)> = conn.query_row(
        "SELECT m.treatment_variable_id,m.outcome_variable_id,t.name
         FROM causal_models m JOIN causal_variables t ON t.id=m.treatment_variable_id
         WHERE m.id=?1 AND m.tenant_id=?2 AND m.acl=?3",
        params![model_id, scope.tenant_id, scope.acl],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).optional()?;
    let Some((treatment_id, outcome_id, treatment_name)) = model else {
        return Err(CausalStoreError::NotFound);
    };
    if variable_id == treatment_id || variable_id == outcome_id {
        return Ok(false);
    }
    let negative_control: Option<(String, String)> = conn.query_row(
        "SELECT v.name,v.value_kind FROM causal_model_variables mv
         JOIN causal_variables v ON v.id=mv.variable_id
         WHERE mv.model_id=?1 AND mv.variable_id=?2 AND v.tenant_id=?3 AND v.acl=?4",
        params![model_id, variable_id, scope.tenant_id, scope.acl],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?;
    let Some((control_name, kind)) = negative_control else {
        return Ok(false);
    };
    if !matches!(kind.as_str(), "continuous" | "count") {
        return Ok(false);
    }
    let edges = conn.prepare(
        "SELECT c.cause_variable,c.effect_variable FROM causal_model_edges me
         JOIN causal_claims c ON c.id=me.claim_id
         WHERE me.model_id=?1 AND c.tenant_id=?2 AND c.acl=?3",
    )?.query_map(params![model_id, scope.tenant_id, scope.acl], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?.collect::<Result<Vec<_>, _>>()?;
    Ok(!descendants(&edges, &treatment_name).contains(&control_name))
}

impl CausalStore {
    pub fn latest_negative_control_review(
        &self,
        scope: &EvidenceScope,
        model_id: &str,
        variable_id: &str,
    ) -> Result<Option<NegativeControlReviewRecord>, CausalStoreError> {
        if !scope.valid() || model_id.trim().is_empty() || variable_id.trim().is_empty() {
            return Err(CausalStoreError::InvalidInput);
        }
        let conn = self.open()?;
        let model_exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM causal_models WHERE id=?1 AND tenant_id=?2 AND acl=?3)",
            params![model_id, scope.tenant_id, scope.acl], |row| row.get(0),
        )?;
        if !model_exists { return Err(CausalStoreError::NotFound); }
        conn.query_row(
            "SELECT r.id,r.model_id,r.variable_id,r.protocol_artifact_id,r.protocol_sha256,
                    r.verdict,
                    CASE WHEN a.id IS NOT NULL AND a.invalidated_at IS NULL
                         AND a.retention_at>?5
                         AND NOT EXISTS (SELECT 1 FROM causal_ccr_revoking rev
                          WHERE rev.tenant_id=a.tenant_id AND rev.acl=a.acl
                           AND rev.artifact_id=a.id AND rev.version=a.version)
                         AND NOT EXISTS (SELECT 1 FROM causal_ccr_revocation_outbox o
                          WHERE o.tenant_id=a.tenant_id AND o.connector='causal'
                           AND o.artifact_id=a.id AND o.version=a.version
                           AND o.delivered_at IS NULL)
                    THEN r.rationale ELSE '' END,
                    r.reviewer,r.reviewed_at
             FROM causal_negative_control_reviews r
             JOIN causal_variables v ON v.id=r.variable_id
             LEFT JOIN causal_artifacts a ON a.id=r.protocol_artifact_id
              AND a.tenant_id=v.tenant_id AND a.acl=v.acl
             WHERE r.model_id=?1 AND r.variable_id=?2 AND v.tenant_id=?3 AND v.acl=?4
             ORDER BY r.rowid DESC LIMIT 1",
            params![model_id, variable_id, scope.tenant_id, scope.acl, now()],
            |row| Ok(NegativeControlReviewRecord {
                id: row.get(0)?, model_id: row.get(1)?, variable_id: row.get(2)?,
                protocol_artifact_id: row.get(3)?, protocol_sha256: row.get(4)?,
                verdict: row.get(5)?, rationale: row.get(6)?, reviewer: row.get(7)?,
                reviewed_at: row.get(8)?,
            }),
        ).optional().map_err(Into::into)
    }

    /// Append a human verdict bound to one versioned model variable and an
    /// active, exact-scope protocol source explaining the exclusion claim.
    pub fn review_negative_control(
        &self,
        scope: &EvidenceScope,
        model_id: &str,
        variable_id: &str,
        protocol_artifact_id: &str,
        verdict: AssumptionVerdict,
        rationale: &str,
        reviewer: &str,
    ) -> Result<String, CausalStoreError> {
        self.review_negative_control_checked(
            scope, model_id, variable_id, protocol_artifact_id,
            verdict, rationale, reviewer, None,
        )
    }

    /// Compare-and-set review for interactive curation. `None` means no
    /// prior review; a stale browser submission cannot overwrite a newer one.
    pub fn review_negative_control_if_review(
        &self,
        scope: &EvidenceScope,
        model_id: &str,
        variable_id: &str,
        protocol_artifact_id: &str,
        verdict: AssumptionVerdict,
        rationale: &str,
        reviewer: &str,
        expected_review_id: Option<&str>,
    ) -> Result<String, CausalStoreError> {
        self.review_negative_control_checked(
            scope, model_id, variable_id, protocol_artifact_id,
            verdict, rationale, reviewer, Some(expected_review_id),
        )
    }

    fn review_negative_control_checked(
        &self,
        scope: &EvidenceScope,
        model_id: &str,
        variable_id: &str,
        protocol_artifact_id: &str,
        verdict: AssumptionVerdict,
        rationale: &str,
        reviewer: &str,
        expected_review_id: Option<Option<&str>>,
    ) -> Result<String, CausalStoreError> {
        if !scope.valid()
            || [model_id, variable_id, protocol_artifact_id, reviewer]
                .iter().any(|value| value.trim().is_empty())
            || rationale.trim().chars().count() < 20
        {
            return Err(CausalStoreError::InvalidInput);
        }
        if self.model_state(scope, model_id)? != "approved" {
            return Err(CausalStoreError::InvalidInput);
        }
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if let Some(expected) = expected_review_id {
            let latest: Option<String> = tx.query_row(
                "SELECT id FROM causal_negative_control_reviews
                 WHERE model_id=?1 AND variable_id=?2 ORDER BY rowid DESC LIMIT 1",
                params![model_id, variable_id], |row| row.get(0),
            ).optional()?;
            if latest.as_deref() != expected {
                return Err(CausalStoreError::Conflict);
            }
        }
        if !variable_is_eligible(&tx, scope, model_id, variable_id)? {
            return Err(CausalStoreError::InvalidInput);
        }
        let protocol: Option<(String, String, String)> = tx.query_row(
            "SELECT a.kind,a.content,a.content_sha256 FROM causal_artifacts a
             WHERE a.id=?1 AND a.tenant_id=?2 AND a.acl=?3
               AND a.invalidated_at IS NULL AND a.retention_at>?4
               AND NOT EXISTS (SELECT 1 FROM causal_ccr_revoking r
                WHERE r.tenant_id=a.tenant_id AND r.acl=a.acl
                 AND r.artifact_id=a.id AND r.version=a.version)
               AND NOT EXISTS (SELECT 1 FROM causal_ccr_revocation_outbox o
                WHERE o.tenant_id=a.tenant_id AND o.connector='causal'
                 AND o.artifact_id=a.id AND o.version=a.version
                 AND o.delivered_at IS NULL)",
            params![protocol_artifact_id, scope.tenant_id, scope.acl, now()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).optional()?;
        let Some((kind, content, sha256)) = protocol else {
            return Err(CausalStoreError::NotFound);
        };
        if kind != "negative_control_protocol"
            || content.trim().is_empty()
            || format!("{:x}", Sha256::digest(content.as_bytes())) != sha256
        {
            return Err(CausalStoreError::InvalidInput);
        }
        let id = Uuid::new_v4().to_string();
        let verdict = match verdict {
            AssumptionVerdict::Pass => "pass",
            AssumptionVerdict::Fail => "fail",
            AssumptionVerdict::Unknown => "unknown",
        };
        tx.execute(
            "INSERT INTO causal_negative_control_reviews
             (id,model_id,variable_id,protocol_artifact_id,protocol_sha256,
              verdict,rationale,reviewer,reviewed_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![id, model_id, variable_id, protocol_artifact_id, sha256,
                verdict, rationale, reviewer, now()],
        )?;
        // A changed exclusion judgment invalidates estimates that used this
        // control. Estimates without this control retain their own provenance.
        tx.execute(
            "UPDATE causal_effect_estimates SET estimate=NULL,lower_bound=NULL,upper_bound=NULL,
             diagnostics_json='{}',identification_state='negative_control_review_changed'
             WHERE model_id=?1 AND json_extract(diagnostics_json,
               '$.negative_control.variable_id')=?2",
            params![model_id, variable_id],
        )?;
        tx.commit()?;
        Ok(id)
    }

    /// The newest review for this model/control pair must pass, its bound
    /// protocol must still be active and byte-identical, and the current
    /// reviewed graph must still exclude a treatment-to-control path.
    pub fn negative_control_readiness(
        &self,
        scope: &EvidenceScope,
        model_id: &str,
        variable_id: &str,
        review_id: &str,
    ) -> Result<NegativeControlReadiness, CausalStoreError> {
        if !scope.valid() || [model_id, variable_id, review_id].iter().any(|value| value.trim().is_empty()) {
            return Err(CausalStoreError::InvalidInput);
        }
        if self.model_state(scope, model_id)? != "approved" {
            return Ok(NegativeControlReadiness::Unknown {
                reasons: vec!["negative-control model is not approved".into()],
            });
        }
        let conn = self.open()?;
        if !variable_is_eligible(&conn, scope, model_id, variable_id)? {
            return Ok(NegativeControlReadiness::Unknown {
                reasons: vec!["negative-control variable is absent, unsupported, or a treatment descendant".into()],
            });
        }
        let latest: Option<(String, String, String, String)> = conn.query_row(
            "SELECT id,protocol_artifact_id,protocol_sha256,verdict
             FROM causal_negative_control_reviews
             WHERE model_id=?1 AND variable_id=?2 ORDER BY rowid DESC LIMIT 1",
            params![model_id, variable_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        ).optional()?;
        let Some((latest_id, protocol_id, recorded_sha256, verdict)) = latest else {
            return Ok(NegativeControlReadiness::Unknown {
                reasons: vec!["negative-control exclusion review missing".into()],
            });
        };
        if latest_id != review_id || verdict != "pass" {
            return Ok(NegativeControlReadiness::Unknown {
                reasons: vec!["negative-control exclusion review is stale or did not pass".into()],
            });
        }
        let protocol: Option<(String, String, String)> = conn.query_row(
            "SELECT a.kind,a.content,a.content_sha256 FROM causal_artifacts a
             WHERE a.id=?1 AND a.tenant_id=?2 AND a.acl=?3
               AND a.invalidated_at IS NULL AND a.retention_at>?4
               AND NOT EXISTS (SELECT 1 FROM causal_ccr_revoking r
                WHERE r.tenant_id=a.tenant_id AND r.acl=a.acl
                 AND r.artifact_id=a.id AND r.version=a.version)
               AND NOT EXISTS (SELECT 1 FROM causal_ccr_revocation_outbox o
                WHERE o.tenant_id=a.tenant_id AND o.connector='causal'
                 AND o.artifact_id=a.id AND o.version=a.version
                 AND o.delivered_at IS NULL)",
            params![protocol_id, scope.tenant_id, scope.acl, now()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).optional()?;
        let Some((kind, content, current_sha256)) = protocol else {
            return Ok(NegativeControlReadiness::Unknown {
                reasons: vec!["negative-control protocol unavailable".into()],
            });
        };
        if kind != "negative_control_protocol"
            || current_sha256 != recorded_sha256
            || format!("{:x}", Sha256::digest(content.as_bytes())) != current_sha256
        {
            return Ok(NegativeControlReadiness::Unknown {
                reasons: vec!["negative-control protocol version or digest invalid".into()],
            });
        }
        Ok(NegativeControlReadiness::Reviewed { review_id: latest_id })
    }
}
