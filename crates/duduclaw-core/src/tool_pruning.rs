//! Observation-only discovery pruning. This module never authorizes a call.
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;
pub const CACHE_TTL: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Off,
    #[default]
    Shadow,
    On,
}
impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Shadow => "shadow",
            Self::On => "on",
        }
    }
}
fn warn_invalid() {
    static WARNED: std::sync::Once = std::sync::Once::new();
    WARNED.call_once(|| tracing::warn!("Invalid capabilities.tool_pruning; using off"));
}
impl<'de> Deserialize<'de> for Mode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        Ok(match value.as_str() {
            Some("off") => Self::Off,
            Some("shadow") => Self::Shadow,
            Some("on") => Self::On,
            _ => {
                warn_invalid();
                Self::Off
            }
        })
    }
}
pub fn read_mode(agent_dir: &Path) -> Mode {
    let Ok(raw) = std::fs::read_to_string(agent_dir.join("agent.toml")) else {
        return Mode::Off;
    };
    let Ok(table) = raw.parse::<toml::Table>() else {
        warn_invalid();
        return Mode::Off;
    };
    let Some(cap) = table.get("capabilities") else {
        return Mode::Shadow;
    };
    let Some(cap) = cap.as_table() else {
        warn_invalid();
        return Mode::Off;
    };
    match cap.get("tool_pruning") {
        None => Mode::Shadow,
        Some(value) => match value.as_str() {
            Some("off") => Mode::Off,
            Some("shadow") => Mode::Shadow,
            Some("on") => Mode::On,
            _ => {
                warn_invalid();
                Mode::Off
            }
        },
    }
}

/// Exact names; future unclassified tools are also protected by effect_of.
pub const PROTECTED_TOOLS: &[&str] = &[
    "memory_alias_add",
    "memory_alias_list",
    "memory_consolidation_status",
    "memory_episodic_pressure",
    "memory_fetch_batch",
    "memory_get_at",
    "memory_get_history",
    "memory_improve",
    "memory_read",
    "memory_search",
    "memory_search_by_layer",
    "memory_store",
    "memory_successful_conversations",
    "memory_invalidate_by_origin",
    "user_profile_record",
    "user_profile_get",
    "session_restore_context",
    "tasks_list",
    "tasks_create",
    "tasks_update",
    "tasks_claim",
    "tasks_renew",
    "tasks_complete",
    "tasks_block",
    "discovery_catalog",
    "discovery_list",
    "discovery_tree",
    "discovery_artifact",
    "discovery_cancel",
    "goals_list",
    "goals_create",
    "plan_start",
    "plan_get",
    "plan_update_step",
    "list_reminders",
    "create_reminder",
    "cancel_reminder",
    "task_status",
    "create_task",
    "list_cron_tasks",
    "update_cron_task",
    "delete_cron_task",
    "pause_cron_task",
    "run_cron_task",
    "working_state_get",
    "working_state_set",
    "working_state_clear",
    "working_state_handoff",
    "send_message",
    "send_photo",
    "send_sticker",
    "send_to_agent",
    "check_responses",
    "mail_list",
    "mail_read",
    "mail_send",
    "team_handoff",
    "responsibility_get",
    "responsibility_followup",
    "responsibility_ask",
    "wiki_dedup",
    "wiki_lint",
    "wiki_namespace_status",
    "wiki_trust_audit",
    "wiki_trust_history",
    "approval_status",
    "capability_request",
    "wiki_read",
    "wiki_ls",
    "wiki_search",
    "wiki_stats",
    "wiki_graph",
    "tools",
    "tools_list",
    "tool_search",
    "tool_discover",
];
pub fn protected(name: &str) -> bool {
    PROTECTED_TOOLS.contains(&name) || crate::effect_of(name) == crate::ToolEffect::Admin
}

#[derive(Debug, Clone, Deserialize)]
pub struct UsageRecord {
    pub timestamp: chrono::DateTime<chrono::FixedOffset>,
    pub agent_id: String,
    pub tool_name: String,
    pub success: bool,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub turn_id: String,
}

