//! `agents.channels.*` — an AI employee's own channel identities, managed
//! from the employee's page (`crate::agent_channels`).
//!
//! Authority (the existing rules, not widened):
//! - status: the employee's Owner or an admin (read-only, no credential);
//! - binding or removing the employee's own Telegram bot or Google Chat app:
//!   admin only — `channels.telegram` / `channels.googlechat` are authority
//!   keys (`agent_authority::AUTHORITY`) and `channels.add` is admin-only.
//!
//! Writes re-check the caller's binding from `users.db` (owner decision D7)
//! before and inside the file lock.

use super::*;

use crate::agent_channels::{self, conflicts, identity, status, store, telegram_probe};

const STATUS: &str = "agents.channels.status";
const TELEGRAM_SET: &str = "agents.channels.telegram.set";
const TELEGRAM_REMOVE: &str = "agents.channels.telegram.remove";
pub(super) const GOOGLECHAT_SET: &str = "agents.channels.googlechat.set";
pub(super) const GOOGLECHAT_REMOVE: &str = "agents.channels.googlechat.remove";

const METHODS: &[&str] = &[
    STATUS,
    TELEGRAM_SET,
    TELEGRAM_REMOVE,
    GOOGLECHAT_SET,
    GOOGLECHAT_REMOVE,
];

impl MethodHandler {
    /// `Some(frame)` for an `agents.channels.*` method, `None` otherwise.
    pub(crate) async fn dispatch_agent_channels(
        &self,
        method: &str,
        params: &Value,
        ctx: &UserContext,
    ) -> Option<WsFrame> {
        if !METHODS.contains(&method) {
            return None;
        }
        let agent_id = match acl::extract_and_check_agent(ctx, params, AccessLevel::Owner) {
            Ok(id) => id,
            Err(e) => return Some(WsFrame::error_response("", &e)),
        };
        if method == STATUS {
            return Some(self.handle_agent_channels_status(&agent_id, ctx).await);
        }
        if let Err(e) = acl::require_role(ctx, UserRole::Admin) {
            return Some(WsFrame::error_response("", &e));
        }
        let live = match crate::agent_files::flow::require_live_agent_access(
            &self.home_dir,
            ctx,
            &agent_id,
            AccessLevel::Owner,
        ) {
            Ok(live) => live,
            Err(e) => return Some(WsFrame::error_response("", &e)),
        };
        let frame = match method {
            TELEGRAM_SET => {
                self.handle_agent_telegram_set(&agent_id, params, &live, ctx)
                    .await
            }
            TELEGRAM_REMOVE => {
                self.handle_agent_telegram_remove(&agent_id, &live, ctx)
                    .await
            }
            GOOGLECHAT_SET => {
                self.handle_agent_googlechat_set(&agent_id, params, &live, ctx)
                    .await
            }
            _ => {
                self.handle_agent_googlechat_remove(&agent_id, &live, ctx)
                    .await
            }
        };
        Some(frame)
    }

    /// Runtime state of `label` from the channel status map.
    pub(super) async fn channel_runtime(&self, label: &str) -> status::Runtime {
        let map = self.channel_status.read().await;
        match map.get(label) {
            Some(s) => status::Runtime {
                present: true,
                connected: s.connected,
                error: s.error.clone(),
                last_event: s.last_event.as_ref().map(|t| t.to_rfc3339()),
            },
            None => status::Runtime::default(),
        }
    }

    async fn handle_agent_channels_status(&self, agent_id: &str, ctx: &UserContext) -> WsFrame {
        let (tg_cfg, gc_cfg) = {
            let reg = self.registry.read().await;
            let Some(agent) = reg.get(agent_id) else {
                return WsFrame::error_response("", "Agent not found");
            };
            let channels = agent.config.channels.as_ref();
            (
                channels.and_then(|c| c.telegram.clone()),
                channels.and_then(|c| c.googlechat.clone()),
            )
        };
        let home = &self.home_dir;
        let tg_label = agent_channels::label("telegram", agent_id);
        let gc_label = agent_channels::label("googlechat", agent_id);
        let tg_configured = store::telegram_configured(tg_cfg.as_ref());
        let gc_configured = store::googlechat_configured(gc_cfg.as_ref());
        let shared_tg = crate::config_crypto::read_encrypted_config_field(
            home,
            "channels",
            "telegram_bot_token",
        )
        .await
        .is_some();
        let shared_gc = crate::config_crypto::read_encrypted_config_field(
            home,
            "channels",
            "googlechat_project_number",
        )
        .await
        .is_some();
        let gc_email = match identity::get(&gc_label) {
            Some(e) => Some(e),
            None if gc_configured => store::agent_chat_app(home, agent_id).await.and_then(|app| {
                agent_channels::googlechat_config::client_email(app.service_account.expose())
            }),
            None => None,
        };
        let telegram = status::telegram(
            tg_configured,
            &self.channel_runtime(&tg_label).await,
            identity::get(&tg_label),
            shared_tg,
            ctx.is_admin(),
        );
        let googlechat = status::googlechat(
            agent_id,
            gc_configured,
            &self.channel_runtime(&gc_label).await,
            gc_cfg
                .map(|g| g.project_number)
                .filter(|p| !p.trim().is_empty()),
            gc_email,
            shared_gc,
            ctx.is_admin(),
        );
        WsFrame::ok_response(
            "",
            json!({ "agent_id": agent_id, "telegram": telegram, "googlechat": googlechat }),
        )
    }

