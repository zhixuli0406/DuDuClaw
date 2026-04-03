//! MCP (Model Context Protocol) server implementation.
//!
//! Communicates via stdin/stdout using JSON-RPC 2.0.
//! Exposes DuDuClaw tools for Claude Code integration.

use std::path::Path;

use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_memory::SqliteMemoryEngine;
use duduclaw_core::traits::MemoryEngine;
use duduclaw_core::types::MemoryEntry;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::{info, warn};

// ── Tool definitions ─────────────────────────────────────────

struct ToolDef {
    name: &'static str,
    description: &'static str,
    params: &'static [ParamDef],
}

struct ParamDef {
    name: &'static str,
    description: &'static str,
    required: bool,
}

const TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "send_message",
        description: "Send a message to a channel (Telegram/LINE/Discord)",
        params: &[
            ParamDef { name: "channel", description: "Channel type (telegram, line, discord)", required: true },
            ParamDef { name: "chat_id", description: "Chat/group ID", required: true },
            ParamDef { name: "text", description: "Message text", required: true },
        ],
    },
    ToolDef {
        name: "send_photo",
        description: "Send a photo to a channel",
        params: &[
            ParamDef { name: "channel", description: "Channel type", required: true },
            ParamDef { name: "chat_id", description: "Chat/group ID", required: true },
            ParamDef { name: "url_or_path", description: "URL or file path of the photo", required: true },
        ],
    },
    ToolDef {
        name: "send_sticker",
        description: "Send a sticker (LINE only)",
        params: &[
            ParamDef { name: "chat_id", description: "Chat/group ID", required: true },
            ParamDef { name: "sticker_id", description: "LINE sticker ID", required: true },
        ],
    },
    ToolDef {
        name: "web_search",
        description: "Search the web",
        params: &[
            ParamDef { name: "query", description: "Search query", required: true },
        ],
    },
    ToolDef {
        name: "send_to_agent",
        description: "Delegate task to another agent",
        params: &[
            ParamDef { name: "agent_id", description: "Target agent ID", required: true },
            ParamDef { name: "prompt", description: "Prompt/task for the agent", required: true },
        ],
    },
    ToolDef {
        name: "log_mood",
        description: "Log user mood",
        params: &[
            ParamDef { name: "mood", description: "Mood label (e.g. happy, tired, neutral)", required: true },
            ParamDef { name: "note", description: "Optional note", required: false },
        ],
    },
    ToolDef {
        name: "schedule_task",
        description: "Schedule a task",
        params: &[
            ParamDef { name: "cron", description: "Cron expression", required: true },
            ParamDef { name: "description", description: "Task description", required: true },
        ],
    },
    ToolDef {
        name: "memory_search",
        description: "Search agent memory",
        params: &[
            ParamDef { name: "query", description: "Search query", required: true },
        ],
    },
    ToolDef {
        name: "memory_store",
        description: "Store a memory entry",
        params: &[
            ParamDef { name: "content", description: "Memory content", required: true },
            ParamDef { name: "tags", description: "Comma-separated tags", required: false },
        ],
    },
    // ── Sub-agent management tools ──────────────────────────────
    ToolDef {
        name: "create_agent",
        description: "Create a persistent sub-agent with its own identity, skills, and configuration. The agent is registered and available for delegation immediately.",
        params: &[
            ParamDef { name: "name", description: "Agent name (lowercase, no spaces, e.g. 'researcher')", required: true },
            ParamDef { name: "display_name", description: "Human-readable display name (e.g. 'Research Assistant')", required: true },
            ParamDef { name: "role", description: "Agent role: 'specialist' or 'worker' (default: specialist)", required: false },
            ParamDef { name: "reports_to", description: "Parent agent name this agent reports to (default: main agent)", required: false },
            ParamDef { name: "soul", description: "Personality/system prompt for this agent (written to SOUL.md)", required: false },
            ParamDef { name: "model", description: "Preferred model (default: claude-sonnet-4-6)", required: false },
            ParamDef { name: "trigger", description: "Trigger keyword (default: @display_name)", required: false },
            ParamDef { name: "icon", description: "Emoji icon for this agent (default: 🤖)", required: false },
        ],
    },
    ToolDef {
        name: "list_agents",
        description: "List all registered agents with their role, status, and reports_to hierarchy",
        params: &[],
    },
    ToolDef {
        name: "agent_status",
        description: "Get detailed status and configuration of a specific agent",
        params: &[
            ParamDef { name: "agent_id", description: "Agent name to inspect", required: true },
        ],
    },
    ToolDef {
        name: "spawn_agent",
        description: "Spawn a persistent sub-agent task. The agent runs in the background with its own session, executing the given prompt. Use agent_status to check progress.",
        params: &[
            ParamDef { name: "agent_id", description: "Target agent name", required: true },
            ParamDef { name: "task", description: "Task prompt for the agent to execute", required: true },
            ParamDef { name: "session_key", description: "Optional session key to resume a previous conversation context", required: false },
        ],
    },
    ToolDef {
        name: "agent_update",
        description: "Update one or more fields of an existing agent's configuration (agent.toml). Supports identity, model, budget, heartbeat, and container fields. Uses atomic write for safety.",
        params: &[
            ParamDef { name: "agent_id", description: "Agent name to update", required: true },
            ParamDef { name: "display_name", description: "New display name", required: false },
            ParamDef { name: "role", description: "New role: main, specialist, worker, developer, qa, planner", required: false },
            ParamDef { name: "status", description: "New status: active, paused, terminated", required: false },
            ParamDef { name: "trigger", description: "New trigger keyword", required: false },
            ParamDef { name: "icon", description: "New emoji icon", required: false },
            ParamDef { name: "reports_to", description: "New parent agent name", required: false },
            ParamDef { name: "model", description: "New preferred model", required: false },
            ParamDef { name: "fallback_model", description: "New fallback model", required: false },
            ParamDef { name: "api_mode", description: "API mode: cli, direct, auto", required: false },
            ParamDef { name: "budget_cents", description: "Monthly budget limit in cents", required: false },
            ParamDef { name: "max_concurrent", description: "Max concurrent container tasks", required: false },
            ParamDef { name: "heartbeat_enabled", description: "Enable/disable heartbeat (true/false)", required: false },
            ParamDef { name: "heartbeat_cron", description: "Heartbeat cron expression", required: false },
        ],
    },
    ToolDef {
        name: "agent_remove",
        description: "Remove an agent (moves to _trash/ for recovery). Refuses to remove the main agent.",
        params: &[
            ParamDef { name: "agent_id", description: "Agent name to remove", required: true },
        ],
    },
    ToolDef {
        name: "agent_update_soul",
        description: "Update an agent's SOUL.md personality file via the trusted MCP channel. Bypasses file-protect hooks. Uses atomic write with SHA-256 fingerprinting.",
        params: &[
            ParamDef { name: "agent_id", description: "Agent name", required: true },
            ParamDef { name: "content", description: "New SOUL.md content (full replacement)", required: true },
        ],
    },
    // ── Skill management tools ──────────────────────────────────
    ToolDef {
        name: "skill_search",
        description: "Search the local skill registry for available skills to install",
        params: &[
            ParamDef { name: "query", description: "Search query (name, tag, or description)", required: true },
        ],
    },
    ToolDef {
        name: "skill_list",
        description: "List all skills installed for a specific agent",
        params: &[
            ParamDef { name: "agent_id", description: "Agent name (default: main agent)", required: false },
        ],
    },
    // ── Feedback tool ────────────────────────────────────────────
    ToolDef {
        name: "submit_feedback",
        description: "Submit user feedback signal (positive/negative/correction) to influence agent evolution",
        params: &[
            ParamDef { name: "signal_type", description: "Feedback type: positive, negative, or correction", required: true },
            ParamDef { name: "detail", description: "What the feedback is about", required: true },
            ParamDef { name: "agent_id", description: "Target agent (default: main agent)", required: false },
        ],
    },
    // ── Evolution controls ──────────────────────────────────────
    ToolDef {
        name: "evolution_toggle",
        description: "Toggle evolution engine flags for an agent (gvu_enabled, cognitive_memory, etc.). Changes take effect within seconds.",
        params: &[
            ParamDef { name: "agent_id", description: "Target agent name", required: true },
            ParamDef { name: "field", description: "Config field to toggle: gvu_enabled, cognitive_memory, skill_auto_activate, skill_security_scan", required: true },
            ParamDef { name: "value", description: "New value: true/false (for booleans), or a number (for max_silence_hours, skill_token_budget, etc.)", required: true },
        ],
    },
    ToolDef {
        name: "evolution_status",
        description: "Get the current evolution engine configuration and status for an agent",
        params: &[
            ParamDef { name: "agent_id", description: "Target agent name (default: main agent)", required: false },
        ],
    },
    // ── Channel settings tools ────────────────────────────────────
    ToolDef {
        name: "channel_config",
        description: "Get or set channel settings (mention_only, auto_thread, allowed_channels, agent_override, response_mode). Omit 'value' to read current setting.",
        params: &[
            ParamDef { name: "channel", description: "Channel type: discord, telegram, slack, line", required: true },
            ParamDef { name: "scope_id", description: "Scope: guild_id, chat_id, or 'global'", required: true },
            ParamDef { name: "key", description: "Setting key: mention_only, auto_thread, allowed_channels, agent_override, response_mode", required: true },
            ParamDef { name: "value", description: "New value (omit to read current value)", required: false },
        ],
    },
    ToolDef {
        name: "channel_config_list",
        description: "List all channel settings for a scope",
        params: &[
            ParamDef { name: "channel", description: "Channel type: discord, telegram, slack, line", required: true },
            ParamDef { name: "scope_id", description: "Scope: guild_id, chat_id, or 'global'", required: true },
        ],
    },
    // ── Local inference tools ─────────────────────────────────────
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
        params: &[
            ParamDef { name: "model_id", description: "Model ID or filename (e.g., 'qwen3-8b-q4_k_m')", required: true },
        ],
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
            ParamDef { name: "prompt", description: "The user prompt to test routing for", required: true },
            ParamDef { name: "system_prompt", description: "Optional system prompt context", required: false },
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
        params: &[
            ParamDef { name: "file", description: "llamafile filename (optional, uses default)", required: false },
        ],
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
    // ── Compression tools ──────────────────────────────────────────
    ToolDef {
        name: "compress_text",
        description: "Compress text using Meta-Token (lossless) compression. Returns compressed text and compression ratio. Best for JSON, code, and repetitive templates.",
        params: &[
            ParamDef { name: "text", description: "Text to compress", required: true },
        ],
    },
    ToolDef {
        name: "decompress_text",
        description: "Decompress a Meta-Token compressed string back to the original text (lossless).",
        params: &[
            ParamDef { name: "text", description: "Compressed text to decompress", required: true },
        ],
    },
    // ── Model registry tools ────────────────────────────────────────
    ToolDef {
        name: "model_search",
        description: "Search for GGUF models from curated recommendations and HuggingFace. Results are filtered by available RAM. Trusted repos are marked [推薦].",
        params: &[
            ParamDef { name: "query", description: "Search query (e.g., 'qwen 8b', 'code llama', 'gemma')", required: true },
        ],
    },
    ToolDef {
        name: "model_download",
        description: "Download a GGUF model from HuggingFace to ~/.duduclaw/models/. Supports resume and mirror fallback.",
        params: &[
            ParamDef { name: "repo", description: "HuggingFace repo (e.g., 'Qwen/Qwen3-8B-GGUF')", required: true },
            ParamDef { name: "filename", description: "GGUF filename (e.g., 'qwen3-8b-q4_k_m.gguf')", required: true },
        ],
    },
    ToolDef {
        name: "model_recommend",
        description: "Get hardware-aware model recommendations based on detected GPU and available RAM.",
        params: &[],
    },
    // ── Odoo ERP tools ────────────────────────────────────────────
    ToolDef {
        name: "odoo_connect",
        description: "Connect to Odoo ERP and authenticate. Must be called before using other odoo_* tools.",
        params: &[],
    },
    ToolDef {
        name: "odoo_status",
        description: "Show Odoo connection status, version, edition (CE/EE), and installed modules",
        params: &[],
    },
    ToolDef {
        name: "odoo_crm_leads",
        description: "Search CRM leads/opportunities in Odoo",
        params: &[
            ParamDef { name: "stage", description: "Filter by stage name (optional)", required: false },
            ParamDef { name: "limit", description: "Max results (default 20)", required: false },
        ],
    },
    ToolDef {
        name: "odoo_crm_create_lead",
        description: "Create a new CRM lead in Odoo",
        params: &[
            ParamDef { name: "name", description: "Lead name / subject", required: true },
            ParamDef { name: "contact_name", description: "Contact person name", required: false },
            ParamDef { name: "email", description: "Contact email", required: false },
            ParamDef { name: "phone", description: "Contact phone", required: false },
            ParamDef { name: "expected_revenue", description: "Expected revenue", required: false },
        ],
    },
    ToolDef {
        name: "odoo_crm_update_stage",
        description: "Move a CRM lead to a different stage",
        params: &[
            ParamDef { name: "lead_id", description: "Lead ID", required: true },
            ParamDef { name: "stage_name", description: "Target stage name", required: true },
        ],
    },
    ToolDef {
        name: "odoo_sale_orders",
        description: "Search sale orders in Odoo",
        params: &[
            ParamDef { name: "status", description: "Filter by status (draft/sale/done)", required: false },
            ParamDef { name: "limit", description: "Max results (default 20)", required: false },
        ],
    },
    ToolDef {
        name: "odoo_sale_create_quotation",
        description: "Create a new quotation (draft sale order) in Odoo",
        params: &[
            ParamDef { name: "partner_id", description: "Customer partner ID", required: true },
            ParamDef { name: "product_id", description: "Product ID", required: true },
            ParamDef { name: "quantity", description: "Quantity (default 1)", required: false },
        ],
    },
    ToolDef {
        name: "odoo_sale_confirm",
        description: "Confirm a quotation into a sale order",
        params: &[
            ParamDef { name: "order_id", description: "Sale order ID to confirm", required: true },
        ],
    },
    ToolDef {
        name: "odoo_inventory_products",
        description: "Search products with stock levels in Odoo",
        params: &[
            ParamDef { name: "query", description: "Product name search", required: false },
            ParamDef { name: "limit", description: "Max results (default 20)", required: false },
        ],
    },
    ToolDef {
        name: "odoo_inventory_check",
        description: "Check real-time stock level for a specific product",
        params: &[
            ParamDef { name: "product_id", description: "Product ID", required: true },
        ],
    },
    ToolDef {
        name: "odoo_invoice_list",
        description: "List invoices from Odoo (draft/posted/paid)",
        params: &[
            ParamDef { name: "status", description: "Filter: draft/posted/paid", required: false },
            ParamDef { name: "limit", description: "Max results (default 20)", required: false },
        ],
    },
    ToolDef {
        name: "odoo_payment_status",
        description: "Check payment status for an invoice",
        params: &[
            ParamDef { name: "invoice_id", description: "Invoice ID", required: true },
        ],
    },
    ToolDef {
        name: "odoo_search",
        description: "Generic Odoo model search (advanced). Blocked models: ir.config_parameter, res.users, ir.cron, etc.",
        params: &[
            ParamDef { name: "model", description: "Odoo model name (e.g. res.partner)", required: true },
            ParamDef { name: "domain", description: "Search domain as JSON array", required: false },
            ParamDef { name: "fields", description: "Comma-separated field names", required: false },
            ParamDef { name: "limit", description: "Max results (default 20)", required: false },
        ],
    },
    ToolDef {
        name: "odoo_execute",
        description: "Call a method on an Odoo model (advanced). Example: action_confirm on sale.order.",
        params: &[
            ParamDef { name: "model", description: "Odoo model name", required: true },
            ParamDef { name: "method", description: "Method name to call", required: true },
            ParamDef { name: "ids", description: "Record IDs as JSON array", required: true },
        ],
    },
    ToolDef {
        name: "odoo_report",
        description: "Generate a PDF report from Odoo (e.g. invoice, quotation)",
        params: &[
            ParamDef { name: "report_name", description: "Report template name (e.g. account.report_invoice)", required: true },
            ParamDef { name: "record_id", description: "Record ID", required: true },
        ],
    },
    // ── Cost telemetry tools ─────────────────────────────────────
    ToolDef {
        name: "cost_summary",
        description: "Get token usage and cost summary (global or per-agent). Shows cache efficiency, total tokens, estimated cost. Use to monitor API spending and detect cache degradation.",
        params: &[
            ParamDef { name: "agent_id", description: "Agent ID to filter (optional, omit for global summary)", required: false },
            ParamDef { name: "hours", description: "Time window in hours (default 24)", required: false },
        ],
    },
    ToolDef {
        name: "cost_agents",
        description: "List all agents ranked by cost. Shows per-agent cache efficiency and health status (healthy/normal/degraded).",
        params: &[
            ParamDef { name: "hours", description: "Time window in hours (default 24)", required: false },
        ],
    },
    ToolDef {
        name: "cost_recent",
        description: "Show recent individual API call records with detailed token breakdown (input, cache_read, cache_write, output).",
        params: &[
            ParamDef { name: "limit", description: "Number of recent records (default 20)", required: false },
        ],
    },
    // ── Voice / ASR / TTS tools ──────────────────────────────────
    ToolDef {
        name: "transcribe_audio",
        description: "Transcribe audio to text using Whisper ASR. Accepts base64-encoded audio (OGG/MP3/WAV/M4A). Returns transcribed text. Default language: zh (Mandarin).",
        params: &[
            ParamDef { name: "audio_base64", description: "Base64-encoded audio data", required: true },
            ParamDef { name: "language", description: "Language hint (default: zh). BCP-47 code.", required: false },
        ],
    },
    ToolDef {
        name: "synthesize_speech",
        description: "Convert text to speech audio using TTS (edge-tts free or MiniMax paid). Returns base64-encoded MP3 audio.",
        params: &[
            ParamDef { name: "text", description: "Text to synthesize", required: true },
            ParamDef { name: "voice", description: "Voice name (default: auto-detect zh-TW/en-US)", required: false },
        ],
    },
];

