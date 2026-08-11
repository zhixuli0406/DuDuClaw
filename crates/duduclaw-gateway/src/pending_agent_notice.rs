//! Read-once "something about this agent's rules changed" marker.
//!
//! ## The gap this closes
//!
//! `contract.update` (CONTRACT.toml) and a `[model] preferred` hot-switch
//! both take effect starting the agent's *next* turn — but nothing tells the
//! person currently talking to the agent on a channel that anything changed.
//! They can only infer it after the fact from a behavior shift. This module
//! is the L1 FYI fix: a tiny pending flag per agent, set at the moment the
//! change lands, read (and cleared) exactly once by the next channel reply
//! that agent sends. It is never pushed as its own message — calm-notify
//! principle: piggyback on a reply that was going out anyway, never create
//! new noise for a change nobody asked to be alerted about.
//!
//! ## Persistence
//!
//! A small per-home JSON file rather than in-memory state: the writer
//! (`handlers.rs`'s `contract.update` RPC, `channel_reply::update_agent_toml_with`)
//! and the reader (the channel reply pipeline) are not guaranteed to run in
//! the same process in every deployment shape, and a gateway restart between
//! "contract saved" and "next reply sent" must not silently drop the FYI.
//! Read-modify-write is wrapped in [`duduclaw_core::with_file_lock`] so a
//! concurrent write from one path and a clearing read from another never
//! interleave into a corrupt file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const STATE_FILE_NAME: &str = "pending_agent_notice_state.json";

/// Per-agent pending flags. Each field is `Some(rfc3339 timestamp)` while
/// pending, `None` once read — a field is never re-armed by reading it, only
/// by a fresh change landing.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct PendingNotice {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    contract_changed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model_changed_at: Option<String>,
}

