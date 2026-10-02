//! Account selection: strategy application, provider filtering and the
//! seat/token accessors. Moved verbatim out of `account_rotator.rs`.

use super::*;

impl AccountRotator {
    /// Select the best available Anthropic account and return env vars for the
    /// `claude` CLI.
    ///
    /// Back-compat shim: identical to `select_for_provider("anthropic")`. Every
    /// pre-existing caller (gateway channel reply, claude_runner, agent runner,
    /// fork `RotatorProvider`) keeps working byte-for-byte.
    pub async fn select(&self) -> Option<AccountEnv> {
        self.select_for_provider("anthropic").await
    }

    /// [`select`](Self::select) restricted to an agent's configured
    /// `agent.toml [model] account_pool`.
    ///
    /// An empty `pool` is byte-identical to [`select`](Self::select).
    /// See [`select_for_provider_with_pool`](Self::select_for_provider_with_pool)
    /// for the full semantics (including the fail-open rule).
    pub async fn select_with_pool(&self, pool: &[String]) -> Option<AccountEnv> {
        self.select_for_provider_with_pool("anthropic", pool).await
    }

    /// Select the best available account *for a specific provider* and return
    /// its env vars + raw key.
    ///
    /// Only accounts whose `provider` matches are considered; health, cooldown,
    /// budget, and the rotation strategy are all applied *within* that provider
    /// pool. If the config declares no accounts for `provider`, a single
    /// ephemeral account is synthesized from the provider's standard env var
    /// (e.g. `OPENAI_API_KEY`) so a user with just that env var still rotates
    /// (trivially) through the same machinery. This fallback is disabled by
    /// `[account_loading] inherit_host_credentials = false`.
    pub async fn select_for_provider(&self, provider: &str) -> Option<AccountEnv> {
        self.select_for_provider_with_pool(provider, &[]).await
    }

