//! Conversation metrics extraction — per-conversation signal collector.
//!
//! Extracts behavioural signals from a completed conversation without
//! calling any LLM. Used by the PredictionEngine to calculate prediction errors.
//!
//! ## Hardening (2025-Q2)
//!
//! - **Feedback grading**: `FeedbackSeverity` replaces binary correction counting.
//!   Based on EMNLP 2025 "User Feedback in Human-LLM Dialogues" taxonomy.
//! - **Cultural context**: `CulturalContext` adjusts signal interpretation for
//!   high-context cultures (zh-TW). Based on CHI 2024 cross-cultural analysis.
//! - **Indirect disagreement**: Detects zh-TW indirect negation patterns.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::session::SessionMessage;

// ---------------------------------------------------------------------------
// Cultural context configuration
// ---------------------------------------------------------------------------

/// Cultural context for adjusting behavioural signal interpretation.
///
/// High-context cultures (East Asian) use indirect communication patterns
/// where silence may indicate agreement and indirect phrasing signals
/// disagreement. (CHI 2024, ScienceDirect 2025)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CulturalContext {
    /// IANA locale (e.g., "zh-TW", "en-US").
    pub locale: String,
    /// High-context culture: silence/short replies may mean agreement.
    pub high_context: bool,
    /// Character count below which a reply is considered "short" (CJK default: 15).
    pub short_reply_threshold: usize,
    /// Weight for silence-as-agreement interpretation (0.0-1.0).
    pub silence_as_agreement_weight: f64,
    /// Weight for indirect disagreement signals (0.0-1.0).
    pub indirect_disagreement_weight: f64,
}

impl Default for CulturalContext {
    fn default() -> Self {
        Self {
            locale: "zh-TW".to_string(),
            high_context: true,
            short_reply_threshold: 15,
            silence_as_agreement_weight: 0.7,
            indirect_disagreement_weight: 0.3,
        }
    }
}

// ---------------------------------------------------------------------------
// Feedback severity grading
// ---------------------------------------------------------------------------

/// Graded severity of user feedback — replaces binary correction counting.
///
/// Taxonomy from EMNLP 2025 "User Feedback in Human-LLM Dialogues":
/// ExplicitCorrection > AwareWithoutFix > Rephrasing > Clarification
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FeedbackSeverity {
    /// User explicitly says agent is wrong AND provides correct answer.
    /// e.g., "你錯了，應該是 X", "that's wrong, it should be X"
    ExplicitCorrection,
    /// User indicates error but doesn't provide a fix.
    /// e.g., "不對", "incorrect"
    AwareWithoutFix,
    /// User restates their previous message (overlap > 60%).
    /// Implies previous answer was unsatisfactory.
    Rephrasing,
    /// User clarifies their own intent, not correcting the agent.
    /// e.g., "不是，我的意思是...", "no I mean..."
    Clarification,
    /// Indirect disagreement (high-context cultures).
    /// e.g., "可能", "或許", "有沒有其他"
    IndirectDisagreement,
}

impl FeedbackSeverity {
    /// Weight of this feedback type for satisfaction inference.
    pub fn weight(self) -> f64 {
        match self {
            Self::ExplicitCorrection => 1.0,
            Self::AwareWithoutFix => 0.6,
            Self::Rephrasing => 0.3,
            Self::IndirectDisagreement => 0.3,
            Self::Clarification => 0.1,
        }
    }
}

/// Detailed feedback breakdown for a conversation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FeedbackDetail {
    /// Count per severity level.
    pub severity_counts: HashMap<String, u32>,
    /// Weighted correction score (sum of count * weight for each severity).
    pub weighted_correction_score: f64,
    /// Total raw correction count (backwards-compatible).
    pub raw_correction_count: u32,
}

// ---------------------------------------------------------------------------
// Question type detection (zh-TW aware)
// ---------------------------------------------------------------------------

