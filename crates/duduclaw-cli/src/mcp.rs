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
    // ── Odoo ERP tools (Phase 3) ────────────────────────────────
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
            let token = config
                .get("channels")
                .and_then(|c| c.get("telegram_bot_token"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
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
            let token = config
                .get("channels")
                .and_then(|c| c.get("line_channel_token"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
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
            let token = config
                .get("channels")
                .and_then(|c| c.get("discord_bot_token"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
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

    let result = match http
        .get(&url)
        .header("User-Agent", "DuDuClaw/0.1")
        .send()
        .await
    {
        Ok(resp) => match resp.text().await {
            Ok(body) => extract_search_results(&body),
            Err(e) => format!("Error reading response: {e}"),
        },
        Err(e) => format!("Error performing search: {e}"),
    };

    serde_json::json!({
        "content": [{"type": "text", "text": result}]
    })
}

/// Extract text results from DuckDuckGo HTML response.
fn extract_search_results(html: &str) -> String {
    // Simple extraction: find result snippets between common markers
    let mut results = Vec::new();
    let mut remaining = html;

    // Look for result__snippet class
    while let Some(start) = remaining.find("class=\"result__snippet") {
        remaining = &remaining[start..];
        if let Some(tag_end) = remaining.find('>') {
            remaining = &remaining[tag_end + 1..];
            if let Some(end) = remaining.find("</") {
                let snippet = &remaining[..end];
                let clean = strip_html_tags(snippet).trim().to_string();
                if !clean.is_empty() {
                    results.push(clean);
                }
                remaining = &remaining[end..];
            }
        }
        if results.len() >= 5 {
            break;
        }
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

/// Strip HTML tags from a string.
fn strip_html_tags(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => output.push(ch),
            _ => {}
        }
    }
    output
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

    let entry = MemoryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        agent_id: agent_id.to_string(),
        content: content.to_string(),
        timestamp: chrono::Utc::now(),
        tags,
        embedding: None,
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
    let result = match channel {
        "telegram" => {
            let token = config.as_ref()
                .and_then(|c| c.get("channels"))
                .and_then(|c| c.get("telegram_bot_token"))
                .and_then(|v| v.as_str()).unwrap_or("");
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
            let token = config.as_ref()
                .and_then(|c| c.get("channels"))
                .and_then(|c| c.get("discord_bot_token"))
                .and_then(|v| v.as_str()).unwrap_or("");
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

    let entry = MemoryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        agent_id: agent_id.to_string(),
        content,
        timestamp: chrono::Utc::now(),
        tags: vec!["mood".to_string(), mood.to_string()],
        embedding: None,
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

    // Validate name: lowercase, alphanumeric + hyphens only
    if !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: name must be lowercase alphanumeric with hyphens only"}],
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

    // Write agent.toml
    let agent_toml = format!(
r#"[agent]
name = "{name}"
display_name = "{display_name}"
role = "{role}"
status = "active"
trigger = "{trigger}"
reports_to = "{reports_to}"
icon = "{icon}"

[model]
preferred = "{model}"
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
micro_reflection = true
meso_reflection = false
macro_reflection = false
skill_auto_activate = false
skill_security_scan = true
"#);

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
             Reports to: {reports_to}\n\
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

/// Count pending agent_message entries in bus_queue.jsonl for a given agent.
async fn count_pending_tasks(home_dir: &Path, agent_id: &str) -> usize {
    let queue_path = home_dir.join("bus_queue.jsonl");
    let content = match tokio::fs::read_to_string(&queue_path).await {
        Ok(c) => c,
        Err(_) => return 0,
    };

    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| {
            v.get("type").and_then(|t| t.as_str()) == Some("agent_message")
                && v.get("agent_id").and_then(|a| a.as_str()) == Some(agent_id)
        })
        .count()
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
    let credential = if !odoo_config.api_key_enc.is_empty() {
        // Try to decrypt
        let keyfile = home_dir.join(".keyfile");
        if let Ok(bytes) = std::fs::read(&keyfile) {
            if bytes.len() == 32 {
                let mut key = [0u8; 32];
                key.copy_from_slice(&bytes);
                duduclaw_security::crypto::CryptoEngine::new(&key)
                    .ok()
                    .and_then(|e| e.decrypt_string(&odoo_config.api_key_enc).ok())
                    .unwrap_or_default()
            } else { String::new() }
        } else { String::new() }
    } else if !odoo_config.password_enc.is_empty() {
        let keyfile = home_dir.join(".keyfile");
        if let Ok(bytes) = std::fs::read(&keyfile) {
            if bytes.len() == 32 {
                let mut key = [0u8; 32];
                key.copy_from_slice(&bytes);
                duduclaw_security::crypto::CryptoEngine::new(&key)
                    .ok()
                    .and_then(|e| e.decrypt_string(&odoo_config.password_enc).ok())
                    .unwrap_or_default()
            } else { String::new() }
        } else { String::new() }
    } else {
        String::new()
    };

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

/// List all skills installed for a specific agent.
async fn handle_skill_list(params: &Value, home_dir: &Path) -> Value {
    let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");

    let agent_name = if agent_id.is_empty() {
        resolve_main_agent_name(home_dir).await
    } else {
        agent_id.to_string()
    };

    let skills_dir = home_dir.join("agents").join(&agent_name).join("SKILLS");
    let mut skills = Vec::new();

    if let Ok(mut entries) = tokio::fs::read_dir(&skills_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("md") {
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("?")
                    .to_string();

                // Try to parse metadata
                let meta = duduclaw_agent::skill_loader::parse_skill_file(&path).ok();
                let desc = meta
                    .as_ref()
                    .map(|m| m.meta.description.clone())
                    .unwrap_or_default();

                skills.push(format!("- {name}: {desc}"));
            }
        }
    }

    if skills.is_empty() {
        serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "No skills installed for agent '{agent_name}'."
            )}]
        })
    } else {
        let text = format!(
            "Agent '{}' has {} skill(s):\n\n{}",
            agent_name,
            skills.len(),
            skills.join("\n")
        );
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
        "skill_search" => handle_skill_search(&arguments, home_dir).await,
        "skill_list" => handle_skill_list(&arguments, home_dir).await,
        "submit_feedback" => handle_submit_feedback(&arguments, home_dir, default_agent).await,
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
