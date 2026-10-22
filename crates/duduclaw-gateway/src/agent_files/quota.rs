//! Bounds on the AI review: daily quotas (deployment, per person, per
//! employee), a per-person check rate, and a short cache so checking the
//! same change again does not spend another call.
//!
//! A quota that runs out makes the review "failed", which the merge turns
//! into at least a warning that a non-admin can only send to an admin — so
//! exhausting the quota never becomes a way to skip the review.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::verdict::{Reason, Verdict};

/// The daily limits (0 = no reviews).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Limits {
    pub deployment: u32,
    pub per_user: u32,
    pub per_agent: u32,
}

/// Count one review for `user` on `agent`. `Err` names the limit hit; nothing
/// is counted then.
pub(crate) fn take(home: &Path, user: &str, agent: &str, limits: Limits) -> Result<(), &'static str> {
    let path = home.join("agent_files_review_usage.json");
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let r = duduclaw_core::with_file_lock(&path, || {
        let state: Value = std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null);
        let fresh = state["date"] != today.as_str();
        let n = |v: &Value| if fresh { 0 } else { v.as_u64().unwrap_or(0) };
        let total = n(&state["count"]);
        let by_user = n(&state["users"][user]);
        let by_agent = n(&state["agents"][agent]);
        if total >= u64::from(limits.deployment) {
            return Ok(Err("quota_deployment"));
        }
        if by_user >= u64::from(limits.per_user) {
            return Ok(Err("quota_user"));
        }
        if by_agent >= u64::from(limits.per_agent) {
            return Ok(Err("quota_agent"));
        }
        let mut users = if fresh { serde_json::Map::new() } else { state["users"].as_object().cloned().unwrap_or_default() };
        let mut agents = if fresh { serde_json::Map::new() } else { state["agents"].as_object().cloned().unwrap_or_default() };
        users.insert(user.to_string(), json!(by_user + 1));
        agents.insert(agent.to_string(), json!(by_agent + 1));
        std::fs::write(&path, json!({ "date": today, "count": total + 1, "users": users, "agents": agents }).to_string())?;
        Ok(Ok(()))
    });
    r.unwrap_or(Err("quota_unreadable"))
}

// ── Check rate ──────────────────────────────────────────────────────────────

fn rate_map() -> &'static Mutex<HashMap<(PathBuf, String), VecDeque<Instant>>> {
    static M: OnceLock<Mutex<HashMap<(PathBuf, String), VecDeque<Instant>>>> = OnceLock::new();
    M.get_or_init(Default::default)
}

/// Whether `user` may run one more check now (`per_minute` in a sliding
/// minute). Counts the check when allowed.
pub(crate) fn allow_check(home: &Path, user: &str, per_minute: u32) -> bool {
    let mut m = match rate_map().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let q = m.entry((home.to_path_buf(), user.to_string())).or_default();
    let now = Instant::now();
    while q.front().is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(60)) {
        q.pop_front();
    }
    if q.len() >= per_minute as usize {
        return false;
    }
    q.push_back(now);
    true
}

// ── Result cache ────────────────────────────────────────────────────────────

/// A finished review, reused for the same change.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Cached {
    pub verdict: Verdict,
    pub summary: String,
    pub reasons: Vec<Reason>,
    pub partial: bool,
}

const CACHE_TTL: Duration = Duration::from_secs(15 * 60);
const CACHE_MAX: usize = 500;

type CacheMap = HashMap<String, (Instant, Cached)>;

fn cache() -> &'static Mutex<CacheMap> {
    static C: OnceLock<Mutex<CacheMap>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// Cache key: the home, the employee, the file, the text the reviewer
/// compared against and the proposed text.
pub(crate) fn cache_key(home: &Path, agent: &str, file: &str, old: &str, new: &str) -> String {
    format!("{}|{agent}|{file}|{}|{}", home.display(), super::hash_of(old), super::hash_of(new))
}

pub(crate) fn cached(key: &str) -> Option<Cached> {
    let m = cache().lock().ok()?;
    m.get(key).filter(|(at, _)| at.elapsed() < CACHE_TTL).map(|(_, c)| c.clone())
}

pub(crate) fn store(key: String, c: Cached) {
    let Ok(mut m) = cache().lock() else { return };
    m.retain(|_, (at, _)| at.elapsed() < CACHE_TTL);
    while m.len() >= CACHE_MAX {
        let Some(oldest) = m.iter().min_by_key(|(_, (at, _))| *at).map(|(k, _)| k.clone()) else { break };
        m.remove(&oldest);
    }
    m.insert(key, (Instant::now(), c));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotas_per_deployment_person_and_employee() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        let l = Limits { deployment: 3, per_user: 2, per_agent: 2 };
        assert!(take(h, "u1", "a1", l).is_ok());
        assert!(take(h, "u1", "a2", l).is_ok());
        assert_eq!(take(h, "u1", "a3", l), Err("quota_user"));
        assert!(take(h, "u2", "a1", l).is_ok());
        assert_eq!(take(h, "u3", "a1", l), Err("quota_deployment"));
        let l2 = Limits { deployment: 100, per_user: 100, per_agent: 2 };
        assert_eq!(take(h, "u4", "a1", l2), Err("quota_agent"));
        assert_eq!(take(tempfile::tempdir().unwrap().path(), "u", "a", Limits { deployment: 0, per_user: 9, per_agent: 9 }), Err("quota_deployment"));
    }

    #[test]
    fn check_rate_per_person() {
        let home = tempfile::tempdir().unwrap();
        for _ in 0..3 {
            assert!(allow_check(home.path(), "u1", 3));
        }
        assert!(!allow_check(home.path(), "u1", 3));
        assert!(allow_check(home.path(), "u2", 3), "another person has their own budget");
    }

    #[test]
    fn cache_round_trip() {
        let key = cache_key(Path::new("/h"), "a1", "soul", "old", "new");
        assert!(cached(&key).is_none());
        let c = Cached { verdict: Verdict::Warn, summary: "s".into(), reasons: vec![], partial: false };
        store(key.clone(), c.clone());
        assert_eq!(cached(&key), Some(c));
        assert!(cached(&cache_key(Path::new("/h"), "a1", "soul", "old", "other")).is_none());
    }
}
