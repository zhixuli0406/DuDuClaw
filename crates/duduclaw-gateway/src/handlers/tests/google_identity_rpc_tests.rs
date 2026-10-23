//! Per-employee Google identity RPCs through `MethodHandler::handle`
//! (in-memory protected-key store; no keychain, no network).
use super::*;

fn ok(frame: &WsFrame) -> Value {
    match frame {
        WsFrame::Response {
            ok: true,
            payload: Some(p),
            ..
        } => p.clone(),
        other => panic!("expected ok, got {other:?}"),
    }
}

fn err(frame: &WsFrame) -> String {
    match frame {
        WsFrame::Response {
            ok: false,
            error: Some(e),
            ..
        } => e
            .as_str()
            .or_else(|| e.get("message").and_then(Value::as_str))
            .expect("refusal must carry a user-facing message")
            .to_string(),
        other => panic!("expected error, got {other:?}"),
    }
}

fn owner_of(agent: &str) -> UserContext {
    let mut access = std::collections::HashMap::new();
    access.insert(agent.to_string(), AccessLevel::Owner);
    UserContext {
        user_id: "owner-1".into(),
        email: "owner@c.test".into(),
        role: UserRole::Employee,
        agent_access: access,
        must_change_password: false,
        team_access: Default::default(),
        acl_generation: 0,
    }
}

async fn call(h: &MethodHandler, method: &str, params: Value, who: &UserContext) -> WsFrame {
    Box::pin(h.handle(method, params, who)).await
}

#[tokio::test]
async fn leaving_a_person_identity_does_not_reconfirm_its_anyone_policy() {
    for identity in ["none", "home"] {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().canonicalize().unwrap();
        let h = MethodHandler::new(home.clone()).await;
        ok(&h.handle_agents_create_as(json!({"name":"bot", "display_name":"Bot"}),
            Some(&UserContext::admin_fallback())).await);
        let path = home.join("agents/bot/agent.toml");
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("\n[google]\nidentity='delegate:alice@c.test'\nrequesters='anyone'\nfallback='home'\n");
        std::fs::write(path, text).unwrap();
        ok(&call(&h, "agents.google_identity_set", json!({"agent_id":"bot", "identity":identity}),
            &UserContext::admin_fallback()).await);
        let table = std::fs::read_to_string(home.join("agents/bot/agent.toml")).unwrap()
            .parse::<toml::Table>().unwrap();
        let google = table["google"].as_table().unwrap();
        assert_eq!(google["identity"].as_str(), Some(identity));
        assert!(!google.contains_key("requesters"));
        assert!(!google.contains_key("fallback"));
    }
}

fn home_with_agent(agent: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap();
    let a = home.join("agents").join(agent);
    std::fs::create_dir_all(&a).unwrap();
    std::fs::write(
        a.join("agent.toml"),
        format!("[agent]\nname = \"{agent}\"\n"),
    )
    .unwrap();
    (dir, home)
}

#[tokio::test]
async fn writes_and_tests_are_admin_only() {
    let (_d, home) = home_with_agent("bot");
    let h = MethodHandler::new(home.clone()).await;
    let owner = owner_of("bot");
    for (method, params) in [
        (
            "agents.google_identity_set",
            json!({"agent_id": "bot", "identity": "none"}),
        ),
        ("google.delegation.get", json!({})),
        ("google.delegation.import_key", json!({"key_json": "{}"})),
        ("google.delegation.remove_key", json!({"confirm": true})),
        (
            "google.delegation.settings_set",
            json!({"allowed_subject_domains": ["c.test"]}),
        ),
        ("google.identity.test", json!({"agent_id": "bot"})),
    ] {
        let e = err(&call(&h, method, params, &owner).await);
        assert!(!e.is_empty(), "{method} must refuse a non-admin owner");
    }
    let text = std::fs::read_to_string(home.join("agents/bot/agent.toml")).unwrap();
    assert!(!text.contains("[google]"), "nothing written: {text}");
    // The owner may read which identity is in effect.
    let v = ok(&call(
        &h,
        "agents.google_identity_get",
        json!({"agent_id": "bot"}),
        &owner,
    )
    .await);
    assert_eq!(v["kind"], "home");
    // A stranger may not.
    let e = err(&call(
        &h,
        "agents.google_identity_get",
        json!({"agent_id": "bot"}),
        &owner_of("other"),
    )
    .await);
    assert!(!e.is_empty());
}

