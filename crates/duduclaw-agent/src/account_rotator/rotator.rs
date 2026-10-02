//! [`AccountRotator`] itself plus its construction and the `config.toml` /
//! OAuth-session load path. Moved verbatim out of `account_rotator.rs`.

use super::*;

impl AccountRotator {
    pub fn new(strategy: RotationStrategy, cooldown_seconds: u64) -> Self {
        Self {
            accounts: Arc::new(RwLock::new(Vec::new())),
            inherit_host_credentials: AtomicBool::new(true),
            strategy,
            round_robin_index: Arc::new(RwLock::new(0)),
            cooldown_seconds,
            probe_base_url: ANTHROPIC_API_BASE.to_string(),
        }
    }

    /// Point the credential health probe at a different API base.
    ///
    /// Builder-style; production never calls this (the default is the real
    /// Anthropic API). Tests use it to drive `probe_and_restore` against a
    /// local listener with no network access.
    pub fn with_probe_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.probe_base_url = base_url.into();
        self
    }

    /// Load explicit accounts, optionally inheriting host OAuth/environment keys.
    pub async fn load_from_config(&self, home_dir: &Path) -> Result<usize, String> {
        self.load_from_config_using(home_dir, detect_default_oauth_session).await
    }

    /// Keep host-session detection injectable without mutating process PATH/HOME.
    pub(super) async fn load_from_config_using(
        &self,
        home_dir: &Path,
        detector: impl FnOnce() -> Option<Account> + Send + 'static,
    ) -> Result<usize, String> {
        let config_path = home_dir.join("config.toml");
        let config = async {
            let content = match tokio::fs::read_to_string(&config_path).await {
                Ok(content) => content,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(error) => return Err(format!("Failed to read account config.toml ({:?})", error.kind())),
            };
            // TOML errors can quote source lines containing credentials. Keep
            // the diagnostic actionable without returning the source text.
            let table: toml::Table = content.parse()
                .map_err(|_| "Failed to parse account config.toml".to_string())?;
            let inherit = host_credentials_policy(&table)?;
            Ok((table, inherit))
        }.await;
        let (table, inherit_host_credentials) = match config {
            Ok(config) => config,
            Err(error) => {
                let mut accounts = self.accounts.write().await;
                accounts.clear();
                self.inherit_host_credentials.store(false, Ordering::Relaxed);
                return Err(error);
            }
        };

        let mut loaded = Vec::new();

        // 1. Load API key accounts from [[accounts]]
        if let Some(accs) = table.get("accounts").and_then(|v| v.as_array()) {
            for acc in accs {
                if let Some(acc_table) = acc.as_table() {
                    let id = acc_table.get("id").and_then(|v| v.as_str()).unwrap_or("unnamed");
                    let auth_type = acc_table.get("type").and_then(|v| v.as_str()).unwrap_or("api_key");

                    if auth_type == "api_key" {
                        let api_key = resolve_api_key(home_dir, acc_table).await;
                        // D4: an entry that *declares* an encrypted key but
                        // resolves to nothing is BROKEN, not absent. Skipping
                        // it (the old behavior) hid a wrong/rotated `.keyfile`
                        // behind a silently smaller pool.
                        let broken =
                            api_key.is_empty() && has_nonempty_field(acc_table, API_KEY_ENC_FIELDS);
                        if broken {
                            warn!(
                                account = id,
                                reason = "api_key_enc present but decrypted to nothing",
                                "Account credential is BROKEN — it will never be selected. \
                                 Check `~/.duduclaw/.keyfile` (was it regenerated or copied \
                                 from another machine?) and re-save this account's API key."
                            );
                        } else if api_key.is_empty() {
                            continue;
                        }
                        let provider = acc_table
                            .get("provider")
                            .and_then(|v| v.as_str())
                            .unwrap_or("anthropic")
                            .to_string();
                        loaded.push(Account {
                            id: id.to_string(),
                            auth_method: AuthMethod::ApiKey,
                            provider,
                            priority: acc_table.get("priority").and_then(|v| v.as_integer()).unwrap_or(10) as u32,
                            monthly_budget_cents: acc_table.get("monthly_budget_cents").and_then(|v| v.as_integer()).unwrap_or(5000) as u64,
                            tags: Vec::new(),
                            profile: String::new(),
                            email: String::new(),
                            subscription: String::new(),
                            label: acc_table.get("label").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                            expires_at: None,
                            api_key,
                            oauth_token: None,
                            credentials_dir: None,
                            is_healthy: !broken,
                            consecutive_errors: 0,
                            spent_this_month: 0,
                            cooldown_until: None,
                            last_used: None,
                            total_requests: 0,
                            credential_state: if broken {
                                CredentialState::Broken
                            } else {
                                CredentialState::Unverified
                            },
                            auth_dead_strikes: 0,
                            next_probe_at: None,
                            probe_failures: 0,
                        });
                    } else if auth_type == "oauth" {
                        let profile = acc_table.get("profile").and_then(|v| v.as_str()).unwrap_or("default");
                        // Subscription source. Absent → "anthropic" (Claude.ai),
                        // preserving byte-identical behavior for every existing
                        // config. A non-anthropic value (openai/github/qwen) marks
                        // a consumer subscription seat from another provider.
                        let provider = acc_table
                            .get("provider")
                            .and_then(|v| v.as_str())
                            .unwrap_or("anthropic")
                            .to_string();
                        let email = acc_table.get("email").and_then(|v| v.as_str()).unwrap_or("");
                        let sub = acc_table.get("subscription").and_then(|v| v.as_str()).unwrap_or("");
                        let label = acc_table.get("label").and_then(|v| v.as_str()).unwrap_or("");
                        let expires_at = acc_table.get("expires_at").and_then(|v| v.as_str()).map(|s| s.to_string());
                        let creds_dir = resolve_oauth_credentials(profile);
                        let oauth_token = resolve_oauth_token(home_dir, acc_table).await;

                        // D4: `oauth_token_enc` declared but resolving to
                        // None/"" is the exact trap `detect_default_oauth_session`'s
                        // own doc comment describes and nothing enforced — the
                        // account loaded normally and spawned credential-less
                        // children with zero warnings. An OAuth entry with NO
                        // `_enc` field is not broken: it legitimately relies on
                        // the OS keychain (behavior unchanged).
                        let token_usable =
                            oauth_token.as_ref().is_some_and(|t| !t.trim().is_empty());
                        let broken =
                            !token_usable && has_nonempty_field(acc_table, OAUTH_TOKEN_ENC_FIELDS);
                        if broken {
                            warn!(
                                account = id,
                                reason = "oauth_token_enc present but decrypted to nothing",
                                "Account credential is BROKEN — it will never be selected. \
                                 Check `~/.duduclaw/.keyfile` (was it regenerated or copied \
                                 from another machine?) and re-save this account's token \
                                 (`claude setup-token`)."
                            );
                        }
                        // An empty-string token must never reach the spawn env
                        // as `CLAUDE_CODE_OAUTH_TOKEN=`; normalize it away.
                        let oauth_token = if token_usable { oauth_token } else { None };

                        let has_auth = oauth_token.is_some() || creds_dir.is_some();

                        loaded.push(Account {
                            id: id.to_string(),
                            auth_method: AuthMethod::OAuth,
                            provider,
                            priority: acc_table.get("priority").and_then(|v| v.as_integer()).unwrap_or(5) as u32,
                            monthly_budget_cents: 0,
                            tags: Vec::new(),
                            profile: profile.to_string(),
                            email: email.to_string(),
                            subscription: sub.to_string(),
                            label: label.to_string(),
                            expires_at,
                            api_key: String::new(),
                            oauth_token,
                            credentials_dir: creds_dir,
                            is_healthy: has_auth && !broken,
                            consecutive_errors: 0,
                            spent_this_month: 0,
                            cooldown_until: None,
                            last_used: None,
                            total_requests: 0,
                            credential_state: if broken {
                                CredentialState::Broken
                            } else {
                                CredentialState::Unverified
                            },
                            auth_dead_strikes: 0,
                            next_probe_at: None,
                            probe_failures: 0,
                        });
                    }
                }
            }
        }

        // 2. Auto-detect default OAuth session via `claude auth status`.
        //
        // Gate on an *Anthropic* OAuth account specifically — a foreign-provider
        // OAuth seat (copilot / qwen / codex added via `duduclaw auth device`)
        // must NOT suppress the Anthropic host-login auto-detect, or the
        // anthropic pool ends up empty and every channel reply fails NoAccounts.
        if inherit_host_credentials && should_autodetect_anthropic_oauth(&loaded) {
            // Use spawn_blocking to avoid holding a tokio worker thread
            // while waiting for the `claude` CLI subprocess.
            let detected = tokio::task::spawn_blocking(detector)
                .await
                .ok()
                .flatten();
            if let Some(creds) = detected {
                loaded.push(creds);
            }
        }

        // 3. Fallback: single API key from [api] or env var
        if loaded.is_empty()
            && let Some(api) = table.get("api").and_then(|v| v.as_table()) {
                let api_key = resolve_api_key(home_dir, api).await;
                if !api_key.is_empty() {
                    loaded.push(Account {
                        id: "main".to_string(),
                        auth_method: AuthMethod::ApiKey,
                        provider: "anthropic".to_string(),
                        priority: 1,
                        monthly_budget_cents: 10000,
                        tags: Vec::new(),
                        profile: String::new(),
                        email: String::new(),
                        subscription: String::new(),
                        label: String::new(),
                        expires_at: None,
                        api_key,
                        oauth_token: None,
                        credentials_dir: None,
                        is_healthy: true,
                        consecutive_errors: 0,
                        spent_this_month: 0,
                        cooldown_until: None,
                        last_used: None,
                        total_requests: 0,
                        credential_state: CredentialState::Unverified,
                        auth_dead_strikes: 0,
                        next_probe_at: None,
                        probe_failures: 0,
                    });
                }
            }

        if inherit_host_credentials && loaded.is_empty()
            && let Ok(key) = std::env::var("ANTHROPIC_API_KEY")
                && !key.is_empty() {
                    loaded.push(Account {
                        id: "env".to_string(),
                        auth_method: AuthMethod::ApiKey,
                        provider: "anthropic".to_string(),
                        priority: 99,
                        monthly_budget_cents: 10000,
                        tags: Vec::new(),
                        profile: String::new(),
                        email: String::new(),
                        subscription: String::new(),
                        label: "環境變數".to_string(),
                        expires_at: None,
                        api_key: key,
                        oauth_token: None,
                        credentials_dir: None,
                        is_healthy: true,
                        consecutive_errors: 0,
                        spent_this_month: 0,
                        cooldown_until: None,
                        last_used: None,
                        total_requests: 0,
                        credential_state: CredentialState::Unverified,
                        auth_dead_strikes: 0,
                        next_probe_at: None,
                        probe_failures: 0,
                    });
                }

        let oauth_count = loaded.iter().filter(|a| a.auth_method == AuthMethod::OAuth).count();
        let apikey_count = loaded.iter().filter(|a| a.auth_method == AuthMethod::ApiKey).count();
        let count = loaded.len();

        // Check token expiry warnings
        for acc in &loaded {
            if let Some(days) = acc.days_until_expiry() {
                if days <= 0 {
                    warn!(
                        account = %acc.id,
                        label = %acc.label,
                        "OAuth token EXPIRED — run `claude setup-token` to renew"
                    );
                } else if days <= 7 {
                    warn!(
                        account = %acc.id,
                        label = %acc.label,
                        days_remaining = days,
                        "OAuth token expiring soon — run `claude setup-token` to renew"
                    );
                } else if days <= 30 {
                    info!(
                        account = %acc.id,
                        label = %acc.label,
                        days_remaining = days,
                        "OAuth token will expire in {days} days"
                    );
                }
            }
        }

        info!(total = count, oauth = oauth_count, api_key = apikey_count, strategy = ?self.strategy, "Accounts loaded");
        let mut accounts = self.accounts.write().await;
        *accounts = loaded;
        self.inherit_host_credentials.store(inherit_host_credentials, Ordering::Relaxed);
        Ok(count)
    }
}
