//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

/// The redaction poison state (DESIGN-redaction-field-rules-2026-09 §12).
///
/// Set when `[redaction]` was configured but could not be resolved into a live
/// manager — an unparseable section, a rule that will not compile, a broken
/// key directory. It is deliberately NOT the same thing as
/// `redaction_manager: None` (which is the ordinary "redaction not enabled"
/// state): the gateway keeps serving, but says so loudly instead of running
/// unprotected in silence.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RedactionPoison {
    /// Operator-facing cause — the parse / open error text, single-lined and
    /// byte-capped by `redaction_integration::poison_reason`.
    pub reason: String,
    /// When this poison state was entered.
    pub since: DateTime<Utc>,
}

impl RedactionPoison {
    /// Enter the poison state now with `reason` as the cause.
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            since: Utc::now(),
        }
    }

    /// Wire shape for `redaction.get` / `redaction.policy_status`.
    fn to_wire(&self) -> Value {
        json!({ "reason": self.reason, "since": self.since.to_rfc3339() })
    }
}

/// `poisoned` field value for an RPC response: the poison object, or `null`.
pub(crate) fn poison_wire(poison: Option<&RedactionPoison>) -> Value {
    poison.map(RedactionPoison::to_wire).unwrap_or(Value::Null)
}

/// Validate a redaction source mode string.
pub(crate) fn is_valid_source_mode(v: &str) -> bool {
    matches!(v, "on" | "off" | "selective" | "inherit")
}

