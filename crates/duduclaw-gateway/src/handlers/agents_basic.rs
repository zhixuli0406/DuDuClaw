//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Agents ───────────────────────────────────────────────

    pub(crate) async fn handle_agents_status(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let reg = self.registry.read().await;
        match reg.get(agent_id) {
            Some(a) => {
                let cfg = &a.config;
                WsFrame::ok_response(
                    "",
                    json!({
                        "name": cfg.agent.name,
                        "display_name": cfg.agent.display_name,
                        "status": format!("{:?}", cfg.agent.status).to_lowercase(),
                        "role": format!("{:?}", cfg.agent.role).to_lowercase(),
                    }),
                )
            }
            None => WsFrame::error_response("", &format!("Agent not found: {agent_id}")),
        }
    }

    /// WP-6F P1 — `presets.list`: every preset under `~/.duduclaw/presets/`,
    /// read-only (design §9 P1 scope excludes any switching UI). A preset
    /// that fails to parse is still listed (with an `error` field) rather
    /// than silently dropped, matching `duduclaw preset list`'s CLI twin.
    pub(crate) async fn handle_presets_list(&self) -> WsFrame {
        let ids = duduclaw_core::preset::list_presets(&self.home_dir);
        let items: Vec<Value> = ids
            .into_iter()
            .map(
                |id| match duduclaw_core::preset::load_preset(&self.home_dir, &id) {
                    Ok(p) => json!({
                        "id": id,
                        "version": p.meta.version,
                        "label": p.meta.label,
                        "description": p.meta.description,
                    }),
                    Err(e) => json!({ "id": id, "error": e.to_string() }),
                },
            )
            .collect();
        WsFrame::ok_response("", json!({ "presets": items }))
    }

    /// WP-6F P1 — `presets.status`: one agent's current preset binding +
    /// live resolution outcome + which fields its own `agent.toml` overrides
    /// (design §1 R1.4 "已覆寫"). Read-only — binding writes are CLI-only in
    /// P1 (`duduclaw preset bind`), see `preset_cmd` module docs.
    pub(crate) async fn handle_presets_status(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let reg = self.registry.read().await;
        let Some(agent) = reg.get(agent_id) else {
            return WsFrame::error_response("", &format!("Agent not found: {agent_id}"));
        };
        let resolution = match &agent.preset_resolution {
            duduclaw_core::preset::PresetResolution::Unbound => json!({ "state": "unbound" }),
            duduclaw_core::preset::PresetResolution::Applied {
                preset_id,
                version,
                label,
                changed_fields,
                ..
            } => json!({
                "state": "applied",
                "preset_id": preset_id,
                "version": version,
                "label": label,
                "changed_fields": changed_fields,
            }),
            duduclaw_core::preset::PresetResolution::Unresolved {
                preset_id,
                version,
                reason,
            } => json!({
                "state": "unresolved",
                "preset_id": preset_id,
                "version": version,
                "reason": reason,
            }),
        };
        WsFrame::ok_response(
            "",
            json!({ "agent_id": agent_id, "resolution": resolution }),
        )
    }

    /// Cloud-tier resource cap. Returns `Some(message)` when the active tier
    /// caps this resource, the deployment is Cloud (`DUDUCLAW_DEPLOYMENT=cloud`,
    /// injected into managed tenant containers), and the cap is already
    /// reached. Self-hosted deployments (Apache 2.0) are NEVER capped; a
    /// `max` of 0 in features.toml means unlimited.
    pub(crate) async fn tier_limit_message(&self, kind: &str, current: usize) -> Option<String> {
        // Personal-edition agent cap. B+C decision (2026-07-16): UNLIMITED by
        // default (`personal_max_agents()` = 0, honoring the self-host
        // promise); the dashboard shows a soft upgrade hint above the
        // recommended size instead. `DUDUCLAW_PERSONAL_MAX_AGENTS` lets a
        // managed/hosted deployment opt into a hard cap. Enterprise edition
        // falls through to the license-tier logic below.
        if kind == "agent" && self.resolve_edition_profile().await.is_personal() {
            let cap = duduclaw_core::EditionProfile::personal_max_agents();
            if crate::license_runtime::cap_exceeded(cap, current) {
                return Some(format!(
                    "個人版最多可建立 {cap} 個 AI 員工。\
                     升級企業版以建立更多，並解鎖部門、多帳號等管理功能：\
                     https://duduclaw.dudustudio.monster#pricing"
                ));
            }
            // Under the personal cap → allow, without consulting license tiers.
            return None;
        }

        let rt = crate::license_runtime::global()?;

        // P-License §7 decision (b): a signed per-license `max_agents` override is
        // the issuer's EXPLICIT intent to cap this seat count, so it overrides the
        // "self-host is never limited" default (an OEM/self-host distributor who
        // sells "system + N agents" must actually be held to N). Only the agent
        // cap participates in this override — channels keep the pure self-host
        // exemption.
        let agent_override = kind == "agent" && rt.snapshot().await.max_agents.is_some();

        // Apache 2.0 promise: never limit self-host. Default deployment is
        // self-host, so the limit only ever bites managed Cloud tenants — UNLESS
        // an explicit signed agent-count override says otherwise (§7(b)). Channels
        // keep the pure self-host exemption.
        let is_self_host = crate::license_runtime::is_self_host_deployment();
        let enforce = match kind {
            "agent" => crate::license_runtime::agent_cap_enforced(is_self_host, agent_override),
            _ => !is_self_host,
        };
        if !enforce {
            return None;
        }
        let tier = rt.current_tier().await;
        let max = match kind {
            // Uses the effective (override > tier) limit so a per-license quota
            // actually bites, not just the tier default.
            "agent" => rt.effective_max_agents().await,
            "channel" => rt.feature_gate().max_channels(tier),
            _ => 0,
        };
        if !crate::license_runtime::cap_exceeded(max, current) {
            return None;
        }
        let noun = if kind == "agent" { "Agent" } else { "通道" };
        Some(format!(
            "您的方案（{tier}）最多可建立 {max} 個{noun}。\
             請升級方案以新增更多：https://duduclaw.dudustudio.monster#pricing"
        ))
    }

    /// Count configured channels across global config.toml + every agent's
    /// `[channels]` section. Mirrors `handle_channels_status`'s enumeration so
    /// the cap counts exactly what the dashboard shows.
    pub(crate) async fn count_configured_channels(&self) -> usize {
        let mut n = 0usize;
        let config_path = self.home_dir.join("config.toml");
        if let Ok(content) = tokio::fs::read_to_string(&config_path).await
            && let Ok(config) = content.parse::<toml::Table>()
            && let Some(ch) = config.get("channels").and_then(|v| v.as_table())
        {
            for key in [
                "line_channel_token",
                "telegram_bot_token",
                "discord_bot_token",
            ] {
                // Presence = plaintext OR `_enc` (2026-07 MED: channels.add is
                // now enc-only for these too; a plaintext-only check would
                // undercount freshly-saved channels).
                let plain = ch
                    .get(key)
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| !s.is_empty());
                let enc = ch
                    .get(&format!("{key}_enc"))
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| !s.is_empty());
                if plain || enc {
                    n += 1;
                }
            }
        }
        let reg = self.registry.read().await;
        for agent in reg.list() {
            if let Some(ch) = &agent.config.channels {
                if ch.discord.as_ref().is_some_and(|d| {
                    !d.bot_token.is_empty()
                        || d.bot_token_enc.as_ref().is_some_and(|e| !e.is_empty())
                }) {
                    n += 1;
                }
                if ch.telegram.as_ref().is_some_and(|t| {
                    !t.bot_token.is_empty()
                        || t.bot_token_enc.as_ref().is_some_and(|e| !e.is_empty())
                }) {
                    n += 1;
                }
                if ch.slack.as_ref().is_some_and(|s| {
                    !s.bot_token.is_empty()
                        || s.bot_token_enc.as_ref().is_some_and(|e| !e.is_empty())
                }) {
                    n += 1;
                }
            }
        }
        n
    }
}
