//! RM-131 P3: handler harness, fake folder reads, real config commit and audit.
use super::*;
use super::super::google_drive_write_rpc::FolderLookup;
use crate::google_workspace::drive_write_policy as policy;
use std::sync::Mutex;

const ID: &str = "Folder_0123456789";
const OTHER: &str = "Another_0123456789";
const ORIGINAL: &str = "# keep this comment\n[integrations]\ngoogle_workspace = true\n\n[general]\nlog_level = 'info' # keep spelling\n";
struct Fake { value: Option<Value>, calls: Mutex<Vec<String>> }
impl Fake {
    fn unavailable() -> Self { Self { value: None, calls: Mutex::new(Vec::new()) } }
    fn folder(kind: &str) -> Self {
        Self { value: Some(json!({"id":ID,"name":"業務\n資料夾","mimeType":kind})), calls: Mutex::new(Vec::new()) }
    }
}
#[async_trait::async_trait]
impl FolderLookup for Fake {
    async fn lookup(&self, id: &str) -> Option<Value> {
        self.calls.lock().unwrap().push(id.into());
        self.value.clone()
    }
}
fn admin() -> UserContext {
    let mut ctx = UserContext::admin_fallback();
    ctx.user_id = "admin-test".into(); ctx.email = "admin@example.test".into(); ctx
}
fn input() -> Value {
    json!({"create":false,"modify":false,"move":false,"trash":false,
        "approval":{"create":"member","modify":"member","move":"member","trash":"member"},
        "folders":[],"acknowledge_risk":false})
}
fn ok(frame: WsFrame) -> Value {
    match frame { WsFrame::Response { ok:true, payload:Some(p), .. } => p,
        other => panic!("expected success: {other:?}") }
}
fn refused(frame: WsFrame) {
    assert!(matches!(frame, WsFrame::Response { ok:false, .. }), "expected refusal: {frame:?}");
}
async fn fixture() -> (tempfile::TempDir, MethodHandler) {
    let dir = tempfile::tempdir().unwrap();
    let h = MethodHandler::new(dir.path().to_path_buf()).await;
    std::fs::write(dir.path().join("config.toml"), ORIGINAL).unwrap();
    (dir, h)
}
async fn rpc(h: &MethodHandler, method: &str, params: Value, ctx: &UserContext) -> WsFrame {
    let params = if method == "google.drive_write.set" { fresh_params(h, params, ctx).await } else { params };
    Box::pin(h.handle(method, params, ctx)).await
}

#[tokio::test]
async fn non_admin_get_and_set_are_refused() {
    let (dir, h) = fixture().await;
    let mut member = admin(); member.role = UserRole::Employee;
    for method in ["google.drive_write.get", "google.drive_write.set",
        "google.drive_write.consent_home_start", "google.drive_write.consent_home_disconnect",
        "google.drive_write.consent_home_finish", "google.drive_write.consent_home_paste"] {
        refused(rpc(&h, method, input(), &member).await);
    }
    assert_eq!(std::fs::read_to_string(dir.path().join("config.toml")).unwrap(), ORIGINAL);
}

#[tokio::test]
async fn widening_requires_ack_and_records_authenticated_admin() {
    let (dir, h) = fixture().await;
    let mut p = input(); p["create"] = json!(true);
    refused(rpc(&h, "google.drive_write.set", p.clone(), &admin()).await);
    assert_eq!(std::fs::read_to_string(dir.path().join("config.toml")).unwrap(), ORIGINAL);
    p["acknowledge_risk"] = json!(true);
    let response = ok(rpc(&h, "google.drive_write.set", p, &admin()).await);
    assert_eq!(response["effective"]["create"], true);
    assert!(response["ack"].get("by_user_id").is_none());
    let effective = policy::load(dir.path());
    assert!(effective.create);
    let ack = effective.ack.as_ref().unwrap();
    assert_eq!(ack.by_user_id, admin().user_id);
    assert_eq!(ack.by_email, admin().email);
    assert_eq!(ack.by_role, "admin");
    assert_eq!(ack.digest, policy::digest_of(&effective));
    let raw = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(raw.starts_with(ORIGINAL));
    let events = duduclaw_security::audit::read_recent_events(dir.path(), 20);
    let event = events.iter().find(|e| e.event_type == "config_changed").unwrap();
    let changes = event.details["changes"].to_string();
    assert!(changes.contains("google.drive_write.create: false -> true"));
    assert!(changes.contains("folders added") && changes.contains("approval.create"));
}

