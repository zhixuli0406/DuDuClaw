//! Versioned causal model curation, separate from extracted claims and effects.

use std::collections::{HashMap, HashSet, VecDeque};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::causal::{CausalClaim, CausalStore, CausalStoreError, EvidenceScope, now};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VariableKind {
    Binary,
    Count,
    Continuous,
    Categorical,
}

impl VariableKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Binary => "binary",
            Self::Count => "count",
            Self::Continuous => "continuous",
            Self::Categorical => "categorical",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CausalVariable {
    pub id: String,
    pub scope: EvidenceScope,
    pub name: String,
    pub version: String,
    pub definition: String,
    pub unit: String,
    pub kind: VariableKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelDraft {
    pub name: String,
    pub version: String,
    pub treatment_variable_id: String,
    pub outcome_variable_id: String,
    pub population: String,
    pub window_start: i64,
    pub window_end: i64,
    pub variable_ids: Vec<String>,
    pub claim_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CausalModel {
    pub id: String,
    pub scope: EvidenceScope,
    pub name: String,
    pub version: String,
    pub treatment_variable_id: String,
    pub outcome_variable_id: String,
    pub population: String,
    pub window_start: i64,
    pub window_end: i64,
    pub review_state: String,
    pub reviewer: Option<String>,
    pub conflicts_acknowledged: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelReviewView {
    pub model: CausalModel,
    pub effective_state: String,
    pub variables: Vec<CausalVariable>,
    pub edges: Vec<CausalClaim>,
    pub active_opposition_count: usize,
    pub active_opposition_digest: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssumptionKind {
    DataAvailability,
    TemporalOrder,
    Positivity,
    Exchangeability,
    Consistency,
    Identifiability,
}

impl AssumptionKind {
    pub const ALL: [Self; 6] = [
        Self::DataAvailability,
        Self::TemporalOrder,
        Self::Positivity,
        Self::Exchangeability,
        Self::Consistency,
        Self::Identifiability,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::DataAvailability => "data_availability",
            Self::TemporalOrder => "temporal_order",
            Self::Positivity => "positivity",
            Self::Exchangeability => "exchangeability",
            Self::Consistency => "consistency",
            Self::Identifiability => "identifiability",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssumptionVerdict {
    Pass,
    Fail,
    Unknown,
}

impl AssumptionVerdict {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectReadiness {
    Unknown {
        reasons: Vec<String>,
    },
    /// Review preflight is complete; an estimator still must validate data
    /// and produce a result. This is not an identified effect by itself.
    ReadyForEstimator,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AssumptionReviewRecord {
    pub kind: String,
    pub verdict: String,
    pub rationale: String,
    pub reviewer: String,
    pub reviewed_at: i64,
    pub review_id: String,
}

fn acyclic(edges: &[(String, String)], variable_names: &HashSet<String>) -> bool {
    let mut indegree: HashMap<&str, usize> = variable_names
        .iter()
        .map(|name| (name.as_str(), 0))
        .collect();
    let mut next: HashMap<&str, Vec<&str>> = HashMap::new();
    for (cause, effect) in edges {
        *indegree.entry(effect).or_default() += 1;
        next.entry(cause).or_default().push(effect);
    }
    let mut queue: VecDeque<&str> = indegree
        .iter()
        .filter_map(|(name, count)| (*count == 0).then_some(*name))
        .collect();
    let mut visited = 0;
    while let Some(name) = queue.pop_front() {
        visited += 1;
        if let Some(children) = next.get(name) {
            for child in children {
                let count = indegree
                    .get_mut(child)
                    .expect("model variable was validated");
                *count -= 1;
                if *count == 0 {
                    queue.push_back(child);
                }
            }
        }
    }
    visited == variable_names.len()
}

fn opposition_fingerprint(conn: &Connection, model_id: &str) -> rusqlite::Result<(usize, String)> {
    let mut stmt = conn.prepare(
        "SELECT e.id FROM causal_model_edges me JOIN causal_evidence e ON e.claim_id=me.claim_id
         JOIN causal_artifacts a ON a.id=e.artifact_id
         WHERE me.model_id=?1 AND e.stance='opposes' AND a.invalidated_at IS NULL
         AND a.retention_at>?2
         AND NOT EXISTS (SELECT 1 FROM causal_ccr_revoking r
          WHERE r.tenant_id=a.tenant_id AND r.acl=a.acl
           AND r.artifact_id=a.id AND r.version=a.version)
         AND NOT EXISTS (SELECT 1 FROM causal_ccr_revocation_outbox o
          WHERE o.tenant_id=a.tenant_id AND o.connector='causal'
           AND o.artifact_id=a.id AND o.version=a.version
           AND o.delivered_at IS NULL)
         ORDER BY e.id",
    )?;
    let ids = stmt
        .query_map(params![model_id, now()], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut hasher = Sha256::new();
    for id in &ids {
        hasher.update(id.as_bytes());
        hasher.update([0]);
    }
    Ok((ids.len(), format!("{:x}", hasher.finalize())))
}

impl CausalStore {
    /// Reusing a variable name/version with changed semantics is rejected.
    pub fn register_variable(
        &self,
        scope: &EvidenceScope,
        name: &str,
        version: &str,
        definition: &str,
        unit: &str,
        kind: VariableKind,
    ) -> Result<CausalVariable, CausalStoreError> {
        if !scope.valid()
            || [name, version, definition, unit]
                .iter()
                .any(|value| value.trim().is_empty())
        {
            return Err(CausalStoreError::InvalidInput);
        }
        let mut conn = self.open()?;
        let tx = conn.transaction()?;
        let reserved_alias: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM causal_variable_aliases
             WHERE tenant_id=?1 AND acl=?2 AND alias=?3)",
            params![scope.tenant_id, scope.acl, name],
            |row| row.get(0),
        )?;
        if reserved_alias {
            return Err(CausalStoreError::InvalidInput);
        }
        let existing: Option<(String, String, String, String)> = tx
            .query_row(
                "SELECT id,definition,unit,value_kind FROM causal_variables
             WHERE tenant_id=?1 AND acl=?2 AND name=?3 AND version=?4",
                params![scope.tenant_id, scope.acl, name, version],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let id = if let Some((id, old_definition, old_unit, old_kind)) = existing {
            if old_definition != definition || old_unit != unit || old_kind != kind.as_str() {
                return Err(CausalStoreError::InvalidInput);
            }
            id
        } else {
            let id = Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO causal_variables
                (id,tenant_id,acl,name,version,definition,unit,value_kind,created_at)
                VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![
                    id,
                    scope.tenant_id,
                    scope.acl,
                    name,
                    version,
                    definition,
                    unit,
                    kind.as_str(),
                    now()
                ],
            )?;
            id
        };
        tx.commit()?;
        Ok(CausalVariable {
            id,
            scope: scope.clone(),
            name: name.into(),
            version: version.into(),
            definition: definition.into(),
            unit: unit.into(),
            kind,
        })
    }

    /// Store a DAG draft using exact versions of every variable and exact
    /// claim IDs. Candidates may appear in drafts but cannot approve a model.
    pub fn create_model(
        &self,
        scope: &EvidenceScope,
        draft: &ModelDraft,
    ) -> Result<CausalModel, CausalStoreError> {
        if !scope.valid()
            || [
                draft.name.as_str(),
                draft.version.as_str(),
                draft.population.as_str(),
                draft.treatment_variable_id.as_str(),
                draft.outcome_variable_id.as_str(),
            ]
            .iter()
            .any(|value| value.trim().is_empty())
            || draft.window_start >= draft.window_end
            || draft.treatment_variable_id == draft.outcome_variable_id
            || draft.variable_ids.len() < 2
            || draft.variable_ids.len() > 128
            || draft.claim_ids.is_empty()
            || draft.claim_ids.len() > 256
            || draft.variable_ids.iter().collect::<HashSet<_>>().len() != draft.variable_ids.len()
            || draft.claim_ids.iter().collect::<HashSet<_>>().len() != draft.claim_ids.len()
            || !draft.variable_ids.contains(&draft.treatment_variable_id)
            || !draft.variable_ids.contains(&draft.outcome_variable_id)
        {
            return Err(CausalStoreError::InvalidInput);
        }
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut variable_names = HashSet::new();
        for id in &draft.variable_ids {
            let name: Option<String> = tx
                .query_row(
                    "SELECT name FROM causal_variables WHERE id=?1 AND tenant_id=?2 AND acl=?3",
                    params![id, scope.tenant_id, scope.acl],
                    |row| row.get(0),
                )
                .optional()?;
            if !variable_names.insert(name.ok_or(CausalStoreError::NotFound)?) {
                return Err(CausalStoreError::InvalidInput);
            }
        }
        let mut edges = Vec::with_capacity(draft.claim_ids.len());
        for id in &draft.claim_ids {
            let edge: Option<(String, String, String)> = tx
                .query_row(
                    "SELECT cause_variable,effect_variable,modality FROM causal_claims
                 WHERE id=?1 AND tenant_id=?2 AND acl=?3",
                    params![id, scope.tenant_id, scope.acl],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            let (cause, effect, modality) = edge.ok_or(CausalStoreError::NotFound)?;
            if modality != "asserted"
                || !variable_names.contains(&cause)
                || !variable_names.contains(&effect)
            {
                return Err(CausalStoreError::InvalidInput);
            }
            edges.push((cause, effect));
        }
        if !acyclic(&edges, &variable_names) {
            return Err(CausalStoreError::InvalidInput);
        }
        let id = Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO causal_models
            (id,tenant_id,acl,name,version,treatment_variable_id,outcome_variable_id,
             population,window_start,window_end,created_at)
            VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                id,
                scope.tenant_id,
                scope.acl,
                draft.name,
                draft.version,
                draft.treatment_variable_id,
                draft.outcome_variable_id,
                draft.population,
                draft.window_start,
                draft.window_end,
                now()
            ],
        )?;
        for variable_id in &draft.variable_ids {
            tx.execute(
                "INSERT INTO causal_model_variables(model_id,variable_id) VALUES (?1,?2)",
                params![id, variable_id],
            )?;
        }
        for claim_id in &draft.claim_ids {
            tx.execute(
                "INSERT INTO causal_model_edges(model_id,claim_id) VALUES (?1,?2)",
                params![id, claim_id],
            )?;
        }
        tx.commit()?;
        Ok(CausalModel {
            id,
            scope: scope.clone(),
            name: draft.name.clone(),
            version: draft.version.clone(),
            treatment_variable_id: draft.treatment_variable_id.clone(),
            outcome_variable_id: draft.outcome_variable_id.clone(),
            population: draft.population.clone(),
            window_start: draft.window_start,
            window_end: draft.window_end,
            review_state: "draft".into(),
            reviewer: None,
            conflicts_acknowledged: false,
        })
    }

    pub fn list_model_ids(
        &self,
        scope: &EvidenceScope,
        limit: usize,
    ) -> Result<Vec<String>, CausalStoreError> {
        if !scope.valid() {
            return Err(CausalStoreError::InvalidInput);
        }
        let conn = self.open()?;
        let mut stmt = conn.prepare(
            "SELECT id FROM causal_models WHERE tenant_id=?1 AND acl=?2
             ORDER BY created_at DESC, rowid DESC LIMIT ?3",
        )?;
        let ids = stmt.query_map(
            params![scope.tenant_id, scope.acl, limit.clamp(1, 100) as i64],
            |row| row.get(0),
        )?;
        ids.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn read_model(
        &self,
        scope: &EvidenceScope,
        model_id: &str,
    ) -> Result<CausalModel, CausalStoreError> {
        let conn = self.open()?;
        Self::read_model_with_conn(&conn, scope, model_id)
    }

    /// `read_model` on a connection the caller already opened — see
    /// `CausalStore::read_claim_with_conn` for why the listing paths need it.
    pub fn read_model_with_conn(
        conn: &Connection,
        scope: &EvidenceScope,
        model_id: &str,
    ) -> Result<CausalModel, CausalStoreError> {
        if !scope.valid() || model_id.trim().is_empty() {
            return Err(CausalStoreError::InvalidInput);
        }
        let row = conn
            .query_row(
                "SELECT name,version,treatment_variable_id,outcome_variable_id,population,
             window_start,window_end,review_state,reviewer,conflicts_acknowledged
             FROM causal_models WHERE id=?1 AND tenant_id=?2 AND acl=?3",
                params![model_id, scope.tenant_id, scope.acl],
                |row| {
                    Ok(CausalModel {
                        id: model_id.into(),
                        scope: scope.clone(),
                        name: row.get(0)?,
                        version: row.get(1)?,
                        treatment_variable_id: row.get(2)?,
                        outcome_variable_id: row.get(3)?,
                        population: row.get(4)?,
                        window_start: row.get(5)?,
                        window_end: row.get(6)?,
                        review_state: row.get(7)?,
                        reviewer: row.get(8)?,
                        conflicts_acknowledged: row.get(9)?,
                    })
                },
            )
            .optional()?;
        row.ok_or(CausalStoreError::NotFound)
    }

    /// Bounded graph view for human review. The digest binds a review action
    /// to the active opposing evidence the reviewer actually saw.
    pub fn model_review_view(
        &self,
        scope: &EvidenceScope,
        model_id: &str,
    ) -> Result<ModelReviewView, CausalStoreError> {
        // One connection for the whole view: a model may carry up to 256
        // edges, and a `read_claim` per edge used to mean a `CausalStore::open()`
        // — schema setup plus a full live-Wiki resync — per edge.
        let conn = self.open()?;
        let model = Self::read_model_with_conn(&conn, scope, model_id)?;
        let mut variable_stmt = conn.prepare(
            "SELECT v.id,v.name,v.version,v.definition,v.unit,v.value_kind
             FROM causal_model_variables mv JOIN causal_variables v ON v.id=mv.variable_id
             WHERE mv.model_id=?1 AND v.tenant_id=?2 AND v.acl=?3 ORDER BY v.name,v.version",
        )?;
        let variables = variable_stmt
            .query_map(params![model_id, scope.tenant_id, scope.acl], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|(id, name, version, definition, unit, kind)| {
                let kind = match kind.as_str() {
                    "binary" => VariableKind::Binary,
                    "count" => VariableKind::Count,
                    "continuous" => VariableKind::Continuous,
                    "categorical" => VariableKind::Categorical,
                    _ => return Err(CausalStoreError::InvalidInput),
                };
                Ok(CausalVariable {
                    id,
                    scope: scope.clone(),
                    name,
                    version,
                    definition,
                    unit,
                    kind,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut edge_stmt = conn.prepare(
            "SELECT claim_id FROM causal_model_edges WHERE model_id=?1 ORDER BY claim_id",
        )?;
        let edge_ids = edge_stmt
            .query_map([model_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let edges = edge_ids
            .iter()
            .map(|id| CausalStore::read_claim_with_conn(&conn, scope, id))
            .collect::<Result<Vec<_>, _>>()?;
        let (active_opposition_count, active_opposition_digest) =
            opposition_fingerprint(&conn, model_id)?;
        Ok(ModelReviewView {
            model,
            effective_state: Self::model_state_with_conn(&conn, scope, model_id)?,
            variables,
            edges,
            active_opposition_count,
            active_opposition_digest,
        })
    }

    /// Approval is a separate human act. Every edge must still be an accepted,
    /// source-backed asserted claim. Opposing spans require explicit review.
    pub fn review_model(
        &self,
        scope: &EvidenceScope,
        model_id: &str,
        reviewer: &str,
        approve: bool,
        acknowledge_conflicts: bool,
    ) -> Result<(), CausalStoreError> {
        self.review_model_checked(
            scope,
            model_id,
            reviewer,
            approve,
            acknowledge_conflicts,
            None,
            None,
        )
    }

    pub fn review_model_if_state(
        &self,
        scope: &EvidenceScope,
        model_id: &str,
        reviewer: &str,
        approve: bool,
        acknowledge_conflicts: bool,
        expected_state: &str,
        expected_opposition_digest: &str,
    ) -> Result<(), CausalStoreError> {
        if !matches!(
            expected_state,
            "draft" | "approved" | "rejected" | "needs_review"
        ) || expected_opposition_digest.len() != 64
            || !expected_opposition_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(CausalStoreError::InvalidInput);
        }
        self.review_model_checked(
            scope,
            model_id,
            reviewer,
            approve,
            acknowledge_conflicts,
            Some(expected_state),
            Some(expected_opposition_digest),
        )
    }

    fn review_model_checked(
        &self,
        scope: &EvidenceScope,
        model_id: &str,
        reviewer: &str,
        approve: bool,
        acknowledge_conflicts: bool,
        expected_state: Option<&str>,
        expected_opposition_digest: Option<&str>,
    ) -> Result<(), CausalStoreError> {
        if !scope.valid() || reviewer.trim().is_empty() {
            return Err(CausalStoreError::InvalidInput);
        }
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current_state: Option<String> = tx
            .query_row(
                "SELECT review_state FROM causal_models WHERE id=?1 AND tenant_id=?2 AND acl=?3",
                params![model_id, scope.tenant_id, scope.acl],
                |row| row.get(0),
            )
            .optional()?;
        let current_state = current_state.ok_or(CausalStoreError::NotFound)?;
        if expected_state.is_some_and(|expected| expected != current_state) {
            return Err(CausalStoreError::Conflict);
        }
        let (conflicts, fingerprint) = opposition_fingerprint(&tx, model_id)?;
        if expected_opposition_digest.is_some_and(|expected| expected != fingerprint) {
            return Err(CausalStoreError::Conflict);
        }
        let mut opposition_digest = String::new();
        if approve {
            let invalid_edges: i64 = tx.query_row(
                "SELECT COUNT(*) FROM causal_model_edges me JOIN causal_claims c ON c.id=me.claim_id
                 WHERE me.model_id=?1 AND (c.review_state!='accepted' OR c.modality!='asserted'
                 OR NOT EXISTS (SELECT 1 FROM causal_evidence e JOIN causal_artifacts a ON a.id=e.artifact_id
                    WHERE e.claim_id=c.id AND e.stance='supports' AND a.invalidated_at IS NULL
                    AND a.retention_at>?2 AND a.tenant_id=c.tenant_id AND a.acl=c.acl
                    AND NOT EXISTS (SELECT 1 FROM causal_ccr_revoking r
                     WHERE r.tenant_id=a.tenant_id AND r.acl=a.acl
                      AND r.artifact_id=a.id AND r.version=a.version)
                    AND NOT EXISTS (SELECT 1 FROM causal_ccr_revocation_outbox o
                     WHERE o.tenant_id=a.tenant_id AND o.connector='causal'
                      AND o.artifact_id=a.id AND o.version=a.version
                      AND o.delivered_at IS NULL)))",
                params![model_id,now()], |row| row.get(0),
            )?;
            if invalid_edges > 0 {
                return Err(CausalStoreError::MissingSupport);
            }
            if conflicts > 0 && !acknowledge_conflicts {
                return Err(CausalStoreError::InvalidInput);
            }
            opposition_digest = fingerprint;
        }
        let reviewed_at = now();
        tx.execute(
            "UPDATE causal_models SET review_state=?1, reviewer=?2, reviewed_at=?3,
            conflicts_acknowledged=?4, acknowledged_opposition_digest=?5
            WHERE id=?6 AND tenant_id=?7 AND acl=?8",
            params![
                if approve { "approved" } else { "rejected" },
                reviewer,
                reviewed_at,
                acknowledge_conflicts,
                opposition_digest,
                model_id,
                scope.tenant_id,
                scope.acl
            ],
        )?;
        tx.execute(
            "INSERT INTO causal_model_reviews
             (id,model_id,reviewer,decision,acknowledged_opposition_digest,reviewed_at)
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                Uuid::new_v4().to_string(),
                model_id,
                reviewer,
                if approve { "approved" } else { "rejected" },
                opposition_digest,
                reviewed_at
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Recompute viability from linked claims rather than trusting a stale
    /// stored approval after a source expires or is deleted.
    pub fn model_state(
        &self,
        scope: &EvidenceScope,
        model_id: &str,
    ) -> Result<String, CausalStoreError> {
        let conn = self.open()?;
        Self::model_state_with_conn(&conn, scope, model_id)
    }

    /// `model_state` on a connection the caller already opened.
    pub fn model_state_with_conn(
        conn: &Connection,
        scope: &EvidenceScope,
        model_id: &str,
    ) -> Result<String, CausalStoreError> {
        if !scope.valid() {
            return Err(CausalStoreError::InvalidInput);
        }
        let state: Option<(String, bool, String)> = conn
            .query_row(
                "SELECT review_state, conflicts_acknowledged, acknowledged_opposition_digest FROM causal_models
                 WHERE id=?1 AND tenant_id=?2 AND acl=?3",
                params![model_id, scope.tenant_id, scope.acl],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let (state, conflicts_acknowledged, acknowledged_opposition_digest) =
            state.ok_or(CausalStoreError::NotFound)?;
        if state != "approved" {
            return Ok(state);
        }
        let invalid: i64 = conn.query_row(
            "SELECT COUNT(*) FROM causal_model_edges me JOIN causal_claims c ON c.id=me.claim_id
             WHERE me.model_id=?1 AND (c.review_state!='accepted' OR NOT EXISTS (
                SELECT 1 FROM causal_evidence e JOIN causal_artifacts a ON a.id=e.artifact_id
                WHERE e.claim_id=c.id AND e.stance='supports' AND a.invalidated_at IS NULL
                AND a.retention_at>?2 AND a.tenant_id=c.tenant_id AND a.acl=c.acl
                AND NOT EXISTS (SELECT 1 FROM causal_ccr_revoking r
                 WHERE r.tenant_id=a.tenant_id AND r.acl=a.acl
                  AND r.artifact_id=a.id AND r.version=a.version)
                AND NOT EXISTS (SELECT 1 FROM causal_ccr_revocation_outbox o
                 WHERE o.tenant_id=a.tenant_id AND o.connector='causal'
                  AND o.artifact_id=a.id AND o.version=a.version
                  AND o.delivered_at IS NULL)))",
            params![model_id, now()],
            |row| row.get(0),
        )?;
        let (active_conflicts, active_digest) = opposition_fingerprint(&conn, model_id)?;
        let unreviewed_conflicts = active_conflicts > 0
            && (!conflicts_acknowledged || active_digest != acknowledged_opposition_digest);
        Ok(if invalid == 0 && !unreviewed_conflicts {
            "approved"
        } else {
            "needs_review"
        }
        .into())
    }

    /// Assumption reviews are append-only; the current verdict is a scoped
    /// projection that can be revised without rewriting earlier reviews.
    pub fn record_assumption(
        &self,
        scope: &EvidenceScope,
        model_id: &str,
        kind: AssumptionKind,
        verdict: AssumptionVerdict,
        rationale: &str,
        reviewer: &str,
    ) -> Result<String, CausalStoreError> {
        self.record_assumption_checked(
            scope, model_id, kind, verdict, rationale, reviewer, None,
        )
    }

    pub fn record_assumption_if_review(
        &self,
        scope: &EvidenceScope,
        model_id: &str,
        kind: AssumptionKind,
        verdict: AssumptionVerdict,
        rationale: &str,
        reviewer: &str,
        expected_review_id: Option<&str>,
    ) -> Result<String, CausalStoreError> {
        self.record_assumption_checked(
            scope, model_id, kind, verdict, rationale, reviewer, Some(expected_review_id),
        )
    }

    fn record_assumption_checked(
        &self,
        scope: &EvidenceScope,
        model_id: &str,
        kind: AssumptionKind,
        verdict: AssumptionVerdict,
        rationale: &str,
        reviewer: &str,
        expected_review_id: Option<Option<&str>>,
    ) -> Result<String, CausalStoreError> {
        if !scope.valid() || rationale.trim().is_empty() || reviewer.trim().is_empty() {
            return Err(CausalStoreError::InvalidInput);
        }
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let model_exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM causal_models WHERE id=?1 AND tenant_id=?2 AND acl=?3)",
            params![model_id, scope.tenant_id, scope.acl],
            |row| row.get(0),
        )?;
        if !model_exists {
            return Err(CausalStoreError::NotFound);
        }
        if let Some(expected) = expected_review_id {
            let current: Option<String> = tx.query_row(
                "SELECT review_id FROM causal_assumptions WHERE model_id=?1 AND kind=?2",
                params![model_id, kind.as_str()], |row| row.get(0),
            ).optional()?;
            if current.as_deref() != expected {
                return Err(CausalStoreError::Conflict);
            }
        }
        let review_id = Uuid::new_v4().to_string();
        let reviewed_at = now();
        tx.execute(
            "INSERT INTO causal_assumption_reviews
            (id,model_id,kind,verdict,rationale,reviewer,reviewed_at)
            VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                review_id,
                model_id,
                kind.as_str(),
                verdict.as_str(),
                rationale,
                reviewer,
                reviewed_at
            ],
        )?;
        tx.execute(
            "INSERT INTO causal_assumptions
            (model_id,kind,verdict,rationale,reviewer,reviewed_at,review_id)
            VALUES (?1,?2,?3,?4,?5,?6,?7)
            ON CONFLICT(model_id,kind) DO UPDATE SET
            verdict=excluded.verdict,rationale=excluded.rationale,reviewer=excluded.reviewer,
            reviewed_at=excluded.reviewed_at,review_id=excluded.review_id",
            params![
                model_id,
                kind.as_str(),
                verdict.as_str(),
                rationale,
                reviewer,
                reviewed_at,
                review_id
            ],
        )?;
        // A fresh judgment, including pass-to-pass with a revised rationale,
        // changes the evidence underlying any existing model estimate.
        tx.execute(
            "UPDATE causal_effect_estimates SET estimate=NULL,lower_bound=NULL,upper_bound=NULL,
             diagnostics_json='{}',identification_state='assumption_review_changed'
             WHERE model_id=?1",
            [model_id],
        )?;
        tx.commit()?;
        Ok(review_id)
    }

    pub fn current_assumption_reviews(
        &self,
        scope: &EvidenceScope,
        model_id: &str,
    ) -> Result<Vec<AssumptionReviewRecord>, CausalStoreError> {
        if !scope.valid() || model_id.trim().is_empty() {
            return Err(CausalStoreError::InvalidInput);
        }
        let conn = self.open()?;
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM causal_models WHERE id=?1 AND tenant_id=?2 AND acl=?3)",
            params![model_id, scope.tenant_id, scope.acl], |row| row.get(0),
        )?;
        if !exists { return Err(CausalStoreError::NotFound); }
        conn.prepare(
            "SELECT kind,verdict,rationale,reviewer,reviewed_at,review_id
             FROM causal_assumptions WHERE model_id=?1 ORDER BY kind",
        )?.query_map([model_id], |row| Ok(AssumptionReviewRecord {
            kind: row.get(0)?, verdict: row.get(1)?, rationale: row.get(2)?,
            reviewer: row.get(3)?, reviewed_at: row.get(4)?, review_id: row.get(5)?,
        }))?.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Fail closed when any graph review or required assumption is missing,
    /// failed, or unknown. An estimator performs further data checks.
    pub fn effect_readiness(
        &self,
        scope: &EvidenceScope,
        model_id: &str,
    ) -> Result<EffectReadiness, CausalStoreError> {
        let conn = self.open()?;
        Self::effect_readiness_with_conn(&conn, scope, model_id)
    }

    /// `effect_readiness` on a connection the caller already opened.
    pub fn effect_readiness_with_conn(
        conn: &Connection,
        scope: &EvidenceScope,
        model_id: &str,
    ) -> Result<EffectReadiness, CausalStoreError> {
        let state = Self::model_state_with_conn(conn, scope, model_id)?;
        let mut reasons = Vec::new();
        if state != "approved" {
            reasons.push(format!("model_state:{state}"));
        }
        let mut stmt =
            conn.prepare("SELECT kind,verdict FROM causal_assumptions WHERE model_id=?1")?;
        let reviews = stmt
            .query_map([model_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<HashMap<_, _>, _>>()?;
        for kind in AssumptionKind::ALL {
            match reviews.get(kind.as_str()).map(String::as_str) {
                Some("pass") => {}
                Some(other) => reasons.push(format!("{}:{other}", kind.as_str())),
                None => reasons.push(format!("{}:missing", kind.as_str())),
            }
        }
        Ok(if reasons.is_empty() {
            EffectReadiness::ReadyForEstimator
        } else {
            EffectReadiness::Unknown { reasons }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::causal::{ClaimModality, EvidenceStance};

    fn scope() -> EvidenceScope {
        EvidenceScope {
            tenant_id: "a".into(),
            acl: "private".into(),
        }
    }

    #[test]
    fn model_requires_accepted_grounded_claim_and_rechecks_after_revocation() {
        let dir = tempfile::tempdir().unwrap();
        let store = CausalStore::new(dir.path().join("causal.db"));
        let x = store
            .register_variable(
                &scope(),
                "staff",
                "v1",
                "staff on shift",
                "people",
                VariableKind::Count,
            )
            .unwrap();
        let y = store
            .register_variable(
                &scope(),
                "backlog",
                "v1",
                "unresolved tickets",
                "tickets",
                VariableKind::Count,
            )
            .unwrap();
        assert!(matches!(
            store.register_variable(
                &scope(),
                "staff",
                "v1",
                "different",
                "people",
                VariableKind::Count
            ),
            Err(CausalStoreError::InvalidInput)
        ));
        let source = store
            .add_artifact(
                &scope(),
                "ticket",
                "t1",
                "v1",
                "lineage-1",
                "staff reduced backlog",
                1,
                i64::MAX,
            )
            .unwrap();
        let claim = store
            .add_claim(
                &scope(),
                "staff",
                "backlog",
                0,
                86400,
                &serde_json::json!({}),
                ClaimModality::Asserted,
            )
            .unwrap();
        store
            .add_evidence(
                &scope(),
                &claim.id,
                &source.id,
                0,
                5,
                "staff",
                EvidenceStance::Supports,
                None,
                "test",
            )
            .unwrap();
        let draft = ModelDraft {
            name: "support".into(),
            version: "v1".into(),
            treatment_variable_id: x.id.clone(),
            outcome_variable_id: y.id.clone(),
            population: "support tickets".into(),
            window_start: 1,
            window_end: 100,
            variable_ids: vec![x.id.clone(), y.id.clone()],
            claim_ids: vec![claim.id.clone()],
        };
        let model = store.create_model(&scope(), &draft).unwrap();
        assert!(matches!(
            store.review_model(&scope(), &model.id, "reviewer", true, false),
            Err(CausalStoreError::MissingSupport)
        ));
        store
            .review_claim(&scope(), &claim.id, "reviewer", true)
            .unwrap();
        let draft_view = store.model_review_view(&scope(), &model.id).unwrap();
        assert_eq!(draft_view.effective_state, "draft");
        assert_eq!(draft_view.variables.len(), 2);
        assert_eq!(draft_view.edges.len(), 1);
        assert_eq!(
            store.list_model_ids(&scope(), 10).unwrap(),
            vec![model.id.clone()]
        );
        store
            .review_model_if_state(
                &scope(),
                &model.id,
                "reviewer",
                true,
                false,
                "draft",
                &draft_view.active_opposition_digest,
            )
            .unwrap();
        assert!(matches!(
            store.review_model_if_state(
                &scope(),
                &model.id,
                "reviewer",
                false,
                false,
                "draft",
                &draft_view.active_opposition_digest,
            ),
            Err(CausalStoreError::Conflict)
        ));
        assert_eq!(store.model_state(&scope(), &model.id).unwrap(), "approved");
        assert!(matches!(
            store.effect_readiness(&scope(), &model.id).unwrap(),
            EffectReadiness::Unknown { .. }
        ));
        for kind in AssumptionKind::ALL {
            store
                .record_assumption(
                    &scope(),
                    &model.id,
                    kind,
                    AssumptionVerdict::Pass,
                    "Reviewed against the pilot data and design",
                    "reviewer",
                )
                .unwrap();
        }
        assert_eq!(
            store.effect_readiness(&scope(), &model.id).unwrap(),
            EffectReadiness::ReadyForEstimator
        );
        let current_reviews = store.current_assumption_reviews(&scope(), &model.id).unwrap();
        assert_eq!(current_reviews.len(), AssumptionKind::ALL.len());
        let positivity_review = current_reviews.iter()
            .find(|review| review.kind == "positivity").unwrap();
        assert!(matches!(
            store.record_assumption_if_review(
                &scope(), &model.id, AssumptionKind::Positivity,
                AssumptionVerdict::Unknown, "Stale review must be rejected", "reviewer", None,
            ),
            Err(CausalStoreError::Conflict)
        ));
        store
            .record_assumption_if_review(
                &scope(),
                &model.id,
                AssumptionKind::Positivity,
                AssumptionVerdict::Unknown,
                "Sparse treatment strata",
                "reviewer",
                Some(&positivity_review.review_id),
            )
            .unwrap();
        assert!(matches!(
            store.effect_readiness(&scope(), &model.id).unwrap(),
            EffectReadiness::Unknown { .. }
        ));
        let conn = rusqlite::Connection::open(store.path()).unwrap();
        let review_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM causal_assumption_reviews",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(review_count, 7);
        let opposing = store
            .add_artifact(
                &scope(),
                "ticket",
                "t2",
                "v1",
                "lineage-2",
                "staff did not reduce backlog",
                2,
                i64::MAX,
            )
            .unwrap();
        store
            .add_evidence(
                &scope(),
                &claim.id,
                &opposing.id,
                0,
                5,
                "staff",
                EvidenceStance::Opposes,
                None,
                "test",
            )
            .unwrap();
        assert_eq!(
            store.model_state(&scope(), &model.id).unwrap(),
            "needs_review"
        );
        assert!(matches!(
            store.review_model_if_state(
                &scope(),
                &model.id,
                "reviewer",
                true,
                true,
                "approved",
                &draft_view.active_opposition_digest,
            ),
            Err(CausalStoreError::Conflict)
        ));
        assert!(matches!(
            store.review_model(&scope(), &model.id, "reviewer", true, false),
            Err(CausalStoreError::InvalidInput)
        ));
        let first_opposition_view = store.model_review_view(&scope(), &model.id).unwrap();
        assert_eq!(first_opposition_view.active_opposition_count, 1);
        store
            .review_model_if_state(
                &scope(),
                &model.id,
                "reviewer",
                true,
                true,
                "approved",
                &first_opposition_view.active_opposition_digest,
            )
            .unwrap();
        assert_eq!(store.model_state(&scope(), &model.id).unwrap(), "approved");
        let later = store
            .add_artifact(
                &scope(),
                "ticket",
                "t3",
                "v1",
                "lineage-3",
                "staff had mixed results",
                3,
                i64::MAX,
            )
            .unwrap();
        store
            .add_evidence(
                &scope(),
                &claim.id,
                &later.id,
                0,
                5,
                "staff",
                EvidenceStance::Opposes,
                None,
                "test",
            )
            .unwrap();
        assert_eq!(
            store.model_state(&scope(), &model.id).unwrap(),
            "needs_review"
        );
        assert!(matches!(
            store.review_model_if_state(
                &scope(),
                &model.id,
                "reviewer",
                true,
                true,
                "approved",
                &first_opposition_view.active_opposition_digest,
            ),
            Err(CausalStoreError::Conflict)
        ));
        let second_opposition_view = store.model_review_view(&scope(), &model.id).unwrap();
        assert_eq!(second_opposition_view.active_opposition_count, 2);
        store
            .review_model_if_state(
                &scope(),
                &model.id,
                "reviewer",
                true,
                true,
                "approved",
                &second_opposition_view.active_opposition_digest,
            )
            .unwrap();
        store.begin_ccr_revocation(&scope(), &source.id).unwrap();
        assert_eq!(store.model_state(&scope(), &model.id).unwrap(), "needs_review");
        store.invalidate_artifact(&scope(), &source.id).unwrap();
        assert_eq!(
            store.model_state(&scope(), &model.id).unwrap(),
            "needs_review"
        );
        let conn = rusqlite::Connection::open(store.path()).unwrap();
        let model_reviews: i64 = conn
            .query_row("SELECT COUNT(*) FROM causal_model_reviews", [], |row| {
                row.get(0)
            })
            .unwrap();
        let claim_reviews: i64 = conn
            .query_row("SELECT COUNT(*) FROM causal_claim_reviews", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(model_reviews, 3);
        assert_eq!(claim_reviews, 1);
    }

    #[test]
    fn cyclic_claims_cannot_form_a_model() {
        let dir = tempfile::tempdir().unwrap();
        let store = CausalStore::new(dir.path().join("causal.db"));
        let a = store
            .register_variable(&scope(), "a", "v1", "a", "count", VariableKind::Count)
            .unwrap();
        let b = store
            .register_variable(&scope(), "b", "v1", "b", "count", VariableKind::Count)
            .unwrap();
        let ab = store
            .add_claim(
                &scope(),
                "a",
                "b",
                0,
                1,
                &serde_json::json!({}),
                ClaimModality::Asserted,
            )
            .unwrap();
        let ba = store
            .add_claim(
                &scope(),
                "b",
                "a",
                0,
                1,
                &serde_json::json!({}),
                ClaimModality::Asserted,
            )
            .unwrap();
        let draft = ModelDraft {
            name: "cycle".into(),
            version: "v1".into(),
            treatment_variable_id: a.id.clone(),
            outcome_variable_id: b.id.clone(),
            population: "tickets".into(),
            window_start: 1,
            window_end: 2,
            variable_ids: vec![a.id, b.id],
            claim_ids: vec![ab.id, ba.id],
        };
        assert!(matches!(
            store.create_model(&scope(), &draft),
            Err(CausalStoreError::InvalidInput)
        ));
    }
}
