//! OpenAI-compatible API runtime — works with MiniMax, DeepSeek, OpenRouter, etc.
//!
//! Uses the standard `/v1/chat/completions` endpoint with SSE streaming.
//! MiniMax-specific notes:
//!   - base_url: https://api.minimax.io/v1
//!   - Ignores `presence_penalty`, `frequency_penalty`, `logit_bias`
//!   - Supports MiniMax-M2.7, MiniMax-M2.5, etc.

use async_trait::async_trait;
use duduclaw_core::truncate_bytes;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tracing::info;

use super::{AgentRuntime, RuntimeContext, RuntimeResponse};

/// Runtime that calls any OpenAI-compatible chat completions API.
pub struct OpenAiCompatRuntime;

impl OpenAiCompatRuntime {
    pub fn new() -> Self {
        Self
    }
}

// ── API types ───────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<ChatMessage>,
    max_tokens: u32,
    stream: bool,
}

#[derive(Debug, Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<Choice>,
    usage: Option<CompletionUsage>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: Option<ChoiceMessage>,
}

#[derive(Debug, Deserialize)]
struct ChoiceMessage {
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CompletionUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
}

#[derive(Debug, Deserialize)]
struct ApiErrorResponse {
    error: Option<ApiErrorDetail>,
}

#[derive(Debug, Deserialize)]
struct ApiErrorDetail {
    message: String,
}

// ── Shared HTTP client ──────────────────────────────────────────

static HTTP_CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();

fn http_client() -> &'static reqwest::Client {
    HTTP_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("Failed to build HTTP client")
    })
}

// ── AgentRuntime impl ───────────────────────────────────────────

#[async_trait]
impl AgentRuntime for OpenAiCompatRuntime {
    fn name(&self) -> &str {
        "openai_compat"
    }

    async fn execute(
        &self,
        prompt: &str,
        context: &RuntimeContext,
    ) -> Result<RuntimeResponse, String> {
        // Look up provider config from config.toml accounts, honouring agent's preferred_provider
        let (api_key, base_url) = resolve_provider_config(&context.home_dir, &context.agent_id, context.preferred_provider.as_deref()).await?;

        info!(
            agent = %context.agent_id,
            model = %context.model,
            base_url = %base_url,
            "OpenAiCompatRuntime: calling chat/completions"
        );

        let client = http_client();

        let messages = vec![
            ChatMessage {
                role: "system".to_string(),
                content: context.system_prompt.clone(),
            },
            ChatMessage {
                role: "user".to_string(),
                content: prompt.to_string(),
            },
        ];

        let body = ChatCompletionRequest {
            model: context.model.clone(),
            messages,
            max_tokens: context.max_tokens,
            stream: false,
        };

        let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
        let response = client
            .post(&url)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("HTTP request to {url} failed: {e}"))?;

        let status = response.status();
        let response_text = response
            .text()
            .await
            .map_err(|e| format!("Failed to read response body: {e}"))?;

        if !status.is_success() {
            if let Ok(err) = serde_json::from_str::<ApiErrorResponse>(&response_text) {
                if let Some(detail) = err.error {
                    return Err(format!("API error ({status}): {}", detail.message));
                }
            }
            return Err(format!(
                "API error ({status}): {}",
                truncate_bytes(&response_text, 300)
            ));
        }

        let parsed: ChatCompletionResponse = serde_json::from_str(&response_text)
            .map_err(|e| format!("Failed to parse response: {e}"))?;

        let content = parsed
            .choices
            .first()
            .and_then(|c| c.message.as_ref())
            .and_then(|m| m.content.clone())
            .unwrap_or_default();

        let (input_tokens, output_tokens) = parsed
            .usage
            .map(|u| (u.prompt_tokens, u.completion_tokens))
            .unwrap_or((0, 0));

        Ok(RuntimeResponse {
            content,
            input_tokens,
            output_tokens,
            cache_read_tokens: 0,
            model_used: context.model.clone(),
            runtime_name: "openai_compat".to_string(),
        })
    }

    async fn is_available(&self) -> bool {
        // Check if any OpenAI-compatible provider API key is configured
        const PROVIDER_KEYS: &[&str] = &[
            "OPENAI_API_KEY", "DEEPSEEK_API_KEY", "MINIMAX_API_KEY",
            "GROQ_API_KEY", "TOGETHER_API_KEY", "MISTRAL_API_KEY",
            "OPENROUTER_API_KEY",
        ];
        PROVIDER_KEYS.iter().any(|k| std::env::var(k).is_ok())
    }
}