#[tokio::test]
async fn admin_sets_a_delegate_within_the_allowed_domains() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap();
    let h = MethodHandler::new(home.clone()).await;
    let admin = UserContext::admin_fallback();
    // A registered employee (the write path hot-reloads through the registry).
    ok(&h
        .handle_agents_create_as(
            json!({ "name": "bot", "display_name": "Bot" }),
            Some(&admin),
        )
        .await);
    // No allowed domain yet: refused.
    let e = err(&call(
        &h,
        "agents.google_identity_set",
        json!({"agent_id": "bot", "identity": "delegate:alice@c.test"}),
        &admin,
    )
    .await);
    assert!(e.contains("允許的網域"), "{e}");
    ok(&call(
        &h,
        "google.delegation.settings_set",
        json!({"allowed_subject_domains": ["@C.test"]}),
        &admin,
    )
    .await);
    let cfg = std::fs::read_to_string(home.join("config.toml")).unwrap();
    assert!(
        cfg.contains("allowed_subject_domains") && cfg.contains("c.test"),
        "{cfg}"
    );
    let v = ok(&call(
        &h,
        "agents.google_identity_set",
        json!({"agent_id": "bot", "identity": "delegate:Alice@C.test", "fallback": "home"}),
        &admin,
    )
    .await);
    assert_eq!(v["kind"], "delegate");
    assert_eq!(v["email"], "alice@c.test");
    assert_eq!(v["fallback"], "home");
    // Blocked until the protection key and the sealed key exist.
    assert_eq!(v["state"], "blocked");
    ok(&call(
        &h,
        "agents.google_identity_set",
        json!({"agent_id": "bot", "identity": "delegate:alice@c.test", "requesters": "owner_only"}),
        &admin,
    )
    .await);
    let preserved = ok(&call(
        &h,
        "agents.google_identity_set",
        json!({"agent_id": "bot", "identity": "delegate:alice@c.test", "fallback": "home"}),
        &admin,
    )
    .await);
    assert_eq!(
        preserved["requesters"], "owner_only",
        "omission must retain the current restriction"
    );
    assert!(
        !err(&call(
            &h,
            "agents.google_identity_set",
            json!({"agent_id": "bot", "identity": "delegate:alice@c.test", "requesters": 42}),
            &admin
        )
        .await)
        .is_empty()
    );
    let text = std::fs::read_to_string(home.join("agents/bot/agent.toml")).unwrap();
    assert!(
        text.contains("identity = \"delegate:alice@c.test\""),
        "{text}"
    );
    // Personal edition rejects B2b selectors with the structured edition gate.
    h.set_edition_override(Some(duduclaw_core::EditionProfile::Personal)).await;
    for selector in ["owner", "user:u1"] {
        let frame = call(&h, "agents.google_identity_set", json!({"agent_id":"bot","identity":selector}), &admin).await;
        assert!(!err(&frame).is_empty());
        let WsFrame::Response { ok: false, error: Some(error), .. } = frame else { panic!("expected edition refusal") };
        assert_eq!(error["code"], super::super::gates::ENTERPRISE_ONLY_ERROR_CODE);
    }
    // Unconfirmed widening and malformed or missing targets stay refused.
    for params in [
        json!({"agent_id": "bot", "identity": "delegate:alice@c.test", "requesters": "anyone"}),
        json!({"agent_id": "bot", "identity": "delegate:not-an-email"}),
        json!({"agent_id": "ghost", "identity": "none"}),
    ] {
        assert!(!err(&call(&h, "agents.google_identity_set", params, &admin).await).is_empty());
    }
    let audit = std::fs::read_to_string(home.join("security_audit.jsonl")).unwrap_or_default();
    assert!(audit.contains("agent_google_identity_set"), "{audit}");
}

