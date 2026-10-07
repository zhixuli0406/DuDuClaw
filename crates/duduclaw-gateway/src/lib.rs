#![recursion_limit = "512"]
#![allow(unused_mut)]
#![allow(unused_variables)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::collapsible_else_if)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::should_implement_trait)]
#![allow(clippy::suspicious_open_options)]
#![allow(clippy::manual_strip)]
#![allow(clippy::redundant_closure)]
#![allow(clippy::useless_format)]
#![allow(clippy::needless_return)]
#![allow(clippy::map_identity)]
#![allow(clippy::map_unwrap_or)]
#![allow(clippy::type_complexity)]
#![allow(clippy::manual_is_multiple_of)]
#![allow(clippy::manual_div_ceil)]
#![allow(clippy::ptr_arg)]
#![allow(clippy::redundant_pattern_matching)]
#![allow(clippy::io_other_error)]
#![allow(private_interfaces)]
#![allow(clippy::needless_borrow)]
#![allow(clippy::needless_borrows_for_generic_args)]
#![allow(clippy::let_and_return)]
#![allow(clippy::unnecessary_map_or)]
#![allow(clippy::collapsible_str_replace)]
#![allow(clippy::new_without_default)]
#![allow(clippy::manual_flatten)]
#![allow(clippy::unwrap_or_default)]
#![allow(clippy::sliced_string_as_bytes)]
#![allow(clippy::if_same_then_else)]
pub mod a2a_signing;
pub mod access_control;
pub mod workflow;
pub mod review_evidence;
pub mod workflow_drafts;
pub mod workflow_draft_context;
pub mod agent_binding;
pub mod agent_hook_installer;
pub mod auth;
// ── D7: "every Claude account failed authentication" alarm bell ──────────
pub mod auth_outage;
// ── WP-B: appliance-image device management (`device.*` dashboard RPCs) ──
pub mod device;
// ── System-settings app: `device.about` / `device.timedate*` data ─────────
pub mod device_about;
pub mod device_ops;
// ── O16: the single authority behind every `os_*` device/system capability.
// The MCP tools (`duduclaw-cli::mcp_os_ops`), the operator CLI leaves
// (`duduclaw-cli::os_drive`) and the dashboard `device.*`/`network.*` RPCs
// are all thin adapters over this module — gate, parse, render, nothing
// more. See its module doc for what is deliberately NOT consolidated. ─────
pub mod os_ops;
pub mod os_update;
pub mod pre_update_backup;
// ── A7c: agent→display gateway bridge (comp's shell_control display group,
// reachable from an agent identity via A7c's PeerAuthority::Agent tier) ──
pub mod display_bridge;
// ── Y10-1: agent→audio gateway bridge (wpctl/PipeWire volume/mute/output —
// never touches duduclaw-comp at all, see this module's own doc for why) ──
pub mod audio_bridge;
// ── H3g-b: surface a failed /data migration (H3g) to the dashboard ───────
pub mod migration_alert;
// ── D4a: network settings (Wi-Fi over iwd D-Bus) — `network.*` RPCs +
// `/api/first-run/network/*` OOBE pre-auth endpoints ─────────────────────
pub mod network;
// ── IMPL-POWER: the appliance lock screen's login-free power surface ─────
pub mod power_local;
// ── WP-G1: scheduled backups + device-migration restore ──────────────────
pub mod backup_restore;
pub mod backup_schedule;
pub(crate) mod causal_ccr_outbox;
pub mod channel_alerts;
pub mod channel_capabilities;
pub mod channel_format;
pub mod channel_reply;
pub mod channel_settings;
pub mod channel_typing;
pub mod claude_runner;
pub mod config_crypto;
pub mod connector_lifecycle;
pub mod consolidation_failures;
pub mod cost_telemetry;
pub mod decision_action;
pub mod decision_capture;
pub mod decision_card;
pub mod decision_gate;
pub mod decision_message_store;
pub mod decision_notify;
pub mod dingtalk;
pub mod doctor_probes;
pub mod extension;
pub mod github_workspace;
pub mod google_apps_script;
pub mod google_service_account;
pub mod google_workspace;
pub mod googlechat;
pub mod markdown_render;
pub mod mcp_external;
pub mod mcp_internal_key;
pub mod msteams;
pub mod notion_workspace;
pub mod rate_limit_watch;
pub(crate) mod synthetic_connector_adapter;
#[cfg(test)]
pub(crate) mod test_channel_provider;
pub mod watchdog;
pub mod killswitch_triggers;
pub mod redaction_sources;
pub mod webhook_jwt;
pub mod webhook_slots;
pub mod wecom;
// WP1.6 (ecosystem): text-reply decisions — replying to a decision card with
// a bare verb counts as a button press (wrist/watch clients have no buttons).
pub mod decision_text;
// P0-B F4: what counts as a channel decision reply (verb + full request id),
// Discord decision permits, Slack decision-identity status.
pub mod channel_decision_route;
// W2-4 notification governance: the gate every outbound notification passes
// through (levels + quiet hours + deferred queue), its action-rate telemetry,
// and the scheduled daily digest.
pub mod autopilot_engine;
pub mod autopilot_notify;
pub mod autopilot_screen;
pub mod autopilot_store;
pub mod branding;
pub mod cep_matcher;
pub mod cron_scheduler;
pub mod cron_store;
pub mod cron_templates;
pub mod departments;
pub mod distributor_store;
pub mod license_runtime;
pub mod license_seed;
pub mod license_serve;
pub mod notify_digest;
pub mod notify_governance;
// O5 — `push(card, dest)`: the single outbound entry the four notification
// modules (goal / approval / install / autopilot) share. Authorization and
// card rendering deliberately stay with them.
pub(crate) mod notify_push;
pub mod notify_stats;
pub mod partner_store;
pub mod posture_watch;
pub mod premium_templates;
pub mod security_autopilot;
pub mod security_rules_seed;
pub mod takeover;
pub mod task_store;
/// Per-agent task sandbox (`[container] sandbox_enabled`), built on the
/// Discovery attempt-container blocks.
pub mod task_sandbox;
pub mod tick_config;
pub mod tick_headers;
pub mod tick_source;
pub mod tick_source_poll;
pub mod tick_source_ws;
// WP-E2: box-side relay client (crates/duduclaw-relay's WebSocket
// counterpart) — reuses tick_source_ws's reconnect-backoff shape, hence the
// grouping alongside the resident-sensing modules above.
pub mod approval;
pub mod approval_notify;
pub mod audit_export;
pub mod budget;
pub mod canvas;
pub mod capability;
pub mod capability_grants;
pub mod channel_link;
pub mod cli_auth;
pub mod cli_noise;
pub mod codrive;
pub mod cost_anomaly;
pub mod custom_skills;
pub mod custom_widgets;
pub mod dashboard_feedback;
pub mod dashboard_navigate;
pub mod db_source_grants; // §13.7 WP-A: per-agent [capabilities] db_sources grants
pub mod db_sources_rpc; // §13.7 WP-D: operator RPCs for [db_sources.*]
pub mod deep_link;
pub mod delegation;
/// WP21 C1 — delegation gate on the bus-consumption path (`dispatcher.rs`).
pub mod delegation_gate;
pub mod delegation_router;
pub mod direct_api;
/// B0-1 exploration-tree ledger, replay simulator and policy protocol (offline only).
pub mod discovery;
pub mod discord;
pub mod discord_voice;
pub mod dispatcher;
/// WP-4G — resource ceilings applied to inbound office / compressed documents
/// before any parser (LibreOffice, the bundled Python skills) is handed them.
pub mod document_limits;
pub mod email;
pub mod ephemeral;
pub mod events_store;
pub mod expert_admin;
pub mod expert_generate;
pub mod external_factors;
pub mod footprint_distill;
pub mod foresight;
pub mod growth;
pub mod guardrail;
pub mod handlers;
// G5 (2026-09 feature audit) — one `config.toml [identity]` → provider
// builder shared by the dashboard RPC, the channel `<sender>` block, and the
// `identity_resolve` MCP tool (the latter two used to hard-code wiki-cache).
pub mod identity_provider;
pub mod interruptibility;
pub mod knowledge_guard;
pub(crate) mod local_session;
pub mod maintenance;
pub mod mast;
pub mod memory_factory;
pub mod memory_migrate;
pub mod memory_forget_approval;
pub mod memory_forget_steps;
pub mod wiki_host_sources;
pub mod memory_provenance;
pub mod message_queue;
pub mod miniapp;
// G4 (2026-09 feature audit) — Odoo ERP change bridge: `/webhook/odoo` +
// background poller → autopilot bus. Both transports default OFF.
pub mod odoo_events;
pub mod os_events;
pub mod os_frontmost;
pub mod persona_induction;
pub mod proactive_feedback;
pub mod proactive_gate;
pub mod profile_distill;
pub mod redteam;
pub mod relay_client;
pub mod relay_config;
pub mod relay_device;
pub mod rule_induction;
pub mod secaudit_reports;
pub mod security_posture;
pub mod setup_token_wizard;
pub mod situation_classifier;
// WP5c — conversation → knowledge-base semantic routing.
pub mod auto_wiki_page;
pub mod builtin_skills_seed_migration;
/// X1 方案 2 — the tool-call audit trail as the causal graph's first real source.
pub mod causal_audit_ingest;
pub mod causal_extraction_runner;
mod causal_mcp_source;
pub mod ccr_dashboard;
pub mod ccr_replay;
pub mod ccr_runtime;
pub mod channel_sender;
pub mod chat_commands;
pub mod computer_use;
pub mod computer_use_image;
pub mod computer_use_orchestrator;
pub mod computer_use_sessions;
pub mod computer_workspaces;
pub mod decision_brief;
pub mod decision_calibration;
pub mod decision_dashboard;
pub mod decision_empirical;
pub mod decision_event;
pub mod decision_forecast_dashboard;
pub mod decision_ingest;
pub mod decision_model_candidate;
pub mod decision_model_candidate_dashboard;
pub mod decision_model_review;
pub mod decision_odoo_export;
pub mod decision_operator_import;
pub mod decision_outcome_calibration;
pub mod decision_outcome_dashboard;
pub mod decision_policy;
pub mod decision_sensitivity;
pub mod decision_shadow_dashboard;
pub mod decision_shadow_screen;
pub mod decision_sim;
pub mod decision_sla_shadow_dashboard;
pub mod decision_sla_shadow_screen;
pub mod decision_store;
pub mod decision_synthetic;
pub mod decision_task_board_export;
pub mod decision_task_board_shadow;
pub mod defensive_prompt;
// Three failover layers under one tree: `failover::model` (model-level
// timeout/overload fallback, formerly the top-level `llm_fallback` module)
// alongside `FailoverManager` (runtime-level). See the module header.
pub mod failover;
pub mod fault_attribution;
pub mod feishu;
pub mod files_api;
pub(crate) mod fs_safe;
pub mod governance;
pub mod gvu;
pub mod install_notify;
pub mod install_requests;
pub mod knowledge_route;
pub mod lifecycle_flush;
pub mod channel_ingress;
pub mod line;
pub mod local_llm;
pub mod log;
pub mod mail;
pub mod mail_worker;
pub mod mcp_oauth;
pub mod mcp_scan;
pub mod mdns;
pub mod media;
pub mod metrics;
pub mod model_capabilities;
pub mod office_docs;
pub mod otel;
pub mod otp_delivery;
pub(crate) mod pending_account;
pub(crate) mod pending_network;
pub mod playbook;
pub mod prediction;
pub mod prompt_audit;
pub mod prompt_compression;
pub mod prompt_identity;
pub mod prompt_minimal;
pub mod protocol;
pub mod pty_runtime;
pub mod ranked_wiki_injection;
pub mod reflexion;
pub mod relevance_ranker;
pub mod reminder_scheduler;
pub mod risk_detector;
pub mod run_steps;
pub mod runtime;
pub mod runtime_config;
pub mod runtime_dispatch;
pub mod runtime_install;
pub mod runtime_models;
pub mod screenshot_audit;
pub mod search_index;
/// Credential redaction for operator-visible channel diagnostics (WP12).
pub mod secret_redact;
pub mod server;
pub mod session;
pub mod session_portability;
pub mod session_summarizer;
pub mod session_summarizer_task;
pub mod session_titler_task;
pub mod skill_approval;
pub mod skill_gap_digest;
pub mod skill_lifecycle;
pub mod slack;
pub mod stt;
pub mod task_spec;
pub mod telegram;
pub mod tts;
pub mod uki_patch;
pub mod updater;
pub mod web_extract;
pub mod web_fetch;
pub mod webchat;
pub mod whatsapp;
pub mod wiki_ingest;
mod wiki_mcp_source;
pub mod wiki_trust_federation;
pub mod workforce_private;
pub mod xml_fence;
pub mod decide;

