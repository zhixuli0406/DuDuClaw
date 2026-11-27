use super::*;
use crate::approval::{ApprovalId, ApprovalStatus, ExecutionBinding, DecisionContext, RequestKind};
use crate::dept_grants::tests::{fixture, Fixture};
use crate::protocol::WsFrame;
use serde_json::{json, Value};

const SECRET: &str = "private outbound content";

fn ok(frame: &WsFrame) -> bool { matches!(frame, WsFrame::Response { ok:true, .. }) }

fn payload(frame: WsFrame) -> Value {
    match frame {
        WsFrame::Response { ok:true, payload:Some(value), .. } => value,
        other => panic!("unexpected response: {other:?}"),
    }
}

async fn card(f: &Fixture, agent: &str, principal: Option<&str>, kind: &str, bound: bool) -> ApprovalRecord {
    let broker = ApprovalBroker::open(&f.home).unwrap();
    let body = json!({"name":"send_message", "arguments":{"text":SECRET, "recipient":"someone@example.invalid"},
        "egress_guard":{"sources":["private-source"]}, "action_grant":{"effect":"send", "summary":"private-summary"},
        "google_account":"account@example.invalid"});
    let id = crate::on_behalf_of::ON_BEHALF_OF.scope(principal.map(str::to_owned), async {
        if bound {
            let binding = ExecutionBinding {
                schema_version:1, run_id:uuid::Uuid::new_v4().to_string(), run_origin_kind:"ingress".into(),
                actor_principal:agent.into(), conversation_audience:Default::default(),
                decision_context:DecisionContext { channel:"telegram".into(), account_id:"bot".into(),
                    conversation_id:"12345".into(), principal_id:"requester".into() },
                task_id:None, task_revision:None, task_snapshot_hash:None,
                payload_hash:crate::approval::payload_hash(&body),
                policy_revision:crate::approval::policy_revision(&f.home, agent).unwrap(),
                cwd:None, environment_hash:"fixture".into(), file_hashes:Default::default(),
                expires_at:(chrono::Utc::now() + chrono::Duration::seconds(600)).to_rfc3339(),
                resume_handler:"live_only_v1".into(), resume_version:1,
            };
            broker.request_bound(RequestKind::Approval, agent, "Review", body, binding).await.unwrap()
        } else {
            broker.request(agent, kind, "Review", body, 600).await.unwrap()
        }
    }).await;
    broker.get(&id).await.unwrap().unwrap()
}

async fn get(f: &Fixture, rec: &ApprovalRecord, ctx: &UserContext) -> Value {
    payload(f.h.handle_approvals_get(json!({"id":rec.id.as_str()}), ctx).await)
}

fn audits(f: &Fixture, event: &str) -> Vec<Value> {
    std::fs::read_to_string(f.home.join("security_audit.jsonl")).unwrap().lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|row| row["event_type"] == event).collect()
}

#[tokio::test]
async fn outbound_reviewer_other_department_redacted() {
    let f = fixture().await;
    for bound in [false, true] {
        let sales = card(&f, "a1", None, "mcp_call", bound).await;
        let other = card(&f, "b1", None, "mcp_call", bound).await;
        let listed = payload(f.h.handle_approvals_list(json!({"id":sales.id.as_str(), "detail":true}), &f.mgr_a).await);
        assert_eq!(listed["approvals"][0]["payload"], json!({"redacted":true}));
        assert_eq!(listed["approvals"][0]["may_decide"], true);
        assert!(listed["approvals"][0]["digest"].is_null());
        assert!(listed["approvals"][0]["binding"].is_null());
        assert!(!listed.to_string().contains(SECRET));
        let full = get(&f, &sales, &f.mgr_a).await;
        assert_eq!(full["approvals"][0]["payload"], sales.payload);
        assert!(full["approvals"][0]["digest"].is_string());
        assert_eq!(get(&f, &other, &f.mgr_a).await["count"], 0);
        assert_eq!(f.h.authorize_bound_decider(&sales, &f.mgr_a).await.is_ok(), bound);
        assert!(f.h.authorize_egress_decider(&other, &f.mgr_a).await.is_err());
        let result = f.h.handle_approvals_decide(json!({"id":sales.id.as_str(), "approve":false,
            "expected_digest":full["approvals"][0]["digest"]}), &f.mgr_a).await;
        assert!(ok(&result), "{result:?}");
        assert_eq!(ApprovalBroker::open(&f.home).unwrap().get(&sales.id).await.unwrap().unwrap().decided_by,
            Some("dashboard:mgrA".into()));
    }
}

