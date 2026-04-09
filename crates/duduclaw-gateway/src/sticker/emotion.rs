//! Zero-LLM emotion detection via pattern matching.
//!
//! Detects emotion from user messages and assistant replies using:
//! - Emoji patterns (highest weight — explicit emotional signals)
//! - zh-TW keyword matching
//! - en keyword matching
//! - Assistant reply context bonuses
//!
//! Designed for bilingual (zh-TW + en) conversations.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Emotion categories — intentionally small set for practical sticker mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmotionType {
    /// Happiness, satisfaction, contentment.
    Happy,
    /// Gratitude, appreciation.
    Grateful,
    /// Excitement, celebration, task completion.
    Excited,
    /// Sadness, disappointment, sympathy.
    Sad,
    /// Frustration, confusion, anxiety.
    Frustrated,
    /// Warmth, care, encouragement.
    Loving,
    /// No clear emotional signal.
    Neutral,
}

impl fmt::Display for EmotionType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Happy => write!(f, "happy"),
            Self::Grateful => write!(f, "grateful"),
            Self::Excited => write!(f, "excited"),
            Self::Sad => write!(f, "sad"),
            Self::Frustrated => write!(f, "frustrated"),
            Self::Loving => write!(f, "loving"),
            Self::Neutral => write!(f, "neutral"),
        }
    }
}

/// Detected emotion with intensity score.
#[derive(Debug, Clone)]
pub struct EmotionSignal {
    pub emotion: EmotionType,
    pub intensity: f32,
}

impl EmotionSignal {
    pub fn neutral() -> Self {
        Self { emotion: EmotionType::Neutral, intensity: 0.0 }
    }
}

