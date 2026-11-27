use super::{owner_test_support::{Fixture, AGENT}, *};
use serde_json::json;
use duduclaw_core::authority_coordinator::AuthorityCoordinator;

#[tokio::test]
async fn no_google_connection_refuses_before_owner_binding() {
    let f = Fixture::new();
    let config = f.home.path().join("config.toml");
    std::fs::write(&config, std::fs::read_to_string(&config).unwrap()
        .replace("google_workspace=true", "google_workspace=false")).unwrap();
    std::fs::remove_file(delegation_key::path(f.home.path())).unwrap();
    let turn = uuid::Uuid::new_v4().to_string();
    let _guard = crate::knowledge::turn_requesters::register(AGENT, &turn,
        duduclaw_knowledge::requester::RequesterIdentity::DashboardUser { user_id:f.alice.clone() }).unwrap();
    let probe = route_test_probe::Guard::install(f.home.path());
    for turn in [None, Some(turn)] {
        for (tool, arguments) in [
            ("google_chat_send", json!({"space":"spaces/local", "text":"never sent"})),
            ("gmail_send_draft", json!({"draft_id":"local-draft"})),
            ("calendar_delete_event", json!({"event_id":"local-event"})),
        ] {
            let body = serde_json::to_vec(&json!({"kind":"employee_prepare", "tool":tool,
                "arguments":arguments, "turn_id":turn, "call_id":"no-connection"})).unwrap();
            let (status, result) = f.post(execute::ROUTE, body.clone(), f.signed(&body)).await;
            assert_eq!(status, 403, "{tool}: {result}");
            assert_eq!(result["code"], "disabled", "{tool}: {result}");
            assert_eq!(result["message"], "管理員尚未開啟 Google 整合。");
        }
    }
    assert_eq!(probe.counts(), [0, 0]);
    assert!(!f.home.path().join("approvals.db").exists());
}

#[tokio::test]
async fn no_google_connection_member_chat_reports_not_connected() {
    let f = Fixture::new();
    let config = f.home.path().join("config.toml");
    let mut raw = std::fs::read_to_string(&config).unwrap();
    raw.push_str("\n[google]\nchat_send=true\n");
    std::fs::write(config, raw).unwrap();
    std::fs::write(f.home.path().join("agents").join(AGENT).join("agent.toml"),
        format!("[agent]\nname='{AGENT}'\n[google]\nidentity='owner'\n")).unwrap();
    f.db.set_google_consent(&f.alice, AGENT, true, &f.alice).unwrap();
    let turn = uuid::Uuid::new_v4().to_string();
    let _guard = crate::knowledge::turn_requesters::register(AGENT, &turn,
        duduclaw_knowledge::requester::RequesterIdentity::DashboardUser { user_id:f.alice.clone() }).unwrap();
    let probe = route_test_probe::Guard::install(f.home.path());
    let body = serde_json::to_vec(&json!({"kind":"employee_prepare", "tool":"google_chat_send",
        "arguments":{"space":"spaces/local", "text":"never sent"},
        "turn_id":turn, "call_id":"member-no-connection"})).unwrap();
    let (status, result) = f.post(execute::ROUTE, body.clone(), f.signed(&body)).await;
    assert_eq!(status, 403, "{result}");
    assert_eq!(result["code"], "not_connected", "{result}");
    assert_eq!(result["message"], "負責人還沒連結 Google，請本人到「我的連線」按「連結我的 Google」。");
    assert_eq!(probe.counts(), [0, 0]);
    assert!(!f.home.path().join("approvals.db").exists());
}

