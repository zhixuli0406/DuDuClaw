pub mod error;
pub mod traits;
pub mod types;

pub use error::{DuDuClawError, Result};
pub use traits::{Channel, ContainerRuntime, MemoryEngine};
pub use types::*;

// ── Delegation safety constants ──────────────────────────────

/// Maximum number of agent-to-agent delegation hops before messages are
/// dropped.  Shared across MCP tools (pre-check) and the bus dispatcher
/// (runtime guard).
pub const MAX_DELEGATION_DEPTH: u8 = 5;

/// Environment variable names used to inject delegation context into
/// Claude CLI subprocesses.  The MCP server reads these to track depth
/// without relying on (spoofable) tool parameters.
pub const ENV_DELEGATION_DEPTH: &str = "DUDUCLAW_DELEGATION_DEPTH";
pub const ENV_DELEGATION_ORIGIN: &str = "DUDUCLAW_DELEGATION_ORIGIN";
pub const ENV_DELEGATION_SENDER: &str = "DUDUCLAW_DELEGATION_SENDER";

/// Validate that an agent ID is safe for filesystem and log use.
///
/// A valid agent ID contains only lowercase alphanumerics, hyphens, and
/// underscores; is non-empty; and is at most 64 characters long.
pub fn is_valid_agent_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Find the `claude` binary in PATH or common locations (BE-L1, BE-M1).
///
/// Uses async-safe `tokio::process::Command` when called from an async context.
/// This is the single shared implementation — replaces 4+ duplicates.
pub fn which_claude() -> Option<String> {
    // Check PATH via `which` (uses std::process::Command — acceptable for one-time startup)
    if let Ok(output) = std::process::Command::new("which")
        .arg("claude")
        .output()
    {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(path);
            }
        }
    }

    // Common locations
    let home = std::env::var("HOME").unwrap_or_default();
    let candidates = [
        format!("{home}/.npm-global/bin/claude"),
        "/usr/local/bin/claude".to_string(),
        format!("{home}/.claude/bin/claude"),
        format!("{home}/.local/bin/claude"),
    ];

    for path in &candidates {
        if std::path::Path::new(path).exists() {
            return Some(path.clone());
        }
    }

    None
}
