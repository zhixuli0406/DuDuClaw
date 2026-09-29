//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    /// `task_forward_model.get` — the three global v1.54 evolution switches.
    ///
    /// Reads through `TaskForwardModelConfig::from_home` (the same loader the
    /// engine uses), so the values returned are exactly what the runtime would
    /// observe — a missing / malformed `[task_forward_model]` section resolves
    /// to the struct default rather than a hand-copied "default" that could
    /// drift out of sync with actual behavior.
    pub(crate) async fn handle_task_forward_model_get(&self) -> WsFrame {
        let cfg = crate::prediction::task_forward_store::TaskForwardModelConfig::from_home(
            &self.home_dir,
        );
        WsFrame::ok_response(
            "",
            json!({
                "enabled": cfg.enabled,
                "calibration_enabled": cfg.calibration_enabled,
                "held_out_gate_enabled": cfg.held_out_gate_enabled,
            }),
        )
    }

    /// `task_forward_model.set` — partial update of the three global switches.
    ///
    /// Only the keys present in `params` are written; every other key in
    /// `[task_forward_model]` and every other config section is preserved
    /// (edit-in-place `toml::Table`, never a whole-file rewrite). Fail-closed:
    /// a non-boolean value or an existing-but-non-table `[task_forward_model]`
    /// section rejects the whole payload rather than corrupting the file.
    /// Returns the full post-write value set.
    pub(crate) async fn handle_task_forward_model_set(&self, params: Value, ctx: &UserContext) -> WsFrame {
        // Accept only these three keys; ignore anything else so a future
        // sub-field can't be smuggled in through this narrow toggle RPC.
        const KEYS: [&str; 3] = ["enabled", "calibration_enabled", "held_out_gate_enabled"];

        // Parse each provided key as a strict boolean. Absent ⇒ leave as-is.
        let mut updates: Vec<(&str, bool)> = Vec::new();
        for key in KEYS {
            match params.get(key) {
                None => {}
                Some(v) => match v.as_bool() {
                    Some(b) => updates.push((key, b)),
                    None => {
                        return WsFrame::error_response(
                            "",
                            &format!("設定「{key}」必須是開或關(true / false)。"),
                        );
                    }
                },
            }
        }
        if updates.is_empty() {
            return WsFrame::error_response("", "沒有要更新的項目。");
        }

        let before = crate::prediction::task_forward_store::TaskForwardModelConfig::from_home(
            &self.home_dir,
        );

        // ── persist ──
        let config_path = self.home_dir.join("config.toml");
        let mut table = self.read_config_table(&config_path).await;

        // An existing-but-non-table section is operator data we refuse to
        // silently destroy; report it instead of overwriting.
        if table
            .get("task_forward_model")
            .is_some_and(|v| v.as_table().is_none())
        {
            return WsFrame::error_response(
                "",
                "config.toml 的 [task_forward_model] 區段格式有誤,請先修正設定檔後再儲存。",
            );
        }
        let section = table
            .entry("task_forward_model")
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
            .as_table_mut()
            .expect("task_forward_model section verified as a table above");

        for (key, val) in &updates {
            section.insert((*key).to_string(), toml::Value::Boolean(*val));
        }

        // Atomic write: temp + rename (same discipline as delegation.set).
        let tmp_path = config_path.with_extension("toml.tmp");
        if let Err(e) = self.write_config_table(&tmp_path, &table).await {
            return WsFrame::error_response("", &format!("寫入設定失敗:{e}"));
        }
        if let Err(e) = tokio::fs::rename(&tmp_path, &config_path).await {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return WsFrame::error_response("", &format!("儲存設定失敗:{e}"));
        }

        let after = crate::prediction::task_forward_store::TaskForwardModelConfig::from_home(
            &self.home_dir,
        );

        // Audit: who changed the global evolution switches, from what to what.
        crate::security_autopilot::audit_and_emit(
            &self.home_dir,
            &duduclaw_security::audit::AuditEvent::new(
                "task_forward_model_config_changed",
                "dashboard",
                duduclaw_security::audit::Severity::Warning,
                json!({
                    "actor_user_id": ctx.user_id,
                    "actor_email": ctx.email,
                    "actor_role": format!("{:?}", ctx.role).to_lowercase(),
                    "before": {
                        "enabled": before.enabled,
                        "calibration_enabled": before.calibration_enabled,
                        "held_out_gate_enabled": before.held_out_gate_enabled,
                    },
                    "after": {
                        "enabled": after.enabled,
                        "calibration_enabled": after.calibration_enabled,
                        "held_out_gate_enabled": after.held_out_gate_enabled,
                    },
                }),
            ),
        );

        info!(
            actor = %ctx.email,
            enabled = after.enabled,
            calibration_enabled = after.calibration_enabled,
            held_out_gate_enabled = after.held_out_gate_enabled,
            "task_forward_model.set committed"
        );

        // Effect timing: calibration/held-out are re-read per task settle, so
        // they apply on the next task with no restart. The `enabled` master
        // switch also gates a predict hook built once at gateway startup, so a
        // change to it only fully takes effect after a restart. Surface that
        // caveat only when `enabled` actually changed.
        let enabled_changed = before.enabled != after.enabled;
        WsFrame::ok_response(
            "",
            json!({
                "success": true,
                "enabled": after.enabled,
                "calibration_enabled": after.calibration_enabled,
                "held_out_gate_enabled": after.held_out_gate_enabled,
                "enabled_requires_restart": enabled_changed,
            }),
        )
    }
}
