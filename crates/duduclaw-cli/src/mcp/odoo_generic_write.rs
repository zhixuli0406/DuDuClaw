//! RM-120: permission-controlled generic `odoo_create` / `odoo_write`.
//!
//! These two tools are allowed **only** when the employee's own
//! `agent.toml [odoo]` settings grant them twice over:
//!
//! * the regular grant — `allowed_models` names the model exactly and
//!   `allowed_actions` holds the bare verb (`create` / `write`) or
//!   `verb:model`; an empty list grants nothing here (stricter than
//!   [`crate::odoo_pool::check_action_permission`], which is unchanged);
//! * the separate, default-off opt-in — the per-table `allowed_actions` token
//!   `generic_create:<model>` / `generic_write:<model>` (dashboard toggle
//!   允許通用新增／修改). Existing settings hold no such token, so they do
//!   not gain the tools.
//!
//! Gate order (all fail closed). Scope (`odoo:write`) and the effect class
//! (`modify`: action rules / approvals / explore lane) run in the dispatcher
//! before this handler is reached. Here:
//!
//! 1. `model` argument (name syntax);
//! 2. the fixed never-generically-writable list ([`never_generically_writable`]:
//!    users, groups, companies, `ir.*`, mail / discuss records) — regardless
//!    of any grant or unblock setting;
//! 3. regular grant + opt-in ([`generic_write_grant`]);
//! 4. 功能模組 switch and the security block list (`AccessKind::Mutate`);
//!    steps 2–4 are one predicate, [`generic_write_model_gate`], which
//!    `tools/list` uses too (listed ⇔ callable for some model);
//! 5. `values` / `ids` shape and size, x2many command codes (only 0 / 1 / 4),
//!    nesting depth, and archiving (`active` other than `true`) — local;
//! 6. connection precheck — the first step that touches credentials;
//! 7. account ACL from the schema snapshot (`create` / `write` must be
//!    `Some(true)`);
//! 8. every key of `values` against the snapshot — stored, writable, not a
//!    related / `_inherits` field; x2many values by field type (no `false` /
//!    `null` / empty / plain id lists); nested command values recursively
//!    against the related model's snapshot;
//! 9. every related model reached by a command: steps 2–4 for the verb the
//!    command needs (0 ⇒ create, 1 ⇒ write, 4 on a one2many ⇒ write because
//!    it moves the record, 4 on a many2many ⇒ link: no grant of its own, but
//!    still steps 2 and 4), plus the account ACL of step 7 for create / write.
//!
//! Steps 7–9 read the snapshot of this connection identity, and that
//! identity (credential hash, uid) is only known after connecting, so they
//! run after authentication but before any create / write RPC.

use super::*;

/// Largest accepted `values` object, serialized.
pub(crate) const GENERIC_WRITE_MAX_VALUES_BYTES: usize = 64 * 1024;
/// Largest accepted `ids` list for `odoo_write`.
pub(crate) const GENERIC_WRITE_MAX_IDS: usize = 100;
/// Deepest accepted level of nested x2many command values (top-level
/// `values` is level 0; the values of a command on one of its fields are
/// level 1, and so on).
pub(crate) const GENERIC_WRITE_MAX_DEPTH: usize = 3;

const POLICY_SUFFIX: &str =
    "This is a policy decision; retrying the same call will not succeed. 這是政策限制，重試相同呼叫不會成功。";
const UNAVAILABLE_SUFFIX: &str = "This capability is not available. This is a policy decision; retrying the same call will not succeed. 這項功能不提供；這是政策限制，重試相同呼叫不會成功。";

/// Use of a related model by a nested x2many command that only links an
/// existing record (many2many command 4).
pub(crate) const LINK_USE: &str = "link";

/// The `allowed_actions` verb a generic write tool needs.
pub(crate) fn generic_write_verb(tool: &str) -> Option<&'static str> {
    match tool {
        "odoo_create" => Some("create"),
        "odoo_write" => Some("write"),
        _ => None,
    }
}

