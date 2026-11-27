use super::*;
use duduclaw_auth::{UserRole,UserStatus};
use serde_json::json;

fn row(home:&Path) -> Request {
    Request {id:uuid::Uuid::new_v4().to_string(),home:home.canonicalize().unwrap(),user:"alice".into(),
        agent:"bot".into(),tool:"gmail_search".into(),purpose:"base".into(),mode:"oauth".into(),
        email:"alice@example.com".into(),google_sub:Some("123".into()),channel:"telegram".into(),
        sender:"42".into(),conversation:"7".into(),session_id:"telegram:7".into(),session_generation:None,source:None,
        text:"Find my Gmail receipt".into(),acl_version:1,config_version:"config-v1".into(),
        expires:chrono::Utc::now().timestamp()+600}
}
#[test]
fn ticket_is_hashed_preview_is_read_only_and_other_members_cannot_consume() {
    let home=tempfile::tempdir().unwrap();let store=Store::open(home.path()).unwrap();
    let request=row(home.path());let ticket=store.issue(request).unwrap();
    assert_eq!(ticket.len(),64);
    for _ in 0..3 { assert!(store.preview(&ticket,"alice").is_ok()); }
    assert!(store.preview(&ticket,"bob").is_err());
    assert!(store.claim_ticket(&ticket,"bob","s").is_err());
    assert!(store.claim_ticket(&ticket,"alice","s").is_ok());
    assert!(store.claim_ticket(&ticket,"alice","s").is_err());
    assert!(store.preview(&ticket,"alice").is_err());
    let bytes=std::fs::read(home.path().join("protected/google_authorization_requests.db")).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains(&ticket));
}
#[test]
fn copied_store_cannot_cross_home_and_expiry_is_fail_closed() {
    let home=tempfile::tempdir().unwrap();let other=tempfile::tempdir().unwrap();
    let store=Store::open(home.path()).unwrap();let ticket=store.issue(row(home.path())).unwrap();
    drop(store);
    std::fs::create_dir(other.path().join("protected")).unwrap();
    std::fs::copy(home.path().join("protected/google_authorization_requests.db"),other.path().join("protected/google_authorization_requests.db")).unwrap();
    assert!(Store::open(other.path()).unwrap().preview(&ticket,"alice").is_err());
    let conn=rusqlite::Connection::open(home.path().join("protected/google_authorization_requests.db")).unwrap();
    conn.execute("UPDATE requests SET expires=0",[]).unwrap();
    assert!(Store::open(home.path()).unwrap().claim_ticket(&ticket,"alice","s").is_err());
}
#[test]
fn restart_and_duplicate_completion_claim_only_one_continuation() {
    let home=tempfile::tempdir().unwrap();let store=Store::open(home.path()).unwrap();
    let ticket=store.issue(row(home.path())).unwrap();let row=store.claim_ticket(&ticket,"alice","login-a").unwrap();
    store.bind_oauth(&row.id,"oauth-state").unwrap();
    assert!(store.oauth("oauth-state","bob","login-a").is_err());
    assert!(store.oauth("oauth-state","alice","login-b").is_err());
    let bound=store.oauth("oauth-state","alice","login-a").unwrap().unwrap();
    store.ready(&bound,&json!({"credential_id":"c1"})).unwrap();
    assert!(store.ready(&bound,&json!({"credential_id":"c2"})).is_err());
    drop(store);
    let restarted=Store::open(home.path()).unwrap();
    assert_eq!(restarted.ready_rows().unwrap().len(),1);
    restarted.cas(&row.id,"ready","claimed").unwrap();
    assert!(Store::open(home.path()).unwrap().cas(&row.id,"ready","claimed").is_err());
    assert!(Store::open(home.path()).unwrap().ready_rows().unwrap().is_empty());
    assert!(restarted.oauth("oauth-state","alice","login-a").is_err());
}

fn ready_request(home:&Path) -> Request {
    let store=Store::open(home).unwrap();
    let ticket=store.issue(row(home)).unwrap();
    let request=store.claim_ticket(&ticket,"alice","login-a").unwrap();
    store.ready(&request,&json!({"credential_id":"c1"})).unwrap();
    request
}

