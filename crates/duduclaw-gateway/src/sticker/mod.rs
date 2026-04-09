//! Emotion-based sticker sending — zero-LLM cost emotion detection + platform-native stickers.
//!
//! Architecture:
//! 1. `emotion::detect_emotion()` — pattern matching on user text + assistant reply
//! 2. `catalog::StickerCatalog` — per-platform sticker mappings (built-in + custom)
//! 3. `selector::StickerSelector` — triple gate (intensity → probability → cooldown)
//!
//! Each channel calls the pipeline independently after sending the text reply.
//! Sticker sending is async (`tokio::spawn`) and never blocks the text reply.

pub mod catalog;
pub mod emotion;
pub mod selector;

pub use catalog::{PlatformSticker, StickerCatalog};
pub use emotion::{EmotionSignal, EmotionType, detect_emotion};
pub use selector::StickerSelector;
