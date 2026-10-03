//! Channels, agent messaging, cron scheduling and reminders.
//!
//! Part of the split `tools_def` registration face (O7). Every `description`
//! here is bounded by the O7 budget (<=200 bytes for a tool, <=200 bytes for a
//! parameter) and enforced by `mcp::tools_list_budget_tests` — long prose
//! belongs in `docs/guides/mcp-tools.md`, not on the wire.

use super::super::{ParamDef, ToolDef};

pub(super) const TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "send_message",
        description: "Send a message to a channel (Telegram/LINE/Discord/Slack/WhatsApp/Feishu/Google Chat/Teams/WeCom/DingTalk)",
        params: &[
            ParamDef {
                name: "channel",
                description: "Channel type (telegram, line, discord, slack, whatsapp, feishu, googlechat, teams, wecom, dingtalk)",
                required: true,
            },
            ParamDef {
                name: "chat_id",
                description: "Chat/group ID",
                required: true,
            },
            ParamDef {
                name: "text",
                description: "Message text",
                required: true,
            },
        ],
    },
    ToolDef {
        name: "send_photo",
        description: "Send a photo to a channel",
        params: &[
            ParamDef {
                name: "channel",
                description: "Channel type",
                required: true,
            },
            ParamDef {
                name: "chat_id",
                description: "Chat/group ID",
                required: true,
            },
            ParamDef {
                name: "url_or_path",
                description: "URL or file path of the photo",
                required: true,
            },
        ],
    },
    ToolDef {
        name: "send_sticker",
        description: "Send a sticker (LINE only)",
        params: &[
            ParamDef {
                name: "chat_id",
                description: "Chat/group ID",
                required: true,
            },
            ParamDef {
                name: "sticker_id",
                description: "LINE sticker ID",
                required: true,
            },
        ],
    },
    ToolDef {
        name: "web_search",
        description: "Search the web",
        params: &[ParamDef {
            name: "query",
            description: "Search query",
            required: true,
        }],
    },
    ToolDef {
        name: "send_to_agent",
        description: "Delegate task to another agent. Delegation depth is tracked automatically via environment to prevent infinite loops (max 5 hops).",
        params: &[
            ParamDef {
                name: "agent_id",
                description: "Target agent ID",
                required: true,
            },
            ParamDef {
                name: "prompt",
                description: "Prompt/task for the agent",
                required: true,
            },
        ],
    },
    ToolDef {
        name: "schedule_task",
        description: "[deprecated → tasks_create schedule=\"<cron>\"; removed in v1.69.0] Schedule a recurring task in the persistent CronScheduler. Survives restarts; can target any agent via agent_id.",
        params: &[
            ParamDef {
                name: "cron",
                description: "Cron expression (5 fields, or 6 with seconds). Evaluated in `cron_timezone` when set, else UTC. E.g. '0 9 * * *' with cron_timezone='Asia/Taipei' fires 09:00 Taipei daily.",
                required: true,
            },
            ParamDef {
                name: "task",
                description: "Task prompt sent to the target agent when the cron fires. Write it as an instruction the agent will follow.",
                required: true,
            },
            ParamDef {
                name: "name",
                description: "Human-readable task name for listing / pausing / deleting later (e.g., 'xianwen-pm-daily-research').",
                required: true,
            },
            ParamDef {
                name: "agent_id",
                description: "Target agent that will execute the task (e.g. 'xianwen-pm', 'duduclaw-tl'). Defaults to 'default' if omitted — explicit is strongly recommended.",
                required: false,
            },
            ParamDef {
                name: "notify_channel",
                description: "Optional: channel type to auto-deliver the result to. With notify_chat_id, the response is sent there after a successful run.",
                required: false,
            },
            ParamDef {
                name: "notify_chat_id",
                description: "Optional: chat / channel / room ID on the notify platform. Required when notify_channel is set.",
                required: false,
            },
            ParamDef {
                name: "notify_thread_id",
                description: "Optional: Discord thread ID. Only used when notify_channel='discord' and the result should land in a specific thread.",
                required: false,
            },
            ParamDef {
                name: "cron_timezone",
                description: "Optional IANA timezone for the cron expression (e.g. 'Asia/Taipei'). Omit to auto-detect the host's local zone; pass 'UTC' to force UTC.",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "list_cron_tasks",
        description: "List scheduled cron tasks. Returns tasks owned by the calling agent (or all tasks if agent_id is omitted).",
        params: &[
            ParamDef {
                name: "agent_id",
                description: "Filter by agent ID (default: calling agent)",
                required: false,
            },
            ParamDef {
                name: "enabled_only",
                description: "Only show enabled tasks (default: false)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "update_cron_task",
        description: "Update a scheduled cron task by ID or name. Only the fields you provide will be changed.",
        params: &[
            ParamDef {
                name: "id",
                description: "Task ID to update",
                required: false,
            },
            ParamDef {
                name: "name",
                description: "Task name to update (used if id is omitted)",
                required: false,
            },
            ParamDef {
                name: "cron",
                description: "New cron expression",
                required: false,
            },
            ParamDef {
                name: "task",
                description: "New task description/prompt",
                required: false,
            },
            ParamDef {
                name: "new_name",
                description: "Rename the task",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "delete_cron_task",
        description: "Delete a scheduled cron task by ID or name",
        params: &[
            ParamDef {
                name: "id",
                description: "Task ID to delete",
                required: false,
            },
            ParamDef {
                name: "name",
                description: "Task name to delete (used if id is omitted)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "pause_cron_task",
        description: "Pause or resume a scheduled cron task by ID or name",
        params: &[
            ParamDef {
                name: "id",
                description: "Task ID",
                required: false,
            },
            ParamDef {
                name: "name",
                description: "Task name (used if id is omitted)",
                required: false,
            },
            ParamDef {
                name: "enabled",
                description: "Set to true to resume, false to pause (default: false)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "run_cron_task",
        description: "Run a scheduled cron task once right now (test execution) by id or name, through the exact scheduled-fire path including any trigger gate. Blocks until done, records the run, ignores enabled state.",
        params: &[
            ParamDef {
                name: "id",
                description: "Task ID to run",
                required: false,
            },
            ParamDef {
                name: "name",
                description: "Task name to run (used if id is omitted)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "create_reminder",
        description: "Create a one-shot reminder that messages a channel at a time (relative 5m/2h/1d or ISO 8601). 'direct' sends a static message at zero LLM cost; 'agent_callback' wakes the agent to answer.",
        params: &[
            ParamDef {
                name: "time",
                description: "When to trigger: relative (5m, 2h, 1d, 1h30m) or absolute ISO 8601 (2026-04-07T15:00:00+08:00)",
                required: true,
            },
            ParamDef {
                name: "message",
                description: "Message text to send (required for direct mode)",
                required: true,
            },
            ParamDef {
                name: "channel",
                description: "Channel type (telegram, line, discord, slack, whatsapp, feishu, googlechat, teams, wecom, dingtalk)",
                required: true,
            },
            ParamDef {
                name: "chat_id",
                description: "Chat/group/channel ID to send the reminder to",
                required: true,
            },
            ParamDef {
                name: "mode",
                description: "Delivery mode: 'direct' (default, zero cost) or 'agent_callback' (wakes agent with prompt)",
                required: false,
            },
            ParamDef {
                name: "prompt",
                description: "Prompt for the agent (required when mode=agent_callback)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "list_reminders",
        description: "List reminders, optionally filtered by status and agent",
        params: &[
            ParamDef {
                name: "status",
                description: "Filter by status: pending, delivered, failed, cancelled (default: pending)",
                required: false,
            },
            ParamDef {
                name: "agent_id",
                description: "Filter by agent ID",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "cancel_reminder",
        description: "Cancel a pending reminder by ID",
        params: &[ParamDef {
            name: "id",
            description: "Reminder ID to cancel",
            required: true,
        }],
    },
];
