//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    /// Update one or more fields of an agent's `agent.toml`.
    ///
    /// Supports identity, model, budget, heartbeat, permissions, and evolution fields.
    /// Only sends changed fields — unchanged fields are omitted from the request.
    #[cfg(test)]
    pub(crate) async fn handle_agents_update(&self, params: Value) -> WsFrame {
        // Tests drive the operator path; authority-key gating is exercised
        // through `handle_agents_update_as` with explicit contexts.
        self.handle_agents_update_as(params, Some(&UserContext::admin_fallback())).await
    }

    /// `agents.update`. `caller` is the authenticated dashboard user; it is
    /// recorded on the `runtime_provider_deprecated` audit row (R1, 2026-10).
    /// `None` only for paths that genuinely carry no identity.
    pub(crate) async fn handle_agents_update_as(
        &self,
        params: Value,
        caller: Option<&UserContext>,
    ) -> WsFrame {
        let agent_id = match params.get("agent_id").and_then(|v| v.as_str()) {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => return WsFrame::error_response("", "Missing 'agent_id' parameter"),
        };

        // ── OS-native quota gate (write side) ────────────────────────────
        // Setting this agent's os_native capability to true must not exceed
        // the edition quota (Personal = 1 OS-native seat). Fail-closed before
        // any write. A no-op / false write is never blocked.
        let setting_os_native_true = params
            .get("capabilities")
            .and_then(|c| c.as_object())
            .and_then(|c| c.get("os_native"))
            .and_then(|v| v.as_bool())
            == Some(true);
        if setting_os_native_true {
            if let Some(reject) = self.os_native_quota_reject(&agent_id).await {
                return reject;
            }
        }

        // ── db_sources grant gate (WP-A, write side) ─────────────────────
        // Unlike `allowed_tools` / `denied_tools` / `wiki_visible_to`, an entry
        // here must name a real `config.toml [db_sources.<id>]` block: a typo
        // would otherwise persist as a grant that silently authorizes nothing,
        // and the operator would have no way to tell that from a database that
        // is simply refusing them. Fail-closed BEFORE any write — one unknown
        // id rejects the whole update. The check lives here rather than in
        // `apply_capabilities_to_table` because that function is sync and has
        // no `home_dir`; it re-normalizes the same list through the same
        // helper, so the validated value and the written value cannot drift.
        if let Some(raw) = params
            .get("capabilities")
            .and_then(|c| c.as_object())
            .and_then(|c| c.get("db_sources"))
            && let Err(msg) =
                crate::db_source_grants::validate_capability_grants(&self.home_dir, raw).await
        {
            return WsFrame::error_response("", &msg);
        }

        // If promoting to main, demote the current main agent first
        if let Some("main") = params.get("role").and_then(|v| v.as_str()) {
            if let Err(e) = self.demote_current_main(&agent_id).await {
                return WsFrame::error_response("", &e);
            }
        }

        // Detect per-agent channel token changes BEFORE the closure consumes
        // params — so we know what to hot-restart after the write succeeds.
        let mut channels_to_restart: Vec<&'static str> = Vec::new();
        if params
            .get("discord_bot_token")
            .and_then(|v| v.as_str())
            .is_some()
        {
            channels_to_restart.push("discord");
        }
        if params
            .get("telegram_bot_token")
            .and_then(|v| v.as_str())
            .is_some()
        {
            channels_to_restart.push("telegram");
        }
        if params
            .get("slack_bot_token")
            .and_then(|v| v.as_str())
            .is_some()
            || params
                .get("slack_app_token")
                .and_then(|v| v.as_str())
                .is_some()
        {
            channels_to_restart.push("slack");
        }

        // Detect an OS-watch-relevant edit (the `os_native` capability flag or
        // any `[os_watch]` field) so we can hot stop/start the agent's watcher
        // after the write succeeds.
        let os_watch_touched = params.get("os_watch").is_some()
            || params
                .get("capabilities")
                .and_then(|c| c.as_object())
                .map(|c| c.contains_key("os_native"))
                .unwrap_or(false);

        // WP: capture the pre-update display_name + agent dir when this call
        // is renaming the agent, so we can keep SOUL.md / IDENTITY.md self-
        // introduction text and the default `@trigger` in sync afterward.
        // Root cause: the agent's system-prompt self-name comes 100% from
        // literal text burned into SOUL.md at creation time — agent.toml's
        // display_name never reached the prompt on its own, so a rename left
        // the agent introducing itself with its old name forever. Read
        // BEFORE the mutation closure runs (which re-parses agent.toml fresh
        // from disk), so this is the authoritative "before" value.
        let new_display_name = params
            .get("display_name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let (old_display_name, agent_dir_for_rename) = if new_display_name.is_some() {
            let reg = self.registry.read().await;
            match reg.get(&agent_id) {
                Some(a) => (
                    Some(a.config.agent.display_name.clone()),
                    Some(a.dir.clone()),
                ),
                None => (None, None),
            }
        } else {
            (None, None)
        };
        let old_display_name_for_closure = old_display_name.clone();

        // WP22 T1/T5 + v1.68 — the authoritative `<home>/org.toml` record
        // follows the agent only when this write actually CHANGED
        // `reports_to` / `department` in agent.toml (computed from the
        // before/after diff inside the closure, committed after the mirror
        // write succeeded). Re-sending the prefilled value on an unrelated
        // autosave is no longer a change, so a hand-edited mirror is never
        // promoted into the authority by a dashboard save.
        //
        // v1.68 — authority keys (`AUTHORITY_KEYS`: org fields, all of
        // `[capabilities]`, `[container] sandbox_enabled / network_access`,
        // `[permissions] can_modify_own_soul`) may only be changed by an
        // admin; `agents.update` itself stays Owner-level so non-admin owners
        // keep every other field. `None` (no identity) is treated as
        // non-admin: fail closed.
        let caller_is_admin = caller.is_some_and(|c| c.is_admin());
        let authority_changes: std::sync::Arc<
            std::sync::Mutex<Vec<super::system_update_config_v168::ProtectedChange>>,
        > = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let authority_for_closure = authority_changes.clone();
        let audit_agent_id = agent_id.clone();
        let audit_user_id = caller.map(|c| c.user_id.clone()).unwrap_or_else(|| "unknown".into());

        let params_clone = params.clone();
        let mut changes: Vec<String> = Vec::new();
        let home_for_update = self.home_dir.clone();

        // Save-time model↔provider auto-align result, surfaced back to the
        // dashboard so it can toast the adjustment (set inside the closure).
        let aligned_provider: std::sync::Arc<std::sync::Mutex<Option<String>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let aligned_for_closure = aligned_provider.clone();
        // R1 (2026-10): deprecated runtime values this write put into
        // `[runtime]` (audited after the commit), and why the auto-align was
        // skipped when the runtime it would have chosen is deprecated.
        let deprecated_writes: std::sync::Arc<
            std::sync::Mutex<Vec<super::runtime_apply::DeprecatedRuntimeWrite>>,
        > = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let deprecated_for_closure = deprecated_writes.clone();
        let align_skipped: std::sync::Arc<std::sync::Mutex<Option<&'static str>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let align_skipped_for_closure = align_skipped.clone();

        let result = self.update_agent_toml(&agent_id, move |table| {
            // v1.68: snapshot for the authority diff and the final typed
            // check (only enforced when the file parsed before this write, so
            // an already-broken file can still be repaired through here).
            let table_before = table.clone();
            let parsed_before = super::agents_update_v168::agent_config_check(&table_before).is_ok();

            // ── Identity fields ([agent] section) ──
            if let Some(agent_section) = table.get_mut("agent").and_then(|v| v.as_table_mut()) {
                if let Some(v) = params_clone.get("display_name").and_then(|v| v.as_str()) {
                    agent_section.insert("display_name".into(), toml::Value::String(v.into()));
                    changes.push(format!("display_name = \"{v}\""));

                    // Auto-sync the default `@{old_name}` mention trigger to
                    // the new name, unless the caller also set `trigger`
                    // explicitly in this same request (that wins) or the
                    // existing trigger was already customized away from the
                    // default pattern.
                    if params_clone.get("trigger").and_then(|t| t.as_str()).is_none() {
                        if let Some(old_name) = old_display_name_for_closure.as_deref() {
                            let current_trigger = agent_section
                                .get("trigger")
                                .and_then(|t| t.as_str())
                                .unwrap_or("");
                            if let Some(new_trigger) =
                                duduclaw_core::synced_trigger(current_trigger, old_name, v)
                            {
                                agent_section.insert(
                                    "trigger".into(),
                                    toml::Value::String(new_trigger.clone()),
                                );
                                changes.push(format!("trigger synced -> \"{new_trigger}\""));
                            }
                        }
                    }
                }
                if let Some(v) = params_clone.get("role").and_then(|v| v.as_str()) {
                    match v {
                        "main" | "specialist" | "worker" | "developer" | "qa" | "planner" => {
                            agent_section.insert("role".into(), toml::Value::String(v.into()));
                            changes.push(format!("role = \"{v}\""));
                        }
                        _ => return Err(format!("Invalid role '{v}'. Valid: main, specialist, worker, developer, qa, planner")),
                    }
                }
                if let Some(v) = params_clone.get("status").and_then(|v| v.as_str()) {
                    match v {
                        "active" | "paused" | "terminated" => {
                            agent_section.insert("status".into(), toml::Value::String(v.into()));
                            changes.push(format!("status = \"{v}\""));
                        }
                        _ => return Err(format!("Invalid status '{v}'. Valid: active, paused, terminated")),
                    }
                }
                if let Some(v) = params_clone.get("trigger").and_then(|v| v.as_str()) {
                    agent_section.insert("trigger".into(), toml::Value::String(v.into()));
                    changes.push(format!("trigger = \"{v}\""));
                }
                if let Some(v) = params_clone.get("icon").and_then(|v| v.as_str()) {
                    agent_section.insert("icon".into(), toml::Value::String(v.into()));
                    changes.push(format!("icon = \"{v}\""));
                }
                if let Some(v) = params_clone.get("reports_to").and_then(|v| v.as_str()) {
                    agent_section.insert("reports_to".into(), toml::Value::String(v.into()));
                    changes.push(format!("reports_to = \"{v}\""));
                }
                // WP7: department (company → department → personal layering).
                // Empty string clears it (agent leaves its department).
                if let Some(v) = params_clone.get("department").and_then(|v| v.as_str()) {
                    let v = v.trim();
                    if v.is_empty() {
                        agent_section.remove("department");
                        changes.push("department cleared".into());
                    } else if duduclaw_core::is_valid_department(v) {
                        agent_section.insert("department".into(), toml::Value::String(v.into()));
                        changes.push(format!("department = \"{v}\""));
                    } else {
                        return Err(format!(
                            "Invalid department '{v}' (1..=64 bytes, no path separators / whitespace / control chars)"
                        ));
                    }
                }
            }

            // ── Model fields ([model] section) ──
            let model = table.entry("model")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut();
            if let Some(model) = model {
                if let Some(v) = params_clone.get("preferred").and_then(|v| v.as_str()) {
                    model.insert("preferred".into(), toml::Value::String(v.into()));
                    changes.push(format!("model.preferred = \"{v}\""));
                }
                if let Some(v) = params_clone.get("fallback").and_then(|v| v.as_str()) {
                    model.insert("fallback".into(), toml::Value::String(v.into()));
                    changes.push(format!("model.fallback = \"{v}\""));
                }
                if let Some(v) = params_clone.get("api_mode").and_then(|v| v.as_str()) {
                    match v {
                        "cli" | "direct" | "auto" => {
                            model.insert("api_mode".into(), toml::Value::String(v.into()));
                            changes.push(format!("model.api_mode = \"{v}\""));
                        }
                        _ => return Err(format!("Invalid api_mode '{v}'. Valid: cli, direct, auto")),
                    }
                }
            }

            // ── Local model fields ([model.local] section) ──
            // v1.68: `local_backend` / `local_context_length` / `local_gpu_layers`
            // are no longer accepted (no reader). An empty `local_model`
            // removes the key, and a `[model.local]` left without a model is
            // removed entirely (`model` is required by the typed loader, and
            // local inference with no model is not a setting).
            if let Some(model) = table.get_mut("model").and_then(|v| v.as_table_mut()) {
                let has_local_params = ["local_model", "prefer_local", "use_router"]
                    .iter().any(|k| params_clone.get(*k).is_some());

                if has_local_params {
                    let local = model.entry("local")
                        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                        .as_table_mut();
                    if let Some(local) = local {
                        if let Some(v) = params_clone.get("local_model").and_then(|v| v.as_str()) {
                            let v = v.trim();
                            if v.is_empty() {
                                local.remove("model");
                                changes.push("model.local.model cleared".into());
                            } else {
                                local.insert("model".into(), toml::Value::String(v.into()));
                                changes.push(format!("model.local.model = \"{v}\""));
                            }
                        }
                        if let Some(v) = params_clone.get("prefer_local").and_then(|v| v.as_bool()) {
                            local.insert("prefer_local".into(), toml::Value::Boolean(v));
                            changes.push(format!("model.local.prefer_local = {v}"));
                        }
                        if let Some(v) = params_clone.get("use_router").and_then(|v| v.as_bool()) {
                            local.insert("use_router".into(), toml::Value::Boolean(v));
                            changes.push(format!("model.local.use_router = {v}"));
                        }
                    }
                    if model
                        .get("local")
                        .and_then(|l| l.as_table())
                        .is_some_and(|l| !l.contains_key("model"))
                    {
                        model.remove("local");
                    }
                }
            }

            // ── Budget fields ([budget] section) ──
            let budget = table.entry("budget")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut();
            if let Some(budget) = budget {
                if let Some(v) = params_clone.get("monthly_limit_cents").and_then(|v| v.as_u64()) {
                    budget.insert("monthly_limit_cents".into(), toml::Value::Integer(v as i64));
                    changes.push(format!("budget.monthly_limit_cents = {v}"));
                }
                if let Some(v) = params_clone.get("warn_threshold_percent").and_then(|v| v.as_u64()) {
                    if v > 100 {
                        return Err("warn_threshold_percent must be 0-100".into());
                    }
                    budget.insert("warn_threshold_percent".into(), toml::Value::Integer(v as i64));
                    changes.push(format!("budget.warn_threshold_percent = {v}"));
                }
                if let Some(v) = params_clone.get("hard_stop").and_then(|v| v.as_bool()) {
                    budget.insert("hard_stop".into(), toml::Value::Boolean(v));
                    changes.push(format!("budget.hard_stop = {v}"));
                }
            }

            // ── Heartbeat fields ([heartbeat] section) ──
            let heartbeat = table.entry("heartbeat")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut();
            if let Some(hb) = heartbeat {
                if let Some(v) = params_clone.get("heartbeat_enabled").and_then(|v| v.as_bool()) {
                    hb.insert("enabled".into(), toml::Value::Boolean(v));
                    changes.push(format!("heartbeat.enabled = {v}"));
                }
                if let Some(v) = params_clone.get("heartbeat_interval").and_then(|v| v.as_u64()) {
                    hb.insert("interval_seconds".into(), toml::Value::Integer(v as i64));
                    changes.push(format!("heartbeat.interval_seconds = {v}"));
                }
                if let Some(v) = params_clone.get("heartbeat_cron").and_then(|v| v.as_str()) {
                    hb.insert("cron".into(), toml::Value::String(v.into()));
                    changes.push(format!("heartbeat.cron = \"{v}\""));
                }
            }

            // ── Proactive fields ([proactive] section) ──
            if let Some(p) = params_clone.get("proactive").and_then(|v| v.as_object()) {
                let proactive = table.entry("proactive")
                    .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                    .as_table_mut();
                if let Some(pt) = proactive {
                    if let Some(v) = p.get("enabled").and_then(|v| v.as_bool()) {
                        pt.insert("enabled".into(), toml::Value::Boolean(v));
                        changes.push(format!("proactive.enabled = {v}"));
                    }
                    // v1.68: `check_interval` is no longer accepted (validated
                    // as cron but never read).
                    for key in &["quiet_hours_start", "quiet_hours_end"] {
                        if let Some(v) = p.get(*key).and_then(|v| v.as_u64()) {
                            if v > 23 {
                                return Err(format!("Invalid proactive {key}: {v} (must be 0-23)"));
                            }
                            pt.insert((*key).into(), toml::Value::Integer(v as i64));
                            changes.push(format!("proactive.{key} = {v}"));
                        }
                    }
                    if let Some(v) = p.get("max_messages_per_hour").and_then(|v| v.as_u64()) {
                        pt.insert("max_messages_per_hour".into(), toml::Value::Integer(v as i64));
                        changes.push(format!("proactive.max_messages_per_hour = {v}"));
                    }
                    if let Some(v) = p.get("notify_channel").and_then(|v| v.as_str()) {
                        pt.insert("notify_channel".into(), toml::Value::String(v.into()));
                        changes.push(format!("proactive.notify_channel = \"{v}\""));
                    }
                    if let Some(v) = p.get("notify_chat_id").and_then(|v| v.as_str()) {
                        pt.insert("notify_chat_id".into(), toml::Value::String(v.into()));
                        changes.push(format!("proactive.notify_chat_id = \"{v}\""));
                    }
                    // Optional thread/topic id (Discord thread, Telegram topic).
                    // Empty string clears it (readers treat empty as unset).
                    if let Some(v) = p.get("notify_thread_id").and_then(|v| v.as_str()) {
                        pt.insert("notify_thread_id".into(), toml::Value::String(v.into()));
                        changes.push(format!("proactive.notify_thread_id = \"{v}\""));
                    }
                    // ── quiet_hours (W2-8 — dashboard-editable suppression
                    // window read by `notify_governance::load_agent_policy`).
                    // Validated with the SAME parser the runtime gate uses
                    // (`QuietWindow::parse`) so a malformed value is rejected
                    // at write time here rather than silently landing as "no
                    // quiet hours" days later when the gate fails open on it.
                    // Empty string clears it (readers treat blank as unset).
                    if let Some(v) = p.get("quiet_hours").and_then(|v| v.as_str()) {
                        let trimmed = v.trim();
                        if !trimmed.is_empty()
                            && crate::notify_governance::QuietWindow::parse(trimmed).is_none()
                        {
                            return Err(format!(
                                "Invalid proactive quiet_hours '{v}' (need \"HH:MM-HH:MM\", e.g. \"22:00-08:00\")"
                            ));
                        }
                        pt.insert("quiet_hours".into(), toml::Value::String(trimmed.into()));
                        changes.push(format!("proactive.quiet_hours = \"{trimmed}\""));
                    }
                }
            }

            // ── Permissions fields ([permissions] section) ──
            let perms = table.entry("permissions")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut();
            if let Some(perms) = perms {
                for key in &[
                    "can_create_agents",
                    "can_send_cross_agent",
                    "can_modify_own_skills",
                    "can_modify_own_soul",
                    "can_schedule_tasks",
                ] {
                    if let Some(v) = params_clone.get(*key).and_then(|v| v.as_bool()) {
                        perms.insert((*key).into(), toml::Value::Boolean(v));
                        changes.push(format!("permissions.{key} = {v}"));
                    }
                }
                // v1.68: an explicit write is an operator choice — mark the
                // file so the boot migration never resets it.
                if !perms.contains_key(super::agents_update_v168::PERMISSIONS_MARKER_KEY)
                    && changes.iter().any(|c| c.starts_with("permissions."))
                {
                    perms.insert(
                        super::agents_update_v168::PERMISSIONS_MARKER_KEY.into(),
                        toml::Value::String(super::agents_update_v168::PERMISSIONS_MARKER_VALUE.into()),
                    );
                }
            }

            // ── Container fields ([container] section) ──
            let container = table.entry("container")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut();
            if let Some(ct) = container {
                if let Some(v) = params_clone.get("timeout_ms").and_then(|v| v.as_u64()) {
                    ct.insert("timeout_ms".into(), toml::Value::Integer(v as i64));
                    changes.push(format!("container.timeout_ms = {v}"));
                }
                // v1.68: `max_concurrent` / `readonly_project` are no longer
                // accepted (no reader; the task sandbox mounts its own
                // allowlist). Existing values stay in the file untouched.
                for key in &["sandbox_enabled", "network_access"] {
                    if let Some(v) = params_clone.get(*key).and_then(|v| v.as_bool()) {
                        ct.insert((*key).into(), toml::Value::Boolean(v));
                        changes.push(format!("container.{key} = {v}"));
                    }
                }
            }

            // ── Evolution fields ([evolution] section) ──
            let evo = table.entry("evolution")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut();
            if let Some(evo) = evo {
                // D7 (2026-08-04): `cognitive_memory` is deliberately NOT in
                // this list any more — the layer is always on, so a stale
                // client that still posts the key is ignored rather than
                // writing a dead flag back into agent.toml.
                // v1.68: `skill_auto_activate` (the activation path never read
                // it) and `skill_security_scan` (the scanner always runs) are
                // no longer accepted.
                for key in &["gvu_enabled"] {
                    if let Some(v) = params_clone.get(*key).and_then(|v| v.as_bool()) {
                        evo.insert((*key).into(), toml::Value::Boolean(v));
                        changes.push(format!("evolution.{key} = {v}"));
                    }
                }
                for key in &["max_active_skills", "skill_token_budget"] {
                    if let Some(v) = params_clone.get(*key).and_then(|v| v.as_u64()) {
                        evo.insert((*key).into(), toml::Value::Integer(v as i64));
                        changes.push(format!("evolution.{key} = {v}"));
                    }
                }
                for key in &["max_silence_hours"] {
                    if let Some(v) = params_clone.get(*key).and_then(|v| v.as_f64()) {
                        evo.insert((*key).into(), toml::Value::Float(v));
                        changes.push(format!("evolution.{key} = {v}"));
                    }
                }

                // v1.68: `stagnation_*` params are no longer accepted. The live
                // detector (`gvu/stagnation.rs`) reads `gvu_stagnation_*` keys
                // with different semantics; `[evolution.stagnation_detection]`
                // only fed a stub that never suppressed anything.
            }

            // ── Per-agent channel tokens ([channels.*] sections) ──
            // Helper: write a token (+ encrypted version) into [channels.{channel}].{field}
            // Empty token removes the entire [channels.{channel}] section.
            let home = home_for_update.clone();
            let mut set_channel_token = |table: &mut toml::Table,
                                          channel: &str,
                                          fields: &[(&str, Option<&str>)], // (param_key, toml_key) pairs
                                          changes: &mut Vec<String>| -> Result<(), String> {
                // Check if any field has a value
                let has_any = fields.iter().any(|(param_key, _)| {
                    params_clone.get(*param_key).and_then(|v| v.as_str()).map_or(false, |s| !s.is_empty())
                });
                let all_empty = fields.iter().all(|(param_key, _)| {
                    params_clone.get(*param_key).and_then(|v| v.as_str()).map_or(true, |s| s.is_empty())
                });

                // If the param exists but is empty → remove
                let param_present = fields.iter().any(|(param_key, _)| params_clone.get(*param_key).is_some());
                if param_present && all_empty {
                    if let Some(channels) = table.get_mut("channels").and_then(|v| v.as_table_mut()) {
                        channels.remove(channel);
                        changes.push(format!("channels.{channel} removed"));
                    }
                    return Ok(());
                }

                if !has_any { return Ok(()); }

                let channels = table.entry("channels")
                    .or_insert_with(|| toml::Value::Table(toml::Table::new()))
                    .as_table_mut()
                    .ok_or_else(|| format!("Invalid [channels] section"))?;
                let section = channels.entry(channel)
                    .or_insert_with(|| toml::Value::Table(toml::Table::new()))
                    .as_table_mut()
                    .ok_or_else(|| format!("Invalid [channels.{channel}] section"))?;

                for (param_key, toml_key_override) in fields {
                    if let Some(val) = params_clone.get(*param_key).and_then(|v| v.as_str()) {
                        if !val.is_empty() {
                            let toml_key = toml_key_override.unwrap_or(param_key);
                            // WP12 (M1) — same write-boundary shape check as
                            // `channels.add`: a corrupted Telegram token must
                            // not be silently encrypted into agent.toml.
                            // Scoped to the primary credential field on purpose:
                            // a future `[channels.telegram]` field (webhook
                            // secret, chat id…) must not be validated as if it
                            // were a bot token.
                            let validated = if *toml_key_override == Some("bot_token") {
                                crate::config_crypto::validate_channel_token(channel, val)?
                            } else {
                                val.to_string()
                            };
                            let val: &str = &validated;
                            // Sensitive tokens: enc-only when encryption is
                            // available (MED-B parity with channels.add — the
                            // plaintext key is REMOVED, never blanked, because
                            // enc-aware readers treat a present-but-empty
                            // plaintext as "channel removed"). All per-agent
                            // readers (telegram/discord/slack bots,
                            // config_crypto::resolve_agent_token) prefer
                            // `_enc`. Keyfile-unavailable ⇒ legacy plaintext.
                            // Applies to every sensitive field routed through
                            // this closure, incl. the wecom/dingtalk secrets.
                            let is_sensitive = toml_key.contains("token")
                                || toml_key.contains("secret")
                                || toml_key == "app_id"
                                || toml_key.contains("aes_key");
                            if is_sensitive {
                                let enc_key = format!("{toml_key}_enc");
                                match crate::config_crypto::encrypt_value(val, &home) {
                                    Some(enc) => {
                                        section.remove(toml_key); // drop stale plaintext
                                        section.insert(enc_key, toml::Value::String(enc));
                                    }
                                    None => {
                                        section.insert(
                                            toml_key.to_string(),
                                            toml::Value::String(val.into()),
                                        );
                                    }
                                }
                            } else {
                                // Identifier, not a secret — plaintext is the
                                // canonical copy (e.g. phone_number_id).
                                section.insert(
                                    toml_key.to_string(),
                                    toml::Value::String(val.into()),
                                );
                            }
                        }
                    }
                }

                changes.push(format!("channels.{channel} = [CONFIGURED]"));
                Ok(())
            };

            // Discord
            set_channel_token(table, "discord", &[
                ("discord_bot_token", Some("bot_token")),
            ], &mut changes)?;

            // Telegram
            set_channel_token(table, "telegram", &[
                ("telegram_bot_token", Some("bot_token")),
            ], &mut changes)?;


            // v1.68: per-agent LINE / WhatsApp / Feishu / WeCom / DingTalk
            // credentials are no longer accepted — those channels read only
            // the global `config.toml [channels]`. Discord / Telegram / Slack
            // per-agent bots stay.

            // Slack
            set_channel_token(table, "slack", &[
                ("slack_app_token", Some("app_token")),
                ("slack_bot_token", Some("bot_token")),
            ], &mut changes)?;





            // v1.68: `sticker_*` params are no longer accepted (nothing
            // reads `[sticker]`).

            // ── Capabilities fields ([capabilities] section, CAP.1–CAP.4) ──
            // High-risk tool / computer-use / browser permissions. Delegated to
            // a pure, unit-tested helper that validates enum + numeric ranges.
            let cap_changes = apply_capabilities_to_table(table, &params_clone)?;
            changes.extend(cap_changes);

            // ── OS-native filesystem watch ([os_watch] table) ──
            // paths / ignore / debounce_ms / max_events_per_min. Gated at runtime
            // by `capabilities.os_native`; hot-reloaded after the write below.
            let os_watch_changes = apply_os_watch_to_table(table, &params_clone)?;
            changes.extend(os_watch_changes);

            // ── Self-study opt-in ([research] table) ──
            // self_study / self_study_hour. Read by
            // `self_study::ResearchConfig::from_agent_dir` on every 5-min
            // scheduler sweep (design-market-belief-loop-2026-08.md §3).
            let research_changes = apply_research_to_table(table, &params_clone)?;
            changes.extend(research_changes);

            // ── Runtime ([runtime] section, RT.1) ──
            // provider enum / fallback.
            let rt_outcome = apply_runtime_to_table_reporting(table, &params_clone)?;
            changes.extend(rt_outcome.changes);
            if let Ok(mut slot) = deprecated_for_closure.lock() {
                *slot = rt_outcome.deprecated;
            }

            // ── Evolution advanced ([evolution.*] fields, EVO.1–EVO.3) ──
            // external_factors + the skill-synthesis / graduation scalars.
            // Does NOT duplicate the inline
            // gvu/cognitive/max_active_skills/stagnation_* handling above.
            let evo_adv_changes = apply_evolution_advanced_to_table(table, &params_clone)?;
            changes.extend(evo_adv_changes);

            // v1.68: `container_advanced` (additional_mounts / cmd / env) is
            // no longer accepted: the task sandbox mounts only its own
            // allowlist and never read these.

            // ── Per-agent Odoo override ([odoo] section, ODO.1) ──
            // profile / allowed_models / allowed_actions (verb:model) /
            // company_ids + api_key|password → *_enc. Delegated to a helper so
            // the encryption + validation are unit-testable.
            let odoo_changes = apply_odoo_to_table(table, &params_clone, &home_for_update)?;
            changes.extend(odoo_changes);

            // ── Per-agent scattered fields (G.8) ──
            // These extend existing sections WITHOUT duplicating fields already
            // handled inline above (preferred/fallback, enabled/interval/cron, …).

            // [model].account_pool[] + [model].utility
            {
                let has_model_extra = ["account_pool", "utility"]
                    .iter()
                    .any(|k| params_clone.get(*k).is_some());
                if has_model_extra {
                    let model = table
                        .entry("model")
                        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                        .as_table_mut()
                        .ok_or("Invalid [model] section")?;
                    if let Some(arr) = params_clone.get("account_pool").and_then(|v| v.as_array()) {
                        let pool: Vec<toml::Value> = arr
                            .iter()
                            .filter_map(|v| v.as_str().filter(|s| !s.is_empty()).map(|s| toml::Value::String(s.into())))
                            .collect();
                        model.insert("account_pool".into(), toml::Value::Array(pool.clone()));
                        changes.push(format!("model.account_pool = [{} entries]", pool.len()));
                    }
                    if let Some(v) = params_clone.get("utility").and_then(|v| v.as_str()) {
                        // v1.68: empty removes the key (the loader then uses
                        // the default utility model) instead of writing "".
                        let v = v.trim();
                        if v.is_empty() {
                            model.remove("utility");
                            changes.push("model.utility cleared".into());
                        } else {
                            model.insert("utility".into(), toml::Value::String(v.into()));
                            changes.push(format!("model.utility = \"{v}\""));
                        }
                    }
                }
            }

            // [heartbeat].max_concurrent_runs + [heartbeat].cron_timezone
            {
                let has_hb_extra = ["heartbeat_max_concurrent_runs", "heartbeat_cron_timezone"]
                    .iter()
                    .any(|k| params_clone.get(*k).is_some());
                if has_hb_extra {
                    let hb = table
                        .entry("heartbeat")
                        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                        .as_table_mut()
                        .ok_or("Invalid [heartbeat] section")?;
                    if let Some(v) = params_clone.get("heartbeat_max_concurrent_runs").and_then(|v| v.as_u64()) {
                        if v == 0 || v > 64 {
                            return Err("heartbeat max_concurrent_runs must be 1-64".into());
                        }
                        hb.insert("max_concurrent_runs".into(), toml::Value::Integer(v as i64));
                        changes.push(format!("heartbeat.max_concurrent_runs = {v}"));
                    }
                    if let Some(v) = params_clone.get("heartbeat_cron_timezone").and_then(|v| v.as_str()) {
                        if v.parse::<chrono_tz::Tz>().is_err() {
                            return Err(format!("Invalid heartbeat cron_timezone '{v}' (IANA tz, e.g. Asia/Taipei)"));
                        }
                        hb.insert("cron_timezone".into(), toml::Value::String(v.into()));
                        changes.push(format!("heartbeat.cron_timezone = \"{v}\""));
                    }
                }
            }

            // [proactive].enabled / base_threshold / max_per_hour (P2-2 gate,
            // `proactive_gate::read_proactive_config`) + token_budget_per_check
            // / timezone / max_turns. These are read per-evaluate by
            // ProactiveGate, so a written toggle is effective on the next event
            // with no hot-reload needed (see `hot_reload_os_watcher` doc).
            if let Some(p) = params_clone.get("proactive").and_then(|v| v.as_object()) {
                let has_pro_extra = [
                    "enabled",
                    "base_threshold",
                    "max_per_hour",
                    "timezone",
                    "max_turns",
                ]
                .iter()
                .any(|k| p.contains_key(*k));
                if has_pro_extra {
                    let pt = table
                        .entry("proactive")
                        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                        .as_table_mut()
                        .ok_or("Invalid [proactive] section")?;
                    if let Some(v) = p.get("enabled").and_then(|v| v.as_bool()) {
                        pt.insert("enabled".into(), toml::Value::Boolean(v));
                        changes.push(format!("proactive.enabled = {v}"));
                    }
                    if let Some(v) = p.get("base_threshold").and_then(|v| v.as_u64()) {
                        // ProactiveGate scores are 1..=5; reject out-of-range so
                        // the dashboard surfaces the error rather than the reader
                        // silently clamping.
                        if !(1..=5).contains(&v) {
                            return Err("proactive.base_threshold must be 1-5".into());
                        }
                        pt.insert("base_threshold".into(), toml::Value::Integer(v as i64));
                        changes.push(format!("proactive.base_threshold = {v}"));
                    }
                    if let Some(v) = p.get("max_per_hour").and_then(|v| v.as_u64()) {
                        if v > 1000 {
                            return Err("proactive.max_per_hour must be 0-1000".into());
                        }
                        pt.insert("max_per_hour".into(), toml::Value::Integer(v as i64));
                        changes.push(format!("proactive.max_per_hour = {v}"));
                    }
                    // v1.68: `token_budget_per_check` is no longer accepted (no reader).
                    if let Some(v) = p.get("timezone").and_then(|v| v.as_str()) {
                        if v.parse::<chrono_tz::Tz>().is_err() {
                            return Err(format!("Invalid proactive timezone '{v}' (IANA tz)"));
                        }
                        pt.insert("timezone".into(), toml::Value::String(v.into()));
                        changes.push(format!("proactive.timezone = \"{v}\""));
                    }
                    if let Some(v) = p.get("max_turns").and_then(|v| v.as_u64()) {
                        if v == 0 || v > 100 {
                            return Err("proactive max_turns must be 1-100".into());
                        }
                        pt.insert("max_turns".into(), toml::Value::Integer(v as i64));
                        changes.push(format!("proactive.max_turns = {v}"));
                    }
                }
            }

            // [ptc] / [prompt] / [cultural_context] — string-keyed scalar tables.
            // Each accepts a flat object of string|bool|int|float scalars; unknown
            // keys are written verbatim (these sections are free-form per-agent
            // tuning, not enum-validated). Empty object is a no-op.
            // v1.68: `[ptc]` / `[cultural_context]` had no reader and are no
            // longer accepted; prefer the typed `advanced_kv` rows. The whole
            // result is checked against `AgentConfig` below either way.
            for sect in &["prompt"] {
                if let Some(obj) = params_clone.get(*sect).and_then(|v| v.as_object()) {
                    if obj.is_empty() {
                        continue;
                    }
                    let st = table
                        .entry(*sect)
                        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                        .as_table_mut()
                        .ok_or_else(|| format!("Invalid [{sect}] section"))?;
                    for (k, v) in obj {
                        if !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                            return Err(format!("Invalid {sect} key '{k}' (alphanumeric + underscore only)"));
                        }
                        let tv = match v {
                            Value::String(s) => toml::Value::String(s.clone()),
                            Value::Bool(b) => toml::Value::Boolean(*b),
                            Value::Number(n) if n.is_i64() => toml::Value::Integer(n.as_i64().unwrap()),
                            Value::Number(n) if n.is_u64() => toml::Value::Integer(n.as_u64().unwrap() as i64),
                            Value::Number(n) => toml::Value::Float(n.as_f64().unwrap_or(0.0)),
                            Value::Array(a) => {
                                let items: Vec<toml::Value> = a
                                    .iter()
                                    .filter_map(|x| x.as_str().map(|s| toml::Value::String(s.into())))
                                    .collect();
                                toml::Value::Array(items)
                            }
                            _ => return Err(format!("Unsupported {sect}.{k} value type")),
                        };
                        st.insert(k.clone(), tv);
                        changes.push(format!("{sect}.{k} updated"));
                    }
                }
            }

            // ── Auto-align [runtime] provider to [model] preferred ──
            // Live incident (2026-07-28): model grok-4.5 saved with the
            // default provider "claude" routed into the Claude CLI and died
            // with model_not_found — the warning log existed but nothing
            // stopped the broken save. On the FINAL table state, a confident
            // family mismatch rewrites the provider to the runtime that can
            // actually serve the model (family CLI if installed, else
            // openai_compat). `openai_compat` always passes the match check,
            // so API-mode setups are never touched.
            {
                let preferred = table
                    .get("model")
                    .and_then(|m| m.get("preferred"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                if let Some(model) = preferred {
                    // An unparseable stored provider means we cannot tell
                    // whether the model matches it, so the auto-align is
                    // SKIPPED rather than run against a guessed default —
                    // rewriting `[runtime] provider` on a guess is exactly the
                    // kind of silent config damage `RuntimeType::parse` now
                    // refuses to enable. `load_runtime_settings` already logs
                    // the bad value on every read of this agent.
                    let provider = table
                        .get("runtime")
                        .and_then(|r| r.get("provider"))
                        .and_then(|v| v.as_str())
                        .map(duduclaw_core::types::RuntimeType::parse)
                        .unwrap_or(Some(duduclaw_core::types::RuntimeType::default()));
                    if let Some(provider) = provider
                        && !crate::runtime_config::model_matches_provider(&model, provider)
                    {
                        let inferred = crate::runtime_config::infer_provider_for_model(&model);
                        let (inferred, skipped) = auto_align_target(inferred);
                        if let Some(reason) = skipped {
                            // Reported through the response field only: a
                            // skip is not a change, so it must not satisfy
                            // the "No valid fields to update" guard below.
                            tracing::info!(
                                model = %model,
                                reason,
                                "runtime.provider not auto-aligned: the matching runtime is deprecated"
                            );
                            if let Ok(mut slot) = align_skipped_for_closure.lock() {
                                *slot = Some(reason);
                            }
                        }
                        if let Some(aligned) = inferred
                        {
                            let rt_section = table
                                .entry("runtime")
                                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                                .as_table_mut()
                                .ok_or("Invalid [runtime] section")?;
                            rt_section.insert(
                                "provider".into(),
                                toml::Value::String(aligned.as_str().into()),
                            );
                            changes.push(format!(
                                "runtime.provider auto-aligned \"{}\" → \"{}\" (model \"{model}\")",
                                provider.as_str(),
                                aligned.as_str()
                            ));
                            if let Ok(mut slot) = aligned_for_closure.lock() {
                                *slot = Some(aligned.as_str().to_string());
                            }
                        }
                    }
                }
            }

            // ── v1.68 per-agent keys (budget.daily_cap_cents, model.effort,
            // fork, team, guardrails, memory, night_engine,
            // runtime.minimal_context, advanced_kv) ──
            changes.extend(super::agents_update_v168::apply_agent_v168_keys(table, &params_clone, caller_is_admin)?);

            // ── v1.68 authority guard: a real change to an org-guarded key
            // needs an admin; every change is audited after the commit. ──
            let authority = super::agents_update_v168::authority_diff(&table_before, table);
            if !authority.is_empty() && !caller_is_admin {
                let keys: Vec<&str> = authority.iter().map(|c| c.key.as_str()).collect();
                crate::security_autopilot::audit_and_emit(
                    &home_for_update,
                    &duduclaw_security::audit::AuditEvent::new(
                        "agent_authority_refused",
                        audit_agent_id.as_str(),
                        duduclaw_security::audit::Severity::Warning,
                        json!({
                            "agent_id": audit_agent_id,
                            "keys": keys,
                            "user_id": audit_user_id,
                            "source": "agents.update",
                        }),
                    ),
                );
                return Err(format!(
                    "Only an administrator can change {} — ask an admin, or leave those fields as they are",
                    keys.join(", ")
                ));
            }
            if let Ok(mut slot) = authority_for_closure.lock() {
                *slot = authority;
            }

            if changes.is_empty() {
                return Err("No valid fields to update".into());
            }

            // ── v1.68: the file about to be written must load with the
            // registry's typed parser, or the employee would silently drop
            // out of the registry on the next scan. ──
            if parsed_before {
                super::agents_update_v168::agent_config_check(table)?;
            }

            Ok(())
        }).await;

        match result {
            Ok(hot_reloaded) => {
                // WP22 T5 — the mirror write committed, so the authority may
                // now follow, and only when the org pair actually changed
                // (see the comment above `caller_is_admin`). A failed write
                // never reaches here, so a rejected request cannot move the
                // agent in the authority.
                let authority = authority_changes.lock().map(|g| g.clone()).unwrap_or_default();
                let org_moved = authority
                    .iter()
                    .any(|c| c.key == "agent.reports_to" || c.key == "agent.department");
                if org_moved {
                    // Carry the new mirror pair into the authority store.
                    let mirror = duduclaw_core::org_store::read_mirror(
                        &self.home_dir.join("agents").join(&agent_id).join("agent.toml"),
                    )
                    .unwrap_or_default();
                    if let Err(e) =
                        duduclaw_core::org_store::upsert(&self.home_dir, &agent_id, mirror)
                    {
                        warn!(agent = %agent_id, error = %e, "org.toml upsert failed on agents.update");
                    }
                }
                if !authority.is_empty() {
                    crate::security_autopilot::audit_and_emit(
                        &self.home_dir,
                        &duduclaw_security::audit::AuditEvent::new(
                            "agent_authority_changed",
                            agent_id.as_str(),
                            duduclaw_security::audit::Severity::Warning,
                            json!({
                                "agent_id": agent_id,
                                "user_id": caller.map(|c| c.user_id.as_str()).unwrap_or("unknown"),
                                "source": "agents.update",
                                "changes": super::agents_update_v168::audit_details(&authority),
                            }),
                        ),
                    );
                }

                // R1 (2026-10): the write committed; audit any deprecated
                // runtime value it carried, naming the caller.
                if let Ok(writes) = deprecated_writes.lock() {
                    audit_deprecated_runtime_writes(
                        &self.home_dir,
                        &agent_id,
                        "agents.update",
                        caller.map(|c| c.user_id.as_str()).unwrap_or("unknown"),
                        &writes,
                    );
                }

                // WP: sync SOUL.md / IDENTITY.md self-introduction text to
                // the new display_name (see comment above the capture site).
                // Best-effort: a missing file is skipped, an IO error is
                // logged but does not fail the already-committed agent.toml
                // write.
                let mut soul_sync_changes: Vec<String> = Vec::new();
                if let (Some(new_name), Some(old_name), Some(dir)) =
                    (&new_display_name, &old_display_name, &agent_dir_for_rename)
                {
                    if old_name != new_name && !old_name.is_empty() {
                        for fname in ["SOUL.md", "IDENTITY.md"] {
                            let path = dir.join(fname);
                            let content = match tokio::fs::read_to_string(&path).await {
                                Ok(c) => c,
                                Err(_) => continue, // file doesn't exist — nothing to sync
                            };
                            let (new_content, changed) =
                                duduclaw_core::rename_in_markdown(&content, old_name, new_name);
                            if !changed {
                                continue;
                            }
                            let tmp_path = path.with_extension("md.tmp");
                            if let Err(e) = tokio::fs::write(&tmp_path, &new_content).await {
                                warn!(agent_id = agent_id.as_str(), file = fname, error = %e, "Failed to write identity-rename tmp file");
                                continue;
                            }
                            if let Err(e) = tokio::fs::rename(&tmp_path, &path).await {
                                let _ = tokio::fs::remove_file(&tmp_path).await;
                                warn!(agent_id = agent_id.as_str(), file = fname, error = %e, "Failed to commit identity-rename");
                                continue;
                            }
                            soul_sync_changes.push(format!(
                                "{fname} self-name synced \"{old_name}\" -> \"{new_name}\""
                            ));
                        }
                    }
                }
                if !soul_sync_changes.is_empty() {
                    info!(
                        agent_id = agent_id.as_str(),
                        changes = ?soul_sync_changes,
                        "Synced agent identity files after display_name change"
                    );
                }

                // Hot-restart channel bots whose tokens just changed. Without
                // this, the running bot loop keeps the previous captured token
                // until gateway restart, so user-visible behavior diverges
                // from agent.toml on disk.
                let restarted = if !channels_to_restart.is_empty() {
                    self.hot_restart_agent_channels(&channels_to_restart, &agent_id)
                        .await
                } else {
                    Vec::new()
                };

                // Hot stop/start the agent's OS filesystem watcher when the
                // os_native flag or [os_watch] config changed — no restart. Only
                // meaningful once the registry rescan picked up the new config.
                let os_watch_hot_reloaded = if os_watch_touched && hot_reloaded {
                    self.hot_reload_os_watcher(&agent_id).await;
                    true
                } else {
                    false
                };

                info!(
                    agent_id = agent_id.as_str(),
                    hot_reloaded,
                    channels_restarted = ?restarted,
                    os_watch_hot_reloaded,
                    "agents.update completed"
                );
                WsFrame::ok_response(
                    "",
                    json!({
                        "success": true,
                        "agent_id": agent_id,
                        "hot_reloaded": hot_reloaded,
                        "channels_restarted": restarted,
                        "os_watch_hot_reloaded": os_watch_hot_reloaded,
                        "identity_files_synced": soul_sync_changes,
                        // Save-time model↔provider auto-align: the provider the
                        // gateway rewrote [runtime] to, or null when untouched.
                        "runtime_provider_aligned": aligned_provider.lock().ok().and_then(|s| s.clone()),
                        // R1 (2026-10): why the auto-align did NOT run (the
                        // runtime the model maps to is deprecated), or null.
                        "runtime_provider_align_skipped": align_skipped.lock().ok().and_then(|s| *s),
                        "message": if hot_reloaded {
                            "Agent updated successfully"
                        } else {
                            "Agent updated successfully — registry hot reload deferred to next periodic sync (≤5min)"
                        },
                    }),
                )
            }
            Err(e) => WsFrame::error_response("", &e),
        }
    }
}

/// R1 (2026-10): the save-time auto-align never writes a deprecated runtime.
/// Returns the runtime to align to (unchanged when not deprecated) and, when
/// the inferred runtime is deprecated, `None` plus the reason token surfaced
/// as `runtime_provider_align_skipped`. Deliberately does not retarget to the
/// replacement: Antigravity's `--model` takes display names, so handing it a
/// Gemini CLI model id could silently pick a different model.
pub(crate) fn auto_align_target(
    inferred: Option<duduclaw_core::types::RuntimeType>,
) -> (Option<duduclaw_core::types::RuntimeType>, Option<&'static str>) {
    match inferred {
        Some(rt) if rt.is_deprecated() => (None, Some("deprecated_runtime")),
        other => (other, None),
    }
}
