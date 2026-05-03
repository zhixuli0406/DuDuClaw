#![recursion_limit = "256"]
#![allow(dead_code)]
#![allow(unused_imports)]
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
pub mod agent_hook_installer;
pub mod auth;
pub mod channel_format;
pub mod channel_reply;
pub mod extension;
pub mod channel_settings;
pub mod config_crypto;
pub mod claude_runner;
pub mod cost_telemetry;
pub mod cron_scheduler;
pub mod cron_store;
pub mod task_store;
pub mod partner_store;
pub mod autopilot_store;
pub mod autopilot_engine;
pub mod events_store;
pub mod direct_api;
pub mod delegation;
pub mod discord;
pub mod discord_voice;
pub mod dispatcher;
pub mod message_queue;
pub mod evolution;
pub mod external_factors;
pub mod handlers;
pub mod line;
pub mod mcp_oauth;
pub mod media;
pub mod tts;
pub mod log;
pub mod metrics;
pub mod gvu;
pub mod prediction;
pub mod protocol;
pub mod skill_lifecycle;
pub mod server;
pub mod session;
pub mod sticker;
pub mod task_spec;
pub mod telegram;
pub mod channel_sender;
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
pub mod reminder_scheduler;
pub mod wiki_ingest;
pub mod wiki_trust_federation;
pub mod worktree;

// ── Hermes-learnings modules (Phase 3, 4, 6) ──
pub mod compression;
pub mod rl;
pub mod skill_extraction;
pub mod tool_classifier;

// ── Sprint N P0: EvolutionEvents JSONL audit log ──
pub mod evolution_events;
pub mod skill_synthesis_pipeline;

// ── LLM fallback helpers (timeout / rate-limit → lighter model) ──
pub mod llm_fallback;

pub use extension::{GatewayExtension, NullExtension};
pub use server::{start_gateway, GatewayConfig};
