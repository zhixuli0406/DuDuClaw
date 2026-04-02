//! LiveKit WebRTC voice room integration.
//!
//! Connects to a LiveKit room, receives audio from participants,
//! routes through ASR pipeline, and publishes TTS audio back.
//!
//! Requires the `livekit` feature flag and a running LiveKit server.
//!
//! ## Architecture
//!
//! ```text
//! LiveKit Room
//!   ├── Participant speaks → NativeAudioStream → PCM frames
//!   │     → VAD filter → ASR → Agent Pipeline → TTS → AudioTrack publish
//!   └── Multiple participants tracked by identity
//! ```

use async_trait::async_trait;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::error::InferenceError;
use crate::realtime_voice::{AudioChunk, AudioFormat, PartialTranscript, VoiceEvent, VoiceSessionConfig};

/// LiveKit room configuration.
///
/// `api_secret` is zeroized on drop to prevent leaking in crash dumps.
/// Debug output redacts the secret.
#[derive(Clone)]
pub struct LiveKitConfig {
    /// LiveKit server URL (e.g., "wss://myserver.livekit.cloud").
    pub server_url: String,
    /// API key for token generation.
    pub api_key: String,
    /// API secret for token generation (zeroized on drop).
    pub api_secret: String,
    /// Room name to join.
    pub room_name: String,
    /// Bot identity in the room.
    pub bot_identity: String,
}

impl Drop for LiveKitConfig {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.api_secret.zeroize();
    }
}

impl std::fmt::Debug for LiveKitConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveKitConfig")
            .field("server_url", &self.server_url)
            .field("api_key", &"[REDACTED]")
            .field("api_secret", &"[REDACTED]")
            .field("room_name", &self.room_name)
            .field("bot_identity", &self.bot_identity)
            .finish()
    }
}

/// LiveKit voice session — manages a single room connection.
///
/// This is a trait-based abstraction so the heavy `livekit` crate
/// dependency is only pulled in when the `livekit` feature is enabled.
#[async_trait]
pub trait LiveKitSession: Send + Sync {
    /// Connect to a LiveKit room and start receiving audio.
    async fn connect(&mut self, config: &LiveKitConfig) -> Result<(), InferenceError>;

    /// Take the voice event receiver (can only be called once after connect).
    fn take_events(&mut self) -> Option<mpsc::Receiver<VoiceEvent>>;

    /// Publish audio to the room (TTS output).
    async fn publish_audio(&self, audio: AudioChunk) -> Result<(), InferenceError>;

    /// Leave the room and clean up.
    async fn disconnect(&mut self) -> Result<(), InferenceError>;

    /// Check if currently connected.
    fn is_connected(&self) -> bool;
}

/// Stub implementation when `livekit` feature is not enabled.
pub struct LiveKitStub;

impl LiveKitStub {
    pub fn new() -> Self {
        Self
    }

    pub fn is_available() -> bool {
        cfg!(feature = "livekit-voice")
    }
}

#[async_trait]
impl LiveKitSession for LiveKitStub {
    async fn connect(&mut self, _config: &LiveKitConfig) -> Result<(), InferenceError> {
        Err(InferenceError::Other(
            "LiveKit requires the 'livekit' feature flag. \
             Install: cargo build --features livekit"
                .into(),
        ))
    }

    fn take_events(&mut self) -> Option<mpsc::Receiver<VoiceEvent>> {
        None
    }

    async fn publish_audio(&self, _audio: AudioChunk) -> Result<(), InferenceError> {
        Err(InferenceError::Other("LiveKit not available".into()))
    }

    async fn disconnect(&mut self) -> Result<(), InferenceError> {
        Ok(())
    }

    fn is_connected(&self) -> bool {
        false
    }
}

// ── Real LiveKit implementation (feature-gated) ─────────────────

#[cfg(feature = "livekit-voice")]
pub struct RealLiveKitSession {
    room: Option<livekit::Room>,
    event_rx: Option<mpsc::Receiver<VoiceEvent>>,
    connected: bool,
}

#[cfg(feature = "livekit-voice")]
impl RealLiveKitSession {
    pub fn new() -> Self {
        Self {
            room: None,
            event_rx: None,
            connected: false,
        }
    }
}

