//! LLMLingua-2 prompt compression — Python subprocess bridge.
//!
//! Uses JSON stdin protocol to avoid code injection.
//! Requires: `pip install llmlingua`

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;

use crate::error::{InferenceError, Result};

use super::CompressionStats;

/// LLMLingua configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LlmLinguaConfig {
    pub enabled: bool,
    pub target_ratio: f32,
    pub model: String,
    pub python: String,
    pub force_tokens: bool,
    pub min_length: usize,
}

impl Default for LlmLinguaConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            target_ratio: 0.5,
            model: "microsoft/llmlingua-2-bert-base-multilingual-cased-meetingbank".to_string(),
            python: "python3".to_string(),
            force_tokens: true,
            min_length: 500,
        }
    }
}

/// LLMLingua-2 prompt compressor using JSON stdin protocol.
pub struct LlmLinguaCompressor {
    config: LlmLinguaConfig,
    available_cache: OnceCell<bool>,
}

impl LlmLinguaCompressor {
    pub fn new(config: LlmLinguaConfig) -> Self {
        Self {
            config,
            available_cache: OnceCell::new(),
        }
    }

    fn validate_python(python: &str) -> bool {
        matches!(python, "python3" | "python") || {
            let p = std::path::Path::new(python);
            p.is_absolute() && matches!(
                p.file_name().and_then(|n| n.to_str()),
                Some("python3" | "python")
            )
        }
    }

    /// Check if LLMLingua is available (cached).
    pub async fn is_available(&self) -> bool {
        *self.available_cache.get_or_init(|| async {
            if !self.config.enabled {
                return false;
            }
            if !Self::validate_python(&self.config.python) {
                return false;
            }
            let output = tokio::process::Command::new(&self.config.python)
                .args(["-c", "from llmlingua import PromptCompressor; print('ok')"])
                .output()
                .await;
            matches!(output, Ok(o) if o.status.success())
        }).await
    }

    /// Compress a prompt using LLMLingua-2 via JSON stdin protocol.
    pub async fn compress(&self, text: &str) -> Result<(String, CompressionStats)> {
        if text.len() < self.config.min_length {
            return Ok((
                text.to_string(),
                CompressionStats::new(text.len(), text.len(), "llmlingua-2", false),
            ));
        }

        if !Self::validate_python(&self.config.python) {
            return Err(InferenceError::Config(format!(
                "Invalid python path: {}", self.config.python
            )));
        }

        let payload = serde_json::json!({
            "model": self.config.model,
            "text": text,
            "ratio": self.config.target_ratio,
            "force_tokens": self.config.force_tokens,
        });

        let mut child = tokio::process::Command::new(&self.config.python)
            .args(["-c", LLMLINGUA_COMPRESS_SCRIPT])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| InferenceError::GenerationFailed(format!("LLMLingua spawn error: {e}")))?;

        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            let _ = stdin.write_all(payload.to_string().as_bytes()).await;
            drop(stdin);
        }

        let output = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            child.wait_with_output(),
        )
        .await
        .map_err(|_| InferenceError::GenerationFailed("LLMLingua timeout (60s)".to_string()))?
        .map_err(|e| InferenceError::GenerationFailed(format!("LLMLingua wait error: {e}")))?;

        if !output.status.success() {
            let stderr: String = String::from_utf8_lossy(&output.stderr).chars().take(300).collect();
            return Err(InferenceError::GenerationFailed(
                format!("LLMLingua error: {stderr}"),
            ));
        }

        let compressed = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if compressed.is_empty() {
            return Err(InferenceError::GenerationFailed(
                "Empty LLMLingua output".to_string(),
            ));
        }

        let stats = CompressionStats::new(text.len(), compressed.len(), "llmlingua-2", false);
        Ok((compressed, stats))
    }

    /// Compress session history — applies compression only to older messages.
    pub async fn compress_session_history(
        &self,
        messages: &[(String, String)],
        keep_recent: usize,
    ) -> Result<Vec<(String, String)>> {
        if messages.len() <= keep_recent {
            return Ok(messages.to_vec());
        }

        let split_at = messages.len() - keep_recent;
        let old_messages = &messages[..split_at];
        let recent_messages = &messages[split_at..];

        let old_text: String = old_messages
            .iter()
            .map(|(role, content)| format!("[{role}]: {content}"))
            .collect::<Vec<_>>()
            .join("\n\n");

        let (compressed, stats) = self.compress(&old_text).await?;
        tracing::info!(
            original = stats.original_len,
            compressed = stats.compressed_len,
            ratio = format!("{:.2}x", stats.ratio),
            "Session history compressed"
        );

        let mut result = vec![("system".to_string(), format!("[Compressed history]\n{compressed}"))];
        result.extend(recent_messages.to_vec());
        Ok(result)
    }
}

/// Python script that reads JSON from stdin — no string interpolation.
const LLMLINGUA_COMPRESS_SCRIPT: &str = r#"
import sys, json
data = json.load(sys.stdin)

from llmlingua import PromptCompressor

compressor = PromptCompressor(
    model_name=data["model"],
    use_llmlingua2=True,
)

result = compressor.compress_prompt(
    context=[data["text"]],
    rate=data.get("ratio", 0.5),
    force_tokens=[],
    force_reserve_digit=data.get("force_tokens", True),
)

print(result["compressed_prompt"])
"#;
