//! Deepgram streaming ASR provider.
//!
//! Uses Deepgram's WebSocket API for real-time speech-to-text with
//! extremely low latency (<300ms). Supports zh, en, ja, ko and 30+ languages.
//!
//! API: `wss://api.deepgram.com/v1/listen`
//! Pricing: $0.0043/min (Nova-3, pay-as-you-go)

use async_trait::async_trait;
use tokio::sync::mpsc;
use tracing::info;

use crate::error::InferenceError;
use crate::realtime_voice::{PartialTranscript, StreamingAsrProvider};

/// Deepgram streaming ASR provider.
pub struct DeepgramStreamingAsr {
    api_key: String,
    model: String,
    language: String,
}

impl DeepgramStreamingAsr {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            model: "nova-3".into(),
            language: "zh".into(),
        }
    }

    pub fn from_env() -> Result<Self, InferenceError> {
        let api_key = std::env::var("DEEPGRAM_API_KEY")
            .map_err(|_| InferenceError::Other("DEEPGRAM_API_KEY not set".into()))?;
        Ok(Self::new(api_key))
    }

    pub fn with_model(mut self, model: &str) -> Self {
        // Allowlist: alphanumeric + hyphen only (prevent URL parameter injection)
        let safe: String = model.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
        self.model = safe;
        self
    }

    pub fn with_language(mut self, lang: &str) -> Self {
        // Allowlist: alphanumeric + hyphen only
        let safe: String = lang.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
        self.language = safe;
        self
    }
}

impl Drop for DeepgramStreamingAsr {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.api_key.zeroize();
    }
}

#[async_trait]
impl StreamingAsrProvider for DeepgramStreamingAsr {
    async fn start_stream(
        &self,
        lang: &str,
    ) -> Result<(mpsc::Sender<Vec<f32>>, mpsc::Receiver<PartialTranscript>), InferenceError> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        // Sanitize caller-supplied lang (same allowlist as with_language)
        let sanitized_lang: String;
        let language = if lang.is_empty() {
            self.language.as_str()
        } else {
            sanitized_lang = lang.chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
                .collect();
            sanitized_lang.as_str()
        };
        let url = format!(
            "wss://api.deepgram.com/v1/listen?model={}&language={}&encoding=linear16&sample_rate=16000&channels=1&interim_results=true&punctuate=true",
            self.model, language
        );

        let request = tokio_tungstenite::tungstenite::http::Request::builder()
            .uri(&url)
            .header("Authorization", format!("Token {}", self.api_key))
            .header("Host", "api.deepgram.com")
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", tokio_tungstenite::tungstenite::handshake::client::generate_key())
            .body(())
            .map_err(|e| InferenceError::Other(format!("Deepgram request: {e}")))?;

        let (ws_stream, _) = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            tokio_tungstenite::connect_async(request),
        )
        .await
        .map_err(|_| InferenceError::Other("Deepgram connect timeout (10s)".into()))?
        .map_err(|e| InferenceError::Other(format!("Deepgram connect: {e}")))?;

        info!(model = %self.model, language, "Deepgram streaming ASR connected");

        let (mut ws_write, mut ws_read) = ws_stream.split();

        // Audio input channel: caller sends PCM f32 chunks
        let (audio_tx, mut audio_rx) = mpsc::channel::<Vec<f32>>(32);
        // Transcript output channel: we send partial transcripts
        let (transcript_tx, transcript_rx) = mpsc::channel::<PartialTranscript>(32);

        // Task: forward audio chunks to Deepgram as PCM i16 bytes
        let tx_clone = transcript_tx.clone();
        tokio::spawn(async move {
            while let Some(pcm_f32) = audio_rx.recv().await {
                // Convert f32 → i16 PCM bytes (Deepgram expects linear16)
                let mut pcm_bytes = Vec::with_capacity(pcm_f32.len() * 2);
                for &sample in &pcm_f32 {
                    let i16_val = (sample.clamp(-1.0, 1.0) * 32767.0) as i16;
                    pcm_bytes.extend_from_slice(&i16_val.to_le_bytes());
                }
                if ws_write.send(Message::Binary(pcm_bytes.into())).await.is_err() {
                    break;
                }
            }
            // Send close frame to signal end of audio
            let _ = ws_write.send(Message::Text(r#"{"type":"CloseStream"}"#.into())).await;
        });

        // Task: read transcripts from Deepgram (5 min max session)
        tokio::spawn(async move {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(300), async move {
                while let Some(Ok(msg)) = ws_read.next().await {
                    if let Message::Text(text) = msg
                        && let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                            let is_final = json.get("is_final").and_then(|v| v.as_bool()).unwrap_or(false);
                            let channel = json.get("channel").and_then(|c| c.get("alternatives"))
                                .and_then(|a| a.get(0));

                            if let Some(alt) = channel {
                                let transcript = alt.get("transcript")
                                    .and_then(|t| t.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let confidence = alt.get("confidence")
                                    .and_then(|c| c.as_f64())
                                    .unwrap_or(0.0) as f32;

                                if !transcript.is_empty() {
                                    let lang_detected = json.get("metadata")
                                        .and_then(|m| m.get("detected_language"))
                                        .and_then(|l| l.as_str())
                                        .map(String::from);

                                    let partial = PartialTranscript {
                                        text: transcript,
                                        is_final,
                                        confidence,
                                        language: lang_detected,
                                    };

                                    if tx_clone.send(partial).await.is_err() {
                                        break;
                                    }
                                }
                            }
                        }
                }
            }).await;
            // Timeout or normal end — tx_clone dropped here, notifying receiver
        });

        Ok((audio_tx, transcript_rx))
    }

    fn name(&self) -> &str {
        "deepgram"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_name() {
        let provider = DeepgramStreamingAsr::new("test-key".into());
        assert_eq!(provider.name(), "deepgram");
    }

    #[test]
    fn default_config() {
        let provider = DeepgramStreamingAsr::new("key".into())
            .with_model("nova-3")
            .with_language("en");
        assert_eq!(provider.model, "nova-3");
        assert_eq!(provider.language, "en");
    }
}
