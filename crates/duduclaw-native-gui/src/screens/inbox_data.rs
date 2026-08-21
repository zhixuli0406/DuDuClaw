// Data model + pure parsing/filtering for the "收件匣" page — split out of
// `inbox.rs` purely to keep that file under this crate's own <800-line
// convention (WP-NG-debt, 2026-08-21). No behavior differs from an unsplit
// version: `FeedCategory`/`FilterKind`/`FeedBadge`/`ApprovalExpand`/
// `FeedItem` and their build/filter/classify/params functions have no
// dependency on `InboxState`, fetch orchestration, or rendering — a clean,
// self-contained layer `inbox.rs` and `inbox_rows.rs` both import from, same
// shape `goals_data.rs` already established for `goals.rs`/
// `goals_inspector.rs` (see that file's own doc comment for the precedent).
// See `inbox.rs`'s own module doc comment for the page's full design
// (visual authority, RPC shapes, the honest 提及/System-vs-Notification
// scope cuts) — unchanged by this split, just relocated.

use serde_json::{json, Value};

// ── Model ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedCategory {
    Approval,
    Notification,
    Mention,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterKind {
    All,
    Approval,
    Notification,
    Mention,
    System,
}

impl FilterKind {
    /// The 5 chips, in the canvas's own left-to-right order.
    pub const ALL: [FilterKind; 5] = [
        FilterKind::All,
        FilterKind::Approval,
        FilterKind::Notification,
        FilterKind::Mention,
        FilterKind::System,
    ];

    fn matches(self, category: FeedCategory) -> bool {
        match self {
            FilterKind::All => true,
            FilterKind::Approval => category == FeedCategory::Approval,
            FilterKind::Notification => category == FeedCategory::Notification,
            FilterKind::Mention => category == FeedCategory::Mention,
            FilterKind::System => category == FeedCategory::System,
        }
    }

