//! PTC routing logic — decides whether to use PTC scripts or standard JSON tool calls.

/// Decision outcome from the PTC router.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PtcDecision {
    /// Use a PTC script for batched multi-tool execution.
    UsePtcScript,
    /// Use standard JSON tool calls (one at a time).
    UseJsonToolCall,
}

/// Router that decides when PTC scripts are beneficial over individual tool calls.
///
/// Heuristics:
/// - High recent tool call count suggests a tool-heavy task that benefits from batching.
/// - Certain keywords in the user message suggest multi-step workflows.
pub struct PtcRouter {
    enabled: bool,
}

/// Minimum recent tool calls before PTC is considered beneficial.
const TOOL_CALL_THRESHOLD: u32 = 3;

/// Keywords that suggest multi-step tool-heavy tasks.
const MULTI_STEP_KEYWORDS: &[&str] = &[
    "batch",
    "for each",
    "iterate",
    "loop",
    "all of",
    "every",
    "search and",
    "find and",
    "collect",
    "aggregate",
];

impl PtcRouter {
    /// Create a new PTC router.
    pub fn new(enabled: bool) -> Self {
        Self { enabled }
    }

    /// Decide whether to use PTC based on the user message and recent tool call count.
    pub fn should_use_ptc(&self, user_message: &str, recent_tool_call_count: u32) -> PtcDecision {
        if !self.enabled {
            return PtcDecision::UseJsonToolCall;
        }

        // High tool call count in recent turns suggests batching would help
        if recent_tool_call_count >= TOOL_CALL_THRESHOLD {
            return PtcDecision::UsePtcScript;
        }

        // Check for multi-step keywords in the user message
        let lower = user_message.to_lowercase();
        for keyword in MULTI_STEP_KEYWORDS {
            if lower.contains(keyword) {
                return PtcDecision::UsePtcScript;
            }
        }

        PtcDecision::UseJsonToolCall
    }

    /// Check if the InferenceRouter suggests using a local model for this query.
    /// If so, PTC is less likely to be needed (simple queries stay as JSON tool calls).
    pub fn with_inference_hint(
        &self,
        user_message: &str,
        recent_tool_call_count: u32,
        is_complex_query: bool,
    ) -> PtcDecision {
        if !self.enabled {
            return PtcDecision::UseJsonToolCall;
        }

        // If inference router flagged as complex, more likely to benefit from PTC
        if is_complex_query {
            return PtcDecision::UsePtcScript;
        }

        self.should_use_ptc(user_message, recent_tool_call_count)
    }
}
