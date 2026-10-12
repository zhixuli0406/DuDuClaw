//! `odoo.livechat.*` — dashboard settings for the Odoo website Live Chat
//! bridge (`crate::odoo_livechat`). All methods are Admin-only (gated in
//! `dispatch_system`).
//!
//! `[odoo_livechat]` lives in its own section because `odoo.configure`
//! rewrites the whole `[odoo]` section on every save.

#[allow(unused_imports)]
use super::*;

use duduclaw_odoo::livechat::{self, rpc as lc_rpc, LivechatConfig};
use duduclaw_odoo::{OdooConfig, OdooConnector};

use super::config_commit::{commit_table_locked, content_hash, read_text_or_empty};

const BOT_KEY_ENC: &str = "bot_api_key_enc";
/// Hand-written plaintext twin (`config_crypto` reads `_enc` first, then this).
const BOT_KEY_PLAIN: &str = "bot_api_key";

/// Whether a bot key is on file, encrypted or hand-written in plaintext.
fn bot_key_set(table: &toml::Table) -> bool {
    livechat_section(table).is_some_and(|t| {
        [BOT_KEY_ENC, BOT_KEY_PLAIN]
            .iter()
            .any(|k| t.get(*k).and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty()))
    })
}

impl MethodHandler {
    /// `odoo.livechat.get` → `{config, bot_api_key_set, status, sessions}`.
    pub(crate) async fn handle_odoo_livechat_get(&self) -> WsFrame {
        let table = self.read_config_table(&self.home_dir.join("config.toml")).await;
        let cfg = LivechatConfig::from_toml(&table);
        let key_set = bot_key_set(&table);
        let mut out = crate::odoo_livechat::status_json(&self.home_dir);
        out["config"] = serde_json::to_value(&cfg).unwrap_or(Value::Null);
        out["bot_api_key_set"] = json!(key_set);
        out["odoo_configured"] = json!(OdooConfig::from_toml(&table).is_configured());
        WsFrame::ok_response("", out)
    }

    /// `odoo.livechat.status` → live bridge status and open sessions.
    pub(crate) async fn handle_odoo_livechat_status(&self) -> WsFrame {
        WsFrame::ok_response("", crate::odoo_livechat::status_json(&self.home_dir))
    }

