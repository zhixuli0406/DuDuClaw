//! Outcome bookkeeping: success / auth-failure / rate-limit / billing
//! transitions, monthly reset and the status projections.
//! Moved verbatim out of `account_rotator.rs`.

use super::*;

impl AccountRotator {
    pub async fn on_success(&self, account_id: &str, cost_cents: u64) {
        let mut accounts = self.accounts.write().await;
        if let Some(acc) = accounts.iter_mut().find(|a| a.id == account_id) {
            acc.consecutive_errors = 0;
            // Only restore health if not in active cooldown set by another worker.
            // This prevents a stale success from overriding a concurrent rate-limit.
            let in_cooldown = acc.cooldown_until.is_some_and(|cd| Utc::now() < cd);
            if !in_cooldown {
                acc.is_healthy = true;
            }
            // A completed request is the strongest possible evidence that the
            // credential works, so it clears an AuthDead verdict and the
            // backoff ladder even mid-cooldown (mirroring the existing
            // `consecutive_errors = 0`). `Broken` is deliberately sticky: it
            // means we never produced a usable secret in the first place, so a
            // success attributed to this id cannot be evidence about it — and
            // a Broken account is never selectable, so this branch is
            // unreachable in practice. Fail closed rather than resurrect.
            if acc.credential_state != CredentialState::Broken {
                acc.credential_state = CredentialState::Ok;
            }
            acc.auth_dead_strikes = 0;
            // A completed request also settles the probe schedule: there is
            // nothing left to back off from.
            acc.probe_failures = 0;
            acc.next_probe_at = None;
            acc.spent_this_month += cost_cents;
            acc.total_requests += 1;
            acc.last_used = Some(Utc::now());
        }
    }

    /// Record an **authentication** failure (2026-09 hardening, D2).
    ///
    /// Distinct from [`on_error`](Self::on_error) because a dead credential is
    /// not a transient fault: three strikes and a 2-minute cooldown were
    /// exactly wrong for it. The account goes unhealthy immediately, is marked
    /// [`CredentialState::AuthDead`], and books an exponential cooldown
    /// (15 min → 30 → 60 → … capped at 6 h) so a re-issued token still recovers
    /// on its own, but a genuinely dead one stops burning one spawn per cron
    /// tick.
    ///
    /// Callers classify the failure (`oauth_not_allowed_for_organization` and
    /// friends → [`AuthFailureKind::OrgDisabled`]; `invalid bearer token` /
    /// `authentication_failed` → [`AuthFailureKind::InvalidToken`]).
    pub async fn on_auth_failed(&self, account_id: &str, kind: AuthFailureKind) {
        let mut accounts = self.accounts.write().await;
        if let Some(acc) = accounts.iter_mut().find(|a| a.id == account_id) {
            acc.is_healthy = false;
            acc.credential_state = CredentialState::AuthDead(kind);
            acc.auth_dead_strikes = acc.auth_dead_strikes.saturating_add(1);
            // A real spawn just failed, so the next health tick must be free
            // to probe and classify *this* failure — clear any probe schedule
            // an earlier round booked. `probe_failures` is deliberately NOT
            // bumped here: it counts probe verdicts, not spawn outcomes, and
            // bumping it would let an observed failure silently lengthen the
            // schedule without a single probe having been answered.
            acc.next_probe_at = None;
            let backoff = auth_dead_backoff(acc.auth_dead_strikes);
            let until = Utc::now() + backoff;
            // Never shorten an existing (e.g. 24 h billing) cooldown.
            acc.cooldown_until = Some(acc.cooldown_until.map_or(until, |cur| cur.max(until)));
            warn!(
                account = account_id,
                kind = %kind,
                strikes = acc.auth_dead_strikes,
                cooldown_minutes = backoff.num_minutes(),
                "Account authentication FAILED — credential marked auth-dead. \
                 It will be retried once when the cooldown expires; fix the \
                 credential in 設定→帳號 to recover sooner."
            );
        }
    }

