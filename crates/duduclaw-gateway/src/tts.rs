//! Text-to-Speech (TTS) support — MiniMax T2A API.
//!
//! MiniMax Speech API: POST https://api.minimax.io/v1/t2a_v2
//! Returns audio bytes (MP3 by default).

use duduclaw_core::truncate_bytes;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use tracing::{info, warn};
use zeroize::Zeroize;

static HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

fn http_client() -> &'static reqwest::Client {
    HTTP_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .unwrap_or_default()
    })
}

// ── Provider trait ──────────────────────────────────────────────

/// Abstract TTS provider.
#[async_trait::async_trait]
pub trait TtsProvider: Send + Sync {
    fn name(&self) -> &str;
    async fn synthesize(&self, text: &str, voice: &str) -> Result<Vec<u8>, String>;
}

// ── MiniMax T2A ─────────────────────────────────────────────────

const MINIMAX_T2A_URL: &str = "https://api.minimax.io/v1/t2a_v2";

/// MiniMax T2A (Text-to-Audio) provider.
pub struct MiniMaxTts {
    api_key: String,
}

impl Drop for MiniMaxTts {
    fn drop(&mut self) {
        self.api_key.zeroize();
    }
}

impl MiniMaxTts {
    pub fn new(api_key: String) -> Self {
        Self { api_key }
    }

    /// Create from environment variable.
    pub fn from_env() -> Option<Self> {
        let key = std::env::var("MINIMAX_API_KEY").ok()?;
        if key.is_empty() {
            return None;
        }
        Some(Self::new(key))
    }

    /// Auto-detect language from text content (CJK vs Latin).
    fn detect_voice(text: &str) -> &'static str {
        let cjk_count = text.chars().filter(|c| *c > '\u{2E80}').count();
        let total = text.chars().count().max(1);
        if cjk_count as f64 / total as f64 > 0.3 {
            "Cute_Girl" // Chinese voice
        } else {
            "English_Female_1" // English voice
        }
    }
}

#[derive(Debug, Serialize)]
struct T2aRequest {
    text: String,
    voice_id: String,
    model: String,
    speed: f32,
    output_format: String,
}

#[derive(Debug, Deserialize)]
struct T2aResponse {
    audio_file: Option<String>, // base64-encoded audio
    #[serde(default)]
    base_resp: Option<T2aBaseResp>,
}

#[derive(Debug, Deserialize)]
struct T2aBaseResp {
    status_code: i32,
    status_msg: String,
}

#[async_trait::async_trait]
impl TtsProvider for MiniMaxTts {
    fn name(&self) -> &str {
        "minimax"
    }

    async fn synthesize(&self, text: &str, voice: &str) -> Result<Vec<u8>, String> {
        let voice_id = if voice.is_empty() {
            Self::detect_voice(text).to_string()
        } else {
            voice.to_string()
        };

        info!(voice = %voice_id, text_len = text.len(), "MiniMax TTS: synthesizing");

        let body = T2aRequest {
            text: text.to_string(),
            voice_id,
            model: "speech-02-hd".to_string(),
            speed: 1.0,
            output_format: "mp3".to_string(),
        };

        let response = http_client()
            .post(MINIMAX_T2A_URL)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("MiniMax TTS request failed: {e}"))?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(format!(
                "MiniMax TTS error ({status}): {}",
                truncate_bytes(&text, 300)
            ));
        }

        let resp: T2aResponse = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse TTS response: {e}"))?;

        // Check for API-level errors
        if let Some(base) = &resp.base_resp {
            if base.status_code != 0 {
                return Err(format!("MiniMax TTS error: {}", base.status_msg));
            }
        }

        let audio_b64 = resp
            .audio_file
            .ok_or_else(|| "MiniMax TTS returned no audio".to_string())?;

        use base64::Engine;
        let audio_bytes = base64::engine::general_purpose::STANDARD
            .decode(&audio_b64)
            .map_err(|e| format!("Failed to decode audio base64: {e}"))?;

        info!(bytes = audio_bytes.len(), "MiniMax TTS: synthesis complete");
        Ok(audio_bytes)
    }
}

// ── edge-tts (Microsoft Edge TTS, free) ─────────────────────────

