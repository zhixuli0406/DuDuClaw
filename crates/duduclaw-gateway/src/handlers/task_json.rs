//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

// ── JSON serialization helpers ──────────────────────────────

pub(crate) fn task_row_to_json(r: &TaskRow) -> Value {
    json!({
        "id": r.id,
        // Canonical `TaskKind` spelling; the dashboard locks discovery rows
        // read-only and keeps them off the goal board based on this field.
        "kind": r.kind.as_str(),
        "title": r.title,
        "description": r.description,
        "status": r.status,
        "priority": r.priority,
        "assigned_to": r.assigned_to,
        "created_by": r.created_by,
        "created_at": r.created_at,
        "updated_at": r.updated_at,
        "completed_at": r.completed_at,
        "blocked_reason": r.blocked_reason,
        "judge_feedback": r.judge_feedback,
        "parent_task_id": r.parent_task_id,
        "tags": r.tags.split(',').filter(|s| !s.is_empty()).collect::<Vec<_>>(),
        "message_id": r.message_id,
        // Iterative Kanban (v1.45): revision-round cache + agent clock + lease.
        "revision_round": r.revision_round,
        "diminishing": r.diminishing,
        "agent_seconds": r.agent_seconds,
        "lease_expires_at": r.lease_expires_at,
        // Goal-loop surface (2026-08-14 /goals page): previously none of
        // these reached the dashboard — the FE had to *guess* goal-ness from
        // revision_round/status heuristics, and the acceptance contract was
        // invisible.
        "goal_mode": r.goal_mode,
        "acceptance_criteria": r.acceptance_criteria,
        // H9-G goal contract freeze: the immutable snapshot taken at goal
        // creation, when one exists (see `TaskRow::acceptance_criteria_baseline`).
        "acceptance_criteria_baseline": r.acceptance_criteria_baseline,
        // WP-G2: never show the raw `<criteria_status>` tag (the settle
        // rewrites the stored copy; this covers the window before it).
        "result_summary": crate::goal_loop::criteria_ledger::display_result_summary(
            r.criteria_ledger.as_deref(),
            r.result_summary.as_deref(),
        ),
        "retry_count": r.retry_count,
        "max_retries": r.max_retries,
        "claimed_by": r.claimed_by,
        "goal_state": r
            .goal_state_json
            .as_ref()
            .and_then(|s| serde_json::from_str::<Value>(s).ok()),
        // Goal assignment form v2 (design-market-belief-loop-2026-08.md §6,
        // G1): per-goal deadline + risk boundary, when explicitly set. `null`
        // ⇒ the global wall clock / deployment baseline boundary applies.
        "deadline_at": r.deadline_at,
        "risk_boundary": r.risk_boundary,
        // H11 pause-reason classification. Always a RESOLVED token, never the
        // raw column: a legacy / unrecognised row must reach the dashboard as
        // `unknown` = 「需要人工確認」 (a real chip) rather than as a missing
        // field the UI would have to guess about. Scoped to `needs_human` so a
        // class can never linger on a task that is no longer paused — the
        // store clears it on `resolve_needs_human`, but a direct `tasks.update`
        // status write bypasses that, and a stale chip is worse than none.
        "pause_reason": (r.status == "needs_human").then(|| {
            crate::pause_reason::PauseReason::from_stored(r.pause_reason.as_deref()).as_str()
        }),
        // I-1c "想一想": a generated plan awaiting approval. Scoped to
        // `needs_human` the same defensive way as `pause_reason` above — the
        // store already clears it after the first post-approval dispatch, but
        // a stale value must never render on a task that is no longer paused.
        "plan_pending": (r.status == "needs_human").then(|| r.plan_pending.clone()).flatten(),
        // I-3b: archive/pin flags for the `/goals` board (task_store.rs).
        "archived": r.archived,
        "pinned": r.pinned,
    })
}

