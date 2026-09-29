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
        description: "Execute a program that can call DuDuClaw MCP tools via RPC. Only final stdout enters context.",
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
        description: "Capture a screenshot of the virtual display (L5 container) or host screen (L5b native). Returns base64-encoded PNG. Requires computer_use capability.",
        params: &[ParamDef {
            name: "display",
            description: "Which display to capture: 'container' (default) or 'native'",
            required: false,
        }],
    },
    ToolDef {
        name: "computer_click",
        description: "Click at specific coordinates on the screen. Requires computer_use capability.",
        params: &[
            ParamDef {
                name: "x",
                description: "X coordinate",
                required: true,
            },
            ParamDef {
                name: "y",
                description: "Y coordinate",
                required: true,
            },
            ParamDef {
                name: "button",
                description: "Mouse button: 'left' (default), 'right', 'middle'",
                required: false,
            },
            ParamDef {
                name: "double",
                description: "Double-click if true (default: false)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "computer_type",
        description: "Type text at the current cursor position. Requires computer_use capability.",
        params: &[ParamDef {
            name: "text",
            description: "Text to type",
            required: true,
        }],
    },
    ToolDef {
        name: "computer_key",
        description: "Press a key combination (e.g., 'ctrl+s', 'Return', 'Tab'). Requires computer_use capability.",
        params: &[ParamDef {
            name: "key",
            description: "Key combination (e.g., 'ctrl+c', 'Return', 'alt+Tab')",
            required: true,
        }],
    },
    ToolDef {
        name: "computer_scroll",
        description: "Scroll at specific coordinates. Requires computer_use capability.",
        params: &[
            ParamDef {
                name: "x",
                description: "X coordinate",
                required: true,
            },
            ParamDef {
                name: "y",
                description: "Y coordinate",
                required: true,
            },
            ParamDef {
                name: "direction",
                description: "Scroll direction: 'up' or 'down' (default: 'down')",
                required: false,
            },
            ParamDef {
                name: "amount",
                description: "Number of scroll clicks (default: 3)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "computer_session_start",
        description: "Start a new Computer Use session with a virtual display container. Returns session_id on success. Requires computer_use capability.",
        params: &[
            ParamDef {
                name: "task",
                description: "Description of what to accomplish",
                required: true,
            },
            ParamDef {
                name: "width",
                description: "Display width in pixels (default: 1280)",
                required: false,
            },
            ParamDef {
                name: "height",
                description: "Display height in pixels (default: 800)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "computer_session_stop",
        description: "Stop an active Computer Use session and clean up the container.",
        params: &[ParamDef {
            name: "session_id",
            description: "Session ID returned by computer_session_start",
            required: true,
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
