//! ConversationOutcome — zero-LLM conversation result detection.
//!
//! Detects task type, user satisfaction, and task completion from conversation
//! messages using pattern matching (no LLM calls). Feeds into:
//! - PredictionEngine composite error (task_completion signal)
//! - MistakeNotebook (auto-records failures)
//!
//! Designed for zh-TW + en bilingual detection.

use serde::{Deserialize, Serialize};

use crate::session::SessionMessage;

/// Detected task type — inferred from message content patterns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskType {
    /// General conversation, greetings, small talk.
    Chat,
    /// Question-answering: user asks factual/how-to/why questions.
    QA,
    /// Code writing, debugging, review.
    Coding,
    /// Task planning, step decomposition, project management.
    Planning,
    /// Could not determine.
    Unknown,
}

/// User satisfaction signal — inferred from final messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SatisfactionSignal {
    Positive,
    Negative,
    Neutral,
}

/// Extracted conversation outcome — all zero-LLM detection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationOutcome {
    pub session_id: String,
    pub agent_id: String,
    pub task_type: TaskType,
    pub satisfaction: SatisfactionSignal,
    pub task_completed: Option<bool>,
    pub correction_count: u32,
    pub explicit_feedback: Option<String>,
}

impl ConversationOutcome {
    /// Extract outcome from session messages (zero LLM cost).
    pub fn extract(
        session_id: &str,
        agent_id: &str,
        messages: &[SessionMessage],
    ) -> Self {
        let user_msgs: Vec<&SessionMessage> = messages
            .iter()
            .filter(|m| m.role == "user")
            .collect();

        let all_user_text: String = user_msgs
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
            .join(" ");

        let task_type = detect_task_type(&all_user_text, messages);
        let satisfaction = detect_satisfaction(&user_msgs);
        let task_completed = detect_task_completion(&user_msgs, &satisfaction);
        let correction_count = count_corrections(&user_msgs);
        let explicit_feedback = detect_explicit_feedback(&user_msgs);

        Self {
            session_id: session_id.to_string(),
            agent_id: agent_id.to_string(),
            task_type,
            satisfaction,
            task_completed,
            correction_count,
            explicit_feedback,
        }
    }

    /// Whether this outcome indicates a failure worth recording.
    pub fn is_failure(&self) -> bool {
        self.satisfaction == SatisfactionSignal::Negative
            || self.task_completed == Some(false)
            || self.correction_count >= 2
    }
}

/// Detect task type from conversation content.
fn detect_task_type(all_user_text: &str, messages: &[SessionMessage]) -> TaskType {
    let lower = all_user_text.to_lowercase();
    let has_code_block = messages.iter().any(|m| m.content.contains("```"));

    // Coding signals — use multi-word phrases to reduce false positives (review #5/#6)
    let coding_keywords_en = ["source code", "function", "debug", "compile", "bug fix", "implement", "refactor"];
    let coding_keywords_zh = [
        "\u{5BEB}\u{7A0B}\u{5F0F}", // 寫程式 (write code)
        "\u{7A0B}\u{5F0F}\u{78BC}", // 程式碼 (source code)
        "\u{4EE3}\u{78BC}",         // 代碼 (code)
        "\u{6E2C}\u{8A66}",         // 測試 (test)
        "\u{932F}\u{8AA4}",         // 錯誤 (error)
    ];

    let coding_score: usize = coding_keywords_en.iter().filter(|kw| lower.contains(*kw)).count()
        + coding_keywords_zh.iter().filter(|kw| lower.contains(*kw)).count()
        + if has_code_block { 3 } else { 0 }; // code block is a strong signal

    if coding_score >= 2 {
        return TaskType::Coding;
    }

    // Planning signals
    let planning_keywords_en = ["plan", "step", "roadmap", "strategy", "schedule", "phase"];
    let planning_keywords_zh = [
        "\u{8A08}\u{756B}", // 計畫
        "\u{6B65}\u{9A5F}", // 步驟
        "\u{898F}\u{5283}", // 規劃
        "\u{65B9}\u{6848}", // 方案
        "\u{6D41}\u{7A0B}", // 流程
    ];

    let planning_score: usize = planning_keywords_en.iter().filter(|kw| lower.contains(*kw)).count()
        + planning_keywords_zh.iter().filter(|kw| lower.contains(*kw)).count();

    if planning_score >= 2 {
        return TaskType::Planning;
    }

    // QA signals: questions
    let has_question = lower.contains('?')
        || lower.contains('\u{FF1F}')
        || lower.contains("\u{4EC0}\u{9EBC}")  // 什麼
        || lower.contains("\u{600E}\u{9EBC}")  // 怎麼
        || lower.contains("\u{70BA}\u{4EC0}\u{9EBC}") // 為什麼
        || lower.contains("\u{5982}\u{4F55}")  // 如何
        || lower.contains("how ")
        || lower.contains("what ")
        || lower.contains("why ")
        || lower.contains("where ");

    if has_question && messages.len() <= 6 {
        return TaskType::QA;
    }

    // Short conversations are likely chat
    if messages.len() <= 4 && !has_question {
        return TaskType::Chat;
    }

    TaskType::Unknown
}

