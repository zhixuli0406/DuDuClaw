//! ACP (Agent Communication Protocol) reverse-RPC types.
//!
//! These types model the file-system and permission messages that a remote
//! orchestrator can send back to the agent via the reverse-RPC channel.

use serde::{Deserialize, Serialize};

/// Parameters for reading a file via reverse-RPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsReadFileParams {
    pub path: String,
}

/// Result of a reverse-RPC file read.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsReadFileResult {
    pub content: String,
}

/// Parameters for writing a file via reverse-RPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsWriteFileParams {
    pub path: String,
    pub content: String,
}

/// Parameters for requesting tool-use permission via reverse-RPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestPermissionParams {
    pub tool_name: String,
    pub description: String,
}

/// Result of a permission request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestPermissionResult {
    pub granted: bool,
}

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
