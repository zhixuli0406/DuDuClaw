//! v1.81 P1d: canonical delegation targets, every delegation route, id-keyed
//! and optional-scope dashboard reads, push events of every rule, refusal
//! texts and the dashboard dispatch coverage table. Real users.db /
//! tasks.db / approvals.db stores (hardened opener, private test home). As
//! in P1–P1c, `protected` is passed in: no test sets the process-wide flag.
use super::*;
use super::super::personal_data_gate::{STEP_UP_REQUIRED, agent_scoped, task_scoped};
use super::super::personal_delegation_gate::{delegation_decision, delegation_target_keys, direct_targets};
use super::super::personal_push::{NO_EMPLOYEE_DATA, withheld};
use super::super::push_filter::PreparedPush;
use super::super::task_privacy::PERMISSION_DENIED;
use crate::personal_visibility::tests::{PERSONAL, SHARED, home};
use crate::task_store::{ActivityRow, GoalRow, PlanRow, TaskRow, TaskStore};
use crate::personal_visibility::{
    NON_CANONICAL_AGENT, OWNERSHIP_UNAVAILABLE, PERSONAL_DECISION_REFUSED, PERSONAL_DELEGATION_REFUSED, WorkItem,
    canonical_agent,
};

fn task(id: &str, agent: &str) -> TaskRow {
    TaskRow::new(id.into(), "Quarterly report".into(), "secret description".into(), "normal".into(), agent.into(),
        "system".into())
}

fn goal_created(id: &str, agent: &str, title: &str, goal: Option<&str>) -> ActivityRow {
    ActivityRow {
        id: id.into(),
        event_type: "goal_created".into(),
        agent_id: agent.into(),
        task_id: None,
        summary: format!("Created goal: {title}"),
        timestamp: chrono::Utc::now().to_rfc3339(),
        metadata: goal.map(|goal| json!({ "goal_id": goal }).to_string()),
    }
}

// ── 1–4. delegation ──────────────────────────────────────────────────────

#[test]
fn p1d_delegation_target_is_canonical_in_gate_and_handlers() {
    let h = home();
    let ctx = |id: &str| h.db.live_user_context(id).unwrap().unwrap();
    let (alice, bob, admin) = (ctx(h.alice.as_str()), ctx(h.bob.as_str()), ctx(h.admin.as_str()));
    assert_eq!(canonical_agent(PERSONAL), Ok(PERSONAL));
    assert_eq!(canonical_agent(""), Ok(""));
    let routes = [("tasks.create", "assigned_to"), ("tasks.create", "agent_id"), ("tasks.update", "assigned_to"),
        ("tasks.assign", "agent_id"), ("tasks.goal_create", "agent_id"), ("plans.create", "agent_id"),
        ("agents.delegate", "agent_id"), ("plans.add_step", "assignee"), ("plans.update_step", "assignee")];
    for raw in [" alice-personal", "alice-personal ", "\talice-personal", "alice-personal\n", " "] {
        // The handlers (`delegation_check`) and the dispatch gate share this
        // rule: refused, never trimmed — in every mode.
        assert_eq!(canonical_agent(raw), Err(NON_CANONICAL_AGENT), "{raw:?}");
        for (method, key) in routes {
            let params = json!({ key: raw, "task_id": "t", "title": "x" });
            for protected in [true, false] {
                assert_eq!(delegation_decision(h.path(), protected, &alice, method, &params), Some(NON_CANONICAL_AGENT),
                    "{method}.{key} {raw:?}");
            }
        }
    }
    for (method, key) in routes {
        let params = json!({ key: PERSONAL, "task_id": "t", "title": "x" });
        // The owner hands work to its own Personal employee.
        assert_eq!(delegation_decision(h.path(), true, &alice, method, &params), None, "{method}.{key}");
        // An Admin who is not the owner is refused (no break-glass) and told why.
        assert_eq!(delegation_decision(h.path(), true, &admin, method, &params), Some(PERSONAL_DELEGATION_REFUSED));
        // Anyone else gets the text of an unknown employee (P1d item 8).
        assert_eq!(delegation_decision(h.path(), true, &bob, method, &params), Some(PERMISSION_DENIED));
        // Shared employees and unprotected installs: unchanged.
        let shared = json!({ key: SHARED, "task_id": "t" });
        assert_eq!(delegation_decision(h.path(), true, &admin, method, &shared), None);
        assert_eq!(delegation_decision(h.path(), false, &admin, method, &params), None);
    }
    // Fail closed: no readable ownership mirror.
    let empty = crate::fs_safe::private_test_home().unwrap();
    assert_eq!(delegation_decision(empty.path(), true, &admin, "tasks.create", &json!({"assigned_to": PERSONAL})),
        Some(OWNERSHIP_UNAVAILABLE));
}