// ── JSON-RPC helpers ─────────────────────────────────────────

fn jsonrpc_response(id: &Value, result: Value) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
}

/// Validate agent ID is safe for filesystem paths (no traversal).
fn is_valid_agent_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Maximum JSONL queue file size (10 MB).
const MAX_QUEUE_FILE_SIZE: u64 = 10 * 1024 * 1024;

/// Append a line to a JSONL file with size limit check.
///
/// **Concurrency note (MCP-M4)**: Uses `O_APPEND` which is atomic on POSIX
/// for writes ≤ PIPE_BUF (typically 4096 bytes). JSONL lines are typically
/// < 1KB so concurrent appends from MCP server and gateway are safe.
/// The dispatcher uses its own Mutex for read-modify-write operations.
fn append_to_jsonl_sync(path: &std::path::Path, line: &str) -> bool {
    // Check size limit
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.len() > MAX_QUEUE_FILE_SIZE {
            tracing::warn!("Queue file {} exceeds size limit", path.display());
            return false;
        }
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        writeln!(f, "{line}").is_ok()
    } else {
        false
    }
}

fn jsonrpc_error(id: &Value, code: i64, message: &str) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message
        }
    })
}

// ── Tool schema builder ──────────────────────────────────────

fn build_tool_schema(tool: &ToolDef) -> Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();

    for param in tool.params {
        properties.insert(
            param.name.to_string(),
            serde_json::json!({
                "type": "string",
                "description": param.description
            }),
        );
        if param.required {
            required.push(Value::String(param.name.to_string()));
        }
    }

    serde_json::json!({
        "name": tool.name,
        "description": tool.description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required
        }
    })
}

// ── Tool handlers ────────────────────────────────────────────

async fn handle_send_message(
    params: &Value,
    home_dir: &Path,
    http: &reqwest::Client,
) -> Value {
    let channel = params.get("channel").and_then(|v| v.as_str()).unwrap_or("");
    let chat_id = params.get("chat_id").and_then(|v| v.as_str()).unwrap_or("");
    let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");

    if channel.is_empty() || chat_id.is_empty() || text.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: channel, chat_id, and text are required"}],
            "isError": true
        });
    }

    let config = match read_config(home_dir).await {
        Some(c) => c,
        None => {
            return serde_json::json!({
                "content": [{"type": "text", "text": "Error: could not read config.toml"}],
                "isError": true
            });
        }
    };

    let result = match channel {
        "telegram" => {
            let token = decrypt_channel_token(&config, "telegram_bot_token_enc", "telegram_bot_token", home_dir);
            if token.is_empty() {
                "Error: telegram_bot_token not configured".to_string()
            } else {
                let url = format!(
                    "https://api.telegram.org/bot{}/sendMessage",
                    token
                );
                match http
                    .post(&url)
                    .json(&serde_json::json!({
                        "chat_id": chat_id,
                        "text": text
                    }))
                    .send()
                    .await
                {
                    Ok(resp) => if resp.status().is_success() { "Message sent successfully.".to_string() } else { format!("Error: API returned {}", resp.status()) },
                    Err(e) => format!("Error sending Telegram message: {e}"),
                }
            }
        }
        "line" => {
            let token = decrypt_channel_token(&config, "line_channel_token_enc", "line_channel_token", home_dir);
            if token.is_empty() {
                "Error: line_channel_token not configured".to_string()
            } else {
                let url = "https://api.line.me/v2/bot/message/push";
                match http
                    .post(url)
                    .header("Authorization", format!("Bearer {}", token))
                    .json(&serde_json::json!({
                        "to": chat_id,
                        "messages": [{"type": "text", "text": text}]
                    }))
                    .send()
                    .await
                {
                    Ok(resp) => if resp.status().is_success() { "Message sent successfully.".to_string() } else { format!("Error: API returned {}", resp.status()) },
                    Err(e) => format!("Error sending LINE message: {e}"),
                }
            }
        }
        "discord" => {
            let token = decrypt_channel_token(&config, "discord_bot_token_enc", "discord_bot_token", home_dir);
            if token.is_empty() {
                "Error: discord_bot_token not configured".to_string()
            } else {
                let url = format!(
                    "https://discord.com/api/v10/channels/{}/messages",
                    chat_id
                );
                match http
                    .post(&url)
                    .header("Authorization", format!("Bot {}", token))
                    .json(&serde_json::json!({
                        "content": text
                    }))
                    .send()
                    .await
                {
                    Ok(resp) => if resp.status().is_success() { "Message sent successfully.".to_string() } else { format!("Error: API returned {}", resp.status()) },
                    Err(e) => format!("Error sending Discord message: {e}"),
                }
            }
        }
        _ => format!("Unknown channel: {channel}"),
    };

    serde_json::json!({
        "content": [{"type": "text", "text": result}]
    })
}

async fn handle_web_search(params: &Value, http: &reqwest::Client) -> Value {
    let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
    if query.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: query is required"}],
            "isError": true
        });
    }

    let url = format!(
        "https://html.duckduckgo.com/html/?q={}",
        urlencoding::encode(query)
    );

    // Enforce 10s timeout so web_search doesn't block the MCP server (CLI-H5)
    let search_future = async {
        let resp = http
            .get(&url)
            .header("User-Agent", "DuDuClaw/0.6")
            .send()
            .await?;
        resp.text().await
    };

    let result = match tokio::time::timeout(std::time::Duration::from_secs(10), search_future).await {
        Ok(Ok(body)) => extract_search_results(&body),
        Ok(Err(e)) => format!("Error performing search: {e}"),
        Err(_) => "Error: web search timed out after 10 seconds".to_string(),
    };

    serde_json::json!({
        "content": [{"type": "text", "text": result}]
    })
}

/// Extract text results from DuckDuckGo HTML response using `scraper` (CLI-M5).
fn extract_search_results(html: &str) -> String {
    use scraper::{Html, Selector};

    let document = Html::parse_document(html);
    let mut results = Vec::new();

    // Try selectors in priority order
    let selectors = [
        ".result__snippet",
        ".result__a",
        ".links_main a",
    ];

    for sel_str in selectors {
        if let Ok(selector) = Selector::parse(sel_str) {
            for element in document.select(&selector) {
                let text: String = element.text().collect::<Vec<_>>().join(" ");
                let clean = text.trim().to_string();
                if !clean.is_empty() && clean.len() > 10 {
                    results.push(clean);
                }
                if results.len() >= 5 { break; }
            }
        }
        if !results.is_empty() { break; }
    }

    if results.is_empty() {
        "No results found.".to_string()
    } else {
        results
            .iter()
            .enumerate()
            .map(|(i, r)| format!("{}. {}", i + 1, r))
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

async fn handle_memory_search(
    params: &Value,
    memory: &SqliteMemoryEngine,
    agent_id: &str,
) -> Value {
    let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
    if query.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: query is required"}],
            "isError": true
        });
    }

    match memory.search(agent_id, query, 10).await {
        Ok(entries) => {
            if entries.is_empty() {
                serde_json::json!({
                    "content": [{"type": "text", "text": "No memories found."}]
                })
            } else {
                let text = entries
                    .iter()
                    .map(|e| format!("[{}] {}", e.timestamp.format("%Y-%m-%d %H:%M"), e.content))
                    .collect::<Vec<_>>()
                    .join("\n");
                serde_json::json!({
                    "content": [{"type": "text", "text": text}]
                })
            }
        }
        Err(e) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Error searching memory: {e}")}],
            "isError": true
        }),
    }
}

async fn handle_memory_store(
    params: &Value,
    memory: &SqliteMemoryEngine,
    agent_id: &str,
) -> Value {
    let content = params.get("content").and_then(|v| v.as_str()).unwrap_or("");
    if content.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: content is required"}],
            "isError": true
        });
    }

    let tags_str = params.get("tags").and_then(|v| v.as_str()).unwrap_or("");
    let tags: Vec<String> = if tags_str.is_empty() {
        Vec::new()
    } else {
        tags_str.split(',').map(|s| s.trim().to_string()).collect()
    };

    let classification = duduclaw_memory::classify(content, "user_input");
    let entry = MemoryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        agent_id: agent_id.to_string(),
        content: content.to_string(),
        timestamp: chrono::Utc::now(),
        tags,
        embedding: None,
        layer: classification.layer,
        importance: classification.importance,
        access_count: 0,
        last_accessed: None,
        source_event: "mcp_memory_store".to_string(),
    };

    match memory.store(agent_id, entry).await {
        Ok(()) => serde_json::json!({
            "content": [{"type": "text", "text": "Memory stored successfully."}]
        }),
        Err(e) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Error storing memory: {e}")}],
            "isError": true
        }),
    }
}

/// Send a message to another agent via the bus queue.
async fn handle_send_to_agent(params: &Value, home_dir: &Path) -> Value {
    let target = params.get("agent_id").or_else(|| params.get("agent")).and_then(|v| v.as_str()).unwrap_or("");
    let prompt = params.get("prompt").or_else(|| params.get("message")).and_then(|v| v.as_str()).unwrap_or("");

    if target.is_empty() || prompt.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: agent_id and prompt are required"}],
            "isError": true
        });
    }

    // Validate agent_id format
    if !is_valid_agent_id(target) {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: agent_id must be lowercase alphanumeric with hyphens"}],
            "isError": true
        });
    }

    let msg_id = uuid::Uuid::new_v4().to_string();
    let queue_path = home_dir.join("bus_queue.jsonl");
    let task = serde_json::json!({
        "type": "agent_message",
        "message_id": &msg_id,
        "agent_id": target,
        "payload": prompt,
        "timestamp": chrono::Utc::now().to_rfc3339(),
    });

    let queued = tokio::task::spawn_blocking({
        let path = queue_path.clone();
        let task_str = task.to_string();
        move || append_to_jsonl_sync(&path, &task_str)
    }).await.unwrap_or(false);

    serde_json::json!({
        "content": [{"type": "text", "text": if queued {
            format!("Message queued for agent '{target}' (id: {msg_id})")
        } else {
            format!("Failed to queue message for agent '{target}'")
        }}]
    })
}

