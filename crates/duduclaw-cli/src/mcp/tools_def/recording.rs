//! Browser / desktop recording and recording-to-skill distillation.
//!
//! Part of the split `tools_def` registration face (O7). Every `description`
//! here is bounded by the O7 budget (<=200 bytes for a tool, <=200 bytes for a
//! parameter) and enforced by `mcp::tools_list_budget_tests` — long prose
//! belongs in `docs/guides/mcp-tools.md`, not on the wire.

use super::super::{ParamDef, ToolDef};

pub(super) const TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "browser_record_start",
        description: "Start a browser recording: opens a Playwright browser at `url` with tracing, HAR capture and a UI-action recorder for a human to demonstrate an SOP. Requires [capabilities] recording = true.",
        params: &[
            ParamDef {
                name: "url",
                description: "http(s):// URL to open for the demonstration",
                required: true,
            },
            ParamDef {
                name: "name",
                description: "Human-readable recording name (becomes the default skill name)",
                required: false,
            },
            ParamDef {
                name: "headless",
                description: "Run headless (default false — recordings are human-driven)",
                required: false,
            },
            ParamDef {
                name: "max_seconds",
                description: "Auto-stop cap in seconds (default 1800, max 7200)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "browser_record_stop",
        description: "Stop a browser recording: flushes trace.zip / session.har / actions.json, then redacts the HAR in place (auth, cookies, token-like values). Requires [capabilities] recording = true.",
        params: &[ParamDef {
            name: "id",
            description: "Recording id returned by browser_record_start",
            required: true,
        }],
    },
    ToolDef {
        name: "desktop_record_start",
        description: "Start a desktop recording (macOS): 1 fps screenshots plus foreground app/window titles. Typed content is NEVER captured. Requires [capabilities] recording = true.",
        params: &[
            ParamDef {
                name: "name",
                description: "Human-readable recording name",
                required: false,
            },
            ParamDef {
                name: "max_seconds",
                description: "Auto-stop cap in seconds (default 1800, max 7200)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "desktop_record_stop",
        description: "Stop a desktop recording session and report captured frame/event counts. Requires the agent's [capabilities] recording = true.",
        params: &[ParamDef {
            name: "id",
            description: "Recording id returned by desktop_record_start",
            required: true,
        }],
    },
    ToolDef {
        name: "skill_from_recording",
        description: "Distill a finished recording into a draft SKILL.md. The draft is security-scanned and staged into the approval pipeline — NEVER installed directly; a human must accept it in the dashboard first.",
        params: &[
            ParamDef {
                name: "id",
                description: "Recording id (must be stopped first)",
                required: true,
            },
            ParamDef {
                name: "name",
                description: "Skill name override (slugified; defaults to the recording name)",
                required: false,
            },
        ],
    },
];
