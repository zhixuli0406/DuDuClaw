//! PTC (Process-to-Claude) types for script execution and RPC.

use serde::{Deserialize, Serialize};

/// Supported scripting languages for PTC execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScriptLanguage {
    Python,
    Bash,
}

/// A request to execute a script in the PTC sandbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptRequest {
    /// The script source code to execute.
    pub script: String,
    /// Language of the script.
    pub language: ScriptLanguage,
    /// Maximum execution time in milliseconds.
    pub timeout_ms: u64,
    /// Maximum output bytes before truncation.
    pub max_output_bytes: usize,
}

/// Result of a PTC script execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    /// Number of MCP tool calls made via RPC during execution.
    pub tool_calls_count: u64,
    /// Wall-clock execution time in milliseconds.
    pub execution_ms: u64,
    /// Whether the output was truncated.
    pub truncated: bool,
}
