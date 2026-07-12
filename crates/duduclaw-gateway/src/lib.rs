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
pub mod config_crypto;
pub mod claude_runner;
pub mod cost_telemetry;
pub mod mcp_external;
pub mod decision_capture;
pub mod cron_scheduler;
pub mod cron_store;
pub mod license_runtime;
pub mod task_store;
pub mod partner_store;
pub mod branding;
pub mod distributor_store;
pub mod license_serve;
pub mod autopilot_store;
pub mod autopilot_engine;
pub mod approval;
pub mod capability;
pub mod growth;
pub mod custom_skills;
pub mod audit_export;
pub mod budget;
pub mod cost_anomaly;
pub mod guardrail;
pub mod redteam;
pub mod mast;
pub mod foresight;
pub mod security_posture;
pub mod events_store;
pub mod canvas;
pub mod direct_api;
pub mod delegation;
pub mod delegation_router;
pub mod discord;
pub mod discord_voice;
pub mod email;
pub mod dispatcher;
pub mod ephemeral;
pub mod message_queue;
pub mod external_factors;
pub mod cli_auth;
pub mod handlers;
pub mod line;
pub mod local_llm;
pub mod mcp_oauth;
pub mod media;
pub mod model_capabilities;
pub mod tts;
pub mod stt;
pub mod lifecycle_flush;
pub mod log;
pub mod metrics;
pub mod otel;
pub mod failover;
pub mod gvu;
pub mod prediction;
pub mod reflexion;
pub mod run_steps;
pub mod runtime;
pub mod runtime_config;
pub mod runtime_dispatch;
pub mod prompt_audit;
pub mod prompt_compression;
pub mod prompt_minimal;
pub mod protocol;
pub mod pty_runtime;
pub mod runtime_models;
pub mod runtime_status;
pub mod worker_supervisor;
pub mod ranked_wiki_injection;
pub mod relevance_ranker;
pub mod session_summarizer;
pub mod session_summarizer_task;
pub mod channel_approval;
pub mod credit;
pub mod delegation_scope;
pub mod governance;
pub mod workforce_private;
pub mod skill_approval;
pub mod skill_lifecycle;
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

// ── G1: durable multi-agent dispatch engine (atomic claim / zombie reclaim /
//        dependency unlock / goal-mode judge acceptance) ──
pub mod dispatch_engine;