#[tokio::test]
async fn outbound_reviewer_cannot_self_approve() {
    let f = fixture().await;
    let broker = ApprovalBroker::open(&f.home).unwrap();
    for bound in [false, true] {
        let rec = card(&f, "a1", Some("user:mgrA"), "mcp_call", bound).await;
        assert_eq!(get(&f, &rec, &f.mgr_a).await["count"], 0);
        for approve in [false, true] {
            assert!(!ok(&f.h.handle_approvals_decide(json!({"id":rec.id.as_str(), "approve":approve}), &f.mgr_a).await));
            let decision = if bound { broker.decide_bound_dashboard(&rec.id, &f.mgr_a, approve).await }
                else { broker.decide_egress_dashboard(&rec.id, &f.mgr_a, approve).await };
            assert!(decision.is_err());
        }
        assert_eq!(broker.get(&rec.id).await.unwrap().unwrap().status, ApprovalStatus::Pending);
    }
    let rec = card(&f, "a1", Some("user:mgrA2"), "mcp_call", false).await;
    assert!(f.h.authorize_egress_decider(&rec, &f.mgr_a).await.is_ok(), "exact principal equality only");
}

#[tokio::test]
async fn outbound_reviewer_requires_grant_and_dashboard_session() {
    let f = fixture().await;
    let rec = card(&f, "a1", None, "mcp_call", false).await;
    let mut member = f.mgr_a.clone();
    member.origin = ContextOrigin::MemberKey;
    member.member_key_id = Some("member-key".into());
    let mut chat = f.mgr_a.clone();
    chat.origin = ContextOrigin::Other;
    let mut admin_key = f.admin.clone();
    admin_key.origin = ContextOrigin::MemberKey;
    for ctx in [&f.mgr_b, &f.emp, &member, &chat, &admin_key] {
        assert!(f.h.authorize_egress_decider(&rec, ctx).await.is_err());
        assert!(!ok(&f.h.handle_approvals_decide(json!({"id":rec.id.as_str(), "approve":true}), ctx).await));
        assert_eq!(get(&f, &rec, ctx).await["count"], 0);
        assert!(ApprovalBroker::open(&f.home).unwrap().decide_egress_dashboard(&rec.id, ctx, true).await.is_err());
    }
}

#[tokio::test]
async fn outbound_broker_rechecks_revoked_grant_and_rejects_generic_decide() {
    let f = fixture().await;
    let broker = ApprovalBroker::open(&f.home).unwrap();
    let plain = card(&f, "a1", None, "mcp_call", false).await;
    let bound = card(&f, "a1", None, "mcp_call", true).await;
    for rec in [&plain, &bound] {
        assert_eq!(get(&f, rec, &f.mgr_a).await["approvals"][0]["may_decide"], true);
        assert!(broker.decide(&rec.id, true, "dashboard:mgrA").await.is_err());
    }
    f.db.set_dept_grants(&f.admin.user_id, &f.mgr_a.user_id, "sales", &[], true).unwrap();
    assert!(broker.decide_egress_dashboard(&plain.id, &f.mgr_a, true).await.is_err());
    assert!(broker.decide_bound_dashboard(&bound.id, &f.mgr_a, true).await.is_err());
    for rec in [&plain, &bound] {
        assert_eq!(broker.get(&rec.id).await.unwrap().unwrap().status, ApprovalStatus::Pending);
        assert_eq!(get(&f, rec, &f.mgr_a).await["count"], 0);
    }
}

#[tokio::test]
async fn outbound_admin_and_audits() {
    let f = fixture().await;
    let rec = card(&f, "a1", None, "mcp_call", false).await;
    for _ in 0..2 { assert_eq!(get(&f, &rec, &f.mgr_a).await["approvals"][0]["payload"], rec.payload); }
    assert_eq!(get(&f, &rec, &f.admin).await["approvals"][0]["payload"], rec.payload);
    assert_eq!(audits(&f, "outbound_card_opened").len(), 2);
    assert!(ok(&f.h.handle_approvals_decide(json!({"id":rec.id.as_str(), "approve":true}), &f.mgr_a).await));
    let admin_card = card(&f, "b1", Some("user:admin"), "cli_egress", false).await;
    let listed = payload(f.h.handle_approvals_list(json!({"id":admin_card.id.as_str()}), &f.admin).await);
    assert_eq!(listed["approvals"][0]["payload"], json!({"redacted":true}));
    assert!(ok(&f.h.handle_approvals_decide(json!({"id":admin_card.id.as_str(), "approve":false}), &f.admin).await));
    let decisions = audits(&f, "outbound_decided");
    assert_eq!(decisions.len(), 2);
    assert_eq!(decisions[0]["details"]["reviewer_type"], "department_manager");
    assert_eq!(decisions[1]["details"]["reviewer_type"], "admin");
    let raw = std::fs::read_to_string(f.home.join("security_audit.jsonl")).unwrap();
    for secret in [SECRET, "private-source", "someone@example.invalid", "account@example.invalid"] {
        assert!(!raw.contains(secret));
    }
}

