//! The zero-cost credential probe: the operator-facing report, the
//! restore sweep and their supporting types.
//! Moved verbatim out of `account_rotator.rs`.

use super::*;

impl AccountRotator {
    /// Probe every account that carries a probe-able Anthropic secret and
    /// report the verdict, **without touching account state** (D5).
    ///
    /// The diagnostic twin of [`probe_and_restore`](Self::probe_and_restore):
    /// that one is a control loop and only looks at unavailable accounts; this
    /// one looks at *all* of them and changes nothing, so `duduclaw doctor`
    /// can answer "is this token still good?" without perturbing a running
    /// gateway's rotation. Secrets never leave this crate — only the
    /// [`CredentialProbe`] outcome does.
    ///
    /// Probes run sequentially (each capped by the probe's own 10 s timeout);
    /// an account with nothing probe-able reports `probe: None` and costs no
    /// request at all.
    pub async fn probe_credentials_report(&self) -> Vec<CredentialReport> {
        let snapshot: Vec<(CredentialReport, Option<(CredentialKind, String)>)> = {
            let accounts = self.accounts.read().await;
            accounts
                .iter()
                .map(|a| {
                    (
                        CredentialReport {
                            id: a.id.clone(),
                            provider: a.provider.clone(),
                            auth_method: a.auth_method.clone(),
                            state: a.credential_state,
                            probe: None,
                        },
                        probe_secret_for(a),
                    )
                })
                .collect()
        };

        let mut out = Vec::with_capacity(snapshot.len());
        for (mut report, secret) in snapshot {
            if let Some((kind, secret)) = secret {
                report.probe =
                    Some(probe_anthropic_credential_at(&self.probe_base_url, kind, &secret).await);
            }
            out.push(report);
        }
        out
    }