/// Send a photo or sticker via a channel.
async fn handle_send_media(
    params: &Value,
    home_dir: &Path,
    http: &reqwest::Client,
    media_type: &str,
) -> Value {
    let channel = params.get("channel").and_then(|v| v.as_str()).unwrap_or("");
    let chat_id = params.get("chat_id").and_then(|v| v.as_str()).unwrap_or("");
    let url_or_id = params.get("url_or_path")
        .or_else(|| params.get("url"))
        .or_else(|| params.get("sticker_id"))
        .or_else(|| params.get("file_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if chat_id.is_empty() || url_or_id.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: chat_id and url/sticker_id are required for {media_type}")}],
            "isError": true
        });
    }

    let config = read_config(home_dir).await;
    let config_ref = config.as_ref();
    let result = match channel {
        "telegram" => {
            let token = config_ref
                .map(|c| decrypt_channel_token(c, "telegram_bot_token_enc", "telegram_bot_token", home_dir))
                .unwrap_or_default();
            if token.is_empty() {
                "Error: telegram_bot_token not configured".to_string()
            } else {
                let (method, key) = match media_type {
                    "photo" => ("sendPhoto", "photo"),
                    _ => ("sendSticker", "sticker"),
                };
                let api_url = format!("https://api.telegram.org/bot{token}/{method}");
                match http.post(&api_url)
                    .json(&serde_json::json!({ "chat_id": chat_id, key: url_or_id }))
                    .send().await
                {
                    Ok(r) => format!("{media_type} sent. Status: {}", r.status()),
                    Err(e) => format!("Error: {e}"),
                }
            }
        }
        "discord" => {
            let token = config_ref
                .map(|c| decrypt_channel_token(c, "discord_bot_token_enc", "discord_bot_token", home_dir))
                .unwrap_or_default();
            if token.is_empty() {
                "Error: discord_bot_token not configured".to_string()
            } else {
                let api_url = format!("https://discord.com/api/v10/channels/{chat_id}/messages");
                match http.post(&api_url)
                    .header("Authorization", format!("Bot {token}"))
                    .json(&serde_json::json!({ "content": url_or_id }))
                    .send().await
                {
                    Ok(r) => format!("{media_type} sent. Status: {}", r.status()),
                    Err(e) => format!("Error: {e}"),
                }
            }
        }
        _ => format!("Channel '{channel}' does not support {media_type} yet"),
    };

    serde_json::json!({ "content": [{"type": "text", "text": result}] })
}

/// Log a mood/emotion entry to agent memory.
async fn handle_log_mood(
    params: &Value,
    _home_dir: &Path,
    memory: &duduclaw_memory::SqliteMemoryEngine,
    default_agent: &str,
) -> Value {
    use duduclaw_core::traits::MemoryEngine;
    use duduclaw_core::types::MemoryEntry;

    let mood = params.get("mood").and_then(|v| v.as_str()).unwrap_or("neutral");
    let note = params.get("note").and_then(|v| v.as_str()).unwrap_or("");
    let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or(default_agent);

    let content = if note.is_empty() {
        format!("[mood] {mood}")
    } else {
        format!("[mood] {mood}: {note}")
    };

    let classification = duduclaw_memory::classify(&content, "agent_mood");
    let entry = MemoryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        agent_id: agent_id.to_string(),
        content,
        timestamp: chrono::Utc::now(),
        tags: vec!["mood".to_string(), mood.to_string()],
        embedding: None,
        layer: classification.layer,
        importance: classification.importance,
        access_count: 0,
        last_accessed: None,
        source_event: "agent_mood".to_string(),
    };

    match memory.store(agent_id, entry).await {
        Ok(()) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Mood '{mood}' logged for agent '{agent_id}'")}]
        }),
        Err(e) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Error logging mood: {e}")}],
            "isError": true
        }),
    }
}

/// Schedule a recurring or one-shot task.
async fn handle_schedule_task(params: &Value, home_dir: &Path) -> Value {
    let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("default");
    let cron = params.get("cron").and_then(|v| v.as_str()).unwrap_or("");
    let task = params.get("task").or_else(|| params.get("prompt")).or_else(|| params.get("description")).and_then(|v| v.as_str()).unwrap_or("");
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("unnamed");

    if cron.is_empty() || task.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: cron and task are required"}],
            "isError": true
        });
    }

    // Validate cron expression before persisting
    let normalised_cron = if cron.split_whitespace().count() == 5 {
        format!("0 {cron}")
    } else {
        cron.to_string()
    };
    if normalised_cron.parse::<cron::Schedule>().is_err() {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: invalid cron expression: {cron}")}],
            "isError": true
        });
    }

    let task_id = uuid::Uuid::new_v4().to_string();
    let cron_path = home_dir.join("cron_tasks.jsonl");
    let entry = serde_json::json!({
        "id": &task_id,
        "name": name,
        "agent_id": agent_id,
        "cron": cron,
        "task": task,
        "enabled": true,
        "created_at": chrono::Utc::now().to_rfc3339(),
    });

    let queued = tokio::task::spawn_blocking({
        let path = cron_path;
        let entry_str = entry.to_string();
        move || append_to_jsonl_sync(&path, &entry_str)
    }).await.unwrap_or(false);

    serde_json::json!({
        "content": [{"type": "text", "text": if queued {
            format!("Task '{name}' scheduled (id: {task_id}, cron: {cron})")
        } else {
            "Error: Failed to persist task (queue full or write error)".to_string()
        }}]
    })
}

// ── Sub-agent management handlers ───────────────────────────

/// Create a persistent sub-agent directory with agent.toml, SOUL.md, etc.
async fn handle_create_agent(params: &Value, home_dir: &Path) -> Value {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let display_name = params.get("display_name").and_then(|v| v.as_str()).unwrap_or("");

    if name.is_empty() || display_name.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: name and display_name are required"}],
            "isError": true
        });
    }

    // Validate name: safe for filesystem paths (no traversal)
    if !is_valid_agent_id(name) {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: name must be lowercase alphanumeric with hyphens, max 64 chars"}],
            "isError": true
        });
    }

    let agent_dir = home_dir.join("agents").join(name);
    if agent_dir.exists() {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: agent '{name}' already exists at {}", agent_dir.display())}],
            "isError": true
        });
    }

    let role = params.get("role").and_then(|v| v.as_str()).unwrap_or("specialist");
    let reports_to = params.get("reports_to").and_then(|v| v.as_str()).unwrap_or("");
    let soul = params.get("soul").and_then(|v| v.as_str()).unwrap_or("");
    let model = params.get("model").and_then(|v| v.as_str()).unwrap_or("claude-sonnet-4-6");
    let trigger = params.get("trigger").and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("@{display_name}"));
    let icon = params.get("icon").and_then(|v| v.as_str()).unwrap_or("\u{1F916}");

    // Resolve reports_to: default to the main agent if not specified
    let reports_to = if reports_to.is_empty() {
        resolve_main_agent_name(home_dir).await
    } else {
        reports_to.to_string()
    };

    // Create directory structure
    if let Err(e) = tokio::fs::create_dir_all(&agent_dir).await {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error creating agent directory: {e}")}],
            "isError": true
        });
    }
    let _ = tokio::fs::create_dir_all(agent_dir.join("SKILLS")).await;

    // Write agent.toml — use toml crate to prevent injection via display_name/trigger/icon
    // Clone values that will be consumed by the toml! macro
    let reports_to_display = reports_to.clone();
    let agent_config = toml::toml! {
        [agent]
        name = name
        display_name = display_name
        role = role
        status = "active"
        trigger = trigger
        reports_to = reports_to
        icon = icon

        [model]
        preferred = model
        fallback = "claude-haiku-4-5"
        account_pool = ["main"]

        [container]
        timeout_ms = 1800000
        max_concurrent = 2
        readonly_project = true
        additional_mounts = []

        [heartbeat]
        enabled = false
        interval_seconds = 3600
        max_concurrent_runs = 1
        cron = ""

        [budget]
        monthly_limit_cents = 2000
        warn_threshold_percent = 80
        hard_stop = true

        [permissions]
        can_create_agents = false
        can_send_cross_agent = true
        can_modify_own_skills = true
        can_modify_own_soul = false
        can_schedule_tasks = false
        allowed_channels = []

        [evolution]
        skill_auto_activate = false
        skill_security_scan = true
        gvu_enabled = false
        cognitive_memory = false
        max_silence_hours = 12.0
        max_gvu_generations = 3
        observation_period_hours = 24.0
    };
    let agent_toml = toml::to_string_pretty(&agent_config).unwrap_or_default();

    if let Err(e) = tokio::fs::write(agent_dir.join("agent.toml"), &agent_toml).await {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error writing agent.toml: {e}")}],
            "isError": true
        });
    }

    // Write SOUL.md if provided
    if !soul.is_empty() {
        let _ = tokio::fs::write(agent_dir.join("SOUL.md"), soul).await;
    }

    // Write empty MEMORY.md
    let _ = tokio::fs::write(agent_dir.join("MEMORY.md"), "").await;

    serde_json::json!({
        "content": [{"type": "text", "text": format!(
            "Agent '{display_name}' ({name}) created successfully.\n\
             Role: {role}\n\
             Reports to: {reports_to_display}\n\
             Model: {model}\n\
             Directory: {}\n\n\
             The agent is now available for delegation via send_to_agent or spawn_agent.",
            agent_dir.display()
        )}]
    })
}

/// List all registered agents with role, status, and hierarchy.
async fn handle_list_agents(home_dir: &Path) -> Value {
    let agents_dir = home_dir.join("agents");
    let mut entries = match tokio::fs::read_dir(&agents_dir).await {
        Ok(e) => e,
        Err(e) => {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error reading agents directory: {e}")}],
                "isError": true
            });
        }
    };

    let mut agents = Vec::new();

    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let dir_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        if dir_name.starts_with('_') {
            continue;
        }

        let toml_path = path.join("agent.toml");
        if let Ok(content) = tokio::fs::read_to_string(&toml_path).await {
            if let Ok(config) = toml::from_str::<duduclaw_core::types::AgentConfig>(&content) {
                agents.push(serde_json::json!({
                    "name": config.agent.name,
                    "display_name": config.agent.display_name,
                    "role": format!("{:?}", config.agent.role).to_lowercase(),
                    "status": format!("{:?}", config.agent.status).to_lowercase(),
                    "reports_to": config.agent.reports_to,
                    "icon": config.agent.icon,
                    "model": config.model.preferred,
                    "can_create_agents": config.permissions.can_create_agents,
                    "can_schedule_tasks": config.permissions.can_schedule_tasks,
                }));
            }
        }
    }

    if agents.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "No agents found."}]
        });
    }

    // Build a readable text table
    let mut lines = vec![format!("Found {} agent(s):\n", agents.len())];
    for a in &agents {
        let name = a["name"].as_str().unwrap_or("?");
        let display = a["display_name"].as_str().unwrap_or("?");
        let role = a["role"].as_str().unwrap_or("?");
        let status = a["status"].as_str().unwrap_or("?");
        let reports_to = a["reports_to"].as_str().unwrap_or("");
        let icon = a["icon"].as_str().unwrap_or("");
        let hierarchy = if reports_to.is_empty() {
            "(root)".to_string()
        } else {
            format!("-> {reports_to}")
        };
        lines.push(format!("{icon} {display} ({name}) [{role}/{status}] {hierarchy}"));
    }

    serde_json::json!({
        "content": [{"type": "text", "text": lines.join("\n")}]
    })
}

/// Get detailed status of a specific agent.
async fn handle_agent_status(params: &Value, home_dir: &Path) -> Value {
    let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
    if agent_id.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: agent_id is required"}],
            "isError": true
        });
    }
    if !is_valid_agent_id(agent_id) {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: agent_id must be lowercase alphanumeric with hyphens"}],
            "isError": true
        });
    }

    let agent_dir = home_dir.join("agents").join(agent_id);
    let toml_path = agent_dir.join("agent.toml");

    let content = match tokio::fs::read_to_string(&toml_path).await {
        Ok(c) => c,
        Err(_) => {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error: agent '{agent_id}' not found")}],
                "isError": true
            });
        }
    };

    let config: duduclaw_core::types::AgentConfig = match toml::from_str(&content) {
        Ok(c) => c,
        Err(e) => {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error parsing agent.toml: {e}")}],
                "isError": true
            });
        }
    };

    // Check for SOUL.md, skills, memory
    let has_soul = agent_dir.join("SOUL.md").exists();
    let has_identity = agent_dir.join("IDENTITY.md").exists();
    let skill_count = match tokio::fs::read_dir(agent_dir.join("SKILLS")).await {
        Ok(mut entries) => {
            let mut count = 0u32;
            while let Ok(Some(_)) = entries.next_entry().await {
                count += 1;
            }
            count
        }
        Err(_) => 0,
    };

    // Check pending bus_queue messages for this agent
    let pending_tasks = count_pending_tasks(home_dir, agent_id).await;

    let info = format!(
        "Agent: {} ({})\n\
         Role: {:?} | Status: {:?}\n\
         Reports to: {}\n\
         Model: {} (fallback: {})\n\
         Icon: {}\n\
         Trigger: {}\n\
         \n\
         Files:\n\
         - SOUL.md: {}\n\
         - IDENTITY.md: {}\n\
         - Skills: {} file(s)\n\
         - Directory: {}\n\
         \n\
         Permissions:\n\
         - Create agents: {}\n\
         - Cross-agent messaging: {}\n\
         - Schedule tasks: {}\n\
         - Modify own skills: {}\n\
         - Allowed channels: {:?}\n\
         \n\
         Budget: {} cents/month (warn: {}%, hard stop: {})\n\
         Heartbeat: {} (interval: {}s)\n\
         Pending tasks in queue: {}",
        config.agent.display_name,
        config.agent.name,
        config.agent.role,
        config.agent.status,
        if config.agent.reports_to.is_empty() { "(root)" } else { &config.agent.reports_to },
        config.model.preferred,
        config.model.fallback,
        config.agent.icon,
        config.agent.trigger,
        if has_soul { "yes" } else { "no" },
        if has_identity { "yes" } else { "no" },
        skill_count,
        agent_dir.display(),
        config.permissions.can_create_agents,
        config.permissions.can_send_cross_agent,
        config.permissions.can_schedule_tasks,
        config.permissions.can_modify_own_skills,
        config.permissions.allowed_channels,
        config.budget.monthly_limit_cents,
        config.budget.warn_threshold_percent,
        config.budget.hard_stop,
        if config.heartbeat.enabled { "enabled" } else { "disabled" },
        config.heartbeat.interval_seconds,
        pending_tasks,
    );

    serde_json::json!({
        "content": [{"type": "text", "text": info}]
    })
}

