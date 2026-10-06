//! Is a Team-as-Agent round possibly still running for a task?
//!
//! A team round runs as a detached task inside the gateway process and never
//! touches the message queue, so queue reconciliation alone cannot see it.
//! Two signals are used, and the answer never claims more than they prove:
//! - **in-process registry** — every team round registers its task id for its
//!   whole lifetime (`RoundGuard`). Authoritative only inside the process
//!   whose goal-loop driver dispatched it ([`registry_is_live`]).
//! - **role member scaffolds on disk** — `agents/.ephemeral/<id>/agent.toml`
//!   with a `[team_member] task_id` exists while a member's turn runs. Visible
//!   from any process, but there are short gaps between stages.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};

static ACTIVE: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
static LIVE: AtomicBool = AtomicBool::new(false);

/// Called by the goal-loop driver: from now on this process's registry is
/// the authority on team rounds it dispatches.
pub fn mark_registry_live() {
    LIVE.store(true, Ordering::SeqCst);
}

pub fn registry_is_live() -> bool {
    #[cfg(test)]
    if let Some(v) = LIVE_OVERRIDE.with(|c| c.get()) {
        return v;
    }
    LIVE.load(Ordering::SeqCst)
}

// Tests run in parallel in one process and any driver tick flips `LIVE`, so a
// test that needs a known answer pins it for its own thread.
#[cfg(test)]
thread_local! {
    static LIVE_OVERRIDE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn pin_registry_live_for_test(v: Option<bool>) {
    LIVE_OVERRIDE.with(|c| c.set(v));
}

/// Registered for the lifetime of one team round; dropping it unregisters.
pub struct RoundGuard(String);

impl RoundGuard {
    pub fn register(task_id: &str) -> Self {
        ACTIVE
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(task_id.to_string());
        Self(task_id.to_string())
    }
}

impl Drop for RoundGuard {
    fn drop(&mut self) {
        ACTIVE
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&self.0);
    }
}

/// E-H2: a goal task the driver is between reading it as a candidate and
/// handing its round off (queue or team). A stop reported while one is
/// registered stays `cancel_pending`.
static DISPATCHING: LazyLock<Mutex<HashMap<String, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Held by the driver for one candidate's whole turn in a tick.
pub struct DispatchGuard(String);

impl DispatchGuard {
    pub fn register(task_id: &str) -> Self {
        *DISPATCHING
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(task_id.to_string())
            .or_insert(0) += 1;
        Self(task_id.to_string())
    }
}

impl Drop for DispatchGuard {
    fn drop(&mut self) {
        let mut map = DISPATCHING.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(n) = map.get_mut(&self.0) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                map.remove(&self.0);
            }
        }
    }
}

pub fn dispatching(task_id: &str) -> bool {
    DISPATCHING
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .contains_key(task_id)
}

pub fn registered(task_id: &str) -> bool {
    ACTIVE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .contains(task_id)
}

/// Task ids that have a live role member scaffold under `home`.
pub fn role_member_tasks(home: &Path) -> HashSet<String> {
    let Ok(entries) = std::fs::read_dir(home.join("agents").join(".ephemeral")) else {
        return HashSet::new();
    };
    entries
        .flatten()
        .filter_map(|e| crate::ephemeral::read_role_member(&e.path()))
        .map(|r| r.task_id)
        .collect()
}
