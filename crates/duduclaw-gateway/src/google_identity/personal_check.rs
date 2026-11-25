//! Administrator diagnostics: local records and OAuth metadata only.
use std::path::Path;
use duduclaw_core::google_identity::{self as gi, Effective};
use crate::{google_personal::{access, oauth, scopes}, person_credentials as vault};
use super::check::Step;

pub(crate) async fn run(home: &Path, agent: &str, db: &duduclaw_auth::UserDb, user_id: &str) -> Vec<Step> {
    let mut out = Vec::new();
    let mut add = |id: &str, ok: bool, detail: String| out.push(Step {
        id: id.into(), label: "個人 Google 連線", ok, detail,
    });
    let result: Result<(), String> = async {
        let selection = gi::effective_for(home, Some(agent));
        let owner_generation = db.google_primary_owner_generation(agent)?;
        let validate = || -> Result<(), String> {
            if db.google_primary_owner_generation(agent)? != owner_generation || gi::effective_for(home, Some(agent)) != selection { return Err("身分設定已變更，請重新測試。".into()); }
            let Effective::Personal { selector } = &selection.effective else { return Err("不是個人連線。".into()); };
            let user = access::select_user(db, agent, selector).map_err(|r| r.message_zh)?;
            if user.id != user_id || user.status != duduclaw_auth::UserStatus::Active {
                return Err("受測成員或負責人狀態已變更，已停止測試。".into());
            }
            Ok(())
        };
        let lock = access::refresh_lock(home, user_id).map_err(|r| r.message_zh)?;
        let _guard = lock.lock().await;
        validate()?;
        let record = vault::get(home, user_id)?.ok_or("本人尚未連結 Google。")?;
        add("connection", record.metadata.status == vault::Status::Connected, format!("連線帳號：{}", record.metadata.account_email));
        let consent = db.google_consent(user_id, agent)?;
        add("consent", consent == Some(true), match consent {
            Some(true) => "本人已允許這位 AI 員工使用連線。",
            Some(false) => "本人已拒絕這位 AI 員工使用連線。",
            None => "尚無本人同意紀錄。",
        }.into());
        let covered: Vec<_> = scopes::TOKEN_TOOLS.iter().copied().filter(|tool| {
            scopes::permitted_for(tool, &record.metadata)
                && (*tool != "google_chat_send" || crate::google_workspace::chat_send_enabled(home))
                && (!matches!(*tool, "google_chat_spaces" | "google_chat_messages") || crate::google_workspace::chat_read_enabled(home))
        }).collect();
        add("tools", !covered.is_empty(), format!("已授權範圍與用途涵蓋的工具：{}", covered.join(", ")));
        if record.metadata.status != vault::Status::Connected { return Err("連線需要本人重新連結。".into()); }
        crate::google_data_consent::require_current(record.metadata.google_data_consent.as_ref())
            .map_err(|_| "請本人重新確認 Google 資料存取說明。")?;
        let config = crate::mcp_oauth::get_client_config(home, "google").ok_or("管理員尚未設定 Google OAuth。")?;
        if config.client_id != record.metadata.oauth_client_id { return Err("OAuth 用戶端已變更，請本人重新連結。".into()); }
        if !access::domain_allowed(home, &record.metadata)? { return Err("Google 帳號不在允許的網域內。".into()); }
        let key = super::protection_gate_async(home).await.map_err(|r| r.message_zh)?;
        validate()?;
        let secret = vault::unseal(&key, &record)?;
        let refresh = secret.refresh_token.as_deref().ok_or("缺少 refresh token，請本人重新連結。")?;
        let live_record = || -> Result<(), String> {
            validate()?;
            let now = vault::get(home, user_id)?.ok_or("連線已移除。")?;
            if now.metadata.credential_id != record.metadata.credential_id || now.metadata.revision != record.metadata.revision
                || now.metadata.status != vault::Status::Connected {
                return Err("連線已變更，請重新測試。".into());
            }
            Ok(())
        };
        live_record()?;
        let tokens = oauth::refresh(&config.client_id, &config.client_secret, refresh).await
            .map_err(|_| "Google 無法更新連線，請本人重新連結或稍後再試。")?;
        live_record()?;
        add("refresh", true, "OAuth refresh 成功；未讀取任何 Google 資料。".into());
        let granted = oauth::tokeninfo(&tokens.secret.access_token, &config.client_id).await
            .map_err(|_| "無法確認 tokeninfo 權限或用戶端。")?;
        live_record()?;
        let missing: Vec<_> = record.metadata.granted_scopes.iter().filter(|scope| !granted.contains(scope)).cloned().collect();
        add("tokeninfo", missing.is_empty(), if missing.is_empty() { "tokeninfo 已確認記錄的權限範圍。".into() }
            else { format!("Google 未回報的權限：{}", missing.join(", ")) });
        Ok(())
    }.await;
    if let Err(message) = result { add("check_failed", false, message); }
    out
}