#[tokio::test]
async fn notification_db_opens_preserve_pending_and_approval_waiting_owner_calls() {
    for require_approval in [false, true] {
        let f = Fixture::new();
        if require_approval {
            let _write = AuthorityCoordinator::new(f.home.path()).write().unwrap();
            let path = f.home.path().join("agents").join(AGENT).join("agent.toml");
            let mut config = std::fs::read_to_string(&path).unwrap();
            config.push_str("\n[capabilities]\napproval_required_tools=['google_status']\n");
            std::fs::write(path, config).unwrap();
        }
        // google_status exercises real prepare/execute validation without
        // requiring a provider or credentials. The probe forbids network work.
        let probe = route_test_probe::Guard::install(f.home.path());
        let before = f.snapshot();
        let mut request = json!({"kind":"employee_prepare", "tool":"google_status",
            "arguments":{}, "turn_id":f.turn, "call_id":"notification-open"});
        let body = serde_json::to_vec(&request).unwrap();
        let (status, prepared) = f.post(execute::ROUTE, body.clone(), f.signed(&body)).await;
        assert_eq!(status, 200, "{prepared}");
        let digest = prepared["result"]["approval_binding_digest"].clone();
        assert!(digest.is_string());
        let broker = crate::approval::ApprovalBroker::open(f.home.path()).unwrap();
        let approval = if require_approval {
            Some(broker.request(AGENT, "mcp_call", "owner notification fixture",
                json!({"arguments":request["arguments"], "google_owner_binding":digest}), 300).await.unwrap())
        } else { None };

        drop(crate::decision_notify::open_user_db(f.home.path()).unwrap().unwrap());
        assert!(crate::decision_notify::approver_links(f.home.path()).is_empty());
        // approval_notify also uses the constructor directly.
        drop(duduclaw_auth::UserDb::new(&f.home.path().join("users.db")).unwrap());
        assert_eq!(before.epoch, AuthorityCoordinator::new(f.home.path()).epoch().unwrap());
        drop(before.lease(f.home.path(), AGENT).await.unwrap());
        if let Some(approval) = approval {
            broker.decide(&approval, true, "fixture-admin").await.unwrap();
        }
        request["kind"] = json!("employee");
        request["approval_binding_digest"] = digest;
        let body = serde_json::to_vec(&request).unwrap();
        let (status, result) = f.post(execute::ROUTE, body.clone(), f.signed(&body)).await;
        assert_eq!(status, 200, "approval={require_approval}: {result}");
        assert_eq!(probe.counts(), [0, 0]);
    }
}

#[test]
fn owner_aba_and_revoke_reauth_never_restore_a_snapshot() {
    let f = Fixture::new();
    let first = f.snapshot();
    f.db.set_google_primary_owner(AGENT, &f.bob, &f.admin).unwrap();
    f.db.set_google_primary_owner(AGENT, &f.alice, &f.admin).unwrap();
    f.attest();
    assert!(first.validate(f.home.path(), AGENT).is_err());
    let second = f.snapshot();
    f.db.revoke_google_delegate_attestation(AGENT, &f.admin).unwrap();
    f.attest();
    let third = f.snapshot();
    assert_ne!(second.attestation_revision, third.attestation_revision);
    assert!(second.validate(f.home.path(), AGENT).is_err());
}

#[tokio::test]
async fn revocation_between_mint_and_send_denies_the_next_lease() {
    let f = Fixture::new();
    let snapshot = f.snapshot();
    f.db.revoke_google_delegate_attestation(AGENT, &f.admin).unwrap();
    assert!(snapshot.lease(f.home.path(), AGENT).await.is_err());
}

