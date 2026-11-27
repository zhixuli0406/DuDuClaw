use super::*;
use crate::google_service_account::CacheMode;
use crate::google_workspace::drive_write_policy as policy;

fn enable_write(home: &Home) {
    let mut value: toml::Value = std::fs::read_to_string(home.p().join("config.toml"))
        .unwrap().parse().unwrap();
    let mut enabled = policy::DriveWritePolicy::all_off();
    enabled.create = true;
    let ack = policy::Ack {
        by_user_id: "admin-p8".into(), by_email: "admin@c.test".into(), by_role: "admin".into(),
        at: "2026-11-27T00:00:00Z".into(), digest: policy::digest_of(&enabled),
    };
    value.as_table_mut().unwrap().entry("google").or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut().unwrap().insert("drive_write".into(), policy::render_section(&enabled, &ack));
    std::fs::write(home.p().join("config.toml"), toml::to_string(&value).unwrap()).unwrap();
    assert!(policy::load(home.p()).any_enabled());
}

async fn write_step(home: &Home, people: &dyn People) -> check::Step {
    let configured = duduclaw_core::google_identity::effective_for(home.p(), Some("bot"));
    let effective = delegate_owner::effective_for(home.p(), Some("bot"));
    check::WRITE_TEST_API_BASE.scope(url::Url::parse(&format!("{}/", mock())).unwrap(),
        check::drive_write_step(home.p(), "bot", people, &configured, &effective)).await
}

#[tokio::test]
async fn private_delegate_write_mint_requests_exact_scopes_and_checks_response() {
    let home = Home::new();
    home.ready("bot", "p8-mint@c.test", "");
    for email in ["p8-mint@c.test", "p8-explicit-scope@c.test"] {
        resolve::mint_delegate_for_drive_write(home.p(), email, CacheMode::Bypass).await.unwrap();
        let requests = seen_for(email);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].scope, delegated_scopes::for_drive_write().join(" "));
    }
    for email in ["unauth-p8-mint@c.test", "p8-missing-scope@c.test"] {
        let refusal = resolve::mint_delegate_for_drive_write(home.p(), email, CacheMode::Bypass).await.unwrap_err();
        assert_eq!(refusal.code, "drive_write_admin_scope_missing");
        assert!(!refusal.fallback_eligible);
        assert_eq!(refusal.message_zh, "公司管理員尚未允許雲端硬碟寫入，這次沒有執行，請公司管理員在 Google 管理控制台開放寫入權限。");
    }
    assert_eq!(resolve::mint_delegate(home.p(), "p8-mint@c.test",
        delegated_scopes::for_drive_write(), CacheMode::Bypass).await.unwrap_err().code, "write_scope_isolated");
    assert_eq!(seen_for("p8-mint@c.test").len(), 1);
}

#[tokio::test]
async fn private_home_sa_write_mint_requests_exact_scopes_and_maps_admin_refusal() {
    let home = Home::new();
    let key_path = home.p().join("sa.json");
    std::fs::write(&key_path, key_json(&format!("{}/token", mock()))).unwrap();
    for email in ["p8-home@c.test", "unauth-p8-home@c.test"] {
        std::fs::write(home.p().join("config.toml"), format!(
            "[integrations]\ngoogle_workspace=true\ngoogle_credential_path='service_account'\n[integrations.google_service_account]\nkey_file='sa.json'\nsubject='{email}'\n"
        )).unwrap();
        let result = home_source::home_sa_token_for_drive_write(home.p()).await;
        if email == "p8-home@c.test" { assert!(result.is_ok()); }
        else {
            let refusal = result.unwrap_err();
            assert_eq!(refusal.code, "drive_write_admin_scope_missing");
            assert!(!refusal.fallback_eligible);
        }
        assert_eq!(seen_for(email).len(), 1);
        assert_eq!(seen_for(email)[0].scope, delegated_scopes::for_drive_write().join(" "));
    }
}

