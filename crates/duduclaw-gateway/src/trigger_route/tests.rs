use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use duduclaw_agent::registry::AgentRegistry;
use duduclaw_auth::UserRole;

use super::access::{Sender, SenderAccess};
use super::list;
use super::pending::{self, PendingKey};
use super::turn::{ChannelTurn, TurnDecision, decision_for_route};
use super::*;

pub(crate) fn write_agent(
    root: &Path,
    id: &str,
    display: &str,
    trigger: &str,
    role: &str,
    extra: &str,
) {
    let toml = format!(
        r#"[agent]
name = "{id}"
display_name = "{display}"
role = "{role}"
status = "active"
trigger = "{trigger}"
reports_to = ""
icon = "🐾"

[model]
preferred = "claude-haiku-4-5"
fallback = "claude-haiku-4-5"
account_pool = ["main"]
api_mode = "cli"

[container]
timeout_ms = 60000
max_concurrent = 1
readonly_project = true
additional_mounts = []

[heartbeat]
enabled = false
interval_seconds = 3600
max_concurrent_runs = 1
cron = ""

[budget]
monthly_limit_cents = 500
warn_threshold_percent = 80
hard_stop = false

[permissions]
can_create_agents = false
can_send_cross_agent = false
can_modify_own_skills = false
can_modify_own_soul = false
can_schedule_tasks = false
allowed_channels = []

[evolution]
skill_auto_activate = false
skill_security_scan = true
gvu_enabled = false
cognitive_memory = false
max_silence_hours = 168.0
max_gvu_generations = 0
observation_period_hours = 24.0
skill_token_budget = 500
max_active_skills = 2
{extra}
"#
    );
    let dir = root.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("agent.toml"), toml).unwrap();
}

pub(crate) async fn registry(public_sales: bool) -> (tempfile::TempDir, AgentRegistry) {
    let tmp = tempfile::TempDir::new().unwrap();
    let agents = tmp.path().join("agents");
    write_agent(&agents, "dudu", "嘟嘟總機", "@嘟嘟總機", "main", "");
    write_agent(
        &agents,
        "finance",
        "財務小幫手",
        "@財務小幫手",
        "specialist",
        "",
    );
    let extra = if public_sales {
        "\n[channels]\npublic_trigger = true\n"
    } else {
        ""
    };
    write_agent(
        &agents,
        "sales-rep",
        "業務專員",
        "@sales-rep",
        "specialist",
        extra,
    );
    let mut reg = AgentRegistry::new(agents);
    reg.scan().await.unwrap();
    assert_eq!(reg.list().len(), 3);
    (tmp, reg)
}

fn effective(role: UserRole, bound: &[&str]) -> duduclaw_auth::EffectiveAccess {
    duduclaw_auth::EffectiveAccess {
        user_id: if role == UserRole::Admin { "a1" } else { "u1" }.into(),
        role,
        direct: bound
            .iter()
            .map(|s| (s.to_string(), duduclaw_auth::AccessLevel::Operator))
            .collect(),
        team: BTreeMap::new(),
        generation: 1,
    }
}

fn account(bound: &[&str], _channel: bool) -> SenderAccess {
    SenderAccess::from_effective(Arc::new(effective(UserRole::Employee, bound)))
}

fn admin() -> SenderAccess {
    SenderAccess::from_effective(Arc::new(effective(UserRole::Admin, &[])))
}

#[tokio::test]
async fn webchat_account_bound_to_finance_cannot_reach_sales_by_trigger() {
    let (_t, reg) = registry(false).await;
    let acc = account(&["finance"], false);
    assert_eq!(
        decide(&reg, "@sales-rep 報價", &acc, Some("dudu")),
        TriggerRoute::Forbidden {
            agent: "sales-rep".into()
        }
    );
    assert_eq!(
        decide(&reg, "@財務小幫手 幫我算帳", &acc, Some("dudu")),
        TriggerRoute::Routed {
            agent: "finance".into(),
            bare: false
        }
    );
    // The default employee stays callable without a binding (owner
    // decision: it is reachable by every signed-in account).
    assert_eq!(
        decide(&reg, "@嘟嘟總機 你好", &acc, Some("dudu")),
        TriggerRoute::Routed {
            agent: "dudu".into(),
            bare: false
        }
    );
}

