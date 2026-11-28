//! Admin consent for the company OAuth account. Ordinary Home material is
//! observed only; all new grants and disconnect epochs belong to write_store.
use super::{flow, oauth, pending};
use crate::google_identity::home_source;
use crate::person_credentials::{write_store, Status};
use duduclaw_auth::{User, UserDb, UserRole};
use serde_json::{json, Value};
use std::path::Path;

pub(crate) fn require_admin(db: &UserDb, user: &str) -> Result<User, String> {
    let user = flow::active_user(db, user)?;
    if user.role != UserRole::Admin {
        return Err("只有管理員可以變更公司 Google 寫入連線。".into());
    }
    Ok(user)
}

pub(crate) async fn start(
    home: &Path, db: &UserDb, user: &str, session: &str, origin: &str,
    allowed: &[String], acknowledge_google_data: bool, acknowledge_write: bool,
) -> Result<Value, String> {
    require_admin(db, user)?;
    if !acknowledge_google_data || !acknowledge_write {
        return Err("請先確認 Google 資料說明與雲端硬碟寫入風險。".into());
    }
    flow::require_write_policy(home)?;
    if crate::google_workspace::selected_path(home).ok().as_deref() != Some("oauth") {
        return Err("公司 Google 帳號必須使用 OAuth 連線才能同意寫入。".into());
    }
    flow::start_consent(home, db, user, session, origin, allowed,
        true, false, false, false, true, None, true).await
}

fn domain_allowed(home: &Path, info: &oauth::UserInfo) -> Result<bool, String> {
    let config = duduclaw_core::google_identity::read_home_config(home)?;
    let domain = info.hd.as_deref()
        .or_else(|| duduclaw_core::google_identity::email_domain(&info.email));
    Ok(config.allowed_domains.is_empty()
        || domain.is_some_and(|d| config.allowed_domains.iter().any(|allowed| allowed == d)))
}

