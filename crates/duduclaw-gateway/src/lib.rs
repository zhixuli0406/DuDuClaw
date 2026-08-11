#![recursion_limit = "256"]
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
pub mod agent_binding;
pub mod agent_hook_installer;
pub mod auth;
pub mod channel_alerts;
pub mod channel_format;
pub mod channel_reply;
pub mod markdown_render;
pub mod channel_typing;
pub mod webhook_jwt;
pub mod googlechat;
pub mod msteams;
pub mod wecom;
pub mod dingtalk;
pub mod extension;
pub mod channel_settings;
pub mod google_apps_script;
pub mod google_service_account;
pub mod google_workspace;
pub mod notion_workspace;
pub mod github_workspace;
pub mod config_crypto;
pub mod claude_runner;
pub mod cost_telemetry;
pub mod doctor_probes;
pub mod mcp_external;
pub mod mcp_internal_key;
pub mod decision_action;
pub mod decision_capture;
pub mod decision_card;
pub mod decision_message_store;
pub mod decision_notify;
pub mod cron_scheduler;
pub mod cron_store;
pub mod cron_templates;
pub mod license_runtime;
pub mod task_store;
pub mod partner_store;
pub mod departments;
pub mod premium_templates;
pub mod branding;
pub mod distributor_store;
pub mod license_serve;
pub mod license_seed;
pub mod autopilot_store;
pub mod autopilot_engine;
pub mod autopilot_notify;
pub mod cep_matcher;
pub mod rule_induction;
pub mod approval;
pub mod approval_notify;
pub mod deep_link;
pub mod expert_admin;
pub mod expert_generate;
pub mod capability;
pub mod capability_grants;
pub mod growth;
pub mod custom_skills;
pub mod custom_widgets;
pub mod audit_export;
pub mod budget;
pub mod cost_anomaly;
pub mod guardrail;
pub mod redteam;
pub mod mast;
pub mod foresight;
pub mod security_posture;
pub mod events_store;
pub mod dashboard_feedback;
pub mod os_events;
pub mod os_frontmost;
pub mod interruptibility;
pub mod proactive_gate;
pub mod proactive_feedback;
pub mod footprint_distill;
pub mod profile_distill;
pub mod persona_induction;
pub mod situation_classifier;
pub mod canvas;
pub mod direct_api;
pub mod delegation;
/// WP21 C1 — delegation gate on the bus-consumption path (`dispatcher.rs`).
pub mod delegation_gate;
pub mod delegation_router;
pub mod discord;
pub mod discord_voice;
pub mod email;
pub mod dispatcher;
pub mod ephemeral;
pub mod message_queue;
pub mod external_factors;
pub mod cli_auth;
pub mod cli_noise;
pub mod handlers;
pub mod knowledge_guard;
pub mod memory_factory;
// WP5c — conversation → knowledge-base semantic routing.
pub mod knowledge_route;
pub mod auto_wiki_page;
pub mod line;
pub mod local_llm;
pub mod install_notify;
pub mod install_requests;
pub mod mcp_oauth;
pub mod mcp_scan;
pub mod media;
pub mod model_capabilities;
pub mod office_docs;
pub mod tts;
pub mod stt;
pub mod lifecycle_flush;
pub mod log;
pub mod metrics;
pub mod otel;
pub mod failover;
pub mod gvu;
pub mod playbook;
pub mod prediction;
pub mod reflexion;
pub mod run_steps;
pub mod runtime;
pub mod runtime_config;
pub mod runtime_dispatch;
pub mod prompt_audit;
pub mod prompt_compression;
pub mod prompt_identity;
pub mod prompt_minimal;
pub mod protocol;
pub mod builtin_skills_seed_migration;
pub mod pty_default_migration;
pub mod pty_runtime;
pub mod files_api;
pub mod runtime_install;
pub mod runtime_models;
pub mod runtime_status;
pub mod worker_supervisor;
pub mod ranked_wiki_injection;
pub mod relevance_ranker;
pub mod session_summarizer;
pub mod session_summarizer_task;
pub mod session_titler_task;
pub mod credit;
pub mod delegation_scope;
pub mod governance;
pub mod workforce_private;
pub mod skill_approval;
pub mod skill_gap_digest;
pub mod skill_lifecycle;
pub mod mdns;
pub mod server;
pub mod session;
pub mod session_portability;
pub mod task_spec;
pub mod telegram;
pub mod slack;
pub mod channel_sender;
pub mod otp_delivery;
pub mod chat_commands;
pub mod computer_use;
pub mod computer_use_orchestrator;
pub mod browser_router;
pub mod screenshot_audit;
/// Credential redaction for operator-visible channel diagnostics (WP12).
pub mod secret_redact;
pub mod risk_detector;
pub mod defensive_prompt;
pub mod updater;
pub mod webchat;
pub mod webhook;
pub mod web_extract;
pub mod web_fetch;
pub mod whatsapp;
pub mod feishu;
pub mod reminder_scheduler;
pub mod wiki_ingest;
pub mod wiki_trust_federation;
pub mod worktree;

// ── Hermes-learnings modules (Phase 3, 4, 6) ──
pub mod rl;
pub mod skill_extraction;

// ── Sprint N P0: EvolutionEvents JSONL audit log ──
pub mod evolution_events;
pub mod skill_synthesis_pipeline;

// ── LLM fallback helpers (timeout / rate-limit → lighter model) ──
pub mod llm_fallback;

// ── RFC-23 redaction-pipeline integration shim ──
pub mod redaction_integration;

pub use extension::{GatewayExtension, NullExtension};
pub use server::{start_gateway, GatewayConfig};

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
pub mod tool_activity;

// ── G1: durable multi-agent dispatch engine (atomic claim / zombie reclaim /
//        dependency unlock / goal-mode judge acceptance) ──
pub mod dispatch_engine;

// ── P1: autonomous goal loop — outer-loop driver that dispatches goal_mode
//        tasks, enforces iteration/wall-clock/concurrency caps, and re-dispatches
//        judge-rejected tasks with feedback ──
pub mod goal_loop;
// ── A1: structured <state> block (StateAct arXiv:2410.02810) round-tripped
//        through the goal loop's dispatch prompt + agent self-report ──
pub mod goal_state;
// ── A2: (state_hash, action) visit graph (Graph-Based Exploration
//        arXiv:2512.24156) — structural loop detection, replaces the old
//        two-round identical-feedback oscillation guard ──
pub mod goal_visit_graph;
// ── D4: pluggable dispatch policy (agent selection = data) + LLMCompiler-style
//        goal decomposition (planner → dependency DAG) ──
pub mod dispatch_policy;
// ── D5: semi-automatic topology evolution (edge optimization, human-gated) ──
pub mod topology_evolution;
pub mod goal_plan;
// ── WP2.4: structured outcome acceptance — deterministic (zero-LLM) validation
//         of a goal's ```json / files:<glob> contract before the MAV judge ──
pub mod outcome_spec;
// ── P2a: goal-loop channel push + decision (needs_human exit + autonomy kickoff) ──
pub mod goal_notify;
// ── WP2.2: gateway-side subprocess driver for `duduclaw eval --replay`
//         (B1 cli↔gateway dependency-direction boundary) ──
pub mod eval_runner;
// ── Read-once "rules/model changed" FYI marker for the channel reply path ──
pub mod pending_agent_notice;
