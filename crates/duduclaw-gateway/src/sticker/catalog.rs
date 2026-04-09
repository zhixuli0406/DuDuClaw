//! Sticker catalog — per-platform sticker/reaction mappings.
//!
//! Built-in defaults cover all 7 channels with sensible stickers.
//! Agents can override via `agent.toml [sticker]` custom mappings.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::EmotionType;

/// Platform-specific sticker/reaction payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "platform", rename_all = "snake_case")]
pub enum PlatformSticker {
    /// Telegram: send emoji character via sendSticker (animated emoji sticker).
    Telegram { emoji: String },
    /// LINE: sticker message via Push API.
    Line { package_id: String, sticker_id: String },
    /// Discord: add reaction emoji to the bot's own message.
    Discord { emoji: String },
    /// Slack: add reaction via `reactions.add` (emoji name without colons).
    Slack { emoji_name: String },
    /// WhatsApp: reaction emoji on the user's message.
    WhatsApp { emoji: String },
    /// Feishu: emoji in text (sticker upload not yet supported).
    Feishu { emoji: String },
    /// WebChat: emoji event over WebSocket.
    WebChat { emoji: String },
}

/// Sticker catalog with per-platform, per-emotion mappings.
#[derive(Debug, Clone)]
pub struct StickerCatalog {
    entries: HashMap<EmotionType, Vec<PlatformSticker>>,
}

impl Default for StickerCatalog {
    fn default() -> Self {
        Self::new()
    }
}

impl StickerCatalog {
    /// Create catalog with built-in defaults for all platforms.
    pub fn new() -> Self {
        let mut entries: HashMap<EmotionType, Vec<PlatformSticker>> = HashMap::new();

        // ── Happy ───────────────────────────────────────────────
        entries.insert(EmotionType::Happy, vec![
            PlatformSticker::Telegram { emoji: "\u{1F60A}".into() }, // 😊
            PlatformSticker::Line { package_id: "11537".into(), sticker_id: "52002734".into() },
            PlatformSticker::Discord { emoji: "\u{1F60A}".into() },
            PlatformSticker::Slack { emoji_name: "blush".into() },
            PlatformSticker::WhatsApp { emoji: "\u{1F60A}".into() },
            PlatformSticker::Feishu { emoji: "\u{1F60A}".into() },
            PlatformSticker::WebChat { emoji: "\u{1F60A}".into() },
        ]);

        // ── Grateful ────────────────────────────────────────────
        entries.insert(EmotionType::Grateful, vec![
            PlatformSticker::Telegram { emoji: "\u{1F64F}".into() }, // 🙏
            PlatformSticker::Line { package_id: "11537".into(), sticker_id: "52002735".into() },
            PlatformSticker::Discord { emoji: "\u{1F64F}".into() },
            PlatformSticker::Slack { emoji_name: "pray".into() },
            PlatformSticker::WhatsApp { emoji: "\u{1F64F}".into() },
            PlatformSticker::Feishu { emoji: "\u{1F64F}".into() },
            PlatformSticker::WebChat { emoji: "\u{1F64F}".into() },
        ]);

        // ── Excited ─────────────────────────────────────────────
        entries.insert(EmotionType::Excited, vec![
            PlatformSticker::Telegram { emoji: "\u{1F389}".into() }, // 🎉
            PlatformSticker::Line { package_id: "11538".into(), sticker_id: "51626494".into() },
            PlatformSticker::Discord { emoji: "\u{1F389}".into() },
            PlatformSticker::Slack { emoji_name: "tada".into() },
            PlatformSticker::WhatsApp { emoji: "\u{1F389}".into() },
            PlatformSticker::Feishu { emoji: "\u{1F389}".into() },
            PlatformSticker::WebChat { emoji: "\u{1F389}".into() },
        ]);

        // ── Sad ─────────────────────────────────────────────────
        entries.insert(EmotionType::Sad, vec![
            PlatformSticker::Telegram { emoji: "\u{1F622}".into() }, // 😢
            PlatformSticker::Line { package_id: "11537".into(), sticker_id: "52002736".into() },
            PlatformSticker::Discord { emoji: "\u{1F622}".into() },
            PlatformSticker::Slack { emoji_name: "cry".into() },
            PlatformSticker::WhatsApp { emoji: "\u{1F622}".into() },
            PlatformSticker::Feishu { emoji: "\u{1F622}".into() },
            PlatformSticker::WebChat { emoji: "\u{1F622}".into() },
        ]);

        // ── Frustrated ──────────────────────────────────────────
        entries.insert(EmotionType::Frustrated, vec![
            PlatformSticker::Telegram { emoji: "\u{1F4AA}".into() }, // 💪 (encouragement)
            PlatformSticker::Line { package_id: "11538".into(), sticker_id: "51626496".into() },
            PlatformSticker::Discord { emoji: "\u{1F4AA}".into() },
            PlatformSticker::Slack { emoji_name: "muscle".into() },
            PlatformSticker::WhatsApp { emoji: "\u{1F4AA}".into() },
            PlatformSticker::Feishu { emoji: "\u{1F4AA}".into() },
            PlatformSticker::WebChat { emoji: "\u{1F4AA}".into() },
        ]);

        // ── Loving ──────────────────────────────────────────────
        entries.insert(EmotionType::Loving, vec![
            PlatformSticker::Telegram { emoji: "\u{2764}\u{FE0F}".into() }, // ❤️
            PlatformSticker::Line { package_id: "11537".into(), sticker_id: "52002737".into() },
            PlatformSticker::Discord { emoji: "\u{2764}".into() }, // ❤ without variation selector for Discord API compatibility
            PlatformSticker::Slack { emoji_name: "heart".into() },
            PlatformSticker::WhatsApp { emoji: "\u{2764}\u{FE0F}".into() },
            PlatformSticker::Feishu { emoji: "\u{2764}\u{FE0F}".into() },
            PlatformSticker::WebChat { emoji: "\u{2764}\u{FE0F}".into() },
        ]);

        Self { entries }
    }