// ── Provider config resolution ──────────────────────────────────

/// Known provider presets.
pub struct ProviderPreset {
    pub name: &'static str,
    pub base_url: &'static str,
    pub default_model: &'static str,
}

/// Built-in provider presets.
pub const PROVIDERS: &[ProviderPreset] = &[
    ProviderPreset {
        name: "minimax",
        base_url: "https://api.minimax.io/v1",
        default_model: "MiniMax-M2.7",
    },
    ProviderPreset {
        name: "deepseek",
        base_url: "https://api.deepseek.com/v1",
        default_model: "deepseek-chat",
    },
    ProviderPreset {
        name: "openrouter",
        base_url: "https://openrouter.ai/api/v1",
        default_model: "anthropic/claude-sonnet-4",
    },
    ProviderPreset {
        name: "groq",
        base_url: "https://api.groq.com/openai/v1",
        default_model: "llama-3.3-70b-versatile",
    },
    ProviderPreset {
        name: "openai",
        base_url: "https://api.openai.com/v1",
        default_model: "gpt-4o",
    },
];

/// Resolve API key and base URL for an OpenAI-compatible provider.
///
/// Lookup order:
/// 1. If `preferred_provider` is set, try that provider's env var first.
/// 2. Environment variable: `{PROVIDER}_API_KEY` (e.g., MINIMAX_API_KEY, DEEPSEEK_API_KEY)
/// 3. Generic `OPENAI_API_KEY` with provider-specific base_url
/// 4. Config file accounts with `provider = "..."` and `base_url = "..."`
async fn resolve_provider_config(
    home_dir: &std::path::Path,
    _agent_id: &str,
    preferred_provider: Option<&str>,
) -> Result<(String, String), String> {
    // 1. If agent specifies a provider, try that first
    if let Some(provider_name) = preferred_provider {
        if let Some(provider) = PROVIDERS.iter().find(|p| p.name == provider_name) {
            let env_key = format!("{}_API_KEY", provider.name.to_uppercase());
            if let Ok(key) = std::env::var(&env_key) {
                if !key.is_empty() {
                    return Ok((key, provider.base_url.to_string()));
                }
            }
        }
    }

    // 2. Check provider-specific env vars
    for provider in PROVIDERS {
        let env_key = format!("{}_API_KEY", provider.name.to_uppercase());
        if let Ok(key) = std::env::var(&env_key) {
            if !key.is_empty() {
                return Ok((key, provider.base_url.to_string()));
            }
        }
    }

    // Fallback to OPENAI_API_KEY with default OpenAI base
    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        if !key.is_empty() {
            return Ok((key, "https://api.openai.com/v1".to_string()));
        }
    }

    // Try reading from config.toml
    let config_path = home_dir.join("config.toml");
    if let Ok(content) = tokio::fs::read_to_string(&config_path).await {
        if let Ok(table) = content.parse::<toml::Table>() {
            if let Some(accounts) = table.get("accounts").and_then(|a| a.as_array()) {
                for acc in accounts {
                    let provider = acc.get("provider").and_then(|p| p.as_str()).unwrap_or("");
                    let base_url = acc.get("base_url").and_then(|u| u.as_str());

                    if !provider.is_empty() {
                        // Try encrypted field first (api_key_enc), fall back to plaintext api_key
                        let api_key_opt: Option<String> = acc
                            .get("api_key_enc")
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty())
                            .and_then(|enc_val| {
                                let key = crate::config_crypto::load_keyfile_public(home_dir)?;
                                let engine = duduclaw_security::crypto::CryptoEngine::new(&key).ok()?;
                                engine.decrypt_string(enc_val).ok()
                            });

                        // Fall back to plaintext api_key with a warning
                        let api_key_opt = api_key_opt.or_else(|| {
                            let plain = acc.get("api_key").and_then(|k| k.as_str())?;
                            if plain.is_empty() { return None; }
                            tracing::warn!(
                                provider,
                                "OpenAI-compat account uses plaintext api_key; \
                                 migrate to api_key_enc for better security"
                            );
                            Some(plain.to_string())
                        });

                        if let Some(key) = api_key_opt {
                            let url = base_url
                                .map(|u| u.to_string())
                                .or_else(|| {
                                    PROVIDERS.iter()
                                        .find(|p| p.name == provider)
                                        .map(|p| p.base_url.to_string())
                                })
                                .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
                            if !url.starts_with("https://") && !url.starts_with("http://") {
                                return Err(format!(
                                    "Invalid base_url scheme: {url}. Must use http:// or https://"
                                ));
                            }
                            return Ok((key, url));
                        }
                    }
                }
            }
        }
    }

    Err("No OpenAI-compatible API key found. Set MINIMAX_API_KEY, DEEPSEEK_API_KEY, or OPENAI_API_KEY".to_string())
}

