//! Executor tests use real local credential stores and an instance-local fake HTTP boundary.
use super::*;
use super::super::transport::{BuiltRequest, SendError, WriteHttp};
use crate::google_personal::oauth;
use crate::person_credentials::{self as vault, write_store as store, Secret, Status};
use duduclaw_auth::{AccessLevel, UserDb, UserRole};
use duduclaw_security::credential_protection::ProtectedKey;
use std::sync::Arc;

pub(crate) struct Fixture {
    pub(crate) home: tempfile::TempDir,
    key: ProtectedKey,
    pub(crate) db: UserDb,
    pub(crate) user: String,
}
impl Fixture {
    pub(crate) fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        duduclaw_core::gateway_instance::acquire(home.path()).unwrap();
        crate::handlers::credential_protection_rpc::protection_for(home.path()).enable(false).unwrap();
        let key = crate::google_identity::protection_gate(home.path()).unwrap();
        crate::team_edition::record(home.path(), duduclaw_core::EditionProfile::Enterprise);
        std::fs::create_dir_all(home.path().join("agents/bot")).unwrap();
        std::fs::write(home.path().join("config.toml"), "[integrations]\ngoogle_workspace=true\n").unwrap();
        let db = UserDb::new(&home.path().join("users.db")).unwrap();
        let user = db.create_user("owner@c.test", "Owner", "fixture-password", UserRole::Employee).unwrap();
        db.bind_agent(&user.id, "bot", AccessLevel::Owner).unwrap();
        db.set_google_consent(&user.id, "bot", true, &user.id).unwrap();
        let f = Self { home, key, db, user: user.id };
        f.selector("owner");
        crate::mcp_oauth::upsert_client_config(f.home.path(), crate::mcp_oauth::McpOAuthClientConfig {
            provider_id: "google".into(), client_id: "client-1".into(), client_secret: "client-secret".into(),
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token_url: "https://oauth2.googleapis.com/token".into(), scopes: vec![],
            redirect_uri: "http://127.0.0.1:18889/api/mcp/oauth/callback".into(),
        }).unwrap();
        let mut ordinary = serde_json::to_value(f.metadata("user")).unwrap();
        ordinary.as_object_mut().unwrap().remove("purpose");
        ordinary["granted_scopes"] = json!(["openid", "email"]);
        let epoch = vault::begin_connection(f.home.path(), &f.user).unwrap();
        vault::connect(f.home.path(), &f.key, serde_json::from_value(ordinary).unwrap(), &secret(false), epoch).unwrap();
        f
    }
    fn selector(&self, selector: &str) {
        let fallback = if selector == "delegate:owner" { "none" } else { "home" };
        std::fs::write(self.home.path().join("agents/bot/agent.toml"),
            format!("[google]\nidentity='{selector}'\nfallback='{fallback}'\n")).unwrap();
    }
    fn metadata(&self, kind: &str) -> store::Metadata {
        store::Metadata {
            owner_kind: kind.into(), owner_id: if kind == "home" { "home".into() } else { self.user.clone() },
            purpose: "drive_write".into(), provider: "google".into(),
            credential_id: uuid::Uuid::new_v4().to_string(), revision: 1,
            account_email: "linked@c.test".into(), google_sub: "google-sub-1".into(), hosted_domain: Some("c.test".into()),
            granted_scopes: vec![oauth::DRIVE_WRITE_SCOPE.into(), "openid".into(), "email".into()],
            status: Status::Connected, status_before_suspension: None,
            connected_at: chrono::Utc::now().to_rfc3339(), last_refresh_at: None,
            oauth_client_id: "client-1".into(), google_data_consent: Some(crate::google_data_consent::Evidence::issue(&self.user).unwrap()),
        }
    }
    fn connect(&self, metadata: store::Metadata, expired: bool) {
        let epoch = store::begin_connection(self.home.path(), &metadata.owner_kind, &metadata.owner_id).unwrap();
        store::connect(self.home.path(), &self.key, metadata, &secret(expired), epoch).unwrap();
    }
}
fn secret(expired: bool) -> Secret {
    Secret { access_token: "write-fixture-secret".into(), refresh_token: Some("write-refresh-secret".into()),
        expires_at: Some(if expired { chrono::Utc::now().timestamp() + 120 } else { 4_000_000_000 }) }
}

