//! RM-120: permission-controlled generic `odoo_create` / `odoo_write`.
//!
//! These two tools are allowed **only** when the employee's Odoo settings
//! explicitly grant them: a per-agent `agent.toml [odoo]` override must exist,
//! `allowed_models` must name the model exactly and `allowed_actions` must
//! hold the bare verb (`create` / `write`) or `verb:model`. This is stricter
//! than [`crate::odoo_pool::check_action_permission`], where a missing
//! override or an empty list is permissive; that function and the tools that
//! use it are unchanged.
//!
//! Gate order (all fail closed). Scope (`odoo:write`) and the effect class
//! (`modify`, action rules / approvals / explore lane) run in the dispatcher
//! before this handler is reached. Here:
//!
//! 1. arguments: model name, `ids`, `values` shape and size, x2many commands;
//! 2. explicit grant ([`generic_write_grant`]), also used by `tools/list`;
//! 3. 功能模組 switch and the security block list (`AccessKind::Mutate`);
//! 4. connection precheck — the first step that touches credentials;
//! 5. account ACL from the schema snapshot (`create` / `write` must be
//!    `Some(true)`);
//! 6. every key of `values` against the snapshot's fields.
//!
//! Steps 5 and 6 read the snapshot of this connection identity, and that
//! identity (credential hash, uid) is only known after connecting, so they
//! run after authentication but before any create / write RPC.

use super::*;

/// Largest accepted `values` object, serialized.
pub(crate) const GENERIC_WRITE_MAX_VALUES_BYTES: usize = 64 * 1024;
/// Largest accepted `ids` list for `odoo_write`.
pub(crate) const GENERIC_WRITE_MAX_IDS: usize = 100;

const POLICY_SUFFIX: &str =
    "This is a policy decision; retrying the same call will not succeed. 這是政策限制，重試相同呼叫不會成功。";
const UNAVAILABLE_SUFFIX: &str = "This capability is not available. This is a policy decision; retrying the same call will not succeed. 這項功能不提供；這是政策限制，重試相同呼叫不會成功。";

/// The `allowed_actions` verb a generic write tool needs.
pub(crate) fn generic_write_verb(tool: &str) -> Option<&'static str> {
    match tool {
        "odoo_create" => Some("create"),
        "odoo_write" => Some("write"),
        _ => None,
    }
}

/// The employee's own `agent.toml [odoo]` block, read strictly.
///
/// Both the dispatch gate and `tools/list` read the grant through this one
/// function. A missing file or block is `Ok(None)` (no grant); an unreadable
/// one is `Err` (refuse). A registered override is never used in place of the
/// file: a grant must be visible in the settings that are in force.
pub(crate) fn load_generic_write_override(
    home_dir: &Path,
    caller_agent: &str,
) -> std::result::Result<Option<duduclaw_odoo::AgentOdooConfig>, String> {
    if caller_agent.is_empty() {
        return Ok(None);
    }
    let path = caller_agent_dir(home_dir, caller_agent).join("agent.toml");
    match std::fs::read_to_string(&path) {
        Ok(raw) => duduclaw_odoo::AgentOdooConfig::parse_agent_toml(&raw),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(duduclaw_odoo::ODOO_UNREADABLE.to_string()),
    }
}

/// Explicit-grant gate (RM-120 owner decision). `Err` carries the reason.
pub(crate) fn generic_write_grant(
    cfg: Option<&duduclaw_odoo::AgentOdooConfig>,
    verb: &str,
    model: &str,
) -> std::result::Result<(), String> {
    let Some(cfg) = cfg else {
        return Err(format!(
            "explicit grant gate: generic '{verb}' needs the employee's own Odoo settings to list model '{model}' \
             under allowed tables and '{verb}' under allowed actions; this employee has no own Odoo settings \
             (dashboard: 員工 → 編輯 → 整合 → Odoo)"
        ));
    };
    if !cfg.allowed_models.iter().any(|m| m == model) {
        return Err(format!(
            "explicit grant gate: model '{model}' is not listed under allowed tables (allowed_models) in the \
             employee's Odoo settings; an empty list does not grant generic '{verb}' \
             (dashboard: 員工 → 編輯 → 整合 → Odoo)"
        ));
    }
    if !cfg.grants_explicitly(verb, model) {
        return Err(format!(
            "explicit grant gate: action '{verb}' on model '{model}' is not listed under allowed actions \
             (allowed_actions) in the employee's Odoo settings; an empty list does not grant it \
             (dashboard: 員工 → 編輯 → 整合 → Odoo)"
        ));
    }
    Ok(())
}

