//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    pub(crate) async fn handle_channels_add(&self, params: Value) -> WsFrame {
        let channel_type = match params.get("type").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => return WsFrame::error_response("", "Missing 'type' parameter"),
        };
        if let Err(e) = crate::product_policy::require_channel(&self.home_dir, channel_type) {
            return WsFrame::error_response("", &e);
        }
        let config_obj = params.get("config").cloned().unwrap_or(json!({}));
        let token = config_obj
            .get("token")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let secret = config_obj
            .get("secret")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let agent_name = params.get("agent").and_then(|v| v.as_str()).unwrap_or("");
        let normalized_project;
        let secret = if channel_type == "googlechat" {
            normalized_project = match crate::agent_channels::googlechat_config::validate_project_number(secret) {
                Ok(project) => project,
                Err(e) => return WsFrame::error_response("", &e),
            };
            normalized_project.as_str()
        } else { secret };

        if token.is_empty() {
            return WsFrame::error_response("", "Missing 'config.token' parameter");
        }

        // WP12 — validate the credential shape BEFORE it is encrypted and
        // stored. A malformed Telegram token used to be persisted silently and
        // only surfaced later as a permanently-offline channel; now the save
        // either normalises it or refuses with an actionable message.
        let token = match crate::config_crypto::validate_channel_token(channel_type, token) {
            Ok(t) => t,
            Err(msg) => return WsFrame::error_response("", &msg),
        };
        let token = token.as_str();

        if channel_type == "googlechat" {
            if let Some(fields) = config_obj.as_object() {
                if let Err(error) = crate::agent_channels::googlechat_config::validate_fields(fields.keys().map(String::as_str), &["token", "secret"]) {
                    return WsFrame::error_response("", &error);
                }
            }
            // Preserve external secret references: validate their resolved
            // candidate, but persist the original encrypted reference.
            let sm = duduclaw_security::secret_manager::SecretManagerConfig::load_from_home(&self.home_dir).await;
            let candidate = match duduclaw_security::secret_ref::SecretRef::from_typed(None, token).resolve(&sm, &self.home_dir).await {
                Some(key) => key,
                None => return WsFrame::error_response("", "無法讀取 Google Chat 服務帳戶金鑰。"),
            };
            if let Err(e) = crate::googlechat::verify_import_key(candidate.expose()).await {
                return WsFrame::error_response("", &e);
            }
        }

        // Cloud-tier channel cap (self-host is never capped — Apache 2.0).
        let channel_count = self.count_configured_channels().await;
        if let Some(msg) = self.tier_limit_message("channel", channel_count).await {
            return WsFrame::error_response("", &msg);
        }

        // Per-agent channel: write to agent.toml [channels.{platform}]. Only the
        // token-exclusive channels can be bound per agent; LINE/WhatsApp/Feishu are
        // single global webhook endpoints, so when an agent is selected for them we
        // fall through to the global path and bind the agent as `default_agent`
        // (below) instead of erroring out — otherwise the save silently fails and
        // nothing is persisted.
        if !agent_name.is_empty() && matches!(channel_type, "discord" | "telegram" | "slack") {
            // One Telegram bot belongs to one employee (getUpdates is
            // exclusive per bot): refuse a bot another employee already uses.
            let _bind = crate::agent_channels::binding_lock().await;
            if channel_type == "telegram" {
                if let Some(holder) = crate::agent_channels::conflicts::telegram_bot_holder(
                    &self.home_dir,
                    &self.registry,
                    token,
                    agent_name,
                )
                .await
                {
                    return WsFrame::error_response("", &holder.telegram_refusal());
                }
            }
            let (token_field, secret_field) = match channel_type {
                "discord" => ("bot_token", None),
                "telegram" => ("bot_token", None),
                "slack" => ("bot_token", Some("app_token")),
                _ => {
                    return WsFrame::error_response(
                        "",
                        &format!("Per-agent channels not supported for: {channel_type}"),
                    );
                }
            };

            let token_owned = token.to_string();
            let secret_owned = secret.to_string();
            let channel_type_owned = channel_type.to_string();
            let home = self.home_dir.clone();

            if let Err(e) = crate::disconnect_forget::connections::mark_connecting(&self.home_dir,
                &format!("channel:{channel_type}:{agent_name}")) {
                return WsFrame::error_response("", &e);
            }
            if let Err(e) = self
                .update_agent_toml(agent_name, move |table| {
                    let channels = table
                        .entry("channels")
                        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
                        .as_table_mut()
                        .ok_or("Invalid [channels] section")?;
                    let section = channels
                        .entry(&channel_type_owned)
                        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
                        .as_table_mut()
                        .ok_or_else(|| {
                            format!("Invalid [channels.{}] section", channel_type_owned)
                        })?;

                    // Parity with the wecom/dingtalk MED-B fix: when encryption
                    // succeeds, persist ONLY the `_enc` copy and REMOVE the
                    // plaintext key (never blank it — the enc-aware readers treat
                    // a present-but-empty plaintext as "channel removed"). All
                    // per-agent readers (telegram.rs / discord.rs / slack.rs /
                    // config_crypto::resolve_agent_token) prefer `_enc`.
                    // agent.toml lives in the employee's own directory: a
                    // token that cannot be encrypted is refused, never stored
                    // as plaintext there (B11).
                    let enc = crate::agent_channels::store::encrypt_or_refuse(&token_owned, &home)?;
                    section.remove(token_field); // drop any stale plaintext copy
                    section.insert(format!("{token_field}_enc"), toml::Value::String(enc));
                    if let Some(sf) = secret_field {
                        if !secret_owned.is_empty() {
                            let enc = crate::agent_channels::store::encrypt_or_refuse(&secret_owned, &home)?;
                            section.remove(sf); // drop any stale plaintext copy
                            section.insert(format!("{sf}_enc"), toml::Value::String(enc));
                        }
                    }
                    Ok(())
                })
                .await
            {
                return WsFrame::error_response("", &format!("Failed to update agent config: {e}"));
            }

            // Hot-start: stop existing per-agent bot if any, then re-launch all per-agent bots
            let label = format!("{channel_type}:{agent_name}");
            self.hot_stop_channel(&label).await;

            let mut hot_started = false;
            if channel_type == "telegram" {
                hot_started = self.restart_agent_telegram(agent_name).await;
            } else if let Some(ctx) = self.reply_ctx.read().await.clone() {
                let handles: Vec<(String, tokio::task::JoinHandle<()>)> = match channel_type {
                    "discord" => crate::discord::start_discord_bots(&self.home_dir, ctx).await,
                    "telegram" => crate::telegram::start_telegram_bots(&self.home_dir, ctx).await,
                    "slack" => crate::slack::start_slack_bots(&self.home_dir, ctx).await,
                    _ => Vec::new(),
                };
                for (l, h) in handles {
                    if l == label {
                        hot_started = true;
                    }
                    self.register_channel_handle(&l, h).await;
                }
            }

            info!(channel_type, agent_name, "Per-agent channel config saved");
            return WsFrame::ok_response(
                "",
                json!({
                    "success": true,
                    "type": label,
                    "hot_started": hot_started,
                    // v1.68.0: every per-agent transport hot-starts; a false
                    // `hot_started` means the credentials did not work.
                    "restart_required": false,
                    "not_started_reason": (!hot_started).then_some("check_credentials"),
                }),
            );
        }

        // The shared Google Chat app may not reuse an employee's own app
        // project (B11 L3).
        let _bind = crate::agent_channels::binding_lock().await;
        if channel_type == "googlechat" {
            if let Some(msg) =
                crate::agent_channels::conflicts::shared_project_conflict(&self.registry, secret).await
            {
                return WsFrame::error_response("", &msg);
            }
        }

        // Global channel: write to config.toml [channels]
        let (token_key, secret_key) = match channel_type {
            "line" => ("line_channel_token", Some("line_channel_secret")),
            "telegram" => ("telegram_bot_token", None),
            "discord" => ("discord_bot_token", None),
            "slack" => ("slack_bot_token", Some("slack_app_token")),
            "whatsapp" => ("whatsapp_access_token", Some("whatsapp_phone_number_id")),
            "feishu" => ("feishu_app_id", Some("feishu_app_secret")),
            // token = service-account JSON key; secret = Cloud project number
            "googlechat" => (
                "googlechat_service_account_json",
                Some("googlechat_project_number"),
            ),
            // token = client secret; secret = Microsoft App ID
            "teams" => ("teams_app_password", Some("teams_app_id")),
            // token = corpsecret; secret = corpid (identifier, plain)
            "wecom" => ("wecom_corp_secret", Some("wecom_corp_id")),
            // token = robot AppSecret; secret = AppKey (identifier, plain)
            "dingtalk" => ("dingtalk_app_secret", Some("dingtalk_app_key")),
            _ => {
                return WsFrame::error_response(
                    "",
                    &format!("Unknown channel type: {channel_type}"),
                );
            }
        };

        // Encrypt the primary token before storing (H3).
        let enc_token_key = format!("{token_key}_enc");
        let encrypted_token = crate::config_crypto::encrypt_value(token, &self.home_dir);

        if channel_type == "googlechat" && encrypted_token.is_none() {
            return WsFrame::error_response("", crate::agent_channels::store::ENCRYPTION_UNAVAILABLE);
        }

        // XC.3: `whatsapp_phone_number_id` is NOT a secret — it must be stored as
        // plaintext (consistent with the per-agent `agents.update` path, which
        // does not encrypt it). Google Chat's project number and Teams' App ID
        // are likewise identifiers, not secrets. All other secret_key fields
        // ARE encrypted.
        let secret_is_plain = matches!(
            secret_key,
            Some("whatsapp_phone_number_id")
                | Some("googlechat_project_number")
                | Some("teams_app_id")
                | Some("wecom_corp_id")
                | Some("dingtalk_app_key")
        );

        let config_path = self.home_dir.join("config.toml");
        let (mut table, config_snapshot) = match self.read_toml_write_snapshot(&config_path).await {
            Ok(snapshot) => snapshot,
            Err(e) => return WsFrame::error_response("", &e),
        };

        let channels = table
            .entry("channels")
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
            .as_table_mut();

        let channels = match channels {
            Some(ch) => ch,
            None => {
                return WsFrame::error_response("", "Invalid [channels] section in config.toml");
            }
        };

        // MED-B (extended to all channels, 2026-07 nit sweep): secrets never
        // persist a plaintext copy when encryption succeeds. Every read path
        // (`config_crypto::read_encrypted_config_field` / `decrypt_config_field`,
        // dispatcher `resolve_forward_token`, reminder `decrypt_channel_token`)
        // prefers `_enc` and only uses plaintext as a legacy fallback — note it
        // treats an *empty* plaintext value as "channel removed", so the
        // plaintext key must be REMOVED, never blanked. If encryption is
        // unavailable (no keyfile) we fall back to the legacy plaintext write
        // so the channel still works.
        //
        // The former telegram/discord/line exception is gone (2026-07 MED):
        // `count_configured_channels` and `handle_channels_status` presence
        // checks are now `_enc`-aware, so ALL channels are enc-only here.
        match &encrypted_token {
            Some(enc) => {
                channels.remove(token_key); // drop any stale plaintext copy too
                channels.insert(enc_token_key, toml::Value::String(enc.clone()));
            }
            None => {
                channels.insert(
                    token_key.to_string(),
                    toml::Value::String(token.to_string()),
                );
            }
        }
        if let Some(sk) = secret_key {
            if !secret.is_empty() {
                if secret_is_plain {
                    // Identifier, not a secret — plaintext is the canonical copy.
                    channels.insert(sk.to_string(), toml::Value::String(secret.to_string()));
                } else {
                    match crate::config_crypto::encrypt_value(secret, &self.home_dir) {
                        Some(enc) => {
                            channels.remove(sk); // drop any stale plaintext copy
                            channels.insert(format!("{sk}_enc"), toml::Value::String(enc));
                        }
                        None => {
                            channels
                                .insert(sk.to_string(), toml::Value::String(secret.to_string()));
                        }
                    }
                }
            }
        }

        // ── G.6: additional global channel tokens carried in `config.*` ──
        // whatsapp_verify_token / whatsapp_app_secret / feishu_verification_token.
        // Secrets are encrypted to `_enc`; never echoed back.
        let extra_secret_fields: &[&str] = match channel_type {
            "whatsapp" => &["whatsapp_verify_token", "whatsapp_app_secret"],
            "feishu" => &["feishu_verification_token"],
            "teams" => &["teams_tenant_id"],
            "wecom" => &[
                "wecom_agent_id",
                "wecom_callback_token",
                "wecom_encoding_aes_key",
            ],
            _ => &[],
        };
        for field in extra_secret_fields {
            if let Some(v) = config_obj.get(*field).and_then(|v| v.as_str()) {
                let v = v.trim();
                if v.is_empty() {
                    channels.remove(*field);
                    channels.remove(&format!("{field}_enc"));
                    continue;
                }
                // MED-B (extended): extra *secrets* never keep a plaintext
                // copy when encryption succeeds (see the primary-token note
                // above). Identifiers (`wecom_agent_id`, `teams_tenant_id`)
                // are not secrets and keep the plaintext write. All readers
                // (whatsapp.rs / feishu.rs / msteams.rs / wecom.rs) resolve
                // via the enc-first `read_encrypted_config_field`.
                let forbid_plain = matches!(
                    *field,
                    "wecom_callback_token"
                        | "wecom_encoding_aes_key"
                        | "whatsapp_verify_token"
                        | "whatsapp_app_secret"
                        | "feishu_verification_token"
                );
                match crate::config_crypto::encrypt_value(v, &self.home_dir) {
                    Some(enc) => {
                        channels.insert(format!("{field}_enc"), toml::Value::String(enc));
                        if forbid_plain {
                            channels.remove(*field); // drop any stale plaintext copy
                        } else {
                            channels.insert((*field).to_string(), toml::Value::String(v.into()));
                        }
                    }
                    None => {
                        // No keyfile — plaintext is the only workable copy.
                        channels.insert((*field).to_string(), toml::Value::String(v.into()));
                    }
                }
            }
        }

        // Webhook/global channels (LINE/WhatsApp/Feishu) are a single endpoint and
        // can't bind a token per agent; if the user picked an agent, record it as
        // the global `default_agent` so incoming messages route to it.
        if !agent_name.is_empty() {
            if let Some(general) = table
                .entry("general")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut()
            {
                general.insert(
                    "default_agent".to_string(),
                    toml::Value::String(agent_name.to_string()),
                );
            }
        }

        // XC.2: atomic write (temp + rename), mirroring the per-agent path.
        if let Err(e) = crate::disconnect_forget::connections::mark_connecting(&self.home_dir,
            &format!("channel:{channel_type}")) {
            return WsFrame::error_response("", &e);
        }
        if let Err(e) = self.atomic_write_toml_snapshot(&config_path, &table, config_snapshot).await {
            return WsFrame::error_response("", &e);
        }

        info!(channel_type, agent = agent_name, "Channel config saved");

        // Hot-start: launch the channel bot immediately without gateway restart
        let hot_started = self.hot_start_channel(channel_type).await;

        WsFrame::ok_response(
            "",
            json!({
                "success": true,
                "type": channel_type,
                "hot_started": hot_started,
                // v1.68.0: all global transports (bots and webhooks) start
                // without a restart. `hot_started: false` means the saved
                // configuration is incomplete or the credentials failed.
                "restart_required": false,
                "not_started_reason": (!hot_started).then_some(
                    if crate::webhook_slots::is_webhook_channel(channel_type) {
                        "webhook_config_incomplete"
                    } else {
                        "check_credentials"
                    }
                ),
            }),
        )
    }
}

