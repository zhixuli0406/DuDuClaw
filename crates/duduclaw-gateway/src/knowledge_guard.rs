//! Same-origin write-burst detector for the knowledge-ingest path (D2).
//!
//! ## Why this exists (PoisonedRAG, arXiv:2402.07867)
//!
//! PoisonedRAG shows that a handful of crafted documents pointing at one
//! subject can dominate a retrieval-augmented answer. DuDuClaw's conversation
//! distillation is exactly that write surface: an attacker who can drive one
//! channel could hammer the same `(origin, subject)` with contradicting facts
//! to overwrite curated knowledge (the "One Shot Dominance" / k-doc poisoning
//! pattern).
//!
//! This module is the burst bound: a per-`(agent_id, origin, subject)`
//! sliding-window counter whose state is durable and **shared across
//! processes**. When a single origin writes `>= max_per_subject` facts about
//! the same subject inside `window_secs`, the whole batch is *quarantined*
//! (held inert, excluded from retrieval) and routed to a human via the
//! ApprovalBroker instead of silently landing in memory.
//!
//! It deliberately reuses the [`crate::dispatch_guard`] shape: state in
//! `<home>/knowledge_guard.json`, every read-modify-write wrapped in
//! [`duduclaw_core::with_file_lock`] (project convention #3), atomic
//! temp+rename replace, corrupt state treated as empty (fresh window).
//!
//! ## Failure posture
//!
//! This is a *detector*, not an authorization gate — but its failure mode is
//! the opposite of the dispatch breaker. A quarantine is a conservative,
//! recoverable action (the fact is held for review, not dropped), so when the
//! lock or state file is unusable the safe default is **Quarantine** for any
//! batch that reached the counting path — never silently `Allow` a burst
//! through. Callers that see `Quarantine` MUST hold the batch and request
//! approval.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Tuning for the knowledge write-burst detector. Overridable via
/// `config.toml [knowledge_guard]` (see [`KnowledgeGuardConfig::from_home`]).
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct KnowledgeGuardConfig {
    /// Master switch. When `false` the detector always returns `Allow`
    /// (zero overhead, no state file touched).
    pub enabled: bool,
    /// Rolling window length in seconds.
    pub window_secs: u64,
    /// Max facts one origin may write about a single subject within the window
    /// before the batch is quarantined.
    pub max_per_subject: u32,
}

impl Default for KnowledgeGuardConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            window_secs: 3600,
            max_per_subject: 5,
        }
    }
}

impl KnowledgeGuardConfig {
    /// Load `[knowledge_guard]` from `<home>/config.toml`. Parsed in isolation
    /// from a generic `toml::Table` so unrelated / malformed config elsewhere
    /// can never make this fail — absent / malformed section ⇒ built-in
    /// defaults (fail-safe, detector stays ON by default).
    pub fn from_home(home_dir: &Path) -> Self {
        let path = home_dir.join("config.toml");
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        let Ok(table) = content.parse::<toml::Table>() else {
            return Self::default();
        };
        match table.get("knowledge_guard") {
            Some(section) => section
                .clone()
                .try_into::<KnowledgeGuardConfig>()
                .unwrap_or_default(),
            None => Self::default(),
        }
    }
}

/// The detector's decision for one `(origin, subject)` write group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KnowledgeGuardDecision {
    /// Under the burst threshold; the group may be stored normally.
    Allow,
    /// Burst threshold reached — the group must be quarantined for review.
    Quarantine {
        /// Human-readable reason (for the approval summary / logs).
        reason: String,
        /// Count of same-`(origin, subject)` writes now seen in the window.
        count_in_window: u32,
    },
}

impl KnowledgeGuardDecision {
    pub fn is_quarantine(&self) -> bool {
        matches!(self, KnowledgeGuardDecision::Quarantine { .. })
    }
}

/// One `(agent, origin, subject)` bucket. Timestamps are epoch milliseconds.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct Bucket {
    /// Write timestamps within (or recently within) the window.
    events: Vec<i64>,
}

type State = HashMap<String, Bucket>;

fn now_epoch_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn load_state(path: &Path) -> State {
    // Missing or corrupt state ⇒ empty (fresh window). Never propagate an
    // error out of the loader.
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => State::new(),
    }
}

fn save_state(path: &Path, state: &State) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(state)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    // Atomic replace (temp + rename) so a crash mid-write cannot leave a
    // truncated JSON file the next reader would discard.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, path)
}

/// Drop idle buckets so the state file stays bounded: a bucket with no event
/// newer than one full window has nothing left to enforce.
fn prune(state: &mut State, now_ms: i64, window_ms: i64) {
    state.retain(|_, b| b.events.iter().any(|&t| now_ms - t < window_ms));
}

/// Compose the bucket key. `origin` and `subject` are matched by exact
/// equality (never substring — a routing/security decision, project
/// convention #2); the separator is a control char that cannot appear in a
/// normal origin/subject so keys never collide.
fn bucket_key(agent_id: &str, origin: &str, subject: &str) -> String {
    format!("{agent_id}\u{1f}{origin}\u{1f}{subject}")
}

