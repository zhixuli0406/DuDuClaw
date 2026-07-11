//! Speech-to-Text (STT) support — provider abstraction + two implementations.
//!
//! The API-level twin of `tts.rs`. Two providers:
//!  1. [`OpenAiCompatStt`] — POST `{base_url}/audio/transcriptions` (multipart,
//!     bearer key). OpenAI Whisper and Groq Whisper both speak this shape.
//!  2. [`CommandStt`] — a local subprocess template (e.g. `whisper-cli`); the
//!     audio is written to a scratch temp file and fed to the command, and the
//!     transcript is read back from stdout.
//!
//! Config lives in `~/.duduclaw/config.toml [voice]`:
//! ```toml
//! [voice]
//! stt_provider  = "openai_compat"        # or "command"
//! stt_base_url  = "https://api.openai.com/v1"
//! stt_api_key   = "sk-..."               # or stt_api_key_enc (AES-256-GCM)
//! stt_model     = "whisper-1"            # default: whisper-1
//! stt_command   = "whisper-cli -m /models/ggml-base.bin -f {audio} --output-txt --no-prints"
//! ```
//!
//! **Fail-closed**: when `stt_provider` is unset the endpoint returns HTTP 501.
//! We never guess or fabricate a transcript.

use std::path::Path;
use std::sync::OnceLock;

use tracing::info;

static HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

fn http_client() -> &'static reqwest::Client {
    HTTP_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            // Audio upload + model latency — generous but bounded.
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .unwrap_or_default()
    })
}

/// Default transcription model when `stt_model` is unset.
pub const DEFAULT_STT_MODEL: &str = "whisper-1";

// ── Provider trait ──────────────────────────────────────────────

/// Abstract STT provider.
#[async_trait::async_trait]
pub trait SttProvider: Send + Sync {
    fn name(&self) -> &str;
    /// Transcribe `audio` (the raw bytes of `filename`, e.g. `voice.webm`) into
    /// text. `language` is an optional ISO hint (e.g. `"zh"`); providers may
    /// ignore it.
    async fn transcribe(
        &self,
        audio: &[u8],
        filename: &str,
        language: Option<&str>,
    ) -> Result<String, String>;
}

// ── Provider kind parsing ───────────────────────────────────────

/// Which concrete STT provider a `[voice] stt_provider` string selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SttProviderKind {
    OpenAiCompat,
    Command,
}

/// Parse the `stt_provider` config value. Returns `None` for unknown values so
/// the caller can fail closed with a clear error (never a silent default).
pub fn parse_provider_kind(s: &str) -> Option<SttProviderKind> {
    match s.trim().to_ascii_lowercase().as_str() {
        "openai_compat" | "openai-compat" | "openai" | "whisper-api" => {
            Some(SttProviderKind::OpenAiCompat)
        }
        "command" | "whisper-local" | "local" | "cli" => Some(SttProviderKind::Command),
        _ => None,
    }
}

/// Guess an audio MIME type from a filename extension. Falls back to
/// `application/octet-stream` for unknown extensions.
pub fn guess_audio_mime(filename: &str) -> &'static str {
    let ext = filename
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "webm" => "audio/webm",
        "ogg" | "oga" => "audio/ogg",
        "wav" => "audio/wav",
        "mp3" => "audio/mpeg",
        "m4a" | "mp4" => "audio/mp4",
        "flac" => "audio/flac",
        _ => "application/octet-stream",
    }
}

/// Reject over-budget uploads before buffering the whole body. Pure + testable.
pub fn check_audio_size(len: usize, max: usize) -> Result<(), String> {
    if len == 0 {
        return Err("empty audio upload".to_string());
    }
    if len > max {
        return Err(format!(
            "audio too large: {len} bytes exceeds {max} byte limit"
        ));
    }
    Ok(())
}

// ── Config → provider ───────────────────────────────────────────

