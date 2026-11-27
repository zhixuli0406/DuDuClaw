use super::*;
use crate::dept_grants::tests::{fixture, Fixture};
use duduclaw_auth::acl::ContextOrigin;
use duduclaw_core::EditionProfile;

const GET: &str = "agents.main_access.get";
const SET: &str = "agents.main_access.set";
const RISK_NOTICE: &str = "被加入的人可以和主要 AI 員工對話、交辦工作，也可能問到它記得的內容（包含管理員和它的對話）。建立員工、改其他員工設定、排程仍只限管理員。";
const INEFFECTIVE_REASON: &str = "主要 AI 員工目前可以執行命令列指令，名單暫時不會生效。請先在「工具與權限」移除命令列工具，或啟用受保護模式。";

fn remove_shell(f: &Fixture, agent: &str) {
    let path = f.home.join("agents").join(agent).join("agent.toml");
    let raw = std::fs::read_to_string(&path).unwrap();
    std::fs::write(path, format!("{raw}\n[capabilities]\nallowed_tools = ['Read']\ndenied_tools = ['Bash']\n")).unwrap();
}

async fn rpc(f: &Fixture, method: &str, params: Value, ctx: &UserContext) -> WsFrame {
    Box::pin(f.h.handle(method, params, ctx)).await
}

fn payload(frame: WsFrame) -> Value {
    match frame {
        WsFrame::Response { ok: true, payload: Some(payload), .. } => payload,
        other => panic!("expected success, got {other:?}"),
    }
}

