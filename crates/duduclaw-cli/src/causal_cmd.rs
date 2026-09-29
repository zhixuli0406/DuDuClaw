//! A reproducible, explicitly scoped fixture for the causal curation surface.
//! It demonstrates source grounding and human review without claiming an
//! observed intervention effect.

use std::path::Path;

use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_memory::causal::{
    CausalStore, ClaimModality, EvidenceScope, EvidenceStance, ProposedCausalClaim,
};
use duduclaw_memory::causal_effect_eval::{EffectEvalDataset, evaluate_effects};
use duduclaw_memory::causal_eval::{
    ExtractionCompareDataset, ExtractionEvalDataset, compare_extraction, evaluate_extraction,
};
use duduclaw_memory::causal_model::{ModelDraft, VariableKind};
use serde::Serialize;

const STAFF_RULE: &str = "Synthetic simulator rule: each added staffed support agent can resolve up to four extra tickets per day when tickets are available, reducing next-day backlog.";
const ARRIVAL_RULE: &str = "Synthetic simulator rule: more arriving support tickets increase next-day backlog when service capacity is fixed.";

#[derive(Serialize)]
struct DemoReport {
    scope: EvidenceScope,
    source_ids: Vec<String>,
    candidate_claim_ids: Vec<String>,
    draft_model_id: String,
    limitations: Vec<&'static str>,
}

fn causal_error(error: impl std::fmt::Display) -> DuDuClawError {
    DuDuClawError::Memory(format!("causal demo: {error}"))
}

pub fn evaluate(dataset_path: &Path) -> Result<()> {
    let bytes = std::fs::read(dataset_path).map_err(causal_error)?;
    let dataset: ExtractionEvalDataset = serde_json::from_slice(&bytes).map_err(causal_error)?;
    let report = evaluate_extraction(&dataset).map_err(causal_error)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(causal_error)?
    );
    Ok(())
}

pub fn compare(dataset_path: &Path) -> Result<()> {
    let bytes = std::fs::read(dataset_path).map_err(causal_error)?;
    let dataset: ExtractionCompareDataset = serde_json::from_slice(&bytes).map_err(causal_error)?;
    let report = compare_extraction(&dataset).map_err(causal_error)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(causal_error)?
    );
    Ok(())
}

pub fn evaluate_effect(db: &Path, tenant: &str, acl: &str, dataset_path: &Path) -> Result<()> {
    let bytes = std::fs::read(dataset_path).map_err(causal_error)?;
    let dataset: EffectEvalDataset = serde_json::from_slice(&bytes).map_err(causal_error)?;
    let scope = EvidenceScope {
        tenant_id: tenant.into(),
        acl: acl.into(),
    };
    let report = evaluate_effects(&CausalStore::new(db), &scope, &dataset).map_err(causal_error)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(causal_error)?
    );
    Ok(())
}

/// Clear a revocation fence that a failed revoke left behind.
///
/// Starting a revocation hides the source from readers immediately, and that
/// is deliberate. This does not restore a source whose revocation took
/// effect: the store refuses unless the artifact is still intact, no CCR
/// tombstone notice is queued, and no delivery lease for that version
/// remains. Use it when an abandoned revoke is still hiding a live source.
pub fn clear_revocation_fence(db: &Path, tenant: &str, acl: &str, artifact: &str) -> Result<()> {
    let scope = EvidenceScope {
        tenant_id: tenant.into(),
        acl: acl.into(),
    };
    let version = CausalStore::new(db)
        .clear_revocation_fence(&scope, artifact)
        .map_err(causal_error)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "artifact_id": artifact,
            "cleared_version": version,
        }))
        .map_err(causal_error)?
    );
    Ok(())
}

