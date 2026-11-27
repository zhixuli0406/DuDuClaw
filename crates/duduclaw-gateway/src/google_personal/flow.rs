//! The callback records a code; only its original verified dashboard session
//! can claim and exchange it. Neither path returns a bearer credential.

use super::{access, oauth, pending};
use crate::person_credentials::{self as vault, write_store, Metadata, Status};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use duduclaw_auth::{User, UserDb, UserStatus};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::time::Instant;

pub const CALLBACK_PATH: &str = "/api/me/google/callback";

pub fn active_user(db: &UserDb, user: &str) -> Result<User, String> {
    db.get_user(user)?
        .filter(|u| u.status == UserStatus::Active)
        .ok_or_else(|| "目前帳號不是可用狀態，不能變更 Google 連線。".into())
}

pub async fn start(
    home: &Path,
    db: &UserDb,
    user: &str,
    session: &str,
    origin: &str,
    allowed: &[String],
    acknowledge_google_data: bool,
) -> Result<Value, String> {
    start_with_meetings(home, db, user, session, origin, allowed, acknowledge_google_data, false).await
}

pub async fn start_with_meetings(
    home: &Path,
    db: &UserDb,
    user: &str,
    session: &str,
    origin: &str,
    allowed: &[String],
    acknowledge_google_data: bool,
    meeting_recordings: bool,
) -> Result<Value, String> {
    start_consent(home, db, user, session, origin, allowed, acknowledge_google_data, meeting_recordings, false, false, false, None).await
}

pub async fn start_drive_tools(
    home: &Path,
    db: &UserDb,
    user: &str,
    session: &str,
    origin: &str,
    allowed: &[String],
    acknowledge_google_data: bool,
) -> Result<Value, String> {
    start_consent(home, db, user, session, origin, allowed, acknowledge_google_data, false, true, false, false, None).await
}

pub(crate) async fn start_authorization_request(
    home: &Path, db: &UserDb, user: &str, session: &str, origin: &str,
    allowed: &[String], request: &crate::google_authorization_requests::store::Request,
) -> Result<Value,String> {
    start_consent(home,db,user,session,origin,allowed,true,false,
        matches!(request.purpose.as_str(), "drive_tools" | "workspace"),false,false,Some(request)).await
}

pub async fn start_chat_send(home: &Path, db: &UserDb, user: &str, session: &str, origin: &str, allowed: &[String], acknowledge: bool) -> Result<Value, String> {
    start_consent(home, db, user, session, origin, allowed, acknowledge, false, false, true, false, None).await
}

pub async fn start_drive_write(
    home: &Path, db: &UserDb, user: &str, session: &str, origin: &str,
    allowed: &[String], acknowledge_google_data: bool, acknowledge_write: bool,
) -> Result<Value, String> {
    if !acknowledge_google_data {
        return Err("請先閱讀並確認 Google 資料的存取、分享、保存與刪除說明。".into());
    }
    if !acknowledge_write {
        return Err("請先確認要開放 AI 在雲端硬碟寫入。".into());
    }
    start_consent(home, db, user, session, origin, allowed, true, false, false, false, true, None).await
}

fn require_write_policy(home: &Path) -> Result<(), String> {
    if !crate::google_workspace::drive_write_policy::load(home).any_enabled() {
        return Err("管理員還沒開放 AI 在雲端硬碟寫入。".into());
    }
    Ok(())
}

fn ordinary_for_write(home: &Path, user: &str) -> Result<Metadata, String> {
    vault::get(home, user)?.filter(|r| r.metadata.status == Status::Connected)
        .map(|r| r.metadata)
        .ok_or_else(|| "請先重新連結自己的 Google 帳號，再開放雲端硬碟寫入。".into())
}

