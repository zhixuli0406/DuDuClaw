//! Data-source registry — which tools return records from which table, and
//! where those records sit inside the tool's JSON result.
//!
//! `db_field` rules speak in table columns (`res.partner.name`,
//! `customers.email`). Turning a column into a concrete
//! [`crate::rules::RuleKind::JsonPath`] needs three facts the rule itself
//! does not carry:
//!
//! 1. **which tools** return rows of that table,
//! 2. **how the table is decided** for a given call — a fixed table per tool,
//!    a tool argument naming it, or a pointer into the tool's own result,
//! 3. **where the records live** in the returned JSON, and whether the JSON
//!    keys still carry the column names.
//!
//! Until 2026-09 those three facts existed only as a hard-coded Odoo table,
//! so `db_field` could protect Odoo and nothing else. They now live in this
//! registry: Odoo is one entry, the native SQL connector is another, and an
//! operator adds their own with a `[redaction.data_sources.<name>]` block:
//!
//! ```toml
//! [redaction.data_sources.crm_pg]
//! label = "客戶 CRM 資料庫"
//! tools = ["pg_query", "pg_select"]     # ≥1; exact name or trailing-`*` glob
//! table_arg = "table"                   # exactly one of table_arg / table /
//!                                       # table_result = "/table"
//! record_paths = ["$.rows[*]", "$[*]", "$"]
//! key_alias = { name = "customer_name" }  # column = returned key
//! free_form_names = false                 # true ⇒ CJK / dotted table & column names
//! ```
//!
//! Every problem here is a **load-time** error, never a skipped source: an
//! operator who typos a source name or a record path must not end up with a
//! column they believe is masked and isn't.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::error::{RedactionError, Result};
use crate::rules::json_path::JsonPath;

/// Record locations used when a source does not name its own. Covers the two
/// shapes tool results actually take: a `{"rows": [...]}` envelope, a bare
/// record array, and a single record object.
pub const DEFAULT_RECORD_PATHS: &[&str] = &["$.rows[*]", "$[*]", "$"];

/// Built-in source: the Odoo MCP tools (`odoo_*`).
pub const BUILTIN_ODOO: &str = "odoo";

/// Built-in source: DuDuClaw's own SQL connector (`db_*` MCP tools).
pub const BUILTIN_DUDUCLAW_DB: &str = "duduclaw_db";

/// Built-in source: DuDuClaw's local data-file readers (`csv_read` /
/// `xlsx_read`).
pub const BUILTIN_DUDUCLAW_FILES: &str = "duduclaw_files";

/// Names an operator may not define — a config entry using one is a load
/// error rather than a silent override, because shadowing `odoo` with a
/// half-specified copy would quietly unbind columns that used to be masked.
pub const BUILTIN_SOURCE_NAMES: &[&str] =
    &[BUILTIN_ODOO, BUILTIN_DUDUCLAW_DB, BUILTIN_DUDUCLAW_FILES];

/// How a binding decides which table a given call returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableSource {
    /// The table is whatever this top-level tool argument says. The generated
    /// rule carries a `match_args` gate on that argument.
    FromArg(String),
    /// The tool always reads one table. The binding only produces rules when
    /// the rule's table equals this one.
    Fixed(String),
    /// The table is named by the tool's own **result**, at this JSON pointer.
    /// The generated rule carries a `match_result` gate on that pointer.
    ///
    /// This is how a file reader binds: `csv_read {path}` is asked for a path,
    /// not a table, and answers `{"table": "customers.csv", "rows": […]}` — so
    /// the only place the table name exists is the result itself.
    FromResult(String),
}

/// One tool that returns records of a data source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolBinding {
    /// Tool name — exact, or a trailing-`*` prefix glob (the `match_tool`
    /// semantics of [`crate::rules::RuleKind::JsonPath`]).
    pub tool: String,
    /// How this tool's table is decided.
    pub table: TableSource,
    /// Where records sit in the returned JSON.
    pub record_paths: Vec<String>,
    /// `(column → returned key)` pairs for tools whose result went through a
    /// mapper that renames columns. Empty ⇒ the result carries column names
    /// verbatim. A column may appear more than once when the mapper splits it
    /// into several keys.
    pub key_alias: Vec<(String, String)>,
    /// Accept table and column names that are not SQL identifiers.
    ///
    /// `false` (the default) keeps the historical `^[a-z][a-z0-9_]*$`
    /// validators: a database source's names really are identifiers, and a
    /// typo caught at load time is worth more than a permissive rule.
    /// `true` is for sources whose "tables" are file names and whose "columns"
    /// are spreadsheet headers (`客戶清單.xlsx` / `地址`) — there the only
    /// rules that can hold are non-empty, no control characters, no `'`
    /// (it would break the `['key']` path form) and no `/` in a column.
    pub free_form_names: bool,
}

