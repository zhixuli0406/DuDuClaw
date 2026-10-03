//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    pub(crate) async fn handle_channels_add(&self, params: Value) -> WsFrame {
        let channel_type = match params.get("type").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => return WsFrame::error_response("", "Missing 'type' parameter"),
        };
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
                    // Keyfile-unavailable falls back to legacy plaintext.
                    match crate::config_crypto::encrypt_value(&token_owned, &home) {
                        Some(enc) => {
                            section.remove(token_field); // drop any stale plaintext copy
                            section.insert(format!("{token_field}_enc"), toml::Value::String(enc));
                        }
                        None => {
                            section.insert(
                                token_field.to_string(),
                                toml::Value::String(token_owned.clone()),
                            );
                        }
                    }
                    if let Some(sf) = secret_field {
                        if !secret_owned.is_empty() {
                            match crate::config_crypto::encrypt_value(&secret_owned, &home) {
                                Some(enc) => {
                                    section.remove(sf); // drop any stale plaintext copy
                                    section.insert(format!("{sf}_enc"), toml::Value::String(enc));
                                }
                                None => {
                                    section.insert(
                                        sf.to_string(),
                                        toml::Value::String(secret_owned.clone()),
                                    );
                                }
                            }
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
            if let Some(ctx) = self.reply_ctx.read().await.clone() {
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
        let mut table = self.read_config_table(&config_path).await;

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
        if let Err(e) = self.atomic_write_toml(&config_path, &table).await {
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
