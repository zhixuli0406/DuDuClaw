//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Accounts ─────────────────────────────────────────────

    pub(crate) async fn handle_accounts_list(&self) -> WsFrame {
        let rotator = self.cached_rotator().await;
        let accounts = rotator.status().await;
        let accounts_json: Vec<Value> = accounts.iter().map(account_status_to_json).collect();
        WsFrame::ok_response("", json!({ "accounts": accounts_json }))
    }

    /// `accounts.cli_credentials` — see the dispatch comment. Presence + mtime
    /// only; never reads credential content.
    pub(crate) async fn handle_accounts_cli_credentials(&self) -> WsFrame {
        let credentials: Vec<Value> = crate::cli_auth::cli_credential_statuses()
            .into_iter()
            .map(|c| {
                json!({
                    "runtime": c.runtime.as_str(),
                    "store": c.store,
                    "installed": c.installed,
                    "present": c.present,
                    "modified_at": c.modified_epoch,
                })
            })
            .collect();
        WsFrame::ok_response("", json!({ "credentials": credentials }))
    }

    pub(crate) async fn handle_budget_summary(&self) -> WsFrame {
        let rotator = self.cached_rotator().await;
        let accounts = rotator.status().await;
        let total_budget: u64 = accounts.iter().map(|a| a.monthly_budget_cents).sum();

        // Headline "spent" comes from CostTelemetry (persistent, real) rather
        // than summing the rotator's in-memory per-account counters, which reset
        // on restart / rebuild and stay 0 for OAuth-subscription accounts.
        //
        // CostTelemetry attributes cost per AGENT, not per ACCOUNT (API key), so
        // there is no faithful per-account breakdown — `spent_this_month` on each
        // account card stays the rotator's best-effort value, but the aggregate
        // bar (the figure users actually read) is now correct.
        let total_spent = self.telemetry_spent_cents_total().await;

        // WP-A: use the same row shape as `accounts.list` (`account_status_to_json`)
        // instead of a second, narrower hand-rolled subset — the dashboard's
        // `AccountInfo` type already declared `provider`/`label`/`email`/
        // `subscription`/`total_requests`/`is_available`/`expires_at`/
        // `days_until_expiry`, but this RPC (the one `AccountsPage` actually
        // calls for its account cards) never sent them, so they always read
        // as `undefined` at runtime. Widening this to the full row fixes that
        // latent gap for free and lets `provider` reach the accounts table.
        let accounts_json: Vec<Value> = accounts.iter().map(account_status_to_json).collect();

        WsFrame::ok_response(
            "",
            json!({
                "total_budget_cents": total_budget,
                "total_spent_cents": total_spent,
                "accounts": accounts_json,
            }),
        )
    }

    pub(crate) async fn handle_accounts_rotate(&self, _params: Value) -> WsFrame {
        let rotator = self.cached_rotator().await;
        match rotator.select().await {
            Some(selected) => WsFrame::ok_response(
                "",
                json!({
                    "success": true,
                    "selected_account": selected.id,
                    "strategy": "configured",
                    "message": format!("Rotated to account '{}'", selected.id),
                }),
            ),
            None => WsFrame::error_response("", "No available accounts for rotation"),
        }
    }

    pub(crate) async fn handle_accounts_health(&self) -> WsFrame {
        let rotator = self.cached_rotator().await;
        let accounts = rotator.status().await;
        let healthy_count = accounts.iter().filter(|a| a.is_healthy).count();
        let status = if accounts.is_empty() {
            "no_accounts"
        } else if healthy_count == accounts.len() {
            "healthy"
        } else if healthy_count > 0 {
            "degraded"
        } else {
            "unhealthy"
        };

        let accounts_json: Vec<Value> = accounts
            .iter()
            .map(|a| {
                json!({
                    "id": a.id,
                    "healthy": a.is_healthy,
                    "available": a.is_available,
                    "spent": a.spent_this_month,
                    "budget": a.monthly_budget_cents,
                    "requests": a.total_requests,
                })
            })
            .collect();

        WsFrame::ok_response(
            "",
            json!({
                "status": status,
                "healthy_count": healthy_count,
                "total_count": accounts.len(),
                "accounts": accounts_json,
            }),
        )
    }

    /// Get or create a cached rotator (uses the same static cache as claude_runner).
    pub(crate) async fn cached_rotator(
        &self,
    ) -> std::sync::Arc<duduclaw_agent::account_rotator::AccountRotator> {
        // Reuse the global cache from claude_runner to avoid redundant disk reads
        match crate::claude_runner::get_rotator_cached(&self.home_dir).await {
            Ok(r) => r,
            Err(_) => {
                // Fallback: create a fresh one
                let config_content = tokio::fs::read_to_string(self.home_dir.join("config.toml"))
                    .await
                    .unwrap_or_default();
                let config_table: toml::Table = config_content.parse().unwrap_or_default();
                let rotator = duduclaw_agent::account_rotator::create_from_config(&config_table);
                let _ = rotator.load_from_config(&self.home_dir).await;
                std::sync::Arc::new(rotator)
            }
        }
    }

    /// Real month-to-date spend in **cents** across all agents, sourced from
    /// `CostTelemetry` (the persistent SQLite ledger).
    ///
    /// The `AccountRotator`'s `spent_this_month` counter is in-memory only: it
    /// resets to 0 on every gateway restart and every rotator rebuild — which
    /// now happens on the next call after any `[[accounts]]` write
    /// (invalidate-on-write, WP-8A) or, for a write this process has no
    /// invalidate hook for, after the 30-minute backstop TTL — and stays 0
    /// for OAuth-subscription accounts (which have no per-call cost).
    /// CostTelemetry records every request's real token cost keyed by agent, so
    /// it is the correct source for "how much was actually used this month".
    pub(crate) async fn telemetry_spent_cents_total(&self) -> u64 {
        let Some(telemetry) = crate::cost_telemetry::get_telemetry() else {
            return 0;
        };
        match telemetry.summary_global(hours_since_month_start()).await {
            // `cost_millicents` is a misnomer — `estimated_cost_millicents()`
            // produces whole CENTS (e.g. 1M output tokens @ $15/M → 1500 = $15.00),
            // so this value is already in cents. No scaling.
            Ok(summary) => summary.total_cost_millicents,
            Err(_) => 0,
        }
    }

    /// Real month-to-date spend in **cents** for a single agent, from
    /// `CostTelemetry`. See [`Self::telemetry_spent_cents_total`] for why this
    /// is preferred over the rotator counter.
    pub(crate) async fn telemetry_spent_cents_for_agent(&self, agent_id: &str) -> u64 {
        let Some(telemetry) = crate::cost_telemetry::get_telemetry() else {
            return 0;
        };
        match telemetry
            .summary_by_agent(agent_id, hours_since_month_start())
            .await
        {
            // Already in cents — see `telemetry_spent_cents_total`.
            Ok(agent) => agent.summary.total_cost_millicents,
            Err(_) => 0,
        }
    }
}