#[tokio::test]
async fn narrowing_resigns_without_ack_and_preserves_existing_folder_metadata() {
    let (dir, h) = fixture().await;
    let fake = Fake::unavailable();
    let mut p = input(); p["create"] = json!(true); p["trash"] = json!(true);
    p["folders"] = json!([ID, OTHER]); p["acknowledge_risk"] = json!(true);
    ok(save(&h, p.clone(), &admin(), &fake).await);
    assert_eq!(fake.calls.lock().unwrap().len(), 2);
    p["trash"] = json!(false); p["folders"] = json!([ID]);
    p["approval"]["create"] = json!("member_and_admin"); p["acknowledge_risk"] = json!(false);
    let mut second = admin(); second.user_id = "second-admin".into(); second.email = "second@example.test".into();
    ok(save(&h, p, &second, &fake).await);
    let loaded = policy::load(dir.path());
    assert!(!loaded.trash && loaded.create);
    assert_eq!(loaded.folders.len(), 1);
    assert_eq!(loaded.approval.create, policy::ApprovalMode::MemberAndAdmin);
    assert_eq!(loaded.ack.unwrap().by_user_id, second.user_id);
    assert_eq!(fake.calls.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn folder_link_forms_resolve_and_names_are_sanitized() {
    for link in [ID.to_string(), format!("https://drive.google.com/drive/folders/{ID}"),
        format!("https://drive.google.com/drive/u/3/folders/{ID}"), format!("https://drive.google.com/open?id={ID}")] {
        let (dir, h) = fixture().await;
        let mut fake = Fake::folder("application/vnd.google-apps.folder");
        fake.value.as_mut().unwrap()["name"] = json!(format!("\n{}\u{7f}", "字".repeat(201)));
        fake.value.as_mut().unwrap()["driveId"] = json!(ID);
        let mut p = input(); p["folders"] = json!([link]); p["acknowledge_risk"] = json!(true);
        ok(save(&h, p, &admin(), &fake).await);
        let loaded = policy::load(dir.path());
        assert_eq!(loaded.folders[0].id, ID);
        assert_eq!(loaded.folders[0].name, "字".repeat(200));
        assert_eq!(loaded.folders[0].kind, policy::FolderKind::SharedDrive);
        assert_eq!(*fake.calls.lock().unwrap(), vec![ID.to_string()]);
    }
}

#[tokio::test]
async fn shortcut_and_non_folder_rejected_unavailable_stored_unverified() {
    let (dir, h) = fixture().await;
    let mut p = input(); p["folders"] = json!([ID]); p["acknowledge_risk"] = json!(true);
    for mime in ["application/vnd.google-apps.shortcut", "text/plain"] {
        refused(save(&h, p.clone(), &admin(), &Fake::folder(mime)).await);
        assert_eq!(std::fs::read_to_string(dir.path().join("config.toml")).unwrap(), ORIGINAL);
    }
    let mut fake = Fake::folder("application/vnd.google-apps.folder");
    fake.value.as_mut().unwrap()["shortcutDetails"] = json!({"targetId":OTHER});
    refused(save(&h, p.clone(), &admin(), &fake).await);
    ok(save(&h, p, &admin(), &Fake::unavailable()).await);
    let loaded = policy::load(dir.path());
    assert_eq!(loaded.folders[0].kind, policy::FolderKind::Unverified);
    assert_eq!(loaded.folders[0].name, "");
}

#[tokio::test]
async fn invalid_inputs_never_write_or_lookup() {
    let (dir, h) = fixture().await;
    let fake = Fake::unavailable();
    let mut cases = Vec::new();
    let mut p = input(); p["folders"] = json!(vec![ID; 51]); cases.push(p);
    let mut p = input(); p["folders"] = json!(["https://evil.test/?id=Folder_0123456789"]); cases.push(p);
    let mut p = input(); p["folders"] = json!([42]); cases.push(p);
    let mut p = input(); p["create"] = json!("true"); cases.push(p);
    let mut p = input(); p["acknowledge_risk"] = Value::Null; cases.push(p);
    let mut p = input(); p["approval"]["unknown"] = json!("member"); cases.push(p);
    let mut p = input(); p["approval"]["create"] = json!("invalid"); cases.push(p);
    let mut p = input(); p["by_user_id"] = json!("forged-admin"); cases.push(p);
    for p in cases {
        refused(save(&h, p, &admin(), &fake).await);
        assert_eq!(std::fs::read_to_string(dir.path().join("config.toml")).unwrap(), ORIGINAL);
    }
    assert!(fake.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn all_off_is_signed_and_hand_edit_invalidates_effective_switches() {
    let (dir, h) = fixture().await;
    ok(rpc(&h, "google.drive_write.set", input(), &admin()).await);
    assert!(policy::load(dir.path()).ack.is_some());
    let path = dir.path().join("config.toml");
    let raw = std::fs::read_to_string(&path).unwrap();
    let mut table = raw.parse::<toml::Table>().unwrap();
    table["google"]["drive_write"]["trash"] = toml::Value::Boolean(true);
    std::fs::write(&path, toml::to_string(&table).unwrap()).unwrap();
    assert_eq!(policy::load(dir.path()), policy::DriveWritePolicy::all_off());
    let response = ok(rpc(&h, "google.drive_write.get", json!({}), &admin()).await);
    assert_eq!(response["configured"]["trash"], true);
    assert_eq!(response["effective"]["trash"], false);
    let mut p = input(); p["trash"] = json!(true);
    refused(save(&h, p, &admin(), &Fake::unavailable()).await);
}

#[tokio::test]
async fn raw_editor_and_shared_commit_only_allow_whole_section_removal() {
    let (dir, h) = fixture().await;
    let db = UserDb::new(&dir.path().join("users.db")).unwrap();
    let user = db.create_user("admin@example.test", "Admin", "fixture-password", UserRole::Admin).unwrap();
    let ctx = db.live_user_context(&user.id).unwrap().unwrap();
    let refusal = "請到 Google 整合設定頁調整雲端硬碟寫入權限。";
    ok(rpc(&h, "google.drive_write.set", input(), &ctx).await);
    let path = dir.path().join("config.toml");
    let raw = std::fs::read_to_string(&path).unwrap();
    let before = raw.parse::<toml::Table>().unwrap();
    let guard = super::super::config_raw_rpc::refuse_drive_write_edit;
    for key in ["trash", "ack"] {
        let mut changed = before.clone();
        if key == "trash" { changed["google"]["drive_write"]["trash"] = toml::Value::Boolean(true); }
        else { changed["google"]["drive_write"]["ack"]["by_email"] = toml::Value::String("forged@test.invalid".into()); }
        assert_eq!(guard(&before, &changed).unwrap_err(), refusal);
        assert!(super::super::config_commit::commit_table_locked(&path,
            super::super::config_commit::content_hash(&raw), &changed).await.unwrap_err().contains(refusal));
        let response = rpc(&h, "config.raw.set", json!({"file":"config","content":toml::to_string(&changed).unwrap(),"base_hash":super::super::config_commit::content_hash(&raw)}), &ctx).await;
        match response {
            WsFrame::Response { ok:false, error:Some(error), .. } => assert_eq!(error.as_str(), Some(refusal)),
            other => panic!("expected Drive write refusal: {other:?}"),
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), raw);
    }
    let mut removed = before.clone(); removed["google"].as_table_mut().unwrap().remove("drive_write");
    assert!(guard(&before, &removed).is_ok());
    assert_eq!(guard(&removed, &before).unwrap_err(), refusal);
    ok(rpc(&h, "config.raw.set", json!({"file":"config","content":toml::to_string(&removed).unwrap(),"base_hash":super::super::config_commit::content_hash(&raw)}), &ctx).await);
    assert_eq!(policy::load(dir.path()), policy::DriveWritePolicy::all_off());
}

#[tokio::test]
async fn unreadable_write_store_is_reported_without_rpc_error() {
    let (dir, h) = fixture().await;
    let path = crate::person_credentials::write_store::path(dir.path());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, "broken").unwrap();
    let response = ok(rpc(&h, "google.drive_write.get", json!({}), &admin()).await);
    assert_eq!(response["write_connections"], Value::Null);
    assert_eq!(response["write_store_unreadable"], true);
    assert_eq!(response["identity_routes"], json!([]));
}

#[tokio::test]
async fn member_notice_only_on_first_enable_per_set() {
    let (dir, h) = fixture().await;
    duduclaw_core::gateway_instance::acquire(dir.path()).unwrap();
    super::super::credential_protection_rpc::protection_for(dir.path()).enable(false).unwrap();
    let key = crate::google_identity::protection_gate(dir.path()).unwrap();
    let user = "connected-member";
    let metadata = crate::person_credentials::Metadata {
        owner_kind:"user".into(), owner_id:user.into(), provider:"google".into(),
        credential_id:uuid::Uuid::new_v4().to_string(), revision:1,
        account_email:"member@example.test".into(), google_sub:"sub-member".into(), hosted_domain:None,
        granted_scopes:vec!["openid".into(),"email".into()], local_purposes:vec!["base".into()],
        status:crate::person_credentials::Status::Connected, status_before_suspension:None,
        connected_at:chrono::Utc::now().to_rfc3339(), last_refresh_at:None, oauth_client_id:"fixture-client".into(),
        google_data_consent:Some(crate::google_data_consent::Evidence::issue(user).unwrap()),
    };
    let epoch = crate::person_credentials::begin_connection(dir.path(), user).unwrap();
    crate::person_credentials::connect(dir.path(), &key, metadata,
        &crate::person_credentials::Secret { access_token:"fixture-access".into(), refresh_token:None, expires_at:Some(4_000_000_000) }, epoch).unwrap();
    let mut p = input(); p["create"] = json!(true); p["modify"] = json!(true); p["acknowledge_risk"] = json!(true);
    ok(save(&h, p.clone(), &admin(), &Fake::unavailable()).await);
    let notices = crate::notify_push::person_notice_test::take(dir.path());
    assert_eq!(notices.len(), 1); assert_eq!(notices[0].0, user);
    assert!(notices[0].1.contains("我的連線"));
    p["acknowledge_risk"] = json!(false);
    ok(save(&h, p, &admin(), &Fake::unavailable()).await);
    assert!(crate::notify_push::person_notice_test::take(dir.path()).is_empty());
    ok(save(&h, input(), &admin(), &Fake::unavailable()).await);
    assert!(crate::notify_push::person_notice_test::take(dir.path()).is_empty());
}

#[tokio::test]
async fn generic_settings_refuse_nested_and_flat_drive_write_params() {
    let (dir, h) = fixture().await;
    for method in ["system.update_config", "google.integration.set"] {
        for p in [json!({"enabled":true,"google":{"drive_write":{"trash":true}}}),
            json!({"enabled":true,"google.drive_write.trash":true}),
            json!({"enabled":true,"drive_write":{"trash":true}})] {
            refused(rpc(&h, method, p, &admin()).await);
            assert_eq!(std::fs::read_to_string(dir.path().join("config.toml")).unwrap(), ORIGINAL);
        }
    }
}

#[tokio::test]
async fn concurrent_config_edit_during_folder_lookup_is_not_overwritten() {
    struct RacingLookup(PathBuf);
    #[async_trait::async_trait]
    impl FolderLookup for RacingLookup {
        async fn lookup(&self, _: &str) -> Option<Value> {
            std::fs::write(&self.0, format!("{ORIGINAL}\n# concurrent edit\n")).unwrap();
            None
        }
    }
    let (dir, h) = fixture().await;
    let path = dir.path().join("config.toml");
    let mut p = input(); p["folders"] = json!([ID]); p["acknowledge_risk"] = json!(true);
    refused(save(&h, p, &admin(), &RacingLookup(path.clone())).await);
    assert_eq!(std::fs::read_to_string(path).unwrap(), format!("{ORIGINAL}\n# concurrent edit\n"));
}

#[tokio::test]
async fn http_error_folder_metadata_is_unavailable_and_stored_unverified() {
    use super::super::google_drive_write_rpc::folder_response_value;
    for status in [403, 404] {
        let response = axum::http::Response::builder().status(status)
            .body(format!(r#"{{"error":{{"code":{status},"message":"Unavailable"}}}}"#)).unwrap();
        let value = folder_response_value(reqwest::Response::from(response)).await;
        assert!(value.is_none());
        let (dir, h) = fixture().await;
        let fake = Fake { value, calls: Mutex::new(Vec::new()) };
        let mut p = input(); p["folders"] = json!([ID]); p["acknowledge_risk"] = json!(true);
        ok(save(&h, p, &admin(), &fake).await);
        assert_eq!(policy::load(dir.path()).folders[0].kind, policy::FolderKind::Unverified);
    }
    let metadata = json!({"id":ID,"mimeType":"application/vnd.google-apps.folder"});
    let response = axum::http::Response::builder().status(200)
        .body(metadata.to_string()).unwrap();
    assert_eq!(folder_response_value(reqwest::Response::from(response)).await, Some(metadata));
}

#[tokio::test]
async fn home_write_status_is_safe_and_unreadable_store_fails_closed() {
    let (dir, h) = fixture().await;
    let status = ok(rpc(&h, "google.drive_write.get", json!({}), &admin()).await);
    assert_eq!(status["home_write"]["connected"], false);
    assert_eq!(status["home_write"]["status"], "disconnected");
    assert!(status["home_write"]["account_email"].is_null());
    let store = crate::person_credentials::write_store::path(dir.path());
    std::fs::create_dir_all(store.parent().unwrap()).unwrap();
    std::fs::write(store, "broken").unwrap();
    let status = ok(rpc(&h, "google.drive_write.get", json!({}), &admin()).await);
    assert_eq!(status["home_write"]["connected"], false);
    assert_eq!(status["home_write"]["status"], "unreadable");
}

#[tokio::test]
async fn home_write_rpcs_require_authenticated_dashboard_session() {
    let (_dir, h) = fixture().await;
    for method in ["google.drive_write.consent_home_start", "google.drive_write.consent_home_disconnect",
        "google.drive_write.consent_home_finish", "google.drive_write.consent_home_paste"] {
        refused(rpc(&h, method, json!({"acknowledge_google_data":true,"acknowledge_write":true,
            "redirect_origin":"http://127.0.0.1:18889","user_id":"admin-test"}), &admin()).await);
    }
}


async fn fresh_params(h: &MethodHandler, mut params: Value, ctx: &UserContext) -> Value {
    // Non-Admin tests still exercise the authorization gate.
    if ctx.is_admin() {
        let get = ok(Box::pin(h.handle("google.drive_write.get", json!({}), ctx)).await);
        params["expected_hash"] = get["hash"].clone();
    }
    params
}
async fn save(h: &MethodHandler, params: Value, ctx: &UserContext, lookup: &dyn FolderLookup) -> WsFrame {
    h.drive_write_set(fresh_params(h, params, ctx).await, ctx, lookup).await
}

#[tokio::test]
async fn drive_settings_require_fresh_client_hash() {
    let (dir, h) = fixture().await;
    let path = dir.path().join("config.toml");
    let get = ok(rpc(&h, "google.drive_write.get", json!({}), &admin()).await);
    assert_eq!(get["hash"], super::super::config_commit::content_hash(ORIGINAL));
    for hash in [None, Some(json!(null)), Some(json!(42)), Some(json!(false))] {
        let mut p = input();
        if let Some(hash) = hash { p["expected_hash"] = hash; }
        let result = h.drive_write_set(p, &admin(), &Fake::unavailable()).await;
        match result { WsFrame::Response {ok:false,error:Some(e),..} => assert_eq!(e.as_str(), Some("設定頁的資料已過期，請重新整理後再儲存。")), other => panic!("{other:?}") }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), ORIGINAL);
    }
    let changed = format!("{ORIGINAL}\n# another admin\n");
    std::fs::write(&path, &changed).unwrap();
    let mut p = input(); p["expected_hash"] = get["hash"].clone();
    let result = h.drive_write_set(p, &admin(), &Fake::unavailable()).await;
    match result { WsFrame::Response {ok:false,error:Some(e),..} => assert_eq!(e.as_str(), Some("設定已被其他人變更，請重新整理後再儲存。")), other => panic!("{other:?}") }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), changed);
    let fresh = ok(rpc(&h, "google.drive_write.get", json!({}), &admin()).await);
    let mut p = input(); p["expected_hash"] = fresh["hash"].clone();
    let result = ok(h.drive_write_set(p, &admin(), &Fake::unavailable()).await);
    let raw = std::fs::read_to_string(&path).unwrap();
    assert_eq!(result["hash"], super::super::config_commit::content_hash(&raw));
    assert!(policy::load(dir.path()).ack.is_some());
}