/// Truncate a string to at most `max_bytes` bytes at a valid UTF-8 char boundary.
fn truncate_utf8(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

// ── Emoji patterns ──────────────────────────────────────────────

const HAPPY_EMOJI: &[&str] = &[
    "\u{1F60A}", // 😊
    "\u{1F604}", // 😄
    "\u{1F603}", // 😃
    "\u{1F601}", // 😁
    "\u{1F606}", // 😆
    "\u{263A}",  // ☺
    "\u{1F44D}", // 👍
];

const GRATEFUL_EMOJI: &[&str] = &[
    "\u{1F64F}", // 🙏
];

const EXCITED_EMOJI: &[&str] = &[
    "\u{1F389}", // 🎉
    "\u{1F38A}", // 🎊
    "\u{2728}",  // ✨
    "\u{1F525}", // 🔥
    "\u{1F680}", // 🚀
    "\u{1F44F}", // 👏
    "\u{1F4AA}", // 💪
    "\u{1F31F}", // 🌟
];

const SAD_EMOJI: &[&str] = &[
    "\u{1F622}", // 😢
    "\u{1F625}", // 😥
    "\u{1F62D}", // 😭
    "\u{1F614}", // 😔 Pensive Face
    "\u{1F61E}", // 😞 Disappointed Face
    "\u{1F494}", // 💔
];

const FRUSTRATED_EMOJI: &[&str] = &[
    "\u{1F624}", // 😤
    "\u{1F621}", // 😡
    "\u{1F620}", // 😠
    "\u{1F926}", // 🤦
    "\u{1F62B}", // 😫
    "\u{1F629}", // 😩
    "\u{1F44E}", // 👎
];

const LOVING_EMOJI: &[&str] = &[
    "\u{2764}",  // ❤️
    "\u{1F495}", // 💕
    "\u{1F497}", // 💗
    "\u{1F496}", // 💖
    "\u{1F60D}", // 😍
    "\u{1F970}", // 🥰
    "\u{1F917}", // 🤗
];

// ── Keyword patterns ────────────────────────────────────────────

// zh-TW keywords
const HAPPY_ZH: &[&str] = &["開心", "高興", "滿意", "愉快", "太好了", "很好", "不錯"];
const GRATEFUL_ZH: &[&str] = &["謝謝", "感謝", "感恩", "多謝", "謝啦", "感激"];
const EXCITED_ZH: &[&str] = &["太棒了", "太厲害", "完美", "成功", "搞定", "完成", "讚", "酷"];
const SAD_ZH: &[&str] = &["難過", "傷心", "失落", "可惜", "遺憾", "唉"];
const FRUSTRATED_ZH: &[&str] = &["好難", "不懂", "怎麼辦", "救命", "崩潰", "頭痛", "糟糕", "失敗", "卡住"];
const LOVING_ZH: &[&str] = &["愛你", "好溫暖", "窩心", "貼心", "暖心"];

// en keywords
const HAPPY_EN: &[&str] = &["happy", "glad", "nice", "good", "cool"];
const GRATEFUL_EN: &[&str] = &["thank", "thanks", "appreciate", "grateful"];
const EXCITED_EN: &[&str] = &["perfect", "awesome", "excellent", "amazing", "wonderful", "great", "fantastic", "incredible"];
const SAD_EN: &[&str] = &["sad", "disappointed", "unfortunate", "pity", "sorry to hear"];
const FRUSTRATED_EN: &[&str] = &["difficult", "confused", "frustrated", "stuck", "broken", "failed", "help me", "struggling"];
const LOVING_EN: &[&str] = &["love it", "love this", "love you", "warmth", "caring"];

// Assistant reply context patterns (bonus signals)
const CONTEXT_EXCITED: &[&str] = &["恭喜", "成功", "完成", "congratulations", "well done", "mission accomplished"];
const CONTEXT_LOVING: &[&str] = &["辛苦了", "別擔心", "沒問題的", "理解", "加油", "i understand", "don't worry", "you got this"];

/// Detect emotion from a user message and the assistant's reply.
///
/// Pure function, no side effects, zero LLM cost.
pub fn detect_emotion(user_text: &str, assistant_reply: &str) -> EmotionSignal {
    if user_text.is_empty() {
        return EmotionSignal::neutral();
    }

    // Truncate to avoid unnecessary work on very long messages.
    // Emotion signals are typically in the first ~500 chars.
    let user_text = truncate_utf8(user_text, 512);
    let assistant_reply = truncate_utf8(assistant_reply, 512);

    let user_lower = user_text.to_lowercase();
    let reply_lower = assistant_reply.to_lowercase();

    // Score each emotion category
    let scores = [
        (EmotionType::Happy, score_emotion(user_text, &user_lower, &reply_lower, HAPPY_EMOJI, HAPPY_ZH, HAPPY_EN, &[])),
        (EmotionType::Grateful, score_emotion(user_text, &user_lower, &reply_lower, GRATEFUL_EMOJI, GRATEFUL_ZH, GRATEFUL_EN, &[])),
        (EmotionType::Excited, score_emotion(user_text, &user_lower, &reply_lower, EXCITED_EMOJI, EXCITED_ZH, EXCITED_EN, CONTEXT_EXCITED)),
        (EmotionType::Sad, score_emotion(user_text, &user_lower, &reply_lower, SAD_EMOJI, SAD_ZH, SAD_EN, &[])),
        (EmotionType::Frustrated, score_emotion(user_text, &user_lower, &reply_lower, FRUSTRATED_EMOJI, FRUSTRATED_ZH, FRUSTRATED_EN, &[])),
        (EmotionType::Loving, score_emotion(user_text, &user_lower, &reply_lower, LOVING_EMOJI, LOVING_ZH, LOVING_EN, CONTEXT_LOVING)),
    ];

    // Pick highest-scoring emotion
    let (emotion, raw_score) = scores
        .iter()
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .copied()
        .unwrap_or((EmotionType::Neutral, 0.0));

    if raw_score < 0.1 {
        return EmotionSignal::neutral();
    }

    // Normalize to 0.0-1.0 (raw score typically 0.0-2.0+)
    let intensity = (raw_score / 1.5).min(1.0);

    EmotionSignal { emotion, intensity }
}

/// Score a single emotion category.
fn score_emotion(
    user_text: &str,
    user_lower: &str,
    reply_lower: &str,
    emoji_patterns: &[&str],
    zh_keywords: &[&str],
    en_keywords: &[&str],
    context_patterns: &[&str],
) -> f32 {
    let mut score: f32 = 0.0;

    // Emoji matches — strongest signal (0.4 per match)
    let emoji_count = emoji_patterns.iter().filter(|e| user_text.contains(*e)).count();
    score += emoji_count as f32 * 0.4;

    // zh-TW keyword matches (0.25 per match)
    let zh_count = zh_keywords.iter().filter(|kw| user_lower.contains(*kw)).count();
    score += zh_count as f32 * 0.25;

    // en keyword matches (0.25 per match)
    let en_count = en_keywords.iter().filter(|kw| user_lower.contains(*kw)).count();
    score += en_count as f32 * 0.25;

    // Assistant reply context bonus (0.2 per match)
    let ctx_count = context_patterns.iter().filter(|kw| reply_lower.contains(*kw)).count();
    score += ctx_count as f32 * 0.2;

    score
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_grateful_zh() {
        // "謝謝" (grateful) + "太棒了" (excited) — both score 0.25 each,
        // but "太棒了" is in excited list making excited slightly higher with more keywords.
        // Pure grateful test:
        let sig = detect_emotion("謝謝你！感謝", "不客氣！");
        assert_eq!(sig.emotion, EmotionType::Grateful);
        assert!(sig.intensity >= 0.1);
    }

    #[test]
    fn test_grateful_en() {
        let sig = detect_emotion("thanks, that's perfect!", "You're welcome!");
        // "thanks" → Grateful, "perfect" → Excited; both score similarly
        assert!(sig.emotion == EmotionType::Grateful || sig.emotion == EmotionType::Excited);
        assert!(sig.intensity >= 0.1);
    }

    #[test]
    fn test_happy_emoji() {
        let sig = detect_emotion("好的 😊👍", "");
        assert_eq!(sig.emotion, EmotionType::Happy);
        assert!(sig.intensity >= 0.3);
    }

    #[test]
    fn test_loving_emoji() {
        let sig = detect_emotion("❤️ 太感謝了", "");
        assert_eq!(sig.emotion, EmotionType::Loving);
    }

    #[test]
    fn test_excited_with_context() {
        let sig = detect_emotion("搞定了！", "恭喜！任務完成！");
        assert_eq!(sig.emotion, EmotionType::Excited);
        assert!(sig.intensity >= 0.3);
    }

    #[test]
    fn test_sad_emoji() {
        let sig = detect_emotion("好可惜 😢", "");
        assert_eq!(sig.emotion, EmotionType::Sad);
    }

    #[test]
    fn test_frustrated_zh() {
        let sig = detect_emotion("好難，我不懂怎麼辦", "");
        assert_eq!(sig.emotion, EmotionType::Frustrated);
        assert!(sig.intensity >= 0.3);
    }

    #[test]
    fn test_frustrated_en() {
        let sig = detect_emotion("I'm stuck and confused, help me", "");
        assert_eq!(sig.emotion, EmotionType::Frustrated);
    }

    #[test]
    fn test_neutral_short() {
        let sig = detect_emotion("嗯", "好的");
        assert_eq!(sig.emotion, EmotionType::Neutral);
        assert!(sig.intensity < 0.1);
    }

    #[test]
    fn test_neutral_ok() {
        let sig = detect_emotion("ok", "");
        assert_eq!(sig.emotion, EmotionType::Neutral);
    }

    #[test]
    fn test_empty_input() {
        let sig = detect_emotion("", "some reply");
        assert_eq!(sig.emotion, EmotionType::Neutral);
        assert!(sig.intensity == 0.0);
    }

    #[test]
    fn test_mixed_high_intensity() {
        let sig = detect_emotion("謝謝 😊 太棒了！完美！", "恭喜完成！");
        // Multiple signals should produce high intensity
        assert!(sig.intensity >= 0.5);
    }

    #[test]
    fn test_loving_context_bonus() {
        let sig = detect_emotion("好", "辛苦了，別擔心！");
        assert_eq!(sig.emotion, EmotionType::Loving);
    }

    #[test]
    fn test_excited_celebration() {
        let sig = detect_emotion("🎉🎊 太厲害了！", "");
        assert_eq!(sig.emotion, EmotionType::Excited);
        assert!(sig.intensity >= 0.5);
    }

    #[test]
    fn test_negative_emoji_cluster() {
        let sig = detect_emotion("😤😡 糟糕失敗了", "");
        assert_eq!(sig.emotion, EmotionType::Frustrated);
        assert!(sig.intensity >= 0.5);
    }

    #[test]
    fn test_grateful_wins_over_excited() {
        // "謝謝" (grateful) + "太棒了" (excited) — grateful has emoji boost
        let sig = detect_emotion("謝謝 🙏", "");
        assert_eq!(sig.emotion, EmotionType::Grateful);
    }

    #[test]
    fn test_pure_emoji_message() {
        let sig = detect_emotion("👍", "");
        assert_eq!(sig.emotion, EmotionType::Happy);
        assert!(sig.intensity > 0.0);
    }

    #[test]
    fn test_english_love() {
        let sig = detect_emotion("I love this! 😍", "");
        assert_eq!(sig.emotion, EmotionType::Loving);
    }

    #[test]
    fn test_case_insensitive() {
        let sig = detect_emotion("THANKS! PERFECT!", "");
        assert!(sig.emotion == EmotionType::Grateful || sig.emotion == EmotionType::Excited);
        assert!(sig.intensity >= 0.1);
    }

    #[test]
    fn test_sad_with_sympathy() {
        let sig = detect_emotion("好傷心 💔", "");
        assert_eq!(sig.emotion, EmotionType::Sad);
    }
}