impl PendingNotice {
    fn is_empty(&self) -> bool {
        self.contract_changed_at.is_none() && self.model_changed_at.is_none()
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct NoticeState {
    #[serde(default)]
    agents: HashMap<String, PendingNotice>,
}

fn state_path(home_dir: &Path) -> PathBuf {
    home_dir.join(STATE_FILE_NAME)
}

fn load_state(home_dir: &Path) -> NoticeState {
    std::fs::read_to_string(state_path(home_dir))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_state(home_dir: &Path, state: &NoticeState) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(state)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(state_path(home_dir), json)
}

/// Mark that `agent_id`'s CONTRACT.toml just changed. Best-effort: a write
/// failure here (unwritable home dir, disk full) must never fail the
/// `contract.update` RPC that calls this — the contract write already
/// succeeded by the time this runs; worst case is a missed FYI line, not a
/// blocked config change.
pub fn mark_contract_changed(home_dir: &Path, agent_id: &str) {
    mark_changed(home_dir, agent_id, true, false);
}

/// Mark that `agent_id`'s `[model] preferred` just changed. Same best-effort
/// posture as [`mark_contract_changed`].
pub fn mark_model_changed(home_dir: &Path, agent_id: &str) {
    mark_changed(home_dir, agent_id, false, true);
}

fn mark_changed(home_dir: &Path, agent_id: &str, contract: bool, model: bool) {
    let path = state_path(home_dir);
    let now = chrono::Utc::now().to_rfc3339();
    let _ = duduclaw_core::with_file_lock(&path, || {
        let mut state = load_state(home_dir);
        let entry = state.agents.entry(agent_id.to_string()).or_default();
        if contract {
            entry.contract_changed_at = Some(now.clone());
        }
        if model {
            entry.model_changed_at = Some(now.clone());
        }
        save_state(home_dir, &state)
    });
}

/// Read-once: returns the FYI suffix line(s) pending for `agent_id` (contract
/// first, then model, when both fired between replies) and clears whichever
/// flags it consumed so the same change is never announced twice. `None`
/// means nothing is pending — callers must leave the reply untouched in that
/// case rather than appending an empty string (an empty-but-present suffix
/// would still alter the reply for no reason).
pub fn take_pending_notice_suffix(home_dir: &Path, agent_id: &str) -> Option<String> {
    let path = state_path(home_dir);
    duduclaw_core::with_file_lock(&path, || {
        let mut state = load_state(home_dir);
        let mut lines = Vec::new();
        let mut now_empty = false;
        if let Some(entry) = state.agents.get_mut(agent_id) {
            if let Some(ts) = entry.contract_changed_at.take() {
                lines.push(format!(
                    "（行為規則已於 {} 更新）",
                    format_local_mm_dd_hhmm(&ts)
                ));
            }
            if entry.model_changed_at.take().is_some() {
                lines.push("（已切換至新模型）".to_string());
            }
            now_empty = entry.is_empty();
        }
        if lines.is_empty() {
            return Ok(None);
        }
        if now_empty {
            state.agents.remove(agent_id);
        }
        save_state(home_dir, &state)?;
        Ok(Some(lines.join("\n")))
    })
    .ok()
    .flatten()
}

/// Render an RFC3339 instant as a person-readable local `MM-DD HH:mm` clock.
/// Falls back to a vague "近期" (recently) on a malformed timestamp rather
/// than surfacing raw ISO 8601 to the end user.
fn format_local_mm_dd_hhmm(rfc3339: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .map(|dt| {
            dt.with_timezone(&chrono::Local)
                .format("%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|_| "近期".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_pending_notice_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(take_pending_notice_suffix(dir.path(), "agent-1"), None);
    }

    #[test]
    fn contract_change_is_read_once_then_cleared() {
        let dir = tempfile::tempdir().unwrap();
        mark_contract_changed(dir.path(), "agent-1");

        let first = take_pending_notice_suffix(dir.path(), "agent-1");
        assert!(first.is_some());
        assert!(first.unwrap().contains("行為規則已於"));

        // Read-once: the second read (no new change landed) is None.
        assert_eq!(take_pending_notice_suffix(dir.path(), "agent-1"), None);
    }

    #[test]
    fn model_change_is_read_once_then_cleared() {
        let dir = tempfile::tempdir().unwrap();
        mark_model_changed(dir.path(), "agent-1");

        let first = take_pending_notice_suffix(dir.path(), "agent-1");
        assert_eq!(first, Some("（已切換至新模型）".to_string()));
        assert_eq!(take_pending_notice_suffix(dir.path(), "agent-1"), None);
    }

    #[test]
    fn both_pending_render_as_two_lines_and_both_clear() {
        let dir = tempfile::tempdir().unwrap();
        mark_contract_changed(dir.path(), "agent-1");
        mark_model_changed(dir.path(), "agent-1");

        let suffix = take_pending_notice_suffix(dir.path(), "agent-1").unwrap();
        assert!(suffix.contains("行為規則已於"));
        assert!(suffix.contains("已切換至新模型"));
        assert_eq!(suffix.lines().count(), 2);

        assert_eq!(take_pending_notice_suffix(dir.path(), "agent-1"), None);
    }

    #[test]
    fn different_agents_do_not_leak_into_each_other() {
        let dir = tempfile::tempdir().unwrap();
        mark_contract_changed(dir.path(), "agent-1");
        assert_eq!(take_pending_notice_suffix(dir.path(), "agent-2"), None);
        // agent-1's flag must still be pending — a lookup miss for a
        // different agent must not consume or corrupt agent-1's entry.
        assert!(take_pending_notice_suffix(dir.path(), "agent-1").is_some());
    }

    #[test]
    fn missing_state_file_degrades_to_none_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        // No mark_* call ever happened — the state file doesn't exist yet.
        assert_eq!(take_pending_notice_suffix(dir.path(), "ghost"), None);
    }

    #[test]
    fn malformed_timestamp_degrades_to_placeholder() {
        assert_eq!(format_local_mm_dd_hhmm("not-a-timestamp"), "近期");
    }

    #[test]
    fn well_formed_timestamp_renders_mm_dd_hh_mm() {
        let rendered = format_local_mm_dd_hhmm("2026-08-11T03:04:05Z");
        // Exactly "MM-DD HH:mm" shape (11 chars: 2+1+2+1+2+1+2).
        assert_eq!(rendered.len(), 11);
        assert!(rendered.contains('-') && rendered.contains(':'));
    }
}