/// Microsoft Edge TTS provider — free, high-quality Neural TTS.
///
/// Invokes the separately installed `edge-tts` CLI (`pipx install edge-tts`).
/// Its unofficial Microsoft endpoint can stop working; update with `pipx upgrade edge-tts`.
///
/// Default voices:
/// - zh-TW: `zh-TW-HsiaoChenNeural` (female) / `zh-TW-YunJheNeural` (male)
/// - en-US: `en-US-AriaNeural` (female)
pub struct EdgeTtsProvider {
    default_voice_zh: String,
    default_voice_en: String,
    #[cfg(test)]
    binary_path: Option<std::path::PathBuf>,
}

impl EdgeTtsProvider {
    pub fn new() -> Self {
        Self {
            default_voice_zh: "zh-TW-HsiaoChenNeural".into(),
            default_voice_en: "en-US-AriaNeural".into(),
            #[cfg(test)]
            binary_path: None,
        }
    }

    pub fn with_voices(zh_voice: String, en_voice: String) -> Self {
        Self {
            default_voice_zh: zh_voice,
            default_voice_en: en_voice,
            #[cfg(test)]
            binary_path: None,
        }
    }

    fn detect_voice(&self, text: &str) -> &str {
        let cjk_count = text.chars().filter(|c| *c > '\u{2E80}').count();
        let total = text.chars().count().max(1);
        if cjk_count as f64 / total as f64 > 0.3 {
            &self.default_voice_zh
        } else {
            &self.default_voice_en
        }
    }

    /// Discover the separately installed CLI, without contacting the service.
    pub fn is_installed() -> bool {
        Self::binary().is_some()
    }

    fn binary() -> Option<String> {
        let path = duduclaw_core::which_cli("edge-tts")?;
        let metadata = std::fs::metadata(&path).ok()?;
        if !metadata.is_file() { return None; }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o111 == 0 { return None; }
        }
        Some(path)
    }

    fn valid_voice(voice: &str) -> bool {
        !voice.is_empty() && voice.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
    }
}

#[async_trait::async_trait]
impl TtsProvider for EdgeTtsProvider {
    fn name(&self) -> &str {
        "edge-tts"
    }

    async fn synthesize(&self, text: &str, voice: &str) -> Result<Vec<u8>, String> {
        let voice = if voice.is_empty() { self.detect_voice(text) } else { voice };
        if !Self::valid_voice(voice) {
            return Err("Invalid edge-tts voice".into());
        }
        #[cfg(test)]
        let binary = self.binary_path.clone().or_else(|| Self::binary().map(Into::into));
        #[cfg(not(test))]
        let binary = Self::binary().map(std::path::PathBuf::from);
        let binary = binary.ok_or("edge-tts is not installed. Install: pipx install edge-tts")?;

        // TempDir owns cleanup on success, errors, timeout and cancellation.
        let dir = tempfile::Builder::new().prefix("duduclaw-tts-").tempdir()
            .map_err(|e| format!("edge-tts temporary directory: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
                .map_err(|e| format!("edge-tts directory permissions: {e}"))?;
        }
        let input = dir.path().join("input.txt");
        let output = dir.path().join("output.mp3");
        tokio::fs::write(&input, text).await.map_err(|e| format!("edge-tts input: {e}"))?;
        let mut cmd = tokio::process::Command::new(binary);
        cmd.env_clear();
        for key in ["PATH", "HOME", "USERPROFILE", "TEMP", "TMP", "SYSTEMROOT"] {
            if let Some(value) = std::env::var_os(key) { cmd.env(key, value); }
        }
        duduclaw_core::ns_guard::guard_command(&mut cmd);
        cmd.arg("--file").arg(&input).arg("--voice").arg(voice)
            .arg("--write-media").arg(&output)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let result = tokio::time::timeout(std::time::Duration::from_secs(30), cmd.output())
            .await.map_err(|_| "edge-tts synthesis timeout (30s)".to_string())?
            .map_err(|e| format!("edge-tts execution failed: {e}"))?;
        if !result.status.success() {
            return Err(format!("edge-tts failed ({}): {}", result.status,
                truncate_bytes(&String::from_utf8_lossy(&result.stderr), 300)));
        }
        let audio = tokio::fs::read(&output).await.map_err(|e| format!("edge-tts output: {e}"))?;
        if audio.is_empty() { return Err("edge-tts returned no audio data".into()); }
        Ok(audio)
    }
}

// ── OpenAI TTS ──────────────────────────────────────────────────

/// OpenAI TTS API provider.
pub struct OpenAiTtsProvider {
    api_key: String,
    model: String,
}

impl Drop for OpenAiTtsProvider {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.api_key.zeroize();
    }
}

