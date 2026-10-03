//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    pub(crate) async fn handle_agents_inspect(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        // Real month-to-date spend for THIS agent (not the all-account aggregate).
        let spent = self.telemetry_spent_cents_for_agent(agent_id).await;
        let reg = self.registry.read().await;
        match reg.get(agent_id) {
            Some(a) => {
                let cfg = &a.config;
                // `autonomy_level` is folded into the serialized capabilities
                // separately — it is not a typed `CapabilitiesConfig` field
                // (raw-toml additive gate, read the same way
                // `goal_loop::AutonomyLevel::for_agent` reads it at dispatch
                // time), so it would otherwise be silently absent from the
                // block below even when set on disk. Computed outside the
                // `json!` call below: a nested `{ statement; expr }` block is
                // ambiguous with the macro's own object-literal syntax.
                let mut capabilities_json =
                    serde_json::to_value(&cfg.capabilities).unwrap_or_else(|_| json!({}));
                if let Some(obj) = capabilities_json.as_object_mut() {
                    obj.insert(
                        "autonomy_level".into(),
                        json!(
                            crate::goal_loop::AutonomyLevel::for_agent(
                                &self.home_dir,
                                &cfg.agent.name,
                            )
                            .as_str()
                        ),
                    );
                    // WP-A: `db_sources` is `skip_serializing_if = Vec::is_empty`
                    // on `CapabilitiesConfig` (so an agent with no database
                    // grant keeps its on-disk shape), which would otherwise
                    // make the field *absent* here for the overwhelming
                    // majority of agents. The dashboard grant editor needs a
                    // stable `string[]`, so materialize the empty case.
                    obj.entry("db_sources").or_insert_with(|| json!([]));
                }
                let notify_policy =
                    crate::notify_governance::load_agent_policy(&self.home_dir, &cfg.agent.name);
                // [research] table read (belief loop × goal contract gap 2) —
                // computed outside the `json!` call below for the same reason
                // `capabilities_json` above is: a nested `{ statement; expr }`
                // block is ambiguous with the macro's own object-literal syntax.
                let research_cfg = crate::self_study::ResearchConfig::from_agent_dir(
                    &self.home_dir.join("agents").join(&cfg.agent.name),
                );
                // v1.68: the employee's OWN values (raw agent.toml, not the
                // preset-merged view) so the edit form can prefill every
                // field and tell "unset" from a value. Credentials excluded.
                let raw_table: toml::Table = std::fs::read_to_string(
                    self.home_dir.join("agents").join(&cfg.agent.name).join("agent.toml"),
                )
                .ok()
                .and_then(|t| t.parse().ok())
                .unwrap_or_default();
                let settings = super::agents_update_v168::agent_settings_json(&raw_table);
                let mut runtime_json = crate::runtime_config::read_runtime_json(
                    &self.home_dir.join("agents").join(&cfg.agent.name),
                );
                if let (Some(obj), Some(mc)) = (
                    runtime_json.as_object_mut(),
                    raw_table.get("runtime").and_then(|r| r.get("minimal_context")).and_then(|v| v.as_bool()),
                ) {
                    obj.insert("minimal_context".into(), json!(mc));
                }
                if let Some(obj) = capabilities_json.as_object_mut() {
                    // Empty lists are skipped on serialize; the editors need
                    // a stable `string[]`.
                    for k in ["approval_required_tools", "irreversible_tools", "maybe_irreversible_tools", "scoped_tools"] {
                        obj.entry(k).or_insert_with(|| json!([]));
                    }
                }
                WsFrame::ok_response(
                    "",
                    json!({
                        "name": cfg.agent.name,
                        "display_name": cfg.agent.display_name,
                        "role": format!("{:?}", cfg.agent.role).to_lowercase(),
                        "status": format!("{:?}", cfg.agent.status).to_lowercase(),
                        "archived": matches!(cfg.agent.status, duduclaw_core::types::AgentStatus::Archived),
                        // WP4: full avatar as an inline data URI (branding-logo model)
                        // + a cheap boolean the list view also uses.
                        "has_avatar": self.agent_has_avatar(&cfg.agent.name),
                        "avatar": self.agent_avatar_data_uri(&cfg.agent.name),
                        "outfit": read_agent_outfit(&self.home_dir.join("agents").join(&cfg.agent.name)),
                        "department": cfg.agent.department,
                        "trigger": cfg.agent.trigger,
                        "icon": cfg.agent.icon,
                        "reports_to": cfg.agent.reports_to,
                        // `[container] sandbox_enabled` / `network_access` —
                        // the edit page's two task-sandbox switches bind to
                        // these top-level keys (`AgentDetail` in api.ts).
                        // Absent `[container]` ⇒ the typed defaults (false).
                        "sandbox_enabled": cfg.container.sandbox_enabled,
                        "network_access": cfg.container.network_access,
                        "soul_preview": a.soul.as_ref().map(|s| {
                            let t = truncate_bytes(s, 500);
                            if t.len() < s.len() { format!("{t}…") } else { s.clone() }
                        }),
                        "identity_preview": a.identity.as_ref().map(|s| {
                            let t = truncate_bytes(s, 500);
                            if t.len() < s.len() { format!("{t}…") } else { s.clone() }
                        }),
                        "memory_summary": a.memory,
                        "skills": a.skills.iter().map(|s| &s.name).collect::<Vec<_>>(),
                        "model": {
                            "preferred": cfg.model.preferred,
                            "fallback": cfg.model.fallback,
                            "account_pool": cfg.model.account_pool,
                            "api_mode": cfg.model.api_mode,
                            "utility": cfg.model.utility,
                            // Raw: absent ⇒ null (the runtime default).
                            "effort": settings["model"].get("effort").cloned().unwrap_or(Value::Null),
                            // v1.68: backend / context_length / gpu_layers had no reader.
                            "local": cfg.model.local.as_ref().map(|l| json!({
                                "model": l.model,
                                "prefer_local": l.prefer_local,
                                "use_router": l.use_router,
                            })),
                        },
                        "budget": { "monthly_limit_cents": cfg.budget.monthly_limit_cents, "spent_cents": spent, "warn_threshold_percent": cfg.budget.warn_threshold_percent, "hard_stop": cfg.budget.hard_stop, "daily_cap_cents": cfg.budget.daily_cap_cents },
                        "heartbeat": {
                            "enabled": cfg.heartbeat.enabled,
                            "interval_seconds": cfg.heartbeat.interval_seconds,
                            // v1.68: returned so the form prefills it — an
                            // autosave no longer has to send `""` and wipe it.
                            "cron": cfg.heartbeat.cron,
                            "cron_timezone": cfg.heartbeat.cron_timezone,
                            "max_concurrent_runs": cfg.heartbeat.max_concurrent_runs,
                        },
                        "proactive": {
                            "enabled": cfg.proactive.enabled,
                            "quiet_hours_start": cfg.proactive.quiet_hours_start,
                            "quiet_hours_end": cfg.proactive.quiet_hours_end,
                            "max_messages_per_hour": cfg.proactive.max_messages_per_hour,
                            "notify_channel": cfg.proactive.notify_channel,
                            "notify_chat_id": cfg.proactive.notify_chat_id,
                            "notify_thread_id": cfg.proactive.notify_thread_id,
                            "timezone": cfg.proactive.timezone,
                            "max_turns": cfg.proactive.max_turns,
                            // W2-4 notification governance (F10: a suppression
                            // rule the UI cannot state is a silent failure).
                            // `quiet_hours` is the effective `HH:MM-HH:MM`
                            // window after the agent → global fallback, or
                            // null when nothing is ever held back;
                            // `quiet_hours_note` is the zh-TW sentence saying
                            // exactly what is deferred and what still gets
                            // through, ready to render as-is.
                            "quiet_hours": notify_policy.window.map(|w| w.to_display()),
                            "quiet_hours_note": notify_policy.suppression_note_zh(),
                            // W2-8 — the agent's OWN raw value (never the
                            // fallen-back one above), for the edit form's
                            // initial value. See `agent_raw_quiet_hours` doc:
                            // prefilling from the effective `quiet_hours`
                            // instead would pin the global default into this
                            // agent's own config on the next unrelated save.
                            "quiet_hours_own": crate::notify_governance::agent_raw_quiet_hours(
                                &self.home_dir,
                                &cfg.agent.name,
                            ),
                        },
                        "permissions": {
                            "can_create_agents": cfg.permissions.can_create_agents,
                            "can_send_cross_agent": cfg.permissions.can_send_cross_agent,
                            "can_modify_own_skills": cfg.permissions.can_modify_own_skills,
                            "can_modify_own_soul": cfg.permissions.can_modify_own_soul,
                            "can_schedule_tasks": cfg.permissions.can_schedule_tasks,
                            // v1.68: present ⇒ the four flags above are
                            // enforced as stored (boot migration done).
                            "permissions_enforced_since": raw_table
                                .get("permissions")
                                .and_then(|p| p.get(super::agents_update_v168::PERMISSIONS_MARKER_KEY))
                                .and_then(|v| v.as_str()),
                        },
                        "evolution": {
                            "gvu_enabled": cfg.evolution.gvu_enabled,
                            "cognitive_memory": cfg.evolution.cognitive_memory_enabled(),
                            "max_silence_hours": cfg.evolution.max_silence_hours,
                            "max_active_skills": cfg.evolution.max_active_skills,
                            "skill_token_budget": cfg.evolution.skill_token_budget,
                            "skill_synthesis_enabled": cfg.evolution.skill_synthesis_enabled,
                            "skill_synthesis_threshold": cfg.evolution.skill_synthesis_threshold,
                            "skill_synthesis_cooldown_hours": cfg.evolution.skill_synthesis_cooldown_hours,
                            "skill_trial_ttl": cfg.evolution.skill_trial_ttl,
                            "skill_graduation_min_lift": cfg.evolution.skill_graduation_min_lift,
                            "external_factors": {
                                "user_feedback": cfg.evolution.external_factors.user_feedback,
                                "security_events": cfg.evolution.external_factors.security_events,
                                "channel_metrics": cfg.evolution.external_factors.channel_metrics,
                                "business_context": cfg.evolution.external_factors.business_context,
                                "peer_signals": cfg.evolution.external_factors.peer_signals,
                            },
                        },
                        "container": { "timeout_ms": cfg.container.timeout_ms },
                        "fork": { "enabled": cfg.fork.enabled },
                        "team": settings["team"].clone(),
                        "guardrails": {
                            "enabled": cfg.guardrails.enabled,
                            "block_secrets": cfg.guardrails.block_secrets,
                            "block_injection_echo": cfg.guardrails.block_injection_echo,
                            "redact_pii": cfg.guardrails.redact_pii,
                            "deny_phrases": cfg.guardrails.deny_phrases,
                        },
                        "memory": {
                            "decision_continuity": cfg.memory.decision_continuity,
                            "decision_ttl_days": cfg.memory.decision_ttl_days,
                        },
                        "night_engine": { "enabled": cfg.night_engine.enabled },
                        "odoo": settings["odoo"].clone(),
                        "odoo_api_key_set": settings["odoo_api_key_set"].clone(),
                        "odoo_password_set": settings["odoo_password_set"].clone(),
                        // Raw `[prompt]` table for the advanced editor's prefill.
                        "prompt": settings["prompt"].clone(),
                        // Every editable key as stored in this employee's own
                        // agent.toml (absent ⇒ not set).
                        "settings": settings,
                        // #6: full capabilities incl. native_sandbox + Progent policy
                        // + autonomy_level (folded in above). `serde` renders
                        // snake_case enums (effect: allow|forbid|ask, op:
                        // equals|contains|starts_with).
                        "capabilities": capabilities_json,
                        // [runtime] block — read straight from agent.toml (the typed
                        // config doesn't surface it). Emits ONLY keys present in the
                        // file so the dashboard tells "unset" from an explicit false
                        // (drives the PTY-pool OAuth default-enable materialization).
                        "runtime": runtime_json,
                        // [os_watch] table — raw from agent.toml (typed config doesn't
                        // surface it), so the OS-native watch editor prefills the
                        // operator's own paths/ignore/debounce rather than defaults.
                        "os_watch": crate::os_events::read_os_watch_json(
                            &self.home_dir.join("agents").join(&cfg.agent.name),
                        ),
                        // [research] table — raw from agent.toml (typed config
                        // doesn't surface it), so the automation tab's
                        // self-study toggle prefills the agent's own value
                        // rather than the compiled-in default (belief loop ×
                        // goal contract gap 2).
                        "research": {
                            "self_study": research_cfg.self_study,
                            "self_study_hour": research_cfg.self_study_hour,
                        },
                    }),
                )
            }
            None => WsFrame::error_response("", &format!("Agent not found: {agent_id}")),
        }
    }
}
