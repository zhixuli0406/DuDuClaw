//! Agent lifecycle, dispatch and sub-agent orchestration.
//!
//! Part of the split `tools_def` registration face (O7). Every `description`
//! here is bounded by the O7 budget (<=200 bytes for a tool, <=200 bytes for a
//! parameter) and enforced by `mcp::tools_list_budget_tests` — long prose
//! belongs in `docs/guides/mcp-tools.md`, not on the wire.

use super::super::{ParamDef, ToolDef};

pub(super) const TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "create_agent",
        description: "Create a persistent sub-agent with its own identity, skills, and configuration. The agent is registered and available for delegation immediately.",
        params: &[
            ParamDef {
                name: "name",
                description: "Agent name (lowercase, no spaces, e.g. 'researcher')",
                required: true,
            },
            ParamDef {
                name: "display_name",
                description: "Human-readable display name (e.g. 'Research Assistant')",
                required: true,
            },
            ParamDef {
                name: "role",
                description: "Agent role: 'specialist' or 'worker' (default: specialist)",
                required: false,
            },
            ParamDef {
                name: "reports_to",
                description: "Parent agent name this agent reports to (default: main agent)",
                required: false,
            },
            ParamDef {
                name: "soul",
                description: "Personality/system prompt for this agent (written to SOUL.md)",
                required: false,
            },
            ParamDef {
                name: "model",
                description: "Preferred model (default: claude-sonnet-4-6)",
                required: false,
            },
            ParamDef {
                name: "trigger",
                description: "Trigger keyword (default: @display_name)",
                required: false,
            },
            ParamDef {
                name: "icon",
                description: "Emoji icon for this agent (default: 🤖)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "list_agents",
        description: "List all registered operational agents with their role, status, and reports_to hierarchy. Soft-deleted agents are always hidden; archived agents are hidden unless include_archived is true.",
        params: &[ParamDef {
            name: "include_archived",
            description: "Include archived (off-boarded but recoverable) agents. Default false.",
            required: false,
        }],
    },
    ToolDef {
        name: "create_task",
        description: "Submit a structured multi-step PLAN (ordered `steps`) for deterministic gateway execution with retry and replan. Not the Kanban board and not an autonomous goal — use tasks_create for those.",
        params: &[
            ParamDef {
                name: "goal",
                description: "Overall task goal / description",
                required: true,
            },
            ParamDef {
                name: "steps",
                description: "JSON array. Each step: {\"description\", \"agent\"? (default=caller), \"depends_on\"? [indices], \"acceptance_criteria\"? [{\"description\"}]}.",
                required: true,
            },
        ],
    },
    ToolDef {
        name: "check_responses",
        description: "Check responses from agents you messaged via send_to_agent. Returns the most recent bus-queue responses for an agent — use it to verify whether an agent actually replied.",
        params: &[
            ParamDef {
                name: "agent_id",
                description: "Agent ID to check responses from",
                required: true,
            },
            ParamDef {
                name: "limit",
                description: "Max number of responses to return (default: 5)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "task_status",
        description: "Check the status of a previously created task (from create_task)",
        params: &[ParamDef {
            name: "task_id",
            description: "Task ID returned by create_task",
            required: true,
        }],
    },
    ToolDef {
        name: "agent_status",
        description: "Get detailed status and configuration of a specific agent",
        params: &[ParamDef {
            name: "agent_id",
            description: "Agent name to inspect",
            required: true,
        }],
    },
    ToolDef {
        name: "spawn_agent",
        description: "Spawn a persistent sub-agent task; it runs in the background with its own session executing the prompt. Check progress with agent_status. Delegation depth is tracked automatically (max 5 hops).",
        params: &[
            ParamDef {
                name: "agent_id",
                description: "Target agent name",
                required: true,
            },
            ParamDef {
                name: "task",
                description: "Task prompt for the agent to execute",
                required: true,
            },
            ParamDef {
                name: "session_key",
                description: "Optional session key to resume a previous conversation context",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "spawn_ephemeral",
        description: "Synthesize a purpose-built ephemeral sub-agent and dispatch one task to it. Restricted to a subset of YOUR capabilities (no escalation), collected after 24h. Model is chosen by TIER, never by id.",
        params: &[
            ParamDef {
                name: "instruction",
                description: "System-prompt fragment defining the ephemeral agent's role (becomes its SOUL.md)",
                required: true,
            },
            ParamDef {
                name: "context",
                description: "The task payload the ephemeral agent must execute",
                required: true,
            },
            ParamDef {
                name: "tools",
                description: "JSON array of allowed tool names (must be a subset of the calling agent's own capabilities; deny-by-default)",
                required: true,
            },
            ParamDef {
                name: "tier",
                description: "Model tier: cheap | standard | preferred (default: standard). Raw model ids are rejected.",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "agent_update",
        description: "Update one or more fields of an existing agent's configuration (agent.toml). Supports identity, model, budget, heartbeat, container, and database-source-grant fields. Uses atomic write for safety.",
        params: &[
            ParamDef {
                name: "agent_id",
                description: "Agent name to update",
                required: true,
            },
            ParamDef {
                name: "display_name",
                description: "New display name",
                required: false,
            },
            ParamDef {
                name: "role",
                description: "New role: main, specialist, worker, developer, qa, planner",
                required: false,
            },
            ParamDef {
                name: "status",
                description: "New status: active, paused, terminated",
                required: false,
            },
            ParamDef {
                name: "trigger",
                description: "New trigger keyword",
                required: false,
            },
            ParamDef {
                name: "icon",
                description: "New emoji icon",
                required: false,
            },
            ParamDef {
                name: "reports_to",
                description: "New parent agent name",
                required: false,
            },
            ParamDef {
                name: "model",
                description: "New preferred model",
                required: false,
            },
            ParamDef {
                name: "fallback_model",
                description: "New fallback model",
                required: false,
            },
            ParamDef {
                name: "api_mode",
                description: "API mode: cli, direct, auto",
                required: false,
            },
            ParamDef {
                name: "budget_cents",
                description: "Monthly budget limit in cents",
                required: false,
            },
            ParamDef {
                name: "max_concurrent",
                description: "Max concurrent container tasks",
                required: false,
            },
            ParamDef {
                name: "heartbeat_enabled",
                description: "Enable/disable heartbeat (true/false)",
                required: false,
            },
            ParamDef {
                name: "heartbeat_cron",
                description: "Heartbeat cron expression",
                required: false,
            },
            ParamDef {
                name: "db_sources",
                description: "REPLACE the agent's authorized DB sources with this comma-separated list of `[db_sources.<id>]` ids; \"\" revokes all. Unknown ids are rejected and nothing is written.",
                required: false,
            },
            ParamDef {
                name: "db_sources_add",
                description: "GRANT these `[db_sources.<id>]` ids on top of what the agent holds (comma-separated). Idempotent; unknown ids are rejected with the configured ids listed.",
                required: false,
            },
            ParamDef {
                name: "db_sources_remove",
                description: "REVOKE these source ids (comma-separated). Not checked against config.toml, so a stale grant can still be revoked. Applied after db_sources and db_sources_add.",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "agent_remove",
        description: "Remove an agent. The administrator can restore it; its name stays reserved and cannot be reused with create_agent. Refuses to remove the main agent.",
        params: &[ParamDef {
            name: "agent_id",
            description: "Agent name to remove",
            required: true,
        }],
    },
    ToolDef {
        name: "agent_update_soul",
        description: "Update an agent's SOUL.md personality file via the trusted MCP channel. Bypasses file-protect hooks. Uses atomic write with SHA-256 fingerprinting.",
        params: &[
            ParamDef {
                name: "agent_id",
                description: "Agent name",
                required: true,
            },
            ParamDef {
                name: "content",
                description: "New SOUL.md content (full replacement)",
                required: true,
            },
        ],
    },
];
