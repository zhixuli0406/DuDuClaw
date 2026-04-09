//! Classifies tool call results into fidelity tiers for context optimization.

use serde::{Deserialize, Serialize};

/// Fidelity tier for a tool call result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolResultFidelity {
    /// Remove entirely (health checks, pings, redundant results).
    Discard,
    /// Short stub: "[tool: {name}, turn {n}, result archived]"
    Placeholder,
    /// Heuristic truncation: first 200 chars + "..." + last 100 chars.
    Compressed,
    /// Keep complete result (recent or user-referenced).
    Full,
}

impl ToolResultFidelity {
    /// Numeric rank for ordering: higher means more data retained.
    fn rank(self) -> u8 {
        match self {
            Self::Discard => 0,
            Self::Placeholder => 1,
            Self::Compressed => 2,
            Self::Full => 3,
        }
    }

    /// Return the higher-fidelity of two tiers.
    pub fn max(self, other: Self) -> Self {
        if self.rank() >= other.rank() {
            self
        } else {
            other
        }
    }
}

/// Metadata about a tool call result for classification.
#[derive(Debug, Clone)]
pub struct ToolCallMeta {
    /// Name of the MCP tool that was called.
    pub tool_name: String,
    /// The raw result text.
    pub result_text: String,
    /// How many turns ago this result was generated.
    pub turn_age: usize,
    /// Whether the user referenced this result in a subsequent message.
    pub referenced_by_user: bool,
    /// Token count estimate of the result.
    pub token_estimate: u32,
}

/// CJK-aware token estimation: CJK chars ≈ 1 token each, ASCII ≈ 4 chars per token.
pub fn estimate_tokens(text: &str) -> u32 {
    let mut cjk = 0u32;
    let mut ascii = 0u32;
    for ch in text.chars() {
        if ch as u32 > 0x2E80 {
            cjk += 1;
        } else {
            ascii += 1;
        }
    }
    cjk + (ascii + 3) / 4
}

/// Classifies tool results into fidelity tiers.
pub struct ToolResultClassifier {
    /// Number of recent turns to keep at Full fidelity.
    recency_window: usize,
    /// Number of turns to keep at Compressed fidelity.
    compressed_window: usize,
}

/// Tool names that should always be discarded (case-insensitive substring match).
const DISCARD_TOOL_NAMES: &[&str] = &["health_check", "ping"];

/// Trivial result values that carry no useful information (case-insensitive exact match).
const DISCARD_TRIVIAL_RESULTS: &[&str] = &["ok", "success", "pong", "true", "false"];

impl ToolResultClassifier {
    /// Create a classifier with default windows (recency=2, compressed=5)
    /// and built-in discard patterns for health checks and trivial responses.
    pub fn new() -> Self {
        Self::with_windows(2, 5)
    }

    /// Create a classifier with custom recency and compressed windows.
    pub fn with_windows(recency: usize, compressed: usize) -> Self {
        Self {
            recency_window: recency,
            compressed_window: compressed,
        }
    }

    /// Check if a tool name matches any discard pattern (case-insensitive).
    fn is_discard_tool_name(name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        DISCARD_TOOL_NAMES.iter().any(|pat| lower.contains(pat))
    }

    /// Check if a result is a trivial short response (case-insensitive exact match).
    fn is_trivial_result(text: &str) -> bool {
        let lower = text.to_ascii_lowercase();
        DISCARD_TRIVIAL_RESULTS.iter().any(|pat| lower == *pat)
    }

    /// Check if result looks like a JSON health status response: `{"status": "ok"...`
    fn is_json_health_response(text: &str) -> bool {
        // Match `{ "status": "ok"` with flexible whitespace
        let s = text.trim_start();
        if !s.starts_with('{') {
            return false;
        }
        let after_brace = s[1..].trim_start();
        if !after_brace.starts_with("\"status\"") {
            return false;
        }
        let after_key = after_brace["\"status\"".len()..].trim_start();
        if !after_key.starts_with(':') {
            return false;
        }
        let after_colon = after_key[1..].trim_start();
        after_colon.starts_with("\"ok\"")
    }