#[tokio::test]
async fn p1d_every_create_route_and_link_checks_its_employee() {
    let h = home();
    let ctx = |id: &str| h.db.live_user_context(id).unwrap().unwrap();
    let (alice, admin) = (ctx(h.alice.as_str()), ctx(h.admin.as_str()));
    // `tasks.create kind:"goal"` reads `agent_id`: a Shared `assigned_to`
    // cannot carry a Personal `agent_id` past the gate.
    assert_eq!(delegation_target_keys("tasks.create"), ["assigned_to", "agent_id"]);
    let goal = json!({"kind": "goal", "assigned_to": SHARED, "agent_id": PERSONAL, "title": "x"});
    assert_eq!(direct_targets("tasks.create", &goal), Ok(vec![SHARED.to_string(), PERSONAL.to_string()]));
    assert_eq!(delegation_decision(h.path(), true, &admin, "tasks.create", &goal), Some(PERSONAL_DELEGATION_REFUSED));
    assert_eq!(delegation_decision(h.path(), true, &alice, "tasks.create", &goal), None);
    // `kind:"discovery"` reads `assigned_to`.
    let discovery = json!({"kind": "discovery", "assigned_to": PERSONAL, "title": "x"});
    assert_eq!(delegation_decision(h.path(), true, &admin, "tasks.create", &discovery), Some(PERSONAL_DELEGATION_REFUSED));
    // Every agent step of a new plan; a human step names a person, not an employee.
    let plan = json!({"agent_id": SHARED, "steps": [{"text": "a"}, {"text": "b", "assignee": PERSONAL}]});
    assert_eq!(delegation_decision(h.path(), true, &admin, "plans.create", &plan), Some(PERSONAL_DELEGATION_REFUSED));
    let human = json!({"agent_id": SHARED, "steps": [{"text": "b", "assignee_kind": "human", "assignee": h.bob}]});
    assert_eq!(delegation_decision(h.path(), true, &admin, "plans.create", &human), None);
    // Work linked under another person's Personal task.
    let store = TaskStore::open(h.path()).unwrap();
    store.insert_task(&task("personal-task", PERSONAL)).await.unwrap();
    store.insert_task(&task("shared-task", SHARED)).await.unwrap();
    for (method, params) in [
        ("tasks.create", json!({"assigned_to": SHARED, "title": "x", "parent_task_id": "personal-task"})),
        ("tasks.create", json!({"assigned_to": SHARED, "title": "x", "depends_on": ["shared-task", "personal-task"]})),
        ("tasks.update", json!({"task_id": "shared-task", "depends_on": "[\"personal-task\"]"})),
    ] {
        assert_eq!(delegation_decision(h.path(), true, &admin, method, &params), Some(PERSONAL_DELEGATION_REFUSED),
            "{params}");
        assert_eq!(delegation_decision(h.path(), true, &alice, method, &params), None, "{params}");
    }
    let unknown = json!({"assigned_to": SHARED, "title": "x", "parent_task_id": "no-such-task"});
    assert_eq!(delegation_decision(h.path(), true, &admin, "tasks.create", &unknown), None);
}

