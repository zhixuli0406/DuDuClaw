//! Cross-process sliding-window circuit breaker for feedback dispatch paths.
//!
//! ## Why this exists (paper arXiv:2607.01641)
//!
//! The core finding of the 2607.01641 runaway study is that a framework's
//! `max_iterations` fails because the bound is placed *outside* the real
//! feedback path. Every path that can *re-generate work* — bus delegation
//! (`spawn_agent` / ephemeral synthesis), cron dispatch, sub-agent re-entry —
//! must carry its own bound at the point where the feedback is produced.
//!
//! This module is that bound: a per-`(path_kind, agent_id)` sliding-window rate
//! limiter whose state is durable and **shared across processes** (the Rust
//! gateway and the CLI MCP server both feed the same bus). State lives in
//! `<home>/dispatch_guard.json` and every read-modify-write is wrapped in
//! [`crate::with_file_lock`] (project convention #3: cross-process mutation of a
//! shared file must hold the advisory lock) so two writers cannot corrupt the
//! counters or race a trip decision apart.
//!
//! ## Semantics
//!
//! - Each `(path_kind, agent_id)` gets its own bucket.
//! - Allow up to [`DispatchGuardConfig::limit_for_kind`] dispatches per rolling
//!   `window_secs` — `max_in_window` for every path kind except
//!   [`PATH_KIND_ROLE_TEAM`], which has its own (higher) budget because the
//!   gateway's team composer spawns several role members per goal round and must
//!   not consume the same budget as user-triggered `spawn_ephemeral` calls.
//! - The `(limit)`-th dispatch inside the window **trips** the breaker:
//!   the bucket enters a `cooldown_secs` cooldown during which every call
//!   returns [`DispatchGuardDecision::Trip`]. After the cooldown elapses the
//!   bucket resets and dispatch resumes.
//! - Old events and idle buckets are pruned on every call so the state file
//!   cannot grow without bound.
//!
//! ## Failure posture
//!
//! This is a *rate limiter*, not an authorization gate. If its own state file
//! cannot be read or written (transient FS error, corrupt JSON) the call returns
//! [`DispatchGuardDecision::Allow`] — a broken counter file must never wedge the
//! whole gateway. Corrupt state is treated as empty (fresh window). Callers that
//! receive `Trip` MUST surface a clear error / log (fail-visible), never drop it
//! silently.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Path kind for **team role-member** spawns (Team-as-Agent P1/WP-2).
///
/// A separate label — not a separate state file — so the existing
/// `(path_kind, agent_id)` key already isolates the bucket: the composer's
/// per-round planner/executor/verifier spawns can never trip the breaker that
/// guards an *agent's own* `spawn_ephemeral` calls, and vice versa.
///
/// Why it needs its own budget as well as its own bucket: the minimum team is
/// 3 spawns per goal round, `iteration_cap` defaults to 5, and
/// `[goal_loop] max_concurrent` defaults to 3 — 9 spawns inside one 60 s window
/// for a single in-flight task, ~30 with three tasks in flight. That is over
/// the default `max_in_window = 20` on arithmetic alone, which would read to an
/// operator as "the platform broke" rather than "a limit was reached"
/// (design `DESIGN-team-as-agent-2026-09.md` §3.8, fix ②).
pub const PATH_KIND_ROLE_TEAM: &str = "role_team";

/// Default for [`DispatchGuardConfig::role_team_max_in_window`].
///
/// 60 = 3 roles × 5 rounds × 3 concurrent tasks — the arithmetic worst case of
/// the default goal-loop configuration, with nothing added for headroom. It is
/// still a *bound*: a composer that exceeds it is looping, not merely busy.
pub const DEFAULT_ROLE_TEAM_MAX_IN_WINDOW: u32 = 60;

