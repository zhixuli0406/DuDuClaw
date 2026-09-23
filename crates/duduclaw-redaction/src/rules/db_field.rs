//! `db_field` — database `table.column` sugar over [`super::json_path`].
//!
//! Operators think in table columns (`res.partner.name`, `customers.email`),
//! not in JSON pointers. This module is the one-way translation: at load time
//! each `fields` entry is expanded into concrete [`RuleKind::JsonPath`] specs
//! bound to the tools that actually return that table's records. There is no
//! runtime component — the engine only ever sees JsonPath rules.
//!
//! ## Where the bindings come from
//!
//! Which tools carry a table's records, how each call's table is decided, and
//! where the records sit in the returned JSON all live in the data-source
//! registry ([`crate::data_source`]). Odoo is one registry entry among
//! others; an operator's own database is another. A rule names its registry
//! entry in `source` (`connector` is the deprecated spelling).
//!
//! An unknown source is a load-time error rather than a skipped rule: an
//! operator who typos the source name must not end up with a silently
//! unprotected column.

use std::collections::HashMap;

use crate::data_source::{DataSource, TableSource, ToolBinding};
use crate::error::{RedactionError, Result};
use crate::rules::{RuleKind, RuleSpec};

/// Source assumed when a rule names neither `source` nor `connector` — the
/// only data source that existed before the registry, so pre-2026-09 configs
/// keep their meaning.
pub const DEFAULT_SOURCE: &str = crate::data_source::BUILTIN_ODOO;

/// Keys never tokenised when a whole record is selected via `table.*`.
/// Record ids must survive so the agent can still issue follow-up
/// read/write calls against the row it was shown.
pub const WILDCARD_EXCLUDE_KEYS: &[&str] = &["id"];

/// Resolve a `db_field` rule's data-source name.
///
/// `source` wins; `connector` is accepted as its deprecated alias; giving
/// both with different values is an error rather than a silent winner.
pub fn resolve_source<'a>(
    rule_id: &str,
    source: Option<&'a str>,
    connector: Option<&'a str>,
) -> Result<&'a str> {
    match (source, connector) {
        (Some(s), Some(c)) if s != c => Err(RedactionError::rule_compile(
            rule_id,
            format!(
                "db_field rule sets both source = '{s}' and connector = '{c}' — \
                 `connector` is the deprecated spelling of `source`, so they must agree \
                 (drop `connector`)"
            ),
        )),
        (Some(s), _) => Ok(s),
        (None, Some(c)) => Ok(c),
        (None, None) => Ok(DEFAULT_SOURCE),
    }
}