/// A registered data source: a label plus the tools that carry its records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataSource {
    /// Registry key, also the value a rule's `source` names.
    pub name: String,
    /// Operator-facing label (dashboard).
    pub label: String,
    /// `true` for the shipped sources — not definable or removable.
    pub builtin: bool,
    /// One binding per tool.
    pub bindings: Vec<ToolBinding>,
}

impl DataSource {
    /// Collapse this source back into the simple TOML form for display.
    ///
    /// Built-in sources can have per-tool tables and aliases that the simple
    /// form cannot express (Odoo does). Rather than inventing a summary that
    /// reads as fact, a field the bindings disagree on comes back empty:
    /// `table` / `table_arg` / `table_result` are `None` unless every binding
    /// agrees on the same one, and `key_alias` is empty unless every binding
    /// carries the same table.
    pub fn simple_form(&self) -> DataSourceDef {
        let tools: Vec<String> = {
            let mut seen: Vec<String> = Vec::new();
            for b in &self.bindings {
                if !seen.iter().any(|t| t == &b.tool) {
                    seen.push(b.tool.clone());
                }
            }
            seen
        };

        // Every binding must carry the SAME table source for the simple form
        // to describe it; anything else is reported as "unknown" rather than
        // as a summary that reads like fact.
        let mut distinct: Vec<&TableSource> = Vec::new();
        for b in &self.bindings {
            if !distinct.contains(&&b.table) {
                distinct.push(&b.table);
            }
        }
        let (table_arg, table, table_result) = match distinct.as_slice() {
            [TableSource::FromArg(a)] => (Some(a.clone()), None, None),
            [TableSource::Fixed(t)] => (None, Some(t.clone()), None),
            [TableSource::FromResult(p)] => (None, None, Some(p.clone())),
            _ => (None, None, None),
        };

        // record_paths: the shared list when every binding agrees, else the
        // union in first-seen order (that IS the set of places records can be).
        let mut record_paths: Vec<String> = Vec::new();
        for b in &self.bindings {
            for p in &b.record_paths {
                if !record_paths.iter().any(|x| x == p) {
                    record_paths.push(p.clone());
                }
            }
        }

        let alias_agree = self
            .bindings
            .windows(2)
            .all(|w| w[0].key_alias == w[1].key_alias);
        let key_alias: BTreeMap<String, String> = if alias_agree {
            self.bindings
                .first()
                .map(|b| b.key_alias.iter().cloned().collect())
                .unwrap_or_default()
        } else {
            BTreeMap::new()
        };

        // A bool has no "unknown", so the honest summary is the conservative
        // one: free-form only when every binding is free-form.
        let free_form_names =
            !self.bindings.is_empty() && self.bindings.iter().all(|b| b.free_form_names);

        DataSourceDef {
            label: self.label.clone(),
            tools,
            table_arg,
            table,
            table_result,
            record_paths,
            key_alias,
            free_form_names,
        }
    }
}

/// The simple TOML form of a data source: one shared description that expands
/// into one [`ToolBinding`] per tool.
///
/// This is what `[redaction.data_sources.<name>]` deserialises into and what
/// the dashboard writes back. Sources whose tools need *different* tables or
/// aliases (Odoo) are built-ins expressed directly as bindings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataSourceDef {
    /// Operator-facing label. Empty ⇒ the source name is used.
    #[serde(default)]
    pub label: String,

    /// Tools that return this source's records. Exact names or trailing-`*`
    /// globs. Empty ⇒ load error (a source nothing is bound to protects
    /// nothing, and looks like it does).
    #[serde(default)]
    pub tools: Vec<String>,

    /// Name of the tool argument carrying the table. Exactly one of
    /// `table_arg` / [`Self::table`] / [`Self::table_result`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table_arg: Option<String>,

    /// Fixed table for every bound tool. Exactly one of
    /// [`Self::table_arg`] / `table` / [`Self::table_result`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table: Option<String>,

    /// JSON pointer into the tool **result** naming the table (`"/table"`).
    /// Exactly one of [`Self::table_arg`] / [`Self::table`] / `table_result`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table_result: Option<String>,

    /// Where records sit in the returned JSON. Empty ⇒ [`DEFAULT_RECORD_PATHS`].
    #[serde(default)]
    pub record_paths: Vec<String>,

    /// Accept non-identifier table and column names — see
    /// [`ToolBinding::free_form_names`]. Default `false`.
    #[serde(default)]
    pub free_form_names: bool,

    /// `column = "returned key"` — only needed when the tool renames columns.
    #[serde(default)]
    pub key_alias: BTreeMap<String, String>,
}

