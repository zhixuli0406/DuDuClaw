//! Text-to-speech routing for local models and official cloud APIs.
//! Returns WAV for local synthesis and MP3 for cloud synthesis.

use duduclaw_core::truncate_bytes;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use tracing::{info, warn};
use zeroize::Zeroize;

pub const MAX_TTS_TEXT_CHARS: usize = 5000;
pub const MAX_TTS_AUDIO_BYTES: usize = 50 * 1024 * 1024;

fn audio_limit_error() -> String { "TTS audio exceeds the 50 MiB limit".into() }

async fn read_bounded(reader: impl tokio::io::AsyncRead + Unpin, limit: usize) -> Result<Vec<u8>, String> {
    use tokio::io::AsyncReadExt;
    let mut bytes = Vec::new();
    reader.take(limit as u64 + 1).read_to_end(&mut bytes).await
        .map_err(|e| format!("TTS read failed: {e}"))?;
    if bytes.len() > limit { return Err("TTS response exceeds the byte limit".into()); }
    Ok(bytes)
}

async fn read_audio_file(path: &std::path::Path) -> Result<Vec<u8>, String> {
    let file = tokio::fs::File::open(path).await.map_err(|e| format!("TTS audio open failed: {e}"))?;
    let metadata = file.metadata().await.map_err(|e| format!("TTS audio metadata failed: {e}"))?;
    if !metadata.is_file() { return Err("TTS output must be a regular file".into()); }
    if metadata.len() > MAX_TTS_AUDIO_BYTES as u64 { return Err(audio_limit_error()); }
    // Bound the same open handle as well, in case the file grows after metadata.
    read_bounded(file, MAX_TTS_AUDIO_BYTES).await
}

async fn read_http_bounded(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>, String> {
    if response.content_length().is_some_and(|len| len > limit as u64) {
        return Err("TTS response exceeds the byte limit".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "TTS response read failed".to_string())? {
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            return Err("TTS response exceeds the byte limit".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Split a TTS command template like STT does, replacing path placeholders as
/// individual argv tokens. The input text is deliberately never an argument.
pub fn build_tts_command_args(command: &str, input_path: &str, output_path: &str) -> Result<(String, Vec<String>), String> {
    let mut tokens: Vec<String> = command.split_whitespace().map(str::to_string).collect();
    if tokens.is_empty() { return Err("tts_command is empty".into()); }
    let has_output = tokens.iter().any(|arg| arg.contains("{output}"));
    if !has_output { return Err("tts_command must contain the {output} placeholder".into()); }
    for arg in &mut tokens {
        *arg = arg.replace("{input}", input_path).replace("{output}", output_path);
    }
    let program = tokens.remove(0);
    Ok((program, tokens))
}

pub struct CommandTtsProvider { command: String }
impl CommandTtsProvider { pub fn new(command: String) -> Self { Self { command } } }

async fn run_tts_command(command: &str, input: &std::path::Path, output: &std::path::Path, timeout: std::time::Duration) -> Result<(), String> {
    let input_s = input.to_string_lossy();
    let output_s = output.to_string_lossy();
    let (program, args) = build_tts_command_args(command, &input_s, &output_s)?;
    let mut cmd = tokio::process::Command::new(&program);
    duduclaw_core::ns_guard::guard_command(&mut cmd);
    cmd.args(&args).env_clear();
    for key in ["PATH", "HOME", "USERPROFILE", "TEMP", "TMP", "SYSTEMROOT"] {
        if let Some(value) = std::env::var_os(key) { cmd.env(key, value); }
    }
    cmd.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped()).kill_on_drop(true);
    let mut child = duduclaw_core::execution_gate::spawn(&mut cmd, duduclaw_core::execution_gate::ExecutionClass::ParseWorker).await.map_err(|e| format!("TTS command spawn failed: {e}"))?;
    let stderr = child.stderr.take().ok_or("TTS command stderr unavailable")?;
    let result = tokio::time::timeout(timeout, async {
        let stderr = read_bounded(stderr, 64 * 1024).await?;
        let status = child.wait().await.map_err(|e| format!("TTS command wait failed: {e}"))?;
        Ok::<_, String>((status, stderr))
    }).await;
    let (status, stderr) = match result {
        Ok(result) => result?,
        Err(_) => return Err("TTS command timeout (60s)".into()),
    };
    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr);
        return Err(format!("TTS command exited with {}: {}", status, truncate_bytes(stderr.trim(), 300)));
    }
    Ok(())
}

#[async_trait::async_trait]
impl TtsProvider for CommandTtsProvider {
    fn name(&self) -> &str { "command" }
    async fn synthesize(&self, text: &str, _voice: &str) -> Result<Vec<u8>, String> {
        let dir = tempfile::Builder::new().prefix("duduclaw-tts-").tempdir()
            .map_err(|e| format!("TTS temp directory: {e}"))?;
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
                .map_err(|e| format!("TTS temp permissions: {e}"))?;
        }
        let input = dir.path().join("input.txt");
        let output = dir.path().join("output.audio");
        tokio::fs::write(&input, text.as_bytes()).await.map_err(|e| format!("TTS input write: {e}"))?;
        run_tts_command(&self.command, &input, &output, std::time::Duration::from_secs(60)).await?;
        read_audio_file(&output).await
    }
}

static HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

fn http_client() -> &'static reqwest::Client {
    HTTP_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .unwrap_or_default()
    })
}

