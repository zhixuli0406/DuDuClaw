//! "Stop this run (and its sub-tasks)" — service and reconciliation.
//!
//! The transaction (`TaskStore::stop_task_tree`) cancels the tree at once.
//! What cannot be undone that way is reported honestly: a turn the
//! dispatcher is already awaiting runs to its end (`cancel_pending` until it
//! does), an external action already executing cannot be recalled, and an
//! action whose outcome is unknown needs a human (`stopped_uncertain`).
//! Reconciliation reruns on every driver tick until a terminal state.
//! Delegations the run already sent are separate runs and are not followed.
//!
//! What keeps a stop `cancel_pending` (round 3, E-H2/E-H3/S-M2): a goal
//! message waiting or held in the queue, the goal-loop driver in the middle of
//! dispatching a tree member, a claim on a tree member whose lease has not
//! expired (any path: heartbeat, delegation, …), a team round, an executing
//! external action, an unreadable approval store, or tree members the batch
//! cancel has not reached yet. A lapsed claim with no finished round on
//! record, or a tree larger than the scan limit, ends `stopped_uncertain`.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::activity;
use super::service::ServiceError;
use super::wake::post;
use crate::approval::{ApprovalBroker, OperationState};
use crate::message_queue::{MessageQueue, MessageStatus};
use crate::task_store::{
    STOP_TREE_SCAN_LIMIT, StopRequestRow, StopTreeOutcome, TaskStore, resp_ts,
};

/// Reconciliation detail stored in `task_stop_requests.detail_json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct StopDetail {
    /// Turns the dispatcher is still running (cannot be interrupted).
    pub running_turns: usize,
    /// Waiting queue messages failed by this reconciliation.
    pub messages_failed: usize,
    pub approvals_invalidated: usize,
    pub operations_prepared: usize,
    pub operations_executing: usize,
    pub operations_uncertain: usize,
    pub operations_succeeded: usize,
    pub operations_failed: usize,
    /// The approval store could not be read: operation states unknown.
    pub approval_store_unavailable: bool,
    /// Team rounds positively seen running (in-process registry or a live
    /// role member scaffold). They never touch the queue.
    pub team_rounds_running: usize,
    /// A team round may still be running but this process cannot see it (the
    /// round would run in a gateway whose registry is not this one). The stop
    /// stays `cancel_pending` until `unverified_hold_until`, then ends
    /// `stopped_uncertain` — never `stopped`.
    pub unverified_work_possible: bool,
    pub unverified_hold_until: Option<String>,
    /// Tree members the goal-loop driver is dispatching right now.
    pub dispatch_in_flight: usize,
    /// Tree members still claimed with an unexpired lease (may be running a
    /// turn on a path the queue does not show).
    pub claims_running: usize,
    /// Latest lease among `claims_running`.
    pub claims_lease_until: Option<String>,
    /// Claimed members whose lease lapsed with no finished round on record:
    /// cannot be confirmed either way.
    pub claims_unconfirmed: usize,
    /// Members cancelled by this pass's batch.
    pub tree_cancelled: usize,
    /// Members still open after this pass (handled by the next passes).
    pub tree_still_open: usize,
    /// The tree is larger than the scan limit: members beyond it were never
    /// looked at, so the stop cannot be confirmed.
    pub tree_beyond_scan: bool,
    pub step_errors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StopStatus {
    pub root_task_id: String,
    /// `cancel_pending` | `stopped` | `stopped_uncertain`.
    pub state: String,
    pub affected_task_ids: Vec<String>,
    pub detail: StopDetail,
}