/// Expand a `DbField` spec into concrete JsonPath specs.
///
/// Fails closed: an unknown source, an empty field list, or any malformed
/// `table.column` entry aborts the load instead of dropping the rule.
pub fn expand(spec: &RuleSpec, registry: &HashMap<String, DataSource>) -> Result<Vec<RuleSpec>> {
    let (source_name, fields) = match &spec.kind {
        RuleKind::DbField {
            source,
            connector,
            fields,
        } => (
            resolve_source(&spec.id, source.as_deref(), connector.as_deref())?,
            fields,
        ),
        other => {
            return Err(RedactionError::rule_compile(
                &spec.id,
                format!("expected DbField kind, got {other:?}"),
            ));
        }
    };

    let Some(source) = registry.get(source_name) else {
        let mut known: Vec<&str> = registry.keys().map(String::as_str).collect();
        known.sort_unstable();
        return Err(RedactionError::rule_compile(
            &spec.id,
            format!(
                "unknown data source '{source_name}' (known: {}) — define it under \
                 [redaction.data_sources.{source_name}]",
                known.join(", ")
            ),
        ));
    };
    if fields.is_empty() {
        return Err(RedactionError::rule_compile(
            &spec.id,
            "db_field rule needs at least one `table.column` entry",
        ));
    }

    // Entry validation is per-SOURCE, expansion is per-BINDING: the `fields`
    // list is parsed once, so a source with any free-form binding must accept
    // free-form entries. A binding that is NOT free-form then declines an
    // entry it cannot express (below) rather than emitting a path that would
    // not parse — and if every binding declines, the empty `out` below turns
    // it into a load error.
    let free_form = source.bindings.iter().any(|b| b.free_form_names);

    let mut out = Vec::new();
    for entry in fields {
        let (table, field) = split_entry(&spec.id, entry, free_form)?;
        for binding in &source.bindings {
            if !binding.free_form_names && !identifier_entry(&table, &field) {
                // An identifier-mode binding cannot carry a free-form name.
                continue;
            }
            let Some((match_args, match_result)) = binding_gates(binding, &table) else {
                continue;
            };
            let (paths, exclude_keys) = if field == "*" {
                (
                    binding.record_paths.clone(),
                    WILDCARD_EXCLUDE_KEYS
                        .iter()
                        .map(|k| k.to_string())
                        .collect::<Vec<_>>(),
                )
            } else {
                let keys = output_keys(binding, &field);
                if keys.is_empty() {
                    // This tool's mapper has no output for the column.
                    continue;
                }
                let mut paths = Vec::with_capacity(keys.len() * binding.record_paths.len());
                for key in keys {
                    for record_path in &binding.record_paths {
                        paths.push(render_path(record_path, &key, binding.free_form_names));
                    }
                }
                (paths, Vec::new())
            };

            out.push(RuleSpec {
                id: spec.id.clone(),
                category: spec.category.clone(),
                restore_scope: spec.restore_scope.clone(),
                priority: spec.priority,
                cross_session_stable: spec.cross_session_stable,
                apply_to_system_prompt: spec.apply_to_system_prompt,
                kind: RuleKind::JsonPath {
                    paths,
                    match_tool: Some(binding.tool.clone()),
                    match_args,
                    match_result,
                    exclude_keys: exclude_keys.clone(),
                },
            });
        }
    }

    if out.is_empty() {
        return Err(RedactionError::rule_compile(
            &spec.id,
            format!(
                "no tool of data source '{source_name}' returns any of {fields:?} — check \
                 the table name against the source's tool bindings"
            ),
        ));
    }
    Ok(out)
}

/// The `(match_args, match_result)` gates that bind this binding's generated
/// rule to `table`, or `None` when the binding's fixed table is a different
/// one (nothing to generate).
fn binding_gates(
    binding: &ToolBinding,
    table: &str,
) -> Option<(HashMap<String, String>, HashMap<String, String>)> {
    match &binding.table {
        TableSource::Fixed(t) => {
            if t == table {
                Some((HashMap::new(), HashMap::new()))
            } else {
                None
            }
        }
        TableSource::FromArg(arg) => {
            let mut args = HashMap::new();
            args.insert(arg.clone(), table.to_string());
            Some((args, HashMap::new()))
        }
        TableSource::FromResult(pointer) => {
            let mut result = HashMap::new();
            result.insert(pointer.clone(), table.to_string());
            Some((HashMap::new(), result))
        }
    }
}

/// Render one record path + output key into a path expression.
///
/// Identifier-mode bindings keep the historical `{rp}.{key}` spelling
/// verbatim. Free-form bindings use the quoted `{rp}['{key}']` form for any
/// key the bare `.key` grammar would reject (`地址`, `unit price`) — an
/// identifier-shaped key still renders bare, so a source that flips
/// `free_form_names` on does not churn every path it already produced.
fn render_path(record_path: &str, key: &str, free_form: bool) -> String {
    if !free_form || bare_key(key) {
        format!("{record_path}.{key}")
    } else {
        format!("{record_path}['{key}']")
    }
}

/// `^[A-Za-z0-9_-]+$` — the `json_path` bare-key charset.
fn bare_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Would this `table` / `field` pair pass the identifier validators?
/// `*` counts as a legal field (it is the wildcard, not a column name).
fn identifier_entry(table: &str, field: &str) -> bool {
    valid_table(table) && (field == "*" || valid_field(field))
}

/// Output JSON keys carrying `field` for this tool. Empty ⇒ not emitted.
fn output_keys(binding: &ToolBinding, field: &str) -> Vec<String> {
    if binding.key_alias.is_empty() {
        // Column names pass straight through.
        return vec![field.to_string()];
    }
    binding
        .key_alias
        .iter()
        .filter(|(column, _)| column == field)
        .map(|(_, out)| out.clone())
        .collect()
}