pub(crate) fn task_iteration_to_json(r: &TaskIterationRow) -> Value {
    json!({
        "round": r.round,
        "dispatched_at": r.dispatched_at,
        "submitted_at": r.submitted_at,
        "judged_at": r.judged_at,
        "verdict": r.verdict,
        "judge_feedback": r.judge_feedback,
        "feedback_class": r.feedback_class,
        // Per-aspect MAV panel results (`[{name, pass, reason}]`) — null for
        // deterministic rejections and legacy rows.
        "aspects": r
            .verdict_json
            .as_ref()
            .and_then(|s| serde_json::from_str::<Value>(s).ok()),
        "dispatch_count": r.dispatch_count,
        "repeat_streak": r.repeat_streak,
        // A1 ledger (2026-09-30): additive fields; null on older rows. The
        // larger JSON blobs (gate inputs, state block, knobs) stay in the DB
        // for offline analysis and are not shipped to the timeline.
        "evaluator_verdict": r.evaluator_verdict,
        "iter_seq": r.iter_seq,
        "team_mode": r.team_mode,
        "pause_reason": r.pause_reason,
    })
}

pub(crate) fn activity_row_to_json(r: &ActivityRow) -> Value {
    json!({
        "id": r.id,
        "type": r.event_type,
        "agent_id": r.agent_id,
        "task_id": r.task_id,
        "summary": r.summary,
        "timestamp": r.timestamp,
        "metadata": r.metadata.as_ref().and_then(|s| serde_json::from_str::<Value>(s).ok()),
    })
}

pub(crate) fn comment_row_to_json(r: &CommentRow) -> Value {
    json!({
        "id": r.id,
        "task_id": r.task_id,
        "author_user": r.author_user,
        "body": r.body,
        "created_at": r.created_at,
    })
}

// ── U4 co-edited plan JSON shapes ───────────────────────────

pub(crate) fn plan_row_to_json(r: &PlanRow) -> Value {
    json!({
        "id": r.id,
        "title": r.title,
        "description": r.description,
        "agent_id": r.agent_id,
        "goal_id": r.goal_id,
        "status": r.status,
        "created_by": r.created_by,
        "created_at": r.created_at,
        "updated_at": r.updated_at,
    })
}

pub(crate) fn plan_step_row_to_json(r: &PlanStepRow) -> Value {
    json!({
        "id": r.id,
        "plan_id": r.plan_id,
        "text": r.text,
        "assignee_kind": r.assignee_kind,
        "assignee": r.assignee,
        "status": r.status,
        "step_order": r.step_order,
        "created_at": r.created_at,
        "updated_at": r.updated_at,
    })
}

pub(crate) fn autopilot_rule_to_json(r: &AutopilotRuleRow) -> Value {
    json!({
        "id": r.id,
        "name": r.name,
        "enabled": r.enabled,
        "trigger_event": r.trigger_event,
        "conditions": serde_json::from_str::<Value>(&r.conditions).unwrap_or(json!({})),
        "action": serde_json::from_str::<Value>(&r.action).unwrap_or(json!({})),
        "created_at": r.created_at,
        "last_triggered_at": r.last_triggered_at,
        "trigger_count": r.trigger_count,
        // P3-3: present only for CEP sequence rules; null for ordinary rules.
        "sequence": r.sequence.as_ref().and_then(|s| serde_json::from_str::<Value>(s).ok()),
        // P4-1: induced-rule provenance ({induced, induced_at, fingerprint, source});
        // null for hand-authored rules. Lets the dashboard badge PBD-induced rules.
        "metadata": r.metadata.as_ref().and_then(|s| serde_json::from_str::<Value>(s).ok()),
    })
}

