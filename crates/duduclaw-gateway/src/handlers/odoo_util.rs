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
                let profile = conn.schema_profile("default");
                let schema = duduclaw_odoo::schema::load_scan_status(&self.home_dir, &profile);
                duduclaw_odoo::schema::spawn_auto_scan(Arc::new(conn), self.home_dir.clone(), profile);
                WsFrame::ok_response(
                    "",
                    json!({
                        "connected": st.connected,
                        "edition": st.edition,
                        "version": st.version,
                        "uid": st.uid,
                        "schema": schema,
                    }),
                )
            }
            Err(e) => {
                warn!("Odoo connection failed");
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

    /// Permission editor metadata. Reads the existing snapshot, never scans
    /// business records or changes permissions. Authenticate the saved default
    /// connection first so metadata cannot cross Odoo user/company boundaries.
    pub(crate) async fn handle_odoo_permission_options(&self) -> WsFrame {
        let table = self.read_config_table(&self.home_dir.join("config.toml")).await;
        let cfg = duduclaw_odoo::OdooConfig::from_toml(&table);
        let mut snapshot = None;
        if cfg.is_configured() {
            if let Some(credential) = self.resolve_odoo_credential(&table).filter(|s| !s.is_empty()) {
                if let Ok(Ok(conn)) = tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    duduclaw_odoo::OdooConnector::connect(&cfg, &credential),
                ).await {
                    let profile = conn.schema_profile("default");
                    snapshot = duduclaw_odoo::schema::load_snapshot(&self.home_dir, &profile)
                        .ok().flatten()
                        .filter(|s| s.connection_identity == conn.connection_identity());
                    duduclaw_odoo::schema::spawn_auto_scan(Arc::new(conn), self.home_dir.clone(), profile);
                }
            }
        }
        WsFrame::ok_response("", odoo_permission_options(snapshot.as_ref()))
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
                "blocked_models": duduclaw_odoo::connector::BLOCKED_MODELS,
                "read_only_models": duduclaw_odoo::connector::READ_ONLY_WHEN_UNBLOCKED,
                "allow_private_network": cfg.allow_private_network,
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
        duduclaw_odoo::url_guard::is_safe_odoo_url(url)
    }
}

fn odoo_permission_options(snapshot: Option<&duduclaw_odoo::schema::SchemaSnapshot>) -> Value {
    let models: Vec<Value> = snapshot.into_iter().flat_map(|s| &s.models)
        .map(|model| json!({ "model": model.model, "name": model.name, "access": snapshot.and_then(|s| s.access_rights.get(&model.model)) }))
        .collect();
    json!({
        "models": models,
        "scan_available": snapshot.is_some(),
        "blocked_models": duduclaw_odoo::connector::BLOCKED_MODELS,
        "read_only_models": duduclaw_odoo::connector::READ_ONLY_WHEN_UNBLOCKED,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_options_return_only_model_labels_from_snapshot() {
        let snapshot: duduclaw_odoo::schema::SchemaSnapshot = serde_json::from_value(json!({
            "schema_version": 1, "scanned_at": "2026-11-03T00:00:00Z",
            "server_version": "19.0", "connection_identity": "private-identity",
            "modules": [], "total_models": 1, "truncated": false, "truncation_reasons": [],
            "models": [{ "model": "crm.lead", "name": "Leads", "transient": false,
                "custom": false, "fields": [], "total_fields": 0, "truncated": false }],
        })).unwrap();
        let options = odoo_permission_options(Some(&snapshot));
        assert_eq!(options["models"], json!([{ "model": "crm.lead", "name": "Leads", "access": null }]));
        assert_eq!(options["scan_available"], true);
        assert!(!options.to_string().contains("private-identity"));
        assert!(options["blocked_models"].as_array().unwrap().contains(&json!("hr.employee")));
    }

    #[test]
    fn permission_options_include_cached_tri_state_rights_without_connection_identity() {
        let snapshot = serde_json::from_value::<duduclaw_odoo::schema::SchemaSnapshot>(json!({
            "schema_version": 1, "scanned_at": "now", "server_version": "18.0",
            "connection_identity": "private-identity", "modules": [], "total_models": 1,
            "truncated": false, "truncation_reasons": [],
            "models": [{"model":"crm.lead", "name":"Leads", "transient":false,
                "custom":false, "fields":[], "total_fields":0, "truncated":false}],
            "access_rights": {"crm.lead":{"read":true,"create":false,"write":null,"unlink":false,"checked_at":"now"}}
        })).unwrap();
        let options = odoo_permission_options(Some(&snapshot));
        assert_eq!(options["models"][0]["access"]["create"], false);
        assert!(options["models"][0]["access"]["write"].is_null());
        assert_eq!(options["models"][0]["access"]["checked_at"], "now");
        assert!(!options.to_string().contains("private-identity"));
    }

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