async fn start_consent(
    home: &Path,
    db: &UserDb,
    user: &str,
    session: &str,
    origin: &str,
    allowed: &[String],
    acknowledge_google_data: bool,
    meeting_recordings: bool,
    drive_tools: bool,
    chat_send: bool,
    drive_write: bool,
    authorization_request: Option<&crate::google_authorization_requests::store::Request>,
) -> Result<Value, String> {
    if drive_write { require_write_policy(home)?; }
    if chat_send && !crate::google_workspace::chat_send_enabled(home) { return Err(crate::google_workspace::chat_send::GUIDANCE.into()); }
    if [meeting_recordings, drive_tools, chat_send, drive_write].iter().filter(|v| **v).count() > 1 {
        return Err("請每次選擇一項 Google 額外服務。".into());
    }
    if !acknowledge_google_data {
        return Err("請先閱讀並確認 Google 資料的存取、分享、保存與刪除說明。".into());
    }
    active_user(db, user)?;
    let _grant_lease = super::grants::acquire(home).await?;
    let _key = crate::google_identity::protection_gate_async(home)
        .await
        .map_err(|r| r.message_zh)?;
    let client = crate::mcp_oauth::get_client_config(home, "google")
        .ok_or("管理員還沒設定 Google 連結，請聯絡管理員。")?;
    if client.client_id.is_empty() {
        return Err("Google 應用程式設定不完整，請聯絡管理員。".into());
    }
    let (plan, dashboard_base) = crate::remote_mcp::connect::dashboard_redirect_plan(
        home, origin, allowed, CALLBACK_PATH,
    )?;
    let paste = matches!(&plan, crate::remote_mcp::connect::RedirectPlan::Paste { .. });
    let redirect_uri = plan.redirect_uri().to_string();
    let verifier = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    // Incremental consent extends this person's existing Google account;
    // choosing another account cannot silently replace it.
    let expected_connection = if drive_write {
        Some(ordinary_for_write(home, user)?)
    } else if chat_send {
        Some(vault::get(home, user)?.ok_or("請先連結自己的 Google 帳號，再開放 Chat 傳送。")?.metadata)
    } else if authorization_request.is_some() {
        vault::get(home,user)?.map(|r|r.metadata)
    } else if drive_tools {
        Some(vault::get(home, user)?.ok_or("請先連結自己的 Google 帳號，再開放雲端硬碟工具。")?.metadata)
    } else if meeting_recordings {
        Some(vault::get(home, user)?.ok_or("請先連結自己的 Google 帳號，再開放會議錄影。")?.metadata)
    } else {
        None
    };
    let chat_read = !drive_write && !chat_send && !meeting_recordings && !drive_tools && crate::google_workspace::chat_read_enabled(home);
    let requested = if drive_write {
        vec![oauth::DRIVE_WRITE_SCOPE, "https://www.googleapis.com/auth/documents",
            "https://www.googleapis.com/auth/spreadsheets", "openid", "email"]
    } else {
        let purpose = if chat_send { "chat_send" } else if drive_tools { "drive_tools" }
            else if meeting_recordings { "meeting_recordings" } else { "base" };
        let mut purposes = expected_connection.as_ref().map(|old|
            old.local_purposes.iter().map(String::as_str).collect::<Vec<_>>()
        ).unwrap_or_default();
        purposes.push(purpose);
        if authorization_request.is_some_and(|request| request.purpose == "workspace") {
            purposes.extend(["base", "drive_tools"]);
        }
        oauth::scopes_for_purposes(&purposes, chat_read)
    };
    let epoch = if drive_write { write_store::begin_connection(home, "user", user)? }
        else { vault::begin_connection(home, user)? };
    let google_data_consent = crate::google_data_consent::Evidence::issue(user)?;
    pending::cancel_user(home, user);
    let state = pending::insert(pending::Pending {
        home: home.into(),
        user_id: user.into(),
        session_ref: session.into(),
        redirect_uri: redirect_uri.clone(),
        dashboard_base,
        paste,
        code_verifier: verifier,
        client_id: client.client_id.clone(),
        client_secret: client.client_secret,
        epoch,
        meeting_recordings,
        drive_tools,
        drive_write,
        chat_read,
        chat_send,
        expected_connection,
        google_data_consent,
        created: Instant::now(),
        code: None,
    });
    let mut authorization_url = oauth::authorization_url_for_scopes(
        &client.client_id, &redirect_uri, &state, &challenge, &requested,
    )?;
    if let Some(request) = authorization_request {
        let mut url = url::Url::parse(&authorization_url).map_err(|_| "Google 授權網址無效。")?;
        url.query_pairs_mut().append_pair("login_hint", &request.email);
        authorization_url = url.to_string();
    }
    Ok(
        json!({ "authorization_url": authorization_url, "state": state, "completion": if paste { "paste" } else { "redirect" }, "redirect_uri": redirect_uri, "expires_in": 600 }),
    )
}