#[test]
fn p1d_assign_delegate_and_skill_runner_are_owner_only() {
    let h = home();
    let ctx = |id: &str| h.db.live_user_context(id).unwrap().unwrap();
    let (alice, admin) = (ctx(h.alice.as_str()), ctx(h.admin.as_str()));
    for method in ["tasks.assign", "agents.delegate"] {
        assert_eq!(delegation_target_keys(method), ["agent_id"], "{method}");
        let params = json!({"agent_id": PERSONAL, "task_id": "t", "prompt": "x", "wait_for_response": true});
        assert_eq!(delegation_decision(h.path(), true, &admin, method, &params), Some(PERSONAL_DELEGATION_REFUSED));
        assert_eq!(delegation_decision(h.path(), true, &alice, method, &params), None);
    }
    // `tasks.assign` is no longer a Work read an Admin step-up could pass;
    // its source task is still gated on that task's employee.
    assert_eq!(agent_scoped("tasks.assign"), None);
    assert!(task_scoped("tasks.assign"));
    // `tasks.skill_assign`: the server-picked runner is never another
    // person's Personal employee.
    let mut runners = vec![PERSONAL.to_string(), SHARED.to_string()];
    let hidden = visibility::hidden_from(h.path(), true, &h.admin).unwrap();
    retain_visible(&mut runners, &hidden, String::as_str);
    assert_eq!(runners, [SHARED.to_string()]);
    let mut owned = vec![PERSONAL.to_string(), SHARED.to_string()];
    retain_visible(&mut owned, &visibility::hidden_from(h.path(), true, &h.alice).unwrap(), String::as_str);
    assert_eq!(owned.len(), 2);
}

// ── 5. dashboard reads ───────────────────────────────────────────────────

