//! Local display scheduling only. No gateway mutation or authority is stored.
use crate::state::InboxItem;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snooze { until: i64, resurface_at: i64, expires_at: Option<i64> }
impl Snooze {
    pub fn new(item: &InboxItem, now: i64, seconds: i64) -> Option<Self> {
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
        Some(Self { until: now.saturating_add(seconds.max(0).min(cap)).min(resurface_at), resurface_at, expires_at })
    }
    pub fn hidden(&self, item: &InboxItem, now: i64) -> bool {
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