fn verify_tts_authority() -> Result<(), String> {
    duduclaw_mcp_boundary::io_authority::verify_before_io()
        .map_err(|_| "Isolated TTS authority unavailable".into())
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

        verify_tts_authority()?;
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
            let bytes = read_http_bounded(response, 4096).await?;
            let text = String::from_utf8_lossy(&bytes);
            return Err(format!(
                "MiniMax TTS error ({status}): {}",
                truncate_bytes(&text, 300)
            ));
        }

        // Base64 expands audio by 4/3; leave bounded room for the JSON envelope.
        let body = read_http_bounded(response, MAX_TTS_AUDIO_BYTES.div_ceil(3) * 4 + 64 * 1024).await?;
        let resp: T2aResponse = serde_json::from_slice(&body)
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

        if audio_b64.len() > MAX_TTS_AUDIO_BYTES.div_ceil(3) * 4 { return Err(audio_limit_error()); }
        use base64::Engine;
        let audio_bytes = base64::engine::general_purpose::STANDARD
            .decode(&audio_b64)
            .map_err(|e| format!("Failed to decode audio base64: {e}"))?;

        if audio_bytes.len() > MAX_TTS_AUDIO_BYTES { return Err(audio_limit_error()); }
        info!(bytes = audio_bytes.len(), "MiniMax TTS: synthesis complete");
        Ok(audio_bytes)
    }
}

// ── Azure AI Speech REST ───────────────────────────────────────

/// Official Azure AI Speech service. Credentials remain in the gateway.
pub struct AzureTts {
    api_key: String,
    region: String,
}

impl Drop for AzureTts {
    fn drop(&mut self) { self.api_key.zeroize(); }
}

impl AzureTts {
    pub fn from_env() -> Option<Self> {
        let api_key = std::env::var("AZURE_SPEECH_KEY").ok()?;
        let region = std::env::var("AZURE_SPEECH_REGION").ok()?;
        if api_key.is_empty() || !Self::valid_region(&region) { return None; }
        Some(Self { api_key, region })
    }

    fn valid_region(region: &str) -> bool {
        !region.is_empty() && region.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    }

    fn valid_voice(voice: &str) -> bool {
        !voice.is_empty() && voice.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
    }

