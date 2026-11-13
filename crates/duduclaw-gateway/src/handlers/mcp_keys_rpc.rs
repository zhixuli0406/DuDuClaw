//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.
//!
//! RM-134 (v1.82): the `mcp_keys.*` methods manage the hashed key store
//! (`duduclaw_auth::mcp_keys`) by key id. The plaintext key appears in
//! exactly one response — the one to `mcp_keys.create` — and is never
//! stored, logged or audited. Every create / rename / expiry change /
//! disable / enable / delete / hint dismissal writes a security audit
//! record; a refused (non-Admin) call is audited too. A loosening change
//! whose audit record cannot be written is put back — only the columns it
//! changed, and only while the key is still at the version it produced; if
//! the key moved on (or the compensation fails) the answer says the change
//! stayed applied without an audit record, and the gateway logs an error. A
//! tightening one (disable, delete) stays.
//!
//! H3: a key issued by client id is always EXTERNAL; its client id may not be
//! an AI employee id or a reserved system name, and it carries only the
//! externally grantable scopes (RM-157). The one internal key this RPC issues
//! is an AI employee's own per-agent key — the documented RFC-21 per-agent
//! keys and the v1.76 dedicated Google key — requested explicitly with
//! `agent_id` (never `admin`), behind the Personal gate
//! (`personal_data_gate::write_scoped`: owner, or an Admin with a fresh,
//! audited step-up).

#[allow(unused_imports)]
use super::*;

use std::collections::{HashMap, HashSet};

use duduclaw_auth::mcp_keys::{
    Expiry, KeyBinding, KeyChange, McpKeyError, McpKeyStore, NewMcpKey, Revert, first_ungrantable,
};
use duduclaw_core::mcp_key_store::{self as ks, KeyState, StoredKey};
use duduclaw_security::audit::Severity;

use crate::mcp_keys_lifecycle::{audit, employee_dir_ids, key_summary, legacy_config_report};

/// Shown when a change stayed applied but has no audit record.
const APPLIED_UNAUDITED: &str = "變更已套用，但安全稽核紀錄無法寫入，而且無法安全復原（金鑰已被其他操作變更，或復原失敗）。請檢查閘道紀錄。 / \
    The change stayed applied, but its security audit record could not be written and it could not be safely undone \
    (the key was changed again meanwhile, or the undo failed). Check the gateway log.";

impl MethodHandler {
    pub(crate) async fn handle_mcp_oauth_grants_list(&self) -> WsFrame {
        let home = self.home_dir.clone();
        let result = tokio::task::spawn_blocking(move || {
            duduclaw_core::mcp_oauth_store::load_store(&home)?.active_grants(&home)
        }).await;
        match result {
            Ok(Ok(grants)) => WsFrame::ok_response("", json!({ "grants": grants })),
            _ => WsFrame::error_response("", "OAuth grants or key configuration unavailable"),
        }
    }

    pub(crate) async fn audit_mcp_oauth_revoke(
        &self, ctx: &UserContext, id: Option<&str>, outcome: &str,
        grant: Option<&duduclaw_core::mcp_oauth_store::Grant>,
    ) -> bool {
        let event = duduclaw_security::audit::AuditEvent::new(
            "mcp_oauth_grant_revoke", "dashboard", duduclaw_security::audit::Severity::Info,
            json!({
                "actor": ctx.user_id, "family_id": id, "outcome": outcome,
                "client_name": grant.map(|g| g.client_name.as_str()),
                "scopes": grant.map(|g| g.scope.split_whitespace().collect::<Vec<_>>()),
            }),
        );
        let home = self.home_dir.clone();
        matches!(tokio::task::spawn_blocking(move || {
            duduclaw_security::audit::try_append_audit_event(&home, &event)
        }).await, Ok(Ok(())))
    }

    pub(crate) async fn handle_mcp_oauth_grants_revoke(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let id = params.get("id").and_then(Value::as_str)
            .and_then(|id| uuid::Uuid::parse_str(id).ok()).map(|id| id.to_string());
        let Some(id) = id else {
            if !self.audit_mcp_oauth_revoke(ctx, None, "invalid_request", None).await {
                return WsFrame::error_response("", "OAuth revocation refused; audit unavailable");
            }
            return WsFrame::error_response("", "Valid grant id required");
        };
        let home = self.home_dir.clone();
        let target = id.clone();
        let result = tokio::task::spawn_blocking(move || {
            duduclaw_core::mcp_oauth_store::update_store(&home, |store| {
                let grant = store.grants.get(&target).cloned();
                let revoked = store.revoke_family(&target);
                (revoked, grant)
            })
        }).await;
        match result {
            Ok(Ok((revoked, grant))) => {
                let outcome = if revoked { "revoked" } else { "not_found" };
                if !self.audit_mcp_oauth_revoke(ctx, Some(&id), outcome, grant.as_ref()).await {
                    return WsFrame::error_response("", "OAuth revocation processed; audit unavailable");
                }
                WsFrame::ok_response("", json!({ "success": true, "revoked": revoked }))
            }
            _ => {
                self.audit_mcp_oauth_revoke(ctx, Some(&id), "store_unavailable", None).await;
                WsFrame::error_response("", "OAuth grant store unavailable")
            }
        }
    }