/// Models the generic tools never create or change, whatever the grants,
/// opt-ins or `unblock_models` say: accounts and rights, companies, Odoo's own
/// `ir.*` tables (including `ir.actions.*` / `ir.ui.*`) and mail / discuss
/// records. Sub-tables of users, groups and channels (`res.users.apikeys`,
/// `res.users.settings`, `discuss.channel.member`, …) are covered by prefix.
pub(crate) fn never_generically_writable(model: &str) -> bool {
    const EXACT: &[&str] = &[
        "res.users",
        "res.groups",
        "res.company",
        "res.users.apikeys",
        "mail.mail",
        "mail.followers",
        "mail.message",
        "mail.template",
        "mail.compose.message",
        "discuss.channel",
        "mail.channel",
    ];
    const PREFIXES: &[&str] = &["ir.", "res.users.", "res.groups.", "discuss.channel.", "mail.channel."];
    EXACT.contains(&model) || PREFIXES.iter().any(|prefix| model.starts_with(prefix))
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

/// The global `config.toml [odoo]` block, parsed exactly as
/// `OdooConnectorPool::load_global_for_gate` does; `None` where that refuses.
fn load_global_odoo(home_dir: &Path) -> Option<duduclaw_odoo::OdooConfig> {
    let raw = std::fs::read_to_string(home_dir.join("config.toml")).ok()?;
    let table: toml::Table = raw.parse().ok()?;
    table.get("odoo")?.clone().try_into().ok()
}

/// Regular grant + separate opt-in gate (RM-120 owner decisions). `Err`
/// carries the reason.
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
    if !cfg.generic_opt_in(verb, model) {
        let token = duduclaw_odoo::agent_config::generic_opt_in_verb(verb).unwrap_or("generic_write");
        return Err(format!(
            "generic write opt-in gate: table '{model}' does not have 允許通用新增／修改 (allow generic create / \
             update) switched on in the employee's Odoo settings; the regular Create / Update grant alone does not \
             enable generic '{verb}' (allowed_actions token '{token}:{model}'; dashboard: 員工 → 編輯 → 整合 → Odoo)"
        ));
    }
    Ok(())
}

/// Why [`generic_write_model_gate`] refused a model.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum GenericWriteDenial {
    /// On the never-generically-writable list.
    NeverWritable,
    /// Regular grant or opt-in missing (the reason text).
    Grant(String),
    /// 功能模組 switch off.
    Module(&'static str),
    /// Security block list.
    Block(duduclaw_odoo::BlockDenial),
}

impl GenericWriteDenial {
    pub(crate) fn message(&self, tool: &str, model: &str) -> String {
        match self {
            Self::NeverWritable => format!(
                "Odoo {tool}: '{model}' is never created or changed by the generic tools (users, groups, companies, \
                 system ir.* tables, mail and discuss records). {UNAVAILABLE_SUFFIX}"
            ),
            Self::Grant(reason) => format!("Odoo permission denied: {reason}. {POLICY_SUFFIX}"),
            Self::Module(module) => format!(
                "Odoo feature_gate: module '{module}' is switched off in the dashboard (Odoo → 功能模組); '{model}' is not available. {POLICY_SUFFIX}"
            ),
            Self::Block(denial) => blocklist_denial_message(model, *denial),
        }
    }
}

/// Steps 2–4 of the gate order for one model and verb (`create`, `write`, or
/// [`LINK_USE`], which needs no grant of its own). The dispatch gate, the
/// related-model check and `tools/list` all use this predicate.
pub(crate) fn generic_write_model_gate(
    cfg: Option<&duduclaw_odoo::AgentOdooConfig>,
    global: &duduclaw_odoo::OdooConfig,
    unblock_models: &[String],
    verb: &str,
    model: &str,
) -> std::result::Result<(), GenericWriteDenial> {
    if never_generically_writable(model) {
        return Err(GenericWriteDenial::NeverWritable);
    }
    if verb != LINK_USE {
        generic_write_grant(cfg, verb, model).map_err(GenericWriteDenial::Grant)?;
    }
    global.feature_allows_model(model).map_err(GenericWriteDenial::Module)?;
    duduclaw_odoo::check_blocklist(model, duduclaw_odoo::AccessKind::Mutate, unblock_models)
        .map_err(GenericWriteDenial::Block)
}