/// Apply a `redaction.update` payload onto a config.toml table's `[redaction]`
/// section. Returns the change list. Validates ttl/purge ranges + source-mode
/// + tool-egress restore-args enums.
pub(crate) fn apply_redaction_to_table(
    table: &mut toml::Table,
    params: &Value,
) -> Result<Vec<String>, String> {
    let mut changes: Vec<String> = Vec::new();

    let red = table
        .entry("redaction")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .ok_or_else(|| "Invalid [redaction] section".to_string())?;

    // ── Root scalars ──
    if let Some(v) = params.get("enabled").and_then(|v| v.as_bool()) {
        red.insert("enabled".into(), toml::Value::Boolean(v));
        changes.push(format!("redaction.enabled = {v}"));
    }
    if let Some(v) = params.get("vault_ttl_hours").and_then(|v| v.as_i64()) {
        if v <= 0 || v > 8760 {
            return Err("vault_ttl_hours must be 1-8760".into());
        }
        red.insert("vault_ttl_hours".into(), toml::Value::Integer(v));
        changes.push(format!("redaction.vault_ttl_hours = {v}"));
    }
    if let Some(v) = params
        .get("purge_after_expire_days")
        .and_then(|v| v.as_u64())
    {
        if v > 3650 {
            return Err("purge_after_expire_days must be 0-3650".into());
        }
        red.insert(
            "purge_after_expire_days".into(),
            toml::Value::Integer(v as i64),
        );
        changes.push(format!("redaction.purge_after_expire_days = {v}"));
    }
    if let Some(arr) = params.get("profiles").and_then(|v| v.as_array()) {
        let mut out = Vec::with_capacity(arr.len());
        for item in arr {
            let s = item
                .as_str()
                .ok_or_else(|| "profiles entries must be strings".to_string())?;
            let s = s.trim();
            if s.is_empty() {
                return Err("profiles entries must be non-empty".into());
            }
            out.push(toml::Value::String(s.into()));
        }
        red.insert("profiles".into(), toml::Value::Array(out));
        changes.push(format!("redaction.profiles = [{} entries]", arr.len()));
    }

    // ── [redaction.sources] per-source modes + optional category filters ──
    // Each source accepts either a bare mode string ("on") or a detail object
    // `{ mode, only_categories[], exclude_categories[] }` — mirroring the two
    // TOML forms `duduclaw-redaction` parses. Empty filter lists collapse back
    // to the compact string form.
    if let Some(sources) = params.get("sources").and_then(|v| v.as_object()) {
        let parse_categories = |obj: &serde_json::Map<String, Value>,
                                key: &str,
                                field: &str|
         -> Result<Vec<String>, String> {
            let Some(arr) = obj.get(field) else {
                return Ok(Vec::new());
            };
            let arr = arr
                .as_array()
                .ok_or_else(|| format!("sources.{key}.{field} must be an array"))?;
            if arr.len() > 64 {
                return Err(format!("sources.{key}.{field} accepts at most 64 entries"));
            }
            let mut out = Vec::with_capacity(arr.len());
            for item in arr {
                let s = item
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        format!("sources.{key}.{field} entries must be non-empty strings")
                    })?;
                if s.len() > 64 {
                    return Err(format!(
                        "sources.{key}.{field} entry too long (max 64 chars)"
                    ));
                }
                out.push(s.to_string());
            }
            Ok(out)
        };

        let sub = red
            .entry("sources")
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
            .as_table_mut()
            .ok_or_else(|| "Invalid [redaction.sources] section".to_string())?;
        for key in &[
            "user_input",
            "tool_results",
            "system_prompt",
            "sub_agent",
            "cron_context",
        ] {
            let Some(v) = sources.get(*key) else { continue };
            let (mode, only, exclude) = if let Some(s) = v.as_str() {
                (s.to_string(), Vec::new(), Vec::new())
            } else if let Some(obj) = v.as_object() {
                let mode = obj
                    .get("mode")
                    .and_then(|m| m.as_str())
                    .ok_or_else(|| format!("sources.{key}.mode is required"))?
                    .to_string();
                (
                    mode,
                    parse_categories(obj, key, "only_categories")?,
                    parse_categories(obj, key, "exclude_categories")?,
                )
            } else {
                return Err(format!("sources.{key} must be a mode string or an object"));
            };
            if !is_valid_source_mode(&mode) {
                return Err(format!(
                    "Invalid redaction source mode '{mode}' for {key}. Valid: on, off, selective, inherit"
                ));
            }
            if only.is_empty() && exclude.is_empty() {
                sub.insert((*key).into(), toml::Value::String(mode.clone()));
                changes.push(format!("redaction.sources.{key} = \"{mode}\""));
            } else {
                let mut detail = toml::map::Map::new();
                detail.insert("mode".into(), toml::Value::String(mode.clone()));
                if !only.is_empty() {
                    detail.insert(
                        "only_categories".into(),
                        toml::Value::Array(
                            only.iter()
                                .map(|c| toml::Value::String(c.clone()))
                                .collect(),
                        ),
                    );
                }
                if !exclude.is_empty() {
                    detail.insert(
                        "exclude_categories".into(),
                        toml::Value::Array(
                            exclude
                                .iter()
                                .map(|c| toml::Value::String(c.clone()))
                                .collect(),
                        ),
                    );
                }
                sub.insert((*key).into(), toml::Value::Table(detail));
                changes.push(format!(
                    "redaction.sources.{key} = {{ mode = \"{mode}\", only = {}, exclude = {} }}",
                    only.len(),
                    exclude.len()
                ));
            }
        }
    }

    // ── [redaction.tool_egress.<tool>] add/update/remove ──
    // `tool_egress` is an object keyed by tool name. A value of `null` removes
    // that tool's rule; otherwise `{ restore_args, audit_reveal }` upserts it.
    if let Some(egress) = params.get("tool_egress").and_then(|v| v.as_object()) {
        for (tool, rule) in egress {
            let tool_trim = tool.trim();
            if tool_trim.is_empty() {
                return Err("tool_egress tool names must be non-empty".into());
            }
            if rule.is_null() {
                if let Some(eg) = red.get_mut("tool_egress").and_then(|v| v.as_table_mut()) {
                    eg.remove(tool_trim);
                    changes.push(format!("redaction.tool_egress.{tool_trim} removed"));
                }
                continue;
            }
            let rule_obj = rule
                .as_object()
                .ok_or_else(|| format!("tool_egress.{tool_trim} must be an object or null"))?;
            let restore = rule_obj
                .get("restore_args")
                .and_then(|v| v.as_str())
                .unwrap_or("deny");
            if !matches!(restore, "restore" | "passthrough" | "deny") {
                return Err(format!(
                    "Invalid restore_args '{restore}' for tool_egress.{tool_trim}. Valid: restore, passthrough, deny"
                ));
            }
            let audit_reveal = rule_obj
                .get("audit_reveal")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let eg = red
                .entry("tool_egress")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut()
                .ok_or_else(|| "Invalid [redaction.tool_egress] section".to_string())?;
            let mut entry = toml::map::Map::new();
            entry.insert("restore_args".into(), toml::Value::String(restore.into()));
            entry.insert("audit_reveal".into(), toml::Value::Boolean(audit_reveal));
            eg.insert(tool_trim.to_string(), toml::Value::Table(entry));
            changes.push(format!(
                "redaction.tool_egress.{tool_trim} = {{ restore_args = \"{restore}\", audit_reveal = {audit_reveal} }}"
            ));
        }
    }

    // ── [redaction.rules.<id>] structured-field rules (§11.2) ──
    // Upsert-merge keyed by rule id, `null` removes. Only db_field /
    // json_path rules are reachable from here; the caller dry-compiles the
    // resulting table before anything is written to disk.
    if let Some(field_rules) = params.get("field_rules").and_then(|v| v.as_object()) {
        apply_field_rules_to_table(red, field_rules, &mut changes)?;
    }

    // ── [redaction.data_sources.<name>] registry entries (§13.5) ──
    // Applied AFTER field_rules on purpose: one call may retire a rule and the
    // source it was the last user of, and the "still referenced" guard must
    // see the rule list as it will be written, not as it was.
    if let Some(data_sources) = params.get("data_sources").and_then(|v| v.as_object()) {
        apply_data_sources_to_table(red, data_sources, &mut changes)?;
    }

    Ok(changes)
}