#[tokio::test]
async fn drive_settings_recheck_client_hash_after_folder_lookup() {
    struct ConcurrentAdmin { path: std::path::PathBuf, content: String }
    #[async_trait::async_trait]
    impl FolderLookup for ConcurrentAdmin {
        async fn lookup(&self, _: &str) -> Option<Value> {
            std::fs::write(&self.path, &self.content).unwrap();
            None
        }
    }
    let (dir, h) = fixture().await;
    let path = dir.path().join("config.toml");
    let get = ok(rpc(&h, "google.drive_write.get", json!({}), &admin()).await);
    let changed = format!("{ORIGINAL}\n# concurrent admin\n");
    let lookup = ConcurrentAdmin {path:path.clone(), content:changed.clone()};
    let mut p = input(); p["expected_hash"] = get["hash"].clone();
    p["folders"] = json!([ID]); p["acknowledge_risk"] = json!(true);
    let response = h.drive_write_set(p, &admin(), &lookup).await;
    match response { WsFrame::Response {ok:false,error:Some(e),..} => assert_eq!(e.as_str(), Some("設定已被其他人變更，請重新整理後再儲存。")), other => panic!("{other:?}") }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), changed);
    assert!(!duduclaw_security::audit::read_recent_events(dir.path(), 20).iter().any(|e| e.event_type == "config_changed"));
}
