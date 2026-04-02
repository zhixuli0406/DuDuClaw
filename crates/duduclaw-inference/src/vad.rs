//! Voice Activity Detection (VAD) using Silero VAD ONNX model.
//!
//! Filters silence from audio before sending to ASR, reducing
//! processing time and improving accuracy. Model is only ~2MB.
//!
//! Requires the `onnx` feature flag.

use tracing::{debug, info};

use crate::error::InferenceError;

/// A detected speech segment with start/end timestamps.
#[derive(Debug, Clone)]
pub struct SpeechSegment {
    /// Start time in seconds.
    pub start: f32,
    /// End time in seconds.
    pub end: f32,
    /// Average speech probability in this segment.
    pub confidence: f32,
}

/// Silero VAD configuration.
pub struct VadConfig {
    /// Probability threshold for speech detection (default: 0.5).
    pub threshold: f32,
    /// Minimum speech segment duration in seconds (default: 0.25).
    pub min_speech_duration: f32,
    /// Minimum silence duration to split segments in seconds (default: 0.5).
    pub min_silence_duration: f32,
    /// Window size in samples for Silero VAD (512 for 16kHz).
    pub window_size: usize,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            threshold: 0.5,
            min_speech_duration: 0.25,
            min_silence_duration: 0.5,
            window_size: 512,
        }
    }
}

/// Silero VAD processor with LSTM state tracking.
pub struct SileroVad {
    #[cfg(feature = "onnx")]
    session: ort::session::Session,
    /// LSTM hidden state (h) — persisted across windows within a detect_speech call.
    /// Shape: [2, 1, 64] for Silero VAD v3/v4.
    #[cfg(feature = "onnx")]
    h_state: std::sync::Mutex<Vec<f32>>,
    /// LSTM cell state (c) — same shape as h_state.
    #[cfg(feature = "onnx")]
    c_state: std::sync::Mutex<Vec<f32>>,
    config: VadConfig,
}

impl SileroVad {
    /// Load the Silero VAD ONNX model.
    pub fn load(model_path: &str) -> Result<Self, InferenceError> {
        Self::load_with_config(model_path, VadConfig::default())
    }

    /// Load with custom config.
    pub fn load_with_config(model_path: &str, config: VadConfig) -> Result<Self, InferenceError> {
        // Path traversal protection
        if model_path.contains("..") || model_path.contains('\0') {
            return Err(InferenceError::Other(format!("Unsafe model path: {model_path}")));
        }

        #[cfg(feature = "onnx")]
        {
            info!(model = model_path, "Loading Silero VAD model");
            let session = ort::session::Session::builder()
                .map_err(|e| InferenceError::Other(format!("ONNX session: {e}")))?
                .with_intra_threads(1)
                .map_err(|e| InferenceError::Other(format!("ONNX threads: {e}")))?
                .commit_from_file(model_path)
                .map_err(|e| InferenceError::Other(format!("Load VAD model: {e}")))?;
            info!("Silero VAD model loaded");
            // LSTM state: [2, 1, 64] = 128 floats, initialized to zero
            let state_size = 2 * 1 * 64;
            Ok(Self {
                session,
                h_state: std::sync::Mutex::new(vec![0.0f32; state_size]),
                c_state: std::sync::Mutex::new(vec![0.0f32; state_size]),
                config,
            })
        }

        #[cfg(not(feature = "onnx"))]
        {
            let _ = (model_path, config);
            Err(InferenceError::Other("VAD requires the 'onnx' feature flag".into()))
        }
    }

    /// Detect speech segments in PCM f32 mono 16kHz audio.
    ///
    /// Returns a list of speech segments with timestamps and confidence.
    /// If no speech is detected, returns an empty list.
    pub fn detect_speech(&self, pcm_data: &[f32]) -> Result<Vec<SpeechSegment>, InferenceError> {
        if pcm_data.is_empty() {
            return Ok(Vec::new());
        }

        // Reset LSTM state for each new audio segment
        #[cfg(feature = "onnx")]
        {
            let state_size = 2 * 1 * 64;
            *self.h_state.lock().unwrap_or_else(|e| e.into_inner()) = vec![0.0f32; state_size];
            *self.c_state.lock().unwrap_or_else(|e| e.into_inner()) = vec![0.0f32; state_size];
        }

        let sample_rate = 16000.0f32;
        let window = self.config.window_size;
        let mut segments = Vec::new();
        let mut in_speech = false;
        let mut speech_start = 0.0f32;
        let mut silence_start = 0.0f32;
        let mut speech_probs = Vec::new();

        // Process audio in windows
        let num_windows = pcm_data.len() / window;
        for i in 0..num_windows {
            let start = i * window;
            let chunk = &pcm_data[start..start + window];
            let prob = self.predict_chunk(chunk)?;
            let time = start as f32 / sample_rate;

            if prob >= self.config.threshold {
                if !in_speech {
                    speech_start = time;
                    in_speech = true;
                    speech_probs.clear();
                }
                speech_probs.push(prob);
                silence_start = time;
            } else if in_speech {
                let silence_duration = time - silence_start;
                if silence_duration >= self.config.min_silence_duration {
                    let speech_duration = silence_start - speech_start;
                    if speech_duration >= self.config.min_speech_duration {
                        let avg_confidence =
                            speech_probs.iter().sum::<f32>() / speech_probs.len().max(1) as f32;
                        segments.push(SpeechSegment {
                            start: speech_start,
                            end: silence_start,
                            confidence: avg_confidence,
                        });
                    }
                    in_speech = false;
                }
            }
        }

        // Close final segment if still in speech
        if in_speech {
            let end_time = pcm_data.len() as f32 / sample_rate;
            let speech_duration = end_time - speech_start;
            if speech_duration >= self.config.min_speech_duration {
                let avg_confidence =
                    speech_probs.iter().sum::<f32>() / speech_probs.len().max(1) as f32;
                segments.push(SpeechSegment {
                    start: speech_start,
                    end: end_time,
                    confidence: avg_confidence,
                });
            }
        }

        debug!(segments = segments.len(), "VAD: detected speech segments");
        Ok(segments)
    }