#[tokio::test]
async fn p1d_dashboard_reads_resolve_their_employee() {
    use DataCategory::*;
    let h = home();
    for (method, category) in [
        ("agents.export_profile", AgentFiles), ("skills.content", AgentFiles), ("skills.share", AgentFiles),
        ("playbook.list", Memory), ("playbook.export", Memory), ("playbook.retire", Memory),
        ("wiki.trust_audit", Wiki), ("wiki.trust_history", Wiki), ("wiki.trust_override", Wiki),
    ] {
        assert_eq!(agent_scoped(method), Some(category), "{method}");
    }
    for (method, key, kind) in [
        ("plans.get", "plan_id", IdKind::Plan), ("plans.update", "plan_id", IdKind::Plan),
        ("plans.remove", "plan_id", IdKind::Plan), ("plans.add_step", "plan_id", IdKind::Plan),
        ("plans.update_step", "step_id", IdKind::PlanStep), ("plans.remove_step", "step_id", IdKind::PlanStep),
        ("mail.read", "mail_id", IdKind::Mail), ("mail.archive", "mail_id", IdKind::Mail),
        ("agent_files.pending_get", "approval_id", IdKind::Approval),
        ("fork.inspect", "fork_id", IdKind::Fork), ("fork.resolve", "fork_id", IdKind::Fork),
    ] {
        let scope = id_scoped(method).unwrap_or_else(|| panic!("{method} must be id-scoped"));
        assert_eq!((scope.key, scope.kind), (key, kind), "{method}");
    }
    // Optional-scope lists hide instead (`p1d_list_hidden`).
    for method in ["plans.list", "mail.list", "mail.outbox", "fork.list"] {
        assert!(id_scoped(method).is_none() && agent_scoped(method).is_none(), "{method}");
    }

    let store = TaskStore::open(h.path()).unwrap();
    store.insert_plan(&PlanRow::new("personal-plan".into(), "p".into(), PERSONAL.into(), "system".into())).await.unwrap();
    store.add_plan_step("personal-plan", "personal-step", "secret step", "agent", PERSONAL, None).await.unwrap();
    assert_eq!(id_employee(h.path(), IdKind::Plan, "personal-plan"), Ok(Some(PERSONAL.to_string())));
    assert_eq!(id_employee(h.path(), IdKind::PlanStep, "personal-step"), Ok(Some(PERSONAL.to_string())));
    assert_eq!(id_employee(h.path(), IdKind::Plan, "missing"), Ok(None));
    assert_eq!(id_employee(h.path(), IdKind::Mail, "missing"), Ok(None));
    assert_eq!(id_employee(h.path(), IdKind::Fork, "missing"), Ok(None), "no fork store yet");
    let broker = crate::approval::ApprovalBroker::open(h.path()).unwrap();
    let card = broker.request(PERSONAL, "dynamic_review_kind", "secret", json!({}), 300).await.unwrap();
    assert_eq!(id_employee(h.path(), IdKind::Approval, card.as_str()), Ok(Some(PERSONAL.to_string())));
    // Goals belong to their creating employee (metadata, or a legacy title match).
    store.insert_goal(&GoalRow::new("g-personal".into(), "Personal goal".into(), String::new())).await.unwrap();
    store.insert_goal(&GoalRow::new("g-legacy".into(), "Legacy goal".into(), String::new())).await.unwrap();
    store.insert_goal(&GoalRow::new("g-company".into(), "Company goal".into(), String::new())).await.unwrap();
    store.append_activity(&goal_created("a1", PERSONAL, "Personal goal", Some("g-personal"))).await.unwrap();
    store.append_activity(&goal_created("a2", PERSONAL, "Legacy goal", None)).await.unwrap();
    for goal in ["g-personal", "g-legacy"] {
        assert_eq!(visibility::work_item_employee(h.path(), WorkItem::Goal, goal), Ok(Some(PERSONAL.to_string())));
    }
    assert_eq!(visibility::work_item_employee(h.path(), WorkItem::Goal, "g-company"), Ok(None));
    let creators = visibility::goal_creators(h.path()).unwrap();
    assert_eq!(creators.get("g-personal").map(String::as_str), Some(PERSONAL));
    assert!(!creators.contains_key("g-company"));

    // A non-Admin refused sees the handler's own unknown-id text.
    let mail = id_scoped("mail.read").unwrap();
    assert_eq!(id_refusal_text(false, mail, NOT_ALLOWED), "mail not found");
    assert_eq!(id_refusal_text(true, mail, STEP_UP_REQUIRED), STEP_UP_REQUIRED);
    assert_eq!(id_refusal_text(false, id_scoped("plans.get").unwrap(), NOT_ALLOWED), NOT_ALLOWED);
    // Fail closed: unreadable stores.
    let empty = crate::fs_safe::private_test_home().unwrap();
    assert_eq!(id_employee(empty.path(), IdKind::Plan, "personal-plan"), Err(Unavailable));
    assert_eq!(id_employee(empty.path(), IdKind::PlanStep, "personal-step"), Err(Unavailable));
    assert_eq!(DataCategory::Mail.as_str(), "mail");
}

// ── 7. push events of every rule ─────────────────────────────────────────