/// Deterministic ordering: prepare and capture authority, commit the real
/// public writer, then execute the saved call. No sleeps or network access.
async fn credential_writer_retires_pending_owner_call(writer: &str) {
    struct UnusedCipher;
    impl duduclaw_auth::mfa::MfaCipher for UnusedCipher {
        fn encrypt(&self, _: &str) -> Result<String, String> { panic!("password proof needs no cipher") }
        fn decrypt(&self, _: &str) -> Result<String, String> { panic!("password proof needs no cipher") }
    }
    let f = Fixture::new();
    let now = chrono::Utc::now().timestamp();
    // Arrange legacy/invited/bootstrap states before capturing authority.
    let raw = rusqlite::Connection::open(f.home.path().join("users.db")).unwrap();
    let invite = if writer == "invite_password" {
        raw.execute("UPDATE users SET password_hash='' WHERE id=?1", [&f.alice]).unwrap();
        Some(f.db.issue_user_invite(&f.alice, &f.admin).unwrap())
    } else { None };
    if writer == "first_run_claim" {
        raw.execute("UPDATE users SET bootstrap_owner=1,must_change_password=1 WHERE id=?1", [&f.alice]).unwrap();
    }
    if matches!(writer, "operator_mfa_reset" | "admin_mfa_reset") {
        raw.execute("INSERT INTO mfa_credentials(user_id,secret_enc,last_step) VALUES(?1,'fixture',0)", [&f.alice]).unwrap();
        assert!(f.db.mfa_active(&f.alice).unwrap());
    }
    drop(raw);
    let reauth = if writer == "admin_mfa_reset" {
        Some(f.db.mfa_factor_password_challenge(&f.admin, "fixture-password", "owner-race", now).unwrap())
    } else { None };
    let probe = route_test_probe::Guard::install(f.home.path());
    let before = f.snapshot();
    let mut request = json!({"kind":"employee_prepare", "tool":"gmail_read",
        "arguments":{"message_id":"m1"}, "turn_id":f.turn, "call_id":writer});
    let body = serde_json::to_vec(&request).unwrap();
    let (status, prepared) = f.post(execute::ROUTE, body.clone(), f.signed(&body)).await;
    assert_eq!(status, 200, "{writer}: {prepared}");
    assert!(prepared["result"]["approval_binding_digest"].is_string());
    match writer {
        "self_revoke_all" => f.db.remember_revoke(&f.alice, None).unwrap(),
        "admin_revoke_all" => {
            let epoch = f.db.get_user(&f.admin).unwrap().unwrap().credential_epoch;
            f.db.remember_admin_revoke_all(&f.admin, epoch, &f.alice).unwrap();
        },
        "operator_mfa_reset" => f.db.mfa_operator_reset("alice@c.test").unwrap(),
        "admin_mfa_reset" => {
            let proof = duduclaw_auth::mfa::AdminResetProof::Password {
                reauth_token: reauth.as_deref().unwrap(), session: "owner-race",
            };
            f.db.mfa_admin_reset_checked(&f.admin, &f.alice, proof, &UnusedCipher, now).unwrap();
        },
        "invite_password" => assert_eq!(f.db.consume_user_invite(&invite.unwrap().token,
            "replacement-password").unwrap().as_deref(), Some(f.alice.as_str())),
        "first_run_claim" => assert!(f.db.claim_default_admin_with_email(
            Some("claimed@c.test"), "replacement-password").unwrap()),
        "user_epoch_only" => {
            // Defense in depth: an epoch-only mutation must invalidate the
            // snapshot even without a corresponding global epoch change.
            rusqlite::Connection::open(f.home.path().join("users.db")).unwrap()
                .execute("UPDATE users SET credential_epoch=credential_epoch+1 WHERE id=?1", [&f.alice]).unwrap();
        },
        _ => panic!("unknown credential writer"),
    }
    assert!(f.db.get_user(&f.alice).unwrap().unwrap().credential_epoch > before.credential_epoch);
    let epoch = AuthorityCoordinator::new(f.home.path()).epoch().unwrap();
    if writer == "user_epoch_only" { assert_eq!(before.epoch, epoch); }
    else { assert_ne!(before.epoch, epoch, "writer must publish under the authority lease"); }
    assert!(before.lease(f.home.path(), AGENT).await.is_err());
    request["kind"] = json!("employee");
    request["approval_binding_digest"] = prepared["result"]["approval_binding_digest"].clone();
    let body = serde_json::to_vec(&request).unwrap();
    let (status, result) = f.post(execute::ROUTE, body.clone(), f.signed(&body)).await;
    assert_eq!(status, 403, "{writer}: {result}");
    assert_eq!(probe.counts(), [0, 0], "{writer}: pending call must neither mint nor send");
}

#[tokio::test]
async fn owner_self_revoke_all_prevents_pending_send() {
    credential_writer_retires_pending_owner_call("self_revoke_all").await;
}
#[tokio::test]
async fn admin_revoke_all_prevents_pending_owner_send() {
    credential_writer_retires_pending_owner_call("admin_revoke_all").await;
}
#[tokio::test]
async fn operator_mfa_reset_prevents_pending_owner_send() {
    credential_writer_retires_pending_owner_call("operator_mfa_reset").await;
}
#[tokio::test]
async fn admin_mfa_reset_prevents_pending_owner_send() {
    credential_writer_retires_pending_owner_call("admin_mfa_reset").await;
}
#[tokio::test]
async fn invite_password_prevents_pending_owner_send() {
    credential_writer_retires_pending_owner_call("invite_password").await;
}
#[tokio::test]
async fn first_run_email_claim_prevents_pending_owner_send() {
    credential_writer_retires_pending_owner_call("first_run_claim").await;
}
#[tokio::test]
async fn user_credential_epoch_alone_prevents_pending_owner_send() {
    credential_writer_retires_pending_owner_call("user_epoch_only").await;
}

