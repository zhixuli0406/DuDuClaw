//! Conversation metrics extraction — per-conversation signal collector.
//!
//! Extracts behavioural signals from a completed conversation without
//! calling any LLM. Used by the PredictionEngine to calculate prediction errors.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::session::SessionMessage;

/// Signals extracted from a single completed conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationMetrics {
    pub session_id: String,
    pub user_id: String,
    pub agent_id: String,

    /// Total messages in the conversation.
    pub message_count: u32,
    pub user_message_count: u32,
    pub assistant_message_count: u32,

    /// Average length (in chars) of assistant responses.
    pub avg_assistant_response_length: f64,

    /// Total tokens consumed in this conversation.
    pub total_tokens: u32,

    /// How long the agent took to respond (milliseconds, 0 if unknown).
    pub response_time_ms: u64,

    /// Number of user follow-up questions (short user messages after assistant).
    pub user_follow_ups: u32,

    /// Number of detected user corrections.
    pub user_corrections: u32,

    /// Detected primary language of the conversation.
    pub detected_language: String,

    /// Top keywords extracted from user messages (max 5).
    pub extracted_topics: Vec<String>,

    /// Whether the conversation ended naturally (vs timeout/error).
    pub ended_naturally: bool,

    /// Optional explicit feedback signal.
    pub feedback_signal: Option<String>,

    /// Timestamp of the conversation.
    pub timestamp: DateTime<Utc>,
}

impl ConversationMetrics {
    /// Extract metrics from session messages.
    ///
    /// This is a pure function — no LLM calls, no I/O.
    pub fn extract(
        session_id: &str,
        agent_id: &str,
        user_id: &str,
        messages: &[SessionMessage],
        response_time_ms: u64,
    ) -> Self {
        let user_msgs: Vec<&SessionMessage> = messages.iter().filter(|m| m.role == "user").collect();
        let asst_msgs: Vec<&SessionMessage> = messages.iter().filter(|m| m.role == "assistant").collect();

        let user_message_count = user_msgs.len() as u32;
        let assistant_message_count = asst_msgs.len() as u32;

        // Average assistant response length
        let avg_assistant_response_length = if asst_msgs.is_empty() {
            0.0
        } else {
            let total_len: usize = asst_msgs.iter().map(|m| m.content.len()).sum();
            total_len as f64 / asst_msgs.len() as f64
        };

        // Total tokens
        let total_tokens: u32 = messages.iter().map(|m| m.tokens).sum();

        // Count follow-ups: user message right after assistant, where user msg is short or a question
        let user_follow_ups = count_follow_ups(messages);

        // Count corrections
        let user_corrections = count_corrections(&user_msgs);

        // Detect language
        let all_user_text: String = user_msgs.iter().map(|m| m.content.as_str()).collect::<Vec<_>>().join(" ");
        let detected_language = detect_language(&all_user_text);

        // Extract top keywords
        let extracted_topics = extract_keywords(&all_user_text, 5);

        Self {
            session_id: session_id.to_string(),
            user_id: user_id.to_string(),
            agent_id: agent_id.to_string(),
            message_count: messages.len() as u32,
            user_message_count,
            assistant_message_count,
            avg_assistant_response_length,
            total_tokens,
            response_time_ms,
            user_follow_ups,
            user_corrections,
            detected_language,
            extracted_topics,
            ended_naturally: true,
            feedback_signal: None,
            timestamp: Utc::now(),
        }
    }
}

/// Count follow-up patterns: user→assistant→user(short/question).
fn count_follow_ups(messages: &[SessionMessage]) -> u32 {
    let mut count = 0u32;
    for window in messages.windows(3) {
        if window[0].role == "user"
            && window[1].role == "assistant"
            && window[2].role == "user"
        {
            let next_user = &window[2].content;
            // Short follow-up or question mark → likely a follow-up
            if next_user.len() < 50 || next_user.contains('?') || next_user.contains('\u{FF1F}') {
                count += 1;
            }
        }
    }
    count
}