/// Spawn a persistent sub-agent task in the background.
async fn handle_spawn_agent(params: &Value, home_dir: &Path) -> Value {
    let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
    let task = params.get("task").and_then(|v| v.as_str()).unwrap_or("");
    let session_key = params.get("session_key").and_then(|v| v.as_str()).unwrap_or("");

    if agent_id.is_empty() || task.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: agent_id and task are required"}],
            "isError": true
        });
    }
    if !is_valid_agent_id(agent_id) {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: agent_id must be lowercase alphanumeric with hyphens"}],
            "isError": true
        });
    }

    // Verify agent exists
    let agent_dir = home_dir.join("agents").join(agent_id);
    if !agent_dir.join("agent.toml").exists() {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: agent '{agent_id}' not found")}],
            "isError": true
        });
    }

    let task_id = uuid::Uuid::new_v4().to_string();

    // Write a structured task entry to bus_queue.jsonl with spawn metadata
    let queue_path = home_dir.join("bus_queue.jsonl");
    let entry = serde_json::json!({
        "type": "agent_message",
        "message_id": &task_id,
        "agent_id": agent_id,
        "payload": task,
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "session_key": if session_key.is_empty() { &task_id } else { session_key },
        "persistent": true,
    });

    let queued = tokio::task::spawn_blocking({
        let path = queue_path;
        let entry_str = entry.to_string();
        move || -> bool {
            use std::io::Write;
            // Enforce bus_queue.jsonl size limit (CLI-H4)
            const MAX_QUEUE_SIZE: u64 = 10 * 1024 * 1024; // 10 MB
            if let Ok(meta) = std::fs::metadata(&path) {
                if meta.len() > MAX_QUEUE_SIZE {
                    return false;
                }
            }
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                writeln!(f, "{entry_str}").is_ok()
            } else {
                false
            }
        }
    })
    .await
    .unwrap_or(false);

    if queued {
        serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "Sub-agent '{agent_id}' task spawned successfully.\n\
                 Task ID: {task_id}\n\
                 Session key: {}\n\
                 \n\
                 The task is queued and will be picked up by the dispatcher.\n\
                 Use agent_status to check progress, or check bus_queue.jsonl for the response.",
                if session_key.is_empty() { &task_id } else { session_key }
            )}]
        })
    } else {
        serde_json::json!({
            "content": [{"type": "text", "text": "Error: Failed to queue agent task"}],
            "isError": true
        })
    }
}

/// Update one or more fields in an existing agent's agent.toml.
///
/// Reads the current config, applies the requested changes, and writes back.
/// Uses `toml::to_string_pretty` for consistent formatting.
async fn handle_agent_update(params: &Value, home_dir: &Path) -> Value {
    let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
    if agent_id.is_empty() || !is_valid_agent_id(agent_id) {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: valid agent_id is required (lowercase alphanumeric with hyphens, max 64 chars)"}],
            "isError": true
        });
    }

    let agent_dir = home_dir.join("agents").join(agent_id);
    let toml_path = agent_dir.join("agent.toml");

    let content = match tokio::fs::read_to_string(&toml_path).await {
        Ok(c) => c,
        Err(_) => return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: agent '{agent_id}' not found")}],
            "isError": true
        }),
    };

    let mut config: duduclaw_core::types::AgentConfig = match toml::from_str(&content) {
        Ok(c) => c,
        Err(e) => return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error parsing agent.toml: {e}")}],
            "isError": true
        }),
    };

    let mut changes = Vec::new();

    // -- Agent identity fields --
    if let Some(v) = params.get("display_name").and_then(|v| v.as_str()) {
        config.agent.display_name = v.to_string();
        changes.push(format!("display_name = \"{v}\""));
    }
    if let Some(v) = params.get("role").and_then(|v| v.as_str()) {
        let role = match v.to_lowercase().as_str() {
            "main" => duduclaw_core::types::AgentRole::Main,
            "specialist" => duduclaw_core::types::AgentRole::Specialist,
            "worker" => duduclaw_core::types::AgentRole::Worker,
            "developer" => duduclaw_core::types::AgentRole::Developer,
            "qa" => duduclaw_core::types::AgentRole::Qa,
            "planner" => duduclaw_core::types::AgentRole::Planner,
            _ => return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error: invalid role '{v}'. Valid: main, specialist, worker, developer, qa, planner")}],
                "isError": true
            }),
        };
        config.agent.role = role;
        changes.push(format!("role = \"{v}\""));
    }
    if let Some(v) = params.get("status").and_then(|v| v.as_str()) {
        let status = match v.to_lowercase().as_str() {
            "active" => duduclaw_core::types::AgentStatus::Active,
            "paused" => duduclaw_core::types::AgentStatus::Paused,
            "terminated" => duduclaw_core::types::AgentStatus::Terminated,
            _ => return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error: invalid status '{v}'. Valid: active, paused, terminated")}],
                "isError": true
            }),
        };
        config.agent.status = status;
        changes.push(format!("status = \"{v}\""));
    }
    if let Some(v) = params.get("trigger").and_then(|v| v.as_str()) {
        config.agent.trigger = v.to_string();
        changes.push(format!("trigger = \"{v}\""));
    }
    if let Some(v) = params.get("icon").and_then(|v| v.as_str()) {
        config.agent.icon = v.to_string();
        changes.push(format!("icon = \"{v}\""));
    }
    if let Some(v) = params.get("reports_to").and_then(|v| v.as_str()) {
        config.agent.reports_to = v.to_string();
        changes.push(format!("reports_to = \"{v}\""));
    }

    // -- Model fields --
    if let Some(v) = params.get("model").and_then(|v| v.as_str()) {
        config.model.preferred = v.to_string();
        changes.push(format!("model.preferred = \"{v}\""));
    }
    if let Some(v) = params.get("fallback_model").and_then(|v| v.as_str()) {
        config.model.fallback = v.to_string();
        changes.push(format!("model.fallback = \"{v}\""));
    }
    if let Some(v) = params.get("api_mode").and_then(|v| v.as_str()) {
        match v {
            "cli" | "direct" | "auto" => {
                config.model.api_mode = v.to_string();
                changes.push(format!("model.api_mode = \"{v}\""));
            }
            _ => return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error: invalid api_mode '{v}'. Valid: cli, direct, auto")}],
                "isError": true
            }),
        }
    }

    // -- Budget fields --
    if let Some(v) = params.get("budget_cents").and_then(|v| v.as_u64()) {
        config.budget.monthly_limit_cents = v;
        changes.push(format!("budget.monthly_limit_cents = {v}"));
    }

    // -- Container fields --
    if let Some(v) = params.get("max_concurrent").and_then(|v| v.as_u64()) {
        config.container.max_concurrent = v as u32;
        changes.push(format!("container.max_concurrent = {v}"));
    }

    // -- Heartbeat fields --
    if let Some(v) = params.get("heartbeat_enabled") {
        if let Some(b) = v.as_bool() {
            config.heartbeat.enabled = b;
            changes.push(format!("heartbeat.enabled = {b}"));
        }
    }
    if let Some(v) = params.get("heartbeat_cron").and_then(|v| v.as_str()) {
        config.heartbeat.cron = v.to_string();
        changes.push(format!("heartbeat.cron = \"{v}\""));
    }

    if changes.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: no valid fields to update. Supported fields: display_name, role, status, trigger, icon, reports_to, model, fallback_model, api_mode, budget_cents, max_concurrent, heartbeat_enabled, heartbeat_cron"}],
            "isError": true
        });
    }

    // Serialize and write atomically (temp + rename)
    let updated_toml = match toml::to_string_pretty(&config) {
        Ok(s) => s,
        Err(e) => return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error serializing agent.toml: {e}")}],
            "isError": true
        }),
    };

    let tmp_path = toml_path.with_extension("toml.tmp");
    if let Err(e) = tokio::fs::write(&tmp_path, &updated_toml).await {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error writing agent.toml: {e}")}],
            "isError": true
        });
    }
    if let Err(e) = tokio::fs::rename(&tmp_path, &toml_path).await {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error committing agent.toml: {e}")}],
            "isError": true
        });
    }

    serde_json::json!({
        "content": [{"type": "text", "text": format!(
            "Agent '{agent_id}' updated successfully.\n\nChanges:\n{}",
            changes.iter().map(|c| format!("  • {c}")).collect::<Vec<_>>().join("\n")
        )}]
    })
}

/// Remove an agent directory after safety checks.
///
/// Refuses to remove the main agent. Moves to `_trash/{name}_{timestamp}` instead
/// of hard-deleting, so recovery is possible.
async fn handle_agent_remove(params: &Value, home_dir: &Path) -> Value {
    let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
    if agent_id.is_empty() || !is_valid_agent_id(agent_id) {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: valid agent_id is required"}],
            "isError": true
        });
    }

    let agent_dir = home_dir.join("agents").join(agent_id);
    let toml_path = agent_dir.join("agent.toml");

    // Verify agent exists
    let content = match tokio::fs::read_to_string(&toml_path).await {
        Ok(c) => c,
        Err(_) => return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: agent '{agent_id}' not found")}],
            "isError": true
        }),
    };

    // Refuse to remove main agent
    if let Ok(config) = toml::from_str::<duduclaw_core::types::AgentConfig>(&content) {
        if config.agent.role == duduclaw_core::types::AgentRole::Main {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error: cannot remove main agent '{agent_id}'. Change its role first if you really mean to.")}],
                "isError": true
            });
        }
    }

    // Move to trash instead of hard delete
    let trash_dir = home_dir.join("agents").join("_trash");
    let _ = tokio::fs::create_dir_all(&trash_dir).await;
    let timestamp = chrono::Utc::now().format("%Y%m%d%H%M%S");
    let trash_name = format!("{agent_id}_{timestamp}");
    let trash_path = trash_dir.join(&trash_name);

    if let Err(e) = tokio::fs::rename(&agent_dir, &trash_path).await {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error moving agent to trash: {e}")}],
            "isError": true
        });
    }

    serde_json::json!({
        "content": [{"type": "text", "text": format!(
            "Agent '{agent_id}' removed (moved to trash).\n\
             Recovery path: {}\n\n\
             To permanently delete: rm -rf {}",
            trash_path.display(),
            trash_path.display()
        )}]
    })
}

/// Update SOUL.md for an agent via the trusted MCP channel.
///
/// This bypasses file-protect.sh (which blocks Write/Edit on SOUL.md)
/// because MCP tools are a trusted code path in the DuDuClaw architecture.
async fn handle_agent_update_soul(params: &Value, home_dir: &Path) -> Value {
    let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
    if agent_id.is_empty() || !is_valid_agent_id(agent_id) {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: valid agent_id is required"}],
            "isError": true
        });
    }

    let soul_content = params.get("content").and_then(|v| v.as_str()).unwrap_or("");
    if soul_content.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: content is required (the new SOUL.md text)"}],
            "isError": true
        });
    }

    let agent_dir = home_dir.join("agents").join(agent_id);

    // Verify agent exists
    if !agent_dir.join("agent.toml").exists() {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: agent '{agent_id}' not found")}],
            "isError": true
        });
    }

    let soul_path = agent_dir.join("SOUL.md");

    // Read old content for SHA-256 fingerprint comparison
    let old_content = tokio::fs::read_to_string(&soul_path).await.unwrap_or_default();
    let old_hash = {
        let digest = <sha2::Sha256 as sha2::Digest>::digest(old_content.as_bytes());
        format!("{:x}", digest)
    };

    // Atomic write: temp file + rename
    let tmp_path = soul_path.with_extension("md.tmp");
    if let Err(e) = tokio::fs::write(&tmp_path, soul_content).await {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error writing SOUL.md: {e}")}],
            "isError": true
        });
    }
    if let Err(e) = tokio::fs::rename(&tmp_path, &soul_path).await {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error committing SOUL.md: {e}")}],
            "isError": true
        });
    }

    let new_hash = {
        let digest = <sha2::Sha256 as sha2::Digest>::digest(soul_content.as_bytes());
        format!("{:x}", digest)
    };

    serde_json::json!({
        "content": [{"type": "text", "text": format!(
            "SOUL.md updated for agent '{agent_id}'.\n\
             Old SHA-256: {old_hash}\n\
             New SHA-256: {new_hash}\n\
             Size: {} bytes",
            soul_content.len()
        )}]
    })
}

/// Count pending agent_message entries in bus_queue.jsonl for a given agent.
///
/// Reads line-by-line with a size cap to avoid OOM on large queues (CLI-M2).
async fn count_pending_tasks(home_dir: &Path, agent_id: &str) -> usize {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let queue_path = home_dir.join("bus_queue.jsonl");
    let file = match tokio::fs::File::open(&queue_path).await {
        Ok(f) => f,
        Err(_) => return 0,
    };

    let reader = BufReader::new(file);
    let mut lines = reader.lines();
    let mut count = 0usize;

    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() { continue; }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
            if v.get("type").and_then(|t| t.as_str()) == Some("agent_message")
                && v.get("agent_id").and_then(|a| a.as_str()) == Some(agent_id)
            {
                count += 1;
            }
        }
    }

    count
}

// ── Feedback handler ────────────────────────────────────────

async fn handle_submit_feedback(params: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let signal_type = params.get("signal_type").and_then(|v| v.as_str()).unwrap_or("");
    let detail = params.get("detail").and_then(|v| v.as_str()).unwrap_or("");
    let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or(default_agent);

    if signal_type.is_empty() || detail.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: signal_type and detail are required"}],
            "isError": true
        });
    }

    if !["positive", "negative", "correction"].contains(&signal_type) {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: signal_type must be positive, negative, or correction"}],
            "isError": true
        });
    }

    match duduclaw_gateway::external_factors::submit_feedback(
        home_dir, agent_id, signal_type, "mcp", detail,
    ).await {
        Ok(()) => serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "Feedback recorded: [{signal_type}] for agent '{agent_id}'. This will be included in the next evolution reflection."
            )}]
        }),
        Err(e) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Error submitting feedback: {e}")}],
            "isError": true
        }),
    }
}

