//! ASR (Automatic Speech Recognition) provider abstraction.
//!
//! Defines the `AsrProvider` trait and concrete implementations:
//! - `WhisperApiProvider` — OpenAI Whisper API (cloud)
//! - `WhisperLocalProvider` — whisper.cpp via whisper-rs (local, feature-gated)
//!
//! Usage:
//! ```ignore
//! let provider = WhisperApiProvider::new();
//! let pcm = audio_decode::decode_to_pcm(&ogg_bytes)?;
//! let text = provider.transcribe(&pcm, "zh").await?;
//! ```

use async_trait::async_trait;

use crate::error::InferenceError;

/// ASR provider trait — transcribes PCM audio to text.
#[async_trait]
pub trait AsrProvider: Send + Sync {
    /// Transcribe PCM f32 mono 16kHz audio to text.
    ///
    /// `lang` is a BCP-47 language hint (e.g., "zh", "en", "ja").
    async fn transcribe(&self, pcm_data: &[f32], lang: &str) -> Result<String, InferenceError>;

    /// Provider name for logging and metrics.
    fn name(&self) -> &str;
}

/// OpenAI Whisper API provider (cloud).
pub struct WhisperApiProvider {
    api_key: String,
    client: reqwest::Client,
}

impl Drop for WhisperApiProvider {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.api_key.zeroize();
    }
}

impl WhisperApiProvider {
    /// Create from environment variable `OPENAI_API_KEY`.
    pub fn from_env() -> Result<Self, InferenceError> {
        let api_key = std::env::var("OPENAI_API_KEY")
            .map_err(|_| InferenceError::Other("OPENAI_API_KEY not set".into()))?;
        Ok(Self {
            api_key,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .build()
                .unwrap_or_default(),
        })
    }

    /// Create with explicit API key.
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .build()
                .unwrap_or_default(),
        }
    }
}

#[async_trait]
impl AsrProvider for WhisperApiProvider {
    async fn transcribe(&self, pcm_data: &[f32], lang: &str) -> Result<String, InferenceError> {
        // Convert PCM f32 to WAV bytes for the API
        let wav_bytes = pcm_to_wav(pcm_data, 16000);

        let part = reqwest::multipart::Part::bytes(wav_bytes)
            .file_name("audio.wav")
            .mime_str("audio/wav")
            .map_err(|e| InferenceError::Other(format!("Multipart: {e}")))?;
        let form = reqwest::multipart::Form::new()
            .text("model", "whisper-1")
            .text("language", lang.to_string())
            .part("file", part);

        let resp = self.client
            .post("https://api.openai.com/v1/audio/transcriptions")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .multipart(form)
            .send()
            .await
            .map_err(|e| InferenceError::Other(format!("Whisper API: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            tracing::warn!(status = %status, body_len = body.len(), "Whisper API error");
            return Err(InferenceError::Other(format!("ASR API error: {status}")));
        }

        let result: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| InferenceError::Other(e.to_string()))?;
        result
            .get("text")
            .and_then(|t| t.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| InferenceError::Other("No text in response".into()))
    }

    fn name(&self) -> &str {
        "whisper-api"
    }
}

/// Encode PCM f32 mono samples as a minimal WAV file.
fn pcm_to_wav(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    let num_samples = samples.len();
    let bytes_per_sample = 2u16; // 16-bit PCM
    let data_size = (num_samples * bytes_per_sample as usize) as u32;
    let file_size = 36 + data_size;

    let mut buf = Vec::with_capacity(44 + data_size as usize);

    // RIFF header
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&file_size.to_le_bytes());
    buf.extend_from_slice(b"WAVE");

    // fmt chunk
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes()); // chunk size
    buf.extend_from_slice(&1u16.to_le_bytes()); // PCM format
    buf.extend_from_slice(&1u16.to_le_bytes()); // mono
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&(sample_rate * bytes_per_sample as u32).to_le_bytes()); // byte rate
    buf.extend_from_slice(&bytes_per_sample.to_le_bytes()); // block align
    buf.extend_from_slice(&16u16.to_le_bytes()); // bits per sample

    // data chunk
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_size.to_le_bytes());

    // Convert f32 → i16
    for &sample in samples {
        let clamped = sample.clamp(-1.0, 1.0);
        let i16_val = (clamped * 32767.0) as i16;
        buf.extend_from_slice(&i16_val.to_le_bytes());
    }

    buf
}