/// `tools/list` predicate: is `tool` callable for at least one model? The
/// same [`generic_write_grant`] rule, quantified over `allowed_models`.
pub(crate) fn generic_write_listed(home_dir: &Path, caller_agent: &str, tool: &str) -> bool {
    let Some(verb) = generic_write_verb(tool) else { return true };
    match load_generic_write_override(home_dir, caller_agent) {
        Ok(Some(cfg)) => cfg
            .allowed_models
            .iter()
            .any(|model| generic_write_grant(Some(&cfg), verb, model).is_ok()),
        _ => false,
    }
}

/// Validated arguments of one generic write call.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GenericWriteArgs {
    pub model: String,
    pub ids: Vec<i64>,
    pub values: serde_json::Map<String, Value>,
}

/// Accept a JSON value or its stringified form (some MCP clients send
/// objects and arrays as JSON text). Unparseable text stays a string, so the
/// caller reports a type error rather than a missing argument.
fn json_arg(params: &Value, name: &str) -> Option<Value> {
    match params.get(name)? {
        Value::String(text) => Some(serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.clone()))),
        other => Some(other.clone()),
    }
}

/// The `model` argument, validated with `schema::valid_model_name`.
pub(crate) fn parse_generic_write_model(params: &Value) -> std::result::Result<String, String> {
    let model = params.get("model").and_then(Value::as_str).unwrap_or("").trim();
    if model.is_empty() {
        return Err("model is required".into());
    }
    if !duduclaw_odoo::schema::valid_model_name(model) {
        return Err("model must be an Odoo model name (lowercase letters, digits, '_' and '.')".into());
    }
    Ok(model.to_string())
}

/// Local argument validation — no config, credential or network.
pub(crate) fn parse_generic_write_args(
    tool: &str,
    params: &Value,
) -> std::result::Result<GenericWriteArgs, String> {
    let model = parse_generic_write_model(params)?;
    let values = match json_arg(params, "values") {
        Some(Value::Object(map)) => map,
        Some(_) => return Err("values must be a JSON object of field names to values".into()),
        None => return Err("values is required (a JSON object of field names to values)".into()),
    };
    if values.is_empty() {
        return Err("values must name at least one field".into());
    }
    let size = serde_json::to_vec(&values).map(|v| v.len()).unwrap_or(usize::MAX);
    if size > GENERIC_WRITE_MAX_VALUES_BYTES {
        return Err(format!("values is {size} bytes; the limit is {GENERIC_WRITE_MAX_VALUES_BYTES} bytes (64 KiB)"));
    }
    for (field, value) in &values {
        if !duduclaw_odoo::schema::valid_field_name(field) {
            return Err(format!("values: '{}' is not an Odoo field name", duduclaw_core::truncate_chars(field, 64)));
        }
        check_x2many_commands(field, value)?;
    }
    let mut ids = Vec::new();
    if tool == "odoo_write" {
        let list = match json_arg(params, "ids") {
            Some(Value::Array(list)) => list,
            Some(Value::Number(n)) => vec![Value::Number(n)],
            Some(_) => return Err("ids must be a JSON array of positive integers".into()),
            None => return Err("ids is required (1 to 100 positive integers)".into()),
        };
        for item in &list {
            let id = match item {
                Value::Number(n) => n.as_i64(),
                Value::String(s) => s.trim().parse::<i64>().ok(),
                _ => None,
            };
            match id {
                Some(id) if id > 0 => {
                    if !ids.contains(&id) {
                        ids.push(id);
                    }
                }
                _ => return Err("ids must contain only positive integers".into()),
            }
        }
        if ids.is_empty() {
            return Err("ids must name at least one record".into());
        }
        if ids.len() > GENERIC_WRITE_MAX_IDS {
            return Err(format!("ids names {} records; the limit is {GENERIC_WRITE_MAX_IDS}", ids.len()));
        }
    }
    Ok(GenericWriteArgs { model, ids, values })
}

/// x2many command lists: only 0 (create), 1 (update), 4 (link) and 6 (set)
/// pass. 2 (delete), 3 (unlink), 5 (clear) and anything unrecognised are
/// refused. A list that is not a command list (e.g. a plain id list for a
/// many2many) is left for the schema check and Odoo to judge.
pub(crate) fn check_x2many_commands(field: &str, value: &Value) -> std::result::Result<(), String> {
    let Some(items) = value.as_array() else { return Ok(()) };
    let is_command_list = items.iter().any(Value::is_array);
    if !is_command_list {
        return Ok(());
    }
    for item in items {
        let code = item.as_array().and_then(|cmd| cmd.first()).and_then(Value::as_i64);
        match code {
            Some(0 | 1 | 4 | 6) => {
                // Nested values of a create / update command may hold their
                // own x2many lists.
                if let Some(Value::Object(nested)) = item.as_array().and_then(|cmd| cmd.get(2)) {
                    for (nested_field, nested_value) in nested {
                        check_x2many_commands(nested_field, nested_value)?;
                    }
                }
            }
            Some(code @ (2 | 3 | 5)) => {
                return Err(format!(
                    "values.{field}: x2many command {code} (delete / unlink / clear) is refused; odoo_create and \
                     odoo_write never remove records or links. Only commands 0 (create), 1 (update), 4 (link) and \
                     6 (set) are accepted. {UNAVAILABLE_SUFFIX}"
                ));
            }
            _ => {
                return Err(format!(
                    "values.{field}: unrecognised x2many command; only 0 (create), 1 (update), 4 (link) and 6 (set) \
                     are accepted"
                ));
            }
        }
    }
    Ok(())
}

