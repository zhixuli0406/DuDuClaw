//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

//! Evolution v3 dashboard convergence RPCs (TODO-evolution-v3-2026-08.md
//! §"dashboard 統一收斂"): `evolution.stagnation` / `.telemetry` and
//! `playbook.list` / `.retire` / `.export`. (`.versions` /
//! `.consolidations` went with the legacy SOUL path in S11.) Mirrors the
//! `MethodHandler::new(root).await` +
//! `handler.handle_xxx(json!({...})).await` harness used throughout this
//! file (see `skills_install_scan_tests` above).
use super::*;
use crate::gvu::telemetry::record_rejection;
use crate::gvu::version_store::ExperimentLogEntry;
use crate::playbook::{EvalCaseRef, PlaybookCategory, PlaybookDelta};

fn payload(frame: &WsFrame) -> Value {
    match frame {
        WsFrame::Response {
            ok: true,
            payload: Some(p),
            ..
        } => p.clone(),
        WsFrame::Response {
            ok: false, error, ..
        } => {
            panic!("RPC returned an error frame: {error:?}")
        }
        other => panic!("unexpected frame shape: {other:?}"),
    }
}

fn error_text(frame: &WsFrame) -> String {
    match frame {
        WsFrame::Response { error: Some(e), .. } => e.to_string(),
        _ => String::new(),
    }
}

// ── evolution.stagnation ─────────────────────────────────

#[tokio::test]
async fn stagnation_with_no_gvu_history_is_never_stagnant() {
    let home = tempfile::tempdir().expect("tempdir");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let frame = handler
        .handle_evolution_stagnation(json!({ "agent_id": "agent-a" }))
        .await;
    let p = payload(&frame);
    let snapshots = p["snapshots"].as_array().expect("snapshots array");
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0]["agent_id"], "agent-a");
    assert_eq!(snapshots[0]["is_stagnant"], false);
    assert!(snapshots[0]["signals"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn stagnation_detects_consecutive_non_applied_signal() {
    let home = tempfile::tempdir().expect("tempdir");
    let db_path = home.path().join("evolution.db");
    let vs = VersionStore::new(&db_path);
    // Real rejected attempts — `skipped` rounds (cooldown / "no change")
    // deliberately no longer count toward any stagnation signal
    // (2026-08-20 self-feeding escalation fix).
    for _ in 0..5 {
        vs.record_experiment(&ExperimentLogEntry::new(
            "agent-a",
            3,
            3,
            std::time::Duration::from_secs(60),
            "abandoned",
            "cap exceeded",
        ));
    }

    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle_evolution_stagnation(json!({ "agent_id": "agent-a" }))
        .await;
    let p = payload(&frame);
    let snap = &p["snapshots"][0];
    assert_eq!(snap["is_stagnant"], true);
    assert!(!snap["signals"].as_array().unwrap().is_empty());
    assert!(snap["summary"].as_str().unwrap().contains("agent-a"));
}

// ── evolution.telemetry ──────────────────────────────────

#[tokio::test]
async fn telemetry_aggregates_rejection_counts_by_stage_and_layer() {
    let home = tempfile::tempdir().expect("tempdir");
    record_rejection(
        home.path(),
        "agent-a",
        "verify",
        "L1-Deterministic",
        "too long",
        "c1",
        1,
    );
    record_rejection(
        home.path(),
        "agent-a",
        "verify",
        "L1-Deterministic",
        "too long again",
        "c2",
        2,
    );
    record_rejection(
        home.path(),
        "agent-a",
        "apply",
        "cap_lines",
        "over cap",
        "c3",
        1,
    );
    // Different agent — must not leak into the "agent-a" summary.
    record_rejection(
        home.path(),
        "agent-b",
        "verify",
        "L1-Deterministic",
        "unrelated",
        "c4",
        1,
    );

    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle_evolution_telemetry(json!({ "agent_id": "agent-a", "days": 7 }))
        .await;
    let p = payload(&frame);
    assert_eq!(p["total"], 3);
    assert_eq!(p["by_stage_layer"]["verify"]["L1-Deterministic"], 2);
    assert_eq!(p["by_stage_layer"]["apply"]["cap_lines"], 1);
}

// ── playbook.list / .retire / .export ────────────────────

/// Fixture eval case tree — mirrors `crate::playbook::store::tests::temp_eval_root`.
fn temp_eval_root() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let suite = dir.path().join("s");
    std::fs::create_dir(&suite).unwrap();
    std::fs::write(
        suite.join("c.toml"),
        "[case]\nname = \"c\"\nagent = \"a\"\nprompt = \"hi\"\n[judge]\nrubric = \"r\"\n",
    )
    .unwrap();
    dir
}

/// Seed one playbook entry directly through `playbook::apply_deltas` — the
/// same write path the AEE loop uses — into the on-disk memory db the RPC
/// layer's `agent_memory_db_path` will resolve to.
async fn seed_playbook_entry(home: &std::path::Path, agent_id: &str) -> String {
    let evals = temp_eval_root();
    let state_dir = home.join("agents").join(agent_id).join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let db_path = state_dir.join("memory.db");
    let engine = SqliteMemoryEngine::new(&db_path).expect("open memory db");

    let add = PlaybookDelta::Add {
        assertions: crate::playbook::entry::EntryAssertions {
            output_contains: vec!["ok".to_string()],
            ..Default::default()
        },
        content: "always confirm the refund amount before issuing it".to_string(),
        category: PlaybookCategory::Repair,
        signals_match: vec!["mistake:capability".to_string()],
        eval_cases: vec![EvalCaseRef("s/c".to_string())],
        strategy: Vec::new(),
        rationale: "seed".to_string(),
    };
    let outcome =
        playbook::apply_deltas(&engine, agent_id, vec![add], &[], evals.path(), Utc::now())
            .await;
    assert!(
        outcome.rejected.is_empty(),
        "seed Add must not be rejected: {:?}",
        outcome.rejected
    );

    let active = playbook::list_active(&engine, agent_id).await;
    active[0].0.id.clone()
}

#[tokio::test]
async fn playbook_list_returns_the_seeded_entry() {
    let home = tempfile::tempdir().expect("tempdir");
    seed_playbook_entry(home.path(), "agent-a").await;

    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle_playbook_list(json!({ "agent_id": "agent-a" }))
        .await;
    let p = payload(&frame);
    let entries = p["entries"].as_array().expect("entries array");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["category"], "repair");
    assert_eq!(entries[0]["state"], "probation");
    assert_eq!(entries[0]["helpful"], 1);
    assert_eq!(entries[0]["harmful"], 0);
}

