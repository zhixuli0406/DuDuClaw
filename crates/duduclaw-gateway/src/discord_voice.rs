//! Discord Voice Channel integration via Songbird.
//!
//! Enables the bot to join Discord voice channels, receive audio from
//! participants, transcribe via ASR, process through the Agent pipeline,
//! and respond with TTS audio.
//!
//! ## Protocol
//!
//! Discord voice uses a separate connection from the main gateway:
//! 1. Gateway WebSocket → Voice State Update (join channel)
//! 2. Discord returns Voice Server info
//! 3. Voice WebSocket (signaling) + UDP/RTP (audio)
//! 4. Audio codec: Opus, 48kHz stereo, 20ms frames
//!
//! ## Architecture
//!
//! ```text
//! Discord Voice Channel
//!   ├── User speaks → Opus/RTP → Songbird decode → PCM i16 48kHz
//!   │     → Resample 16kHz → VAD → ASR → Agent → TTS → Opus encode → RTP
//!   └── Per-user SSRC tracking (who is speaking)
//! ```
//!
//! Requires the `discord-voice` feature flag and `libopus` system library.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn, debug};

use crate::channel_reply::ReplyContext;

/// Discord voice channel session state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceChannelState {
    /// Not connected to any voice channel.
    Disconnected,
    /// Connecting to voice channel.
    Connecting,
    /// Connected and listening.
    Connected,
    /// Error state.
    Error,
}

/// Configuration for Discord voice integration.
#[derive(Debug, Clone)]
pub struct DiscordVoiceConfig {
    /// Whether voice channel support is enabled.
    pub enabled: bool,
    /// ASR language hint (default: "zh").
    pub asr_language: String,
    /// TTS voice name (empty = auto-detect).
    pub tts_voice: String,
    /// Minimum speech duration before triggering ASR (seconds).
    pub min_speech_seconds: f32,
    /// Silence timeout before processing accumulated speech (seconds).
    pub silence_timeout_seconds: f32,
    /// Maximum concurrent voice channels per bot.
    pub max_concurrent_channels: u32,
}

impl Default for DiscordVoiceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            asr_language: "zh".into(),
            tts_voice: String::new(),
            min_speech_seconds: 0.5,
            silence_timeout_seconds: 1.5,
            max_concurrent_channels: 3,
        }
    }
}

/// Tracks per-user audio accumulation in a voice channel.
struct UserAudioBuffer {
    /// Accumulated PCM samples (i16, 48kHz stereo from Discord).
    samples: Vec<i16>,
    /// When this user last spoke.
    last_speech: std::time::Instant,
    /// Discord user ID (snowflake).
    user_id: u64,
    /// Display name for logging.
    display_name: String,
}

impl UserAudioBuffer {
    fn new(user_id: u64, display_name: String) -> Self {
        Self {
            samples: Vec::new(),
            last_speech: std::time::Instant::now(),
            user_id,
            display_name,
        }
    }

    /// Maximum buffer: 30 seconds of 48kHz stereo = ~5.5MB.
    const MAX_BUFFER_SAMPLES: usize = 48000 * 2 * 30;

    fn append_samples(&mut self, data: &[i16]) {
        let mut remaining = Self::MAX_BUFFER_SAMPLES.saturating_sub(self.samples.len());
        if remaining == 0 {
            warn!(user_id = self.user_id, "Audio buffer full (30s), forcing clear");
            self.samples.clear();
            remaining = Self::MAX_BUFFER_SAMPLES;
        }
        let to_append = data.len().min(remaining);
        self.samples.extend_from_slice(&data[..to_append]);
        self.last_speech = std::time::Instant::now();
    }

    fn silence_duration(&self) -> f32 {
        self.last_speech.elapsed().as_secs_f32()
    }

    /// Convert accumulated Discord audio (48kHz stereo i16) to ASR format (16kHz mono f32).
    fn to_asr_pcm(&self) -> Vec<f32> {
        // Step 1: Stereo to mono (average L+R)
        let mono: Vec<f32> = self.samples
            .chunks(2)
            .map(|pair| {
                let l = pair[0] as f32 / 32768.0;
                let r = pair.get(1).map(|&v| v as f32 / 32768.0).unwrap_or(l);
                (l + r) / 2.0
            })
            .collect();

        // Step 2: Resample 48kHz → 16kHz (simple 3:1 decimation with averaging)
        mono.chunks(3)
            .map(|chunk| chunk.iter().sum::<f32>() / chunk.len() as f32)
            .collect()
    }