/// Split `table.column` on the **last** dot and validate both halves.
///
/// `free_form` relaxes the identifier validators for sources whose tables are
/// file names and whose columns are spreadsheet headers — see
/// [`crate::data_source::ToolBinding::free_form_names`]. The split itself is
/// unchanged either way: on the LAST dot, so `客戶清單.xlsx.地址` is the
/// column `地址` of the table `客戶清單.xlsx`.
fn split_entry(rule_id: &str, entry: &str, free_form: bool) -> Result<(String, String)> {
    let Some((table, field)) = entry.rsplit_once('.') else {
        return Err(RedactionError::rule_compile(
            rule_id,
            format!("field entry '{entry}' must be 'table.column' or 'table.*'"),
        ));
    };
    if free_form {
        if let Err(reason) = free_form_table(table) {
            return Err(RedactionError::rule_compile(
                rule_id,
                format!("invalid table name '{table}' in '{entry}' ({reason})"),
            ));
        }
        if field != "*"
            && let Err(reason) = free_form_field(field)
        {
            return Err(RedactionError::rule_compile(
                rule_id,
                format!("invalid column name '{field}' in '{entry}' ({reason})"),
            ));
        }
        return Ok((table.to_string(), field.to_string()));
    }
    if !valid_table(table) {
        return Err(RedactionError::rule_compile(
            rule_id,
            format!(
                "invalid table name '{table}' in '{entry}' \
                 (expected lowercase identifier, optionally dotted, e.g. res.partner or customers)"
            ),
        ));
    }
    if field != "*" && !valid_field(field) {
        return Err(RedactionError::rule_compile(
            rule_id,
            format!(
                "invalid column name '{field}' in '{entry}' \
                 (expected lowercase identifier or '*')"
            ),
        ));
    }
    Ok((table.to_string(), field.to_string()))
}

/// Free-form table name: non-empty, no control characters, no `'`.
///
/// `'` is refused because the table name is compared against a value the tool
/// reports, and the column beside it is spliced into a `['key']` path — one
/// permissive charset for both halves is easier to reason about than two.
fn free_form_table(table: &str) -> std::result::Result<(), &'static str> {
    if table.is_empty() {
        return Err("must not be empty");
    }
    if table.chars().any(char::is_control) {
        return Err("must not contain control characters");
    }
    if table.contains('\'') {
        return Err("must not contain a single quote");
    }
    Ok(())
}

/// Free-form column name: [`free_form_table`] plus no `/`.
///
/// The column becomes a path key and, through it, an RFC-6901 pointer
/// segment; `/` is the one character whose escaping would make the rendered
/// path and the operator's spelling disagree.
fn free_form_field(field: &str) -> std::result::Result<(), &'static str> {
    free_form_table(field)?;
    if field.contains('/') {
        return Err("must not contain '/'");
    }
    Ok(())
}

/// `^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)*$` — dots optional.
///
/// Odoo models are dotted (`res.partner`); a SQL table usually is not
/// (`customers`), and may be schema-qualified (`public.customers`). Since the
/// entry is split on its LAST dot, a dotless table is unambiguous.
fn valid_table(table: &str) -> bool {
    !table.is_empty() && table.split('.').all(valid_field)
}