// ── Evolution control handlers ──────────────────────────────

async fn handle_evolution_toggle(params: &Value, home_dir: &Path) -> Value {
    let agent_id = match params.get("agent_id").and_then(|v| v.as_str()) {
        Some(id) if !id.is_empty() => id,
        _ => return serde_json::json!({
            "content": [{"type": "text", "text": "Error: agent_id is required"}],
            "isError": true
        }),
    };
    let field = match params.get("field").and_then(|v| v.as_str()) {
        Some(f) if !f.is_empty() => f,
        _ => return serde_json::json!({
            "content": [{"type": "text", "text": "Error: field is required"}],
            "isError": true
        }),
    };
    let value_str = match params.get("value").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => match params.get("value") {
            Some(v) => &v.to_string(),
            None => return serde_json::json!({
                "content": [{"type": "text", "text": "Error: value is required"}],
                "isError": true
            }),
        },
    };

    // Read current agent.toml
    let agent_dir = home_dir.join("agents").join(agent_id);
    let toml_path = agent_dir.join("agent.toml");
    let content = match tokio::fs::read_to_string(&toml_path).await {
        Ok(c) => c,
        Err(e) => return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: agent '{agent_id}' not found: {e}")}],
            "isError": true
        }),
    };

    let mut doc: toml::Table = match content.parse() {
        Ok(t) => t,
        Err(e) => return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error parsing agent.toml: {e}")}],
            "isError": true
        }),
    };

    // Ensure [evolution] section exists
    if !doc.contains_key("evolution") {
        doc.insert("evolution".to_string(), toml::Value::Table(toml::Table::new()));
    }
    let evo = doc.get_mut("evolution").unwrap().as_table_mut().unwrap();

    // Validate field name
    let boolean_fields = [
        "gvu_enabled", "cognitive_memory",
        "skill_auto_activate", "skill_security_scan",
    ];
    let numeric_fields = [
        "max_silence_hours", "max_gvu_generations", "observation_period_hours",
        "skill_token_budget", "max_active_skills",
    ];

    if boolean_fields.contains(&field) {
        let bool_val = match value_str {
            "true" | "1" | "yes" | "on" => true,
            "false" | "0" | "no" | "off" => false,
            _ => return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error: invalid boolean value '{value_str}' — use true/false")}],
                "isError": true
            }),
        };
        evo.insert(field.to_string(), toml::Value::Boolean(bool_val));
    } else if numeric_fields.contains(&field) {
        if let Ok(int_val) = value_str.parse::<i64>() {
            evo.insert(field.to_string(), toml::Value::Integer(int_val));
        } else if let Ok(float_val) = value_str.parse::<f64>() {
            evo.insert(field.to_string(), toml::Value::Float(float_val));
        } else {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error: invalid numeric value '{value_str}'")}],
                "isError": true
            });
        }
    } else {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "Error: unknown field '{field}'. Valid fields: {}",
                boolean_fields.iter().chain(numeric_fields.iter()).cloned().collect::<Vec<_>>().join(", ")
            )}],
            "isError": true
        });
    }

    // Write back
    let new_content = toml::to_string_pretty(&doc).unwrap_or_default();
    if let Err(e) = tokio::fs::write(&toml_path, &new_content).await {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error writing agent.toml: {e}")}],
            "isError": true
        });
    }

    serde_json::json!({
        "content": [{"type": "text", "text": format!(
            "Evolution config updated: {agent_id}.evolution.{field} = {value_str}\n\
             Changes take effect within 5 minutes (next heartbeat sync) or immediately on restart."
        )}]
    })
}

async fn handle_evolution_status_tool(params: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let agent_id = params.get("agent_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(default_agent);

    let toml_path = home_dir.join("agents").join(agent_id).join("agent.toml");
    let content = match tokio::fs::read_to_string(&toml_path).await {
        Ok(c) => c,
        Err(e) => return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: agent '{agent_id}' not found: {e}")}],
            "isError": true
        }),
    };

    let config: duduclaw_core::types::AgentConfig = match toml::from_str(&content) {
        Ok(c) => c,
        Err(e) => return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error parsing agent.toml: {e}")}],
            "isError": true
        }),
    };

    let evo = &config.evolution;
    let status = format!(
        "Evolution status for agent '{agent_id}':\n\
         \n\
         GVU self-play:     {}\n\
         Cognitive memory:  {}\n\
         \n\
         Skill auto-activate:  {}\n\
         Skill security scan:  {}\n\
         Skill token budget:   {}\n\
         Max active skills:    {}\n\
         \n\
         Max silence hours:         {:.1}\n\
         Max GVU generations:       {}\n\
         Observation period hours:  {:.1}",
        evo.gvu_enabled, evo.cognitive_memory,
        evo.skill_auto_activate, evo.skill_security_scan,
        evo.skill_token_budget, evo.max_active_skills,
        evo.max_silence_hours, evo.max_gvu_generations, evo.observation_period_hours,
    );

    serde_json::json!({
        "content": [{"type": "text", "text": status}]
    })
}

// ── Local inference handlers ────────────────────────────────

async fn handle_inference_status(home_dir: &Path) -> Value {
    let engine = duduclaw_inference::InferenceEngine::new(home_dir).await;
    let available = engine.is_available().await;
    let hw = engine.hardware_info().await;
    let models = engine.list_models().await;
    let loaded_count = models.iter().filter(|m| m.is_loaded).count();

    let mut status = format!(
        "Inference Engine Status:\n  Enabled: {}\n  Available: {}\n  Models: {} available, {} loaded",
        engine.config().enabled, available, models.len(), loaded_count
    );

    if let Some(ref hw) = hw {
        status.push_str(&format!(
            "\n  GPU: {} ({})\n  RAM: {}MB / {}MB\n  Recommended backend: {}",
            hw.gpu_name,
            format!("{:?}", hw.gpu_type),
            hw.ram_available_mb,
            hw.ram_total_mb,
            hw.recommended_backend
        ));
    }

    serde_json::json!({
        "content": [{"type": "text", "text": status}]
    })
}

async fn handle_model_list(home_dir: &Path) -> Value {
    let engine = duduclaw_inference::InferenceEngine::new(home_dir).await;
    let models = engine.list_models().await;

    if models.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "No models found in ~/.duduclaw/models/\n\nTo get started:\n1. Download a GGUF model (e.g., from huggingface.co)\n2. Place it in ~/.duduclaw/models/\n3. Run model_list again"}]
        });
    }

    // Check StreamingLLM config for KV cache compression
    let config = engine.config();
    let streaming_llm = config.streaming_llm.as_ref().filter(|s| s.enabled);
    let effective_ctx: Option<u32> = streaming_llm.map(|s| {
        (s.sink_size + s.window_size).try_into().unwrap_or(u32::MAX)
    });

    let mut text = format!("Available models ({}):\n", models.len());

    if let Some(slm) = streaming_llm {
        text.push_str(&format!(
            "\nStreamingLLM: ON (sink={}, window={}, effective ctx={})\n",
            slm.sink_size, slm.window_size, slm.sink_size + slm.window_size
        ));
    }

    text.push_str("\n  (KV cache estimates are approximate lower bounds for typical GQA models)");

    for m in &models {
        let loaded = if m.is_loaded { " [LOADED]" } else { "" };
        let size_mb = m.file_size_bytes / (1024 * 1024);
        let total_mb = m.estimated_memory_mb + m.kv_cache_mb;

        let kv_info = if m.kv_cache_mb == 0 {
            format!("total ~{}MB", m.estimated_memory_mb)
        } else if let Some(eff_ctx) = effective_ctx {
            let compressed_kv = duduclaw_inference::ModelInfo::estimate_kv_cache_mb(
                &m.parameter_count, eff_ctx,
            );
            let compressed_total = m.estimated_memory_mb + compressed_kv;
            format!(
                "KV ~{}→~{}MB, total ~{}→~{}MB",
                m.kv_cache_mb, compressed_kv, total_mb, compressed_total
            )
        } else {
            format!("KV ~{}MB, total ~{}MB", m.kv_cache_mb, total_mb)
        };

        text.push_str(&format!(
            "\n  {} ({} {} {}) — {}MB weights, {}{loaded}",
            m.id, m.architecture, m.parameter_count, m.quantization,
            size_mb, kv_info,
        ));
    }

    serde_json::json!({
        "content": [{"type": "text", "text": text}]
    })
}

async fn handle_model_load(params: &Value, home_dir: &Path) -> Value {
    let model_id = params.get("model_id").and_then(|v| v.as_str()).unwrap_or("");
    if model_id.is_empty() {
        return serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": "model_id is required"}]
        });
    }

    let engine = duduclaw_inference::InferenceEngine::new(home_dir).await;
    if let Err(e) = engine.init().await {
        return serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": format!("Failed to init inference engine: {e}")}]
        });
    }

    match engine.load_model(model_id).await {
        Ok(info) => serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "Model loaded: {} ({} {} {})\nEstimated memory: {}MB\nContext length: {}",
                info.id, info.architecture, info.parameter_count, info.quantization,
                info.estimated_memory_mb, info.context_length
            )}]
        }),
        Err(e) => serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": format!("Failed to load model: {e}")}]
        }),
    }
}

async fn handle_model_unload(home_dir: &Path) -> Value {
    let engine = duduclaw_inference::InferenceEngine::new(home_dir).await;
    if let Err(e) = engine.init().await {
        return serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": format!("Failed to init inference engine: {e}")}]
        });
    }

    match engine.unload_model().await {
        Ok(()) => serde_json::json!({
            "content": [{"type": "text", "text": "Model unloaded successfully. Memory freed."}]
        }),
        Err(e) => serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": format!("Failed to unload: {e}")}]
        }),
    }
}

async fn handle_hardware_info() -> Value {
    let hw = duduclaw_inference::hardware::detect_hardware().await;
    let text = format!(
        "Hardware Detection Results:\n\
         \n  GPU: {} ({:?})\
         \n  VRAM: {}MB total, {}MB available\
         \n  RAM: {}MB total, {}MB available\
         \n  CPU cores: {}\
         \n  Recommended backend: {}\
         \n  Recommended max model: {:.1}GB",
        hw.gpu_name, hw.gpu_type,
        hw.vram_total_mb, hw.vram_available_mb,
        hw.ram_total_mb, hw.ram_available_mb,
        hw.cpu_cores,
        hw.recommended_backend,
        hw.recommended_max_model_gb,
    );

    serde_json::json!({
        "content": [{"type": "text", "text": text}]
    })
}

async fn handle_route_query(params: &Value, home_dir: &Path) -> Value {
    let prompt = params.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
    let system_prompt = params.get("system_prompt").and_then(|v| v.as_str()).unwrap_or("");

    if prompt.is_empty() {
        return serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": "prompt is required"}]
        });
    }

    let engine = duduclaw_inference::InferenceEngine::new(home_dir).await;
    let decision = engine.route(system_prompt, prompt);

    let text = format!(
        "Routing Decision:\n\
         \n  Tier: {}\
         \n  Confidence: {:.2}\
         \n  Model: {}\
         \n  Reason: {}\
         \n  Router enabled: {}",
        decision.tier,
        decision.confidence,
        decision.model_id.as_deref().unwrap_or("(cloud api)"),
        decision.reason,
        engine.router_enabled(),
    );

    serde_json::json!({
        "content": [{"type": "text", "text": text}]
    })
}

async fn handle_inference_mode(home_dir: &Path) -> Value {
    let engine = duduclaw_inference::InferenceEngine::new(home_dir).await;
    let mode = engine.current_mode().await;
    let status = engine.manager().status().await;
    let mlx = engine.mlx_available().await;

    let text = format!(
        "Inference Manager Status:\n\
         \n  Current mode: {}\
         \n  Exo cluster: {}\
         \n  llamafile: {}\
         \n  MLX bridge: {}",
        mode,
        if status.exo_available { "available" } else { "unavailable" },
        if status.llamafile_available { "running" } else { "stopped" },
        if mlx { "available (Apple Silicon)" } else { "unavailable" },
    );

    serde_json::json!({
        "content": [{"type": "text", "text": text}]
    })
}

async fn handle_llamafile_start(params: &Value, home_dir: &Path) -> Value {
    let _file = params.get("file").and_then(|v| v.as_str());
    let engine = duduclaw_inference::InferenceEngine::new(home_dir).await;

    match engine.manager().start_llamafile().await {
        Ok(()) => serde_json::json!({
            "content": [{"type": "text", "text": "llamafile server started successfully"}]
        }),
        Err(e) => serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": format!("Failed to start llamafile: {e}")}]
        }),
    }
}

async fn handle_llamafile_stop(home_dir: &Path) -> Value {
    let engine = duduclaw_inference::InferenceEngine::new(home_dir).await;
    engine.manager().stop_llamafile().await;
    serde_json::json!({
        "content": [{"type": "text", "text": "llamafile server stopped"}]
    })
}

async fn handle_llamafile_list(home_dir: &Path) -> Value {
    let engine = duduclaw_inference::InferenceEngine::new(home_dir).await;
    let files = engine.manager().list_llamafiles().await;

    if files.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "No llamafiles found in ~/.duduclaw/llamafiles/\n\nTo get started:\n1. Download a .llamafile from huggingface.co or github.com/Mozilla-Ocho/llamafile\n2. Place it in ~/.duduclaw/llamafiles/\n3. Run llamafile_list again"}]
        });
    }

    let text = format!("Available llamafiles ({}):\n{}", files.len(),
        files.iter().map(|f| format!("  - {f}")).collect::<Vec<_>>().join("\n"));

    serde_json::json!({
        "content": [{"type": "text", "text": text}]
    })
}

// ── Model registry handlers ─────────────────────────────────