/// Structured-field rule kinds the dashboard editor owns. Everything else
/// (`regex`, `keyword`, `identity`) stays TOML-only: those rules are neither
/// listed by `redaction.get` nor writable through `redaction.update`, so an
/// editor round-trip can never silently rewrite a detection rule it cannot
/// render.
pub(crate) const FIELD_RULE_KINDS: [&str; 2] = ["db_field", "json_path"];

/// Rule id charset for the field-rule editor: `^[a-z][a-z0-9_-]{0,63}$`.
/// Checked by hand rather than by regex — the id becomes a TOML key and an
/// audit-log field, so the accepted set stays small and explicit.
pub(crate) fn is_valid_field_rule_id(id: &str) -> bool {
    let mut chars = id.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    if id.len() > 64 {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// The `type` of an existing `[redaction.rules.<id>]` entry, if any.
pub(crate) fn existing_rule_kind<'a>(red: &'a toml::Table, id: &str) -> Option<&'a str> {
    red.get("rules")
        .and_then(|v| v.as_table())
        .and_then(|rules| rules.get(id))
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("type"))
        .and_then(|v| v.as_str())
}

/// Wire shape of one structured-field rule for `redaction.get`.
pub(crate) fn field_rule_to_wire(spec: &duduclaw_redaction::RuleSpec) -> Option<Value> {
    use duduclaw_redaction::RuleKind;
    let restore_scope = serde_json::to_value(&spec.restore_scope).ok()?;
    let mut obj = serde_json::Map::new();
    obj.insert("id".into(), json!(spec.id));
    obj.insert("category".into(), json!(spec.category));
    obj.insert("restore_scope".into(), restore_scope);
    obj.insert("priority".into(), json!(spec.priority));
    obj.insert(
        "cross_session_stable".into(),
        json!(spec.cross_session_stable),
    );
    match &spec.kind {
        RuleKind::DbField {
            source,
            connector,
            fields,
        } => {
            // The wire keeps the historical `connector` key and fills it with
            // the RESOLVED data-source name (`source` wins, `connector` is its
            // deprecated alias, neither ⇒ `odoo`). An editor round-trip
            // therefore stays lossless whichever spelling the TOML uses, and
            // the dashboard never has to re-implement the resolution.
            let resolved = duduclaw_redaction::rules::db_field::resolve_source(
                &spec.id,
                source.as_deref(),
                connector.as_deref(),
            )
            .ok()?;
            obj.insert("kind".into(), json!("db_field"));
            obj.insert("connector".into(), json!(resolved));
            obj.insert("fields".into(), json!(fields));
        }
        RuleKind::JsonPath {
            paths,
            match_tool,
            match_args,
            match_result,
            exclude_keys,
        } => {
            obj.insert("kind".into(), json!("json_path"));
            obj.insert("paths".into(), json!(paths));
            obj.insert("match_tool".into(), json!(match_tool));
            obj.insert("match_args".into(), json!(match_args));
            // Rendered even though the editor has no widget for it yet: the
            // update path parses it straight back, so shipping it keeps an
            // editor round-trip lossless. Dropping it would silently widen a
            // rule bound to one table into one that fires on every result of
            // that tool.
            obj.insert("match_result".into(), json!(match_result));
            obj.insert("exclude_keys".into(), json!(exclude_keys));
        }
        // Not an editor-owned kind — never listed.
        _ => return None,
    }
    Some(Value::Object(obj))
}