/// Trigger events a NEW rule may subscribe to — every name the engine
/// actually emits (`AutopilotEvent::event_name`). The dashboard mirrors this
/// list by hand in `web/src/lib/autopilot-rules.ts` (`SERVER_TRIGGER_EVENTS`);
/// `autopilot_validation_tests` pins the two together.
pub(crate) const AUTOPILOT_CREATE_TRIGGER_EVENTS: &[&str] = &[
    "task_created",
    "task_updated",
    "task_status_changed",
    "activity_new",
    "channel_message",
    "agent_idle",
    // Foresight signal (see `autopilot_engine::Event::RunAtRisk`) — was
    // missing here, so rules could never subscribe to it (2026-07 MED).
    "run_at_risk",
    // OS-native perception events (`autopilot_engine::AutopilotEvent::OsFileEvent`
    // / `OsFrontmostEvent`) — same class of gap as `run_at_risk` above: the
    // engine has fired these since P1/P2-4 but a dashboard-authored rule
    // could never subscribe to either (2026-07-23 P3-4 audit follow-up).
    "os_file",
    "os_frontmost",
    // Resident sensing (WP2) — `autopilot_engine::AutopilotEvent::Tick`.
    // Without this entry a dashboard-authored rule could never subscribe
    // to a configured `[[tick.sources]]` feed at all.
    "tick",
    // OS security line P0 (C1) — `autopilot_engine::AutopilotEvent::SecurityEvent`.
    // Same gap class as `run_at_risk`/`os_file` above: without this entry
    // a dashboard-authored rule (or `rule_induction`'s
    // `enable_induced_rule`) could never subscribe to a security event
    // even though the engine has fired them since this change.
    "security_event",
    // G4 (2026-09 feature audit) — `autopilot_engine::AutopilotEvent::OdooEvent`.
    // The Odoo bridge (poller + `/webhook/odoo`) had no bus variant at all
    // before this; without the entry here a dashboard-authored rule could
    // not subscribe to an ERP change.
    "odoo_event",
];

/// Trigger events still accepted on stored rules (update / re-validation) but
/// refused on create. `cron_tick` has an enum variant but no emitter anywhere —
/// a rule on it never fires; scheduled work belongs to the cron scheduler.
pub(crate) const AUTOPILOT_LEGACY_TRIGGER_EVENTS: &[&str] = &["cron_tick"];

/// Validate a trigger_event string against the set understood by AutopilotEngine.
/// Rejecting unknown values at write time avoids rules that are stored
/// successfully but can never fire.
///
/// Accepts the legacy names in [`AUTOPILOT_LEGACY_TRIGGER_EVENTS`] so updating
/// an already-stored rule keeps working; new rules go through
/// [`validate_autopilot_trigger_event_for_create`].
pub(crate) fn validate_autopilot_trigger_event(ev: &str) -> Result<(), String> {
    if AUTOPILOT_CREATE_TRIGGER_EVENTS.iter().any(|k| *k == ev)
        || AUTOPILOT_LEGACY_TRIGGER_EVENTS.iter().any(|k| *k == ev)
    {
        Ok(())
    } else {
        Err(format!(
            "unknown trigger_event '{ev}'; must be one of: {}",
            AUTOPILOT_CREATE_TRIGGER_EVENTS.join(", ")
        ))
    }
}

/// [`validate_autopilot_trigger_event`] for a rule being created: the legacy
/// `cron_tick` is refused because nothing emits it.
pub(crate) fn validate_autopilot_trigger_event_for_create(ev: &str) -> Result<(), String> {
    if AUTOPILOT_LEGACY_TRIGGER_EVENTS.iter().any(|k| *k == ev) {
        return Err(format!(
            "trigger_event '{ev}' is never emitted, so a rule on it would never fire; \
             create a scheduled task instead (tasks_create with schedule / the scheduled tasks page)"
        ));
    }
    validate_autopilot_trigger_event(ev)
}