fn build(db: &Path, tenant: &str, acl: &str) -> Result<DemoReport> {
    if tenant.trim().is_empty() || acl.trim().is_empty() {
        return Err(DuDuClawError::Config(
            "causal demo requires tenant and ACL".into(),
        ));
    }
    let scope = EvidenceScope {
        tenant_id: tenant.into(),
        acl: acl.into(),
    };
    let store = CausalStore::new(db);
    let specs = [
        (
            "staff-rule",
            "staff-lineage",
            STAFF_RULE,
            "staffed_agents",
            "next_day_backlog",
        ),
        (
            "arrival-rule",
            "arrival-lineage",
            ARRIVAL_RULE,
            "daily_arrivals",
            "next_day_backlog",
        ),
    ];
    let mut source_ids = Vec::new();
    let mut claim_ids = Vec::new();
    for (external_id, lineage, content, cause, effect) in specs {
        let source = store
            .add_artifact(
                &scope,
                "synthetic_rule",
                external_id,
                "v1",
                lineage,
                content,
                1,
                i64::MAX,
            )
            .map_err(causal_error)?;
        let (claim, _) = store
            .ingest_extracted_claim(
                &scope,
                &source.id,
                "What rules define the synthetic support queue?",
                "causal-demo-v1",
                &ProposedCausalClaim {
                    cause_variable: cause.into(),
                    effect_variable: effect.into(),
                    lag_min_seconds: 0,
                    lag_max_seconds: 86_400,
                    modality: ClaimModality::Asserted,
                    stance: EvidenceStance::Supports,
                    span_start: 0,
                    span_end: content.len(),
                    excerpt: content.into(),
                    speaker_id: Some("synthetic-simulator".into()),
                    context: serde_json::json!({ "synthetic": true, "kind": "simulator_rule" }),
                },
            )
            .map_err(causal_error)?;
        source_ids.push(source.id);
        claim_ids.push(claim.id);
    }

    let staff = store
        .register_variable(
            &scope,
            "staffed_agents",
            "v1",
            "Staff agents scheduled during the simulated day",
            "agents",
            VariableKind::Count,
        )
        .map_err(causal_error)?;
    let arrivals = store
        .register_variable(
            &scope,
            "daily_arrivals",
            "v1",
            "Tickets arriving during the simulated day",
            "tickets/day",
            VariableKind::Count,
        )
        .map_err(causal_error)?;
    let backlog = store
        .register_variable(
            &scope,
            "next_day_backlog",
            "v1",
            "Tickets waiting at the end of the next simulated day",
            "tickets",
            VariableKind::Count,
        )
        .map_err(causal_error)?;
    let existing = store
        .list_model_ids(&scope, 100)
        .map_err(causal_error)?
        .into_iter()
        .map(|id| store.read_model(&scope, &id).map_err(causal_error))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .find(|model| model.name == "synthetic-support-rules" && model.version == "v1");
    let model_id = if let Some(model) = existing {
        model.id
    } else {
        store
            .create_model(
                &scope,
                &ModelDraft {
                    name: "synthetic-support-rules".into(),
                    version: "v1".into(),
                    treatment_variable_id: staff.id.clone(),
                    outcome_variable_id: backlog.id.clone(),
                    population: "synthetic support tickets".into(),
                    window_start: 0,
                    window_end: 86_400 * 35,
                    variable_ids: vec![staff.id, arrivals.id, backlog.id],
                    claim_ids: claim_ids.clone(),
                },
            )
            .map_err(causal_error)?
            .id
    };
    Ok(DemoReport {
        scope,
        source_ids,
        candidate_claim_ids: claim_ids,
        draft_model_id: model_id,
        limitations: vec![
            "All source text is synthetic simulator documentation, not observed support data.",
            "Claims and the graph are candidates until human review; no effect is estimated.",
        ],
    })
}

pub fn demo(db: &Path, tenant: &str, acl: &str) -> Result<()> {
    let report = build(db, tenant, acl)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::run_on_big_stack;
    use clap::Parser;

    /// W3-2 regression: an abandoned source revoke used to hide a live
    /// source permanently — the fence had no DELETE path anywhere, let alone
    /// an operator-reachable one.
    #[test]
    fn clear_revocation_fence_cli_restores_an_abandoned_revoke() {
        run_on_big_stack(clear_revocation_fence_cli_restores_an_abandoned_revoke_body);
    }

    fn clear_revocation_fence_cli_restores_an_abandoned_revoke_body() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("memory.db");
        let store = CausalStore::new(&db);
        let scope = EvidenceScope {
            tenant_id: "synthetic".into(),
            acl: "private".into(),
        };
        let source = store
            .add_artifact(
                &scope,
                "synthetic_rule",
                "fence-cli",
                "v1",
                "lineage",
                STAFF_RULE,
                1,
                i64::MAX,
            )
            .unwrap();
        store.begin_ccr_revocation(&scope, &source.id).unwrap();
        assert!(store.source_text(&scope, &source.id).is_err());

        let parsed = crate::Cli::try_parse_from([
            "duduclaw",
            "causal-clear-revocation-fence",
            "--db",
            db.to_str().unwrap(),
            "--tenant",
            "synthetic",
            "--acl",
            "private",
            "--artifact",
            &source.id,
        ])
        .unwrap();
        let crate::Commands::Causal(crate::CausalCommands::CausalClearRevocationFence {
            db,
            tenant,
            acl,
            artifact,
        }) = parsed.command
        else {
            panic!("causal fence command did not parse")
        };
        clear_revocation_fence(&db, &tenant, &acl, &artifact).unwrap();
        assert_eq!(store.source_text(&scope, &source.id).unwrap(), STAFF_RULE);

        // A revocation that took effect stays irreversible.
        store.invalidate_artifact(&scope, &source.id).unwrap();
        assert!(clear_revocation_fence(&db, &tenant, &acl, &artifact).is_err());
    }

    #[test]
    fn synthetic_demo_is_replayable_without_approval_or_effect() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("memory.db");
        let first = build(&db, "synthetic", "private").unwrap();
        let second = build(&db, "synthetic", "private").unwrap();
        assert_eq!(first.source_ids, second.source_ids);
        assert_eq!(first.candidate_claim_ids, second.candidate_claim_ids);
        assert_eq!(first.draft_model_id, second.draft_model_id);
        let store = CausalStore::new(&db);
        for id in &first.candidate_claim_ids {
            assert_eq!(store.claim_state(&first.scope, id).unwrap(), "candidate");
            assert_eq!(store.evidence_for_claim(&first.scope, id).unwrap().len(), 1);
        }
        assert_eq!(
            store
                .model_state(&first.scope, &first.draft_model_id)
                .unwrap(),
            "draft"
        );
    }
}