/// Every key must be a stored, writable field of `model` in the snapshot.
/// `fields_get` reports no `compute` attribute; a non-stored field is
/// computed or related, and a computed field without an inverse is readonly,
/// so `store && !readonly` is the writable set.
pub(crate) fn check_values_against_schema(
    model: &duduclaw_odoo::schema::ScannedModel,
    values: &serde_json::Map<String, Value>,
) -> std::result::Result<(), String> {
    for key in values.keys() {
        match model.fields.iter().find(|f| &f.name == key) {
            None if model.truncated => {
                return Err(format!(
                    "values: field '{key}' is not in the scanned schema of '{}' (the scan of this model is incomplete; \
                     re-scan the Odoo schema, then try again)",
                    model.model
                ));
            }
            None => {
                return Err(format!("values: '{key}' is not a field of '{}'", model.model));
            }
            Some(field) if !field.store => {
                return Err(format!("values: '{key}' is a computed (non-stored) field of '{}' and cannot be written", model.model));
            }
            Some(field) if field.readonly => {
                return Err(format!("values: '{key}' is a read-only field of '{}'", model.model));
            }
            Some(_) => {}
        }
    }
    Ok(())
}

fn deny(home_dir: &Path, caller_agent: &str, tool: &str, model: &str, verb: &str, reason: &str) {
    duduclaw_security::audit::append_tool_call(
        home_dir,
        caller_agent,
        tool,
        &format!("DENIED: {model}/{verb} — {}", duduclaw_core::truncate_bytes(reason, 400)),
        false,
    );
}