#[derive(Default)]
pub(super) struct Http {
    methods: Mutex<Vec<String>>,
    maybe_sent: bool,
    violation: bool,
    forbidden: bool,
    stall: bool,
    changed_scope: std::sync::atomic::AtomicBool,
    renamed_folder: std::sync::atomic::AtomicBool,
}
impl WriteHttp for Arc<Http> {
    fn send(&self, request: BuiltRequest) -> Pin<Box<dyn Future<Output = Result<(u16, Vec<u8>), SendError>> + Send + '_>> {
        let request = request.into_test_request();
        let method = request.method().as_str().to_owned();
        assert_ne!(method, "DELETE", "no executor request may permanently delete");
        self.methods.lock().unwrap().push(method.clone());
        let write = method != "GET";
        let path = request.url().path().to_owned();
        let id = request.url().path_segments().unwrap().last().unwrap().to_owned();
        let payload: Value = request.body().and_then(|b| b.as_bytes()).and_then(|b| serde_json::from_slice(b).ok()).unwrap_or(Value::Null);
        let added_parent = request.url().query_pairs().find(|(k, _)| k == "addParents").map(|(_, v)| v.into_owned());
        Box::pin(async move {
            if self.forbidden { return Ok((403, serde_json::to_vec(&json!({"error":{"errors":[{"reason":"insufficientPermissions"}]}})).unwrap())); }
            if write && self.stall { std::future::pending::<()>().await; }
            if write && self.maybe_sent { return Err(SendError::Network); }
            if path.starts_with("/v1/documents/") || path.starts_with("/v4/spreadsheets/") {
                let document = path.starts_with("/v1/documents/");
                let target = id.strip_suffix(":batchUpdate").unwrap_or(&id);
                let value = if !write && document {
                    json!({"documentId":target,"body":{"content":[{"startIndex":1,"endIndex":7,"paragraph":{"elements":[{"startIndex":1,"endIndex":7,"textRun":{"content":"hello\n"}}]}}]}})
                } else if !write { json!({"sheets":[{"properties":{"sheetId":0,"title":"Sheet1"}}]}) }
                else {
                    let replies: Vec<Value> = payload["requests"].as_array().unwrap().iter().map(|r| {
                        if r.get("createHeader").is_some() { json!({"createHeader":{"headerId":"header.1"}}) }
                        else if r.get("addSheet").is_some() { json!({"addSheet":{"properties":{"sheetId":1}}}) }
                        else { json!({}) }
                    }).collect();
                    if document { json!({"documentId":target,"replies":replies}) }
                    else { json!({"spreadsheetId":target,"replies":replies}) }
                };
                return Ok((200, serde_json::to_vec(&value).unwrap()));
            }
            let id = if write && method == "POST" { "Created_file_123" } else { id.as_str() };
            let parent = if (write && self.violation) || self.changed_scope.load(Ordering::SeqCst) { "Outside_folder_123" } else { added_parent.as_deref().unwrap_or("Parent_folder_123") };
            Ok((200, serde_json::to_vec(&json!({"id":id,"name":if id == "Parent_folder_123" && self.renamed_folder.load(Ordering::SeqCst) { "Changed folder" } else { "Google name" },"mimeType":
                if matches!(id, "Parent_folder_123" | "Destination_folder_123") { "application/vnd.google-apps.folder" } else { "text/plain" },
                "parents":if id == "Parent_folder_123" && !self.changed_scope.load(Ordering::SeqCst) { vec![] } else { vec![parent] },"trashed":payload["trashed"].as_bool().unwrap_or(false),"capabilities":{"canAddChildren":true,"canEdit":true,"canRename":true,"canTrash":true,"canMoveItemWithinDrive":true}})).unwrap()))
        })
    }
}
impl Http {
    fn writes(&self) -> usize {
        let methods = self.methods.lock().unwrap();
        assert!(!methods.iter().any(|m| m == "DELETE"));
        methods.iter().filter(|m| m.as_str() != "GET").count()
    }
}
struct Confirmer {
    outcome: u8,
    change: Option<Box<dyn Fn() + Send + Sync>>,
}
impl DriveWriteConfirmer for Confirmer {
    fn confirm<'a>(&'a self, request: ConfirmRequest) -> Pin<Box<dyn Future<Output = ConfirmOutcome> + Send + 'a>> {
        assert_eq!(request.account_email, "linked@c.test");
        assert!(!request.snapshot_digest.is_empty());
        assert!(!request.scope_digest.is_empty());
        Box::pin(async move {
            if let Some(change) = &self.change { change(); }
            match self.outcome {
                0 => ConfirmOutcome::Approved { approval_ids: vec!["approval".into()], decided_by: vec!["holder".into()] },
                1 => ConfirmOutcome::Denied,
                _ => ConfirmOutcome::Expired,
            }
        })
    }
}
pub(crate) fn policy(f: &Fixture, mode: ApprovalMode) {
    let mut p = policy::DriveWritePolicy::all_off();
    p.create = true; p.modify = true; p.move_ = true; p.trash = true;
    p.approval.create = mode; p.approval.modify = mode;
    let section = policy::render_section(&p, &policy::sign(&p, "admin", "admin@c.test", chrono::Utc::now()));
    let config = toml::to_string(&json!({
        "integrations": {"google_workspace": true},
        "google": {"drive_write": section},
    })).unwrap();
    std::fs::write(f.home.path().join("config.toml"), config).unwrap();
}
pub(crate) fn state(f: &Fixture) -> ExecuteState {
    f.connect(f.metadata("user"), false);
    std::fs::write(f.home.path().join("agents/bot/agent.toml"),
        "[google]\nidentity='owner'\nfallback='none'\n[capabilities]\nallowed_tools=['mcp__duduclaw__*']\n").unwrap();
    ExecuteState::new(f.home.path().to_path_buf(), None)
}
pub(crate) fn create_args() -> Value { json!({"name":"new.txt", "text":"private content", "parent_folder_id":"Parent_folder_123"}) }
fn assert_audit(f: &Fixture, result: &Result<Value, Refusal>, outcome: &str) -> Value {
    let raw = std::fs::read_to_string(f.home.path().join("tool_calls.jsonl")).unwrap();
    for secret in ["write-fixture-secret", "write-refresh-secret", "private content"] {
        assert!(!raw.contains(secret));
        let output = match result { Ok(v) => v.to_string(), Err(e) => e.message_zh.clone() };
        assert!(!output.contains(secret));
    }
    let row: Value = serde_json::from_str(raw.lines().last().unwrap()).unwrap();
    assert_eq!(row["outcome"], outcome);
    assert!(row["google_turn_id"].as_str().is_some());
    row
}
async fn invoke(f: &Fixture, state: &ExecuteState, http: Arc<Http>, tool: &str, args: Value, who: RequesterIdentity) -> Result<Value, Refusal> {
    let turn = uuid::Uuid::new_v4().to_string();
    let _guard = crate::knowledge::turn_requesters::register("bot", &turn, who.clone()).unwrap();
    let progress = AtomicUsize::new(0);
    TEST_HTTP.scope(http, async {
        let prepared = serve(state, "bot", tool, &args, Some(turn.clone()), None, "call".into(), None, &progress).await?;
        let digest = prepared["approval_binding_digest"].as_str().unwrap().to_owned();
        let result = serve(state, "bot", tool, &args, Some(turn), None, "call".into(), Some(digest), &progress).await;
        assert!(std::fs::metadata(f.home.path().join("tool_calls.jsonl")).is_ok());
        result
    }).await
}
#[tokio::test]
async fn run_only_employee_write_audits_run_id_for_prepare_and_execute() {
    use crate::google_identity::employee_execute::{self, Request};
    let f = Fixture::new(); let state = state(&f); policy(&f, ApprovalMode::None);
    let run = uuid::Uuid::new_v4().to_string();
    let _guard = crate::knowledge::turn_requesters::register("bot", &run,
        RequesterIdentity::DashboardUser { user_id: f.user.clone() }).unwrap();
    let http = Arc::new(Http::default());
    TEST_HTTP.scope(http.clone(), async {
        let prepared = employee_execute::serve(&state, "bot", Request::Prepare {
            tool: "drive_create_file".into(), arguments: create_args(), turn_id: None,
            run_id: Some(run.clone()), call_id: "run-call".into(),
        }).await.unwrap();
        let result = employee_execute::serve(&state, "bot", Request::Execute {
            tool: "drive_create_file".into(), arguments: create_args(), turn_id: None,
            run_id: Some(run.clone()), call_id: "run-call".into(),
            approval_binding_digest: prepared["approval_binding_digest"].as_str().unwrap().into(),
        }).await;
        assert!(result.is_ok());
        assert_audit(&f, &result, "confirmed");
    }).await;
    assert_eq!(http.writes(), 1);
    let raw = std::fs::read_to_string(f.home.path().join("tool_calls.jsonl")).unwrap();
    let rows: Vec<Value> = raw.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    assert_eq!(rows.len(), 2);
    for row in rows {
        assert_eq!(row["tool_name"], "google_drive_write");
        assert_eq!(row["google_turn_id"], run);
        assert_eq!(row["run_id"], run);
        assert_eq!(row["id_source"], "run");
    }
}

