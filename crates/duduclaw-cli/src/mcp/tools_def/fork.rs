//! RFC-26 Live Run Forking.
//!
//! Part of the split `tools_def` registration face (O7). Every `description`
//! here is bounded by the O7 budget (<=200 bytes for a tool, <=200 bytes for a
//! parameter) and enforced by `mcp::tools_list_budget_tests` — long prose
//! belongs in `docs/guides/mcp-tools.md`, not on the wire.

use super::super::{ParamDef, ToolDef};

pub(super) const TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "fork_run",
        description: "Split the current task into N competing branches exploring different strategies in parallel, each in an isolated copy-on-write workspace with its own budget. Pick a winner later with merge_or_select.",
        params: &[
            ParamDef {
                name: "prompt",
                description: "The base task all branches work on",
                required: true,
            },
            ParamDef {
                name: "n",
                description: "Number of branches (capped at [fork] max_branches). Defaults to the number of strategies, min 2.",
                required: false,
            },
            ParamDef {
                name: "strategies",
                description: "Optional array of per-branch steering messages (e.g. [\"MVP-first\", \"risk-first\"]). Branch i gets strategies[i].",
                required: false,
            },
            ParamDef {
                name: "budget_usd",
                description: "Per-branch spend cap in USD (default: [fork] default_budget_usd)",
                required: false,
            },
            ParamDef {
                name: "merge_mode",
                description: "manual | auto | auto_with_fallback | vote (default: [fork] merge_mode)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "inspect_branches",
        description: "List a fork's branches with their state, steering, spend, and test result.",
        params: &[ParamDef {
            name: "fork_id",
            description: "Fork id returned by fork_run",
            required: true,
        }],
    },
    ToolDef {
        name: "diff_branches",
        description: "Show the outputs of two branches in a fork side by side.",
        params: &[
            ParamDef {
                name: "fork_id",
                description: "Fork id",
                required: true,
            },
            ParamDef {
                name: "branch_a",
                description: "First branch id",
                required: true,
            },
            ParamDef {
                name: "branch_b",
                description: "Second branch id",
                required: true,
            },
        ],
    },
    ToolDef {
        name: "merge_or_select",
        description: "Resolve a fork. Pass branch_id to select a winner explicitly; omit it to let the judge decide (judge auto-selection requires the execution backend). Promotes the winner's workspace.",
        params: &[
            ParamDef {
                name: "fork_id",
                description: "Fork id",
                required: true,
            },
            ParamDef {
                name: "branch_id",
                description: "Optional explicit winner branch id",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "terminate_branch",
        description: "Terminate a runaway branch in a fork (kills its subprocess and marks it terminated).",
        params: &[
            ParamDef {
                name: "fork_id",
                description: "Fork id",
                required: true,
            },
            ParamDef {
                name: "branch_id",
                description: "Branch id to terminate",
                required: true,
            },
        ],
    },
    ToolDef {
        name: "fork_cost",
        description: "Report aggregate and per-branch spend for a fork.",
        params: &[ParamDef {
            name: "fork_id",
            description: "Fork id",
            required: true,
        }],
    },
];
