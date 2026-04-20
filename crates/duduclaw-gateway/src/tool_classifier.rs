//! Tool-result classifier for pre-compression pruning.
//!
//! Classifies tool call results by recency, size, and content patterns to
//! determine the appropriate fidelity level before expensive LLM compression.
//!
//! Reference: AFM (arXiv 2511.12712), RECOMP (ICLR 2024)

use duduclaw_core::truncate_bytes;

/// Metadata about a tool call result used for classification.
pub struct ToolCallMeta {
    /// Name of the tool that produced the result (e.g. "bash", "read_file").
    pub tool_name: String,
    /// The raw result text.
    pub result_text: String,
    /// How many turns ago this result was produced (0 = current turn).
    pub turn_age: usize,
    /// Whether a subsequent user message references this result.
    pub referenced_by_user: bool,
    /// Estimated token count for this result.
    pub token_estimate: u32,
}

/// Fidelity level for a tool result after classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolResultFidelity {
    /// Keep the full result text unchanged.
    Full,
    /// Compress: keep first/last N lines with a placeholder in the middle.
    Compressed,
    /// Replace with a short placeholder summary.
    Placeholder,
    /// Remove entirely (zero tokens).
    Discard,
}

/// Thresholds for the classifier (turn age boundaries).
const AGE_RECENT: usize = 6;
const AGE_STALE: usize = 20;

/// Minimum token count to consider compression worthwhile.
const MIN_TOKENS_FOR_COMPRESSION: u32 = 80;

/// Large result threshold — results above this are aggressively pruned when old.
const LARGE_RESULT_TOKENS: u32 = 500;

/// Number of lines to keep at head/tail in compressed mode.
const COMPRESSED_KEEP_LINES: usize = 5;

/// Classifies tool results by recency and content patterns.
pub struct ToolResultClassifier;

impl ToolResultClassifier {
    pub fn new() -> Self {
        Self
    }

    /// Classify a tool result into a fidelity level.
    ///
    /// Rules (applied in order):
    /// 1. Recent results (≤ AGE_RECENT turns) → Full
    /// 2. Referenced by user → Full
    /// 3. Small results (< MIN_TOKENS_FOR_COMPRESSION) → Full
    /// 4. Error results → Placeholder (keep error type, discard stack trace)
    /// 5. Large + stale (> AGE_STALE turns, > LARGE_RESULT_TOKENS) → Discard
    /// 6. Stale (> AGE_STALE turns) → Placeholder
    /// 7. Medium age + large → Compressed
    /// 8. Otherwise → Full
    pub fn classify(&self, meta: &ToolCallMeta) -> ToolResultFidelity {
        // Rule 1: recent results always kept
        if meta.turn_age <= AGE_RECENT {
            return ToolResultFidelity::Full;
        }

        // Rule 2: user-referenced results always kept
        if meta.referenced_by_user {
            return ToolResultFidelity::Full;
        }

        // Rule 3: small results not worth compressing
        if meta.token_estimate < MIN_TOKENS_FOR_COMPRESSION {
            return ToolResultFidelity::Full;
        }

        // Rule 4: error results — keep error type, drop verbose traces
        if Self::looks_like_error(&meta.result_text) {
            return ToolResultFidelity::Placeholder;
        }

        // Rule 5: large + very stale → discard
        if meta.turn_age > AGE_STALE && meta.token_estimate > LARGE_RESULT_TOKENS {
            return ToolResultFidelity::Discard;
        }

        // Rule 6: stale → placeholder
        if meta.turn_age > AGE_STALE {
            return ToolResultFidelity::Placeholder;
        }

        // Rule 7: medium age + large → compressed (head/tail)
        if meta.token_estimate > LARGE_RESULT_TOKENS {
            return ToolResultFidelity::Compressed;
        }

        // Rule 8: default — keep full
        ToolResultFidelity::Full
    }

    /// Apply the fidelity decision to produce a replacement string.
    pub fn apply_fidelity(fidelity: ToolResultFidelity, meta: &ToolCallMeta) -> String {
        match fidelity {
            ToolResultFidelity::Full => meta.result_text.clone(),
            ToolResultFidelity::Compressed => Self::head_tail_compress(&meta.result_text),
            ToolResultFidelity::Placeholder => {
                let preview = &meta.result_text[..meta.result_text.len().min(120)];
                format!(
                    "[Tool result pruned — {} tokens, {} turns ago] {}…",
                    meta.token_estimate, meta.turn_age, preview
                )
            }
            ToolResultFidelity::Discard => {
                format!(
                    "[Tool result discarded — {} tokens, {} turns ago]",
                    meta.token_estimate, meta.turn_age
                )
            }
        }
    }