#[tokio::test]
async fn route_rejects_bad_signature_replay_subject_and_old_token_clients() {
    let f = Fixture::new();
    let probe = route_test_probe::Guard::install(f.home.path());
    let body = serde_json::to_vec(&json!({"kind":"employee_prepare", "tool":"google_status",
        "arguments":{}, "turn_id":f.turn, "call_id":"test-call"})).unwrap();
    let headers = f.signed(&body);
    let mut bad = headers.clone();
    bad.insert(crate::computer_use_sessions::auth::SIGNATURE_HEADER, "00".parse().unwrap());
    assert_eq!(f.post(execute::ROUTE, body.clone(), bad).await.0, 401);
    assert_eq!(f.post(execute::ROUTE, body.clone(), headers.clone()).await.0, 200);
    assert_eq!(f.post(execute::ROUTE, body, headers).await.0, 401);
    let subject = serde_json::to_vec(&json!({"kind":"employee_prepare", "tool":"google_status",
        "arguments":{}, "turn_id":f.turn, "call_id":"bad-subject", "subject":"other@c.test"})).unwrap();
    assert_eq!(f.post(execute::ROUTE, subject.clone(), f.signed(&subject)).await.0, 400);
    let token = serde_json::to_vec(&json!({"tool":"gmail_read", "turn_id":f.turn})).unwrap();
    let (status, value) = f.post("/api/internal/google-token", token.clone(), f.signed(&token)).await;
    assert_eq!(status, 403);
    assert_eq!(value["code"], "owner_execution_required");
    assert_eq!(probe.counts(), [0, 0]);
}

#[tokio::test]
async fn approval_required_without_grant_never_mints_or_sends() {
    let f = Fixture::new();
    let path = f.home.path().join("agents").join(AGENT).join("agent.toml");
    let _write = AuthorityCoordinator::new(f.home.path()).write().unwrap();
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str("\n[capabilities]\napproval_required_tools=['gmail_read']\n");
    std::fs::write(path, text).unwrap();
    drop(_write);
    let probe = route_test_probe::Guard::install(f.home.path());
    let request = json!({"kind":"employee_prepare", "tool":"gmail_read", "arguments":{"message_id":"m1"},
        "turn_id":f.turn, "call_id":"denied-call"});
    let body = serde_json::to_vec(&request).unwrap();
    let (status, prepared) = f.post(execute::ROUTE, body.clone(), f.signed(&body)).await;
    assert_eq!(status, 200, "{prepared}");
    let broker = crate::approval::ApprovalBroker::open(f.home.path()).unwrap();
    let approval = broker.request(AGENT, "mcp_call", "owner denied fixture",
        json!({"arguments":request["arguments"], "google_owner_binding":prepared["result"]["approval_binding_digest"]}), 300).await.unwrap();
    broker.decide(&approval, false, "fixture-admin").await.unwrap();
    let mut execute = request;
    execute["kind"] = json!("employee");
    execute["approval_binding_digest"] = prepared["result"]["approval_binding_digest"].clone();
    let body = serde_json::to_vec(&execute).unwrap();
    let (status, result) = f.post(execute::ROUTE, body.clone(), f.signed(&body)).await;
    assert_eq!(status, 403);
    assert_eq!(result["code"], "approval_denied");
    assert_eq!(probe.counts(), [0, 0]);
}

#[tokio::test]
async fn approval_owner_a_cannot_execute_as_b_or_reuse_a_consumed_call() {
    let f = Fixture::new();
    let request = json!({"kind":"employee_prepare", "tool":"google_status", "arguments":{},
        "turn_id":f.turn, "call_id":"owner-bound"});
    let body = serde_json::to_vec(&request).unwrap();
    let (status, prepared) = f.post(execute::ROUTE, body.clone(), f.signed(&body)).await;
    assert_eq!(status, 200);
    f.db.set_google_primary_owner(AGENT, &f.bob, &f.admin).unwrap();
    f.attest();
    let mut request = request;
    request["kind"] = json!("employee");
    request["approval_binding_digest"] = prepared["result"]["approval_binding_digest"].clone();
    let body = serde_json::to_vec(&request).unwrap();
    assert_eq!(f.post(execute::ROUTE, body.clone(), f.signed(&body)).await.0, 403);
    assert_eq!(f.post(execute::ROUTE, body.clone(), f.signed(&body)).await.0, 403);
}

#[tokio::test]
async fn all_native_tools_enter_the_employee_route_without_minting() {
    let f = Fixture::new();
    let probe = route_test_probe::Guard::install(f.home.path());
    for tool in duduclaw_core::google_identity::NATIVE_GOOGLE_TOOLS {
        let body = serde_json::to_vec(&json!({"kind":"employee_prepare", "tool":tool,
            "arguments":{}, "turn_id":f.turn, "call_id":format!("coverage-{tool}")})).unwrap();
        let (status, result) = f.post(execute::ROUTE, body.clone(), f.signed(&body)).await;
        // Missing tool arguments / Chat consent are legitimate preflight refusals;
        // neither is a route or closed-vocabulary failure.
        assert!(status == 200 || status == 403, "{tool}: {result}");
        assert_ne!(result["code"], "bad_request", "{tool}");
    }
    assert_eq!(probe.counts(), [0, 0]);
}