#[tokio::test]
async fn policy_off_folder_required_and_unsupported_make_zero_requests() {
    let f = Fixture::new(); let state = state(&f); let http = Arc::new(Http::default());
    for (tool, args, code) in [
        ("drive_create_file", create_args(), "action_off"),
        ("drive_move_file", json!({"file_id":"Target_file_123", "destination_folder_id":"Parent_folder_123"}), "folder_required"),
        ("drive_create_file", create_args(), "drive_write_unsupported_identity"),
    ] {
        if code != "action_off" { policy(&f, ApprovalMode::None); }
        if code == "drive_write_unsupported_identity" { f.selector("external"); }
        let result = invoke(&f, &state, http.clone(), tool, args, RequesterIdentity::DashboardUser { user_id:f.user.clone() }).await;
        assert_eq!(result.as_ref().unwrap_err().code, code);
        assert_audit(&f, &result, "refused");
        assert_eq!(http.writes(), 0); assert!(http.methods.lock().unwrap().is_empty());
    }
}
#[tokio::test]
async fn member_requires_confirmation_even_with_allowed_tool_and_none_sends() {
    for mode in [ApprovalMode::Member, ApprovalMode::MemberAndAdmin, ApprovalMode::None] {
        let f = Fixture::new(); let mut state = state(&f); policy(&f, mode);
        state.drive_write_confirmer = Arc::new(UnavailableConfirmer);
        std::fs::write(f.home.path().join("agents/bot/agent.toml"),
            "[google]\nidentity='owner'\nfallback='none'\n[capabilities]\nallowed_tools=['drive_create_file']\n").unwrap();
        let http = Arc::new(Http::default());
        let result = invoke(&f, &state, http.clone(), "drive_create_file", create_args(), RequesterIdentity::DashboardUser { user_id:f.user.clone() }).await;
        if mode == ApprovalMode::None { assert!(result.is_ok()); assert_eq!(http.writes(), 1); assert_audit(&f, &result, "confirmed"); }
        else { assert_eq!(result.as_ref().unwrap_err().code, "approval_unavailable"); assert_eq!(http.writes(), 0); assert_audit(&f, &result, "refused"); }
    }
}
#[tokio::test]
async fn approved_sends_denied_and_expired_do_not_send() {
    for outcome in 0..3 {
        let f = Fixture::new(); let mut state = state(&f); policy(&f, ApprovalMode::Member);
        state.drive_write_confirmer = Arc::new(Confirmer { outcome, change: None });
        let http = Arc::new(Http::default());
        let result = invoke(&f, &state, http.clone(), "drive_create_file", create_args(), RequesterIdentity::DashboardUser { user_id:f.user.clone() }).await;
        if outcome == 0 { assert!(result.is_ok()); assert_eq!(http.writes(), 1); }
        else { assert_eq!(result.as_ref().unwrap_err().code, "not_sent"); assert_eq!(http.writes(), 0); assert_audit(&f, &result, "not_sent"); }
    }
}
#[tokio::test]
async fn snapshot_or_scope_change_after_confirmation_never_sends() {
    for snapshot in [true, false] {
        let f = Fixture::new(); let mut state = state(&f); policy(&f, ApprovalMode::Member);
        let http = Arc::new(Http::default());
        let home = f.home.path().to_path_buf(); let changing_http = http.clone();
        state.drive_write_confirmer = Arc::new(Confirmer { outcome:0, change:Some(Box::new(move || {
            if snapshot { std::fs::write(home.join("config.toml"), "").unwrap(); }
            else { changing_http.changed_scope.store(true, Ordering::SeqCst); }
        })) });
        let result = invoke(&f, &state, http.clone(), "drive_create_file", create_args(), RequesterIdentity::DashboardUser { user_id:f.user.clone() }).await;
        assert_eq!(result.as_ref().unwrap_err().code, "changed"); assert_eq!(http.writes(), 0);
    }
}
#[tokio::test]
async fn unattended_notifies_once_and_nonholder_is_refused_before_google() {
    let f = Fixture::new(); let state = state(&f); policy(&f, ApprovalMode::Member);
    let http = Arc::new(Http::default());
    for _ in 0..2 {
        let result = invoke(&f, &state, http.clone(), "drive_create_file", create_args(), RequesterIdentity::System { kind:"cron".into(), on_behalf_of:Some(f.user.clone()) }).await;
        assert_eq!(result.unwrap_err().code, "approval_unavailable_unattended");
    }
    assert_eq!(crate::notify_push::person_notice_test::take(f.home.path()).len(), 1);
    let result = invoke(&f, &state, http.clone(), "drive_create_file", create_args(), RequesterIdentity::DashboardUser { user_id:"other".into() }).await;
    assert_eq!(result.unwrap_err().code, "requester_not_account_holder");
    assert_eq!(http.writes(), 0); assert!(http.methods.lock().unwrap().is_empty());
}
#[tokio::test]
async fn maybe_sent_is_not_retried_and_never_exposes_bearer() {
    let f = Fixture::new(); let state = state(&f); policy(&f, ApprovalMode::None);
    let http = Arc::new(Http { maybe_sent:true, ..Default::default() });
    let result = invoke(&f, &state, http.clone(), "drive_create_file", create_args(), RequesterIdentity::DashboardUser { user_id:f.user.clone() }).await;
    assert_eq!(result.as_ref().unwrap_err().code, "result_unconfirmed"); assert_eq!(http.writes(), 1);
    assert_audit(&f, &result, "may_have_sent");
}