// ── Hermes-learnings modules (Phase 3, 4, 6) ──
pub mod rl;
pub mod skill_extraction;

// ── Sprint N P0: EvolutionEvents JSONL audit log ──
pub mod evolution_events;
pub mod skill_synthesis_pipeline;

// ── Dashboard-authored redaction rules + imported rule packs (§13) ──
pub mod redaction_custom_rules;
// `redaction.model.*` — install / inspect / remove the local NER model
// that the "AI 智慧偵測" rule set needs.
pub mod redaction_ner_model;

// ── RFC-23 redaction-pipeline integration shim ──
pub mod redaction_integration;

// ── RFC-23 §13.6: redaction for external MCP servers + the direct-API loop ──
pub mod redaction_proxy;

pub use extension::{GatewayExtension, NullExtension};
pub use server::{GatewayConfig, start_gateway};

/// Process-wide HTTP client shared by channel integrations that reconnect in
/// a loop (e.g. Slack Socket Mode) — reuses connection pools instead of
/// rebuilding a client per reconnect (Fix CR-G9).
pub fn shared_http_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_default()
    })
}

// ── G3: event-triggered cron (condition script + on_exit) ──
pub mod condition_eval;

// ── R1: lightweight deterministic trajectory anomaly detection ──
pub mod trajectory_guard;

