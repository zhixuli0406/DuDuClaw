//! Sticker selector with triple-gate frequency control.
//!
//! Three gates must all pass for a sticker to be sent:
//! 1. **Intensity gate**: emotion intensity ≥ threshold (default 0.7)
//! 2. **Probability gate**: random chance × expressiveness multiplier
//! 3. **Cooldown gate**: ≥ N messages since last sticker in this session
//!
//! This ensures stickers are sent sparingly ("少量") — roughly 1 per 10-20 messages
//! in typical conversations.

use std::collections::HashMap;
use std::time::Instant;

use super::catalog::{PlatformSticker, StickerCatalog};
use super::emotion::{EmotionSignal, EmotionType};
use duduclaw_core::types::StickerConfig;

/// Simple LCG-based pseudo-random for lightweight random without external crate.
///
/// SECURITY: NOT cryptographically secure — suitable ONLY for non-security
/// purposes such as sticker send probability. Never use for tokens or secrets.
struct SimpleRng {
    state: u64,
}

impl SimpleRng {
    fn new() -> Self {
        // Seed from current time
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(42);
        Self { state: seed }
    }

    /// Generate a float in [0.0, 1.0).
    fn next_f32(&mut self) -> f32 {
        // LCG parameters from Numerical Recipes
        self.state = self.state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        // Use top 32 bits for best quality, cast to u32 to get full [0, u32::MAX] range
        ((self.state >> 32) as u32 as f32) / (u32::MAX as f32)
    }
}

/// Tracks per-session cooldown state.
struct SessionCooldown {
    messages_since_last: u32,
    last_activity: Instant,
}

/// Sticker selector — call `try_select()` after each reply to conditionally get a sticker.
pub struct StickerSelector {
    catalog: StickerCatalog,
    cooldowns: HashMap<String, SessionCooldown>,
    rng: SimpleRng,
}

impl StickerSelector {
    pub fn new(catalog: StickerCatalog) -> Self {
        Self {
            catalog,
            cooldowns: HashMap::new(),
            rng: SimpleRng::new(),
        }
    }

    /// Attempt to select a sticker. Returns `None` if any gate rejects.
    ///
    /// Call this after every reply — it internally tracks cooldown per session.
    pub fn try_select(
        &mut self,
        emotion: &EmotionSignal,
        session_id: &str,
        platform: &str,
        config: &StickerConfig,
    ) -> Option<PlatformSticker> {
        if !config.enabled {
            return None;
        }

        // Always increment cooldown counter
        let cooldown = self.cooldowns
            .entry(session_id.to_string())
            .or_insert(SessionCooldown {
                messages_since_last: config.cooldown_messages, // start ready
                last_activity: Instant::now(),
            });
        cooldown.messages_since_last = cooldown.messages_since_last.saturating_add(1);
        cooldown.last_activity = Instant::now();

        // Gate 1: Emotion intensity
        if emotion.emotion == EmotionType::Neutral || emotion.intensity < config.intensity_threshold {
            return None;
        }

        // Gate 2: Cooldown
        if cooldown.messages_since_last < config.cooldown_messages {
            return None;
        }

        // Gate 3: Probability (with expressiveness multiplier, clamped to 1.0)
        let effective_prob = (config.probability * config.expressiveness.multiplier()).min(1.0);
        if self.rng.next_f32() >= effective_prob {
            return None;
        }

        // All gates passed — select sticker
        let sticker = self.catalog.get(emotion.emotion, platform)?.clone();

        // Reset cooldown
        cooldown.messages_since_last = 0;

        Some(sticker)
    }

