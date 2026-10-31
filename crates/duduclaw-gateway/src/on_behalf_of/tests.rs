use std::path::Path;

use duduclaw_auth::{AccessLevel, UserDb, UserRole};
use duduclaw_core::{
    DelegationDenyReason, DelegationPolicy, HumanPrincipal, HumanPrincipalMode, HumanRight,
};

use super::*;

/// mgr (Manager) → emp1 (sales Owner, LINE `U_emp1`); viewer (hr Viewer);
/// admin. `public` employee has `[channels] public_trigger = true`.
struct Org {
    mgr: String,
    emp1: String,
    viewer: String,
    admin: String,
}

fn org(home: &Path) -> Org {
    crate::team_edition::record(home, duduclaw_core::EditionProfile::Enterprise);
    let db = UserDb::new(&home.join("users.db")).unwrap();
    let mk = |name: &str, role| {
        db.create_user(&format!("{name}@test.local"), name, "TeamPass-12345", role)
            .unwrap()
            .id
    };
    let mgr = mk("mgr", UserRole::Manager);
    let emp1 = mk("emp1", UserRole::Employee);
    let viewer = mk("viewer", UserRole::Employee);
    let admin = mk("admin", UserRole::Admin);
    db.set_manager(&emp1, Some(&mgr)).unwrap();
    db.bind_agent(&emp1, "sales", AccessLevel::Owner).unwrap();
    db.bind_agent(&viewer, "hr", AccessLevel::Viewer).unwrap();
    db.bind_channel_identity(&emp1, "line", "U_emp1", true).unwrap();
    for (name, public) in [("sales", false), ("hr", false), ("finance", false), ("public", true)] {
        let dir = home.join("agents").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let channels = if public { "\n[channels]\npublic_trigger = true\n" } else { "" };
        std::fs::write(dir.join("agent.toml"), format!("[agent]\nname = \"{name}\"\n{channels}")).unwrap();
    }
    Org { mgr, emp1, viewer, admin }
}

fn user(id: &str) -> HumanPrincipal {
    HumanPrincipal::user(id).unwrap()
}

fn unmapped() -> HumanPrincipal {
    HumanPrincipal::unmapped("line", "0123456789abcdef").unwrap()
}

fn enforce(home: &Path, p: Result<Option<HumanPrincipal>, duduclaw_core::PrincipalRejection>, target: &str, path: &str) -> bool {
    access::check(home, HumanPrincipalMode::Enforce, &p, "lead-bot", target, path, DelegationPolicy::Department).is_ok()
}