impl DataSourceDef {
    /// Expand into a [`DataSource`], one binding per tool.
    ///
    /// Fails closed on: no tools, a table that is decided by none or by more
    /// than one of `table_arg` / `table` / `table_result`, a `table_result`
    /// that is not a JSON pointer, and any record path the [`JsonPath`]
    /// grammar rejects.
    pub fn into_source(self, name: &str) -> Result<DataSource> {
        let err = |reason: String| {
            RedactionError::rule_compile(format!("redaction.data_sources.{name}"), reason)
        };

        if self.tools.is_empty() {
            return Err(err(format!(
                "data source '{name}' lists no tools — `tools` needs at least one entry"
            )));
        }
        for tool in &self.tools {
            if tool.trim().is_empty() {
                return Err(err(format!(
                    "data source '{name}' has an empty tool name in `tools`"
                )));
            }
        }

        // Exactly one of the three ways a table can be decided. The error text
        // names all three so an operator who set none, or two, sees the whole
        // menu rather than the half the code happened to check first.
        let mut given: Vec<&str> = Vec::new();
        if self.table_arg.is_some() {
            given.push("table_arg");
        }
        if self.table.is_some() {
            given.push("table");
        }
        if self.table_result.is_some() {
            given.push("table_result");
        }
        if given.len() != 1 {
            let what = if given.is_empty() {
                "sets none of them".to_string()
            } else {
                format!("sets {}", given.join(" and "))
            };
            return Err(err(format!(
                "data source '{name}' needs exactly one of `table_arg` (which tool argument \
                 names the table), `table` (a fixed table) or `table_result` (a JSON pointer \
                 into the result naming the table) — it {what}"
            )));
        }

        let table = if let Some(arg) = &self.table_arg {
            if arg.trim().is_empty() {
                return Err(err(format!(
                    "data source '{name}' has an empty `table_arg`"
                )));
            }
            TableSource::FromArg(arg.clone())
        } else if let Some(t) = &self.table {
            if t.trim().is_empty() {
                return Err(err(format!("data source '{name}' has an empty `table`")));
            }
            TableSource::Fixed(t.clone())
        } else {
            // `given.len() == 1` above, so this is the `table_result` case.
            let ptr = self.table_result.as_deref().unwrap_or_default();
            if !ptr.starts_with('/') {
                return Err(err(format!(
                    "data source '{name}' `table_result` must be a JSON pointer starting with \
                     '/' (e.g. \"/table\"), got '{ptr}'"
                )));
            }
            TableSource::FromResult(ptr.to_string())
        };

        let record_paths = if self.record_paths.is_empty() {
            default_record_paths()
        } else {
            self.record_paths.clone()
        };
        for path in &record_paths {
            JsonPath::parse(path).map_err(|reason| {
                err(format!(
                    "data source '{name}' record path '{path}' is invalid: {reason}"
                ))
            })?;
        }

        let key_alias: Vec<(String, String)> = self
            .key_alias
            .iter()
            .map(|(column, out)| (column.clone(), out.clone()))
            .collect();

        let bindings = self
            .tools
            .iter()
            .map(|tool| ToolBinding {
                tool: tool.clone(),
                table: table.clone(),
                record_paths: record_paths.clone(),
                key_alias: key_alias.clone(),
                free_form_names: self.free_form_names,
            })
            .collect();

        let label = if self.label.trim().is_empty() {
            name.to_string()
        } else {
            self.label.clone()
        };

        Ok(DataSource {
            name: name.to_string(),
            label,
            builtin: false,
            bindings,
        })
    }
}

/// [`DEFAULT_RECORD_PATHS`] as owned strings.
pub fn default_record_paths() -> Vec<String> {
    DEFAULT_RECORD_PATHS
        .iter()
        .map(|p| (*p).to_string())
        .collect()
}

/// Is `name` one of the shipped sources?
pub fn is_builtin_source(name: &str) -> bool {
    BUILTIN_SOURCE_NAMES.contains(&name)
}

/// Accepted charset for an operator-defined source name:
/// `^[a-z][a-z0-9_-]{0,63}$`. Checked by hand rather than by regex — the name
/// becomes a TOML key and a rule reference, so the accepted set stays small
/// and explicit.
pub fn is_valid_data_source_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 64 {
        return false;
    }
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// The shipped sources.
pub fn builtin_sources() -> Vec<DataSource> {
    vec![odoo_source(), duduclaw_db_source(), duduclaw_files_source()]
}

/// [`builtin_sources`] keyed by name.
pub fn builtin_registry() -> HashMap<String, DataSource> {
    builtin_sources()
        .into_iter()
        .map(|s| (s.name.clone(), s))
        .collect()
}