pub(crate) async fn handle_odoo_generic_write(
    tool: &str,
    params: &Value,
    home_dir: &Path,
    odoo: &OdooState,
    caller_agent: &str,
) -> Value {
    let Some(verb) = generic_write_verb(tool) else {
        return super::egress_gate::pre_dispatch_error(&format!("Odoo: '{tool}' is not offered. {UNAVAILABLE_SUFFIX}"));
    };
    // ── 1. model name (local) ────────────────────────────────────────────
    let model_name = match parse_generic_write_model(params) {
        Ok(model) => model,
        Err(error) => {
            let hint = params.get("model").and_then(Value::as_str).unwrap_or("");
            deny(home_dir, caller_agent, tool, duduclaw_core::truncate_bytes(hint, 128), verb, &error);
            return super::egress_gate::pre_dispatch_error(&format!("Odoo {tool}: {error}"));
        }
    };
    let model = model_name.as_str();

    // ── 2. explicit grant (shared with tools/list) ──────────────────────
    let cfg = match load_generic_write_override(home_dir, caller_agent) {
        Ok(cfg) => cfg,
        Err(error) => {
            deny(home_dir, caller_agent, tool, model, verb, &error);
            return super::egress_gate::pre_dispatch_error(&error);
        }
    };
    if let Err(reason) = generic_write_grant(cfg.as_ref(), verb, model) {
        deny(home_dir, caller_agent, tool, model, verb, &reason);
        return super::egress_gate::pre_dispatch_error(&format!("Odoo permission denied: {reason}. {POLICY_SUFFIX}"));
    }
    // Keep the pool's override in step with the file, as the other tools do.
    if let Some(cfg) = &cfg {
        odoo.register_agent(caller_agent, cfg.clone()).await;
    }

    // ── 3. module switch + security block list ───────────────────────────
    let config_result = odoo.load_global_for_gate(home_dir).await;
    if let Err(error) = config_result {
        return super::egress_gate::pre_dispatch_error(&error);
    }
    if let Err(module) = odoo.feature_gate(model).await {
        let reason = format!("Odoo module '{module}' is switched off");
        deny(home_dir, caller_agent, tool, model, verb, &reason);
        return super::egress_gate::pre_dispatch_error(&format!(
            "Odoo feature_gate: module '{module}' is switched off in the dashboard (Odoo → 功能模組); '{model}' is not available. {POLICY_SUFFIX}"
        ));
    }
    let unblock_models = odoo.unblock_models(caller_agent).await;
    if let Err(denial) = duduclaw_odoo::check_blocklist(model, duduclaw_odoo::AccessKind::Mutate, &unblock_models) {
        let message = blocklist_denial_message(model, denial);
        deny(home_dir, caller_agent, tool, model, verb, &message);
        return super::egress_gate::pre_dispatch_error(&message);
    }

    // ── values / ids shape, size and x2many commands (local) ─────────────
    let args = match parse_generic_write_args(tool, params) {
        Ok(args) => args,
        Err(error) => {
            deny(home_dir, caller_agent, tool, model, verb, &error);
            return super::egress_gate::pre_dispatch_error(&format!("Odoo {tool}: {error}"));
        }
    };

    // ── 4. connection (first credential use) ─────────────────────────────
    if let Err(error) = odoo.connect_precheck(caller_agent).await {
        return super::egress_gate::pre_dispatch_error(&format!(
            "Odoo not connected for agent '{caller_agent}'. Call odoo_connect first. {error}"
        ));
    }
    let conn = match connect_odoo_pool(home_dir, odoo, caller_agent).await {
        Ok(conn) => conn,
        Err(error) => {
            return mcp_error(&format!("Odoo not connected for agent '{caller_agent}'. Call odoo_connect first. {error}"));
        }
    };
    let profile = odoo.pool_key(caller_agent).await.1;

    // ── 5./6. account ACL + field validation from this identity's snapshot
    let scan_first = "Odoo 結構正在背景準備中，尚未能確認這張資料表的欄位與帳號權限；請等結構準備完成後再試。 / \
                      The Odoo schema has not been scanned for this connection yet, so the fields and account rights \
                      of this model cannot be checked; try again once the schema is ready.";
    let snapshot = match duduclaw_odoo::schema::load_snapshot(home_dir, &conn.schema_profile(&profile)) {
        Ok(Some(snapshot)) if snapshot.connection_identity == conn.connection_identity() => snapshot,
        Ok(_) | Err(_) => {
            deny(home_dir, caller_agent, tool, model, verb, "schema snapshot unavailable");
            return mcp_error(&format!("Odoo {tool}: {scan_first}"));
        }
    };
    let rights = snapshot.access_rights.get(model);
    let allowed = match verb {
        "create" => rights.and_then(|r| r.create),
        _ => rights.and_then(|r| r.write),
    };
    match allowed {
        Some(true) => {}
        Some(false) => {
            let reason = format!("the Odoo account this employee uses has no '{verb}' right on '{model}'");
            deny(home_dir, caller_agent, tool, model, verb, &reason);
            return mcp_error(&format!("Odoo account permission: {reason}. {UNAVAILABLE_SUFFIX}"));
        }
        None => {
            let reason = format!("the '{verb}' right of the Odoo account on '{model}' is unknown (not yet checked)");
            deny(home_dir, caller_agent, tool, model, verb, &reason);
            return mcp_error(&format!(
                "Odoo account permission: {reason}; the call is refused until the schema scan has checked it. {scan_first}"
            ));
        }
    }
    let Some(scanned) = snapshot.models.iter().find(|m| m.model == model) else {
        deny(home_dir, caller_agent, tool, model, verb, "model not in schema snapshot");
        return mcp_error(&format!("Odoo {tool}: model '{model}' is not in the scanned schema. {scan_first}"));
    };
    if let Err(error) = check_values_against_schema(scanned, &args.values) {
        deny(home_dir, caller_agent, tool, model, verb, &error);
        return mcp_error(&format!("Odoo {tool}: {error}"));
    }

    // ── RPC ──────────────────────────────────────────────────────────────
    let values = Value::Object(args.values.clone());
    let result: std::result::Result<String, String> = match verb {
        "create" => conn
            .create(model, values)
            .await
            .map(|id| serde_json::json!({"ok": true, "model": model, "id": id}).to_string()),
        _ => match conn.write(model, &args.ids, values).await {
            Ok(true) => Ok(serde_json::json!({"ok": true, "model": model, "ids": args.ids}).to_string()),
            Ok(false) => Err(format!("Odoo reported that the write on '{model}' did not apply")),
            Err(error) => Err(error),
        },
    };
    let fields: Vec<&str> = args.values.keys().map(String::as_str).collect();
    let summary = format!(
        "profile={profile}; tool={tool}; model={model}; fields={}; ids={}; ok={}",
        duduclaw_core::truncate_bytes(&fields.join(","), 400),
        args.ids.len(),
        result.is_ok(),
    );
    let result_text: &str = match &result {
        Ok(text) => text.as_str(),
        Err(error) => error.as_str(),
    };
    duduclaw_security::audit::append_tool_call_with_input(
        home_dir,
        caller_agent,
        tool,
        &summary,
        result.is_ok(),
        None,
        Some(result_text),
    );
    match result {
        Ok(text) => serde_json::json!({ "content": [{"type": "text", "text": text}] }),
        Err(error) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Odoo error: {error}")}],
            "isError": true
        }),
    }
}