/// Detect user satisfaction from the last few messages.
fn detect_satisfaction(user_msgs: &[&SessionMessage]) -> SatisfactionSignal {
    // Look at last 2 user messages
    let tail: Vec<&&SessionMessage> = user_msgs.iter().rev().take(2).collect();
    if tail.is_empty() {
        return SatisfactionSignal::Neutral;
    }

    // Negative and positive keyword lists
    let positive_en = ["thank", "thanks", "perfect", "great", "awesome", "excellent", "good job", "works", "nice"];
    let positive_zh = [
        "\u{8B1D}\u{8B1D}", "\u{611F}\u{8B1D}", "\u{592A}\u{68D2}",
        "\u{5F88}\u{597D}", "\u{8B9A}", "\u{5B8C}\u{7F8E}", "\u{53EF}\u{4EE5}",
    ];
    let positive_emoji = ["\u{1F44D}", "\u{2764}", "\u{1F389}"];
    let negative_en = ["wrong", "bad", "terrible", "useless", "doesn't work", "not what i"];
    let negative_zh = [
        "\u{4E0D}\u{5C0D}", "\u{932F}\u{4E86}", "\u{6C92}\u{7528}",
        "\u{592A}\u{7226}", "\u{4E0D}\u{884C}",
    ];
    let negative_emoji = ["\u{1F44E}"];

    // Check NEGATIVES first — they override positives in the same message (review R2-3).
    for msg in &tail {
        let lower = msg.content.to_lowercase();
        if negative_en.iter().any(|kw| lower.contains(kw))
            || negative_zh.iter().any(|kw| lower.contains(kw))
            || negative_emoji.iter().any(|e| msg.content.contains(e))
        {
            return SatisfactionSignal::Negative;
        }
    }

    // Then check positives
    for msg in &tail {
        let lower = msg.content.to_lowercase();
        if positive_en.iter().any(|kw| lower.contains(kw))
            || positive_zh.iter().any(|kw| lower.contains(kw))
            || positive_emoji.iter().any(|e| msg.content.contains(e))
        {
            return SatisfactionSignal::Positive;
        }
    }

    SatisfactionSignal::Neutral
}

/// Detect whether the task was completed.
fn detect_task_completion(
    user_msgs: &[&SessionMessage],
    satisfaction: &SatisfactionSignal,
) -> Option<bool> {
    if user_msgs.is_empty() {
        return None;
    }

    let last = &user_msgs.last()?.content.to_lowercase();

    // Completion signals
    let done_en = ["done", "works", "solved", "fixed", "got it", "that's it"];
    let done_zh = [
        "\u{597D}\u{4E86}",     // 好了
        "\u{641E}\u{5B9A}",     // 搞定
        "\u{5B8C}\u{6210}",     // 完成
        "\u{6C92}\u{554F}\u{984C}", // 沒問題
        "\u{53EF}\u{4EE5}\u{4E86}", // 可以了
    ];

    if done_en.iter().any(|kw| last.contains(kw))
        || done_zh.iter().any(|kw| last.contains(kw))
    {
        return Some(true);
    }

    // Strong negative = not completed
    if *satisfaction == SatisfactionSignal::Negative {
        return Some(false);
    }

    // Positive satisfaction on a task usually means completion
    if *satisfaction == SatisfactionSignal::Positive {
        return Some(true);
    }

    None // Indeterminate
}

