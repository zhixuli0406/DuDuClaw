//! T5/O4 — the one place an **autonomous goal** is created.
//!
//! The 2026-09-29 feature audit (§1 T5, row O4) found four MCP/RPC entry
//! points that all read as "create a task". Two of them create genuinely
//! different objects (`tasks_create` with `schedule` writes a cron row; `goals_create`
//! writes a why-chain hierarchy node), but `tasks.goal_create` — the
//! dashboard's "hand this agent a goal" RPC — had no agent-facing twin at
//! all: an agent could only create a plain board task and hope the goal loop
//! noticed. Merging the entry points therefore meant giving the MCP
//! `tasks_create` tool a `kind = "goal"` that lands on **this** function,
//! rather than on a second, drifting copy of the goal contract.
//!
//! What lives here is everything that must be identical no matter who asks:
//!
//! - priority / `outcome` / `duration_hours` validation (fail-closed — a
//!   malformed structured outcome refuses the whole create, it is never
//!   dropped),
//! - the belief-declaration teaching paragraph (`require_beliefs`),
//! - **the H9-G contract freeze**: `acceptance_criteria_baseline` is the
//!   immutable snapshot the judge reads; the mutable `acceptance_criteria`
//!   copy is display-only,
//! - I-1c `plan_first` ("想一想"): generate a narrative plan and park the task
//!   `needs_human` instead of letting the loop start — fail-closed, a planner
//!   failure parks under the `infra` pause class rather than falling through
//!   to `todo`,
//! - the Team-as-Agent role freeze and the `goal_loop.created` activity row.
//!
//! What stays at the call sites: **authorisation** (dashboard Operator ACL vs.
//! MCP delegation policy) and the response shape. Authorisation is
//! deliberately *not* folded in — the two callers answer to different
//! authorities and a single "is this allowed" flag would be a fake seam.

use std::path::Path;

use chrono::{Duration as ChronoDuration, Utc};
use tracing::warn;

use crate::task_store::{TaskRow, TaskStore};

/// Everything a caller must decide before a goal exists. Already-authorised:
/// constructing one of these does not grant permission, it only describes the
/// goal.
#[derive(Debug, Clone)]
pub struct GoalCreateRequest {
    /// Agent the goal is assigned to. Callers validate the id and their own
    /// right to assign to it before calling.
    pub agent_id: String,
    /// `created_by` provenance, e.g. `goal:dashboard` or the calling agent id.
    pub created_by: String,
    /// The goal text. Truncated to 4000 chars here so both rails share one cap.
    pub description: String,
    /// Explicit acceptance criteria. `None` ⇒ the goal text itself becomes the
    /// contract (same rule as the chat `/goal` path).
    pub acceptance_criteria: Option<String>,
    /// `low` / `medium` / `high` / `urgent`. Empty ⇒ `medium`.
    pub priority: String,
    /// Structured outcome spec source text (`outcome_spec::OutcomeSpec`).
    pub outcome: Option<String>,
    /// Per-goal wall clock, 1–720 hours. `None` ⇒ deployment default.
    pub duration_hours: Option<f64>,
    /// Per-goal risk boundary text. `None` ⇒ `[goal_defaults] baseline_boundary`.
    pub risk_boundary: Option<String>,
    /// Require structured belief declarations during execution.
    pub require_beliefs: bool,
    /// I-1c: plan first, park for human approval instead of executing.
    pub plan_first: bool,
    /// The task this goal hangs under (P2-A H-2): for an employee creating a
    /// goal during a round, the host decides it (the round's task), so the
    /// goal lands in that run's tree — its cost is counted there and a stop
    /// of the run reaches it. `None` ⇒ a top-level goal (dashboard, chat).
    pub parent_task_id: Option<String>,
    /// Human-readable origin used in the `goal_loop.created` activity line
    /// (e.g. `儀表板` / `AI 員工`). Empty ⇒ no prefix. Display only — never a
    /// permission or routing input.
    pub source_label: String,
}