#[tokio::test]
async fn admin_may_call_everyone() {
    let (_t, reg) = registry(false).await;
    for (text, id) in [
        ("@sales-rep x", "sales-rep"),
        ("@業務專員 x", "sales-rep"),
        ("@嘟嘟總機 x", "dudu"),
    ] {
        assert_eq!(
            decide(&reg, text, &admin(), Some("dudu")),
            TriggerRoute::Routed {
                agent: id.into(),
                bare: false
            }
        );
    }
}

#[tokio::test]
async fn unmapped_channel_sender_reaches_only_default_and_public_employees() {
    let (_t, reg) = registry(false).await;
    let u = SenderAccess::unmapped();
    // An unmapped sender is not told the name belongs to an employee they
    // may not call: it reads like an unknown name (owner decision L5).
    assert_eq!(decide(&reg, "@財務小幫手 幫我", &u, Some("dudu")), TriggerRoute::None);
    assert_eq!(decide(&reg, "@不存在的人 幫我", &u, Some("dudu")), TriggerRoute::None);
    // A dashboard account still gets the explicit refusal.
    assert!(matches!(
        decide(&reg, "@財務小幫手 幫我", &account(&[], true), Some("dudu")),
        TriggerRoute::Forbidden { .. }
    ));
    assert_eq!(
        decide(&reg, "@嘟嘟總機 你好", &u, Some("dudu")),
        TriggerRoute::Routed {
            agent: "dudu".into(),
            bare: false
        }
    );
    let (_t2, reg_pub) = registry(true).await;
    assert_eq!(
        decide(&reg_pub, "@sales-rep 報價", &u, Some("dudu")),
        TriggerRoute::Routed {
            agent: "sales-rep".into(),
            bare: false
        }
    );
    // The list shows exactly what may be called.
    assert_eq!(
        callable_ids(&reg_pub, &u, Some("dudu")),
        vec!["dudu".to_string(), "sales-rep".to_string()]
    );
}

#[tokio::test]
async fn mapped_channel_account_keeps_default_and_adds_bindings() {
    let (_t, reg) = registry(false).await;
    let acc = account(&["finance"], true);
    assert_eq!(
        callable_ids(&reg, &acc, Some("dudu")),
        vec!["dudu".to_string(), "finance".to_string()]
    );
    // WebChat accounts get the same set (the default employee stays
    // reachable by everyone).
    let webchat = account(&["finance"], false);
    assert_eq!(
        callable_ids(&reg, &webchat, Some("dudu")),
        vec!["dudu".to_string(), "finance".to_string()]
    );
}

#[tokio::test]
async fn bare_mention_and_list_requests() {
    let (_t, reg) = registry(false).await;
    assert_eq!(
        decide(&reg, "@財務小幫手", &admin(), Some("dudu")),
        TriggerRoute::Routed {
            agent: "finance".into(),
            bare: true
        }
    );
    match decide(&reg, "@", &account(&["finance"], true), Some("dudu")) {
        TriggerRoute::List { candidates, .. } => {
            assert_eq!(candidates, vec!["dudu".to_string(), "finance".to_string()])
        }
        other => panic!("expected list, got {other:?}"),
    }
    match decide(&reg, "@財務 幫我", &admin(), Some("dudu")) {
        TriggerRoute::List { candidates, .. } => {
            assert_eq!(candidates, vec!["finance".to_string()])
        }
        other => panic!("expected list, got {other:?}"),
    }
    assert_eq!(
        decide(&reg, "財務小幫手 幫我", &admin(), Some("dudu")),
        TriggerRoute::None
    );
}

