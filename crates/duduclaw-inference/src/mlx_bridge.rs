//! MLX bridge — calls Python `mlx_lm` for local inference on Apple Silicon.
//!
//! Uses JSON stdin protocol to avoid code injection.
//! Only available on macOS aarch64 (Apple Silicon).

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;

use crate::error::{InferenceError, Result};

/// MLX bridge configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MlxConfig {
    pub enabled: bool,
    pub model: String,
    pub adapter_path: Option<String>,
    pub max_tokens: u32,
    pub temperature: f32,
    pub python: String,
}

impl Default for MlxConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model: "mlx-community/Qwen2.5-7B-Instruct-4bit".to_string(),
            adapter_path: None,
            max_tokens: 1024,
            temperature: 0.7,
            python: "python3".to_string(),
        }
    }
}

/// MLX bridge — calls mlx_lm via Python subprocess with JSON stdin protocol.
pub struct MlxBridge {
    config: MlxConfig,
    available_cache: OnceCell<bool>,
}

impl MlxBridge {
    pub fn new(config: MlxConfig) -> Self {
        Self {
            config,
            available_cache: OnceCell::new(),
        }
    }

    /// Validate model/adapter path to prevent path traversal attacks.
    fn is_safe_model_path(path: &str) -> bool {
        !path.contains("..") && !path.starts_with('/') && !path.contains('\0')
    }

    /// Validate the python executable path (must be python/python3 basename).
    fn validate_python(python: &str) -> bool {
        matches!(python, "python3" | "python") || {
            let p = std::path::Path::new(python);
            p.is_absolute() && matches!(
                p.file_name().and_then(|n| n.to_str()),
                Some("python3" | "python")
            )
        }
    }

    /// Check if MLX is available on this system (cached).
    pub async fn is_available(&self) -> bool {
        *self.available_cache.get_or_init(|| async {
            if !cfg!(all(target_os = "macos", target_arch = "aarch64")) {
                return false;
            }
            if !self.config.enabled {
                return false;
            }
            if !Self::validate_python(&self.config.python) {
                return false;
            }
            let output = tokio::process::Command::new(&self.config.python)
                .args(["-c", "import mlx_lm; print('ok')"])
                .output()
                .await;
            matches!(output, Ok(o) if o.status.success())
        }).await
    }

    /// Generate text using MLX locally via JSON stdin protocol.
    pub async fn generate(
        &self,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<String> {
        if !Self::validate_python(&self.config.python) {
            return Err(InferenceError::Config(format!(
                "Invalid python path: {}. Must be 'python3', 'python', or absolute path ending in python/python3",
                self.config.python
            )));
        }

        if !Self::is_safe_model_path(&self.config.model) {
            return Err(InferenceError::GenerationFailed("Invalid model path".to_string()));
        }
        if let Some(ref adapter) = self.config.adapter_path
            && !Self::is_safe_model_path(adapter) {
                return Err(InferenceError::GenerationFailed("Invalid adapter path".to_string()));
            }

        let payload = serde_json::json!({
            "model": self.config.model,
            "adapter_path": self.config.adapter_path,
            "system_prompt": system_prompt,
            "user_prompt": user_prompt,
            "max_tokens": self.config.max_tokens,
            "temperature": self.config.temperature,
        });

        let script = MLX_GENERATE_SCRIPT;

        let mut child = tokio::process::Command::new(&self.config.python)
            .args(["-c", script])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| InferenceError::GenerationFailed(format!("MLX spawn error: {e}")))?;

        // Write JSON payload to stdin
        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            let _ = stdin.write_all(payload.to_string().as_bytes()).await;
            drop(stdin);
        }

        let output = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            child.wait_with_output(),
        )
        .await
        .map_err(|_| InferenceError::GenerationFailed("MLX generation timeout (120s)".to_string()))?
        .map_err(|e| InferenceError::GenerationFailed(format!("MLX wait error: {e}")))?;

        if !output.status.success() {
            let stderr: String = String::from_utf8_lossy(&output.stderr).chars().take(300).collect();
            return Err(InferenceError::GenerationFailed(
                format!("MLX exit {}: {stderr}", output.status.code().unwrap_or(-1))
            ));
        }

        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if stdout.is_empty() {
            return Err(InferenceError::GenerationFailed("Empty MLX response".to_string()));
        }

        Ok(stdout)
    }

    /// Run an evolution reflection using local MLX model.
    pub async fn run_evolution_reflection(
        &self,
        reflection_type: &str,
        agent_soul: &str,
        context: &str,
    ) -> Result<String> {
        let system_prompt = format!(
            "You are an evolution engine performing a {reflection_type} reflection.\n\
             Your task is to analyze the agent's recent behavior and suggest improvements.\n\n\
             # Agent Soul\n{agent_soul}"
        );

        let user_prompt = format!(
            "Perform a {reflection_type} reflection on the following context:\n\n{context}\n\n\
             Respond in JSON format with keys: status, insights, suggestions"
        );

        self.generate(&system_prompt, &user_prompt).await
    }
}

/// Python script that reads JSON from stdin — no string interpolation.
const MLX_GENERATE_SCRIPT: &str = r#"
import sys, json
data = json.load(sys.stdin)

from mlx_lm import load, generate

kwargs = {}
if data.get("adapter_path"):
    kwargs["adapter_path"] = data["adapter_path"]

model, tokenizer = load(data["model"], **kwargs)

messages = []
if data.get("system_prompt"):
    messages.append({"role": "system", "content": data["system_prompt"]})
messages.append({"role": "user", "content": data["user_prompt"]})

prompt = tokenizer.apply_chat_template(messages, tokenize=False, add_generation_prompt=True)

response = generate(
    model,
    tokenizer,
    prompt=prompt,
    max_tokens=data.get("max_tokens", 1024),
    temp=data.get("temperature", 0.7),
    verbose=False,
)

print(response)
"#;