    // ── MK: dashboard MCP API keys (hashed store, RM-134) ─────────────────────

    /// The whole early-routed `mcp_keys.*` branch of `MethodHandler::dispatch`:
    /// the Admin gate, the refusal audit and the handler. `dispatch` awaits it
    /// as ONE boxed frame (`boxed_frame`), so none of these futures is
    /// embedded in the shared dispatcher future
    /// (`team_history_rpc_future_sizes` keeps that under 8 KiB).
    pub(crate) async fn route_mcp_keys_rpc(&self, method: &str, params: Value, ctx: &UserContext) -> WsFrame {
        if let Err(e) = acl::require_role(ctx, UserRole::Admin) {
            if method != "mcp_keys.list" {
                self.audit_mcp_keys_refused(method, &params, ctx).await;
            }
            return WsFrame::error_response("", &e);
        }
        self.handle_mcp_keys_rpc(method, params, ctx).await
    }

    /// Dispatch for every `mcp_keys.*` method. The caller has already
    /// required Admin; `mcp_keys.create` with `agent_id` has already passed
    /// the Personal gate.
    pub(crate) async fn handle_mcp_keys_rpc(&self, method: &str, params: Value, ctx: &UserContext) -> WsFrame {
        match method {
            "mcp_keys.list" => self.handle_mcp_keys_list().await,
            "mcp_keys.create" => self.handle_mcp_keys_create(params, ctx).await,
            "mcp_keys.rename" => self.handle_mcp_keys_rename(params, ctx).await,
            "mcp_keys.set_expiry" => self.handle_mcp_keys_set_expiry(params, ctx).await,
            "mcp_keys.disable" => self.handle_mcp_keys_set_disabled(params, ctx, true).await,
            "mcp_keys.enable" => self.handle_mcp_keys_set_disabled(params, ctx, false).await,
            // `mcp_keys.revoke` (v1.81 and earlier took the full plaintext)
            // is kept as an alias of delete that accepts only a key id.
            "mcp_keys.delete" | "mcp_keys.revoke" => self.handle_mcp_keys_delete(params, ctx).await,
            "mcp_keys.dismiss_migration_hint" => self.handle_mcp_keys_dismiss_hint(ctx).await,
            _ => coded("mcp_key_unknown_method", "Unknown mcp_keys method"),
        }
    }

    /// Audit a refused (non-Admin) call. Best effort: the refusal stands
    /// whether or not the record could be written.
    pub(crate) async fn audit_mcp_keys_refused(&self, method: &str, params: &Value, ctx: &UserContext) {
        let details = json!({
            "actor": ctx.user_id, "role": format!("{:?}", ctx.role), "method": method,
            "key_id": key_id_param(params), "outcome": "denied",
        });
        let home = self.home_dir.clone();
        let _ = tokio::task::spawn_blocking(move || audit(&home, "mcp_key_refused", Severity::Warning, details)).await;
    }

    async fn write_audit(&self, event: &'static str, severity: Severity, details: Value) -> bool {
        let home = self.home_dir.clone();
        matches!(
            tokio::task::spawn_blocking(move || audit(&home, event, severity, details)).await,
            Ok(Ok(()))
        )
    }

    /// `user id → display label` for the creator / disabler columns.
    async fn user_labels(&self) -> HashMap<String, String> {
        let mut labels = HashMap::new();
        let Some(db) = self.user_db.read().await.clone() else { return labels };
        if let Ok(Ok(users)) = tokio::task::spawn_blocking(move || db.list_users()).await {
            for user in users {
                let label = if user.display_name.trim().is_empty() { user.email.clone() } else { user.display_name.clone() };
                labels.insert(user.id.clone(), label);
            }
        }
        labels
    }