/// [`validate_autopilot_trigger_event`] for `autopilot.update`: a legacy
/// trigger (`cron_tick`) is accepted only when the stored rule already has it
/// (an unchanged value round-tripped by the editor); changing any other rule
/// TO it is refused with the create path's message, since it would never fire.
pub(crate) fn validate_autopilot_trigger_event_for_update(
    new_ev: &str,
    stored_ev: &str,
) -> Result<(), String> {
    if AUTOPILOT_LEGACY_TRIGGER_EVENTS.iter().any(|k| *k == new_ev) && new_ev != stored_ev {
        return validate_autopilot_trigger_event_for_create(new_ev);
    }
    validate_autopilot_trigger_event(new_ev)
}

/// Validate a rule's `conditions` tree at write time.
///
/// "No conditions" — `null`, `{}`, `{"all": []}` — is legal and means the rule
/// fires on every event of its trigger (see `autopilot_engine::evaluate`).
/// `{"any": []}` is legal but never matches. Everything else must be an
/// `all`/`any` group whose value is an array of valid conditions, or a leaf
/// with a non-empty string `field` and (optionally, default `eq`) a known `op`.
/// A malformed leaf would evaluate false forever, so it is refused here.
pub(crate) fn validate_autopilot_conditions(conditions: &Value) -> Result<(), String> {
    validate_condition_node(conditions, "conditions", 0)
}

/// Nesting cap for condition groups — far beyond any hand-authored rule.
const MAX_CONDITION_DEPTH: usize = 16;

fn validate_condition_node(node: &Value, path: &str, depth: usize) -> Result<(), String> {
    if depth > MAX_CONDITION_DEPTH {
        return Err(format!("{path}: conditions nested deeper than {MAX_CONDITION_DEPTH} levels"));
    }
    if node.is_null() {
        return Ok(());
    }
    let obj = node
        .as_object()
        .ok_or_else(|| format!("{path} must be an object (a condition or an all/any group)"))?;
    if obj.is_empty() {
        return Ok(());
    }
    for group in ["all", "any"] {
        if let Some(v) = obj.get(group) {
            let items = v
                .as_array()
                .ok_or_else(|| format!("{path}.{group} must be an array of conditions"))?;
            for (i, item) in items.iter().enumerate() {
                validate_condition_node(item, &format!("{path}.{group}[{i}]"), depth + 1)?;
            }
            return Ok(());
        }
    }
    match obj.get("field") {
        Some(Value::String(f)) if !f.trim().is_empty() => {}
        _ => {
            return Err(format!(
                "{path}: a condition needs a non-empty \"field\" (or use an \"all\"/\"any\" group)"
            ));
        }
    }
    match obj.get("op") {
        None => {}
        Some(Value::String(op))
            if crate::autopilot_engine::CONDITION_OPS.iter().any(|k| *k == op.as_str()) => {}
        Some(other) => {
            return Err(format!(
                "{path}: unknown op {other}; must be one of: {}",
                crate::autopilot_engine::CONDITION_OPS.join(", ")
            ));
        }
    }
    Ok(())
}

/// Validate an autopilot action JSON object at rule-write time.
///
/// Requires `type` ∈ {delegate, notify, run_skill} and the fields the
/// engine will eventually need. Catches misconfiguration immediately
/// rather than silently during the first fire.
pub(crate) fn validate_autopilot_action(action: &Value) -> Result<(), String> {
    let obj = action
        .as_object()
        .ok_or_else(|| "action must be a JSON object".to_string())?;
    let t = obj
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "action.type is required".to_string())?;
    let require_str = |key: &str| -> Result<(), String> {
        obj.get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|_| ())
            .ok_or_else(|| format!("action.{key} is required for type '{t}'"))
    };
    match t {
        "delegate" => {
            require_str("target_agent")?;
            require_str("prompt")?;
        }
        "notify" => {
            require_str("channel")?;
            require_str("chat_id")?;
            require_str("text")?;
        }
        "run_skill" => {
            require_str("target_agent")?;
            require_str("skill_name")?;
        }
        // P2-2 proactive path: same shape as `notify` (the underlying action the
        // gate performs on Allow), but routed through the ProactiveGate first.
        "proactive_notify" => {
            require_str("channel")?;
            require_str("chat_id")?;
            require_str("text")?;
        }
        other => return Err(format!("unknown action.type '{other}'")),
    }

    // Resident sensing WP3: the optional local-model screening layer. Same
    // write-time contract as everything above — a typo'd `mode`, an
    // over-long prompt, or an out-of-range `timeout_secs` is refused here
    // rather than surfacing as a degraded fire much later.
    if let Some(screen) = obj.get("screen") {
        if !screen.is_null() {
            crate::autopilot_screen::validate_screen_spec(screen)?;
        }
    }
    Ok(())
}