    /// Classify a single tool result into a fidelity tier.
    pub fn classify(&self, meta: &ToolCallMeta) -> ToolResultFidelity {
        // 1. User-referenced results are always Full.
        if meta.referenced_by_user {
            return ToolResultFidelity::Full;
        }

        // 2. Recent results stay Full.
        if meta.turn_age <= self.recency_window {
            return ToolResultFidelity::Full;
        }

        // 3. Check discard patterns on tool name.
        if Self::is_discard_tool_name(&meta.tool_name) {
            return ToolResultFidelity::Discard;
        }

        // 4. Check trivial short results (< 10 chars).
        let trimmed = meta.result_text.trim();
        if trimmed.len() < 10 && Self::is_trivial_result(trimmed) {
            return ToolResultFidelity::Discard;
        }

        // 5. Check JSON health status pattern regardless of length.
        if Self::is_json_health_response(trimmed) {
            return ToolResultFidelity::Discard;
        }

        // 6. Within compressed window -> Compressed.
        if meta.turn_age <= self.compressed_window {
            return ToolResultFidelity::Compressed;
        }

        // 7. Everything else -> Placeholder.
        ToolResultFidelity::Placeholder
    }

    /// Apply a fidelity classification to produce the compressed result string.
    pub fn apply_fidelity(fidelity: ToolResultFidelity, meta: &ToolCallMeta) -> String {
        match fidelity {
            ToolResultFidelity::Full => meta.result_text.clone(),
            ToolResultFidelity::Compressed => {
                let text = &meta.result_text;
                if text.len() <= 300 {
                    // Short enough — no truncation needed.
                    text.clone()
                } else {
                    let head: String = text.chars().take(200).collect();
                    let tail: String = text
                        .chars()
                        .rev()
                        .take(100)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect();
                    format!("{}...{}", head, tail)
                }
            }
            ToolResultFidelity::Placeholder => {
                format!(
                    "[tool: {}, called at turn {}, result archived]",
                    meta.tool_name, meta.turn_age
                )
            }
            ToolResultFidelity::Discard => String::new(),
        }
    }

    /// Batch-process a list of tool results, returning the processed texts
    /// and a summary of savings.
    pub fn process_batch(&self, results: &mut Vec<ToolCallMeta>) -> BatchResult {
        let mut original_tokens: u32 = 0;
        let mut compressed_tokens: u32 = 0;
        let mut full_count: usize = 0;
        let mut compressed_count: usize = 0;
        let mut placeholder_count: usize = 0;
        let mut discarded_count: usize = 0;

        for meta in results.iter_mut() {
            let orig_est = estimate_tokens(&meta.result_text);
            original_tokens = original_tokens.saturating_add(orig_est);

            let fidelity = self.classify(meta);
            let new_text = Self::apply_fidelity(fidelity, meta);
            let new_est = estimate_tokens(&new_text);
            compressed_tokens = compressed_tokens.saturating_add(new_est);

            match fidelity {
                ToolResultFidelity::Full => full_count += 1,
                ToolResultFidelity::Compressed => compressed_count += 1,
                ToolResultFidelity::Placeholder => placeholder_count += 1,
                ToolResultFidelity::Discard => discarded_count += 1,
            }

            meta.result_text = new_text;
        }

        BatchResult {
            original_tokens,
            compressed_tokens,
            tokens_saved: original_tokens.saturating_sub(compressed_tokens),
            full_count,
            compressed_count,
            placeholder_count,
            discarded_count,
        }
    }
}

impl Default for ToolResultClassifier {
    fn default() -> Self {
        Self::new()
    }
}

/// Summary of batch processing results.
#[derive(Debug, Clone)]
pub struct BatchResult {
    pub original_tokens: u32,
    pub compressed_tokens: u32,
    pub tokens_saved: u32,
    pub full_count: usize,
    pub compressed_count: usize,
    pub placeholder_count: usize,
    pub discarded_count: usize,
}
