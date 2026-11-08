//! RM-026 — `agents.permission_history`: Admin-only, one employee only,
//! capabilities/permissions changes only, newest first, cursor paging.
use super::*;
use crate::handlers::audit_rpc::permission_history_page;

fn ok(frame: &WsFrame) -> Value {
    match frame {
        WsFrame::Response { ok: true, payload: Some(p), .. } => p.clone(),
        other => panic!("expected ok, got {other:?}"),
    }
}

fn err(frame: &WsFrame) -> String {
    match frame {
        WsFrame::Response { ok: false, error: Some(e), .. } => e.as_str().unwrap_or_default().to_string(),
        other => panic!("expected error, got {other:?}"),
    }
}

fn admin() -> UserContext {
    let mut c = UserContext::admin_fallback();
    c.user_id = "admin-1".into();
    c
}

fn employee() -> UserContext {
    UserContext {
        credential_epoch: 0,
        user_id: "emp-1".into(),
        email: "emp@example.com".into(),
        role: duduclaw_auth::models::UserRole::Employee,
        agent_access: [("a1".to_string(), AccessLevel::Owner)].into_iter().collect(),
        must_change_password: false,
        team_access: Default::default(),
        acl_generation: 0,
    }
}

fn event(ts: &str, agent: &str, user: &str, origin: Option<&str>, keys: &[&str]) -> duduclaw_security::audit::AuditEvent {
    duduclaw_security::audit::AuditEvent {
        timestamp: ts.into(),
        event_type: "agent_authority_changed".into(),
        agent_id: agent.into(),
        severity: duduclaw_security::audit::Severity::Warning,
        details: json!({
            "agent_id": agent,
            "user_id": user,
            "source": "agents.update",
            "permission_origin": origin,
            "changes": keys.iter().map(|k| json!({ "key": k, "before": [], "after": ["x"] })).collect::<Vec<_>>(),
        }),
    }
}

fn write_audit(home: &Path, events: &[duduclaw_security::audit::AuditEvent]) {
    let lines: Vec<String> = events.iter().map(|e| serde_json::to_string(e).unwrap()).collect();
    std::fs::write(home.join("security_audit.jsonl"), lines.join("\n") + "\n").unwrap();
}

#[test]
fn page_filters_by_agent_and_permission_keys_newest_first() {
    let events = vec![
        event("2026-11-01T10:00:00Z", "a1", "u1", None, &["capabilities.denied_tools"]),
        event("2026-11-02T10:00:00Z", "a2", "u1", None, &["capabilities.denied_tools"]),
        event("2026-11-03T10:00:00Z", "a1", "u2", Some("matrix"), &["capabilities.allowed_tools", "agent.reports_to"]),
        // Org-only change: not a permission change.
        event("2026-11-04T10:00:00Z", "a1", "u1", None, &["agent.reports_to"]),
        // A different event type for the same agent is ignored.
        duduclaw_security::audit::AuditEvent {
            event_type: "agent_authority_refused".into(),
            ..event("2026-11-05T10:00:00Z", "a1", "u1", None, &["capabilities.denied_tools"])
        },
    ];
    let (rows, next) = permission_history_page(&events, "a1", 50, None);
    assert!(next.is_none());
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0]["timestamp"], "2026-11-03T10:00:00Z");
    assert_eq!(rows[0]["actor"], "u2");
    // A client-claimed origin (legacy rows) is never echoed.
    assert!(rows.iter().all(|r| r.get("origin").is_none()), "{rows:?}");
    // The org key is trimmed from the row; only the permission key stays.
    assert_eq!(rows[0]["changes"].as_array().unwrap().len(), 1);
    assert!(rows.iter().all(|r| r["agent_id"] == "a1"));
}

#[test]
fn page_cursor_walks_older_rows_without_overlap() {
    let events: Vec<_> = (1..=5)
        .map(|d| event(&format!("2026-11-0{d}T10:00:00Z"), "a1", "u1", None, &["capabilities.denied_tools"]))
        .collect();
    let (p1, c1) = permission_history_page(&events, "a1", 2, None);
    assert_eq!(p1.len(), 2);
    assert_eq!(p1[0]["timestamp"], "2026-11-05T10:00:00Z");
    let c1 = c1.expect("more rows");
    let (p2, c2) = permission_history_page(&events, "a1", 2, Some(&c1));
    assert_eq!(p2[0]["timestamp"], "2026-11-03T10:00:00Z");
    let (p3, c3) = permission_history_page(&events, "a1", 2, Some(&c2.expect("more rows")));
    assert_eq!(p3.len(), 1);
    assert!(c3.is_none());
}

#[tokio::test]
async fn rpc_is_admin_only_and_filters_by_agent() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "[general]\ndefault_agent = \"dudu\"\n").unwrap();
    write_audit(
        home.path(),
        &[
            event("2026-11-01T10:00:00Z", "a1", "admin-1", None, &["capabilities.denied_tools"]),
            event("2026-11-02T10:00:00Z", "a2", "admin-1", None, &["capabilities.denied_tools"]),
        ],
    );
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    // A non-admin — even this employee's own owner — is refused.
    let refused = err(&handler
        .handle("agents.permission_history", json!({ "agent_id": "a1" }), &employee())
        .await);
    assert!(!refused.is_empty());

    let out = ok(&handler
        .handle("agents.permission_history", json!({ "agent_id": "a1", "limit": 1000 }), &admin())
        .await);
    let rows = out["events"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{out}");
    assert_eq!(rows[0]["agent_id"], "a1");
    assert_eq!(rows[0]["actor"], "admin-1");
    assert!(out["next_cursor"].is_null());

    let missing = err(&handler.handle("agents.permission_history", json!({}), &admin()).await);
    assert!(missing.contains("agent_id"), "{missing}");
}
