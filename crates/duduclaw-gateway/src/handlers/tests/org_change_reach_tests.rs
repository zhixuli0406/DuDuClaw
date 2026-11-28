//! RM-142 P12: actual RPC decisions must confirm current grant impact.
use super::*;
use super::agent_files_rpc_tests::audit_rows;
use crate::approval::{ApprovalBroker, ApprovalId, ApprovalStatus};
use crate::dept_grants::tests::{fixture, Fixture};
use crate::dept_grants::GrantGroup;

async fn rpc(f: &Fixture, method: &str, params: Value) -> WsFrame {
    Box::pin(f.h.handle(method, params, &f.admin)).await
}

fn ok(frame: WsFrame) -> Value {
    match frame {
        WsFrame::Response { ok: true, payload: Some(p), .. } => p,
        other => panic!("expected success: {other:?}"),
    }
}

fn error(frame: WsFrame, code: &str) -> Value {
    match frame {
        WsFrame::Response { ok: false, error: Some(e), .. } => {
            assert_eq!(e["code"], code, "{e}");
            e
        }
        other => panic!("expected {code}: {other:?}"),
    }
}

async fn propose(f: &Fixture) -> Value {
    f.db.bind_agent("mgrA", "b1", AccessLevel::Operator).unwrap();
    let mut manager = f.db.live_user_context("mgrA").unwrap().unwrap();
    manager.origin = duduclaw_auth::acl::ContextOrigin::DashboardSession;
    ok(Box::pin(f.h.handle("org.propose_change",
        json!({"agent_id": "b1", "department": "sales"}), &manager)).await)
}

fn base(f: &Fixture) -> String { duduclaw_core::org_change::entry_hash(&f.home, "b1") }

async fn record(f: &Fixture, proposal: &Value) -> crate::approval::ApprovalRecord {
    ApprovalBroker::open(&f.home).unwrap()
        .get(&ApprovalId::from(proposal["approval_id"].as_str().unwrap().to_owned()))
        .await.unwrap().unwrap()
}

#[tokio::test]
async fn org_change_reach_digest_refuses_stale() {
    let f = fixture().await;
    let p = propose(&f).await;
    assert_eq!(p["reach"][0]["user_id"], "mgrA");
    assert_eq!(p["reach"][0]["groups"].as_array().unwrap().len(), 5);
    assert_eq!(p["reach"][0]["change"], "gains");
    let rec = record(&f, &p).await;
    let sentence = p["sentence"].as_str().unwrap();
    assert!(rec.summary.ends_with(sentence));
    assert!(crate::org_change_approval::notice_body(&rec, false, "稍後").contains(sentence));
    assert_eq!(rec.payload["reach_digest"], p["reach_digest"]);
    let d = ok(rpc(&f, "approvals.decide", json!({
        "id": p["approval_id"], "approve": true, "reach_digest": p["reach_digest"],
    })).await);
    assert_eq!(d["decided"], "approved");
    assert_eq!(duduclaw_core::org_change::effective_entry(&f.home, "b1").unwrap().department, "sales");

    let f = fixture().await;
    let p = propose(&f).await;
    let original_base = base(&f);
    error(rpc(&f, "approvals.decide", json!({"id": p["approval_id"], "approve": true})).await, "reach_changed");
    f.db.set_dept_grants("admin", "mgrB", "sales", &[GrantGroup::Skills], true).unwrap();
    let e = error(rpc(&f, "approvals.decide", json!({
        "id": p["approval_id"], "approve": true, "reach_digest": p["reach_digest"],
    })).await, "reach_changed");
    assert_eq!(e["message"], "授權範圍在你檢視後有變動，請重新確認。");
    assert_eq!(e["current"]["reach"].as_array().unwrap().len(), 2);
    assert_ne!(e["current"]["reach_digest"], p["reach_digest"]);
    assert_eq!(base(&f), original_base);
    assert_eq!(record(&f, &p).await.status, ApprovalStatus::Pending);
    let rows = audit_rows(&f.home, "org_change_rejected");
    assert_eq!(rows.iter().filter(|r| r["details"]["why"] == "reach_changed").count(), 2);
    // A refusal leaves the same card retryable with the newly shown impact.
    ok(rpc(&f, "approvals.decide", json!({"id": p["approval_id"], "approve": true,
        "reach_digest": e["current"]["reach_digest"]})).await);

    let f = fixture().await;
    f.db.set_dept_grants("admin", "mgrA", "sales", &[], true).unwrap();
    let p = propose(&f).await;
    assert_eq!(p["reach"], json!([]));
    assert!(p["sentence"].is_null());
    ok(rpc(&f, "approvals.decide", json!({"id": p["approval_id"], "approve": true})).await);
}