#[tokio::test]
async fn p1d_push_events_of_every_rule_are_filtered() {
    let h = home();
    let store = TaskStore::open(h.path()).unwrap();
    store.insert_task(&task("personal-task", PERSONAL)).await.unwrap();
    store.insert_plan(&PlanRow::new("personal-plan".into(), "p".into(), PERSONAL.into(), "system".into())).await.unwrap();
    let empty = crate::fs_safe::private_test_home().unwrap();
    let prepare = |frame: WsFrame| {
        let home = h.path().to_path_buf();
        async move { PreparedPush::prepare(home, serde_json::to_string(&frame).unwrap()).await.expect("prepared") }
    };
    for frame in [
        WsFrame::event("plan.updated", json!({"plan_id": "personal-plan"})),
        WsFrame::event("budget.breaker_open", json!({"agent_id": PERSONAL})),
        WsFrame::event("assign_schedule.changed", json!({"agent": PERSONAL})),
        WsFrame::event("memory.changed", json!({"agents": [SHARED, PERSONAL]})),
        WsFrame::event("unknown.event", json!({"task_id": "personal-task"})),
        // No recognizable subject: it could concern one, so it is withheld
        // from anyone with a hidden Personal employee.
        WsFrame::event("cron.changed", json!({})),
        WsFrame::event("assign_schedule.changed", json!({"schedule_id": "s"})),
    ] {
        let prepared = prepare(frame).await;
        for user in [h.admin.as_str(), h.bob.as_str(), "system"] {
            assert!(withheld(h.path(), true, user, &prepared), "{user}: {}", prepared.line);
        }
        // The owner hides nothing, so gets everything as before.
        assert!(!withheld(h.path(), true, &h.alice, &prepared), "{}", prepared.line);
        assert!(withheld(empty.path(), true, &h.alice, &prepared), "unreadable mirror: {}", prepared.line);
        assert!(!withheld(h.path(), false, &h.admin, &prepared), "unprotected: {}", prepared.line);
    }
    // System / Authenticated events, the no-employee-data list and Shared
    // subjects are unchanged; the list entries never read users.db.
    assert!(NO_EMPLOYEE_DATA.contains(&"system.update_failed"));
    for frame in [
        WsFrame::event("system.update_failed", json!({"code": "update_failed"})),
        WsFrame::event("events_lost", json!({})),
        WsFrame::event("dashboard.navigate", json!({"agent_id": PERSONAL})),
        WsFrame::event("task.removed", json!({"task_id": "personal-task"})),
    ] {
        let prepared = prepare(frame).await;
        assert!(!withheld(empty.path(), true, &h.admin, &prepared), "{}", prepared.line);
    }
    let shared = prepare(WsFrame::event("memory.changed", json!({"agent_id": SHARED}))).await;
    assert!(!withheld(h.path(), true, &h.admin, &shared));
}

// ── 8. refusal texts ─────────────────────────────────────────────────────

#[tokio::test]
async fn p1d_refusals_never_reveal_personal_ownership() {
    let h = home();
    let bob = h.db.live_user_context(&h.bob).unwrap().unwrap();
    // Dashboard: the delegation / task / id refusal a non-Admin gets is the
    // text an unknown or unbound employee gets.
    assert_eq!(PERMISSION_DENIED, NOT_ALLOWED);
    assert_eq!(acl::require_agent_access(&bob, "no-such-employee", AccessLevel::Viewer).unwrap_err(), NOT_ALLOWED);
    // Channels: the text of an unknown or expired card.
    let broker = crate::approval::ApprovalBroker::open(h.path()).unwrap();
    let id = broker.request(PERSONAL, "dynamic_review_kind", "secret", json!({}), 300).await.unwrap();
    let rec = broker.get(&id).await.unwrap().unwrap();
    let refusal = crate::personal_decision::channel_card_refusal_with(h.path(), true, &rec, "telegram", "424242");
    assert_eq!(refusal, Some(crate::personal_decision::CHANNEL_CARD_NOT_FOUND));
    assert_ne!(refusal, Some(PERSONAL_DECISION_REFUSED));
}

// ── 10. dashboard dispatch coverage ──────────────────────────────────────