    /// [`select_for_provider`](Self::select_for_provider) restricted to an
    /// agent's configured `agent.toml [model] account_pool`.
    ///
    /// The restriction is applied to the **candidate set only** — after the
    /// provider / health / cooldown / budget filters and *before* the rotation
    /// strategy runs — so Priority / LeastCost / Failover / RoundRobin keep
    /// their exact semantics, just over a narrower set.
    ///
    /// Semantics:
    /// - empty `pool` ⇒ byte-identical to [`select_for_provider`](Self::select_for_provider);
    /// - non-empty `pool` ⇒ candidates are those whose account `id` **or**
    ///   `label` equals a pool entry (trimmed, ASCII-case-insensitive — both
    ///   are user-visible in the dashboard account picker);
    /// - **fail-open**: a pool that matches no *available* account (stale ids,
    ///   renamed accounts, everything cooling down) logs a `warn` and falls
    ///   back to the full candidate set. A stale pool must never brick an
    ///   agent — availability outranks the operator's preference here, and the
    ///   warn is the signal to fix the config.
    pub async fn select_for_provider_with_pool(
        &self,
        provider: &str,
        pool: &[String],
    ) -> Option<AccountEnv> {
        let accounts = self.accounts.read().await;
        let has_any_for_provider = accounts.iter().any(|a| a.provider == provider);
        let mut available: Vec<&Account> = accounts
            .iter()
            .filter(|a| a.provider == provider && a.is_available())
            .collect();

        // Candidate-set narrowing by the agent's account pool (fail-open).
        match narrow_by_pool(&available, pool) {
            PoolNarrowing::NotRequested => {}
            PoolNarrowing::Applied(filtered) => available = filtered,
            PoolNarrowing::FailedOpen => {
                // Distinguish "the pool names accounts that do not exist" from
                // "they exist but are all cooling down" — the operator fix is
                // different (edit the pool vs. wait / add capacity). Computed
                // only on this cold path.
                let known = accounts
                    .iter()
                    .any(|a| a.provider == provider && account_in_pool(a, pool));
                warn!(
                    provider,
                    pool = ?pool,
                    pool_accounts_known = known,
                    "account_pool matched no available account — falling back to the full \
                     account set (fail-open). Fix `agent.toml [model] account_pool` if this \
                     is not intended."
                );
            }
        }

        if available.is_empty() {
            if !has_any_for_provider && self.inherit_host_credentials.load(Ordering::Relaxed) {
                // Hold the accounts read lock through synchronous env lookup
                // so a reload cannot publish a different inheritance policy
                // between checking it and synthesizing an ambient account.
                return env_fallback_account_env(provider);
            }
            warn!(provider, "No available accounts for rotation");
            return None;
        }

        let selected = match self.strategy {
            RotationStrategy::Priority | RotationStrategy::Failover => {
                available.iter().min_by_key(|a| a.priority).copied()
            }
            RotationStrategy::LeastCost => {
                // Prefer OAuth (subscription, no per-token cost), then least spent API key.
                let oauth: Vec<&&Account> = available.iter().filter(|a| a.auth_method == AuthMethod::OAuth).collect();
                if !oauth.is_empty() {
                    // Among OAuth accounts, the lowest "cost" tier is the one
                    // with the least spend. Within that equal-cost tier, rotate
                    // fairly using a least-recently-used tiebreaker instead of
                    // always picking index 0 — otherwise the first OAuth account
                    // takes every request and the others never get used.
                    let min_spent = oauth
                        .iter()
                        .map(|a| a.spent_this_month)
                        .min()
                        .unwrap_or(0);
                    oauth
                        .iter()
                        .filter(|a| a.spent_this_month == min_spent)
                        // `None` (never used) sorts before any timestamp, so
                        // unused accounts are preferred first.
                        .min_by_key(|a| a.last_used)
                        .map(|a| **a)
                } else {
                    available.iter().min_by_key(|a| a.spent_this_month).copied()
                }
            }
            RotationStrategy::RoundRobin => {
                let mut idx = self.round_robin_index.write().await;
                let selected = available[*idx % available.len()];
                *idx = (*idx + 1) % available.len();
                Some(selected)
            }
        };

        selected.map(|a| {
            info!(
                account = %a.id,
                provider = %a.provider,
                method = ?a.auth_method,
                email = %a.email,
                "Account selected for rotation"
            );
            build_account_env(a)
        })
    }

    /// Whether the pool currently has an *available* non-Anthropic OAuth
    /// subscription seat for `provider` that carries a stored seat credential.
    ///
    /// Read-only (no rotation side effects), so it is safe to call from the
    /// proxy's model-catalogue handler to fail-closed: no seat ⇒ no advertised
    /// models for that provider ⇒ 404/503, never a silent Anthropic fallback.
    pub async fn has_seat_for_provider(&self, provider: &str) -> bool {
        let accounts = self.accounts.read().await;
        accounts.iter().any(|a| {
            a.provider == provider
                && a.auth_method == AuthMethod::OAuth
                && a.is_available()
                && a.oauth_token.as_ref().is_some_and(|t| !t.is_empty())
        })
    }

    /// Swap the in-memory seat credential after a token refresh (Qwen seat
    /// rotation). Without this, the next request would re-read the stale
    /// in-memory bundle and try to refresh with an already-rotated (revoked)
    /// refresh token. The caller is responsible for the encrypted persist to
    /// `config.toml`; this only updates the live pool.
    pub async fn update_seat_token(&self, account_id: &str, new_token: &str) {
        let mut accounts = self.accounts.write().await;
        if let Some(acc) = accounts
            .iter_mut()
            .find(|a| a.id == account_id && a.auth_method == AuthMethod::OAuth)
        {
            acc.oauth_token = Some(new_token.to_string());
        } else {
            warn!(account = account_id, "update_seat_token: no OAuth account with this id");
        }
    }
}