/// Build the configured STT provider from `~/.duduclaw/config.toml [voice]`.
///
/// Returns:
/// - `Ok(None)` when `stt_provider` is unset/empty → caller returns HTTP 501
///   (fail-closed; we never guess).
/// - `Ok(Some(provider))` when configured and valid.
/// - `Err(msg)` when configured but invalid (unknown provider, missing
///   base_url / command).
pub async fn build_provider_from_config(
    home_dir: &Path,
) -> Result<Option<Box<dyn SttProvider>>, String> {
    let config_path = home_dir.join("config.toml");
    let content = match tokio::fs::read_to_string(&config_path).await {
        Ok(c) => c,
        // No config file at all ⇒ treat as unset (fail-closed 501, not error).
        Err(_) => return Ok(None),
    };
    let table: toml::Table = content
        .parse()
        .map_err(|e| format!("config.toml parse error: {e}"))?;

    let voice = match table.get("voice").and_then(|v| v.as_table()) {
        Some(v) => v,
        None => return Ok(None),
    };

    let provider_str = voice
        .get("stt_provider")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if provider_str.is_empty() {
        return Ok(None);
    }

    let kind = parse_provider_kind(provider_str).ok_or_else(|| {
        format!("Unknown stt_provider '{provider_str}' (expected 'openai_compat' or 'command')")
    })?;

    match kind {
        SttProviderKind::OpenAiCompat => {
            let base_url = voice
                .get("stt_base_url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if base_url.is_empty() {
                return Err("stt_base_url is required for openai_compat STT".to_string());
            }
            // Key supports both plaintext `stt_api_key` and `stt_api_key_enc`
            // (AES-256-GCM), plus `secret://` references — same resolver as
            // every other gateway secret.
            let api_key =
                crate::config_crypto::read_encrypted_config_field(home_dir, "voice", "stt_api_key")
                    .await;
            let model = voice
                .get("stt_model")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(DEFAULT_STT_MODEL)
                .to_string();
            Ok(Some(Box::new(OpenAiCompatStt::new(base_url, api_key, model))))
        }
        SttProviderKind::Command => {
            let command = voice
                .get("stt_command")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if command.is_empty() {
                return Err("stt_command is required for command STT".to_string());
            }
            Ok(Some(Box::new(CommandStt::new(command))))
        }
    }
}

// ── OpenAI-compatible STT (Whisper HTTP) ────────────────────────

/// OpenAI-compatible transcription endpoint (`/audio/transcriptions`).
/// Works with OpenAI Whisper, Groq Whisper, and any compatible relay.
pub struct OpenAiCompatStt {
    base_url: String,
    api_key: Option<String>,
    model: String,
}

impl Drop for OpenAiCompatStt {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        if let Some(k) = self.api_key.as_mut() {
            k.zeroize();
        }
    }
}

impl OpenAiCompatStt {
    pub fn new(base_url: String, api_key: Option<String>, model: String) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            model,
        }
    }

    fn endpoint(&self) -> String {
        format!("{}/audio/transcriptions", self.base_url)
    }
}

#[async_trait::async_trait]
impl SttProvider for OpenAiCompatStt {
    fn name(&self) -> &str {
        "openai_compat"
    }

    async fn transcribe(
        &self,
        audio: &[u8],
        filename: &str,
        language: Option<&str>,
    ) -> Result<String, String> {
        info!(
            model = %self.model,
            bytes = audio.len(),
            filename = filename,
            "STT (openai_compat): transcribing"
        );

        let part = reqwest::multipart::Part::bytes(audio.to_vec())
            .file_name(filename.to_string())
            .mime_str(guess_audio_mime(filename))
            .map_err(|e| format!("STT: invalid audio mime: {e}"))?;

        let mut form = reqwest::multipart::Form::new()
            .text("model", self.model.clone())
            .text("response_format", "json")
            .part("file", part);
        if let Some(lang) = language {
            if !lang.is_empty() {
                form = form.text("language", lang.to_string());
            }
        }

        let mut req = http_client().post(self.endpoint());
        if let Some(key) = &self.api_key {
            if !key.is_empty() {
                req = req.header("Authorization", format!("Bearer {key}"));
            }
        }

        let resp = req
            .multipart(form)
            .send()
            .await
            .map_err(|e| format!("STT request failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!(
                "STT error ({status}): {}",
                duduclaw_core::truncate_bytes(&body, 300)
            ));
        }

        // OpenAI returns `{ "text": "..." }`. Be lenient: accept a bare string
        // too (some relays return `text/plain`).
        let raw = resp
            .text()
            .await
            .map_err(|e| format!("STT: failed to read response: {e}"))?;
        let text = match serde_json::from_str::<serde_json::Value>(&raw) {
            Ok(v) => v
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .trim()
                .to_string(),
            Err(_) => raw.trim().to_string(),
        };