/// Dashboard `tasks.stop` backing. Authorization is the caller's; the root
/// CAS is on the task's `authority_revision`.
pub async fn stop_task(
    store: &TaskStore,
    queue: &MessageQueue,
    broker: Option<&ApprovalBroker>,
    notifier: Option<&super::notify::Notifier<'_>>,
    home: &Path,
    root_task_id: &str,
    expected_authority_revision: i64,
    actor: &str,
    counts_as_failure: bool,
    now: DateTime<Utc>,
) -> Result<StopStatus, ServiceError> {
    if actor.trim().is_empty() {
        return Err(ServiceError::new("invalid_actor", "actor is required"));
    }
    let outcome = store
        .stop_task_tree(
            root_task_id,
            expected_authority_revision,
            actor,
            counts_as_failure,
            now,
        )
        .await
        .map_err(|e| ServiceError::new("internal", e))?;
    let request = match outcome {
        StopTreeOutcome::Requested(r) => {
            post(
                store,
                activity::STOP_REQUESTED,
                actor,
                Some(root_task_id),
                format!(
                    "操作者要求停止本次執行（共 {} 個任務）；本輪已發出的委派不受影響",
                    r.affected_task_ids.len()
                ),
                now,
            )
            .await;
            revoke_grants(home, &r).await;
            r
        }
        StopTreeOutcome::AlreadyRequested(r) => r,
        StopTreeOutcome::Conflict { current_revision } => {
            return Err(ServiceError::new(
                "conflict",
                format!("task changed since it was read (current revision {current_revision:?})"),
            ));
        }
        StopTreeOutcome::AlreadyFinished { status } => {
            return Err(ServiceError::new(
                "already_finished",
                format!("task already {status}"),
            ));
        }
    };
    reconcile(store, queue, broker, notifier, &request, now).await
}

/// Dashboard `tasks.stop_status` backing (read + one reconciliation pass).
pub async fn stop_status(
    store: &TaskStore,
    queue: &MessageQueue,
    broker: Option<&ApprovalBroker>,
    notifier: Option<&super::notify::Notifier<'_>>,
    root_task_id: &str,
    now: DateTime<Utc>,
) -> Result<Option<StopStatus>, ServiceError> {
    let Some(request) = store
        .get_stop_request(root_task_id)
        .await
        .map_err(|e| ServiceError::new("internal", e))?
    else {
        return Ok(None);
    };
    reconcile(store, queue, broker, notifier, &request, now)
        .await
        .map(Some)
}

async fn revoke_grants(home: &Path, request: &StopRequestRow) {
    if !home.join("tasks.db").exists() {
        return;
    }
    match crate::capability_grants::CapabilityGrantStore::open(home) {
        Ok(grants) => {
            for id in &request.affected_task_ids {
                if let Err(e) = grants.revoke_for_task(id, "task_stopped").await {
                    tracing::warn!(task = %id, error = %e, "stop: capability grant revoke failed");
                }
            }
        }
        Err(e) => tracing::warn!(error = %e, "stop: capability grant store unavailable"),
    }
}

/// Read-only `tasks.stop_status` for a Viewer: the stored state, no
/// reconciliation side effects (S-L4).
pub async fn stop_status_stored(
    store: &TaskStore,
    root_task_id: &str,
) -> Result<Option<StopStatus>, ServiceError> {
    Ok(store
        .get_stop_request(root_task_id)
        .await
        .map_err(|e| ServiceError::new("internal", e))?
        .map(|r| stored_status(&r)))
}

fn stored_status(request: &StopRequestRow) -> StopStatus {
    let v = request
        .detail_json
        .as_deref()
        .and_then(|d| serde_json::from_str::<serde_json::Value>(d).ok())
        .unwrap_or_default();
    let n = |k: &str| v[k].as_u64().unwrap_or(0) as usize;
    let d = StopDetail {
        running_turns: n("running_turns"),
        messages_failed: n("messages_failed"),
        approvals_invalidated: n("approvals_invalidated"),
        operations_executing: n("operations_executing"),
        operations_uncertain: n("operations_uncertain"),
        team_rounds_running: n("team_rounds_running"),
        unverified_work_possible: v["unverified_work_possible"].as_bool().unwrap_or(false),
        unverified_hold_until: v["unverified_hold_until"].as_str().map(str::to_string),
        dispatch_in_flight: n("dispatch_in_flight"),
        claims_running: n("claims_running"),
        claims_lease_until: v["claims_lease_until"].as_str().map(str::to_string),
        claims_unconfirmed: n("claims_unconfirmed"),
        tree_still_open: n("tree_still_open"),
        tree_beyond_scan: v["tree_beyond_scan"].as_bool().unwrap_or(false),
        ..StopDetail::default()
    };
    StopStatus {
        root_task_id: request.root_task_id.clone(),
        state: request.state.clone(),
        affected_task_ids: request.affected_task_ids.clone(),
        detail: d,
    }
}