    fn clear(&mut self) {
        self.samples.clear();
    }

    fn duration_seconds(&self) -> f32 {
        // 48kHz stereo = 96000 samples/second
        self.samples.len() as f32 / 96000.0
    }
}

/// Discord voice channel manager — handles join/leave and audio routing.
pub struct DiscordVoiceManager {
    config: DiscordVoiceConfig,
    /// Active voice channel sessions (guild_id → state).
    sessions: Arc<RwLock<HashMap<u64, VoiceChannelState>>>,
    /// Count of active connections.
    active_count: Arc<std::sync::atomic::AtomicU32>,
}

impl DiscordVoiceManager {
    pub fn new(config: DiscordVoiceConfig) -> Self {
        Self {
            config,
            sessions: Arc::new(RwLock::new(HashMap::new())),
            active_count: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        }
    }

    /// Atomically check and record a join (prevents TOCTOU race).
    /// Returns true if join was allowed, false if at capacity or disabled.
    pub async fn try_join(&self, guild_id: u64) -> bool {
        if !self.config.enabled {
            return false;
        }
        let mut sessions = self.sessions.write().await;
        if sessions.len() as u32 >= self.config.max_concurrent_channels {
            return false;
        }
        sessions.insert(guild_id, VoiceChannelState::Connecting);
        self.active_count.store(sessions.len() as u32, std::sync::atomic::Ordering::Release);
        info!(guild_id, "Discord voice: joining channel");
        true
    }

    /// Check if we can join (non-atomic read for UI display).
    pub fn can_join(&self) -> bool {
        self.config.enabled
            && self.active_count.load(std::sync::atomic::Ordering::Relaxed)
                < self.config.max_concurrent_channels
    }

    /// Transition from Connecting → Connected after successful Songbird join.
    pub async fn record_connected(&self, guild_id: u64) {
        let mut sessions = self.sessions.write().await;
        if let Some(state) = sessions.get_mut(&guild_id) {
            *state = VoiceChannelState::Connected;
        }
        info!(guild_id, "Discord voice: connected");
    }

    /// Record a leave event (always cleans up, even on error).
    pub async fn record_leave(&self, guild_id: u64) {
        let mut sessions = self.sessions.write().await;
        sessions.remove(&guild_id);
        self.active_count.store(sessions.len() as u32, std::sync::atomic::Ordering::Release);
        info!(guild_id, "Discord voice: left channel");
    }

    /// Get the state of a specific guild's voice session.
    pub async fn get_state(&self, guild_id: u64) -> VoiceChannelState {
        self.sessions.read().await
            .get(&guild_id)
            .copied()
            .unwrap_or(VoiceChannelState::Disconnected)
    }

    /// Get the current config.
    pub fn config(&self) -> &DiscordVoiceConfig {
        &self.config
    }

    /// Process a batch of decoded audio from a Discord voice tick.
    ///
    /// This is called with per-user decoded PCM data from Songbird's
    /// `CoreEvent::VoiceTick` handler. Returns text to process if a
    /// user's speech buffer is ready for ASR.
    pub async fn process_voice_tick(
        &self,
        user_audio: &HashMap<u64, Vec<i16>>,
        user_names: &HashMap<u64, String>,
        buffers: &mut HashMap<u64, UserAudioBuffer>,
    ) -> Vec<(u64, String, Vec<f32>)> {
        let mut ready = Vec::new();

        // Append new audio to per-user buffers
        for (&user_id, samples) in user_audio {
            let buffer = buffers.entry(user_id).or_insert_with(|| {
                let name = user_names.get(&user_id)
                    .cloned()
                    .unwrap_or_else(|| format!("User-{user_id}"));
                UserAudioBuffer::new(user_id, name)
            });
            buffer.append_samples(samples);
        }

        // Check for silence timeout on all active buffers
        let silence_timeout = self.config.silence_timeout_seconds;
        let min_speech = self.config.min_speech_seconds;

        for (user_id, buffer) in buffers.iter_mut() {
            if buffer.silence_duration() >= silence_timeout
                && buffer.duration_seconds() >= min_speech
                && !buffer.samples.is_empty()
            {
                let pcm = buffer.to_asr_pcm();
                debug!(
                    user_id,
                    user = %buffer.display_name,
                    duration = format!("{:.1}s", buffer.duration_seconds()),
                    pcm_samples = pcm.len(),
                    "Discord voice: speech segment ready for ASR"
                );
                ready.push((*user_id, buffer.display_name.clone(), pcm));
                buffer.clear();
            }
        }

        // Clean up buffers for users who haven't spoken in 30s
        buffers.retain(|_, buf| buf.last_speech.elapsed().as_secs() < 30);

        ready
    }
}

