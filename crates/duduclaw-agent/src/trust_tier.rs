//! Skill / MCP-server trust tiering.
//!
//! Most community MCP servers and skills are single-maintainer repos of wildly
//! varying upkeep — some official and fresh, many abandoned. This classifier
//! turns the GitHub metadata the indexer already fetches (last-push age +
//! owner type + star count) into a three-level `TrustTier` so the UI can steer
//! users away from orphaned servers instead of surfacing them blindly.
//!
//! The rule is deterministic and freshness-first:
//! - **Orphan** — last push older than [`ORPHAN_MONTHS`], regardless of owner.
//!   The dominant risk signal: unmaintained code.
//! - **Official** — an Organization-owned repo pushed within [`FRESH_MONTHS`].
//!   Org ownership + recent activity is the closest proxy for "maintained by a
//!   team, not a weekend project".
//! - **Active** — everything else that is not stale: a personal repo under
//!   active maintenance, or one whose push date could not be parsed (unknown is
//!   treated as neutral, never as a hard demotion).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Push age (months) beyond which a repo is considered abandoned.
pub const ORPHAN_MONTHS: f64 = 12.0;
/// Push age (months) within which an org repo qualifies as `Official`.
pub const FRESH_MONTHS: f64 = 6.0;

/// Three-level maintenance-trust classification for a skill/MCP repo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TrustTier {
    /// Organization-owned and recently active.
    Official,
    /// Actively maintained (or freshness unknown) — the neutral default.
    #[default]
    Active,
    /// Stale — last push older than [`ORPHAN_MONTHS`]. Surface a warning.
    Orphan,
}

impl TrustTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            TrustTier::Official => "official",
            TrustTier::Active => "active",
            TrustTier::Orphan => "orphan",
        }
    }
}

/// Whole-months elapsed between `pushed` and `now` (approx: 30.44-day months).
fn months_since(pushed: DateTime<Utc>, now: DateTime<Utc>) -> f64 {
    let days = (now - pushed).num_seconds() as f64 / 86_400.0;
    days / 30.44
}

/// Classify a repo from its GitHub metadata.
///
/// `pushed_at` is the raw `pushed_at` RFC3339 string (any parse failure ⇒
/// treated as unknown freshness ⇒ `Active`, never `Orphan`). `owner_type` is
/// GitHub's `owner.type` (`"Organization"` / `"User"`). `stars` is retained by
/// the caller for display and does not currently gate the tier.
pub fn classify_trust_tier(
    pushed_at: Option<&str>,
    owner_type: Option<&str>,
    _stars: u64,
    now: DateTime<Utc>,
) -> TrustTier {
    let months = pushed_at
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| months_since(d.with_timezone(&Utc), now));

    match months {
        Some(m) if m > ORPHAN_MONTHS => TrustTier::Orphan,
        Some(m) => {
            let is_org = owner_type == Some("Organization");
            if is_org && m <= FRESH_MONTHS {
                TrustTier::Official
            } else {
                TrustTier::Active
            }
        }
        // Unknown push date: neutral, not a demotion.
        None => TrustTier::Active,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 8, 0, 0, 0).unwrap()
    }

    fn ago_months(m: i64) -> String {
        // Approx m months back from `now()`.
        (now() - chrono::Duration::days((m as f64 * 30.44) as i64)).to_rfc3339()
    }

    #[test]
    fn org_and_fresh_is_official() {
        let t = classify_trust_tier(Some(&ago_months(2)), Some("Organization"), 500, now());
        assert_eq!(t, TrustTier::Official);
    }

    #[test]
    fn org_but_stale_is_orphan() {
        // Age dominates: even an org repo untouched for >12mo is orphaned.
        let t = classify_trust_tier(Some(&ago_months(18)), Some("Organization"), 9000, now());
        assert_eq!(t, TrustTier::Orphan);
    }

    #[test]
    fn user_recent_is_active_not_official() {
        let t = classify_trust_tier(Some(&ago_months(1)), Some("User"), 44, now());
        assert_eq!(t, TrustTier::Active);
    }

    #[test]
    fn old_user_repo_is_orphan() {
        let t = classify_trust_tier(Some(&ago_months(20)), Some("User"), 3, now());
        assert_eq!(t, TrustTier::Orphan);
    }

    #[test]
    fn org_fresh_boundary_between_6_and_12_is_active() {
        // 8 months: not stale (<12) but past the 6-month "official" freshness bar.
        let t = classify_trust_tier(Some(&ago_months(8)), Some("Organization"), 100, now());
        assert_eq!(t, TrustTier::Active);
    }

    #[test]
    fn unknown_push_date_is_active_neutral() {
        assert_eq!(
            classify_trust_tier(None, Some("User"), 0, now()),
            TrustTier::Active
        );
        assert_eq!(
            classify_trust_tier(Some("not-a-date"), Some("Organization"), 0, now()),
            TrustTier::Active
        );
    }

    #[test]
    fn tier_serializes_lowercase() {
        assert_eq!(TrustTier::Official.as_str(), "official");
        assert_eq!(
            serde_json::to_string(&TrustTier::Orphan).unwrap(),
            "\"orphan\""
        );
    }
}