/// One reconciliation pass. A request already in a terminal state is
/// reported as stored and never reopened.
pub async fn reconcile(
    store: &TaskStore,
    queue: &MessageQueue,
    broker: Option<&ApprovalBroker>,
    notifier: Option<&super::notify::Notifier<'_>>,
    request: &StopRequestRow,
    now: DateTime<Utc>,
) -> Result<StopStatus, ServiceError> {
    if request.state != "cancel_pending" {
        return Ok(stored_status(request));
    }
    let mut d = StopDetail::default();
    let ids = tree_members(store, request, &mut d).await;
    batch_cancel(store, request, now, &mut d).await;
    queue_pass(queue, &ids, &mut d).await;
    for id in &ids {
        if super::team_activity::dispatching(id) {
            d.dispatch_in_flight += 1;
        }
    }
    claims_pass(store, queue, &ids, now, &mut d).await;
    team_signals(store, home_of(store), request, &ids, now, &mut d).await;
    approvals_pass(broker, &ids, &mut d).await;
    let state = decide_state(&d, now);
    finish(store, notifier, request, state, d, now).await
}

/// Tree ids (root first) up to the scan limit; the stored affected list is
/// the fallback when the tree cannot be read.
async fn tree_members(
    store: &TaskStore,
    request: &StopRequestRow,
    d: &mut StopDetail,
) -> Vec<String> {
    match store
        .stop_tree_ids(&request.root_task_id, STOP_TREE_SCAN_LIMIT + 1)
        .await
    {
        Ok(mut ids) => {
            if ids.len() > STOP_TREE_SCAN_LIMIT {
                ids.truncate(STOP_TREE_SCAN_LIMIT);
                d.tree_beyond_scan = true;
            }
            ids
        }
        Err(e) => {
            d.step_errors.push(format!("tree read: {e}"));
            // Unknown tree ⇒ the stop cannot be confirmed this pass.
            d.tree_still_open += 1;
            request.affected_task_ids.clone()
        }
    }
}

/// S-M2: cancel the next batch of still-open tree members.
async fn batch_cancel(
    store: &TaskStore,
    request: &StopRequestRow,
    now: DateTime<Utc>,
    d: &mut StopDetail,
) {
    match store
        .cancel_stop_tree_batch(&request.root_task_id, now)
        .await
    {
        Ok((cancelled, still_open)) => {
            d.tree_cancelled = cancelled;
            d.tree_still_open += still_open;
        }
        Err(e) => {
            d.tree_still_open += 1;
            d.step_errors.push(format!("tree cancel: {e}"));
        }
    }
}

/// Fail waiting messages of tree members and count held ones as running.
/// M-5: one indexed query over the open goal-loop and heartbeat messages
/// (M-2: heartbeat task-board wake-ups count too), matched against the tree
/// in memory — never one queue scan per member.
async fn queue_pass(queue: &MessageQueue, ids: &[String], d: &mut StopDetail) {
    let tree: std::collections::HashSet<&str> = ids.iter().map(String::as_str).collect();
    let messages = match queue.open_task_messages().await {
        Ok(m) => m,
        Err(e) => {
            // Unknown queue state ⇒ assume a turn may still be running.
            d.running_turns += 1;
            d.step_errors.push(format!("queue read: {e}"));
            return;
        }
    };
    for m in messages {
        let Some(task) = message_task_id(&m) else {
            continue;
        };
        if !tree.contains(task) {
            continue;
        }
        match m.status {
            MessageStatus::Pending => match queue.fail(&m.id, "stopped by operator").await {
                Ok(()) => d.messages_failed += 1,
                Err(e) => {
                    // A message we could not fail may still be dispatched.
                    d.running_turns += 1;
                    d.step_errors.push(format!("queue fail {}: {e}", m.id));
                }
            },
            MessageStatus::Acked | MessageStatus::Processing => d.running_turns += 1,
            MessageStatus::Done | MessageStatus::Failed => {}
        }
    }
}