/// The created goal plus the flags a caller needs to render its own response.
#[derive(Debug)]
pub struct GoalCreated {
    pub task: TaskRow,
    pub plan_first: bool,
}

/// Create one autonomous goal task. `Err` is a caller-facing message.
pub async fn create_goal_task(
    home_dir: &Path,
    store: &TaskStore,
    req: GoalCreateRequest,
) -> Result<GoalCreated, String> {
    let description = req.description.trim();
    if description.is_empty() {
        return Err("description is required".to_string());
    }
    let description = duduclaw_core::truncate_chars(description, 4000);
    let acceptance = req
        .acceptance_criteria
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| duduclaw_core::truncate_chars(s, 4000));
    let priority = match req.priority.trim() {
        "" => "medium",
        p @ ("low" | "medium" | "high" | "urgent") => p,
        other => return Err(format!("invalid priority: {other}")),
    };
    // Structured outcome spec: fail-closed parse — a malformed spec refuses
    // the whole create rather than silently dropping the contract.
    let outcome_tag = match req
        .outcome
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(crate::outcome_spec::OutcomeSpec::parse)
    {
        Some(Ok(spec)) => spec.to_tag(),
        Some(Err(e)) => return Err(format!("outcome 產出驗收格式錯誤：{e}")),
        None => None,
    };
    let duration_hours = match req.duration_hours {
        None => None,
        Some(h) if (1.0..=720.0).contains(&h) => Some(h),
        Some(_) => return Err("duration_hours must be between 1 and 720".to_string()),
    };
    let risk_boundary = req
        .risk_boundary
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| duduclaw_core::truncate_chars(s, 2000));

    let task_id = uuid::Uuid::new_v4().to_string();
    // Title reflects the goal itself — computed from the base description
    // BEFORE the belief-declaration teaching paragraph is appended below, so
    // the 60-char truncation never crowds out the actual goal text with
    // boilerplate.
    let title = duduclaw_core::truncate_chars(&description, 60);
    let description = if req.require_beliefs {
        format!(
            "{description}\n\n【信念申報要求】執行期間對目標相關的可量測外部指標用 \
             belief_submit 申報結構化預測（方向+信心 0-1+基準值），結果可得時用 \
             belief_settle 結算；驗收時需附申報與結算摘要。"
        )
    } else {
        description
    };
    let mut task = TaskRow::new(
        task_id.clone(),
        title,
        description.clone(),
        priority.to_string(),
        req.agent_id.clone(),
        req.created_by.clone(),
    );
    task.status = "todo".to_string();
    task.goal_mode = true;
    task.parent_task_id = req.parent_task_id.clone();
    let mut acceptance_criteria = acceptance.unwrap_or_else(|| description.clone());
    if req.require_beliefs {
        acceptance_criteria.push_str("；至少一筆 belief_submit 申報且已知結果者皆已結算");
    }
    task.acceptance_criteria = Some(acceptance_criteria.clone());
    // H9-G goal contract freeze (harness-borrowings 2026-08 WP-D): the judge
    // reads this column, never the mutable `acceptance_criteria` field.
    task.acceptance_criteria_baseline = Some(acceptance_criteria);
    // WP-G2: number the frozen baseline into a per-criterion ledger, once,
    // at the same frozen-contract moment. `[goal_loop] criteria_ledger =
    // "off"` (or an empty baseline) creates none, and a goal without a
    // ledger behaves exactly as before WP-G2.
    task.criteria_ledger = crate::goal_loop::criteria_ledger::ledger_for_new_goal(
        home_dir,
        &task_id,
        task.acceptance_criteria_baseline.as_deref(),
    );
    if let Some(tag) = &outcome_tag {
        task.tags = tag.clone();
    }
    if let Some(hours) = duration_hours {
        task.deadline_at =
            Some((Utc::now() + ChronoDuration::minutes((hours * 60.0).round() as i64)).to_rfc3339());
    }
    task.risk_boundary = risk_boundary;

    // I-1c "想一想": generate the plan and park the task `needs_human` instead
    // of `todo` — runs synchronously, so no dispatch-engine round (not even a
    // restricted one) ever starts before a human approves. Fail-closed: a
    // planner failure never falls through to the normal `todo` path —
    // `apply_plan_first_result`'s `Err` branch still parks `needs_human`,
    // under the `infra` pause class, for a human to retry or cancel.
    if req.plan_first {
        let criteria_for_plan = task.acceptance_criteria.as_deref().unwrap_or("");
        let plan_result =
            crate::goal_plan::generate_plan_first(home_dir, &task.description, criteria_for_plan)
                .await;
        if let Err(e) = &plan_result {
            warn!(
                agent = %req.agent_id,
                error = %e,
                "goal plan-first: planner failed — parking needs_human (infra class) instead of auto-starting"
            );
        }
        crate::goal_plan::apply_plan_first_result(&mut task, plan_result);
    }

    store
        .insert_task(&task)
        .await
        .map_err(|e| format!("create goal task: {e}"))?;

    // Team-as-Agent P1/WP-4: freeze the role→{runtime, model, effort} team for
    // this task, once, right after creation — same frozen-contract moment as
    // `acceptance_criteria_baseline` above. `[team] enabled` defaults to false,
    // in which case this stores nothing, audits nothing, and the task runs
    // exactly as it does today. A spec that fails validation (notably a
    // verifier sharing the executor's model family) forms no team at all and
    // the task runs Solo — never a partial team.
    let freeze = crate::team_composer::freeze_for_task(home_dir, store, &task_id, &req.agent_id).await;
    if let crate::team_composer::FreezeOutcome::Frozen(spec) = &freeze {
        task.team_spec_json = serde_json::to_string(spec).ok();
    }
    let _ = store
        .append_activity(&crate::task_store::ActivityRow {
            id: uuid::Uuid::new_v4().to_string(),
            event_type: "goal_loop.created".into(),
            agent_id: req.agent_id.clone(),
            task_id: Some(task_id),
            summary: if req.plan_first {
                format!(
                    "{}產生執行計畫「{}」，等待核准後才開始執行",
                    req.source_label,
                    duduclaw_core::truncate_chars(&description, 60)
                )
            } else {
                format!(
                    "{}指派目標任務「{}」",
                    req.source_label,
                    duduclaw_core::truncate_chars(&description, 60)
                )
            },
            timestamp: Utc::now().to_rfc3339(),
            metadata: None,
        })
        .await;

    Ok(GoalCreated {
        task,
        plan_first: req.plan_first,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(agent: &str) -> GoalCreateRequest {
        GoalCreateRequest {
            agent_id: agent.to_string(),
            created_by: "goal:test".to_string(),
            description: "整理報表".to_string(),
            acceptance_criteria: None,
            priority: String::new(),
            outcome: None,
            duration_hours: None,
            risk_boundary: None,
            require_beliefs: false,
            plan_first: false,
            parent_task_id: None,
            source_label: String::new(),
        }
    }

    async fn store_in(home: &Path) -> TaskStore {
        TaskStore::open(home).expect("open task store")
    }

    /// The contract freeze is the whole point of routing both callers here:
    /// the judge-visible baseline must exist even when the caller supplied no
    /// explicit criteria.
    #[tokio::test]
    async fn goal_create_core_freezes_the_goal_text_as_the_baseline() {
        let home = tempfile::tempdir().expect("tempdir");
        let store = store_in(home.path()).await;
        let created = create_goal_task(home.path(), &store, req("a"))
            .await
            .expect("create");
        assert!(created.task.goal_mode);
        assert_eq!(created.task.status, "todo");
        assert_eq!(
            created.task.acceptance_criteria_baseline.as_deref(),
            Some("整理報表")
        );
        assert_eq!(created.task.priority, "medium");
    }

    /// Round 4 (H-2a): a goal created during a round carries the parent the
    /// caller resolved (the round's task), so its spend counts toward it.
    #[tokio::test]
    async fn goal_create_core_keeps_the_parent() {
        let home = tempfile::tempdir().expect("tempdir");
        let store = store_in(home.path()).await;
        let parent = create_goal_task(home.path(), &store, req("a"))
            .await
            .expect("parent");
        let mut r = req("a");
        r.parent_task_id = Some(parent.task.id.clone());
        let child = create_goal_task(home.path(), &store, r).await.expect("child");
        assert_eq!(child.task.parent_task_id.as_deref(), Some(parent.task.id.as_str()));
        let stored = store.get_task(&child.task.id).await.unwrap().unwrap();
        assert_eq!(stored.parent_task_id.as_deref(), Some(parent.task.id.as_str()));
    }

    #[tokio::test]
    async fn goal_create_core_freezes_explicit_criteria() {
        let home = tempfile::tempdir().expect("tempdir");
        let store = store_in(home.path()).await;
        let mut r = req("a");
        r.acceptance_criteria = Some("含營收圖表".into());
        let created = create_goal_task(home.path(), &store, r).await.expect("create");
        assert_eq!(
            created.task.acceptance_criteria_baseline.as_deref(),
            Some("含營收圖表")
        );
    }

    /// WP-G2: the default mode (`report`, no config) numbers the frozen
    /// baseline into a ledger; `off` creates none.
    #[tokio::test]
    async fn goal_create_core_builds_the_criteria_ledger_unless_off() {
        use crate::goal_loop::criteria_ledger::{CriteriaLedger, CriterionStatus};
        let home = tempfile::tempdir().expect("tempdir");
        let store = store_in(home.path()).await;
        let mut r = req("a");
        r.acceptance_criteria = Some("含營收圖表\n\n寄出月報".into());
        let created = create_goal_task(home.path(), &store, r).await.expect("create");
        let stored = store.get_task(&created.task.id).await.unwrap().unwrap();
        let ledger = CriteriaLedger::from_json(stored.criteria_ledger.as_deref()).expect("ledger");
        assert_eq!(ledger.units.len(), 2);
        assert_eq!(ledger.units[1].handle, "C2");
        assert_eq!(ledger.units[1].text, "寄出月報");
        assert!(ledger.units.iter().all(|u| u.status == CriterionStatus::Planned));
        assert!(ledger.units[0].id.starts_with(&created.task.id));

        std::fs::write(
            home.path().join("config.toml"),
            "[goal_loop]\ncriteria_ledger = \"off\"\n",
        )
        .unwrap();
        let created = create_goal_task(home.path(), &store, req("a")).await.expect("create");
        let stored = store.get_task(&created.task.id).await.unwrap().unwrap();
        assert!(stored.criteria_ledger.is_none());
    }

    /// Fail-closed validation lives here so both rails reject the same inputs
    /// with the same message.
    #[tokio::test]
    async fn goal_create_core_rejects_bad_priority_outcome_and_duration() {
        let home = tempfile::tempdir().expect("tempdir");
        let store = store_in(home.path()).await;

        let mut r = req("a");
        r.priority = "urgentish".into();
        assert_eq!(
            create_goal_task(home.path(), &store, r).await.unwrap_err(),
            "invalid priority: urgentish"
        );

        let mut r = req("a");
        r.duration_hours = Some(0.5);
        assert_eq!(
            create_goal_task(home.path(), &store, r).await.unwrap_err(),
            "duration_hours must be between 1 and 720"
        );

        let mut r = req("a");
        r.description = "   ".into();
        assert_eq!(
            create_goal_task(home.path(), &store, r).await.unwrap_err(),
            "description is required"
        );
    }
}
