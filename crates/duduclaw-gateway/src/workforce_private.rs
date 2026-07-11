//! WP6-T6.4b — semantic "suspected private use" detection (opt-in, off by default).
//!
//! This is labour-relations sensitive, so the false-positive guards are not
//! optional. This module owns the deterministic decision core — everything that
//! decides *whether* something becomes a flag — so it can be unit-tested away
//! from the LLM batch. The Haiku classification call and the operator-only UI
//! build on top; they must not weaken these rules.
//!
//! Non-negotiable rules encoded here:
//! - **Fail-closed on no baseline**: without an operator-provided business scope,
//!   "private" is undefined, so detection refuses to run.
//! - **Only high-confidence "suspected private"** becomes a flag; "undetermined"
//!   is never flagged.
//! - **Exempt list** short-circuits before any flag.
//! - **Flags expire** (default 30 days) unless an operator confirms them.
//! - Flags are advisory ("建議關注"), never grounds for discipline, and never
//!   visible to employees — enforced at the UI layer, asserted by these rules.

use serde::{Deserialize, Serialize};

/// Operator config for the feature. Off by default; requires a business-scope
/// baseline before it will run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PrivateUseConfig {
    /// Master opt-in. Default false.
    pub enabled: bool,
    /// Free-text description of what counts as company business. Empty ⇒ feature
    /// refuses to run (fail-closed) even if `enabled = true`.
    pub business_scope: String,
    /// Optional shared-wiki page holding the business scope (read fresh each run).
    pub business_scope_wiki_page: String,
    /// Minimum confidence for a "suspected private" classification to be flagged.
    pub confidence_threshold: f64,
    /// user_ids / channels excluded from detection entirely.
    pub exempt: Vec<String>,
    /// Days a flag lives before auto-expiry unless an operator confirms it.
    pub flag_ttl_days: i64,
}

impl Default for PrivateUseConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            business_scope: String::new(),
            business_scope_wiki_page: String::new(),
            confidence_threshold: 0.8,
            exempt: Vec::new(),
            flag_ttl_days: 30,
        }
    }
}

impl PrivateUseConfig {
    /// Whether detection may run: opt-in AND a non-empty business-scope baseline
    /// (inline or via wiki page). Fail-closed — no baseline, no detection.
    pub fn detection_enabled(&self) -> bool {
        self.enabled
            && (!self.business_scope.trim().is_empty()
                || !self.business_scope_wiki_page.trim().is_empty())
    }

    /// Is this user/channel exempt from detection? Exact match on either.
    pub fn is_exempt(&self, user_id: &str, channel: &str) -> bool {
        self.exempt.iter().any(|e| e == user_id || e == channel)
    }
}

/// The classifier's verdict for one sampled message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivacyClass {
    /// Clearly company business.
    Business,
    /// Looks like personal/private use.
    SuspectedPrivate,
    /// Not enough signal to decide — must NEVER be flagged.
    Undetermined,
}

/// Decide whether a classification result becomes a flag. Fail-closed:
/// only a `SuspectedPrivate` verdict at/above the confidence threshold, for a
/// non-exempt user, is flagged. Everything else returns false.
pub fn should_flag(
    cfg: &PrivateUseConfig,
    class: PrivacyClass,
    confidence: f64,
    user_id: &str,
    channel: &str,
) -> bool {
    if !cfg.detection_enabled() {
        return false;
    }
    if cfg.is_exempt(user_id, channel) {
        return false;
    }
    matches!(class, PrivacyClass::SuspectedPrivate) && confidence >= cfg.confidence_threshold
}

/// Has an unconfirmed flag created at `created_at_rfc3339` expired at `now`?
/// Confirmed flags never expire here (the caller checks `confirmed` first).
pub fn flag_expired(cfg: &PrivateUseConfig, created_at_rfc3339: &str, now: chrono::DateTime<chrono::Utc>) -> bool {
    match chrono::DateTime::parse_from_rfc3339(created_at_rfc3339) {
        Ok(created) => {
            let age = now.signed_duration_since(created.with_timezone(&chrono::Utc));
            age.num_days() >= cfg.flag_ttl_days
        }
        // Unparseable timestamp ⇒ treat as expired (fail-safe: don't keep a flag
        // we can't age).
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> PrivateUseConfig {
        PrivateUseConfig {
            enabled: true,
            business_scope: "電商客服與訂單處理".into(),
            confidence_threshold: 0.8,
            exempt: vec!["u-boss".into()],
            ..Default::default()
        }
    }

    #[test]
    fn disabled_or_no_baseline_never_flags() {
        let mut c = cfg();
        c.enabled = false;
        assert!(!should_flag(&c, PrivacyClass::SuspectedPrivate, 0.99, "u1", "tg"));
        // Enabled but no baseline ⇒ still fail-closed.
        let mut c2 = cfg();
        c2.business_scope = String::new();
        c2.business_scope_wiki_page = String::new();
        assert!(!c2.detection_enabled());
        assert!(!should_flag(&c2, PrivacyClass::SuspectedPrivate, 0.99, "u1", "tg"));
    }

    #[test]
    fn only_high_confidence_suspected_private_flags() {
        let c = cfg();
        assert!(should_flag(&c, PrivacyClass::SuspectedPrivate, 0.80, "u1", "tg"));
        assert!(should_flag(&c, PrivacyClass::SuspectedPrivate, 0.95, "u1", "tg"));
        // Below threshold ⇒ no flag.
        assert!(!should_flag(&c, PrivacyClass::SuspectedPrivate, 0.79, "u1", "tg"));
        // Undetermined ⇒ NEVER flagged, even at confidence 1.0.
        assert!(!should_flag(&c, PrivacyClass::Undetermined, 1.0, "u1", "tg"));
        // Business ⇒ never flagged.
        assert!(!should_flag(&c, PrivacyClass::Business, 1.0, "u1", "tg"));
    }

    #[test]
    fn exempt_user_or_channel_never_flags() {
        let c = cfg();
        assert!(!should_flag(&c, PrivacyClass::SuspectedPrivate, 0.99, "u-boss", "tg"));
        let mut c2 = cfg();
        c2.exempt = vec!["private-channel".into()];
        assert!(!should_flag(&c2, PrivacyClass::SuspectedPrivate, 0.99, "u1", "private-channel"));
    }

    #[test]
    fn flag_expiry() {
        let c = cfg(); // 30 day ttl
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert!(flag_expired(&c, "2026-06-01T00:00:00Z", now)); // 61 days old
        assert!(!flag_expired(&c, "2026-07-25T00:00:00Z", now)); // 7 days old
        assert!(flag_expired(&c, "not-a-date", now)); // unparseable ⇒ expired
    }
}