#[tokio::test]
async fn paused_employee_cannot_be_called() {
    let tmp = tempfile::TempDir::new().unwrap();
    let agents = tmp.path().join("agents");
    write_agent(&agents, "dudu", "嘟嘟總機", "@嘟嘟總機", "main", "");
    write_agent(
        &agents,
        "finance",
        "財務小幫手",
        "@財務小幫手",
        "specialist",
        "",
    );
    let p = agents.join("finance").join("agent.toml");
    let body = std::fs::read_to_string(&p)
        .unwrap()
        .replace("status = \"active\"", "status = \"paused\"");
    std::fs::write(&p, body).unwrap();
    let mut reg = AgentRegistry::new(agents);
    reg.scan().await.unwrap();
    assert_eq!(
        decide(&reg, "@財務小幫手 x", &admin(), Some("dudu")),
        TriggerRoute::None
    );
}

fn key(tag: &str) -> PendingKey {
    PendingKey::new(
        "test",
        &format!("conv-{tag}-{}", uuid::Uuid::new_v4()),
        "sender",
    )
}

#[test]
fn pending_addressee_is_used_once() {
    let k = key("once");
    pending::arm(&k, "finance", "e1", 10);
    assert_eq!(pending::peek(&k, 11), Some("finance".into()));
    assert_eq!(pending::claim(&k, "e2", 11), Some("finance".into()));
    // Same event recomputing its route keeps the answer (durable ingress).
    assert_eq!(pending::claim(&k, "e2", 11), Some("finance".into()));
    // The message after that is back to normal.
    assert_eq!(pending::claim(&k, "e3", 12), None);
    assert_eq!(pending::peek(&k, 12), None);
}

#[test]
fn pending_addressee_expires() {
    let k = key("ttl");
    pending::arm(&k, "finance", "e1", 10);
    pending::age_for_test(&k, pending::PENDING_TTL + Duration::from_secs(1));
    assert_eq!(pending::claim(&k, "e2", 11), None);
}

#[test]
fn an_older_event_never_consumes_or_rearms() {
    let k = key("order");
    pending::arm(&k, "finance", "e5", 50);
    // An event that arrived before the arming one, recomputed late.
    assert_eq!(pending::claim(&k, "e4", 40), None);
    pending::arm(&k, "sales-rep", "e3", 30);
    assert_eq!(pending::claim(&k, "e6", 60), Some("finance".into()));
    // Re-arming from the same origin after use changes nothing.
    pending::arm(&k, "finance", "e5", 50);
    assert_eq!(pending::claim(&k, "e7", 70), None);
}

#[test]
fn a_newer_explicit_mention_cancels() {
    let k = key("cancel");
    pending::arm(&k, "finance", "e1", 10);
    pending::cancel(&k, "e2", 20);
    assert_eq!(pending::claim(&k, "e3", 30), None);
}

#[tokio::test]
async fn bare_mention_arms_and_next_message_goes_to_that_employee_only() {
    let (t, reg) = registry(false).await;
    let conv = format!("telegram:{}", uuid::Uuid::new_v4());
    let turn = |text: &'static str, event: &'static str, seq: i64| ChannelTurn {
        channel: "telegram",
        conversation: Box::leak(conv.clone().into_boxed_str()),
        sender: "42",
        text,
        fallback: Some("dudu"),
        event,
        seq,
        quiet: false,
        pending: turn::PendingPolicy::Live,
    };
    // An admin-shaped access stands in for a mapped account here.
    let acc = admin();
    let first = decision_for_route(
        t.path(),
        &reg,
        turn("@財務小幫手", "m1", 1),
        &acc,
        TriggerRoute::Routed {
            agent: "finance".into(),
            bare: true,
        },
    );
    match first {
        TurnDecision::Reply { text, list, .. } => {
            assert!(text.contains("財務小幫手"));
            assert!(list.is_none());
        }
        other => panic!("expected the task prompt, got {other:?}"),
    }
    // The pending addressee is re-checked against the sender's access when
    // used; an unmapped sender (no users.db in this home) may not call
    // finance, so it falls back to the default employee and is consumed.
    let second = decision_for_route(
        t.path(),
        &reg,
        turn("幫我算帳", "m2", 2),
        &acc,
        TriggerRoute::None,
    );
    assert_eq!(second, TurnDecision::Default);
    let third = decision_for_route(
        t.path(),
        &reg,
        turn("再一次", "m3", 3),
        &acc,
        TriggerRoute::None,
    );
    assert_eq!(third, TurnDecision::Default);
}