    fn detect_voice(text: &str) -> &'static str {
        let cjk_count = text.chars().filter(|c| *c > '\u{2E80}').count();
        let total = text.chars().count().max(1);
        if cjk_count as f64 / total as f64 > 0.3 {
            "zh-TW-HsiaoChenNeural"
        } else {
            "en-US-AriaNeural"
        }
    }

    fn ssml(text: &str, voice: &str) -> String {
        let language = voice.split('-').take(2).collect::<Vec<_>>().join("-");
        let escaped = text.replace('&', "&amp;").replace('<', "&lt;")
            .replace('>', "&gt;").replace('\"', "&quot;").replace('\'', "&apos;");
        format!("<speak version=\"1.0\" xmlns=\"http://www.w3.org/2001/10/synthesis\" xml:lang=\"{language}\"><voice name=\"{voice}\">{escaped}</voice></speak>")
    }
}

#[async_trait::async_trait]
impl TtsProvider for AzureTts {
    fn name(&self) -> &str { "azure" }

    async fn synthesize(&self, text: &str, voice: &str) -> Result<Vec<u8>, String> {
        if !Self::valid_region(&self.region) { return Err("Invalid Azure Speech region".into()); }
        let voice = if voice.is_empty() { Self::detect_voice(text) } else { voice };
        if !Self::valid_voice(voice) { return Err("Invalid Azure Speech voice".into()); }
        // A redirect must never forward the subscription key to a different endpoint.
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .build().map_err(|_| "Azure Speech HTTP client initialization failed".to_string())?;
        verify_tts_authority()?;
        let response = client
            .post(format!("https://{}.tts.speech.microsoft.com/cognitiveservices/v1", self.region))
            .header("Ocp-Apim-Subscription-Key", &self.api_key)
            .header("Content-Type", "application/ssml+xml")
            .header("X-Microsoft-OutputFormat", "audio-24khz-48kbitrate-mono-mp3")
            .header("User-Agent", "duduclaw")
            .body(Self::ssml(text, voice))
            .send().await.map_err(|_| "Azure Speech request failed".to_string())?;
        let status = response.status();
        if !status.is_success() {
            // Do not echo the remote response or request headers containing credentials.
            return Err(format!("Azure Speech error ({status})"));
        }
        let audio = read_http_bounded(response, MAX_TTS_AUDIO_BYTES).await?;
        if audio.is_empty() { return Err("Azure Speech returned no audio".into()); }
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

        verify_tts_authority()?;
        let resp = http_client()
            .post("https://api.openai.com/v1/audio/speech")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("OpenAI TTS: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let bytes = read_http_bounded(resp, 4096).await?;
            let text = String::from_utf8_lossy(&bytes);
            return Err(format!(
                "OpenAI TTS error ({status}): {}",
                truncate_bytes(&text, 200)
            ));
        }

        let audio = read_http_bounded(resp, MAX_TTS_AUDIO_BYTES).await?;
        info!(bytes = audio.len(), "OpenAI TTS: synthesis complete");
        Ok(audio)
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
        cmd.kill_on_drop(true)
            .args([
                "--model",
                &self.model_path,
                "--config",
                &self.config_path,
                "--output-raw",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = duduclaw_core::execution_gate::spawn(&mut cmd, duduclaw_core::execution_gate::ExecutionClass::ParseWorker).await
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

        // Read both pipes concurrently, bounding raw PCM before WAV allocation.
        let stdout = child.stdout.take().ok_or("Piper stdout unavailable")?;
        let stderr = child.stderr.take().ok_or("Piper stderr unavailable")?;
        let wait_result = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            let (stdout_data, stderr_data) = tokio::try_join!(
                read_bounded(stdout, MAX_TTS_AUDIO_BYTES - 44),
                read_bounded(stderr, 64 * 1024),
            )?;
            let status = child.wait().await;
            Ok::<_, String>((status, stdout_data, stderr_data))
        }).await;
        let (status, stdout_data, stderr_data) = match wait_result {
            Ok(result) => result?,
            Err(_) => return Err("Piper TTS timeout (30s)".into()),
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
pub(crate) fn pcm_i16_to_wav(pcm: &[u8], sample_rate: u32) -> Vec<u8> {
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
    /// Piper → Kokoro → MiniMax → OpenAI → Azure → configured command.
    LocalFirst,
    /// Local Kokoro only.
    KokoroOnly,
    /// Official Azure AI Speech only.
    AzureOnly,
    /// Best quality cloud provider.
    CloudBest,
    PiperOnly,
    MiniMaxOnly,
    OpenAiOnly,
    CommandOnly,
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

    /// Protected E tools may use official HTTP providers only. Configured
    /// local/command selection stays unavailable until A has a concrete plan.
    pub fn for_protected_home(home: &std::path::Path) -> Self {
        let mut router = Self::for_home(home);
        router.providers.retain(|provider| matches!(provider.name(), "minimax" | "azure" | "openai-tts"));
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
            // Removed settings leave TTS disabled; point operators to Kokoro.
            "edge-tts" | "edge" => {
                static WARN: std::sync::Once = std::sync::Once::new();
                WARN.call_once(|| warn!("edge-tts is no longer supported; TTS is disabled. Choose Kokoro instead."));
                return None;
            }
            "kokoro" => TtsStrategy::KokoroOnly,
            "azure" => TtsStrategy::AzureOnly,
            "piper" => TtsStrategy::PiperOnly,
            "minimax" => TtsStrategy::MiniMaxOnly,
            "openai-tts" | "openai" => TtsStrategy::OpenAiOnly,
            "command" => TtsStrategy::CommandOnly,
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
    /// Auto prefers installed local providers, then configured cloud services.
    pub fn auto_detect(models_dir: &std::path::Path, strategy: TtsStrategy) -> Self {
        Self::auto_detect_for_home(&duduclaw_core::platform::duduclaw_home(), models_dir, strategy)
    }

    pub fn auto_detect_for_home(home_dir: &std::path::Path, models_dir: &std::path::Path, strategy: TtsStrategy) -> Self {
        let mut router = Self::new();
        router.home_dir = Some(home_dir.to_path_buf());
        let Ok(policy) = crate::product_policy::load(home_dir) else { return router; };

        if matches!(strategy, TtsStrategy::LocalFirst | TtsStrategy::PiperOnly) {
            if let Some(piper) = PiperTtsProvider::from_models_dir(models_dir) {
                router.providers.push(Box::new(piper));
            }
        }
        if matches!(strategy, TtsStrategy::LocalFirst | TtsStrategy::KokoroOnly)
            && crate::tts_kokoro::is_installed(home_dir) {
            router.providers.push(Box::new(crate::tts_kokoro::KokoroTtsProvider::new(home_dir.to_path_buf())));
        }
        if matches!(strategy, TtsStrategy::LocalFirst | TtsStrategy::CloudBest | TtsStrategy::MiniMaxOnly) {
            if let Some(minimax) = MiniMaxTts::from_env() { router.providers.push(Box::new(minimax)); }
        }
        if matches!(strategy, TtsStrategy::LocalFirst | TtsStrategy::CloudBest | TtsStrategy::OpenAiOnly) {
            if let Some(openai) = OpenAiTtsProvider::from_env() { router.providers.push(Box::new(openai)); }
        }
        if matches!(strategy, TtsStrategy::LocalFirst | TtsStrategy::CloudBest | TtsStrategy::AzureOnly) {
            if let Some(azure) = AzureTts::from_env() { router.providers.push(Box::new(azure)); }
        }

        if matches!(strategy, TtsStrategy::LocalFirst | TtsStrategy::CommandOnly) {
            if let Ok(config) = Self::load_inference_config(home_dir) {
                if let Some(command) = config.get("voice").and_then(|v| v.get("tts_command")).and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty()) {
                    if command.split_whitespace().any(|arg| arg.contains("{output}")) {
                        router.providers.push(Box::new(CommandTtsProvider::new(command.to_string())));
                    } else {
                        warn!("TTS command provider unavailable: tts_command has no {{output}} placeholder");
                    }
                }
            }
        }

        router.providers.retain(|provider| {
            policy.require_provider(provider.name()).is_ok()
                && (!matches!(strategy, TtsStrategy::CommandOnly) || provider.name() == "command")
        });
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
        if text.chars().count() > MAX_TTS_TEXT_CHARS {
            return Err(format!("TTS text exceeds the {MAX_TTS_TEXT_CHARS} characters limit"));
        }
        let voice = if voice.is_empty() { &self.default_voice } else { voice };
        let mut last_err = None;
        for provider in &self.providers {
            if let Some(home) = self.home_dir.as_deref() {
                let policy = crate::product_policy::load(home)?;
                if policy.require_provider(provider.name()).is_err() { continue; }
            }
            verify_tts_authority()?;
            match provider.synthesize(text, voice).await {
                Ok(audio) if audio.len() > MAX_TTS_AUDIO_BYTES => return Err(audio_limit_error()),
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

    #[tokio::test]
    async fn router_rejects_overlong_text_by_character_count() {
        let router = TtsRouter::new().add_provider(Box::new(VoiceEcho));
        assert!(router.synthesize(&"字".repeat(MAX_TTS_TEXT_CHARS), "ok").await.is_ok());
        let err = router.synthesize(&"字".repeat(MAX_TTS_TEXT_CHARS + 1), "").await.unwrap_err();
        assert!(err.contains("5000 characters"));
    }

    struct OversizedAudio;
    #[async_trait::async_trait]
    impl TtsProvider for OversizedAudio {
        fn name(&self) -> &str { "oversized-test" }
        async fn synthesize(&self, _: &str, _: &str) -> Result<Vec<u8>, String> {
            Ok(vec![0; MAX_TTS_AUDIO_BYTES + 1])
        }
    }

    #[tokio::test]
    async fn router_rejects_oversized_audio() {
        let router = TtsRouter::new().add_provider(Box::new(OversizedAudio));
        assert!(router.synthesize("hello", "").await.unwrap_err().contains("50 MiB"));
    }

    #[tokio::test]
    async fn output_file_size_is_checked_before_reading() {
        let file = tempfile::NamedTempFile::new().unwrap();
        file.as_file().set_len(MAX_TTS_AUDIO_BYTES as u64 + 1).unwrap();
        assert!(read_audio_file(file.path()).await.unwrap_err().contains("50 MiB"));
        file.as_file().set_len(0).unwrap();
        std::fs::write(file.path(), b"small audio").unwrap();
        assert_eq!(read_audio_file(file.path()).await.unwrap(), b"small audio");
    }

    #[tokio::test]
    async fn http_audio_limit_checks_declared_and_streamed_sizes() {
        for wire in [
            "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\n12345",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\n12345\r\n0\r\n\r\n",
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0; 2048];
                stream.read(&mut request).await.unwrap();
                stream.write_all(wire.as_bytes()).await.unwrap();
            });
            let response = reqwest::Client::builder().no_proxy().build().unwrap()
                .get(format!("http://{addr}/")).send().await.unwrap();
            assert!(read_http_bounded(response, 4).await.unwrap_err().contains("limit"));
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn bounded_reader_rejects_growth_past_limit() {
        assert!(read_bounded(&b"12345"[..], 4).await.unwrap_err().contains("limit"));
        assert_eq!(read_bounded(&b"1234"[..], 4).await.unwrap(), b"1234");
    }

    #[test]
    fn voice_validation_rejects_shell_syntax() {
        assert!(!AzureTts::valid_voice("a;b"));
        assert!(!AzureTts::valid_voice(""));
        assert!(AzureTts::valid_voice("zh-TW-HsiaoChenNeural"));
    }

    #[test]
    fn router_settings_map_to_the_selected_provider() {
        for (setting, strategy) in [
            ("", TtsStrategy::LocalFirst),
            ("auto", TtsStrategy::LocalFirst),
            ("kokoro", TtsStrategy::KokoroOnly),
            ("azure", TtsStrategy::AzureOnly),
            ("piper", TtsStrategy::PiperOnly),
            ("minimax", TtsStrategy::MiniMaxOnly),
            ("openai", TtsStrategy::OpenAiOnly),
            ("openai-tts", TtsStrategy::OpenAiOnly),
            ("command", TtsStrategy::CommandOnly),
        ] {
            assert_eq!(TtsRouter::strategy_for_setting(setting), Some(strategy));
        }
        for setting in ["edge-tts", "edge"] {
            assert_eq!(TtsRouter::strategy_for_setting(setting), None);
        }
        for setting in ["none", "off", "disabled"] {
            assert_eq!(TtsRouter::strategy_for_setting(setting), None);
        }
        assert!(!TtsRouter::new().is_available());
    }

    #[test]
    fn command_placeholders_are_separate_argv_and_missing_output_is_rejected() {
        let (program, args) = build_tts_command_args("/bin/tool --in {input} --out {output}", "/tmp/in file", "/tmp/out file").unwrap();
        assert_eq!(program, "/bin/tool");
        assert_eq!(args, ["--in", "/tmp/in file", "--out", "/tmp/out file"]);
        assert!(build_tts_command_args("tool --in {input}", "/tmp/in", "/tmp/out").unwrap_err().contains("{output}"));
    }

    #[test]
    fn command_is_a_supported_router_strategy() {
        assert_eq!(TtsRouter::strategy_for_setting("command"), Some(TtsStrategy::CommandOnly));
    }

    #[tokio::test]
    async fn command_provider_reports_nonzero_exit_with_truncated_stderr() {
        #[cfg(unix)] {
            let dir = tempfile::tempdir().unwrap();
            let script = dir.path().join("tts");
            crate::test_exec::write_executable(&script, "#!/bin/sh\nprintf 'failure detail' >&2\nexit 7\n", 0o700);
            let err = run_tts_command(&format!("{} {{output}}", script.display()), std::path::Path::new("/tmp/in"), std::path::Path::new("/tmp/out"), std::time::Duration::from_secs(2)).await.unwrap_err();
            assert!(err.contains("failure detail"));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_provider_uses_input_file_and_returns_script_output() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("tts");
        crate::test_exec::write_executable(&script, "#!/bin/sh\n[ \"$#\" -eq 4 ] || exit 21\n[ \"$1\" = --in ] || exit 22\n[ \"$3\" = --out ] || exit 23\n[ -f \"$2\" ] || exit 24\n[ \"$2\" != 'hidden phrase' ] || exit 25\ncat \"$2\" | grep -q 'hidden phrase' || exit 26\nprintf 'RIFFfake' > \"$4\"\n", 0o700);
        let provider = CommandTtsProvider::new(format!("{} --in {{input}} --out {{output}}", script.display()));
        assert_eq!(provider.synthesize("hidden phrase", "").await.unwrap(), b"RIFFfake");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_provider_timeout_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("tts");
        crate::test_exec::write_executable(&script, "#!/bin/sh\nsleep 61\n", 0o700);
        let provider = CommandTtsProvider::new(format!("{} {{output}}", script.display()));
        let result = run_tts_command(&provider.command, std::path::Path::new("/tmp/in"), std::path::Path::new("/tmp/out"), std::time::Duration::from_millis(100)).await;
        assert!(result.unwrap_err().contains("timeout"));
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

    #[test]
    fn azure_validates_region_and_escapes_ssml() {
        assert!(AzureTts::valid_region("eastasia"));
        assert!(!AzureTts::valid_region("a.evil.com"));
        assert!(!AzureTts::valid_region(""));
        assert!(!AzureTts::valid_region("EastAsia"));
        assert_eq!(AzureTts::detect_voice("你好，今天天氣真好。"), "zh-TW-HsiaoChenNeural");
        assert_eq!(AzureTts::detect_voice("Hello world"), "en-US-AriaNeural");
        let ssml = AzureTts::ssml("<&>", "zh-TW-HsiaoChenNeural");
        assert!(ssml.contains("&lt;&amp;&gt;"));
        assert!(ssml.contains("xml:lang=\"zh-TW\""));
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