#[cfg(test)]
mod googlechat_import_tests {
    use super::*;
    use crate::agent_channels::e2e_gchat_tests;

    #[tokio::test]
    async fn googlechat_import_validates_candidate_and_commits_encrypted_key() {
        let home = tempfile::tempdir().unwrap();
        let fake = e2e_gchat_tests::start_fake().await;
        let email = format!("import-{}@test-project.iam.gserviceaccount.com", uuid::Uuid::new_v4().simple());
        let mut key: Value = serde_json::from_str(&e2e_gchat_tests::service_account(&fake, &email)).unwrap();
        key["project_id"] = json!("test-project");
        let raw_key = key.to_string();
        let handler = MethodHandler::new(home.path().to_path_buf()).await;
        let params = json!({"type":"googlechat", "config":{"token":raw_key,"secret":"123456789"}});
        let result = handler.handle_channels_add(params.clone()).await;
        assert!(matches!(result, WsFrame::Response { ok: true, .. }), "{result:?}");
        let stored = tokio::fs::read_to_string(home.path().join("config.toml")).await.unwrap();
        let table: toml::Table = stored.parse().unwrap();
        let channels = table["channels"].as_table().unwrap();
        assert!(channels.get("googlechat_service_account_json").is_none());
        assert!(!stored.contains("PRIVATE KEY"));
        let restored = crate::config_crypto::read_encrypted_config_field(home.path(), "channels", "googlechat_service_account_json").await.unwrap();
        assert_eq!(restored, raw_key);
        for (field, value) in [("secret", "invalid-number"), ("token", "{}"), ("connection", "invalid")] {
            let mut bad = params.clone();
            bad["config"][field] = json!(value);
            let result = handler.handle_channels_add(bad).await;
            assert!(matches!(result, WsFrame::Response { ok: false, .. }), "{field}: {result:?}");
            assert_eq!(tokio::fs::read_to_string(home.path().join("config.toml")).await.unwrap(), stored, "invalid import must not change config");
        }
        // A syntactically valid but unreachable candidate must not be
        // verified using the already installed, working key.
        key["token_uri"] = json!("http://127.0.0.1:1/token");
        let mut bad = params;
        bad["config"]["token"] = json!(key.to_string());
        assert!(matches!(handler.handle_channels_add(bad).await, WsFrame::Response { ok: false, .. }));
        assert_eq!(tokio::fs::read_to_string(home.path().join("config.toml")).await.unwrap(), stored);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn googlechat_manual_secret_reference_is_resolved_before_validation() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let fake = e2e_gchat_tests::start_fake().await;
        let raw = e2e_gchat_tests::service_account(&fake, &format!("reference-{}@test-project.iam.gserviceaccount.com", uuid::Uuid::new_v4().simple()));
        // The production resolver confines file references to <home>/secrets
        // (or an orchestrator secret root); a random file under home is denied.
        let outside = home.path().join("key.json");
        tokio::fs::write(&outside, &raw).await.unwrap();
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o600)).unwrap();
        let handler = MethodHandler::new(home.path().to_path_buf()).await;
        let refused = handler.handle_channels_add(json!({"type":"googlechat","config":{"token":format!("secret://file/{}", outside.display()),"secret":"123456789"}})).await;
        assert!(matches!(refused, WsFrame::Response { ok: false, .. }));
        let secrets = home.path().join("secrets");
        tokio::fs::create_dir(&secrets).await.unwrap();
        let key_file = secrets.join("key.json");
        tokio::fs::write(&key_file, &raw).await.unwrap();
        std::fs::set_permissions(&key_file, std::fs::Permissions::from_mode(0o600)).unwrap();
        let reference = format!("secret://file/{}", key_file.display());
        let result = handler.handle_channels_add(json!({"type":"googlechat","config":{"token":reference,"secret":"123456789"}})).await;
        assert!(matches!(result, WsFrame::Response { ok: true, .. }), "{result:?}");
        assert_eq!(crate::config_crypto::read_encrypted_config_field(home.path(), "channels", "googlechat_service_account_json").await.as_deref(), Some(raw.as_str()));
    }
}