/// Dispatch arms that read `agent_id` but are deliberately not gated as a
/// Personal employee's private data, each with its reason.
const AGENT_ID_ALLOWLIST: &[(&str, &str)] = &[
    ("agents.status", "runtime status metadata only (state, model, activity counters); Admins get metadata"),
    ("presets.status", "the name of the bound preset only"),
    ("agents.avatar", "the avatar image only (also shown in every listing)"),
    ("agents.pause", "operational control under Manager + Operator binding; reads no private data"),
    ("agents.resume", "operational control under Manager + Operator binding; reads no private data"),
    ("agents.update", "configuration write under the Owner binding; SOUL / CONTRACT / files are gated separately"),
    ("agents.google_identity_get", "binding metadata (address, status); mailbox content is gated by mail.*"),
    ("agents.google_identity_set", "configuration write under the Owner binding"),
    ("plans.list", "optional scope: gated and filtered in the arm (`p1d_list_hidden`)"),
];

const DISPATCH_SOURCES: &[(&str, &str)] = &[
    ("dispatch_core.rs", include_str!("dispatch_core.rs")),
    ("dispatch_org.rs", include_str!("dispatch_org.rs")),
    ("dispatch_knowledge.rs", include_str!("dispatch_knowledge.rs")),
    ("dispatch_system.rs", include_str!("dispatch_system.rs")),
    ("dispatch_ops.rs", include_str!("dispatch_ops.rs")),
];

fn reads_agent_id(code: &str) -> bool {
    ["check_agent!(", "check_agent_filter!(", "check_agent_named!(", "check_task_board_filter!(", "\"agent_id\""]
        .iter()
        .any(|needle| code.contains(needle))
}

/// Every method of a top-level `match method` arm (12-space indent) whose
/// pattern or body reads `agent_id`.
fn agent_id_arms(source: &str) -> std::collections::BTreeSet<String> {
    let mut found = std::collections::BTreeSet::new();
    let mut current: Vec<String> = Vec::new();
    let mut in_pattern = false;
    for line in source.lines() {
        let text = line.trim_start();
        if text.starts_with("//") {
            continue;
        }
        let indent = line.len() - text.len();
        let starts_arm = indent == 12 && (text.starts_with('"') || text.starts_with('_'));
        let continues = in_pattern && indent == 12 && text.starts_with('|');
        if starts_arm || continues {
            if starts_arm && !in_pattern {
                current.clear();
            }
            let (head, body) = text.split_once("=>").unwrap_or((text, ""));
            let head = head.split(" if ").next().unwrap_or(head);
            current.extend(head.split('"').skip(1).step_by(2).map(str::to_owned));
            in_pattern = !text.contains("=>");
            if reads_agent_id(body) {
                found.extend(current.iter().cloned());
            }
            continue;
        }
        if reads_agent_id(text) {
            found.extend(current.iter().cloned());
        }
    }
    found
}

#[test]
fn p1d_every_dashboard_agent_id_arm_is_gated_or_allowlisted() {
    use super::super::personal_delegation_gate::delegation_target_key;
    let mut found = std::collections::BTreeSet::new();
    for (_, source) in DISPATCH_SOURCES {
        found.extend(agent_id_arms(source));
    }
    // The scan itself works (a broken parser would find nothing).
    assert!(found.len() >= 50, "only {} arms found: {found:?}", found.len());
    for method in ["memory.search", "agent_files.get", "plans.create", "agents.delegate", "playbook.list", "tasks.list"] {
        assert!(found.contains(method), "{method} not found by the scan");
    }
    let missing: Vec<_> = found
        .iter()
        .filter(|method| {
            let method = method.as_str();
            agent_scoped(method).is_none()
                && delegation_target_key(method).is_none()
                && !AGENT_ID_ALLOWLIST.iter().any(|(name, _)| *name == method)
        })
        .collect();
    assert!(missing.is_empty(), "dispatch arms reading agent_id that are neither gated nor allowlisted: {missing:?}");
    // The allowlist stays honest: every entry is a real, ungated arm.
    for (method, reason) in AGENT_ID_ALLOWLIST {
        assert!(!reason.is_empty());
        assert!(found.contains(*method), "stale allowlist entry {method}");
        assert!(agent_scoped(method).is_none() && delegation_target_key(method).is_none(), "{method} is gated");
    }
}
