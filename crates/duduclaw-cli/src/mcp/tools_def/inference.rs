//! Web fetch/extract plus the local inference and model tools.
//!
//! Part of the split `tools_def` registration face (O7). Every `description`
//! here is bounded by the O7 budget (<=200 bytes for a tool, <=200 bytes for a
//! parameter) and enforced by `mcp::tools_list_budget_tests` — long prose
//! belongs in `docs/guides/mcp-tools.md`, not on the wire.

use super::super::{ParamDef, ToolDef};

pub(super) const TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "web_fetch_cached",
        description: "Fetch a URL over HTTP with SSRF protection, disk caching and rate limiting (browser automation L1 — try this before headless browsers). Returns status, content type and body, truncated at 60k chars.",
        params: &[
            ParamDef {
                name: "url",
                description: "The http(s) URL to fetch (internal hosts and cloud metadata endpoints are blocked)",
                required: true,
            },
            ParamDef {
                name: "ttl_seconds",
                description: "Cache TTL in seconds (default 86400 = 24h; 0 also means default)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "web_extract",
        description: "Fetch a URL and extract elements with a CSS selector (browser automation L2 — static scrape). Formats: text (default), html, json (structured with attributes/children).",
        params: &[
            ParamDef {
                name: "url",
                description: "The http(s) URL to fetch (SSRF-validated, cached)",
                required: true,
            },
            ParamDef {
                name: "selector",
                description: "CSS selector, e.g. 'h1', '.article p', 'a[href]'",
                required: true,
            },
            ParamDef {
                name: "format",
                description: "Output format: text | html | json (default text)",
                required: false,
            },
        ],
    },
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
        params: &[ParamDef {
            name: "model_id",
            description: "Model ID or filename (e.g., 'qwen3-8b-q4_k_m')",
            required: true,
        }],
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
            ParamDef {
                name: "prompt",
                description: "The user prompt to test routing for",
                required: true,
            },
            ParamDef {
                name: "system_prompt",
                description: "Optional system prompt context",
                required: false,
            },
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
        params: &[ParamDef {
            name: "file",
            description: "llamafile filename (optional, uses default)",
            required: false,
        }],
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
    ToolDef {
        name: "model_search",
        description: "Search for GGUF models from curated recommendations and HuggingFace. Results are filtered by available RAM. Trusted repos are marked [推薦].",
        params: &[ParamDef {
            name: "query",
            description: "Search query (e.g., 'qwen 8b', 'code llama', 'gemma')",
            required: true,
        }],
    },
    ToolDef {
        name: "model_download",
        description: "Download a GGUF model from HuggingFace to ~/.duduclaw/models/. Supports resume and mirror fallback.",
        params: &[
            ParamDef {
                name: "repo",
                description: "HuggingFace repo (e.g., 'Qwen/Qwen3-8B-GGUF')",
                required: true,
            },
            ParamDef {
                name: "filename",
                description: "GGUF filename (e.g., 'qwen3-8b-q4_k_m.gguf')",
                required: true,
            },
        ],
    },
    ToolDef {
        name: "model_recommend",
        description: "Get hardware-aware model recommendations based on detected GPU and available RAM.",
        params: &[],
    },
];