#[tokio::test]
async fn playbook_retire_marks_entry_retired() {
    let home = tempfile::tempdir().expect("tempdir");
    let id = seed_playbook_entry(home.path(), "agent-a").await;

    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle_playbook_retire(
            json!({ "agent_id": "agent-a", "id": id, "reason": "no longer needed" }),
        )
        .await;
    let p = payload(&frame);
    assert_eq!(p["success"], true);
    assert_eq!(p["retired"], true);

    let list_frame = handler
        .handle_playbook_list(json!({ "agent_id": "agent-a" }))
        .await;
    let entries = payload(&list_frame);
    assert_eq!(entries["entries"][0]["state"], "retired");
}

#[tokio::test]
async fn playbook_retire_unknown_id_reports_not_retired_without_erroring() {
    let home = tempfile::tempdir().expect("tempdir");
    seed_playbook_entry(home.path(), "agent-a").await;

    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle_playbook_retire(json!({ "agent_id": "agent-a", "id": "does-not-exist" }))
        .await;
    let p = payload(&frame);
    assert_eq!(p["success"], false);
    assert_eq!(p["retired"], false);
}

#[tokio::test]
async fn playbook_retire_missing_id_param_is_an_error() {
    let home = tempfile::tempdir().expect("tempdir");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle_playbook_retire(json!({ "agent_id": "agent-a" }))
        .await;
    assert!(matches!(frame, WsFrame::Response { ok: false, .. }));
    assert!(error_text(&frame).contains("id"));
}

#[tokio::test]
async fn playbook_export_produces_lossless_gene_json() {
    let home = tempfile::tempdir().expect("tempdir");
    seed_playbook_entry(home.path(), "agent-a").await;

    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle_playbook_export(json!({ "agent_id": "agent-a" }))
        .await;
    let p = payload(&frame);
    assert_eq!(p["gene_schema"], playbook::gene::GENE_SCHEMA);
    let genes = p["genes"].as_array().expect("genes array");
    assert_eq!(genes.len(), 1);
    assert_eq!(genes[0]["type"], "gene");
    assert_eq!(genes[0]["category"], "repair");
    assert_eq!(genes[0]["x-duduclaw"]["agent_id"], "agent-a");
    assert!(
        !genes[0]["x-duduclaw"]["entry_id"]
            .as_str()
            .unwrap()
            .is_empty()
    );
}