/// Detected question type — used to distinguish genuine follow-ups from
/// acknowledgment in high-context cultures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionType {
    /// 嗎/呢/？ — yes/no question
    YesNo,
    /// 怎麼/如何 — how-to question
    HowTo,
    /// 為什麼/為何 — why question
    Why,
    /// Not a question — statement or acknowledgment
    Statement,
}

fn detect_question_type(text: &str) -> QuestionType {
    // Check for question marks first
    let has_question_mark = text.contains('?') || text.contains('\u{FF1F}');

    if text.contains('\u{55CE}') || text.contains('\u{5462}') {
        // 嗎, 呢
        QuestionType::YesNo
    } else if text.contains("\u{600E}\u{9EBC}") || text.contains("\u{5982}\u{4F55}") {
        // 怎麼, 如何
        QuestionType::HowTo
    } else if text.contains("\u{70BA}\u{4EC0}\u{9EBC}") || text.contains("\u{70BA}\u{4F55}") {
        // 為什麼, 為何
        QuestionType::Why
    } else if has_question_mark {
        QuestionType::YesNo
    } else {
        QuestionType::Statement
    }
}

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

    /// Number of detected user corrections (raw count, backwards-compatible).
    pub user_corrections: u32,

    /// Graded feedback details — replaces binary correction counting.
    /// When present, `engine.rs` uses `weighted_correction_score` instead of
    /// `user_corrections * 0.3`.
    #[serde(default)]
    pub feedback_details: FeedbackDetail,

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

    /// Concatenated user message text for embedding computation.
    /// Not serialized to reduce storage size — recomputed on demand.
    #[serde(skip)]
    pub user_text: String,
}

impl ConversationMetrics {
    /// Extract metrics from session messages.
    ///
    /// This is a pure function — no LLM calls, no I/O.
    /// Pass `cultural_context` to adjust signal interpretation for the agent's
    /// target culture. Use `None` for default (zh-TW high-context).
    pub fn extract(
        session_id: &str,
        agent_id: &str,
        user_id: &str,
        messages: &[SessionMessage],
        response_time_ms: u64,
    ) -> Self {
        Self::extract_with_culture(session_id, agent_id, user_id, messages, response_time_ms, None)
    }

    /// Extract with explicit cultural context.
    pub fn extract_with_culture(
        session_id: &str,
        agent_id: &str,
        user_id: &str,
        messages: &[SessionMessage],
        response_time_ms: u64,
        cultural_context: Option<&CulturalContext>,
    ) -> Self {
        let culture = cultural_context.cloned().unwrap_or_default();
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

        // Count follow-ups (culture-aware)
        let user_follow_ups = count_follow_ups_cultural(messages, &culture);

        // Graded feedback analysis (replaces binary correction counting)
        let feedback_details = classify_feedback(&user_msgs, &culture);
        let user_corrections = feedback_details.raw_correction_count;

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
            feedback_details,
            detected_language,
            extracted_topics,
            ended_naturally: true,
            feedback_signal: None,
            timestamp: Utc::now(),
            user_text: all_user_text,
        }
    }
}

/// Count follow-up patterns with cultural awareness.
///
/// In high-context cultures (zh-TW), short replies without question markers
/// may indicate agreement/acknowledgment, not follow-up questions.
/// (CHI 2024 "Cross-Cultural Perceptions of AI Conversational Agents")
fn count_follow_ups_cultural(messages: &[SessionMessage], culture: &CulturalContext) -> u32 {
    let mut count = 0u32;
    for window in messages.windows(3) {
        if window[0].role == "user"
            && window[1].role == "assistant"
            && window[2].role == "user"
        {
            let next_user = &window[2].content;
            let char_count = next_user.chars().count();
            let question_type = detect_question_type(next_user);

            if culture.high_context {
                // High-context: short reply without question = likely acknowledgment
                if char_count < culture.short_reply_threshold
                    && question_type == QuestionType::Statement
                {
                    continue; // Skip — silence/short reply = agreement
                }
                // Only count as follow-up if it contains a question or is long enough
                if question_type != QuestionType::Statement
                    || char_count >= culture.short_reply_threshold
                {
                    count += 1;
                }
            } else {
                // Low-context: short or question = follow-up
                // Use chars().count() for consistency (len() returns bytes, not characters)
                if char_count < 50
                    || next_user.contains('?')
                    || next_user.contains('\u{FF1F}')
                {
                    count += 1;
                }
            }
        }
    }
    count
}

