//! Program / office-script execution, skill bank and Computer Use.
//!
//! Part of the split `tools_def` registration face (O7). Every `description`
//! here is bounded by the O7 budget (<=200 bytes for a tool, <=200 bytes for a
//! parameter) and enforced by `mcp::tools_list_budget_tests` — long prose
//! belongs in `docs/guides/mcp-tools.md`, not on the wire.

use super::super::{ParamDef, ToolDef};

pub(super) const TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "skill_extract",
        description: "Extract structured knowledge from a skill into the agent's wiki. Creates concept pages, entity pages, and a source summary. Zero LLM cost (heuristic mode).",
        params: &[
            ParamDef {
                name: "skill_name",
                description: "Name of the skill to extract from",
                required: true,
            },
            ParamDef {
                name: "agent_id",
                description: "Agent name (default: main agent)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "execute_program",
        description: "Run a python, bash or javascript script in an offline, non-root container and return its output. The script cannot call DuDuClaw tools. Only final stdout enters context.",
        params: &[
            ParamDef {
                name: "code",
                description: "Source code to execute",
                required: true,
            },
            ParamDef {
                name: "language",
                description: "Language: 'python', 'bash', or 'javascript'",
                required: true,
            },
            ParamDef {
                name: "timeout_seconds",
                description: "Execution timeout in seconds (default: 30, max: 300)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "office_script",
        description: "Run a bundled office skill's Python script (docx/xlsx/pptx/pdf) to read or create a document. Use instead of Bash when you have no shell. End your reply with `DELIVER:<absolute path>` to hand it over.",
        params: &[
            ParamDef {
                name: "skill",
                description: "Skill to run: one of docx, xlsx, pptx, pdf",
                required: true,
            },
            ParamDef {
                name: "script",
                description: "Script name without any path: create, extract, or to_pdf (the .py suffix is optional)",
                required: true,
            },
            ParamDef {
                name: "args",
                description: "JSON array of string arguments, e.g. [\"outline.md\", \"--out\", \"/abs/agent-dir/attachments/deck.pptx\"]. Any path must resolve inside your agent directory.",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "skill_bank_search",
        description: "[deprecated → skill_search source=\"bank\"; removed in v1.68.0] Search the skill bank for learned skills matching a query. Returns ranked results with confidence scores.",
        params: &[
            ParamDef {
                name: "query",
                description: "Search query to match against skill names and descriptions",
                required: true,
            },
            ParamDef {
                name: "limit",
                description: "Maximum number of results to return (default: 5)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "skill_bank_feedback",
        description: "Provide success/failure feedback for a skill execution. Updates confidence via Bayesian update.",
        params: &[
            ParamDef {
                name: "skill_id",
                description: "ID of the skill to provide feedback for",
                required: true,
            },
            ParamDef {
                name: "success",
                description: "Whether the skill execution was successful (true/false)",
                required: true,
            },
        ],
    },
    ToolDef {
        name: "computer_screenshot",
        description: "See your computer-use session: returns a PNG image (sensitive areas masked) plus actions used and time left. Actions never return a screenshot; call this to check.",
        params: &[],
    },
    ToolDef {
        name: "computer_click",
        description: "Click in your computer-use session at screenshot pixel (x, y). Left button by default; button='right', or double=true for a left double-click.",
        params: &[
            ParamDef {
                name: "x",
                description: "X pixel from the left edge (0-based, within the display width)",
                required: true,
            },
            ParamDef {
                name: "y",
                description: "Y pixel from the top edge (0-based, within the display height)",
                required: true,
            },
            ParamDef {
                name: "button",
                description: "'left' (default) or 'right'",
                required: false,
            },
            ParamDef {
                name: "double",
                description: "true for a double-click (left button only)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "computer_type",
        description: "Type text into the focused element of your computer-use session (max 2,000 characters). Risky input may need a person to confirm in the chat.",
        params: &[ParamDef {
            name: "text",
            description: "Text to type",
            required: true,
        }],
    },
    ToolDef {
        name: "computer_key",
        description: "Press a key or key combination in your computer-use session, e.g. 'Return', 'Tab', 'ctrl+s'. Letters, digits, '+', '-', '_' only.",
        params: &[ParamDef {
            name: "key",
            description: "Key or combination, e.g. 'Return', 'ctrl+c', 'alt+Tab'",
            required: true,
        }],
    },
    ToolDef {
        name: "computer_scroll",
        description: "Scroll in your computer-use session with the pointer at pixel (x, y).",
        params: &[
            ParamDef {
                name: "x",
                description: "X pixel from the left edge",
                required: true,
            },
            ParamDef {
                name: "y",
                description: "Y pixel from the top edge",
                required: true,
            },
            ParamDef {
                name: "direction",
                description: "'up' or 'down' (default)",
                required: false,
            },
            ParamDef {
                name: "amount",
                description: "Scroll clicks, 1-20 (default 3)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "computer_session_start",
        description: "Start your own computer-use session: a virtual display in an isolated container; network only to allowlisted sites. One per employee; ends on stop, after 2 idle minutes, or at its limits.",
        params: &[
            ParamDef {
                name: "task",
                description: "What the session is for (kept in the audit log)",
                required: false,
            },
            ParamDef {
                name: "width",
                description: "Display width in pixels, 320-1920 (default from agent.toml, usually 1280)",
                required: false,
            },
            ParamDef {
                name: "height",
                description: "Display height in pixels, 240-1200 (default from agent.toml, usually 800)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "computer_navigate",
        description: "Open an https:// page in your computer-use session's browser. Only sites on your allowlist (listed at session start) are reachable. Counts as one action.",
        params: &[ParamDef {
            name: "url",
            description: "Full https:// URL whose host is on your allowlist, e.g. https://example.com/page",
            required: true,
        }],
    },
    ToolDef {
        name: "computer_session_stop",
        description: "Stop your computer-use session and remove its container. Call it when you are done.",
        params: &[ParamDef {
            name: "session_id",
            description: "Optional; defaults to your active session",
            required: false,
        }],
    },
    ToolDef {
        name: "session_restore_context",
        description: "Search hidden/archived messages in the current session to restore relevant context.",
        params: &[ParamDef {
            name: "query",
            description: "Search query to find relevant archived messages",
            required: true,
        }],
    },
];
