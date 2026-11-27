use super::*;
use crate::dept_grants::tests::{fixture, Fixture};
use crate::dept_grants::GrantGroup;
use duduclaw_auth::acl::ContextOrigin;
use duduclaw_core::EditionProfile;

const METHODS: [&str; 4] = [
    "users.dept_grants.list", "users.dept_grants.set",
    "users.dept_grants.options", "users.dept_grants.mine",
];

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

fn empty_mine() -> Value { json!({"grants": [], "agents": [], "needs_sync": []}) }

fn drift_a1(f: &Fixture) {
    let path = f.home.join("agents/a1/agent.toml");
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(raw.contains("department = 'sales'"));
    std::fs::write(path, raw.replace("department = 'sales'", "department = 'mfg'")).unwrap();
}

#[tokio::test]
async fn department_admin_rpcs_enforce_roles_and_return_metadata() {
    let f = fixture().await;
    let params = json!({"user_id": "mgrA", "department": "sales", "groups": ["schedules"]});
    for method in &METHODS[..3] {
        for ctx in [&f.mgr_a, &f.mgr_b, &f.emp] {
            assert_eq!(error(rpc(&f, method, params.clone(), ctx).await), json!("permission denied"));
        }
        payload(rpc(&f, method, params.clone(), &f.admin).await);
    }
    let rows = payload(rpc(&f, METHODS[0], json!({}), &f.admin).await);
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["user_id"], "mgrA");
    assert_eq!(rows[0]["display_name"], "mgrA");
    assert_eq!(rows[0]["email"], "mgrA@example.invalid");
    assert_eq!(rows[0]["department"], "sales");
    assert_eq!(rows[0]["groups"], json!(["schedules"]));
    assert_eq!(rows[0]["granted_by"], "admin");
    assert!(!rows[0]["granted_at"].as_str().unwrap().is_empty());
    let options = payload(rpc(&f, METHODS[2], json!({}), &f.admin).await);
    assert_eq!(options, json!({
        "departments": [{"name": "mfg", "agent_count": 1}, {"name": "sales", "agent_count": 2}],
        "mirror_mismatch_count": 0,
    }));
    drift_a1(&f);
    let options = payload(rpc(&f, METHODS[2], json!({}), &f.admin).await);
    assert_eq!(options["mirror_mismatch_count"], 1);
    assert_eq!(options["departments"][1], json!({"name": "sales", "agent_count": 1}));
    // A default employee is excluded even when its role is not Main.
    std::fs::write(f.home.join("config.toml"), "[general]\ndefault_agent = 'a2'\n").unwrap();
    let options = payload(rpc(&f, METHODS[2], json!({}), &f.admin).await);
    assert_eq!(options["departments"][1], json!({"name": "sales", "agent_count": 0}));
}

#[tokio::test]
async fn department_set_strictly_validates_groups_department_and_target() {
    let f = fixture().await;
    for groups in [json!(["schedules", "unknown"]), json!(["Schedules"]), json!([1]),
        json!("schedules"), Value::Null] {
        let message = error(rpc(&f, METHODS[1], json!({
            "user_id": "mgrA", "department": "sales", "groups": groups,
        }), &f.admin).await);
        assert!(message.as_str().unwrap().contains("請選擇有效的管理功能"));
        assert_eq!(f.db.dept_grants_for_user("mgrA").unwrap()[0].1, GrantGroup::ALL.to_vec());
    }
    for department in ["unknown", "Sales", " sales "] {
        let message = error(rpc(&f, METHODS[1], json!({
            "user_id": "mgrA", "department": department, "groups": ["schedules"],
        }), &f.admin).await);
        assert!(message.as_str().unwrap().contains("此部門尚未確認或已不存在"));
    }
    let message = error(rpc(&f, METHODS[1], json!({
        "user_id": "emp", "department": "sales", "groups": ["schedules"],
    }), &f.admin).await);
    assert_eq!(message, "只能授權給使用中的主管帳號。");
    let message = error(rpc(&f, METHODS[1], json!({
        "user_id": "missing", "department": "sales", "groups": ["schedules"],
    }), &f.admin).await);
    assert_eq!(message, "找不到指定的成員。");
    // The sole mfg mirror no longer agrees, so even an org department is insufficient.
    let path = f.home.join("agents/b1/agent.toml");
    let raw = std::fs::read_to_string(&path).unwrap();
    std::fs::write(path, raw.replace("department = 'mfg'", "department = 'sales'")).unwrap();
    let message = error(rpc(&f, METHODS[1], json!({
        "user_id": "mgrA", "department": "mfg", "groups": ["schedules"],
    }), &f.admin).await);
    assert!(message.as_str().unwrap().contains("此部門尚未確認或已不存在"));
}