#[tokio::test]
async fn service_account_write_and_read_cache_entries_are_separate() {
    let home = Home::new();
    home.ready("bot", "p8-cache@c.test", "");
    let read = &["https://www.googleapis.com/auth/drive.readonly"];
    for _ in 0..2 {
        resolve::mint_delegate_for_drive_write(home.p(), "p8-cache@c.test", CacheMode::Use).await.unwrap();
        resolve::mint_delegate(home.p(), "p8-cache@c.test", read, CacheMode::Use).await.unwrap();
    }
    let requests = seen_for("p8-cache@c.test");
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].scope, delegated_scopes::for_drive_write().join(" "));
    assert_eq!(requests[1].scope, read[0]);
}

#[tokio::test]
async fn drive_write_check_policy_off_makes_no_request() {
    let home = Home::new();
    home.ready("bot", "p8-off@c.test", "");
    let step = write_step(&home, &NoPeople).await;
    assert_eq!(step.id, "drive_write");
    assert!(step.ok);
    assert_eq!(step.detail, "管理員尚未開放");
    assert!(seen_for("p8-off@c.test").is_empty());
    assert_eq!(data_endpoint_reads_for("p8-off@c.test"), 0);
}

#[tokio::test]
async fn drive_write_check_delegate_probe_and_admin_scope_missing() {
    for (email, ok) in [("p8-probe@c.test", true), ("unauth-p8-probe@c.test", false)] {
        let home = Home::new();
        home.ready("bot", email, "");
        enable_write(&home);
        let step = write_step(&home, &NoPeople).await;
        assert_eq!(step.ok, ok, "{step:?}");
        assert_eq!(seen_for(email).len(), 1);
        assert_eq!(seen_for(email)[0].scope, delegated_scopes::for_drive_write().join(" "));
        if ok {
            let reads: Vec<_> = service_reads().lock().unwrap().iter()
                .filter(|(account, _)| account == email).cloned().collect();
            assert_eq!(reads.len(), 1);
            assert_eq!(reads[0].1, "/drive/v3/files?pageSize=1&fields=files(id)&supportsAllDrives=true&includeItemsFromAllDrives=true");
        } else {
            assert_eq!(data_endpoint_reads_for(email), 0);
            assert_eq!(step.detail, "公司管理員尚未允許雲端硬碟寫入，這次沒有執行，請公司管理員在 Google 管理控制台開放寫入權限。");
        }
    }
}

#[tokio::test]
async fn drive_write_check_personal_uses_owner_snapshot_without_minting() {
    use duduclaw_auth::{AccessLevel, UserDb, UserRole};
    use crate::person_credentials::{write_store as store, Secret, Status};
    let home = Home::new();
    let email = "p8-personal@c.test";
    home.ready("bot", email, "");
    home.agent("bot", "[google]\nidentity='owner'\n");
    enable_write(&home);
    let db = std::sync::Arc::new(UserDb::new(&home.p().join("users.db")).unwrap());
    let user = db.create_user(email, "Owner", "test-password", UserRole::Employee).unwrap();
    db.bind_agent(&user.id, "bot", AccessLevel::Owner).unwrap();
    let people = requester_gate::UserDbPeople(db);
    assert_eq!(write_step(&home, &people).await.detail, "尚未連結");
    let metadata = store::Metadata {
        owner_kind: "user".into(), owner_id: user.id.clone(), purpose: "drive_write".into(),
        provider: "google".into(), credential_id: uuid::Uuid::new_v4().to_string(), revision: 1,
        account_email: email.into(), google_sub: "google-p8-owner".into(), hosted_domain: Some("c.test".into()),
        granted_scopes: delegated_scopes::for_drive_write().iter().map(|s| s.to_string()).collect(),
        status: Status::Connected, status_before_suspension: None,
        connected_at: chrono::Utc::now().to_rfc3339(), last_refresh_at: None, oauth_client_id: "p8-client".into(),
        google_data_consent: Some(crate::google_data_consent::Evidence::issue(&user.id).unwrap()),
    };
    let secret = Secret { access_token: "unused-write-token".into(), refresh_token: None, expires_at: Some(1) };
    let key = protection_gate(home.p()).unwrap();
    let epoch = store::begin_connection(home.p(), "user", &user.id).unwrap();
    store::connect(home.p(), &key, metadata.clone(), &secret, epoch).unwrap();
    let step = write_step(&home, &people).await;
    assert!(step.ok);
    assert_eq!(step.detail, "已連結");
    store::mark_needs_reconnect(home.p(), &metadata).unwrap();
    let step = write_step(&home, &people).await;
    assert!(!step.ok);
    assert_eq!(step.detail, "需要重新連結");
    assert!(seen_for(email).is_empty());
    assert_eq!(data_endpoint_reads_for(email), 0);
}