    /// The facts a key row is rendered with.
    async fn row_context(&self) -> RowContext {
        let home = self.home_dir.clone();
        let (usage, employees, bindings) = tokio::task::spawn_blocking(move || {
            let usage = ks::usage_map(&home).unwrap_or_else(|error| {
                warn!(%error, "MCP key usage file unreadable; last use not shown");
                HashMap::new()
            });
            // Third review: the state of every existing employee directory,
            // for the "employee no longer exists" marker on bound keys —
            // and (fourth review) its birth time, for "employee re-created".
            let bindings: HashMap<String, (ks::BindingState, Option<chrono::DateTime<Utc>>)> = employee_dir_ids(&home)
                .unwrap_or_default()
                .into_iter()
                .map(|id| {
                    let state = ks::binding_state(&home, &id);
                    let born = ks::employee_dir_birth(&home, &id);
                    (id, (state, born))
                })
                .collect();
            (usage, employee_names(&home).unwrap_or_default(), bindings)
        })
        .await
        .unwrap_or_default();
        RowContext { labels: self.user_labels().await, usage, employees, bindings }
    }

    /// `mcp_keys.list` — every key that is not deleted. Never returns a key
    /// or its digest. Response: `{ keys: [..], migration_hint,
    /// legacy_pending, ignored_legacy_entries: [hint], effective_scopes,
    /// employee_key_scopes, expiry_presets_days }`. `last_used_at` comes from
    /// the usage file, read now (display only).
    pub(crate) async fn handle_mcp_keys_list(&self) -> WsFrame {
        let home = self.home_dir.clone();
        let config_home = home.clone();
        let result = store_call(&home, move |store| {
            let report = legacy_config_report(&config_home, store)
                .unwrap_or_else(|error| {
                    warn!(%error, "MCP key list: config.toml [mcp_keys] could not be classified");
                    Default::default()
                });
            Ok((store.list()?, store.migration_hint_pending()?, report))
        })
        .await;
        let (keys, migration_hint, report) = match result {
            Ok(v) => v,
            Err(e) => return key_error(e),
        };
        let context = self.row_context().await;
        let now = Utc::now();
        let rows: Vec<Value> = keys.iter().map(|k| key_json(k, now, &context)).collect();
        WsFrame::ok_response(
            "",
            json!({
                "keys": rows,
                "migration_hint": migration_hint,
                "legacy_pending": report.pending,
                "ignored_legacy_entries": report.ignored,
                "effective_scopes": duduclaw_core::mcp_scopes::EXTERNALLY_GRANTABLE_SCOPE_STRINGS,
                "employee_key_scopes": duduclaw_core::mcp_scopes::EMPLOYEE_KEY_SCOPE_STRINGS,
                "expiry_presets_days": duduclaw_auth::mcp_keys::EXPIRY_PRESET_DAYS,
            }),
        )
    }

