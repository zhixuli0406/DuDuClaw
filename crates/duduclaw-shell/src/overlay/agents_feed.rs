//! Real AI-team roster for the shell — `agents.list` cached with the same
//! 30 s stale/refresh shape as `task_progress_feed`. 2026-09-05 (QEMU
//! feature walkthrough): the Launcher's 交辦 card, the dock's agent tiles
//! and the Control Centre footer were all `fake_data` (a "財務助理" nobody
//! had, "2 位在值" on an empty machine); every one of them now reads this.
use std::time::Instant;

use crate::gateway_client::{pick_default_agent, AgentRef};

pub use super::task_progress_feed::FeedStatus;

pub const REFRESH_STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq)]
pub struct AgentsFeed {
    status: FeedStatus,
    busy: bool,
    last_refreshed_at: Option<Instant>,
    rows: Vec<AgentRef>,
}

impl Default for AgentsFeed {
    fn default() -> Self {
        Self { status: FeedStatus::Idle, busy: false, last_refreshed_at: None, rows: Vec::new() }
    }
}

impl AgentsFeed {
    pub fn count(&self) -> usize {
        self.rows.len()
    }

    pub fn rows(&self) -> &[AgentRef] {
        &self.rows
    }

    /// The agent 交辦 will target — the org's `role: "main"` agent, else the
    /// first one. `None` until the first successful fetch or on an empty
    /// roster; callers render the honest empty state for that.
    pub fn default_agent(&self) -> Option<&AgentRef> {
        pick_default_agent(&self.rows)
    }

    #[allow(dead_code)]
    pub fn status(&self) -> FeedStatus {
        self.status
    }

    pub(crate) fn is_stale(&self) -> bool {
        match self.last_refreshed_at {
            None => true,
            Some(t) => t.elapsed() >= REFRESH_STALE_AFTER,
        }
    }

    pub(crate) fn begin_refresh(&mut self) -> bool {
        if self.busy {
            return false;
        }
        self.busy = true;
        if self.status == FeedStatus::Idle {
            self.status = FeedStatus::Loading;
        }
        true
    }

    pub(crate) fn apply_list_ok(&mut self, items: Vec<AgentRef>) -> bool {
        let changed = items != self.rows || self.status != FeedStatus::Ready;
        self.rows = items;
        self.status = FeedStatus::Ready;
        self.busy = false;
        self.last_refreshed_at = Some(Instant::now());
        changed
    }

    pub(crate) fn apply_list_err(&mut self) {
        self.status = FeedStatus::Offline;
        self.busy = false;
        self.last_refreshed_at = Some(Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str, role: &str, display: &str) -> AgentRef {
        AgentRef { id: id.to_string(), role: role.to_string(), display_name: display.to_string() }
    }

    #[test]
    fn empty_roster_has_no_default_agent() {
        let feed = AgentsFeed::default();
        assert!(feed.is_stale());
        assert_eq!(feed.default_agent(), None);
        assert_eq!(feed.count(), 0);
    }

    #[test]
    fn default_agent_prefers_main_role_and_label_falls_back_to_id() {
        let mut feed = AgentsFeed::default();
        assert!(feed.begin_refresh());
        assert!(feed.apply_list_ok(vec![agent("ops", "specialist", ""), agent("boss", "main", "總管助理")]));
        let a = feed.default_agent().expect("main agent");
        assert_eq!(a.id, "boss");
        assert_eq!(a.label(), "總管助理");
        assert_eq!(feed.rows()[0].label(), "ops");
    }
}