/// Classify user feedback with graded severity.
///
/// Replaces binary `count_corrections` with a severity-weighted system.
/// Based on EMNLP 2025 "User Feedback in Human-LLM Dialogues" taxonomy:
/// ExplicitCorrection > AwareWithoutFix > Rephrasing > Clarification
fn classify_feedback(
    user_msgs: &[&SessionMessage],
    culture: &CulturalContext,
) -> FeedbackDetail {
    let mut severity_counts: HashMap<String, u32> = HashMap::new();
    let mut weighted_score = 0.0_f64;
    let mut raw_count = 0u32;

    // Clarification indicators — user is clarifying their OWN intent
    let clarification_suffixes_zh = [
        "\u{6211}\u{7684}\u{610F}\u{601D}",     // 我的意思
        "\u{6211}\u{662F}\u{8AAA}",               // 我是說
        "\u{6211}\u{60F3}\u{8981}",               // 我想要
        "\u{6211}\u{60F3}\u{8AAA}\u{7684}\u{662F}", // 我想說的是
        "\u{6211}\u{7684}\u{610F}\u{601D}\u{662F}", // 我的意思是
    ];

    // Explicit correction indicators — user says agent is wrong
    let explicit_correction_zh = [
        "\u{932F}\u{4E86}",     // 錯了
        "\u{4E0D}\u{5C0D}",     // 不對
        "\u{91CD}\u{4F86}",     // 重來
        "\u{4FEE}\u{6539}",     // 修改
    ];
    let explicit_correction_en = [
        "that's wrong",
        "incorrect",
        "please fix",
        "try again",
    ];

    // Negation that could be either correction or clarification
    let ambiguous_negation_zh = [
        "\u{4E0D}\u{662F}",     // 不是
        "\u{4E0D}\u{8981}",     // 不要
    ];
    let ambiguous_negation_en = [
        "not what i",
        "no, ",
    ];

    // Indirect disagreement (high-context cultures).
    //
    // IMPORTANT: Single-word patterns like "可能", "但是", "不過" are extremely
    // common in normal Chinese and would cause massive false positives.
    // Only multi-word phrases that strongly imply disagreement are included.
    // (CHI 2024: indirect signals must be contextually disambiguated)
    let indirect_disagreement_zh = [
        "\u{4E0D}\u{4E00}\u{5B9A}\u{662F}",             // 不一定是 (not necessarily)
        "\u{6709}\u{6C92}\u{6709}\u{5176}\u{4ED6}",     // 有沒有其他 (are there others)
        "\u{9084}\u{6709}\u{5225}\u{7684}\u{55CE}",     // 還有別的嗎 (anything else?)
        "\u{9084}\u{6709}\u{5176}\u{4ED6}\u{65B9}\u{6CD5}", // 還有其他方法 (other methods?)
        "\u{4F46}\u{6211}\u{89BA}\u{5F97}",               // 但我覺得 (but I think)
        "\u{4E0D}\u{904E}\u{6211}\u{8A8D}\u{70BA}",     // 不過我認為 (however I believe)
        "\u{53EF}\u{80FD}\u{4E0D}\u{592A}",               // 可能不太 (maybe not quite)
        "\u{6216}\u{8A31}\u{4E0D}\u{662F}",               // 或許不是 (perhaps not)
    ];

    for (i, msg) in user_msgs.iter().enumerate() {
        let content = &msg.content;
        let lower = content.to_lowercase();

        // 1. Check for clarification first (highest priority — prevents false correction)
        let is_clarification = {
            let has_negation = ambiguous_negation_zh.iter().any(|p| lower.contains(p))
                || ambiguous_negation_en.iter().any(|p| lower.contains(p));
            let has_clarification_suffix = clarification_suffixes_zh.iter().any(|p| lower.contains(p))
                || lower.contains("i mean")
                || lower.contains("what i meant");
            has_negation && has_clarification_suffix
        };

        if is_clarification {
            let sev = FeedbackSeverity::Clarification;
            *severity_counts.entry(format!("{sev:?}")).or_insert(0) += 1;
            weighted_score += sev.weight();
            raw_count += 1;
            continue;
        }

        // 2. Check for explicit correction
        let is_explicit = explicit_correction_zh.iter().any(|p| lower.contains(p))
            || explicit_correction_en.iter().any(|p| lower.contains(p));

        if is_explicit {
            let sev = FeedbackSeverity::ExplicitCorrection;
            *severity_counts.entry(format!("{sev:?}")).or_insert(0) += 1;
            weighted_score += sev.weight();
            raw_count += 1;
            continue;
        }

        // 3. Check for ambiguous negation (without clarification suffix → AwareWithoutFix)
        let has_negation = ambiguous_negation_zh.iter().any(|p| lower.contains(p))
            || ambiguous_negation_en.iter().any(|p| lower.contains(p));

        if has_negation {
            let sev = FeedbackSeverity::AwareWithoutFix;
            *severity_counts.entry(format!("{sev:?}")).or_insert(0) += 1;
            weighted_score += sev.weight();
            raw_count += 1;
            continue;
        }

        // 4. Check for rephrasing (high overlap with previous user message)
        if i > 0 {
            let prev_content = &user_msgs[i - 1].content;
            let overlap = char_bigram_jaccard(prev_content, content);
            if overlap > 0.6 {
                let sev = FeedbackSeverity::Rephrasing;
                *severity_counts.entry(format!("{sev:?}")).or_insert(0) += 1;
                weighted_score += sev.weight();
                raw_count += 1;
                continue;
            }
        }

        // 5. Check for indirect disagreement (high-context cultures only)
        if culture.high_context {
            let is_indirect = indirect_disagreement_zh.iter().any(|p| lower.contains(p));
            if is_indirect {
                let sev = FeedbackSeverity::IndirectDisagreement;
                *severity_counts.entry(format!("{sev:?}")).or_insert(0) += 1;
                weighted_score += sev.weight() * culture.indirect_disagreement_weight;
                // Note: indirect disagreement does NOT increment raw_count
                // to maintain backwards compatibility
            }
        }
    }

    FeedbackDetail {
        severity_counts,
        weighted_correction_score: weighted_score,
        raw_correction_count: raw_count,
    }
}