/// Count correction patterns in user messages.
///
/// Looks for common correction indicators in both English and Chinese.
fn count_corrections(user_msgs: &[&SessionMessage]) -> u32 {
    let correction_patterns = [
        // Chinese
        "\u{4e0d}\u{662f}", // 不是
        "\u{932f}\u{4e86}", // 錯了
        "\u{4e0d}\u{5c0d}", // 不對
        "\u{91cd}\u{4f86}", // 重來
        "\u{4e0d}\u{8981}", // 不要
        "\u{4fee}\u{6539}", // 修改
        // English
        "not what i",
        "that's wrong",
        "no, ",
        "incorrect",
        "please fix",
        "try again",
    ];

    let mut count = 0u32;
    for msg in user_msgs {
        let lower = msg.content.to_lowercase();
        if correction_patterns.iter().any(|p| lower.contains(p)) {
            count += 1;
        }
    }
    count
}

/// Simple language detection based on CJK character ratio.
fn detect_language(text: &str) -> String {
    if text.is_empty() {
        return "unknown".to_string();
    }

    let mut cjk_count = 0u32;
    let mut total_count = 0u32;

    for ch in text.chars() {
        if ch.is_whitespace() {
            continue;
        }
        total_count += 1;
        let cp = ch as u32;
        if (0x3000..=0x9FFF).contains(&cp)
            || (0xF900..=0xFAFF).contains(&cp)
            || (0x20000..=0x2A6DF).contains(&cp)
        {
            cjk_count += 1;
        }
    }

    if total_count == 0 {
        return "unknown".to_string();
    }

    let ratio = cjk_count as f64 / total_count as f64;
    if ratio > 0.3 {
        "zh".to_string()
    } else {
        "en".to_string()
    }
}

/// Extract top-k keywords from text using simple frequency counting.
///
/// For CJK text: uses character bigrams as terms.
/// For ASCII text: splits on whitespace, filters common stopwords.
pub fn extract_keywords(text: &str, top_k: usize) -> Vec<String> {
    let mut freq: HashMap<String, u32> = HashMap::new();

    // Collect CJK bigrams
    let chars: Vec<char> = text.chars().collect();
    for window in chars.windows(2) {
        let c0 = window[0] as u32;
        let c1 = window[1] as u32;
        if is_cjk(c0) && is_cjk(c1) {
            let bigram: String = window.iter().collect();
            *freq.entry(bigram).or_insert(0) += 1;
        }
    }

    // Collect ASCII words
    let stopwords = [
        "the", "a", "an", "is", "are", "was", "were", "be", "been", "being",
        "have", "has", "had", "do", "does", "did", "will", "would", "could",
        "should", "may", "might", "can", "shall", "to", "of", "in", "for",
        "on", "with", "at", "by", "from", "as", "into", "through", "during",
        "it", "its", "this", "that", "these", "those", "i", "you", "he", "she",
        "we", "they", "me", "him", "her", "us", "them", "my", "your", "his",
        "and", "or", "but", "not", "if", "then", "else", "so", "just", "also",
    ];

    for word in text.split_whitespace() {
        let cleaned: String = word.chars().filter(|c| c.is_alphanumeric()).collect();
        let lower = cleaned.to_lowercase();
        if lower.len() >= 2 && !stopwords.contains(&lower.as_str()) && lower.chars().all(|c| c.is_ascii_alphabetic()) {
            *freq.entry(lower).or_insert(0) += 1;
        }
    }

    // Sort by frequency and return top-k
    let mut entries: Vec<(String, u32)> = freq.into_iter().collect();
    entries.sort_by(|a, b| b.1.cmp(&a.1));
    entries.into_iter().take(top_k).map(|(k, _)| k).collect()
}

fn is_cjk(cp: u32) -> bool {
    (0x4E00..=0x9FFF).contains(&cp)
        || (0x3400..=0x4DBF).contains(&cp)
        || (0xF900..=0xFAFF).contains(&cp)
        || (0x20000..=0x2A6DF).contains(&cp)
}
