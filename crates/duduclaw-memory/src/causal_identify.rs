//! Conservative graphical backdoor check for a reviewed single-treatment DAG.
//!
//! A passing result is conditional on the curated graph being complete and
//! correct. It cannot detect an unmeasured confounder omitted from that graph.

use std::collections::{HashMap, HashSet, VecDeque};

use rusqlite::{OptionalExtension, params};

use crate::causal::{CausalStore, CausalStoreError, EvidenceScope};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdjustmentReadiness {
    Unknown {
        reasons: Vec<String>,
    },
    GraphicallyAdmissible {
        adjustment_variable_ids: Vec<String>,
    },
}

/// D-separation in a DAG via ancestral moralization. `edges` already have
/// outgoing treatment edges removed for the backdoor graph.
fn d_separated(
    nodes: &HashSet<String>,
    edges: &[(String, String)],
    treatment: &str,
    outcome: &str,
    adjusted: &HashSet<String>,
) -> bool {
    let mut parents: HashMap<&str, Vec<&str>> = HashMap::new();
    for (cause, effect) in edges {
        parents.entry(effect).or_default().push(cause);
    }
    let mut ancestors: HashSet<&str> = HashSet::new();
    let mut pending: Vec<&str> = vec![treatment, outcome];
    pending.extend(adjusted.iter().map(String::as_str));
    while let Some(node) = pending.pop() {
        if ancestors.insert(node) {
            pending.extend(parents.get(node).into_iter().flatten().copied());
        }
    }
    let mut moral: HashMap<&str, HashSet<&str>> = HashMap::new();
    for node in nodes {
        let node = node.as_str();
        if !ancestors.contains(node) {
            continue;
        }
        let selected: Vec<&str> = parents
            .get(node)
            .into_iter()
            .flatten()
            .filter(|parent| ancestors.contains(**parent))
            .copied()
            .collect();
        for parent in &selected {
            moral.entry(node).or_default().insert(parent);
            moral.entry(parent).or_default().insert(node);
        }
        for (index, left) in selected.iter().enumerate() {
            for right in selected.iter().skip(index + 1) {
                moral.entry(left).or_default().insert(right);
                moral.entry(right).or_default().insert(left);
            }
        }
    }
    let mut visited: HashSet<&str> = HashSet::new();
    let mut queue = VecDeque::from([treatment]);
    while let Some(node) = queue.pop_front() {
        if node == outcome {
            return false;
        }
        if !visited.insert(node) {
            continue;
        }
        for neighbor in moral.get(node).into_iter().flatten() {
            if !adjusted.contains(*neighbor) && !visited.contains(neighbor) {
                queue.push_back(neighbor);
            }
        }
    }
    true
}

pub(crate) fn descendants(edges: &[(String, String)], treatment: &str) -> HashSet<String> {
    let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
    for (cause, effect) in edges {
        children.entry(cause).or_default().push(effect);
    }
    let mut found = HashSet::new();
    let mut pending = vec![treatment];
    while let Some(node) = pending.pop() {
        for child in children.get(node).into_iter().flatten() {
            if found.insert((*child).to_owned()) {
                pending.push(child);
            }
        }
    }
    found
}

