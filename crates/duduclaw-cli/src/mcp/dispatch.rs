use super::*;

pub(crate) async fn handle_tools_call(
    id: &Value,
    params: &Value,
    home_dir: &Path,
    http: &reqwest::Client,
    memory: &SqliteMemoryEngine,
    default_agent: &str,
    odoo: &OdooState,
    ns_ctx: &crate::mcp_namespace::NamespaceContext,
    daily_quota: &crate::mcp_memory_quota::DailyQuota,
    caller_client_id: &str,
    caller_is_admin: bool,
) -> Value {
    let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));

    // ── Namespace-aware wiki agent (W19-P0 M2) ──────────────────────────────
    // For external clients, any agent_id was stripped upstream; use the
    // namespace-derived client_id as the fallback wiki agent so their wiki
    // operations stay isolated in "external/{client_id}" rather than leaking
    // into the default internal agent's wiki directory.
    let wiki_agent = wiki_agent_from_ns(ns_ctx, default_agent);

    // ── Namespace hijacking prevention (W19-P0) ────────────────────────────
    // If the caller supplied an explicit `namespace` or `agent_id` in the
    // arguments, verify it falls within their permitted read namespaces.
    // This prevents an external client from reading another client's data by
    // passing `"namespace": "internal/other-agent"` in the tool arguments.
    if let Some(requested_ns) = arguments.get("namespace").and_then(|v| v.as_str()) {
        if let Err(e) = crate::mcp_namespace::assert_can_access(ns_ctx, requested_ns) {
            return jsonrpc_error(id, -32003, &format!("Namespace access denied: {e}"));
        }
    }

    info!(tool = %tool_name, "MCP tools/call");

    // Record state-changing tool calls for post-action hallucination audit.
    // Only tools that mutate agent/system state are tracked.
    //
    // 2026-07 HIGH-C sweep: the list previously omitted several mutating
    // tools (execute_program, the computer-use actions, config/pairing/skill
    // installs, model lifecycle, reminders, memory_store, decision_resolve),
    // so their calls left no audit trail at all. Tools that already write
    // their own tool_calls.jsonl records stay OUT of this list to avoid
    // double-logging: `odoo_*` (per-call audit in handle_odoo_tool) and
    // `wiki_write` with `scope="shared"` (authorship-extras record).
    let is_state_changing = matches!(
        tool_name,
        "create_agent"
            | "agent_remove"
            | "agent_update"
            | "agent_update_soul"
            | "spawn_agent"
            | "spawn_ephemeral"
            | "send_to_agent"
            | "create_task"
            | "update_cron_task"
            | "delete_cron_task"
            | "pause_cron_task"
            | "run_cron_task"
            | "tasks_create"
            | "tasks_update"
            | "tasks_claim"
            | "tasks_renew"
            | "tasks_complete"
            | "tasks_block"
            | "responsibility_followup"
            | "responsibility_ask"
            | "goals_create"
            | "plan_update_step"
            | "activity_post"
            | "shared_skill_share"
            | "shared_skill_adopt"
            | "fork_run"
            | "merge_or_select"
            | "terminate_branch"
            // ── 2026-07 additions (verified mutating, previously untracked) ──
            | "execute_program"
            | "office_script"
            // `computer_screenshot` is audited too (summary only: the image
            // block never reaches `extract_tool_result_text`).
            | "computer_screenshot"
            | "computer_click"
            | "computer_type"
            | "computer_key"
            | "computer_scroll"
            | "computer_session_start"
            | "computer_session_stop"
            | "computer_navigate"
            // P2-C: audited as workspace id + path hash + length only.
            | "computer_workspace_write"
            | "computer_workspace_list"
            | "computer_workspace_read"
            | "shared_wiki_delete"
            | "canvas_push"
            | "canvas_clear"
            | "channel_config"
            | "pairing_manage"
            | "skill_hub_install"
            | "skill_pin"
            | "memory_store"
            | "memory_invalidate_by_origin"
            | "memory_alias_add"
            | "working_state_set"
            | "working_state_clear"
            | "working_state_handoff"
            // P1/WP-5: filing a TaskPacket is a durable write that moves work
            // between roles — audit-worthy on every call, refusals included.
            | "team_handoff"
            | "belief_submit"
            | "belief_settle"
            // CD-1 co-drive: drives real mouse/keyboard input on a shared
            // desktop — audit-worthy on every call, success or refusal.
            | "codrive_run"
            // Agent Mail: `mail_send` writes a pending outbound draft (it
            // cannot transmit). `mail_read` flips a message to read.
            | "mail_send"
            | "mail_read"
            | "model_load"
            | "model_download"
            | "model_unload"
            | "create_reminder"
            | "cancel_reminder"
            | "decision_resolve"
            // Google Workspace write tools (draft creation, calendar event,
            // spreadsheet row append, Google Tasks create/complete). The
            // Forms tools are read-only and stay out of this list.
            | "gmail_create_draft"
            | "calendar_create_event"
            | "sheets_append"
            | "gtasks_create"
            | "gtasks_complete"
            | "docs_append"
            // Notion / GitHub write tools (page append, public issue comment).
            | "notion_page_append"
            | "github_issue_comment"
            // Recording → skill (WP3.3): all five mutate recording/draft state.
            | "browser_record_start"
            | "browser_record_stop"
            | "desktop_record_start"
            | "desktop_record_stop"
            | "skill_from_recording"
            // O-0 system-operator tool face: EVERY os_* system tool is
            // audited on success, not just the destructive ones — design
            // §6.4 "操作透明：操作員做完主動回報做了什麼（audit-transparent）"
            // treats "an agent queried/changed the physical machine's state"
            // as audit-worthy on its own, unlike high-frequency reads like
            // memory_search which deliberately stay out of this list.
            | "os_device_status"
            | "os_system_status"
            | "os_check_update"
            | "os_backup_list"
            | "os_network_info"
            | "os_wifi_status"
            | "os_wifi_scan"
            | "os_wifi_connect"
            | "os_apply_update"
            | "os_boot_assessment"
            | "os_update_rollback"
            | "os_backup_create"
            | "os_power"
            | "os_factory_reset"
            | "os_doctor_repair"
            | "os_display_get"
            | "os_display_set"
            // Y10-1: agent→audio bridge — same "audit every os_* call on
            // success" rule as the rest of the O-0 system-operator face.
            | "os_audio_get"
            | "os_audio_set"
    );
    // Who this call acts as for the record-relationship checks
    // (`record_authz.rs`), resolved once per call.
    let record_actor = record_actor_for(home_dir, caller_client_id, default_agent);
    let result = match tool_name {
        "send_message" => handle_send_message(&arguments, home_dir, http, default_agent).await,
        "web_search" => handle_web_search(&arguments, http).await,
        // ── W19-P0 M1: namespace-aware memory endpoints ────────────────────
        "memory_search" => {
            crate::mcp_memory_handlers::handle_memory_search(&arguments, memory, ns_ctx).await
        }
        "memory_store" => {
            crate::mcp_memory_handlers::handle_memory_store(&arguments, memory, ns_ctx, daily_quota)
                .await
        }
        // ── Cross-wake authoritative working state (D3 ghost-memory fix) ──
        "working_state_set" => handle_working_state_set(&arguments, home_dir, default_agent).await,
        "working_state_clear" => {
            handle_working_state_clear(&arguments, home_dir, default_agent).await
        }
        "working_state_handoff" => {
            handle_working_state_handoff(&arguments, home_dir, default_agent).await
        }
        "working_state_get" => handle_working_state_get(&arguments, home_dir, default_agent).await,
        // ── Team-as-Agent handoff (P1/WP-5, design §3.4) ──
        "team_handoff" => handle_team_handoff(&arguments, home_dir, default_agent).await,
        // ── Belief Loop (design-market-belief-loop-2026-08.md WP2) ──
        "belief_submit" => handle_belief_submit(&arguments, home_dir, default_agent).await,
        "belief_settle" => handle_belief_settle(&arguments, home_dir, default_agent).await,
        "belief_stats" => handle_belief_stats(home_dir, default_agent).await,
        // ── Human-machine co-drive (CD-1) ──
        "codrive_run" => handle_codrive_run(&arguments, home_dir, default_agent).await,
        "codrive_status" => handle_codrive_status(home_dir, default_agent).await,
        // ── Agent Mail (P2-d) ──
        "mail_list" => handle_mail_list(&arguments, home_dir, default_agent).await,
        "mail_read" => handle_mail_read(&arguments, home_dir, default_agent).await,
        "mail_send" => handle_mail_send(&arguments, home_dir, default_agent).await,
        "user_profile_record" => {
            crate::mcp_memory_handlers::handle_user_profile_record(&arguments, memory, ns_ctx).await
        }
        "user_profile_get" => {
            crate::mcp_memory_handlers::handle_user_profile_get(&arguments, memory, ns_ctx).await
        }
        "user_code_profile" => {
            crate::mcp_memory_handlers::handle_user_code_profile(memory, ns_ctx).await
        }
        "memory_read" => {
            crate::mcp_memory_handlers::handle_memory_read(&arguments, memory, ns_ctx).await
        }
        "memory_fetch_batch" => {
            crate::mcp_memory_handlers::handle_memory_fetch_batch(&arguments, memory, ns_ctx).await
        }
        "memory_alias_add" => {
            crate::mcp_memory_handlers::handle_memory_alias_add(&arguments, memory, ns_ctx).await
        }
        "memory_alias_list" => {
            crate::mcp_memory_handlers::handle_memory_alias_list(memory, ns_ctx).await
        }
        "memory_get_history" => {
            crate::mcp_memory_handlers::handle_memory_get_history(&arguments, memory, ns_ctx).await
        }
        "memory_get_at" => {
            crate::mcp_memory_handlers::handle_memory_get_at(&arguments, memory, ns_ctx).await
        }
        "memory_invalidate_by_origin" => {
            let env_agent = std::env::var(duduclaw_core::ENV_AGENT_ID).ok();
            let client_is_agent = duduclaw_core::is_valid_agent_id(caller_client_id)
                && home_dir.join("agents").join(caller_client_id).join("agent.toml").is_file();
            let acting = crate::mcp_memory_handlers::ai_employee_caller(
                caller_client_id,
                env_agent.as_deref(),
                client_is_agent,
            );
            crate::mcp_memory_handlers::handle_memory_invalidate_by_origin(
                &arguments,
                memory,
                ns_ctx,
                acting.as_deref(),
                home_dir,
            )
            .await
        }
        "memory_search_by_layer" => {
            handle_memory_search_by_layer(&arguments, memory, default_agent).await
        }
        "code_map" => crate::mcp_memory_handlers::handle_code_map(&arguments).await,
        "memory_successful_conversations" => {
            handle_memory_successful_conversations(&arguments, memory, default_agent).await
        }
        "memory_episodic_pressure" => {
            handle_memory_episodic_pressure(&arguments, memory, default_agent).await
        }
        "memory_consolidation_status" => {
            handle_memory_consolidation_status(memory, default_agent).await
        }
        "decision_list" => handle_decision_list(&arguments, memory, default_agent).await,
        "decision_resolve" => handle_decision_resolve(&arguments, memory, default_agent).await,
        "memory_improve" => {
            crate::mcp_memory_handlers::handle_memory_improve(&arguments, memory, ns_ctx).await
        }
        "plan_start" => {
            crate::mcp_planner::handle_plan_start(&arguments, home_dir, default_agent).await
        }
        "send_to_agent" => handle_send_to_agent(&arguments, home_dir, default_agent).await,
        "send_photo" => handle_send_media(&arguments, home_dir, http, "photo").await,
        "send_sticker" => handle_send_media(&arguments, home_dir, http, "sticker").await,
        "list_cron_tasks" => handle_list_cron_tasks(&arguments, home_dir, default_agent).await,
        "update_cron_task" => handle_update_cron_task(&arguments, home_dir, record_actor).await,
        "delete_cron_task" => handle_delete_cron_task(&arguments, home_dir, record_actor).await,
        "pause_cron_task" => handle_pause_cron_task(&arguments, home_dir, record_actor).await,
        "run_cron_task" => handle_run_cron_task(&arguments, home_dir, record_actor).await,
        "create_reminder" => handle_create_reminder(&arguments, home_dir, record_actor).await,
        "list_reminders" => handle_list_reminders(&arguments, home_dir, default_agent).await,
        "cancel_reminder" => handle_cancel_reminder(&arguments, home_dir, default_agent).await,
        // The org gates and the removed-name reservation judge the agent the
        // call acts for: the process's `default_agent` on the internal key,
        // the key's own client id otherwise (a per-agent key IS the agent).
        "create_agent" => {
            handle_create_agent(&arguments, home_dir, acting_agent_id(caller_client_id, default_agent))
                .await
        }
        "list_agents" => handle_list_agents(&arguments, home_dir, default_agent).await,
        "create_task" => handle_create_task(&arguments, home_dir, default_agent).await,
        "check_responses" => handle_check_responses(&arguments, home_dir).await,
        "task_status" => handle_task_status(&arguments, home_dir, default_agent).await,
        "agent_status" => handle_agent_status(&arguments, home_dir, default_agent).await,
        "spawn_agent" => handle_spawn_agent(&arguments, home_dir, default_agent).await,
        "spawn_ephemeral" => handle_spawn_ephemeral(&arguments, home_dir, default_agent).await,
        "agent_update" => handle_agent_update(&arguments, home_dir, record_actor).await,
        "agent_remove" => {
            handle_agent_remove(&arguments, home_dir, acting_agent_id(caller_client_id, default_agent))
                .await
        }
        "agent_update_soul" => handle_agent_update_soul(&arguments, home_dir).await,
        // T5/O13 merged skill-search entry (`source` picks hubs / bank).
        "skill_search" => handle_skill_search(&arguments, home_dir).await,
        "skill_gaps" => handle_skill_gaps(&arguments, home_dir, default_agent).await,
        "skill_list" => handle_skill_list(&arguments, home_dir).await,
        "skill_security_scan" => handle_skill_security_scan(&arguments, home_dir).await,
        "skill_graduate" => handle_skill_graduate(&arguments, home_dir).await,
        "skill_synthesis_status" => handle_skill_synthesis_status(&arguments, home_dir).await,
        "skill_synthesis_run" => {
            handle_skill_synthesis_run(&arguments, home_dir, default_agent).await
        }
        "skill_hub_install" => {
            handle_skill_hub_install(&arguments, home_dir, default_agent, caller_is_admin).await
        }
        "skill_curator_status" => handle_skill_curator_status(&arguments, home_dir).await,
        "skill_pin" => handle_skill_pin(&arguments, home_dir).await,
        "submit_feedback" => handle_submit_feedback(&arguments, home_dir, default_agent).await,
        "evolution_toggle" => handle_evolution_toggle(&arguments, home_dir).await,
        "evolution_status" => {
            handle_evolution_status_tool(&arguments, home_dir, default_agent).await
        }
        // The grant subject IS an agent directory name (`agents/<id>`), so the
        // internal client_id must resolve to the acting agent or every request
        // is filed against a non-existent `agents/gateway-internal`.
        "capability_request" => {
            handle_capability_request(
                &arguments,
                home_dir,
                acting_agent_id(caller_client_id, default_agent),
            )
            .await
        }
        "audit_trail_query" => {
            handle_audit_trail_query(&arguments, home_dir, caller_client_id, caller_is_admin).await
        }
        "reliability_summary" => {
            handle_reliability_summary(&arguments, home_dir, caller_client_id, caller_is_admin)
                .await
        }
        // Channel settings tools
        "channel_config" => handle_channel_config(&arguments, home_dir).await,
        "channel_config_list" => handle_channel_config_list(&arguments, home_dir).await,
        "channel_status" => handle_channel_status(&arguments, home_dir).await,
        "pairing_manage" => handle_pairing_manage(&arguments, home_dir).await,
        "web_fetch_cached" => handle_web_fetch_cached(&arguments, home_dir).await,
        "web_extract" => handle_web_extract(&arguments, home_dir).await,
        // Local inference tools
        "inference_status" => handle_inference_status(home_dir).await,
        "model_list" => handle_model_list(home_dir).await,
        "model_load" => handle_model_load(&arguments, home_dir).await,
        "model_unload" => handle_model_unload(home_dir).await,
        "hardware_info" => handle_hardware_info().await,
        "route_query" => handle_route_query(&arguments, home_dir).await,
        "inference_mode" => handle_inference_mode(home_dir).await,
        "llamafile_start" => handle_llamafile_start(&arguments, home_dir).await,
        "llamafile_stop" => handle_llamafile_stop(home_dir).await,
        "llamafile_list" => handle_llamafile_list(home_dir).await,
        // Model registry tools
        "model_search" => handle_model_search(&arguments, home_dir).await,
        "model_download" => handle_model_download(&arguments, home_dir).await,
        "model_recommend" => handle_model_recommend(home_dir).await,
        // Cost telemetry tools
        "cost_summary" => handle_cost_summary(&arguments, home_dir).await,
        "cost_agents" => handle_cost_agents(&arguments, home_dir).await,
        "cost_users" => handle_cost_users(&arguments, home_dir).await,
        "cost_recent" => handle_cost_recent(&arguments).await,
        "cost_multi_vs_single" => handle_cost_multi_vs_single(&arguments, home_dir).await,
        // Voice / ASR / TTS tools
        "transcribe_audio" => handle_transcribe_audio(&arguments).await,
        "synthesize_speech" => handle_synthesize_speech(&arguments).await,
        // Wiki Knowledge Base tools — use wiki_agent (namespace-aware) instead of
        // default_agent so external clients stay isolated in their own namespace.
        // T5/O3 — one `wiki_*` entry point with a `scope` parameter
        // (`mcp_alias::resolve_wiki_scope`). An unknown `scope` fails closed
        // rather than defaulting to either wiki: the two have different trust
        // boundaries.
        name @ ("wiki_ls" | "wiki_read" | "wiki_write" | "wiki_search" | "wiki_lint"
        | "wiki_stats") => {
            match crate::mcp_alias::resolve_wiki_scope(&arguments) {
                Err(e) => tool_error(&e),
                Ok(crate::mcp_alias::WikiScope::Shared) => match name {
                    "wiki_ls" => handle_shared_wiki_ls(home_dir, default_agent).await,
                    "wiki_read" => {
                        handle_shared_wiki_read(&arguments, home_dir, default_agent).await
                    }
                    "wiki_write" => {
                        handle_shared_wiki_write(&arguments, home_dir, default_agent).await
                    }
                    "wiki_search" => {
                        handle_shared_wiki_search(&arguments, home_dir, default_agent).await
                    }
                    "wiki_stats" => {
                        handle_shared_wiki_stats(home_dir, default_agent).await
                    }
                    _ => handle_shared_wiki_lint(home_dir, default_agent).await,
                },
                Ok(crate::mcp_alias::WikiScope::Agent) => match name {
                    "wiki_ls" => handle_wiki_ls(&arguments, home_dir, wiki_agent).await,
                    "wiki_read" => handle_wiki_read(&arguments, home_dir, wiki_agent).await,
                    "wiki_write" => handle_wiki_write(&arguments, home_dir, wiki_agent).await,
                    "wiki_search" => handle_wiki_search(&arguments, home_dir, wiki_agent).await,
                    "wiki_stats" => handle_wiki_stats(&arguments, home_dir, wiki_agent).await,
                    _ => handle_wiki_lint(&arguments, home_dir, wiki_agent).await,
                },
            }
        }
        "wiki_export" => handle_wiki_export(&arguments, home_dir, wiki_agent).await,
        "wiki_dedup" => handle_wiki_dedup(&arguments, home_dir, wiki_agent).await,
        "wiki_graph" => handle_wiki_graph(&arguments, home_dir, wiki_agent).await,
        "wiki_rebuild_fts" => handle_wiki_rebuild_fts(&arguments, home_dir, wiki_agent).await,
        "wiki_trust_audit" => handle_wiki_trust_audit(&arguments, home_dir, wiki_agent).await,
        "wiki_trust_history" => handle_wiki_trust_history(&arguments, home_dir, wiki_agent).await,
        // Shared Wiki: delete has no agent-local twin (the agent wiki is
        // curated from the dashboard, never self-deleted over MCP), so it
        // keeps its own name — merging it would have *added* a destructive
        // capability rather than removing a duplicate one.
        "shared_wiki_delete" => {
            handle_shared_wiki_delete(&arguments, home_dir, default_agent).await
        }
        "wiki_namespace_status" => handle_wiki_namespace_status(home_dir, default_agent).await,
        // Live Canvas tools (G15) — agent_id comes from the caller context
        // (default_agent), never from arguments, so an agent can only ever
        // write its own canvas.
        "canvas_push" => handle_canvas_push(&arguments, home_dir, default_agent).await,
        "canvas_clear" => handle_canvas_clear(home_dir, default_agent).await,
        "identity_resolve" => handle_identity_resolve(&arguments, home_dir, default_agent).await,
        "wiki_share" => handle_wiki_share(&arguments, home_dir, wiki_agent).await,
        // Skill Internalization tools
        "skill_extract" => handle_skill_extract(&arguments, home_dir, default_agent).await,
        // Program execution
        "execute_program" => handle_execute_program(&arguments, home_dir, default_agent).await,
        // Office document script execution (agent_id from caller context)
        "office_script" => handle_office_script(&arguments, home_dir, default_agent).await,
        // Skill Bank tools
        "skill_bank_feedback" => handle_skill_bank_feedback(&arguments).await,
        // Session tools
        "session_restore_context" => handle_session_restore_context(&arguments).await,
        // Task Board tools
        "tasks_list" => handle_tasks_list(&arguments, home_dir, default_agent, record_actor).await,
        "tasks_create" => handle_tasks_create(&arguments, home_dir, record_actor).await,
        "discovery_catalog" | "discovery_list" | "discovery_tree" | "discovery_artifact" | "discovery_cancel" =>
            handle_discovery_query(tool_name, &arguments, home_dir, default_agent).await,
        "tasks_update" => handle_tasks_update(&arguments, home_dir, record_actor).await,
        "tasks_claim" => handle_tasks_claim(&arguments, home_dir, record_actor).await,
        "tasks_renew" => handle_tasks_renew(&arguments, home_dir, default_agent).await,
        "tasks_complete" => handle_tasks_complete(&arguments, home_dir, record_actor).await,
        "tasks_block" => handle_tasks_block(&arguments, home_dir, record_actor).await,
        // P2-A continuous responsibilities (employee side)
        "responsibility_get" => handle_responsibility_get(&arguments, home_dir, record_actor).await,
        "responsibility_followup" => {
            handle_responsibility_followup(&arguments, home_dir, record_actor).await
        }
        "responsibility_ask" => handle_responsibility_ask(&arguments, home_dir, record_actor).await,
        // Goal chain tools (G8)
        "goals_create" => handle_goals_create(&arguments, home_dir, default_agent).await,
        "goals_list" => handle_goals_list(&arguments, home_dir).await,
        // Co-edited plan tools (U4)
        "plan_get" => handle_plan_get(&arguments, home_dir, default_agent).await,
        "plan_update_step" => handle_plan_update_step(&arguments, home_dir, default_agent).await,
        // Activity Feed tools
        "activity_post" => handle_activity_post(&arguments, home_dir, record_actor).await,
        "activity_list" => {
            handle_activity_list(&arguments, home_dir, default_agent, record_actor).await
        }
        // Autopilot tools
        "autopilot_list" => handle_autopilot_list(&arguments, home_dir).await,
        // Shared Skills tools
        "shared_skill_list" => handle_shared_skill_list(&arguments, home_dir).await,
        "shared_skill_share" => {
            handle_shared_skill_share(&arguments, home_dir, default_agent).await
        }
        "shared_skill_adopt" => {
            handle_shared_skill_adopt(&arguments, home_dir, default_agent).await
        }
        // Computer Use tools — require computer_use capability.
        // O7: the arm reads `mcp_dispatch::COMPUTER_USE_TOOLS` rather than
        // re-listing the seven names, so this gate and the `tools/list` filter
        // cannot drift apart.
        t if crate::mcp_dispatch::COMPUTER_USE_TOOLS.contains(&t)
            || crate::mcp_dispatch::COMPUTER_WORKSPACE_TOOLS.contains(&t) =>
        {
            // SEC: Validate agent ID before path construction (prevent traversal)
            if !is_valid_agent_id(default_agent) {
                return jsonrpc_error(id, -32602, "Invalid agent ID");
            }
            // SEC: Verify the calling agent has computer_use capability enabled.
            let cu_allowed = {
                // W3-3b (a): caller-derived, `.ephemeral/` included.
                let agent_dir = caller_agent_dir(home_dir, default_agent);
                let toml_path = agent_dir.join("agent.toml");
                // Use async read to avoid blocking the Tokio worker thread,
                // then hand the text to the shared typed parse point (R2
                // unification). Absent file / malformed TOML / absent or
                // wrong-typed `computer_use` all still deny (fail-closed).
                tokio::fs::read_to_string(&toml_path)
                    .await
                    .map(|c| {
                        duduclaw_core::agent_toml::parse(&c)
                            .capabilities
                            .computer_use
                    })
                    .unwrap_or(false)
            };
            if !cu_allowed {
                return jsonrpc_error(
                    id,
                    -32603,
                    "computer_use capability is not enabled for this agent. Set [capabilities] computer_use = true in agent.toml",
                );
            }
            // The session lives in the gateway, which re-checks the
            // capability, the image (never pulled; a missing image comes back
            // as readable text naming the remedy), ownership and every action.
            handle_computer_use_tool(tool_name, &arguments, home_dir, default_agent).await
        }
        // RFC-26: Live Run Forking tools (gated by Scope::ForkExecute + per-agent
        // [fork] enabled toggle checked inside each handler).
        "fork_run" => crate::mcp_fork::handle_fork_run(&arguments, home_dir, default_agent).await,
        "inspect_branches" => {
            crate::mcp_fork::handle_inspect_branches(&arguments, home_dir, default_agent).await
        }
        "diff_branches" => {
            crate::mcp_fork::handle_diff_branches(&arguments, home_dir, default_agent).await
        }
        "merge_or_select" => {
            crate::mcp_fork::handle_merge_or_select(&arguments, home_dir, default_agent).await
        }
        "terminate_branch" => {
            crate::mcp_fork::handle_terminate_branch(&arguments, home_dir, default_agent).await
        }
        "fork_cost" => crate::mcp_fork::handle_fork_cost(&arguments, home_dir, default_agent).await,
        // WP-D §13.7: read-only SQL data sources. `Scope::DbRead` and the
        // per-agent `[capabilities] db_sources` grant are enforced upstream in
        // mcp_dispatch; the specific source name is checked inside each
        // handler. Source ownership follows the CALLER (same rationale as
        // os_watch_status): a gateway-spawned agent presents the shared
        // internal client_id (or, legacy stdio, an empty one), in which case
        // the process's default agent is the caller — see `acting_agent_id`.
        "db_sources" | "db_tables" | "db_select" | "db_query" => {
            let db_agent = acting_agent_id(caller_client_id, default_agent);
            match tool_name {
                "db_sources" => crate::mcp_db::handle_db_sources(home_dir, db_agent).await,
                "db_tables" => {
                    crate::mcp_db::handle_db_tables(&arguments, home_dir, db_agent).await
                }
                "db_select" => {
                    crate::mcp_db::handle_db_select(&arguments, home_dir, db_agent).await
                }
                _ => crate::mcp_db::handle_db_query(&arguments, home_dir, db_agent).await,
            }
        }
        // WP-F2 §14.2: local data files. `Scope::FilesRead` is enforced
        // upstream; the path fence lives in `mcp_files::vet_path` and is keyed
        // to the CALLER (same rationale as the db family above): an internal
        // stdio caller may present an empty client_id, in which case the
        // process's default agent is the caller. Getting this wrong would let
        // one agent read another agent's attachments.
        "file_read" | "csv_read" | "xlsx_read" => {
            let files_agent = acting_agent_id(caller_client_id, default_agent);
            match tool_name {
                "file_read" => {
                    crate::mcp_files::handle_file_read(&arguments, home_dir, files_agent)
                }
                "csv_read" => crate::mcp_files::handle_csv_read(&arguments, home_dir, files_agent),
                _ => crate::mcp_files::handle_xlsx_read(&arguments, home_dir, files_agent),
            }
        }
        // OS-native Phase 1 tools. The os_native capability + scope + (for
        // os_open) ActionGuard gates are enforced upstream in mcp_dispatch;
        // these handlers are the mechanism.
        "os_notify" => handle_os_notify(&arguments).await,
        "os_watch_status" => {
            // Report the CALLER's watch stats, not the process-level default
            // agent. The os_native gate authorizes on `principal.client_id`
            // (== `caller_client_id`) and the stats file is keyed by agent
            // directory name, so using `default_agent` here would leak another
            // agent's watched paths (or wrongly report "no watch"). Internal
            // stdio callers may present an empty client_id → fall back to the
            // default agent so single-agent setups keep working.
            let watch_agent = acting_agent_id(caller_client_id, default_agent);
            handle_os_watch_status(home_dir, watch_agent).await
        }
        "os_open" => handle_os_open(&arguments).await,
        // OS-native P2-4: structured sensing sources. Read-only, no
        // ActionGuard — gated only by [capabilities] os_native + Scope::OsNative
        // (enforced upstream in mcp_dispatch, same choke point as the P1 tools).
        "os_frontmost" => handle_os_frontmost().await,
        "os_spotlight_search" => handle_os_spotlight_search(&arguments).await,
        "os_calendar_today" => handle_os_calendar_today().await,
        // O-0: system-operator tool face (device.*/system.* bridge). Admin
        // scope is enforced upstream in mcp_dispatch (`tool_requires_scope`);
        // appliance/confirm/ApprovalBroker gates live inside each handler —
        // see `mcp_os_ops.rs` module doc.
        "os_device_status" => crate::mcp_os_ops::handle_os_device_status(home_dir).await,
        "os_system_status" => crate::mcp_os_ops::handle_os_system_status(home_dir).await,
        "os_check_update" => crate::mcp_os_ops::handle_os_check_update(home_dir).await,
        "os_backup_list" => crate::mcp_os_ops::handle_os_backup_list(home_dir).await,
        "os_network_info" => crate::mcp_os_ops::handle_os_network_info().await,
        "os_wifi_status" => crate::mcp_os_ops::handle_os_wifi_status().await,
        "os_wifi_scan" => crate::mcp_os_ops::handle_os_wifi_scan(&arguments).await,
        "os_wifi_connect" => crate::mcp_os_ops::handle_os_wifi_connect(&arguments, home_dir).await,
        "os_apply_update" => {
            crate::mcp_os_ops::handle_os_apply_update(&arguments, home_dir, default_agent).await
        }
        "os_boot_assessment" => crate::mcp_os_ops::handle_os_boot_assessment().await,
        "os_update_rollback" => crate::mcp_os_ops::handle_os_update_rollback(&arguments).await,
        "os_backup_create" => crate::mcp_os_ops::handle_os_backup_create(home_dir).await,
        "os_power" => crate::mcp_os_ops::handle_os_power(&arguments).await,
        "os_factory_reset" => {
            // Not an authorization input — the value is only the requester
            // shown on the approval card — but "gateway-internal" is never a
            // real requester, so resolve it to the acting agent for honest
            // attribution on an irreversible action.
            crate::mcp_os_ops::handle_os_factory_reset(
                &arguments,
                home_dir,
                acting_agent_id(caller_client_id, default_agent),
            )
            .await
        }
        "os_doctor_repair" => crate::mcp_os_ops::handle_os_doctor_repair(home_dir).await,
        "os_display_get" => crate::mcp_os_ops::handle_os_display_get().await,
        "os_display_set" => crate::mcp_os_ops::handle_os_display_set(&arguments).await,
        "os_audio_get" => crate::mcp_os_ops::handle_os_audio_get().await,
        "os_audio_set" => crate::mcp_os_ops::handle_os_audio_set(&arguments).await,
        // Recording → skill tools (WP3.3). The [capabilities] recording gate +
        // Scope::Recording are enforced upstream in mcp_dispatch (fail-closed);
        // these handlers are the mechanism. Recording ownership follows the
        // CALLER (same rationale as os_watch_status): internal stdio callers
        // may present an empty client_id → fall back to the default agent.
        "browser_record_start"
        | "browser_record_stop"
        | "desktop_record_start"
        | "desktop_record_stop"
        | "skill_from_recording" => {
            let rec_agent = acting_agent_id(caller_client_id, default_agent);
            match tool_name {
                "browser_record_start" => {
                    crate::mcp_recording::handle_browser_record_start(
                        &arguments, home_dir, rec_agent,
                    )
                    .await
                }
                "browser_record_stop" => {
                    crate::mcp_recording::handle_browser_record_stop(&arguments, home_dir).await
                }
                "desktop_record_start" => {
                    crate::mcp_recording::handle_desktop_record_start(
                        &arguments, home_dir, rec_agent,
                    )
                    .await
                }
                "desktop_record_stop" => {
                    crate::mcp_recording::handle_desktop_record_stop(&arguments, home_dir).await
                }
                _ => {
                    crate::mcp_recording_distill::handle_skill_from_recording(
                        &arguments, home_dir, rec_agent,
                    )
                    .await
                }
            }
        }
        // Google Workspace native tools (Gmail + Calendar). Scope gates
        // (google:read / google:write) are enforced upstream in mcp_dispatch;
        // these handlers consume the OAuth vault token directly.
        // Whole group sits behind the integration gate (hidden from tools/list
        // too) until DuDu Studio's OAuth app clears Google verification.
        name if GOOGLE_WORKSPACE_TOOLS.contains(&name)
            && !duduclaw_gateway::google_workspace::integration_enabled(home_dir) =>
        {
            serde_json::json!({
                "content": [{"type": "text", "text": "Google Workspace 整合尚未啟用。請操作者在 dashboard 的 整合 → Google 頁面完成連線或儲存憑證（會自動啟用），或手動在 config.toml 加上 [integrations] google_workspace = true。"}],
                "isError": true
            })
        }
        "google_status" => handle_google_status(home_dir).await,
        "gmail_search" => handle_gmail_search(&arguments, home_dir).await,
        "gmail_read" => handle_gmail_read(&arguments, home_dir).await,
        "gmail_create_draft" => handle_gmail_create_draft(&arguments, home_dir).await,
        "calendar_list_events" => handle_calendar_list_events(&arguments, home_dir).await,
        "calendar_create_event" => handle_calendar_create_event(&arguments, home_dir).await,
        "sheets_read" => handle_sheets_read(&arguments, home_dir).await,
        "sheets_append" => handle_sheets_append(&arguments, home_dir).await,
        "forms_get" => handle_forms_get(&arguments, home_dir).await,
        "forms_list_responses" => handle_forms_list_responses(&arguments, home_dir).await,
        "gtasks_lists" => handle_gtasks_lists(home_dir).await,
        "gtasks_list" => handle_gtasks_list(&arguments, home_dir).await,
        "gtasks_create" => handle_gtasks_create(&arguments, home_dir).await,
        "gtasks_complete" => handle_gtasks_complete(&arguments, home_dir).await,
        "drive_search" => handle_drive_search(&arguments, home_dir).await,
        "drive_read" => handle_drive_read(&arguments, home_dir).await,
        "docs_read" => handle_docs_read(&arguments, home_dir).await,
        "docs_append" => handle_docs_append(&arguments, home_dir).await,
        "slides_read" => handle_slides_read(&arguments, home_dir).await,
        "notion_status" => handle_notion_status(home_dir).await,
        "notion_search" => handle_notion_search(&arguments, home_dir).await,
        "notion_page_read" => handle_notion_page_read(&arguments, home_dir).await,
        "notion_page_append" => handle_notion_page_append(&arguments, home_dir).await,
        // H8: GitHub native tools sit behind the same deny-by-default gate the
        // Google group does (hidden from tools/list too). Connecting GitHub
        // through the dashboard flips it on, so the only people who see this
        // refusal are those whose operator never connected the integration.
        name if GITHUB_WORKSPACE_TOOLS.contains(&name)
            && !duduclaw_gateway::github_workspace::integration_enabled(home_dir) =>
        {
            serde_json::json!({
                "content": [{"type": "text", "text": duduclaw_gateway::github_workspace::INTEGRATION_DISABLED_MESSAGE}],
                "isError": true
            })
        }
        "github_status" => handle_github_status(home_dir).await,
        "github_search_issues" => handle_github_search_issues(&arguments, home_dir).await,
        "github_issue_read" => handle_github_issue_read(&arguments, home_dir).await,
        "github_pr_read" => handle_github_pr_read(&arguments, home_dir).await,
        "github_issue_comment" => handle_github_issue_comment(&arguments, home_dir).await,
        // Odoo ERP tools
        t if t.starts_with("odoo_") => {
            handle_odoo_tool(t, &arguments, home_dir, odoo, default_agent).await
        }
        // A name removed after its deprecation window. `McpDispatcher` answers
        // these before any gate; this arm covers direct callers of this
        // function so they get the same pointer to the replacement.
        name if duduclaw_core::tool_catalog::removed_mcp_tool(name).is_some() => {
            removed_tool_result(name).unwrap_or_else(|| tool_error("tool removed"))
        }
        _ => {
            return jsonrpc_error(id, -32602, &format!("Unknown tool: {tool_name}"));
        }
    };

    // ── Tool call audit trail (L1 anti-hallucination) ──────────
    // Use the actual EXECUTING agent's ID, not just default_agent verbatim.
    // In a genuine agent-to-agent delegation, DUDUCLAW_DELEGATION_SENDER
    // identifies the real caller; when it is a system/scheduler sender
    // (goal-loop-driver / cron / heartbeat / autopilot / dashboard / webhook)
    // it names who dispatched the work, not who is executing this tool call —
    // see `resolve_audit_agent` (WP-A10 BUG-1 fix).
    if is_state_changing {
        let success = !result
            .get("isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let actual_agent = resolve_audit_agent(|| default_agent.to_string());
        let params_summary = build_params_summary(tool_name, &arguments);
        // R4 (TraceElephant): capture the tool's INPUT arguments, not just the
        // outcome summary — masked (secret keys/values), size-capped, and
        // skipped for read-only tool names inside the helper. Previously
        // `append_tool_call_with_input` had zero production callers.
        //
        // B3b (GroundEval evidence source): also capture the tool's RESULT
        // text — masked/capped inside the audit helper — so the B3 grounding
        // pre-check in `dispatch_engine.rs` has evidence to compare an
        // agent's claim against, instead of observing `ResultTextMissing`
        // on every row.
        //
        // Fix-2 C1a (2026-08 grounding self-echo audit): tools on
        // `duduclaw_core::grounding::SELF_ECHO_TOOL_NAMES` (tasks_complete,
        // tasks_update, ...) return a response envelope that is
        // substantially the caller's OWN input echoed back
        // (`task_row_to_json`'s `result_summary`/`title`/`blocked_reason`
        // fields ARE the `summary`/`title`/`reason` arguments just
        // supplied). Capturing that as `result_text` let the B3 grounding
        // pre-check compare an agent's claim against its own words and
        // trivially pass every time — and, worse, a task JSON that happened
        // to exceed the audit char cap could truncate exactly the echoed
        // span and flip the verdict to a false reject. Skip `result_text`
        // capture entirely for these tools; they still get the `input` +
        // `success` audit fields like every other state-changing tool, only
        // the grounding-evidence field is suppressed.
        // P2-C: workspace answers carry file paths and file content; the
        // audit row keeps only the argument summary (no result text).
        let result_text_for_grounding = if duduclaw_core::grounding::is_self_echo_tool(tool_name)
            || crate::mcp_dispatch::COMPUTER_WORKSPACE_TOOLS.contains(&tool_name)
        {
            None
        } else {
            extract_tool_result_text(&result)
        };
        // `computer_type`'s text never reaches the audit (F5): only its length.
        let audit_arguments = audit_safe_arguments(tool_name, &arguments);
        duduclaw_security::audit::append_tool_call_with_input(
            home_dir,
            &actual_agent,
            tool_name,
            &params_summary,
            success,
            Some(&audit_arguments),
            result_text_for_grounding.as_deref(),
        );
    }

    // ── WP6: channel-action → dashboard live feedback ──────────
    // Everything this subprocess persists on behalf of a channel command
    // (a cron routine, a memory write, a synthesised skill) is invisible to
    // the dashboard until the operator reloads — and invisible reads as
    // broken. Raise one whitelisted `events.db` row; the gateway's existing
    // `spawn_events_db_poll` tail pushes it to every connected dashboard,
    // which refetches the matching page. Best-effort: never affects `result`.
    //
    // The caller agent is resolved the same way the audit trail resolves it
    // (`resolve_audit_agent` — a genuine agent-to-agent delegation is
    // attributed to the real sender, a system/scheduler sender is not) and
    // falls back to the memory write namespace, which is exactly the key
    // `memory.db` rows are stored under and therefore exactly what
    // MemoryBrowser filters on.
    let feedback_agent = resolve_audit_agent(|| {
        if ns_ctx.write_namespace.is_empty() {
            default_agent.to_string()
        } else {
            ns_ctx.write_namespace.clone()
        }
    });
    duduclaw_gateway::dashboard_feedback::emit_for_tool(
        home_dir,
        tool_name,
        &arguments,
        &result,
        &feedback_agent,
    )
    .await;

    jsonrpc_response(id, result)
}