    /// Probe all unhealthy accounts and restore those whose credential really
    /// still authenticates.
    ///
    /// ## 2026-09 rewrite (D3)
    ///
    /// This used to "verify" every OAuth account by running `claude auth
    /// status` and accepting `loggedIn: true`. That signal is true whenever
    /// *any* `CLAUDE_CODE_OAUTH_TOKEN` exists in the environment and says
    /// nothing about the account being probed — so a token that Anthropic had
    /// started answering with `403 oauth_not_allowed_for_organization` was
    /// resurrected every 60 s for 18 hours, each resurrection costing one more
    /// failed scheduled dispatch.
    ///
    /// Now, for accounts that carry a probe-able Anthropic secret (an OAuth
    /// setup-token, or an API key), the probe authenticates **that secret**:
    ///
    /// | outcome | action |
    /// |---|---|
    /// | `Valid` | restore (healthy, no cooldown, state `Ok`, strikes reset, probe schedule cleared) |
    /// | `InvalidCredential` | stay dead, state `AuthDead(InvalidToken)`, cooldown doubled (cap 6 h), next probe backed off |
    /// | `OrgDisabled` | stay dead, state `AuthDead(OrgDisabled)`, cooldown doubled (cap 6 h), next probe backed off |
    /// | `RateLimited` / `Unknown` | untouched — retry next tick |
    ///
    /// A conclusive failure also books [`Account::next_probe_at`], so a
    /// credential the API keeps rejecting is re-checked on a widening
    /// schedule ([`probe_backoff`]: 1 min doubling to a 30 min ceiling)
    /// rather than once a minute forever. Inconclusive outcomes leave the
    /// schedule alone, and a real spawn failure
    /// ([`on_auth_failed`](Self::on_auth_failed)) clears it so the very next
    /// tick classifies the fresh failure.
    ///
    /// Accounts with no probe-able secret (an OS-keychain OAuth session, a
    /// foreign-provider subscription seat) keep the legacy `claude auth
    /// status` / cooldown-expiry path — it remains the only signal available —
    /// **except** when they are already `AuthDead`, which that signal is not
    /// strong enough to clear. Those wait for cooldown expiry and get exactly
    /// one real retry. [`CredentialState::Broken`] accounts are never probed
    /// and never restored.
    ///
    /// Call this periodically (e.g. every 60s) from a background task.
    pub async fn probe_and_restore(&self) -> usize {
        let now = Utc::now();
        let candidates: Vec<ProbeCandidate> = {
            let accounts = self.accounts.read().await;
            accounts.iter()
                .filter(|a| !a.is_healthy || a.cooldown_until.is_some_and(|cd| Utc::now() >= cd))
                .filter(|a| !a.is_available()) // truly unavailable, not just cooled-down-and-ready
                // D4: an undecryptable credential cannot be probed and must
                // never be restored — only re-saving it (which rebuilds the
                // rotator) can fix it.
                .filter(|a| !a.credential_state.is_blocking())
                // Probe schedule: a credential the API has already rejected
                // conclusively waits out its backoff (1 min → 30 min) instead
                // of being re-asked on every 60 s tick. `None` = probe now,
                // which is what every account looks like until its first
                // conclusive failure.
                .filter(|a| a.next_probe_at.is_none_or(|t| now >= t))
                .map(|a| ProbeCandidate {
                    id: a.id.clone(),
                    method: a.auth_method.clone(),
                    state: a.credential_state,
                    secret: probe_secret_for(a),
                })
                .collect()
        };

        if candidates.is_empty() {
            return 0;
        }

        let mut restored = 0u64;

        for candidate in &candidates {
            let id = &candidate.id;
            let method = &candidate.method;

            // ── Path A: a real credential we can actually authenticate ──
            if let Some((kind, secret)) = &candidate.secret {
                let outcome =
                    probe_anthropic_credential_at(&self.probe_base_url, *kind, secret).await;
                match outcome {
                    CredentialProbe::Valid => {
                        let mut accounts = self.accounts.write().await;
                        if let Some(acc) = accounts.iter_mut().find(|a| a.id == *id) {
                            acc.is_healthy = true;
                            acc.consecutive_errors = 0;
                            acc.cooldown_until = None;
                            acc.credential_state = CredentialState::Ok;
                            acc.auth_dead_strikes = 0;
                            acc.probe_failures = 0;
                            acc.next_probe_at = None;
                            restored += 1;
                            info!(
                                account = id.as_str(),
                                method = ?method,
                                priority = acc.priority,
                                "Account restored by credential probe (200 from /v1/models)"
                            );
                        }
                    }
                    CredentialProbe::InvalidCredential | CredentialProbe::OrgDisabled => {
                        let failure = if outcome == CredentialProbe::OrgDisabled {
                            AuthFailureKind::OrgDisabled
                        } else {
                            AuthFailureKind::InvalidToken
                        };
                        let mut accounts = self.accounts.write().await;
                        if let Some(acc) = accounts.iter_mut().find(|a| a.id == *id) {
                            acc.is_healthy = false;
                            acc.credential_state = CredentialState::AuthDead(failure);
                            let until = doubled_cooldown(acc.cooldown_until);
                            acc.cooldown_until =
                                Some(acc.cooldown_until.map_or(until, |cur| cur.max(until)));
                            // Space out the *probe* as well as the rotation
                            // cooldown: re-asking the API every 60 s about a
                            // credential it has conclusively rejected buys
                            // nothing and drowns the log.
                            acc.probe_failures = acc.probe_failures.saturating_add(1);
                            let probe_delay = probe_backoff(acc.probe_failures);
                            acc.next_probe_at = Some(Utc::now() + probe_delay);
                            warn!(
                                account = id.as_str(),
                                kind = %failure,
                                until = %until,
                                next_probe_in_minutes = probe_delay.num_minutes(),
                                "Credential probe confirms the account is auth-dead — \
                                 staying out of rotation with a doubled cooldown"
                            );
                        }
                    }
                    // Inconclusive: the probe says nothing about the
                    // credential, so account state must not move in either
                    // direction. Retry on the next tick.
                    CredentialProbe::RateLimited | CredentialProbe::Unknown(_) => {
                        debug!(
                            account = id.as_str(),
                            outcome = ?outcome,
                            "Credential probe inconclusive — leaving account state untouched"
                        );
                    }
                }
                continue;
            }

            // ── Path B: nothing to authenticate with ────────────────────
            // D3: `claude auth status` is the only signal for a keychain
            // session, but it is far too weak to overturn an observed
            // authentication failure.
            if !legacy_status_probe_may_restore(candidate.state) {
                debug!(
                    account = id.as_str(),
                    state = %candidate.state,
                    "Auth-dead account has no probe-able secret — `claude auth status` \
                     must not restore it; waiting for cooldown expiry"
                );
                continue;
            }

            let ok = match method {
                AuthMethod::OAuth => {
                    // Legacy path: keychain / profile sessions and foreign
                    // subscription seats have no secret we can present.
                    tokio::task::spawn_blocking(|| {
                        let claude = duduclaw_core::which_claude();
                        claude.and_then(|bin| {
                            let output = duduclaw_core::platform::command_for(&bin)
                                .args(["auth", "status"])
                                .stdout(std::process::Stdio::piped())
                                .stderr(std::process::Stdio::null())
                                .output()
                                .ok()?;
                            if !output.status.success() { return None; }
                            let stdout = String::from_utf8_lossy(&output.stdout);
                            let json: serde_json::Value = serde_json::from_str(&stdout).ok()?;
                            json.get("loggedIn").and_then(|v| v.as_bool()).filter(|&b| b)
                        })
                    }).await.ok().flatten().is_some()
                }
                AuthMethod::ApiKey => {
                    // API key accounts: cooldown expiry already handled by is_available().
                    // If we're here, it means the account is unhealthy for non-cooldown reasons.
                    // Just check if cooldown expired — if so, it's safe to restore.
                    let accounts = self.accounts.read().await;
                    accounts.iter()
                        .find(|a| a.id == *id)
                        .is_some_and(|a| {
                            a.cooldown_until.is_none_or(|cd| Utc::now() >= cd)
                        })
                }
            };

            if ok {
                let mut accounts = self.accounts.write().await;
                if let Some(acc) = accounts.iter_mut().find(|a| a.id == *id) {
                    acc.is_healthy = true;
                    acc.consecutive_errors = 0;
                    acc.cooldown_until = None;
                    restored += 1;
                    info!(
                        account = id.as_str(),
                        method = ?method,
                        priority = acc.priority,
                        "Account restored by health probe"
                    );
                }
            }
        }

        restored as usize
    }
}