    /// Get a sticker for the given emotion and platform.
    ///
    /// Returns `None` for `Neutral` emotion or if no mapping exists.
    pub fn get(&self, emotion: EmotionType, platform: &str) -> Option<&PlatformSticker> {
        if emotion == EmotionType::Neutral {
            return None;
        }
        self.entries.get(&emotion)?.iter().find(|s| {
            matches!(
                (platform, s),
                ("telegram", PlatformSticker::Telegram { .. })
                    | ("line", PlatformSticker::Line { .. })
                    | ("discord", PlatformSticker::Discord { .. })
                    | ("slack", PlatformSticker::Slack { .. })
                    | ("whatsapp", PlatformSticker::WhatsApp { .. })
                    | ("feishu", PlatformSticker::Feishu { .. })
                    | ("webchat", PlatformSticker::WebChat { .. })
            )
        })
    }

    /// Merge custom sticker overrides (from agent.toml).
    ///
    /// Per-platform merge: only replaces entries whose platform tag matches
    /// an override. Other platforms' stickers are preserved.
    pub fn with_overrides(mut self, overrides: &HashMap<String, Vec<PlatformSticker>>) -> Self {
        for (emotion_str, new_stickers) in overrides {
            let emotion = match emotion_str.as_str() {
                "happy" => EmotionType::Happy,
                "grateful" => EmotionType::Grateful,
                "excited" => EmotionType::Excited,
                "sad" => EmotionType::Sad,
                "frustrated" => EmotionType::Frustrated,
                "loving" => EmotionType::Loving,
                _ => continue,
            };
            let existing = self.entries.entry(emotion).or_insert_with(Vec::new);
            // For each override sticker, replace the matching platform entry or append
            for new in new_stickers {
                let platform_tag = platform_of(new);
                if let Some(pos) = existing.iter().position(|s| platform_of(s) == platform_tag) {
                    existing[pos] = new.clone();
                } else {
                    existing.push(new.clone());
                }
            }
        }
        self
    }
}

/// Extract the platform discriminant from a `PlatformSticker`.
fn platform_of(s: &PlatformSticker) -> &'static str {
    match s {
        PlatformSticker::Telegram { .. } => "telegram",
        PlatformSticker::Line { .. } => "line",
        PlatformSticker::Discord { .. } => "discord",
        PlatformSticker::Slack { .. } => "slack",
        PlatformSticker::WhatsApp { .. } => "whatsapp",
        PlatformSticker::Feishu { .. } => "feishu",
        PlatformSticker::WebChat { .. } => "webchat",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_emotions_have_all_platforms() {
        let catalog = StickerCatalog::new();
        let platforms = ["telegram", "line", "discord", "slack", "whatsapp", "feishu", "webchat"];
        let emotions = [
            EmotionType::Happy,
            EmotionType::Grateful,
            EmotionType::Excited,
            EmotionType::Sad,
            EmotionType::Frustrated,
            EmotionType::Loving,
        ];

        for emotion in &emotions {
            for platform in &platforms {
                assert!(
                    catalog.get(*emotion, platform).is_some(),
                    "Missing sticker for {emotion:?} on {platform}"
                );
            }
        }
    }

    #[test]
    fn test_neutral_returns_none() {
        let catalog = StickerCatalog::new();
        assert!(catalog.get(EmotionType::Neutral, "telegram").is_none());
    }

    #[test]
    fn test_unknown_platform_returns_none() {
        let catalog = StickerCatalog::new();
        assert!(catalog.get(EmotionType::Happy, "unknown").is_none());
    }

    #[test]
    fn test_override_replaces_per_platform() {
        let catalog = StickerCatalog::new();
        let mut overrides = HashMap::new();
        overrides.insert("happy".into(), vec![
            PlatformSticker::Telegram { emoji: "CUSTOM".into() },
        ]);
        let catalog = catalog.with_overrides(&overrides);
        // Telegram should be overridden
        let sticker = catalog.get(EmotionType::Happy, "telegram").unwrap();
        match sticker {
            PlatformSticker::Telegram { emoji } => assert_eq!(emoji, "CUSTOM"),
            _ => panic!("Expected Telegram sticker"),
        }
        // Other platforms should be preserved (not wiped)
        assert!(catalog.get(EmotionType::Happy, "discord").is_some());
        assert!(catalog.get(EmotionType::Happy, "line").is_some());
        assert!(catalog.get(EmotionType::Happy, "slack").is_some());
    }
}