/// Build the full registry: built-ins plus the operator's
/// `[redaction.data_sources.*]` entries.
///
/// A config entry reusing a built-in name is an error, not an override.
pub fn registry(
    config_sources: &HashMap<String, DataSourceDef>,
) -> Result<HashMap<String, DataSource>> {
    let mut out = builtin_registry();
    // Deterministic order so the first failure reported for a broken config is
    // always the same one.
    let mut names: Vec<&String> = config_sources.keys().collect();
    names.sort();
    for name in names {
        if is_builtin_source(name) {
            return Err(RedactionError::rule_compile(
                format!("redaction.data_sources.{name}"),
                format!(
                    "'{name}' is a built-in data source and cannot be redefined — \
                     pick another name"
                ),
            ));
        }
        let def = config_sources[name].clone();
        out.insert(name.clone(), def.into_source(name)?);
    }
    Ok(out)
}

// ── Built-in: Odoo ──────────────────────────────────────────────────────────
//
// Derived from what `crates/duduclaw-cli/src/mcp.rs` actually does in its
// `handle_odoo_*` dispatch, not from the Odoo schema:
//
// - `odoo_search` / `odoo_execute` take the model from `arguments.model` and
//   pretty-print the raw `search_read` / `execute_kw` payload, so the JSON
//   keys are **native Odoo field names**.
// - `odoo_partner_search` is fixed to `res.partner` and also returns raw
//   `search_read` rows (projection `PARTNER_SEARCH_FIELDS`).
// - The remaining tools run the row through a mapper struct in
//   `duduclaw-odoo/src/models/`, which renames and sometimes drops fields —
//   those bindings therefore carry an explicit `key_alias` table.
//
// Fields a mapper does not emit are **not expanded** (see the per-binding
// comments); an operator asking for such a column gets no rule from that tool
// rather than a rule that matches nothing under a misleading name.