/// WP9: resolve a Telegram bot's `@username` from its token via getMe.
/// Returns `None` on any network/parse error or a non-ok Telegram response —
/// the caller then fails closed (no deep-link minted).
///
/// `pub(crate)`: also reused by `channel_link::cached_telegram_bot_username`
/// (E8 reverse handoff) — same "resolve live from the configured token"
/// contract, just cached with a TTL there instead of called fresh per bind.
pub(crate) async fn fetch_telegram_bot_username(token: &str) -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .ok()?;
    let url = format!("https://api.telegram.org/bot{token}/getMe");
    let resp = client.get(&url).send().await.ok()?;
    let data: serde_json::Value = resp.json().await.ok()?;
    if data.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        return None;
    }
    data.get("result")
        .and_then(|r| r.get("username"))
        .and_then(|u| u.as_str())
        .map(|s| s.to_string())
}

pub(crate) fn extract_frontmatter(content: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    for line in content.lines() {
        if line == "---" {
            // End of frontmatter
        }
        if let Some(rest) = line.strip_prefix(&prefix) {
            return Some(rest.trim().to_string());
        }
    }
    None
}

/// Update a field in YAML-style frontmatter with a transform function.
pub(crate) fn update_frontmatter_field(
    content: &str,
    key: &str,
    transform: impl Fn(&str) -> String,
) -> String {
    let prefix = format!("{key}:");
    content
        .lines()
        .map(|line| {
            if let Some(rest) = line.strip_prefix(&prefix) {
                format!("{prefix} {}", transform(rest.trim()))
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Map a `tasks.update` status transition OUT of `needs_human` to the
/// settled-decision verb a collapsed channel card should show. Only the
/// three legal goal-loop outcomes (04-orca-object-model-cta-matrix.md §2.1:
/// retry → `pending`, done → `done`, abort → `cancelled`) count as a settled
/// decision; anything else is `None` — fail-closed, never guessed (coding
/// convention: routing/security-adjacent decisions must not fall through to
/// a default).
pub(crate) fn goal_task_settle_verb(new_status: &str) -> Option<crate::decision_card::DecisionVerb> {
    use crate::decision_card::DecisionVerb;
    match new_status {
        "pending" => Some(DecisionVerb::Retried),
        "done" => Some(DecisionVerb::MarkedDone),
        "cancelled" => Some(DecisionVerb::Abandoned),
        _ => None,
    }
}

#[cfg(test)]
mod criteria_tag_tests {
    use super::*;

    /// WP-G2: the dashboard's 「最新產出摘要」 never shows the raw
    /// `<criteria_status>` tag of a ledger goal; other tasks are untouched.
    #[test]
    fn task_row_to_json_strips_the_criteria_tag_only_for_ledger_goals() {
        let mut r = TaskRow::new("t".into(), "g".into(), String::new(), "medium".into(), "a".into(), "s".into());
        let tagged = "已完成\n<criteria_status>[{\"id\":\"C1\"}]</criteria_status>";
        r.result_summary = Some(tagged.into());
        assert_eq!(task_row_to_json(&r)["result_summary"], tagged);
        r.criteria_ledger = Some("{}".into());
        assert_eq!(task_row_to_json(&r)["result_summary"], "已完成");
        r.result_summary = None;
        assert!(task_row_to_json(&r)["result_summary"].is_null());
    }
}