fn request_status(home:&Path,id:&str) -> String {
    rusqlite::Connection::open(home.join("protected/google_authorization_requests.db")).unwrap()
        .query_row("SELECT status FROM requests WHERE id=?1",[id],|r|r.get(0)).unwrap()
}

#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn concurrent_resume_workers_only_claimant_performs_external_io() {
    use std::sync::{Arc,atomic::{AtomicUsize,Ordering}};
    let home=tempfile::tempdir().unwrap();
    let request=ready_request(home.path());
    let calls=Arc::new(AtomicUsize::new(0));
    let barrier=Arc::new(tokio::sync::Barrier::new(2));
    let mut workers=Vec::new();
    for _ in 0..2 {
        let home=home.path().to_path_buf();let id=request.id.clone();
        let calls=calls.clone();let barrier=barrier.clone();
        workers.push(tokio::spawn(async move {
            barrier.wait().await;
            resume::claim_and_run(&home,&id,|| async {
                assert_eq!(request_status(&home,&id),"claimed");
                // Stands in for secret-manager/provider I/O, including Feishu
                // token acquisition. The losing worker must never enter here.
                calls.fetch_add(1,Ordering::SeqCst);
                tokio::task::yield_now().await;
                Ok(())
            }).await
        }));
    }
    let first=workers.remove(0).await.unwrap();let second=workers.remove(0).await.unwrap();
    assert_eq!(usize::from(first.is_ok())+usize::from(second.is_ok()),1);
    assert_eq!(calls.load(Ordering::SeqCst),1);
    assert_eq!(request_status(home.path(),&request.id),"done");
}