/// The `field_rules` array for `redaction.get`: every `[redaction.rules.*]`
/// entry whose `type` is `db_field` / `json_path`, in id order. Entries that
/// do not deserialise are skipped rather than half-rendered — a rule the
/// editor cannot round-trip must not appear editable.
pub(crate) fn redaction_field_rules(table: &toml::Table) -> Vec<Value> {
    let Some(rules) = table
        .get("redaction")
        .and_then(|v| v.as_table())
        .and_then(|r| r.get("rules"))
        .and_then(|v| v.as_table())
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (id, body) in rules {
        let kind = body
            .as_table()
            .and_then(|t| t.get("type"))
            .and_then(|v| v.as_str());
        if !kind.is_some_and(|k| FIELD_RULE_KINDS.contains(&k)) {
            continue;
        }
        let Ok(mut spec) = body.clone().try_into::<duduclaw_redaction::RuleSpec>() else {
            continue;
        };
        spec.id = id.clone();
        if let Some(v) = field_rule_to_wire(&spec) {
            out.push(v);
        }
    }
    out
}

/// Apply the `field_rules` upsert-merge onto `[redaction.rules.*]`.
///
/// Same semantics as `tool_egress`: `null` removes that id, an absent id is
/// untouched. Every write is validated BEFORE the table is mutated —
/// id charset, deserialisable body, editor-owned kind, and (both for upsert
/// and delete) that the id does not already belong to a non-field rule.
pub(crate) fn apply_field_rules_to_table(
    red: &mut toml::Table,
    field_rules: &serde_json::Map<String, Value>,
    changes: &mut Vec<String>,
) -> Result<(), String> {
    use duduclaw_redaction::RuleKind;

    // ── Validate everything first (no partial writes) ──
    let mut planned: Vec<(String, Option<toml::Value>)> = Vec::new();
    for (id, rule) in field_rules {
        let id_trim = id.trim();
        if !is_valid_field_rule_id(id_trim) {
            return Err(format!(
                "Invalid field_rules id '{id_trim}'. Must match ^[a-z][a-z0-9_-]{{0,63}}$"
            ));
        }
        // Never let this path touch a rule kind the editor does not own.
        if let Some(kind) = existing_rule_kind(red, id_trim)
            && !FIELD_RULE_KINDS.contains(&kind)
        {
            return Err(format!(
                "redaction.rules.{id_trim} is a '{kind}' rule — field_rules only manages db_field / json_path rules. Edit it in config.toml."
            ));
        }
        if rule.is_null() {
            planned.push((id_trim.to_string(), None));
            continue;
        }
        let Some(body) = rule.as_object() else {
            return Err(format!("field_rules.{id_trim} must be an object or null"));
        };
        // `redaction.get` renders the discriminator as `kind` (it reads better
        // in the editor); serde's tag on `RuleKind` is `type`. Accept either so
        // a rule fetched from `get` can be posted straight back, and refuse a
        // body that carries both with different values rather than silently
        // picking one.
        let mut body = body.clone();
        match (body.get("type").cloned(), body.remove("kind")) {
            (Some(t), Some(k)) if t != k => {
                return Err(format!(
                    "field_rules.{id_trim} has conflicting 'type' and 'kind' values"
                ));
            }
            (None, Some(k)) => {
                body.insert("type".into(), k);
            }
            _ => {}
        }
        let mut spec: duduclaw_redaction::RuleSpec = serde_json::from_value(Value::Object(body))
            .map_err(|e| format!("field_rules.{id_trim} is not a valid rule: {e}"))?;
        if !matches!(
            spec.kind,
            RuleKind::DbField { .. } | RuleKind::JsonPath { .. }
        ) {
            return Err(format!(
                "field_rules.{id_trim} must be type = \"db_field\" or \"json_path\""
            ));
        }
        spec.id = id_trim.to_string();
        let mut body = toml::Value::try_from(&spec)
            .map_err(|e| format!("field_rules.{id_trim} cannot be serialised: {e}"))?;
        // The id lives in the TOML key, not the body (that is where the
        // loader reads it from); carrying both invites them to disagree.
        if let Some(t) = body.as_table_mut() {
            t.remove("id");
        }
        planned.push((id_trim.to_string(), Some(body)));
    }

    // ── Commit ──
    for (id, body) in planned {
        match body {
            None => {
                // Idempotent: removing an id that is already absent still
                // reports a change, so the editor's delete never looks like a
                // failure ("No valid redaction fields to update") on a retry.
                if let Some(rules) = red.get_mut("rules").and_then(|v| v.as_table_mut()) {
                    rules.remove(&id);
                }
                changes.push(format!("redaction.rules.{id} removed"));
            }
            Some(body) => {
                let kind = body
                    .as_table()
                    .and_then(|t| t.get("type"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_string();
                let rules = red
                    .entry("rules")
                    .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                    .as_table_mut()
                    .ok_or_else(|| "Invalid [redaction.rules] section".to_string())?;
                rules.insert(id.clone(), body);
                changes.push(format!("redaction.rules.{id} = {{ type = \"{kind}\" }}"));
            }
        }
    }
    Ok(())
}
