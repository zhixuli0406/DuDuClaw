//! Local display scheduling only. No gateway mutation or authority is stored.
use crate::state::InboxItem;
use serde::{Deserialize, Serialize};
use std::{cell::Cell, time::{Duration, Instant}};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snooze {
    until: i64, resurface_at: i64, expires_at: Option<i64>,
    #[serde(default)]
    set_at: i64,
    #[serde(skip)]
    deadline: Option<Instant>,
    #[serde(skip)]
    invalidated: Cell<bool>,
}
impl Snooze {
    pub fn new(item: &InboxItem, now: i64, seconds: i64) -> Option<Self> {
        Self::new_at(item, now, seconds, Instant::now())
    }
    pub(crate) fn new_at(item: &InboxItem, now: i64, seconds: i64, monotonic: Instant) -> Option<Self> {
        let expires_at = match item {
            InboxItem::Approval(a) => Some(a.expires_at?),
            InboxItem::Task(_) => None,
        };
        let (cap, resurface_at) = if let Some(expiry) = expires_at {
            let remaining = expiry.saturating_sub(now);
            if remaining <= 0 { return None; }
            // Integer rounding never snoozes longer than 80% of the TTL.
            (remaining.saturating_mul(4) / 5, expiry.saturating_sub((remaining / 5).min(600)))
        } else { (seconds.max(0), i64::MAX) };
        let until = now.saturating_add(seconds.max(0).min(cap)).min(resurface_at);
        Some(Self { until, resurface_at, expires_at, set_at: now,
            deadline: monotonic.checked_add(Duration::from_secs(until.saturating_sub(now) as u64)),
            invalidated: Cell::new(false) })
    }
    pub fn hidden(&self, item: &InboxItem, now: i64) -> bool {
        self.hidden_at(item, now, Instant::now())
    }
    pub(crate) fn hidden_at(&self, item: &InboxItem, now: i64, monotonic: Instant) -> bool {
        // A rollback permanently clears this snooze; restart loses monotonic
        // continuity and therefore reveals the card even if wall time looks valid.
        if now < self.set_at || self.deadline.is_none_or(|deadline| monotonic >= deadline) {
            self.invalidated.set(true);
        }
        if self.invalidated.get() { return false; }
        // A shortened deadline after a refetch must never leave the row hidden.
        let current_expiry = match item { InboxItem::Approval(a) => a.expires_at, _ => None };
        current_expiry == self.expires_at && now < self.until.min(self.resurface_at)
            && self.expires_at.is_none_or(|e| now < e)
    }
}
pub fn epoch_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().min(i64::MAX as u64) as i64).unwrap_or(0)
}
