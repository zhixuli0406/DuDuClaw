use super::{owner_test_support::{Fixture, AGENT}, *};
use serde_json::json;
use duduclaw_core::authority_coordinator::AuthorityCoordinator;

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
        let args = match *tool {
            "google_status" => continue,
            "gmail_search" | "drive_search" => json!({"query":"test"}),
            "gmail_read" => json!({"message_id":"message"}),
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
            "google_chat_messages" => json!({"space":"spaces/AAAA"}),
            "google_chat_spaces" | "calendar_list_events" | "gtasks_lists" | "gtasks_list" => json!({}),
            other => panic!("missing owner coverage fixture: {other}"),
        };
        let template = NativeCallTemplate::prepare(tool, &args).unwrap_or_else(|e| panic!("{tool}: {e}"));
        template.validate_closed().unwrap_or_else(|e| panic!("{tool}: {e}"));
        count += 1;
    }
    assert_eq!(count, 25);
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
