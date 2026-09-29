//! Skill discovery, synthesis, vetting and pinning.
//!
//! Part of the split `tools_def` registration face (O7). Every `description`
//! here is bounded by the O7 budget (<=200 bytes for a tool, <=200 bytes for a
//! parameter) and enforced by `mcp::tools_list_budget_tests` — long prose
//! belongs in `docs/guides/mcp-tools.md`, not on the wire.

use super::super::{ParamDef, ToolDef};

pub(super) const TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "skill_search",
        description: "Search for skills to install — one entry point over the configured hubs AND this agent's learned skill bank, de-duplicated and source-labelled. Narrow with `source` only if you know where it lives.",
        params: &[
            ParamDef {
                name: "query",
                description: "Search query (name, tag, or description)",
                required: true,
            },
            ParamDef {
                name: "source",
                description: "'all' (default — hubs + learned bank), 'github', 'hub' (curated hubs), or 'bank' (this deployment's learned skills).",
                required: false,
            },
            ParamDef {
                name: "hub",
                description: "Restrict the hub half to one hub id: anthropic-skills, github, clawhub, lobehub or skills-sh. Not accepted when source='bank'.",
                required: false,
            },
            ParamDef {
                name: "limit",
                description: "Max results per source (default: 20 for hubs, 5 for the skill bank)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "skill_list",
        description: "List all skills installed for a specific agent",
        params: &[ParamDef {
            name: "agent_id",
            description: "Agent name (default: main agent)",
            required: false,
        }],
    },
    ToolDef {
        name: "skill_gaps",
        description: "Report an agent's capability gaps inferred from attachment file types it received with no matching skill (e.g. a .psd with no design skill), plus the template's recommended skills.",
        params: &[ParamDef {
            name: "agent_id",
            description: "Agent name (default: main agent)",
            required: false,
        }],
    },
    ToolDef {
        name: "skill_security_scan",
        description: "Run a security scan on a skill file and report risk level and findings",
        params: &[
            ParamDef {
                name: "agent_id",
                description: "Agent name",
                required: true,
            },
            ParamDef {
                name: "skill_name",
                description: "Skill name to scan",
                required: true,
            },
        ],
    },
    ToolDef {
        name: "skill_graduate",
        description: "Manually graduate a proven agent-local skill to global scope (~/.duduclaw/skills/)",
        params: &[
            ParamDef {
                name: "agent_id",
                description: "Agent that owns the skill",
                required: true,
            },
            ParamDef {
                name: "skill_name",
                description: "Skill name to graduate",
                required: true,
            },
        ],
    },
    ToolDef {
        name: "skill_synthesis_status",
        description: "Report auto-synthesis status: sandboxed skills, gap accumulator state, recent synthesis events",
        params: &[ParamDef {
            name: "agent_id",
            description: "Agent name (default: main agent)",
            required: false,
        }],
    },
    ToolDef {
        name: "skill_synthesis_run",
        description: "Manually trigger the rollout-to-skill synthesis pipeline: parses EvolutionEvents, scores trajectories, and with dry_run=false synthesises and graduates high-quality skills into the Skill Bank.",
        params: &[
            ParamDef {
                name: "agent_id",
                description: "Target agent that will own synthesised skills (default: main agent)",
                required: false,
            },
            ParamDef {
                name: "dry_run",
                description: "true = score only, no Skill Bank writes (default: true)",
                required: false,
            },
            ParamDef {
                name: "lookback_days",
                description: "Days of EvolutionEvents history to scan (default: 1)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "skill_hub_install",
        description: "Install a skill from a configured hub (clawhub / lobehub). Every install is routed through the security scan gate before activation — high-risk or content-less manifests are DENIED (fail-closed).",
        params: &[
            ParamDef {
                name: "hub",
                description: "Hub id (exact): clawhub or lobehub. github is discovery-only and will be denied by the gate.",
                required: true,
            },
            ParamDef {
                name: "skill_name",
                description: "Skill slug/identifier on that hub",
                required: true,
            },
            ParamDef {
                name: "owner",
                description: "Publisher handle — required when the hub reports the slug as ambiguous (clawhub 409)",
                required: false,
            },
            ParamDef {
                name: "scope",
                description: "Install target: 'global' (default, all agents), 'department:<name>' (only agents in that department), or an agent id (that agent only)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "skill_curator_status",
        description: "Report the G5 curator lifecycle state: stale (30d unused), archived (90d, recoverable), pinned, and approaching-stale skills. Optionally force a maintenance pass now.",
        params: &[ParamDef {
            name: "run",
            description: "true = run a curator pass immediately (default: false, report only)",
            required: false,
        }],
    },
    ToolDef {
        name: "skill_pin",
        description: "Pin or unpin a skill for the curator. Pinned skills are exempt from the 30-day stale flag and 90-day archive; pinning an archived skill restores its file from the archive.",
        params: &[
            ParamDef {
                name: "skill_name",
                description: "Skill name (machine identity)",
                required: true,
            },
            ParamDef {
                name: "scope",
                description: "'global' (default) or an agent id",
                required: false,
            },
            ParamDef {
                name: "pinned",
                description: "true (default) to pin, false to unpin",
                required: false,
            },
        ],
    },
];