/// `tools/list` predicate: is `tool` callable for at least one model? The
/// same [`generic_write_model_gate`] (never-writable list, grant + opt-in,
/// module switch, block list), quantified over `allowed_models`.
pub(crate) fn generic_write_listed(home_dir: &Path, caller_agent: &str, tool: &str) -> bool {
    let Some(verb) = generic_write_verb(tool) else { return true };
    let Ok(Some(cfg)) = load_generic_write_override(home_dir, caller_agent) else { return false };
    let Some(global) = load_global_odoo(home_dir) else { return false };
    // Same rule as `OdooConnectorPool::unblock_models`: an absent per-agent
    // list inherits the global one.
    let unblock = cfg.unblock_models.clone().unwrap_or_else(|| global.unblock_models.clone());
    cfg.allowed_models
        .iter()
        .any(|model| generic_write_model_gate(Some(&cfg), &global, &unblock, verb, model).is_ok())
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
        check_not_archiving(&format!("values.{field}"), field, value)?;
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

/// `active` set to anything but `true` archives the record (Odoo reads any
/// falsy value as false). The generic tools never archive.
fn check_not_archiving(at: &str, field: &str, value: &Value) -> std::result::Result<(), String> {
    if field == "active" && *value != Value::Bool(true) {
        return Err(format!(
            "{at}: setting 'active' to anything other than true archives the record, which odoo_create and \
             odoo_write do not do. {UNAVAILABLE_SUFFIX}"
        ));
    }
    Ok(())
}

fn removal_refused(at: &str, code: i64) -> String {
    let what = match code {
        2 => "(delete) deletes the related record",
        3 => "(unlink) removes the link",
        5 => "(clear) removes every line or link",
        _ => "(set) replaces the whole list and removes every line or link it does not name",
    };
    format!(
        "{at}: x2many command {code} {what}; odoo_create and odoo_write never remove records or links. Only \
         commands 0 (create), 1 (update) and 4 (link) are accepted. {UNAVAILABLE_SUFFIX}"
    )
}

fn unrecognised_command(at: &str) -> String {
    format!("{at}: unrecognised x2many command; only 0 (create), 1 (update) and 4 (link) are accepted")
}

fn depth_refused(at: &str) -> String {
    format!(
        "{at}: x2many commands are nested more than {GENERIC_WRITE_MAX_DEPTH} levels deep; split the change into \
         separate calls"
    )
}

/// Local (schema-free) x2many check of one top-level field: in any list of
/// command lists only 0 (create), 1 (update) and 4 (link) pass; 2 (delete),
/// 3 (unlink), 5 (clear), 6 (set) and anything unrecognised are refused,
/// nested command values are checked the same way (and for archiving) up to
/// [`GENERIC_WRITE_MAX_DEPTH`]. `false` / `null` / plain id lists need the
/// field type and are judged against the snapshot
/// ([`check_values_against_schema`]).
pub(crate) fn check_x2many_commands(field: &str, value: &Value) -> std::result::Result<(), String> {
    check_commands_at(&format!("values.{field}"), value, 0)
}

fn check_commands_at(at: &str, value: &Value, depth: usize) -> std::result::Result<(), String> {
    let Some(items) = value.as_array() else { return Ok(()) };
    if !items.iter().any(Value::is_array) {
        return Ok(());
    }
    for item in items {
        let command = item.as_array();
        match command.and_then(|cmd| cmd.first()).and_then(Value::as_i64) {
            Some(code @ (0 | 1)) => {
                if let Some(Value::Object(nested)) = command.and_then(|cmd| cmd.get(2)) {
                    if depth + 1 > GENERIC_WRITE_MAX_DEPTH {
                        return Err(depth_refused(at));
                    }
                    for (nested_field, nested_value) in nested {
                        let nested_at = format!("{at}[{code}].{nested_field}");
                        check_not_archiving(&nested_at, nested_field, nested_value)?;
                        check_commands_at(&nested_at, nested_value, depth + 1)?;
                    }
                }
            }
            Some(4) => {}
            Some(code @ (2 | 3 | 5 | 6)) => return Err(removal_refused(at, code)),
            _ => return Err(unrecognised_command(at)),
        }
    }
    Ok(())
}

/// Authorizes one use of a related model by a nested command: called with
/// the related model and `create`, `write` or [`LINK_USE`].
pub(crate) type RelatedAuthorizer<'a> = dyn FnMut(&str, &'static str) -> std::result::Result<(), String> + 'a;