/// A turn is (session_id, turn_id), never a tool-call row or a timestamp bucket.
/// Count only host-stamped records with both identities. Legacy rows cannot
/// supply evidence of ten turns. A successful use resets that tool's window.
pub fn prunable(records: &[UsageRecord], names: &[&str]) -> BTreeSet<String> {
    let mut records: Vec<_> = records.iter().collect();
    records.sort_by_key(|r| r.timestamp);
    let turns: HashSet<_> = records
        .iter()
        .filter(|r| !r.session_id.is_empty() && !r.turn_id.is_empty())
        .map(|r| (&r.session_id, &r.turn_id))
        .collect();
    if turns.len() < 10 {
        return BTreeSet::new();
    }
    names
        .iter()
        .filter(|name| !protected(name))
        .filter(|name| {
            let last_use = records
                .iter()
                .filter(|r| r.tool_name == **name && r.success)
                .max_by_key(|r| r.timestamp);
            let window: Vec<_> = records
                .iter()
                .filter(|r| !r.session_id.is_empty() && !r.turn_id.is_empty())
                .filter(|r| {
                    last_use.is_none_or(|used| {
                        r.timestamp > used.timestamp
                            && (r.session_id != used.session_id || r.turn_id != used.turn_id)
                    })
                })
                .collect();
            let turns: HashSet<_> = window.iter().map(|r| (&r.session_id, &r.turn_id)).collect();
            let sessions: HashSet<_> = window.iter().map(|r| &r.session_id).collect();
            (turns.len() >= 10 && sessions.len() >= 2) || sessions.len() >= 5
        })
        .map(|name| (*name).to_string())
        .collect()
}

pub fn read_records(home: &Path, agent_dir: &Path, agent: &str) -> Vec<UsageRecord> {
    let marker = agent_dir.join("tool_pruning_reset_at");
    let reset = match std::fs::read_to_string(&marker) {
        Ok(text) => match chrono::DateTime::parse_from_rfc3339(text.trim()) {
            Ok(ts) => Some(ts),
            Err(_) => return Vec::new(),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return Vec::new(),
    };
    let Ok(mut file) = std::fs::File::open(home.join("tool_calls.jsonl")) else {
        return Vec::new();
    };
    let Ok(meta) = file.metadata() else {
        return Vec::new();
    };
    let start = meta.len().saturating_sub(MAX_LOG_BYTES);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut bytes = Vec::new();
    if file.take(MAX_LOG_BYTES).read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }
    let bytes = if start > 0 {
        match bytes.iter().position(|b| *b == b'\n') {
            Some(i) => &bytes[i + 1..],
            None => return Vec::new(),
        }
    } else {
        &bytes[..]
    };
    bytes
        .split(|byte| *byte == b'\n')
        .filter_map(|line| serde_json::from_slice::<UsageRecord>(line).ok())
        .filter(|r| r.agent_id == agent && reset.is_none_or(|ts| r.timestamp > ts))
        .collect()
}

struct Cached {
    at: Instant,
    marker: Option<String>,
    log_len: u64,
    names: Vec<String>,
    set: BTreeSet<String>,
}
type CacheKey = (PathBuf, PathBuf, String);
fn cache() -> &'static Mutex<HashMap<CacheKey, Cached>> {
    static CACHE: OnceLock<Mutex<HashMap<CacheKey, Cached>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}
