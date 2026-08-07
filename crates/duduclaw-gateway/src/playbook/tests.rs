//! Cross-module integration tests for the playbook pipeline (WP1.2 + WP1.3).
//!
//! Per-module unit tests live beside their code (`entry.rs`, `dedup.rs`,
//! `signals.rs`, `gene.rs`, `delta.rs`, `store.rs`, `select.rs`, `sweep.rs`).
//! This file is for behavior that genuinely spans multiple modules — the
//! full `apply_deltas` → `select_playbook` → `render_section` pipeline, as a
//! real caller (`channel_reply.rs`) exercises it — matching the `gvu/tests.rs`
//! convention of a dedicated top-level test file per subsystem.

use chrono::Utc;

use duduclaw_memory::SqliteMemoryEngine;

use crate::playbook::delta::PlaybookDelta;
use crate::playbook::entry::PlaybookCategory;
use crate::playbook::gene::EvalCaseRef;
use crate::playbook::select::{render_section, select_playbook, InjectionBudget};
use crate::playbook::signals::TurnSignals;
use crate::playbook::store::apply_deltas;

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

fn add_delta(content: &str, signals: Vec<&str>) -> PlaybookDelta {
    PlaybookDelta::Add {
        assertions: crate::playbook::entry::EntryAssertions { output_contains: vec!["ok".to_string()], ..Default::default() },
        content: content.to_string(),
        category: PlaybookCategory::Repair,
        signals_match: signals.into_iter().map(String::from).collect(),
        eval_cases: vec![EvalCaseRef("s/c".to_string())],
        strategy: Vec::new(),
        rationale: "test".to_string(),
    }
}

#[tokio::test]
async fn end_to_end_add_then_signal_matched_selection_and_render() {
    let engine = SqliteMemoryEngine::in_memory().unwrap();
    let evals = temp_eval_root();
    let agent = "agent-e2e";
    let now = Utc::now();

    let deltas = vec![
        // Not matched by this turn's signals at all.
        add_delta("watch discord rate limits", vec!["channel:discord"]),
        // Matched by this turn's mistake:capability signal.
        add_delta("always confirm refund amount before executing", vec!["mistake:capability"]),
    ];
    let outcome = apply_deltas(&engine, agent, deltas, &[], evals.path(), now).await;
    assert_eq!(outcome.applied.len(), 2);
    assert!(outcome.rejected.is_empty());

    let turn = TurnSignals::new().with_mistake_category("capability").with_channel("telegram");
    let selected = select_playbook(&engine, agent, &turn).await;

    // The signal-matched entry (mistake:capability) must be first, ahead of
    // the discord-only entry (doesn't match this turn's signals at all).
    assert_eq!(selected[0].content, "always confirm refund amount before executing");
    assert!(selected[0].signal_matched);
    assert!(!selected[1].signal_matched);

    let budget = InjectionBudget::default_budget();
    let (section, ids) = render_section(&selected, &budget).unwrap();
    assert!(section.starts_with("## Learned Rules"));
    assert!(section.contains("always confirm refund amount"));
    assert_eq!(ids.len(), 2, "small budget comfortably fits both short entries");
}

#[tokio::test]
async fn retired_entry_never_resurfaces_in_selection_after_retire_delta() {
    let engine = SqliteMemoryEngine::in_memory().unwrap();
    let evals = temp_eval_root();
    let agent = "agent-e2e-retire";
    let now = Utc::now();

    let outcome = apply_deltas(&engine, agent, vec![add_delta("temporary rule", vec!["*"])], &[], evals.path(), now).await;
    let id = match &outcome.applied[0] {
        crate::playbook::delta::AppliedOp::Added { .. } => {
            // Fetch the real persisted id via select (Added doesn't carry
            // the minted storage id — only store.rs knows it).
            let turn = TurnSignals::new();
            select_playbook(&engine, agent, &turn).await[0].id.clone()
        }
        _ => panic!("expected Added"),
    };

    let retire = PlaybookDelta::Retire { id, reason: "no longer needed".to_string() };
    apply_deltas(&engine, agent, vec![retire], &[], evals.path(), now).await;

    let turn = TurnSignals::new();
    let selected = select_playbook(&engine, agent, &turn).await;
    assert!(selected.is_empty(), "retired entry must be excluded from selection");
}

#[tokio::test]
async fn no_eval_case_link_blocks_add_end_to_end_g6() {
    let engine = SqliteMemoryEngine::in_memory().unwrap();
    let evals = temp_eval_root();
    let agent = "agent-e2e-g6";
    let now = Utc::now();

    let bad = PlaybookDelta::Add {
        assertions: crate::playbook::entry::EntryAssertions { output_contains: vec!["ok".to_string()], ..Default::default() },
        content: "rule with no validation".to_string(),
        category: PlaybookCategory::Repair,
        signals_match: vec!["*".to_string()],
        eval_cases: Vec::new(),
        strategy: Vec::new(),
        rationale: "x".to_string(),
    };
    let outcome = apply_deltas(&engine, agent, vec![bad], &[], evals.path(), now).await;
    assert!(outcome.applied.is_empty());
    assert_eq!(outcome.rejected.len(), 1);

    let turn = TurnSignals::new();
    assert!(select_playbook(&engine, agent, &turn).await.is_empty());
}
