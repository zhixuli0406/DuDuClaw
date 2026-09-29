//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── RED: global [redaction] in config.toml ────────────────────────────────

    /// `redaction.get` — read config.toml `[redaction]`.
    /// Response: `{ enabled, vault_ttl_hours, purge_after_expire_days, profiles[],
    /// sources{...}, tool_egress{...} }`.
    pub(crate) async fn handle_redaction_get(&self) -> WsFrame {
        let config_path = self.home_dir.join("config.toml");
        let table = self.read_config_table(&config_path).await;
        let poison = self.get_redaction_poison().await;
        let mut resp = redaction_table_to_response(&table);
        if let Some(obj) = resp.as_object_mut() {
            // Field-picker catalogue: which profiles exist and which PII
            // categories (fields) each one covers.
            obj.insert(
                "available_profiles".into(),
                Value::Array(redaction_available_profiles(&self.home_dir)),
            );
            // §11.2: the editable structured-field rules (db_field / json_path
            // only — regex / keyword / identity rules stay TOML-only and are
            // never listed here, so the editor cannot touch them).
            obj.insert(
                "field_rules".into(),
                Value::Array(redaction_field_rules(&table)),
            );
            // §13.5: the data-source registry a `db_field` rule's `source`
            // may name — the built-ins plus the operator's own entries.
            obj.insert(
                "data_sources".into(),
                Value::Array(redaction_data_sources(&table)),
            );
            // §14.4: the data-file guard mode the spawn sites will apply.
            obj.insert(
                "data_file_guard".into(),
                Value::String(redaction_data_file_guard(&table)),
            );
            // §13.2: category id → the name a human sees, merged across every
            // profile currently listed in `[redaction] profiles`. The frontend
            // falls back to `redaction.cat.<id>` i18n and then the raw id.
            obj.insert(
                "category_labels".into(),
                json!(crate::redaction_custom_rules::merged_category_labels(
                    &self.home_dir,
                    &table
                )),
            );
            // §12: poison state — `null` when redaction resolved cleanly.
            obj.insert("poisoned".into(), poison_wire(poison.as_ref()));
        }
        WsFrame::ok_response("", resp)
    }

    /// `redaction.update` — atomic write of config.toml `[redaction]`.
    /// Params (all optional, partial update): `{ enabled, vault_ttl_hours,
    /// purge_after_expire_days, profiles[], sources{user_input|tool_results|
    /// system_prompt|sub_agent|cron_context = on|off|selective|inherit},
    /// tool_egress{<tool>: {restore_args: restore|passthrough|deny, audit_reveal}
    /// | null}, field_rules{<id>: <db_field|json_path rule> | null},
    /// data_file_guard: on|read_only|off }`.
    /// Response: `{ success, changes[], applied, warning }`.
    pub(crate) async fn handle_redaction_update(&self, params: Value) -> WsFrame {
        let config_path = self.home_dir.join("config.toml");
        let mut table = self.read_config_table(&config_path).await;
        let mut changes = match apply_redaction_to_table(&mut table, &params) {
            Ok(c) => c,
            Err(e) => return WsFrame::error_response("", &e),
        };
        // §14.4 (WP-F2) — applied here, before the "nothing to do" check, so a
        // payload carrying only `data_file_guard` is a valid update.
        if let Err(e) = apply_data_file_guard_to_table(&mut table, &params, &mut changes) {
            return WsFrame::error_response("", &e);
        }
        if changes.is_empty() {
            return WsFrame::error_response("", "No valid redaction fields to update");
        }
        // §11.2 dry-compile: a field-rule edit must prove the WHOLE resolved
        // rule set still compiles (profiles + every inline rule, merge applied)
        // before anything touches the disk. A bad path expression / unknown
        // source otherwise lands in config.toml and poisons the next boot.
        // §13.5: a data-source edit is the same hazard from the other end (a
        // rule that compiled yesterday stops compiling when its source
        // changes), so it goes through the identical gate.
        if (params.get("field_rules").is_some() || params.get("data_sources").is_some())
            && let Err(e) = dry_compile_redaction_table(&table, &self.home_dir)
        {
            return WsFrame::error_response("", &format!("規則試編失敗，未寫入：{e}"));
        }
        if let Err(e) = self.atomic_write_toml(&config_path, &table).await {
            return WsFrame::error_response("", &e);
        }

        let (applied, warning) = self.apply_redaction_hot_reload(&table).await;

        info!(?changes, applied, "redaction.update completed");
        WsFrame::ok_response(
            "",
            json!({ "success": true, "changes": changes, "applied": applied, "warning": warning }),
        )
    }

    /// Rebuild the live redaction pipeline from a just-written config table.
    ///
    /// Extracted from `redaction.update` so every writer of redaction state —
    /// the settings form, the custom-rules card, the rule-pack importer —
    /// goes through ONE reload with one poison-recovery story. A rebuild
    /// failure leaves the previous live manager untouched and is reported
    /// honestly in the returned warning.
    ///
    /// §12 recovery: a successful rebuild clears the poison state (and
    /// announces it once); a failure while poisoned keeps it, refreshing the
    /// reason so the banner names the *current* cause. A failure while NOT
    /// poisoned does not newly poison — the live manager is untouched and the
    /// caller's response already carries the warning.
    pub(crate) async fn apply_redaction_hot_reload(&self, table: &toml::Table) -> (bool, Option<String>) {
        let parsed: Option<duduclaw_redaction::RedactionConfig> =
            toml::to_string(table).ok().and_then(|s| {
                #[derive(serde::Deserialize)]
                struct Wrap {
                    #[serde(default)]
                    redaction: duduclaw_redaction::RedactionConfig,
                }
                toml::from_str::<Wrap>(&s).ok().map(|w| w.redaction)
            });
        let was_poisoned = self.get_redaction_poison().await;
        let mut recovered = false;
        let (applied, warning) = match parsed {
            Some(rcfg) if rcfg.enabled => {
                match crate::redaction_integration::build_manager_from_home(&self.home_dir, rcfg) {
                    Ok(m) => {
                        info!(rules = m.engine().rule_count(), "redaction hot-reloaded");
                        self.swap_redaction_manager(Some(m)).await;
                        if was_poisoned.is_some() {
                            self.set_redaction_poison(None).await;
                            recovered = true;
                        }
                        (true, None)
                    }
                    Err(e) => {
                        warn!(error = %e, "redaction config saved but hot reload FAILED — live pipeline unchanged");
                        if was_poisoned.is_some() {
                            self.set_redaction_poison(Some(RedactionPoison::new(
                                crate::redaction_integration::poison_reason(e.to_string()),
                            )))
                            .await;
                        }
                        (
                            false,
                            Some(format!(
                                "設定已儲存，但即時套用失敗（目前仍沿用變更前的規則）：{e}"
                            )),
                        )
                    }
                }
            }
            Some(_) => {
                self.swap_redaction_manager(None).await;
                if was_poisoned.is_some() {
                    self.set_redaction_poison(None).await;
                    recovered = true;
                }
                (true, None)
            }
            None => (
                false,
                Some("設定已儲存，但無法解析新設定以即時套用，請重啟 gateway".to_string()),
            ),
        };

        if recovered {
            info!("redaction poison state cleared by redaction hot reload");
            crate::redaction_integration::post_redaction_activity(
                &self.home_dir,
                "redaction_recovered",
                "去識別化保護已恢復：設定更新後成功套用。",
            )
            .await;
        }
        (applied, warning)
    }

    // ── RED §13: 「我的規則」custom rules + imported rule packs ───────────────

    /// Make sure a custom profile file is listed in `[redaction] profiles`,
    /// then hot-reload the live pipeline so the just-written rules apply.
    ///
    /// Listing only happens when the file actually EXISTS: listing a profile
    /// with no file behind it is a load error (`profile '<name>' not found`)
    /// that would poison the pipeline — which is exactly what a `remove` of
    /// the last rule, or of a rule that was never there, would otherwise do.
    pub(crate) async fn ensure_custom_profile_active(
        &self,
        name: &str,
    ) -> Result<(bool, Option<String>), String> {
        let config_path = self.home_dir.join("config.toml");
        let mut table = self.read_config_table(&config_path).await;
        if crate::redaction_custom_rules::profile_path(&self.home_dir, name).exists()
            && crate::redaction_custom_rules::ensure_profile_listed(&mut table, name)
        {
            self.atomic_write_toml(&config_path, &table).await?;
        }
        Ok(self.apply_redaction_hot_reload(&table).await)
    }

    /// `redaction.custom_rules.list` → `{ rules: [...] }` (§13.2).
    pub(crate) async fn handle_redaction_custom_rules_list(&self) -> WsFrame {
        match crate::redaction_custom_rules::list_rules(&self.home_dir) {
            Ok(rules) => WsFrame::ok_response("", json!({ "rules": rules })),
            // Fail closed: an unreadable custom.toml is an error the operator
            // must see, never an empty list that reads as "you have no rules".
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    /// `redaction.custom_rules.upsert` — create or edit one rule (§13.2).
    pub(crate) async fn handle_redaction_custom_rules_upsert(&self, params: Value) -> WsFrame {
        let input = match crate::redaction_custom_rules::parse_upsert(&params) {
            Ok(i) => i,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let rule = match crate::redaction_custom_rules::upsert_rule(&self.home_dir, &input) {
            Ok(r) => r,
            Err(e) => return WsFrame::error_response("", &e),
        };
        self.finish_custom_rule_write(rule).await
    }

    /// Rebuild the live pipeline if an install / removal just changed whether
    /// a `type = "ner"` rule can compile.
    ///
    /// The download runs in a detached task that has no `&self`, so the flag
    /// it sets is consumed here, on the next `redaction.model.*` call. The
    /// card polls `status` every two seconds while a download runs, so the
    /// reload lands within one poll of the download finishing.
    pub(crate) async fn reload_redaction_if_model_changed(&self) -> Option<String> {
        if !crate::redaction_ner_model::take_pending_reload() {
            return None;
        }
        let config_path = self.home_dir.join("config.toml");
        let table = self.read_config_table(&config_path).await;
        let (_applied, warning) = self.apply_redaction_hot_reload(&table).await;
        warning
    }

    /// `redaction.model.status` — install state, progress and live latency
    /// telemetry for the NER model (§13.4).
    pub(crate) async fn handle_redaction_model_status(&self) -> WsFrame {
        let warning = self.reload_redaction_if_model_changed().await;
        let mut resp = crate::redaction_ner_model::status(&self.home_dir);
        if let (Some(obj), Some(w)) = (resp.as_object_mut(), warning) {
            obj.insert("warning".into(), Value::String(w));
        }
        WsFrame::ok_response("", resp)
    }

    /// `redaction.model.install` — start the background download. Idempotent.
    pub(crate) async fn handle_redaction_model_install(&self) -> WsFrame {
        match crate::redaction_ner_model::install_start(&self.home_dir) {
            Ok(v) => WsFrame::ok_response("", v),
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    /// `redaction.model.cancel` — stop a running download, keeping the
    /// partial files so the next attempt resumes.
    pub(crate) async fn handle_redaction_model_cancel(&self) -> WsFrame {
        match crate::redaction_ner_model::cancel() {
            Ok(v) => WsFrame::ok_response("", v),
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    /// `redaction.model.remove` — delete the model files (not the ONNX
    /// Runtime library) and rebuild the pipeline, which will now refuse any
    /// active `ner` rule rather than pretend it is working.
    pub(crate) async fn handle_redaction_model_remove(&self) -> WsFrame {
        match crate::redaction_ner_model::remove(&self.home_dir) {
            Ok(mut v) => {
                if let (Some(obj), Some(w)) = (
                    v.as_object_mut(),
                    self.reload_redaction_if_model_changed().await,
                ) {
                    obj.insert("warning".into(), Value::String(w));
                }
                WsFrame::ok_response("", v)
            }
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    /// `redaction.custom_rules.remove` → `{ ok }` (§13.2).
    pub(crate) async fn handle_redaction_custom_rules_remove(&self, params: Value) -> WsFrame {
        let Some(id) = params.get("id").and_then(|v| v.as_str()).map(str::trim) else {
            return WsFrame::error_response("", "Missing 'id' parameter");
        };
        let removed = match crate::redaction_custom_rules::remove_rule(&self.home_dir, id) {
            Ok(r) => r,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let (applied, warning) = match self
            .ensure_custom_profile_active(crate::redaction_custom_rules::CUSTOM_PROFILE)
            .await
        {
            Ok(v) => v,
            Err(e) => return WsFrame::error_response("", &e),
        };
        WsFrame::ok_response(
            "",
            json!({ "ok": true, "removed": removed, "applied": applied, "warning": warning }),
        )
    }

    /// `redaction.custom_rules.set_enabled` — toggle one rule (§13.2).
    pub(crate) async fn handle_redaction_custom_rules_set_enabled(&self, params: Value) -> WsFrame {
        let Some(id) = params.get("id").and_then(|v| v.as_str()).map(str::trim) else {
            return WsFrame::error_response("", "Missing 'id' parameter");
        };
        let Some(enabled) = params.get("enabled").and_then(|v| v.as_bool()) else {
            return WsFrame::error_response("", "Missing 'enabled' parameter");
        };
        let rule =
            match crate::redaction_custom_rules::set_rule_enabled(&self.home_dir, id, enabled) {
                Ok(r) => r,
                Err(e) => return WsFrame::error_response("", &e),
            };
        self.finish_custom_rule_write(rule).await
    }

    /// Shared tail for `upsert` / `set_enabled`: make sure the profile is
    /// listed, hot-reload, answer with the single rule row plus the reload
    /// verdict.
    pub(crate) async fn finish_custom_rule_write(&self, rule: Value) -> WsFrame {
        let (applied, warning) = match self
            .ensure_custom_profile_active(crate::redaction_custom_rules::CUSTOM_PROFILE)
            .await
        {
            Ok(v) => v,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let mut resp = rule;
        if let Some(obj) = resp.as_object_mut() {
            obj.insert("applied".into(), json!(applied));
            obj.insert("warning".into(), json!(warning));
        }
        WsFrame::ok_response("", resp)
    }

    /// `redaction.suggest_pattern` — examples → regex (§13.2).
    ///
    /// Engine order: local inference → cloud utility model → heuristic. The
    /// example VALUES never reach a log or the audit trail; only counts and
    /// the chosen engine are recorded.
    pub(crate) async fn handle_redaction_suggest_pattern(&self, params: Value, caller: &str) -> WsFrame {
        let input = match crate::redaction_custom_rules::parse_suggest(&params) {
            Ok(i) => i,
            Err(e) => return WsFrame::error_response("", &e),
        };
        if !suggest_pattern_limiter()
            .check_and_record(&format!("redaction.suggest_pattern:{caller}"))
            .await
        {
            return WsFrame::error_response(
                "",
                "產生樣式的次數太頻繁，請稍候再試（每分鐘最多 10 次）",
            );
        }
        let out = crate::redaction_custom_rules::suggest_pattern(&self.home_dir, &input).await;
        info!(
            examples = input.examples.len(),
            counter_examples = input.counter_examples.len(),
            engine = out.get("engine").and_then(|v| v.as_str()).unwrap_or("?"),
            all_ok = out.get("all_ok").and_then(|v| v.as_bool()).unwrap_or(false),
            "redaction.suggest_pattern completed"
        );
        WsFrame::ok_response("", out)
    }

    /// `redaction.profiles.import` — bring a TOML rule pack in as a second
    /// custom profile (§13.2). `dry_run` reports without writing.
    pub(crate) async fn handle_redaction_profiles_import(&self, params: Value) -> WsFrame {
        let Some(source) = params.get("toml").and_then(|v| v.as_str()) else {
            return WsFrame::error_response("", "Missing 'toml' parameter");
        };
        let requested = params.get("name").and_then(|v| v.as_str());
        let dry_run = params
            .get("dry_run")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Compile imported rules against the SAME engine options the live
        // manager resolves, so a rule that would poison the next reload is
        // skipped here rather than written to disk.
        let config_path = self.home_dir.join("config.toml");
        let table = self.read_config_table(&config_path).await;
        let options = match redaction_engine_options(&table, &self.home_dir) {
            Ok(o) => o,
            Err(e) => return WsFrame::error_response("", &e),
        };

        let report = match crate::redaction_custom_rules::import_profile(
            &self.home_dir,
            source,
            requested,
            dry_run,
            &options,
        ) {
            Ok(r) => r,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let mut resp = report.to_wire();
        if !dry_run {
            let (applied, warning) = match self.ensure_custom_profile_active(&report.name).await {
                Ok(v) => v,
                Err(e) => return WsFrame::error_response("", &e),
            };
            if let Some(obj) = resp.as_object_mut() {
                obj.insert("applied".into(), json!(applied));
                obj.insert("warning".into(), json!(warning));
            }
            info!(
                profile = %report.name,
                imported = report.imported,
                skipped = report.skipped.len(),
                "redaction rule pack imported"
            );
        }
        WsFrame::ok_response("", resp)
    }

    /// `redaction.profiles.remove` — delete a CUSTOM profile file, drop it
    /// from `[redaction] profiles`, reload (§13.2). Built-ins are refused.
    pub(crate) async fn handle_redaction_profiles_remove(&self, params: Value) -> WsFrame {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or("");
        if name.is_empty() {
            return WsFrame::error_response("", "Missing 'name' parameter");
        }
        if !crate::redaction_custom_rules::is_valid_profile_slug(name) {
            return WsFrame::error_response("", &format!("規則集名稱 '{name}' 不合法"));
        }
        // Built-ins are compiled in — there is no file to delete, and
        // unlisting one silently would look like a delete that "worked".
        if duduclaw_redaction::profiles::builtin_profiles().contains_key(name) {
            return WsFrame::error_response(
                "",
                &format!("'{name}' 是內建規則集，不能刪除（可在偵測規則集取消勾選）"),
            );
        }
        let removed = match crate::redaction_custom_rules::delete_profile(&self.home_dir, name) {
            Ok(r) => r,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let config_path = self.home_dir.join("config.toml");
        let mut table = self.read_config_table(&config_path).await;
        if crate::redaction_custom_rules::unlist_profile(&mut table, name)
            && let Err(e) = self.atomic_write_toml(&config_path, &table).await
        {
            return WsFrame::error_response("", &e);
        }
        let (applied, warning) = self.apply_redaction_hot_reload(&table).await;
        info!(profile = %name, removed, "redaction custom profile removed");
        WsFrame::ok_response(
            "",
            json!({ "ok": true, "removed": removed, "applied": applied, "warning": warning }),
        )
    }
}