        info!(chars = text.chars().count(), "STT (openai_compat): done");
        Ok(text)
    }
}

// ── Command / local subprocess STT ──────────────────────────────

/// Local STT via a subprocess command template.
///
/// The command string is split on whitespace; a `{audio}` placeholder is
/// replaced by the scratch temp-file path (if no placeholder is present the
/// path is appended as the final argument). The transcript is read from stdout.
pub struct CommandStt {
    command: String,
}

impl CommandStt {
    pub fn new(command: String) -> Self {
        Self { command }
    }
}

/// Split a command template into `(program, args)`, substituting the `{audio}`
/// placeholder with `audio_path` (or appending it when absent). Pure + testable.
///
/// Note: whitespace-splitting does not honour shell quoting — command templates
/// must not embed spaces inside a single argument. The scratch path is a
/// system temp file (no spaces on the supported platforms).
pub fn build_command_args(
    command: &str,
    audio_path: &str,
) -> Result<(String, Vec<String>), String> {
    let mut tokens: Vec<String> = command.split_whitespace().map(|s| s.to_string()).collect();
    if tokens.is_empty() {
        return Err("stt_command is empty".to_string());
    }
    let mut had_placeholder = false;
    for t in tokens.iter_mut() {
        if t.contains("{audio}") {
            *t = t.replace("{audio}", audio_path);
            had_placeholder = true;
        }
    }
    if !had_placeholder {
        tokens.push(audio_path.to_string());
    }
    let prog = tokens.remove(0);
    Ok((prog, tokens))
}

#[async_trait::async_trait]
impl SttProvider for CommandStt {
    fn name(&self) -> &str {
        "command"
    }

    async fn transcribe(
        &self,
        audio: &[u8],
        filename: &str,
        _language: Option<&str>,
    ) -> Result<String, String> {
        let ext = filename
            .rsplit('.')
            .next()
            .filter(|e| !e.is_empty() && e.len() <= 5 && e.chars().all(|c| c.is_ascii_alphanumeric()))
            .unwrap_or("webm");
        let tmp = std::env::temp_dir().join(format!(
            "duduclaw-stt-{}.{}",
            uuid::Uuid::new_v4().as_simple(),
            ext
        ));

        tokio::fs::write(&tmp, audio)
            .await
            .map_err(|e| format!("STT: temp write failed: {e}"))?;

        let tmp_str = tmp.to_string_lossy().to_string();
        let result = self.run(&tmp_str).await;

        // Best-effort cleanup — always attempt, even on error.
        let _ = tokio::fs::remove_file(&tmp).await;
        result
    }
}

impl CommandStt {
    async fn run(&self, audio_path: &str) -> Result<String, String> {
        let (prog, args) = build_command_args(&self.command, audio_path)?;
        info!(prog = %prog, "STT (command): transcribing");

        let output = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            tokio::process::Command::new(&prog)
                .args(&args)
                .output(),
        )
        .await
        .map_err(|_| "STT command timeout (120s)".to_string())?;

