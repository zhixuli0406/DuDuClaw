//! Unit tests for [`super`], moved verbatim out of the former `goal_loop.rs`.
//!
//! Shared fixtures live here; the cases are split across the sibling
//! files for size only.

mod config_cases;
mod enqueue_cases;
mod escalate_cases;
mod kickoff_cases;
mod notify_cases;
mod tick_cases;

use super::*;

use crate::task_store::TaskRow;

// ── R5: `[capabilities] autonomy_level` direction, pinned ────────────
//
// absent / malformed / wrong-typed / unrecognised ⇒ `Approver` — the
// conservative level, NEVER the most-autonomous one. Two separate
// fallbacks point the same way on purpose: the missing-key fallback here
// and `from_toml_str`'s unknown-string fallback. The value stays a raw
// String on the typed section so the second one keeps running instead of
// a strict serde enum making a typo fatal to the whole `AgentConfig`
// (which would drop the agent from the registry entirely).

fn home_with_agent(agent_id: &str, body: &str) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("agents").join(agent_id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("agent.toml"), body).unwrap();
    home
}

fn driver(
    store: Arc<TaskStore>,
    queue: Arc<MessageQueue>,
    cfg: GoalLoopConfig,
) -> GoalLoopDriver {
    GoalLoopDriver::new(store, queue, cfg)
}

fn small_cfg() -> GoalLoopConfig {
    GoalLoopConfig {
        iteration_cap: 2,
        // Kept equal to `iteration_cap` so the short test goal texts (which
        // classify as Simple) exercise the same effective cap as before D4.
        iteration_cap_simple: 2,
        soft_cap: 3,
        wall_clock_hours: 24,
        max_concurrent: 3,
        tick_secs: 30,
        stalled_secs: 600,
        // H22 off by default in tests — the timeout-report tests build
        // their own config so every other test's tick stays query-free.
        progress_report_minutes: 0,
        resume_on_restart: "auto".to_string(),
        tool_streak_advisory: true,
    }
}

/// A todo goal task assigned to `agent`.
fn goal_task(id: &str, agent: &str) -> TaskRow {
    let mut t = TaskRow::new(
        id.into(),
        format!("goal {id}"),
        "do the work".into(),
        "medium".into(),
        agent.into(),
        "system".into(),
    );
    t.status = "todo".into();
    t.goal_mode = true;
    t.acceptance_criteria = Some("must be correct".into());
    t
}

async fn open_stores(dir: &Path) -> (Arc<TaskStore>, Arc<MessageQueue>) {
    let store = Arc::new(TaskStore::open(dir).unwrap());
    let queue = Arc::new(MessageQueue::open(dir).unwrap());
    (store, queue)
}

// ── Team-as-Agent P1/WP-4: the Solo path must stay unchanged ────────

/// A `[team]` config with a valid cross-family pair, so the freeze
/// succeeds and only the *gate* decides Solo vs Team.
fn write_enabled_team(home: &Path) {
    std::fs::write(
        home.join("config.toml"),
        "[team]\nenabled = true\n\
             [team.roles.planner]\nruntime = \"claude\"\nmodel = \"claude-fable-5-1\"\n\
             [team.roles.executor]\nruntime = \"codex\"\nmodel = \"gpt-5.5\"\n\
             [team.roles.verifier]\nruntime = \"gemini\"\nmodel = \"gemini-3.7-flash\"\n",
    )
    .unwrap();
}

// ── H10: tool-call streak advisory — capture_round_state integration ──

/// Write `count` identical `(tool, input)` calls for `agent` into
/// `<dir>/tool_calls.jsonl`, timestamped "now" — paired with a task
/// `claimed_at` fixed safely in the past, this lands every write inside
/// `capture_round_state`'s `[since, until]` evidence window.
fn write_tool_calls_jsonl(dir: &Path, agent: &str, tool: &str, input: &str, count: usize) {
    let lines: Vec<String> = (0..count)
        .map(|_| {
            serde_json::json!({
                "timestamp": Utc::now().to_rfc3339(),
                "agent_id": agent,
                "tool_name": tool,
                "success": true,
                "input": input,
            })
            .to_string()
        })
        .collect();
    std::fs::write(
        dir.join("tool_calls.jsonl"),
        format!("{}\n", lines.join("\n")),
    )
    .unwrap();
}

fn claimed_review_task(id: &str, agent: &str) -> TaskRow {
    let mut t = goal_task(id, agent);
    t.claimed_by = Some(agent.to_string());
    // Safely in the past so any "now"-stamped tool_calls.jsonl row lands
    // inside `capture_round_state`'s `[since, until]` window.
    t.claimed_at = Some("2020-01-01T00:00:00Z".into());
    t.status = "review".into();
    t.result_summary = Some("did the thing".into());
    t
}