// ── N1–N4: Night Engine idle-time compute suite ──
pub mod night_engine;
pub mod night_llm;

// WP-A3 (task-forward-model design, 2026-08-06): shared `tool_calls.jsonl`
// record shape + window filter, used by both `dispatch_engine` (judge
// evidence block) and `prediction::task_observe` (A3 observation layer).
pub mod recent_actions;
pub mod tool_activity;

// WP-F (P2-c): durable per-task file-change evidence behind the dashboard's
// needs_human 「變更」tab — persisted from the same native-tool collector,
// merged with the MCP audit window at read time.
pub mod task_changes;

// I-2b 產物物件化: provenance for every file that lands in `attachments/` —
// which agent handed it over, for which task/round, declared with `📎DELIVER:`
// or recovered by the sweep, versus a file a human sent in. Backs the task
// detail page's 「產物」tab and the `/files` origin column.
pub mod artifacts;

// WP-4H: zero-LLM delivery gate for the `📎DELIVER:` outbound path —
// deterministic hard-fail (corrupt/empty/magic-mismatched file → never sent)
// and soft-warn (placeholder residue → logged, not blocked) checks that run
// right before `office_docs::deliver_one` archives + sends a file.
pub mod artifact_gate;

// ── D3 (LWM incident): per-agent authoritative working state — pinned
//    key-value block + handoff note injected into every wake-up, updated
//    only via explicit MCP tools with CAS supersession (ghost-memory fix,
//    A-TMA arXiv:2607.01935 / Letta memory-block pattern) ──
pub mod working_state;