    pub fn label_key(self) -> &'static str {
        match self {
            FilterKind::All => "native.inbox.filter.all",
            FilterKind::Approval => "native.inbox.filter.approval",
            FilterKind::Notification => "native.inbox.filter.notification",
            FilterKind::Mention => "native.inbox.filter.mention",
            FilterKind::System => "native.inbox.filter.system",
        }
    }

    pub fn empty_key(self) -> &'static str {
        match self {
            FilterKind::All => "native.inbox.empty.all",
            FilterKind::Approval => "native.inbox.empty.approval",
            FilterKind::Notification => "native.inbox.empty.notification",
            FilterKind::Mention => "native.inbox.empty.mention",
            FilterKind::System => "native.inbox.empty.system",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedBadge {
    /// An approval, always pending (`approvals.list` only ever returns
    /// pending rows — see `handle_approvals_list`).
    Pending,
    /// `activity.list` row whose `type == "task_completed"`.
    Done,
}

/// Only populated for [`FeedCategory::Approval`] rows — the fields the
/// expanded card needs that a plain notification row never carries.
#[derive(Debug, Clone)]
pub struct ApprovalExpand {
    pub approval_id: String,
    pub kind: String,
    /// D1 ActionGuard forward-simulation narrative (`approval.rs`'s
    /// `SimulationNarrative`), parsed straight from the RPC's raw
    /// `simulation` JSON — `None` for the overwhelming majority of
    /// approvals that never ran that judge (see `handle_approvals_list`'s
    /// own comment on this field). The mono "will execute" block in the
    /// expanded card is omitted entirely when this is `None`, never
    /// fabricated.
    pub world_state_change: Option<String>,
    pub risk_points: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct FeedItem {
    /// Stable across a single fetch cycle, namespaced by source
    /// (`"approval:<id>"` / `"activity:<id>"`) — the two id spaces are NOT
    /// guaranteed disjoint on their own (both are opaque server ids), so an
    /// unprefixed id could theoretically collide between an approval and an
    /// activity row.
    pub id: String,
    pub category: FeedCategory,
    pub agent_id: String,
    pub title: String,
    /// Raw RFC3339 (or empty) — formatted on demand by `inbox_rows.rs`, same
    /// "store raw, parse at render" convention `conversation_row.rs` already
    /// established for this crate's other timestamp field.
    pub timestamp: String,
    pub badge: Option<FeedBadge>,
    pub approval: Option<ApprovalExpand>,
}

/// Explicit prefix table — see `inbox.rs`'s header comment ("System vs.
/// Notification") for why `agent_id` can't be the signal instead.
const SYSTEM_EVENT_PREFIXES: &[&str] = &[
    "channel_",
    "autopilot",
    "gvu_",
    "goal_loop.",
    "os_watch_",
    "playbook_rule_retired",
    "approval.channel_decision",
];

fn classify_activity_event(event_type: &str) -> FeedCategory {
    if SYSTEM_EVENT_PREFIXES.iter().any(|p| event_type.starts_with(p)) {
        FeedCategory::System
    } else {
        FeedCategory::Notification
    }
}

/// Pure merge: `approvals.list`'s `approvals` array + `activity.list`'s
/// `events` array → one ordered feed, approvals first (task brief: "審批
/// pending 置頂"). Field names read directly from `handlers.rs::
/// handle_approvals_list` / `activity_row_to_json` — see this crate's S4b
/// report for the exact line numbers, not guessed.
pub fn build_feed(approvals: &[Value], activities: &[Value]) -> Vec<FeedItem> {
    let mut items = Vec::with_capacity(approvals.len() + activities.len());

    for a in approvals {
        let Some(id) = a.get("id").and_then(Value::as_str) else { continue };
        let agent_id = a.get("agent_id").and_then(Value::as_str).unwrap_or("").to_string();
        let title = a.get("summary").and_then(Value::as_str).unwrap_or("").to_string();
        let kind = a.get("kind").and_then(Value::as_str).unwrap_or("other").to_string();
        let timestamp = a.get("created_at").and_then(Value::as_str).unwrap_or("").to_string();

        let sim = a.get("simulation").filter(|v| !v.is_null());
        let world_state_change = sim
            .and_then(|s| s.get("world_state_change"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let risk_points: Vec<String> = sim
            .and_then(|s| s.get("risk_points"))
            .and_then(Value::as_array)
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();

        items.push(FeedItem {
            id: format!("approval:{id}"),
            category: FeedCategory::Approval,
            agent_id,
            title,
            timestamp,
            badge: Some(FeedBadge::Pending),
            approval: Some(ApprovalExpand {
                approval_id: id.to_string(),
                kind,
                world_state_change,
                risk_points,
            }),
        });
    }

    for e in activities {
        let Some(id) = e.get("id").and_then(Value::as_str) else { continue };
        let event_type = e.get("type").and_then(Value::as_str).unwrap_or("");
        let agent_id = e.get("agent_id").and_then(Value::as_str).unwrap_or("").to_string();
        let title = e.get("summary").and_then(Value::as_str).unwrap_or("").to_string();
        let timestamp = e.get("timestamp").and_then(Value::as_str).unwrap_or("").to_string();
        items.push(FeedItem {
            id: format!("activity:{id}"),
            category: classify_activity_event(event_type),
            agent_id,
            title,
            timestamp,
            badge: (event_type == "task_completed").then_some(FeedBadge::Done),
            approval: None,
        });
    }

    items
}

pub fn filter_feed(items: &[FeedItem], filter: FilterKind) -> Vec<&FeedItem> {
    items.iter().filter(|it| filter.matches(it.category)).collect()
}

/// `approvals.decide` params — pure, unit-tested. The task brief is explicit
/// that a live approve/reject click must never actually happen during this
/// pass's own verification (it would mutate real approval state on
/// whichever gateway is running) — this function is the substitute: it pins
/// the exact shape the real button's `on_click` sends, verified against
/// `handle_approvals_decide`'s param reads (`id` / `approve` bool / `reason`
/// optional str) instead of guessed.
pub fn build_decide_params(approval_id: &str, approve: bool, note: &str) -> Value {
    json!({ "id": approval_id, "approve": approve, "reason": note })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approval(id: &str, agent: &str, summary: &str, kind: &str, created: &str, sim: Value) -> Value {
        json!({
            "id": id, "agent_id": agent, "kind": kind, "summary": summary,
            "payload": {}, "created_at": created, "ttl_seconds": 3600,
            "expires_at": 0, "simulation": sim, "channel": null, "channel_link": null,
        })
    }

    fn activity(id: &str, event_type: &str, agent: &str, summary: &str, ts: &str) -> Value {
        json!({ "id": id, "type": event_type, "agent_id": agent, "task_id": null, "summary": summary, "timestamp": ts, "metadata": null })
    }

    #[test]
    fn build_feed_puts_all_approvals_before_all_activity() {
        let approvals = vec![approval("a1", "finance", "s1", "tool_call", "2026-08-21T10:00:00Z", Value::Null)];
        let activities = vec![activity("e1", "task_completed", "duke", "done", "2026-08-21T09:00:00Z")];
        let items = build_feed(&approvals, &activities);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].category, FeedCategory::Approval);
        assert_eq!(items[1].category, FeedCategory::Notification);
    }

    #[test]
    fn approval_row_carries_the_simulation_narrative_when_present() {
        let sim = json!({ "world_state_change": "  會付款 NT$2,180  ", "risk_points": ["不可逆"] });
        let approvals = vec![approval("a1", "finance", "s1", "tool_call", "2026-08-21T10:00:00Z", sim)];
        let items = build_feed(&approvals, &[]);
        let exp = items[0].approval.as_ref().expect("approval row must carry ApprovalExpand");
        assert_eq!(exp.world_state_change.as_deref(), Some("會付款 NT$2,180")); // trimmed
        assert_eq!(exp.risk_points, vec!["不可逆".to_string()]);
    }

    #[test]
    fn approval_row_has_no_simulation_block_when_null() {
        let approvals = vec![approval("a1", "finance", "s1", "tool_call", "2026-08-21T10:00:00Z", Value::Null)];
        let items = build_feed(&approvals, &[]);
        let exp = items[0].approval.as_ref().unwrap();
        assert!(exp.world_state_change.is_none());
        assert!(exp.risk_points.is_empty());
    }

    #[test]
    fn approval_row_ignores_blank_world_state_change() {
        let sim = json!({ "world_state_change": "   ", "risk_points": [] });
        let approvals = vec![approval("a1", "finance", "s1", "tool_call", "2026-08-21T10:00:00Z", sim)];
        let items = build_feed(&approvals, &[]);
        assert!(items[0].approval.as_ref().unwrap().world_state_change.is_none());
    }

    #[test]
    fn activity_row_missing_id_is_dropped_not_panicked() {
        let activities = vec![json!({ "type": "task_completed", "summary": "x" })];
        assert!(build_feed(&[], &activities).is_empty());
    }

    #[test]
    fn classify_activity_event_routes_known_system_prefixes() {
        for ty in ["channel_recovered", "autopilot_triggered", "gvu_consolidated", "goal_loop.created", "os_watch_goal_kickoff", "playbook_rule_retired", "approval.channel_decision"] {
            assert_eq!(classify_activity_event(ty), FeedCategory::System, "expected {ty} to classify as System");
        }
    }

    #[test]
    fn classify_activity_event_defaults_unknown_types_to_notification() {
        for ty in ["task_completed", "agent.progress", "goal_intent.outcome", "os_file", "something_never_seen_before"] {
            assert_eq!(classify_activity_event(ty), FeedCategory::Notification, "expected {ty} to classify as Notification");
        }
    }

    #[test]
    fn no_activity_event_type_ever_classifies_as_mention() {
        // Pins the header comment's honesty claim: whatever the classifier
        // does, Mention is unreachable from real activity data today.
        for ty in ["task_completed", "channel_recovered", "goal_loop.created", "totally_unknown"] {
            assert_ne!(classify_activity_event(ty), FeedCategory::Mention);
        }
    }

    #[test]
    fn task_completed_activity_gets_the_done_badge_others_do_not() {
        let activities = vec![
            activity("e1", "task_completed", "duke", "done", "2026-08-21T09:00:00Z"),
            activity("e2", "agent.progress", "duke", "still going", "2026-08-21T09:05:00Z"),
        ];
        let items = build_feed(&[], &activities);
        assert_eq!(items[0].badge, Some(FeedBadge::Done));
        assert_eq!(items[1].badge, None);
    }

    #[test]
    fn approval_row_always_gets_the_pending_badge() {
        let approvals = vec![approval("a1", "finance", "s", "tool_call", "2026-08-21T10:00:00Z", Value::Null)];
        assert_eq!(build_feed(&approvals, &[])[0].badge, Some(FeedBadge::Pending));
    }

    #[test]
    fn filter_feed_all_returns_everything() {
        let approvals = vec![approval("a1", "finance", "s", "tool_call", "t", Value::Null)];
        let activities = vec![activity("e1", "task_completed", "duke", "s", "t")];
        let items = build_feed(&approvals, &activities);
        assert_eq!(filter_feed(&items, FilterKind::All).len(), 2);
    }

    #[test]
    fn filter_feed_approval_excludes_activity_rows() {
        let approvals = vec![approval("a1", "finance", "s", "tool_call", "t", Value::Null)];
        let activities = vec![activity("e1", "task_completed", "duke", "s", "t")];
        let items = build_feed(&approvals, &activities);
        let visible = filter_feed(&items, FilterKind::Approval);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].category, FeedCategory::Approval);
    }

    #[test]
    fn filter_feed_system_excludes_notification_rows() {
        let activities = vec![
            activity("e1", "channel_recovered", "autopilot", "s", "t"),
            activity("e2", "task_completed", "duke", "s", "t"),
        ];
        let items = build_feed(&[], &activities);
        let visible = filter_feed(&items, FilterKind::System);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].id, "activity:e1");
    }

    #[test]
    fn filter_feed_mention_is_always_empty_given_todays_data() {
        let approvals = vec![approval("a1", "finance", "s", "tool_call", "t", Value::Null)];
        let activities = vec![
            activity("e1", "channel_recovered", "autopilot", "s", "t"),
            activity("e2", "task_completed", "duke", "s", "t"),
        ];
        let items = build_feed(&approvals, &activities);
        assert!(filter_feed(&items, FilterKind::Mention).is_empty());
    }

    #[test]
    fn build_decide_params_matches_handle_approvals_decide_shape() {
        // `id` required str, `approve` bool, `reason` optional str — read
        // straight from `handle_approvals_decide`'s param parsing, not
        // guessed. See this function's own doc comment.
        let v = build_decide_params("appr-1", true, "看起來沒問題");
        assert_eq!(v["id"], json!("appr-1"));
        assert_eq!(v["approve"], json!(true));
        assert_eq!(v["reason"], json!("看起來沒問題"));
    }

    #[test]
    fn build_decide_params_reject_carries_approve_false() {
        let v = build_decide_params("appr-2", false, "");
        assert_eq!(v["approve"], json!(false));
    }

    #[test]
    fn filter_kind_all_chip_order_matches_the_canvas() {
        assert_eq!(
            FilterKind::ALL,
            [FilterKind::All, FilterKind::Approval, FilterKind::Notification, FilterKind::Mention, FilterKind::System]
        );
    }
}