    /// `mcp_keys.create` — params `{ name, client_id, env?, scopes[], expiry }`
    /// for an external integration key, or `{ name, agent_id, env?, scopes[],
    /// expiry }` for an AI employee's internal per-agent key. `expiry` is
    /// `{kind:"days",days:30|90|365}`, `{kind:"date",at:<RFC 3339>}` or
    /// `{kind:"never",confirm:true}`. `is_external`, when sent, must agree:
    /// `true` for a client-id key (an internal key by client id is refused),
    /// `false` for an `agent_id` key.
    /// Response: `{ success, key (plaintext, once), key_info, message }`.
    pub(crate) async fn handle_mcp_keys_create(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let name = params.get("name").and_then(Value::as_str).unwrap_or("").to_string();
        let client_param = params.get("client_id").and_then(Value::as_str).map(|s| s.trim().to_string());
        let agent_param = params.get("agent_id").and_then(Value::as_str).map(|s| s.trim().to_string());
        let wanted_external = agent_param.is_none();
        if params.get("is_external").is_some_and(|v| v.as_bool() != Some(wanted_external)) {
            return coded(
                "mcp_key_internal_not_allowed",
                "儀表板以用戶端代號只發出外部金鑰；AI 員工自己的金鑰請以 agent_id 建立。 / By client id the dashboard \
                 issues external keys only; an AI employee's own key is requested with agent_id.",
            );
        }
        let home = self.home_dir.clone();
        let (employees, directory_ids) = {
            let home = home.clone();
            match tokio::task::spawn_blocking(move || Ok::<_, String>((employee_names(&home)?, employee_dir_ids(&home)?))).await {
                Ok(Ok(found)) => found,
                _ => return key_error(McpKeyError::Store),
            }
        };
        let (binding, client_id) = match agent_param {
            Some(agent) => {
                // The per-agent key: its client id IS the employee's exact
                // directory id, stored as the key's immutable binding (the
                // Personal gate already ran on `agent_id`). Third review
                // (High): an `[agent] name` / display name or another case
                // of the id is refused with its own message — such a key
                // would name no directory and could never act as the
                // employee.
                if client_param.as_deref().is_some_and(|c| c != agent) {
                    return coded(McpKeyError::InvalidClientId.code(), EMPLOYEE_KEY_TARGET);
                }
                if !duduclaw_core::is_valid_agent_id(&agent) || !directory_ids.contains(&agent) {
                    let named = employees.iter().any(|name| name.eq_ignore_ascii_case(&agent))
                        || directory_ids.iter().any(|id| id.eq_ignore_ascii_case(&agent));
                    return if named {
                        coded("mcp_key_employee_id_required", EMPLOYEE_KEY_NEEDS_ID)
                    } else {
                        coded(McpKeyError::InvalidClientId.code(), EMPLOYEE_KEY_TARGET)
                    };
                }
                (KeyBinding::Employee, agent)
            }
            None => {
                let client_id = client_param.unwrap_or_default();
                // v1.81 P1e: never an ephemeral sub-agent id (`eph-…`), which
                // other gates resolve to its parent employee.
                if let Some(refusal) = external_client_id_refusal(true, &client_id) {
                    return coded(McpKeyError::InvalidClientId.code(), refusal);
                }
                if let Some(refusal) = integration_client_refusal(&client_id, &employees) {
                    return coded(McpKeyError::InvalidClientId.code(), refusal);
                }
                (KeyBinding::Integration, client_id)
            }
        };
        let env = match params.get("env").and_then(Value::as_str) {
            None => "prod".to_string(),
            Some(env) => env.to_string(),
        };
        let Some(scopes_raw) = params.get("scopes").and_then(Value::as_array) else {
            return key_error(McpKeyError::InvalidScopes);
        };
        let mut scopes = Vec::with_capacity(scopes_raw.len());
        for s in scopes_raw {
            match s.as_str() {
                Some(s) => scopes.push(s.to_string()),
                None => return key_error(McpKeyError::InvalidScopes),
            }
        }
        // RM-157: name the first scope this key may not carry.
        if let Some(scope) = first_ungrantable(binding, &scopes) {
            let known = duduclaw_core::mcp_scopes::MCP_SCOPE_STRINGS.contains(&scope.as_str());
            let error = if known { McpKeyError::ScopeNotGrantable } else { McpKeyError::InvalidScopes };
            return WsFrame::Response {
                id: String::new(),
                ok: false,
                payload: None,
                error: Some(json!({
                    "code": error.code(), "message": error.message(), "scope": scope,
                    "allowed_scopes": duduclaw_auth::mcp_keys::grantable_scopes(binding),
                })),
            };
        }
        let expiry = match parse_expiry(params.get("expiry")) {
            Ok(expiry) => expiry,
            Err(frame) => return frame,
        };
        let request = NewMcpKey {
            name, client_id, binding, env, scopes, expiry,
            created_by: Some(ctx.user_id.clone()),
        };
        let created = match store_call(&home, move |store| store.create(request, Utc::now())).await {
            Ok(created) => created,
            Err(e) => return key_error(e),
        };
        let mut details = key_summary(&created.key);
        details["actor"] = json!(ctx.user_id);
        details["employee_key"] = json!(binding == KeyBinding::Employee);
        let severity = no_expiry_details(&mut details, &created.key, ctx);
        if !self.write_audit("mcp_key_created", severity, details).await {
            // Never hand out a key whose issuance is not on record.
            let key = created.key.clone();
            if let Err(e) = store_call(&home, move |store| store.discard_created(&key, Utc::now())).await {
                tracing::error!(key_id = created.key.id.as_str(), error = e.code(),
                    "unaudited MCP key could not be withdrawn; its secret was never handed out");
            }
            return coded("mcp_key_audit_unavailable", "金鑰未發出：安全稽核紀錄無法寫入。 / Key not issued: the security audit record could not be written.");
        }
        info!(key_id = created.key.id.as_str(), "mcp_keys.create completed");
        let context = self.row_context().await;
        WsFrame::ok_response(
            "",
            json!({
                "success": true,
                // Returned exactly once — the gateway never stores or echoes it again.
                "key": created.secret,
                "key_info": key_json(&created.key, Utc::now(), &context),
                "message": "Store this key now — it cannot be retrieved again.",
            }),
        )
    }