#[tokio::test]
async fn attachment_provenance_size_symlink_and_traversal_refuse() {
    use crate::knowledge::turn_requesters::{register_inbound, InboundContent};
    let f = Fixture::new(); let state = state(&f); policy(&f, ApprovalMode::None);
    let who = RequesterIdentity::DashboardUser { user_id:f.user.clone() };
    let dir = f.home.path().join("agents/bot/attachments"); std::fs::create_dir_all(&dir).unwrap();
    let content = b"current attachment";
    let hash = hex::encode(Sha256::digest(content));
    std::fs::write(dir.join("current.txt"), content).unwrap();
    let _prior = register_inbound("bot", "previous-attachment-turn", who.clone(), Some(InboundContent { text:String::new(), attachment_hashes:vec![hash.clone()] })).unwrap();
    let large = std::fs::File::create(dir.join("large.txt")).unwrap(); large.set_len(5 * 1024 * 1024 + 1).unwrap();
    #[cfg(unix)] std::os::unix::fs::symlink(dir.join("current.txt"), dir.join("link.txt")).unwrap();
    let mut cases = vec![("current.txt", "invalid_attachment"), ("large.txt", "content_too_large"), ("../current.txt", "invalid_attachment")];
    #[cfg(unix)] cases.push(("link.txt", "invalid_attachment"));
    for (name, code) in cases {
        let http = Arc::new(Http::default());
        let result = invoke(&f, &state, http.clone(), "drive_create_file", json!({"name":"upload", "attachment":name}), who.clone()).await;
        assert_eq!(result.as_ref().unwrap_err().code, code); assert_eq!(http.writes(), 0);
        assert_audit(&f, &result, "refused");
    }
    let turn = uuid::Uuid::new_v4().to_string();
    let _guard = register_inbound("bot", &turn, who.clone(), Some(InboundContent { text:String::new(), attachment_hashes:vec!["wrong hash".into()] })).unwrap();
    assert_eq!(read_attachment(f.home.path(), "bot", Some(&turn), &who, "current.txt").err().unwrap().code, "invalid_attachment");
    let _guard2 = register_inbound("bot", &turn, who.clone(), Some(InboundContent { text:String::new(), attachment_hashes:vec![hash] })).unwrap();
    assert!(read_attachment(f.home.path(), "bot", Some(&turn), &who, "current.txt").is_ok());
    assert!(read_attachment(f.home.path(), "other", Some(&turn), &who, "current.txt").is_err());
    std::fs::write(dir.join("current.txt"), b"changed attachment").unwrap();
    assert_eq!(read_attachment(f.home.path(), "bot", Some(&turn), &who, "current.txt").err().unwrap().code, "invalid_attachment");
}

