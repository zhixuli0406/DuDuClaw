//! Provider-independent meeting transcription. Provider credentials are supplied by the caller.

pub mod config;
pub mod gemini;
pub mod google_stt;
pub mod pipeline;
pub mod whisper;

use std::{fmt, path::Path};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub use gemini::GeminiEngine;
pub use google_stt::GoogleSttEngine;
pub use whisper::WhisperEngine;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Segment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub speaker: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transcript {
    pub segments: Vec<Segment>,
    pub language: String,
}

/// One provider request, including an unsuccessful JSON-repair attempt.
/// Output tokens exclude reasoning tokens; audio input tokens are a subset of input tokens.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TranscriptionUsage {
    pub model: String,
    pub input_tokens: u64,
    pub input_audio_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub audio_seconds: Option<f64>,
    /// Provider usage was missing or implausible; values are local estimates.
    #[serde(default)]
    pub estimated: bool,
}

/// Validate usage against locally probed audio, never provider duration. Without
/// ffprobe, use the maximum supported chunk as an explicitly estimated ceiling.
pub(crate) fn bounded_usage(item: &TranscriptionUsage, duration_ms: Option<u64>) -> TranscriptionUsage {
    let mut usage = item.clone();
    let seconds = duration_ms.unwrap_or(pipeline::CHUNK_SECONDS * 1000).saturating_add(999) / 1000;
    let seconds = seconds.min(pipeline::CHUNK_SECONDS);
    if matches!(item.model.as_str(), "openai/whisper-1" | "google-stt/chirp_2") {
        let local_seconds = duration_ms.map_or(seconds as f64, |ms| ms.min(pipeline::CHUNK_SECONDS * 1000) as f64 / 1000.0);
        // Provider duration is untrusted, including plausible but tiny values.
        // Always price the audio measured locally (or the fallback ceiling).
        usage.audio_seconds = Some(local_seconds);
        usage.estimated |= duration_ms.is_none()
            || !item.audio_seconds.is_some_and(|s| s.is_finite() && (s - local_seconds).abs() < 0.000_001);
        usage.input_tokens = 0;
        usage.input_audio_tokens = 0;
        usage.output_tokens = 0;
        usage.reasoning_tokens = 0;
        return usage;
    }
    // 32 audio tokens/s, 2x tolerance, plus a bounded language/speaker prompt.
    let cap = seconds.saturating_mul(64).saturating_add(16_384);
    // Even valid-looking usage cannot fall below half the 32 audio tokens/s
    // estimate. Round upward so subsecond recordings retain a nonzero floor.
    let audio_floor = duration_ms.unwrap_or(pipeline::CHUNK_SECONDS * 1000)
        .min(pipeline::CHUNK_SECONDS * 1000).saturating_mul(16).saturating_add(999) / 1000;
    let total = item.input_tokens.checked_add(item.output_tokens).and_then(|n| n.checked_add(item.reasoning_tokens));
    if item.estimated || duration_ms.is_none() || total.is_none_or(|n| n == 0 || n > cap)
        || item.input_audio_tokens < audio_floor || item.input_tokens < audio_floor
        || item.input_audio_tokens > item.input_tokens || item.input_audio_tokens > seconds.saturating_mul(64)
        || item.output_tokens > 32_768 {
        usage.input_audio_tokens = seconds.saturating_mul(32);
        usage.input_tokens = usage.input_audio_tokens.saturating_add(4096);
        usage.output_tokens = seconds.saturating_mul(8).min(32_768);
        usage.reasoning_tokens = 0;
        usage.audio_seconds = None;
        usage.estimated = true;
    }
    usage
}

#[derive(Debug, Clone)]
pub struct TranscriptionResult {
    pub transcript: Transcript,
    pub usage: Vec<TranscriptionUsage>,
}

#[derive(Debug, Clone)]
pub struct TranscriptionError {
    pub message: String,
    // Internal diagnostics only; Display and user-facing payloads use message.
    pub(crate) detail: Option<String>,
    pub usage: Vec<TranscriptionUsage>,
}

impl TranscriptionError {
    pub fn new(message: impl Into<String>) -> Self {
        Self { message: message.into(), detail: None, usage: Vec::new() }
    }
}

impl fmt::Display for TranscriptionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.message) }
}

impl std::error::Error for TranscriptionError {}

#[async_trait]
pub trait TranscriptionEngine: Send + Sync {
    async fn transcribe(
        &self,
        audio_path: &Path,
        language_hint: &str,
        speaker_context: &[String],
    ) -> Result<TranscriptionResult, TranscriptionError>;
}

pub(crate) fn audio_mime(path: &Path) -> Result<&'static str, TranscriptionError> {
    match path.extension().and_then(|v| v.to_str()).unwrap_or("").to_ascii_lowercase().as_str() {
        "mp3" | "mpga" | "mpeg" => Ok("audio/mpeg"),
        "m4a" | "mp4" => Ok("audio/mp4"),
        "wav" => Ok("audio/wav"),
        "flac" => Ok("audio/flac"),
        "ogg" | "opus" => Ok("audio/ogg"),
        "aac" => Ok("audio/aac"),
        "aiff" | "aif" => Ok("audio/aiff"),
        "webm" => Ok("audio/webm"),
        _ => Err(TranscriptionError::new("不支援此音訊格式，請先使用 ffmpeg 轉成 M4A。")),
    }
}

pub(crate) fn http_client() -> Result<reqwest::Client, TranscriptionError> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(30))
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(|_| TranscriptionError::new("無法建立轉錄連線。"))
}

pub(crate) async fn bounded_json(mut response: reqwest::Response) -> Result<serde_json::Value, TranscriptionError> {
    const LIMIT: usize = 8 * 1024 * 1024;
    if !response.status().is_success() {
        return Err(TranscriptionError::new(format!("轉錄服務回應 HTTP {}。", response.status().as_u16())));
    }
    if response.content_length().is_some_and(|n| n > LIMIT as u64) {
        return Err(TranscriptionError::new("轉錄服務回應過大。"));
    }
    let mut data = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| TranscriptionError::new("讀取轉錄服務回應失敗。"))? {
        if data.len().saturating_add(chunk.len()) > LIMIT {
            return Err(TranscriptionError::new("轉錄服務回應過大。"));
        }
        data.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&data).map_err(|_| TranscriptionError::new("轉錄服務回應不是有效的 JSON。"))
}

#[cfg(test)]
mod tests;