    async fn handle_mcp_keys_rename(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let Some(id) = key_id_param(&params) else { return key_error(McpKeyError::NotFound) };
        let name = params.get("name").and_then(Value::as_str).unwrap_or("").to_string();
        let home = self.home_dir.clone();
        let change = match store_call(&home, move |store| store.rename(&id, &name)).await {
            Ok(v) => v,
            Err(e) => return key_error(e),
        };
        let mut details = key_summary(&change.after);
        details["actor"] = json!(ctx.user_id);
        details["from"] = json!(change.before.name);
        details["to"] = json!(change.after.name);
        if !self.write_audit("mcp_key_renamed", Severity::Info, details).await {
            return self.undo(change).await;
        }
        self.key_response(&change.after).await
    }

    async fn handle_mcp_keys_set_expiry(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let Some(id) = key_id_param(&params) else { return key_error(McpKeyError::NotFound) };
        let expiry = match parse_expiry(params.get("expiry")) {
            Ok(expiry) => expiry,
            Err(frame) => return frame,
        };
        let home = self.home_dir.clone();
        let change = match store_call(&home, move |store| store.set_expiry(&id, expiry, Utc::now())).await {
            Ok(v) => v,
            Err(e) => return key_error(e),
        };
        let mut details = key_summary(&change.after);
        details["actor"] = json!(ctx.user_id);
        details["from"] = json!(change.before.expires_at.map(ks::timestamp));
        details["to"] = json!(change.after.expires_at.map(ks::timestamp));
        let severity = no_expiry_details(&mut details, &change.after, ctx);
        if !self.write_audit("mcp_key_expiry_changed", severity, details).await {
            return self.undo(change).await;
        }
        self.key_response(&change.after).await
    }

    async fn handle_mcp_keys_set_disabled(&self, params: Value, ctx: &UserContext, disabled: bool) -> WsFrame {
        let Some(id) = key_id_param(&params) else { return key_error(McpKeyError::NotFound) };
        let home = self.home_dir.clone();
        let actor = ctx.user_id.clone();
        let change = match store_call(&home, move |store| store.set_disabled(&id, disabled, &actor, Utc::now())).await {
            Ok(v) => v,
            Err(e) => return key_error(e),
        };
        let mut details = key_summary(&change.after);
        details["actor"] = json!(ctx.user_id);
        details["was_disabled"] = json!(change.before.disabled_at.is_some());
        let (event, severity) = if disabled {
            ("mcp_key_disabled", Severity::Warning)
        } else {
            ("mcp_key_enabled", Severity::Info)
        };
        if !self.write_audit(event, severity, details).await {
            if disabled {
                // Tightening stays applied.
                return coded("mcp_key_audit_unavailable", "金鑰已停用，但安全稽核紀錄無法寫入。 / Key disabled, but the security audit record could not be written.");
            }
            return self.undo(change).await;
        }
        self.key_response(&change.after).await
    }

    async fn handle_mcp_keys_delete(&self, params: Value, ctx: &UserContext) -> WsFrame {
        if params.get("key").is_some() && key_id_param(&params).is_none() {
            return coded(
                "mcp_key_id_required",
                "請以金鑰 ID 刪除；不再接受貼上完整金鑰。 / Delete by key id; the full key is no longer accepted.",
            );
        }
        let Some(id) = key_id_param(&params) else { return key_error(McpKeyError::NotFound) };
        let home = self.home_dir.clone();
        let actor = ctx.user_id.clone();
        let change = match store_call(&home, move |store| store.delete(&id, &actor, Utc::now())).await {
            Ok(v) => v,
            Err(e) => return key_error(e),
        };
        let mut details = key_summary(&change.before);
        details["actor"] = json!(ctx.user_id);
        if !self.write_audit("mcp_key_deleted", Severity::Warning, details).await {
            return coded("mcp_key_audit_unavailable", "金鑰已刪除，但安全稽核紀錄無法寫入。 / Key deleted, but the security audit record could not be written.");
        }
        info!(key_id = change.before.id.as_str(), "mcp_keys.delete completed");
        WsFrame::ok_response("", json!({ "success": true, "deleted": change.before.id }))
    }