// ── WP-6F (agent presets P1): the agent-visible "目前職務組合" dynamic-tail
//    line — same injection pipeline as `working_state`, one section earlier
//    (design §3.2 trace ③: a preset switch must be visible to the agent
//    itself, not just to the dashboard/audit log) ──
pub mod preset_prompt;

// ── Local-model marketplace backend (`localmodels.*` RPCs): HF intent
//    sweep + hardware fit via duduclaw-inference::model_registry::market,
//    install-job registry over the resumable downloader ──
pub mod local_models;

// ── WP-D: appliance local-model control plane (`inference.local.*` RPCs) —
//    the verified six-model catalog, one-click download, and the env-file +
//    service (re)start that points the image's llama.cpp server at a
//    downloaded GGUF. Narrow, appliance-shaped counterpart to the
//    open-ended `localmodels.*` marketplace above; shares its job registry ──
pub mod inference_local;
// ── WP-E: fine-tuning / post-training (`finetune.*` RPCs). Curate here,
//    train elsewhere, deploy here — the appliance's iGPU cannot train, so
//    this module only builds datasets, ships them to a GPU the user
//    supplies, and imports the resulting GGUF/LoRA back into
//    `<DUDUCLAW_HOME>/models` (the same dir `local_models` scans) ──
pub mod finetune;

// ── G1: durable multi-agent dispatch engine (atomic claim / zombie reclaim /
//        dependency unlock / goal-mode judge acceptance) ──
pub mod dispatch_engine;

// ── Y8-3 T1: agent-body update vertical slice — cross-restart update result
//        reconciliation sweep, piggy-backed on `dispatch_engine`'s tick ──
pub mod update_report_reconcile;

