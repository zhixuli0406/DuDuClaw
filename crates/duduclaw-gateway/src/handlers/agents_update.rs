//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

#[derive(Debug, Clone)]
pub(crate) enum UpdateOrigin {
    Dashboard,
    OsSettings,
    AdminAi { approval_id: String, expected_snapshot: Option<crate::agent_files::writer::AdminAiSnapshot> },
}

impl UpdateOrigin {
    fn source(&self) -> &'static str {
        match self { Self::Dashboard => "agents.update", Self::OsSettings => "os.settings.update", Self::AdminAi { .. } => "admin_ai" }
    }
}

/// Request-derived allowlist; the final table is checked separately under the writer lock.
fn admin_ai_requested_fields(params: &Value) -> Result<Vec<duduclaw_core::admin_ai_settings::AdminAiField>, &'static str> {
    use duduclaw_core::admin_ai_settings::{AdminAiField as F, DIFF_REFUSED};
    let mut fields = Vec::new();
    for (key, value) in params.as_object().ok_or(DIFF_REFUSED)? {
        let field = match key.as_str() {
            "agent_id" => continue,
            "display_name" => F::DisplayName, "icon" => F::Icon, "trigger" => F::Trigger,
            "status" => F::Status, "reply_language" => F::ReplyLanguage,
            "preferred" => F::Model, "fallback" => F::FallbackModel,
            "heartbeat_enabled" => F::HeartbeatEnabled, "heartbeat_interval" => F::HeartbeatInterval,
            "heartbeat_cron" => F::HeartbeatCron, "heartbeat_cron_timezone" => F::HeartbeatCronTimezone,
            "model.effort" => F::Effort,
            "runtime.timeout_secs" => F::TimeoutSecs, "runtime.max_turns" => F::MaxTurns,
            "runtime.idle_timeout_secs" => F::IdleTimeoutSecs,
            "runtime" | "model" | "prompt" => {
                for child in value.as_object().ok_or(DIFF_REFUSED)?.keys() {
                    let path = format!("{key}.{child}");
                    let f = F::ALL.iter().find(|f| f.toml_path() == path).ok_or(DIFF_REFUSED)?;
                    fields.push(*f);
                }
                continue;
            }
            _ => return Err(DIFF_REFUSED),
        };
        fields.push(field);
    }
    Ok(fields)
}