#[tokio::test]
async fn department_set_deduplicates_audits_and_updates_users_me() {
    let f = fixture().await;
    assert_eq!(payload(rpc(&f, METHODS[1], json!({
        "user_id": "mgrA", "department": "sales", "groups": ["cost", "schedules", "cost"],
        "actor": "emp", "granted_by": "emp",
    }), &f.admin).await), json!({"ok": true}));
    let me = payload(rpc(&f, "users.me", json!({}), &f.mgr_a).await);
    assert_eq!(me["dept_grants"], json!([{"department": "sales", "groups": ["schedules", "cost"]}]));
    let conn = rusqlite::Connection::open(f.home.join("users.db")).unwrap();
    let (actor, target, detail): (String, String, String) = conn.query_row(
        "SELECT user_id, target, detail FROM auth_audit_log WHERE action='dept_grant_changed' ORDER BY rowid DESC LIMIT 1",
        [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).unwrap();
    assert_eq!(actor, "admin");
    assert_eq!(target, "mgrA");
    assert_eq!(serde_json::from_str::<Value>(&detail).unwrap()["after"], json!(["cost", "schedules"]));
}

#[tokio::test]
async fn department_set_revokes_stale_grants_and_accepts_system_admin_actor() {
    let f = fixture().await;
    f.db.set_dept_grants("admin", "mgrA", "gone", &[GrantGroup::Cost], true).unwrap();
    let admin = UserContext::admin_fallback();
    payload(rpc(&f, METHODS[1], json!({
        "user_id": "mgrA", "department": "gone", "groups": [],
    }), &admin).await);
    assert!(f.db.dept_grants_for_user("mgrA").unwrap().iter().all(|(dept, _)| dept != "gone"));
    // Empty revocation also succeeds if there never was a row for the department.
    payload(rpc(&f, METHODS[1], json!({
        "user_id": "mgrA", "department": "never-existed", "groups": [],
    }), &f.admin).await);
    let conn = rusqlite::Connection::open(f.home.join("users.db")).unwrap();
    let actor: String = conn.query_row(
        "SELECT user_id FROM auth_audit_log WHERE action='dept_grant_changed' ORDER BY rowid DESC LIMIT 1",
        [], |row| row.get(0),
    ).unwrap();
    assert_eq!(actor, "system");
}

#[tokio::test]
async fn department_mine_scopes_agents_groups_and_mirror_drift() {
    let f = fixture().await;
    let mine = payload(rpc(&f, METHODS[3], json!({"group": "schedules"}), &f.mgr_a).await);
    assert_eq!(mine["agents"], json!([
        {"id": "a1", "display_name": "a1", "groups": ["schedules"]},
        {"id": "a2", "display_name": "a2", "groups": ["schedules"]},
    ]));
    assert_eq!(mine["needs_sync"], json!([]));
    let all = payload(rpc(&f, METHODS[3], json!({}), &f.mgr_a).await);
    let all_groups = json!(["schedules", "automation", "skills", "cost", "egress_review"]);
    assert_eq!(all["grants"], json!([{"department": "sales", "groups": all_groups}]));
    for agent in all["agents"].as_array().unwrap() {
        assert_eq!(agent["groups"], all_groups);
    }
    drift_a1(&f);
    let mine = payload(rpc(&f, METHODS[3], json!({"group": "schedules"}), &f.mgr_a).await);
    assert_eq!(mine["agents"], json!([{"id": "a2", "display_name": "a2", "groups": ["schedules"]}]));
    assert_eq!(mine["needs_sync"], json!([{"id": "a1", "display_name": "a1"}]));
    for group in [json!("unknown"), json!("Schedules"), json!(3), Value::Null] {
        assert!(error(rpc(&f, METHODS[3], json!({"group": group}), &f.mgr_a).await)
            .as_str().unwrap().contains("請選擇有效的管理功能"));
    }
}

#[tokio::test]
async fn department_effective_grants_require_dashboard_manager() {
    let f = fixture().await;
    let mut member_origin = f.mgr_a.clone();
    // Provenance-only context: real member keys are refused earlier by the contract gate.
    member_origin.origin = ContextOrigin::MemberKey;
    let mut other_origin = f.mgr_a.clone();
    other_origin.origin = ContextOrigin::Other;
    let admin_token = UserContext::admin_fallback();
    for ctx in [&f.mgr_b, &f.emp, &f.admin, &member_origin, &other_origin, &admin_token] {
        assert_eq!(payload(rpc(&f, METHODS[3], json!({}), ctx).await), empty_mine());
        assert_eq!(payload(rpc(&f, "users.me", json!({}), ctx).await)["dept_grants"], json!([]));
    }
    for method in METHODS {
        assert!(super::super::member_contract::member_key_method(method).is_none(), "{method}");
    }
}

#[tokio::test]
async fn department_rpcs_are_enterprise_only_but_users_me_remains_available() {
    let f = fixture().await;
    f.h.set_test_edition(Some(EditionProfile::Personal)).await;
    for method in METHODS {
        for ctx in [&f.admin, &f.mgr_a, &f.emp] {
            let refused = error(rpc(&f, method, json!({}), ctx).await);
            assert_eq!(refused["code"], ENTERPRISE_ONLY_ERROR_CODE, "{method}");
        }
    }
    assert_eq!(payload(rpc(&f, "users.me", json!({}), &f.mgr_a).await)["dept_grants"], json!([]));
}

#[tokio::test]
async fn users_me_grant_read_failure_returns_empty_authority() {
    let f = fixture().await;
    let conn = rusqlite::Connection::open(f.home.join("users.db")).unwrap();
    conn.execute("DROP TABLE department_manager_grants", []).unwrap();
    assert_eq!(payload(rpc(&f, "users.me", json!({}), &f.mgr_a).await)["dept_grants"], json!([]));
    assert_eq!(payload(rpc(&f, METHODS[3], json!({}), &f.mgr_a).await), empty_mine());
    assert_eq!(error(rpc(&f, METHODS[0], json!({}), &f.admin).await), "無法讀取部門管理授權，請稍後再試。");
    assert_eq!(error(rpc(&f, METHODS[1], json!({
        "user_id": "mgrA", "department": "sales", "groups": ["cost"],
    }), &f.admin).await), "無法儲存部門管理授權，請稍後再試。");
}

#[tokio::test]
async fn widening_requires_strict_boolean_acknowledgement_but_revocation_does_not() {
    let f = fixture().await;
    let base = json!({"user_id": "mgrB", "department": "sales", "groups": ["schedules"]});
    for ack in [None, Some(json!(false)), Some(json!("true")), Some(json!(1)), Some(Value::Null)] {
        let mut params = base.clone();
        if let Some(ack) = ack { params["acknowledged"] = ack; }
        let refused = error(rpc(&f, METHODS[1], params, &f.admin).await);
        assert_eq!(refused, json!({
            "code": "acknowledgement_required",
            "message": "請先閱讀風險說明並勾選「我了解」後再授權。",
        }));
        assert!(f.db.dept_grants_for_user("mgrB").unwrap().is_empty());
    }
    let mut params = base.clone();
    params["acknowledged"] = json!(true);
    payload(rpc(&f, METHODS[1], params, &f.admin).await);
    // Identical sets and revocations do not require confirmation.
    payload(rpc(&f, METHODS[1], base, &f.admin).await);
    payload(rpc(&f, METHODS[1], json!({"user_id": "mgrB", "department": "sales", "groups": []}), &f.admin).await);
    assert!(f.db.dept_grants_for_user("mgrB").unwrap().is_empty());
}