    /// L1: dismissing the hint is audited; an unaudited dismissal is put back.
    async fn handle_mcp_keys_dismiss_hint(&self, ctx: &UserContext) -> WsFrame {
        let home = self.home_dir.clone();
        let at = Utc::now();
        let previous = match store_call(&home, move |store| store.dismiss_migration_hint(at)).await {
            Ok(previous) => previous,
            Err(e) => return key_error(e),
        };
        let details = json!({ "actor": ctx.user_id, "previous": previous.as_deref() });
        if self.write_audit("mcp_key_migration_hint_dismissed", Severity::Info, details).await {
            return WsFrame::ok_response("", json!({ "success": true }));
        }
        match store_call(&home, move |store| store.restore_migration_hint(previous.as_deref(), at)).await {
            Ok(Revert::Reverted) => coded(
                "mcp_key_audit_unavailable",
                "變更未套用：安全稽核紀錄無法寫入。 / Change not applied: the security audit record could not be written.",
            ),
            _ => {
                tracing::error!("MCP key migration hint dismissal stayed applied without an audit record");
                coded("mcp_key_audit_unavailable_applied", APPLIED_UNAUDITED)
            }
        }
    }

    /// Put a loosening change back after its audit record failed — only the
    /// columns it changed, only while the key is still at the version it
    /// produced. Never claims an undo that did not happen.
    async fn undo(&self, change: KeyChange) -> WsFrame {
        let home = self.home_dir.clone();
        let key_id = change.after.id.clone();
        match store_call(&home, move |store| store.revert(&change)).await {
            Ok(Revert::Reverted) => coded(
                "mcp_key_audit_unavailable",
                "變更未套用：安全稽核紀錄無法寫入。 / Change not applied: the security audit record could not be written.",
            ),
            Ok(Revert::Superseded) => {
                tracing::error!(key_id = key_id.as_str(),
                    "MCP key change stayed applied WITHOUT an audit record: the key was changed again before it could be undone");
                coded("mcp_key_audit_unavailable_applied", APPLIED_UNAUDITED)
            }
            Err(e) => {
                tracing::error!(key_id = key_id.as_str(), error = e.code(),
                    "MCP key change could not be undone after an audit failure; it stays applied WITHOUT an audit record");
                coded("mcp_key_audit_unavailable_applied", APPLIED_UNAUDITED)
            }
        }
    }

    async fn key_response(&self, key: &StoredKey) -> WsFrame {
        let context = self.row_context().await;
        WsFrame::ok_response("", json!({ "success": true, "key_info": key_json(key, Utc::now(), &context) }))
    }
}

/// What a row is rendered with: user labels, the usage map (display only)
/// and the AI employee ids.
#[derive(Default)]
struct RowContext {
    labels: HashMap<String, String>,
    usage: HashMap<String, chrono::DateTime<Utc>>,
    employees: HashSet<String>,
    /// Existing employee directory id → its binding state (live / deleted /
    /// unreadable) and its directory birth time, where the platform reports
    /// one. An id not in the map has no directory.
    bindings: HashMap<String, (ks::BindingState, Option<chrono::DateTime<Utc>>)>,
}

/// The keys-page view of a key's employee binding: `None` for an unbound
/// key, else the binding's current state as the authenticators decide it
/// ([`ks::key_binding_state`]). A binding whose directory is gone reads
/// `employee_missing`; a live employee whose directory is younger than the
/// key reads `employee_recreated` (fourth review).
fn binding_status(key: &StoredKey, context: &RowContext) -> Option<ks::BindingState> {
    key.agent_binding.as_ref().map(|id| match context.bindings.get(id).copied() {
        None => ks::BindingState::Missing,
        Some((ks::BindingState::Live, born)) if ks::employee_born_after(born, key.created_at) => {
            ks::BindingState::Recreated
        }
        Some((state, _)) => state,
    })
}

/// Run one store operation off the async runtime.
async fn store_call<T: Send + 'static>(
    home: &Path,
    work: impl FnOnce(&McpKeyStore) -> Result<T, McpKeyError> + Send + 'static,
) -> Result<T, McpKeyError> {
    let home = home.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let store = McpKeyStore::open(&home).map_err(|e| {
            warn!(error = %e, "MCP key store unavailable");
            McpKeyError::Store
        })?;
        work(&store)
    })
    .await
    .unwrap_or(Err(McpKeyError::Store))
}

fn coded(code: &str, message: &str) -> WsFrame {
    WsFrame::Response {
        id: String::new(),
        ok: false,
        payload: None,
        error: Some(json!({ "code": code, "message": message })),
    }
}

fn key_error(e: McpKeyError) -> WsFrame {
    coded(e.code(), e.message())
}

/// Longest key id accepted from a request (`mk_` + 24 hex).
const KEY_ID_MAX: usize = 64;

fn key_id_param(params: &Value) -> Option<String> {
    params
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty() && id.len() <= KEY_ID_MAX && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
        .map(str::to_owned)
}

