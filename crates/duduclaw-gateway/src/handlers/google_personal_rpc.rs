//! Enterprise self-service connections. Every person id is derived from the
//! verified context, apart from the separately Admin-gated emergency removal.

use super::*;
use crate::google_personal::{self, access, flow, lifecycle, pending};
use duduclaw_core::google_identity::{self as gi, Effective};

#[cfg(test)]
tokio::task_local! {
    pub(super) static PRIMARY_OWNER_AFTER_BROWSER_RESET: (
        Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>
    );
}

impl MethodHandler {
    pub(crate) async fn handle_personal_google_rpc(
        &self,
        method: &str,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        if self.resolve_edition_profile().await.is_personal() {
            return super::gates::enterprise_only_reject_frame();
        }
        let Some(db) = self.user_db.read().await.clone() else {
            return WsFrame::error_response("", "人員資料尚未就緒，請聯絡管理員。");
        };
        if let Err(e) = flow::active_user(&db, &ctx.user_id) {
            return WsFrame::error_response("", &e);
        }
        let result: Result<Value, String> = async {
            match method {
                "me.google.status" => self.personal_google_status(&db, ctx).await,
                "me.google.connect_start" => {
                    let session = google_personal::session_for(&ctx.user_id)?;
                    let origin = params
                        .get("redirect_origin")
                        .and_then(Value::as_str)
                        .ok_or("請提供目前控制台的網址。")?;
                    flow::start(
                        &self.home_dir,
                        &db,
                        &ctx.user_id,
                        &session,
                        origin,
                        &crate::server::allowed_origins_snapshot(),
                    )
                    .await
                }
                "me.google.connect_finish" => {
                    let session = google_personal::session_for(&ctx.user_id)?;
                    flow::finish(
                        &self.home_dir,
                        &db,
                        &ctx.user_id,
                        &session,
                        params.get("state").and_then(Value::as_str).unwrap_or(""),
                    )
                    .await
                }
                "me.google.connect_paste" => {
                    let session = google_personal::session_for(&ctx.user_id)?;
                    let nonce = pending::receive_pasted(
                        &self.home_dir,
                        &ctx.user_id,
                        &session,
                        params.get("url").and_then(Value::as_str).unwrap_or(""),
                    )?;
                    flow::finish(&self.home_dir, &db, &ctx.user_id, &session, &nonce).await
                }
                "me.google.disconnect" | "users.google_disconnect" => {
                    let target = if method == "users.google_disconnect" {
                        acl::require_role(ctx, UserRole::Admin)?;
                        params
                            .get("user_id")
                            .and_then(Value::as_str)
                            .ok_or("請指定同事。")?
                    } else {
                        ctx.user_id.as_str()
                    };
                    if params.get("confirm").and_then(Value::as_bool) != Some(true) {
                        return Err("請確認要中斷 Google 連線。".into());
                    }
                    let revoked =
                        lifecycle::disconnect(&self.home_dir, &db, target, &ctx.user_id, false)
                            .await?;
                    Ok(json!({"disconnected":true,"provider_revoked":revoked,"provider_note":if revoked {None} else {Some("已中斷本機連線。為保護其他共用授權，或因 Google 尚未確認撤銷，沒有確認 Google 端已撤銷；請到 Google 已連結應用程式檢查。")}}))
                }
                "me.google.consents.set" => {
                    let agent = params
                        .get("agent_id")
                        .and_then(Value::as_str)
                        .filter(|a| is_valid_agent_id(a))
                        .ok_or("請指定 AI 員工。")?;
                    let allowed = params
                        .get("allowed")
                        .and_then(Value::as_bool)
                        .ok_or("請選擇允許或拒絕。")?;
                    if allowed
                        && params.get("confirm_shared_access").and_then(Value::as_bool)
                            != Some(true)
                    {
                        return Err("請先確認誰能透過這位員工使用你的帳號。".into());
                    }
                    db.set_google_consent(&ctx.user_id, agent, allowed, &ctx.user_id)?;
                    Ok(json!({"agent_id":agent,"allowed":allowed}))
                }
                "agents.primary_owner_set" => {
                    acl::require_role(ctx, UserRole::Admin)?;
                    let agent = params
                        .get("agent_id")
                        .and_then(Value::as_str)
                        .filter(|a| is_valid_agent_id(a))
                        .ok_or("請指定 AI 員工。")?;
                    let user = params
                        .get("user_id")
                        .and_then(Value::as_str)
                        .ok_or("請指定主要負責人。")?;
                    // Reject an invalid candidate without destroying a valid
                    // login. Invalidate before EVERY write, including A→B→A;
                    // the final owner fingerprint alone is insufficient.
                    db.validate_google_primary_owner_candidate(agent, user)?;
                    crate::computer_use_sessions::workspace_admin::admin_sessions(&self.home_dir)
                        .forget_login(agent, "primary_owner_changed")
                        .await
                        .map_err(|_| "目前無法清除先前的瀏覽器登入狀態，這次變更未套用。".to_owned())?;
                    #[cfg(test)]
                    if let Ok((reached, release)) = PRIMARY_OWNER_AFTER_BROWSER_RESET.try_with(Clone::clone) {
                        reached.notify_one();
                        release.notified().await;
                    }
                    db.set_google_primary_owner(agent, user, &ctx.user_id)?;
                    Ok(json!({"agent_id":agent,"user_id":user}))
                }
                _ => Err("找不到這項連線操作。".into()),
            }
        }
        .await;
        match result {
            Ok(payload) => WsFrame::ok_response("", payload),
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    async fn personal_google_status(
        &self,
        db: &duduclaw_auth::UserDb,
        ctx: &UserContext,
    ) -> Result<Value, String> {
        crate::person_credentials::sync_user_status(&self.home_dir, &ctx.user_id, true)?;
        let own = crate::person_credentials::get(&self.home_dir, &ctx.user_id)?.map(|r| r.metadata);
        let consents = db.google_consents(&ctx.user_id)?;
        let registry = self.registry.read().await;
        let mut requests = Vec::new();
        let mut delegated = Vec::new();
        for agent in registry.list() {
            let name = &agent.config.agent.name;
            let effective = gi::effective_for(&self.home_dir, Some(name));
            match &effective.effective {
                Effective::Personal { selector } => {
                    let selected = access::select_user(db, name, selector);
                    if selected.as_ref().is_ok_and(|u| u.id == ctx.user_id) {
                        let consent = consents.iter().find(|c| c.agent_name == *name);
                        let people = db
                            .get_agent_users(name)?
                            .into_iter()
                            .filter(|b| b.user_id != ctx.user_id)
                            .filter_map(|b| db.get_user(&b.user_id).ok().flatten())
                            .filter(|u| u.status == duduclaw_auth::UserStatus::Active)
                            .map(|u| json!({"user_id":u.id,"name":u.display_name,"email":u.email}))
                            .collect::<Vec<_>>();
                        requests.push(json!({"agent_id":name,"allowed":consent.map(|c| c.allowed),"requesters":effective.requesters.as_str(),"bound_people":people,
                            "channels":agent.config.permissions.allowed_channels,"public_visitors_allowed":false}));
                    }
                }
                Effective::Delegate { email } if email.eq_ignore_ascii_case(&ctx.email) => {
                    delegated.push(json!({"agent_id":name,"email":email}))
                }
                _ => {}
            }
        }
        requests.sort_by_key(|r| r.get("allowed").is_some_and(|v| !v.is_null()));
        Ok(
            json!({"connection":own,"login_email":ctx.email,"client_configured":crate::mcp_oauth::get_client_config(&self.home_dir,"google").is_some(),
            "integration_enabled":crate::google_workspace::integration_enabled(&self.home_dir),"requests":requests,"delegated":delegated,
            "requires_login":google_personal::session_for(&ctx.user_id).is_err(),"recent_usage":google_personal::history::recent(&self.home_dir,&ctx.user_id,&ctx.email)?}),
        )
    }
}