pub fn candidates_from_files(
    home: &Path,
    agent_dir: &Path,
    agent: &str,
    names: &[&str],
) -> BTreeSet<String> {
    // Cached observations cannot outlive missing/unreadable evidence.
    let Ok(log_len) = std::fs::File::open(home.join("tool_calls.jsonl"))
        .and_then(|file| file.metadata())
        .map(|meta| meta.len())
    else {
        return BTreeSet::new();
    };
    let key = (home.to_owned(), agent_dir.to_owned(), agent.to_string());
    let marker = match std::fs::read_to_string(agent_dir.join("tool_pruning_reset_at")) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return BTreeSet::new(),
    };
    let names_owned: Vec<String> = names.iter().map(|s| (*s).into()).collect();
    let Ok(mut cache) = cache().lock() else {
        return BTreeSet::new();
    };
    if let Some(entry) = cache.get(&key) {
        if entry.at.elapsed() < CACHE_TTL
            && entry.marker == marker
            && entry.names == names_owned
            && log_len >= entry.log_len
        {
            return entry.set.clone();
        }
    }
    let set = prunable(&read_records(home, agent_dir, agent), names);
    cache.insert(
        key,
        Cached {
            at: Instant::now(),
            marker,
            log_len,
            names: names_owned,
            set: set.clone(),
        },
    );
    set
}
/// Successful calls become visible immediately; unrelated cached candidates
/// retain their five-minute observation window. No dispatch gate consults this.
pub fn note_success(home: &Path, agent: &str, name: &str) {
    if let Ok(mut entries) = cache().lock() {
        for ((h, _, a), entry) in entries.iter_mut() {
            if h == home && a == agent {
                entry.set.remove(name);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rows(turns: usize, sessions: usize) -> Vec<UsageRecord> {
        (0..turns)
            .map(|i| UsageRecord {
                timestamp: chrono::DateTime::parse_from_rfc3339("2026-11-01T00:00:00Z").unwrap()
                    + chrono::Duration::seconds(i as i64),
                agent_id: "alice".into(),
                tool_name: "memory_search".into(),
                success: true,
                session_id: format!("s{}", i % sessions),
                turn_id: format!("t{i}"),
            })
            .collect()
    }
    #[test]
    fn turn_and_session_boundaries() {
        let names = ["web_search"];
        assert!(prunable(&rows(9, 2), &names).is_empty());
        assert!(prunable(&rows(10, 1), &names).is_empty());
        assert!(prunable(&rows(10, 2), &names).contains("web_search"));
        // The five-session alternative can apply after fewer than ten new
        // turns, but still requires ten distinct turns in the whole sample.
        let mut r = rows(6, 1);
        r[5].tool_name = "web_search".into();
        for i in 6..10 {
            let mut next = rows(10, 1)[i].clone();
            next.session_id = format!("new{i}");
            r.push(next);
        }
        assert!(prunable(&r, &names).is_empty()); // 4 sessions since use
        let mut next = rows(11, 1)[10].clone();
        next.session_id = "fifth".into();
        r.push(next);
        assert!(prunable(&r, &names).contains("web_search"));
        r.last_mut().unwrap().tool_name = "web_search".into();
        assert!(prunable(&r, &names).is_empty());
    }
    #[test]
    fn protected_list_and_unknown_effect() {
        let mut unique = HashSet::new();
        for name in PROTECTED_TOOLS {
            assert!(unique.insert(*name), "duplicate {name}");
            assert!(protected(name));
        }
        for tool in crate::mcp_tool_defs::tools() {
            if tool.name.starts_with("memory_")
                || tool.name.starts_with("tasks_")
                || tool.name.starts_with("working_state_")
                || tool.name.starts_with("mail_")
                || crate::effect_of(tool.name) == crate::ToolEffect::Admin
            {
                assert!(
                    protected(tool.name),
                    "unprotected foundational tool {}",
                    tool.name
                );
            }
        }
        assert!(protected("unclassified_future_tool"));
        assert!(
            prunable(
                &rows(10, 2),
                &["wiki_read", "tasks_list", "unclassified_future_tool"]
            )
            .is_empty()
        );
    }
    #[test]
    fn duplicate_calls_are_one_turn() {
        let r = vec![rows(1, 1)[0].clone(); 20];
        assert!(prunable(&r, &["web_search"]).is_empty());
    }
    fn write_rows(home: &Path, r: &[UsageRecord]) {
        let mut text = String::from("malformed\n{\"legacy\":true}\n");
        for row in r {
            text.push_str(&serde_json::json!({"timestamp":row.timestamp,"agent_id":row.agent_id,"tool_name":row.tool_name,"success":row.success,"session_id":row.session_id,"turn_id":row.turn_id}).to_string());
            text.push('\n');
        }
        std::fs::write(home.join("tool_calls.jsonl"), text).unwrap();
    }
    #[test]
    fn missing_malformed_reset_and_cached_use() {
        let home = tempfile::tempdir().unwrap();
        let agent = home.path().join("alice");
        std::fs::create_dir(&agent).unwrap();
        assert!(read_records(home.path(), &agent, "alice").is_empty());
        write_rows(home.path(), &rows(10, 2));
        assert_eq!(read_records(home.path(), &agent, "alice").len(), 10);
        assert!(read_records(home.path(), &agent, "bob").is_empty());
        assert!(
            candidates_from_files(home.path(), &agent, "alice", &["web_search"])
                .contains("web_search")
        );
        note_success(home.path(), "alice", "web_search");
        assert!(candidates_from_files(home.path(), &agent, "alice", &["web_search"]).is_empty());
        std::fs::write(agent.join("tool_pruning_reset_at"), "2026-11-02T00:00:00Z").unwrap();
        assert!(candidates_from_files(home.path(), &agent, "alice", &["web_search"]).is_empty());
        std::fs::write(agent.join("tool_pruning_reset_at"), "invalid").unwrap();
        assert!(read_records(home.path(), &agent, "alice").is_empty());
    }
    #[test]
    fn bounded_tail_discards_partial_line() {
        let home = tempfile::tempdir().unwrap();
        let agent = home.path().join("alice");
        std::fs::create_dir(&agent).unwrap();
        write_rows(home.path(), &rows(10, 2));
        let tail = std::fs::read(home.path().join("tool_calls.jsonl")).unwrap();
        let mut bytes = vec![b'x'; MAX_LOG_BYTES as usize + 100];
        bytes.push(b'\n');
        bytes.extend(tail);
        std::fs::write(home.path().join("tool_calls.jsonl"), bytes).unwrap();
        assert_eq!(read_records(home.path(), &agent, "alice").len(), 10);
    }
    #[test]
    fn mode_default_and_malformed_values() {
        let home = tempfile::tempdir().unwrap();
        for (value, mode) in [
            ("", Mode::Shadow),
            ("tool_pruning = 'off'", Mode::Off),
            ("tool_pruning = 'shadow'", Mode::Shadow),
            ("tool_pruning = 'on'", Mode::On),
            ("tool_pruning = 'unknown'", Mode::Off),
            ("tool_pruning = 42", Mode::Off),
            ("tool_pruning = []", Mode::Off),
        ] {
            let raw = format!("[capabilities]\n{value}\n");
            std::fs::write(home.path().join("agent.toml"), &raw).unwrap();
            assert_eq!(read_mode(home.path()), mode);
            #[derive(Deserialize)]
            struct Config {
                capabilities: crate::types::CapabilitiesConfig,
            }
            assert_eq!(
                toml::from_str::<Config>(&raw)
                    .unwrap()
                    .capabilities
                    .tool_pruning,
                mode
            );
        }
    }
}

#[cfg(test)]
mod turn_window_tests {
    use super::*;
    #[test]
    fn later_calls_in_used_turn_do_not_count_as_an_unused_turn() {
        let base = chrono::DateTime::parse_from_rfc3339("2026-11-01T00:00:00Z").unwrap();
        let mut rows = vec![UsageRecord {
            timestamp: base,
            agent_id: "a".into(),
            tool_name: "web_search".into(),
            success: true,
            session_id: "s0".into(),
            turn_id: "t0".into(),
        }];
        for i in 0..10 {
            rows.push(UsageRecord {
                timestamp: base + chrono::Duration::seconds(i + 1),
                agent_id: "a".into(),
                tool_name: "memory_search".into(),
                success: true,
                session_id: format!("s{}", i % 2),
                turn_id: format!("t{i}"),
            });
        }
        assert!(prunable(&rows, &["web_search"]).is_empty());
        rows.push(UsageRecord {
            timestamp: base + chrono::Duration::seconds(11),
            agent_id: "a".into(),
            tool_name: "memory_search".into(),
            success: true,
            session_id: "s1".into(),
            turn_id: "t10".into(),
        });
        assert!(prunable(&rows, &["web_search"]).contains("web_search"));
    }
}

#[cfg(test)]
mod cache_evidence_tests {
    use super::*;
    #[test]
    fn unreadable_marker_never_hits_a_cached_observation() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("tool_calls.jsonl"), "").unwrap();
        assert!(candidates_from_files(home.path(), home.path(), "a", &["web_search"]).is_empty());
        std::fs::create_dir(home.path().join("tool_pruning_reset_at")).unwrap();
        assert!(candidates_from_files(home.path(), home.path(), "a", &["web_search"]).is_empty());
    }
}

#[cfg(test)]
mod unidentified_use_tests {
    use super::*;
    #[test]
    fn unidentified_success_resets_but_does_not_establish_turns() {
        let base = chrono::DateTime::parse_from_rfc3339("2026-11-01T00:00:00Z").unwrap();
        let mut rows: Vec<_> = (0..10)
            .map(|i| UsageRecord {
                timestamp: base + chrono::Duration::seconds(i),
                agent_id: "a".into(),
                tool_name: "memory_search".into(),
                success: true,
                session_id: format!("s{}", i % 2),
                turn_id: format!("t{i}"),
            })
            .collect();
        assert!(prunable(&rows, &["web_search"]).contains("web_search"));
        rows.push(UsageRecord {
            timestamp: base + chrono::Duration::seconds(11),
            agent_id: "a".into(),
            tool_name: "web_search".into(),
            success: true,
            session_id: String::new(),
            turn_id: String::new(),
        });
        assert!(prunable(&rows, &["web_search"]).is_empty());
        for row in &mut rows {
            row.session_id.clear();
            row.turn_id.clear();
        }
        assert!(prunable(&rows, &["skill_list"]).is_empty());
    }
}