/// `^[a-z][a-z0-9_]*$`.
fn valid_field(field: &str) -> bool {
    let mut bytes = field.bytes();
    match bytes.next() {
        Some(b) if b.is_ascii_lowercase() => {}
        _ => return false,
    }
    bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_source::{DataSourceDef, builtin_registry, registry};
    use crate::rules::RestoreScope;
    use std::collections::BTreeMap;

    /// The shipped registry — what every pre-registry test implicitly used.
    fn builtins() -> HashMap<String, DataSource> {
        builtin_registry()
    }

    fn db_spec(fields: &[&str], connector: &str) -> RuleSpec {
        spec_with(fields, None, Some(connector))
    }

    fn spec_with(fields: &[&str], source: Option<&str>, connector: Option<&str>) -> RuleSpec {
        RuleSpec {
            id: "customer_master".into(),
            category: "DB_FIELD".into(),
            restore_scope: RestoreScope::Owner,
            priority: 70,
            cross_session_stable: false,
            apply_to_system_prompt: false,
            kind: RuleKind::DbField {
                source: source.map(str::to_string),
                connector: connector.map(str::to_string),
                fields: fields.iter().map(|s| s.to_string()).collect(),
            },
        }
    }

    /// Custom sources: `crm_pg` (two tools, table from `arguments.table`, rows
    /// under `$.rows[*]`, column names verbatim), `crm_pg_alias` (same, but the
    /// tool renames `name` → `customer_name`) and `legacy_fixed` (one tool, a
    /// fixed table).
    fn custom_registry() -> HashMap<String, DataSource> {
        let mut key_alias = BTreeMap::new();
        key_alias.insert("name".to_string(), "customer_name".to_string());
        let mut cfg = HashMap::new();
        cfg.insert(
            "crm_pg".to_string(),
            DataSourceDef {
                label: "客戶 CRM 資料庫".into(),
                tools: vec!["pg_query".into(), "pg_select".into()],
                table_arg: Some("table".into()),
                table: None,
                table_result: None,
                record_paths: vec!["$.rows[*]".into()],
                key_alias: BTreeMap::new(),
                free_form_names: false,
            },
        );
        cfg.insert(
            "crm_pg_alias".to_string(),
            DataSourceDef {
                label: String::new(),
                tools: vec!["pg_select".into()],
                table_arg: Some("table".into()),
                table: None,
                table_result: None,
                record_paths: vec!["$.rows[*]".into()],
                key_alias,
                free_form_names: false,
            },
        );
        cfg.insert(
            "legacy_fixed".to_string(),
            DataSourceDef {
                label: String::new(),
                tools: vec!["erp_customers".into()],
                table_arg: None,
                table: Some("customers".into()),
                table_result: None,
                record_paths: vec!["$.rows[*]".into(), "$[*]".into()],
                key_alias: BTreeMap::new(),
                free_form_names: false,
            },
        );
        registry(&cfg).unwrap()
    }

    type KindParts<'a> = (
        &'a Vec<String>,
        &'a Option<String>,
        &'a std::collections::HashMap<String, String>,
        &'a Vec<String>,
    );

    fn kind_of(spec: &RuleSpec) -> KindParts<'_> {
        let (paths, tool, args, _result, excl) = kind_of_full(spec);
        (paths, tool, args, excl)
    }

    /// `kind_of` plus the `match_result` gate.
    #[allow(clippy::type_complexity)]
    fn kind_of_full(
        spec: &RuleSpec,
    ) -> (
        &Vec<String>,
        &Option<String>,
        &std::collections::HashMap<String, String>,
        &std::collections::HashMap<String, String>,
        &Vec<String>,
    ) {
        match &spec.kind {
            RuleKind::JsonPath {
                paths,
                match_tool,
                match_args,
                match_result,
                exclude_keys,
            } => (paths, match_tool, match_args, match_result, exclude_keys),
            other => panic!("expected JsonPath, got {other:?}"),
        }
    }

    #[test]
    fn res_partner_name_expands_to_search_and_partner_search() {
        let out = expand(&db_spec(&["res.partner.name"], "odoo"), &builtins()).unwrap();
        let tools: Vec<String> = out.iter().map(|s| kind_of(s).1.clone().unwrap()).collect();
        assert!(tools.contains(&"odoo_search".to_string()));
        assert!(tools.contains(&"odoo_partner_search".to_string()));
        assert!(tools.contains(&"odoo_execute".to_string()));
        // No fixed-model tool other than partner_search is bound to res.partner.
        assert!(!tools.contains(&"odoo_crm_leads".to_string()));

        // The dynamic-model tools carry the model arg gate; the fixed one doesn't.
        for spec in &out {
            let (paths, tool, args, excl) = kind_of(spec);
            assert_eq!(paths, &vec!["$[*].name".to_string(), "$.name".to_string()]);
            assert!(excl.is_empty());
            match tool.as_deref() {
                Some("odoo_search") | Some("odoo_execute") => {
                    assert_eq!(args.get("model").map(String::as_str), Some("res.partner"));
                }
                Some("odoo_partner_search") => assert!(args.is_empty()),
                other => panic!("unexpected tool {other:?}"),
            }
        }
    }

    #[test]
    fn wildcard_excludes_id() {
        let out = expand(&db_spec(&["hr.employee.*"], "odoo"), &builtins()).unwrap();
        assert!(!out.is_empty());
        for spec in &out {
            let (paths, _, _, excl) = kind_of(spec);
            assert_eq!(paths, &vec!["$[*]".to_string(), "$".to_string()]);
            assert_eq!(excl, &vec!["id".to_string()]);
        }
        // hr.employee is not a fixed-model tool, so only the dynamic pair binds.
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn mapper_tool_uses_key_alias() {
        // crm.lead.email_from is renamed to `email` by crm::map_crm_lead.
        let out = expand(&db_spec(&["crm.lead.email_from"], "odoo"), &builtins()).unwrap();
        let leads: Vec<&RuleSpec> = out
            .iter()
            .filter(|s| kind_of(s).1.as_deref() == Some("odoo_crm_leads"))
            .collect();
        assert_eq!(leads.len(), 1);
        assert_eq!(
            kind_of(leads[0]).0,
            &vec!["$[*].email".to_string(), "$.email".to_string()]
        );

        // The generic tools keep the native name.
        let generic: Vec<&RuleSpec> = out
            .iter()
            .filter(|s| kind_of(s).1.as_deref() == Some("odoo_search"))
            .collect();
        assert_eq!(
            kind_of(generic[0]).0,
            &vec!["$[*].email_from".to_string(), "$.email_from".to_string()]
        );
    }

    #[test]
    fn mapper_split_field_yields_both_output_keys() {
        // crm.lead.stage_id becomes `stage` (display name) + `stage_id` (id).
        let out = expand(&db_spec(&["crm.lead.stage_id"], "odoo"), &builtins()).unwrap();
        let leads = out
            .iter()
            .find(|s| kind_of(s).1.as_deref() == Some("odoo_crm_leads"))
            .unwrap();
        assert_eq!(
            kind_of(leads).0,
            &vec![
                "$[*].stage".to_string(),
                "$.stage".to_string(),
                "$[*].stage_id".to_string(),
                "$.stage_id".to_string(),
            ]
        );
    }

    #[test]
    fn field_the_mapper_drops_yields_no_rule_for_that_tool() {
        // sale.order.order_line only becomes a count in the mapper.
        let out = expand(&db_spec(&["sale.order.order_line"], "odoo"), &builtins()).unwrap();
        assert!(
            !out.iter()
                .any(|s| kind_of(s).1.as_deref() == Some("odoo_sale_orders")),
            "a dropped mapper field must not produce a rule for that tool"
        );
        // The generic model-from-args tools still cover it.
        assert!(
            out.iter()
                .any(|s| kind_of(s).1.as_deref() == Some("odoo_search"))
        );
    }

    #[test]
    fn invoice_shared_model_binds_both_tools() {
        let out = expand(&db_spec(&["account.move.partner_id"], "odoo"), &builtins()).unwrap();
        let tools: Vec<String> = out.iter().map(|s| kind_of(s).1.clone().unwrap()).collect();
        assert!(tools.contains(&"odoo_invoice_list".to_string()));
        assert!(tools.contains(&"odoo_payment_status".to_string()));
        for spec in out
            .iter()
            .filter(|s| kind_of(s).1.as_deref() == Some("odoo_invoice_list"))
        {
            assert_eq!(
                kind_of(spec).0,
                &vec!["$[*].partner".to_string(), "$.partner".to_string()]
            );
        }
    }

    #[test]
    fn unknown_connector_is_an_error() {
        let err = expand(&db_spec(&["res.partner.name"], "dingxin"), &builtins()).unwrap_err();
        assert!(matches!(err, RedactionError::RuleCompile { .. }));
        assert!(err.to_string().contains("dingxin"));
        // The message names the sources that DO exist so the fix is obvious.
        assert!(err.to_string().contains("odoo"), "{err}");
    }

    #[test]
    fn unknown_source_is_an_error() {
        let err = expand(
            &spec_with(&["customers.name"], Some("crm_pg"), None),
            &builtins(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("crm_pg"), "{err}");
    }

    #[test]
    fn custom_source_from_arg_binds_every_tool() {
        let out = expand(
            &spec_with(&["customers.email"], Some("crm_pg"), None),
            &custom_registry(),
        )
        .unwrap();
        assert_eq!(out.len(), 2, "one rule per bound tool");
        let mut tools: Vec<String> = out.iter().map(|s| kind_of(s).1.clone().unwrap()).collect();
        tools.sort();
        assert_eq!(tools, ["pg_query", "pg_select"]);
        for spec in &out {
            let (paths, _, args, excl) = kind_of(spec);
            assert_eq!(paths, &vec!["$.rows[*].email".to_string()]);
            assert_eq!(args.get("table").map(String::as_str), Some("customers"));
            assert!(excl.is_empty());
        }
    }

    #[test]
    fn custom_source_fixed_table_only_binds_its_own_table() {
        let reg = custom_registry();
        // The fixed-table source matches its own table …
        let out = expand(
            &spec_with(&["customers.email"], Some("legacy_fixed"), None),
            &reg,
        )
        .unwrap();
        assert_eq!(out.len(), 1);
        let (paths, tool, args, _) = kind_of(&out[0]);
        assert_eq!(tool.as_deref(), Some("erp_customers"));
        assert!(args.is_empty(), "a fixed table needs no arg gate");
        assert_eq!(
            paths,
            &vec![
                "$.rows[*].email".to_string(),
                "$[*].email".to_string(),
            ]
        );

        // … and refuses to bind a different one.
        let err = expand(&spec_with(&["orders.total"], Some("legacy_fixed"), None), &reg)
            .unwrap_err();
        assert!(err.to_string().contains("orders.total"), "{err}");
    }

    #[test]
    fn custom_source_key_alias_maps_column_to_returned_key() {
        // Direction check: the COLUMN is `name`, the RETURNED KEY is
        // `customer_name`, so the generated path must carry the returned key.
        let out = expand(
            &spec_with(&["customers.name"], Some("crm_pg_alias"), None),
            &custom_registry(),
        )
        .unwrap();
        for spec in &out {
            assert_eq!(
                kind_of(spec).0,
                &vec!["$.rows[*].customer_name".to_string()],
                "key_alias maps column → returned key, not the other way round"
            );
        }
        // A column the alias table does not mention yields nothing for this
        // source (the tool does not return it under that name).
        let err = expand(
            &spec_with(&["customers.unmapped"], Some("crm_pg_alias"), None),
            &custom_registry(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("unmapped"), "{err}");
    }

    #[test]
    fn duduclaw_db_binds_db_select_only() {
        let out = expand(
            &spec_with(&["customers.email"], Some("duduclaw_db"), None),
            &builtins(),
        )
        .unwrap();
        assert_eq!(out.len(), 1);
        let (paths, tool, args, _) = kind_of(&out[0]);
        assert_eq!(tool.as_deref(), Some("db_select"));
        assert_eq!(args.get("table").map(String::as_str), Some("customers"));
        assert_eq!(paths, &vec!["$.rows[*].email".to_string()]);
    }

    // ── free-form names + FromResult (`duduclaw_files`) ──────────────────

    #[test]
    fn files_source_gates_on_the_result_not_the_args() {
        let out = expand(
            &spec_with(&["customers.csv.name"], Some("duduclaw_files"), None),
            &builtins(),
        )
        .unwrap();
        // One rule per reader; both gate on the table the RESULT reports.
        assert_eq!(out.len(), 2);
        let mut tools: Vec<String> = out.iter().map(|s| kind_of(s).1.clone().unwrap()).collect();
        tools.sort();
        assert_eq!(tools, ["csv_read", "xlsx_read"]);
        for spec in &out {
            let (paths, _, args, result, excl) = kind_of_full(spec);
            assert_eq!(paths, &vec!["$.rows[*].name".to_string()]);
            assert!(args.is_empty(), "the table is not an argument here");
            assert_eq!(result.get("/table").map(String::as_str), Some("customers.csv"));
            assert!(excl.is_empty());
        }
    }

    #[test]
    fn free_form_cjk_header_renders_the_quoted_path_form() {
        let out = expand(
            &spec_with(&["客戶清單.xlsx.地址"], Some("duduclaw_files"), None),
            &builtins(),
        )
        .unwrap();
        for spec in &out {
            let (paths, _, _, result, _) = kind_of_full(spec);
            assert_eq!(paths, &vec!["$.rows[*]['地址']".to_string()]);
            assert_eq!(
                result.get("/table").map(String::as_str),
                Some("客戶清單.xlsx"),
                "the entry splits on its LAST dot, so the extension stays with the table"
            );
            // And the rendered path really parses.
            crate::rules::JsonPathRule::compile((*spec).clone()).unwrap();
        }
    }

    #[test]
    fn free_form_identifier_keys_still_render_bare() {
        // Flipping a source to free-form must not churn the paths it already
        // produced for identifier-shaped columns.
        let out = expand(
            &spec_with(&["customers.csv.email"], Some("duduclaw_files"), None),
            &builtins(),
        )
        .unwrap();
        assert!(
            out.iter()
                .all(|s| kind_of(s).0 == &vec!["$.rows[*].email".to_string()])
        );
    }

    #[test]
    fn free_form_wildcard_keeps_the_record_paths_and_id_exclusion() {
        let out = expand(
            &spec_with(&["客戶清單.xlsx.*"], Some("duduclaw_files"), None),
            &builtins(),
        )
        .unwrap();
        for spec in &out {
            let (paths, _, _, result, excl) = kind_of_full(spec);
            assert_eq!(paths, &vec!["$.rows[*]".to_string()]);
            assert_eq!(excl, &vec!["id".to_string()]);
            assert_eq!(result.get("/table").map(String::as_str), Some("客戶清單.xlsx"));
        }
    }

    #[test]
    fn free_form_refuses_control_characters_quotes_and_slashes() {
        for bad in [
            "客戶清單.xlsx.地\u{0}址", // control character in the column
            "客戶清\u{7}單.xlsx.地址", // control character in the table
            "客戶清單.xlsx.it's",      // `'` would break the ['key'] form
            "客戶'清單.xlsx.地址",
            "客戶清單.xlsx.a/b", // `/` is a pointer separator
            "客戶清單.xlsx.",    // empty column
            ".地址",             // empty table
        ] {
            assert!(
                expand(&spec_with(&[bad], Some("duduclaw_files"), None), &builtins()).is_err(),
                "expected error for {bad:?}"
            );
        }
    }

    #[test]
    fn identifier_sources_still_refuse_free_form_entries() {
        // The relaxation is per-source: a database source must keep rejecting
        // a spreadsheet-shaped entry at load time.
        for source in ["odoo", "duduclaw_db"] {
            let err = expand(
                &spec_with(&["客戶清單.xlsx.地址"], Some(source), None),
                &builtins(),
            )
            .unwrap_err();
            assert!(err.to_string().contains("客戶清單"), "{err}");
        }
    }

    #[test]
    fn deprecated_connector_alias_still_resolves() {
        let via_connector = expand(&db_spec(&["res.partner.name"], "odoo"), &builtins()).unwrap();
        let via_source = expand(
            &spec_with(&["res.partner.name"], Some("odoo"), None),
            &builtins(),
        )
        .unwrap();
        assert_eq!(via_connector.len(), via_source.len());
        for (a, b) in via_connector.iter().zip(via_source.iter()) {
            assert_eq!(kind_of(a), kind_of(b));
        }
        // Omitting both keeps the historical default.
        let implicit = expand(&spec_with(&["res.partner.name"], None, None), &builtins()).unwrap();
        assert_eq!(implicit.len(), via_source.len());
    }

    #[test]
    fn source_and_connector_must_agree() {
        // Same value on both keys is harmless.
        assert!(
            expand(
                &spec_with(&["res.partner.name"], Some("odoo"), Some("odoo")),
                &builtins()
            )
            .is_ok()
        );
        // Disagreeing values are an error — never a silent winner.
        let err = expand(
            &spec_with(&["customers.name"], Some("crm_pg"), Some("odoo")),
            &custom_registry(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("crm_pg"), "{err}");
        assert!(err.to_string().contains("odoo"), "{err}");
    }

    #[test]
    fn a_config_source_may_not_shadow_a_builtin() {
        let mut cfg = HashMap::new();
        cfg.insert(
            "odoo".to_string(),
            DataSourceDef {
                label: "mine".into(),
                tools: vec!["pg_select".into()],
                table_arg: Some("table".into()),
                table: None,
                table_result: None,
                record_paths: Vec::new(),
                key_alias: BTreeMap::new(),
                free_form_names: false,
            },
        );
        let err = registry(&cfg).unwrap_err();
        assert!(matches!(err, RedactionError::RuleCompile { .. }));
        assert!(err.to_string().contains("built-in"), "{err}");
    }

    #[test]
    fn a_malformed_record_path_fails_the_registry() {
        let mut cfg = HashMap::new();
        cfg.insert(
            "crm_pg".to_string(),
            DataSourceDef {
                label: String::new(),
                tools: vec!["pg_select".into()],
                table_arg: Some("table".into()),
                table: None,
                table_result: None,
                record_paths: vec!["rows[*]".into()], // missing the leading `$`
                key_alias: BTreeMap::new(),
                free_form_names: false,
            },
        );
        let err = registry(&cfg).unwrap_err();
        assert!(err.to_string().contains("rows[*]"), "{err}");
    }

    #[test]
    fn malformed_entries_are_errors() {
        for bad in [
            "res_partner",      // no dot at all
            "res.partner.Name", // uppercase column
            "res.Partner.name", // uppercase table part
            "res.partner.",     // empty column
            ".name",            // empty table
            "res.partner.na me",
        ] {
            assert!(
                expand(&db_spec(&[bad], "odoo"), &builtins()).is_err(),
                "expected error for {bad:?}"
            );
        }
    }

    #[test]
    fn empty_fields_list_is_an_error() {
        assert!(expand(&db_spec(&[], "odoo"), &builtins()).is_err());
    }

    #[test]
    fn expanded_specs_inherit_parent_metadata() {
        let parent = db_spec(&["res.partner.name"], "odoo");
        let out = expand(&parent, &builtins()).unwrap();
        for spec in &out {
            assert_eq!(spec.id, parent.id);
            assert_eq!(spec.category, parent.category);
            assert_eq!(spec.priority, parent.priority);
            assert_eq!(spec.restore_scope, parent.restore_scope);
            assert_eq!(spec.cross_session_stable, parent.cross_session_stable);
        }
    }

    #[test]
    fn wrong_kind_is_an_error() {
        let mut s = db_spec(&["res.partner.name"], "odoo");
        s.kind = RuleKind::Regex {
            pattern: "x".into(),
        };
        assert!(expand(&s, &builtins()).is_err());
    }

    #[test]
    fn table_validator_matches_the_documented_shape() {
        assert!(valid_table("res.partner"));
        assert!(valid_table("hr.employee.private"));
        assert!(valid_table("x_custom_1.line_2"));
        // Relaxed in 2026-09 for SQL sources: a table need not be dotted.
        assert!(valid_table("customers"));
        assert!(valid_table("public.customers"));
        assert!(!valid_table("Res.Partner"));
        assert!(!valid_table("res..partner"));
        assert!(!valid_table("1res.partner"));
        assert!(!valid_table(""));
    }

    #[test]
    fn every_expanded_path_compiles() {
        // Guards against a key_alias entry that isn't a legal path key.
        let mut entries: Vec<String> = Vec::new();
        for source in crate::data_source::builtin_sources() {
            for binding in &source.bindings {
                if let TableSource::Fixed(table) = &binding.table {
                    for (column, _) in &binding.key_alias {
                        entries.push(format!("{table}.{column}"));
                    }
                }
            }
        }
        entries.push("res.partner.*".into());
        let refs: Vec<&str> = entries.iter().map(String::as_str).collect();
        let out = expand(&db_spec(&refs, "odoo"), &builtins()).unwrap();
        for spec in out {
            crate::rules::JsonPathRule::compile(spec).expect("expanded spec must compile");
        }
    }
}
