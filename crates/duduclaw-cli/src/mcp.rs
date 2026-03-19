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
            ParamDef { name: "score", description: "Mood score (1-10)", required: true },
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
];

// ── JSON-RPC helpers ─────────────────────────────────────────

fn jsonrpc_response(id: &Value, result: Value) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
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
                    Ok(resp) => format!("Message sent. Status: {}", resp.status()),
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
                    Ok(resp) => format!("Message sent. Status: {}", resp.status()),
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
                    Ok(resp) => format!("Message sent. Status: {}", resp.status()),
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
        move || -> bool {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                writeln!(f, "{task_str}").is_ok()
            } else { false }
        }
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
    let channel = params.get("channel").and_then(|v| v.as_str()).unwrap_or("telegram");
    let chat_id = params.get("chat_id").and_then(|v| v.as_str()).unwrap_or("");
    let url_or_id = params.get("url")
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
    let task = params.get("task").or_else(|| params.get("prompt")).and_then(|v| v.as_str()).unwrap_or("");
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("unnamed");

    if cron.is_empty() || task.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: cron and task are required"}],
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
        move || -> bool {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                writeln!(f, "{entry_str}").is_ok()
            } else { false }
        }
    }).await.unwrap_or(false);

    serde_json::json!({
        "content": [{"type": "text", "text": if queued {
            format!("Task '{name}' scheduled (id: {task_id}, cron: {cron})")
        } else {
            "Error: Failed to persist task".to_string()
        }}]
    })
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
                handle_tools_call(&id, &params, home_dir, &http, &memory, &default_agent).await
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

async fn handle_tools_call(
    id: &Value,
    params: &Value,
    home_dir: &Path,
    http: &reqwest::Client,
    memory: &SqliteMemoryEngine,
    default_agent: &str,
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
