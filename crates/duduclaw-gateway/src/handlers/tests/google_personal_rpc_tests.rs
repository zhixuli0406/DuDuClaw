use super::*;

fn payload(frame: WsFrame) -> Value {
    match frame {
        WsFrame::Response {
            ok: true,
            payload: Some(value),
            ..
        } => value,
        other => panic!("expected success: {other:?}"),
    }
}
fn refused(frame: WsFrame) {
    assert!(
        matches!(frame, WsFrame::Response { ok: false, .. }),
        "expected refusal: {frame:?}"
    );
}
async fn rpc(h: &MethodHandler, method: &str, params: Value, ctx: &UserContext) -> WsFrame {
    Box::pin(h.handle(method, params, ctx)).await
}

#[tokio::test]
async fn personal_rpc_self_consent_owner_change_and_legacy_session_are_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap();
    let db = Arc::new(UserDb::new(&home.join("users.db")).unwrap());
    let alice = db
        .create_user(
            "alice@c.test",
            "Alice",
            "fixture-password",
            UserRole::Employee,
        )
        .unwrap();
    let bob = db
        .create_user("bob@c.test", "Bob", "fixture-password", UserRole::Employee)
        .unwrap();
    let admin = db
        .create_user("admin@c.test", "Admin", "fixture-password", UserRole::Admin)
        .unwrap();
    db.bind_agent(&alice.id, "bot", AccessLevel::Owner).unwrap();
    db.bind_agent(&bob.id, "bot", AccessLevel::Owner).unwrap();
    let h = MethodHandler::new(home.clone()).await;
    h.set_user_db(
        db.clone(),
        Arc::new(JwtConfig::new(b"fixture-signing-key-32-bytes-only")),
    )
    .await;
    h.set_edition_override(Some(duduclaw_core::EditionProfile::Enterprise))
        .await;
    let alice_ctx = db.live_user_context(&alice.id).unwrap().unwrap();
    let admin_ctx = db.live_user_context(&admin.id).unwrap().unwrap();
    // Register a complete employee through the real creation path. A minimal
    // hand-written TOML is not a valid AgentConfig and is skipped by the scan.
    payload(h.handle_agents_create_as(json!({"name":"bot","display_name":"Bot"}), Some(&admin_ctx)).await);
    // Admin may select Alice, but the selection creates no permission.
    payload(
        rpc(
            &h,
            "agents.google_identity_set",
            json!({"agent_id":"bot","identity":format!("user:{}",alice.id)}),
            &admin_ctx,
        )
        .await,
    );
    assert_eq!(db.google_consent(&alice.id, "bot").unwrap(), None);
    refused(rpc(&h,"me.google.consents.set",json!({"agent_id":"bot","user_id":alice.id,"allowed":true,"confirm_shared_access":true}),&admin_ctx).await);
    assert_eq!(db.google_consent(&alice.id, "bot").unwrap(), None);
    // The person param cannot redirect a self-service grant to Bob.
    payload(
        rpc(
            &h,
            "me.google.consents.set",
            json!({"agent_id":"bot","user_id":bob.id,"allowed":true,"confirm_shared_access":true}),
            &alice_ctx,
        )
        .await,
    );
    assert_eq!(db.google_consent(&alice.id, "bot").unwrap(), Some(true));
    assert_eq!(db.google_consent(&bob.id, "bot").unwrap(), None);
    refused(rpc(&h,"me.google.connect_start",json!({"redirect_origin":"http://127.0.0.1:18789","session_ref":uuid::Uuid::new_v4(),"jti":uuid::Uuid::new_v4(),"user_id":bob.id}),&alice_ctx).await);
    refused(rpc(&h,"agents.google_identity_set",json!({"agent_id":"bot","identity":format!("user:{}",bob.id),"confirm_shared_access":true}),&alice_ctx).await);
    refused(
        rpc(
            &h,
            "agents.google_identity_set",
            json!({"agent_id":"bot","identity":format!("user:{}",alice.id)}),
            &alice_ctx,
        )
        .await,
    );
    payload(rpc(&h,"agents.google_identity_set",json!({"agent_id":"bot","identity":format!("user:{}",alice.id),"confirm_shared_access":true}),&alice_ctx).await);
    // A stale Owner context cannot keep changing the employee after unbind.
    db.unbind_agent(&alice.id, "bot").unwrap();
    refused(
        rpc(
            &h,
            "agents.google_identity_set",
            json!({"agent_id":"bot","identity":"none"}),
            &alice_ctx,
        )
        .await,
    );
    h.set_edition_override(Some(duduclaw_core::EditionProfile::Personal))
        .await;
    refused(rpc(&h, "me.google.status", json!({}), &admin_ctx).await);
    refused(
        rpc(
            &h,
            "agents.primary_owner_set",
            json!({"agent_id":"bot","user_id":bob.id}),
            &admin_ctx,
        )
        .await,
    );
}

#[test]
fn personal_rpc_family_is_enterprise_only_including_future_members() {
    for method in [
        "me.google.status",
        "me.google.connect_start",
        "me.google.future",
        "users.google_disconnect",
        "agents.primary_owner_set",
    ] {
        assert!(super::super::gates::is_enterprise_only_method(method));
    }
    assert!(!super::super::gates::is_enterprise_only_method(
        "me.google_export"
    ));
}
