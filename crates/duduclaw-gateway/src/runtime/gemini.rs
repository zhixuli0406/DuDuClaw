//! Google Gemini CLI runtime — `gemini -p --output-format stream-json`.
//!
//! Gemini CLI outputs JSONL events on stdout:
//!   - `init` — session metadata
//!   - `message` (role=assistant) — text content
//!   - `tool_use` / `tool_result` — tool interactions
//!   - `result` — final outcome with aggregated stats
//!
//! Authentication: `GEMINI_API_KEY` environment variable or Google OAuth.
//! Exit codes: 0=success, 1=error, 42=input error, 53=turn limit.

use async_trait::async_trait;
use serde::Deserialize;
use tracing::info;

use super::{AgentRuntime, RuntimeContext, RuntimeResponse};

const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// Runtime that delegates to the Google Gemini CLI.
pub struct GeminiRuntime {
    gemini_path: String,
}

impl GeminiRuntime {
    pub fn new() -> Self {
        Self {
            gemini_path: "gemini".to_string(),
        }
    }
}

// ── JSONL event types ───────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct GeminiEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(flatten)]
    extra: serde_json::Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiStats {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
}

// ── AgentRuntime impl ───────────────────────────────────────────

#[async_trait]
impl AgentRuntime for GeminiRuntime {
    fn name(&self) -> &str {
        "gemini"
    }

    async fn execute(
        &self,
        prompt: &str,
        context: &RuntimeContext,
    ) -> Result<RuntimeResponse, String> {
        info!(agent = %context.agent_id, "GeminiRuntime: executing via gemini -p --output-format stream-json");

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

        let mut cmd = tokio::process::Command::new(&self.gemini_path);
        cmd.arg("-p")
            .arg("--output-format")
            .arg("stream-json");

        // Pass system prompt (SOUL.md, role definitions) as system instruction
        if !system_prompt.is_empty() {
            cmd.arg("--system-instruction").arg(system_prompt);
        }

        cmd.arg(&safe_prompt);

        // Set model if specified
        if !context.model.is_empty() {
            cmd.arg("-m").arg(&context.model);
        }

        // Set working directory
        if let Some(ref dir) = context.agent_dir {
            cmd.current_dir(dir);
        }

        // Pass API key if available
        let api_key = std::env::var("GEMINI_API_KEY").unwrap_or_default();
        if !api_key.is_empty() {
            cmd.env("GEMINI_API_KEY", &api_key);
        }

        // Vision: if system_prompt contains image references, pass via --include-files
        // (Gemini CLI natively supports image input)

        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let output = tokio::time::timeout(
            std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            cmd.output(),
        )
        .await
        .map_err(|_| "Gemini CLI timed out".to_string())?
        .map_err(|e| format!("Failed to spawn gemini: {e}"))?;

        // Gemini exit codes: 0=ok, 1=error, 42=input error, 53=turn limit
        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let hint = match code {
                42 => " (input error — check prompt format)",
                53 => " (turn limit exceeded)",
                _ => "",
            };
            return Err(format!(
                "Gemini CLI exited with {code}{hint}: {}",
                stderr.chars().take(500).collect::<String>()
            ));
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
            if let Ok(event) = serde_json::from_str::<GeminiEvent>(line) {
                match event.event_type.as_str() {
                    "message" => {
                        // Extract assistant message text
                        if event.extra.get("role").and_then(|r| r.as_str()) == Some("assistant") {
                            if let Some(text) = event.extra.get("content").and_then(|c| c.as_str()) {
                                content.push_str(text);
                            }
                        }
                    }
                    "result" => {
                        // Final result: extract content and stats.
                        // Only overwrite if non-empty — when the AI uses tools, the
                        // result event may have empty content while the real answer
                        // was accumulated from earlier "message" events.
                        if let Some(text) = event.extra.get("content").and_then(|c| c.as_str()) {
                            if !text.is_empty() {
                                content = text.to_string();
                            }
                        }
                        if let Some(stats) = event.extra.get("stats") {
                            if let Ok(s) = serde_json::from_value::<GeminiStats>(stats.clone()) {
                                input_tokens = s.input_tokens;
                                output_tokens = s.output_tokens;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        if content.is_empty() {
            // Fallback: use raw stdout
            content = stdout.trim().to_string();
        }

        Ok(RuntimeResponse {
            content,
            input_tokens,
            output_tokens,
            cache_read_tokens: 0,
            model_used: context.model.clone(),
            runtime_name: "gemini".to_string(),
        })
    }

    async fn is_available(&self) -> bool {
        tokio::process::Command::new(&self.gemini_path)
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

impl GeminiRuntime {
    /// Execute and return chunks. Gemini CLI does not support true streaming,
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

impl GeminiRuntime {
    /// Write MCP server configuration to Gemini settings.
    ///
    /// If `agent_dir` is provided, writes to `agent_dir/.gemini/settings.json` for
    /// per-agent isolation. Otherwise writes to the global `~/.gemini/settings.json`.
    pub async fn write_mcp_config(agent_dir: Option<&std::path::Path>, servers: &std::collections::HashMap<String, serde_json::Value>) -> Result<(), String> {
        let settings_path = if let Some(dir) = agent_dir {
            dir.join(".gemini").join("settings.json")
        } else {
            dirs::home_dir()
                .ok_or("No home dir")?
                .join(".gemini").join("settings.json")
        };
        if let Some(parent) = settings_path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| e.to_string())?;
        }
        let existing = tokio::fs::read_to_string(&settings_path).await.unwrap_or_else(|_| "{}".to_string());
        let mut settings: serde_json::Value = serde_json::from_str(&existing).unwrap_or(serde_json::json!({}));
        settings["mcpServers"] = serde_json::Value::Object(
            servers.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
        );
        tokio::fs::write(&settings_path, serde_json::to_string_pretty(&settings).unwrap_or_default())
            .await
            .map_err(|e| e.to_string())
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_gemini_result_event() {
        let line = r#"{"type":"result","content":"Hello from Gemini","stats":{"inputTokens":200,"outputTokens":80}}"#;
        let event: GeminiEvent = serde_json::from_str(line).unwrap();
        assert_eq!(event.event_type, "result");
        let content = event.extra.get("content").unwrap().as_str().unwrap();
        assert_eq!(content, "Hello from Gemini");
    }

    #[test]
    fn test_parse_gemini_message_event() {
        let line = r#"{"type":"message","role":"assistant","content":"Thinking..."}"#;
        let event: GeminiEvent = serde_json::from_str(line).unwrap();
        assert_eq!(event.event_type, "message");
        assert_eq!(event.extra.get("role").unwrap().as_str().unwrap(), "assistant");
    }
}