/// Parse the dashboard's expiry choice. "No expiry" needs `confirm: true`
/// (the dialog's risk notice); anything else is refused.
fn parse_expiry(raw: Option<&Value>) -> Result<Expiry, WsFrame> {
    let invalid = || key_error(McpKeyError::InvalidExpiry);
    let raw = raw.ok_or_else(invalid)?;
    match raw.get("kind").and_then(Value::as_str) {
        Some("days") => raw.get("days").and_then(Value::as_i64).map(Expiry::Days).ok_or_else(invalid),
        Some("date") => raw
            .get("at")
            .and_then(Value::as_str)
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| Expiry::At(dt.with_timezone(&Utc)))
            .ok_or_else(invalid),
        Some("never") if raw.get("confirm").and_then(Value::as_bool) == Some(true) => Ok(Expiry::Never),
        Some("never") => Err(coded(
            "mcp_key_no_expiry_unconfirmed",
            "選擇「不過期」必須先確認風險說明。 / Choosing no expiry requires confirming the risk notice.",
        )),
        _ => Err(invalid()),
    }
}

/// A key without expiry is recorded with who confirmed it and when, at
/// Warning severity.
fn no_expiry_details(details: &mut Value, key: &StoredKey, ctx: &UserContext) -> Severity {
    if key.expires_at.is_some() {
        details["no_expiry"] = json!(false);
        return Severity::Info;
    }
    details["no_expiry"] = json!(true);
    details["no_expiry_confirmed_by"] = json!(ctx.user_id);
    details["no_expiry_confirmed_at"] = json!(ks::timestamp(Utc::now()));
    Severity::Warning
}

/// Dashboard row. Never the digest.
fn key_json(key: &StoredKey, now: chrono::DateTime<Utc>, context: &RowContext) -> Value {
    let state = key.state(now);
    let days_left = key.expires_at.map(|at| (at - now).num_days());
    let expiring_soon = state == KeyState::Active
        && key.expires_at.is_some_and(|at| at - now <= chrono::Duration::days(ks::NOTICE_THRESHOLDS_DAYS[0]));
    let label = |id: &Option<String>| id.as_ref().and_then(|id| context.labels.get(id).cloned());
    let employee_key = context.employees.contains(&key.client_id);
    // Display only, from the usage file: a value outside the key's lifetime
    // (a tampered row) is not shown.
    let slack = chrono::Duration::seconds(ks::LAST_USED_THROTTLE_SECS);
    let last_used = context
        .usage
        .get(&key.id)
        .filter(|at| **at + slack >= key.created_at && **at <= now + slack)
        .map(|at| ks::timestamp(*at));
    let binding = binding_status(key, context);
    json!({
        "id": key.id,
        "name": key.name,
        "hint": key.hint,
        "client_id": key.client_id,
        "is_external": key.is_external,
        "employee_key": employee_key,
        // Third review: the employee this key is bound to and whether that
        // employee still exists. A bound key whose employee was removed,
        // renamed or deleted stops working over the HTTP MCP server; the
        // page shows it as "employee no longer exists".
        "agent_binding": key.agent_binding,
        "binding_status": binding.map(ks::BindingState::as_str),
        "employee_missing": matches!(
            binding,
            Some(
                ks::BindingState::Missing
                    | ks::BindingState::Deleted
                    | ks::BindingState::Invalid
                    | ks::BindingState::Recreated
            )
        ),
        // Fourth review (Medium 2): a migrated employee entry left unbound
        // because its scopes are not allowed for a per-employee key — shown
        // as "needs re-issue: scopes not allowed for a per-employee key".
        "needs_reissue": crate::mcp_keys_lifecycle::needs_reissue(key, context.bindings.contains_key(&key.client_id)),
        "scopes": key.scopes,
        "ineffective_scopes": key.ineffective_scopes(employee_key),
        "created_by": key.created_by,
        "created_by_name": label(&key.created_by),
        "created_at": ks::timestamp(key.created_at),
        "expires_at": key.expires_at.map(ks::timestamp),
        "days_left": days_left,
        "last_used_at": last_used,
        "disabled_at": key.disabled_at.map(ks::timestamp),
        "disabled_by": key.disabled_by,
        "disabled_by_name": label(&key.disabled_by),
        "status": state.as_str(),
        "expiring_soon": expiring_soon,
        "migrated": key.migrated,
        "expiry_reviewed": key.expiry_reviewed,
    })
}