fn error(frame: WsFrame) -> Value {
    match frame {
        WsFrame::Response { ok: false, error: Some(error), .. } => error,
        other => panic!("expected refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn admin_get_set_deduplicates_returns_only_listed_users_and_audits_actor() {
    let f = fixture().await;
    assert_eq!(payload(rpc(&f, GET, json!({}), &f.admin).await),
        json!({"users": [], "risk_notice": RISK_NOTICE, "effective": true}));
    let expected = json!({"users": [{
        "user_id": "emp", "display_name": "emp", "email": "emp@example.invalid",
        "role": "employee", "status": "active",
    }], "risk_notice": RISK_NOTICE, "effective": false, "ineffective_reason": INEFFECTIVE_REASON});
    assert_eq!(payload(rpc(&f, SET, json!({
        "user_ids": ["emp", "emp"], "acknowledged": true, "actor": "mgrA", "user_id": "mgrA",
    }), &f.admin).await), expected);
    assert_eq!(payload(rpc(&f, GET, json!({}), &f.admin).await), expected);
    let conn = rusqlite::Connection::open(f.home.join("users.db")).unwrap();
    let (actor, target, detail): (String, String, String) = conn.query_row(
        "SELECT user_id, target, detail FROM auth_audit_log WHERE action='main_access_changed'",
        [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).unwrap();
    assert_eq!(actor, "admin");
    assert_eq!(target, "main_agent_users");
    assert_eq!(serde_json::from_str::<Value>(&detail).unwrap(), json!({"before": [], "after": ["emp"], "acknowledged": true}));
    payload(rpc(&f, SET, json!({"user_ids": ["emp"]}), &f.admin).await);
    let count: i64 = conn.query_row(
        "SELECT count(*) FROM auth_audit_log WHERE action='main_access_changed'", [], |row| row.get(0),
    ).unwrap();
    assert_eq!(count, 1);
    assert_eq!(payload(rpc(&f, SET, json!({"user_ids": []}), &f.admin).await),
        json!({"users": [], "risk_notice": RISK_NOTICE, "effective": true}));
    assert!(f.db.main_agent_users().unwrap().is_empty());
}

#[tokio::test]
async fn managers_and_employees_cannot_read_or_edit_main_access() {
    let f = fixture().await;
    f.db.set_main_agent_users("admin", &["emp".into(), "mgrA".into()], true).unwrap();
    for method in [GET, SET] {
        for ctx in [&f.mgr_a, &f.mgr_b, &f.emp] {
            assert_eq!(error(rpc(&f, method, json!({"user_ids": []}), ctx).await), "permission denied");
        }
    }
    assert_eq!(f.db.main_agent_users().unwrap(), vec!["emp".to_owned(), "mgrA".to_owned()]);
}

#[tokio::test]
async fn invalid_main_access_payloads_are_refused_without_partial_writes() {
    let f = fixture().await;
    f.db.set_main_agent_users("admin", &["mgrA".into()], true).unwrap();
    for params in [
        json!({}), Value::Null, json!({"user_ids": null}), json!({"user_ids": "emp"}),
        json!({"user_ids": {"emp": true}}), json!({"user_ids": ["emp", 1]}),
        json!({"user_ids": ["emp", null]}), json!({"user_ids": ["emp", ""]}),
        json!({"user_ids": ["emp", "   "]}), json!({"user_ids": vec!["emp"; 501]}),
    ] {
        assert_eq!(error(rpc(&f, SET, params, &f.admin).await),
            "請提供成員編號陣列，每個編號都必須是非空白文字，最多 500 筆。");
        assert_eq!(f.db.main_agent_users().unwrap(), vec!["mgrA".to_owned()]);
    }
    for id in ["missing", "Emp", " emp "] {
        assert_eq!(error(rpc(&f, SET, json!({"user_ids": ["emp", id]}), &f.admin).await),
            "找不到指定的成員。");
        assert_eq!(f.db.main_agent_users().unwrap(), vec!["mgrA".to_owned()]);
    }
    rusqlite::Connection::open(f.home.join("users.db")).unwrap()
        .execute("UPDATE users SET status='suspended' WHERE id='emp'", []).unwrap();
    assert_eq!(error(rpc(&f, SET, json!({"user_ids": ["emp"]}), &f.admin).await),
        "只能加入使用中的成員帳號。");
    assert_eq!(f.db.main_agent_users().unwrap(), vec!["mgrA".to_owned()]);
}

#[tokio::test]
async fn personal_edition_refuses_both_rpcs_and_ignores_stale_list_in_users_me() {
    let f = fixture().await;
    f.db.set_main_agent_users("admin", &["emp".into()], true).unwrap();
    f.h.set_test_edition(Some(EditionProfile::Personal)).await;
    for method in [GET, SET] {
        for ctx in [&f.admin, &f.mgr_a, &f.emp] {
            assert_eq!(error(rpc(&f, method, json!({"user_ids": []}), ctx).await)["code"],
                ENTERPRISE_ONLY_ERROR_CODE);
        }
    }
    assert!(f.db.is_main_agent_user("emp").unwrap());
    assert_eq!(payload(rpc(&f, "users.me", json!({}), &f.emp).await)["main_agent_access"], false);
}

#[tokio::test]
async fn users_me_main_access_requires_dashboard_session_and_new_rpcs_have_no_member_contract() {
    let f = fixture().await;
    assert_eq!(payload(rpc(&f, "users.me", json!({}), &f.emp).await)["main_agent_access"], false);
    f.db.set_main_agent_users("admin", &["emp".into()], true).unwrap();
    assert_eq!(payload(rpc(&f, "users.me", json!({}), &f.emp).await)["main_agent_access"], false);
    assert_eq!(payload(rpc(&f, "users.me", json!({}), &f.admin).await)["main_agent_access"], true);
    remove_shell(&f, "main");
    assert_eq!(payload(rpc(&f, "users.me", json!({}), &f.emp).await)["main_agent_access"], true);
    assert_eq!(payload(rpc(&f, "users.me", json!({}), &f.admin).await)["main_agent_access"], true);
    for origin in [ContextOrigin::MemberKey, ContextOrigin::Other] {
        // Provenance-only context: real member keys also pass the contract gate.
        let mut ctx = f.emp.clone();
        ctx.origin = origin;
        assert_eq!(payload(rpc(&f, "users.me", json!({}), &ctx).await)["main_agent_access"], false);
    }
    assert_eq!(payload(rpc(&f, "users.me", json!({}), &UserContext::admin_fallback()).await)
        ["main_agent_access"], false);
    for method in [GET, SET] {
        assert!(super::super::member_contract::member_key_method(method).is_none());
    }
}

#[tokio::test]
async fn main_access_store_errors_are_sanitized_and_users_me_fails_closed() {
    let f = fixture().await;
    // Keep the account readable while forcing list reads/writes to fail.
    let conn = rusqlite::Connection::open(f.home.join("users.db")).unwrap();
    conn.execute_batch("ALTER TABLE main_agent_users RENAME COLUMN user_id TO broken_user_id;").unwrap();
    assert_eq!(error(rpc(&f, GET, json!({}), &f.admin).await),
        "無法讀取主要 AI 員工開放名單，請稍後再試。");
    assert_eq!(error(rpc(&f, SET, json!({"user_ids": ["emp"]}), &f.admin).await),
        "無法儲存主要 AI 員工開放名單，請稍後再試。");
    assert_eq!(payload(rpc(&f, "users.me", json!({}), &f.emp).await)["main_agent_access"], false);
}

#[tokio::test]
async fn widening_requires_strict_boolean_acknowledgement_but_revocation_does_not() {
    let f = fixture().await;
    let base = json!({"user_ids": ["emp"]});
    for ack in [None, Some(json!(false)), Some(json!("true")), Some(json!(1)), Some(Value::Null)] {
        let mut params = base.clone();
        if let Some(ack) = ack { params["acknowledged"] = ack; }
        let refused = error(rpc(&f, SET, params, &f.admin).await);
        assert_eq!(refused, json!({
            "code": "acknowledgement_required",
            "message": "請先閱讀風險說明並勾選「我了解」後再授權。",
        }));
        assert!(f.db.main_agent_users().unwrap().is_empty());
    }
    let mut params = base.clone();
    params["acknowledged"] = json!(true);
    payload(rpc(&f, SET, params, &f.admin).await);
    // Identical sets and revocations do not require confirmation.
    payload(rpc(&f, SET, base, &f.admin).await);
    payload(rpc(&f, SET, json!({"user_ids": []}), &f.admin).await);
    assert!(f.db.main_agent_users().unwrap().is_empty());
}

#[tokio::test]
async fn shell_warning_does_not_prevent_save_and_disappears_after_tool_removal() {
    let f = fixture().await;
    for method in [SET, GET] {
        let response = payload(rpc(&f, method,
            json!({"user_ids": ["emp"], "acknowledged": true}), &f.admin).await);
        assert_eq!(response["effective"], false);
        assert_eq!(response["ineffective_reason"], INEFFECTIVE_REASON);
        assert_eq!(f.db.main_agent_users().unwrap(), vec!["emp".to_owned()]);
    }
    remove_shell(&f, "main");
    for method in [GET, SET] {
        let response = payload(rpc(&f, method, json!({"user_ids": ["emp"]}), &f.admin).await);
        assert_eq!(response["effective"], true);
        assert!(response.get("ineffective_reason").is_none());
    }
    assert_eq!(payload(rpc(&f, "users.me", json!({}), &f.emp).await)["main_agent_access"], true);
}

#[tokio::test]
async fn every_main_and_configured_default_must_be_shell_free() {
    let f = fixture().await;
    remove_shell(&f, "main");
    // A specialist can still be the gateway default; its shell blocks the list.
    std::fs::write(f.home.join("config.toml"), "[general]\ndefault_agent = 'a1'\n").unwrap();
    let response = payload(rpc(&f, SET,
        json!({"user_ids": ["emp"], "acknowledged": true}), &f.admin).await);
    assert_eq!(response["effective"], false);
    assert_eq!(payload(rpc(&f, "users.me", json!({}), &f.emp).await)["main_agent_access"], false);
    remove_shell(&f, "a1");
    assert_eq!(payload(rpc(&f, GET, json!({}), &f.admin).await)["effective"], true);
    crate::trigger_route::tests::write_agent(
        &f.home.join("agents"), "second-main", "Second", "", "main", "",
    );
    assert_eq!(payload(rpc(&f, GET, json!({}), &f.admin).await)["effective"], false);
    remove_shell(&f, "second-main");
    assert_eq!(payload(rpc(&f, GET, json!({}), &f.admin).await)["effective"], true);
}

#[tokio::test]
async fn undeterminable_main_tools_warn_and_keep_saved_list() {
    let f = fixture().await;
    std::fs::write(f.home.join("agents/main/agent.toml"), "invalid TOML !").unwrap();
    for method in [SET, GET] {
        let response = payload(rpc(&f, method,
            json!({"user_ids": ["emp"], "acknowledged": true}), &f.admin).await);
        assert_eq!(response["effective"], false);
        assert_eq!(response["ineffective_reason"], INEFFECTIVE_REASON);
        assert_eq!(f.db.main_agent_users().unwrap(), vec!["emp".to_owned()]);
    }
    assert_eq!(payload(rpc(&f, "users.me", json!({}), &f.emp).await)["main_agent_access"], false);
    assert_eq!(payload(rpc(&f, "users.me", json!({}), &f.admin).await)["main_agent_access"], true);
}
