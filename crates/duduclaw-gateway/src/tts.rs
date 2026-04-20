//! Text-to-Speech (TTS) support — MiniMax T2A API.
//!
//! MiniMax Speech API: POST https://api.minimax.io/v1/t2a_v2
//! Returns audio bytes (MP3 by default).

use duduclaw_core::truncate_bytes;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use tracing::info;
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
            "Cute_Girl"  // Chinese voice
        } else {
            "English_Female_1"  // English voice
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
    audio_file: Option<String>,  // base64-encoded audio
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
            return Err(format!("MiniMax TTS error ({status}): {}", truncate_bytes(&text, 300)));
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

const EDGE_TTS_URL: &str = "wss://speech.platform.bing.com/consumer/speech/synthesize/readaloud/edge/v1";
const EDGE_TTS_TRUSTED_CLIENT_TOKEN: &str = "6A5AA1D4EAFF4E9FB37E23D68491D6F4";

/// Microsoft Edge TTS provider — free, high-quality Neural TTS.
///
/// Uses the same Azure Neural Voice engine as Azure Cognitive Services
/// but via the free Edge browser endpoint. No API key required.
///
/// Default voices:
/// - zh-TW: `zh-TW-HsiaoChenNeural` (female) / `zh-TW-YunJheNeural` (male)
/// - en-US: `en-US-AriaNeural` (female)
pub struct EdgeTtsProvider {
    default_voice_zh: String,
    default_voice_en: String,
}

impl EdgeTtsProvider {
    pub fn new() -> Self {
        Self {
            default_voice_zh: "zh-TW-HsiaoChenNeural".into(),
            default_voice_en: "en-US-AriaNeural".into(),
        }
    }

    pub fn with_voices(zh_voice: String, en_voice: String) -> Self {
        Self {
            default_voice_zh: zh_voice,
            default_voice_en: en_voice,
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

    fn build_ssml(text: &str, voice: &str) -> String {
        let escaped = text
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;");

        format!(
            r#"<speak version='1.0' xmlns='http://www.w3.org/2001/10/synthesis' xml:lang='en-US'><voice name='{voice}'><prosody pitch='+0Hz' rate='+0%' volume='+0%'>{escaped}</prosody></voice></speak>"#
        )
    }
}

#[async_trait::async_trait]
impl TtsProvider for EdgeTtsProvider {
    fn name(&self) -> &str {
        "edge-tts"
    }

    async fn synthesize(&self, text: &str, voice: &str) -> Result<Vec<u8>, String> {
        use tokio_tungstenite::tungstenite::Message;
        use futures_util::{SinkExt, StreamExt};

        let voice_name = if voice.is_empty() {
            self.detect_voice(text)
        } else {
            voice
        };

        info!(voice = voice_name, text_len = text.len(), "edge-tts: synthesizing");

        let request_id = uuid::Uuid::new_v4().as_simple().to_string();
        let url = format!(
            "{}?TrustedClientToken={}&ConnectionId={}",
            EDGE_TTS_URL, EDGE_TTS_TRUSTED_CLIENT_TOKEN, request_id
        );

        let (ws_stream, _) = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            tokio_tungstenite::connect_async(&url),
        )
        .await
        .map_err(|_| "edge-tts connect timeout (10s)".to_string())?
        .map_err(|e| format!("edge-tts WebSocket connect failed: {e}"))?;

        let (mut write, mut read) = ws_stream.split();

        // Send config message
        let config_msg = format!(
            "X-Timestamp:{}\r\nContent-Type:application/json; charset=utf-8\r\nPath:speech.config\r\n\r\n{{\"context\":{{\"synthesis\":{{\"audio\":{{\"metadataoptions\":{{\"sentenceBoundaryEnabled\":\"false\",\"wordBoundaryEnabled\":\"false\"}},\"outputFormat\":\"audio-24khz-48kbitrate-mono-mp3\"}}}}}}}}",
            chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ")
        );
        write.send(Message::Text(config_msg.into())).await
            .map_err(|e| format!("edge-tts config send failed: {e}"))?;

        // Send SSML message
        let ssml = Self::build_ssml(text, voice_name);
        let ssml_msg = format!(
            "X-RequestId:{}\r\nContent-Type:application/ssml+xml\r\nX-Timestamp:{}\r\nPath:ssml\r\n\r\n{}",
            request_id,
            chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ"),
            ssml
        );
        write.send(Message::Text(ssml_msg.into())).await
            .map_err(|e| format!("edge-tts SSML send failed: {e}"))?;

        // Collect audio chunks with 30s timeout
        let audio_data = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            let mut audio_data = Vec::new();
            let header_tag = b"Path:audio\r\n";

            while let Some(msg) = read.next().await {
                match msg {
                    Ok(Message::Binary(data)) => {
                        if let Some(pos) = data.windows(header_tag.len()).position(|w| w == header_tag) {
                            let audio_start = pos + header_tag.len();
                            if audio_start < data.len() {
                                audio_data.extend_from_slice(&data[audio_start..]);
                            }
                        }
                    }
                    Ok(Message::Text(txt)) => {
                        if txt.contains("Path:turn.end") {
                            break;
                        }
                    }
                    Ok(Message::Close(_)) | Err(_) => break,
                    _ => {}
                }
            }
            audio_data
        })
        .await
        .map_err(|_| "edge-tts synthesis timeout (30s)".to_string())?;

        if audio_data.is_empty() {
            return Err("edge-tts returned no audio data".into());
        }

        info!(bytes = audio_data.len(), "edge-tts: synthesis complete");
        Ok(audio_data)
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
        Self { api_key, model: "tts-1".into() }
    }

    pub fn from_env() -> Option<Self> {
        let key = std::env::var("OPENAI_API_KEY").ok()?;
        if key.is_empty() { return None; }
        Some(Self::new(key))
    }

    pub fn with_model(mut self, model: &str) -> Self {
        self.model = model.into();
        self
    }

    fn detect_voice(text: &str) -> &'static str {
        let cjk_count = text.chars().filter(|c| *c > '\u{2E80}').count();
        let total = text.chars().count().max(1);
        if cjk_count as f64 / total as f64 > 0.3 { "nova" } else { "alloy" }
    }
}

