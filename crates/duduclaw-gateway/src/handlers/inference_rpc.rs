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
    /// `models_dir`/`default_model`/`auto_load`/`max_memory_mb`), `generation`,
    /// `router` (validates `strong_threshold < fast_threshold`), `openai_compat`
    /// (`base_url`/`model`/`api_key` → encrypted to `api_key_enc`), and the
    /// `llamafile`/`embedding` sub-sections (generic pass-through).
    /// Response: `{ success, changes[] }`.
    pub(crate) async fn handle_inference_update(&self, params: Value) -> WsFrame {
        let path = self.home_dir.join("inference.toml");
        let mut table = self.read_config_table(&path).await;

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
            if api_key != SECRET_MASK_SET {
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
        info!(?changes, "inference.update completed");
        WsFrame::ok_response("", json!({ "success": true, "changes": changes }))
    }
}