    /// `odoo.livechat.set {config, bot_api_key?}` — validate, encrypt the bot
    /// key, and commit `[odoo_livechat]` under the config lock. An absent or
    /// empty `bot_api_key` keeps the stored one; `clear_bot_api_key: true`
    /// removes it.
    pub(crate) async fn handle_odoo_livechat_set(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let Some(raw_cfg) = params.get("config") else {
            return WsFrame::error_response("", "Missing 'config' parameter");
        };
        let cfg: LivechatConfig = match serde_json::from_value(raw_cfg.clone()) {
            Ok(c) => c,
            Err(e) => return WsFrame::error_response("", &format!("設定格式錯誤：{e}")),
        };
        if let Err(e) = cfg.validate() {
            return WsFrame::error_response("", &e);
        }
        {
            let reg = self.registry.read().await;
            let named = std::iter::once(cfg.default_agent.trim())
                .chain(cfg.audience_rules.iter().map(|r| r.agent.trim()))
                .filter(|a| !a.is_empty());
            for agent in named {
                if reg.get(agent).is_none() {
                    return WsFrame::error_response("", &format!("找不到 AI 員工「{agent}」"));
                }
            }
        }

        let path = self.home_dir.join("config.toml");
        let text = match read_text_or_empty(&path) {
            Ok(t) => t,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let mut table: toml::Table = match text.parse() {
            Ok(t) => t,
            Err(e) => {
                return WsFrame::error_response(
                    "",
                    &format!("config.toml is not valid TOML, refusing to rewrite it: {e}"),
                )
            }
        };
        // Stored key fields (encrypted, or a hand-written plaintext twin),
        // carried over unless this save sets or clears the key.
        let old_keys: Vec<(&str, toml::Value)> = livechat_section(&table)
            .map(|t| {
                [BOT_KEY_ENC, BOT_KEY_PLAIN]
                    .iter()
                    .filter_map(|k| t.get(*k).map(|v| (*k, v.clone())))
                    .collect()
            })
            .unwrap_or_default();

        let mut section = match toml::Value::try_from(&cfg) {
            Ok(toml::Value::Table(t)) => t,
            _ => return WsFrame::error_response("", "無法序列化線上客服設定"),
        };
        let new_key = params
            .get("bot_api_key")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty() && !super::config_commit::is_secret_placeholder(s));
        let clear_key = params
            .get("clear_bot_api_key")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let key_change = if let Some(key) = new_key {
            if key.chars().count() > 512 {
                return WsFrame::error_response("", "API 金鑰過長");
            }
            match crate::config_crypto::encrypt_value(key, &self.home_dir) {
                Some(enc) => {
                    section.insert(BOT_KEY_ENC.into(), toml::Value::String(enc));
                    Some("odoo_livechat.bot_api_key set")
                }
                None => {
                    return WsFrame::error_response(
                        "",
                        "Could not encrypt the API key — keyfile write failed. See gateway log.",
                    )
                }
            }
        } else if clear_key {
            Some("odoo_livechat.bot_api_key cleared")
        } else {
            for (k, v) in old_keys {
                section.insert(k.into(), v);
            }
            None
        };
        table.insert(livechat::SECTION.into(), toml::Value::Table(section));

        if let Err(e) = commit_table_locked(&path, content_hash(&text), &table).await {
            return WsFrame::error_response("", &e);
        }
        let mut changes = vec![format!(
            "odoo_livechat saved (enabled={}, rules={}, default_agent={})",
            cfg.enabled,
            cfg.audience_rules.len(),
            if cfg.default_agent.is_empty() { "(main)" } else { &cfg.default_agent }
        )];
        changes.extend(key_change.map(String::from));
        duduclaw_security::audit::log_config_changed(
            &self.home_dir,
            &ctx.email,
            &format!("{:?}", ctx.role).to_lowercase(),
            &changes,
        );
        crate::security_autopilot::emit_config_changed();
        info!(?changes, "odoo.livechat.set completed");
        WsFrame::ok_response("", json!({"saved": true}))
    }

    /// `odoo.livechat.resume {session_id}` — hand a taken-over session back
    /// to the AI.
    pub(crate) async fn handle_odoo_livechat_resume(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let Some(id) = params.get("session_id").and_then(|v| v.as_i64()) else {
            return WsFrame::error_response("", "Missing 'session_id' parameter");
        };
        if crate::odoo_livechat::resume_session(&self.home_dir, id) {
            // Re-enabling the AI over a human takeover is an operator action.
            duduclaw_security::audit::log_config_changed(
                &self.home_dir,
                &ctx.email,
                &format!("{:?}", ctx.role).to_lowercase(),
                &[format!("odoo_livechat session {id} handed back to the AI")],
            );
            WsFrame::ok_response("", json!({"resumed": true}))
        } else {
            WsFrame::error_response("", "找不到這個對話（可能已結束）")
        }
    }

    /// `odoo.livechat.test {bot_username?, bot_api_key?}` — sign in as the
    /// bot (the given credentials, else the saved ones) and report what the
    /// settings page needs: bot identity, Odoo dialect, Live Chat channels
    /// (and whether the bot is an operator in each), Chatbot scripts and
    /// internal users. Read-only: it never changes the bot's presence.
    pub(crate) async fn handle_odoo_livechat_test(&self, params: Value) -> WsFrame {
        let table = self.read_config_table(&self.home_dir.join("config.toml")).await;
        let odoo_cfg = OdooConfig::from_toml(&table);
        if !odoo_cfg.is_configured() {
            return WsFrame::error_response("", "請先在上方完成 Odoo 連線設定");
        }
        let saved = LivechatConfig::from_toml(&table);
        let username = params
            .get("bot_username")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or(saved.bot_username.trim())
            .to_string();
        let given_key = params
            .get("bot_api_key")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty() && !super::config_commit::is_secret_placeholder(s))
            .map(String::from);

