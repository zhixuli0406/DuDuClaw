//! OpenAI Codex CLI runtime — `codex exec --json` JSONL streaming.
//!
//! Codex CLI outputs JSONL events on stdout when invoked with `--json`:
//!   - `thread.started` — session created
//!   - `turn.started` / `turn.completed` — contains token usage
//!   - `item.completed` (type=message) — assistant text content
//!
//! Authentication: `OPENAI_API_KEY` environment variable.

use async_trait::async_trait;
use serde::Deserialize;
use tracing::info;

use super::{AgentRuntime, RuntimeContext, RuntimeResponse};

const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// Runtime that delegates to the OpenAI Codex CLI.
pub struct CodexRuntime {
    codex_path: String,
}

impl CodexRuntime {
    pub fn new() -> Self {
        Self {
            codex_path: "codex".to_string(),
        }
    }
}

// ── JSONL event types ───────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct CodexEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(flatten)]
    extra: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct CodexUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
}

// ── AgentRuntime impl ───────────────────────────────────────────

#[async_trait]
impl AgentRuntime for CodexRuntime {
    fn name(&self) -> &str {
        "codex"
    }

    async fn execute(
        &self,
        prompt: &str,
        context: &RuntimeContext,
    ) -> Result<RuntimeResponse, String> {
        info!(agent = %context.agent_id, "CodexRuntime: executing via codex exec --json");

        // Limit system_prompt to 64KB to avoid ARG_MAX issues
        const MAX_SYSTEM_PROMPT_BYTES: usize = 65536;
        let system_prompt: &str = if context.system_prompt.len() > MAX_SYSTEM_PROMPT_BYTES {
            tracing::warn!(
                agent = %context.agent_id,
                original_len = context.system_prompt.len(),
                "system_prompt truncated to 64KB"
            );
            &context.system_prompt[..MAX_SYSTEM_PROMPT_BYTES]
        } else {
            &context.system_prompt
        };

        // Prevent argument injection: prompts starting with '-' would be parsed as flags
        let safe_prompt = if prompt.starts_with('-') {
            format!(" {prompt}")
        } else {
            prompt.to_string()
        };

        let mut cmd = tokio::process::Command::new(&self.codex_path);
        cmd.arg("exec")
            .arg("--json")
            .arg("--full-auto");

        // Pass system prompt (SOUL.md, role definitions) as instructions
        if !system_prompt.is_empty() {
            cmd.arg("--instructions").arg(system_prompt);
        }

        cmd.arg(&safe_prompt);

        // Set model if specified
        if !context.model.is_empty() {
            cmd.arg("-m").arg(&context.model);
        }

        // Set working directory
        if let Some(ref dir) = context.agent_dir {
            cmd.arg("--cd").arg(dir);
        }

        // Pass API key if available
        let api_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
        if !api_key.is_empty() {
            cmd.env("OPENAI_API_KEY", &api_key);
        }

        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let output = tokio::time::timeout(
            std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            cmd.output(),
        )
        .await
        .map_err(|_| "Codex CLI timed out".to_string())?
        .map_err(|e| format!("Failed to spawn codex: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("Codex CLI exited with {}: {}", output.status, stderr.chars().take(500).collect::<String>()));
        }

        // Parse JSONL output
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut content = String::new();
        let mut input_tokens: u64 = 0;
        let mut output_tokens: u64 = 0;

        for line in stdout.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(event) = serde_json::from_str::<CodexEvent>(line) {
                match event.event_type.as_str() {
                    "item.completed" => {
                        // Extract text from message items
                        if let Some(item) = event.extra.get("item") {
                            if item.get("type").and_then(|t| t.as_str()) == Some("message") {
                                if let Some(text) = item
                                    .get("content")
                                    .and_then(|c| c.as_array())
                                    .and_then(|arr| arr.iter().find(|b| b.get("type").and_then(|t| t.as_str()) == Some("output_text")))
                                    .and_then(|b| b.get("text"))
                                    .and_then(|t| t.as_str())
                                {
                                    content = text.to_string();
                                }
                            }
                        }
                    }
                    "turn.completed" => {
                        // Extract token usage
                        if let Some(usage) = event.extra.get("usage") {
                            if let Ok(u) = serde_json::from_value::<CodexUsage>(usage.clone()) {
                                input_tokens = u.input_tokens;
                                output_tokens = u.output_tokens;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        if content.is_empty() {
            // Fallback: use the last line as content
            content = stdout.lines().last().unwrap_or("").to_string();
        }

        Ok(RuntimeResponse {
            content,
            input_tokens,
            output_tokens,
            cache_read_tokens: 0,
            model_used: context.model.clone(),
            runtime_name: "codex".to_string(),
        })
    }

    async fn is_available(&self) -> bool {
        tokio::process::Command::new(&self.codex_path)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

// ── Streaming ───────────────────────────────────────────────────

impl CodexRuntime {
    /// Execute and return chunks. Codex CLI does not support true streaming,
    /// so this wraps the normal execution into a single `Done` chunk.
    pub async fn execute_streaming(
        &self,
        prompt: &str,
        context: &super::RuntimeContext,
    ) -> Result<Vec<super::RuntimeChunk>, String> {
        let response = self.execute(prompt, context).await?;
        Ok(vec![super::RuntimeChunk::Done(response)])
    }
}

// ── MCP config ──────────────────────────────────────────────────

impl CodexRuntime {
    /// Write MCP server configuration to the agent's codex config.
    pub fn write_mcp_config(agent_dir: &std::path::Path, servers: &std::collections::HashMap<String, serde_json::Value>) -> Result<(), String> {
        let config_path = agent_dir.join(".codex").join("config.toml");
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut content = String::from("[mcp_servers]\n");
        for (name, config) in servers {
            if name.contains('.') {
                content.push_str(&format!("[mcp_servers.\"{}\"]\n", name));
            } else {
                content.push_str(&format!("[mcp_servers.{name}]\n"));
            }
            if let Some(obj) = config.as_object() {
                for (k, v) in obj {
                    let toml_val = match v {
                        serde_json::Value::String(s) => format!("{k} = \"{}\"\n", s.replace('\\', "\\\\").replace('"', "\\\"")),
                        serde_json::Value::Array(arr) => {
                            let items: Vec<String> = arr.iter().map(|item| {
                                if let Some(s) = item.as_str() {
                                    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
                                } else {
                                    item.to_string()
                                }
                            }).collect();
                            format!("{k} = [{}]\n", items.join(", "))
                        }
                        _ => format!("{k} = {v}\n"),
                    };
                    content.push_str(&toml_val);
                }
            }
        }
        std::fs::write(&config_path, content).map_err(|e| e.to_string())
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_codex_event() {
        let line = r#"{"type":"turn.completed","usage":{"input_tokens":100,"output_tokens":50}}"#;
        let event: CodexEvent = serde_json::from_str(line).unwrap();
        assert_eq!(event.event_type, "turn.completed");
        let usage: CodexUsage = serde_json::from_value(event.extra.get("usage").unwrap().clone()).unwrap();
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
    }

    #[test]
    fn test_parse_item_completed() {
        let line = r#"{"type":"item.completed","item":{"type":"message","content":[{"type":"output_text","text":"Hello world"}]}}"#;
        let event: CodexEvent = serde_json::from_str(line).unwrap();
        assert_eq!(event.event_type, "item.completed");
        let text = event.extra
            .get("item").unwrap()
            .get("content").unwrap()
            .as_array().unwrap()[0]
            .get("text").unwrap()
            .as_str().unwrap();
        assert_eq!(text, "Hello world");
    }
}