    /// Record a generic (non-billing, non-rate-limit) failure for an account.
    ///
    /// **WP10 fix (2026-08-04 field incident)**: marking `is_healthy = false`
    /// used to leave `cooldown_until = None`. `Account::is_available` only
    /// forgives an unhealthy account once its cooldown has *expired*, so an
    /// account with no cooldown at all was permanently unavailable — for a
    /// single-account install that meant every subsequent message failed with
    /// "All accounts exhausted" until the 5-minute rotator cache happened to
    /// rebuild or the 60 s health probe managed a successful
    /// `claude auth status`. Attaching the standard cooldown makes the
    /// degradation self-healing and bounded.
    pub async fn on_error(&self, account_id: &str) {
        let mut accounts = self.accounts.write().await;
        if let Some(acc) = accounts.iter_mut().find(|a| a.id == account_id) {
            acc.consecutive_errors += 1;
            if acc.consecutive_errors >= 3 {
                let until = Utc::now() + chrono::Duration::seconds(self.cooldown_seconds as i64);
                warn!(
                    account = account_id,
                    cooldown = self.cooldown_seconds,
                    "Account marked unhealthy after 3 errors — cooling down (auto-recovers)"
                );
                acc.is_healthy = false;
                // Never shorten an existing (e.g. 24 h billing) cooldown.
                acc.cooldown_until = Some(acc.cooldown_until.map_or(until, |cur| cur.max(until)));
            }
        }
    }

    pub async fn on_rate_limited(&self, account_id: &str) {
        let mut accounts = self.accounts.write().await;
        if let Some(acc) = accounts.iter_mut().find(|a| a.id == account_id) {
            acc.cooldown_until = Some(
                Utc::now() + chrono::Duration::seconds(self.cooldown_seconds as i64),
            );
            warn!(account = account_id, cooldown = self.cooldown_seconds, "Account rate-limited");
        }
    }

    /// Billing/credit exhaustion — mark account unhealthy with 24-hour cooldown.
    ///
    /// Unlike rate limiting (minutes), billing exhaustion requires manual top-up
    /// or a new billing cycle, so we use a much longer cooldown.
    pub async fn on_billing_exhausted(&self, account_id: &str) {
        let mut accounts = self.accounts.write().await;
        if let Some(acc) = accounts.iter_mut().find(|a| a.id == account_id) {
            acc.is_healthy = false;
            acc.cooldown_until = Some(Utc::now() + chrono::Duration::hours(24));
            warn!(
                account = account_id,
                "Account billing exhausted — marked unhealthy with 24h cooldown"
            );
        }
    }

    pub async fn reset_monthly(&self) {
        let mut accounts = self.accounts.write().await;
        for acc in accounts.iter_mut() {
            acc.spent_this_month = 0;
        }
    }

    /// WP10 M4 — why is nothing selectable right now?
    ///
    /// Called on the "no account available" path so the user-facing message can
    /// state a realistic recovery horizon instead of one generic sentence. Only
    /// information already in memory is used; nothing is probed.
    ///
    /// The tiers are separated by cooldown length because that IS the recovery
    /// horizon: billing exhaustion books 24 h, while rate-limit and generic
    /// errors book `cooldown_seconds` (120 s by default). Anything above an
    /// hour is therefore billing-class.
    pub async fn unavailable_reason(&self) -> UnavailableReason {
        let accounts = self.accounts.read().await;
        let now = Utc::now();
        let longest = accounts
            .iter()
            .filter(|a| !a.is_available())
            .filter_map(|a| a.cooldown_until)
            .filter(|cd| *cd > now)
            .max();
        match longest {
            Some(cd) if (cd - now) > chrono::Duration::hours(1) => UnavailableReason::LongCooldown,
            Some(_) => UnavailableReason::ShortCooldown,
            // Unavailable for a non-cooldown reason (expired token, budget
            // exhausted, unhealthy with no cooldown attached) — or no accounts
            // at all. Callers must use conservative wording here.
            None => UnavailableReason::Unknown,
        }
    }