#[tokio::test]
async fn current_turn_attachment_is_sent_and_audited_by_hash_only() {
    use crate::knowledge::turn_requesters::{register_inbound, InboundContent};
    let f = Fixture::new(); let state = state(&f); policy(&f, ApprovalMode::None);
    let who = RequesterIdentity::DashboardUser { user_id:f.user.clone() };
    let dir = f.home.path().join("agents/bot/attachments"); std::fs::create_dir_all(&dir).unwrap();
    let bytes = b"private attachment contents"; let hash = hex::encode(Sha256::digest(bytes));
    std::fs::write(dir.join("current.txt"), bytes).unwrap();
    let turn = uuid::Uuid::new_v4().to_string();
    let _guard = register_inbound("bot", &turn, who.clone(), Some(InboundContent { text:String::new(), attachment_hashes:vec![hash.clone()] })).unwrap();
    let args = json!({"name":"upload", "attachment":"current.txt", "parent_folder_id":"Parent_folder_123"});
    let http = Arc::new(Http::default()); let progress = AtomicUsize::new(0);
    let result = TEST_HTTP.scope(http.clone(), async {
        let prepared = serve(&state, "bot", "drive_create_file", &args, Some(turn.clone()), None, "upload-call".into(), None, &progress).await.unwrap();
        serve(&state, "bot", "drive_create_file", &args, Some(turn), None, "upload-call".into(), Some(prepared["approval_binding_digest"].as_str().unwrap().into()), &progress).await
    }).await;
    assert!(result.is_ok()); assert_eq!(http.writes(), 1);
    let row = assert_audit(&f, &result, "confirmed"); assert_eq!(row["content_sha256"], hash);
    assert_eq!(row["bytes"], bytes.len());
    assert!(!std::fs::read_to_string(f.home.path().join("tool_calls.jsonl")).unwrap().contains("private attachment contents"));
}

#[tokio::test]
async fn post_check_violation_notifies_admin_and_records_violation() {
    use std::io::Write;
    let f = Fixture::new(); let state = state(&f); policy(&f, ApprovalMode::None);
    let admin = f.db.create_user("admin@c.test", "Admin", "fixture-password", UserRole::Admin).unwrap();
    f.db.bind_channel_identity(&admin.id, "telegram", "12345", true).unwrap();
    // Queue the real notice in quiet hours; no outbound notification service is needed.
    let now = chrono::Local::now();
    let start = (now - chrono::Duration::hours(1)).format("%H:%M").to_string();
    let end = (now + chrono::Duration::hours(1)).format("%H:%M").to_string();
    writeln!(std::fs::OpenOptions::new().append(true).open(f.home.path().join("config.toml")).unwrap(),
        "\n[notify]\nquiet_hours='{start}-{end}'").unwrap();
    let http = Arc::new(Http { violation:true, ..Default::default() });
    let result = invoke(&f, &state, http.clone(), "drive_create_file", create_args(), RequesterIdentity::DashboardUser { user_id:f.user.clone() }).await;
    assert!(result.is_ok()); assert_eq!(http.writes(), 1);
    let row = assert_audit(&f, &result, "confirmed"); assert_eq!(row["post_check"], "violation"); assert_eq!(row["reason"], "post_check_violation");
    let notices = crate::notify_governance::take_due(f.home.path(), chrono::Utc::now() + chrono::Duration::hours(3));
    assert_eq!(notices.len(), 1); assert_eq!(notices[0].notify_type, "google.drive_write.post_check_violation");
    assert_eq!(notices[0].chat_id, "12345");
}