#[tokio::test]
async fn import_needs_protection_and_status_never_returns_the_key() {
    let (_d, home) = home_with_agent("bot");
    duduclaw_core::gateway_instance::acquire(&home).unwrap();
    let h = MethodHandler::new(home.clone()).await;
    let admin = UserContext::admin_fallback();
    let key = json!({
        "type": "service_account",
        "client_email": "sa@proj.iam.gserviceaccount.test",
        "client_id": "42",
        "private_key": "-----BEGIN PRIVATE KEY-----\nNOTREALBUTSECRETLOOKING\n-----END PRIVATE KEY-----\n",
        "token_uri": "https://oauth2.googleapis.com/token",
    })
    .to_string();
    let e = err(&call(
        &h,
        "google.delegation.import_key",
        json!({"key_json": key}),
        &admin,
    )
    .await);
    assert!(e.contains("安全"), "protection first: {e}");
    ok(&call(
        &h,
        "security.credential_protection_enable",
        json!({}),
        &admin,
    )
    .await);
    let v = ok(&call(
        &h,
        "google.delegation.import_key",
        json!({"key_json": key}),
        &admin,
    )
    .await);
    assert_eq!(v["key"]["client_email"], "sa@proj.iam.gserviceaccount.test");
    let status = ok(&call(&h, "google.delegation.get", json!({}), &admin).await);
    let text = status.to_string();
    assert!(!text.contains("NOTREALBUTSECRETLOOKING"), "{text}");
    assert_eq!(status["key"]["client_id"], "42");
    assert_eq!(status["protection"]["state"], "ready");
    let sealed =
        std::fs::read_to_string(home.join("protected/google-service-account.sealed")).unwrap();
    assert!(!sealed.contains("NOTREALBUTSECRETLOOKING"));
    let e = err(&call(&h, "google.delegation.remove_key", json!({}), &admin).await);
    assert!(e.contains("confirm"), "{e}");
    ok(&call(
        &h,
        "google.delegation.remove_key",
        json!({"confirm": true}),
        &admin,
    )
    .await);
    assert!(
        !home
            .join("protected/google-service-account.sealed")
            .exists()
    );
}

#[test]
fn owner_may_only_narrow_google() {
    use crate::handlers::agent_authority::authority_diff;
    let t = |s: &str| -> toml::Table { s.parse().unwrap() };
    let base = t(
        "[agent]\nname='a'\n[google]\nidentity='delegate:a@c.test'\nfallback='home'\nrequesters='bound_people'\n",
    );
    for (after, needs_admin) in [
        ("[agent]\nname='a'\n[google]\nidentity='none'\n", false),
        (
            "[agent]\nname='a'\n[google]\nidentity='delegate:a@c.test'\nrequesters='owner_only'\n",
            false,
        ),
        (
            "[agent]\nname='a'\n[google]\nidentity='delegate:ceo@c.test'\nfallback='home'\n",
            true,
        ),
        (
            "[agent]\nname='a'\n[google]\nidentity='delegate:a@c.test'\nfallback='home'\nrequesters='anyone'\n",
            true,
        ),
        ("[agent]\nname='a'\n", true),
    ] {
        let diff = authority_diff(&base, &t(after));
        assert_eq!(
            diff.iter().any(|c| c.key == "google"),
            needs_admin,
            "{after}"
        );
    }
    // Adding a delegate to an employee without [google] needs an admin.
    let plain = t("[agent]\nname='a'\n");
    assert!(
        authority_diff(
            &plain,
            &t("[agent]\nname='a'\n[google]\nidentity='delegate:x@c.test'\n")
        )
        .iter()
        .any(|c| c.key == "google")
    );
    assert!(
        !authority_diff(&plain, &t("[agent]\nname='a'\n[google]\nidentity='none'\n"))
            .iter()
            .any(|c| c.key == "google")
    );
}