/// The task a goal-loop or heartbeat message is about.
fn message_task_id(m: &crate::message_queue::QueueMessage) -> Option<&str> {
    match m.sender.as_str() {
        super::GOAL_LOOP_SENDER => m
            .id
            .strip_prefix("goal:")
            .and_then(|rest| rest.rsplit_once(':').map(|(task, _)| task))
            .or_else(|| {
                crate::dispatcher::extract_goal_loop_task_id_and_round(&m.payload).map(|(t, _)| t)
            }),
        super::HEARTBEAT_SENDER => crate::dispatcher::extract_heartbeat_task_id(&m.payload),
        _ => None,
    }
}

/// Whether a lapsed claim's task has a round on record that really ran to
/// an end (L-6: a `failed` message that was fenced never ran).
async fn finished_round_on_record(queue: &MessageQueue, task_id: &str) -> bool {
    queue
        .goal_messages_for_task(task_id)
        .await
        .map(|msgs| {
            msgs.iter().any(|m| match m.status {
                MessageStatus::Done => true,
                MessageStatus::Failed => !m
                    .error
                    .as_deref()
                    .unwrap_or("")
                    .starts_with(super::FENCE_ERROR_PREFIX),
                _ => false,
            })
        })
        .unwrap_or(false)
}

/// E-H3: claims and leases cover every dispatch path, not only goal rounds.
async fn claims_pass(
    store: &TaskStore,
    queue: &MessageQueue,
    ids: &[String],
    now: DateTime<Utc>,
    d: &mut StopDetail,
) {
    match store.claimed_in_tree(ids, now).await {
        Ok((running, lapsed)) => {
            d.claims_running = running.len();
            d.claims_lease_until = running.iter().map(|(_, l)| l.clone()).max();
            // M-5: per-member queue lookups are bounded; beyond the bound a
            // lapsed claim simply cannot be confirmed.
            for (i, id) in lapsed.iter().enumerate() {
                if i >= LAPSED_LOOKUP_LIMIT || !finished_round_on_record(queue, id).await {
                    d.claims_unconfirmed += 1;
                }
            }
        }
        Err(e) => {
            d.claims_running += 1;
            d.step_errors.push(format!("claims read: {e}"));
        }
    }
}

async fn approvals_pass(broker: Option<&ApprovalBroker>, ids: &[String], d: &mut StopDetail) {
    let Some(broker) = broker else {
        d.approval_store_unavailable = true;
        return;
    };
    match broker.task_bound_records(ids).await {
        Ok((pending, operations)) => {
            for id in pending {
                match broker.invalidate_request(&id, "task_stopped").await {
                    Ok(()) => d.approvals_invalidated += 1,
                    Err(e) => d.step_errors.push(format!("invalidate {id}: {e}")),
                }
            }
            for (_, state) in operations {
                match state {
                    OperationState::Prepared => d.operations_prepared += 1,
                    OperationState::Executing => d.operations_executing += 1,
                    OperationState::Uncertain => d.operations_uncertain += 1,
                    OperationState::Succeeded => d.operations_succeeded += 1,
                    OperationState::Failed => d.operations_failed += 1,
                }
            }
        }
        Err(e) => {
            d.approval_store_unavailable = true;
            d.step_errors.push(format!("approval store: {e}"));
        }
    }
}

/// Terminal only when nothing can still change.
fn decide_state(d: &StopDetail, now: DateTime<Utc>) -> &'static str {
    let holding = d.unverified_work_possible
        && d.unverified_hold_until
            .as_deref()
            .is_some_and(|h| h > resp_ts(now).as_str());
    if d.running_turns > 0
        || d.team_rounds_running > 0
        || d.dispatch_in_flight > 0
        || d.claims_running > 0
        || d.tree_still_open > 0
        || d.operations_executing > 0
        || d.approval_store_unavailable
        || holding
    {
        "cancel_pending"
    } else if d.operations_uncertain > 0
        || d.unverified_work_possible
        || d.claims_unconfirmed > 0
        || d.tree_beyond_scan
    {
        "stopped_uncertain"
    } else {
        "stopped"
    }
}