fn audit_rows(home: &Path, event: &str) -> Vec<serde_json::Value> {
    std::fs::read_to_string(home.join("security_audit.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v["event_type"] == event)
        .collect()
}

// ── resolving the person of a turn ──────────────────────────────────────

#[test]
fn a_turn_names_its_account_or_a_keyed_digest_never_the_raw_id() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let dash = RequesterIdentity::DashboardUser { user_id: "u-1".into() };
    assert_eq!(principal_for_requester(h, &dash).as_deref(), Some("user:u-1"));
    let bound = RequesterIdentity::ChannelUser {
        channel: "line".into(),
        chat: "C1".into(),
        sender: "U_emp1".into(),
        user_id: Some("u-2".into()),
    };
    assert_eq!(principal_for_requester(h, &bound).as_deref(), Some("user:u-2"));
    let unbound = RequesterIdentity::ChannelUser {
        channel: "line".into(),
        chat: "C1".into(),
        sender: "U_stranger".into(),
        user_id: None,
    };
    let p = principal_for_requester(h, &unbound).unwrap();
    assert!(p.starts_with("unmapped:line:"), "{p}");
    assert!(!p.contains("U_stranger"), "{p}");
    assert!(HumanPrincipal::parse(&p).is_ok());
    // A public visitor has no sender; the conversation names them.
    let visitor = RequesterIdentity::ChannelUser {
        channel: "widget".into(),
        chat: "v-9".into(),
        sender: String::new(),
        user_id: None,
    };
    assert!(principal_for_requester(h, &visitor).unwrap().starts_with("unmapped:widget:"));
    let delegated = RequesterIdentity::System { kind: "dispatch".into(), on_behalf_of: Some("u-3".into()) };
    assert_eq!(principal_for_requester(h, &delegated).as_deref(), Some("user:u-3"));
    let system = RequesterIdentity::System { kind: "cron".into(), on_behalf_of: None };
    assert!(principal_for_requester(h, &system).is_none());
    assert!(principal_for_requester(h, &RequesterIdentity::Unknown).is_none());
}

#[test]
fn system_sender_names_never_become_a_principal() {
    let home = tempfile::tempdir().unwrap();
    for name in ["dashboard", "cron", "heartbeat", "system", "goal-loop-driver"] {
        let who = RequesterIdentity::DashboardUser { user_id: name.into() };
        assert!(principal_for_requester(home.path(), &who).is_none(), "{name}");
    }
}

// ── carrying it ─────────────────────────────────────────────────────────

#[tokio::test]
async fn the_first_writer_wins_and_spawns_carry_only_the_scope() {
    assert!(current().is_none());
    assert!(env_pairs().is_empty());
    let inner = scope_first(Some("user:a".into()), async {
        let outer = current();
        let nested = scope_first(Some("user:b".into()), async { current() }).await;
        (outer, nested, env_pairs())
    })
    .await;
    assert_eq!(inner.0.as_deref(), Some("user:a"));
    assert_eq!(inner.1.as_deref(), Some("user:a"), "a later hop never overwrites");
    assert_eq!(inner.2, vec![(ENV_ON_BEHALF_OF.to_string(), "user:a".to_string())]);
    // `None` below a value keeps the value; a value below `None` is used.
    let filled = ON_BEHALF_OF
        .scope(None, scope_first(Some("user:c".into()), async { current() }))
        .await;
    assert_eq!(filled.as_deref(), Some("user:c"));
}

#[tokio::test]
async fn the_turn_source_pairs_every_runtime_uses_include_the_principal() {
    let pairs = scope_first(Some("user:u-7".into()), async {
        crate::memory_provenance::turn_source_env_pairs()
    })
    .await;
    assert!(pairs.iter().any(|(k, v)| k == ENV_ON_BEHALF_OF && v == "user:u-7"), "{pairs:?}");
    assert!(!crate::memory_provenance::turn_source_env_pairs()
        .iter()
        .any(|(k, _)| k == ENV_ON_BEHALF_OF));
}

#[tokio::test]
async fn a_spawn_drops_an_inherited_value_and_sets_the_scoped_one() {
    let mut outside = tokio::process::Command::new("true");
    outside.env(ENV_ON_BEHALF_OF, "user:forged");
    inject_env(&mut outside);
    let removed = outside
        .as_std()
        .get_envs()
        .find(|(k, _)| *k == std::ffi::OsStr::new(ENV_ON_BEHALF_OF))
        .map(|(_, v)| v.is_none());
    assert_eq!(removed, Some(true), "outside a scope the variable is removed");
    let value = scope_first(Some("user:u-8".into()), async {
        let mut cmd = tokio::process::Command::new("true");
        inject_env(&mut cmd);
        cmd.as_std()
            .get_envs()
            .find(|(k, _)| *k == std::ffi::OsStr::new(ENV_ON_BEHALF_OF))
            .and_then(|(_, v)| v.map(|v| v.to_string_lossy().into_owned()))
    })
    .await;
    assert_eq!(value.as_deref(), Some("user:u-8"));
}

#[test]
fn stored_provenance_distinguishes_human_background_and_unknown_and_audits() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    assert_eq!(from_stored(h, Some("user:u-1"), "sales", "message_queue").as_deref(), Some("user:u-1"));
    assert!(from_stored(h, None, "sales", "message_queue").is_none());
    assert!(audit_rows(h, AUDIT_REJECTED).is_empty());
    for raw in ["user:dashboard", "admin", "", "  "] {
        let source = if raw == "user:dashboard" { "message_queue" } else { "bus_queue" };
        let carried = from_stored(h, Some(raw), "sales", source);
        assert!(carried.is_some(), "rejected stored provenance must stay present");
        assert_eq!(PrincipalState::from_wire(carried.as_deref()), PrincipalState::Unknown);
        assert!(!access::system_operation_permitted(h, carried.as_deref()));
    }
    assert_eq!(PrincipalState::from_wire(None), PrincipalState::Background);
    assert!(access::system_operation_permitted(h, None), "old rows keep background work");
    assert_eq!(PrincipalState::from_wire(Some("user:u-1")), PrincipalState::Human(user("u-1")));
    let rows = audit_rows(h, AUDIT_REJECTED);
    assert_eq!(rows.len(), 4, "{rows:?}");
    assert_eq!(rows[0]["details"]["reason"], "system_sender");
    assert_eq!(rows[1]["details"]["reason"], "missing_prefix");
    assert_eq!(rows[1]["details"]["source"], "bus_queue");
}

// ── enforce mode ────────────────────────────────────────────────────────