// ── SSE streaming ───────────────────────────────────────────────

impl OpenAiCompatRuntime {
    /// Execute with SSE streaming — parses `data: {"choices":[{"delta":{"content":"..."}}]}` events.
    pub async fn execute_sse_streaming(
        &self,
        prompt: &str,
        context: &super::RuntimeContext,
    ) -> Result<super::RuntimeResponse, String> {
        let (api_key, base_url) = resolve_provider_config(&context.home_dir, &context.agent_id, context.preferred_provider.as_deref()).await?;
        let client = http_client();
        let messages = vec![
            ChatMessage { role: "system".to_string(), content: context.system_prompt.clone() },
            ChatMessage { role: "user".to_string(), content: prompt.to_string() },
        ];
        let body = ChatCompletionRequest {
            model: context.model.clone(),
            messages,
            max_tokens: context.max_tokens,
            stream: true,
        };
        let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
        let response = client.post(&url)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .json(&body)
            .send().await
            .map_err(|e| format!("SSE request failed: {e}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(format!("SSE error ({status}): {}", body.chars().take(300).collect::<String>()));
        }

        // Stream SSE chunks instead of buffering the entire response
        let mut stream = response.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        let mut content = String::new();
        let mut input_tokens: u64 = 0;
        let mut output_tokens: u64 = 0;
        let mut done = false;

        while let Some(chunk) = stream.next().await {
            if done { break; }
            let chunk = chunk.map_err(|e| format!("SSE stream error: {e}"))?;
            buf.extend_from_slice(&chunk);

            // Process all complete lines available in the buffer
            while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                let line_bytes = buf.drain(..pos + 1).collect::<Vec<u8>>();
                let line = String::from_utf8_lossy(&line_bytes);
                let line = line.trim();

                if line.is_empty() || line.starts_with(':') {
                    continue;
                }

                if let Some(json_str) = line.strip_prefix("data: ") {
                    if json_str == "[DONE]" {
                        done = true;
                        break;
                    }
                    if let Ok(chunk) = serde_json::from_str::<serde_json::Value>(json_str) {
                        if let Some(delta) = chunk.pointer("/choices/0/delta/content").and_then(|v| v.as_str()) {
                            content.push_str(delta);
                        }
                        if let Some(usage) = chunk.get("usage") {
                            input_tokens = usage.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(input_tokens);
                            output_tokens = usage.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(output_tokens);
                        }
                    }
                }
            }
        }

        // Process any remaining data in the buffer (line without trailing newline)
        if !buf.is_empty() {
            let line = String::from_utf8_lossy(&buf);
            let line = line.trim();
            if let Some(json_str) = line.strip_prefix("data: ") {
                if json_str != "[DONE]" {
                    if let Ok(chunk) = serde_json::from_str::<serde_json::Value>(json_str) {
                        if let Some(delta) = chunk.pointer("/choices/0/delta/content").and_then(|v| v.as_str()) {
                            content.push_str(delta);
                        }
                        if let Some(usage) = chunk.get("usage") {
                            input_tokens = usage.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(input_tokens);
                            output_tokens = usage.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(output_tokens);
                        }
                    }
                }
            }
        }

        Ok(super::RuntimeResponse {
            content,
            input_tokens,
            output_tokens,
            cache_read_tokens: 0,
            model_used: context.model.clone(),
            runtime_name: "openai_compat_sse".to_string(),
        })
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_presets() {
        assert_eq!(PROVIDERS.len(), 5);
        let minimax = PROVIDERS.iter().find(|p| p.name == "minimax").unwrap();
        assert_eq!(minimax.base_url, "https://api.minimax.io/v1");
        assert_eq!(minimax.default_model, "MiniMax-M2.7");
    }

    #[test]
    fn test_parse_completion_response() {
        let json = r#"{"choices":[{"message":{"role":"assistant","content":"Hello!"}}],"usage":{"prompt_tokens":10,"completion_tokens":5}}"#;
        let resp: ChatCompletionResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.choices[0].message.as_ref().unwrap().content.as_deref().unwrap(), "Hello!");
        assert_eq!(resp.usage.unwrap().prompt_tokens, 10);
    }
}
