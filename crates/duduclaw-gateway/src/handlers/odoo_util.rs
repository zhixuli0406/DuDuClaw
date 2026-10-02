//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    pub(crate) async fn handle_odoo_status(&self) -> WsFrame {
        let config_path = self.home_dir.join("config.toml");
        let table = self.read_config_table(&config_path).await;
        let odoo_cfg = duduclaw_odoo::OdooConfig::from_toml(&table);

        if !odoo_cfg.is_configured() {
            return WsFrame::ok_response(
                "",
                json!({
                    "connected": false,
                }),
            );
        }

        // Decrypt credential
        let credential = match self.resolve_odoo_credential(&table) {
            Some(c) if !c.is_empty() => c,
            _ => {
                return WsFrame::ok_response(
                    "",
                    json!({
                        "connected": false,
                        "error": "No credential configured",
                    }),
                );
            }
        };

        match duduclaw_odoo::OdooConnector::connect(&odoo_cfg, &credential).await {
            Ok(conn) => {
                let st = conn.status();
                WsFrame::ok_response(
                    "",
                    json!({
                        "connected": st.connected,
                        "edition": st.edition,
                        "version": st.version,
                        "uid": st.uid,
                    }),
                )
            }
            Err(e) => {
                warn!("Odoo connection failed: {e}");
                WsFrame::ok_response(
                    "",
                    json!({
                        "connected": false,
                        "error": "Connection failed",
                    }),
                )
            }
        }
    }

    /// Return the current Odoo config (without secrets).
    /// Returns `null` if Odoo is not configured.
    pub(crate) async fn handle_odoo_config(&self) -> WsFrame {
        let config_path = self.home_dir.join("config.toml");
        let table = self.read_config_table(&config_path).await;
        let cfg = duduclaw_odoo::OdooConfig::from_toml(&table);

        if !cfg.is_configured() {
            return WsFrame::ok_response("", json!(null));
        }

        WsFrame::ok_response(
            "",
            json!({
                "url": cfg.url,
                "db": cfg.db,
                "protocol": cfg.protocol,
                "auth_method": cfg.auth_method,
                "username": cfg.username,
                "poll_enabled": cfg.poll_enabled,
                "poll_interval_seconds": cfg.poll_interval_seconds,
                "poll_models": cfg.poll_models,
                "webhook_enabled": cfg.webhook_enabled,
                // Whether each write-only credential is on file. The values stay
                // server-side, but without these flags the form shows the same
                // `••••••••` placeholder whether a secret is stored or not — the
                // user cannot tell a saved credential from an empty field, which
                // is the same confusion the Google tab caused.
                "has_api_key": !cfg.api_key_enc.is_empty(),
                "has_password": !cfg.password_enc.is_empty(),
                "has_webhook_secret": !cfg.webhook_secret_enc.is_empty()
                    || !cfg.webhook_secret.is_empty(),
                "unblock_models": cfg.unblock_models,
                "features_crm": cfg.features_crm,
                "features_sale": cfg.features_sale,
                "features_inventory": cfg.features_inventory,
                "features_accounting": cfg.features_accounting,
                "features_project": cfg.features_project,
                "features_hr": cfg.features_hr,
            }),
        )
    }

    /// Validate an Odoo model name (e.g. `crm.lead`, `sale.order`) for a
    /// per-agent `allowed_models` entry. Syntax-only: `allowed_models` is a
    /// *restriction* filter (a model listed here is still subject to the
    /// built-in block list + `unblock_models` at call time — listing it never
    /// grants access past a security default), so blocked model names such as
    /// `res.partner` are accepted here. This lets an operator scope an agent to
    /// `res.partner` for the field-whitelisted `odoo_partner_search`, or pair it
    /// with `unblock_models` for generic access, without the form rejecting it.
    pub(crate) fn is_valid_odoo_model(name: &str) -> bool {
        !name.is_empty()
            && name.len() < 100
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
    }

    /// Validate that a URL is safe for Odoo connections.
    /// Requires HTTPS with non-private host, except for strict localhost.
    ///
    /// The host is taken from the parsed URL, never by prefix: with a prefix
    /// check `http://localhost:3000@evil.com` passed as local (its host is
    /// `evil.com`), and `https://user@10.0.0.1/` hid a private address behind
    /// userinfo. Any userinfo is refused outright.
    pub(crate) fn is_safe_odoo_url(url: &str) -> bool {
        if url.len() > 512 {
            return false;
        }
        let Ok(parsed) = url::Url::parse(url) else {
            return false;
        };
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return false;
        }
        let Some(host) = parsed.host_str() else {
            return false;
        };
        match parsed.scheme() {
            // Allow HTTP only for strict localhost.
            "http" => matches!(host, "127.0.0.1" | "localhost" | "[::1]"),
            // Reject private/reserved IPs to prevent SSRF against cloud metadata, LAN, etc.
            "https" => !Self::is_private_host(host),
            _ => false,
        }
    }

    /// Check if a hostname is a private/reserved IP or a known metadata endpoint.
    /// Uses `std::net::IpAddr` parsing to correctly handle all IPv4/IPv6 representations,
    /// including IPv4-mapped IPv6 (`::ffff:10.0.0.1`), compressed forms, etc.
    pub(crate) fn is_private_host(host: &str) -> bool {
        // Strip brackets for IPv6 literals (e.g. "[::1]" → "::1")
        let raw = host.trim_start_matches('[').trim_end_matches(']');

        // Bare IPv6 without brackets (contains ':' but no '[') — reject as ambiguous
        if !host.starts_with('[') && raw.contains(':') {
            return true;
        }

        if let Ok(ip) = raw.parse::<std::net::IpAddr>() {
            return Self::is_private_ip(ip);
        }

        // Hostname-based checks
        let lower = host.to_ascii_lowercase();
        lower == "localhost"
            || lower.ends_with(".localhost")
            || lower == "metadata.google.internal"
            || lower == "metadata.azure.internal"
    }

    /// Check if an IP address is private, loopback, link-local, or otherwise
    /// not a public internet address — the workspace-wide classifier
    /// ([`duduclaw_core::net_addr::is_public_ip`]).
    pub(crate) fn is_private_ip(ip: std::net::IpAddr) -> bool {
        !duduclaw_core::net_addr::is_public_ip(&ip)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn odoo_url_host_is_parsed_not_prefix_matched() {
        for url in [
            "http://localhost:3000@evil.com",
            "http://localhost@evil.com/odoo",
            "http://localhost.evil.com",
            "http://127.0.0.1.evil.com",
            "https://user@10.0.0.1/",
            "https://user:pw@odoo.example.com/",
            "https://169.254.169.254/",
            "http://odoo.example.com",
            "ftp://localhost",
            "not a url",
        ] {
            assert!(!MethodHandler::is_safe_odoo_url(url), "{url} must be refused");
        }
        for url in [
            "http://localhost",
            "http://localhost:8069",
            "http://127.0.0.1:8069/web",
            "http://[::1]:8069",
            "https://odoo.example.com",
            "https://odoo.example.com:8443/odoo",
        ] {
            assert!(MethodHandler::is_safe_odoo_url(url), "{url} must be accepted");
        }
    }
}
