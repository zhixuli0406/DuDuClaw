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
    let plan = crate::remote_mcp::connect::plan_redirect(origin, allowed)?;
    let (dashboard_origin, paste) = match &plan {
        crate::remote_mcp::connect::RedirectPlan::Direct { origin, .. } => (origin.clone(), false),
        crate::remote_mcp::connect::RedirectPlan::Paste { origin, .. } => (origin.clone(), true),
    };
    let mut redirect =
        url::Url::parse(plan.redirect_uri()).map_err(|_| "無法建立 Google 回呼網址。")?;
    redirect.set_path(CALLBACK_PATH);
    let redirect_uri = redirect.to_string();
    let verifier = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let epoch = vault::begin_connection(home, user)?;
    let google_data_consent = crate::google_data_consent::Evidence::issue(user)?;
    pending::cancel_user(home, user);
    let state = pending::insert(pending::Pending {
        home: home.into(),
        user_id: user.into(),
        session_ref: session.into(),
        redirect_uri: redirect_uri.clone(),
        dashboard_origin,
        paste,
        code_verifier: verifier,
        client_id: client.client_id.clone(),
        client_secret: client.client_secret,
        epoch,
        google_data_consent,
        created: Instant::now(),
        code: None,
    });
    let authorization_url =
        oauth::authorization_url(&client.client_id, &redirect_uri, &state, &challenge)?;
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
    vault::connect(home, &key, metadata.clone(), &tokens.secret, pending.epoch)?;
    let detail = json!({
        "google_email": metadata.account_email,
        "scope_count": metadata.granted_scopes.len(),
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
