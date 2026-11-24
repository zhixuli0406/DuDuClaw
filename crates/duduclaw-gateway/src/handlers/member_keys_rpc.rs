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
        MemberKeyError::HistoryLimitReached => "金鑰保留紀錄已達 50 筆，請等候撤銷紀錄超過 90 天後再建立。",
        MemberKeyError::InvalidPage => "分頁參數無效，請重新整理清單。",
        MemberKeyError::ReauthenticationRequired => "請重新輸入目前密碼或新的二次驗證碼。",
        MemberKeyError::NotFound => "找不到這把金鑰。",
        _ => "目前無法處理本人金鑰，請重新登入後再試。",
    })
}
impl MethodHandler {
    pub(crate) async fn handle_member_keys_rpc(&self, method: &str, params: Value, ctx: &UserContext, conn: crate::power_local::RpcConnInfo) -> WsFrame {
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
                "me.member_keys.list" => {
                    let (limit, cursor) = pagination(&params)?;
                    let page = db.member_api_keys_for_user_page(&user, limit, cursor).map_err(error)?;
                    Ok(json!({"keys": page.keys, "next_cursor": page.next_cursor, "max_active": 5,
                        "max_total": 50, "reauthentication": if db.mfa_active(&user).map_err(|_| error(MemberKeyError::Store))? { "mfa" } else { "password" }}))
                },
                "me.member_keys.create" => {
                    if live.is_admin() { return Err(error(MemberKeyError::AdminForbidden)); }
                    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                    let days = params.get("expires_in_days").and_then(Value::as_u64)
                        .filter(|days| (1..=365).contains(days)).ok_or_else(|| error(MemberKeyError::InvalidExpiry))?;
                    // The peer comes from the socket, never from request parameters.
                    let peer = conn.peer.ok_or_else(|| coded("member_keys_browser_required", "請使用瀏覽器登入帳號後管理金鑰。"))?;
                    if !crate::server::check_login_rate_limit(peer.ip(), &live.email) {
                        return Err(coded("member_key_reauth_rate_limited", "嘗試次數過多，請於 15 分鐘後再試。"));
                    }
                    let password = params.get("password").and_then(Value::as_str).unwrap_or("");
                    let code = params.get("code").and_then(Value::as_str).unwrap_or("");
                    let proof = db.member_key_reauthenticate(&user, &session, epoch, password, code,
                        &crate::mfa::Cipher(&home), Utc::now().timestamp()).map_err(error)?;
                    let created = db.member_key_create_authenticated(&user, name, days, epoch, &session, &proof).map_err(error)?;
                    Ok(json!({"key": created.info, "secret": created.secret}))
                }
                "me.member_keys.revoke" => {
                    db.member_key_revoke_own(&user, id()?).map_err(error)?;
                    Ok(json!({"revoked": true}))
                }
                "member_keys.admin_list" if live.is_admin() => {
                    let (limit, cursor) = pagination(&params)?;
                    let page = db.member_api_keys_all_page(&user, limit, cursor).map_err(error)?;
                    Ok(json!({"keys": page.keys, "next_cursor": page.next_cursor}))
                },
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
        let field_error = if method == "agents.update" {
            super::member_contract::member_key_agent_update_params(params).err()
        } else { None };
        let db = self.user_db.read().await.clone().ok_or_else(|| error(MemberKeyError::Store))?;
        let home = self.home_dir.clone();
        let ctx = ctx.clone();
        let method = method.to_owned();
        tokio::task::spawn_blocking(move || crate::member_keys::record_call(&home, &db, &ctx, &method,
            kind == Some(super::member_contract::MemberContractKind::Write))).await
            .map_err(|_| error(MemberKeyError::Store))?
            .map_err(|e| coded(if e == "member_key_rate_limited" { "member_key_rate_limited" } else { "member_key_refused" }, &e))?;
        if let Some(message) = field_error { return Err(coded("member_key_forbidden_field", &message)); }
        if excluded { return Err(coded("member_key_forbidden", "此方法不開放給本人金鑰。")); }
        Ok(())
    }
}

fn pagination(params: &Value) -> Result<(u32, Option<&str>), WsFrame> {
    let limit = match params.get("limit") {
        None => 25,
        Some(value) => value.as_u64().filter(|v| (1..=100).contains(v))
            .ok_or_else(|| error(MemberKeyError::InvalidPage))? as u32,
    };
    let cursor = match params.get("cursor") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => Some(value.as_str()),
        _ => return Err(error(MemberKeyError::InvalidPage)),
    };
    Ok((limit, cursor))
}