#[tokio::test]
async fn insufficient_permissions_on_metadata_invalidates_only_write_grant() {
    let f = Fixture::new(); let state = state(&f); policy(&f, ApprovalMode::None);
    let http = Arc::new(Http { forbidden:true, ..Default::default() });
    let ordinary = std::fs::read(vault::path(f.home.path())).unwrap();
    let result = invoke(&f, &state, http.clone(), "drive_create_file", create_args(), RequesterIdentity::DashboardUser { user_id:f.user.clone() }).await;
    assert_eq!(result.as_ref().unwrap_err().code, "insufficient_permissions"); assert_eq!(http.writes(), 0);
    assert_audit(&f, &result, "refused");
    assert_eq!(store::get(f.home.path(), "user", &f.user).unwrap().unwrap().metadata.status, Status::NeedsReconnect);
    assert_eq!(std::fs::read(vault::path(f.home.path())).unwrap(), ordinary);
}

#[tokio::test]
async fn cancelled_write_records_uncertain_outcome() {
    let f = Fixture::new(); let state = state(&f); policy(&f, ApprovalMode::None);
    let http = Arc::new(Http { stall:true, ..Default::default() });
    let operation = invoke(&f, &state, http.clone(), "drive_create_file", create_args(), RequesterIdentity::DashboardUser { user_id:f.user.clone() });
    let result = tokio::time::timeout(Duration::from_secs(2), operation).await;
    assert!(result.is_err()); assert_eq!(http.writes(), 1);
    let row = assert_audit(&f, &Err(unconfirmed()), "may_have_sent"); assert_eq!(row["reason"], "result_unconfirmed");
}

#[tokio::test]
async fn pending_is_single_use_and_changed_arguments_cannot_be_retried() {
    let f = Fixture::new(); let state = state(&f); policy(&f, ApprovalMode::None);
    let who = RequesterIdentity::DashboardUser { user_id:f.user.clone() };
    let turn = uuid::Uuid::new_v4().to_string();
    let _guard = crate::knowledge::turn_requesters::register("bot", &turn, who.clone()).unwrap();
    let progress = AtomicUsize::new(0); let args = create_args();
    let prepared = serve(&state, "bot", "drive_create_file", &args, Some(turn.clone()), None, "once".into(), None, &progress).await.unwrap();
    let mut changed_args = args.clone(); changed_args["name"] = json!("different");
    let http = Arc::new(Http::default());
    for input in [changed_args, args] {
        let result = TEST_HTTP.scope(http.clone(), serve(&state, "bot", "drive_create_file", &input, Some(turn.clone()), None, "once".into(), Some(prepared["approval_binding_digest"].as_str().unwrap().into()), &progress)).await;
        assert_eq!(result.unwrap_err().code, "changed");
    }
    assert_eq!(http.writes(), 0); assert!(http.methods.lock().unwrap().is_empty());
}