/// Count correction patterns in user messages.
/// Uses multi-word phrases to avoid false positives (review R2-5).
fn count_corrections(user_msgs: &[&SessionMessage]) -> u32 {
    let correction_en = ["that's wrong", "that is wrong", "incorrect", "try again", "not what i", "please redo"];
    let correction_zh = [
        "\u{932F}\u{4E86}",     // 錯了
        "\u{4E0D}\u{5C0D}",     // 不對
        "\u{91CD}\u{4F86}",     // 重來
    ];

    let mut count = 0u32;
    for msg in user_msgs {
        let lower = msg.content.to_lowercase();
        if correction_en.iter().any(|kw| lower.contains(kw))
            || correction_zh.iter().any(|kw| lower.contains(kw))
        {
            count += 1;
        }
    }
    count
}

/// Detect explicit emoji/text feedback.
fn detect_explicit_feedback(user_msgs: &[&SessionMessage]) -> Option<String> {
    for msg in user_msgs.iter().rev().take(3) {
        let content = &msg.content;
        if content.contains('\u{1F44D}') {
            return Some("\u{1F44D}".to_string());
        }
        if content.contains('\u{1F44E}') {
            return Some("\u{1F44E}".to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_msg(role: &str, content: &str) -> SessionMessage {
        SessionMessage {
            role: role.to_string(),
            content: content.to_string(),
            tokens: 10,
            timestamp: "2026-04-05T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn test_task_type_coding() {
        let msgs = vec![
            make_msg("user", "幫我寫一個 Python function"),
            make_msg("assistant", "好的，這是一個 function:\n```python\ndef foo(): pass\n```"),
        ];
        let outcome = ConversationOutcome::extract("s1", "a1", &msgs);
        assert_eq!(outcome.task_type, TaskType::Coding);
    }

    #[test]
    fn test_task_type_qa() {
        let msgs = vec![
            make_msg("user", "什麼是 Active Inference?"),
            make_msg("assistant", "Active Inference 是..."),
        ];
        let outcome = ConversationOutcome::extract("s1", "a1", &msgs);
        assert_eq!(outcome.task_type, TaskType::QA);
    }

    #[test]
    fn test_satisfaction_positive_zh() {
        let msgs = vec![
            make_msg("user", "幫我翻譯"),
            make_msg("assistant", "翻譯結果..."),
            make_msg("user", "謝謝！太棒了"),
        ];
        let outcome = ConversationOutcome::extract("s1", "a1", &msgs);
        assert_eq!(outcome.satisfaction, SatisfactionSignal::Positive);
    }

    #[test]
    fn test_satisfaction_negative() {
        let msgs = vec![
            make_msg("user", "幫我算"),
            make_msg("assistant", "答案是 5"),
            make_msg("user", "錯了，應該是 4"),
        ];
        let outcome = ConversationOutcome::extract("s1", "a1", &msgs);
        assert_eq!(outcome.satisfaction, SatisfactionSignal::Negative);
        assert!(outcome.is_failure());
    }

    #[test]
    fn test_task_completion_done() {
        let msgs = vec![
            make_msg("user", "幫我修 bug"),
            make_msg("assistant", "已修復"),
            make_msg("user", "好了，搞定"),
        ];
        let outcome = ConversationOutcome::extract("s1", "a1", &msgs);
        assert_eq!(outcome.task_completed, Some(true));
    }

    #[test]
    fn test_task_completion_not_done() {
        let msgs = vec![
            make_msg("user", "幫我修 bug"),
            make_msg("assistant", "已修復"),
            make_msg("user", "不對，還是壞的"),
        ];
        let outcome = ConversationOutcome::extract("s1", "a1", &msgs);
        assert_eq!(outcome.task_completed, Some(false));
    }

    #[test]
    fn test_correction_count() {
        let msgs = vec![
            make_msg("user", "翻譯這段"),
            make_msg("assistant", "..."),
            make_msg("user", "不對，重來"),
            make_msg("assistant", "..."),
            make_msg("user", "還是錯了"),
        ];
        let outcome = ConversationOutcome::extract("s1", "a1", &msgs);
        assert_eq!(outcome.correction_count, 2);
    }

    #[test]
    fn test_emoji_feedback() {
        let msgs = vec![
            make_msg("user", "幫我"),
            make_msg("assistant", "好"),
            make_msg("user", "👍"),
        ];
        let outcome = ConversationOutcome::extract("s1", "a1", &msgs);
        assert_eq!(outcome.explicit_feedback, Some("👍".to_string()));
        assert_eq!(outcome.satisfaction, SatisfactionSignal::Positive);
    }
}