/// Every key must be a stored, writable, non-related field of `model` in the
/// snapshot, recursively through nested x2many command values. `fields_get`
/// reports no `compute` attribute; a non-stored field is computed or related,
/// and a computed field without an inverse is readonly, so `store &&
/// !readonly` is the writable set; `related` (also set for `_inherits`
/// fields) writes through to another record and is refused as well.
///
/// Every command that reaches a related model is passed to `authorize`
/// before its values are looked at (the dispatch gate passes
/// [`generic_write_model_gate`] plus the account ACL).
pub(crate) fn check_values_against_schema(
    snapshot: &duduclaw_odoo::schema::SchemaSnapshot,
    model: &duduclaw_odoo::schema::ScannedModel,
    values: &serde_json::Map<String, Value>,
    authorize: &mut RelatedAuthorizer<'_>,
) -> std::result::Result<(), String> {
    check_fields_at(snapshot, model, values, "values", 0, authorize)
}

fn check_fields_at(
    snapshot: &duduclaw_odoo::schema::SchemaSnapshot,
    model: &duduclaw_odoo::schema::ScannedModel,
    values: &serde_json::Map<String, Value>,
    path: &str,
    depth: usize,
    authorize: &mut RelatedAuthorizer<'_>,
) -> std::result::Result<(), String> {
    for (key, value) in values {
        if !duduclaw_odoo::schema::valid_field_name(key) {
            return Err(format!("{path}: '{}' is not an Odoo field name", duduclaw_core::truncate_chars(key, 64)));
        }
        let at = format!("{path}.{key}");
        check_not_archiving(&at, key, value)?;
        let field = match model.fields.iter().find(|f| &f.name == key) {
            None if model.truncated => {
                return Err(format!(
                    "{path}: field '{key}' is not in the scanned schema of '{}' (the scan of this model is incomplete; \
                     re-scan the Odoo schema, then try again)",
                    model.model
                ));
            }
            None => return Err(format!("{path}: '{key}' is not a field of '{}'", model.model)),
            Some(field) => field,
        };
        if !field.store {
            return Err(format!("{path}: '{key}' is a computed (non-stored) field of '{}' and cannot be written", model.model));
        }
        if field.readonly {
            return Err(format!("{path}: '{key}' is a read-only field of '{}'", model.model));
        }
        if matches!(field.ttype.as_str(), "properties" | "properties_definition") {
            return Err(format!(
                "{path}: '{key}' is a {} field of '{}' (writing it can change a property definition stored on another \
                 record) and cannot be written by odoo_create / odoo_write. {UNAVAILABLE_SUFFIX}",
                field.ttype, model.model
            ));
        }
        match field.related {
            Some(false) => {}
            Some(true) => {
                return Err(format!(
                    "{path}: '{key}' is a related field of '{}' (it writes through to another record) and cannot be \
                     written by odoo_create / odoo_write. {UNAVAILABLE_SUFFIX}",
                    model.model
                ));
            }
            None => {
                return Err(format!(
                    "{path}: whether '{key}' of '{}' is a related field is not recorded in this schema snapshot (it was \
                     scanned by an older version); re-scan the Odoo schema, then try again",
                    model.model
                ));
            }
        }
        if matches!(field.ttype.as_str(), "one2many" | "many2many") {
            check_x2many_value(snapshot, field, value, &at, depth, authorize)?;
        }
    }
    Ok(())
}