pub(super) async fn finish(
    home: &Path, db: &UserDb, user: &str, pending: pending::Pending,
) -> Result<Value, String> {
    require_admin(db, user)?;
    flow::require_write_policy(home)?;
    if !pending.drive_write || pending.expected_connection.is_some()
        || pending.meeting_recordings || pending.drive_tools || pending.chat_read || pending.chat_send {
        return Err("這次 Google 連結已失效，請重新開始。".into());
    }
    let expected = pending.expected_home_write.as_ref().ok_or("公司 Google 連線已變更。")?;
    let validate = || -> Result<(), String> {
        require_admin(db, user)?;
        flow::require_write_policy(home)?;
        home_source::validate_home_write_identity(home, expected)
            .map_err(|_| "公司 Google 連線已變更，請重新開始。")?;
        if crate::mcp_oauth::get_client_config(home, "google").is_none_or(|c|
            c.client_id != pending.client_id || c.client_secret != pending.client_secret) {
            return Err("管理員已變更 Google 連結設定，請重新開始。".into());
        }
        Ok(())
    };
    let _lease = super::grants::acquire(home).await?;
    validate()?;
    let tokens = oauth::exchange(&pending.client_id, &pending.client_secret,
        pending.code.as_deref().ok_or("尚未收到 Google 回覆。")?,
        &pending.code_verifier, &pending.redirect_uri).await
        .map_err(|_| "Google 連結未完成，沒有保存這次的憑證。")?;
    let info = oauth::userinfo(&tokens.secret.access_token).await
        .map_err(|_| "無法確認 Google 帳號或信箱尚未驗證，沒有保存連線。")?;
    // Both subjects come from verified Google userinfo, never caller input.
    if info.sub != expected.sub {
        return Err("請使用原本連結的公司 Google 帳號開放寫入，這次沒有保存。".into());
    }
    let scopes = tokens.granted_scopes.ok_or("Google 沒有回傳實際授權範圍。")?;
    if !scopes.iter().any(|scope| scope == oauth::DRIVE_WRITE_SCOPE) {
        return Err("Google 尚未授予雲端硬碟寫入權限，這次沒有保存。".into());
    }
    if tokens.secret.refresh_token.as_deref().is_none_or(str::is_empty) {
        return Err("Google 沒有提供背景更新憑證，請重新連結。".into());
    }
    let key = crate::google_identity::protection_gate_async(home).await.map_err(|r| r.message_zh)?;
    // Network/protection work is complete. Keep the grant lease while the
    // blocking commit takes source -> authority, matching config_commit.
    // The pending map and original UserDb mutexes are already released.
    let home = home.to_path_buf();
    let user = user.to_owned();
    let io_authority = duduclaw_mcp_boundary::io_authority::capture();
    tokio::task::spawn_blocking(move || {
        // Cancellation cannot release the grant while this worker commits.
        let _grant = _lease;
        io_authority.run(|| {
            let expected = pending.expected_home_write.as_ref().ok_or("公司 Google 連線已變更。")?;
            home_source::with_home_write_authority(&home, expected, || {
                // The source receipt rechecks the original client/material after
                // waiting. Open/read SQLite only after authority, with no await.
                let db = crate::decision_notify::open_user_db_read_only(&home)
                    .map_err(|_| "無法確認管理員身分，請稍後重試。")?
                    .ok_or("無法確認管理員身分，請稍後重試。")?;
                let admin = require_admin(&db, &user)?;
                flow::require_write_policy(&home)?;
                crate::google_data_consent::require_current(Some(&pending.google_data_consent))?;
                if pending.created.elapsed() >= pending::TTL {
                    return Err("這次 Google 連結已失效，請重新開始。".into());
                }
                if !domain_allowed(&home, &info)? {
                    return Err("這個 Google 帳號不在管理員允許的網域內，沒有保存連線。".into());
                }
                let metadata = write_store::Metadata {
                    owner_kind: "home".into(), owner_id: "home".into(), purpose: "drive_write".into(),
                    provider: "google".into(), credential_id: uuid::Uuid::new_v4().to_string(), revision: 1,
                    account_email: info.email, google_sub: info.sub, hosted_domain: info.hd,
                    granted_scopes: scopes, status: Status::Connected, status_before_suspension: None,
                    connected_at: chrono::Utc::now().to_rfc3339(), last_refresh_at: None,
                    oauth_client_id: pending.client_id.clone(),
                    google_data_consent: Some(pending.google_data_consent.clone()),
                };
                // The pending epoch is compared under the store file lock.
                write_store::connect(&home, &key, metadata.clone(), &tokens.secret, pending.epoch)?;
                duduclaw_security::audit::log_config_changed(&home, &admin.email, "admin",
                    &[format!("company Drive write consent connected: {}", metadata.account_email)]);
                Ok(json!({"connection": metadata}))
            })
        })
    }).await.map_err(|_| "無法保存公司 Google 寫入連線，請稍後重試。".to_owned())?
}

pub(crate) async fn disconnect(home: &Path, db: &UserDb, user: &str) -> Result<Value, String> {
    require_admin(db, user)?;
    let user = user.to_owned();
    write_store::mutate_home(home, move |home| {
        // Reopen and revalidate after authority; no DB mutex crosses the wait.
        let db = crate::decision_notify::open_user_db_read_only(home)
            .map_err(|_| "無法確認管理員身分，請稍後重試。")?
            .ok_or("無法確認管理員身分，請稍後重試。")?;
        let admin = require_admin(&db, &user)?;
        write_store::disconnect(home, "home", "home")?;
        duduclaw_security::audit::log_config_changed(home, &admin.email, "admin",
            &["company Drive write consent disconnected".into()]);
        Ok(json!({"disconnected": true,
            "provider_note": "只移除了這裡的寫入連線。若要完整移除授權，請到 Google 帳戶的第三方存取頁操作。"}))
    }).await
}