impl OpenAiTtsProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            model: "tts-1".into(),
        }
    }

    pub fn from_env() -> Option<Self> {
        let key = std::env::var("OPENAI_API_KEY").ok()?;
        if key.is_empty() {
            return None;
        }
        Some(Self::new(key))
    }

    pub fn with_model(mut self, model: &str) -> Self {
        self.model = model.into();
        self
    }

    fn detect_voice(text: &str) -> &'static str {
        let cjk_count = text.chars().filter(|c| *c > '\u{2E80}').count();
        let total = text.chars().count().max(1);
        if cjk_count as f64 / total as f64 > 0.3 {
            "nova"
        } else {
            "alloy"
        }
    }
}

#[async_trait::async_trait]
impl TtsProvider for OpenAiTtsProvider {
    fn name(&self) -> &str {
        "openai-tts"
    }

    async fn synthesize(&self, text: &str, voice: &str) -> Result<Vec<u8>, String> {
        let voice_name = if voice.is_empty() {
            Self::detect_voice(text)
        } else {
            voice
        };
        info!(voice = voice_name, model = %self.model, "OpenAI TTS: synthesizing");

        let body = serde_json::json!({
            "model": self.model,
            "input": text,
            "voice": voice_name,
            "response_format": "mp3",
        });

        let resp = http_client()
            .post("https://api.openai.com/v1/audio/speech")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("OpenAI TTS: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!(
                "OpenAI TTS error ({status}): {}",
                truncate_bytes(&text, 200)
            ));
        }

        let audio = resp
            .bytes()
            .await
            .map_err(|e| format!("OpenAI TTS read: {e}"))?;
        info!(bytes = audio.len(), "OpenAI TTS: synthesis complete");
        Ok(audio.to_vec())
    }
}

// ── Piper TTS (local ONNX, CPU-only) ───────────────────────────

/// Piper TTS provider — lightweight local TTS via ONNX Runtime.
///
/// Model files are stored in `~/.duduclaw/models/piper/`.
/// Requires the `onnx` feature in `duduclaw-inference`.
///
/// This is a stub that invokes piper as a subprocess for now.
/// Full ONNX integration planned when piper-rs crate stabilizes.
pub struct PiperTtsProvider {
    model_path: String,
    config_path: String,
}

impl PiperTtsProvider {
    pub fn new(model_path: String, config_path: String) -> Result<Self, String> {
        // Path traversal protection
        for path in [&model_path, &config_path] {
            if path.contains("..") || path.contains('\0') {
                return Err(format!("Unsafe path: {path}"));
            }
        }
        Ok(Self {
            model_path,
            config_path,
        })
    }

    /// Auto-detect from models directory.
    pub fn from_models_dir(models_dir: &std::path::Path) -> Option<Self> {
        let piper_dir = models_dir.join("piper");
        // Look for any .onnx file
        let model = std::fs::read_dir(&piper_dir)
            .ok()?
            .filter_map(|e| e.ok())
            .find(|e| e.path().extension().is_some_and(|ext| ext == "onnx"))?
            .path();
        let config = model.with_extension("onnx.json");
        if !config.exists() {
            return None;
        }
        Self::new(
            model.to_string_lossy().into(),
            config.to_string_lossy().into(),
        )
        .ok()
    }
}

#[async_trait::async_trait]
impl TtsProvider for PiperTtsProvider {
    fn name(&self) -> &str {
        "piper"
    }

