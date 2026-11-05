//! The callback records a code; only its original verified dashboard session
//! can claim and exchange it. Neither path returns a bearer credential.

use super::{access, oauth, pending};
use crate::person_credentials::{self as vault, Metadata, Status};
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
    start_consent(home, db, user, session, origin, allowed, acknowledge_google_data, meeting_recordings, false).await
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
    start_consent(home, db, user, session, origin, allowed, acknowledge_google_data, false, true).await
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
) -> Result<Value, String> {
    if meeting_recordings && drive_tools {
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
    let expected_connection = if drive_tools {
        Some(vault::get(home, user)?.ok_or("請先連結自己的 Google 帳號，再開放雲端硬碟工具。")?.metadata)
    } else if meeting_recordings {
        Some(vault::get(home, user)?.ok_or("請先連結自己的 Google 帳號，再開放會議錄影。")?.metadata)
    } else {
        None
    };
    let epoch = vault::begin_connection(home, user)?;
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
        expected_connection,
        google_data_consent,
        created: Instant::now(),
        code: None,
    });
    let authorization_url = if drive_tools {
        oauth::authorization_url_with_drive_tools(&client.client_id, &redirect_uri, &state, &challenge)?
    } else {
        oauth::authorization_url_with_meetings(&client.client_id, &redirect_uri, &state, &challenge, meeting_recordings)?
    };
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
    let pending = pending::take(home, state, user, session)?;
    crate::google_data_consent::require_current(Some(&pending.google_data_consent))?;
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
    let scopes = tokens
        .granted_scopes
        .ok_or("Google 沒有回傳實際授權範圍，請重新連結。")?;
    if pending.expected_connection.as_ref().is_some_and(|old| old.google_sub != info.sub) {
        return Err("請使用原本連結的 Google 帳號開放額外服務。這次沒有取代既有連線。".into());
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
    let purpose = if pending.drive_tools { "drive_tools" }
        else if pending.meeting_recordings { "meeting_recordings" } else { "base" };
    let mut local_purposes = pending.expected_connection.as_ref()
        .map(|old| old.local_purposes.clone()).unwrap_or_default();
    if !local_purposes.iter().any(|p| p == purpose) {
        local_purposes.push(purpose.into());
    }
    local_purposes.sort();
    local_purposes.dedup();
    let mut expected_scopes = oauth::requested_scopes(pending.meeting_recordings, pending.drive_tools)
        .into_iter().map(str::to_string).collect::<Vec<_>>();
    // Previously returned but locally unconsented scopes remain unexpected
    // on later flows; receiving them once must not bless them in the audit.
    for granted_purpose in &local_purposes {
        expected_scopes.extend(oauth::requested_scopes(
            granted_purpose == "meeting_recordings", granted_purpose == "drive_tools",
        ).into_iter().map(str::to_string));
    }
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
    vault::connect_with_previous(home, &key, metadata.clone(), &tokens.secret, pending.epoch, pending.expected_connection.as_ref())?;
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