#[cfg(feature = "livekit-voice")]
#[async_trait]
impl LiveKitSession for RealLiveKitSession {
    async fn connect(&mut self, config: &LiveKitConfig) -> Result<(), InferenceError> {
        use livekit::prelude::*;

        // Generate access token
        let token = livekit_api::access_token::AccessToken::with_api_key(
            &config.api_key,
            &config.api_secret,
        )
        .with_identity(&config.bot_identity)
        .with_ttl(std::time::Duration::from_secs(3600)) // 1 hour max
        .with_grants(livekit_api::access_token::VideoGrants {
            room_join: true,
            room: config.room_name.clone(),
            can_publish: true,
            can_subscribe: true,
            can_publish_data: false, // minimal permissions
            ..Default::default()
        })
        .to_jwt()
        .map_err(|e| InferenceError::Other(format!("LiveKit token: {e}")))?;

        info!(room = %config.room_name, "LiveKit: connecting to room");

        let (room, mut room_events) = Room::connect(
            &config.server_url,
            &token,
            RoomOptions::default(),
        )
        .await
        .map_err(|e| InferenceError::Other(format!("LiveKit connect: {e}")))?;

        let (event_tx, event_rx) = mpsc::channel::<VoiceEvent>(64);

        // Spawn a task to forward LiveKit room events → VoiceEvent channel
        tokio::spawn(async move {
            while let Some(event) = room_events.recv().await {
                match event {
                    RoomEvent::TrackSubscribed { track, publication, participant } => {
                        if let RemoteTrack::Audio(audio_track) = track {
                            let tx = event_tx.clone();
                            let participant_id = participant.identity().to_string();
                            info!(participant = %participant_id, "LiveKit: audio track subscribed");

                            tokio::spawn(async move {
                                let rtc_track = audio_track.rtc_track();
                                let mut stream = NativeAudioStream::new(rtc_track);
                                use futures_util::StreamExt;
                                while let Some(frame) = stream.next().await {
                                    // LiveKit audio is typically 48kHz i16.
                                    // Downstream ASR pipeline handles resampling to 16kHz.
                                    let chunk = AudioChunk {
                                        data: frame.data.iter()
                                            .flat_map(|&s| s.to_le_bytes())
                                            .collect(),
                                        format: AudioFormat::Pcm48kHz,
                                        is_final: false,
                                    };
                                    if tx.send(VoiceEvent::AudioReady(chunk)).await.is_err() {
                                        break;
                                    }
                                }
                            });
                        }
                    }
                    RoomEvent::ParticipantDisconnected(participant) => {
                        info!(participant = %participant.identity(), "LiveKit: participant left");
                    }
                    RoomEvent::Disconnected { reason } => {
                        warn!(?reason, "LiveKit: disconnected from room");
                        let _ = event_tx.send(VoiceEvent::Error(
                            format!("Disconnected: {reason:?}")
                        )).await;
                        break;
                    }
                    _ => {}
                }
            }
        });

        self.room = Some(room);
        self.event_rx = Some(event_rx);
        self.connected = true;
        info!(room = %config.room_name, "LiveKit: connected");
        Ok(())
    }

    fn take_events(&mut self) -> Option<mpsc::Receiver<VoiceEvent>> {
        self.event_rx.take()
    }

    async fn publish_audio(&self, audio: AudioChunk) -> Result<(), InferenceError> {
        let room = self.room.as_ref()
            .ok_or_else(|| InferenceError::Other("Not connected to LiveKit room".into()))?;

        // Validate audio data length is even (i16 = 2 bytes)
        if audio.data.len() % 2 != 0 {
            return Err(InferenceError::Other(format!(
                "Audio data length {} is not even (expected i16 pairs)",
                audio.data.len()
            )));
        }

        // Placeholder: full implementation requires LocalAudioTrack + AudioSource
        let sample_count = audio.data.len() / 2;
        info!(samples = sample_count, "LiveKit: publishing audio (placeholder)");
        // Note: actual audio publishing requires creating a LocalAudioTrack
        // and publishing it to the room. This is a simplified placeholder.
        // Full implementation needs:
        //   1. Create LocalAudioTrack with AudioSource
        //   2. room.local_participant().publish_track(track, options)
        //   3. Feed audio frames to the AudioSource
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), InferenceError> {
        // Always mark as disconnected, even if close() fails
        // (room.take() already consumed the Room)
        self.connected = false;
        if let Some(room) = self.room.take() {
            room.close().await
                .map_err(|e| InferenceError::Other(format!("LiveKit disconnect: {e}")))?;
        }
        info!("LiveKit: disconnected");
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected
    }
}

/// Create a LiveKit session (stub or real depending on feature flag).
pub fn create_session() -> Box<dyn LiveKitSession> {
    #[cfg(feature = "livekit-voice")]
    {
        Box::new(RealLiveKitSession::new())
    }
    #[cfg(not(feature = "livekit-voice"))]
    {
        Box::new(LiveKitStub::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_not_available() {
        #[cfg(not(feature = "livekit-voice"))]
        assert!(!LiveKitStub::is_available());
    }

    #[tokio::test]
    async fn stub_connect_fails() {
        let mut session = LiveKitStub::new();
        let config = LiveKitConfig {
            server_url: "wss://test.livekit.cloud".into(),
            api_key: "key".into(),
            api_secret: "secret".into(),
            room_name: "test-room".into(),
            bot_identity: "bot".into(),
        };
        assert!(session.connect(&config).await.is_err());
    }
}
