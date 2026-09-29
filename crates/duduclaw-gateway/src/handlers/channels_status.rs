//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Channels ─────────────────────────────────────────────

    pub(crate) async fn handle_channels_status(&self) -> WsFrame {
        let config_path = self.home_dir.join("config.toml");
        let runtime_status = self.channel_status.read().await;
        let mut channels = Vec::new();

        if let Ok(content) = tokio::fs::read_to_string(&config_path).await
            && let Ok(config) = content.parse::<toml::Table>()
            && let Some(ch) = config.get("channels").and_then(|v| v.as_table())
        {
            let token_map = [
                ("line_channel_token", "line"),
                ("telegram_bot_token", "telegram"),
                ("discord_bot_token", "discord"),
            ];
            for (key, name) in token_map {
                // Enc-aware presence (2026-07 MED) — matches
                // `count_configured_channels` so enc-only saves show up here.
                let configured = ch
                    .get(key)
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| !s.is_empty())
                    || ch
                        .get(&format!("{key}_enc"))
                        .and_then(|v| v.as_str())
                        .is_some_and(|s| !s.is_empty());
                if configured {
                    // Use runtime state if available; otherwise use "connecting" status
                    let (connected, last_ts, error) = match runtime_status.get(name) {
                        Some(state) => (
                            state.connected,
                            state.last_event.as_ref().map(|t| t.to_rfc3339()),
                            state.error.clone(),
                        ),
                        None => (false, None, Some("connecting".to_string())),
                    };
                    channels.push(json!({
                        "name": name,
                        "connected": connected,
                        "last_connected": last_ts,
                        "error": error,
                    }));
                }
            }
        }

        // Include per-agent channels from agent registry configs
        let mut seen_labels = std::collections::HashSet::new();
        {
            let reg = self.registry.read().await;
            for agent in reg.list() {
                if let Some(ch) = &agent.config.channels {
                    let name = &agent.config.agent.name;
                    let pairs: &[(&str, bool)] = &[
                        (
                            "discord",
                            ch.discord.as_ref().is_some_and(|d| {
                                !d.bot_token.is_empty()
                                    || d.bot_token_enc.as_ref().is_some_and(|e| !e.is_empty())
                            }),
                        ),
                        (
                            "telegram",
                            ch.telegram.as_ref().is_some_and(|t| {
                                !t.bot_token.is_empty()
                                    || t.bot_token_enc.as_ref().is_some_and(|e| !e.is_empty())
                            }),
                        ),
                        (
                            "slack",
                            ch.slack.as_ref().is_some_and(|s| {
                                !s.bot_token.is_empty()
                                    || s.bot_token_enc.as_ref().is_some_and(|e| !e.is_empty())
                            }),
                        ),
                    ];
                    for &(platform, configured) in pairs {
                        if configured {
                            let label = format!("{platform}:{name}");
                            seen_labels.insert(label.clone());
                            let (connected, last_ts, error) = match runtime_status.get(&label) {
                                Some(state) => (
                                    state.connected,
                                    state.last_event.as_ref().map(|t| t.to_rfc3339()),
                                    state.error.clone(),
                                ),
                                None => (false, None, Some("connecting".to_string())),
                            };
                            channels.push(json!({
                                "name": label,
                                "connected": connected,
                                "last_connected": last_ts,
                                "error": error,
                            }));
                        }
                    }
                }
            }
        }

        // Also include runtime-only per-agent entries not yet in registry (edge case)
        for (key, state) in runtime_status.iter() {
            if key.contains(':') && !seen_labels.contains(key.as_str()) {
                channels.push(json!({
                    "name": key,
                    "connected": state.connected,
                    "last_connected": state.last_event.as_ref().map(|t| t.to_rfc3339()),
                    "error": state.error.clone(),
                }));
            }
        }

        WsFrame::ok_response("", json!({ "channels": channels }))
    }
}