async fn handle_model_search(params: &Value, home_dir: &Path) -> Value {
    let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("chat gguf");

    let hw = duduclaw_inference::hardware::detect_hardware().await;
    let results = duduclaw_inference::model_registry::hf_api::search_models(
        query, hw.ram_available_mb, home_dir,
    ).await;

    // Also include curated
    let curated = duduclaw_inference::model_registry::curated::builtin_registry();
    let curated_filtered = duduclaw_inference::model_registry::curated::filter_by_hardware(&curated, hw.ram_available_mb);

    let mut all = curated_filtered;
    for r in &results {
        if !all.iter().any(|a| a.repo == r.repo && a.filename == r.filename) {
            all.push(r.clone());
        }
    }

    if all.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("No models found for query: '{query}' (RAM: {} MB)", hw.ram_available_mb)}]
        });
    }

    let mut text = format!("Models for '{}' (RAM: {} MB):\n", query, hw.ram_available_mb);
    for (i, e) in all.iter().enumerate().take(15) {
        let tier = match e.tier {
            duduclaw_inference::model_registry::ModelTier::Recommended => "[推薦]",
            duduclaw_inference::model_registry::ModelTier::Community => "[社群]",
        };
        text.push_str(&format!(
            "\n  {}. {} {} ({}, {}) — {}\n     repo: {} file: {}",
            i + 1, tier, e.name, e.params, e.size_display(),
            e.description, e.repo, e.filename
        ));
    }

    serde_json::json!({"content": [{"type": "text", "text": text}]})
}

async fn handle_model_download(params: &Value, home_dir: &Path) -> Value {
    let repo = params.get("repo").and_then(|v| v.as_str()).unwrap_or("");
    let filename = params.get("filename").and_then(|v| v.as_str()).unwrap_or("");

    if repo.is_empty() || filename.is_empty() {
        return serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": "repo and filename are required"}]
        });
    }

    // C-2: validate repo format (owner/name, safe characters only)
    if let Err(e) = duduclaw_inference::model_registry::downloader::validate_repo(repo) {
        return serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": format!("{e}")}]
        });
    }

    let models_dir = home_dir.join("models");
    let entry = duduclaw_inference::model_registry::RegistryEntry {
        name: String::new(), repo: repo.to_string(), filename: filename.to_string(),
        size_bytes: 0, quantization: String::new(), params: String::new(),
        languages: vec![], tags: vec![], min_ram_mb: 0, description: String::new(),
        tier: duduclaw_inference::model_registry::ModelTier::Community, downloads: 0,
    };

    match duduclaw_inference::model_registry::downloader::download_model(
        &entry.download_url(), &entry.mirror_url(), &models_dir, filename, None,
    ).await {
        Ok(path) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Downloaded to: {}", path.display())}]
        }),
        Err(_e) => serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": format!("Download failed. Check logs for details.\nManual URL: {}", entry.download_url())}]
        }),
    }
}

async fn handle_model_recommend(_home_dir: &Path) -> Value {
    let hw = duduclaw_inference::hardware::detect_hardware().await;
    let curated = duduclaw_inference::model_registry::curated::builtin_registry();
    let filtered = duduclaw_inference::model_registry::curated::filter_by_hardware(&curated, hw.ram_available_mb);

    let mut text = format!(
        "Hardware: {} ({:?})\nRAM: {} MB available / {} MB total\nRecommended max model: {:.1} GB\n\nRecommended models:\n",
        hw.gpu_name, hw.gpu_type, hw.ram_available_mb, hw.ram_total_mb, hw.recommended_max_model_gb
    );

    if filtered.is_empty() {
        text.push_str("\n  No models fit in available RAM.");
    } else {
        for (i, e) in filtered.iter().enumerate() {
            text.push_str(&format!(
                "\n  {}. {} ({}, {}) — {}\n     repo: {} file: {}",
                i + 1, e.name, e.params, e.size_display(), e.description, e.repo, e.filename
            ));
        }
    }

    serde_json::json!({"content": [{"type": "text", "text": text}]})
}

// ── Compression handlers ────────────────────────────────────

async fn handle_compress_text(params: &Value) -> Value {
    let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
    if text.is_empty() {
        return serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": "text is required"}]
        });
    }

    let (compressed, stats) = duduclaw_inference::compression::meta_token::compress(text);

    let result = format!(
        "Compression Result (Meta-Token, lossless):\n\
         \n  Original: {} chars\
         \n  Compressed: {} chars\
         \n  Ratio: {:.2}x\
         \n  Savings: {:.1}%\n\n{}",
        stats.original_len,
        stats.compressed_len,
        stats.ratio,
        (1.0 - 1.0 / stats.ratio) * 100.0,
        if stats.ratio > 1.05 {
            format!("Compressed text:\n{compressed}")
        } else {
            "No significant compression achieved (text has little repetition).".to_string()
        }
    );

    serde_json::json!({
        "content": [{"type": "text", "text": result}]
    })
}

async fn handle_decompress_text(params: &Value) -> Value {
    let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
    if text.is_empty() {
        return serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": "text is required"}]
        });
    }

    let decompressed = duduclaw_inference::compression::meta_token::decompress(text);

    serde_json::json!({
        "content": [{"type": "text", "text": decompressed}]
    })
}

// ── Cost telemetry handlers ─────────────────────────────────

async fn handle_cost_summary(params: &Value, home_dir: &Path) -> Value {
    // Ensure telemetry is initialized (idempotent on second call)
    let _ = duduclaw_gateway::cost_telemetry::init_telemetry(home_dir);

    let telemetry = match duduclaw_gateway::cost_telemetry::get_telemetry() {
        Some(t) => t,
        None => return serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": "Cost telemetry not initialized"}]
        }),
    };

    let hours = params.get("hours").and_then(|v| v.as_u64()).unwrap_or(24);

    if let Some(agent_id) = params.get("agent_id").and_then(|v| v.as_str()) {
        match telemetry.summary_by_agent(agent_id, hours).await {
            Ok(summary) => serde_json::json!({
                "content": [{"type": "text", "text": serde_json::to_string_pretty(&summary).unwrap_or_default()}]
            }),
            Err(e) => serde_json::json!({
                "isError": true,
                "content": [{"type": "text", "text": format!("Error: {e}")}]
            }),
        }
    } else {
        match telemetry.summary_global(hours).await {
            Ok(summary) => serde_json::json!({
                "content": [{"type": "text", "text": serde_json::to_string_pretty(&summary).unwrap_or_default()}]
            }),
            Err(e) => serde_json::json!({
                "isError": true,
                "content": [{"type": "text", "text": format!("Error: {e}")}]
            }),
        }
    }
}

async fn handle_cost_agents(params: &Value, home_dir: &Path) -> Value {
    let _ = duduclaw_gateway::cost_telemetry::init_telemetry(home_dir);

    let telemetry = match duduclaw_gateway::cost_telemetry::get_telemetry() {
        Some(t) => t,
        None => return serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": "Cost telemetry not initialized"}]
        }),
    };

    let hours = params.get("hours").and_then(|v| v.as_u64()).unwrap_or(24);

    match telemetry.all_agents_summary(hours).await {
        Ok(agents) => {
            if agents.is_empty() {
                return serde_json::json!({
                    "content": [{"type": "text", "text": "No cost data in the selected time window."}]
                });
            }
            let text = serde_json::to_string_pretty(&agents).unwrap_or_default();
            serde_json::json!({
                "content": [{"type": "text", "text": text}]
            })
        }
        Err(e) => serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": format!("Error: {e}")}]
        }),
    }
}

async fn handle_cost_recent(params: &Value) -> Value {
    let telemetry = match duduclaw_gateway::cost_telemetry::get_telemetry() {
        Some(t) => t,
        None => return serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": "Cost telemetry not initialized"}]
        }),
    };

    let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as u32;

    match telemetry.recent_records(limit).await {
        Ok(records) => {
            if records.is_empty() {
                return serde_json::json!({
                    "content": [{"type": "text", "text": "No cost records yet."}]
                });
            }
            let text = serde_json::to_string_pretty(&records).unwrap_or_default();
            serde_json::json!({
                "content": [{"type": "text", "text": text}]
            })
        }
        Err(e) => serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": format!("Error: {e}")}]
        }),
    }
}

// ── Odoo ERP handlers ───────────────────────────────────────

async fn handle_odoo_tool(tool: &str, params: &Value, home_dir: &Path, odoo: &OdooState) -> Value {
    use duduclaw_odoo::connector::OdooConnector;
    use duduclaw_odoo::models::{crm, sale, inventory, accounting};

    // odoo_connect doesn't require an existing connection
    if tool == "odoo_connect" {
        return handle_odoo_connect(home_dir, odoo).await;
    }

    if tool == "odoo_status" {
        let guard = odoo.read().await;
        return match guard.as_ref() {
            Some(conn) => {
                let s = conn.status();
                serde_json::json!({ "content": [{"type": "text", "text": format!(
                    "Odoo connected: {} ({})\nEdition: {}\nVersion: {}\nUser ID: {}\nEE modules: {}",
                    s.url, s.db, s.edition, s.version,
                    s.uid.map(|u| u.to_string()).unwrap_or("-".into()),
                    if s.ee_modules.is_empty() { "none".to_string() } else { s.ee_modules.join(", ") },
                )}]})
            }
            None => serde_json::json!({
                "content": [{"type": "text", "text": "Odoo not connected. Call odoo_connect first."}],
                "isError": true
            }),
        };
    }

    // All other tools require an active connection
    let guard = odoo.read().await;
    let conn = match guard.as_ref() {
        Some(c) => c,
        None => {
            return serde_json::json!({
                "content": [{"type": "text", "text": "Odoo not connected. Call odoo_connect first."}],
                "isError": true
            });
        }
    };

    let result: std::result::Result<String, String> = match tool {
        "odoo_crm_leads" => {
            let stage = params.get("stage").and_then(|v| v.as_str()).unwrap_or("");
            let limit = params.get("limit").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(20usize);
            let mut domain = vec![];
            if !stage.is_empty() {
                domain.push(serde_json::json!(["stage_id.name", "ilike", stage]));
            }
            match conn.search_read("crm.lead", domain, crm::CRM_LEAD_FIELDS, limit).await {
                Ok(data) => {
                    let leads: Vec<crm::CrmLead> = data.as_array().unwrap_or(&vec![]).iter().map(crm::map_crm_lead).collect();
                    Ok(serde_json::to_string_pretty(&leads).unwrap_or_default())
                }
                Err(e) => Err(e),
            }
        }
        "odoo_crm_create_lead" => {
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if name.is_empty() { return mcp_error("name is required"); }
            let mut vals = serde_json::json!({"name": name, "type": "lead"});
            if let Some(v) = params.get("contact_name").and_then(|v| v.as_str()) { vals["contact_name"] = serde_json::json!(v); }
            if let Some(v) = params.get("email").and_then(|v| v.as_str()) { vals["email_from"] = serde_json::json!(v); }
            if let Some(v) = params.get("phone").and_then(|v| v.as_str()) { vals["phone"] = serde_json::json!(v); }
            if let Some(v) = params.get("expected_revenue").and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok()) { vals["expected_revenue"] = serde_json::json!(v); }
            match conn.create("crm.lead", vals).await {
                Ok(id) => Ok(format!("CRM lead created (ID: {id})")),
                Err(e) => Err(e),
            }
        }
        "odoo_crm_update_stage" => {
            let lead_id = params.get("lead_id").and_then(|v| v.as_str()).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
            let stage_name = params.get("stage_name").and_then(|v| v.as_str()).unwrap_or("");
            if lead_id == 0 || stage_name.is_empty() { return mcp_error("lead_id and stage_name are required"); }
            // Find stage ID by name
            match conn.search_read("crm.stage", vec![serde_json::json!(["name", "ilike", stage_name])], &["id", "name"], 1).await {
                Ok(stages) => {
                    let stage_id = stages.as_array().and_then(|a| a.first()).and_then(|s| s["id"].as_i64()).unwrap_or(0);
                    if stage_id == 0 { return mcp_error(&format!("Stage '{stage_name}' not found")); }
                    match conn.write("crm.lead", &[lead_id], serde_json::json!({"stage_id": stage_id})).await {
                        Ok(_) => Ok(format!("Lead {lead_id} moved to stage '{stage_name}'")),
                        Err(e) => Err(e),
                    }
                }
                Err(e) => Err(e),
            }
        }
        "odoo_sale_orders" => {
            let status = params.get("status").and_then(|v| v.as_str()).unwrap_or("");
            let limit = params.get("limit").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(20usize);
            let mut domain = vec![];
            if !status.is_empty() { domain.push(serde_json::json!(["state", "=", status])); }
            match conn.search_read("sale.order", domain, sale::SALE_ORDER_FIELDS, limit).await {
                Ok(data) => {
                    let orders: Vec<sale::SaleOrder> = data.as_array().unwrap_or(&vec![]).iter().map(sale::map_sale_order).collect();
                    Ok(serde_json::to_string_pretty(&orders).unwrap_or_default())
                }
                Err(e) => Err(e),
            }
        }
        "odoo_sale_create_quotation" => {
            let partner_id = params.get("partner_id").and_then(|v| v.as_str()).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
            let product_id = params.get("product_id").and_then(|v| v.as_str()).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
            let qty = params.get("quantity").and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok()).unwrap_or(1.0);
            if partner_id == 0 || product_id == 0 { return mcp_error("partner_id and product_id are required"); }
            let vals = serde_json::json!({
                "partner_id": partner_id,
                "order_line": [[0, 0, {"product_id": product_id, "product_uom_qty": qty}]],
            });
            match conn.create("sale.order", vals).await {
                Ok(id) => Ok(format!("Quotation created (ID: {id})")),
                Err(e) => Err(e),
            }
        }
        "odoo_sale_confirm" => {
            let order_id = params.get("order_id").and_then(|v| v.as_str()).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
            if order_id == 0 { return mcp_error("order_id is required"); }
            match conn.execute_kw("sale.order", "action_confirm", vec![serde_json::json!([order_id])], serde_json::json!({})).await {
                Ok(_) => Ok(format!("Order {order_id} confirmed")),
                Err(e) => Err(e),
            }
        }
        "odoo_inventory_products" => {
            let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let limit = params.get("limit").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(20usize);
            let mut domain = vec![serde_json::json!(["detailed_type", "=", "product"])];
            if !query.is_empty() { domain.push(serde_json::json!(["name", "ilike", query])); }
            match conn.search_read("product.product", domain, inventory::PRODUCT_FIELDS, limit).await {
                Ok(data) => {
                    let products: Vec<inventory::Product> = data.as_array().unwrap_or(&vec![]).iter().map(inventory::map_product).collect();
                    Ok(serde_json::to_string_pretty(&products).unwrap_or_default())
                }
                Err(e) => Err(e),
            }
        }
        "odoo_inventory_check" => {
            let product_id = params.get("product_id").and_then(|v| v.as_str()).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
            if product_id == 0 { return mcp_error("product_id is required"); }
            let domain = vec![serde_json::json!(["product_id", "=", product_id])];
            match conn.search_read("stock.quant", domain, inventory::STOCK_QUANT_FIELDS, 10).await {
                Ok(data) => {
                    let quants: Vec<inventory::StockQuant> = data.as_array().unwrap_or(&vec![]).iter().map(inventory::map_stock_quant).collect();
                    Ok(serde_json::to_string_pretty(&quants).unwrap_or_default())
                }
                Err(e) => Err(e),
            }
        }
        "odoo_invoice_list" => {
            let status = params.get("status").and_then(|v| v.as_str()).unwrap_or("");
            let limit = params.get("limit").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(20usize);
            let mut domain = vec![serde_json::json!(["move_type", "in", ["out_invoice", "in_invoice"]])];
            if !status.is_empty() {
                match status {
                    "paid" => domain.push(serde_json::json!(["payment_state", "=", "paid"])),
                    "draft" => domain.push(serde_json::json!(["state", "=", "draft"])),
                    "posted" => domain.push(serde_json::json!(["state", "=", "posted"])),
                    _ => {}
                }
            }
            match conn.search_read("account.move", domain, accounting::INVOICE_FIELDS, limit).await {
                Ok(data) => {
                    let invoices: Vec<accounting::Invoice> = data.as_array().unwrap_or(&vec![]).iter().map(accounting::map_invoice).collect();
                    Ok(serde_json::to_string_pretty(&invoices).unwrap_or_default())
                }
                Err(e) => Err(e),
            }
        }
        "odoo_payment_status" => {
            let invoice_id = params.get("invoice_id").and_then(|v| v.as_str()).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
            if invoice_id == 0 { return mcp_error("invoice_id is required"); }
            match conn.search_read("account.move", vec![serde_json::json!(["id", "=", invoice_id])], accounting::INVOICE_FIELDS, 1).await {
                Ok(data) => {
                    let inv = data.as_array().and_then(|a| a.first()).map(accounting::map_invoice);
                    match inv {
                        Some(i) => Ok(serde_json::to_string_pretty(&i).unwrap_or_default()),
                        None => Err(format!("Invoice {invoice_id} not found")),
                    }
                }
                Err(e) => Err(e),
            }
        }
        "odoo_search" => {
            let model = params.get("model").and_then(|v| v.as_str()).unwrap_or("");
            if model.is_empty() { return mcp_error("model is required"); }
            if OdooConnector::is_model_blocked(model) { return mcp_error(&format!("Model '{model}' is blocked for security reasons")); }
            let domain_str = params.get("domain").and_then(|v| v.as_str()).unwrap_or("[]");
            let domain: Vec<Value> = serde_json::from_str(domain_str).unwrap_or_default();
            let fields_str = params.get("fields").and_then(|v| v.as_str()).unwrap_or("id,name");
            let fields: Vec<&str> = fields_str.split(',').map(|s| s.trim()).collect();
            let limit = params.get("limit").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(20usize);
            match conn.search_read(model, domain, &fields, limit).await {
                Ok(data) => Ok(serde_json::to_string_pretty(&data).unwrap_or_default()),
                Err(e) => Err(e),
            }
        }
        "odoo_execute" => {
            let model = params.get("model").and_then(|v| v.as_str()).unwrap_or("");
            let method = params.get("method").and_then(|v| v.as_str()).unwrap_or("");
            let ids_str = params.get("ids").and_then(|v| v.as_str()).unwrap_or("[]");
            if model.is_empty() || method.is_empty() { return mcp_error("model and method are required"); }
            if OdooConnector::is_model_blocked(model) { return mcp_error(&format!("Model '{model}' is blocked")); }

            // Whitelist safe Odoo methods — block dangerous ones like unlink, write on sensitive models (MCP-H6)
            const BLOCKED_METHODS: &[&str] = &[
                "unlink", "uninstall", "uninstall_hook",
                "init", "_auto_init", "_register_hook",
                "signal_workflow", "execute_import",
            ];
            if BLOCKED_METHODS.contains(&method) {
                return mcp_error(&format!("Method '{method}' is blocked for security reasons"));
            }
            let ids: Vec<Value> = serde_json::from_str(ids_str).unwrap_or_default();
            match conn.execute_kw(model, method, vec![serde_json::json!(ids)], serde_json::json!({})).await {
                Ok(data) => Ok(serde_json::to_string_pretty(&data).unwrap_or_default()),
                Err(e) => Err(e),
            }
        }
        "odoo_report" => {
            let report_name = params.get("report_name").and_then(|v| v.as_str()).unwrap_or("");
            let record_id = params.get("record_id").and_then(|v| v.as_str()).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
            if report_name.is_empty() || record_id == 0 { return mcp_error("report_name and record_id are required"); }
            // Reports use a special render method
            match conn.execute_kw("ir.actions.report", "render_qweb_pdf", vec![serde_json::json!(report_name), serde_json::json!([record_id])], serde_json::json!({})).await {
                Ok(_) => Ok(format!("Report '{report_name}' generated for record {record_id}. Download from Odoo.")),
                Err(e) => Err(format!("Report generation failed: {e}")),
            }
        }
        _ => Err(format!("Unknown Odoo tool: {tool}")),
    };

    match result {
        Ok(text) => serde_json::json!({ "content": [{"type": "text", "text": text}] }),
        Err(e) => serde_json::json!({ "content": [{"type": "text", "text": format!("Odoo error: {e}")}], "isError": true }),
    }
}