#[test]
fn all_25_effect_tools_have_a_closed_native_template() {
    use crate::google_workspace::native_call::NativeCallTemplate;
    let mut count = 0;
    for tool in duduclaw_core::google_identity::NATIVE_GOOGLE_TOOLS {
        if duduclaw_core::google_identity::is_drive_write_tool(tool) {
            assert!(NativeCallTemplate::prepare(tool, &json!({})).is_err(), "{tool}");
            continue;
        }
        let args = match *tool {
            "google_status" => continue,
            "gmail_search" | "drive_search" => json!({"query":"test"}),
            "gmail_read" => json!({"message_id":"message"}),
            "gmail_send_draft" => json!({"draft_id":"draft"}),
            "calendar_delete_event" => json!({"event_id":"event"}),
            "gmail_create_draft" => json!({"to":"alice@c.test", "subject":"test", "body":"test"}),
            "calendar_create_event" => json!({"summary":"test", "start":"2026-11-25T10:00:00Z", "end":"2026-11-25T11:00:00Z"}),
            "calendar_update_event" => json!({"event_id":"event", "summary":"test"}),
            "sheets_read" => json!({"spreadsheet_id":"sheet", "range":"A1:B2"}),
            "sheets_append" => json!({"spreadsheet_id":"sheet", "range":"A1", "values":["cell"]}),
            "sheets_update" => json!({"spreadsheet_id":"sheet", "range":"A1", "values":[["cell"]]}),
            "sheets_create" | "docs_create" | "gtasks_create" => json!({"title":"test"}),
            "forms_get" | "forms_list_responses" => json!({"form_id":"form"}),
            "gtasks_complete" => json!({"task_id":"task"}),
            "drive_read" => json!({"file_id":"file"}),
            "docs_read" => json!({"document_id":"doc"}),
            "docs_append" => json!({"document_id":"doc", "text":"test"}),
            "docs_replace_text" => json!({"document_id":"doc", "find":"old", "replace":"new"}),
            "slides_read" => json!({"presentation_id":"slides"}),
            "google_chat_send" => json!({"space":"spaces/AAAA","text":"text"}),
            "google_chat_messages" => json!({"space":"spaces/AAAA"}),
            "google_chat_spaces" | "calendar_list_events" | "gtasks_lists" | "gtasks_list" => json!({}),
            other => panic!("missing owner coverage fixture: {other}"),
        };
        let template = NativeCallTemplate::prepare(tool, &args).unwrap_or_else(|e| panic!("{tool}: {e}"));
        template.validate_closed().unwrap_or_else(|e| panic!("{tool}: {e}"));
        count += 1;
    }
    assert_eq!(count, 28);
}

#[test]
fn email_restore_and_suspension_resume_retire_previous_authority() {
    let f = Fixture::new();
    let first = f.snapshot();
    f.db.change_own_email(&f.alice, "changed@c.test").unwrap();
    f.db.change_own_email(&f.alice, "alice@c.test").unwrap();
    f.attest();
    assert!(first.validate(f.home.path(), AGENT).is_err());
    let second = f.snapshot();
    f.db.set_user_status(&f.alice, duduclaw_auth::UserStatus::Suspended).unwrap();
    f.db.set_user_status(&f.alice, duduclaw_auth::UserStatus::Active).unwrap();
    f.attest();
    assert!(second.validate(f.home.path(), AGENT).is_err());
}

#[tokio::test]
async fn google_token_refuses_all_drive_write_tools_before_resolution_and_audits() {
    let f = Fixture::new();
    let probe = route_test_probe::Guard::install(f.home.path());
    for tool in crate::google_workspace::drive_write_policy::DRIVE_WRITE_TOOLS {
        let body = serde_json::to_vec(&json!({"tool":tool, "turn_id":f.turn})).unwrap();
        let (status, value) = f.post(route::ROUTE, body.clone(), f.signed(&body)).await;
        assert_eq!(status, 403, "{tool}");
        assert_eq!(value["code"], "write_tool_gateway_only", "{tool}");
        assert!(value.get("access_token").is_none());
    }
    assert_eq!(probe.counts(), [0, 0]);
    let audit = std::fs::read_to_string(f.home.path().join("tool_calls.jsonl")).unwrap();
    let rows: Vec<serde_json::Value> = audit.lines().map(|s| serde_json::from_str(s).unwrap()).collect();
    for tool in crate::google_workspace::drive_write_policy::DRIVE_WRITE_TOOLS {
        assert!(rows.iter().any(|row| row["google_tool"] == *tool
            && row["reason"] == "write_tool_gateway_only" && row["success"] == false));
    }
}