// ── Whisper Local Provider (whisper.cpp) ─────────────────────────

/// Local Whisper ASR via whisper.cpp (whisper-rs crate).
///
/// Requires the `whisper` feature flag and a GGUF model file.
/// Model files stored in `~/.duduclaw/models/whisper/`.
#[cfg(feature = "whisper")]
pub struct WhisperLocalProvider {
    model_path: std::path::PathBuf,
}

#[cfg(feature = "whisper")]
impl WhisperLocalProvider {
    /// Load a Whisper model from the given path.
    ///
    /// Recommended models:
    /// - `ggml-base.bin` (~142MB, fast, WER ~18% zh)
    /// - `ggml-large-v3-turbo.bin` (~800MB, best balance)
    /// - `ggml-large-v3.bin` (~1.5GB, best quality)
    pub fn new(model_path: std::path::PathBuf) -> Result<Self, InferenceError> {
        if model_path.to_str().is_some_and(|p| p.contains("..") || p.contains('\0')) {
            return Err(InferenceError::Other("Unsafe model path".into()));
        }
        if !model_path.exists() {
            return Err(InferenceError::Other(format!(
                "Whisper model not found: {}",
                model_path.display()
            )));
        }
        Ok(Self { model_path })
    }

    /// Auto-detect from models directory.
    pub fn from_models_dir(models_dir: &std::path::Path) -> Option<Self> {
        let whisper_dir = models_dir.join("whisper");
        // Find any .bin model file
        let model = std::fs::read_dir(&whisper_dir).ok()?
            .filter_map(|e| e.ok())
            .find(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                name.starts_with("ggml-") && name.ends_with(".bin")
            })?
            .path();
        Self::new(model).ok()
    }
}

#[cfg(feature = "whisper")]
#[async_trait]
impl AsrProvider for WhisperLocalProvider {
    async fn transcribe(&self, pcm_data: &[f32], lang: &str) -> Result<String, InferenceError> {
        use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

        if pcm_data.is_empty() {
            return Err(InferenceError::Other("Empty audio input".into()));
        }

        let model_path = self.model_path.clone();
        let pcm = pcm_data.to_vec();
        let language = lang.to_string();

        // Run whisper inference on a blocking thread with 120s timeout
        let text = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            tokio::task::spawn_blocking(move || -> Result<String, InferenceError> {
                let ctx = WhisperContext::new_with_params(
                    model_path.to_str().ok_or_else(|| InferenceError::Other("Invalid path".into()))?,
                    WhisperContextParameters::default(),
                )
                .map_err(|e| InferenceError::Other(format!("Load whisper model: {e}")))?;

                let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
                params.set_language(Some(&language));
                params.set_print_special(false);
                params.set_print_progress(false);
                params.set_print_realtime(false);
                params.set_print_timestamps(false);

                let mut state = ctx.create_state()
                    .map_err(|e| InferenceError::Other(format!("Whisper state: {e}")))?;
                state.full(params, &pcm)
                    .map_err(|e| InferenceError::Other(format!("Whisper transcribe: {e}")))?;

                let n = state.full_n_segments()
                    .map_err(|e| InferenceError::Other(format!("Segments: {e}")))?;
                let mut text = String::new();
                for i in 0..n {
                    if let Ok(seg) = state.full_get_segment_text(i) {
                        text.push_str(&seg);
                    }
                }
                Ok(text.trim().to_string())
            }),
        )
        .await
        .map_err(|_| InferenceError::Other("Whisper local timeout (120s)".into()))?
        .map_err(|e| InferenceError::Other(format!("Whisper task: {e}")))?;

        text
    }

    fn name(&self) -> &str {
        "whisper-local"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header_is_valid() {
        let samples = vec![0.0f32; 16000]; // 1 second of silence
        let wav = pcm_to_wav(&samples, 16000);

        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(&wav[36..40], b"data");
        // Data size = 16000 samples * 2 bytes = 32000
        let data_size = u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]);
        assert_eq!(data_size, 32000);
    }

    #[test]
    fn whisper_api_provider_name() {
        let provider = WhisperApiProvider::new("test".into());
        assert_eq!(provider.name(), "whisper-api");
    }
}
