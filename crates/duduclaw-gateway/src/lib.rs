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
pub mod agent_binding;
pub mod agent_hook_installer;
pub mod auth;
// ── WP-B: appliance-image device management (`device.*` dashboard RPCs) ──
pub mod device;
// ── System-settings app: `device.about` / `device.timedate*` data ─────────
pub mod device_about;
pub mod device_ops;
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
pub mod watchdog;
pub mod channel_alerts;
pub mod channel_capabilities;
pub mod channel_format;
pub mod channel_reply;
pub mod rate_limit_watch;
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
pub mod consolidation_failures;
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
// WP1.6 (ecosystem): text-reply decisions — replying to a decision card with
// a bare verb counts as a button press (wrist/watch clients have no buttons).
pub mod decision_text;
// W2-4 notification governance: the gate every outbound notification passes
// through (levels + quiet hours + deferred queue), its action-rate telemetry,
// and the scheduled daily digest.
pub mod notify_digest;
pub mod notify_governance;
pub mod notify_stats;
pub mod cron_scheduler;
pub mod cron_store;
pub mod cron_templates;
pub mod license_runtime;
pub mod takeover;
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
pub mod autopilot_screen;
pub mod cep_matcher;
pub mod tick_config;
pub mod tick_headers;
pub mod tick_source;
pub mod tick_source_poll;
pub mod tick_source_ws;
// WP-E2: box-side relay client (crates/duduclaw-relay's WebSocket
// counterpart) — reuses tick_source_ws's reconnect-backoff shape, hence the
// grouping alongside the resident-sensing modules above.
pub mod relay_client;
pub mod relay_config;
pub mod relay_device;
pub mod rule_induction;
pub mod approval;
pub mod approval_notify;
pub mod codrive;
pub mod channel_link;
pub mod deep_link;
pub(crate) mod local_session;
pub mod miniapp;
pub mod expert_admin;
pub mod expert_generate;
pub mod capability;
pub mod capability_grants;
pub mod maintenance;
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
pub mod secaudit_reports;
pub mod events_store;
pub mod dashboard_feedback;
pub mod dashboard_navigate;
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
/// WP-4G — resource ceilings applied to inbound office / compressed documents
/// before any parser (LibreOffice, the bundled Python skills) is handed them.
pub mod document_limits;
pub mod email;
pub mod dispatcher;
pub mod ephemeral;
pub mod message_queue;
pub mod external_factors;
pub mod cli_auth;
pub mod setup_token_wizard;
pub mod cli_noise;
pub mod handlers;
pub mod knowledge_guard;
pub mod memory_factory;
pub mod memory_migrate;
// WP5c — conversation → knowledge-base semantic routing.
pub mod knowledge_route;
pub mod auto_wiki_page;
pub mod line;
pub mod local_llm;
pub mod install_notify;
pub mod install_requests;
pub(crate) mod pending_account;
pub mod mcp_oauth;
pub mod mcp_scan;
pub mod mail;
pub mod mail_worker;
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
pub mod search_index;
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
pub mod uki_patch;
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
pub mod recent_actions;
pub mod tool_activity;
/// Code Mode Phase 0 measurement gate (WP-H2 / WP-6E) — pure observation.
pub mod tool_loop_probe;

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
// ── A1: structured <state> block (StateAct arXiv:2410.02810) round-tripped
//        through the goal loop's dispatch prompt + agent self-report ──
pub mod goal_state;
// ── A2: (state_hash, action) visit graph (Graph-Based Exploration
//        arXiv:2512.24156) — structural loop detection, replaces the old
//        two-round identical-feedback oscillation guard ──
pub mod goal_visit_graph;
// ── H4 (WP-B): stagnation gap fingerprinting — extracts path:line citations
//        + key tokens from judge rejection feedback and normalizes them so a
//        reworded-but-same gap collapses to the same fingerprint, instead of
//        the byte-identical text comparison `StateBlock::hash_input` alone
//        would otherwise perform ──
pub mod goal_gap_fingerprint;
// ── WP-4F: deterministic "best round" picker for budget-exhausted
//        `needs_human` escalations — attaches the closest-to-done round's
//        excerpt + gap list instead of an empty-handed pause note ──
pub mod goal_budget_best_round;
// ── H5 (WP-B): premature-stop ("bail") regex panel — zh+en anchored
//        patterns compared against the last non-empty paragraph of an
//        agent's completion text ──
pub mod goal_bail_detect;
// ── H10: tool-call streak advisory (deepseek-harness §2.16
//        repeat-tool-reminder) — detects a long run of identical
//        (tool, masked-params) calls within one round's evidence and
//        surfaces an escalating [3, 5, 8] zh-TW advisory hint into the
//        NEXT dispatch round's <state> block. Advisory only, zero LLM cost ──
pub mod goal_tool_streak;
// ── D4: pluggable dispatch policy (agent selection = data) + LLMCompiler-style
//        goal decomposition (planner → dependency DAG) ──
pub mod dispatch_policy;
// ── WP-5D: the acceptance judge as a REAL seam ("everything is a plugin"
//        design §2 row 8 / §6-P1) — `[dispatch] judge` selects
//        mav | evaluator_only | external | human_only; every failure path
//        falls back to `mav`, the strongest verifier ──
pub mod judge_mode;
// ── D5: semi-automatic topology evolution (edge optimization, human-gated) ──
pub mod topology_evolution;
pub mod goal_plan;
// ── WP2.4: structured outcome acceptance — deterministic (zero-LLM) validation
//         of a goal's ```json / files:<glob> contract before the MAV judge ──
pub mod outcome_spec;
// ── P2a: goal-loop channel push + decision (needs_human exit + autonomy kickoff) ──
pub mod goal_notify;
// ── H11: closed classification of WHY a goal task parked `needs_human`
//        (grok-build §2.3 eight-state machine, adapted — a reason column, not
//        a new task status) ──
pub mod pause_reason;
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