/// Connect to Odoo using config.toml [odoo] settings.
async fn handle_odoo_connect(home_dir: &Path, odoo: &OdooState) -> Value {
    let config_path = home_dir.join("config.toml");
    let content = match tokio::fs::read_to_string(&config_path).await {
        Ok(c) => c,
        Err(e) => return mcp_error(&format!("Cannot read config.toml: {e}")),
    };
    let table: toml::Table = match content.parse() {
        Ok(t) => t,
        Err(e) => return mcp_error(&format!("Invalid config.toml: {e}")),
    };
    let odoo_config = duduclaw_odoo::OdooConfig::from_toml(&table);
    if !odoo_config.is_configured() {
        return mcp_error("Odoo not configured. Add [odoo] section to config.toml with url and db.");
    }

    // Resolve credential (try api_key_enc first, then password_enc)
    let credential = decrypt_encrypted_value(&odoo_config.api_key_enc, home_dir)
        .or_else(|| decrypt_encrypted_value(&odoo_config.password_enc, home_dir))
        .unwrap_or_default();

    if credential.is_empty() {
        return mcp_error("Odoo credential not found. Set api_key_enc or password_enc in [odoo] config.");
    }

    match duduclaw_odoo::OdooConnector::connect(&odoo_config, &credential).await {
        Ok(conn) => {
            let status = conn.status();
            *odoo.write().await = Some(conn);
            serde_json::json!({
                "content": [{"type": "text", "text": format!(
                    "Connected to Odoo {} ({}) — {} v{}",
                    status.url, status.db, status.edition, status.version,
                )}]
            })
        }
        Err(e) => mcp_error(&format!("Odoo connection failed: {e}")),
    }
}

fn mcp_error(msg: &str) -> Value {
    serde_json::json!({ "content": [{"type": "text", "text": format!("Error: {msg}")}], "isError": true })
}

// ── Skill management handlers ───────────────────────────────

/// Search the local skill registry.
async fn handle_skill_search(params: &Value, home_dir: &Path) -> Value {
    let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
    if query.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: query is required"}],
            "isError": true
        });
    }

    let registry = duduclaw_agent::skill_registry::SkillRegistry::load(home_dir);
    let results = registry.search(query, 20);

    if results.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "No skills found for '{query}'. Registry has {} skills indexed.",
                registry.count()
            )}]
        });
    }

    let mut lines = vec![format!("Found {} skill(s) for '{query}':\n", results.len())];
    for s in &results {
        let tags = if s.tags.is_empty() {
            String::new()
        } else {
            format!(" [{}]", s.tags.join(", "))
        };
        lines.push(format!("- **{}**: {}{}", s.name, s.description, tags));
    }

    serde_json::json!({
        "content": [{"type": "text", "text": lines.join("\n")}]
    })
}

/// List all skills installed for a specific agent, including global skills.
async fn handle_skill_list(params: &Value, home_dir: &Path) -> Value {
    let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");

    let agent_name = if agent_id.is_empty() {
        resolve_main_agent_name(home_dir).await
    } else {
        agent_id.to_string()
    };

    // Collect global skills from ~/.duduclaw/skills/
    let global_skills_dir = home_dir.join("skills");
    let mut global_skills = Vec::new();
    let mut global_names = std::collections::HashSet::new();

    if let Ok(mut entries) = tokio::fs::read_dir(&global_skills_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("md") {
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("?")
                    .to_string();

                let meta = duduclaw_agent::skill_loader::parse_skill_file(&path).ok();
                let desc = meta
                    .as_ref()
                    .map(|m| m.meta.description.clone())
                    .unwrap_or_default();

                global_names.insert(name.clone());
                global_skills.push(format!("- {name}: {desc} (global)"));
            }
        }
    }

    // Collect agent-local skills from ~/.duduclaw/agents/<agent>/SKILLS/
    let skills_dir = home_dir.join("agents").join(&agent_name).join("SKILLS");
    let mut agent_skills = Vec::new();

    if let Ok(mut entries) = tokio::fs::read_dir(&skills_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("md") {
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("?")
                    .to_string();

                let meta = duduclaw_agent::skill_loader::parse_skill_file(&path).ok();
                let desc = meta
                    .as_ref()
                    .map(|m| m.meta.description.clone())
                    .unwrap_or_default();

                // If agent-local overrides a global skill, mark it
                let suffix = if global_names.contains(&name) { " (override)" } else { "" };
                agent_skills.push(format!("- {name}: {desc}{suffix}"));
            }
        }
    }

    // Remove global skills that are overridden by agent-local
    let agent_local_names: std::collections::HashSet<String> = agent_skills.iter()
        .filter_map(|s| s.strip_prefix("- ").and_then(|s| s.split(':').next()).map(String::from))
        .collect();
    global_skills.retain(|s| {
        let name = s.strip_prefix("- ")
            .and_then(|s| s.split(':').next())
            .unwrap_or("");
        !agent_local_names.contains(name)
    });

    let total = global_skills.len() + agent_skills.len();
    if total == 0 {
        serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "No skills installed for agent '{agent_name}'."
            )}]
        })
    } else {
        let mut parts = Vec::new();
        if !global_skills.is_empty() {
            parts.push(format!("**Global skills** ({}):\n{}", global_skills.len(), global_skills.join("\n")));
        }
        if !agent_skills.is_empty() {
            parts.push(format!("**Agent '{}' skills** ({}):\n{}", agent_name, agent_skills.len(), agent_skills.join("\n")));
        }
        let text = format!("Total {} skill(s):\n\n{}", total, parts.join("\n\n"));
        serde_json::json!({
            "content": [{"type": "text", "text": text}]
        })
    }
}

/// Resolve the main agent name from the agents directory.
async fn resolve_main_agent_name(home_dir: &Path) -> String {
    let agents_dir = home_dir.join("agents");
    let mut entries = match tokio::fs::read_dir(&agents_dir).await {
        Ok(e) => e,
        Err(_) => return String::new(),
    };

    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let toml_path = path.join("agent.toml");
        if let Ok(content) = tokio::fs::read_to_string(&toml_path).await {
            if let Ok(config) = toml::from_str::<duduclaw_core::types::AgentConfig>(&content) {
                if config.agent.role == duduclaw_core::types::AgentRole::Main {
                    return config.agent.name;
                }
            }
        }
    }

    String::new()
}

// ── Config reader ────────────────────────────────────────────

async fn read_config(home_dir: &Path) -> Option<toml::Table> {
    let config_path = home_dir.join("config.toml");
    let content = tokio::fs::read_to_string(&config_path).await.ok()?;
    content.parse().ok()
}

/// Load the AES-256 keyfile and create a CryptoEngine.
fn load_crypto_engine(home_dir: &Path) -> Option<duduclaw_security::crypto::CryptoEngine> {
    let keyfile = home_dir.join(".keyfile");
    let bytes = std::fs::read(&keyfile).ok()?;
    if bytes.len() == 32 {
        let mut key = [0u8; 32];
        key.copy_from_slice(&bytes);
        duduclaw_security::crypto::CryptoEngine::new(&key).ok()
    } else {
        None
    }
}

/// Decrypt an encrypted base64 value using the per-machine keyfile.
fn decrypt_encrypted_value(encrypted: &str, home_dir: &Path) -> Option<String> {
    if encrypted.is_empty() { return None; }
    let engine = load_crypto_engine(home_dir)?;
    let plain = engine.decrypt_string(encrypted).ok()?;
    if plain.is_empty() { None } else { Some(plain) }
}

