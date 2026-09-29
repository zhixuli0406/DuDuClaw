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
    pub(crate) fn is_safe_odoo_url(url: &str) -> bool {
        if url.len() > 512 {
            return false;
        }
        // Allow HTTP only for strict localhost — must be followed by '/' or ':' or end of string
        for prefix in &["http://127.0.0.1", "http://localhost", "http://[::1]"] {
            if let Some(rest) = url.strip_prefix(prefix) {
                if rest.is_empty() || rest.starts_with('/') || rest.starts_with(':') {
                    return true;
                }
            }
        }
        if url.starts_with("https://") {
            // Reject private/reserved IPs to prevent SSRF against cloud metadata, LAN, etc.
            let host_part = &url["https://".len()..];
            // Extract host (before first '/' or ':' for port)
            let host = host_part.split(&['/', ':'][..]).next().unwrap_or("");
            return !Self::is_private_host(host);
        }
        false
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

    /// Check if an IP address is private, loopback, link-local, or otherwise reserved.
    pub(crate) fn is_private_ip(ip: std::net::IpAddr) -> bool {
        match ip {
            std::net::IpAddr::V4(v4) => {
                v4.is_loopback()           // 127.0.0.0/8
                    || v4.is_private()      // 10/8, 172.16/12, 192.168/16
                    || v4.is_link_local()   // 169.254/16
                    || v4.is_unspecified()  // 0.0.0.0
                    || v4.is_broadcast()    // 255.255.255.255
                    || v4.octets()[0] == 100 && (v4.octets()[1] & 0xC0) == 64 // 100.64/10 (CGNAT)
            }
            std::net::IpAddr::V6(v6) => {
                v6.is_loopback()           // ::1
                    || v6.is_unspecified()  // ::
                    // IPv4-mapped (::ffff:x.x.x.x) — check the embedded v4
                    || v6.to_ipv4_mapped().is_some_and(|v4| Self::is_private_ip(std::net::IpAddr::V4(v4)))
                    // Link-local (fe80::/10)
                    || (v6.segments()[0] & 0xffc0) == 0xfe80
                    // Unique Local Address (fc00::/7)
                    || (v6.octets()[0] & 0xfe) == 0xfc
            }
        }
    }
}
