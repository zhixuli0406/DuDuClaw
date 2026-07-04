//! ACP (Agent Communication Protocol) session update types.

use serde::{Deserialize, Serialize};

// ── Session update notifications (Agent → Client) ──

/// Streaming session update from agent to client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SessionUpdate {
    /// Streaming text chunk.
    #[serde(rename = "text")]
    TextChunk { session_id: String, content: String },
    /// Session completed.
    #[serde(rename = "complete")]
    Complete { session_id: String, final_message: String },
    /// Error occurred.
    #[serde(rename = "error")]
    Error { session_id: String, message: String },
}