/// Authority paths never appear twice in audit; secrets are omitted recursively.
fn ordinary_settings_diff(before: &toml::Table, after: &toml::Table) -> Vec<Value> {
    fn safe(value: Value) -> Value {
        match value {
            Value::String(s) => {
                if super::config_raw_rpc::url_has_password(&s) || super::config_raw_rpc::url_has_secret_query(&s) {
                    return json!("[redacted]");
                }
                let s = duduclaw_security::credential_protection::sanitize_output_text(&s);
                json!(duduclaw_core::truncate_chars(&s, 200))
            }
            Value::Array(values) => Value::Array(values.into_iter().map(safe).collect()),
            Value::Object(values) => Value::Object(values.into_iter()
                .filter(|(k, _)| !super::config_raw_rpc::is_secret_key(k) && !["headers", "otlp_headers", "env"].contains(&k.as_str()))
                .map(|(k, v)| (k, safe(v))).collect()),
            other => other,
        }
    }
    duduclaw_core::admin_ai_settings::changed_leaves(before, after).into_iter().filter(|(path, _, _)| {
        let under = |prefix: &str| path == prefix || path.strip_prefix(prefix).is_some_and(|rest| rest.starts_with('.'));
        !super::agent_authority::AUTHORITY.iter().any(|key| under(key.toml_key))
            && !["channels", "odoo", "google"].iter().any(|prefix| under(prefix))
            && !path.split('.').any(|key| super::config_raw_rpc::is_secret_key(key) || ["headers", "otlp_headers", "env"].contains(&key))
    }).map(|(path, before, after)| json!({"path": path, "before": safe(before), "after": safe(after)})).collect()
}

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
        self.agents_update_inner(params, caller, None, UpdateOrigin::Dashboard).await
    }

    /// `agents.update` from the dashboard: `live` is the caller re-read from
    /// `users.db` for this request (owner decision D7: bindings are checked
    /// live, never from the session token), and `session` is re-checked the
    /// same way inside the file lock right before the write.
    pub(crate) async fn handle_agents_update_live(
        &self,
        params: Value,
        live: &UserContext,
        session: &UserContext,
    ) -> WsFrame {
        self.agents_update_inner(params, Some(live), Some(session), UpdateOrigin::Dashboard).await
    }

    pub(crate) async fn agents_update_inner(
        &self,
        params: Value,
        caller: Option<&UserContext>,
        session: Option<&UserContext>,
        origin: UpdateOrigin,
    ) -> WsFrame {
        let update_source = origin.source();
        let expected_snapshot = match &origin {
            UpdateOrigin::AdminAi { expected_snapshot: Some(hash), .. } => Some(hash.clone()),
            UpdateOrigin::AdminAi { .. } => return WsFrame::error_response("", crate::admin_ai::proposal::STALE),
            _ => None,
        };
        let requested_fields = if matches!(&origin, UpdateOrigin::AdminAi { .. }) {
            if !caller.is_some_and(|ctx| ctx.has_role(UserRole::Admin)) {
                return WsFrame::error_response("", "管理員 AI 設定需要管理員權限。");
            }
            match admin_ai_requested_fields(&params) {
                Ok(fields) => Some(fields),
                Err(e) => return WsFrame::error_response("", e),
            }
        } else { None };
        // Also protect direct callers, before registry, locks or channel side effects.
        if caller.is_some_and(|ctx| ctx.member_key_id.is_some())
            || session.is_some_and(|ctx| ctx.member_key_id.is_some()) {
            if let Err(message) = super::member_contract::member_key_agent_update_params(&params) {
                return WsFrame::Response { id: String::new(), ok: false, payload: None,
                    error: Some(json!({"code": "member_key_forbidden_field", "message": message})) };
            }
        }
        let agent_id = match params.get("agent_id").and_then(|v| v.as_str()) {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => return WsFrame::error_response("", "Missing 'agent_id' parameter"),
        };
        // Status writes serialize with avatar eligibility and lifecycle
        // changes before taking the registry, org or profile locks.
        let seat_admission = if params.get("status").is_some() {
            match self.acquire_seat_admission().await {
                Ok(lock) => Some(lock),
                Err(error) => return WsFrame::error_response("", &error.to_string()),
            }
        } else { None };

        // Every update can change proactive behavior; check main-agent access
        // before parsing fields or performing any side effect.
        if let Some(ctx) = caller {
            let stored_id = {
                let reg = self.registry.read().await;
                reg.get(&agent_id).map(|agent| agent.config.agent.name.clone())
            };
            let Some(stored_id) = stored_id else {
                return WsFrame::error_response("", "Agent not found");
            };
            if let Err(frame) = self.deny_main_for_non_admin(ctx, &stored_id).await {
                return frame;
            }
        }

        if params.get("skill_budget_unlimited").is_some_and(|v| !v.is_boolean()) {
            return WsFrame::error_response("", "skill_budget_unlimited must be a boolean");
        }

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

        // The new name is written into SOUL.md / IDENTITY.md, which reach
        // every prompt: plain text only.
        if let Some(name) = params.get("display_name").and_then(|v| v.as_str()) {
            if let Err(e) = duduclaw_core::validate_display_name(name) {
                return WsFrame::error_response("", &e);
            }
            // The name also reaches team rosters in other employees' prompts.
            if let Err(cats) = crate::agent_files::external::rename_is_safe("", name) {
                return WsFrame::error_response("", &format!("display_name looks like an instruction to the AI ({cats}); choose a plain name"));
            }
        }

        // RM-027: a new supervisor must exist, must not be the agent itself and
        // must not close a loop in the authoritative org tree — the same rule
        // the MCP path and the `org.*` RPCs apply (`validate_org_change`).
        // Checked before any side effect, and only for an actual move: an
        // autosave re-sending the current value is not an org write (and must
        // keep working while the authority is unusable). A move is refused
        // when the seeded authority is lost / unreadable / corrupt / newer.
        // The decisive check is repeated inside the org lock below. The
        // department keeps its existing agents.update semantics (a valid new
        // name creates it implicitly); only the reporting line is validated.
        let org_params = params.get("reports_to").and_then(Value::as_str).is_some()
            || params.get("department").and_then(Value::as_str).is_some();
        let mirror_path = self.home_dir.join("agents").join(&agent_id).join("agent.toml");
        if org_params {
            let mirror_now = duduclaw_core::org_store::read_mirror(&mirror_path).unwrap_or_default();
            let requested = requested_org_mirror(&mirror_now, &params);
            // Only a field the caller actually edited is judged (an echoed,
            // possibly drifted, prefilled value is not a request).
            let edited_rt = (requested.reports_to != mirror_now.reports_to).then_some(requested.reports_to.as_str());
            if requested != mirror_now
                && let Err(e) = duduclaw_core::org_change::validate_org_change(&self.home_dir, &agent_id, edited_rt, None)
            {
                return WsFrame::error_response("", &e.to_string());
            }
        }

        // `role = "main"` is an authority change (`agent_authority`): refuse a
        // non-admin BEFORE the current main is demoted below.
        let caller_is_admin = caller.is_some_and(|c| c.is_admin());
        if let Some(role) = params.get("role").and_then(|v| v.as_str())
            && !caller_is_admin
        {
            let current_is_main = self
                .registry
                .read()
                .await
                .get(&agent_id)
                .is_some_and(|a| a.config.agent.role == duduclaw_core::types::AgentRole::Main);
            if (role == "main") != current_is_main {
                crate::security_autopilot::audit_and_emit(
                    &self.home_dir,
                    &duduclaw_security::audit::AuditEvent::new(
                        "agent_authority_refused",
                        agent_id.as_str(),
                        duduclaw_security::audit::Severity::Warning,
                        json!({
                            "agent_id": agent_id,
                            "keys": ["agent.role"],
                            "user_id": caller.map(|c| c.user_id.as_str()).unwrap_or("unknown"),
                            "source": update_source,
                        }),
                    ),
                );
                return WsFrame::error_response(
                    "",
                    "Only an administrator can change agent.role to or from \"main\" — ask an admin, or leave it as it is",
                );
            }
        }

        // ── [knowledge] collections (document knowledge base) ────────────
        // Narrowing only: the list may name only collections granted to this
        // employee (or its department), `null` clears the key and `[]` turns
        // knowledge off. Validated against the knowledge database BEFORE any
        // write; an unreadable database refuses every non-empty list.
        let knowledge_binding = match params.get("knowledge") {
            None => None,
            Some(raw) => {
                let Some(agent_ref) = self.knowledge_agent_ref(&agent_id).await else {
                    return WsFrame::error_response("", "Agent not found");
                };
                let home = self.home_dir.clone();
                let raw = raw.clone();
                match tokio::task::spawn_blocking(move || {
                    crate::knowledge::agent_binding::validate(&home, &agent_ref, &raw)
                })
                .await
                {
                    Ok(Ok(b)) => Some(b),
                    Ok(Err(msg)) => return WsFrame::error_response("", &msg),
                    Err(_) => return WsFrame::error_response("", "暫時無法確認這位 AI 員工可用的知識集合，請稍後再試。"),
                }
            }
        };

        // RM-027 round 6: promoting to main demotes the current main — but
        // only under the global org lock and right before the write (below),
        // and a failed write puts the old main back (`MainDemotion`). Never
        // here, ahead of the refusals that follow.
        let promote_main = params.get("role").and_then(|v| v.as_str()) == Some("main");

        // One Telegram bot per employee (B11): refuse a bot another employee
        // already uses before anything is written; the bind lock is held
        // until the write is done (L3).
        let _bind = if params.get("telegram_bot_token").is_some() {
            Some(crate::agent_channels::binding_lock().await)
        } else {
            None
        };
        if let Some(raw) = params
            .get("telegram_bot_token")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            let token = match crate::config_crypto::validate_channel_token("telegram", raw) {
                Ok(t) => t,
                Err(e) => return WsFrame::error_response("", &e),
            };
            if let Some(holder) = crate::agent_channels::conflicts::telegram_bot_holder(
                &self.home_dir,
                &self.registry,
                &token,
                &agent_id,
            )
            .await
            {
                return WsFrame::error_response("", &holder.telegram_refusal());
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
        let authority_changes: std::sync::Arc<
            std::sync::Mutex<Vec<super::system_update_config_v168::ProtectedChange>>,
        > = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let authority_for_closure = authority_changes.clone();
        let ordinary_changes = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let ordinary_for_closure = ordinary_changes.clone();
        let audit_agent_id = agent_id.clone();
        let audit_user_id = caller.map(|c| c.user_id.clone()).unwrap_or_else(|| "unknown".into());

        let mut params_clone = params.clone();
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

        // RM-027 review — an org move holds the global org mutation lock
        // (shared with the `org.*` RPCs, the approval, MCP `agent_update` and
        // `duduclaw org sync`) from validation to commit, and commits the
        // authority BEFORE the mirror: re-read the authority strictly,
        // validate the whole tree, upsert, then write agent.toml. A failed
        // mirror write puts the previous authority entry back (still inside
        // the lock), so a refused request never moves the employee. A
        // non-admin's move is never pre-written: the closure refuses it.
        // A promotion to main holds it too (round 6): the demotion of the
        // old main, the write and any restore form one org mutation.
        let mut org_lock = if org_params || promote_main {
            match super::org_change_rpc::lock_org(&self.home_dir).await {
                Ok(g) => Some(g),
                Err(e) => return WsFrame::error_response("", &e),
            }
        } else {
            None
        };
        // `(previous authority entry, entry written)` when pre-written.
        let mut org_prewrite: Option<(Option<duduclaw_core::OrgEntry>, duduclaw_core::OrgEntry)> = None;
        if let Some(lock) = &org_lock {
            let mirror_before = duduclaw_core::org_store::read_mirror(&mirror_path).unwrap_or_default();
            let mirror_after = requested_org_mirror(&mirror_before, &params);
            if mirror_after != mirror_before && caller_is_admin {
                let store = match lock.load() {
                    Ok(s) => s,
                    Err(e) => return WsFrame::error_response("", &e.to_string()),
                };
                // RM-027 round 3: the baseline is the STRICT AUTHORITY entry,
                // never the agent.toml mirror (which the employee can
                // tamper with). Only the fields this request actually edited
                // (differ from what the mirror showed) override it; an
                // unsubmitted or merely echoed field keeps the authority's
                // value — a tampered mirror value is never promoted.
                let authority_before = duduclaw_core::org_change::effective_in(&store, &self.home_dir, &agent_id)
                    .unwrap_or_default();
                let edited_rt =
                    (mirror_after.reports_to != mirror_before.reports_to).then(|| mirror_after.reports_to.clone());
                let edited_dept =
                    (mirror_after.department != mirror_before.department).then(|| mirror_after.department.clone());
                if let Some(d) = edited_dept.as_deref()
                    && !d.is_empty()
                    && !duduclaw_core::is_valid_department(d)
                {
                    return WsFrame::error_response(
                        "",
                        &format!("Invalid department '{d}' (1..=64 bytes, no path separators / whitespace / control chars)"),
                    );
                }
                if let Err(e) = duduclaw_core::org_change::validate_org_change_in(
                    &store,
                    &self.home_dir,
                    &agent_id,
                    edited_rt.as_deref(),
                    None,
                ) {
                    return WsFrame::error_response("", &e.to_string());
                }
                let authority_after = duduclaw_core::OrgEntry::new(
                    edited_rt.as_deref().unwrap_or(&authority_before.reports_to),
                    edited_dept.as_deref().unwrap_or(&authority_before.department),
                );
                if authority_after != authority_before || !store.contains(&agent_id) {
                    let prior = store.get(&agent_id).cloned();
                    if let Err(e) = lock.upsert(&agent_id, authority_after.clone()) {
                        return WsFrame::error_response("", &e.to_string());
                    }
                    org_prewrite = Some((prior, authority_after.clone()));
                }
                // The mirror follows the authority on both fields: a drifted
                // field the caller did not edit is repaired, not promoted.
                if let Some(p) = params_clone.as_object_mut() {
                    p.insert("reports_to".into(), Value::String(authority_after.reports_to.clone()));
                    p.insert("department".into(), Value::String(authority_after.department.clone()));
                }
            }
        }

        // RM-027 round 5: while the org lock is held the agent.toml lock wait
        // is bounded — a stuck writer fails this update (the pre-written
        // authority entry is put back below) instead of pinning the org lock.
        let lock_wait = org_lock.as_ref().map(|_| crate::org_change_approval::mirror_lock_wait(&self.home_dir));

        // RM-027 round 6: the demotion happens here — under the org lock,
        // after every refusal above, right before the write — with its
        // `agent.toml` wait bounded. A failed demotion writes nothing else;
        // the pre-written authority entry (if any) is put back.
        let demotion = if promote_main {
            match self.demote_current_main(&agent_id).await {
                Ok(d) => d,
                Err(e) => {
                    let rollback = match (org_lock.take(), &org_prewrite) {
                        (Some(lock), Some((prior, _))) => lock.restore(&agent_id, prior.clone()).err(),
                        _ => None,
                    };
                    return WsFrame::error_response("", &match rollback {
                        None => e,
                        Some(re) => format!("{e}（另外，組織權威資料沒有還原成功：{re}；請管理員在組織圖確認這位 AI 員工的位置）"),
                    });
                }
            }
        } else {
            None
        };

        let result = self.update_agent_toml_as_within(&agent_id, update_source, caller, session, lock_wait, expected_snapshot, move |table| {
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
                    let current = agent_section.get("status").and_then(toml::Value::as_str).unwrap_or("active");
                    if !matches!(current, "active" | "paused" | "terminated") {
                        return Err("已封存、已刪除或狀態不明的 AI 員工，請使用還原流程。".into());
                    }
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
                if let Some(raw) = params_clone.get("when_unavailable") {
                    let policy: duduclaw_core::types::BudgetWhenUnavailable =
                        serde_json::from_value(raw.clone()).map_err(|e| e.to_string())?;
                    let v = if policy == duduclaw_core::types::BudgetWhenUnavailable::Allow { "allow" } else { "deny" };
                    budget.insert("when_unavailable".into(), toml::Value::String(v.into()));
                    changes.push(format!("budget.when_unavailable = {v}"));
                }
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
                for key in &["gvu_enabled", "skill_budget_unlimited"] {
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
                            // `_enc`.
                            // Applies to every sensitive field routed through
                            // this closure, incl. the wecom/dingtalk secrets.
                            let is_sensitive = toml_key.contains("token")
                                || toml_key.contains("secret")
                                || toml_key == "app_id"
                                || toml_key.contains("aes_key");
                            if is_sensitive {
                                // B11: agent.toml sits in the employee's own
                                // directory — refuse rather than store a
                                // plaintext token there.
                                let enc_key = format!("{toml_key}_enc");
                                let enc = crate::agent_channels::store::encrypt_or_refuse(val, &home)?;
                                section.remove(toml_key); // drop stale plaintext
                                section.insert(enc_key, toml::Value::String(enc));
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
            let rt_outcome = super::runtime_apply::apply_runtime_with_limits(table, &params_clone, crate::runtime_config::TimeoutPolicy::load(&home_for_update).max_secs, duduclaw_core::turn_limits::TurnPolicy::load(&home_for_update).limit)?;
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

            // ── [knowledge] collections (validated above) ──
            if let Some(binding) = knowledge_binding.as_ref() {
                changes.extend(crate::knowledge::agent_binding::apply_to_table(table, binding)?);
            }

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
                    if let Some(value) = p.get("max_turns") {
                        let v = value.as_u64().ok_or("proactive max_turns must be an integer between 1 and 64")?;
                        if v == 0 || v > u64::from(duduclaw_core::types::MAX_PROACTIVE_TURNS) {
                            return Err("proactive max_turns must be 1-64".into());
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
            // Top-level `reply_language` (dashboard form / MCP parity) writes
            // `[prompt] reply_language`; same validation as the `prompt` object.
            if let Some(v) = params_clone.get("reply_language") {
                let st = table
                    .entry("prompt")
                    .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                    .as_table_mut()
                    .ok_or_else(|| "Invalid [prompt] section".to_string())?;
                apply_reply_language(st, v, &mut changes)?;
            }
            if let Some(v) = params_clone.get("memory_language").or_else(|| params_clone.get("memory").and_then(|m| m.get("language"))) {
                let st = table.entry("memory").or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                    .as_table_mut().ok_or_else(|| "Invalid [memory] section".to_string())?;
                apply_memory_language(st, v, &mut changes)?;
            }
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
                        // `[prompt] reply_language`: fixed list; empty clears
                        // (inherit the global default_language).
                        if *sect == "prompt" && k == "reply_language" {
                            apply_reply_language(st, v, &mut changes)?;
                            continue;
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

            // Canonical final-table alignment remains inside the same
            // authority diff/product-policy/atomic writer boundary.
            let alignment = crate::runtime_config::align_model_runtime(table)?;
            if let Some(reason) = alignment.skipped_reason() {
                if let Ok(mut slot) = align_skipped_for_closure.lock() { *slot = Some(reason); }
            }
            if let Some(description) = alignment.description() {
                changes.push(description);
                if let Ok(mut slot) = aligned_for_closure.lock() {
                    *slot = alignment.aligned_provider().map(|p|p.as_str().to_owned());
                }
            }

            // ── v1.68 per-agent keys (budget.daily_cap_cents, model.effort,
            // fork, team, guardrails, memory, night_engine,
            // runtime.minimal_context, advanced_kv) ──
            changes.extend(super::agents_update_v168::apply_agent_v168_keys(table, &params_clone, caller_is_admin)?);

            // ── v1.68 authority guard: a real change to an org-guarded key
            // needs an admin; every change is audited after the commit. ──
            let turn_policy = if !caller_is_admin {
                duduclaw_core::turn_limits::TurnPolicy::load_checked(&home_for_update)?
            } else { duduclaw_core::turn_limits::TurnPolicy::load(&home_for_update) };
            let idle_default = crate::runtime_config::idle_policy_checked(&home_for_update)?;
            let mut authority = super::agent_authority::authority_diff_with_runtime_policy(&table_before, table, crate::runtime_config::TimeoutPolicy::load(&home_for_update), turn_policy);
            authority.retain(|c| c.key != "runtime.idle_timeout_secs");
            authority.extend(super::agent_authority::idle_authority_diff(&table_before, table, idle_default));
            if !authority.is_empty() && !caller_is_admin {
                let keys: Vec<&str> = authority.iter().map(|c| c.key.as_str()).collect();
                // The model↔provider auto-align above is what moved the
                // runtime: say so, instead of naming a field the caller
                // never sent.
                let aligned = aligned_for_closure.lock().ok().and_then(|s| s.clone());
                if let Some(rt) = aligned.filter(|_| keys.contains(&"runtime.provider")) {
                    crate::security_autopilot::audit_and_emit(
                        &home_for_update,
                        &duduclaw_security::audit::AuditEvent::new(
                            "agent_authority_refused",
                            audit_agent_id.as_str(),
                            duduclaw_security::audit::Severity::Warning,
                            json!({ "agent_id": audit_agent_id, "keys": keys, "user_id": audit_user_id, "source": update_source }),
                        ),
                    );
                    return Err(format!(
                        "This model runs on a different runtime (\"{rt}\"), and only an administrator can switch the runtime — ask an admin, or pick a model the current runtime supports"
                    ));
                }
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
                            "source": update_source,
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

            if let Some(fields) = &requested_fields {
                duduclaw_core::admin_ai_settings::validate_changed_leaves(&table_before, table, fields)
                    .map_err(str::to_string)?;
                if fields.iter().any(|f| matches!(f,
                    duduclaw_core::admin_ai_settings::AdminAiField::Trigger
                    | duduclaw_core::admin_ai_settings::AdminAiField::DisplayName)) {
                    // The writer holds both the authority namespace lease and
                    // this profile's lock through the eventual commit.
                    crate::admin_ai::proposal::check_live_trigger(&home_for_update, &audit_agent_id, table)
                        .map_err(|_| crate::admin_ai::proposal::STALE.to_string())?;
                }
            }
            *ordinary_for_closure.lock().map_err(|_| "settings audit lock poisoned".to_string())? =
                ordinary_settings_diff(&table_before, table);
            Ok(())
        }).await;

        match result {
            Ok(hot_reloaded) => {
                let changed = ordinary_changes.lock().map(|g| g.clone()).unwrap_or_default();
                let mut details = json!({"agent_id": agent_id,
                    "user_id": caller.map(|c| c.user_id.as_str()).unwrap_or("unknown"),
                    "source": origin.source(), "changed": changed});
                if let UpdateOrigin::AdminAi { approval_id, .. } = &origin {
                    details["approval_id"] = json!(approval_id);
                }
                crate::security_autopilot::audit_and_emit(&self.home_dir,
                    &duduclaw_security::audit::AuditEvent::new("agent_settings_changed", agent_id.as_str(),
                        duduclaw_security::audit::Severity::Info, details));
                if let Some(admission) = &seat_admission {
                    if let Err(error) = admission.recount(0) {
                        return WsFrame::error_response("", &error);
                    }
                }
                if let Some(status) = params.get("status").and_then(Value::as_str) {
                    self.broadcast_event("agent.status_changed",
                        json!({ "agent_id": agent_id, "status": status })).await;
                }
                // WP22 T5 + RM-027 — the authority was committed above, before
                // the mirror, from the strict authority baseline. The mirror
                // is NEVER promoted into the authority afterwards (round 3):
                // if it ended up elsewhere than the authority, that is drift,
                // recorded and left for an explicit resolution.
                let authority = authority_changes.lock().map(|g| g.clone()).unwrap_or_default();
                let org_moved = authority
                    .iter()
                    .any(|c| c.key == "agent.reports_to" || c.key == "agent.department");
                if org_moved || org_prewrite.is_some() {
                    let mirror = duduclaw_core::org_store::read_mirror(&mirror_path).unwrap_or_default();
                    let committed = org_prewrite.as_ref().map(|(_, written)| written.clone()).or_else(|| {
                        org_lock.as_ref().and_then(|lock| lock.load().ok()).and_then(|store| {
                            duduclaw_core::org_change::effective_in(&store, &self.home_dir, &agent_id)
                        })
                    });
                    if committed.as_ref() != Some(&mirror) {
                        warn!(agent = %agent_id, "agent.toml org fields differ from the authority after agents.update (not promoted)");
                        crate::security_autopilot::audit_and_emit(
                            &self.home_dir,
                            &duduclaw_security::audit::AuditEvent::new(
                                "org_change_mirror_drift",
                                agent_id.as_str(),
                                duduclaw_security::audit::Severity::Warning,
                                json!({
                                    "agent_id": agent_id,
                                    "authority": committed.as_ref().map(|c| json!({ "reports_to": c.reports_to, "department": c.department })),
                                    "mirror": { "reports_to": mirror.reports_to, "department": mirror.department },
                                    "source": update_source,
                                }),
                            ),
                        );
                    }
                }
                drop(org_lock.take());
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
                                "source": update_source,
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
                // Through the profile file writer (lock, version, audit,
                // registry rescan afterwards). The SOUL baseline follows only
                // when the file was the approved version before the rename:
                // a rename never blesses an out-of-band edit, and never
                // leaves a legitimate file reported as drifted (P1).
                // Best-effort: a failure is logged but does not fail the
                // already-committed agent.toml write.
                let mut soul_sync_changes: Vec<String> = Vec::new();
                let mut soul_sync_skipped: Vec<String> = Vec::new();
                if let (Some(new_name), Some(old_name), Some(_)) =
                    (&new_display_name, &old_display_name, &agent_dir_for_rename)
                {
                    if old_name != new_name && !old_name.is_empty() {
                        for key in [crate::agent_files::FileKey::Soul, crate::agent_files::FileKey::Identity] {
                            match self.sync_renamed_profile_file(&agent_id, key.clone(), old_name, new_name, caller).await {
                                Ok(true) => soul_sync_changes.push(format!(
                                    "{} self-name synced \"{old_name}\" -> \"{new_name}\"",
                                    key.file_name()
                                )),
                                Ok(false) => {}
                                Err(e) => {
                                    warn!(agent_id = agent_id.as_str(), file = key.file_name(), error = %e, "New name not synced into a profile file");
                                    soul_sync_skipped.push(format!("{}: {e}", key.file_name()));
                                }
                            }
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
                if duduclaw_core::execution_gate::isolation_required() && params.get("proactive").is_some()
                    && params.get("proactive").and_then(|value| value.get("enabled")).and_then(Value::as_bool) != Some(false) {
                    // This authenticated settings write identifies the owner of
                    // the recurring work. It submits a card, never auto-approves.
                    let requested = async {
                        let caller = caller.ok_or("背景工作需要可驗證的交辦人。")?;
                        let owner = duduclaw_core::HumanPrincipal::user(&caller.user_id)
                            .ok_or("背景工作需要可驗證的交辦人。")?.to_string();
                        let material = crate::isolation_background::material::heartbeat(&self.home_dir, &agent_id)?;
                        crate::isolation_background::request_record_from_settings(&self.home_dir, "heartbeat", &agent_id,
                            &agent_id, &owner, &material).await
                            .map_err(|error| error.user_message()
                                .unwrap_or("背景工作設定已儲存，但執行授權尚未送審；請重新儲存設定。").to_string())
                    }.await;
                    if let Err(error) = requested { return WsFrame::error_response("", &error); }
                }
                WsFrame::ok_response(
                    "",
                    json!({
                        "success": true,
                        "agent_id": agent_id,
                        "hot_reloaded": hot_reloaded,
                        "channels_restarted": restarted,
                        "os_watch_hot_reloaded": os_watch_hot_reloaded,
                        "identity_files_synced": soul_sync_changes,
                        // Files the rename sync left alone because the result
                        // tripped a fixed rule (edit them in the dashboard).
                        "identity_files_skipped": soul_sync_skipped,
                        // Save-time model↔provider auto-align: the provider the
                        // gateway rewrote [runtime] to, or null when untouched.
                        "runtime_provider_aligned": aligned_provider.lock().ok().and_then(|s| s.clone()),
                        // R1 (2026-10): why the auto-align did NOT run (the
                        // runtime the model maps to is deprecated), or null.
                        "runtime_provider_align_skipped": align_skipped.lock().ok().and_then(|s| *s),
                        "message": if hot_reloaded {
                            "Agent updated successfully"
                        } else {
                            "已儲存，但暫時無法立即套用；請稍後重新整理，仍未生效請重新啟動服務"
                        },
                    }),
                )
            }
            Err(e) => {
                // RM-027 round 6: the promotion did not happen, so the main
                // it demoted is put back — still under the org lock.
                let e = match demotion {
                    Some(d) => match self.restore_demoted_main(d).await {
                        Ok(()) => e,
                        Err(re) => format!(
                            "{e}（另外，原本的主代理沒有成功恢復為主代理：{}；請管理員在 AI 員工設定把它改回主代理）",
                            duduclaw_core::truncate_chars(&re, 300)
                        ),
                    },
                    None => e,
                };
                // The mirror was not written: put the authority back so a
                // refused request never moves the employee.
                if let (Some(lock), Some((prior, written))) = (org_lock.take(), &org_prewrite)
                    && let Err(re) = lock.restore(&agent_id, prior.clone())
                {
                    crate::security_autopilot::audit_and_emit(
                        &self.home_dir,
                        &duduclaw_security::audit::AuditEvent::new(
                            "org_change_mirror_drift",
                            agent_id.as_str(),
                            duduclaw_security::audit::Severity::Warning,
                            json!({
                                "agent_id": agent_id,
                                "authority": { "reports_to": written.reports_to, "department": written.department },
                                "source": update_source,
                                "error": duduclaw_core::truncate_chars(&format!("{e}; rollback: {re}"), 300),
                            }),
                        ),
                    );
                    return WsFrame::error_response(
                        "",
                        &format!("{e}（另外，組織權威資料沒有還原成功：{re}；請管理員在組織圖確認這位 AI 員工的位置）"),
                    );
                }
                WsFrame::error_response("", &e)
            }
        }
    }
}

/// Write (or clear) `[prompt] reply_language` into the `[prompt]` table.
///
/// Accepts only the fixed list (`auto` / `zh-TW` / `zh-CN` / `en` / `ja-JP`);
/// an empty string removes the key so the employee inherits the global
/// `[general] default_language`. Anything else is refused before any write.
fn apply_reply_language(
    prompt: &mut toml::map::Map<String, toml::Value>,
    value: &Value,
    changes: &mut Vec<String>,
) -> Result<(), String> {
    let s = value
        .as_str()
        .map(str::trim)
        .ok_or_else(|| "reply_language must be a string".to_string())?;
    if s.is_empty() {
        prompt.remove("reply_language");
        changes.push("prompt.reply_language cleared".into());
    } else if duduclaw_core::profile_sections::is_valid_reply_language(s) {
        prompt.insert("reply_language".into(), toml::Value::String(s.into()));
        changes.push(format!("prompt.reply_language = \"{s}\""));
    } else {
        return Err(format!(
            "Invalid reply_language '{s}'. Valid: {} (empty = inherit the global setting)",
            duduclaw_core::profile_sections::REPLY_LANGUAGE_CHOICES.join(", ")
        ));
    }
    Ok(())
}

fn apply_memory_language(
    memory: &mut toml::map::Map<String, toml::Value>,
    value: &Value,
    changes: &mut Vec<String>,
) -> Result<(), String> {
    let s = value
        .as_str()
        .map(str::trim)
        .ok_or_else(|| "language must be a string".to_string())?;
    if s.is_empty() {
        memory.remove("language");
        changes.push("memory.language cleared".into());
    } else if duduclaw_core::profile_sections::is_valid_reply_language(s) {
        memory.insert("language".into(), toml::Value::String(s.into()));
        changes.push(format!("memory.language = \"{s}\""));
    } else {
        return Err(format!(
            "Invalid language '{s}'. Valid: {} (empty = inherit the global setting)",
            duduclaw_core::profile_sections::REPLY_LANGUAGE_CHOICES.join(", ")
        ));
    }
    Ok(())
}

impl MethodHandler {
    /// Rename sync for one profile file through the writer (source
    /// `rename`, no AI review). `Ok(false)` when the file is absent or the
    /// old name does not appear.
    async fn sync_renamed_profile_file(
        &self,
        agent_id: &str,
        key: crate::agent_files::FileKey,
        old_name: &str,
        new_name: &str,
        caller: Option<&UserContext>,
    ) -> Result<bool, String> {
        use crate::agent_files::writer::{Baseline, Commit, CommitMeta, commit};
        let path = crate::agent_files::file_path(&self.home_dir, agent_id, &key)?;
        if !path.exists() {
            return Ok(false);
        }
        let (old, new) = (old_name.to_string(), new_name.to_string());
        let outcome = commit(
            &self.registry,
            Commit {
                home: &self.home_dir,
                agent_id,
                key,
                expected_hash: None,
                meta: CommitMeta::new(caller.map(|c| c.user_id.as_str()).unwrap_or("system"), "rename"),
                baseline: Baseline::AcceptIfWasIntact,
            },
            move |current| {
                let renamed = duduclaw_core::rename_in_markdown(current, &old, &new).0;
                // An automatic edit nobody reviews: write it only when the
                // fixed rules find nothing in the result.
                crate::agent_files::external::rename_is_safe(current, &renamed)
                    .map_err(|cats| format!("not synced, the result needs a review ({cats})"))?;
                Ok(renamed)
            },
            || Ok(()),
        )
        .await
        .map_err(|e| e.message())?;
        Ok(!outcome.unchanged)
    }
}

/// The `agent.toml` org pair this `agents.update` request would leave
/// (`reports_to` / `department` params on top of `before`, canonicalised the
/// way the authority stores them).
fn requested_org_mirror(before: &duduclaw_core::OrgEntry, params: &Value) -> duduclaw_core::OrgEntry {
    duduclaw_core::OrgEntry::new(
        params.get("reports_to").and_then(Value::as_str).unwrap_or(before.reports_to.as_str()),
        params.get("department").and_then(Value::as_str).unwrap_or(before.department.as_str()),
    )
}

#[cfg(test)]
mod memory_language_writer_tests {
    use super::*;
    #[test]
    fn employee_memory_language_writer_validates_choices() {
        let mut table = toml::Table::new();
        let mut changes = Vec::new();
        for code in duduclaw_core::profile_sections::REPLY_LANGUAGE_CHOICES {
            apply_memory_language(&mut table, &json!(code), &mut changes).unwrap();
            assert_eq!(table.get("language").and_then(|v| v.as_str()), Some(*code));
        }
        for value in [json!("invalid"), json!(42), json!({})] {
            assert!(apply_memory_language(&mut table, &value, &mut changes).is_err());
        }
        apply_memory_language(&mut table, &json!(""), &mut changes).unwrap();
        assert!(!table.contains_key("language"));
    }
}

#[cfg(test)]
mod admin_ai_update_tests {
    use super::*;

    fn events(home: &Path) -> Vec<Value> {
        std::fs::read_to_string(home.join("security_audit.jsonl")).unwrap_or_default().lines()
            .filter_map(|line| serde_json::from_str(line).ok()).collect()
    }

    #[test]
    fn ordinary_diff_excludes_authority_and_secrets() {
        let before: toml::Table = "[agent]\nicon='A'\n[runtime]\nprovider='claude'".parse().unwrap();
        let after: toml::Table = "[agent]\nicon='B'\n[runtime]\nprovider='codex'\n[channels.test]\ntoken='canary'\n[google]\nsubject='private'\n[prompt]\nsecret='canary'".parse().unwrap();
        assert_eq!(ordinary_settings_diff(&before, &after), vec![json!({"path":"agent.icon", "before":"A", "after":"B"})]);
    }

    #[tokio::test]
    async fn ordinary_update_emits_one_event_with_before_after() {
        let home = tempfile::tempdir().unwrap();
        let _db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
        let h = MethodHandler::new(home.path().to_owned()).await;
        let admin = UserContext::admin_fallback();
        let frame = h.handle_agents_create_as(json!({"name":"audit-a"}), Some(&admin)).await;
        assert!(matches!(frame, WsFrame::Response {ok:true,..}), "{frame:?}");
        let path = home.path().join("agents/audit-a/agent.toml");
        let before: toml::Table = std::fs::read_to_string(path).unwrap().parse().unwrap();
        let frame = h.handle_agents_update_as(json!({"agent_id":"audit-a", "icon":"🦊", "can_schedule_tasks":true}), Some(&admin)).await;
        assert!(matches!(frame, WsFrame::Response {ok:true,..}), "{frame:?}");
        let rows = events(home.path());
        let rows: Vec<_> = rows.iter().filter(|e| e["event_type"] == "agent_settings_changed").collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["details"]["source"], "agents.update");
        assert!(rows[0]["details"]["changed"].as_array().unwrap().iter()
            .all(|c| !c["path"].as_str().unwrap().starts_with("permissions.")));
        assert!(rows[0]["details"]["changed"].as_array().unwrap().iter().any(|c|
            c["path"] == "agent.icon" && c["before"] == before["agent"]["icon"].as_str().unwrap() && c["after"] == "🦊"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn admin_ai_refuses_extra_path_and_auto_aligned_engine_without_writing() {
        let home = tempfile::tempdir().unwrap();
        let _db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
        let h = MethodHandler::new(home.path().to_owned()).await;
        let admin = UserContext::admin_fallback();
        let frame = h.handle_agents_create_as(json!({"name":"ai-a", "model_preferred":"claude-sonnet-4-6", "runtime":{"provider":"claude"}}), Some(&admin)).await;
        assert!(matches!(frame, WsFrame::Response {ok:true,..}), "{frame:?}");
        let path = home.path().join("agents/ai-a/agent.toml");
        let before = std::fs::read_to_string(&path).unwrap();
        let _availability = crate::runtime_config::testing::AvailabilityGuard::at("gpt-5.4", true);
        for params in [json!({"agent_id":"ai-a", "icon":"B", "runtime":{"provider":"codex"}}),
            json!({"agent_id":"ai-a", "preferred":"gpt-5.4"})] {
            let frame = h.agents_update_inner(params, Some(&admin), Some(&admin),
                UpdateOrigin::AdminAi {approval_id:"approved-1".into(), expected_snapshot:Some(crate::agent_files::writer::AdminAiSnapshot {
                    effective_hash: crate::admin_ai::proposal::hash_text(&before),
                    raw_hash: crate::admin_ai::proposal::hash_text(&before),
                })}).await;
            assert!(matches!(&frame, WsFrame::Response {ok:false,..}), "{frame:?}");
            assert!(serde_json::to_string(&frame).unwrap().contains("admin_ai_diff_refused"), "{frame:?}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        }
        let frame = h.agents_update_inner(json!({"agent_id":"ai-a", "icon":"🦊"}), Some(&admin), Some(&admin),
            UpdateOrigin::AdminAi {approval_id:"approved-2".into(), expected_snapshot:Some(crate::agent_files::writer::AdminAiSnapshot {
                    effective_hash: crate::admin_ai::proposal::hash_text(&before),
                    raw_hash: crate::admin_ai::proposal::hash_text(&before),
                })}).await;
        assert!(matches!(frame, WsFrame::Response {ok:true,..}), "{frame:?}");
        let rows = events(home.path());
        let row = rows.iter().find(|e| e["event_type"] == "agent_settings_changed").unwrap();
        assert_eq!(row["details"]["source"], "admin_ai");
        assert_eq!(row["details"]["approval_id"], "approved-2");
    }
}
