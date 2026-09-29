//! Task board, goals, plans, activity feed, autopilot and shared skills.
//!
//! Part of the split `tools_def` registration face (O7). Every `description`
//! here is bounded by the O7 budget (<=200 bytes for a tool, <=200 bytes for a
//! parameter) and enforced by `mcp::tools_list_budget_tests` — long prose
//! belongs in `docs/guides/mcp-tools.md`, not on the wire.

use super::super::{ParamDef, ToolDef};

pub(super) const TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "tasks_list",
        description: "List tasks from the shared Kanban board. Defaults to tasks assigned to the calling agent. Pass assigned_to='*' for all agents. Use this to see your task queue.",
        params: &[
            ParamDef {
                name: "status",
                description: "Filter by status: todo / in_progress / done / blocked",
                required: false,
            },
            ParamDef {
                name: "priority",
                description: "Filter by priority: low / medium / high / urgent",
                required: false,
            },
            ParamDef {
                name: "assigned_to",
                description: "Filter by agent ID. Defaults to caller. Pass '*' for all agents.",
                required: false,
            },
            ParamDef {
                name: "limit",
                description: "Max results (default 20, max 100)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "tasks_create",
        description: "Create work — one entry point for tasks, autonomous goals and scheduled work. Default: a Kanban task. kind='goal': an AI judge panel decides done. schedule=<cron|RFC3339>: recurring or one-shot.",
        params: &[
            ParamDef {
                name: "title",
                description: "Task title (required, <200 chars)",
                required: true,
            },
            ParamDef {
                name: "kind",
                description: "What to create: 'task' (Kanban board task, default) or 'goal' (autonomous goal — judge acceptance, frozen contract, optional plan_first)",
                required: false,
            },
            ParamDef {
                name: "schedule",
                description: "Run on a schedule instead of now: a cron expression ('0 9 * * *') for recurring work, or an RFC3339 instant for a one-shot wake-up. Cannot be combined with kind='goal'.",
                required: false,
            },
            ParamDef {
                name: "notify_channel",
                description: "With schedule: channel to deliver the run's result to. Required for a one-shot schedule.",
                required: false,
            },
            ParamDef {
                name: "notify_chat_id",
                description: "With schedule: chat / channel / room id on notify_channel.",
                required: false,
            },
            ParamDef {
                name: "notify_thread_id",
                description: "With a cron schedule on Discord: thread id the result should land in.",
                required: false,
            },
            ParamDef {
                name: "cron_timezone",
                description: "With a cron schedule: IANA timezone the expression is evaluated in (e.g. 'Asia/Taipei'). Omit to auto-detect the host timezone.",
                required: false,
            },
            ParamDef {
                name: "outcome",
                description: "With kind='goal': structured outcome spec (deterministic pre-judge acceptance). Malformed specs refuse the whole create.",
                required: false,
            },
            ParamDef {
                name: "duration_hours",
                description: "With kind='goal': per-goal wall clock in hours (1-720). Omit for the deployment default.",
                required: false,
            },
            ParamDef {
                name: "risk_boundary",
                description: "With kind='goal': what the agent must not do while pursuing it. Omit for the deployment baseline boundary.",
                required: false,
            },
            ParamDef {
                name: "require_beliefs",
                description: "With kind='goal': require structured belief_submit / belief_settle declarations, and fold them into the acceptance bar.",
                required: false,
            },
            ParamDef {
                name: "plan_first",
                description: "With kind='goal': generate a plan and park the goal for human approval instead of starting it.",
                required: false,
            },
            ParamDef {
                name: "description",
                description: "Markdown description",
                required: false,
            },
            ParamDef {
                name: "assigned_to",
                description: "Agent ID to assign the task to. Defaults to caller.",
                required: false,
            },
            ParamDef {
                name: "priority",
                description: "low / medium / high / urgent (default: medium)",
                required: false,
            },
            ParamDef {
                name: "tags",
                description: "Comma-separated tags",
                required: false,
            },
            ParamDef {
                name: "parent_task_id",
                description: "Parent task ID for sub-tasks",
                required: false,
            },
            ParamDef {
                name: "goal_id",
                description: "Goal this task serves (see goals_create). Its why-chain (Initiative → Project → Issue) is shown to the assignee in their task queue.",
                required: false,
            },
            ParamDef {
                name: "depends_on",
                description: "Task ids (JSON array or comma-separated) that must be 'done' before this task can be claimed. Cycles are rejected.",
                required: false,
            },
            ParamDef {
                name: "goal_mode",
                description: "If true, completion goes through judge acceptance (acceptance_criteria) before 'done'.",
                required: false,
            },
            ParamDef {
                name: "acceptance_criteria",
                description: "Criteria the judge checks when goal_mode is set.",
                required: false,
            },
            ParamDef {
                name: "max_retries",
                description: "Requeue cap for zombie reclaim / goal rejection (default 3).",
                required: false,
            },
            ParamDef {
                name: "durable",
                description: "Force the durable dispatch lifecycle (status 'pending' + atomic claim + lease).",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "tasks_update",
        description: "Update task fields. For common state transitions use tasks_claim / tasks_complete / tasks_block instead.",
        params: &[
            ParamDef {
                name: "task_id",
                description: "Task ID",
                required: true,
            },
            ParamDef {
                name: "title",
                description: "New title",
                required: false,
            },
            ParamDef {
                name: "description",
                description: "New description",
                required: false,
            },
            ParamDef {
                name: "priority",
                description: "New priority",
                required: false,
            },
            ParamDef {
                name: "tags",
                description: "New comma-separated tags",
                required: false,
            },
            ParamDef {
                name: "assigned_to",
                description: "Reassign to a different agent ID. Subject to the same department/hierarchy delegation rules as tasks_create; use tasks_claim to take a task yourself.",
                required: false,
            },
            ParamDef {
                name: "depends_on",
                description: "New dependency list (JSON array or comma-separated task ids). Dependency cycles are rejected.",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "tasks_claim",
        description: "Atomically claim a task: one worker wins (compare-and-set), it reassigns to you, moves to in_progress, and stamps a lease so a crashed worker is reclaimable. Posts a task_assigned activity event.",
        params: &[ParamDef {
            name: "task_id",
            description: "Task ID to claim",
            required: true,
        }],
    },
    ToolDef {
        name: "tasks_renew",
        description: "Heartbeat for a claimed task: extend the lease you hold so long-running work is not reclaimed as a zombie. Call well within the lease window while still working. Only the claiming agent can renew.",
        params: &[ParamDef {
            name: "task_id",
            description: "Task ID you claimed via tasks_claim",
            required: true,
        }],
    },
    ToolDef {
        name: "tasks_complete",
        description: "Mark a task as done and post a task_completed activity event with the optional summary.",
        params: &[
            ParamDef {
                name: "task_id",
                description: "Task ID",
                required: true,
            },
            ParamDef {
                name: "summary",
                description: "Optional completion summary (posted to the activity feed)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "tasks_block",
        description: "Mark a task as blocked with a reason. Posts a task_blocked activity event.",
        params: &[
            ParamDef {
                name: "task_id",
                description: "Task ID",
                required: true,
            },
            ParamDef {
                name: "reason",
                description: "Blocker reason (required, shown on the card)",
                required: true,
            },
        ],
    },
    ToolDef {
        name: "capability_request",
        description: "Request a task-scoped grant for a tool in your agent's scoped_tools (denied by default). Files a human approval; once approved the grant lasts until the task phase ends or its TTL elapses.",
        params: &[
            ParamDef {
                name: "tool",
                description: "The tool name you need a grant for (must be in your scoped_tools)",
                required: true,
            },
            ParamDef {
                name: "reason",
                description: "Why you need this tool now (shown to the human approver)",
                required: true,
            },
            ParamDef {
                name: "task_id",
                description: "Bind the grant to this task; revoked when the task phase ends. Omit for an agent-level grant.",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "goals_create",
        description: "Create a node in the goal HIERARCHY (Initiative → Project → Issue) — the why-chain assignees see. Not an autonomous goal: for that use tasks_create kind=\"goal\". Cycles are rejected.",
        params: &[
            ParamDef {
                name: "title",
                description: "Goal title (required, <200 chars)",
                required: true,
            },
            ParamDef {
                name: "description",
                description: "The WHY — rationale carried down to agents working linked tasks",
                required: false,
            },
            ParamDef {
                name: "parent_goal_id",
                description: "Parent goal ID (must exist; cycles rejected)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "goals_list",
        description: "List goals in the goal hierarchy, including each goal's parent linkage.",
        params: &[
            ParamDef {
                name: "status",
                description: "Filter by status: active / done / archived",
                required: false,
            },
            ParamDef {
                name: "limit",
                description: "Max results (default 50, max 200)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "plan_get",
        description: "Read the shared plan you co-edit with your user; without plan_id returns your most recently updated active plan. Update only the steps assigned to you, via plan_update_step — never the user's.",
        params: &[ParamDef {
            name: "plan_id",
            description: "Plan ID. Omit to get your most recently updated active plan.",
            required: false,
        }],
    },
    ToolDef {
        name: "plan_update_step",
        description: "Update a shared-plan step assigned to YOU (enforced). Tick progress todo → doing → done (or skipped), and optionally refine the step text.",
        params: &[
            ParamDef {
                name: "step_id",
                description: "Plan step ID (from plan_get)",
                required: true,
            },
            ParamDef {
                name: "status",
                description: "New status: todo / doing / done / skipped",
                required: false,
            },
            ParamDef {
                name: "text",
                description: "Refined step text (optional)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "activity_post",
        description: "Post a progress / comment event to the Activity Feed. Use to report intermediate progress without changing task status.",
        params: &[
            ParamDef {
                name: "summary",
                description: "One-line human-readable summary (required)",
                required: true,
            },
            ParamDef {
                name: "task_id",
                description: "Optional task ID to link the activity to",
                required: false,
            },
            ParamDef {
                name: "event_type",
                description: "Event type (progress, comment, info, etc.). Default: agent_comment",
                required: false,
            },
            ParamDef {
                name: "metadata",
                description: "Optional JSON metadata blob",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "activity_list",
        description: "List recent Activity Feed events. Filterable by agent / task / type.",
        params: &[
            ParamDef {
                name: "task_id",
                description: "Filter by task ID",
                required: false,
            },
            ParamDef {
                name: "agent_id",
                description: "Filter by agent ID (default: caller)",
                required: false,
            },
            ParamDef {
                name: "event_type",
                description: "Filter by event type",
                required: false,
            },
            ParamDef {
                name: "limit",
                description: "Max results (default 20, max 100)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "autopilot_list",
        description: "List automation rules (read-only for agents). Rule creation / edit is restricted to the web dashboard.",
        params: &[ParamDef {
            name: "enabled_only",
            description: "Only show enabled rules (default: true)",
            required: false,
        }],
    },
    ToolDef {
        name: "shared_skill_list",
        description: "List skills in the team-shared skill pool (~/.duduclaw/shared/skills/). Skills shared by other agents are available for adoption.",
        params: &[ParamDef {
            name: "tag",
            description: "Filter by tag",
            required: false,
        }],
    },
    ToolDef {
        name: "shared_skill_share",
        description: "Share one of your own skills to the team-shared skill pool. Skill must already exist in your agent's SKILLS/ directory.",
        params: &[ParamDef {
            name: "skill_name",
            description: "Skill name (matches SKILLS/<name>.md)",
            required: true,
        }],
    },
    ToolDef {
        name: "shared_skill_adopt",
        description: "Adopt a shared skill into an agent's SKILLS directory. Bumps usage_count on the shared skill and records the adopter.",
        params: &[
            ParamDef {
                name: "skill_name",
                description: "Shared skill name",
                required: true,
            },
            ParamDef {
                name: "target_agent",
                description: "Agent to adopt the skill into (default: caller)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "plan_start",
        description: "Start clarify-first planning for an ambiguous task: returns up to 3 clarifying questions to ask the user before executing, then a decomposition instruction. Honours agent.toml [planner] clarify_first.",
        params: &[ParamDef {
            name: "task",
            description: "The task to plan",
            required: true,
        }],
    },
    ToolDef {
        name: "memory_improve",
        description: "Reflect over your memories about a topic and get a clustered proposal scaffold for consolidated rules. Returns candidates only — review, then persist with memory_store. Writes nothing itself.",
        params: &[
            ParamDef {
                name: "topic",
                description: "The area to reflect on (e.g. 'refund handling', 'deploy mistakes')",
                required: true,
            },
            ParamDef {
                name: "limit",
                description: "Max memories to examine (default 40, max 100)",
                required: false,
            },
        ],
    },
];