#[tokio::test]
async fn drive_write_check_home_sa_probes_and_other_company_routes_do_not() {
    let home = Home::new();
    home.domains(&["c.test"]);
    home.agent("bot", "[google]\nidentity='home'\n");
    for mode in ["oauth", "apps_script", "service_account"] {
        let section = match mode {
            "apps_script" => "[integrations.google_apps_script]\nurl='https://script.google.com/example'\n",
            "service_account" => "[integrations.google_service_account]\nkey_file='sa.json'\nsubject='p8-home-probe@c.test'\n",
            _ => "",
        };
        std::fs::write(home.p().join("config.toml"), format!(
            "[integrations]\ngoogle_workspace=true\ngoogle_credential_path='{mode}'\n{section}"
        )).unwrap();
        enable_write(&home);
        if mode == "service_account" {
            std::fs::write(home.p().join("sa.json"), key_json(&format!("{}/token", mock()))).unwrap();
        }
        let step = write_step(&home, &NoPeople).await;
        if mode == "service_account" {
            assert!(step.ok, "{step:?}");
            assert_eq!(seen_for("p8-home-probe@c.test").len(), 1);
            assert_eq!(data_endpoint_reads_for("p8-home-probe@c.test"), 1);
        } else {
            assert!(!step.ok);
            assert_eq!(step.detail, "這種公司帳號連線方式不支援雲端硬碟寫入");
            assert!(seen_for("p8-home-probe@c.test").is_empty());
            assert_eq!(data_endpoint_reads_for("p8-home-probe@c.test"), 0);
        }
    }
}

#[tokio::test]
async fn drive_write_check_delegate_owner_probes_once() {
    use duduclaw_auth::{AccessLevel, UserDb, UserRole};
    let home = Home::new();
    let email = "p8-owner-probe@c.test";
    home.ready("bot", email, "");
    home.agent("bot", "[google]\nidentity='delegate:owner'\n");
    enable_write(&home);
    let db = std::sync::Arc::new(UserDb::new(&home.p().join("users.db")).unwrap());
    let owner = db.create_user(email, "Owner", "test-password", UserRole::Employee).unwrap();
    let admin = db.create_user("admin-p8-owner@c.test", "Admin", "test-password", UserRole::Admin).unwrap();
    db.bind_agent(&owner.id, "bot", AccessLevel::Owner).unwrap();
    db.ensure_google_employee_nonce("bot").unwrap();
    let employee = delegate_owner::incarnation(home.p(), "bot").unwrap();
    db.attest_google_delegate_owner(&duduclaw_auth::google_delegate_attestation::AttestationRequest {
        agent_id: "bot", employee_id: &hex::encode(employee.0.0), employee_incarnation: employee.1,
        subject: email, user_id: &owner.id, acknowledge_self_set: true,
    }, &|value| value == email, &admin.id).unwrap();
    let step = write_step(&home, &requester_gate::UserDbPeople(db)).await;
    assert!(step.ok, "{step:?}");
    assert_eq!(seen_for(email).len(), 1);
    assert_eq!(data_endpoint_reads_for(email), 1);
}

#[tokio::test]
async fn drive_write_check_other_failures_are_generic() {
    for (email, reads) in [("gone-p8-probe@c.test", 0), ("p8-probe-denied@c.test", 1)] {
        let home = Home::new();
        home.ready("bot", email, "");
        enable_write(&home);
        let step = write_step(&home, &NoPeople).await;
        assert!(!step.ok);
        assert_eq!(step.detail, "無法確認雲端硬碟寫入連線，請管理員檢查後再試。");
        assert_eq!(seen_for(email).len(), 1);
        assert_eq!(data_endpoint_reads_for(email), reads);
    }
}