#[tokio::test]
async fn pending_for_a_default_employee_routes_without_label() {
    let (t, reg) = registry(false).await;
    let conv = format!("telegram:{}", uuid::Uuid::new_v4());
    let mk = |text: &'static str, event: &'static str, seq: i64| ChannelTurn {
        channel: "telegram",
        conversation: Box::leak(conv.clone().into_boxed_str()),
        sender: "42",
        text,
        fallback: Some("dudu"),
        event,
        seq,
        quiet: false,
        pending: turn::PendingPolicy::Live,
    };
    let acc = SenderAccess::unmapped();
    let _ = decision_for_route(
        t.path(),
        &reg,
        mk("@嘟嘟總機", "m1", 1),
        &acc,
        TriggerRoute::Routed {
            agent: "dudu".into(),
            bare: true,
        },
    );
    assert_eq!(
        decision_for_route(
            t.path(),
            &reg,
            mk("你好", "m2", 2),
            &acc,
            TriggerRoute::None
        ),
        TurnDecision::Agent {
            id: "dudu".into(),
            via_trigger: false
        }
    );
}

#[tokio::test]
async fn forbidden_turn_answers_without_a_model_call_and_is_audited() {
    let (t, reg) = registry(false).await;
    let d = decision_for_route(
        t.path(),
        &reg,
        ChannelTurn {
            channel: "line",
            conversation: "line:U1",
            sender: "U1",
            text: "@財務小幫手 x",
            fallback: Some("dudu"),
            event: "e",
            seq: 1,
            quiet: false,
            pending: turn::PendingPolicy::Live,
        },
        &SenderAccess::unmapped(),
        TriggerRoute::Forbidden {
            agent: "finance".into(),
        },
    );
    assert_eq!(
        d,
        TurnDecision::Reply {
            text: FORBIDDEN_TEXT.into(),
            list: None,
            kind: turn::ReplyKind::Refusal,
        }
    );
    let audit = std::fs::read_to_string(t.path().join("security_audit.jsonl")).unwrap();
    assert!(audit.contains("trigger_route_refused"));
    assert!(
        !audit.contains("\"U1\""),
        "raw platform id must not be logged"
    );
}

#[tokio::test]
async fn line_quick_reply_payload_is_capped_and_paginated() {
    let agents: Vec<list::CallableAgent> = (0..20)
        .map(|i| list::CallableAgent {
            id: format!("a{i}"),
            display_name: format!("員工{i}號，名字很長很長很長很長很長很長"),
            icon: String::new(),
            call_text: format!("@員工{i}號"),
        })
        .collect();
    let first = list::line_list_message(&agents, 0, super::texts::Lang::ZhTw);
    let items = first["quickReply"]["items"].as_array().unwrap();
    assert_eq!(items.len(), list::LINE_QUICK_REPLY_MAX);
    assert_eq!(items[0]["action"]["type"], "message");
    assert_eq!(items[0]["action"]["text"], "@員工0號");
    assert!(
        items[0]["action"]["label"]
            .as_str()
            .unwrap()
            .chars()
            .count()
            <= 20
    );
    let more = &items[12]["action"];
    assert_eq!(more["type"], "postback");
    assert_eq!(
        list::parse_line_page(more["data"].as_str().unwrap()),
        Some(1)
    );
    let second = list::line_list_message(&agents, 1, super::texts::Lang::ZhTw);
    let items = second["quickReply"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 8, "the remaining 8 fit without another 更多");
    assert_eq!(items[0]["action"]["text"], "@員工12號");
}