#[tokio::test]
async fn sender_resolution_failure_after_claim_stops_without_retry_even_if_expired() {
    for expire in [false,true] {
        let home=tempfile::tempdir().unwrap();let request=ready_request(home.path());
        let calls=std::sync::atomic::AtomicUsize::new(0);
        let outcome=resume::claim_and_run(home.path(),&request.id,|| async {
            assert_eq!(request_status(home.path(),&request.id),"claimed");
            calls.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
            if expire {
                // Expire during claimed I/O without crossing the one-day
                // retention cutoff, which intentionally deletes old rows.
                rusqlite::Connection::open(home.path().join("protected/google_authorization_requests.db")).unwrap()
                    .execute("UPDATE requests SET expires=?2 WHERE id=?1",
                        rusqlite::params![&request.id,chrono::Utc::now().timestamp()-1]).unwrap();
            }
            Err("sender credential/token resolution failed".into())
        }).await;
        assert!(outcome.is_err());
        assert_eq!(request_status(home.path(),&request.id),"stopped");
        assert!(Store::open(home.path()).unwrap().ready_rows().unwrap().is_empty());
        assert!(resume::claim_and_run(home.path(),&request.id,|| async {
            calls.fetch_add(1,std::sync::atomic::Ordering::SeqCst);Ok(())
        }).await.is_err());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst),1);
    }
}
#[test]
fn account_hint_cannot_replace_verified_sub_or_email() {
    let home=tempfile::tempdir().unwrap();let row=row(home.path());
    assert!(account_matches(&row,"123","ALICE@example.com"));
    assert!(!account_matches(&row,"attacker","alice@example.com"));
    assert!(!account_matches(&row,"123","attacker@example.com"));
    assert!(!account_matches(&row,"","alice@example.com"));
}
#[test]
fn external_urls_are_admin_only_https_non_loopback_and_preserve_prefix() {
    for raw in ["http://example.com/", "https://localhost/", "https://localhost./", "https://a.localhost/",
        "https://127.0.0.1/", "https://127.1/", "https://[::1]/", "https://[::ffff:127.0.0.1]/",
        "https://user:pass@example.com/", "https://example.com/?redirect=evil", "https://example.com/#evil"] {
        assert!(!url::external_base(&::url::Url::parse(raw).unwrap()),"{raw}");
    }
    let home=tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"),"[dashboard]\npublic_url='https://example.com/team/dudu'").unwrap();
    assert_eq!(url::landing_base(home.path(),"telegram").unwrap().unwrap(),"https://example.com/team/dudu/me/google/authorize");
    std::fs::write(home.path().join("config.toml"),"").unwrap();
    for channel in ["telegram","slack","discord","line","whatsapp","feishu","googlechat","teams","wecom","dingtalk"] {
        assert!(url::landing_base(home.path(),channel).unwrap().is_none());
    }
    assert_eq!(url::landing_base(home.path(),"webchat").unwrap().as_deref(),Some("/me/google/authorize"));
}
#[test]
fn all_channels_have_text_and_buttons_only_open_urls() {
    let url="https://dashboard.example/team/me/google/authorize#ticket=fixture";
    for channel in ["telegram","slack","discord","line","whatsapp","feishu","googlechat","teams","wecom","dingtalk","webchat"] {
        let text=render::notice(url);assert_eq!(text.lines().last(),Some(url));
        let markup=render::markup(channel,url);
        assert_eq!(markup.is_some(),matches!(channel,"telegram"|"slack"|"discord"|"line"));
        if let Some(markup)=markup {
            let body=crate::channel_sender::card_message_payload(channel,"conversation",&text,markup,true).unwrap();
            let raw=body.to_string();assert!(raw.contains(url));
            assert!(!raw.contains("callback_data"));assert!(!raw.contains("postback"));
        }
    }
}
#[test]
fn explicit_verified_bindings_only_and_rebinding_or_suspension_refuses() {
    let home=tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"),"[google]\nchat_identity_from_account=true").unwrap();
    let db=UserDb::new(&home.path().join("users.db")).unwrap();
    let alice=db.create_user("alice@c.test","Alice","fixture-password",UserRole::Employee).unwrap();
    let bob=db.create_user("bob@c.test","Bob","fixture-password",UserRole::Employee).unwrap();
    crate::googlechat_identity::test_support::link_google(home.path(),&[(&alice.id,"101","connected")]);
    assert!(mapped_member(home.path(),&db,"googlechat","users/101").is_err(),"derived-only identity is insufficient");
    db.bind_channel_identity(&alice.id,"googlechat","users/101",true).unwrap();
    assert_eq!(mapped_member(home.path(),&db,"googlechat","users/101").unwrap(),alice.id);
    db.bind_channel_identity(&alice.id,"telegram","display-name",false).unwrap();
    assert!(mapped_member(home.path(),&db,"telegram","display-name").is_err());
    let before=db.acl_generation().unwrap();
    db.unbind_channel_identity("googlechat","users/101").unwrap();
    db.bind_channel_identity(&bob.id,"googlechat","users/101",true).unwrap();
    assert_ne!(before,db.acl_generation().unwrap());
    assert!(mapped_member(home.path(),&db,"googlechat","users/101").is_err(),"conflicting binding refuses");
    db.bind_channel_identity(&alice.id,"slack","member-a",true).unwrap();
    db.set_user_status(&alice.id,UserStatus::Suspended).unwrap();
    assert!(mapped_member(home.path(),&db,"slack","member-a").is_err());
}
#[test]
fn stale_acl_cannot_record_consent_even_when_member_and_employee_match() {
    let home=tempfile::tempdir().unwrap();let db=UserDb::new(&home.path().join("users.db")).unwrap();
    let alice=db.create_user("a@c.test","Alice","fixture-password",UserRole::Employee).unwrap();
    db.bind_agent(&alice.id,"bot",AccessLevel::Owner).unwrap();let generation=db.acl_generation().unwrap();
    db.bind_channel_identity(&alice.id,"telegram","42",true).unwrap();
    assert!(db.set_google_consent_versioned(&alice.id,"bot",true,&alice.id,Some(generation)).is_err());
    assert_eq!(db.google_consent(&alice.id,"bot").unwrap(),None);
    assert!(db.set_google_consent_versioned(&alice.id,"bot",true,"admin",Some(db.acl_generation().unwrap())).is_err());
}
#[tokio::test]
async fn confirmation_requires_both_disclosures_before_consuming_ticket() {
    let home=tempfile::tempdir().unwrap();let db=UserDb::new(&home.path().join("users.db")).unwrap();
    let ticket=Store::open(home.path()).unwrap().issue(row(home.path())).unwrap();
    for (data,shared) in [(false,false),(true,false),(false,true)] {
        assert!(routes::confirm(home.path(),&db,"alice",&uuid::Uuid::new_v4().to_string(),&ticket,"https://example.com",data,shared,false).await.is_err());
        assert!(Store::open(home.path()).unwrap().preview(&ticket,"alice").is_ok());
    }
}
#[test]
fn preview_and_failure_responses_cannot_be_cached_or_referred() {
    use axum::response::IntoResponse;
    let response=routes::private_response(axum::http::StatusCode::FORBIDDEN.into_response());
    assert_eq!(response.headers()["cache-control"],"no-store");
    assert_eq!(response.headers()["referrer-policy"],"no-referrer");
}