async fn finish(
    store: &TaskStore,
    notifier: Option<&super::notify::Notifier<'_>>,
    request: &StopRequestRow,
    state: &str,
    d: StopDetail,
    now: DateTime<Utc>,
) -> Result<StopStatus, ServiceError> {
    let detail_json = serde_json::to_string(&d).unwrap_or_else(|_| "{}".into());
    if state == "cancel_pending" {
        // Keep the latest detail visible while waiting.
        let _ = store
            .update_stop_request(&request.root_task_id, "cancel_pending", &detail_json, now)
            .await;
    } else if store
        .update_stop_request(&request.root_task_id, state, &detail_json, now)
        .await
        .map_err(|e| ServiceError::new("internal", e))?
    {
        post(
            store,
            activity::STOP_RECONCILED,
            &request.requested_by,
            Some(&request.root_task_id),
            match state {
                "stopped_uncertain" => {
                    "停止完成，但有工作或外部動作的結果無法確認，需要人工確認".to_string()
                }
                _ => "停止完成".to_string(),
            },
            now,
        )
        .await;
        if state == "stopped_uncertain" {
            if let (Some(n), Ok(Some((_, resp)))) = (
                notifier,
                store.occurrence_for_task(&request.root_task_id).await,
            ) {
                let event = super::notify::NoticeEvent::StopUncertain {
                    root_task_id: request.root_task_id.clone(),
                };
                n.notify(&resp, &event, now).await;
            }
        }
    }
    for e in &d.step_errors {
        post(
            store,
            activity::STOP_STEP_FAILED,
            &request.requested_by,
            Some(&request.root_task_id),
            format!(
                "停止對帳步驟失敗：{}",
                duduclaw_core::truncate_chars(e, 200)
            ),
            now,
        )
        .await;
    }
    Ok(StopStatus {
        root_task_id: request.root_task_id.clone(),
        state: state.to_string(),
        affected_task_ids: request.affected_task_ids.clone(),
        detail: d,
    })
}

/// Driver tick hook: reconcile every request still `cancel_pending`.
pub async fn reconcile_pending(
    store: &TaskStore,
    queue: &MessageQueue,
    broker: Option<&ApprovalBroker>,
    notifier: Option<&super::notify::Notifier<'_>>,
    now: DateTime<Utc>,
) -> usize {
    let Ok(requests) = store.pending_stop_requests().await else {
        return 0;
    };
    let mut done = 0;
    for r in requests {
        if let Ok(s) = reconcile(store, queue, broker, notifier, &r, now).await {
            done += (s.state != "cancel_pending") as usize;
        }
    }
    done
}

/// M-5: lapsed claims whose finished round is looked up per pass.
const LAPSED_LOOKUP_LIMIT: usize = 100;

/// How long an unobservable team round is assumed possibly alive when the
/// stopped tasks carry no lease of their own.
pub const UNVERIFIED_WORK_HOLD_SECS: i64 = 3600;

fn home_of(store: &TaskStore) -> Option<std::path::PathBuf> {
    store.db_path().parent().map(std::path::Path::to_path_buf)
}

async fn team_signals(
    store: &TaskStore,
    home: Option<std::path::PathBuf>,
    request: &StopRequestRow,
    ids: &[String],
    now: DateTime<Utc>,
    d: &mut StopDetail,
) {
    let on_disk = home
        .as_deref()
        .map(super::team_activity::role_member_tasks)
        .unwrap_or_default();
    let live = super::team_activity::registry_is_live();
    let mut rest: Vec<String> = Vec::new();
    for id in ids {
        if super::team_activity::registered(id) || on_disk.contains(id) {
            d.team_rounds_running += 1;
        } else {
            rest.push(id.clone());
        }
    }
    // M-5: one query for the remaining members.
    let (may_team, lease_until) = match store.team_and_lease_in(&rest).await {
        Ok((team, lease)) => (team, lease.as_deref().and_then(crate::task_store::parse_ts)),
        // Unknown task state ⇒ assume work may be running.
        Err(_) => (true, None),
    };
    if may_team && !live {
        d.unverified_work_possible = true;
        let base = crate::task_store::parse_ts(&request.requested_at).unwrap_or(now)
            + chrono::Duration::seconds(UNVERIFIED_WORK_HOLD_SECS);
        d.unverified_hold_until = Some(resp_ts(lease_until.map_or(base, |l| l.max(base))));
    }
}