pub async fn finish(
    home: &Path,
    db: &UserDb,
    user: &str,
    session: &str,
    state: &str,
) -> Result<Value, String> {
    active_user(db, user)?;
    let authorization_request = crate::google_authorization_requests::store::Store::open(home)?
        .oauth(state, user, session)?;
    if let Some(request) = &authorization_request {
        crate::google_authorization_requests::validate(home, db, request)?;
    }
    let pending = pending::take(home, state, user, session)?;
    crate::google_data_consent::require_current(Some(&pending.google_data_consent))?;
    if pending.drive_write {
        if authorization_request.is_some() {
            return Err("這次 Google 連結已失效，請重新開始。".into());
        }
        return finish_drive_write(home, db, user, &pending).await;
    }
    let grant_lease = super::grants::acquire(home).await?;
    let tokens = oauth::exchange(
        &pending.client_id,
        &pending.client_secret,
        pending.code.as_deref().ok_or("尚未收到 Google 回覆。")?,
        &pending.code_verifier,
        &pending.redirect_uri,
    )
    .await
    .map_err(|_| "Google 連結未完成，請重新開始。沒有保存這次的憑證。")?;
    let info = oauth::userinfo(&tokens.secret.access_token)
        .await
        .map_err(|_| "無法確認 Google 帳號或信箱尚未驗證，沒有保存連線。")?;
    if let Some(request) = &authorization_request {
        crate::google_authorization_requests::validate(home, db, request)?;
        if !crate::google_authorization_requests::account_matches(request, &info.sub, &info.email) {
            return Err("Google 帳號與授權請求指定的本人身分不符，沒有保存這次連線。".into());
        }
    }
    let scopes = tokens
        .granted_scopes
        .ok_or("Google 沒有回傳實際授權範圍，請重新連結。")?;
    if scopes.iter().any(|scope| scope == oauth::DRIVE_WRITE_SCOPE) {
        return Err("一般 Google 連線收到不應持有的雲端硬碟寫入權限，這次沒有保存。請到「我的連線」移除寫入連線，再重新連結。".into());
    }
    if pending.expected_connection.as_ref().is_some_and(|old| old.google_sub != info.sub) {
        return Err("請使用原本連結的 Google 帳號開放額外服務。這次沒有取代既有連線。".into());
    }
    if pending.chat_send && (!crate::google_workspace::chat_send_enabled(home)
        || !super::scopes::permitted("google_chat_send", &scopes)) {
        return Err("Chat 傳送未啟用或 Google 未授予傳送權限，沒有變更連線。".into());
    }
    if pending.meeting_recordings && !super::meeting_access::has_scopes(&scopes) {
        return Err("尚未開放所選雲端硬碟檔案及 Meet 錄影的讀取權限，這次沒有變更連線。".into());
    }
    if pending.drive_tools && !scopes.iter().any(|scope| scope == oauth::DRIVE_TOOLS_SCOPE) {
        return Err("尚未開放雲端硬碟唯讀權限，這次沒有變更連線。".into());
    }
    if pending.expected_connection.as_ref().is_some_and(|old| {
        !old.granted_scopes.iter().all(|scope| scopes.contains(scope))
    }) {
        return Err("Google 未保留原有授權範圍，這次沒有取代既有憑證。請重新開始。".into());
    }
    let purpose = if pending.chat_send { "chat_send" } else if pending.drive_tools { "drive_tools" }
        else if pending.meeting_recordings { "meeting_recordings" } else { "base" };
    let mut local_purposes = pending.expected_connection.as_ref()
        .map(|old| old.local_purposes.clone()).unwrap_or_default();
    if !local_purposes.iter().any(|p| p == purpose) {
        local_purposes.push(purpose.into());
    }
    if authorization_request.as_ref().is_some_and(|request| request.purpose == "workspace") {
        local_purposes.push("base".into());
        local_purposes.push("drive_tools".into());
    }
    if pending.chat_read && crate::google_workspace::CHAT_READ_SCOPES.iter()
        .all(|scope| scopes.iter().any(|granted| granted == scope)) {
        local_purposes.push("chat_read".into());
    }
    local_purposes.sort();
    local_purposes.dedup();
    // Provider-returned scopes never become locally consented purposes.
    let purposes = local_purposes.iter().map(String::as_str).collect::<Vec<_>>();
    let expected_scopes = oauth::scopes_for_purposes(&purposes, pending.chat_read)
        .into_iter().map(str::to_string).collect::<Vec<_>>();
    let mut unexpected_scopes: Vec<_> = scopes.iter()
        .filter(|scope| !expected_scopes.contains(scope)).cloned().collect();
    unexpected_scopes.sort();
    unexpected_scopes.dedup();
    let scope_digest = scope_digest(&scopes);
    let metadata = Metadata {
        owner_kind: "user".into(),
        owner_id: user.into(),
        provider: "google".into(),
        credential_id: uuid::Uuid::new_v4().to_string(),
        revision: 1,
        account_email: info.email,
        google_sub: info.sub,
        hosted_domain: info.hd,
        granted_scopes: scopes,
        local_purposes,
        status: Status::Connected,
        status_before_suspension: None,
        connected_at: chrono::Utc::now().to_rfc3339(),
        last_refresh_at: None,
        oauth_client_id: pending.client_id.clone(),
        google_data_consent: Some(pending.google_data_consent.clone()),
    };
    if !access::domain_allowed(home, &metadata)? {
        // Apply the same shared-grant protection to a rejected connection.
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            super::lifecycle::safe_revoke_with_lease(home, &metadata, &tokens.secret, &grant_lease),
        )
        .await;
        return Err("這個 Google 帳號不在管理員允許的網域內，沒有保存連線。".into());
    }
    if tokens.secret.refresh_token.is_none() {
        return Err("Google 沒有提供背景更新憑證；請重新連結，並同意背景存取。".into());
    }
    active_user(db, user)?;
    if crate::mcp_oauth::get_client_config(home, "google").is_none_or(|c| {
        c.client_id != pending.client_id || c.client_secret != pending.client_secret
    }) {
        return Err("管理員已變更 Google 連結設定，請重新開始。".into());
    }
    let key = crate::google_identity::protection_gate_async(home)
        .await
        .map_err(|r| r.message_zh)?;
    active_user(db, user)?;
    if !access::domain_allowed(home, &metadata)? {
        return Err("管理員已變更允許的 Google 網域，請重新開始。".into());
    }
    if let Some(request) = &authorization_request {
        crate::google_authorization_requests::validate(home, db, request)?;
        if !super::scopes::permitted_for(&request.tool, &metadata) {
            return Err("Google 尚未授予原任務所需的用途與範圍，沒有保存這次連線。".into());
        }
    }
    // v1.81 C4: the isolated connector catalog is fenced around publication
    // (same account: new generation / relink; another account: revoke).
    super::lifecycle::publish_connection(home, user, &metadata, || {
        vault::connect_with_previous(home, &key, metadata.clone(), &tokens.secret, pending.epoch, pending.expected_connection.as_ref())
    })?;
    if let Some(request) = authorization_request {
        crate::google_authorization_requests::routes::complete(home, db, request, Some(&metadata))?;
    }
    let detail = json!({
        "google_email": metadata.account_email,
        "scope_count": metadata.granted_scopes.len(),
        "purpose": purpose,
        "scope_digest": scope_digest,
        "unexpected_scopes": unexpected_scopes,
        "google_data_consent": metadata.google_data_consent,
    })
    .to_string();
    let _ = db.log_action(
        Some(user),
        "user.google_connect",
        Some(user),
        Some(&detail),
        None,
    );
    Ok(json!({"connection": metadata}))
}

