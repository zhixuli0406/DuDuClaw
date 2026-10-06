//! Agent-armed wake-ups (D4): while working on its own open occurrence an
//! employee may schedule one follow-up or ask the operator one question.
//! Split out of `service.rs` (file size); re-exported there.

use chrono::{DateTime, Duration, Utc};

use super::service::ServiceError;
use super::{AGENT_ARMED_LIMIT, DECISION_KIND, sha256_hex};
use crate::approval::ApprovalBroker;
use crate::task_store::{
    OccurrenceRow, ResponsibilityRow, TaskStore, WakeupRow, period_key, resp_ts,
};

fn internal(e: String) -> ServiceError {
    ServiceError::new("internal", e)
}

/// The open occurrence an employee is working on, required for agent-armed
/// wake-ups (D4). Refuses a caller that is not the responsibility's owner.
async fn agent_occurrence(
    store: &TaskStore,
    id: &str,
    caller: &str,
) -> Result<(ResponsibilityRow, OccurrenceRow), ServiceError> {
    let resp = store
        .get_responsibility(id)
        .await
        .map_err(internal)?
        .filter(|r| r.owner_agent_id == caller)
        .ok_or_else(|| ServiceError::new("not_found", "responsibility not found"))?;
    if resp.state != "active" {
        return Err(ServiceError::new(
            "not_active",
            "responsibility is not active",
        ));
    }
    let open = store
        .list_occurrences(id)
        .await
        .map_err(internal)?
        .into_iter()
        .find(|o| o.outcome.is_none())
        .ok_or_else(|| {
            ServiceError::new(
                "no_open_occurrence",
                "only callable while working on this responsibility",
            )
        })?;
    Ok((resp, open))
}

/// Refuse when the window `at` falls in already used its occurrence quota
/// (B.1 D4: an employee's own follow-up counts against the window limit).
async fn check_window_quota(
    store: &TaskStore,
    resp: &ResponsibilityRow,
    at: DateTime<Utc>,
) -> Result<(), ServiceError> {
    let window = period_key(&resp.budget_period, &resp.budget_timezone, at).map_err(internal)?;
    let used = store
        .period_usage(&resp.responsibility_id, &window)
        .await
        .map_err(internal)?;
    if used.occurrences >= resp.period_occurrence_limit {
        return Err(ServiceError::new(
            "period_occurrence_limit",
            format!("window {window} has no occurrence left"),
        ));
    }
    Ok(())
}