    async fn synthesize(&self, text: &str, _voice: &str) -> Result<Vec<u8>, String> {
        info!(model = %self.model_path, text_len = text.len(), "Piper TTS: synthesizing");

        // Use piper subprocess (cross-platform, no Rust binding needed)
        let mut cmd = tokio::process::Command::new("piper");
        duduclaw_core::ns_guard::guard_command(&mut cmd);
        let mut child = cmd
            .args([
                "--model",
                &self.model_path,
                "--config",
                &self.config_path,
                "--output-raw",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("Piper not found: {e}. Install: pip install piper-tts"))?;

        // Write text to stdin then close it (piper reads from stdin)
        {
            use tokio::io::AsyncWriteExt;
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| "Piper stdin unavailable".to_string())?;
            stdin
                .write_all(text.as_bytes())
                .await
                .map_err(|e| format!("Piper stdin write: {e}"))?;
            // stdin dropped here → EOF to piper
        }

        // Wait with timeout; kill child on timeout to prevent zombie processes
        // Note: child.kill() requires &mut, so we keep ownership outside the timeout.
        let wait_result = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            use tokio::io::AsyncReadExt;
            let mut stdout_data = Vec::new();
            let mut stderr_data = Vec::new();
            if let Some(mut stdout) = child.stdout.take() {
                let _ = stdout.read_to_end(&mut stdout_data).await;
            }
            if let Some(mut stderr) = child.stderr.take() {
                let _ = stderr.read_to_end(&mut stderr_data).await;
            }
            let status = child.wait().await;
            (status, stdout_data, stderr_data)
        })
        .await;

        let (status, stdout_data, stderr_data) = match wait_result {
            Ok((status, stdout, stderr)) => (status, stdout, stderr),
            Err(_) => {
                // Timeout — kill is best-effort (child handles already taken)
                tracing::warn!("Piper TTS timeout (30s) — process may linger");
                return Err("Piper TTS timeout (30s)".to_string());
            }
        };

        let status = status.map_err(|e| format!("Piper wait: {e}"))?;
        if !status.success() {
            let stderr = String::from_utf8_lossy(&stderr_data);
            return Err(format!("Piper error: {stderr}"));
        }

        // Piper outputs raw PCM i16 mono 22050Hz, convert to WAV
        let wav = pcm_i16_to_wav(&stdout_data, 22050);
        info!(bytes = wav.len(), "Piper TTS: synthesis complete");
        Ok(wav)
    }
}

/// Encode raw PCM i16 bytes as WAV.
fn pcm_i16_to_wav(pcm: &[u8], sample_rate: u32) -> Vec<u8> {
    let data_size = pcm.len() as u32;
    let file_size = 36 + data_size;
    let mut buf = Vec::with_capacity(44 + pcm.len());
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&file_size.to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes()); // PCM
    buf.extend_from_slice(&1u16.to_le_bytes()); // mono
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // byte rate
    buf.extend_from_slice(&2u16.to_le_bytes()); // block align
    buf.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_size.to_le_bytes());
    buf.extend_from_slice(pcm);
    buf
}

// ── TTS Router ──────────────────────────────────────────────────

/// TTS routing strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TtsStrategy {
    /// Local first → edge-tts → MiniMax → OpenAI.
    LocalFirst,
    /// edge-tts only (free, no API key).
    EdgeOnly,
    /// Best quality cloud provider.
    CloudBest,
    PiperOnly,
    MiniMaxOnly,
    OpenAiOnly,
}

/// TTS Router — dispatches to the best available provider.
pub struct TtsRouter {
    home_dir: Option<std::path::PathBuf>,
    providers: Vec<Box<dyn TtsProvider>>,
    default_voice: String,
}

impl TtsRouter {
    pub fn new() -> Self {
        Self {
            home_dir: None,
            providers: Vec::new(),
            default_voice: String::new(),
        }
    }

    pub fn add_provider(mut self, provider: Box<dyn TtsProvider>) -> Self {
        info!(
            provider = provider.name(),
            "TTS Router: registered provider"
        );
        self.providers.push(provider);
        self
    }