    /// Periodically clean up stale sessions (> 1 hour inactive).
    pub fn cleanup_stale_sessions(&mut self) {
        let cutoff = Instant::now() - std::time::Duration::from_secs(3600);
        self.cooldowns.retain(|_, v| v.last_activity > cutoff);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_core::types::Expressiveness;

    fn test_config(enabled: bool, probability: f32, threshold: f32, cooldown: u32) -> StickerConfig {
        StickerConfig {
            enabled,
            probability,
            intensity_threshold: threshold,
            cooldown_messages: cooldown,
            expressiveness: Expressiveness::Moderate,
        }
    }

    fn high_emotion() -> EmotionSignal {
        EmotionSignal { emotion: EmotionType::Happy, intensity: 0.9 }
    }

    fn low_emotion() -> EmotionSignal {
        EmotionSignal { emotion: EmotionType::Happy, intensity: 0.3 }
    }

    #[test]
    fn test_disabled_returns_none() {
        let mut sel = StickerSelector::new(StickerCatalog::new());
        let config = test_config(false, 1.0, 0.5, 0);
        assert!(sel.try_select(&high_emotion(), "s1", "telegram", &config).is_none());
    }

    #[test]
    fn test_low_intensity_returns_none() {
        let mut sel = StickerSelector::new(StickerCatalog::new());
        let config = test_config(true, 1.0, 0.7, 0);
        assert!(sel.try_select(&low_emotion(), "s1", "telegram", &config).is_none());
    }

    #[test]
    fn test_zero_probability_returns_none() {
        let mut sel = StickerSelector::new(StickerCatalog::new());
        let config = test_config(true, 0.0, 0.5, 0);
        // Try many times — should always be None
        for _ in 0..100 {
            assert!(sel.try_select(&high_emotion(), "s1", "telegram", &config).is_none());
        }
    }

    #[test]
    fn test_full_probability_returns_some() {
        let mut sel = StickerSelector::new(StickerCatalog::new());
        let config = test_config(true, 1.0, 0.5, 0);
        // With probability=1.0, cooldown=0, high emotion → should always return Some
        let result = sel.try_select(&high_emotion(), "s1", "telegram", &config);
        assert!(result.is_some());
    }

    #[test]
    fn test_cooldown_blocks() {
        let mut sel = StickerSelector::new(StickerCatalog::new());
        let config = test_config(true, 1.0, 0.5, 5);

        // First call succeeds (starts with cooldown=5, +1 = 6 ≥ 5)
        let r1 = sel.try_select(&high_emotion(), "s1", "telegram", &config);
        assert!(r1.is_some());

        // Next calls should be blocked (cooldown reset to 0, then +1 = 1 < 5)
        for _ in 0..4 {
            assert!(sel.try_select(&high_emotion(), "s1", "telegram", &config).is_none());
        }

        // After 5 messages, should be allowed again
        let r2 = sel.try_select(&high_emotion(), "s1", "telegram", &config);
        assert!(r2.is_some());
    }

    #[test]
    fn test_neutral_returns_none() {
        let mut sel = StickerSelector::new(StickerCatalog::new());
        let config = test_config(true, 1.0, 0.0, 0);
        assert!(sel.try_select(&EmotionSignal::neutral(), "s1", "telegram", &config).is_none());
    }

    #[test]
    fn test_different_sessions_independent() {
        let mut sel = StickerSelector::new(StickerCatalog::new());
        let config = test_config(true, 1.0, 0.5, 5);

        // Session 1 sends sticker
        assert!(sel.try_select(&high_emotion(), "s1", "telegram", &config).is_some());
        // Session 1 in cooldown
        assert!(sel.try_select(&high_emotion(), "s1", "telegram", &config).is_none());
        // Session 2 should be independent
        assert!(sel.try_select(&high_emotion(), "s2", "telegram", &config).is_some());
    }

    #[test]
    fn test_rng_covers_full_range() {
        // Verify next_f32() produces values across the full [0.0, 1.0) range,
        // not just [0.0, 0.5) which was a bug in the original implementation.
        let mut rng = super::SimpleRng::new();
        let mut max_seen: f32 = 0.0;
        for _ in 0..10_000 {
            let v = rng.next_f32();
            assert!((0.0..1.0).contains(&v), "RNG value {v} out of [0.0, 1.0)");
            if v > max_seen { max_seen = v; }
        }
        // With 10k samples, we should see values > 0.9
        assert!(max_seen > 0.9, "RNG max {max_seen} too low — range likely truncated");
    }

    #[test]
    fn test_cleanup_stale() {
        let mut sel = StickerSelector::new(StickerCatalog::new());
        let config = test_config(true, 1.0, 0.5, 0);
        sel.try_select(&high_emotion(), "s1", "telegram", &config);
        assert!(!sel.cooldowns.is_empty());
        // Can't test time-based cleanup easily, but ensure function doesn't panic
        sel.cleanup_stale_sessions();
    }
}
