//! Dashboard-only administration of company Google sign-in (RM-214 P9).
//! Caller identity comes from the verified session; all storage runs on the
//! blocking pool. Authentication namespaces never reach extension handlers.
use super::*;
use crate::google_signin::{config::{self, Approval, AvatarTemplate, GoogleSigninSettings, SettingsUpdate}, routes};
use serde::Deserialize;

pub(super) fn signin_error(code: &str) -> WsFrame {
    let message = match code {
        "signin_unavailable" => "公司 Google 登入目前無法使用，請聯絡管理員。",
        "signin_session_expired" => "這次驗證已失效，請重新登入後再試。",
        "signin_password_invalid" => "密碼不正確，請重新輸入。",
        "signin_link_email_mismatch" => "這個 Google 帳號的信箱和你的帳號信箱不同，無法綁定。請改用相同信箱的公司帳號，或請管理員協助。",
        "signin_link_needs_password" => "請先設定本機密碼，再綁定 Google 登入。",
        "signin_unlink_needs_password" => "這個帳號沒有本機密碼，取消 Google 登入會無法登入，請先聯絡管理員。",
        "signin_already_linked" => "這個帳號已綁定 Google 登入。",
        "signin_personal_sub_mismatch" => "請使用與個人 Google 連線相同的 Google 帳號。",
        "signin_reauth_password_account" => "這個帳號有本機密碼，請使用密碼驗證身分。",
        "signin_reauth_sub_mismatch" => "請使用原本綁定的公司 Google 帳號驗證身分。",
        "signin_reauth_unavailable" => "Google 未提供近期身分驗證紀錄，請聯絡管理員重設兩步驟驗證。",
        "signin_personal_account" => "請使用公司 Google 帳號，不能使用個人帳號。",
        "signin_domain_not_allowed" => "這個 Google 帳號不在公司允許的網域內。",
        "signin_rate_limited" => "嘗試次數過多，請稍後再試。",
        "signin_admin_required" => "請以管理員帳號重新登入後再操作。",
        "signin_dashboard_required" => "請登入儀表板後再操作，不能使用 API 金鑰。",
        "signin_step_up_required" => "請先以密碼與驗證器驗證碼確認身分，並在五分鐘內完成操作。",
        "signin_request_not_found" => "找不到這筆登入申請，請重新整理清單。",
        "signin_request_not_pending" => "這筆登入申請已經處理，請重新整理清單。",
        "signin_request_expired" => "這筆登入申請已過期，請同仁重新申請。",
        "signin_email_exists" => "這個信箱已有帳號，請先確認帳號歸屬。",
        "signin_sub_linked" => "這個 Google 帳號已綁定其他帳號。",
        "signin_attest_invalid_target" => "請選擇一百個以內、不重複且仍啟用的帳號；已綁定 Google 的帳號不能再次確認。",
        "signin_not_linked" => "這個帳號尚未綁定 Google 登入。",
        "signin_avatar_not_waiting" => "這個帳號目前不需要管理員重試建立分身。",
        "role_requires_local_password" => "這個帳號只用公司 Google 帳號登入，沒有本機密碼，不能設為管理員或主管。",
        "signin_preauthorize_disabled" => "尚未開啟核准時允許註冊兩步驟驗證，請先調整 Google 登入設定。",
        "signin_personal_edition" => "個人版不提供公司 Google 帳號登入。",
        "signin_invalid_params" => "填寫的資料不完整或格式不正確，請確認後再試。",
        "signin_unknown_method" => "不支援這項 Google 登入操作。",
        "signin_audit_failed_after_save" => "設定已儲存，但無法完整記錄這次變更，請聯絡系統管理員確認。",
        _ => "目前無法讀取或儲存登入資料，請稍後再試。",
    };
    WsFrame::Response { id: String::new(), ok: false, payload: None,
        error: Some(json!({"code": code, "message": message})) }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetRequest {
    expected_hash: String,
    enabled: bool,
    allowed_hd: Vec<String>,
    approval: String,
    daily_new_accounts: u32,
    avatar_template: String,
    avatar_template_revision: Option<u64>,
    approval_grants_mfa_enrollment: bool,
    #[serde(default)]
    acknowledge_risk: bool,
    step_up_csrf: Option<[u8; 32]>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApproveRequest {
    id: String,
    #[serde(default)]
    mfa_preauthorize: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestId { id: String }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UserId { user_id: String }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AttestRequest { user_ids: Vec<String>, step_up_csrf: Option<[u8; 32]> }

fn parse<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, WsFrame> {
    serde_json::from_value(params).map_err(|_| signin_error("signin_invalid_params"))
}
fn settings_value(s: &GoogleSigninSettings) -> Value {
    let (template, revision) = match &s.avatar_template {
        AvatarTemplate::Blank => (Some(String::new()), None),
        AvatarTemplate::Company { key, revision } => (Some(format!("company:{key}")), Some(*revision)),
        AvatarTemplate::Invalid => (None, None),
    };
    json!({"enabled": s.enabled, "enabled_requested": s.enabled_requested,
        "allowed_hd": s.allowed_hd, "approval": if s.approval == Approval::Direct { "direct" } else { "admin" },
        "daily_new_accounts": s.daily_new_accounts, "avatar_template": template, "avatar_template_revision": revision,
        "approval_grants_mfa_enrollment": s.approval_grants_mfa_enrollment,
        "ack": s.ack.as_ref().map(|(by, at)| json!({"by": by, "at": at}))})
}
fn settings_get(home: &Path) -> Result<Value, WsFrame> {
    let (s, hash) = config::read_with_hash(home).map_err(|message| WsFrame::error_response("", &message))?;
    let base = routes::public_base(home);
    // This is the exact domain policy used by google_personal::access.
    // A malformed policy is a failed check, never a permissive fallback.
    let covers = Some(duduclaw_core::google_identity::read_home_config(home).is_ok_and(|c|
        c.allowed_domains.is_empty() || s.allowed_hd.iter().all(|hd| c.allowed_domains.contains(hd))));
    let mfa = match crate::mfa::enforced(home, false) {
        Ok(true) => "enforced", Ok(false) => "not_enforced", Err(_) => "unknown",
    };
    Ok(json!({"settings": settings_value(&s), "hash": hash,
        "redirect_uri": base.as_ref().map(|(base, _)| routes::redirect_uri(base)),
        "checks": {"client_configured": routes::configured_client(home).is_some(),
            "public_url_ok": base.is_some(), "domains_set": !s.allowed_hd.is_empty(),
            "mfa_policy": mfa, "personal_edition": false, "personal_connection_domains_cover": covers}}))
}

fn step_up(db: &UserDb, ctx: &UserContext, session: &str, csrf: Option<[u8; 32]>) -> Result<(), WsFrame> {
    let csrf = csrf.ok_or_else(|| signin_error("signin_step_up_required"))?;
    super::google_tenant_settings_rpc::check_step_up(db, &ctx.user_id, session, ctx.credential_epoch, &csrf)
        .map_err(|_| signin_error("signin_step_up_required"))
}

fn is_pure_disable(home: &Path, update: &SettingsUpdate) -> bool {
    let current = config::read_strict(home);
    let allowed_hd = update.allowed_hd.iter().try_fold(Vec::new(), |mut domains, domain| {
        let domain = crate::google_signin::oidc::normalize_allowed_hd(domain)?;
        if !domains.contains(&domain) { domains.push(domain); }
        Some(domains)
    });
    !update.enabled
        && allowed_hd.as_ref() == Some(&current.allowed_hd)
        && update.approval == current.approval
        && update.daily_new_accounts == current.daily_new_accounts
        && update.avatar_template == current.avatar_template
        && update.approval_grants_mfa_enrollment == current.approval_grants_mfa_enrollment
}

fn apply_settings(
    home: &Path, actor_user_id: &str, update: &SettingsUpdate, pure_disable: bool,
    acknowledge_risk: bool, now_rfc3339: &str, expected_hash: &str,
) -> Result<(GoogleSigninSettings, GoogleSigninSettings, String), String> {
    if pure_disable {
        // The exemption may have been decided before another admin's update.
        // Never write request settings through the unauthenticated branch.
        config::disable_settings(home, actor_user_id, now_rfc3339, expected_hash)
    } else {
        config::write_settings(home, actor_user_id, update, acknowledge_risk, now_rfc3339, expected_hash)
    }
}

impl MethodHandler {
    pub(crate) async fn handle_google_signin_rpc(&self, method: &str, params: Value, ctx: &UserContext) -> WsFrame {
        if ctx.role != UserRole::Admin || ctx.must_change_password {
            return signin_error("signin_admin_required");
        }
        if ctx.member_key_id.is_some() || ctx.user_id.is_empty() || ctx.user_id == "system" {
            return signin_error("signin_dashboard_required");
        }
        let session = match crate::google_personal::session_for(&ctx.user_id) {
            Ok(session) => session, Err(_) => return signin_error("signin_dashboard_required"),
        };
        let personal = self.resolve_edition_profile().await.is_personal();
        if personal { return signin_error("signin_personal_edition"); }
        let Some(db) = self.user_db.read().await.clone() else { return signin_error("signin_storage_unavailable"); };
        let (home, ctx, method) = (self.home_dir.clone(), ctx.clone(), method.to_owned());
        let cache = self.live_ctx_cache.clone();
        let result = tokio::task::spawn_blocking(move || -> Result<Value, WsFrame> {
            if !super::user_invites_rpc::live_admin(&db, &ctx) { return Err(signin_error("signin_admin_required")); }
            let now = chrono::Utc::now();
            match method.as_str() {
                "google.signin.get" => {
                    let _: Empty = parse(params)?;
                    settings_get(&home)
                }
                "google.signin.set" => {
                    if params.get("expected_hash").and_then(Value::as_str).is_none() {
                        return Err(WsFrame::error_response("", "設定頁的資料已過期，請重新整理後再儲存。"));
                    }
                    let input: SetRequest = parse(params)?;
                    let approval = match input.approval.as_str() {
                        "admin" => Approval::Admin, "direct" => Approval::Direct,
                        _ => return Err(signin_error("signin_invalid_params")),
                    };
                    let template = if input.avatar_template.is_empty() { AvatarTemplate::Blank }
                    else if let (Some(key), Some(revision)) = (input.avatar_template.strip_prefix("company:"), input.avatar_template_revision) {
                        AvatarTemplate::Company { key: key.into(), revision }
                    } else { return Err(signin_error("signin_invalid_params")); };
                    let update = SettingsUpdate { enabled: input.enabled, allowed_hd: input.allowed_hd, approval,
                        daily_new_accounts: input.daily_new_accounts, avatar_template: template,
                        approval_grants_mfa_enrollment: input.approval_grants_mfa_enrollment };
                    let pure_disable = is_pure_disable(&home, &update);
                    if !pure_disable { step_up(&db, &ctx, &session, input.step_up_csrf)?; }
                    // Refuse before touching config if the audit store is unavailable.
                    db.log_action(Some(&ctx.user_id), "google_signin.settings_change_requested", None, None, None)
                        .map_err(|_| signin_error("signin_storage_unavailable"))?;
                    let (before, after, hash) = apply_settings(&home, &ctx.user_id, &update, pure_disable, input.acknowledge_risk, &now.to_rfc3339(), &input.expected_hash)
                        .map_err(|message| WsFrame::error_response("", &message))?;
                    let detail = json!({"before": settings_value(&before), "after": settings_value(&after)}).to_string();
                    duduclaw_security::audit::log_config_changed(&home, &ctx.email, "admin", &[detail.clone()]);
                    crate::security_autopilot::emit_config_changed();
                    db.log_action(Some(&ctx.user_id), "google_signin.settings_changed", None, Some(&detail), None)
                        .map_err(|_| signin_error("signin_audit_failed_after_save"))?;
                    Ok(json!({"settings": settings_value(&after), "hash": hash}))
                }
                "users.signin_requests.list" => {
                    let _: Empty = parse(params)?;
                    let rows = db.list_pending_signin_requests().map_err(|_| signin_error("signin_storage_unavailable"))?;
                    let rows: Vec<_> = rows.into_iter().map(|r| json!({"id": r.id, "email": r.email,
                        "display_name": r.display_name, "hd": r.hd, "requested_at": r.requested_at, "expires_at": r.expires_at})).collect();
                    Ok(json!({"requests": rows}))
                }
                "users.signin_requests.approve" => {
                    let input: ApproveRequest = parse(params)?;
                    if input.mfa_preauthorize {
                        let settings = config::read_strict(&home);
                        if !settings.enabled || !settings.approval_grants_mfa_enrollment {
                            return Err(signin_error("signin_preauthorize_disabled"));
                        }
                    }
                    let user_id = db.approve_signin_request(&ctx.user_id, ctx.credential_epoch, &input.id, input.mfa_preauthorize, now.timestamp())
                        .map_err(signin_error)?;
                    Ok(json!({"user_id": user_id}))
                }
                "users.signin_requests.reject" => {
                    let input: RequestId = parse(params)?;
                    db.reject_signin_request(&ctx.user_id, ctx.credential_epoch, &input.id, now.timestamp()).map_err(signin_error)?;
                    Ok(json!({"rejected": true}))
                }
                "users.google_signin.attest" => {
                    let input: AttestRequest = parse(params)?;
                    step_up(&db, &ctx, &session, input.step_up_csrf)?;
                    let count = db.attest_google_signin_users(&ctx.user_id, ctx.credential_epoch, &input.user_ids, now.timestamp()).map_err(signin_error)?;
                    Ok(json!({"attested": count}))
                }
                "users.google_signin.unlink" => {
                    let input: UserId = parse(params)?;
                    db.unlink_google_signin(&ctx.user_id, ctx.credential_epoch, &input.user_id, now.timestamp()).map_err(signin_error)?;
                    // Do this inside the job even if the request future is cancelled.
                    cache.invalidate_user(&input.user_id);
                    Ok(json!({"unlinked": true}))
                }
                "users.signin_onboarding.retry_avatar" => {
                    let input: UserId = parse(params)?;
                    db.signin_avatar_admin_retry(&input.user_id).map_err(|code| signin_error(if code == "signin_avatar_not_waiting" {
                        "signin_avatar_not_waiting"
                    } else { "signin_storage_unavailable" }))?;
                    Ok(json!({"queued": true}))
                }
                _ => Err(signin_error("signin_unknown_method")),
            }
        }).await;
        match result {
            Ok(Ok(value)) => WsFrame::ok_response("", value),
            Ok(Err(frame)) => frame,
            Err(_) => signin_error("signin_storage_unavailable"),
        }
    }
}

#[cfg(test)]
#[path = "google_signin_rpc_tests.rs"]
mod tests;
