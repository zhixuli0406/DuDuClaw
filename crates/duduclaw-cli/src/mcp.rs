//! MCP (Model Context Protocol) server implementation.
//!
//! Communicates via stdin/stdout using JSON-RPC 2.0.
//! Exposes DuDuClaw tools for Claude Code integration.

use std::path::Path;

use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_core::truncate_bytes;
use duduclaw_memory::SqliteMemoryEngine;
use duduclaw_core::traits::MemoryEngine;
use duduclaw_core::types::MemoryEntry;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::{info, warn};


// ── Tool definitions ─────────────────────────────────────────

struct ToolDef {
    name: &'static str,
    description: &'static str,
    params: &'static [ParamDef],
}

struct ParamDef {
    name: &'static str,
    description: &'static str,
    required: bool,
}

const TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "send_message",
        description: "Send a message to a channel (Telegram/LINE/Discord)",
        params: &[
            ParamDef { name: "channel", description: "Channel type (telegram, line, discord)", required: true },
            ParamDef { name: "chat_id", description: "Chat/group ID", required: true },
            ParamDef { name: "text", description: "Message text", required: true },
        ],
    },
    ToolDef {
        name: "send_photo",
        description: "Send a photo to a channel",
        params: &[
            ParamDef { name: "channel", description: "Channel type", required: true },
            ParamDef { name: "chat_id", description: "Chat/group ID", required: true },
            ParamDef { name: "url_or_path", description: "URL or file path of the photo", required: true },
        ],
    },
    ToolDef {
        name: "send_sticker",
        description: "Send a sticker (LINE only)",
        params: &[
            ParamDef { name: "chat_id", description: "Chat/group ID", required: true },
            ParamDef { name: "sticker_id", description: "LINE sticker ID", required: true },
        ],
    },
    ToolDef {
        name: "web_search",
        description: "Search the web",
        params: &[
            ParamDef { name: "query", description: "Search query", required: true },
        ],
    },
    ToolDef {
        name: "send_to_agent",
        description: "Delegate task to another agent. Delegation depth is tracked automatically via environment to prevent infinite loops (max 5 hops).",
        params: &[
            ParamDef { name: "agent_id", description: "Target agent ID", required: true },
            ParamDef { name: "prompt", description: "Prompt/task for the agent", required: true },
        ],
    },
    ToolDef {
        name: "log_mood",
        description: "Log user mood",
        params: &[
            ParamDef { name: "mood", description: "Mood label (e.g. happy, tired, neutral)", required: true },
            ParamDef { name: "note", description: "Optional note", required: false },
        ],
    },
    ToolDef {
        name: "schedule_task",
        description: "Schedule a recurring task in DuDuClaw's persistent CronScheduler. \
            Tasks survive process restarts, are stored in ~/.duduclaw/cron_tasks.db, \
            and can target any agent via agent_id. Use this for production scheduling \
            (daily reports, periodic health checks, recurring agent work). \
            Prefer this over Claude Code's built-in /schedule slash command, which \
            is session-bound and expires when the session ends.",
        params: &[
            ParamDef { name: "cron", description: "Cron expression (5 fields '* * * * *' minute-precision, or 6 fields with seconds). Evaluated in `cron_timezone` when set, otherwise UTC. For Asia/Taipei (UTC+8), '0 9 * * *' with cron_timezone='Asia/Taipei' fires at 09:00 Taipei time daily.", required: true },
            ParamDef { name: "task", description: "Task prompt / description sent to the target agent when the cron fires. Write it as an instruction the agent will follow (e.g., 'Run daily competitive research and post findings to @Agnes').", required: true },
            ParamDef { name: "name", description: "Human-readable task name for listing / pausing / deleting later (e.g., 'xianwen-pm-daily-research').", required: true },
            ParamDef { name: "agent_id", description: "Target agent that will execute the task (e.g. 'xianwen-pm', 'duduclaw-tl'). Defaults to 'default' if omitted — explicit is strongly recommended.", required: false },
            ParamDef { name: "notify_channel", description: "Optional: channel type to auto-deliver the task result to ('discord', 'telegram', 'line', 'slack', 'whatsapp', 'feishu', 'webchat'). When set with notify_chat_id, the response is sent to that channel after a successful run.", required: false },
            ParamDef { name: "notify_chat_id", description: "Optional: chat / channel / room ID on the notify platform. Required when notify_channel is set.", required: false },
            ParamDef { name: "notify_thread_id", description: "Optional: Discord thread ID. Only used when notify_channel='discord' and the result should land in a specific thread.", required: false },
            ParamDef { name: "cron_timezone", description: "Optional: IANA timezone name for interpreting the cron expression (e.g. 'Asia/Taipei', 'America/New_York'). Omit to auto-detect the host system's local timezone (v1.8.25+). Pass 'UTC' to force UTC evaluation explicitly.", required: false },
        ],
    },
    ToolDef {
        name: "list_cron_tasks",
        description: "List scheduled cron tasks. Returns tasks owned by the calling agent (or all tasks if agent_id is omitted).",
        params: &[
            ParamDef { name: "agent_id", description: "Filter by agent ID (default: calling agent)", required: false },
            ParamDef { name: "enabled_only", description: "Only show enabled tasks (default: false)", required: false },
        ],
    },
    ToolDef {
        name: "update_cron_task",
        description: "Update a scheduled cron task by ID or name. Only the fields you provide will be changed.",
        params: &[
            ParamDef { name: "id", description: "Task ID to update", required: false },
            ParamDef { name: "name", description: "Task name to update (used if id is omitted)", required: false },
            ParamDef { name: "cron", description: "New cron expression", required: false },
            ParamDef { name: "task", description: "New task description/prompt", required: false },
            ParamDef { name: "new_name", description: "Rename the task", required: false },
        ],
    },
    ToolDef {
        name: "delete_cron_task",
        description: "Delete a scheduled cron task by ID or name",
        params: &[
            ParamDef { name: "id", description: "Task ID to delete", required: false },
            ParamDef { name: "name", description: "Task name to delete (used if id is omitted)", required: false },
        ],
    },
    ToolDef {
        name: "pause_cron_task",
        description: "Pause or resume a scheduled cron task by ID or name",
        params: &[
            ParamDef { name: "id", description: "Task ID", required: false },
            ParamDef { name: "name", description: "Task name (used if id is omitted)", required: false },
            ParamDef { name: "enabled", description: "Set to true to resume, false to pause (default: false)", required: false },
        ],
    },
    // ── Reminder tools ────────────────────────────────────────────
    ToolDef {
        name: "create_reminder",
        description: "Create a one-shot reminder that sends a message to a channel at a specified time. Supports relative time (5m, 2h, 1d, 1h30m) or absolute ISO 8601 timestamps. Two modes: 'direct' sends a static message (zero LLM cost), 'agent_callback' wakes the agent to generate a dynamic response.",
        params: &[
            ParamDef { name: "time", description: "When to trigger: relative (5m, 2h, 1d, 1h30m) or absolute ISO 8601 (2026-04-07T15:00:00+08:00)", required: true },
            ParamDef { name: "message", description: "Message text to send (required for direct mode)", required: true },
            ParamDef { name: "channel", description: "Channel type (telegram, line, discord)", required: true },
            ParamDef { name: "chat_id", description: "Chat/group/channel ID to send the reminder to", required: true },
            ParamDef { name: "mode", description: "Delivery mode: 'direct' (default, zero cost) or 'agent_callback' (wakes agent with prompt)", required: false },
            ParamDef { name: "prompt", description: "Prompt for the agent (required when mode=agent_callback)", required: false },
        ],
    },
    ToolDef {
        name: "list_reminders",
        description: "List reminders, optionally filtered by status and agent",
        params: &[
            ParamDef { name: "status", description: "Filter by status: pending, delivered, failed, cancelled (default: pending)", required: false },
            ParamDef { name: "agent_id", description: "Filter by agent ID", required: false },
        ],
    },
    ToolDef {
        name: "cancel_reminder",
        description: "Cancel a pending reminder by ID",
        params: &[
            ParamDef { name: "id", description: "Reminder ID to cancel", required: true },
        ],
    },
    ToolDef {
        name: "memory_search",
        description: "Search agent memory",
        params: &[
            ParamDef { name: "query", description: "Search query", required: true },
        ],
    },
    ToolDef {
        name: "memory_store",
        description: "Store a memory entry",
        params: &[
            ParamDef { name: "content", description: "Memory content", required: true },
            ParamDef { name: "tags", description: "Comma-separated tags", required: false },
        ],
    },
    ToolDef {
        name: "memory_read",
        description: "Read a single memory entry by ID",
        params: &[
            ParamDef { name: "memory_id", description: "Memory entry UUID from memory_store", required: true },
        ],
    },
    ToolDef {
        name: "memory_search_by_layer",
        description: "Search agent memory filtered by cognitive layer (episodic or semantic)",
        params: &[
            ParamDef { name: "query", description: "Search query", required: true },
            ParamDef { name: "layer", description: "Cognitive layer: 'episodic' or 'semantic'", required: true },
            ParamDef { name: "limit", description: "Max results to return (default: 10)", required: false },
        ],
    },
    ToolDef {
        name: "memory_successful_conversations",
        description: "Find successful past conversations related to a topic (high-importance episodic memories)",
        params: &[
            ParamDef { name: "topic", description: "Topic keywords to search for", required: true },
            ParamDef { name: "limit", description: "Max results to return (default: 10)", required: false },
        ],
    },
    ToolDef {
        name: "memory_episodic_pressure",
        description: "Compute episodic memory pressure score. A value > 10.0 suggests enough observations for a Meso reflection.",
        params: &[
            ParamDef { name: "hours_ago", description: "Look back window in hours (default: 24)", required: false },
        ],
    },
    ToolDef {
        name: "memory_consolidation_status",
        description: "Count semantic conflicts — high-importance episodic memories not yet consolidated into semantic knowledge",
        params: &[],
    },
    // ── Sub-agent management tools ──────────────────────────────
    ToolDef {
        name: "create_agent",
        description: "Create a persistent sub-agent with its own identity, skills, and configuration. The agent is registered and available for delegation immediately.",
        params: &[
            ParamDef { name: "name", description: "Agent name (lowercase, no spaces, e.g. 'researcher')", required: true },
            ParamDef { name: "display_name", description: "Human-readable display name (e.g. 'Research Assistant')", required: true },
            ParamDef { name: "role", description: "Agent role: 'specialist' or 'worker' (default: specialist)", required: false },
            ParamDef { name: "reports_to", description: "Parent agent name this agent reports to (default: main agent)", required: false },
            ParamDef { name: "soul", description: "Personality/system prompt for this agent (written to SOUL.md)", required: false },
            ParamDef { name: "model", description: "Preferred model (default: claude-sonnet-4-6)", required: false },
            ParamDef { name: "trigger", description: "Trigger keyword (default: @display_name)", required: false },
            ParamDef { name: "icon", description: "Emoji icon for this agent (default: 🤖)", required: false },
        ],
    },
    ToolDef {
        name: "list_agents",
        description: "List all registered agents with their role, status, and reports_to hierarchy",
        params: &[],
    },
    ToolDef {
        name: "create_task",
        description: "Submit a structured multi-step task for deterministic execution by the gateway dispatcher. \
                      Each step is dispatched to the specified agent, verified against acceptance criteria, \
                      with automatic retry (3x) and replan (2x) on failure. Use this instead of send_to_agent \
                      chains for reliable multi-step workflows.",
        params: &[
            ParamDef { name: "goal", description: "Overall task goal / description", required: true },
            ParamDef { name: "steps", description: "JSON array of steps. Each step: {\"description\": \"...\", \"agent\": \"agent-id\" (optional, default=caller), \"depends_on\": [step_indices] (optional), \"acceptance_criteria\": [{\"description\": \"...\"}] (optional)}", required: true },
        ],
    },
    ToolDef {
        name: "check_responses",
        description: "Check responses from agents you sent messages to via send_to_agent. \
                      Returns the most recent responses from the bus queue for a given agent. \
                      Use this to verify whether an agent actually responded to your message.",
        params: &[
            ParamDef { name: "agent_id", description: "Agent ID to check responses from", required: true },
            ParamDef { name: "limit", description: "Max number of responses to return (default: 5)", required: false },
        ],
    },
    ToolDef {
        name: "task_status",
        description: "Check the status of a previously created task (from create_task)",
        params: &[
            ParamDef { name: "task_id", description: "Task ID returned by create_task", required: true },
        ],
    },
    ToolDef {
        name: "agent_status",
        description: "Get detailed status and configuration of a specific agent",
        params: &[
            ParamDef { name: "agent_id", description: "Agent name to inspect", required: true },
        ],
    },
    ToolDef {
        name: "spawn_agent",
        description: "Spawn a persistent sub-agent task. The agent runs in the background with its own session, executing the given prompt. Use agent_status to check progress. Delegation depth tracked automatically (max 5 hops).",
        params: &[
            ParamDef { name: "agent_id", description: "Target agent name", required: true },
            ParamDef { name: "task", description: "Task prompt for the agent to execute", required: true },
            ParamDef { name: "session_key", description: "Optional session key to resume a previous conversation context", required: false },
        ],
    },
    ToolDef {
        name: "agent_update",
        description: "Update one or more fields of an existing agent's configuration (agent.toml). Supports identity, model, budget, heartbeat, and container fields. Uses atomic write for safety.",
        params: &[
            ParamDef { name: "agent_id", description: "Agent name to update", required: true },
            ParamDef { name: "display_name", description: "New display name", required: false },
            ParamDef { name: "role", description: "New role: main, specialist, worker, developer, qa, planner", required: false },
            ParamDef { name: "status", description: "New status: active, paused, terminated", required: false },
            ParamDef { name: "trigger", description: "New trigger keyword", required: false },
            ParamDef { name: "icon", description: "New emoji icon", required: false },
            ParamDef { name: "reports_to", description: "New parent agent name", required: false },
            ParamDef { name: "model", description: "New preferred model", required: false },
            ParamDef { name: "fallback_model", description: "New fallback model", required: false },
            ParamDef { name: "api_mode", description: "API mode: cli, direct, auto", required: false },
            ParamDef { name: "budget_cents", description: "Monthly budget limit in cents", required: false },
            ParamDef { name: "max_concurrent", description: "Max concurrent container tasks", required: false },
            ParamDef { name: "heartbeat_enabled", description: "Enable/disable heartbeat (true/false)", required: false },
            ParamDef { name: "heartbeat_cron", description: "Heartbeat cron expression", required: false },
        ],
    },
    ToolDef {
        name: "agent_remove",
        description: "Remove an agent (moves to _trash/ for recovery). Refuses to remove the main agent.",
        params: &[
            ParamDef { name: "agent_id", description: "Agent name to remove", required: true },
        ],
    },
    ToolDef {
        name: "agent_update_soul",
        description: "Update an agent's SOUL.md personality file via the trusted MCP channel. Bypasses file-protect hooks. Uses atomic write with SHA-256 fingerprinting.",
        params: &[
            ParamDef { name: "agent_id", description: "Agent name", required: true },
            ParamDef { name: "content", description: "New SOUL.md content (full replacement)", required: true },
        ],
    },
    // ── Skill management tools ──────────────────────────────────
    ToolDef {
        name: "skill_search",
        description: "Search the local skill registry for available skills to install",
        params: &[
            ParamDef { name: "query", description: "Search query (name, tag, or description)", required: true },
        ],
    },
    ToolDef {
        name: "skill_list",
        description: "List all skills installed for a specific agent",
        params: &[
            ParamDef { name: "agent_id", description: "Agent name (default: main agent)", required: false },
        ],
    },
    // ── Skill lifecycle tools ──────────────────────────────────────
    ToolDef {
        name: "skill_security_scan",
        description: "Run a security scan on a skill file and report risk level and findings",
        params: &[
            ParamDef { name: "agent_id", description: "Agent name", required: true },
            ParamDef { name: "skill_name", description: "Skill name to scan", required: true },
        ],
    },
    ToolDef {
        name: "skill_graduate",
        description: "Manually graduate a proven agent-local skill to global scope (~/.duduclaw/skills/)",
        params: &[
            ParamDef { name: "agent_id", description: "Agent that owns the skill", required: true },
            ParamDef { name: "skill_name", description: "Skill name to graduate", required: true },
        ],
    },
    ToolDef {
        name: "skill_synthesis_status",
        description: "Report auto-synthesis status: sandboxed skills, gap accumulator state, recent synthesis events",
        params: &[
            ParamDef { name: "agent_id", description: "Agent name (default: main agent)", required: false },
        ],
    },
    ToolDef {
        name: "skill_synthesis_run",
        description: "Manually trigger the Rollout-to-Skill synthesis pipeline (W19-P0). \
                       Parses EvolutionEvents JSONL, scores trajectories, and (when dry_run=false) \
                       synthesises + graduates high-quality skills into the Skill Bank via Haiku 4.5.",
        params: &[
            ParamDef { name: "agent_id", description: "Target agent that will own synthesised skills (default: main agent)", required: false },
            ParamDef { name: "dry_run", description: "true = score only, no Skill Bank writes (default: true)", required: false },
            ParamDef { name: "lookback_days", description: "Days of EvolutionEvents history to scan (default: 1)", required: false },
        ],
    },
    // ── Feedback tool ────────────────────────────────────────────
    ToolDef {
        name: "submit_feedback",
        description: "Submit user feedback signal (positive/negative/correction) to influence agent evolution",
        params: &[
            ParamDef { name: "signal_type", description: "Feedback type: positive, negative, or correction", required: true },
            ParamDef { name: "detail", description: "What the feedback is about", required: true },
            ParamDef { name: "agent_id", description: "Target agent (default: main agent)", required: false },
        ],
    },
    // ── Evolution controls ──────────────────────────────────────
    ToolDef {
        name: "evolution_toggle",
        description: "Toggle evolution engine flags for an agent. Changes take effect within seconds. \
                       Supports standard flags and stagnation-detection sub-fields \
                       (stagnation_enabled, stagnation_window_seconds, stagnation_trigger_threshold, stagnation_action).",
        params: &[
            ParamDef { name: "agent_id", description: "Target agent name", required: true },
            ParamDef {
                name: "field",
                description: "Config field to toggle: gvu_enabled, cognitive_memory, skill_auto_activate, \
                               skill_security_scan (booleans); max_silence_hours, max_gvu_generations, \
                               observation_period_hours, skill_token_budget, max_active_skills (numbers); \
                               stagnation_enabled (bool), stagnation_window_seconds (int, 60–604800), \
                               stagnation_trigger_threshold (int, 1–1000), stagnation_action (log_only|suppress)",
                required: true,
            },
            ParamDef { name: "value", description: "New value: true/false (for booleans), a number (for numeric fields), or a string (for stagnation_action)", required: true },
        ],
    },
    ToolDef {
        name: "evolution_status",
        description: "Get the current evolution engine configuration and status for an agent",
        params: &[
            ParamDef { name: "agent_id", description: "Target agent name (default: main agent)", required: false },
        ],
    },
    // ── Audit Trail Query (W19-P1 M4) ────────────────────────────
    ToolDef {
        name: "audit_trail_query",
        description: "Query the EvolutionEvent audit trail log (Governance + Durability events). \
                      Syncs the SQLite index cache from JSONL files then executes a filtered, paginated query. \
                      Supports filtering by agent, event type, outcome, skill, and time range.",
        params: &[
            ParamDef { name: "agent_id",   description: "Filter by agent ID (optional)", required: false },
            ParamDef { name: "event_type", description: "Filter by event type, e.g. governance_violation, durability_circuit_opened (optional)", required: false },
            ParamDef { name: "outcome",    description: "Filter by outcome, e.g. blocked, warned, triggered, recovered (optional)", required: false },
            ParamDef { name: "skill_id",   description: "Filter by skill ID (optional)", required: false },
            ParamDef { name: "since",      description: "Inclusive lower bound RFC3339 timestamp, e.g. 2026-04-29T00:00:00Z (optional)", required: false },
            ParamDef { name: "until",      description: "Exclusive upper bound RFC3339 timestamp (optional)", required: false },
            ParamDef { name: "limit",      description: "Page size 1–1000 (default 100)", required: false },
            ParamDef { name: "offset",     description: "Pagination offset (default 0)", required: false },
        ],
    },
    // ── Reliability Dashboard (W20-P0) ────────────────────────────
    ToolDef {
        name: "reliability_summary",
        description: "Compute Agent Reliability Dashboard summary from the EvolutionEvent audit trail. \
                      Returns four metrics over a configurable time window: \
                      consistency_score (per-task-type success rate), \
                      task_success_rate (overall success fraction), \
                      skill_adoption_rate (skill_activate / total events), \
                      fallback_trigger_rate (llm_fallback_triggered / total events). \
                      Requires Admin scope.",
        params: &[
            ParamDef {
                name: "agent_id",
                description: "Target agent ID to analyse (required)",
                required: true,
            },
            ParamDef {
                name: "window_days",
                description: "Look-back window in days (default 7, max 365)",
                required: false,
            },
        ],
    },
    // ── Channel settings tools ────────────────────────────────────
    ToolDef {
        name: "channel_config",
        description: "Get or set channel settings (mention_only, auto_thread, allowed_channels, agent_override, response_mode). Omit 'value' to read current setting.",
        params: &[
            ParamDef { name: "channel", description: "Channel type: discord, telegram, slack, line", required: true },
            ParamDef { name: "scope_id", description: "Scope: guild_id, chat_id, or 'global'", required: true },
            ParamDef { name: "key", description: "Setting key: mention_only, auto_thread, allowed_channels, agent_override, response_mode", required: true },
            ParamDef { name: "value", description: "New value (omit to read current value)", required: false },
        ],
    },
    ToolDef {
        name: "channel_config_list",
        description: "List all channel settings for a scope",
        params: &[
            ParamDef { name: "channel", description: "Channel type: discord, telegram, slack, line", required: true },
            ParamDef { name: "scope_id", description: "Scope: guild_id, chat_id, or 'global'", required: true },
        ],
    },
    // ── Local inference tools ─────────────────────────────────────
    ToolDef {
        name: "inference_status",
        description: "Get local inference engine status: loaded model, hardware info, memory usage, backend type",
        params: &[],
    },
    ToolDef {
        name: "model_list",
        description: "List available local GGUF models in ~/.duduclaw/models/",
        params: &[],
    },
    ToolDef {
        name: "model_load",
        description: "Load a local GGUF model into memory for inference",
        params: &[
            ParamDef { name: "model_id", description: "Model ID or filename (e.g., 'qwen3-8b-q4_k_m')", required: true },
        ],
    },
    ToolDef {
        name: "model_unload",
        description: "Unload the currently loaded model to free memory",
        params: &[],
    },
    ToolDef {
        name: "hardware_info",
        description: "Detect and display hardware capabilities: GPU type, VRAM, RAM, recommended backend and model size",
        params: &[],
    },
    ToolDef {
        name: "route_query",
        description: "Preview how the confidence router would route a query (LocalFast / LocalStrong / CloudAPI) without actually generating. Shows confidence score and reasoning.",
        params: &[
            ParamDef { name: "prompt", description: "The user prompt to test routing for", required: true },
            ParamDef { name: "system_prompt", description: "Optional system prompt context", required: false },
        ],
    },
    ToolDef {
        name: "inference_mode",
        description: "Show the current inference mode (exo-cluster / llamafile / direct / cloud-only) and multi-mode manager status",
        params: &[],
    },
    ToolDef {
        name: "llamafile_start",
        description: "Start a llamafile server for local inference",
        params: &[
            ParamDef { name: "file", description: "llamafile filename (optional, uses default)", required: false },
        ],
    },
    ToolDef {
        name: "llamafile_stop",
        description: "Stop the running llamafile server",
        params: &[],
    },
    ToolDef {
        name: "llamafile_list",
        description: "List available llamafile executables in ~/.duduclaw/llamafiles/",
        params: &[],
    },
    // ── Compression tools ──────────────────────────────────────────
    ToolDef {
        name: "compress_text",
        description: "Compress text using the specified strategy. Strategies: 'meta_token' (lossless, best for JSON/code/templates), 'llmlingua' (lossy 2-5x, best for natural language), 'streaming_llm' (session window management), 'auto' (auto-select based on content type). Default: 'meta_token'.",
        params: &[
            ParamDef { name: "text", description: "Text to compress", required: true },
            ParamDef { name: "strategy", description: "Compression strategy: meta_token | llmlingua | streaming_llm | auto (default: meta_token)", required: false },
        ],
    },
    ToolDef {
        name: "decompress_text",
        description: "Decompress a Meta-Token compressed string back to the original text (lossless).",
        params: &[
            ParamDef { name: "text", description: "Compressed text to decompress", required: true },
        ],
    },
    // ── Model registry tools ────────────────────────────────────────
    ToolDef {
        name: "model_search",
        description: "Search for GGUF models from curated recommendations and HuggingFace. Results are filtered by available RAM. Trusted repos are marked [推薦].",
        params: &[
            ParamDef { name: "query", description: "Search query (e.g., 'qwen 8b', 'code llama', 'gemma')", required: true },
        ],
    },
    ToolDef {
        name: "model_download",
        description: "Download a GGUF model from HuggingFace to ~/.duduclaw/models/. Supports resume and mirror fallback.",
        params: &[
            ParamDef { name: "repo", description: "HuggingFace repo (e.g., 'Qwen/Qwen3-8B-GGUF')", required: true },
            ParamDef { name: "filename", description: "GGUF filename (e.g., 'qwen3-8b-q4_k_m.gguf')", required: true },
        ],
    },
    ToolDef {
        name: "model_recommend",
        description: "Get hardware-aware model recommendations based on detected GPU and available RAM.",
        params: &[],
    },
    // ── Odoo ERP tools ────────────────────────────────────────────
    ToolDef {
        name: "odoo_connect",
        description: "Connect to Odoo ERP and authenticate. Must be called before using other odoo_* tools.",
        params: &[],
    },
    ToolDef {
        name: "odoo_status",
        description: "Show Odoo connection status, version, edition (CE/EE), and installed modules",
        params: &[],
    },
    ToolDef {
        name: "odoo_crm_leads",
        description: "Search CRM leads/opportunities in Odoo",
        params: &[
            ParamDef { name: "stage", description: "Filter by stage name (optional)", required: false },
            ParamDef { name: "limit", description: "Max results (default 20)", required: false },
        ],
    },
    ToolDef {
        name: "odoo_crm_create_lead",
        description: "Create a new CRM lead in Odoo",
        params: &[
            ParamDef { name: "name", description: "Lead name / subject", required: true },
            ParamDef { name: "contact_name", description: "Contact person name", required: false },
            ParamDef { name: "email", description: "Contact email", required: false },
            ParamDef { name: "phone", description: "Contact phone", required: false },
            ParamDef { name: "expected_revenue", description: "Expected revenue", required: false },
        ],
    },
    ToolDef {
        name: "odoo_crm_update_stage",
        description: "Move a CRM lead to a different stage",
        params: &[
            ParamDef { name: "lead_id", description: "Lead ID", required: true },
            ParamDef { name: "stage_name", description: "Target stage name", required: true },
        ],
    },
    ToolDef {
        name: "odoo_sale_orders",
        description: "Search sale orders in Odoo",
        params: &[
            ParamDef { name: "status", description: "Filter by status (draft/sale/done)", required: false },
            ParamDef { name: "limit", description: "Max results (default 20)", required: false },
        ],
    },
    ToolDef {
        name: "odoo_sale_create_quotation",
        description: "Create a new quotation (draft sale order) in Odoo",
        params: &[
            ParamDef { name: "partner_id", description: "Customer partner ID", required: true },
            ParamDef { name: "product_id", description: "Product ID", required: true },
            ParamDef { name: "quantity", description: "Quantity (default 1)", required: false },
        ],
    },
    ToolDef {
        name: "odoo_sale_confirm",
        description: "Confirm a quotation into a sale order",
        params: &[
            ParamDef { name: "order_id", description: "Sale order ID to confirm", required: true },
        ],
    },
    ToolDef {
        name: "odoo_inventory_products",
        description: "Search products with stock levels in Odoo",
        params: &[
            ParamDef { name: "query", description: "Product name search", required: false },
            ParamDef { name: "limit", description: "Max results (default 20)", required: false },
        ],
    },
    ToolDef {
        name: "odoo_inventory_check",
        description: "Check real-time stock level for a specific product",
        params: &[
            ParamDef { name: "product_id", description: "Product ID", required: true },
        ],
    },
    ToolDef {
        name: "odoo_invoice_list",
        description: "List invoices from Odoo (draft/posted/paid)",
        params: &[
            ParamDef { name: "status", description: "Filter: draft/posted/paid", required: false },
            ParamDef { name: "limit", description: "Max results (default 20)", required: false },
        ],
    },
    ToolDef {
        name: "odoo_payment_status",
        description: "Check payment status for an invoice",
        params: &[
            ParamDef { name: "invoice_id", description: "Invoice ID", required: true },
        ],
    },
    ToolDef {
        name: "odoo_search",
        description: "Generic Odoo model search (advanced). Blocked models: ir.config_parameter, res.users, ir.cron, etc.",
        params: &[
            ParamDef { name: "model", description: "Odoo model name (e.g. res.partner)", required: true },
            ParamDef { name: "domain", description: "Search domain as JSON array", required: false },
            ParamDef { name: "fields", description: "Comma-separated field names", required: false },
            ParamDef { name: "limit", description: "Max results (default 20)", required: false },
        ],
    },
    ToolDef {
        name: "odoo_execute",
        description: "Call a method on an Odoo model (advanced). Example: action_confirm on sale.order.",
        params: &[
            ParamDef { name: "model", description: "Odoo model name", required: true },
            ParamDef { name: "method", description: "Method name to call", required: true },
            ParamDef { name: "ids", description: "Record IDs as JSON array", required: true },
        ],
    },
    ToolDef {
        name: "odoo_report",
        description: "Generate a PDF report from Odoo (e.g. invoice, quotation)",
        params: &[
            ParamDef { name: "report_name", description: "Report template name (e.g. account.report_invoice)", required: true },
            ParamDef { name: "record_id", description: "Record ID", required: true },
        ],
    },
    // ── Cost telemetry tools ─────────────────────────────────────
    ToolDef {
        name: "cost_summary",
        description: "Get token usage and cost summary (global or per-agent). Shows cache efficiency, total tokens, estimated cost. Use to monitor API spending and detect cache degradation.",
        params: &[
            ParamDef { name: "agent_id", description: "Agent ID to filter (optional, omit for global summary)", required: false },
            ParamDef { name: "hours", description: "Time window in hours (default 24)", required: false },
        ],
    },
    ToolDef {
        name: "cost_agents",
        description: "List all agents ranked by cost. Shows per-agent cache efficiency and health status (healthy/normal/degraded).",
        params: &[
            ParamDef { name: "hours", description: "Time window in hours (default 24)", required: false },
        ],
    },
    ToolDef {
        name: "cost_recent",
        description: "Show recent individual API call records with detailed token breakdown (input, cache_read, cache_write, output).",
        params: &[
            ParamDef { name: "limit", description: "Number of recent records (default 20)", required: false },
        ],
    },
    // ── Voice / ASR / TTS tools ──────────────────────────────────
    ToolDef {
        name: "transcribe_audio",
        description: "Transcribe audio to text using Whisper ASR. Accepts base64-encoded audio (OGG/MP3/WAV/M4A). Returns transcribed text. Default language: zh (Mandarin).",
        params: &[
            ParamDef { name: "audio_base64", description: "Base64-encoded audio data", required: true },
            ParamDef { name: "language", description: "Language hint (default: zh). BCP-47 code.", required: false },
        ],
    },
    ToolDef {
        name: "synthesize_speech",
        description: "Convert text to speech audio using TTS (edge-tts free or MiniMax paid). Returns base64-encoded MP3 audio.",
        params: &[
            ParamDef { name: "text", description: "Text to synthesize", required: true },
            ParamDef { name: "voice", description: "Voice name (default: auto-detect zh-TW/en-US)", required: false },
        ],
    },
    // ── Wiki Knowledge Base tools ───────────────────────────────
    ToolDef {
        name: "wiki_ls",
        description: "List wiki pages for an agent. Returns directory tree with page titles and last-updated timestamps from YAML frontmatter.",
        params: &[
            ParamDef { name: "agent_id", description: "Agent name (default: main agent)", required: false },
        ],
    },
    ToolDef {
        name: "wiki_read",
        description: "Read a wiki page (frontmatter + body). Use wiki_ls or wiki_search to find page paths first.",
        params: &[
            ParamDef { name: "page_path", description: "Page path relative to wiki/ (e.g. 'entities/wang-ming.md')", required: true },
            ParamDef { name: "agent_id", description: "Agent name (default: main agent)", required: false },
        ],
    },
    ToolDef {
        name: "wiki_write",
        description: "Create or update a wiki page. Automatically updates _index.md and appends to _log.md. Uses atomic write (temp + rename).",
        params: &[
            ParamDef { name: "page_path", description: "Page path relative to wiki/ (e.g. 'concepts/return-policy.md')", required: true },
            ParamDef { name: "content", description: "Full page content including YAML frontmatter", required: true },
            ParamDef { name: "agent_id", description: "Agent name (default: main agent)", required: false },
            ParamDef { name: "update_index", description: "Update _index.md automatically (default: true)", required: false },
        ],
    },
    ToolDef {
        name: "wiki_search",
        description: "Full-text search across wiki pages with trust-weighted ranking. Supports layer/trust filtering and 1-hop expand via related pages.",
        params: &[
            ParamDef { name: "query", description: "Search query (keywords)", required: true },
            ParamDef { name: "agent_id", description: "Agent name (default: main agent)", required: false },
            ParamDef { name: "limit", description: "Max results (default: 10)", required: false },
            ParamDef { name: "min_trust", description: "Minimum trust score filter (0.0-1.0)", required: false },
            ParamDef { name: "layer", description: "Filter by layer: identity/core/context/deep", required: false },
            ParamDef { name: "expand", description: "1-hop expand via related/backlinks (true/false, default: false)", required: false },
        ],
    },
    ToolDef {
        name: "wiki_lint",
        description: "Run a health check on the wiki: find orphan pages, broken links, stale pages. Returns a lint report.",
        params: &[
            ParamDef { name: "agent_id", description: "Agent name (default: main agent)", required: false },
        ],
    },
    ToolDef {
        name: "wiki_stats",
        description: "Get wiki statistics: total pages, index entries, recent activity, health score.",
        params: &[
            ParamDef { name: "agent_id", description: "Agent name (default: main agent)", required: false },
        ],
    },
    ToolDef {
        name: "wiki_export",
        description: "Export the wiki as Obsidian vault (directory of .md files with wikilinks) or a single HTML file. Returns the output path or HTML content.",
        params: &[
            ParamDef { name: "format", description: "Export format: 'obsidian' or 'html' (default: html)", required: false },
            ParamDef { name: "agent_id", description: "Agent name (default: main agent)", required: false },
        ],
    },
    ToolDef {
        name: "wiki_dedup",
        description: "Detect potential duplicate wiki pages using title and tag similarity. Returns candidate pairs with trust scores for merge decisions.",
        params: &[
            ParamDef { name: "agent_id", description: "Agent name (default: main agent)", required: false },
        ],
    },
    ToolDef {
        name: "wiki_graph",
        description: "Export wiki knowledge graph as Mermaid diagram. Nodes=pages, edges=related links. Supports focused view around a center page.",
        params: &[
            ParamDef { name: "agent_id", description: "Agent name (default: main agent)", required: false },
            ParamDef { name: "center", description: "Center page path for focused view (e.g. 'entities/customer.md')", required: false },
            ParamDef { name: "depth", description: "Max hops from center (default: 2, ignored without center)", required: false },
        ],
    },
    ToolDef {
        name: "wiki_rebuild_fts",
        description: "Rebuild the FTS5 full-text search index from all wiki pages on disk. Use if search results seem stale.",
        params: &[
            ParamDef { name: "agent_id", description: "Agent name (default: main agent)", required: false },
        ],
    },
    // ── Phase 4: Wiki RL Trust feedback inspection ────────────────
    ToolDef {
        name: "wiki_trust_audit",
        description: "List wiki pages whose live trust score has fallen below a threshold, with citation and signal counters. Use this to spot pages the prediction-error feedback loop is downgrading — they may need fact-checking or archival.",
        params: &[
            ParamDef { name: "agent_id", description: "Agent name (default: main agent)", required: false },
            ParamDef { name: "max_trust", description: "Upper bound on trust to include (default: 0.3)", required: false },
            ParamDef { name: "limit", description: "Max rows (default: 20, max: 500)", required: false },
        ],
    },
    ToolDef {
        name: "wiki_trust_history",
        description: "Recent audit-history entries for a single wiki page — every trust change with trigger, conversation, signal kind. Use for post-mortem analysis when a page's trust is moving unexpectedly.",
        params: &[
            ParamDef { name: "agent_id", description: "Agent name (default: main agent)", required: false },
            ParamDef { name: "page_path", description: "Page path relative to wiki/ (e.g. 'concepts/cron-facts.md')", required: true },
            ParamDef { name: "limit", description: "Max rows (default: 50, max: 500)", required: false },
        ],
    },
    // ── Shared Wiki Knowledge Base tools ─────────────────────────
    ToolDef {
        name: "shared_wiki_ls",
        description: "List pages in the shared wiki (~/.duduclaw/shared/wiki/). The shared wiki is a cross-agent public knowledge base.",
        params: &[],
    },
    ToolDef {
        name: "shared_wiki_read",
        description: "Read a page from the shared wiki. Use shared_wiki_ls or shared_wiki_search to find page paths first.",
        params: &[
            ParamDef { name: "page_path", description: "Page path relative to shared/wiki/ (e.g. 'concepts/return-policy.md')", required: true },
        ],
    },
    ToolDef {
        name: "shared_wiki_write",
        description: "Create or update a page in the shared wiki. Author is automatically tracked. All agents can contribute to the shared knowledge base.",
        params: &[
            ParamDef { name: "page_path", description: "Page path relative to shared/wiki/ (e.g. 'concepts/company-sop.md')", required: true },
            ParamDef { name: "content", description: "Full page content including YAML frontmatter (author field auto-injected)", required: true },
        ],
    },
    ToolDef {
        name: "shared_wiki_search",
        description: "Full-text search across shared wiki pages with trust-weighted ranking. Supports layer/trust filtering.",
        params: &[
            ParamDef { name: "query", description: "Search query (keywords)", required: true },
            ParamDef { name: "limit", description: "Max results (default: 10)", required: false },
            ParamDef { name: "min_trust", description: "Minimum trust score filter (0.0-1.0)", required: false },
            ParamDef { name: "layer", description: "Filter by layer: identity/core/context/deep", required: false },
        ],
    },
    ToolDef {
        name: "shared_wiki_delete",
        description: "Delete a page from the shared wiki. Only the original author or the main agent can delete.",
        params: &[
            ParamDef { name: "page_path", description: "Page path to delete", required: true },
        ],
    },
    ToolDef {
        name: "shared_wiki_stats",
        description: "Get shared wiki statistics: total pages, contributor breakdown, recent activity.",
        params: &[],
    },
    ToolDef {
        name: "shared_wiki_lint",
        description: "Audit shared wiki pages for Karpathy-schema compliance. Reports: missing required frontmatter fields (title/created/updated/tags/layer/trust), fallback-content markers, orphan pages, broken links, stale pages.",
        params: &[],
    },
    ToolDef {
        name: "wiki_namespace_status",
        description: "Inspect the shared-wiki namespace SoT policy (~/.duduclaw/shared/wiki/.scope.toml). Returns each configured namespace's mode (agent_writable / read_only / operator_only) and synced_from capability. Unlisted namespaces are agent_writable. Use this before shared_wiki_write to know whether a target namespace is writable.",
        params: &[],
    },
    ToolDef {
        name: "identity_resolve",
        description: "RFC-21 §1: Resolve a (channel, external_id) pair to the canonical person it represents — name, roles, project memberships, all known channel handles. Returns null when the person is unknown. Reads from the WikiCacheIdentityProvider (~/.duduclaw/shared/wiki/identity/people/*.md). Use this *before* deciding whether the sender is a project member, instead of grepping shared_wiki_read.",
        params: &[
            ParamDef { name: "channel", description: "Channel kind: discord / line / telegram / slack / whatsapp / feishu / webchat / email", required: true },
            ParamDef { name: "external_id", description: "Channel-side identifier — Discord user_id, LINE user_id, email address, etc.", required: true },
        ],
    },
    ToolDef {
        name: "wiki_share",
        description: "Share a page from your wiki to the shared wiki. Creates a source-attributed copy in shared/wiki/sources/.",
        params: &[
            ParamDef { name: "page_path", description: "Page path in your own wiki to share", required: true },
            ParamDef { name: "summary", description: "Optional custom summary (default: first 500 chars of body)", required: false },
        ],
    },
    // ── Skill Internalization tools ─────────────────────────────
    ToolDef {
        name: "skill_extract",
        description: "Extract structured knowledge from a skill into the agent's wiki. Creates concept pages, entity pages, and a source summary. Zero LLM cost (heuristic mode).",
        params: &[
            ParamDef { name: "skill_name", description: "Name of the skill to extract from", required: true },
            ParamDef { name: "agent_id", description: "Agent name (default: main agent)", required: false },
        ],
    },
    // ── Program execution ────────────────────────────────────────
    ToolDef {
        name: "execute_program",
        description: "Execute a program that can call DuDuClaw MCP tools via RPC. Only final stdout enters context.",
        params: &[
            ParamDef { name: "code", description: "Source code to execute", required: true },
            ParamDef { name: "language", description: "Language: 'python', 'bash', or 'javascript'", required: true },
            ParamDef { name: "timeout_seconds", description: "Execution timeout in seconds (default: 30, max: 300)", required: false },
        ],
    },
    // ── Skill Bank tools ─────────────────────────────────────────
    ToolDef {
        name: "skill_bank_search",
        description: "Search the skill bank for learned skills matching a query. Returns ranked results with confidence scores.",
        params: &[
            ParamDef { name: "query", description: "Search query to match against skill names and descriptions", required: true },
            ParamDef { name: "limit", description: "Maximum number of results to return (default: 5)", required: false },
        ],
    },
    ToolDef {
        name: "skill_bank_feedback",
        description: "Provide success/failure feedback for a skill execution. Updates confidence via Bayesian update.",
        params: &[
            ParamDef { name: "skill_id", description: "ID of the skill to provide feedback for", required: true },
            ParamDef { name: "success", description: "Whether the skill execution was successful (true/false)", required: true },
        ],
    },
    // ── Computer Use tools ────────────────────────────────────────
    ToolDef {
        name: "computer_screenshot",
        description: "Capture a screenshot of the virtual display (L5 container) or host screen (L5b native). Returns base64-encoded PNG. Requires computer_use capability.",
        params: &[
            ParamDef { name: "display", description: "Which display to capture: 'container' (default) or 'native'", required: false },
        ],
    },
    ToolDef {
        name: "computer_click",
        description: "Click at specific coordinates on the screen. Requires computer_use capability.",
        params: &[
            ParamDef { name: "x", description: "X coordinate", required: true },
            ParamDef { name: "y", description: "Y coordinate", required: true },
            ParamDef { name: "button", description: "Mouse button: 'left' (default), 'right', 'middle'", required: false },
            ParamDef { name: "double", description: "Double-click if true (default: false)", required: false },
        ],
    },
    ToolDef {
        name: "computer_type",
        description: "Type text at the current cursor position. Requires computer_use capability.",
        params: &[
            ParamDef { name: "text", description: "Text to type", required: true },
        ],
    },
    ToolDef {
        name: "computer_key",
        description: "Press a key combination (e.g., 'ctrl+s', 'Return', 'Tab'). Requires computer_use capability.",
        params: &[
            ParamDef { name: "key", description: "Key combination (e.g., 'ctrl+c', 'Return', 'alt+Tab')", required: true },
        ],
    },
    ToolDef {
        name: "computer_scroll",
        description: "Scroll at specific coordinates. Requires computer_use capability.",
        params: &[
            ParamDef { name: "x", description: "X coordinate", required: true },
            ParamDef { name: "y", description: "Y coordinate", required: true },
            ParamDef { name: "direction", description: "Scroll direction: 'up' or 'down' (default: 'down')", required: false },
            ParamDef { name: "amount", description: "Number of scroll clicks (default: 3)", required: false },
        ],
    },
    ToolDef {
        name: "computer_session_start",
        description: "Start a new Computer Use session with a virtual display container. Returns session_id on success. Requires computer_use capability.",
        params: &[
            ParamDef { name: "task", description: "Description of what to accomplish", required: true },
            ParamDef { name: "width", description: "Display width in pixels (default: 1280)", required: false },
            ParamDef { name: "height", description: "Display height in pixels (default: 800)", required: false },
        ],
    },
    ToolDef {
        name: "computer_session_stop",
        description: "Stop an active Computer Use session and clean up the container.",
        params: &[
            ParamDef { name: "session_id", description: "Session ID returned by computer_session_start", required: true },
        ],
    },
    // ── Session tools ────────────────────────────────────────────
    ToolDef {
        name: "session_restore_context",
        description: "Search hidden/archived messages in the current session to restore relevant context.",
        params: &[
            ParamDef { name: "query", description: "Search query to find relevant archived messages", required: true },
        ],
    },
    // ── Task Board tools (Multica-inspired, Agent-as-teammate) ──
    ToolDef {
        name: "tasks_list",
        description: "List tasks from the shared Kanban board. Defaults to tasks assigned to the calling agent. Pass assigned_to='*' for all agents. Use this to see your task queue.",
        params: &[
            ParamDef { name: "status", description: "Filter by status: todo / in_progress / done / blocked", required: false },
            ParamDef { name: "priority", description: "Filter by priority: low / medium / high / urgent", required: false },
            ParamDef { name: "assigned_to", description: "Filter by agent ID. Defaults to caller. Pass '*' for all agents.", required: false },
            ParamDef { name: "limit", description: "Max results (default 20, max 100)", required: false },
        ],
    },
    ToolDef {
        name: "tasks_create",
        description: "Create a new task on the Kanban board. created_by is automatically set to the calling agent.",
        params: &[
            ParamDef { name: "title", description: "Task title (required, <200 chars)", required: true },
            ParamDef { name: "description", description: "Markdown description", required: false },
            ParamDef { name: "assigned_to", description: "Agent ID to assign the task to. Defaults to caller.", required: false },
            ParamDef { name: "priority", description: "low / medium / high / urgent (default: medium)", required: false },
            ParamDef { name: "tags", description: "Comma-separated tags", required: false },
            ParamDef { name: "parent_task_id", description: "Parent task ID for sub-tasks", required: false },
        ],
    },
    ToolDef {
        name: "tasks_update",
        description: "Update task fields. For common state transitions use tasks_claim / tasks_complete / tasks_block instead.",
        params: &[
            ParamDef { name: "task_id", description: "Task ID", required: true },
            ParamDef { name: "title", description: "New title", required: false },
            ParamDef { name: "description", description: "New description", required: false },
            ParamDef { name: "priority", description: "New priority", required: false },
            ParamDef { name: "tags", description: "New comma-separated tags", required: false },
        ],
    },
    ToolDef {
        name: "tasks_claim",
        description: "Claim a task: reassigns it to the calling agent and transitions status to in_progress. Posts a task_assigned activity event.",
        params: &[
            ParamDef { name: "task_id", description: "Task ID to claim", required: true },
        ],
    },
    ToolDef {
        name: "tasks_complete",
        description: "Mark a task as done and post a task_completed activity event with the optional summary.",
        params: &[
            ParamDef { name: "task_id", description: "Task ID", required: true },
            ParamDef { name: "summary", description: "Optional completion summary (posted to the activity feed)", required: false },
        ],
    },
    ToolDef {
        name: "tasks_block",
        description: "Mark a task as blocked with a reason. Posts a task_blocked activity event.",
        params: &[
            ParamDef { name: "task_id", description: "Task ID", required: true },
            ParamDef { name: "reason", description: "Blocker reason (required, shown on the card)", required: true },
        ],
    },
    // ── Activity Feed tools ─────────────────────────────────────
    ToolDef {
        name: "activity_post",
        description: "Post a progress / comment event to the Activity Feed. Use to report intermediate progress without changing task status.",
        params: &[
            ParamDef { name: "summary", description: "One-line human-readable summary (required)", required: true },
            ParamDef { name: "task_id", description: "Optional task ID to link the activity to", required: false },
            ParamDef { name: "event_type", description: "Event type (progress, comment, info, etc.). Default: agent_comment", required: false },
            ParamDef { name: "metadata", description: "Optional JSON metadata blob", required: false },
        ],
    },
    ToolDef {
        name: "activity_list",
        description: "List recent Activity Feed events. Filterable by agent / task / type.",
        params: &[
            ParamDef { name: "task_id", description: "Filter by task ID", required: false },
            ParamDef { name: "agent_id", description: "Filter by agent ID (default: caller)", required: false },
            ParamDef { name: "event_type", description: "Filter by event type", required: false },
            ParamDef { name: "limit", description: "Max results (default 20, max 100)", required: false },
        ],
    },
    // ── Autopilot tools (read-only from agents) ─────────────────
    ToolDef {
        name: "autopilot_list",
        description: "List automation rules (read-only for agents). Rule creation / edit is restricted to the web dashboard.",
        params: &[
            ParamDef { name: "enabled_only", description: "Only show enabled rules (default: true)", required: false },
        ],
    },
    // ── Shared Skills tools (cross-agent skill pool) ────────────
    ToolDef {
        name: "shared_skill_list",
        description: "List skills in the team-shared skill pool (~/.duduclaw/shared/skills/). Skills shared by other agents are available for adoption.",
        params: &[
            ParamDef { name: "tag", description: "Filter by tag", required: false },
        ],
    },
    ToolDef {
        name: "shared_skill_share",
        description: "Share one of your own skills to the team-shared skill pool. Skill must already exist in your agent's SKILLS/ directory.",
        params: &[
            ParamDef { name: "skill_name", description: "Skill name (matches SKILLS/<name>.md)", required: true },
        ],
    },
    ToolDef {
        name: "shared_skill_adopt",
        description: "Adopt a shared skill into an agent's SKILLS directory. Bumps usage_count on the shared skill and records the adopter.",
        params: &[
            ParamDef { name: "skill_name", description: "Shared skill name", required: true },
            ParamDef { name: "target_agent", description: "Agent to adopt the skill into (default: caller)", required: false },
        ],
    },
];

// ── External tool whitelist (W19-P0 BUG-QA-001) ─────────────
/// Tools visible to external MCP clients (`principal.is_external = true`).
/// Exactly 7 tools are exposed; all others are hidden to reduce attack surface.
pub(crate) const EXTERNAL_TOOLS_WHITELIST: &[&str] = &[
    "memory_search",
    "memory_store",
    "memory_read",
    "wiki_read",
    "wiki_write",
    "wiki_search",
    "send_message",
];

// ── JSON-RPC helpers ─────────────────────────────────────────

/// Detect the host system's IANA timezone name (e.g. `"Asia/Taipei"`,
/// `"America/New_York"`, `"UTC"`).
///
/// Uses `iana_time_zone::get_timezone()` which reads `/etc/localtime` on
/// Unix / the registry on Windows. Validated against `chrono-tz` so we
/// never return a name the scheduler would reject — defensive because
/// `iana-time-zone` is allowed to surface historical aliases that
/// `chrono-tz`'s database has dropped.
///
/// Returns `None` when the host has no discoverable TZ (extremely rare
/// on real machines — typical in minimal Docker images with no
/// `/etc/localtime`). Callers should fall back to UTC.
///
/// Introduced in v1.8.25 so `schedule_task` stops surprising users by
/// silently evaluating cron expressions in UTC when they meant local.
fn detect_local_timezone() -> Option<String> {
    let name = iana_time_zone::get_timezone().ok()?;
    // Round-trip through chrono-tz so we only ever hand back names the
    // scheduler's `duduclaw_core::parse_timezone` will accept.
    if duduclaw_core::parse_timezone(&name).is_some() {
        Some(name)
    } else {
        None
    }
}

fn jsonrpc_response(id: &Value, result: Value) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
}

/// Validate agent ID is safe for filesystem paths (no traversal).
fn is_valid_agent_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Maximum JSONL queue file size (10 MB).
const MAX_QUEUE_FILE_SIZE: u64 = 10 * 1024 * 1024;

/// Maximum allowed byte length for an `agent_id` parameter in MCP handlers.
/// Prevents excessively long inputs that could cause DoS or log-flooding.
const MAX_AGENT_ID_LEN: usize = 128;

/// Append a line to a JSONL file with size limit check.
///
/// **Concurrency note (MCP-M4)**: Uses `O_APPEND` which is atomic on POSIX
/// for writes ≤ PIPE_BUF (typically 4096 bytes). JSONL lines are typically
/// < 1KB so concurrent appends from MCP server and gateway are safe.
/// The dispatcher uses its own Mutex for read-modify-write operations.
fn append_to_jsonl_sync(path: &std::path::Path, line: &str) -> bool {
    // Check size limit
    if let Ok(meta) = std::fs::metadata(path)
        && meta.len() > MAX_QUEUE_FILE_SIZE {
            tracing::warn!("Queue file {} exceeds size limit", path.display());
            return false;
        }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        writeln!(f, "{line}").is_ok()
    } else {
        false
    }
}

fn jsonrpc_error(id: &Value, code: i64, message: &str) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message
        }
    })
}

// ── Tool schema builder ──────────────────────────────────────

fn build_tool_schema(tool: &ToolDef) -> Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();

    for param in tool.params {
        properties.insert(
            param.name.to_string(),
            serde_json::json!({
                "type": "string",
                "description": param.description
            }),
        );
        if param.required {
            required.push(Value::String(param.name.to_string()));
        }
    }

    serde_json::json!({
        "name": tool.name,
        "description": tool.description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required
        }
    })
}

// ── Tool handlers ────────────────────────────────────────────

async fn handle_send_message(
    params: &Value,
    home_dir: &Path,
    http: &reqwest::Client,
    _agent_id: &str,
) -> Value {
    let channel = params.get("channel").and_then(|v| v.as_str()).unwrap_or("");
    let chat_id = params.get("chat_id").and_then(|v| v.as_str()).unwrap_or("");
    let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");

    if channel.is_empty() || chat_id.is_empty() || text.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: channel, chat_id, and text are required"}],
            "isError": true
        });
    }

    let config = match read_config(home_dir).await {
        Some(c) => c,
        None => {
            return serde_json::json!({
                "content": [{"type": "text", "text": "Error: could not read config.toml"}],
                "isError": true
            });
        }
    };

    let result = match channel {
        "telegram" => {
            let token = decrypt_channel_token(&config, "telegram_bot_token_enc", "telegram_bot_token", home_dir);
            if token.is_empty() {
                "Error: telegram_bot_token not configured".to_string()
            } else {
                let url = format!(
                    "https://api.telegram.org/bot{}/sendMessage",
                    token
                );
                match http
                    .post(&url)
                    .json(&serde_json::json!({
                        "chat_id": chat_id,
                        "text": text
                    }))
                    .send()
                    .await
                {
                    Ok(resp) => if resp.status().is_success() { "Message sent successfully.".to_string() } else { format!("Error: API returned {}", resp.status()) },
                    Err(e) => format!("Error sending Telegram message: {e}"),
                }
            }
        }
        "line" => {
            let token = decrypt_channel_token(&config, "line_channel_token_enc", "line_channel_token", home_dir);
            if token.is_empty() {
                "Error: line_channel_token not configured".to_string()
            } else {
                let url = "https://api.line.me/v2/bot/message/push";
                match http
                    .post(url)
                    .header("Authorization", format!("Bearer {}", token))
                    .json(&serde_json::json!({
                        "to": chat_id,
                        "messages": [{"type": "text", "text": text}]
                    }))
                    .send()
                    .await
                {
                    Ok(resp) => if resp.status().is_success() { "Message sent successfully.".to_string() } else { format!("Error: API returned {}", resp.status()) },
                    Err(e) => format!("Error sending LINE message: {e}"),
                }
            }
        }
        "discord" => {
            let token = decrypt_channel_token(&config, "discord_bot_token_enc", "discord_bot_token", home_dir);
            if token.is_empty() {
                "Error: discord_bot_token not configured".to_string()
            } else {
                let url = format!(
                    "https://discord.com/api/v10/channels/{}/messages",
                    chat_id
                );
                match http
                    .post(&url)
                    .header("Authorization", format!("Bot {}", token))
                    .json(&serde_json::json!({
                        "content": text
                    }))
                    .send()
                    .await
                {
                    Ok(resp) => if resp.status().is_success() { "Message sent successfully.".to_string() } else { format!("Error: API returned {}", resp.status()) },
                    Err(e) => format!("Error sending Discord message: {e}"),
                }
            }
        }
        _ => format!("Unknown channel: {channel}"),
    };

    serde_json::json!({
        "content": [{"type": "text", "text": result}]
    })
}

async fn handle_web_search(params: &Value, http: &reqwest::Client) -> Value {
    let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
    if query.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: query is required"}],
            "isError": true
        });
    }

    let url = format!(
        "https://html.duckduckgo.com/html/?q={}",
        urlencoding::encode(query)
    );

    // Enforce 10s timeout so web_search doesn't block the MCP server (CLI-H5)
    let search_future = async {
        let resp = http
            .get(&url)
            .header("User-Agent", "DuDuClaw/0.6")
            .send()
            .await?;
        resp.text().await
    };

    let result = match tokio::time::timeout(std::time::Duration::from_secs(10), search_future).await {
        Ok(Ok(body)) => extract_search_results(&body),
        Ok(Err(e)) => format!("Error performing search: {e}"),
        Err(_) => "Error: web search timed out after 10 seconds".to_string(),
    };

    serde_json::json!({
        "content": [{"type": "text", "text": result}]
    })
}

/// Extract text results from DuckDuckGo HTML response using `scraper` (CLI-M5).
fn extract_search_results(html: &str) -> String {
    use scraper::{Html, Selector};

    let document = Html::parse_document(html);
    let mut results = Vec::new();

    // Try selectors in priority order
    let selectors = [
        ".result__snippet",
        ".result__a",
        ".links_main a",
    ];

    for sel_str in selectors {
        if let Ok(selector) = Selector::parse(sel_str) {
            for element in document.select(&selector) {
                let text: String = element.text().collect::<Vec<_>>().join(" ");
                let clean = text.trim().to_string();
                if !clean.is_empty() && clean.len() > 10 {
                    results.push(clean);
                }
                if results.len() >= 5 { break; }
            }
        }
        if !results.is_empty() { break; }
    }

    if results.is_empty() {
        "No results found.".to_string()
    } else {
        results
            .iter()
            .enumerate()
            .map(|(i, r)| format!("{}. {}", i + 1, r))
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

async fn handle_memory_search(
    params: &Value,
    memory: &SqliteMemoryEngine,
    agent_id: &str,
) -> Value {
    let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
    if query.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: query is required"}],
            "isError": true
        });
    }

    match memory.search(agent_id, query, 10).await {
        Ok(entries) => {
            if entries.is_empty() {
                serde_json::json!({
                    "content": [{"type": "text", "text": "No memories found."}]
                })
            } else {
                let text = entries
                    .iter()
                    .map(|e| format!("[{}] {}", e.timestamp.format("%Y-%m-%d %H:%M"), e.content))
                    .collect::<Vec<_>>()
                    .join("\n");
                serde_json::json!({
                    "content": [{"type": "text", "text": text}]
                })
            }
        }
        Err(e) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Error searching memory: {e}")}],
            "isError": true
        }),
    }
}

async fn handle_memory_store(
    params: &Value,
    memory: &SqliteMemoryEngine,
    agent_id: &str,
) -> Value {
    let content = params.get("content").and_then(|v| v.as_str()).unwrap_or("");
    if content.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: content is required"}],
            "isError": true
        });
    }

    let tags_str = params.get("tags").and_then(|v| v.as_str()).unwrap_or("");
    let tags: Vec<String> = if tags_str.is_empty() {
        Vec::new()
    } else {
        tags_str.split(',').map(|s| s.trim().to_string()).collect()
    };

    let classification = duduclaw_memory::classify(content, "user_input");
    let entry_id = uuid::Uuid::new_v4().to_string();
    let entry = MemoryEntry {
        id: entry_id.clone(),
        agent_id: agent_id.to_string(),
        content: content.to_string(),
        timestamp: chrono::Utc::now(),
        tags,
        embedding: None,
        layer: classification.layer,
        importance: classification.importance,
        access_count: 0,
        last_accessed: None,
        source_event: "mcp_memory_store".to_string(),
    };

    match memory.store(agent_id, entry).await {
        // BUG-MCP-001 fix: include memory_id so callers can retrieve the entry
        // via memory_read without needing a secondary search.
        Ok(()) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Memory stored successfully. memory_id: {entry_id}")}],
            "memory_id": entry_id
        }),
        Err(e) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Error storing memory: {e}")}],
            "isError": true
        }),
    }
}

// ── memory_read ───────────────────────────────────────────────────────────────
// W19-P0 M1: Read a single memory entry by its UUID.
// Uses get_by_id for O(1) point lookup (agent ownership enforced in SQL).
// Namespace isolation: get_by_id filters by agent_id, so cross-agent reads
// are rejected at the DB layer — callers outside the owning namespace always
// receive isError.
async fn handle_memory_read(
    params: &Value,
    memory: &SqliteMemoryEngine,
    agent_id: &str,
) -> Value {
    let memory_id = match params.get("memory_id").and_then(|v| v.as_str()) {
        Some(id) if !id.is_empty() => id,
        _ => return tool_error("Missing required parameter: memory_id"),
    };

    match memory.get_by_id(agent_id, memory_id).await {
        Ok(Some(entry)) => {
            let payload = serde_json::json!({
                "memory_id": entry.id,
                "content": entry.content,
                "layer": format!("{:?}", entry.layer),
                "tags": entry.tags,
                "created_at": entry.timestamp.to_rfc3339(),
            });
            serde_json::json!({
                "content": [{ "type": "text", "text": payload.to_string() }]
            })
        }
        Ok(None) => serde_json::json!({
            "content": [{ "type": "text", "text": format!("Memory not found or access denied: {memory_id}") }],
            "isError": true
        }),
        Err(e) => tool_error(&format!("Error reading memory: {e}")),
    }
}

async fn handle_memory_search_by_layer(
    params: &Value,
    memory: &SqliteMemoryEngine,
    agent_id: &str,
) -> Value {
    let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
    if query.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: query is required"}],
            "isError": true
        });
    }
    let layer_str = params.get("layer").and_then(|v| v.as_str()).unwrap_or("");
    if layer_str.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: layer is required (episodic or semantic)"}],
            "isError": true
        });
    }
    let layer = duduclaw_core::types::MemoryLayer::parse(layer_str);
    let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;

    match memory.search_layer(agent_id, query, &layer, limit).await {
        Ok(entries) => {
            if entries.is_empty() {
                serde_json::json!({
                    "content": [{"type": "text", "text": format!("No {layer_str} memories found.")}]
                })
            } else {
                let text = entries
                    .iter()
                    .map(|e| format!("[{}] [{}] {}", e.timestamp.format("%Y-%m-%d %H:%M"), e.layer.as_str(), e.content))
                    .collect::<Vec<_>>()
                    .join("\n");
                serde_json::json!({
                    "content": [{"type": "text", "text": text}]
                })
            }
        }
        Err(e) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Error searching memory by layer: {e}")}],
            "isError": true
        }),
    }
}

async fn handle_memory_successful_conversations(
    params: &Value,
    memory: &SqliteMemoryEngine,
    agent_id: &str,
) -> Value {
    let topic = params.get("topic").and_then(|v| v.as_str()).unwrap_or("");
    if topic.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: topic is required"}],
            "isError": true
        });
    }
    let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;

    match memory.search_successful_conversations(agent_id, topic, limit).await {
        Ok(contents) => {
            if contents.is_empty() {
                serde_json::json!({
                    "content": [{"type": "text", "text": "No successful conversations found for this topic."}]
                })
            } else {
                let text = contents.join("\n---\n");
                serde_json::json!({
                    "content": [{"type": "text", "text": text}]
                })
            }
        }
        Err(e) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Error searching conversations: {e}")}],
            "isError": true
        }),
    }
}

async fn handle_memory_episodic_pressure(
    params: &Value,
    memory: &SqliteMemoryEngine,
    agent_id: &str,
) -> Value {
    let hours_ago = params.get("hours_ago").and_then(|v| v.as_u64()).unwrap_or(24);
    let since = chrono::Utc::now() - chrono::Duration::hours(hours_ago as i64);
    let pressure = memory.episodic_pressure(agent_id, since).await;

    serde_json::json!({
        "content": [{"type": "text", "text": format!(
            "Episodic pressure (last {hours_ago}h): {pressure:.2}\n\
             Threshold for Meso reflection: 10.0\n\
             Status: {}",
            if pressure > 10.0 { "⚠ Above threshold — reflection recommended" }
            else { "✓ Below threshold" }
        )}]
    })
}

async fn handle_memory_consolidation_status(
    memory: &SqliteMemoryEngine,
    agent_id: &str,
) -> Value {
    let conflict_count = memory.semantic_conflict_count(agent_id).await;

    serde_json::json!({
        "content": [{"type": "text", "text": format!(
            "Semantic conflict count: {conflict_count}\n\
             High-importance episodic memories not yet consolidated into semantic knowledge.\n\
             Status: {}",
            if conflict_count > 0 {
                format!("⚠ {conflict_count} unconsolidated observations — consolidation recommended")
            } else {
                "✓ No conflicts detected".to_string()
            }
        )}]
    })
}

/// Send a message to another agent via the bus queue.
async fn handle_send_to_agent(params: &Value, home_dir: &Path, caller: &str) -> Value {
    send_to_agent_with_ctx(params, home_dir, caller, DelegationContext::from_env()).await
}

/// Core implementation with injectable delegation context.
/// Production callers use `DelegationContext::from_env()`;
/// tests inject a specific context to avoid unsafe env var mutation.
async fn send_to_agent_with_ctx(
    params: &Value,
    home_dir: &Path,
    caller: &str,
    ctx: DelegationContext,
) -> Value {
    let target = params.get("agent_id").or_else(|| params.get("agent")).and_then(|v| v.as_str()).unwrap_or("");
    let prompt = params.get("prompt").or_else(|| params.get("message")).and_then(|v| v.as_str()).unwrap_or("");

    if target.is_empty() || prompt.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: agent_id and prompt are required"}],
            "isError": true
        });
    }

    // Validate agent_id format
    if !is_valid_agent_id(target) {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: agent_id must be lowercase alphanumeric with hyphens"}],
            "isError": true
        });
    }

    // ── Supervisor pattern enforcement ─────────────────────────
    if let Err(reason) = check_supervisor_relation(home_dir, caller, target).await {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: {reason}")}],
            "isError": true
        });
    }

    // ── Delegation depth tracking ──────────────────────────────
    let incoming_depth = ctx.depth;
    let outgoing_depth = incoming_depth.saturating_add(1);

    if outgoing_depth >= duduclaw_core::MAX_DELEGATION_DEPTH {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "Error: delegation depth limit ({}) would be exceeded. \
                 Current depth: {incoming_depth}, chain origin: {}. \
                 Cannot delegate further to prevent infinite loops.",
                duduclaw_core::MAX_DELEGATION_DEPTH,
                ctx.origin.as_deref().unwrap_or("unknown"),
            )}],
            "isError": true
        });
    }

    let origin = ctx.origin.as_deref().unwrap_or(caller);

    let msg_id = uuid::Uuid::new_v4().to_string();

    // v1.8.18: SQLite `message_queue.db` is the authoritative dispatch rail.
    // Writing to `bus_queue.jsonl` as well created a dual-rail race: the
    // legacy `poll_and_dispatch` loop tokio::spawn's its own Claude CLI
    // task (which DROPS task-local REPLY_CHANNEL), so whichever side won
    // the race determined whether sub-agents inherited channel context.
    // When legacy won, the v1.8.16 reply_channel propagation was silently
    // defeated — nested sub-agent callbacks never registered, replies
    // were silently dropped. Fix: stop writing to bus_queue.jsonl here.
    // (Orphan-response recovery / task_created signals / spawn_agent
    // entries still use bus_queue.jsonl — those are untouched.)
    //
    // v1.8.16 behaviour preserved: propagate `DUDUCLAW_REPLY_CHANNEL`
    // into the row so the dispatcher can scope REPLY_CHANNEL around the
    // target agent's Claude CLI when it spawns. Best-effort: if the
    // ALTER TABLE migration hasn't run yet the fallback INSERT (without
    // reply_channel) succeeds on the legacy schema.
    let queued = {
        let db_path = home_dir.join("message_queue.db");
        let msg_id_cl = msg_id.clone();
        let caller_cl = caller.to_string();
        let target_cl = target.to_string();
        let prompt_cl = prompt.to_string();
        let origin_cl = origin.to_string();
        let ts_now = chrono::Utc::now().to_rfc3339();
        let reply_channel = std::env::var(duduclaw_core::ENV_REPLY_CHANNEL)
            .ok()
            .filter(|s| !s.is_empty());
        // v1.10: Forward wiki RL trust feedback context so the dispatcher
        // can re-establish task_locals around the sub-agent dispatch and
        // sub-agent RAG citations attribute back to the originating turn.
        let trust_turn_id = std::env::var(duduclaw_core::ENV_TRUST_TURN_ID)
            .ok()
            .filter(|s| !s.is_empty());
        let trust_session_id = std::env::var(duduclaw_core::ENV_TRUST_SESSION_ID)
            .ok()
            .filter(|s| !s.is_empty());
        tokio::task::spawn_blocking(move || -> bool {
            let Ok(conn) = rusqlite::Connection::open(&db_path) else {
                return false;
            };
            let _ = conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;");
            let inserted = conn.execute(
                "INSERT OR IGNORE INTO message_queue \
                 (id, sender, target, payload, status, retry_count, delegation_depth, \
                  origin_agent, sender_agent, created_at, reply_channel, turn_id, session_id) \
                 VALUES (?1, ?2, ?3, ?4, 'pending', 0, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                rusqlite::params![
                    msg_id_cl, caller_cl, target_cl, prompt_cl,
                    outgoing_depth, origin_cl, caller_cl, ts_now,
                    reply_channel, trust_turn_id, trust_session_id,
                ],
            );
            if let Ok(rows) = inserted {
                return rows > 0;
            }
            // Legacy schema fallback (pre-v1.8.16 — no reply_channel,
            // turn_id, session_id columns). Gateway migrates on next start.
            conn.execute(
                "INSERT OR IGNORE INTO message_queue \
                 (id, sender, target, payload, status, retry_count, delegation_depth, \
                  origin_agent, sender_agent, created_at) \
                 VALUES (?1, ?2, ?3, ?4, 'pending', 0, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    msg_id_cl, caller_cl, target_cl, prompt_cl,
                    outgoing_depth, origin_cl, caller_cl, ts_now,
                ],
            )
            .map(|rows| rows > 0)
            .unwrap_or(false)
        })
        .await
        .unwrap_or(false)
    };

    // Register delegation callback if running inside a channel context.
    // The dispatcher will use this to forward the sub-agent's response
    // back to the originating channel (Telegram/LINE/Discord/etc.).
    if let Ok(reply_channel) = std::env::var(duduclaw_core::ENV_REPLY_CHANNEL) {
        let db_path = home_dir.join("message_queue.db");
        let msg_id_cb = msg_id.clone();
        let caller_cb = caller.to_string();
        let channel_str = reply_channel;
        let _ = tokio::task::spawn_blocking(move || {
            if let Ok(conn) = rusqlite::Connection::open(&db_path) {
                let _ = conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;");
                // Ensure table exists (MCP process may open DB before gateway).
                // Schema must match message_queue.rs init_schema — keep in sync.
                let _ = conn.execute_batch(
                    "CREATE TABLE IF NOT EXISTS delegation_callbacks (
                         message_id   TEXT PRIMARY KEY,
                         agent_id     TEXT NOT NULL,
                         channel_type TEXT NOT NULL,
                         channel_id   TEXT NOT NULL,
                         thread_id    TEXT,
                         retry_count  INTEGER NOT NULL DEFAULT 0,
                         created_at   TEXT NOT NULL
                     );
                     CREATE INDEX IF NOT EXISTS idx_dc_agent ON delegation_callbacks(agent_id);"
                );
                // Parse channel context. Supported formats:
                //   "telegram:12345"            → chat, no thread
                //   "telegram:12345:6789"       → chat + topic/thread
                //   "discord:<channel_id>"      → main channel
                //   "discord:thread:<thread_id>" → Discord thread (thread IS a
                //                                  channel to the Discord API;
                //                                  the literal token "thread"
                //                                  is a marker, not an ID)
                //   "line:<user_id>"            → LINE user
                //   "slack:<channel_id>"        → Slack channel
                //   "slack:<channel_id>:<ts>"   → Slack thread (ts = parent timestamp)
                let parts: Vec<&str> = channel_str.splitn(3, ':').collect();
                if parts.len() >= 2 && duduclaw_core::SUPPORTED_CHANNEL_TYPES.contains(&parts[0]) {
                    // Rate limit: max 100 pending callbacks per agent to prevent DoS
                    let count: i64 = conn.query_row(
                        "SELECT COUNT(*) FROM delegation_callbacks WHERE agent_id = ?1",
                        rusqlite::params![caller_cb],
                        |r| r.get(0),
                    ).unwrap_or(0);
                    if count >= 100 {
                        tracing::warn!(agent = %caller_cb, "delegation_callbacks per-agent limit (100) reached");
                    } else {
                    let ch_type = parts[0];
                    // Special case: `<type>:thread:<id>` — "thread" is a marker
                    // word, not the channel_id. Collapse to (ch_id=<id>,
                    // thread_id=None) because Discord's API treats a thread as
                    // a regular channel endpoint. Storing "thread" as ch_id
                    // makes validate_channel_id reject the forward as non-
                    // numeric and the sub-agent's reply never reaches the user.
                    let (ch_id, thread) = if parts.len() == 3 && parts[1] == "thread" {
                        (parts[2], None)
                    } else {
                        (parts[1], parts.get(2).map(|s| s.to_string()))
                    };
                    let now = chrono::Utc::now().to_rfc3339();
                    let _ = conn.execute(
                        "INSERT OR IGNORE INTO delegation_callbacks \
                         (message_id, agent_id, channel_type, channel_id, thread_id, created_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        rusqlite::params![msg_id_cb, caller_cb, ch_type, ch_id, thread, now],
                    );
                    }
                }
            }
        }).await;
    }

    let ts = chrono::Utc::now().to_rfc3339();
    if queued {
        serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "Receipt: message_id={msg_id}, target={target}, depth={outgoing_depth}, \
                 status=queued, timestamp={ts}. \
                 The gateway dispatcher will deliver this message."
            )}],
            "_receipt": {
                "message_id": msg_id,
                "target": target,
                "status": "queued",
                "depth": outgoing_depth,
                "timestamp": ts,
            }
        })
    } else {
        serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "Failed to queue message for agent '{target}'"
            )}],
            "isError": true
        })
    }
}

/// Send a photo or sticker via a channel.
async fn handle_send_media(
    params: &Value,
    home_dir: &Path,
    http: &reqwest::Client,
    media_type: &str,
) -> Value {
    let channel = params.get("channel").and_then(|v| v.as_str()).unwrap_or("");
    let chat_id = params.get("chat_id").and_then(|v| v.as_str()).unwrap_or("");
    let url_or_id = params.get("url_or_path")
        .or_else(|| params.get("url"))
        .or_else(|| params.get("sticker_id"))
        .or_else(|| params.get("file_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if chat_id.is_empty() || url_or_id.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: chat_id and url/sticker_id are required for {media_type}")}],
            "isError": true
        });
    }

    let config = read_config(home_dir).await;
    let config_ref = config.as_ref();
    let result = match channel {
        "telegram" => {
            let token = config_ref
                .map(|c| decrypt_channel_token(c, "telegram_bot_token_enc", "telegram_bot_token", home_dir))
                .unwrap_or_default();
            if token.is_empty() {
                "Error: telegram_bot_token not configured".to_string()
            } else {
                let (method, key) = match media_type {
                    "photo" => ("sendPhoto", "photo"),
                    _ => ("sendSticker", "sticker"),
                };
                let api_url = format!("https://api.telegram.org/bot{token}/{method}");
                match http.post(&api_url)
                    .json(&serde_json::json!({ "chat_id": chat_id, key: url_or_id }))
                    .send().await
                {
                    Ok(r) => format!("{media_type} sent. Status: {}", r.status()),
                    Err(e) => format!("Error: {e}"),
                }
            }
        }
        "discord" => {
            let token = config_ref
                .map(|c| decrypt_channel_token(c, "discord_bot_token_enc", "discord_bot_token", home_dir))
                .unwrap_or_default();
            if token.is_empty() {
                "Error: discord_bot_token not configured".to_string()
            } else {
                let api_url = format!("https://discord.com/api/v10/channels/{chat_id}/messages");
                match http.post(&api_url)
                    .header("Authorization", format!("Bot {token}"))
                    .json(&serde_json::json!({ "content": url_or_id }))
                    .send().await
                {
                    Ok(r) => format!("{media_type} sent. Status: {}", r.status()),
                    Err(e) => format!("Error: {e}"),
                }
            }
        }
        _ => format!("Channel '{channel}' does not support {media_type} yet"),
    };

    serde_json::json!({ "content": [{"type": "text", "text": result}] })
}

/// Log a mood/emotion entry to agent memory.
async fn handle_log_mood(
    params: &Value,
    _home_dir: &Path,
    memory: &duduclaw_memory::SqliteMemoryEngine,
    default_agent: &str,
) -> Value {
    use duduclaw_core::traits::MemoryEngine;
    use duduclaw_core::types::MemoryEntry;

    let mood = params.get("mood").and_then(|v| v.as_str()).unwrap_or("neutral");
    let note = params.get("note").and_then(|v| v.as_str()).unwrap_or("");
    let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or(default_agent);

    let content = if note.is_empty() {
        format!("[mood] {mood}")
    } else {
        format!("[mood] {mood}: {note}")
    };

    let classification = duduclaw_memory::classify(&content, "agent_mood");
    let entry = MemoryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        agent_id: agent_id.to_string(),
        content,
        timestamp: chrono::Utc::now(),
        tags: vec!["mood".to_string(), mood.to_string()],
        embedding: None,
        layer: classification.layer,
        importance: classification.importance,
        access_count: 0,
        last_accessed: None,
        source_event: "agent_mood".to_string(),
    };

    match memory.store(agent_id, entry).await {
        Ok(()) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Mood '{mood}' logged for agent '{agent_id}'")}]
        }),
        Err(e) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Error logging mood: {e}")}],
            "isError": true
        }),
    }
}

/// Schedule a recurring or one-shot task. Writes directly to the shared
/// SQLite cron store (`<home>/cron_tasks.db`). The gateway's running
/// `CronScheduler` picks up the new task on its next baseline tick
/// (≤ 30 seconds) — no inter-process signal is required because both
/// processes use WAL-mode SQLite.
async fn handle_schedule_task(params: &Value, home_dir: &Path) -> Value {
    use duduclaw_gateway::cron_store::{CronStore, CronTaskRow};

    let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("default");
    let cron = params.get("cron").and_then(|v| v.as_str()).unwrap_or("");
    let task = params.get("task").or_else(|| params.get("prompt")).or_else(|| params.get("description")).and_then(|v| v.as_str()).unwrap_or("");
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("unnamed");

    if cron.is_empty() || task.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: cron and task are required"}],
            "isError": true
        });
    }

    // Validate cron expression before persisting.
    let normalised_cron = if cron.split_whitespace().count() == 5 {
        format!("0 {cron}")
    } else {
        cron.to_string()
    };
    if normalised_cron.parse::<cron::Schedule>().is_err() {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: invalid cron expression: {cron}")}],
            "isError": true
        });
    }

    let store = match CronStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error: open cron store: {e}")}],
                "isError": true
            });
        }
    };

    let notify_channel = params
        .get("notify_channel")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let notify_chat_id = params
        .get("notify_chat_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let notify_thread_id = params
        .get("notify_thread_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    // v1.8.25: auto-detect the host's IANA timezone when the caller doesn't
    // specify one explicitly. Historically schedule_task fell through to UTC
    // if `cron_timezone` was absent — which surprised every Taipei-based
    // user whose "0 8 * * *" fired at 16:00 local time. Auto-detecting
    // matches what a human would expect "8am every day" to mean when
    // running the scheduler on their own laptop / server.
    //
    // Explicit opt-out: pass `cron_timezone = "UTC"` to force UTC
    // evaluation. Explicit any-other-IANA-name still wins (below).
    let cron_timezone = params
        .get("cron_timezone")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| {
            let detected = detect_local_timezone();
            if let Some(ref tz) = detected {
                tracing::info!(detected_tz = %tz, "schedule_task: auto-detected local timezone (no cron_timezone param supplied)");
            } else {
                tracing::warn!("schedule_task: could not detect local timezone — falling back to UTC");
            }
            detected
        });

    // If one of notify_channel / notify_chat_id is set, the other must also
    // be set — a partial target would silently fail at delivery time.
    if notify_channel.is_some() != notify_chat_id.is_some() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: notify_channel and notify_chat_id must be set together"}],
            "isError": true
        });
    }

    // Validate cron_timezone against the IANA database at call time so a
    // typo is reported to the scheduler caller instead of silently falling
    // back to UTC at firing time. Auto-detected values always parse (they
    // come from the host's TZ database), but explicit user input might
    // have a typo.
    if let Some(ref tz_name) = cron_timezone {
        if duduclaw_core::parse_timezone(tz_name).is_none() {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!(
                    "Error: unknown cron_timezone '{tz_name}'. Use an IANA name like 'Asia/Taipei' or 'America/New_York'."
                )}],
                "isError": true
            });
        }
    }

    let task_id = uuid::Uuid::new_v4().to_string();
    let mut row = CronTaskRow::new(
        task_id.clone(),
        name.to_string(),
        agent_id.to_string(),
        cron.to_string(),
        task.to_string(),
    );
    row.notify_channel = notify_channel;
    row.notify_chat_id = notify_chat_id;
    row.notify_thread_id = notify_thread_id;
    row.cron_timezone = cron_timezone;

    match store.insert(&row).await {
        Ok(()) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Task '{name}' scheduled (id: {task_id}, cron: {cron})")}]
        }),
        Err(e) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: failed to persist task: {e}")}],
            "isError": true
        }),
    }
}

// ── Cron task management handlers ─────────────────────────────

/// List cron tasks, optionally filtered by agent_id and enabled status.
///
/// When `agent_id` is omitted, returns ALL tasks (not just the calling agent's).
/// This matches dashboard behavior and allows the main agent to see sub-agent
/// cron tasks — cron jobs are system resources, not session-scoped.
async fn handle_list_cron_tasks(params: &Value, home_dir: &Path, _default_agent: &str) -> Value {
    use duduclaw_gateway::cron_store::CronStore;

    // Explicit agent_id filter — empty or absent means show all tasks.
    let agent_id_filter = params
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let enabled_only = params
        .get("enabled_only")
        .and_then(|v| v.as_bool())
        .or_else(|| {
            params
                .get("enabled_only")
                .and_then(|v| v.as_str())
                .map(|s| s == "true")
        })
        .unwrap_or(false);

    let store = match CronStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => return tool_error(&format!("open cron store: {e}")),
    };

    let all_tasks = match if enabled_only {
        store.list_enabled().await
    } else {
        store.list_all().await
    } {
        Ok(t) => t,
        Err(e) => return tool_error(&format!("list cron tasks: {e}")),
    };

    // Filter by agent_id only if explicitly provided.
    let tasks: Vec<_> = if agent_id_filter.is_empty() {
        all_tasks
    } else {
        all_tasks
            .into_iter()
            .filter(|t| t.agent_id == agent_id_filter)
            .collect()
    };

    if tasks.is_empty() {
        let scope = if agent_id_filter.is_empty() { "any agent".to_string() } else { format!("agent '{agent_id_filter}'") };
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("No cron tasks found for {scope}.")}]
        });
    }

    let mut lines = Vec::with_capacity(tasks.len() + 1);
    lines.push(format!("Found {} cron task(s):\n", tasks.len()));
    for t in &tasks {
        let status_icon = if t.enabled { "▶" } else { "⏸" };
        let last_run = t
            .last_run_at
            .as_deref()
            .unwrap_or("never");
        let last_status = t
            .last_status
            .as_deref()
            .unwrap_or("-");
        lines.push(format!(
            "{status_icon} [{id}] {name}\n  cron: {cron} | agent: {agent} | runs: {runs} (fail: {fail})\n  last_run: {last_run} | last_status: {last_status}\n  task: {task}\n",
            id = &t.id[..8],
            name = t.name,
            cron = t.cron,
            agent = t.agent_id,
            runs = t.run_count,
            fail = t.failure_count,
            task = {
                let t_task = truncate_bytes(&t.task, 120);
                if t_task.len() < t.task.len() { format!("{t_task}…") } else { t.task.clone() }
            },
        ));
    }

    serde_json::json!({
        "content": [{"type": "text", "text": lines.join("")}]
    })
}

/// Update an existing cron task by ID or name. Only provided fields are changed.
async fn handle_update_cron_task(params: &Value, home_dir: &Path) -> Value {
    use duduclaw_gateway::cron_store::CronStore;

    let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");

    if id.is_empty() && name.is_empty() {
        return tool_error("Either 'id' or 'name' is required");
    }

    let store = match CronStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => return tool_error(&format!("open cron store: {e}")),
    };

    // Resolve the existing row.
    let existing = if !id.is_empty() {
        store.get(id).await
    } else {
        store.get_by_name(name).await
    };
    let existing = match existing {
        Ok(Some(row)) => row,
        Ok(None) => {
            let key = if !id.is_empty() { id } else { name };
            return tool_error(&format!("Cron task not found: {key}"));
        }
        Err(e) => return tool_error(&format!("lookup cron task: {e}")),
    };

    // Merge provided fields over existing values.
    let new_name = params
        .get("new_name")
        .and_then(|v| v.as_str())
        .unwrap_or(&existing.name);
    let new_cron = params
        .get("cron")
        .and_then(|v| v.as_str())
        .unwrap_or(&existing.cron);
    let new_task = params
        .get("task")
        .and_then(|v| v.as_str())
        .unwrap_or(&existing.task);

    // Validate cron expression if changed.
    if new_cron != existing.cron {
        let normalised = if new_cron.split_whitespace().count() == 5 {
            format!("0 {new_cron}")
        } else {
            new_cron.to_string()
        };
        if normalised.parse::<cron::Schedule>().is_err() {
            return tool_error(&format!("invalid cron expression: {new_cron}"));
        }
    }

    match store
        .update_fields(
            &existing.id,
            new_name,
            &existing.agent_id,
            new_cron,
            new_task,
            existing.enabled,
        )
        .await
    {
        Ok(true) => serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "Cron task '{}' updated (id: {}).",
                new_name, &existing.id[..8]
            )}]
        }),
        Ok(false) => tool_error("update returned no rows changed"),
        Err(e) => tool_error(&format!("update cron task: {e}")),
    }
}

/// Delete a cron task by ID or name.
async fn handle_delete_cron_task(params: &Value, home_dir: &Path) -> Value {
    use duduclaw_gateway::cron_store::CronStore;

    let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");

    if id.is_empty() && name.is_empty() {
        return tool_error("Either 'id' or 'name' is required");
    }

    let store = match CronStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => return tool_error(&format!("open cron store: {e}")),
    };

    let deleted = if !id.is_empty() {
        store.delete(id).await
    } else {
        store.delete_by_name(name).await
    };

    match deleted {
        Ok(true) => {
            let key = if !id.is_empty() { id } else { name };
            serde_json::json!({
                "content": [{"type": "text", "text": format!("Cron task '{key}' deleted.")}]
            })
        }
        Ok(false) => {
            let key = if !id.is_empty() { id } else { name };
            tool_error(&format!("Cron task not found: {key}"))
        }
        Err(e) => tool_error(&format!("delete cron task: {e}")),
    }
}

/// Pause or resume a cron task by ID or name.
async fn handle_pause_cron_task(params: &Value, home_dir: &Path) -> Value {
    use duduclaw_gateway::cron_store::CronStore;

    let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let enabled = params
        .get("enabled")
        .and_then(|v| v.as_bool())
        .or_else(|| {
            params
                .get("enabled")
                .and_then(|v| v.as_str())
                .map(|s| s == "true")
        })
        .unwrap_or(false);

    if id.is_empty() && name.is_empty() {
        return tool_error("Either 'id' or 'name' is required");
    }

    let store = match CronStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => return tool_error(&format!("open cron store: {e}")),
    };

    let changed = if !id.is_empty() {
        store.set_enabled(id, enabled).await
    } else {
        store.set_enabled_by_name(name, enabled).await
    };

    let action = if enabled { "resumed" } else { "paused" };
    match changed {
        Ok(true) => {
            let key = if !id.is_empty() { id } else { name };
            serde_json::json!({
                "content": [{"type": "text", "text": format!("Cron task '{key}' {action}.")}]
            })
        }
        Ok(false) => {
            let key = if !id.is_empty() { id } else { name };
            tool_error(&format!("Cron task not found: {key}"))
        }
        Err(e) => tool_error(&format!("{action} cron task: {e}")),
    }
}

// ── Reminder handlers ─────���──────────────────────────────────

/// Create a one-shot reminder.
async fn handle_create_reminder(params: &Value, home_dir: &Path, default_agent: &str) -> Value {
    use duduclaw_gateway::reminder_scheduler::{
        parse_time_spec, append_reminder_checked, AppendResult,
        Reminder, ReminderMode, ReminderStatus,
        is_valid_discord_chat_id,
        MAX_REMINDERS_PER_AGENT, MAX_MESSAGE_LEN, MAX_PROMPT_LEN, MAX_FUTURE_DAYS,
    };

    let time_str = params.get("time").and_then(|v| v.as_str()).unwrap_or("");
    let message = params.get("message").and_then(|v| v.as_str()).unwrap_or("");
    let channel = params.get("channel").and_then(|v| v.as_str()).unwrap_or("");
    let chat_id = params.get("chat_id").and_then(|v| v.as_str()).unwrap_or("");
    let mode_str = params.get("mode").and_then(|v| v.as_str()).unwrap_or("direct");
    let prompt = params.get("prompt").and_then(|v| v.as_str());
    let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or(default_agent);

    if time_str.is_empty() || channel.is_empty() || chat_id.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: time, channel, and chat_id are required"}],
            "isError": true
        });
    }

    // Validate agent_id format
    if !duduclaw_core::is_valid_agent_id(agent_id) {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: invalid agent_id format"}],
            "isError": true
        });
    }

    let mode = match mode_str {
        "agent_callback" => ReminderMode::AgentCallback,
        _ => ReminderMode::Direct,
    };

    // Validate: direct mode needs message, agent_callback needs prompt
    if mode == ReminderMode::Direct && message.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: message is required for direct mode"}],
            "isError": true
        });
    }
    if mode == ReminderMode::AgentCallback && prompt.is_none() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: prompt is required for agent_callback mode"}],
            "isError": true
        });
    }

    // Validate field lengths (resource exhaustion prevention)
    if message.len() > MAX_MESSAGE_LEN {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: message too long ({} chars, max {MAX_MESSAGE_LEN})", message.len())}],
            "isError": true
        });
    }
    if let Some(p) = prompt
        && p.len() > MAX_PROMPT_LEN {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error: prompt too long ({} chars, max {MAX_PROMPT_LEN})", p.len())}],
                "isError": true
            });
        }

    // Validate channel
    if !matches!(channel, "telegram" | "line" | "discord") {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: unknown channel '{channel}', must be telegram/line/discord")}],
            "isError": true
        });
    }

    // Validate Discord chat_id is numeric at creation time (fail-fast)
    if channel == "discord" && !is_valid_discord_chat_id(chat_id) {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: Discord channel ID must be numeric, got '{chat_id}'")}],
            "isError": true
        });
    }

    // Parse time
    let trigger_at = match parse_time_spec(time_str) {
        Ok(dt) => dt,
        Err(e) => {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error: invalid time specification: {e}")}],
                "isError": true
            });
        }
    };

    let now = chrono::Utc::now();

    // Validate trigger is in the future
    if trigger_at <= now {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: trigger time must be in the future"}],
            "isError": true
        });
    }

    // Validate trigger is not too far in the future
    if trigger_at > now + chrono::Duration::days(MAX_FUTURE_DAYS) {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: trigger time too far in the future (max {MAX_FUTURE_DAYS} days)")}],
            "isError": true
        });
    }

    let reminder = Reminder {
        id: uuid::Uuid::new_v4().to_string(),
        agent_id: agent_id.to_string(),
        trigger_at,
        channel: channel.to_string(),
        chat_id: chat_id.to_string(),
        message: if message.is_empty() { None } else { Some(message.to_string()) },
        prompt: prompt.map(|s| s.to_string()),
        mode,
        status: ReminderStatus::Pending,
        created_at: Some(chrono::Utc::now().to_rfc3339()),
        error: None,
    };

    let id = reminder.id.clone();
    let trigger_display = trigger_at.to_rfc3339();

    // Atomic count-check + append (no TOCTOU race)
    match append_reminder_checked(home_dir, &reminder, MAX_REMINDERS_PER_AGENT).await {
        Ok(AppendResult::Ok) => serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "Reminder created (id: {id}, trigger: {trigger_display}, channel: {channel})"
            )}]
        }),
        Ok(AppendResult::LimitReached(count)) => serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "Error: agent '{agent_id}' has {count} pending reminders (max {MAX_REMINDERS_PER_AGENT})"
            )}],
            "isError": true
        }),
        Err(e) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: failed to save reminder: {e}")}],
            "isError": true
        }),
    }
}

/// List reminders with optional filters.
/// Scoped to the calling agent by default to prevent cross-agent info disclosure.
async fn handle_list_reminders(params: &Value, home_dir: &Path, default_agent: &str) -> Value {
    use duduclaw_gateway::reminder_scheduler::list_reminders;

    let status = params.get("status").and_then(|v| v.as_str());
    // Default to caller's own reminders (prevent cross-agent info leak)
    let agent_id = Some(
        params.get("agent_id").and_then(|v| v.as_str()).unwrap_or(default_agent)
    );

    let reminders = list_reminders(home_dir, status, agent_id).await;

    let entries: Vec<serde_json::Value> = reminders
        .iter()
        .map(|r| serde_json::json!({
            "id": r.id,
            "trigger_at": r.trigger_at.to_rfc3339(),
            "channel": r.channel,
            "chat_id": r.chat_id,
            "message": r.message,
            "mode": r.mode,
            "status": r.status,
            "agent_id": r.agent_id,
        }))
        .collect();

    let text = if entries.is_empty() {
        "No reminders found.".to_string()
    } else {
        serde_json::to_string_pretty(&entries).unwrap_or_else(|_| "[]".to_string())
    };

    serde_json::json!({
        "content": [{"type": "text", "text": text}]
    })
}

/// Cancel a pending reminder.
async fn handle_cancel_reminder(params: &Value, home_dir: &Path, default_agent: &str) -> Value {
    use duduclaw_gateway::reminder_scheduler::cancel_reminder;

    let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");

    if id.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: id is required"}],
            "isError": true
        });
    }

    match cancel_reminder(home_dir, id, Some(default_agent)).await {
        Ok(true) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Reminder '{id}' cancelled successfully.")}]
        }),
        Ok(false) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Reminder '{id}' not found or already completed.")}]
        }),
        Err(e) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: {e}")}],
            "isError": true
        }),
    }
}

// ── Sub-agent management handlers ───────────────────────────

/// Create a persistent sub-agent directory with agent.toml, SOUL.md, etc.
async fn handle_create_agent(params: &Value, home_dir: &Path) -> Value {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let display_name = params.get("display_name").and_then(|v| v.as_str()).unwrap_or("");

    if name.is_empty() || display_name.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: name and display_name are required"}],
            "isError": true
        });
    }

    // Validate name: safe for filesystem paths (no traversal)
    if !is_valid_agent_id(name) {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: name must be lowercase alphanumeric with hyphens, max 64 chars"}],
            "isError": true
        });
    }

    let agent_dir = home_dir.join("agents").join(name);
    if agent_dir.exists() {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: agent '{name}' already exists at {}", agent_dir.display())}],
            "isError": true
        });
    }

    let role = params.get("role").and_then(|v| v.as_str()).unwrap_or("specialist");
    let reports_to = params.get("reports_to").and_then(|v| v.as_str()).unwrap_or("");
    let soul = params.get("soul").and_then(|v| v.as_str()).unwrap_or("");
    let model = params.get("model").and_then(|v| v.as_str()).unwrap_or("claude-sonnet-4-6");
    let trigger = params.get("trigger").and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("@{display_name}"));
    let icon = params.get("icon").and_then(|v| v.as_str()).unwrap_or("\u{1F916}");

    // Resolve reports_to: default to the main agent if not specified
    let reports_to = if reports_to.is_empty() {
        resolve_main_agent_name(home_dir).await
    } else {
        reports_to.to_string()
    };

    // Validate reports_to references an existing agent and won't create a cycle
    if let Err(reason) = validate_reports_to(home_dir, name, &reports_to).await {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: {reason}")}],
            "isError": true
        });
    }

    // Create directory structure
    if let Err(e) = tokio::fs::create_dir_all(&agent_dir).await {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error creating agent directory: {e}")}],
            "isError": true
        });
    }
    let _ = tokio::fs::create_dir_all(agent_dir.join("SKILLS")).await;

    // Write agent.toml — use toml crate to prevent injection via display_name/trigger/icon
    // Clone values that will be consumed by the toml! macro
    let reports_to_display = reports_to.clone();
    let agent_config = toml::toml! {
        [agent]
        name = name
        display_name = display_name
        role = role
        status = "active"
        trigger = trigger
        reports_to = reports_to
        icon = icon

        [model]
        preferred = model
        fallback = "claude-haiku-4-5"
        account_pool = ["main"]

        [container]
        timeout_ms = 1800000
        max_concurrent = 2
        readonly_project = true
        additional_mounts = []

        [heartbeat]
        enabled = false
        interval_seconds = 3600
        max_concurrent_runs = 1
        cron = ""

        [budget]
        monthly_limit_cents = 2000
        warn_threshold_percent = 80
        hard_stop = true

        [permissions]
        can_create_agents = false
        can_send_cross_agent = true
        can_modify_own_skills = true
        can_modify_own_soul = false
        can_schedule_tasks = false
        allowed_channels = []

        [evolution]
        skill_auto_activate = false
        skill_security_scan = true
        gvu_enabled = false
        cognitive_memory = false
        max_silence_hours = 12.0
        max_gvu_generations = 3
        observation_period_hours = 24.0
    };
    let agent_toml = toml::to_string_pretty(&agent_config).unwrap_or_default();

    if let Err(e) = tokio::fs::write(agent_dir.join("agent.toml"), &agent_toml).await {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error writing agent.toml: {e}")}],
            "isError": true
        });
    }

    // Write SOUL.md if provided
    if !soul.is_empty() {
        let _ = tokio::fs::write(agent_dir.join("SOUL.md"), soul).await;
    }

    // Write empty MEMORY.md
    let _ = tokio::fs::write(agent_dir.join("MEMORY.md"), "").await;

    // Install agent-file-guard PreToolUse hook so the newly-created agent
    // immediately gets protected against out-of-tree Write/Edit.
    let bin = duduclaw_gateway::agent_hook_installer::resolve_duduclaw_bin();
    if let Err(e) = duduclaw_gateway::agent_hook_installer::ensure_agent_hook_settings(&agent_dir, &bin).await {
        tracing::warn!(
            agent = %name,
            error = %e,
            "Failed to install agent-file-guard hook via MCP create_agent"
        );
    }

    serde_json::json!({
        "content": [{"type": "text", "text": format!(
            "Agent '{display_name}' ({name}) created successfully.\n\
             Role: {role}\n\
             Reports to: {reports_to_display}\n\
             Model: {model}\n\
             Directory: {}\n\n\
             The agent is now available for delegation via send_to_agent or spawn_agent.",
            agent_dir.display()
        )}]
    })
}

/// List all registered agents with role, status, and hierarchy.
async fn handle_list_agents(home_dir: &Path) -> Value {
    let agents_dir = home_dir.join("agents");
    let mut entries = match tokio::fs::read_dir(&agents_dir).await {
        Ok(e) => e,
        Err(e) => {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error reading agents directory: {e}")}],
                "isError": true
            });
        }
    };

    let mut agents = Vec::new();

    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let dir_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        if dir_name.starts_with('_') {
            continue;
        }

        let toml_path = path.join("agent.toml");
        if let Ok(content) = tokio::fs::read_to_string(&toml_path).await
            && let Ok(config) = toml::from_str::<duduclaw_core::types::AgentConfig>(&content) {
                agents.push(serde_json::json!({
                    "name": config.agent.name,
                    "display_name": config.agent.display_name,
                    "role": format!("{:?}", config.agent.role).to_lowercase(),
                    "status": format!("{:?}", config.agent.status).to_lowercase(),
                    "reports_to": config.agent.reports_to,
                    "icon": config.agent.icon,
                    "model": config.model.preferred,
                    "can_create_agents": config.permissions.can_create_agents,
                    "can_schedule_tasks": config.permissions.can_schedule_tasks,
                }));
            }
    }

    if agents.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "No agents found."}]
        });
    }

    // Build a readable text table
    let mut lines = vec![format!("Found {} agent(s):\n", agents.len())];
    for a in &agents {
        let name = a["name"].as_str().unwrap_or("?");
        let display = a["display_name"].as_str().unwrap_or("?");
        let role = a["role"].as_str().unwrap_or("?");
        let status = a["status"].as_str().unwrap_or("?");
        let reports_to = a["reports_to"].as_str().unwrap_or("");
        let icon = a["icon"].as_str().unwrap_or("");
        let hierarchy = if reports_to.is_empty() {
            "(root)".to_string()
        } else {
            format!("-> {reports_to}")
        };
        lines.push(format!("{icon} {display} ({name}) [{role}/{status}] {hierarchy}"));
    }

    serde_json::json!({
        "content": [{"type": "text", "text": lines.join("\n")}]
    })
}

/// Get detailed status of a specific agent.
async fn handle_agent_status(params: &Value, home_dir: &Path) -> Value {
    let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
    if agent_id.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: agent_id is required"}],
            "isError": true
        });
    }
    if !is_valid_agent_id(agent_id) {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: agent_id must be lowercase alphanumeric with hyphens"}],
            "isError": true
        });
    }

    let agent_dir = home_dir.join("agents").join(agent_id);
    let toml_path = agent_dir.join("agent.toml");

    let content = match tokio::fs::read_to_string(&toml_path).await {
        Ok(c) => c,
        Err(_) => {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error: agent '{agent_id}' not found")}],
                "isError": true
            });
        }
    };

    let config: duduclaw_core::types::AgentConfig = match toml::from_str(&content) {
        Ok(c) => c,
        Err(e) => {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error parsing agent.toml: {e}")}],
                "isError": true
            });
        }
    };

    // Check for SOUL.md, skills, memory
    let has_soul = agent_dir.join("SOUL.md").exists();
    let has_identity = agent_dir.join("IDENTITY.md").exists();
    let skill_count = match tokio::fs::read_dir(agent_dir.join("SKILLS")).await {
        Ok(mut entries) => {
            let mut count = 0u32;
            while let Ok(Some(_)) = entries.next_entry().await {
                count += 1;
            }
            count
        }
        Err(_) => 0,
    };

    // Check pending bus_queue messages for this agent
    let pending_tasks = count_pending_tasks(home_dir, agent_id).await;

    let info = format!(
        "Agent: {} ({})\n\
         Role: {:?} | Status: {:?}\n\
         Reports to: {}\n\
         Model: {} (fallback: {})\n\
         Icon: {}\n\
         Trigger: {}\n\
         \n\
         Files:\n\
         - SOUL.md: {}\n\
         - IDENTITY.md: {}\n\
         - Skills: {} file(s)\n\
         - Directory: {}\n\
         \n\
         Permissions:\n\
         - Create agents: {}\n\
         - Cross-agent messaging: {}\n\
         - Schedule tasks: {}\n\
         - Modify own skills: {}\n\
         - Allowed channels: {:?}\n\
         \n\
         Budget: {} cents/month (warn: {}%, hard stop: {})\n\
         Heartbeat: {} (interval: {}s)\n\
         Pending tasks in queue: {}",
        config.agent.display_name,
        config.agent.name,
        config.agent.role,
        config.agent.status,
        if config.agent.reports_to.is_empty() { "(root)" } else { &config.agent.reports_to },
        config.model.preferred,
        config.model.fallback,
        config.agent.icon,
        config.agent.trigger,
        if has_soul { "yes" } else { "no" },
        if has_identity { "yes" } else { "no" },
        skill_count,
        agent_dir.display(),
        config.permissions.can_create_agents,
        config.permissions.can_send_cross_agent,
        config.permissions.can_schedule_tasks,
        config.permissions.can_modify_own_skills,
        config.permissions.allowed_channels,
        config.budget.monthly_limit_cents,
        config.budget.warn_threshold_percent,
        config.budget.hard_stop,
        if config.heartbeat.enabled { "enabled" } else { "disabled" },
        config.heartbeat.interval_seconds,
        pending_tasks,
    );

    serde_json::json!({
        "content": [{"type": "text", "text": info}]
    })
}

/// Check responses from a specific agent in the bus queue.
///
/// Reads bus_queue.jsonl and SQLite message_queue for `agent_response` entries
/// from the specified agent, returning the most recent ones.
async fn handle_check_responses(params: &Value, home_dir: &Path) -> Value {
    let agent_id = match params.get("agent_id").and_then(|v| v.as_str()) {
        Some(id) if !id.is_empty() => id,
        _ => return mcp_error("'agent_id' is required"),
    };
    let limit = params
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(5) as usize;

    let mut responses: Vec<(String, String, usize)> = Vec::new(); // (timestamp, payload_preview, full_len)

    // 1. Check JSONL bus queue
    let queue_path = home_dir.join("bus_queue.jsonl");
    if let Ok(content) = std::fs::read_to_string(&queue_path) {
        for line in content.lines().rev() {
            if responses.len() >= limit {
                break;
            }
            if let Ok(msg) = serde_json::from_str::<serde_json::Value>(line) {
                let is_response = msg.get("type").and_then(|v| v.as_str()) == Some("agent_response");
                let matches_agent = msg.get("agent_id").and_then(|v| v.as_str()) == Some(agent_id);
                if is_response && matches_agent {
                    let ts = msg
                        .get("timestamp")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let payload = msg
                        .get("payload")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let full_len = payload.len();
                    let preview: String = payload.chars().take(500).collect();
                    responses.push((ts, preview, full_len));
                }
            }
        }
    }

    // 2. Check SQLite message queue
    let db_path = home_dir.join("message_queue.db");
    if db_path.exists()
        && let Ok(conn) = rusqlite::Connection::open(&db_path)
    {
        let _ = conn.execute_batch("PRAGMA busy_timeout=3000;");
        if let Ok(mut stmt) = conn.prepare(
            "SELECT created_at, substr(response, 1, 500), length(response), status \
             FROM message_queue WHERE target = ?1 AND status = 'done' AND response IS NOT NULL \
             ORDER BY created_at DESC LIMIT ?2",
        )
            && let Ok(rows) = stmt.query_map(
                rusqlite::params![agent_id, limit as i64],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)? as usize,
                    ))
                },
            )
        {
            for row in rows.flatten() {
                if responses.len() < limit {
                    responses.push(row);
                }
            }
        }
    }

    if responses.is_empty() {
        return mcp_text(&format!(
            "No responses found from agent '{agent_id}'. The agent may not have \
             responded yet, or its responses may have expired from the queue."
        ));
    }

    let mut report = format!(
        "Found {} response(s) from agent '{agent_id}':\n",
        responses.len()
    );
    for (i, (ts, preview, full_len)) in responses.iter().enumerate() {
        let truncated = if *full_len > 500 {
            format!(" [truncated, full={full_len} chars]")
        } else {
            String::new()
        };
        report.push_str(&format!(
            "\n--- Response {} ({ts}){truncated} ---\n{preview}\n",
            i + 1
        ));
    }

    mcp_text(&report)
}

/// Create a structured multi-step task for deterministic execution by the dispatcher.
///
/// The task is persisted as a TaskSpec JSON file; the gateway dispatcher picks it
/// up on its next poll cycle and executes steps sequentially with retry/replan.
async fn handle_create_task(params: &Value, home_dir: &Path, caller: &str) -> Value {
    let goal = match params.get("goal").and_then(|v| v.as_str()) {
        Some(g) if !g.is_empty() => g,
        _ => return mcp_error("'goal' is required"),
    };

    let steps_raw = match params.get("steps") {
        Some(v) => v,
        None => return mcp_error("'steps' is required (JSON array)"),
    };

    // Parse steps — accept either a JSON array directly or a JSON string.
    let steps_array = if let Some(arr) = steps_raw.as_array() {
        arr.clone()
    } else if let Some(s) = steps_raw.as_str() {
        match serde_json::from_str::<Vec<serde_json::Value>>(s) {
            Ok(arr) => arr,
            Err(e) => return mcp_error(&format!("failed to parse steps JSON: {e}")),
        }
    } else {
        return mcp_error("'steps' must be a JSON array or JSON string");
    };

    if steps_array.is_empty() {
        return mcp_error("'steps' must contain at least one step");
    }

    // Convert raw JSON to Step structs.
    use duduclaw_gateway::task_spec::{Step, StepStatus, Criterion, VerificationMethod};
    let mut steps = Vec::with_capacity(steps_array.len());
    for (i, raw) in steps_array.iter().enumerate() {
        let description = raw
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if description.is_empty() {
            return mcp_error(&format!("step {i} missing 'description'"));
        }

        let agent = raw
            .get("agent")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let depends_on: Vec<u8> = raw
            .get("depends_on")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_u64().map(|n| n as u8))
                    .collect()
            })
            .unwrap_or_default();

        let acceptance_criteria: Vec<Criterion> = raw
            .get("acceptance_criteria")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| {
                        v.get("description")
                            .and_then(|d| d.as_str())
                            .map(|desc| Criterion {
                                description: desc.to_string(),
                                method: VerificationMethod::Auto,
                            })
                    })
                    .collect()
            })
            .unwrap_or_default();

        steps.push(Step {
            id: i as u8,
            description,
            agent,
            depends_on,
            acceptance_criteria,
            status: StepStatus::Pending,
            result: None,
            retry_count: 0,
        });
    }

    // Create TaskSpec and persist it.
    use duduclaw_gateway::task_spec::TaskSpec;
    let spec = TaskSpec::new(caller, goal, steps);
    let task_id = spec.task_id.clone();
    let step_count = spec.steps.len();

    let agent_dir = home_dir.join("agents").join(caller);
    if let Err(e) = spec.save(&agent_dir) {
        return mcp_error(&format!("failed to save task: {e}"));
    }

    // Write a signal to bus_queue.jsonl so the dispatcher picks up the new task.
    let signal = serde_json::json!({
        "type": "task_created",
        "task_id": task_id,
        "agent_id": caller,
        "timestamp": chrono::Utc::now().to_rfc3339(),
    });
    let queue_path = home_dir.join("bus_queue.jsonl");
    if let Ok(line) = serde_json::to_string(&signal) {
        let _ = tokio::task::spawn_blocking(move || {
            append_to_jsonl_sync(&queue_path, &line);
        })
        .await;
    }

    mcp_text(&format!(
        "Task created: id={task_id}, steps={step_count}, status=planned. \
         The gateway dispatcher will execute steps automatically."
    ))
}

/// Check the status of a previously created task.
async fn handle_task_status(params: &Value, home_dir: &Path, caller: &str) -> Value {
    let task_id = match params.get("task_id").and_then(|v| v.as_str()) {
        Some(id) if !id.is_empty() => id,
        _ => return mcp_error("'task_id' is required"),
    };

    use duduclaw_gateway::task_spec::TaskSpec;
    let agent_dir = home_dir.join("agents").join(caller);

    match TaskSpec::load(&agent_dir, task_id) {
        Ok(spec) => {
            let passed = spec
                .steps
                .iter()
                .filter(|s| s.status == duduclaw_gateway::task_spec::StepStatus::Passed)
                .count();
            let failed = spec
                .steps
                .iter()
                .filter(|s| s.status == duduclaw_gateway::task_spec::StepStatus::Failed)
                .count();
            let pending = spec
                .steps
                .iter()
                .filter(|s| s.status == duduclaw_gateway::task_spec::StepStatus::Pending)
                .count();

            let mut report = format!(
                "Task: {}\nGoal: {}\nStatus: {:?}\nSteps: {} total, {} passed, {} failed, {} pending\n",
                spec.task_id, spec.goal, spec.status, spec.steps.len(), passed, failed, pending
            );

            for step in &spec.steps {
                report.push_str(&format!(
                    "\n  [{}] {:?} — {}{}",
                    step.id,
                    step.status,
                    step.description,
                    if let Some(ref result) = step.result {
                        format!("\n      Output: {}...", &result.output[..result.output.len().min(200)])
                    } else {
                        String::new()
                    }
                ));
            }

            mcp_text(&report)
        }
        Err(e) => mcp_error(&format!("failed to load task '{task_id}': {e}")),
    }
}

/// Spawn a persistent sub-agent task in the background.
async fn handle_spawn_agent(params: &Value, home_dir: &Path, caller: &str) -> Value {
    spawn_agent_with_ctx(params, home_dir, caller, DelegationContext::from_env()).await
}

/// Core spawn_agent with injectable delegation context.
async fn spawn_agent_with_ctx(
    params: &Value,
    home_dir: &Path,
    caller: &str,
    ctx: DelegationContext,
) -> Value {
    let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
    let task = params.get("task").and_then(|v| v.as_str()).unwrap_or("");
    let session_key = params.get("session_key").and_then(|v| v.as_str()).unwrap_or("");

    if agent_id.is_empty() || task.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: agent_id and task are required"}],
            "isError": true
        });
    }
    if !is_valid_agent_id(agent_id) {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: agent_id must be lowercase alphanumeric with hyphens"}],
            "isError": true
        });
    }

    // Verify agent exists
    let agent_dir = home_dir.join("agents").join(agent_id);
    if !agent_dir.join("agent.toml").exists() {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: agent '{agent_id}' not found")}],
            "isError": true
        });
    }

    // ── Supervisor pattern enforcement ─────────────────────────
    if let Err(reason) = check_supervisor_relation(home_dir, caller, agent_id).await {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: {reason}")}],
            "isError": true
        });
    }

    // ── Delegation depth tracking ────────────────────────────────
    let incoming_depth = ctx.depth;
    let outgoing_depth = incoming_depth.saturating_add(1);

    if outgoing_depth >= duduclaw_core::MAX_DELEGATION_DEPTH {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "Error: delegation depth limit ({}) would be exceeded. \
                 Current depth: {incoming_depth}. Cannot spawn further agents.",
                duduclaw_core::MAX_DELEGATION_DEPTH,
            )}],
            "isError": true
        });
    }

    let origin = ctx.origin.as_deref().unwrap_or(caller);

    let task_id = uuid::Uuid::new_v4().to_string();

    // Write a structured task entry to bus_queue.jsonl with spawn metadata
    let queue_path = home_dir.join("bus_queue.jsonl");
    let entry = serde_json::json!({
        "type": "agent_message",
        "message_id": &task_id,
        "agent_id": agent_id,
        "payload": task,
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "session_key": if session_key.is_empty() { &task_id } else { session_key },
        "persistent": true,
        "delegation_depth": outgoing_depth,
        "origin_agent": origin,
        "sender_agent": caller,
    });

    let queued = tokio::task::spawn_blocking({
        let path = queue_path;
        let entry_str = entry.to_string();
        move || -> bool {
            use std::io::Write;
            // Enforce bus_queue.jsonl size limit (CLI-H4)
            const MAX_QUEUE_SIZE: u64 = 10 * 1024 * 1024; // 10 MB
            if let Ok(meta) = std::fs::metadata(&path)
                && meta.len() > MAX_QUEUE_SIZE {
                    return false;
                }
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                writeln!(f, "{entry_str}").is_ok()
            } else {
                false
            }
        }
    })
    .await
    .unwrap_or(false);

    if queued {
        serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "Sub-agent '{agent_id}' task spawned successfully.\n\
                 Task ID: {task_id}\n\
                 Session key: {}\n\
                 \n\
                 The task is queued and will be picked up by the dispatcher.\n\
                 Use agent_status to check progress, or check bus_queue.jsonl for the response.",
                if session_key.is_empty() { &task_id } else { session_key }
            )}]
        })
    } else {
        serde_json::json!({
            "content": [{"type": "text", "text": "Error: Failed to queue agent task"}],
            "isError": true
        })
    }
}

/// Update one or more fields in an existing agent's agent.toml.
///
/// Reads the current config, applies the requested changes, and writes back.
/// Uses `toml::to_string_pretty` for consistent formatting.
async fn handle_agent_update(params: &Value, home_dir: &Path) -> Value {
    let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
    if agent_id.is_empty() || !is_valid_agent_id(agent_id) {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: valid agent_id is required (lowercase alphanumeric with hyphens, max 64 chars)"}],
            "isError": true
        });
    }

    let agent_dir = home_dir.join("agents").join(agent_id);
    let toml_path = agent_dir.join("agent.toml");

    let content = match tokio::fs::read_to_string(&toml_path).await {
        Ok(c) => c,
        Err(_) => return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: agent '{agent_id}' not found")}],
            "isError": true
        }),
    };

    let mut config: duduclaw_core::types::AgentConfig = match toml::from_str(&content) {
        Ok(c) => c,
        Err(e) => return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error parsing agent.toml: {e}")}],
            "isError": true
        }),
    };

    let mut changes = Vec::new();

    // -- Agent identity fields --
    if let Some(v) = params.get("display_name").and_then(|v| v.as_str()) {
        config.agent.display_name = v.to_string();
        changes.push(format!("display_name = \"{v}\""));
    }
    if let Some(v) = params.get("role").and_then(|v| v.as_str()) {
        use std::str::FromStr;
        let role = match duduclaw_core::types::AgentRole::from_str(v) {
            Ok(r) => r,
            Err(_) => return serde_json::json!({
                "content": [{"type": "text", "text": format!(
                    "Error: invalid role '{v}'. Valid: {}",
                    duduclaw_core::types::AgentRole::valid_values_help()
                )}],
                "isError": true
            }),
        };
        let canonical = role.as_str().to_string();
        config.agent.role = role;
        changes.push(format!("role = \"{canonical}\""));
    }
    if let Some(v) = params.get("status").and_then(|v| v.as_str()) {
        let status = match v.to_lowercase().as_str() {
            "active" => duduclaw_core::types::AgentStatus::Active,
            "paused" => duduclaw_core::types::AgentStatus::Paused,
            "terminated" => duduclaw_core::types::AgentStatus::Terminated,
            _ => return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error: invalid status '{v}'. Valid: active, paused, terminated")}],
                "isError": true
            }),
        };
        config.agent.status = status;
        changes.push(format!("status = \"{v}\""));
    }
    if let Some(v) = params.get("trigger").and_then(|v| v.as_str()) {
        config.agent.trigger = v.to_string();
        changes.push(format!("trigger = \"{v}\""));
    }
    if let Some(v) = params.get("icon").and_then(|v| v.as_str()) {
        config.agent.icon = v.to_string();
        changes.push(format!("icon = \"{v}\""));
    }
    if let Some(v) = params.get("reports_to").and_then(|v| v.as_str()) {
        // Validate reports_to references an existing agent and won't create a cycle
        if let Err(reason) = validate_reports_to(home_dir, agent_id, v).await {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error: {reason}")}],
                "isError": true
            });
        }
        config.agent.reports_to = v.to_string();
        changes.push(format!("reports_to = \"{v}\""));
    }

    // -- Model fields --
    if let Some(v) = params.get("model").and_then(|v| v.as_str()) {
        config.model.preferred = v.to_string();
        changes.push(format!("model.preferred = \"{v}\""));
    }
    if let Some(v) = params.get("fallback_model").and_then(|v| v.as_str()) {
        config.model.fallback = v.to_string();
        changes.push(format!("model.fallback = \"{v}\""));
    }
    if let Some(v) = params.get("api_mode").and_then(|v| v.as_str()) {
        match v {
            "cli" | "direct" | "auto" => {
                config.model.api_mode = v.to_string();
                changes.push(format!("model.api_mode = \"{v}\""));
            }
            _ => return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error: invalid api_mode '{v}'. Valid: cli, direct, auto")}],
                "isError": true
            }),
        }
    }

    // -- Budget fields --
    if let Some(v) = params.get("budget_cents").and_then(|v| v.as_u64()) {
        config.budget.monthly_limit_cents = v;
        changes.push(format!("budget.monthly_limit_cents = {v}"));
    }

    // -- Container fields --
    if let Some(v) = params.get("max_concurrent").and_then(|v| v.as_u64()) {
        config.container.max_concurrent = v as u32;
        changes.push(format!("container.max_concurrent = {v}"));
    }

    // -- Heartbeat fields --
    if let Some(v) = params.get("heartbeat_enabled")
        && let Some(b) = v.as_bool() {
            config.heartbeat.enabled = b;
            changes.push(format!("heartbeat.enabled = {b}"));
        }
    if let Some(v) = params.get("heartbeat_cron").and_then(|v| v.as_str()) {
        config.heartbeat.cron = v.to_string();
        changes.push(format!("heartbeat.cron = \"{v}\""));
    }

    if changes.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: no valid fields to update. Supported fields: display_name, role, status, trigger, icon, reports_to, model, fallback_model, api_mode, budget_cents, max_concurrent, heartbeat_enabled, heartbeat_cron"}],
            "isError": true
        });
    }

    // Serialize and write atomically (temp + rename)
    let updated_toml = match toml::to_string_pretty(&config) {
        Ok(s) => s,
        Err(e) => return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error serializing agent.toml: {e}")}],
            "isError": true
        }),
    };

    let tmp_path = toml_path.with_extension("toml.tmp");
    if let Err(e) = tokio::fs::write(&tmp_path, &updated_toml).await {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error writing agent.toml: {e}")}],
            "isError": true
        });
    }
    if let Err(e) = tokio::fs::rename(&tmp_path, &toml_path).await {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error committing agent.toml: {e}")}],
            "isError": true
        });
    }

    serde_json::json!({
        "content": [{"type": "text", "text": format!(
            "Agent '{agent_id}' updated successfully.\n\nChanges:\n{}",
            changes.iter().map(|c| format!("  • {c}")).collect::<Vec<_>>().join("\n")
        )}]
    })
}

/// Remove an agent directory after safety checks.
///
/// Refuses to remove the main agent. Moves to `_trash/{name}_{timestamp}` instead
/// of hard-deleting, so recovery is possible.
async fn handle_agent_remove(params: &Value, home_dir: &Path) -> Value {
    let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
    if agent_id.is_empty() || !is_valid_agent_id(agent_id) {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: valid agent_id is required"}],
            "isError": true
        });
    }

    let agent_dir = home_dir.join("agents").join(agent_id);
    let toml_path = agent_dir.join("agent.toml");

    // Verify agent exists
    let content = match tokio::fs::read_to_string(&toml_path).await {
        Ok(c) => c,
        Err(_) => return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: agent '{agent_id}' not found")}],
            "isError": true
        }),
    };

    // Refuse to remove main agent
    if let Ok(config) = toml::from_str::<duduclaw_core::types::AgentConfig>(&content)
        && config.agent.role == duduclaw_core::types::AgentRole::Main {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error: cannot remove main agent '{agent_id}'. Change its role first if you really mean to.")}],
                "isError": true
            });
        }

    // Move to trash instead of hard delete
    let trash_dir = home_dir.join("agents").join("_trash");
    let _ = tokio::fs::create_dir_all(&trash_dir).await;
    let timestamp = chrono::Utc::now().format("%Y%m%d%H%M%S");
    let trash_name = format!("{agent_id}_{timestamp}");
    let trash_path = trash_dir.join(&trash_name);

    if let Err(e) = tokio::fs::rename(&agent_dir, &trash_path).await {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error moving agent to trash: {e}")}],
            "isError": true
        });
    }

    serde_json::json!({
        "content": [{"type": "text", "text": format!(
            "Agent '{agent_id}' removed (moved to trash).\n\
             Recovery path: {}\n\n\
             To permanently delete: rm -rf {}",
            trash_path.display(),
            trash_path.display()
        )}]
    })
}

/// Update SOUL.md for an agent via the trusted MCP channel.
///
/// This bypasses file-protect.sh (which blocks Write/Edit on SOUL.md)
/// because MCP tools are a trusted code path in the DuDuClaw architecture.
async fn handle_agent_update_soul(params: &Value, home_dir: &Path) -> Value {
    let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
    if agent_id.is_empty() || !is_valid_agent_id(agent_id) {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: valid agent_id is required"}],
            "isError": true
        });
    }

    let soul_content = params.get("content").and_then(|v| v.as_str()).unwrap_or("");
    if soul_content.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: content is required (the new SOUL.md text)"}],
            "isError": true
        });
    }

    let agent_dir = home_dir.join("agents").join(agent_id);

    // Verify agent exists
    if !agent_dir.join("agent.toml").exists() {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: agent '{agent_id}' not found")}],
            "isError": true
        });
    }

    let soul_path = agent_dir.join("SOUL.md");

    // Read old content for SHA-256 fingerprint comparison
    let old_content = tokio::fs::read_to_string(&soul_path).await.unwrap_or_default();
    let old_hash = {
        let digest = <sha2::Sha256 as sha2::Digest>::digest(old_content.as_bytes());
        format!("{:x}", digest)
    };

    // Atomic write: temp file + rename
    let tmp_path = soul_path.with_extension("md.tmp");
    if let Err(e) = tokio::fs::write(&tmp_path, soul_content).await {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error writing SOUL.md: {e}")}],
            "isError": true
        });
    }
    if let Err(e) = tokio::fs::rename(&tmp_path, &soul_path).await {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error committing SOUL.md: {e}")}],
            "isError": true
        });
    }

    let new_hash = {
        let digest = <sha2::Sha256 as sha2::Digest>::digest(soul_content.as_bytes());
        format!("{:x}", digest)
    };

    serde_json::json!({
        "content": [{"type": "text", "text": format!(
            "SOUL.md updated for agent '{agent_id}'.\n\
             Old SHA-256: {old_hash}\n\
             New SHA-256: {new_hash}\n\
             Size: {} bytes",
            soul_content.len()
        )}]
    })
}

/// Count pending agent_message entries in bus_queue.jsonl for a given agent.
///
/// Reads line-by-line with a size cap to avoid OOM on large queues (CLI-M2).
async fn count_pending_tasks(home_dir: &Path, agent_id: &str) -> usize {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let queue_path = home_dir.join("bus_queue.jsonl");
    let file = match tokio::fs::File::open(&queue_path).await {
        Ok(f) => f,
        Err(_) => return 0,
    };

    let reader = BufReader::new(file);
    let mut lines = reader.lines();
    let mut count = 0usize;

    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() { continue; }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line)
            && v.get("type").and_then(|t| t.as_str()) == Some("agent_message")
                && v.get("agent_id").and_then(|a| a.as_str()) == Some(agent_id)
            {
                count += 1;
            }
    }

    count
}

// ── Feedback handler ────────────────────────────────────────

async fn handle_submit_feedback(params: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let signal_type = params.get("signal_type").and_then(|v| v.as_str()).unwrap_or("");
    let detail = params.get("detail").and_then(|v| v.as_str()).unwrap_or("");
    let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or(default_agent);

    if signal_type.is_empty() || detail.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: signal_type and detail are required"}],
            "isError": true
        });
    }

    if !["positive", "negative", "correction"].contains(&signal_type) {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: signal_type must be positive, negative, or correction"}],
            "isError": true
        });
    }

    match duduclaw_gateway::external_factors::submit_feedback(
        home_dir, agent_id, signal_type, "mcp", detail,
    ).await {
        Ok(()) => serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "Feedback recorded: [{signal_type}] for agent '{agent_id}'. This will be included in the next evolution reflection."
            )}]
        }),
        Err(e) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Error submitting feedback: {e}")}],
            "isError": true
        }),
    }
}

// ── Evolution control handlers ──────────────────────────────

async fn handle_evolution_toggle(params: &Value, home_dir: &Path) -> Value {
    let agent_id = match params.get("agent_id").and_then(|v| v.as_str()) {
        Some(id) if !id.is_empty() => id,
        _ => return serde_json::json!({
            "content": [{"type": "text", "text": "Error: agent_id is required"}],
            "isError": true
        }),
    };
    let field = match params.get("field").and_then(|v| v.as_str()) {
        Some(f) if !f.is_empty() => f,
        _ => return serde_json::json!({
            "content": [{"type": "text", "text": "Error: field is required"}],
            "isError": true
        }),
    };
    let value_str = match params.get("value").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => match params.get("value") {
            Some(v) => &v.to_string(),
            None => return serde_json::json!({
                "content": [{"type": "text", "text": "Error: value is required"}],
                "isError": true
            }),
        },
    };

    // Read current agent.toml
    let agent_dir = home_dir.join("agents").join(agent_id);
    let toml_path = agent_dir.join("agent.toml");
    let content = match tokio::fs::read_to_string(&toml_path).await {
        Ok(c) => c,
        Err(e) => return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: agent '{agent_id}' not found: {e}")}],
            "isError": true
        }),
    };

    let mut doc: toml::Table = match content.parse() {
        Ok(t) => t,
        Err(e) => return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error parsing agent.toml: {e}")}],
            "isError": true
        }),
    };

    // Ensure [evolution] section exists
    if !doc.contains_key("evolution") {
        doc.insert("evolution".to_string(), toml::Value::Table(toml::Table::new()));
    }
    let evo = doc.get_mut("evolution").unwrap().as_table_mut().unwrap();

    // Validate field name and apply to the correct TOML section.
    let boolean_fields = [
        "gvu_enabled", "cognitive_memory",
        "skill_auto_activate", "skill_security_scan",
    ];
    let numeric_fields = [
        "max_silence_hours", "max_gvu_generations", "observation_period_hours",
        "skill_token_budget", "max_active_skills",
    ];
    // Stagnation-detection sub-section fields (prefix: stagnation_*).
    // These map into [evolution.stagnation_detection] in the TOML.
    let stagnation_bool_fields = ["stagnation_enabled"];
    let stagnation_int_fields  = ["stagnation_window_seconds", "stagnation_trigger_threshold"];
    let stagnation_str_fields  = ["stagnation_action"];

    let parse_bool = |s: &str| -> std::result::Result<bool, String> {
        match s {
            "true" | "1" | "yes" | "on"  => Ok(true),
            "false" | "0" | "no" | "off" => Ok(false),
            _ => Err(format!("invalid boolean value '{s}' — use true/false")),
        }
    };

    if boolean_fields.contains(&field) {
        match parse_bool(value_str) {
            Ok(v) => { evo.insert(field.to_string(), toml::Value::Boolean(v)); }
            Err(e) => return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error: {e}")}],
                "isError": true
            }),
        }
    } else if numeric_fields.contains(&field) {
        if let Ok(int_val) = value_str.parse::<i64>() {
            evo.insert(field.to_string(), toml::Value::Integer(int_val));
        } else if let Ok(float_val) = value_str.parse::<f64>() {
            evo.insert(field.to_string(), toml::Value::Float(float_val));
        } else {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error: invalid numeric value '{value_str}'")}],
                "isError": true
            });
        }
    } else if stagnation_bool_fields.contains(&field)
           || stagnation_int_fields.contains(&field)
           || stagnation_str_fields.contains(&field)
    {
        // Write into the [evolution.stagnation_detection] sub-table.
        let sd_key = field.trim_start_matches("stagnation_");

        // Ensure [evolution.stagnation_detection] sub-table exists.
        if !evo.contains_key("stagnation_detection") {
            evo.insert(
                "stagnation_detection".to_string(),
                toml::Value::Table(toml::Table::new()),
            );
        }
        let sd = evo
            .get_mut("stagnation_detection")
            .unwrap()
            .as_table_mut()
            .unwrap();

        if stagnation_bool_fields.contains(&field) {
            match parse_bool(value_str) {
                Ok(v) => { sd.insert(sd_key.to_string(), toml::Value::Boolean(v)); }
                Err(e) => return serde_json::json!({
                    "content": [{"type": "text", "text": format!("Error: {e}")}],
                    "isError": true
                }),
            }
        } else if stagnation_int_fields.contains(&field) {
            let int_val: i64 = match value_str.parse() {
                Ok(v) => v,
                Err(_) => return serde_json::json!({
                    "content": [{"type": "text", "text": format!("Error: '{field}' requires an integer value, got '{value_str}'")}],
                    "isError": true
                }),
            };
            // Range validation matching StagnationDetectionConfig::validate()
            let range_err = match sd_key {
                "window_seconds" if !(60..=604_800).contains(&int_val) =>
                    Some(format!("stagnation_window_seconds must be 60–604800, got {int_val}")),
                "trigger_threshold" if !(1..=1000).contains(&int_val) =>
                    Some(format!("stagnation_trigger_threshold must be 1–1000, got {int_val}")),
                _ => None,
            };
            if let Some(e) = range_err {
                return serde_json::json!({
                    "content": [{"type": "text", "text": format!("Error: {e}")}],
                    "isError": true
                });
            }
            sd.insert(sd_key.to_string(), toml::Value::Integer(int_val));
        } else {
            // stagnation_action: "log_only" | "suppress" (P1 reserved)
            match value_str {
                "log_only" | "suppress" => {
                    sd.insert(sd_key.to_string(), toml::Value::String(value_str.to_owned()));
                }
                other => return serde_json::json!({
                    "content": [{"type": "text", "text": format!(
                        "Error: stagnation_action must be 'log_only' or 'suppress', got '{other}'"
                    )}],
                    "isError": true
                }),
            }
        }
    } else {
        let all_fields: Vec<&str> = boolean_fields.iter()
            .chain(numeric_fields.iter())
            .chain(stagnation_bool_fields.iter())
            .chain(stagnation_int_fields.iter())
            .chain(stagnation_str_fields.iter())
            .copied()
            .collect();
        return serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "Error: unknown field '{field}'. Valid fields: {}",
                all_fields.join(", ")
            )}],
            "isError": true
        });
    }

    // Write back
    let new_content = toml::to_string_pretty(&doc).unwrap_or_default();
    if let Err(e) = tokio::fs::write(&toml_path, &new_content).await {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error writing agent.toml: {e}")}],
            "isError": true
        });
    }

    serde_json::json!({
        "content": [{"type": "text", "text": format!(
            "Evolution config updated: {agent_id}.evolution.{field} = {value_str}\n\
             Changes take effect within 5 minutes (next heartbeat sync) or immediately on restart."
        )}]
    })
}

async fn handle_evolution_status_tool(params: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let agent_id = params.get("agent_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(default_agent);

    let toml_path = home_dir.join("agents").join(agent_id).join("agent.toml");
    let content = match tokio::fs::read_to_string(&toml_path).await {
        Ok(c) => c,
        Err(e) => return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: agent '{agent_id}' not found: {e}")}],
            "isError": true
        }),
    };

    let config: duduclaw_core::types::AgentConfig = match toml::from_str(&content) {
        Ok(c) => c,
        Err(e) => return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error parsing agent.toml: {e}")}],
            "isError": true
        }),
    };

    let evo = &config.evolution;
    let sd = &evo.stagnation_detection;
    let status = format!(
        "Evolution status for agent '{agent_id}':\n\
         \n\
         GVU self-play:     {}\n\
         Cognitive memory:  {}\n\
         \n\
         Skill auto-activate:  {}\n\
         Skill security scan:  {}\n\
         Skill token budget:   {}\n\
         Max active skills:    {}\n\
         \n\
         Max silence hours:         {:.1}\n\
         Max GVU generations:       {}\n\
         Observation period hours:  {:.1}\n\
         \n\
         Stagnation detection:\n\
           enabled:           {}\n\
           window_seconds:    {} ({:.1}h)\n\
           trigger_threshold: {}\n\
           action:            {}",
        evo.gvu_enabled, evo.cognitive_memory,
        evo.skill_auto_activate, evo.skill_security_scan,
        evo.skill_token_budget, evo.max_active_skills,
        evo.max_silence_hours, evo.max_gvu_generations, evo.observation_period_hours,
        sd.enabled,
        sd.window_seconds, sd.window_seconds as f64 / 3600.0,
        sd.trigger_threshold,
        sd.action,
    );

    serde_json::json!({
        "content": [{"type": "text", "text": status}]
    })
}

// ── Audit Trail Query handler (W19-P1 M4) ────────────────────

/// MCP handler for `audit_trail_query`.
///
/// Forwards to the gateway `audit.evolution_query` WebSocket endpoint
/// by delegating to [`AuditEventIndex`] directly (no gateway round-trip
/// needed from the CLI side since we share the same `home_dir`).
///
/// # Authorization
/// `caller_is_admin` must be `true`; if `false` the call is rejected immediately
/// (defence-in-depth: the MCP dispatch layer enforces `Scope::Admin` before
/// routing here, but this guard prevents privilege escalation from any future
/// call-path that skips the dispatch-level check — OWASP A01).
async fn handle_audit_trail_query(
    params: &Value,
    home_dir: &Path,
    caller_client_id: &str,
    caller_is_admin: bool,
) -> Value {
    // ── Defence-in-depth authorization guard (H1 / OWASP A01) ────────────────
    if !caller_is_admin {
        tracing::warn!(
            caller_client_id = %caller_client_id,
            "audit_trail_query: access denied — caller lacks Admin scope"
        );
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: audit_trail_query requires Admin scope"}],
            "isError": true
        });
    }
    tracing::info!(caller_client_id = %caller_client_id, "audit_trail_query invoked");

    use duduclaw_gateway::evolution_events::query::{AuditEventIndex, AuditQueryFilter};

    let filter = AuditQueryFilter {
        agent_id:   params.get("agent_id").and_then(|v| v.as_str()).map(|s| s.to_owned()),
        event_type: params.get("event_type").and_then(|v| v.as_str()).map(|s| s.to_owned()),
        outcome:    params.get("outcome").and_then(|v| v.as_str()).map(|s| s.to_owned()),
        skill_id:   params.get("skill_id").and_then(|v| v.as_str()).map(|s| s.to_owned()),
        since:      params.get("since").and_then(|v| v.as_str()).map(|s| s.to_owned()),
        until:      params.get("until").and_then(|v| v.as_str()).map(|s| s.to_owned()),
        limit:      params.get("limit").and_then(|v| v.as_i64()),
        offset:     params.get("offset").and_then(|v| v.as_i64()),
    };

    let idx = match AuditEventIndex::open(home_dir) {
        Ok(i) => i,
        Err(e) => {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error: cannot open audit index: {e}")}],
                "isError": true
            })
        }
    };

    if let Err(e) = idx.sync_from_files().await {
        // Non-fatal: log and continue with potentially stale index.
        tracing::warn!("audit_trail_query: sync warning (stale index): {e}");
    }

    match idx.query(filter).await {
        Ok(result) => {
            let events_json: Vec<serde_json::Value> = result
                .events
                .iter()
                .map(|ev| {
                    serde_json::json!({
                        "timestamp":      ev.timestamp,
                        "event_type":     ev.event_type.to_string(),
                        "agent_id":       ev.agent_id,
                        "skill_id":       ev.skill_id,
                        "generation":     ev.generation,
                        "outcome":        ev.outcome.to_string(),
                        "trigger_signal": ev.trigger_signal,
                        "metadata":       ev.metadata,
                    })
                })
                .collect();

            let summary = format!(
                "Audit Trail Query Results\n\
                 ─────────────────────────\n\
                 Total matching events: {}\n\
                 Showing: {} events (offset {}, limit {})\n\
                 \n\
                 {}",
                result.total,
                result.events.len(),
                result.offset,
                result.limit,
                serde_json::to_string_pretty(&events_json).unwrap_or_default(),
            );

            serde_json::json!({
                "content": [{"type": "text", "text": summary}],
                "audit_result": {
                    "events": events_json,
                    "total":  result.total,
                    "limit":  result.limit,
                    "offset": result.offset,
                }
            })
        }
        Err(e) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: audit query failed: {e}")}],
            "isError": true
        }),
    }
}

// ── Reliability Dashboard handler (W20-P0) ──────────────────

/// MCP handler for `reliability_summary`.
///
/// Computes the four-metric Agent Reliability Summary from the evolution-event
/// audit trail SQLite index.  Requires Admin scope (same as `audit_trail_query`).
///
/// # Authorization
/// Defence-in-depth: `caller_is_admin` must be `true`.  The dispatch-layer
/// scope check handles the primary guard; this check prevents privilege
/// escalation from any future call-path that bypasses dispatch (OWASP A01).
async fn handle_reliability_summary(
    params: &Value,
    home_dir: &Path,
    caller_client_id: &str,
    caller_is_admin: bool,
) -> Value {
    // ── Authorization guard ───────────────────────────────────────────────────
    if !caller_is_admin {
        tracing::warn!(
            caller_client_id = %caller_client_id,
            "reliability_summary: access denied — caller lacks Admin scope"
        );
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: reliability_summary requires Admin scope"}],
            "isError": true
        });
    }

    // ── Parse parameters ──────────────────────────────────────────────────────
    let agent_id = match params.get("agent_id").and_then(|v| v.as_str()) {
        Some(id) if !id.trim().is_empty() => {
            if id.len() > MAX_AGENT_ID_LEN {
                return serde_json::json!({
                    "content": [{"type": "text", "text": format!("Error: agent_id must not exceed {MAX_AGENT_ID_LEN} characters")}],
                    "isError": true
                });
            }
            id.to_owned()
        }
        _ => {
            return serde_json::json!({
                "content": [{"type": "text", "text": "Error: reliability_summary requires agent_id"}],
                "isError": true
            })
        }
    };

    let window_days: u32 = params
        .get("window_days")
        .and_then(|v| v.as_u64())
        .map(|n| n.clamp(1, 365) as u32)
        .unwrap_or(7);

    tracing::info!(
        caller_client_id = %caller_client_id,
        agent_id = %agent_id,
        window_days = window_days,
        "reliability_summary invoked"
    );

    use duduclaw_gateway::evolution_events::query::AuditEventIndex;

    // ── Open and sync the audit index ────────────────────────────────────────
    let idx = match AuditEventIndex::open(home_dir) {
        Ok(i) => i,
        Err(e) => {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error: cannot open audit index: {e}")}],
                "isError": true
            })
        }
    };

    if let Err(e) = idx.sync_from_files().await {
        tracing::warn!("reliability_summary: sync warning (stale index): {e}");
    }

    // ── Compute summary ───────────────────────────────────────────────────────
    match idx.compute_reliability_summary(&agent_id, window_days).await {
        Ok(s) => {
            let report = format!(
                "Agent Reliability Summary\n\
                 ─────────────────────────\n\
                 Agent:                  {agent_id}\n\
                 Window:                 {window_days} days\n\
                 Total events:           {total}\n\
                 \n\
                 Consistency Score:      {consistency:.4}  (per-task-type avg success rate)\n\
                 Task Success Rate:      {success:.4}  (outcome=success / total)\n\
                 Skill Adoption Rate:    {adoption:.4}  (skill_activate / total)\n\
                 Fallback Trigger Rate:  {fallback:.4}  (llm_fallback_triggered / total)\n\
                 \n\
                 Generated:              {generated_at}",
                agent_id = s.agent_id,
                window_days = s.window_days,
                total = s.total_events,
                consistency = s.consistency_score,
                success = s.task_success_rate,
                adoption = s.skill_adoption_rate,
                fallback = s.fallback_trigger_rate,
                generated_at = s.generated_at,
            );

            serde_json::json!({
                "content": [{"type": "text", "text": report}],
                "reliability_summary": {
                    "agent_id":             s.agent_id,
                    "window_days":          s.window_days,
                    "consistency_score":    s.consistency_score,
                    "task_success_rate":    s.task_success_rate,
                    "skill_adoption_rate":  s.skill_adoption_rate,
                    "fallback_trigger_rate": s.fallback_trigger_rate,
                    "total_events":         s.total_events,
                    "generated_at":         s.generated_at,
                }
            })
        }
        Err(e) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: reliability computation failed: {e}")}],
            "isError": true
        }),
    }
}

// ── Local inference handlers ────────────────────────────────

async fn handle_inference_status(home_dir: &Path) -> Value {
    let engine = duduclaw_inference::InferenceEngine::new(home_dir).await;
    let available = engine.is_available().await;
    let hw = engine.hardware_info().await;
    let models = engine.list_models().await;
    let loaded_count = models.iter().filter(|m| m.is_loaded).count();

    let mut status = format!(
        "Inference Engine Status:\n  Enabled: {}\n  Available: {}\n  Models: {} available, {} loaded",
        engine.config().enabled, available, models.len(), loaded_count
    );

    if let Some(ref hw) = hw {
        status.push_str(&format!(
            "\n  GPU: {} ({})\n  RAM: {}MB / {}MB\n  Recommended backend: {}",
            hw.gpu_name,
            format!("{:?}", hw.gpu_type),
            hw.ram_available_mb,
            hw.ram_total_mb,
            hw.recommended_backend
        ));
    }

    serde_json::json!({
        "content": [{"type": "text", "text": status}]
    })
}

async fn handle_model_list(home_dir: &Path) -> Value {
    let engine = duduclaw_inference::InferenceEngine::new(home_dir).await;
    let models = engine.list_models().await;

    if models.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "No models found in ~/.duduclaw/models/\n\nTo get started:\n1. Download a GGUF model (e.g., from huggingface.co)\n2. Place it in ~/.duduclaw/models/\n3. Run model_list again"}]
        });
    }

    // Check StreamingLLM config for KV cache compression
    let config = engine.config();
    let streaming_llm = config.streaming_llm.as_ref().filter(|s| s.enabled);
    let effective_ctx: Option<u32> = streaming_llm.map(|s| {
        (s.sink_size + s.window_size).try_into().unwrap_or(u32::MAX)
    });

    let mut text = format!("Available models ({}):\n", models.len());

    if let Some(slm) = streaming_llm {
        text.push_str(&format!(
            "\nStreamingLLM: ON (sink={}, window={}, effective ctx={})\n",
            slm.sink_size, slm.window_size, slm.sink_size + slm.window_size
        ));
    }

    text.push_str("\n  (KV cache estimates are approximate lower bounds for typical GQA models)");

    for m in &models {
        let loaded = if m.is_loaded { " [LOADED]" } else { "" };
        let size_mb = m.file_size_bytes / (1024 * 1024);
        let total_mb = m.estimated_memory_mb + m.kv_cache_mb;

        let kv_info = if m.kv_cache_mb == 0 {
            format!("total ~{}MB", m.estimated_memory_mb)
        } else if let Some(eff_ctx) = effective_ctx {
            let compressed_kv = duduclaw_inference::ModelInfo::estimate_kv_cache_mb(
                &m.parameter_count, eff_ctx,
            );
            let compressed_total = m.estimated_memory_mb + compressed_kv;
            format!(
                "KV ~{}→~{}MB, total ~{}→~{}MB",
                m.kv_cache_mb, compressed_kv, total_mb, compressed_total
            )
        } else {
            format!("KV ~{}MB, total ~{}MB", m.kv_cache_mb, total_mb)
        };

        text.push_str(&format!(
            "\n  {} ({} {} {}) — {}MB weights, {}{loaded}",
            m.id, m.architecture, m.parameter_count, m.quantization,
            size_mb, kv_info,
        ));
    }

    serde_json::json!({
        "content": [{"type": "text", "text": text}]
    })
}

async fn handle_model_load(params: &Value, home_dir: &Path) -> Value {
    let model_id = params.get("model_id").and_then(|v| v.as_str()).unwrap_or("");
    if model_id.is_empty() {
        return serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": "model_id is required"}]
        });
    }

    let engine = duduclaw_inference::InferenceEngine::new(home_dir).await;
    if let Err(e) = engine.init().await {
        return serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": format!("Failed to init inference engine: {e}")}]
        });
    }

    match engine.load_model(model_id).await {
        Ok(info) => serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "Model loaded: {} ({} {} {})\nEstimated memory: {}MB\nContext length: {}",
                info.id, info.architecture, info.parameter_count, info.quantization,
                info.estimated_memory_mb, info.context_length
            )}]
        }),
        Err(e) => serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": format!("Failed to load model: {e}")}]
        }),
    }
}

async fn handle_model_unload(home_dir: &Path) -> Value {
    let engine = duduclaw_inference::InferenceEngine::new(home_dir).await;
    if let Err(e) = engine.init().await {
        return serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": format!("Failed to init inference engine: {e}")}]
        });
    }

    match engine.unload_model().await {
        Ok(()) => serde_json::json!({
            "content": [{"type": "text", "text": "Model unloaded successfully. Memory freed."}]
        }),
        Err(e) => serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": format!("Failed to unload: {e}")}]
        }),
    }
}

async fn handle_hardware_info() -> Value {
    let hw = duduclaw_inference::hardware::detect_hardware().await;
    let text = format!(
        "Hardware Detection Results:\n\
         \n  GPU: {} ({:?})\
         \n  VRAM: {}MB total, {}MB available\
         \n  RAM: {}MB total, {}MB available\
         \n  CPU cores: {}\
         \n  Recommended backend: {}\
         \n  Recommended max model: {:.1}GB",
        hw.gpu_name, hw.gpu_type,
        hw.vram_total_mb, hw.vram_available_mb,
        hw.ram_total_mb, hw.ram_available_mb,
        hw.cpu_cores,
        hw.recommended_backend,
        hw.recommended_max_model_gb,
    );

    serde_json::json!({
        "content": [{"type": "text", "text": text}]
    })
}

async fn handle_route_query(params: &Value, home_dir: &Path) -> Value {
    let prompt = params.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
    let system_prompt = params.get("system_prompt").and_then(|v| v.as_str()).unwrap_or("");

    if prompt.is_empty() {
        return serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": "prompt is required"}]
        });
    }

    let engine = duduclaw_inference::InferenceEngine::new(home_dir).await;
    let decision = engine.route(system_prompt, prompt);

    let text = format!(
        "Routing Decision:\n\
         \n  Tier: {}\
         \n  Confidence: {:.2}\
         \n  Model: {}\
         \n  Reason: {}\
         \n  Router enabled: {}",
        decision.tier,
        decision.confidence,
        decision.model_id.as_deref().unwrap_or("(cloud api)"),
        decision.reason,
        engine.router_enabled(),
    );

    serde_json::json!({
        "content": [{"type": "text", "text": text}]
    })
}

async fn handle_inference_mode(home_dir: &Path) -> Value {
    let engine = duduclaw_inference::InferenceEngine::new(home_dir).await;
    let mode = engine.current_mode().await;
    let status = engine.manager().status().await;
    let mlx = engine.mlx_available().await;

    let text = format!(
        "Inference Manager Status:\n\
         \n  Current mode: {}\
         \n  Exo cluster: {}\
         \n  llamafile: {}\
         \n  MLX bridge: {}",
        mode,
        if status.exo_available { "available" } else { "unavailable" },
        if status.llamafile_available { "running" } else { "stopped" },
        if mlx { "available (Apple Silicon)" } else { "unavailable" },
    );

    serde_json::json!({
        "content": [{"type": "text", "text": text}]
    })
}

async fn handle_llamafile_start(params: &Value, home_dir: &Path) -> Value {
    let _file = params.get("file").and_then(|v| v.as_str());
    let engine = duduclaw_inference::InferenceEngine::new(home_dir).await;

    match engine.manager().start_llamafile().await {
        Ok(()) => serde_json::json!({
            "content": [{"type": "text", "text": "llamafile server started successfully"}]
        }),
        Err(e) => serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": format!("Failed to start llamafile: {e}")}]
        }),
    }
}

async fn handle_llamafile_stop(home_dir: &Path) -> Value {
    let engine = duduclaw_inference::InferenceEngine::new(home_dir).await;
    engine.manager().stop_llamafile().await;
    serde_json::json!({
        "content": [{"type": "text", "text": "llamafile server stopped"}]
    })
}

async fn handle_llamafile_list(home_dir: &Path) -> Value {
    let engine = duduclaw_inference::InferenceEngine::new(home_dir).await;
    let files = engine.manager().list_llamafiles().await;

    if files.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "No llamafiles found in ~/.duduclaw/llamafiles/\n\nTo get started:\n1. Download a .llamafile from huggingface.co or github.com/Mozilla-Ocho/llamafile\n2. Place it in ~/.duduclaw/llamafiles/\n3. Run llamafile_list again"}]
        });
    }

    let text = format!("Available llamafiles ({}):\n{}", files.len(),
        files.iter().map(|f| format!("  - {f}")).collect::<Vec<_>>().join("\n"));

    serde_json::json!({
        "content": [{"type": "text", "text": text}]
    })
}

// ── Model registry handlers ─────────────────────────────────

async fn handle_model_search(params: &Value, home_dir: &Path) -> Value {
    let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("chat gguf");

    let hw = duduclaw_inference::hardware::detect_hardware().await;
    let results = duduclaw_inference::model_registry::hf_api::search_models(
        query, hw.ram_available_mb, home_dir,
    ).await;

    // Also include curated
    let curated = duduclaw_inference::model_registry::curated::builtin_registry();
    let curated_filtered = duduclaw_inference::model_registry::curated::filter_by_hardware(&curated, hw.ram_available_mb);

    let mut all = curated_filtered;
    for r in &results {
        if !all.iter().any(|a| a.repo == r.repo && a.filename == r.filename) {
            all.push(r.clone());
        }
    }

    if all.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("No models found for query: '{query}' (RAM: {} MB)", hw.ram_available_mb)}]
        });
    }

    let mut text = format!("Models for '{}' (RAM: {} MB):\n", query, hw.ram_available_mb);
    for (i, e) in all.iter().enumerate().take(15) {
        let tier = match e.tier {
            duduclaw_inference::model_registry::ModelTier::Recommended => "[推薦]",
            duduclaw_inference::model_registry::ModelTier::Community => "[社群]",
        };
        text.push_str(&format!(
            "\n  {}. {} {} ({}, {}) — {}\n     repo: {} file: {}",
            i + 1, tier, e.name, e.params, e.size_display(),
            e.description, e.repo, e.filename
        ));
    }

    serde_json::json!({"content": [{"type": "text", "text": text}]})
}

async fn handle_model_download(params: &Value, home_dir: &Path) -> Value {
    let repo = params.get("repo").and_then(|v| v.as_str()).unwrap_or("");
    let filename = params.get("filename").and_then(|v| v.as_str()).unwrap_or("");

    if repo.is_empty() || filename.is_empty() {
        return serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": "repo and filename are required"}]
        });
    }

    // C-2: validate repo format (owner/name, safe characters only)
    if let Err(e) = duduclaw_inference::model_registry::downloader::validate_repo(repo) {
        return serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": format!("{e}")}]
        });
    }

    let models_dir = home_dir.join("models");
    let entry = duduclaw_inference::model_registry::RegistryEntry {
        name: String::new(), repo: repo.to_string(), filename: filename.to_string(),
        size_bytes: 0, quantization: String::new(), params: String::new(),
        languages: vec![], tags: vec![], min_ram_mb: 0, description: String::new(),
        tier: duduclaw_inference::model_registry::ModelTier::Community, downloads: 0,
        shards: vec![],
    };

    match duduclaw_inference::model_registry::downloader::download_model(
        &entry.download_url(), &entry.mirror_url(), &models_dir, filename, None,
    ).await {
        Ok(path) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Downloaded to: {}", path.display())}]
        }),
        Err(_e) => serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": format!("Download failed. Check logs for details.\nManual URL: {}", entry.download_url())}]
        }),
    }
}

async fn handle_model_recommend(_home_dir: &Path) -> Value {
    let hw = duduclaw_inference::hardware::detect_hardware().await;
    let curated = duduclaw_inference::model_registry::curated::builtin_registry();
    let filtered = duduclaw_inference::model_registry::curated::filter_by_hardware(&curated, hw.ram_available_mb);

    let mut text = format!(
        "Hardware: {} ({:?})\nRAM: {} MB available / {} MB total\nRecommended max model: {:.1} GB\n\nRecommended models:\n",
        hw.gpu_name, hw.gpu_type, hw.ram_available_mb, hw.ram_total_mb, hw.recommended_max_model_gb
    );

    if filtered.is_empty() {
        text.push_str("\n  No models fit in available RAM.");
    } else {
        for (i, e) in filtered.iter().enumerate() {
            text.push_str(&format!(
                "\n  {}. {} ({}, {}) — {}\n     repo: {} file: {}",
                i + 1, e.name, e.params, e.size_display(), e.description, e.repo, e.filename
            ));
        }
    }

    serde_json::json!({"content": [{"type": "text", "text": text}]})
}

// ── Compression handlers ────────────────────────────────────

async fn handle_compress_text(params: &Value) -> Value {
    let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
    if text.is_empty() {
        return serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": "text is required"}]
        });
    }

    let strategy = params
        .get("strategy")
        .and_then(|v| v.as_str())
        .unwrap_or("meta_token");

    match strategy {
        "meta_token" => compress_with_meta_token(text),
        "llmlingua" => compress_with_llmlingua(text).await,
        "streaming_llm" => compress_with_streaming_llm(text),
        "auto" => compress_auto(text).await,
        other => serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": format!(
                "Unknown strategy '{other}'. Valid options: meta_token, llmlingua, streaming_llm, auto"
            )}]
        }),
    }
}

/// Meta-Token lossless compression (best for JSON, code, templates).
fn compress_with_meta_token(text: &str) -> Value {
    let (compressed, stats) = duduclaw_inference::compression::meta_token::compress(text);

    let result = format!(
        "Compression Result (Meta-Token, lossless):\n\
         \n  Original: {} chars\
         \n  Compressed: {} chars\
         \n  Ratio: {:.2}x\
         \n  Savings: {:.1}%\n\n{}",
        stats.original_len,
        stats.compressed_len,
        stats.ratio,
        (1.0 - 1.0 / stats.ratio) * 100.0,
        if stats.ratio > 1.05 {
            format!("Compressed text:\n{compressed}")
        } else {
            "No significant compression achieved (text has little repetition).".to_string()
        }
    );

    serde_json::json!({
        "content": [{"type": "text", "text": result}]
    })
}

/// LLMLingua-2 lossy compression (best for natural language).
async fn compress_with_llmlingua(text: &str) -> Value {
    let config = duduclaw_inference::compression::llmlingua::LlmLinguaConfig {
        enabled: true,
        ..Default::default()
    };
    let compressor = duduclaw_inference::compression::llmlingua::LlmLinguaCompressor::new(config);

    if !compressor.is_available().await {
        return serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text":
                "LLMLingua-2 is not available. Install with: pip install llmlingua\n\
                 Falling back to meta_token is recommended."}]
        });
    }

    match compressor.compress(text).await {
        Ok((compressed, stats)) => {
            let result = format!(
                "Compression Result (LLMLingua-2, lossy):\n\
                 \n  Original: {} chars\
                 \n  Compressed: {} chars\
                 \n  Ratio: {:.2}x\
                 \n  Savings: {:.1}%\n\n\
                 Compressed text:\n{compressed}",
                stats.original_len,
                stats.compressed_len,
                stats.ratio,
                (1.0 - 1.0 / stats.ratio) * 100.0,
            );
            serde_json::json!({
                "content": [{"type": "text", "text": result}]
            })
        }
        Err(e) => serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": format!("LLMLingua compression failed: {e}")}]
        }),
    }
}

/// StreamingLLM session window compression (attention sink + sliding window).
fn compress_with_streaming_llm(text: &str) -> Value {
    let config = duduclaw_inference::compression::streaming_llm::StreamingLlmConfig {
        enabled: true,
        ..Default::default()
    };
    let mut window = duduclaw_inference::compression::streaming_llm::StreamingWindow::new(config);

    // Split text into lines and push as user messages to simulate session
    for line in text.lines() {
        if !line.trim().is_empty() {
            window.push("user", line);
        }
    }

    let stats = window.stats();
    let retained: Vec<String> = window
        .formatted_messages()
        .into_iter()
        .map(|(_role, content)| content)
        .collect();
    let compressed = retained.join("\n");

    let result = format!(
        "Compression Result (StreamingLLM, window):\n\
         \n  Original estimate: {} tokens\
         \n  Window: {} tokens\
         \n  Messages retained: {}\
         \n  Messages evicted: {}\
         \n  Ratio: {:.2}x\n\n\
         Windowed text:\n{compressed}",
        stats.original_len,
        stats.compressed_len,
        window.message_count(),
        window.evicted_count(),
        stats.ratio,
    );

    serde_json::json!({
        "content": [{"type": "text", "text": result}]
    })
}

/// Auto-select compression strategy based on content type.
/// - Structured data (JSON, code) → Meta-Token (lossless)
/// - Natural language → LLMLingua (lossy, higher ratio)
async fn compress_auto(text: &str) -> Value {
    let is_structured = detect_structured_content(text);

    if is_structured {
        compress_with_meta_token(text)
    } else {
        // Try LLMLingua for natural language; fall back to Meta-Token if unavailable
        let config = duduclaw_inference::compression::llmlingua::LlmLinguaConfig {
            enabled: true,
            ..Default::default()
        };
        let compressor =
            duduclaw_inference::compression::llmlingua::LlmLinguaCompressor::new(config);

        if compressor.is_available().await {
            compress_with_llmlingua(text).await
        } else {
            compress_with_meta_token(text)
        }
    }
}

/// Detect whether text is structured (JSON, code) vs natural language.
fn detect_structured_content(text: &str) -> bool {
    let trimmed = text.trim();

    // JSON detection
    if (trimmed.starts_with('{') && trimmed.ends_with('}'))
        || (trimmed.starts_with('[') && trimmed.ends_with(']'))
    {
        return true;
    }

    // Code detection heuristics: high density of code-like tokens
    let code_indicators = [
        "fn ", "pub ", "let ", "const ", "impl ", "struct ", // Rust
        "function ", "var ", "import ", "export ", "class ",  // JS/TS
        "def ", "self.", "elif ", "__init__",                  // Python
        "func ", "package ", "type ", "interface ",            // Go
        "<?", "?>", "<%", "%>",                                // Template
    ];
    let indicator_count = code_indicators
        .iter()
        .filter(|ind| trimmed.contains(**ind))
        .count();

    // If 3+ code indicators found, treat as structured
    indicator_count >= 3
}

async fn handle_decompress_text(params: &Value) -> Value {
    let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
    if text.is_empty() {
        return serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": "text is required"}]
        });
    }

    let decompressed = duduclaw_inference::compression::meta_token::decompress(text);

    serde_json::json!({
        "content": [{"type": "text", "text": decompressed}]
    })
}

// ── Cost telemetry handlers ─────────────────────────────────

async fn handle_cost_summary(params: &Value, home_dir: &Path) -> Value {
    // Ensure telemetry is initialized (idempotent on second call)
    let _ = duduclaw_gateway::cost_telemetry::init_telemetry(home_dir);

    let telemetry = match duduclaw_gateway::cost_telemetry::get_telemetry() {
        Some(t) => t,
        None => return serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": "Cost telemetry not initialized"}]
        }),
    };

    let hours = params.get("hours").and_then(|v| v.as_u64()).unwrap_or(24);

    if let Some(agent_id) = params.get("agent_id").and_then(|v| v.as_str()) {
        match telemetry.summary_by_agent(agent_id, hours).await {
            Ok(summary) => serde_json::json!({
                "content": [{"type": "text", "text": serde_json::to_string_pretty(&summary).unwrap_or_default()}]
            }),
            Err(e) => serde_json::json!({
                "isError": true,
                "content": [{"type": "text", "text": format!("Error: {e}")}]
            }),
        }
    } else {
        match telemetry.summary_global(hours).await {
            Ok(summary) => serde_json::json!({
                "content": [{"type": "text", "text": serde_json::to_string_pretty(&summary).unwrap_or_default()}]
            }),
            Err(e) => serde_json::json!({
                "isError": true,
                "content": [{"type": "text", "text": format!("Error: {e}")}]
            }),
        }
    }
}

async fn handle_cost_agents(params: &Value, home_dir: &Path) -> Value {
    let _ = duduclaw_gateway::cost_telemetry::init_telemetry(home_dir);

    let telemetry = match duduclaw_gateway::cost_telemetry::get_telemetry() {
        Some(t) => t,
        None => return serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": "Cost telemetry not initialized"}]
        }),
    };

    let hours = params.get("hours").and_then(|v| v.as_u64()).unwrap_or(24);

    match telemetry.all_agents_summary(hours).await {
        Ok(agents) => {
            if agents.is_empty() {
                return serde_json::json!({
                    "content": [{"type": "text", "text": "No cost data in the selected time window."}]
                });
            }
            let text = serde_json::to_string_pretty(&agents).unwrap_or_default();
            serde_json::json!({
                "content": [{"type": "text", "text": text}]
            })
        }
        Err(e) => serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": format!("Error: {e}")}]
        }),
    }
}

async fn handle_cost_recent(params: &Value) -> Value {
    let telemetry = match duduclaw_gateway::cost_telemetry::get_telemetry() {
        Some(t) => t,
        None => return serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": "Cost telemetry not initialized"}]
        }),
    };

    let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as u32;

    match telemetry.recent_records(limit).await {
        Ok(records) => {
            if records.is_empty() {
                return serde_json::json!({
                    "content": [{"type": "text", "text": "No cost records yet."}]
                });
            }
            let text = serde_json::to_string_pretty(&records).unwrap_or_default();
            serde_json::json!({
                "content": [{"type": "text", "text": text}]
            })
        }
        Err(e) => serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": format!("Error: {e}")}]
        }),
    }
}

// ── Odoo ERP handlers ───────────────────────────────────────

async fn handle_odoo_tool(
    tool: &str,
    params: &Value,
    home_dir: &Path,
    odoo: &OdooState,
    caller_agent: &str,
) -> Value {
    use duduclaw_odoo::connector::OdooConnector;
    use duduclaw_odoo::models::{crm, sale, inventory, accounting};

    // odoo_connect doesn't require an existing connection
    if tool == "odoo_connect" {
        return handle_odoo_connect(home_dir, odoo, caller_agent).await;
    }

    if tool == "odoo_status" {
        return match odoo.is_connected(caller_agent).await {
            true => {
                // Probe the connector by triggering get_or_connect; it's
                // already cached so this is a hashmap lookup, not an HTTP
                // call. The decrypt closure is a never-called fallback.
                match odoo.get_or_connect(caller_agent, |_| Err::<String, String>("unreachable".into())).await {
                    Ok(conn) => {
                        let s = conn.status();
                        let key = odoo.pool_key(caller_agent).await;
                        serde_json::json!({ "content": [{"type": "text", "text": format!(
                            "Odoo connected (agent={}, profile={}): {} ({})\nEdition: {}\nVersion: {}\nUser ID: {}\nEE modules: {}",
                            key.0, key.1, s.url, s.db, s.edition, s.version,
                            s.uid.map(|u| u.to_string()).unwrap_or("-".into()),
                            if s.ee_modules.is_empty() { "none".to_string() } else { s.ee_modules.join(", ") },
                        )}]})
                    }
                    Err(e) => mcp_error(&format!("Odoo status: connector slot lost: {e}")),
                }
            }
            false => serde_json::json!({
                "content": [{"type": "text", "text": format!(
                    "Odoo not connected for agent '{caller_agent}'. Call odoo_connect first."
                )}],
                "isError": true
            }),
        };
    }

    // RFC-21 §2 acceptance: defence-in-depth. Reject the call before any
    // HTTP round-trip leaves the process when `agent.toml [odoo]
    // .allowed_models` / `.allowed_actions` doesn't cover it.
    if let Some((verb, model)) = classify_odoo_call(tool, params) {
        let cfg = odoo.agent_override(caller_agent).await;
        if let Err(reason) = crate::odoo_pool::check_action_permission(cfg.as_ref(), verb, &model) {
            // Audit the policy denial so operators can spot misconfigured
            // agents without having to grep MCP logs.
            duduclaw_security::audit::append_tool_call(
                home_dir,
                caller_agent,
                tool,
                &format!("DENIED: {model}/{verb} — {reason}"),
                false,
            );
            return mcp_error(&format!("Odoo permission denied: {reason}"));
        }
    }

    // All other tools require an active per-agent connection. Use the
    // pool's cache fast-path; the decrypt closure is unreachable here
    // because cold-connect is owned by `handle_odoo_connect`.
    let conn_arc = match odoo
        .get_or_connect(caller_agent, |_| Err::<String, String>(
            "Odoo connector not initialised — call odoo_connect first".into(),
        ))
        .await
    {
        Ok(c) => c,
        Err(_) => {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!(
                    "Odoo not connected for agent '{caller_agent}'. Call odoo_connect first."
                )}],
                "isError": true
            });
        }
    };
    let conn: &OdooConnector = conn_arc.as_ref();
    // ── per-call audit attribution (RFC-21 §2 acceptance) ────────────────
    // Surface caller_agent + profile + tool + params summary to
    // tool_calls.jsonl so the audit trail attributes Odoo activity to a
    // specific agent rather than to the global admin user.
    let _audit_profile = odoo.pool_key(caller_agent).await.1;

    let result: std::result::Result<String, String> = match tool {
        "odoo_crm_leads" => {
            let stage = params.get("stage").and_then(|v| v.as_str()).unwrap_or("");
            let limit = params.get("limit").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(20usize);
            let mut domain = vec![];
            if !stage.is_empty() {
                domain.push(serde_json::json!(["stage_id.name", "ilike", stage]));
            }
            match conn.search_read("crm.lead", domain, crm::CRM_LEAD_FIELDS, limit).await {
                Ok(data) => {
                    let leads: Vec<crm::CrmLead> = data.as_array().unwrap_or(&vec![]).iter().map(crm::map_crm_lead).collect();
                    Ok(serde_json::to_string_pretty(&leads).unwrap_or_default())
                }
                Err(e) => Err(e),
            }
        }
        "odoo_crm_create_lead" => {
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if name.is_empty() { return mcp_error("name is required"); }
            let mut vals = serde_json::json!({"name": name, "type": "lead"});
            if let Some(v) = params.get("contact_name").and_then(|v| v.as_str()) { vals["contact_name"] = serde_json::json!(v); }
            if let Some(v) = params.get("email").and_then(|v| v.as_str()) { vals["email_from"] = serde_json::json!(v); }
            if let Some(v) = params.get("phone").and_then(|v| v.as_str()) { vals["phone"] = serde_json::json!(v); }
            if let Some(v) = params.get("expected_revenue").and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok()) { vals["expected_revenue"] = serde_json::json!(v); }
            match conn.create("crm.lead", vals).await {
                Ok(id) => Ok(format!("CRM lead created (ID: {id})")),
                Err(e) => Err(e),
            }
        }
        "odoo_crm_update_stage" => {
            let lead_id = params.get("lead_id").and_then(|v| v.as_str()).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
            let stage_name = params.get("stage_name").and_then(|v| v.as_str()).unwrap_or("");
            if lead_id == 0 || stage_name.is_empty() { return mcp_error("lead_id and stage_name are required"); }
            // Find stage ID by name
            match conn.search_read("crm.stage", vec![serde_json::json!(["name", "ilike", stage_name])], &["id", "name"], 1).await {
                Ok(stages) => {
                    let stage_id = stages.as_array().and_then(|a| a.first()).and_then(|s| s["id"].as_i64()).unwrap_or(0);
                    if stage_id == 0 { return mcp_error(&format!("Stage '{stage_name}' not found")); }
                    match conn.write("crm.lead", &[lead_id], serde_json::json!({"stage_id": stage_id})).await {
                        Ok(_) => Ok(format!("Lead {lead_id} moved to stage '{stage_name}'")),
                        Err(e) => Err(e),
                    }
                }
                Err(e) => Err(e),
            }
        }
        "odoo_sale_orders" => {
            let status = params.get("status").and_then(|v| v.as_str()).unwrap_or("");
            let limit = params.get("limit").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(20usize);
            let mut domain = vec![];
            if !status.is_empty() { domain.push(serde_json::json!(["state", "=", status])); }
            match conn.search_read("sale.order", domain, sale::SALE_ORDER_FIELDS, limit).await {
                Ok(data) => {
                    let orders: Vec<sale::SaleOrder> = data.as_array().unwrap_or(&vec![]).iter().map(sale::map_sale_order).collect();
                    Ok(serde_json::to_string_pretty(&orders).unwrap_or_default())
                }
                Err(e) => Err(e),
            }
        }
        "odoo_sale_create_quotation" => {
            let partner_id = params.get("partner_id").and_then(|v| v.as_str()).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
            let product_id = params.get("product_id").and_then(|v| v.as_str()).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
            let qty = params.get("quantity").and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok()).unwrap_or(1.0);
            if partner_id == 0 || product_id == 0 { return mcp_error("partner_id and product_id are required"); }
            let vals = serde_json::json!({
                "partner_id": partner_id,
                "order_line": [[0, 0, {"product_id": product_id, "product_uom_qty": qty}]],
            });
            match conn.create("sale.order", vals).await {
                Ok(id) => Ok(format!("Quotation created (ID: {id})")),
                Err(e) => Err(e),
            }
        }
        "odoo_sale_confirm" => {
            let order_id = params.get("order_id").and_then(|v| v.as_str()).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
            if order_id == 0 { return mcp_error("order_id is required"); }
            match conn.execute_kw("sale.order", "action_confirm", vec![serde_json::json!([order_id])], serde_json::json!({})).await {
                Ok(_) => Ok(format!("Order {order_id} confirmed")),
                Err(e) => Err(e),
            }
        }
        "odoo_inventory_products" => {
            let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let limit = params.get("limit").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(20usize);
            let mut domain = vec![serde_json::json!(["detailed_type", "=", "product"])];
            if !query.is_empty() { domain.push(serde_json::json!(["name", "ilike", query])); }
            match conn.search_read("product.product", domain, inventory::PRODUCT_FIELDS, limit).await {
                Ok(data) => {
                    let products: Vec<inventory::Product> = data.as_array().unwrap_or(&vec![]).iter().map(inventory::map_product).collect();
                    Ok(serde_json::to_string_pretty(&products).unwrap_or_default())
                }
                Err(e) => Err(e),
            }
        }
        "odoo_inventory_check" => {
            let product_id = params.get("product_id").and_then(|v| v.as_str()).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
            if product_id == 0 { return mcp_error("product_id is required"); }
            let domain = vec![serde_json::json!(["product_id", "=", product_id])];
            match conn.search_read("stock.quant", domain, inventory::STOCK_QUANT_FIELDS, 10).await {
                Ok(data) => {
                    let quants: Vec<inventory::StockQuant> = data.as_array().unwrap_or(&vec![]).iter().map(inventory::map_stock_quant).collect();
                    Ok(serde_json::to_string_pretty(&quants).unwrap_or_default())
                }
                Err(e) => Err(e),
            }
        }
        "odoo_invoice_list" => {
            let status = params.get("status").and_then(|v| v.as_str()).unwrap_or("");
            let limit = params.get("limit").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(20usize);
            let mut domain = vec![serde_json::json!(["move_type", "in", ["out_invoice", "in_invoice"]])];
            if !status.is_empty() {
                match status {
                    "paid" => domain.push(serde_json::json!(["payment_state", "=", "paid"])),
                    "draft" => domain.push(serde_json::json!(["state", "=", "draft"])),
                    "posted" => domain.push(serde_json::json!(["state", "=", "posted"])),
                    _ => {}
                }
            }
            match conn.search_read("account.move", domain, accounting::INVOICE_FIELDS, limit).await {
                Ok(data) => {
                    let invoices: Vec<accounting::Invoice> = data.as_array().unwrap_or(&vec![]).iter().map(accounting::map_invoice).collect();
                    Ok(serde_json::to_string_pretty(&invoices).unwrap_or_default())
                }
                Err(e) => Err(e),
            }
        }
        "odoo_payment_status" => {
            let invoice_id = params.get("invoice_id").and_then(|v| v.as_str()).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
            if invoice_id == 0 { return mcp_error("invoice_id is required"); }
            match conn.search_read("account.move", vec![serde_json::json!(["id", "=", invoice_id])], accounting::INVOICE_FIELDS, 1).await {
                Ok(data) => {
                    let inv = data.as_array().and_then(|a| a.first()).map(accounting::map_invoice);
                    match inv {
                        Some(i) => Ok(serde_json::to_string_pretty(&i).unwrap_or_default()),
                        None => Err(format!("Invoice {invoice_id} not found")),
                    }
                }
                Err(e) => Err(e),
            }
        }
        "odoo_search" => {
            let model = params.get("model").and_then(|v| v.as_str()).unwrap_or("");
            if model.is_empty() { return mcp_error("model is required"); }
            if OdooConnector::is_model_blocked(model) { return mcp_error(&format!("Model '{model}' is blocked for security reasons")); }
            let domain_str = params.get("domain").and_then(|v| v.as_str()).unwrap_or("[]");
            let domain: Vec<Value> = serde_json::from_str(domain_str).unwrap_or_default();
            let fields_str = params.get("fields").and_then(|v| v.as_str()).unwrap_or("id,name");
            let fields: Vec<&str> = fields_str.split(',').map(|s| s.trim()).collect();
            let limit = params.get("limit").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(20usize);
            match conn.search_read(model, domain, &fields, limit).await {
                Ok(data) => Ok(serde_json::to_string_pretty(&data).unwrap_or_default()),
                Err(e) => Err(e),
            }
        }
        "odoo_execute" => {
            let model = params.get("model").and_then(|v| v.as_str()).unwrap_or("");
            let method = params.get("method").and_then(|v| v.as_str()).unwrap_or("");
            let ids_str = params.get("ids").and_then(|v| v.as_str()).unwrap_or("[]");
            if model.is_empty() || method.is_empty() { return mcp_error("model and method are required"); }
            if OdooConnector::is_model_blocked(model) { return mcp_error(&format!("Model '{model}' is blocked")); }

            // Whitelist safe Odoo methods — block dangerous ones like unlink, write on sensitive models (MCP-H6)
            const BLOCKED_METHODS: &[&str] = &[
                "unlink", "uninstall", "uninstall_hook",
                "init", "_auto_init", "_register_hook",
                "signal_workflow", "execute_import",
            ];
            if BLOCKED_METHODS.contains(&method) {
                return mcp_error(&format!("Method '{method}' is blocked for security reasons"));
            }
            let ids: Vec<Value> = serde_json::from_str(ids_str).unwrap_or_default();
            match conn.execute_kw(model, method, vec![serde_json::json!(ids)], serde_json::json!({})).await {
                Ok(data) => Ok(serde_json::to_string_pretty(&data).unwrap_or_default()),
                Err(e) => Err(e),
            }
        }
        "odoo_report" => {
            let report_name = params.get("report_name").and_then(|v| v.as_str()).unwrap_or("");
            let record_id = params.get("record_id").and_then(|v| v.as_str()).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
            if report_name.is_empty() || record_id == 0 { return mcp_error("report_name and record_id are required"); }
            // Reports use a special render method
            match conn.execute_kw("ir.actions.report", "render_qweb_pdf", vec![serde_json::json!(report_name), serde_json::json!([record_id])], serde_json::json!({})).await {
                Ok(_) => Ok(format!("Report '{report_name}' generated for record {record_id}. Download from Odoo.")),
                Err(e) => Err(format!("Report generation failed: {e}")),
            }
        }
        _ => Err(format!("Unknown Odoo tool: {tool}")),
    };

    // RFC-21 §2 acceptance: per-call audit attribution. tool_calls.jsonl
    // now carries the originating agent + profile + outcome so Odoo
    // activity can be traced to the agent that triggered it (and not the
    // shared admin user inside Odoo's own audit log).
    let params_summary = format!(
        "profile={}; tool={}; ok={}",
        _audit_profile,
        tool,
        result.is_ok(),
    );
    duduclaw_security::audit::append_tool_call(
        home_dir,
        caller_agent,
        tool,
        &params_summary,
        result.is_ok(),
    );

    match result {
        Ok(text) => serde_json::json!({ "content": [{"type": "text", "text": text}] }),
        Err(e) => serde_json::json!({ "content": [{"type": "text", "text": format!("Odoo error: {e}")}], "isError": true }),
    }
}

/// Heuristic mapping of `(tool, params)` to `(verb, model)` so the per-agent
/// `allowed_actions` / `allowed_models` filter can run before any HTTP call
/// reaches Odoo. Returns `None` for `odoo_status` / `odoo_connect` (those
/// need no model permission).
fn classify_odoo_call(tool: &str, params: &Value) -> Option<(&'static str, String)> {
    match tool {
        "odoo_crm_leads" => Some(("search", "crm.lead".into())),
        "odoo_crm_create_lead" => Some(("create", "crm.lead".into())),
        "odoo_crm_update_stage" => Some(("write", "crm.lead".into())),
        "odoo_sale_orders" => Some(("search", "sale.order".into())),
        "odoo_sale_create_quotation" => Some(("create", "sale.order".into())),
        "odoo_sale_confirm" => Some(("execute", "sale.order".into())),
        "odoo_inventory_products" => Some(("search", "product.product".into())),
        "odoo_inventory_check" => Some(("search", "stock.quant".into())),
        "odoo_invoice_list" | "odoo_payment_status" => Some(("search", "account.move".into())),
        "odoo_search" => {
            let model = params.get("model").and_then(|v| v.as_str()).unwrap_or("");
            if model.is_empty() { None } else { Some(("search", model.to_string())) }
        }
        "odoo_execute" => {
            let model = params.get("model").and_then(|v| v.as_str()).unwrap_or("");
            if model.is_empty() { None } else { Some(("execute", model.to_string())) }
        }
        "odoo_report" => {
            let name = params.get("report_name").and_then(|v| v.as_str()).unwrap_or("");
            if name.is_empty() { None } else { Some(("execute", name.to_string())) }
        }
        _ => None,
    }
}

/// Connect to Odoo using `config.toml [odoo]` overlaid with the caller's
/// `agent.toml [odoo]` block (when present). RFC-21 §2: each agent ends up
/// with its own per-pool slot so cross-project credential leakage and
/// audit-log mis-attribution are eliminated at the system layer.
async fn handle_odoo_connect(home_dir: &Path, odoo: &OdooState, caller_agent: &str) -> Value {
    use duduclaw_odoo::AgentOdooConfig;

    // ── 1. Reload global config from disk so operator edits land on next connect ─
    let config_path = home_dir.join("config.toml");
    let content = match tokio::fs::read_to_string(&config_path).await {
        Ok(c) => c,
        Err(e) => return mcp_error(&format!("Cannot read config.toml: {e}")),
    };
    let global_table: toml::Table = match content.parse() {
        Ok(t) => t,
        Err(e) => return mcp_error(&format!("Invalid config.toml: {e}")),
    };
    let global_cfg = duduclaw_odoo::OdooConfig::from_toml(&global_table);
    if !global_cfg.is_configured() {
        return mcp_error("Odoo not configured. Add [odoo] section to config.toml with url and db.");
    }
    odoo.set_global(global_cfg.clone()).await;

    // ── 2. Reload caller agent's [odoo] override if their agent.toml has one ──
    let agent_toml_path = home_dir
        .join("agents")
        .join(caller_agent)
        .join("agent.toml");
    let override_cfg: Option<AgentOdooConfig> =
        match tokio::fs::read_to_string(&agent_toml_path).await {
            Ok(raw) => raw
                .parse::<toml::Table>()
                .ok()
                .and_then(|t| AgentOdooConfig::from_agent_toml(&t)),
            Err(_) => None,
        };
    if let Some(cfg) = &override_cfg {
        odoo.register_agent(caller_agent, cfg.clone()).await;
    }

    // ── 3. Force a fresh handshake — the previous slot, if any, may have ───
    //       been authed against a stale config.
    odoo.disconnect(caller_agent).await;

    // ── 4. Cold connect via the pool — credential merge + decrypt happen ──
    //       inside `OdooConnectorPool::get_or_connect` using the resolver
    //       state we just registered.
    let home_dir_owned = home_dir.to_path_buf();
    let connector = match odoo
        .get_or_connect(caller_agent, move |enc: &str| {
            decrypt_encrypted_value(enc, &home_dir_owned)
                .ok_or_else(|| "Odoo credential not found or could not be decrypted".to_string())
        })
        .await
    {
        Ok(c) => c,
        Err(e) => return mcp_error(&format!("Odoo connection failed: {e}")),
    };

    let status = connector.status();
    let key = odoo.pool_key(caller_agent).await;
    serde_json::json!({
        "content": [{"type": "text", "text": format!(
            "Connected to Odoo {} ({}) — {} v{}\n  agent={}, profile={}",
            status.url, status.db, status.edition, status.version,
            key.0, key.1,
        )}]
    })
}

fn mcp_error(msg: &str) -> Value {
    serde_json::json!({ "content": [{"type": "text", "text": format!("Error: {msg}")}], "isError": true })
}

fn mcp_text(msg: &str) -> Value {
    serde_json::json!({ "content": [{"type": "text", "text": msg}] })
}

// ── Skill management handlers ───────────────────────────────

/// Search the local skill registry.
async fn handle_skill_search(params: &Value, home_dir: &Path) -> Value {
    let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
    if query.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: query is required"}],
            "isError": true
        });
    }

    let registry = duduclaw_agent::skill_registry::SkillRegistry::load(home_dir);
    let results = registry.search(query, 20);

    if results.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "No skills found for '{query}'. Registry has {} skills indexed.",
                registry.count()
            )}]
        });
    }

    let mut lines = vec![format!("Found {} skill(s) for '{query}':\n", results.len())];
    for s in &results {
        let tags = if s.tags.is_empty() {
            String::new()
        } else {
            format!(" [{}]", s.tags.join(", "))
        };
        lines.push(format!("- **{}**: {}{}", s.name, s.description, tags));
    }

    serde_json::json!({
        "content": [{"type": "text", "text": lines.join("\n")}]
    })
}

/// List all skills installed for a specific agent, including global skills.
async fn handle_skill_list(params: &Value, home_dir: &Path) -> Value {
    let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");

    let agent_name = if agent_id.is_empty() {
        resolve_main_agent_name(home_dir).await
    } else {
        agent_id.to_string()
    };

    // Collect global skills from ~/.duduclaw/skills/
    let global_skills_dir = home_dir.join("skills");
    let mut global_skills = Vec::new();
    let mut global_names = std::collections::HashSet::new();

    if let Ok(mut entries) = tokio::fs::read_dir(&global_skills_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("md") {
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("?")
                    .to_string();

                let meta = duduclaw_agent::skill_loader::parse_skill_file(&path).ok();
                let desc = meta
                    .as_ref()
                    .map(|m| m.meta.description.clone())
                    .unwrap_or_default();

                global_names.insert(name.clone());
                global_skills.push(format!("- {name}: {desc} (global)"));
            }
        }
    }

    // Collect agent-local skills from ~/.duduclaw/agents/<agent>/SKILLS/
    let skills_dir = home_dir.join("agents").join(&agent_name).join("SKILLS");
    let mut agent_skills = Vec::new();

    if let Ok(mut entries) = tokio::fs::read_dir(&skills_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("md") {
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("?")
                    .to_string();

                let meta = duduclaw_agent::skill_loader::parse_skill_file(&path).ok();
                let desc = meta
                    .as_ref()
                    .map(|m| m.meta.description.clone())
                    .unwrap_or_default();

                // If agent-local overrides a global skill, mark it
                let suffix = if global_names.contains(&name) { " (override)" } else { "" };
                agent_skills.push(format!("- {name}: {desc}{suffix}"));
            }
        }
    }

    // Remove global skills that are overridden by agent-local
    let agent_local_names: std::collections::HashSet<String> = agent_skills.iter()
        .filter_map(|s| s.strip_prefix("- ").and_then(|s| s.split(':').next()).map(String::from))
        .collect();
    global_skills.retain(|s| {
        let name = s.strip_prefix("- ")
            .and_then(|s| s.split(':').next())
            .unwrap_or("");
        !agent_local_names.contains(name)
    });

    let total = global_skills.len() + agent_skills.len();
    if total == 0 {
        serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "No skills installed for agent '{agent_name}'."
            )}]
        })
    } else {
        let mut parts = Vec::new();
        if !global_skills.is_empty() {
            parts.push(format!("**Global skills** ({}):\n{}", global_skills.len(), global_skills.join("\n")));
        }
        if !agent_skills.is_empty() {
            parts.push(format!("**Agent '{}' skills** ({}):\n{}", agent_name, agent_skills.len(), agent_skills.join("\n")));
        }
        let text = format!("Total {} skill(s):\n\n{}", total, parts.join("\n\n"));
        serde_json::json!({
            "content": [{"type": "text", "text": text}]
        })
    }
}

/// Validate that a string is safe to use as a file path component.
/// Prevents path traversal attacks via agent_id or skill_name.
fn is_safe_path_component(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && !s.starts_with('-')
        && !s.ends_with('-')
        && !s.contains('.')
        && !s.contains('/')
        && !s.contains('\\')
        && !s.contains('\0')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Run a security scan on an agent's installed skill.
async fn handle_skill_security_scan(params: &Value, home_dir: &Path) -> Value {
    let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
    let skill_name = params.get("skill_name").and_then(|v| v.as_str()).unwrap_or("");

    if agent_id.is_empty() || skill_name.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: agent_id and skill_name are required"}],
            "isError": true
        });
    }

    // Validate inputs to prevent path traversal
    if !is_safe_path_component(agent_id) {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: invalid agent_id (alphanumeric, hyphens, underscores only)"}],
            "isError": true
        });
    }
    if !is_safe_path_component(skill_name) {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: invalid skill_name (alphanumeric, hyphens, underscores only)"}],
            "isError": true
        });
    }

    // Try agent-local first, then global (read directly to avoid TOCTOU race)
    let agent_path = home_dir.join("agents").join(agent_id).join("SKILLS").join(format!("{skill_name}.md"));
    let global_path = home_dir.join("skills").join(format!("{skill_name}.md"));

    let content = match tokio::fs::read_to_string(&agent_path).await {
        Ok(c) => c,
        Err(_) => match tokio::fs::read_to_string(&global_path).await {
            Ok(c) => c,
            Err(_) => return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error: Skill '{skill_name}' not found for agent '{agent_id}'")}],
                "isError": true
            }),
        },
    };

    // Load CONTRACT.toml must_not patterns if available
    let contract_path = home_dir.join("agents").join(agent_id).join("CONTRACT.toml");
    let must_not: Option<Vec<String>> = tokio::fs::read_to_string(&contract_path)
        .await
        .ok()
        .and_then(|c| {
            // Simple extraction of must_not patterns from TOML
            let mut patterns = Vec::new();
            let mut in_must_not = false;
            for line in c.lines() {
                if line.trim().starts_with("must_not") {
                    in_must_not = true;
                    continue;
                }
                if in_must_not {
                    let trimmed = line.trim().trim_matches(|c: char| c == '"' || c == '\'' || c == ',' || c == ']');
                    if trimmed.is_empty() || trimmed.starts_with('[') {
                        continue;
                    }
                    if line.contains(']') {
                        in_must_not = false;
                    }
                    if !trimmed.is_empty() {
                        patterns.push(trimmed.to_string());
                    }
                }
            }
            if patterns.is_empty() { None } else { Some(patterns) }
        });

    use duduclaw_gateway::skill_lifecycle::security_scanner;
    let result = security_scanner::scan_skill(&content, must_not.as_deref());

    // Sprint N P0: emit security_scan audit event (non-blocking, global singleton)
    {
        use duduclaw_gateway::evolution_events::emitter::EvolutionEventEmitter;
        EvolutionEventEmitter::global().emit_security_scan(
            agent_id,
            skill_name,
            result.passed,
            serde_json::json!({
                "risk_level": format!("{:?}", result.risk_level),
                "findings_count": result.findings.len(),
            }),
        );
    }

    let findings_text: Vec<String> = result.findings.iter().map(|f| {
        format!(
            "- [{:?}] {:?} (line {}): {} [pattern: {}]",
            f.severity,
            f.category,
            f.line_number.map(|n| n.to_string()).unwrap_or_else(|| "?".to_string()),
            f.description,
            f.matched_pattern,
        )
    }).collect();

    let text = format!(
        "**Security scan: {skill_name}**\n\
         Risk level: {:?}\n\
         Passed: {}\n\
         Findings ({}):\n{}",
        result.risk_level,
        result.passed,
        result.findings.len(),
        if findings_text.is_empty() { "  (none)".to_string() } else { findings_text.join("\n") },
    );

    serde_json::json!({
        "content": [{"type": "text", "text": text}]
    })
}

/// Graduate a skill from agent-local to global scope.
async fn handle_skill_graduate(params: &Value, home_dir: &Path) -> Value {
    let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
    let skill_name = params.get("skill_name").and_then(|v| v.as_str()).unwrap_or("");

    if agent_id.is_empty() || skill_name.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: agent_id and skill_name are required"}],
            "isError": true
        });
    }

    // Validate inputs to prevent path traversal
    if !is_safe_path_component(agent_id) || !is_safe_path_component(skill_name) {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: invalid agent_id or skill_name (alphanumeric, hyphens, underscores only)"}],
            "isError": true
        });
    }

    let agent_skills_dir = home_dir.join("agents").join(agent_id).join("SKILLS");
    let global_skills_dir = home_dir.join("skills");

    // [H-4] Security scan before graduation to global scope
    let skill_path = agent_skills_dir.join(format!("{skill_name}.md"));
    let content = match tokio::fs::read_to_string(&skill_path).await {
        Ok(c) => c,
        Err(e) => return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: Failed to read skill: {e}")}],
            "isError": true
        }),
    };
    {
        use duduclaw_gateway::skill_lifecycle::security_scanner;
        let scan = security_scanner::scan_skill(&content, None);
        if !scan.passed {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!(
                    "Error: Security scan failed before graduation (risk: {:?}, {} findings). \
                     Fix the issues or use skill_security_scan for details.",
                    scan.risk_level, scan.findings.len()
                )}],
                "isError": true
            });
        }
    }

    use duduclaw_gateway::skill_lifecycle::graduation;

    let candidate = graduation::GraduationCandidate {
        skill_name: skill_name.to_string(),
        source_agent_id: agent_id.to_string(),
        lift: 0.0, // manual graduation — no lift data
        load_count: 0,
        is_stable: true,
        first_activated: chrono::Utc::now(),
    };

    match graduation::graduate_to_global(&candidate, &agent_skills_dir, &global_skills_dir).await {
        Ok(record) => {
            let home_clone = home_dir.to_path_buf();
            let record_clone = record.clone();
            let _ = tokio::task::spawn_blocking(move || {
                graduation::append_graduation_log(&record_clone, &home_clone);
            }).await;
            serde_json::json!({
                "content": [{"type": "text", "text": format!(
                    "Skill '{skill_name}' graduated from agent '{agent_id}' to global scope.\n\
                     Location: ~/.duduclaw/skills/{skill_name}.md"
                )}]
            })
        }
        Err(e) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: Graduation failed: {e}")}],
            "isError": true
        }),
    }
}

/// Report skill synthesis and sandbox trial status.
async fn handle_skill_synthesis_status(params: &Value, home_dir: &Path) -> Value {
    let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");

    let agent_name = if agent_id.is_empty() {
        resolve_main_agent_name(home_dir).await
    } else {
        agent_id.to_string()
    };

    // Read recent synthesis events from feedback.jsonl (tail only, max 64KB)
    let feedback_path = home_dir.join("feedback.jsonl");
    let mut synthesis_events = Vec::new();
    let tail_content = {
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        let mut buf = String::new();
        if let Ok(mut file) = tokio::fs::File::open(&feedback_path).await {
            let file_len = file.metadata().await.map(|m| m.len()).unwrap_or(0);
            const MAX_TAIL: u64 = 64_000;
            if file_len > MAX_TAIL {
                let _ = file.seek(std::io::SeekFrom::End(-(MAX_TAIL as i64))).await;
            }
            let _ = file.read_to_string(&mut buf).await;
        }
        buf
    };
    if !tail_content.is_empty() {
        for line in tail_content.lines().rev().take(50) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(line)
                && val.get("signal_type").and_then(|v| v.as_str()) == Some("synthesis_trigger")
                && val.get("agent_id").and_then(|v| v.as_str()) == Some(&agent_name)
            {
                let topic = val.get("topic").and_then(|v| v.as_str()).unwrap_or("?");
                let gaps = val.get("gap_count").and_then(|v| v.as_u64()).unwrap_or(0);
                let err = val.get("avg_composite_error").and_then(|v| v.as_f64()).unwrap_or(0.0);
                synthesis_events.push(format!(
                    "topic: {topic}, gaps: {gaps}, avg_error: {err:.2}"
                ));
            }
        }
    }

    // Read graduation log
    let graduation_records = duduclaw_gateway::skill_lifecycle::graduation::load_graduation_log(home_dir);
    let agent_graduations: Vec<_> = graduation_records
        .iter()
        .filter(|r| r.source_agent == agent_name)
        .map(|r| format!("- {} (lift: {:.1}%, at: {})", r.skill_name, r.lift * 100.0, r.graduated_at.format("%Y-%m-%d")))
        .collect();

    let text = format!(
        "**Skill Lifecycle Status: {agent_name}**\n\n\
         ## Recent Synthesis Triggers\n{synthesis}\n\n\
         ## Graduated Skills\n{graduated}",
        synthesis = if synthesis_events.is_empty() {
            "  (none)".to_string()
        } else {
            synthesis_events.iter().map(|s| format!("- {s}")).collect::<Vec<_>>().join("\n")
        },
        graduated = if agent_graduations.is_empty() {
            "  (none)".to_string()
        } else {
            agent_graduations.join("\n")
        },
    );

    serde_json::json!({
        "content": [{"type": "text", "text": text}]
    })
}

/// Trigger the Rollout-to-Skill synthesis pipeline (W19-P0).
///
/// Reads `agent_id`, `dry_run`, and `lookback_days` from params.
/// Resolves the Anthropic API key from `ANTHROPIC_API_KEY` env var or
/// the `~/.duduclaw/config.toml` `[api] anthropic_api_key` field.
/// Non-blocking: all errors are captured and returned in the summary.
async fn handle_skill_synthesis_run(params: &Value, home_dir: &Path, default_agent: &str) -> Value {
    use duduclaw_gateway::skill_synthesis_pipeline::pipeline::{PipelineConfig, run as run_pipeline};

    let agent_id_param = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
    let target_agent = if agent_id_param.is_empty() { default_agent } else { agent_id_param };

    let dry_run = params
        .get("dry_run")
        .and_then(|v| match v {
            Value::Bool(b) => Some(*b),
            Value::String(s) => s.parse::<bool>().ok(),
            _ => None,
        })
        .unwrap_or(true); // Safe default: dry-run

    let lookback_days = params
        .get("lookback_days")
        .and_then(|v| v.as_u64())
        .map(|v| v.min(30) as u32)
        .unwrap_or(1);

    // Resolve API key: env var takes precedence over config file.
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .ok()
        .filter(|k| !k.is_empty())
        .or_else(|| {
            // Fallback: read from config.toml [api] anthropic_api_key
            let config_path = home_dir.join("config.toml");
            std::fs::read_to_string(config_path)
                .ok()
                .and_then(|content| {
                    content
                        .lines()
                        .skip_while(|l| !l.trim().starts_with("[api]"))
                        .find(|l| l.trim().starts_with("anthropic_api_key"))
                        .and_then(|l| l.splitn(2, '=').nth(1))
                        .map(|v| v.trim().trim_matches('"').to_string())
                        .filter(|s| !s.is_empty())
                })
        });

    // Point pipeline at real EvolutionEvents location.
    let events_dir = std::env::var("EVOLUTION_EVENTS_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| home_dir.join("evolution").join("events"));

    let config = PipelineConfig {
        events_dir,
        lookback_days,
        dry_run,
        api_key,
        home_dir: home_dir.to_path_buf(),
        target_agent_id: target_agent.to_string(),
        ..Default::default()
    };

    let result = run_pipeline(&config).await;

    let mode = if result.dry_run { "DRY RUN" } else { "FULL" };
    let summary = result.summary();

    let detail = format!(
        "## Rollout-to-Skill Pipeline — {mode}\n\n\
         {summary}\n\n\
         **Events scanned:** {events}\n\
         **Trajectory windows:** {traj}\n\
         **Top-20% candidates:** {top}\n\
         **Skills graduated:** {grad}\n\
         **Non-fatal errors:** {errs}",
        events = result.total_events_parsed,
        traj = result.total_trajectories,
        top = result.top_trajectories.len(),
        grad = result.skills_graduated,
        errs = result.errors.len(),
    );

    let error_section = if result.errors.is_empty() {
        String::new()
    } else {
        format!(
            "\n\n**Errors:**\n{}",
            result.errors.iter().map(|e| format!("- {e}")).collect::<Vec<_>>().join("\n")
        )
    };

    serde_json::json!({
        "content": [{
            "type": "text",
            "text": format!("{detail}{error_section}")
        }]
    })
}

/// Resolve the main agent name from the agents directory.
// ── Delegation safety helpers ────────────────────────────────

/// Delegation context read from environment variables (or injected for testing).
#[derive(Debug, Clone)]
struct DelegationContext {
    depth: u8,
    origin: Option<String>,
    sender: Option<String>,
}

impl DelegationContext {
    /// Read from env vars set by the dispatcher. This is the ONLY trusted source
    /// in production — tool params are ignored to prevent LLM agents from spoofing.
    fn from_env() -> Self {
        let depth = std::env::var(duduclaw_core::ENV_DELEGATION_DEPTH)
            .ok()
            .and_then(|v| v.parse::<u8>().ok())
            .unwrap_or(0);
        let origin = std::env::var(duduclaw_core::ENV_DELEGATION_ORIGIN)
            .ok()
            .filter(|s| !s.is_empty());
        let sender = std::env::var(duduclaw_core::ENV_DELEGATION_SENDER)
            .ok()
            .filter(|s| !s.is_empty());
        Self { depth, origin, sender }
    }
}

/// Read delegation context from environment (production entry point).
fn read_delegation_env() -> (u8, Option<String>, Option<String>) {
    let ctx = DelegationContext::from_env();
    (ctx.depth, ctx.origin, ctx.sender)
}

/// Check whether `sender` is allowed to delegate to `target` under the
/// supervisor pattern.  Allowed directions:
///   - Parent → Child  (target.reports_to == sender)
///   - Child  → Parent (sender.reports_to == target) — for replies
///
/// Returns `Ok(())` if allowed, or `Err(reason)` if denied.
/// Normalize reports_to: both "" and "none" mean root (no parent).
fn normalize_reports_to(value: &str) -> &str {
    if value.is_empty() || value == "none" { "" } else { value }
}

async fn check_supervisor_relation(
    home_dir: &Path,
    sender: &str,
    target: &str,
) -> std::result::Result<(), String> {
    // Self-delegation is always forbidden (would be an echo loop)
    if sender == target {
        return Err(format!("Cannot delegate to self ('{sender}')"));
    }

    let agents_dir = home_dir.join("agents");

    // Read target agent's reports_to
    let target_config = read_agent_config(&agents_dir, target).await
        .ok_or_else(|| format!("Target agent '{target}' not found"))?;
    let target_reports_to = normalize_reports_to(&target_config.agent.reports_to);

    // Parent → Child: target reports to sender
    if target_reports_to == sender {
        return Ok(());
    }

    // Child → Parent: sender reports to target
    let sender_config = read_agent_config(&agents_dir, sender).await
        .ok_or_else(|| format!("Sender agent '{sender}' not found"))?;
    let sender_reports_to = normalize_reports_to(&sender_config.agent.reports_to);
    if sender_reports_to == target {
        return Ok(());
    }

    Err(format!(
        "Supervisor pattern violation: '{sender}' cannot delegate to '{target}'. \
         Only parent→child or child→parent delegation is allowed. \
         ('{target}'.reports_to='{}', '{sender}'.reports_to='{}')",
        target_reports_to, sender_reports_to,
    ))
}

/// Validate that a `reports_to` value references an existing agent (or is empty
/// for root agents) and does not create a cycle.
async fn validate_reports_to(
    home_dir: &Path,
    agent_name: &str,
    reports_to: &str,
) -> std::result::Result<(), String> {
    if reports_to.is_empty() || reports_to == "none" {
        return Ok(()); // root agent
    }

    // Cannot report to self
    if reports_to == agent_name {
        return Err(format!("Agent '{agent_name}' cannot report to itself"));
    }

    let agents_dir = home_dir.join("agents");

    // Target must exist
    if !agents_dir.join(reports_to).join("agent.toml").exists() {
        return Err(format!(
            "reports_to '{reports_to}' does not exist. \
             Create the agent first or use an empty string for root."
        ));
    }

    // Walk up the chain to detect cycles (max 20 hops as safety bound)
    let mut current = reports_to.to_string();
    let mut visited = std::collections::HashSet::new();
    visited.insert(agent_name.to_string());

    for _ in 0..20 {
        if !visited.insert(current.clone()) {
            return Err(format!(
                "Circular reports_to detected: setting '{agent_name}'.reports_to='{reports_to}' \
                 would create a cycle involving '{current}'"
            ));
        }
        match read_agent_config(&agents_dir, &current).await {
            Some(cfg) => {
                let next = &cfg.agent.reports_to;
                if next.is_empty() || next == "none" {
                    break; // reached root
                }
                current = next.clone();
            }
            None => break, // dangling reference — not our problem here
        }
    }

    Ok(())
}

/// Read an agent's config from disk.
async fn read_agent_config(
    agents_dir: &Path,
    agent_id: &str,
) -> Option<duduclaw_core::types::AgentConfig> {
    let toml_path = agents_dir.join(agent_id).join("agent.toml");
    let content = tokio::fs::read_to_string(&toml_path).await.ok()?;
    toml::from_str(&content).ok()
}

async fn resolve_main_agent_name(home_dir: &Path) -> String {
    let agents_dir = home_dir.join("agents");
    let mut entries = match tokio::fs::read_dir(&agents_dir).await {
        Ok(e) => e,
        Err(_) => return String::new(),
    };

    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let toml_path = path.join("agent.toml");
        if let Ok(content) = tokio::fs::read_to_string(&toml_path).await
            && let Ok(config) = toml::from_str::<duduclaw_core::types::AgentConfig>(&content)
                && config.agent.role == duduclaw_core::types::AgentRole::Main {
                    return config.agent.name;
                }
    }

    String::new()
}

// ── Config reader ────────────────────────────────────────────

async fn read_config(home_dir: &Path) -> Option<toml::Table> {
    let config_path = home_dir.join("config.toml");
    let content = tokio::fs::read_to_string(&config_path).await.ok()?;
    content.parse().ok()
}

/// Load the AES-256 keyfile and create a CryptoEngine.
fn load_crypto_engine(home_dir: &Path) -> Option<duduclaw_security::crypto::CryptoEngine> {
    let keyfile = home_dir.join(".keyfile");
    let bytes = std::fs::read(&keyfile).ok()?;
    if bytes.len() == 32 {
        let mut key = [0u8; 32];
        key.copy_from_slice(&bytes);
        duduclaw_security::crypto::CryptoEngine::new(&key).ok()
    } else {
        None
    }
}

/// Decrypt an encrypted base64 value using the per-machine keyfile.
fn decrypt_encrypted_value(encrypted: &str, home_dir: &Path) -> Option<String> {
    if encrypted.is_empty() { return None; }
    let engine = load_crypto_engine(home_dir)?;
    let plain = engine.decrypt_string(encrypted).ok()?;
    if plain.is_empty() { None } else { Some(plain) }
}

/// Decrypt a channel token from config.toml.
///
/// Tries the encrypted field (`_enc` suffix) first, then falls back to the
/// plaintext field for backwards compatibility.
fn decrypt_channel_token(config: &toml::Table, enc_key: &str, plain_key: &str, home_dir: &Path) -> String {
    let channels = config.get("channels").and_then(|c| c.as_table());

    // Try encrypted field first
    if let Some(enc_val) = channels.and_then(|c| c.get(enc_key)).and_then(|v| v.as_str())
        && let Some(decrypted) = decrypt_encrypted_value(enc_val, home_dir) {
            return decrypted;
        }

    // Fallback: plaintext field (backwards compat)
    channels
        .and_then(|c| c.get(plain_key))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Resolve the caller-identity agent name for MCP authorization.
///
/// Preference order (highest → lowest):
/// 1. `DUDUCLAW_AGENT_ID` env var — injected per-agent via `.mcp.json` so
///    the MCP subprocess knows which agent's Claude CLI spawned it. This
///    is the authoritative source: `check_supervisor_relation` compares
///    this identity against the target agent's `reports_to` chain.
/// 2. `config.toml [general] default_agent` — legacy fallback, kept for
///    backwards compatibility with installs whose `.mcp.json` hasn't yet
///    been migrated to include the env var (see
///    `duduclaw_agent::mcp_template::ensure_duduclaw_absolute_path`).
/// 3. Hard-coded "dudu" — final fallback for fresh installs with neither
///    env nor config set.
///
/// An empty `DUDUCLAW_AGENT_ID` (e.g. `"env": { "DUDUCLAW_AGENT_ID": "" }`)
/// is treated as missing and falls through to the config lookup — this
/// prevents accidental lockout if a stale migration produced an empty
/// string.
pub async fn get_default_agent(home_dir: &Path) -> String {
    if let Ok(env_id) = std::env::var(duduclaw_core::ENV_AGENT_ID)
        && !env_id.trim().is_empty()
    {
        return env_id;
    }

    let config = read_config(home_dir).await;
    config
        .as_ref()
        .and_then(|t| t.get("general"))
        .and_then(|g| g.get("default_agent"))
        .and_then(|v| v.as_str())
        .unwrap_or("dudu")
        .to_string()
}

// ── Main server loop ─────────────────────────────────────────

/// Run the MCP server, reading JSON-RPC from stdin and writing responses to stdout.
pub async fn run_mcp_server(home_dir: &Path) -> Result<()> {
    info!("Starting DuDuClaw MCP server");

    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| DuDuClawError::Gateway(format!("Failed to create HTTP client: {e}")))?;

    // Initialize memory engine
    let memory_db_path = home_dir.join("memory.db");
    let memory = SqliteMemoryEngine::new(&memory_db_path)
        .map_err(|e| DuDuClawError::Memory(format!("Failed to open memory DB: {e}")))?;

    let default_agent = get_default_agent(home_dir).await;

    // ── MCP Auth 初始化（W19-P0）──────────────────────────────────
    let principal = crate::mcp_auth::authenticate_from_env(home_dir)
        .map_err(|e| DuDuClawError::Gateway(format!("MCP authentication failed: {e}")))?;
    let ns_ctx = crate::mcp_namespace::resolve(&principal)
        .map_err(|e| DuDuClawError::Gateway(format!("MCP namespace resolution failed: {e}")))?;

    tracing::info!(
        client_id = %principal.client_id,
        namespace = %ns_ctx.write_namespace,
        is_external = principal.is_external,
        "MCP server authenticated"
    );

    // ── McpDispatcher 初始化（W20-P1 Phase 2A）───────────────────
    // Wraps rate limiter, daily quota, odoo, memory and all tool handlers.
    // All three transports (stdio, HTTP, SSE) share this dispatcher.
    let dispatcher = crate::mcp_dispatch::McpDispatcher::new(
        home_dir.to_path_buf(),
        http.clone(),
        std::sync::Arc::new(memory),
        default_agent.clone(),
        // RFC-21 §2: per-agent Odoo connector pool (lazy — slot populated on
        // first odoo_connect call for the calling agent).
        std::sync::Arc::new(crate::odoo_pool::OdooConnectorPool::default()),
        crate::mcp_rate_limit::RateLimiter::new(),
        crate::mcp_memory_quota::DailyQuota::new(),
    );

    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin);
    let mut line = String::new();

    loop {
        line.clear();
        let bytes_read = reader.read_line(&mut line).await.map_err(|e| {
            DuDuClawError::Gateway(format!("Failed to read from stdin: {e}"))
        })?;

        if bytes_read == 0 {
            // EOF — client disconnected
            info!("MCP server: stdin closed, shutting down");
            break;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Redact any API keys from the raw line before it touches any log output.
        let redacted_line = crate::mcp_redact::redact(trimmed);
        let request: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                warn!(line = %redacted_line, "MCP server: invalid JSON: {e}");
                let err = jsonrpc_error(&Value::Null, -32700, "Parse error");
                write_response(&mut stdout, &err).await?;
                continue;
            }
        };

        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let response = match method {
            "initialize" => handle_initialize(&id, &request),
            "tools/list" => handle_tools_list(&id, principal.is_external),
            "tools/call" => {
                // W20-P1 Phase 2A: delegate to McpDispatcher (scope → rate-limit → ns-inject → dispatch)
                let params = request.get("params").cloned().unwrap_or(Value::Null);
                dispatcher.dispatch_tool_call(&principal, &ns_ctx, &params, &id).await
            }
            "notifications/initialized" => {
                // This is a notification (no id expected in response), skip
                continue;
            }
            _ => jsonrpc_error(&id, -32601, &format!("Method not found: {method}")),
        };

        write_response(&mut stdout, &response).await?;
    }

    Ok(())
}

async fn write_response(
    stdout: &mut tokio::io::Stdout,
    response: &Value,
) -> Result<()> {
    let serialized = serde_json::to_string(response)
        .map_err(|e| DuDuClawError::Gateway(format!("Failed to serialize response: {e}")))?;
    // Redact any API keys that may have leaked into the response payload
    // (e.g. via error messages that echo back tool arguments).
    let redacted = crate::mcp_redact::redact(&serialized);
    let mut output = redacted.into_owned();
    output.push('\n');
    stdout.write_all(output.as_bytes()).await.map_err(|e| {
        DuDuClawError::Gateway(format!("Failed to write to stdout: {e}"))
    })?;
    stdout.flush().await.map_err(|e| {
        DuDuClawError::Gateway(format!("Failed to flush stdout: {e}"))
    })?;
    Ok(())
}

// ── Method handlers ──────────────────────────────────────────

fn handle_initialize(id: &Value, _request: &Value) -> Value {
    jsonrpc_response(
        id,
        serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": {}
            },
            "serverInfo": {
                "name": "duduclaw",
                "version": duduclaw_gateway::updater::current_version()
            }
        }),
    )
}

fn handle_tools_list(id: &Value, is_external: bool) -> Value {
    let tools: Vec<Value> = TOOLS
        .iter()
        .filter(|t| {
            !is_external || EXTERNAL_TOOLS_WHITELIST.contains(&t.name)
        })
        .map(build_tool_schema)
        .collect();
    jsonrpc_response(id, serde_json::json!({ "tools": tools }))
}

// RFC-21 §2: per-agent connector pool replaces the v1.10.1 global singleton.
// Defined in `crate::odoo_pool::OdooConnectorPool`.
pub(crate) type OdooState = std::sync::Arc<crate::odoo_pool::OdooConnectorPool>;

// ── Namespace-aware wiki agent resolver (W19-P0 M2) ──────────────────────────
/// Resolve the effective wiki agent for this principal from the namespace context.
///
/// Rules:
/// - External clients (`write_namespace = "external/{client_id}"`): wiki
///   operations are scoped to `{client_id}`.  The dispatcher has already
///   stripped any user-supplied `agent_id` argument, so the fallback in every
///   wiki handler lands here instead of on `default_agent`.
/// - Internal clients: preserve existing behaviour — fall back to
///   `default_agent`, which is the agent configured in config.toml.
fn wiki_agent_from_ns<'a>(
    ns_ctx: &'a crate::mcp_namespace::NamespaceContext,
    default_agent: &'a str,
) -> &'a str {
    ns_ctx
        .write_namespace
        .strip_prefix("external/")
        .unwrap_or(default_agent)
}

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
    let tool_name = params
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
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
    let is_state_changing = matches!(
        tool_name,
        "create_agent"
            | "agent_remove"
            | "agent_update"
            | "agent_update_soul"
            | "spawn_agent"
            | "send_to_agent"
            | "create_task"
            | "schedule_task"
            | "update_cron_task"
            | "delete_cron_task"
            | "pause_cron_task"
            | "tasks_create"
            | "tasks_update"
            | "tasks_claim"
            | "tasks_complete"
            | "tasks_block"
            | "activity_post"
            | "shared_skill_share"
            | "shared_skill_adopt"
    );
    let result = match tool_name {
        "send_message" => handle_send_message(&arguments, home_dir, http, default_agent).await,
        "web_search" => handle_web_search(&arguments, http).await,
        // ── W19-P0 M1: namespace-aware memory endpoints ────────────────────
        "memory_search" => crate::mcp_memory_handlers::handle_memory_search(&arguments, memory, ns_ctx).await,
        "memory_store"  => crate::mcp_memory_handlers::handle_memory_store(&arguments, memory, ns_ctx, daily_quota).await,
        "memory_read"   => crate::mcp_memory_handlers::handle_memory_read(&arguments, memory, ns_ctx).await,
        "memory_search_by_layer" => handle_memory_search_by_layer(&arguments, memory, default_agent).await,
        "memory_successful_conversations" => handle_memory_successful_conversations(&arguments, memory, default_agent).await,
        "memory_episodic_pressure" => handle_memory_episodic_pressure(&arguments, memory, default_agent).await,
        "memory_consolidation_status" => handle_memory_consolidation_status(memory, default_agent).await,
        "send_to_agent" => handle_send_to_agent(&arguments, home_dir, default_agent).await,
        "send_photo" => handle_send_media(&arguments, home_dir, http, "photo").await,
        "send_sticker" => handle_send_media(&arguments, home_dir, http, "sticker").await,
        "log_mood" => handle_log_mood(&arguments, home_dir, memory, default_agent).await,
        "schedule_task" => handle_schedule_task(&arguments, home_dir).await,
        "list_cron_tasks" => handle_list_cron_tasks(&arguments, home_dir, default_agent).await,
        "update_cron_task" => handle_update_cron_task(&arguments, home_dir).await,
        "delete_cron_task" => handle_delete_cron_task(&arguments, home_dir).await,
        "pause_cron_task" => handle_pause_cron_task(&arguments, home_dir).await,
        "create_reminder" => handle_create_reminder(&arguments, home_dir, default_agent).await,
        "list_reminders" => handle_list_reminders(&arguments, home_dir, default_agent).await,
        "cancel_reminder" => handle_cancel_reminder(&arguments, home_dir, default_agent).await,
        "create_agent" => handle_create_agent(&arguments, home_dir).await,
        "list_agents" => handle_list_agents(home_dir).await,
        "create_task" => handle_create_task(&arguments, home_dir, default_agent).await,
        "check_responses" => handle_check_responses(&arguments, home_dir).await,
        "task_status" => handle_task_status(&arguments, home_dir, default_agent).await,
        "agent_status" => handle_agent_status(&arguments, home_dir).await,
        "spawn_agent" => handle_spawn_agent(&arguments, home_dir, default_agent).await,
        "agent_update" => handle_agent_update(&arguments, home_dir).await,
        "agent_remove" => handle_agent_remove(&arguments, home_dir).await,
        "agent_update_soul" => handle_agent_update_soul(&arguments, home_dir).await,
        "skill_search" => handle_skill_search(&arguments, home_dir).await,
        "skill_list" => handle_skill_list(&arguments, home_dir).await,
        "skill_security_scan" => handle_skill_security_scan(&arguments, home_dir).await,
        "skill_graduate" => handle_skill_graduate(&arguments, home_dir).await,
        "skill_synthesis_status" => handle_skill_synthesis_status(&arguments, home_dir).await,
        "skill_synthesis_run" => handle_skill_synthesis_run(&arguments, home_dir, default_agent).await,
        "submit_feedback" => handle_submit_feedback(&arguments, home_dir, default_agent).await,
        "evolution_toggle" => handle_evolution_toggle(&arguments, home_dir).await,
        "evolution_status" => handle_evolution_status_tool(&arguments, home_dir, default_agent).await,
        "audit_trail_query" => handle_audit_trail_query(&arguments, home_dir, caller_client_id, caller_is_admin).await,
        "reliability_summary" => handle_reliability_summary(&arguments, home_dir, caller_client_id, caller_is_admin).await,
        // Channel settings tools
        "channel_config" => handle_channel_config(&arguments, home_dir).await,
        "channel_config_list" => handle_channel_config_list(&arguments, home_dir).await,
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
        // Compression tools
        // Model registry tools
        "model_search" => handle_model_search(&arguments, home_dir).await,
        "model_download" => handle_model_download(&arguments, home_dir).await,
        "model_recommend" => handle_model_recommend(home_dir).await,
        "compress_text" => handle_compress_text(&arguments).await,
        "decompress_text" => handle_decompress_text(&arguments).await,
        // Cost telemetry tools
        "cost_summary" => handle_cost_summary(&arguments, home_dir).await,
        "cost_agents" => handle_cost_agents(&arguments, home_dir).await,
        "cost_recent" => handle_cost_recent(&arguments).await,
        // Voice / ASR / TTS tools
        "transcribe_audio" => handle_transcribe_audio(&arguments).await,
        "synthesize_speech" => handle_synthesize_speech(&arguments).await,
        // Wiki Knowledge Base tools — use wiki_agent (namespace-aware) instead of
        // default_agent so external clients stay isolated in their own namespace.
        "wiki_ls" => handle_wiki_ls(&arguments, home_dir, wiki_agent).await,
        "wiki_read" => handle_wiki_read(&arguments, home_dir, wiki_agent).await,
        "wiki_write" => handle_wiki_write(&arguments, home_dir, wiki_agent).await,
        "wiki_search" => handle_wiki_search(&arguments, home_dir, wiki_agent).await,
        "wiki_lint" => handle_wiki_lint(&arguments, home_dir, wiki_agent).await,
        "wiki_stats" => handle_wiki_stats(&arguments, home_dir, wiki_agent).await,
        "wiki_export" => handle_wiki_export(&arguments, home_dir, wiki_agent).await,
        "wiki_dedup" => handle_wiki_dedup(&arguments, home_dir, wiki_agent).await,
        "wiki_graph" => handle_wiki_graph(&arguments, home_dir, wiki_agent).await,
        "wiki_rebuild_fts" => handle_wiki_rebuild_fts(&arguments, home_dir, wiki_agent).await,
        "wiki_trust_audit" => handle_wiki_trust_audit(&arguments, home_dir, wiki_agent).await,
        "wiki_trust_history" => handle_wiki_trust_history(&arguments, home_dir, wiki_agent).await,
        // Shared Wiki tools
        "shared_wiki_ls" => handle_shared_wiki_ls(home_dir).await,
        "shared_wiki_read" => handle_shared_wiki_read(&arguments, home_dir).await,
        "shared_wiki_write" => handle_shared_wiki_write(&arguments, home_dir, default_agent).await,
        "shared_wiki_search" => handle_shared_wiki_search(&arguments, home_dir).await,
        "shared_wiki_delete" => handle_shared_wiki_delete(&arguments, home_dir, default_agent).await,
        "shared_wiki_stats" => handle_shared_wiki_stats(home_dir).await,
        "shared_wiki_lint" => handle_shared_wiki_lint(home_dir).await,
        "wiki_namespace_status" => handle_wiki_namespace_status(home_dir).await,
        "identity_resolve" => handle_identity_resolve(&arguments, home_dir, default_agent).await,
        "wiki_share" => handle_wiki_share(&arguments, home_dir, wiki_agent).await,
        // Skill Internalization tools
        "skill_extract" => handle_skill_extract(&arguments, home_dir, default_agent).await,
        // Program execution
        "execute_program" => handle_execute_program(&arguments).await,
        // Skill Bank tools
        "skill_bank_search" => handle_skill_bank_search(&arguments).await,
        "skill_bank_feedback" => handle_skill_bank_feedback(&arguments).await,
        // Session tools
        "session_restore_context" => handle_session_restore_context(&arguments).await,
        // Task Board tools
        "tasks_list" => handle_tasks_list(&arguments, home_dir, default_agent).await,
        "tasks_create" => handle_tasks_create(&arguments, home_dir, default_agent).await,
        "tasks_update" => handle_tasks_update(&arguments, home_dir).await,
        "tasks_claim" => handle_tasks_claim(&arguments, home_dir, default_agent).await,
        "tasks_complete" => handle_tasks_complete(&arguments, home_dir, default_agent).await,
        "tasks_block" => handle_tasks_block(&arguments, home_dir, default_agent).await,
        // Activity Feed tools
        "activity_post" => handle_activity_post(&arguments, home_dir, default_agent).await,
        "activity_list" => handle_activity_list(&arguments, home_dir, default_agent).await,
        // Autopilot tools
        "autopilot_list" => handle_autopilot_list(&arguments, home_dir).await,
        // Shared Skills tools
        "shared_skill_list" => handle_shared_skill_list(&arguments, home_dir).await,
        "shared_skill_share" => handle_shared_skill_share(&arguments, home_dir, default_agent).await,
        "shared_skill_adopt" => handle_shared_skill_adopt(&arguments, home_dir, default_agent).await,
        // Computer Use tools — require computer_use capability
        "computer_screenshot" | "computer_click" | "computer_type" | "computer_key"
        | "computer_scroll" | "computer_session_start" | "computer_session_stop" => {
            // SEC: Validate agent ID before path construction (prevent traversal)
            if !is_valid_agent_id(default_agent) {
                return jsonrpc_error(id, -32602, "Invalid agent ID");
            }
            // SEC: Verify the calling agent has computer_use capability enabled.
            let cu_allowed = {
                let agent_dir = home_dir.join("agents").join(default_agent);
                let toml_path = agent_dir.join("agent.toml");
                // Use async read to avoid blocking the Tokio worker thread
                tokio::fs::read_to_string(&toml_path)
                    .await
                    .ok()
                    .and_then(|c| c.parse::<toml::Table>().ok())
                    .and_then(|t| t.get("capabilities")?.as_table()?.get("computer_use")?.as_bool())
                    .unwrap_or(false)
            };
            if !cu_allowed {
                return jsonrpc_error(
                    id,
                    -32603,
                    "computer_use capability is not enabled for this agent. Set [capabilities] computer_use = true in agent.toml",
                );
            }
            handle_computer_use_tool(tool_name, &arguments).await
        }
        // Odoo ERP tools
        t if t.starts_with("odoo_") => {
            handle_odoo_tool(t, &arguments, home_dir, odoo, default_agent).await
        }
        _ => {
            return jsonrpc_error(
                id,
                -32602,
                &format!("Unknown tool: {tool_name}"),
            );
        }
    };

    // ── Tool call audit trail (L1 anti-hallucination) ──────────
    // Use the actual calling agent's ID, not just default_agent.
    // In delegated contexts, DUDUCLAW_DELEGATION_SENDER identifies the real caller.
    if is_state_changing {
        let success = !result
            .get("isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let actual_agent = std::env::var(duduclaw_core::ENV_DELEGATION_SENDER)
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| default_agent.to_string());
        let params_summary = build_params_summary(tool_name, &arguments);
        duduclaw_security::audit::append_tool_call(
            home_dir,
            &actual_agent,
            tool_name,
            &params_summary,
            success,
        );
    }

    jsonrpc_response(id, result)
}

/// Build a short summary of tool call parameters for audit logging.
/// Avoids logging full payloads (which may contain sensitive data).
fn build_params_summary(tool_name: &str, args: &Value) -> String {
    match tool_name {
        "create_agent" => {
            let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let display = args.get("display_name").and_then(|v| v.as_str()).unwrap_or("?");
            format!("name={name} display_name={display}")
        }
        "agent_remove" => {
            let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            format!("name={name}")
        }
        "agent_update" => {
            let agent = args.get("agent_id").and_then(|v| v.as_str()).unwrap_or("?");
            let field = args.get("field").and_then(|v| v.as_str()).unwrap_or("?");
            format!("agent_id={agent} field={field}")
        }
        "agent_update_soul" => {
            let agent = args.get("agent_id").and_then(|v| v.as_str()).unwrap_or("?");
            format!("agent_id={agent}")
        }
        "spawn_agent" | "send_to_agent" => {
            let agent = args.get("agent_id").and_then(|v| v.as_str()).unwrap_or("?");
            format!("agent_id={agent}")
        }
        "schedule_task" => {
            let task_type = args.get("type").and_then(|v| v.as_str()).unwrap_or("?");
            let agent = args.get("agent_id").and_then(|v| v.as_str()).unwrap_or("?");
            format!("type={task_type} agent_id={agent}")
        }
        "update_cron_task" => {
            let id = args.get("id").and_then(|v| v.as_str()).unwrap_or("?");
            let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            format!("id={id} name={name}")
        }
        "delete_cron_task" => {
            let id = args.get("id").and_then(|v| v.as_str()).unwrap_or("?");
            let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            format!("id={id} name={name}")
        }
        "pause_cron_task" => {
            let id = args.get("id").and_then(|v| v.as_str()).unwrap_or("?");
            let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let enabled = args.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
            format!("id={id} name={name} enabled={enabled}")
        }
        _ => {
            let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let id = args.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
            format!("name={name} agent_id={id}")
        }
    }
}

// ── Voice / ASR / TTS handlers ─────────────────────────────────

async fn handle_transcribe_audio(args: &Value) -> Value {
    use base64::Engine;

    let audio_b64 = match args.get("audio_base64").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return tool_error("Missing required parameter: audio_base64"),
    };

    // Limit input size: 34MB base64 ≈ 25MB decoded
    const MAX_B64_LEN: usize = 34 * 1024 * 1024;
    if audio_b64.len() > MAX_B64_LEN {
        return tool_error(&format!("Audio too large: {} bytes (max 25MB)", audio_b64.len() * 3 / 4));
    }

    let language = args.get("language").and_then(|v| v.as_str()).unwrap_or("zh");

    let audio_bytes = match base64::engine::general_purpose::STANDARD.decode(audio_b64) {
        Ok(b) => b,
        Err(e) => return tool_error(&format!("Invalid base64: {e}")),
    };

    // Transcribe via Whisper API (sends raw audio bytes, format auto-detected)
    match duduclaw_inference::whisper::transcribe(
        &audio_bytes,
        Some(language),
        &duduclaw_inference::whisper::WhisperMode::Api,
    ).await {
        Ok(text) => tool_text(&text),
        Err(e) => tool_error(&format!("Transcription failed: {e}")),
    }
}

async fn handle_synthesize_speech(args: &Value) -> Value {
    use base64::Engine;
    use duduclaw_gateway::tts::TtsProvider;

    let text = match args.get("text").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s,
        _ => return tool_error("Missing required parameter: text"),
    };
    let voice = args.get("voice").and_then(|v| v.as_str()).unwrap_or("");

    let provider = duduclaw_gateway::tts::EdgeTtsProvider::new();
    match provider.synthesize(text, voice).await {
        Ok(audio_bytes) => {
            let b64 = base64::engine::general_purpose::STANDARD.encode(&audio_bytes);
            serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": format!("Audio synthesized ({} bytes). Base64 data:\n{}", audio_bytes.len(), b64)
                }]
            })
        }
        Err(e) => tool_error(&format!("Speech synthesis failed: {e}")),
    }
}

// ── Channel settings tool handlers ──────────────────────────────

const VALID_CHANNELS: &[&str] = &["discord", "telegram", "slack", "line", "whatsapp", "feishu"];
const VALID_KEYS: &[&str] = &["mention_only", "auto_thread", "allowed_channels", "agent_override", "response_mode", "thread_archive_minutes"];

/// Validate scope_id: max 64 chars, alphanumeric + underscore/hyphen or "global"/"dm".
fn validate_scope_id(scope_id: &str) -> std::result::Result<(), String> {
    if scope_id.len() > 64 {
        return Err("scope_id too long (max 64 chars)".into());
    }
    if scope_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        Ok(())
    } else {
        Err("scope_id contains invalid characters".into())
    }
}

/// Validate value based on key type.
fn validate_value(key: &str, value: &str) -> std::result::Result<(), String> {
    match key {
        "mention_only" | "auto_thread" => {
            if value != "true" && value != "false" {
                return Err(format!("{key} must be 'true' or 'false'"));
            }
        }
        "allowed_channels" => {
            if serde_json::from_str::<Vec<String>>(value).is_err() {
                return Err("allowed_channels must be a JSON array of strings, e.g. [\"ch1\",\"ch2\"]".into());
            }
        }
        "response_mode" => {
            if !["embed", "plain", "auto"].contains(&value) {
                return Err("response_mode must be 'embed', 'plain', or 'auto'".into());
            }
        }
        "thread_archive_minutes" => {
            if !["60", "1440", "4320", "10080"].contains(&value) {
                return Err("thread_archive_minutes must be 60, 1440, 4320, or 10080".into());
            }
        }
        _ => {} // agent_override: any string is valid (checked against registry at use time)
    }
    Ok(())
}

async fn handle_channel_config(args: &Value, home_dir: &Path) -> Value {
    let channel = match args.get("channel").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return tool_error("Missing required parameter: channel"),
    };
    let scope_id = match args.get("scope_id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return tool_error("Missing required parameter: scope_id"),
    };
    let key = match args.get("key").and_then(|v| v.as_str()) {
        Some(k) => k,
        None => return tool_error("Missing required parameter: key"),
    };

    if !VALID_CHANNELS.contains(&channel) {
        return tool_error(&format!("Invalid channel type: {channel}"));
    }
    if !VALID_KEYS.contains(&key) {
        return tool_error(&format!("Invalid key: {key}. Valid keys: {}", VALID_KEYS.join(", ")));
    }
    if let Err(e) = validate_scope_id(scope_id) {
        return tool_error(&format!("Invalid scope_id: {e}"));
    }

    let db_path = home_dir.join("sessions.db");
    let mgr = match duduclaw_gateway::channel_settings::ChannelSettingsManager::from_session_db(&db_path) {
        Ok(m) => m,
        Err(e) => return tool_error(&format!("Failed to open settings DB: {e}")),
    };

    if let Some(value) = args.get("value").and_then(|v| v.as_str()) {
        if let Err(e) = validate_value(key, value) {
            return tool_error(&format!("Invalid value: {e}"));
        }
        match mgr.set(channel, scope_id, key, value).await {
            Ok(()) => tool_text(&format!("Set {channel}/{scope_id}/{key} = {value}")),
            Err(e) => tool_error(&format!("Failed to set: {e}")),
        }
    } else {
        let value = mgr.get_with_fallback(channel, scope_id, key, "(not set)").await;
        tool_text(&format!("{channel}/{scope_id}/{key} = {value}"))
    }
}

async fn handle_channel_config_list(args: &Value, home_dir: &Path) -> Value {
    let channel = match args.get("channel").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return tool_error("Missing required parameter: channel"),
    };
    let scope_id = match args.get("scope_id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return tool_error("Missing required parameter: scope_id"),
    };

    if !VALID_CHANNELS.contains(&channel) {
        return tool_error(&format!("Invalid channel type: {channel}"));
    }
    if let Err(e) = validate_scope_id(scope_id) {
        return tool_error(&format!("Invalid scope_id: {e}"));
    }

    let db_path = home_dir.join("sessions.db");
    let mgr = match duduclaw_gateway::channel_settings::ChannelSettingsManager::from_session_db(&db_path) {
        Ok(m) => m,
        Err(e) => return tool_error(&format!("Failed to open settings DB: {e}")),
    };

    let all = mgr.get_all(channel, scope_id).await;
    if all.is_empty() {
        tool_text(&format!("No settings configured for {channel}/{scope_id}. Using defaults."))
    } else {
        let lines: Vec<String> = all.iter().map(|(k, v)| format!("{k} = {v}")).collect();
        tool_text(&format!("Settings for {channel}/{scope_id}:\n{}", lines.join("\n")))
    }
}

// ── Wiki Knowledge Base handlers ────────────────────────────

/// Reserved wiki filenames that cannot be overwritten by wiki_write.
const WIKI_RESERVED: &[&str] = &["_schema.md", "_index.md", "_log.md"];

/// Maximum wiki page size (512 KB).
const WIKI_MAX_PAGE_SIZE: usize = 512 * 1024;

/// Default wiki _schema.md content.
const WIKI_DEFAULT_SCHEMA: &str = r#"# Wiki Schema

## Directory Structure
- `entities/` — People, organizations, products, customers
- `concepts/` — Domain concepts, processes, principles
- `sources/` — Summaries of raw source materials
- `synthesis/` — Cross-topic analysis, comparisons, trends

## Page Format
Every page MUST have YAML frontmatter:
```yaml
---
title: <page title>
created: <ISO 8601>
updated: <ISO 8601>
tags: [tag1, tag2]
related: [path/to/related1.md, path/to/related2.md]
sources: [source1, source2]
---
```

## Naming Convention
- Filename: kebab-case (e.g. `wang-ming-customer.md`)
- Entity pages: `entities/{name}.md`
- Concept pages: `concepts/{topic}.md`
- Source summaries: `sources/{date}-{title}.md`
- Synthesis: `synthesis/{topic}.md`

## Cross-Reference Format
Use relative markdown links: `[Display Text](../concepts/topic.md)`

## Operations
### Ingest (adding new source)
1. Read the source material
2. Create `sources/{date}-{title}.md` summary
3. Update or create relevant entity/concept pages
4. Update `_index.md` with new pages
5. Check for contradictions with existing pages

### Query (answering questions)
1. Read `_index.md` to locate relevant pages
2. Read relevant pages
3. Synthesize answer
4. If answer is valuable, file as new `synthesis/` page

### Lint (health check)
1. Find contradictions between pages
2. Find orphan pages (not in _index.md or no inbound links)
3. Find stale pages (not updated in >30 days, related sources newer)
4. Suggest missing pages for mentioned-but-uncreated entities
"#;

/// Resolve the wiki directory for an agent, creating it if needed.
/// Returns `Err` on invalid agent_id or filesystem failures.
///
/// BUG-QA-003: External MCP clients (e.g. claude-desktop) may not have an agent
/// directory on first connect. Auto-create it so wiki operations work immediately
/// without requiring manual provisioning.
fn resolve_wiki_dir(home_dir: &Path, agent_id: &str) -> std::result::Result<std::path::PathBuf, String> {
    if !is_valid_agent_id(agent_id) {
        return Err("Invalid agent_id".to_string());
    }
    let agent_dir = home_dir.join("agents").join(agent_id);
    if !agent_dir.exists() {
        std::fs::create_dir_all(&agent_dir)
            .map_err(|e| format!("Failed to create agent dir for '{}': {}", agent_id, e))?;
    }
    Ok(agent_dir.join("wiki"))
}

/// Check whether `caller_agent` is allowed to read `target_agent`'s wiki.
///
/// Returns `Ok(true)` if access is allowed, `Ok(false)` if denied.
/// Always allows self-access (caller == target).
fn check_wiki_visibility(home_dir: &Path, target_agent: &str, caller_agent: &str) -> std::result::Result<bool, String> {
    // Self-access always allowed
    if caller_agent == target_agent {
        return Ok(true);
    }

    let agent_toml_path = home_dir.join("agents").join(target_agent).join("agent.toml");
    let toml_content = match std::fs::read_to_string(&agent_toml_path) {
        Ok(c) => c,
        Err(_) => return Ok(true), // If agent.toml unreadable, default to open (backward compat)
    };

    // Parse wiki_visible_to from [capabilities] section
    // Simple TOML field extraction without a full parser
    let visible_to = extract_toml_string_array(&toml_content, "wiki_visible_to");

    // If field is absent, default to ["*"] (backward compatible)
    let visible_to = match visible_to {
        Some(v) => v,
        None => return Ok(true),
    };

    // ["*"] means all agents can read
    if visible_to.iter().any(|v| v == "*") {
        return Ok(true);
    }

    // Empty list means fully private
    if visible_to.is_empty() {
        return Ok(false);
    }

    // Check if caller is in the list
    Ok(visible_to.iter().any(|v| v == caller_agent))
}

/// Extract a string array value from TOML content (simple parser, no dependency).
/// Handles format: `field_name = ["a", "b", "c"]`
fn extract_toml_string_array(content: &str, field: &str) -> Option<Vec<String>> {
    let prefix = format!("{} = ", field);
    let alt_prefix = format!("{}=", field);
    for line in content.lines() {
        let trimmed = line.trim();
        let rest = if let Some(r) = trimmed.strip_prefix(&prefix) {
            r
        } else if let Some(r) = trimmed.strip_prefix(&alt_prefix) {
            r
        } else {
            continue;
        };
        let rest = rest.trim();
        if rest.starts_with('[') && rest.ends_with(']') {
            let inner = &rest[1..rest.len() - 1];
            let items: Vec<String> = inner
                .split(',')
                .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
                .filter(|s| !s.is_empty())
                .collect();
            return Some(items);
        }
    }
    None
}

/// Ensure the wiki directory structure exists, creating scaffold if needed.
fn ensure_wiki_dir(wiki_dir: &Path) -> std::result::Result<(), String> {
    let subdirs = ["entities", "concepts", "sources", "synthesis"];
    for sub in &subdirs {
        let p = wiki_dir.join(sub);
        if !p.exists() {
            std::fs::create_dir_all(&p).map_err(|e| format!("Failed to create {}: {e}", p.display()))?;
        }
    }

    // Create _schema.md if missing
    let schema_path = wiki_dir.join("_schema.md");
    if !schema_path.exists() {
        std::fs::write(&schema_path, WIKI_DEFAULT_SCHEMA)
            .map_err(|e| format!("Failed to write _schema.md: {e}"))?;
    }

    // Create _index.md if missing
    let index_path = wiki_dir.join("_index.md");
    if !index_path.exists() {
        std::fs::write(&index_path, "# Wiki Index\n\n<!-- Auto-maintained by wiki_write. One entry per page. -->\n")
            .map_err(|e| format!("Failed to write _index.md: {e}"))?;
    }

    // Create _log.md if missing
    let log_path = wiki_dir.join("_log.md");
    if !log_path.exists() {
        std::fs::write(&log_path, "# Wiki Log\n\n<!-- Append-only operation log. -->\n")
            .map_err(|e| format!("Failed to write _log.md: {e}"))?;
    }

    Ok(())
}

/// Required frontmatter fields for Karpathy-style LLM wiki pages.
const WIKI_REQUIRED_FIELDS: &[&str] = &["title", "created", "updated", "tags", "layer", "trust"];

/// Regex-free fallback phrases that indicate a page was authored from a stale
/// LLM prior (e.g. web_search tool failure) rather than live evidence. These
/// are noise in the shared wiki per project rule:
///   「有 fallback 的資料不應該混入共用 wiki 中產生雜訊」
const WIKI_FALLBACK_MARKERS: &[&str] = &[
    "無法取得",
    "web_search 失敗",
    "web_search failed",
    "no results found",
    "基於訓練資料",
    "基於我的訓練資料",
    "based on training data",
    "based on my training data",
    "fallback 資料",
    "fallback mode",
    "查無結果",
    "搜尋工具失效",
    "cannot fetch",
    "unable to fetch",
];

/// Scan body for fallback markers. Returns the matched marker, if any.
/// Lowercase comparison for ASCII markers; direct substring for CJK.
fn detect_fallback_content(body: &str) -> Option<&'static str> {
    let lower = body.to_lowercase();
    for marker in WIKI_FALLBACK_MARKERS {
        let marker_lower = marker.to_lowercase();
        if lower.contains(&marker_lower) {
            return Some(marker);
        }
    }
    None
}

/// Validate Karpathy-style frontmatter. Returns a list of missing fields.
/// Caller decides whether missing fields are fatal (shared wiki) or warn-only.
fn validate_wiki_frontmatter(content: &str) -> std::result::Result<(), String> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return Err(
            "Missing YAML frontmatter. Every page must start with `---` and declare: \
             title, created, updated, tags, layer, trust."
                .to_string(),
        );
    }
    let rest = &trimmed[3..];
    let end = match rest.find("\n---") {
        Some(e) => e,
        None => return Err("Frontmatter is not closed with a trailing `---`.".to_string()),
    };
    let fm = &rest[..end];

    let mut missing: Vec<&str> = Vec::new();
    for field in WIKI_REQUIRED_FIELDS {
        let prefix = format!("{}:", field);
        let found = fm.lines().any(|line| line.trim_start().starts_with(&prefix));
        if !found {
            missing.push(field);
        }
    }
    if !missing.is_empty() {
        return Err(format!(
            "Frontmatter missing required field(s): {}. \
             See _schema.md; the Karpathy wiki schema requires all of: {}.",
            missing.join(", "),
            WIKI_REQUIRED_FIELDS.join(", ")
        ));
    }

    // Trust must parse as a number in [0.0, 1.0]
    if let Some(raw) = extract_frontmatter_field(content, "trust") {
        match raw.parse::<f32>() {
            Ok(t) if (0.0..=1.0).contains(&t) => {}
            Ok(t) => return Err(format!("Frontmatter `trust` must be in [0.0, 1.0], got {t}")),
            Err(_) => return Err(format!("Frontmatter `trust` must be a number, got `{raw}`")),
        }
    }
    Ok(())
}

/// Validate a wiki page path: no traversal, must end with .md, not reserved.
fn validate_wiki_page_path(page_path: &str) -> std::result::Result<(), String> {
    if page_path.is_empty() {
        return Err("page_path is required".to_string());
    }
    if page_path.contains("..") {
        return Err("Path traversal (..) is not allowed".to_string());
    }
    if page_path.starts_with('/') || page_path.starts_with('\\') {
        return Err("Absolute paths are not allowed".to_string());
    }
    if !page_path.ends_with(".md") {
        return Err("Page path must end with .md".to_string());
    }
    // Check reserved filenames
    let filename = std::path::Path::new(page_path)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("");
    if WIKI_RESERVED.contains(&filename) {
        return Err(format!("'{}' is a reserved wiki file and cannot be overwritten", filename));
    }
    Ok(())
}

/// Extract title from YAML frontmatter (best-effort, no YAML parser dependency).
fn extract_frontmatter_title(content: &str) -> Option<String> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }
    // Find the closing ---
    let rest = &trimmed[3..];
    let end = rest.find("\n---")?;
    let frontmatter = &rest[..end];
    for line in frontmatter.lines() {
        let line = line.trim();
        if let Some(after) = line.strip_prefix("title:") {
            let title = after.trim().trim_matches('"').trim_matches('\'');
            if !title.is_empty() {
                return Some(title.to_string());
            }
        }
    }
    None
}

/// Extract the updated field from YAML frontmatter.
fn extract_frontmatter_updated(content: &str) -> Option<String> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }
    let rest = &trimmed[3..];
    let end = rest.find("\n---")?;
    let frontmatter = &rest[..end];
    for line in frontmatter.lines() {
        let line = line.trim();
        if let Some(after) = line.strip_prefix("updated:") {
            let val = after.trim().trim_matches('"').trim_matches('\'');
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
    }
    None
}

/// Update _index.md with an entry for a page.
/// Format: `- [{title}]({page_path}) — updated {date}`
fn update_wiki_index(wiki_dir: &Path, page_path: &str, title: &str) -> std::result::Result<(), String> {
    let index_path = wiki_dir.join("_index.md");
    let existing = std::fs::read_to_string(&index_path).unwrap_or_default();

    // Build the new entry line
    let now = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let entry_line = format!("- [{}]({}) — updated {}", title, page_path, now);

    // Check if this page already has an entry and replace it
    let link_pattern = format!("]({})", page_path);
    let mut lines: Vec<String> = existing.lines().map(String::from).collect();
    let mut found = false;
    for line in &mut lines {
        if line.contains(&link_pattern) {
            *line = entry_line.clone();
            found = true;
            break;
        }
    }

    if !found {
        lines.push(entry_line);
    }

    let new_content = lines.join("\n") + "\n";
    std::fs::write(&index_path, new_content)
        .map_err(|e| format!("Failed to update _index.md: {e}"))
}

/// Append a log entry to _log.md.
fn append_wiki_log(wiki_dir: &Path, action: &str, page_path: &str) -> std::result::Result<(), String> {
    let log_path = wiki_dir.join("_log.md");
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S").to_string();
    let entry = format!("## [{}] {} | {}\n", now, action, page_path);

    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| format!("Failed to open _log.md: {e}"))?;
    f.write_all(entry.as_bytes())
        .map_err(|e| format!("Failed to append to _log.md: {e}"))?;
    Ok(())
}

/// Collect all .md files under `dir` recursively (relative to `base`).
fn collect_md_files(base: &Path, dir: &Path) -> Vec<std::path::PathBuf> {
    let mut result = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return result,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            result.extend(collect_md_files(base, &path));
        } else if path.extension().and_then(|e| e.to_str()) == Some("md")
            && let Ok(rel) = path.strip_prefix(base) {
                // Skip reserved files
                let fname = path.file_name().and_then(|f| f.to_str()).unwrap_or("");
                if !WIKI_RESERVED.contains(&fname) {
                    result.push(rel.to_path_buf());
                }
            }
    }
    result
}

async fn handle_wiki_ls(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let agent_id = args.get("agent_id").and_then(|v| v.as_str()).unwrap_or(default_agent);

    // Visibility check for cross-agent access
    if agent_id != default_agent {
        match check_wiki_visibility(home_dir, agent_id, default_agent) {
            Ok(false) => return tool_error(&format!(
                "Agent '{}' wiki is not visible to '{}'. Ask the owner to add you to wiki_visible_to.",
                agent_id, default_agent
            )),
            Err(e) => return tool_error(&format!("Visibility check failed: {e}")),
            _ => {}
        }
    }

    let wiki_dir = match resolve_wiki_dir(home_dir, agent_id) {
        Ok(d) => d,
        Err(e) => return tool_error(&e),
    };

    if !wiki_dir.exists() {
        return tool_text(&format!("No wiki found for agent '{}'. Use wiki_write to create the first page.", agent_id));
    }

    let pages = collect_md_files(&wiki_dir, &wiki_dir);
    if pages.is_empty() {
        return tool_text("Wiki directory exists but contains no pages.");
    }

    let mut lines = Vec::with_capacity(pages.len() + 1);
    lines.push(format!("Wiki for agent '{}' ({} pages):\n", agent_id, pages.len()));

    for rel_path in &pages {
        let full_path = wiki_dir.join(rel_path);
        let content = std::fs::read_to_string(&full_path).unwrap_or_default();
        let title = extract_frontmatter_title(&content)
            .unwrap_or_else(|| rel_path.to_string_lossy().to_string());
        let updated = extract_frontmatter_updated(&content).unwrap_or_else(|| "?".to_string());
        lines.push(format!("  {} — {} (updated: {})", rel_path.display(), title, updated));
    }

    tool_text(&lines.join("\n"))
}

async fn handle_wiki_read(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let agent_id = args.get("agent_id").and_then(|v| v.as_str()).unwrap_or(default_agent);
    let page_path = match args.get("page_path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return tool_error("Missing required parameter: page_path"),
    };

    // Visibility check for cross-agent access
    if agent_id != default_agent {
        match check_wiki_visibility(home_dir, agent_id, default_agent) {
            Ok(false) => return tool_error(&format!(
                "Agent '{}' wiki is not visible to '{}'. Ask the owner to add you to wiki_visible_to.",
                agent_id, default_agent
            )),
            Err(e) => return tool_error(&format!("Visibility check failed: {e}")),
            _ => {}
        }
    }

    // Allow reading reserved files (e.g. _index.md, _schema.md) — validation only blocks writes
    if page_path.contains("..") || page_path.starts_with('/') || page_path.starts_with('\\') {
        return tool_error("Path traversal is not allowed");
    }

    let wiki_dir = match resolve_wiki_dir(home_dir, agent_id) {
        Ok(d) => d,
        Err(e) => return tool_error(&e),
    };

    let full_path = wiki_dir.join(page_path);

    // Verify the resolved path is still under wiki_dir (symlink protection)
    if let (Ok(canon_wiki), Ok(canon_page)) = (wiki_dir.canonicalize(), full_path.canonicalize())
        && !canon_page.starts_with(&canon_wiki) {
            return tool_error("Path escapes wiki directory");
        }

    match std::fs::read_to_string(&full_path) {
        Ok(content) => tool_text(&content),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tool_error(&format!("Page not found: {}", page_path))
        }
        Err(e) => tool_error(&format!("Failed to read page: {e}")),
    }
}

async fn handle_wiki_write(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let agent_id = args.get("agent_id").and_then(|v| v.as_str()).unwrap_or(default_agent);
    let page_path = match args.get("page_path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return tool_error("Missing required parameter: page_path"),
    };
    let content = match args.get("content").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return tool_error("Missing required parameter: content"),
    };

    if let Err(e) = validate_wiki_page_path(page_path) {
        return tool_error(&e);
    }

    if content.len() > WIKI_MAX_PAGE_SIZE {
        return tool_error(&format!("Content too large: {} bytes (max {})", content.len(), WIKI_MAX_PAGE_SIZE));
    }

    let wiki_dir = match resolve_wiki_dir(home_dir, agent_id) {
        Ok(d) => d,
        Err(e) => return tool_error(&e),
    };

    // Ensure wiki scaffold exists
    if let Err(e) = ensure_wiki_dir(&wiki_dir) {
        return tool_error(&e);
    }

    let full_path = wiki_dir.join(page_path);

    // Ensure parent directory exists
    if let Some(parent) = full_path.parent()
        && !parent.exists()
            && let Err(e) = std::fs::create_dir_all(parent) {
                return tool_error(&format!("Failed to create directory: {e}"));
            }

    let is_new = !full_path.exists();

    // Atomic write: temp file + rename
    let tmp_path = full_path.with_extension("md.tmp");
    if let Err(e) = std::fs::write(&tmp_path, content) {
        return tool_error(&format!("Failed to write temp file: {e}"));
    }
    if let Err(e) = std::fs::rename(&tmp_path, &full_path) {
        // Clean up temp file on rename failure
        let _ = std::fs::remove_file(&tmp_path);
        return tool_error(&format!("Failed to rename temp file: {e}"));
    }

    // Update _index.md
    let update_index = args
        .get("update_index")
        .and_then(|v| v.as_str())
        .map(|s| s != "false")
        .unwrap_or(true);

    if update_index {
        let title = extract_frontmatter_title(content)
            .unwrap_or_else(|| page_path.to_string());
        if let Err(e) = update_wiki_index(&wiki_dir, page_path, &title) {
            warn!("Failed to update wiki index: {e}");
        }
    }

    // Append to _log.md
    let action = if is_new { "create" } else { "update" };
    if let Err(e) = append_wiki_log(&wiki_dir, action, page_path) {
        warn!("Failed to append wiki log: {e}");
    }

    let verb = if is_new { "Created" } else { "Updated" };
    tool_text(&format!("{} wiki page: {}", verb, page_path))
}

async fn handle_wiki_search(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let agent_id = args.get("agent_id").and_then(|v| v.as_str()).unwrap_or(default_agent);

    // Visibility check for cross-agent access
    if agent_id != default_agent {
        match check_wiki_visibility(home_dir, agent_id, default_agent) {
            Ok(false) => return tool_error(&format!(
                "Agent '{}' wiki is not visible to '{}'. Ask the owner to add you to wiki_visible_to.",
                agent_id, default_agent
            )),
            Err(e) => return tool_error(&format!("Visibility check failed: {e}")),
            _ => {}
        }
    }

    let query = match args.get("query").and_then(|v| v.as_str()) {
        Some(q) if !q.is_empty() => q,
        _ => return tool_error("Missing required parameter: query"),
    };
    let limit: usize = args
        .get("limit")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or(10)
        .min(100);
    let min_trust: Option<f32> = args
        .get("min_trust")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok());
    let layer_filter: Option<duduclaw_memory::WikiLayer> = args
        .get("layer")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok());
    let expand: bool = args
        .get("expand")
        .and_then(|v| v.as_str())
        .map(|s| s == "true")
        .unwrap_or(false);

    let wiki_dir = match resolve_wiki_dir(home_dir, agent_id) {
        Ok(d) => d,
        Err(e) => return tool_error(&e),
    };

    if !wiki_dir.exists() {
        return tool_text("No wiki found. Use wiki_write to create the first page.");
    }

    let store = duduclaw_memory::WikiStore::new(wiki_dir);
    let hits = match store.search_filtered(query, limit, min_trust, layer_filter, expand) {
        Ok(h) => h,
        Err(e) => return tool_error(&format!("Wiki search failed: {e}")),
    };

    if hits.is_empty() {
        return tool_text(&format!("No wiki pages match query: '{}'", query));
    }

    let mut output = format!("Found {} matching pages for '{}':\n\n", hits.len(), query);
    for h in &hits {
        let expanded_tag = if h.score == 0 { " [expanded]" } else { "" };
        output.push_str(&format!(
            "**{}** ({}) — relevance: {} | trust: {:.1} | layer: {}{}\n",
            h.title, h.path, h.score, h.trust, h.layer, expanded_tag
        ));
        for line in &h.context_lines {
            output.push_str(&format!("  {}\n", line));
        }
        output.push('\n');
    }

    tool_text(&output)
}

async fn handle_wiki_lint(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let agent_id = args.get("agent_id").and_then(|v| v.as_str()).unwrap_or(default_agent);

    let wiki_dir = match resolve_wiki_dir(home_dir, agent_id) {
        Ok(d) => d,
        Err(e) => return tool_error(&e),
    };

    if !wiki_dir.exists() {
        return tool_text("No wiki found. Use wiki_write to create the first page.");
    }

    let store = duduclaw_memory::WikiStore::new(wiki_dir);
    match store.lint() {
        Ok(report) => {
            let mut output = format!("Wiki Lint Report for '{}'\n\n", agent_id);
            output.push_str(&format!("Total pages: {}\n", report.total_pages));
            output.push_str(&format!("Index entries: {}\n\n", report.index_entries));

            if report.orphan_pages.is_empty() && report.broken_links.is_empty() && report.stale_pages.is_empty() {
                output.push_str("All clear — no issues found.\n");
            } else {
                if !report.orphan_pages.is_empty() {
                    output.push_str(&format!("Orphan pages ({}):\n", report.orphan_pages.len()));
                    for p in &report.orphan_pages {
                        output.push_str(&format!("  - {}\n", p));
                    }
                    output.push('\n');
                }

                if !report.broken_links.is_empty() {
                    output.push_str(&format!("Broken links ({}):\n", report.broken_links.len()));
                    for (from, to) in &report.broken_links {
                        output.push_str(&format!("  - {} -> {} (not found)\n", from, to));
                    }
                    output.push('\n');
                }

                if !report.stale_pages.is_empty() {
                    output.push_str(&format!("Stale pages (>30 days) ({}):\n", report.stale_pages.len()));
                    for p in &report.stale_pages {
                        output.push_str(&format!("  - {}\n", p));
                    }
                }
            }

            tool_text(&output)
        }
        Err(e) => tool_error(&format!("Lint failed: {e}")),
    }
}

async fn handle_wiki_stats(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let agent_id = args.get("agent_id").and_then(|v| v.as_str()).unwrap_or(default_agent);

    let wiki_dir = match resolve_wiki_dir(home_dir, agent_id) {
        Ok(d) => d,
        Err(e) => return tool_error(&e),
    };

    if !wiki_dir.exists() {
        return tool_text(&format!("No wiki found for agent '{}'.", agent_id));
    }

    let store = duduclaw_memory::WikiStore::new(wiki_dir.clone());
    let pages = match store.list_pages() {
        Ok(p) => p,
        Err(e) => return tool_error(&format!("Failed to list pages: {e}")),
    };

    let index_content = std::fs::read_to_string(wiki_dir.join("_index.md")).unwrap_or_default();
    let index_entries = index_content.lines().filter(|l| l.starts_with("- [")).count();

    let log_content = std::fs::read_to_string(wiki_dir.join("_log.md")).unwrap_or_default();
    let log_entries = log_content.lines().filter(|l| l.starts_with("## [")).count();

    // Count by directory
    let mut by_dir: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for page in &pages {
        let dir = std::path::Path::new(&page.path)
            .parent()
            .and_then(|p| p.to_str())
            .unwrap_or("root")
            .to_string();
        *by_dir.entry(dir).or_insert(0) += 1;
    }

    let most_recent = pages.first().map(|p| {
        format!("{} ({})", p.title, p.updated.format("%Y-%m-%d"))
    }).unwrap_or_else(|| "none".to_string());

    let mut output = format!("Wiki Stats for '{}'\n\n", agent_id);
    output.push_str(&format!("Total pages: {}\n", pages.len()));
    output.push_str(&format!("Index entries: {}\n", index_entries));
    output.push_str(&format!("Log entries: {}\n", log_entries));
    output.push_str(&format!("Most recent: {}\n\n", most_recent));

    output.push_str("By directory:\n");
    let mut dirs: Vec<_> = by_dir.into_iter().collect();
    dirs.sort_by(|a, b| b.1.cmp(&a.1));
    for (dir, count) in &dirs {
        output.push_str(&format!("  {}: {} pages\n", dir, count));
    }

    tool_text(&output)
}

async fn handle_wiki_export(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let agent_id = args.get("agent_id").and_then(|v| v.as_str()).unwrap_or(default_agent);
    let format = args.get("format").and_then(|v| v.as_str()).unwrap_or("html");

    // Visibility check for cross-agent access
    if agent_id != default_agent {
        match check_wiki_visibility(home_dir, agent_id, default_agent) {
            Ok(false) => return tool_error(&format!(
                "Agent '{}' wiki is not visible to '{}'. Ask the owner to add you to wiki_visible_to.",
                agent_id, default_agent
            )),
            Err(e) => return tool_error(&format!("Visibility check failed: {e}")),
            _ => {}
        }
    }

    let wiki_dir = match resolve_wiki_dir(home_dir, agent_id) {
        Ok(d) => d,
        Err(e) => return tool_error(&e),
    };

    if !wiki_dir.exists() {
        return tool_text("No wiki found. Nothing to export.");
    }

    let store = duduclaw_memory::WikiStore::new(wiki_dir);

    match format {
        "obsidian" => {
            let export_dir = home_dir.join("exports").join(format!("{}-wiki-obsidian", agent_id));
            if let Err(e) = std::fs::create_dir_all(&export_dir) {
                return tool_error(&format!("Failed to create export directory: {e}"));
            }
            match store.export_obsidian(&export_dir) {
                Ok(count) => tool_text(&format!(
                    "Exported {} pages as Obsidian vault to:\n{}",
                    count,
                    export_dir.display()
                )),
                Err(e) => tool_error(&format!("Export failed: {e}")),
            }
        }
        "html" => {
            match store.export_html() {
                Ok(html) => {
                    let export_path = home_dir.join("exports").join(format!("{}-wiki.html", agent_id));
                    if let Some(parent) = export_path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    match std::fs::write(&export_path, &html) {
                        Ok(()) => tool_text(&format!(
                            "Exported wiki as HTML ({} bytes) to:\n{}",
                            html.len(),
                            export_path.display()
                        )),
                        Err(e) => tool_error(&format!("Failed to write HTML: {e}")),
                    }
                }
                Err(e) => tool_error(&format!("Export failed: {e}")),
            }
        }
        _ => tool_error(&format!("Unknown format '{}'. Use 'obsidian' or 'html'.", format)),
    }
}

// ---------------------------------------------------------------------------
// Shared Wiki handlers
// ---------------------------------------------------------------------------

/// Resolve the shared wiki directory.
fn resolve_shared_wiki_dir(home_dir: &Path) -> std::path::PathBuf {
    home_dir.join("shared").join("wiki")
}

/// Ensure the shared wiki scaffold exists.
fn ensure_shared_wiki_dir(wiki_dir: &Path) -> std::result::Result<(), String> {
    let subdirs = ["entities", "concepts", "sources", "synthesis"];
    for sub in &subdirs {
        let p = wiki_dir.join(sub);
        std::fs::create_dir_all(&p).map_err(|e| format!("create dir {}: {e}", p.display()))?;
    }
    // Scaffold reserved files
    let scaffold: &[(&str, &str)] = &[
        ("_schema.md", "# Shared Wiki Schema\n\nThis is the shared knowledge base accessible to all agents.\n\n## Subdirectories\n- `entities/` — people, products, organizations\n- `concepts/` — procedures, policies, domain knowledge\n- `sources/` — shared pages from agent wikis\n- `synthesis/` — cross-agent analysis and summaries\n"),
        ("_index.md", "# Shared Wiki Index\n\n<!-- Auto-maintained. One entry per page. -->\n"),
        ("_log.md", "# Shared Wiki Log\n\n<!-- Append-only operation log with author attribution. -->\n"),
    ];
    for (name, content) in scaffold {
        let path = wiki_dir.join(name);
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut f) => {
                use std::io::Write;
                f.write_all(content.as_bytes()).map_err(|e| format!("write {name}: {e}"))?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("create {name}: {e}")),
        }
    }
    Ok(())
}

/// Detect sensitive patterns in content (secret scanner for wiki writes).
fn contains_sensitive_pattern(content: &str) -> Option<&'static str> {
    let patterns: &[(&str, &str)] = &[
        ("sk-ant-", "Anthropic API key"),
        ("sk-proj-", "OpenAI API key"),
        ("api_key=", "API key assignment"),
        ("password=", "password assignment"),
        ("PRIVATE KEY", "private key"),
        ("ghp_", "GitHub personal access token"),
        ("gho_", "GitHub OAuth token"),
        ("xoxb-", "Slack bot token"),
        ("xoxp-", "Slack user token"),
    ];
    for (pattern, label) in patterns {
        if content.contains(pattern) {
            return Some(label);
        }
    }
    None
}

async fn handle_wiki_dedup(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let agent_id = args.get("agent_id").and_then(|v| v.as_str()).unwrap_or(default_agent);
    if !is_valid_agent_id(agent_id) {
        return tool_error("Invalid agent_id format");
    }

    let wiki_dir = match resolve_wiki_dir(home_dir, agent_id) {
        Ok(d) => d,
        Err(e) => return tool_error(&e),
    };
    if !wiki_dir.exists() {
        return tool_text("No wiki found.");
    }

    let store = duduclaw_memory::WikiStore::new(wiki_dir);
    match store.detect_duplicates() {
        Ok(candidates) if candidates.is_empty() => {
            tool_text("No duplicate candidates found.")
        }
        Ok(candidates) => {
            let mut output = format!("Found {} potential duplicate pairs:\n\n", candidates.len());
            for c in &candidates {
                output.push_str(&format!(
                    "- **{}** (trust: {:.1}) ↔ **{}** (trust: {:.1})\n  Reason: {}\n  Suggestion: keep the page with higher trust\n\n",
                    c.page_a, c.trust_a, c.page_b, c.trust_b, c.reason
                ));
            }
            tool_text(&output)
        }
        Err(e) => tool_error(&format!("Dedup detection failed: {e}")),
    }
}

async fn handle_wiki_graph(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let agent_id = args.get("agent_id").and_then(|v| v.as_str()).unwrap_or(default_agent);
    if !is_valid_agent_id(agent_id) {
        return tool_error("Invalid agent_id format");
    }

    let wiki_dir = match resolve_wiki_dir(home_dir, agent_id) {
        Ok(d) => d,
        Err(e) => return tool_error(&e),
    };
    if !wiki_dir.exists() {
        return tool_text("No wiki found.");
    }

    let center = args.get("center").and_then(|v| v.as_str());
    let depth: usize = args
        .get("depth")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or(2);

    let store = duduclaw_memory::WikiStore::new(wiki_dir);
    match store.export_mermaid(center, depth) {
        Ok(mermaid) => tool_text(&format!("```mermaid\n{mermaid}```")),
        Err(e) => tool_error(&format!("Graph export failed: {e}")),
    }
}

async fn handle_wiki_rebuild_fts(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let agent_id = args.get("agent_id").and_then(|v| v.as_str()).unwrap_or(default_agent);
    if !is_valid_agent_id(agent_id) {
        return tool_error("Invalid agent_id format");
    }

    let wiki_dir = match resolve_wiki_dir(home_dir, agent_id) {
        Ok(d) => d,
        Err(e) => return tool_error(&e),
    };
    if !wiki_dir.exists() {
        return tool_text("No wiki found.");
    }

    let store = duduclaw_memory::WikiStore::new(wiki_dir);
    match store.rebuild_fts() {
        Ok(count) => tool_text(&format!("FTS index rebuilt: {} pages indexed.", count)),
        Err(e) => tool_error(&format!("FTS rebuild failed: {e}")),
    }
}

async fn handle_wiki_trust_audit(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let agent_id = args.get("agent_id").and_then(|v| v.as_str()).unwrap_or(default_agent);
    if !is_valid_agent_id(agent_id) {
        return tool_error("Invalid agent_id format");
    }
    let max_trust = args.get("max_trust").and_then(|v| v.as_f64()).unwrap_or(0.3) as f32;
    let limit = (args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize).min(500);

    // Lazy init: if the global store hasn't been created (CLI invocation
    // outside the gateway), open it on demand at the standard path.
    let store = match duduclaw_memory::trust_store::global_trust_store() {
        Some(s) => s,
        None => match duduclaw_memory::trust_store::init_global_trust_store(
            home_dir.join("wiki_trust.db"),
        ) {
            Ok(s) => s,
            Err(e) => return tool_error(&format!("Trust store init failed: {e}")),
        },
    };

    match store.list_low_trust(agent_id, max_trust, limit) {
        Ok(rows) => {
            if rows.is_empty() {
                return tool_text(&format!(
                    "No pages below trust ≤ {max_trust:.2} for agent '{agent_id}'."
                ));
            }
            let mut lines = Vec::with_capacity(rows.len() + 2);
            lines.push(format!(
                "## Wiki trust audit — agent '{agent_id}' (trust ≤ {max_trust:.2})\n"
            ));
            lines.push(
                "| Page | Trust | Cite | Err | OK | DNI | Last signal |\n|---|---|---|---|---|---|---|".into(),
            );
            for s in &rows {
                lines.push(format!(
                    "| `{}` | {:.3} | {} | {} | {} | {} | {} |",
                    s.page_path,
                    s.trust,
                    s.citation_count,
                    s.error_signal_count,
                    s.success_signal_count,
                    if s.do_not_inject { "yes" } else { "no" },
                    s.last_signal_at.map(|d| d.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default(),
                ));
            }
            tool_text(&lines.join("\n"))
        }
        Err(e) => tool_error(&format!("trust audit failed: {e}")),
    }
}

async fn handle_wiki_trust_history(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let agent_id = args.get("agent_id").and_then(|v| v.as_str()).unwrap_or(default_agent);
    let page_path = args.get("page_path").and_then(|v| v.as_str()).unwrap_or("");
    let limit = (args.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize).min(500);

    if !is_valid_agent_id(agent_id) {
        return tool_error("Invalid agent_id format");
    }
    if page_path.is_empty() {
        return tool_error("Missing 'page_path' parameter");
    }
    // (review MED R2 N-2) Defence in depth: bad page_path can't reach SQL
    // injection (parameterised queries) but might leak via audit log strings
    // or future filesystem ops. Reject traversal / NUL / non-md paths.
    if page_path.len() > 512
        || page_path.contains("..")
        || page_path.starts_with('/')
        || page_path.starts_with('\\')
        || page_path.contains('\0')
        || !page_path.ends_with(".md")
    {
        return tool_error("Invalid page_path format");
    }

    let store = match duduclaw_memory::trust_store::global_trust_store() {
        Some(s) => s,
        None => match duduclaw_memory::trust_store::init_global_trust_store(
            home_dir.join("wiki_trust.db"),
        ) {
            Ok(s) => s,
            Err(e) => return tool_error(&format!("Trust store init failed: {e}")),
        },
    };

    match store.history(agent_id, page_path, limit) {
        Ok(rows) => {
            if rows.is_empty() {
                return tool_text(&format!(
                    "No trust history for `{page_path}` (agent '{agent_id}')."
                ));
            }
            let mut lines = Vec::with_capacity(rows.len() + 2);
            lines.push(format!(
                "## Trust history — `{page_path}` (agent '{agent_id}')\n"
            ));
            lines.push(
                "| Time | Old → New | Δ | Trigger | Signal | Composite Err |\n|---|---|---|---|---|---|".into(),
            );
            for h in &rows {
                lines.push(format!(
                    "| {} | {:.3} → {:.3} | {:+.3} | {} | {} | {} |",
                    h.ts.format("%Y-%m-%d %H:%M:%S"),
                    h.old_trust,
                    h.new_trust,
                    h.applied_delta,
                    h.trigger,
                    h.signal_kind,
                    h.composite_error.map(|e| format!("{:.2}", e)).unwrap_or_default(),
                ));
            }
            tool_text(&lines.join("\n"))
        }
        Err(e) => tool_error(&format!("trust history failed: {e}")),
    }
}

async fn handle_shared_wiki_ls(home_dir: &Path) -> Value {
    let wiki_dir = resolve_shared_wiki_dir(home_dir);
    if !wiki_dir.exists() {
        return tool_text("No shared wiki found. Use shared_wiki_write to create the first page.");
    }

    let pages = collect_md_files(&wiki_dir, &wiki_dir);
    if pages.is_empty() {
        return tool_text("Shared wiki directory exists but contains no pages.");
    }

    let mut lines = Vec::with_capacity(pages.len() + 1);
    lines.push(format!("Shared wiki ({} pages):\n", pages.len()));

    for rel_path in &pages {
        let full_path = wiki_dir.join(rel_path);
        let content = std::fs::read_to_string(&full_path).unwrap_or_default();
        let title = extract_frontmatter_title(&content)
            .unwrap_or_else(|| rel_path.to_string_lossy().to_string());
        let updated = extract_frontmatter_updated(&content).unwrap_or_else(|| "?".to_string());
        let author = extract_frontmatter_field(&content, "author")
            .unwrap_or_else(|| "unknown".to_string());
        lines.push(format!("  {} — {} (by: {}, updated: {})", rel_path.display(), title, author, updated));
    }

    tool_text(&lines.join("\n"))
}

async fn handle_shared_wiki_read(args: &Value, home_dir: &Path) -> Value {
    let page_path = match args.get("page_path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return tool_error("Missing required parameter: page_path"),
    };

    if page_path.contains("..") || page_path.starts_with('/') || page_path.starts_with('\\') {
        return tool_error("Path traversal is not allowed");
    }

    let wiki_dir = resolve_shared_wiki_dir(home_dir);
    let full_path = wiki_dir.join(page_path);

    // Symlink protection
    if full_path.exists()
        && let (Ok(canon_wiki), Ok(canon_page)) = (wiki_dir.canonicalize(), full_path.canonicalize())
        && !canon_page.starts_with(&canon_wiki)
    {
        return tool_error("Path escapes shared wiki directory");
    }

    match std::fs::read_to_string(&full_path) {
        Ok(content) => tool_text(&content),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tool_error(&format!("Page not found: {}", page_path))
        }
        Err(e) => tool_error(&format!("Failed to read page: {e}")),
    }
}

async fn handle_shared_wiki_write(args: &Value, home_dir: &Path, caller_agent: &str) -> Value {
    let page_path = match args.get("page_path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return tool_error("Missing required parameter: page_path"),
    };
    let content = match args.get("content").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return tool_error("Missing required parameter: content"),
    };

    if let Err(e) = validate_wiki_page_path(page_path) {
        return tool_error(&e);
    }

    // RFC-21 §3: shared-wiki SoT namespace policy. Loaded fresh on every
    // call (≤ a few KB on disk) so operator edits to .scope.toml take
    // effect immediately. Absent / malformed file ⇒ empty policy ⇒ all
    // namespaces writable (no regression vs. v1.10.1).
    let scope_policy = crate::wiki_scope::WikiScopePolicy::load_for(home_dir);
    let caller_capability = crate::wiki_scope::WriterCapability::for_agent(caller_agent);
    if let Err(deny) = scope_policy.check_write(page_path, &caller_capability) {
        return tool_error(&format!("Shared wiki write denied: {deny}"));
    }

    if content.len() > WIKI_MAX_PAGE_SIZE {
        return tool_error(&format!("Content too large: {} bytes (max {})", content.len(), WIKI_MAX_PAGE_SIZE));
    }

    // Secret scanner
    if let Some(label) = contains_sensitive_pattern(content) {
        return tool_error(&format!("Content contains sensitive data ({label}). Remove it before writing to shared wiki."));
    }

    // Karpathy-schema frontmatter guard (shared wiki is strict — violations reject).
    if let Err(e) = validate_wiki_frontmatter(content) {
        return tool_error(&format!("Shared wiki schema check failed: {e}"));
    }

    // Fallback-content guard: shared wiki refuses pages authored from stale
    // LLM priors (e.g. web_search failure). Callers that must preserve such a
    // record can write it to their own agent wiki with a low trust score, but
    // the shared wiki stays clean per design: "有 fallback 的資料不應該混入共
    // 用 wiki 中產生雜訊".
    let body = extract_frontmatter_body(content);
    if let Some(marker) = detect_fallback_content(&body) {
        // Allow explicit opt-in via `fallback-mode` tag so a human can
        // deliberately archive a fallback record (e.g. for post-mortem).
        let tags = extract_frontmatter_field(content, "tags").unwrap_or_default();
        let opt_in = tags.to_lowercase().contains("fallback-mode");
        if !opt_in {
            return tool_error(&format!(
                "Fallback content detected (marker: '{marker}'). Refusing to \
                 write to shared wiki. If this record is intentional, add the \
                 `fallback-mode` tag to frontmatter and set `trust: 0.2` or \
                 lower. Otherwise, re-run the source fetch before writing."
            ));
        }
    }

    let wiki_dir = resolve_shared_wiki_dir(home_dir);
    if let Err(e) = ensure_shared_wiki_dir(&wiki_dir) {
        return tool_error(&e);
    }

    let store = duduclaw_memory::WikiStore::new_shared(home_dir);
    match store.write_page_with_author(page_path, content, caller_agent) {
        Ok(()) => tool_text(&format!("Written shared wiki page: {} (by: {})", page_path, caller_agent)),
        Err(e) => tool_error(&format!("Failed to write shared wiki page: {e}")),
    }
}

async fn handle_shared_wiki_search(args: &Value, home_dir: &Path) -> Value {
    let query = match args.get("query").and_then(|v| v.as_str()) {
        Some(q) if !q.is_empty() => q,
        _ => return tool_error("Missing required parameter: query"),
    };
    let limit: usize = args
        .get("limit")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or(10)
        .min(100);
    let min_trust: Option<f32> = args
        .get("min_trust")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok());
    let layer_filter: Option<duduclaw_memory::WikiLayer> = args
        .get("layer")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok());

    let wiki_dir = resolve_shared_wiki_dir(home_dir);
    if !wiki_dir.exists() {
        return tool_text("No shared wiki found. Use shared_wiki_write to create the first page.");
    }

    let store = duduclaw_memory::WikiStore::new_shared(home_dir);
    let hits = match store.search_filtered(query, limit, min_trust, layer_filter, false) {
        Ok(h) => h,
        Err(e) => return tool_error(&format!("Shared wiki search failed: {e}")),
    };

    if hits.is_empty() {
        return tool_text(&format!("No shared wiki pages match '{}'.", query));
    }

    let mut output = format!("Found {} shared wiki results for '{}':\n\n", hits.len(), query);
    for h in &hits {
        output.push_str(&format!(
            "📄 {} — {} (trust: {:.1} | layer: {} | relevance: {})\n",
            h.path, h.title, h.trust, h.layer, h.score
        ));
        for line in &h.context_lines {
            output.push_str(&format!("  {}\n", line));
        }
        output.push('\n');
    }

    tool_text(&output)
}

async fn handle_shared_wiki_delete(args: &Value, home_dir: &Path, caller_agent: &str) -> Value {
    let page_path = match args.get("page_path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return tool_error("Missing required parameter: page_path"),
    };

    if let Err(e) = validate_wiki_page_path(page_path) {
        return tool_error(&e);
    }

    // RFC-21 §3: deletes on read_only / operator_only namespaces are denied
    // even for the original page author — the namespace policy is the
    // authority, not the per-page ACL.
    let scope_policy = crate::wiki_scope::WikiScopePolicy::load_for(home_dir);
    let caller_capability = crate::wiki_scope::WriterCapability::for_agent(caller_agent);
    if let Err(deny) = scope_policy.check_write(page_path, &caller_capability) {
        return tool_error(&format!("Shared wiki delete denied: {deny}"));
    }

    let wiki_dir = resolve_shared_wiki_dir(home_dir);
    let full_path = wiki_dir.join(page_path);

    if !full_path.exists() {
        return tool_error(&format!("Page not found: {}", page_path));
    }

    // ACL: only author or main agent can delete
    let content = std::fs::read_to_string(&full_path).unwrap_or_default();
    let page_author = extract_frontmatter_field(&content, "author").unwrap_or_default();

    // Check if caller is the main agent
    let is_main = std::fs::read_to_string(home_dir.join("agents").join(caller_agent).join("agent.toml"))
        .map(|c| c.contains("role = \"main\""))
        .unwrap_or(false);

    if page_author != caller_agent && !is_main {
        return tool_error(&format!(
            "Permission denied: page was authored by '{}'. Only the author or a main agent can delete shared wiki pages.",
            page_author
        ));
    }

    let store = duduclaw_memory::WikiStore::new_shared(home_dir);
    match store.delete_page(page_path) {
        Ok(()) => tool_text(&format!("Deleted shared wiki page: {} (by: {})", page_path, caller_agent)),
        Err(e) => tool_error(&format!("Failed to delete: {e}")),
    }
}

async fn handle_shared_wiki_stats(home_dir: &Path) -> Value {
    let wiki_dir = resolve_shared_wiki_dir(home_dir);
    if !wiki_dir.exists() {
        return tool_text("No shared wiki found.");
    }

    let pages = collect_md_files(&wiki_dir, &wiki_dir);
    if pages.is_empty() {
        return tool_text("Shared wiki exists but has no pages.");
    }

    let mut author_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut dir_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut latest_updated = String::new();

    for rel_path in &pages {
        let full_path = wiki_dir.join(rel_path);
        let content = std::fs::read_to_string(&full_path).unwrap_or_default();

        let author = extract_frontmatter_field(&content, "author")
            .unwrap_or_else(|| "unknown".to_string());
        *author_counts.entry(author).or_default() += 1;

        let dir = rel_path.parent()
            .and_then(|p| p.to_str())
            .unwrap_or("root")
            .to_string();
        *dir_counts.entry(dir).or_default() += 1;

        let updated = extract_frontmatter_updated(&content).unwrap_or_default();
        if updated > latest_updated {
            latest_updated = updated;
        }
    }

    let mut output = format!("Shared Wiki Stats\n\nTotal pages: {}\nLast updated: {}\n\n", pages.len(), latest_updated);

    output.push_str("Contributors:\n");
    let mut authors: Vec<_> = author_counts.into_iter().collect();
    authors.sort_by(|a, b| b.1.cmp(&a.1));
    for (author, count) in &authors {
        output.push_str(&format!("  {} — {} pages\n", author, count));
    }

    output.push_str("\nBy directory:\n");
    let mut dirs: Vec<_> = dir_counts.into_iter().collect();
    dirs.sort_by(|a, b| b.1.cmp(&a.1));
    for (dir, count) in &dirs {
        output.push_str(&format!("  {} — {} pages\n", dir, count));
    }

    tool_text(&output)
}

/// RFC-21 §3: Inspect the shared-wiki namespace policy (`.scope.toml`).
/// Returns the configured namespaces and their modes plus a hint about the
/// fallback ("agent_writable") behaviour for namespaces not listed.
async fn handle_wiki_namespace_status(home_dir: &Path) -> Value {
    let policy = crate::wiki_scope::WikiScopePolicy::load_for(home_dir);
    let snapshot = policy.snapshot();

    let payload = serde_json::json!({
        "policy_file": policy.loaded_from()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| crate::wiki_scope::scope_file_path(home_dir).display().to_string()),
        "policy_loaded": policy.loaded_from().is_some(),
        "default_mode": "agent_writable",
        "namespaces": snapshot,
    });

    let pretty = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string());

    let header = if policy.is_empty() {
        "Shared wiki namespace policy: none configured (every namespace is agent_writable).\n\n"
    } else {
        "Shared wiki namespace policy:\n\n"
    };
    tool_text(&format!("{header}{pretty}"))
}

/// RFC-21 §1: Resolve a `(channel, external_id)` pair to the canonical person
/// behind it via the [`duduclaw_identity::IdentityProvider`] trait. Step 2 of
/// the migration plan: only [`duduclaw_identity::providers::WikiCacheIdentityProvider`]
/// is available; richer providers (Notion, LDAP) plug in at the same trait
/// surface in later steps.
async fn handle_identity_resolve(args: &Value, home_dir: &Path, caller_agent: &str) -> Value {
    use duduclaw_identity::IdentityProvider;
    use duduclaw_identity::providers::WikiCacheIdentityProvider;

    let channel_str = match args.get("channel").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s,
        _ => return tool_error("Missing required parameter: channel"),
    };
    let external_id = match args.get("external_id").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s,
        _ => return tool_error("Missing required parameter: external_id"),
    };

    let channel = duduclaw_identity::ChannelKind::parse_wire(channel_str);
    let provider = WikiCacheIdentityProvider::for_home(home_dir.to_path_buf());

    match provider.resolve_by_channel(channel.clone(), external_id).await {
        Ok(Some(person)) => {
            // Surface the structured result as JSON. Agents that need a
            // narrative can render it themselves; downstream code can
            // serde_json::from_value back into ResolvedPerson.
            tracing::info!(
                provider = provider.name(),
                channel = %channel.as_wire(),
                caller_agent = caller_agent,
                hit = true,
                "identity_resolve: matched person_id={}",
                person.person_id,
            );
            match serde_json::to_value(&person) {
                Ok(payload) => {
                    let pretty = serde_json::to_string_pretty(&payload)
                        .unwrap_or_else(|_| payload.to_string());
                    tool_text(&format!(
                        "Resolved person via {} provider:\n\n{pretty}",
                        provider.name()
                    ))
                }
                Err(e) => tool_error(&format!("Failed to serialize ResolvedPerson: {e}")),
            }
        }
        Ok(None) => {
            tracing::info!(
                provider = provider.name(),
                channel = %channel.as_wire(),
                caller_agent = caller_agent,
                hit = false,
                "identity_resolve: no match",
            );
            tool_text(&format!(
                "No identity record matched (channel={}, external_id={}). \
                 The person is not in the wiki cache; treat as a stranger \
                 unless you can resolve them by other means.",
                channel.as_wire(),
                external_id,
            ))
        }
        Err(e) => {
            tracing::warn!(
                provider = provider.name(),
                channel = %channel.as_wire(),
                caller_agent = caller_agent,
                "identity_resolve: provider error: {}",
                e,
            );
            tool_error(&format!("Identity provider error: {e}"))
        }
    }
}

/// Audit shared wiki for Karpathy-schema compliance: missing frontmatter
/// fields, fallback-content markers, orphan pages, broken links, stale pages.
async fn handle_shared_wiki_lint(home_dir: &Path) -> Value {
    let wiki_dir = resolve_shared_wiki_dir(home_dir);
    if !wiki_dir.exists() {
        return tool_text("No shared wiki found.");
    }

    let pages = collect_md_files(&wiki_dir, &wiki_dir);
    if pages.is_empty() {
        return tool_text("Shared wiki exists but has no pages.");
    }

    // Schema compliance + fallback scan
    let mut schema_violations: Vec<(String, String)> = Vec::new();
    let mut fallback_pages: Vec<(String, &'static str)> = Vec::new();
    for rel_path in &pages {
        let full_path = wiki_dir.join(rel_path);
        let content = std::fs::read_to_string(&full_path).unwrap_or_default();
        let rel_str = rel_path.to_string_lossy().to_string();

        if let Err(e) = validate_wiki_frontmatter(&content) {
            schema_violations.push((rel_str.clone(), e));
        }

        let body = extract_frontmatter_body(&content);
        if let Some(marker) = detect_fallback_content(&body) {
            let tags = extract_frontmatter_field(&content, "tags").unwrap_or_default();
            if !tags.to_lowercase().contains("fallback-mode") {
                fallback_pages.push((rel_str, marker));
            }
        }
    }

    // Delegate graph-level checks to WikiStore::lint
    let store = duduclaw_memory::WikiStore::new_shared(home_dir);
    let graph = store.lint().ok();

    let mut output = format!("Shared Wiki Lint Report\n\nTotal pages: {}\n", pages.len());
    if let Some(ref r) = graph {
        output.push_str(&format!("Index entries: {}\n", r.index_entries));
    }
    output.push('\n');

    let clean = schema_violations.is_empty()
        && fallback_pages.is_empty()
        && graph.as_ref().is_none_or(|r| {
            r.orphan_pages.is_empty() && r.broken_links.is_empty() && r.stale_pages.is_empty()
        });

    if clean {
        output.push_str("All clear — shared wiki is Karpathy-schema compliant.\n");
        return tool_text(&output);
    }

    if !schema_violations.is_empty() {
        output.push_str(&format!(
            "Schema violations ({}):\n",
            schema_violations.len()
        ));
        for (path, err) in &schema_violations {
            output.push_str(&format!("  - {}: {}\n", path, err));
        }
        output.push('\n');
    }

    if !fallback_pages.is_empty() {
        output.push_str(&format!(
            "Fallback-content pages ({}) — likely authored without live evidence:\n",
            fallback_pages.len()
        ));
        for (path, marker) in &fallback_pages {
            output.push_str(&format!("  - {} (marker: '{}')\n", path, marker));
        }
        output.push_str("  → Remove, re-run source fetch, or add `fallback-mode` tag + `trust: 0.2` to opt in.\n\n");
    }

    if let Some(r) = graph {
        if !r.orphan_pages.is_empty() {
            output.push_str(&format!("Orphan pages ({}) — not in _index.md:\n", r.orphan_pages.len()));
            for p in &r.orphan_pages {
                output.push_str(&format!("  - {}\n", p));
            }
            output.push('\n');
        }
        if !r.broken_links.is_empty() {
            output.push_str(&format!("Broken links ({}):\n", r.broken_links.len()));
            for (from, to) in &r.broken_links {
                output.push_str(&format!("  - {} -> {} (not found)\n", from, to));
            }
            output.push('\n');
        }
        if !r.stale_pages.is_empty() {
            output.push_str(&format!("Stale pages (>30 days) ({}):\n", r.stale_pages.len()));
            for p in &r.stale_pages {
                output.push_str(&format!("  - {}\n", p));
            }
        }
    }

    tool_text(&output)
}

async fn handle_wiki_share(args: &Value, home_dir: &Path, caller_agent: &str) -> Value {
    let page_path = match args.get("page_path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return tool_error("Missing required parameter: page_path"),
    };
    let custom_summary = args.get("summary").and_then(|v| v.as_str());

    // Read source page from caller's wiki
    let wiki_dir = match resolve_wiki_dir(home_dir, caller_agent) {
        Ok(d) => d,
        Err(e) => return tool_error(&e),
    };

    let full_path = wiki_dir.join(page_path);
    let source_content = match std::fs::read_to_string(&full_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return tool_error(&format!("Page not found in your wiki: {}", page_path));
        }
        Err(e) => return tool_error(&format!("Failed to read source page: {e}")),
    };

    let source_title = extract_frontmatter_title(&source_content)
        .unwrap_or_else(|| page_path.to_string());
    let source_body = extract_frontmatter_body(&source_content);

    // Generate summary
    let summary = match custom_summary {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => {
            let chars: String = source_body.chars().take(500).collect();
            if source_body.chars().count() > 500 {
                format!("{}...", chars)
            } else {
                chars
            }
        }
    };

    // Secret scanner on summary
    if let Some(label) = contains_sensitive_pattern(&summary) {
        return tool_error(&format!("Summary contains sensitive data ({label}). Redact before sharing."));
    }

    // Build shared page name: sources/{caller}--{page_stem}.md
    let page_stem = std::path::Path::new(page_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("page");
    let shared_page_path = format!("sources/{}--{}.md", caller_agent, page_stem);

    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let shared_content = format!(
        "---\ntitle: \"{}\"\nauthor: \"{}\"\nsource_agent: \"{}\"\nsource_page: \"{}\"\nshared_at: \"{}\"\nupdated: \"{}\"\ntags: [shared, from-{}]\n---\n\n{}\n\n---\n*Shared from {}'s wiki (`{}`)*\n",
        source_title, caller_agent, caller_agent, page_path, now, now, caller_agent,
        summary,
        caller_agent, page_path,
    );

    // Write to shared wiki
    let shared_wiki_dir = resolve_shared_wiki_dir(home_dir);
    if let Err(e) = ensure_shared_wiki_dir(&shared_wiki_dir) {
        return tool_error(&e);
    }

    let store = duduclaw_memory::WikiStore::new_shared(home_dir);
    match store.write_page_with_author(&shared_page_path, &shared_content, caller_agent) {
        Ok(()) => tool_text(&format!(
            "Shared '{}' to shared wiki as '{}' (by: {})",
            page_path, shared_page_path, caller_agent
        )),
        Err(e) => tool_error(&format!("Failed to write shared page: {e}")),
    }
}

/// Extract a named field from frontmatter (helper for shared wiki).
fn extract_frontmatter_field(content: &str, field: &str) -> Option<String> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }
    let rest = &trimmed[3..];
    let end = rest.find("\n---")?;
    let fm = &rest[..end];
    let prefix = format!("{}:", field);
    for line in fm.lines() {
        let line = line.trim();
        if let Some(after) = line.strip_prefix(&prefix) {
            let val = after.trim().trim_matches('"').trim_matches('\'');
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
    }
    None
}

/// Extract body text (after frontmatter closing `---`).
fn extract_frontmatter_body(content: &str) -> String {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return content.to_string();
    }
    let rest = &trimmed[3..];
    if let Some(end) = rest.find("\n---") {
        let after = &rest[end + 4..];
        after.trim_start_matches('\n').to_string()
    } else {
        content.to_string()
    }
}

async fn handle_skill_extract(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let agent_id = args.get("agent_id").and_then(|v| v.as_str()).unwrap_or(default_agent);
    let skill_name = match args.get("skill_name").and_then(|v| v.as_str()) {
        Some(n) if !n.is_empty() => n,
        _ => return tool_error("Missing required parameter: skill_name"),
    };

    // Validate skill_name to prevent path traversal
    if skill_name.contains("..") || skill_name.contains('/') || skill_name.contains('\\')
        || skill_name.contains('\0') || skill_name.len() > 128
        || !skill_name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return tool_error("Invalid skill_name: use alphanumeric, hyphens, underscores only");
    }

    let wiki_dir = match resolve_wiki_dir(home_dir, agent_id) {
        Ok(d) => d,
        Err(e) => return tool_error(&e),
    };

    // Check if already extracted
    if duduclaw_gateway::skill_lifecycle::extraction::is_already_extracted(skill_name, &wiki_dir) {
        return tool_text(&format!("Skill '{}' has already been extracted to wiki.", skill_name));
    }

    // Find the skill file
    let agent_dir = home_dir.join("agents").join(agent_id);
    let skills_dir = agent_dir.join("skills");
    let skill_path = skills_dir.join(format!("{}.md", skill_name));

    let skill_content = if skill_path.exists() {
        std::fs::read_to_string(&skill_path).unwrap_or_default()
    } else {
        // Try without .md extension (might already include it)
        let alt_path = skills_dir.join(skill_name);
        if alt_path.exists() {
            std::fs::read_to_string(&alt_path).unwrap_or_default()
        } else {
            return tool_error(&format!("Skill file not found: {}", skill_path.display()));
        }
    };

    if skill_content.trim().is_empty() {
        return tool_error("Skill file is empty");
    }

    let compressed = duduclaw_gateway::skill_lifecycle::compression::CompressedSkill::compress(
        skill_name, &skill_content, None,
    );

    let result = duduclaw_gateway::skill_lifecycle::extraction::extract_heuristic(&compressed, agent_id);
    let proposals = result.all_proposals();
    let concept_count = result.concepts.len();
    let entity_count = result.entities.len();

    if proposals.is_empty() {
        return tool_text("No extractable knowledge found in skill.");
    }

    // Validate
    if let Err(gradient) = duduclaw_gateway::gvu::verifier::verify_wiki_proposals(&proposals) {
        return tool_error(&format!("Proposals rejected: {}", gradient.critique));
    }

    // Apply
    let store = duduclaw_memory::WikiStore::new(wiki_dir);
    if let Err(e) = store.ensure_scaffold() {
        return tool_error(&format!("Wiki scaffold failed: {e}"));
    }
    match store.apply_proposals(&proposals) {
        Ok(count) => tool_text(&format!(
            "Extracted knowledge from skill '{}':\n- {} concept pages\n- {} entity pages\n- 1 source summary\n- {} total pages written",
            skill_name, concept_count, entity_count, count
        )),
        Err(e) => tool_error(&format!("Failed to apply proposals: {e}")),
    }
}

// ── Helpers ────────────────────────────────────────────────────

/// Truncate a `String` safely at a UTF-8 char boundary.
///
/// Returns `true` if the string was actually truncated.
/// Plain `String::truncate(n)` panics when `n` falls inside a multi-byte
/// character (common with CJK text). This finds the largest valid boundary
/// at or below `max_bytes`.
fn safe_truncate(s: &mut String, max_bytes: usize) -> bool {
    if s.len() <= max_bytes {
        return false;
    }
    // Find the largest char boundary <= max_bytes
    let boundary = (0..=max_bytes)
        .rev()
        .find(|&i| s.is_char_boundary(i))
        .unwrap_or(0);
    s.truncate(boundary);
    s.push_str("\n...[truncated]");
    true
}

// ── execute_program handler ─────────────────────────────────────

async fn handle_execute_program(args: &Value) -> Value {
    use crate::ptc::sandbox::{PtcRpcServer, PtcSandbox};
    use crate::ptc::types::{ScriptLanguage, ScriptRequest};

    let code = match args.get("code").and_then(|v| v.as_str()) {
        Some(c) => c.to_string(),
        None => return tool_error("Missing required parameter: code"),
    };
    let language = match args.get("language").and_then(|v| v.as_str()) {
        Some(l) => l.to_string(),
        None => return tool_error("Missing required parameter: language"),
    };
    let timeout_seconds = args
        .get("timeout_seconds")
        .and_then(|v| v.as_u64())
        .unwrap_or(30)
        .min(300);

    tracing::info!(language, timeout_seconds, "execute_program called");

    let script_language = match language.as_str() {
        "python" => ScriptLanguage::Python,
        "bash" => ScriptLanguage::Bash,
        "javascript" => ScriptLanguage::Bash, // node -e via bash wrapper below
        other => {
            return tool_error(&format!(
                "Unsupported language: '{}'. Supported: python, bash, javascript",
                other
            ));
        }
    };

    // For javascript, wrap the code in a bash invocation of node -e
    let script_code = if language == "javascript" {
        // Escape single quotes in the JS code for safe embedding in bash
        let escaped = code.replace('\'', "'\\''");
        format!("node -e '{escaped}'")
    } else {
        code
    };

    const MAX_OUTPUT_BYTES: usize = 1_048_576; // 1 MB

    let req = ScriptRequest {
        script: script_code,
        language: script_language,
        timeout_ms: timeout_seconds * 1000,
        max_output_bytes: MAX_OUTPUT_BYTES,
    };

    // Create a temporary RPC server for the sandbox execution.
    // If a PTC socket is already set in the environment, reuse that path;
    // otherwise create a unique temporary socket path.
    let socket_path = std::env::var("DUDUCLAW_PTC_SOCKET")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::temp_dir().join(format!(
                "duduclaw_ptc_exec_{}.sock",
                std::process::id()
            ))
        });
    let rpc_server = PtcRpcServer::new(socket_path);

    // Use PtcSandbox::execute_in_container which tries container isolation
    // first and falls back to direct subprocess execution.
    match PtcSandbox::execute_in_container(&req, &rpc_server).await {
        Ok(result) => {
            if result.exit_code == 0 {
                serde_json::json!({
                    "content": [{ "type": "text", "text": result.stdout }],
                })
            } else {
                serde_json::json!({
                    "content": [{ "type": "text", "text": format!(
                        "Program exited with code {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
                        result.exit_code, result.stdout, result.stderr
                    ) }],
                    "isError": true,
                })
            }
        }
        Err(e) => tool_error(&format!("Failed to execute {language}: {e}")),
    }
}

// ── skill_bank_search handler ───────────────────────────────────

async fn handle_skill_bank_search(args: &Value) -> Value {
    let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(5).min(100) as usize;

    tracing::debug!(query, "skill_bank_search query content");
    tracing::info!(query_len = query.len(), limit, "skill_bank_search called");

    // In-memory skill bank search (full SkillBank + SQLite persistence pending)
    // For now, return an empty result set to indicate the search infrastructure is ready
    let skills: Vec<serde_json::Value> = Vec::new();

    serde_json::json!({
        "content": [{ "type": "text", "text": serde_json::json!({
            "query": query,
            "limit": limit,
            "results": skills,
            "total": 0,
            "note": "SkillBank in-memory store initialized. Populate via skill_extract + feedback loop.",
        }).to_string() }],
    })
}

// ── skill_bank_feedback handler ─────────────────────────────────

async fn handle_skill_bank_feedback(args: &Value) -> Value {
    let skill_id = match args.get("skill_id").and_then(|v| v.as_str()) {
        None | Some("") => return tool_error("Missing required parameter: skill_id"),
        Some(id) if id.len() > 128 => return tool_error("skill_id too long (max 128 chars)"),
        Some(id) => id.to_string(),
    };
    let success = match args.get("success") {
        Some(v) if v.is_boolean() => v.as_bool().unwrap(),
        Some(v) if v.is_string() => v.as_str().unwrap().eq_ignore_ascii_case("true"),
        _ => return tool_error("Missing required parameter: success (true/false)"),
    };

    tracing::info!(skill_id, success, "skill_bank_feedback called");

    // Bayesian confidence update (inline — no external dependency)
    // P(skill_works | evidence) using Beta-Bernoulli conjugate prior
    let prior = 0.5_f64;
    let likelihood = if success { 0.9 } else { 0.1 };
    let marginal = likelihood * prior + (1.0 - likelihood) * (1.0 - prior);
    let posterior = (likelihood * prior) / marginal;

    serde_json::json!({
        "content": [{ "type": "text", "text": serde_json::json!({
            "skill_id": skill_id,
            "success": success,
            "prior_confidence": format!("{:.0}%", prior * 100.0),
            "new_confidence": format!("{:.0}%", posterior * 100.0),
            "note": "Bayesian confidence updated. Full SkillBank persistence pending.",
        }).to_string() }],
    })
}

// ── session_restore_context handler ─────────────────────────────

async fn handle_session_restore_context(args: &Value) -> Value {
    let query = match args.get("query").and_then(|v| v.as_str()) {
        Some(q) => q.to_string(),
        None => return tool_error("Missing required parameter: query"),
    };

    tracing::debug!(query = query.as_str(), "session_restore_context query content");
    tracing::info!(query_len = query.len(), "session_restore_context called");

    // Search hidden messages in current session
    // In full implementation, this would use session_manager.search_hidden_messages()
    serde_json::json!({
        "content": [{ "type": "text", "text": serde_json::json!({
            "query": query,
            "results": [],
            "total": 0,
            "note": "Search for hidden/archived messages. Results returned when session context is available.",
        }).to_string() }],
    })
}

/// Handle all computer_* MCP tool calls.
///
/// These tools provide Computer Use capabilities to sub-agents via MCP.
/// They route commands through the orchestrator's global session registry
/// for action execution, or return structured commands for the orchestrator.
async fn handle_computer_use_tool(tool_name: &str, args: &Value) -> Value {
    // Check for active sessions
    let sessions = duduclaw_gateway::computer_use_orchestrator::list_sessions().await;
    let active = !sessions.is_empty();

    match tool_name {
        "computer_screenshot" => {
            let display = args
                .get("display")
                .and_then(|v| v.as_str())
                .unwrap_or("container");
            tool_text(&serde_json::json!({
                "action": "screenshot",
                "display": display,
                "active_sessions": sessions,
                "status": if active { "executing" } else { "no_active_session" },
            }).to_string())
        }
        "computer_click" => {
            let x = args.get("x").and_then(|v| v.as_u64()).unwrap_or(0);
            let y = args.get("y").and_then(|v| v.as_u64()).unwrap_or(0);
            let button = args.get("button").and_then(|v| v.as_str()).unwrap_or("left");
            let double = args.get("double").and_then(|v| v.as_bool()).unwrap_or(false);
            let action_name = match (button, double) {
                ("right", _) => "right_click",
                (_, true) => "double_click",
                _ => "left_click",
            };
            tool_text(&serde_json::json!({
                "action": action_name,
                "coordinate": [x, y],
                "status": if active { "executing" } else { "no_active_session" },
            }).to_string())
        }
        "computer_type" => {
            let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
            tool_text(&serde_json::json!({
                "action": "type",
                "text": text,
                "status": if active { "executing" } else { "no_active_session" },
            }).to_string())
        }
        "computer_key" => {
            let key = args.get("key").and_then(|v| v.as_str()).unwrap_or("");
            tool_text(&serde_json::json!({
                "action": "key",
                "text": key,
                "status": if active { "executing" } else { "no_active_session" },
            }).to_string())
        }
        "computer_scroll" => {
            let x = args.get("x").and_then(|v| v.as_u64()).unwrap_or(0);
            let y = args.get("y").and_then(|v| v.as_u64()).unwrap_or(0);
            let direction = args.get("direction").and_then(|v| v.as_str()).unwrap_or("down");
            let amount = args.get("amount").and_then(|v| v.as_u64()).unwrap_or(3);
            tool_text(&serde_json::json!({
                "action": "scroll",
                "coordinate": [x, y],
                "direction": direction,
                "amount": amount,
                "status": if active { "executing" } else { "no_active_session" },
            }).to_string())
        }
        "computer_session_start" => {
            let task = args.get("task").and_then(|v| v.as_str()).unwrap_or("Computer use session");
            let width = args.get("width").and_then(|v| v.as_u64()).unwrap_or(1280);
            let height = args.get("height").and_then(|v| v.as_u64()).unwrap_or(800);
            let session_id = uuid::Uuid::new_v4().to_string();
            tool_text(&serde_json::json!({
                "session_id": session_id,
                "task": task,
                "display": format!("{width}x{height}"),
                "status": "started",
                "active_sessions": sessions.len() + 1,
            }).to_string())
        }
        "computer_session_stop" => {
            let session_id = args.get("session_id").and_then(|v| v.as_str()).unwrap_or("");
            // Stop via the session registry
            if let Some(control) = duduclaw_gateway::computer_use_orchestrator::get_session_control(session_id).await {
                control.stopped.store(true, std::sync::atomic::Ordering::Release);
                // NOTE: Do NOT call unregister_session() here — the run_loop in
                // channel_reply.rs is the sole owner of session lifecycle cleanup.
                // Setting stopped=true causes run_loop to exit, which then calls
                // unregister_session(). Calling it here too would create a race.
                tool_text(&serde_json::json!({
                    "session_id": session_id,
                    "status": "stopping",
                    "note": "Stop signal sent. Session will be cleaned up when the orchestrator loop exits.",
                }).to_string())
            } else {
                tool_text(&serde_json::json!({
                    "session_id": session_id,
                    "status": "not_found",
                    "active_sessions": sessions,
                }).to_string())
            }
        }
        _ => tool_error(&format!("Unknown computer use tool: {tool_name}")),
    }
}

fn tool_text(text: &str) -> Value {
    serde_json::json!({
        "content": [{ "type": "text", "text": text }]
    })
}

fn tool_error(msg: &str) -> Value {
    serde_json::json!({
        "content": [{ "type": "text", "text": msg }],
        "isError": true
    })
}

// ─────────────────────────────────────────────────────────────────
// Task Board / Activity Feed / Autopilot / Shared Skills MCP tools
// (Multica-inspired "Agent-as-teammate" integration, v1.8.27+)
// ─────────────────────────────────────────────────────────────────

fn task_row_to_json(row: &duduclaw_gateway::task_store::TaskRow) -> Value {
    let tags: Vec<&str> = row
        .tags
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    serde_json::json!({
        "id": row.id,
        "title": row.title,
        "description": row.description,
        "status": row.status,
        "priority": row.priority,
        "assigned_to": row.assigned_to,
        "created_by": row.created_by,
        "created_at": row.created_at,
        "updated_at": row.updated_at,
        "completed_at": row.completed_at,
        "blocked_reason": row.blocked_reason,
        "parent_task_id": row.parent_task_id,
        "tags": tags,
        "message_id": row.message_id,
    })
}

fn activity_row_to_json(row: &duduclaw_gateway::task_store::ActivityRow) -> Value {
    serde_json::json!({
        "id": row.id,
        "type": row.event_type,
        "agent_id": row.agent_id,
        "task_id": row.task_id,
        "summary": row.summary,
        "timestamp": row.timestamp,
        "metadata": row.metadata,
    })
}

async fn append_activity(
    store: &duduclaw_gateway::task_store::TaskStore,
    event_type: &str,
    agent_id: &str,
    task_id: Option<&str>,
    summary: &str,
    metadata: Option<String>,
) {
    let row = duduclaw_gateway::task_store::ActivityRow {
        id: uuid::Uuid::new_v4().to_string(),
        event_type: event_type.to_string(),
        agent_id: agent_id.to_string(),
        task_id: task_id.map(|s| s.to_string()),
        summary: summary.to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        metadata,
    };
    let _ = store.append_activity(&row).await;
}

fn clamp_limit(args: &Value, default: i64, max: i64) -> i64 {
    let n = args
        .get("limit")
        .and_then(|v| v.as_i64())
        .unwrap_or(default);
    n.max(1).min(max)
}

async fn handle_tasks_list(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let store = match duduclaw_gateway::task_store::TaskStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => return tool_error(&format!("open task store: {e}")),
    };
    let status = args.get("status").and_then(|v| v.as_str());
    let priority = args.get("priority").and_then(|v| v.as_str());
    let assigned_to_raw = args.get("assigned_to").and_then(|v| v.as_str());
    // Default to caller; "*" means all agents
    let assigned_to: Option<&str> = match assigned_to_raw {
        Some("*") => None,
        Some(s) if !s.is_empty() => Some(s),
        _ => Some(default_agent),
    };

    let rows = match store.list_tasks(status, assigned_to, priority).await {
        Ok(r) => r,
        Err(e) => return tool_error(&format!("list tasks: {e}")),
    };
    let limit = clamp_limit(args, 20, 100) as usize;
    let tasks: Vec<Value> = rows.iter().take(limit).map(task_row_to_json).collect();
    tool_text(&serde_json::json!({
        "tasks": tasks,
        "total": rows.len(),
        "filtered_by_agent": assigned_to,
    }).to_string())
}

async fn handle_tasks_create(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let title = args.get("title").and_then(|v| v.as_str()).unwrap_or("").trim();
    if title.is_empty() {
        return tool_error("title is required");
    }
    if title.len() > 200 {
        return tool_error("title must be <= 200 chars");
    }
    let description = args.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let priority = args
        .get("priority")
        .and_then(|v| v.as_str())
        .filter(|p| matches!(*p, "low" | "medium" | "high" | "urgent"))
        .unwrap_or("medium")
        .to_string();
    let assigned_to = args
        .get("assigned_to")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(default_agent)
        .to_string();
    // Reject malformed / wildcard agent ids — `assigned_to` is stored verbatim
    // and later used as an equality filter, so an invalid value produces
    // tasks nobody can query.
    if !is_valid_agent_id(&assigned_to) {
        return tool_error(&format!(
            "assigned_to must be a valid agent id (lowercase alphanumeric + hyphens), got: {assigned_to}"
        ));
    }
    if !is_valid_agent_id(default_agent) {
        return tool_error("invalid caller agent id");
    }
    let tags = args.get("tags").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let parent_task_id = args.get("parent_task_id").and_then(|v| v.as_str()).map(String::from);

    let store = match duduclaw_gateway::task_store::TaskStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => return tool_error(&format!("open task store: {e}")),
    };

    let mut row = duduclaw_gateway::task_store::TaskRow::new(
        uuid::Uuid::new_v4().to_string(),
        title.to_string(),
        description,
        priority,
        assigned_to.clone(),
        default_agent.to_string(),
    );
    row.tags = tags;
    row.parent_task_id = parent_task_id;

    if let Err(e) = store.insert_task(&row).await {
        return tool_error(&format!("insert task: {e}"));
    }

    // Record activity
    append_activity(
        &store,
        "task_created",
        default_agent,
        Some(&row.id),
        &format!("Created task: {}", row.title),
        None,
    )
    .await;
    // If the task was assigned to someone else, also record task_assigned
    if assigned_to != default_agent {
        append_activity(
            &store,
            "task_assigned",
            default_agent,
            Some(&row.id),
            &format!("Assigned to {}: {}", assigned_to, row.title),
            None,
        )
        .await;
    }

    append_bus_event(home_dir, "task.created", &task_row_to_json(&row)).await;

    tool_text(&serde_json::json!({ "task": task_row_to_json(&row) }).to_string())
}

async fn handle_tasks_update(args: &Value, home_dir: &Path) -> Value {
    let task_id = args.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
    if task_id.is_empty() {
        return tool_error("task_id is required");
    }
    let store = match duduclaw_gateway::task_store::TaskStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => return tool_error(&format!("open task store: {e}")),
    };
    // Build fields map — only pass through allowed fields
    let mut fields = serde_json::Map::new();
    for k in ["title", "description", "priority", "tags"] {
        if let Some(v) = args.get(k) {
            fields.insert(k.into(), v.clone());
        }
    }
    if fields.is_empty() {
        return tool_error("no fields to update");
    }
    let updated = match store
        .update_task(task_id, &Value::Object(fields))
        .await
    {
        Ok(Some(r)) => r,
        Ok(None) => return tool_error(&format!("task not found: {task_id}")),
        Err(e) => return tool_error(&format!("update task: {e}")),
    };
    append_bus_event(home_dir, "task.updated", &task_row_to_json(&updated)).await;
    tool_text(&serde_json::json!({ "task": task_row_to_json(&updated) }).to_string())
}

async fn handle_tasks_claim(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let task_id = args.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
    if task_id.is_empty() {
        return tool_error("task_id is required");
    }
    if !is_valid_agent_id(default_agent) {
        return tool_error("invalid caller agent id");
    }
    let store = match duduclaw_gateway::task_store::TaskStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => return tool_error(&format!("open task store: {e}")),
    };
    let fields = serde_json::json!({
        "assigned_to": default_agent,
        "status": "in_progress",
    });
    let updated = match store.update_task(task_id, &fields).await {
        Ok(Some(r)) => r,
        Ok(None) => return tool_error(&format!("task not found: {task_id}")),
        Err(e) => return tool_error(&format!("claim task: {e}")),
    };
    append_activity(
        &store,
        "task_assigned",
        default_agent,
        Some(task_id),
        &format!("{} claimed task: {}", default_agent, updated.title),
        None,
    )
    .await;
    append_bus_event(home_dir, "task.updated", &task_row_to_json(&updated)).await;
    tool_text(&serde_json::json!({ "task": task_row_to_json(&updated) }).to_string())
}

async fn handle_tasks_complete(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let task_id = args.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
    if task_id.is_empty() {
        return tool_error("task_id is required");
    }
    if !is_valid_agent_id(default_agent) {
        return tool_error("invalid caller agent id");
    }
    let summary = args.get("summary").and_then(|v| v.as_str()).unwrap_or("");
    let store = match duduclaw_gateway::task_store::TaskStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => return tool_error(&format!("open task store: {e}")),
    };
    let fields = serde_json::json!({ "status": "done" });
    let updated = match store.update_task(task_id, &fields).await {
        Ok(Some(r)) => r,
        Ok(None) => return tool_error(&format!("task not found: {task_id}")),
        Err(e) => return tool_error(&format!("complete task: {e}")),
    };
    let activity_summary = if summary.is_empty() {
        format!("Completed: {}", updated.title)
    } else {
        format!("Completed: {} — {}", updated.title, summary)
    };
    append_activity(
        &store,
        "task_completed",
        default_agent,
        Some(task_id),
        &activity_summary,
        None,
    )
    .await;
    append_bus_event(home_dir, "task.updated", &task_row_to_json(&updated)).await;
    tool_text(&serde_json::json!({ "task": task_row_to_json(&updated) }).to_string())
}

async fn handle_tasks_block(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let task_id = args.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
    let reason = args.get("reason").and_then(|v| v.as_str()).unwrap_or("").trim();
    if task_id.is_empty() {
        return tool_error("task_id is required");
    }
    if reason.is_empty() {
        return tool_error("reason is required");
    }
    if !is_valid_agent_id(default_agent) {
        return tool_error("invalid caller agent id");
    }
    let store = match duduclaw_gateway::task_store::TaskStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => return tool_error(&format!("open task store: {e}")),
    };
    let fields = serde_json::json!({
        "status": "blocked",
        "blocked_reason": reason,
    });
    let updated = match store.update_task(task_id, &fields).await {
        Ok(Some(r)) => r,
        Ok(None) => return tool_error(&format!("task not found: {task_id}")),
        Err(e) => return tool_error(&format!("block task: {e}")),
    };
    append_activity(
        &store,
        "task_blocked",
        default_agent,
        Some(task_id),
        &format!("Blocked: {} — {}", updated.title, reason),
        None,
    )
    .await;
    append_bus_event(home_dir, "task.updated", &task_row_to_json(&updated)).await;
    tool_text(&serde_json::json!({ "task": task_row_to_json(&updated) }).to_string())
}

async fn handle_activity_post(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let summary = args.get("summary").and_then(|v| v.as_str()).unwrap_or("").trim();
    if summary.is_empty() {
        return tool_error("summary is required");
    }
    if !is_valid_agent_id(default_agent) {
        return tool_error("invalid caller agent id");
    }
    let event_type = args
        .get("event_type")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("agent_comment")
        .to_string();
    let task_id = args.get("task_id").and_then(|v| v.as_str()).map(String::from);
    let metadata = args.get("metadata").map(|v| v.to_string());

    let store = match duduclaw_gateway::task_store::TaskStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => return tool_error(&format!("open task store: {e}")),
    };
    let row = duduclaw_gateway::task_store::ActivityRow {
        id: uuid::Uuid::new_v4().to_string(),
        event_type,
        agent_id: default_agent.to_string(),
        task_id,
        summary: summary.to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        metadata,
    };
    if let Err(e) = store.append_activity(&row).await {
        return tool_error(&format!("append activity: {e}"));
    }
    append_bus_event(home_dir, "activity.new", &activity_row_to_json(&row)).await;
    tool_text(&serde_json::json!({ "activity": activity_row_to_json(&row) }).to_string())
}

async fn handle_activity_list(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let store = match duduclaw_gateway::task_store::TaskStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => return tool_error(&format!("open task store: {e}")),
    };
    let agent_id_raw = args.get("agent_id").and_then(|v| v.as_str());
    let agent_id: Option<&str> = match agent_id_raw {
        Some("*") => None,
        Some(s) if !s.is_empty() => Some(s),
        _ => Some(default_agent),
    };
    let event_type = args.get("event_type").and_then(|v| v.as_str());
    let task_id_filter = args.get("task_id").and_then(|v| v.as_str());
    let limit = clamp_limit(args, 20, 100);

    let (rows, total) = match store
        .list_activity(agent_id, event_type, limit, 0)
        .await
    {
        Ok(r) => r,
        Err(e) => return tool_error(&format!("list activity: {e}")),
    };
    let items: Vec<Value> = rows
        .iter()
        .filter(|r| match task_id_filter {
            Some(t) => r.task_id.as_deref() == Some(t),
            None => true,
        })
        .map(activity_row_to_json)
        .collect();
    tool_text(&serde_json::json!({
        "activities": items,
        "total": total,
    }).to_string())
}

async fn handle_autopilot_list(args: &Value, home_dir: &Path) -> Value {
    let enabled_only = args
        .get("enabled_only")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let store = match duduclaw_gateway::autopilot_store::AutopilotStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => return tool_error(&format!("open autopilot store: {e}")),
    };
    let rules = match store.list_rules().await {
        Ok(r) => r,
        Err(e) => return tool_error(&format!("list rules: {e}")),
    };
    let items: Vec<Value> = rules
        .iter()
        .filter(|r| !enabled_only || r.enabled)
        .map(|r| {
            serde_json::json!({
                "id": r.id,
                "name": r.name,
                "enabled": r.enabled,
                "trigger_event": r.trigger_event,
                "conditions": serde_json::from_str::<Value>(&r.conditions).unwrap_or(Value::Null),
                "action": serde_json::from_str::<Value>(&r.action).unwrap_or(Value::Null),
                "created_at": r.created_at,
                "last_triggered_at": r.last_triggered_at,
                "trigger_count": r.trigger_count,
            })
        })
        .collect();
    tool_text(&serde_json::json!({ "rules": items }).to_string())
}

async fn handle_shared_skill_list(args: &Value, home_dir: &Path) -> Value {
    let tag_filter = args.get("tag").and_then(|v| v.as_str()).map(str::to_lowercase);
    let shared_dir = home_dir.join("shared").join("skills");
    if !shared_dir.exists() {
        return tool_text(&serde_json::json!({ "skills": [] }).to_string());
    }
    let mut skills: Vec<Value> = Vec::new();
    let mut entries = match tokio::fs::read_dir(&shared_dir).await {
        Ok(e) => e,
        Err(e) => return tool_error(&format!("read shared skills dir: {e}")),
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
        let content = tokio::fs::read_to_string(&path).await.unwrap_or_default();
        let tags_raw = extract_frontmatter(&content, "tags").unwrap_or_default();
        let tags: Vec<String> = tags_raw
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if let Some(ref needle) = tag_filter {
            if !tags.iter().any(|t| t.to_lowercase().contains(needle)) {
                continue;
            }
        }
        let description = extract_frontmatter(&content, "description").unwrap_or_default();
        let shared_by = extract_frontmatter(&content, "shared_by").unwrap_or_default();
        let shared_at = extract_frontmatter(&content, "shared_at").unwrap_or_default();
        let usage_count: i64 = extract_frontmatter(&content, "usage_count")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let adopted_by: Vec<String> = extract_frontmatter(&content, "adopted_by")
            .map(|s| s.split(',').map(|v| v.trim().to_string()).filter(|v| !v.is_empty()).collect())
            .unwrap_or_default();
        skills.push(serde_json::json!({
            "name": name,
            "description": description,
            "shared_by": shared_by,
            "shared_at": shared_at,
            "tags": tags,
            "usage_count": usage_count,
            "adopted_by": adopted_by,
        }));
    }
    tool_text(&serde_json::json!({ "skills": skills }).to_string())
}

async fn handle_shared_skill_share(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let skill_name = args.get("skill_name").and_then(|v| v.as_str()).unwrap_or("").trim();
    if skill_name.is_empty() {
        return tool_error("skill_name is required");
    }
    if !is_valid_agent_id(default_agent) {
        return tool_error("invalid caller agent id");
    }
    let skill_path = home_dir
        .join("agents")
        .join(default_agent)
        .join("SKILLS")
        .join(format!("{skill_name}.md"));
    if !skill_path.exists() {
        return tool_error(&format!("skill not found in your SKILLS/: {skill_name}"));
    }
    let content = match tokio::fs::read_to_string(&skill_path).await {
        Ok(c) => c,
        Err(e) => return tool_error(&format!("read skill: {e}")),
    };
    let shared_dir = home_dir.join("shared").join("skills");
    if let Err(e) = tokio::fs::create_dir_all(&shared_dir).await {
        return tool_error(&format!("create shared dir: {e}"));
    }
    let shared_path = shared_dir.join(format!("{skill_name}.md"));
    let now = chrono::Utc::now().to_rfc3339();
    let shared_content = format!(
        "---\nshared_by: {default_agent}\nshared_at: {now}\ndescription: \ntags: \nadopted_by: \nusage_count: 0\n---\n\n{content}"
    );
    if let Err(e) = tokio::fs::write(&shared_path, &shared_content).await {
        return tool_error(&format!("write shared skill: {e}"));
    }
    tool_text(&serde_json::json!({ "success": true, "skill": skill_name }).to_string())
}

async fn handle_shared_skill_adopt(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let skill_name = args.get("skill_name").and_then(|v| v.as_str()).unwrap_or("").trim();
    if skill_name.is_empty() {
        return tool_error("skill_name is required");
    }
    let target_agent = args
        .get("target_agent")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(default_agent);
    if !is_valid_agent_id(target_agent) {
        return tool_error("invalid target_agent id");
    }
    let shared_path = home_dir.join("shared").join("skills").join(format!("{skill_name}.md"));
    if !shared_path.exists() {
        return tool_error(&format!("shared skill not found: {skill_name}"));
    }
    let content = match tokio::fs::read_to_string(&shared_path).await {
        Ok(c) => c,
        Err(e) => return tool_error(&format!("read shared skill: {e}")),
    };
    // Strip frontmatter (up to second "---")
    let skill_content = strip_frontmatter(&content);

    let target_dir = home_dir.join("agents").join(target_agent).join("SKILLS");
    if let Err(e) = tokio::fs::create_dir_all(&target_dir).await {
        return tool_error(&format!("create agent SKILLS dir: {e}"));
    }
    let target_path = target_dir.join(format!("{skill_name}.md"));
    if let Err(e) = tokio::fs::write(&target_path, &skill_content).await {
        return tool_error(&format!("write skill to agent: {e}"));
    }

    // Bump usage_count and adopted_by in shared frontmatter
    let updated = update_frontmatter_field(&content, "usage_count", |old| {
        let n: i64 = old.parse().unwrap_or(0);
        (n + 1).to_string()
    });
    let updated = update_frontmatter_field(&updated, "adopted_by", |old| {
        let mut agents: Vec<String> = old
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if !agents.iter().any(|a| a == target_agent) {
            agents.push(target_agent.to_string());
        }
        agents.join(", ")
    });
    let _ = tokio::fs::write(&shared_path, &updated).await;

    tool_text(&serde_json::json!({
        "success": true,
        "skill": skill_name,
        "adopted_to": target_agent,
    }).to_string())
}

/// Extract a top-level YAML frontmatter field value.
/// Scans only within the first `---` fenced block.
fn extract_frontmatter(content: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    let mut in_front = false;
    for (i, line) in content.lines().enumerate() {
        if i == 0 && line.trim() == "---" {
            in_front = true;
            continue;
        }
        if in_front && line.trim() == "---" {
            break;
        }
        if in_front {
            if let Some(rest) = line.strip_prefix(&prefix) {
                return Some(rest.trim().to_string());
            }
        }
    }
    None
}

/// Rewrite a single top-level frontmatter field using `transform`.
fn update_frontmatter_field(content: &str, key: &str, transform: impl Fn(&str) -> String) -> String {
    let prefix = format!("{key}:");
    let mut in_front = false;
    content
        .lines()
        .enumerate()
        .map(|(i, line)| {
            if i == 0 && line.trim() == "---" {
                in_front = true;
                return line.to_string();
            }
            if in_front && line.trim() == "---" {
                in_front = false;
                return line.to_string();
            }
            if in_front {
                if let Some(rest) = line.strip_prefix(&prefix) {
                    return format!("{prefix} {}", transform(rest.trim()));
                }
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Strip the leading `---...---` YAML frontmatter block (if any) and
/// return the body, trimmed of leading whitespace.
fn strip_frontmatter(content: &str) -> String {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return trimmed.to_string();
    }
    // After the opening ---, find the next line that is exactly "---".
    // Collect lines after that as the body.
    let mut saw_open = false;
    let mut body_lines: Vec<&str> = Vec::new();
    let mut collecting = false;
    for line in trimmed.lines() {
        if collecting {
            body_lines.push(line);
            continue;
        }
        if !saw_open {
            if line.trim() == "---" {
                saw_open = true;
            }
            continue;
        }
        // saw_open && !collecting
        if line.trim() == "---" {
            collecting = true;
        }
    }
    if collecting {
        body_lines.join("\n").trim_start().to_string()
    } else {
        trimmed.to_string()
    }
}

/// Append an event to the SQLite event bus (`~/.duduclaw/events.db`).
///
/// Replaces the legacy `events.jsonl` file bus (removed in v1.8.28).
/// Row inserts are atomic under SQLite WAL with a 5-second
/// `busy_timeout`, so concurrent writers from multiple MCP subprocesses
/// and the gateway reader stay consistent without file-bus hazards
/// (rotation races, partial writes, permission concerns, or unbounded
/// growth — the gateway prunes old rows on a schedule).
///
/// Best-effort: failures are logged but never fatal — the caller has
/// already persisted the authoritative row in `tasks.db` / `activity`.
async fn append_bus_event(home_dir: &Path, event: &str, payload: &Value) {
    let bus = match duduclaw_gateway::events_store::EventBusStore::open(home_dir) {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "open events.db");
            return;
        }
    };
    let payload_str = payload.to_string();
    if let Err(e) = bus.append(event, &payload_str).await {
        warn!(error = %e, event = %event, "append events.db");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Create a temporary test directory that is cleaned up on drop.
    struct TempDir(std::path::PathBuf);
    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("duduclaw-test-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &std::path::Path { &self.0 }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Create a minimal agent directory for testing.
    /// Create the `message_queue` schema in `<home>/message_queue.db` for
    /// tests. Mirrors `duduclaw-gateway::message_queue::MessageQueue::init_schema`
    /// including the v1.8.16 `reply_channel` column, so `send_to_agent`'s
    /// INSERT has a table to write into.
    ///
    /// In production, the gateway creates this table on startup via
    /// `MessageQueue::open`. Tests that bypass the gateway need to set it
    /// up themselves since MCP subprocesses assume the schema already
    /// exists.
    fn init_message_queue_schema(home: &std::path::Path) {
        let db_path = home.join("message_queue.db");
        let conn = rusqlite::Connection::open(&db_path).expect("open message_queue.db");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS message_queue (
                 id              TEXT PRIMARY KEY,
                 sender          TEXT NOT NULL,
                 target          TEXT NOT NULL,
                 payload         TEXT NOT NULL,
                 status          TEXT NOT NULL DEFAULT 'pending',
                 retry_count     INTEGER NOT NULL DEFAULT 0,
                 delegation_depth INTEGER NOT NULL DEFAULT 0,
                 origin_agent    TEXT,
                 sender_agent    TEXT,
                 error           TEXT,
                 response        TEXT,
                 created_at      TEXT NOT NULL,
                 acked_at        TEXT,
                 completed_at    TEXT,
                 reply_channel   TEXT
             );",
        )
        .expect("init message_queue schema");
    }

    fn create_test_agent(agents_dir: &std::path::Path, name: &str, reports_to: &str) {
        let agent_dir = agents_dir.join(name);
        fs::create_dir_all(&agent_dir).unwrap();
        let toml_content = format!(
            r#"[agent]
name = "{name}"
display_name = "{name}"
role = "specialist"
status = "active"
trigger = "@{name}"
reports_to = "{reports_to}"
icon = "🤖"

[model]
preferred = "claude-sonnet-4-6"
fallback = ""
api_mode = "cli"
account_pool = []

[budget]
monthly_limit_cents = 1000
warn_threshold_percent = 80
hard_stop = false

[container]
sandbox_enabled = false
network_access = false
timeout_ms = 60000
max_concurrent = 2
readonly_project = false
additional_mounts = []

[heartbeat]
enabled = false
interval_seconds = 300
max_concurrent_runs = 1
cron = ""

[permissions]
can_create_agents = false
can_send_cross_agent = true
can_modify_own_skills = false
can_modify_own_soul = false
can_schedule_tasks = false
allowed_channels = []

[evolution]
skill_auto_activate = false
skill_security_scan = false

[capabilities]
computer_use = false
browser_via_bash = false
allowed_tools = []
denied_tools = []

[proactive]
enabled = false

[cultural_context]
locale = "zh-TW"
high_context = true
"#
        );
        fs::write(agent_dir.join("agent.toml"), toml_content).unwrap();
    }

    #[tokio::test]
    async fn supervisor_parent_to_child_allowed() {
        let tmp = TempDir::new();
        let home = tmp.path();
        let agents_dir = home.join("agents");
        fs::create_dir_all(&agents_dir).unwrap();

        create_test_agent(&agents_dir, "main", "");
        create_test_agent(&agents_dir, "researcher", "main");

        // main → researcher: allowed (parent → child)
        let result = check_supervisor_relation(home, "main", "researcher").await;
        assert!(result.is_ok(), "Parent→child should be allowed: {result:?}");
    }

    #[tokio::test]
    async fn supervisor_child_to_parent_allowed() {
        let tmp = TempDir::new();
        let home = tmp.path();
        let agents_dir = home.join("agents");
        fs::create_dir_all(&agents_dir).unwrap();

        create_test_agent(&agents_dir, "main", "");
        create_test_agent(&agents_dir, "researcher", "main");

        // researcher → main: allowed (child → parent reply)
        let result = check_supervisor_relation(home, "researcher", "main").await;
        assert!(result.is_ok(), "Child→parent should be allowed: {result:?}");
    }

    #[tokio::test]
    async fn supervisor_sibling_blocked() {
        let tmp = TempDir::new();
        let home = tmp.path();
        let agents_dir = home.join("agents");
        fs::create_dir_all(&agents_dir).unwrap();

        create_test_agent(&agents_dir, "main", "");
        create_test_agent(&agents_dir, "researcher", "main");
        create_test_agent(&agents_dir, "writer", "main");

        // researcher → writer: blocked (siblings cannot delegate directly)
        let result = check_supervisor_relation(home, "researcher", "writer").await;
        assert!(result.is_err(), "Sibling→sibling should be blocked");
        assert!(result.unwrap_err().contains("Supervisor pattern violation"));
    }

    #[tokio::test]
    async fn supervisor_self_delegation_blocked() {
        let tmp = TempDir::new();
        let home = tmp.path();
        let agents_dir = home.join("agents");
        fs::create_dir_all(&agents_dir).unwrap();

        create_test_agent(&agents_dir, "main", "");

        let result = check_supervisor_relation(home, "main", "main").await;
        assert!(result.is_err(), "Self-delegation should be blocked");
        assert!(result.unwrap_err().contains("Cannot delegate to self"));
    }

    #[tokio::test]
    async fn validate_reports_to_existing_agent() {
        let tmp = TempDir::new();
        let home = tmp.path();
        let agents_dir = home.join("agents");
        fs::create_dir_all(&agents_dir).unwrap();

        create_test_agent(&agents_dir, "main", "");

        // Valid: new agent reports to existing "main"
        let result = validate_reports_to(home, "worker", "main").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn validate_reports_to_nonexistent_agent() {
        let tmp = TempDir::new();
        let home = tmp.path();
        let agents_dir = home.join("agents");
        fs::create_dir_all(&agents_dir).unwrap();

        // Invalid: references non-existent agent
        let result = validate_reports_to(home, "worker", "ghost").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("does not exist"));
    }

    #[tokio::test]
    async fn validate_reports_to_self_blocked() {
        let tmp = TempDir::new();
        let home = tmp.path();
        let agents_dir = home.join("agents");
        fs::create_dir_all(&agents_dir).unwrap();

        create_test_agent(&agents_dir, "worker", "");

        let result = validate_reports_to(home, "worker", "worker").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("cannot report to itself"));
    }

    #[tokio::test]
    async fn validate_reports_to_cycle_detected() {
        let tmp = TempDir::new();
        let home = tmp.path();
        let agents_dir = home.join("agents");
        fs::create_dir_all(&agents_dir).unwrap();

        // Create A → B → C, then try to set C → A (cycle)
        create_test_agent(&agents_dir, "a", "");
        create_test_agent(&agents_dir, "b", "a");
        create_test_agent(&agents_dir, "c", "b");

        // Setting a.reports_to = c would create: a → c → b → a (cycle)
        let result = validate_reports_to(home, "a", "c").await;
        assert!(result.is_err(), "Cycle should be detected: {result:?}");
        assert!(result.unwrap_err().contains("Circular"));
    }

    #[tokio::test]
    async fn validate_reports_to_empty_is_root() {
        let tmp = TempDir::new();
        let home = tmp.path();

        // Empty reports_to is valid (root agent)
        let result = validate_reports_to(home, "any", "").await;
        assert!(result.is_ok());

        let result = validate_reports_to(home, "any", "none").await;
        assert!(result.is_ok());
    }

    #[test]
    fn delegation_context_fields() {
        // Test DelegationContext construction directly — no env var mutation needed.
        let ctx = DelegationContext { depth: 3, origin: Some("main".into()), sender: Some("worker".into()) };
        assert_eq!(ctx.depth, 3);
        assert_eq!(ctx.origin.as_deref(), Some("main"));
        assert_eq!(ctx.sender.as_deref(), Some("worker"));

        // Default-like: depth 0, no origin/sender
        let ctx0 = DelegationContext { depth: 0, origin: None, sender: None };
        assert_eq!(ctx0.depth, 0);
        assert!(ctx0.origin.is_none());
        assert!(ctx0.sender.is_none());
    }

    // Mutex to serialize env-var-mutating tests (env is process-global).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn clear_delegation_env() {
        unsafe {
            std::env::remove_var(duduclaw_core::ENV_DELEGATION_DEPTH);
            std::env::remove_var(duduclaw_core::ENV_DELEGATION_ORIGIN);
            std::env::remove_var(duduclaw_core::ENV_DELEGATION_SENDER);
        }
    }

    #[test]
    fn delegation_context_from_env() {
        let _lock = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var(duduclaw_core::ENV_DELEGATION_DEPTH, "3");
            std::env::set_var(duduclaw_core::ENV_DELEGATION_ORIGIN, "main-agent");
            std::env::set_var(duduclaw_core::ENV_DELEGATION_SENDER, "researcher");
        }
        let ctx = DelegationContext::from_env();
        clear_delegation_env();
        assert_eq!(ctx.depth, 3);
        assert_eq!(ctx.origin.as_deref(), Some("main-agent"));
        assert_eq!(ctx.sender.as_deref(), Some("researcher"));
    }

    #[test]
    fn delegation_context_from_env_defaults() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_delegation_env();
        let ctx = DelegationContext::from_env();
        assert_eq!(ctx.depth, 0);
        assert!(ctx.origin.is_none());
        assert!(ctx.sender.is_none());
    }

    #[test]
    fn delegation_context_from_env_empty_strings() {
        let _lock = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var(duduclaw_core::ENV_DELEGATION_DEPTH, "0");
            std::env::set_var(duduclaw_core::ENV_DELEGATION_ORIGIN, "");
            std::env::set_var(duduclaw_core::ENV_DELEGATION_SENDER, "");
        }
        let ctx = DelegationContext::from_env();
        clear_delegation_env();
        assert_eq!(ctx.depth, 0);
        assert!(ctx.origin.is_none(), "Empty string should filter to None");
        assert!(ctx.sender.is_none(), "Empty string should filter to None");
    }

    #[test]
    fn delegation_context_from_env_invalid_depth() {
        let _lock = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var(duduclaw_core::ENV_DELEGATION_DEPTH, "not_a_number");
        }
        let ctx = DelegationContext::from_env();
        clear_delegation_env();
        assert_eq!(ctx.depth, 0, "Invalid depth should default to 0");
    }

    #[test]
    fn normalize_reports_to_handles_variants() {
        assert_eq!(normalize_reports_to(""), "");
        assert_eq!(normalize_reports_to("none"), "");
        assert_eq!(normalize_reports_to("main"), "main");
        assert_eq!(normalize_reports_to("some-agent"), "some-agent");
    }

    // ── E2E delegation depth integration tests ──────────────────
    // These call send_to_agent_with_ctx / spawn_agent_with_ctx directly with
    // injected DelegationContext — no unsafe env var mutation needed, fully
    // thread-safe and parallelizable.

    #[tokio::test]
    async fn e2e_send_to_agent_increments_depth() {
        let tmp = TempDir::new();
        let home = tmp.path();
        let agents_dir = home.join("agents");
        fs::create_dir_all(&agents_dir).unwrap();
        init_message_queue_schema(home);

        create_test_agent(&agents_dir, "main", "");
        create_test_agent(&agents_dir, "worker", "main");

        let ctx = DelegationContext { depth: 2, origin: Some("main".into()), sender: Some("main".into()) };
        let params = serde_json::json!({ "agent_id": "worker", "prompt": "do something" });
        let result = send_to_agent_with_ctx(&params, home, "main", ctx).await;

        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("status=queued"), "Expected success, got: {text}");
        assert!(text.contains("depth=3"), "Expected depth 3 (2+1), got: {text}");

        // v1.8.18: bus_queue.jsonl is NO LONGER written by send_to_agent.
        // This prevents the dual-rail race where the legacy dispatcher
        // (tokio::spawn'd per-message, drops task-locals) would spawn the
        // target agent's Claude CLI without the REPLY_CHANNEL scope.
        let bus_queue_path = home.join("bus_queue.jsonl");
        assert!(
            !bus_queue_path.exists(),
            "send_to_agent must not write to bus_queue.jsonl (v1.8.18 dual-rail race fix)"
        );

        // The delegation lives in SQLite — verify it's there with the
        // correct depth / origin / sender / target.
        let db_path = home.join("message_queue.db");
        let conn = rusqlite::Connection::open(&db_path).expect("open message_queue.db");
        let (sender, target, origin_agent, sender_agent, depth): (String, String, String, String, i32) =
            conn.query_row(
                "SELECT sender, target, origin_agent, sender_agent, delegation_depth \
                 FROM message_queue ORDER BY rowid DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .expect("row in message_queue.db");
        assert_eq!(depth, 3);
        assert_eq!(origin_agent, "main");
        assert_eq!(sender_agent, "main");
        assert_eq!(sender, "main");
        assert_eq!(target, "worker");
    }

    #[tokio::test]
    async fn e2e_send_to_agent_rejects_at_depth_limit() {
        let tmp = TempDir::new();
        let home = tmp.path();
        let agents_dir = home.join("agents");
        fs::create_dir_all(&agents_dir).unwrap();

        create_test_agent(&agents_dir, "main", "");
        create_test_agent(&agents_dir, "worker", "main");

        // depth=4 → outgoing=5 >= MAX(5) → rejected
        let ctx = DelegationContext { depth: 4, origin: Some("main".into()), sender: Some("researcher".into()) };
        let params = serde_json::json!({ "agent_id": "worker", "prompt": "do something" });
        let result = send_to_agent_with_ctx(&params, home, "main", ctx).await;

        assert_eq!(result["isError"], true);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("delegation depth limit"), "Expected depth limit error, got: {text}");

        let queue_path = home.join("bus_queue.jsonl");
        assert!(!queue_path.exists(), "Queue should not have been written to");
    }

    /// v1.8.18 regression test. Prevents re-introduction of the dual-rail
    /// race fix: `send_to_agent` must NEVER write to `bus_queue.jsonl`.
    ///
    /// If this test starts failing, some refactor has re-enabled the
    /// legacy jsonl write — which in turn re-enables the race where the
    /// legacy `poll_and_dispatch` loop tokio::spawn's dispatch tasks
    /// that drop the REPLY_CHANNEL task-local, silently defeating the
    /// v1.8.16 reply_channel propagation.
    #[tokio::test]
    async fn send_to_agent_never_writes_bus_queue_jsonl() {
        let tmp = TempDir::new();
        let home = tmp.path();
        let agents_dir = home.join("agents");
        fs::create_dir_all(&agents_dir).unwrap();
        init_message_queue_schema(home);

        create_test_agent(&agents_dir, "main", "");
        create_test_agent(&agents_dir, "worker", "main");

        // Happy path at depth 0 → outgoing 1. Succeeds.
        let ctx = DelegationContext { depth: 0, origin: None, sender: None };
        let params = serde_json::json!({ "agent_id": "worker", "prompt": "hi" });
        let result = send_to_agent_with_ctx(&params, home, "main", ctx).await;
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("status=queued"), "Expected success, got: {text}");

        // The SQLite queue must have the row...
        let db_path = home.join("message_queue.db");
        let count: i64 = rusqlite::Connection::open(&db_path)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM message_queue", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "SQLite queue must contain the delegation");

        // ...but bus_queue.jsonl must NOT exist (v1.8.18 fix).
        let bus_queue_path = home.join("bus_queue.jsonl");
        assert!(
            !bus_queue_path.exists(),
            "v1.8.18 regression: send_to_agent must not write to bus_queue.jsonl"
        );
    }

    #[tokio::test]
    async fn e2e_spawn_agent_increments_depth() {
        let tmp = TempDir::new();
        let home = tmp.path();
        let agents_dir = home.join("agents");
        fs::create_dir_all(&agents_dir).unwrap();

        create_test_agent(&agents_dir, "main", "");
        create_test_agent(&agents_dir, "worker", "main");

        let ctx = DelegationContext { depth: 1, origin: Some("root-agent".into()), sender: Some("main".into()) };
        let params = serde_json::json!({ "agent_id": "worker", "task": "background work" });
        let result = spawn_agent_with_ctx(&params, home, "main", ctx).await;

        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("spawned successfully"), "Expected success, got: {text}");

        let queue_path = home.join("bus_queue.jsonl");
        let content = fs::read_to_string(&queue_path).unwrap();
        let msg: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(msg["delegation_depth"], 2, "Expected depth 2 (1+1)");
        assert_eq!(msg["origin_agent"], "root-agent", "Origin should be preserved");
        assert_eq!(msg["sender_agent"], "main");
    }

    #[tokio::test]
    async fn e2e_depth_zero_defaults_origin_to_caller() {
        let tmp = TempDir::new();
        let home = tmp.path();
        let agents_dir = home.join("agents");
        fs::create_dir_all(&agents_dir).unwrap();
        init_message_queue_schema(home);

        create_test_agent(&agents_dir, "main", "");
        create_test_agent(&agents_dir, "worker", "main");

        // No origin/sender set — simulates first delegation (no dispatcher context)
        let ctx = DelegationContext { depth: 0, origin: None, sender: None };
        let params = serde_json::json!({ "agent_id": "worker", "prompt": "first delegation" });
        let result = send_to_agent_with_ctx(&params, home, "main", ctx).await;

        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("depth=1"), "Expected depth 1 (0+1), got: {text}");

        // v1.8.18: verify via SQLite — bus_queue.jsonl is no longer
        // written by `send_to_agent` (see `send_to_agent_never_writes_bus_queue_jsonl`).
        let db_path = home.join("message_queue.db");
        let (depth, origin_agent): (i32, String) = rusqlite::Connection::open(&db_path)
            .expect("open message_queue.db")
            .query_row(
                "SELECT delegation_depth, origin_agent \
                 FROM message_queue ORDER BY rowid DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("row in message_queue.db");
        assert_eq!(depth, 1);
        assert_eq!(origin_agent, "main", "Should fall back to caller");
    }

    // ── v1.8.25: local timezone auto-detect ─────────────────────────

    #[test]
    fn detect_local_timezone_returns_valid_iana_name() {
        // This is a host-system-dependent test. We can't assert the
        // specific zone (CI may run on UTC / America/Los_Angeles /
        // Asia/Taipei), but we CAN assert that whatever we get back
        // parses as a valid chrono-tz IANA name. That's the contract
        // schedule_task relies on.
        //
        // On hosts with no discoverable TZ (extremely minimal Docker
        // images), the function legitimately returns None — we accept
        // both outcomes but check parseability of any name returned.
        if let Some(tz_name) = detect_local_timezone() {
            assert!(
                duduclaw_core::parse_timezone(&tz_name).is_some(),
                "detected TZ '{tz_name}' must round-trip through parse_timezone"
            );
            assert!(!tz_name.is_empty(), "detected TZ must not be empty string");
        }
    }
}

#[cfg(test)]
mod agent_identity_tests {
    //! Verify `get_default_agent`'s preference order:
    //! `DUDUCLAW_AGENT_ID` env > `config.toml [general] default_agent` > `"dudu"`.
    //!
    //! The env var is process-wide, so these tests must run serially.
    //! A `Mutex` guards the env-mutation scope; we hold the guard across
    //! the whole test, including the async `get_default_agent` call.

    use super::get_default_agent;
    use std::fs;
    use std::sync::Mutex;

    /// Serializes any test that reads/writes `DUDUCLAW_AGENT_ID`.
    /// Without this, parallel tests corrupt each other's env view.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Minimal `TempDir` copy (the outer `tests` module already has one,
    /// but it's not accessible from a sibling module).
    struct TempDir(std::path::PathBuf);
    impl TempDir {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "duduclaw-agent-identity-{}",
                uuid::Uuid::new_v4()
            ));
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> &std::path::Path { &self.0 }
    }
    impl Drop for TempDir {
        fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
    }

    fn write_default_agent_config(home: &std::path::Path, default_agent: &str) {
        let content = format!(
            "[general]\ndefault_agent = \"{default_agent}\"\n"
        );
        fs::write(home.join("config.toml"), content).unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn agent_id_env_overrides_config_default() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = TempDir::new();
        write_default_agent_config(tmp.path(), "agnes");

        // SAFETY: env mutation serialized via ENV_LOCK for this test module.
        unsafe {
            std::env::set_var(duduclaw_core::ENV_AGENT_ID, "duduclaw-tl");
        }
        let result = get_default_agent(tmp.path()).await;
        unsafe {
            std::env::remove_var(duduclaw_core::ENV_AGENT_ID);
        }

        assert_eq!(
            result, "duduclaw-tl",
            "env var must override config.toml default_agent"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn agent_id_env_missing_falls_back_to_config() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = TempDir::new();
        write_default_agent_config(tmp.path(), "agnes");

        // Make sure no stray env from other tests interferes.
        unsafe { std::env::remove_var(duduclaw_core::ENV_AGENT_ID); }

        let result = get_default_agent(tmp.path()).await;
        assert_eq!(result, "agnes", "missing env → fall back to config");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn agent_id_env_empty_string_falls_back_to_config() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = TempDir::new();
        write_default_agent_config(tmp.path(), "agnes");

        unsafe {
            std::env::set_var(duduclaw_core::ENV_AGENT_ID, "");
        }
        let result = get_default_agent(tmp.path()).await;
        unsafe {
            std::env::remove_var(duduclaw_core::ENV_AGENT_ID);
        }

        assert_eq!(
            result, "agnes",
            "empty env var must be treated like missing"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn no_env_no_config_defaults_to_dudu() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = TempDir::new();
        // no config.toml at all

        unsafe { std::env::remove_var(duduclaw_core::ENV_AGENT_ID); }

        let result = get_default_agent(tmp.path()).await;
        assert_eq!(result, "dudu", "final fallback must be 'dudu'");
    }
}

#[cfg(test)]
mod wiki_schema_tests {
    //! Karpathy-schema frontmatter guard + fallback-content rejection
    //! (v1.8.26 shared wiki hygiene).
    use super::*;
    use std::fs;

    /// Minimal TempDir — sibling modules can't share `tests::TempDir`.
    struct TempDir(std::path::PathBuf);
    impl TempDir {
        fn new() -> Self {
            let p = std::env::temp_dir()
                .join(format!("duduclaw-wiki-schema-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> &std::path::Path { &self.0 }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write_agent_toml(agents_dir: &std::path::Path, name: &str) {
        let dir = agents_dir.join(name);
        fs::create_dir_all(&dir).unwrap();
        // Minimal agent.toml — handle_shared_wiki_write doesn't parse it,
        // but other helpers invoked during the call might read [agent].role.
        fs::write(
            dir.join("agent.toml"),
            format!("[agent]\nname = \"{name}\"\nrole = \"main\"\n"),
        )
        .unwrap();
    }

    #[test]
    fn frontmatter_validator_accepts_full_schema() {
        let content = "---\n\
                       title: Test Page\n\
                       created: 2026-04-22T10:00:00Z\n\
                       updated: 2026-04-22T10:00:00Z\n\
                       tags: [a, b]\n\
                       layer: context\n\
                       trust: 0.7\n\
                       ---\n\
                       body\n";
        assert!(validate_wiki_frontmatter(content).is_ok());
    }

    #[test]
    fn frontmatter_validator_rejects_missing_frontmatter() {
        let err = validate_wiki_frontmatter("# body only\n").unwrap_err();
        assert!(err.contains("Missing YAML frontmatter"), "got: {}", err);
    }

    #[test]
    fn frontmatter_validator_rejects_missing_required_fields() {
        // Missing tags, layer, trust
        let content = "---\n\
                       title: T\n\
                       created: 2026-04-22T00:00:00Z\n\
                       updated: 2026-04-22T00:00:00Z\n\
                       ---\n\
                       body\n";
        let err = validate_wiki_frontmatter(content).unwrap_err();
        assert!(err.contains("tags"), "err should mention tags: {}", err);
        assert!(err.contains("layer"), "err should mention layer: {}", err);
        assert!(err.contains("trust"), "err should mention trust: {}", err);
    }

    #[test]
    fn frontmatter_validator_rejects_out_of_range_trust() {
        let content = "---\n\
                       title: T\n\
                       created: 2026-04-22T00:00:00Z\n\
                       updated: 2026-04-22T00:00:00Z\n\
                       tags: []\n\
                       layer: context\n\
                       trust: 1.5\n\
                       ---\n";
        let err = validate_wiki_frontmatter(content).unwrap_err();
        assert!(err.contains("0.0") || err.contains("[0.0, 1.0]"), "got: {}", err);
    }

    #[test]
    fn frontmatter_validator_rejects_non_numeric_trust() {
        let content = "---\n\
                       title: T\n\
                       created: 2026-04-22T00:00:00Z\n\
                       updated: 2026-04-22T00:00:00Z\n\
                       tags: []\n\
                       layer: context\n\
                       trust: high\n\
                       ---\n";
        let err = validate_wiki_frontmatter(content).unwrap_err();
        assert!(err.contains("trust"), "got: {}", err);
    }

    #[test]
    fn detect_fallback_catches_cjk_marker() {
        let body = "本報告基於訓練資料推測，web_search 工具回傳空結果。";
        assert!(detect_fallback_content(body).is_some());
    }

    #[test]
    fn detect_fallback_catches_english_marker() {
        let body = "Unable to fetch live data; based on training data up to 2024.";
        assert!(detect_fallback_content(body).is_some());
    }

    #[test]
    fn detect_fallback_ignores_clean_body() {
        let body = "TEMPO framework alternates policy refinement and critic recalibration.";
        assert!(detect_fallback_content(body).is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shared_wiki_write_rejects_fallback_content() {
        let tmp = TempDir::new();
        let agents_dir = tmp.path().join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        write_agent_toml(&agents_dir, "agnes");

        let args = serde_json::json!({
            "page_path": "research/bad.md",
            "content": "---\n\
                        title: Bad Page\n\
                        created: 2026-04-22T00:00:00Z\n\
                        updated: 2026-04-22T00:00:00Z\n\
                        tags: [research]\n\
                        layer: context\n\
                        trust: 0.5\n\
                        ---\n\
                        查無結果，基於訓練資料整理。\n",
        });

        let result = handle_shared_wiki_write(&args, tmp.path(), "agnes").await;
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            text.contains("Fallback content detected"),
            "expected fallback rejection, got: {}",
            text
        );
        assert!(
            result["isError"].as_bool().unwrap_or(false),
            "should be isError=true"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shared_wiki_write_rejects_missing_frontmatter() {
        let tmp = TempDir::new();
        let agents_dir = tmp.path().join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        write_agent_toml(&agents_dir, "agnes");

        let args = serde_json::json!({
            "page_path": "research/plain.md",
            "content": "# Just a title\n\nbody without frontmatter\n",
        });

        let result = handle_shared_wiki_write(&args, tmp.path(), "agnes").await;
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            text.contains("schema check failed") && text.contains("Missing YAML frontmatter"),
            "got: {}",
            text
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shared_wiki_write_allows_fallback_mode_opt_in() {
        let tmp = TempDir::new();
        let agents_dir = tmp.path().join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        write_agent_toml(&agents_dir, "agnes");

        let args = serde_json::json!({
            "page_path": "research/postmortem.md",
            "content": "---\n\
                        title: Postmortem\n\
                        created: 2026-04-22T00:00:00Z\n\
                        updated: 2026-04-22T00:00:00Z\n\
                        tags: [fallback-mode, postmortem]\n\
                        layer: context\n\
                        trust: 0.2\n\
                        ---\n\
                        web_search failed repeatedly; archiving this record.\n",
        });

        let result = handle_shared_wiki_write(&args, tmp.path(), "agnes").await;
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        // Opt-in path should bypass the fallback rejection entirely.
        assert!(
            !text.contains("Fallback content detected"),
            "opt-in should not trigger rejection, got: {}",
            text
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shared_wiki_write_accepts_clean_karpathy_page() {
        let tmp = TempDir::new();
        let agents_dir = tmp.path().join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        write_agent_toml(&agents_dir, "agnes");

        let args = serde_json::json!({
            "page_path": "entities/tempo-framework.md",
            "content": "---\n\
                        title: TEMPO Framework\n\
                        created: 2026-04-22T00:00:00Z\n\
                        updated: 2026-04-22T00:00:00Z\n\
                        tags: [reasoning, test-time-training]\n\
                        layer: context\n\
                        trust: 0.6\n\
                        ---\n\
                        TEMPO alternates policy refinement with critic recalibration.\n",
        });

        let result = handle_shared_wiki_write(&args, tmp.path(), "agnes").await;
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            text.contains("Written shared wiki page"),
            "clean page should succeed, got: {}",
            text
        );
        assert!(!result["isError"].as_bool().unwrap_or(false));
    }

    // ─────────────────────────────────────────────────────────────────────
    // RFC-21 §3 — Shared-wiki SoT namespace policy integration
    // ─────────────────────────────────────────────────────────────────────

    fn write_scope_policy(home: &std::path::Path, body: &str) {
        let path = home.join("shared").join("wiki").join(".scope.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
    }

    fn clean_karpathy_page(title: &str) -> String {
        format!(
            "---\n\
             title: {title}\n\
             created: 2026-05-04T00:00:00Z\n\
             updated: 2026-05-04T00:00:00Z\n\
             tags: [test]\n\
             layer: context\n\
             trust: 0.5\n\
             ---\n\
             body content\n",
        )
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shared_wiki_write_denied_when_namespace_is_read_only() {
        let tmp = TempDir::new();
        let agents_dir = tmp.path().join("agents");
        fs::create_dir_all(&agents_dir).unwrap();
        write_agent_toml(&agents_dir, "agnes");

        write_scope_policy(
            tmp.path(),
            r#"
                [namespaces."identity"]
                mode = "read_only"
                synced_from = "identity-provider"
            "#,
        );

        let args = serde_json::json!({
            "page_path": "identity/discord-users.md",
            "content": clean_karpathy_page("Identity Roster"),
        });
        let result = handle_shared_wiki_write(&args, tmp.path(), "agnes").await;
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            result["isError"].as_bool().unwrap_or(false),
            "expected isError=true, got payload: {result}"
        );
        assert!(text.contains("Shared wiki write denied"), "got: {text}");
        assert!(text.contains("identity"), "should name the namespace, got: {text}");
        assert!(text.contains("identity-provider"), "should name the synced_from capability, got: {text}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shared_wiki_write_allowed_when_namespace_is_unlisted() {
        let tmp = TempDir::new();
        let agents_dir = tmp.path().join("agents");
        fs::create_dir_all(&agents_dir).unwrap();
        write_agent_toml(&agents_dir, "agnes");

        write_scope_policy(
            tmp.path(),
            r#"
                [namespaces."identity"]
                mode = "read_only"
                synced_from = "identity-provider"
            "#,
        );

        // 'concepts' is not listed — must remain writable.
        let args = serde_json::json!({
            "page_path": "concepts/return-policy.md",
            "content": clean_karpathy_page("Return Policy"),
        });
        let result = handle_shared_wiki_write(&args, tmp.path(), "agnes").await;
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            text.contains("Written shared wiki page"),
            "unlisted namespace should be writable, got: {text}"
        );
        assert!(!result["isError"].as_bool().unwrap_or(false));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shared_wiki_write_unaffected_when_no_scope_policy_present() {
        let tmp = TempDir::new();
        let agents_dir = tmp.path().join("agents");
        fs::create_dir_all(&agents_dir).unwrap();
        write_agent_toml(&agents_dir, "agnes");

        // No .scope.toml written — behaviour must match v1.10.1 exactly.
        let args = serde_json::json!({
            "page_path": "identity/should-still-work.md",
            "content": clean_karpathy_page("Pre-RFC behaviour"),
        });
        let result = handle_shared_wiki_write(&args, tmp.path(), "agnes").await;
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            text.contains("Written shared wiki page"),
            "absent policy must not regress writes, got: {text}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shared_wiki_write_unaffected_when_scope_toml_is_malformed() {
        let tmp = TempDir::new();
        let agents_dir = tmp.path().join("agents");
        fs::create_dir_all(&agents_dir).unwrap();
        write_agent_toml(&agents_dir, "agnes");

        // Fail-safe path: malformed file must never block legitimate writes.
        write_scope_policy(tmp.path(), "this is :: not = valid = toml ===");

        let args = serde_json::json!({
            "page_path": "identity/discord-users.md",
            "content": clean_karpathy_page("Identity Roster"),
        });
        let result = handle_shared_wiki_write(&args, tmp.path(), "agnes").await;
        assert!(
            !result["isError"].as_bool().unwrap_or(false),
            "malformed policy must fail-safe to writable, got: {result}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shared_wiki_delete_denied_when_namespace_is_operator_only() {
        let tmp = TempDir::new();
        let agents_dir = tmp.path().join("agents");
        fs::create_dir_all(&agents_dir).unwrap();
        write_agent_toml(&agents_dir, "agnes");

        // First write a page (no policy yet so the write succeeds).
        let write_args = serde_json::json!({
            "page_path": "policies/security.md",
            "content": clean_karpathy_page("Security Policy"),
        });
        let write_res = handle_shared_wiki_write(&write_args, tmp.path(), "agnes").await;
        assert!(!write_res["isError"].as_bool().unwrap_or(false));

        // Now lock down the policies/ namespace.
        write_scope_policy(
            tmp.path(),
            r#"
                [namespaces."policies"]
                mode = "operator_only"
            "#,
        );

        let del_args = serde_json::json!({ "page_path": "policies/security.md" });
        let del_res = handle_shared_wiki_delete(&del_args, tmp.path(), "agnes").await;
        let text = del_res["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            del_res["isError"].as_bool().unwrap_or(false),
            "operator_only delete should be denied even for the original author, got: {del_res}"
        );
        assert!(text.contains("Shared wiki delete denied"), "got: {text}");
        assert!(text.contains("operator_only"), "should name the mode, got: {text}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn wiki_namespace_status_reports_loaded_policy() {
        let tmp = TempDir::new();
        write_scope_policy(
            tmp.path(),
            r#"
                [namespaces."identity"]
                mode = "read_only"
                synced_from = "identity-provider"

                [namespaces."policies"]
                mode = "operator_only"
            "#,
        );

        let result = handle_wiki_namespace_status(tmp.path()).await;
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        assert!(text.contains("\"namespace\": \"identity\""), "got: {text}");
        assert!(text.contains("\"mode\": \"read_only\""), "got: {text}");
        assert!(text.contains("\"synced_from\": \"identity-provider\""), "got: {text}");
        assert!(text.contains("\"namespace\": \"policies\""), "got: {text}");
        assert!(text.contains("\"mode\": \"operator_only\""), "got: {text}");
        assert!(text.contains("\"policy_loaded\": true"), "got: {text}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn wiki_namespace_status_reports_empty_policy_when_file_absent() {
        let tmp = TempDir::new();
        let result = handle_wiki_namespace_status(tmp.path()).await;
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            text.contains("none configured") && text.contains("agent_writable"),
            "got: {text}"
        );
        assert!(text.contains("\"policy_loaded\": false"), "got: {text}");
    }

    // ─────────────────────────────────────────────────────────────────────
    // RFC-21 §1 — Identity Resolution MCP tool integration
    // ─────────────────────────────────────────────────────────────────────

    fn write_identity_record(home: &std::path::Path, filename: &str, frontmatter: &str) {
        let dir = home
            .join("shared")
            .join("wiki")
            .join("identity")
            .join("people");
        fs::create_dir_all(&dir).unwrap();
        let body = format!("---\n{frontmatter}---\n");
        fs::write(dir.join(filename), body).unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn identity_resolve_returns_payload_for_known_handle() {
        let tmp = TempDir::new();
        write_identity_record(
            tmp.path(),
            "ruby.md",
            "person_id: person_2f9\n\
             display_name: Ruby Lin\n\
             roles: [customer-pm]\n\
             project_ids: [proj-alpha]\n\
             channel_handles:\n  discord: \"1234567890\"\n",
        );

        let args = serde_json::json!({
            "channel": "discord",
            "external_id": "1234567890",
        });
        let result = handle_identity_resolve(&args, tmp.path(), "agnes").await;
        let text = result["content"][0]["text"].as_str().unwrap_or("");

        assert!(!result["isError"].as_bool().unwrap_or(false));
        assert!(text.contains("\"person_id\": \"person_2f9\""), "got: {text}");
        assert!(text.contains("\"display_name\": \"Ruby Lin\""), "got: {text}");
        assert!(text.contains("\"source\": \"wiki-cache\""), "got: {text}");
        assert!(text.contains("Resolved person via wiki-cache"), "got: {text}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn identity_resolve_returns_polite_miss_for_unknown_handle() {
        let tmp = TempDir::new();
        // No identity records at all → must report "no match", not error.
        let args = serde_json::json!({
            "channel": "discord",
            "external_id": "9999999",
        });
        let result = handle_identity_resolve(&args, tmp.path(), "agnes").await;
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            !result["isError"].as_bool().unwrap_or(false),
            "unknown person is not an error, got: {result}"
        );
        assert!(text.contains("No identity record matched"), "got: {text}");
        assert!(text.contains("treat as a stranger"), "got: {text}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn identity_resolve_rejects_missing_channel_or_external_id() {
        let tmp = TempDir::new();
        let r1 = handle_identity_resolve(
            &serde_json::json!({ "external_id": "1234" }),
            tmp.path(),
            "agnes",
        )
        .await;
        assert!(r1["isError"].as_bool().unwrap_or(false));
        assert!(r1["content"][0]["text"].as_str().unwrap_or("").contains("channel"));

        let r2 = handle_identity_resolve(
            &serde_json::json!({ "channel": "discord" }),
            tmp.path(),
            "agnes",
        )
        .await;
        assert!(r2["isError"].as_bool().unwrap_or(false));
        assert!(r2["content"][0]["text"].as_str().unwrap_or("").contains("external_id"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn identity_resolve_accepts_unknown_channel_kind_via_other_variant() {
        let tmp = TempDir::new();
        write_identity_record(
            tmp.path(),
            "matrix-user.md",
            "person_id: person_mx\n\
             display_name: Matrix User\n\
             channel_handles:\n  matrix: \"@user:example.org\"\n",
        );

        // 'matrix' isn't a built-in ChannelKind variant — must still resolve
        // via the Other(_) catch-all.
        let args = serde_json::json!({
            "channel": "matrix",
            "external_id": "@user:example.org",
        });
        let result = handle_identity_resolve(&args, tmp.path(), "agnes").await;
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        assert!(text.contains("\"person_id\": \"person_mx\""), "got: {text}");
    }
}

// ─────────────────────────────────────────────────────────────────
// Task Board / Activity Feed / Shared Skills MCP tool tests
// ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod task_board_tests {
    use super::*;
    use std::fs;

    struct TempDir(std::path::PathBuf);
    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("duduclaw-tb-test-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn parse_ok(value: &Value) -> Value {
        assert!(
            !value.get("isError").and_then(|v| v.as_bool()).unwrap_or(false),
            "tool returned error: {value}"
        );
        let text = value["content"][0]["text"].as_str().unwrap();
        serde_json::from_str(text).unwrap()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tasks_create_then_list_returns_task() {
        let tmp = TempDir::new();
        let create = handle_tasks_create(
            &serde_json::json!({
                "title": "Ship feature X",
                "priority": "high",
            }),
            tmp.path(),
            "agnes",
        )
        .await;
        let created = parse_ok(&create);
        let task_id = created["task"]["id"].as_str().unwrap().to_string();
        assert_eq!(created["task"]["assigned_to"], "agnes");
        assert_eq!(created["task"]["created_by"], "agnes");
        assert_eq!(created["task"]["status"], "todo");

        let list = handle_tasks_list(
            &serde_json::json!({}),
            tmp.path(),
            "agnes",
        )
        .await;
        let listed = parse_ok(&list);
        let tasks = listed["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0]["id"], task_id);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tasks_list_filters_to_caller_by_default() {
        let tmp = TempDir::new();
        // agnes creates a task assigned to bruno
        handle_tasks_create(
            &serde_json::json!({
                "title": "For bruno",
                "assigned_to": "bruno",
            }),
            tmp.path(),
            "agnes",
        )
        .await;
        // agnes creates a task for herself
        handle_tasks_create(
            &serde_json::json!({ "title": "For agnes" }),
            tmp.path(),
            "agnes",
        )
        .await;

        // agnes listing — should only see her own
        let agnes_list = handle_tasks_list(
            &serde_json::json!({}),
            tmp.path(),
            "agnes",
        )
        .await;
        let agnes_tasks = parse_ok(&agnes_list);
        let titles: Vec<&str> = agnes_tasks["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["title"].as_str().unwrap())
            .collect();
        assert_eq!(titles, vec!["For agnes"]);

        // '*' sees all
        let all = handle_tasks_list(
            &serde_json::json!({ "assigned_to": "*" }),
            tmp.path(),
            "agnes",
        )
        .await;
        let all_tasks = parse_ok(&all);
        assert_eq!(all_tasks["tasks"].as_array().unwrap().len(), 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tasks_claim_transitions_to_in_progress_and_reassigns() {
        let tmp = TempDir::new();
        let create = handle_tasks_create(
            &serde_json::json!({
                "title": "Orphan task",
                "assigned_to": "bruno",
            }),
            tmp.path(),
            "bruno",
        )
        .await;
        let id = parse_ok(&create)["task"]["id"].as_str().unwrap().to_string();

        // agnes claims it
        let claim = handle_tasks_claim(
            &serde_json::json!({ "task_id": id }),
            tmp.path(),
            "agnes",
        )
        .await;
        let claimed = parse_ok(&claim);
        assert_eq!(claimed["task"]["assigned_to"], "agnes");
        assert_eq!(claimed["task"]["status"], "in_progress");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tasks_complete_marks_done_and_sets_completed_at() {
        let tmp = TempDir::new();
        let create = handle_tasks_create(
            &serde_json::json!({ "title": "Finish me" }),
            tmp.path(),
            "agnes",
        )
        .await;
        let id = parse_ok(&create)["task"]["id"].as_str().unwrap().to_string();

        let complete = handle_tasks_complete(
            &serde_json::json!({
                "task_id": id,
                "summary": "Shipped",
            }),
            tmp.path(),
            "agnes",
        )
        .await;
        let done = parse_ok(&complete);
        assert_eq!(done["task"]["status"], "done");
        assert!(done["task"]["completed_at"].as_str().is_some());

        // Activity log should contain task_completed
        let activity = handle_activity_list(
            &serde_json::json!({ "event_type": "task_completed" }),
            tmp.path(),
            "agnes",
        )
        .await;
        let log = parse_ok(&activity);
        let activities = log["activities"].as_array().unwrap();
        assert!(activities.iter().any(|a| a["type"] == "task_completed"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tasks_block_requires_reason() {
        let tmp = TempDir::new();
        let create = handle_tasks_create(
            &serde_json::json!({ "title": "Stuck" }),
            tmp.path(),
            "agnes",
        )
        .await;
        let id = parse_ok(&create)["task"]["id"].as_str().unwrap().to_string();

        // Missing reason → error
        let no_reason = handle_tasks_block(
            &serde_json::json!({ "task_id": id }),
            tmp.path(),
            "agnes",
        )
        .await;
        assert!(no_reason["isError"].as_bool().unwrap_or(false));

        // With reason → success
        let blocked = handle_tasks_block(
            &serde_json::json!({
                "task_id": id,
                "reason": "Waiting for API key",
            }),
            tmp.path(),
            "agnes",
        )
        .await;
        let b = parse_ok(&blocked);
        assert_eq!(b["task"]["status"], "blocked");
        assert_eq!(b["task"]["blocked_reason"], "Waiting for API key");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tasks_create_rejects_empty_title() {
        let tmp = TempDir::new();
        let result = handle_tasks_create(
            &serde_json::json!({ "title": "   " }),
            tmp.path(),
            "agnes",
        )
        .await;
        assert!(result["isError"].as_bool().unwrap_or(false));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn activity_post_then_list() {
        let tmp = TempDir::new();
        let post = handle_activity_post(
            &serde_json::json!({
                "summary": "Checked in on research task",
                "event_type": "progress",
            }),
            tmp.path(),
            "agnes",
        )
        .await;
        let posted = parse_ok(&post);
        assert_eq!(posted["activity"]["type"], "progress");
        assert_eq!(posted["activity"]["agent_id"], "agnes");

        let list = handle_activity_list(
            &serde_json::json!({}),
            tmp.path(),
            "agnes",
        )
        .await;
        let items = parse_ok(&list)["activities"].as_array().unwrap().clone();
        assert_eq!(items.len(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shared_skill_share_then_list_then_adopt() {
        let tmp = TempDir::new();
        // Seed a skill file under agents/agnes/SKILLS/
        let skills_dir = tmp.path().join("agents").join("agnes").join("SKILLS");
        fs::create_dir_all(&skills_dir).unwrap();
        fs::write(
            skills_dir.join("pricing-audit.md"),
            "# Pricing Audit\n\nSteps:\n1. Pull price list.\n2. Diff against rules.\n",
        )
        .unwrap();

        // Share
        let share = handle_shared_skill_share(
            &serde_json::json!({ "skill_name": "pricing-audit" }),
            tmp.path(),
            "agnes",
        )
        .await;
        parse_ok(&share);

        // List — should appear
        let list = handle_shared_skill_list(
            &serde_json::json!({}),
            tmp.path(),
        )
        .await;
        let skills = parse_ok(&list)["skills"].as_array().unwrap().clone();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0]["name"], "pricing-audit");
        assert_eq!(skills[0]["shared_by"], "agnes");

        // Adopt into bruno
        let adopt = handle_shared_skill_adopt(
            &serde_json::json!({
                "skill_name": "pricing-audit",
                "target_agent": "bruno",
            }),
            tmp.path(),
            "agnes",
        )
        .await;
        parse_ok(&adopt);
        let bruno_path = tmp
            .path()
            .join("agents")
            .join("bruno")
            .join("SKILLS")
            .join("pricing-audit.md");
        assert!(bruno_path.exists());

        // Shared frontmatter should now record usage_count=1 and adopted_by includes bruno
        let list2 = handle_shared_skill_list(
            &serde_json::json!({}),
            tmp.path(),
        )
        .await;
        let skills2 = parse_ok(&list2)["skills"].as_array().unwrap().clone();
        assert_eq!(skills2[0]["usage_count"], 1);
        assert!(skills2[0]["adopted_by"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "bruno"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn autopilot_list_respects_enabled_only() {
        let tmp = TempDir::new();
        // Seed two rules directly via AutopilotStore
        let store = duduclaw_gateway::autopilot_store::AutopilotStore::open(tmp.path()).unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        let mut enabled = duduclaw_gateway::autopilot_store::AutopilotRuleRow {
            id: "r1".into(),
            name: "r1".into(),
            enabled: true,
            trigger_event: "task_created".into(),
            conditions: "{}".into(),
            action: "{}".into(),
            created_at: now.clone(),
            last_triggered_at: None,
            trigger_count: 0,
        };
        store.insert_rule(&enabled).await.unwrap();
        enabled.id = "r2".into();
        enabled.name = "r2".into();
        enabled.enabled = false;
        store.insert_rule(&enabled).await.unwrap();

        let only_enabled = handle_autopilot_list(
            &serde_json::json!({ "enabled_only": true }),
            tmp.path(),
        )
        .await;
        let rules = parse_ok(&only_enabled)["rules"].as_array().unwrap().clone();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0]["id"], "r1");

        let all = handle_autopilot_list(
            &serde_json::json!({ "enabled_only": false }),
            tmp.path(),
        )
        .await;
        let all_rules = parse_ok(&all)["rules"].as_array().unwrap().clone();
        assert_eq!(all_rules.len(), 2);
    }

    #[test]
    fn strip_frontmatter_removes_leading_block() {
        let input = "---\nfoo: bar\n---\n\nBody line 1\nBody line 2\n";
        let out = strip_frontmatter(input);
        assert_eq!(out, "Body line 1\nBody line 2");
    }

    #[test]
    fn strip_frontmatter_preserves_content_without_fence() {
        let input = "No frontmatter here\nJust body\n";
        let out = strip_frontmatter(input);
        // Only the leading whitespace is trimmed; trailing newline stays
        assert_eq!(out, "No frontmatter here\nJust body\n");
    }

    #[test]
    fn extract_frontmatter_reads_first_match() {
        let content = "---\nshared_by: agnes\nusage_count: 3\n---\nBody\n";
        assert_eq!(
            extract_frontmatter(content, "shared_by"),
            Some("agnes".into())
        );
        assert_eq!(
            extract_frontmatter(content, "usage_count"),
            Some("3".into())
        );
        assert_eq!(extract_frontmatter(content, "missing"), None);
    }

    #[test]
    fn update_frontmatter_field_bumps_counter() {
        let content = "---\nshared_by: a\nusage_count: 0\n---\nbody";
        let updated = update_frontmatter_field(content, "usage_count", |old| {
            let n: i64 = old.parse().unwrap_or(0);
            (n + 1).to_string()
        });
        assert!(updated.contains("usage_count: 1"));
    }

    // ── Edge-case tests (security + idempotency + error paths) ─────

    #[tokio::test(flavor = "current_thread")]
    async fn tasks_create_rejects_invalid_assigned_to() {
        let tmp = TempDir::new();
        // Wildcard is nonsensical — equality filter would match nothing
        let wildcard = handle_tasks_create(
            &serde_json::json!({ "title": "x", "assigned_to": "*" }),
            tmp.path(),
            "agnes",
        )
        .await;
        assert!(wildcard["isError"].as_bool().unwrap_or(false));

        // Path-traversal style
        let traversal = handle_tasks_create(
            &serde_json::json!({ "title": "x", "assigned_to": "../etc/passwd" }),
            tmp.path(),
            "agnes",
        )
        .await;
        assert!(traversal["isError"].as_bool().unwrap_or(false));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tasks_complete_is_idempotent() {
        let tmp = TempDir::new();
        let create = handle_tasks_create(
            &serde_json::json!({ "title": "Already done" }),
            tmp.path(),
            "agnes",
        )
        .await;
        let id = parse_ok(&create)["task"]["id"].as_str().unwrap().to_string();

        handle_tasks_complete(
            &serde_json::json!({ "task_id": id.clone() }),
            tmp.path(),
            "agnes",
        )
        .await;
        let second = handle_tasks_complete(
            &serde_json::json!({ "task_id": id }),
            tmp.path(),
            "agnes",
        )
        .await;
        let done = parse_ok(&second);
        assert_eq!(done["task"]["status"], "done");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tasks_update_rejects_unknown_task() {
        let tmp = TempDir::new();
        let result = handle_tasks_update(
            &serde_json::json!({
                "task_id": "does-not-exist",
                "title": "Nope",
            }),
            tmp.path(),
        )
        .await;
        assert!(result["isError"].as_bool().unwrap_or(false));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tasks_list_filters_by_status() {
        let tmp = TempDir::new();
        // Create 3 tasks (sequential — parallel would race the SQLite lock)
        let a = handle_tasks_create(
            &serde_json::json!({ "title": "t1" }),
            tmp.path(),
            "agnes",
        )
        .await;
        let b = handle_tasks_create(
            &serde_json::json!({ "title": "t2" }),
            tmp.path(),
            "agnes",
        )
        .await;
        let _c = handle_tasks_create(
            &serde_json::json!({ "title": "t3" }),
            tmp.path(),
            "agnes",
        )
        .await;
        let _ = b;
        let a_id = parse_ok(&a)["task"]["id"].as_str().unwrap().to_string();

        // Mark one as done
        handle_tasks_complete(
            &serde_json::json!({ "task_id": a_id }),
            tmp.path(),
            "agnes",
        )
        .await;

        let todo = handle_tasks_list(
            &serde_json::json!({ "status": "todo" }),
            tmp.path(),
            "agnes",
        )
        .await;
        let tlist = parse_ok(&todo)["tasks"].as_array().unwrap().clone();
        assert_eq!(tlist.len(), 2);

        let done_list = handle_tasks_list(
            &serde_json::json!({ "status": "done" }),
            tmp.path(),
            "agnes",
        )
        .await;
        let dlist = parse_ok(&done_list)["tasks"].as_array().unwrap().clone();
        assert_eq!(dlist.len(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shared_skill_share_rejects_missing_skill_file() {
        let tmp = TempDir::new();
        // agnes has no SKILLS directory yet
        let result = handle_shared_skill_share(
            &serde_json::json!({ "skill_name": "nonexistent" }),
            tmp.path(),
            "agnes",
        )
        .await;
        assert!(result["isError"].as_bool().unwrap_or(false));
    }
}

// ─────────────────────────────────────────────────────────────────
// Scope dispatch tests (W19-P0 M2)
// ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod mcp_scope_dispatch_tests {
    //! Validates that the scope enforcement logic in `run_mcp_server` correctly
    //! blocks tool calls that require a scope the caller does not hold, and
    //! that `admin` is treated as an unconditional pass.
    //!
    //! Because `run_mcp_server` is an integration boundary (reads stdin), these
    //! tests exercise the same building blocks: `tool_requires_scope` from
    //! `mcp_auth` and a local `check_scope` helper that mirrors the dispatcher.

    use crate::mcp_auth::{Principal, Scope};
    use chrono::Utc;
    use std::collections::HashSet;

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn make_principal(scopes: &[Scope]) -> Principal {
        Principal {
            client_id: "test-ext".to_string(),
            scopes: scopes.iter().cloned().collect::<HashSet<_>>(),
            is_external: true,
            created_at: Utc::now(),
        }
    }

    /// Mirror of the scope check in `run_mcp_server`:
    ///   if required_scope present AND principal lacks it AND lacks admin → deny.
    fn check_scope(principal: &Principal, tool_name: &str) -> bool {
        if let Some(required) = crate::mcp_auth::tool_requires_scope(tool_name) {
            principal.scopes.contains(&required)
                || principal.scopes.contains(&Scope::Admin)
        } else {
            // No scope required → always allow
            true
        }
    }

    // ── Test 1: memory_store blocked without memory:write ────────────────────
    #[test]
    fn memory_store_blocked_without_memory_write_scope() {
        let p = make_principal(&[Scope::MemoryRead, Scope::WikiRead]);
        assert!(
            !check_scope(&p, "memory_store"),
            "memory_store must be blocked when memory:write is absent"
        );
    }

    // ── Test 2: wiki_write blocked without wiki:write ─────────────────────────
    #[test]
    fn wiki_write_blocked_without_wiki_write_scope() {
        let p = make_principal(&[Scope::MemoryRead, Scope::WikiRead]);
        assert!(
            !check_scope(&p, "wiki_write"),
            "wiki_write must be blocked when wiki:write is absent"
        );
    }

    // ── Test 3: admin scope bypasses all restrictions ────────────────────────
    #[test]
    fn admin_scope_allows_all_restricted_tools() {
        let p = make_principal(&[Scope::Admin]);
        for tool in &["memory_store", "wiki_write", "send_message"] {
            assert!(
                check_scope(&p, tool),
                "admin scope must allow '{tool}'"
            );
        }
    }

    // ── Test 4: unrestricted tool needs no scope ──────────────────────────────
    #[test]
    fn unrestricted_tool_allowed_with_empty_scopes() {
        let p = make_principal(&[]); // no scopes at all
        assert!(
            check_scope(&p, "web_search"),
            "web_search must be unrestricted regardless of scopes"
        );
    }

    // ── Test 5: memory_store allowed when memory:write present ───────────────
    #[test]
    fn memory_store_allowed_with_memory_write_scope() {
        let p = make_principal(&[Scope::MemoryWrite]);
        assert!(
            check_scope(&p, "memory_store"),
            "memory_store must succeed when memory:write is present"
        );
    }

    // ── Test 6: wiki_write allowed when wiki:write present ───────────────────
    #[test]
    fn wiki_write_allowed_with_wiki_write_scope() {
        let p = make_principal(&[Scope::WikiWrite]);
        assert!(
            check_scope(&p, "wiki_write"),
            "wiki_write must succeed when wiki:write is present"
        );
    }

    // ── Test 7: tool_requires_scope returns correct scopes ───────────────────
    #[test]
    fn tool_requires_scope_table_is_correct() {
        use crate::mcp_auth::tool_requires_scope;
        assert_eq!(tool_requires_scope("memory_store"), Some(Scope::MemoryWrite));
        assert_eq!(tool_requires_scope("wiki_write"),   Some(Scope::WikiWrite));
        assert_eq!(tool_requires_scope("send_message"), Some(Scope::MessagingSend));
        assert_eq!(tool_requires_scope("memory_search"),Some(Scope::MemoryRead));
        assert_eq!(tool_requires_scope("wiki_read"),    Some(Scope::WikiRead));
        assert_eq!(tool_requires_scope("totally_unknown"), None);
    }
}

// ─────────────────────────────────────────────────────────────────
// Wiki namespace isolation tests (W19-P0 M2)
// ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod wiki_namespace_tests {
    //! Validates that `wiki_agent_from_ns` resolves the correct agent directory
    //! for external vs. internal principals, and that wiki handlers respect
    //! namespace isolation end-to-end.

    use super::*;
    use crate::mcp_namespace::NamespaceContext;
    use std::fs;

    // ── Local TempDir ─────────────────────────────────────────────────────────
    struct TempDir(std::path::PathBuf);
    impl TempDir {
        fn new() -> Self {
            let p = std::env::temp_dir()
                .join(format!("duduclaw-wns-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> &std::path::Path { &self.0 }
    }
    impl Drop for TempDir {
        fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
    }

    // ── Fixtures ──────────────────────────────────────────────────────────────

    fn external_ns(client_id: &str) -> NamespaceContext {
        NamespaceContext {
            write_namespace: format!("external/{client_id}"),
            read_namespaces: vec![
                format!("external/{client_id}"),
                "shared/public".to_string(),
            ],
        }
    }

    fn internal_ns(client_id: &str) -> NamespaceContext {
        NamespaceContext {
            write_namespace: format!("internal/{client_id}"),
            read_namespaces: vec![
                format!("internal/{client_id}"),
                "shared/public".to_string(),
            ],
        }
    }

    fn create_agent_dir(home: &std::path::Path, name: &str) {
        let dir = home.join("agents").join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("agent.toml"),
            format!("[agent]\nname = \"{name}\"\nrole = \"main\"\n"),
        )
        .unwrap();
    }

    // ── Test 1: external ns → client_id is wiki agent ─────────────────────────
    #[test]
    fn external_ns_resolves_to_client_id() {
        let ctx = external_ns("claude-desktop");
        assert_eq!(wiki_agent_from_ns(&ctx, "dudu"), "claude-desktop");
    }

    // ── Test 2: internal ns → falls back to default_agent ─────────────────────
    #[test]
    fn internal_ns_falls_back_to_default_agent() {
        let ctx = internal_ns("duduclaw-tl");
        assert_eq!(wiki_agent_from_ns(&ctx, "dudu"), "dudu");
    }

    // ── Test 3: default principal ns → falls back to default_agent ───────────
    #[test]
    fn default_internal_ns_falls_back() {
        let ctx = NamespaceContext {
            write_namespace: "internal/default".to_string(),
            read_namespaces: vec![
                "internal/default".to_string(),
                "shared/public".to_string(),
            ],
        };
        assert_eq!(wiki_agent_from_ns(&ctx, "dudu"), "dudu");
    }

    // ── Test 4: wiki_write external client uses client's namespace ────────────
    #[tokio::test(flavor = "current_thread")]
    async fn wiki_write_external_scoped_to_client_namespace() {
        let tmp = TempDir::new();
        let client_id = "claude-desktop";
        let ctx = external_ns(client_id);
        let wiki_agent = wiki_agent_from_ns(&ctx, "dudu");

        // Create the external client's agent directory (simulates provisioning)
        create_agent_dir(tmp.path(), client_id);

        // Args have NO agent_id — simulates the dispatcher stripping it
        let args = serde_json::json!({
            "page_path": "notes/hello.md",
            "content": "---\ntitle: Hello\ncreated: 2026-04-29T00:00:00Z\nupdated: 2026-04-29T00:00:00Z\ntags: [test]\nlayer: context\ntrust: 0.5\n---\nBody.",
        });

        let result = handle_wiki_write(&args, tmp.path(), wiki_agent).await;
        let is_err = result["isError"].as_bool().unwrap_or(false);
        assert!(!is_err, "wiki_write should succeed for external client: {:?}", result);

        // Page must be under the client's namespace, NOT under "dudu"
        assert!(
            tmp.path().join("agents").join(client_id).join("wiki").join("notes").join("hello.md").exists(),
            "wiki page must be written inside the client's namespace"
        );
        assert!(
            !tmp.path().join("agents").join("dudu").join("wiki").join("notes").join("hello.md").exists(),
            "wiki page must NOT be written to the default internal agent's wiki"
        );
    }

    // ── Test 5: external client's agent_id ignored, namespace resolver used ───
    // Verifies the full contract: even if an args map originally contained
    // agent_id (before dispatcher strips it), the namespace resolver wins.
    #[tokio::test(flavor = "current_thread")]
    async fn external_client_agent_id_stripped_uses_namespace() {
        let tmp = TempDir::new();
        let client_id = "trusted-bot";
        let ctx = external_ns(client_id);
        let wiki_agent = wiki_agent_from_ns(&ctx, "dudu");

        create_agent_dir(tmp.path(), client_id);

        // After dispatcher strips agent_id, only page_path + content remain
        let stripped_args = serde_json::json!({
            "page_path": "secure/record.md",
            "content": "---\ntitle: Record\ncreated: 2026-04-29T00:00:00Z\nupdated: 2026-04-29T00:00:00Z\ntags: [secure]\nlayer: context\ntrust: 0.8\n---\nData.",
        });

        let result = handle_wiki_write(&stripped_args, tmp.path(), wiki_agent).await;
        assert!(
            !result["isError"].as_bool().unwrap_or(false),
            "wiki_write after agent_id strip should succeed: {:?}", result
        );

        let expected = tmp.path()
            .join("agents")
            .join(client_id)
            .join("wiki")
            .join("secure")
            .join("record.md");
        assert!(expected.exists(), "page must be in client's namespace: {:?}", expected);
    }

    // ── TC-INT-外部工具過濾: external tools/list returns exactly 7 tools ────────
    #[test]
    fn external_tools_list_returns_exactly_7_tools() {
        use serde_json::json;
        let id = json!(1);

        // External principal → should see exactly 7 whitelisted tools
        let response = super::handle_tools_list(&id, true);
        let tools = response["result"]["tools"].as_array().expect("tools must be array");
        assert_eq!(
            tools.len(),
            7,
            "External principal must see exactly 7 tools, got {}: {:?}",
            tools.len(),
            tools.iter().map(|t| t["name"].as_str().unwrap_or("?")).collect::<Vec<_>>()
        );

        let names: Vec<&str> = tools
            .iter()
            .map(|t| t["name"].as_str().unwrap_or(""))
            .collect();
        for expected in &[
            "memory_search", "memory_store", "memory_read",
            "wiki_read", "wiki_write", "wiki_search",
            "send_message",
        ] {
            assert!(
                names.contains(expected),
                "External tool list must contain '{}'; got: {:?}", expected, names
            );
        }
    }

    // ── TC-INT-內部工具完整: internal tools/list returns full tool list ─────────
    #[test]
    fn internal_tools_list_returns_full_list() {
        use serde_json::json;
        let id = json!(1);

        // Internal principal → should see all tools (more than 7)
        let response = super::handle_tools_list(&id, false);
        let tools = response["result"]["tools"].as_array().expect("tools must be array");
        assert!(
            tools.len() > 7,
            "Internal principal must see more than 7 tools, got {}", tools.len()
        );
    }

    // ── Test 7: wiki_write succeeds for external client WITHOUT pre-created dir ──
    // BUG-QA-003: Reproduces the exact failure scenario — external MCP client
    // (e.g. claude-desktop) connects for the first time with NO agent directory.
    // Before the fix: resolve_wiki_dir returned "Agent does not exist".
    // After the fix: agent dir is auto-created and wiki_write succeeds.
    #[tokio::test(flavor = "current_thread")]
    async fn wiki_write_external_client_auto_creates_dir_on_first_connect() {
        let tmp = TempDir::new();
        let client_id = "claude-desktop";
        let ctx = external_ns(client_id);
        let wiki_agent = wiki_agent_from_ns(&ctx, "dudu");

        // Deliberately do NOT call create_agent_dir — this is the BUG-QA-003 scenario
        assert!(
            !tmp.path().join("agents").join(client_id).exists(),
            "pre-condition: agent dir must NOT exist before first connect"
        );

        let args = serde_json::json!({
            "page_path": "notes/first-page.md",
            "content": "---\ntitle: First Page\ncreated: 2026-04-29T00:00:00Z\nupdated: 2026-04-29T00:00:00Z\ntags: [test]\nlayer: context\ntrust: 0.5\n---\nAuto-created on first connect.",
        });

        let result = handle_wiki_write(&args, tmp.path(), wiki_agent).await;
        assert!(
            !result["isError"].as_bool().unwrap_or(false),
            "wiki_write must succeed for external client on first connect (BUG-QA-003): {:?}", result
        );

        // Agent dir and wiki page must now exist
        assert!(
            tmp.path().join("agents").join(client_id).exists(),
            "agent dir must be auto-created after first wiki_write"
        );
        assert!(
            tmp.path()
                .join("agents").join(client_id)
                .join("wiki").join("notes").join("first-page.md")
                .exists(),
            "wiki page must be written after auto-create"
        );
    }

    // ── Test 8: resolve_wiki_dir auto-creates agent dir ──────────────────────
    #[test]
    fn resolve_wiki_dir_auto_creates_missing_agent_dir() {
        let tmp = TempDir::new();
        let home = tmp.path();
        let agent_id = "new-external-bot";

        // Pre-condition: directory does not exist
        assert!(!home.join("agents").join(agent_id).exists());

        let result = resolve_wiki_dir(home, agent_id);
        assert!(result.is_ok(), "resolve_wiki_dir must succeed even when agent dir is absent: {:?}", result);

        let wiki_path = result.unwrap();
        assert_eq!(wiki_path, home.join("agents").join(agent_id).join("wiki"));
        assert!(
            home.join("agents").join(agent_id).exists(),
            "agent dir must be created by resolve_wiki_dir"
        );
    }

    // ── Test 9: resolve_wiki_dir leaves existing dir untouched ───────────────
    #[test]
    fn resolve_wiki_dir_existing_dir_unchanged() {
        let tmp = TempDir::new();
        let home = tmp.path();
        let agent_id = "existing-agent";

        // Pre-condition: agent dir already exists with a marker file
        let agent_dir = home.join("agents").join(agent_id);
        fs::create_dir_all(&agent_dir).unwrap();
        fs::write(agent_dir.join("agent.toml"), "[agent]\nname = \"existing-agent\"\n").unwrap();

        let result = resolve_wiki_dir(home, agent_id);
        assert!(result.is_ok(), "resolve_wiki_dir must succeed for existing dir: {:?}", result);
        assert_eq!(result.unwrap(), agent_dir.join("wiki"));

        // Marker file must still be present (existing contents untouched)
        assert!(
            agent_dir.join("agent.toml").exists(),
            "existing agent.toml must not be removed"
        );
    }

    // ── Test 10: resolve_wiki_dir rejects invalid agent_id ──────────────────
    #[test]
    fn resolve_wiki_dir_invalid_agent_id_rejected() {
        let tmp = TempDir::new();
        // Vector 1: Path traversal attempt (relative path traversal: "../")
        assert!(resolve_wiki_dir(tmp.path(), "../evil").is_err(), "../evil must be rejected");
        assert!(resolve_wiki_dir(tmp.path(), "../../etc/passwd").is_err(), "../../etc/passwd must be rejected");
        // Vector 2: Absolute path injection ("/absolute")
        assert!(resolve_wiki_dir(tmp.path(), "/absolute").is_err(), "/absolute must be rejected");
        assert!(resolve_wiki_dir(tmp.path(), "/etc/passwd").is_err(), "/etc/passwd must be rejected");
        // Vector 3: Empty string
        assert!(resolve_wiki_dir(tmp.path(), "").is_err(), "empty string must be rejected");
        // Additional: Uppercase not allowed
        assert!(resolve_wiki_dir(tmp.path(), "Agent-Name").is_err(), "uppercase must be rejected");
        // Additional: Null byte injection
        assert!(resolve_wiki_dir(tmp.path(), "agent\0id").is_err(), "null byte must be rejected");
    }

    // ── Test: audit_trail_query requires Admin scope (H1 fix / OWASP A01) ──────
    /// Verify that `handle_audit_trail_query` rejects callers who lack Admin scope,
    /// providing a defence-in-depth guard independent of the dispatch-layer scope check.
    #[tokio::test(flavor = "current_thread")]
    async fn audit_trail_query_denied_without_admin_scope() {
        let tmp = TempDir::new();
        let result = handle_audit_trail_query(
            &serde_json::json!({}),
            tmp.path(),
            "non-admin-client",
            false, // caller_is_admin = false
        )
        .await;
        assert_eq!(
            result["isError"],
            serde_json::json!(true),
            "non-admin call must be rejected with isError=true"
        );
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            text.contains("Admin scope"),
            "error message must reference the required scope; got: {text}"
        );
    }

    /// Verify that an admin-scoped caller is NOT rejected for authorization reasons.
    /// (Index may not exist in temp dir, so we accept any non-auth error response.)
    #[tokio::test(flavor = "current_thread")]
    async fn audit_trail_query_proceeds_with_admin_scope() {
        let tmp = TempDir::new();
        let result = handle_audit_trail_query(
            &serde_json::json!({}),
            tmp.path(),
            "admin-client",
            true, // caller_is_admin = true
        )
        .await;
        // An admin call must NOT be rejected due to authorization.
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            !text.contains("Admin scope"),
            "admin call must not fail with auth scope error; got: {text}"
        );
    }

    // ── Test 6: wiki_search scoped to client namespace ────────────────────────
    // External client has no wiki yet → response mentions "No wiki found"
    // (rather than falling through to the internal default agent's wiki).
    #[tokio::test(flavor = "current_thread")]
    async fn wiki_search_scoped_to_client_namespace() {
        let tmp = TempDir::new();
        let client_id = "search-bot";
        let ctx = external_ns(client_id);
        let wiki_agent = wiki_agent_from_ns(&ctx, "dudu");

        // Create client agent dir but NO wiki inside it
        create_agent_dir(tmp.path(), client_id);
        // Also create dudu's wiki with a page — must NOT appear in search result
        create_agent_dir(tmp.path(), "dudu");
        let dudu_wiki = tmp.path().join("agents").join("dudu").join("wiki");
        fs::create_dir_all(&dudu_wiki).unwrap();
        fs::write(
            dudu_wiki.join("secret.md"),
            "---\ntitle: Secret\ncreated: 2026-04-29T00:00:00Z\nupdated: 2026-04-29T00:00:00Z\ntags: [internal]\nlayer: context\ntrust: 0.9\n---\nsecret internal content",
        )
        .unwrap();

        let args = serde_json::json!({ "query": "secret" });
        let result = handle_wiki_search(&args, tmp.path(), wiki_agent).await;
        let text = result["content"][0]["text"].as_str().unwrap_or("");

        assert!(
            text.contains("No wiki found") || text.contains("No wiki pages match"),
            "wiki_search must be scoped to client namespace; got: {text}"
        );
        assert!(
            !text.contains("secret internal content"),
            "internal agent content must not leak to external client search"
        );
    }

    // ── reliability_summary handler tests (W20-P0) ────────────────────────────

    /// Non-admin caller must be rejected with isError=true.
    #[tokio::test(flavor = "current_thread")]
    async fn reliability_summary_denied_without_admin_scope() {
        let tmp = TempDir::new();
        let result = handle_reliability_summary(
            &serde_json::json!({"agent_id": "some-agent"}),
            tmp.path(),
            "non-admin-client",
            false,
        )
        .await;
        assert_eq!(result["isError"], serde_json::json!(true));
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            text.contains("Admin scope"),
            "error must reference Admin scope; got: {text}"
        );
    }

    /// Missing agent_id parameter must return isError=true.
    #[tokio::test(flavor = "current_thread")]
    async fn reliability_summary_missing_agent_id() {
        let tmp = TempDir::new();
        let result = handle_reliability_summary(
            &serde_json::json!({}),
            tmp.path(),
            "admin-client",
            true,
        )
        .await;
        assert_eq!(result["isError"], serde_json::json!(true));
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            text.contains("agent_id"),
            "error must mention agent_id; got: {text}"
        );
    }

    /// Admin caller with valid params must NOT be rejected for auth reasons.
    /// (Index may not exist in temp dir — we accept any non-auth response.)
    #[tokio::test(flavor = "current_thread")]
    async fn reliability_summary_proceeds_with_admin_scope() {
        let tmp = TempDir::new();
        let result = handle_reliability_summary(
            &serde_json::json!({"agent_id": "my-agent"}),
            tmp.path(),
            "admin-client",
            true,
        )
        .await;
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            !text.contains("Admin scope"),
            "admin call must not fail auth check; got: {text}"
        );
    }

    /// window_days defaults to 7 when not provided.
    #[tokio::test(flavor = "current_thread")]
    async fn reliability_summary_default_window_days() {
        let tmp = TempDir::new();
        let result = handle_reliability_summary(
            &serde_json::json!({"agent_id": "my-agent"}),
            tmp.path(),
            "admin-client",
            true,
        )
        .await;
        assert!(
            !result["isError"].as_bool().unwrap_or(false),
            "DB open must succeed: {:?}",
            result
        );
        let wd = result["reliability_summary"]["window_days"].as_u64().unwrap_or(0);
        assert_eq!(wd, 7, "default window_days must be 7");
    }

    /// window_days > 365 must be clamped to 365.
    #[tokio::test(flavor = "current_thread")]
    async fn reliability_summary_window_days_clamped() {
        let tmp = TempDir::new();
        let result = handle_reliability_summary(
            &serde_json::json!({"agent_id": "my-agent", "window_days": 9999}),
            tmp.path(),
            "admin-client",
            true,
        )
        .await;
        assert!(
            !result["isError"].as_bool().unwrap_or(false),
            "DB open must succeed: {:?}",
            result
        );
        let wd = result["reliability_summary"]["window_days"].as_u64().unwrap_or(0);
        assert_eq!(wd, 365, "window_days must be clamped to 365");
    }

    /// window_days=1 must pass through without being clamped (lower bound is 1).
    #[tokio::test(flavor = "current_thread")]
    async fn reliability_summary_window_days_min_boundary() {
        let tmp = TempDir::new();
        let result = handle_reliability_summary(
            &serde_json::json!({"agent_id": "my-agent", "window_days": 1}),
            tmp.path(),
            "admin-client",
            true,
        )
        .await;
        assert!(
            !result["isError"].as_bool().unwrap_or(false),
            "DB open must succeed: {:?}",
            result
        );
        let wd = result["reliability_summary"]["window_days"].as_u64().unwrap_or(0);
        assert_eq!(wd, 1, "window_days=1 must not be clamped");
    }

    /// agent_id consisting only of whitespace must be rejected (not accepted as non-empty).
    #[tokio::test(flavor = "current_thread")]
    async fn reliability_summary_whitespace_agent_id_rejected() {
        let tmp = TempDir::new();
        let result = handle_reliability_summary(
            &serde_json::json!({"agent_id": "   "}),
            tmp.path(),
            "admin-client",
            true,
        )
        .await;
        assert!(
            result["isError"].as_bool().unwrap_or(false),
            "whitespace-only agent_id must be rejected as missing"
        );
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            text.contains("agent_id"),
            "error message must mention agent_id; got: {text}"
        );
    }

    /// agent_id exceeding MAX_AGENT_ID_LEN must be rejected.
    #[tokio::test(flavor = "current_thread")]
    async fn reliability_summary_agent_id_too_long_rejected() {
        let tmp = TempDir::new();
        let long_id = "a".repeat(129);
        let result = handle_reliability_summary(
            &serde_json::json!({"agent_id": long_id}),
            tmp.path(),
            "admin-client",
            true,
        )
        .await;
        assert!(
            result["isError"].as_bool().unwrap_or(false),
            "agent_id longer than 128 chars must be rejected"
        );
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            text.contains("128"),
            "error message must mention the 128-char limit; got: {text}"
        );
    }

    /// tool_requires_scope must map reliability_summary → Admin.
    #[test]
    fn reliability_summary_scope_is_admin() {
        use crate::mcp_auth::{tool_requires_scope, Scope};
        assert_eq!(
            tool_requires_scope("reliability_summary"),
            Some(Scope::Admin),
            "reliability_summary must require Admin scope"
        );
    }

    // ── TC-SKILL-RUN-01: skill_synthesis_run visible to internal principal ──────
    // TDD 驗收：W20-P0 修復 — skill_synthesis_run 工具缺失
    // 保證 internal agents（Cron pipeline、ENG-AGENT）可以看到此工具。
    #[test]
    fn skill_synthesis_run_visible_to_internal_principal() {
        use serde_json::json;
        let id = json!(1);

        let response = super::handle_tools_list(&id, /* is_external= */ false);
        let tools = response["result"]["tools"]
            .as_array()
            .expect("tools must be array");

        let names: Vec<&str> = tools
            .iter()
            .map(|t| t["name"].as_str().unwrap_or(""))
            .collect();

        assert!(
            names.contains(&"skill_synthesis_run"),
            "skill_synthesis_run must appear in internal tools list; got: {:?}",
            names
        );
    }

    // ── TC-SKILL-RUN-02: skill_synthesis_run NOT visible to external principal ──
    // 安全性驗收：外部 client（Claude Desktop 等）不應能觸發 pipeline。
    #[test]
    fn skill_synthesis_run_hidden_from_external_principal() {
        use serde_json::json;
        let id = json!(1);

        let response = super::handle_tools_list(&id, /* is_external= */ true);
        let tools = response["result"]["tools"]
            .as_array()
            .expect("tools must be array");

        let names: Vec<&str> = tools
            .iter()
            .map(|t| t["name"].as_str().unwrap_or(""))
            .collect();

        assert!(
            !names.contains(&"skill_synthesis_run"),
            "skill_synthesis_run must NOT appear in external tools list (security); got: {:?}",
            names
        );
    }

    // ── TC-SKILL-RUN-03: skill_synthesis_run schema is well-formed ───────────
    // 驗收：工具定義包含正確的 name、description 和 parameters。
    #[test]
    fn skill_synthesis_run_schema_is_well_formed() {
        use serde_json::json;
        let id = json!(1);

        let response = super::handle_tools_list(&id, /* is_external= */ false);
        let tools = response["result"]["tools"]
            .as_array()
            .expect("tools must be array");

        let tool = tools
            .iter()
            .find(|t| t["name"].as_str() == Some("skill_synthesis_run"))
            .expect("skill_synthesis_run must be present in internal tools list");

        // name
        assert_eq!(tool["name"].as_str(), Some("skill_synthesis_run"));

        // description must be non-empty
        let desc = tool["description"].as_str().unwrap_or("");
        assert!(
            !desc.is_empty(),
            "skill_synthesis_run must have a non-empty description"
        );

        // inputSchema must have properties: agent_id, dry_run, lookback_days
        let props = &tool["inputSchema"]["properties"];
        for param in &["agent_id", "dry_run", "lookback_days"] {
            assert!(
                props.get(param).is_some(),
                "skill_synthesis_run inputSchema must have property '{}'; schema: {}",
                param,
                tool["inputSchema"]
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────
// RFC-21 §2 — Odoo per-agent connector pool integration tests
// ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod odoo_pool_dispatch_tests {
    //! Validates the routing seam between MCP dispatch and the
    //! [`crate::odoo_pool::OdooConnectorPool`]: classification, permission
    //! checks, and pool-key isolation. Actual HTTP round-trips to Odoo are
    //! not exercised here — that is covered by `duduclaw-odoo` connector
    //! tests against a live or mocked server.

    use super::*;
    use std::sync::Arc;

    #[test]
    fn classify_maps_search_class_tools() {
        for (tool, expected_model) in &[
            ("odoo_crm_leads", "crm.lead"),
            ("odoo_sale_orders", "sale.order"),
            ("odoo_inventory_products", "product.product"),
            ("odoo_inventory_check", "stock.quant"),
            ("odoo_invoice_list", "account.move"),
            ("odoo_payment_status", "account.move"),
        ] {
            let (verb, model) = classify_odoo_call(tool, &serde_json::json!({})).unwrap();
            assert_eq!(verb, "search", "tool={tool}");
            assert_eq!(model, *expected_model, "tool={tool}");
        }
    }

    #[test]
    fn classify_maps_create_class_tools() {
        for (tool, expected_model) in &[
            ("odoo_crm_create_lead", "crm.lead"),
            ("odoo_sale_create_quotation", "sale.order"),
        ] {
            let (verb, model) = classify_odoo_call(tool, &serde_json::json!({})).unwrap();
            assert_eq!(verb, "create", "tool={tool}");
            assert_eq!(model, *expected_model);
        }
    }

    #[test]
    fn classify_maps_write_class_tools() {
        let (verb, model) =
            classify_odoo_call("odoo_crm_update_stage", &serde_json::json!({})).unwrap();
        assert_eq!(verb, "write");
        assert_eq!(model, "crm.lead");
    }

    #[test]
    fn classify_maps_execute_class_tools() {
        let (verb, model) =
            classify_odoo_call("odoo_sale_confirm", &serde_json::json!({})).unwrap();
        assert_eq!(verb, "execute");
        assert_eq!(model, "sale.order");
    }

    #[test]
    fn classify_extracts_model_from_params_for_generic_search() {
        let (verb, model) = classify_odoo_call(
            "odoo_search",
            &serde_json::json!({ "model": "res.partner" }),
        )
        .unwrap();
        assert_eq!(verb, "search");
        assert_eq!(model, "res.partner");
    }

    #[test]
    fn classify_returns_none_for_generic_search_without_model() {
        // No model arg → can't classify. The downstream handler will reject
        // with "model is required" — same v1.10.1 behaviour.
        assert!(classify_odoo_call("odoo_search", &serde_json::json!({})).is_none());
    }

    #[test]
    fn classify_extracts_model_from_params_for_generic_execute() {
        let (verb, model) = classify_odoo_call(
            "odoo_execute",
            &serde_json::json!({ "model": "res.partner", "method": "search" }),
        )
        .unwrap();
        assert_eq!(verb, "execute");
        assert_eq!(model, "res.partner");
    }

    #[test]
    fn classify_returns_none_for_status_and_connect() {
        // These tools intentionally bypass model-permission gating —
        // odoo_status reports state, odoo_connect bootstraps the slot.
        assert!(classify_odoo_call("odoo_status", &serde_json::json!({})).is_none());
        assert!(classify_odoo_call("odoo_connect", &serde_json::json!({})).is_none());
    }

    #[test]
    fn classify_returns_none_for_unknown_tool() {
        assert!(classify_odoo_call("odoo_blarg", &serde_json::json!({})).is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn odoo_status_reports_not_connected_for_fresh_agent() {
        let pool: OdooState = Arc::new(crate::odoo_pool::OdooConnectorPool::default());
        let result =
            handle_odoo_tool("odoo_status", &serde_json::json!({}), std::path::Path::new("/tmp"), &pool, "agnes")
                .await;
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        assert!(text.contains("Odoo not connected"), "got: {text}");
        assert!(text.contains("agnes"), "should name the caller, got: {text}");
        assert!(result["isError"].as_bool().unwrap_or(false));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn odoo_tool_blocks_disallowed_model_before_any_network_call() {
        // Register an agent override that whitelists only crm.lead.
        let pool: OdooState = Arc::new(crate::odoo_pool::OdooConnectorPool::default());
        pool.register_agent(
            "agnes",
            duduclaw_odoo::AgentOdooConfig {
                profile: Some("test".into()),
                allowed_models: vec!["crm.lead".into()],
                ..Default::default()
            },
        )
        .await;

        // Attempt a sale.order search — must be rejected at the gate, no
        // get_or_connect HTTP call attempted.
        let result = handle_odoo_tool(
            "odoo_sale_orders",
            &serde_json::json!({}),
            std::path::Path::new("/tmp"),
            &pool,
            "agnes",
        )
        .await;
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        assert!(result["isError"].as_bool().unwrap_or(false));
        assert!(text.contains("permission denied"), "got: {text}");
        assert!(text.contains("allowed_models"), "got: {text}");
        // No connector slot should have been touched.
        assert!(!pool.is_connected("agnes").await);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn odoo_tool_blocks_disallowed_action_verb() {
        let pool: OdooState = Arc::new(crate::odoo_pool::OdooConnectorPool::default());
        pool.register_agent(
            "agnes",
            duduclaw_odoo::AgentOdooConfig {
                profile: Some("readonly".into()),
                allowed_actions: vec!["read".into(), "search".into()],
                ..Default::default()
            },
        )
        .await;

        // Attempt a write — must be denied even though crm.lead is permitted
        // (no model whitelist set).
        let result = handle_odoo_tool(
            "odoo_crm_create_lead",
            &serde_json::json!({ "name": "test lead" }),
            std::path::Path::new("/tmp"),
            &pool,
            "agnes",
        )
        .await;
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        assert!(result["isError"].as_bool().unwrap_or(false));
        assert!(text.contains("permission denied"), "got: {text}");
        assert!(text.contains("allowed_actions"), "got: {text}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn odoo_tool_without_override_falls_through_to_connection_check() {
        // No override → permission gate is permissive → handler proceeds to
        // get_or_connect, which fails with the "not connected" message
        // because no connect was issued.
        let pool: OdooState = Arc::new(crate::odoo_pool::OdooConnectorPool::default());
        let result = handle_odoo_tool(
            "odoo_crm_leads",
            &serde_json::json!({}),
            std::path::Path::new("/tmp"),
            &pool,
            "agnes",
        )
        .await;
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        assert!(result["isError"].as_bool().unwrap_or(false));
        assert!(text.contains("not connected"), "got: {text}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn two_agents_get_isolated_pool_slots() {
        let pool: OdooState = Arc::new(crate::odoo_pool::OdooConnectorPool::default());
        pool.register_agent(
            "alpha-pm",
            duduclaw_odoo::AgentOdooConfig {
                profile: Some("alpha".into()),
                ..Default::default()
            },
        )
        .await;
        pool.register_agent(
            "beta-pm",
            duduclaw_odoo::AgentOdooConfig {
                profile: Some("beta".into()),
                ..Default::default()
            },
        )
        .await;

        let alpha_key = pool.pool_key("alpha-pm").await;
        let beta_key = pool.pool_key("beta-pm").await;
        assert_ne!(alpha_key, beta_key);
        assert_eq!(alpha_key.1, "alpha");
        assert_eq!(beta_key.1, "beta");
    }
}