    pub async fn status(&self) -> Vec<AccountStatus> {
        let accounts = self.accounts.read().await;
        accounts.iter().map(|a| AccountStatus {
            id: a.id.clone(),
            auth_method: format!("{:?}", a.auth_method).to_lowercase(),
            provider: a.provider.clone(),
            priority: a.priority,
            is_healthy: a.is_healthy,
            spent_this_month: a.spent_this_month,
            monthly_budget_cents: a.monthly_budget_cents,
            total_requests: a.total_requests,
            is_available: a.is_available(),
            email: mask_email(&a.email),
            subscription: a.subscription.clone(),
            label: a.label.clone(),
            tags: a.tags.clone(),
            expires_at: a.expires_at.clone(),
            days_until_expiry: a.days_until_expiry(),
            credential_state: a.credential_state,
            credential_detail: a.credential_state.credential_detail(),
            auth_dead_strikes: a.auth_dead_strikes,
            next_probe_at: a.next_probe_at.map(|t| t.to_rfc3339()),
            probe_failures: a.probe_failures,
        }).collect()
    }

    pub async fn count(&self) -> usize {
        self.accounts.read().await.len()
    }

    /// Test-only: push a pre-built account directly into the rotator.
    ///
    /// Bypasses config file loading and OAuth auto-detection. Cross-crate
    /// integration tests need deterministic account state — in particular,
    /// channel-reply rotation tests inject synthetic OAuth accounts so the
    /// spawn closure can simulate rate-limit / success patterns.
    ///
    /// Not intended for production code. Marked `#[doc(hidden)]` so it does
    /// not appear in public API docs.
    #[doc(hidden)]
    pub async fn push_account_for_test(&self, account: Account) {
        self.accounts.write().await.push(account);
    }

    /// Test-only: the account's current cooldown deadline.
    ///
    /// [`AccountStatus`] deliberately exposes only `is_available` (a boolean),
    /// which cannot tell a 2-minute generic-error cooldown from the 15-minute
    /// auth-dead base — a distinction the gateway's D2 wiring tests must be
    /// able to make. Read-only and `#[doc(hidden)]`, like
    /// [`push_account_for_test`](Self::push_account_for_test).
    #[doc(hidden)]
    pub async fn cooldown_until_for_test(&self, account_id: &str) -> Option<DateTime<Utc>> {
        self.accounts
            .read()
            .await
            .iter()
            .find(|a| a.id == account_id)
            .and_then(|a| a.cooldown_until)
    }
}

/// Mask an account e-mail for status output: first two characters of the
/// local part, then `***@domain`. Char-based, so a non-ASCII local part
/// (e.g. `王小明@…`) cannot panic on a byte boundary (v1.68.0; was a byte
/// slice).
pub(crate) fn mask_email(email: &str) -> String {
    match email.split_once('@') {
        Some((local, domain)) => {
            let prefix: String = local.chars().take(2).collect();
            format!("{prefix}***@{domain}")
        }
        None if email.is_empty() => String::new(),
        None => "***".to_string(),
    }
}

#[cfg(test)]
mod mask_email_tests {
    use super::mask_email;

    #[test]
    fn masks_ascii_and_non_ascii_local_parts() {
        assert_eq!(mask_email("alice@example.com"), "al***@example.com");
        assert_eq!(mask_email("王小明@example.com"), "王小***@example.com");
        assert_eq!(mask_email("é@x.tw"), "é***@x.tw");
        assert_eq!(mask_email("@x.tw"), "***@x.tw");
        assert_eq!(mask_email(""), "");
        assert_eq!(mask_email("no-at-sign"), "***");
    }
}
