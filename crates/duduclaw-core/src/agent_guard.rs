//! Agent-structure write guard (CLI-S5 / Option 3 hardening).
//!
//! Prevents agents from silently creating parallel agent hierarchies outside
//! the canonical `<duduclaw_home>/agents/` directory by using the raw Write /
//! Edit tools. Agents should use the `create_agent` MCP tool instead.
//!
//! This is enforced via a Claude Code `PreToolUse` hook that runs the
//! `duduclaw hook agent-file-guard` subcommand. The subcommand delegates to
//! [`check_agent_file_write`] below.

use std::path::{Component, Path, PathBuf};

/// Filenames that indicate an agent-structure file.
///
/// Writes to these filenames are only allowed under `<home>/agents/<name>/`
/// (any depth below an agent directory is fine — e.g. `wiki/`, `SKILLS/`).
///
/// This intentionally covers the file that's *checked in to every agent*.
/// Additional sentinel files can be added here without touching call sites.
pub const AGENT_STRUCTURE_FILES: &[&str] = &[
    "agent.toml",
    "SOUL.md",
    "CLAUDE.md",
    "MEMORY.md",
    ".mcp.json",
    "CONTRACT.toml",
];

/// Outcome of the guard check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardDecision {
    /// Path is safe — let the tool call proceed.
    Allow,
    /// Path is a non-agent-structure file — not our concern.
    NotAgentFile,
    /// Path is an agent-structure file under the canonical agents dir.
    AllowedAgentWrite,
    /// Path is an agent-structure file but lives *outside* `<home>/agents/`.
    /// The caller should block the tool call and tell the user to use
    /// the `create_agent` MCP tool instead.
    BlockedOutsideHome {
        file_name: String,
        attempted_path: PathBuf,
    },
}

impl GuardDecision {
    pub fn is_allowed(&self) -> bool {
        matches!(
            self,
            Self::Allow | Self::NotAgentFile | Self::AllowedAgentWrite
        )
    }

    /// Format a user-facing block message suitable for surfacing through
    /// Claude Code's hook stderr (which the agent sees in-conversation).
    pub fn block_message(&self) -> Option<String> {
        match self {
            Self::BlockedOutsideHome { file_name, attempted_path } => Some(format!(
                "Blocked: refusing to write agent-structure file '{}' outside DuDuClaw home.\n\
                 Attempted path: {}\n\
                 Agents must be created via the `create_agent` MCP tool. \
                 Do not use Write/Edit to scaffold agents at arbitrary locations — \
                 the dashboard and registry only recognise agents under ~/.duduclaw/agents/<name>/.",
                file_name,
                attempted_path.display()
            )),
            _ => None,
        }
    }
}

/// Check whether a Write / Edit / MultiEdit `file_path` is permitted.
///
/// # Policy
/// - If `file_path`'s basename is not in [`AGENT_STRUCTURE_FILES`] → `NotAgentFile`
/// - If it *is* and the path lives under `<home>/agents/<name>/...` → `AllowedAgentWrite`
/// - Otherwise → `BlockedOutsideHome`
///
/// The `file_path` is lexically normalized (resolves `..` / `.` / repeated
/// separators) without touching the filesystem, so the guard works even
/// when the target file does not yet exist. Symlinks are **not** resolved
/// (the agent has no control over symlinks on the host, so following them
/// would only create TOCTOU risk without blocking any realistic attack).
///
/// `home` is typically `<user_home>/.duduclaw` (`DUDUCLAW_HOME` env var).
pub fn check_agent_file_write(file_path: &Path, home: &Path) -> GuardDecision {
    let Some(file_name) = file_path.file_name().and_then(|n| n.to_str()) else {
        return GuardDecision::NotAgentFile;
    };

    if !AGENT_STRUCTURE_FILES.contains(&file_name) {
        return GuardDecision::NotAgentFile;
    }

    let normalized = lexical_normalize(file_path);
    let agents_root = lexical_normalize(&home.join("agents"));

    // Must be strictly under <home>/agents/<some-name>/...
    //
    // Using components lets us avoid a false positive where a file path
    // is *equal to* `<home>/agents/` itself (which has no <name> segment),
    // and also avoids being fooled by sibling paths like `<home>/agentsX/`.
    if normalized.starts_with(&agents_root) {
        let suffix: Vec<_> = normalized
            .strip_prefix(&agents_root)
            .map(|p| p.components().collect())
            .unwrap_or_default();
        // Need at least one component (the agent name) after `agents/`,
        // plus the file basename — so >= 2 components total.
        if suffix.len() >= 2 {
            return GuardDecision::AllowedAgentWrite;
        }
    }

    GuardDecision::BlockedOutsideHome {
        file_name: file_name.to_string(),
        attempted_path: normalized,
    }
}