/// This path never revokes a rejected token: Google revocation can affect
/// the whole project, including the member's ordinary connection.
async fn finish_drive_write(
    home: &Path, db: &UserDb, user: &str, pending: &pending::Pending,
) -> Result<Value, String> {
    if pending.meeting_recordings || pending.drive_tools || pending.chat_read || pending.chat_send {
        return Err("請每次選擇一項 Google 額外服務。".into());
    }
    require_write_policy(home)?;
    let expected = pending.expected_connection.as_ref()
        .ok_or("請先連結自己的 Google 帳號，再重新開始。")?;
    let _grant_lease = super::grants::acquire(home).await?;
    let tokens = oauth::exchange(
        &pending.client_id, &pending.client_secret,
        pending.code.as_deref().ok_or("尚未收到 Google 回覆。")?,
        &pending.code_verifier, &pending.redirect_uri,
    ).await.map_err(|_| "Google 連結未完成，請重新開始。沒有保存這次的憑證。")?;
    let info = oauth::userinfo(&tokens.secret.access_token).await
        .map_err(|_| "無法確認 Google 帳號或信箱尚未驗證，沒有保存連線。")?;
    if info.sub != expected.google_sub {
        return Err("請使用原本連結的 Google 帳號開放雲端硬碟寫入，這次沒有保存。".into());
    }
    let scopes = tokens.granted_scopes
        .ok_or("Google 沒有回傳實際授權範圍，請重新連結。")?;
    if !scopes.iter().any(|scope| scope == oauth::DRIVE_WRITE_SCOPE) {
        return Err("Google 尚未授予雲端硬碟寫入權限，這次沒有保存。".into());
    }
    if tokens.secret.refresh_token.as_deref().is_none_or(str::is_empty) {
        return Err("Google 沒有提供背景更新憑證；請重新連結，並同意背景存取。".into());
    }
    // Domain validation uses the newly verified provider identity, not the
    // old ordinary account's email/domain. This clone is never published.
    let mut identity = expected.clone();
    identity.account_email = info.email.clone();
    identity.google_sub = info.sub.clone();
    identity.hosted_domain = info.hd.clone();
    if !access::domain_allowed(home, &identity)? {
        return Err("這個 Google 帳號不在管理員允許的網域內，沒有保存連線。".into());
    }
    active_user(db, user)?;
    if crate::mcp_oauth::get_client_config(home, "google").is_none_or(|c| {
        c.client_id != pending.client_id || c.client_secret != pending.client_secret
    }) {
        return Err("管理員已變更 Google 連結設定，請重新開始。".into());
    }
    let key = crate::google_identity::protection_gate_async(home).await
        .map_err(|r| r.message_zh)?;
    active_user(db, user)?;
    require_write_policy(home)?;
    if crate::mcp_oauth::get_client_config(home, "google").is_none_or(|c| {
        c.client_id != pending.client_id || c.client_secret != pending.client_secret
    }) {
        return Err("管理員已變更 Google 連結設定，請重新開始。".into());
    }
    if !access::domain_allowed(home, &identity)? {
        return Err("管理員已變更允許的 Google 網域，請重新開始。".into());
    }
    let current = ordinary_for_write(home, user)?;
    if current.google_sub != expected.google_sub || current.credential_id != expected.credential_id {
        return Err("原本的 Google 連線已變更，這次沒有保存。請重新開始。".into());
    }
    let metadata = write_store::Metadata {
        owner_kind: "user".into(), owner_id: user.into(), purpose: "drive_write".into(),
        provider: "google".into(), credential_id: uuid::Uuid::new_v4().to_string(),
        revision: 1, account_email: info.email, google_sub: info.sub, hosted_domain: info.hd,
        granted_scopes: scopes, status: Status::Connected, status_before_suspension: None,
        connected_at: chrono::Utc::now().to_rfc3339(), last_refresh_at: None,
        oauth_client_id: pending.client_id.clone(),
        google_data_consent: Some(pending.google_data_consent.clone()),
    };
    write_store::connect(home, &key, metadata.clone(), &tokens.secret, pending.epoch)?;
    let detail = json!({"google_email": metadata.account_email,
        "scope_count": metadata.granted_scopes.len(),
        "scope_digest": scope_digest(&metadata.granted_scopes)}).to_string();
    let _ = db.log_action(Some(user), "user.google_write_connect", Some(user), Some(&detail), None);
    Ok(json!({"connection": metadata}))
}

pub fn write_disconnect(home: &Path, db: &UserDb, user: &str) -> Result<(), String> {
    active_user(db, user)?;
    write_store::disconnect(home, "user", user)?;
    let _ = db.log_action(Some(user), "user.google_write_disconnect", Some(user), None, None);
    Ok(())
}

/// Stable set digest: ordering and duplicate provider entries do not affect it.
fn scope_digest(scopes: &[String]) -> String {
    let mut scopes = scopes.to_vec();
    scopes.sort();
    scopes.dedup();
    let mut digest = Sha256::new();
    for scope in scopes {
        digest.update((scope.len() as u64).to_be_bytes());
        digest.update(scope.as_bytes());
    }
    hex::encode(digest.finalize())
}