fn positive_id(value: Option<&Value>) -> bool {
    value.and_then(Value::as_i64).is_some_and(|id| id > 0)
}

/// An x2many value, judged by the field type: only a non-empty list of
/// commands 0 / 1 / 4 passes; `false` / `null` (clear), an empty list and a
/// plain id list (replace) are refused. Nested values are checked against
/// the related model's snapshot.
fn check_x2many_value(
    snapshot: &duduclaw_odoo::schema::SchemaSnapshot,
    field: &duduclaw_odoo::schema::ScannedField,
    value: &Value,
    at: &str,
    depth: usize,
    authorize: &mut RelatedAuthorizer<'_>,
) -> std::result::Result<(), String> {
    let kind = field.ttype.as_str();
    let one2many = kind == "one2many";
    let items = match value {
        Value::Array(items) if !items.is_empty() => items,
        Value::Array(_) => {
            return Err(format!(
                "{at}: an empty list on a {kind} field is refused (some Odoo versions read it as 'remove all'); leave \
                 the field out instead. {UNAVAILABLE_SUFFIX}"
            ));
        }
        Value::Bool(false) | Value::Null => {
            return Err(format!(
                "{at}: false / null on a {kind} field clears it (removes every line or link) and is refused. \
                 {UNAVAILABLE_SUFFIX}"
            ));
        }
        _ => {
            return Err(format!(
                "{at}: a {kind} field takes a list of x2many commands: [0, 0, {{values}}] (create), [1, id, {{values}}] \
                 (update) or [4, id] (link)"
            ));
        }
    };
    if items.iter().any(|item| !item.is_array()) {
        return Err(format!(
            "{at}: a plain id list on a {kind} field replaces the whole set (removing the lines or links it does not \
             name) and is refused; use [4, id] commands to link records. {UNAVAILABLE_SUFFIX}"
        ));
    }
    let Some(related) = field.relation.as_deref() else {
        return Err(format!(
            "{at}: the related table of this {kind} field is not in the scanned schema (it may be blocked or switched \
             off), so its commands cannot be checked. {POLICY_SUFFIX}"
        ));
    };
    for item in items {
        let command = item.as_array().map(Vec::as_slice).unwrap_or(&[]);
        let code = command.first().and_then(Value::as_i64);
        match code {
            Some(code @ (0 | 1)) => {
                let shape_ok = command.len() == 3 && (code == 0 || positive_id(command.get(1)));
                let Some(Value::Object(nested)) = command.get(2).filter(|_| shape_ok) else {
                    return Err(format!(
                        "{at}: command {code} must be [{}, {{values}}]",
                        if code == 0 { "0, 0" } else { "1, id" }
                    ));
                };
                authorize(related, if code == 0 { "create" } else { "write" }).map_err(|e| format!("{at}: {e}"))?;
                if depth + 1 > GENERIC_WRITE_MAX_DEPTH {
                    return Err(depth_refused(at));
                }
                let Some(related_model) = snapshot.models.iter().find(|m| m.model == related) else {
                    return Err(format!(
                        "{at}: related table '{related}' is not in the scanned schema, so the values of command {code} \
                         cannot be checked. {POLICY_SUFFIX}"
                    ));
                };
                check_fields_at(snapshot, related_model, nested, &format!("{at}[{code}]"), depth + 1, authorize)?;
            }
            Some(4) => {
                if !(command.len() == 2 || command.len() == 3) || !positive_id(command.get(1)) {
                    return Err(format!("{at}: command 4 must be [4, id]"));
                }
                // On a one2many, linking moves an existing record away from
                // its current parent: that is a write on the related record.
                authorize(related, if one2many { "write" } else { LINK_USE }).map_err(|e| format!("{at}: {e}"))?;
            }
            Some(code @ (2 | 3 | 5 | 6)) => return Err(removal_refused(at, code)),
            _ => return Err(unrecognised_command(at)),
        }
    }
    Ok(())
}