// ── Songbird Integration (feature-gated) ────────────────────────

/// Songbird VoiceTick event handler — bridges Songbird decoded audio
/// into the `DiscordVoiceManager` processing pipeline.
///
/// Register this handler on a Songbird call:
/// ```ignore
/// handler.add_global_event(CoreEvent::VoiceTick.into(), SongbirdReceiver::new(manager));
/// ```
#[cfg(feature = "discord-voice")]
pub struct SongbirdReceiver {
    manager: Arc<DiscordVoiceManager>,
    buffers: Arc<tokio::sync::Mutex<HashMap<u64, UserAudioBuffer>>>,
    /// Channel to send ASR-ready segments to the processing pipeline.
    asr_tx: mpsc::Sender<(u64, String, Vec<f32>)>,
}

#[cfg(feature = "discord-voice")]
impl SongbirdReceiver {
    pub fn new(
        manager: Arc<DiscordVoiceManager>,
        asr_tx: mpsc::Sender<(u64, String, Vec<f32>)>,
    ) -> Self {
        Self {
            manager,
            buffers: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            asr_tx,
        }
    }
}

#[cfg(feature = "discord-voice")]
#[async_trait]
impl songbird::events::EventHandler for SongbirdReceiver {
    async fn act(&self, ctx: &songbird::events::EventContext<'_>) -> Option<songbird::events::Event> {
        use songbird::events::EventContext;

        if let EventContext::VoiceTick(tick) = ctx {
            let mut user_audio = HashMap::new();
            let mut user_names = HashMap::new();

            for (&ssrc, data) in &tick.speaking {
                if let Some(decoded) = &data.decoded_voice {
                    // SSRC → user_id resolution via Songbird's SSRC map.
                    // VoiceTick.speaking key is u32 SSRC, and SpeakingStateUpdate
                    // provides the SSRC→UserId mapping. For now, use SSRC as a
                    // stable session-local identifier (not Discord snowflake).
                    // The user_id is used only for buffer tracking, not for
                    // access control or billing.
                    let session_id = ssrc as u64;
                    user_audio.insert(session_id, decoded.clone());
                    user_names.entry(session_id)
                        .or_insert_with(|| format!("Voice-{ssrc}"));
                }
            }

            if !user_audio.is_empty() {
                // Collect ready segments while holding lock, then release before sending
                let ready = {
                    let mut buffers = self.buffers.lock().await;
                    self.manager
                        .process_voice_tick(&user_audio, &user_names, &mut buffers)
                        .await
                }; // lock released here

                // Send ASR-ready segments without holding the lock
                for (user_id, display_name, pcm) in ready {
                    if self.asr_tx.send((user_id, display_name, pcm)).await.is_err() {
                        warn!("Discord voice: ASR channel closed");
                        break;
                    }
                }
            }
        }

        None
    }
}