        let (cfg, credential) = if username.is_empty() {
            match crate::odoo_events::resolve_credential(&self.home_dir).await {
                Some(c) => (odoo_cfg.clone(), c),
                None => return WsFrame::error_response("", "[odoo] 的連線憑證尚未設定或無法解密"),
            }
        } else {
            let key = match given_key {
                Some(k) => Some(k),
                None => crate::config_crypto::decrypt_config_field_async(
                    &table,
                    livechat::SECTION,
                    "bot_api_key",
                    &self.home_dir,
                )
                .await
                .map(|s| s.expose().to_string())
                .filter(|s| !s.is_empty()),
            };
            let Some(key) = key else {
                return WsFrame::error_response("", "請輸入機器人帳號的 API 金鑰");
            };
            (
                OdooConfig {
                    username: username.clone(),
                    ..odoo_cfg.clone()
                },
                key,
            )
        };
        let conn = match OdooConnector::connect(&cfg, &credential).await {
            Ok(c) => c,
            Err(e) => return WsFrame::error_response("", &format!("機器人帳號登入 Odoo 失敗：{e}")),
        };
        match probe(&conn, username.is_empty() || username == odoo_cfg.username).await {
            Ok(v) => WsFrame::ok_response("", v),
            Err(e) => WsFrame::error_response("", &e),
        }
    }
}

fn livechat_section(table: &toml::Table) -> Option<&toml::Table> {
    table.get(livechat::SECTION).and_then(|v| v.as_table())
}

async fn probe(conn: &OdooConnector, shares_main_user: bool) -> Result<Value, String> {
    let dialect = lc_rpc::detect_dialect(conn).await?;
    let (partner_id, name, im_status) = lc_rpc::bot_identity(conn).await?;
    let uid = conn.uid.unwrap_or_default();
    let channels = lc_rpc::list_livechat_channels(conn).await?;
    let channels: Vec<Value> = channels
        .into_iter()
        .map(|mut c| {
            let is_operator = c["operator_ids"]
                .as_array()
                .is_some_and(|a| a.iter().any(|v| v.as_i64() == Some(uid)));
            c["bot_is_operator"] = json!(is_operator);
            c
        })
        .collect();
    // Chatbot scripts and the user list are conveniences for the pickers;
    // a missing right there must not fail the whole test.
    let scripts = lc_rpc::list_chatbot_scripts(conn).await.unwrap_or_default();
    let users = lc_rpc::list_internal_users(conn).await.unwrap_or_default();

    // Codes, translated by the dashboard (`odoo.livechat.warn.<code>`).
    let mut warnings = Vec::new();
    if shares_main_user {
        // Visitors would see the [odoo] user's name, and that person's own
        // messages could not be told apart from the bot's.
        warnings.push("shares_main_user");
    }
    if !channels.iter().any(|c| c["bot_is_operator"] == json!(true)) {
        warnings.push("not_operator");
    }
    let odoo19 = !dialect.has_livechat_active;
    if odoo19 {
        // Odoo 19 lets only administrators write presence: the [odoo]
        // connection user keeps the bot online and must be an admin.
        warnings.push("odoo19_admin_presence");
    }
    Ok(json!({
        "bot": {"uid": uid, "partner_id": partner_id, "name": name, "im_status": im_status},
        "odoo_generation": if odoo19 { "19+" } else { "17/18" },
        "channels": channels,
        "chatbot_scripts": scripts,
        "users": users,
        "warnings": warnings,
    }))
}