impl CausalStore {
    /// Suggest the observed direct causes of treatment as a conservative
    /// backdoor set, then verify it through the same reviewed-graph check.
    /// This does not establish that the graph includes every confounder or
    /// that these variables are available before treatment in a dataset.
    pub fn suggest_parent_backdoor_adjustment(
        &self,
        scope: &EvidenceScope,
        model_id: &str,
    ) -> Result<AdjustmentReadiness, CausalStoreError> {
        if !scope.valid() || model_id.trim().is_empty() {
            return Err(CausalStoreError::InvalidInput);
        }
        let state = self.model_state(scope, model_id)?;
        if state != "approved" {
            return Ok(AdjustmentReadiness::Unknown {
                reasons: vec![format!("model_state:{state}")],
            });
        }
        let conn = self.open()?;
        let treatment: Option<(String, String)> = conn
            .query_row(
                "SELECT m.treatment_variable_id,v.name FROM causal_models m
                 JOIN causal_variables v ON v.id=m.treatment_variable_id
                 WHERE m.id=?1 AND m.tenant_id=?2 AND m.acl=?3
                   AND v.tenant_id=?2 AND v.acl=?3",
                params![model_id, scope.tenant_id, scope.acl],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (_, treatment_name) = treatment.ok_or(CausalStoreError::NotFound)?;
        let mut parent_ids: Vec<String> = conn
            .prepare(
                "SELECT DISTINCT v.id FROM causal_model_edges me
                 JOIN causal_claims c ON c.id=me.claim_id
                 JOIN causal_model_variables mv ON mv.model_id=me.model_id
                 JOIN causal_variables v ON v.id=mv.variable_id AND v.name=c.cause_variable
                 WHERE me.model_id=?1 AND c.effect_variable=?2
                   AND c.tenant_id=?3 AND c.acl=?4
                   AND v.tenant_id=?3 AND v.acl=?4",
            )?
            .query_map(
                params![model_id, treatment_name, scope.tenant_id, scope.acl],
                |row| row.get(0),
            )?
            .collect::<Result<_, _>>()?;
        parent_ids.sort();
        self.check_backdoor_adjustment(scope, model_id, &parent_ids)
    }

    /// Check the classical backdoor criterion in the exact reviewed graph:
    /// adjustment nodes cannot descend from treatment, and must d-separate
    /// treatment from outcome after treatment's outgoing edges are removed.
    pub fn check_backdoor_adjustment(
        &self,
        scope: &EvidenceScope,
        model_id: &str,
        adjustment_variable_ids: &[String],
    ) -> Result<AdjustmentReadiness, CausalStoreError> {
        if !scope.valid()
            || model_id.trim().is_empty()
            || adjustment_variable_ids.len() > 128
            || adjustment_variable_ids
                .iter()
                .any(|id| id.trim().is_empty())
            || adjustment_variable_ids.iter().collect::<HashSet<_>>().len()
                != adjustment_variable_ids.len()
        {
            return Err(CausalStoreError::InvalidInput);
        }
        let state = self.model_state(scope, model_id)?;
        if state != "approved" {
            return Ok(AdjustmentReadiness::Unknown {
                reasons: vec![format!("model_state:{state}")],
            });
        }
        let conn = self.open()?;
        let model: Option<(String, String)> = conn
            .query_row(
                "SELECT treatment_variable_id,outcome_variable_id FROM causal_models
                 WHERE id=?1 AND tenant_id=?2 AND acl=?3",
                params![model_id, scope.tenant_id, scope.acl],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (treatment_id, outcome_id) = model.ok_or(CausalStoreError::NotFound)?;
        let variables: HashMap<String, String> = conn
            .prepare(
                "SELECT v.id,v.name FROM causal_model_variables mv
                 JOIN causal_variables v ON v.id=mv.variable_id
                 WHERE mv.model_id=?1 AND v.tenant_id=?2 AND v.acl=?3",
            )?
            .query_map(params![model_id, scope.tenant_id, scope.acl], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<Result<_, _>>()?;
        let treatment = variables
            .get(&treatment_id)
            .ok_or(CausalStoreError::InvalidInput)?;
        let outcome = variables
            .get(&outcome_id)
            .ok_or(CausalStoreError::InvalidInput)?;
        let names: HashSet<String> = variables.values().cloned().collect();
        let mut adjusted = HashSet::new();
        for id in adjustment_variable_ids {
            if id == &treatment_id || id == &outcome_id {
                return Ok(AdjustmentReadiness::Unknown {
                    reasons: vec!["adjustment contains treatment or outcome".into()],
                });
            }
            let Some(name) = variables.get(id) else {
                return Ok(AdjustmentReadiness::Unknown {
                    reasons: vec!["adjustment variable is absent from scoped model".into()],
                });
            };
            adjusted.insert(name.clone());
        }
        let edges: Vec<(String, String)> = conn
            .prepare(
                "SELECT c.cause_variable,c.effect_variable FROM causal_model_edges me
                 JOIN causal_claims c ON c.id=me.claim_id
                 WHERE me.model_id=?1 AND c.tenant_id=?2 AND c.acl=?3",
            )?
            .query_map(params![model_id, scope.tenant_id, scope.acl], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<Result<_, _>>()?;
        let treatment_descendants = descendants(&edges, treatment);
        if adjusted
            .iter()
            .any(|name| treatment_descendants.contains(name))
        {
            return Ok(AdjustmentReadiness::Unknown {
                reasons: vec!["adjustment includes a treatment descendant".into()],
            });
        }
        let backdoor_edges: Vec<(String, String)> = edges
            .into_iter()
            .filter(|(cause, _)| cause != treatment)
            .collect();
        if !d_separated(&names, &backdoor_edges, treatment, outcome, &adjusted) {
            return Ok(AdjustmentReadiness::Unknown {
                reasons: vec!["adjustment does not block every backdoor path".into()],
            });
        }
        let mut sorted = adjustment_variable_ids.to_vec();
        sorted.sort();
        Ok(AdjustmentReadiness::GraphicallyAdmissible {
            adjustment_variable_ids: sorted,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::causal::{ClaimModality, EvidenceStance};
    use crate::causal_model::{ModelDraft, VariableKind};

    fn nodes(names: &[&str]) -> HashSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }
    fn edges(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(cause, effect)| ((*cause).into(), (*effect).into()))
            .collect()
    }

    #[test]
    fn moralized_backdoor_blocks_fork_and_detects_opened_collider() {
        let fork = edges(&[("W", "A"), ("W", "Y")]);
        assert!(!d_separated(
            &nodes(&["W", "A", "Y"]),
            &fork,
            "A",
            "Y",
            &HashSet::new()
        ));
        assert!(d_separated(
            &nodes(&["W", "A", "Y"]),
            &fork,
            "A",
            "Y",
            &HashSet::from(["W".to_owned()])
        ));
        let collider = edges(&[("A", "C"), ("Y", "C")]);
        assert!(d_separated(
            &nodes(&["A", "Y", "C"]),
            &collider,
            "A",
            "Y",
            &HashSet::new()
        ));
        assert!(!d_separated(
            &nodes(&["A", "Y", "C"]),
            &collider,
            "A",
            "Y",
            &HashSet::from(["C".to_owned()])
        ));
    }

    #[test]
    fn descendant_walk_marks_mediators() {
        let graph = edges(&[("A", "M"), ("M", "Y"), ("W", "A")]);
        let found = descendants(&graph, "A");
        assert!(found.contains("M"));
        assert!(found.contains("Y"));
        assert!(!found.contains("W"));
    }

    #[test]
    fn reviewed_graph_requires_confounder_and_rejects_mediator_adjustment() {
        let dir = tempfile::tempdir().unwrap();
        let store = CausalStore::new(dir.path().join("causal.db"));
        let scope = EvidenceScope {
            tenant_id: "tenant-a".into(),
            acl: "private".into(),
        };
        let mut ids = HashMap::new();
        for name in ["W", "A", "M", "Y"] {
            let variable = store
                .register_variable(
                    &scope,
                    name,
                    "v1",
                    "test variable",
                    "units",
                    VariableKind::Continuous,
                )
                .unwrap();
            ids.insert(name, variable.id);
        }
        let source = store
            .add_artifact(
                &scope,
                "review_note",
                "graph",
                "v1",
                "graph-lineage",
                "W A M Y",
                1,
                i64::MAX,
            )
            .unwrap();
        let mut claim_ids = Vec::new();
        for (cause, effect) in [("W", "A"), ("W", "Y"), ("A", "M"), ("M", "Y")] {
            let claim = store
                .add_claim(
                    &scope,
                    cause,
                    effect,
                    0,
                    10,
                    &serde_json::json!({}),
                    ClaimModality::Asserted,
                )
                .unwrap();
            let start = "W A M Y".find(cause).unwrap();
            store
                .add_evidence(
                    &scope,
                    &claim.id,
                    &source.id,
                    start,
                    start + cause.len(),
                    cause,
                    EvidenceStance::Supports,
                    None,
                    "test",
                )
                .unwrap();
            store
                .review_claim(&scope, &claim.id, "reviewer", true)
                .unwrap();
            claim_ids.push(claim.id);
        }
        let draft = ModelDraft {
            name: "test-DAG".into(),
            version: "v1".into(),
            treatment_variable_id: ids["A"].clone(),
            outcome_variable_id: ids["Y"].clone(),
            population: "support tickets".into(),
            window_start: 1,
            window_end: 100,
            variable_ids: ["W", "A", "M", "Y"]
                .iter()
                .map(|name| ids[name].clone())
                .collect(),
            claim_ids,
        };
        let model = store.create_model(&scope, &draft).unwrap();
        assert!(matches!(
            store
                .suggest_parent_backdoor_adjustment(&scope, &model.id)
                .unwrap(),
            AdjustmentReadiness::Unknown { .. }
        ));
        store
            .review_model(&scope, &model.id, "reviewer", true, false)
            .unwrap();
        assert_eq!(
            store
                .suggest_parent_backdoor_adjustment(&scope, &model.id)
                .unwrap(),
            AdjustmentReadiness::GraphicallyAdmissible {
                adjustment_variable_ids: vec![ids["W"].clone()]
            }
        );
        assert!(matches!(
            store
                .check_backdoor_adjustment(&scope, &model.id, &[])
                .unwrap(),
            AdjustmentReadiness::Unknown { .. }
        ));
        assert_eq!(
            store
                .check_backdoor_adjustment(&scope, &model.id, &[ids["W"].clone()])
                .unwrap(),
            AdjustmentReadiness::GraphicallyAdmissible {
                adjustment_variable_ids: vec![ids["W"].clone()]
            }
        );
        assert!(matches!(
            store
                .check_backdoor_adjustment(&scope, &model.id, &[ids["M"].clone()])
                .unwrap(),
            AdjustmentReadiness::Unknown { .. }
        ));
        assert!(matches!(
            store
                .check_backdoor_adjustment(&scope, &model.id, &[ids["W"].clone(), ids["M"].clone()])
                .unwrap(),
            AdjustmentReadiness::Unknown { .. }
        ));
    }
}