/// Character-bigram Jaccard similarity between two strings.
///
/// Used to detect rephrasing: if a user message has > 60% bigram overlap
/// with their previous message, it's likely a rephrase.
fn char_bigram_jaccard(a: &str, b: &str) -> f64 {
    fn bigrams(s: &str) -> HashMap<String, u32> {
        let chars: Vec<char> = s.chars().filter(|c| !c.is_whitespace()).collect();
        let mut freq = HashMap::new();
        for window in chars.windows(2) {
            let bigram: String = window.iter().collect();
            *freq.entry(bigram).or_insert(0) += 1;
        }
        freq
    }

    let a_bg = bigrams(a);
    let b_bg = bigrams(b);

    if a_bg.is_empty() && b_bg.is_empty() {
        return 0.0;
    }

    let all_keys: std::collections::HashSet<&String> =
        a_bg.keys().chain(b_bg.keys()).collect();

    let mut intersection = 0u32;
    let mut union = 0u32;
    for key in all_keys {
        let ca = a_bg.get(key).copied().unwrap_or(0);
        let cb = b_bg.get(key).copied().unwrap_or(0);
        intersection += ca.min(cb);
        union += ca.max(cb);
    }

    if union == 0 { 0.0 } else { intersection as f64 / union as f64 }
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
        // Use same CJK ranges as is_cjk() for consistency (audit #18).
        // Excludes Japanese Hiragana/Katakana (0x3040-0x30FF) and CJK punctuation (0x3000-0x303F).
        if is_cjk(cp) {
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