/// Error class of a failed create / write, for the audit log: a fixed local
/// category, never the remote text.
fn error_class(error: &str) -> &'static str {
    if let Some(rest) = error.strip_prefix("Odoo server error: ") {
        const CLASSES: &[&str] = &[
            "UserError", "ValidationError", "MissingError", "AccessError", "AccessDenied", "RedirectWarning",
            "CacheMiss", "DatabaseError", "HttpError",
        ];
        let class = rest.split(|c: char| !c.is_ascii_alphanumeric()).next().unwrap_or("");
        return CLASSES.iter().find(|known| **known == class).copied().unwrap_or("UnknownError");
    }
    if error.contains("ACL gate") {
        "AccessError"
    } else if error.contains("authentication gate") {
        "AccessDenied"
    } else if error.contains("capability is not available") {
        "Unavailable"
    } else if error.contains("HTTP") {
        "HttpError"
    } else if error.contains("did not apply") {
        "WriteNotApplied"
    } else {
        "OtherError"
    }
}

/// Caller-supplied text bound for the audit line: ASCII control characters
/// (including DEL) become `?`, so it cannot forge line breaks or terminal
/// escapes.
fn audit_safe(text: &str) -> String {
    text.chars().map(|c| if c.is_ascii_control() { '?' } else { c }).collect()
}

pub(super) fn deny(home_dir: &Path, caller_agent: &str, tool: &str, model: &str, verb: &str, reason: &str) {
    duduclaw_security::audit::append_tool_call(
        home_dir,
        caller_agent,
        tool,
        &format!(
            "DENIED: {}/{verb} — {}",
            audit_safe(duduclaw_core::truncate_bytes(model, 128)),
            audit_safe(duduclaw_core::truncate_bytes(reason, 400))
        ),
        false,
    );
}

/// `(verb, model)` of a generic write call for the isolation provider's
/// pre-credential `check_operation`; `None` for other tools or a missing /
/// malformed model (which [`generic_write_local_gate`] refuses).
pub(crate) fn classify_generic_write_call(tool: &str, params: &Value) -> Option<(&'static str, String)> {
    Some((generic_write_verb(tool)?, parse_generic_write_model(params).ok()?))
}

/// Protected mode (owner decision C): gate steps 1–5 — all local, no
/// credential, no connection — read from the employee's own settings files,
/// exactly as [`generic_write_listed`] reads them. The isolation-bound Odoo
/// executor runs this before it resolves the connector credential or
/// authenticates, so a call RM-120 refuses never dials Odoo. The bound
/// handler ([`handle_bound_odoo_generic_write`]) repeats every step, then
/// steps 6–9. `Err` is the tool result to return (already audited).
pub(crate) fn generic_write_local_gate(
    tool: &str,
    params: &Value,
    home_dir: &Path,
    caller_agent: &str,
) -> std::result::Result<(), Value> {
    let Some(verb) = generic_write_verb(tool) else {
        return Err(super::egress_gate::pre_dispatch_error(&format!("Odoo: '{tool}' is not offered. {UNAVAILABLE_SUFFIX}")));
    };
    let model = match parse_generic_write_model(params) {
        Ok(model) => model,
        Err(error) => {
            let hint = params.get("model").and_then(Value::as_str).unwrap_or("");
            deny(home_dir, caller_agent, tool, duduclaw_core::truncate_bytes(hint, 128), verb, &error);
            return Err(super::egress_gate::pre_dispatch_error(&format!("Odoo {tool}: {error}")));
        }
    };
    let model = model.as_str();
    if never_generically_writable(model) {
        deny(home_dir, caller_agent, tool, model, verb, "never generically writable");
        return Err(super::egress_gate::pre_dispatch_error(&GenericWriteDenial::NeverWritable.message(tool, model)));
    }
    let cfg = match load_generic_write_override(home_dir, caller_agent) {
        Ok(cfg) => cfg,
        Err(error) => {
            deny(home_dir, caller_agent, tool, model, verb, &error);
            return Err(super::egress_gate::pre_dispatch_error(&error));
        }
    };
    if let Err(reason) = generic_write_grant(cfg.as_ref(), verb, model) {
        deny(home_dir, caller_agent, tool, model, verb, &reason);
        return Err(super::egress_gate::pre_dispatch_error(&GenericWriteDenial::Grant(reason).message(tool, model)));
    }
    let Some(global) = load_global_odoo(home_dir) else {
        return Err(super::egress_gate::pre_dispatch_error("Odoo not connected. Call odoo_connect first."));
    };
    let unblock = cfg.as_ref().and_then(|c| c.unblock_models.clone())
        .unwrap_or_else(|| global.unblock_models.clone());
    if let Err(denial) = generic_write_model_gate(cfg.as_ref(), &global, &unblock, verb, model) {
        let message = denial.message(tool, model);
        deny(home_dir, caller_agent, tool, model, verb, &message);
        return Err(super::egress_gate::pre_dispatch_error(&message));
    }
    if let Err(error) = parse_generic_write_args(tool, params) {
        deny(home_dir, caller_agent, tool, model, verb, &error);
        return Err(super::egress_gate::pre_dispatch_error(&format!("Odoo {tool}: {error}")));
    }
    Ok(())
}