#[test]
fn record_mode_never_refuses() {
    let home = tempfile::tempdir().unwrap();
    let rejected = HumanPrincipal::parse("nope").map(Some);
    for p in [Ok(None), Ok(Some(unmapped())), rejected] {
        assert!(access::check(home.path(), HumanPrincipalMode::Record, &p, "a", "finance", "tasks_create", DelegationPolicy::Open).is_ok());
    }
}

#[test]
fn enforce_narrows_to_what_the_person_may_do() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let o = org(h);
    // Nobody named: no person check.
    assert!(enforce(h, Ok(None), "finance", "tasks_create"));
    // An Owner binding: call and assign.
    assert!(enforce(h, Ok(Some(user(&o.emp1))), "sales", "send_to_agent"));
    assert!(enforce(h, Ok(Some(user(&o.emp1))), "sales", "tasks_create"));
    assert!(!enforce(h, Ok(Some(user(&o.emp1))), "finance", "send_to_agent"));
    // A Viewer may talk but not hand work over.
    assert!(enforce(h, Ok(Some(user(&o.viewer))), "hr", "send_to_agent"));
    assert!(!enforce(h, Ok(Some(user(&o.viewer))), "hr", "tasks_create"));
    // A manager's team grant (min(Owner, Operator) = Operator) assigns.
    assert!(enforce(h, Ok(Some(user(&o.mgr))), "sales", "tasks_create"));
    assert!(!enforce(h, Ok(Some(user(&o.mgr))), "hr", "send_to_agent"));
    // Admin: everything.
    assert!(enforce(h, Ok(Some(user(&o.admin))), "finance", "tasks_create"));
    // Public employees are callable by anyone, assignable by nobody extra.
    assert!(enforce(h, Ok(Some(user(&o.viewer))), "public", "send_to_agent"));
    assert!(!enforce(h, Ok(Some(user(&o.viewer))), "public", "tasks_create"));
    // Unmapped: public_trigger calls only, never a hand-over.
    assert!(enforce(h, Ok(Some(unmapped())), "public", "spawn_agent"));
    assert!(!enforce(h, Ok(Some(unmapped())), "sales", "send_to_agent"));
    assert!(!enforce(h, Ok(Some(unmapped())), "public", "tasks_create"));
    // Unknown account, malformed value: denied.
    assert!(!enforce(h, Ok(Some(user("no-such-user"))), "sales", "send_to_agent"));
    assert!(!enforce(h, HumanPrincipal::parse("user:dashboard").map(Some), "sales", "send_to_agent"));
}

#[test]
fn enforce_denies_with_the_person_reason_and_fails_closed_without_users_db() {
    let home = tempfile::tempdir().unwrap();
    let err = access::check(
        home.path(),
        HumanPrincipalMode::Enforce,
        &Ok(Some(user("u-1"))),
        "lead-bot",
        "sales",
        "send_to_agent",
        DelegationPolicy::Hierarchy,
    )
    .unwrap_err();
    assert_eq!(err.reason, DelegationDenyReason::HumanPrincipalNotPermitted);
    assert_eq!(err.policy, DelegationPolicy::Hierarchy);
    assert_eq!((err.sender.as_str(), err.target.as_str()), ("lead-bot", "sales"));
    assert!(!access::principal_permits(home.path(), &user("u-1"), "sales", HumanRight::Call).unwrap());
}

#[test]
fn public_trigger_reads_only_a_valid_agent_toml() {
    let home = tempfile::tempdir().unwrap();
    org(home.path());
    assert!(access::is_public_trigger(home.path(), "public"));
    assert!(!access::is_public_trigger(home.path(), "sales"));
    assert!(!access::is_public_trigger(home.path(), "../public"));
    assert!(!access::is_public_trigger(home.path(), "ghost"));
}

// ── the C1 gate ─────────────────────────────────────────────────────────

fn tree(home: &Path, policy: &str, mode: &str) -> std::sync::Arc<tokio::sync::RwLock<duduclaw_agent::registry::AgentRegistry>> {
    for (name, reports_to) in [("lead-bot", ""), ("sales", "lead-bot"), ("finance", "lead-bot")] {
        let dir = home.join("agents").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("agent.toml"),
            format!("[agent]\nname = \"{name}\"\nrole = \"worker\"\nreports_to = \"{reports_to}\"\ndepartment = \"ops\"\n"),
        )
        .unwrap();
    }
    std::fs::write(
        home.join("config.toml"),
        format!("[delegation]\npolicy = \"{policy}\"\nhuman_principal = \"{mode}\"\n"),
    )
    .unwrap();
    std::sync::Arc::new(tokio::sync::RwLock::new(duduclaw_agent::registry::AgentRegistry::new(home.join("agents"))))
}

