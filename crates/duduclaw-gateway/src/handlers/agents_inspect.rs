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
                            "local": cfg.model.local.as_ref().map(|l| json!({
                                "model": l.model,
                                "backend": l.backend,
                                "context_length": l.context_length,
                                "gpu_layers": l.gpu_layers,
                                "prefer_local": l.prefer_local,
                                "use_router": l.use_router,
                            })),
                        },
                        "budget": { "monthly_limit_cents": cfg.budget.monthly_limit_cents, "spent_cents": spent, "warn_threshold_percent": cfg.budget.warn_threshold_percent, "hard_stop": cfg.budget.hard_stop },
                        "heartbeat": { "enabled": cfg.heartbeat.enabled, "interval_seconds": cfg.heartbeat.interval_seconds },
                        "proactive": {
                            "enabled": cfg.proactive.enabled,
                            "check_interval": cfg.proactive.check_interval,
                            "quiet_hours_start": cfg.proactive.quiet_hours_start,
                            "quiet_hours_end": cfg.proactive.quiet_hours_end,
                            "max_messages_per_hour": cfg.proactive.max_messages_per_hour,
                            "notify_channel": cfg.proactive.notify_channel,
                            "notify_chat_id": cfg.proactive.notify_chat_id,
                            "notify_thread_id": cfg.proactive.notify_thread_id,
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
                        },
                        "sticker": {
                            "enabled": cfg.sticker.enabled,
                            "probability": cfg.sticker.probability,
                            "intensity_threshold": cfg.sticker.intensity_threshold,
                            "cooldown_messages": cfg.sticker.cooldown_messages,
                            "expressiveness": match cfg.sticker.expressiveness {
                                duduclaw_core::types::Expressiveness::Minimal => "minimal",
                                duduclaw_core::types::Expressiveness::Moderate => "moderate",
                                duduclaw_core::types::Expressiveness::Expressive => "expressive",
                            },
                        },
                        "evolution": {
                            "gvu_enabled": cfg.evolution.gvu_enabled,
                            "cognitive_memory": cfg.evolution.cognitive_memory_enabled(),
                            "skill_auto_activate": cfg.evolution.skill_auto_activate,
                            "skill_security_scan": cfg.evolution.skill_security_scan,
                            "max_silence_hours": cfg.evolution.max_silence_hours,
                        },
                        // #6: full capabilities incl. native_sandbox + Progent policy
                        // + autonomy_level (folded in above). `serde` renders
                        // snake_case enums (effect: allow|forbid|ask, op:
                        // equals|contains|starts_with).
                        "capabilities": capabilities_json,
                        // [runtime] block — read straight from agent.toml (the typed
                        // config doesn't surface it). Emits ONLY keys present in the
                        // file so the dashboard tells "unset" from an explicit false
                        // (drives the PTY-pool OAuth default-enable materialization).
                        "runtime": crate::runtime_config::read_runtime_json(
                            &self.home_dir.join("agents").join(&cfg.agent.name),
                        ),
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