pub(crate) async fn handle_odoo_generic_write(
    tool: &str,
    params: &Value,
    home_dir: &Path,
    odoo: &OdooState,
    caller_agent: &str,
) -> Value {
    generic_write_with_connection(tool, params, home_dir, odoo, caller_agent, None).await
}

/// Protected mode: the same gate chain on the employee's own isolation
/// connector (already selected, bound and authenticated by the provider).
/// Step 6 uses that connector — no pool lookup, account resolution or
/// auto-connect — and every other step is unchanged.
pub(crate) async fn handle_bound_odoo_generic_write(
    tool: &str,
    params: &Value,
    home_dir: &Path,
    odoo: &OdooState,
    caller_agent: &str,
    connector: std::sync::Arc<duduclaw_odoo::OdooConnector>,
) -> Value {
    generic_write_with_connection(tool, params, home_dir, odoo, caller_agent, Some(connector)).await
}

async fn generic_write_with_connection(
    tool: &str,
    params: &Value,
    home_dir: &Path,
    odoo: &OdooState,
    caller_agent: &str,
    bound: Option<std::sync::Arc<duduclaw_odoo::OdooConnector>>,
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

    // ── 2. never-generically-writable list (fixed, local) ───────────────
    if never_generically_writable(model) {
        let message = GenericWriteDenial::NeverWritable.message(tool, model);
        deny(home_dir, caller_agent, tool, model, verb, "never generically writable");
        return super::egress_gate::pre_dispatch_error(&message);
    }

    // ── 3. regular grant + opt-in (shared with tools/list) ──────────────
    let cfg = match load_generic_write_override(home_dir, caller_agent) {
        Ok(cfg) => cfg,
        Err(error) => {
            deny(home_dir, caller_agent, tool, model, verb, &error);
            return super::egress_gate::pre_dispatch_error(&error);
        }
    };
    if let Err(reason) = generic_write_grant(cfg.as_ref(), verb, model) {
        deny(home_dir, caller_agent, tool, model, verb, &reason);
        return super::egress_gate::pre_dispatch_error(&GenericWriteDenial::Grant(reason).message(tool, model));
    }
    // Keep the pool's override in step with the file, as the other tools do.
    if let Some(cfg) = &cfg {
        odoo.register_agent(caller_agent, cfg.clone()).await;
    }

    // ── 4. module switch + security block list ───────────────────────────
    if let Err(error) = odoo.load_global_for_gate(home_dir).await {
        return super::egress_gate::pre_dispatch_error(&error);
    }
    let global = odoo.global_config().await;
    let unblock_models = odoo.unblock_models(caller_agent).await;
    if let Err(denial) = generic_write_model_gate(cfg.as_ref(), &global, &unblock_models, verb, model) {
        let message = denial.message(tool, model);
        deny(home_dir, caller_agent, tool, model, verb, &message);
        return super::egress_gate::pre_dispatch_error(&message);
    }

    // ── 5. values / ids shape, size, x2many codes, archiving (local) ────
    let args = match parse_generic_write_args(tool, params) {
        Ok(args) => args,
        Err(error) => {
            deny(home_dir, caller_agent, tool, model, verb, &error);
            return super::egress_gate::pre_dispatch_error(&format!("Odoo {tool}: {error}"));
        }
    };

    // ── 6. connection (first credential use) ─────────────────────────────
    // Protected mode: the isolation connector the provider bound to this
    // employee, used as-is (no pool lookup / account resolution).
    let conn = if let Some(connector) = bound {
        connector
    } else {
        if let Err(error) = odoo.connect_precheck(caller_agent).await {
            return super::egress_gate::pre_dispatch_error(&format!(
                "Odoo not connected for agent '{caller_agent}'. Call odoo_connect first. {error}"
            ));
        }
        match connect_odoo_pool(home_dir, odoo, caller_agent).await {
            Ok(conn) => conn,
            Err(error) => {
                return mcp_error(&format!("Odoo not connected for agent '{caller_agent}'. Call odoo_connect first. {error}"));
            }
        }
    };
    if conn.verify_io().is_err() {
        return mcp_error("Odoo authority revoked");
    }
    let profile = odoo.pool_key(caller_agent).await.1;

    // ── 7. account ACL from this identity's snapshot ─────────────────────
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
    let account_right = |target: &str, right: &str| {
        let rights = snapshot.access_rights.get(target);
        match right {
            "create" => rights.and_then(|r| r.create),
            _ => rights.and_then(|r| r.write),
        }
    };
    match account_right(model, verb) {
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

    // ── 8./9. fields and x2many values against the snapshot; every related
    // model a nested command reaches passes the same model gate (and, for
    // create / update, the account ACL) before its values are looked at.
    let Some(scanned) = snapshot.models.iter().find(|m| m.model == model) else {
        deny(home_dir, caller_agent, tool, model, verb, "model not in schema snapshot");
        return mcp_error(&format!("Odoo {tool}: model '{model}' is not in the scanned schema. {scan_first}"));
    };
    let mut authorize = |related: &str, use_verb: &'static str| -> std::result::Result<(), String> {
        if let Err(denial) = generic_write_model_gate(cfg.as_ref(), &global, &unblock_models, use_verb, related) {
            return Err(format!(
                "a nested command needs '{use_verb}' on related table '{related}', which is refused — {}",
                denial.message(tool, related)
            ));
        }
        if use_verb != LINK_USE && account_right(related, use_verb) != Some(true) {
            return Err(format!(
                "Odoo account permission: the '{use_verb}' right of the Odoo account on related table '{related}' is \
                 not confirmed by the schema scan. {POLICY_SUFFIX}"
            ));
        }
        Ok(())
    };
    if let Err(error) = check_values_against_schema(&snapshot, scanned, &args.values, &mut authorize) {
        deny(home_dir, caller_agent, tool, model, verb, &error);
        return mcp_error(&format!("Odoo {tool}: {error}"));
    }

    // ── RPC ──────────────────────────────────────────────────────────────
    // A revoked binding (isolation) stops here, before the create / write.
    if conn.verify_io().is_err() {
        return mcp_error("Odoo authority revoked");
    }
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
        audit_safe(duduclaw_core::truncate_bytes(&fields.join(","), 400)),
        args.ids.len(),
        result.is_ok(),
    );
    // Values never reach the audit log; on failure only the error class and
    // length do, never the remote text.
    let result_text = match &result {
        Ok(text) => text.clone(),
        Err(error) => format!("error class={}; length={}", error_class(error), error.len()),
    };
    duduclaw_security::audit::append_tool_call_with_input(
        home_dir,
        caller_agent,
        tool,
        &summary,
        result.is_ok(),
        None,
        Some(result_text.as_str()),
    );
    match result {
        Ok(text) => serde_json::json!({ "content": [{"type": "text", "text": text}] }),
        Err(error) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Odoo error: {error}")}],
            "isError": true
        }),
    }
}