#[tokio::test]
async fn outbound_reviewer_ignores_internal_and_non_employee_directories() {
    let f = fixture().await;
    let rec = card(&f, "a1", None, "mcp_call", false).await;
    let _ = DeptGrantView::load(&f.h, &f.mgr_a).await;
    for id in ["_internal", ".hidden", "empty"] {
        std::fs::create_dir(f.home.join("agents").join(id)).unwrap();
    }
    // Internal files are not employee configs, even when present but invalid.
    std::fs::write(f.home.join("agents/_internal/agent.toml"), "invalid TOML !").unwrap();
    assert!(authorize(&f.home, &f.mgr_a, &rec).is_ok());
    assert!(DeptGrantView::notification_allows(&f.home, &f.mgr_a.user_id, "a1"));
    assert!(ok(&f.h.handle_approvals_decide(json!({"id":rec.id.as_str(), "approve":true}), &f.mgr_a).await));
}

#[tokio::test]
async fn outbound_reviewer_denied_for_corrupt_or_unreadable_employee_config() {
    let f = fixture().await;
    let rec = card(&f, "a1", None, "mcp_call", false).await;
    let _ = DeptGrantView::load(&f.h, &f.mgr_a).await;
    assert!(authorize(&f.home, &f.mgr_a, &rec).is_ok());
    let path = f.home.join("agents/main/agent.toml");
    std::fs::write(&path, "invalid TOML !").unwrap();
    assert!(authorize(&f.home, &f.mgr_a, &rec).is_err());
    assert!(!DeptGrantView::notification_allows(&f.home, &f.mgr_a.user_id, "a1"));
    // A directory at the config path produces a non-NotFound read error.
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(authorize(&f.home, &f.mgr_a, &rec).is_err());
    assert!(!DeptGrantView::notification_allows(&f.home, &f.mgr_a.user_id, "a1"));
}

#[tokio::test]
async fn outbound_notification_targets_only_admin_and_granted_department_manager() {
    let f = fixture().await;
    let mut rec = card(&f, "a1", None, "mcp_call", false).await;
    let _ = DeptGrantView::load(&f.h, &f.mgr_a).await;
    for (ctx, destination) in [(&f.admin, "11111"), (&f.mgr_a, "22222"), (&f.mgr_b, "33333"), (&f.emp, "44444")] {
        f.db.bind_channel_identity(&ctx.user_id, "telegram", destination, true).unwrap();
    }
    rec.notify_channel = Some("telegram".into());
    rec.notify_chat_id = Some("33333".into());
    let targets = crate::approval_notify::dashboard_only_targets_for_test(&f.home, &rec);
    assert_eq!(targets, vec![("telegram".into(), "11111".into()), ("telegram".into(), "22222".into())]);
    f.db.set_dept_grants(&f.admin.user_id, &f.mgr_a.user_id, "sales", &[], true).unwrap();
    assert_eq!(crate::approval_notify::dashboard_only_targets_for_test(&f.home, &rec), vec![("telegram".into(), "11111".into())]);
}

#[tokio::test]
async fn outbound_chat_manager_refused_even_with_review_grant() {
    use crate::approval::decision_service::{self, Decider};
    let f = decision_service::tests::Fixture::new();
    let id = f.simple("other_outgoing").await;
    let proof = f.proof(&id).await;
    f.db().update_user(&f.user, None, Some(duduclaw_auth::UserRole::Manager), None).unwrap();
    let admin = f.db().list_users().unwrap().into_iter().find(|u| u.role == duduclaw_auth::UserRole::Admin).unwrap();
    f.db().set_dept_grants(&admin.id, &f.user, "sales", &[GrantGroup::EgressReview], true).unwrap();
    assert!(decision_service::decide(&f.env, &id, true, "", &Decider::ChatAdmin(proof)).await.is_err());
    assert_eq!(f.status(&id).await, ApprovalStatus::Pending);
}

#[tokio::test]
async fn outbound_cli_kind_without_guard_is_still_protected() {
    let f = fixture().await;
    let broker = ApprovalBroker::open(&f.home).unwrap();
    let id: ApprovalId = broker.request("a1", "cli_egress", "Review", json!({"arguments":{"text":SECRET}}), 600).await.unwrap();
    let rec = broker.get(&id).await.unwrap().unwrap();
    assert!(is_outbound_data_card(&rec));
    assert_eq!(get(&f, &rec, &f.mgr_a).await["approvals"][0]["payload"], rec.payload);
    assert!(broker.decide(&id, true, "dashboard:admin").await.is_err());
    assert!(ok(&f.h.handle_approvals_decide(json!({"id":id.as_str(), "approve":false}), &f.mgr_a).await));
    let mut rec = rec;
    rec.action_kind = "ordinary".into();
    assert!(!is_outbound_data_card(&rec));
    for marker in [Value::Null, json!(false), json!({})] {
        rec.payload["egress_guard"] = marker;
        assert!(is_outbound_data_card(&rec));
    }
}