        let output = output.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                format!("STT command not found: '{prog}'. Check [voice] stt_command in config.toml.")
            } else {
                format!("STT command spawn failed: {e}")
            }
        })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "STT command exited with {}: {}",
                output.status,
                duduclaw_core::truncate_bytes(stderr.trim(), 300)
            ));
        }

        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        info!(chars = text.chars().count(), "STT (command): done");
        Ok(text)
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_provider_kind_maps_known_values() {
        assert_eq!(
            parse_provider_kind("openai_compat"),
            Some(SttProviderKind::OpenAiCompat)
        );
        assert_eq!(
            parse_provider_kind("whisper-api"),
            Some(SttProviderKind::OpenAiCompat)
        );
        assert_eq!(
            parse_provider_kind("command"),
            Some(SttProviderKind::Command)
        );
        assert_eq!(
            parse_provider_kind("  WHISPER-LOCAL "),
            Some(SttProviderKind::Command)
        );
        assert_eq!(parse_provider_kind("bogus"), None);
        assert_eq!(parse_provider_kind(""), None);
    }

    #[test]
    fn guess_audio_mime_by_extension() {
        assert_eq!(guess_audio_mime("voice.webm"), "audio/webm");
        assert_eq!(guess_audio_mime("a.WAV"), "audio/wav");
        assert_eq!(guess_audio_mime("clip.ogg"), "audio/ogg");
        assert_eq!(guess_audio_mime("noext"), "application/octet-stream");
    }

    #[test]
    fn check_audio_size_enforces_bounds() {
        assert!(check_audio_size(0, 10).is_err()); // empty rejected
        assert!(check_audio_size(11, 10).is_err()); // over budget rejected
        assert!(check_audio_size(10, 10).is_ok()); // exactly at limit ok
        assert!(check_audio_size(1, 10).is_ok());
    }

    #[test]
    fn build_command_args_substitutes_placeholder() {
        let (prog, args) =
            build_command_args("whisper-cli -m model.bin -f {audio} --txt", "/tmp/x.webm")
                .unwrap();
        assert_eq!(prog, "whisper-cli");
        assert_eq!(
            args,
            vec!["-m", "model.bin", "-f", "/tmp/x.webm", "--txt"]
        );
    }

    #[test]
    fn build_command_args_appends_when_no_placeholder() {
        let (prog, args) = build_command_args("transcribe --fast", "/tmp/y.wav").unwrap();
        assert_eq!(prog, "transcribe");
        assert_eq!(args, vec!["--fast", "/tmp/y.wav"]);
    }

    #[test]
    fn build_command_args_rejects_empty() {
        assert!(build_command_args("   ", "/tmp/x").is_err());
    }

    #[tokio::test]
    async fn build_provider_unset_returns_none() {
        // No config.toml in an empty temp home → unset (fail-closed None).
        let home = std::env::temp_dir().join(format!(
            "duduclaw-stt-test-{}",
            uuid::Uuid::new_v4().as_simple()
        ));
        std::fs::create_dir_all(&home).unwrap();
        let got = build_provider_from_config(&home).await.unwrap();
        assert!(got.is_none());

        // [voice] present but no stt_provider → still None.
        std::fs::write(home.join("config.toml"), "[voice]\ntts_voice = \"nova\"\n").unwrap();
        let got = build_provider_from_config(&home).await.unwrap();
        assert!(got.is_none());

        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn build_provider_unknown_kind_is_err() {
        let home = std::env::temp_dir().join(format!(
            "duduclaw-stt-test-{}",
            uuid::Uuid::new_v4().as_simple()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join("config.toml"),
            "[voice]\nstt_provider = \"bogus\"\n",
        )
        .unwrap();
        assert!(build_provider_from_config(&home).await.is_err());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn build_provider_openai_requires_base_url() {
        let home = std::env::temp_dir().join(format!(
            "duduclaw-stt-test-{}",
            uuid::Uuid::new_v4().as_simple()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join("config.toml"),
            "[voice]\nstt_provider = \"openai_compat\"\n",
        )
        .unwrap();
        assert!(build_provider_from_config(&home).await.is_err());

        // With a base_url it builds and names itself openai_compat.
        std::fs::write(
            home.join("config.toml"),
            "[voice]\nstt_provider = \"openai_compat\"\nstt_base_url = \"https://api.openai.com/v1\"\nstt_api_key = \"sk-test\"\n",
        )
        .unwrap();
        let got = build_provider_from_config(&home).await.unwrap();
        assert_eq!(got.map(|p| p.name().to_string()), Some("openai_compat".to_string()));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn build_provider_command_requires_command() {
        let home = std::env::temp_dir().join(format!(
            "duduclaw-stt-test-{}",
            uuid::Uuid::new_v4().as_simple()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join("config.toml"),
            "[voice]\nstt_provider = \"command\"\n",
        )
        .unwrap();
        assert!(build_provider_from_config(&home).await.is_err());

        std::fs::write(
            home.join("config.toml"),
            "[voice]\nstt_provider = \"command\"\nstt_command = \"whisper-cli -f {audio}\"\n",
        )
        .unwrap();
        let got = build_provider_from_config(&home).await.unwrap();
        assert_eq!(got.map(|p| p.name().to_string()), Some("command".to_string()));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn command_stt_missing_binary_is_clear_error() {
        let provider = CommandStt::new(
            "duduclaw-nonexistent-stt-binary-xyz {audio}".to_string(),
        );
        let err = provider
            .transcribe(b"fake audio bytes", "voice.webm", None)
            .await
            .unwrap_err();
        assert!(
            err.contains("not found"),
            "expected a not-found error, got: {err}"
        );
    }
}