/// MCP `responsibility_followup` backing: a one-shot time wake-up armed by the
/// owning employee during its open occurrence. Never silently rescheduled.
pub async fn followup(
    store: &TaskStore,
    id: &str,
    caller: &str,
    due_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<WakeupRow, ServiceError> {
    let (resp, _occ) = agent_occurrence(store, id, caller).await?;
    let earliest = now + Duration::seconds(resp.min_wake_interval_secs);
    let stop_at = crate::task_store::parse_ts(&resp.stop_at).unwrap_or(now);
    if due_at < earliest || due_at > stop_at {
        return Err(ServiceError::new(
            "invalid_due_at",
            "due_at must be ≥ now + min_wake_interval and ≤ stop_at",
        ));
    }
    check_window_quota(store, &resp, due_at).await?;
    let ts = resp_ts(now);
    let w = WakeupRow {
        wakeup_id: uuid::Uuid::new_v4().to_string(),
        responsibility_id: id.to_string(),
        control_epoch: resp.control_epoch,
        kind: "time".into(),
        recurring: false,
        due_at: Some(resp_ts(due_at)),
        event_name: None,
        event_filter_json: None,
        approval_id: None,
        armed_by: format!("agent:{caller}"),
        state: "armed".into(),
        created_at: ts.clone(),
        updated_at: ts,
        armed_after_event_id: None,
    };
    store
        .arm_wakeup(&w, Some(AGENT_ARMED_LIMIT))
        .await
        .map_err(|e| {
            ServiceError::new(
                if e == "agent_followup_limit" {
                    "agent_followup_limit"
                } else if e == "epoch_changed" {
                    "epoch_changed"
                } else {
                    "internal"
                },
                e,
            )
        })?;
    Ok(w)
}

/// MCP `responsibility_ask` / dashboard `responsibilities.ask` backing: an
/// unbound `responsibility_decision` request plus a decision subscription.
/// The answer is DATA for the next occurrence; it grants nothing.
pub async fn ask(
    store: &TaskStore,
    broker: &ApprovalBroker,
    id: &str,
    caller: &str,
    question: &str,
    options: &[String],
    ttl_secs: i64,
    notifier: Option<&super::notify::Notifier<'_>>,
    now: DateTime<Utc>,
) -> Result<WakeupRow, ServiceError> {
    let question = question.trim();
    if question.is_empty()
        || question.chars().count() > 1000
        || options.len() > 5
        || options
            .iter()
            .any(|o| o.trim().is_empty() || o.chars().count() > 1000)
    {
        return Err(ServiceError::new(
            "invalid_question",
            "question ≤1000 characters, ≤5 non-empty options",
        ));
    }
    let (resp, occ) = agent_occurrence(store, id, caller).await?;
    check_window_quota(store, &resp, now).await?;
    let stop_at = crate::task_store::parse_ts(&resp.stop_at).unwrap_or(now);
    let ttl = ttl_secs.clamp(60, (stop_at - now).num_seconds().max(60));
    // S-M7: the question is the employee's own text. It is scanned, kept as
    // quoted DATA on a server-built card, and the push is the responsibility
    // notice (policy, proactive gate, per-window cap) sent by the wake pass —
    // the broker never pushes this kind itself.
    let scan = duduclaw_security::input_guard::scan_input(question, 60);
    let payload = serde_json::json!({
        "responsibility_id": id,
        "control_epoch": resp.control_epoch,
        "occurrence_task_id": occ.task_id,
        // S-L5: `task_id` binds the request to the run, so stopping that
        // run also withdraws the question.
        "task_id": occ.task_id,
        "question_hash": sha256_hex(question),
        "question": question,
        "options": options,
        "guard_flags": scan.matched_rules,
        "guard_blocked": scan.blocked,
    });
    let one_line: String = question.split_whitespace().collect::<Vec<_>>().join(" ");
    let summary = format!(
        "持續任務需要你決定（AI 員工 {} 撰寫的問題，僅供參考）：「{}」{}",
        resp.owner_agent_id,
        duduclaw_core::truncate_chars(&one_line, 200),
        if scan.blocked || !scan.matched_rules.is_empty() {
            "\n注意：問題內容含疑似指令的文字，請小心判讀。"
        } else {
            ""
        }
    );
    let approval = broker
        .request(&resp.owner_agent_id, DECISION_KIND, &summary, payload, ttl)
        .await
        .map_err(internal)?;
    let ts = resp_ts(now);
    let w = WakeupRow {
        wakeup_id: uuid::Uuid::new_v4().to_string(),
        responsibility_id: id.to_string(),
        control_epoch: resp.control_epoch,
        kind: "decision".into(),
        recurring: false,
        due_at: None,
        event_name: None,
        event_filter_json: None,
        approval_id: Some(approval.as_str().to_string()),
        armed_by: format!("agent:{caller}"),
        state: "armed".into(),
        created_at: ts.clone(),
        updated_at: ts,
        armed_after_event_id: None,
    };
    if let Err(e) = store.arm_wakeup(&w, Some(AGENT_ARMED_LIMIT)).await {
        let _ = broker
            .invalidate_request(&approval, "subscription_refused")
            .await;
        return Err(ServiceError::new(
            if e == "agent_followup_limit" {
                "agent_followup_limit"
            } else {
                "internal"
            },
            e,
        ));
    }
    // The push (if the policy allows one) is sent by the next wake pass,
    // keyed to this subscription, so it goes out once on every path.
    let _ = notifier;
    Ok(w)
}