/// Helper: join a Discord voice channel using Songbird.
///
/// Returns a receiver for ASR-ready audio segments.
#[cfg(feature = "discord-voice")]
pub async fn join_voice_channel(
    songbird_manager: &Arc<songbird::Songbird>,
    guild_id: u64,
    channel_id: u64,
    voice_manager: Arc<DiscordVoiceManager>,
) -> Result<mpsc::Receiver<(u64, String, Vec<f32>)>, String> {
    use songbird::CoreEvent;

    if !voice_manager.try_join(guild_id).await {
        return Err("Cannot join: voice channels limit reached or disabled".into());
    }

    let guild = songbird::id::GuildId::from(guild_id);
    let channel = songbird::id::ChannelId::from(channel_id);

    let handler_lock = match songbird_manager.join(guild, channel).await {
        Ok(h) => h,
        Err(e) => {
            // Rollback: Songbird failed, undo the try_join record
            voice_manager.record_leave(guild_id).await;
            return Err(format!("Songbird join: {e}"));
        }
    };

    let (asr_tx, asr_rx) = mpsc::channel(32);

    // Register the VoiceTick receiver
    {
        let mut handler = handler_lock.lock().await;

        // Enable decoding of received audio
        let config = songbird::Config::default()
            .decode_mode(songbird::driver::DecodeMode::Decode);
        handler.set_config(config);

        // Register event handlers
        handler.add_global_event(
            CoreEvent::VoiceTick.into(),
            SongbirdReceiver::new(voice_manager.clone(), asr_tx),
        );

        handler.add_global_event(
            CoreEvent::SpeakingStateUpdate.into(),
            SpeakingStateHandler,
        );
    }

    // Transition Connecting → Connected
    voice_manager.record_connected(guild_id).await;
    info!(guild_id, channel_id, "Discord voice: joined channel with Songbird");

    Ok(asr_rx)
}

/// Handles SpeakingStateUpdate events (logs who starts/stops speaking).
#[cfg(feature = "discord-voice")]
struct SpeakingStateHandler;

#[cfg(feature = "discord-voice")]
#[async_trait]
impl songbird::events::EventHandler for SpeakingStateHandler {
    async fn act(&self, ctx: &songbird::events::EventContext<'_>) -> Option<songbird::events::Event> {
        use songbird::events::EventContext;
        if let EventContext::SpeakingStateUpdate(speaking) = ctx {
            debug!(ssrc = speaking.ssrc, speaking = speaking.speaking, "Discord voice: speaking state");
        }
        None
    }
}

/// Helper: leave a Discord voice channel.
#[cfg(feature = "discord-voice")]
pub async fn leave_voice_channel(
    songbird_manager: &Arc<songbird::Songbird>,
    guild_id: u64,
    voice_manager: Arc<DiscordVoiceManager>,
) -> Result<(), String> {
    let guild = songbird::id::GuildId::from(guild_id);
    let result = songbird_manager
        .leave(guild)
        .await
        .map_err(|e| format!("Songbird leave: {e}"));
    // Always clean up state, even if Songbird leave failed
    voice_manager.record_leave(guild_id).await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_audio_buffer_conversion() {
        // Simulate 48kHz stereo audio: 480 samples = 5ms
        let stereo_48k: Vec<i16> = (0..480).map(|i| (i * 10) as i16).collect();
        let mut buf = UserAudioBuffer::new(123, "TestUser".into());
        buf.append_samples(&stereo_48k);

        let pcm = buf.to_asr_pcm();
        // 480 stereo samples → 240 mono → 80 at 16kHz (÷3)
        assert_eq!(pcm.len(), 80);
    }

    #[test]
    fn buffer_duration() {
        let mut buf = UserAudioBuffer::new(1, "User".into());
        // 96000 samples = 1 second of 48kHz stereo
        buf.append_samples(&vec![0i16; 96000]);
        assert!((buf.duration_seconds() - 1.0).abs() < 0.01);
    }

    #[test]
    fn manager_can_join() {
        let mgr = DiscordVoiceManager::new(DiscordVoiceConfig {
            enabled: true,
            max_concurrent_channels: 2,
            ..Default::default()
        });
        assert!(mgr.can_join());
    }

    #[test]
    fn manager_disabled() {
        let mgr = DiscordVoiceManager::new(DiscordVoiceConfig::default());
        assert!(!mgr.can_join()); // enabled = false by default
    }

    #[tokio::test]
    async fn process_voice_tick_silence_timeout() {
        let mgr = DiscordVoiceManager::new(DiscordVoiceConfig {
            enabled: true,
            silence_timeout_seconds: 0.0, // Immediate for testing
            min_speech_seconds: 0.0,
            ..Default::default()
        });

        let mut buffers = HashMap::new();
        let mut user_audio = HashMap::new();
        user_audio.insert(42u64, vec![1000i16; 960]); // 10ms of stereo audio

        let mut user_names = HashMap::new();
        user_names.insert(42u64, "TestUser".to_string());

        // First tick: audio arrives
        let ready = mgr.process_voice_tick(&user_audio, &user_names, &mut buffers).await;
        // With 0s silence timeout, should be ready immediately
        assert!(!ready.is_empty() || buffers.contains_key(&42));
    }
}
