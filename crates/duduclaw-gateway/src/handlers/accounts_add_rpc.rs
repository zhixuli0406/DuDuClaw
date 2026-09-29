//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    /// Add a new account to config.toml [[accounts]] array.
    ///
    /// Encrypts the API key before storing. Supports `api_key` and `oauth` types.
    ///
    /// WP-A: accepts an optional `provider` (one of
    /// `duduclaw_core::provider_env::KNOWN_PROVIDER_IDS`; defaults to
    /// `"anthropic"` for back-compat with every pre-WP-A caller). An unknown
    /// provider id is rejected fail-closed rather than silently accepted —
    /// `resolve_env_key`/`select_for_provider` would otherwise treat it as a
    /// pool nothing ever selects, a confusing way to fail.
    pub(crate) async fn handle_accounts_add(&self, params: Value) -> WsFrame {
        self.handle_accounts_add_with_probe_base(
            params,
            duduclaw_agent::credential_probe::ANTHROPIC_API_BASE,
        )
        .await
    }

    /// `accounts.add` with an injectable probe base URL (D6).
    ///
    /// Production always calls this with the real Anthropic API base via
    /// [`handle_accounts_add`](Self::handle_accounts_add); tests point it at a
    /// local listener so write-time verification is exercised without a
    /// network round-trip (and without ever sending a fixture "secret"
    /// anywhere).
    pub(crate) async fn handle_accounts_add_with_probe_base(
        &self,
        params: Value,
        probe_base: &str,
    ) -> WsFrame {
        let id = match params.get("id").and_then(|v| v.as_str()) {
            Some(id) if !id.is_empty() => id,
            _ => return WsFrame::error_response("", "Missing 'id' parameter"),
        };
        let auth_type = params
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("api_key");
        if auth_type != "api_key" && auth_type != "oauth" {
            return WsFrame::error_response(
                "",
                &format!("Invalid 'type' parameter '{auth_type}' (must be 'api_key' or 'oauth')"),
            );
        }
        let provider = params
            .get("provider")
            .and_then(|v| v.as_str())
            .unwrap_or("anthropic");
        if !duduclaw_core::provider_env::KNOWN_PROVIDER_IDS.contains(&provider) {
            return WsFrame::error_response(
                "",
                &format!(
                    "Unknown 'provider' parameter '{provider}' (must be one of: {})",
                    duduclaw_core::provider_env::KNOWN_PROVIDER_IDS.join(", ")
                ),
            );
        }
        let key = match params.get("key").and_then(|v| v.as_str()) {
            Some(k) if !k.is_empty() => k,
            _ => return WsFrame::error_response("", "Missing 'key' parameter"),
        };
        let budget_cents = params
            .get("monthly_budget_cents")
            .and_then(|v| v.as_u64())
            .unwrap_or(5000);
        let priority = params.get("priority").and_then(|v| v.as_u64()).unwrap_or(1);

        let config_path = self.home_dir.join("config.toml");
        let mut table = self.read_config_table(&config_path).await;

        // Ensure [[accounts]] array exists
        let accounts = table
            .entry("accounts")
            .or_insert_with(|| toml::Value::Array(Vec::new()));
        let arr = match accounts.as_array_mut() {
            Some(a) => a,
            None => {
                return WsFrame::error_response("", "Invalid 'accounts' section in config.toml");
            }
        };

        // Check for duplicate id
        if arr.iter().any(|a| {
            a.as_table()
                .and_then(|t| t.get("id").and_then(|v| v.as_str()))
                == Some(id)
        }) {
            return WsFrame::error_response("", &format!("Account '{id}' already exists"));
        }

        // D6 (2026-09 credential hardening): verify BEFORE persisting.
        //
        // On 2026-09-08 an operator pasted a short-lived `sk-ant-at01-` access
        // token into this form. It was accepted, encrypted, written, and every
        // scheduled job in the install failed for 18 hours. A credential the
        // dashboard has never authenticated must not be presented to the user
        // as a working account.
        //
        // Three-way outcome, mirroring `CredentialProbe`'s own honesty split:
        // a *conclusive* rejection (401/403) refuses the write; a *proven*
        // credential is saved with `verified: true`; an inconclusive probe
        // (offline install, rate-limited probe) still saves — configuring an
        // account must work without connectivity — but says so honestly with
        // `verified: false`. Non-Anthropic providers have no probe endpoint
        // here at all (D8) and report `verified: null`.
        //
        // Runs after every id/type/provider/duplicate check (a rejected call
        // still costs zero network) and before the encrypt/write.
        let verified: Option<bool> = if provider == "anthropic" {
            use duduclaw_agent::credential_probe::{
                CredentialKind, CredentialProbe, looks_like_access_token,
                probe_anthropic_credential_at,
            };
            // Shape check first: no network call can improve on knowing the
            // paste is the wrong *kind* of token (a fresh at01 authenticates
            // fine right now and dies in a few hours — probing it would
            // cheerfully return 200 and enshrine the incident).
            if auth_type == "oauth" && looks_like_access_token(key) {
                return WsFrame::error_response(
                    "",
                    "這是短效 access token（sk-ant-at01-），幾小時就會失效；請在終端執行 \
                     `claude setup-token` 取得 sk-ant-oat01- 開頭的 token 再貼上",
                );
            }
            let kind = if auth_type == "oauth" {
                CredentialKind::OAuthToken
            } else {
                CredentialKind::ApiKey
            };
            match probe_anthropic_credential_at(probe_base, kind, key).await {
                CredentialProbe::Valid => Some(true),
                CredentialProbe::InvalidCredential => {
                    return WsFrame::error_response(
                        "",
                        "憑證無效（401）：Anthropic 拒絕了這個 token/key，請確認是否貼錯或已撤銷",
                    );
                }
                CredentialProbe::OrgDisabled => {
                    return WsFrame::error_response(
                        "",
                        "此組織已停用 Claude Code 訂閱存取（403）：請改用 API key 帳號，或請組織管理員在 Anthropic 後台開啟",
                    );
                }
                // Inconclusive — says nothing about the credential, so it must
                // not block the write. `detail` is secret-redacted at the
                // source (`credential_probe::unknown_detail`).
                other => {
                    let detail = match &other {
                        CredentialProbe::Unknown(d) => d.as_str(),
                        _ => "rate limited",
                    };
                    warn!(
                        id,
                        detail,
                        "credential could not be verified (network/rate limit) — saved unverified"
                    );
                    Some(false)
                }
            }
        } else {
            None
        };

        // Encrypt the key
        let encrypted = crate::config_crypto::encrypt_value(key, &self.home_dir);

        let entry = build_account_entry(
            id,
            auth_type,
            provider,
            budget_cents,
            priority,
            key,
            encrypted.as_deref(),
        );
        if entry.plaintext_fallback {
            warn!(
                id,
                field = entry.key_field,
                "could not encrypt account credential (no writable keyfile) — storing it as \
                 plaintext in config.toml; run `duduclaw doctor` after fixing the keyfile"
            );
        }
        arr.push(toml::Value::Table(entry.table));

        // Atomic write
        let tmp_path = config_path.with_extension("toml.tmp");
        if let Err(e) = self.write_config_table(&tmp_path, &table).await {
            return WsFrame::error_response("", &format!("Failed to write config: {e}"));
        }
        if let Err(e) = tokio::fs::rename(&tmp_path, &config_path).await {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return WsFrame::error_response("", &format!("Failed to commit config: {e}"));
        }

        // Credentials doctrine P2 (WP-8A): invalidate-on-write, not the old
        // 5-minute rotator TTL — the new account is visible to the very next
        // dispatch/channel-reply call instead of up to 5 minutes later.
        crate::claude_runner::invalidate_rotator_cache().await;

        info!(id, auth_type, provider, verified, "accounts.add completed");
        WsFrame::ok_response(
            "",
            json!({
                "success": true,
                "id": id,
                "type": auth_type,
                "provider": provider,
                // D6: `true` = authenticated just now, `false` = saved but the
                // probe could not reach a verdict, `null` = not probe-able
                // (non-Anthropic provider). The dashboard renders all three.
                "verified": verified,
            }),
        )
    }

    /// Update the monthly budget for a specific account in config.toml.
    pub(crate) async fn handle_accounts_update_budget(&self, params: Value) -> WsFrame {
        let account_id = match params.get("account_id").and_then(|v| v.as_str()) {
            Some(id) if !id.is_empty() => id,
            _ => return WsFrame::error_response("", "Missing 'account_id' parameter"),
        };
        let budget_cents = match params.get("monthly_budget_cents").and_then(|v| v.as_u64()) {
            Some(v) => v,
            None => {
                return WsFrame::error_response(
                    "",
                    "Missing 'monthly_budget_cents' parameter (integer)",
                );
            }
        };

        let config_path = self.home_dir.join("config.toml");
        let mut table = self.read_config_table(&config_path).await;

        // Find the target account in [[accounts]] array
        let accounts = match table.get_mut("accounts").and_then(|v| v.as_array_mut()) {
            Some(arr) => arr,
            None => return WsFrame::error_response("", "No [[accounts]] section in config.toml"),
        };

        let target = accounts.iter_mut().find(|a| {
            a.as_table()
                .and_then(|t| t.get("id").and_then(|v| v.as_str()))
                == Some(account_id)
        });

        match target {
            Some(account) => {
                if let Some(t) = account.as_table_mut() {
                    t.insert(
                        "monthly_budget_cents".into(),
                        toml::Value::Integer(budget_cents as i64),
                    );
                }
            }
            None => {
                return WsFrame::error_response("", &format!("Account not found: {account_id}"));
            }
        }

        // Atomic write
        let tmp_path = config_path.with_extension("toml.tmp");
        if let Err(e) = self.write_config_table(&tmp_path, &table).await {
            return WsFrame::error_response("", &format!("Failed to write config: {e}"));
        }
        if let Err(e) = tokio::fs::rename(&tmp_path, &config_path).await {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return WsFrame::error_response("", &format!("Failed to commit config: {e}"));
        }

        // Credentials doctrine P2 (WP-8A): the cached rotator holds the whole
        // parsed [[accounts]] row including budget — invalidate so budget
        // enforcement sees the new ceiling on the next call, not up to 5
        // minutes later.
        crate::claude_runner::invalidate_rotator_cache().await;

        info!(account_id, budget_cents, "accounts.update_budget completed");
        WsFrame::ok_response(
            "",
            json!({
                "success": true,
                "account_id": account_id,
                "monthly_budget_cents": budget_cents,
            }),
        )
    }

    /// `accounts.update` — general edit of a `[[accounts]]` entry (G.5).
    /// Params: `{ account_id, priority?, tags?[], profile?, email?,
    /// subscription?, label?, monthly_budget_cents? }`. Does NOT touch the
    /// account secret (use `accounts.add` to (re)set keys). Atomic write.
    pub(crate) async fn handle_accounts_update(&self, params: Value) -> WsFrame {
        let account_id = match params.get("account_id").and_then(|v| v.as_str()) {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => return WsFrame::error_response("", "Missing 'account_id' parameter"),
        };

        let config_path = self.home_dir.join("config.toml");
        let mut table = self.read_config_table(&config_path).await;

        let accounts = match table.get_mut("accounts").and_then(|v| v.as_array_mut()) {
            Some(arr) => arr,
            None => return WsFrame::error_response("", "No [[accounts]] section in config.toml"),
        };
        let target = accounts.iter_mut().find(|a| {
            a.as_table()
                .and_then(|t| t.get("id").and_then(|v| v.as_str()))
                == Some(account_id.as_str())
        });
        let account = match target.and_then(|a| a.as_table_mut()) {
            Some(t) => t,
            None => {
                return WsFrame::error_response("", &format!("Account not found: {account_id}"));
            }
        };

        let mut changes: Vec<String> = Vec::new();
        if let Some(v) = params.get("priority").and_then(|v| v.as_u64()) {
            account.insert("priority".into(), toml::Value::Integer(v as i64));
            changes.push(format!("priority = {v}"));
        }
        if let Some(v) = params.get("monthly_budget_cents").and_then(|v| v.as_u64()) {
            account.insert(
                "monthly_budget_cents".into(),
                toml::Value::Integer(v as i64),
            );
            changes.push(format!("monthly_budget_cents = {v}"));
        }
        for key in &["profile", "email", "subscription", "label"] {
            if let Some(v) = params.get(*key).and_then(|v| v.as_str()) {
                account.insert((*key).into(), toml::Value::String(v.trim().into()));
                changes.push(format!("{key} = \"{}\"", v.trim()));
            }
        }
        if let Some(arr) = params.get("tags").and_then(|v| v.as_array()) {
            let tags: Vec<toml::Value> = arr
                .iter()
                .filter_map(|v| {
                    v.as_str()
                        .filter(|s| !s.is_empty())
                        .map(|s| toml::Value::String(s.into()))
                })
                .collect();
            account.insert("tags".into(), toml::Value::Array(tags.clone()));
            changes.push(format!("tags = [{} entries]", tags.len()));
        }

        if changes.is_empty() {
            return WsFrame::error_response(
                "",
                "No valid fields to update (priority/tags/profile/email/subscription/label/monthly_budget_cents)",
            );
        }

        if let Err(e) = self.atomic_write_toml(&config_path, &table).await {
            return WsFrame::error_response("", &e);
        }

        // Credentials doctrine P2 (WP-8A): priority/tags/profile changes also
        // land in the cached rotator's parsed account list — invalidate so
        // rotation strategy reflects them on the next call.
        crate::claude_runner::invalidate_rotator_cache().await;

        info!(account_id, ?changes, "accounts.update completed");
        WsFrame::ok_response(
            "",
            json!({ "success": true, "account_id": account_id, "changes": changes }),
        )
    }
}
