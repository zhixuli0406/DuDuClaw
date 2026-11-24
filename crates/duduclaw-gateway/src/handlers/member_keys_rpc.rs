//! Personal key management is gateway-owned and browser-session-only.
use super::*;
use duduclaw_auth::member_keys::MemberKeyError;

pub(super) fn owns(method: &str) -> bool {
    method.split('.').next() == Some("member_keys")
        || method.split('.').take(2).eq(["me", "member_keys"])
}
fn coded(code: &str, message: &str) -> WsFrame {
    WsFrame::Response { id: String::new(), ok: false, payload: None,
        error: Some(json!({"code": code, "message": message})) }
}
fn error(e: MemberKeyError) -> WsFrame {
    coded(e.code(), match e {
        MemberKeyError::InvalidName => "名稱必須是 1 到 60 個字，不能包含控制字元。",
        MemberKeyError::InvalidExpiry => "有效期限必須是 1 到 365 天。",
        MemberKeyError::AdminForbidden => "管理員帳號不能建立本人金鑰，請使用一般成員帳號。",
        MemberKeyError::LimitReached => "每位成員最多保有 5 把有效金鑰，請先撤銷舊金鑰。",
        MemberKeyError::NotFound => "找不到這把金鑰。",
        _ => "目前無法處理本人金鑰，請重新登入後再試。",
    })
}
impl MethodHandler {
    pub(crate) async fn handle_member_keys_rpc(&self, method: &str, params: Value, ctx: &UserContext) -> WsFrame {
        if !crate::member_keys::config(&self.home_dir).enabled {
            return coded("member_keys_disabled", "管理員尚未開啟本人金鑰。");
        }
        if ctx.member_key_id.is_some() || ctx.user_id == "system" {
            return coded("member_keys_browser_required", "請使用瀏覽器登入帳號後管理金鑰。");
        }
        let session = match crate::google_personal::session_for(&ctx.user_id) {
            Ok(session) => session,
            Err(_) => return coded("member_keys_browser_required", "請使用瀏覽器登入帳號後管理金鑰。"),
        };
        let Some(db) = self.user_db.read().await.clone() else { return error(MemberKeyError::Store); };
        let user = ctx.user_id.clone();
        let epoch = ctx.credential_epoch;
        let home = self.home_dir.clone();
        let method = method.to_owned();
        let result = tokio::task::spawn_blocking(move || -> Result<Value, WsFrame> {
            if !crate::member_keys::config(&home).enabled { return Err(coded("member_keys_disabled", "本人金鑰已關閉。")); }
            let live = db.live_user_context(&user).map_err(|_| error(MemberKeyError::Store))?
                .filter(|u| u.credential_epoch == epoch && !u.must_change_password)
                .ok_or_else(|| error(MemberKeyError::AccountInactive))?;
            if !db.remember_session_valid(Some(&session), &user, Utc::now().timestamp())
                .map_err(|_| error(MemberKeyError::Store))? {
                return Err(error(MemberKeyError::AccountInactive));
            }
            let id = || params.get("id").and_then(Value::as_str)
                .filter(|id| !id.is_empty() && id.len() <= 64)
                .ok_or_else(|| error(MemberKeyError::NotFound));
            match method.as_str() {
                "me.member_keys.list" => Ok(json!({"keys": db.member_api_keys_for_user(&user).map_err(error)?, "max_active": 5})),
                "me.member_keys.create" => {
                    if live.is_admin() { return Err(error(MemberKeyError::AdminForbidden)); }
                    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                    let days = params.get("expires_in_days").and_then(Value::as_u64)
                        .filter(|days| (1..=365).contains(days)).ok_or_else(|| error(MemberKeyError::InvalidExpiry))?;
                    if db.mfa_active(&user).map_err(|_| error(MemberKeyError::Store))? {
                        let code = params.get("code").and_then(Value::as_str).unwrap_or("");
                        db.mfa_step_up_verify(&user, code, &crate::mfa::Cipher(&home), Utc::now().timestamp())
                            .map_err(|_| coded("member_key_step_up_required", "請輸入新的二次驗證碼或備援碼。"))?;
                    }
                    let created = db.member_key_create(&user, name, days, epoch).map_err(error)?;
                    Ok(json!({"key": created.info, "secret": created.secret}))
                }
                "me.member_keys.revoke" => {
                    db.member_key_revoke_own(&user, id()?).map_err(error)?;
                    Ok(json!({"revoked": true}))
                }
                "member_keys.admin_list" if live.is_admin() =>
                    Ok(json!({"keys": db.member_api_keys_all(&user).map_err(error)?})),
                "member_keys.admin_revoke" if live.is_admin() => {
                    db.member_key_admin_revoke(&user, id()?).map_err(error)?;
                    Ok(json!({"revoked": true}))
                }
                _ => Err(coded("member_key_forbidden", "不允許此操作。")),
            }
        }).await;
        match result {
            Ok(Ok(value)) => WsFrame::ok_response("", value),
            Ok(Err(frame)) => frame,
            Err(_) => error(MemberKeyError::Store),
        }
    }

    pub(super) async fn member_key_gate(&self, method: &str, params: &Value, ctx: &UserContext) -> Result<(), WsFrame> {
        if ctx.member_key_id.is_none() { return Ok(()); }
        let kind = super::member_contract::member_key_method(method);
        let excluded = kind.is_none() || params.get("step_up_csrf").is_some();
        let db = self.user_db.read().await.clone().ok_or_else(|| error(MemberKeyError::Store))?;
        let home = self.home_dir.clone();
        let ctx = ctx.clone();
        let method = method.to_owned();
        tokio::task::spawn_blocking(move || crate::member_keys::record_call(&home, &db, &ctx, &method,
            kind == Some(super::member_contract::MemberContractKind::Write))).await
            .map_err(|_| error(MemberKeyError::Store))?
            .map_err(|e| coded(if e == "member_key_rate_limited" { "member_key_rate_limited" } else { "member_key_refused" }, &e))?;
        if excluded { return Err(coded("member_key_forbidden", "此方法不開放給本人金鑰。")); }
        Ok(())
    }
}
