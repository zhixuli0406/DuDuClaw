//! Realtime voice conversation architecture.
//!
//! Defines the traits and types for streaming ASR + TTS with interruption
//! support. Designed for future integration with LiveKit, Deepgram WebSocket,
//! and Discord Voice Channels.
//!
//! This module is the architecture blueprint — concrete implementations
//! will be added as the infrastructure matures.

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::error::InferenceError;

// ── Streaming ASR ───────────────────────────────────────────────

/// A partial transcription result from streaming ASR.
#[derive(Debug, Clone)]
pub struct PartialTranscript {
    /// Partially recognized text (may change as more audio arrives).
    pub text: String,
    /// Whether this is a final result (won't change).
    pub is_final: bool,
    /// Confidence score (0.0-1.0).
    pub confidence: f32,
    /// Language detected.
    pub language: Option<String>,
}

/// Streaming ASR provider — receives audio chunks, emits transcripts.
#[async_trait]
pub trait StreamingAsrProvider: Send + Sync {
    /// Start a new streaming session. Returns a sender for audio chunks
    /// and a receiver for transcript events.
    async fn start_stream(
        &self,
        lang: &str,
    ) -> Result<
        (
            mpsc::Sender<Vec<f32>>,        // Send PCM chunks (f32 mono 16kHz)
            mpsc::Receiver<PartialTranscript>, // Receive transcripts
        ),
        InferenceError,
    >;

    fn name(&self) -> &str;
}

// ── Streaming TTS ───────────────────────────────────────────────

/// A chunk of synthesized audio from streaming TTS.
#[derive(Debug, Clone)]
pub struct AudioChunk {
    /// Raw audio bytes (MP3 or PCM).
    pub data: Vec<u8>,
    /// Format of the audio data.
    pub format: AudioFormat,
    /// Whether this is the last chunk.
    pub is_final: bool,
}

/// Audio format for streamed chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFormat {
    Pcm16kHz,
    Pcm22kHz,
    Pcm44kHz,
    Pcm48kHz,
    Mp3,
    OggOpus,
}

/// Streaming TTS provider — receives text, emits audio chunks.
#[async_trait]
pub trait StreamingTtsProvider: Send + Sync {
    /// Start synthesizing text into audio chunks.
    async fn start_synthesis(
        &self,
        text: &str,
        voice: &str,
    ) -> Result<mpsc::Receiver<AudioChunk>, InferenceError>;

    /// Interrupt an ongoing synthesis (for barge-in support).
    async fn interrupt(&self) -> Result<(), InferenceError>;

    fn name(&self) -> &str;
}

// ── Voice Session ───────────────────────────────────────────────

/// State of a realtime voice conversation session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceSessionState {
    /// Waiting for user to speak.
    Listening,
    /// User is speaking, ASR processing.
    Recognizing,
    /// AI is thinking (LLM inference).
    Thinking,
    /// AI is speaking (TTS playback).
    Speaking,
    /// Session paused or disconnected.
    Idle,
}

/// Events emitted by a voice session.
#[derive(Debug, Clone)]
pub enum VoiceEvent {
    /// User started speaking.
    SpeechStart,
    /// Partial transcription available.
    Transcript(PartialTranscript),
    /// Final user input ready.
    UserInputComplete(String),
    /// AI response text ready.
    AiResponseReady(String),
    /// Audio chunk ready for playback.
    AudioReady(AudioChunk),
    /// AI finished speaking.
    SpeechEnd,
    /// User interrupted AI (barge-in).
    BargeIn,
    /// State changed.
    StateChanged(VoiceSessionState),
    /// Error occurred.
    Error(String),
}

/// Configuration for a realtime voice session.
#[derive(Debug, Clone)]
pub struct VoiceSessionConfig {
    /// Language hint for ASR.
    pub language: String,
    /// Voice name for TTS.
    pub voice: String,
    /// Enable barge-in (user can interrupt AI).
    pub allow_barge_in: bool,
    /// Silence timeout before ending user turn (seconds).
    pub silence_timeout_secs: f32,
    /// Maximum user turn duration (seconds).
    pub max_turn_duration_secs: f32,
}

impl Default for VoiceSessionConfig {
    fn default() -> Self {
        Self {
            language: "zh".into(),
            voice: String::new(), // Auto-detect
            allow_barge_in: true,
            silence_timeout_secs: 1.5,
            max_turn_duration_secs: 30.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let cfg = VoiceSessionConfig::default();
        assert_eq!(cfg.language, "zh");
        assert!(cfg.allow_barge_in);
        assert_eq!(cfg.silence_timeout_secs, 1.5);
    }

    #[test]
    fn voice_session_states() {
        assert_ne!(VoiceSessionState::Listening, VoiceSessionState::Speaking);
    }
}
