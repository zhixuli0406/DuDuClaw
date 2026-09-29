//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── MCP OAuth handlers ──────────────────────────────────

    /// List available OAuth providers with configuration and token status.
    pub(crate) async fn handle_mcp_oauth_providers(&self) -> WsFrame {
        use crate::mcp_oauth;

        let redirect_uri = mcp_oauth::redirect_uri(&self.home_dir);
        // Every built-in provider is listed, Google included. Filtering Google
        // out when `[integrations] google_workspace` is off used to hide the
        // *tools* gate behind a *credentials* lie: the Google tab (visible since
        // v1.49.0) reads this list to decide whether credentials are on file, so
        // a filtered-out provider rendered the "you have not set this up yet"
        // form on top of a working, fully-configured integration. The tools gate
        // is surfaced where it belongs — the Google tab prints an explicit
        // warning when it is off, and the MCP tab hides the Google card itself.
        let providers = mcp_oauth::builtin_providers(&redirect_uri);

        let results: Vec<Value> = providers
            .iter()
            .map(|p| {
                let token = mcp_oauth::get_stored_token(&self.home_dir, &p.provider_id);
                let stored = mcp_oauth::get_client_config(&self.home_dir, &p.provider_id);
                Self::oauth_provider_entry(p, token, stored)
            })
            .collect();

        WsFrame::ok_response("", json!({ "providers": results }))
    }

    /// Build one `mcp.oauth.providers` row. Pure — takes what was loaded from
    /// disk so the display contract can be tested without a live gateway.
    ///
    /// Two rules this encodes:
    ///  - **Expiry is not connection state.** An expired access token that still
    ///    carries a refresh token is refreshed transparently on the next call,
    ///    so it reports `authenticated`. Only a token that expired with nothing
    ///    to refresh from really needs the user to authorize again. Google
    ///    access tokens last an hour, which made the old expiry-based reading
    ///    call every healthy connection "not connected" within the hour.
    ///  - **Saved credentials must be visible.** The client id goes back in full
    ///    (it is public — it travels in the authorization URL); the client
    ///    secret never does, only proof one is on file plus a four-character
    ///    tail so the operator can tell which secret is stored.
    pub(crate) fn oauth_provider_entry(
        p: &crate::mcp_oauth::McpOAuthConfig,
        token: Option<crate::mcp_oauth::McpOAuthToken>,
        stored: Option<crate::mcp_oauth::McpOAuthClientConfig>,
    ) -> Value {
        use crate::mcp_oauth;

        let expired = token
            .as_ref()
            .map(mcp_oauth::token_expired)
            .unwrap_or(false);
        let can_refresh = token
            .as_ref()
            .map(|t| t.refresh_token.is_some())
            .unwrap_or(false);
        let status = match &token {
            Some(_) if !expired || can_refresh => "authenticated",
            Some(_) => "expired",
            None => "none",
        };

        // `configured` reflects persisted client credentials — the built-in
        // templates always carry an empty client_id, so the stored config is
        // what tells us the user has set theirs up.
        let client_id = if p.client_id.is_empty() {
            stored
                .as_ref()
                .map(|c| c.client_id.clone())
                .unwrap_or_default()
        } else {
            p.client_id.clone()
        };
        let stored_secret = stored
            .as_ref()
            .map(|c| c.client_secret.as_str())
            .unwrap_or("");

        json!({
            "provider_id": p.provider_id,
            "name": Self::oauth_provider_name(&p.provider_id),
            "auth_url": p.auth_url,
            "scopes": p.scopes,
            "configured": !client_id.is_empty(),
            "client_id": client_id,
            "has_client_secret": !stored_secret.is_empty(),
            "client_secret_masked": mcp_oauth::mask_secret_tail(stored_secret),
            "status": status,
            // `status` is the historical key; `token_status` is what the
            // dashboard's provider type has always read — it was never sent, so
            // every connected provider rendered as unauthenticated. Both go out.
            "token_status": status,
            "can_refresh": can_refresh,
            "access_token_valid": token.is_some() && !expired,
            "expires_at": token.and_then(|t| t.expires_at),
            // The exact URI to register with the provider. Derived from the live
            // gateway port — a UI constant would drift the moment anyone sets
            // DUDUCLAW_PORT or edits config.toml [gateway] port.
            "redirect_uri": p.redirect_uri,
        })
    }

    /// Start an OAuth flow: generate PKCE, store pending state, return auth URL.
    pub(crate) async fn handle_mcp_oauth_start(&self, params: Value) -> WsFrame {
        use crate::mcp_oauth;

        let provider_id = match params.get("provider_id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => return WsFrame::error_response("", "provider_id is required"),
        };

        let client_id = params
            .get("client_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let client_secret = params
            .get("client_secret")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // Find the built-in provider or create a custom one
        let redirect_uri = mcp_oauth::redirect_uri(&self.home_dir);
        let mut config = mcp_oauth::builtin_providers(&redirect_uri)
            .into_iter()
            .find(|p| p.provider_id == provider_id)
            .unwrap_or_else(|| mcp_oauth::McpOAuthConfig {
                provider_id: provider_id.clone(),
                client_id: String::new(),
                client_secret: String::new(),
                auth_url: params
                    .get("auth_url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                token_url: params
                    .get("token_url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                scopes: params
                    .get("scopes")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|s| s.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default(),
                redirect_uri: redirect_uri.clone(),
            });

        // Prefill from previously-stored client credentials so re-authorizing
        // (e.g. to grant new scopes) doesn't require re-entering the secret.
        if let Some(stored) = mcp_oauth::get_client_config(&self.home_dir, &provider_id) {
            if config.client_id.is_empty() {
                config.client_id = stored.client_id;
            }
            if config.client_secret.is_empty() {
                config.client_secret = stored.client_secret;
            }
            if config.auth_url.is_empty() {
                config.auth_url = stored.auth_url;
            }
            if config.token_url.is_empty() {
                config.token_url = stored.token_url;
            }
        }

        // Override client_id/secret if provided in params (form input wins).
        if !client_id.is_empty() {
            config.client_id = client_id;
        }
        if !client_secret.is_empty() {
            config.client_secret = client_secret;
        }

        if config.client_id.is_empty() {
            return WsFrame::error_response(
                "",
                "client_id is required (provide in params or pre-configure)",
            );
        }
        if config.auth_url.is_empty() || config.token_url.is_empty() {
            return WsFrame::error_response(
                "",
                "auth_url and token_url are required for custom providers",
            );
        }

        // Persist client credentials so the token can be refreshed later without
        // re-prompting the user (secret encrypted at rest). See mcp_oauth.
        if let Err(e) = mcp_oauth::upsert_client_config(
            &self.home_dir,
            mcp_oauth::McpOAuthClientConfig {
                provider_id: provider_id.clone(),
                client_id: config.client_id.clone(),
                client_secret: config.client_secret.clone(),
                auth_url: config.auth_url.clone(),
                token_url: config.token_url.clone(),
                scopes: config.scopes.clone(),
                redirect_uri: config.redirect_uri.clone(),
            },
        ) {
            warn!(provider = %provider_id, error = %e, "Failed to persist OAuth client config");
        }

        // Generate PKCE
        let (code_verifier, code_challenge) = mcp_oauth::generate_pkce();
        let state = uuid::Uuid::new_v4().to_string();

        let auth_url = mcp_oauth::build_auth_url(&config, &state, &code_challenge);

        // Store pending
        let pending = mcp_oauth::PendingOAuth {
            provider_id: provider_id.clone(),
            state: state.clone(),
            code_verifier,
            config,
            created_at: std::time::Instant::now(),
        };

        {
            let mut map = self.mcp_oauth_pending.write().await;
            // Cleanup expired entries
            mcp_oauth::cleanup_pending(&mut map);
            map.insert(state.clone(), pending);
        }

        info!(provider = %provider_id, "MCP OAuth flow started");

        WsFrame::ok_response(
            "",
            json!({
                "auth_url": auth_url,
                "state": state,
            }),
        )
    }
}
