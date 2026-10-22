//! The normalised thinking event every runtime maps its own signal onto.
//!
//! By construction there is no field for a signature, `redacted_thinking`
//! ciphertext or an encrypted reasoning item: those stay inside the runtime
//! that needs to replay them and can never reach a viewer.

/// What the text of a block is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingKind {
    /// Short between-tool progress notes (Anthropic `display: "updates"`).
    Progress,
    /// A vendor-written summary of the reasoning (Anthropic summarized,
    /// OpenAI / Codex reasoning summary).
    Summary,
    /// The model's own reasoning text (open models: `reasoning_content`,
    /// `<think>`). Shown only when redaction is enabled.
    Raw,
    /// Withheld: encrypted by the vendor, or by our own outbound checks.
    Redacted,
    /// The model thought but returned no text (new-model default).
    Omitted,
}

impl ThinkingKind {
    /// Stable wire token (WebChat `thinking` frame `kind`).
    pub fn as_str(self) -> &'static str {
        match self {
            ThinkingKind::Progress => "progress",
            ThinkingKind::Summary => "summary",
            ThinkingKind::Raw => "raw",
            ThinkingKind::Redacted => "redacted",
            ThinkingKind::Omitted => "omitted",
        }
    }
}

/// Where a block is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingPhase {
    Start,
    Update,
    End,
}

impl ThinkingPhase {
    /// Stable wire token (WebChat `thinking` frame `phase`).
    pub fn as_str(self) -> &'static str {
        match self {
            ThinkingPhase::Start => "start",
            ThinkingPhase::Update => "update",
            ThinkingPhase::End => "end",
        }
    }
}

/// Block id of the whole-turn usage event ([`super::Recorder::turn_usage`]).
pub const TURN_BLOCK_ID: &str = "turn";

/// One thinking signal.
///
/// Producer side: `text` is the delta on `Update` and the full text on `End`.
/// After the gate: `text` is a masked, truncated snapshot of the whole block
/// so far, and `line` is its last non-empty line (≤ 80 chars).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThinkingEvent {
    /// `"t0"`, `"t1"`, … per block; [`TURN_BLOCK_ID`] for turn usage.
    pub block_id: String,
    pub phase: ThinkingPhase,
    pub kind: ThinkingKind,
    pub text: Option<String>,
    /// Filled by the gate.
    pub line: Option<String>,
    /// Unix ms, gateway clock.
    pub started_at: Option<u64>,
    /// `End` only.
    pub duration_ms: Option<u64>,
    /// Estimate on `Update`, final count on `End`.
    pub tokens: Option<u64>,
    pub truncated: bool,
    /// Set by the gate when raw reasoning was dropped because redaction is
    /// off (the viewer sees seconds and tokens and a hint to enable it).
    pub raw_needs_redaction: bool,
    /// `claude_cli` | `anthropic_api` | `codex` | `agy` | `openai_compat` |
    /// `direct_api`.
    pub source: &'static str,
}

impl ThinkingEvent {
    /// A bare event; producers fill in the rest.
    pub fn new(
        block_id: impl Into<String>,
        phase: ThinkingPhase,
        kind: ThinkingKind,
        source: &'static str,
    ) -> Self {
        Self {
            block_id: block_id.into(),
            phase,
            kind,
            text: None,
            line: None,
            started_at: None,
            duration_ms: None,
            tokens: None,
            truncated: false,
            raw_needs_redaction: false,
            source,
        }
    }

    /// The same event with every text field removed (what a viewer that may
    /// only see timing receives).
    pub fn without_text(mut self) -> Self {
        self.text = None;
        self.line = None;
        self
    }

    /// Is this the whole-turn usage event?
    pub fn is_turn_usage(&self) -> bool {
        self.block_id == TURN_BLOCK_ID
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_tokens_are_stable() {
        let kinds: Vec<&str> = [
            ThinkingKind::Progress,
            ThinkingKind::Summary,
            ThinkingKind::Raw,
            ThinkingKind::Redacted,
            ThinkingKind::Omitted,
        ]
        .iter()
        .map(|k| k.as_str())
        .collect();
        assert_eq!(kinds, ["progress", "summary", "raw", "redacted", "omitted"]);
        assert_eq!(ThinkingPhase::Update.as_str(), "update");
        let ev = ThinkingEvent {
            text: Some("x".into()),
            line: Some("x".into()),
            ..ThinkingEvent::new(
                "t0",
                ThinkingPhase::Update,
                ThinkingKind::Summary,
                "claude_cli",
            )
        };
        let bare = ev.without_text();
        assert!(bare.text.is_none() && bare.line.is_none());
        assert!(!bare.is_turn_usage());
    }
}
