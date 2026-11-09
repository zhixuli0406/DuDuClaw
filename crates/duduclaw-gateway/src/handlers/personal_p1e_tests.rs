//! v1.81 P1e: config / identity writes, pause / resume audit, digest, goal
//! ownership, refusal texts, the scoped-list step-up and the widened
//! dashboard dispatch coverage scan. Real users.db / tasks.db stores
//! (hardened opener, private test home); `protected` is passed in — no test
//! sets the process-wide flag.
use super::*;
use super::super::personal_delegation_gate::delegation_target_key;
use super::super::personal_p1d_gate::{IdKind, id_employee, id_scoped, list_break_glass, retain_visible};
use crate::personal_visibility::tests::{PERSONAL, SHARED, home};
use crate::personal_visibility::{NON_CANONICAL_AGENT, OWNERSHIP_UNAVAILABLE, WorkItem};
use crate::task_store::{ActivityRow, GoalRow, TaskStore};
use std::collections::{BTreeMap, BTreeSet, HashMap};

fn proof(verified_at: i64) -> MigrationConfirmationSession {
    MigrationConfirmationSession { verified_at: verified_at as u64, csrf: [7; 32] }
}

fn goal_row(id: &str, agent: &str, summary: &str, metadata: Option<String>) -> ActivityRow {
    ActivityRow {
        id: id.into(),
        event_type: visibility::GOAL_CREATED_EVENT.into(),
        agent_id: agent.into(),
        task_id: None,
        summary: summary.into(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        metadata,
    }
}

// ── N1. config / identity writes ─────────────────────────────────────────

#[test]
fn p1e_config_and_identity_writes_are_gated_methods() {
    use DataCategory::*;
    for (method, key, category) in [
        ("agents.update", "agent_id", Config), ("agents.set_avatar", "agent_id", Config),
        ("agents.set_outfit", "agent_id", Config), ("agents.clear_avatar", "agent_id", Config),
        ("agents.tool_pruning_restore", "agent_id", Config), ("agents.archive", "agent_id", Config),
        ("agents.unarchive", "agent_id", Config), ("agents.remove", "agent_id", Config),
        ("agents.handoff", "from_agent", Config), ("skills.restore", "agent_id", Config),
        ("skills.adopt", "target_agent_id", Config), ("agents.google_identity_set", "agent_id", Identity),
        ("agents.channels.telegram.set", "agent_id", Identity), ("agents.channels.googlechat.remove", "agent_id", Identity),
    ] {
        assert_eq!(write_scoped(method), Some((key, category)), "{method}");
        assert!(category.is_write());
        assert_eq!(agent_scoped(method), None, "{method}: a write, never a read");
    }
    assert_eq!(Config.as_str(), "config");
    assert_eq!(Identity.as_str(), "identity");
    assert!(!Memory.is_write() && !Work.is_write());
    // Reads that stay metadata for Admins are not writes.
    for method in ["agents.status", "agents.google_identity_get", "agents.tool_pruning_status", "agents.list"] {
        assert_eq!(write_scoped(method), None, "{method}");
    }
    // The destination of a handoff is delegation (owner only).
    assert_eq!(delegation_target_key("agents.handoff"), Some("to_agent"));
    // Multi-target skill switches gate every named employee.
    assert_eq!(items_write_scoped("skills.set_enabled"), Some(("items", "agent_id")));
    assert_eq!(items_write_scoped("skills.remove"), Some(("items", "agent_id")));
    let items = json!({"items": [{"agent_id": SHARED, "skill_name": "a"}, {"agent_id": PERSONAL, "skill_name": "b"},
        {"agent_id": PERSONAL, "skill_name": "c"}]});
    assert_eq!(item_agents(&items, "items", "agent_id"), Ok([PERSONAL.to_string(), SHARED.to_string()].into()));
    let padded = json!({"items": [{"agent_id": " alice-personal", "skill_name": "b"}]});
    assert_eq!(item_agents(&padded, "items", "agent_id"), Err(NON_CANONICAL_AGENT));
    // The named id must be canonical (refused, never trimmed).
    assert_eq!(scoped_agent(&json!({"agent_id": PERSONAL}), None, "agent_id"), Ok(Some(PERSONAL.to_string())));
    assert_eq!(scoped_agent(&json!({"agent_id": "alice-personal "}), None, "agent_id"), Err(NON_CANONICAL_AGENT));
    assert_eq!(scoped_agent(&json!({"agent_id": ""}), None, "agent_id"), Ok(None));
    assert_eq!(scoped_agent(&json!({}), None, "agent_id"), Ok(None));
}

#[test]
fn p1e_admin_config_write_needs_step_up_and_is_audited_as_a_write() {
    let h = home();
    let now = chrono::Utc::now().timestamp();
    // The owner passes, an Admin who is not the owner needs break-glass,
    // anyone else is refused with the text of an unknown employee.
    assert_eq!(visibility::classify_reader(h.path(), true, &h.alice, false, PERSONAL), Ok(Reader::Owner));
    let Ok(Reader::BreakGlass { owner }) = visibility::classify_reader(h.path(), true, &h.admin, true, PERSONAL) else {
        panic!("an Admin who is not the owner needs break-glass");
    };
    assert_eq!(visibility::classify_reader(h.path(), true, &h.bob, false, PERSONAL), Ok(Reader::Refused));
    assert_eq!(NOT_ALLOWED, "permission denied");
    for (method, category) in [("agents.update", DataCategory::Config), ("agents.google_identity_set", DataCategory::Identity)] {
        let target = Target { method, category, agent: PERSONAL };
        assert_eq!(authorize_break_glass(&h.db, None, &h.admin, Some(&[7; 32]), now, &target, &owner),
            Err(STEP_UP_REQUIRED_WRITE), "{method}: no step-up, no write");
        assert_eq!(authorize_break_glass(&h.db, Some(&proof(now - 60)), &h.admin, None, now, &target, &owner),
            Err(STEP_UP_REQUIRED_WRITE), "{method}: no token, no write");
        assert_eq!(authorize_break_glass(&h.db, Some(&proof(now - 60)), &h.admin, Some(&[7; 32]), now, &target, &owner),
            Ok(()), "{method}");
    }
    let rows = h.db.query_audit_log(Some(&h.admin), Some(WRITE_ACTION), 50).unwrap();
    assert_eq!(rows.len(), 2);
    let categories: BTreeSet<String> = rows.iter().map(|row| {
        let detail: Value = serde_json::from_str(row.detail.as_deref().unwrap()).unwrap();
        assert_eq!(detail["employee"], json!(PERSONAL));
        assert_eq!(detail["owner_user_id"], json!(h.alice));
        assert_eq!(row.target.as_deref(), Some(PERSONAL));
        detail["category"].as_str().unwrap().to_owned()
    }).collect();
    assert_eq!(categories, ["config".to_string(), "identity".to_string()].into());
    assert_eq!(h.db.query_audit_log(Some(&h.admin), Some(WRITE_REFUSED_ACTION), 50).unwrap().len(), 4);
    // A write is never recorded as a read.
    assert!(h.db.query_audit_log(Some(&h.admin), Some(READ_ACTION), 50).unwrap().is_empty());
}

#[test]
fn p1e_pause_and_resume_of_someone_elses_personal_employee_are_audited() {
    let h = home();
    for method in ["agents.pause", "agents.resume"] {
        assert_eq!(audit_personal_control(h.path(), Some(&h.db), true, &h.admin, method, PERSONAL), Ok(()));
    }
    let rows = h.db.query_audit_log(Some(&h.admin), Some(CONTROL_ACTION), 50).unwrap();
    assert_eq!(rows.len(), 2);
    let detail: Value = serde_json::from_str(rows[0].detail.as_deref().unwrap()).unwrap();
    assert_eq!(detail["employee"], json!(PERSONAL));
    assert_eq!(detail["owner_user_id"], json!(h.alice));
    assert_eq!(detail["category"], json!("config"));
    // The owner, a Shared employee and an unprotected install: no row.
    assert_eq!(audit_personal_control(h.path(), Some(&h.db), true, &h.alice, "agents.pause", PERSONAL), Ok(()));
    assert_eq!(audit_personal_control(h.path(), Some(&h.db), true, &h.admin, "agents.pause", SHARED), Ok(()));
    assert_eq!(audit_personal_control(h.path(), Some(&h.db), false, &h.admin, "agents.pause", PERSONAL), Ok(()));
    assert!(h.db.query_audit_log(Some(&h.alice), Some(CONTROL_ACTION), 50).unwrap().is_empty());
    assert_eq!(h.db.query_audit_log(Some(&h.admin), Some(CONTROL_ACTION), 50).unwrap().len(), 2);
    // Fail closed: unreadable ownership or no audit store.
    let empty = crate::fs_safe::private_test_home().unwrap();
    assert_eq!(audit_personal_control(empty.path(), Some(&h.db), true, &h.admin, "agents.pause", PERSONAL),
        Err(OWNERSHIP_UNAVAILABLE));
    assert!(audit_personal_control(h.path(), None, true, &h.admin, "agents.pause", PERSONAL).is_err());
}

// ── N2. digest ───────────────────────────────────────────────────────────

#[test]
fn p1e_digest_is_gated_and_hides_other_personal_employees() {
    // `digest.feedback`: its task (`task_id`) or its file's employee
    // (`artifact.agent_id`) goes through the P1 gate before the handler.
    assert!(task_scoped("digest.feedback"));
    assert_eq!(nested_agent_scoped("digest.feedback"), Some(("artifact", "agent_id")));
    let artifact = json!({"artifact": {"agent_id": PERSONAL, "archived_name": "x"}, "verdict": "up"});
    assert_eq!(scoped_agent(&artifact, Some("artifact"), "agent_id"), Ok(Some(PERSONAL.to_string())));
    let padded = json!({"artifact": {"agent_id": " alice-personal", "archived_name": "x"}});
    assert_eq!(scoped_agent(&padded, Some("artifact"), "agent_id"), Err(NON_CANONICAL_AGENT));
    // `digest.latest`: the employees an Admin (not the owner) gets exclude
    // the Personal one; the owner keeps it.
    let h = home();
    let mut for_admin = vec![PERSONAL.to_string(), SHARED.to_string()];
    retain_visible(&mut for_admin, &visibility::hidden_from(h.path(), true, &h.admin).unwrap(), String::as_str);
    assert_eq!(for_admin, [SHARED.to_string()]);
    let mut for_owner = vec![PERSONAL.to_string(), SHARED.to_string()];
    retain_visible(&mut for_owner, &visibility::hidden_from(h.path(), true, &h.alice).unwrap(), String::as_str);
    assert_eq!(for_owner.len(), 2);
    // The handler drops them before the per-agent loop, for Admins too.
    let source = include_str!("digest_rpc.rs");
    let hidden_at = source.find("self.personal_hidden_agents(&live)").expect("digest.latest reads the hidden set");
    let drop_at = source.find("retain_visible(&mut d.agents, &hidden").expect("digest.latest drops hidden employees");
    let admin_skip = source.find("if !live.is_admin()").unwrap();
    assert!(hidden_at < drop_at && drop_at < admin_skip, "hidden employees are dropped before the Admin shortcut");
}

// ── N3. goal ownership ───────────────────────────────────────────────────

#[test]
fn p1e_goal_created_is_reserved_and_metadata_only() {
    for reserved in ["goal_created", "GOAL_CREATED", " goal_created ", "Goal_Created"] {
        assert!(visibility::is_reserved_activity_event(reserved), "{reserved}");
    }
    for allowed in ["agent_comment", "goal_updated", "goal_created_note"] {
        assert!(!visibility::is_reserved_activity_event(allowed), "{allowed}");
    }
    assert_eq!(visibility::linked_goals(Some(r#"{"goal_id":"g1"}"#)), ["g1"]);
    assert_eq!(visibility::linked_goals(Some(r#"{"goal_ids":["g1","g2"]}"#)), ["g1", "g2"]);
    assert!(visibility::linked_goals(None).is_empty(), "no title matching");
    assert!(visibility::linked_goals(Some("not json")).is_empty());
    assert!(visibility::linked_goals(Some(r#"{"goal_id":""}"#)).is_empty());
    // The one-time backfill.
    use crate::task_store::goal_creator_backfill::backfilled_metadata;
    let one = backfilled_metadata(None, &["g1".to_string()]).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&one).unwrap(), json!({"goal_id": "g1", "goal_backfill": "p1e"}));
    let many = backfilled_metadata(Some(r#"{"note":"x"}"#), &["g1".to_string(), "g2".to_string()]).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&many).unwrap(),
        json!({"note": "x", "goal_ids": ["g1", "g2"], "goal_backfill": "p1e"}));
    let legacy = backfilled_metadata(Some("[1]"), &["g1".to_string()]).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&legacy).unwrap()["legacy_metadata"], json!("[1]"));
    assert_eq!(backfilled_metadata(Some(r#"{"goal_id":"g9"}"#), &["g1".to_string()]), None, "already linked");
    assert_eq!(backfilled_metadata(None, &[]), None, "no goal has the title");
}

#[tokio::test]
async fn p1e_goal_creators_backfill_once_and_fail_closed() {
    let h = home();
    let store = TaskStore::open(h.path()).unwrap();
    for (id, title) in [("g-legacy", "Legacy goal"), ("g-twin-a", "Twin goal"), ("g-twin-b", "Twin goal"),
        ("g-claimed", "Claimed goal"), ("g-company", "Company goal")] {
        store.insert_goal(&GoalRow::new(id.into(), title.into(), String::new())).await.unwrap();
    }
    // Rows written before P1d (no metadata) …
    store.append_activity(&goal_row("legacy", PERSONAL, "Created goal: Legacy goal", None)).await.unwrap();
    store.append_activity(&goal_row("twin", PERSONAL, "Created goal: Twin goal", None)).await.unwrap();
    // … and two creators claiming one goal (a row forged before P1e).
    store.append_activity(&goal_row("claim-1", SHARED, "Created goal: Claimed goal",
        Some(json!({"goal_id": "g-claimed"}).to_string()))).await.unwrap();
    store.append_activity(&goal_row("claim-2", PERSONAL, "Created goal: Claimed goal",
        Some(json!({"goal_id": "g-claimed"}).to_string()))).await.unwrap();
    // A forged title-only row naming a company goal never links it once
    // the migration has run (title matching is gone).
    drop(store);
    let employees = |goal: &str| visibility::work_item_employees(h.path(), WorkItem::Goal, goal).unwrap();
    assert!(employees("g-legacy").is_empty(), "the store migration already ran at open: no title match");

    // Simulate the first open after the upgrade: the marker is absent.
    rusqlite::Connection::open(h.path().join("tasks.db")).unwrap()
        .execute("DELETE FROM task_store_markers WHERE name = 'p1e_goal_creator_backfill'", []).unwrap();
    let store = TaskStore::open(h.path()).unwrap();
    assert_eq!(employees("g-legacy"), [PERSONAL.to_string()].into(), "exact (unique title) linkage");
    for twin in ["g-twin-a", "g-twin-b"] {
        assert_eq!(employees(twin), [PERSONAL.to_string()].into(), "{twin}: ambiguous title links every match");
    }
    assert_eq!(employees("g-claimed"), [PERSONAL.to_string(), SHARED.to_string()].into());
    assert!(employees("g-company").is_empty());
    // A later row without metadata is never linked: the migration ran once.
    store.append_activity(&goal_row("late", PERSONAL, "Created goal: Company goal", None)).await.unwrap();
    drop(store);
    let _reopened = TaskStore::open(h.path()).unwrap();
    assert!(employees("g-company").is_empty(), "the backfill is one-time");

    // Several distinct creators: hidden whenever ANY of them is hidden.
    let creators = visibility::goal_creators(h.path()).unwrap();
    let hidden_from_admin = visibility::hidden_from(h.path(), true, &h.admin).unwrap();
    assert!(visibility::goal_hidden(creators.get("g-claimed"), &hidden_from_admin));
    assert!(visibility::goal_hidden(creators.get("g-legacy"), &hidden_from_admin));
    assert!(!visibility::goal_hidden(creators.get("g-company"), &hidden_from_admin));
    assert!(!visibility::goal_hidden(creators.get("g-claimed"), &BTreeSet::new()));
    // Fail closed: no readable tasks.db.
    let empty = crate::fs_safe::private_test_home().unwrap();
    assert!(visibility::goal_creators(empty.path()).is_err());
}

// ── N4 / N5 / discovery / steering ───────────────────────────────────────

#[test]
fn p1e_skill_assign_hides_personal_holders_before_any_refusal() {
    let source = include_str!("tasks_skill_assign_rpc.rs");
    let hidden = source.find("retain_visible(&mut holdings").expect("hidden holders are dropped");
    let not_found = source.find("skill_not_found").unwrap();
    assert!(hidden < not_found, "a skill held only by a hidden employee reads like an unknown skill");
    assert_eq!(source.matches("retain_visible(&mut holdings").count(), 1);
}

#[test]
fn p1e_scoped_list_step_up_goes_through_the_audited_gate() {
    let h = home();
    let now = chrono::Utc::now().timestamp();
    let hidden = visibility::hidden_from(h.path(), true, &h.admin).unwrap();
    let token = serde_json::to_value([7u8; 32]).unwrap();
    for (method, category) in [("plans.list", DataCategory::Work), ("mail.list", DataCategory::Mail),
        ("mail.outbox", DataCategory::Mail)] {
        // Not dispatch-gated, so the token is still in the request.
        assert!(agent_scoped(method).is_none() && id_scoped(method).is_none() && write_scoped(method).is_none());
        assert_eq!(STEP_UP_FIELD, "step_up_csrf");
        let params = json!({"agent_id": PERSONAL, "step_up_csrf": token});
        let Some((agent, csrf)) = list_break_glass(&hidden, true, &params) else {
            panic!("{method}: an Admin scope on a hidden employee runs the break-glass gate");
        };
        assert_eq!((agent.as_str(), csrf), (PERSONAL, Some([7; 32])));
        // Decoded exactly as the dispatch gate decodes it.
        let mut taken = params.clone();
        assert_eq!(take_step_up(&mut taken), csrf);
        // … and authorized + audited by the same core as the main gate.
        let target = Target { method, category, agent: &agent };
        assert_eq!(authorize_break_glass(&h.db, Some(&proof(now - 60)), &h.admin, csrf.as_ref(), now, &target, &h.alice),
            Ok(()));
        // A non-Admin or a visible scope never reaches it.
        assert_eq!(list_break_glass(&hidden, false, &params), None);
        assert_eq!(list_break_glass(&hidden, true, &json!({"agent_id": SHARED})), None);
    }
    let rows = h.db.query_audit_log(Some(&h.admin), Some(READ_ACTION), 50).unwrap();
    let methods: BTreeSet<String> = rows.iter().map(|row| {
        let detail: Value = serde_json::from_str(row.detail.as_deref().unwrap()).unwrap();
        assert_eq!(detail["employee"], json!(PERSONAL));
        detail["method"].as_str().unwrap().to_owned()
    }).collect();
    assert_eq!(methods, ["mail.list".to_string(), "mail.outbox".to_string(), "plans.list".to_string()].into());
}

#[test]
fn p1e_discovery_and_task_control_are_gated() {
    for method in ["discovery.tree", "discovery.artifact", "discovery.cancel"] {
        let scope = id_scoped(method).unwrap_or_else(|| panic!("{method}"));
        assert_eq!((scope.key, scope.kind), ("run_id", IdKind::DiscoveryRun));
    }
    assert_eq!(agent_scoped("discovery.list"), Some(DataCategory::Work));
    for method in ["tasks.steer", "tasks.steering", "tasks.stop", "tasks.stop_status", "tasks.review_snapshot",
        "tasks.review_accept"] {
        assert!(task_scoped(method), "{method}");
    }
    assert_eq!(agent_scoped("skills.archived"), Some(DataCategory::AgentFiles));
    let h = home();
    assert_eq!(id_employee(h.path(), IdKind::DiscoveryRun, "missing"), Ok(None));
}

#[test]
fn p1e_external_keys_never_carry_an_ephemeral_id() {
    use super::super::mcp_keys_rpc::{EXTERNAL_EPHEMERAL_ID, external_client_id_refusal};
    for id in ["eph-0123456789ab", "EPH-0123456789ab", " eph-x", "eph-"] {
        assert_eq!(external_client_id_refusal(true, id), Some(EXTERNAL_EPHEMERAL_ID), "{id}");
        assert_eq!(external_client_id_refusal(false, id), None, "{id}: not an external key");
    }
    for id in ["crm-client", "ephemeral-report", "team"] {
        assert_eq!(external_client_id_refusal(true, id), None, "{id}");
    }
}

// ── 6. dashboard dispatch coverage (every arm) ───────────────────────────

const DISPATCH_SOURCES: &[(&str, &str)] = &[
    ("dispatch_core.rs", include_str!("dispatch_core.rs")),
    ("dispatch_org.rs", include_str!("dispatch_org.rs")),
    ("dispatch_knowledge.rs", include_str!("dispatch_knowledge.rs")),
    ("dispatch_system.rs", include_str!("dispatch_system.rs")),
    ("dispatch_ops.rs", include_str!("dispatch_ops.rs")),
    ("dispatch_kb.rs", include_str!("dispatch_kb.rs")),
];

/// Parameter keys that name an employee.
const EMPLOYEE_KEYS: &[&str] = &["agent_id", "agent", "agent_name", "from_agent", "to_agent", "assigned_to",
    "target_agent", "target_agent_id", "employee", "assignee"];

/// Ungated arms that take request parameters naming (or possibly naming)
/// an employee, method by method, each with its reason. Required for every
/// such arm in a namespace that also has a gated method.
const EMPLOYEE_ARMS: &[(&str, &str)] = &[
    ("agents.list", "metadata listing (name, role, status, avatar); Admins get metadata, private data is gated per method"),
    ("agents.callable", "who the caller may call (names only): anything listed is callable"),
    ("agents.health", "health counters of employees the caller has Viewer access to (metadata)"),
    ("agents.status", "runtime status metadata only (state, model, activity counters)"),
    ("agents.create", "creates a new employee; names no existing one"),
    ("agents.pause", "operational control under Manager + Operator binding; audited for another person's Personal employee"),
    ("agents.resume", "operational control under Manager + Operator binding; audited for another person's Personal employee"),
    ("agents.tool_pruning_status", "which tools are pruned (metadata); the restore is a gated config write"),
    ("agents.avatar", "the avatar image only (also shown in every listing)"),
    ("agents.google_identity_get", "binding metadata (address, status); mailbox content is gated by mail.*"),
    ("agents.primary_owner_set", "P1f follow-up: who answers for the employee (Google primary owner) vs. its Personal owner"),
    ("agents.permission_history", "permission-change history (audit metadata)"),
    ("presets.status", "the name of the bound preset only"),
    ("tasks.skill_assign", "the server-picked runner is never a hidden Personal employee (dropped before any refusal)"),
    ("memory.forget_batch_retry", "P1f follow-up: Admin-decider retry of an approved forget (confirm no memory content)"),
    ("memory.forget_receipts_list", "P1f follow-up: Admin-decider forget receipts (confirm no memory content)"),
    ("memory.forget_receipts_get", "P1f follow-up: Admin-decider forget receipts (confirm no memory content)"),
    ("agent_files.withdraw", "the requester (or an Admin) withdraws a pending change; reads no file"),
    ("chat.sessions.history", "gated in the handler on the session's employee (personal_data_gate)"),
    ("runs.get", "gated in the handler on the run's employee (personal_data_gate)"),
    ("runs.subscribe", "metadata-only run authorization; one refusal text for unknown and refused runs"),
    ("runs.unsubscribe", "metadata-only run authorization; one refusal text for unknown and refused runs"),
    ("plans.list", "optional scope: filtered, and gated through the audited step-up, in the arm (p1d_list_hidden)"),
    ("mail.list", "optional scope: filtered, and gated through the audited step-up, in the arm (p1d_list_hidden)"),
    ("mail.outbox", "optional scope: filtered, and gated through the audited step-up, in the arm (p1d_list_hidden)"),
    ("mail.decide", "decision gate (personal_decide_gate): owner, or an audited Admin step-up"),
    ("fork.list", "hides other Personal employees' forks in its handler"),
    ("skills.recent", "recently used skill names (metadata)"),
    ("skills.list", "installed skill names and switches (metadata); content is gated (skills.content)"),
    ("skills.search", "skill hub search; no employee data"),
    ("skills.vet", "security vet of a skill package; no employee data"),
    ("skills.install", "P1f follow-up: Admin hub install (confirm it never targets a Personal employee's layer)"),
    ("skills.install_request", "files a request for an Admin decision; installs nothing"),
    ("skills.leaderboard", "usage counts per skill (metadata)"),
    ("skills.custom_create", "P1f follow-up: the caller's custom-skill drafts"),
    ("skills.custom_generate", "P1f follow-up: the caller's custom-skill drafts"),
    ("skills.custom_update", "P1f follow-up: the caller's custom-skill drafts"),
    ("skills.custom_submit", "P1f follow-up: the caller's custom-skill drafts"),
    ("skills.custom_retire", "P1f follow-up: the caller's custom-skill drafts"),
    ("digest.latest", "drops another person's Personal employees in the handler, Admins included (P1e)"),
    ("discovery.catalog", "the evaluator catalog for an assignee; no run data"),
];

/// Namespaces (no gated method in them) whose arms may name an employee,
/// with the reviewed rule — or an explicit P1f follow-up.
const EMPLOYEE_NAMESPACES: &[(&str, &str)] = &[
    ("autonomy", "P1f follow-up: pause / resume background autonomy (control, reads no data; audit like agents.pause)"),
    ("templates", "Admin template library: `agent` names a template role or a new employee, never an existing one's data"),
    ("redaction", "P1f follow-up: Admin redaction policy, model and audit counters (confirm recent_audit has no content)"),
    ("identity", "P1f follow-up: channel-user identity mapping"),
    ("skill_synthesis", "P1f follow-up: Admin pipeline config; `target_agent` names where synthesized drafts land"),
    ("wiki_scope", "P1f follow-up: Admin wiki-scope policy"),
    ("channels", "Admin channel configuration: `agent` names the employee a channel routes to (routing metadata)"),
    ("accounts", "P1f follow-up: provider accounts and budget summary"),
    ("users", "people, bindings and team views; team grants skip Personal employees in SQL (P1). P1f follow-up: binding writes"),
    ("me", "the caller's own Google connection and consents"),
    ("dashboard", "P1f follow-up: dashboard layout and widget catalog"),
    ("widgets", "P1f follow-up: custom widgets may query employee data"),
    ("departments", "department names and membership (org metadata)"),
    ("org", "P1f follow-up: org placement proposals naming an employee (reports_to)"),
    ("mcp", "P1f follow-up: Admin MCP server wiring per employee (a config write on a Personal employee)"),
    ("google", "Admin Google Workspace integration and delegation keys; per-employee identity is agents.google_identity_*"),
    ("responsibilities", "P1f follow-up: an employee's continuous responsibilities"),
    ("workflow_drafts", "P1f follow-up: workflow drafts naming an employee"),
    ("workflow_runs", "P1f follow-up: workflow runs naming an employee"),
    ("assign_schedules", "P1f follow-up: scheduled assignments (the fire-time delegation is gated)"),
    ("notify", "P1f follow-up: notification statistics per employee"),
    ("migrate", "Admin migration scan / apply of imported agents (host authority)"),
    ("approvals", "another person's Personal cards are metadata only in the handler; deciding is personal_decide_gate"),
    ("topology", "P1f follow-up: approval topology filtered by employee"),
    ("takeover", "P1f follow-up: takeover list"),
    ("install_requests", "requests filed by people for an Admin decision"),
    ("budget", "P1f follow-up: budget incidents per employee"),
    ("autopilot", "P1f follow-up: autopilot rules and breakers naming employees"),
    ("shared_wiki", "the shared wiki, never an employee's own pages"),
    ("experts", "P1f follow-up: Admin expert packs installed into employees"),
    ("reminders", "P1f follow-up: an employee's reminders"),
    ("cron", "P1f follow-up: an employee's cron routines (binding-filtered; Admin sees all)"),
    ("system", "P1f follow-up: system status, doctor, update and migrate bundle"),
    ("finetune", "P1f follow-up: datasets built from conversations"),
    ("compliance", "P1f follow-up: compliance report contents"),
    ("credentials", "P1f follow-up: credential audit rows"),
    ("data_exports", "P1f follow-up: data exports"),
    ("delegation", "P1f follow-up: delegation policy"),
    ("security", "P1f follow-up: security audit log and credential inventory"),
    ("secaudit", "P1f follow-up: security audit reports"),
    ("logs", "P1f follow-up: gateway log stream"),
    ("audit", "P1f follow-up: unified audit, evolution and reliability views by employee"),
    ("analytics", "P1f follow-up: conversation analytics by employee"),
    ("heartbeat", "P1f follow-up: heartbeat status / trigger of an employee"),
    ("evolution", "P1f follow-up: evolution status and telemetry per employee"),
    ("forward", "P1f follow-up: prediction ledger by employee"),
    ("belief", "P1f follow-up: belief ledger by employee"),
    ("cost", "cost and token metadata per employee (Admins get metadata)"),
    ("db_sources", "P1f follow-up: data-source grants per employee"),
    ("odoo", "P1f follow-up: per-employee Odoo configuration"),
    ("ticks", "P1f follow-up: tick sources"),
    ("tick", "P1f follow-up: tick sources"),
    ("channel_ingress", "P1f follow-up: channel ingress inspection"),
    ("computer_profiles", "P1f follow-up: computer-use profiles of an employee"),
    ("computer_sessions", "P1f follow-up: computer-use sessions of an employee"),
    ("computer_workspaces", "P1f follow-up: computer-use workspaces of an employee"),
    ("os", "P1f follow-up: OS gate events naming an employee"),
    ("browser", "P1f follow-up: browser audit log"),
    ("marketplace", "P1f follow-up: marketplace install into an employee"),
    ("knowledge", "knowledge-base ACLs are their own authority; agent.collections needs Owner binding (Admin: metadata)"),
];

/// Namespaces that must never name an employee: the scan fails if any arm
/// (or the handler it calls) reads an employee key.
const NO_EMPLOYEE_NAMESPACES: &[(&str, &str)] = &[
    ("connect", "connection handshake"),
    ("ping", "liveness"),
    ("hello-ok", "connection handshake"),
    ("tools", "static tool catalogs"),
    ("voice", "local TTS model install / status"),
    ("inference", "inference provider settings and local model serving"),
    ("mcp_oauth_grants", "OAuth grants of MCP clients"),
    ("mcp_keys", "MCP API keys (external ids may not be eph-…)"),
    ("killswitch", "global kill switch"),
    ("license", "license status and activation"),
    ("mfa", "the caller's own second factor"),
    ("invitation", "invitation links"),
    ("onboarding", "onboarding state"),
    ("auth", "CLI login of a provider account"),
    ("gallery", "static gallery"),
    ("models", "model catalog"),
    ("localmodels", "local model downloads"),
    ("runtime", "runtime detection / install"),
    ("config", "raw config.toml (Admin)"),
    ("task_forward_model", "forward-model settings"),
    ("growth", "gamification counters"),
    ("about", "product information"),
    ("distributor", "distributor licences"),
    ("billing", "billing plan and usage"),
    ("device", "appliance device controls"),
    ("network", "appliance network"),
    ("maintenance", "maintenance mode"),
];

/// Ratchet: the number of P1f follow-ups may only go down.
const FOLLOW_UP_CEILING: usize = 58;

struct Arm {
    methods: Vec<String>,
    body: String,
}

/// The method names of a `match method` arm head, and whether the pattern
/// is complete on this line. `None` = not an arm head.
fn arm_head(text: &str, continuation: bool) -> Option<(Vec<String>, bool)> {
    let mut rest = text;
    if continuation {
        rest = rest.strip_prefix('|')?.trim_start();
    }
    let mut methods = Vec::new();
    loop {
        let quoted = rest.strip_prefix('"')?;
        let end = quoted.find('"')?;
        let name = &quoted[..end];
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c)) {
            return None;
        }
        methods.push(name.to_owned());
        rest = quoted[end + 1..].trim_start();
        if rest.is_empty() {
            return Some((methods, false));
        }
        if rest.starts_with("=>") || rest.starts_with("if ") {
            return Some((methods, true));
        }
        rest = rest.strip_prefix('|')?.trim_start();
        if rest.is_empty() {
            return Some((methods, false));
        }
    }
}

/// Every top-level arm (12-space indent) of the `match method` blocks.
fn dispatch_arms(source: &str) -> Vec<Arm> {
    let mut arms = Vec::new();
    let mut current: Option<Arm> = None;
    let mut in_pattern = false;
    for line in source.lines() {
        let text = line.trim_start();
        if text.is_empty() || text.starts_with("//") {
            continue;
        }
        let indent = line.len() - text.len();
        if indent < 12 {
            arms.extend(current.take());
            in_pattern = false;
            continue;
        }
        if indent == 12 {
            if let Some((methods, complete)) = arm_head(text, in_pattern) {
                if !in_pattern {
                    arms.extend(current.take());
                    current = Some(Arm { methods: Vec::new(), body: String::new() });
                }
                let arm = current.get_or_insert_with(|| Arm { methods: Vec::new(), body: String::new() });
                arm.methods.extend(methods);
                in_pattern = !complete;
                if let Some((_, body)) = text.split_once("=>") {
                    arm.body.push_str(body);
                    arm.body.push('\n');
                }
                continue;
            }
            if !in_pattern && text.contains("=>") {
                // `_ =>`, `other if … =>`: an arm without method names.
                arms.extend(current.take());
                current = Some(Arm { methods: Vec::new(), body: String::new() });
            }
        }
        if let Some(arm) = current.as_mut() {
            arm.body.push_str(text);
            arm.body.push('\n');
        }
    }
    arms.extend(current);
    arms
}

/// `.rs` sources of `src/handlers` (recursively), tests excluded.
fn handler_sources() -> Vec<String> {
    let mut stack = vec![std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("handlers")];
    let mut sources = Vec::new();
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if path.is_dir() {
                if name != "tests" {
                    stack.push(path);
                }
            } else if name.ends_with(".rs") && !name.contains("test") {
                sources.push(std::fs::read_to_string(&path).unwrap());
            }
        }
    }
    sources
}

/// Function name → body text (bodies of same-named functions concatenated).
fn fn_bodies(sources: &[String]) -> HashMap<String, String> {
    const ENDS: &[&str] = &["\n    }\n", "\n}\n", "\n    pub", "\n    fn ", "\n    async fn ", "\npub", "\nfn ", "\nasync fn "];
    let mut bodies: HashMap<String, String> = HashMap::new();
    for source in sources {
        let mut rest = source.as_str();
        while let Some(at) = rest.find("fn ") {
            let boundary = at == 0 || rest.as_bytes()[at - 1].is_ascii_whitespace();
            let after = &rest[at + 3..];
            let name: String = after.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
            let tail = &after[name.len()..];
            if boundary && !name.is_empty() && (tail.starts_with('(') || tail.starts_with('<')) {
                let end = ENDS.iter().filter_map(|marker| tail.find(marker)).min().unwrap_or(tail.len());
                bodies.entry(name.clone()).or_default().push_str(&tail[..end]);
            }
            rest = &after[name.len()..];
        }
    }
    bodies
}

/// `self.<name>(` calls of an arm body.
fn self_calls(body: &str) -> Vec<String> {
    body.match_indices("self.")
        .filter_map(|(at, _)| {
            let after = &body[at + 5..];
            let name: String = after.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
            after[name.len()..].starts_with('(').then_some(name)
        })
        .collect()
}

fn arm_names_employee(body: &str) -> bool {
    ["check_agent!(", "check_agent_filter!(", "check_agent_named!(", "check_task_board_filter!("]
        .iter()
        .any(|needle| body.contains(needle))
        || EMPLOYEE_KEYS.iter().any(|key| body.contains(&format!("\"{key}\"")))
}

fn handler_names_employee(body: &str) -> bool {
    body.contains("extract_and_check_agent(")
        || EMPLOYEE_KEYS.iter().any(|key| body.contains(&format!(".get(\"{key}\")")) || body.contains(&format!(", \"{key}\")")))
}

fn namespace(method: &str) -> &str {
    method.split('.').next().unwrap_or(method)
}

/// Gated before the handler by the P1–P1e dispatch gates.
fn gated(method: &str) -> bool {
    agent_scoped(method).is_some()
        || task_scoped(method)
        || id_scoped(method).is_some()
        || write_scoped(method).is_some()
        || nested_agent_scoped(method).is_some()
        || items_write_scoped(method).is_some()
        || delegation_target_key(method).is_some()
}

#[test]
fn p1e_every_dashboard_arm_is_gated_or_allowlisted() {
    let bodies = fn_bodies(&handler_sources());
    // method → (takes request params, names an employee in the arm or in a
    // handler it calls).
    let mut found: BTreeMap<String, (bool, bool)> = BTreeMap::new();
    for (_, source) in DISPATCH_SOURCES {
        for arm in dispatch_arms(source) {
            let takes = arm.body.contains("params");
            let names = arm_names_employee(&arm.body)
                || self_calls(&arm.body).iter().any(|call| bodies.get(call).is_some_and(|b| handler_names_employee(b)));
            for method in arm.methods {
                let entry = found.entry(method).or_default();
                entry.0 |= takes;
                entry.1 |= names;
            }
        }
    }
    // The scan itself works.
    assert!(found.len() >= 450, "only {} arms found", found.len());
    for method in ["memory.search", "agents.update", "digest.latest", "skills.set_enabled", "device.power",
        "knowledge.limits", "mfa.status", "responsibilities.list"] {
        assert!(found.contains_key(method), "{method} not found by the scan");
    }
    // Handler-level detection: these arms never spell `agent_id`, their
    // handlers read it.
    for method in ["agents.set_avatar", "agents.remove", "agents.tool_pruning_status"] {
        assert!(found[method].1, "{method}: the handler reads an employee id");
    }
    let mixed: BTreeSet<&str> = found.keys().filter(|m| gated(m)).map(|m| namespace(m)).collect();
    let in_table = |table: &[(&str, &str)], ns: &str| table.iter().any(|(name, _)| *name == ns);
    let mut missing = Vec::new();
    for (method, (takes, names)) in &found {
        if gated(method) || !takes || EMPLOYEE_ARMS.iter().any(|(name, _)| name == method) {
            continue;
        }
        let ns = namespace(method);
        if mixed.contains(ns) {
            missing.push(format!("{method} (namespace with gated methods: list it in EMPLOYEE_ARMS or gate it)"));
        } else if in_table(EMPLOYEE_NAMESPACES, ns) || (!names && in_table(NO_EMPLOYEE_NAMESPACES, ns)) {
            continue;
        } else if *names {
            missing.push(format!("{method} (names an employee)"));
        } else {
            missing.push(format!("{method} (unclassified namespace)"));
        }
    }
    assert!(missing.is_empty(), "dispatch arms neither gated nor allowlisted:\n{}", missing.join("\n"));
    // The tables stay honest.
    for (method, reason) in EMPLOYEE_ARMS {
        assert!(!reason.is_empty(), "{method}");
        assert!(found.contains_key(*method), "stale EMPLOYEE_ARMS entry {method}");
        assert!(!gated(method), "{method} is gated; drop it from EMPLOYEE_ARMS");
    }
    for (ns, reason) in EMPLOYEE_NAMESPACES.iter().chain(NO_EMPLOYEE_NAMESPACES) {
        assert!(!reason.is_empty(), "{ns}");
        assert!(found.keys().any(|m| namespace(m) == *ns), "stale namespace entry {ns}");
        assert!(!mixed.contains(ns), "{ns} has gated methods: list its other arms method by method");
        assert!(!(in_table(EMPLOYEE_NAMESPACES, ns) && in_table(NO_EMPLOYEE_NAMESPACES, ns)), "{ns} in both tables");
    }
    let follow_ups = EMPLOYEE_ARMS.iter().chain(EMPLOYEE_NAMESPACES).filter(|(_, r)| r.starts_with("P1f follow-up")).count();
    assert!(follow_ups <= FOLLOW_UP_CEILING, "{follow_ups} P1f follow-ups; the ceiling only goes down");
}

#[test]
fn p1e_dispatch_scan_parser_reads_arm_heads_only() {
    assert_eq!(arm_head(r#""a.b" | "a.c" => x,"#, false), Some((vec!["a.b".into(), "a.c".into()], true)));
    assert_eq!(arm_head(r#""a.b""#, false), Some((vec!["a.b".into()], false)));
    assert_eq!(arm_head(r#"| "a.d" => {"#, true), Some((vec!["a.d".into()], true)));
    assert_eq!(arm_head(r#""digest.feedback" if params.get("x").is_some() => {"#, false),
        Some((vec!["digest.feedback".into()], true)));
    // JSON keys and string arguments are not arm heads.
    assert_eq!(arm_head(r#""collection": outcome.collection,"#, false), None);
    assert_eq!(arm_head(r#""","#, false), None);
    let source = "        match method {\n            \"x.a\" => self.handle_a(params),\n            \"x.b\"\n            | \"x.c\" => {\n                self.handle_b()\n            }\n            _ => none(params),\n        }\n    fn helper() { params.get(\"agent_id\") }\n";
    let arms = dispatch_arms(source);
    let methods: Vec<Vec<String>> = arms.iter().map(|arm| arm.methods.clone()).collect();
    assert_eq!(methods, [vec!["x.a".to_string()], vec!["x.b".to_string(), "x.c".to_string()], vec![]]);
    assert!(arms[0].body.contains("params") && !arms[1].body.contains("params"));
    assert!(!arms.iter().any(|arm| arm.body.contains("agent_id")), "code after the match is no arm");
    assert_eq!(self_calls("boxed_frame(|| self.handle_a(params, ctx)).await; self.home_dir"), ["handle_a"]);
    let bodies = fn_bodies(&["impl X {\n    pub fn handle_a(&self, params: Value) {\n        params.get(\"agent_id\");\n    }\n    fn other(&self) {}\n}\n".to_string()]);
    assert!(handler_names_employee(&bodies["handle_a"]));
    assert!(!handler_names_employee(&bodies["other"]));
}