/// Lexical path normalization — resolves `.`, `..`, and duplicate separators
/// without touching the filesystem. Does **not** follow symlinks or require
/// the path to exist.
///
/// Extracted as a helper so guard tests can exercise it independently.
pub fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::ParentDir => {
                // Don't pop past the root/prefix.
                if !matches!(
                    out.components().next_back(),
                    Some(Component::RootDir)
                        | Some(Component::Prefix(_))
                        | None
                ) {
                    out.pop();
                }
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn home() -> PathBuf {
        PathBuf::from("/Users/alice/.duduclaw")
    }

    #[test]
    fn write_to_canonical_agent_dir_is_allowed() {
        let p = PathBuf::from("/Users/alice/.duduclaw/agents/mybot/agent.toml");
        assert_eq!(
            check_agent_file_write(&p, &home()),
            GuardDecision::AllowedAgentWrite
        );
    }

    #[test]
    fn write_to_nested_path_in_canonical_dir_is_allowed() {
        let p = PathBuf::from("/Users/alice/.duduclaw/agents/mybot/subteam/SOUL.md");
        assert_eq!(
            check_agent_file_write(&p, &home()),
            GuardDecision::AllowedAgentWrite
        );
    }

    #[test]
    fn write_to_project_dir_is_blocked() {
        let p = PathBuf::from("/Users/alice/Project/agents/tl-xianwen/SOUL.md");
        let decision = check_agent_file_write(&p, &home());
        match decision {
            GuardDecision::BlockedOutsideHome { file_name, .. } => {
                assert_eq!(file_name, "SOUL.md");
            }
            other => panic!("expected BlockedOutsideHome, got {other:?}"),
        }
    }

    #[test]
    fn write_to_sibling_agentsx_is_blocked() {
        // `/Users/alice/.duduclaw/agentsX/foo/SOUL.md` looks similar but is
        // not under `/Users/alice/.duduclaw/agents/`.
        let p = PathBuf::from("/Users/alice/.duduclaw/agentsX/foo/SOUL.md");
        assert!(matches!(
            check_agent_file_write(&p, &home()),
            GuardDecision::BlockedOutsideHome { .. }
        ));
    }

    #[test]
    fn write_to_agents_root_without_name_is_blocked() {
        // Directly writing <home>/agents/SOUL.md — missing the <name> segment.
        let p = PathBuf::from("/Users/alice/.duduclaw/agents/SOUL.md");
        assert!(matches!(
            check_agent_file_write(&p, &home()),
            GuardDecision::BlockedOutsideHome { .. }
        ));
    }

    #[test]
    fn non_agent_files_are_not_our_concern() {
        let p = PathBuf::from("/Users/alice/Project/DuDuClaw/src/main.rs");
        assert_eq!(
            check_agent_file_write(&p, &home()),
            GuardDecision::NotAgentFile
        );
    }

    #[test]
    fn mcp_json_in_canonical_dir_is_allowed() {
        let p = PathBuf::from("/Users/alice/.duduclaw/agents/mybot/.mcp.json");
        assert_eq!(
            check_agent_file_write(&p, &home()),
            GuardDecision::AllowedAgentWrite
        );
    }

    #[test]
    fn mcp_json_outside_is_blocked() {
        let p = PathBuf::from("/Users/alice/Project/x/.mcp.json");
        assert!(matches!(
            check_agent_file_write(&p, &home()),
            GuardDecision::BlockedOutsideHome { .. }
        ));
    }

    #[test]
    fn relative_path_with_parent_traversal_is_resolved() {
        // Edit tool can be called with a relative path from the agent's cwd.
        // After normalization it must still land inside the canonical dir
        // or be blocked.
        let p = PathBuf::from("/Users/alice/.duduclaw/agents/mybot/../../../../evil/agent.toml");
        assert!(matches!(
            check_agent_file_write(&p, &home()),
            GuardDecision::BlockedOutsideHome { .. }
        ));
    }

    #[test]
    fn contract_toml_is_covered() {
        let p = PathBuf::from("/Users/alice/Project/agents/x/CONTRACT.toml");
        assert!(matches!(
            check_agent_file_write(&p, &home()),
            GuardDecision::BlockedOutsideHome { .. }
        ));
    }

    #[test]
    fn block_message_contains_create_agent_hint() {
        let decision = GuardDecision::BlockedOutsideHome {
            file_name: "agent.toml".to_string(),
            attempted_path: PathBuf::from("/tmp/x/agent.toml"),
        };
        let msg = decision.block_message().unwrap();
        assert!(msg.contains("create_agent"));
        assert!(msg.contains("agent.toml"));
        assert!(msg.contains("/tmp/x/agent.toml"));
    }

    #[test]
    fn guard_decision_is_allowed_classification() {
        assert!(GuardDecision::Allow.is_allowed());
        assert!(GuardDecision::NotAgentFile.is_allowed());
        assert!(GuardDecision::AllowedAgentWrite.is_allowed());
        assert!(!GuardDecision::BlockedOutsideHome {
            file_name: "x".to_string(),
            attempted_path: PathBuf::from("/x"),
        }
        .is_allowed());
    }

    #[test]
    fn lexical_normalize_handles_dot_and_dotdot() {
        assert_eq!(
            lexical_normalize(Path::new("/a/b/./c/../d")),
            PathBuf::from("/a/b/d")
        );
    }

    #[test]
    fn lexical_normalize_does_not_escape_root() {
        assert_eq!(
            lexical_normalize(Path::new("/../../x")),
            PathBuf::from("/x")
        );
    }
}