/// Where an Odoo tool's model comes from (static twin of [`TableSource`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelSource {
    Fixed(&'static str),
    FromArg(&'static str),
}

/// One Odoo MCP tool that returns records.
struct OdooToolBinding {
    tool: &'static str,
    model: ModelSource,
    key_alias: &'static [(&'static str, &'static str)],
}

/// Odoo record shapes: an array of records, or a single record object.
const ODOO_RECORD_PATHS: &[&str] = &["$[*]", "$"];

const ODOO_TOOLS: &[OdooToolBinding] = &[
    // Raw `search_read` output — native Odoo field names, model from args.
    OdooToolBinding {
        tool: "odoo_search",
        model: ModelSource::FromArg("model"),
        key_alias: &[],
    },
    // Raw `execute_kw` output. For read/search_read methods the payload is a
    // record array with native field names; other methods return whatever the
    // Odoo method returns, in which case the paths simply miss.
    OdooToolBinding {
        tool: "odoo_execute",
        model: ModelSource::FromArg("model"),
        key_alias: &[],
    },
    // Raw `search_read` over res.partner with the fixed PARTNER_SEARCH_FIELDS
    // projection (connector.rs). Native field names.
    //
    // NOT REACHABLE HERE: any res.partner column outside the projection
    // (street, comment, vat, …) — the tool never returns it. Those columns are
    // still covered through `odoo_search` with `model = res.partner`.
    OdooToolBinding {
        tool: "odoo_partner_search",
        model: ModelSource::Fixed("res.partner"),
        key_alias: &[],
    },
    // crm.lead → crm::CrmLead (models/crm.rs). Every CRM_LEAD_FIELDS entry has
    // an output; `stage_id` splits into the display name and the numeric id.
    OdooToolBinding {
        tool: "odoo_crm_leads",
        model: ModelSource::Fixed("crm.lead"),
        key_alias: &[
            ("id", "id"),
            ("name", "name"),
            ("contact_name", "contact_name"),
            ("email_from", "email"),
            ("phone", "phone"),
            ("stage_id", "stage"),
            ("stage_id", "stage_id"),
            ("expected_revenue", "expected_revenue"),
            ("probability", "probability"),
            // many2one → display name only; the numeric id is dropped by the
            // mapper, so redacting these hides the person/team name.
            ("user_id", "salesperson"),
            ("team_id", "team"),
            ("type", "lead_type"),
        ],
    },
    // sale.order → sale::SaleOrder (models/sale.rs).
    //
    // NOT COVERED: `order_line` — the mapper emits only `line_count` (a count,
    // not the line data), so there is nothing field-shaped to tokenise.
    OdooToolBinding {
        tool: "odoo_sale_orders",
        model: ModelSource::Fixed("sale.order"),
        key_alias: &[
            ("id", "id"),
            ("name", "name"),
            ("partner_id", "customer"),
            ("date_order", "date"),
            ("state", "status"),
            ("amount_total", "total"),
            ("user_id", "salesperson"),
        ],
    },
    // product.product → inventory::Product (models/inventory.rs). Complete.
    OdooToolBinding {
        tool: "odoo_inventory_products",
        model: ModelSource::Fixed("product.product"),
        key_alias: &[
            ("id", "id"),
            ("name", "name"),
            ("default_code", "default_code"),
            ("list_price", "list_price"),
            ("qty_available", "qty_available"),
            ("virtual_available", "virtual_available"),
            ("detailed_type", "product_type"),
        ],
    },
    // stock.quant → inventory::StockQuant (models/inventory.rs).
    //
    // NOT COVERED: `id` — STOCK_QUANT_FIELDS never requests it and the mapper
    // has no id field, so `stock.quant.id` yields no rule (and the `*` form's
    // id exclusion is a no-op here).
    OdooToolBinding {
        tool: "odoo_inventory_check",
        model: ModelSource::Fixed("stock.quant"),
        key_alias: &[
            ("product_id", "product"),
            ("location_id", "location"),
            ("quantity", "quantity"),
            ("reserved_quantity", "reserved"),
        ],
    },
    // account.move → accounting::Invoice (models/accounting.rs). Complete.
    // Two tools share the model: a list (array) and a single lookup (object).
    OdooToolBinding {
        tool: "odoo_invoice_list",
        model: ModelSource::Fixed("account.move"),
        key_alias: INVOICE_KEY_ALIAS,
    },
    OdooToolBinding {
        tool: "odoo_payment_status",
        model: ModelSource::Fixed("account.move"),
        key_alias: INVOICE_KEY_ALIAS,
    },
];

const INVOICE_KEY_ALIAS: &[(&str, &str)] = &[
    ("id", "id"),
    ("name", "number"),
    ("partner_id", "partner"),
    ("move_type", "move_type"),
    ("state", "status"),
    ("amount_total", "total"),
    ("amount_residual", "balance_due"),
    ("payment_state", "payment_status"),
    ("invoice_date", "date"),
];

fn odoo_source() -> DataSource {
    DataSource {
        name: BUILTIN_ODOO.to_string(),
        label: "Odoo ERP".to_string(),
        builtin: true,
        bindings: ODOO_TOOLS
            .iter()
            .map(|b| ToolBinding {
                tool: b.tool.to_string(),
                table: match b.model {
                    ModelSource::Fixed(m) => TableSource::Fixed(m.to_string()),
                    ModelSource::FromArg(a) => TableSource::FromArg(a.to_string()),
                },
                record_paths: ODOO_RECORD_PATHS.iter().map(|p| (*p).to_string()).collect(),
                key_alias: b
                    .key_alias
                    .iter()
                    .map(|(c, o)| ((*c).to_string(), (*o).to_string()))
                    .collect(),
                // Odoo models and fields are identifiers.
                free_form_names: false,
            })
            .collect(),
    }
}

// ── Built-in: DuDuClaw's own SQL connector ─────────────────────────────────

fn duduclaw_db_source() -> DataSource {
    DataSource {
        name: BUILTIN_DUDUCLAW_DB.to_string(),
        label: "DuDuClaw 資料庫連接器".to_string(),
        builtin: true,
        bindings: vec![ToolBinding {
            // `db_select` names its table in `arguments.table` and returns
            // `{"rows": [...], "row_count", "truncated"}` — the one shape a
            // column rule can be bound to.
            tool: "db_select".to_string(),
            table: TableSource::FromArg("table".to_string()),
            record_paths: vec!["$.rows[*]".to_string()],
            // The connector returns column names verbatim.
            key_alias: Vec::new(),
            // SQL identifiers.
            free_form_names: false,
        }],
        // DELIBERATELY UNBOUND: `db_query` takes raw SQL. Its result columns
        // come from a projection nobody declared — aliases, joins and
        // expressions mean "the table this row belongs to" is not knowable
        // from the call, so a `table.column` rule cannot be bound to it
        // without guessing. Cover `db_query` with an explicit `json_path`
        // rule instead.
    }
}

// ── Built-in: DuDuClaw's local data-file readers ───────────────────────────

/// The file readers of `crates/duduclaw-cli/src/mcp_files.rs`.
///
/// Their "table" is the file's basename and their "columns" are the header
/// row, so neither is an identifier — `free_form_names` is on and a rule
/// reads `fields = ["customers.csv.name", "客戶清單.xlsx.地址"]` (split on the
/// LAST dot, so the extension stays with the table).
///
/// The table cannot come from the arguments: both tools are called with a
/// `path`, and the basename is resolved on the way out. It is read from the
/// result instead — `{"table": "customers.csv", "columns": […], "rows": […]}`.
fn duduclaw_files_source() -> DataSource {
    let binding = |tool: &str| ToolBinding {
        tool: tool.to_string(),
        table: TableSource::FromResult("/table".to_string()),
        record_paths: vec!["$.rows[*]".to_string()],
        // Row keys are the header cells themselves.
        key_alias: Vec::new(),
        free_form_names: true,
    };
    DataSource {
        name: BUILTIN_DUDUCLAW_FILES.to_string(),
        label: "地端資料檔（CSV／Excel）".to_string(),
        builtin: true,
        bindings: vec![binding("csv_read"), binding("xlsx_read")],
        // DELIBERATELY UNBOUND: `file_read` returns plain text (`"text"`), not
        // rows — there is no column to name, so a `table.column` rule has
        // nothing to bind to. Plain-text files are covered by the pattern
        // rules of the text pass, which runs over every string leaf anyway.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(tools: &[&str]) -> DataSourceDef {
        DataSourceDef {
            label: "Customer DB".into(),
            tools: tools.iter().map(|t| (*t).to_string()).collect(),
            table_arg: Some("table".into()),
            table: None,
            table_result: None,
            record_paths: Vec::new(),
            key_alias: BTreeMap::new(),
            free_form_names: false,
        }
    }

    #[test]
    fn simple_form_expands_one_binding_per_tool() {
        let src = def(&["pg_query", "pg_select"]).into_source("crm_pg").unwrap();
        assert_eq!(src.name, "crm_pg");
        assert_eq!(src.label, "Customer DB");
        assert!(!src.builtin);
        assert_eq!(src.bindings.len(), 2);
        assert_eq!(src.bindings[0].tool, "pg_query");
        assert_eq!(src.bindings[1].tool, "pg_select");
        for b in &src.bindings {
            assert_eq!(b.table, TableSource::FromArg("table".into()));
            assert_eq!(b.record_paths, default_record_paths());
            assert!(b.key_alias.is_empty());
        }
    }

    #[test]
    fn empty_label_falls_back_to_the_name() {
        let mut d = def(&["pg_select"]);
        d.label = "   ".into();
        assert_eq!(d.into_source("crm_pg").unwrap().label, "crm_pg");
    }

    #[test]
    fn missing_tools_is_an_error() {
        let d = def(&[]);
        let err = d.into_source("crm_pg").unwrap_err();
        assert!(err.to_string().contains("no tools"), "{err}");
    }

    #[test]
    fn table_and_table_arg_are_exclusive_and_required() {
        let mut both = def(&["pg_select"]);
        both.table = Some("customers".into());
        assert!(both.into_source("crm_pg").is_err());

        let mut neither = def(&["pg_select"]);
        neither.table_arg = None;
        assert!(neither.into_source("crm_pg").is_err());

        let mut fixed = def(&["pg_select"]);
        fixed.table_arg = None;
        fixed.table = Some("customers".into());
        let src = fixed.into_source("crm_pg").unwrap();
        assert_eq!(src.bindings[0].table, TableSource::Fixed("customers".into()));
    }

    #[test]
    fn bad_record_path_is_a_load_error() {
        let mut d = def(&["pg_select"]);
        d.record_paths = vec!["$.rows[*]".into(), "not-a-path".into()];
        let err = d.into_source("crm_pg").unwrap_err();
        assert!(err.to_string().contains("not-a-path"), "{err}");
    }

    #[test]
    fn registry_refuses_to_redefine_a_builtin() {
        let mut cfg = HashMap::new();
        cfg.insert(BUILTIN_ODOO.to_string(), def(&["pg_select"]));
        let err = registry(&cfg).unwrap_err();
        assert!(err.to_string().contains("built-in"), "{err}");

        let mut cfg = HashMap::new();
        cfg.insert(BUILTIN_DUDUCLAW_DB.to_string(), def(&["pg_select"]));
        assert!(registry(&cfg).is_err());

        // Every reserved name, so adding one can never quietly become
        // overridable.
        for name in BUILTIN_SOURCE_NAMES {
            let mut cfg = HashMap::new();
            cfg.insert((*name).to_string(), def(&["pg_select"]));
            assert!(registry(&cfg).is_err(), "'{name}' must not be redefinable");
        }
    }

    #[test]
    fn registry_contains_builtins_and_custom_sources() {
        let mut cfg = HashMap::new();
        cfg.insert("crm_pg".to_string(), def(&["pg_select"]));
        let reg = registry(&cfg).unwrap();
        assert!(reg[BUILTIN_ODOO].builtin);
        assert!(reg[BUILTIN_DUDUCLAW_DB].builtin);
        assert!(reg[BUILTIN_DUDUCLAW_FILES].builtin);
        assert!(!reg["crm_pg"].builtin);
        assert_eq!(reg.len(), BUILTIN_SOURCE_NAMES.len() + 1);
    }

    #[test]
    fn builtin_odoo_matches_the_shipped_tool_table() {
        let odoo = odoo_source();
        let tools: Vec<&str> = odoo.bindings.iter().map(|b| b.tool.as_str()).collect();
        assert_eq!(
            tools,
            [
                "odoo_search",
                "odoo_execute",
                "odoo_partner_search",
                "odoo_crm_leads",
                "odoo_sale_orders",
                "odoo_inventory_products",
                "odoo_inventory_check",
                "odoo_invoice_list",
                "odoo_payment_status",
            ]
        );
        for b in &odoo.bindings {
            assert_eq!(b.record_paths, vec!["$[*]".to_string(), "$".to_string()]);
        }
        // The two dynamic-model tools read `arguments.model`.
        assert_eq!(
            odoo.bindings[0].table,
            TableSource::FromArg("model".to_string())
        );
        assert_eq!(
            odoo.bindings[2].table,
            TableSource::Fixed("res.partner".to_string())
        );
        // A mapper split survives the conversion (stage_id → stage + stage_id).
        let leads = odoo
            .bindings
            .iter()
            .find(|b| b.tool == "odoo_crm_leads")
            .unwrap();
        let stage: Vec<&str> = leads
            .key_alias
            .iter()
            .filter(|(c, _)| c == "stage_id")
            .map(|(_, o)| o.as_str())
            .collect();
        assert_eq!(stage, ["stage", "stage_id"]);
    }

    #[test]
    fn builtin_duduclaw_db_binds_only_db_select() {
        let db = duduclaw_db_source();
        assert_eq!(db.bindings.len(), 1);
        assert_eq!(db.bindings[0].tool, "db_select");
        assert_eq!(
            db.bindings[0].table,
            TableSource::FromArg("table".to_string())
        );
        assert_eq!(db.bindings[0].record_paths, vec!["$.rows[*]".to_string()]);
        assert!(db.bindings[0].key_alias.is_empty());
        assert!(!db.bindings[0].free_form_names, "SQL names are identifiers");
        // `db_query` is deliberately unbound — raw SQL has no declared table.
        assert!(!db.bindings.iter().any(|b| b.tool == "db_query"));
    }

    #[test]
    fn builtin_duduclaw_files_binds_the_two_readers_from_the_result() {
        let files = duduclaw_files_source();
        let tools: Vec<&str> = files.bindings.iter().map(|b| b.tool.as_str()).collect();
        assert_eq!(tools, ["csv_read", "xlsx_read"]);
        for b in &files.bindings {
            assert_eq!(b.table, TableSource::FromResult("/table".to_string()));
            assert_eq!(b.record_paths, vec!["$.rows[*]".to_string()]);
            assert!(b.key_alias.is_empty(), "row keys ARE the header cells");
            assert!(b.free_form_names, "file names / headers are not identifiers");
        }
        // `file_read` is deliberately unbound — plain text has no columns.
        assert!(!files.bindings.iter().any(|b| b.tool == "file_read"));
    }

    #[test]
    fn table_result_is_the_third_exclusive_option() {
        // Accepted on its own …
        let mut d = def(&["csv_read"]);
        d.table_arg = None;
        d.table_result = Some("/table".into());
        let src = d.clone().into_source("files").unwrap();
        assert_eq!(
            src.bindings[0].table,
            TableSource::FromResult("/table".to_string())
        );

        // … must be a JSON pointer …
        let mut bad = d.clone();
        bad.table_result = Some("table".into());
        let err = bad.into_source("files").unwrap_err();
        assert!(err.to_string().contains("JSON pointer"), "{err}");

        // … and is exclusive with both siblings, with an error naming all three.
        let mut two = d.clone();
        two.table_arg = Some("table".into());
        let err = two.into_source("files").unwrap_err();
        for field in ["table_arg", "table", "table_result"] {
            assert!(err.to_string().contains(field), "{field} missing from: {err}");
        }

        let mut none = d.clone();
        none.table_result = None;
        let err = none.into_source("files").unwrap_err();
        for field in ["table_arg", "table", "table_result"] {
            assert!(err.to_string().contains(field), "{field} missing from: {err}");
        }
    }

    #[test]
    fn simple_form_reports_table_result_and_free_form() {
        let files = duduclaw_files_source().simple_form();
        assert_eq!(files.table_result.as_deref(), Some("/table"));
        assert!(files.table_arg.is_none());
        assert!(files.table.is_none());
        assert!(files.free_form_names);
        assert_eq!(files.tools, vec!["csv_read".to_string(), "xlsx_read".to_string()]);

        // Identifier-mode built-ins keep reporting `false`.
        assert!(!duduclaw_db_source().simple_form().free_form_names);
        assert!(!odoo_source().simple_form().free_form_names);
        assert!(odoo_source().simple_form().table_result.is_none());
    }

    #[test]
    fn free_form_names_round_trips_through_the_simple_form() {
        let mut d = def(&["csv_read", "xlsx_read"]);
        d.table_arg = None;
        d.table_result = Some("/table".into());
        d.free_form_names = true;
        d.record_paths = vec!["$.rows[*]".into()];
        let src = d.clone().into_source("files").unwrap();
        assert!(src.bindings.iter().all(|b| b.free_form_names));
        assert_eq!(src.simple_form(), d);
    }

    #[test]
    fn every_builtin_record_path_parses() {
        for src in builtin_sources() {
            for b in &src.bindings {
                for p in &b.record_paths {
                    JsonPath::parse(p).unwrap_or_else(|e| panic!("{}: {p}: {e}", src.name));
                }
            }
        }
    }

    #[test]
    fn simple_form_round_trips_a_custom_source() {
        let mut d = def(&["pg_query", "pg_select"]);
        d.key_alias.insert("name".into(), "customer_name".into());
        d.record_paths = vec!["$.rows[*]".into()];
        let src = d.clone().into_source("crm_pg").unwrap();
        assert_eq!(src.simple_form(), d);
    }

    #[test]
    fn simple_form_of_a_disagreeing_builtin_reports_nothing_it_cannot_prove() {
        let odoo = odoo_source().simple_form();
        // Odoo mixes FromArg and Fixed tables, and per-tool aliases.
        assert!(odoo.table.is_none());
        assert!(odoo.table_arg.is_none());
        assert!(odoo.key_alias.is_empty());
        assert_eq!(odoo.tools.len(), 9);
        assert_eq!(odoo.record_paths, vec!["$[*]".to_string(), "$".to_string()]);

        // A single-binding built-in renders exactly.
        let db = duduclaw_db_source().simple_form();
        assert_eq!(db.table_arg.as_deref(), Some("table"));
        assert_eq!(db.tools, vec!["db_select".to_string()]);
    }

    #[test]
    fn source_name_charset_is_anchored() {
        assert!(is_valid_data_source_name("crm_pg"));
        assert!(is_valid_data_source_name("a"));
        assert!(is_valid_data_source_name("pg-2"));
        assert!(!is_valid_data_source_name("Crm"));
        assert!(!is_valid_data_source_name("2pg"));
        assert!(!is_valid_data_source_name("crm.pg"));
        assert!(!is_valid_data_source_name(""));
        assert!(!is_valid_data_source_name(&"a".repeat(65)));
    }

    #[test]
    fn def_parses_from_the_documented_toml() {
        let src = r#"
[redaction.data_sources.crm_pg]
label = "客戶 CRM 資料庫"
tools = ["pg_query", "pg_select"]
table_arg = "table"
record_paths = ["$.rows[*]", "$[*]"]
key_alias = { name = "customer_name" }
"#;
        #[derive(Deserialize)]
        struct W {
            redaction: R,
        }
        #[derive(Deserialize)]
        struct R {
            data_sources: HashMap<String, DataSourceDef>,
        }
        let w: W = toml::from_str(src).unwrap();
        let d = &w.redaction.data_sources["crm_pg"];
        assert_eq!(d.label, "客戶 CRM 資料庫");
        assert_eq!(d.tools, ["pg_query", "pg_select"]);
        assert_eq!(d.table_arg.as_deref(), Some("table"));
        assert_eq!(d.key_alias["name"], "customer_name");

        let src = d.clone().into_source("crm_pg").unwrap();
        assert_eq!(
            src.bindings[0].key_alias,
            vec![("name".to_string(), "customer_name".to_string())]
        );
    }

    #[test]
    fn def_parses_table_result_and_free_form_from_toml() {
        let src = r#"
[redaction.data_sources.excel_drop]
label = "匯入的試算表"
tools = ["sheet_read"]
table_result = "/table"
record_paths = ["$.rows[*]"]
free_form_names = true
"#;
        #[derive(Deserialize)]
        struct W {
            redaction: R,
        }
        #[derive(Deserialize)]
        struct R {
            data_sources: HashMap<String, DataSourceDef>,
        }
        let w: W = toml::from_str(src).unwrap();
        let d = &w.redaction.data_sources["excel_drop"];
        assert_eq!(d.table_result.as_deref(), Some("/table"));
        assert!(d.free_form_names);
        assert!(d.table_arg.is_none() && d.table.is_none());

        let built = d.clone().into_source("excel_drop").unwrap();
        assert_eq!(
            built.bindings[0].table,
            TableSource::FromResult("/table".to_string())
        );
        assert!(built.bindings[0].free_form_names);

        // Omitting both new keys keeps the pre-2026-09 meaning.
        let older = r#"
[redaction.data_sources.crm_pg]
tools = ["pg_select"]
table_arg = "table"
"#;
        let w: W = toml::from_str(older).unwrap();
        let d = &w.redaction.data_sources["crm_pg"];
        assert!(d.table_result.is_none());
        assert!(!d.free_form_names);
    }
}