fn member_fixture() -> (tempfile::TempDir,UserDb,Request) {
    let home=tempfile::tempdir().unwrap();
    let db=UserDb::new(&home.path().join("users.db")).unwrap();
    let alice=db.create_user("alice@c.test","Alice","fixture-password",UserRole::Employee).unwrap();
    db.bind_agent(&alice.id,"bot",AccessLevel::Owner).unwrap();
    db.bind_channel_identity(&alice.id,"telegram","42",true).unwrap();
    std::fs::create_dir_all(home.path().join("agents/bot")).unwrap();
    std::fs::write(home.path().join("config.toml"),"[integrations]\ngoogle_workspace=true\n[dashboard]\npublic_url='https://example.com/team'").unwrap();
    std::fs::write(home.path().join("agents/bot/agent.toml"),format!("[google]\nidentity='user:{}'",alice.id)).unwrap();
    crate::googlechat_identity::test_support::link_google(home.path(),&[(&alice.id,"123","connected")]);
    let mut request=row(home.path());request.user=alice.id;request.email="person0@c.test".into();
    request.acl_version=db.acl_generation().unwrap();request.config_version=config_version(home.path(),"bot").unwrap();
    (home,db,request)
}
#[test]
fn pending_request_rechecks_live_binding_suspension_permission_and_config() {
    let (home,db,mut request)=member_fixture();
    validate(home.path(),&db,&request).unwrap();
    db.unbind_channel_identity("telegram","42").unwrap();
    db.bind_channel_identity(&request.user,"telegram","42",true).unwrap();
    assert!(validate(home.path(),&db,&request).is_err(),"rebind back to the same person still changes the identity version");
    request.acl_version=db.acl_generation().unwrap();
    validate(home.path(),&db,&request).unwrap();
    db.set_user_status(&request.user,UserStatus::Suspended).unwrap();
    request.acl_version=db.acl_generation().unwrap();
    assert!(validate(home.path(),&db,&request).is_err());
    db.set_user_status(&request.user,UserStatus::Active).unwrap();
    db.bind_agent(&request.user,"bot",AccessLevel::Operator).unwrap();
    request.acl_version=db.acl_generation().unwrap();
    assert!(validate(home.path(),&db,&request).is_err());
    db.bind_agent(&request.user,"bot",AccessLevel::Owner).unwrap();
    request.acl_version=db.acl_generation().unwrap();
    validate(home.path(),&db,&request).unwrap();
    std::fs::write(home.path().join("agents/bot/agent.toml"),"[google]\nidentity='home'").unwrap();
    assert!(validate(home.path(),&db,&request).is_err());
}
#[test]
fn no_public_url_gives_only_a_hint_and_unprivileged_members_get_no_ticket() {
    let (home,db,request)=member_fixture();
    std::fs::write(home.path().join("config.toml"),"[integrations]\ngoogle_workspace=true").unwrap();
    let who=RequesterIdentity::DashboardUser{user_id:request.user.clone()};
    // WebChat is same-origin and still requires the same person to log in.
    let notice=issue_for_turn(home.path(),&db,"bot","gmail_search","Find Gmail","webchat:session",&who,None,None).unwrap().unwrap();
    assert!(notice.contains("/me/google/authorize#ticket="));
    let other=db.create_user("other@c.test","Other","fixture-password",UserRole::Employee).unwrap();
    let who=RequesterIdentity::DashboardUser{user_id:other.id};
    assert!(issue_for_turn(home.path(),&db,"bot","gmail_search","Find Gmail","webchat:session",&who,None,None).is_err());
    assert!(issue_for_turn(home.path(),&db,"bot","unknown_tool","Find Gmail","webchat:session",&who,None,None).is_err());
    let channel=RequesterIdentity::ChannelUser {channel:"telegram".into(),chat:"7".into(),sender:"42".into(),
        user_id:Some(request.user.clone()),attested_private:false};
    assert_eq!(issue_for_turn(home.path(),&db,"bot","gmail_search","Find Gmail","telegram:7",&channel,None,None).unwrap().as_deref(),Some(DASHBOARD_HINT));
    assert_eq!(Store::open(home.path()).unwrap().statuses(&request.user).unwrap().len(),1,"external fallback does not mint another ticket");
    assert!(url::landing_base(home.path(),"telegram").unwrap().is_none());
    assert!(!DASHBOARD_HINT.contains("http"));assert!(!DASHBOARD_HINT.contains("code"));
}
#[test]
fn csrf_origins_are_explicit_and_url_components_cannot_spoof_an_allowed_host() {
    let allowed=vec!["dashboard.example".to_owned()];
    assert!(routes::valid_origin("https://dashboard.example:443",&allowed));
    assert!(routes::valid_origin("http://127.0.0.1:18889",&[]));
    for origin in ["", "null", "https://dashboard.example.evil", "https://evil.test", "http://dashboard.example",
        "https://user@dashboard.example", "https://dashboard.example/path", "https://dashboard.example?next=evil"] {
        assert!(!routes::valid_origin(origin,&allowed),"{origin}");
    }
}