#[tokio::test]
async fn line_list_for_three_uses_message_actions_with_call_text() {
    let (_t, reg) = registry(true).await;
    let ids = callable_ids(&reg, &SenderAccess::unmapped(), Some("dudu"));
    let agents = list::callable_agents(&reg, &ids);
    let msg = list::line_list_message(&agents, 0, super::texts::Lang::ZhTw);
    let texts: Vec<&str> = msg["quickReply"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["action"]["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, vec!["@嘟嘟總機", "@業務專員"]);
}

#[test]
fn telegram_keyboard_buttons_carry_the_pick_callback() {
    let agents = vec![list::CallableAgent {
        id: "finance".into(),
        display_name: "財務小幫手".into(),
        icon: String::new(),
        call_text: "@財務小幫手".into(),
    }];
    let kb = list::telegram_keyboard(&agents).expect("a button fits");
    let data = kb["inline_keyboard"][0][0]["callback_data"]
        .as_str()
        .unwrap();
    assert_eq!(list::parse_telegram_pick(data), Some("finance"));
    let long = vec![list::CallableAgent {
        id: "x".repeat(80),
        display_name: "長代號".into(),
        icon: String::new(),
        call_text: "@長代號".into(),
    }];
    assert!(
        list::telegram_keyboard(&long).is_none(),
        "falls back to the text list"
    );
}

#[test]
fn reply_prefix_skips_empty_replies() {
    assert_eq!(with_reply_prefix(String::new(), "財務小幫手"), "");
    assert_eq!(
        with_reply_prefix("好".into(), "財務小幫手"),
        "【財務小幫手】\n好"
    );
}

#[test]
fn unmapped_audit_label_does_not_carry_the_platform_id() {
    let home = tempfile::TempDir::new().unwrap();
    let label = SenderAccess::unmapped().audit_label(home.path(), "line", "Uabcdef123");
    assert!(label.starts_with("unmapped:line:"));
    assert!(!label.contains("Uabcdef123"));
    let digest = label.trim_start_matches("unmapped:line:");
    assert_eq!(digest.len(), 16, "16 hex characters of an HMAC");
    // Stable within one home (same key), different under another home's key.
    assert_eq!(
        SenderAccess::unmapped().audit_label(home.path(), "line", "Uabcdef123"),
        label
    );
    let other = tempfile::TempDir::new().unwrap();
    assert_ne!(
        SenderAccess::unmapped().audit_label(other.path(), "line", "Uabcdef123"),
        label
    );
    // The key file is created once, owner-only.
    let key = home.path().join("trigger_audit.key");
    assert_eq!(std::fs::read(&key).unwrap().len(), 32);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&key).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert_eq!(
        SenderAccess {
            sender: Sender::Admin { user_id: None }
        }
        .audit_label(home.path(), "line", "x"),
        "admin"
    );
    // A real Admin account is recorded as that account.
    assert_eq!(admin().audit_label(home.path(), "line", "x"), "user:a1");
}

#[tokio::test]
async fn channel_sender_without_identity_store_is_unmapped() {
    let t = tempfile::TempDir::new().unwrap();
    assert_eq!(
        access::for_channel_sender(t.path(), "line", "U1"),
        SenderAccess::unmapped()
    );
}

#[tokio::test]
async fn channel_sender_maps_only_through_a_verified_identity() {
    let t = tempfile::TempDir::new().unwrap();
    let db = duduclaw_auth::UserDb::new(&t.path().join("users.db")).unwrap();
    let user = db
        .create_user("emp@test.local", "Emp", "EmpPass-12345", UserRole::Employee)
        .unwrap();
    db.bind_agent(&user.id, "finance", duduclaw_auth::AccessLevel::Operator)
        .unwrap();
    db.bind_channel_identity(&user.id, "line", "U-unverified", false)
        .unwrap();
    db.bind_channel_identity(&user.id, "line", "U-verified", true)
        .unwrap();
    assert_eq!(
        access::for_channel_sender(t.path(), "line", "U-unverified"),
        SenderAccess::unmapped()
    );
    let mapped = access::for_channel_sender(t.path(), "line", "U-verified");
    match mapped.sender {
        Sender::Account { access, .. } => assert!(access.may_call("finance")),
        other => panic!("expected a mapped account, got {other:?}"),
    }
}

#[tokio::test]
async fn mapped_sender_one_shot_goes_to_the_called_employee_then_back() {
    let (t, reg) = registry(false).await;
    let db = duduclaw_auth::UserDb::new(&t.path().join("users.db")).unwrap();
    let user = db
        .create_user("emp@test.local", "Emp", "EmpPass-12345", UserRole::Employee)
        .unwrap();
    db.bind_agent(&user.id, "finance", duduclaw_auth::AccessLevel::Operator)
        .unwrap();
    db.bind_channel_identity(&user.id, "telegram", "42", true)
        .unwrap();
    drop(db);
    let conv: &'static str =
        Box::leak(format!("telegram:{}", uuid::Uuid::new_v4()).into_boxed_str());
    let mk = |text: &'static str, event: &'static str, seq: i64| ChannelTurn {
        channel: "telegram",
        conversation: conv,
        sender: "42",
        text,
        fallback: Some("dudu"),
        event,
        seq,
        quiet: false,
        pending: turn::PendingPolicy::Live,
    };
    let d = turn::decide_channel_turn(t.path(), &reg, mk("@財務小幫手", "m1", pending::next_seq()));
    assert!(matches!(d, TurnDecision::Reply { .. }), "{d:?}");
    let d = turn::decide_channel_turn(t.path(), &reg, mk("幫我算帳", "m2", pending::next_seq()));
    assert_eq!(
        d,
        TurnDecision::Agent {
            id: "finance".into(),
            via_trigger: true
        }
    );
    let d = turn::decide_channel_turn(
        t.path(),
        &reg,
        mk("那上個月呢？", "m3", pending::next_seq()),
    );
    assert_eq!(d, TurnDecision::Default, "no stickiness after the one-shot");
    // The mapped account still may not call sales.
    let d = turn::decide_channel_turn(
        t.path(),
        &reg,
        mk("@sales-rep 報價", "m4", pending::next_seq()),
    );
    assert!(matches!(d, TurnDecision::Reply { ref text, .. } if text == FORBIDDEN_TEXT));
    // `@` lists default + bound employees.
    let d = turn::decide_channel_turn(t.path(), &reg, mk("@", "m5", pending::next_seq()));
    match d {
        TurnDecision::Reply {
            list: Some(agents), ..
        } => {
            let ids: Vec<&str> = agents.iter().map(|a| a.id.as_str()).collect();
            assert_eq!(ids, vec!["dudu", "finance"]);
        }
        other => panic!("expected the list, got {other:?}"),
    }
}

#[tokio::test]
async fn calls_anyone_ignores_plain_text_and_counts_any_employee() {
    let (_t, reg) = registry(false).await;
    assert!(turn::calls_anyone(&reg, "@sales-rep 報價"));
    assert!(turn::calls_anyone(&reg, "@"));
    assert!(turn::calls_anyone(&reg, "請 @財務小幫手 看一下"));
    assert!(!turn::calls_anyone(&reg, "大家早安"));
    assert!(!turn::calls_anyone(&reg, "mail a@b.com"));
}

#[tokio::test]
async fn picking_from_the_list_rechecks_access() {
    let (t, reg) = registry(false).await;
    let conv: &'static str =
        Box::leak(format!("telegram:{}", uuid::Uuid::new_v4()).into_boxed_str());
    let turn = ChannelTurn {
        channel: "telegram",
        conversation: conv,
        sender: "777",
        text: "",
        fallback: Some("dudu"),
        event: "pick-1",
        seq: pending::next_seq(),
        quiet: false,
        pending: turn::PendingPolicy::Live,
    };
    // Unmapped presser: finance is not open to them.
    assert_eq!(
        turn::pick_from_list(t.path(), &reg, turn, "finance"),
        FORBIDDEN_TEXT
    );
    assert!(!turn::has_pending(
        "telegram",
        conv,
        "777",
        pending::next_seq()
    ));
    // The default employee is.
    let text = turn::pick_from_list(t.path(), &reg, turn, "dudu");
    assert!(text.contains("嘟嘟總機"));
    assert!(turn::has_pending(
        "telegram",
        conv,
        "777",
        pending::next_seq()
    ));
}

#[test]
fn a_claim_survives_a_later_arm_for_the_same_event() {
    let k = key("rearm");
    pending::arm(&k, "finance", "e1", 10);
    assert_eq!(pending::claim(&k, "e2", 11), Some("finance".into()));
    // The sender arms someone else while e2 is still running.
    pending::arm(&k, "sales-rep", "e3", 12);
    // Recomputing e2's route keeps its addressee...
    assert_eq!(pending::claim(&k, "e2", 11), Some("finance".into()));
    // ...and the new arm is still there for the next message.
    assert_eq!(pending::claim(&k, "e4", 13), Some("sales-rep".into()));
}

#[test]
fn group_members_never_share_a_one_shot() {
    let conv = format!("line:G{}", uuid::Uuid::new_v4());
    let alice = PendingKey::new("line", &conv, "Ualice");
    let bob = PendingKey::new("line", &conv, "Ubob");
    pending::arm(&alice, "finance", "a1", 10);
    assert_eq!(pending::claim(&bob, "b1", 11), None);
    assert_eq!(pending::claim(&alice, "a2", 12), Some("finance".into()));
}

async fn reply_ctx(public_sales: bool) -> (tempfile::TempDir, crate::channel_reply::ReplyContext) {
    let (tmp, reg) = registry(public_sales).await;
    let sessions = std::sync::Arc::new(
        crate::session::SessionManager::new(&tmp.path().join("sessions.db")).unwrap(),
    );
    let (event_tx, _) = tokio::sync::broadcast::channel(16);
    let channel_status =
        std::sync::Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
    let ctx = crate::channel_reply::ReplyContext::new(
        std::sync::Arc::new(tokio::sync::RwLock::new(reg)),
        tmp.path().to_path_buf(),
        sessions,
        channel_status,
        event_tx,
    );
    (tmp, ctx)
}

/// Goes through the same entry Discord, Slack, WhatsApp and the other
/// shared-pipeline channels use. Fails if the `@` gate in the reply pipeline
/// were removed (the message would reach the model as finance).
#[tokio::test]
async fn the_shared_channel_pipeline_refuses_a_forbidden_call_without_a_model_call() {
    let (t, ctx) = reply_ctx(false).await;
    // A sender mapped to a dashboard account gets the explicit refusal (an
    // unmapped one would see the name treated as plain text).
    let db = duduclaw_auth::UserDb::new(&t.path().join("users.db")).unwrap();
    let user = db
        .create_user("d1@test.local", "D1", "DiscordPass-123", UserRole::Employee)
        .unwrap();
    db.bind_channel_identity(&user.id, "discord", "discord-user-1", true)
        .unwrap();
    drop(db);
    let session = format!("discord:{}", uuid::Uuid::new_v4().simple());
    let reply = crate::channel_reply::build_guarded_reply_with_session(
        "@財務小幫手 幫我算帳",
        &ctx,
        &session,
        "discord-user-1",
        None,
    )
    .await;
    assert!(reply.text.starts_with(FORBIDDEN_TEXT), "{}", reply.text);
    assert!(reply.answered_by.is_none(), "no employee answered");
    // The list comes back the same way, and a repeat within the window is
    // silence (quota throttle).
    let session = format!("discord:{}", uuid::Uuid::new_v4().simple());
    let list =
        crate::channel_reply::build_guarded_reply_with_session("@", &ctx, &session, "u2", None)
            .await;
    assert!(list.text.contains("@嘟嘟總機"), "{}", list.text);
    let again =
        crate::channel_reply::build_guarded_reply_with_session("@", &ctx, &session, "u2", None)
            .await;
    assert!(
        again.text.trim().is_empty() || !again.text.contains("@嘟嘟總機"),
        "{}",
        again.text
    );
}

#[test]
fn a_called_employee_runs_and_restores_in_its_own_session() {
    assert_eq!(
        called_session("line:U1", "finance"),
        "line:U1#agent:finance"
    );
    let who = AnsweredBy {
        id: "finance".into(),
        display_name: "財務小幫手".into(),
        via_trigger: true,
        session_id: called_session("telegram:42", "finance"),
    };
    assert_eq!(
        crate::channel_reply::restore_target_for_test(&who),
        (
            "finance".to_string(),
            "telegram:42#agent:finance".to_string()
        )
    );
}

#[tokio::test]
async fn a_called_turn_marker_is_scoped_to_its_future() {
    assert!(!in_called_turn());
    assert!(as_called_turn(true, async { in_called_turn() }).await);
    assert!(!as_called_turn(false, async { in_called_turn() }).await);
    assert!(!in_called_turn());
}