    /// Extract only the speech portions from PCM audio based on VAD segments.
    pub fn extract_speech(
        pcm_data: &[f32],
        segments: &[SpeechSegment],
    ) -> Vec<f32> {
        let sample_rate = 16000usize;
        let mut speech = Vec::new();
        for seg in segments {
            let start = (seg.start * sample_rate as f32) as usize;
            let end = (seg.end * sample_rate as f32).min(pcm_data.len() as f32) as usize;
            if start < end && end <= pcm_data.len() {
                speech.extend_from_slice(&pcm_data[start..end]);
            }
        }
        speech
    }

    /// Predict speech probability for a single audio chunk.
    fn predict_chunk(&self, _chunk: &[f32]) -> Result<f32, InferenceError> {
        #[cfg(feature = "onnx")]
        {
            // Silero VAD expects [1, window_size] tensor
            let input = ndarray::Array2::from_shape_vec(
                (1, _chunk.len()),
                _chunk.to_vec(),
            )
            .map_err(|e| InferenceError::Other(format!("VAD tensor: {e}")))?
            .into_dyn();

            let input_val = ort::value::Value::from_array(
                ort::memory::Allocator::default()
                    .map_err(|e| InferenceError::Other(format!("Allocator: {e}")))?,
                &input,
            )
            .map_err(|e| InferenceError::Other(format!("VAD input: {e}")))?;

            // Sample rate tensor
            let sr = ndarray::Array1::from_vec(vec![16000i64]).into_dyn();
            let sr_val = ort::value::Value::from_array(
                ort::memory::Allocator::default()
                    .map_err(|e| InferenceError::Other(format!("Allocator: {e}")))?,
                &sr,
            )
            .map_err(|e| InferenceError::Other(format!("SR input: {e}")))?;

            // LSTM hidden state (h) — [2, 1, 64]
            let h_data = self.h_state.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let h = ndarray::Array3::from_shape_vec((2, 1, 64), h_data)
                .map_err(|e| InferenceError::Other(format!("h state: {e}")))?
                .into_dyn();
            let h_val = ort::value::Value::from_array(
                ort::memory::Allocator::default()
                    .map_err(|e| InferenceError::Other(format!("Allocator: {e}")))?,
                &h,
            )
            .map_err(|e| InferenceError::Other(format!("h input: {e}")))?;

            // LSTM cell state (c) — [2, 1, 64]
            let c_data = self.c_state.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let c = ndarray::Array3::from_shape_vec((2, 1, 64), c_data)
                .map_err(|e| InferenceError::Other(format!("c state: {e}")))?
                .into_dyn();
            let c_val = ort::value::Value::from_array(
                ort::memory::Allocator::default()
                    .map_err(|e| InferenceError::Other(format!("Allocator: {e}")))?,
                &c,
            )
            .map_err(|e| InferenceError::Other(format!("c input: {e}")))?;

            let outputs = self.session
                .run(ort::inputs![input_val, sr_val, h_val, c_val]
                    .map_err(|e| InferenceError::Other(format!("VAD inputs: {e}")))?)
                .map_err(|e| InferenceError::Other(format!("VAD inference: {e}")))?;

            // Extract probability (output 0)
            let prob_tensor = outputs.get(0)
                .ok_or_else(|| InferenceError::Other("No VAD output".into()))?
                .try_extract_raw_tensor::<f32>()
                .map_err(|e| InferenceError::Other(format!("VAD extract: {e}")))?;

            // Update LSTM states for next window (output 1 = hn, output 2 = cn)
            if let Some(hn) = outputs.get(1) {
                if let Ok((_, hn_data)) = hn.try_extract_raw_tensor::<f32>() {
                    *self.h_state.lock().unwrap_or_else(|e| e.into_inner()) = hn_data.to_vec();
                }
            }
            if let Some(cn) = outputs.get(2) {
                if let Ok((_, cn_data)) = cn.try_extract_raw_tensor::<f32>() {
                    *self.c_state.lock().unwrap_or_else(|e| e.into_inner()) = cn_data.to_vec();
                }
            }

            Ok(prob_tensor.1.first().copied().unwrap_or(0.0))
        }

        #[cfg(not(feature = "onnx"))]
        {
            Err(InferenceError::Other("VAD requires 'onnx' feature".into()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_speech_empty() {
        let result = SileroVad::extract_speech(&[], &[]);
        assert!(result.is_empty());
    }

    #[test]
    fn extract_speech_segments() {
        // 2 seconds of audio at 16kHz
        let pcm = vec![0.5f32; 32000];
        let segments = vec![
            SpeechSegment { start: 0.0, end: 0.5, confidence: 0.9 },
            SpeechSegment { start: 1.0, end: 1.5, confidence: 0.8 },
        ];
        let speech = SileroVad::extract_speech(&pcm, &segments);
        // 0.5s + 0.5s = 1.0s = 16000 samples
        assert_eq!(speech.len(), 16000);
    }

    #[test]
    fn default_config() {
        let cfg = VadConfig::default();
        assert_eq!(cfg.threshold, 0.5);
        assert_eq!(cfg.window_size, 512);
    }
}
