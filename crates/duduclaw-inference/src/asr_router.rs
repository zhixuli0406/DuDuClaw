//! ASR Router — routes transcription requests to the best available provider.
//!
//! Priority: SenseVoice (local ONNX, zh-TW best) → Whisper Local → Whisper API (cloud fallback)
//!
//! Applies Silero VAD pre-processing when available to filter silence.

use async_trait::async_trait;
use tracing::{info, warn};

use crate::asr::AsrProvider;
use crate::error::InferenceError;

/// ASR routing strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsrStrategy {
    /// Use the best available local provider, cloud as fallback.
    LocalFirst,
    /// Always use cloud (Whisper API).
    CloudOnly,
    /// Always use local (fail if unavailable).
    LocalOnly,
}

/// ASR Router that dispatches to the best available provider.
pub struct AsrRouter {
    providers: Vec<Box<dyn AsrProvider>>,
    #[allow(dead_code)]
    strategy: AsrStrategy,
    #[cfg(feature = "onnx")]
    vad: Option<crate::vad::SileroVad>,
    #[cfg(not(feature = "onnx"))]
    _no_vad: (),
}

impl AsrRouter {
    /// Create a new router with the given strategy.
    pub fn new(strategy: AsrStrategy) -> Self {
        Self {
            providers: Vec::new(),
            strategy,
            #[cfg(feature = "onnx")]
            vad: None,
            #[cfg(not(feature = "onnx"))]
            _no_vad: (),
        }
    }

    /// Add a provider to the router (order matters — first added = highest priority).
    pub fn add_provider(mut self, provider: Box<dyn AsrProvider>) -> Self {
        info!(provider = provider.name(), "ASR Router: registered provider");
        self.providers.push(provider);
        self
    }

    /// Set the VAD pre-processor (requires `onnx` feature).
    #[cfg(feature = "onnx")]
    pub fn with_vad(mut self, vad: crate::vad::SileroVad) -> Self {
        self.vad = Some(vad);
        self
    }

    /// Build a router with all available providers auto-detected.
    ///
    /// Checks for local models in `models_dir`, cloud API keys in env.
    pub fn auto_detect(_models_dir: &std::path::Path, strategy: AsrStrategy) -> Self {
        let mut router = Self::new(strategy);

        // Try SenseVoice (local ONNX)
        #[cfg(feature = "onnx")]
        {
            let sv_path = models_dir.join("sensevoice").join("sensevoice-small.onnx");
            if sv_path.exists() {
                match crate::sensevoice::SenseVoiceProvider::load(sv_path.to_str().unwrap_or("")) {
                    Ok(provider) => {
                        info!("ASR Router: SenseVoice loaded");
                        router.providers.push(Box::new(provider));
                    }
                    Err(e) => warn!("ASR Router: SenseVoice load failed: {e}"),
                }
            }

            // Try Silero VAD
            let vad_path = models_dir.join("silero-vad").join("silero_vad.onnx");
            if vad_path.exists() {
                match crate::vad::SileroVad::load(vad_path.to_str().unwrap_or("")) {
                    Ok(vad) => {
                        info!("ASR Router: Silero VAD loaded");
                        router.vad = Some(vad);
                    }
                    Err(e) => warn!("ASR Router: Silero VAD load failed: {e}"),
                }
            }
        }

        // Try Whisper Local (whisper.cpp)
        #[cfg(feature = "whisper")]
        {
            if let Some(provider) = crate::asr::WhisperLocalProvider::from_models_dir(models_dir) {
                info!("ASR Router: Whisper Local loaded");
                router.providers.push(Box::new(provider));
            }
        }

        // Try Whisper API (cloud)
        if strategy != AsrStrategy::LocalOnly
            && let Ok(provider) = crate::asr::WhisperApiProvider::from_env() {
                info!("ASR Router: Whisper API available");
                router.providers.push(Box::new(provider));
            }

        if router.providers.is_empty() {
            warn!("ASR Router: no providers available — transcription will fail");
        }

        router
    }

    /// Apply VAD pre-processing. Returns speech-only PCM (owned) or borrows full input.
    fn apply_vad<'a>(&self, pcm_data: &'a [f32]) -> std::borrow::Cow<'a, [f32]> {
        #[cfg(feature = "onnx")]
        {
            if let Some(vad) = &self.vad {
                match vad.detect_speech(pcm_data) {
                    Ok(segments) if !segments.is_empty() => {
                        let total_speech: f32 = segments.iter().map(|s| s.end - s.start).sum();
                        info!(
                            segments = segments.len(),
                            speech_secs = total_speech,
                            "VAD: filtered to speech segments"
                        );
                        let filtered = crate::vad::SileroVad::extract_speech(pcm_data, &segments);
                        return std::borrow::Cow::Owned(filtered);
                    }
                    Ok(_) => {
                        warn!("VAD: no speech detected, using full audio");
                    }
                    Err(e) => {
                        warn!("VAD failed (using full audio): {e}");
                    }
                }
            }
        }
        std::borrow::Cow::Borrowed(pcm_data)
    }
}

#[async_trait]
impl AsrProvider for AsrRouter {
    async fn transcribe(&self, pcm_data: &[f32], lang: &str) -> Result<String, InferenceError> {
        if self.providers.is_empty() {
            return Err(InferenceError::Other("No ASR providers configured".into()));
        }

        // Apply VAD pre-processing if available (onnx feature only)
        let input = self.apply_vad(pcm_data);

        // Try providers in order
        let mut last_error = None;
        for provider in &self.providers {
            match provider.transcribe(&input, lang).await {
                Ok(text) if !text.trim().is_empty() => {
                    info!(provider = provider.name(), "ASR: transcription succeeded");
                    return Ok(text);
                }
                Ok(_) => {
                    warn!(provider = provider.name(), "ASR: returned empty text, trying next");
                    last_error = Some(InferenceError::Other("Empty transcription".into()));
                }
                Err(e) => {
                    warn!(provider = provider.name(), error = %e, "ASR: failed, trying next");
                    last_error = Some(e);
                }
            }
        }

        Err(last_error.unwrap_or_else(|| InferenceError::Other("All ASR providers failed".into())))
    }

    fn name(&self) -> &str {
        "asr-router"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_router_fails() {
        let router = AsrRouter::new(AsrStrategy::CloudOnly);
        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(router.transcribe(&[0.0; 16000], "zh"));
        assert!(result.is_err());
    }
}