#[tokio::test]
async fn personal_mail_read_access_survives_selector_changes_but_never_grants_send() {
    let (_dir, home) = home_with_agent("bot");
    let db = Arc::new(UserDb::new(&home.join("users.db")).unwrap());
    let user = db
        .create_user("alice@c.test", "Alice", "test-password", UserRole::Employee)
        .unwrap();
    let handler = MethodHandler::new(home.clone()).await;
    let mut ctx = owner_of("bot");
    ctx.user_id = user.id.clone();
    ctx.email = user.email;
    let receipt = |id: &str, source: &str, account: Option<&str>| {
        let incoming = crate::mail::IncomingMail {
            source: source.into(),
            external_id: id.into(),
            from: "sender@c.test".into(),
            subject: id.into(),
            body: format!("content for {id}"),
        };
        match crate::mail::ingest_inbound_for_account(
            &home,
            &crate::mail::MailConfig::default(),
            &crate::mail::read_mailbox(&home),
            &incoming,
            "bot",
            account,
        ) {
            crate::mail::IngestOutcome::Stored { mail_id, .. } => mail_id,
            other => panic!("{other:?}"),
        }
    };
    let shared = receipt("same-id", "gmail", None);
    let personal = receipt("same-id", "gmail", Some("alice@c.test"));
    let other_person = receipt("other-person", "gmail", Some("bob@c.test"));
    let dropfolder = receipt("dropfolder", "dropfolder", None);
    assert_ne!(
        shared, personal,
        "Gmail ids from different accounts never alias"
    );
    crate::google_identity::mailbox::protect(&home, "alice@c.test").unwrap();
    // The protection persists without Gmail enabled or a current delegate.
    let list = ok(&call(&handler, "mail.list", json!({}), &ctx).await);
    assert_eq!(list["count"], 1);
    assert_eq!(list["messages"][0]["mail_id"], personal);
    ok(&call(&handler, "mail.read", json!({"mail_id": personal}), &ctx).await);
    for id in [&shared, &other_person, &dropfolder] {
        assert_eq!(
            err(&call(&handler, "mail.read", json!({"mail_id": id}), &ctx).await),
            "mail not found"
        );
        assert!(
            !crate::mail::get_inbox_item(&home, id).unwrap().read,
            "an unauthorized read cannot mark it read"
        );
    }
    // Switching to a delegate does not relabel historical shared receipts.
    std::fs::write(
        home.join("agents/bot/agent.toml"),
        "[agent]\nname='bot'\n[google]\nidentity='delegate:alice@c.test'\n",
    )
    .unwrap();
    assert_eq!(
        ok(&call(&handler, "mail.list", json!({}), &ctx).await)["count"],
        1
    );
    // Switching back and collecting a company/fallback receipt stays shared.
    std::fs::write(
        home.join("agents/bot/agent.toml"),
        "[agent]\nname='bot'\n[google]\nidentity='home'\n",
    )
    .unwrap();
    let fallback = receipt("fallback-company", "gmail", None);
    assert_eq!(
        err(&call(&handler, "mail.read", json!({"mail_id": fallback}), &ctx).await),
        "mail not found"
    );
    for method in ["mail.decide", "mail.archive", "mail.outbox"] {
        assert!(
            !err(&call(&handler, method, json!({}), &ctx).await).is_empty(),
            "{method}"
        );
    }
    let mut stranger = ctx.clone();
    stranger.user_id = "missing".into();
    stranger.role = UserRole::Manager;
    let list = ok(&call(&handler, "mail.list", json!({}), &stranger).await);
    assert_eq!(
        list["count"], 3,
        "a Manager without a matching active identity reads only shared/legacy/dropfolder"
    );
    db.set_user_status(&user.id, duduclaw_auth::models::UserStatus::Suspended)
        .unwrap();
    assert!(
        handler.mailbox_reader_gate(&ctx).await.is_err(),
        "stale context cannot bypass suspension"
    );
    db.set_user_status(&user.id, duduclaw_auth::models::UserStatus::Active)
        .unwrap();
    crate::google_identity::mailbox::protect(&home, "bob@c.test").unwrap();
    let list = ok(&call(&handler, "mail.list", json!({}), &ctx).await);
    assert_eq!(
        list["count"], 1,
        "mixed ledgers are filtered per receipt, never granted wholesale"
    );
}
