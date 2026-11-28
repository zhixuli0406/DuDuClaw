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
        MemberKeyError::InvalidScopes => "選取的使用範圍無效，請重新選擇。",
        MemberKeyError::CannotWiden => "這把金鑰只能縮小使用範圍；若要增加範圍，請撤銷後重新建立。",
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
            let config = crate::member_keys::config(&home);
            if !config.enabled { return Err(coded("member_keys_disabled", "本人金鑰已關閉。")); }
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
                        "policy": {"allow_conversation_content": config.allow_conversation_content, "allow_mcp_scopes": config.allow_mcp_scopes},
                        "max_total": 50, "reauthentication": if db.mfa_active(&user).map_err(|_| error(MemberKeyError::Store))? { "mfa" } else { "password" }}))
                },
                "me.member_keys.create" => {
                    if live.is_admin() { return Err(error(MemberKeyError::AdminForbidden)); }
                    let (include, scopes) = scope_params(&params)?;
                    let include = include.unwrap_or(false);
                    let scopes = scopes.unwrap_or_default();
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
                    // Reauthentication can be slow. Validate the creation policy after it,
                    // immediately before the authenticated insert; parse/read errors disable it.
                    let config = crate::member_keys::config(&home);
                    if !config.enabled { return Err(coded("member_keys_disabled", "本人金鑰已關閉。")); }
                    if include && !config.allow_conversation_content {
                        return Err(coded("member_key_conversation_not_allowed", "管理員尚未開放包含我的對話內容。"));
                    }
                    if scopes.iter().any(|scope| !config.allow_mcp_scopes.contains(scope)) {
                        return Err(coded("member_key_scope_not_allowed", "管理員尚未開放選取的外部 AI 工具使用範圍。"));
                    }
                    let created = db.member_key_create_authenticated(&user, name, days, epoch, &session, &proof, include, &scopes).map_err(error)?;
                    Ok(json!({"key": created.info, "secret": created.secret}))
                }
                "me.member_keys.revoke" => {
                    db.member_key_revoke_own(&user, id()?).map_err(error)?;
                    Ok(json!({"revoked": true}))
                }
                "me.member_keys.narrow" => {
                    let (include, scopes) = scope_params(&params)?;
                    db.member_key_narrow_own(&user, id()?, include, scopes.as_deref()).map_err(error)?;
                    Ok(json!({"narrowed": true}))
                }
                "member_keys.admin_narrow" if live.is_admin() => {
                    let (include, scopes) = scope_params(&params)?;
                    db.member_key_narrow_admin(&user, id()?, include, scopes.as_deref()).map_err(error)?;
                    Ok(json!({"narrowed": true}))
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

    pub(super) async fn member_key_memory_gate(&self, ctx: &UserContext, agent: &str) -> Result<(), WsFrame> {
        if ctx.member_key_id.is_none() { return Ok(()); }
        let denied = || coded("member_key_memory_scope", "這位 AI 員工的記憶包含其他人的對話，本人金鑰不能讀取。");
        let db = self.user_db.read().await.clone().ok_or_else(denied)?;
        let home = self.home_dir.clone();
        let user = ctx.user_id.clone();
        let agent = agent.to_owned();
        let allowed = tokio::task::spawn_blocking(move || -> Result<bool, ()> {
            if agent.is_empty() || crate::personal_visibility::canonical_agent(&agent).is_err() { return Ok(false); }
            let avatar = db.avatar_agent(&user).map_err(|_| ())?;
            if avatar.as_deref() == Some(agent.as_str()) { return Ok(true); }
            if !crate::runtime_readiness::isolation_active() { return Ok(false); }
            crate::personal_visibility::personal_owner(&home, &agent)
                .map(|owner| owner.as_deref() == Some(user.as_str())).map_err(|_| ())
        }).await.map_err(|_| denied())?.map_err(|_| denied())?;
        if allowed { Ok(()) } else { Err(denied()) }
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
        let conversation_method = super::member_contract::MEMBER_KEY_CONVERSATION_METHODS.contains(&method.as_str());
        let include = tokio::task::spawn_blocking(move || -> Result<bool, String> {
            crate::member_keys::record_call(&home, &db, &ctx, &method,
                kind == Some(super::member_contract::MemberContractKind::Write))?;
            // Re-read after audit/rate charging. Never trust the socket's snapshot.
            crate::member_keys::refresh_authority(&home, &db, &ctx).map(|live| live.include_conversations)
        }).await
            .map_err(|_| error(MemberKeyError::Store))?
            .map_err(|e| coded(if e == "member_key_rate_limited" { "member_key_rate_limited" } else { "member_key_refused" },
                if e == "member_key_rate_limited" { "這把金鑰的使用次數過多，請稍後再試。" } else { "這把金鑰目前無法使用，請重新確認。" }))?;
        if conversation_method && !include {
            return Err(coded("member_key_conversation_scope", "這把金鑰沒有開啟『包含我的對話內容』。"));
        }
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

/// Missing means unchanged/default; malformed or unknown values always refuse.
fn scope_params(params: &Value) -> Result<(Option<bool>, Option<Vec<String>>), WsFrame> {
    let include = params.get("include_conversations").map(|value| value.as_bool()
        .ok_or_else(|| coded("member_key_invalid_conversations", "請選擇是否包含我的對話內容。"))).transpose()?;
    let scopes = params.get("mcp_scopes").map(|value| -> Result<Vec<String>, WsFrame> {
        let values = value.as_array().ok_or_else(|| error(MemberKeyError::InvalidScopes))?;
        let mut scopes = Vec::new();
        for value in values {
            let scope = value.as_str().filter(|scope| duduclaw_core::mcp_scopes::PERSON_KEY_MCP_SCOPE_STRINGS.contains(scope))
                .ok_or_else(|| error(MemberKeyError::InvalidScopes))?;
            scopes.push(scope.to_owned());
        }
        scopes.sort();
        scopes.dedup();
        Ok(scopes)
    }).transpose()?;
    Ok((include, scopes))
}