/// Tuning for the dispatch circuit breaker. Overridable via
/// `config.toml [dispatch_guard]` (see [`DispatchGuardConfig::from_home`]).
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct DispatchGuardConfig {
    /// Rolling window length in seconds.
    pub window_secs: u64,
    /// Maximum dispatches permitted within one window before the breaker trips.
    /// Applies to every path kind except [`PATH_KIND_ROLE_TEAM`].
    pub max_in_window: u32,
    /// Maximum [`PATH_KIND_ROLE_TEAM`] dispatches permitted within one window.
    /// Separate key because team composition spawns several members per goal
    /// round; see [`DEFAULT_ROLE_TEAM_MAX_IN_WINDOW`]. `window_secs` and
    /// `cooldown_secs` are deliberately **shared** — one rolling window is
    /// easier to reason about than two, and the budget is the axis that
    /// actually differs.
    pub role_team_max_in_window: u32,
    /// How long the breaker stays tripped (denies) after firing.
    pub cooldown_secs: u64,
    /// Cascade hop-depth ceiling (see [`crate::ENV_HOP_DEPTH`]). Kept here so all
    /// runaway-guard tuning lives in one `[dispatch_guard]` section; consumed by
    /// the CLI MCP delegation gate, not by [`check_and_record`].
    pub max_hop_depth: u8,
}

impl Default for DispatchGuardConfig {
    fn default() -> Self {
        Self {
            window_secs: 60,
            max_in_window: 20,
            role_team_max_in_window: DEFAULT_ROLE_TEAM_MAX_IN_WINDOW,
            cooldown_secs: 60,
            max_hop_depth: crate::DEFAULT_MAX_HOP_DEPTH,
        }
    }
}

impl DispatchGuardConfig {
    /// Load `[dispatch_guard]` from `<home>/config.toml`. The section is parsed
    /// in isolation from a generic `toml::Table`, so unrelated / malformed config
    /// elsewhere can never make this fail — absent / malformed section ⇒
    /// built-in defaults.
    pub fn from_home(home_dir: &Path) -> Self {
        let path = home_dir.join("config.toml");
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        let Ok(table) = content.parse::<toml::Table>() else {
            return Self::default();
        };
        match table.get("dispatch_guard") {
            Some(section) => section
                .clone()
                .try_into::<DispatchGuardConfig>()
                .unwrap_or_default(),
            None => Self::default(),
        }
    }

    /// The in-window budget that applies to `path_kind`.
    ///
    /// Exact match on [`PATH_KIND_ROLE_TEAM`] (project convention #2: never an
    /// unanchored `contains` / `starts_with` for a routing decision) — a path
    /// kind merely *containing* `role_team` does not get the larger budget.
    pub fn limit_for_kind(&self, path_kind: &str) -> u32 {
        if path_kind == PATH_KIND_ROLE_TEAM {
            self.role_team_max_in_window
        } else {
            self.max_in_window
        }
    }
}

/// The breaker's decision for one dispatch attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchGuardDecision {
    /// Dispatch may proceed; the attempt was recorded.
    Allow,
    /// Breaker is open — dispatch is denied.
    Trip {
        /// Human-readable reason (for logs / caller error messages).
        reason: String,
        /// Seconds until the breaker is expected to close again.
        retry_after_secs: u64,
    },
}

impl DispatchGuardDecision {
    /// Convenience: `true` when the breaker denied the dispatch.
    pub fn is_tripped(&self) -> bool {
        matches!(self, DispatchGuardDecision::Trip { .. })
    }
}

/// One `(path_kind, agent_id)` bucket. Timestamps are epoch milliseconds.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct Bucket {
    /// Dispatch timestamps within (or recently within) the window.
    events: Vec<i64>,
    /// When set and still in the future, the breaker is tripped until this ms.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    tripped_until: Option<i64>,
}

type State = HashMap<String, Bucket>;