async fn gate(home: &Path, mode: &str, sender: &str, target: &str, obo: Option<&str>) -> bool {
    let reg = tree(home, "hierarchy", mode);
    reg.write().await.scan().await.unwrap();
    matches!(
        crate::delegation_gate::gate_bus_dispatch(home, &reg, Some(sender), None, target, "m-1", "sqlite_queue", obo).await,
        crate::delegation_gate::GateOutcome::Allow
    )
}

#[tokio::test]
async fn the_dispatch_gate_checks_the_person_after_the_tree() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let o = org(h);
    let emp1 = format!("user:{}", o.emp1);
    // record: the person is not consulted.
    assert!(gate(h, "record", "lead-bot", "finance", Some(&emp1)).await);
    // enforce: the tree allows lead-bot → finance, emp1 may not call finance.
    assert!(!gate(h, "enforce", "lead-bot", "finance", Some(&emp1)).await);
    assert!(gate(h, "enforce", "lead-bot", "sales", Some(&emp1)).await);
    assert!(gate(h, "enforce", "lead-bot", "finance", None).await);
    // A malformed carried value denies in enforce mode.
    assert!(!gate(h, "enforce", "lead-bot", "sales", Some("dashboard")).await);
    // System senders (goal rounds, heartbeat) are not re-judged by person.
    assert!(gate(h, "enforce", "goal-loop-driver", "finance", Some(&emp1)).await);
    // Each refusal is audited with the reason and the carried value.
    let audit = audit_rows(h, "delegation_denied");
    assert_eq!(audit.len(), 2, "{audit:?}");
    assert!(audit.iter().all(|r| r["details"]["reason"] == "human_principal_not_permitted"));
    assert_eq!(audit[0]["details"]["on_behalf_of"], emp1.as_str());
    assert_eq!(audit[1]["details"]["on_behalf_of"], "dashboard");
}

// ── approvals ───────────────────────────────────────────────────────────

#[tokio::test]
async fn an_unreadable_dispatch_config_keeps_human_enforcement() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let o = org(h);
    let registry = tree(h, "hierarchy", "enforce");
    registry.write().await.scan().await.unwrap();
    for content in ["[delegation", "delegation = 'bad'"] {
        std::fs::write(h.join("config.toml"), content).unwrap();
        let result = crate::delegation_gate::gate_bus_dispatch(
            h, &registry, Some("lead-bot"), None, "finance", "invalid-config",
            "sqlite_queue", Some(&format!("user:{}", o.emp1)),
        ).await;
        assert!(matches!(result, crate::delegation_gate::GateOutcome::Deny(_)), "{result:?}");
    }
}

#[test]
fn self_approval_is_refused_only_when_turned_on() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let me = Some("user:u-1");
    assert!(!self_approval::enabled(h));
    assert!(!self_approval::refuses(h, me, me, true));
    std::fs::write(h.join("config.toml"), "[approval]\nforbid_self_approval = true\n").unwrap();
    assert!(self_approval::refuses(h, me, me, true));
    assert!(!self_approval::refuses(h, me, me, false), "denying stays possible");
    assert!(!self_approval::refuses(h, me, Some("user:u-2"), true));
    assert!(!self_approval::refuses(h, None, me, true));
    std::fs::write(h.join("config.toml"), "[approval\n").unwrap();
    assert!(self_approval::enabled(h), "an unreadable config turns it on");
    std::fs::write(h.join("config.toml"), "[approval]\nforbid_self_approval = false\n").unwrap();
    assert!(!self_approval::enabled(h));
    for raw in ["approval = 'bad'", "approval = []", "approval = false"] {
        std::fs::write(h.join("config.toml"), raw).unwrap();
        assert!(self_approval::enabled(h), "{raw}");
    }
}