    /// Keep first and last N lines, replace the middle with a placeholder.
    fn head_tail_compress(text: &str) -> String {
        let lines: Vec<&str> = text.lines().collect();
        if lines.len() <= COMPRESSED_KEEP_LINES * 2 {
            return text.to_string();
        }

        let head = &lines[..COMPRESSED_KEEP_LINES];
        let tail = &lines[lines.len() - COMPRESSED_KEEP_LINES..];
        let omitted = lines.len() - COMPRESSED_KEEP_LINES * 2;

        format!(
            "{}\n[… {} lines omitted …]\n{}",
            head.join("\n"),
            omitted,
            tail.join("\n")
        )
    }

    /// Heuristic: does this look like an error output?
    fn looks_like_error(text: &str) -> bool {
        let lower = text.to_lowercase();
        // Check first 500 chars for efficiency
        let prefix = truncate_bytes(&lower, 500);
        prefix.contains("error")
            || prefix.contains("traceback")
            || prefix.contains("panic")
            || prefix.contains("exception")
            || prefix.starts_with("failed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_meta(turn_age: usize, tokens: u32, content: &str) -> ToolCallMeta {
        ToolCallMeta {
            tool_name: "bash".to_string(),
            result_text: content.to_string(),
            turn_age,
            referenced_by_user: false,
            token_estimate: tokens,
        }
    }

    #[test]
    fn recent_results_always_full() {
        let c = ToolResultClassifier::new();
        let meta = make_meta(3, 1000, "lots of output");
        assert_eq!(c.classify(&meta), ToolResultFidelity::Full);
    }

    #[test]
    fn small_results_always_full() {
        let c = ToolResultClassifier::new();
        let meta = make_meta(30, 50, "small output");
        assert_eq!(c.classify(&meta), ToolResultFidelity::Full);
    }

    #[test]
    fn stale_large_results_discarded() {
        let c = ToolResultClassifier::new();
        let meta = make_meta(25, 600, "very long output from a tool");
        assert_eq!(c.classify(&meta), ToolResultFidelity::Discard);
    }

    #[test]
    fn stale_medium_results_placeholder() {
        let c = ToolResultClassifier::new();
        let meta = make_meta(25, 200, "medium output from a tool");
        assert_eq!(c.classify(&meta), ToolResultFidelity::Placeholder);
    }

    #[test]
    fn medium_age_large_results_compressed() {
        let c = ToolResultClassifier::new();
        let meta = make_meta(12, 600, "large output in mid conversation");
        assert_eq!(c.classify(&meta), ToolResultFidelity::Compressed);
    }

    #[test]
    fn error_results_become_placeholder() {
        let c = ToolResultClassifier::new();
        let meta = make_meta(10, 300, "Error: something went wrong\nTraceback ...\nmore lines");
        assert_eq!(c.classify(&meta), ToolResultFidelity::Placeholder);
    }

    #[test]
    fn referenced_results_always_full() {
        let c = ToolResultClassifier::new();
        let mut meta = make_meta(30, 1000, "old but referenced");
        meta.referenced_by_user = true;
        assert_eq!(c.classify(&meta), ToolResultFidelity::Full);
    }

    #[test]
    fn head_tail_compress_short_text() {
        let text = "line1\nline2\nline3";
        let result = ToolResultClassifier::head_tail_compress(text);
        assert_eq!(result, text); // too short to compress
    }

    #[test]
    fn head_tail_compress_long_text() {
        let lines: Vec<String> = (0..20).map(|i| format!("line {i}")).collect();
        let text = lines.join("\n");
        let result = ToolResultClassifier::head_tail_compress(&text);
        assert!(result.contains("[… 10 lines omitted …]"));
        assert!(result.contains("line 0"));
        assert!(result.contains("line 19"));
    }

    #[test]
    fn apply_fidelity_discard() {
        let meta = make_meta(30, 800, "big old result");
        let result = ToolResultClassifier::apply_fidelity(ToolResultFidelity::Discard, &meta);
        assert!(result.contains("discarded"));
        assert!(result.contains("800 tokens"));
    }
}