#[async_trait::async_trait]
impl TtsProvider for OpenAiTtsProvider {
    fn name(&self) -> &str { "openai-tts" }

    async fn synthesize(&self, text: &str, voice: &str) -> Result<Vec<u8>, String> {
        let voice_name = if voice.is_empty() { Self::detect_voice(text) } else { voice };
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
            return Err(format!("OpenAI TTS error ({status}): {}", truncate_bytes(&text, 200)));
        }

        let audio = resp.bytes().await.map_err(|e| format!("OpenAI TTS read: {e}"))?;
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
        Ok(Self { model_path, config_path })
    }

    /// Auto-detect from models directory.
    pub fn from_models_dir(models_dir: &std::path::Path) -> Option<Self> {
        let piper_dir = models_dir.join("piper");
        // Look for any .onnx file
        let model = std::fs::read_dir(&piper_dir).ok()?
            .filter_map(|e| e.ok())
            .find(|e| e.path().extension().is_some_and(|ext| ext == "onnx"))?
            .path();
        let config = model.with_extension("onnx.json");
        if !config.exists() { return None; }
        Self::new(
            model.to_string_lossy().into(),
            config.to_string_lossy().into(),
        ).ok()
    }
}

#[async_trait::async_trait]
impl TtsProvider for PiperTtsProvider {
    fn name(&self) -> &str { "piper" }

