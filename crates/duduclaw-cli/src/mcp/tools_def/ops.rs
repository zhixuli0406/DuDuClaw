//! Evolution toggles, audit trail, channel config and pairing.
//!
//! Part of the split `tools_def` registration face (O7). Every `description`
//! here is bounded by the O7 budget (<=200 bytes for a tool, <=200 bytes for a
//! parameter) and enforced by `mcp::tools_list_budget_tests` — long prose
//! belongs in `docs/guides/mcp-tools.md`, not on the wire.

use super::super::{ParamDef, ToolDef};

pub(super) const TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "submit_feedback",
        description: "Submit user feedback signal (positive/negative/correction) to influence agent evolution",
        params: &[
            ParamDef {
                name: "signal_type",
                description: "Feedback type: positive, negative, or correction",
                required: true,
            },
            ParamDef {
                name: "detail",
                description: "What the feedback is about",
                required: true,
            },
            ParamDef {
                name: "agent_id",
                description: "Target agent (default: main agent)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "evolution_toggle",
        description: "Toggle evolution engine flags for an agent; effective within seconds.",
        params: &[
            ParamDef {
                name: "agent_id",
                description: "Target agent name",
                required: true,
            },
            ParamDef {
                name: "field",
                description: "gvu_enabled (bool); max_silence_hours / skill_token_budget / max_active_skills (number).",
                required: true,
            },
            ParamDef {
                name: "value",
                description: "New value: true/false (for gvu_enabled) or a number",
                required: true,
            },
        ],
    },
    ToolDef {
        name: "evolution_status",
        description: "Get the current evolution engine configuration and status for an agent",
        params: &[ParamDef {
            name: "agent_id",
            description: "Target agent name (default: main agent)",
            required: false,
        }],
    },
    ToolDef {
        name: "audit_trail_query",
        description: "Query the EvolutionEvent audit trail (Governance + Durability events). Syncs the SQLite index from JSONL, then filters and paginates by agent, event type, outcome, skill and time range.",
        params: &[
            ParamDef {
                name: "agent_id",
                description: "Filter by agent ID (optional)",
                required: false,
            },
            ParamDef {
                name: "event_type",
                description: "Filter by event type, e.g. governance_violation, durability_circuit_opened (optional)",
                required: false,
            },
            ParamDef {
                name: "outcome",
                description: "Filter by outcome, e.g. blocked, warned, triggered, recovered (optional)",
                required: false,
            },
            ParamDef {
                name: "skill_id",
                description: "Filter by skill ID (optional)",
                required: false,
            },
            ParamDef {
                name: "since",
                description: "Inclusive lower bound RFC3339 timestamp, e.g. 2026-04-29T00:00:00Z (optional)",
                required: false,
            },
            ParamDef {
                name: "until",
                description: "Exclusive upper bound RFC3339 timestamp (optional)",
                required: false,
            },
            ParamDef {
                name: "limit",
                description: "Page size 1–1000 (default 100)",
                required: false,
            },
            ParamDef {
                name: "offset",
                description: "Pagination offset (default 0)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "reliability_summary",
        description: "Agent Reliability summary from the EvolutionEvent audit trail over a time window: consistency_score, task_success_rate, skill_adoption_rate, fallback_trigger_rate. Requires Admin scope.",
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
    ToolDef {
        name: "channel_config",
        description: "Get or set channel settings (mention_only, auto_thread, allowed_channels, allowed_guilds, agent_override, response_mode). Omit 'value' to read current setting.",
        params: &[
            ParamDef {
                name: "channel",
                description: "Channel type: discord, telegram, slack, line",
                required: true,
            },
            ParamDef {
                name: "scope_id",
                description: "Scope: guild_id, chat_id, or 'global'",
                required: true,
            },
            ParamDef {
                name: "key",
                description: "Setting key: mention_only, auto_thread, allowed_channels, allowed_guilds (global scope), agent_override, response_mode",
                required: true,
            },
            ParamDef {
                name: "value",
                description: "New value (omit to read current value)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "channel_config_list",
        description: "List all channel settings for a scope",
        params: &[
            ParamDef {
                name: "channel",
                description: "Channel type: discord, telegram, slack, line",
                required: true,
            },
            ParamDef {
                name: "scope_id",
                description: "Scope: guild_id, chat_id, or 'global'",
                required: true,
            },
        ],
    },
    ToolDef {
        name: "channel_status",
        description: "Channel status overview: per-channel connection state, per-channel session counts (total / active in 24h), thread and topic session counts, and known Discord guilds with their settings.",
        params: &[ParamDef {
            name: "channel",
            description: "Filter to one channel type (discord, telegram, slack, line, whatsapp, feishu); omit for all",
            required: false,
        }],
    },
    ToolDef {
        name: "pairing_manage",
        description: "Manage user pairing for channel access control. 'generate' issues a 6-digit code (5 min) redeemed in-channel with /pair <code>; 'approve'/'revoke' manage the list; 'list' shows approved subjects.",
        params: &[
            ParamDef {
                name: "action",
                description: "One of: generate, approve, revoke, list",
                required: true,
            },
            ParamDef {
                name: "subject",
                description: "User id (e.g. Telegram numeric id, Discord snowflake) or session id (e.g. slack:group:C123). Required except for 'list'",
                required: false,
            },
        ],
    },
];
