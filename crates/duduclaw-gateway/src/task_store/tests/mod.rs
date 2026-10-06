//! Unit tests for [`super`], moved verbatim out of the former `task_store.rs`.
//!
//! Shared fixtures live here; the cases are split across the sibling
//! files for size only.

mod claim_cases;
mod authority_cases;
mod criteria_cases;
mod goal_cases;
mod ledger_cases;
mod plan_cases;
mod pure_cases;
mod review_cases;
mod task_cases;

use super::{
    ActivityRow, ClaimOutcome, CommentRow, GoalRow, PlanStepRow, TaskRow, TaskStore, ZombieAction,
    deps_satisfied, introduces_dependency_cycle, introduces_parent_cycle, lease_is_expired,
    parse_depends_on, zombie_action, zombie_reclaim_due,
};

use std::collections::HashSet;

fn temp_store() -> (TaskStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = TaskStore::open(dir.path()).expect("open store");
    (store, dir)
}

fn comment(id: &str, task: &str, at: &str, body: &str) -> CommentRow {
    CommentRow {
        id: id.into(),
        task_id: task.into(),
        author_user: "user-1".into(),
        body: body.into(),
        created_at: at.into(),
    }
}

fn edges(pairs: &[(&str, Option<&str>)]) -> Vec<(String, Option<String>)> {
    pairs
        .iter()
        .map(|(id, p)| (id.to_string(), p.map(|s| s.to_string())))
        .collect()
}

// ── G1 dispatch: SQLite lifecycle ───────────────────────

fn pending_task(id: &str) -> TaskRow {
    let mut t = TaskRow::new(
        id.into(),
        format!("task {id}"),
        String::new(),
        "medium".into(),
        String::new(),
        "system".into(),
    );
    t.status = "pending".into();
    t
}

// ── G8 goal chain ───────────────────────────────────────

fn goal(id: &str, title: &str, parent: Option<&str>) -> GoalRow {
    let mut g = GoalRow::new(id.into(), title.into(), format!("why of {id}"));
    g.parent_goal_id = parent.map(String::from);
    g
}

// ── U4 co-edited plans ───────────────────────────────────

use super::{PLAN_STEP_ORDER_GAP, PlanRow, plan_order_for_insert};

fn plan(id: &str, agent: &str) -> PlanRow {
    PlanRow::new(
        id.into(),
        format!("Plan {id}"),
        agent.into(),
        "user-1".into(),
    )
}

// ── Iterative Kanban (v1.45) ────────────────────────────

/// A goal-mode task in `review`, ready for a judge verdict.
fn goal_review_task(id: &str) -> TaskRow {
    let mut t = TaskRow::new(
        id.into(),
        format!("goal {id}"),
        "do the work".into(),
        "medium".into(),
        "alice".into(),
        "system".into(),
    );
    t.status = "review".into();
    t.goal_mode = true;
    t.max_retries = 5;
    t.acceptance_criteria = Some("must be correct".into());
    t.result_summary = Some("attempt".into());
    t
}