/// Record `n` new writes on the `(agent_id, origin, subject)` group and decide
/// whether the group must be quarantined. Cross-process safe (advisory-locked
/// read-modify-write on `<home>/knowledge_guard.json`).
///
/// `n` is the number of facts in *this* batch for the group; recording them
/// together means a single batch of `>= max_per_subject` facts trips
/// immediately (the k-doc / one-shot case) while a slow drip trips once the
/// rolling window accumulates enough.
pub fn check_and_record(
    home_dir: &Path,
    cfg: &KnowledgeGuardConfig,
    agent_id: &str,
    origin: &str,
    subject: &str,
    n: u32,
) -> KnowledgeGuardDecision {
    if !cfg.enabled || n == 0 {
        return KnowledgeGuardDecision::Allow;
    }
    let path = home_dir.join("knowledge_guard.json");
    let key = bucket_key(agent_id, origin, subject);
    let now_ms = now_epoch_ms();
    let window_ms = cfg.window_secs.saturating_mul(1000) as i64;

    let result = duduclaw_core::with_file_lock(&path, || {
        let mut state = load_state(&path);
        prune(&mut state, now_ms, window_ms);

        let bucket = state.entry(key.clone()).or_default();
        bucket.events.retain(|&t| now_ms - t < window_ms);
        for _ in 0..n {
            bucket.events.push(now_ms);
        }
        let count = bucket.events.len() as u32;
        let _ = save_state(&path, &state);

        if count >= cfg.max_per_subject {
            Ok(KnowledgeGuardDecision::Quarantine {
                reason: format!(
                    "same-origin write burst: {count} facts about subject in {}s window \
                     (limit {}) from origin",
                    cfg.window_secs, cfg.max_per_subject
                ),
                count_in_window: count,
            })
        } else {
            Ok(KnowledgeGuardDecision::Allow)
        }
    });

    // Lock-acquisition failure ⇒ fail-CLOSED for a detector: quarantine the
    // batch rather than let a possible burst through unseen. A quarantine is
    // recoverable (human review); a missed poison write is not.
    result.unwrap_or_else(|_| KnowledgeGuardDecision::Quarantine {
        reason: "knowledge_guard state unavailable — quarantining batch (fail-closed)".to_string(),
        count_in_window: n,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(max: u32, window: u64) -> KnowledgeGuardConfig {
        KnowledgeGuardConfig {
            enabled: true,
            window_secs: window,
            max_per_subject: max,
        }
    }

    #[test]
    fn allows_below_threshold_then_quarantines() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(5, 3600);
        // 4 single writes about the same subject — all allowed.
        for _ in 0..4 {
            assert_eq!(
                check_and_record(dir.path(), &c, "agnes", "channel", "user:alice", 1),
                KnowledgeGuardDecision::Allow
            );
        }
        // 5th reaches the limit → quarantine.
        assert!(check_and_record(dir.path(), &c, "agnes", "channel", "user:alice", 1)
            .is_quarantine());
    }

    #[test]
    fn one_batch_at_or_above_limit_trips_immediately() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(5, 3600);
        // A single batch of 5 same-subject facts is the k-doc poisoning case.
        let d = check_and_record(dir.path(), &c, "agnes", "channel", "user:alice", 5);
        assert!(d.is_quarantine());
        if let KnowledgeGuardDecision::Quarantine { count_in_window, .. } = d {
            assert_eq!(count_in_window, 5);
        }
    }

    #[test]
    fn distinct_subjects_are_isolated() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(3, 3600);
        // 3 about bob → trips; alice untouched.
        for _ in 0..2 {
            assert_eq!(
                check_and_record(dir.path(), &c, "agnes", "channel", "user:bob", 1),
                KnowledgeGuardDecision::Allow
            );
        }
        assert!(check_and_record(dir.path(), &c, "agnes", "channel", "user:bob", 1)
            .is_quarantine());
        assert_eq!(
            check_and_record(dir.path(), &c, "agnes", "channel", "user:alice", 1),
            KnowledgeGuardDecision::Allow
        );
    }

    #[test]
    fn distinct_origins_are_isolated() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(2, 3600);
        assert!(check_and_record(dir.path(), &c, "agnes", "channelA", "s", 2).is_quarantine());
        // Same subject, different origin → own bucket, still under limit.
        assert_eq!(
            check_and_record(dir.path(), &c, "agnes", "channelB", "s", 1),
            KnowledgeGuardDecision::Allow
        );
    }

    #[test]
    fn disabled_always_allows() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = cfg(1, 3600);
        c.enabled = false;
        assert_eq!(
            check_and_record(dir.path(), &c, "agnes", "channel", "s", 100),
            KnowledgeGuardDecision::Allow
        );
        // No state file should have been written.
        assert!(!dir.path().join("knowledge_guard.json").exists());
    }

    #[test]
    fn corrupt_state_is_treated_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("knowledge_guard.json"), b"{not json").unwrap();
        let c = cfg(2, 3600);
        // Corrupt file ⇒ fresh window; a single write is under the limit.
        assert_eq!(
            check_and_record(dir.path(), &c, "agnes", "channel", "s", 1),
            KnowledgeGuardDecision::Allow
        );
    }

    #[test]
    fn old_events_fall_out_of_window() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("knowledge_guard.json");
        // Seed a bucket whose events are far in the past (epoch 0).
        let mut state = State::new();
        state.insert(
            bucket_key("agnes", "channel", "s"),
            Bucket { events: vec![0, 0, 0, 0] },
        );
        std::fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();

        // Fresh single write: the 4 stale events are pruned, so 1 < limit 5.
        let c = cfg(5, 3600);
        assert_eq!(
            check_and_record(dir.path(), &c, "agnes", "channel", "s", 1),
            KnowledgeGuardDecision::Allow
        );
    }

    #[test]
    fn config_from_home_parses_partial_section() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[knowledge_guard]\nmax_per_subject = 3\n",
        )
        .unwrap();
        let c = KnowledgeGuardConfig::from_home(dir.path());
        assert_eq!(c.max_per_subject, 3);
        // Unspecified fields keep defaults.
        assert!(c.enabled);
        assert_eq!(c.window_secs, 3600);

        // Absent file ⇒ all defaults.
        let empty = tempfile::tempdir().unwrap();
        let d = KnowledgeGuardConfig::from_home(empty.path());
        assert_eq!(d.max_per_subject, 5);
        assert!(d.enabled);
    }
}