#[tokio::test]
async fn approvals_raised_in_a_run_carry_its_person_and_the_card_names_them() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let o = org(h);
    let broker = crate::approval::ApprovalBroker::open(h).unwrap();
    let principal = format!("user:{}", o.emp1);
    let id = scope_first(Some(principal.clone()), async {
        broker.request("sales", "mcp_tool", "send the quote", serde_json::json!({}), 600).await
    })
    .await
    .unwrap();
    let rec = broker.get(&id).await.unwrap().unwrap();
    assert_eq!(rec.on_behalf_of.as_deref(), Some(principal.as_str()));
    // Survives a reopen (column, not memory).
    let reopened = crate::approval::ApprovalBroker::open(h).unwrap().get(&id).await.unwrap().unwrap();
    assert_eq!(reopened.on_behalf_of, rec.on_behalf_of);
    let label = label::display_label(h, &principal);
    assert_eq!(label, "emp1");
    let body = crate::approval_notify::approval_body_with(&rec, false, Some(&label));
    assert!(body.contains("由 emp1 交辦"), "{body}");
    assert!(!body.contains(&o.emp1), "the card never shows the account id");
    let plain = broker.request("sales", "mcp_tool", "own work", serde_json::json!({}), 600).await.unwrap();
    assert!(broker.get(&plain).await.unwrap().unwrap().on_behalf_of.is_none());
}

#[test]
fn labels_fall_back_without_exposing_platform_ids() {
    let home = tempfile::tempdir().unwrap();
    assert_eq!(label::display_label(home.path(), "user:abcdefghijkl"), "user:abcdefgh");
    assert_eq!(
        label::display_label(home.path(), "unmapped:line:0123456789abcdef"),
        "未綁定帳號的 line 使用者"
    );
    assert!(label::person_name(home.path(), "unmapped:line:0123456789abcdef").is_none());
}

// ── the queue row ───────────────────────────────────────────────────────

fn queue_msg(id: &str, on_behalf_of: Option<&str>) -> crate::message_queue::QueueMessage {
    crate::message_queue::QueueMessage {
        id: id.into(),
        sender: "sales".into(),
        target: "finance".into(),
        payload: "hi".into(),
        status: crate::message_queue::MessageStatus::Pending,
        retry_count: 0,
        delegation_depth: 1,
        origin_agent: Some("sales".into()),
        sender_agent: Some("sales".into()),
        error: None,
        response: None,
        created_at: chrono::Utc::now().to_rfc3339(),
        acked_at: None,
        completed_at: None,
        reply_channel: None,
        turn_id: None,
        session_id: None,
        upstream_unknown: false,
        lane: None,
        on_behalf_of: on_behalf_of.map(str::to_string),
    }
}

#[tokio::test]
async fn the_queue_column_round_trips_and_old_rows_read_none() {
    let home = tempfile::tempdir().unwrap();
    let q = crate::message_queue::MessageQueue::open(home.path()).unwrap();
    q.enqueue(&queue_msg("m-obo", Some("user:u-1"))).await.unwrap();
    q.enqueue(&queue_msg("m-plain", None)).await.unwrap();
    assert_eq!(q.get_by_id("m-obo").await.unwrap().unwrap().on_behalf_of.as_deref(), Some("user:u-1"));
    assert!(q.get_by_id("m-plain").await.unwrap().unwrap().on_behalf_of.is_none());
    let pending = q.pending_messages(10).await.unwrap();
    assert!(pending.iter().any(|m| m.on_behalf_of.as_deref() == Some("user:u-1")));
}

#[tokio::test]
async fn corrupt_queue_security_columns_are_skipped_before_dispatch_can_acknowledge() {
    for column in ["on_behalf_of", "lane"] {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"),
            "[delegation]\npolicy = 'open'\nhuman_principal = 'enforce'\n").unwrap();
        let q = std::sync::Arc::new(crate::message_queue::MessageQueue::open(home.path()).unwrap());
        q.enqueue(&queue_msg("corrupt", Some("user:u-1"))).await.unwrap();
        let conn = rusqlite::Connection::open(home.path().join("message_queue.db")).unwrap();
        // These column names are fixed test cases, never request input.
        conn.execute(&format!("UPDATE message_queue SET {column} = X'FF' WHERE id = 'corrupt'"), []).unwrap();
        assert!(q.get_by_id("corrupt").await.is_err(), "{column}");
        // Batch readers log malformed rows and omit them so valid work can
        // proceed. They must never return a row with its security stamp lost.
        assert!(q.pending_messages(10).await.unwrap().is_empty(), "{column}");
        let registry = std::sync::Arc::new(tokio::sync::RwLock::new(
            duduclaw_agent::registry::AgentRegistry::new(home.path().join("agents"))));
        assert!(crate::dispatcher::poll_and_dispatch_sqlite(&q, home.path(), &registry, None, None)
            .await.is_ok(), "{column}");
        let state: (String, Option<String>) = conn.query_row(
            "SELECT status, acked_at FROM message_queue WHERE id = 'corrupt'", [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert_eq!(state, ("pending".into(), None), "{column}: no dispatch acknowledgment");
    }
}