    /// Load the dashboard voice configuration once for every TTS entry point.
    pub fn for_home(home: &std::path::Path) -> Self {
        let table = match Self::load_inference_config(home) {
            Ok(table) => table,
            Err(()) => return Self::new(),
        };
        let voice = table.get("voice").and_then(|v| v.as_table());
        let setting = voice.and_then(|v| v.get("tts_provider"))
            .and_then(|v| v.as_str()).unwrap_or("").trim().to_ascii_lowercase();
        let Some(strategy) = Self::strategy_for_setting(&setting) else { return Self::new(); };
        let mut router = Self::auto_detect_for_home(home, &home.join("models"), strategy);
        router.default_voice = voice.and_then(|v| v.get("tts_voice"))
            .and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        router
    }

    fn load_inference_config(home: &std::path::Path) -> Result<toml::Table, ()> {
        let config_path = home.join("inference.toml");
        match std::fs::read_to_string(&config_path) {
            Ok(text) => match text.parse() {
                Ok(table) => Ok(table),
                Err(error) => {
                    warn!(path = %config_path.display(), error = %error, "TTS Router: inference config is malformed; disabling providers");
                    Err(())
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(toml::Table::new()),
            Err(error) => {
                warn!(path = %config_path.display(), error = %error, "TTS Router: inference config is unreadable; disabling providers");
                Err(())
            }
        }
    }

    fn strategy_for_setting(setting: &str) -> Option<TtsStrategy> {
        Some(match setting {
            "none" | "off" | "disabled" => return None,
            "edge-tts" | "edge" => TtsStrategy::EdgeOnly,
            "piper" => TtsStrategy::PiperOnly,
            "minimax" => TtsStrategy::MiniMaxOnly,
            "openai-tts" | "openai" => TtsStrategy::OpenAiOnly,
            _ => TtsStrategy::LocalFirst,
        })
    }

    /// Providers allowed by the current policy; discovery never calls the network.
    pub fn available_providers(&self) -> Vec<&str> {
        let policy = match self.home_dir.as_deref().map(crate::product_policy::load).transpose() {
            Ok(policy) => policy,
            Err(_) => return Vec::new(),
        };
        self.providers.iter().filter(|p| policy.as_ref()
            .is_none_or(|policy| policy.require_provider(p.name()).is_ok()))
            .map(|p| p.name()).collect()
    }

    pub fn is_available(&self) -> bool {
        !self.available_providers().is_empty()
    }

    /// Build a router with auto-detected providers.
    ///
    /// Provider priority depends on strategy:
    /// - LocalFirst: Piper → edge-tts → MiniMax → OpenAI
    /// - EdgeOnly: edge-tts only
    /// - CloudBest: MiniMax → OpenAI → edge-tts (paid first for quality)
    pub fn auto_detect(models_dir: &std::path::Path, strategy: TtsStrategy) -> Self {
        Self::auto_detect_for_home(&duduclaw_core::platform::duduclaw_home(), models_dir, strategy)
    }

    pub fn auto_detect_for_home(home_dir: &std::path::Path, models_dir: &std::path::Path, strategy: TtsStrategy) -> Self {
        let mut router = Self::new();
        router.home_dir = Some(home_dir.to_path_buf());
        let Ok(policy) = crate::product_policy::load(home_dir) else { return router; };

        if matches!(strategy, TtsStrategy::LocalFirst) {
            // Piper local (highest priority)
            if let Some(piper) = PiperTtsProvider::from_models_dir(models_dir) {
                router.providers.push(Box::new(piper));
            }
            // edge-tts free cloud
            if EdgeTtsProvider::is_installed() {
                router.providers.push(Box::new(EdgeTtsProvider::new()));
            }
            // Paid fallbacks
            if let Some(minimax) = MiniMaxTts::from_env() {
                router.providers.push(Box::new(minimax));
            }
            if let Some(openai) = OpenAiTtsProvider::from_env() {
                router.providers.push(Box::new(openai));
            }
        } else if matches!(strategy, TtsStrategy::CloudBest) {
            // Paid providers first (better quality)
            if let Some(minimax) = MiniMaxTts::from_env() {
                router.providers.push(Box::new(minimax));
            }
            if let Some(openai) = OpenAiTtsProvider::from_env() {
                router.providers.push(Box::new(openai));
            }
            // edge-tts as free fallback
            if EdgeTtsProvider::is_installed() {
                router.providers.push(Box::new(EdgeTtsProvider::new()));
            }
        } else if matches!(strategy, TtsStrategy::PiperOnly) {
            if let Some(piper) = PiperTtsProvider::from_models_dir(models_dir) {
                router.providers.push(Box::new(piper));
            }
        } else if matches!(strategy, TtsStrategy::MiniMaxOnly) {
            if let Some(minimax) = MiniMaxTts::from_env() { router.providers.push(Box::new(minimax)); }
        } else if matches!(strategy, TtsStrategy::OpenAiOnly) {
            if let Some(openai) = OpenAiTtsProvider::from_env() { router.providers.push(Box::new(openai)); }
        } else {
            // EdgeOnly
            if EdgeTtsProvider::is_installed() {
                router.providers.push(Box::new(EdgeTtsProvider::new()));
            }
        }

        router.providers.retain(|provider| policy.require_provider(provider.name()).is_ok());
        if router.providers.is_empty() {
            tracing::warn!("TTS Router: no providers available");
        }
        router
    }
}

#[async_trait::async_trait]
impl TtsProvider for TtsRouter {
    fn name(&self) -> &str {
        "tts-router"
    }

    async fn synthesize(&self, text: &str, voice: &str) -> Result<Vec<u8>, String> {
        let voice = if voice.is_empty() { &self.default_voice } else { voice };
        let mut last_err = None;
        for provider in &self.providers {
            if let Some(home) = self.home_dir.as_deref() {
                let policy = crate::product_policy::load(home)?;
                if policy.require_provider(provider.name()).is_err() { continue; }
            }
            match provider.synthesize(text, voice).await {
                Ok(audio) if !audio.is_empty() => {
                    info!(
                        provider = provider.name(),
                        bytes = audio.len(),
                        "TTS: synthesis succeeded"
                    );
                    return Ok(audio);
                }
                Ok(_) => {
                    tracing::warn!(
                        provider = provider.name(),
                        "TTS: returned empty audio, trying next"
                    );
                    last_err = Some("Empty audio".to_string());
                }
                Err(e) => {
                    tracing::warn!(provider = provider.name(), error = %e, "TTS: failed, trying next");
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| "No TTS providers configured".into()))
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_validation_rejects_shell_syntax() {
        assert!(!EdgeTtsProvider::valid_voice("a;b"));
        assert!(!EdgeTtsProvider::valid_voice(""));
        assert!(EdgeTtsProvider::valid_voice("zh-TW-HsiaoChenNeural"));
    }

    #[test]
    fn router_settings_map_to_the_selected_provider() {
        for (setting, strategy) in [
            ("", TtsStrategy::LocalFirst),
            ("auto", TtsStrategy::LocalFirst),
            ("edge", TtsStrategy::EdgeOnly),
            ("edge-tts", TtsStrategy::EdgeOnly),
            ("piper", TtsStrategy::PiperOnly),
            ("minimax", TtsStrategy::MiniMaxOnly),
            ("openai", TtsStrategy::OpenAiOnly),
            ("openai-tts", TtsStrategy::OpenAiOnly),
        ] {
            assert_eq!(TtsRouter::strategy_for_setting(setting), Some(strategy));
        }
        for setting in ["none", "off", "disabled"] {
            assert_eq!(TtsRouter::strategy_for_setting(setting), None);
        }
        assert!(!TtsRouter::new().is_available());
    }

    #[test]
    fn disabled_router_is_unavailable_without_environment_changes() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("inference.toml"),
            "[voice]\ntts_provider = ' OFF '\n").unwrap();
        assert!(!TtsRouter::for_home(home.path()).is_available());
    }

    #[test]
    fn missing_config_uses_default_settings_but_malformed_config_disables_tts() {
        let missing_home = tempfile::tempdir().unwrap();
        let missing_config = TtsRouter::load_inference_config(missing_home.path()).unwrap();
        assert!(missing_config.is_empty());

        let malformed_home = tempfile::tempdir().unwrap();
        std::fs::write(malformed_home.path().join("inference.toml"), "[voice\ntts_provider = 'off'").unwrap();
        assert!(TtsRouter::load_inference_config(malformed_home.path()).is_err());
        assert!(!TtsRouter::for_home(malformed_home.path()).is_available());
    }

    struct VoiceEcho;

    #[async_trait::async_trait]
    impl TtsProvider for VoiceEcho {
        fn name(&self) -> &str { "test-voice" }
        async fn synthesize(&self, _text: &str, voice: &str) -> Result<Vec<u8>, String> {
            Ok(voice.as_bytes().to_vec())
        }
    }

    #[test]
    fn availability_fails_closed_when_policy_is_unreadable() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"), "[invalid").unwrap();
        let mut router = TtsRouter::new().add_provider(Box::new(VoiceEcho));
        assert!(router.is_available());
        router.home_dir = Some(home.path().to_path_buf());
        assert!(!router.is_available());
    }

    #[tokio::test]
    async fn router_uses_default_voice_unless_overridden() {
        let mut router = TtsRouter::new().add_provider(Box::new(VoiceEcho));
        router.default_voice = "configured-voice".into();
        assert_eq!(router.synthesize("hello", "").await.unwrap(), b"configured-voice");
        assert_eq!(router.synthesize("hello", "override").await.unwrap(), b"override");
    }

    #[cfg(unix)]
    fn fake_edge(script: &str) -> (tempfile::TempDir, EdgeTtsProvider) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("edge-tts");
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut provider = EdgeTtsProvider::new();
        provider.binary_path = Some(path);
        (dir, provider)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn edge_cli_passes_text_only_by_private_file_and_returns_audio() {
        let (_dir, provider) = fake_edge(r#"
[ "$#" -eq 6 ] || exit 11
[ "$1" = '--file' ] && [ "$3" = '--voice' ] && [ "$5" = '--write-media' ] || exit 12
[ "$4" = 'zh-TW-HsiaoChenNeural' ] || exit 13
[ "$(/bin/cat "$2")" = '你好 **私密文字** $()' ] || exit 14
[ -z "${OPENAI_API_KEY}${MINIMAX_API_KEY}${DUDUCLAW_AGENT_TOKEN}" ] || exit 15
printf '%s' "$2" > "$0.input-path"
printf 'fake-mp3' > "$6"
"#);
        let audio = provider.synthesize("你好 **私密文字** $()", "").await.unwrap();
        assert_eq!(audio, b"fake-mp3");
        let marker = provider.binary_path.as_ref().unwrap().with_file_name("edge-tts.input-path");
        let input_path = std::fs::read_to_string(marker).unwrap();
        assert!(!std::path::Path::new(&input_path).exists());
        assert!(!std::path::Path::new(&input_path).parent().unwrap().exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn edge_cli_nonzero_exit_truncates_cjk_stderr_safely() {
        // 299 bytes precede a three-byte character, exercising a UTF-8 boundary.
        let stderr = format!("{}中{}", "x".repeat(299), "錯誤".repeat(200));
        let (_dir, provider) = fake_edge(&format!("printf '%s' '{}' >&2\nexit 9", stderr));
        let error = provider.synthesize("hello", "").await.unwrap_err();
        assert!(error.contains("edge-tts failed"));
        assert!(error.contains(&"x".repeat(299)));
        assert!(!error.contains("錯誤"));
        assert!(error.len() < 400);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn edge_cli_rejects_invalid_voice_before_spawning() {
        let (_dir, provider) = fake_edge("exit 99");
        assert_eq!(provider.synthesize("hello", "a;b").await.unwrap_err(), "Invalid edge-tts voice");
    }

    #[test]
    fn test_detect_voice_chinese() {
        assert_eq!(
            MiniMaxTts::detect_voice("你好世界，今天天氣怎麼樣？"),
            "Cute_Girl"
        );
    }

    #[test]
    fn test_detect_voice_english() {
        assert_eq!(
            MiniMaxTts::detect_voice("Hello world, how are you?"),
            "English_Female_1"
        );
    }

    #[test]
    fn test_detect_voice_mixed() {
        // Less than 30% CJK → English
        assert_eq!(
            MiniMaxTts::detect_voice("Hello 你好 world test data"),
            "English_Female_1"
        );
    }
}
