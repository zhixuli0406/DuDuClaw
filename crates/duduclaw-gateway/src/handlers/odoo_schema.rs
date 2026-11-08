//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    /// The owner gateway alone performs periodic, identity-isolated scans.
    pub(crate) async fn run_odoo_schema_background(&self) {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1800));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            if !duduclaw_core::gateway_instance::held(&self.home_dir) { continue; }
            let _ = self.scan_effective_odoo_connections(true).await;
        }
    }

    pub(crate) async fn handle_odoo_schema_scan(&self) -> WsFrame {
        let _manual = match duduclaw_odoo::schema::claim_manual_scan(
            &self.home_dir, "default", chrono::Utc::now().timestamp(),
        ) {
            Ok(lock) => lock,
            Err(e) => return WsFrame::ok_response("", json!({"success":false,"message":e})),
        };
        WsFrame::ok_response("", self.scan_effective_odoo_connections(false).await)
    }

    /// Enumerate every employee, including inherited connections. A bad override
    /// produces an explicit failure and never falls back to another identity.
    pub(crate) async fn effective_odoo_scan_targets(&self) -> (Vec<OdooScanTarget>, Vec<Value>) {
        let global = match tokio::fs::read_to_string(self.home_dir.join("config.toml")).await
            .ok().and_then(|s| s.parse::<toml::Table>().ok()) {
            Some(table) => table,
            None => return (Vec::new(), vec![scan_failure("default", "Odoo not configured or configuration unavailable")]),
        };
        let ids: Vec<String> = self.registry.read().await.list().iter()
            .map(|a| a.config.agent.name.clone()).collect();
        let mut targets = Vec::new();
        let mut failures = Vec::new();
        for id in std::iter::once("default".to_string()).chain(ids) {
            let agent = if id == "default" { Ok(toml::Table::new()) }
                else { self.read_agent_toml_table(&id).await };
            let result: Result<OdooScanTarget, String> = async {
                let agent = agent?;
                let mut cfg = duduclaw_odoo::OdooConfig::from_toml(&global);
                let over = duduclaw_odoo::AgentOdooConfig::from_table(&agent)?;
                if let Some(over) = &over { over.apply_to(&mut cfg)?; }
                if !cfg.is_configured() { return Err("Odoo not configured — save the connection settings first".into()); }
                // Resolve the effective encrypted value or secret reference in
                // the same way as the runtime pool. A broken override denies.
                let (enc_key, plain_key, encrypted) = if cfg.auth_method == "password" {
                    ("password_enc", "password", &cfg.password_enc)
                } else { ("api_key_enc", "api_key", &cfg.api_key_enc) };
                let plain = |table: &toml::Table| table.get("odoo").and_then(|v| v.get(plain_key))
                    .and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_owned);
                let agent_has_credential = agent.get("odoo").and_then(|v| v.as_table())
                    .is_some_and(|s| s.contains_key(enc_key) || s.contains_key(plain_key));
                let credential = if !encrypted.is_empty() {
                    use duduclaw_security::secret_ref::{Secret, SecretRef};
                    let config = duduclaw_security::secret_manager::SecretManagerConfig::load_from_home(&self.home_dir).await;
                    SecretRef::from_single(encrypted).resolve(&config, &self.home_dir).await
                        .map(Secret::expose_owned)
                } else if agent_has_credential { plain(&agent) } else { plain(&global) }
                    .filter(|s| !s.is_empty()).ok_or("Odoo credential unavailable")?;
                Ok(OdooScanTarget {
                    employee: id.clone(), cfg, credential,
                    profile: over.as_ref().map(|o| o.profile_or_default()).unwrap_or("default").to_string(),
                    company_ids: over.as_ref().map(|o| o.company_ids.iter().map(|&id| id as i64).collect()).unwrap_or_default(),
                })
            }.await;
            match result {
                Ok(target) => targets.push(target),
                Err(e) => failures.push(scan_failure(&id, &Self::scrub_odoo_error(&e))),
            }
        }
        (targets, failures)
    }

    async fn scan_effective_odoo_connections(&self, automatic: bool) -> Value {
        let (targets, mut results) = self.effective_odoo_scan_targets().await;
        // The semaphore is shared by manual and periodic jobs in this process.
        // Connect in bounded batches, then deduplicate authenticated identities.
        // Each group also carries the visibility policy its scan is filtered
        // with (Daybreak (a)): blocked / switched-off models are never probed.
        let mut groups: std::collections::BTreeMap<String, (duduclaw_odoo::OdooConnector, duduclaw_odoo::schema::ScanPolicy, Vec<(String, String)>)> = Default::default();
        for chunk in targets.chunks(2) {
            let connect = async |target: Option<&OdooScanTarget>| {
                let target = target?;
                let _permit = duduclaw_odoo::schema::scan_semaphore().acquire().await.ok()?;
                let result = tokio::time::timeout(std::time::Duration::from_secs(60),
                    duduclaw_odoo::OdooConnector::connect(&target.cfg, &target.credential)).await
                    .map_err(|_| "Odoo connection timed out".to_string())
                    .and_then(|r| r).map(|conn| conn.with_company_ids(target.company_ids.clone()));
                let policy = duduclaw_odoo::schema::ScanPolicy::from_config(&target.cfg);
                Some((target.employee.clone(), target.profile.clone(), policy, result))
            };
            let (a, b) = tokio::join!(connect(chunk.first()), connect(chunk.get(1)));
            for (employee, label, policy, result) in [a, b].into_iter().flatten() {
                match result {
                    Ok(conn) => {
                        let identity = conn.connection_identity();
                        let profile = conn.schema_profile(&label);
                        match groups.entry(identity) {
                            std::collections::btree_map::Entry::Vacant(slot) => {
                                slot.insert((conn, policy, vec![(employee.clone(), profile)]));
                            }
                            std::collections::btree_map::Entry::Occupied(mut slot) => {
                                // Members of one identity share a snapshot: keep the
                                // strictest policy so no member widens another's view.
                                let group = slot.get_mut();
                                group.1 = group.1.intersect(&policy);
                                group.2.push((employee.clone(), profile));
                            }
                        }
                    }
                    Err(e) => results.push(scan_failure(&employee, &Self::scrub_odoo_error(&e))),
                }
            }
        }
        let effective: std::collections::BTreeSet<String> = groups.values()
            .flat_map(|(conn, _, users)| std::iter::once(conn.schema_profile("default"))
                .chain(users.iter().map(|(_, p)| p.clone()))).collect();
        // If any identity is unknowable, keep old snapshots rather than deleting
        // a still-effective employee's metadata after a transient login failure.
        let can_cleanup = results.is_empty();
        let groups: Vec<_> = groups.into_values().collect();
        let mut saved = false;
        for chunk in groups.chunks(2) {
            let scan = async |group: Option<&(duduclaw_odoo::OdooConnector, duduclaw_odoo::schema::ScanPolicy, Vec<(String, String)>)>| {
                let (conn, policy, users) = group?;
                let _permit = duduclaw_odoo::schema::scan_semaphore().acquire().await.ok()?;
                let profile_key = conn.schema_profile("default");
                let profile = &profile_key;
                let outcome = if automatic {
                    duduclaw_odoo::schema::auto_scan(conn, &self.home_dir, profile, policy).await
                } else {
                    duduclaw_odoo::schema::scan_and_save(conn, &self.home_dir, profile, policy).await.map(|_| true)
                };
                let did_save = outcome.as_ref().is_ok_and(|saved| *saved);
                let mut rows = Vec::new();
                for (employee, alias) in users {
                    let result = outcome.clone().and_then(|_| {
                        if alias != profile {
                            let _lock = duduclaw_odoo::schema::acquire_scan_lock(&self.home_dir, alias)?;
                            let snapshot = duduclaw_odoo::schema::load_snapshot(&self.home_dir, profile)?
                                .ok_or("Odoo schema snapshot unavailable")?;
                            duduclaw_odoo::schema::save_snapshot(&self.home_dir, alias, &snapshot)?;
                        }
                        Ok(())
                    });
                    match result {
                        Ok(()) => rows.push(json!({"employee":employee,"status":"ok",
                            "schema":duduclaw_odoo::schema::load_scan_status(&self.home_dir, alias)})),
                        Err(e) => rows.push(scan_failure(employee, &Self::scrub_odoo_error(&e))),
                    }
                }
                Some((rows, did_save))
            };
            let (a, b) = tokio::join!(scan(chunk.first()), scan(chunk.get(1)));
            for (rows, did_save) in [a, b].into_iter().flatten() {
                saved |= did_save;
                results.extend(rows);
            }
        }
        if saved && can_cleanup {
            if let Err(e) = duduclaw_odoo::schema::cleanup_stale_profiles(&self.home_dir, &effective) {
                warn!(error=%e, "Odoo stale schema cleanup failed");
            }
        }
        let success = !results.is_empty() && results.iter().all(|r| r["status"] == "ok");
        let default = results.iter().find(|r| r["employee"] == "default");
        json!({"success":success,
            "message": if success { Value::Null } else {
                results.iter().find_map(|r| r.get("message").cloned()).unwrap_or(json!("Odoo schema scan failed"))
            },
            "schema":default.and_then(|r| r.get("schema")),
            "diff":default.and_then(|r| r.get("schema")).and_then(|s| s.get("diff")),
            "results":results})
    }

    /// Test the Odoo connection.
    ///
    /// Two modes:
    /// - **Inline (recommended for the dashboard "Test connection" button):** when
    ///   `params.url` is non-empty, build a transient config from `params`
    ///   (url / db / protocol / auth_method / username + api_key|password).
    ///   The config is **never written to disk** — this lets the user verify
    ///   credentials before persisting them.
    /// - **Stored:** when no `url` in params, fall back to `config.toml`
    ///   (original behaviour).
    ///
    /// Hybrid: in inline mode without an explicit credential, the saved
    /// credential is used so you can re-test after a small URL tweak without
    /// retyping the API key.
    pub(crate) async fn handle_odoo_test(&self, params: Value) -> WsFrame {
        let inline_url = params
            .get("url")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());

        // Always read config.toml once — needed for stored-credential fallback in inline mode.
        let config_path = self.home_dir.join("config.toml");
        let table = self.read_config_table(&config_path).await;

        let (odoo_cfg, credential) = if inline_url.is_some() {
            match Self::build_test_config_from_params(
                &params,
                super::odoo_config::read_allow_private_network(&self.home_dir),
            ) {
                Ok((cfg, Some(cred))) => (cfg, cred),
                Ok((cfg, None)) => {
                    // No credential in params — fall back to saved one so the user
                    // can re-test a tweaked URL without retyping the API key.
                    match self.resolve_odoo_credential(&table) {
                        Some(c) if !c.is_empty() => (cfg, c),
                        _ => {
                            return WsFrame::ok_response(
                                "",
                                json!({
                                    "success": false,
                                    "message": "No credential — enter API key/password, or save once first",
                                }),
                            );
                        }
                    }
                }
                Err(msg) => {
                    return WsFrame::ok_response(
                        "",
                        json!({
                            "success": false,
                            "message": msg,
                        }),
                    );
                }
            }
        } else {
            let cfg = duduclaw_odoo::OdooConfig::from_toml(&table);
            if !cfg.is_configured() {
                return WsFrame::ok_response(
                    "",
                    json!({
                        "success": false,
                        "message": "Odoo not configured — fill URL and database, or save first",
                    }),
                );
            }
            let credential = match self.resolve_odoo_credential(&table) {
                Some(c) if !c.is_empty() => c,
                _ => {
                    return WsFrame::ok_response(
                        "",
                        json!({
                            "success": false,
                            "message": "No API key or password configured",
                        }),
                    );
                }
            };
            (cfg, credential)
        };

        match duduclaw_odoo::OdooConnector::connect(&odoo_cfg, &credential).await {
            Ok(conn) => {
                let st = conn.status();
                WsFrame::ok_response(
                    "",
                    json!({
                        "success": true,
                        "message": format!("Connected — {} {}", st.edition, st.version),
                    }),
                )
            }
            Err(e) => {
                warn!("Odoo test connection failed");
                WsFrame::ok_response(
                    "",
                    json!({
                        "success": false,
                        "message": format!("Connection failed: {}", Self::scrub_odoo_error(&e)),
                    }),
                )
            }
        }
    }

    /// `odoo.discover_schema` — connect with the stored config, introspect the
    /// model/field schema, write a summary to the shared wiki, and return a
    /// lightweight model list to the dashboard. Metadata only — no business
    /// rows are read or returned. Honours the `.scope.toml` policy for the
    /// `odoo` namespace: a locked namespace skips the wiki write (fail-safe)
    /// but still returns the discovered models.
    pub(crate) async fn handle_odoo_discover_schema(&self, params: Value) -> WsFrame {
        let max_models = params
            .get("max_models")
            .and_then(|v| v.as_u64())
            .map(|n| (n as usize).clamp(10, 1000))
            .unwrap_or(300);

        // Build connector from the stored global config (same path as status).
        let config_path = self.home_dir.join("config.toml");
        let table = self.read_config_table(&config_path).await;
        let odoo_cfg = duduclaw_odoo::OdooConfig::from_toml(&table);
        if !odoo_cfg.is_configured() {
            return WsFrame::ok_response(
                "",
                json!({
                    "success": false,
                    "message": "Odoo not configured — fill URL and database, then save first",
                }),
            );
        }
        let credential = match self.resolve_odoo_credential(&table) {
            Some(c) if !c.is_empty() => c,
            _ => {
                return WsFrame::ok_response(
                    "",
                    json!({
                        "success": false,
                        "message": "No API key or password configured",
                    }),
                );
            }
        };

        // Both dashboard endpoints reserve the same persisted slot and hold
        // its lock through connection, metadata reads and wiki persistence.
        let _manual_scan = match duduclaw_odoo::schema::claim_manual_scan(
            &self.home_dir, "default", chrono::Utc::now().timestamp(),
        ) {
            Ok(lock) => lock,
            Err(e) => return WsFrame::ok_response("", json!({ "success": false, "message": e })),
        };

        let conn = match duduclaw_odoo::OdooConnector::connect(&odoo_cfg, &credential).await {
            Ok(c) => c,
            Err(e) => {
                warn!("Odoo discover_schema connect failed");
                return WsFrame::ok_response(
                    "",
                    json!({
                        "success": false,
                        "message": format!("Connection failed: {}", Self::scrub_odoo_error(&e)),
                    }),
                );
            }
        };

        // Also exclude automatic snapshot scans for the same connection.
        let _scan = match duduclaw_odoo::schema::acquire_scan_lock(
            &self.home_dir, &conn.schema_profile("default"),
        ) {
            Ok(lock) => lock,
            Err(e) => return WsFrame::ok_response("", json!({ "success": false, "message": e })),
        };

        // Same visibility rules as the snapshot scan: blocked / switched-off
        // models are never queried, returned or written to the wiki.
        let policy = conn.effective_scan_policy(&duduclaw_odoo::schema::ScanPolicy::from_config(&odoo_cfg));
        let mut report = match conn.introspect_schema(max_models, &policy).await {
            Ok(r) => r,
            Err(e) => {
                warn!("Odoo introspect_schema failed");
                return WsFrame::ok_response(
                    "",
                    json!({
                        "success": false,
                        "message": format!("Schema scan failed: {}", Self::scrub_odoo_error(&e)),
                    }),
                );
            }
        };

        // Defence in depth: filter again at the dashboard boundary.
        policy.retain_report(&mut report);

        // Lightweight model list for the UI (no field payload — that lives in
        // the wiki / odoo_schema_fields).
        let models: Vec<Value> = report
            .models
            .iter()
            .map(|m| {
                json!({
                    "model": m.model,
                    "name": m.name,
                    "custom": m.custom,
                    "field_count": m.field_count,
                })
            })
            .collect();

        // Write schema to the shared wiki, honouring the .scope.toml policy.
        let (wiki_written, wiki_note) = self.write_odoo_schema_wiki(&report, &policy).await;

        WsFrame::ok_response(
            "",
            json!({
                "success": true,
                "models": models,
                "invalid_models_dropped": report.invalid_models_dropped,
                "invalid_fields_dropped": report.invalid_fields_dropped,
                "total_models": report.total_models,
                "truncated": report.truncated,
                "wiki_written": wiki_written,
                "wiki_note": wiki_note,
            }),
        )
    }

    /// Persist an introspected schema to the shared wiki as two pages, both
    /// in the `deep` (search/reference-only) layer so nothing is auto-injected
    /// into an employee's context: `odoo/schema` (compact summary) and
    /// `odoo/schema-fields` (full field tables). Both renderers re-apply
    /// `policy`, so blocked / switched-off models never reach the wiki.
    /// Returns `(written, note)`. Skips the write (fail-safe) when the `odoo`
    /// namespace is locked by `.scope.toml`.
    pub(crate) async fn write_odoo_schema_wiki(
        &self, report: &duduclaw_odoo::SchemaReport, policy: &duduclaw_odoo::schema::ScanPolicy,
    ) -> (bool, String) {
        // .scope.toml policy for the `odoo` namespace.
        let scope_path = self
            .home_dir
            .join("shared")
            .join("wiki")
            .join(".scope.toml");
        let scope_table = self.read_config_table(&scope_path).await;
        if let Some(mode) = scp_namespace_mode(&scope_table, "odoo") {
            if mode != "agent_writable" {
                return (
                    false,
                    format!("wiki 'odoo' namespace is '{mode}' — schema not written"),
                );
            }
        }

        let store = duduclaw_memory::WikiStore::new_shared(&self.home_dir);
        if let Err(e) = store.ensure_scaffold() {
            return (false, format!("wiki scaffold failed: {e}"));
        }

        // Both pages are reference-only; never auto-inject Odoo metadata into
        // an employee's context.
        let summary = render_odoo_schema_summary(report, policy);
        let details = render_odoo_schema_details(report, policy);

        if let Err(e) = store.write_page("odoo/schema.md", &summary) {
            return (false, format!("wiki write failed: {e}"));
        }
        if let Err(e) = store.write_page("odoo/schema-fields.md", &details) {
            // Summary landed; report the partial state honestly.
            return (true, format!("summary written; field detail failed: {e}"));
        }
        (
            true,
            "schema pages written; odoo/schema and odoo/schema-fields are deep/reference-only".into(),
        )
    }

    /// Build a transient `OdooConfig` + credential from RPC params for the
    /// "test before save" flow. Applies the same validation rules as
    /// `handle_odoo_configure` so the test path can't be used to bypass them.
    ///
    /// Returns `(config, Some(credential))` when params include an api_key /
    /// password, or `(config, None)` when the caller wants the handler to fall
    /// back to the stored credential.
    pub(crate) fn build_test_config_from_params(
        params: &Value,
        stored_allow_private: bool,
    ) -> Result<(duduclaw_odoo::OdooConfig, Option<String>), String> {
        // The form's switch value wins (test-before-save); otherwise the stored one.
        let allow_private = params
            .get("allow_private_network")
            .and_then(|v| v.as_bool())
            .unwrap_or(stored_allow_private);
        // URL — reuse the same SSRF-safe validator as `configure`.
        let url = match params.get("url").and_then(|v| v.as_str()).map(str::trim) {
            Some(u) => {
                duduclaw_odoo::url_guard::check_url(u, allow_private)?;
                u
            }
            _ => return Err("Missing 'url' parameter".into()),
        };

        // Database — alphanumeric + `_` + `-`, max 63 chars.
        let db = match params.get("db").and_then(|v| v.as_str()).map(str::trim) {
            Some(d)
                if !d.is_empty()
                    && d.len() < 64
                    && d.chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') =>
            {
                d
            }
            Some(_) => {
                return Err("Invalid database name (alphanumeric, _, - only, max 63 chars)".into());
            }
            _ => return Err("Missing 'db' parameter".into()),
        };

        // v1.68.0: JSON-RPC only, same rule as `odoo.configure`.
        let protocol = match params.get("protocol").and_then(|v| v.as_str()) {
            Some("jsonrpc") | None => "jsonrpc",
            Some("xmlrpc") => {
                return Err("XML-RPC is not supported: DuDuClaw connects to Odoo over JSON-RPC only".into());
            }
            _ => return Err("Invalid protocol: must be 'jsonrpc'".into()),
        };

        let auth_method = match params.get("auth_method").and_then(|v| v.as_str()) {
            Some("password") => "password",
            Some("api_key") | None => "api_key",
            _ => return Err("Invalid auth_method: must be 'api_key' or 'password'".into()),
        };

        let username = params
            .get("username")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if username.len() > 256 {
            return Err("Username too long (max 256 chars)".into());
        }

        let credential_field = if auth_method == "password" {
            "password"
        } else {
            "api_key"
        };
        let credential = params
            .get(credential_field)
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .filter(|s| !s.is_empty());

        let cfg = duduclaw_odoo::OdooConfig {
            url: url.into(),
            db: db.into(),
            protocol: protocol.into(),
            auth_method: auth_method.into(),
            username: username.into(),
            allow_private_network: allow_private,
            ..Default::default()
        };
        Ok((cfg, credential))
    }

    /// Strip identifiers that an attacker could weaponize from a connector
    /// error before forwarding it to the dashboard.
    ///
    /// The reqwest error includes the full URL (with any query string), and may
    /// echo embedded userinfo (`user:pass@host`) or `?api_key=…` query params on
    /// failure. We redact those first, then cap the length. M19: truncation
    /// alone leaked the URL/token on short errors, so scrubbing must happen
    /// regardless of length. We keep the high-level reason so the user can act.
    pub(crate) fn scrub_odoo_error(raw: &str) -> String {
        let scrubbed = scrub_secrets_from_text(raw);
        // Cap to avoid pushing megabyte HTML error pages back to the client.
        const MAX_LEN: usize = 240;
        let mut out = String::with_capacity(scrubbed.len().min(MAX_LEN));
        for ch in scrubbed.chars().take(MAX_LEN) {
            out.push(ch);
        }
        if scrubbed.chars().count() > MAX_LEN {
            out.push_str("…");
        }
        out
    }

    /// Resolve the Odoo credential from config.toml (encrypted or plaintext).
    ///
    /// Returns `None` if decryption fails — never returns raw ciphertext (CRIT-2).
    pub(crate) fn resolve_odoo_credential(&self, table: &toml::Table) -> Option<String> {
        let odoo_section = table.get("odoo")?.as_table()?;
        let auth_method = odoo_section
            .get("auth_method")
            .and_then(|v| v.as_str())
            .unwrap_or("api_key");

        let (enc_field, plain_field) = if auth_method == "password" {
            ("password_enc", "password")
        } else {
            ("api_key_enc", "api_key")
        };

        // Try encrypted first
        if let Some(enc_val) = odoo_section
            .get(enc_field)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            if let Some(key) = crate::config_crypto::load_keyfile_public(&self.home_dir) {
                if let Ok(engine) = duduclaw_security::crypto::CryptoEngine::new(&key) {
                    if let Ok(decrypted) = engine.decrypt_string(enc_val) {
                        return Some(decrypted);
                    }
                }
            }
            // Decryption failed — do NOT return raw ciphertext as credential
            warn!("Failed to decrypt Odoo credential — keyfile may have changed");
            return None;
        }

        // Fallback to plaintext field (legacy / dev environments)
        let plain = odoo_section
            .get(plain_field)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        if plain.is_some() {
            warn!(
                field = plain_field,
                "Odoo credential stored in plaintext — re-save config to encrypt"
            );
        }
        plain
    }
}

/// Never serialize this target: it contains a resolved credential.
pub(crate) struct OdooScanTarget {
    pub(crate) employee: String,
    pub(crate) cfg: duduclaw_odoo::OdooConfig,
    credential: String,
    profile: String,
    company_ids: Vec<i64>,
}
fn scan_failure(employee: &str, message: &str) -> Value {
    json!({"employee":employee,"status":"failed","message":message})
}