#[tokio::test]
async fn org_apply_change_reach_digest_refuses_stale() {
    let f = fixture().await;
    let preview = ok(rpc(&f, "users.dept_grants.reach_preview",
        json!({"agent_id": "b1", "department": "sales"})).await);
    let original_base = base(&f);
    error(rpc(&f, "org.apply_change", json!({"agent_id": "b1", "department": "sales",
        "expected_base": original_base})).await, "reach_changed");
    f.db.set_dept_grants("admin", "mgrB", "sales", &[GrantGroup::Cost], true).unwrap();
    let e = error(rpc(&f, "org.apply_change", json!({"agent_id": "b1", "department": "sales",
        "expected_base": original_base, "reach_digest": preview["reach_digest"]})).await, "reach_changed");
    assert_eq!(base(&f), original_base);
    assert_eq!(audit_rows(&f.home, "org_change_rejected").len(), 2);
    let applied = ok(rpc(&f, "org.apply_change", json!({"agent_id": "b1", "department": "sales",
        "expected_base": original_base, "reach_digest": e["current"]["reach_digest"]})).await);
    assert_eq!(applied["reach"], e["current"]["reach"]);

    let f = fixture().await;
    f.db.set_dept_grants("admin", "mgrA", "sales", &[], true).unwrap();
    ok(rpc(&f, "org.apply_change", json!({"agent_id": "b1", "department": "sales",
        "expected_base": base(&f)})).await);
}

#[tokio::test]
async fn reach_preview_is_admin_only_and_uses_the_org_base_department() {
    let f = fixture().await;
    let params = json!({"agent_id": "b1", "department": "sales"});
    assert!(matches!(Box::pin(f.h.handle("users.dept_grants.reach_preview", params.clone(), &f.mgr_a)).await,
        WsFrame::Response { ok: false, .. }));
    // A drifted display mirror must not turn an mfg -> sales gain into a no-op.
    let path = f.home.join("agents/b1/agent.toml");
    let raw = std::fs::read_to_string(&path).unwrap();
    std::fs::write(path, raw.replace("department = 'mfg'", "department = 'sales'")).unwrap();
    let preview = ok(rpc(&f, "users.dept_grants.reach_preview", params).await);
    assert_eq!(preview["reach"][0]["user_id"], "mgrA");
    let same = ok(rpc(&f, "users.dept_grants.reach_preview", json!({"agent_id": "b1", "department": "mfg"})).await);
    assert_eq!(same["reach"], json!([]));
    let cleared = ok(rpc(&f, "users.dept_grants.reach_preview", json!({"agent_id": "a1", "department": ""})).await);
    assert_eq!(cleared["reach"][0]["change"], "loses");
}

#[tokio::test]
async fn org_reach_read_error_marks_proposal_and_refuses_decision_and_apply() {
    let f = fixture().await;
    // A request-time migrating opener in another subsystem must not heal the fault.
    rusqlite::Connection::open(f.home.join("users.db")).unwrap()
        .execute_batch("DROP TABLE department_manager_grants;
            CREATE VIEW department_manager_grants AS SELECT 1 AS unreadable;").unwrap();
    let p = propose(&f).await;
    assert_eq!(p["sentence"], "授權影響無法計算");
    assert!(p["reach_digest"].is_null());
    assert!(record(&f, &p).await.summary.contains("授權影響無法計算"));
    error(rpc(&f, "approvals.decide", json!({"id": p["approval_id"], "approve": true})).await, "reach_unavailable");
    error(rpc(&f, "org.apply_change", json!({"agent_id": "b1", "department": "sales",
        "expected_base": base(&f)})).await, "reach_unavailable");
    assert_eq!(record(&f, &p).await.status, ApprovalStatus::Pending);
    // Changing only the supervisor still needs no grant read or digest.
    let proposed = ok(rpc(&f, "org.propose_change", json!({"agent_id": "a1", "reports_to": "a2"})).await);
    assert_eq!(proposed["reach"], json!([]));
    ok(rpc(&f, "approvals.decide", json!({"id": proposed["approval_id"], "approve": true})).await);
}

#[tokio::test]
async fn org_reach_channel_decisions_still_require_dashboard() {
    let f = fixture().await;
    let p = propose(&f).await;
    for approve in [true, false] {
        let refusal = crate::approval_notify::apply_decision(&f.home, "telegram", "unmapped",
            p["approval_id"].as_str().unwrap(), approve).await.unwrap_err();
        assert!(refusal.contains("儀表板"));
    }
    assert_eq!(record(&f, &p).await.status, ApprovalStatus::Pending);
}

#[tokio::test]
async fn org_reach_denial_succeeds_without_digest_after_grants_change() {
    let f = fixture().await;
    let p = propose(&f).await;
    let original_base = base(&f);
    assert!(!p["reach"].as_array().unwrap().is_empty());
    f.db.set_dept_grants("admin", "mgrB", "sales", &[GrantGroup::Skills], true).unwrap();
    let denied = ok(rpc(&f, "approvals.decide", json!({
        "id": p["approval_id"], "approve": false,
    })).await);
    assert_eq!(denied["decided"], "denied");
    assert!(denied["side_effect"].is_null());
    assert_eq!(record(&f, &p).await.status, ApprovalStatus::Denied);
    assert_eq!(base(&f), original_base);
}