/// One account's credential verdict, for an operator-facing report
/// (`duduclaw doctor`, D5).
///
/// Carries no secret: the credential is probed inside this crate and only the
/// outcome crosses the boundary.
#[derive(Debug, Clone)]
pub struct CredentialReport {
    pub id: String,
    pub provider: String,
    pub auth_method: AuthMethod,
    /// The account's last known state (before this probe) — surfaced so the
    /// report can say "already marked auth-dead" even when the probe itself
    /// comes back inconclusive.
    pub state: CredentialState,
    /// `None` when the account carries no probe-able Anthropic secret (an
    /// OS-keychain OAuth session, a foreign-provider seat): nothing was sent
    /// and nothing can be concluded.
    pub probe: Option<CredentialProbe>,
}

/// One account's worth of snapshot state for [`AccountRotator::probe_and_restore`].
///
/// Snapshotted under the read lock so no lock is held across the probe's
/// `await` — the pool must stay selectable while a 10-second probe is in
/// flight.
struct ProbeCandidate {
    id: String,
    method: AuthMethod,
    state: CredentialState,
    secret: Option<(CredentialKind, String)>,
}

/// Whether the weak `claude auth status` signal is allowed to restore an
/// account in this credential state (pure, so the rule is testable without a
/// `claude` binary on PATH).
///
/// `AuthDead` is excluded: `loggedIn: true` is reported for *any* ambient
/// session — including one backed by a token Anthropic is currently answering
/// with 403 — so it cannot overturn an observed authentication failure. That
/// false positive is what resurrected a dead account every 60 s for 18 hours
/// on 2026-09-08. `Broken` is excluded because there is nothing to restore.
pub(super) fn legacy_status_probe_may_restore(state: CredentialState) -> bool {
    !matches!(
        state,
        CredentialState::AuthDead(_) | CredentialState::Broken
    )
}

/// The Anthropic secret this account can be probed with, if any.
///
/// `None` for: a non-Anthropic provider (its seat credential means nothing to
/// `api.anthropic.com` — probing it there would produce a confident, wrong
/// verdict), an OS-keychain OAuth session (the secret lives in the keychain,
/// not here), and an empty credential.
pub(super) fn probe_secret_for(a: &Account) -> Option<(CredentialKind, String)> {
    if a.provider != "anthropic" {
        return None;
    }
    match a.auth_method {
        AuthMethod::ApiKey => {
            (!a.api_key.trim().is_empty()).then(|| (CredentialKind::ApiKey, a.api_key.clone()))
        }
        AuthMethod::OAuth => a
            .oauth_token
            .as_ref()
            .filter(|t| !t.trim().is_empty())
            .map(|t| (CredentialKind::OAuthToken, t.clone())),
    }
}