fn now_epoch_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn load_state(path: &Path) -> State {
    // Missing or corrupt state ⇒ empty (fresh). Never propagate an error: a
    // rate limiter must not break the caller because its own file is unreadable.
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

/// Drop idle buckets so the state file stays bounded. A bucket is removable when
/// it is not currently tripped and holds no events newer than one full window
/// (nothing left to enforce). The just-touched key is always retained by the
/// caller because it is re-inserted after pruning.
fn prune(state: &mut State, now_ms: i64, window_ms: i64) {
    state.retain(|_, b| {
        if let Some(until) = b.tripped_until {
            if until > now_ms {
                return true; // still cooling down
            }
        }
        b.events.iter().any(|&t| now_ms - t < window_ms)
    });
}

/// Record a dispatch attempt on the `(path_kind, agent_id)` feedback path and
/// decide whether it may proceed. Cross-process safe (advisory-locked
/// read-modify-write on `<home>/dispatch_guard.json`).
///
/// `path_kind` is a short, stable label for the feedback path
/// (e.g. `"bus"`, `"spawn"`, `"cron"`, [`PATH_KIND_ROLE_TEAM`]). `agent_id`
/// scopes the window so one runaway agent cannot starve the breaker budget of
/// another; the budget itself comes from
/// [`DispatchGuardConfig::limit_for_kind`], so a team's role-member spawns
/// neither share a bucket nor share a budget with the same agent's
/// user-triggered ephemeral spawns.
pub fn check_and_record(
    home_dir: &Path,
    path_kind: &str,
    agent_id: &str,
    cfg: &DispatchGuardConfig,
) -> DispatchGuardDecision {
    let path = home_dir.join("dispatch_guard.json");
    let key = format!("{path_kind}|{agent_id}");
    let max_in_window = cfg.limit_for_kind(path_kind);
    let now_ms = now_epoch_ms();
    let window_ms = cfg.window_secs.saturating_mul(1000) as i64;
    let cooldown_ms = cfg.cooldown_secs.saturating_mul(1000) as i64;

    // Entire read-modify-write under the cross-process advisory lock so two
    // processes cannot both observe `len == max-1` and both be allowed.
    let result = crate::with_file_lock(&path, || {
        let mut state = load_state(&path);
        prune(&mut state, now_ms, window_ms);

        let bucket = state.entry(key.clone()).or_default();

        // ── Cooldown: still tripped? ──
        if let Some(until) = bucket.tripped_until {
            if now_ms < until {
                let retry_after_secs = ((until - now_ms) / 1000).max(0) as u64;
                return Ok(DispatchGuardDecision::Trip {
                    reason: format!(
                        "dispatch circuit breaker OPEN for {key}: >{max_in_window} dispatches in \
                         {}s window; cooling down",
                        cfg.window_secs
                    ),
                    retry_after_secs,
                });
            }
            // Cooldown elapsed — reset the bucket to a fresh window.
            bucket.tripped_until = None;
            bucket.events.clear();
        }

        // ── Drop events that fell out of the rolling window ──
        bucket.events.retain(|&t| now_ms - t < window_ms);

        // ── Over budget? Trip and open the cooldown. ──
        if bucket.events.len() as u32 >= max_in_window {
            bucket.tripped_until = Some(now_ms + cooldown_ms);
            let decision = DispatchGuardDecision::Trip {
                reason: format!(
                    "dispatch circuit breaker TRIPPED for {key}: {} dispatches in {}s window \
                     (limit {max_in_window})",
                    bucket.events.len(),
                    cfg.window_secs,
                ),
                retry_after_secs: cfg.cooldown_secs,
            };
            let _ = save_state(&path, &state);
            return Ok(decision);
        }

        // ── Under budget: record and allow. ──
        bucket.events.push(now_ms);
        let _ = save_state(&path, &state);
        Ok(DispatchGuardDecision::Allow)
    });

    // Lock acquisition failure ⇒ fail-open (availability over strictness for a
    // rate limiter; a broken lock file must not wedge every dispatch).
    result.unwrap_or(DispatchGuardDecision::Allow)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(max: u32, window: u64, cooldown: u64) -> DispatchGuardConfig {
        DispatchGuardConfig {
            window_secs: window,
            max_in_window: max,
            cooldown_secs: cooldown,
            ..DispatchGuardConfig::default()
        }
    }

    #[test]
    fn allows_under_budget_then_trips() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(3, 60, 60);
        // First 3 allowed, 4th trips.
        for _ in 0..3 {
            assert_eq!(
                check_and_record(dir.path(), "bus", "alice", &c),
                DispatchGuardDecision::Allow
            );
        }
        let d = check_and_record(dir.path(), "bus", "alice", &c);
        assert!(d.is_tripped(), "the (max+1)-th dispatch must trip");
    }

    #[test]
    fn cooldown_denies_then_resets_after_elapse() {
        let dir = tempfile::tempdir().unwrap();
        // Zero-length cooldown so it elapses immediately for the test.
        let c = cfg(2, 60, 0);
        assert_eq!(
            check_and_record(dir.path(), "bus", "a", &c),
            DispatchGuardDecision::Allow
        );
        assert_eq!(
            check_and_record(dir.path(), "bus", "a", &c),
            DispatchGuardDecision::Allow
        );
        // 3rd trips (opens cooldown).
        assert!(check_and_record(dir.path(), "bus", "a", &c).is_tripped());
        // cooldown_secs == 0 ⇒ next call sees cooldown elapsed, resets, allows.
        assert_eq!(
            check_and_record(dir.path(), "bus", "a", &c),
            DispatchGuardDecision::Allow
        );
    }

    #[test]
    fn buckets_are_isolated_per_key() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(1, 60, 60);
        // One allowed each for two distinct (path_kind, agent) keys.
        assert_eq!(
            check_and_record(dir.path(), "bus", "alice", &c),
            DispatchGuardDecision::Allow
        );
        assert_eq!(
            check_and_record(dir.path(), "cron", "alice", &c),
            DispatchGuardDecision::Allow
        );
        assert_eq!(
            check_and_record(dir.path(), "bus", "bob", &c),
            DispatchGuardDecision::Allow
        );
        // Second on the same key trips; the others are unaffected.
        assert!(check_and_record(dir.path(), "bus", "alice", &c).is_tripped());
        assert!(check_and_record(dir.path(), "cron", "alice", &c).is_tripped());
    }

    #[test]
    fn corrupt_state_file_is_treated_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("dispatch_guard.json"), b"{not json").unwrap();
        let c = cfg(2, 60, 60);
        // Corrupt file ⇒ fresh window, dispatch allowed.
        assert_eq!(
            check_and_record(dir.path(), "bus", "a", &c),
            DispatchGuardDecision::Allow
        );
    }

    #[test]
    fn idle_bucket_is_pruned() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dispatch_guard.json");
        // Seed a bucket whose only event is far outside the window and is not
        // tripped — the next call must prune it, keeping the file bounded.
        let mut state = State::new();
        state.insert(
            "bus|stale".to_string(),
            Bucket {
                events: vec![0],
                tripped_until: None,
            },
        );
        std::fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();

        let c = cfg(5, 60, 60);
        check_and_record(dir.path(), "bus", "fresh", &c);

        let after: State = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(
            !after.contains_key("bus|stale"),
            "stale bucket must be pruned"
        );
        assert!(after.contains_key("bus|fresh"));
    }

    #[test]
    fn config_from_home_parses_partial_section() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[dispatch_guard]\nmax_in_window = 5\n",
        )
        .unwrap();
        let c = DispatchGuardConfig::from_home(dir.path());
        assert_eq!(c.max_in_window, 5);
        // Unspecified fields keep defaults.
        assert_eq!(c.window_secs, 60);
        assert_eq!(c.cooldown_secs, 60);

        // Absent file ⇒ all defaults.
        let empty = tempfile::tempdir().unwrap();
        let d = DispatchGuardConfig::from_home(empty.path());
        assert_eq!(d.max_in_window, 20);
    }

    // ── role_team: its own bucket AND its own budget (WP-2) ──────────────

    #[test]
    fn role_team_default_budget_is_higher_than_the_shared_one() {
        let d = DispatchGuardConfig::default();
        assert_eq!(d.role_team_max_in_window, DEFAULT_ROLE_TEAM_MAX_IN_WINDOW);
        assert_eq!(d.role_team_max_in_window, 60);
        // 3 roles × 5 rounds × 3 concurrent tasks must fit, or a default
        // install trips the breaker on arithmetic alone (design §3.8).
        assert!(d.role_team_max_in_window >= 3 * 5 * 3);
        assert!(d.role_team_max_in_window > d.max_in_window);
    }

    #[test]
    fn limit_for_kind_matches_role_team_exactly() {
        let d = DispatchGuardConfig::default();
        assert_eq!(d.limit_for_kind(PATH_KIND_ROLE_TEAM), 60);
        assert_eq!(d.limit_for_kind("ephemeral"), 20);
        assert_eq!(d.limit_for_kind("bus"), 20);
        // Convention #2: no unanchored substring match.
        assert_eq!(d.limit_for_kind("role_teamX"), 20);
        assert_eq!(d.limit_for_kind("xrole_team"), 20);
    }

    #[test]
    fn role_team_config_override_is_read_from_home() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[dispatch_guard]\nrole_team_max_in_window = 7\n",
        )
        .unwrap();
        let c = DispatchGuardConfig::from_home(dir.path());
        assert_eq!(c.role_team_max_in_window, 7);
        // Untouched keys keep their defaults.
        assert_eq!(c.max_in_window, 20);
        assert_eq!(c.window_secs, 60);
    }

    #[test]
    fn role_team_spawns_do_not_consume_the_ephemeral_budget() {
        let dir = tempfile::tempdir().unwrap();
        // Shared budget 2, role_team budget 4 — same agent, same window.
        let c = DispatchGuardConfig {
            window_secs: 60,
            max_in_window: 2,
            role_team_max_in_window: 4,
            cooldown_secs: 60,
            ..DispatchGuardConfig::default()
        };
        // Four role-member spawns all fit under the role_team budget.
        for _ in 0..4 {
            assert_eq!(
                check_and_record(dir.path(), PATH_KIND_ROLE_TEAM, "alice", &c),
                DispatchGuardDecision::Allow
            );
        }
        // …and the agent's own ephemeral budget is still completely untouched.
        for _ in 0..2 {
            assert_eq!(
                check_and_record(dir.path(), "ephemeral", "alice", &c),
                DispatchGuardDecision::Allow
            );
        }
        // Each bucket then trips on its own limit, independently.
        assert!(check_and_record(dir.path(), "ephemeral", "alice", &c).is_tripped());
        assert!(check_and_record(dir.path(), PATH_KIND_ROLE_TEAM, "alice", &c).is_tripped());
    }

    #[test]
    fn role_team_budget_is_enforced_not_merely_larger() {
        let dir = tempfile::tempdir().unwrap();
        let c = DispatchGuardConfig {
            window_secs: 60,
            max_in_window: 20,
            role_team_max_in_window: 3,
            cooldown_secs: 60,
            ..DispatchGuardConfig::default()
        };
        for _ in 0..3 {
            assert_eq!(
                check_and_record(dir.path(), PATH_KIND_ROLE_TEAM, "bob", &c),
                DispatchGuardDecision::Allow
            );
        }
        let d = check_and_record(dir.path(), PATH_KIND_ROLE_TEAM, "bob", &c);
        assert!(d.is_tripped(), "role_team must still be bounded");
        if let DispatchGuardDecision::Trip { reason, .. } = d {
            assert!(
                reason.contains("limit 3"),
                "reason must cite the role_team limit: {reason}"
            );
        }
    }
}