// ── P1: autonomous goal loop — outer-loop driver that dispatches goal_mode
//        tasks, enforces iteration/wall-clock/concurrency caps, and re-dispatches
//        judge-rejected tasks with feedback ──
pub mod goal_loop;
/// P2-A: continuous responsibilities, bounded wake-ups, steering, stop.
pub mod responsibility;
#[cfg(test)]
pub(crate) mod model_call_probe;
// ── Audit O8 (2026-09-29): the eight single-purpose goal-loop modules that
//        used to live at the crate root were merged into three siblings under
//        `goal_loop/` — `signals` (A1 gap fingerprint / A2 visit graph / H10
//        tool streak / H5 bail detect), `state` (A1 <state> block / H11 pause
//        reason / WP-4F best round), and `plan` (D4 decomposition + plan-first).
//        The aliases below keep the old `crate::goal_*` / `crate::pause_reason`
//        paths compiling for one release; new code should use the
//        `goal_loop::{signals, state, plan}` paths directly. ──
pub use goal_loop::signals as goal_bail_detect;
pub use goal_loop::signals as goal_gap_fingerprint;
pub use goal_loop::signals as goal_tool_streak;
pub use goal_loop::signals as goal_visit_graph;
pub use goal_loop::state as goal_budget_best_round;
pub use goal_loop::state as goal_state;
// ── D4: pluggable dispatch policy (agent selection = data) + LLMCompiler-style
//        goal decomposition (planner → dependency DAG) ──
pub mod dispatch_policy;
// ── WP-5D: the acceptance judge as a REAL seam ("everything is a plugin"
//        design §2 row 8 / §6-P1) — `[dispatch] judge` selects
//        mav | external (evaluator_only / human_only removed in v1.69.0);
//        every failure path falls back to `mav`, the strongest verifier ──
pub mod judge_mode;
// ── Audit O8 alias — see the `goal_loop` block above ──
pub use goal_loop::plan as goal_plan;
// ── Audit T5/O4 (2026-09-29): the single place an autonomous goal is created.
//        Shared by the dashboard `tasks.goal_create` RPC and the MCP
//        `tasks_create kind="goal"` entry so the H9-G contract freeze and the
//        I-1c plan-first parking can never drift between the two rails ──
pub mod goal_create_core;
// ── D5: semi-automatic topology evolution (edge optimization, human-gated) ──
pub mod topology_evolution;
// ── WP2.4: structured outcome acceptance — deterministic (zero-LLM) validation
//         of a goal's ```json / files:<glob> contract before the MAV judge ──
pub mod outcome_spec;
// ── P2a: goal-loop channel push + decision (needs_human exit + autonomy kickoff) ──
pub mod goal_notify;
pub(crate) mod goal_notify_private;
// ── H11: closed classification of WHY a goal task parked `needs_human`
//        (grok-build §2.3 eight-state machine, adapted — a reason column, not
//        a new task status). Audit O8 alias — lives in `goal_loop::state` ──
pub use goal_loop::state as pause_reason;
// ── WP2.2: gateway-side subprocess driver for `duduclaw eval --replay`
//         (B1 cli↔gateway dependency-direction boundary) ──
pub mod eval_runner;
// ── Read-once "rules/model changed" FYI marker for the channel reply path ──
pub mod pending_agent_notice;
// ── Belief loop × goal contract, gap 2 (design-market-belief-loop-2026-08.md
//        §3 「自主研究」) — per-agent nightly self-study goal creation when
//        today produced a belief miss ──
pub mod self_study;
// ── P0: channel-side goal intent router (DESIGN-goal-intent-router-2026-08.md)
//        — upgrades plain-language delegation typed into any of the 11 chat
//        channels into a confirmable goal-task suggestion, without a new
//        cloud LLM call ──
pub mod goal_intent;
// ── O-1: system-operation intent router (DESIGN-agent-os-native-apps-2026-08.md
//        §6.3 O-1) — natural language → the O-0 os_* tool face, param
//        completion, and safety triage. Routing only, never execution; the
//        O-0 tools' own gate chain is unchanged ──
pub mod os_intent;
// ── O-4: system-operator agent persona/guardrails — wires O-1's intent
//        router into the conversational reply path for agents explicitly
//        capability-gated as system operators ([capabilities]
//        system_operator = true). Never calls an O-0 os_* tool handler
//        itself; only shapes the turn (short-circuit reply or a guiding
//        hint) — execution stays behind the O-0 tools' own unchanged gates ──
pub mod os_operator;

/// `role_turns.jsonl` — per-role-member attribution rows.
pub mod role_turns;
/// Team-as-Agent (P1/WP-4): per-task team spec freeze, the decomposability
/// gate, and the planner → executor(s) → verifier round.
pub mod team_composer;