    async fn handle_agent_telegram_set(
        &self,
        agent_id: &str,
        params: &Value,
        live: &UserContext,
        session: &UserContext,
    ) -> WsFrame {
        if let Err(e) = crate::product_policy::require_channel(&self.home_dir, "telegram") {
            return WsFrame::error_response("", &e);
        }
        let raw = params.get("token").and_then(|v| v.as_str()).unwrap_or("");
        let token = match crate::config_crypto::validate_channel_token("telegram", raw) {
            Ok(t) => t,
            Err(e) => return WsFrame::error_response("", &e),
        };
        if self.registry.read().await.get(agent_id).is_none() {
            return WsFrame::error_response("", "Agent not found");
        }
        // Held across the duplicate check and the write (B11 L3).
        let _bind = agent_channels::binding_lock().await;
        if let Some(holder) =
            conflicts::telegram_bot_holder(&self.home_dir, &self.registry, &token, agent_id).await
        {
            return WsFrame::error_response("", &holder.telegram_refusal());
        }
        let (verified, bot_username) = match telegram_probe::get_me(&token).await {
            telegram_probe::Probe::Rejected(desc) => {
                return WsFrame::error_response(
                    "",
                    &format!(
                        "Telegram 拒絕了這個 Bot Token（{desc}）。請到 BotFather 重新複製完整的 Token 後再貼上。"
                    ),
                );
            }
            telegram_probe::Probe::Verified { username } => (true, Some(username)),
            telegram_probe::Probe::Unverified(e) => {
                warn!(
                    agent_id,
                    "Telegram token could not be verified now ({e}); saving, the bot will keep retrying"
                );
                (false, None)
            }
        };
        let enc = match store::encrypt_or_refuse(&token, &self.home_dir) {
            Ok(enc) => enc,
            Err(e) => return WsFrame::error_response("", &e),
        };
        if let Err(e) = self
            .update_agent_toml_as(
                agent_id,
                TELEGRAM_SET,
                Some(live),
                Some(session),
                move |t| store::apply_telegram(t, &enc),
            )
            .await
        {
            return WsFrame::error_response("", &format!("儲存失敗：{e}"));
        }
        let replaces_shared_bot = conflicts::shared_telegram_is(&self.home_dir, &token).await;
        let hot_started = self.restart_agent_telegram(agent_id).await;
        self.audit_agent_channel("agent_channel_bound", agent_id, "telegram", live);
        WsFrame::ok_response(
            "",
            json!({
                "success": true,
                "agent_id": agent_id,
                "bot_username": bot_username,
                "verified": verified,
                "hot_started": hot_started,
                "replaces_shared_bot": replaces_shared_bot,
            }),
        )
    }

    async fn handle_agent_telegram_remove(
        &self,
        agent_id: &str,
        live: &UserContext,
        session: &UserContext,
    ) -> WsFrame {
        let sm_cfg =
            duduclaw_security::secret_manager::SecretManagerConfig::load_from_home(&self.home_dir)
                .await;
        let old = crate::config_crypto::read_agent_channel_token(
            &self.home_dir,
            agent_id,
            "telegram",
            &sm_cfg,
        )
        .await
        .map(|s| crate::config_crypto::repair_telegram_token(s.expose()).into_owned());
        if let Err(e) = self
            .update_agent_toml_as(agent_id, TELEGRAM_REMOVE, Some(live), Some(session), |t| {
                store::remove_channel(t, "telegram");
                Ok(())
            })
            .await
        {
            return WsFrame::error_response("", &format!("移除失敗：{e}"));
        }
        let label = agent_channels::label("telegram", agent_id);
        self.hot_stop_channel(&label).await;
        identity::clear(&label);
        // The shared bot deferred to this employee's poller; give it back.
        let shared_restarted = match old {
            Some(token) if conflicts::shared_telegram_is(&self.home_dir, &token).await => {
                self.hot_start_channel("telegram").await
            }
            _ => false,
        };
        self.audit_agent_channel("agent_channel_removed", agent_id, "telegram", live);
        WsFrame::ok_response(
            "",
            json!({ "success": true, "agent_id": agent_id, "shared_bot_restarted": shared_restarted }),
        )
    }

    /// Stop and start only `agent_id`'s own Telegram bot (other bots keep
    /// polling). When the shared bot is the same bot, the shared poller is
    /// stopped first — one poller per bot. `true` when the bot is running.
    pub(crate) async fn restart_agent_telegram(&self, agent_id: &str) -> bool {
        let label = agent_channels::label("telegram", agent_id);
        self.hot_stop_channel(&label).await;
        let Some(ctx) = self.reply_ctx.read().await.clone() else {
            return false;
        };
        let sm_cfg =
            duduclaw_security::secret_manager::SecretManagerConfig::load_from_home(&self.home_dir)
                .await;
        if let Some(token) = crate::config_crypto::read_agent_channel_token(
            &self.home_dir,
            agent_id,
            "telegram",
            &sm_cfg,
        )
        .await
        {
            let token = crate::config_crypto::repair_telegram_token(token.expose()).into_owned();
            if conflicts::shared_telegram_is(&self.home_dir, &token).await {
                self.hot_stop_channel("telegram").await;
            }
        }
        match crate::telegram::start_agent_telegram_bot(&self.home_dir, ctx, agent_id).await {
            Some((label, handle)) => {
                self.register_channel_handle(&label, handle).await;
                true
            }
            None => false,
        }
    }

    /// Audit row for a bind / unbind (never carries the credential).
    pub(super) fn audit_agent_channel(
        &self,
        event: &str,
        agent_id: &str,
        channel: &str,
        actor: &UserContext,
    ) {
        crate::security_autopilot::audit_and_emit(
            &self.home_dir,
            &duduclaw_security::audit::AuditEvent::new(
                event,
                agent_id,
                duduclaw_security::audit::Severity::Info,
                json!({ "channel": channel, "actor": actor.user_id, "source": "dashboard" }),
            ),
        );
    }
}