fn enable_drive_write(home:&Path) {
    use crate::google_workspace::drive_write_policy as policy;
    let mut enabled=policy::DriveWritePolicy::all_off();
    enabled.create=true;
    let ack=policy::sign(&enabled,"admin","admin@c.test",chrono::Utc::now());
    let mut config:toml::Value=std::fs::read_to_string(home.join("config.toml")).unwrap().parse().unwrap();
    config.as_table_mut().unwrap().entry("google").or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut().unwrap().insert("drive_write".into(),policy::render_section(&enabled,&ack));
    std::fs::write(home.join("config.toml"),toml::to_string(&config).unwrap()).unwrap();
}

#[test]
fn drive_write_purpose_tool_matrix_and_disabled_policy_refuse() {
    let (home,db,mut request)=member_fixture();
    request.purpose="drive_write".into();
    request.tool="drive_create_file".into();
    assert!(validate(home.path(),&db,&request).is_err());
    enable_drive_write(home.path());
    request.config_version=config_version(home.path(),"bot").unwrap();
    for tool in ["drive_create_file","drive_update_file","drive_move_file","drive_trash_file",
        "drive_copy_file","docs_insert_text","docs_format","sheets_add_sheet","sheets_add_note"] {
        assert_eq!(purpose(tool),Some("drive_write"),"{tool}");
        request.tool=tool.into();
        validate(home.path(),&db,&request).unwrap();
        request.purpose="drive_tools".into();
        assert!(validate(home.path(),&db,&request).is_err());
        request.purpose="drive_write".into();
    }
    for tool in ["drive_read","drive_search","gmail_search","google_remote_mcp","unknown"] {
        request.tool=tool.into();
        assert!(validate(home.path(),&db,&request).is_err());
    }
}

#[tokio::test]
async fn drive_write_confirmation_requires_explicit_write_ack_without_consuming_ticket() {
    let (home,db,mut request)=member_fixture();
    enable_drive_write(home.path());
    request.purpose="drive_write".into();request.tool="drive_create_file".into();
    request.config_version=config_version(home.path(),"bot").unwrap();
    let user=request.user.clone();
    let ticket=Store::open(home.path()).unwrap().issue(request).unwrap();
    assert!(routes::confirm(home.path(),&db,&user,&uuid::Uuid::new_v4().to_string(),
        &ticket,"http://127.0.0.1:18889",true,true,false).await.is_err());
    assert!(Store::open(home.path()).unwrap().preview(&ticket,&user).is_ok());
}