// ── I-1c "想一想" plan-first: end-to-end through the driver ──────────
//
// These simulate exactly what `handlers.rs::handle_tasks_goal_create` +
// `goal_plan::apply_plan_first_result` produce on the `Ok` branch — a task
// born directly in `needs_human` with `plan_pending` set — without going
// through the real (network-calling) planner, matching this file's own
// testing convention (the concrete LLM caller is exercised by live
// verification, not a unit test; see `goal_loop/plan.rs`'s `StubCaller` tests
// for the generation logic itself).
fn plan_first_pending_task(id: &str, agent: &str, plan: &str) -> TaskRow {
    let mut t = goal_task(id, agent);
    t.status = "needs_human".into();
    t.pause_reason = Some(
        crate::pause_reason::PauseReason::BlockedNeedsDecision
            .as_str()
            .into(),
    );
    t.judge_feedback = Some(plan.into());
    t.plan_pending = Some(plan.into());
    t
}

/// A plan awaiting approval must NEVER execute — the whole point of
/// "想一想" is that nothing runs before a human decides. `needs_human` is
/// not one of the driver's dispatch-candidate statuses
/// (`todo`/`pending`/`revising`), so this is really testing that the
/// plan-first creation path (parking directly in `needs_human`) actually
/// keeps the task out of the loop — not a new guard, the existing
/// candidate-status filter already provides it.
// ── Synchronous dispatch failure → slot freed, back-off, escalation ──
//
// 2026-09-06 appliance walkthrough: two goal tasks whose work messages
// failed synchronously (local engine misconfigured) kept their in-flight
// slots AND their edition leases, so with the Personal cap of 2 every
// later goal task was deferred with "edition concurrency cap reached"
// until the 30-minute lease TTL — a restart did not help either.

async fn fail_all_pending(queue: &MessageQueue, error: &str) -> usize {
    let pending = queue.pending_messages(50).await.unwrap();
    for m in &pending {
        queue.fail(&m.id, error).await.unwrap();
    }
    pending.len()
}

// ── A2 no-progress guard (structural, replaces the old P3 two-round
//    identical-judge-feedback oscillation guard) ─────────

/// Drive one full rejection round for a task already tracked in-flight and
/// awaiting pickup: (1) agent moves it to `review` and a tick observes that
/// (flips `awaiting_pickup=false`, does not re-dispatch); (2) the judge
/// rejects with `feedback` (→ `revising`, `judge_feedback` set); (3) the next
/// tick is the rejection re-dispatch the caller runs. This helper performs
/// steps 1–2 and returns; the caller ticks for step 3.
async fn agent_round_then_reject(
    d: &GoalLoopDriver,
    store: &Arc<TaskStore>,
    id: &str,
    feedback: &str,
) {
    // Agent picked it up and produced work → review.
    store
        .update_task(id, &serde_json::json!({ "status": "review" }))
        .await
        .unwrap();
    // Tick while in review so the driver marks it no-longer-awaiting-pickup.
    d.tick_once().await.unwrap();
    // Judge rejects → revising + judge_feedback (soft_cap 3 — a high value so
    // the diminishing flag never interferes with these oscillation tests).
    store.reject_review(id, feedback, 99).await.unwrap();
}

// ── P2a autonomy level + kickoff gate ───────────────────

fn write_agent_toml(home: &Path, agent: &str, body: &str) {
    let dir = home.join("agents").join(agent);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("agent.toml"), body).unwrap();
}

// ── W3-1 (D5): the goal loop is the highest-volume dispatch path into a
//    conversation. A human who takes over must freeze it — and only it. ──

fn begin_takeover(home: &Path, channel: &str, chat_id: &str) {
    duduclaw_core::takeover_state::begin(
        home,
        &duduclaw_core::takeover_state::BeginRequest {
            conversation: format!("{channel}:{chat_id}"),
            agent_id: "alice".into(),
            holder_user_id: "555".into(),
            holder_display: "王小明".into(),
        },
        &duduclaw_core::takeover_state::TakeoverConfig::default(),
        chrono::Utc::now(),
    )
    .unwrap();
}

fn goal_task_from_chat(id: &str, chat_id: &str) -> TaskRow {
    let mut t = goal_task(id, "alice");
    t.source_channel = Some("telegram".into());
    t.source_chat_id = Some(chat_id.to_string());
    t
}

// ── H11: every escalation path stamps its pause class ───────────────
//
// One test per trigger, asserting BOTH the (unchanged) free-text reason
// and the new class — the whole point of H11 is that a human triages on
// the class, so a trigger silently landing in the wrong bucket is a real
// regression, not a cosmetic one.

async fn pause_class_of(store: &TaskStore, id: &str) -> crate::pause_reason::PauseReason {
    let t = store.get_task(id).await.unwrap().unwrap();
    assert_eq!(t.status, "needs_human", "{id} should be parked");
    crate::pause_reason::PauseReason::from_stored(t.pause_reason.as_deref())
}

/// Test-only: put a task into the driver's in-flight map with a chosen
/// dispatch instant, so the silence window can be exercised without
/// waiting real minutes (and without any activity row the driver's own
/// dispatch would have stamped at "now").
fn inflight_entry(iter: u32, enqueued_at: DateTime<Utc>) -> InFlight {
    InFlight {
        iter,
        enqueued_at,
        awaiting_pickup: false,
        lease: None,
        progress_reported_round: None,
        message_id: None,
    }
}

async fn progress_report_events(store: &TaskStore, id: &str) -> usize {
    store
        .list_activity_for_task(id, 100)
        .await
        .unwrap()
        .into_iter()
        .filter(|a| a.event_type == "goal_loop.progress_report")
        .count()
}