#[tokio::test]
async fn delegated_mint_uses_gateway_only_write_transport() {
    let f = Fixture::new(); let state = state(&f); policy(&f, ApprovalMode::None);
    let subject = format!("{}@c.test", uuid::Uuid::new_v4());
    std::fs::write(f.home.path().join("agents/bot/agent.toml"), format!(
        "[google]\nidentity='delegate:{subject}'\nfallback='none'\n[capabilities]\nallowed_tools=['mcp__duduclaw__*']\n")).unwrap();
    let app = axum::Router::new().route("/token", axum::routing::post(|| async {
        axum::Json(json!({"access_token":"write-fixture-secret", "expires_in":3600, "token_type":"Bearer", "scope":oauth::DRIVE_WRITE_SCOPE}))
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let uri = format!("http://{}/token", listener.local_addr().unwrap());
    crate::google_identity::delegation_key::import(f.home.path(), &f.key, &crate::google_identity::tests::key_json(&uri)).unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
    let http = Arc::new(Http::default());
    let result = invoke(&f, &state, http.clone(), "drive_create_file", create_args(), RequesterIdentity::DashboardUser { user_id:f.user.clone() }).await;
    server.abort();
    assert!(result.is_ok()); assert_eq!(http.writes(), 1); assert_audit(&f, &result, "confirmed");
}

#[tokio::test]
async fn all_nine_programs_and_header_phase_two_use_the_same_executor() {
    for (tool, args, writes) in [
        ("drive_create_file", create_args(), 1),
        ("drive_copy_file", json!({"file_id":"Target_file_123","parent_folder_id":"Parent_folder_123"}), 1),
        ("drive_update_file", json!({"file_id":"Target_file_123","name":"renamed"}), 1),
        ("drive_move_file", json!({"file_id":"Target_file_123","destination_folder_id":"Destination_folder_123"}), 1),
        ("drive_trash_file", json!({"file_id":"Target_file_123"}), 1),
        ("docs_insert_text", json!({"document_id":"Target_doc_123","text":"private content","at":"end"}), 1),
        ("docs_format", json!({"document_id":"Target_doc_123","operations":[{"type":"header","text":"private content"}]}), 2),
        ("sheets_add_sheet", json!({"spreadsheet_id":"Target_sheet_123","title":"New sheet"}), 1),
        ("sheets_add_note", json!({"spreadsheet_id":"Target_sheet_123","range":"A1","note":"private content"}), 1),
    ] {
        let f = Fixture::new(); let state = state(&f); policy(&f, ApprovalMode::None);
        let mut p = policy::load(f.home.path()); p.approval.move_ = ApprovalMode::None; p.approval.trash = ApprovalMode::None;
        p.folders = ["Parent_folder_123", "Destination_folder_123"].into_iter().map(|id| policy::FolderEntry {
            id:id.into(), name:"Folder".into(), kind:policy::FolderKind::Folder,
        }).collect();
        let section = policy::render_section(&p, &policy::sign(&p, "admin", "admin@c.test", chrono::Utc::now()));
        let config = toml::to_string(&json!({
            "integrations": {"google_workspace": true},
            "google": {"drive_write": section},
        })).unwrap();
        std::fs::write(f.home.path().join("config.toml"), config).unwrap();
        let http = Arc::new(Http::default());
        let result = invoke(&f, &state, http.clone(), tool, args, RequesterIdentity::DashboardUser { user_id:f.user.clone() }).await;
        assert!(result.is_ok(), "{tool}: {result:?}"); assert_eq!(http.writes(), writes);
        assert_audit(&f, &result, "confirmed");
    }
}

#[tokio::test]
async fn denied_tools_still_wins_with_no_human_confirmation() {
    use std::io::Write;
    let f = Fixture::new(); let state = state(&f); policy(&f, ApprovalMode::None);
    writeln!(std::fs::OpenOptions::new().append(true).open(f.home.path().join("agents/bot/agent.toml")).unwrap(),
        "denied_tools=['drive_create_file']").unwrap();
    let http = Arc::new(Http::default());
    let result = invoke(&f, &state, http.clone(), "drive_create_file", create_args(), RequesterIdentity::DashboardUser { user_id:f.user.clone() }).await;
    assert_eq!(result.as_ref().unwrap_err().code, "identity_changed"); assert_eq!(http.writes(), 0);
    assert!(http.methods.lock().unwrap().is_empty()); assert_audit(&f, &result, "refused");
}

#[tokio::test]
async fn pending_store_rejects_duplicates_expires_and_caps_entries() {
    let f = Fixture::new(); let state = state(&f); policy(&f, ApprovalMode::None);
    let who = RequesterIdentity::DashboardUser { user_id:f.user.clone() };
    let turn = uuid::Uuid::new_v4().to_string();
    let _guard = crate::knowledge::turn_requesters::register("bot", &turn, who.clone()).unwrap();
    let progress = AtomicUsize::new(0); let args = create_args();
    serve(&state, "bot", "drive_create_file", &args, Some(turn.clone()), None, "original".into(), None, &progress).await.unwrap();
    let binding = {
        let mut entries = state.drive_writes.0.lock().unwrap();
        let pending = entries.get_mut(&("bot".into(), "original".into())).unwrap();
        pending.created = Instant::now() - Duration::from_secs(600);
        pending.binding.clone()
    };
    assert_eq!(state.drive_writes.take("bot", "original").err().unwrap().code, "changed");
    state.drive_writes.insert(binding.clone()).unwrap();
    assert_eq!(state.drive_writes.insert(binding.clone()).unwrap_err().code, "changed");
    for i in 1..1024 {
        let mut next = binding.clone(); next.snapshot.call_id = format!("call-{i}");
        state.drive_writes.insert(next).unwrap();
    }
    let mut next = binding; next.snapshot.call_id = "over-limit".into();
    assert_eq!(state.drive_writes.insert(next).unwrap_err().code, "changed");
}

#[tokio::test]
async fn member_card_expiry_in_executor_makes_zero_writes() {
    let f = Fixture::new(); let state = state(&f); policy(&f, ApprovalMode::Member);
    let turn = uuid::Uuid::new_v4().to_string();
    let who = RequesterIdentity::DashboardUser { user_id:f.user.clone() };
    let _requester = crate::knowledge::turn_requesters::register("bot", &turn, who.clone()).unwrap();
    let _context = crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL.scope(f.user.clone(),
        crate::knowledge::turn_requesters::CURRENT_REQUESTER.scope(who.clone(),
        duduclaw_memory::feedback::CURRENT_SESSION_ID.scope(Some(format!("expiry-{turn}")), async {
            crate::turn_context::register("bot", &turn, &format!("webchat:expiry-{turn}")).unwrap()
        }))).await;
    let http = Arc::new(Http::default());
    let progress = AtomicUsize::new(0);
    let raw = create_args();
    let executor = TEST_HTTP.scope(http.clone(), async {
        let prepared = serve(&state, "bot", "drive_create_file", &raw, Some(turn.clone()), None,
            "expiry-call".into(), None, &progress).await.unwrap();
        serve(&state, "bot", "drive_create_file", &raw, Some(turn.clone()), None,
            "expiry-call".into(), Some(prepared["approval_binding_digest"].as_str().unwrap().into()), &progress).await
    });
    let expire = async {
        let broker = crate::approval::ApprovalBroker::open(f.home.path()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(rec) = broker.list_by_kind("bound_action").await.unwrap().into_iter()
                    .find(|r| r.payload["drive_write_member_confirmation"] == true) {
                    rusqlite::Connection::open(f.home.path().join("approvals.db")).unwrap().execute(
                        "UPDATE approvals SET created_at=?1 WHERE id=?2",
                        rusqlite::params![(chrono::Utc::now()-chrono::Duration::minutes(10)).to_rfc3339(), rec.id.as_str()],
                    ).unwrap();
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }).await.expect("member card created");
    };
    let (result, ()) = tokio::join!(executor, expire);
    assert_eq!(result.as_ref().unwrap_err().code, "not_sent");
    assert_eq!(http.writes(), 0);
    assert_audit(&f, &result, "not_sent");
}

#[tokio::test]
async fn public_visitor_executor_creates_no_card_and_makes_zero_writes() {
    let f = Fixture::new(); let state = state(&f); policy(&f, ApprovalMode::Member);
    let http = Arc::new(Http::default());
    for channel in ["widget", "odoolivechat"] {
        let result = invoke(&f, &state, http.clone(), "drive_create_file", create_args(), RequesterIdentity::ChannelUser {
            channel:channel.into(), chat:"public".into(), sender:"visitor".into(), user_id:Some(f.user.clone()), attested_private:false,
        }).await;
        assert_eq!(result.unwrap_err().code, "public_visitor");
    }
    assert_eq!(http.writes(), 0);
    assert!(http.methods.lock().unwrap().is_empty());
    let broker = crate::approval::ApprovalBroker::open(f.home.path()).unwrap();
    assert!(broker.list_by_kind("bound_action").await.unwrap().is_empty());
    assert!(broker.list_by_kind("mcp_call").await.unwrap().is_empty());
}

#[tokio::test]
async fn displayed_folder_path_change_after_confirmation_never_writes() {
    let f = Fixture::new(); let mut state = state(&f); policy(&f, ApprovalMode::Member);
    let http = Arc::new(Http::default());
    let changing = http.clone();
    state.drive_write_confirmer = Arc::new(Confirmer { outcome:0, change:Some(Box::new(move || {
        changing.renamed_folder.store(true, Ordering::SeqCst);
    })) });
    let result = invoke(&f, &state, http.clone(), "drive_create_file", create_args(),
        RequesterIdentity::DashboardUser { user_id:f.user.clone() }).await;
    assert_eq!(result.unwrap_err().code, "changed");
    assert_eq!(http.writes(), 0);
}

#[tokio::test]
async fn readiness_missing_write_credential_returns_authenticated_request_link() {
    use crate::google_authorization_requests::{self as requests, turn as request_turn};
    let f=Fixture::new();
    policy(&f,ApprovalMode::Member);
    let who=RequesterIdentity::DashboardUser {user_id:f.user.clone()};
    let turn=uuid::Uuid::new_v4().to_string();
    let sessions=Arc::new(crate::session::SessionManager::new(&f.home.path().join("sessions.db")).unwrap());
    let notice=Arc::new(Mutex::new(request_turn::Notice::default()));
    crate::knowledge::turn_requesters::CURRENT_REQUESTER.scope(who.clone(),
        request_turn::NOTICE.scope(notice,async {
            let _guard=request_turn::register(f.home.path(),"bot",&turn,"Create a file",
                "webchat:write-test",sessions,None).unwrap();
            let route=credential::resolve_route(f.home.path(),"bot").unwrap();
            let refusal=readiness_check(f.home.path(),&route,"bot","drive_create_file",&who,Some(&turn)).unwrap_err();
            assert_eq!(refusal.code,"drive_write_needs_member_consent");
            let ticket=refusal.message_zh.strip_prefix(
                "請本人開啟這個連結，連結雲端硬碟寫入：/me/google/authorize#ticket=").unwrap();
            let row=requests::store::Store::open(f.home.path()).unwrap().preview(ticket,&f.user).unwrap();
            assert_eq!(row.purpose,"drive_write");
            assert_eq!(row.tool,"drive_create_file");
            assert_eq!(row.google_sub.as_deref(),Some("google-sub-1"));
            let outsider=RequesterIdentity::DashboardUser {user_id:"outsider".into()};
            let refusal=readiness_check(f.home.path(),&route,"bot","drive_create_file",&outsider,Some(&turn)).unwrap_err();
            assert!(!refusal.message_zh.contains("#ticket="));
        })).await;
}

#[test]
fn drive_write_request_completion_refuses_different_stored_google_sub() {
    use crate::google_authorization_requests::{self as requests, store::Store};
    let f=Fixture::new();
    policy(&f,ApprovalMode::Member);
    let who=RequesterIdentity::DashboardUser {user_id:f.user.clone()};
    let notice=requests::issue_for_turn(f.home.path(),&f.db,"bot","drive_create_file",
        "Create a file","webchat:test",&who,None,None).unwrap().unwrap();
    let ticket=notice.split_once("#ticket=").unwrap().1;
    let store=Store::open(f.home.path()).unwrap();
    let row=store.claim_ticket(ticket,&f.user,&uuid::Uuid::new_v4().to_string()).unwrap();
    let ordinary=std::fs::read(vault::path(f.home.path())).unwrap();
    let mut wrong=f.metadata("user");
    wrong.google_sub="different-sub".into();
    f.connect(wrong,false);
    assert!(requests::routes::complete(f.home.path(),&f.db,row,None).is_err());
    assert!(store.ready_rows().unwrap().is_empty());
    assert_eq!(std::fs::read(vault::path(f.home.path())).unwrap(),ordinary);
}