/// Decrypt a channel token from config.toml.
///
/// Tries the encrypted field (`_enc` suffix) first, then falls back to the
/// plaintext field for backwards compatibility.
fn decrypt_channel_token(config: &toml::Table, enc_key: &str, plain_key: &str, home_dir: &Path) -> String {
    let channels = config.get("channels").and_then(|c| c.as_table());

    // Try encrypted field first
    if let Some(enc_val) = channels.and_then(|c| c.get(enc_key)).and_then(|v| v.as_str()) {
        if let Some(decrypted) = decrypt_encrypted_value(enc_val, home_dir) {
            return decrypted;
        }
    }

    // Fallback: plaintext field (backwards compat)
    channels
        .and_then(|c| c.get(plain_key))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Read the default agent name from config.toml.
async fn get_default_agent(home_dir: &Path) -> String {
    let config = read_config(home_dir).await;
    config
        .as_ref()
        .and_then(|t| t.get("general"))
        .and_then(|g| g.get("default_agent"))
        .and_then(|v| v.as_str())
        .unwrap_or("dudu")
        .to_string()
}

// ── Main server loop ─────────────────────────────────────────

/// Run the MCP server, reading JSON-RPC from stdin and writing responses to stdout.
pub async fn run_mcp_server(home_dir: &Path) -> Result<()> {
    info!("Starting DuDuClaw MCP server");

    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| DuDuClawError::Gateway(format!("Failed to create HTTP client: {e}")))?;

    // Initialize memory engine
    let memory_db_path = home_dir.join("memory.db");
    let memory = SqliteMemoryEngine::new(&memory_db_path)
        .map_err(|e| DuDuClawError::Memory(format!("Failed to open memory DB: {e}")))?;

    let default_agent = get_default_agent(home_dir).await;

    // Odoo connector (lazy — connected on first odoo_connect call)
    let odoo: std::sync::Arc<tokio::sync::RwLock<Option<duduclaw_odoo::OdooConnector>>> =
        std::sync::Arc::new(tokio::sync::RwLock::new(None));

    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin);
    let mut line = String::new();

    loop {
        line.clear();
        let bytes_read = reader.read_line(&mut line).await.map_err(|e| {
            DuDuClawError::Gateway(format!("Failed to read from stdin: {e}"))
        })?;

        if bytes_read == 0 {
            // EOF — client disconnected
            info!("MCP server: stdin closed, shutting down");
            break;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let request: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                warn!("MCP server: invalid JSON: {e}");
                let err = jsonrpc_error(&Value::Null, -32700, "Parse error");
                write_response(&mut stdout, &err).await?;
                continue;
            }
        };

        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let response = match method {
            "initialize" => handle_initialize(&id, &request),
            "tools/list" => handle_tools_list(&id),
            "tools/call" => {
                let params = request.get("params").cloned().unwrap_or(Value::Null);
                handle_tools_call(&id, &params, home_dir, &http, &memory, &default_agent, &odoo).await
            }
            "notifications/initialized" => {
                // This is a notification (no id expected in response), skip
                continue;
            }
            _ => jsonrpc_error(&id, -32601, &format!("Method not found: {method}")),
        };

        write_response(&mut stdout, &response).await?;
    }

    Ok(())
}

async fn write_response(
    stdout: &mut tokio::io::Stdout,
    response: &Value,
) -> Result<()> {
    let mut output = serde_json::to_string(response)
        .map_err(|e| DuDuClawError::Gateway(format!("Failed to serialize response: {e}")))?;
    output.push('\n');
    stdout.write_all(output.as_bytes()).await.map_err(|e| {
        DuDuClawError::Gateway(format!("Failed to write to stdout: {e}"))
    })?;
    stdout.flush().await.map_err(|e| {
        DuDuClawError::Gateway(format!("Failed to flush stdout: {e}"))
    })?;
    Ok(())
}

// ── Method handlers ──────────────────────────────────────────

fn handle_initialize(id: &Value, _request: &Value) -> Value {
    jsonrpc_response(
        id,
        serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": {}
            },
            "serverInfo": {
                "name": "duduclaw",
                "version": env!("CARGO_PKG_VERSION")
            }
        }),
    )
}

fn handle_tools_list(id: &Value) -> Value {
    let tools: Vec<Value> = TOOLS.iter().map(build_tool_schema).collect();
    jsonrpc_response(id, serde_json::json!({ "tools": tools }))
}

type OdooState = std::sync::Arc<tokio::sync::RwLock<Option<duduclaw_odoo::OdooConnector>>>;

async fn handle_tools_call(
    id: &Value,
    params: &Value,
    home_dir: &Path,
    http: &reqwest::Client,
    memory: &SqliteMemoryEngine,
    default_agent: &str,
    odoo: &OdooState,
) -> Value {
    let tool_name = params
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));

    info!(tool = %tool_name, "MCP tools/call");

    let result = match tool_name {
        "send_message" => handle_send_message(&arguments, home_dir, http).await,
        "web_search" => handle_web_search(&arguments, http).await,
        "memory_search" => handle_memory_search(&arguments, memory, default_agent).await,
        "memory_store" => handle_memory_store(&arguments, memory, default_agent).await,
        "send_to_agent" => handle_send_to_agent(&arguments, home_dir).await,
        "send_photo" => handle_send_media(&arguments, home_dir, http, "photo").await,
        "send_sticker" => handle_send_media(&arguments, home_dir, http, "sticker").await,
        "log_mood" => handle_log_mood(&arguments, home_dir, memory, default_agent).await,
        "schedule_task" => handle_schedule_task(&arguments, home_dir).await,
        "create_agent" => handle_create_agent(&arguments, home_dir).await,
        "list_agents" => handle_list_agents(home_dir).await,
        "agent_status" => handle_agent_status(&arguments, home_dir).await,
        "spawn_agent" => handle_spawn_agent(&arguments, home_dir).await,
        "agent_update" => handle_agent_update(&arguments, home_dir).await,
        "agent_remove" => handle_agent_remove(&arguments, home_dir).await,
        "agent_update_soul" => handle_agent_update_soul(&arguments, home_dir).await,
        "skill_search" => handle_skill_search(&arguments, home_dir).await,
        "skill_list" => handle_skill_list(&arguments, home_dir).await,
        "submit_feedback" => handle_submit_feedback(&arguments, home_dir, default_agent).await,
        "evolution_toggle" => handle_evolution_toggle(&arguments, home_dir).await,
        "evolution_status" => handle_evolution_status_tool(&arguments, home_dir, default_agent).await,
        // Channel settings tools
        "channel_config" => handle_channel_config(&arguments, home_dir).await,
        "channel_config_list" => handle_channel_config_list(&arguments, home_dir).await,
        // Local inference tools
        "inference_status" => handle_inference_status(home_dir).await,
        "model_list" => handle_model_list(home_dir).await,
        "model_load" => handle_model_load(&arguments, home_dir).await,
        "model_unload" => handle_model_unload(home_dir).await,
        "hardware_info" => handle_hardware_info().await,
        "route_query" => handle_route_query(&arguments, home_dir).await,
        "inference_mode" => handle_inference_mode(home_dir).await,
        "llamafile_start" => handle_llamafile_start(&arguments, home_dir).await,
        "llamafile_stop" => handle_llamafile_stop(home_dir).await,
        "llamafile_list" => handle_llamafile_list(home_dir).await,
        // Compression tools
        // Model registry tools
        "model_search" => handle_model_search(&arguments, home_dir).await,
        "model_download" => handle_model_download(&arguments, home_dir).await,
        "model_recommend" => handle_model_recommend(home_dir).await,
        "compress_text" => handle_compress_text(&arguments).await,
        "decompress_text" => handle_decompress_text(&arguments).await,
        // Cost telemetry tools
        "cost_summary" => handle_cost_summary(&arguments, home_dir).await,
        "cost_agents" => handle_cost_agents(&arguments, home_dir).await,
        "cost_recent" => handle_cost_recent(&arguments).await,
        // Voice / ASR / TTS tools
        "transcribe_audio" => handle_transcribe_audio(&arguments).await,
        "synthesize_speech" => handle_synthesize_speech(&arguments).await,
        // Odoo ERP tools
        t if t.starts_with("odoo_") => handle_odoo_tool(t, &arguments, home_dir, odoo).await,
        _ => {
            return jsonrpc_error(
                id,
                -32602,
                &format!("Unknown tool: {tool_name}"),
            );
        }
    };

    jsonrpc_response(id, result)
}

// ── Voice / ASR / TTS handlers ─────────────────────────────────

async fn handle_transcribe_audio(args: &Value) -> Value {
    use base64::Engine;

    let audio_b64 = match args.get("audio_base64").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return tool_error("Missing required parameter: audio_base64"),
    };

    // Limit input size: 34MB base64 ≈ 25MB decoded
    const MAX_B64_LEN: usize = 34 * 1024 * 1024;
    if audio_b64.len() > MAX_B64_LEN {
        return tool_error(&format!("Audio too large: {} bytes (max 25MB)", audio_b64.len() * 3 / 4));
    }

    let language = args.get("language").and_then(|v| v.as_str()).unwrap_or("zh");

    let audio_bytes = match base64::engine::general_purpose::STANDARD.decode(audio_b64) {
        Ok(b) => b,
        Err(e) => return tool_error(&format!("Invalid base64: {e}")),
    };

    // Transcribe via Whisper API (sends raw audio bytes, format auto-detected)
    match duduclaw_inference::whisper::transcribe(
        &audio_bytes,
        Some(language),
        &duduclaw_inference::whisper::WhisperMode::Api,
    ).await {
        Ok(text) => tool_text(&text),
        Err(e) => tool_error(&format!("Transcription failed: {e}")),
    }
}

async fn handle_synthesize_speech(args: &Value) -> Value {
    use base64::Engine;
    use duduclaw_gateway::tts::TtsProvider;

    let text = match args.get("text").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s,
        _ => return tool_error("Missing required parameter: text"),
    };
    let voice = args.get("voice").and_then(|v| v.as_str()).unwrap_or("");

    let provider = duduclaw_gateway::tts::EdgeTtsProvider::new();
    match provider.synthesize(text, voice).await {
        Ok(audio_bytes) => {
            let b64 = base64::engine::general_purpose::STANDARD.encode(&audio_bytes);
            serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": format!("Audio synthesized ({} bytes). Base64 data:\n{}", audio_bytes.len(), b64)
                }]
            })
        }
        Err(e) => tool_error(&format!("Speech synthesis failed: {e}")),
    }
}

// ── Channel settings tool handlers ──────────────────────────────

const VALID_CHANNELS: &[&str] = &["discord", "telegram", "slack", "line", "whatsapp", "feishu"];
const VALID_KEYS: &[&str] = &["mention_only", "auto_thread", "allowed_channels", "agent_override", "response_mode", "thread_archive_minutes"];

/// Validate scope_id: max 64 chars, alphanumeric + underscore/hyphen or "global"/"dm".
fn validate_scope_id(scope_id: &str) -> std::result::Result<(), String> {
    if scope_id.len() > 64 {
        return Err("scope_id too long (max 64 chars)".into());
    }
    if scope_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        Ok(())
    } else {
        Err("scope_id contains invalid characters".into())
    }
}

/// Validate value based on key type.
fn validate_value(key: &str, value: &str) -> std::result::Result<(), String> {
    match key {
        "mention_only" | "auto_thread" => {
            if value != "true" && value != "false" {
                return Err(format!("{key} must be 'true' or 'false'"));
            }
        }
        "allowed_channels" => {
            if serde_json::from_str::<Vec<String>>(value).is_err() {
                return Err("allowed_channels must be a JSON array of strings, e.g. [\"ch1\",\"ch2\"]".into());
            }
        }
        "response_mode" => {
            if !["embed", "plain", "auto"].contains(&value) {
                return Err("response_mode must be 'embed', 'plain', or 'auto'".into());
            }
        }
        "thread_archive_minutes" => {
            if !["60", "1440", "4320", "10080"].contains(&value) {
                return Err("thread_archive_minutes must be 60, 1440, 4320, or 10080".into());
            }
        }
        _ => {} // agent_override: any string is valid (checked against registry at use time)
    }
    Ok(())
}

async fn handle_channel_config(args: &Value, home_dir: &Path) -> Value {
    let channel = match args.get("channel").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return tool_error("Missing required parameter: channel"),
    };
    let scope_id = match args.get("scope_id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return tool_error("Missing required parameter: scope_id"),
    };
    let key = match args.get("key").and_then(|v| v.as_str()) {
        Some(k) => k,
        None => return tool_error("Missing required parameter: key"),
    };

    if !VALID_CHANNELS.contains(&channel) {
        return tool_error(&format!("Invalid channel type: {channel}"));
    }
    if !VALID_KEYS.contains(&key) {
        return tool_error(&format!("Invalid key: {key}. Valid keys: {}", VALID_KEYS.join(", ")));
    }
    if let Err(e) = validate_scope_id(scope_id) {
        return tool_error(&format!("Invalid scope_id: {e}"));
    }

    let db_path = home_dir.join("sessions.db");
    let mgr = match duduclaw_gateway::channel_settings::ChannelSettingsManager::from_session_db(&db_path) {
        Ok(m) => m,
        Err(e) => return tool_error(&format!("Failed to open settings DB: {e}")),
    };

    if let Some(value) = args.get("value").and_then(|v| v.as_str()) {
        if let Err(e) = validate_value(key, value) {
            return tool_error(&format!("Invalid value: {e}"));
        }
        match mgr.set(channel, scope_id, key, value).await {
            Ok(()) => tool_text(&format!("Set {channel}/{scope_id}/{key} = {value}")),
            Err(e) => tool_error(&format!("Failed to set: {e}")),
        }
    } else {
        let value = mgr.get_with_fallback(channel, scope_id, key, "(not set)").await;
        tool_text(&format!("{channel}/{scope_id}/{key} = {value}"))
    }
}

async fn handle_channel_config_list(args: &Value, home_dir: &Path) -> Value {
    let channel = match args.get("channel").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return tool_error("Missing required parameter: channel"),
    };
    let scope_id = match args.get("scope_id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return tool_error("Missing required parameter: scope_id"),
    };

    if !VALID_CHANNELS.contains(&channel) {
        return tool_error(&format!("Invalid channel type: {channel}"));
    }
    if let Err(e) = validate_scope_id(scope_id) {
        return tool_error(&format!("Invalid scope_id: {e}"));
    }

    let db_path = home_dir.join("sessions.db");
    let mgr = match duduclaw_gateway::channel_settings::ChannelSettingsManager::from_session_db(&db_path) {
        Ok(m) => m,
        Err(e) => return tool_error(&format!("Failed to open settings DB: {e}")),
    };

    let all = mgr.get_all(channel, scope_id).await;
    if all.is_empty() {
        tool_text(&format!("No settings configured for {channel}/{scope_id}. Using defaults."))
    } else {
        let lines: Vec<String> = all.iter().map(|(k, v)| format!("{k} = {v}")).collect();
        tool_text(&format!("Settings for {channel}/{scope_id}:\n{}", lines.join("\n")))
    }
}

fn tool_text(text: &str) -> Value {
    serde_json::json!({
        "content": [{ "type": "text", "text": text }]
    })
}

fn tool_error(msg: &str) -> Value {
    serde_json::json!({
        "content": [{ "type": "text", "text": msg }],
        "isError": true
    })
}
