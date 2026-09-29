//! One tool invocation's outcome plus the mask-then-truncate helper that is
//! the only way raw tool text reaches a [`LoopToolCall`]. Moved verbatim out
//! of `tool_loop.rs` (file-size split).

use super::*;

/// Mask ([`duduclaw_security::audit::mask_sensitive_text`]) then
/// CJK-safe-truncate raw text before it is allowed onto a [`LoopToolCall`].
/// The ONLY path that may populate `LoopToolCall::result_text`/`input_text`
/// — masking always runs before truncation (a secret split across the
/// truncation boundary must still be caught). An empty/all-whitespace result
/// is `None`, never an empty-string placeholder.
pub(super) fn mask_and_cap(text: &str, max_chars: usize) -> Option<String> {
    let masked = duduclaw_security::audit::mask_sensitive_text(text);
    let trimmed = masked.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(duduclaw_core::truncate_chars(trimmed, max_chars))
}

/// The outcome of one tool invocation, as seen by the loop.
///
/// `is_error` maps straight onto [`ContentPart::ToolResult::is_error`] — a
/// tool that ran but failed (validation, upstream 500, `isError` from an MCP
/// server) sets `is_error = true` while still returning descriptive
/// `content`, so the model gets a chance to react rather than the loop
/// aborting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutcome {
    pub content: String,
    pub is_error: bool,
    /// Verified by the executor's trusted connector, never parsed from tool
    /// output text. Generic MCP results leave this unset.
    pub source_artifact: Option<CcrSourceArtifact>,
    /// Optional upstream retention deadline supplied by a trusted connector.
    /// A bound CCR entry expires no later than this source deadline.
    pub source_retention_at: Option<i64>,
    /// Whether this result may be stored in CCR. A verified route with failed
    /// source attestation must also withhold its content from the model.
    pub ccr_eligible: bool,
    /// A refused attestation must retire any older handle for the same call
    /// ID before the sanitized tool result is delivered.
    pub ccr_revoke_call: bool,
}

impl ToolOutcome {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
            source_artifact: None,
            source_retention_at: None,
            ccr_eligible: true,
            ccr_revoke_call: false,
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
            source_artifact: None,
            source_retention_at: None,
            ccr_eligible: false,
            ccr_revoke_call: false,
        }
    }

    pub fn with_source_artifact(mut self, artifact: CcrSourceArtifact) -> Self {
        self.source_artifact = Some(artifact);
        self
    }

    pub fn with_source_retention_at(mut self, retention_at: i64) -> Self {
        self.source_retention_at = Some(retention_at);
        self
    }

    pub fn without_ccr(mut self) -> Self {
        self.ccr_eligible = false;
        self.ccr_revoke_call = true;
        self
    }
}