/// Every AI employee's id (its `agents/<id>` directory with an
/// `agent.toml`) and `[agent] name`. An unreadable `agents/` directory is an
/// error (the caller refuses rather than guesses).
pub(crate) fn employee_names(home: &Path) -> Result<HashSet<String>, String> {
    let mut names = HashSet::new();
    let entries = match std::fs::read_dir(home.join("agents")) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(names),
        Err(e) => return Err(format!("agents directory unreadable: {e}")),
    };
    for entry in entries {
        let entry = entry.map_err(|e| format!("agents directory unreadable: {e}"))?;
        let Some(dir) = entry.file_name().to_str().map(str::to_owned) else { continue };
        if duduclaw_core::is_internal_agent_dir(&dir) || !duduclaw_core::is_valid_agent_id(&dir) {
            continue;
        }
        let config = entry.path().join("agent.toml");
        if !config.is_file() {
            continue;
        }
        if let Ok(meta) = std::fs::metadata(&config)
            && meta.len() <= 4 * 1024 * 1024
            && let Ok(text) = std::fs::read_to_string(&config)
            && let Ok(table) = text.parse::<toml::Table>()
            && let Some(name) = table.get("agent").and_then(|a| a.get("name")).and_then(toml::Value::as_str)
            && !name.trim().is_empty()
        {
            names.insert(name.trim().to_owned());
        }
        names.insert(dir);
    }
    Ok(names)
}

/// Shown when an integration key would carry an AI employee's id.
pub(crate) const CLIENT_IS_EMPLOYEE: &str = "用戶端代號不能是 AI 員工的代號或名稱；要給 AI 員工自己用的金鑰，請改選「AI 員工自己的金鑰」。 / \
    The client id may not be an AI employee's id or name. For an employee's own key, choose \"An AI employee's own key\".";
/// Shown when an integration key would carry a reserved system name.
pub(crate) const CLIENT_IS_RESERVED: &str = "用戶端代號不能是系統保留名稱（例如 dashboard、cron、default、gateway-internal）。 / \
    The client id may not be a reserved system name (e.g. dashboard, cron, default, gateway-internal).";
/// Shown when a per-agent key names no existing employee.
pub(crate) const EMPLOYEE_KEY_TARGET: &str = "找不到這位 AI 員工（或同時給了不同的用戶端代號）。 / No such AI employee \
    (or a different client id was given as well).";
/// Third review (High): shown when a per-agent key is requested by an
/// employee's `[agent] name` / display name or another spelling of its id.
pub(crate) const EMPLOYEE_KEY_NEEDS_ID: &str = "AI 員工自己的金鑰必須以員工代號（agents/ 下的資料夾名稱，大小寫須完全相同）建立，不能用顯示名稱。 / \
    An AI employee's own key must be created with the employee's id (its folder name under agents/, exact case), \
    not its display name.";

/// H3: an integration key's client id is never an AI employee (exact id or
/// `[agent] name`, ASCII case-insensitive) nor a reserved system sender name
/// — such a principal would be treated as that employee or system actor by
/// the gates downstream.
pub(crate) fn integration_client_refusal(client_id: &str, employees: &HashSet<String>) -> Option<&'static str> {
    let id = client_id.trim();
    if duduclaw_core::is_reserved_agent_id(id)
        || duduclaw_core::is_system_sender(id)
        || id.eq_ignore_ascii_case("system")
        || id.eq_ignore_ascii_case(duduclaw_core::UNTRUSTED_AGENT_ID)
        || id.eq_ignore_ascii_case(ks::RESERVED_INTERNAL_CLIENT_ID)
    {
        return Some(CLIENT_IS_RESERVED);
    }
    employees.iter().any(|name| name.eq_ignore_ascii_case(id)).then_some(CLIENT_IS_EMPLOYEE)
}

/// Shown when an external key would be issued under an ephemeral sub-agent id.
pub(crate) const EXTERNAL_EPHEMERAL_ID: &str = "外部 MCP 金鑰的用戶端代號不能以 eph- 開頭（那是臨時子代理的代號）。 / An \
    external MCP key's client id must not be an ephemeral sub-agent id (eph-…).";

/// v1.81 P1e: an external key may not carry an ephemeral sub-agent id: any
/// client id starting `eph-` (case-insensitive, the prefix of
/// `crate::ephemeral::is_ephemeral_id`) is refused. Every mode.
pub(crate) fn external_client_id_refusal(is_external: bool, client_id: &str) -> Option<&'static str> {
    let lowered = client_id.trim().to_ascii_lowercase();
    (is_external && lowered.starts_with("eph-")).then_some(EXTERNAL_EPHEMERAL_ID)
}