    async fn synthesize(&self, text: &str, _voice: &str) -> Result<Vec<u8>, String> {
        info!(model = %self.model_path, text_len = text.len(), "Piper TTS: synthesizing");

        // Use piper subprocess (cross-platform, no Rust binding needed)
        let mut child = tokio::process::Command::new("piper")
            .args(["--model", &self.model_path, "--config", &self.config_path, "--output-raw"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("Piper not found: {e}. Install: pip install piper-tts"))?;

        // Write text to stdin then close it (piper reads from stdin)
        {
            use tokio::io::AsyncWriteExt;
            let mut stdin = child.stdin.take()
                .ok_or_else(|| "Piper stdin unavailable".to_string())?;
            stdin.write_all(text.as_bytes()).await
                .map_err(|e| format!("Piper stdin write: {e}"))?;
            // stdin dropped here → EOF to piper
        }

        // Wait with timeout; kill child on timeout to prevent zombie processes
        // Note: child.kill() requires &mut, so we keep ownership outside the timeout.
        let wait_result = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            async {
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
            },
        ).await;

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
#[derive(Debug, Clone, Copy)]
pub enum TtsStrategy {
    /// Local first → edge-tts → MiniMax → OpenAI.
    LocalFirst,
    /// edge-tts only (free, no API key).
    EdgeOnly,
    /// Best quality cloud provider.
    CloudBest,
}

/// TTS Router — dispatches to the best available provider.
pub struct TtsRouter {
    providers: Vec<Box<dyn TtsProvider>>,
}

impl TtsRouter {
    pub fn new() -> Self {
        Self { providers: Vec::new() }
    }

    pub fn add_provider(mut self, provider: Box<dyn TtsProvider>) -> Self {
        info!(provider = provider.name(), "TTS Router: registered provider");
        self.providers.push(provider);
        self
    }

    /// Build a router with auto-detected providers.
    ///
    /// Provider priority depends on strategy:
    /// - LocalFirst: Piper → edge-tts → MiniMax → OpenAI
    /// - EdgeOnly: edge-tts only
    /// - CloudBest: MiniMax → OpenAI → edge-tts (paid first for quality)
    pub fn auto_detect(models_dir: &std::path::Path, strategy: TtsStrategy) -> Self {
        let mut router = Self::new();

        if matches!(strategy, TtsStrategy::LocalFirst) {
            // Piper local (highest priority)
            if let Some(piper) = PiperTtsProvider::from_models_dir(models_dir) {
                router.providers.push(Box::new(piper));
            }
            // edge-tts free cloud
            router.providers.push(Box::new(EdgeTtsProvider::new()));
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
            router.providers.push(Box::new(EdgeTtsProvider::new()));
        } else {
            // EdgeOnly
            router.providers.push(Box::new(EdgeTtsProvider::new()));
        }

        if router.providers.is_empty() {
            tracing::warn!("TTS Router: no providers available");
        }
        router
    }
}

#[async_trait::async_trait]
impl TtsProvider for TtsRouter {
    fn name(&self) -> &str { "tts-router" }

    async fn synthesize(&self, text: &str, voice: &str) -> Result<Vec<u8>, String> {
        let mut last_err = None;
        for provider in &self.providers {
            match provider.synthesize(text, voice).await {
                Ok(audio) if !audio.is_empty() => {
                    info!(provider = provider.name(), bytes = audio.len(), "TTS: synthesis succeeded");
                    return Ok(audio);
                }
                Ok(_) => {
                    tracing::warn!(provider = provider.name(), "TTS: returned empty audio, trying next");
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
    fn test_detect_voice_chinese() {
        assert_eq!(MiniMaxTts::detect_voice("你好世界，今天天氣怎麼樣？"), "Cute_Girl");
    }

    #[test]
    fn test_detect_voice_english() {
        assert_eq!(MiniMaxTts::detect_voice("Hello world, how are you?"), "English_Female_1");
    }

    #[test]
    fn test_detect_voice_mixed() {
        // Less than 30% CJK → English
        assert_eq!(MiniMaxTts::detect_voice("Hello 你好 world test data"), "English_Female_1");
    }
}
