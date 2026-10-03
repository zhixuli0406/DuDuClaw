//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── SKS: global [skill_synthesis] in config.toml (W19-P1) ─────────────────

    /// `skill_synthesis.get` — read config.toml `[skill_synthesis]`.
    /// Response: `{ auto_run, dry_run, interval_hours, lookback_days, target_agent }`.
    pub(crate) async fn handle_skill_synthesis_get(&self) -> WsFrame {
        let config_path = self.home_dir.join("config.toml");
        let table = self.read_config_table(&config_path).await;
        WsFrame::ok_response("", skill_synthesis_table_to_response(&table))
    }

    /// `skill_synthesis.update` — atomic write of config.toml `[skill_synthesis]`.
    /// Params (all optional, partial update): `{ auto_run, dry_run,
    /// interval_hours (>=1), lookback_days (1-30), target_agent (empty clears) }`.
    /// Takes effect within one scheduler poll (~30 min) — no restart needed.
    /// Response: `{ success, changes[] }`.
    pub(crate) async fn handle_skill_synthesis_update(&self, params: Value) -> WsFrame {
        let config_path = self.home_dir.join("config.toml");
        let mut table = self.read_config_table(&config_path).await;
        let changes = match apply_skill_synthesis_to_table(&mut table, &params) {
            Ok(c) => c,
            Err(e) => return WsFrame::error_response("", &e),
        };
        if changes.is_empty() {
            return WsFrame::error_response("", "No valid skill_synthesis fields to update");
        }
        if let Err(e) = self.atomic_write_toml(&config_path, &table).await {
            return WsFrame::error_response("", &e);
        }
        info!(?changes, "skill_synthesis.update completed");
        WsFrame::ok_response("", json!({ "success": true, "changes": changes }))
    }

    // ── INF: global inference.toml (INF.1–INF.5) ──────────────────────────────

    /// `inference.get` — read `~/.duduclaw/inference.toml` into structured JSON.
    /// The `[openai_compat].api_key` secret is MASKED: the response carries
    /// `openai_compat.api_key_set` (bool) + `openai_compat.api_key` = "***set***"
    /// (or "") — NEVER the cleartext / encrypted value.
    pub(crate) async fn handle_inference_get(&self) -> WsFrame {
        let path = self.home_dir.join("inference.toml");
        let table = self.read_config_table(&path).await;
        WsFrame::ok_response("", inference_table_to_response(&table))
    }

    /// `inference.update` — atomic write of `~/.duduclaw/inference.toml`.
    /// Params (all optional, partial update): root (`enabled`/`backend`/
    /// `models_dir`/`default_model`/`auto_load`), `generation`
    /// (`max_tokens`/`temperature`/`top_p`/`stop`/`capture_logprobs`/
    /// `capture_top_logprobs`), `router` (validates `strong_threshold <
    /// fast_threshold`; plus `local_tools` and the `ucci_*` keys),
    /// `openai_compat` (`base_url`/`model`/`api_key` → encrypted to
    /// `api_key_enc`) and typed `llamafile`. Response: `{ success, changes[],
    /// engine_reset: true, restart_required: [] }`.
    pub(crate) async fn handle_inference_update(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let path = self.home_dir.join("inference.toml");
        let mut table = self.read_config_table(&path).await;
        let before = table.clone();

        // The stored API key only follows the endpoint it was entered for.
        if let Some(oc) = params.get("openai_compat").and_then(|v| v.as_object()) {
            let new_url = oc.get("base_url").and_then(|v| v.as_str()).map(str::trim).unwrap_or("");
            let stored = table.get("openai_compat").and_then(|v| v.as_table());
            let url_changed = !new_url.is_empty()
                && stored.and_then(|t| t.get("base_url")).and_then(|v| v.as_str()) != Some(new_url);
            let has_key = stored.is_some_and(|t| t.contains_key("api_key_enc") || t.contains_key("api_key"));
            let key_kept = oc
                .get("api_key")
                .and_then(|v| v.as_str())
                .is_none_or(|k| super::config_commit::is_secret_placeholder(k));
            if url_changed && has_key && key_kept {
                return WsFrame::error_response(
                    "",
                    "openai_compat.base_url changed — re-enter openai_compat.api_key for the new endpoint (or send \"\" to clear it)",
                );
            }
        }

        // Pure validation + field application (no secret handling).
        let mut changes = match apply_inference_to_table(&mut table, &params) {
            Ok(c) => c,
            Err(e) => return WsFrame::error_response("", &e),
        };

        // ── openai_compat.api_key secret: encrypt → `api_key_enc`, never store
        // cleartext. An empty string clears the secret. (INF.5 / INF.8)
        if let Some(api_key) = params
            .get("openai_compat")
            .and_then(|v| v.as_object())
            .and_then(|oc| oc.get("api_key"))
            .and_then(|v| v.as_str())
        {
            // Refuse to persist the masked placeholder back as a real secret —
            // the dashboard echoes it when the field was left untouched.
            if !super::config_commit::is_secret_placeholder(api_key) {
                let section = match table
                    .entry("openai_compat")
                    .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                    .as_table_mut()
                {
                    Some(s) => s,
                    None => return WsFrame::error_response("", "Invalid [openai_compat] section"),
                };
                // Never keep a cleartext `api_key` on disk.
                section.remove("api_key");
                if api_key.is_empty() {
                    section.remove("api_key_enc");
                    changes.push("openai_compat.api_key cleared".to_string());
                } else if let Some(enc) =
                    crate::config_crypto::encrypt_value(api_key, &self.home_dir)
                {
                    section.insert("api_key_enc".into(), toml::Value::String(enc));
                    changes.push("openai_compat.api_key = [ENCRYPTED]".to_string());
                } else {
                    return WsFrame::error_response("", "Failed to encrypt openai_compat.api_key");
                }
            }
        }

        if changes.is_empty() {
            return WsFrame::error_response("", "No valid inference fields to update");
        }
        if let Err(e) = self.atomic_write_toml(&path, &table).await {
            return WsFrame::error_response("", &e);
        }
        // v1.68: the channel-reply / dispatch path caches the inference
        // engine for the life of the process (`claude_runner`), so without
        // this reset every saved setting was ignored until a restart. The
        // next local-inference call rebuilds it from the file just written.
        crate::claude_runner::reset_inference_engine().await;
        // Paths / argv / bind address the local inference server runs with.
        for key in ["llamafile.dir", "llamafile.default_file", "llamafile.extra_args", "llamafile.host", "router.ucci_observations"] {
            let (b, a) = (
                super::config_commit::toml_at_json(&before, key),
                super::config_commit::toml_at_json(&table, key),
            );
            if b == a {
                continue;
            }
            if key == "llamafile.host"
                && a.as_str().is_some_and(|h| h == "localhost" || h.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback()))
            {
                continue;
            }
            crate::security_autopilot::audit_and_emit(
                &self.home_dir,
                &duduclaw_security::audit::AuditEvent::new(
                    "config_protected_key_changed",
                    ctx.user_id.as_str(),
                    duduclaw_security::audit::Severity::Warning,
                    json!({ "key": key, "before": b, "after": a, "file": "inference", "user_id": ctx.user_id, "source": "inference.update" }),
                ),
            );
        }
        info!(?changes, "inference.update completed");
        WsFrame::ok_response(
            "",
            json!({ "success": true, "changes": changes, "engine_reset": true, "restart_required": [] }),
        )
    }
}
